/**
 * Project store: tracks, clips, selection. Track mutations go through the
 * backend commands optimistically. Clip placement mirrors midi.svelte.ts's
 * moveClip/commitBounds split (Plan E Task 4): `moveClip` is a frontend-only
 * preview applied per pointermove/arrow-keypress during a drag (D-03: one
 * invoke per gesture, not one per event), and `commitClipMove` persists the
 * store's CURRENT position through the `move_clip` channel once, at the end
 * of the gesture.
 */

import { backend } from "../tauri";
import { midiIo } from "./midiio.svelte";
import { modulation } from "./modulation.svelte";
import { toasts } from "./toasts.svelte";
import type { AutomationMode, Clip, Project, ProjectSnapshot, TrackState } from "../types/ipc";

class ProjectStore {
  name = $state("Untitled");
  sampleRate = $state(48000);
  tempoBpm = $state(120);
  timeSignature = $state<[number, number]>([4, 4]);
  tracks = $state<TrackState[]>([]);
  clips = $state<Clip[]>([]);
  selectedClipId = $state<string | null>(null);

  get selectedClip(): Clip | null {
    return this.clips.find((c) => c.id === this.selectedClipId) ?? null;
  }

  /** samples per beat at the project's FLAT tempo. Deferred, not fixed, by
   * Plan C+D's Task 9 (docs/superpowers/plans/2026-08-14-plan-c-d-time-
   * project-v3.md): this is round-2 §3.6's third independent bijection
   * (the other two — midi.svelte.ts's, demo.ts's — now consume the shipped
   * section table). Grid-snapping call sites (App.svelte, ClipView.svelte,
   * Timeline.svelte, view.svelte.ts) assume ONE scalar for the whole
   * timeline; making this position-aware means passing a tick/sample
   * argument through each of them, a UI-shaped refactor this task's own
   * scope didn't reach. Recorded per ADR 0007 — not a silent gap. */
  get samplesPerBeat(): number {
    const den = this.timeSignature[1] || 4;
    return (60 / this.tempoBpm) * this.sampleRate * (4 / den);
  }
  get samplesPerBar(): number {
    return this.samplesPerBeat * this.timeSignature[0];
  }

  clipsOf(trackId: string): Clip[] {
    return this.clips.filter((c) => c.trackId === trackId);
  }

  trackById(id: string): TrackState | undefined {
    return this.tracks.find((t) => t.id === id);
  }

  /** Absolute path of the open .aura dir (null when nothing is open). */
  projectDir = $state<string | null>(null);

  private applyProject(p: Project) {
    this.name = p.name;
    this.sampleRate = p.sampleRate;
    this.tempoBpm = p.tempoBpm;
    if (p.timeSignature) this.timeSignature = [p.timeSignature[0], p.timeSignature[1]];
    this.tracks = p.tracks;
    this.clips = p.clips;
  }

  applySnapshot(snap: ProjectSnapshot) {
    if (snap.projectName) this.name = snap.projectName;
    this.projectDir = snap.projectDir ?? null;
    this.sampleRate = snap.transport.sampleRate;
    this.tempoBpm = snap.transport.tempoBpm;
    const meter = snap.meterMap?.[0];
    if (meter) this.timeSignature = [meter.num, meter.den];
    this.tracks = snap.tracks;
    this.clips = snap.clips;
  }

  /** Re-pull the control-plane snapshot (post-job auto-imports etc.). */
  async reload() {
    try {
      this.applySnapshot(await backend.getProjectState());
    } catch (err) {
      console.warn("[aura] get_project_state failed:", err);
    }
  }

  async init() {
    try {
      this.applySnapshot(await backend.getProjectState());
    } catch {
      // pre-phase-2 engine: assemble from parts
      try {
        this.applyProject(await backend.getProject());
      } catch (err) {
        console.warn("[aura] project load failed:", err);
      }
    }
    backend.on("project://changed", (p) => this.applyProject(p));
    backend.on("recording://state", (rs) => {
      if (!rs.recording && rs.clips) {
        for (const clip of rs.clips) this.upsertClip(clip);
      }
    });
  }

  // ── tracks ──

  async addTrack(init: Partial<TrackState> = {}): Promise<TrackState> {
    const track = await backend.addTrack(init);
    // Backend may ignore cosmetic fields; keep requested ones locally.
    const merged = { ...track, ...("color" in init ? { color: init.color! } : {}), ...("name" in init ? { name: init.name! } : {}) };
    // `project://changed` is emitted inside the add-track transaction too,
    // same as `clips_paste` — a blind append here would carry the identical
    // duplicate-track race `upsertTrack` exists to close (fix round 2,
    // minor 4). `upsertTrack` REPLACES a matching entry rather than
    // skipping it, so the cosmetic-override merge above still lands even
    // if the store already picked the track up via the event.
    this.upsertTrack(merged);
    return merged;
  }

