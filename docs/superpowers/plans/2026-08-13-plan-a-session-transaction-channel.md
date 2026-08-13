# Plan A — Session + the Transaction Channel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One `Session` object owning the document (Store + MidiStore for now), mutable only inside `session.transact(meta, |tx| …)`, producing ops with inverses, actor/run attribution, a revision counter, and folded engine effects — proven on a first vertical slice (track mix, add/remove track).

**Architecture:** A new `control::session` module wraps the two stores `ControlPlane` already holds behind ONE `parking_lot::Mutex`. `Tx` is the only path to `&mut` document state; `apply` computes each op's inverse and effect; effects (rebuild, param writes, events) execute after the lock is released. Existing `control::ops` functions become the internals `Tx::apply` calls. Commands keep their frozen names and become wrappers.

**Tech Stack:** Rust (src-tauri crate), parking_lot 0.12 (`!Send` guards — do not enable `send_guard`), serde, uuid, proptest (add as dev-dependency).

**Spec:** `docs/CORE-REDESIGN-ROUND-2.md` §4 (+§2 for id families used in op addresses); ADR 0003. Orchestration: `docs/PHASE4-PLAN.md`.

## Global Constraints

- The op log stays **dark**: no journal file, no undo UI in this plan (PHASE4-PLAN rule 1).
- Frozen command/event names unchanged; bodies become wrappers (rule 3).
- No blocking `EngineHandle::request` inside `transact` (round-2 §4.2). `engine.send` of folded effects happens **after** the session lock is dropped.
- parking_lot stays 0.12 without `send_guard`; no lock held across `.await` (compile-enforced today — keep it so).
- All existing tests keep passing: `cargo test --manifest-path src-tauri/Cargo.toml` — 277 at baseline.
- Op schema carries `OP_FORMAT_VERSION: u16 = 1` from day one (§4.6) even though nothing is persisted yet.

---

### Task 1: `Session` — one lock over the document

**Files:**
- Create: `src-tauri/src/control/session.rs`
- Modify: `src-tauri/src/control/mod.rs` (ControlPlane fields `store: Arc<Mutex<Store>>` + `midi: Arc<Mutex<MidiStore>>` at lines ~96–105 → one `session: Arc<Mutex<Session>>`; constructor at ~118–134; every `self.store.lock()` / `self.midi.lock()` in the file)
- Modify: every other holder of the two Arcs — find them all with `rg "Arc<Mutex<Store>>|Arc<Mutex<MidiStore>>" src-tauri/src` (audio::init constructs them; `audio/mod.rs`, `midi/mod.rs`, `engine.rs` and the sinks receive clones)
- Test: `src-tauri/src/control/session.rs` (unit tests inline, repo style)

**Interfaces:**
- Produces: `pub struct Session { pub store: Store, pub midi: MidiStore }`, `Session::new(store: Store, midi: MidiStore) -> Session`. Holders that previously took the two Arcs take `Arc<parking_lot::Mutex<Session>>` and reach fields as `session.lock().store` / `.midi`.
- Consumes: existing `Store` (`audio/types.rs:197`), `MidiStore` (`midi/types.rs`).

- [ ] **Step 1: Write the failing test**

```rust
// at the bottom of src-tauri/src/control/session.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_owns_both_stores_under_one_lock() {
        let s = Session::new(Store::default(), MidiStore::default());
        let m = parking_lot::Mutex::new(s);
        let mut g = m.lock();
        // Cross-store write under ONE guard — impossible with the old
        // two-mutex layout ("store first, then midi, never both at once").
        g.store.transport.tempo_bpm = 128.0;
        g.midi.ppq = 960;
        assert_eq!(g.store.transport.tempo_bpm, 128.0);
        assert_eq!(g.midi.ppq, 960);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path src-tauri/Cargo.toml session_owns_both_stores -- --nocapture`
Expected: FAIL to compile — `Session` not defined.

- [ ] **Step 3: Write minimal implementation**

