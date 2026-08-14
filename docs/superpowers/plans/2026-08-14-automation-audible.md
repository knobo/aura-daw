# Track D — Automation Audible + Lane UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan task-by-task. Steps
> use checkbox (`- [ ]`) syntax for tracking. Execution mode for this run is
> the owner's call and is recorded in the execution note at the bottom.

**Goal:** Make automation audible (the engine finally reads
`session.automation` at rebuild and applies compiled ramps to track gain and
to plugin parameters), make it editable (a timeline lane where you draw and
drag points, every edit a gesture-wrapped `automation_set`), and close the
three Plan E review findings that live in this neighbourhood — I-3
(`execute_host_forward`'s unguarded document writeback), I-8 (per-knob
`project.json` rewrite: gestures extended to plugin params), and the
frontend M-3 (undo/redo re-pull misses automation and plugin panels).

**Architecture:** Nothing new is invented. The data layer landed in Plan E
Task 10 (`AutomationLane` persists, `Op::AutomationSetLane` is atomic,
attributed and undoable); the compilation contract landed in
`plugins/automation.rs` (`compile_lane` → `Vec<AbsParamEvent>`, `value_at`'s
linear ramp). This plan adds three seams and wires them:

1. **Track-gain lanes attach at the mixer's per-track gain stage**, not by
   wrapping nodes — a slot-indexed `RtGraph::gain_ramps` table, versioned
   with the graph exactly like `ParamTable` (see scope ruling 1). One
   `RampCursor` per track per block does O(1) work per frame; no allocation,
   no locks.
2. **Plugin-parameter lanes are driven on the engine's own control thread**
   at its ≤2 ms tick, through the same host entry points
   `execute_host_forward` uses — a plugin param write is a host round-trip
   and is banned on the audio callback (see scope ruling 2).
3. **Gestures grow a persist-deferral**, which is what actually fixes I-8:
   a knob drag or a lane drag folds into one history entry AND one
   `project.json` write, instead of one of each per rAF batch.

All engine-touching work is concentrated in ONE final task, per the
cross-track sequencing ruling in the execution notes.

**Tech Stack:** Rust (`src-tauri/`: serde, serde_json, parking_lot, rtrb),
TypeScript/Svelte 5 (`src/lib/`), vitest.

**Spec:** `docs/backlog/automation-audible-and-ui.md` (the captured backlog
spec this plan implements) + `next-prompt.md` §3 "Track D" (scope) and §2
(standing constraints) + `docs/CORE-REDESIGN-ROUND-2.md` §4.4 (the CLAP-style
gesture primitive I-8's fix builds on) + the Plan E whole-branch final review
`.superpowers/sdd/2026-08-14-plan-e-side-channel-totality/final-review-report.md`
entries I-3, I-8, M-3 (frontend), M-6. Orchestration:
`docs/PHASE4-PLAN.md`'s "Plan E handoff" section.

---

## Global Constraints

Every task's requirements implicitly include this section.

- **The op log is ON.** `journal.ndjson` is a persisted format;
  `OP_FORMAT_VERSION` is **2** (`src-tauri/src/control/op.rs:32`). Additive
  `#[serde(default)]` fields on an op or on `TxMeta` stay non-breaking;
  anything else (renaming a field, changing a variant's shape, removing a
  path) needs a version bump AND a dual-shape reader. **This plan changes no
  serialized op shape and does not bump `OP_FORMAT_VERSION`** — see scope
  ruling 5 for how the gesture primitive is extended to lanes without
  touching `op::ObjectRef`.
- **Thin renderer** (ADR 0006): every frontend change here is op emission,
  gesture emission, or chrome. No new authoritative state and no new time
  math client-side — tick↔sample goes through the backend-shipped section
  table (`src/lib/sectionTable.ts` via `midi.sampleAtTick`/`midi.tickAtSample`),
  never through a locally derived bijection.
- **Frozen command/event names stay frozen; new commands are additive.**
  This plan adds **no new Tauri commands at all**: `automation_get`,
  `automation_set`, `gesture_begin`, `gesture_end`, `plugin_set_param`,
  `undo`, `redo` all already exist and are registered
  (`src-tauri/src/lib.rs:208-209`). Only their bodies and their
  `ControlPlane` backing methods change.
- **`transact` closures must not panic** — no panic rollback until Plan F.
  Validate before mutating, every time.
- **Prepare-outside/commit-inside** (round-2 §4.4): unbounded work happens
  before the transaction; the transaction is a short apply; persistence
  happens after the lock as a `PersistEffect`.
- **The M-3 redo invariant**: a transient write must never touch a document
  field an entry's `ops` can address. Enforced in debug builds by
  `debug_assert_transient_invariant` (`src-tauri/src/control/mod.rs:863`):
  transient ops may address only `ObjectRef::Transport`, unless the batch is
  a mid-gesture fold (`IN_GESTURE_FOLD`). **This is why I-3 gets a carve-out
  and not an op** — see scope ruling 3.
- **Gesture lock order is gesture-before-session, everywhere**
  (`GestureState`'s LOCK ORDER doc, `src-tauri/src/control/mod.rs:913-921`).
  `commit_transient_and_fold` is the one method that nests them, in that
  direction only. Nothing added here takes the gesture mutex while the
  session lock is held.
- **RT discipline:** no allocation, no locks, no syscalls on the audio
  callback path. Compiled ramps reach the RT thread as an `Arc<Vec<…>>`
  built on the control thread and read through the existing block/meter
  machinery. Ticks never cross onto the RT thread (ARCHITECTURE §13/§15.1).
- **Foreground `timeout`-guarded test runs only:**
  ```
  timeout 900 cargo test --manifest-path src-tauri/Cargo.toml
  timeout 300 npx vitest run
  ```
  **Baseline at branch base (`3340aa8`, verified 2026-08-14 in this
  worktree): 527 backend + 206 frontend, all green.** The 527 splits as
  499 lib + 3 channel_properties + 2 figma_invariant + 2 identity_properties
  + 13 journal_and_history + 2 pure_readers + 2 real_models + 4 v3_migration.
- **KNOWN FLAKINESS at branch base — read before chasing a phantom
  regression.** Two lib tests fail intermittently under the default parallel
  test-thread count and pass in isolation and under `--test-threads=1`:
  `control::hum::tests::apply_hum_clip_commits_synchronously_and_announces_project_changed`
  and `plugins::host::tests::plugin_main_thread_slots_and_tickers`. Both were
  observed failing on one full run and passing on the next, on an unmodified
  tree. If either fails, re-run it in isolation before treating it as yours:
  ```
  timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- <name> --test-threads=1
  ```
  Do not "fix" them in this plan; record the observation in the ledger.
- **Dated test-count convention:** any task that changes test counts updates
  `README.md:389` and `CONTRIBUTING.md:62` in the same commit, with the date.
  **Note:** both files currently say **506**, which is stale — PR #17
  (hardware MIDI slice 1) took the real count to 527 without updating them.
  Task 10 corrects the number to the true post-plan value; do not "restore"
  506.
- **Audio content/placement stays addressing-only** (Plan C/D scope ruling).
  Automation does not reopen it.

---

## Scope rulings (decided now, marked per ADR 0007, so no task stalls)

1. **Track-gain automation attaches at the MIXER's per-track gain stage, not
   by wrapping live nodes in `GainAutomatedNode`.** The backlog and
   `next-prompt.md` both name `GainAutomatedNode` as the attach vehicle.
   That wrapper is a proven, well-tested per-node ramp applier and it stays
   in the tree exactly as it is — but it is the wrong seam for the
   production graph, for three reasons that are each on their own decisive:
   - **Node reuse.** Live nodes come from `LiveNodeRegistry::resolve_with`
     (`src-tauri/src/midi/playback.rs:199-242`), keyed so voice and plugin
     state SURVIVES rebuilds. Wrapping means either a new registry key per
     lane-content change — i.e. a fresh node, and for a plugin track a full
     re-instantiation, on every point drag — or mutating a cached node's
     baked-in `events` from the control thread while a published snapshot may
     still reference it, which directly violates the `LiveNodeCell` safety
     contract (`src-tauri/src/audio/rt.rs:232-242`).
   - **Audio tracks.** `GainAutomatedNode` wraps a `LiveInstrument`, so only
     MIDI tracks could ever be automated. "Draw a volume curve on an audio
     track" — the primary use — would be impossible.
   - **Cost.** Every lane edit would force a node rebuild and an audible
     discontinuity.

   The mixer seam has none of these: a slot-indexed `Vec<Option<Arc<Vec<
   AbsParamEvent>>>>` on `RtGraph`, versioned with the snapshot exactly as
   `ParamTable` is (round-2 §2.4), swapped by the ordinary RCU graph publish.
   It applies to clips and live nodes alike, and it is what makes the
   REBUILD PIN flip cheap enough to be correct.

2. **Plugin-parameter lanes are driven at the ENGINE CONTROL THREAD's tick
   (≤2 ms), not sample-accurately, and their writes go to the HOST only —
   never to the document.** Sample-accurate plugin-param automation needs a
   wait-free param ring on the plugin node, which is round-2 §8 node-graph
   territory and is explicitly reserved (`next-prompt.md` §4: the RT graph
   invariants "round-2 §8 reserves for the node-graph round"). A host param
   write is a blocking round-trip and is banned on the callback path
   ([C1]). The control loop already ticks every ≤2 ms
   (`src-tauri/src/audio/engine.rs:574`), which is finer than a typical
   audio block — good enough for knob automation, and honest about what it
   is.

   The writes bypass the document deliberately: automation OVERRIDES the
   stored knob value during playback; the document keeps what the user set,
   which is what gets saved and what the panel shows. Routing them through
   the channel instead would (a) trip the M-3 debug assert if marked
   transient (`ObjectRef::Plugin` is addressable by `Op::PluginAdd`), or
   (b) push an undo entry and a `project.json` write every 2 ms if not.
   **Recorded consequence:** after playing an automated section the plugin's
   LIVE value can differ from the document's stored value until the next
   project load or user edit. Task 10 records this in
   `docs/SIDE-CHANNEL-INVENTORY.md`. Making the open param panel follow
   automation live is deferred, not forgotten.

3. **I-3 lands as an epoch guard plus a recorded carve-out (R-4), NOT as an
   op.** The final review offered both ("At minimum, add the epoch check and
   record it as R-4… Better: fold the status/param mirror back through a
   small transient commit"). The "better" option is closed by a constraint
   that landed after the review was written: `status` is a field
   `Op::PluginAdd` carries and addresses, so a **transient** batch writing it
   trips `debug_assert_transient_invariant`
   (`src-tauri/src/control/mod.rs:863-882`) — which is precisely the M-3
   invariant this plan is told to respect — and a **non-transient** batch
   would push a phantom undo entry onto the user's stack for every plugin
   instantiate and every undo-of-a-remove. So: epoch guard (the same shape
   `execute_persist` uses), recorded as residual R-4 in
   `docs/SIDE-CHANNEL-INVENTORY.md`, and `execute_host_forward` added to that
   doc's grep-gate enumeration (that omission IS M-6).

4. **I-8's real fix is gesture-scoped PERSIST DEFERRAL, not gesture folding
   alone.** Folding a plugin-param commit into a gesture makes it transient —
   but a transient commit still executes its full `EngineEffect`, persist
   included (`commit_with_rebuild`, `src-tauri/src/control/mod.rs:530-534`).
   So coalescing by itself leaves the row-13 frequency claim exactly as
   wrong as the review found it. The fix is therefore two-part: the commit
   folds (Task 3), AND its `PersistEffect` is deferred and accumulated on the
   open gesture, executed exactly once at `close_gesture` (Task 2). Row 13's
   wording is corrected in the same breath.

5. **Automation lanes get a coalesce target WITHOUT a new `op::ObjectRef`
   variant.** `CoalesceKey` (`src-tauri/src/control/mod.rs:776`) is
   `pub(crate)`, lives only in memory, and is never serialized — so its
   `object: op::ObjectRef` field becomes an internal `CoalesceTarget` enum
   with an `AutomationLane(String)` arm. `op::ObjectRef` — which IS
   serialized into `journal.ndjson` — is untouched, so no
   `OP_FORMAT_VERSION` question arises and `Op::AutomationSetLane`'s doc
   ("no new `ObjectRef` variant is needed", `op.rs:151-152`) stays true.

6. **Track-gain lane values are LINEAR GAIN MULTIPLIERS in `[0.0, 1.0]`,
   applied on top of the fader.** 0.0 is silence, 1.0 is "whatever the fader
   says". This is exactly `GainAutomatedNode`'s landed semantics (it scales
   the node's output; the fader applies separately), so the existing
   end-to-end tests keep describing the shipped behaviour. The alternative —
   automation REPLACING the fader, with the fader following the curve — needs
   a fader-follows-automation UI mode and a read/write arbitration story; it
   is out of scope and recorded here as deferred. Built-in track params get
   ids in `automation.rs`: `TRACK_PARAM_GAIN = 0` (which is what the module's
   existing tests already use for `"track:t1"`).

7. **Lane drags preview locally and commit ONCE on pointerup**, mirroring
   `project.moveClip`/`commitClipMove` and PR #14's commit-on-release
   convention. The gesture bracket is still opened on pointerdown and closed
   on pointerup, because a single pointer interaction can produce more than
   one commit (drag a point, then delete a neighbour with alt-click before
   releasing) and because the gesture is what defers the persist (ruling 4).
   Making the ramp audible DURING the drag would need a graph rebuild per
   pointermove; that is not a thing this plan does.

8. **The automation lane renders as an in-lane OVERLAY on the track's
   existing timeline row, not as an added row.** `Timeline.svelte` keeps the
   left rail (`TrackHeader` per track) and the lane column
   (`.lane` per track) in lockstep by a shared `--track-height`; inserting a
   sub-row would desynchronise them and require a height-negotiation pass
   through both columns. An overlay costs one absolutely-positioned canvas
   inside the existing `.lane` div and zero layout changes. A dedicated,
   resizable automation row is a follow-up, recorded not silent.

---

## Non-goals (so the diff stays reviewable)

- No sample-accurate plugin-param automation (ruling 2; round-2 §8's round).
- No automation for pan / mute / solo / send levels — track gain and plugin
  params only. New built-in target ids are additive later.
- No curve shapes beyond linear (the compile contract in
  `plugins/automation.rs` is linear-between-breakpoints; bezier/hold/step
  segments would change the persisted point record).
- No automation recording (write/touch/latch modes).
- No snapshot-based rebuild — `engine::rebuild` still reads the session
  under its lock. That is Track A's, and this plan's engine task is written
  to rebase cleanly over it.
- No `GainAutomatedNode` deletion. It stays, tested, as the per-node ramp
  applier; ruling 1 only says it is not the production attach seam.
- No fix for the two flaky tests named in the Global Constraints.

---

### Task 1: I-3 + M-6 — epoch-guard `execute_host_forward`'s document writeback

The `HostForward::Instantiate` arm re-locks the session after the host call
and writes `r.status = "active"` plus the param mirror
(`src-tauri/src/control/mod.rs:676-700`). Unlike `execute_persist` it has no
epoch guard, so if a project open/create/save-as swapped the document while
the host round-trip was in flight, this writes into the NEW document —
worst case `s.plugins.params.entry(instance).or_default()` *inserts* a param
row for an instance the new project has never heard of.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` — `Committer::execute_host_forward`
  (signature + the `Instantiate` arm's writeback), and its one call site at
  `:424`
- Modify: `docs/SIDE-CHANNEL-INVENTORY.md` — new residual **R-4**, and
  `execute_host_forward` added to "The grep gate" enumeration (M-6)
- Test: `src-tauri/src/control/mod.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `session::HostForward` (unchanged), `Session::epoch`
  (`src-tauri/src/control/session.rs:322`, `pub u64`),
  `Committer::execute_persist(&self, p: &PersistEffect, committed_epoch: u64)`
  as the pattern to mirror.
- Produces, for later tasks and for reviewers:
  ```rust
  pub(crate) fn execute_host_forward(
      &self,
      forwards: &[session::HostForward],
      committed_epoch: u64,
  );

  /// The `Instantiate` arm's post-host document writeback, split out of the
  /// match so it is testable without a live plugin host (I-3). Marks the row
  /// "active" and fills an EMPTY param mirror. Skipped whole when the epoch
  /// moved between the commit and this re-lock.
  pub(crate) fn apply_instantiate_writeback(
      &self,
      instance: &str,
      params: Vec<crate::plugins::ParamInfo>,
      committed_epoch: u64,
  );
  ```

- [ ] **Step 1: Write the failing tests.** Add to `control/mod.rs`'s test
  module, next to `set_track_mix_emits_project_changed_with_updated_tracks`
  (which shows the `recording_control_plane()` fixture at `:3273`):

```rust
/// I-3 (Plan E whole-branch review): `execute_host_forward`'s Instantiate
/// writeback used to re-lock the session and write `status`/`params` with
/// no epoch guard, so a project swap in flight got another project's
/// plugin state written into it. Same guard shape `execute_persist` uses.
#[test]
fn instantiate_writeback_lands_when_the_epoch_is_unchanged() {
    let (cp, _events, _engine) = recording_control_plane();
    let row = crate::plugins::PluginInstanceInfo {
        id: "inst-1".into(),
        uid: "lv2:urn:test:synth".into(),
        name: "TestSynth".into(),
        format: "lv2".into(),
        status: "stub".into(),
    };
    let epoch = {
        let mut s = cp.session().lock();
        s.plugins.instances.push(row);
        s.epoch
    };
    let params = vec![crate::plugins::ParamInfo {
        id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
        default: 0.5, value: 0.25, steps: 0,
    }];
    cp.committer().apply_instantiate_writeback("inst-1", params, epoch);

    let s = cp.session().lock();
    assert_eq!(s.plugins.instances[0].status, "active");
    assert_eq!(s.plugins.params["inst-1"].len(), 1);
    assert_eq!(s.plugins.params["inst-1"][0].value, 0.25);
}

#[test]
fn instantiate_writeback_is_skipped_when_the_epoch_moved_under_it() {
    let (cp, _events, _engine) = recording_control_plane();
    let row = crate::plugins::PluginInstanceInfo {
        id: "inst-1".into(),
        uid: "lv2:urn:test:synth".into(),
        name: "TestSynth".into(),
        format: "lv2".into(),
        status: "stub".into(),
    };
    let stale_epoch = {
        let mut s = cp.session().lock();
        s.plugins.instances.push(row);
        let e = s.epoch;
        s.epoch += 1; // an epoch function swapped the document meanwhile
        e
    };
    let params = vec![crate::plugins::ParamInfo {
        id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
        default: 0.5, value: 0.25, steps: 0,
    }];
    cp.committer().apply_instantiate_writeback("inst-1", params, stale_epoch);

    let s = cp.session().lock();
    assert_eq!(s.plugins.instances[0].status, "stub", "status must not be written");
    assert!(
        !s.plugins.params.contains_key("inst-1"),
        "the params mirror must not be CREATED for a document this commit no longer describes"
    );
}
```

  If `PluginInstanceInfo`/`ParamInfo` have more fields than shown, copy the
  literal shape used by `plugins::mod`'s own test module (`scanned_registry`,
  `src-tauri/src/plugins/mod.rs:376`) rather than guessing.

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- instantiate_writeback`
  Expected: FAIL to compile (`apply_instantiate_writeback` does not exist).

- [ ] **Step 3: Implement.** In `control/mod.rs`:

```rust
    /// I-3: the `Instantiate` arm's post-host document writeback, split out
    /// of the match so it is testable without a live plugin host, and
    /// EPOCH-GUARDED (same rule as `execute_persist`: a document swapped
    /// between the commit and this re-lock is a different document, and
    /// writing this commit's host result into it is silent corruption —
    /// `params.entry(..).or_default()` would even CREATE a row for an
    /// instance the new project never had). Recorded as residual R-4 in
    /// `docs/SIDE-CHANNEL-INVENTORY.md`; see that doc for why this stays a
    /// carve-out rather than becoming an op (the M-3 transient invariant).
    pub(crate) fn apply_instantiate_writeback(
        &self,
        instance: &str,
        params: Vec<crate::plugins::ParamInfo>,
        committed_epoch: u64,
    ) {
        let mut s = self.session.lock();
        if s.epoch != committed_epoch {
            log::warn!(
                "plugins: instantiate writeback for {instance} skipped: epoch changed \
                 between commit and host round-trip ({committed_epoch} -> {})",
                s.epoch
            );
            return;
        }
        if let Some(r) = s.plugins.instances.iter_mut().find(|r| r.id == instance) {
            r.status = "active".into();
        }
        // Fill only when absent (Task 9 review round 1, Important-1): an
        // undo-of-remove already restored the REAL param mirror.
        let entry = s.plugins.params.entry(instance.to_string()).or_default();
        if entry.is_empty() {
            *entry = params;
        }
    }
```

  Change `fn execute_host_forward(&self, forwards: &[session::HostForward])`
  to `pub(crate) fn execute_host_forward(&self, forwards: &[session::HostForward], committed_epoch: u64)`,
  replace the `Instantiate` arm's inline `match hosted { Ok(params) => { … } }`
  body with `Ok(params) => self.apply_instantiate_writeback(instance, params, committed_epoch),`
  and update the single call site (`:424`) to
  `self.execute_host_forward(&committed.effect.host_forward, committed.epoch);`.

- [ ] **Step 4: Run the tests** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 527 + 2 = **529** backend, green (modulo the two known flaky
  tests — re-run in isolation before blaming this change).

- [ ] **Step 5: Record R-4 and close M-6 in the inventory.** In
  `docs/SIDE-CHANNEL-INVENTORY.md`:
  - Under "Residual documented non-op document writes", change the opening
    sentence from "Three sites" to "Four sites" and append:

```markdown
**R-4 — `Committer::execute_host_forward`'s `Instantiate` writeback**
(`src-tauri/src/control/mod.rs`, `apply_instantiate_writeback`). After the
host round-trip returns, the arm writes `status = "active"` and fills an
empty param mirror for the instance. `status` IS document state (it
round-trips through `PluginInstanceInfo`, is carried by `Op::PluginAdd`, and
is persisted), so this is a genuine non-op document write — found by the
Plan E whole-branch review as I-3. It stays a carve-out rather than becoming
an op because both op-shaped routes are closed: a TRANSIENT batch addressing
`ObjectRef::Plugin` trips `debug_assert_transient_invariant` (the M-3 redo
invariant), and a NON-transient one would push a phantom undo entry for every
plugin instantiate and every undo-of-a-remove. What the write now has is an
EPOCH GUARD, the same one `execute_persist` uses: a document swapped between
the commit and the host's return is a different document, and this commit's
host result is not about it.
```

  - Under "The grep gate", change
    "`Committer::commit_with_rebuild`/`execute_persist`" to
    "`Committer::commit_with_rebuild`/`execute_persist`/`execute_host_forward`"
    and extend the trailing clause to "…or the three recorded residuals R-1,
    R-3 and R-4." (M-6.)

- [ ] **Step 6: Commit** —

```bash
git add src-tauri/src/control/mod.rs docs/SIDE-CHANNEL-INVENTORY.md
git commit -m "fix(plugins): epoch-guard execute_host_forward's document writeback (I-3, R-4, M-6)"
```

---

### Task 2: Gesture-scoped persist deferral (I-8, half 1)

Today every mid-gesture transient commit still executes its full
`PersistEffect` — so a plugin knob drag rewrites `project.json` once per rAF
batch even though it produces exactly one undo entry. This task makes an
open gesture ACCUMULATE its folded commits' persist effects and execute them
once, at `close_gesture`.

**Files:**
- Modify: `src-tauri/src/control/session.rs` — `impl PersistEffect` gains
  `merge`
- Modify: `src-tauri/src/control/mod.rs` — `Committer::commit_with_rebuild_full`,
  `ControlPlane::commit_transient_for_gesture`, `OpenGesture` (+2 fields),
  `GestureState::fold_committed`, `ControlPlane::close_gesture`,
  `ControlPlane::set_track_mix` (switch to the new helper)
- Test: `src-tauri/src/control/session.rs` tests + `src-tauri/src/control/mod.rs` tests

**Interfaces:**
- Consumes: `session::PersistEffect` (`session.rs:305`, fields `midi`,
  `project`, `plugins`, `automation`, all `bool`);
  `Committer::commit_with_rebuild_mode` (`mod.rs:348`);
  `Committer::execute_persist(&self, &PersistEffect, u64)` (`mod.rs:513`);
  `GestureState::commit_transient_and_fold` (`mod.rs:975`).
- Produces:
  ```rust
  // session.rs
  impl PersistEffect {
      /// OR every field of `other` into `self` — how an open gesture
      /// accumulates the persist effects of the commits it folded.
      pub fn merge(&mut self, other: &PersistEffect);
  }

  // mod.rs — Committer
  pub(crate) fn commit_with_rebuild_full<F, R>(
      &self,
      meta: op::TxMeta,
      f: F,
      emit_project_changed: bool,
      do_rebuild: R,
      history_mode: history::HistoryMode,
      defer_persist: bool,
  ) -> Result<session::Committed, String>
  where F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>, R: FnOnce();

  // mod.rs — ControlPlane
  /// The commit a gesture-folding caller makes: transient, no
  /// `project://changed` (the gesture emits one at close), and its persist
  /// DEFERRED onto the open gesture. Callers: `set_track_mix`,
  /// `set_plugin_params` (Task 3), `set_automation_lane` (Task 4).
  fn commit_transient_for_gesture<F>(&self, meta: op::TxMeta, f: F)
      -> Result<session::Committed, String>
  where F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>;
  ```
  `OpenGesture` gains `persist: session::PersistEffect` and `epoch: u64`.
  `commit_with_rebuild_mode` becomes a thin delegate to
  `commit_with_rebuild_full(.., defer_persist: false)` — every existing
  caller keeps its exact behaviour.

- [ ] **Step 1: Write the failing tests.**

  In `session.rs`'s test module:

```rust
#[test]
fn persist_effect_merge_ors_every_field() {
    let mut a = PersistEffect { midi: true, project: false, plugins: false, automation: false };
    let b = PersistEffect { midi: false, project: true, plugins: true, automation: false };
    a.merge(&b);
    assert_eq!(a, PersistEffect { midi: true, project: true, plugins: true, automation: false });
    // merging default changes nothing
    let before = a.clone();
    a.merge(&PersistEffect::default());
    assert_eq!(a, before);
}
```

  (If `PersistEffect` is not `Clone`, derive it alongside the existing
  `PartialEq`/`Default` — it is a four-bool POD.)

  In `mod.rs`'s test module, next to the existing gesture tests around
  `:3412`:

```rust
/// I-8 (Plan E whole-branch review): folding a knob drag into a gesture is
/// only half the fix — a TRANSIENT commit still executes its full
/// `EngineEffect`, persist included, so `project.json` was still rewritten
/// once per rAF batch. Deferring the persist onto the open gesture is what
/// makes "one drag = one write" true.
#[test]
fn a_gesture_defers_its_folded_commits_persist_and_executes_it_once_at_close() {
    let (cp, _events, _engine) = recording_control_plane();
    let dir = std::env::temp_dir().join(format!(
        "aura-gesture-persist-{}-{}", std::process::id(), uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let (_p, dir) = crate::audio::project::create(&dir, "Song", 48_000, 120.0).unwrap();
    {
        let mut s = cp.session().lock();
        s.store.project_dir = Some(dir.clone());
        s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
        });
        s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.0, value: 0.0, steps: 0,
        }]);
    }
    let stored_value = |dir: &std::path::Path| -> Option<f64> {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("project.json")).ok()?).ok()?;
        v.get("plugins")?.as_array()?.iter()
            .find(|r| r["id"] == "inst-1")?
            .get("params")?.as_array()?.iter()
            .find(|p| p["id"] == 7)?
            .get("value")?.as_f64()
    };

    cp.gesture_begin("plugin param drag".into()).unwrap();
    for v in [0.25f64, 0.5, 0.75] {
        cp.commit_transient_for_gesture(op::TxMeta::user("plugin set param"), |tx| {
            tx.apply(op::Op::Set {
                object: op::ObjectRef::Plugin("inst-1".into()),
                path: op::PropPath::Param { index: 7 },
                from: serde_json::Value::Null,
                to: serde_json::json!(v),
            })
        })
        .unwrap();
    }
    assert!(
        stored_value(&dir).is_none() || stored_value(&dir) == Some(0.0),
        "no project.json write may land while the gesture is open"
    );

    cp.gesture_end().unwrap();
    assert_eq!(stored_value(&dir), Some(0.75), "one write, at close, with the LAST value");
    let (undo_depth, _redo) = cp.history_depths();
    assert_eq!(undo_depth, 1, "three folded commits, one undo entry");
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}
```

  If `plugins::state::save_snapshot_into_project`'s on-disk shape differs
  from the `plugins[].params[].value` path assumed by `stored_value`, read
  the shape it actually writes (`src-tauri/src/plugins/state.rs`) and adjust
  the accessor — the ASSERTIONS (nothing during, exactly the last value
  after, one undo entry) are what this test is for and must not change.

- [ ] **Step 2: Run to verify both fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- persist_effect_merge a_gesture_defers`
  Expected: FAIL to compile (`merge`, `commit_transient_for_gesture` absent).

