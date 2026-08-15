/**
 * Pointer-mode for an automation clip. A body click that missed every
 * point MOVES the clip; a hit edits the point; the right 8 px resizes.
 * (Task 9 review: `|| !nearRight` used to send every body click into
 * point-insert, so a clip could not be dragged in time.)
 */
export type AutomationClipGesture = "move" | "resize" | "point" | "delete" | "ignore";

export function automationClipGesture(opts: {
  nearRight: boolean;
  hit: number;
  erase: boolean;
}): AutomationClipGesture {
  if (opts.erase) return opts.hit >= 0 ? "delete" : "ignore";
  if (opts.nearRight) return "resize";
  if (opts.hit >= 0) return "point";
  return "move";
}
