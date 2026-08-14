# Plan E — The Side-Channel Totality Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:subagent-driven-development` (recommended — this is Plan E's
> default per PHASE4-PLAN) or `superpowers:executing-plans` to implement this
> plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> Execution mode for this run is decided by the owner at execution time and
> recorded in the execution note at the bottom of this file.

**Goal:** Route every document mutation in the product through
`Session::transact` — all bypassing command families, all five sidecar
completion sinks, the engine control thread's five write sites, the LoopJam
watcher, plugin auto-persist, and the frontend-only audio clip move — make
the three mutating readers pure, fold the remaining two document stores
(automation, plugin rows) into `Session`, give gestures their begin/end IPC,
and then — Gate E's exit — turn the op log on: `journal.ndjson` + undo/redo,
with the Figma invariant (test 4) green.

**Architecture:** The existing channel machinery (`Session::transact`,
`ControlPlane::commit`, `EngineEffect`, `fold_ops`) is extended, not
replaced. The op vocabulary grows from 3 kinds to 12; `PropPath` grows
per-family variants (one Rust module keeps declaring every path — §4.6's
anti-drift rule); persistence becomes an *effect* (`PersistEffect`) executed
after the lock from a snapshot, which is what dismantles
`with_synced_store`'s I/O-under-lock. Epoch boundaries (open/create/save-as/
ensure-project) are the two round-2 carve-outs made sanctioned: they swap or
mark the document *outside* the op stream and eagerly adopt midi/plugins/
automation so no read path ever needs a lazy resync again. The engine
control thread gets a `Committer` (the commit core minus the self-referential
engine handle) and stops holding the session for writes. History (in-memory
undo/redo) and the journal writer land last, behind the Figma test.

**Tech Stack:** Rust (src-tauri crate: serde, serde_json, parking_lot,
proptest), TypeScript/Svelte 5 frontend (`src/lib/`), vitest.

**Spec:** `docs/CORE-REDESIGN-ROUND-2.md` §4.5 (the migration + inventory) +
§4.2 (engine submits, never holds) + §4.3 (two tiers, anti-nesting) + §4.4
(gestures/effects/inverses) + §4.6 (op log as persisted format) + §7 tests
4/5; ADR 0003 (mutation channel; log dark until THIS gate) + ADR 0006 (thin
renderer). Orchestration: `docs/PHASE4-PLAN.md` (sub-plan E row, Gate E, all
three handoff sections).

**Gate E (verbatim, PHASE4-PLAN):** *the side-channel inventory inside the
channel is total (checked against dossier 10 §2.3 + review additions, all
14+ paths); test 4 (Figma invariant — reads mutate nothing) passes; only
then does the op log/journal turn on. This gate is the round's whole point;
it does not get negotiated down.*

## Global Constraints

- **The op log stays dark until Task 17.** Tasks 1–16 add no journal file
  and no undo UI. Task 17 is the gate-closing task: it turns the log on
  *after* the Figma test passes in the same task, never before (ADR 0003).
- **Thin renderer** (ADR 0006): every frontend change in this plan is op
  emission, gesture emission, or deletion of frontend-authoritative state.
  No new time math, no new authoritative state client-side.
- **Frozen command/event names stay frozen; bodies become wrappers; new
  commands are additive** (PHASE4-PLAN rule 3). New additive commands in
  this plan: `move_clip`, `gesture_begin`, `gesture_end`, `undo`, `redo`.
- **Prepare-outside/commit-inside** (round-2 §4.4): unbounded work (file
  decode/copy, MIDI file parse, plugin host round-trips) happens before the
  transaction; the transaction is a short apply; persistence happens after
  the lock as a `PersistEffect`. **Blocking engine round-trips inside a
  transaction are banned** (§4.2).
- **`transact` closures must not panic** (no panic rollback until Plan F) —
  every closure validates before mutating, exactly as the A-slice does.
- **Op wire form:** new op kinds and new `PropPath`/`ObjectRef` variants are
  additive; typed ids keep serializing transparently. `OP_FORMAT_VERSION`
  stays **1** through this plan — the format becomes load-bearing at the
  moment Task 17 writes the first journal line; after that, any change is a
  version bump (§4.6).
- **Evidence policy** (ADR 0007): corrections marked, never silent. The
  scope rulings below and the op-envelope-schema correction (Task 17) are
  recorded in the eventual "Plan E handoff" section verbatim.
