//! The document. `&mut` access is (after Task 3) only reachable through
//! `Session::transact`. Round-2 §4.1: the remaining three stores
//! (automation, plugin rows, sampler bank) move in during Plan E.
use crate::audio::types::{Store, TrackState};
use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};
use crate::midi::MidiStore;

pub struct Session {
    pub store: Store,
    pub midi: MidiStore,
    pub(crate) rev: u64,
}

impl Session {
    pub fn new(store: Store, midi: MidiStore) -> Self {
        Self { store, midi, rev: 0 }
    }

    /// The ONLY way to mutate the document. Returns Err and leaves the
    /// session untouched if the closure fails (inverses rolled back).
    pub fn transact<F>(this: &parking_lot::Mutex<Session>, meta: TxMeta, f: F) -> Result<Committed, String>
    where
        F: FnOnce(&mut Tx<'_>) -> Result<(), String>,
    {
        IN_TX.with(|t| {
            if t.get() {
                panic!("nested transact — compose with &mut Tx instead");
            }
            t.set(true);
        });
        let _reset = scopeguard(|| IN_TX.with(|t| t.set(false)));
        let mut guard = this.lock();
        let mut tx = Tx { session: &mut guard, ops: vec![], inverses: vec![], effect: EngineEffect::default() };
        match f(&mut tx) {
            Ok(()) => {
                let (ops, inverses, effect) = (tx.ops, tx.inverses, tx.effect);
                // Commit-time fold (round-2 §4): multiple property-addressed
                // ops on the same (object, path) within one transaction
                // coalesce to their net change; a batch that nets to nothing
                // (e.g. wiggle-and-back) leaves no trace in the history.
                let (ops, mut inverses) = fold_ops(ops, inverses);
                inverses.reverse();
                guard.rev += 1;
                Ok(Committed { rev: guard.rev, ops, inverses, effect, meta })
            }
            Err(e) => {
                // Rollback: run collected inverses in reverse — the same
                // code path undo will use (round-2 §4, "inverse produced by
                // apply, never guessed").
                let inv: Vec<Op> = tx.inverses.drain(..).rev().collect();
                // Rollback discards its effect: the transaction is about to
                // return Err, so no `Committed` (and thus no effect) ever
                // reaches `ControlPlane::commit`.
                let mut discarded = EngineEffect::default();
                for op in inv {
                    apply_raw(tx.session, &op, &mut discarded).expect("rollback must not fail");
                }
                Err(e)
            }
        }
    }
}

thread_local! {
    static IN_TX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Tx<'s> {
    session: &'s mut Session,
    ops: Vec<Op>,
    inverses: Vec<Op>,
    effect: EngineEffect,
}

impl Tx<'_> {
    /// Applies `op`, recording it, its computed inverse, and its effect
    /// (folded into `self.effect` — see `EngineEffect`).
    pub fn apply(&mut self, op: Op) -> Result<(), String> {
        let inverse = apply_raw(self.session, &op, &mut self.effect)?;
        self.ops.push(op);
        self.inverses.push(inverse);
        Ok(())
    }
}

/// What `apply_raw` did that the engine/RT side must eventually see, folded
/// across a whole transaction. The session only DESCRIBES the effect —
/// `ControlPlane::commit` (control/mod.rs, Task 6) EXECUTES it after the
/// session lock is released. Faithfully mirrors what `ops::apply_track_mix`
/// (control/ops.rs:47-95) writes today:
/// * `param_writes`: `(slot, path, value)` — `Gain` as linear (via
///   `mixer::db_to_linear`), `Pan` as-is, `Muted`/`Soloed` as 0.0/1.0 gates
///   (`ControlPlane::commit` turns those back into the RT param store's
///   `set_flag` calls). `apply_track_mix` rewrites ALL FOUR params from the
///   post-write snapshot on every changed (slotted) track, regardless of
///   which single field changed — so does this, per `Set`. `Armed` has no
///   RT param counterpart and never appears as its own entry (matching
///   `apply_track_mix`, which also has nothing to write for it).
/// * `any_solo`: `Store::any_solo()` is a store-wide flag (not per-slot),
///   which `apply_track_mix` recomputes unconditionally once per BATCH. A
///   plain `Vec<(slot, PropPath, f32)>` can't express a slot-less,
///   whole-store flag, so this is a minimal, justified extension beyond the
///   brief's tuple sketch: recomputed on every track `Set` in the
///   transaction (slot or not — `any_solo` doesn't require one); the last
///   write is truth, so repeat recomputation is harmless.
/// * `rebuild`: `TrackAdd`/`TrackRemove` set it; N structural ops still
///   fold to a single flag, i.e. at most one `ControlMsg::Rebuild`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EngineEffect {
    pub rebuild: bool,
    pub param_writes: Vec<(usize, PropPath, f32)>,
    pub any_solo: Option<bool>,
}

