/**
 * Startup progress: what App.svelte's boot chain is doing right now, so
 * BootOverlay can tell a slow boot from a hang instead of painting nothing.
 *
 * Fed two ways, deliberately kept separate:
 *  - App.svelte calls `setPhase()` / `finish()` / `fail()` around its own
 *    await points — those are the only ordering ground truth, and this
 *    store must never change that ordering or which awaits are awaited.
 *  - `wire()` subscribes to the two backend progress events the engine
 *    emits mid-restore (`project://open-progress`, `project://media-
 *    progress`), refining the label while `setPhase()` has parked on
 *    "project". Both are optional: an older backend that never sends them,
 *    and demo mode (which never fires them at all), must still reach
 *    "ready" — wire() only ever refines a label already set elsewhere, it
 *    never blocks or gates completion.
 *
 * "ready" is NOT simply "App.svelte's chain resolved". `open_project`
 * returns as soon as it has sent the engine a Rebuild message — the actual
 * 435 MB-WAV-decode + waveform-pyramid pass runs afterwards, on the engine
 * control thread, and is what emits `project://media-progress`. So the
 * chain's own await can resolve (and `finish()` fire) BEFORE the first
 * media event, WHILE, or (rarely) after the last one — there is no
 * guaranteed ordering across the process boundary. `finish()` therefore
 * only records that the chain is done; `chainDone && !mediaInFlight` is
 * what actually computes "ready" (see `settle()`), so a media event that
 * lands after `finish()` still pulls the phase back to "media" and keeps
 * the overlay showing real progress instead of a frozen "Ready" behind a
 * still-decoding project.
 *
 * Deliberate non-guarantee: once `settle()` has reached "ready" and
 * BootOverlay has faded itself out, nothing in THIS store stops a later,
 * unrelated `project://media-progress` (e.g. the user opening a second
 * project by hand, long after boot) from flipping `phase` back to "media"
 * again. That is fine — BootOverlay's own `inDom` flag is a one-way
 * ratchet (see that component) and never remounts the overlay once it has
 * left the DOM, so a phase flip with no mounted overlay left to show it is
 * inert. This store only guards against resurrecting a *visible* overlay
 * after a real `fail()`, which is why the failed check stays a hard stop.
 */
import { backend } from "../tauri";
import type { AuraEventMap } from "../types/ipc";

export type BootPhase = "starting" | "stores" | "project" | "media" | "ready" | "failed";

const PHASE_LABEL: Record<Exclude<BootPhase, "failed">, string> = {
  starting: "Starting AURA…",
  stores: "Connecting to the audio engine…",
  project: "Opening the last project…",
  media: "Loading audio…",
  ready: "Ready",
};

/** Fallback wording when a `project://open-progress` event arrives without
 * a usable `label` — defensive against an older/partial backend. */
const STEP_LABEL: Record<AuraEventMap["project://open-progress"]["step"], string> = {
  load: "Opening project",
  midi: "Loading MIDI",
  journal: "Replaying history",
  plugins: "Loading plugins",
  automation: "Loading automation",
  midiOut: "Connecting MIDI outputs",
  modulation: "Loading modulation",
  rebuild: "Rebuilding graph",
  done: "Finishing up",
};

const SAFETY_TIMEOUT_MS = 60_000;
const STILL_WORKING_LABEL = "Still working — see the log for details";

class BootStore {
  phase = $state<BootPhase>("starting");
  label = $state<string>(PHASE_LABEL.starting);
  detail = $state<string | null>(null);
  progress = $state<number | null>(null);
  error = $state<string | null>(null);

  private wired = false;
  private safetyTimer: ReturnType<typeof setTimeout> | null = null;
  /** App.svelte's own await chain (the boot Promise.all + restoreLast) has
   * resolved. NOT the same as "ready" — see `settle()`. */
  private chainDone = false;
  /** True from a `project://media-progress` event with `done: false` until
   * its matching `done: true` (or forever, if one never arrives). */
  private mediaInFlight = false;

  /** Move to a new non-terminal, non-computed phase, with its default label
   * unless one is given (the `project://open-progress` label is often more
   * specific than the generic phase wording). Clears any stale detail/
   * error. "ready" and "failed" have their own entry points (`finish()` /
   * `settle()` and `fail()`) because both are computed/terminal rather than
   * something a caller just walks into. */
  setPhase(phase: Exclude<BootPhase, "failed" | "ready">, label?: string) {
    this.phase = phase;
    this.label = label ?? PHASE_LABEL[phase];
    this.detail = null;
    this.progress = null;
    this.error = null;
  }

  /** App.svelte calls this once the boot chain's final await resolves —
   * including the early-return paths in `projectops.restoreLast()` (demo
   * mode, no stored project, one already open): those are success too.
   * Does NOT itself mean "ready": if a media decode is still in flight
   * (already reported, or one that reports in after this call — see the
   * module doc), `settle()` leaves the phase on "media" until it clears. */
  finish() {
    this.chainDone = true;
    this.settle();
  }

