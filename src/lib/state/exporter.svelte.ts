/**
 * Song export ("bounce") orchestration: capabilities (ffmpeg-gated formats),
 * the export dialog's open state, one job at a time with progress from the
 * `export://progress|done|error` app events, and a success toast carrying
 * path / duration / size from `export://done`.
 */

import { backend, type Unsubscribe } from "../tauri";
import { project } from "./project.svelte";
import { toasts } from "./toasts.svelte";
import type {
  ExportCapabilities,
  ExportRequest,
  ExportResult,
} from "../types/ipc";

export interface ExportJobView {
  jobId: string;
  state: "running" | "done" | "error";
  progress: number;
  stage: string;
  result?: ExportResult;
  error?: string;
}

function fmtBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} kB`;
  return `${n} B`;
}

function fmtDuration(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = secs - m * 60;
  return m > 0 ? `${m}:${s.toFixed(1).padStart(4, "0")}` : `${s.toFixed(2)} s`;
}

class ExporterStore {
  /** Dialog visibility. */
  open = $state(false);
  caps = $state<ExportCapabilities | null>(null);
  capsLoading = $state(false);
  /** The in-flight (or just-finished) job; null = idle. */
  job = $state<ExportJobView | null>(null);
  /** Submit-time rejection (invalid path, missing ffmpeg…). */
  submitError = $state<string | null>(null);

  private unsubs: Unsubscribe[] = [];

  init() {
    if (this.unsubs.length) return;
    this.unsubs = [
      backend.on("export://progress", (e) => {
        if (!this.job || this.job.jobId !== e.jobId) return;
        this.job = { ...this.job, state: "running", progress: e.progress, stage: e.stage };
      }),
      backend.on("export://done", (e) => {
        if (!this.job || this.job.jobId !== e.jobId) return;
        const { jobId: _id, ...result } = e;
        this.job = { ...this.job, state: "done", progress: 1, stage: "done", result };
        toasts.success(
          "EXPORT COMPLETE",
          result.path,
          `${result.format.toUpperCase()} · ${fmtDuration(result.durationSeconds)} · ${fmtBytes(result.sizeBytes)}`,
        );
      }),
      backend.on("export://error", (e) => {
        if (!this.job || this.job.jobId !== e.jobId) return;
        this.job = { ...this.job, state: "error", stage: "error", error: e.message };
        toasts.error("EXPORT FAILED", e.message);
      }),
    ];
  }

  /** Sensible default destination inside the open project dir. */
  defaultPath(format = "wav"): string {
    const dir = project.projectDir ?? "/tmp";
    const name = (project.name || "untitled").replace(/[^\w.-]+/g, "-").toLowerCase();
    return `${dir}/export/${name}.${format}`;
  }

  async openDialog() {
    this.open = true;
    this.submitError = null;
    if (!this.caps && !this.capsLoading) {
      this.capsLoading = true;
      try {
        this.caps = await backend.exportCapabilities();
      } catch (err) {
        console.warn("[aura] export_capabilities failed:", err);
        this.caps = { formats: ["wav"], ffmpeg: false, hint: String(err) };
      } finally {
        this.capsLoading = false;
      }
    }
  }

  closeDialog() {
    this.open = false;
    // Keep a finished/errored job visible next open; clear terminal state.
    if (this.job && this.job.state !== "running") this.job = null;
    this.submitError = null;
  }

  get busy(): boolean {
    return this.job?.state === "running";
  }

  async start(request: ExportRequest): Promise<boolean> {
    this.submitError = null;
    try {
      const jobId = await backend.exportSong(request);
      this.job = { jobId, state: "running", progress: 0, stage: "queued" };
      return true;
    } catch (err) {
      this.submitError = String(err);
      return false;
    }
  }
}

export const exporter = new ExporterStore();
