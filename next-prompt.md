# Next: Plan F (history storage) — and four parallel-safe post-Plan-E tracks

Read this file, then pick a track and do the work. Reply to the user in
Norwegian — they write Norwegian; the repo documentation is English.

## Post-merge whole-branch review findings (read first)

The final whole-branch review of Plan E (`15c9909..27911d8`) is at
`.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/final-review-report.md`
(verdict: NEEDS FOLLOW-UP PR). Its **FIX NOW** triage list is done —
follow-up PR #18, `fix/plan-e-followup`:

- **C-1 (Critical)** — no epoch guard on `HistoryLog::record_commit`/
  `record_gesture`; a commit racing an epoch boundary journaled into the
  NEW project's file and pushed a live undo entry for the OLD document.
  → fixed in PR #18 (the urgent item; the other four were bundled
  behind it).
- **I-2** — LoopJam `watch_and_apply` busy-spun at 100 % CPU when a
  retryable `apply` kept failing with the transport stopped.
  → fixed in PR #18 (back-off + bounded retries + the mid-air-race
  test the Task 8 ledger asked for).
- **Task 13 deferral** — the deadlock audit's five stale `request`
  call-site line citations. → fixed in PR #18.
- **I-5 + L-1** — plugin state blobs serialized as JSON number arrays
  (~4x); `Op::PluginRemove.params` was captured but never read.
  → fixed in PR #18 (`OP_FORMAT_VERSION` 2, base64 blobs, apply
  seeds the mirror from the op on cold replay).
- **I-4's two caveats** — journal line order vs `rev` order under
  concurrency, and a panicking `transact` diverging log from document.
  → recorded as L-4/L-5 in `docs/SIDE-CHANNEL-INVENTORY.md` in PR
  #18 (records, not fixes — the structural fix is Track A's).
- **M-3** — the transient/redo invariant was a comment checked by nothing.
  → fixed in PR #18 (a `debug_assert!` in the commit path).

