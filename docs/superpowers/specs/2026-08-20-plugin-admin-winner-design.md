# Plugin administration — winner spec

Date: 2026-08-20 · Branch: `feat/plugin-manager`
Supersedes the *layout* calls in [`2026-08-20-plugin-manager-design.md`](2026-08-20-plugin-manager-design.md) §9.1 (option a). The catalog, rack projection, param chip, and frozen IPC from that document still stand.

Owner steer (same day): do not pick one job. All four must be world-class, the 340 px dock does not set the ceiling, write the spec and ship something to ear-check.

## 1. Why we lose today if we stop at PR 5

PR 5 as built is a good *AURA* panel. It is not yet a reason to leave Ableton, Bitwig, or Reaper.

| Job | Best in class | What they actually steal | PR 5 today |
|---|---|---|---|
| Add in the flow | Raycast / VS Code / Reaper Shift+F | Type 3 letters, **frecency** (how often × how recent), Enter commits, the overlay is the product | Ctrl+P ranks favourites then recents then fuzzy. No frequency. No "add to *this* track" in the row. |
| Find among hundreds | Ableton Live 12 | Tags as **AND across groups, OR inside a group**. Seven colour Collections. Rank/most-used. Preview. | ALL/INST/FX + ★/⏱ + `format:` prefix. Categories are a tree, not facets. |
| See what is live | *nobody* | — | Rack exists, but it is a *mode*. Building a chain means you cannot see the catalog. That is the original complaint with a different hat. |
| Work where the sound is | Bitwig device chain | Nest, bypass, map, jump to a parameter without opening a manager | TrackHeader still has an FX *count*. Param chips are not on the lane. |

The unique bet stays: **inventory**. Winning means the other three are stolen at source, not approximated.

## 2. Thesis

Three surfaces, no single home. The dock is a workspace that grows, not a 340 px archive.

```
1. Ctrl+P     everywhere. Frecency. Track-aware. Overlay, not a panel.
2. The dock   SPLIT by default when the project has instances:
              rack on top (the project), browse below (the catalog),
              draggable divider, shared search and facets.
              BROWSE / SPLIT / RACK are focus, not religions.
3. The lane   instrument + each insert as a strip of status + param chips.
              Click a chip, jump. That is Bitwig's chain, compressed.
```

Palette, type, glass: unchanged. Boldness stays on information architecture, plus one layout risk: **the plugins dock opens wider than the other tabs**, and the split is the default, not a power-user toggle.

## 3. Steal list (normative — each item names the source and the AURA shape)

### 3.1 Raycast — frecency, not recents

`rankQuickPick` today: favourites, then recents by `usedAt`, then fuzzy. That is recency with a star pinned on top.

Steal: **frecency**. A local (not catalog, not op log) table `{ uid: { count, lastUsedAt } }` in `localStorage`, incremented on every successful instantiate/insert. Score:

```
favourite ? 1_000_000 : 0
+ count * 100
+ recencyBoost(lastUsedAt)     // 40 / 20 / 8 / 0 for <1d / <7d / <30d / older
+ fuzzyScore                   // only when a query is running
```

Favourites still win a tie. A favourite you never use still beats a stranger. A plugin you add every session without starring it climbs past a starred one you used once in May.

The picker row spells the action: `Surge XT  →  Bass (instrument)` / `Calf Rev  →  Bass (insert)`. Enter and Shift+Enter stay. The highlighted row's verb is visible *before* you press.

### 3.2 Ableton 12 — facets, not a folder religion

Categories stay as a tree (discovery by wandering). They *also* become chips.

Facet groups, AND across groups, OR inside a group:

| Group | Control | OR / exclusive |
|---|---|---|
| Kind | ALL / INST / FX | exclusive, one on |
| Format | CLAP · LV2 | multi, OR. None on = all formats |
| Type | chips from the scan's actual `categories[]` | multi, OR. None on = all types |
| Shortlist | ★ · ⏱ | independent, union if both |

A plugin matches when it passes kind AND (no format chips or its format is selected) AND (no type chips or one of its categories is selected). Then the tree is built from the survivors, so empty sections disappear.

Do **not** build Cubase MediaBay. Do not invent a query language beyond `format:`. The chips *are* the query.

