<script lang="ts">
  /**
   * Channel-strip lamp: mute / solo / arm. A raised key with an LED that
   * lights when the function is on. Click toggles.
   */
  interface Props {
    on: boolean;
    label: string;
    /** Visual role — colours the LED. */
    role?: "mute" | "solo" | "arm";
    ariaLabel: string;
    onclick?: () => void;
  }

  const { on, label, role = "mute", ariaLabel, onclick }: Props = $props();
</script>

<button
  class="lamp grain"
  class:on
  class:mute={role === "mute"}
  class:solo={role === "solo"}
  class:arm={role === "arm"}
  type="button"
  aria-pressed={on}
  aria-label={ariaLabel}
  {onclick}
>
  <span class="led"></span>
  <span class="silk">{label}</span>
</button>

<style>
  .lamp {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    width: 28px;
    padding: 5px 2px 4px;
    border-radius: var(--ctrl-radius);
    background-color: var(--bg-2);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-raised), var(--relief-1);
    color: var(--text-dim);
    cursor: pointer;
    border: none;
  }
  .lamp:hover .silk {
    color: var(--text);
  }
  .lamp:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: 2px;
  }
  .lamp.on {
    box-shadow: var(--bevel-inset);
    transform: translateY(calc(1px * var(--relief)));
  }
  .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: rgb(var(--line-rgb) / 0.25);
    box-shadow: inset 0 1px 1px rgb(var(--shadow-rgb) / 0.5);
  }
  .lamp.on.mute .led {
    background: var(--red);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--red);
  }
  .lamp.on.solo .led {
    background: var(--amber);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--amber);
  }
  .lamp.on.arm .led {
    background: var(--red-soft);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) var(--red);
  }
  .silk {
    font-size: 8px;
    letter-spacing: 0.12em;
  }
</style>
