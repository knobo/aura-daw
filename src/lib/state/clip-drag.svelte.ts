/**
 * Group drag/resize for the arrangement timeline.
 *
 * ONE controller shared by ClipView and MidiClipView so the group maths
 * exist once. The preview/commit split is the established one (Plan E Task
 * 4, D-03): pointermove mutates the stores locally ONLY; a single
 * `move_clips` invoke lands at pointerup, inside the gesture boundary, so a
 * whole drag is one undo entry.
 *
 * SNAPPING (plan scope ruling D): the ANCHOR — the clip under the pointer —
 * is snapped through the existing `view.snapSamples`, exactly as a
 * single-clip drag already is, and the resulting delta is applied to every
 * selected clip. Snapping each clip on its own would destroy the relative
 * offsets the whole feature exists to preserve. The delta, not the
 * individual clip, is clamped at the timeline origin — the same rule
 * note-ops' `nudgeSelection` follows for notes.
 *
 * ORDERING (the frontend half of the gesture-before-session lock rule):
 * `gestureBegin` is issued before any mutation, and the `move_clips` invoke
 * is AWAITED before `gestureEnd`. An un-awaited mutation racing `gestureEnd`
 * is the exact TOCTOU Plan E Task 14 closed backend-side.
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import { view } from "./view.svelte";
import { uiZoomFactor } from "../utils/ui-zoom";
import { clipSelection } from "./clip-selection.svelte";
import type { ClipRef } from "../utils/clip-selection";
import type { ClipPlacement } from "../types/ipc";

interface AudioOrigin {
  kind: "audio";
  id: string;
  startSamples: number;
}
interface MidiOrigin {
  kind: "midi";
  id: string;
  startTicks: number;
  lengthTicks: number;
  /** Original optional — `undefined` means "same as length". Cancel must
   * write this back, not the pinned effective value, or a cancelled
   * first-time resize silently establishes a loop length. */
  contentLengthTicks: number | undefined;
  /** Effective length at drag start, used only for the live preview. */
  pinnedContentLengthTicks: number;
}
type Origin = AudioOrigin | MidiOrigin;

class ClipDragController {
  active = $state(false);
  /** True once the pointer has travelled far enough to count as a drag. */
  moved = $state(false);
  mode = $state<"move" | "resize">("move");
  /** Set by MidiClipView while a right-edge hover is live, so sibling
   * selected clips can show the same affordance. Pure chrome — no document
   * meaning. */
  edgeHoverActive = $state(false);

  private origins: Origin[] = [];
  private anchorOrigSamples = 0;
  private minStartSamples = 0;
  private minStartTicks = 0;
  private startClientX = 0;
  private gestureId: Promise<string | undefined> | undefined;

