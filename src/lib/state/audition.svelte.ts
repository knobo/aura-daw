/**
 * Unified browser audition (design §8.2, Step 7).
 *
 * One gesture — double-click a row — reaches every browser, and one store
 * decides what that means. The store is deliberately thin: it owns the
 * gate, the "what is sounding right now" highlight, and the dispatch from
 * an `AuditionTarget` to whichever preview command already exists. It owns
 * no authoritative state (ADR 0006) and emits no ops.
 *
 * Two invariants, both tested:
 *
 * 1. **The preference gates everything.** `browserAudition` defaults off,
 *    because a browser that makes noise you did not ask for is the fastest
 *    way to lose someone. `enabled` reads and writes that pref directly
 *    rather than shadowing it in a second session-level mute — one truth,
 *    so the toolbar chip and the Preferences dialog can never disagree
 *    (ruling R-2).
 * 2. **An audition is never a project edit.** Every command below is a
 *    document-decoupled preview path: no op, no dirty flag, no undo entry.
 *    A target that could only be reached by editing resolves to `silent`
 *    upstream, in `utils/audition-target.ts`.
 */

import { backend } from "../tauri";
import { prefs } from "../prefs/prefs.svelte";
import type { AuditionTarget } from "../utils/audition-target";

/** How long a row stays lit after it was auditioned. Long enough to see
 * which row answered, short enough not to look like a selection. */
export const AUDITION_DECAY_MS = 1200;

const AUDITION_VELOCITY = 100;

class AuditionStore {
  /** Opaque id of what last sounded — sample path, instrument id, or
   * instance id. Rows compare their own id against it to light up. */
  sounding = $state<string | null>(null);
  /** Why the last attempt made no sound, when it made none. A UI hint, not
   * an error state: "no live instance of this plugin to audition" is a
   * perfectly ordinary answer. */
  lastSilentReason = $state<string | null>(null);

  /** True while a sample is on the preview stream, so `stop` knows whether
   * `library_audition_stop` has anything to release. */
  #sampleSounding = false;
  #decay: ReturnType<typeof setTimeout> | null = null;

  get enabled(): boolean {
    return prefs.values.browserAudition;
  }
  set enabled(on: boolean) {
    // `prefs.set`, never `prefs.values.x = on`: only `set` validates,
    // writes through to the `aura.prefs` blob and notifies `onChange`
    // subscribers. A direct field write is reactive but does not persist,
    // so the toolbar chip would silently forget itself on restart.
    prefs.set("browserAudition", on);
  }

  async play(target: AuditionTarget): Promise<void> {
    if (!this.enabled) return;
    if (target.kind === "silent") {
      this.lastSilentReason = target.reason;
      return;
    }
    // Whatever was sounding gets cut first: two overlapping auditions tell
    // you nothing about either one.
    await this.stop();
    this.lastSilentReason = null;
    try {
      switch (target.kind) {
        case "sample":
          await backend.libraryAudition?.(target.path);
          this.#sampleSounding = true;
          this.#light(target.path);
          break;
        case "instrument":
          await backend.samplerPreviewNote(target.instrumentId, target.key, AUDITION_VELOCITY);
          this.#light(target.instrumentId);
          break;
        case "pluginInstance":
          await backend.pluginPreviewNote(target.instanceId, target.key, AUDITION_VELOCITY);
          this.#light(target.instanceId);
          break;
      }
    } catch (err) {
      // A preview that cannot sound is a hint, never a thrown error that
      // takes a click handler down with it.
      this.sounding = null;
      this.#sampleSounding = false;
      this.lastSilentReason = String(err);
    }
  }

  async stop(): Promise<void> {
    if (this.#decay) {
      clearTimeout(this.#decay);
      this.#decay = null;
    }
    this.sounding = null;
    if (!this.#sampleSounding) return;
    this.#sampleSounding = false;
    try {
      await backend.libraryAuditionStop?.();
    } catch {
      /* nothing was sounding — the preview stream is lazy */
    }
  }

  #light(id: string) {
    this.sounding = id;
    if (this.#decay) clearTimeout(this.#decay);
    this.#decay = setTimeout(() => {
      this.#decay = null;
      if (this.sounding === id) this.sounding = null;
    }, AUDITION_DECAY_MS);
  }
}

export const audition = new AuditionStore();
