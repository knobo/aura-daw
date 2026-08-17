# Handoff — Lanes UX (rename · fold · group · drag-reorder · scroll fix)

**Status: LANDED.** Merged to `main` as PR #60 (2026-08-17), `5f891cb`.
Do not restart this track.

**Branch:** `worktree-lanes-ux` (kept — its per-commit history is cited
above) · **Worktree:** `.claude/worktrees/lanes-ux`
**Base:** `origin/main` @ `7c3cb87` (Pitch Coach phase 2); rebased before
merge onto `c14916d` (Pitch Coach phase 3, PR #61) + `46df20d` (theme
system, PR #63).

This file exists so a fresh session can finish the work without re-deriving
anything. Trust the code over this file where they disagree.

---

## What was asked for (user, in Norwegian)

1. A simple way to **rename a lane**.
2. **Collapse a lane** to a thin strip of information.
3. **Group lanes**, with the whole group collapsible.
4. **Drag and drop lanes** to reorder, and to move in/out of a group.
5. Improve **vertical scrolling** of lanes.
6. Bottom lanes end up **behind the MIDI clip editor / Pitch Coach**;
   worse when the interface is zoomed.
7. The **track-info column is not aligned** with the track-data column at
   all scroll positions.

---

## The diagnosis for 5 + 6 + 7 — they were ONE bug

Measured in headless Chrome against a faithful reproduction of the old
markup (10 tracks, 473 px body):

```
body clientH=473 scrollH=473 SCROLLABLE=false      <- could not scroll at all
track0: headerH=44 top=0.0   | laneH=88 top=0.0
track5: headerH=44 top=217.5 | laneH=88 top=440.0  <- 222 px adrift
track9: headerH=44 top=391.5 | laneH=88 top=792.0  <- 400 px adrift
lanes scrollH=880 clientH=473 (clipped=true)       <- lanes below the fold
                                                      painted nowhere
```

Root cause: `.body` was **both** the vertical scroller **and** the flex row
holding the two columns.

- With a definite height, `align-items: stretch` sized `.rail` and `.lanes`
  to the **viewport**, not to their content.
- `.rail` is itself a flex column, so its `TrackHeader`s — flex items with
  the default `flex-shrink: 1` — were **squeezed** to fit: 88 px lanes
  against 44 px headers, drifting further apart with every row. That is
  report 7.
- `.lanes` has `overflow: hidden` (it must, horizontally — clips run past
  the right edge). A clipped child contributes **no scroll height**, so the
  body never became scrollable and rows past the fold were unreachable.
  That is reports 5 and 6, and interface zoom makes it worse purely by
  shrinking the viewport in layout px.

**Fix:** make the scroller a plain block and put the flex row inside it.

```css
.body      { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden;
             overscroll-behavior-y: contain; }
.bodyinner { display: flex; align-items: stretch; min-height: 100%; }
.rail > *  { flex: none; }    /* + `flex: none` on .header itself */
```

Verified with the same harness (`fixed.html`, scroll to bottom, re-measure):

| case | scrollable | worst row misalignment | lanes clipped |
|---|---|---|---|
| 10 tracks | yes (918/473) | **0.00 px** | no |
| 3 tracks (fits) | n/a | **0.00 px** | no |
| 30 tracks @ zoom 2 | yes (2679/67) | **0.00 px** | no |

The two harness files are in the session scratchpad
(`repro.html` = before, `fixed.html` = after). They are throwaway; the
regression net that ships is `lane-layout.test.ts`.

---

## Design decisions (and why)

**Rename / order / group membership are DOCUMENT state** — they go through
the op log, are undoable and persist in the project.

**Fold state is VIEW state** — a frontend store persisted per project in
`localStorage`. Reason: folding a lane changes nothing another musician
needs, and routing it through the op log would put "collapsed the drum
group" on the undo stack between two real edits.

**A group IS its name.** `TrackState.group` is the group's display name,
not an id into a second collection. Consequences, all deliberate:
- no second document collection, no orphan-group GC, no id→name lookup;
- renaming a group = one transaction rewriting `group` on each member,
  which is also exactly the undo unit a user expects;
- a group is rendered as the **maximal contiguous run** of tracks sharing
  the name, so every UI gesture keeps runs contiguous
  (`normalizeGroupRuns`).

**`arrange_lanes` is one coarse command, on purpose.** Every lane gesture
changes order *and* membership together; splitting it into
`reorder` + N × `set_group` would make one drag two-to-N+1 undo steps and
would publish intermediate arrangements with a non-contiguous group.

---

## Backend — DONE and green

### New wire surface (all additive; `OP_FORMAT_VERSION` **not** bumped)

| Command | Signature |
|---|---|
| `set_track_name` | `(trackId, name) -> TrackState` |
| `set_track_group` | `(trackId, group: string \| null) -> TrackState` |
| `arrange_lanes` | `(lanes: [{trackId, group}]) -> TrackState[]` |

Registered in `src-tauri/src/lib.rs` under `---- audio: tracks ----`.

### Changes

- `audio/types.rs` — `TrackState.group: Option<String>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
- `control/op.rs` — `PropPath::Group`; `PropPath::Name` re-documented as
  MidiClip **or** Track; new `Op::TrackReorder { order: Vec<TrackId> }`
  (whole-list, its own inverse shape).
- `control/session.rs` — `read_prop`/`write_prop` handle `Name` (trim,
  reject empty, ≤256 chars) and `Group` (trim, `""`→`None`); the
  `Set{Track}` arm returns early for both (persist, **no** rebuild, **no**
  param writes); new `apply_raw` arm for `Op::TrackReorder` that validates
  the permutation **before** mutating and skips the rebuild on a no-op
  reorder. `MAX_TRACK_NAME_CHARS = 256`.
- `control/snapshot.rs` — `TrackReorder → cs.tracks`.
- `control/vergraph.rs` — `TrackReorder` charge accounting.
- `control/ops.rs` — `LaneArrangement { track_id, group }`;
  `new_track_row` sets `group: None`.
- `control/mod.rs` — `set_track_name`, `set_track_group`,
  `set_track_prop` (shared), `arrange_lanes`.
- `docs/ipc-schemas/track-state.schema.json` — `group` property.
- ~24 `TrackState { … }` test fixtures gained `group: None`.

**Why reorder must rebuild:** `audio::types::derive_slots` numbers the RT
mixer slots **from display order**. A partial or unvalidated order would
silently re-point live param slots at the wrong track.

### Rust tests added (12, all passing)

`control/session.rs`: `track_rename_round_trips_and_is_a_document_only_change`,
`track_rename_rejects_empty_and_whitespace_only_names`,
`track_group_normalizes_empty_to_none_and_round_trips`,
`track_reorder_permutes_and_inverse_restores_the_previous_order`,
`track_reorder_rejects_non_permutations_without_touching_the_store`,
`track_reorder_to_the_same_order_skips_the_rebuild`,
`track_reorder_preserves_every_row_field_including_inserts`.

`control/mod.rs`: `arrange_lanes_reorders_and_regroups_in_one_undoable_step`,
`arrange_lanes_emits_group_writes_only_where_the_group_changed`,
`arrange_lanes_rejects_an_incomplete_arrangement_atomically`,
`set_track_name_trims_and_rejects_a_blank_rename`,
`set_track_group_sets_and_clears`.

---

## PRE-EXISTING test failures — do not chase them

This sandbox has no working audio device. Both figures come from the same
machine, same command:

```
cargo test --lib -- --skip plugins::
```

| | passed | failed |
|---|---|---|
| `origin/main` (baseline worktree `.claude/worktrees/lanes-baseline`) | 888 | **18** |
| this branch | 900 | **18** |

**Identical 18 test names**, all environmental: `audio::engine::tests::*`
(meter frames, transport advance, waveform pyramids),
`control::loopjam::tests::*`, `control::tests::*` loop/auto-stop, and
`mcp::server::tests::read_meters_hears_the_headless_engine`. The +12 is
exactly this branch's new tests.

`cargo test --lib plugins::` **SIGSEGVs** in the Zyn LV2 host on this
machine — also reproduced on the baseline worktree. Pre-existing;
CONTRIBUTING already documents plugin tests as gated/crash-prone.

The baseline worktree can be removed when no longer needed:
`git worktree remove .claude/worktrees/lanes-baseline`

---

## Frontend

### New files

| File | Role |
|---|---|
| `src/lib/utils/lane-layout.ts` | **The row table.** `buildLaneLayout` → ordered rows with `top`/`height`; `trackIndexAtY`, `nearestTrackIndexAtY`, `trackBand`, `bandForTrackRange`, `groupOf`. Replaces the old "row i is at `i * TRACK_HEIGHT_PX`" assumption, which stopped being true the moment a lane could be full / folded / hidden-in-a-folded-group. |
| `src/lib/utils/lane-arrange.ts` | Drag maths: `dropTargetAtY`, `arrangementForMove`, `normalizeGroupRuns`, `groupNames`, `nextGroupName`, `arrangementForGroupRename`, `arrangementForGroupDissolve`, `arrangementForAssign`. |
| `src/lib/state/lanes.svelte.ts` | Fold state + live drag/rename state; per-project `localStorage`, pruned on project switch. |
| `src/lib/components/LaneGroupHeader.svelte` | Rail strip: fold triangle, group name (dbl-click to rename), member count, dissolve. |
| `src/lib/components/LaneGroupMenu.svelte` | "Which group?" popup — the click path to what drag does by hand. |

### Modified

- `src/lib/types/ipc.ts` — `TrackState.group?`.
- `src/lib/tauri.ts` — `setTrackName` / `setTrackGroup` / `arrangeLanes`
  (interface + real impl).
- `src/lib/demo.ts` — the same three on the browser-only demo engine,
  **including the validation rules**, so the UI is never exercised against
  a more permissive engine than it ships against.
- `src/lib/state/project.svelte.ts` — `renameTrack` (not optimistic: takes
  the name the backend stored), `arrangeLanes` (optimistic, with rollback
  + `reload()` on failure).
- `src/lib/components/TrackHeader.svelte` — folded/full variants, inline
  rename, fold chevron, group chip, colour stripe as drag grip
  (`data-lane-grip`), `flex: none`.
- `src/lib/components/Timeline.svelte` — `.bodyinner` wrapper (the layout
  fix), row-table rendering in both columns, grid canvas height from
  `layout.totalHeight`, marquee + launch-mark geometry through the row
  table, lane-drag pointer handlers on the rail, drop indicator, FOLD ALL.
- `src/lib/utils/lane-geometry.ts` — `laneIndexAt` removed (moved to
  lane-layout); `TRACK_HEIGHT_PX` now re-exports `LANE_HEIGHT_PX`.
- `src/app.css` — `--lane-collapsed: 22px`, `--lane-group-height: 22px`.

### The one interaction rule worth knowing

`dropTargetAtY`: **the row you are over decides the group; which half of it
you are over decides above or below.** Joining a group is therefore the
same gesture as reordering inside it, and leaving one is the same gesture
as reordering outside it. A folded group is one row and atomic — you can
drop above or below it, never inside (dropping into something invisible is
how lanes get lost).

---

## REMAINING WORK — done, kept as a record

1. ~~`npx svelte-check` is not clean yet.~~ Fixed; 0 errors.
2. ~~Write the frontend tests.~~ `lane-layout.test.ts` + `lane-arrange.test.ts`
   written, covering everything listed here originally.
3. ~~`npm test` and `npm run build` must be green.~~ Both green.
4. ~~Update the test counts.~~ Done, recounted again after the rebase onto
   Pitch Coach phase 3 + the theme system: 1083 backend / 738 frontend.
5. **Run the app and look at it — done.** Exercised in a running
   browser-demo instance: rename (this is where the bug below was found),
   fold/unfold a lane, create a group, rename a group, drag a lane into a
   group, fold a group. Not yet heard/seen in the full Tauri build against
   a real project — an ear/eye-check with real content is still owed, same
   caveat as every other UI track.
6. ~~Open the PR.~~ #60, merged.

### Code review round (2026-08-17) — all fixed before merge

A review of the open PR found 10 issues, all confirmed and fixed:

- **`LaneGroupHeader`'s rename `commit()` had no re-entrancy guard.**
  Clearing `renamingGroup` before the `await` unmounts the focused input,
  whose native `blur` re-invoked `commit()` a second time — double-firing
  the rename, and defeating Escape's cancel (the blur-triggered commit
  fired anyway). Guarded the same way `TrackHeader.commitRename` already
  was.
- **Group-fold-state migration ran optimistically with no rollback.**
  `renameGroupFold` moved the collapsed-state key before `arrangeLanes`
  resolved; a failed arrangement left the wrong key migrated.
  `project.arrangeLanes` now returns whether it went through, so a failed
  rename un-migrates the fold key.
- **Renaming a group onto an existing DIFFERENT group's name silently
  merged the two** (the name IS the group's identity —
  `normalizeGroupRuns` gathers every placement sharing a name into one
  run). `arrangementForGroupRename` now returns `null` on a collision;
  the UI toasts instead of committing.
- **A launch-mark drag froze entirely** (not just its lane axis) when
  every track was folded into one group — `nearestTrackIndexAtY` has
  nothing to fall back to when zero track rows are painted. The time
  shift now stays live; only the lane delta is skipped when no lane is
  visible.
- **`renameTrack` silently discarded a blank rename** with no feedback,
  unlike every other rejection path. Now toasts, same as a backend
  failure.
- **Two O(n²) algorithms**: `Op::TrackReorder`'s permutation check
  (`Vec::contains` in a loop, Rust) and `normalizeGroupRuns` (a full
  array rescan per distinct group, TS). Both now single-pass / hash-set.
- **`set_track_group` has no UI caller** — every user-facing group change
  goes through `arrange_lanes`, which keeps runs contiguous by
  construction. Documented the non-contiguity hazard at both call-site
  discovery points (the Rust command's doc comment, the `Backend`
  interface in `tauri.ts`) rather than leaving it a silent trap.
- **The focus+select-on-rename effect was duplicated** across
  `TrackHeader`, `LaneGroupHeader`, `TempoControl` and `MidiClipView`.
  Extracted to `src/lib/utils/focusAndSelect.ts`, a `use:` action.
- **`--lane-collapsed`/`--lane-group-height` duplicated
  `LANE_COLLAPSED_PX`/`GROUP_HEADER_PX`** as separate CSS literals kept in
  sync by a comment. `main.ts` now sets both custom properties from the
  TS constants before the app mounts; `app.css` carries no copy of the
  number.

### Rebase onto the theme system (PR #63) and Pitch Coach phase 3 (PR #61)

Both landed on `main` while this branch was open. Rebasing hit real
conflicts (not just noise) in `CONTRIBUTING.md`/`README.md` (the dated
test-count lines) and `Timeline.svelte`/`TrackHeader.svelte` (the theme
sweep re-themed lines this branch also touched — `.lane`'s height and
border, `.stripe`'s glow). Resolved by combining both sides' actual
changes, not by picking one wholesale.

The theme system's `no-literals.test.ts` guard then caught 12 raw
`rgba(...)` colours in the new lane components (`LaneGroupHeader`,
`LaneGroupMenu`, plus new rules in `Timeline`/`TrackHeader`) — this
branch predates the sweep. Every one was an exact match for an existing
`AURA_DARK_TOKENS` value (confirmed by decimal-converting each literal
against `aura-dark.ts`), so they were a straight substitution to
`rgb(var(--x-rgb) / alpha)` — no new tokens needed.

## Things deliberately NOT done

- No vertical lane **resize** (drag the bottom edge to make a lane taller).
  Out of scope for the ask; the row table would support it.
- Fold state is not in the project file, so it does not travel with a
  project to another machine. Called out above as a deliberate trade.
- No MCP tool for the new commands. The control plane is the seam, so
  adding them later is a handler, not a redesign.