**Still open, deliberately HELD for the owner with the context** (do NOT
fix these blind — read the report's entry first):

- **I-1** `save_project_as_epoch` writes only project.json + midi, so
  Save-As silently drops plugin `.state` blobs and automation chunks —
  and **I-7** a new/opened project inherits the previous project's plugin
  rows when `project.json` has no `plugins` key. These two are the
  branch's real data-loss surface. **Owner: the epoch/persist path — take
  them together, they interact (see also R-3).**
- ~~**I-3** `execute_host_forward` writes `status`/`params` with no op, no
  epoch guard, and no inventory residual (with **M-6**).~~ → **closed by
  Track D** (`061786b`): epoch guard + residual R-4 + the grep-gate
  enumeration corrected.
- **I-6** `undo`/`redo` are sync Tauri commands and can block the UI
  thread on plugin re-instantiation + disk I/O. **Owner: Track A** (it
  owns undo/redo's substrate; `async` + `spawn_blocking`, mirroring
  `seed_demo_project`).
- **C-1 residual, found while fixing C-1 and deliberately NOT fixed**:
  `undo`/`redo` pop an entry, commit, then push it back via `push_redo`/
  `push_undo_unchanged`. An epoch boundary landing between the pop and the
  push RESURRECTS that entry onto the new document's stack — the same
  class as C-1, at the entry-MIGRATION path rather than the recording one.
  The commit itself is now correctly dropped by C-1's guard, so the
  journal half is closed; the stack half needs `Committed.epoch` plumbed
  through `undo`/`redo`. **Bundle with I-6** — it restructures exactly
  that code, and doing both at once is one edit instead of two.
- ~~**I-8** inventory row 13 claims the per-knob `project.json` rewrite is
  closed; only its position moved off the lock, the frequency is
  unchanged.~~ → **closed by Track D** (`7ef1f70`/`feec7e9`/`bb20280`):
  gesture-scoped persist DEFERRAL (folding alone was not enough — a
  transient commit still runs its full `EngineEffect`), so a knob or lane
  drag is one undo entry and one `project.json` write; row 13's wording
  corrected.
- **Minors M-1, M-2, M-4, M-5, M-7, M-8** (dirty_state clear race,
  Ctrl+S cannot recover a failed auto-persist, undo during an open
  gesture bypasses the fold, the Gate E precision sentence, `VecDeque`
  for the undo stack, the Figma oracle's omitted derived fields) —
  recorded in the report, unowned, all cheap. **M-9 is RESOLVED** by the
  review itself (`ClapNode::reset` verified to leave `steady_fallback`
  alone); close that ledger item.
- ~~**M-3 (frontend)** undo/redo re-pull misses automation and plugin
  panels.~~ → **closed by Track D** (`2a11ed0`).

**New, from Track D, still open** (details in `docs/PHASE4-PLAN.md`'s
"Track D handoff"):

- **The ear check is OWED.** Nobody has heard an automation lane change
  the volume during playback — the implementing agents had no audio
  device. It is the sole verification of `engine.rs:884`, the line the
  whole engine task exists for. **Do this before anything else:** start
  the app, draw a fade on a track, press play, listen.
- **A bounce ignores PLUGIN-PARAM automation** and captures whatever the
  live host instance happens to hold. Track-gain lanes DO export
  correctly. The fix needs private per-render plugin instances — see the
  handoff and the note on `audio::offline::build_graph`.
- **The non-blocking CLAP param path.** `clap_host::set_params` is
  `plugin_main().run(…)`, a blocking round-trip; `lv2_host::set_params`
  already posts, so only CLAP blocks. Writes are batched per instance per
  tick, but an active ramp on two instances still costs ~1000 blocking
  round-trips/s onto the plugin-main thread that also serves the param
  panel, `instantiate` and `save_state`. Wanted: a fire-and-forget
  sibling to `set_params` plus a driver that uses it. **Owner: the
  plugin-host path.**
- **An automated plugin param is PINNED for the whole playthrough**, not
  just while a knob is held. The "A" button is the only way to create a
  plugin-param lane and it mints a single-point (flat) lane, and the lane
  editor draws track gain only — so there is no way to give a plugin param
  a curve yet. Turn that param in the plugin's own GUI during playback and
  it snaps back within ~0.5 s and stays, while AURA's panel still shows the
  new value. Intended scope (automation overrides the knob), but say so
  plainly. Follow-ups that change it: a curve editor for plugin params, and
  write/touch/latch modes.
- **`gesture_end` has no id — it closes whatever is open. TRACKS A AND C
  INHERIT THIS.** Any `endGesture()` fired from a promise continuation can
  close a gesture that began while it was awaiting; `gesture_begin`
  auto-closes a stale one, so the two compose into a real regression.
  Live example, Track D's own: release a plugin knob (awaits a rAF + one
  IPC round trip) and press a track fader inside that window — the pending
  `endGesture()` closes the FADER's gesture, and the rest of that drag
  commits unbracketed, one undo entry and one `project.json` write per rAF
  batch. That is the I-8 regression inside I-8's own fix. Two more
  instances (`library.svelte.ts`, the automation delete path) are listed
  in the handoff. Fix to make: `gesture_begin` returns an id,
  `gesture_end(id)` no-ops on mismatch — additive, so it stays inside the
  frozen-command rules. Worst case is an extra undo entry and an extra
  persist, never data loss. **If your track drives gestures from an async
  path, read the handoff entry before writing that code.**
- **No DOM test environment exists** (no jsdom/testing-library), so nothing
  inside a `.svelte` file is covered by any test. Both of Track D's real
  frontend bugs lived in event handlers and both were found by reading.
  Move async-ordering logic into a store where it can be tested.
- **Two UI minors, deliberately left open** by the whole-track review:
  `movePoint` silently deletes a neighbour on a tick collision
  (`automation-edit.ts:57-66`), and `.tog.auto.on` is byte-identical to
  `.tog.arm.on` in `TrackHeader.svelte`, so an automation-visible track
  reads as armed.

This file is written for a **fresh session after `/clear`**: it assumes no
memory of the Plan E conversation. Everything it asserts is checked against
git/README at write time (2026-08-14) — trust files over this file if they
disagree, and update this file (marked correction, ADR 0007) if they do.

## 1. State of the world

The project is **AURA**, an AI-native DAW: Tauri v2 + Svelte 5 around a
lock-free real-time Rust engine (`src-tauri/`), local AI sidecars, and an
embedded MCP server so agents mutate the session alongside the user.

**Plan E (the side-channel totality) is IMPLEMENTED, Gate E is CLOSED, and
PR #12 is MERGED.** The owner ordered the merge; `main` now carries
squash commit `27911d8` ("Plan E — the side-channel totality; Gate E
closed, op log ON"). Branch `plan-e-side-channels` is **kept** (not
deleted) so every SHA cited in this file and in `docs/PHASE4-PLAN.md`'s
"Plan E handoff" section still resolves — the branch's own history is the
task-by-task record; `27911d8` is just its squashed shape on `main`. A
post-merge whole-branch review is running as a follow-up; nothing in this
file depends on its outcome. **`main` now contains Plan A + B + C+D +
Plan E's full channel rewrite — a fresh session branching from
`origin/main` today gets the whole channel, undo/redo, and the journal for
free.** Full handoff (every scope ruling, every mid-flight ruling, every
carry-forward, the deferred-minors roll-up): `docs/PHASE4-PLAN.md`'s
**"Plan E handoff"** section, appended after the "Plan C/D handoff"
section, same conventions. The landed side-channel inventory (34 rows, all
closed, plus residual carve-outs R-1..R-3 and recorded replay limitations
L-1..L-5, of which L-1 is now closed and L-4/L-5 were added by the
post-merge follow-up PR #18): `docs/SIDE-CHANNEL-INVENTORY.md`.

**Also open: PR #17**, `midi-input-ports` — hardware MIDI input slice 1
(port list/select + activity indicator + live monitoring, `midir`,
owner-verified end-to-end with an LPK25). It has been updated with
post-merge `origin/main` (conflict-free, suites green) and is **cleanly
mergeable now** (`gh pr view 17` — `mergeStateStatus: CLEAN`), but is
**still OPEN** — verify its status before starting Track B, which needs it
merged. (Historical: PR #17 was merged as `3340aa8`, which is the base
Track B's slice 2 branched from.)

Main also carries **PR #9** (timeline/piano-roll horizontal scrollbars),
**PR #10** (interface-zoom preference), **PR #11** (piano-roll note
selection + copy/paste ops, all three folded into PR #12's diff via its
own mid-flight merge, commit `f886306`), plus **PR #13** (preferences
system), **PR #14** (prefs slider commit-on-release), **PR #15** (sidecar/
MCP setup docs), and **PR #16** (adaptive top-bar overflow) — all merged
independently of Plan E, before or around the same time as PR #12. Nothing
further to do about any of them; they're why the frontend baseline moved
past Plan E's own count (see below).

