/**
 * Svelte action: focus and select an input's text the moment it mounts —
 * for rename-style editors that swap a button for an input the instant
 * editing starts. `autofocus` alone places the caret but doesn't select,
 * so typing right after open would append to the old text instead of
 * replacing it. An action re-runs this on every mount, which is exactly
 * "editing started again" for a `{#if editing}`-gated input.
 */
export function focusAndSelect(node: HTMLInputElement) {
  node.focus();
  node.select();
}
