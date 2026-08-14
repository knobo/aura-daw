/**
 * SPLIT STEMS (Task 11): the gesture must be one invoke to the real backend
 * command (`split_stems_for_clip`) with the selected clip's id — the
 * frontend no longer invents clips/tracks itself. Stems land through the
 * backend's own project://changed announcements (mirrored here by
 * `getProjectState`/`reload`), never by the store writing `project.clips`
 * or minting tracks directly.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Clip, SidecarEvent } from "../types/ipc";

const invokes = {
  splitStemsForClip: vi.fn(
    (_clipId: string, _onEvent: (e: SidecarEvent) => void) => Promise.resolve("job-1"),
  ),
  getProjectState: vi.fn(() =>
    Promise.resolve({
      projectName: "Song",
      projectDir: "/p/Song.aura",
      transport: { sampleRate: 48000, tempoBpm: 120 },
      tracks: [],
      clips: [],
      ppq: 960,
      tempoEvents: [{ tick: 0, bpm: 120 }],
      midiClips: [],
    }),
  ),
  sidecarCancelJob: vi.fn(() => Promise.resolve()),
};

const mockBackend = {
  mode: "tauri" as "tauri" | "demo",
  on: () => () => {},
  ...invokes,
};

vi.mock("../tauri", () => ({ backend: mockBackend }));

const { project } = await import("./project.svelte");
const { jobs } = await import("./jobs.svelte");

function makeClip(overrides: Partial<Clip> = {}): Clip {
  return {
    id: "clip-1",
    trackId: "track-1",
    name: "Take 1",
    sourcePath: "audio/take1.wav",
    sourceChannels: 2,
    sourceSampleRate: 44_100,
    sourceLengthSamples: 44_100,
    timelineStartSamples: 0,
    offsetSamples: 0,
    lengthSamples: 44_100,
    gainDb: 0,
    fadeInSamples: 0,
    fadeOutSamples: 0,
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mockBackend.mode = "tauri";
  project.tracks = [];
  project.clips = [];
  project.projectDir = "/p/Song.aura";
  jobs.jobs = {};
});

describe("jobs.splitStems", () => {
  it("invokes split_stems_for_clip with the selected clip's id", async () => {
    const clip = makeClip();

    await jobs.splitStems(clip);

    expect(invokes.splitStemsForClip).toHaveBeenCalledTimes(1);
    expect(invokes.splitStemsForClip.mock.calls[0]![0]).toBe("clip-1");
  });

  it("does not touch project.clips directly — no local clip/track invention", async () => {
    const clip = makeClip();
    project.clips = [clip];
    const clipsBefore = project.clips;
    const addTrackSpy = vi.spyOn(project, "addTrack");

    await jobs.splitStems(clip);

    // Same array reference: the store never assigned a new `clips` list
    // itself while submitting the job (the backend's project://changed /
    // reload() path is what mutates it, exercised separately below).
    expect(project.clips).toBe(clipsBefore);
    expect(addTrackSpy).not.toHaveBeenCalled();
    // The deleted frontend-invention API must actually be gone.
    expect((project as unknown as Record<string, unknown>).createClip).toBeUndefined();
    expect((project as unknown as Record<string, unknown>).placeTrackAfter).toBeUndefined();
  });

  it("re-pulls the project when the backend announces a landed stem", async () => {
    const clip = makeClip();
    let captured: ((e: SidecarEvent) => void) | undefined;
    invokes.splitStemsForClip.mockImplementationOnce((_clipId, onEvent) => {
      captured = onEvent;
      return Promise.resolve("job-2");
    });

    await jobs.splitStems(clip);
    expect(captured).toBeDefined();

    captured!({ type: "done", jobId: "job-2", result: { kind: "stemSplit", stems: {} } });
    captured!({
      type: "log",
      jobId: "job-2",
      line: "auto-import stem `drums`: clip c2 on track t2 (t2)",
    });

    expect(invokes.getProjectState).toHaveBeenCalled();
  });

  it("tracks job progress under the clip's job id", async () => {
    const clip = makeClip();
    let captured: ((e: SidecarEvent) => void) | undefined;
    invokes.splitStemsForClip.mockImplementationOnce((_clipId, onEvent) => {
      captured = onEvent;
      return Promise.resolve("job-3");
    });

    await jobs.splitStems(clip);
    captured!({ type: "progress", jobId: "job-3", progress: 0.5, stage: "separating" });

    expect(jobs.jobs["job-3"]).toMatchObject({
      clipId: "clip-1",
      trackId: "track-1",
      progress: 0.5,
      stage: "separating",
      state: "running",
    });
  });

  it("does not submit a second job while one is already running for the clip", async () => {
    const clip = makeClip();
    jobs.jobs = {
      "existing-job": {
        jobId: "existing-job",
        kind: "stemSplit",
        clipId: "clip-1",
        trackId: "track-1",
        progress: 0.3,
        stage: "separating",
        state: "running",
      },
    };

    await jobs.splitStems(clip);

    expect(invokes.splitStemsForClip).not.toHaveBeenCalled();
  });
});