**Baseline to verify at the start of any track**: MEASURE IT, don't trust
this line — several tracks are in flight and each moves the number, so it
is a standing merge-conflict point. Marked correction (ADR 0007): this
section previously said **506 backend + 206 frontend**, which was already
stale when written — the real count at `3340aa8` (Track D's branch base,
verified in a worktree) was **527 backend + 206 frontend**. Known points
since: **538 + 234** after Track E (`a98d7ff`), and **566 backend (537
lib + 29 integration) + 258 frontend** on `automation-audible`
2026-08-15 (Track D, PR #20, includes Track E via a merge), and **662
backend (633 lib + 29 integration) + 271 frontend** on `midi-slice-2`
2026-08-15 (Track B, PR #21, includes both E and D via a merge). Doc-tests
report 0 and are not a test target — do not add them to the count. Run
both suites before writing the first line of a track:

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

**Consumes**: the journal format (**v2**, `OP_FORMAT_VERSION` load-bearing
— see §2 above; the freedom to reshape it without a dual-shape reader ends
the moment this track ships a replayer), the undo bound (200 entries —
Plan F may change the retention policy but must not silently change
user-visible undo depth without a ruling), and the recorded replay
limitations R-3/L-2/L-4/L-5 from `docs/SIDE-CHANNEL-INVENTORY.md` (R-3:
`seed_demo`'s Zyn bootstrap rows outside an op — fold into Plan F's seed
transaction; L-2: `MidiSetNotes` mint sentinels re-mint on replay; L-4:
journal FILE order is not `rev` order under concurrent committers, so this
track's reader MUST sort by `(epoch, rev)` — the structural fix, a
commit-sequence lock or out-of-order buffering in `record_commit`, is this
track's to make; L-5: a panicking `transact` closure diverges log from
document permanently, which is the strongest argument for the panic
rollback this track is also the right home for). **L-1 is closed** by PR
#18 — `PluginRemove.params` is now read on apply.

**Prerequisites**: branch straight from `origin/main` — it already contains
Plan E's full channel rewrite (PR #12 merged, squash `27911d8`), so
`Session`'s shape (plugins/automation moved in, the `Committer`, the
journal/history layer) is present from the start. No separate merge step
needed.

**Conflicts**: with Track B and Track D in `engine.rs` — see their
sections. Both have LANDED (PR #21, PR #20), so Track A rebases onto them
rather than sequencing with them. Track B's engine footprint is THREE
places, not the "one line" the plan predicted — count on all three when
planning the `rebuild` rewrite: the hub read at `engine.rs:1070`
(before the session lock is taken), the `append_from_with_input`
target-track argument at `:1159` (sourced from the hub, not the
session, so a snapshot-based read does not change its value), and
`follow_live_in_target` (`:1221`), called from the engine loop at
`:880`, which rebuilds when the routing target changes and then
re-resolves it. Track A's `engine.rs` touch is the `rebuild` function
specifically; sequence with B/D if running genuinely concurrently
(smallest safe unit: land A's `rebuild` change first, since B and D both
build on top of "how does the engine read the document" more than they
change it).

### Track B — MIDI slice 2: routing, recording, clock/sync out — LANDED 2026-08-15

**Done, branch `midi-slice-2`, PR #21** (12 tasks, plan doc
`docs/superpowers/plans/2026-08-14-midi-slice-2.md`). Full handoff — the
scope rulings later rounds inherit, the standing hazards, the deferred
minors and the three ear checks the owner still owes — is
**`docs/PHASE4-PLAN.md`'s "Track B handoff"** section; read that, not this
paragraph, before touching hardware MIDI.

A `MidiInHub` (`src-tauri/src/audio/midi_in.rs`) rings hardware MIDI from
the midir callback thread into the RT output callback, where the mixer
dispatches it into the target track's own live node — so a keyboard plays
the track's real instrument, playing or stopped. A take is captured
control-side and registered as ONE `Actor::Engine` transaction:
`Op::MidiClipAdd` rides inside the existing `commit_recording_finalize`
alongside the audio `ClipAdd`s, so a take is one undo entry. No new op
kind, no shape change, `OP_FORMAT_VERSION` stays 2. MIDI OUT is a
dedicated non-RT `aura-midi-out` thread (`src-tauri/src/midi_out.rs`) over
`SharedRt` + a tempo/event snapshot — 24 PPQN clock, Start/Stop/Continue/
SPP, and note-out from one chosen MIDI track — and it touches `engine.rs`
not at all. User docs: `docs/midi-input.md`, `docs/midi-output.md`.

**Rulings a later round inherits** (all in the plan, ADR 0007-marked): (1)
the MIDI-in target track is app config behind a `ControlPlane` seam, NOT
derived from `TrackState.armed` — the frontend calls it from the same arm
click; (4) the clock's musical position is `SharedRt::position` + the
`TempoMap`, not `steady`; (5) a backward jump re-cues the slave with
`Stop`/SPP/`Continue`; (9) MIDI-in timestamps are quantised to the audio
block; (10) note-out is a routing carve-out, not a track field; (11) no
persistence of ports/targets across restarts while port ids are unstable.

**What is left**, in priority order: the owner's **three ear checks**
(note-out to real gear, Hydrogen sync, a real keyboard through
arm→record→undo→reopen) — nothing in the test suite substitutes; then
**recording under an active loop, which silently produces a musically
wrong take** (mechanism, both candidate fixes and why the plan's non-goals
do not cover it: `docs/backlog/hardware-midi-io.md`, "Still open after
slice 2" — the choice between the two fixes is a behaviour decision for
the owner); then sub-block timestamping, port persistence, MIDI thru, CC/
pitch-bend, a MIDI-out latency offset, and the document-model
external-instrument target.

### Track C — Multi-clip selection, group-drag, cross-track paste + cross-instance clipboard — LANDED 2026-08-15

**Done, branch `multiclip-clipboard`, PR #22** (13 tasks, plan doc
`docs/superpowers/plans/2026-08-14-multiclip-selection-paste.md`). Full
handoff — all eight scope rulings, the mid-flight rulings, the
deferred-minors roll-up and what is still OWED — is
**`docs/PHASE4-PLAN.md`'s "Track C handoff"** section; read that, not this
paragraph, before touching selection, group drag or the clipboard.

