/**
 * The Pitch Coach panel's extractable logic. The drawing itself is validated
 * by the owner driving the app; everything a test can reach lives in
 * `panel-logic.ts` so it is reachable without a canvas.
 */
import { describe, it, expect } from "vitest";
import { laneScrollFor, rehearseKeyMatches, stabilityCents, targetNotesFor } from "./panel-logic";
import type { MidiClip, PitchFrame } from "../../types/ipc";

describe("laneScrollFor", () => {
  it("pins the playhead at 35% in fixed mode", () => {
    // The plan's own numbers (96000 samples, spp 1000, 400 px) put the lane
    // start at -44000 — before time zero. Same formula, a position far
    // enough in that the clamp below is not what is being measured.
    expect(laneScrollFor("fixed", 960000, 1000, 400)).toBe(960000 - 0.35 * 400 * 1000);
  });

  it("leaves the viewport alone in free mode", () => {
    expect(laneScrollFor("free", 96000, 1000, 400, 12345)).toBe(12345);
  });

  it("never scrolls past zero, so the lane cannot show negative time", () => {
    expect(laneScrollFor("fixed", 0, 1000, 400)).toBe(0);
  });

  it("free mode starts at zero when the caller has no viewport yet", () => {
    expect(laneScrollFor("free", 96000, 1000, 400)).toBe(0);
  });
});

describe("rehearseKeyMatches", () => {
  it("matches the configured key and ignores others", () => {
    expect(rehearseKeyMatches("h", "h")).toBe(true);
    expect(rehearseKeyMatches("h", "j")).toBe(false);
  });

  it("never matches when the preference is off", () => {
    expect(rehearseKeyMatches("none", "h")).toBe(false);
    expect(rehearseKeyMatches("none", "none")).toBe(false);
  });

  it("is case-insensitive, so caps lock does not disarm it", () => {
    expect(rehearseKeyMatches("h", "H")).toBe(true);
  });

  it("ignores non-character keys", () => {
    expect(rehearseKeyMatches("h", "Shift")).toBe(false);
  });
});

const clip = (over: Partial<MidiClip> = {}): MidiClip => ({
  id: "c1",
  trackId: "t1",
  name: "melody",
  timelineStartTicks: 0,
  lengthTicks: 960,
  notes: [{ tick: 0, lengthTicks: 480, key: 60, velocity: 100 }],
  ...over,
});

/** One sample per tick keeps the expected numbers readable. */
const toSamples = (ticks: number) => ticks;

describe("targetNotesFor", () => {
  it("converts each note to a sample span through the caller's bijection", () => {
    const notes = targetNotesFor([clip()], (ticks) => ticks * 50);
    expect(notes).toEqual([{ noteId: 0, startSample: 0, endSample: 480 * 50, key: 60 }]);
  });

  it("repeats the content across a longer placement, like the timeline preview", () => {
    const notes = targetNotesFor([clip({ lengthTicks: 2880, contentLengthTicks: 960 })], toSamples);
    expect(notes.map((n) => n.startSample)).toEqual([0, 960, 1920]);
  });

  it("crops a note at the placement end rather than overhanging it", () => {
    const notes = targetNotesFor([clip({ lengthTicks: 1200, contentLengthTicks: 960 })], toSamples);
    expect(notes.map((n) => n.startSample)).toEqual([0, 960]);
    // The second repeat's note would run to 1440; the placement ends at 1200.
    expect(notes[1].endSample).toBe(1200);
  });

  it("drops a note that starts past the placement end", () => {
    const notes = targetNotesFor(
      [
        clip({
          lengthTicks: 940,
          contentLengthTicks: 480,
          notes: [{ tick: 470, lengthTicks: 10, key: 60, velocity: 100 }],
        }),
      ],
      toSamples,
    );
    expect(notes.map((n) => n.startSample)).toEqual([470]);
  });

  it("applies the placement transpose, so the target is the note that sounds", () => {
    const notes = targetNotesFor([clip({ transposeSemitones: -12 })], toSamples);
    expect(notes[0].key).toBe(48);
  });

  it("offsets by the clip's timeline start", () => {
    const notes = targetNotesFor([clip({ timelineStartTicks: 1920 })], toSamples);
    expect(notes[0].startSample).toBe(1920);
  });

  it("gives every note a distinct id across clips, so redraws stay keyed", () => {
    const notes = targetNotesFor([clip(), clip({ id: "c2", timelineStartTicks: 960 })], toSamples);
    expect(new Set(notes.map((n) => n.noteId)).size).toBe(notes.length);
  });

  it("returns nothing for no clips", () => {
    expect(targetNotesFor([], toSamples)).toEqual([]);
  });
});

const frame = (midi: number, voiced = true): PitchFrame => ({
  sample: 0,
  midi,
  hz: 440 * 2 ** ((midi - 69) / 12),
  clarity: 0.9,
  rms: 0.2,
  voiced,
});

describe("stabilityCents", () => {
  it("is near zero for a held pitch", () => {
    expect(stabilityCents([frame(57), frame(57), frame(57), frame(57)])).toBeCloseTo(0);
  });

  it("grows with the wobble, in cents", () => {
    // ±0.1 semitone around 57 is 10 cents of deviation either way.
    const wobble = [frame(56.9), frame(57.1), frame(56.9), frame(57.1)];
    expect(stabilityCents(wobble)).toBeCloseTo(10, 5);
  });

  it("ignores unvoiced frames rather than reading them as a huge swing", () => {
    // An unvoiced frame carries midi 0; counted, it would report ~5700 cents.
    const withBreath = [frame(57), frame(0, false), frame(57), frame(57)];
    expect(stabilityCents(withBreath)).toBeCloseTo(0);
  });

  it("is null when there is not enough voiced signal to judge", () => {
    expect(stabilityCents([])).toBeNull();
    expect(stabilityCents([frame(57)])).toBeNull();
    expect(stabilityCents([frame(0, false), frame(0, false)])).toBeNull();
  });

  it("uses a median deviation, so one octave-slip frame does not dominate", () => {
    const mostlySteady = [frame(57), frame(57), frame(57), frame(45), frame(57), frame(57)];
    expect(stabilityCents(mostlySteady)!).toBeLessThan(50);
  });
});