```rust
// src-tauri/src/control/session.rs
//! The document. `&mut` access is (after Task 3) only reachable through
//! `Session::transact`. Round-2 §4.1: the remaining three stores
//! (automation, plugin rows, sampler bank) move in during Plan E.
use crate::audio::types::Store;
use crate::midi::types::MidiStore;

pub struct Session {
    pub store: Store,
    pub midi: MidiStore,
}

impl Session {
    pub fn new(store: Store, midi: MidiStore) -> Self {
        Self { store, midi }
    }
}
```

Register the module in `src-tauri/src/control/mod.rs`: `pub mod session;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path src-tauri/Cargo.toml session_owns_both_stores`
Expected: PASS

- [ ] **Step 5: Mechanical migration of all holders**

Replace the two Arcs with one `Arc<Mutex<Session>>` end to end. Rules:
- `audio::init` (or wherever `Arc::new(Mutex::new(Store::default()))` is constructed — find with `rg "Mutex::new\(Store"`) builds `Arc<Mutex<Session>>` instead and hands clones to everyone who took either store.
- Every `store.lock()` becomes `session.lock()`, field access `.store.…`; every `midi.lock()` becomes `session.lock()`, field access `.midi.…`.
- Sites that previously locked BOTH in sequence (e.g. `ControlPlane::project_state`, `control/mod.rs:144–154`; `set_tempo_map`, `midi/mod.rs:203–214`) now take ONE guard for the whole block — delete the "store first, then midi" ordering comments (`control/hum.rs:151`, `control/mod.rs` near 366) as no longer true.
- Do NOT change behavior anywhere; this is a pure locking refactor.

- [ ] **Step 6: Run the full suite**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: all 277 + 1 new PASS. Fix compile fallout until green; no test's assertion may change.

- [ ] **Step 7: Commit**

```bash
git add -A src-tauri
git commit -m "feat(control): Session owns Store+MidiStore behind one lock"
```

---

### Task 2: Op vocabulary, meta, and the format version

**Files:**
- Create: `src-tauri/src/control/op.rs`
- Modify: `src-tauri/src/control/mod.rs` (`pub mod op;`)
- Test: inline in `op.rs`

**Interfaces:**
- Produces (later tasks and Plan E rely on these exact names):

```rust
pub const OP_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Op {
    /// Property-addressed change (round-2 §4: Set { object, path, from, to }).
    Set { object: ObjectRef, path: PropPath, from: serde_json::Value, to: serde_json::Value },
    /// Structural: create a track (payload = full row, so inverse is Remove).
    TrackAdd { track: crate::audio::types::TrackState, index: usize },
    /// Structural: remove a track. Payload kept so the inverse can restore
    /// identity + row byte-identically (gate test 2 arrives in Plan B).
    TrackRemove { track: crate::audio::types::TrackState, index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "family", content = "id", rename_all = "camelCase")]
pub enum ObjectRef { Track(String), Clip(String), MidiClip(String) }

/// Property paths are a closed enum, not strings — renaming a variant is a
/// compile error at every use site, which is the §4.6 anti-drift rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PropPath { Gain, Pan, Muted, Soloed, Armed }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Actor { User, Agent { tool: String }, Engine, System }

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxMeta {
    pub actor: Actor,
    /// Correlation id for a multi-transaction run (an agent task, an import).
    pub run: String,
    pub label: String,
}
```