pub struct Committed {
    pub rev: u64,
    pub ops: Vec<Op>,
    pub inverses: Vec<Op>, // reverse order, ready to apply
    pub effect: EngineEffect,
    pub meta: TxMeta,
}

/// Applies `op` to `session`, returning the computed inverse, and folds its
/// effect into `effect` (see `EngineEffect`). The inverse's `from`/`to` are
/// both derived from store truth (the value actually written, post-clamp,
/// and the value that was there before) — never from the caller-supplied
/// `op.from`, which is advisory only.
///
/// This computes the effect DESCRIPTION only — plain data, no RT param
/// store / engine handle / control-message reference anywhere in this
/// module (grep-gated, see `ControlPlane::commit`). `ControlPlane::commit`
/// EXECUTES the folded effect after the session lock is released.
fn apply_raw(session: &mut Session, op: &Op, effect: &mut EngineEffect) -> Result<Op, String> {
    match op {
        Op::Set { object: ObjectRef::Track(id), path, to, .. } => {
            let t = session
                .store
                .tracks
                .iter_mut()
                .find(|t| &t.id == id)
                .ok_or_else(|| format!("unknown track: {id}"))?;
            let from_now = read_prop(t, *path); // truth, not caller's `from`
            let applied = write_prop(t, *path, to)?; // clamps like ops.rs does
            let inverse = Op::Set {
                object: ObjectRef::Track(id.clone()),
                path: *path,
                from: applied,   // the inverse's `from`: value we just wrote
                to: from_now,    // the inverse's `to`: value to restore
            };
            // Mirror `ops::apply_track_mix` (control/ops.rs:47-95): a
            // changed track WITH a slot gets all four mix params rewritten
            // from the current (post-write) snapshot, not just the touched
            // path — that's what apply_track_mix does too (unconditional
            // per changed-track write of gain/pan/mute/solo). A track
            // without a slot contributes no param_write (op::TrackAdd
            // leaves slot bookkeeping untouched, round-2 §2.4).
            if let Some(&slot) = session.store.slots.get(id) {
                let t = session
                    .store
                    .tracks
                    .iter()
                    .find(|t| &t.id == id)
                    .expect("just wrote this track above");
                effect.param_writes.push((
                    slot,
                    PropPath::Gain,
                    crate::audio::mixer::db_to_linear(t.gain_db),
                ));
                effect.param_writes.push((slot, PropPath::Pan, t.pan as f32));
                effect.param_writes.push((
                    slot,
                    PropPath::Muted,
                    if t.muted { 1.0 } else { 0.0 },
                ));
                effect.param_writes.push((
                    slot,
                    PropPath::Soloed,
                    if t.soloed { 1.0 } else { 0.0 },
                ));
            }
            // any_solo is store-wide (Store::any_solo), independent of
            // slot presence — apply_track_mix recomputes it unconditionally
            // once per batch; here, per track Set (idempotent: the last
            // recompute in the transaction is truth).
            effect.any_solo = Some(session.store.any_solo());
            Ok(inverse)
        }
        Op::TrackAdd { track, index } => {
            let idx = (*index).min(session.store.tracks.len());
            crate::control::ops::insert_track(&mut session.store, track.clone(), idx);
            // Structural: at most one Rebuild per transaction, however many
            // structural ops it contains — a plain flag naturally folds.
            effect.rebuild = true;
            // Inverse: remove the same row, same id, no slot bookkeeping
            // here (round-2 §2.4 — slots stay in the command's effect
            // layer until Plan B).
            Ok(Op::TrackRemove { track: track.clone(), index: idx })
        }
        Op::TrackRemove { track, .. } => {
            // Found by id, not blindly by the caller's recorded index —
            // store truth wins (same rule as `Op::Set`'s from/to). The
            // slot is deliberately left untouched: slot bookkeeping is
            // outside the op layer this plan (round-2 §2.4).
            let pos = session
                .store
                .tracks
                .iter()
                .position(|t| t.id == track.id)
                .ok_or_else(|| format!("unknown track: {}", track.id))?;
            let removed = session.store.tracks.remove(pos);
            effect.rebuild = true;
            Ok(Op::TrackAdd { track: removed, index: pos })
        }
        _ => Err("op not yet supported".into()),
    }
}

