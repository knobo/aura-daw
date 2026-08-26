/**
 * Virtual control surface: layout chrome plus command emission.
 *
 * Authoritative mix/launch/plugin state stays in those stores. This store
 * owns the faceplate arrangement, hydrates it from localStorage, and turns
 * widget gestures into the same commands TrackHeader and LaunchMap already
 * use. Thin renderer (ADR 0006, ruling S-1).
 */

import { backend } from "../tauri";
import type { TargetRef } from "../types/ipc";
import {
  activePage,
  addRecipe,
  bindOptions,
  cellOptions,
  addStrip,
  addWidget,
  applyTemplate,
  bindWidget,
  emptyLayout,
  parseLayout,
  removeGroup,
  removeWidget,
  setGridCell,
  setPadMode,
  storageKey,
  unboundWidget,
  type AddMenuId,
  type AddRecipe,
  type BindOption,
  type LampRole,
  type PadMode,
  type SurfaceAutomation,
  type SurfaceContext,
  type SurfaceLayout,
  type SurfacePluginParam,
  type SurfaceTarget,
  type SurfaceTemplateId,
  type SurfaceWidgetKind,
} from "../utils/control-surface";
import { buildMatrix } from "../utils/automation-matrix";
import { launch } from "./launch.svelte";
import { midi } from "./midi.svelte";
import { plugins } from "./plugins.svelte";
import { project } from "./project.svelte";
import { modulation } from "./modulation.svelte";

const GAIN_MIN = -60;
const GAIN_MAX = 12;

class SurfaceStore {
  layout = $state<SurfaceLayout>(emptyLayout());
  /** Remove-buttons and the bind picker. Empty decks force this on. */
  editMode = $state(true);
  addOpen = $state(false);
  /** Local latch for toggle pads that are not currently the launch overlay. */
  latch = $state<Record<string, boolean>>({});
  /** Widget whose bind picker is open, if any. */
  bindFor = $state<string | null>(null);

  private persistTimer: ReturnType<typeof setTimeout> | null = null;
  private hydratedKey: string | null = null;
  private paramPending = new Map<string, { instanceId: string; paramId: number; value: number }>();
  private paramRaf: number | null = null;
  private gestureToken: Promise<string | undefined> | undefined;
  private gestureTail: Promise<void> = Promise.resolve();

  get page() {
    return activePage(this.layout);
  }

  context(): SurfaceContext {
    const tracks = project.tracks.map((t) => ({
      id: t.id,
      name: t.name,
      kind: t.kind,
      color: t.color,
    }));
    const midiClips = midi.clips.map((c) => ({
      id: c.id,
      name: c.name,
      trackId: c.trackId,
    }));
    const rows = buildMatrix({
      bindings: modulation.bindings,
      instances: plugins.instances,
      tracks: project.tracks,
      visible: modulation.visible,
      paramInfo: (instanceId, paramId) =>
        plugins.paramCache[instanceId]?.find((p) => p.id === paramId),
    });
    const automations: SurfaceAutomation[] = [];
    for (const r of rows) {
      const target = targetFromMatrix(r.target);
      if (!target) continue;
      automations.push({
        id: r.bindingId,
        trackId: r.trackId,
        trackName: r.trackName,
        paramLabel: r.paramLabel,
        target,
      });
    }
    const pluginParams: SurfacePluginParam[] = [];
    for (const inst of plugins.instances) {
      for (const p of plugins.paramCache[inst.id] ?? []) {
        pluginParams.push({
          instanceId: inst.id,
          instanceName: inst.name,
          paramId: p.id,
          paramName: p.name,
        });
      }
    }
    return { tracks, midiClips, automations, pluginParams };
  }

  hydrate(projectDir: string | null) {
    const key = storageKey(projectDir);
    if (this.hydratedKey === key) return;
    // A debounced write still belongs to the deck we are leaving: land it
    // under the key it was edited in, before the swap moves the target.
    this.flushPersist();
    this.hydratedKey = key;
    try {
      const raw = localStorage.getItem(key);
      const parsed = raw ? parseLayout(JSON.parse(raw)) : null;
      this.layout = parsed ?? emptyLayout();
    } catch {
      this.layout = emptyLayout();
    }
    this.editMode = activePage(this.layout).widgets.length === 0;
  }

