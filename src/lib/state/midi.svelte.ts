/**
 * MIDI domain store: ppq + tempo map + midi clips (ticks everywhere musical —
 * D-02). The tick↔sample bijection is the backend-shipped section table
 * (round-2 §3.6): this store holds the table, `sectionTable.ts` does the
 * (pure, tempo-knowledge-free) lookup — this is the ONE bijection
 * implementation feeding every surface, not a frontend re-derivation.
 * Edits are batch-shaped: one midi_set_notes per edit gesture, never per note.
 */

import { backend } from "../tauri";
import { clipEditLoop } from "./clip-edit-loop.svelte";
import { project } from "./project.svelte";
import { sampleAtTick, tickAtSample, type SectionRow } from "../sectionTable";
import { toasts } from "./toasts.svelte";
import { byTickKey } from "../utils/note-ops";
import type { MidiClip, MidiNote, ProjectSnapshot, TempoEvent } from "../types/ipc";

export interface MidiRegion {
  clipId: string;
  startTicks: number;
  endTicks: number;
}

class MidiStore {
  ppq = $state(960);
  tempoEvents = $state<TempoEvent[]>([{ tick: 0, bpm: 120 }]);
  /** The shipped section table (round-2 §3.6) — sectionTable.ts's ONLY input
   * besides sampleRate/ppq. Populated from the cold-start snapshot. */
  sectionTable = $state<SectionRow[]>([]);
  clips = $state<MidiClip[]>([]);
  /** Clip open in the piano roll (null = closed). */
  openClipId = $state<string | null>(null);
  /** Selected clip on the timeline (for AI Studio targeting). */
  selectedClipId = $state<string | null>(null);
  /** Tick region marquee-selected in the piano roll — feeds AMT infill. */
  region = $state<MidiRegion | null>(null);
  /** Clip glowing from a just-landed result (hum-to-song etc.); transient. */
  flashClipId = $state<string | null>(null);
  private flashTimer: ReturnType<typeof setTimeout> | null = null;
  /** Clipboard for Ctrl+C/V/D clip stamping (spec §6) — a snapshot, not a
   * live reference, so later edits to the source clip don't leak into a
   * clip already copied. */
  clipboard = $state<MidiClip | null>(null);
  /** Note-level clipboard (piano roll Ctrl+C/X/V) — deep copies with their
   * original in-clip ticks, so paste lands in place (also across clips) and
   * the pasted selection can then be nudged/transposed with the arrow keys.
   * Separate from `clipboard`, which stamps whole clips on the timeline. */
  noteClipboard = $state<MidiNote[] | null>(null);

  get openClip(): MidiClip | null {
    return this.clips.find((c) => c.id === this.openClipId) ?? null;
  }
  get selectedClip(): MidiClip | null {
    return this.clips.find((c) => c.id === this.selectedClipId) ?? null;
  }

  clipsOf(trackId: string): MidiClip[] {
    return this.clips.filter((c) => c.trackId === trackId);
  }
  clipById(id: string): MidiClip | undefined {
    return this.clips.find((c) => c.id === id);
  }

  // ── tick ↔ sample (the shipped section table, round-2 §3.6) ──

  ticksToSamples(ticks: number): number {
    return sampleAtTick(this.sectionTable, project.sampleRate, this.ppq, ticks);
  }

  samplesToTicks(samples: number): number {
    return tickAtSample(this.sectionTable, project.sampleRate, this.ppq, samples);
  }

  /** Ticks per bar at the project time signature. */
  get ticksPerBar(): number {
    return this.ppq * project.timeSignature[0];
  }

  /** Effective content (loop) length: explicit, else the placement length —
   * the single accessor every content-relative reader (piano roll bounds,
   * repeat rendering, the clip-edit loop) goes through. */
  effectiveContentLengthTicks(clip: MidiClip): number {
    return Math.max(1, clip.contentLengthTicks ?? clip.lengthTicks);
  }

  applySnapshot(snap: ProjectSnapshot) {
    this.ppq = snap.ppq || 960;
    if (snap.tempoEvents?.length) this.tempoEvents = snap.tempoEvents;
    this.sectionTable = snap.sectionTable ?? [];
    this.clips = snap.midiClips ?? [];
  }

  async init() {
    this.subscribeToTakes();
    try {
      this.applySnapshot(await backend.getProjectState());
    } catch (err) {
      console.warn("[aura] midi init failed:", err);
    }
  }