- [ ] **Step 3: Implement `PersistEffect::merge`** in `session.rs`, directly
  under the struct:

```rust
impl PersistEffect {
    /// OR every field of `other` into `self`. An open gesture accumulates
    /// the persist effects of the transient commits it folds and executes
    /// the union ONCE at `close_gesture` (I-8): a knob or lane drag is one
    /// `project.json` write, not one per rAF batch.
    pub fn merge(&mut self, other: &PersistEffect) {
        self.midi |= other.midi;
        self.project |= other.project;
        self.plugins |= other.plugins;
        self.automation |= other.automation;
    }
}
```

- [ ] **Step 4: Implement the deferral in `mod.rs`.**

  (a) Rename the existing `commit_with_rebuild_mode` body to
  `commit_with_rebuild_full` with the extra `defer_persist: bool` parameter,
  and make the persist block conditional:

```rust
        if !defer_persist && committed.effect.persist != session::PersistEffect::default() {
            self.execute_persist(&committed.effect.persist, committed.epoch);
        }
```

  then reinstate the old name as a delegate:

```rust
    pub(crate) fn commit_with_rebuild_mode<F, R>(
        &self,
        meta: op::TxMeta,
        f: F,
        emit_project_changed: bool,
        do_rebuild: R,
        history_mode: history::HistoryMode,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
        R: FnOnce(),
    {
        self.commit_with_rebuild_full(meta, f, emit_project_changed, do_rebuild, history_mode, false)
    }
```

  (b) `OpenGesture` gains two fields; `GestureState::begin` seeds them:

```rust
struct OpenGesture {
    actor: op::Actor,
    run: String,
    baselines: Vec<(CoalesceKey, op::Op)>,
    last: Vec<(CoalesceKey, op::Op)>,
    label: String,
    /// Union of the persist effects of every commit folded into this
    /// gesture, executed ONCE at `close_gesture` (I-8). Deferring is what
    /// turns "one project.json write per rAF batch" into "one per drag".
    persist: session::PersistEffect,
    /// `Committed.epoch` of the LAST folded commit — what `execute_persist`
    /// checks against the current session epoch at close. An epoch boundary
    /// mid-gesture swaps the document out from under the accumulated
    /// snapshot, and the epoch's own save owns durability from there.
    epoch: u64,
}
```
  (in `begin`: `persist: session::PersistEffect::default(), epoch: 0,`).

  (c) `GestureState::fold_committed` accumulates them — add, before the
  per-op loop:

```rust
        g.persist.merge(&committed.effect.persist);
        g.epoch = committed.epoch;
```

  (d) `ControlPlane::commit_transient_for_gesture`, next to `commit_with`:

```rust
    /// The commit shape every gesture-folding caller uses (`set_track_mix`,
    /// `set_plugin_params`, `set_automation_lane`): TRANSIENT (no history
    /// entry, no journal line — the synthesized gesture batch is the
    /// history-bound one), `emit_project_changed: false` (the gesture emits
    /// exactly one at close), and `defer_persist: true` (I-8 — the persist
    /// rides `close_gesture`, once).
    ///
    /// Callers must only reach this from INSIDE
    /// `GestureState::commit_transient_and_fold`, which is what guarantees
    /// the deferred persist is actually accumulated by an open gesture
    /// rather than silently dropped.
    fn commit_transient_for_gesture<F>(
        &self,
        meta: op::TxMeta,
        f: F,
    ) -> Result<session::Committed, String>
    where
        F: FnOnce(&mut session::Tx<'_>) -> Result<(), String>,
    {
        self.committer.commit_with_rebuild_full(
            meta.transient(),
            f,
            false,
            || self.engine.send(ControlMsg::Rebuild),
            history::HistoryMode::Record,
            true,
        )
    }
```

  (e) `close_gesture` executes it — read the two fields BEFORE the
  destructuring moves, and run the persist after `record_gesture` and before
  the `project://changed` emit (same ordering rule
  `commit_with_rebuild_full` uses: the event announces durable truth):

```rust
        let gesture_persist = gesture.persist;
        let gesture_epoch = gesture.epoch;
        // … existing ops/inverses/meta/rev/epoch/committed construction …
        self.committer.log().record_gesture(/* unchanged */);
        *self.last_gesture_batch.lock() = Some(committed);
        // I-8: the whole drag's persist, once, here — never once per folded
        // commit. Before the emit, for the same reason `commit_with_rebuild_full`
        // persists before its own emit.
        if gesture_persist != session::PersistEffect::default() {
            self.committer.execute_persist(&gesture_persist, gesture_epoch);
        }
        // … existing payload + emit …
```

  (f) `set_track_mix`'s gesture branch switches to the new helper (behaviour
  identical — `Op::Set{Track, …}` sets no persist flags — but it makes the
  mechanism uniform and is what Tasks 3/4 copy):

```rust
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| apply_mix_changes(&changes, tx))
        });
```

  **Note the test-visibility requirement:** the Step-1 test calls
  `cp.commit_transient_for_gesture(...)` directly, so mark it
  `#[cfg_attr(test, allow(dead_code))]`-free by giving it `pub(crate)`
  visibility if the test module cannot otherwise reach it (it is in the same
  module, so private `fn` is fine — keep it private).

- [ ] **Step 5: Run the whole backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 529 + 2 = **531**, green. Existing gesture tests
  (`:3412`, `:3459`) must still pass unchanged.

