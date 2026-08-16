/**
 * Pure clip-selection operations for the arrangement timeline — the
 * clip-level mirror of note-ops.ts's note-level selection (PR #11).
 *
 * Selection is VIEWER state: it never enters the document, never becomes an
 * op, and the backend never learns what is selected. It is keyed by
 * `"<kind>:<id>"` rather than a bare clip id because audio clips
 * (project.clips) and MIDI clips (midi.clips) are minted by two independent
 * stores, so an id alone cannot say which store to resolve it against.
 * `refKey`/`parseKey` are the ONLY places that encoding is known.
 */

export type ClipKind = "audio" | "midi";

export interface ClipRef {
  kind: ClipKind;
  id: string;
}

export type SelectionMode = "replace" | "add" | "toggle" | "subtract";

/** The set's element type: `"audio:<id>"` / `"midi:<id>"`. */
export function refKey(ref: ClipRef): string {
  return `${ref.kind}:${ref.id}`;
}

/** Inverse of refKey. Splits on the FIRST colon only, so an id that itself
 * contains a colon survives the round trip. */
export function parseKey(key: string): ClipRef {
  const cut = key.indexOf(":");
  return { kind: key.slice(0, cut) as ClipKind, id: key.slice(cut + 1) };
}

/** Combine `hits` into `current` under `mode`. Never mutates `current`. */
export function applySelection(
  current: Set<string>,
  hits: ClipRef[],
  mode: SelectionMode,
): Set<string> {
  if (mode === "replace") return new Set(hits.map(refKey));
  const next = new Set(current);
  for (const ref of hits) {
    const k = refKey(ref);
    if (mode === "add") next.add(k);
    else if (mode === "subtract") next.delete(k);
    else if (next.has(k)) next.delete(k);
    else next.add(k);
  }
  return next;
}

/** One clip's screen-independent extent, for marquee hit-testing: its lane
 * (track) index and its timeline span in SAMPLES — MIDI clips are converted
 * by the caller through the shipped section table, never re-derived here. */
export interface LaneBox {
  ref: ClipRef;
  laneIndex: number;
  startSamples: number;
  endSamples: number;
}

/** Clips whose half-open span [start, end) overlaps [s0, s1) and whose lane
 * index is within [laneLo, laneHi]. Half-open on purpose: a marquee that
 * starts exactly where a clip ends must not select it, matching
 * `marqueeHits`'s note-level rule in note-ops.ts. */
export function marqueeClipHits(
  boxes: LaneBox[],
  s0: number,
  s1: number,
  laneLo: number,
  laneHi: number,
): ClipRef[] {
  return boxes
    .filter(
      (b) =>
        b.laneIndex >= laneLo &&
        b.laneIndex <= laneHi &&
        b.endSamples > s0 &&
        b.startSamples < s1,
    )
    .map((b) => b.ref);
}

/** Elements inside the lane area that own the pointer themselves, so a
 * pointerdown on them must NOT also start a marquee. */
const MARQUEE_IGNORE = ".clip, .mclip, button, .autolane, .launchmark";

/** Whether a pointerdown on `target` should begin a marquee band.
 *
 * The guard lives HERE, on Track C's side, rather than as a
 * `stopPropagation()` in the automation lane: adding a selector cannot
 * change any other track's behaviour, while swallowing the event in a
 * component owned by another track changes the flow for every ancestor
 * listener, present and future. Whoever adds a new absolutely-positioned
 * overlay inside `.lane` adds it to this list. */
export function startsMarquee(target: { closest(sel: string): unknown } | null): boolean {
  return !!target && !target.closest(MARQUEE_IGNORE);
}

/** Pixels of movement before an empty-lane press becomes a marquee.
 * Below this, the press is a click / the first half of a double-click —
 * capturing the pointer on pointerdown (WebKit/Tauri) swallows `dblclick`
 * on the lane, which is how a MIDI clip is created. */
export const MARQUEE_SLOP_PX = 4;

export function movedPastMarqueeSlop(dx: number, dy: number): boolean {
  return dx * dx + dy * dy >= MARQUEE_SLOP_PX * MARQUEE_SLOP_PX;
}
