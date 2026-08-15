//! The document. `&mut` access is (after Task 3) only reachable through
//! `Session::transact`. Round-2 §4.1: the remaining three stores
//! (automation, plugin rows, sampler bank) move in during Plan E.
use std::collections::HashMap;

use crate::audio::types::{Store, TrackState, TransportState};
use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};
use crate::ids::TrackId;
use crate::midi::MidiStore;
use crate::plugins::automation::AutomationLane;
use crate::plugins::{ParamInfo, PluginInstanceInfo};

/// Automation lanes document (Plan E Task 10) — the in-memory half of the
/// former standalone `plugins::automation::AutomationStore` +
/// process-global `STORE`, moved under the Session lock (round-2 §4.1's
/// cross-store atomicity: automation edits are now tx-atomic with every
/// other document field, mirroring `MidiStore`'s earlier merge). No
/// `loaded_dir` field — unlike the old lazily-resyncing store, epochs
/// (project open/create) adopt eagerly (`plugins::automation::
/// adopt_open_project`), so there is nothing left to lazily reconcile.
#[derive(Debug, Default)]
pub struct AutomationDoc {
    pub lanes: Vec<AutomationLane>,
}

/// The registry's DOCUMENT half (Plan E Task 9, round-2 inventory rows
/// 12-15 + 14): instance rows, the generic-UI param mirror, and state blobs
/// parked for instances whose host half hasn't (yet) accepted them. The
/// LIVE half — the plugin main thread, the format hosts' own instance
/// tables, `PluginRegistry::scanned` — stays outside the session, reached
/// by id through `plugins::clap_host`/`plugins::lv2_host` (their own locks,
/// never the session's — [C1]).
///
/// `instances` is an ordered `Vec` (not a map), matching the
/// `TrackAdd`/`MidiClipAdd` precedent: `Op::PluginAdd`/`PluginRemove`
/// address a row by `index` (advisory) with store truth (by `id`) winning,
/// exactly like `ClipRemove`/`MidiClipRemove`.
///
/// `params` keeps the FULL `ParamInfo` mirror (not just the raw values) —
/// deliberately: ranges/names are synthesized PER INSTANCE for stub rows
/// (restore-before-activation, `plugins::state::restore_into_registry`) and
/// per real plugin once activated, so they are genuinely instance-scoped
/// document state, not a separate lookup keyed by plugin uid.
///
/// `pending_state` holds APST-ENCODED bytes (`plugins::state::encode_state`
/// — magic/version/kind/uid/data, self-describing) — the SAME bytes written
/// to `<project>/plugins/<id>.state`, so a blob can move between disk,
/// `PluginRemove.state`/`PluginSetState.state`, and this map without any
/// re-encoding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginDoc {
    pub instances: Vec<PluginInstanceInfo>,
    pub params: HashMap<String, Vec<ParamInfo>>,
    pub pending_state: HashMap<String, Vec<u8>>,
    /// Instance ids whose `pending_state` bytes are NEWER than whatever's
    /// on disk (or nothing is on disk yet for them) — Task 9 review round 1
    /// Critical-2: `save_snapshot_into_project`'s persist ladder previously
    /// let a merely-EXISTING `.state` file win over fresher `pending_state`
    /// bytes, so a second `PluginSetState` (e.g. a second zyn patch load
    /// into the same instance) was silently dropped on disk. Set by every
    /// `apply_raw` arm that writes `pending_state` with new authoritative
    /// bytes (`PluginRemove`'s captured blob, `PluginSetState`); cleared by
    /// `ControlPlane::execute_persist` for whichever ids
    /// `save_snapshot_into_project` actually wrote. NOT set by
    /// `restore_into_session` — a blob just read from that exact file is,
    /// by definition, not ahead of it.
    pub dirty_state: std::collections::HashSet<String>,
    /// Per-instance state-load counter, bumped by every `PluginSetState`
    /// (a zyn patch load, and its undo). `midi::playback::node_for_track`
    /// carries it in the live-node key, so a load retires the cached node
    /// and the next rebuild instantiates a replacement — without this the
    /// blob would sit in `lv2_host`'s `loaded_blob` reaching only FUTURE
    /// nodes, i.e. never the one currently rendering the track. Purely
    /// derived bookkeeping: it is never persisted or restored (a document
    /// re-opened from disk builds every node from scratch anyway), so it
    /// starts at 0 for each session and only ever moves forward.
    pub state_rev: HashMap<String, u64>,
}

pub struct Session {
    pub store: Store,
    pub midi: MidiStore,
    pub automation: AutomationDoc,
    /// Track F modulation graph (curves, bindings, automation clips).
    /// Source of truth after Task 7's facade. `automation` is the derived
    /// lane view `automation_get` still reads.
    pub modulation: crate::modulation::ModulationDoc,
    pub plugins: PluginDoc,
    pub(crate) rev: u64,
    /// Document-identity counter (fix round 1, Task 7 review finding 2):
    /// bumped exactly once, under the session lock, at every DOCUMENT-SWAP
    /// epoch boundary — `audio::project::ensure_default_project`,
    /// `ControlPlane::create_project_at`, `ControlPlane::open_project_epoch`,
    /// `ControlPlane::save_project_as_epoch` (their own `// epoch boundary:`
    /// markers are the bump sites; `save_project_mark`'s marker explicitly
    /// says "no document swap here" and does NOT bump). `Committed` captures
    /// this under the SAME `transact` lock that captures `rev`;
    /// `ControlPlane::execute_persist` re-locks afterward (persistence runs
    /// with the session lock released — round-2 §4) and refuses to persist
    /// if `session.epoch` has moved past what the commit captured: an epoch
    /// function interleaved between `transact` returning and `execute_persist`
    /// re-locking, so the document `execute_persist` would snapshot is no
    /// longer the one this commit's `PersistEffect` describes — silently
    /// writing it would either corrupt the NEW document (e.g. persist a
    /// stale in-memory edit into a project just opened) or silently drop the
    /// commit's own edit (its target document already moved on). The new
    /// epoch's own swap/save owns durability from that point forward.
    pub(crate) epoch: u64,
}