- **Foreground test runs only, `timeout`-guarded:**
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml` and
  `timeout 300 npx vitest run`. Baseline at plan start (verified
  2026-08-14, post-PR-#8 main): **384 backend + 100 frontend, all green.**
  (The C/D handoff's 372+85 predates PR #8's merge; the delta is PR #8's
  own tests. Recorded here so nobody chases a phantom regression.)
- **Lock order [C1] holds:** session before tables, never the reverse; the
  engine `Committer` (Task 13) obeys it identically.

## The re-derived inventory (what Gate E checks)

Re-derived against HEAD `7606d8d` (2026-08-14, four-agent sweep) from
dossier 10 §2.3 + the round-2 review's additions. This table IS the Gate E
checklist; Task 17 commits it (updated to "done" form) as
`docs/SIDE-CHANNEL-INVENTORY.md`. "Mechanism" is where the path ends up.

| # | Path (HEAD anchor) | Today | Mechanism | Task |
|---|---|---|---|---|
| 1 | `add_track`, `remove_track`, `set_track_*`/`set_track_mix` | already channel (Plan A/B) | — | done |
| 2 | `set_track_instrument` (audio/mod.rs:527) | direct write + Rebuild, no event | `Op::Set{Track, InstrumentId}` | 3 |
| 3 | audio clip move (frontend-only, project.svelte.ts:185; "worst offender": lost on save) | never reaches backend | `Op::Set{Clip, TimelineStartSamples}` + additive `move_clip` command | 3, 4 |
| 4 | `set_tempo_map` (midi/mod.rs:328; two non-atomic phases incl. `transport.tempo_bpm`) | bypass, I/O under lock | `Op::TempoSet` (one atomic apply incl. the bpm mirror) | 5, 7 |
| 5 | `midi_add_clip` (midi/mod.rs:350) | bypass | `Op::MidiClipAdd` | 5, 7 |
| 6 | `midi_set_notes` (midi/mod.rs:403; wholesale replace) | bypass | `Op::MidiSetNotes` (value-replacement wrapper, coalescable, §4.4) | 5, 7 |
| 7 | `midi_set_clip_bounds` (midi/mod.rs:505; new since dossier 10 — PR #8) | bypass | `Op::Set{MidiClip, TimelineStartTicks/LengthTicks/ContentLengthTicks}` | 5, 7 |
| 8 | `midi_import_file` (midi/mod.rs:533; 3 lock acquisitions, `ops::add_track` under lock) | bypass | prepare-outside parse → ONE composite transaction | 7 |
| 9 | mutating readers `midi_get_clips`/`midi_export_file` (midi/mod.rs:519/622 — lazy resync can re-instantiate live plugins) | §7-test-4 violation | pure reads; adopt happens eagerly at epochs only | 6 |
| 10 | mutating reader `automation_get` (plugins/automation.rs:588) | §7-test-4 violation | pure read | 10 |
| 11 | `automation_set` (plugins/automation.rs:599; own store, own persistence) | bypass | `AutomationStore` into Session + `Op::AutomationSetLane` | 10 |
| 12 | `plugin_instantiate`/`plugin_remove` (plugins/mod.rs:313/330) | bypass + auto-persist | registry doc-half into Session; `Op::PluginAdd`/`Op::PluginRemove` (restore-from-blob inverse, §4.4) | 9 |
| 13 | `plugin_set_param` (plugins/mod.rs:416; project.json rewritten per knob gesture) | bypass | `Op::Set{Plugin, Param}` + host-forward effect + `PersistEffect` | 9 |
| 14 | `zyn_load_patch` unmanaged global (plugins/patches.rs:312; patch lost on save) | bypass | `Op::PluginSetState` (blob inverse) | 9 |
| 15 | plugin auto-persist `state::persist_after_mutation` (state.rs:641; unserialized project.json read-modify-write) | side channel | replaced by `PersistEffect` executed post-lock | 2, 9 |
| 16 | sink `wrap_sink_with_import` / `do_import` (import.rs:382/187) | bypass (add_track + clips.push under two locks) | prepare-outside + one `Actor::System` tx (`TrackAdd`? no — tx-tier `ops::add_track` + `Op::ClipAdd`) | 8 |
| 17 | sink `wrap_sink_with_stem_import` (import.rs:466; track via channel, clip not) | mixed | fully channel: per-stem tx (`ClipAdd`) | 8 |
| 18 | sinks `wrap_sink_with_hum_apply`/`wrap_sink_with_accompany_apply` → `apply_hum_clip` (hum.rs:355; midi disk write under lock — Plan A handoff item) | bypass, I/O under lock | prepare-outside + `Actor::System` tx (`MidiClipAdd`), persist as effect | 8 |
| 19 | sink `wrap_sink_with_instrument_register` (control/mod.rs:1083) | touches only the global `SamplerBank` | **ruling 5**: live resource, not document — out of op scope, recorded | — |
| 20 | LoopJam watcher `apply` (loopjam.rs:516; replace_region_clips + push, no undo) | bypass | `Actor::System` tx: `ClipRemove`/`Set{Clip,…}`/`ClipAdd` batch | 8 |
| 21 | engine thread site 1: `open_output` sample-rate writeback (engine.rs:659) | direct | `Actor::Engine` transient tx via `Committer` | 13 |
| 22 | engine thread site 2: auto-stop (engine.rs:1021) | direct | `Actor::Engine` transient transport tx | 13 |
| 23 | engine thread sites 3+4: recording start/finalize (engine.rs:1186/1221; `clips.extend` never an op) | direct | `Actor::Engine` txs; finalize = `ClipAdd` ops (op created AFTER I/O completes, §4.4) | 13 |
| 24 | engine thread site 5: `ensure_project` (engine.rs:1256) | direct + emit | epoch function (carve-out: log epoch boundary) | 6, 13 |
| 25 | `open_project` (audio/mod.rs:605; wholesale swap) | bypass | **epoch boundary** (round-2 carve-out), sanctioned epoch fn, eager adopt | 6 |
| 26 | `save_project`/`save_project_as`/`create_project` (audio/mod.rs:661/593, control/mod.rs:564; save_as writes midi under lock) | bypass | **snapshot mark / epoch boundaries**, sanctioned, I/O outside lock | 6 |
| 27 | `seed_demo_project` (control/mod.rs:687; 3 direct add_tracks + midi pushes + save under lock) | bypass | routed through one channel tx (tx-tier ops) | 7 |
| 28 | transport family (`transport_play/stop/set_loop/set_stop_at_end` → ControlPlane::transport, control/mod.rs:242) | direct store writes | transient transport ops through commit (§4.2: "auto-stop produces a transport op") | 12 |
| 29 | device selection (audio/mod.rs:263/274) | EngineHandle direct | **carve-out** (app config, not document): behind a ControlPlane method for attribution, no op | 12 |
| 30 | frontend stems demo materialization + track reorder (project.svelte.ts:196/112, jobs.svelte.ts:243-259) | webview-only clips | deleted; frontend calls the real backend split-stems import | 11 |
| 31 | mix-fader drags: one `TxMeta::user` tx per `input` event, no boundaries | channel but unbatched | `gesture_begin`/`gesture_end` IPC + transient coalescing (§4.4) | 14 |
| 32 | `fold_ops` merges Sets across intervening structural ops (session.rs:346; Plan A handoff: constrain before ops persist) | wrong for replay | structural barrier in fold | 1 |
| 33 | piano-roll note-id churn (frontend `MidiNote` has no `noteId`; Plan B handoff) | ids survive only by object-spread accident | `noteId` in the TS type + explicit mint sentinel | 15 |
| 34 | `steady_time` per-node counter (clap_host.rs; C/D handoff ruling 3, bound to Plan E) | resets on rebind | engine-global steady clock | 16 |

Paths verified NOT to be writers (checked this session, recorded so the
totality claim is auditable): `sidecar_split_stems`/`sidecar_transcribe`
(job submits only), export thread (deep-copies under one lock, never
writes), MCP handlers (all via ControlPlane), scan worker, sampler preview,
recorder thread, meter cache.

### Scope rulings (decided now, marked per ADR 0007, so no task stalls)

1. **Op kind naming keeps the landed style** (`"set"`, `"trackAdd"`, …).
   D-03's draft envelope schema (`op-envelope.schema.json`) prescribes
   dotted kinds (`clip.move`); the three landed kinds don't dot, no journal
   has ever been written, and renaming landed kinds would churn Plan A/B
   tests for zero replay benefit (the format is born at Task 17's first
   journal line). The envelope schema's `kind` pattern is corrected to match
   the implementation in Task 17, as a **marked** correction. Round-2's
   "`clip.move` op" therefore lands as `Op::Set{object: Clip,
   path: TimelineStartSamples}` + the additive `move_clip` command — the
   capability round-2 demands, under the house naming.
2. **Transport ops are transient: through the channel, never journaled,
   never undoable.** `transport.state`/loop window/stop-at-end writes become
   `Op::Set` on `ObjectRef::Transport` paths so they are attributed,
   atomic, and single-writer (§4.2's auto-stop op), but they carry
   `TxMeta.transient = true`: no history entry, no journal line (no DAW
   undoes play/stop). `transport_seek` stays a pure RT atomic (position is
   engine state, not document state). `sample_rate` writeback is likewise
   transient (device property mirrored into the document for save).
3. **Undo-round-trip oracle masks the note-id watermark.** ADR 0001 makes
   `next_note_id` monotonic and never-rewinding; an undo of `MidiSetNotes`
   restores the notes but NOT the watermark (ids are never reused). The
   byte-identical snapshot oracle (Gate A test 1) therefore normalizes
   `next_note_id` to 0 in both snapshots before comparing, and separately
   asserts it never decreases. This is the test honoring the ADR, not the
   test being weakened — recorded so nobody "fixes" it back.
4. **Epoch boundaries are not ops** (round-2 §4.5 carve-out, verbatim):
   `open_project` is a log epoch boundary (document swap, history root);
   `save_project`/`save_project_as` are snapshot marks; `create_project`
   and the engine's `ensure_project` are epoch boundaries too (document
   birth). They become *sanctioned* epoch functions: single-lock swap
   discipline, eager adopt of midi/plugins/automation, history + redo stack
   cleared, journal rotated (Task 17) — but they never appear as ops in the
   log.
5. **`SamplerBank` stays outside `Session`.** Round-2 §4.1 names
   "SamplerBank's document half" as Session material — but at HEAD the bank
   has NO document half: loaded instruments are never persisted
   (`load_into_registered_bank` writes only the process-global bank; nothing
   lands in project.json), and the bank is exactly the "live plugin host
   handles / loaded voice data" class §4.1 explicitly keeps outside,
   referenced by id. The *document* half of instrument assignment is
   `TrackState.instrument_id`, which DOES become an op (Task 3). If a later
   round persists bank contents, that round moves them into Session.
   Recorded as a marked scope ruling; inventory row 19 closes on this.
6. **No MCP MIDI tools in this plan.** The MCP tool roster is frozen
   (ARCHITECTURE §12.2) and PHASE4-PLAN's E row doesn't include it. The
   channel work makes future MIDI tools one-liners over ControlPlane, but
   adding them is not Gate E material. (Dossier 10's "MCP has no MIDI
   tools" stands as a ledgered gap.)
7. **Track reorder gets no op.** The only caller of the frontend-only
   `placeTrackAfter` is the demo stems path, which Task 11 deletes; backend
   append order is the product's track order today. A `track.move` op is
   deferred to whichever round adds a real reorder gesture — recorded, not
   silent.
8. **`midi_export_file` writes the exported `.mid` file but stays
   classified a read**: it reads the document and writes *outside* it (an
   export artifact, like `export_song`). What made it a §7-test-4 violation
   was the lazy resync, which Task 6 removes.
9. **History is in-memory in this plan; Plan F owns persistence/retention.**
   Task 17's undo/redo stack is a bounded `Vec` of committed batches
   (cleared at epochs); the journal file is append-only NDJSON. Plan F
   (history storage, §6) replaces the storage substrate; the *exposure*
   (undo/redo commands, journal format) is Plan E's per PHASE4-PLAN ("F …
   informs E's undo exposure").

### Non-goals (so the diff stays reviewable)

- No snapshot-based rebuild (`engine::rebuild` still holds the session lock
  across the graph build — Plan F, standing deferral from the Plan A
  handoff).
- No panic rollback in `transact` (Plan F).
- No content split/merge/copy ops (round-2 §2.1's remint rules bind them
  the day they land — still unconsumed, passes forward again).
- No waveform-pyramid dedup by source (ledgered since Plan B).
- No `project.svelte.ts` `samplesPerBeat` position-aware refactor beyond
  what Task 4's clip-move commit needs (C/D handoff item, stays flagged).

---

### Task 1: `fold_ops` structural barrier

The Plan A handoff's binding pre-log item: `fold_ops` currently merges
`Set` ops on the same `(object, path)` **across intervening structural
ops**, and re-emits the merged Set at the position of its *first*
occurrence — harmless while the log is dark, wrong for replay (a
`[Set(t1), TrackRemove(t1), TrackAdd(t1), Set(t1)]` sequence must not fold
into one Set that predates the remove). Fix: a structural op is a barrier;
grouping only reaches back to the last barrier.

**Files:**
- Modify: `src-tauri/src/control/session.rs` (`fold_ops`, ~:346-405)
- Test: same file, `#[cfg(test)]` module

**Interfaces:**
- Consumes: `fold_ops(ops, inverses) -> (Vec<Op>, Vec<Op>)` (private, called
  once from `Session::transact`).
- Produces: same signature, new invariant: **no `Set` group spans a
  `Passthrough` slot**. Later tasks (7, 9, 10) rely on this when their
  structural ops interleave with Sets; Task 17's journal relies on it for
  replay order.

- [ ] **Step 1: Write the failing test** (in `session.rs` tests, next to
  `move_and_move_back_elides_to_noop_history`):

```rust
#[test]
fn fold_never_merges_sets_across_a_structural_op() {
    // gain 0→3, then a structural TrackAdd, then gain 3→6: the two Sets
    // must NOT merge into one 0→6 emitted before the TrackAdd.
    let m = fresh_session();          // existing helper pattern in this module
    let t2 = test_track("t-2");
    let c = Session::transact(&m, user_meta(), |tx| {
        tx.apply(testutil::set_gain("t-1", 3.0))?;
        tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![] })?;
        tx.apply(testutil::set_gain("t-1", 6.0))
    })
    .unwrap();
    assert_eq!(c.ops.len(), 3, "no cross-barrier merge: {:?}", c.ops);
    assert!(matches!(&c.ops[0], Op::Set { .. }));
    assert!(matches!(&c.ops[1], Op::TrackAdd { .. }));
    assert!(matches!(&c.ops[2], Op::Set { .. }));
    // and the inverses still undo cleanly in reverse order
    let snap_before = snapshot(&m); // if no local snapshot helper, assert on gain value instead
    let _ = snap_before;
}

#[test]
fn wiggle_back_across_a_structural_op_is_not_elided() {
    // gain 0→3, TrackAdd, gain 3→0: net-noop for the gain, but the two Sets
    // sit in different barrier regions — BOTH survive (replay order truth
    // beats history minimalism once ops persist).
    let m = fresh_session();
    let t2 = test_track("t-2");
    let c = Session::transact(&m, user_meta(), |tx| {
        tx.apply(testutil::set_gain("t-1", 3.0))?;
        tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![] })?;
        tx.apply(testutil::set_gain("t-1", 0.0))
    })
    .unwrap();
    assert_eq!(c.ops.len(), 3, "cross-barrier wiggle must not elide: {:?}", c.ops);
}
```

(If `session.rs`'s test module lacks a `fresh_session`/`test_track` local
helper, reuse the existing per-test construction pattern already in that
module — `session_owns_both_stores_under_one_lock` shows it.)

