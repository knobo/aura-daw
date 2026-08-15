/**
 * Modulation document mirror. Thin by construction (ADR 0006): the backend
 * owns curves/bindings/clips, this store holds a copy for painting and
 * emits whole-curve replaces through `modulation_set_curve`. Preview is
 * local (no invoke); commit is one invoke per gesture (D-03), same shape
 * as `automation.svelte.ts`.
 */

import { backend } from "../tauri";
import type {
  AutomationClip,
  AutomationPoint,
  Binding,
  BindingMode,
  Curve,
  ModulationSnapshot,
  TargetRef,
} from "../types/ipc";

function emptyBinding(partial: Partial<Binding> & Pick<Binding, "source" | "target">): Binding {
  return {
    id: "",
    mode: "absolute",
    depth: 1,
    ...partial,
  };
}

export function targetsEqual(a: TargetRef, b: TargetRef): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case "trackParam":
      return b.kind === "trackParam" && a.trackId === b.trackId && a.param === b.param;
    case "pluginParam":
      return b.kind === "pluginParam" && a.instanceId === b.instanceId && a.paramId === b.paramId;
    case "selfTrackParam":
      return b.kind === "selfTrackParam" && a.param === b.param;
    case "selfInstrumentParam":
      return b.kind === "selfInstrumentParam" && a.paramId === b.paramId;
    case "macro":
      return b.kind === "macro" && a.macroId === b.macroId;
    case "port":
      return b.kind === "port" && a.nodeId === b.nodeId && a.portId === b.portId;
  }
}

/** Curve 0..1 → native pan (−1 hard left .. +1 hard right). */
export function panNativeOf(normalized: number): number {
  return normalized * 2 - 1;
}

/** Native pan (−1..+1) → curve 0..1. */
export function panNormalizedOf(native: number): number {
  return (native + 1) / 2;
}

function curveIdOf(b: Binding): string | null {
  return b.source.kind === "curve" ? b.source.curveId : null;
}

function defaultMode(target: TargetRef): BindingMode {
  return target.kind === "trackParam" && target.param === "gain" ? "multiply" : "absolute";
}

function defaultName(target: TargetRef): string {
  switch (target.kind) {
    case "trackParam":
    case "selfTrackParam":
      return target.param;
    case "pluginParam":
      return `param ${target.paramId}`;
    case "selfInstrumentParam":
      return `param ${target.paramId}`;
    case "macro":
      return "macro";
    case "port":
      return target.portId;
  }
}

class ModulationStore {
  curves = $state<Curve[]>([]);
  bindings = $state<Binding[]>([]);
  automationClips = $state<AutomationClip[]>([]);
  /** Track id → binding ids whose overlays are shown. Reassigned, not
   * mutated (same convention as PianoRoll's selection Set). Insertion
   * order is paint order: last id is the top, editable overlay. */
  visible = $state<Map<string, Set<string>>>(new Map());

  bindingsFor(target: TargetRef): Binding[] {
    return this.bindings.filter((b) => targetsEqual(b.target, target));
  }

  /** Bindings whose source is this automation track. */
  bindingsFrom(trackId: string): Binding[] {
    return this.bindings.filter(
      (b) => b.source.kind === "automationTrack" && b.source.trackId === trackId,
    );
  }

  clipsOn(trackId: string): AutomationClip[] {
    return this.automationClips.filter((c) => c.trackId === trackId);
  }

  curveOf(binding: Binding): Curve | undefined {
    const id = curveIdOf(binding);
    return id ? this.curves.find((c) => c.id === id) : undefined;
  }

  visibleBindingsFor(trackId: string): Binding[] {
    const ids = this.visible.get(trackId);
    if (!ids) return [];
    const out: Binding[] = [];
    for (const id of ids) {
      const b = this.bindings.find((x) => x.id === id);
      if (b) out.push(b);
    }
    return out;
  }

  hasVisible(trackId: string): boolean {
    return (this.visible.get(trackId)?.size ?? 0) > 0;
  }

