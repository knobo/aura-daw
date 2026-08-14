//! The document. `&mut` access is (after Task 3) only reachable through
//! `Session::transact`. Round-2 §4.1: the remaining three stores
//! (automation, plugin rows, sampler bank) move in during Plan E.
use crate::audio::types::{Store, TrackState};
use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};
use crate::ids::TrackId;
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

    /// Clone of the midi fields `midi::persist` persists — `ppq`,
    /// `tempo_events`, `meter_events`, `clips` — taken under the session
    /// lock so `ControlPlane::execute_persist` can write it to disk AFTER
    /// the lock is released (round-2 §4: no disk I/O under the session
    /// lock). `midi::persist::V3Data` already has exactly this shape.
    pub fn midi_snapshot(&self) -> crate::midi::persist::V3Data {
        crate::midi::persist::V3Data {
            ppq: self.midi.ppq,
            tempo_events: self.midi.tempo_events.clone(),
            meter_events: self.midi.meter_events.clone(),
            clips: self.midi.clips.clone(),
        }
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
/// session lock is released. Faithfully mirrors what the retired
/// `ops::apply_track_mix` (deleted in Plan B, behavior preserved here)
/// used to write:
/// * `param_writes`: `(track_id, path, value)` — `Gain` as linear (via
///   `mixer::db_to_linear`), `Pan` as-is, `Muted`/`Soloed` as 0.0/1.0 gates
///   (`ControlPlane::commit` turns those back into the RT param store's
///   `set_flag` calls). Round-2 §2.4: keyed by `TrackId`, not slot — slots
///   are per-graph-generation state the session never sees; `commit`
///   resolves `TrackId -> slot` through the CURRENT `GraphTables` after the
///   session lock is released (a track with no slot yet, i.e. no rebuild
///   has run since it was added, is simply skipped — see `commit`'s doc for
///   why that is sound). The retired `apply_track_mix` rewrote ALL FOUR
///   params from the post-write snapshot on every changed track, regardless
///   of which single field changed — so does this, per `Set` (no slot
///   gate — EVERY changed track contributes writes now, since resolving
///   whether it has a slot is `commit`'s job, not `apply_raw`'s).
///   `Armed` has no RT param counterpart and never appears as its own entry
///   (matching the retired `apply_track_mix`, which also had nothing to
///   write for it).
/// * `any_solo`: `Store::any_solo()` is a store-wide flag (not per-track),
///   which the retired `apply_track_mix` recomputed unconditionally once per
///   BATCH. A plain `Vec<(TrackId, PropPath, f32)>` can't express a
///   track-less, whole-store flag, so this is a minimal, justified extension
///   beyond the brief's tuple sketch: recomputed on every track `Set` in the
///   transaction; the last write is truth, so repeat recomputation is
///   harmless. Also recomputed on
///   `Op::TrackRemove` (Task 7, controller ruling 2): the removed row may
///   have been the only soloed track. `Op::TrackAdd` does NOT need this — a
///   fresh row is never soloed, so an add can never flip the flag.
/// * `rebuild`: `TrackAdd`/`TrackRemove` set it; N structural ops still
///   fold to a single flag, i.e. at most one `ControlMsg::Rebuild`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EngineEffect {
    pub rebuild: bool,
    pub param_writes: Vec<(TrackId, PropPath, f32)>,
    pub any_solo: Option<bool>,
    /// Which stores to persist to disk, executed by `ControlPlane::commit`
    /// (Task 6) AFTER the session lock is released — see `PersistEffect`.
    pub persist: PersistEffect,
}

