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
                for op in inv {
                    apply_raw(tx.session, &op).expect("rollback must not fail");
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
    /// Applies `op`, recording it and its computed inverse.
    pub fn apply(&mut self, op: Op) -> Result<(), String> {
        let inverse = apply_raw(self.session, &op)?;
        self.ops.push(op);
        self.inverses.push(inverse);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineEffect {
    pub rebuild: bool,
} // grows in Task 6

pub struct Committed {
    pub rev: u64,
    pub ops: Vec<Op>,
    pub inverses: Vec<Op>, // reverse order, ready to apply
    pub effect: EngineEffect,
    pub meta: TxMeta,
}

/// Applies `op` to `session`, returning the computed inverse. The inverse's
/// `from`/`to` are both derived from store truth (the value actually
/// written, post-clamp, and the value that was there before) — never from
/// the caller-supplied `op.from`, which is advisory only.
///
/// Param-table/RT propagation is deliberately NOT touched here (Task 6's
/// job): this only moves `Store` truth. The existing command still calls
/// `ops::apply_track_mix` for the RT side until Task 7 rewires it.
fn apply_raw(session: &mut Session, op: &Op) -> Result<Op, String> {
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
            Ok(Op::Set {
                object: ObjectRef::Track(id.clone()),
                path: *path,
                from: applied,   // the inverse's `from`: value we just wrote
                to: from_now,    // the inverse's `to`: value to restore
            })
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