The timeline has a real multi-selection (click / shift / ctrl / marquee) as
**viewer state** — a `Set<"<kind>:<id>">` that never enters the document and
that the backend never reads. A group drag or MIDI group resize is ONE
gesture and ONE undo entry through the new `move_clips`; copy/paste rides a
backend-built `application/x-aura-clips` JSON envelope through two `arboard`
commands, so it works across two running AURA instances. Five new additive
commands: **`move_clips`, `clips_copy`, `clips_paste`,
`os_clipboard_write_text`, `os_clipboard_read_text`**. Keys: Ctrl+C,
Ctrl+V, Ctrl+Shift+V (paste to new tracks), Ctrl+Shift+M (export the
selection as .mid).

**Rulings other tracks must not contradict:** no new op kind, no new
`PropPath`/`ObjectRef` variant, `OP_FORMAT_VERSION` still **2** (paste
composes `TrackAdd`/`ClipAdd`/`MidiClipAdd` in one transaction); selection
is viewer state and never document state; the clipboard wire's tempo domain
is the NOMINAL 48 kHz rate; SMF is export-only (the OS clipboard slot this
app can own is plain text); `engine.rs` untouched.

**Still owed, and it belongs to the owner:** the manual **cross-instance
check** (copy in instance A, Ctrl+V in instance B) — the one link no test in
this repo can make, with a **binding no-input-automation rule**; procedure
and rationale in the handoff. Also read the handoff's **clipboard size
ceiling** before promising anyone large-selection copy between windows: a
failed large copy empties the system clipboard for every application.