impl Session {
    pub fn new(store: Store, midi: MidiStore) -> Self {
        Self {
            store,
            midi,
            automation: AutomationDoc::default(),
            modulation: crate::modulation::ModulationDoc::default(),
            plugins: PluginDoc::default(),
            rev: 0,
            epoch: 0,
        }
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

    /// Clone of the plugin document, taken under the session lock so
    /// `ControlPlane::execute_persist` can write it to disk AFTER the lock
    /// is released (Task 9, same discipline as `midi_snapshot`).
    pub fn plugin_snapshot(&self) -> PluginDoc {
        self.plugins.clone()
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
                // `epoch` is captured under this SAME lock, alongside `rev`
                // — see `Session::epoch`'s doc for why `execute_persist`
                // needs this to detect an interleaved document swap.
                Ok(Committed { rev: guard.rev, epoch: guard.epoch, ops, inverses, effect, meta })
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

    /// Read-only view of the document store, for validating/deriving inside
    /// a transaction closure without a second lock (Plan E Task 7, §4.3) —
    /// e.g. `ops::add_track_tx` reading track count/color, or a wrapper
    /// validating a track's kind before applying a structural op, or a
    /// closure branching on current truth (e.g. "don't downgrade an
    /// in-flight recording", Task 12) under the SAME lock `apply` writes
    /// through — reading via a separate `self.session.lock()` outside the
    /// closure is a TOCTOU: another thread can write between that read and
    /// this transaction's commit. Never `&mut`: the only way to mutate is
    /// [`Tx::apply`], so this can't be used to bypass the op/inverse/effect
    /// bookkeeping.
    pub fn store(&self) -> &Store {
        &self.session.store
    }

    /// Same as [`Tx::store`], for the MIDI store.
    pub fn midi(&self) -> &MidiStore {
        &self.session.midi
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
    /// Plan E Task 9: plugin host round-trips a commit's plugin ops
    /// describe, EXECUTED by `ControlPlane::commit` after the session lock
    /// is released (§4.1/[C1]: hosts have their own locks, never called
    /// while the session lock is held) — see `HostForward`.
    pub host_forward: Vec<HostForward>,
}

/// A plugin-host round-trip `apply_raw` determined is needed, described
/// here (plain data — id + numbers, no host handle anywhere in this
/// module), EXECUTED by `ControlPlane::commit` by calling the SAME host
/// entry points commands call today (`plugins::clap_host`/`lv2_host`/
/// `state`'s `HostStateBridge`), now sequenced after the session lock.
#[derive(Debug, Clone, PartialEq)]
pub enum HostForward {
    /// Forward a single (already-clamped) param value from `Op::Set{Plugin,
    /// Param}` to the format host — mirrors `plugins::set_params_and_forward`.
    ParamWrite { instance: String, index: u32, value: f32 },
    /// Ensure `instance` is live on its format host, then mirror the real
    /// param list + `"active"` status back into the document (short session
    /// lock, post host call). Pushed by every `Op::PluginAdd` apply — the
    /// executor is idempotent (checks `has_instance` first), so the
    /// prepare-outside fresh-instantiate path (host already live) safely
    /// no-ops the host call and just re-syncs params; the undo-of-remove
    /// path (host gone, destroyed by the forward `Destroy`) actually
    /// re-instantiates.
    Instantiate { instance: String },
    /// Destroy `instance` on its format host (`Op::PluginRemove`'s forward
    /// direction).
    Destroy { instance: String },
    /// Offer `session.plugins.pending_state[instance]` to the registered
    /// `HostStateBridge` (`Op::PluginAdd` when a blob is pending —
    /// restore-from-blob undo — and `Op::PluginSetState`'s forward
    /// direction — zyn patch loads).
    LoadState { instance: String },
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
    /// `plugins::state::save_snapshot_into_project` from a `PluginDoc`
    /// snapshot taken under a short session lock (Task 9).
    pub plugins: bool,
    /// Task 10 wires the executor; the field exists now.
    pub automation: bool,
    /// Track F: `modulation::persist::save_into_project` from a
    /// `ModulationDoc` snapshot. Authoritative once the Task 7 facade
    /// owns the document — see `execute_persist`.
    pub modulation: bool,
}

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
        self.modulation |= other.modulation;
    }
}

pub struct Committed {
    pub rev: u64,
    /// `Session::epoch` as observed under the SAME lock as `rev` — see that
    /// field's doc. `ControlPlane::execute_persist` compares this against
    /// the CURRENT `session.epoch` after re-locking, post-`transact`.
    pub epoch: u64,
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
/// Keep `session.automation` as the derived lane view of the modulation
/// document so `automation_get` and the plugin-param driver stay in sync
/// when the new commands mutate curves/bindings directly.
fn sync_derived_lanes(session: &mut Session) {
    session.automation.lanes = crate::modulation::compat::lanes_from_doc(&session.modulation);
}

fn apply_raw(session: &mut Session, op: &Op, effect: &mut EngineEffect) -> Result<Op, String> {
    match op {
        Op::Set { object: ObjectRef::Track(id), path, to, .. } => {
            let t = session
                .store
                .tracks
                .iter_mut()
                .find(|t| &t.id == id)
                .ok_or_else(|| format!("unknown track: {id}"))?;
            let from_now = read_prop(t, *path)?; // truth, not caller's `from`
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
        Op::TrackAdd { track, index, clips, clip_indices, automation_clips, bindings } => {
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
            if !automation_clips.is_empty() || !bindings.is_empty() {
                for c in automation_clips {
                    if session.modulation.automation_clips.iter().all(|x| x.id != c.id) {
                        session.modulation.automation_clips.push(c.clone());
                    }
                }
                for b in bindings {
                    if session.modulation.bindings.iter().all(|x| x.id != b.id) {
                        session.modulation.bindings.push(b.clone());
                    }
                }
                effect.persist.modulation = true;
                sync_derived_lanes(session);
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
            // Automation clips and bindings sourced from this track leave
            // with it (Task 9 review): compile expands `clips_on(track_id)`
            // without checking the track still exists, so leaving them
            // would keep driving targets after the row is gone.
            let mut removed_auto_clips = Vec::new();
            session.modulation.automation_clips.retain(|c| {
                let keep = c.track_id != track.id.as_str();
                if !keep {
                    removed_auto_clips.push(c.clone());
                }
                keep
            });
            let mut removed_bindings = Vec::new();
            session.modulation.bindings.retain(|b| {
                let drop = matches!(
                    &b.source,
                    crate::modulation::Source::AutomationTrack { track_id } if track_id == track.id.as_str()
                );
                if drop {
                    removed_bindings.push(b.clone());
                }
                !drop
            });
            if !removed_auto_clips.is_empty() || !removed_bindings.is_empty() {
                effect.persist.modulation = true;
                sync_derived_lanes(session);
            }
            effect.rebuild = true;
            // Removing a track can flip the store-wide any_solo flag (the
            // removed row may have been the only soloed track) — recompute
            // it here, same as TrackAdd's Set-path recompute above. A fresh
            // TrackAdd row is never soloed (ops::new_track_row), so adding
            // never changes any_solo and doesn't need this.
            effect.any_solo = Some(session.store.any_solo());
            Ok(Op::TrackAdd {
                track: removed, index: pos, clips: removed_clips, clip_indices: removed_indices,
                automation_clips: removed_auto_clips, bindings: removed_bindings,
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
        // Plan E Task 8: LoopJam's region-replacement trims a clip IN PLACE
        // (shrink its length, or — paired with `OffsetSamples` below —
        // reposition its head) instead of a remove-then-fresh-id-add, so an
        // undo restores the exact original row. Same structural-in-effect
        // reasoning as `TimelineStartSamples` above (a trimmed clip changes
        // what the RT graph renders).
        Op::Set { object: ObjectRef::Clip(id), path: PropPath::LengthSamples, to, .. } => {
            let c = session
                .store
                .clips
                .iter_mut()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("unknown clip: {id}"))?;
            let from_now = serde_json::json!(c.length_samples); // truth, not caller's `from`
            let v = to.as_u64().ok_or("lengthSamples: expected a non-negative integer")?;
            c.length_samples = v;
            let applied = serde_json::json!(c.length_samples);
            effect.rebuild = true;
            effect.persist.project = true;
            Ok(Op::Set {
                object: ObjectRef::Clip(id.clone()),
                path: PropPath::LengthSamples,
                from: applied, // the inverse's `from`: value we just wrote
                to: from_now,  // the inverse's `to`: value to restore
            })
        }
        Op::Set { object: ObjectRef::Clip(id), path: PropPath::OffsetSamples, to, .. } => {
            let c = session
                .store
                .clips
                .iter_mut()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("unknown clip: {id}"))?;
            let from_now = serde_json::json!(c.offset_samples); // truth, not caller's `from`
            let v = to.as_u64().ok_or("offsetSamples: expected a non-negative integer")?;
            c.offset_samples = v;
            let applied = serde_json::json!(c.offset_samples);
            effect.rebuild = true;
            effect.persist.project = true;
            Ok(Op::Set {
                object: ObjectRef::Clip(id.clone()),
                path: PropPath::OffsetSamples,
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
        // Plan E Task 5: the MIDI half of the op vocabulary — tempo/meter,
        // MIDI clip structural add/remove, MIDI clip bounds, and note
        // replacement, all through the same session lock as `store` (round-2
        // §4.1's cross-store atomicity).
        Op::TempoSet { ppq, events, meter } => {
            // Validate BEFORE any mutation — atomic (round-2 §4: a failed
            // apply must leave the session untouched).
            if events.is_empty() {
                return Err("tempoSet: events must not be empty".into());
            }
            if !events.windows(2).all(|w| w[0].tick <= w[1].tick) {
                return Err("tempoSet: events must be sorted by tick".into());
            }
            if !meter.windows(2).all(|w| w[0].tick <= w[1].tick) {
                return Err("tempoSet: meter must be sorted by tick".into());
            }
            let inverse = Op::TempoSet {
                ppq: session.midi.ppq,
                events: session.midi.tempo_events.clone(),
                meter: session.midi.meter_events.clone(),
            };
            // Cross-store atomicity (§4.1, this task's reason to exist): the
            // three midi fields AND the store.transport.tempo_bpm mirror
            // write together, under the one session lock — today's
            // `set_tempo_map` command does this as two separate,
            // non-atomic phases (round-2 inventory row 4).
            session.midi.ppq = *ppq;
            session.midi.tempo_events = events.clone();
            session.midi.meter_events = meter.clone();
            session.store.transport.tempo_bpm = events[0].bpm;
            // A new tempo map changes every tick->sample conversion, which
            // `engine::rebuild`'s `midi::playback::append_from` bakes into
            // the RT track's pre-rendered note events — only invoked from
            // `rebuild`, never at render time — so this needs a rebuild.
            effect.rebuild = true;
            effect.persist.midi = true;
            effect.persist.project = true; // the transport.tempo_bpm mirror lives in project.json
            Ok(inverse)
        }
        Op::MidiClipAdd { clip, index } => {
            if session.midi.clips.iter().any(|c| c.id == clip.id) {
                return Err(format!("duplicate midi clip id: {}", clip.id));
            }
            let idx = (*index).min(session.midi.clips.len());
            session.midi.clips.insert(idx, clip.clone());
            effect.rebuild = true;
            effect.persist.midi = true;
            Ok(Op::MidiClipRemove { clip: clip.clone(), index: idx })
        }
        Op::MidiClipRemove { clip, .. } => {
            // Found by id, not blindly by the caller's recorded index —
            // store truth wins (same rule as `ClipRemove`).
            let pos = session
                .midi
                .clips
                .iter()
                .position(|c| c.id == clip.id)
                .ok_or_else(|| format!("unknown midi clip: {}", clip.id))?;
            let removed = session.midi.clips.remove(pos);
            effect.rebuild = true;
            effect.persist.midi = true;
            Ok(Op::MidiClipAdd { clip: removed, index: pos })
        }
        // MIDI clip bounds: property-addressed, but structural in effect
        // (rebuild + persist) for the same reason `Set{Clip,
        // TimelineStartSamples}` is (Task 3) — `engine::rebuild`'s
        // `midi::playback::append_from` reads `session.midi.clips`'
        // placement/content-length fields to pre-render the RT track's note
        // events, and is only ever invoked from `rebuild`, never at render
        // time via a live table read — so any bounds edit needs a rebuild
        // to become audible.
        Op::Set { object: ObjectRef::MidiClip(id), path, to, .. } => {
            let c = session
                .midi
                .clips
                .iter_mut()
                .find(|c| &c.id == id)
                .ok_or_else(|| format!("unknown midi clip: {id}"))?;
            let from_now = read_midi_prop(c, *path)?; // truth, not caller's `from`
            let applied = write_midi_prop(c, *path, to)?; // clamps LengthTicks like write_prop clamps Gain
            // Only the placement/content-length paths feed `append_from`'s
            // pre-render (see the arm's doc above); `Name` is document-only,
            // so a rename must not cost a full engine rebuild.
            effect.rebuild = !matches!(path, PropPath::Name);
            effect.persist.midi = true;
            Ok(Op::Set {
                object: ObjectRef::MidiClip(id.clone()),
                path: *path,
                from: applied, // the inverse's `from`: value we just wrote
                to: from_now,  // the inverse's `to`: value to restore
            })
        }
        // §4.4 value-replacement wrapper: whole-notes replace, diffed
        // against store truth via `assign_incoming_note_ids`'s keep-rule
        // (midi/mod.rs) — same rebuild reasoning as the bounds arm above
        // (a note edit changes what `append_from` pre-renders).
        Op::MidiSetNotes { clip: clip_id, notes } => {
            let c = session
                .midi
                .clips
                .iter_mut()
                .find(|c| &c.id == clip_id)
                .ok_or_else(|| format!("unknown midi clip: {clip_id}"))?;
            // `from` inverse = current notes (already live-id-resolved from
            // whatever produced them), captured before this write.
            let previous_notes = c.notes.clone();
            // Everything computed on LOCALS first (assign_incoming_note_ids
            // is pure); `c.notes`/`c.next_note_id` are assigned only as the
            // last statements, mirroring `midi_set_notes`'s own discipline
            // (midi/mod.rs) so no partial state is observable on early
            // return.
            let (local_notes, next_watermark) =
                crate::midi::assign_incoming_note_ids(&c.notes, c.next_note_id, notes.clone());
            c.notes = local_notes;
            // Watermark rule (scope ruling 3, ADR 0001): advances
            // monotonically, NEVER rewound by the inverse — the inverse
            // below restores `previous_notes`' VALUES, but going through
            // this same arm again means it too only ever advances
            // `next_note_id`, never resets it.
            c.next_note_id = next_watermark;
            effect.rebuild = true;
            effect.persist.midi = true;
            Ok(Op::MidiSetNotes { clip: clip_id.clone(), notes: previous_notes })
        }
        // Plan E Task 12 (inventory rows 28-29): the transport family — a
        // property-addressed mirror of what `ControlPlane::transport` used
        // to write directly into `session.store.transport.*` in four
        // places. Deliberately sets NO `effect` flags:
        // * no `rebuild` — the transport isn't baked into the RT graph by
        //   `engine::rebuild` the way clip/track structure is; the RT side
        //   reads its own atomics (`SharedRt`), not this document mirror.
        // * no `persist.project` — RULING (Task 12 brief, binding),
        //   diverging from the generic "structural/property Set persists"
        //   pattern the Clip/MidiClip arms above follow: these fields DO
        //   live in project.json, but setting `persist.project` on every
        //   transport commit would write project.json once per
        //   play/stop/loop-drag gesture. Persistence for transport fields
        //   rides explicit saves only, exactly as before this task.
        Op::Set { object: ObjectRef::Transport, path, to, .. } => {
            let t = &mut session.store.transport;
            let from_now = read_transport_prop(t, *path)?; // truth, not caller's `from`
            let applied = write_transport_prop(t, *path, to)?;
            Ok(Op::Set {
                object: ObjectRef::Transport,
                path: *path,
                from: applied, // the inverse's `from`: value we just wrote
                to: from_now,  // the inverse's `to`: value to restore
            })
        }
        // §4.4 value-replacement wrapper (Plan E Task 10): one op replaces
        // (or deletes) ONE lane's full point set — an edit gesture is one
        // lane write, never one op per point (D-03), mirroring
        // `MidiSetNotes`. Addressed by `key` (== `AutomationLane::id`, a
        // plain string) rather than `ObjectRef` — automation lanes have no
        // struct key, just a string id, so no new `ObjectRef` variant is
        // needed (checked against the brief's NEEDS_CONTEXT trigger).
        //
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
        Op::AutomationSetLane { key, lane } => {
            let pos = session.automation.lanes.iter().position(|l| &l.id == key);
            // Validate + normalize BEFORE any mutation (atomic — round-2 §4:
            // a failed apply must leave the session untouched).
            let mut new_lane = match lane {
                Some(l) => {
                    let mut nl = l.clone();
                    nl.id = key.clone(); // the op's `key` is authoritative
                    crate::plugins::automation::normalize_lane(&mut nl)?;
                    Some(nl)
                }
                None => None,
            };
            // Upsert/remove, capturing whatever was there before (store
            // truth) as the inverse's payload — `None` when the key didn't
            // exist (mirrors `Op::Set`'s "inverse from store truth, never
            // guessed" rule).
            // Facade: journals replay into the modulation document.
            // `session.automation` stays as the derived view the existing
            // tests and `automation_get` inspect; the pair is written
            // together so a persist of either half describes the same edit.
            crate::modulation::compat::apply_lane(&mut session.modulation, key, new_lane.as_ref());
            let previous = match pos {
                Some(idx) => match new_lane.take() {
                    Some(nl) => Some(std::mem::replace(&mut session.automation.lanes[idx], nl)),
                    None => Some(session.automation.lanes.remove(idx)),
                },
                None => {
                    if let Some(nl) = new_lane.take() {
                        session.automation.lanes.push(nl);
                    }
                    None // deleting an already-absent lane is a no-op, not an error
                }
            };
            effect.persist.automation = true;
            effect.persist.modulation = true;
            effect.rebuild = true;
            Ok(Op::AutomationSetLane { key: key.clone(), lane: previous })
        }
        // Track F: modulation document arms. Same shape as
        // `AutomationSetLane` — validate/normalize BEFORE mutation,
        // upsert/remove by key, previous store truth as inverse, rebuild.
        // `Op::AutomationSetLane` also writes `session.modulation` via
        // `modulation::compat` so old journals replay into the new document.
        Op::ModulationSetCurve { key, curve } => {
            let pos = session.modulation.curves.iter().position(|c| &c.id == key);
            let mut new_curve = match curve {
                Some(c) => {
                    let mut nc = c.clone();
                    nc.id = key.clone(); // the op's `key` is authoritative
                    crate::modulation::normalize_curve(&mut nc)?;
                    Some(nc)
                }
                None => None,
            };
            let previous = match pos {
                Some(idx) => match new_curve.take() {
                    Some(nc) => Some(std::mem::replace(&mut session.modulation.curves[idx], nc)),
                    None => Some(session.modulation.curves.remove(idx)),
                },
                None => {
                    if let Some(nc) = new_curve.take() {
                        session.modulation.curves.push(nc);
                    }
                    None // deleting an already-absent curve is a no-op
                }
            };
            effect.persist.modulation = true;
            effect.rebuild = true;
            sync_derived_lanes(session);
            Ok(Op::ModulationSetCurve { key: key.clone(), curve: previous })
        }
        Op::ModulationSetBinding { key, binding } => {
            let pos = session.modulation.bindings.iter().position(|b| &b.id == key);
            let new_binding = match binding {
                Some(b) => {
                    let mut nb = b.clone();
                    nb.id = key.clone();
                    crate::modulation::validate_binding(&nb)?;
                    Some(nb)
                }
                None => None,
            };
            let previous = match pos {
                Some(idx) => match new_binding {
                    Some(nb) => Some(std::mem::replace(&mut session.modulation.bindings[idx], nb)),
                    None => Some(session.modulation.bindings.remove(idx)),
                },
                None => {
                    if let Some(nb) = new_binding {
                        session.modulation.bindings.push(nb);
                    }
                    None
                }
            };
            effect.persist.modulation = true;
            effect.rebuild = true;
            sync_derived_lanes(session);
            Ok(Op::ModulationSetBinding { key: key.clone(), binding: previous })
        }
        Op::AutomationClipSet { key, clip } => {
            let pos = session.modulation.automation_clips.iter().position(|c| &c.id == key);
            let new_clip = match clip {
                Some(c) => {
                    let mut nc = c.clone();
                    nc.id = key.clone();
                    if nc.id.is_empty() {
                        return Err("automation clip id must not be empty".into());
                    }
                    if nc.track_id.is_empty() {
                        return Err("automation clip trackId must not be empty".into());
                    }
                    if nc.curve_id.is_empty() {
                        return Err("automation clip curveId must not be empty".into());
                    }
                    Some(nc)
                }
                None => None,
            };
            let previous = match pos {
                Some(idx) => match new_clip {
                    Some(nc) => {
                        Some(std::mem::replace(&mut session.modulation.automation_clips[idx], nc))
                    }
                    None => Some(session.modulation.automation_clips.remove(idx)),
                },
                None => {
                    if let Some(nc) = new_clip {
                        session.modulation.automation_clips.push(nc);
                    }
                    None
                }
            };
            effect.persist.modulation = true;
            effect.rebuild = true;
            sync_derived_lanes(session);
            Ok(Op::AutomationClipSet { key: key.clone(), clip: previous })
        }
        // Plan E Task 9 (round-2 inventory rows 12-15): plugin instance
        // rows. Same store-truth-wins-by-id discipline as
        // ClipAdd/ClipRemove/MidiClipAdd/MidiClipRemove.
        Op::PluginAdd { row, index } => {
            if session.plugins.instances.iter().any(|r| r.id == row.id) {
                return Err(format!("duplicate plugin instance id: {}", row.id));
            }
            let idx = (*index).min(session.plugins.instances.len());
            session.plugins.instances.insert(idx, row.clone());
            // Seed an EMPTY params mirror only when nothing is parked there
            // already (`.or_default()` is a no-op on an Occupied entry) —
            // Task 9 review round 1, Important-1: undo-of-remove parks the
            // removed row's real mirror in `session.plugins.params` (see
            // `PluginRemove` below) instead of clearing it, specifically so
            // THIS seed doesn't clobber it with an empty vec. A genuinely
            // fresh instantiate's id has never been seen before, so this
            // still seeds empty there; `HostForward::Instantiate` fills
            // real values in — but ONLY when the mirror is still empty
            // (same round-1 fix, control/mod.rs), so it never clobbers a
            // restored mirror with the host's post-reinstantiate defaults.
            session.plugins.params.entry(row.id.clone()).or_default();
            effect.rebuild = true;
            effect.persist.plugins = true;
            // Always folded in (doc comment on the op): the executor is the
            // one that knows whether the host call is actually needed.
            effect.host_forward.push(HostForward::Instantiate { instance: row.id.clone() });
            if session.plugins.pending_state.contains_key(&row.id) {
                effect.host_forward.push(HostForward::LoadState { instance: row.id.clone() });
            }
            Ok(Op::PluginRemove {
                row: row.clone(),
                index: idx,
                state: session.plugins.pending_state.get(&row.id).cloned(),
                params: session.plugins.params.get(&row.id).cloned().unwrap_or_default(),
            })
        }
        Op::PluginRemove { row, state, params, .. } => {
            // Found by id, not blindly by the caller's recorded index —
            // store truth wins (same rule as `ClipRemove`/`TrackRemove`).
            let pos = session
                .plugins
                .instances
                .iter()
                .position(|r| r.id == row.id)
                .ok_or_else(|| format!("unknown plugin instance: {}", row.id))?;
            let removed = session.plugins.instances.remove(pos);
            // PARK the param mirror — do NOT clear it (Task 9 review round
            // 1, Important-1). `Op::PluginAdd`'s fixed `{row, index}` shape
            // has no field to carry the mirror back through on undo, so it
            // travels via the document itself: a later `PluginAdd` for this
            // SAME id (undo replaying THIS op's inverse) finds it already
            // here and its `.or_default()` seed leaves it untouched. A
            // permanent removal (never undone) leaves a small, bounded
            // orphaned entry, same trade-off `pending_state` already makes
            // below — both are GC'd wholesale on the next
            // `restore_into_session`/project open.
            //
            // L-1 CLOSED (whole-branch review): parking is an IN-MEMORY
            // fact, and a COLD REPLAY has nothing parked — a fresh process
            // applying journal lines would restore the row with an empty
            // mirror, which is exactly the gap `Op::PluginRemove.params`
            // was captured for and then never read. Seed from the op's own
            // field when the in-memory mirror is absent or empty, so the op
            // is self-describing in fact. In-process this changes nothing:
            // the parked mirror is already populated and wins, so an undo
            // still restores the REAL values rather than whatever the op
            // recorded.
            let mirror = session.plugins.params.entry(removed.id.clone()).or_default();
            if mirror.is_empty() && !params.is_empty() {
                *mirror = params.clone();
            }
            effect.rebuild = true;
            effect.persist.plugins = true;
            // Park (or clear) the state blob: `state` is the caller-captured
            // host blob (host `save_state`, taken outside the lock before
            // this op was applied) — `None` when the instance had nothing to
            // save. Preserved verbatim so a later undo (`PluginAdd`) can
            // restore it via `HostForward::LoadState`.
            match state {
                Some(bytes) => {
                    session.plugins.pending_state.insert(removed.id.clone(), bytes.clone());
                    // Dirty (Task 9 review round 1, Critical-2): these bytes
                    // are NEWER than whatever `.state` file (if any) is on
                    // disk for this id — `save_snapshot_into_project`'s
                    // persist ladder must write them, not silently keep the
                    // stale file just because it happens to exist.
                    session.plugins.dirty_state.insert(removed.id.clone());
                }
                None => {
                    session.plugins.pending_state.remove(&removed.id);
                    session.plugins.dirty_state.remove(&removed.id);
                }
            }
            effect.host_forward.push(HostForward::Destroy { instance: removed.id.clone() });
            Ok(Op::PluginAdd { row: removed, index: pos })
        }
        Op::PluginSetState { instance, state } => {
            if !session.plugins.instances.iter().any(|r| r.id == *instance) {
                return Err(format!("unknown plugin instance: {instance}"));
            }
            let previous = session.plugins.pending_state.get(instance).cloned();
            session.plugins.pending_state.insert(instance.clone(), state.clone());
            // Dirty (Critical-2, same reasoning as `PluginRemove` above):
            // these are new authoritative bytes, not necessarily reflected
            // in whatever `.state` file already exists on disk.
            session.plugins.dirty_state.insert(instance.clone());
            // The blob only becomes AUDIBLE through a fresh RT node (see
            // `state_rev`'s doc): bump the revision so the node key changes,
            // and ask for the rebuild that acts on it. `ControlPlane::commit`
            // runs `host_forward` BEFORE the rebuild, so the host already
            // holds the new blob when the replacement node is instantiated.
            *session.plugins.state_rev.entry(instance.clone()).or_insert(0) += 1;
            effect.rebuild = true;
            effect.persist.plugins = true;
            effect.host_forward.push(HostForward::LoadState { instance: instance.clone() });
            Ok(Op::PluginSetState {
                instance: instance.clone(),
                // Inverse restores whatever was there before (which may be
                // "nothing" — an instance that never had a blob) by
                // reapplying an EMPTY blob is wrong (that would forward-load
                // zero bytes into the host); an absent previous blob simply
                // means there is nothing meaningful to undo TO, so the
                // instance's own current blob is kept as a harmless
                // self-inverse in that corner case (no data loss either way
                // — never observed by `zyn_load_patch`, which always saves a
                // real prior blob first).
                state: previous.unwrap_or_else(|| state.clone()),
            })
        }
        // Plugin params: property-addressed, mirrors `Op::Set{Track, Gain}`
        // — clamped write, inverse from store truth, folds like mix Sets
        // (coalescable per (object, path) — a knob drag is Sets on one
        // instance/param).
        Op::Set { object: ObjectRef::Plugin(id), path: PropPath::Param { index }, to, .. } => {
            if !session.plugins.instances.iter().any(|r| &r.id == id) {
                return Err(format!("unknown plugin instance: {id}"));
            }
            let v = to.as_f64().ok_or("param: expected a number")?;
            let params = session.plugins.params.entry(id.clone()).or_default();
            let (from_now, applied) = match params.iter_mut().find(|p| p.id == *index) {
                Some(p) => {
                    let from_now = p.value;
                    p.value = v.clamp(p.min, p.max);
                    (from_now, p.value)
                }
                None => {
                    // Unseen param id (mirrors the retired
                    // `PluginRegistry::set_params`'s dynamic-entry path for
                    // stub instances whose real ranges aren't known yet): a
                    // fresh 0..1-spanning entry, clamped the same way.
                    let clamped = v.clamp(0.0, 1.0);
                    params.push(ParamInfo {
                        id: *index,
                        name: format!("param {index}"),
                        min: 0.0,
                        max: 1.0,
                        default: 0.0,
                        value: clamped,
                        steps: 0,
                    });
                    (0.0, clamped)
                }
            };
            effect.persist.plugins = true;
            effect.host_forward.push(HostForward::ParamWrite {
                instance: id.clone(),
                index: *index,
                value: applied as f32,
            });
            Ok(Op::Set {
                object: ObjectRef::Plugin(id.clone()),
                path: PropPath::Param { index: *index },
                from: serde_json::json!(applied), // the inverse's `from`: value we just wrote
                to: serde_json::json!(from_now),  // the inverse's `to`: value to restore
            })
        }
        _ => Err("op not yet supported".into()),
    }
}

/// Read a track property as its JSON-wire representation (round-trippable
/// through `write_prop`). Fallible: `PropPath` and `ObjectRef` are
/// independent serde-derived enums, so a deserialized (e.g. replayed
/// journal, or agent-authored) `Op::Set` can pair `ObjectRef::Track` with a
/// Clip-only path like `TimelineStartSamples` — that must fail the
/// transaction atomically (via `apply_raw`'s `?`), never panic a `transact`
/// closure (round-2 §4's no-panic-in-transact invariant).
fn read_prop(t: &TrackState, path: PropPath) -> Result<serde_json::Value, String> {
    match path {
        PropPath::Gain => Ok(serde_json::json!(t.gain_db)),
        PropPath::Pan => Ok(serde_json::json!(t.pan)),
        PropPath::Muted => Ok(serde_json::json!(t.muted)),
        PropPath::Soloed => Ok(serde_json::json!(t.soloed)),
        PropPath::Armed => Ok(serde_json::json!(t.armed)),
        // Option<String> serializes as a JSON string or null, never a
        // wrapping object — same wire shape `write_prop` below accepts.
        PropPath::InstrumentId => Ok(serde_json::json!(t.instrument_id)),
        PropPath::Name
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TimelineStartTicks
        | PropPath::LengthTicks
        | PropPath::ContentLengthTicks
        | PropPath::TransportState
        | PropPath::LoopEnabled
        | PropPath::LoopStartSamples
        | PropPath::LoopEndSamples
        | PropPath::StopAtEnd
        | PropPath::SampleRate
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a Track property (it's a Clip/MidiClip/Transport/Plugin path)"))
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
        PropPath::Name
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TimelineStartTicks
        | PropPath::LengthTicks
        | PropPath::ContentLengthTicks
        | PropPath::TransportState
        | PropPath::LoopEnabled
        | PropPath::LoopStartSamples
        | PropPath::LoopEndSamples
        | PropPath::StopAtEnd
        | PropPath::SampleRate
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a Track property (it's a Clip/MidiClip/Transport/Plugin path)"))
        }
    }
}

/// Read a MIDI clip property as its JSON-wire representation (mirrors
/// `read_prop` for `TrackState`, scoped to `MidiClip`'s three
/// placement/content-length paths). Fallible for the same reason:
/// `PropPath` and `ObjectRef` are independent serde-derived enums, so a
/// `Set{MidiClip, Gain}` (a Track-only path) must fail the transaction
/// atomically, never panic.
fn read_midi_prop(c: &crate::midi::types::MidiClip, path: PropPath) -> Result<serde_json::Value, String> {
    match path {
        PropPath::Name => Ok(serde_json::json!(c.name)),
        PropPath::TimelineStartTicks => Ok(serde_json::json!(c.timeline_start_ticks)),
        PropPath::LengthTicks => Ok(serde_json::json!(c.length_ticks)),
        PropPath::ContentLengthTicks => Ok(serde_json::json!(c.content_length_ticks)),
        PropPath::Gain
        | PropPath::Pan
        | PropPath::Muted
        | PropPath::Soloed
        | PropPath::Armed
        | PropPath::InstrumentId
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TransportState
        | PropPath::LoopEnabled
        | PropPath::LoopStartSamples
        | PropPath::LoopEndSamples
        | PropPath::StopAtEnd
        | PropPath::SampleRate
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a MidiClip property"))
        }
    }
}

/// Write a MIDI clip property, mirroring `write_prop`'s clamp discipline:
/// `LengthTicks` clamps to >= 1 on write (the write side is the clamp
/// authority, mirroring the gain-clamp round-trip rule — Plan A) so the
/// inverse observes the post-clamp value, never the caller's raw (possibly
/// zero) `to`. Returns the as-applied (post-clamp) value.
fn write_midi_prop(
    c: &mut crate::midi::types::MidiClip,
    path: PropPath,
    to: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match path {
        PropPath::Name => {
            let v = to.as_str().ok_or("name: expected a string")?.trim();
            if v.is_empty() {
                return Err("name: must not be empty".into());
            }
            c.name = v.to_string();
            Ok(serde_json::json!(c.name))
        }
        PropPath::TimelineStartTicks => {
            let v = to.as_u64().ok_or("timelineStartTicks: expected a non-negative integer")?;
            c.timeline_start_ticks = v;
            Ok(serde_json::json!(c.timeline_start_ticks))
        }
        PropPath::LengthTicks => {
            let v = to.as_u64().ok_or("lengthTicks: expected a non-negative integer")?;
            c.length_ticks = v.max(1); // clamp, never Err — mirrors the gain clamp
            Ok(serde_json::json!(c.length_ticks))
        }
        PropPath::ContentLengthTicks => {
            let v = match to {
                serde_json::Value::Null => None,
                serde_json::Value::Number(n) => Some(
                    n.as_u64()
                        .ok_or("contentLengthTicks: expected a non-negative integer or null")?,
                ),
                _ => return Err("contentLengthTicks: expected a non-negative integer or null".into()),
            };
            c.content_length_ticks = v;
            Ok(serde_json::json!(c.content_length_ticks))
        }
        PropPath::Gain
        | PropPath::Pan
        | PropPath::Muted
        | PropPath::Soloed
        | PropPath::Armed
        | PropPath::InstrumentId
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TransportState
        | PropPath::LoopEnabled
        | PropPath::LoopStartSamples
        | PropPath::LoopEndSamples
        | PropPath::StopAtEnd
        | PropPath::SampleRate
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a MidiClip property"))
        }
    }
}

/// Read/write a transport property as its JSON-wire representation —
/// mirrors `read_prop`/`write_prop` (`TrackState`) and
/// `read_midi_prop`/`write_midi_prop` (`MidiClip`), scoped to the six
/// `Transport` paths. Fallible for the same reason: `PropPath` and
/// `ObjectRef` are independent serde-derived enums, so e.g.
/// `Set{Transport, Gain}` (a Track-only path) must fail the transaction
/// atomically, never panic.
fn read_transport_prop(t: &TransportState, path: PropPath) -> Result<serde_json::Value, String> {
    match path {
        PropPath::TransportState => Ok(serde_json::json!(t.state)),
        PropPath::LoopEnabled => Ok(serde_json::json!(t.loop_enabled)),
        PropPath::LoopStartSamples => Ok(serde_json::json!(t.loop_start_samples)),
        PropPath::LoopEndSamples => Ok(serde_json::json!(t.loop_end_samples)),
        PropPath::StopAtEnd => Ok(serde_json::json!(t.stop_at_end)),
        PropPath::SampleRate => Ok(serde_json::json!(t.sample_rate)),
        PropPath::Gain
        | PropPath::Pan
        | PropPath::Muted
        | PropPath::Soloed
        | PropPath::Armed
        | PropPath::InstrumentId
        | PropPath::Name
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TimelineStartTicks
        | PropPath::LengthTicks
        | PropPath::ContentLengthTicks
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a Transport property"))
        }
    }
}

/// Write a transport property. `TransportState`'s wire form is validated
/// against the closed `"playing"|"stopped"|"recording"` set (the same set
/// `audio/engine.rs` writes directly); the numeric fields are u64/u32
/// wire-checked like their Clip/MidiClip counterparts, with no clamping
/// (unlike `Gain`/`LengthTicks`) since a transport commit's caller —
/// `ControlPlane::transport` — already validates the loop region before
/// ever building the op.
fn write_transport_prop(
    t: &mut TransportState,
    path: PropPath,
    to: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match path {
        PropPath::TransportState => {
            let v = to.as_str().ok_or("transportState: expected a string")?;
            if !matches!(v, "playing" | "stopped" | "recording") {
                return Err(format!(
                    "transportState: expected \"playing\"|\"stopped\"|\"recording\", got {v:?}"
                ));
            }
            t.state = v.to_string();
            Ok(serde_json::json!(t.state))
        }
        PropPath::LoopEnabled => {
            let v = to.as_bool().ok_or("loopEnabled: expected a bool")?;
            t.loop_enabled = v;
            Ok(serde_json::json!(t.loop_enabled))
        }
        PropPath::LoopStartSamples => {
            let v = to.as_u64().ok_or("loopStartSamples: expected a non-negative integer")?;
            t.loop_start_samples = v;
            Ok(serde_json::json!(t.loop_start_samples))
        }
        PropPath::LoopEndSamples => {
            let v = to.as_u64().ok_or("loopEndSamples: expected a non-negative integer")?;
            t.loop_end_samples = v;
            Ok(serde_json::json!(t.loop_end_samples))
        }
        PropPath::StopAtEnd => {
            let v = to.as_bool().ok_or("stopAtEnd: expected a bool")?;
            t.stop_at_end = v;
            Ok(serde_json::json!(t.stop_at_end))
        }
        PropPath::SampleRate => {
            let v = to
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("sampleRate: expected a non-negative 32-bit integer")?;
            t.sample_rate = v;
            Ok(serde_json::json!(t.sample_rate))
        }
        PropPath::Gain
        | PropPath::Pan
        | PropPath::Muted
        | PropPath::Soloed
        | PropPath::Armed
        | PropPath::InstrumentId
        | PropPath::Name
        | PropPath::TimelineStartSamples
        | PropPath::LengthSamples
        | PropPath::OffsetSamples
        | PropPath::TimelineStartTicks
        | PropPath::LengthTicks
        | PropPath::ContentLengthTicks
        | PropPath::Param { .. } => {
            Err(format!("path {path:?} is not a Transport property"))
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
/// NON-`Set` OPS NEVER FOLD AWAY (Plan E review ask, made explicit here so
/// nobody "optimizes" it later): only the `Grouped` arm can drop an entry,
/// and only when a group's net `from == to`. Every non-`Set` op takes the
/// `Passthrough` arm, which always emits both the op and its inverse — so a
/// structurally no-op mutation still reaches history and the journal. A
/// `Op::AutomationSetLane { lane: None }` that deletes an already-absent
/// lane changes nothing in the document, and it STILL journals: the log
/// records what was attempted through the channel, not merely what moved.
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
    use crate::plugins::automation::AutomationPoint;

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
    fn persist_effect_merge_ors_every_field() {
        let mut a = PersistEffect {
            midi: true,
            project: false,
            plugins: false,
            automation: false,
            modulation: false,
        };
        let b = PersistEffect {
            midi: false,
            project: true,
            plugins: true,
            automation: false,
            modulation: true,
        };
        a.merge(&b);
        assert_eq!(
            a,
            PersistEffect {
                midi: true,
                project: true,
                plugins: true,
                automation: false,
                modulation: true,
            }
        );
        // merging default changes nothing
        let before = a.clone();
        a.merge(&PersistEffect::default());
        assert_eq!(a, before);
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
                automation_clips: vec![], bindings: vec![],
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
            tx.apply(Op::TrackAdd { track: new_row, index: 1, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })?;
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
            tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })?;
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
            tx.apply(Op::TrackAdd { track: t2.clone(), index: 1, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] })?;
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

    #[test]
    fn mismatched_track_timeline_start_samples_fails_cleanly_not_panics() {
        // `PropPath` and `ObjectRef` are independent serde-derived enums, so
        // a corrupted/replayed journal or a bad agent-authored op can pair
        // `ObjectRef::Track` with the Clip-only `TimelineStartSamples` path
        // — a value that deserializes cleanly. `apply_raw` must fail this
        // transaction atomically, not panic (round-2 §4: transact closures
        // must not panic).
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before_gain = m.lock().store.tracks[0].gain_db;
        let before_rev = m.lock().rev;
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::TimelineStartSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(42u64),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().store.tracks[0].gain_db, before_gain, "store untouched");
        assert_eq!(m.lock().rev, before_rev, "failed tx must not consume a revision");
    }

    #[test]
    fn mismatched_clip_instrument_id_fails_cleanly_not_panics() {
        // The mirror case: `Set{Clip, InstrumentId}` — a Track-only path
        // paired with a Clip object. Already safe (falls to `apply_raw`'s
        // catch-all `_ => Err(...)` arm) — pinned here so both mismatched
        // directions have coverage, not just the one that used to panic.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let clip = test_clip("c-1", "t-1");
        m.lock().store.clips.push(clip.clone());
        let before = m.lock().store.clips.clone();
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Clip("c-1".into()),
                path: PropPath::InstrumentId,
                from: serde_json::Value::Null,
                to: serde_json::json!("plugin:x"),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().store.clips, before, "store untouched");
    }

    // ---- Plan E Task 5: MIDI + tempo op kinds ----

    use crate::ids::{ContentId, LaneId, NoteId};
    use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent};

    fn test_midi_clip(id: &str, track_id: &str) -> MidiClip {
        MidiClip {
            id: id.into(),
            track_id: track_id.into(),
            name: format!("MIDI {id}"),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            notes: vec![],
            next_note_id: 1,
            content_id: ContentId::mint(),
            lane_id: LaneId::default_for_track(track_id),
            content_length_ticks: None,
        }
    }

    fn note(tick: u32, key: u8, id: u32) -> MidiNote {
        MidiNote { tick, length_ticks: 480, key, velocity: 100, channel: 0, note_id: NoteId(id) }
    }

    #[test]
    fn tempo_set_is_atomic_including_the_bpm_mirror() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let c = Session::transact(&m, TxMeta::user("set tempo"), |tx| {
            tx.apply(Op::TempoSet {
                ppq: 960,
                events: vec![TempoEvent { tick: 0, bpm: 140.0 }],
                meter: vec![MeterEvent { tick: 0, num: 3, den: 4 }],
            })
        })
        .unwrap();
        {
            let g = m.lock();
            assert_eq!(g.midi.ppq, 960);
            assert_eq!(g.midi.tempo_events, vec![TempoEvent { tick: 0, bpm: 140.0 }]);
            assert_eq!(g.midi.meter_events, vec![MeterEvent { tick: 0, num: 3, den: 4 }]);
            // The cross-store atomicity this task exists for: the bpm
            // mirror lands in the SAME commit, not a second phase.
            assert_eq!(g.store.transport.tempo_bpm, 140.0);
        }
        assert!(c.effect.rebuild, "tempo change must request a rebuild");
        assert!(c.effect.persist.midi, "tempo change must persist midi");
        assert!(c.effect.persist.project, "tempo change must persist the project (bpm mirror)");

        // Inverse restores all four fields in one commit.
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        let g = m.lock();
        assert_eq!(g.midi.ppq, MidiStore::default().ppq);
        assert_eq!(g.midi.tempo_events, MidiStore::default().tempo_events);
        assert_eq!(g.midi.meter_events, MidiStore::default().meter_events);
        assert_eq!(g.store.transport.tempo_bpm, Store::default().transport.tempo_bpm);
    }

    #[test]
    fn tempo_set_rejects_empty_or_unsorted_events_atomically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let before = { let g = m.lock(); (g.midi.ppq, g.midi.tempo_events.clone(), g.midi.meter_events.clone()) };

        let r = Session::transact(&m, TxMeta::user("empty"), |tx| {
            tx.apply(Op::TempoSet { ppq: 960, events: vec![], meter: vec![MeterEvent { tick: 0, num: 4, den: 4 }] })
        });
        assert!(r.is_err(), "empty events must be rejected");

        let r = Session::transact(&m, TxMeta::user("unsorted"), |tx| {
            tx.apply(Op::TempoSet {
                ppq: 960,
                events: vec![TempoEvent { tick: 100, bpm: 140.0 }, TempoEvent { tick: 0, bpm: 120.0 }],
                meter: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            })
        });
        assert!(r.is_err(), "unsorted events must be rejected");

        let g = m.lock();
        assert_eq!((g.midi.ppq, g.midi.tempo_events.clone(), g.midi.meter_events.clone()), before, "store untouched");
    }

