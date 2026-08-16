/**
 * Pitch frame bus. Deliberately NOT reactive, for the same reason the meter
 * bus is not (`meters.svelte.ts`): frames arrive at 100 Hz in 60 Hz batches
 * and are consumed by the Pitch Coach canvas inside its own rAF loop.
 * Routing them through Svelte's reactivity graph would invalidate the UI
 * 60×/s to redraw a canvas that was going to redraw anyway.
 *
 * The frames live in a fixed ring — 30 seconds at 100 Hz — so a session left
 * listening overnight costs the same as one left listening for a minute.
 * Only `pitchMode` is `$state`: mode changes are rare, and chrome (the
 * listen toggle, the rehearse indicator) should re-render on them.
 *
 * Thin renderer (ADR 0006): nothing here derives, scores, or converts. It
 * stores what the backend pushed and hands it back in arrival order.
 */

import { backend } from "../tauri";
import type { PitchFrame, PitchFrameBatch } from "../types/ipc";

/** 30 s at the detector's 100 Hz frame rate. */
export const RING_CAPACITY = 3000;

const ring: PitchFrame[] = new Array(RING_CAPACITY);
/** Where the next frame goes; `count` frames precede it, wrapping. */
let cursor = 0;
let count = 0;
let lastVoiced: PitchFrame | null = null;

let unsubscribeChannel: (() => void) | null = null;
let unsubscribeState: (() => void) | null = null;

/**
 * Live mode flags. `listening`, `rehearseHold` and `deviceRate` ride on
 * every batch so the UI sees a hold start without waiting for the event;
 * `referenceTrackId` only ever arrives on `pitch://state`.
 */
export const pitchMode = $state({
  listening: false,
  rehearseHold: false,
  referenceTrackId: null as string | null,
  deviceRate: 0,
});

/**
 * The frames still in the ring, oldest first. Allocates a copy — call it
 * once per rAF frame, not once per drawn point.
 */
export function recentFrames(): readonly PitchFrame[] {
  const out: PitchFrame[] = new Array(count);
  const start = (cursor - count + RING_CAPACITY) % RING_CAPACITY;
  for (let i = 0; i < count; i++) out[i] = ring[(start + i) % RING_CAPACITY];
  return out;
}

/**
 * The most recent voiced frame, or null if nothing has been voiced since
 * the last reset. Survives a breath: the tuner keeps showing the last real
 * note instead of blanking between phrases.
 */
export function latestVoiced(): PitchFrame | null {
  return lastVoiced;
}

/** Frames whose `sample` falls in `[startSample, endSample)`. */
export function framesBetween(startSample: number, endSample: number): PitchFrame[] {
  const out: PitchFrame[] = [];
  const start = (cursor - count + RING_CAPACITY) % RING_CAPACITY;
  for (let i = 0; i < count; i++) {
    const frame = ring[(start + i) % RING_CAPACITY];
    if (frame.sample >= startSample && frame.sample < endSample) out.push(frame);
  }
  return out;
}

/** Feed one batch into the ring. Exported so the bus is testable bare. */
export function ingestBatch(batch: PitchFrameBatch): void {
  pitchMode.listening = batch.listening;
  pitchMode.rehearseHold = batch.rehearseHold;
  pitchMode.deviceRate = batch.deviceRate;

  // A batch bigger than the ring would otherwise wrap onto itself; keep its
  // tail, which is the only part that would have survived anyway.
  const frames =
    batch.frames.length > RING_CAPACITY ? batch.frames.slice(batch.frames.length - RING_CAPACITY) : batch.frames;

  for (const frame of frames) {
    ring[cursor] = frame;
    cursor = (cursor + 1) % RING_CAPACITY;
    if (count < RING_CAPACITY) count++;
    if (frame.voiced) lastVoiced = frame;
  }
}

/** Drop every frame and reset the flags. */
export function resetPitchBus(): void {
  ring.length = 0;
  ring.length = RING_CAPACITY;
  cursor = 0;
  count = 0;
  lastVoiced = null;
  pitchMode.listening = false;
  pitchMode.rehearseHold = false;
  pitchMode.referenceTrackId = null;
  pitchMode.deviceRate = 0;
}

/**
 * Subscribe to the batch channel and to `pitch://state`. Idempotent — the
 * panel calls it on mount and the listen toggle may call it again.
 *
 * This does NOT open the microphone: `pitch_listen_start` does, and the
 * mic opens only on an explicit listen toggle or an open panel (R6).
 */
export async function startPitchStream(): Promise<void> {
  if (unsubscribeChannel) return;
  try {
    unsubscribeChannel = await backend.subscribePitch(ingestBatch);
  } catch (err) {
    console.warn("[aura] pitch_subscribe failed:", err);
    return;
  }
  unsubscribeState = backend.on("pitch://state", (state) => {
    pitchMode.listening = state.listening;
    pitchMode.rehearseHold = state.rehearseHold;
    pitchMode.referenceTrackId = state.referenceTrackId;
    pitchMode.deviceRate = state.deviceRate;
  });
}

/** Stop consuming batches and clear the ring. */
export function stopPitchStream(): void {
  unsubscribeChannel?.();
  unsubscribeChannel = null;
  unsubscribeState?.();
  unsubscribeState = null;
  resetPitchBus();
}
