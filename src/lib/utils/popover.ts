/**
 * Where a popover goes when its anchor is near an edge.
 *
 * The surface panel is docked at the BOTTOM of the window and its deck
 * scrolls, so a menu anchored below its trigger runs off-screen and one
 * anchored above can run past the top instead — the `+` menu did both, which
 * left its last items unreachable at a 950 px window height. Pure geometry so
 * the rule is testable without a DOM.
 */

export interface PopoverAnchor {
  top: number;
  bottom: number;
  left: number;
}

export interface PopoverPlacement {
  left: number;
  top: number;
  /** Cap for the popover's own scroll box, so it never leaves the viewport. */
  maxHeight: number;
  side: "above" | "below";
}

export function placePopover(
  anchor: PopoverAnchor,
  size: { width: number; height: number },
  viewport: { width: number; height: number },
  gap = 6,
  pad = 8,
): PopoverPlacement {
  const below = Math.max(0, viewport.height - pad - (anchor.bottom + gap));
  const above = Math.max(0, anchor.top - gap - pad);
  // Prefer below when it fits; otherwise whichever side has more room.
  const side: "above" | "below" = size.height <= below || below >= above ? "below" : "above";
  const maxHeight = Math.max(48, side === "below" ? below : above);
  const height = Math.min(size.height, maxHeight);
  const top = side === "below" ? anchor.bottom + gap : Math.max(pad, anchor.top - gap - height);
  const left = Math.max(pad, Math.min(anchor.left, viewport.width - pad - size.width));
  return { left, top, maxHeight, side };
}
