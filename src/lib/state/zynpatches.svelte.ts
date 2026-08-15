/**
 * ZynAddSubFX patch bank browser state: the 1318-patch .xiz bank list
 * (zyn_list_patches) and per-instance loads (zyn_load_patch).
 *
 * A patch reaches an LV2 instance only when the instance is instantiated, so
 * a load has to retire the track's live node. That is the ENGINE's job now:
 * `Op::PluginSetState` bumps the instance's state revision, which the live-node
 * key carries, so the commit's own rebuild instantiates the replacement. This
 * store used to re-bind the owning track to force that rebuild — a workaround
 * that never worked (the rebind produced an identical node key, so the cached
 * node was reused) and cost a second undo entry per patch load.
 */

import { backend } from "../tauri";
import { plugins } from "./plugins.svelte";
import { toasts } from "./toasts.svelte";
import type { PluginInstanceInfo, ZynPatch } from "../types/ipc";

/** Is this instance a ZynAddSubFX (the only bank-patch host)? */
export function isZynInstance(inst: PluginInstanceInfo | undefined | null): boolean {
  return !!inst && inst.uid.toLowerCase().includes("zyn");
}

class ZynPatchStore {
  /** Instance whose patch browser is open ("" = closed). */
  openInstanceId = $state("");
  patches = $state<ZynPatch[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);
  /** Patch currently loaded per instance (frontend mirror). */
  loaded = $state<Record<string, ZynPatch>>({});
  /** Patch path with a load in flight. */
  busyPath = $state<string | null>(null);

  get openInstance(): PluginInstanceInfo | undefined {
    return plugins.byId(this.openInstanceId);
  }

  async openFor(instanceId: string) {
    this.openInstanceId = instanceId;
    this.error = null;
    if (this.patches.length === 0) await this.refresh();
  }

  close() {
    this.openInstanceId = "";
    this.error = null;
  }

  async refresh() {
    this.loading = true;
    try {
      this.patches = await backend.zynListPatches();
      this.error = null;
    } catch (err) {
      this.error = String(err);
    } finally {
      this.loading = false;
    }
  }

  /**
   * Load a patch into a Zyn instance. `zyn_load_patch` commits the state,
   * which retires the instance's live node and rebuilds it — no rebind here.
   */
  async load(instanceId: string, patch: ZynPatch): Promise<boolean> {
    this.busyPath = patch.path;
    this.error = null;
    try {
      await backend.zynLoadPatch(instanceId, patch.path);
      this.loaded = { ...this.loaded, [instanceId]: patch };
      return true;
    } catch (err) {
      this.error = String(err);
      return false;
    } finally {
      this.busyPath = null;
    }
  }

  // ── hum-to-song suggestion: "give the melody a Zyn plucked/lead voice" ──

  /** A Zyn instrument is installed (scan knows it) or already instantiated. */
  get zynAvailable(): boolean {
    return (
      plugins.descriptors.some((d) => d.uid.toLowerCase().includes("zyn") && d.isInstrument) ||
      plugins.instances.some((i) => isZynInstance(i))
    );
  }

  /**
   * One-click: (re)use or instantiate a Zyn instance, load a plucked/lead
   * patch, bind it to `trackId`. Returns the patch name on success.
   */
  async bindZynVoice(trackId: string): Promise<string | null> {
    try {
      // Prefer an instance not bound elsewhere; else make a fresh one.
      let inst =
        plugins.instances.find((i) => isZynInstance(i) && (!i.trackId || i.trackId === trackId)) ??
        null;
      if (!inst) {
        const desc = plugins.descriptors.find(
          (d) => d.uid.toLowerCase().includes("zyn") && d.isInstrument,
        );
        if (!desc) {
          toasts.info("NO ZYN FOUND", "scan plugins first — ZynAddSubFX provides the patch bank");
          return null;
        }
        inst = await plugins.instantiate(desc.uid, null);
        if (!inst) {
          toasts.error("ZYN BIND FAILED", plugins.error ?? "instantiate failed");
          return null;
        }
        // Give the host thread a beat to activate the DSP.
        await new Promise((r) => setTimeout(r, 1500));
      }
      if (this.patches.length === 0) await this.refresh();
      const pick =
        this.patches.find((p) => /pluck/i.test(p.bank) || /pluck/i.test(p.name)) ??
        this.patches.find((p) => /lead/i.test(p.bank) || /lead/i.test(p.name)) ??
        this.patches[0];
      if (!pick) {
        toasts.error("ZYN BIND FAILED", this.error ?? "no patches found in the Zyn banks");
        return null;
      }
      await backend.zynLoadPatch(inst.id, pick.path);
      this.loaded = { ...this.loaded, [inst.id]: pick };
      await plugins.bind(inst.id, trackId); // bind AFTER load ⇒ node sees the patch
      if (plugins.error) {
        toasts.error("ZYN BIND FAILED", plugins.error);
        return null;
      }
      toasts.success("ZYN VOICE BOUND", `${pick.bank} / ${pick.name} → melody track`);
      return pick.name;
    } catch (err) {
      toasts.error("ZYN BIND FAILED", String(err));
      return null;
    }
  }
}

export const zyn = new ZynPatchStore();
