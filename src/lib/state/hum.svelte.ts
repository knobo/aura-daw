/**
 * Hum-to-song orchestration: submit `hum_to_song` over its Channel, follow
 * the analysis stages (loadingAudio → trackingPitch → segmenting →
 * quantizing), and pick up the post-job apply announced as a follow-up `log`
 * event ("hum-to-song: melody clip …") — then refresh the stores and
 * highlight the landed clip. The optional AMT accompaniment job streams over
 * the SAME channel with its own job id.
 */

import { backend } from "../tauri";
import { midi } from "./midi.svelte";
import { project } from "./project.svelte";
import { toasts } from "./toasts.svelte";
import type { HumToSongRequest, SidecarEvent } from "../types/ipc";

export type HumPhase = "idle" | "running" | "applying" | "done" | "error";

const MELODY_RE = /^hum-to-song: melody clip (\S+) \((\d+) notes\) on track (\S+?)(?: \(auto-created "(.+?)"\))?(?:;|$)/;
const ACCOMP_RE = /^hum-to-song: accompaniment clip (\S+) \((\d+) notes\) on track (\S+)/;

class HumStore {
  phase = $state<HumPhase>("idle");
  progress = $state(0);
  stage = $state("");
  error = $state<string | null>(null);
  /** Landed melody clip (post-apply). */
  melodyClipId = $state<string | null>(null);
  melodyTrackId = $state<string | null>(null);
  melodyNoteCount = $state(0);
  createdTrackName = $state<string | null>(null);
  /** Accompaniment chain state ("" = not requested). */
  accompStage = $state<"" | "pending" | "done" | "failed">("");
  accompClipId = $state<string | null>(null);
  log = $state<string[]>([]);

  private jobId = "";
  private accompany = false;

  get busy(): boolean {
    return this.phase === "running" || this.phase === "applying";
  }

  reset() {
    if (this.busy) return;
    this.phase = "idle";
    this.progress = 0;
    this.stage = "";
    this.error = null;
    this.melodyClipId = null;
    this.melodyTrackId = null;
    this.melodyNoteCount = 0;
    this.createdTrackName = null;
    this.accompStage = "";
    this.accompClipId = null;
    this.log = [];
  }

  async run(request: HumToSongRequest): Promise<boolean> {
    if (this.busy) return false;
    this.reset();
    this.phase = "running";
    this.stage = "submitting";
    this.accompany = !!request.accompany;
    this.accompStage = this.accompany ? "pending" : "";
    try {
      this.jobId = await backend.humToSong(request, (e) => this.onEvent(e));
      return true;
    } catch (err) {
      this.phase = "error";
      this.error = String(err);
      return false;
    }
  }

  private appendLog(line: string) {
    this.log = [...this.log, line].slice(-40);
  }

  private onEvent(e: SidecarEvent) {
    switch (e.type) {
      case "progress":
        // Melody analysis drives the bar; accompaniment progress only labels.
        if (!this.jobId || e.jobId === this.jobId) {
          this.progress = e.progress;
          this.stage = e.stage;
        } else if (this.accompStage === "pending") {
          this.stage = `accompaniment · ${e.stage}`;
        }
        break;
      case "done":
        if (e.jobId === this.jobId) {
          this.progress = 1;
          this.phase = "applying";
          this.stage = "placing melody clip";
        }
        break;
      case "error":
        if (e.jobId === this.jobId) {
          this.phase = "error";
          this.error = e.message;
        } else if (this.accompStage === "pending") {
          this.accompStage = "failed";
          this.appendLog(`accompaniment failed: ${e.message}`);
        }
        break;
      case "log": {
        this.appendLog(e.line);
        const melody = MELODY_RE.exec(e.line);
        if (melody) {
          void this.onMelodyApplied(melody[1], Number(melody[2]), melody[3], melody[4] ?? null);
          break;
        }
        const accomp = ACCOMP_RE.exec(e.line);
        if (accomp) {
          this.accompStage = "done";
          this.accompClipId = accomp[1];
          void this.refresh(accomp[1]);
          break;
        }
        if (e.line.includes("apply failed") && e.jobId === this.jobId) {
          this.phase = "error";
          this.error = e.line;
        } else if (e.line.includes("accompaniment submit failed")) {
          this.accompStage = "failed";
        }
        break;
      }
    }
  }

  private async onMelodyApplied(
    clipId: string,
    noteCount: number,
    trackId: string,
    createdName: string | null,
  ) {
    this.melodyClipId = clipId;
    this.melodyTrackId = trackId;
    this.melodyNoteCount = noteCount;
    this.createdTrackName = createdName;
    this.phase = "done";
    this.stage = "melody landed";
    toasts.success(
      "MELODY LANDED",
      `${noteCount} notes${createdName ? ` on new track "${createdName}"` : ""} — bind an instrument to shape the sound`,
    );
    await this.refresh(clipId);
  }

  /** Re-pull project + midi state, then highlight/select the new clip. */
  private async refresh(flashClipId: string) {
    await Promise.all([project.reload(), midi.refresh()]);
    midi.select(flashClipId);
    midi.flash(flashClipId);
  }
}

export const hum = new HumStore();