- [ ] **Step 1: Write the failing round-trip test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_serde_round_trips_and_carries_version() {
        assert_eq!(OP_FORMAT_VERSION, 1);
        let op = Op::Set {
            object: ObjectRef::Track("t-1".into()),
            path: PropPath::Gain,
            from: serde_json::json!(1.0),
            to: serde_json::json!(0.5),
        };
        let s = serde_json::to_string(&op).unwrap();
        // camelCase tag on the wire, per the repo's IPC convention.
        assert!(s.contains("\"kind\":\"set\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, op);
    }

    #[test]
    fn meta_requires_actor_and_run() {
        // actor/run are non-optional by construction: TxMeta has no Default
        // and no Option fields — this test just locks the shape.
        let m = TxMeta { actor: Actor::Engine, run: "r-1".into(), label: "auto-stop".into() };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("engine") && s.contains("r-1"));
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --manifest-path src-tauri/Cargo.toml op_serde_round_trips` → compile FAIL (module missing).

- [ ] **Step 3: Implement** — the code in **Interfaces** verbatim, plus `pub mod op;` in `control/mod.rs`.

- [ ] **Step 4: Run to verify PASS** — same command.

- [ ] **Step 5: Commit** — `git add -A src-tauri && git commit -m "feat(control): op vocabulary v1 with actor/run meta"`

---

### Task 3: `transact` — the only door, with honest anti-nesting

**Files:**
- Modify: `src-tauri/src/control/session.rs`
- Test: inline

**Interfaces:**
- Produces:

```rust
pub struct Tx<'s> { /* private fields: &mut Session, collected ops+inverses, folded effect */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineEffect { pub rebuild: bool }   // grows in Task 5

pub struct Committed {
    pub rev: u64,
    pub ops: Vec<crate::control::op::Op>,
    pub inverses: Vec<crate::control::op::Op>, // reverse order, ready to apply
    pub effect: EngineEffect,
    pub meta: crate::control::op::TxMeta,
}

impl Session {
    /// The ONLY way to mutate the document. Returns Err and leaves the
    /// session untouched if the closure fails (inverses rolled back).
    pub fn transact<F>(this: &parking_lot::Mutex<Session>, meta: TxMeta, f: F)
        -> Result<Committed, String>
    where F: FnOnce(&mut Tx<'_>) -> Result<(), String>;
}
```

- `Tx::apply(&mut self, op: Op) -> Result<(), String>` — applies, records op + computed inverse. `apply_raw` is a private free function; nothing else in the crate mutates `Session` fields after Plan E (this plan: the A-slice sites).
- Nesting: a `thread_local!` in-transaction flag; re-entering `Session::transact` on the same thread **panics** with `"nested transact — compose with &mut Tx instead"` (round-2 §4.3: honest runtime trap, since the deadlock alternative is silent).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn transact_commits_and_bumps_rev() {
    let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
    let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "noop".into() };
    let c1 = Session::transact(&m, meta.clone(), |_tx| Ok(())).unwrap();
    let c2 = Session::transact(&m, meta, |_tx| Ok(())).unwrap();
    assert_eq!(c2.rev, c1.rev + 1);
}

#[test]
fn failing_closure_leaves_session_untouched_and_no_rev() {
    let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
    let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "fail".into() };
    let before = Session::transact(&m, meta.clone(), |_| Ok(())).unwrap().rev;
    let r = Session::transact(&m, meta.clone(), |_tx| Err("boom".into()));
    assert!(r.is_err());
    let after = Session::transact(&m, meta, |_| Ok(())).unwrap().rev;
    assert_eq!(after, before + 1, "failed tx must not consume a revision");
}

#[test]
#[should_panic(expected = "nested transact")]
fn nested_transact_panics_not_deadlocks() {
    let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
    let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "outer".into() };
    let _ = Session::transact(&m, meta.clone(), |_tx| {
        let _ = Session::transact(&m, meta.clone(), |_| Ok(()));
        Ok(())
    });
}
```

- [ ] **Step 2: Run to verify FAIL** (compile: `transact` missing).

- [ ] **Step 3: Implement minimal `transact`**

```rust
thread_local! {
    static IN_TX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Tx<'s> {
    session: &'s mut Session,
    ops: Vec<Op>,
    inverses: Vec<Op>,
    effect: EngineEffect,
}

impl Session {
    pub fn transact<F>(this: &parking_lot::Mutex<Session>, meta: TxMeta, f: F)
        -> Result<Committed, String>
    where F: FnOnce(&mut Tx<'_>) -> Result<(), String>,
    {
        IN_TX.with(|t| {
            if t.get() { panic!("nested transact — compose with &mut Tx instead"); }
            t.set(true);
        });
        let _reset = scopeguard(|| IN_TX.with(|t| t.set(false)));
        let mut guard = this.lock();
        let mut tx = Tx { session: &mut guard, ops: vec![], inverses: vec![], effect: EngineEffect::default() };
        match f(&mut tx) {
            Ok(()) => {
                let (ops, mut inverses, effect) = (tx.ops, tx.inverses, tx.effect);
                inverses.reverse();
                guard.rev += 1;
                Ok(Committed { rev: guard.rev, ops, inverses, effect, meta })
            }
            Err(e) => {
                // Rollback: run collected inverses in reverse — the same
                // code path undo will use (round-2 §4, "inverse produced by
                // apply, never guessed").
                let inv: Vec<Op> = tx.inverses.drain(..).rev().collect();
                for op in inv { apply_raw(tx.session, &op).expect("rollback must not fail"); }
                Err(e)
            }
        }
    }
}

/// Tiny local scope guard (avoid a new dependency).
fn scopeguard<F: FnMut()>(f: F) -> impl Drop { struct G<F: FnMut()>(F); impl<F: FnMut()> Drop for G<F> { fn drop(&mut self) { (self.0)() } } G(f) }
```

Add `pub(crate) rev: u64` (init 0) to `Session`; `apply_raw` exists as a stub returning `Err("no ops yet")` until Task 4.

- [ ] **Step 4: Run all three tests to verify PASS.**

- [ ] **Step 5: Commit** — `git commit -am "feat(control): Session::transact with rollback and nesting trap"`

---

### Task 4: The first property-addressed op — track mix as `Set`

**Files:**
- Modify: `src-tauri/src/control/session.rs` (`apply_raw`, `Tx::apply`)
- Modify: `src-tauri/src/control/op.rs` (nothing new — uses Task 2 shapes)
- Test: inline in `session.rs`

**Interfaces:**
- Consumes: `ops::apply_track_mix(store, params, changes)` (`control/ops.rs:47`) — stays the arithmetic core; `apply_raw` calls a new narrower helper.
- Produces: `Tx::apply(Op) -> Result<(), String>` working for `Op::Set` on `PropPath::{Gain,Pan,Muted,Soloed,Armed}` against `ObjectRef::Track`; inverse = `Set` with `from`/`to` swapped, `from` read from the store **by apply**, not trusted from the caller.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn set_gain_applies_and_inverse_restores() {
    let m = parking_lot::Mutex::new(session_with_one_track("t-1")); // helper: Store with one TrackState id "t-1", gain 1.0
    let meta = user_meta("set gain");
    let c = Session::transact(&m, meta.clone(), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Track("t-1".into()),
            path: PropPath::Gain,
            from: serde_json::json!(1.0),   // advisory; apply re-reads truth
            to: serde_json::json!(0.25),
        })
    }).unwrap();
    assert_eq!(m.lock().store.tracks[0].gain, 0.25);
    // Undo = apply the recorded inverses through a new transaction.
    Session::transact(&m, user_meta("undo"), |tx| {
        for op in c.inverses.clone() { tx.apply(op)?; }
        Ok(())
    }).unwrap();
    assert_eq!(m.lock().store.tracks[0].gain, 1.0);
}

