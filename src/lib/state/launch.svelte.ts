/**
 * MIDI launchers: named note→region/clip maps, plus which clips drive
 * each map. Mutations go through `launch_set` / `launch_set_map` when
 * the backend has them; demo mode keeps the list in memory.
 */

import { backend } from "../tauri";
import { midi } from "./midi.svelte";
import { transport } from "./transport.svelte";
import { view } from "./view.svelte";
import type { LaunchFired } from "../types/ipc";
import {
  bindingFocusSamples,
  clipWouldSelfTrigger,
  defaultLaunchMap,
  driveMapId,
  nextBindingName,
  nextFreeNote,
  nextLauncherName,
  type LaunchBinding,
  type LaunchMap,
  type LaunchTarget,
} from "../utils/launch-map";

class LaunchStore {
  panelOpen = $state(false);
  maps = $state<LaunchMap[]>([defaultLaunchMap()]);
  activeMapId = $state(defaultLaunchMap().id);
  selectedId = $state<string | null>(null);
  learningId = $state<string | null>(null);
  /** Draw a new region on the timeline; clips stop capturing the pointer. */
  marking = $state(false);
  /** Drive-clip scene currently sounding on the shadow playhead. */
  overlay = $state<{ id: string; name: string } | null>(null);
  error = $state<string | null>(null);

  get activeMap(): LaunchMap {
    return this.maps.find((m) => m.id === this.activeMapId) ?? this.maps[0] ?? defaultLaunchMap();
  }

  get bindings(): LaunchBinding[] {
    return this.activeMap.bindings;
  }

  get selected(): LaunchBinding | null {
    return this.bindings.find((b) => b.id === this.selectedId) ?? null;
  }

  get learning(): boolean {
    return this.learningId !== null;
  }

  selectMap(id: string) {
    if (!this.maps.some((m) => m.id === id)) return;
    this.activeMapId = id;
    this.selectedId = null;
    this.stopLearn();
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
    backend.on?.("launch://fired", (ev: LaunchFired) => {
      if (ev.origin !== "drive") {
        this.overlay = null;
        return;
      }
      if (ev.playing === false) {
        if (this.overlay?.id === ev.id) this.overlay = null;
        return;
      }
      this.overlay = { id: ev.id, name: ev.name };
    });
    backend.on?.("transport://state", (s) => {
      if (s.state === "stopped") this.overlay = null;
    });
    // Undo/open go through project://changed, not launch://changed.
    backend.on?.("project://changed", () => {
      void this.reload();
    });
  }

  private accept(snap: { maps?: LaunchMap[]; bindings?: LaunchBinding[]; driveClipIds?: string[] }) {
    if (snap.maps && snap.maps.length > 0) {
      this.maps = snap.maps;
    } else if (snap.bindings || snap.driveClipIds) {
      const fallback = defaultLaunchMap();
      fallback.bindings = snap.bindings ?? [];
      fallback.driveClipIds = snap.driveClipIds ?? [];
      this.maps = [fallback];
    } else {
      this.maps = [defaultLaunchMap()];
    }
    if (!this.maps.some((m) => m.id === this.activeMapId)) {
      this.activeMapId = this.maps[0].id;
    }
    if (this.selectedId && !this.bindings.some((b) => b.id === this.selectedId)) {
      this.selectedId = null;
    }
  }

  drives(clipId: string): boolean {
    return driveMapId(this.maps, clipId) !== null;
  }

  driveMap(clipId: string): LaunchMap | null {
    const id = driveMapId(this.maps, clipId);
    return id ? (this.maps.find((m) => m.id === id) ?? null) : null;
  }

  /** Assign the clip to `mapId`, or detach it when `mapId` is null. */
  async setClipLauncher(clipId: string, mapId: string | null) {
    const current = driveMapId(this.maps, clipId);
    if (current === mapId || (mapId === null && current === null)) return;
    const target = mapId ?? current;
    if (!target) return;
    this.maps = this.maps.map((m) => ({
      ...m,
      driveClipIds:
        m.id === mapId
          ? m.driveClipIds.includes(clipId)
            ? m.driveClipIds
            : [...m.driveClipIds, clipId]
          : m.driveClipIds.filter((id) => id !== clipId),
    }));
    if (!backend.launchSetDrive) return;
    try {
      this.accept(await backend.launchSetDrive(clipId, mapId !== null, target));
    } catch (err) {
      this.error = String(err);
    }
  }