  /** Snapshot the drag: every selected clip's origin, plus the anchor's.
   * When the anchor is NOT part of the selection, the drag is that clip
   * alone (clicking an unselected clip and dragging in one motion). */
  begin(anchor: ClipRef, clientX: number, mode: "move" | "resize" = "move") {
    const refs = clipSelection.has(anchor) ? clipSelection.refs() : [anchor];
    this.origins = [];
    for (const r of refs) {
      if (r.kind === "audio") {
        const c = project.clips.find((x) => x.id === r.id);
        if (c) this.origins.push({ kind: "audio", id: c.id, startSamples: c.timelineStartSamples });
      } else {
        const c = midi.clipById(r.id);
        if (c)
          this.origins.push({
            kind: "midi",
            id: c.id,
            startTicks: c.timelineStartTicks,
            lengthTicks: c.lengthTicks,
            contentLengthTicks: c.contentLengthTicks,
            pinnedContentLengthTicks: midi.effectiveContentLengthTicks(c),
          });
      }
    }
    // The reference point the pointer's dx targets, then (re)snaps: MOVE
    // snaps the clip's START (unchanged single-clip behavior). RESIZE snaps
    // the clip's END — the pre-refactor MidiClipView code snapped
    // `dragOrigTicks + dragOrigLengthTicks` (converted through the section
    // table) and derived the new length from the snapped end. Anchoring the
    // START for resize too, as an earlier version of this controller did,
    // gives a DIFFERENT snapped length whenever the length isn't a grid
    // multiple — ruling D's "a one-clip selection behaves bit-for-bit as it
    // does today" makes this a correctness bug, not a style choice.
    if (mode === "resize") {
      const c = anchor.kind === "midi" ? midi.clipById(anchor.id) : undefined;
      this.anchorOrigSamples = c ? midi.ticksToSamples(c.timelineStartTicks + c.lengthTicks) : 0;
    } else {
      this.anchorOrigSamples =
        anchor.kind === "audio"
          ? (project.clips.find((c) => c.id === anchor.id)?.timelineStartSamples ?? 0)
          : midi.ticksToSamples(midi.clipById(anchor.id)?.timelineStartTicks ?? 0);
    }
    this.minStartSamples = Math.min(
      ...this.origins.map((o) =>
        o.kind === "audio" ? o.startSamples : midi.ticksToSamples(o.startTicks),
      ),
      // Only a MOVE anchor is a "start" in the same unit as the rest of this
      // min — a RESIZE anchor is the clip's END and must not pull the floor
      // in (the position clamp below is move-only regardless).
      mode === "move" ? this.anchorOrigSamples : Number.MAX_SAFE_INTEGER,
    );
    this.minStartTicks = Math.min(
      ...this.origins.filter((o): o is MidiOrigin => o.kind === "midi").map((o) => o.startTicks),
      Number.MAX_SAFE_INTEGER,
    );
    this.startClientX = clientX;
    this.mode = mode;
    this.moved = false;
    this.active = true;
    this.gestureId = backend.gestureBegin?.(mode === "resize" ? "resize clips" : "move clips");
  }

  /** The one delta pair the whole group moves by. Pure given the snapshot. */
  computeDelta(clientX: number, altKey: boolean): { deltaSamples: number; deltaTicks: number } {
    // clientX deltas are VISUAL px; view.spp is samples-per-LAYOUT-px
    // (view.width comes from clientWidth) — divide out the interface zoom
    // before scaling, or the clip runs ahead of the pointer under zoom.
    const dx = ((clientX - this.startClientX) / uiZoomFactor()) * view.spp;
    let target = this.anchorOrigSamples + dx;
    if (!altKey) target = view.snapSamples(target);
    let deltaSamples = Math.round(target - this.anchorOrigSamples);
    // The timeline-origin position clamp is MOVE-only: for RESIZE the
    // anchor is the clip's END, not a start position, so "keep it >= 0"
    // is meaningless here — the new length gets its own >= 1 clamp in
    // previewResize/end() instead.
    if (this.mode === "move" && this.minStartSamples + deltaSamples < 0) {
      deltaSamples = -this.minStartSamples;
    }
    let deltaTicks =
      midi.samplesToTicks(this.anchorOrigSamples + deltaSamples) -
      midi.samplesToTicks(this.anchorOrigSamples);
    if (
      this.mode === "move" &&
      this.minStartTicks !== Number.MAX_SAFE_INTEGER &&
      this.minStartTicks + deltaTicks < 0
    ) {
      deltaTicks = -this.minStartTicks;
    }
    return { deltaSamples, deltaTicks: Math.round(deltaTicks) };
  }

  /** Live preview — store-local only, no invoke (D-03). */
  move(clientX: number, altKey: boolean) {
    if (!this.active) return;
    if (Math.abs(clientX - this.startClientX) > 2) this.moved = true;
    if (!this.moved) return;
    const { deltaSamples, deltaTicks } = this.computeDelta(clientX, altKey);
    if (this.mode === "resize") {
      this.previewResize(deltaTicks);
      return;
    }
    for (const o of this.origins) {
      if (o.kind === "audio") project.moveClip(o.id, o.startSamples + deltaSamples);
      else midi.moveClip(o.id, o.startTicks + deltaTicks);
    }
  }

