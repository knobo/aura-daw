/**
 * Loop-while-editing: opening a MIDI clip in the piano roll loops the clip
 * (solo or with the full mix), and closing the editor puts the transport and
 * solo states back exactly as they were. These tests pin the orchestration:
 * snapshot on enter, exclusive solo, and faithful restore on exit.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TransportState } from "../types/ipc";

const DEFAULTS: TransportState = {
  state: "stopped",
  positionSamples: 0,
  sampleRate: 48000,
  tempoBpm: 120,
  loopEnabled: false,
  loopStartSamples: 0,
  loopEndSamples: 0,
  songEndSamples: 0,
  stopAtEnd: true,
};

/** Stateful fake engine: each command mutates it and replies with a snapshot. */
const engine: TransportState = { ...DEFAULTS };

const setTrackSolo = vi.fn((_trackId: string, _soloed: boolean) => Promise.resolve());
const setTrackMute = vi.fn((_trackId: string, _muted: boolean) => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    transportPlay: vi.fn(() => {
      engine.state = "playing";
      return Promise.resolve({ ...engine });
    }),
    transportStop: vi.fn(() => {
      engine.state = "stopped";
      return Promise.resolve({ ...engine });
    }),
    transportSeek: vi.fn((positionSamples: number) => {
      engine.positionSamples = positionSamples;
      return Promise.resolve({ ...engine });
    }),
    transportSetLoop: vi.fn((enabled: boolean, s: number, e: number) => {
      engine.loopEnabled = enabled;
      engine.loopStartSamples = s;
      engine.loopEndSamples = e;
      return Promise.resolve({ ...engine });
    }),
    getTransportState: () => Promise.resolve({ ...engine }),
    setTrackSolo: (trackId: string, soloed: boolean) => setTrackSolo(trackId, soloed),
    setTrackMute: (trackId: string, muted: boolean) => setTrackMute(trackId, muted),
  },
}));

const { backend } = await import("../tauri");
const { transport } = await import("./transport.svelte");
const { project } = await import("./project.svelte");
const { clipEditLoop } = await import("./clip-edit-loop.svelte");
const { midi } = await import("./midi.svelte");
const { prefs } = await import("../prefs/prefs.svelte");

const mocked = backend as unknown as {
  transportPlay: ReturnType<typeof vi.fn>;
  transportStop: ReturnType<typeof vi.fn>;
  transportSeek: ReturnType<typeof vi.fn>;
  transportSetLoop: ReturnType<typeof vi.fn>;
};

function track(id: string, soloed = false, muted = false) {
  return {
    id,
    name: id,
    kind: "midi",
    gainDb: 0,
    pan: 0,
    muted,
    soloed,
    armed: false,
    color: "#888888",
  } as (typeof project.tracks)[number];
}

/** Put the fake engine (and the store mirror) into a given state. */
function setEngine(patch: Partial<TransportState> = {}) {
  Object.assign(engine, DEFAULTS, patch);
  transport.snap = { ...engine };
}

beforeEach(async () => {
  // Editor closed, engine stopped at 0, no loop, three tracks, B pre-soloed.
  await clipEditLoop.exit();
  vi.clearAllMocks();
  setEngine();
  project.tracks = [track("A"), track("B", true), track("C")];
  clipEditLoop.solo = false;
  prefs.restoreDefaults(); // autoplay pref back to its default (off)
});

const CLIP = { trackId: "A", startSamples: 1000, endSamples: 5000 };

describe("entering the clip editor", () => {
  it("loops the clip region and parks the playhead at its start — WITHOUT playing", async () => {
    await clipEditLoop.enter(CLIP);

    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 1000, 5000);
    expect(mocked.transportSeek).toHaveBeenCalledWith(1000);
    expect(mocked.transportPlay).not.toHaveBeenCalled();
  });

  it("starts playback on open when the clipOpenAutoplay preference is on", async () => {
    prefs.set("clipOpenAutoplay", true);
    await clipEditLoop.enter(CLIP);

    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 1000, 5000);
    expect(mocked.transportSeek).toHaveBeenCalledWith(1000);
    expect(mocked.transportPlay).toHaveBeenCalled();
  });

  it("solos the clip's track exclusively when the solo choice is on", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);

    expect(setTrackSolo).toHaveBeenCalledWith("A", true);
    expect(setTrackSolo).toHaveBeenCalledWith("B", false);
    // C was not soloed — no redundant call
    expect(setTrackSolo).not.toHaveBeenCalledWith("C", expect.anything());
    expect(project.trackById("A")?.soloed).toBe(true);
    expect(project.trackById("B")?.soloed).toBe(false);
  });

  it("touches nothing when the clip region is empty", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter({ trackId: "A", startSamples: 1000, endSamples: 1000 });

    expect(mocked.transportSetLoop).not.toHaveBeenCalled();
    expect(mocked.transportPlay).not.toHaveBeenCalled();
    expect(setTrackSolo).not.toHaveBeenCalled();
  });

  it("does not re-issue play when the transport is already rolling", async () => {
    prefs.set("clipOpenAutoplay", true);
    setEngine({ state: "playing", positionSamples: 200 });
    await clipEditLoop.enter(CLIP);

    expect(mocked.transportPlay).not.toHaveBeenCalled();
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 1000, 5000);
  });

  it("keeps an already-rolling transport rolling even with autoplay off", async () => {
    setEngine({ state: "playing", positionSamples: 200 });
    await clipEditLoop.enter(CLIP);

    expect(mocked.transportPlay).not.toHaveBeenCalled();
    expect(mocked.transportStop).not.toHaveBeenCalled();
  });
});

