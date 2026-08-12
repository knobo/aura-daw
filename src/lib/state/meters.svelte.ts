/**
 * Meter bus. Deliberately NOT reactive: meter frames arrive at 60 Hz and are
 * consumed by canvas renderers inside their own rAF loops — routing them
 * through Svelte's reactivity graph would invalidate the whole UI 60×/s.
 * Components read `latestMeter(trackId)` imperatively each frame.
 */

import { backend } from "../tauri";
import type { MeterFrame, TrackMeter } from "../types/ipc";
import { transport } from "./transport.svelte";

let latest: MeterFrame | null = null;
const byTrack = new Map<string, TrackMeter>();
let unsubscribe: (() => void) | null = null;

export function latestFrame(): MeterFrame | null {
  return latest;
}

/** Latest meter for a track id, or "master". */
export function latestMeter(trackId: string): TrackMeter | null {
  return byTrack.get(trackId) ?? null;
}

export async function startMeterStream(): Promise<void> {
  if (unsubscribe) return;
  try {
    unsubscribe = await backend.subscribeMeters((frame) => {
      latest = frame;
      byTrack.clear();
      for (const t of frame.tracks) byTrack.set(t.trackId, t);
      byTrack.set("master", frame.master);
      transport.syncFromMeters(frame.positionSamples);
    });
  } catch (err) {
    console.warn("[aura] subscribe_meters failed:", err);
  }
}

export function stopMeterStream(): void {
  unsubscribe?.();
  unsubscribe = null;
  latest = null;
  byTrack.clear();
}
