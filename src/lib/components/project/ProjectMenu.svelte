<script lang="ts">
  /**
   * The TransportBar PROJECT block: New / Open / Save, recent history, and
   * Preferences. Actions live in the projectops store; this is only the
   * affordance.
   */
  import { prefs } from "../../prefs/prefs.svelte";
  import { project } from "../../state/project.svelte";
  import { projectops } from "../../state/projectops.svelte";
  import { recentProjectLabel } from "../../utils/last-project";

  let open = $state(false);

  const recent = $derived(
    projectops.available
      ? projectops.recent.slice(0, prefs.values.recentProjectsMax)
      : [],
  );

  function toggle() {
    if (!open) projectops.refreshRecent();
    open = !open;
  }

  function run(action: () => void) {
    open = false;
    action();
  }
</script>

<div class="pm">
  <button
    class="projbtn"
    class:open
    title="Project menu — new / open / save / recent"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={toggle}
  >
    <span class="silk">project ▾</span>
    <span class="pname">{project.name}</span>
  </button>

  {#if open}
    <div class="backdrop" role="presentation" onpointerdown={() => (open = false)}></div>
    <div class="menu glass mono" role="menu" aria-label="Project">
      <button role="menuitem" onclick={() => run(() => projectops.requestNew())}>
        NEW <span class="kbd">ctrl+n</span>
      </button>
      <button role="menuitem" onclick={() => run(() => projectops.requestOpen())}>
        OPEN… <span class="kbd">ctrl+o</span>
      </button>
      <button role="menuitem" onclick={() => run(() => void projectops.save())}>
        SAVE <span class="kbd">ctrl+s</span>
      </button>
      {#if recent.length}
        <div class="sect silk">RECENT</div>
        {#each recent as path (path)}
          {@const current = path === project.projectDir}
          <button
            role="menuitem"
            class="recent"
            disabled={current}
            title={path}
            onclick={() => run(() => void projectops.requestOpenRecent(path))}
          >
            <span class="rname">{recentProjectLabel(path)}</span>
            {#if current}<span class="kbd">open</span>{/if}
          </button>
        {/each}
      {/if}
      <button role="menuitem" onclick={() => run(() => (prefs.dialogOpen = true))}>
        PREFERENCES… <span class="kbd">ctrl+,</span>
      </button>
      {#if project.projectDir}
        <div class="dir silk" title={project.projectDir}>{project.projectDir}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .pm {
    position: relative;
  }
  .projbtn {
    display: flex;
    flex-direction: column;
    gap: 1px;
    align-items: flex-start;
    background: transparent;
    border: none;
    padding: 2px 6px;
    margin: -2px -6px;
    border-radius: 5px;
    cursor: pointer;
    text-align: left;
  }
  .projbtn:hover,
  .projbtn.open {
    background: rgb(var(--cyan-rgb) / 0.07);
  }
  .pname {
    font-size: 12px;
    color: var(--text-dim);
  }
  .projbtn:hover .pname {
    color: var(--text);
  }

  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 90;
    min-width: 220px;
    max-width: 320px;
    display: flex;
    flex-direction: column;
    padding: 5px;
    border-radius: 7px;
    background: rgb(var(--bg-sunken-rgb) / 0.96);
  }
  .menu button {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 14px;
    min-width: 0;
    max-width: 100%;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 7px 9px;
    font-size: 10px;
    letter-spacing: 0.16em;
    color: var(--text-dim);
    cursor: pointer;
  }
  .menu button:hover:not(:disabled) {
    background: rgb(var(--cyan-rgb) / 0.09);
    color: var(--cyan);
  }
  .menu button:disabled {
    cursor: default;
    opacity: 0.55;
  }
  .kbd {
    font-size: 9px;
    letter-spacing: 0.08em;
    color: var(--text-faint);
  }
  .sect {
    padding: 8px 9px 2px;
    border-top: 1px solid var(--glass-border);
    margin-top: 4px;
    font-size: 8px;
    letter-spacing: 0.2em;
  }
  .recent {
    letter-spacing: 0.04em;
    text-transform: none;
  }
  .rname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dir {
    padding: 6px 9px 4px;
    border-top: 1px solid var(--glass-border);
    margin-top: 4px;
    font-size: 9px;
    text-transform: none;
    letter-spacing: 0.02em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 300px;
  }
</style>
