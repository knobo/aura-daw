/**
 * Automation lane mirror. Thin by construction (ADR 0006): the backend owns
 * the lanes, this store holds a copy for painting and emits whole-lane
 * replaces through `automation_set`. No tick<->sample math lives here —
 * callers convert through `midi.sampleAtTick`/`midi.tickAtSample`, which read
 * the backend-shipped section table.
 *
 * Edit shape (D-03 + round-2 §4.4): a drag PREVIEWS locally (`preview`, no
 * invoke) and COMMITS once on pointerup (`commit`), inside a
 * `gesture_begin`/`gesture_end` bracket so the whole interaction is one undo
 * entry and one persist.
 */

import { backend } from "../tauri";
import type { AutomationLane, AutomationPoint } from "../types/ipc";

/** `targetNode` prefix for built-in params of a track's node. */
export const TRACK_TARGET_PREFIX = "track:";
/** Built-in track param ids (mirrors `plugins::automation`'s constants). */
export const TRACK_PARAM_GAIN = 0;

export function trackTarget(trackId: string): string {
  return `${TRACK_TARGET_PREFIX}${trackId}`;
}

/** Track id of a `"track:<id>"` target, or null for a plugin target. */
export function trackIdOfTarget(targetNode: string): string | null {
  return targetNode.startsWith(TRACK_TARGET_PREFIX)
    ? targetNode.slice(TRACK_TARGET_PREFIX.length)
    : null;
}

class AutomationStore {
  lanes = $state<AutomationLane[]>([]);
  /** Track ids whose automation overlay is shown (reassigned, not mutated —
   * the reactivity convention PianoRoll's selection Set uses). */
  visible = $state<Set<string>>(new Set());

  laneFor(targetNode: string, paramId: number): AutomationLane | undefined {
    return this.lanes.find((l) => l.targetNode === targetNode && l.paramId === paramId);
  }

  gainLaneFor(trackId: string): AutomationLane | undefined {
    return this.laneFor(trackTarget(trackId), TRACK_PARAM_GAIN);
  }

  lanesForPlugin(instanceId: string): AutomationLane[] {
    return this.lanes.filter((l) => l.targetNode === instanceId);
  }

  async reload(): Promise<void> {
    if (!backend.automationGet) return; // demo backend: no automation
    try {
      this.lanes = await backend.automationGet();
    } catch (err) {
      console.warn("[aura] automation_get failed:", err);
    }
  }

  /** Drag-time local patch — no invoke (mirrors `project.moveClip`). */
  preview(laneId: string, points: AutomationPoint[]) {
    this.lanes = this.lanes.map((l) => (l.id === laneId ? { ...l, points } : l));
  }

  /** Persist a lane's CURRENT point set. An empty `points` deletes it. The
   * returned list is authoritative (the backend normalizes: sorted by tick,
   * duplicate ticks collapsed last-wins). */
  async commit(lane: AutomationLane): Promise<void> {
    if (!backend.automationSet) return;
    try {
      this.lanes = await backend.automationSet(lane);
    } catch (err) {
      console.error("[aura] automation_set failed:", err);
    }
  }

  isVisible(trackId: string): boolean {
    return this.visible.has(trackId);
  }

  toggleVisible(trackId: string) {
    const next = new Set(this.visible);
    if (next.has(trackId)) next.delete(trackId);
    else next.add(trackId);
    this.visible = next;
  }
}

export const automation = new AutomationStore();