**Left deliberately undone:** multi-clip delete/duplicate (Delete and the
`×` from PR #26 stay single-clip — a batch `clips_remove` in ONE transaction
is the prerequisite, see the handoff), arrow-key nudge over a selection,
audio-clip resize, true content instancing, SMF paste, and the hardcoded MCP
port 41717 that real two-instance workflows still hit.

Baseline after this track: **729 backend (700 lib + 29 integration) + 359
frontend**, measured on `multiclip-clipboard` 2026-08-15.

### Track D — Automation audible + lane UI — LANDED 2026-08-14

**Done, branch `automation-audible`, PR #20** (10 tasks, plan doc
`docs/superpowers/plans/2026-08-14-automation-audible.md`). Full handoff —
all eight scope rulings, the non-goals, the mid-flight rulings and the
deferred-minors roll-up — is **`docs/PHASE4-PLAN.md`'s "Track D
handoff"** section; read that, not this paragraph, before touching
automation.

`engine::rebuild` now reads `session.automation`: track-gain lanes compile
into a slot-indexed `RtGraph::gain_ramps` table applied at the mixer's
per-track gain stage (NOT by wrapping nodes in `GainAutomatedNode` — see
ruling 1), and plugin-param lanes are driven on the engine control thread's
≤2 ms tick, host-only, never through the document (ruling 2). The REBUILD
PIN in `control/session.rs` is resolved. The timeline gained an in-lane
overlay for drawing/dragging/deleting points, plus a plugin-param target
picker. Gestures grew a persist deferral, so a drag is one undo entry and
one `project.json` write. Song export compiles the same gain ramps.