  /**
   * Debounced localStorage write, always keyed on `hydratedKey` — the key the
   * layout was actually loaded from, never on the live `project.projectDir`:
   * a timer that fires after a project switch would otherwise write the
   * previous deck into the new project's key.
   */
  private persist() {
    if (this.persistTimer) clearTimeout(this.persistTimer);
    const key = this.hydratedKey ?? storageKey(project.projectDir);
    this.persistTimer = setTimeout(() => {
      this.persistTimer = null;
      writeLayout(key, this.layout);
    }, 80);
  }

  /** Land a pending debounced layout now, under the key it was edited in. */
  private flushPersist() {
    if (!this.persistTimer) return;
    clearTimeout(this.persistTimer);
    this.persistTimer = null;
    if (this.hydratedKey) writeLayout(this.hydratedKey, this.layout);
  }

  private commit(next: SurfaceLayout) {
    this.layout = next;
    this.persist();
  }

  choose(id: AddMenuId) {
    this.addOpen = false;
    const ctx = this.context();
    if (id.startsWith("recipe:")) {
      this.commit(addRecipe(this.layout, id.slice("recipe:".length) as AddRecipe, ctx));
      this.editMode = true;
      return;
    }
    if (id.startsWith("template:")) {
      this.commit(applyTemplate(this.layout, id.slice("template:".length) as SurfaceTemplateId, ctx));
      this.editMode = id === "template:blank";
      return;
    }
    if (id === "strip") {
      const first = ctx.tracks.find((t) => t.kind !== "automation");
      if (!first) return;
      this.commit(addStrip(this.layout, first));
      this.editMode = true;
      return;
    }
    this.commit(addWidget(this.layout, unboundWidget(id as SurfaceWidgetKind)));
    this.editMode = true;
  }

  remove(widgetId: string) {
    if (this.bindFor === widgetId) this.bindFor = null;
    this.commit(removeWidget(this.layout, widgetId));
  }

  removeStrip(groupId: string) {
    this.commit(removeGroup(this.layout, groupId));
  }

  bind(widgetId: string, target: SurfaceTarget | null, label?: string, extra?: { lampRole?: LampRole }) {
    this.commit(bindWidget(this.layout, widgetId, target, label, extra));
    this.bindFor = null;
  }

  setEdit(on: boolean) {
    this.editMode = on;
    if (!on) this.bindFor = null;
  }

  /** Open the bind picker for one widget; the same button closes it. */
  toggleBind(widgetId: string) {
    this.bindFor = this.bindFor === widgetId ? null : widgetId;
  }

  options(kind: SurfaceWidgetKind): BindOption[] {
    return bindOptions(kind, this.context());
  }

  /** Picker key for a pad-grid cell: one grid has many independent slots. */
  cellKey(widgetId: string, index: number): string {
    return `${widgetId}#${index}`;
  }

  cellChoices(): BindOption[] {
    return cellOptions(this.context());
  }

  bindCell(widgetId: string, index: number, clipId: string | null) {
    this.commit(setGridCell(this.layout, widgetId, index, clipId));
    this.bindFor = null;
  }

  setMode(widgetId: string, mode: PadMode) {
    this.commit(setPadMode(this.layout, widgetId, mode));
  }

  /**
   * Serialize one widget write behind everything already queued for this
   * gesture. Same contract as TrackHeader: a pointermove cannot land a mix
   * op before `gesture_begin` has returned, and `closeGesture` waits on the
   * same tail, so the trailing write lands before `gesture_end`. Without it
   * one drag becomes several undo entries.
   */
  private queueGestureWrite(write: () => Promise<unknown>) {
    this.gestureTail = settle(this.gestureTail.then(write));
  }

  openGesture(label: string) {
    this.gestureToken = this.gestureTail.then(() => project.beginGesture(label));
    this.gestureTail = settle(this.gestureToken);
  }

  /**
   * Close the boundary: cancel the pending param rAF, put its flush on the
   * tail, and only then `gesture_end`. The order is load-bearing (I-8) — a
   * batch that reaches the backend after `gesture_end` gets its own undo
   * entry and its own project.json write, the two things the gesture exists
   * to collapse.
   */
  closeGesture() {
    const idp = this.gestureToken;
    this.gestureToken = undefined;
    if (this.paramRaf != null) {
      cancelAnimationFrame(this.paramRaf);
      this.paramRaf = null;
    }
    if (this.paramPending.size > 0) this.queueGestureWrite(() => this.flushParams());
    if (!idp) return;
    this.gestureTail = settle(
      this.gestureTail.then(async () => {
        const id = await idp;
        await project.endGesture(id);
      }),
    );
  }

