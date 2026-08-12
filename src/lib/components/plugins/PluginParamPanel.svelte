<script lang="ts">
  /**
   * Generic plugin parameter panel: renders whatever plugin_get_params
   * enumerates — sliders for continuous ranges, toggles for two-state
   * params, steppers/selects for small enums. Grouped by "Group / Name"
   * prefixes and collapsed by default so ZynAddSubFX-scale surfaces (130+
   * params) stay light: closed groups render zero param DOM (windowing),
   * inside one scroll container. Writes go through the store's rAF batch —
   * one plugin_set_param per frame regardless of drag rate.
   */
  import { plugins } from "../../state/plugins.svelte";
  import type { PluginParamInfo } from "../../types/ipc";

  interface ParamGroup {
    label: string;
    params: { p: PluginParamInfo; short: string }[];
  }

  const groups = $derived.by((): ParamGroup[] => {
    const out: ParamGroup[] = [];
    const index = new Map<string, ParamGroup>();
    for (const p of plugins.params) {
      const cut = p.name.indexOf(" / ");
      const label = cut > 0 ? p.name.slice(0, cut) : "parameters";
      const short = cut > 0 ? p.name.slice(cut + 3) : p.name;
      let g = index.get(label);
      if (!g) {
        g = { label, params: [] };
        index.set(label, g);
        out.push(g);
      }
      g.params.push({ p, short });
    }
    return out;
  });

  // Open the first group of a freshly loaded param list; keep closed groups
  // unrendered so huge plugins stay cheap.
  let open = $state<Record<string, boolean>>({});
  let openedFor = $state("");
  $effect(() => {
    const id = plugins.openInstanceId;
    const first = groups[0]?.label;
    if (id && id !== openedFor && first !== undefined) {
      openedFor = id;
      open = { [first]: true };
    }
  });

  const inst = $derived(plugins.openInstance);

  function isToggle(p: PluginParamInfo): boolean {
    return p.steps === 2 && p.max - p.min === 1;
  }
  function isEnum(p: PluginParamInfo): boolean {
    return (p.steps ?? 0) > 2 && (p.steps ?? 0) <= 12;
  }
  function sliderStep(p: PluginParamInfo): number {
    if ((p.steps ?? 0) > 1) return (p.max - p.min) / ((p.steps ?? 2) - 1);
    return (p.max - p.min) / 1000;
  }
  function enumValues(p: PluginParamInfo): number[] {
    const n = p.steps ?? 0;
    const step = (p.max - p.min) / (n - 1);
    return Array.from({ length: n }, (_, i) => p.min + i * step);
  }
  function pct(p: PluginParamInfo): number {
    return p.max === p.min ? 0 : ((p.value - p.min) / (p.max - p.min)) * 100;
  }
  function fmt(p: PluginParamInfo, v = p.value): string {
    const name = p.name.toLowerCase();
    if ((p.steps ?? 0) > 0) return String(Math.round(v));
    if (name.includes("cutoff") || name.includes("freq"))
      return v >= 1000 ? `${(v / 1000).toFixed(2)}k` : `${Math.round(v)}`;
    if (Math.abs(p.max) <= 1 && Math.abs(p.min) <= 1) return v.toFixed(2);
    return Math.abs(v) >= 100 ? v.toFixed(0) : v.toFixed(1);
  }
  function unitOf(p: PluginParamInfo): string {
    const name = p.name.toLowerCase();
    if ((p.steps ?? 0) > 0) return "";
    if (name.includes("cutoff") || name.includes("freq")) return "hz";
    if (name.includes("detune")) return "ct";
    if (name.includes("attack") || name.includes("release") || name.includes("decay")) return "s";
    if (name.includes("pan")) return "";
    if (p.min === 0 && p.max === 1) return "";
    return "";
  }

  function onSlide(p: PluginParamInfo, e: Event) {
    plugins.setParam(p.id, parseFloat((e.currentTarget as HTMLInputElement).value));
  }
  function onEnum(p: PluginParamInfo, e: Event) {
    plugins.setParam(p.id, parseFloat((e.currentTarget as HTMLSelectElement).value));
  }
</script>

