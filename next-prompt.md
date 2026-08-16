# Next: after Plan F

Plan F (history storage) is **IMPLEMENTED and MERGED** as squash `6af46dd`
(**PR #23**, 2026-08-16). `origin/main` is the baseline for new work.
Read the Plan F handoff in `docs/PHASE4-PLAN.md` before touching
snapshots, the journal reader, the version graph, or `engine::rebuild`.
Branch `plan-f-history` is **kept** so task SHAs cited in the handoff
still resolve.

Pitch Coach **phase 1** (YIN, InputHub, listen/rehearse commands) is in
**[PR #49](https://github.com/knobo/aura-daw/pull/49)** on `feat/pitch-coach`.
Owner checkpoint before any panel: listen, sing a known pitch, read the
frames. Phase 2 is the panel; phase 3 is scoring.

The five parallel tracks (A–E / F) are all landed. What remains is
named below: owner ear-checks, Plan F carry-forwards, Track D/B leftovers,
modulation design §8, and Plan G. Do not re-open a closed Plan F item.

**In flight — do not start these, other agents own them:**
- **PR #42** `feat/midi-launch-map` (open, CONFLICTING with the Plan F
  squash — rebase onto `origin/main` before more work).
- `feat/pitch-coach` (worktree, tasks 2–4 recorded done).
- `feat/external-audio-editor` (worktree, one commit ahead of pre-Plan-F
  main).

Read this file, then pick a leftover and do the work. Reply to the user
in Norwegian — they write Norwegian; the repo documentation is English.

## Track F (modulation system) — LANDED; path to the finished system

Track F is **IMPLEMENTED** on branch `modulation-system` (11 tasks, design
+ plan under `docs/superpowers/`). Full handoff — per-task commits,
controller rulings under R5, non-goals held, divergences — is
**`docs/PHASE4-PLAN.md`'s "Track F handoff"** section. The decision record
is **ADR 0008** (`docs/adr/0008-modulation-graph.md`).

**The ordered path to the finished modulation system** (ports, modulators,
macros, curve shapes, recording, sample-accurate plugin params, lazy
expansion, per-voice modulation) is **design §8** — do not restate it
here; open:

→ [`docs/superpowers/specs/2026-08-15-modulation-system-design.md` §8](docs/superpowers/specs/2026-08-15-modulation-system-design.md#8-the-path-to-the-finished-system)

R2 requires that path to stay findable in local files (design, this file,
ADR 0008). When a later round ships any item from §8, update the handoff
and re-point if the section moves.

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
  #18; **both CLOSED by Plan F** (PR #23). L-4: reader sorts by
  `(epoch, rev)`, undo stack is rev-ordered. L-5: `catch_unwind` +
  snapshot restore.
- **M-3** — the transient/redo invariant was a comment checked by nothing.
  → fixed in PR #18 (a `debug_assert!` in the commit path).

**Held review findings** (do NOT re-open a closed item; remaining
unowned notes are M-8 and owner ear-checks — read the report first):

- ~~**I-1** `save_project_as_epoch` writes only project.json + midi, so
  Save-As silently drops plugin `.state` blobs and automation chunks —
  and **I-7** a new/opened project inherits the previous project's plugin
  rows when `project.json` has no `plugins` key.~~ → **closed by Plan F
  Tasks 1–2** (I-1 option (b): Save-As writes plugin/automation/
  modulation into the new dir; I-7: adopt-on-open clears when the file
  has no `plugins` key). Residual: option (a) — flush outgoing persist
  before an epoch swap — is deferred (ruling F-6).
- ~~**I-3** `execute_host_forward` writes `status`/`params` with no op, no
  epoch guard, and no inventory residual (with **M-6**).~~ → **closed by
  Track D** (`061786b`): epoch guard + residual R-4 + the grep-gate
  enumeration corrected.
- ~~**I-6** `undo`/`redo` are sync Tauri commands and can block the UI
  thread on plugin re-instantiation + disk I/O.~~ → **closed by Plan F
  Task 4** (`async` + `spawn_blocking`, epoch-guarded, serialized by
  `history_gate`).
- ~~**C-1 residual** — `undo`/`redo` pop an entry, commit, then push it
  back; an epoch between pop and push resurrects it onto the new
  stack.~~ → **closed by Plan F Task 4** (`Committed.epoch` plumbed
  through undo/redo; mismatch drops the entry).
- ~~**I-8** inventory row 13 claims the per-knob `project.json` rewrite is
  closed; only its position moved off the lock, the frequency is
  unchanged.~~ → **closed by Track D** (`7ef1f70`/`feec7e9`/`bb20280`):
  gesture-scoped persist DEFERRAL (folding alone was not enough — a
  transient commit still runs its full `EngineEffect`), so a knob or lane
  drag is one undo entry and one `project.json` write; row 13's wording
  corrected.
- ~~**Minors M-1, M-2, M-4, M-5, M-7**~~ → **closed by Plan F**
  (Tasks 3 / 4 / 11 / 13). **M-8** (Figma oracle omitted derived
  fields) is still recorded, unowned. **M-9 is RESOLVED** by the
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
  just while a knob is held. Track F shipped the curve editor (lane
  picker + overlay), so a plugin param *can* have a drawn curve now —
  the remaining gap is write/touch/latch, not "no way to draw". Turn
  that param in the plugin's own GUI during playback and it snaps back
  within ~0.5 s and stays, while AURA's panel still shows the new
  value. Intended scope (automation overrides the knob), but say so
  plainly.
- ~~**`gesture_end` has no id — it closes whatever is open.**~~ →
  **closed on `fix/gesture-end-id`**: `gesture_begin` returns the
  gesture's run id; `gesture_end(id)` no-ops on mismatch (omitting `id`
  keeps the old close-whatever contract). Async callers (plugin knobs,
  library stamp, lane/envelope delete+commit, clip-drag, faders, tempo)
  hold the token across the await.
- **No DOM test environment exists** (no jsdom/testing-library), so nothing
  inside a `.svelte` file is covered by any test. Both of Track D's real
  frontend bugs lived in event handlers and both were found by reading.
  Move async-ordering logic into a store where it can be tested.
- ~~**Two UI minors** — `movePoint` deleted a neighbour on a tick
  collision, and `.tog.auto.on` was byte-identical to `.tog.arm.on`.~~
  → **closed by PR #32** (`feat/lowhanging-fl-fruits`): a colliding
  drag keeps the neighbour's tick; automation-visible is magenta, ARM
  stays red. Piano-roll **Q / Shift+Q** (quantize) landed in the same
  PR.

This file is written for a **fresh session after `/clear`**. Everything
it asserts was checked against the `plan-f-history` tree at write time
(2026-08-16, after PR #23's review follow-up). Trust files over this
file if they disagree, and update this file (marked correction, ADR
0007) if they do.

## 1. State of the world

The project is **AURA**, an AI-native DAW: Tauri v2 + Svelte 5 around a
lock-free real-time Rust engine (`src-tauri/`), local AI sidecars, and an
embedded MCP server so agents mutate the session alongside the user.

**Plan F (history storage) is MERGED** (`6af46dd`, PR #23). Tasks 1–13
plus the persist/clipboard review follow-up. A fresh session branching
from `origin/main` gets the published `SessionSnapshot`, lock-free
rebuild assembly, version graph, panic rollback, journal reader
(detection only), and placement offsets. Full handoff:
`docs/PHASE4-PLAN.md`'s **"Plan F handoff"**.

Plan E is already on `main` (squash `27911d8`, PR #12). Its follow-up
PR #18 is also merged. Branch `plan-e-side-channels` is **kept** so SHAs
cited in the Plan E handoff still resolve. The landed inventory is
`docs/SIDE-CHANNEL-INVENTORY.md`: R-3/L-4/L-5 CLOSED (Plan F); residuals
are R-1, R-4, and the F-6 outgoing-persist skip.

**PR #17** (`midi-input-ports`) is **merged** (`3340aa8`) — that was
Track B's prerequisite. Tracks B/C/D/E/F have their own landed PRs
(#21/#22/#20/library-browser/`modulation-system`). Nothing further to
do about those merges.

**Baseline to verify at the start of any leftover**: MEASURE IT, don't
trust this line. Current measured count on `fix/gesture-end-id` 2026-08-16 (after the
gesture-token leftover, on top of Plan F's 900): **901 backend
(867 lib + 34 integration, plus 2 `#[ignore]`) + 456 frontend**.
Doc-tests report 0 and are not a test target. Run both suites before
writing the first line of a leftover:

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
  understands both shapes. Plan F's journal reader (`control/replay.rs`)
  requires `v == 2` on batch lines (ruling F-5); v1 lines are skipped
  with a warn. A change of this kind now costs a migration.
- **Thin renderer** (ADR 0006) still holds: no new authoritative state,
  business logic, or time math lands frontend-side. Every frontend change
  is op emission, gesture emission, or UI/chrome.
- **Frozen command/event names stay frozen**; bodies become wrappers; new
  commands are additive (the same rule that shaped every Plan E task).
- **`transact` closures must not panic.** Plan F landed panic
  *containment* (`catch_unwind` + restore from the pre-tx snapshot;
  ruling F-3) — that is a crash-consistency net, not a license.
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
  epochs (project open/create/save-as). The journal is unbounded and
  append-only; Plan F added a reader (detection + primitive, no
  auto-apply — ruling F-8).
- **Foreground timeout-guarded test runs** and the **dated-count
  convention**: any task that changes test counts updates README.md +
  CONTRIBUTING.md in the same commit, with the date.
- **Gesture lock order is gesture-before-session, everywhere** (Task 14's
  fix) — if a track adds a new gesture-shaped commit path, follow this
  order or reintroduce the TOCTOU that fix closed.

## 3. The five tracks (all landed)

Historical record. Do not restart these. Leftovers live in each
"Still owed" / carry-forward list, not as a sixth parallel track.

### Track A — Plan F: history storage — LANDED 2026-08-16

**Done, branch `plan-f-history`, PR #23** (13 tasks, plan
`docs/superpowers/plans/2026-08-14-plan-f-history-storage.md`). Full
handoff — rulings F-1..F-12, lifted carry-forwards, new carry-forwards —
is **`docs/PHASE4-PLAN.md`'s "Plan F handoff"**; read that before touching
`SessionSnapshot`, `engine::rebuild`, the journal reader, or the version
graph. Implementation note on ADR 0005 (per-clip Arc, tree deferred) is
under Consequences.

What landed: published snapshot + lock-free rebuild assembly, version
graph (64 KB replay-only, janitor), panic rollback in `transact`, journal
reader (detection only), rev-ordered undo stack, R-3 closed, MIDI
placement offsets (including clipboard copy/paste), persist_gate +
re-dirty on a stale write. The Plan A `engine.rs` sequencing note is
**resolved** — assembly no longer holds the session lock.

Carry-forwards for later rounds live in the handoff: live-document B-tree
(trigger = note-delta op), I-1 option-(a) residual, no auto-apply of
journal tails, seeded-PRNG for future random ops, version-graph product
surface unbuilt.

Baseline after this track: **900 backend (866 lib + 34 integration, plus
2 `#[ignore]`) + 456 frontend**, measured on `plan-f-history` 2026-08-16
after the PR #23 review follow-up (rebased onto main). Known flakes:
`midi_out` under parallel lib runs;
`apply_hum_clip_commits_synchronously_and_announces_project_changed`.

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

**Superseded for the document model by Track F** (below): lanes become
curve+binding pairs under `session.modulation` / project v4; the lane IPC
remains as a compatibility facade. Track D's engine rulings 1, 2, 6 and
gesture rulings 4/7 still stand.

Baseline after this track: **566 backend (537 lib + 29 integration) + 258
frontend**, measured on `automation-audible` 2026-08-15 (superseded by
Track F's 731 + 299).

### Track F — Modulation system — LANDED 2026-08-16

**Done, branch `modulation-system`** (11 tasks, design
`docs/superpowers/specs/2026-08-15-modulation-system-design.md`, plan
`docs/superpowers/plans/2026-08-15-modulation-system.md`, **ADR 0008**).
Full handoff — controller rulings, non-goals, divergences — is
**`docs/PHASE4-PLAN.md`'s "Track F handoff"**; read that before touching
modulation, automation lanes, or project v4.

What the user can do now: several curves per track (gain, pan, plugin
params), an automation-track kind routed to many targets, and clip
envelopes that loop with MIDI content. What is *not* done yet is the
finished graph — **path is design §8** (linked at the top of this file).

Baseline after this track: **731 backend (702 lib + 29 integration) + 299
frontend**, measured on `modulation-system` 2026-08-16.

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
  smaller **inline** items — metronome/click+count-in is **[done]**
  (PR #38), piano-roll quantize is **[done]** (PR #32) — and the
  biggest remaining Tier-1 architecture item — **insert FX chains +
  sends/busses, Plan G**, product decision in
  `docs/backlog/insert-fx-sends-sidechain.md`: host already-scanned
  CLAP/LV2 effects, do not write a stock FX suite; G1 inserts+PDC, G2
  bus+sends, G3 sidechain taps, G4 envelope-follower later. Still needs
  its own research → plan → gates round because it touches the RT graph
  invariants round-2 §8 reserves for the node-graph round, and is bound
  by the standing "PDC before sends ship" rule), and Tier 2 (months — time-stretch, pattern
  instancing, takes/comping, stems export, freeze/bounce, external
  instrument tracks, two-instance coexistence). **Start here** for
  anything not already a landed track above. Plan F is landed; do not
  start another history-storage track.
- `docs/superpowers/plans/2026-08-14-plan-e-side-channel-totality.md` —
  Plan E's own plan doc (scope rulings, non-goals, all 18 tasks).
- `docs/SIDE-CHANNEL-INVENTORY.md` — the landed inventory, carve-outs,
  and replay limitations. L-4/L-5/R-3 are CLOSED (Plan F); L-2 is
  documented benign (ruling F-9).
- `docs/PHASE4-PLAN.md` — orchestration; "Plan F handoff" is the current
  history-storage state (F-1..F-12, new carry-forwards). "Plan E handoff"
  still has every earlier scope ruling, mid-flight ruling, and
  deferred-minor from that ledger, verbatim.
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

*Working note. When a leftover lands, update the relevant section of
this file (marked correction, ADR 0007) so the next fresh session isn't
reading stale status. After PR #23 merges, this file is the post-Plan-F
briefing on `main`.*