  /** The boot chain threw. Stays visible with the error and a dismiss
   * button (BootOverlay) rather than spinning forever. A hard stop: unlike
   * "ready", a failed boot must never be pulled back to "media" by a
   * straggling progress event. */
  fail(err: unknown) {
    this.phase = "failed";
    this.error = err instanceof Error ? err.message : String(err);
    this.clearSafetyTimeout();
  }

  /** The only place "ready" is set. Fires from `finish()` (chain resolved)
   * and from `onMediaProgress` (a decode finished) — either can be the one
   * that makes both conditions true, depending on which order they land in
   * for this particular project. */
  private settle() {
    if (this.phase === "failed") return;
    if (!this.chainDone || this.mediaInFlight) return;
    this.phase = "ready";
    this.label = PHASE_LABEL.ready;
    this.detail = null;
    this.progress = 1;
    this.error = null;
    this.clearSafetyTimeout();
  }

  /** Refine the "project" phase's label with the backend's own step/detail.
   * Ignored once boot has failed — a late event from a torn-down
   * subscription must not resurrect the overlay's text over an error the
   * user still needs to see/dismiss. Open-progress events only ever occur
   * *before* `open_project`'s promise resolves, so unlike media-progress
   * there is no legitimate late arrival to accommodate here. */
  private onOpenProgress = (e: AuraEventMap["project://open-progress"]) => {
    if (this.phase === "ready" || this.phase === "failed") return;
    this.phase = "project";
    this.label = e.label || STEP_LABEL[e.step] || PHASE_LABEL.project;
    this.detail = e.detail;
    this.progress = e.total > 0 ? e.index / e.total : null;
  };

  /**
   * The audio-decode/peaks pass that follows project load — and, per the
   * module doc, can still be running (or not yet started) once `finish()`
   * has already fired. Unlike `onOpenProgress`, this only bails on
   * "failed": a media event arriving after `chainDone` is exactly the case
   * this store exists to handle, so it must be able to pull the phase back
   * from "ready" to "media" rather than being dropped. `settle()` then
   * decides whether that leaves the boot done (both conditions true) or
   * still waiting on this decode.
   */
  private onMediaProgress = (e: AuraEventMap["project://media-progress"]) => {
    if (this.phase === "failed") return;
    this.mediaInFlight = !e.done;
    this.phase = "media";
    const verb = e.phase === "peaks" ? "Building waveforms" : "Loading audio";
    this.label = e.total > 0 ? `${verb} — ${e.loaded} of ${e.total} files` : verb;
    this.detail = e.name;
    this.progress = e.total > 0 ? e.loaded / e.total : null;
    this.settle();
  };

  /**
   * Subscribe to the two backend progress events. Safe to call once; a
   * second call is a no-op. Defensive against `backend.on` throwing for an
   * event name an older backend doesn't recognize — that must not stop
   * boot from reaching "ready" through App.svelte's own `finish()` call.
   * Returns an unsubscribe (used by tests and by App.svelte's teardown).
   */
  wire(): () => void {
    if (this.wired) return () => {};
    this.wired = true;
    const stops: Array<() => void> = [];
    try {
      stops.push(backend.on("project://open-progress", this.onOpenProgress));
    } catch (err) {
      console.warn("[aura] project://open-progress not available", err);
    }
    try {
      stops.push(backend.on("project://media-progress", this.onMediaProgress));
    } catch (err) {
      console.warn("[aura] project://media-progress not available", err);
    }
    return () => {
      for (const stop of stops) stop();
      this.wired = false;
    };
  }

  /** Arm the "is this a hang?" fallback: if boot hasn't settled within `ms`,
   * be honest about it instead of leaving the label — and the user —
   * spinning forever. Does not change phase and does not auto-dismiss. */
  armSafetyTimeout(ms = SAFETY_TIMEOUT_MS) {
    this.clearSafetyTimeout();
    this.safetyTimer = setTimeout(() => {
      if (this.phase !== "ready" && this.phase !== "failed") {
        this.label = STILL_WORKING_LABEL;
      }
    }, ms);
  }

  clearSafetyTimeout() {
    if (this.safetyTimer !== null) clearTimeout(this.safetyTimer);
    this.safetyTimer = null;
  }

  /** Test-only: back to the pre-boot state. */
  reset() {
    this.phase = "starting";
    this.label = PHASE_LABEL.starting;
    this.detail = null;
    this.progress = null;
    this.error = null;
    this.wired = false;
    this.chainDone = false;
    this.mediaInFlight = false;
    this.clearSafetyTimeout();
  }
}

export const boot = new BootStore();