  writeGain(trackId: string, gainDb: number) {
    this.queueGestureWrite(() => project.setGain(trackId, clamp(gainDb, GAIN_MIN, GAIN_MAX)));
  }

  writePan(trackId: string, pan: number) {
    this.queueGestureWrite(() => project.setPan(trackId, clamp(pan, -1, 1)));
  }

  async writeMute(trackId: string, muted: boolean) {
    await project.setMute(trackId, muted);
  }

  async writeSolo(trackId: string, soloed: boolean) {
    await project.setSolo(trackId, soloed);
  }

  async writeArm(trackId: string, armed: boolean) {
    await project.setArmed(trackId, armed);
  }

  writePluginParam(instanceId: string, paramId: number, value: number) {
    const key = `${instanceId}:${paramId}`;
    this.paramPending.set(key, { instanceId, paramId, value });
    const cached = plugins.paramCache[instanceId];
    if (cached) {
      plugins.paramCache = {
        ...plugins.paramCache,
        [instanceId]: cached.map((p) => (p.id === paramId ? { ...p, value } : p)),
      };
    }
    if (this.paramRaf == null) {
      this.paramRaf = requestAnimationFrame(() => {
        this.paramRaf = null;
        this.queueGestureWrite(() => this.flushParams());
      });
    }
  }

  private async flushParams() {
    const batch = [...this.paramPending.values()];
    this.paramPending.clear();
    const byInst = new Map<string, { id: number; value: number }[]>();
    for (const p of batch) {
      const list = byInst.get(p.instanceId) ?? [];
      list.push({ id: p.paramId, value: p.value });
      byInst.set(p.instanceId, list);
    }
    for (const [instanceId, changes] of byInst) {
      try {
        await backend.pluginSetParam(instanceId, changes);
      } catch (err) {
        console.warn("[aura] surface plugin_set_param failed:", err);
      }
    }
  }

  async fireClip(clipId: string) {
    const existing = launch.bindings.find((b) => b.target.kind === "clip" && b.target.clipId === clipId);
    const binding = existing ?? (await launch.mapClip(clipId));
    if (!binding) return;
    await launch.preview(binding.id);
  }

  async fireBinding(bindingId: string) {
    await launch.preview(bindingId);
  }

  async toggleDrive(clipId: string) {
    await launch.toggleDrive(clipId);
  }

  isClipPlaying(clipId: string): boolean {
    const overlay = launch.overlay;
    if (!overlay) return false;
    const b = launch.bindings.find((x) => x.id === overlay.id);
    return b?.target.kind === "clip" && b.target.clipId === clipId;
  }

  toggleLatch(widgetId: string): boolean {
    const next = !this.latch[widgetId];
    this.latch = { ...this.latch, [widgetId]: next };
    return next;
  }

  latched(widgetId: string): boolean {
    return !!this.latch[widgetId];
  }
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

/** A gesture tail must never reject: one failed write cannot be allowed to
 * strand the queue and leave `gesture_end` unsent. */
function settle(operation: Promise<unknown>): Promise<void> {
  return operation.then(
    () => undefined,
    () => undefined,
  );
}

function writeLayout(key: string, layout: SurfaceLayout) {
  try {
    localStorage.setItem(key, JSON.stringify(layout));
  } catch {
    /* quota / private mode — the deck still works this session */
  }
}

function targetFromMatrix(target: TargetRef): SurfaceTarget | null {
  if (target.kind === "trackParam" && target.param === "gain") {
    return { kind: "trackGain", trackId: target.trackId };
  }
  if (target.kind === "trackParam" && target.param === "pan") {
    return { kind: "trackPan", trackId: target.trackId };
  }
  if (target.kind === "pluginParam") {
    return { kind: "pluginParam", instanceId: target.instanceId, paramId: target.paramId };
  }
  return null;
}

export const surface = new SurfaceStore();
export { GAIN_MIN, GAIN_MAX };
