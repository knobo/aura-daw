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
  Curve,
  ModulationSnapshot,
  TargetRef,
} from "../types/ipc";

function targetsEqual(a: TargetRef, b: TargetRef): boolean {
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

class ModulationStore {
  curves = $state<Curve[]>([]);
  bindings = $state<Binding[]>([]);
  automationClips = $state<AutomationClip[]>([]);

  bindingsFor(target: TargetRef): Binding[] {
    return this.bindings.filter((b) => targetsEqual(b.target, target));
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

  private adopt(snap: ModulationSnapshot) {
    this.curves = snap.curves;
    this.bindings = snap.bindings;
    this.automationClips = snap.automationClips;
  }
}

export const modulation = new ModulationStore();