- [ ] **Step 6: Commit** —

```bash
git add src-tauri/src/control/session.rs src-tauri/src/control/mod.rs
git commit -m "feat(channel): gesture-scoped persist deferral — one project.json write per drag (I-8)"
```

---

### Task 3: I-8 half 2 — `plugin_set_param` folds into gestures; the knob drag opens one

**Files:**
- Modify: `src-tauri/src/control/mod.rs` — new `ControlPlane::set_plugin_params`
- Modify: `src-tauri/src/plugins/mod.rs` — `plugin_set_param` delegates
- Modify: `src/lib/state/plugins.svelte.ts` — `flushParamQueue` returns a
  promise; new `beginParamGesture`/`endParamGesture`
- Modify: `src/lib/components/plugins/PluginParamPanel.svelte` — pointer
  brackets on the range inputs
- Modify: `docs/SIDE-CHANNEL-INVENTORY.md` — row 13's wording correction
- Test: `src-tauri/src/control/mod.rs` tests; new
  `src/lib/state/plugins-gesture.test.ts`

**Interfaces:**
- Consumes: `ControlPlane::commit_transient_for_gesture` (Task 2),
  `GestureState::commit_transient_and_fold`, `CoalesceKey::for_op` (already
  keys `Op::Set{Plugin, Param}` — no change needed for plugin params),
  `crate::plugins::ParamChange { id: u32, value: f64 }`.
- Produces:
  ```rust
  // ControlPlane
  /// Batched plugin-param writes through the channel — the exact shape
  /// `set_track_mix` uses. Inside an open `Actor::User` gesture the commit
  /// runs transient with its persist deferred and folds into the gesture's
  /// accumulator (I-8); outside one it is an ordinary history-bound commit.
  pub fn set_plugin_params(
      &self,
      instance_id: &str,
      changes: &[crate::plugins::ParamChange],
      meta: op::TxMeta,
  ) -> Result<(), String>;
  ```
  ```ts
  // PluginsStore
  private flushParamQueue(): Promise<void>;
  beginParamGesture(): void;
  endParamGesture(): Promise<void>;
  ```

- [ ] **Step 1: Write the failing backend test** (in `control/mod.rs`'s test
  module):

```rust
/// I-8: a knob drag inside a gesture is ONE undo entry — the per-(instance,
/// param) `CoalesceKey` already exists for `Op::Set{Plugin, Param}`; what
/// was missing is that `plugin_set_param` never consulted the gesture at
/// all (`commit_transient_and_fold` was wired only into `set_track_mix`).
#[test]
fn plugin_param_writes_fold_into_an_open_gesture() {
    let (cp, _events, _engine) = recording_control_plane();
    {
        let mut s = cp.session().lock();
        s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
        });
        s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.0, value: 0.0, steps: 0,
        }]);
    }
    cp.gesture_begin("plugin param drag".into()).unwrap();
    for v in [0.25f64, 0.5, 0.75] {
        cp.set_plugin_params(
            "inst-1",
            &[crate::plugins::ParamChange { id: 7, value: v }],
            op::TxMeta::user("plugin set param"),
        )
        .unwrap();
    }
    assert_eq!(cp.history_depths().0, 0, "nothing reaches history while the gesture is open");
    cp.gesture_end().unwrap();
    assert_eq!(cp.history_depths().0, 1, "the whole drag is one undo entry");

    let batch = cp.take_last_gesture_batch().expect("gesture_end must produce a batch");
    assert_eq!(batch.ops.len(), 1, "coalesced to the LAST write per (instance, param)");
    assert!(matches!(
        &batch.ops[0],
        op::Op::Set { object: op::ObjectRef::Plugin(id), path: op::PropPath::Param { index: 7 }, to, .. }
            if id == "inst-1" && to.as_f64() == Some(0.75)
    ), "{:?}", batch.ops[0]);
    // and the baseline is the value BEFORE the drag, not the previous move
    assert!(matches!(
        &batch.inverses[0],
        op::Op::Set { to, .. } if to.as_f64() == Some(0.0)
    ), "{:?}", batch.inverses[0]);
}

/// Outside a gesture, nothing changes: one invoke, one history entry.
#[test]
fn plugin_param_writes_outside_a_gesture_stay_one_entry_each() {
    let (cp, _events, _engine) = recording_control_plane();
    {
        let mut s = cp.session().lock();
        s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
        });
        s.plugins.params.insert("inst-1".into(), vec![crate::plugins::ParamInfo {
            id: 7, name: "cutoff".into(), min: 0.0, max: 1.0,
            default: 0.0, value: 0.0, steps: 0,
        }]);
    }
    cp.set_plugin_params(
        "inst-1",
        &[crate::plugins::ParamChange { id: 7, value: 0.4 }],
        op::TxMeta::user("plugin set param"),
    )
    .unwrap();
    assert_eq!(cp.history_depths().0, 1);
}
```

  **Note on the 350 ms same-key merge:** `HistoryLog` merges consecutive
  same-`CoalesceKey` entries within 350 ms
  (`src-tauri/src/control/history.rs`). The second test writes ONE param and
  asserts depth 1, so it is unaffected; if a future edit adds a second write
  to it, the merge is why the depth would still be 1.

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- plugin_param_writes`
  Expected: FAIL to compile (`set_plugin_params` does not exist).

- [ ] **Step 3: Implement `ControlPlane::set_plugin_params`**, directly after
  `set_track_mix`:

```rust
    /// Batched plugin-param writes through the transaction channel — one
    /// `Op::Set{Plugin, Param}` per change, applied atomically. Gesture-aware
    /// in exactly the shape `set_track_mix` uses (I-8: plugin knobs are the
    /// canonical CLAP gesture, round-2 §4.4, and until now they were the one
    /// drag surface that never consulted `GestureState` — so every rAF batch
    /// was its own undo entry AND its own `project.json` rewrite).
    ///
    /// LOCK ORDER: `commit_transient_and_fold` holds the gesture mutex
    /// across the nested session-lock acquisition; that direction (gesture,
    /// then session) is the only safe one and is the one used here.
    pub fn set_plugin_params(
        &self,
        instance_id: &str,
        changes: &[crate::plugins::ParamChange],
        meta: op::TxMeta,
    ) -> Result<(), String> {
        // Validate before any commit (the `transact` closure must not panic,
        // and an unknown instance must fail the whole batch atomically).
        {
            let s = self.session.lock();
            if !s.plugins.instances.iter().any(|r| r.id == instance_id) {
                return Err(format!("unknown plugin instance: {instance_id}"));
            }
        }
        let apply = |changes: &[crate::plugins::ParamChange], tx: &mut session::Tx<'_>| {
            for c in changes {
                tx.apply(op::Op::Set {
                    object: op::ObjectRef::Plugin(instance_id.to_string()),
                    path: op::PropPath::Param { index: c.id },
                    from: serde_json::Value::Null,
                    to: serde_json::json!(c.value),
                })?;
            }
            Ok(())
        };
        let gesture_meta = meta.clone();
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, |tx| apply(changes, tx))
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, |tx| apply(changes, tx))?;
            }
        }
        Ok(())
    }
```

  Then in `src-tauri/src/plugins/mod.rs`, `plugin_set_param`'s body becomes:

```rust
    control.set_plugin_params(
        &instance_id,
        &changes,
        crate::control::op::TxMeta::user("plugin set param"),
    )?;
    control
        .plugin_params(&instance_id)
        .ok_or_else(|| format!("unknown plugin instance: {instance_id}"))
```

- [ ] **Step 4: Run the backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 531 + 2 = **533**, green.

- [ ] **Step 5: Write the failing frontend test** —
  `src/lib/state/plugins-gesture.test.ts`:

```ts
/**
 * I-8's frontend half: a knob drag brackets its rAF-batched
 * `plugin_set_param` invokes in `gesture_begin`/`gesture_end`, and the
 * TRAILING batch must land BEFORE the gesture closes — a batch that
 * reaches the backend after `gesture_end` gets its own undo entry and its
 * own project.json write, which is exactly what the gesture exists to
 * collapse.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

let resolveSet: (() => void) | null = null;
const pluginSetParam = vi.fn(
  () =>
    new Promise<void>((res) => {
      resolveSet = res;
    }),
);
const gestureBegin = vi.fn(() => Promise.resolve());
const gestureEnd = vi.fn(() => Promise.resolve());
const calls: string[] = [];

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    pluginGetParams: vi.fn(() => Promise.resolve([])),
    pluginList: vi.fn(() => Promise.resolve({ plugins: [], scanned: true, instances: [] })),
    pluginSetParam: (...a: unknown[]) => {
      calls.push("setParam");
      return pluginSetParam(...(a as []));
    },
    gestureBegin: (...a: unknown[]) => {
      calls.push("gestureBegin");
      return gestureBegin(...(a as []));
    },
    gestureEnd: (...a: unknown[]) => {
      calls.push("gestureEnd");
      return gestureEnd();
    },
  },
}));

const { plugins } = await import("./plugins.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  calls.length = 0;
  resolveSet = null;
  plugins.openInstanceId = "inst-1";
  plugins.params = [
    { id: 7, name: "cutoff", min: 0, max: 1, default: 0, value: 0, steps: 0 },
  ];
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    return setTimeout(() => cb(0), 0) as unknown as number;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) =>
    clearTimeout(id as unknown as NodeJS.Timeout),
  );
});

describe("plugin knob gesture", () => {
  it("closes the gesture only after the trailing param batch has landed", async () => {
    plugins.beginParamGesture();
    expect(gestureBegin).toHaveBeenCalledWith("plugin param drag");

    plugins.setParam(7, 0.25);
    plugins.setParam(7, 0.75);
    expect(plugins.params[0].value).toBe(0.75); // optimistic local

    const closing = plugins.endParamGesture();
    // the flush is on the wire; the gesture must NOT be closed yet
    await Promise.resolve();
    expect(calls).toEqual(["gestureBegin", "setParam"]);
    resolveSet?.();
    await closing;
    expect(calls).toEqual(["gestureBegin", "setParam", "gestureEnd"]);
    expect(pluginSetParam).toHaveBeenCalledTimes(1);
    expect(pluginSetParam.mock.calls[0][1]).toEqual([{ id: 7, value: 0.75 }]);
  });
});
```

- [ ] **Step 6: Run it to verify it fails** —
  `timeout 300 npx vitest run src/lib/state/plugins-gesture.test.ts`
  Expected: FAIL (`beginParamGesture`/`endParamGesture` are not functions).

- [ ] **Step 7: Implement the frontend half.** In
  `src/lib/state/plugins.svelte.ts`, make `flushParamQueue` return a promise
  and add the two gesture methods:

```ts
  /** Open a knob-drag gesture boundary — call on `pointerdown` of a param
   * fader, before the first `setParam` of the drag (I-8). */
  beginParamGesture() {
    project.beginGesture("plugin param drag");
  }

  /** Close a knob-drag gesture: cancel the pending rAF, flush whatever is
   * queued, WAIT for it to land, and only then close the boundary. The
   * order is load-bearing (I-8): a batch that reaches the backend after
   * `gesture_end` gets its own undo entry and its own project.json write —
   * the two things the gesture exists to collapse. */
  async endParamGesture(): Promise<void> {
    if (this.rafId != null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    await this.flushParamQueue();
    project.endGesture();
  }

  private flushParamQueue(): Promise<void> {
    if (this.pending.size === 0 || !this.openInstanceId) return Promise.resolve();
    if (this.inFlight) {
      // A batch is on the wire; the rAF after it lands picks these up.
      return new Promise<void>((resolve) => {
        if (this.rafId != null) cancelAnimationFrame(this.rafId);
        this.rafId = requestAnimationFrame(() => {
          this.rafId = null;
          void this.flushParamQueue().then(resolve);
        });
      });
    }
    const instanceId = this.openInstanceId;
    const changes: PluginParamChange[] = [...this.pending.entries()].map(([id, value]) => ({
      id,
      value,
    }));
    this.pending.clear();
    this.inFlight = true;
    return backend
      .pluginSetParam(instanceId, changes)
      .then(() => {
        this.paramError = null;
      })
      .catch((err) => {
        this.paramError = String(err);
      })
      .finally(() => {
        this.inFlight = false;
      });
  }
```

  `closeParams()` currently calls `this.flushParamQueue();` — keep it, now
  as `void this.flushParamQueue();` so the ignored promise is explicit.

  In `src/lib/components/plugins/PluginParamPanel.svelte`, add the brackets
  to the range input (around `:163-173`):

```svelte
                    oninput={(e) => onSlide(p, e)}
                    onpointerdown={() => plugins.beginParamGesture()}
                    onpointerup={() => void plugins.endParamGesture()}
                    onpointercancel={() => void plugins.endParamGesture()}
                    ondblclick={() => plugins.resetParam(p)}
```

- [ ] **Step 8: Run both suites** —
  `timeout 300 npx vitest run` → 206 + 1 = **207**, green.
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` → 533, green.

- [ ] **Step 9: Correct inventory row 13's wording** in
  `docs/SIDE-CHANNEL-INVENTORY.md` (I-8's "at minimum" item, now genuinely
  fixed). Replace row 13's "Mechanism" cell text with:

```
`Op::Set{Plugin, Param}` + host-forward effect + `PersistEffect`, folded into `gesture_begin`/`gesture_end` with the persist DEFERRED to gesture close (Track D — the review's I-8: before that, the rewrite had only moved off the lock, one full `project.json` read-modify-write still ran per rAF batch)
```

- [ ] **Step 10: Commit** —

```bash
git add src-tauri/src/control/mod.rs src-tauri/src/plugins/mod.rs \
        src/lib/state/plugins.svelte.ts src/lib/state/plugins-gesture.test.ts \
        src/lib/components/plugins/PluginParamPanel.svelte \
        docs/SIDE-CHANNEL-INVENTORY.md
git commit -m "feat(plugins): knob drags are one undo entry and one persist (I-8, inventory row 13)"
```

---

### Task 4: Automation lanes become gesture-foldable

`CoalesceKey::for_op` returns `None` for everything but `Op::Set`, so an
automation lane edit inside an open gesture is never folded. This task gives
lanes a coalesce target (per scope ruling 5, without touching the serialized
`op::ObjectRef`) and routes `automation_set` through the gesture path.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` — `CoalesceTarget`, `CoalesceKey`,
  `for_op`, new `ControlPlane::set_automation_lane`
- Modify: `src-tauri/src/plugins/automation.rs` — `automation_set` delegates
- Test: `src-tauri/src/control/mod.rs` tests

**Interfaces:**
- Consumes: `ControlPlane::commit_transient_for_gesture` (Task 2),
  `op::Op::AutomationSetLane { key: String, lane: Option<AutomationLane> }`,
  `plugins::automation::normalize_lane`.
- Produces:
  ```rust
  /// What a `CoalesceKey` addresses. Internal to this module (`CoalesceKey`
  /// is `pub(crate)` and is never serialized), which is exactly why an
  /// automation lane can get a coalesce target here without adding a
  /// variant to the JOURNALED `op::ObjectRef` enum.
  #[derive(Debug, Clone, PartialEq)]
  enum CoalesceTarget {
      Object(op::ObjectRef),
      /// `AutomationLane::id` — lanes have a string id, not a struct key.
      AutomationLane(String),
  }

  pub(crate) struct CoalesceKey {
      kind: &'static str,
      target: CoalesceTarget,
      path: Option<op::PropPath>,
  }

  // ControlPlane
  /// Upsert (or delete, when `lane.points` is empty) ONE automation lane
  /// through the channel, gesture-aware — the same shape as
  /// `set_track_mix`/`set_plugin_params`.
  pub fn set_automation_lane(
      &self,
      lane: crate::plugins::automation::AutomationLane,
      meta: op::TxMeta,
  ) -> Result<(), String>;
  ```

- [ ] **Step 1: Write the failing tests** (in `control/mod.rs`'s test module):

```rust
/// A lane drag is ONE undo entry: successive whole-lane replaces of the
/// same lane fold by lane id inside an open gesture (the §4.4
/// value-replacement wrapper is coalescable by construction — what was
/// missing is that `CoalesceKey::for_op` only ever keyed `Op::Set`).
#[test]
fn automation_lane_edits_fold_into_an_open_gesture_by_lane_id() {
    use crate::plugins::automation::{AutomationLane, AutomationPoint};
    let (cp, _events, _engine) = recording_control_plane();
    let mk = |id: &str, v: f32| AutomationLane {
        id: id.into(),
        target_node: "track:t-1".into(),
        param_id: 0,
        points: vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: v },
        ],
    };
    // seed the lane outside the gesture so the gesture's baseline is a real
    // previous lane, not "absent"
    cp.set_automation_lane(mk("lane-a", 0.9), op::TxMeta::user("edit automation")).unwrap();
    assert_eq!(cp.history_depths().0, 1);

    cp.gesture_begin("automation drag".into()).unwrap();
    for v in [0.6f32, 0.3, 0.0] {
        cp.set_automation_lane(mk("lane-a", v), op::TxMeta::user("edit automation")).unwrap();
    }
    assert_eq!(cp.history_depths().0, 1, "nothing new reaches history while open");
    cp.gesture_end().unwrap();
    assert_eq!(cp.history_depths().0, 2, "the whole drag adds exactly one entry");

    let batch = cp.take_last_gesture_batch().expect("a batch");
    assert_eq!(batch.ops.len(), 1, "coalesced by lane id: {:?}", batch.ops);
    match &batch.ops[0] {
        op::Op::AutomationSetLane { key, lane: Some(l) } => {
            assert_eq!(key, "lane-a");
            assert_eq!(l.points.last().unwrap().value, 0.0, "last write wins");
        }
        other => panic!("{other:?}"),
    }
    match &batch.inverses[0] {
        op::Op::AutomationSetLane { lane: Some(l), .. } => {
            assert_eq!(l.points.last().unwrap().value, 0.9, "baseline is pre-gesture truth");
        }
        other => panic!("{other:?}"),
    }
}