  async toggleDrive(clipId: string, mapId?: string) {
    const current = driveMapId(this.maps, clipId);
    const target = mapId ?? this.activeMap.id;
    await this.setClipLauncher(clipId, current === target ? null : target);
  }

  async createMap(name?: string): Promise<LaunchMap> {
    const map: LaunchMap = {
      id: crypto.randomUUID(),
      name: name ?? nextLauncherName(this.maps),
      bindings: [],
      driveClipIds: [],
      playMode: "gate",
    };
    this.maps = [...this.maps, map];
    this.activeMapId = map.id;
    this.selectedId = null;
    await this.persistMap(map);
    return map;
  }

  async setPlayMode(mode: "gate" | "oneShot") {
    const map = this.activeMap;
    if ((map.playMode ?? "gate") === mode) return;
    const next = { ...map, playMode: mode };
    this.maps = this.maps.map((m) => (m.id === map.id ? next : m));
    await this.persistMap(next);
  }

  async renameMap(id: string, name: string) {
    const trimmed = name.trim();
    if (!trimmed) return;
    const map = this.maps.find((m) => m.id === id);
    if (!map || map.name === trimmed) return;
    const next = { ...map, name: trimmed };
    this.maps = this.maps.map((m) => (m.id === id ? next : m));
    await this.persistMap(next);
  }

  async removeMap(id: string) {
    if (this.maps.length <= 1) {
      this.error = "cannot delete the last launcher";
      return;
    }
    const gone = this.maps.find((m) => m.id === id);
    if (!gone) return;
    this.maps = this.maps.filter((m) => m.id !== id);
    if (this.activeMapId === id) this.activeMapId = this.maps[0].id;
    if (this.selectedId && !this.bindings.some((b) => b.id === this.selectedId)) {
      this.selectedId = null;
    }
    if (!backend.launchSetMap) return;
    try {
      this.accept(await backend.launchSetMap(id, null));
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
    this.maps = this.maps.map((m) =>
      m.id === this.activeMap.id ? { ...m, bindings: [...m.bindings, binding] } : m,
    );
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
    const current = this.bindings.find((b) => b.id === id);
    if (!current) return;
    const next = { ...current, ...patch };
    this.maps = this.maps.map((m) =>
      m.id === this.activeMap.id ? { ...m, bindings: m.bindings.map((b) => (b.id === id ? next : b)) } : m,
    );
    await this.persist(next);
  }

  async remove(id: string) {
    const gone = this.bindings.find((b) => b.id === id);
    this.maps = this.maps.map((m) =>
      m.id === this.activeMap.id ? { ...m, bindings: m.bindings.filter((b) => b.id !== id) } : m,
    );
    if (this.selectedId === id) this.selectedId = null;
    if (this.learningId === id) this.stopLearn();
    if (gone && backend.launchSet) {
      try {
        this.accept(await backend.launchSet(null, id, this.activeMap.id));
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
    const current = this.bindings.find((b) => b.id === id);
    if (!current) return;
    const next = { ...current, ...patch };
    this.maps = this.maps.map((m) =>
      m.id === this.activeMap.id ? { ...m, bindings: m.bindings.map((b) => (b.id === id ? next : b)) } : m,
    );
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
      this.accept(await backend.launchSet(binding, binding.id, this.activeMap.id));
    } catch (err) {
      this.error = String(err);
    }
  }

  private async persistMap(map: LaunchMap) {
    if (!backend.launchSetMap) return;
    try {
      this.accept(await backend.launchSetMap(map.id, map));
    } catch (err) {
      this.error = String(err);
    }
  }
}

export const launch = new LaunchStore();