Collections (Ableton's 7 colours) are a later ear-check, not this pass. ★ is the one collection until someone asks for a second.

### 3.3 Our ledger — split, not a mode you hide the catalog behind

§9.1 option (a) is revoked as the default. Option (c) is the default, with the dock allowed to grow so the cramped-objection dies:

- **SPLIT** when `instances.length > 0` and the user has not pinned a focus.
- Rack on top, browse below, shared search + facets. Two scroll regions, one query.
- Divider is draggable. Height persisted in the same view blob as mode/chips.
- **BROWSE** / **RACK** still exist: full-height focus for when you are *only* scanning or *only* debugging crashes.
- A stored focus wins, same as before. Clearing it (clicking SPLIT) returns to the default rule.

Opening the plugins tab on a dock still at the generic 340 px **widens it to 480** (clamped by `DOCK_RESIZE`). Other tabs keep 340. The plugins workspace is allowed to be larger because it has more information density, not because it is more important as a brand.

### 3.4 Bitwig — the chain lives on the lane

TrackHeader's FX chip becomes a strip:

```
[● Surge XT] [cutoff 62%] [▸ IRVerb] [dry 40%]  [+2]
```

- Instrument first, inserts in slot order, overflow `+N`.
- Status dot on the name (stub / active / crashed).
- Up to two pinned-or-automated param chips per device; click jumps to the lane or offers to create one.
- Folded lane: dots only, no chips.
- This is plan §6.3, promoted from "PR 6 later" to the same winner track. It can ship a beat after the split if the header fights `--track-height`, but it is not optional in the spec.

Bypass stays on the rack row *and* on the strip (click the name with Alt, or a small BYP on hover). One without the other is how Ableton and FL split your brain.

### 3.5 Reaper — type-ahead and smart folders, taken selectively

Steal: the FX window is a *real* working surface, and typing always filters. We already filter on query.

Do not steal: user-maintained flat folder trees as the primary organisation. Reaper users spend more time tending folders than mixing; that is the complaint we are not importing.

Smart folders (a saved facet set named "Live rig") wait until Collections land.

### 3.6 Logic — patches as a saved stack, explicitly later

A saved "Bass patch" = instrument instance + insert chain + pinned params. That is a document-shaped object and a new command. Out of scope for this pass. The rack and the lane strip must not paint themselves into a corner that forbids it: placements stay derived.

## 4. Layout (the thing you ear-check)

```
┌─ PLUGINS ────────────── 480+ px ──── [BROWSE] [SPLIT] [RACK] ─┐
│ ⌕ search…   [ALL][INST][FX]  [CLAP][LV2]  [Reverb][Synth]… ★ ⏱ ⌃ │
├─ RACK ────────────────────────────────────────────  ~40% ─────┤
│  BASS          Surge XT     ● clap    │cutoff ~│         PARAMS ×│
│  VOX           IRVerb       ● clap    insert 1  BYP      PARAMS ×│
│  Not on any    Dead CLAP    ● crashed  orphan            PARAMS ×│
├─ ▭ divider ───────────────────────────────────────────────────┤
│  ▾ ★ Favourites (3)                                           │
│  ▾ Instruments ▾ Synth                                        │
│      clap  Surge XT                               ADD         │
│  ▾ Effects ▾ Reverb                                           │
│      clap  IRVerb                                 INSERT      │
├───────────────────────────────────────────────────────────────┤
│  4 live · 1 crashed · 1 unused · 214 plugins                   │
└───────────────────────────────────────────────────────────────┘
```

Empty rack (no instances): SPLIT is hidden, layout is browse full height. The BROWSE chip is pressed. Scanning is the job.

Ctrl+P is unchanged in placement (centred overlay, `ui.pluginPickerOpen`). It does not live in the dock.

## 5. View state

Extends `plugin-manager-view.ts`:

```
mode: "browse" | "split" | "rack"
kind, favoritesOnly, recentsOnly     // unchanged
formats: PluginFormat[]              // empty = all
categories: string[]                 // empty = all
splitPx: number                      // rack pane height, default 220
```

`initialManagerMode(stored, instanceCount)`: stored `"browse"|"split"|"rack"` wins; else `instanceCount > 0 ? "split" : "browse"`.

Frecency table is a *separate* key (`aura.plugin.frecency`), machine-global like the catalog, not per project. A plugin you use in every song should climb everywhere.

## 6. What we will not steal

- Cubase MediaBay attribute database.
- A second dock tab (option b). Shortcut strip is already nine wide.
- I/O-count filters.
- Audition-on-browse that makes noise by default (still PR 7, pref off).
- Migrating Zyn's 1318-row virtual list onto `BrowserRow`.
- Floating OS windows this pass. Tear-off is the escalation if SPLIT-in-dock still feels like a drawer after the ear-check.

## 7. Key decisions

| Decision | Why |
|---|---|
| SPLIT is the default, not RACK-only | The original complaint was "active is at the bottom of a scroll". A mode toggle answers the scroll and then hides the catalog while you build a chain. Split answers both. |
| Plugins dock opens at 480 | Option (c) was rejected as cramped at 340. Raising *this tab's* width is the honest fix; other tabs stay 340. |
| Frecency is localStorage, not catalog | Frequency is a gesture statistic, not a collaborator fact, and not worth an IPC bump. |
| Facets AND across groups | Ableton's rule. OR-everything is a pile, exclusive-everything is a folder tree with extra steps. |
| Lane strip is in this spec | Without it, Bitwig still wins the "I am looking at the track" job and the manager is a detour. |
| No Collections yet | One ★ until the ear-check asks for a second colour. |

## 8. Delivery (what ships so it can be tried)

**This pass (ear-check 1)** — must be in the worktree before the owner launches the app:

1. Mode is BROWSE / SPLIT / RACK. Default SPLIT when the project has instances.
2. Split layout, draggable divider, persisted `splitPx`.
3. Facet chips: format + category, AND/OR as §3.2.
4. Frecency in `rankQuickPick` / Ctrl+P, with the destination spelled on the row.
5. Opening the plugins tab widens a still-default dock to 480.

**Next pass (ear-check 2)** — lane strip (Bitwig), Collections if asked, audition pref (PR 7), saved patches (Logic).

## 9. PR Plan

| PR | Title | Depends | Notes |
|---|---|---|---|
| 5b | Split + facets + frecency + wider plugins dock | PR 5 groundwork | This pass. No IPC. |
| 6 | Lane plugin strip + automation matrix | 5b | Header density is the risk. |
| 7 | Unified audition, pref default off | 5b | Touches every browser. |

## 10. Open questions (answered by the ear-check, not by more design)

- Is 480 wide enough, or does this want to eat the left column like Ableton?
- Is rack-on-top the right split, or rack-on-the-right once the dock is wide?
- Do we need a second Collection colour in week one?

Those are things you feel in the app. They are not things we settle in prose.