    #[test]
    fn midi_set_notes_inverse_restores_notes_but_never_rewinds_watermark() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let mut clip = test_midi_clip("mc-1", "t-1");
        clip.notes = vec![note(0, 60, 1), note(480, 62, 2)];
        clip.next_note_id = 3;
        m.lock().midi.clips.push(clip.clone());

        let c = Session::transact(&m, TxMeta::user("set notes"), |tx| {
            // Re-send the two existing notes with their live ids (kept) plus
            // a brand-new note carrying id 0 (minted -> 3, watermark -> 4).
            tx.apply(Op::MidiSetNotes {
                clip: "mc-1".into(),
                notes: vec![note(0, 60, 1), note(480, 62, 2), note(960, 64, 0)],
            })
        })
        .unwrap();
        {
            let g = m.lock();
            let c = g.midi.clips.iter().find(|c| c.id == "mc-1").unwrap();
            assert_eq!(c.notes.len(), 3);
            assert_eq!(c.notes[2].note_id.0, 3, "id 0 minted from the watermark");
            assert_eq!(c.next_note_id, 4);
        }
        assert!(c.effect.rebuild, "note replacement must request a rebuild");
        assert!(c.effect.persist.midi, "note replacement must persist midi");

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        let g = m.lock();
        let restored = g.midi.clips.iter().find(|c| c.id == "mc-1").unwrap();
        assert_eq!(restored.notes, clip.notes, "undo restores the original notes exactly");
        assert_eq!(restored.next_note_id, 4, "watermark is NEVER rewound by the inverse (ADR 0001)");
    }

