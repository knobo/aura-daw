import { describe, expect, it } from "vitest";
import { instanceConnectionLabel, tracksBoundToInstance } from "./plugin-binding";

const audio = (id: string, name: string, instrumentId?: string | null) => ({
  id,
  name,
  kind: "audio" as const,
  instrumentId,
});
const midi = (id: string, name: string, instrumentId?: string | null) => ({
  id,
  name,
  kind: "midi" as const,
  instrumentId,
});

describe("tracksBoundToInstance", () => {
  it("returns no tracks when no midi track points at the instance", () => {
    const tracks = [
      midi("m1", "Midi 1", "plugin:other"),
      midi("m2", "Midi 2", null),
      audio("a1", "Audio 1", "plugin:zyn-1"),
    ];
    expect(tracksBoundToInstance(tracks, "zyn-1")).toEqual([]);
  });

  it("returns the midi track whose instrumentId is plugin:<id>", () => {
    const bound = midi("m5", "Midi 5", "plugin:zyn-1");
    const tracks = [midi("m1", "Midi 1", null), bound, audio("a1", "Audio 1")];
    expect(tracksBoundToInstance(tracks, "zyn-1")).toEqual([bound]);
  });

  it("returns every midi track bound to the same instance", () => {
    const a = midi("m2", "Midi 2", "plugin:zyn-1");
    const b = midi("m5", "Midi 5", "plugin:zyn-1");
    expect(tracksBoundToInstance([a, midi("m3", "Midi 3", "plugin:other"), b], "zyn-1")).toEqual([
      a,
      b,
    ]);
  });
});

describe("instanceConnectionLabel", () => {
  it("says unbound when nothing is connected", () => {
    expect(instanceConnectionLabel([midi("m1", "Midi 1", null)], "zyn-1")).toBe("unbound");
  });

  it("names the bound track as the current connection", () => {
    expect(
      instanceConnectionLabel([midi("m5", "Midi 5", "plugin:zyn-1")], "zyn-1"),
    ).toBe("connection: Midi 5");
  });

  it("lists every bound track, not a selected or current one", () => {
    expect(
      instanceConnectionLabel(
        [
          midi("m1", "Midi 1", null),
          midi("m2", "Midi 2", "plugin:zyn-1"),
          midi("m5", "Midi 5", "plugin:zyn-1"),
        ],
        "zyn-1",
      ),
    ).toBe("connection: Midi 2 · Midi 5");
  });
});
