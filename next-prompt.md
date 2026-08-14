# Next: Plan F (history storage) — and four parallel-safe post-Plan-E tracks

Read this file, then pick a track and do the work. Reply to the user in
Norwegian — they write Norwegian; the repo documentation is English.

## Post-merge whole-branch review findings (read first)

The final whole-branch review of Plan E (`15c9909..27911d8`) is at
`.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/final-review-report.md`
(verdict: NEEDS FOLLOW-UP PR). Its **FIX NOW** triage list is done —
follow-up PR #PR_NUMBER, `fix/plan-e-followup`:

- **C-1 (Critical)** — no epoch guard on `HistoryLog::record_commit`/
  `record_gesture`; a commit racing an epoch boundary journaled into the
  NEW project's file and pushed a live undo entry for the OLD document.
  → fixed in PR #PR_NUMBER (the urgent item; the other four were bundled
  behind it).
- **I-2** — LoopJam `watch_and_apply` busy-spun at 100 % CPU when a
  retryable `apply` kept failing with the transport stopped.
  → fixed in PR #PR_NUMBER (back-off + bounded retries + the mid-air-race
  test the Task 8 ledger asked for).
- **Task 13 deferral** — the deadlock audit's five stale `request`
  call-site line citations. → fixed in PR #PR_NUMBER.
- **I-5 + L-1** — plugin state blobs serialized as JSON number arrays
  (~4x); `Op::PluginRemove.params` was captured but never read.
  → fixed in PR #PR_NUMBER (`OP_FORMAT_VERSION` 2, base64 blobs, apply
  seeds the mirror from the op on cold replay).
- **I-4's two caveats** — journal line order vs `rev` order under
  concurrency, and a panicking `transact` diverging log from document.
  → recorded as L-4/L-5 in `docs/SIDE-CHANNEL-INVENTORY.md` in PR
  #PR_NUMBER (records, not fixes — the structural fix is Track A's).
- **M-3** — the transient/redo invariant was a comment checked by nothing.
  → fixed in PR #PR_NUMBER (a `debug_assert!` in the commit path).

