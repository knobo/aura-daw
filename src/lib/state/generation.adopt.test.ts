/**
 * Adoption of agent-launched jobs (MCP front-door parity).
 *
 * The backend fans every sidecar job onto the `sidecar://*` app-event bus
 * whichever front door started it, so the frontend can only tell an agent's
 * job from its own by ownership: an id no UI flow holds is an agent's. These
 * tests pin that rule and the grace window that keeps a UI job — whose events
 * can beat its own submit reply — from being adopted and shown twice.
 */

import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import type { AuraEventName } from "../types/ipc";

type Listener = (payload: unknown) => void;
const listeners = new Map<string, Set<Listener>>();

/** Emit an app event the way the Tauri/demo backends do. */
function emit(event: AuraEventName, payload: unknown) {
  for (const cb of listeners.get(event) ?? []) cb(payload);
}

const sidecarJobStatus = vi.fn();
const instrumentsRefreshed = vi.fn();

vi.mock("../tauri", () => ({
  backend: {
    on(event: string, cb: Listener) {
      let subs = listeners.get(event);
      if (!subs) listeners.set(event, (subs = new Set()));
      subs.add(cb);
      return () => subs!.delete(cb);
    },
    sidecarJobStatus: (jobId: string) => sidecarJobStatus(jobId),
    sidecarCancelJob: vi.fn(),
    listInstruments: () => {
      instrumentsRefreshed();
      return Promise.resolve([]);
    },
    getProjectState: () => Promise.resolve({ tracks: [], clips: [] }),
  },
}));

const { generation } = await import("./generation.svelte");
const { jobs } = await import("./jobs.svelte");

/** The status reply for a job an agent is running. */
function running(kind: string, progress = 0.25) {
  return { jobId: "j-agent", kind, state: "running", progress, stage: "generating" };
}

beforeEach(() => {
  vi.useFakeTimers();
  generation.jobs = {};
  jobs.jobs = {};
  sidecarJobStatus.mockReset();
  generation.init();
});

afterEach(() => {
  generation.dispose();
  listeners.clear();
  vi.useRealTimers();
});

describe("agent job adoption", () => {
  it("adopts a job no UI flow owns and shows it as agent-driven", async () => {
    sidecarJobStatus.mockResolvedValue(running("aceStepGenerate"));

    emit("sidecar://progress", {
      type: "progress",
      jobId: "j-agent",
      progress: 0.25,
      stage: "generating",
    });
    expect(generation.active, "not adopted before the grace window").toHaveLength(0);

    await vi.advanceTimersByTimeAsync(500);

    const [job] = generation.active;
    expect(job).toBeDefined();
    expect(job.jobId).toBe("j-agent");
    expect(job.origin).toBe("agent");
    expect(job.kind).toBe("aceStepGenerate");
    expect(job.label).toBe("agent · ACE-STEP · GENERATE");
    expect(job.progress).toBe(0.25);
  });

  it("keeps tracking an adopted job through progress, then done", async () => {
    sidecarJobStatus.mockResolvedValue(running("aceStepGenerate"));
    emit("sidecar://progress", { type: "progress", jobId: "j-agent", progress: 0.25, stage: "a" });
    await vi.advanceTimersByTimeAsync(500);

    emit("sidecar://progress", { type: "progress", jobId: "j-agent", progress: 0.8, stage: "decoding" });
    expect(generation.jobs["j-agent"].progress).toBe(0.8);
    expect(generation.jobs["j-agent"].stage).toBe("decoding");

    emit("sidecar://done", {
      type: "done",
      jobId: "j-agent",
      result: { kind: "aceStepGenerate", outputPath: "/tmp/out.wav" },
    });
    expect(generation.jobs["j-agent"].state).toBe("done");
    expect(generation.active, "a finished job leaves the JOBS box").toHaveLength(0);
  });

  it("does not adopt a job this store claims during the grace window", async () => {
    sidecarJobStatus.mockResolvedValue(running("aceStepGenerate"));

    emit("sidecar://progress", { type: "progress", jobId: "j-agent", progress: 0.1, stage: "a" });
    // The submit reply lands mid-window — this is the race the window exists for.
    generation.jobs = {
      ...generation.jobs,
      "j-agent": {
        jobId: "j-agent",
        kind: "aceStepGenerate",
        label: "my prompt",
        state: "running",
        progress: 0.1,
        stage: "a",
        log: [],
        startedAt: 1,
        origin: "ui",
      },
    };
    await vi.advanceTimersByTimeAsync(500);

    expect(Object.keys(generation.jobs), "no duplicate row").toHaveLength(1);
    expect(generation.jobs["j-agent"].label).toBe("my prompt");
    expect(generation.jobs["j-agent"].origin).toBe("ui");
    expect(sidecarJobStatus, "never even asked about a job we own").not.toHaveBeenCalled();
  });

  it("leaves stem-split jobs to the stems store", async () => {
    sidecarJobStatus.mockResolvedValue(running("stemSplit"));
    jobs.jobs = {
      "j-stems": {
        jobId: "j-stems",
        kind: "stemSplit",
        clipId: "c1",
        trackId: "t1",
        progress: 0,
        stage: "queued",
        state: "running",
      },
    };

    emit("sidecar://progress", { type: "progress", jobId: "j-stems", progress: 0.5, stage: "sep" });
    await vi.advanceTimersByTimeAsync(500);

    expect(generation.jobs, "the stems store owns it").toEqual({});
  });

  it("does not resurrect a job that already finished", async () => {
    sidecarJobStatus.mockResolvedValue({
      jobId: "j-agent",
      kind: "aceStepGenerate",
      state: "done",
      progress: 1,
      stage: "done",
    });

    emit("sidecar://progress", { type: "progress", jobId: "j-agent", progress: 0.9, stage: "a" });
    await vi.advanceTimersByTimeAsync(500);

    expect(generation.jobs, "nothing left to show").toEqual({});
  });

  it("ignores UI-launched jobs entirely — their channel owns them", async () => {
    generation.jobs = {
      "j-ui": {
        jobId: "j-ui",
        kind: "aceStepGenerate",
        label: "my prompt",
        state: "running",
        progress: 0.2,
        stage: "a",
        log: [],
        startedAt: 1,
        origin: "ui",
      },
    };

    emit("sidecar://progress", { type: "progress", jobId: "j-ui", progress: 0.9, stage: "late" });
    await vi.advanceTimersByTimeAsync(500);

    expect(
      generation.jobs["j-ui"].progress,
      "the app-event bus must not double-drive a channel-owned job",
    ).toBe(0.2);
  });

  it("asks for only one status per unknown job however many events arrive", async () => {
    sidecarJobStatus.mockResolvedValue(running("elevenLabsMusic"));

    for (let i = 0; i < 5; i++) {
      emit("sidecar://progress", { type: "progress", jobId: "j-agent", progress: i / 5, stage: "s" });
    }
    await vi.advanceTimersByTimeAsync(500);

    expect(sidecarJobStatus).toHaveBeenCalledTimes(1);
    expect(generation.active).toHaveLength(1);
  });
});