  /** Idempotent track insert (the `addTrack`-shaped duplicate `upsertClip`
   * already guards against on the clip side). Needed because `clips_paste`
   * commits `project://changed` INSIDE the transaction — `applyProject` can
   * already have replaced `tracks` with the authoritative post-paste list
   * by the time the paste's own `await` resolves and tries to append the
   * track it just asked for, which a blind spread would duplicate. */
  upsertTrack(track: TrackState) {
    const exists = this.tracks.some((t) => t.id === track.id);
    this.tracks = exists
      ? this.tracks.map((t) => (t.id === track.id ? track : t))
      : [...this.tracks, track];
  }

  async removeTrack(trackId: string) {
    this.tracks = this.tracks.filter((t) => t.id !== trackId);
    this.clips = this.clips.filter((c) => c.trackId !== trackId);
    modulation.dropTrack(trackId);
    await backend.removeTrack(trackId);
  }

  private patchTrack(trackId: string, patch: Partial<TrackState>) {
    this.tracks = this.tracks.map((t) => (t.id === trackId ? { ...t, ...patch } : t));
  }

  /** Local-only patch (used when a command already persisted the change). */
  patchTrackLocal(trackId: string, patch: Partial<TrackState>) {
    this.patchTrack(trackId, patch);
  }

  async setGain(trackId: string, gainDb: number) {
    this.patchTrack(trackId, { gainDb });
    await backend.setTrackGain(trackId, gainDb);
  }
  async setPan(trackId: string, pan: number) {
    this.patchTrack(trackId, { pan });
    await backend.setTrackPan(trackId, pan);
  }
  async toggleMute(trackId: string) {
    const t = this.trackById(trackId);
    if (!t) return;
    await this.setMute(trackId, !t.muted);
  }
  /** Set mute to an absolute value; no-op (no invoke) when already there. */
  async setMute(trackId: string, muted: boolean) {
    const t = this.trackById(trackId);
    if (!t || t.muted === muted) return;
    this.patchTrack(trackId, { muted });
    await backend.setTrackMute(trackId, muted);
  }
  async toggleSolo(trackId: string) {
    const t = this.trackById(trackId);
    if (!t) return;
    await this.setSolo(trackId, !t.soloed);
  }
  /** Set solo to an absolute value; no-op (no invoke) when already there. */
  async setSolo(trackId: string, soloed: boolean) {
    const t = this.trackById(trackId);
    if (!t || t.soloed === soloed) return;
    this.patchTrack(trackId, { soloed });
    await backend.setTrackSolo(trackId, soloed);
  }
  async toggleArm(trackId: string) {
    const t = this.trackById(trackId);
    if (!t) return;
    this.patchTrack(trackId, { armed: !t.armed });
    await backend.setTrackArm(trackId, !t.armed);
    // Ruling 1's UX bridge: arming a MIDI track routes the keyboard to it;
    // disarming clears the route. `armed` stays the document flag, the
    // routing target is a separate app-config selection (no op).
    // Disarming only clears the route when THIS track holds it: with two
    // MIDI tracks armed the route belongs to the last one armed, and
    // disarming the other used to take the keyboard away from it silently
    // (whole-track review).
    if (t.kind === "midi") {
      if (!t.armed) {
        await midiIo.setInputTrack(trackId);
      } else if (midiIo.targetTrackId === trackId) {
        await midiIo.setInputTrack(null);
      }
    }
  }
  /** Set the track's automation mode; no-op (no invoke) when already there. */
  async setAutomationMode(trackId: string, mode: AutomationMode) {
    const t = this.trackById(trackId);
    if (!t || t.automationMode === mode) return;
    this.patchTrack(trackId, { automationMode: mode });
    await backend.setTrackAutomationMode(trackId, mode);
  }

  /**
   * Rename a track (lanes UX). NOT optimistic, unlike the mix setters
   * above: the backend trims and rejects an empty name, so the store takes
   * the name it actually stored rather than the string that was typed —
   * otherwise a rejected rename would leave the UI showing a name the
   * project does not have.
   *
   * Returns true when the name changed. A no-op rename (same name, or only
   * whitespace added) never reaches the backend, so it costs no undo step.
   */
  async renameTrack(trackId: string, name: string): Promise<boolean> {
    const t = this.trackById(trackId);
    if (!t) return false;
    const trimmed = name.trim();
    if (trimmed === t.name) return false;
    if (!trimmed) {
      toasts.error("RENAME FAILED", "Name cannot be empty");
      return false;
    }
    try {
      const updated = await backend.setTrackName(trackId, trimmed);
      this.patchTrack(trackId, { name: updated.name });
      return true;
    } catch (err) {
      console.warn("[aura] set_track_name failed:", err);
      toasts.error("RENAME FAILED", String(err));
      return false;
    }
  }

