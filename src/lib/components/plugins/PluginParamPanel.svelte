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
  import { modulation } from "../../state/modulation.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import type { PluginParamInfo, TargetRef } from "../../types/ipc";
  import { revealParamLane } from "../../utils/lane-reveal";
  import {
    formatParamDisplay,
    formatParamValue,
    paramGroupName,
    paramNormalized,
    paramUnit,
    shortParamName,
  } from "../../utils/plugin-params";
  import ParamChip from "../browser/ParamChip.svelte";
  import PluginConnectionBadge from "./PluginConnectionBadge.svelte";

  interface ParamGroup {
    label: string;
    params: { p: PluginParamInfo; short: string }[];
  }

  // The Pinned section joins the same `open` record as the real plugin
  // groups (§6.2's "same visual language"), so its key must not collide
  // with a real "Group / Name" prefix — those never start with a space,
  // so a leading space is a cheap, readable way to reserve this one.
  const PINNED_GROUP = " pinned";

  const groups = $derived.by((): ParamGroup[] => {
    const out: ParamGroup[] = [];
    const index = new Map<string, ParamGroup>();
    for (const p of plugins.params) {
      const label = paramGroupName(p.name);
      const short = shortParamName(p.name);
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

  // Pinned params, in pin order, with a stale pin (from another build of
  // the same plugin) silently dropped rather than rendered as a blank row.
  const pinnedParams = $derived.by((): { p: PluginParamInfo; short: string }[] => {
    if (!inst) return [];
    const byId = new Map(plugins.params.map((p) => [p.id, p]));
    const out: { p: PluginParamInfo; short: string }[] = [];
    for (const id of plugins.pinnedParamsFor(inst.uid)) {
      const p = byId.get(id);
      if (p) out.push({ p, short: shortParamName(p.name) });
    }
    return out;
  });

  function isPinned(paramId: number): boolean {
    return !!inst && plugins.pinnedParamsFor(inst.uid).includes(paramId);
  }
  function togglePin(p: PluginParamInfo) {
    if (!inst) return;
    const current = plugins.pinnedParamsFor(inst.uid);
    if (current.includes(p.id)) {
      plugins.setPinnedParams(
        inst.uid,
        current.filter((id) => id !== p.id),
      );
      return;
    }
    // The store caps at 8 and would just truncate silently — tell the user
    // instead of letting a ninth click vanish with no feedback.
    if (current.length >= 8) {
      toasts.error("PINNED FULL", "Unpin a parameter first — the strip holds 8.");
      return;
    }
    plugins.setPinnedParams(inst.uid, [...current, p.id]);
  }

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
    return paramNormalized(p) * 100;
  }
  function fmt(p: PluginParamInfo, v = p.value): string {
    return formatParamValue(p, v);
  }
  function unitOf(p: PluginParamInfo): string {
    return paramUnit(p);
  }

  function onSlide(p: PluginParamInfo, e: Event) {
    plugins.setParam(p.id, parseFloat((e.currentTarget as HTMLInputElement).value));
  }
  function onEnum(p: PluginParamInfo, e: Event) {
    plugins.setParam(p.id, parseFloat((e.currentTarget as HTMLSelectElement).value));
  }
  function pluginBound(paramId: number): boolean {
    return (
      modulation.bindingsFor({
        kind: "pluginParam",
        instanceId: plugins.openInstanceId,
        paramId,
      }).length > 0
    );
  }
  // The `A` button toggles the modulation-picker overlay (click again to
  // hide it) — the right behaviour for a persistent per-row button, and
  // already asserted by this file's dom test. The chip below instead
  // *jumps* to the lane, always ending with it visible; that's a different
  // gesture (`revealParamLane`, shared with the automation matrix), not a
  // "unify these two buttons" opportunity.
  function automateParam(p: PluginParamInfo) {
    const inst = plugins.openInstance;
    const n = p.max === p.min ? 0 : (p.value - p.min) / (p.max - p.min);
    void modulation.pickTarget(
      inst?.trackId ?? "",
      { kind: "pluginParam", instanceId: plugins.openInstanceId, paramId: p.id },
      n,
    );
  }

  function jumpToLane(p: PluginParamInfo) {
    const target: TargetRef = { kind: "pluginParam", instanceId: plugins.openInstanceId, paramId: p.id };
    void revealParamLane(plugins.openInstance?.trackId ?? "", target, paramNormalized(p));
  }
  function chipTitle(p: PluginParamInfo, short: string): string {
    return pluginBound(p.id)
      ? `Jump to the automation lane for ${short}`
      : `Create automation lane for ${short}`;
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
      {#if plugins.hasGui(inst.id)}
        <button
          type="button"
          class="guibtn mono"
          title="Open native plugin GUI"
          onclick={() => void plugins.showGui(inst.id)}
        >GUI</button>
      {/if}
    {/if}
  </div>
  {#if inst}
    <div class="connrow">
      <PluginConnectionBadge instanceId={inst.id} />
    </div>
  {/if}

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

  {#snippet paramRow(p: PluginParamInfo, short: string)}
    <div class="param">
      <div class="prow">
        <div class="chipwrap">
          <ParamChip
            label={short}
            value={p.value}
            format={() => formatParamDisplay(p)}
            state={pluginBound(p.id) ? "automated" : "plain"}
            title={chipTitle(p, short)}
            onclick={() => jumpToLane(p)}
          />
        </div>
        {#if isToggle(p)}
          <button
            class="toggle mono"
            class:on={p.value >= (p.min + p.max) / 2}
            title="Toggle {short}"
            onclick={() => plugins.setParam(p.id, p.value >= (p.min + p.max) / 2 ? p.min : p.max)}
          >
            {p.value >= (p.min + p.max) / 2 ? "ON" : "OFF"}
          </button>
        {:else if isEnum(p)}
          <select class="enum mono" value={p.value} onchange={(e) => onEnum(p, e)}>
            {#each enumValues(p) as v, i (i)}
              <option value={v}>{fmt(p, v)}</option>
            {/each}
          </select>
        {/if}
        <button
          class="pinbtn mono"
          class:on={isPinned(p.id)}
          aria-pressed={isPinned(p.id)}
          aria-label={isPinned(p.id) ? `Unpin ${short}` : `Pin ${short}`}
          onclick={() => togglePin(p)}
          >{isPinned(p.id) ? "★" : "☆"}</button
        >
        <button
          class="autobtn mono"
          class:on={pluginBound(p.id)}
          title="Automate {p.name}"
          aria-pressed={pluginBound(p.id)}
          onclick={() => automateParam(p)}
          >A</button
        >
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
          onpointerdown={() => plugins.beginParamGesture()}
          onpointerup={() => void plugins.endParamGesture()}
          onpointercancel={() => void plugins.endParamGesture()}
          ondblclick={() => plugins.resetParam(p)}
        />
      {/if}
    </div>
  {/snippet}

  <div class="groups">
    {#if pinnedParams.length > 0}
      {@const pinnedOpen = open[PINNED_GROUP] ?? true}
      <section class="group">
        <button
          class="ghead mono"
          aria-expanded={pinnedOpen}
          onclick={() => (open[PINNED_GROUP] = !pinnedOpen)}
        >
          <span class="arrow">{pinnedOpen ? "▾" : "▸"}</span>
          <span class="glabel">Pinned</span>
          <span class="gcount silk">{pinnedParams.length}</span>
        </button>
        {#if pinnedOpen}
          <div class="gbody">
            {#each pinnedParams as { p, short } (p.id)}
              {@render paramRow(p, short)}
            {/each}
          </div>
        {/if}
      </section>
    {/if}
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
              {@render paramRow(p, short)}
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
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-2-rgb) / 0.6);
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
  .connrow {
    flex: none;
  }

  .badge {
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    padding: 2px 5px;
    border-radius: 3px;
    border: var(--border-width) solid;
  }
  .badge.clap {
    color: var(--cyan);
    border-color: rgb(var(--cyan-rgb) / 0.4);
  }
  .badge.lv2 {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.4);
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
    background: rgb(var(--amber-rgb) / 0.1);
  }
  .status.active {
    color: var(--green);
    background: rgb(var(--green-rgb) / 0.1);
  }
  .status.crashed {
    color: var(--red);
    background: rgb(var(--red-rgb) / 0.12);
  }

  .guibtn {
    flex: none;
    padding: 2px 6px;
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    border-radius: 3px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-2-rgb) / 0.6);
    color: var(--text-dim);
    cursor: pointer;
  }
  .guibtn:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
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
    border: var(--border-width) solid var(--glass-border);
    border-radius: 6px;
    background: rgb(var(--bg-1-rgb) / 0.55);
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
  /* Wraps the ParamChip so it grows to fill the row (the chip itself has
     no opinion on flex-basis) and pushes the toggle/enum/pin/A controls
     to the right, the same job `.pname` used to do. */
  .chipwrap {
    flex: 1;
    min-width: 0;
  }

  .toggle {
    font-size: 8px;
    letter-spacing: 0.14em;
    padding: 3px 8px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.2);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .toggle.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.35);
  }

  /* "automate this knob" toggle — same small-button language as `.toggle`,
     violet so it reads as a lane affordance, not a param value. */
  .autobtn {
    flex: none;
    width: 16px;
    height: 16px;
    line-height: 1;
    font-size: 8px;
    letter-spacing: 0;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.2);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .autobtn:hover {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.4);
  }
  .autobtn.on {
    color: var(--bg-0);
    background: var(--violet);
    border-color: var(--violet);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--violet-rgb) / 0.35);
  }

  /* Pin toggle — same small-button language as `.autobtn`, amber for the
     pinned state (the catalog's favourite-star colour elsewhere). */
  .pinbtn {
    flex: none;
    width: 16px;
    height: 16px;
    line-height: 1;
    font-size: 9px;
    letter-spacing: 0;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.2);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .pinbtn:hover {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.4);
  }
  .pinbtn.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.35);
  }

  .enum {
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 2px 6px;
    max-width: 110px;
  }

  input[type="range"].fader {
    width: 100%;
  }
</style>
