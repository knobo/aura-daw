/**
 * The Pitch Coach panel's extractable logic. The drawing itself is validated
 * by the owner driving the app; everything a test can reach lives in
 * `panel-logic.ts` so it is reachable without a canvas.
 */
import { describe, it, expect, vi } from "vitest";
import {
  LANE_MAX_VISIBLE_S,
  LANE_MIN_VISIBLE_S,
  laneScrollFor,
  laneWindowFor,
  rehearseKeyMatches,
  rehearseKeyReleases,
  stabilityCents,
  targetNotesFor,
} from "./panel-logic";
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

describe("laneWindowFor", () => {
  const RATE = 48000;
  const W = 800;
  // A melody from 4 s to 10 s.
  const melody = [
    { noteId: 0, startSample: 4 * RATE, endSample: 5 * RATE, key: 60 },
    { noteId: 1, startSample: 9 * RATE, endSample: 10 * RATE, key: 64 },
  ];
  const base = {
    mode: "fixed" as const,
    playing: false,
    positionSamples: 0,
    widthPx: W,
    targets: melody,
    rate: RATE,
    timelineStart: 0,
    timelineSpp: 1200,
  };

  it("brings a melody that starts later into view, instead of showing empty lane", () => {
    // The bug this exists for: stopped at 0 with a melody at bar 5, the
    // pinned-playhead rule shows nothing at all.
    const win = laneWindowFor(base);
    const visibleEnd = win.startSample + W * win.spp;
    expect(win.startSample).toBeLessThanOrEqual(melody[0].startSample);
    expect(visibleEnd).toBeGreaterThan(melody[0].startSample);
  });

  it("frames the whole melody when it fits the readable span", () => {
    const win = laneWindowFor(base);
    const visibleEnd = win.startSample + W * win.spp;
    expect(visibleEnd).toBeGreaterThanOrEqual(melody[1].endSample);
  });

  it("keeps a short phrase from filling the lane at absurd zoom", () => {
    const short = [{ noteId: 0, startSample: 0, endSample: RATE / 4, key: 60 }];
    const win = laneWindowFor({ ...base, targets: short });
    expect((W * win.spp) / RATE).toBeGreaterThanOrEqual(LANE_MIN_VISIBLE_S);
  });

  it("caps how much of a long song is shown at once", () => {
    const long = [
      { noteId: 0, startSample: 0, endSample: RATE, key: 60 },
      { noteId: 1, startSample: 300 * RATE, endSample: 301 * RATE, key: 60 },
    ];
    const win = laneWindowFor({ ...base, targets: long });
    expect((W * win.spp) / RATE).toBeLessThanOrEqual(LANE_MAX_VISIBLE_S);
  });

  it("pins the playhead once the transport rolls", () => {
    const win = laneWindowFor({ ...base, playing: true, positionSamples: 60 * RATE });
    expect(win.startSample).toBe(60 * RATE - 0.35 * W * win.spp);
  });

  it("does not change zoom between stopped and rolling, so record is not a jump", () => {
    const stopped = laneWindowFor(base);
    const rolling = laneWindowFor({ ...base, playing: true, positionSamples: 5 * RATE });
    expect(rolling.spp).toBe(stopped.spp);
  });

  it("follows the playhead when stopped inside the melody", () => {
    // Seeked into the phrase: show where the playhead is, not the start.
    const win = laneWindowFor({ ...base, positionSamples: 9.5 * RATE });
    const visibleEnd = win.startSample + W * win.spp;
    expect(win.startSample).toBeLessThanOrEqual(9.5 * RATE);
    expect(visibleEnd).toBeGreaterThan(9.5 * RATE);
  });

  it("falls back to the timeline's own view with no melody", () => {
    const win = laneWindowFor({ ...base, targets: [], timelineStart: 7777, timelineSpp: 999 });
    expect(win).toEqual({ startSample: 7777, spp: 999 });
  });

  it("free mode is the timeline's view, melody or not", () => {
    const win = laneWindowFor({ ...base, mode: "free", timelineStart: 4242, timelineSpp: 888 });
    expect(win).toEqual({ startSample: 4242, spp: 888 });
  });

  it("never starts before time zero", () => {
    const atZero = [{ noteId: 0, startSample: 0, endSample: RATE, key: 60 }];
    expect(laneWindowFor({ ...base, targets: atZero }).startSample).toBeGreaterThanOrEqual(0);
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

describe("rehearseKeyReleases", () => {
  it("releases on the key that started the hold", () => {
    expect(rehearseKeyReleases("h", "h")).toBe(true);
    expect(rehearseKeyReleases("h", "H")).toBe(true);
  });

  /** The take-corruption case: any other keyup ending the hold means the
   * engine starts committing real audio while the rehearse key is still
   * physically down, and the later real keyup is a no-op. */
  it("does not release on a different key", () => {
    expect(rehearseKeyReleases("h", "Shift")).toBe(false);
    expect(rehearseKeyReleases("h", "ArrowLeft")).toBe(false);
    expect(rehearseKeyReleases("h", "j")).toBe(false);
  });

  it("is a no-op when no hold is down", () => {
    expect(rehearseKeyReleases(null, "h")).toBe(false);
  });

  /** Compared against what went DOWN, not against the preference: changing
   * the key mid-hold must not strand the engine writing silence. */
  it("releases a key that is no longer the configured one", () => {
    expect(rehearseKeyMatches("j", "h")).toBe(false);
    expect(rehearseKeyReleases("h", "h")).toBe(true);
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

  /** The backend's `clip_note_key_vel` clamps, so an unclamped lane would
   * colour its bar against a different target than the report's row. */
  it("clamps a transpose that leaves the MIDI range, like the backend does", () => {
    expect(targetNotesFor([clip({ transposeSemitones: 96 })], toSamples)[0].key).toBe(127);
    expect(targetNotesFor([clip({ transposeSemitones: -96 })], toSamples)[0].key).toBe(0);
  });

  it("offsets by the clip's timeline start", () => {
    const notes = targetNotesFor([clip({ timelineStartTicks: 1920 })], toSamples);
    expect(notes[0].startSample).toBe(1920);
  });

  it("gives every note a distinct id across clips, so redraws stay keyed", () => {
    const notes = targetNotesFor([clip(), clip({ id: "c2", timelineStartTicks: 960 })], toSamples);
    expect(new Set(notes.map((n) => n.noteId)).size).toBe(notes.length);
  });

  it("sorts by start across clips, since the panel walks it with a cursor", () => {
    const late = clip({ id: "late", timelineStartTicks: 3840 });
    const early = clip({ id: "early", timelineStartTicks: 0 });
    const notes = targetNotesFor([late, early], toSamples);
    expect(notes.map((n) => n.startSample)).toEqual([0, 3840]);
    expect(notes.map((n) => n.noteId)).toEqual([0, 1]);
  });

  it("takes the content length by injection, so one loop rule stays in the store", () => {
    const effective = vi.fn(() => 480);
    const notes = targetNotesFor([clip({ lengthTicks: 960 })], toSamples, effective);
    expect(effective).toHaveBeenCalled();
    // content 480 across a 960 placement is two repeats, not one.
    expect(notes.map((n) => n.startSample)).toEqual([0, 480]);
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