    #[test]
    fn midi_clip_add_then_inverse_removes_byte_identically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let clip = test_midi_clip("mc-1", "t-1");
        let c = Session::transact(&m, TxMeta::user("add midi clip"), |tx| {
            tx.apply(Op::MidiClipAdd { clip: clip.clone(), index: 0 })
        })
        .unwrap();
        assert_eq!(m.lock().midi.clips, vec![clip.clone()]);
        assert!(c.effect.rebuild, "midi clip add must request a rebuild");
        assert!(c.effect.persist.midi, "midi clip add must persist midi");

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert!(m.lock().midi.clips.is_empty(), "inverse removes the row byte-identically (full round trip)");
    }

    #[test]
    fn midi_clip_add_duplicate_id_is_rejected_atomically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let clip = test_midi_clip("mc-1", "t-1");
        m.lock().midi.clips.push(clip.clone());
        let before = m.lock().midi.clips.clone();
        let r = Session::transact(&m, TxMeta::user("dup"), |tx| {
            tx.apply(Op::MidiClipAdd { clip: clip.clone(), index: 1 })
        });
        assert!(r.is_err());
        assert_eq!(m.lock().midi.clips, before, "store untouched");
    }

    #[test]
    fn midi_bounds_set_clamps_and_round_trips() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let mut clip = test_midi_clip("mc-1", "t-1");
        clip.length_ticks = 1920;
        m.lock().midi.clips.push(clip.clone());

        let c = Session::transact(&m, TxMeta::user("resize to zero"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-1".into()),
                path: PropPath::LengthTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(0u64),
            })
        })
        .unwrap();
        {
            let g = m.lock();
            let c = g.midi.clips.iter().find(|c| c.id == "mc-1").unwrap();
            assert_eq!(c.length_ticks, 1, "clamps to >= 1, does not error");
        }
        match &c.ops[0] {
            Op::Set { to, .. } => assert_eq!(to, &serde_json::json!(1u64), "recorded op carries the as-applied (clamped) value"),
            other => panic!("expected Set, got {other:?}"),
        }
        assert!(c.effect.rebuild, "bounds change must request a rebuild");
        assert!(c.effect.persist.midi, "bounds change must persist midi");

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        let g = m.lock();
        let restored = g.midi.clips.iter().find(|c| c.id == "mc-1").unwrap();
        assert_eq!(restored.length_ticks, 1920, "inverse restores the pre-clamp truth");
    }

    #[test]
    fn midi_bounds_content_length_ticks_round_trips_including_null() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let clip = test_midi_clip("mc-1", "t-1");
        m.lock().midi.clips.push(clip.clone());

        let c = Session::transact(&m, TxMeta::user("set content length"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-1".into()),
                path: PropPath::ContentLengthTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(1920u64),
            })
        })
        .unwrap();
        assert_eq!(
            m.lock().midi.clips.iter().find(|c| c.id == "mc-1").unwrap().content_length_ticks,
            Some(1920)
        );
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            m.lock().midi.clips.iter().find(|c| c.id == "mc-1").unwrap().content_length_ticks,
            None,
            "undo restores the absent (null) content length"
        );
    }

    #[test]
    fn mismatched_track_length_ticks_fails_cleanly_not_panics() {
        // `Set{Track, LengthTicks}` — a MidiClip-only path paired with a
        // Track object — must error, never panic.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before_gain = m.lock().store.tracks[0].gain_db;
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::LengthTicks,
                from: serde_json::Value::Null,
                to: serde_json::json!(42u64),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().store.tracks[0].gain_db, before_gain, "store untouched");
    }

    #[test]
    fn mismatched_midi_clip_gain_fails_cleanly_not_panics() {
        // The mirror case: `Set{MidiClip, Gain}` — a Track-only path paired
        // with a MidiClip object.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let clip = test_midi_clip("mc-1", "t-1");
        m.lock().midi.clips.push(clip.clone());
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::MidiClip("mc-1".into()),
                path: PropPath::Gain,
                from: serde_json::Value::Null,
                to: serde_json::json!(0.5),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().midi.clips, vec![clip], "store untouched");
    }

    #[test]
    fn unknown_midi_clip_fails_whole_batch_atomically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let r = Session::transact(&m, TxMeta::user("bad"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "no-such-clip".into(), notes: vec![] })
        });
        assert!(r.is_err());
    }

    // ---- Plan E Task 12: transport family --------------------------------

    #[test]
    fn transport_state_set_round_trips_through_inverse() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        assert_eq!(m.lock().store.transport.state, "stopped");
        let c = Session::transact(&m, TxMeta::user("transport play").transient(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::TransportState,
                from: serde_json::Value::Null,
                to: serde_json::json!("playing"),
            })
        })
        .unwrap();
        assert!(c.meta.transient, "transport commits are transient");
        assert_eq!(m.lock().store.transport.state, "playing");
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().store.transport.state, "stopped", "undo restores the original state");
    }

    #[test]
    fn transport_loop_set_round_trips_through_inverse() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let c = Session::transact(&m, TxMeta::user("transport set loop").transient(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::LoopEnabled,
                from: serde_json::Value::Null,
                to: serde_json::json!(true),
            })?;
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::LoopStartSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(1_000u64),
            })?;
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::LoopEndSamples,
                from: serde_json::Value::Null,
                to: serde_json::json!(2_000u64),
            })
        })
        .unwrap();
        assert!(c.meta.transient);
        {
            let s = m.lock();
            assert!(s.store.transport.loop_enabled);
            assert_eq!(s.store.transport.loop_start_samples, 1_000);
            assert_eq!(s.store.transport.loop_end_samples, 2_000);
        }
        // Sets on transport paths carry NO persist flag (RULING, Task 12
        // brief) — a divergence from the Clip/MidiClip Set arms above,
        // which do set `effect.persist.project`/`.midi`.
        assert_eq!(c.effect.persist, PersistEffect::default(), "transport Sets set no persist flag");
        assert!(!c.effect.rebuild, "transport Sets don't need a graph rebuild");
    }

    // ---- Plan E Task 10: automation lane op kind ----

    fn test_lane(id: &str, points: Vec<AutomationPoint>) -> AutomationLane {
        AutomationLane { id: id.into(), target_node: "track:t-1".into(), param_id: 0, points }
    }

    #[test]
    fn automation_set_lane_inserts_and_inverse_deletes() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let lane = test_lane("a-1", vec![AutomationPoint { tick: 0, value: 1.0 }]);
        let c = Session::transact(&m, TxMeta::user("set lane"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "a-1".into(), lane: Some(lane.clone()) })
        })
        .unwrap();
        assert_eq!(m.lock().automation.lanes, vec![lane]);
        assert!(c.effect.persist.automation, "lane write must persist automation");
        assert!(
            c.effect.rebuild,
            "Track D flipped the REBUILD PIN: `engine::rebuild` reads session.automation now"
        );
        match &c.inverses[0] {
            Op::AutomationSetLane { key, lane } => {
                assert_eq!(key, "a-1");
                assert!(lane.is_none(), "the lane didn't exist before — inverse deletes it");
            }
            other => panic!("expected AutomationSetLane, got {other:?}"),
        }

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        let s = m.lock();
        assert!(!s.store.transport.loop_enabled, "undo restores loop_enabled");
        assert_eq!(s.store.transport.loop_start_samples, 0, "undo restores loop_start_samples");
        assert_eq!(s.store.transport.loop_end_samples, 0, "undo restores loop_end_samples");
    }

    #[test]
    fn transport_stop_at_end_and_sample_rate_round_trip() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        assert!(m.lock().store.transport.stop_at_end); // default true
        Session::transact(&m, TxMeta::user("stop at end").transient(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::StopAtEnd,
                from: serde_json::Value::Null,
                to: serde_json::json!(false),
            })
        })
        .unwrap();
        assert!(!m.lock().store.transport.stop_at_end);

        Session::transact(&m, TxMeta::user("sample rate").transient(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::SampleRate,
                from: serde_json::Value::Null,
                to: serde_json::json!(96_000u32),
            })
        })
        .unwrap();
        assert_eq!(m.lock().store.transport.sample_rate, 96_000);
    }

    #[test]
    fn mismatched_transport_gain_fails_cleanly_not_panics() {
        // `Set{Transport, Gain}` — a Track-only path paired with the
        // Transport object.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let before = m.lock().store.transport.state.clone();
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::Gain,
                from: serde_json::Value::Null,
                to: serde_json::json!(0.5),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().store.transport.state, before, "store untouched");
    }

    #[test]
    fn mismatched_track_loop_enabled_fails_cleanly_not_panics() {
        // The mirror case: `Set{Track, LoopEnabled}` — a Transport-only
        // path paired with a Track object.
        let m = parking_lot::Mutex::new(session_with_one_track("t-1"));
        let before_gain = m.lock().store.tracks[0].gain_db;
        let r = Session::transact(&m, TxMeta::user("mismatched"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::LoopEnabled,
                from: serde_json::Value::Null,
                to: serde_json::json!(true),
            })
        });
        assert!(r.is_err(), "mismatched object/path must error, not panic");
        assert_eq!(m.lock().store.tracks[0].gain_db, before_gain, "store untouched");
    }

    #[test]
    fn transport_state_rejects_unknown_string() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let r = Session::transact(&m, TxMeta::user("bad state"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::TransportState,
                from: serde_json::Value::Null,
                to: serde_json::json!("paused"), // not one of playing/stopped/recording
            })
        });
        assert!(r.is_err(), "an unrecognized transport state must error, not silently write garbage");
        assert!(m.lock().automation.lanes.is_empty(), "undo removes the inserted lane");
    }

    #[test]
    fn automation_set_lane_overwrites_and_inverse_restores_previous() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let original = test_lane("a-1", vec![AutomationPoint { tick: 0, value: 1.0 }]);
        m.lock().automation.lanes.push(original.clone());

        let replacement = test_lane("a-1", vec![AutomationPoint { tick: 960, value: 0.5 }]);
        let c = Session::transact(&m, TxMeta::user("overwrite lane"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "a-1".into(), lane: Some(replacement.clone()) })
        })
        .unwrap();
        assert_eq!(m.lock().automation.lanes, vec![replacement]);
        assert!(c.effect.persist.automation);
        match &c.inverses[0] {
            Op::AutomationSetLane { key, lane: Some(l) } => {
                assert_eq!(key, "a-1");
                assert_eq!(l.points, original.points, "inverse carries the PREVIOUS lane, from store truth");
            }
            other => panic!("expected AutomationSetLane{{..Some}}, got {other:?}"),
        }

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().automation.lanes, vec![original], "undo restores the original lane exactly");
    }

    #[test]
    fn automation_set_lane_deletes_and_inverse_reinserts() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let lane = test_lane("a-1", vec![AutomationPoint { tick: 0, value: 1.0 }]);
        m.lock().automation.lanes.push(lane.clone());

        let c = Session::transact(&m, TxMeta::user("delete lane"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "a-1".into(), lane: None })
        })
        .unwrap();
        assert!(m.lock().automation.lanes.is_empty(), "lane removed");
        assert!(c.effect.persist.automation);

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() { tx.apply(op)?; }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().automation.lanes, vec![lane], "undo reinserts the deleted lane");
    }

    #[test]
    fn automation_set_lane_deleting_an_unknown_key_is_a_harmless_noop() {
        // Mirrors the retired command's leniency (`s.lanes.retain(...)`
        // silently dropped nothing for an unknown id) — deleting a lane
        // that was never there is not an error.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let c = Session::transact(&m, TxMeta::user("delete ghost"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "ghost".into(), lane: None })
        })
        .unwrap();
        assert!(m.lock().automation.lanes.is_empty());
        // Track D: the arm schedules a rebuild unconditionally, so even this
        // no-op costs one. Cheaper than proving a delete changed nothing,
        // and a rebuild of an unchanged session is inaudible — but it IS the
        // behaviour, so it is asserted rather than left to be discovered.
        assert!(c.effect.rebuild, "an absent-key delete still rebuilds");
        match &c.inverses[0] {
            Op::AutomationSetLane { key, lane } => {
                assert_eq!(key, "ghost");
                assert!(lane.is_none());
            }
            other => panic!("expected AutomationSetLane, got {other:?}"),
        }
    }

    #[test]
    fn automation_set_lane_normalizes_and_rejects_invalid_points_atomically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let bad = test_lane("a-1", vec![AutomationPoint { tick: 0, value: f32::NAN }]);
        let r = Session::transact(&m, TxMeta::user("bad lane"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "a-1".into(), lane: Some(bad) })
        });
        assert!(r.is_err(), "non-finite values must be rejected");
        assert!(m.lock().automation.lanes.is_empty(), "rejected write leaves the session untouched");

        // Unsorted points get normalized (sorted), not rejected.
        let unsorted = test_lane(
            "a-1",
            vec![AutomationPoint { tick: 960, value: 0.5 }, AutomationPoint { tick: 0, value: 1.0 }],
        );
        Session::transact(&m, TxMeta::user("unsorted lane"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "a-1".into(), lane: Some(unsorted) })
        })
        .unwrap();
        assert_eq!(
            m.lock().automation.lanes[0].points,
            vec![AutomationPoint { tick: 0, value: 1.0 }, AutomationPoint { tick: 960, value: 0.5 }],
            "apply normalizes (sorts) points before storing"
        );
    }

    /// THE REBUILD PIN (Track D). Until the engine read `session.automation`,
    /// this arm deliberately set no engine effect — the arm's own comment
    /// named the day this would have to change. That day is Task 9's.
    #[test]
    fn automation_set_lane_schedules_a_rebuild() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let lane = test_lane("lane-a", vec![AutomationPoint { tick: 0, value: 1.0 }]);
        let c = Session::transact(&m, TxMeta::user("edit automation"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "lane-a".into(), lane: Some(lane) })
        })
        .unwrap();
        assert!(c.effect.rebuild, "the engine reads session.automation now — lanes must rebuild");
        assert!(c.effect.persist.automation, "and still persist");
    }

    /// Deleting a lane rebuilds too — the ramp must come OFF the graph, and
    /// it only can by rebuilding a graph without it.
    #[test]
    fn automation_lane_delete_schedules_a_rebuild() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        m.lock()
            .automation
            .lanes
            .push(test_lane("lane-a", vec![AutomationPoint { tick: 0, value: 1.0 }]));
        let c = Session::transact(&m, TxMeta::user("edit automation"), |tx| {
            tx.apply(Op::AutomationSetLane { key: "lane-a".into(), lane: None })
        })
        .unwrap();
        assert!(m.lock().automation.lanes.is_empty());
        assert!(c.effect.rebuild);
    }

    // ---- Track F Task 5: modulation document + ops + rebuild pin ----

    fn test_curve(id: &str, points: Vec<AutomationPoint>) -> crate::modulation::Curve {
        crate::modulation::Curve {
            id: id.into(),
            name: String::new(),
            length_ticks: None,
            points,
        }
    }

    fn test_binding(id: &str, curve_id: &str) -> crate::modulation::Binding {
        crate::modulation::Binding {
            id: id.into(),
            source: crate::modulation::Source::Curve { curve_id: curve_id.into() },
            target: crate::modulation::TargetRef::TrackParam {
                track_id: "t1".into(),
                param: crate::modulation::TrackParam::Gain,
            },
            mode: crate::modulation::BindingMode::Multiply,
            depth: 1.0,
            range: crate::modulation::Range::default(),
            domain: crate::modulation::Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        }
    }

    fn test_auto_clip(id: &str) -> crate::modulation::AutomationClip {
        crate::modulation::AutomationClip {
            id: id.into(),
            track_id: "t-auto".into(),
            curve_id: "cur-1".into(),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            content_length_ticks: None,
        }
    }

    #[test]
    fn removing_an_automation_track_drops_its_clips_and_sourced_bindings() {
        let m = parking_lot::Mutex::new(session_with_one_track("t-auto"));
        {
            let mut g = m.lock();
            g.store.tracks[0].kind = "automation".into();
            g.modulation.curves.push(test_curve(
                "cur-1",
                vec![AutomationPoint { tick: 0, value: 1.0 }],
            ));
            g.modulation.automation_clips.push(test_auto_clip("acl-1"));
            let mut sourced = test_binding("b-auto", "cur-1");
            sourced.source = crate::modulation::Source::AutomationTrack {
                track_id: "t-auto".into(),
            };
            g.modulation.bindings.push(sourced);
            g.modulation.bindings.push(test_binding("b-keep", "cur-1"));
        }
        let row = m.lock().store.tracks[0].clone();
        let c = Session::transact(&m, TxMeta::user("remove"), |tx| {
            tx.apply(Op::TrackRemove {
                track: row,
                index: 0,
                clips: vec![],
                clip_indices: vec![],
            })
        })
        .unwrap();
        {
            let g = m.lock();
            assert!(
                g.modulation.automation_clips.is_empty(),
                "automation clips on the removed track must leave with it"
            );
            assert_eq!(
                g.modulation.bindings.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
                vec!["b-keep"],
                "only bindings sourced from the removed track are dropped"
            );
        }
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        {
            let g = m.lock();
            assert_eq!(g.modulation.automation_clips.len(), 1);
            assert_eq!(g.modulation.automation_clips[0].id, "acl-1");
            assert_eq!(g.modulation.bindings.len(), 2);
        }
    }

    #[test]
    fn set_curve_upserts_and_its_inverse_restores_store_truth() {
        // Mirrors automation_set_lane_inserts_and_inverse_deletes /
        // automation_set_lane_overwrites_and_inverse_restores_previous.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let curve = test_curve("cur-1", vec![AutomationPoint { tick: 0, value: 1.0 }]);
        let c = Session::transact(&m, TxMeta::user("set curve"), |tx| {
            tx.apply(Op::ModulationSetCurve { key: "cur-1".into(), curve: Some(curve.clone()) })
        })
        .unwrap();
        assert_eq!(m.lock().modulation.curves, vec![curve.clone()]);
        assert!(c.effect.persist.modulation, "curve write must persist modulation");
        assert!(c.effect.rebuild, "REBUILD PIN: modulation edits must rebuild");
        match &c.inverses[0] {
            Op::ModulationSetCurve { key, curve } => {
                assert_eq!(key, "cur-1");
                assert!(curve.is_none(), "curve didn't exist before — inverse deletes it");
            }
            other => panic!("expected ModulationSetCurve, got {other:?}"),
        }

        let replacement = test_curve("cur-1", vec![AutomationPoint { tick: 960, value: 0.5 }]);
        let c2 = Session::transact(&m, TxMeta::user("overwrite curve"), |tx| {
            tx.apply(Op::ModulationSetCurve {
                key: "cur-1".into(),
                curve: Some(replacement.clone()),
            })
        })
        .unwrap();
        assert_eq!(m.lock().modulation.curves, vec![replacement]);
        match &c2.inverses[0] {
            Op::ModulationSetCurve { key, curve: Some(prev) } => {
                assert_eq!(key, "cur-1");
                assert_eq!(prev.points, curve.points, "inverse carries PREVIOUS curve, store truth");
            }
            other => panic!("expected ModulationSetCurve{{..Some}}, got {other:?}"),
        }

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c2.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            m.lock().modulation.curves,
            vec![curve],
            "undo restores the original curve exactly"
        );
    }

    #[test]
    fn set_binding_rejects_an_invalid_binding_without_mutating_the_session() {
        // Atomicity — round-2 §4: a failed apply leaves the session untouched.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let mut bad = test_binding("bnd-1", "cur-1");
        bad.depth = 4.0; // outside [-1, 1]
        let r = Session::transact(&m, TxMeta::user("bad binding"), |tx| {
            tx.apply(Op::ModulationSetBinding { key: "bnd-1".into(), binding: Some(bad) })
        });
        assert!(r.is_err(), "out-of-range depth must be rejected");
        assert!(
            m.lock().modulation.bindings.is_empty(),
            "rejected write leaves the session untouched"
        );

        // A second invalid shape: empty curveId.
        let mut empty_src = test_binding("bnd-1", "");
        empty_src.source = crate::modulation::Source::Curve { curve_id: String::new() };
        let r = Session::transact(&m, TxMeta::user("empty curve"), |tx| {
            tx.apply(Op::ModulationSetBinding {
                key: "bnd-1".into(),
                binding: Some(empty_src),
            })
        });
        assert!(r.is_err(), "empty curveId must be rejected");
        assert!(m.lock().modulation.bindings.is_empty());
    }

    #[test]
    fn every_modulation_op_sets_rebuild_on_its_effect() {
        // Track D REBUILD PIN lesson: a document change nobody rebuilds on
        // is inaudible. Every modulation mutation schedules a rebuild.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));

        let c = Session::transact(&m, TxMeta::user("curve"), |tx| {
            tx.apply(Op::ModulationSetCurve {
                key: "cur-1".into(),
                curve: Some(test_curve("cur-1", vec![AutomationPoint { tick: 0, value: 1.0 }])),
            })
        })
        .unwrap();
        assert!(c.effect.rebuild, "ModulationSetCurve must rebuild");
        assert!(c.effect.persist.modulation);

        let c = Session::transact(&m, TxMeta::user("binding"), |tx| {
            tx.apply(Op::ModulationSetBinding {
                key: "bnd-1".into(),
                binding: Some(test_binding("bnd-1", "cur-1")),
            })
        })
        .unwrap();
        assert!(c.effect.rebuild, "ModulationSetBinding must rebuild");
        assert!(c.effect.persist.modulation);

        let c = Session::transact(&m, TxMeta::user("clip"), |tx| {
            tx.apply(Op::AutomationClipSet {
                key: "acl-1".into(),
                clip: Some(test_auto_clip("acl-1")),
            })
        })
        .unwrap();
        assert!(c.effect.rebuild, "AutomationClipSet must rebuild");
        assert!(c.effect.persist.modulation);

        // Deletes rebuild too — the compiled ramps must come OFF the graph.
        let c = Session::transact(&m, TxMeta::user("del curve"), |tx| {
            tx.apply(Op::ModulationSetCurve { key: "cur-1".into(), curve: None })
        })
        .unwrap();
        assert!(c.effect.rebuild, "deleting a curve must rebuild");
    }

    #[test]
    fn deleting_a_curve_that_bindings_still_reference_leaves_those_bindings_inert_not_dangling() {
        // A binding whose curve is gone contributes nothing and is
        // reported, never panics — compile skips missing curves.
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        Session::transact(&m, TxMeta::user("seed"), |tx| {
            tx.apply(Op::ModulationSetCurve {
                key: "cur-1".into(),
                curve: Some(test_curve("cur-1", vec![AutomationPoint { tick: 0, value: 1.0 }])),
            })?;
            tx.apply(Op::ModulationSetBinding {
                key: "bnd-1".into(),
                binding: Some(test_binding("bnd-1", "cur-1")),
            })?;
            Ok(())
        })
        .unwrap();

        Session::transact(&m, TxMeta::user("delete curve"), |tx| {
            tx.apply(Op::ModulationSetCurve { key: "cur-1".into(), curve: None })
        })
        .unwrap();

        let s = m.lock();
        assert!(s.modulation.curves.is_empty(), "curve removed");
        assert_eq!(s.modulation.bindings.len(), 1, "binding stays — not cascade-deleted");
        assert_eq!(s.modulation.bindings[0].id, "bnd-1");

        // Compile must not panic; missing curve → inert (no gain ramp).
        let map = crate::midi::TempoMap::new(
            crate::midi::types::DEFAULT_PPQ,
            vec![crate::midi::types::TempoEvent { tick: 0, bpm: 120.0 }],
            48_000,
        )
        .unwrap();
        let slot_of = |t: &str| (t == "t1").then_some(0usize);
        let track_pan = |_: &str| None::<f32>;
        let param_range = |_: &str, _: u32| None::<(f32, f32)>;
        let param_value = |_: &str, _: u32| None::<f32>;
        let instrument_of = |_: &str| None::<String>;
        let content_placements = |_: &str| Vec::new();
        let ctx = crate::modulation::CompileCtx {
            n_slots: 1,
            slot_of: &slot_of,
            track_pan: &track_pan,
            param_range: &param_range,
            param_value: &param_value,
            instrument_of: &instrument_of,
            content_placements: &content_placements,
        };
        let out = crate::modulation::compile(&s.modulation, &map, &ctx);
        assert!(
            out.tracks[0].gain.is_none(),
            "a binding whose curve is gone contributes nothing"
        );
    }

    // ---- Plan E Task 9: plugin op kinds ----

    fn test_plugin_row(id: &str) -> PluginInstanceInfo {
        PluginInstanceInfo {
            id: id.into(),
            uid: "lv2:urn:test:synth".into(),
            name: "TestSynth".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        }
    }

    /// Mirrors `session_owns_both_stores_under_one_lock`: the plugin
    /// document is a THIRD field behind the SAME session guard, so a
    /// caller can read/write tracks, midi and plugin rows under one lock —
    /// impossible with the old (store, midi) + separate `PluginRegistry`
    /// layout.
    #[test]
    fn session_owns_plugin_rows() {
        let s = Session::new(Store::default(), MidiStore::default());
        let m = parking_lot::Mutex::new(s);
        let mut g = m.lock();
        g.plugins.instances.push(test_plugin_row("p-1"));
        g.plugins.params.insert("p-1".into(), Vec::new());
        g.midi.ppq = 960;
        assert_eq!(g.plugins.instances.len(), 1);
        assert_eq!(g.midi.ppq, 960);
    }

    #[test]
    fn plugin_add_duplicate_id_is_rejected_atomically() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        m.lock().plugins.instances.push(row.clone());
        let before = m.lock().plugins.instances.clone();
        let r = Session::transact(&m, TxMeta::user("dup"), |tx| {
            tx.apply(Op::PluginAdd { row: row.clone(), index: 1 })
        });
        assert!(r.is_err());
        assert_eq!(m.lock().plugins.instances, before, "document untouched");
    }

    /// §4.4 restore-from-blob: `PluginRemove`'s `state` (captured via the
    /// host's `save_state` BEFORE teardown, outside the lock — this test
    /// hands it in directly, no real host needed) travels into
    /// `pending_state`; the computed inverse is `PluginAdd`, and REPLAYING
    /// that inverse (undo) re-emits `Instantiate` + `LoadState` — the
    /// host-forward round trip a generic undo replay needs since there is
    /// no "prepare" step for it (unlike the original forward instantiate).
    #[test]
    fn plugin_remove_then_undo_restores_state_blob() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        {
            let mut g = m.lock();
            g.plugins.instances.push(row.clone());
            g.plugins.params.insert(row.id.clone(), Vec::new());
        }
        let blob = vec![1u8, 2, 3, 4];
        let c = Session::transact(&m, TxMeta::user("remove"), |tx| {
            tx.apply(Op::PluginRemove {
                row: row.clone(),
                index: 0,
                state: Some(blob.clone()),
                params: Vec::new(),
            })
        })
        .unwrap();
        assert!(m.lock().plugins.instances.is_empty(), "row removed");
        assert_eq!(
            m.lock().plugins.pending_state.get("p-1"),
            Some(&blob),
            "blob parked in pending_state"
        );
        assert!(
            c.effect.host_forward.contains(&HostForward::Destroy { instance: "p-1".into() }),
            "forward remove destroys the host instance post-lock"
        );
        assert!(c.effect.persist.plugins, "remove persists via effect");
        match &c.inverses[0] {
            Op::PluginAdd { row: r, .. } => assert_eq!(r.id, "p-1", "inverse carries the row"),
            other => panic!("expected PluginAdd inverse, got {other:?}"),
        }

        let c2 = Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().plugins.instances.len(), 1, "undo restores the row");
        assert!(
            c2.effect.host_forward.contains(&HostForward::Instantiate { instance: "p-1".into() }),
            "undo re-emits Instantiate"
        );
        assert!(
            c2.effect.host_forward.contains(&HostForward::LoadState { instance: "p-1".into() }),
            "undo re-emits LoadState (a blob is pending)"
        );
    }

    /// Task 9 review round 1, Important-1: undo-of-remove must restore the
    /// user's ACTUAL param mirror, not an empty seed later overwritten by
    /// the host's post-reinstantiate DEFAULTS. `apply_raw`'s `PluginRemove`
    /// arm parks the mirror instead of clearing it; `PluginAdd`'s
    /// `.or_default()` seed must find it already there and leave it alone.
    #[test]
    fn plugin_remove_then_undo_restores_the_param_mirror_not_empty() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        let tweaked = ParamInfo {
            id: 7,
            name: "cutoff".into(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            value: 0.9,
            steps: 0,
        };
        {
            let mut g = m.lock();
            g.plugins.instances.push(row.clone());
            g.plugins.params.insert(row.id.clone(), vec![tweaked.clone()]);
        }
        let c = Session::transact(&m, TxMeta::user("remove"), |tx| {
            tx.apply(Op::PluginRemove { row: row.clone(), index: 0, state: None, params: vec![] })
        })
        .unwrap();
        // Document truth: the mirror is PARKED (still readable), not wiped,
        // even though the row itself is gone.
        assert_eq!(
            m.lock().plugins.params.get("p-1"),
            Some(&vec![tweaked.clone()]),
            "param mirror survives removal, parked for a possible undo"
        );

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            m.lock().plugins.params.get("p-1"),
            Some(&vec![tweaked]),
            "undo (PluginAdd replaying the parked id) leaves the real mirror untouched — \
             not reset to empty"
        );
    }

    /// L-1 CLOSED (whole-branch review): the COLD-REPLAY half of the test
    /// above. A fresh process replaying a journal has nothing parked in
    /// `session.plugins.params`, so `Op::PluginRemove.params` — captured
    /// since Task 9 and read by nothing — is now what seeds the mirror when
    /// the in-memory one is absent. Without it, an undo-of-remove during a
    /// replay restored the row with no params at all.
    #[test]
    fn plugin_remove_seeds_the_param_mirror_from_the_op_on_a_cold_replay() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        let journaled = ParamInfo {
            id: 7,
            name: "cutoff".into(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            value: 0.9,
            steps: 0,
        };
        // Cold: the row exists (an earlier replayed `PluginAdd` put it
        // there) but NOTHING is parked in the params map.
        m.lock().plugins.instances.push(row.clone());
        assert!(m.lock().plugins.params.get("p-1").is_none());

        let c = Session::transact(&m, TxMeta::user("replay remove"), |tx| {
            tx.apply(Op::PluginRemove {
                row: row.clone(),
                index: 0,
                state: None,
                params: vec![journaled.clone()],
            })
        })
        .unwrap();
        assert_eq!(
            m.lock().plugins.params.get("p-1"),
            Some(&vec![journaled.clone()]),
            "the op's own params seed the mirror when there is nothing parked"
        );

        // ...so replaying the inverse restores a row WITH its params.
        Session::transact(&m, TxMeta::user("replay undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().plugins.params.get("p-1"), Some(&vec![journaled]));

        // And the seed NEVER clobbers a live mirror: with real values
        // parked, the op's copy is ignored (in-process behaviour unchanged).
        let live = ParamInfo {
            id: 7,
            name: "cutoff".into(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            value: 0.1,
            steps: 0,
        };
        m.lock().plugins.params.insert(row.id.clone(), vec![live.clone()]);
        Session::transact(&m, TxMeta::user("remove again"), |tx| {
            tx.apply(Op::PluginRemove {
                row: row.clone(),
                index: 0,
                state: None,
                params: vec![ParamInfo { value: 0.9, ..live.clone() }],
            })
        })
        .unwrap();
        assert_eq!(
            m.lock().plugins.params.get("p-1"),
            Some(&vec![live]),
            "a populated mirror wins over the op's recorded copy"
        );
    }

    /// A knob drag: several `Set{Plugin, Param}` writes on the SAME
    /// (instance, index) within one transaction fold to their net change
    /// (mirrors mix Sets), and the batch persists via `effect.persist.
    /// plugins` + forwards the FINAL clamped value to the host
    /// (`host_forward`) — never disk I/O or a host call inside `apply_raw`
    /// itself.
    #[test]
    fn plugin_set_param_is_coalescable_and_persists_via_effect() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        {
            let mut g = m.lock();
            g.plugins.instances.push(row.clone());
            g.plugins.params.insert(
                row.id.clone(),
                vec![ParamInfo {
                    id: 7,
                    name: "cutoff".into(),
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    value: 0.5,
                    steps: 0,
                }],
            );
        }
        let set_param = |v: f64| Op::Set {
            object: ObjectRef::Plugin("p-1".into()),
            path: PropPath::Param { index: 7 },
            from: serde_json::Value::Null,
            to: serde_json::json!(v),
        };
        let c = Session::transact(&m, TxMeta::user("knob drag"), |tx| {
            tx.apply(set_param(0.2))?;
            tx.apply(set_param(0.9))
        })
        .unwrap();
        assert_eq!(c.ops.len(), 1, "same (instance, index) coalesces to one net Set");
        match &c.ops[0] {
            Op::Set { to, .. } => assert_eq!(to, &serde_json::json!(0.9)),
            other => panic!("expected Set, got {other:?}"),
        }
        assert_eq!(m.lock().plugins.params.get("p-1").unwrap()[0].value, 0.9);
        assert!(c.effect.persist.plugins, "param write persists via effect");
        assert!(!c.effect.rebuild, "a param write is not structural");
        let last = c.effect.host_forward.last().expect("at least one host forward");
        assert!(
            matches!(last, HostForward::ParamWrite { instance, index, value }
                if instance == "p-1" && *index == 7 && (*value - 0.9).abs() < 1e-6),
            "final host forward carries the net clamped value: {last:?}"
        );

        // Undo restores the original value.
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().plugins.params.get("p-1").unwrap()[0].value, 0.5);
    }

    #[test]
    fn plugin_set_param_unknown_instance_fails_cleanly() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let r = Session::transact(&m, TxMeta::user("bad"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Plugin("nope".into()),
                path: PropPath::Param { index: 0 },
                from: serde_json::Value::Null,
                to: serde_json::json!(0.5),
            })
        });
        assert!(r.is_err());
    }

    #[test]
    fn plugin_set_state_inverse_carries_the_previous_blob() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        let row = test_plugin_row("p-1");
        {
            let mut g = m.lock();
            g.plugins.instances.push(row.clone());
            g.plugins.pending_state.insert("p-1".into(), vec![0u8, 0, 0]);
        }
        let new_blob = vec![9u8, 9, 9, 9];
        let c = Session::transact(&m, TxMeta::user("zyn load patch"), |tx| {
            tx.apply(Op::PluginSetState { instance: "p-1".into(), state: new_blob.clone() })
        })
        .unwrap();
        assert_eq!(m.lock().plugins.pending_state.get("p-1"), Some(&new_blob));
        assert!(c.effect.persist.plugins);
        assert!(c
            .effect
            .host_forward
            .contains(&HostForward::LoadState { instance: "p-1".into() }));
        match &c.inverses[0] {
            Op::PluginSetState { state, .. } => assert_eq!(state, &vec![0u8, 0, 0], "old blob"),
            other => panic!("expected PluginSetState inverse, got {other:?}"),
        }
        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().plugins.pending_state.get("p-1"), Some(&vec![0u8, 0, 0]));
    }

    /// A state load changes what the instance SOUNDS like, and the live RT
    /// node only picks a blob up when it is (re)instantiated. So the arm both
    /// bumps the instance's state revision — which `midi::playback`'s node key
    /// carries, retiring the cached node — and asks for a rebuild to build the
    /// replacement. Undo restores an older blob and needs the same treatment.
    #[test]
    fn plugin_set_state_bumps_the_state_revision_and_rebuilds() {
        let m = parking_lot::Mutex::new(Session::new(Store::default(), MidiStore::default()));
        {
            let mut g = m.lock();
            g.plugins.instances.push(test_plugin_row("p-1"));
            g.plugins.pending_state.insert("p-1".into(), vec![0u8, 0, 0]);
        }
        assert_eq!(m.lock().plugins.state_rev.get("p-1"), None, "no load yet");

        let c = Session::transact(&m, TxMeta::user("zyn load patch"), |tx| {
            tx.apply(Op::PluginSetState { instance: "p-1".into(), state: vec![9u8, 9, 9, 9] })
        })
        .unwrap();
        assert_eq!(m.lock().plugins.state_rev.get("p-1"), Some(&1));
        assert!(c.effect.rebuild, "the live node must be replaced for the patch to be heard");

        Session::transact(&m, TxMeta::user("undo"), |tx| {
            for op in c.inverses.clone() {
                tx.apply(op)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(m.lock().plugins.state_rev.get("p-1"), Some(&2), "undo needs a fresh node too");
    }
}
