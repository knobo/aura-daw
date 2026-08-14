<script lang="ts">
  /**
   * Preferences dialog (ExportDialog idiom), rendered entirely from
   * PREF_SCHEMA: categories in declaration order, one control per entry —
   * toggle for booleans, segmented buttons for enums, slider for numbers.
   * Add a preference to the schema and it appears here; nothing in this
   * component is per-preference.
   */
  import { prefs } from "../../prefs/prefs.svelte";
  import { PREF_CATEGORIES, PREF_SCHEMA, type PrefDef, type PrefId } from "../../prefs/schema";

  const entries = Object.entries(PREF_SCHEMA) as [PrefId, PrefDef][];

  function inCategory(cat: string): [PrefId, PrefDef][] {
    return entries.filter(([, def]) => def.category === cat);
  }

  /** The number control's visible value — percent for "%" defs, raw otherwise. */
  function numberLabel(def: PrefDef, value: number): string {
    if (def.kind !== "number") return String(value);
    return def.unit === "%" ? `${Math.round(value * 100)}%` : String(value);
  }

  function onOverlayKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") prefs.dialogOpen = false;
  }
</script>

{#if prefs.dialogOpen}
  <div
    class="overlay"
    role="presentation"
    onkeydown={onOverlayKeydown}
    onpointerdown={(e) => {
      if (e.target === e.currentTarget) prefs.dialogOpen = false;
    }}
  >
    <div class="dialog glass" role="dialog" aria-modal="true" aria-label="Preferences">
      <div class="head">
        <span class="title mono">⚙ PREFERENCES</span>
        <span class="sub silk">saved on this machine</span>
        <button class="x mono" title="Close (Esc)" onclick={() => (prefs.dialogOpen = false)}>×</button>
      </div>

      {#each PREF_CATEGORIES as cat (cat.id)}
        <section class="cat">
          <h3 class="cathead mono">{cat.label}</h3>
          {#each inCategory(cat.id) as [id, def] (id)}
            <div class="pref">
              <div class="prefhead">
                <span class="preflabel">{def.label}</span>

                {#if def.kind === "boolean"}
                  <button
                    class="toggle mono"
                    class:on={prefs.values[id] === true}
                    role="switch"
                    aria-checked={prefs.values[id] === true}
                    aria-label={def.label}
                    onclick={() => prefs.set(id, !prefs.values[id])}
                  >
                    {prefs.values[id] ? "ON" : "OFF"}
                  </button>
                {:else if def.kind === "enum"}
                  <div class="seg" role="radiogroup" aria-label={def.label}>
                    {#each def.options as opt (opt.value)}
                      <button
                        class="segbtn mono"
                        class:on={prefs.values[id] === opt.value}
                        role="radio"
                        aria-checked={prefs.values[id] === opt.value}
                        onclick={() => prefs.set(id, opt.value)}
                      >
                        {opt.label}
                      </button>
                    {/each}
                  </div>
                {:else if def.kind === "number"}
                  <div class="numctl">
                    <input
                      type="range"
                      min={def.min}
                      max={def.max}
                      step={def.step}
                      value={prefs.values[id] as number}
                      aria-label={def.label}
                      oninput={(e) => prefs.set(id, parseFloat(e.currentTarget.value))}
                    />
                    <span class="numval mono">{numberLabel(def, prefs.values[id] as number)}</span>
                  </div>
                {/if}
              </div>
              <p class="blurb silk">{def.blurb}</p>
            </div>
          {/each}
        </section>
      {/each}

      <div class="foot">
        <button class="resetall mono" onclick={() => prefs.restoreDefaults()}>RESET ALL TO DEFAULTS</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 80;
    display: grid;
    place-items: center;
    background: rgba(3, 4, 8, 0.6);
    backdrop-filter: blur(3px);
  }
  .dialog {
    width: 460px;
    max-width: calc(100vw - 40px);
    max-height: calc(100vh - 60px);
    overflow-y: auto;
    border-radius: 9px;
    padding: 14px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
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

  .cat {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .cathead {
    margin: 0;
    font-size: 9px;
    letter-spacing: 0.24em;
    color: var(--text-faint);
    border-bottom: 1px solid var(--glass-border);
    padding-bottom: 4px;
  }

  .pref {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .prefhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }
  .preflabel {
    font-size: 11px;
    color: var(--text);
  }
  .blurb {
    margin: 0;
    font-size: 9px;
    text-transform: none;
    letter-spacing: 0.03em;
    color: var(--text-faint);
  }

  .toggle {
    min-width: 46px;
    padding: 4px 0;
    font-size: 9px;
    letter-spacing: 0.16em;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.4);
    color: var(--text-dim);
    cursor: pointer;
    transition: border-color 120ms, color 120ms, box-shadow 120ms;
  }
  .toggle.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.18);
  }

  .seg {
    display: flex;
    gap: 5px;
  }
  .segbtn {
    padding: 4px 8px;
    font-size: 9px;
    letter-spacing: 0.12em;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.4);
    color: var(--text-dim);
    cursor: pointer;
    transition: border-color 120ms, color 120ms, box-shadow 120ms;
  }
  .segbtn.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.18);
  }

  .numctl {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .numctl input[type="range"] {
    width: 140px;
    accent-color: var(--cyan);
  }
  .numval {
    min-width: 38px;
    text-align: right;
    font-size: 10px;
    color: var(--text-dim);
  }

  .foot {
    display: flex;
    justify-content: flex-end;
    border-top: 1px solid var(--glass-border);
    padding-top: 10px;
  }
  .resetall {
    padding: 6px 10px;
    font-size: 9px;
    letter-spacing: 0.16em;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .resetall:hover {
    color: var(--amber);
    border-color: var(--amber);
  }
</style>
