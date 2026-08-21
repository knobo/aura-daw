/**
 * "Jump to this target's lane" (design §6.1-6.3): the automation matrix, the
 * pinned-param chips and the header's automate-picker all describe the same
 * gesture — clicking a param jumps to its lane, minting one if it has none.
 * One helper, so the three surfaces cannot drift.
 */
import { lanes } from "../state/lanes.svelte";
import { modulation } from "../state/modulation.svelte";
import { toasts } from "../state/toasts.svelte";
import type { TargetRef } from "../types/ipc";

/** Reveal the automation lane for `target` on `trackId`: unfold the lane if
 * it is folded, show (or mint) the binding's overlay, and scroll the track's
 * header into view. `initialNormalized` seeds a freshly minted curve.
 *
 * `trackId` is empty for an orphaned instance — `plugins.bind()` nulls out
 * the *previous* instance's `trackId` on a rebind, and the rack's "Not on
 * any track" bucket exists precisely because that state is reachable.
 * `modulation.pickTarget` does not validate the track, so minting here
 * would file a real curve+binding under a track key nothing can ever look
 * up again — reachable, invisible, unreachable, undoable only by luck.
 * Refuse instead, and say so, so the click is not silently inert. */
export async function revealParamLane(
  trackId: string,
  target: TargetRef,
  initialNormalized?: number,
): Promise<void> {
  if (!trackId) {
    toasts.error("NOT ON A TRACK", "This plugin isn't on a track, so there's no lane to put automation on.");
    return;
  }
  try {
    if (lanes.isTrackCollapsed(trackId)) lanes.toggleTrack(trackId);

    // `pickTarget` mints a curve+binding when the target has none, and
    // shows the overlay — but it TOGGLES an already-top-visible overlay
    // off, which is correct for the header picker and wrong for a jump:
    // a jump must always end with the lane visible.
    const binding = await modulation.pickTarget(trackId, target, initialNormalized);
    if (binding && !modulation.isBindingVisible(trackId, binding.id)) {
      modulation.show(trackId, binding.id);
    }

    try {
      const el = document?.querySelector?.(`[data-track-id="${CSS.escape(trackId)}"]`);
      el?.scrollIntoView?.({ block: "nearest" });
    } catch {
      /* no DOM (SSR/tests) or scrollIntoView unimplemented (jsdom) — fine */
    }
  } catch (err) {
    console.warn("[aura] revealParamLane failed:", err);
  }
}
