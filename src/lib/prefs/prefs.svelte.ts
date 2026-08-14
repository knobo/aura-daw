/**
 * The live preferences store: schema defaults overlaid with whatever valid
 * values survive on disk, kept reactive ($state) so components track them
 * directly. All mutation funnels through set(), which validates via the
 * schema, writes through to the aura.prefs blob, and notifies onChange
 * subscribers — the hook for preferences that need a side effect beyond
 * reactivity (e.g. pushing the MCP mode to the agent server).
 */

import { readPref, writePref } from "../utils/prefs";
import { PREF_SCHEMA, coercePref, schemaDefaults, type PrefId, type PrefValues } from "./schema";

type Listener<K extends PrefId> = (value: PrefValues[K]) => void;

class PrefsStore {
  values = $state<PrefValues>(schemaDefaults());
  /** Whether the preferences dialog is open. */
  dialogOpen = $state(false);

  private listeners = new Map<PrefId, Set<Listener<PrefId>>>();

  /** Overlay persisted values at boot. Junk on disk falls back to defaults. */
  init() {
    for (const id of Object.keys(PREF_SCHEMA) as PrefId[]) {
      const restored = coercePref(PREF_SCHEMA[id], readPref(id));
      if (restored !== undefined) (this.values as Record<PrefId, unknown>)[id] = restored;
    }
  }

  /** Validate, apply, persist, notify. Invalid values are ignored whole. */
  set<K extends PrefId>(id: K, value: unknown) {
    const next = coercePref(PREF_SCHEMA[id], value) as PrefValues[K] | undefined;
    if (next === undefined || next === this.values[id]) return;
    this.values[id] = next;
    writePref(id, next);
    for (const fn of this.listeners.get(id) ?? []) fn(next);
  }

  /** Subscribe to changes of one preference; returns the unsubscriber. */
  onChange<K extends PrefId>(id: K, fn: Listener<K>): () => void {
    const set = this.listeners.get(id) ?? new Set();
    set.add(fn as Listener<PrefId>);
    this.listeners.set(id, set);
    return () => set.delete(fn as Listener<PrefId>);
  }

  /** Back to factory settings (persisted). The dialog's RESET ALL. */
  restoreDefaults() {
    for (const [id, value] of Object.entries(schemaDefaults()) as [PrefId, PrefValues[PrefId]][]) {
      this.set(id, value);
    }
  }
}

export const prefs = new PrefsStore();