describe("toggling solo while editing", () => {
  it("restores the pre-edit solo states when switched off", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    setTrackSolo.mockClear();

    await clipEditLoop.setSolo(false);

    expect(setTrackSolo).toHaveBeenCalledWith("A", false);
    expect(setTrackSolo).toHaveBeenCalledWith("B", true);
    expect(setTrackSolo).not.toHaveBeenCalledWith("C", expect.anything());
    expect(clipEditLoop.solo).toBe(false);
  });

  it("applies exclusive solo when switched on mid-edit", async () => {
    await clipEditLoop.enter(CLIP);
    setTrackSolo.mockClear();

    await clipEditLoop.setSolo(true);

    expect(setTrackSolo).toHaveBeenCalledWith("A", true);
    expect(setTrackSolo).toHaveBeenCalledWith("B", false);
  });
});

describe("closing the clip editor", () => {
  it("restores loop, solo, playback state, and position", async () => {
    setEngine({ positionSamples: 777, loopStartSamples: 10, loopEndSamples: 20 });
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    vi.clearAllMocks();

    await clipEditLoop.exit();

    expect(mocked.transportSetLoop).toHaveBeenCalledWith(false, 10, 20);
    expect(mocked.transportStop).toHaveBeenCalled();
    expect(mocked.transportSeek).toHaveBeenCalledWith(777);
    expect(setTrackSolo).toHaveBeenCalledWith("A", false);
    expect(setTrackSolo).toHaveBeenCalledWith("B", true);
  });

  it("keeps the transport rolling when it was already playing on entry", async () => {
    setEngine({ state: "playing", positionSamples: 200 });
    await clipEditLoop.enter(CLIP);
    vi.clearAllMocks();

    await clipEditLoop.exit();

    expect(mocked.transportStop).not.toHaveBeenCalled();
    expect(mocked.transportSeek).not.toHaveBeenCalled();
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(false, 0, 0);
  });

  it("is a no-op when the editor was never entered", async () => {
    await clipEditLoop.exit();

    expect(mocked.transportSetLoop).not.toHaveBeenCalled();
    expect(mocked.transportStop).not.toHaveBeenCalled();
    expect(setTrackSolo).not.toHaveBeenCalled();
  });
});

describe("clip track that was manually muted (mute wins over solo)", () => {
  beforeEach(() => {
    project.tracks = [track("A", false, true), track("B", true), track("C", false, true)];
  });

  it("unmutes the clip's track while solo is on and re-mutes it on exit", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);

    expect(setTrackMute).toHaveBeenCalledWith("A", false);
    expect(project.trackById("A")?.muted).toBe(false);
    setTrackMute.mockClear();

    await clipEditLoop.exit();

    expect(setTrackMute).toHaveBeenCalledWith("A", true);
    expect(project.trackById("A")?.muted).toBe(true);
  });

  it("re-mutes the clip's track when solo toggles off mid-edit", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    setTrackMute.mockClear();

    await clipEditLoop.setSolo(false);

    expect(setTrackMute).toHaveBeenCalledWith("A", true);
  });

  it("never touches other manually muted tracks", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    await clipEditLoop.setSolo(false);
    await clipEditLoop.setSolo(true);
    await clipEditLoop.exit();

    expect(setTrackMute).not.toHaveBeenCalledWith("C", expect.anything());
    expect(project.trackById("C")?.muted).toBe(true);
  });

  it("leaves mute alone when the solo choice is off", async () => {
    clipEditLoop.solo = false;
    await clipEditLoop.enter(CLIP);
    await clipEditLoop.exit();

    expect(setTrackMute).not.toHaveBeenCalled();
  });

  it("re-mutes the old track when retargeting to a clip on another track", async () => {
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    setTrackMute.mockClear();

    await clipEditLoop.enter({ trackId: "C", startSamples: 8000, endSamples: 9000 });

    expect(setTrackMute).toHaveBeenCalledWith("A", true);
    expect(setTrackMute).toHaveBeenCalledWith("C", false);
  });
});

