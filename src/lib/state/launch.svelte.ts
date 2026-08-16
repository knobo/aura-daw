/**
 * MIDI launch map store: the list of note→region/clip bindings, the
 * floating panel, and "jump the timeline to this marking". Document-shaped
 * — mutations go through `launch_set` when the backend has it; demo mode
 * keeps the list in memory.
 */

import { backend } from "../tauri";
import { midi } from "./midi.svelte";
import { transport } from "./transport.svelte";
import { view } from "./view.svelte";
import {
  bindingFocusSamples,
  clipWouldSelfTrigger,
  nextBindingName,
  nextFreeNote,
  type LaunchBinding,
  type LaunchTarget,
} from "../utils/launch-map";

class LaunchStore {
  panelOpen = $state(false);
  bindings = $state<LaunchBinding[]>([]);
  driveClipIds = $state<string[]>([]);
  selectedId = $state<string | null>(null);
  learningId = $state<string | null>(null);
  /** Draw a new region on the timeline; clips stop capturing the pointer. */
  marking = $state(false);
  error = $state<string | null>(null);

  get selected(): LaunchBinding | null {
    return this.bindings.find((b) => b.id === this.selectedId) ?? null;
  }

  get learning(): boolean {
    return this.learningId !== null;
  }

  togglePanel() {
    this.panelOpen = !this.panelOpen;
    if (!this.panelOpen) {
      this.stopLearn();
      this.marking = false;
    }
  }

  closePanel() {
    this.panelOpen = false;
    this.marking = false;
    this.stopLearn();
  }

  toggleMarking() {
    this.marking = !this.marking;
    if (this.marking) this.panelOpen = true;
  }

  stopLearn() {
    this.learningId = null;
    void backend.launchLearnArm?.(null);
  }

  async init() {
    await this.reload();
    backend.on?.("launch://changed", (snap) => {
      this.accept(snap);
    });
    // Undo/open go through project://changed, not launch://changed.
    backend.on?.("project://changed", () => {
      void this.reload();
    });
  }

  private accept(snap: { bindings: LaunchBinding[]; driveClipIds: string[] }) {
    this.bindings = snap.bindings;
    this.driveClipIds = snap.driveClipIds;
  }

  drives(clipId: string): boolean {
    return this.driveClipIds.includes(clipId);
  }

  async toggleDrive(clipId: string) {
    const on = !this.drives(clipId);
    this.driveClipIds = on
      ? [...this.driveClipIds, clipId]
      : this.driveClipIds.filter((id) => id !== clipId);
    if (!backend.launchSetDrive) return;
    try {
      this.accept(await backend.launchSetDrive(clipId, on));
    } catch (err) {
      this.error = String(err);
    }
  }

  async reload() {
    if (!backend.launchGet) return;
    try {
      this.accept(await backend.launchGet());
    } catch (err) {
      this.error = String(err);
    }
  }

  focus(id: string) {
    const b = this.bindings.find((x) => x.id === id);
    if (!b) return;
    this.selectedId = id;
    const samples = bindingFocusSamples(b, midi.clips, (t) => midi.ticksToSamples(t));
    if (samples == null) return;
    const pad = view.width * view.spp * 0.15;
    view.scrollToSamples(Math.max(0, samples - pad));
    void transport.seek(samples);
  }

  async create(target: LaunchTarget, name?: string): Promise<LaunchBinding> {
    const binding: LaunchBinding = {
      id: crypto.randomUUID(),
      name: name ?? nextBindingName(this.bindings),
      note: nextFreeNote(this.bindings),
      channel: null,
      target,
    };
    this.bindings = [...this.bindings, binding];
    this.selectedId = binding.id;
    await this.persist(binding);
    return binding;
  }

  async createRegion(startTicks: number, lengthTicks: number, trackIds: string[]) {
    if (lengthTicks < 1 || trackIds.length === 0) return null;
    return this.create({ kind: "region", startTicks, lengthTicks, trackIds });
  }

  async mapClip(clipId: string, name?: string): Promise<LaunchBinding | null> {
    const clip = midi.clipById(clipId);
    if (!clip) return null;
    const existing = this.bindings.find((b) => b.target.kind === "clip" && b.target.clipId === clipId);
    if (existing) {
      this.selectedId = existing.id;
      this.focus(existing.id);
      return existing;
    }
    return this.create({ kind: "clip", clipId }, name ?? clip.name);
  }

  clipSelfTriggers(binding: LaunchBinding): boolean {
    if (binding.target.kind !== "clip") return false;
    const clip = midi.clipById(binding.target.clipId);
    if (!clip) return false;
    return clipWouldSelfTrigger(clip.notes, binding.note, binding.channel);
  }

  async update(id: string, patch: Partial<Pick<LaunchBinding, "name" | "note" | "channel" | "target">>) {
    const i = this.bindings.findIndex((b) => b.id === id);
    if (i < 0) return;
    const next = { ...this.bindings[i], ...patch };
    this.bindings = this.bindings.map((b) => (b.id === id ? next : b));
    await this.persist(next);
  }

  async remove(id: string) {
    const gone = this.bindings.find((b) => b.id === id);
    this.bindings = this.bindings.filter((b) => b.id !== id);
    if (this.selectedId === id) this.selectedId = null;
    if (this.learningId === id) this.stopLearn();
    if (gone && backend.launchSet) {
      try {
        await backend.launchSet(null, id);
      } catch (err) {
        this.error = String(err);
      }
    }
  }

  beginLearn(id: string) {
    if (this.learningId === id) {
      this.stopLearn();
      return;
    }
    this.learningId = id;
    this.selectedId = id;
    void backend.launchLearnArm?.(id);
    this.pollLearn();
  }

  async applyLearn(note: number, channel: number | null) {
    const id = this.learningId;
    if (!id) return;
    this.stopLearn();
    await this.update(id, { note, channel });
  }

  /** In-memory only — used while a timeline handle is dragged. */
  patchLocal(id: string, patch: Partial<Pick<LaunchBinding, "name" | "note" | "channel" | "target">>) {
    const i = this.bindings.findIndex((b) => b.id === id);
    if (i < 0) return;
    const next = { ...this.bindings[i], ...patch };
    this.bindings = this.bindings.map((b) => (b.id === id ? next : b));
  }

  private pollLearn() {
    const armed = this.learningId;
    const tick = async () => {
      if (!this.learningId || this.learningId !== armed) return;
      try {
        const got = await backend.launchLearnTake?.();
        if (got && this.learningId === armed) {
          await this.applyLearn(got.note, got.channel);
          return;
        }
      } catch {
        /* demo / old backend */
      }
      if (this.learningId === armed) setTimeout(tick, 80);
    };
    void tick();
  }

  private async persist(binding: LaunchBinding) {
    if (!backend.launchSet) return;
    try {
      this.accept(await backend.launchSet(binding));
    } catch (err) {
      this.error = String(err);
    }
  }
}

export const launch = new LaunchStore();
