/**
 * Plugin host mirror (phase 3): scanned CLAP/LV2 descriptors, live instance
 * registry, and the generic-parameter bridge. Built purely against the six
 * frozen commands (plugin_scan/list/instantiate/remove/get_params/set_param)
 * plus set_track_instrument's additive "plugin:<instanceId>" refs.
 *
 * Param writes follow the D-03 batching rule: knob drags coalesce into ONE
 * plugin_set_param invoke per animation frame (a changes[] array), never one
 * invoke per input event.
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import type {
  PluginDescriptor,
  PluginInstanceInfo,
  PluginParamChange,
  PluginParamInfo,
} from "../types/ipc";

class PluginsStore {
  /** Last scan result (empty until a scan ran). */
  descriptors = $state<PluginDescriptor[]>([]);
  /** True once the backend has a cached scan. */
  scanned = $state(false);
  scanning = $state(false);
  scanError = $state<string | null>(null);

  /** Live instances (keyed list, delta-applied — no full-list polling loops). */
  instances = $state<PluginInstanceInfo[]>([]);
  /** Structured per-operation error (instantiate/remove/bind), shown in the browser. */
  error = $state<string | null>(null);
  /** Instance id whose parameter panel is open ("" = closed). */
  openInstanceId = $state("");
  /** Params of the open instance. */
  params = $state<PluginParamInfo[]>([]);
  paramsLoading = $state(false);
  paramError = $state<string | null>(null);

  /** uid of the instantiate in flight (row spinner). */
  busyUid = $state<string | null>(null);

  byId(instanceId: string | null | undefined): PluginInstanceInfo | undefined {
    return instanceId ? this.instances.find((i) => i.id === instanceId) : undefined;
  }

  /** Resolve a track's instrumentId ("plugin:<id>") to its instance, if any. */
  instanceForRef(instrumentId: string | null | undefined): PluginInstanceInfo | undefined {
    if (!instrumentId?.startsWith("plugin:")) return undefined;
    return this.byId(instrumentId.slice("plugin:".length));
  }

  get openInstance(): PluginInstanceInfo | undefined {
    return this.byId(this.openInstanceId);
  }

  private applyInstances(list: PluginInstanceInfo[]) {
    this.instances = list;
    if (this.openInstanceId && !list.some((i) => i.id === this.openInstanceId)) {
      this.closeParams();
    }
  }

  async refresh() {
    try {
      const res = await backend.pluginList();
      this.descriptors = res.plugins;
      this.scanned = res.scanned;
      this.applyInstances(res.instances);
    } catch (err) {
      // plugin_list absent = pre-phase-3 engine; stay quietly empty.
      console.warn("[aura] plugin_list failed:", err);
    }
  }

  async scan() {
    if (this.scanning) return;
    this.scanning = true;
    this.scanError = null;
    try {
      this.descriptors = await backend.pluginScan();
      this.scanned = true;
    } catch (err) {
      this.scanError = String(err);
    } finally {
      this.scanning = false;
    }
  }

  /**
   * Instantiate a plugin and (optionally) bind it straight onto a midi track
   * via set_track_instrument("plugin:<id>").
   */
  async instantiate(uid: string, trackId: string | null = null): Promise<PluginInstanceInfo | null> {
    this.busyUid = uid;
    this.error = null;
    try {
      const inst = await backend.pluginInstantiate(uid);
      this.instances = [...this.instances, inst];
      if (trackId) await this.bind(inst.id, trackId);
      // Status may flip stub → active once the host thread brings the DSP up;
      // re-pull the registry shortly after (single shot, not a poll loop).
      setTimeout(() => void this.refreshInstances(), 1400);
      return inst;
    } catch (err) {
      this.error = String(err);
      return null;
    } finally {
      this.busyUid = null;
    }
  }

  /** Bind an existing instance to a midi track. */
  async bind(instanceId: string, trackId: string) {
    this.error = null;
    try {
      const ref = `plugin:${instanceId}`;
      const track = await backend.setTrackInstrument(trackId, ref);
      project.patchTrackLocal(trackId, { instrumentId: track.instrumentId ?? ref });
      this.instances = this.instances.map((i) =>
        i.id === instanceId
          ? { ...i, trackId }
          : i.trackId === trackId
            ? { ...i, trackId: null }
            : i,
      );
    } catch (err) {
      this.error = String(err);
    }
  }

  async remove(instanceId: string) {
    this.error = null;
    try {
      await backend.pluginRemove(instanceId);
    } catch (err) {
      this.error = String(err);
      // Still clean up locally — the instance may be a zombie that
      // the backend can't find but still shows in the UI.
    }
    // Unbind any track that pointed at the removed instance (the engine
    // falls back to PolySynth for dangling refs; keep the UI truthful).
    const ref = `plugin:${instanceId}`;
    for (const t of project.tracks) {
      if (t.instrumentId === ref) {
        try {
          await backend.setTrackInstrument(t.id, null);
        } catch {
          /* dangling ref clears are best-effort */
        }
        project.patchTrackLocal(t.id, { instrumentId: null });
      }
    }
    this.instances = this.instances.filter((i) => i.id !== instanceId);
    // Also remove from any track's insert slots
    for (const t of project.tracks) {
      const inserts = (t.inserts ?? []).filter((s) => s.instanceId !== instanceId);
      if (inserts.length !== (t.inserts ?? []).length) {
        project.patchTrackLocal(t.id, { inserts });
      }
    }
    if (this.openInstanceId === instanceId) this.closeParams();
  }

  private async refreshInstances() {
    try {
      const res = await backend.pluginList();
      this.applyInstances(res.instances);
    } catch {
      /* transient */
    }
  }

  // ── generic parameter panel ──

  async openParams(instanceId: string) {
    this.openInstanceId = instanceId;
    this.params = [];
    this.paramError = null;
    this.paramsLoading = true;
    try {
      this.params = await backend.pluginGetParams(instanceId);
    } catch (err) {
      this.paramError = String(err);
    } finally {
      this.paramsLoading = false;
    }
  }

  /** Re-pull the OPEN instance's params without closing/reopening the panel
   * (M-3): an undo/redo of a plugin-param step changed the backend's values
   * under a panel that is still showing the pre-undo ones. No-op when no
   * panel is open. */
  async reloadOpenParams(): Promise<void> {
    const id = this.openInstanceId;
    if (!id) return;
    try {
      this.params = await backend.pluginGetParams(id);
      this.paramError = null;
    } catch (err) {
      this.paramError = String(err);
    }
  }

  closeParams() {
    void this.flushParamQueue(); // don't drop trailing knob motion
    this.openInstanceId = "";
    this.params = [];
    this.paramError = null;
  }

  // Batched param writes: one plugin_set_param per rAF, changes coalesced by
  // param id (SCALABILITY §5 transient-coalescing — last write per id wins).
  private pending = new Map<number, number>();
  private rafId: number | null = null;
  private inFlight = false;
  /** Token for the open knob-drag gesture; awaited at end so a late
   * close cannot shut a fader that began while we flushed. */
  private paramGesture: Promise<string | undefined> | undefined;

  /** Optimistic local + queued backend write; called at input-event rate. */
  setParam(paramId: number, value: number) {
    this.params = this.params.map((p) =>
      p.id === paramId ? { ...p, value: Math.min(p.max, Math.max(p.min, value)) } : p,
    );
    this.pending.set(paramId, value);
    if (this.rafId == null) {
      this.rafId = requestAnimationFrame(() => {
        this.rafId = null;
        void this.flushParamQueue();
      });
    }
  }

  resetParam(param: PluginParamInfo) {
    this.setParam(param.id, param.default);
  }

  /** Open a knob-drag gesture boundary — call on `pointerdown` of a param
   * fader, before the first `setParam` of the drag (I-8). */
  beginParamGesture() {
    this.paramGesture = project.beginGesture("plugin param drag");
  }

  /** Close a knob-drag gesture: cancel the pending rAF, flush whatever is
   * queued, WAIT for it to land, and only then close the boundary. The
   * order is load-bearing (I-8): a batch that reaches the backend after
   * `gesture_end` gets its own undo entry and its own project.json write —
   * the two things the gesture exists to collapse. */
  async endParamGesture(): Promise<void> {
    const idp = this.paramGesture;
    this.paramGesture = undefined;
    if (this.rafId != null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    await this.flushParamQueue();
    const id = await idp;
    if (id) project.endGesture(id);
  }

  private flushParamQueue(): Promise<void> {
    if (this.pending.size === 0 || !this.openInstanceId) return Promise.resolve();
    if (this.inFlight) {
      // A batch is on the wire; the rAF after it lands picks these up.
      return new Promise<void>((resolve) => {
        if (this.rafId != null) cancelAnimationFrame(this.rafId);
        this.rafId = requestAnimationFrame(() => {
          this.rafId = null;
          void this.flushParamQueue().then(resolve);
        });
      });
    }
    const instanceId = this.openInstanceId;
    const changes: PluginParamChange[] = [...this.pending.entries()].map(([id, value]) => ({
      id,
      value,
    }));
    this.pending.clear();
    this.inFlight = true;
    return backend
      .pluginSetParam(instanceId, changes)
      .then(() => {
        this.paramError = null;
      })
      .catch((err) => {
        this.paramError = String(err);
      })
      .finally(() => {
        this.inFlight = false;
      });
  }
}

export const plugins = new PluginsStore();