/// Two DIFFERENT lanes edited inside one gesture stay two ops — the key is
/// the lane id, not "automation".
#[test]
fn automation_lane_edits_do_not_coalesce_across_lanes() {
    use crate::plugins::automation::{AutomationLane, AutomationPoint};
    let (cp, _events, _engine) = recording_control_plane();
    let mk = |id: &str, v: f32| AutomationLane {
        id: id.into(),
        target_node: "track:t-1".into(),
        param_id: 0,
        points: vec![AutomationPoint { tick: 0, value: v }],
    };
    cp.gesture_begin("automation multi".into()).unwrap();
    cp.set_automation_lane(mk("lane-a", 0.1), op::TxMeta::user("edit automation")).unwrap();
    cp.set_automation_lane(mk("lane-b", 0.2), op::TxMeta::user("edit automation")).unwrap();
    cp.gesture_end().unwrap();
    let batch = cp.take_last_gesture_batch().expect("a batch");
    assert_eq!(batch.ops.len(), 2, "{:?}", batch.ops);
}
```

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- automation_lane_edits`
  Expected: FAIL to compile (`set_automation_lane` absent).

- [ ] **Step 3: Implement the coalesce target.** In `control/mod.rs`, replace
  `CoalesceKey`'s `object` field with `target`, add the enum, and extend
  `for_op`:

```rust
    fn for_op(op: &op::Op) -> Option<Self> {
        match op {
            op::Op::Set { object, path, .. } => Some(Self {
                kind: "set",
                target: CoalesceTarget::Object(object.clone()),
                path: Some(*path),
            }),
            // §4.4 value-replacement wrapper: a lane drag is a run of
            // whole-lane replaces of ONE lane; folding them by lane id is
            // what makes the drag one undo entry AND (with Task 2's
            // deferral) one automation persist.
            op::Op::AutomationSetLane { key, .. } => Some(Self {
                kind: "automationSetLane",
                target: CoalesceTarget::AutomationLane(key.clone()),
                path: None,
            }),
            _ => None,
        }
    }
```

  `for_history_op`'s `Op::MidiSetNotes` arm becomes
  `target: CoalesceTarget::Object(op::ObjectRef::MidiClip(clip.clone()))`.
  Nothing else reads `CoalesceKey`'s fields (`history.rs` only compares whole
  keys), so no other call site changes.

- [ ] **Step 4: Implement `ControlPlane::set_automation_lane`**, next to
  `set_plugin_params`:

```rust
    /// Upsert (or delete, when the normalized lane has no points) ONE
    /// automation lane through the transaction channel — the §4.4
    /// value-replacement wrapper, gesture-aware in the same shape as
    /// `set_track_mix`/`set_plugin_params`. A lane drag therefore folds to
    /// one `Op::AutomationSetLane` (last write per lane id wins), one undo
    /// entry, and — with the gesture's deferred persist — one automation
    /// write to disk.
    pub fn set_automation_lane(
        &self,
        mut lane: crate::plugins::automation::AutomationLane,
        meta: op::TxMeta,
    ) -> Result<(), String> {
        if lane.id.is_empty() {
            lane.id = uuid::Uuid::new_v4().to_string();
        }
        // Validate/normalize BEFORE the transaction (the closure must not
        // panic, and a rejected lane must leave no document trace).
        crate::plugins::automation::normalize_lane(&mut lane)?;
        let key = lane.id.clone();
        let to_apply = if lane.points.is_empty() { None } else { Some(lane) };
        let build = move |key: String, to_apply: Option<_>| {
            move |tx: &mut session::Tx<'_>| {
                tx.apply(op::Op::AutomationSetLane { key, lane: to_apply })
            }
        };
        let gesture_meta = meta.clone();
        let (gk, ga) = (key.clone(), to_apply.clone());
        let gesture_result = self.gesture.commit_transient_and_fold(&meta.actor, || {
            self.commit_transient_for_gesture(gesture_meta, build(gk, ga))
        });
        match gesture_result {
            Some(result) => {
                result?;
            }
            None => {
                self.commit(meta, build(key, to_apply))?;
            }
        }
        Ok(())
    }
```

  (`AutomationLane` derives `Clone`, so the two-closure split above compiles;
  if the borrow checker still objects, factor the closure body into a free
  `fn apply_lane(key: String, lane: Option<AutomationLane>, tx: &mut Tx) ->
  Result<(), String>` exactly as `apply_mix_changes` is factored at
  `mod.rs:1056`, and build a fresh closure at each call site.)

  Then `plugins/automation.rs`'s `automation_set` body becomes:

```rust
    control.set_automation_lane(lane, crate::control::op::TxMeta::user("edit automation"))?;
    Ok(control.automation_lanes())
```
  (drop the now-duplicated id-mint/normalize/`to_apply` block — it moved into
  the `ControlPlane` method, which is the single production entry point.)

- [ ] **Step 5: Run the backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 533 + 2 = **535**, green.

- [ ] **Step 6: Commit** —

```bash
git add src-tauri/src/control/mod.rs src-tauri/src/plugins/automation.rs
git commit -m "feat(automation): lane edits fold into gestures — a drag is one undo entry and one persist"
```

---

### Task 5: Frontend automation store, IPC bindings, and the M-3 undo/redo re-pull

There is no automation code in `src/` at all today (`grep -ri automation
src/lib` → nothing). This task adds the store and the bindings, and fixes the
review's frontend M-3: `projectops.step()` re-pulls project + midi and
nothing else, so undoing an automation-lane or plugin-param step leaves those
views stale.

**Files:**
- Modify: `src/lib/types/ipc.ts` — `AutomationPoint`, `AutomationLane`
- Modify: `src/lib/tauri.ts` — `automationGet?`/`automationSet?` on the
  `Backend` interface + the `TauriBackend` implementations
- Create: `src/lib/state/automation.svelte.ts`
- Modify: `src/lib/state/plugins.svelte.ts` — `reloadOpenParams`
- Modify: `src/lib/state/projectops.svelte.ts` — `step()` and `adopt()`
- Create: `src/lib/state/automation.svelte.test.ts`
- Modify: `src/lib/state/projectops.test.ts` — the M-3 assertions

**Interfaces:**
- Consumes: `backend.automationGet()`/`automationSet(lane)` (the already
  frozen, already registered `automation_get`/`automation_set` commands),
  `project.beginGesture`/`endGesture`, `midi.sampleAtTick`/`tickAtSample`.
- Produces:
  ```ts
  // src/lib/types/ipc.ts
  export interface AutomationPoint { tick: number; value: number }
  export interface AutomationLane {
    id: string;
    /** `"track:<trackId>"` for built-in track params; a plugin instance id
     * for plugin params. */
    targetNode: string;
    paramId: number;
    points: AutomationPoint[];
  }

  // src/lib/state/automation.svelte.ts
  export const TRACK_TARGET_PREFIX = "track:";
  export const TRACK_PARAM_GAIN = 0;
  export function trackTarget(trackId: string): string;

  class AutomationStore {
    lanes: AutomationLane[];
    /** Track ids whose automation overlay is shown. */
    visible: Set<string>;
    laneFor(targetNode: string, paramId: number): AutomationLane | undefined;
    gainLaneFor(trackId: string): AutomationLane | undefined;
    reload(): Promise<void>;
    /** Local-only preview during a drag (no invoke) — mirrors
     * `project.moveClip`. */
    preview(laneId: string, points: AutomationPoint[]): void;
    /** Persist a lane through `automation_set`; empty `points` deletes it.
     * Applies the returned authoritative list. */
    commit(lane: AutomationLane): Promise<void>;
    toggleVisible(trackId: string): void;
    isVisible(trackId: string): boolean;
  }
  export const automation: AutomationStore;
  ```

- [ ] **Step 1: Write the failing store test** —
  `src/lib/state/automation.svelte.test.ts`:

```ts
/**
 * Automation lane store: a thin mirror of the backend's `automation[]`
 * (ADR 0006 — no authoritative state here, no tick math here). `preview`
 * is the drag-time local patch; `commit` is the one invoke per gesture.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AutomationLane } from "../types/ipc";

const lanesOnServer: AutomationLane[] = [];
const automationGet = vi.fn(() => Promise.resolve([...lanesOnServer]));
const automationSet = vi.fn((lane: AutomationLane) => {
  const i = lanesOnServer.findIndex((l) => l.id === lane.id);
  if (lane.points.length === 0) {
    if (i >= 0) lanesOnServer.splice(i, 1);
  } else if (i >= 0) lanesOnServer[i] = lane;
  else lanesOnServer.push(lane);
  return Promise.resolve([...lanesOnServer]);
});

vi.mock("../tauri", () => ({
  backend: { mode: "tauri", on: () => () => {}, automationGet, automationSet },
}));

const { automation, TRACK_PARAM_GAIN, trackTarget } = await import("./automation.svelte");

const lane = (id: string, trackId: string): AutomationLane => ({
  id,
  targetNode: trackTarget(trackId),
  paramId: TRACK_PARAM_GAIN,
  points: [
    { tick: 0, value: 1 },
    { tick: 3840, value: 0 },
  ],
});

beforeEach(() => {
  vi.clearAllMocks();
  lanesOnServer.length = 0;
  automation.lanes = [];
  automation.visible = new Set();
});

describe("automation store", () => {
  it("reload pulls the backend list", async () => {
    lanesOnServer.push(lane("a", "t-1"));
    await automation.reload();
    expect(automation.lanes).toHaveLength(1);
    expect(automation.gainLaneFor("t-1")?.id).toBe("a");
    expect(automation.gainLaneFor("t-2")).toBeUndefined();
  });

  it("preview patches locally without invoking", () => {
    automation.lanes = [lane("a", "t-1")];
    automation.preview("a", [{ tick: 0, value: 0.5 }]);
    expect(automation.gainLaneFor("t-1")!.points).toEqual([{ tick: 0, value: 0.5 }]);
    expect(automationSet).not.toHaveBeenCalled();
  });

  it("commit invokes once and adopts the backend's authoritative list", async () => {
    await automation.commit(lane("a", "t-1"));
    expect(automationSet).toHaveBeenCalledTimes(1);
    expect(automation.lanes).toHaveLength(1);
  });

  it("commit with no points deletes the lane", async () => {
    await automation.commit(lane("a", "t-1"));
    await automation.commit({ ...lane("a", "t-1"), points: [] });
    expect(automation.lanes).toHaveLength(0);
  });

  it("reload is silent when the backend has no automation commands (demo)", async () => {
    const { backend } = await import("../tauri");
    const saved = (backend as Record<string, unknown>).automationGet;
    delete (backend as Record<string, unknown>).automationGet;
    await automation.reload();
    expect(automation.lanes).toEqual([]);
    (backend as Record<string, unknown>).automationGet = saved;
  });

  it("toggleVisible flips per-track overlay visibility", () => {
    expect(automation.isVisible("t-1")).toBe(false);
    automation.toggleVisible("t-1");
    expect(automation.isVisible("t-1")).toBe(true);
    automation.toggleVisible("t-1");
    expect(automation.isVisible("t-1")).toBe(false);
  });
});
```

- [ ] **Step 2: Add the M-3 assertions to `src/lib/state/projectops.test.ts`.**
  Extend the mock object (top of file) with:

```ts
  automationGet: vi.fn(() => Promise.resolve([])),
  pluginList: vi.fn(() =>
    Promise.resolve({ plugins: [], scanned: true, instances: [] }),
  ),
  pluginGetParams: vi.fn(() => Promise.resolve([])),
```

  and add to the `describe("undo / redo (Plan E Task 17)")` block:

```ts
  /**
   * M-3 (Plan E whole-branch review): `step()` re-pulled project + midi and
   * nothing else, so undoing an automation-lane or plugin-param step left
   * those views showing pre-undo values until something else refreshed
   * them. The re-pull now covers every store an op can address.
   */
  it("re-pulls automation lanes and plugin panels after an undo", async () => {
    const { plugins } = await import("./plugins.svelte");
    plugins.openInstanceId = "inst-1";
    await projectops.undo();
    expect(invokes.automationGet).toHaveBeenCalledTimes(1);
    expect(invokes.pluginList).toHaveBeenCalledTimes(1);
    expect(invokes.pluginGetParams).toHaveBeenCalledWith("inst-1");
  });

  it("re-pulls them after a redo too", async () => {
    await projectops.redo();
    expect(invokes.automationGet).toHaveBeenCalledTimes(1);
    expect(invokes.pluginList).toHaveBeenCalledTimes(1);
  });
```

- [ ] **Step 3: Run both new test files to verify they fail** —
  `timeout 300 npx vitest run src/lib/state/automation.svelte.test.ts src/lib/state/projectops.test.ts`
  Expected: FAIL (module `./automation.svelte` not found; `automationGet`
  never called).

- [ ] **Step 4: Add the IPC types.** In `src/lib/types/ipc.ts`, next to the
  plugin types:

```ts
/** One automation breakpoint in MUSICAL time (ticks @ project ppq). Between
 * points the value ramps linearly; before the first / after the last it
 * holds. Mirrors `plugins::automation::AutomationPoint`. */
export interface AutomationPoint {
  tick: number;
  value: number;
}

/** One parameter's automation curve. `targetNode` is `"track:<trackId>"` for
 * built-in track params (today: gain, `paramId` 0) or a plugin instance id
 * for plugin params. Mirrors `plugins::automation::AutomationLane`. */
export interface AutomationLane {
  id: string;
  targetNode: string;
  paramId: number;
  points: AutomationPoint[];
}
```

- [ ] **Step 5: Add the backend bindings.** In `src/lib/tauri.ts`, on the
  `Backend` interface (optional-method convention, same as `moveClip?`):

```ts
  /** All automation lanes (points inline — per-project, not per-frame).
   * Optional: the demo backend has no automation. */
  automationGet?(): Promise<AutomationLane[]>;
  /** Upsert one lane; an empty `points` array deletes it. One invoke carries
   * the lane's FULL point set (D-03: an edit gesture, never one invoke per
   * point). Returns the updated lane list. */
  automationSet?(lane: AutomationLane): Promise<AutomationLane[]>;
```

  and on `TauriBackend`, next to `moveClip`:

```ts
  automationGet() {
    return invoke<AutomationLane[]>("automation_get");
  }
  automationSet(lane: AutomationLane) {
    return invoke<AutomationLane[]>("automation_set", { lane });
  }
```

  (Import `AutomationLane` from `./types/ipc` in `tauri.ts`. The Rust command
  takes `lane: AutomationLane` — Tauri camelCases the argument name, so the
  key is `lane`.)

- [ ] **Step 6: Write the store** — `src/lib/state/automation.svelte.ts`:

```ts
/**
 * Automation lane mirror. Thin by construction (ADR 0006): the backend owns
 * the lanes, this store holds a copy for painting and emits whole-lane
 * replaces through `automation_set`. No tick<->sample math lives here —
 * callers convert through `midi.sampleAtTick`/`midi.tickAtSample`, which read
 * the backend-shipped section table.
 *
 * Edit shape (D-03 + round-2 §4.4): a drag PREVIEWS locally (`preview`, no
 * invoke) and COMMITS once on pointerup (`commit`), inside a
 * `gesture_begin`/`gesture_end` bracket so the whole interaction is one undo
 * entry and one persist.
 */

import { backend } from "../tauri";
import type { AutomationLane, AutomationPoint } from "../types/ipc";

/** `targetNode` prefix for built-in params of a track's node. */
export const TRACK_TARGET_PREFIX = "track:";
/** Built-in track param ids (mirrors `plugins::automation`'s constants). */
export const TRACK_PARAM_GAIN = 0;

export function trackTarget(trackId: string): string {
  return `${TRACK_TARGET_PREFIX}${trackId}`;
}

/** Track id of a `"track:<id>"` target, or null for a plugin target. */
export function trackIdOfTarget(targetNode: string): string | null {
  return targetNode.startsWith(TRACK_TARGET_PREFIX)
    ? targetNode.slice(TRACK_TARGET_PREFIX.length)
    : null;
}

class AutomationStore {
  lanes = $state<AutomationLane[]>([]);
  /** Track ids whose automation overlay is shown (reassigned, not mutated —
   * the reactivity convention PianoRoll's selection Set uses). */
  visible = $state<Set<string>>(new Set());

  laneFor(targetNode: string, paramId: number): AutomationLane | undefined {
    return this.lanes.find((l) => l.targetNode === targetNode && l.paramId === paramId);
  }

  gainLaneFor(trackId: string): AutomationLane | undefined {
    return this.laneFor(trackTarget(trackId), TRACK_PARAM_GAIN);
  }

  lanesForPlugin(instanceId: string): AutomationLane[] {
    return this.lanes.filter((l) => l.targetNode === instanceId);
  }

  async reload(): Promise<void> {
    if (!backend.automationGet) return; // demo backend: no automation
    try {
      this.lanes = await backend.automationGet();
    } catch (err) {
      console.warn("[aura] automation_get failed:", err);
    }
  }

  /** Drag-time local patch — no invoke (mirrors `project.moveClip`). */
  preview(laneId: string, points: AutomationPoint[]) {
    this.lanes = this.lanes.map((l) => (l.id === laneId ? { ...l, points } : l));
  }

  /** Persist a lane's CURRENT point set. An empty `points` deletes it. The
   * returned list is authoritative (the backend normalizes: sorted by tick,
   * duplicate ticks collapsed last-wins). */
  async commit(lane: AutomationLane): Promise<void> {
    if (!backend.automationSet) return;
    try {
      this.lanes = await backend.automationSet(lane);
    } catch (err) {
      console.error("[aura] automation_set failed:", err);
    }
  }

  isVisible(trackId: string): boolean {
    return this.visible.has(trackId);
  }

  toggleVisible(trackId: string) {
    const next = new Set(this.visible);
    if (next.has(trackId)) next.delete(trackId);
    else next.add(trackId);
    this.visible = next;
  }
}

export const automation = new AutomationStore();
```

