/**
 * AI Studio job orchestration over the open-kind `sidecar_run_job` surface
 * (sidecar-job-v2). Streams NDJSON-derived events (progress/log/done/error),
 * then lands results: audio → project reload (control-plane auto-import),
 * amtInfill → note merge into the target clip, stableAudioSfz → instrument
 * bank refresh.
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { instruments } from "./instruments.svelte";
import { jobs } from "./jobs.svelte";
import type {
  AmtInfillResult,
  MidiNote,
  OpenSidecarEvent,
  SidecarJobState,
} from "../types/ipc";

export interface GenJob {
  jobId: string;
  kind: string;
  label: string;
  state: SidecarJobState;
  progress: number;
  stage: string;
  log: string[];
  error?: string;
  /** One-liner shown when the job lands ("clip on Track 5", "instrument …"). */
  outcome?: string;
  startedAt: number;
  /**
   * Who started it. `"ui"` jobs are driven by the per-invoke Channel their
   * flow opened; `"agent"` jobs were started over MCP and adopted off the
   * `sidecar://*` app-event bus (see `init`).
   */
  origin: "ui" | "agent";
}

const LOG_MAX = 120;

/**
 * How long to wait before adopting an unknown job id. A UI-launched job's
 * first events can beat its own `sidecar_run_job` reply — the id is on the
 * bus before the store has it — so a grace window keeps us from adopting a
 * job that a UI flow is about to claim (which would show it twice).
 */
const ADOPT_GRACE_MS = 400;

export const KIND_LABEL: Record<string, string> = {
  aceStepGenerate: "ACE-STEP · GENERATE",
  aceStepRepaint: "ACE-STEP · REPAINT",
  aceStepAudio2Audio: "ACE-STEP · A2A",
  elevenLabsMusic: "ELEVENLABS · MUSIC",
  amtInfill: "AMT · INFILL",
  stableAudioSfz: "STABLE AUDIO · SFZ",
  stemSplit: "STEMS",
  transcribe: "WHISPER",
};

class GenerationStore {
  jobs = $state<Record<string, GenJob>>({});
  private unsubs: Array<() => void> = [];
  private adopting = new Set<string>();

  /**
   * Show jobs no UI flow started — an agent's `run_sidecar_job` over MCP.
   *
   * Every job's progress/done/error reaches the `sidecar://*` app-event bus
   * regardless of front door (`sidecars::make_sink` for UI-launched jobs,
   * `ControlPlane::app_event_sink` for MCP-launched ones), so a job id that
   * neither this store nor the stems store owns means an agent is driving.
   * UI-launched jobs are deliberately ignored here — their Channel already
   * owns them, and handling both would double-count and double-land.
   */
  init() {
    if (this.unsubs.length) return;
    this.unsubs = [
      backend.on("sidecar://progress", (e) => {
        if (this.jobs[e.jobId]?.origin === "agent") {
          this.patch(e.jobId, { progress: e.progress, stage: e.stage, state: "running" });
        } else if (!this.owned(e.jobId)) {
          this.adopt(e.jobId);
        }
      }),
      backend.on("sidecar://error", (e) => {
        if (this.jobs[e.jobId]?.origin !== "agent") return;
        this.patch(e.jobId, { state: "error", error: e.message, stage: "error" });
        this.appendLog(e.jobId, `error: ${e.message}`);
      }),
      backend.on("sidecar://done", (e) => {
        if (this.jobs[e.jobId]?.origin === "agent") {
          this.patch(e.jobId, { state: "done", progress: 1, stage: "done" });
          void this.land(e.jobId, e.result as unknown as Record<string, unknown>);
        } else if (!this.owned(e.jobId)) {
          // An agent job that finished inside the adopt grace window: nothing
          // left to show. A clip it auto-imported announces itself over
          // `project://changed`; a sampler instrument needs a bank re-pull.
          void instruments.refresh();
        }
      }),
    ];
  }

  dispose() {
    for (const off of this.unsubs) off();
    this.unsubs = [];
  }

  /** Is this job already owned by a UI flow (this store, or stem splits)? */
  private owned(jobId: string): boolean {
    return jobId in this.jobs || jobId in jobs.jobs;
  }

  private adopt(jobId: string) {
    if (this.adopting.has(jobId)) return;
    this.adopting.add(jobId);
    setTimeout(() => void this.adoptNow(jobId), ADOPT_GRACE_MS);
  }

  /** Ask the backend what this job actually is, then take it on. */
  private async adoptNow(jobId: string) {
    this.adopting.delete(jobId);
    if (this.owned(jobId)) return; // a UI flow claimed it while we waited
    let status;
    try {
      status = await backend.sidecarJobStatus(jobId);
    } catch (err) {
      console.warn("[aura] could not adopt agent job", jobId, err);
      return;
    }
    if (this.owned(jobId)) return;
    if (status.state !== "queued" && status.state !== "running") return; // over already
    this.jobs = {
      ...this.jobs,
      [jobId]: {
        jobId,
        kind: status.kind,
        label: `agent · ${KIND_LABEL[status.kind] ?? status.kind}`,
        state: status.state,
        progress: status.progress,
        stage: status.stage,
        log: [],
        startedAt: Date.now(),
        origin: "agent",
      },
    };
  }