**Still open, deliberately HELD for the owner with the context** (do NOT
fix these blind — read the report's entry first):

- **I-1** `save_project_as_epoch` writes only project.json + midi, so
  Save-As silently drops plugin `.state` blobs and automation chunks —
  and **I-7** a new/opened project inherits the previous project's plugin
  rows when `project.json` has no `plugins` key. These two are the
  branch's real data-loss surface. **Owner: the epoch/persist path — take
  them together, they interact (see also R-3).**
- **I-3** `execute_host_forward` writes `status`/`params` with no op, no
  epoch guard, and no inventory residual (with **M-6**, the grep gate's
  matching omission). **Owner: the plugin-host path (Track D's
  neighbourhood).**
- **I-6** `undo`/`redo` are sync Tauri commands and can block the UI
  thread on plugin re-instantiation + disk I/O. **Owner: Track A** (it
  owns undo/redo's substrate; `async` + `spawn_blocking`, mirroring
  `seed_demo_project`).
- **I-8** inventory row 13 claims the per-knob `project.json` rewrite is
  closed; only its position moved off the lock, the frequency is
  unchanged. **Owner: Track D / the gesture path** — the real answer is
  extending gestures to plugin params, which is what round-2 §4.4's
  CLAP-style primitive is for.
- **Minors M-1, M-2, M-4, M-5, M-7, M-8** (dirty_state clear race,
  Ctrl+S cannot recover a failed auto-persist, undo during an open
  gesture bypasses the fold, the Gate E precision sentence, `VecDeque`
  for the undo stack, the Figma oracle's omitted derived fields) —
  recorded in the report, unowned, all cheap. **M-9 is RESOLVED** by the
  review itself (`ClapNode::reset` verified to leave `steady_fallback`
  alone); close that ledger item.
- **M-3 (frontend)** undo/redo re-pull misses automation and plugin
  panels. **Owner: Track D.**

This file is written for a **fresh session after `/clear`**: it assumes no
memory of the Plan E conversation. Everything it asserts is checked against
git/README at write time (2026-08-14) — trust files over this file if they
disagree, and update this file (marked correction, ADR 0007) if they do.

## 1. State of the world

The project is **AURA**, an AI-native DAW: Tauri v2 + Svelte 5 around a
lock-free real-time Rust engine (`src-tauri/`), local AI sidecars, and an
embedded MCP server so agents mutate the session alongside the user.

**Plan E (the side-channel totality) is IMPLEMENTED and Gate E is CLOSED.**
Commit range `ac65b76..531d790` on branch `plan-e-side-channels`, open as
**PR #12** on `knobo/aura-daw` — **still OPEN, pending final whole-branch
review, then merge.** Verify its merge status before picking a track that
touches files it changed (`gh pr view 12`). Once merged, `main` carries
Plan A + B + C+D + Plan E's full channel rewrite. Full handoff (every scope
ruling, every mid-flight ruling, every carry-forward, the deferred-minors
roll-up): `docs/PHASE4-PLAN.md`'s **"Plan E handoff"** section, appended
after the "Plan C/D handoff" section, same conventions. The landed
side-channel inventory (34 rows, all closed, plus residual carve-outs
R-1..R-3 and recorded replay limitations L-1..L-5, of which L-1 is now
closed): `docs/SIDE-CHANNEL-INVENTORY.md`.

**Also open: PR #17**, `midi-input-ports` — hardware MIDI input slice 1
(port list/select + activity indicator + live monitoring, `midir`,
owner-verified end-to-end with an LPK25). This is **independent of PR #12**
(cut from `origin/main`, no document coupling — live monitoring is
engine-state-only, the same non-writer category as `sampler_preview_note`)
and is **mergeable now**, on its own timeline. Verify with `gh pr view 17`.

Main also carries **PR #9** (timeline/piano-roll horizontal scrollbars),
**PR #10** (interface-zoom preference), **PR #11** (piano-roll note
selection + copy/paste ops) — all merged before Plan E's branch did its
own mid-flight merge of `origin/main` (commit `f886306`), so they are
already folded into PR #12's diff. Nothing further to do about them.

**Baseline to verify at the start of any track**: **501 backend + 174
frontend tests, all green** (dated 2026-08-14 in README/CONTRIBUTING),
either on `plan-e-side-channels` before merge or on `main` after. Run both
suites before writing the first line of a track:

```
timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
timeout 300 npx vitest run
```

If the counts don't match, something changed underneath you — stop and
find out what before proceeding, don't just update the number.

### Branch and worktree rules (verbatim, binding for every track)

- **New branches start from `origin/main`.** Never branch from a stale
  local main or from another track's branch.
- **Continuing branches merge `origin/main` in** whenever it has advanced,
  at a task boundary — don't let a long-running branch drift.
- **Never use bare `git stash`.** If you need to shelve work, commit it or
  use a named stash; bare `stash` has bitten prior sessions when a worktree
  switch lost track of it.
