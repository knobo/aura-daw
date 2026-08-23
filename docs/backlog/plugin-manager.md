# Backlog — plugin manager

Spec: [`2026-08-20-plugin-admin-winner-design.md`](../superpowers/specs/2026-08-20-plugin-admin-winner-design.md)
(the layout that shipped) and
[`2026-08-20-plugin-manager-design.md`](../superpowers/specs/2026-08-20-plugin-manager-design.md)
(catalog, rack, chips, frozen IPC).
Plan: [`2026-08-20-plugin-manager.md`](../superpowers/plans/2026-08-20-plugin-manager.md)
— status table plus "Step 6 as shipped".

Steps 1–6 have landed; see [`../LANDED.md`](../LANDED.md).

## The owner's standing steer (2026-08-20)

> "Ikke kjenn på begrensninger i UI design i forhold til hva som ligger der
> fra før. Hvis vi skal vinne må vi lage en løsning som er bedre enn hva
> andre har."

**Do not let the existing UI set the ceiling.** This overrides the usual
"follow existing patterns" instinct for UI work on this track — it does
NOT override the frozen IPC surface, the theme-token rule, or the test
gates.

## Next — Step 7, unified audition

Design §8.2. Double-click any browser row to hear it, gated behind a new
`browserAudition` pref defaulting **off**. Last on this track: it touches
every browser plus sampler and plugin hosts. The plan's status table
carries one line for it; the implementable detail is design §8.2, not the
plan.

## Owner ear-checks still owed

- The live *Keep plugin GUI on top* toggle: open an editor → Preferences
  off/on → the window follows, no restart.
- SPLIT vs BROWSE/RACK — is SPLIT the right default?
- **From Step 6:** the lane strip at `--track-height: 132px` on a narrow
  window (does a long plugin/instrument name push the FX chip out of the
  row?); the folded-lane dots (status + name legible enough without a
  name button — a judgment call, not pixel-verified); and MATRIX mode
  (does grouping by parameter read as useful, does the fourth mode chip
  fit the row at 480 px).

## Scope calls already made — do not relitigate without a reason

- No Zyn patches section in browse; the virtualised ~1318-row list stays
  on `ZynPatchBrowser`, and it is not migrated onto
  `BrowserRow`/`BrowserSection`.
- No second dock tab. The matrix is a 4th `ManagerMode`, not a tab.
- No `OP_FORMAT_VERSION` bump anywhere on this track.
- The favourite star talks only to catalog commands.
- The FX chip survived rather than being replaced by the lane strip.

## What shipped, in detail

Kept here because the identifiers are hard to rediscover from the diff.
The merged branches `feat/plugin-manager` (Step 5) and
`feat/plugin-automation-inventory` (Step 6) are **spent** — do not reopen
either.

**Step 5 (PR #93):**

| Piece | Detail |
|---|---|
| Plugin catalog | `plugin-catalog.json`, incremental scan, `plugin_catalog_*` commands |
| Plugin Manager | browse / **split** / rack. Split is the default when the project has instances. Facets: ALL/INST/FX, format + category chips, ★/⏱. The dock widens to 480 on the plugins tab. |
| Ctrl+P quick-pick | frecency in `aura.plugin.frecency`. Click/Enter follow the descriptor (effects insert; Shift+Enter forces insert). |
| Native floating GUI | CLAP `clap.gui`, LV2 `ui:showInterface`, Zyn via `zynaddsubfx-ext-gui` (no `--embed`). Additive IPC: `plugin_show_gui`. GUI button next to PARAMS, hidden when there is no editor. |
| On-top pref | INTERFACE `pluginGuiOnTop`, default on. Live toggle via EWMH ClientMessage + `WM_TRANSIENT_FOR`. Additive IPC: `plugin_set_gui_on_top`. |
| Review fixes in the squash | LV2 Drop moves DSP onto plugin-main and closes the editor before free; `make_node` re-attaches ext-gui; CLAP `gui_created` after `create`; `plugin_list` drops the registry lock before `gui_flags`. |

**Step 6 (PR #98):**

| Piece | Detail |
|---|---|
| Param cache | `plugins.svelte.ts` gained a lazy per-instance `paramCache` over the frozen `plugin_get_params`, so no plugin needs to be open to read its param names and values. |
| Shared lane reveal | `utils/lane-reveal.ts`'s `revealParamLane` — unfold the lane, `modulation.pickTarget`, scroll `[data-track-id]` on `TrackHeader`'s root. Every chip below calls it. |
| Automation matrix | `AutomationMatrix.svelte` + `utils/automation-matrix.ts`: the rack projection grouped by parameter instead of by instance, as a 4th `ManagerMode` (`"matrix"`). |
| Pinned params | `PluginParamPanel.svelte`: every param row is a `ParamChip`; pinned ones (`catalog.pinnedParams[uid]`, max 8) sort into a `Pinned` section up top. A pinned param is *duplicated* there, not moved out of its own group — pinning is a shortcut, not a move. |
| Lane plugin strip | `LanePluginStrip.svelte` + `utils/lane-strip.ts` in `TrackHeader.svelte`'s `.metadata-row`, both the unfolded and folded branches. The strip sits between the instrument chip and FX; its `+N` overflow opens `InsertChain`. Folded lane renders dots only, no name button. |

## Leftovers — do not start unless asked

- **Native GUI:** X11 embed / Wayland embed; out-of-process isolation
  (SCALABILITY step 4). LV2 `save_state` still snapshots the shadow
  instance, so OSC tweaks on the live DSP are not what project save sees.
- **Docs debt:** [`../ARCHITECTURE.md`](../ARCHITECTURE.md) §15.4 and
  [`../SCALABILITY.md`](../SCALABILITY.md) step 3 do not yet list
  `plugin_show_gui` / `plugin_set_gui_on_top`, and step 3 is not marked
  paid. A docs-only follow-up.
- **Performance, from Step 6's final review — deliberately documented,
  not fixed.** The lane strip re-runs a full `buildRack` per mounted
  track header on every knob-drag frame: `LanePluginStrip.svelte`'s
  `devices` derived reads `plugins.paramInfo`, so it subscribes to
  `paramCache`, and `setParam` replaces the whole `paramCache` object at
  input-event rate. Every mounted header's `devices` therefore
  invalidates on every drag frame, and each recompute allocates one
  `RackEntry` per *global* instance, per track — at 30 tracks / 60
  instances, ~1800 objects per frame. The fix is to split the derived so
  the join and the values have different dependency surfaces (structure
  from inert stand-ins, values layered on top — the shape `instanceIds`
  already uses for the `ensureParams` effect), or to memoize the rack
  once per `(instances, bindings)` identity and share it across strips.
  Fixing only the `cacheParams` whole-object write buys almost nothing on
  its own.
- **Two findings from a second final review of Step 6, never landed.**
  Step 6 was run by two SDD controller sessions at once in the same
  worktree — the collision the claim protocol in
  [`../../next-prompt.md`](../../next-prompt.md) now exists to prevent.
  Each ran its own whole-branch review and got a different finding set;
  only one landed. These two are real, small, and unactioned:
  - `plugins.svelte.ts` — write `paramCache` **per property** instead of
    spreading the whole object on every `setParam`. Only worth doing
    together with the performance item above, which is the same path.
  - `AutomationMatrix.svelte` — the matrix is a grouped, expandable list
    and should carry `role="tree"` semantics rather than plain rows, the
    way the lane rail already carries `role="grid"` and
    `aria-multiselectable`.

## Do not

Rebuild PluginManager / Ctrl+P / catalog / floating GUI. Break a frozen
command name. Bump `OP_FORMAT_VERSION`.
