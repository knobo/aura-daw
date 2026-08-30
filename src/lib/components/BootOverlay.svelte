<script lang="ts">
  /**
   * Full-window boot overlay: the AURA wordmark, the current boot label, a
   * progress bar (determinate once the backend reports a fraction,
   * indeterminate shimmer otherwise), and a smaller detail line. Sits above
   * the shell, which keeps rendering unconditionally underneath it — this
   * component only ever covers it, never gates it.
   *
   * Removal is two-step: `phase === "ready"` starts a ~250ms fade, after
   * which the overlay leaves the DOM entirely so it can never eat a click.
   * A `failed` boot stays up with the error and a dismiss button instead —
   * a failed restore must never trap the user behind the overlay.
   *
   * "ready" can un-happen: a media decode that reports in after the boot
   * chain resolved pulls boot.phase back to "media" (see boot.svelte.ts's
   * module doc) mid-fade, and the effect below cancels the pending removal
   * and un-fades so the overlay is legible again rather than sitting there
   * invisible-but-mounted. Once `inDom` actually goes false, though, that
   * is final for this component instance's lifetime (nothing ever sets it
   * back to true) — a later, unrelated media event flipping boot.phase
   * again cannot resurrect an overlay that has already left the DOM.
   */
  import { boot } from "../state/boot.svelte";

  let inDom = $state(true);
  let fading = $state(false);

  function reducedMotion(): boolean {
    return typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  }

  $effect(() => {
    if (boot.phase !== "ready") {
      // A bounce back from "ready" to "media" (a straggling media-progress
      // event) must show the overlay again, not leave it sitting invisible
      // mid-fade — see the module doc above.
      fading = false;
      return;
    }
    fading = true;
    const timer = setTimeout(() => (inDom = false), reducedMotion() ? 0 : 250);
    return () => clearTimeout(timer);
  });

  /**
   * Escape always reaches the shell, boot state notwithstanding: an
   * impatient user (or one staring at a failed restore) must never be
   * stuck behind this overlay just because it hasn't decided to fade.
   */
  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && inDom) {
      e.preventDefault();
      inDom = false;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if inDom}
  <div class="veil" class:fading role="status" aria-live="polite">
    <div class="card glass">
      <div class="wordmark mono">AURA</div>
      <div class="label mono">{boot.label}</div>
      <div class="pbar" class:indeterminate={boot.progress === null}>
        <div
          class="pfill"
          style:width="{boot.progress === null ? 40 : Math.round(boot.progress * 100)}%"
        ></div>
      </div>
      {#if boot.detail}
        <div class="detail mono">{boot.detail}</div>
      {/if}
      {#if boot.phase === "failed"}
        <div class="error">{boot.error ?? "Startup did not finish cleanly."}</div>
        <button class="continue mono" onclick={() => (inDom = false)}>Continue anyway</button>
      {/if}
    </div>
  </div>
{/if}

<style>
  .veil {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: grid;
    place-items: center;
    background: var(--bg-0);
    transition: opacity 250ms ease-out;
  }
  .veil.fading {
    opacity: 0;
    /* the fade must never eat a click on the shell underneath while it plays */
    pointer-events: none;
  }

  .card {
    width: min(360px, calc(100vw - 48px));
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 28px 26px 22px;
    border-radius: 10px;
    text-align: center;
  }

  .wordmark {
    font-size: 20px;
    font-weight: 600;
    letter-spacing: 0.55em;
    padding-left: 0.55em;
    color: var(--text);
    text-shadow:
      0 0 calc(16px * var(--glow-scale)) var(--cyan-dim),
      1px 0 0 rgb(var(--magenta-rgb) / 0.55),
      -1px 0 0 rgb(var(--cyan-rgb) / 0.55);
  }

  .label {
    font-size: 10px;
    letter-spacing: 0.14em;
    color: var(--text-mid);
    min-height: 1.4em;
  }

  .pbar {
    width: 100%;
    height: 3px;
    border-radius: 2px;
    background: rgb(var(--line-rgb) / 0.15);
    overflow: hidden;
    position: relative;
  }
  .pfill {
    height: 100%;
    background: linear-gradient(90deg, var(--cyan), var(--magenta));
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.5);
    transition: width 200ms ease-out;
  }
  /* Indeterminate: a fixed-width segment sweeps the track instead of the
     fill tracking a known fraction. The global prefers-reduced-motion rule
     (app.css) collapses this to a static sliver rather than a moving one. */
  .pbar.indeterminate .pfill {
    position: absolute;
    animation: boot-sweep 1.1s ease-in-out infinite;
  }
  @keyframes boot-sweep {
    0% {
      left: -40%;
    }
    100% {
      left: 100%;
    }
  }

  .detail {
    font-size: 9px;
    color: var(--text-faint);
    word-break: break-all;
  }

  .error {
    font-size: 11px;
    line-height: 1.5;
    color: var(--red-soft);
  }

  .continue {
    margin-top: 4px;
    padding: 8px 16px;
    border: var(--border-width) solid rgb(var(--line-rgb) / 0.3);
    border-radius: 5px;
    background: transparent;
    color: var(--text);
    font-size: 10px;
    letter-spacing: 0.14em;
    cursor: pointer;
    transition: border-color 120ms, color 120ms;
  }
  .continue:hover {
    border-color: var(--cyan);
    color: var(--cyan);
  }
</style>
