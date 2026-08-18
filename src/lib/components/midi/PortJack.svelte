<script lang="ts">
  /**
   * The OUT jack: one port's accent swatch, its name, and the channel it
   * sends on. The swatch is the whole point of the patchbay — two rows
   * wearing the same colour are two tracks sharing a port, readable without
   * reading a single word. Channel is stored 0-15 and shown 1-16, the way a
   * synth's front panel labels it.
   */
  import type { RouteTarget } from "./routing-view";

  let {
    target,
    accent,
    live = false,
    emptyLabel = "not patched",
  }: {
    target: RouteTarget | null;
    /** Resolved CSS colour for this port's accent slot, e.g. `var(--cyan)`. */
    accent: string;
    /** Notes left this port on the last poll. */
    live?: boolean;
    emptyLabel?: string;
  } = $props();
</script>

<span
  class="jack mono"
  class:empty={!target}
  class:closed={target?.health === "closed"}
  class:missing={target?.health === "missing"}
  style="--accent: {accent}"
>
  <span class="plug" class:live aria-hidden="true"></span>
  {#if target}
    <span class="pname" title={target.portName}>{target.portName}</span>
    <span class="ch" title={target.channel === null ? "each note on its own channel" : undefined}
      >{target.channel === null ? "ch •" : `ch ${target.channel + 1}`}</span
    >
    {#if target.health !== "open"}
      <span class="warn" aria-hidden="true">!</span>
    {/if}
  {:else}
    <span class="none silk">{emptyLabel}</span>
  {/if}
</span>

<style>
  .jack {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    max-width: 100%;
    padding: 2px 5px;
    border-radius: 3px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-1-rgb) / 0.55);
    font-size: 10px;
    color: var(--text-mid);
  }
  .jack.empty {
    border-style: dashed;
    background: transparent;
  }
  .jack.closed {
    border-color: rgb(var(--amber-rgb) / 0.5);
  }
  .jack.missing {
    border-color: rgb(var(--red-rgb) / 0.5);
  }

  /* The plug: a bead of the port's colour — same size and idiom as the
     MasterBar activity dot. */
  .plug {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 calc(4px * var(--glow-scale)) var(--accent);
  }
  .jack.empty .plug {
    background: transparent;
    border: 1px solid rgb(var(--line-rgb) / 0.35);
    box-shadow: none;
  }
  .jack.missing .plug {
    background: rgb(var(--line-rgb) / 0.25);
    box-shadow: none;
  }
  .plug.live {
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }

  .pname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .jack.missing .pname {
    color: var(--text-dim);
    text-decoration: line-through;
  }
  .ch {
    flex: none;
    font-size: 9px;
    color: var(--text-dim);
  }
  .warn {
    flex: none;
    font-size: 10px;
    font-weight: 700;
    color: var(--amber);
  }
  .jack.missing .warn {
    color: var(--red);
  }
  .none {
    letter-spacing: 0.14em;
  }

  @media (prefers-reduced-motion: reduce) {
    .plug.live {
      animation: none;
      opacity: 1;
    }
  }
</style>