  /** One subscription for the process. `init()` is ALSO the undo/redo
   * re-pull (projectops.repull), so subscribing unguarded would add a
   * listener per undo. */
  private takesSubscribed = false;
  private subscribeToTakes() {
    if (this.takesSubscribed) return;
    this.takesSubscribed = true;
    backend.on("recording://state", (rs) => {
      if (rs.recording || !rs.midiClipId) return;
      void this.adoptTake(rs.midiClipId);
    });
  }

  /** A MIDI take is registered by the engine's own transaction, and the
   * `project://changed` that transaction emits carries only the
   * audio-shaped `Project` — `clips` on `recording://state` is likewise the
   * audio half. Nothing else pulls the take's MIDI clip in, so without this
   * the notes the user just played stay invisible until an undo/redo or a
   * reopen. Selecting + flashing it mirrors what a landed hum result does. */
  private async adoptTake(clipId: string) {
    await this.refresh();
    this.select(clipId);
    this.flash(clipId);
  }

  async refresh() {
    try {
      this.clips = await backend.midiGetClips();
    } catch (err) {
      console.warn("[aura] midi_get_clips failed:", err);
    }
  }

  // ── mutations (optimistic; backend value wins on return) ──

  private upsert(clip: MidiClip) {
    const exists = this.clips.some((c) => c.id === clip.id);
    this.clips = exists
      ? this.clips.map((c) => (c.id === clip.id ? clip : c))
      : [...this.clips, clip];
  }

  async addClip(
    trackId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    name: string | null = null,
  ): Promise<MidiClip | null> {
    try {
      const clip = await backend.midiAddClip(trackId, name, timelineStartTicks, lengthTicks);
      this.upsert(clip);
      return clip;
    } catch (err) {
      // A double-click that produces no clip and no message reads as a dead
      // UI; the console is not a user surface.
      toasts.error("COULD NOT ADD MIDI CLIP", String(err));
      console.error("[aura] midi_add_clip failed:", err);
      return null;
    }
  }

  /** Rename a clip. The backend trims and rejects empty names; this returns
   *  false (with the reason toasted) rather than throwing at the caller. */
  async renameClip(clipId: string, name: string): Promise<boolean> {
    const previous = this.clips.find((c) => c.id === clipId)?.name;
    if (name.trim() === previous) return true;
    try {
      this.upsert(await backend.midiRenameClip(clipId, name));
      return true;
    } catch (err) {
      toasts.error("RENAME FAILED", String(err));
      return false;
    }
  }

  /** Whole-clip note replace — one invoke per edit gesture (D-03). */
  async setNotes(clipId: string, notes: MidiNote[]): Promise<void> {
    const sorted = [...notes].sort(byTickKey);
    this.clips = this.clips.map((c) => (c.id === clipId ? { ...c, notes: sorted } : c));
    try {
      const clip = await backend.midiSetNotes(clipId, sorted);
      this.upsert(clip);
    } catch (err) {
      console.error("[aura] midi_set_notes failed:", err);
    }
  }

  /** Frontend-only placement move (used DURING a drag; see commitBounds for
   * the persisted end of the gesture — D-03: one invoke per gesture, not
   * one per pointermove). */
  moveClip(clipId: string, timelineStartTicks: number) {
    const t = Math.max(0, Math.round(timelineStartTicks));
    this.clips = this.clips.map((c) => (c.id === clipId ? { ...c, timelineStartTicks: t } : c));
  }

  /** Persist a clip's current placement/content bounds — called once at the
   * END of a move or edge-drag gesture (pointerup), never per pointermove.
   * Closes the pre-existing hole where moveClip() never reached the
   * scheduler or the project file. */
  async setClipBounds(
    clipId: string,
    timelineStartTicks: number,
    lengthTicks: number,
    contentLengthTicks?: number,
  ): Promise<void> {
    const t = Math.max(0, Math.round(timelineStartTicks));
    const len = Math.max(1, Math.round(lengthTicks));
    const cl = contentLengthTicks === undefined ? undefined : Math.max(1, Math.round(contentLengthTicks));
    this.clips = this.clips.map((c) =>
      c.id === clipId
        ? { ...c, timelineStartTicks: t, lengthTicks: len, contentLengthTicks: cl }
        : c,
    );
    try {
      const clip = await backend.midiSetClipBounds(clipId, t, len, cl ?? null);
      this.upsert(clip);
    } catch (err) {
      console.error("[aura] midi_set_clip_bounds failed:", err);
    }
  }

