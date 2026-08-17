/**
 * Rehearse-hold, refcounted across its two controls.
 *
 * The hold has two independent sources — the held key and the REHEARSE
 * button (owner ruling R5) — and one engine-side boolean. With a private
 * flag per control, releasing either one sends `set_rehearse_hold(false)`
 * while the other is still down: the engine closes the held span and the
 * take starts writing real audio again with the button still physically
 * pressed. For a control whose whole contract is "while it is down the take
 * writes silence", that is take corruption, so the sources are counted and
 * only the transitions in and out of "nothing held" reach the engine.
 *
 * The engine's own truth still comes back as `pitch://state` — this counts
 * INPUTS, it does not mirror the hold.
 */

import { backend } from "../tauri";

export type RehearseSource = "key" | "button";

const held = new Set<RehearseSource>();

/** Which sources are currently down. Exposed for tests and diagnostics. */
export function rehearseSourcesHeld(): number {
  return held.size;
}

/**
 * Press or release one source. Returns true when this call actually changed
 * the engine's hold — i.e. the first press or the last release.
 */
export function setRehearseSource(source: RehearseSource, on: boolean): boolean {
  const before = held.size > 0;
  if (on) held.add(source);
  else held.delete(source);
  const after = held.size > 0;
  if (before === after) return false;
  void backend
    .setRehearseHold(after)
    .catch((err) => console.warn("[aura] set_rehearse_hold failed:", err));
  return true;
}

/**
 * Drop every source. For window blur: a window that loses focus mid-hold
 * gets neither the keyup nor the pointerup, and the take would go on
 * recording silence until something else happened to release it.
 */
export function releaseRehearse(): boolean {
  if (held.size === 0) return false;
  held.clear();
  void backend
    .setRehearseHold(false)
    .catch((err) => console.warn("[aura] set_rehearse_hold failed:", err));
  return true;
}
