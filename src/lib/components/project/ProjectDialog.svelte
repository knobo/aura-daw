<script lang="ts">
  /**
   * Project dialogs (ExportDialog idiom): the name/location dialog for
   * "New Project" and "Save Project As", and the unsaved-changes
   * confirmation shown before New/Open replaces session content.
   */
  import { project } from "../../state/project.svelte";
  import { projectops } from "../../state/projectops.svelte";

  const dialog = $derived(projectops.dialog);

  async function browseParent() {
    const picked = await projectops.pickDirectory();
    if (picked && projectops.dialog) {
      projectops.dialog = { ...projectops.dialog, parentDir: picked };
    }
  }

  function onDialogKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" && !projectops.busy) projectops.dialog = null;
    if (e.key === "Enter") void projectops.submitDialog();
  }
</script>

{#if projectops.confirm}
  <div class="overlay" role="presentation">
    <div class="dialog glass" role="alertdialog" aria-modal="true" aria-label="Unsaved changes">
      <div class="head">
        <span class="title mono">UNSAVED CHANGES</span>
      </div>
      <p class="body silk">
        Track, clip, and mixer changes since the last save will be lost.
      </p>
      <div class="actions">
        <button class="btn mono" onclick={() => projectops.cancelConfirm()}>CANCEL</button>
        <button class="btn mono" onclick={() => void projectops.proceedConfirmed(false)}>
          CONTINUE WITHOUT SAVING
        </button>
        <button class="btn primary mono" onclick={() => void projectops.proceedConfirmed(true)}>
          {project.projectDir ? "SAVE, THEN CONTINUE" : "SAVE FIRST…"}
        </button>
      </div>
    </div>
  </div>
{/if}

{#if dialog}
  <div
    class="overlay"
    role="presentation"
    onkeydown={onDialogKeydown}
    onpointerdown={(e) => {
      if (e.target === e.currentTarget && !projectops.busy) projectops.dialog = null;
    }}
  >
    <div class="dialog glass" role="dialog" aria-modal="true" aria-label="Project name and location">
      <div class="head">
        <span class="title mono">
          {dialog.mode === "new" ? "◇ NEW PROJECT" : "◈ SAVE PROJECT AS"}
        </span>
        <span class="sub silk">
          {dialog.mode === "new" ? "blank slate" : "save the current session"}
        </span>
        <button class="x mono" title="Close" onclick={() => (projectops.dialog = null)}>×</button>
      </div>

      <label class="field">
        <span class="silk">name</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="text"
          class="mono"
          autofocus
          spellcheck="false"
          disabled={projectops.busy}
          value={dialog.name}
          oninput={(e) => {
            if (projectops.dialog) {
              projectops.dialog = { ...projectops.dialog, name: e.currentTarget.value };
            }
          }}
        />
      </label>

      <label class="field">
        <span class="silk">location</span>
        <div class="row">
          <input
            type="text"
            class="mono pathin"
            spellcheck="false"
            disabled={projectops.busy}
            value={dialog.parentDir}
            oninput={(e) => {
              if (projectops.dialog) {
                projectops.dialog = { ...projectops.dialog, parentDir: e.currentTarget.value };
              }
            }}
          />
          <button class="btn mono" disabled={projectops.busy} onclick={browseParent}>
            BROWSE…
          </button>
        </div>
        <span class="hintline silk">creates {dialog.parentDir}/{dialog.name || "…"}.aura</span>
      </label>

      <div class="actions">
        <button class="btn mono" disabled={projectops.busy} onclick={() => (projectops.dialog = null)}>
          CANCEL
        </button>
        <button
          class="btn primary mono"
          disabled={projectops.busy || !dialog.name.trim()}
          onclick={() => void projectops.submitDialog()}
        >
          {dialog.mode === "new" ? "CREATE" : "SAVE"}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 85;
    display: grid;
    place-items: center;
    background: rgba(3, 4, 8, 0.6);
    backdrop-filter: blur(3px);
  }
  .dialog {
    width: 420px;
    max-width: calc(100vw - 40px);
    border-radius: 9px;
    padding: 14px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 11px;
    background: rgba(8, 10, 19, 0.94);
    animation: dialog-in 160ms ease-out;
  }
  @keyframes dialog-in {
    from {
      transform: translateY(8px);
      opacity: 0;
    }
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .title {
    font-size: 12px;
    letter-spacing: 0.22em;
    color: var(--cyan);
    text-shadow: 0 0 10px var(--cyan-dim);
  }
  .sub {
    flex: 1;
  }
  .x {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 14px;
  }
  .x:hover {
    color: var(--red);
  }
  .body {
    margin: 0;
    text-transform: none;
    letter-spacing: 0.03em;
    font-size: 11px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .row {
    display: flex;
    gap: 8px;
  }
  .row .pathin {
    flex: 1;
    font-size: 10px;
  }
  input[type="text"] {
    background: rgba(5, 7, 13, 0.7);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 6px 8px;
  }
  input:focus {
    border-color: var(--cyan-dim);
  }
  .hintline {
    font-size: 9px;
    text-transform: none;
    letter-spacing: 0.02em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    flex-wrap: wrap;
  }
  .btn {
    padding: 7px 12px;
    font-size: 10px;
    letter-spacing: 0.14em;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.4);
    color: var(--text-dim);
    cursor: pointer;
  }
  .btn:hover:not(:disabled) {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .btn.primary {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.18);
  }
  .btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
</style>