- **Use a dedicated git worktree per track** (this file's own worktree is
  `/home/knobo/prog/dav/.claude/worktrees/zrythm-arch` — that pattern:
  `git worktree add <path> -b <branch> origin/main`, one path per track, so
  the tracks below can genuinely run in parallel without stepping on each
  other's working tree).
- **Foreground test runs only, `timeout`-guarded.** No backgrounding a test
  run and moving on — every gate in this project has been foreground since
  Plan A.

## 2. Standing constraints now in force (all tracks)

These are new or newly load-bearing as of Gate E closing — read them even
if you worked on this repo before Plan E:

- **The op log is ON.** `journal.ndjson` is a **persisted format** from
  now on: `OP_FORMAT_VERSION` (**2** since the post-merge follow-up PR —
  base64 state blobs, I-5) is load-bearing the moment any project has a
  journal file. Additive `#[serde(default)]` fields on an op or on
  `TxMeta` stay non-breaking; anything else (renaming a field, changing a
  variant's shape, removing a path) needs a version bump AND a reader that
  understands both shapes. The v1→v2 bump shipped WITHOUT a dual-shape
  reader on purpose, because the journal is still write-only — that
  freedom ends the moment Track A gives it a reader, and after that a
  change of this kind costs a migration.
- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`transact` closures must not panic** — no panic rollback until Plan F.
  Validate before mutating, every time, exactly as every landed op arm
  does.
- **The M-3 redo invariant**: a transient write must never touch a
  document field an entry's `ops` can address, or a pending redo silently
  lands on a different state than the entry recorded. If you add a new
  transient transaction (transport-like, engine-thread, or gesture
  mid-flight), this is the check to run before shipping it. As of the
  post-merge follow-up PR a `debug_assert!` in the commit path enforces it
  in debug builds (transient ops may address only `ObjectRef::Transport`,
  unless the batch is a mid-gesture fold) — so a violation now fails the
  test suite instead of waiting to be noticed.
- **Undo is bounded**: 200 entries, in-memory, bottom-eviction, cleared at
  epochs (project open/create/save-as). The journal itself is unbounded
  and append-only but currently has no reader (see Track A).
- **Foreground timeout-guarded test runs** and the **dated-count
  convention**: any task that changes test counts updates README.md +
  CONTRIBUTING.md in the same commit, with the date.
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## 3. The five parallel-safe tracks

All five are independently startable in separate worktrees right now
(subject to each track's own prerequisites, noted below). Cross-track
conflicts are called out explicitly — read the conflict notes for any pair
you plan to run truly concurrently.

### Track A — Plan F: history storage (round-2 §6, ADR 0005)

**The backend-core track.** Round-2 §6: copy-on-write B-tree session
store, version-graph retention, replay-only nodes at a 64 KB threshold
(per `benches/bulkbench/RESULTS.md`), placement-offset routing, a janitor
thread for off-RT eviction. This is what finally lifts the snapshot-rebuild
deferral that's been a carry-forward since the Plan A handoff
(`engine::rebuild` still holds the session lock across the whole graph
build) and gives the journal (write-only since Task 17) an actual reader.

**Convention**: author the plan doc FIRST, just-in-time, per
`docs/PHASE4-PLAN.md`'s stated convention ("Detailed, bite-sized task plans
live in `docs/superpowers/plans/` and are written just-in-time — each
sub-plan is authored when its predecessor lands, against the tree as it
then exists, not speculatively"). Do not start implementation before the
plan document exists. Use `superpowers:writing-plans`; Plan E's own
document
(`docs/superpowers/plans/2026-08-14-plan-e-side-channel-totality.md`) is
the template for shape (global constraints, scope rulings decided and
recorded up front, per-task Files/Interfaces/Steps/commit, self-review,
execution note).

**Footprint**: `src-tauri/src/control/history.rs`'s successor (today's
`History` — bounded `Vec` undo/redo, the 350ms same-key merge — becomes
the exposure layer over the new store), a new COW session store module,
`src-tauri/src/audio/engine.rs`'s `rebuild` (reads an immutable snapshot
instead of holding the session lock — this is the payoff).

**Consumes**: the journal format (v1, `OP_FORMAT_VERSION` load-bearing —
see §2 above), the undo bound (200 entries — Plan F may change the
retention policy but must not silently change user-visible undo depth
without a ruling), and the three recorded replay limitations R-3/L-1/L-2
from `docs/SIDE-CHANNEL-INVENTORY.md` (R-3: `seed_demo`'s Zyn bootstrap
rows outside an op — fold into Plan F's seed transaction; L-1:
`PluginRemove.params` unused on cold replay — needs an op-format decision;
L-2: `MidiSetNotes` mint sentinels re-mint on replay — same class).

**Prerequisites**: none blocking (Plan A landed the transaction channel
this builds under). PR #12 does not need to be merged first, but the plan
doc should be authored against whichever tree (branch or post-merge main)
you're actually implementing on, since Session's shape changed materially
across Plan E's tasks (plugins/automation moved in).

**Conflicts**: with Track B and Track D in `engine.rs` — see their
sections. Track A's `engine.rs` touch is the `rebuild` function
specifically; sequence with B/D if running genuinely concurrently
(smallest safe unit: land A's `rebuild` change first, since B and D both
build on top of "how does the engine read the document" more than they
change it).

### Track B — MIDI slice 2+: routing, recording, clock/sync out

Backlog doc: `docs/backlog/hardware-midi-io.md` (also read for slice 1's
already-landed shape and the design notes that led to this slice's cut).
Scope: MIDI-in routing to a track's instrument (live monitoring beyond
slice 1's preview-only tone), MIDI-in recording registered as ONE
`Actor::Engine` take transaction (`MidiClipAdd` with the captured notes —
"the op is the registration, never the recording itself," same pattern as
audio-recording finalize from Plan E Task 13), and MIDI clock + Start/Stop
output on transport changes (Hydrogen sync) driven by the engine-global
steady clock (Plan E Task 16) + the section table.

**REQUIRES both PR #12 AND PR #17 merged first** — this slice extends
slice 1's branch and needs the full post-Gate-E channel (take registration
as an op needs `Actor::Engine` transactions, which is Plan E's Task 13
machinery).

**Footprint**: `src-tauri/src/audio/engine.rs` (routing + recording +
clock-out on the engine's own turn), `src-tauri/src/midi_input.rs` (slice
1's module — port handling grows here), control-layer seams
(`ControlPlane` methods for attribution, mirroring the device-selection
carve-out pattern).

**Conflicts**: with Track A (`engine.rs`'s `rebuild`/session-read path)
and with Track D (`engine.rs`'s RT-attach work) — all three touch
`engine.rs`. Sequence the engine-touching halves: land whichever of A's
`rebuild` change or D's RT-attach lands first, then rebase the other two.
Do not run B, D, and A's `rebuild` work concurrently in the same file
without a merge plan.

### Track C — Multi-clip selection, group-drag, cross-track paste + cross-instance clipboard

Backlog doc: `docs/backlog/multi-clip-selection-and-paste.md`. Scope:
timeline multi-select (`Set<ClipId>` viewer state, mirrors PR #11's
note-level selection model), group drag as one `gesture_begin`/
`gesture_end`-wrapped multi-op transaction (audio and MIDI clips mix
freely in one tx — the channel is cross-store atomic), copy/paste at the
playhead with per-clip offsets preserved, paste-to-new-tracks, and a
cross-instance/OS-clipboard extension (an `application/x-aura-clips` JSON
payload + SMF fallback on the OS clipboard).

**Mostly frontend + a thin command layer** — the backend primitives this
needs (gesture IPC, `move_clip`, MIDI bounds ops, `ClipAdd`/`MidiClipAdd`,
undo) all landed in Plan E. **Conflict-light**: touches timeline
components + a view-state store + the clipboard glue; no `engine.rs`, no
overlap with A/B/D's backend footprint. The one shared surface is the
gesture IPC commands themselves (already frozen/additive, so no real
contention).

### Track D — Automation audible + lane UI

Backlog doc: `docs/backlog/automation-audible-and-ui.md`. Scope: RT attach
(the data layer landed in Plan E Task 10 — `AutomationLane` persists,
`Op::AutomationSetLane` is atomic/attributed/undoable — but
`engine::rebuild` never reads `session.automation`; compiled ramps and
`GainAutomatedNode` exist but aren't wired into the production graph),
then a timeline lane UI (draw/drag points, gesture-wrapped edits through
`automation_set`), then plugin-parameter targets.

**Start AFTER PR #12 (Plan E) lands** — the RT-attach half edits
`engine.rs`, which Plan E's Task 13 rewrote (`Committer`, `steady_time`).

**Footprint**: `src-tauri/src/audio/engine.rs` (attach at rebuild time —
the day this lands, `Op::AutomationSetLane`'s apply arm must flip
`effect.rebuild = true`; the arm's own comment already says this, marked
"REBUILD PIN" in `control/session.rs`), `src-tauri/src/plugins/automation.rs`
(the attach path), timeline components + a small store (UI).

**Conflicts**: with Track B (MIDI slice 2) and Track A in `engine.rs` — if
run in parallel, sequence the engine-touching halves (see Track B's note;
the same rule applies here).

### Track E — Library & browser panel

Backlog doc: `docs/backlog/library-and-browser.md`. Scope: a side panel
with three roots — samples (user-configured folders + a default library
dir, audition preview reusing the sampler-preview voice path), project
clips (drag back onto tracks), presets/instruments (Zyn patches via
`zyn_list_patches`, sampler instruments via `sampler_list_instruments`).
Drag-out lands on the existing, already-channel-routed import/clip-add
commands, so it's undoable for free.

**Footprint**: a new frontend panel component + a small store, new
scanning backend commands (`library_scan(dir)` or similar — additive), the
existing audition/preview path (no document coupling, Gate-safe category
already established by MIDI slice 1's monitoring). **Conflict-light**: no
`engine.rs`, no overlap with A/B/D.

## 4. Pointers (the master index and everything behind it)

- **`docs/backlog/00-ROADMAP-real-alternative.md`** — the master tiered
  index the owner asked for ("hva mangler for å være et reelt alternativ
  og faktisk brukandes til å lage gode ting?"). Lists what's already
  differentiating (landed), Tier 1 (weeks — includes Tracks B/C/D/E plus
  smaller **inline** items: metronome/click+count-in, piano-roll quantize,
  and the biggest single Tier-1 architecture item — **insert FX chains +
  sends/busses, candidate "Plan G"**, which needs its own research → plan
  → gates round because it touches the RT graph invariants round-2 §8
  reserves for the node-graph round, and is bound by the standing "PDC
  before sends ship" rule), and Tier 2 (months — time-stretch, pattern
  instancing, takes/comping, stems export, freeze/bounce, external
  instrument tracks, two-instance coexistence). **Start here** for
  anything not already one of the five tracks above.
- `docs/superpowers/plans/2026-08-14-plan-e-side-channel-totality.md` —
  Plan E's own plan doc (scope rulings, non-goals, all 18 tasks).
- `docs/SIDE-CHANNEL-INVENTORY.md` — the landed inventory, carve-outs, and
  recorded replay limitations (L-1/L-2/L-3) that Track A consumes.
- `docs/PHASE4-PLAN.md` — orchestration; "Plan E handoff" section has
  every scope ruling, mid-flight ruling, and deferred-minor from the
  ledger, verbatim.
- `.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/progress.md` —
  the full SDD ledger (gitignored) if you need more detail than the
  handoff section carries — every `Ruling:` and `minor (deferred):` line
  is there with full context.
- `docs/backlog/hardware-midi-io.md`, `docs/backlog/multi-clip-selection-and-paste.md`,
  `docs/backlog/automation-audible-and-ui.md`, `docs/backlog/library-and-browser.md`
  — Tracks B/C/D/E's own docs, read in full before starting that track.
- `docs/CORE-REDESIGN-ROUND-2.md` (ACCEPTED) §6 for Plan F's spec; ADR
  0005 for the history-storage decision it implements.

---

*Working note, committed on the PR branch for session continuity. When any
track lands, update the relevant section of this file (or, once a track's
own follow-on work is substantial, split it into its own next-prompt-style
pointer) so the next fresh session isn't reading stale status.*
