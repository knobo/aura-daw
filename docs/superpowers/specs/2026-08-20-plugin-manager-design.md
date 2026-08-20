# Plugin management, lane bulk-state and playhead follow — design

Date: 2026-08-20 · Branch: `feat/plugin-manager` · Worktree: `.worktrees/plugin-manager`

## 1. Why

Four asks, one session. Three are small and independent; one is a
subsystem.

1. **Plugin management** (subsystem). AURA can scan, instantiate, bind and
   tune plugins, but the scan dies with the process, the browser is a flat
   list behind an `instruments` checkbox, and there is no answer to
   "what is actually running in this project, and what of it is automated?"
2. **Lane bulk state.** Lanes group and fold, but M/S/A is strictly
   one lane at a time.
3. **Playhead follow.** `Timeline.svelte` only follows while
   `transport.isPlaying`, so "rewind to start" leaves the view behind.
4. **Lane-info plugin indicator.** The FX chip counts inserts; it does not
   help you reach the parameter you want to automate.

## 2. What the competition gets wrong

| DAW | Discovery | Inventory | Automation inventory |
|---|---|---|---|
| FL Studio | Folder tree; favourites = drag into a folder | Split across channel rack, mixer, playlist | None |
| Ableton Live | Good: categories + coloured Collections | Per-track device chain only | None — you hunt lane by lane |
| Bitwig | Good | Per-track | Modulation is first-class, inventory is not |
| LMMS | Minimal | None | None |

Every one of them lets you browse **what could exist** and none of them
shows you **what does exist**. That is the gap AURA takes.

## 3. Design thesis

AURA's visual identity is already committed and enforced (`app.css` tokens,
`no-literals.test.ts`): dark console, cyan/magenta duotone, glass panels,
silkscreen micro-labels, mono numerics. This design does **not** spend its
boldness on a new palette — it spends it on information architecture.

**Discovery is a tree. Inventory is a ledger.** Two modes in one shell, and
the shell makes the difference structural rather than decorative:

```
┌─ PLUGINS ───────────────────────── [BROWSE] [RACK] ─┐
│  ⌕ search…                      ⚡fav  ⏱recent  ⚙all │
├──────────────────────────────────────────────────────┤
│  BROWSE (tree)          │  RACK (ledger)             │
│  ▾ ★ Favourites    (6)  │  TRACK    PLUGIN     STATE │
│    ▸ Surge XT           │  Drums ▸1 Vital       ●    │
│  ▾ ⏱ Recent        (8)  │    │cutoff│ ~ │ ⎋ch4  │   │
│  ▾ Instruments   (124)  │  Bass  ▸i Surge XT    ●    │
│    ▾ Synth        (61)  │    │drive │ ~ │       │   │
│    ▾ Sampler      (12)  │  Vox   ▸1 Calf Rev    ○    │
│  ▾ Effects       (203)  │                            │
│    ▾ Reverb       (18)  │  2 stubs · 1 crashed       │
└──────────────────────────────────────────────────────┘
```

### 3.1 Signature: the parameter chip

One small mono capsule — parameter name, current value, and a 2 px left
state rail:

```
┌╴────────────┐   rail colour = state
│▍cutoff  62% │   dim     plain
└─────────────┘   cyan    automated (has a curve)
                  magenta MIDI-mapped
                  amber   recording (write/touch/latch armed)
```

The same chip renders in **four** places: the rack row, the parameter
panel, the automation matrix, and the lane-info indicator. That is what
makes the app feel like one app — the user's stated goal — and it is
cheaper than four bespoke treatments.

Clicking a chip jumps to its automation lane, or offers to create one.

### 3.2 Signature: the quick picker

`Ctrl+P` opens a narrow type-to-add overlay. Ranking: favourites, then
recents, then fuzzy score. Enter adds to the selected track; `Shift+Enter`
adds as an insert. The full manager stays in the dock for oversight work —
add-in-the-flow and administer-the-collection are different jobs and get
different surfaces.

## 4. Architecture

### 4.1 Backend: `src-tauri/src/plugins/catalog.rs` (new)

Machine-global, one file at `dirs::config_dir()/aura/plugin-catalog.json`,
written atomically (temp + rename), schema-versioned, and **never fatal**:
an unreadable or malformed catalog logs and starts empty, the same
contract `theme.rs` and `lanes.svelte.ts` already follow.

```jsonc
{
  "version": 1,
  "scan": {
    "scannedAt": 1755648000,
    "entries": [{ "descriptor": { … }, "path": "…", "mtime": 0, "size": 0 }]
  },
  "favorites": ["clap:/usr/lib/clap/Surge XT.clap#…"],
  "recents":   [{ "uid": "…", "usedAt": 1755648000 }],   // capped at 40
  "tags":      { "<uid>": ["Live rig", "Bass"] },
  "pinnedParams": { "<uid>": [3, 7, 11] }                // capped at 8/uid
}
```