#[test]
fn move_and_move_back_elides_to_noop_history() {
    // §4: property-addressed ops coalesce; from==to after fold drops out.
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let c = Session::transact(&m, user_meta("wiggle"), |tx| {
        tx.apply(set_gain("t-1", 0.5))?;
        tx.apply(set_gain("t-1", 1.0))?;   // back to original
        Ok(())
    }).unwrap();
    assert!(c.ops.is_empty(), "net-noop batch must fold to zero ops, got {:?}", c.ops);
}

#[test]
fn unknown_track_fails_whole_batch_atomically() {
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let before_gain = m.lock().store.tracks[0].gain;
    let r = Session::transact(&m, user_meta("bad"), |tx| {
        tx.apply(set_gain("t-1", 0.1))?;
        tx.apply(set_gain("nope", 0.2))?;  // fails → rollback
        Ok(())
    });
    assert!(r.is_err());
    assert_eq!(m.lock().store.tracks[0].gain, before_gain);
}
```

(Test helpers `session_with_one_track`, `user_meta`, `set_gain` are 5-line
private fns in the test module — write them, don't hand-wave them.)

- [ ] **Step 2: Run to verify FAIL** (apply_raw stub errors).

- [ ] **Step 3: Implement `apply_raw` for `Set` + coalescing fold**

```rust
fn apply_raw(s: &mut Session, op: &Op) -> Result<Op, String> {
    match op {
        Op::Set { object: ObjectRef::Track(id), path, to, .. } => {
            let t = s.store.tracks.iter_mut().find(|t| &t.id == id)
                .ok_or_else(|| format!("unknown track: {id}"))?;
            let from_now = read_prop(t, *path);           // truth, not caller's `from`
            write_prop(t, *path, to)?;                    // clamps like ops.rs does
            Ok(Op::Set { object: ObjectRef::Track(id.clone()), path: *path,
                         from: to.clone(), to: from_now }) // the inverse
        }
        _ => Err("op not yet supported".into()),
    }
}
```

`Tx::apply` pushes op + returned inverse. On commit, fold: group `Set` ops
by `(object, path)`, keep first `from`/last `to`, drop entries where they
are equal. `read_prop`/`write_prop` are small match-on-`PropPath` helpers
over `TrackState` fields (`gain: f32` clamped to schema range like
`ops::apply_track_mix` does, `pan`, `muted`, `soloed`, `armed`).

**Note:** param-table/RT propagation stays where it is for now — the
existing command still calls `ops::apply_track_mix`. Task 6 folds the
engine effect; Task 7 rewires the command. Do not touch `ParamTable` here.

- [ ] **Step 4: Run to verify PASS.**

- [ ] **Step 5: Commit** — `git commit -am "feat(control): property-addressed Set ops with derived inverses and no-op elision"`

---

### Task 5: Structural ops — TrackAdd / TrackRemove with inverse payloads

**Files:**
- Modify: `src-tauri/src/control/session.rs` (`apply_raw` arms), `src-tauri/src/control/op.rs` (no new types)
- Test: inline

**Interfaces:**
- Consumes: `ops::add_track(store, params, name, kind)` (`control/ops.rs:99`) — its body moves into a param-free core `fn insert_track(store: &mut Store, track: TrackState, index: usize)`; the old signature remains as a wrapper (it also allocates the slot + resets params — that half stays in the wrapper for now, moving to effects in Plan B).
- Produces: `apply_raw` arms for `Op::TrackAdd`/`Op::TrackRemove`; inverse of Add is Remove with the same payload and vice versa. `TrackRemove` in this plan does **not** free the slot inside the op (round-2 §2.4: slots become derived; until Plan B, slot bookkeeping happens in the effect layer of the command wrapper, keeping today's behavior).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn remove_then_inverse_restores_row_byte_identically() {
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let row = m.lock().store.tracks[0].clone();
    let c = Session::transact(&m, user_meta("remove"), |tx| {
        tx.apply(Op::TrackRemove { track: row.clone(), index: 0 })
    }).unwrap();
    assert!(m.lock().store.tracks.is_empty());
    Session::transact(&m, user_meta("undo"), |tx| {
        for op in c.inverses.clone() { tx.apply(op)?; }
        Ok(())
    }).unwrap();
    assert_eq!(m.lock().store.tracks[0], row, "same id, same everything");
}

#[test]
fn add_remove_batch_is_atomic_with_mixed_ops() {
    let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
    let snapshot = (m.lock().store.tracks.clone(), m.lock().store.clips.clone());
    let new_row = test_track("t-2");
    let r = Session::transact(&m, user_meta("mixed-fail"), |tx| {
        tx.apply(Op::TrackAdd { track: new_row, index: 1 })?;
        tx.apply(set_gain("t-2", 0.5))?;
        tx.apply(set_gain("ghost", 0.1))?;   // fails the batch
        Ok(())
    });
    assert!(r.is_err());
    let now = (m.lock().store.tracks.clone(), m.lock().store.clips.clone());
    assert_eq!(now, snapshot, "rollback must undo the structural op too");
}
```

