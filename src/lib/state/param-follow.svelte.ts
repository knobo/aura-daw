/**
 * What automation is doing to plugin parameters right now.
 *
 * Track D ruling 2 sends plugin-param automation to the HOST ONLY — the
 * document keeps whatever the user set — so the param panel had nothing to
 * paint but the stored value while the parameter itself moved. The engine
 * publishes its driver's own writes on the meter frame
 * (`MeterFrame.drivenParams`); this store is where they land, and the only
 * place the UI reads them.
 *
 * Not authoritative and not persisted (ADR 0006 still holds): no curve is
 * evaluated here, no tick/sample math happens here, nothing is written back.
 * The engine says what it wrote; this repeats it.
 *
 * Reactivity budget: `apply` runs 60x/s for the app's whole lifetime, so it
 * reassigns its state ONLY when the set actually changed — and the common
 * case (nothing automated, or the transport stopped) leaves both sides empty
 * and costs one length check.
 */

import type { DrivenParam, PluginParamInfo } from "../types/ipc";

/** Map key for one (instance, host param index) pair. A space cannot occur
 * in a param index, so no instance id can collide with another's. */
function key(instanceId: string, paramId: number): string {
  return `${instanceId} ${paramId}`;
}

class ParamFollowStore {
  /** Keyed by `key()` -> the value the engine last wrote to the host.
   * Reassigned, never mutated — the reactivity convention the selection Sets
   * in this directory use. */
  driven = $state<Map<string, number>>(new Map());

  /** True while automation is holding at least one plugin param. */
  get active(): boolean {
    return this.driven.size > 0;
  }

  /** The value automation is holding this param at, or undefined when it is
   * the user's (i.e. the document's) again. */
  valueFor(instanceId: string, paramId: number): number | undefined {
    return this.driven.get(key(instanceId, paramId));
  }

  /** Adopt one meter frame's read-back. `undefined` (a backend without the
   * field) reads as "nothing is driven", same as an empty list. */
  apply(frame: DrivenParam[] | undefined): void {
    const next = frame ?? [];
    if (next.length === 0) {
      if (this.driven.size > 0) this.driven = new Map();
      return;
    }
    if (this.#matches(next)) return;
    const map = new Map<string, number>();
    for (const d of next) map.set(key(d.instanceId, d.index), d.value);
    this.driven = map;
  }

  /** The param as it should be PAINTED: the same object when nothing drives
   * it, a copy carrying the driven value when something does. Identity
   * (`id`, `min`, `max`, `steps`, `default`, `name`) is untouched, so a
   * caller can still write to the real param and reset to the real default.
   *
   * Every surface that paints a plugin param value goes through here — the
   * param panel, the lane strip's pinned chips and the automation matrix —
   * so two of them cannot end up disagreeing about the same parameter. */
  overlay(instanceId: string, info: PluginParamInfo): PluginParamInfo {
    const v = this.valueFor(instanceId, info.id);
    return v === undefined ? info : { ...info, value: v };
  }

  /** Drop everything — the meter stream is gone, so nothing is following. */
  reset(): void {
    if (this.driven.size > 0) this.driven = new Map();
  }

  /** Exact-equality check against the held set. Values arrive as f32 the
   * engine already de-duplicated (`ParamAutomationDriver::EPSILON`), so an
   * unchanged hold really does repeat the same bits and a moving ramp really
   * does differ — no tolerance needed, and none wanted: a tolerance here
   * would swallow small steps the engine deliberately sent. */
  #matches(next: DrivenParam[]): boolean {
    if (next.length !== this.driven.size) return false;
    for (const d of next) {
      if (this.driven.get(key(d.instanceId, d.index)) !== d.value) return false;
    }
    return true;
  }
}

export const paramFollow = new ParamFollowStore();