  /** Commit a clip's CURRENT in-store bounds to the backend — the pointerup
   * end of a move/edge-drag gesture that used moveClip()/local mutation
   * during the drag. */
  async commitBounds(clipId: string): Promise<void> {
    const c = this.clipById(clipId);
    if (!c) return;
    await this.setClipBounds(clipId, c.timelineStartTicks, c.lengthTicks, c.contentLengthTicks);
  }

  /** Remove a clip from its track — same optimistic-then-backend shape as
   * `project.removeTrack`. Clears any selection/editor/flash state pointing
   * at the removed clip so nothing lingers referencing a dead id. */
  async removeClip(clipId: string): Promise<void> {
    this.clips = this.clips.filter((c) => c.id !== clipId);
    if (this.selectedClipId === clipId) this.selectedClipId = null;
    if (this.openClipId === clipId) this.closeEditor();
    if (this.flashClipId === clipId) this.flashClipId = null;
    await backend.midiRemoveClip(clipId);
  }

  open(clipId: string) {
    this.openClipId = clipId;
    this.selectedClipId = clipId;
    if (this.region?.clipId !== clipId) this.region = null;
    const clip = this.clipById(clipId);
    if (clip) {
      void clipEditLoop.enter({
        trackId: clip.trackId,
        startSamples: this.ticksToSamples(clip.timelineStartTicks),
        endSamples: this.ticksToSamples(
          clip.timelineStartTicks + this.effectiveContentLengthTicks(clip),
        ),
      });
    }
  }

  closeEditor() {
    this.openClipId = null;
    void clipEditLoop.exit();
  }

  select(clipId: string | null) {
    this.selectedClipId = clipId;
  }

  /** Glow a clip for a few seconds (freshly landed hum/generation results). */
  flash(clipId: string, ms = 4000) {
    if (this.flashTimer) clearTimeout(this.flashTimer);
    this.flashClipId = clipId;
    this.flashTimer = setTimeout(() => {
      if (this.flashClipId === clipId) this.flashClipId = null;
      this.flashTimer = null;
    }, ms);
  }

  // ── clip stamping (Ctrl+C/V/D, spec §6) ──

  copySelected() {
    const c = this.selectedClip;
    this.clipboard = c ? { ...c, notes: c.notes.map((n) => ({ ...n })) } : null;
  }

  /** Independent copy at `timelineStartTicks` on the target track — the
   * SELECTED clip's track when one is selected, else the copied clip's own
   * original track. Fresh backend id + fresh note ids (midi_set_notes's
   * keep-rule mints fresh ids whenever the target clip starts with no
   * notes, which a brand-new clip always does — see progress.md). */
  async pasteAtPlayhead(timelineStartTicks: number): Promise<MidiClip | null> {
    const src = this.clipboard;
    if (!src) return null;
    const targetTrackId = this.selectedClip?.trackId ?? src.trackId;
    return this.stamp(src, targetTrackId, Math.max(0, Math.round(timelineStartTicks)));
  }

  /** Independent copy immediately after the currently selected clip, on the
   * same track. */
  async duplicateSelected(): Promise<MidiClip | null> {
    const src = this.selectedClip;
    if (!src) return null;
    return this.stamp(src, src.trackId, src.timelineStartTicks + src.lengthTicks);
  }

  /** Public entry to the copy-stamp behind paste/duplicate: place an
   * independent copy of `clipId` on `trackId` at `timelineStartTicks`. The
   * library panel's project-clips drag lands here, so there is exactly ONE
   * stamping path in the app. */
  async stampClipTo(
    clipId: string,
    trackId: string,
    timelineStartTicks: number,
  ): Promise<MidiClip | null> {
    const src = this.clipById(clipId);
    if (!src) return null;
    return this.stamp(src, trackId, Math.max(0, Math.round(timelineStartTicks)));
  }

  private async stamp(
    src: MidiClip,
    trackId: string,
    timelineStartTicks: number,
  ): Promise<MidiClip | null> {
    try {
      let created = await backend.midiAddClip(trackId, src.name, timelineStartTicks, src.lengthTicks);
      this.upsert(created);
      if (src.contentLengthTicks !== undefined) {
        created = await backend.midiSetClipBounds(
          created.id,
          timelineStartTicks,
          src.lengthTicks,
          src.contentLengthTicks,
        );
        this.upsert(created);
      }
      if (src.notes.length > 0) {
        const copiedNotes = src.notes.map((n) => ({ ...n }));
        created = await backend.midiSetNotes(created.id, copiedNotes);
        this.upsert(created);
      }
      return created;
    } catch (err) {
      console.error("[aura] clip stamp failed:", err);
      return null;
    }
  }
}

export const midi = new MidiStore();