Three Plan E findings closed on the way: **I-3 + M-6**, **I-8** (inventory
row 13) and the **frontend M-3** — struck from the held list above.

Still open and named there: the **owner's ear check** (nobody has heard it
yet), **plugin-param automation in a bounce**, the **non-blocking CLAP
param path**, and holding a knob against a flat automated param.

Baseline after this track: **566 backend (537 lib + 29 integration) + 258
frontend**, measured on `automation-audible` 2026-08-15.

### Track E — Library & browser panel — LANDED 2026-08-14

**Done, branch `library-browser` (9 tasks, plan doc
`docs/superpowers/plans/2026-08-14-library-browser.md`).** The LIB dock tab
browses three roots: SAMPLES (default library folder + user-configured
folders, click-to-audition without touching the project), CLIPS (the open
project's audio/MIDI clips, draggable back onto tracks), PRESETS (loaded
sampler instruments + ZynAddSubFX bank patches). Four additive backend
commands: `library_scan`, `library_default_root`, `library_audition`,
`library_audition_stop`. Every drag-out lands through the existing,
already-channel-routed import/clip-add commands, so it's undoable for
free — no new document state, no engine.rs touch. The folder list is a
frontend preference: the plan added a new `pathList` preference kind to
`src/lib/prefs/schema.ts` (Preferences → LIBRARY), not a backend config
file.

Deferrals (recursive background scan, metadata probe, waveform
thumbnails, on-disk `.sfz`/`.xiz` browsing, SMF drag-in, tagging/
favourites/search, drag-out to the OS) are recorded explicitly, not
silently, in `docs/backlog/library-and-browser.md`'s updated "Suggested
cut" section — each one line, each dated. The plan doc's scope rulings
1-11 are the reasoning behind each deferral; they still need to be copied
into `docs/PHASE4-PLAN.md`'s Track E handoff section when one is written
(not done as part of this track — `PHASE4-PLAN.md` wasn't touched here).

Baseline after this track: 538 backend + 234 frontend tests, all green
(counted 2026-08-14 on `library-browser`; superseded by Track D's
566 + 258 above, which includes this track).

## 4. Pointers (the master index and everything behind it)

- **`docs/backlog/00-ROADMAP-real-alternative.md`** — the master tiered
  index the owner asked for ("hva mangler for å være et reelt alternativ
  og faktisk brukandes til å lage gode ting?"). Lists what's already
  differentiating (landed), Tier 1 (weeks — includes Tracks B/C/D/E plus
  smaller **inline** items: metronome/click+count-in, piano-roll quantize,
  and the biggest single Tier-1 architecture item — **insert FX chains +
  sends/busses, Plan G**, product decision in
  `docs/backlog/insert-fx-sends-sidechain.md`: host already-scanned
  CLAP/LV2 effects, do not write a stock FX suite; G1 inserts+PDC, G2
  bus+sends, G3 sidechain taps, G4 envelope-follower later. Still needs
  its own research → plan → gates round because it touches the RT graph
  invariants round-2 §8 reserves for the node-graph round, and is bound
  by the standing "PDC before sends ship" rule), and Tier 2 (months — time-stretch, pattern
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
- `docs/backlog/insert-fx-sends-sidechain.md` — Plan G product decision
  (inserts / sends / sidechain / what we will not write). Read before
  opening the graph-compiler plan round.
- `docs/backlog/external-instrument-return.md` — MIDI-out's missing
  half: per-track audio return, visible freeze clips, PipeWire as a
  helper. Read before adding "hidden tracks" or a PW graph orchestrator.
- `docs/CORE-REDESIGN-ROUND-2.md` (ACCEPTED) §6 for Plan F's spec; ADR
  0005 for the history-storage decision it implements.

---

*Working note, committed on the PR branch for session continuity. When any
track lands, update the relevant section of this file (or, once a track's
own follow-on work is substantial, split it into its own next-prompt-style
pointer) so the next fresh session isn't reading stale status.*
