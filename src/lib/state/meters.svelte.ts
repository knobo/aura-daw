/**
 * Meter bus. Deliberately NOT reactive: meter frames arrive at 60 Hz and are
 * consumed by canvas renderers inside their own rAF loops — routing them
 * through Svelte's reactivity graph would invalidate the whole UI 60×/s.
 * Components read `latestMeter(trackId)` imperatively each frame.
 */

import { backend } from "../tauri";
import type { MeterFrame, TrackMeter } from "../types/ipc";
import { paramFollow } from "./param-follow.svelte";
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
      // The one reactive hand-off out of this callback besides the playhead:
      // `paramFollow.apply` is a no-op unless the driven set actually
      // changed, so a project with no plugin automation pays a length check
      // per frame and invalidates nothing.
      paramFollow.apply(frame.drivenParams);
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
  // No stream, no read-back: leaving the last frame's set behind would pin
  // the param panel to values nothing is driving any more.
  paramFollow.reset();
}
