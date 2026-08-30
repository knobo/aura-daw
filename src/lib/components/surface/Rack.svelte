<script lang="ts">
  /**
   * One device faceplate on the deck — a rack. The geometry comes from the
   * device row (`DEVICE_RACKS`), not from a branch here, so a new device is
   * a row of data: a column of mode buttons on the far left, the knobs in
   * their own grid, faders beside them, the pad block on the right.
   *
   * A rack is an object on the page: it removes as a unit and two can sit
   * side by side. That is the whole point of v0.2.1 — before it, a device
   * was a page MODE, so stamping one wiped the deck.
   */
  import type { SurfaceDevice, SurfaceWidget } from "../../utils/control-surface";
  import { GAIN_MAX, GAIN_MIN, surface } from "../../state/surface.svelte";
  import Knob from "../controls/Knob.svelte";
  import Fader from "../controls/Fader.svelte";
  import BindPicker from "./BindPicker.svelte";
  import PadGrid from "./PadGrid.svelte";

  const {
    device,
    groupId,
    widgets,
    edit = false,
  }: {
    device: SurfaceDevice;
    groupId: string;
    widgets: SurfaceWidget[];
    edit?: boolean;
  } = $props();

  const faders = $derived(widgets.filter((w) => w.kind === "fader"));
  const knobs = $derived(widgets.filter((w) => w.kind === "knob"));
  const grids = $derived(widgets.filter((w) => w.kind === "padGrid"));
</script>

<section class="face grain" aria-label={device.name}>
  {#if edit}
    <button class="kill" type="button" title="Remove {device.name}" aria-label="Remove {device.name}" onclick={() => surface.removeRack(groupId)}>×</button>
  {/if}
  <div class="brand">
    <span class="wordmark">AURA</span>
    <span class="model silk">{device.name}</span>
  </div>
  <div class="body">
    {#if device.modes.length > 0}
      <div class="modes">
        {#each device.modes as mode (mode)}
          <span class="modelamp silk">{mode}</span>
        {/each}
      </div>
    {/if}

    {#if faders.length > 0}
      <div class="faders">
        {#each faders as w (w.id)}
          {@const num = surface.numeric(w)}
          <div class="slot">
            {#if edit}
              <button class="tool" type="button" title="Bind this fader" aria-label="Bind {w.label}" onclick={() => surface.toggleBind(w.id)}>⌖</button>
            {/if}
            <Fader
              value={num?.value ?? 0}
              min={num?.min ?? GAIN_MIN}
              max={num?.max ?? GAIN_MAX}
              resetTo={num?.resetTo}
              label={w.label}
              format={num?.format}
              ariaLabel={w.label}
              oninput={(v) => surface.write(w, v)}
              onstart={() => surface.openGesture("fader drag")}
              onend={() => surface.closeGesture()}
            />
            {#if edit && surface.bindFor === w.id}
              <BindPicker widget={w} />
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if knobs.length > 0}
      <div class="knobs" style:grid-template-columns="repeat({device.knobCols}, auto)">
        {#each knobs as w, i (w.id)}
          {@const num = surface.numeric(w)}
          <div class="slot">
            {#if edit}
              <button class="tool" type="button" title="Bind this knob" aria-label="Bind knob {i + 1}" onclick={() => surface.toggleBind(w.id)}>⌖</button>
            {/if}
            <Knob
              value={num?.value ?? 0}
              min={num?.min ?? GAIN_MIN}
              max={num?.max ?? GAIN_MAX}
              resetTo={num?.resetTo}
              bipolar={num?.bipolar}
              label={w.label}
              format={num?.format}
              ariaLabel={w.label}
              size={36}
              oninput={(v) => surface.write(w, v)}
              onstart={() => surface.openGesture("knob drag")}
              onend={() => surface.closeGesture()}
            />
            {#if edit && surface.bindFor === w.id}
              <BindPicker widget={w} />
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#each grids as w (w.id)}
      <!-- The pad block belongs to the rack: it is removed with it, not
           on its own, so it gets no kill button of its own. -->
      <PadGrid widget={w} {edit} removable={false} />
    {/each}
  </div>
</section>

<style>
  .face {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 20px 14px;
    border-radius: 10px;
    background-color: var(--bg-1);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-frame), var(--relief-3);
    width: max-content;
    max-width: 100%;
  }
  .brand {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 24px;
    padding: 0 4px;
  }
  .wordmark {
    font-family: var(--font-ui);
    font-weight: 700;
    letter-spacing: 0.28em;
    font-size: 13px;
    color: var(--text);
  }
  .model {
    letter-spacing: 0.32em;
    color: var(--text-dim);
  }
  /* Landscape, like the hardware: mode buttons on the far left, then the
     pots, then the pad block. A single row of eight knobs above the pads is
     a different instrument. */
  .body {
    display: flex;
    align-items: center;
    gap: 18px;
  }
  .modes {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: none;
  }
  .modelamp {
    padding: 3px 8px;
    border-radius: 2px;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    color: var(--text-faint);
  }
  .knobs {
    display: grid;
    gap: 10px 12px;
    justify-items: center;
  }
  .faders {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    /* Wide enough for a track name over each fader, per the scribble strip. */
    --fader-w: 62px;
  }
  .slot {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }
  .kill {
    position: absolute;
    top: 4px;
    right: 6px;
    z-index: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
  }
  .kill:hover {
    color: var(--red);
  }
  .tool {
    position: absolute;
    top: -6px;
    left: -6px;
    z-index: 1;
    border: none;
    padding: 0;
    background: transparent;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }
  .tool:hover {
    color: var(--cyan);
  }
</style>