- [ ] **Step 2: Run to verify both fail** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml fold_never_merges -- --nocapture`
  Expected: FAIL (today they merge/elide).

- [ ] **Step 3: Implement the barrier.** In `fold_ops`, track the index of
  the slot region grouping may reach into:

```rust
let mut slots: Vec<Slot> = Vec::new();
let mut barrier = 0usize; // grouping never reaches below this index
for (op, inv) in ops.into_iter().zip(inverses.into_iter()) {
    match (&op, &inv) {
        (Op::Set { object, path, .. }, Op::Set { from: true_to, to: true_from, .. }) => {
            let existing = slots[barrier..].iter_mut().find_map(|s| match s {
                Slot::Grouped(g) if g.object == *object && g.path == *path => Some(g),
                _ => None,
            });
            // …existing merge-or-push body unchanged…
        }
        _ => {
            slots.push(Slot::Passthrough(op, inv));
            barrier = slots.len(); // structural op: nothing before it may absorb later Sets
        }
    }
}
```

The net-noop drop rule (`from == to` groups vanish) is unchanged — it now
applies per barrier region, which is exactly what the second test pins.

- [ ] **Step 4: Run the whole backend suite** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
  Expected: 384 + 2 new, all green (the existing same-region tests
  `move_and_move_back_elides_to_noop_history` and
  `three_ops_one_rebuild_one_event` must still pass — they have no
  intervening structural op).

- [ ] **Step 5: Commit** —
  `git commit -m "fix(channel): fold_ops stops merging Sets across structural ops (replay-order barrier)"`

---

### Task 2: Effect layer v2 — `PersistEffect`, `TxMeta::engine`, `transient`

Three enablers every later task consumes. (1) Persistence becomes an effect
executed after the lock — the mechanism that dismantles I/O-under-lock. (2)
`Actor::Engine` gets a constructor (today it is only ever built literally in
a test). (3) `TxMeta.transient` marks commits that must never reach history
or journal (transport, gesture mid-flight).

**Files:**
- Modify: `src-tauri/src/control/op.rs` (TxMeta), `src-tauri/src/control/session.rs` (EngineEffect), `src-tauri/src/control/mod.rs` (commit executes persistence)
- Test: `src-tauri/src/control/mod.rs` tests + `session.rs` tests

**Interfaces:**
- Produces:

```rust
// op.rs
impl TxMeta {
    pub fn engine(label: impl Into<String>) -> Self {
        Self { actor: Actor::Engine, run: uuid::Uuid::new_v4().to_string(),
               label: label.into(), transient: false }
    }
    /// Builder: mark this transaction transient — RT/document-visible,
    /// but excluded from history and journal (round-2 §4.4).
    pub fn transient(mut self) -> Self { self.transient = true; self }
}
pub struct TxMeta { pub actor: Actor, pub run: String, pub label: String,
    #[serde(default)] pub transient: bool }   // additive, default false

// session.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PersistEffect {
    pub midi: bool,      // midi::persist::save_into_project from a snapshot
    pub project: bool,   // audio::project::save from a snapshot
    pub plugins: bool,   // Task 9 wires the executor; field exists now
    pub automation: bool // Task 10 wires the executor; field exists now
}
pub struct EngineEffect { /* existing fields */ pub persist: PersistEffect }
```

- Every existing `TxMeta` literal/constructor gains `transient: false`
  (constructors default it; the one literal in op.rs tests updated).
- `ControlPlane::commit` executes `persist` AFTER param writes/rebuild/emit
  ordering decision: **persist runs after the effect writes and BEFORE the
  `project://changed` emit** (the event announces durable truth), from a
  snapshot taken under a fresh short lock:

```rust
// in commit(), after the rebuild send, before the emit:
if committed.effect.persist != PersistEffect::default() {
    self.execute_persist(&committed.effect.persist);
}
// …
fn execute_persist(&self, p: &PersistEffect) {
    // Snapshot under a short lock; ALL disk I/O happens after drop(guard).
    let (dir, midi_snapshot, project_snapshot) = {
        let s = self.session.lock();
        (s.store.project_dir.clone(),
         p.midi.then(|| s.midi_snapshot()),          // clone of persisted midi fields
         p.project.then(|| crate::audio::project::from_store(&s.store)))
    };
    let Some(dir) = dir else { return };             // unsaved in-memory project
    if let Some(m) = midi_snapshot {
        if let Err(e) = crate::midi::persist::save_snapshot_into_project(&dir, &m) {
            log::warn!("midi persist failed: {e}");
            self.session.lock().midi.dirty = true;    // M-5 semantics preserved
        } else {
            self.session.lock().midi.dirty = false;
        }
    }
    if let Some(pr) = project_snapshot {
        if let Err(e) = crate::audio::project::save(&dir, &pr) { log::warn!("project save failed: {e}"); }
    }
}
```

`Session::midi_snapshot()` is a small new method cloning
`ppq/tempo_events/meter_events/clips` into the existing struct
`midi::persist` already serializes (add
`midi::persist::save_snapshot_into_project` as a thin sibling of
`save_into_project` taking the snapshot instead of `&MidiStore` — same body,
different parameter; `save_into_project` becomes a wrapper over it).

- [ ] **Step 1: Failing tests.** In `control/mod.rs` tests (the harness with
  a fake emitter + temp dir already exists — see
  `set_track_mix_emits_project_changed_with_updated_tracks`):

```rust
#[test]
fn persist_effect_writes_midi_after_the_lock_and_before_the_emit() {
    // A commit whose closure sets effect.persist.midi writes
    // events/<clip>.bin + project.json WITHOUT holding the session lock
    // during I/O. Observable contract: after commit returns, the file
    // exists and midi.dirty == false.
    // (Use the existing test ControlPlane fixture with a tempdir project.)
}
#[test]
fn engine_meta_constructor_stamps_actor_engine() {
    let m = TxMeta::engine("auto-stop");
    assert_eq!(m.actor, Actor::Engine);
    assert!(!m.transient);
    assert!(m.transient().transient);
}
```

(Write the persist test against the fixture concretely — trigger via a
temporary test-only op or by calling `execute_persist` directly with a
constructed `PersistEffect`; the public trigger arrives with Task 5's midi
ops, so directly testing `execute_persist` is acceptable here and the Task 7
wrapper tests re-cover it end-to-end.)

- [ ] **Step 2: Run to verify failure** (compile failure for the missing
  types counts).
- [ ] **Step 3: Implement** the three interfaces exactly as above; update
  every `TxMeta` construction site (`grep -n 'TxMeta {' src-tauri/src`).
- [ ] **Step 4: Full backend suite green** —
  `timeout 900 cargo test --manifest-path src-tauri/Cargo.toml`
- [ ] **Step 5: Commit** —
  `git commit -m "feat(channel): PersistEffect executed post-lock, TxMeta::engine, transient flag"`

---

### Task 3: Audio ops — instrument, clip move, clip add/remove