/// Which durable stores a transaction touched — described here (plain data,
/// no I/O), EXECUTED by `ControlPlane::execute_persist` after the session
/// lock is released (round-2 §4: persistence becomes an effect, not I/O
/// under the lock). `plugins`/`automation` are fields only for now — no
/// executor wires them until Tasks 9/10 (YAGNI: this task adds the
/// machinery, not speculative plugin/automation persistence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PersistEffect {
    /// `midi::persist::save_snapshot_into_project` from a `midi_snapshot()`.
    pub midi: bool,
    /// `audio::project::save` from `audio::project::from_store`.
    pub project: bool,
    /// Task 9 wires the executor; the field exists now.
    pub plugins: bool,
    /// Task 10 wires the executor; the field exists now.
    pub automation: bool,
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
            if matches!(path, PropPath::InstrumentId) {
                // Structural (Plan E Task 3, inventory row 2): rebinding a
                // track's instrument changes the live graph node, so this
                // triggers a rebuild + persists the project, instead of the
                // mix-param-table writes below (InstrumentId has no RT
                // param counterpart — nothing there to write).
                effect.rebuild = true;
                effect.persist.project = true;
                return Ok(inverse);
            }
            // Mirror the retired `ops::apply_track_mix` (deleted in Plan B,
            // behavior preserved here): a changed track gets all four mix
            // params rewritten from the current (post-write) snapshot, not
            // just the touched path — that's what apply_track_mix did too
            // (unconditional per changed-track write of gain/pan/mute/solo).
            // Round-2 §2.4: slots are per-graph-generation state the
            // session never sees, so the writes are always pushed here,
            // keyed by `TrackId`; `commit` resolves `TrackId -> slot`
            // through the CURRENT `GraphTables` AFTER this transaction
            // commits (a track with no slot yet, i.e. no rebuild has run
            // since it was added, is skipped there — sound only because
            // rebuild publishes its tables under the session lock, so
            // either this commit lands before that read (the rebuild bakes
            // the value in) or after the publish (the write executes
            // against the new table); see `GraphTables`/`commit` docs).
            let t = session
                .store
                .tracks
                .iter()
                .find(|t| &t.id == id)
                .expect("just wrote this track above");
            effect.param_writes.push((
                id.clone(),
                PropPath::Gain,
                crate::audio::mixer::db_to_linear(t.gain_db),
            ));
            effect.param_writes.push((id.clone(), PropPath::Pan, t.pan as f32));
            effect.param_writes.push((
                id.clone(),
                PropPath::Muted,
                if t.muted { 1.0 } else { 0.0 },
            ));
            effect.param_writes.push((
                id.clone(),
                PropPath::Soloed,
                if t.soloed { 1.0 } else { 0.0 },
            ));
            // any_solo is store-wide (Store::any_solo), independent of
            // slot presence — the retired apply_track_mix (deleted in
            // Plan B) recomputed it unconditionally once per batch; here,
            // per track Set (idempotent: the last recompute in the
            // transaction is truth).
            effect.any_solo = Some(session.store.any_solo());
            Ok(inverse)
        }
        Op::TrackAdd { track, index, clips, clip_indices } => {
            if session.store.tracks.iter().any(|t| t.id == track.id) {
                return Err(format!("duplicate track id: {}", track.id));
            }
            let idx = (*index).min(session.store.tracks.len());
            crate::control::ops::insert_track(&mut session.store, track.clone(), idx);
            // Clips are document state (part of Project), not effect-layer
            // bookkeeping — they travel WITH the row through the op itself
            // (fix: a prior draft freed/dropped clips outside the channel;
            // the payload here is authoritative, not advisory, since these
            // are the clips being inserted, not pre-existing store truth).
            if clip_indices.len() == clips.len() && !clips.is_empty() {
                // Indices are ascending pre-removal positions; inserting back in
                // ascending order reconstructs the original interleaving exactly.
                for (c, &at) in clips.iter().zip(clip_indices.iter()) {
                    let at = at.min(session.store.clips.len());
                    session.store.clips.insert(at, c.clone());
                }
            } else {
                session.store.clips.extend(clips.iter().cloned());
            }
            // Structural: at most one Rebuild per transaction, however many
            // structural ops it contains — a plain flag naturally folds.
            effect.rebuild = true;
            // Inverse: remove the same row (+ the clips just inserted with
            // it), same id — no slot bookkeeping here or anywhere in the op
            // layer (round-2 §2.4: slots are derived fresh from display
            // order on every rebuild, not stored or freed anywhere).
            Ok(Op::TrackRemove {
                track: track.clone(), index: idx, clips: clips.clone(),
                clip_indices: clip_indices.clone(),
            })
        }
        Op::TrackRemove { track, .. } => {
            // Found by id, not blindly by the caller's recorded index —
            // store truth wins (same rule as `Op::Set`'s from/to). There is
            // no slot to free: slots are derived fresh from display order
            // on every rebuild (round-2 §2.4), so the removed row simply
            // stops appearing in the next `GraphTables`.
            let pos = session
                .store
                .tracks
                .iter()
                .position(|t| t.id == track.id)
                .ok_or_else(|| format!("unknown track: {}", track.id))?;
            let removed = session.store.tracks.remove(pos);
            // Clips are document state — collected from STORE TRUTH (the
            // caller's `clips` field on `Op::TrackRemove` is advisory only,
            // same precedent as `index`), removed alongside the row so an
            // undo (`TrackAdd` inverse below) restores both, not just an
            // empty track. Pre-removal indices are recorded too (retain
            // visits in order) so the inverse can reinsert each clip at its
            // original position instead of appending (Plan A carry-forward:
            // positional restore keeps clip vec order byte-stable).
            let mut removed_clips = Vec::new();
            let mut removed_indices = Vec::new();
            let mut i = 0usize;
            session.store.clips.retain(|c| {
                let keep = c.track_id != track.id;
                if !keep {
                    removed_clips.push(c.clone());
                    removed_indices.push(i);
                }
                i += 1;
                keep
            });
            effect.rebuild = true;
            // Removing a track can flip the store-wide any_solo flag (the
            // removed row may have been the only soloed track) — recompute
            // it here, same as TrackAdd's Set-path recompute above. A fresh
            // TrackAdd row is never soloed (ops::new_track_row), so adding
            // never changes any_solo and doesn't need this.
            effect.any_solo = Some(session.store.any_solo());
            Ok(Op::TrackAdd {
                track: removed, index: pos, clips: removed_clips, clip_indices: removed_indices,
            })
        }
        // Plan E Task 3 (inventory row 3): clip placement — the round-2
        // "most common editing gesture reaches no backend channel at all"
        // gap. Property-addressed, but structural in effect (rebuild +
        // persist) since a moved clip changes what the RT graph renders.
        Op::Set { object: ObjectRef::Clip(id), path: PropPath::TimelineStartSamples, to, .. } => {
            let c = session
                .store
                .clips
                .iter_mut()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("unknown clip: {id}"))?;
            let from_now = serde_json::json!(c.timeline_start_samples); // truth, not caller's `from`
            let v = to.as_u64().ok_or("timelineStartSamples: expected a non-negative integer")?;
            c.timeline_start_samples = v;
            let applied = serde_json::json!(c.timeline_start_samples);
            effect.rebuild = true;
            effect.persist.project = true;
            Ok(Op::Set {
                object: ObjectRef::Clip(id.clone()),
                path: PropPath::TimelineStartSamples,
                from: applied, // the inverse's `from`: value we just wrote
                to: from_now,  // the inverse's `to`: value to restore
            })
        }
        // Plan E Task 3 (inventory rows 16/17/20/23): sinks/threads consume
        // clip add/remove — the same TrackAdd/TrackRemove pattern (store
        // truth wins over the caller's advisory payload beyond `.id`).
        Op::ClipAdd { clip, index } => {
            if session.store.clips.iter().any(|c| c.id == clip.id) {
                return Err(format!("duplicate clip id: {}", clip.id));
            }
            let idx = (*index).min(session.store.clips.len());
            session.store.clips.insert(idx, clip.clone());
            effect.rebuild = true;
            effect.persist.project = true;
            Ok(Op::ClipRemove { clip: clip.clone(), index: idx })
        }
        Op::ClipRemove { clip, .. } => {
            // Found by id, not blindly by the caller's recorded index —
            // store truth wins (same rule as `Op::Set`'s from/to and
            // `Op::TrackRemove`'s row lookup).
            let pos = session
                .store
                .clips
                .iter()
                .position(|c| c.id == clip.id)
                .ok_or_else(|| format!("unknown clip: {}", clip.id))?;
            let removed = session.store.clips.remove(pos);
            effect.rebuild = true;
            effect.persist.project = true;
            Ok(Op::ClipAdd { clip: removed, index: pos })
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
        // Option<String> serializes as a JSON string or null, never a
        // wrapping object — same wire shape `write_prop` below accepts.
        PropPath::InstrumentId => serde_json::json!(t.instrument_id),
        PropPath::TimelineStartSamples => {
            unreachable!("TimelineStartSamples is a Clip path, never dispatched to a Track")
        }
    }
}

