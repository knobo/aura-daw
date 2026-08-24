# Backlog — plugin manager

Spec: [`2026-08-20-plugin-admin-winner-design.md`](../superpowers/specs/2026-08-20-plugin-admin-winner-design.md)
(the layout that shipped) and
[`2026-08-20-plugin-manager-design.md`](../superpowers/specs/2026-08-20-plugin-manager-design.md)
(catalog, rack, chips, frozen IPC).
Plan: [`2026-08-20-plugin-manager.md`](../superpowers/plans/2026-08-20-plugin-manager.md)
— status table plus "Step 6 as shipped".

Steps 1–7 have landed; see [`../LANDED.md`](../LANDED.md).

## The owner's standing steer (2026-08-20)

> "Ikke kjenn på begrensninger i UI design i forhold til hva som ligger der
> fra før. Hvis vi skal vinne må vi lage en løsning som er bedre enn hva
> andre har."

**Do not let the existing UI set the ceiling.** This overrides the usual
"follow existing patterns" instinct for UI work on this track — it does
NOT override the frozen IPC surface, the theme-token rule, or the test
gates.

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
- **From Step 7**, four things no suite can answer:
  - Does double-click-to-audition feel right at C3, and is 1200 ms the right
    decay for the row highlight?
  - Does the chip's off-state glyph `♪̸` actually render in WebKitGTK? It is
    a combining character and was never seen in the real app — if it looks
    broken, fall back to `♪` and let the `on` class carry the state.
  - The R-1 question the plan deliberately did not answer: `SamplesRoot`
    still auditions on *single* click and `InstrumentBrowser` still sounds on
    hold, both regardless of the new preference, because gating them would
    have deleted shipped behaviour. Should the preference own those too?
  - The R-4 consequence: because a real double-click fires `click, click,
    dblclick`, `PresetsRoot`'s instrument rows and `InstrumentBrowser`'s rows
    were given no double-click at all — they reach the store only through
    Shift+Enter, which *is* pref-gated while their mouse preview is not. Is
    that asymmetry acceptable, or should those rows get the gesture with the
    flam solved another way?

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
The merged branches `feat/plugin-manager` (Step 5),
`feat/plugin-automation-inventory` (Step 6) and `feat/browser-audition`
(Step 7) are **spent** — do not reopen any of them.

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

**Step 7 (PR #106):**

| Piece | Detail |
|---|---|
| The gesture | `BrowserRow`'s `ondblclick`, plus Shift+Enter on `BrowserShell` for keyboard parity. Single click is untouched everywhere (ruling R-1). |
| The gate | `browserAudition` pref, default off, plus `AuditionChip.svelte` in every browser toolbar. The chip writes the pref — there is no second session mute (ruling R-2). |
| The dispatch | `utils/audition-target.ts` resolves a row to an `AuditionTarget`; `state/audition.svelte.ts` plays it through `sampler_preview_note` / `plugin_preview_note` / `library_audition`. No new IPC, no ops — including in `ZynPatchBrowser`, whose double-click plays through the same shared store as every other browser and contributes only the note; its pre-existing single-click load (`zyn_load_patch`) remains a project edit, as it always was. |
| Catalog plugins | A descriptor borrows a live, track-bound instance of the same plugin if one exists; otherwise silent with a reason. Instantiating to preview would commit `Op::PluginAdd` (ruling R-3). |

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
- **The document-free plugin preview slot.** §8.2 asks for a lazily
  instantiated, reusable preview slot so a catalog plugin can be heard
  without being added to the project. Step 7 deliberately did not build it:
  `plugin_preview_note` routes through the instance's bound midi track, so
  reaching a bare descriptor means `plugin_instantiate` — an `Op::PluginAdd`
  with an undo entry, which an audition must never be. Doing it properly is
  backend work: host an instance outside the document and render it into the
  preview stream that `audio/sampler_preview.rs` already owns, with a single
  reusable slot and teardown on a timer. Additive IPC. Owner scoped it out of
  Step 7 on 2026-08-23.
- **`audition.sounding` has no consumer.** No row lights up when the new
  gesture fires — wiring it needs a second visual on rack rows, which
  already use `selected` for the open instance.
- **`PresetsRoot`'s zyn patch rows audition whichever patch is *loaded*,**
  ignoring which row was clicked, and render no silent reason when there
  is no open Zyn instance.
- **`library.auditioning` and the audition store both own the sample
  preview stream** and can disagree about what is playing; the store
  should probably route samples through `library.audition()` instead of
  the backend directly.
- **`InstrumentBrowser` sounds on `pointerdown` with no hold delay,** so
  the R-4 table's description of it as "hold-to-play" is wrong — worth
  correcting there and putting to the owner as the sharpest form of the
  R-1 question.
- **`AuditionChip` is sized for `BrowserShell`'s toolbar** (24 px, 5 px
  radius, translucent) but also sits among `PluginManager`'s and
  `ZynPatchBrowser`'s much smaller chips (8-9 px font, transparent), so it
  will render visibly taller and filled next to them — one for the same
  ear-check as the `♪̸` glyph.

## Do not

Rebuild PluginManager / Ctrl+P / catalog / floating GUI. Break a frozen
command name. Bump `OP_FORMAT_VERSION`.
