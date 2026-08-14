/**
 * The ONE place the timeline's selection-modifier convention is written
 * down, so the clip components and the lane marquee cannot drift apart:
 * plain = replace, Shift = add, Ctrl/Cmd = toggle, Shift+Ctrl/Cmd = subtract.
 */
import type { SelectionMode } from "./clip-selection";

export function selectionModeFor(e: {
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
}): SelectionMode {
  const mod = e.ctrlKey || e.metaKey;
  if (e.shiftKey && mod) return "subtract";
  if (mod) return "toggle";
  if (e.shiftKey) return "add";
  return "replace";
}
