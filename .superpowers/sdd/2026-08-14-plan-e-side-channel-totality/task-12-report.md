# Task 12: Transport family as transient ops + device-selection attribution

## Status: DONE

Branch `task-12-transport`, based on `c95368c`.

## Summary

`ControlPlane::transport`'s four direct `session.store.transport.*` writes
(state="playing", state="stopped", the loop mirror, stop_at_end) now go
through the op system as one transient `commit_with` per `TransportAction`.
Device selection (`select_input_device`/`select_output_device`) moved behind
new `ControlPlane` methods that log the actor/label attribution before
sending the existing `ControlMsg`s (no `Op` — app-config carve-out, §4.5).

## Files changed

- `src-tauri/src/control/op.rs`
  - `ObjectRef::Transport` — unit variant. Wire form `{"family":"transport"}`
    (adjacently-tagged enum omits `id` for a unit variant).
  - `PropPath::TransportState/LoopEnabled/LoopStartSamples/LoopEndSamples/StopAtEnd/SampleRate`.
  - Extended `op_serde_round_trips_and_carries_version` with the new wire
    forms (`ObjectRef::Transport`, `Set{Transport,TransportState}`, and each
    new `PropPath`'s camelCase name).
- `src-tauri/src/control/session.rs`
  - New `apply_raw` arm: `Op::Set { object: ObjectRef::Transport, path, to, .. }`,
    reading/writing via new `read_transport_prop`/`write_transport_prop`
    helpers (mirroring `read_prop`/`write_prop` for `TrackState`). Sets
    **no** `effect.rebuild` and **no** `effect.persist.*` — RULING (Task 12
    brief): these fields live in project.json but persistence rides
    explicit saves only, never every play/stop/loop-drag. Documented inline
    as a deliberate divergence from the Clip/MidiClip Set arms above it.
  - `TransportState` validated against the closed
    `"playing"|"stopped"|"recording"` set on write.
  - Extended the Track/MidiClip `read_*`/`write_*` catch-all `Err` arms with
    the six new `PropPath` variants (mismatch → `Err`, never panic — the
    Task-3-fix pattern, both directions).
  - 6 new tests: transport state/loop/stop-at-end/sample-rate round-trip
    through their inverse, `Set{Transport,Gain}` errs,
    `Set{Track,LoopEnabled}` errs, and an unrecognized `TransportState`
    string errs.
- `src-tauri/src/control/mod.rs`
  - `commit_with(meta, f, emit_project_changed: bool)` — new. `commit`
    delegates to it with `true`. `emit_project_changed: false` skips only
    the final `project://changed` emit; param writes / rebuild / persist
    still run through the same pipeline.
  - `ControlPlane::transport` rewritten: each `TransportAction` arm that
    used to write the store directly now builds `TxMeta::user(label).transient()`
    + `Op::Set`(s) and calls `commit_with(..., false)`. RT atomics
    (`shared.loop_start`/`loop_end`/`loop_enabled`/`stop_at_end`/`playing`)
    stay in `transport`, executed **after** `commit_with` returns (comment
    citing lock-order [C1]) — a deliberate reordering for `SetLoop` (today
    atomics ran before the store mirror write; now after, uniformly with
    every other action, per the brief's explicit ruling).
    - `Play`: preserves the pre-existing guard — never downgrades an
      in-flight "recording" state to "playing"; no commit at all in that
      branch (rev doesn't move), matching prior behavior exactly.
    - `Stop`: commits the `TransportState="stopped"` Set **first**, then
      sends `ControlMsg::StopRecording` (if `shared.recording`) — reordered
      per the brief's §4.2 ruling. `StopRecording`'s own finalize write to
      `store.transport.state` (`audio/engine.rs`, Task 13's territory)
      still happens on the engine thread; both converge on "stopped"
      harmlessly.
    - `Seek`: untouched — pure RT atomic, not an op (position is engine
      state).
    - `SetLoop`: three `Op::Set`s (`LoopEnabled`/`LoopStartSamples`/
      `LoopEndSamples`) in one `commit_with` call → one `rev` bump.
    - `SetStopAtEnd`: one `Op::Set`.
  - `select_input_device`/`select_output_device` — new `ControlPlane`
    methods. Log `actor`/`label`/`device` via `log::info!`, then send the
    existing `ControlMsg::SelectInput`/`SelectOutput` (unchanged wire
    shape/behavior — restart-refused-while-recording semantics are the
    engine's, untouched).
  - 7 new tests: `commit_with(..., false)` bumps rev + threads `meta`
    (`transient` included) but never fires `project://changed`; `commit`
    still fires it (delegation check); transport play→stop bumps rev
    twice and never fires `project://changed`, `transport://state` exactly
    once per call; Play-while-recording issues no commit; `SetLoop` is one
    commit and still writes the RT atomics; an empty loop region is
    rejected before any commit; device selection sends the right
    `ControlMsg`s (via a background responder thread against the
    `EngineHandle::for_tests()` fake, since `request` blocks for a reply).
- `src-tauri/src/audio/mod.rs`
  - `select_input_device`/`select_output_device` Tauri commands now take
    `State<'_, Arc<ControlPlane>>` (were `State<'_, AudioState>`) and
    delegate to the new `ControlPlane` methods with
    `TxMeta::user("select input device"/"select output device")` — same
    pattern `add_track`/`set_track_instrument` already use.

## Test summary

`timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` — **fully
green**: 408 lib tests (baseline 395 + 13 new: 6 in `session.rs`, 7 in
`control::mod`), 11 integration tests, 0 failures. Also verified
`cargo build --lib` and `cargo test --lib --no-run` produce zero warnings.

## Self-review checklist (per task instructions)

- RT atomics ordering vs today: preserved for `Play`/`Stop`/`SetStopAtEnd`
  (atomics were already after the store write in those arms); **changed**
  for `SetLoop` specifically — atomics now run after the document commit
  (today they ran before). This is the brief's explicit, binding ruling
  ("keep it simple and honest... the transport atomics stay in
  `ControlPlane::transport` after `commit` returns"), not an oversight.
- No persist flags on transport Sets: confirmed in `session.rs`'s
  `Op::Set{Transport,...}` arm (no `effect.persist.*` write) and pinned by
  `transport_loop_set_round_trips_through_inverse`'s
  `assert_eq!(c.effect.persist, PersistEffect::default(), ...)`.
  Documented inline as a deliberate divergence from the generic
  Clip/MidiClip Set pattern.
- Transient flag on every transport meta: all four `TxMeta::user(...).transient()`
  call sites in `transport()` (`Play`, `Stop`, `SetLoop`, `SetStopAtEnd`).
- Device commands attributed: both Tauri commands now build a `TxMeta` and
  call through `ControlPlane`; the methods log actor+label before sending.
- Mismatches err: `Set{Transport,Gain}` and `Set{Track,LoopEnabled}` both
  tested, both `Err`, no panic — same catch-all-`Err` pattern as every
  other object/path pairing in `session.rs`.
- Pristine output: `cargo build --lib` and `cargo test --lib --no-run` show
  zero warnings.

## Concerns / notes for the controller

- `TransportAction::Stop`'s reordering (Set-commits-before-StopRecording)
  is a real behavior change from today, explicitly directed by the brief.
  It's benign in the current code (both writers converge on `"stopped"`),
  but worth a second look once Task 13 (StopRecording's own finalize
  transaction) lands, since that task owns the other half of this
  interaction.
- Per the "parallel-work note," edits were kept scoped to `transport()`,
  the two new device methods, `commit`/`commit_with`, and the op.rs/session.rs
  arms; nothing else in `control/mod.rs` (epoch fns, midi wrappers) was
  touched.

## Fix round 1 (review findings)

Commit `fix(transport): Play guard atomic under the tx lock; Stop keeps
pre-change ordering`, on top of `54a4ae1`.

**FINDING 1 (Play TOCTOU).** The original `Play` arm read
`session.store.transport.state == "recording"` under one `self.session.lock()`,
dropped it, then `commit_with` re-acquired the lock separately — a window
where a concurrent recording-start (engine control thread) could land
between the read and the commit, letting Play's "playing" Set stomp the
just-set "recording" state (exactly the downgrade the guard exists to
prevent).

Fix: added `pub fn store(&self) -> &Store` on `Tx` in `session.rs`, placed
right after `Tx::apply` (per the controller's exact-shape instruction, for
clean dedup with a parallel task adding the identical accessor). The Play
guard now runs *inside* the `commit_with` closure via `tx.store().transport.state`
— checked and (conditionally) written under the single session lock the
transaction already holds. When already "recording", the closure applies
nothing and returns `Ok(())`: a legal empty transient commit.
`Session::transact` bumps `rev` unconditionally regardless of whether any
op was applied, so this still bumps `rev` by one (unlike the old branch,
which issued no commit and left `rev` untouched) — the VALUE guarantee
("recording" survives) is preserved; a phantom no-op revision is the
honest cost of doing the check atomically instead of via a stale outer
read.

Extended `control::tests::transport_play_does_not_downgrade_recording_state`:
still sets `state = "recording"` via a direct store write in the fixture
(exercising the guard through the same shape a racing engine-thread write
would take) then calls Play, asserting `snap.state == "recording"` and
now `rev == rev_before + 1` (was `rev_before`, no-commit) to match the
empty-commit semantics above.

**FINDING 2 (Stop ordering).** Fix round 1's dispatch was
self-contradictory — it said "commit the Set first, then send
StopRecording" while also saying "mirror today's ordering." The pre-Task-12
code sent `ControlMsg::StopRecording` FIRST (which can block up to 15s —
`writer.finish`'s timeout, `audio/engine.rs`) and only wrote `"stopped"`
afterward, so any reader mid-finalize still saw `"recording"`. My original
Task 12 change reordered this so `"stopped"` became visible immediately,
before the take had actually finished draining to disk — a real behavior
regression.

Fix: restored the original order in the `Stop` arm — `ControlMsg::StopRecording`
sent first (when `shared.recording` is set), *then* the `TransportState =
"stopped"` `commit_with`. The engine round-trip stays outside the
transaction either way (§4.2 is satisfied regardless of ordering). Updated
the inline comment to state the rationale (visibility during finalize,
not just "restored ordering") and kept the note that Task 13 — which
turns the engine's own finalize write into its own `Actor::Engine`
transaction — revisits this interaction.

**Minor (Play/Stop comment precision).** Rewrote the doc comment above
`ControlPlane::transport` to distinguish the two guarantees explicitly:
`Play`'s guard preserves both VALUE and ATOMICITY (fixed via `tx.store()`
under one lock); `Stop`'s reordering preserves visibility/ORDERING, not
atomicity (the engine round-trip is a separate, non-transactional call).

### Fix-round verification

- `cargo build --lib`: clean, zero warnings.
- Covering tests re-run: `cargo test --lib control::` — 102 passed, 0
  failed (includes `transport_play_does_not_downgrade_recording_state`,
  `transport_play_then_stop_bumps_rev_twice_and_never_emits_project_changed`,
  `transport_set_loop_is_one_commit_and_writes_rt_atomics`,
  `transport_set_loop_empty_region_rejected_without_committing`,
  `select_input_and_output_device_send_the_existing_control_msg`,
  `commit_with_false_bumps_rev_and_meta_but_skips_project_changed`,
  `commit_still_fires_project_changed_via_commit_with_delegation`, plus
  every `session::tests::transport_*`/`mismatched_*transport*` test).
- Full suite, foreground: `timeout 900 cargo test --manifest-path
  src-tauri/Cargo.toml` — 408 lib tests + 11 integration tests, 0
  failures (same counts as fix round 0; no new tests added this round,
  only two existing ones edited).
