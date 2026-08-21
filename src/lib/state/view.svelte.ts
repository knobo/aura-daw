/**
 * Timeline viewport: zoom (samples-per-pixel) + horizontal scroll, shared by
 * the ruler, clip lane and playhead so they always agree on the mapping
 * between samples and CSS pixels.
 */

import { project } from "./project.svelte";

const MIN_SPP = 16; // extreme zoom-in
const MAX_SPP = 65536; // extreme zoom-out

/** Fraction of the viewport a revealed position is placed at, measured
 * from the left edge — a little lead-in so the playhead has somewhere to
 * go after a page-scroll. */
const LEAD_FRAC = 0.08;
/** Everything left of this fraction counts as visible; past it, page. */
const VISIBLE_FRAC = 0.92;

class TimelineView {
  /** samples per CSS pixel */
  spp = $state(1200);
  /** sample at the viewport's left edge */
  viewStart = $state(0);
  /** viewport width in CSS px (kept fresh by the Timeline component) */
  width = $state(960);
  /** snap clip drags to the beat grid */
  snap = $state(true);
  /** auto-scroll to keep the playhead visible while playing */
  follow = $state(true);

  get viewEnd(): number {
    return this.viewStart + this.width * this.spp;
  }

  xOf(samples: number): number {
    return (samples - this.viewStart) / this.spp;
  }

  samplesAt(x: number): number {
    return this.viewStart + x * this.spp;
  }

  scrollBy(px: number) {
    this.viewStart = Math.max(0, this.viewStart + px * this.spp);
  }

  scrollToSamples(samples: number) {
    this.viewStart = Math.max(0, samples);
  }

  /**
   * Bring `samples` on screen, and do nothing when it is already there.
   *
   * Both follow paths run through this: the rAF tick while rolling, and
   * the seek handler while stopped. The "already there" half is what makes
   * the stopped path safe — without it, following would yank the view back
   * every time the user scrolled away to look at something.
   *
   * The trailing margin (position past 92% of the viewport counts as off
   * screen) is what makes playback page ahead BEFORE the playhead hits the
   * right edge, rather than after.
   */
  revealSamples(samples: number) {
    const span = this.width * this.spp;
    const x = (samples - this.viewStart) / this.spp;
    if (x >= 0 && x <= this.width * VISIBLE_FRAC) return;
    this.scrollToSamples(samples - span * LEAD_FRAC);
  }

  /** Zoom keeping the sample under cursor-x fixed on screen. */
  zoomAt(x: number, factor: number) {
    const anchor = this.samplesAt(x);
    const next = Math.min(MAX_SPP, Math.max(MIN_SPP, this.spp * factor));
    this.spp = next;
    this.viewStart = Math.max(0, anchor - x * next);
  }

  /** Snap a sample position to the nearest beat (finer when zoomed in). */
  snapSamples(samples: number): number {
    if (!this.snap) return samples;
    const beat = project.samplesPerBeat;
    // use quarter-beats once a beat is wider than ~80 px
    const grid = beat / this.spp > 80 ? beat / 4 : beat;
    return Math.round(samples / grid) * grid;
  }
}

export const view = new TimelineView();