  isBindingVisible(trackId: string, bindingId: string): boolean {
    return this.visible.get(trackId)?.has(bindingId) ?? false;
  }

  show(trackId: string, bindingId: string) {
    const next = new Map(this.visible);
    const set = new Set(next.get(trackId));
    set.delete(bindingId);
    set.add(bindingId);
    next.set(trackId, set);
    this.visible = next;
  }

  hide(trackId: string, bindingId: string) {
    const next = new Map(this.visible);
    const set = new Set(next.get(trackId));
    set.delete(bindingId);
    if (set.size === 0) next.delete(trackId);
    else next.set(trackId, set);
    this.visible = next;
  }

  hideAll(trackId: string) {
    if (!this.visible.has(trackId)) return;
    const next = new Map(this.visible);
    next.delete(trackId);
    this.visible = next;
  }

  /**
   * Header/picker pick: mint a curve+binding when the target has none,
   * then show that overlay (bringing it to the front if already visible).
   */
  async pickTarget(
    trackId: string,
    target: TargetRef,
    initialNormalized?: number,
  ): Promise<Binding | undefined> {
    const existing = this.bindingsFor(target)[0];
    if (existing) {
      const ids = [...(this.visible.get(trackId) ?? [])];
      const top = ids[ids.length - 1];
      if (this.isBindingVisible(trackId, existing.id) && top === existing.id) {
        this.hide(trackId, existing.id);
      } else {
        this.show(trackId, existing.id);
      }
      return existing;
    }
    const value =
      initialNormalized ??
      (target.kind === "trackParam" && target.param === "gain" ? 1 : 0.5);
    const before = new Set(this.curves.map((c) => c.id));
    await this.commit({
      id: "",
      name: defaultName(target),
      points: [{ tick: 0, value: Math.min(1, Math.max(0, value)) }],
    });
    const mintedCurve = this.curves.find((c) => !before.has(c.id));
    if (!mintedCurve) return undefined;
    const beforeB = new Set(this.bindings.map((b) => b.id));
    await this.setBinding({
      id: "",
      source: { kind: "curve", curveId: mintedCurve.id },
      target,
      mode: defaultMode(target),
      depth: 1,
    });
    const minted = this.bindings.find((b) => !beforeB.has(b.id));
    if (minted) this.show(trackId, minted.id);
    return minted;
  }

  /**
   * Bind an automation track to a target. Unlike `pickTarget` this does
   * not mint a timeline curve — the track's clips are the source.
   */
  async addTarget(autoTrackId: string, target: TargetRef): Promise<Binding | undefined> {
    const existing = this.bindingsFrom(autoTrackId).find((b) => targetsEqual(b.target, target));
    if (existing) return existing;
    const before = new Set(this.bindings.map((b) => b.id));
    await this.setBinding(
      emptyBinding({
        source: { kind: "automationTrack", trackId: autoTrackId },
        target,
        mode: defaultMode(target),
        depth: 1,
      }),
    );
    return this.bindings.find((b) => !before.has(b.id));
  }

  async setDepth(binding: Binding, depth: number): Promise<void> {
    const clamped = Math.min(1, Math.max(-1, depth));
    await this.setBinding({ ...binding, depth: clamped });
  }

  async removeBinding(bindingId: string): Promise<void> {
    const binding = this.bindings.find((b) => b.id === bindingId);
    if (!binding || !backend.modulationSetBinding) return;
    try {
      this.adopt(await backend.modulationSetBinding(binding, true));
    } catch (err) {
      console.error("[aura] modulation_set_binding(delete) failed:", err);
    }
  }

  async setClip(clip: AutomationClip): Promise<void> {
    if (!backend.automationClipSet) return;
    try {
      this.adopt(await backend.automationClipSet(clip));
    } catch (err) {
      console.error("[aura] automation_clip_set failed:", err);
    }
  }

