# Plugin manager — implementation plan

Design: [`2026-08-20-plugin-manager-design.md`](../specs/2026-08-20-plugin-manager-design.md)
Branch: `feat/plugin-manager` · Worktree: `.worktrees/plugin-manager`

## Status

| PR | Scope | State |
|---|---|---|
| 1 | Playhead follow on seek (`view.revealSamples`) | **done** — `6b259b4` |
| 2 | Lane multi-select + group/global M/S/A | in flight |
| 3 | Backend catalog, persistent + incremental scan cache | in flight |
| 4 | `browser/` primitives + migrate five browsers | in flight |
| 5 | Plugin Manager: browse/rack modes + quick picker | blocked on 3, 4 |
| 6 | Automation matrix, pinned params, lane-info indicator | blocked on 4, 5 |

## PR 5 — the Plugin Manager

### 5.1 The rack projection — `src/lib/utils/plugin-rack.ts`

Pure and tested. The rack is derived, never stored.

```ts
interface RackEntry {
  instance: PluginInstanceInfo;
  descriptor?: PluginDescriptor;    // undefined = instance of an unscanned uid
  placements: Placement[];          // where it sits, possibly more than one
  automated: ParamRef[];            // params with a modulation curve
  mapped: ParamRef[];               // params bound to a MIDI controller (empty for now)
}
type Placement =
  | { kind: "instrument"; trackId: string; trackName: string }
  | { kind: "insert"; trackId: string; trackName: string; slotIndex: number; bypassed: boolean };
```

Inputs: `plugins.instances`, `plugins.descriptors`, `project.tracks`, `modulation` bindings.
Join rules: `track.instrumentId === "plugin:<id>"` → instrument placement;
`track.inserts[]` with `slot.instanceId === id` → insert placement at that index;
a binding whose target is `{ kind: "pluginParam", instanceId, paramId }` → automated.

An instance with **zero** placements is an orphan — the rack must surface those,
because they are exactly what nothing else in the app will ever show you.

### 5.2 Modes

`PluginManager.svelte` owns a `mode: "browse" | "rack"` and renders
`BrowserShell` either way — same shell, different body. The mode lives in
`localStorage`, not the document.

**Browse.** Sections in this order, each collapsible:
`★ Favourites` · `⏱ Recent` · `Instruments` (sub-grouped by descriptor
category) · `Effects` (sub-grouped) · `Zyn patches` · `Uncategorised`.
A descriptor with no `categories[]` goes to `Uncategorised` — do not
invent a category for it. Row actions: add to selected track, add as
insert, favourite, tag.

**Rack.** One row per `RackEntry`, grouped by track (orphans last, under
`Not on any track`). Each row: status dot, name, format badge, placement,
and its parameter chips. Row actions: open params, bypass, remove.
Footer counts stubs and crashed instances — a crashed plugin is the single
most useful thing this view can tell you, so it must not be buried.

### 5.3 Quick picker — `PluginQuickPick.svelte`

`Ctrl+P`. Narrow centred overlay, one input, ranked list. Ranking:
favourites first, then recents by recency, then `fuzzyScore`. `Enter`
adds to the selected track as its instrument; `Shift+Enter` adds as an
insert; `Esc` closes. Registers through the existing dock/shortcut
mechanism (see `DOCK_SHORTCUT` in `src/lib/state/ui.svelte.ts`) — do not
add a second global keydown listener.

## PR 6 — automation as an inventory

### 6.1 The matrix — `AutomationMatrix.svelte`

The same projection as 5.1, grouped by parameter instead of by instance,
plus track-param curves (gain, pan) so it is genuinely *everything that
moves in this project*. Columns: track, plugin, parameter, current value,
lane visible?, mode. Clicking a row reveals the lane and scrolls to it.

### 6.2 Param chips in the parameter panel

`PluginParamPanel.svelte` renders each param as a `ParamChip` alongside its
fader. Pinned params (`catalog.pinnedParams[uid]`, max 8) sort to the top
under a `Pinned` heading. A chip on an unautomated param offers "Create
automation lane"; on an automated one it jumps to the lane.

### 6.3 The lane-info indicator

`TrackHeader.svelte`'s FX chip becomes a plugin strip: the instrument (if
any) and each insert as a compact entry with its status dot and its pinned
param chips inline. That is the "easy to keep an overview and jump to the
parameter you want to automate" ask — the chips ARE the jump targets.

Constraint: the header is already dense and `--track-height` is 132 px.
The strip must degrade gracefully on a folded lane and on a short track
height; chips overflow to a `+3` affordance rather than wrapping the row.

## Global constraints for every PR

- Theme tokens only in `<style>`; `no-literals.test.ts` is CI-enforced.
- Svelte 5 runes; no `export let`, no Svelte 4 slots.
- Pure logic in `utils/` with `*.test.ts`; component behaviour in
  `*.dom.test.ts` — and read the jsdom pitfalls in
  [`2026-08-18-dom-test-environment.md`](2026-08-18-dom-test-environment.md)
  first, every time.
- Gates: `npx vitest run`, `npx svelte-check --threshold error`,
  `npm run build`, and `cd src-tauri && cargo test`.
