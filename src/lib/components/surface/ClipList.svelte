<script lang="ts">
  import { launch } from "../../state/launch.svelte";
  import { midi } from "../../state/midi.svelte";
  import { surface } from "../../state/surface.svelte";

  const { edit = false, widgetId }: { edit?: boolean; widgetId: string } = $props();
</script>

<section class="list grain" aria-label="MIDI clips">
  <header>
    <span class="silk">CLIPS</span>
    {#if edit}
      <button class="kill" type="button" title="Remove clip list" onclick={() => surface.remove(widgetId)}>×</button>
    {/if}
  </header>
  {#if midi.clips.length === 0}
    <p class="silk empty">no MIDI clips yet</p>
  {:else}
    <ul>
      {#each midi.clips as clip (clip.id)}
        <li>
          <button
            class="row"
            class:on={surface.isClipPlaying(clip.id)}
            type="button"
            onclick={() => void surface.fireClip(clip.id)}
          >
            <span class="play">▶</span>
            <span class="name">{clip.name}</span>
          </button>
          <button
            class="drive"
            class:on={launch.drives(clip.id)}
            type="button"
            title="This clip drives a launcher when it plays"
            onclick={() => void surface.toggleDrive(clip.id)}
          >
            DRIVE
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .list {
    min-width: 180px;
    max-width: 240px;
    padding: 8px;
    border-radius: calc(var(--ctrl-radius) + 2px);
    background-color: var(--bg-1);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-frame), var(--relief-1);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow: auto;
    max-height: 220px;
  }
  li {
    display: flex;
    gap: 4px;
  }
  .row {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border: none;
    border-radius: var(--ctrl-radius);
    background: var(--bg-sunken);
    color: var(--text);
    cursor: pointer;
    box-shadow: var(--bevel-inset);
    text-align: left;
  }
  .row.on {
    color: var(--cyan);
    box-shadow: var(--bevel-raised);
  }
  .play {
    color: var(--cyan);
    font-size: 9px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .drive {
    font-family: var(--font-mono);
    font-size: 8px;
    letter-spacing: 0.12em;
    padding: 0 6px;
    border: var(--border-width) solid transparent;
    border-radius: var(--ctrl-radius);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .drive.on {
    color: var(--amber);
    border-color: var(--amber);
  }
  .empty {
    color: var(--text-dim);
    padding: 8px;
  }
  .kill {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .kill:hover {
    color: var(--red);
  }
</style>