- [ ] **Step 2: Run to verify FAIL.**
- [ ] **Step 3: Implement the two `apply_raw` arms** (`insert_track` at index; remove by id at recorded index, returning the mirrored inverse). `TrackState` needs `PartialEq` — derive it if missing (check `audio/types.rs:103`).
- [ ] **Step 4: Run to verify PASS.**
- [ ] **Step 5: Commit** — `git commit -am "feat(control): structural track ops with payload-carrying inverses"`

---

### Task 6: Effect descriptors — one transaction, at most one Rebuild, zero engine calls under the lock

**Files:**
- Modify: `src-tauri/src/control/session.rs` (fold effects per op), `src-tauri/src/control/mod.rs` (a `ControlPlane::commit` helper that runs a transaction then executes effects)
- Test: inline in `control/mod.rs` test module with a mock

**Interfaces:**
- Produces:

```rust
pub struct EngineEffect { pub rebuild: bool, pub param_writes: Vec<(usize /*slot*/, PropPath, f32)> }

impl ControlPlane {
    /// Runs a transaction, then — with the session lock RELEASED —
    /// executes the folded effect (param writes to ParamTable, at most one
    /// ControlMsg::Rebuild) and emits exactly one notification event.
    pub fn commit<F>(&self, meta: TxMeta, f: F) -> Result<Committed, String>
    where F: FnOnce(&mut Tx<'_>) -> Result<(), String>;
}
```