/// Read a track property as its JSON-wire representation (round-trippable
/// through `write_prop`).
fn read_prop(t: &TrackState, path: PropPath) -> serde_json::Value {
    match path {
        PropPath::Gain => serde_json::json!(t.gain_db),
        PropPath::Pan => serde_json::json!(t.pan),
        PropPath::Muted => serde_json::json!(t.muted),
        PropPath::Soloed => serde_json::json!(t.soloed),
        PropPath::Armed => serde_json::json!(t.armed),
    }
}

/// Write a track property, clamping to the same schema ranges
/// `ops::apply_track_mix` (control/ops.rs:47) uses, and returns the
/// as-applied (post-clamp) value.
fn write_prop(t: &mut TrackState, path: PropPath, to: &serde_json::Value) -> Result<serde_json::Value, String> {
    match path {
        PropPath::Gain => {
            let v = to.as_f64().ok_or("gain: expected a number")?;
            t.gain_db = v.clamp(-160.0, 24.0);
            Ok(serde_json::json!(t.gain_db))
        }
        PropPath::Pan => {
            let v = to.as_f64().ok_or("pan: expected a number")?;
            t.pan = v.clamp(-1.0, 1.0);
            Ok(serde_json::json!(t.pan))
        }
        PropPath::Muted => {
            let v = to.as_bool().ok_or("muted: expected a bool")?;
            t.muted = v;
            Ok(serde_json::json!(t.muted))
        }
        PropPath::Soloed => {
            let v = to.as_bool().ok_or("soloed: expected a bool")?;
            t.soloed = v;
            Ok(serde_json::json!(t.soloed))
        }
        PropPath::Armed => {
            let v = to.as_bool().ok_or("armed: expected a bool")?;
            t.armed = v;
            Ok(serde_json::json!(t.armed))
        }
    }
}

/// Commit-time fold: `ops`/`inverses` are parallel (same length/order —
/// `inverses[i]` is the as-applied inverse of `ops[i]`). `Set` ops on the
/// same `(object, path)` coalesce into one entry keeping the first true
/// `from` and the last true `to` (both read off the inverse, which carries
/// store truth, never the caller's advisory `op.from`); entries whose net
/// `from == to` are dropped from both `ops` and `inverses`. Non-`Set` ops
/// (future op kinds) pass through untouched, each still paired with its own
/// inverse, in original relative order.
///
/// A linear scan (not a `HashMap`) is used deliberately: `PropPath` isn't
/// `Hash` (op.rs's closed enum, Task 2 territory, left untouched), and
/// transaction batches are small enough that O(n²) is a non-issue.
fn fold_ops(ops: Vec<Op>, inverses: Vec<Op>) -> (Vec<Op>, Vec<Op>) {
    struct Group {
        object: ObjectRef,
        path: PropPath,
        from: serde_json::Value,
        to: serde_json::Value,
    }
    enum Slot {
        Grouped(Group),
        Passthrough(Op, Op),
    }

    let mut slots: Vec<Slot> = Vec::new();

    for (op, inv) in ops.into_iter().zip(inverses.into_iter()) {
        match (&op, &inv) {
            (Op::Set { object, path, .. }, Op::Set { from: true_to, to: true_from, .. }) => {
                let existing = slots.iter_mut().find_map(|s| match s {
                    Slot::Grouped(g) if g.object == *object && g.path == *path => Some(g),
                    _ => None,
                });
                if let Some(g) = existing {
                    g.to = true_to.clone(); // last `to` wins
                } else {
                    slots.push(Slot::Grouped(Group {
                        object: object.clone(),
                        path: *path,
                        from: true_from.clone(), // first `from` wins
                        to: true_to.clone(),
                    }));
                }
            }
            _ => slots.push(Slot::Passthrough(op, inv)),
        }
    }

    let mut folded_ops = Vec::with_capacity(slots.len());
    let mut folded_inverses = Vec::with_capacity(slots.len());
    for slot in slots {
        match slot {
            Slot::Grouped(g) => {
                if g.from == g.to {
                    continue; // net no-op: drop op and inverse alike
                }
                folded_ops.push(Op::Set {
                    object: g.object.clone(),
                    path: g.path,
                    from: g.from.clone(),
                    to: g.to.clone(),
                });
                folded_inverses.push(Op::Set { object: g.object, path: g.path, from: g.to, to: g.from });
            }
            Slot::Passthrough(op, inv) => {
                folded_ops.push(op);
                folded_inverses.push(inv);
            }
        }
    }
    (folded_ops, folded_inverses)
}