  /** Group loop-length adjust (Task 7): MIDI clips only — audio clips have
   * no shipped resize gesture (plan scope ruling G). */
  private previewResize(deltaTicks: number) {
    const byId = new Map(
      this.origins.filter((o): o is MidiOrigin => o.kind === "midi").map((o) => [o.id, o]),
    );
    if (byId.size === 0) return;
    midi.clips = midi.clips.map((c) => {
      const o = byId.get(c.id);
      if (!o) return c;
      return {
        ...c,
        lengthTicks: Math.max(1, o.lengthTicks + deltaTicks),
        contentLengthTicks: o.pinnedContentLengthTicks,
      };
    });
  }

  /** Commit: ONE move_clips from the stores' CURRENT values (same rule
   * `commitClipMove` follows), awaited, then close the gesture. */
  async end(): Promise<void> {
    if (!this.active) return;
    const idp = this.gestureId;
    this.gestureId = undefined;
    const moved = this.moved;
    const origins = this.origins;
    const mode = this.mode;
    this.active = false;
    this.moved = false;
    this.origins = [];
    if (moved && origins.length > 0) {
      const placements: ClipPlacement[] = [];
      for (const o of origins) {
        if (o.kind === "audio") {
          if (mode === "resize") continue; // scope ruling G
          const c = project.clips.find((x) => x.id === o.id);
          if (c) placements.push({ kind: "audio", clipId: c.id, timelineStartSamples: c.timelineStartSamples });
        } else {
          const c = midi.clipById(o.id);
          if (!c) continue;
          placements.push(
            mode === "resize"
              ? {
                  kind: "midi",
                  clipId: c.id,
                  timelineStartTicks: c.timelineStartTicks,
                  lengthTicks: c.lengthTicks,
                  contentLengthTicks: c.contentLengthTicks ?? o.pinnedContentLengthTicks,
                }
              : { kind: "midi", clipId: c.id, timelineStartTicks: c.timelineStartTicks },
          );
        }
      }
      if (placements.length > 0) {
        try {
          await backend.moveClips?.(placements);
        } catch (err) {
          console.error("[aura] move_clips failed:", err);
          this.restore(origins, mode);
        }
      }
    }
    const id = await idp;
    if (id) await backend.gestureEnd?.(id);
  }

  private restore(origins: Origin[], mode: "move" | "resize") {
    if (mode === "resize") {
      const byId = new Map(
        origins.filter((o): o is MidiOrigin => o.kind === "midi").map((o) => [o.id, o]),
      );
      midi.clips = midi.clips.map((c) => {
        const o = byId.get(c.id);
        if (!o) return c;
        return { ...c, lengthTicks: o.lengthTicks, contentLengthTicks: o.contentLengthTicks };
      });
      return;
    }
    for (const o of origins) {
      if (o.kind === "audio") project.moveClip(o.id, o.startSamples);
      else midi.moveClip(o.id, o.startTicks);
    }
  }

  /** pointercancel / Escape: undo the local preview back to each origin's
   * exact pre-drag position — the same `moveClip`/`project.moveClip` calls
   * `move()` used to apply it — THEN close the gesture. Leaving the preview
   * in place (the earlier behavior) is a phantom uncommitted edit: nothing
   * was ever sent to the backend, so the store silently disagrees with the
   * document until an unrelated reload happens to paper over it.
   *
   * In resize mode the preview also touched `lengthTicks`/
   * `contentLengthTicks` (never `timelineStartTicks` — resize doesn't move
   * the clip), so those need restoring too, or a cancelled resize leaves a
   * phantom length behind exactly like the phantom position the move-mode
   * fix above closed. */
  async cancel(): Promise<void> {
    if (!this.active) return;
    const idp = this.gestureId;
    this.gestureId = undefined;
    if (this.moved) this.restore(this.origins, this.mode);
    this.active = false;
    this.moved = false;
    this.origins = [];
    const id = await idp;
    if (id) await backend.gestureEnd?.(id);
  }
}

export const clipDrag = new ClipDragController();
