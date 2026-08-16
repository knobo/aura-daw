import { describe, expect, it } from "vitest";
import {
  bindingFocusSamples,
  clipWouldSelfTrigger,
  incomingIsEcho,
  midiNoteName,
  nextBindingName,
  nextFreeNote,
  overlayBox,
  parseMidiNoteName,
  regionFromMarquee,
  resolveLaunch,
  type LaunchBinding,
} from "./launch-map";

const region = (
  id: string,
  note: number,
  extras: Partial<LaunchBinding> = {},
): LaunchBinding => ({
  id,
  name: extras.name ?? id,
  note,
  channel: extras.channel ?? null,
  target: extras.target ?? {
    kind: "region",
    startTicks: 0,
    lengthTicks: 960,
    trackIds: ["t1"],
  },
});

describe("midiNoteName", () => {
  it("names middle C as C4 (MIDI 60)", () => {
    expect(midiNoteName(60)).toBe("C4");
  });

  it("names C# and the top/bottom of the range", () => {
    expect(midiNoteName(61)).toBe("C#4");
    expect(midiNoteName(0)).toBe("C-1");
    expect(midiNoteName(127)).toBe("G9");
  });

  it("round-trips every MIDI key through parseMidiNoteName", () => {
    for (let n = 0; n <= 127; n++) {
      expect(parseMidiNoteName(midiNoteName(n))).toBe(n);
    }
  });

  it("rejects junk", () => {
    expect(parseMidiNoteName("")).toBeNull();
    expect(parseMidiNoteName("H4")).toBeNull();
    expect(parseMidiNoteName("C")).toBeNull();
  });
});

describe("resolveLaunch", () => {
  it("matches a note on any channel when the binding has no channel", () => {
    const bindings = [region("a", 60)];
    expect(resolveLaunch(bindings, 60, 0)?.id).toBe("a");
    expect(resolveLaunch(bindings, 60, 15)?.id).toBe("a");
  });

  it("ignores a channel-scoped binding on a different channel", () => {
    const bindings = [region("a", 60, { channel: 2 })];
    expect(resolveLaunch(bindings, 60, 2)?.id).toBe("a");
    expect(resolveLaunch(bindings, 60, 3)).toBeUndefined();
  });

  it("prefers the first binding when two claim the same note", () => {
    const bindings = [region("first", 60), region("second", 60)];
    expect(resolveLaunch(bindings, 60, 0)?.id).toBe("first");
  });

  it("does not match a different note", () => {
    expect(resolveLaunch([region("a", 60)], 61, 0)).toBeUndefined();
  });
});

describe("clipWouldSelfTrigger", () => {
  it("is true when the clip contains the trigger note on a matching channel", () => {
    expect(clipWouldSelfTrigger([{ key: 60, channel: 0 }], 60, null)).toBe(true);
    expect(clipWouldSelfTrigger([{ key: 60, channel: 2 }], 60, 2)).toBe(true);
  });

  it("is false when the clip has no matching note", () => {
    expect(clipWouldSelfTrigger([{ key: 64, channel: 0 }], 60, null)).toBe(false);
    expect(clipWouldSelfTrigger([{ key: 60, channel: 1 }], 60, 2)).toBe(false);
    expect(clipWouldSelfTrigger([], 60, null)).toBe(false);
  });
});

describe("incomingIsEcho", () => {
  it("treats a recently sent matching note as an echo", () => {
    expect(
      incomingIsEcho([{ note: 60, channel: 0, atMs: 1000 }], { note: 60, channel: 0, atMs: 1040 }, 80),
    ).toBe(true);
  });

  it("lets a note through after the echo window, or if key/channel differ", () => {
    expect(
      incomingIsEcho([{ note: 60, channel: 0, atMs: 1000 }], { note: 60, channel: 0, atMs: 1090 }, 80),
    ).toBe(false);
    expect(
      incomingIsEcho([{ note: 60, channel: 0, atMs: 1000 }], { note: 61, channel: 0, atMs: 1010 }, 80),
    ).toBe(false);
    expect(
      incomingIsEcho([{ note: 60, channel: 0, atMs: 1000 }], { note: 60, channel: 1, atMs: 1010 }, 80),
    ).toBe(false);
  });
});

