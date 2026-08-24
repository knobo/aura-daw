<script lang="ts">
  /**
   * Output picker (Plan G2): where this track's fader output GOES.
   *
   * An output is a MOVE, not a copy — routing a track into a bus takes it
   * off the master. That is the difference between this and the sends rack
   * next to it, and the popover says so out loud, because "why do I hear it
   * twice" is exactly the confusion that made this control exist.
   */
  import type { TrackState } from "../../types/ipc";
  import { project } from "../../state/project.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { backend } from "../../tauri";

  let {
    track,
    onclose,
  }: {
    track: TrackState;
    onclose: () => void;
  } = $props();

  /** Buses this track could feed without closing a loop. Mirrors the
   * backend's `bus::would_cycle` so the UI never offers a choice the commit
   * will refuse — the backend is still the authority, this is just manners. */
  const choices = $derived(
    project.tracks.filter((b) => b.kind === "bus" && b.id !== track.id && !reaches(b.id, track.id)),
  );

  /** Does `from` already reach `to` through outputs or sends? */
  function reaches(from: string, to: string): boolean {
    const seen = new Set<string>();
    const stack = [from];
    while (stack.length) {
      const id = stack.pop()!;
      if (id === to) return true;
      if (seen.has(id)) continue;
      seen.add(id);
      const t = project.tracks.find((x) => x.id === id);
      if (!t) continue;
      if (t.output) stack.push(t.output);
      for (const s of t.sends ?? []) stack.push(s.dest);
    }
    return false;
  }

  let busy = $state(false);

  async function route(output: string | null) {
    busy = true;
    try {
      await backend.trackSetOutput?.(track.id, output);
      await project.reload();
      onclose();
    } catch (err) {
      console.error("[aura] track_set_output failed:", err);
      toasts.error("ROUTING FAILED", String(err));
    } finally {
      busy = false;
    }
  }
</script>

<div class="backdrop" role="presentation" onpointerdown={onclose}></div>
<div class="popover glass mono" role="menu" aria-label="Output for {track.name}">
  <div class="pophead">
    <span class="ptitle">Output</span>
  </div>
  <span class="blurb">Moves the track. A send copies it instead.</span>

  <button
    class="dest"
    class:on={!track.output}
    disabled={busy}
    onclick={() => route(null)}
  >
    Master
  </button>
  {#each choices as b (b.id)}
    <button
      class="dest"
      class:on={track.output === b.id}
      disabled={busy}
      onclick={() => route(b.id)}
    >
      <span class="bbadge mono">bus</span>
      <span class="bname">{b.name}</span>
    </button>
  {/each}
  {#if choices.length === 0}
    <span class="empty-silk">No bus available — add one with + BUS</span>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .popover {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 170px;
    max-width: 240px;
    max-height: 260px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px;
    border-radius: 7px;
    background: rgb(var(--bg-sunken-rgb) / 0.96);
    border: var(--border-width) solid var(--glass-border);
  }
  .pophead {
    display: flex;
    align-items: center;
    margin-bottom: 2px;
  }
  .ptitle {
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text-dim);
  }
  .blurb {
    font-size: 8px;
    line-height: 1.35;
    color: var(--text-faint);
    padding-bottom: 4px;
    margin-bottom: 2px;
    border-bottom: var(--border-width) solid var(--glass-border);
  }
  .dest {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 4px 6px;
    font-size: 10px;
    color: var(--text);
    cursor: pointer;
    text-align: left;
  }
  .dest:hover:not(:disabled) {
    background: rgb(var(--amber-rgb) / 0.1);
    color: var(--amber);
  }
  .dest.on {
    color: var(--bg-0);
    background: var(--amber);
  }
  .dest:disabled {
    cursor: wait;
    opacity: 0.5;
  }
  .bbadge {
    flex: none;
    font-size: 7px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 1px 3px;
    border-radius: 2px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.3);
  }
  .bname {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .empty-silk {
    font-size: 9px;
    color: var(--text-faint);
    padding: 4px 0;
  }
</style>
