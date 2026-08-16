/**
 * Library store: folder navigation, the per-folder cache, and audition.
 * Drag/drop behaviour is Task 6's; these pin the browsing half.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LibraryEntry } from "../types/ipc";

const dirEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "dir",
  ext: "",
  sizeBytes: 0,
  modifiedMs: 0,
});
const fileEntry = (name: string, path: string): LibraryEntry => ({
  name,
  path,
  kind: "audio",
  ext: "wav",
  sizeBytes: 100,
  modifiedMs: 0,
});

const audioTrack = (id: string) => ({
  id,
  name: "Audio",
  kind: "audio" as const,
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  color: "#888888",
});
const midiTrack = (id: string) => ({
  id,
  name: "Synth",
  kind: "midi" as const,
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  color: "#888888",
});

const invokes = {
  libraryScan: vi.fn(async (dir: string): Promise<LibraryEntry[]> =>
    dir === "/lib" ? [dirEntry("drums", "/lib/drums"), fileEntry("kick.wav", "/lib/kick.wav")] : [],
  ),
  libraryDefaultRoot: vi.fn(async () => "/lib"),
  libraryAudition: vi.fn(async (_p: string, _s?: number | null) => {}),
  libraryAuditionStop: vi.fn(async () => {}),
  importAudioClip: vi.fn(async (_req: { path: string; trackId?: string | null; atSamples?: number | null }) => ({
    id: "c1",
  })),
  midiAddClip: vi.fn(async (trackId: string, name: string | null, timelineStartTicks: number, lengthTicks: number) => ({
    id: "k1-copy",
    trackId,
    name: name ?? "riff",
    timelineStartTicks,
    lengthTicks,
    notes: [],
  })),
  midiSetNotes: vi.fn(async (clipId: string, notes: unknown[]) => ({
    id: clipId,
    trackId: "m1",
    name: "riff",
    timelineStartTicks: 0,
    lengthTicks: 1920,
    notes,
  })),
  midiSetClipBounds: vi.fn(
    async (clipId: string, timelineStartTicks: number, lengthTicks: number, contentLengthTicks: number | null) => ({
      id: clipId,
      trackId: "m1",
      name: "riff",
      timelineStartTicks,
      lengthTicks,
      contentLengthTicks: contentLengthTicks ?? undefined,
      notes: [],
    }),
  ),
  gestureBegin: vi.fn(async (_label: string) => "gid-lib"),
  gestureEnd: vi.fn(async (_id?: string) => {}),
  getProjectState: vi.fn(() =>
    Promise.resolve({
      projectName: "Untitled",
      projectDir: null,
      transport: { sampleRate: 48000, tempoBpm: 120 },
      tracks: [],
      clips: [],
      ppq: 960,
      tempoEvents: [{ tick: 0, bpm: 120 }],
      midiClips: [],
    }),
  ),
  setTrackInstrument: vi.fn(async (trackId: string, instrumentId: string | null) => ({
    id: trackId,
    name: "Synth",
    kind: "midi" as const,
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    color: "#888888",
    instrumentId,
  })),
  samplerPreviewNote: vi.fn(async (_instrumentId: string, _key: number, _velocity: number) => {}),
  zynLoadPatch: vi.fn(async (_instanceId: string, _path: string) => {}),
  samplerListInstruments: vi.fn(async () => []),
  zynListPatches: vi.fn(async () => []),
  pluginList: vi.fn(async () => ({ plugins: [], instances: [], scanned: true })),
};

const mockBackend = { mode: "tauri" as const, on: () => () => {}, ...invokes };
vi.mock("../tauri", () => ({ backend: mockBackend }));
vi.mock("@tauri-apps/api/path", () => ({
  join: async (...parts: string[]) => parts.join("/"),
}));

const { prefs } = await import("../prefs/prefs.svelte");
const { library } = await import("./library.svelte");
const { project } = await import("./project.svelte");
const { midi } = await import("./midi.svelte");
const { toasts } = await import("./toasts.svelte");
const { plugins } = await import("./plugins.svelte");

function lastToast() {
  return toasts.list[toasts.list.length - 1];
}

beforeEach(() => {
  vi.clearAllMocks();
  prefs.set("librarySampleFolders", []);
  library.reset();
  project.tracks = [];
  toasts.list = [];
  plugins.instances = [];
});

describe("library store", () => {
  it("resolves the default root once and lists it first among the folders", async () => {
    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
    prefs.set("librarySampleFolders", ["/extra", "/lib"]);
    expect(library.folders).toEqual(["/lib", "/extra"]);

    await library.init();
    expect(invokes.libraryDefaultRoot).toHaveBeenCalledTimes(1);
  });

  it("opens a folder, exposes its entries, and serves a re-open from cache", async () => {
    await library.init();
    await library.open("/lib");
    expect(library.cwd).toBe("/lib");
    expect(library.entries.map((e) => e.name)).toEqual(["drums", "kick.wav"]);
    expect(library.loading).toBe(false);
    expect(library.error).toBeNull();

    await library.open("/lib/drums");
    await library.open("/lib");
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("refresh() busts the cache for the current folder", async () => {
    await library.init();
    await library.open("/lib");
    await library.refresh();
    expect(invokes.libraryScan).toHaveBeenCalledTimes(2);
  });

  it("surfaces a scan failure as an error and leaves the entries empty", async () => {
    invokes.libraryScan.mockRejectedValueOnce(new Error("cannot read /nope"));
    await library.open("/nope");
    expect(library.error).toContain("cannot read /nope");
    expect(library.entries).toEqual([]);
    expect(library.loading).toBe(false);
  });

  it("up() walks to the parent and clears at a configured root", async () => {
    await library.init();
    await library.open("/lib/drums");
    await library.up();
    expect(library.cwd).toBe("/lib");
    await library.up();
    expect(library.cwd).toBeNull();
  });

  it("audition() plays a file and records which one is sounding", async () => {
    await library.audition("/lib/kick.wav");
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/kick.wav", null);
    expect(library.auditioning).toBe("/lib/kick.wav");

    await library.stopAudition();
    expect(invokes.libraryAuditionStop).toHaveBeenCalledTimes(1);
    expect(library.auditioning).toBe("");
  });

  it("clears the auditioning marker when the backend rejects, without touching the listing", async () => {
    await library.init();
    await library.open("/lib");
    const before = library.entries;

    invokes.libraryAudition.mockRejectedValueOnce(new Error("no output device"));
    await library.audition("/lib/kick.wav");
    expect(library.auditioning).toBe("");
    expect(library.auditionError).toContain("no output device");
    expect(library.error).toBeNull();
    expect(library.entries).toBe(before);
    expect(library.entries.map((e) => e.name)).toEqual(["drums", "kick.wav"]);
  });
});

describe("dropOnTrack — sample file", () => {
  it("imports the file onto the target track at the drop position", async () => {
    project.tracks = [
      { id: "t1", name: "Audio 1", kind: "audio", gainDb: 0, pan: 0, muted: false, soloed: false, armed: false, color: "#888888" },
    ];
    await library.dropOnTrack(
      { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" },
      "t1",
      96000,
    );
    expect(invokes.importAudioClip).toHaveBeenCalledWith({
      path: "/lib/kick.wav",
      trackId: "t1",
      atSamples: 96000,
    });
  });

  it("refuses a MIDI track and never calls the import command", async () => {
    project.tracks = [
      { id: "m1", name: "Synth", kind: "midi", gainDb: 0, pan: 0, muted: false, soloed: false, armed: false, color: "#888888" },
    ];
    await library.dropOnTrack(
      { kind: "sampleFile", path: "/lib/kick.wav", name: "kick.wav" },
      "m1",
      0,
    );
    expect(invokes.importAudioClip).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("AUDIO TRACK");
  });
});

describe("dropOnTrack — project clips", () => {
  it("re-imports an audio project clip from its absolute source path", async () => {
    project.projectDir = "/home/u/Music/AURA/Song.aura";
    project.tracks = [audioTrack("t1")];
    project.clips = [
      { id: "c1", trackId: "t1", name: "loop", sourcePath: "audio/abc.wav",
        sourceChannels: 2, sourceSampleRate: 48000, sourceLengthSamples: 48000,
        timelineStartSamples: 0, offsetSamples: 0, lengthSamples: 48000,
        gainDb: 0, fadeInSamples: 0, fadeOutSamples: 0 },
    ];

    await library.dropOnTrack({ kind: "projectAudioClip", clipId: "c1" }, "t1", 24000);

    expect(invokes.importAudioClip).toHaveBeenCalledWith({
      path: "/home/u/Music/AURA/Song.aura/audio/abc.wav",
      trackId: "t1",
      atSamples: 24000,
    });
  });

  it("stamps a MIDI project clip inside ONE gesture, so it is one undo step", async () => {
    project.tracks = [midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920,
        notes: [{ tick: 0, key: 60, velocity: 100, lengthTicks: 480 }] },
    ];

    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "m1", 0);

    expect(invokes.gestureBegin).toHaveBeenCalledTimes(1);
    expect(invokes.midiAddClip).toHaveBeenCalledTimes(1);
    expect(invokes.midiSetNotes).toHaveBeenCalledTimes(1);
    expect(invokes.gestureEnd).toHaveBeenCalledTimes(1);
    expect(invokes.gestureEnd).toHaveBeenCalledWith("gid-lib");
  });

  it("closes the gesture even when the stamp fails", async () => {
    project.tracks = [midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920, notes: [] },
    ];
    invokes.midiAddClip.mockRejectedValueOnce(new Error("boom"));

    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "m1", 0);
    expect(invokes.gestureEnd).toHaveBeenCalledTimes(1);
    expect(invokes.gestureEnd).toHaveBeenCalledWith("gid-lib");
  });

  it("refuses a MIDI clip on an audio track and an audio clip on a MIDI track", async () => {
    project.tracks = [audioTrack("t1"), midiTrack("m1")];
    midi.clips = [
      { id: "k1", trackId: "m1", name: "riff", timelineStartTicks: 0, lengthTicks: 1920, notes: [] },
    ];
    await library.dropOnTrack({ kind: "projectMidiClip", clipId: "k1" }, "t1", 0);
    expect(invokes.midiAddClip).not.toHaveBeenCalled();

    project.projectDir = "/home/u/Music/AURA/Song.aura";
    project.clips = [
      { id: "c1", trackId: "t1", name: "loop", sourcePath: "audio/abc.wav",
        sourceChannels: 2, sourceSampleRate: 48000, sourceLengthSamples: 48000,
        timelineStartSamples: 0, offsetSamples: 0, lengthSamples: 48000,
        gainDb: 0, fadeInSamples: 0, fadeOutSamples: 0 },
    ];
    await library.dropOnTrack({ kind: "projectAudioClip", clipId: "c1" }, "m1", 0);
    expect(invokes.importAudioClip).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("AUDIO TRACK");
  });
});

describe("dropOnTrack — presets", () => {
  it("binds a sampler instrument to a MIDI track", async () => {
    project.tracks = [midiTrack("m1")];
    await library.dropOnTrack(
      { kind: "samplerInstrument", instrumentId: "inst-1", name: "Piano" },
      "m1",
      0,
    );
    expect(invokes.setTrackInstrument).toHaveBeenCalledWith("m1", "inst-1");
  });

  it("refuses to bind an instrument to an audio track", async () => {
    project.tracks = [audioTrack("t1")];
    await library.dropOnTrack(
      { kind: "samplerInstrument", instrumentId: "inst-1", name: "Piano" },
      "t1",
      0,
    );
    expect(invokes.setTrackInstrument).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("MIDI TRACK");
  });

  it("loads a Zyn patch into the instance the target track already hosts", async () => {
    project.tracks = [{ ...midiTrack("m1"), instrumentId: "plugin:zyn-1" }];
    plugins.instances = [
      {
        id: "zyn-1",
        uid: "lv2:http://zynaddsubfx.sf.net",
        name: "ZynAddSubFX",
        format: "lv2",
        trackId: "m1",
        status: "active",
      },
    ];
    const patch = { bank: "Arpeggios", name: "Arp 1", program: 1, path: "/a.xiz" };

    await library.dropOnTrack({ kind: "zynPatch", patch }, "m1", 0);
    expect(invokes.zynLoadPatch).toHaveBeenCalledWith("zyn-1", "/a.xiz");
    // A patch load changes nothing on screen, so the drop has to say so —
    // every REFUSED drop toasts, and a silent success reads as "broken".
    expect(lastToast().title).toContain("PATCH LOADED");
    expect(lastToast().lines.join(" ")).toContain("Arpeggios / Arp 1");
    // The engine retires the live node off the state commit itself, so the
    // store no longer re-binds the track to force a graph rebuild — that
    // workaround cost a second, confusing undo entry per patch load.
    expect(invokes.setTrackInstrument).not.toHaveBeenCalled();
  });

  it("toasts instead of instantiating a plugin when the track hosts no Zyn", async () => {
    project.tracks = [midiTrack("m1")];
    plugins.instances = [];
    await library.dropOnTrack(
      { kind: "zynPatch", patch: { bank: "B", name: "P", program: 0, path: "/p.xiz" } },
      "m1",
      0,
    );
    expect(invokes.zynLoadPatch).not.toHaveBeenCalled();
    expect(lastToast().title).toContain("ZYN");
  });
});
