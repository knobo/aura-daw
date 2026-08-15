/**
 * Transport state with time-based playhead interpolation.
 * The engine pushes discrete TransportState snapshots (events + command
 * responses); between snapshots the playhead is extrapolated from the wall
 * clock so rAF consumers can render at any frame rate (120 FPS included).
 */

import { backend } from "../tauri";
import type { TransportState } from "../types/ipc";

class TransportStore {
  snap = $state<TransportState>({
    state: "stopped",
    positionSamples: 0,
    sampleRate: 48000,
    tempoBpm: 120,
    loopEnabled: false,
    loopStartSamples: 0,
    loopEndSamples: 0,
    songEndSamples: 0,
    stopAtEnd: true,
    countInLeftSamples: 0,
  });

  /** wall time (performance.now()) at which `snap` was captured */
  private anchorMs = performance.now();
  private unlisten: (() => void) | null = null;

  get isPlaying() {
    return this.snap.state !== "stopped";
  }
  get isRecording() {
    return this.snap.state === "recording";
  }

  /**
   * Interpolated playhead position in samples at wall time `nowMs`.
   * Wraps inside an active loop region (same rule as the engine's
   * `transport::advance`), so the playhead loops smoothly between
   * transport snapshots.
   */
  positionAt(nowMs: number): number {
    if (this.snap.state === "stopped") return this.snap.positionSamples;
    const raw =
      this.snap.positionSamples +
      ((nowMs - this.anchorMs) / 1000) * this.snap.sampleRate;
    const { loopEnabled, loopStartSamples: s, loopEndSamples: e } = this.snap;
    if (loopEnabled && e > s && this.snap.positionSamples < e && raw >= e) {
      return s + ((raw - s) % (e - s));
    }
    return raw;
  }

  private accept(state: TransportState) {
    this.snap = state;
    this.anchorMs = performance.now();
  }

  /**
   * Stop the transport when the playhead reaches the end of the material.
   * The engine detects the boundary regardless; this is only the policy.
   */
  async setStopAtEnd(enabled: boolean) {
    this.snap = { ...this.snap, stopAtEnd: enabled }; // optimistic
    try {
      this.accept(await backend.transportSetStopAtEnd(enabled));
    } catch (err) {
      console.warn("[aura] transport_set_stop_at_end failed:", err);
      await this.init();
    }
  }

  /**
   * Gentle re-anchor from 60 Hz meter frames: only correct when prediction
   * drifts noticeably, so the playhead stays visually smooth.
   */
  syncFromMeters(positionSamples: number) {
    if (this.snap.state === "stopped") return;
    const now = performance.now();
    const predicted = this.positionAt(now);
    const driftSec = Math.abs(predicted - positionSamples) / this.snap.sampleRate;
    if (driftSec > 0.06) {
      this.snap = { ...this.snap, positionSamples };
      this.anchorMs = now;
    }
  }

  async init() {
    try {
      this.accept(await backend.getTransportState());
    } catch (err) {
      console.warn("[aura] get_transport_state failed:", err);
    }
    this.unlisten?.();
    this.unlisten = backend.on("transport://state", (state) => this.accept(state));
  }

  async play() {
    this.accept(await backend.transportPlay());
  }

  /**
   * Pause: halt playback, keep the playhead. Both engines implement
   * transport_stop as stop-without-seek, which is exactly pause semantics.
   */
  async pause() {
    this.accept(await backend.transportStop());
  }

  /** Stop: halt playback AND return the playhead to the start. */
  async stop() {
    this.accept(await backend.transportStop());
    await this.seek(0);
  }

  async seek(positionSamples: number) {
    const target = Math.max(0, Math.round(positionSamples));
    // optimistic: move immediately, engine confirms
    this.snap = { ...this.snap, positionSamples: target };
    this.anchorMs = performance.now();
    this.accept(await backend.transportSeek(target));
  }

  async togglePlay() {
    if (this.snap.state === "stopped") await this.play();
    else await this.pause();
  }

  /** Set the loop region (samples) and its enabled flag; optimistic. */
  async setLoop(enabled: boolean, startSamples: number, endSamples: number) {
    const s = Math.max(0, Math.round(startSamples));
    const e = Math.max(0, Math.round(endSamples));
    this.snap = {
      ...this.snap,
      loopEnabled: enabled,
      loopStartSamples: s,
      loopEndSamples: e,
    };
    try {
      this.accept(await backend.transportSetLoop(enabled, s, e));
    } catch (err) {
      console.warn("[aura] transport_set_loop failed:", err);
      await this.init(); // re-sync with the engine's actual state
    }
  }

  /**
   * Toggle looping. Enabling with no region yet defined creates a default
   * region (`fallbackStart`/`fallbackEnd`, provided by the caller from the
   * musical grid) so the toggle is never a silent no-op.
   */
  async toggleLoop(fallbackStart = 0, fallbackEnd = 0) {
    const { loopEnabled, loopStartSamples: s, loopEndSamples: e } = this.snap;
    if (loopEnabled) {
      await this.setLoop(false, s, e);
    } else if (e > s) {
      await this.setLoop(true, s, e);
    } else if (fallbackEnd > fallbackStart) {
      await this.setLoop(true, fallbackStart, fallbackEnd);
    }
  }
}

export const transport = new TransportStore();
