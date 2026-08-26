/**
 * Move a node to `document.body` for as long as it is mounted.
 *
 * Popovers inside the surface panel need this: `.glass` sets
 * `backdrop-filter`, which makes every `.glass` ancestor the containing block
 * for `position: fixed` descendants (CSS Filter Effects §3.1). A menu with
 * `top: 8px` therefore lands 8 px below the PANEL, not the window — and the
 * deck's `overflow: auto` clips whatever is left. Out at body level, viewport
 * coordinates mean what they say and nothing clips.
 */
export function portal(node: HTMLElement, target: HTMLElement = document.body) {
  target.appendChild(node);
  return {
    destroy() {
      node.remove();
    },
  };
}
