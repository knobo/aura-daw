/**
 * Timeline multi-selection — VIEWER state, mirroring PR #11's note-level
 * selection model at the clip level. It never lands in the document: no op,
 * no snapshot field, no project.json key, and the backend is never told
 * "what is selected" (clips_copy/clips_paste take explicit id lists).
 *
 * Reads are LIVE-FILTERED against project.clips/midi.clips rather than
 * pruned on an event: an undo, a project open or a track delete can remove a
 * selected clip at any time, and a stale key would otherwise reach
 * `move_clips` as an unknown id and fail the whole batch.
 */

import {
  applySelection,
  refKey,
  type ClipRef,
  type SelectionMode,
} from "../utils/clip-selection";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";

class ClipSelectionStore {
  /** Raw key set — may contain keys for clips the document has since lost;
   * every read below filters. */
  keys = $state<Set<string>>(new Set());
  /** Last-clicked clip: the group-drag anchor and the "focused" clip. May
   * name a clip that no longer exists — use `anchorLive()` to read it. */
  anchor = $state<ClipRef | null>(null);

  private liveAudio(): Set<string> {
    return new Set(project.clips.map((c) => c.id));
  }
  private liveMidi(): Set<string> {
    return new Set(midi.clips.map((c) => c.id));
  }

  /** The selection, filtered to clips the document still has, audio first
   * then midi, each in document order. */
  refs(): ClipRef[] {
    const out: ClipRef[] = [];
    for (const c of project.clips) {
      if (this.keys.has(refKey({ kind: "audio", id: c.id })))
        out.push({ kind: "audio", id: c.id });
    }
    for (const c of midi.clips) {
      if (this.keys.has(refKey({ kind: "midi", id: c.id })))
        out.push({ kind: "midi", id: c.id });
    }
    return out;
  }

  count(): number {
    return this.refs().length;
  }

  has(ref: ClipRef): boolean {
    if (!this.keys.has(refKey(ref))) return false;
    return ref.kind === "audio" ? this.liveAudio().has(ref.id) : this.liveMidi().has(ref.id);
  }

  audioIds(): string[] {
    return this.refs().filter((r) => r.kind === "audio").map((r) => r.id);
  }
  midiIds(): string[] {
    return this.refs().filter((r) => r.kind === "midi").map((r) => r.id);
  }

  /** The anchor, or null when the anchored clip is gone. */
  anchorLive(): ClipRef | null {
    const a = this.anchor;
    if (!a) return null;
    return this.has(a) ? a : null;
  }

  apply(hits: ClipRef[], mode: SelectionMode) {
    this.keys = applySelection(this.keys, hits, mode);
    if (hits.length > 0 && mode !== "subtract") this.anchor = hits[hits.length - 1];
  }

  selectOnly(ref: ClipRef) {
    this.keys = applySelection(this.keys, [ref], "replace");
    this.anchor = ref;
  }

  clear() {
    this.keys = new Set();
    this.anchor = null;
  }
}

export const clipSelection = new ClipSelectionStore();