  /**
   * Apply a whole lane arrangement — display order + group per lane — in
   * one backend transaction, and mirror it locally.
   *
   * Optimistic, deliberately: this is the tail of a drag, and snapping the
   * lanes back to the old order for the duration of an IPC round trip is
   * exactly the jank the gesture is meant not to have. A failure re-pulls
   * the authoritative snapshot, so a rejected arrangement corrects itself
   * rather than leaving the UI lying.
   *
   * Returns whether it went through, so a caller that layered its own
   * optimistic state on top (e.g. carrying fold state to a renamed group)
   * knows to roll that back too.
   */
  async arrangeLanes(placements: { trackId: string; group: string | null }[]): Promise<boolean> {
    const byId = new Map(this.tracks.map((t) => [t.id, t]));
    const reordered: TrackState[] = [];
    for (const p of placements) {
      const t = byId.get(p.trackId);
      if (t) reordered.push({ ...t, group: p.group });
    }
    // Guard the optimistic write the same way the backend guards the op:
    // a placement list that lost a track would silently delete lanes here.
    if (reordered.length !== this.tracks.length) {
      console.warn("[aura] arrangeLanes: incomplete arrangement, ignoring");
      return false;
    }
    const previous = this.tracks;
    this.tracks = reordered;
    try {
      await backend.arrangeLanes(placements);
      return true;
    } catch (err) {
      console.warn("[aura] arrange_lanes failed:", err);
      this.tracks = previous;
      toasts.error("LANE ARRANGE FAILED", String(err));
      await this.reload();
      return false;
    }
  }

  /** Open a gesture boundary (Plan E Task 14) — call on `pointerdown` of a
   * fader/pan control, before the first `setGain`/`setPan` of the drag.
   * `label` is the gesture's history label (e.g. "gain drag"). No-op in
   * demo mode (`gestureBegin?` is optional — no history to fold into). */
  async beginGesture(label: string): Promise<string | undefined> {
    return backend.gestureBegin?.(label);
  }

  /** Close the gesture boundary opened by `beginGesture` — call on
   * `pointerup`/`pointercancel`. Pass the id `beginGesture` resolved to;
   * a mismatch is a no-op so a late end cannot close a different
   * gesture. Omitting the id keeps the old close-whatever contract.
   * Safe to call even without a matching `beginGesture`. */
  async endGesture(id?: string): Promise<void> {
    if (id == null) return;
    await backend.gestureEnd?.(id);
  }

  // ── clips ──

  upsertClip(clip: Clip) {
    const exists = this.clips.some((c) => c.id === clip.id);
    this.clips = exists
      ? this.clips.map((c) => (c.id === clip.id ? clip : c))
      : [...this.clips, clip];
  }

  /** Frontend-only placement move (used DURING a drag/arrow-keypress; see
   * commitClipMove for the persisted end of the gesture). */
  moveClip(clipId: string, timelineStartSamples: number) {
    this.clips = this.clips.map((c) =>
      c.id === clipId ? { ...c, timelineStartSamples: Math.max(0, Math.round(timelineStartSamples)) } : c,
    );
  }

  /** Persist a clip's CURRENT in-store position — the pointerup/arrow-commit
   * end of a move gesture that used moveClip() for the live preview. No-op
   * (no invoke) when the clip id is unknown, matching setMute/setSolo's
   * no-op-on-nothing-to-do convention. */
  async commitClipMove(clipId: string): Promise<void> {
    const c = this.clips.find((c) => c.id === clipId);
    if (!c) return;
    try {
      await backend.moveClip?.(clipId, c.timelineStartSamples);
    } catch (err) {
      console.error("[aura] move_clip failed:", err);
    }
  }

  select(clipId: string | null) {
    this.selectedClipId = clipId;
  }

  /** Remove a clip from its track — same optimistic-then-backend shape as
   * `removeTrack`. */
  async removeClip(clipId: string): Promise<void> {
    this.clips = this.clips.filter((c) => c.id !== clipId);
    if (this.selectedClipId === clipId) this.selectedClipId = null;
    await backend.removeClip(clipId);
  }

  /** Open a clip's source audio file in the OS's default application for its
   * file type — "open in external editor" (double-click an audio clip on the
   * timeline), the audio-clip analogue of `midi.open`'s double-click-opens-
   * piano-roll. Optional: real backend only, same convention as `moveClip?`/
   * `gestureBegin?` — the demo backend has no on-disk source to hand off. */
  async openInExternalEditor(clipId: string): Promise<void> {
    try {
      await backend.openClipInExternalEditor?.(clipId);
    } catch (err) {
      console.error("[aura] open_clip_in_external_editor failed:", err);
      toasts.error("COULD NOT OPEN EXTERNAL EDITOR", String(err));
    }
  }
}

export const project = new ProjectStore();