  async removeClip(clipId: string): Promise<void> {
    const clip = this.automationClips.find((c) => c.id === clipId);
    if (!clip || !backend.automationClipSet) return;
    try {
      this.adopt(await backend.automationClipSet(clip, true));
    } catch (err) {
      console.error("[aura] automation_clip_set(delete) failed:", err);
    }
  }

  /** Mint a default curve and place it as an automation clip. */
  async addClip(
    trackId: string,
    timelineStartTicks: number,
    lengthTicks: number,
  ): Promise<AutomationClip | undefined> {
    const beforeCurves = new Set(this.curves.map((c) => c.id));
    await this.commit({
      id: "",
      name: "automation",
      lengthTicks,
      points: [
        { tick: 0, value: 1 },
        { tick: Math.max(0, Math.round(lengthTicks) - 1), value: 1 },
      ],
    });
    const mintedCurve = this.curves.find((c) => !beforeCurves.has(c.id));
    if (!mintedCurve) return undefined;
    const beforeClips = new Set(this.automationClips.map((c) => c.id));
    await this.setClip({
      id: "",
      trackId,
      curveId: mintedCurve.id,
      timelineStartTicks,
      lengthTicks,
    });
    return this.automationClips.find((c) => !beforeClips.has(c.id));
  }

  async reload(): Promise<void> {
    if (!backend.modulationGet) return;
    try {
      this.adopt(await backend.modulationGet());
    } catch (err) {
      console.warn("[aura] modulation_get failed:", err);
    }
  }

  /** Drag-time local patch — no invoke (mirrors `automation.preview`). */
  preview(curveId: string, points: AutomationPoint[]) {
    this.curves = this.curves.map((c) => (c.id === curveId ? { ...c, points } : c));
  }

  /** Persist a curve's CURRENT point set. The returned snapshot is
   * authoritative (the backend normalizes). */
  async commit(curve: Curve): Promise<void> {
    if (!backend.modulationSetCurve) return;
    try {
      this.adopt(await backend.modulationSetCurve(curve));
    } catch (err) {
      console.error("[aura] modulation_set_curve failed:", err);
    }
  }

  async setBinding(b: Binding): Promise<void> {
    if (!backend.modulationSetBinding) return;
    try {
      this.adopt(await backend.modulationSetBinding(b));
    } catch (err) {
      console.error("[aura] modulation_set_binding failed:", err);
    }
  }

  /**
   * Per-binding inflight barrier. Keyed by bindingId so two overlays on
   * one track cannot steal each other's closing commit (the KNOWN EDGE
   * in `automation.svelte.ts`: a store-wide / track-keyed barrier).
   */
  #inflight = new Map<string, Promise<unknown>>();

  /** Commit from INSIDE an open gesture (a click-insert, an alt-click
   * delete). Returns the barrier the gesture's closing commit — and its
   * `gesture_end` — must wait for. */
  commitInGesture(bindingId: string, curve: Curve): Promise<void> {
    const done = this.commit(curve);
    this.#inflight.set(bindingId, done);
    return done;
  }

  /** The CLOSING commit of a gesture: the curve as the store now holds it.
   *
   * It must wait for this binding's `commitInGesture` barrier before
   * READING. The store is only written when `modulation_set_curve`
   * resolves, so a pointerup landing inside the insert's round-trip
   * window would otherwise read the PRE-insert curve and commit that
   * back. */
  async commitLatest(bindingId: string): Promise<void> {
    await this.#inflight.get(bindingId);
    const binding = this.bindings.find((b) => b.id === bindingId);
    const fromBinding = binding ? this.curveOf(binding) : undefined;
    // A first-point insert may have minted the curve under an empty id
    // (the binding already names the minted id). Fall back to the
    // just-adopted curve if the binding is not in the store yet.
    const curve = fromBinding ?? this.curves[this.curves.length - 1];
    if (curve) await this.commitInGesture(bindingId, curve);
  }

  private adopt(snap: ModulationSnapshot) {
    this.curves = snap.curves;
    this.bindings = snap.bindings;
    this.automationClips = snap.automationClips;
  }
}

export const modulation = new ModulationStore();
