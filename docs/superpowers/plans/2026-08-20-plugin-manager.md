# Plugin manager — implementation plan

Design: [`2026-08-20-plugin-manager-design.md`](../specs/2026-08-20-plugin-manager-design.md)
Branch: `feat/plugin-manager` · Worktree: `.worktrees/plugin-manager`

## Status

Squash-merged to `main` as PR #93 (`e1ec61f`, 2026-08-21). Branch from
`origin/main`. Do not reopen `feat/plugin-manager`.

**Two numbering schemes, and they are unrelated.** "Step N" below is a
slice of THIS plan. `#NN` is a GitHub pull request, numbered repo-wide
across every track. They do not line up and never will: Steps 1–5 all
shipped inside the single squash `#93`. Say "Step 6" for the work and
"#93" for the pull request — never "PR 6", which reads as a GitHub number
that does not exist.

| Step | Scope | State |
|---|---|---|
| 1 | Playhead follow on seek (`view.revealSamples`) | **done** — in #93 |
| 2 | Lane multi-select + group/global M/S/A | **done** — in #93 |
| 3 | Backend catalog, persistent + incremental scan cache | **done** — in #93 |
| 4 | `browser/` primitives + migrate instruments/samples/presets | **done** — in #93 |
| 5 | Plugin Manager: browse/split/rack + Ctrl+P frecency + native GUI | **done** — PR #93 |
| 6 | Automation matrix, pinned params, lane-info strip (winner §3.4) | **next** |
| 7 | Unified audition (`browserAudition` pref, default off) | blocked on 6 |

### Step 5 as shipped

`PluginManager.svelte` (browse / split / rack), `PluginQuickPick.svelte` +
`Ctrl+P`, facets (ALL/INST/FX, format + category chips, ★/⏱), frecency,
native floating editors (`plugin_show_gui`), live on-top pref
(`plugin_set_gui_on_top`). Review follow-up for LV2 GUI lifetime and
QuickPick click-to-insert is in the same squash.

**Scope call made:** browse mode gets **no Zyn patches section**, though
§5.2 lists one. `next-prompt.md`'s "Do not" forbids putting that
virtualised ~1318-row list onto `BrowserRow`/`BrowserSection`, and a
section here would be exactly that migration.

## Step 5 — the Plugin Manager

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

### 5.4 Round-three asks — design §9 (fold into this PR)

**§9.1 Active before inactive.** The owner's complaint is concrete: the
live instances sit under the whole scanned list, so reaching your own four
plugins means scrolling past two hundred you do not have. The browse/rack
toggle answers the scroll; whether the rack deserves its OWN dock panel is
the owner's call between design §9.1's three options. **Recommended and
assumed until told otherwise: option (a)** — one panel, `RACK` is the
default mode whenever the project has at least one instance, `BROWSE` one
click away, choice sticky per project in `localStorage`.

**§9.2 A three-way kind filter.** Today's `instrumentsOnly` checkbox is a
two-state control over a three-state question — "effects only" is
unreachable. `BrowserShell`'s `filters` slot gets `ALL` / `INST` / `FX`
(exclusive, one always on), plus independent `★` and `⏱` narrowing
toggles. Chip state joins the mode and the folds in `localStorage`. A
filter narrows the SAME section tree; it does not switch to another one,
so an emptied section disappears on its own.

**§9.3 Search what the user actually typed.** `name`, `vendor` and
`categories[]` already rank — `categories[]` IS "type", so that ask is
half-answered. Add the catalog's user `tags[]` to the rank keys (two lines
in `plugin-browse.ts`'s `RANK_KEYS`, plus tests), and, if Step 5 has room, a
single `format:` prefix parsed in `browser-model.ts`. Nothing more
elaborate: a query DSL is a worse answer than a chip. Do NOT build an
I/O-count filter.

## Step 6 — automation as an inventory

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
