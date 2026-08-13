//! The document. `&mut` access is (after Task 3) only reachable through
//! `Session::transact`. Round-2 §4.1: the remaining three stores
//! (automation, plugin rows, sampler bank) move in during Plan E.
use crate::audio::types::Store;
use crate::control::op::{Op, TxMeta};
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

/// Applies `op` to `session`, returning the computed inverse. Stub until
/// Task 4 fills in the op vocabulary's actual mutation logic.
fn apply_raw(_session: &mut Session, _op: &Op) -> Result<Op, String> {
    Err("no ops yet".into())
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
    use crate::control::op::{Actor, TxMeta};

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