describe("regionFromMarquee", () => {
  const tracks = [{ id: "a" }, { id: "b" }, { id: "c" }];
  const samplesToTicks = (s: number) => s; // 1:1 for the test

  it("builds a region covering the lanes and time the marquee spans", () => {
    const r = regionFromMarquee({
      startSamples: 480,
      endSamples: 1920,
      laneLo: 0,
      laneHi: 1,
      tracks,
      samplesToTicks,
      snapTicks: 480,
    });
    expect(r).toEqual({
      startTicks: 480,
      lengthTicks: 1440,
      trackIds: ["a", "b"],
    });
  });

  it("returns null for a zero-length or empty-track drag", () => {
    expect(
      regionFromMarquee({
        startSamples: 100,
        endSamples: 100,
        laneLo: 0,
        laneHi: 0,
        tracks,
        samplesToTicks,
      }),
    ).toBeNull();
    expect(
      regionFromMarquee({
        startSamples: 0,
        endSamples: 960,
        laneLo: 0,
        laneHi: 0,
        tracks: [],
        samplesToTicks,
      }),
    ).toBeNull();
  });
});

describe("bindingFocusSamples", () => {
  const ticksToSamples = (t: number) => t * 2;

  it("returns the region's start in samples", () => {
    const b = region("r", 60, {
      target: { kind: "region", startTicks: 480, lengthTicks: 960, trackIds: ["t"] },
    });
    expect(bindingFocusSamples(b, [], ticksToSamples)).toBe(960);
  });

  it("returns the clip's start, or null if the clip is gone", () => {
    const b = region("c", 60, { target: { kind: "clip", clipId: "clip-1" } });
    expect(
      bindingFocusSamples(b, [{ id: "clip-1", timelineStartTicks: 240 }], ticksToSamples),
    ).toBe(480);
    expect(bindingFocusSamples(b, [], ticksToSamples)).toBeNull();
  });
});

describe("nextBindingName", () => {
  it("numbers scenes from the existing list", () => {
    expect(nextBindingName([])).toBe("Scene 1");
    expect(nextBindingName([region("a", 60, { name: "Scene 1" })])).toBe("Scene 2");
  });
});

describe("nextFreeNote", () => {
  it("starts at C3 and skips notes already bound", () => {
    expect(nextFreeNote([])).toBe(48);
    expect(nextFreeNote([region("a", 48), region("b", 49)])).toBe(50);
  });
});

describe("overlayBox", () => {
  const tracks = [{ id: "a" }, { id: "b" }, { id: "c" }];
  const ticksToSamples = (t: number) => t;

  it("spans the region's tracks and time", () => {
    const b = region("r", 60, {
      target: { kind: "region", startTicks: 100, lengthTicks: 200, trackIds: ["c", "a"] },
    });
    expect(overlayBox(b, tracks, [], ticksToSamples)).toEqual({
      startSamples: 100,
      endSamples: 300,
      laneLo: 0,
      laneHi: 2,
    });
  });

  it("follows a clip onto its lane", () => {
    const b = region("c", 60, { target: { kind: "clip", clipId: "clip-1" } });
    expect(
      overlayBox(
        b,
        tracks,
        [{ id: "clip-1", trackId: "b", timelineStartTicks: 10, lengthTicks: 40 }],
        ticksToSamples,
      ),
    ).toEqual({ startSamples: 10, endSamples: 50, laneLo: 1, laneHi: 1 });
  });

  it("returns null when the target is gone", () => {
    const b = region("c", 60, { target: { kind: "clip", clipId: "missing" } });
    expect(overlayBox(b, tracks, [], ticksToSamples)).toBeNull();
  });
});
