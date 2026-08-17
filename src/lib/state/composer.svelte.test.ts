/**
 * The Composer store. What these pin is the thin-renderer contract (ADR 0006):
 * every edit leaves as ONE `harmony_set` carrying the whole document, and the
 * store never derives theory — it forwards symbols the backend spelled.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ChordSpan, HarmonyView, KeySpan } from "../types/ipc";

const view = (chords: ChordSpan[], key = "C ionian"): HarmonyView => ({
  harmony: { keys: [{ tick: 0, key }], chords },
  key,
  keyLabel: key === "C ionian" ? "C major" : key,
  keySignature: 0,
  wedges: [],
  spans: chords.map((c) => ({
    tick: c.tick,
    lengthTicks: c.lengthTicks,
    symbol: c.chord,
    pretty: c.chord,
    roman: "I",
    function: "tonic",
    borrowed: false,
    why: "because",
    tones: ["C", "E", "G"],
  })),
  neighbours: [],
  borrowed: [],
  schemas: [],
  genres: ["rock"],
  voicingStyles: ["close"],
  bassStyles: ["root"],
  ppq: 960,
  ticksPerBar: 3840,
});

const BAR = 3840;
const bar = (i: number, chord: string): ChordSpan => ({
  tick: i * BAR,
  lengthTicks: BAR,
  chord,
});

let stored: { keys: KeySpan[]; chords: ChordSpan[] } = { keys: [], chords: [] };

const harmonyGet = vi.fn(async (_at?: number) => view(stored.chords));
const harmonySet = vi.fn(async (keys: KeySpan[], chords: ChordSpan[], _at?: number) => {
  stored = { keys, chords };
  return view(chords, keys[0]?.key ?? "C ionian");
});
const composerPalette = vi.fn(async (tick: number) => ({
  tick,
  key: "C ionian",
  keyLabel: "C major",
  chord: "C",
  chordPretty: "C",
  roman: "I",
  classes: [],
}));
const composerSuggest = vi.fn(async (_at?: number, _limit?: number) => [
  { chord: "G7", roman: "V7", function: "dominant", score: 0.8, why: "the dominant" },
]);
const composerGenerate = vi.fn(async (req: Record<string, unknown>) => ({
  clips: [
    {
      part: "chords",
      clipId: "clip-1",
      trackId: "t-1",
      trackName: "Composer Chords",
      createdTrack: true,
      noteCount: 12,
      annotations: [{ tick: 0, lengthTicks: BAR, label: "C · I", why: "home" }],
    },
  ],
  harmony: view([bar(0, "C"), bar(1, "G")]),
  progression: { key: "C ionian", keyLabel: "C major", why: "the axis", slots: [] },
  seed: (req.seed as number) ?? 0,
  bars: 2,
}));

const listeners: Record<string, (p: unknown) => void> = {};

vi.mock("../tauri", () => ({
  backend: {
    on: (name: string, fn: (p: unknown) => void) => {
      listeners[name] = fn;
      return () => delete listeners[name];
    },
    harmonyGet: (at?: number) => harmonyGet(at),
    harmonySet: (k: KeySpan[], c: ChordSpan[], at?: number) => harmonySet(k, c, at),
    composerPalette: (t: number) => composerPalette(t),
    composerSuggest: (at?: number, l?: number) => composerSuggest(at, l),
    composerGenerate: (r: Record<string, unknown>) => composerGenerate(r),
  },
}));

const reload = vi.fn(async () => {});
const refresh = vi.fn(async () => {});
const select = vi.fn();
const flash = vi.fn();
vi.mock("./project.svelte", () => ({ project: { reload: () => reload() } }));
vi.mock("./midi.svelte", () => ({
  midi: {
    refresh: () => refresh(),
    select: (id: string) => select(id),
    flash: (id: string) => flash(id),
    ticksPerBar: 3840,
  },
}));
const errors: string[] = [];
vi.mock("./toasts.svelte", () => ({
  toasts: {
    error: (title: string, body: string) => void errors.push(`${title}: ${body}`),
    info: () => {},
  },
}));

const { composer } = await import("./composer.svelte");

beforeEach(async () => {
  vi.clearAllMocks();
  errors.length = 0;
  stored = { keys: [{ tick: 0, key: "C ionian" }], chords: [bar(0, "C"), bar(1, "Am")] };
  composer.selectedTick = null;
  composer.lastGenerate = null;
  composer.suggestions = [];
  composer.atTicks = 0;
  composer.seed = 1;
  await composer.refresh();
});

describe("reading", () => {
  it("applies the pushed view without deriving anything", async () => {
    expect(composer.keyLabel).toBe("C major");
    expect(composer.chords.map((c) => c.chord)).toEqual(["C", "Am"]);
    expect(composer.ticksPerBar).toBe(3840);
  });

  it("subscribes exactly once, and an undo re-pulls the view", async () => {
    await composer.init();
    await composer.init();
    expect(listeners["project://changed"]).toBeTypeOf("function");
    harmonyGet.mockClear();
    listeners["project://changed"]?.({});
    await Promise.resolve();
    await Promise.resolve();
    expect(harmonyGet).toHaveBeenCalledTimes(1);
  });

  it("reports an older backend instead of throwing", async () => {
    const mod = await import("../tauri");
    const saved = mod.backend.harmonyGet;
    // Legal, because the command is optional on `Backend` — which is the
    // point: an older backend is a missing method, not a throw.
    mod.backend.harmonyGet = undefined;
    await composer.refresh();
    expect(composer.unavailable).toBe(true);
    mod.backend.harmonyGet = saved;
    await composer.refresh();
    expect(composer.unavailable).toBe(false);
  });
});

describe("editing — one harmony_set per gesture", () => {
  it("appends a chord one bar after the last one", async () => {
    await composer.appendChord("G7");
    expect(harmonySet).toHaveBeenCalledTimes(1);
    const [keys, chords] = harmonySet.mock.calls[0];
    expect(keys).toEqual([{ tick: 0, key: "C ionian" }]);
    expect(chords).toEqual([bar(0, "C"), bar(1, "Am"), bar(2, "G7")]);
    // The reply is applied, not a locally-guessed document.
    expect(composer.chords.map((c) => c.chord)).toEqual(["C", "Am", "G7"]);
  });

  it("appends at the panel's cursor when the progression is empty", async () => {
    stored = { keys: [{ tick: 0, key: "C ionian" }], chords: [] };
    await composer.refresh();
    composer.atTicks = 4 * BAR;
    await composer.appendChord("F");
    expect(harmonySet.mock.calls[0][1]).toEqual([{ tick: 4 * BAR, lengthTicks: BAR, chord: "F" }]);
  });

  it("changes the key and leaves every chord alone", async () => {
    await composer.setKey("A aeolian");
    const [keys, chords] = harmonySet.mock.calls[0];
    expect(keys).toEqual([{ tick: 0, key: "A aeolian" }]);
    // Re-read in the new key, not rewritten.
    expect(chords.map((c) => c.chord)).toEqual(["C", "Am"]);
  });

  it("removes a region and closes the gap so the progression stays contiguous", async () => {
    stored.chords = [bar(0, "C"), bar(1, "Am"), bar(2, "F")];
    await composer.refresh();
    await composer.removeChordAt(BAR);
    expect(harmonySet.mock.calls[0][1]).toEqual([bar(0, "C"), bar(1, "F")]);
  });

  it("clears the selection when the selected region is removed", async () => {
    composer.select(BAR);
    expect(composer.selectedTick).toBe(BAR);
    await composer.removeChordAt(BAR);
    expect(composer.selectedTick).toBeNull();
  });

  it("replaces one chord in place", async () => {
    await composer.replaceChordAt(BAR, "Fmaj7");
    expect(harmonySet.mock.calls[0][1]).toEqual([bar(0, "C"), bar(1, "Fmaj7")]);
  });

  it("clears the whole progression but keeps the key", async () => {
    await composer.clear();
    const [keys, chords] = harmonySet.mock.calls[0];
    expect(keys).toEqual([{ tick: 0, key: "C ionian" }]);
    expect(chords).toEqual([]);
  });

  it("toasts a rejected edit rather than swallowing it", async () => {
    harmonySet.mockRejectedValueOnce(new Error("harmony: chord at tick 0 overlaps"));
    await composer.appendChord("G");
    expect(errors[0]).toContain("COULD NOT ADD THE CHORD");
    expect(errors[0]).toContain("overlaps");
    expect(composer.busy).toBe(false);
  });
});

describe("generating", () => {
  it("passes the displayed seed and the panel's cursor", async () => {
    composer.seed = 4242;
    composer.atTicks = 2 * BAR;
    await composer.generate({ parts: ["chords"], plan: "axis", bars: 4 });
    expect(composerGenerate).toHaveBeenCalledWith(
      expect.objectContaining({ seed: 4242, atTicks: 2 * BAR, plan: "axis", bars: 4 }),
    );
  });

  it("adopts the reply, re-pulls the tracks and clips, and points at the result", async () => {
    const reply = await composer.generate({ parts: ["chords"] });
    expect(reply?.clips).toHaveLength(1);
    expect(composer.lastGenerate).toBe(reply);
    // The view came from the reply, not from a local guess.
    expect(composer.chords.map((c) => c.chord)).toEqual(["C", "G"]);
    expect(reload).toHaveBeenCalledTimes(1);
    expect(refresh).toHaveBeenCalledTimes(1);
    expect(select).toHaveBeenCalledWith("clip-1");
    expect(flash).toHaveBeenCalledWith("clip-1");
  });

  it("toasts a failure and clears busy", async () => {
    composerGenerate.mockRejectedValueOnce(new Error("nothing to write: the harmony has no chords"));
    const reply = await composer.generate({ parts: ["chords"] });
    expect(reply).toBeNull();
    expect(errors[0]).toContain("GENERATE FAILED");
    expect(composer.busy).toBe(false);
  });

  it("refuses to overlap two generates", async () => {
    composer.busy = true;
    expect(await composer.generate({})).toBeNull();
    expect(composerGenerate).not.toHaveBeenCalled();
    composer.busy = false;
  });
});

describe("the coaching surfaces", () => {
  it("asks for suggestions AFTER the last chord, not at the cursor", async () => {
    composer.atTicks = 0;
    await composer.loadSuggestions();
    expect(composerSuggest).toHaveBeenCalledWith(2 * BAR, 6);
    expect(composer.suggestions[0].chord).toBe("G7");
  });

  it("loads the palette for the region the user selected", async () => {
    composer.select(BAR);
    await Promise.resolve();
    expect(composerPalette).toHaveBeenCalledWith(BAR);
    composer.select(null);
    expect(composer.selectedTick).toBeNull();
  });

  it("exposes the selected region's own analysis", async () => {
    composer.select(BAR);
    expect(composer.selectedSpan?.tick).toBe(BAR);
    expect(composer.selectedSpan?.why).toBe("because");
    composer.select(9_999_999);
    expect(composer.selectedSpan).toBeNull();
  });
});

describe("the seed", () => {
  it("re-rolls deterministically and never lands on zero", async () => {
    composer.seed = 1;
    const first: number[] = [];
    for (let i = 0; i < 5; i++) {
      composer.rollSeed();
      first.push(composer.seed);
    }
    composer.seed = 1;
    const again: number[] = [];
    for (let i = 0; i < 5; i++) {
      composer.rollSeed();
      again.push(composer.seed);
    }
    expect(again).toEqual(first);
    expect(new Set(first).size).toBe(5);
    expect(first.every((s) => s > 0)).toBe(true);
  });
});
