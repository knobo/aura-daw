/**
 * ZynAddSubFX patch bank browser state: the 1318-patch .xiz bank list
 * (zyn_list_patches), per-instance loads (zyn_load_patch), and the backend
 * caveat handled in one place: a loaded patch reaches only FUTURE nodes, so
 * every successful load re-binds the owning track (set_track_instrument →
 * engine Rebuild) to make the change audible without a restart.
 */

import { backend } from "../tauri";
import { plugins } from "./plugins.svelte";
import { project } from "./project.svelte";
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
  /** Patch path currently loaded per instance (frontend mirror). */
  loaded = $state<Record<string, string>>({});
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

  /** The bound midi track of an instance (registry ref or track mirror). */
  private boundTrackId(instanceId: string): string | null {
    const inst = plugins.byId(instanceId);
    if (inst?.trackId) return inst.trackId;
    const ref = `plugin:${instanceId}`;
    return project.tracks.find((t) => t.instrumentId === ref)?.id ?? null;
  }

  /**
   * Load a patch into a Zyn instance, then re-bind its track so the engine
   * rebuilds the live node with the new sound (backend caveat: patches reach
   * only future nodes until node invalidation ships).
   */
  async load(instanceId: string, patch: ZynPatch): Promise<boolean> {
    this.busyPath = patch.path;
    this.error = null;
    try {
      await backend.zynLoadPatch(instanceId, patch.path);
      this.loaded = { ...this.loaded, [instanceId]: patch.path };
      const trackId = this.boundTrackId(instanceId);
      if (trackId) {
        // Rebind → graph Rebuild → the patched instance is what renders.
        await plugins.bind(instanceId, trackId);
      }
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
      this.loaded = { ...this.loaded, [inst.id]: pick.path };
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
