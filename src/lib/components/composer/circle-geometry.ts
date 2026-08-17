/**
 * Wedge geometry for the circle of fifths — the ONE piece of computation the
 * Composer's frontend owns, and it is presentation, not theory (ADR 0006):
 * angles and SVG paths, given twelve slots the backend already decided the
 * meaning of.
 *
 * Pure TypeScript, no runes, no DOM: importable from node tests, which is the
 * same reason `theme/tokens.ts` is written that way.
 */

/** Where one wedge sits and how to draw it. */
export interface WedgeGeometry {
  /** Index in the backend's clockwise wedge array. */
  index: number;
  /** Mid-angle in degrees, 0 = up, growing clockwise. */
  angle: number;
  /** Filled annulus segment (outer ring: major keys). */
  outerPath: string;
  /** Filled annulus segment (inner ring: relative minors). */
  innerPath: string;
  /** Label anchor on the outer ring. */
  outerLabel: { x: number; y: number };
  /** Label anchor on the inner ring. */
  innerLabel: { x: number; y: number };
}

export interface CircleLayout {
  size: number;
  centre: number;
  wedges: WedgeGeometry[];
}

/** Radii as fractions of the half-size, outermost first. */
const R_OUTER = 1.0;
const R_MID = 0.66;
const R_INNER = 0.36;

/**
 * Lay out twelve wedges.
 *
 * `tonicIndex` is rotated to the TOP: the backend says which slot the tonic
 * is on (`Wedge.isTonic`), so the widget never has to know that C major has
 * no accidentals. Rotating rather than re-spelling is what makes the same
 * component correct in every key.
 */
export function circleLayout(count: number, tonicIndex: number, size = 240): CircleLayout {
  const centre = size / 2;
  const step = 360 / Math.max(1, count);
  const wedges: WedgeGeometry[] = [];
  for (let i = 0; i < count; i++) {
    // Rotate so the tonic's slot is centred at the top.
    const angle = ((i - tonicIndex) * step + 360) % 360;
    const from = angle - step / 2;
    const to = angle + step / 2;
    wedges.push({
      index: i,
      angle,
      outerPath: annulus(centre, R_MID * centre, R_OUTER * centre, from, to),
      innerPath: annulus(centre, R_INNER * centre, R_MID * centre, from, to),
      outerLabel: polar(centre, ((R_OUTER + R_MID) / 2) * centre, angle),
      innerLabel: polar(centre, ((R_MID + R_INNER) / 2) * centre, angle),
    });
  }
  return { size, centre, wedges };
}

/** Point at `radius` and `deg` degrees clockwise from straight up. */
export function polar(centre: number, radius: number, deg: number): { x: number; y: number } {
  const rad = ((deg - 90) * Math.PI) / 180;
  return { x: centre + radius * Math.cos(rad), y: centre + radius * Math.sin(rad) };
}

/**
 * An annulus segment as an SVG path: out along `from`, arc to `to`, in, arc
 * back. Two arcs rather than a stroked circle so each wedge is its own hit
 * target — a click has to name exactly one key.
 */
export function annulus(
  centre: number,
  rInner: number,
  rOuter: number,
  from: number,
  to: number,
): string {
  const a = polar(centre, rOuter, from);
  const b = polar(centre, rOuter, to);
  const c = polar(centre, rInner, to);
  const d = polar(centre, rInner, from);
  // A wedge is always well under a half turn (12 slots = 30°), so the
  // large-arc flag is 0; sweep 1 outward, 0 back.
  const large = Math.abs(to - from) > 180 ? 1 : 0;
  return [
    `M ${r(a.x)} ${r(a.y)}`,
    `A ${r(rOuter)} ${r(rOuter)} 0 ${large} 1 ${r(b.x)} ${r(b.y)}`,
    `L ${r(c.x)} ${r(c.y)}`,
    `A ${r(rInner)} ${r(rInner)} 0 ${large} 0 ${r(d.x)} ${r(d.y)}`,
    "Z",
  ].join(" ");
}

function r(n: number): number {
  return Math.round(n * 100) / 100;
}
