/**
 * Drive a per-frame `tick` only while `el` is on screen. A track header
 * scrolled out of the timeline, or a channel strip scrolled out of the
 * surface rack, stops paying for a redraw every frame the moment it leaves
 * the viewport — and resumes exactly where it left off the instant it
 * scrolls back, since the caller's own closed-over state (smoothing,
 * peak-hold, …) is untouched while paused.
 *
 * `IntersectionObserver` rather than a distance/rect check: it is native,
 * batched off the main thread, and already accounts for a scrollable
 * ancestor clipping the element (the surface rack, the timeline's track
 * list) — the same case `threshold: 0` is tuned for, since a meter is worth
 * drawing at the first sliver of it, not once it is half in view.
 */
export function runWhileVisible(el: Element, tick: (now: number) => void): () => void {
  let raf = 0;
  const loop = (now: number) => {
    raf = requestAnimationFrame(loop);
    tick(now);
  };
  // jsdom (vitest) has no IntersectionObserver — fall back to always running,
  // same convention as Meter.svelte's ResizeObserver guard.
  if (typeof IntersectionObserver !== "function") {
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }
  const io = new IntersectionObserver(
    ([entry]) => {
      if (entry.isIntersecting) {
        if (!raf) raf = requestAnimationFrame(loop);
      } else if (raf) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
    },
    { threshold: 0 },
  );
  io.observe(el);
  return () => {
    if (raf) cancelAnimationFrame(raf);
    io.disconnect();
  };
}
