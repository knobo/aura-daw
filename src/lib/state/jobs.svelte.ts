/**
 * Sidecar job tracking + the "AI Split Stems" flow: submit, stream progress
 * over the Channel. Stem tracks/clips are never built in the webview — the
 * backend's `split_stems_for_clip` resolves the clip, runs Demucs, and
 * auto-imports each stem as a real track+clip, announcing over
 * `project://changed`; this store just mirrors job progress and re-pulls the
 * project when the backend says a stem landed (Task 11).
 */

import { backend } from "../tauri";
import { IMPORT_AUDIO_EXTS, type Clip, type SidecarEvent } from "../types/ipc";
import { project } from "./project.svelte";
import { toasts } from "./toasts.svelte";

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

  /**
   * SPLIT STEMS on a clip already on the timeline: one invoke to the real
   * backend command (`split_stems_for_clip`), which resolves the clip,
   * submits the Demucs job on its EXISTING project-copy audio (no re-import,
   * no duplicate clip), and auto-imports the four stems as new tracks
   * aligned to the clip's current position. Progress/completion mirror the
   * drag-and-drop import+split flow's events (`onImportSplitEvent`) exactly
   * — same envelope, same "stems land as follow-up log lines" protocol.
   */
  async splitStems(clip: Clip): Promise<void> {
    if (this.jobForClip(clip.id)) return; // one job per clip at a time

    let jobId = "";
    const pending: SidecarEvent[] = [];
    const handle = (e: SidecarEvent) => {
      if (!jobId) {
        pending.push(e);
        return;
      }
      this.onImportSplitEvent(clip.name, e);
    };

    try {
      jobId = await backend.splitStemsForClip(clip.id, handle);
    } catch (err) {
      console.error("[aura] split_stems_for_clip failed:", err);
      toasts.error("STEM SPLIT FAILED", clip.name, String(err));
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
    for (const e of pending) this.onImportSplitEvent(clip.name, e);
  }

  async cancel(jobId: string) {
    await backend.sidecarCancelJob(jobId);
    this.patch(jobId, { state: "cancelled" });
  }

  // ── drag-and-drop import (wave 1.5) ──

  /** Does this file look importable (wav/mp3/flac/ogg/aac/m4a)? */
  static importable(path: string): boolean {
    const ext = path.split(".").pop()?.toLowerCase() ?? "";
    return (IMPORT_AUDIO_EXTS as readonly string[]).includes(ext);
  }

  /**
   * Import a dropped file; with `split` the backend chains a Demucs stem
   * split on the imported copy and auto-lands the stems as new tracks.
   */
  async importDropped(path: string, split: boolean): Promise<void> {
    const name = path.split("/").pop() ?? path;
    if (!JobsStore.importable(path)) {
      toasts.error("UNSUPPORTED FILE", name, "supported: wav · mp3 · flac · ogg · aac · m4a");
      return;
    }
    if (!split) {
      try {
        const clip = await backend.importAudioClip({ path });
        await project.reload();
        project.select(clip.id);
        toasts.success("IMPORTED", `${name} → ${project.trackById(clip.trackId)?.name ?? "new track"}`);
      } catch (err) {
        toasts.error("IMPORT FAILED", name, String(err));
      }
      return;
    }

    // import + split: the reply arrives after the import lands; the job id
    // keys the Demucs progress that streams over the channel.
    let jobId = "";
    const early: SidecarEvent[] = [];
    const handle = (e: SidecarEvent) => {
      if (!jobId) {
        early.push(e);
        return;
      }
      this.onImportSplitEvent(name, e);
    };
    try {
      const reply = await backend.importAudioClipSplitStems({ path }, handle);
      await project.reload();
      project.select(reply.clip.id);
      jobId = reply.jobId;
      this.jobs = {
        ...this.jobs,
        [jobId]: {
          jobId,
          kind: "stemSplit",
          clipId: reply.clip.id,
          trackId: reply.clip.trackId,
          progress: 0,
          stage: "queued",
          state: "queued",
        },
      };
      toasts.info("IMPORT + STEMS", `${name} imported — Demucs separating…`);
      for (const e of early) this.onImportSplitEvent(name, e);
    } catch (err) {
      toasts.error("IMPORT FAILED", name, String(err));
    }
  }

  /** Events of the chained Demucs job. The BACKEND lands the stems AFTER the
   * `done` event, announcing each as a follow-up `log` line ("auto-import
   * stem `drums`: …") — so the project re-pull rides those lines, never a
   * frontend materialization. */
  private stemToastShown = new Set<string>();

  private onImportSplitEvent(name: string, e: SidecarEvent) {
    switch (e.type) {
      case "progress":
        this.patch(e.jobId, { progress: e.progress, stage: e.stage, state: "running" });
        break;
      case "done":
        this.patch(e.jobId, { state: "done", progress: 1, stage: "importing stems" });
        break;
      case "error": {
        const wasCancelled = this.jobs[e.jobId]?.state === "cancelled";
        this.patch(e.jobId, {
          state: wasCancelled ? "cancelled" : "error",
          error: e.message,
        });
        if (!wasCancelled) toasts.error("STEM SPLIT FAILED", name, e.message);
        break;
      }
      case "log":
        if (/^auto-import stem `[^`]+` failed/.test(e.line)) {
          toasts.error("STEM IMPORT FAILED", e.line);
        } else if (e.line.startsWith("auto-import stem")) {
          // one new stem track landed control-side — mirror it
          void project.reload();
          if (!this.stemToastShown.has(e.jobId)) {
            this.stemToastShown.add(e.jobId);
            this.patch(e.jobId, { stage: "done" });
            toasts.success("STEMS LANDING", `${name} → drums · bass · vocals · other`);
          }
        }
        break;
    }
  }
}

export const jobs = new JobsStore();