  get all(): GenJob[] {
    return Object.values(this.jobs).sort((a, b) => b.startedAt - a.startedAt);
  }
  get active(): GenJob[] {
    return this.all.filter((j) => j.state === "queued" || j.state === "running");
  }

  private patch(jobId: string, patch: Partial<GenJob>) {
    const job = this.jobs[jobId];
    if (!job) return;
    this.jobs = { ...this.jobs, [jobId]: { ...job, ...patch } };
  }

  private appendLog(jobId: string, line: string) {
    const job = this.jobs[jobId];
    if (!job) return;
    const log = [...job.log, line].slice(-LOG_MAX);
    this.jobs = { ...this.jobs, [jobId]: { ...job, log } };
  }

  /**
   * Submit a job. `amtTarget` carries the clip/region an amtInfill result
   * merges back into (context notes stay, region notes are replaced).
   */
  async run(
    kind: string,
    params: Record<string, unknown>,
    label: string,
    amtTarget?: { clipId: string; startTicks: number; endTicks: number },
  ): Promise<string | null> {
    let jobId = "";
    const early: OpenSidecarEvent[] = [];
    const handle = (e: OpenSidecarEvent) => {
      if (!jobId) {
        early.push(e);
        return;
      }
      this.onEvent(e, amtTarget);
    };

    try {
      jobId = await backend.sidecarRunJob(kind, params, handle);
    } catch (err) {
      console.error("[aura] sidecar_run_job failed:", err);
      const failId = `local-${Date.now()}`;
      this.jobs = {
        ...this.jobs,
        [failId]: {
          jobId: failId,
          kind,
          label,
          state: "error",
          progress: 0,
          stage: "rejected",
          log: [String(err)],
          error: String(err),
          startedAt: Date.now(),
          origin: "ui",
        },
      };
      return null;
    }

    this.jobs = {
      ...this.jobs,
      [jobId]: {
        jobId,
        kind,
        label,
        state: "queued",
        progress: 0,
        stage: "queued",
        log: [],
        startedAt: Date.now(),
        origin: "ui",
      },
    };
    for (const e of early) this.onEvent(e, amtTarget);
    return jobId;
  }

  async cancel(jobId: string) {
    try {
      await backend.sidecarCancelJob(jobId);
    } catch (err) {
      console.warn("[aura] sidecar_cancel_job failed:", err);
    }
    this.patch(jobId, { state: "cancelled", stage: "cancelled" });
  }

  dismiss(jobId: string) {
    const { [jobId]: _gone, ...rest } = this.jobs;
    this.jobs = rest;
  }

  private onEvent(
    e: OpenSidecarEvent,
    amtTarget?: { clipId: string; startTicks: number; endTicks: number },
  ) {
    switch (e.type) {
      case "progress":
        this.patch(e.jobId, { progress: e.progress, stage: e.stage, state: "running" });
        break;
      case "log":
        this.appendLog(e.jobId, e.line);
        break;
      case "error": {
        const cancelled = this.jobs[e.jobId]?.state === "cancelled";
        this.patch(e.jobId, {
          state: cancelled ? "cancelled" : "error",
          error: e.message,
          stage: cancelled ? "cancelled" : "error",
        });
        this.appendLog(e.jobId, `error: ${e.message}`);
        break;
      }
      case "done":
        this.patch(e.jobId, { state: "done", progress: 1, stage: "done" });
        void this.land(e.jobId, e.result, amtTarget);
        break;
    }
  }

  /** Materialize a finished job's result in the stores. */
  private async land(
    jobId: string,
    result: Record<string, unknown>,
    amtTarget?: { clipId: string; startTicks: number; endTicks: number },
  ) {
    const kind = String(result.kind ?? this.jobs[jobId]?.kind ?? "");
    if (kind === "amtInfill" && amtTarget) {
      const r = result as unknown as AmtInfillResult;
      const clip = midi.clipById(amtTarget.clipId);
      if (clip && Array.isArray(r.notes)) {
        const context = clip.notes.filter(
          (n) => n.tick + n.lengthTicks <= amtTarget.startTicks || n.tick >= amtTarget.endTicks,
        );
        const filled = (r.notes as MidiNote[]).filter(
          (n) => n.tick >= amtTarget.startTicks && n.tick < amtTarget.endTicks,
        );
        await midi.setNotes(clip.id, [...context, ...filled]);
        this.patch(jobId, { outcome: `${filled.length} notes → ${clip.name}` });
      }
    } else if (kind === "stableAudioSfz") {
      // Control plane auto-loads the .sfz; mirror the bank.
      await instruments.refresh();
      const name = typeof result.name === "string" ? result.name : "instrument";
      this.patch(jobId, { outcome: `instrument "${name}" ready` });
    } else {
      // audio kinds: the importToTrackId convenience landed a clip — re-pull.
      await project.reload();
      const out = typeof result.outputPath === "string" ? result.outputPath : "";
      this.patch(jobId, { outcome: out ? `imported ${out.split("/").pop()}` : "audio ready" });
    }
  }
}

export const generation = new GenerationStore();
