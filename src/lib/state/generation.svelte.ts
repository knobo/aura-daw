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
}

const LOG_MAX = 120;

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