- Effect folding rules in `apply_raw` (returned alongside the inverse, or set on `Tx`): `Set{Gain|Pan|Muted|Soloed|Armed}` → `param_writes` entry when the track has a slot (mirrors what `ops::apply_track_mix` writes today — read its body at `control/ops.rs:47–95` and reproduce the same `ParamTable` calls, including the `any_solo` recompute); `TrackAdd`/`TrackRemove` → `rebuild = true`. Multiple structural ops still fold to ONE rebuild.
- Notification: `(self.emit)("project://changed", …)` once per committed transaction carrying `{rev, label, actor}` — additive JSON, no schema break (D-06 readers ignore unknowns).

- [ ] **Step 1: Write the failing test (mock the engine channel)**

The test constructs `ControlPlane` with a stub `EngineHandle`… **check**: if `EngineHandle` is a concrete struct without a test constructor (`audio/engine.rs`), add `#[cfg(test)] EngineHandle::for_tests() -> (EngineHandle, std::sync::mpsc::Receiver<ControlMsg>)` that records sent messages instead of driving an engine. Then:

```rust
#[test]
fn three_ops_one_rebuild_one_event() {
    let (plane, engine_rx, events) = test_plane_with_tracks(&["t-1"]); // helper building ControlPlane with for_tests handle + Vec-capturing EventEmitter
    plane.commit(user_meta("batch"), |tx| {
        tx.apply(Op::TrackAdd { track: test_track("t-2"), index: 1 })?;
        tx.apply(Op::TrackAdd { track: test_track("t-3"), index: 2 })?;
        tx.apply(set_gain("t-1", 0.5))?;
        Ok(())
    }).unwrap();
    let rebuilds = engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count();
    assert_eq!(rebuilds, 1, "two structural ops must fold to one Rebuild");
    assert_eq!(events.lock().iter().filter(|(n, _)| n == "project://changed").count(), 1);
}
```

