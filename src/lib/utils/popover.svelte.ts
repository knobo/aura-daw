/**
 * Viewport-aware placement for one popover, as a rune the component owns.
 *
 * Placement has to be re-measured, not computed once at mount: the menus here
 * are taller than the space under a bottom-docked panel, and their natural
 * height is not final until stylesheets and fonts have landed. A single
 * measurement in `$effect` picked "below" from a pre-CSS height and then never
 * looked again, which put the last items off-screen — exactly the bug the flip
 * exists to prevent. A `ResizeObserver` on the popover re-places it whenever
 * its own content box changes.
 */
import { placePopover, type PopoverPlacement } from "./popover";

export interface PopoverBox {
  /** Bind with `bind:this={box.node}`. */
  node: HTMLElement | undefined;
  readonly placement: PopoverPlacement | null;
}

/**
 * Call during component init. `anchor` returns the element the popover hangs
 * off — passed as a getter because the popover itself is usually portaled out
 * of the DOM position it was declared in, so its own parent is no help.
 */
export function popoverBox(anchor: () => HTMLElement | null | undefined): PopoverBox {
  let node = $state<HTMLElement | undefined>();
  let placement = $state<PopoverPlacement | null>(null);

  $effect(() => {
    const el = node;
    if (!el) return;
    const measure = () => {
      const rect = anchor()?.getBoundingClientRect();
      if (!rect) return;
      placement = placePopover(
        rect,
        { width: el.offsetWidth, height: el.scrollHeight },
        { width: window.innerWidth, height: window.innerHeight },
      );
    };
    measure();
    window.addEventListener("resize", measure);
    const ro = typeof ResizeObserver === "function" ? new ResizeObserver(measure) : null;
    ro?.observe(el);
    return () => {
      window.removeEventListener("resize", measure);
      ro?.disconnect();
    };
  });

  return {
    get node() {
      return node;
    },
    set node(v: HTMLElement | undefined) {
      node = v;
    },
    get placement() {
      return placement;
    },
  };
}
