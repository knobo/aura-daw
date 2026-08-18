import type { ClipRef } from "./clip-selection";

/**
 * Which timeline clip a window-level Delete/Backspace should remove.
 * Pointer-select does not focus the clip element, so the clip's own
 * onkeydown never fires after a click — App.svelte has to decide.
 *
 * Only the single-selection case: batch-deleting the whole `clipSelection`
 * needs a `clips_remove` transaction that does not exist yet (Track C
 * handoff), so more than one clip selected is a no-op here, same as today.
 */
export function clipDeleteTarget(selected: ClipRef[]): ClipRef | null {
  return selected.length === 1 ? selected[0] : null;
}