- [ ] **Step 7: Add `plugins.reloadOpenParams`** to
  `src/lib/state/plugins.svelte.ts`, next to `openParams`:

```ts
  /** Re-pull the OPEN instance's params without closing/reopening the panel
   * (M-3): an undo/redo of a plugin-param step changed the backend's values
   * under a panel that is still showing the pre-undo ones. No-op when no
   * panel is open. */
  async reloadOpenParams(): Promise<void> {
    const id = this.openInstanceId;
    if (!id) return;
    try {
      this.params = await backend.pluginGetParams(id);
      this.paramError = null;
    } catch (err) {
      this.paramError = String(err);
    }
  }
```

- [ ] **Step 8: Fix M-3 in `src/lib/state/projectops.svelte.ts`.** Add the
  imports (`automation`, `plugins`) and extend both re-pull paths:

```ts
  private async step(dir: "undo" | "redo") {
    const call = dir === "undo" ? backend.undo : backend.redo;
    if (!call) return; // demo backend: no op log to walk
    try {
      const step = await call.call(backend);
      if (!step?.label) return; // nothing to undo/redo — silent, not an error
      await this.repull();
      toasts.info(dir === "undo" ? "UNDO" : "REDO", step.label);
    } catch (err) {
      toasts.error(dir === "undo" ? "UNDO FAILED" : "REDO FAILED", String(err));
    }
  }

  /**
   * Re-pull every store an op can address. `project://changed` carries only
   * the `Project` shape, so midi, automation and the plugin registry each
   * need their own pull — M-3 (Plan E whole-branch review): this used to
   * stop after project + midi, leaving automation lanes and an open plugin
   * param panel showing pre-undo values until something else refreshed them.
   */
  private async repull() {
    await project.reload();
    await midi.init();
    await automation.reload();
    await plugins.refresh();
    await plugins.reloadOpenParams();
  }

  /** Re-pull the full snapshot; `project://changed` alone leaves projectDir stale. */
  private async adopt() {
    // Finding 8: drop any loop-while-editing state from the OLD project
    // BEFORE pulling the new one in.
    clipEditLoop.reset();
    await this.repull();
  }
```

- [ ] **Step 9: Run the frontend suite** —
  `timeout 300 npx vitest run`
  Expected: 207 + 6 (automation store) + 2 (M-3) = **215**, green.

- [ ] **Step 10: Type-check** — `timeout 300 npx svelte-check` — no new
  errors (the repo's standing pre-PR check, CONTRIBUTING.md:84).

- [ ] **Step 11: Commit** —

```bash
git add src/lib/types/ipc.ts src/lib/tauri.ts src/lib/state/automation.svelte.ts \
        src/lib/state/automation.svelte.test.ts src/lib/state/plugins.svelte.ts \
        src/lib/state/projectops.svelte.ts src/lib/state/projectops.test.ts
git commit -m "feat(automation): frontend lane store + undo/redo re-pull for automation and plugin panels (M-3)"
```

---

### Task 6: The timeline automation lane — draw, drag, delete

Per scope ruling 8 the lane is an OVERLAY canvas inside the track's existing
`.lane` div, toggled per track from `TrackHeader`. All point math lives in a
pure, separately tested util module (the pattern `src/lib/utils/note-ops.ts`
established for the piano roll).

**Files:**
- Create: `src/lib/utils/automation-edit.ts`
- Create: `src/lib/utils/automation-edit.test.ts`
- Create: `src/lib/components/AutomationLaneView.svelte`
- Modify: `src/lib/components/Timeline.svelte` — render the overlay in each
  `.lane`
- Modify: `src/lib/components/TrackHeader.svelte` — an `A` toggle button

**Interfaces:**
- Consumes: `automation` store (Task 5), `view.xOf`/`view.samplesAt`/
  `view.snapSamples` (`src/lib/state/view.svelte.ts`),
  `midi.sampleAtTick`/`midi.tickAtSample`, `canvasPos`
  (`src/lib/utils/canvas-pos.ts` — MANDATORY for every pointer→canvas
  conversion; raw `clientX - rect.left` is the interface-zoom bug PR #11
  fixed), `project.beginGesture`/`endGesture`.
- Produces:
  ```ts
  // src/lib/utils/automation-edit.ts
  export interface Pt { tick: number; value: number }
  /** Index of the point within `radiusPx` of (tick, value) in SCREEN space,
   * or -1. `tickPerPx`/`valuePerPx` convert the radius into domain units. */
  export function hitTest(
    points: Pt[], tick: number, value: number,
    tickPerPx: number, valuePerPx: number, radiusPx: number,
  ): number;
  /** Insert `p`, keeping the array sorted by tick; an exact tick collision
   * REPLACES the existing point (the same last-wins rule `normalize_lane`
   * applies backend-side). Returns a new array. */
  export function insertPoint(points: Pt[], p: Pt): Pt[];
  /** Move `points[index]` to (tick, value), clamped to tick >= 0 and
   * value in [min, max], re-sorted. Returns the new array AND the moved
   * point's new index. */
  export function movePoint(
    points: Pt[], index: number, tick: number, value: number,
    min: number, max: number,
  ): { points: Pt[]; index: number };
  /** Remove `points[index]`; out-of-range is a no-op. */
  export function deletePoint(points: Pt[], index: number): Pt[];
  ```

- [ ] **Step 1: Write the failing util test** —
  `src/lib/utils/automation-edit.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { deletePoint, hitTest, insertPoint, movePoint, type Pt } from "./automation-edit";

const pts: Pt[] = [
  { tick: 0, value: 1 },
  { tick: 960, value: 0.5 },
  { tick: 3840, value: 0 },
];

describe("hitTest", () => {
  it("finds the point under the cursor within the radius", () => {
    // 10 ticks/px horizontally, 0.01 value/px vertically, 6 px radius
    expect(hitTest(pts, 955, 0.5, 10, 0.01, 6)).toBe(1);
  });
  it("misses when either axis is outside the radius", () => {
    expect(hitTest(pts, 960, 0.9, 10, 0.01, 6)).toBe(-1);
    expect(hitTest(pts, 1200, 0.5, 10, 0.01, 6)).toBe(-1);
  });
  it("returns -1 on an empty lane", () => {
    expect(hitTest([], 0, 0, 10, 0.01, 6)).toBe(-1);
  });
});

describe("insertPoint", () => {
  it("keeps the array sorted by tick", () => {
    expect(insertPoint(pts, { tick: 1920, value: 0.25 }).map((p) => p.tick)).toEqual([
      0, 960, 1920, 3840,
    ]);
  });
  it("replaces an exact tick collision (last write wins, like normalize_lane)", () => {
    const out = insertPoint(pts, { tick: 960, value: 0.75 });
    expect(out).toHaveLength(3);
    expect(out[1]).toEqual({ tick: 960, value: 0.75 });
  });
  it("does not mutate its input", () => {
    const before = structuredClone(pts);
    insertPoint(pts, { tick: 5, value: 0.5 });
    expect(pts).toEqual(before);
  });
});

describe("movePoint", () => {
  it("clamps tick to >= 0 and value to [min,max]", () => {
    const { points } = movePoint(pts, 0, -500, 3, 0, 1);
    expect(points[0]).toEqual({ tick: 0, value: 1 });
  });
  it("re-sorts and reports the moved point's new index", () => {
    const { points, index } = movePoint(pts, 0, 2000, 0.2, 0, 1);
    expect(points.map((p) => p.tick)).toEqual([960, 2000, 3840]);
    expect(index).toBe(1);
    expect(points[index]).toEqual({ tick: 2000, value: 0.2 });
  });
  it("is a no-op for an out-of-range index", () => {
    expect(movePoint(pts, 9, 0, 0, 0, 1)).toEqual({ points: pts, index: 9 });
  });
});

describe("deletePoint", () => {
  it("removes the point", () => {
    expect(deletePoint(pts, 1).map((p) => p.tick)).toEqual([0, 3840]);
  });
  it("ignores an out-of-range index", () => {
    expect(deletePoint(pts, 9)).toEqual(pts);
  });
});
```

- [ ] **Step 2: Run to verify it fails** —
  `timeout 300 npx vitest run src/lib/utils/automation-edit.test.ts`
  Expected: FAIL (module not found).

- [ ] **Step 3: Write `src/lib/utils/automation-edit.ts`:**

```ts
/**
 * Pure point-set edits for an automation lane. Kept out of the component so
 * the geometry is testable without a DOM (the split `note-ops.ts` uses for
 * the piano roll). Values are domain values; the caller converts pixels
 * through `view`/`midi` before calling in.
 *
 * The collision rule mirrors the backend's `normalize_lane`: points are
 * sorted by tick and a duplicate tick keeps the LATER value, so the frontend
 * never shows a shape `automation_set` would silently change under it.
 */

export interface Pt {
  tick: number;
  value: number;
}

export function hitTest(
  points: Pt[],
  tick: number,
  value: number,
  tickPerPx: number,
  valuePerPx: number,
  radiusPx: number,
): number {
  const dt = radiusPx * tickPerPx;
  const dv = radiusPx * valuePerPx;
  for (let i = 0; i < points.length; i++) {
    if (Math.abs(points[i].tick - tick) <= dt && Math.abs(points[i].value - value) <= dv) {
      return i;
    }
  }
  return -1;
}

function sorted(points: Pt[]): Pt[] {
  return [...points].sort((a, b) => a.tick - b.tick);
}

export function insertPoint(points: Pt[], p: Pt): Pt[] {
  const out = points.filter((q) => q.tick !== p.tick);
  out.push(p);
  return sorted(out);
}

export function movePoint(
  points: Pt[],
  index: number,
  tick: number,
  value: number,
  min: number,
  max: number,
): { points: Pt[]; index: number } {
  if (index < 0 || index >= points.length) return { points, index };
  const moved: Pt = {
    tick: Math.max(0, Math.round(tick)),
    value: Math.min(max, Math.max(min, value)),
  };
  const rest = points.filter((_, i) => i !== index).filter((q) => q.tick !== moved.tick);
  const out = sorted([...rest, moved]);
  return { points: out, index: out.findIndex((q) => q === moved) };
}