The op vocabulary grows to cover audio document mutations: instrument
assignment (inventory row 2), clip placement (row 3 — round-2's "most
common editing gesture reaches no backend channel at all"), and clip
add/remove (rows 16/17/20/23 consume these from sinks/threads). Backend
half only; Task 4 wires the frontend.

**Files:**
- Modify: `src-tauri/src/control/op.rs` (PropPath + ObjectRef use, new kinds),
  `src-tauri/src/control/session.rs` (`apply_raw` arms, `write_prop`/`read_prop`),
  `src-tauri/src/audio/mod.rs` (`set_track_instrument` becomes a wrapper),
  `src-tauri/src/control/mod.rs` (`ControlPlane::move_clip`, `ControlPlane::set_track_instrument`)
- Test: `session.rs` + `control/mod.rs` tests; `src-tauri/tests/channel_properties.rs` (extend the strategy)

**Interfaces:**
- Produces (op.rs):

```rust
pub enum PropPath { Gain, Pan, Muted, Soloed, Armed,
    InstrumentId,          // Track: Option<String> as JSON string-or-null
    TimelineStartSamples,  // Clip: u64
}
pub enum Op { /* existing */
    /// Structural: insert an audio clip (payload = full row; inverse = ClipRemove).
    ClipAdd { clip: crate::audio::types::Clip, index: usize },
    /// Structural: remove an audio clip; `clip`/`index` advisory beyond `clip.id`
    /// (store truth wins, mirroring TrackRemove).
    ClipRemove { clip: crate::audio::types::Clip, index: usize },
}
```

- `apply_raw` arms (session.rs), following the TrackAdd/TrackRemove
  pattern exactly:
  - `Set{Track, InstrumentId}`: validate track exists; read old
    `instrument_id` as `from`; write; `effect.rebuild = true`;
    `effect.persist.project = true`.
  - `Set{Clip, TimelineStartSamples}`: find clip by id in `store.clips`
    (error if absent — atomic failure); `from` = old value; write;
    `effect.rebuild = true`; `effect.persist.project = true`.
  - `ClipAdd`: duplicate-id guard (reject if `store.clips` already has the
    id — same rule TrackAdd got in Plan B); insert at `index.min(len)`;
    inverse `ClipRemove { clip, index: actual }`; `effect.rebuild = true`;
    `effect.persist.project = true`.
  - `ClipRemove`: find by `clip.id` (error if absent); remove; inverse
    `ClipAdd` with store-truth row + actual index; `effect.rebuild = true`;
    `effect.persist.project = true`.
- `ControlPlane::move_clip(clip_id: &str, timeline_start_samples: u64, meta) -> Result<Committed, String>`
  and `ControlPlane::set_track_instrument(track_id: &str, instrument_id: Option<String>, meta)`
  — thin `commit` wrappers, mirroring `set_track_mix`'s shape.
- `set_track_instrument` Tauri command body becomes
  `control.set_track_instrument(&track_id, instrument_id, TxMeta::user("set instrument"))`
  — frozen name, wrapper body, now emits `project://changed` (it never did
  before; that is a bug fix, not a behavior break — dossier 10 §2.4 lists
  the missing event as the defect).

- [ ] **Step 1: Failing unit tests** (session.rs tests):

```rust
#[test]
fn clip_move_applies_and_inverse_restores() { /* seed a track+clip via the
    module's fixture pattern; transact Set{Clip,TimelineStartSamples,to:48000};
    assert store value; transact the returned inverse; assert original. */ }
#[test]
fn clip_add_duplicate_id_is_rejected_atomically() { /* ClipAdd twice with same
    id inside one tx → whole tx errors, store unchanged, rev unchanged. */ }
#[test]
fn instrument_set_round_trips_and_requests_rebuild() { /* Set{Track,InstrumentId}
    from None to Some("plugin:x"); committed.effect.rebuild == true;
    inverse restores None. */ }
```

- [ ] **Step 2: Run to verify failures** (compile errors count).
- [ ] **Step 3: Implement** op kinds + arms + the two ControlPlane methods +
  the command wrapper. Keep the serde test
  `op_serde_round_trips_and_carries_version` extended with
  `"kind":"clipAdd"` / `"clipRemove"` wire-form assertions and a
  `"timelineStartSamples"` path assertion.
- [ ] **Step 4: Extend the Gate A property strategy**
  (`tests/channel_properties.rs`): add `ClipMove` and `ClipAdd/ClipRemove`
  actions to `RawAction` with weights 2/1/1, model-threading clip ids the
  same way track ids are threaded. The undo-round-trip and atomicity
  properties now cover the new arms with zero new property code.
- [ ] **Step 5: Full backend suite green.**
- [ ] **Step 6: Commit** —
  `git commit -m "feat(ops): audio clip move/add/remove + track instrument through the channel"`

---

### Task 4: `move_clip` command + the frontend audio-clip commit

Closes inventory row 3 end to end: the drag stays a local live preview
(thin-renderer-compliant, same as the MIDI pattern PR #8 established), and
pointerup/arrow-keys commit through the new additive command.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (Tauri command `move_clip`),
  `src-tauri/src/lib.rs` (register — additive),
  `src/lib/tauri.ts` (binding), `src/lib/state/project.svelte.ts`
  (`commitClipMove`, header comment), `src/lib/components/ClipView.svelte`
  (pointerup + arrow commit)
- Test: `src-tauri/src/control/mod.rs` test; frontend
  `src/lib/state/project.svelte.test.ts` (or the existing state test file
  colocated with the store)

**Interfaces:**
- Produces: Tauri command
  `move_clip(clip_id: String, timeline_start_samples: u64) -> Result<(), String>`
  → `control.move_clip(&clip_id, timeline_start_samples, TxMeta::user("move clip"))`.
- Frontend: `backend.moveClip(clipId, timelineStartSamples)` in `tauri.ts`;
  `project.commitClipMove(clipId)` reads the already-dragged local value and
  invokes; `project.moveClip` keeps its signature (live preview only, doc
  comment updated to say "frontend preview during drag; commitClipMove
  persists — mirrors midi.svelte.ts moveClip/commitBounds").

- [ ] **Step 1: Backend test first** (control/mod.rs tests): commit
  `move_clip` on the fixture, assert store truth moved, one
  `project://changed` with the new position, error path for unknown clip id.
- [ ] **Step 2: Implement command + register + run backend suite.**
- [ ] **Step 3: Frontend failing test**: `commitClipMove` invokes
  `move_clip` with the store's current `timelineStartSamples` (mock
  `invoke`, mirror the store's existing test style).
- [ ] **Step 4: Wire ClipView**: `onPointerUp` → if the drag moved, `await
  project.commitClipMove(clip.id)`; arrow-key handler commits after the
  local move (per keypress, matching MidiClipView's established behavior).
  Update the stale header comment in `project.svelte.ts:2-5` ("no clip-move
  command exists" is false as of this task).
- [ ] **Step 5: Both suites green** (backend + `timeout 300 npx vitest run`).
- [ ] **Step 6: Commit** —
  `git commit -m "feat(clips): audio clip move commits through the channel (move_clip command)"`

### Task 5: MIDI + tempo op kinds

The op vocabulary's MIDI half: `TempoSet` (one atomic apply covering
`midi.ppq/tempo_events/meter_events` AND the `store.transport.tempo_bpm`
mirror — the cross-store atomicity §4.1 exists for), `MidiClipAdd`/
`MidiClipRemove`, `Set{MidiClip, …bounds}`, and `MidiSetNotes` (the §4.4
value-replacement wrapper form, marked coalescable). Backend vocabulary +
apply arms only; Task 7 rewires the commands.

**Files:**
- Modify: `src-tauri/src/control/op.rs`, `src-tauri/src/control/session.rs`
- Test: `session.rs` tests; `tests/channel_properties.rs` (strategy +
  watermark-mask per scope ruling 3)

**Interfaces:**
- Produces (op.rs):

```rust
pub enum PropPath { /* existing + Task 3 */
    TimelineStartTicks, LengthTicks, ContentLengthTicks,  // MidiClip: u64
}
pub enum Op { /* existing + Task 3 */
    /// Atomic tempo/meter replacement, including the transport.tempo_bpm
    /// mirror (= first event's bpm), which today is a second non-atomic
    /// phase (inventory row 4). Inverse carries the previous triple.
    TempoSet { ppq: u32,
               events: Vec<crate::midi::types::TempoEvent>,
               meter: Vec<crate::midi::types::MeterEvent> },
    /// Structural: insert a MIDI clip (payload = full row incl. content_id/
    /// lane_id/next_note_id; inverse = MidiClipRemove). Duplicate-id guard
    /// like ClipAdd. Restoring a removed clip's row restores its watermark
    /// — correct: that watermark was the clip's own (ADR 0001).
    MidiClipAdd { clip: crate::midi::types::MidiClip, index: usize },
    MidiClipRemove { clip: crate::midi::types::MidiClip, index: usize },
    /// §4.4 value-replacement wrapper form: whole-notes replace, diffed
    /// against store truth server-side, coalescable by (kind, object,
    /// actor). `notes` may carry noteId 0 (mint) or live ids (keep) —
    /// assign_incoming_note_ids' keep-rule applies inside apply_raw.
    /// Inverse = MidiSetNotes with the previous notes (all live ids).
    /// The watermark advances monotonically and is NOT rewound by the
    /// inverse (scope ruling 3, ADR 0001).
    MidiSetNotes { clip: crate::ids::ClipId, notes: Vec<crate::midi::types::MidiNote> },
}
```

- `apply_raw` arms (all set `effect.persist.midi = true`; structural and
  tempo arms also `effect.rebuild = true`; TempoSet additionally
  `effect.persist.project = true` for the bpm mirror in project.json):
  - `TempoSet`: validate `!events.is_empty()` and sortedness (reject,
    atomic); inverse = `TempoSet` with old triple; write the three midi
    fields + `store.transport.tempo_bpm = events[0].bpm`.
  - `MidiClipAdd`/`MidiClipRemove`: mirror ClipAdd/ClipRemove against
    `session.midi.clips`, incl. duplicate-id guard and store-truth inverse.
  - `Set{MidiClip, TimelineStartTicks|LengthTicks|ContentLengthTicks}`:
    find by id, from = old, write, clamp `length_ticks >= 1` (the command
    does this today — keep the clamp in `write_prop` so inverses see the
    post-clamp value, mirroring the gain-clamp round-trip rule from Plan A).
  - `MidiSetNotes`: find clip; `from` inverse = current notes; run the
    existing `assign_incoming_note_ids(&mut clip, notes)` (midi/mod.rs) —
    move/adapt that helper so session.rs can call it without a midi lock
    (it already operates on `&mut MidiClip` + payload).

- [ ] **Step 1: Failing tests** (session.rs):

```rust
#[test] fn tempo_set_is_atomic_including_the_bpm_mirror() { /* transact
    TempoSet{ppq:960, events:[{0,140.0}], meter:[{0,3,4}]}; assert midi
    fields AND store.transport.tempo_bpm == 140.0 in one commit; inverse
    restores all four. */ }
#[test] fn midi_set_notes_inverse_restores_notes_but_never_rewinds_watermark()
    { /* seed clip with 2 noted ids, watermark 3; set notes to 3 notes (one
    id 0 → minted 3, watermark 4); undo via inverse; notes == original,
    next_note_id == 4 (not 3). */ }
#[test] fn midi_clip_add_then_inverse_removes_byte_identically() { /* full
    row round trip incl. content_id/lane_id. */ }
#[test] fn midi_bounds_set_clamps_and_round_trips() { /* LengthTicks to 0
    clamps to 1; inverse restores pre-clamp truth. */ }
```

- [ ] **Step 2: Verify failures.**
- [ ] **Step 3: Implement** kinds + arms; extend the serde wire-form test
  (`"kind":"tempoSet"`, `"midiClipAdd"`, `"midiSetNotes"`,
  `"contentLengthTicks"` path).
- [ ] **Step 4: Property coverage**: extend `channel_properties.rs`'s
  strategy with `MidiClipAdd/Remove`, `MidiBoundsSet`, `MidiSetNotes`,
  `TempoSet` actions; apply scope ruling 3 to the snapshot oracle
  (normalize every clip's `next_note_id` to 0 in both snapshots + assert
  monotonicity separately). 256 cases stay.
- [ ] **Step 5: Full backend suite green.**
- [ ] **Step 6: Commit** —
  `git commit -m "feat(ops): MIDI clip/notes/bounds + atomic TempoSet through the channel"`

---

### Task 6: Epoch boundaries — sanctioned swap, eager adopt, pure MIDI readers

Round-2's two carve-outs made real (scope ruling 4), and inventory rows 9,
24, 25, 26 closed. After this task, **no read path lazily resyncs**: the
midi/plugins/automation adopt-chain runs exactly at epoch boundaries
(open/create/save-as/ensure-project), so `midi_get_clips`/
`midi_export_file` become pure reads, and `sync_midi_store`'s
plugin-teardown-inside-a-read horror (inventory row 9) is gone.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (epoch functions on ControlPlane),
  `src-tauri/src/audio/mod.rs` (`open_project_impl`/`save_project_impl`
  delegate), `src-tauri/src/midi/mod.rs` (`with_synced_store` read paths,
  `sync_midi_store`, `notify_project_opened`), `src-tauri/src/midi/persist.rs`
  (`load_from_project` stops adopting plugins/automation — the epoch fn
  sequences that explicitly)
- Test: `control/mod.rs` tests + a new `src-tauri/tests/pure_readers.rs`

**Interfaces:**
- Produces on `ControlPlane` (all take `&self`, none produce ops):

```rust
/// Epoch boundary (round-2 §4.5 carve-out): swaps the document wholesale.
/// Sequencing: (1) short lock — swap store fields + midi from the parsed
/// project; (2) drop lock; (3) adopt plugins + automation from dir (host
/// round-trips OUTSIDE the session lock); (4) Rebuild; (5) emit
/// project://changed. History/journal hooks (Task 17) fire at (1).
pub fn open_project_epoch(&self, dir: &Path) -> Result<Project, String>;
pub fn create_project_epoch(&self, name: Option<String>) -> Result<Project, String>; // absorbs today's create_project
pub fn save_project_as_epoch(&self, dir: &Path) -> Result<Project, String>;          // snapshot mark; midi write OUTSIDE the lock (fixes control/mod.rs:659)
pub fn save_project_mark(&self) -> Result<Project, String>;                          // snapshot mark (today's save, sanctioned)
pub fn ensure_project_epoch(&self) -> Result<PathBuf, String>;                       // engine auto-project (Task 13 switches the engine to call this)
```

- `midi::persist::load_from_project` loses its two adopt calls
  (persist.rs:292-293); the epoch functions call
  `plugins::state::adopt_open_project` / `plugins::automation::adopt_open_project`
  explicitly, after the session lock is dropped.
- `sync_midi_store` becomes `pub(crate) fn adopt_midi_from_dir(midi: &mut MidiStore, dir, bpm)`
  — called ONLY from epoch functions; the lazy call sites in
  `with_synced_store`'s read path (midi/mod.rs:255) and `midi_import_file`
  (midi/mod.rs:551) are deleted. The `dirty` guard semantics move with it.
- `with_synced_store(mutating=false)` collapses to
  `read_midi<R>(state, f: impl FnOnce(&MidiStore) -> Result<R, String>)` —
  lock, read, drop. (`mutating=true` callers survive until Task 7 rewires
  them; they keep persisting but stop lazy-syncing — eager epochs make the
  sync redundant, and `dirty` still guards the failure case.)

- [ ] **Step 1: Failing purity test** (`tests/pure_readers.rs`):

```rust
//! Gate E test-4 precursor: declared read paths mutate nothing.
//! Builds a ControlPlane fixture with a saved project on disk, snapshots
//! (store + midi via canonical JSON, automation lane count, plugin row
//! count), invokes each read path twice, asserts snapshot equality:
//!   - ControlPlane::get_project_state-equivalent read
//!   - midi read helper (post-Task-6 read_midi) after a project_dir change
//!     — the old lazy-resync trigger — asserts the midi store is NOT
//!     replaced by a read (staleness is impossible: epochs adopt eagerly,
//!     so simulate the old trigger by calling the epoch fn, then read).
```

- [ ] **Step 2: Verify it fails** against today's lazy path (construct the
  dir-change scenario that used to trigger resync).
- [ ] **Step 3: Implement** the five epoch functions by MOVING the existing
  bodies (`open_project_impl`, `ControlPlane::create_project`,
  `save_project_as`, `save_project_impl`, engine `ensure_project`'s
  project-creation half) onto ControlPlane with the sequencing above; the
  Tauri commands become one-line delegates (frozen names). Delete the adopt
  calls from `load_from_project`; delete lazy sync from read paths.
  `hum.rs:319`'s `notify_project_opened` call is deleted (it was a lazy
  resync crutch; hum's sink runs against an already-adopted store).
- [ ] **Step 4: Full backend suite green** — pay attention to
  `v3_migration.rs` (uses load paths) and midi tests that relied on lazy
  sync; rewrite those fixtures to call the epoch fn first (that IS the new
  contract).
- [ ] **Step 5: Commit** —
  `git commit -m "feat(epochs): sanctioned open/create/save/ensure epochs; eager adopt; MIDI readers pure"`

---

### Task 7: MIDI commands become channel wrappers

Inventory rows 4-8 + 27. Every MIDI command body becomes a wrapper that
builds ops and commits; `with_synced_store`'s mutating form dies;
persistence rides `PersistEffect`; `midi_import_file` is the §4.4
prepare-outside exemplar; `seed_demo_project` stops being the last
direct-store writer.

**Files:**
- Modify: `src-tauri/src/midi/mod.rs` (all seven command bodies),
  `src-tauri/src/control/mod.rs` (`seed_demo_project`, tx-tier `ops::add_track`
  usage), `src-tauri/src/control/ops.rs` (add the `&mut Tx` tier for track
  add — §4.3's two-tier rule, so composite transactions can create tracks)
- Test: midi/mod.rs tests + control/mod.rs tests

**Interfaces:**
- Consumes: Task 5 ops, Task 2 PersistEffect, Task 6 epochs.
- Produces:

```rust
// ops.rs — the §4.3 Tx tier (compile-time composition):
pub fn add_track_tx(tx: &mut session::Tx<'_>, name: Option<String>, kind: Option<String>)
    -> Result<TrackState, String>
// builds the row via new_track_row against tx's store view, then
// tx.apply(Op::TrackAdd { .. })?; returns the row. new_track_row needs
// &Store only for len/color — expose a read accessor on Tx (pub fn
// store(&self) -> &Store, pub fn midi(&self) -> &MidiStore) so wrappers
// can validate/derive without a second lock. (Read-only accessors keep
// the &mut-only-through-apply discipline intact.)
```

  Command bodies (frozen names, wrapper bodies):
  - `set_tempo_map(ppq, events)` → quantize exactly as today, then
    `control.commit(TxMeta::user("set tempo map"), |tx| tx.apply(Op::TempoSet{…}))`.
    The separate `transport.tempo_bpm` writeback (midi/mod.rs:343) is
    DELETED — the op's apply owns the mirror.
  - `midi_add_clip` → validate track kind via `tx.store()`, then
    `tx.apply(Op::MidiClipAdd{…})`.
  - `midi_set_notes` → `tx.apply(Op::MidiSetNotes{…})` (server-side diff
    lives in apply; command marked coalescable via meta label for Task 17's
    350ms fallback merge).
  - `midi_set_clip_bounds` → up to three `Op::Set{MidiClip,…}` in one tx.
  - `midi_import_file` → **prepare outside**: parse the SMF fully into
    `(tempo_events, meter?, Vec<(track_name, MidiClip)>)` with NO lock;
    then ONE `control.commit(TxMeta::user("import midi file"), |tx| { …
    add_track_tx for missing tracks … tx.apply(TempoSet)… tx.apply(MidiClipAdd)… })`.
    Three-lock dance (midi/mod.rs:548/563/612) deleted.
  - `midi_get_clips`/`midi_export_file` — already pure from Task 6; no change.
  - `seed_demo_project` → one commit: 3× `add_track_tx` + instrument Sets +
    3× `MidiClipAdd`; `persist` effect replaces its manual saves; emits via
    commit (fixes its missing event).
- `with_synced_store` is deleted; `PLAYBACK_SESSION` global stays (playback
  reads; not a writer — verified in the sweep) but gets a doc comment
  naming it read-only.

- [ ] **Step 1: Failing tests**: for each command, an integration-style
  test against the ControlPlane fixture asserting (a) document truth, (b)
  exactly one `project://changed` per invoke, (c) undo via inverses
  restores (notes case respects scope ruling 3), (d) `midi_import_file` on
  a fixture SMF creates tracks+clips+tempo in ONE rev bump. Reuse
  midi/mod.rs's existing test SMF fixtures.
- [ ] **Step 2: Verify failures.**
- [ ] **Step 3: Implement** in the order: Tx read accessors → add_track_tx
  → command bodies smallest-first (`midi_set_clip_bounds`, `midi_add_clip`,
  `midi_set_notes`, `set_tempo_map`, `midi_import_file`, `seed_demo_project`)
  — run the suite between each (many existing midi tests will need their
  setup switched from direct store pokes to channel calls; change the
  setup, keep the assertions).
- [ ] **Step 4: Full backend suite green; frontend suite green** (wire
  shapes unchanged — commands kept signatures; if any frontend test stubs
  `midi_get_clips` resync behavior, fix the stub).
- [ ] **Step 5: Commit** —
  `git commit -m "feat(midi): all MIDI commands through the channel; with_synced_store retired"`

---

### Task 8: Sidecar sinks + LoopJam through the channel

Inventory rows 16-18, 20. Every job-completion mutation becomes
prepare-outside + one `Actor::System` transaction. Fixes the Plan A handoff
item (`apply_hum_clip` disk write under lock) by construction — persistence
is an effect now.

**Files:**
- Modify: `src-tauri/src/control/import.rs` (`do_import`,
  `import_audio_clip_impl`, both sink wrappers),
  `src-tauri/src/control/hum.rs` (`apply_hum_clip`/`apply_hum_notes`),
  `src-tauri/src/control/loopjam.rs` (`apply`)
- Test: existing sink/loopjam tests in those files + control/mod.rs

**Interfaces:**
- Consumes: `ClipAdd`/`ClipRemove`/`Set{Clip,…}` (Task 3), `MidiClipAdd`
  (Task 5), `add_track_tx` (Task 7), `TxMeta::system`.
- Produces:
  - `do_import`: decode/copy/pyramid FIRST (no lock — already true), then
    ONE commit: optional `add_track_tx` + `Op::ClipAdd`; `persist.project`
    effect replaces the manual `project::save`; commit's emit replaces the
    raw emit. The two-lock window (import.rs:220/299) is gone.
  - `wrap_sink_with_stem_import`: per stem, ONE commit (`add_track_tx` +
    `ClipAdd`) — `TxMeta::system("auto-import stem: {stem}")`, run id
    SHARED across the stems of one job (mint one uuid before the loop and
    pass it into the metas — that's what `run` is FOR, round-2 §4.6).
  - `apply_hum_clip`: notes prepared outside; ONE commit: optional
    `add_track_tx` + `MidiClipAdd`; `persist.midi` effect; commit emits
    `project://changed` (fixes hum's silent-UI defect).
  - `LoopJam::apply`: compute the region diff OUTSIDE the lock from a read
    snapshot (which clips to remove/trim), then ONE commit emitting
    `ClipRemove`×n + `Set{Clip,…}` trims + `ClipAdd` — the whole swap is
    one atomic, undoable batch (`TxMeta::system("loopjam apply")`, marked
    NOT transient — a user can undo a LoopJam replacement). Race note: if
    the store changed between snapshot and commit, the closure re-validates
    ids and errs cleanly; the watcher retries on next poll (same liveness
    as today's lock-then-apply, now without the lost-update window).
- [ ] **Step 1: Failing tests** — extend each file's existing sink test to
  assert: one rev bump per apply, ops present in the Committed, undo
  restores the pre-apply clip set (loopjam case), no `project.json` write
  while the session lock is held (assert via the persist-effect pathway:
  the manual `project::save` call sites are gone — grep-gate in the test
  file: `assert!(!source_contains("project::save"))` is NOT how we test;
  instead test behavior: file mtime changes only after commit returns).
  Keep it behavioral: commit, then read project.json and assert the clip
  row is there.
- [ ] **Step 2: Verify failures.**
- [ ] **Step 3: Implement** import → stem → hum → loopjam, suite between
  each.
- [ ] **Step 4: Full backend suite green.**
- [ ] **Step 5: Commit** —
  `git commit -m "feat(sinks): import/stem/hum/loopjam apply through Actor::System transactions"`

---

### Task 9: Plugin rows into Session + plugin ops

The biggest store move (inventory rows 12-15 + zyn row 14). The registry's
document half (instances, params mirror, pending_state) moves into
`Session`; host handles stay outside, referenced by id (§4.1). Plugin
commands become wrappers; auto-persist becomes `persist.plugins`;
`plugin_remove` gets the §4.4 restore-from-blob inverse.

**Files:**
- Modify: `src-tauri/src/control/session.rs` (Session grows
  `plugins: PluginDoc`), `src-tauri/src/plugins/mod.rs` (commands,
  `PluginState` loses the doc half), `src-tauri/src/plugins/state.rs`
  (persist path takes a snapshot; `persist_after_mutation` deleted),
  `src-tauri/src/plugins/patches.rs` (`zyn_load_patch`),
  `src-tauri/src/control/op.rs` + `session.rs` (op kinds + arms),
  `src-tauri/src/control/mod.rs` (commit executes `persist.plugins` +
  host-forward effect)
- Test: plugins/mod.rs + control/mod.rs tests

**Interfaces:**
- Produces:

```rust
// session.rs
pub struct PluginDoc { pub instances: Vec<PluginInstanceRow>,
                       pub params: HashMap<String, Vec<f32>>,
                       pub pending_state: HashMap<String, Vec<u8>> }
pub struct Session { pub store: Store, pub midi: MidiStore,
                     pub plugins: PluginDoc, pub(crate) rev: u64 }
// op.rs
pub enum ObjectRef { /* existing */ Plugin(String) /* instance id */ }
pub enum PropPath { /* existing */ Param { index: u32 } }
pub enum Op { /* existing */
    /// Prepare-outside: the host instance already exists (instantiation
    /// round-trip BEFORE the tx); this op registers the document row.
    PluginAdd { row: PluginInstanceRow, index: usize },
    /// Restore-from-blob (§4.4): `state` captured before teardown; the
    /// inverse is PluginAdd with the row + the blob in pending_state, and
    /// undo re-instantiates through the host state machine (host-forward
    /// effect, executed after the lock).
    PluginRemove { row: PluginInstanceRow, index: usize, state: Option<Vec<u8>> },
    /// Full-state write (zyn patch loads): inverse carries the old blob.
    PluginSetState { instance: String, state: Vec<u8> },
}
// session.rs EngineEffect
pub host_forward: Vec<HostForward>,   // executed post-lock
pub enum HostForward { ParamWrite { instance: String, index: u32, value: f32 },
                       Instantiate { instance: String },     // undo of remove
                       Destroy { instance: String },          // forward of remove
                       LoadState { instance: String } }
```

  Effect execution in `commit` (after param writes, before persist): walk
  `host_forward` calling the existing `plugins::set_params_and_forward` /
  clap_host/lv2_host entry points — the SAME functions commands call today,
  now sequenced post-lock. `Set{Plugin, Param}` folds like mix Sets
  (coalescable — a knob drag is Sets on one (object, path)).
- Command wrappers: `plugin_instantiate` = host instantiate FIRST
  (prepare), then commit `PluginAdd` (failure → destroy the orphan host
  instance, document untouched — the §7-test-3 once-failing path);
  `plugin_remove` = capture blob (host save_state, outside lock) → commit
  `PluginRemove` → Destroy effect; `plugin_set_param` = commit Sets with
  host_forward + `persist.plugins`; `zyn_load_patch` = host load (prepare) →
  commit `PluginSetState` (old blob read via host save_state before load) —
  the patch is now durable AND undoable, closing row 14.
- `persist.plugins` executor: snapshot `PluginDoc` + dir under a short
  lock; `state::save_snapshot_into_project(dir, &doc, with_host_state)`
  outside; the `update_project_v2` read-modify-write keeps its file-level
  atomic-rename discipline.
- `plugin_scan` stays outside the document (scanner cache) — recorded
  inline; `adopt_open_project` (epoch-called since Task 6) now writes
  `session.plugins` under the epoch's swap lock and does host work outside.

- [ ] **Step 1: Failing tests**: `plugin_remove_then_undo_restores_state_blob`
  (fake host: use the existing test host stubs in plugins tests; assert the
  inverse carries the blob and undo re-emits Instantiate+LoadState
  effects); `plugin_set_param_is_coalescable_and_persists_via_effect`;
  `session_owns_plugin_rows` (mirror `session_owns_both_stores_under_one_lock`).
- [ ] **Step 2: Verify failures.**
- [ ] **Step 3: Implement** in order: PluginDoc move (mechanical; the
  global `REGISTRY` static becomes host-only state) → op kinds/arms →
  effect executor → command wrappers → zyn. Suite between each.
- [ ] **Step 4: Full backend suite green** (plugins has 55 tests — expect
  fixture churn, keep assertions).
- [ ] **Step 5: Commit** —
  `git commit -m "feat(plugins): registry document half into Session; plugin ops with restore-from-blob undo"`

---

### Task 10: AutomationStore into Session + automation ops

Inventory rows 10-11. Same shape as Task 9, smaller: the automation lanes
move under the Session lock; `automation_set` commits ops;
`automation_get` becomes pure.

**Files:**
- Modify: `src-tauri/src/control/session.rs` (Session grows
  `automation: AutomationDoc`), `src-tauri/src/plugins/automation.rs`
  (commands + `with_synced` retirement + the `STORE` global deleted),
  `src-tauri/src/control/op.rs`/`session.rs` (op kind + arm),
  `src-tauri/src/control/mod.rs` (`persist.automation` executor)
- Test: automation.rs tests + `tests/pure_readers.rs` (add automation_get)

**Interfaces:**
- Produces:

```rust
pub enum Op { /* existing */
    /// Value-replacement per lane (§4.4 wrapper form; an edit gesture is
    /// one lane write, never one op per point). to=None deletes the lane;
    /// inverse carries the previous lane (or None if it didn't exist).
    AutomationSetLane { key: String /* lane target id */,
                        lane: Option<AutomationLane> },
}
```

  Apply: upsert/remove in `session.automation.lanes`;
  `effect.persist.automation = true`; rebuild only if the engine reads
  lanes at graph build (check `engine::rebuild` — if lanes are read at
  render time via tables instead, no rebuild; pin whichever is true in a
  test comment with a file:line cite).
- `automation_get` reads `session.automation` under the lock — no
  `loaded_dir` bookkeeping left (epochs adopt).
- Persist executor mirrors midi: snapshot lanes, write
  `automation[]` + chunk files outside the lock, keep the chunk GC.

- [ ] **Step 1: Failing tests**: set/overwrite/delete lane round-trip with
  inverses; `automation_get` purity (extend `pure_readers.rs`);
  lane persisted to project.json only after commit returns.
- [ ] **Step 2: Verify failures.**
- [ ] **Step 3: Implement** (store move → op → wrappers → persist).
- [ ] **Step 4: Full backend suite green.**
- [ ] **Step 5: Commit** —
  `git commit -m "feat(automation): lanes into Session; automation_set through the channel; automation_get pure"`

### Task 11: Frontend stems path — real backend import, demo materialization deleted

Inventory row 30. The SPLIT STEMS gesture stops inventing webview-only
clips (`createClip` + `crypto.randomUUID()` + demo-only hooks) and calls
the real backend path, which already lands stems through the channel after
Task 8.

**Files:**
- Modify: `src/lib/state/jobs.svelte.ts` (`materializeStems` callers →
  backend split-stems flow), `src/lib/state/project.svelte.ts` (delete
  `createClip`, `placeTrackAfter`), `src/lib/tauri.ts` (ensure
  `import_audio_clip_split_stems`/`sidecar_run_job`-with-import binding
  exists; delete the demo-only `hintClipCharacter`/`registerClip` optional
  methods), `src/lib/components/ClipView.svelte` (SPLIT STEMS handler)
- Test: frontend state tests

**Interfaces:**
- Consumes: backend `import_audio_clip_split_stems` (src-tauri/src/lib.rs:143,
  already registered) — runs demucs and auto-imports each stem as a real
  track+clip via Task 8's channel txs, announcing via `project://changed`.
- Produces: the SPLIT STEMS gesture = one invoke; the UI updates from
  `project://changed` events (which every stem tx now emits), not from
  hand-built local clips. `placeTrackAfter`'s grouping disappears with it
  (scope ruling 7).

- [ ] **Step 1: Failing frontend test**: SPLIT STEMS handler invokes the
  backend command with the selected clip's id and does NOT touch
  `project.clips` directly (spy on the store).
- [ ] **Step 2: Implement**; delete `createClip`/`placeTrackAfter` and the
  demo-only tauri.ts hooks; `demo.ts`'s browser-mode path keeps whatever it
  needs by constructing its own fixture data internally (demo mode is not
  the product path — do not let it keep the deleted store methods alive;
  adapt demo.ts to build its state without them).
- [ ] **Step 3: Both suites green.**
- [ ] **Step 4: Commit** —
  `git commit -m "feat(stems): SPLIT STEMS uses the real backend import; frontend-invented clips deleted"`

---

### Task 12: Transport family as transient ops + device-selection attribution

Inventory rows 28-29, scope ruling 2. `ControlPlane::transport`'s four
direct store writes become one transient commit; device selection moves
behind ControlPlane methods for attribution (carve-out: config, not
document, no op).

**Files:**
- Modify: `src-tauri/src/control/op.rs` (`ObjectRef::Transport`, transport
  PropPaths), `src-tauri/src/control/session.rs` (arms),
  `src-tauri/src/control/mod.rs` (`transport` rewritten; `select_input_device`/
  `select_output_device` methods), `src-tauri/src/audio/mod.rs` (device
  commands delegate)
- Test: control/mod.rs tests

**Interfaces:**

```rust
pub enum ObjectRef { /* existing */ Transport }   // unit: {"family":"transport"}
pub enum PropPath { /* existing */ TransportState /* "playing"|"stopped"|"recording" */,
    LoopEnabled, LoopStartSamples, LoopEndSamples, StopAtEnd, SampleRate }
```

- `ControlPlane::transport(action)` builds
  `TxMeta::user(label).transient()` + the Sets, commits, THEN does the RT
  atomic writes + `transport://state` emit exactly as today (atomics are
  effects — order: atomics after the document commit, same as today's
  sequence for `set_loop` is atomics-first; preserve today's RT ordering
  by putting the atomic writes in the effect executor as `param_writes`-
  style entries? No — keep it simple and honest: the transport atomics
  stay in `ControlPlane::transport` after `commit` returns, with a comment
  citing [C1]; they are engine-visible state, not document state, and the
  document mirror is what the op covers).
- Transient commits skip history/journal (Task 17 enforces; the flag
  exists since Task 2) but DO bump rev and DO emit `project://changed`?
  **No** — transport commits keep emitting `transport://state` only
  (frozen event, right payload); `commit` gains a
  `quiet: bool`-free alternative: add
  `ControlPlane::commit_with(meta, f, emit_project_changed: bool)` used by
  transport (false) and default `commit` delegating with true. (The frozen
  `project://changed` contract is full-Project; spamming it per play/stop
  would be a behavior change.)
- `StopRecording`'s engine round-trip stays OUTSIDE the transaction
  (§4.2): `transport_stop` commits the state Set, then sends
  `ControlMsg::StopRecording` after commit returns (the engine's own
  finalize tx is Task 13).
- Device selection: `ControlPlane::select_input_device(name, meta)` logs
  the attribution (label + actor into the tracing log — the §4.5 "moves
  behind the ControlPlane for attribution" requirement) and sends the
  ControlMsg; commands delegate.

- [ ] **Step 1: Failing tests**: transport play→stop leaves rev bumped
  twice, no history entries (assert via Task-17 stub: for now assert
  `committed.meta.transient == true`), loop Set round-trips through
  inverse, `transport://state` emitted once per call, NO
  `project://changed`.
- [ ] **Step 2: Implement; suite green.**
- [ ] **Step 3: Commit** —
  `git commit -m "feat(transport): transient transport ops; device selection attributed via ControlPlane"`

---

### Task 13: Engine-thread inversion — the `Committer`

Inventory rows 21-24: the engine control thread never holds the session
for writes again (§4.2). It gets the commit core (session + tables + emit,
minus the self-referential `EngineHandle`) and submits `Actor::Engine`
transactions; its two self-originated rebuilds enter the same accounting.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (extract `commit_core`),
  `src-tauri/src/control/session.rs` (nothing — transact unchanged),
  `src-tauri/src/audio/engine.rs` (five sites), `src-tauri/src/audio/mod.rs`
  (engine construction receives the Committer)
- Test: engine.rs tests (the headless-engine fixture at engine.rs:1433
  exists) + control/mod.rs

**Interfaces:**

```rust
// control/mod.rs
/// The commit core, shareable with the engine control thread. Owns
/// everything commit needs EXCEPT the EngineHandle: the engine executes
/// `rebuild` effects by calling its own rebuild directly (it IS the
/// engine), which keeps "at most one Rebuild per transaction" a claim
/// about ALL rebuilds (§4.4).
pub struct Committer {
    session: Arc<Mutex<Session>>,
    tables: SharedGraphTables,
    emit: Arc<EventEmitter>,           // EventEmitter becomes Arc-shared
}
impl Committer {
    pub fn commit_with_rebuild<F, R>(&self, meta: TxMeta, f: F, do_rebuild: R)
        -> Result<Committed, String>
    where F: FnOnce(&mut Tx<'_>) -> Result<(), String>, R: FnOnce() {
        /* transact → param/any_solo writes via tables → if effect.rebuild
           { do_rebuild() } → persist effect → project://changed emit
           (skipped for transient metas per Task 12's commit_with rule) */
    }
}
// ControlPlane::commit becomes a wrapper: self.committer.commit_with_rebuild(
//     meta, f, || self.engine.send(ControlMsg::Rebuild))
```

  Engine sites:
  - `open_output` (site 1): `committer.commit_with_rebuild(TxMeta::engine("sample rate").transient(), |tx| tx.apply(Set{Transport, SampleRate, …}), || self.rebuild())`.
  - `apply_end_policy` (site 2): transient Engine tx setting
    `TransportState` (+ keeps the RT atomic + `transport://state` emit it
    does today).
  - `start_recording` (site 3): transient Engine tx (`TransportState` →
    "recording").
  - `stop_recording` finalize (site 4): the recorded takes are registered
    AFTER I/O completes (§4.4 — "the op is the registration, never the
    recording itself"): ONE non-transient Engine tx applying
    `ClipAdd`×n + `Set{Transport, TransportState}`; rebuild via effect;
    `project::save` replaced by `persist.project`; `recording://state`
    emit stays.
  - `ensure_project` (site 5): calls `ControlPlane::ensure_project_epoch`
    via the Committer? No — epochs live on ControlPlane; give the engine a
    narrow `ensure_project: Arc<dyn Fn() -> Result<PathBuf, String> + Send + Sync>`
    closure installed at construction (the epoch fn bound over the
    ControlPlane Arc; lib.rs wires it after both exist). The engine thread
    calls it and never touches project fields itself.
- Deadlock audit (write it as a comment at the Committer definition): the
  engine control thread calling `transact` is safe because (a) `transact`
  never sends engine messages (grep-gated), (b) the only blocking
  request-reply INTO the engine happens from other threads and is handled
  by this same control loop — which is exactly why the engine must never
  `commit` while servicing a blocking request that the commit's effect
  would re-enter. Sites 1-5 are all fire-and-forget from the control
  loop's own turn; none holds a reply channel while committing.

- [ ] **Step 1: Failing tests**: headless-engine fixture — recording
  finalize produces a Committed with `Actor::Engine` + ClipAdd ops and one
  rebuild; auto-stop produces a transient Engine tx;
  `attribution_survives_commit` gains an `Actor::Engine` case (§7 test 5's
  "Actor::Engine appears where expected").
- [ ] **Step 2: Implement** (Committer extraction first — pure refactor,
  suite green — then sites 1-5 one at a time, suite between each).
- [ ] **Step 3: Grep-gate**: `grep -n 'session.lock()' src-tauri/src/audio/engine.rs`
  — remaining hits must be READS (rebuild/meters/targets); document each
  with a `// read-only:` comment. No write site remains.
- [ ] **Step 4: Full backend suite green.**
- [ ] **Step 5: Commit** —
  `git commit -m "feat(engine): the engine thread submits Actor::Engine transactions, never holds"`

---

### Task 14: Gesture IPC — `gesture_begin`/`gesture_end` + fader migration

Inventory row 31, ADR 0003's CLAP-style primitives. Explicit boundaries
from the frontend; between them, updates are transient commits coalesced
by `(op_kind, target, actor)`; `gesture_end` produces the ONE history
batch. Faders/pan are the first migration targets (round-2 §4.4 verbatim).
The 350 ms fallback for boundary-less callers is Task 17's history-side
merge (it needs the history stack to exist), noted there.

**Files:**
- Modify: `src-tauri/src/control/mod.rs` (GestureState + two commands),
  `src-tauri/src/lib.rs` (register additive commands),
  `src/lib/tauri.ts`, `src/lib/state/project.svelte.ts`,
  `src/lib/components/TrackHeader.svelte`
- Test: control/mod.rs tests + frontend TrackHeader/store test

**Interfaces:**

```rust
// control/mod.rs
struct OpenGesture {
    actor: Actor,
    run: String,                       // one run id for the whole gesture
    /// First-seen inverse per coalesce key — the gesture's baseline.
    baselines: Vec<(CoalesceKey, Op)>, // key = (op discriminant, ObjectRef, PropPath?)
    last: Vec<(CoalesceKey, Op)>,      // last-seen forward op per key
    label: String,
}
pub struct GestureState(Mutex<Option<OpenGesture>>);
```

  - `gesture_begin(label: String)` command → opens (errors if already open
    — one gesture at a time per window is the product reality; the error
    auto-closes the stale one first, committing its batch, so a missed
    pointerup can't wedge the channel).
  - While open, `ControlPlane::set_track_mix` (and any Set-emitting commit
    whose meta actor matches the gesture's) runs as a **transient** commit
    (document + RT effects immediate — RT-visible per §4.4) and folds its
    ops/inverses into the accumulator instead of history.
  - `gesture_end()` → closes, synthesizes one `Committed`-shaped history
    entry: ops = last-forward per key, inverses = first-baseline per key
    (reverse order), meta = non-transient with the gesture's run/label;
    hands it to the history sink (Task 17; until then, the entry is
    returned from a `pub(crate)` hook the test asserts on) and emits ONE
    `project://changed`.
  - Frontend: `TrackHeader` fader/pan `onpointerdown` →
    `backend.gestureBegin("gain drag")`; `oninput` unchanged (the
    per-event invokes now coalesce backend-side); `onpointerup`/
    `onpointercancel` → `backend.gestureEnd()`. Mute/solo/arm clicks stay
    boundary-less (single-shot ops don't need gestures).

- [ ] **Step 1: Failing backend test**: begin → 5× set_track_mix gain →
  end produces exactly ONE history-bound batch whose inverse restores the
  pre-gesture gain, and 5 transient commits' `project://changed` spam is
  reduced to one final emit (assert emit count via the fake emitter).
- [ ] **Step 2: Implement backend; suite green.**
- [ ] **Step 3: Frontend wiring + test** (pointerdown/up call the two
  bindings; jsdom pointer events per the existing component-test style).
- [ ] **Step 4: Both suites green.**
- [ ] **Step 5: Commit** —
  `git commit -m "feat(gestures): begin/end IPC with transient coalescing; faders emit gesture boundaries"`

---

### Task 15: `noteId` through the wire + IPC-type/schema honesty

Inventory row 33 (Plan B handoff) + the C/D stale-docs item. The TS
`MidiNote` gains the field the backend already persists; the accidental
spread-survival becomes contractual; the two stale schema docs and the
`schemaVersion: 1`-pinned TS `Project` type are brought current.

**Files:**
- Modify: `src/lib/types/ipc.ts` (`MidiNote.noteId`, `Project` v3 shape),
  `src/lib/components/pianoroll/PianoRoll.svelte` (new-note literal gets
  `noteId: 0` explicitly), `src/lib/state/generation.svelte.ts` (infill
  keeps context ids, fills get `noteId: 0`),
  `docs/ipc-schemas/project-v2.schema.json` → superseded by new
  `docs/ipc-schemas/project-v3.schema.json` (v2 file gets a header note
  pointing at v3 — marked, not deleted: it documents the migration
  source), `docs/ipc-schemas/midi-clip.schema.json` ($defs/persistedClip
  updated to the v3 content/placements rows)
- Test: frontend type-level test (a `MidiNote` literal without `noteId`
  must fail `tsc` — make the field REQUIRED in TS with `0` as the explicit
  mint sentinel, matching the backend's `#[serde(default)]` read
  tolerance); vitest for the infill merge.

- [ ] **Step 1: Add `noteId: number` (required) to `MidiNote`; fix every
  literal the compiler now flags** (`PianoRoll.svelte:156` note-draw →
  `noteId: 0`; `generation.svelte.ts:283-291` infill → context notes keep
  theirs, filled notes get `0`; any test fixtures).
- [ ] **Step 2: Failing vitest**: infill merge preserves context note ids
  and marks filled notes 0; paste path still sends source ids (backend
  keep-rule mints — assert the frontend doesn't strip them).
- [ ] **Step 3: `tsc --noEmit` + both suites green.**
- [ ] **Step 4: Write `project-v3.schema.json`** describing what
  `midi::persist` actually writes (schemaVersion 3, content[]/placements[]/
  lanes[]/meterMap/periodEvents/sectionTableRuleVersion — transcribe from
  `persist.rs:178-249`, cite it in the schema's description field);
  update `midi-clip.schema.json`'s persistedClip; update ipc.ts `Project`.
- [ ] **Step 5: Commit** —
  `git commit -m "feat(midi): noteId is contractual on the wire; v3 schema docs current"`

---

### Task 16: `steady_time` — one engine-global clock

The C/D handoff's ruling 3, bound to this plan: `clap_host.rs`'s per-node
`self.steady: u64` resets to 0 on node re-creation (instrument rebind,
sample-rate change, live-set re-entry) — the exact §3.5 hazard. Replace
with one engine-global steady sample counter, advanced once per block,
threaded to hosts.

**Files:**
- Modify: `src-tauri/src/audio/engine.rs` (global counter, advanced in the
  process callback's block prologue), `src-tauri/src/plugins/clap_host.rs`
  (nodes read the shared counter instead of self-counting),
  whatever `RtNode`-adjacent plumbing carries per-block context (find the
  existing per-block param struct the process path already passes — extend
  it, do NOT add a new global static)
- Test: clap_host/engine unit test

- [ ] **Step 1: Locate the per-block context** the RT process path hands
  nodes (`grep -n 'steady' src-tauri/src` + the process fn signatures);
  decide the carrier (an `AtomicU64` on `SharedRt` advanced by the RT
  callback is acceptable — it is engine state, and `SharedRt` is already
  the RT-shared atomics home).
- [ ] **Step 2: Failing test**: simulate node re-creation (the existing
  clap_host test fixtures) and assert `steady_time` passed to the host
  does NOT reset across the rebind (monotonic across two `process` calls
  spanning a rebuild).
- [ ] **Step 3: Implement; RT-audit**: no alloc, no lock on the RT path
  (Relaxed atomic add), `disable_release` conventions per the repo's RT
  test setup.
- [ ] **Step 4: Full backend suite green.**
- [ ] **Step 5: Commit** —
  `git commit -m "fix(rt): engine-global steady_time survives node re-creation (round-2 §3.5)"`

---

### Task 17: Gate E — Figma invariant, totality doc, and the log turns ON

The gate task. Order inside the task is the ADR-0003 order: (1) test 4
green, (2) inventory documented total, (3) ONLY THEN the journal +
undo/redo. Do not reorder.

**Files:**
- Create: `src-tauri/tests/figma_invariant.rs`, `docs/SIDE-CHANNEL-INVENTORY.md`,
  `src-tauri/src/control/history.rs` (History + JournalWriter)
- Modify: `src-tauri/src/control/mod.rs` (history/journal hooks in
  commit_core + epoch fns; `undo`/`redo` commands),
  `src-tauri/src/lib.rs` (register `undo`/`redo`),
  `docs/ipc-schemas/op-envelope.schema.json` (kind-pattern correction,
  marked), `src/lib/tauri.ts` + a minimal keyboard hook in `App.svelte`
  (Ctrl+Z/Ctrl+Shift+Z → invoke; chrome-only, thin-renderer-compliant)
- Test: `figma_invariant.rs`, history.rs unit tests, an attribution
  integration test

**Interfaces:**

```rust
// history.rs
pub struct HistoryEntry { pub label: String, pub actor: Actor, pub run: String,
    pub ops: Vec<Op>, pub inverses: Vec<Op>, pub at: std::time::Instant,
    /// Some(key) when every op in the batch shares one coalesce key —
    /// the 350ms boundary-less fallback merges consecutive same-key
    /// entries (round-2 §4.4: fallback, not mechanism).
    pub coalesce: Option<CoalesceKey> }
pub struct History { undo: Vec<HistoryEntry>, redo: Vec<HistoryEntry> }
impl History {
    pub fn record(&mut self, e: HistoryEntry);   // clears redo; applies the
        // 350ms same-key merge: if top-of-undo has the same key and
        // e.at - top.at < 350ms and both are single-key batches, fold e
        // into top (keep top's inverses, take e's ops).
    pub fn pop_undo(&mut self) -> Option<HistoryEntry>;
    pub fn pop_redo(&mut self) -> Option<HistoryEntry>;
    pub fn clear(&mut self);                     // epoch boundary
}
// JournalWriter: opened at epoch (project dir known), append-only
// <dir>/journal.ndjson; one line per NON-transient committed batch:
// {"v":1,"rev":N,"baseRev":N-1,"actor":…,"run":…,"label":…,"ops":[…]}
// plus epoch records {"v":1,"epoch":"open"|"create"|"saveAs"|"ensure",
// "rev":0,"dir":…}. Flush per line; no fsync (journal is a log, the
// project file is the snapshot — Plan F owns durability policy).
```

- `commit_core`: after effects, if `!meta.transient` → `history.record` +
  `journal.append`; `undo` command: pop → `commit` the inverses with
  `TxMeta { actor: Actor::User, run: entry.run.clone(), label: format!("undo: {}", entry.label), transient: false }`
  but routed so the resulting commit lands on the REDO stack instead of
  re-entering undo (a `history_mode` flag on the call, not a thread-local);
  redo mirrors. Undo/redo commits ARE journaled (they are mutations;
  replay must see them).
- **Figma test** (`figma_invariant.rs`) — §7 test 4 verbatim: build the
  fixture; apply a scripted mixed session (mix Sets, clip add/move, midi
  notes, tempo, automation lane, plugin param — every family from this
  plan); `undo`; run EVERY declared read path (`get_project_state`-core,
  midi read, automation read, waveform-tile read on a real clip,
  transport snapshot, plugin list/params, export capabilities); `redo`;
  assert canonical snapshot (store+midi+plugins+automation, watermark
  masked per ruling 3) equals the pre-undo snapshot, AND a second
  pure-reader sweep: each read path bracketed by snapshots, byte-equal.
- **Attribution test** (§7 test 5, "from the first enabled day"): scripted
  mixed session with User + Agent + Engine + System actors; read
  journal.ndjson back; assert every line carries a resolvable actor/run
  and the expected actors appear (engine finalize → `"engine"`, MCP tool →
  `{"agent":{"tool":…}}`, stem sink → `"system"`).
- **`docs/SIDE-CHANNEL-INVENTORY.md`**: the table from this plan's
  inventory section, each row updated to its landed mechanism +
  commit-verified file:line, with the four carve-outs/rulings restated.
  This is the document Gate E's "checked against dossier 10 §2.3 + review
  additions" clause audits.
- **Envelope schema correction** (scope ruling 1): change the `kind`
  pattern in `op-envelope.schema.json` to `^[a-z][a-zA-Z0-9]*$` with a
  `$comment` marking the correction ("2026-08: implementation kinds are
  single camelCase tokens; dotted form was D-03 draft, never shipped —
  corrected per ADR 0007, not silently").

- [ ] **Step 1: Write `figma_invariant.rs` + the pure-reader sweep; run —
  MUST PASS already** (every prior task earned it). If it fails, STOP:
  the failing path is an inventory hole; fix the path (new mini-task),
  never weaken the test.
- [ ] **Step 2: Write `SIDE-CHANNEL-INVENTORY.md`**; cross-check every row
  against `grep -rn 'session.lock()\|\.lock()' src-tauri/src` — any
  document-writing lock site outside session.rs/commit_core/epoch fns is
  a hole; fix before proceeding.
- [ ] **Step 3: History + journal + undo/redo, TDD** (failing tests: undo
  round-trip via the commands incl. redo; 350ms same-key merge; transient
  excluded; epoch clears + rotates journal; journal lines parse against
  the corrected envelope schema).
- [ ] **Step 4: Frontend Ctrl+Z/Ctrl+Shift+Z** → `undo`/`redo` invokes
  (App.svelte keydown, guard: not when a text input is focused).
- [ ] **Step 5: Full backend + frontend suites green; update dated counts**
  in README.md:389 + CONTRIBUTING.md:28.
- [ ] **Step 6: Commit** —
  `git commit -m "feat(gate-e): Figma invariant green, inventory total — the op log turns on (journal + undo/redo)"`

---

### Task 18: Close-out — handoff + next-prompt

**Files:**
- Modify: `docs/PHASE4-PLAN.md` (new "Plan E handoff" section: scope
  rulings 1-9 verbatim, carry-forwards, dated counts),
  `next-prompt.md` (rewritten for Plan F — history storage, §6: COW
  B-tree, version-graph retention, replay-only nodes at 64 KB per
  bulkbench, janitor thread; carry-forwards: snapshot-rebuild deferral NOW
  UNBLOCKABLE by F, panic rollback, journal durability policy),
  `README.md`/`CONTRIBUTING.md` if counts changed after Task 17
- [ ] **Step 1: Write the handoff** (every ruling, every deferral, the
  new test counts, the inventory doc pointer).
- [ ] **Step 2: Update next-prompt.md** in the same commit (the standing
  convention from the C/D close-out).
- [ ] **Step 3: Full suites green one last time; commit** —
  `git commit -m "docs: Plan E close-out — handoff, inventory pointer, next stage (Plan F)"`

---

## Self-review notes (writing-plans skill, run against this plan before Task 1 starts)

1. **Spec coverage.** §4.5 inventory → rows 1-30 all have tasks or marked
   rulings; §4.2 engine inversion → Task 13; §4.3 two tiers →
   `add_track_tx` (Task 7) + existing runtime guard; §4.4 gestures → Task
   14, effects → Tasks 2/9, inverses/restore-from-blob → Task 9,
   prepare-outside → Tasks 7/8/9; §4.6 versioned format + one-module paths
   → PropPath stays the single module, envelope correction in Task 17; §7
   test 4 → Task 17 step 1, test 5 → Task 17 attribution test. PHASE4-PLAN
   E-row items: "all 14+ paths" (rows), "engine-thread inversion" (13),
   "gesture begin/end IPC" (14), "clip.move op + command" (3/4),
   "id-preserving piano roll" (15), "mutating readers become pure" (6/10),
   "remaining three stores into Session" (9/10 + ruling 5 for the third —
   the sampler bank has no document half at HEAD; §4.1's own live-state
   exclusion covers it, recorded not silent).
2. **Known risks, named:** (a) Task 9 is the widest diff (55 plugin tests
   churn) — it is deliberately AFTER the midi/epoch tasks so the persist/
   effect machinery is proven first; (b) Task 13's deadlock surface is
   audited in-task with a written invariant comment; (c) test-count
   volatility: counts are re-verified at every task boundary, and the
   384/100 baseline is dated above; (d) `commit_with(emit_project_changed)`
   (Task 12) is a pragmatic wart — folding emit choice into the effect
   would be cleaner; deferred to keep the frozen-event contract change
   out of this plan, noted for the handoff.
3. **Type consistency check:** `PersistEffect` fields referenced in Tasks
   5/7/8/9/10 match Task 2's definition; `add_track_tx` signature (Task 7)
   matches its Task 8 uses; `Committer::commit_with_rebuild` (Task 13)
   subsumes Task 12's `commit_with` (implementers: `commit_with` becomes a
   thin wrapper when 13 lands — the earlier task's tests keep passing);
   `HistoryEntry.coalesce`'s `CoalesceKey` is Task 14's key type — Task 14
   defines it, Task 17 imports it.
4. **Placeholder scan:** the step-1 test blocks in Tasks 5/8/9/10 use
   comment-sketch bodies where the fixture pattern already exists in the
   named file — each names the concrete fixture to copy
   (`session_owns_both_stores_under_one_lock`,
   `set_track_mix_emits_project_changed_with_updated_tracks`, engine.rs:1433,
   hum.rs:638). That is a deliberate density trade against an existing,
   named in-repo example, not a TBD: the implementer copies the cited
   fixture and fills the quoted assertions.
5. **Ordering sanity:** 1→2 are enablers; 3→4 audio; 5→6→7 midi (epochs
   before wrappers so lazy-sync deletion never leaves a stale window);
   8 needs 3+5+7; 9→10 store moves ride proven machinery; 11 needs 8;
   12→13 transport before engine (auto-stop op exists); 14 needs 12's
   transient plumbing exercised; 15/16 independent; 17 last, 18 paperwork.

## Execution note

Executed from the `zrythm-arch` worktree, branch `plan-e-side-channels`
(cut from `origin/main` at `7606d8d`, post-PR-#8; owner rule: new branches
start at `origin/main`, continuing branches merge it in). Push to a new PR
when Task 1 lands green; push at every task boundary after. Foreground
test runs only, `timeout`-guarded. SDD ledger (gitignored):
`.superpowers/sdd/plan-e/progress.md`. Execution mode (solo vs
subagent-driven) is the owner's call at run start — recorded here once
made: **[filled in at execution]**.


