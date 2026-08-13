/**
 * Project store: tracks, clips, selection. Track mutations go through the
 * backend commands optimistically; clip placement is a frontend concern in
 * the prototype (no clip-move command exists in the frozen surface).
 */

import { backend } from "../tauri";
import type { Clip, Project, ProjectSnapshot, TrackState } from "../types/ipc";

const uuid = () =>
  typeof crypto !== "undefined" && crypto.randomUUID
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(16).slice(2)}`;

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

  /** samples per beat at the project tempo */
  get samplesPerBeat(): number {
    return (60 / this.tempoBpm) * this.sampleRate;
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
    this.tracks = [...this.tracks, merged];
    return merged;
  }

  /** Insert (already-created) track right after another for stem grouping. */
  placeTrackAfter(track: TrackState, afterTrackId: string) {
    const rest = this.tracks.filter((t) => t.id !== track.id);
    const at = rest.findIndex((t) => t.id === afterTrackId);
    if (at === -1) {
      this.tracks = [...rest, track];
    } else {
      this.tracks = [...rest.slice(0, at + 1), track, ...rest.slice(at + 1)];
    }
  }

  async removeTrack(trackId: string) {
    this.tracks = this.tracks.filter((t) => t.id !== trackId);
    this.clips = this.clips.filter((c) => c.trackId !== trackId);
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
    this.patchTrack(trackId, { muted: !t.muted });
    await backend.setTrackMute(trackId, !t.muted);
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
  }

  // ── clips ──

  upsertClip(clip: Clip) {
    const exists = this.clips.some((c) => c.id === clip.id);
    this.clips = exists
      ? this.clips.map((c) => (c.id === clip.id ? clip : c))
      : [...this.clips, clip];
  }

  moveClip(clipId: string, timelineStartSamples: number) {
    this.clips = this.clips.map((c) =>
      c.id === clipId ? { ...c, timelineStartSamples: Math.max(0, Math.round(timelineStartSamples)) } : c,
    );
  }

  select(clipId: string | null) {
    this.selectedClipId = clipId;
  }

  /** Create a frontend clip (stem results, demo takes). */
  createClip(init: Omit<Clip, "id"> & { id?: string }): Clip {
    const clip: Clip = { id: init.id ?? uuid(), ...init } as Clip;
    this.clips = [...this.clips, clip];
    return clip;
  }
}

export const project = new ProjectStore();