<div class="panel">
  <div class="head">
    <button class="back mono" title="Back to the plugin browser" onclick={() => plugins.closeParams()}>
      ‹
    </button>
    <span class="title" title={inst?.uid}>{inst?.name ?? "plugin"}</span>
    {#if inst}
      <span class="badge mono {inst.format}">{inst.format}</span>
      <span class="status mono {inst.status}">{inst.status}</span>
    {/if}
  </div>

  {#if inst?.status === "stub"}
    <div class="note silk">dsp not hosted yet — parameters mirror into the registry</div>
  {/if}
  {#if plugins.paramError}
    <div class="err silk" role="alert">{plugins.paramError}</div>
  {/if}

  {#if plugins.paramsLoading}
    <div class="note silk">enumerating parameters…</div>
  {:else if plugins.params.length === 0 && !plugins.paramError}
    <div class="note silk">no parameters reported (stub instances enumerate after activation)</div>
  {/if}

  <div class="groups">
    {#each groups as g (g.label)}
      <section class="group">
        <button
          class="ghead mono"
          aria-expanded={!!open[g.label]}
          onclick={() => (open[g.label] = !open[g.label])}
        >
          <span class="arrow">{open[g.label] ? "▾" : "▸"}</span>
          <span class="glabel">{g.label}</span>
          <span class="gcount silk">{g.params.length}</span>
        </button>
        {#if open[g.label]}
          <div class="gbody">
            {#each g.params as { p, short } (p.id)}
              <div class="param">
                <div class="prow">
                  <span class="pname" title="{p.name} (id {p.id})">{short}</span>
                  {#if isToggle(p)}
                    <button
                      class="toggle mono"
                      class:on={p.value >= (p.min + p.max) / 2}
                      title="Toggle {short}"
                      onclick={() =>
                        plugins.setParam(p.id, p.value >= (p.min + p.max) / 2 ? p.min : p.max)}
                    >
                      {p.value >= (p.min + p.max) / 2 ? "ON" : "OFF"}
                    </button>
                  {:else if isEnum(p)}
                    <select class="enum mono" value={p.value} onchange={(e) => onEnum(p, e)}>
                      {#each enumValues(p) as v, i (i)}
                        <option value={v}>{fmt(p, v)}</option>
                      {/each}
                    </select>
                  {:else}
                    <span class="val mono">{fmt(p)}<span class="unit">{unitOf(p)}</span></span>
                  {/if}
                </div>
                {#if !isToggle(p) && !isEnum(p)}
                  <input
                    class="fader"
                    type="range"
                    min={p.min}
                    max={p.max}
                    step={sliderStep(p)}
                    value={p.value}
                    style:--fader-pct="{pct(p)}%"
                    style:--fader-fill="var(--cyan)"
                    aria-label="{p.name} ({fmt(p)}{unitOf(p)})"
                    title="{p.name} — double-click resets to {fmt(p, p.default)}"
                    oninput={(e) => onSlide(p, e)}
                    ondblclick={() => plugins.resetParam(p)}
                  />
                {/if}
              </div>
            {/each}
          </div>
        {/if}
      </section>
    {/each}
  </div>
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 1;
    min-height: 0;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }
  .back {
    width: 24px;
    height: 24px;
    line-height: 1;
    font-size: 15px;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.6);
    color: var(--text-dim);
    cursor: pointer;
  }
  .back:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .title {
    flex: 1;
    min-width: 0;
    font-size: 13px;
    font-weight: 500;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .badge {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 5px;
    border-radius: 3px;
    border: 1px solid;
  }
  .badge.clap {
    color: var(--cyan);
    border-color: rgba(82, 229, 255, 0.4);
  }
  .badge.lv2 {
    color: var(--violet);
    border-color: rgba(157, 123, 255, 0.4);
  }
  .status {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 3px;
  }
  .status.stub {
    color: var(--amber);
    background: rgba(255, 200, 87, 0.1);
  }
  .status.active {
    color: #5cf2b8;
    background: rgba(92, 242, 184, 0.1);
  }
  .status.crashed {
    color: var(--red);
    background: rgba(255, 65, 82, 0.12);
  }

  .note {
    flex: none;
  }
  .err {
    color: var(--red);
    flex: none;
  }

  .groups {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-right: 2px;
  }

  .group {
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    background: rgba(10, 13, 23, 0.55);
  }
  .ghead {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    border: none;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .ghead:hover {
    color: var(--cyan);
  }
  .arrow {
    width: 10px;
    color: var(--text-faint);
  }
  .glabel {
    flex: 1;
    text-align: left;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .gcount {
    letter-spacing: 0.1em;
  }

  .gbody {
    display: flex;
    flex-direction: column;
    gap: 7px;
    padding: 2px 10px 10px;
  }

  .param {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .prow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pname {
    flex: 1;
    min-width: 0;
    font-size: 10px;
    color: var(--text-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .val {
    font-size: 10px;
    color: var(--cyan);
  }
  .unit {
    margin-left: 2px;
    font-size: 8px;
    color: var(--text-faint);
    text-transform: uppercase;
  }

  .toggle {
    font-size: 8px;
    letter-spacing: 0.14em;
    padding: 3px 8px;
    border-radius: 3px;
    border: 1px solid rgba(122, 160, 220, 0.2);
    background: rgba(5, 7, 13, 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .toggle.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 8px rgba(82, 229, 255, 0.35);
  }

  .enum {
    background: rgba(5, 7, 13, 0.7);
    color: var(--text-dim);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 2px 6px;
    max-width: 110px;
  }

  input[type="range"].fader {
    width: 100%;
  }
</style>