describe("piano roll wiring (midi store)", () => {
  // ppq 960 @ 120 BPM, 48 kHz → 25 samples per tick.
  const clip = {
    id: "c1",
    trackId: "A",
    name: "riff",
    timelineStartTicks: 960,
    lengthTicks: 960,
    notes: [],
  } as unknown as (typeof midi.clips)[number];

  it("opening a MIDI clip enters the loop with tick-converted bounds", async () => {
    midi.clips = [clip];
    // 120bpm section, superticks/quarter = 60/120 * 508_032_000.
    midi.sectionTable = [
      { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 },
    ];

    midi.open("c1");

    await vi.waitFor(() => expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 24000, 48000));
  });

  it("a looped clip (placement longer than content) loops the CONTENT, not the placement", async () => {
    // Placement is 4 beats (3840 ticks), content is 1 beat (960 ticks) —
    // the clip-edit loop must span only the content, per spec §6.
    midi.clips = [{ ...clip, lengthTicks: 3840, contentLengthTicks: 960 }];

    midi.open("c1");

    await vi.waitFor(() =>
      expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 24000, 24000 + 960 * 25),
    );
  });

  it("closing the editor restores the pre-edit state", async () => {
    midi.clips = [clip];
    midi.sectionTable = [
      { startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 },
    ];
    prefs.set("clipOpenAutoplay", true); // playback must be restored (stopped) on close
    midi.open("c1");
    await vi.waitFor(() => expect(mocked.transportPlay).toHaveBeenCalled());
    vi.clearAllMocks();

    midi.closeEditor();

    await vi.waitFor(() => expect(mocked.transportStop).toHaveBeenCalled());
    expect(mocked.transportSetLoop).toHaveBeenCalledWith(false, 0, 0);
    expect(mocked.transportSeek).toHaveBeenCalledWith(0);
  });
});

describe("reset (finding 8: adopting a different project)", () => {
  it("drops the snapshot without replaying any transport or solo writes", async () => {
    setEngine({ positionSamples: 777, loopStartSamples: 10, loopEndSamples: 20 });
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    expect(clipEditLoop.active).toBe(true);
    vi.clearAllMocks();

    clipEditLoop.reset();

    expect(clipEditLoop.active).toBe(false);
    expect(mocked.transportSetLoop).not.toHaveBeenCalled();
    expect(mocked.transportStop).not.toHaveBeenCalled();
    expect(mocked.transportSeek).not.toHaveBeenCalled();
    expect(setTrackSolo).not.toHaveBeenCalled();
    expect(setTrackMute).not.toHaveBeenCalled();
  });

  it("a later exit() (e.g. a stray close from the old editor) is a no-op after reset", async () => {
    await clipEditLoop.enter(CLIP);
    clipEditLoop.reset();
    vi.clearAllMocks();

    await clipEditLoop.exit();

    expect(mocked.transportSetLoop).not.toHaveBeenCalled();
    expect(setTrackSolo).not.toHaveBeenCalled();
  });

  it("is a no-op when the editor was never entered", () => {
    clipEditLoop.reset();
    expect(clipEditLoop.active).toBe(false);
  });
});

describe("switching clips while the editor is open", () => {
  it("retargets the loop but restores the original snapshot on exit", async () => {
    setEngine({ positionSamples: 42 });
    clipEditLoop.solo = true;
    await clipEditLoop.enter(CLIP);
    await clipEditLoop.enter({ trackId: "C", startSamples: 8000, endSamples: 9000 });

    expect(mocked.transportSetLoop).toHaveBeenCalledWith(true, 8000, 9000);
    expect(setTrackSolo).toHaveBeenCalledWith("C", true);
    expect(setTrackSolo).toHaveBeenCalledWith("A", false);
    vi.clearAllMocks();

    await clipEditLoop.exit();

    // restore is against the FIRST snapshot, not the retarget
    expect(mocked.transportSeek).toHaveBeenCalledWith(42);
    expect(setTrackSolo).toHaveBeenCalledWith("B", true);
    expect(setTrackSolo).toHaveBeenCalledWith("C", false);
    expect(project.trackById("A")?.soloed).toBe(false);
  });
});
