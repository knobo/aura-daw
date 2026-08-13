/**
 * Playhead navigation math — pure, so it can be reasoned about (and tested)
 * without a transport, a project or a DOM.
 *
 * Positions are samples; callers supply the grid (samples per bar/beat) and
 * the edge list, since where those come from is store business.
 */

/**
 * Step the playhead one grid unit in `dir` (+1 forward, -1 back), landing ON
 * a grid line rather than at `pos ± step`. A playhead already sitting on a
 * line moves a full unit; one between lines snaps to the next line in that
 * direction, which is what makes repeated presses feel like a grid walk.
 * Clamped at 0 — there is no timeline before the start.
 */
export function gridStep(pos: number, step: number, dir: 1 | -1): number {
  if (!(step > 0)) return Math.max(0, pos);
  const line = pos / step;
  // Guard against float dust making an on-the-line position look off it.
  const eps = 1e-6;
  const next =
    dir > 0 ? Math.floor(line + eps) + 1 : Math.ceil(line - eps) - 1;
  return Math.max(0, Math.round(next * step));
}

/**
 * The nearest clip edge strictly beyond `pos` in `dir`, or null when there
 * is none (so the caller can leave the playhead alone rather than jumping
 * it to an arbitrary bound).
 *
 * Edges need not be sorted or unique — arrangements produce plenty of
 * coincident starts and ends.
 */
export function edgeJump(edges: number[], pos: number, dir: 1 | -1): number | null {
  let best: number | null = null;
  for (const e of edges) {
    if (e < 0) continue;
    if (dir > 0 ? e > pos : e < pos) {
      if (best === null || (dir > 0 ? e < best : e > best)) best = e;
    }
  }
  return best === null ? null : Math.max(0, Math.round(best));
}

/** Start and end of every clip, the positions worth navigating between. */
export function clipEdges(
  clips: { timelineStartSamples: number; lengthSamples: number }[],
): number[] {
  const edges: number[] = [];
  for (const c of clips) {
    edges.push(c.timelineStartSamples, c.timelineStartSamples + c.lengthSamples);
  }
  return edges;
}
