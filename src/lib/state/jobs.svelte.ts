/**
 * Sidecar job tracking + the "AI Split Stems" flow: submit, stream progress
 * over the Channel, and on completion materialize four stem tracks/clips
 * under the source track.
 */

import { backend } from "../tauri";
import type { Clip, SidecarEvent, StemName } from "../types/ipc";
import { project } from "./project.svelte";

export interface StemJob {
  jobId: string;
  kind: "stemSplit" | "transcribe";
  clipId: string;
  trackId: string;
  progress: number;
  stage: string;
  state: "queued" | "running" | "done" | "error" | "cancelled";
  error?: string;
}

const STEM_ORDER: StemName[] = ["drums", "bass", "vocals", "other"];
const STEM_LABEL: Record<StemName, string> = {
  drums: "Drums",
  bass: "Bass",
  vocals: "Vocals",
  other: "Other",
};
const STEM_COLOR: Record<StemName, string> = {
  drums: "#ffc857",
  bass: "#ff4fd8",
  vocals: "#52e5ff",
  other: "#9d7bff",
};

class JobsStore {
  jobs = $state<Record<string, StemJob>>({});

  get active(): StemJob[] {
    return Object.values(this.jobs).filter(
      (j) => j.state === "queued" || j.state === "running",
    );
  }

  jobForClip(clipId: string): StemJob | null {
    return (
      Object.values(this.jobs).find(
        (j) => j.clipId === clipId && (j.state === "running" || j.state === "queued"),
      ) ?? null
    );
  }

  private patch(jobId: string, patch: Partial<StemJob>) {
    const job = this.jobs[jobId];
    if (!job) return;
    this.jobs = { ...this.jobs, [jobId]: { ...job, ...patch } };
  }

  async splitStems(clip: Clip): Promise<void> {
    if (this.jobForClip(clip.id)) return; // one job per clip at a time

    let jobId = "";
    const pending: SidecarEvent[] = [];
    const handle = (e: SidecarEvent) => {
      if (!jobId) {
        pending.push(e);
        return;
      }
      this.onEvent(clip, e);
    };

    try {
      jobId = await backend.sidecarSplitStems({ inputPath: clip.sourcePath }, handle);
    } catch (err) {
      console.error("[aura] sidecar_split_stems failed:", err);
      return;
    }

    this.jobs = {
      ...this.jobs,
      [jobId]: {
        jobId,
        kind: "stemSplit",
        clipId: clip.id,
        trackId: clip.trackId,
        progress: 0,
        stage: "queued",
        state: "queued",
      },
    };
    for (const e of pending) this.onEvent(clip, e);
  }

  async cancel(jobId: string) {
    await backend.sidecarCancelJob(jobId);
    this.patch(jobId, { state: "cancelled" });
  }

  private onEvent(sourceClip: Clip, e: SidecarEvent) {
    switch (e.type) {
      case "progress":
        this.patch(e.jobId, { progress: e.progress, stage: e.stage, state: "running" });
        break;
      case "log":
        break;
      case "error": {
        const wasCancelled = this.jobs[e.jobId]?.state === "cancelled";
        this.patch(e.jobId, {
          state: wasCancelled ? "cancelled" : "error",
          error: e.message,
        });
        break;
      }
      case "done": {
        this.patch(e.jobId, { state: "done", progress: 1, stage: "done" });
        if (e.result.kind === "stemSplit") {
          void this.materializeStems(sourceClip, e.result.stems);
        }
        break;
      }
    }
  }

  /** Turn a finished split into four stem tracks below the source track. */
  private async materializeStems(
    source: Clip,
    stems: Partial<Record<StemName, string>>,
  ) {
    let after = source.trackId;
    for (const stem of STEM_ORDER) {
      const path = stems[stem];
      if (!path) continue;
      try {
        const track = await project.addTrack({
          name: `${STEM_LABEL[stem]} · ${source.name}`,
          color: STEM_COLOR[stem],
        });
        project.placeTrackAfter(track, after);
        after = track.id;
        const clip = project.createClip({
          trackId: track.id,
          name: `${source.name} — ${stem}`,
          sourcePath: path,
          sourceChannels: source.sourceChannels,
          sourceSampleRate: source.sourceSampleRate,
          sourceLengthSamples: source.sourceLengthSamples,
          timelineStartSamples: source.timelineStartSamples,
          offsetSamples: source.offsetSamples,
          lengthSamples: source.lengthSamples,
          gainDb: 0,
          fadeInSamples: 0,
          fadeOutSamples: 0,
        });
        // demo engine: make the synthetic waveform match the stem's role
        // and register the clip so its meters play back
        backend.hintClipCharacter?.(clip.id, stem === "other" ? "pad" : stem);
        backend.registerClip?.(clip);
      } catch (err) {
        console.error("[aura] failed to create stem track:", err);
      }
    }
  }
}

export const jobs = new JobsStore();
