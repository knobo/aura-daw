/**
 * Loop-jam ("EVOLVE"): ACE-Step repaint of the transport loop region, applied
 * at the next loop wrap. Mirrors the backend's LoopJam state machine through
 * `loopjam://state` (idle | generating | ready | applied + lastError) and the
 * three commands loopjam_evolve / loopjam_cancel / loopjam_status.
 */

import { backend, type Unsubscribe } from "../tauri";
import { toasts } from "./toasts.svelte";
import { project } from "./project.svelte";
import type { LoopJamStatus } from "../types/ipc";

class LoopJamStore {
  status = $state<LoopJamStatus>({
    state: "idle",
    jobId: null,
    trackId: null,
    loopStartSamples: 0,
    loopEndSamples: 0,
    generation: 0,
    lastError: null,
  });
  /** Submit-time rejection (no region, nothing to evolve…). */
  error = $state<string | null>(null);
  /** Bumped on every applied hot-swap — drives the loop band flash. */
  appliedFlash = $state(0);

  private unsub: Unsubscribe | null = null;

  get busy(): boolean {
    return this.status.state === "generating" || this.status.state === "ready";
  }

  async init() {
    this.unsub?.();
    this.unsub = backend.on("loopjam://state", (s) => this.accept(s));
    try {
      this.accept(await backend.loopjamStatus());
    } catch {
      // pre-wave-1.5 engine — stay idle
    }
  }

  private accept(s: LoopJamStatus) {
    const wasApplied = s.state === "applied" && s.generation > this.status.generation;
    this.status = s;
    if (wasApplied) {
      this.appliedFlash++;
      toasts.success("LOOP EVOLVED", `generation ${s.generation} swapped in at the wrap`);
      // The repaint replaced clip audio in place — re-pull so waveforms redraw.
      void project.reload();
    }
    if (s.lastError && s.state === "idle") this.error = s.lastError;
  }

  async evolve(prompt: string, seed: string) {
    this.error = null;
    const seedNum = parseInt(seed, 10);
    try {
      this.accept(
        await backend.loopjamEvolve({
          prompt: prompt.trim() ? prompt.trim() : null,
          seed: Number.isFinite(seedNum) ? seedNum : null,
        }),
      );
    } catch (err) {
      this.error = String(err);
    }
  }

  async cancel() {
    try {
      this.accept(await backend.loopjamCancel());
    } catch (err) {
      this.error = String(err);
    }
  }
}

export const loopjam = new LoopJamStore();