export function deletePoint(points: Pt[], index: number): Pt[] {
  if (index < 0 || index >= points.length) return points;
  return points.filter((_, i) => i !== index);
}
```

- [ ] **Step 4: Run to verify it passes** —
  `timeout 300 npx vitest run src/lib/utils/automation-edit.test.ts`
  Expected: PASS (13 tests).

- [ ] **Step 5: Write `src/lib/components/AutomationLaneView.svelte`:**

```svelte
<script lang="ts">
  /**
   * Per-track automation overlay (scope ruling 8: an overlay INSIDE the
   * track's existing timeline row, not an added row — the left rail and the
   * lane column share `--track-height` and inserting a sub-row would
   * desynchronise them).
   *
   * Values are LINEAR GAIN MULTIPLIERS in [0,1] applied on top of the fader
   * (scope ruling 6): y=top is 1.0 (fader unchanged), y=bottom is 0.0
   * (silence).
   *
   * Edit shape: pointerdown opens a gesture and either grabs an existing
   * point or inserts one; pointermove PREVIEWS locally (no invoke);
   * pointerup commits ONCE through `automation_set` and closes the gesture
   * (scope ruling 7). Alt/right-click deletes a point.
   */
  import type { TrackState } from "../types/ipc";
  import type { AutomationLane } from "../types/ipc";
  import { automation, TRACK_PARAM_GAIN, trackTarget } from "../state/automation.svelte";
  import { midi } from "../state/midi.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { canvasPos } from "../utils/canvas-pos";
  import { deletePoint, hitTest, insertPoint, movePoint } from "../utils/automation-edit";

  let { track }: { track: TrackState } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let dragIndex = $state(-1);
  const HIT_RADIUS_PX = 6;

  const lane = $derived(automation.gainLaneFor(track.id));

  /** The lane this overlay edits, minted locally when the track has none
   * yet. Empty id = "the backend mints one" (`automation_set`'s contract). */
  function laneOrNew(): AutomationLane {
    return (
      lane ?? {
        id: "",
        targetNode: trackTarget(track.id),
        paramId: TRACK_PARAM_GAIN,
        points: [],
      }
    );
  }

  function xOfTick(tick: number): number {
    return view.xOf(midi.sampleAtTick(tick));
  }
  function tickAtX(x: number): number {
    return midi.tickAtSample(view.snapSamples(view.samplesAt(x)));
  }

  $effect(() => {
    // touch the reactive inputs so the canvas repaints on any of them
    void [lane?.points, view.viewStart, view.spp, view.width, canvas];
    paint();
  });

  function paint() {
    const c = canvas;
    if (!c) return;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (w === 0 || h === 0) return;
    c.width = Math.max(1, Math.round(w * dpr));
    c.height = Math.max(1, Math.round(h * dpr));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const pts = lane?.points ?? [];
    if (pts.length === 0) return;

    ctx.strokeStyle = track.color ?? "var(--cyan)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    // hold before the first and after the last point (the compile contract)
    ctx.moveTo(0, (1 - pts[0].value) * h);
    for (const p of pts) ctx.lineTo(xOfTick(p.tick), (1 - p.value) * h);
    ctx.lineTo(w, (1 - pts[pts.length - 1].value) * h);
    ctx.stroke();
    ctx.fillStyle = track.color ?? "var(--cyan)";
    for (const p of pts) {
      ctx.beginPath();
      ctx.arc(xOfTick(p.tick), (1 - p.value) * h, 3, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  function domainAt(e: PointerEvent | MouseEvent) {
    const p = canvasPos(canvas!, e.clientX, e.clientY);
    const h = canvas!.clientHeight || 1;
    return { tick: tickAtX(p.x), value: Math.min(1, Math.max(0, 1 - p.y / h)) };
  }

  function onPointerDown(e: PointerEvent) {
    if (!canvas) return;
    const { tick, value } = domainAt(e);
    const l = laneOrNew();
    const tickPerPx = Math.max(1e-9, midi.tickAtSample(view.spp) - midi.tickAtSample(0));
    const valuePerPx = 1 / Math.max(1, canvas.clientHeight);
    const hit = hitTest(l.points, tick, value, tickPerPx, valuePerPx, HIT_RADIUS_PX);

    if (e.button === 2 || e.altKey) {
      if (hit < 0) return;
      project.beginGesture("automation delete point");
      void automation.commit({ ...l, points: deletePoint(l.points, hit) }).then(() =>
        project.endGesture(),
      );
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    project.beginGesture("automation edit");
    if (hit >= 0) {
      dragIndex = hit;
      if (l.id) automation.preview(l.id, l.points);
    } else {
      const points = insertPoint(l.points, { tick, value });
      dragIndex = points.findIndex((p) => p.tick === tick);
      // an unminted lane must reach the backend before it can be previewed
      void automation.commit({ ...l, points });
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (dragIndex < 0 || !canvas) return;
    const l = automation.gainLaneFor(track.id);
    if (!l) return;
    const { tick, value } = domainAt(e);
    const moved = movePoint(l.points, dragIndex, tick, value, 0, 1);
    dragIndex = moved.index;
    automation.preview(l.id, moved.points); // LOCAL only — no invoke per move
  }

  function onPointerUp(e: PointerEvent) {
    if (!canvas) return;
    canvas.releasePointerCapture?.(e.pointerId);
    const wasDragging = dragIndex >= 0;
    dragIndex = -1;
    const l = automation.gainLaneFor(track.id);
    if (wasDragging && l) {
      void automation.commit(l).then(() => project.endGesture()); // ONE invoke per gesture
    } else {
      project.endGesture();
    }
  }
</script>

<canvas
  bind:this={canvas}
  class="autolane"
  role="presentation"
  aria-label="Gain automation for {track.name}"
  oncontextmenu={(e) => e.preventDefault()}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
></canvas>

<style>
  .autolane {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    z-index: 3;
    background: color-mix(in srgb, var(--bg-0) 45%, transparent);
    cursor: crosshair;
  }
</style>
```

- [ ] **Step 6: Render it from `Timeline.svelte`.** Add the import
  (`import AutomationLaneView from "./AutomationLaneView.svelte";` and
  `import { automation } from "../state/automation.svelte";`), and inside the
  per-track `.lane` div (after the MidiClipView `{#each}`, around `:470`):

```svelte
          {#if automation.isVisible(track.id)}
            <AutomationLaneView {track} />
          {/if}
```

  The `.lane` rule already needs `position: relative` for the overlay's
  `inset: 0` to anchor — check the existing `.lane` block (`:768`) and add
  `position: relative;` if it is absent.

- [ ] **Step 7: Add the toggle to `TrackHeader.svelte`**, in the `.toggles`
  row next to R/M/S:

```svelte
        <button
          class="tog auto"
          class:on={automation.isVisible(track.id)}
          title="Show gain automation lane"
          aria-pressed={automation.isVisible(track.id)}
          onclick={() => automation.toggleVisible(track.id)}>A</button
        >
```
  (plus `import { automation } from "../state/automation.svelte";`, and a
  `.tog.auto.on { … }` colour rule copied from the existing `.tog.arm.on`
  block so the button reads as active).

- [ ] **Step 8: Pull the lanes at cold start.** Wherever the app's other
  stores initialise (search for `midi.init()` in `src/App.svelte` or
  `src/lib/state`), add `void automation.reload();` alongside — the lanes
  otherwise stay empty until the first undo.

- [ ] **Step 9: Run both suites + type-check** —
  `timeout 300 npx vitest run` → 215 + 13 = **228**, green.
  `timeout 300 npx svelte-check` → no new errors.
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` → 535, green.

- [ ] **Step 10: Commit** —

```bash
git add src/lib/utils/automation-edit.ts src/lib/utils/automation-edit.test.ts \
        src/lib/components/AutomationLaneView.svelte src/lib/components/Timeline.svelte \
        src/lib/components/TrackHeader.svelte src/App.svelte
git commit -m "feat(automation): timeline gain-automation lane — draw, drag and delete points"
```

---

### Task 7: Plugin-parameter lane targets — resolution, the control-side driver, the UI entry point

**Files:**
- Modify: `src-tauri/src/plugins/automation.rs` — `TRACK_PARAM_GAIN`,
  `LaneTarget`, `resolve_target`, `RampCursor`, `ParamWrite`,
  `ParamAutomationDriver`
- Modify: `src-tauri/src/control/mod.rs` — extract
  `forward_param_to_host`, used by both `execute_host_forward`'s `ParamWrite`
  arm and (Task 9) the engine's driver
- Modify: `src/lib/state/automation.svelte.ts` — `pluginLaneFor`
- Modify: `src/lib/components/plugins/PluginParamPanel.svelte` — an
  "automate" affordance per param
- Test: `src-tauri/src/plugins/automation.rs` tests

**Interfaces:**
- Consumes: `AutomationLane`, `compile_lane(&AutomationLane, &TempoMap) ->
  Vec<AbsParamEvent>`, `AbsParamEvent { sample: u64, value: f32 }`,
  `session::PluginDoc` (`instances: Vec<PluginInstanceInfo>` with `id`/`format`).
- Produces:
  ```rust
  // plugins/automation.rs
  /// `targetNode` prefix addressing a track's built-in node params.
  pub const TRACK_TARGET_PREFIX: &str = "track:";
  /// Built-in param ids on a `"track:<id>"` target node. Gain is 0 — the id
  /// this module's own tests have used since Plan E Task 10.
  pub const TRACK_PARAM_GAIN: u32 = 0;

  #[derive(Debug, Clone, PartialEq)]
  pub enum LaneTarget {
      /// Track id; the lane's value is a LINEAR GAIN MULTIPLIER applied on
      /// top of the fader (scope ruling 6).
      TrackGain(String),
      PluginParam { instance: String, index: u32 },
  }

  /// Classify a lane's target. `None` for a `"track:"` target naming a
  /// built-in param this build does not know — an unknown id is ignored,
  /// never guessed at.
  pub fn resolve_target(lane: &AutomationLane) -> Option<LaneTarget>;

  /// RT-side ramp reader: O(1) amortized per frame, correct across BACKWARD
  /// jumps (a loop wrap moves the position back mid-block). No allocation,
  /// no locks — safe on the audio callback.
  pub struct RampCursor { /* idx, last */ }
  impl RampCursor {
      pub fn new() -> Self;
      /// Value at `sample`, or `None` for an empty curve (parameter
      /// untouched — the caller's neutral applies).
      #[inline]
      pub fn value(&mut self, events: &[AbsParamEvent], sample: u64) -> Option<f32>;
  }

  /// One host param write the driver decided is due. `format` is carried so
  /// the tick path never has to take the session lock to look it up.
  #[derive(Debug, Clone, PartialEq)]
  pub struct ParamWrite { pub instance: String, pub format: String, pub index: u32, pub value: f32 }

  /// Control-thread evaluator for plugin-parameter lanes (scope ruling 2:
  /// block-rate, on the engine control thread, host-only). Built fresh at
  /// every rebuild from the session's lanes + plugin rows.
  pub struct ParamAutomationDriver { /* compiled lanes */ }
  impl ParamAutomationDriver {
      /// Values closer than this to the last emitted one are not re-sent.
      pub const EPSILON: f32 = 1e-4;
      pub fn new(
          lanes: &[AutomationLane],
          plugins: &crate::control::session::PluginDoc,
          map: &TempoMap,
      ) -> Self;
      pub fn empty() -> Self;
      pub fn is_empty(&self) -> bool;
      /// Append every write due at `position` into `out` (cleared first).
      pub fn tick(&mut self, position: u64, out: &mut Vec<ParamWrite>);
  }

  // control/mod.rs
  /// Forward one already-clamped param value to whichever host owns
  /// `instance`. Shared by `execute_host_forward`'s `ParamWrite` arm and the
  /// engine's automation driver (Track D).
  pub(crate) fn forward_param_to_host(instance: &str, format: &str, index: u32, value: f32);
  ```

- [ ] **Step 1: Write the failing tests** in `plugins/automation.rs`'s test
  module:

```rust
    #[test]
    fn resolve_target_classifies_track_and_plugin_lanes() {
        let mut l = lane(vec![]);
        l.target_node = "track:t-1".into();
        l.param_id = TRACK_PARAM_GAIN;
        assert_eq!(resolve_target(&l), Some(LaneTarget::TrackGain("t-1".into())));

        // an unknown built-in param id is IGNORED, never guessed at
        l.param_id = 99;
        assert_eq!(resolve_target(&l), None);

        let mut p = lane(vec![]);
        p.target_node = "inst-1".into();
        p.param_id = 7;
        assert_eq!(
            resolve_target(&p),
            Some(LaneTarget::PluginParam { instance: "inst-1".into(), index: 7 })
        );
    }

    #[test]
    fn ramp_cursor_matches_value_at_forward_and_after_a_backward_jump() {
        let ev = vec![
            AbsParamEvent { sample: 100, value: 1.0 },
            AbsParamEvent { sample: 200, value: 0.5 },
        ];
        let mut c = RampCursor::new();
        assert_eq!(c.value(&[], 0), None, "empty curve leaves the param untouched");
        for s in [0u64, 50, 100, 150, 199, 200, 10_000] {
            assert_eq!(c.value(&ev, s), value_at(&ev, s), "forward walk at {s}");
        }
        // a loop wrap moves the position BACKWARD: the cursor must re-seed,
        // not keep its advanced index
        assert_eq!(c.value(&ev, 150), Some(0.75), "re-seeded after a backward jump");
        assert_eq!(c.value(&ev, 0), Some(1.0));
    }

    #[test]
    fn param_driver_emits_only_on_change_and_carries_the_host_format() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut l = lane(vec![
            AutomationPoint { tick: 0, value: 0.0 },
            AutomationPoint { tick: 3840, value: 1.0 }, // 96_000 samples
        ]);
        l.target_node = "inst-1".into();
        l.param_id = 7;

        let mut doc = PluginDoc::default();
        doc.instances.push(crate::plugins::PluginInstanceInfo {
            id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
        });

        let mut d = ParamAutomationDriver::new(&[l], &doc, &map);
        assert!(!d.is_empty());
        let mut out = Vec::new();

        d.tick(0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].format, "lv2");
        assert_eq!(out[0].index, 7);
        assert!((out[0].value - 0.0).abs() < 1e-6);

        d.tick(0, &mut out);
        assert!(out.is_empty(), "an unchanged value is not re-sent");

        d.tick(48_000, &mut out);
        assert_eq!(out.len(), 1);
        assert!((out[0].value - 0.5).abs() < 1e-3, "halfway up the ramp: {}", out[0].value);
    }

    #[test]
    fn param_driver_skips_lanes_whose_instance_is_gone_and_track_lanes() {
        use crate::control::session::PluginDoc;
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut orphan = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        orphan.target_node = "inst-missing".into();
        orphan.param_id = 1;
        let track_lane = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]); // "track:t1"
        let d = ParamAutomationDriver::new(&[orphan, track_lane], &PluginDoc::default(), &map);
        assert!(d.is_empty(), "no plugin lane resolves");
    }
```

  (`lane(..)` is the module's existing test helper at `:608`; it builds a
  `"track:t1"` / `param_id: 0` lane, which is why the last test uses it as
  the track-lane case.)

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- automation::tests`
  Expected: FAIL to compile.

- [ ] **Step 3: Implement in `plugins/automation.rs`**, in a new section
  after `value_at`:

```rust
// ---------------------------------------------------------------------------
// Lane targets, the RT ramp cursor, and the control-side param driver
// (Track D)
// ---------------------------------------------------------------------------

pub const TRACK_TARGET_PREFIX: &str = "track:";
pub const TRACK_PARAM_GAIN: u32 = 0;

#[derive(Debug, Clone, PartialEq)]
pub enum LaneTarget {
    TrackGain(String),
    PluginParam { instance: String, index: u32 },
}

pub fn resolve_target(lane: &AutomationLane) -> Option<LaneTarget> {
    match lane.target_node.strip_prefix(TRACK_TARGET_PREFIX) {
        Some(track_id) if !track_id.is_empty() => match lane.param_id {
            TRACK_PARAM_GAIN => Some(LaneTarget::TrackGain(track_id.to_string())),
            // Unknown built-in param: ignored, never guessed at. New ids are
            // additive here and in `automation-edit`'s frontend twin.
            _ => None,
        },
        Some(_) => None, // "track:" with no id
        None if !lane.target_node.is_empty() => Some(LaneTarget::PluginParam {
            instance: lane.target_node.clone(),
            index: lane.param_id,
        }),
        None => None,
    }
}

pub struct RampCursor {
    idx: usize,
    last: u64,
}

impl Default for RampCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl RampCursor {
    pub fn new() -> Self {
        // `u64::MAX` forces the first call to re-seed by binary search.
        Self { idx: 0, last: u64::MAX }
    }

    #[inline]
    pub fn value(&mut self, events: &[AbsParamEvent], sample: u64) -> Option<f32> {
        if events.is_empty() {
            return None;
        }
        if sample < self.last {
            // Backward jump (loop wrap, seek): re-seed. O(log n), once.
            self.idx = events.partition_point(|e| e.sample <= sample);
        } else {
            while self.idx < events.len() && events[self.idx].sample <= sample {
                self.idx += 1;
            }
        }
        self.last = sample;
        Some(segment_value(events, self.idx, sample))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamWrite {
    pub instance: String,
    pub format: String,
    pub index: u32,
    pub value: f32,
}

struct CompiledParamLane {
    instance: String,
    format: String,
    index: u32,
    events: Vec<AbsParamEvent>,
    cursor: RampCursor,
    last_emitted: Option<f32>,
}

/// Control-thread evaluator for plugin-parameter lanes. See scope ruling 2
/// in `docs/superpowers/plans/2026-08-14-automation-audible.md`: block-rate
/// (the engine control loop's ≤2 ms tick), never on the audio callback (a
/// host param write is a blocking round-trip), and HOST-ONLY — automation
/// overrides the stored knob value during playback; the document keeps what
/// the user set.
#[derive(Default)]
pub struct ParamAutomationDriver {
    lanes: Vec<CompiledParamLane>,
}

impl ParamAutomationDriver {
    pub const EPSILON: f32 = 1e-4;

    pub fn empty() -> Self {
        Self { lanes: Vec::new() }
    }

    pub fn new(
        lanes: &[AutomationLane],
        plugins: &crate::control::session::PluginDoc,
        map: &TempoMap,
    ) -> Self {
        let mut compiled = Vec::new();
        for lane in lanes {
            let Some(LaneTarget::PluginParam { instance, index }) = resolve_target(lane) else {
                continue;
            };
            let Some(row) = plugins.instances.iter().find(|r| r.id == instance) else {
                continue; // the instance is gone; the lane stays on disk, silent
            };
            let events = compile_lane(lane, map);
            if events.is_empty() {
                continue;
            }
            compiled.push(CompiledParamLane {
                instance,
                format: row.format.clone(),
                index,
                events,
                cursor: RampCursor::new(),
                last_emitted: None,
            });
        }
        Self { lanes: compiled }
    }

    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    pub fn tick(&mut self, position: u64, out: &mut Vec<ParamWrite>) {
        out.clear();
        for l in self.lanes.iter_mut() {
            let Some(v) = l.cursor.value(&l.events, position) else { continue };
            if let Some(prev) = l.last_emitted {
                if (v - prev).abs() <= Self::EPSILON {
                    continue;
                }
            }
            l.last_emitted = Some(v);
            out.push(ParamWrite {
                instance: l.instance.clone(),
                format: l.format.clone(),
                index: l.index,
                value: v,
            });
        }
    }
}
```

- [ ] **Step 4: Extract `forward_param_to_host`** in `control/mod.rs`, as a
  free `pub(crate)` fn in the module (next to `set_prop`), and make
  `execute_host_forward`'s `ParamWrite` arm call it:

```rust
/// Forward one already-clamped param value to whichever host owns
/// `instance`. Two callers: `Committer::execute_host_forward`'s `ParamWrite`
/// arm (a document edit's host effect) and the engine control thread's
/// automation driver (Track D — an RT-visible override that never touches
/// the document; see `ParamAutomationDriver`'s doc and
/// `docs/SIDE-CHANNEL-INVENTORY.md`). Taking `format` as an argument, rather
/// than looking it up, is what lets the driver run with zero session locks
/// on its 2 ms tick.
pub(crate) fn forward_param_to_host(instance: &str, format: &str, index: u32, value: f32) {
    use crate::plugins::{clap_host, lv2_host};
    match format {
        "lv2" => {
            if let Some(host) = lv2_host::try_global() {
                host.set_params(instance, vec![(index, value)]);
            }
        }
        "clap" => {
            let change = crate::plugins::ParamChange { id: index, value: value as f64 };
            if let Err(e) = clap_host::set_params(instance, vec![change]) {
                log::warn!("plugins: clap param write for {instance}: {e}");
            }
        }
        _ => {}
    }
}
```

  The `ParamWrite` arm becomes:

```rust
                HostForward::ParamWrite { instance, index, value } => {
                    let format = {
                        let s = self.session.lock();
                        s.plugins.instances.iter().find(|r| &r.id == instance).map(|r| r.format.clone())
                    };
                    if let Some(format) = format {
                        forward_param_to_host(instance, &format, *index, *value);
                    }
                }
```

- [ ] **Step 5: Run the backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 535 + 4 = **539**, green.

- [ ] **Step 6: Frontend — a plugin-param lane entry point.** In
  `src/lib/state/automation.svelte.ts` add:

```ts
  /** The lane automating one plugin instance's parameter, if any. */
  pluginLaneFor(instanceId: string, paramId: number): AutomationLane | undefined {
    return this.laneFor(instanceId, paramId);
  }

  /** Create (or clear) a flat lane at `value` for a plugin parameter — the
   * "automate this knob" affordance's backing call. One point is enough to
   * make the lane exist and visible; the user then draws on it. */
  async automatePluginParam(instanceId: string, paramId: number, value: number) {
    const existing = this.pluginLaneFor(instanceId, paramId);
    if (existing) {
      await this.commit({ ...existing, points: [] }); // toggle off = delete
      return;
    }
    await this.commit({
      id: "",
      targetNode: instanceId,
      paramId,
      points: [{ tick: 0, value }],
    });
  }
```

  In `src/lib/components/plugins/PluginParamPanel.svelte`, add a small
  toggle next to each param's value readout:

```svelte
                  <button
                    class="autobtn mono"
                    class:on={!!automation.pluginLaneFor(plugins.openInstanceId, p.id)}
                    title="Automate {p.name}"
                    aria-pressed={!!automation.pluginLaneFor(plugins.openInstanceId, p.id)}
                    onclick={() =>
                      void automation.automatePluginParam(plugins.openInstanceId, p.id, p.value)}
                    >A</button
                  >
```
  (plus `import { automation } from "../../state/automation.svelte";` and an
  `.autobtn` style modelled on the panel's existing small buttons).

  Add one store test to `src/lib/state/automation.svelte.test.ts`:

```ts
  it("automatePluginParam creates a flat lane, and toggling again deletes it", async () => {
    await automation.automatePluginParam("inst-1", 7, 0.25);
    expect(automation.pluginLaneFor("inst-1", 7)?.points).toEqual([{ tick: 0, value: 0.25 }]);
    await automation.automatePluginParam("inst-1", 7, 0.25);
    expect(automation.pluginLaneFor("inst-1", 7)).toBeUndefined();
  });
```

- [ ] **Step 7: Run both suites + type-check** —
  `timeout 300 npx vitest run` → 228 + 1 = **229**, green.
  `timeout 300 npx svelte-check` → no new errors.

- [ ] **Step 8: Commit** —

```bash
git add src-tauri/src/plugins/automation.rs src-tauri/src/control/mod.rs \
        src/lib/state/automation.svelte.ts src/lib/state/automation.svelte.test.ts \
        src/lib/components/plugins/PluginParamPanel.svelte
git commit -m "feat(automation): plugin-parameter lane targets — resolution, control-side driver, UI entry point"
```

---

### Task 8: RT plumbing — slot-indexed gain ramps applied by the mixer

Everything the engine will need, WITHOUT touching `engine.rs`. `RtGraph`
gains a slot-indexed ramp table (empty by default, so no constructor call
site changes); the mixer applies it per frame through `RampCursor`;
`compile_gain_ramps` builds the table on the control thread.

**Files:**
- Modify: `src-tauri/src/audio/rt.rs` — `RtGraph::gain_ramps` +
  `set_gain_ramps`
- Modify: `src-tauri/src/audio/mixer.rs` — `render_impl` and `render_live`
  apply the ramp
- Modify: `src-tauri/src/plugins/automation.rs` — `compile_gain_ramps`
- Test: `src-tauri/src/audio/mixer.rs` tests + `plugins/automation.rs` tests

**Interfaces:**
- Consumes: `RampCursor`, `resolve_target`, `LaneTarget`, `compile_lane`,
  `AbsParamEvent` (Task 7); `frame_pos(base, i, lp)`
  (`src-tauri/src/audio/transport.rs:40`).
- Produces:
  ```rust
  // audio/rt.rs
  pub struct RtGraph {
      /* … existing fields … */
      /// This snapshot's compiled track-gain automation, indexed BY SLOT
      /// exactly like `ParamTable` (round-2 §2.4: per-graph, versioned with
      /// the snapshot, so a retired graph keeps reading its own). Empty =
      /// no automation, which is what `new` leaves it as — every existing
      /// construction site is unchanged.
      pub gain_ramps: Vec<Option<Arc<Vec<crate::plugins::automation::AbsParamEvent>>>>,
  }
  impl RtGraph {
      /// Attach this rebuild's compiled gain ramps. Control thread only,
      /// BEFORE the graph is published (RCU discipline).
      pub fn set_gain_ramps(
          &mut self,
          ramps: Vec<Option<Arc<Vec<crate::plugins::automation::AbsParamEvent>>>>,
      );
  }

  // plugins/automation.rs
  /// Compile every TRACK-GAIN lane into a slot-indexed ramp table for one
  /// rebuild (CONTROL THREAD — ticks never cross onto the RT thread).
  /// `slot_of` resolves a track id to this rebuild's slot; lanes whose
  /// target doesn't resolve, whose track has no slot, or that have no
  /// points, are skipped.
  pub fn compile_gain_ramps(
      lanes: &[AutomationLane],
      map: &TempoMap,
      n_slots: usize,
      slot_of: &dyn Fn(&str) -> Option<usize>,
  ) -> Vec<Option<Arc<Vec<AbsParamEvent>>>>;
  ```

- [ ] **Step 1: Write the failing tests.**

  In `plugins/automation.rs`'s test module:

```rust
    #[test]
    fn compile_gain_ramps_indexes_by_slot_and_skips_what_cannot_resolve() {
        let map = TempoMap::from_v1(120.0, 48_000).unwrap();
        let mut a = lane(vec![
            AutomationPoint { tick: 0, value: 1.0 },
            AutomationPoint { tick: 3840, value: 0.0 },
        ]);
        a.target_node = "track:t-1".into();
        let mut orphan = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        orphan.target_node = "track:t-gone".into();
        let mut plugin_lane = lane(vec![AutomationPoint { tick: 0, value: 0.5 }]);
        plugin_lane.target_node = "inst-1".into();
        let mut empty = lane(vec![]);
        empty.target_node = "track:t-2".into();

        let slot_of = |id: &str| match id {
            "t-1" => Some(0usize),
            "t-2" => Some(1),
            _ => None,
        };
        let ramps = compile_gain_ramps(&[a, orphan, plugin_lane, empty], &map, 2, &slot_of);
        assert_eq!(ramps.len(), 2);
        let r0 = ramps[0].as_ref().expect("t-1 has a compiled ramp");
        assert_eq!(r0[0], AbsParamEvent { sample: 0, value: 1.0 });
        assert_eq!(r0[1], AbsParamEvent { sample: 96_000, value: 0.0 });
        assert!(ramps[1].is_none(), "an empty lane compiles to no ramp");
    }
```

  In `mixer.rs`'s test module (the fixtures `live_track` at `:608` and the
  clip-track pattern at `:777` are the ones to copy):

```rust
    /// Track D's audibility proof at the MIXER seam (scope ruling 1: the
    /// ramp attaches at the per-track gain stage, so it scales CLIP audio
    /// and LIVE audio alike — `GainAutomatedNode` could only ever have done
    /// the latter). Offline render, amplitude asserted against the lane.
    #[test]
    fn track_gain_ramp_scales_clip_output_sample_accurately() {
        use crate::plugins::automation::AbsParamEvent;
        // A DC-1.0 mono clip so the applied gain is directly readable.
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; 4096] });
        let clip = RtClip {
            start: 0, offset: 0, len: 4096, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        g.params.set_pan(0, -1.0); // hard left: channel 0 carries unity
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 1000, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev.clone())]);

        let mut out = vec![0.0f32; 1024 * 2];
        mixer::render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            let want = crate::plugins::automation::value_at(&ev, i as u64).unwrap();
            assert!(
                (frame[0] - want).abs() < 1e-4,
                "sample {i}: got {} want {want}",
                frame[0]
            );
        }
    }

    /// The cursor must re-seed on a BACKWARD position jump: one callback
    /// block crossing a loop end renders the tail of the ramp and then the
    /// loop start's value, mid-block.
    #[test]
    fn track_gain_ramp_re_seeds_across_a_loop_wrap() {
        use crate::plugins::automation::AbsParamEvent;
        let data = Arc::new(RtClipData { channels: 1, data: vec![1.0; 8192] });
        let clip = RtClip {
            start: 0, offset: 0, len: 8192, gain: 1.0,
            fade_in: 0, fade_out: 0, samples: data,
        };
        let mut g = RtGraph::new(
            vec![RtTrack::clips(0, vec![clip])],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        g.params.set_pan(0, -1.0);
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 1.0 },
            AbsParamEvent { sample: 2000, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev.clone())]);

        let lp = LoopSpec { enabled: true, start: 500, end: 2000 };
        let mut out = vec![0.0f32; 1024 * 2];
        mixer::render(&mut g, 1_744, &lp, &mut out, 2, 48_000, true, None);
        for (i, frame) in out.chunks_exact(2).enumerate() {
            let pos = crate::audio::transport::frame_pos(1_744, i as u64, &lp);
            let want = crate::plugins::automation::value_at(&ev, pos).unwrap();
            assert!(
                (frame[0] - want).abs() < 1e-4,
                "frame {i} (pos {pos}): got {} want {want}",
                frame[0]
            );
        }
    }

    /// A LIVE (instrument) track's output goes through the same gain stage,
    /// so one ramp covers both source kinds.
    #[test]
    fn track_gain_ramp_scales_live_output_too() {
        use crate::plugins::automation::AbsParamEvent;
        let mut g = RtGraph::new(
            vec![live_track(0, vec![], 48_000)],
            1,
            Arc::new(ParamTable::with_slots(1)),
        );
        let ev = Arc::new(vec![
            AbsParamEvent { sample: 0, value: 0.0 },
            AbsParamEvent { sample: 512, value: 0.0 },
        ]);
        g.set_gain_ramps(vec![Some(ev)]);
        let mut out = vec![0.0f32; 512 * 2];
        mixer::render(&mut g, 0, &LoopSpec::OFF, &mut out, 2, 48_000, false, None);
        assert!(
            out.iter().all(|s| s.abs() < 1e-6),
            "a lane pinned at 0.0 silences the live node's contribution"
        );
    }
```

  If `live_track`'s fixture renders silence with no note events anyway, feed
  it one `AbsNoteEvent { sample: 0, key: 69, velocity: 110 }` and assert the
  ramped output against an unramped control render instead — the assertion
  that matters is "the ramp scales the live path", not the exact fixture.

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- track_gain_ramp compile_gain_ramps`
  Expected: FAIL to compile (`set_gain_ramps`, `compile_gain_ramps` absent).

- [ ] **Step 3: Add the field and setter** in `audio/rt.rs`:

```rust
    /// This snapshot's compiled track-gain automation, indexed BY SLOT —
    /// exactly like `ParamTable` (round-2 §2.4: per-graph, versioned with
    /// the snapshot, so a retired graph keeps reading its own). `None` at a
    /// slot means "no lane"; an EMPTY vec means "no automation at all",
    /// which is what `new` leaves behind so every existing construction
    /// site is unchanged.
    ///
    /// Why here and not on the live node (Track D scope ruling 1): the
    /// registry reuses live nodes ACROSS rebuilds to keep voice and plugin
    /// state, so a ramp baked into a node could only change by discarding
    /// that state — and a node-side ramp could never reach an audio-clip
    /// track at all.
    pub gain_ramps: Vec<Option<Arc<Vec<crate::plugins::automation::AbsParamEvent>>>>,
```

  (The `Arc` wraps the whole event vector — one allocation per lane per
  rebuild, on the control thread, shared with the published snapshot.)
  Add `gain_ramps: Vec::new(),` to `RtGraph::new`'s struct literal, and:

```rust
    /// Attach this rebuild's compiled gain ramps (`engine::rebuild`'s one
    /// call). CONTROL THREAD, before the graph is published — after
    /// publication the snapshot is immutable, RCU-style.
    pub fn set_gain_ramps(
        &mut self,
        ramps: Vec<Option<Arc<Vec<crate::plugins::automation::AbsParamEvent>>>>,
    ) {
        self.gain_ramps = ramps;
    }
```

- [ ] **Step 4: Apply the ramp in `audio/mixer.rs`.** Add
  `use crate::plugins::automation::{AbsParamEvent, RampCursor};` at the top.
  In `render_impl`, extend the destructure and resolve the per-track ramp:

```rust
    let RtGraph { tracks, scratch, meter_scratch, gain_ramps, .. } = graph;
    let gain_ramps: &[Option<Arc<Vec<AbsParamEvent>>>] = gain_ramps;
```

  Inside the `for tr in tracks.iter()` loop, after the existing
  `gain`/`pan`/`flags` reads:

```rust
        // Track D: this snapshot's compiled gain automation for the slot.
        // RT-safe: a slice read + an index walk, no allocation, no locks.
        let ramp: &[AbsParamEvent] = gain_ramps
            .get(tr.slot)
            .and_then(|r| r.as_ref())
            .map(|a| a.as_slice())
            .unwrap_or(&[]);
        let mut clip_ramp = RampCursor::new();
```

  In the clip loop, replace `l *= gain * gl; r *= gain * gr;` with:

```rust
                let g = gain * clip_ramp.value(ramp, pos).unwrap_or(1.0);
                l *= g * gl;
                r *= g * gr;
```

  Pass `ramp` into `render_live` (one more parameter, after `mix`), and in
  `render_live`'s per-frame mix loop:

```rust
        // one cursor for the whole call; `pos` is this run's base, and runs
        // advance monotonically except across a loop wrap, which the cursor
        // re-seeds for.
        for i in 0..run {
            let g = gain * ramp_cursor.value(ramp, pos + i as u64).unwrap_or(1.0);
            let mut l = scratch[i * 2] * g * gl;
            let mut r = scratch[i * 2 + 1] * g * gr;
```
  with `let mut ramp_cursor = RampCursor::new();` declared once, before the
  `while f < frames` loop.

- [ ] **Step 5: Implement `compile_gain_ramps`** in `plugins/automation.rs`:

```rust
/// Compile every TRACK-GAIN lane into a slot-indexed ramp table for one
/// rebuild. CONTROL THREAD ONLY — this is where ticks become absolute
/// samples; nothing tick-shaped crosses onto the RT thread
/// (ARCHITECTURE §13/§15.1).
pub fn compile_gain_ramps(
    lanes: &[AutomationLane],
    map: &TempoMap,
    n_slots: usize,
    slot_of: &dyn Fn(&str) -> Option<usize>,
) -> Vec<Option<Arc<Vec<AbsParamEvent>>>> {
    let mut out: Vec<Option<Arc<Vec<AbsParamEvent>>>> = (0..n_slots).map(|_| None).collect();
    for lane in lanes {
        let Some(LaneTarget::TrackGain(track_id)) = resolve_target(lane) else { continue };
        let Some(slot) = slot_of(&track_id) else { continue };
        if slot >= out.len() {
            continue;
        }
        let events = compile_lane(lane, map);
        if events.is_empty() {
            continue;
        }
        out[slot] = Some(Arc::new(events));
    }
    out
}
```

- [ ] **Step 6: Run the backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 539 + 4 = **543**, green. `plugins/automation.rs`'s existing
  `GainAutomatedNode` tests must still pass untouched (ruling 1: the wrapper
  stays).

- [ ] **Step 7: Commit** —

```bash
git add src-tauri/src/audio/rt.rs src-tauri/src/audio/mixer.rs \
        src-tauri/src/plugins/automation.rs
git commit -m "feat(rt): slot-indexed track-gain automation ramps applied by the mixer"
```

---

### Task 9: THE ENGINE TASK — attach at rebuild, flip the REBUILD PIN, drive plugin params

**This is the only task in the plan that touches `engine.rs`. Read the
execution notes' cross-track sequencing ruling before starting it.** It is
deliberately last, deliberately small (~35 lines in `engine.rs`, all of them
calls into functions Tasks 7 and 8 already tested), and deliberately
self-contained so it rebases cleanly over Track A's rewrite of
`rebuild`'s session-read path.

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` — `Control` (+2 fields, both
  construction sites), `Control::rebuild`, `Control::run`, new
  `Control::drive_param_automation`
- Modify: `src-tauri/src/control/session.rs` — the `Op::AutomationSetLane`
  arm: flip `effect.rebuild = true` and rewrite the REBUILD PIN comment
- Test: `src-tauri/src/control/session.rs` tests + `src-tauri/src/audio/engine.rs` tests

**Interfaces:**
- Consumes: `compile_gain_ramps`, `RtGraph::set_gain_ramps` (Task 8);
  `ParamAutomationDriver::{new, empty, is_empty, tick}`, `ParamWrite`,
  `control::forward_param_to_host` (Task 7); `TempoMap::new(ppq, events, rate)`;
  `SharedRt::{playing, position}` (`audio/rt.rs:31-32`); the existing
  `bare_control()` headless test fixture (`engine.rs:1743`).
- Produces: nothing later tasks consume — this is the leaf.

- [ ] **Step 1: Write the failing tests.**

  In `control/session.rs`'s test module:

```rust
/// THE REBUILD PIN (Track D). Until the engine read `session.automation`,
/// `Op::AutomationSetLane` deliberately set no engine effect — the arm's own
/// comment named the day this would have to change. That day is this task's.
#[test]
fn automation_set_lane_schedules_a_rebuild() {
    use crate::plugins::automation::{AutomationLane, AutomationPoint};
    let m = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    let c = Session::transact(&m, TxMeta::user("edit automation"), |tx| {
        tx.apply(Op::AutomationSetLane {
            key: "lane-a".into(),
            lane: Some(AutomationLane {
                id: "lane-a".into(),
                target_node: "track:t-1".into(),
                param_id: 0,
                points: vec![AutomationPoint { tick: 0, value: 1.0 }],
            }),
        })
    })
    .unwrap();
    assert!(c.effect.rebuild, "the engine reads session.automation now — lanes must rebuild");
    assert!(c.effect.persist.automation, "and still persist");
}

/// Deleting a lane rebuilds too — the ramp must come OFF the graph.
#[test]
fn automation_lane_delete_schedules_a_rebuild() {
    let m = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    let c = Session::transact(&m, TxMeta::user("edit automation"), |tx| {
        tx.apply(Op::AutomationSetLane { key: "nope".into(), lane: None })
    })
    .unwrap();
    assert!(c.effect.rebuild);
}
```

  (Copy the exact `Session::transact` fixture shape from the neighbouring
  tests in that module — e.g. `plugin_set_param_is_coalescable_and_persists_via_effect`
  at `:2495` — rather than assuming the constructors above.)

  In `engine.rs`'s test module, using `bare_control()`:

```rust
    /// Track D: a rebuild compiles the session's plugin-param lanes into the
    /// control-thread driver. Headless (`bare_control`) builds no graph, so
    /// the driver is the observable half of the attach here; the graph half
    /// is covered at the mixer seam (`audio::mixer`'s
    /// `track_gain_ramp_*` tests) and by the ear check in this task's step 6.
    #[test]
    fn rebuild_compiles_plugin_param_lanes_into_the_driver() {
        use crate::plugins::automation::{AutomationLane, AutomationPoint};
        let (mut ctl, session) = bare_control();
        ctl.cache_rate = 48_000; // headless has no device; the tempo map needs a rate
        {
            let mut s = session.lock();
            s.plugins.instances.push(crate::plugins::PluginInstanceInfo {
                id: "inst-1".into(), uid: "lv2:urn:test:synth".into(),
                name: "TestSynth".into(), format: "lv2".into(), status: "active".into(),
            });
            s.automation.lanes.push(AutomationLane {
                id: "l1".into(),
                target_node: "inst-1".into(),
                param_id: 7,
                points: vec![
                    AutomationPoint { tick: 0, value: 0.0 },
                    AutomationPoint { tick: 3840, value: 1.0 },
                ],
            });
        }
        assert!(ctl.param_automation.is_empty(), "nothing compiled before the rebuild");
        ctl.rebuild();
        assert!(!ctl.param_automation.is_empty(), "the rebuild compiled the lane");

        // and a rebuild AFTER the lane is gone drops it again
        session.lock().automation.lanes.clear();
        ctl.rebuild();
        assert!(ctl.param_automation.is_empty());
    }
```

- [ ] **Step 2: Run to verify they fail** —
  `timeout 300 cargo test --manifest-path src-tauri/Cargo.toml --lib -- automation_set_lane_schedules automation_lane_delete_schedules rebuild_compiles_plugin_param`
  Expected: FAIL (`effect.rebuild` is false; `param_automation` does not exist).

- [ ] **Step 3: Flip the REBUILD PIN** in `control/session.rs`. Replace the
  eleven-line "REBUILD PIN" comment block above `Op::AutomationSetLane` with:

```rust
        // REBUILD PIN — RESOLVED (Track D, automation audible). Until this
        // task, `engine::rebuild` never read `session.automation`, so a lane
        // edit had no RT-visible effect and this arm deliberately set no
        // `effect.rebuild`. It does now: `rebuild` compiles track-gain lanes
        // into `RtGraph::gain_ramps` (slot-indexed, versioned with the
        // snapshot) and plugin-param lanes into the control thread's
        // `ParamAutomationDriver`. Both are rebuilt WHOLESALE from the
        // session at every rebuild, so an upsert AND a delete must schedule
        // one — a deleted lane's ramp comes off the graph only by rebuilding
        // without it.
```

  and add, next to `effect.persist.automation = true;`:

```rust
            effect.rebuild = true;
```

- [ ] **Step 4: Wire the engine.** In `engine.rs`:

  (a) Two new fields on `struct Control`, documented:

```rust
    /// Track D: this rebuild's compiled plugin-parameter lanes. Ticked by
    /// `run` (control thread, ≤2 ms), never by the audio callback — a host
    /// param write is a blocking round-trip. Rebuilt wholesale at every
    /// `rebuild`, like the graph itself.
    param_automation: crate::plugins::automation::ParamAutomationDriver,
    /// Reused scratch for `param_automation.tick` so the tick allocates
    /// nothing steady-state.
    param_writes: Vec<crate::plugins::automation::ParamWrite>,
```
  Add `param_automation: crate::plugins::automation::ParamAutomationDriver::empty(),`
  and `param_writes: Vec::new(),` to BOTH `Control` struct literals
  (`start()`'s, and `bare_control()`'s in the test module).

  (b) In `rebuild`, change `let graph = { … };` to yield the driver too. The
  compile happens inside the existing session-lock block (it is a pure read
  of `session.automation`/`session.midi`/`session.plugins`); the assignment
  to `self` happens after the block, because `self.session.lock()` borrows
  `self`:

```rust
        let (graph, param_driver) = {
            let session = self.session.lock(); // read-only: … (unchanged)
            /* … existing slots/params/tables/gen_maps publish, unchanged … */

            // Track D: compile the automation the session carries into this
            // rebuild's products. CONTROL THREAD — ticks become absolute
            // samples here and nothing tick-shaped crosses onto the RT
            // thread. A tempo map needs a rate; headless (rate 0) yields
            // None and simply attaches nothing.
            let map = crate::midi::TempoMap::new(
                session.midi.ppq,
                session.midi.tempo_events.clone(),
                self.cache_rate,
            )
            .ok();
            let gain_ramps = map.as_ref().map(|m| {
                crate::plugins::automation::compile_gain_ramps(
                    &session.automation.lanes,
                    m,
                    store.tracks.len(),
                    &|tid| slots.iter().find(|(id, _)| id.as_str() == tid).map(|(_, s)| *s),
                )
            });
            let param_driver = match map.as_ref() {
                Some(m) => crate::plugins::automation::ParamAutomationDriver::new(
                    &session.automation.lanes,
                    &session.plugins,
                    m,
                ),
                None => crate::plugins::automation::ParamAutomationDriver::empty(),
            };

            let graph = if headless {
                /* … unchanged … */
                None
            } else {
                /* … unchanged track assembly, append_from, song_end … */
                let mut g = RtGraph::new(tracks, self.generation, params);
                if let Some(ramps) = gain_ramps {
                    g.set_gain_ramps(ramps);
                }
                Some(Box::new(g))
            };
            (graph, param_driver)
        };
        self.param_automation = param_driver;
        let Some(graph) = graph else { return };
```

  (c) In `run`, after `self.drain_rt_events();`:

```rust
            self.drive_param_automation();
```

  (d) The driver itself, next to `rebuild`:

```rust
    /// Track D: apply plugin-parameter automation at this thread's own tick
    /// (≤2 ms), never on the audio callback — a host param write is a
    /// blocking round-trip and is banned there ([C1]).
    ///
    /// The writes go to the HOST ONLY, never to the document. That is the
    /// point, not an omission: automation OVERRIDES the stored knob value
    /// during playback, while the document keeps what the user set (which is
    /// what gets saved and what the param panel shows). Routing these
    /// through the channel would either trip the M-3 transient invariant
    /// (`ObjectRef::Plugin` is a field history entries address) or push an
    /// undo entry and a `project.json` write every 2 ms. Recorded in
    /// `docs/SIDE-CHANNEL-INVENTORY.md`.
    ///
    /// Only while the transport is playing: a stopped transport leaves the
    /// last automated value in place, which is what the user sees and hears
    /// until they move the knob or reload the project.
    fn drive_param_automation(&mut self) {
        if self.param_automation.is_empty() || !self.shared.playing.load(Relaxed) {
            return;
        }
        let pos = self.shared.position.load(Relaxed);
        let mut writes = std::mem::take(&mut self.param_writes);
        self.param_automation.tick(pos, &mut writes);
        for w in writes.iter() {
            crate::control::forward_param_to_host(&w.instance, &w.format, w.index, w.value);
        }
        self.param_writes = writes;
    }
```

- [ ] **Step 5: Run the whole backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 543 + 3 = **546**, green. Watch specifically for tests that
  counted rebuilds around automation commits — the pin flip adds one rebuild
  per lane edit by design; if a test asserts "no rebuild" for an automation
  op, that test encodes the OLD pin and must be updated (with a comment
  citing this task), not the behaviour.

- [ ] **Step 6: Verify by ear** — this is the task's real acceptance test and
  the one the suite cannot give you (headless `Control` builds no graph, so
  there is no in-suite observation point for the `set_gain_ramps` wiring
  line itself; both halves of it are unit-tested in Task 8).
  Use the `run-aura` skill to launch the app. Load the demo song, open a
  track's automation lane (`A` in the track header), draw a curve from top
  left to bottom right across a bar, and press play: the track must fade out
  across that bar and be silent at the end of it. Undo (Ctrl+Z): the curve
  and the level both come back. Record the observation in the SDD ledger.

- [ ] **Step 7: Commit** —

```bash
git add src-tauri/src/audio/engine.rs src-tauri/src/control/session.rs
git commit -m "feat(engine): automation is audible — rebuild attaches gain ramps and drives plugin params (REBUILD PIN resolved)"
```

---

### Task 10: Close-out — inventory, handoff, next-prompt, dated counts

**Files:**
- Modify: `docs/SIDE-CHANNEL-INVENTORY.md` — the host-only automation drive
  note
- Modify: `docs/PHASE4-PLAN.md` — a new "Track D handoff" section
- Modify: `next-prompt.md` — Track D's entry and the held-findings list
- Modify: `README.md:389`, `CONTRIBUTING.md:62` — dated counts

- [ ] **Step 1: Record the host-only automation drive** in
  `docs/SIDE-CHANNEL-INVENTORY.md`, under "Verified non-writers" (it is not a
  residual — it writes no document field at all — but it IS a new host-write
  site outside `execute_host_forward`, and the grep gate's readers need to
  know it exists):

```markdown
* **`engine::Control::drive_param_automation`** (Track D) — writes plugin
  PARAMS ON THE HOST, at the engine control thread's ≤2 ms tick, and writes
  NO document field. Deliberate: automation overrides the stored knob value
  during playback while the document keeps what the user set. Consequence,
  recorded so it is not mistaken for a bug: after playing an automated
  section the plugin's live value can differ from the document's stored
  value until the next project load or user edit; the document is
  authoritative on save, and the param panel shows the document. Making the
  panel follow automation live is deferred.
```

- [ ] **Step 2: Write the "Track D handoff" section** in
  `docs/PHASE4-PLAN.md`, appended after the "Plan E handoff" section, same
  conventions: all eight scope rulings verbatim, the non-goals, the
  findings closed (I-3 + M-6 as R-4, I-8 with row 13 corrected, frontend
  M-3), the deferrals created (sample-accurate plugin params; fader-follows
  automation; a dedicated resizable automation row; live panel follow), and
  the new dated test counts.

- [ ] **Step 3: Update `next-prompt.md`** in the same commit (the standing
  convention): rewrite the "Track D" entry in §3 to landed form with a
  pointer to this plan and the handoff section, and strike I-3, I-8 and the
  frontend M-3 from the "Still open, deliberately HELD" list in the
  post-merge findings section, each with a one-line "closed by Track D
  (<commit>)" note. Also correct that section's baseline line (it says 506
  backend; the real number at branch base was 527, and after this plan it is
  546).

- [ ] **Step 4: Update the dated counts.** `README.md:389` and
  `CONTRIBUTING.md:62` both currently read `506 tests (counted 2026-08-14)`,
  which was already stale at branch base. Set them to the verified
  post-plan numbers with today's date, and `README.md:390`'s frontend line
  likewise (206 → the post-plan count).

- [ ] **Step 5: Full suites green one last time** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Record the exact numbers; they are what the
  doc lines must say.

- [ ] **Step 6: Commit** —

```bash
git add docs/SIDE-CHANNEL-INVENTORY.md docs/PHASE4-PLAN.md next-prompt.md \
        README.md CONTRIBUTING.md
git commit -m "docs: Track D close-out — automation handoff, inventory note, dated counts"
```

---

## Self-review notes (run against the spec before Task 1 starts)

1. **Spec coverage.** `docs/backlog/automation-audible-and-ui.md`'s three
   items: **(1) RT attach** → Tasks 8 (mechanism, tested) + 9 (wiring + the
   REBUILD PIN flip the backlog explicitly demands, plus the audibility check
   the backlog asks for — landed as a mixer-seam render test in Task 8 rather
   than an engine-level one, because headless `Control` builds no graph);
   **(2) UI** → Tasks 5 (store/IPC) + 6 (lane, draw/drag/delete,
   gesture-wrapped, whole-lane replaces through `automation_set`) + 7 (target
   picker for plugin params); **(3) playback semantics** → Task 8's
   `RampCursor` tests verify the compile contract against the engine's block
   loop, specifically the loop-wrap backward jump `render_live`'s existing
   seam already handles for `GainAutomatedNode`. The backlog's suggested cut
   is followed, with the order inverted per the cross-track ordering ruling
   (its cut 1 is this plan's Tasks 8+9, last).
   `next-prompt.md` Track D's four scope bullets: RT attach (8/9), timeline
   lane UI (5/6), plugin-parameter targets (7), and the three orchestrator
   rulings — I-3 + M-6 (Task 1), I-8 (Tasks 2+3), frontend M-3 (Task 5).
   Audio stays addressing-only: no task touches content/placement.
2. **Type consistency.** `PersistEffect::merge` (Task 2) is used by
   `fold_committed` in the same task. `commit_transient_for_gesture` (Task 2)
   is consumed by Tasks 3 and 4 under that exact name.
   `CoalesceTarget`/`CoalesceKey.target` (Task 4) is the only shape change to
   a `pub(crate)` type; `history.rs` compares whole keys and needs no edit.
   `RampCursor::value -> Option<f32>` (Task 7) is what Task 8's mixer calls
   with `.unwrap_or(1.0)`. `compile_gain_ramps`'s return type
   `Vec<Option<Arc<Vec<AbsParamEvent>>>>` is exactly `RtGraph::gain_ramps`'s
   type and exactly what `set_gain_ramps` takes (Task 8), and exactly what
   Task 9 passes. `ParamAutomationDriver::{new, empty, is_empty, tick}` and
   `ParamWrite`'s four fields (Task 7) are what Task 9's `Control` field and
   `drive_param_automation` use. `forward_param_to_host(instance, format,
   index, value)` is defined in Task 7 and called in Task 9.
   `automation.reload()`/`plugins.reloadOpenParams()` (Task 5) are what
   `projectops.repull()` calls in the same task. `TRACK_PARAM_GAIN` is 0 on
   both sides (`plugins/automation.rs` and `automation.svelte.ts`).
3. **Placeholder scan.** Three steps deliberately point at an in-repo fixture
   instead of inlining it: Task 1's `PluginInstanceInfo` literal (cites
   `plugins/mod.rs:376`'s `scanned_registry`), Task 2's `stored_value`
   accessor (cites `plugins/state.rs` for the on-disk shape, with the
   assertions fixed regardless), and Task 9's session-test fixture (cites
   `session.rs:2495`). Each names the exact file and symbol to copy, and each
   pins the assertions that must survive the copy. No "TBD", no "similar to
   Task N", no "add appropriate error handling" anywhere in the plan.
4. **Risks, named.** (a) Task 9 rebases over Track A — see the execution
   note; its `engine.rs` diff is ~35 lines, all of them calls into
   already-tested functions, precisely so the rebase is mechanical.
   (b) The pin flip (Task 9) adds a rebuild per lane edit; a lane drag is one
   commit (ruling 7) so a drag is one rebuild, but a fast repeated-click
   delete could rebuild several times — acceptable, and the same cost
   `set_track_instrument` already pays. (c) Task 2 changes when persistence
   happens for every gesture-folding caller; a drag that never receives its
   `pointerup` (webview reload mid-drag) now loses the drag's persist as well
   as its history entry — the gesture auto-close in `gesture_begin` covers
   the common case, and the loss is one drag, recorded. (d) `Timeline.svelte`
   and `TrackHeader.svelte` are also Track C's surface (multi-clip
   selection); Task 6's edits are additive (one `{#if}` block, one button)
   and should merge cleanly, but sequence with Track C if it is in flight.
   (e) Test-count arithmetic in this plan is cumulative and therefore
   fragile; re-verify at each task boundary rather than trusting the number.
5. **Ordering sanity.** 1 (independent, closes a held finding) → 2 (the
   deferral mechanism) → 3, 4 (both consume it) → 5 (frontend foundation +
   M-3) → 6 (lane UI on top of 5) → 7 (plugin targets; extracts
   `forward_param_to_host`) → 8 (RT plumbing; consumes 7's `RampCursor`) → 9
   (engine, last, consumes 7 and 8) → 10 (paperwork). Tasks 1-8 touch
   **zero** lines of `engine.rs`.

## Execution notes

**CROSS-TRACK SEQUENCING (orchestrator ruling — binding).** Track A
(parallel worktree) is rewriting `engine::rebuild`'s session-read path, and
Track B (MIDI slice 2) also touches `engine.rs`. **Only ONE track's
`engine.rs`-touching task may be in flight at a time, and Track A goes
first.** That is why every non-`engine.rs` item in this plan — the lane UI,
the gesture wrapping of `automation_set`, I-3, the I-8 gesture extension, the
frontend re-pull, and all of the RT plumbing — comes first, and why Task 9 is
last, minimal, and self-contained.

Before starting **Task 9**: check whether Track A's `rebuild` change has
landed on `origin/main`. If it has, merge `origin/main` into this branch at
the Task 8/9 boundary (the standing rule: continuing branches merge
`origin/main` in at a task boundary) and re-run both suites before writing a
line of Task 9. **Task 9 may need a rebase over Track A's landed rebuild
change** — its edits are (i) two struct fields on `Control` at two
construction sites, (ii) turning `let graph = { … }` into
`let (graph, param_driver) = { … }` plus two `let` bindings and one
`set_gain_ramps` call inside the existing session-read block, and (iii) one
call added to `run`. If Track A has replaced the session-lock block with an
immutable-snapshot read, the compile step moves onto that snapshot verbatim —
it only ever reads `session.automation`, `session.midi` and `session.plugins`.
If Track A has NOT landed, do not wait: land Task 9 and tell the orchestrator,
so Track A rebases over it instead.

Worktree: `/home/knobo/prog/dav/.claude/worktrees/track-d-automation`, branch
`automation-audible`, cut from `origin/main` at `3340aa8`. Work only there.
Never bare `git stash`. Push to a PR when Task 1 lands green; push at every
task boundary after. Foreground `timeout`-guarded test runs only. SDD ledger
(gitignored): `.superpowers/sdd/track-d-automation/progress.md`. Execution
mode (solo vs subagent-driven) is the owner's call at run start — recorded
here once made: **[filled in at execution]**.