/// Tiny local scope guard (avoid a new dependency).
fn scopeguard<F: FnMut()>(f: F) -> impl Drop {
    struct G<F: FnMut()>(F);
    impl<F: FnMut()> Drop for G<F> {
        fn drop(&mut self) {
            (self.0)()
        }
    }
    G(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::TrackState;
    use crate::control::op::{Actor, ObjectRef, PropPath, TxMeta};

    /// Store with one TrackState id `id`. Initial `gain_db` is 1.0 — a
    /// deliberately odd-but-valid dB value inside ops.rs's -160..24 clamp
    /// range, chosen only so tests can tell "unchanged" from "changed".
    fn session_with_one_track(id: &str) -> Session {
        let mut store = Store::default();
        store.tracks.push(TrackState {
            id: id.into(),
            name: "Track 1".into(),
            kind: "audio".into(),
            gain_db: 1.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        });
        Session::new(store, MidiStore::default())
    }

    fn user_meta(label: &str) -> TxMeta {
        TxMeta { actor: Actor::User, run: "r-1".into(), label: label.into() }
    }

    /// A standalone `TrackState` row (not yet in any store), for structural
    /// op tests (`TrackAdd`/`TrackRemove` payloads).
    fn test_track(id: &str) -> TrackState {
        TrackState {
            id: id.into(),
            name: "New Track".into(),
            kind: "audio".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    /// `from` is intentionally `Null`: it's advisory only, apply_raw re-reads
    /// truth from the store and ignores the caller's `from`.
    fn set_gain(track_id: &str, to: f64) -> Op {
        Op::Set {
            object: ObjectRef::Track(track_id.into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(to),
        }
    }

    #[test]
    fn set_gain_applies_and_inverse_restores() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let meta = user_meta("set gain");
        let c = Session::transact(&m, meta.clone(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::Gain,
                from: serde_json::json!(1.0), // advisory; apply re-reads truth
                to: serde_json::json!(0.25),
            })
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].gain_db, 0.25);
        // Undo = apply the recorded inverses through a new transaction.
        Session::transact(&m, user_meta("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].gain_db, 1.0);
    }

    #[test]
    fn move_and_move_back_elides_to_noop_history() {
        // §4: property-addressed ops coalesce; from==to after fold drops out.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let c = Session::transact(&m, user_meta("wiggle"), |tx| {
            tx.apply(set_gain("t-1", 0.5))?;
            tx.apply(set_gain("t-1", 1.0))?; // back to original
            Ok(())
        })
        .unwrap();
        assert!(c.ops.is_empty(), "net-noop batch must fold to zero ops, got {:?}", c.ops);
    }

    #[test]
    fn unknown_track_fails_whole_batch_atomically() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before_gain = m.lock().store.tracks[0].gain_db;
        let r = Session::transact(&m, user_meta("bad"), |tx| {
            tx.apply(set_gain("t-1", 0.1))?;
            tx.apply(set_gain("nope", 0.2))?; // fails → rollback
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(m.lock().store.tracks[0].gain_db, before_gain);
    }

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
        // NB: each snapshot takes ONE lock guard and reads both fields off
        // it — `(m.lock().x, m.lock().y)` would deadlock, since the first
        // `m.lock()` temporary lives until the end of the full tuple
        // statement and parking_lot's Mutex isn't reentrant.
        let snapshot = { let g = m.lock(); (g.store.tracks.clone(), g.store.clips.clone()) };
        let new_row = test_track("t-2");
        let r = Session::transact(&m, user_meta("mixed-fail"), |tx| {
            tx.apply(Op::TrackAdd { track: new_row, index: 1 })?;
            tx.apply(set_gain("t-2", 0.5))?;
            tx.apply(set_gain("ghost", 0.1))?;   // fails the batch
            Ok(())
        });
        assert!(r.is_err());
        let now = { let g = m.lock(); (g.store.tracks.clone(), g.store.clips.clone()) };
        assert_eq!(now, snapshot, "rollback must undo the structural op too");
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
}
