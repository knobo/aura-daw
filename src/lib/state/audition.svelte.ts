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
   * instance id. Rows compare their own id against it to light up.
   *
   * No consumer renders this yet — no row lights up on the new gesture
   * today. The decay timer and the `#gen` supersession logic below are
   * correct but currently unobserved; wiring a visual is deferred (see
   * `docs/backlog/plugin-manager.md`'s Leftovers). */
  sounding = $state<string | null>(null);
  /** Why the last attempt made no sound, when it made none. A UI hint, not
   * an error state: "no live instance of this plugin to audition" is a
   * perfectly ordinary answer.
   *
   * Unlike `sounding`, this field has no decay timer — it is cleared only
   * by the next successful `play()` anywhere in the app, from any browser.
   * A component that renders it is therefore responsible for clearing it
   * on its own mount, or a reason left behind by a double-click in some
   * other browser can surface as a stale message the moment this one
   * mounts (see `PluginManager.svelte`'s `onMount`). */
  lastSilentReason = $state<string | null>(null);

  /** True while a sample is on the preview stream, so `stop` knows whether
   * `library_audition_stop` has anything to release. */
  #sampleSounding = false;
  #decay: ReturnType<typeof setTimeout> | null = null;
  /** Bumped by every `play()`/`stop()` call. Each `play()` captures its own
   * value and checks it after every `await`: if some later call has since
   * bumped the counter, this one has been superseded and must not touch
   * `sounding`/`#sampleSounding` — otherwise a stale IPC round-trip that
   * resolves after a newer one can resurrect the row it lost to (fix for
   * the overlapping-play race: two rows auditioned in quick succession
   * must never both end up sounding, and the highlight must always land on
   * the last one clicked, not whichever happened to resolve last). */
  #gen = 0;

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
    // This call now owns the generation. Anything still in flight from an
    // earlier call — including the `#release()` below, and including a
    // concurrent external `stop()` — checks against `#gen` and backs off
    // once it sees a newer one has taken over.
    const token = ++this.#gen;
    // Whatever was sounding gets cut first: two overlapping auditions tell
    // you nothing about either one — and a "no live instance to audition"
    // notice must not land while a previous sample keeps ringing, so this
    // runs before the `silent` branch below, not after it.
    await this.#release();
    if (token !== this.#gen) return; // superseded again while releasing
    if (target.kind === "silent") {
      this.lastSilentReason = target.reason;
      return;
    }
    this.lastSilentReason = null;
    try {
      switch (target.kind) {
        case "sample":
          // Only claim the stream — and only light the row — when there is
          // actually a backend to play it: the browser-demo build has no
          // `libraryAudition`, and reporting a sounding row for a sound
          // that never happened is worse than staying silent.
          if (!backend.libraryAudition) break;
          // Set *before* the await, not after: a `stop()` (or a newer
          // `play()`'s `#release()`) that runs while this call is still
          // waiting on the backend must see that there is a sample stream
          // to cut, not find `#sampleSounding` still false and skip it.
          this.#sampleSounding = true;
          await backend.libraryAudition(target.path);
          if (token !== this.#gen) return; // a newer play()/stop() won
          this.#light(target.path);
          break;
        case "instrument":
          await backend.samplerPreviewNote(target.instrumentId, target.key, AUDITION_VELOCITY);
          if (token !== this.#gen) return;
          this.#light(target.instrumentId);
          break;
        case "pluginInstance":
          await backend.pluginPreviewNote(target.instanceId, target.key, AUDITION_VELOCITY);
          if (token !== this.#gen) return;
          this.#light(target.instanceId);
          break;
      }
    } catch (err) {
      // A preview that cannot sound is a hint, never a thrown error that
      // takes a click handler down with it. But if a newer call has since
      // taken over, this stale failure must not clobber its state either.
      if (token !== this.#gen) return;
      this.sounding = null;
      this.#sampleSounding = false;
      this.lastSilentReason = String(err);
    }
  }

  async stop(): Promise<void> {
    // Supersede whatever `play()` may still be in flight — its pending
    // `await`s will see `#gen` has moved on and stand down instead of
    // starting (or re-lighting) after this `stop()` already ran.
    this.#gen++;
    await this.#release();
  }

  /** The actual teardown: clear the decay timer, clear the highlight, and
   * release the sample stream if one is open. Shared by `stop()` and by
   * `play()`'s own cut-the-previous-one-off step. Does not touch `#gen` —
   * the caller owns that, since `play()` must keep the generation it just
   * claimed rather than immediately superseding itself. */
  async #release(): Promise<void> {
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