**Persistent scan cache.** `plugins::init()` loads the file and seeds the
registry, so `plugin_list` returns a populated list and `scanned: true` on
the first frame after launch — no IPC change, no new command.

**Incremental rescan.** `plugin_scan` stats each discovered bundle first
and reuses the cached descriptor when `path + mtime + size` all match;
only new or changed bundles are handed to the sacrificial scan worker. The
result is written back to the catalog. Signature unchanged.

**Three additive commands.** A patch-shaped update keeps the surface small
rather than growing one command per field:

| Command | Shape |
|---|---|
| `plugin_catalog_get` | `() -> PluginCatalog` |
| `plugin_catalog_update` | `(patch: PluginCatalogPatch) -> PluginCatalog` |
| `plugin_scan_status` | `() -> { scannedAt, cached, stale, missing }` |

`PluginCatalogPatch` is an all-optional struct:
`setFavorite? {uid,on}`, `noteUsed? uid`, `setTags? {uid,tags}`,
`setPinnedParams? {uid,paramIds}`, `forgetRecent? uid`, `clearRecents? bool`.

### 4.2 What stays in the frontend

**Collapse state is view state.** Which categories and groups are folded
lives in `localStorage`, not the catalog and not the op log — the same
ruling `lanes.svelte.ts` documents for lane folds, for the same reason:
"collapsed the reverb category" does not belong between two real edits on
the undo stack, and it is not something a collaborator needs.

### 4.3 Generic components — `src/lib/components/browser/`

The reusable layer, so every browser in the app speaks the same language:

| File | Job |
|---|---|
| `BrowserShell.svelte` | Search field, filter chips, scroll body, footer status, keyboard nav (`↑↓` move, `→←` fold, `Enter` act, `/` focus search) |
| `BrowserSection.svelte` | Collapsible group: disclosure triangle, label, count badge |
| `BrowserRow.svelte` | Format badge, primary label, secondary meta, favourite star, trailing action slot |
| `ParamChip.svelte` | §3.1 — the signature capsule |
| `EmptyState.svelte` | Directive empty/error text ("No plugins yet. Scan to find your CLAP and LV2 installs.") |
| `browser-model.ts` | Pure filter / group / sort / fuzzy-rank functions — where the unit tests live |

Migrated onto the shell in the same PR: `PluginBrowser`,
`InstrumentBrowser`, `ZynPatchBrowser`, `SamplesRoot`, `PresetsRoot`,
`LibraryPanel`.

### 4.4 The rack and the automation matrix

The rack is derived, not stored: `plugins.instances` joined against
`project.tracks` (`instrumentId === "plugin:<id>"` and `inserts[]`) and
against `modulation` bindings whose target is
`{ kind: "pluginParam", instanceId, paramId }`. All of that is pure
projection and belongs in a tested `src/lib/utils/plugin-rack.ts`, not in
a component.

The automation matrix is the same projection grouped by parameter instead
of by instance, plus track-param curves, giving one project-wide list of
everything that moves.

### 4.5 Lane bulk state

New `lanes.selection: Set<string>` (view state, not persisted — a
selection is a gesture, not a document). Click selects, `Ctrl/Cmd+click`
toggles, `Shift+click` extends over the visible lane order. `project`
gains `setMuted/setSoloed/setArmed(trackId, value)` — explicit setters
next to the existing toggles, because "make all of these muted" must not
flip half of them off. Bulk writes go through one `project.beginGesture`
so twelve lanes are one undo step.

Three entry points: the lane's own M/S/A (applies to the whole selection
when the lane is in it), the group header (whole group), and the master
bar (project-wide, with a distinct "some lanes are muted" tri-state).

## 5. Testing

- `browser-model.ts`, `plugin-rack.ts`, `catalog` reducers: plain `*.test.ts`.
- `BrowserShell`, `ParamChip`, bulk-M/S/A wiring: `*.dom.test.ts`. Read the
  jsdom pitfalls in `docs/superpowers/plans/2026-08-18-dom-test-environment.md`
  before writing one — pointer capture, zero rects and `sectionTable` all bite.
- Rust: catalog round-trip, malformed-file tolerance, incremental-rescan
  hit/miss, recents cap.
- Every new `<style>` block must pass `no-literals.test.ts` — tokens only.

## 6. Delivery

| PR | Scope | Depends on |
|---|---|---|
| 1 | Playhead follow on seek | — |
| 2 | Lane multi-select + group/global M/S/A | — |
| 3 | Backend catalog, persistent + incremental scan cache, store mirror | — |
| 4 | `browser/` primitives + migrate the five existing browsers | — |
| 5 | Plugin Manager: browse/rack modes + quick picker | 3, 4 |
| 6 | Automation matrix, pinned params, lane-info indicator | 4, 5 |

## 7. Out of scope

MIDI Learn itself (the chip shows mapping state; nothing binds a
controller yet — that is the Track D follow-up), plugin sandboxing,
plugin-format support beyond CLAP/LV2, and a stock FX suite.