/// Write a track property, clamping to the same schema ranges the retired
/// `ops::apply_track_mix` (deleted in Plan B, behavior preserved here) used,
/// and returns the as-applied (post-clamp) value.
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
        PropPath::InstrumentId => {
            let v = match to {
                serde_json::Value::Null => None,
                serde_json::Value::String(s) => Some(s.clone()),
                _ => return Err("instrumentId: expected a string or null".into()),
            };
            t.instrument_id = v;
            Ok(serde_json::json!(t.instrument_id))
        }
        PropPath::TimelineStartSamples => {
            unreachable!("TimelineStartSamples is a Clip path, never dispatched to a Track")
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
/// inverse, in original relative order. Structural (non-Set) ops act as a
/// barrier: grouping never reaches back across a structural op boundary to
/// merge with earlier Set groups, preserving replay order once ops persist.
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
    let mut barrier = 0usize; // grouping never reaches below this index

    for (op, inv) in ops.into_iter().zip(inverses.into_iter()) {
        match (&op, &inv) {
            (Op::Set { object, path, .. }, Op::Set { from: true_to, to: true_from, .. }) => {
                let existing = slots[barrier..].iter_mut().find_map(|s| match s {
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
            _ => {
                slots.push(Slot::Passthrough(op, inv));
                barrier = slots.len(); // structural op: nothing before it may absorb later Sets
            }
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
    use crate::audio::types::testutil::{test_clip, test_track};
    use crate::control::op::testutil::set_gain;
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

    #[test]
    fn set_gain_applies_and_inverse_restores() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let meta = TxMeta::user("set gain");
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
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].gain_db, 1.0);
    }

    #[test]
    fn clamped_write_round_trips_through_inverse() {
        // 999 dB clamps to 24.0 on write; the recorded op must carry the
        // POST-CLAMP value (store truth), and its inverse must restore the
        // original exactly — the ledgered 999→24 case.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let c = Session::transact(&m, TxMeta::user("clamp"), |tx| {
            tx.apply(set_gain("t-1", 999.0))
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].gain_db, 24.0);
        match &c.ops[0] {
            Op::Set { to, .. } => assert_eq!(to, &serde_json::json!(24.0),
                "recorded op must carry the as-applied (clamped) value"),
            other => panic!("expected Set, got {other:?}"),
        }
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].gain_db, 1.0, "back to the original");
    }

    #[test]
    fn move_and_move_back_elides_to_noop_history() {
        // §4: property-addressed ops coalesce; from==to after fold drops out.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let c = Session::transact(&m, TxMeta::user("wiggle"), |tx| {
            tx.apply(set_gain("t-1", 0.5))?;
            tx.apply(set_gain("t-1", 1.0))?; // back to original
            Ok(())
        })
        .unwrap();
        assert!(c.ops.is_empty(), "net-noop batch must fold to zero ops, got {:?}", c.ops);
    }

    #[test]
    fn track_add_with_duplicate_id_is_rejected() {
        // Rollback removes-by-id: a duplicate id would make the inverse
        // mis-target. Reject, don't clamp (ledgered Plan A carry-forward).
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before = m.lock().store.tracks.clone();
        let r = Session::transact(&m, TxMeta::user("dup"), |tx| {
            tx.apply(Op::TrackAdd {
                track: test_track("t-1"), index: 1,
                clips: vec![], clip_indices: vec![],
            })
        });
        assert!(r.is_err());
        assert_eq!(m.lock().store.tracks, before, "store untouched");
    }

    #[test]
    fn unknown_track_fails_whole_batch_atomically() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before_gain = m.lock().store.tracks[0].gain_db;
        let r = Session::transact(&m, TxMeta::user("bad"), |tx| {
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
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "noop".into(), transient: false };
        let c1 = Session::transact(&m, meta.clone(), |_tx| Ok(())).unwrap();
        let c2 = Session::transact(&m, meta, |_tx| Ok(())).unwrap();
        assert_eq!(c2.rev, c1.rev + 1);
    }

    #[test]
    fn failing_closure_leaves_session_untouched_and_no_rev() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "fail".into(), transient: false };
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
        let c = Session::transact(&m, TxMeta::user("remove"), |tx| {
            tx.apply(Op::TrackRemove { track: row.clone(), index: 0, clips: vec![], clip_indices: vec![] })
        }).unwrap();
        assert!(m.lock().store.tracks.is_empty());
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        }).unwrap();
        assert_eq!(m.lock().store.tracks[0], row, "same id, same everything");
    }

    #[test]
    fn remove_undo_restores_interleaved_clip_order_byte_identically() {
        // store.clips = [a1, b1, a2] (a-clips on t-A, b1 on t-B). Removing t-A
        // then undoing must yield EXACTLY [a1, b1, a2] — not [b1, a1, a2].
        let m = parking_lot::Mutex::new(session_with_one_track("t-B"));
        {
            let mut g = m.lock();
            g.store.tracks.push(test_track("t-A"));
            g.store.clips.push(test_clip("a1", "t-A"));
            g.store.clips.push(test_clip("b1", "t-B"));
            g.store.clips.push(test_clip("a2", "t-A"));
        }
        let row = m.lock().store.tracks.iter().find(|t| t.id == "t-A").cloned().unwrap();
        let before = m.lock().store.clips.clone();
        let c = Session::transact(&m, TxMeta::user("remove"), |tx| {
            tx.apply(Op::TrackRemove { track: row, index: 0, clips: vec![], clip_indices: vec![] })
        })
        .unwrap();
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.clips, before, "same clips, same ORDER");
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
        let r = Session::transact(&m, TxMeta::user("mixed-fail"), |tx| {
            tx.apply(Op::TrackAdd { track: new_row, index: 1, clips: vec![], clip_indices: vec![] })?;
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
        let meta = TxMeta { actor: Actor::User, run: "r".into(), label: "outer".into(), transient: false };
        let _ = Session::transact(&m, meta.clone(), |_tx| {
            let _ = Session::transact(&m, meta.clone(), |_| Ok(()));
            Ok(())
        });
    }

    #[test]
    fn fold_never_merges_sets_across_a_structural_op() {
        // gain 0→3, then a structural TrackAdd, then gain 3→6: the two Sets
        // must NOT merge into one 0→6 emitted before the TrackAdd.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let t2 = test_track("t-2");
        let c = Session::transact(&m, TxMeta::user("fold-barrier"), |tx| {
            tx.apply(set_gain("t-1", 3.0))?;
            tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![] })?;
            tx.apply(set_gain("t-1", 6.0))
        })
        .unwrap();
        assert_eq!(c.ops.len(), 3, "no cross-barrier merge: {:?}", c.ops);
        assert!(matches!(&c.ops[0], Op::Set { .. }));
        assert!(matches!(&c.ops[1], Op::TrackAdd { .. }));
        assert!(matches!(&c.ops[2], Op::Set { .. }));
        // Verify the gained value is correct
        assert_eq!(m.lock().store.tracks[0].gain_db, 6.0);
    }

    #[test]
    fn wiggle_back_across_a_structural_op_is_not_elided() {
        // gain 0→3, TrackAdd, gain 3→0: net-noop for the gain, but the two Sets
        // sit in different barrier regions — BOTH survive (replay order truth
        // beats history minimalism once ops persist).
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let t2 = test_track("t-2");
        let c = Session::transact(&m, TxMeta::user("wiggle-barrier"), |tx| {
            tx.apply(set_gain("t-1", 3.0))?;
            tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![] })?;
            tx.apply(set_gain("t-1", 1.0)) // back to original
        })
        .unwrap();
        assert_eq!(c.ops.len(), 3, "cross-barrier wiggle must not elide: {:?}", c.ops);
        // Even though we're back to the original value, the two Sets are in
        // different barrier regions so neither can cancel out
    }

    // ---- Plan E Task 3: clip move / add / remove, track instrument ----

    #[test]
    fn clip_move_applies_and_inverse_restores() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let clip = test_clip("c-1", "t-1");
        m.lock().store.clips.push(clip.clone());
        let c = Session::transact(&m, TxMeta::user("move clip"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Clip("c-1".into()),
                path: PropPath::TimelineStartSamples,
                from: serde_json::Value::Null, // advisory; apply re-reads truth
                to: serde_json::json!(48_000u64),
            })
        })
        .unwrap();
        assert_eq!(m.lock().store.clips[0].timeline_start_samples, 48_000);
        assert!(c.effect.rebuild, "clip move must request a rebuild");
        assert!(c.effect.persist.project, "clip move must persist the project");
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            m.lock().store.clips[0].timeline_start_samples,
            clip.timeline_start_samples,
            "undo restores the original position"
        );
    }

    #[test]
    fn clip_add_duplicate_id_is_rejected_atomically() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let clip = test_clip("c-1", "t-1");
        m.lock().store.clips.push(clip.clone());
        let before = m.lock().store.clips.clone();
        let r = Session::transact(&m, TxMeta::user("dup clip"), |tx| {
            tx.apply(Op::ClipAdd { clip: clip.clone(), index: 0 })?;
            tx.apply(Op::ClipAdd { clip: clip.clone(), index: 1 }) // duplicate id, in-tx
        });
        assert!(r.is_err());
        assert_eq!(m.lock().store.clips, before, "store untouched, rev unchanged");
    }

    #[test]
    fn instrument_set_round_trips_and_requests_rebuild() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        assert_eq!(m.lock().store.tracks[0].instrument_id, None);
        let c = Session::transact(&m, TxMeta::user("set instrument"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::InstrumentId,
                from: serde_json::Value::Null,
                to: serde_json::json!("plugin:x"),
            })
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].instrument_id, Some("plugin:x".to_string()));
        assert!(c.effect.rebuild, "instrument change must request a rebuild");
        assert!(c.effect.persist.project, "instrument change must persist the project");
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.tracks[0].instrument_id, None, "undo restores None");
    }
}