- [ ] **Step 2: Run to verify FAIL.**
- [ ] **Step 3: Implement `commit`** — transact under the lock; drop the guard; then param writes → `self.params`, `if effect.rebuild { self.engine.send(ControlMsg::Rebuild) }`, then the single event. **Grep-gate:** `rg "engine\.(send|request)" src-tauri/src/control/session.rs` must return nothing — the session module never talks to the engine.
- [ ] **Step 4: Run to verify PASS.**
- [ ] **Step 5: Commit** — `git commit -am "feat(control): commit executes folded effects after the lock, one rebuild max"`

---

### Task 7: Wire the frozen commands of the A-slice through the channel

**Files:**
- Modify: `src-tauri/src/audio/mod.rs` — `set_track_gain`/`set_track_pan`/`set_track_mute`/`set_track_solo`/`set_track_arm`/`set_track_mix` path (today they delegate to `ControlPlane`/`ops::apply_track_mix`) and `add_track` (`audio/mod.rs:360–366`) + `remove_track` (`audio/mod.rs:369–379` — the review's "worst asymmetry" dies here)
- Modify: `src-tauri/src/control/mod.rs` — the ControlPlane methods those commands call (`apply_track_mix`, `add_track`) become `commit`-based; add `remove_track`
- Modify: `src-tauri/src/mcp/tools.rs` — `set_track_mix`/`add_track` tool paths get `Actor::Agent { tool }` meta (mechanical: they already call ControlPlane methods; add a meta parameter defaulting to `Actor::User` for Tauri and `Agent` for MCP)
- Test: existing suite is the guard; plus one new integration test

**Interfaces:**
- Consumes: `ControlPlane::commit` (Task 6), ops (Tasks 4–5).
- Produces: `ControlPlane::remove_track(&self, id: &str, meta: TxMeta) -> Result<(), String>` — store mutation via `Op::TrackRemove`, slot free + rebuild in the effect layer (behavior identical to today's command; the asymmetry with `add_track` is gone because both now run through commit).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn remove_track_goes_through_the_channel_and_rebuilds_once() {
    let (plane, engine_rx, _events) = test_plane_with_tracks(&["t-1", "t-2"]);
    plane.remove_track("t-1", user_meta("remove track")).unwrap();
    assert!(plane.session().lock().store.tracks.iter().all(|t| t.id != "t-1"));
    assert_eq!(engine_rx.try_iter().filter(|m| matches!(m, ControlMsg::Rebuild)).count(), 1);
}
```

- [ ] **Step 2: Run to verify FAIL** (`remove_track` missing on ControlPlane).
- [ ] **Step 3: Implement + rewire the command bodies.** Command signatures, names, and reply shapes are FROZEN — only bodies change to call the ControlPlane methods. `remove_track`'s Tauri body (`audio/mod.rs:369–379`) shrinks to a `plane.remove_track(&id, meta)` call. Keep the slot-free in the effect layer this plan (Plan B derives slots).
- [ ] **Step 4: Run the FULL suite** — `cargo test --manifest-path src-tauri/Cargo.toml` all green (277 baseline + new); `npm test` still 17.
- [ ] **Step 5: Commit** — `git commit -am "feat(control): A-slice commands (mix/add/remove track) run through the transaction channel"`

---

### Task 8: The property-test gate (round-2 §7 tests 1, 3, 5 on the A-slice)

**Files:**
- Modify: `src-tauri/Cargo.toml` (`[dev-dependencies] proptest = "1"`)
- Create: `src-tauri/tests/channel_properties.rs`

**Interfaces:**
- Consumes: everything above. This task is the Gate A check from `docs/PHASE4-PLAN.md`.

- [ ] **Step 1: Write the property tests**

```rust
use proptest::prelude::*;
// Strategy: a Vec<Op> over 3 seeded tracks — gains/pans in range, adds with
// fresh uuids, removes of existing ids (generate against a model to keep
// batches applicable).

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Gate test 1: apply all, undo all, byte-identical state.
    #[test]
    fn undo_round_trip(batches in arb_op_batches(1..20)) {
        let m = fresh_session_3_tracks();
        let before = snapshot(&m);            // serde_json of Store+MidiStore
        let mut undo_stack = vec![];
        for b in batches {
            if let Ok(c) = Session::transact(&m, user_meta("p"), |tx| { for op in b { tx.apply(op)?; } Ok(()) }) {
                undo_stack.push(c.inverses);
            } // Err = rolled back = state unchanged; nothing pushed.
        }
        for inv in undo_stack.into_iter().rev() {
            Session::transact(&m, user_meta("undo"), |tx| { for op in inv { tx.apply(op)?; } Ok(()) }).unwrap();
        }
        prop_assert_eq!(snapshot(&m), before);
    }

    /// Gate test 3: a failing batch leaves the session exactly as it was.
    #[test]
    fn atomicity(prefix in arb_op_batches(1..5)) {
        let m = fresh_session_3_tracks();
        let before = snapshot(&m);
        let _ = Session::transact(&m, user_meta("f"), |tx| {
            for b in prefix { for op in b { tx.apply(op)?; } }
            tx.apply(set_gain("no-such-track", 0.5))   // guaranteed failure
        });
        prop_assert_eq!(snapshot(&m), before);
    }
}

