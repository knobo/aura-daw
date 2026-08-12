/**
 * MIDI domain store: ppq + tempo map + midi clips (ticks everywhere musical —
 * D-02). The tick↔sample bijection lives HERE on the frontend side, mirroring
 * midi::TempoMap: piecewise over sorted {tick,bpm} events, first at tick 0.
 * Edits are batch-shaped: one midi_set_notes per edit gesture, never per note.
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import type { MidiClip, MidiNote, ProjectSnapshot, TempoEvent } from "../types/ipc";

export interface MidiRegion {
  clipId: string;
  startTicks: number;
  endTicks: number;
}

class MidiStore {
  ppq = $state(960);
  tempoEvents = $state<TempoEvent[]>([{ tick: 0, bpm: 120 }]);
  clips = $state<MidiClip[]>([]);
  /** Clip open in the piano roll (null = closed). */
  openClipId = $state<string | null>(null);
  /** Selected clip on the timeline (for AI Studio targeting). */
  selectedClipId = $state<string | null>(null);
  /** Tick region marquee-selected in the piano roll — feeds AMT infill. */
  region = $state<MidiRegion | null>(null);

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

  // ── tick ↔ sample (piecewise over the tempo map) ──

  /** samples per tick while `bpm` rules. */
  private spt(bpm: number): number {
    return (project.sampleRate * 60) / (bpm * this.ppq);
  }

  ticksToSamples(ticks: number): number {
    const ev = this.tempoEvents;
    let samples = 0;
    for (let i = 0; i < ev.length; i++) {
      const end = i + 1 < ev.length ? ev[i + 1].tick : Infinity;
      if (ticks <= end || i === ev.length - 1) {
        return samples + (ticks - ev[i].tick) * this.spt(ev[i].bpm);
      }
      samples += (end - ev[i].tick) * this.spt(ev[i].bpm);
    }
    return samples;
  }

  samplesToTicks(samples: number): number {
    const ev = this.tempoEvents;
    let acc = 0;
    for (let i = 0; i < ev.length; i++) {
      const end = i + 1 < ev.length ? ev[i + 1].tick : Infinity;
      const span = (end - ev[i].tick) * this.spt(ev[i].bpm);
      if (samples <= acc + span || i === ev.length - 1) {
        return ev[i].tick + (samples - acc) / this.spt(ev[i].bpm);
      }
      acc += span;
    }
    return 0;
  }

  /** Ticks per bar at the project time signature. */
  get ticksPerBar(): number {
    return this.ppq * project.timeSignature[0];
  }

  applySnapshot(snap: ProjectSnapshot) {
    this.ppq = snap.ppq || 960;
    if (snap.tempoEvents?.length) this.tempoEvents = snap.tempoEvents;
    this.clips = snap.midiClips ?? [];
  }

  async init() {
    try {
      this.applySnapshot(await backend.getProjectState());
    } catch (err) {
      console.warn("[aura] midi init failed:", err);
    }
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
      console.error("[aura] midi_add_clip failed:", err);
      return null;
    }
  }

  /** Whole-clip note replace — one invoke per edit gesture (D-03). */
  async setNotes(clipId: string, notes: MidiNote[]): Promise<void> {
    const sorted = [...notes].sort((a, b) => a.tick - b.tick || a.key - b.key);
    this.clips = this.clips.map((c) => (c.id === clipId ? { ...c, notes: sorted } : c));
    try {
      const clip = await backend.midiSetNotes(clipId, sorted);
      this.upsert(clip);
    } catch (err) {
      console.error("[aura] midi_set_notes failed:", err);
    }
  }

  /** Frontend-only placement move (no midi clip-move command in the surface). */
  moveClip(clipId: string, timelineStartTicks: number) {
    const t = Math.max(0, Math.round(timelineStartTicks));
    this.clips = this.clips.map((c) => (c.id === clipId ? { ...c, timelineStartTicks: t } : c));
  }

  open(clipId: string) {
    this.openClipId = clipId;
    this.selectedClipId = clipId;
    if (this.region?.clipId !== clipId) this.region = null;
  }

  closeEditor() {
    this.openClipId = null;
  }

  select(clipId: string | null) {
    this.selectedClipId = clipId;
  }
}

export const midi = new MidiStore();