/// Gate test 5: every committed batch carries resolvable attribution.
#[test]
fn attribution_survives_commit() {
    let m = fresh_session_3_tracks();
    let c = Session::transact(&m,
        TxMeta { actor: Actor::Agent { tool: "set_track_mix".into() }, run: "run-42".into(), label: "agent mix".into() },
        |tx| tx.apply(set_gain("t-1", 0.5))).unwrap();
    assert!(matches!(c.meta.actor, Actor::Agent { ref tool } if tool == "set_track_mix"));
    assert_eq!(c.meta.run, "run-42");
}
```

(`arb_op_batches`, `fresh_session_3_tracks`, `snapshot` are real code in the
test file — the strategy generates removes only for ids currently alive in a
model `Vec<String>` it threads through, so generated batches are valid by
construction except where the test injects failure.)

- [ ] **Step 2: Run** — `cargo test --manifest-path src-tauri/Cargo.toml --test channel_properties` → PASS (256 cases each). If a case fails, that is a real bug in Tasks 3–5: minimize, fix there, re-run.
- [ ] **Step 3: Full suite + counts** — run backend + frontend suites; update the dated test counts in `README.md` and `CONTRIBUTING.md`.
- [ ] **Step 4: Commit** — `git commit -am "test(control): property gate for the transaction channel (undo round-trip, atomicity, attribution)"`

---

## Self-review notes (done at authoring time)

- Spec coverage vs round-2 §4: §4.1 Session (Task 1 — Store+MidiStore now, remaining stores explicitly Plan E), §4.3 two tiers + honest anti-nesting (Task 3), §4 property ops/inverse-from-apply/no-op elision (Task 4), structural ops (Task 5), §4.4 effect descriptors + one-notification (Task 6), actor/run at both seams (Tasks 2, 7), §4.6 op-format version constant (Task 2; persistence itself stays dark per PHASE4 rule 1), §7 tests 1/3/5 (Task 8). NOT in this plan, by design: gesture begin/end IPC, prepare-outside staging, engine-thread inversion, mutating readers — all Plan E; §4.2's ban is enforced here structurally (session module cannot reach the engine — Task 6 grep-gate).
- Type consistency: `Op`/`ObjectRef`/`PropPath`/`TxMeta`/`Actor` defined once in Task 2 and used verbatim in 3–8; `Committed.inverses` is reverse-ordered everywhere it is consumed.
- Known integration risks called out in-task: `EngineHandle` test constructor (Task 6 Step 1), `TrackState: PartialEq` derive (Task 5 Step 3), slot handling deliberately left in the effect layer until Plan B (Tasks 5, 7).
