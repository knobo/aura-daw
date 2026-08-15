//! Plan F Task 7: the VERSION GRAPH — the retention substrate ADR 0005
//! specifies, fed from the one sink `HistoryLog::record_commit` already is.
//!
//! One node per non-transient batch, in rev order within the current epoch
//! (ruling F-11). A node is either MATERIALIZED (it holds the published
//! image the batch produced — O(1) to read back) or REPLAY-ONLY (it holds
//! the batch's ops and inverses, and is reconstructed by replaying forward
//! from the nearest materialized ancestor).
//!
//! WHAT DEFENDS WHAT — stated here so nobody re-derives replay-only as a
//! budget defense (round-2 §6): replay-only bounds an individual node's
//! charge and saves the capture/iteration burst of a bulk batch; EVICTION
//! defends the budget. They are different mechanisms with different jobs.
//!
//! WHAT THE GRAPH IS NOT: it is not the undo stack. `History` (200 entries,
//! ops + inverses) is what Ctrl+Z walks, and it needs no image at all — undo
//! replays inverses through an ordinary commit. So **evicting a version node
//! can never make an undo impossible**; it can only make a REV unreadable
//! for browsing/materialization (Tasks 9–11). [`VER_STEPS_FLOOR`] is what
//! ties the two together: the graph never evicts below as many steps as undo
//! can reach, so every rev the UI still offers is materializable.
//!
//! WHAT AN EVICTION ACTUALLY FREES — and `retained_bytes` IS A LOWER BOUND,
//! NOT A CEILING. Images are Arc-structurally shared with each other and
//! with `Session::published`, so dropping a node frees only the allocations
//! no OTHER holder still points at. Own-created bytes is the right
//! accounting UNIT (charging each node the whole document would count the
//! same allocation once per version), but it is not a measure of what an
//! eviction releases, because structural sharing runs FORWARD as well as
//! back: an image reuses the previous image's untouched content Arcs, which
//! is the entire point of the COW substrate. So a node's own-created content
//! outlives its eviction whenever a SURVIVING node's image still reuses it.
//!
//! MEASURED (fix round 1, I-1): v1 rewrites a clip to 1 000 notes (charge
//! 16 244 B), v2 touches only a track — so v2's capture REUSES v1's clip
//! content Arc — and v3 releases it from the live document. Evicting v1
//! leaves the graph reporting `retained_bytes: 427` while still exclusively
//! holding ~16 KB through v2. `evicting_a_node_frees_only_what_no_survivor_
//! reuses` pins exactly that.
//!
//! The overhang is BOUNDED BY ONE DOCUMENT: everything a survivor reuses
//! from before the cut is, by construction, a subset of the document as of
//! the cut. So the graph can exceed its budget by at most one document's
//! worth — a 50–100 % overshoot on a large project at a 512 MiB budget,
//! which is a real number and belongs to whoever fixes the accounting
//! (charging the LAST holder rather than the creator, or a reachability
//! sweep). It is not a leak and it is not unbounded. Note also that the
//! FRONT node pins a complete document while being charged only its delta.
//!
//! An eviction racing Task 6's lock-free rebuild is safe by the same
//! sharing mechanism: the rebuild holds its own `Arc` to the image, so the
//! drop merely decrements and the memory is released when that reader
//! finishes.

use std::sync::Arc;

use super::op::Op;
use super::session::Committed;
use super::snapshot::{ChangeSet, SessionSnapshot};

/// Classification threshold (`benches/bulkbench/RESULTS.md` §6, MEASURED):
/// a batch whose own-created bytes exceed this stores op + inverse instead
/// of a materialized image. At 256 KB the rule was a measured no-op (0.7 %
/// saving) because whole-clip ops sit at ~104–131 KB; at 64 KB the whole-
/// clip class falls on the replay-only side, saving 21 % on the AGENT
/// profile, with a worst measured replay chain of 23 whole-clip transforms
/// — well under a millisecond to walk.
pub const REPLAY_ONLY_THRESHOLD: usize = 64 * 1024;

/// Bytes ceiling the janitor defends (round-2 §6's measured budget:
/// ~54 700 HUMAN / ~6 550 AGENT steps retained inside it).
pub const VER_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// Never evict below this many retained steps (ruling F-2: aligned with
/// `history::UNDO_STACK_LIMIT` so browsing never covers less than undo
/// reaches). A HARD floor — the budget yields to it, not the other way
/// round, because a step the UI still offers must stay materializable.
pub const VER_STEPS_FLOOR: usize = 200;

/// One retained version.
pub enum VersionNode {
    Materialized { snapshot: Arc<SessionSnapshot>, charge: usize },
    ReplayOnly { ops: Vec<Op>, inverses: Vec<Op>, charge: usize },
}

impl VersionNode {
    /// Bytes this node ADDED — its own-created image bytes, or (replay-only)
    /// the op and inverse vectors it stores. Charging a replay-only node the
    /// image charge it avoided would defend the budget against a fiction.
    ///
    /// NOT the bytes dropping it frees: a surviving node's image may still
    /// reuse this node's own-created content Arcs, in which case the drop
    /// frees none of it. See the module doc's lower-bound argument.
    pub fn charge(&self) -> usize {
        match self {
            VersionNode::Materialized { charge, .. } | VersionNode::ReplayOnly { charge, .. } => {
                *charge
            }
        }
    }

    pub fn is_materialized(&self) -> bool {
        matches!(self, VersionNode::Materialized { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VersionStats {
    pub nodes: usize,
    pub materialized: usize,
    pub replay_only: usize,
    pub retained_bytes: usize,
    pub oldest_rev: Option<u64>,
    pub newest_rev: Option<u64>,
}

/// Everything needed to reconstruct a rev, CLONED OUT of the graph so the
/// replay itself runs with no mutex held (`materialize` is CPU-bound).
pub struct ReplayPlan {
    rev: u64,
    base: Arc<SessionSnapshot>,
    /// Each subsequent node's forward ops, in rev order.
    steps: Vec<Vec<Op>>,
}

impl ReplayPlan {
    /// Rebuild the document at [`Self::rev`]. Scratch session, no locks, no
    /// journal, no effects — `Session::apply_replay` is a bare `apply_raw`
    /// loop, and the effect descriptions it produces are discarded (the ops
    /// already ran, once, when they were committed).
    ///
    /// THE RESULT'S `transport` IS THE REPLAY BASE'S, NOT THE TARGET REV'S,
    /// and a caller MUST NOT restore it (fix round 1, I-2). Transport writes
    /// (play/stop/loop/stop-at-end/sample-rate, recording start and stop)
    /// commit TRANSIENTLY: they bump `rev` but leave no version node, so the
    /// chain has gaps and no `apply_replay` step ever carries their ops. A
    /// replay-only rev therefore reproduces CONTENT faithfully and transport
    /// only as of the nearest materialized ancestor. Restoring it through
    /// `Session::restore_from_snapshot` (which does write `store.transport`)
    /// would silently rewind loop points, stop-at-end and recording state.
    /// Fixing it properly means either recording transient batches as
    /// something, or replaying the journal rather than the graph — a
    /// question for the round that owns transport-in-history, not a
    /// paper-over here.
    pub fn run(self) -> Option<SessionSnapshot> {
        let mut scratch = super::session::Session::from_snapshot(&self.base);
        for ops in &self.steps {
            if let Err(e) = scratch.apply_replay(ops) {
                log::warn!("vergraph: cannot materialize rev {} — replay failed: {e}", self.rev);
                return None;
            }
        }
        let mut snap = SessionSnapshot::capture(&scratch, &self.base, &ChangeSet::all());
        // `apply_raw` does not bump `rev` (only `transact` does, and this is
        // deliberately transact-free), so the capture carries the BASE's rev.
        // The caller asked for a specific revision; say so.
        snap.rev = self.rev;
        Some(snap)
    }
}

/// The rev-ordered chain of retained versions for ONE epoch.
///
/// INVARIANT (the retention root): `nodes` is empty, or `nodes[0]` is
/// [`VersionNode::Materialized`]. Every retained rev therefore has a
/// materialized ancestor at or below it and is materializable. Both the
/// classifier (a node inserted at the front is always materialized) and the
/// evictor (the cut always lands ON a materialized node) uphold it.
pub struct VersionGraph {
    nodes: Vec<(u64, VersionNode)>,
    retained_bytes: usize,
    /// The document epoch this chain describes (ruling F-11). A batch from
    /// another epoch describes a document that is no longer open and is
    /// recorded nowhere — the same guard `record_commit` applies to the
    /// journal and the undo stack, repeated here so the sink's three
    /// streams cannot drift apart.
    epoch: u64,
    budget_bytes: usize,
    steps_floor: usize,
}

impl Default for VersionGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl VersionGraph {
    pub fn new() -> Self {
        Self::with_limits(VER_BUDGET_BYTES, VER_STEPS_FLOOR)
    }

    /// Budget overrides, for tests that must reach eviction without
    /// allocating half a gigabyte.
    pub fn with_limits(budget_bytes: usize, steps_floor: usize) -> Self {
        Self { nodes: Vec::new(), retained_bytes: 0, epoch: 0, budget_bytes, steps_floor }
    }

    /// Push a node for `committed`, then evict down to the budget. Returns
    /// the evicted loads for the caller to hand the janitor — they are
    /// NEVER dropped on the committing thread (ADR 0005's janitor rule;
    /// dossier 06 measured a worst retire-queue drop of 83.8 ms).
    pub fn record(&mut self, committed: &Committed) -> Vec<VersionNode> {
        if committed.epoch != self.epoch {
            log::warn!(
                "vergraph: dropping rev {} — epoch {} is not the chain's epoch {}",
                committed.rev,
                committed.epoch,
                self.epoch
            );
            return Vec::new();
        }
        // Rev order, not arrival order (L-4's discipline): `rev` is assigned
        // under the session lock but this sink is reached after it is
        // released, so two committing threads CAN arrive inverted. Replay
        // walks the chain forward, so the chain must BE forward.
        let pos = self.nodes.partition_point(|(r, _)| *r < committed.rev);
        let node = self.classify(committed, pos);
        self.retained_bytes += node.charge();
        self.nodes.insert(pos, (committed.rev, node));
        self.evict()
    }

    /// Materialized or replay-only?
    ///
    /// A node landing at the FRONT is always materialized, whatever it
    /// costs: a replay-only node replays from its nearest materialized
    /// ancestor, and the front node has none — see the type's invariant.
    ///
    /// Otherwise the plan's measured rule (over [`REPLAY_ONLY_THRESHOLD`]
    /// own-created bytes) decides — WITH one guard the measurement forces,
    /// see [`replay_pays_off`].
    fn classify(&self, committed: &Committed, pos: usize) -> VersionNode {
        let image_charge = committed.snapshot_charge;
        if pos > 0 && image_charge > REPLAY_ONLY_THRESHOLD {
            let replay_charge =
                ops_bytes(&committed.ops).saturating_add(ops_bytes(&committed.inverses));
            if replay_pays_off(replay_charge, image_charge) {
                return VersionNode::ReplayOnly {
                    ops: committed.ops.clone(),
                    inverses: committed.inverses.clone(),
                    charge: replay_charge,
                };
            }
        }
        VersionNode::Materialized { snapshot: committed.snapshot.clone(), charge: image_charge }
    }

    /// Drain everything (returned for the janitor) and root the chain at
    /// `epoch`. A document swap makes every retained version describe a
    /// document that is no longer open.
    pub fn clear(&mut self, epoch: u64) -> Vec<VersionNode> {
        self.epoch = epoch;
        self.retained_bytes = 0;
        self.nodes.drain(..).map(|(_, n)| n).collect()
    }

    /// Evict the oldest prefix until the budget is met, subject to two hard
    /// rules: never below [`Self::steps_floor`] retained steps, and the new
    /// front must be MATERIALIZED (a replay-only front would leave every
    /// node after it with no ancestor to replay from — evicting the base
    /// silently unreadable-izes its whole tail, which is the one way this
    /// mechanism could lose a version the graph still claims to hold).
    ///
    /// The cut is computed BEFORE anything is removed, so a budget that no
    /// legal cut can satisfy evicts the largest legal prefix rather than
    /// half-evicting into an illegal state. The budget is a target; the
    /// floor is a guarantee.
    fn evict(&mut self) -> Vec<VersionNode> {
        if self.retained_bytes <= self.budget_bytes {
            return Vec::new();
        }
        let max_cut = self.nodes.len().saturating_sub(self.steps_floor);
        let mut cut = 0usize;
        let mut freed = 0usize;
        let mut freed_at_cut = 0usize;
        for i in 0..max_cut {
            freed += self.nodes[i].1.charge();
            // i+1 is where the chain would start; it must be a base.
            if self.nodes.get(i + 1).is_some_and(|(_, n)| n.is_materialized()) {
                cut = i + 1;
                freed_at_cut = freed;
                if self.retained_bytes - freed_at_cut <= self.budget_bytes {
                    break;
                }
            }
        }
        if cut == 0 {
            return Vec::new();
        }
        self.retained_bytes -= freed_at_cut;
        self.nodes.drain(..cut).map(|(_, n)| n).collect()
    }

    /// Everything needed to rebuild `rev`, cloned out so the caller can drop
    /// the graph mutex before replaying. `None` when `rev` is unknown (never
    /// recorded, or evicted).
    pub fn replay_plan(&self, rev: u64) -> Option<ReplayPlan> {
        let idx = self.nodes.iter().position(|(r, _)| *r == rev)?;
        let base_idx = (0..=idx).rev().find(|&i| self.nodes[i].1.is_materialized())?;
        if base_idx == idx {
            let VersionNode::Materialized { snapshot, .. } = &self.nodes[idx].1 else {
                unreachable!("base_idx is materialized by construction")
            };
            return Some(ReplayPlan { rev, base: snapshot.clone(), steps: Vec::new() });
        }
        let VersionNode::Materialized { snapshot, .. } = &self.nodes[base_idx].1 else {
            unreachable!("base_idx is materialized by construction")
        };
        let steps = self.nodes[base_idx + 1..=idx]
            .iter()
            .map(|(_, n)| match n {
                VersionNode::ReplayOnly { ops, .. } => ops.clone(),
                VersionNode::Materialized { .. } => {
                    unreachable!("base_idx is the NEAREST materialized ancestor")
                }
            })
            .collect();
        Some(ReplayPlan { rev, base: snapshot.clone(), steps })
    }

    /// Convenience for tests and single-shot callers: [`Self::replay_plan`]
    /// then [`ReplayPlan::run`]. The result's `transport` is the replay
    /// BASE's and must not be restored — see [`ReplayPlan::run`]. Production callers holding the `HistoryLog`
    /// mutex must use the two halves (see `HistoryLog::materialize_version`)
    /// — the replay is CPU-bound and must not run under a lock.
    pub fn materialize(&self, rev: u64) -> Option<SessionSnapshot> {
        self.replay_plan(rev)?.run()
    }

    pub fn stats(&self) -> VersionStats {
        VersionStats {
            nodes: self.nodes.len(),
            materialized: self.nodes.iter().filter(|(_, n)| n.is_materialized()).count(),
            replay_only: self.nodes.iter().filter(|(_, n)| !n.is_materialized()).count(),
            retained_bytes: self.retained_bytes,
            oldest_rev: self.nodes.first().map(|(r, _)| *r),
            newest_rev: self.nodes.last().map(|(r, _)| *r),
        }
    }
}

/// Is storing ops + inverses actually cheaper than keeping the image?
///
/// THE MEASUREMENT THAT FORCES THIS GUARD (Task 7, and it contradicts a
/// premise of RESULTS.md §6's rule as written — ADR 0007 correction). Every
/// landed op that carries bulk content carries it BY VALUE: `MidiSetNotes`
/// holds the whole new note vector and its inverse holds the whole old one;
/// `MidiClipAdd` holds the entire clip. A materialized node, by contrast,
/// holds an Arc-SHARED image whose own-created bytes are one copy of that
/// same content. So for exactly the op class the 64 KB threshold targets,
/// the replay-only representation costs ~2x what it saves.
///
/// MEASURED, on Track C's multi-clip paste shape (20 clips x 200 notes, one
/// `MidiClipAdd` per clip; `size_of::<Op>() = 248`, `MidiNote = 16`,
/// `MidiClip = 184`). At the finding, charging no row heap on the ops side:
/// ops 68 960 B, ops + inverses 137 920 B, against an image own-created
/// charge of 67 680 B — **2.038x**. Under the parity accounting this file
/// ships (row `String` heap on both sides): ops **70 020 B**, ops + inverses
/// **140 040 B**, against **68 740 B** — **2.037x**. MARKED CORRECTION (ADR
/// 0007, Task 7 re-review): this row previously read 69 810 / 139 620 /
/// 68 530, which implies a row-heap sum of 850 B; the shipped
/// `midi_clip_heap` also counts `lane_id`, so the fixture's rows weigh
/// 1 060 B (lane_id 36 x 20 = 720, ids 90, track_ids 60, names 190). The
/// ratio and the conclusion are unchanged. The ops halves are `ops_bytes`'
/// actual output; the IMAGE half of BOTH rows is a HAND-COMPUTED PROXY, not
/// `charge_of`'s output — the real thing would add the list of pointers,
/// 20 x `size_of::<Arc<MidiClip>>()` = 160 B, which moves neither the ratio
/// nor which side of the 64 KB threshold the image lands on.
/// The pathology is SCALE-INVARIANT: replay is
/// ~2x content and the image ~1x content at any size, so no threshold VALUE
/// separates the classes — only this guard does.
///
/// It is not a mechanism-killer either: a single gain nudge on a 300-clip
/// project charges ~70 KB of image against ~500 B of ops, a ~140x
/// replay-only win, and this guard fires there.
///
/// So the threshold decides WHETHER to consider replay-only, and this
/// decides whether it would pay. Without the guard the mechanism is a
/// memory pessimization on the one class it was built for, which no
/// eviction budget can make correct.
///
/// BOTH SIDES ARE WEIGHED BY THE SAME RULES (fix round 1): `charge_of` and
/// [`ops_bytes`] share `snapshot::{track,clip,midi_clip}_heap`, so a row's
/// `String` heap counts on both sides and no future tightening of one can
/// silently move this decision boundary. Plugin rows are charged at their
/// slot on both sides.
///
/// The route to making replay-only pay off is named rather than left to be
/// rediscovered: the undo stack ALREADY retains these same op vectors for
/// the newest 200 entries, so a node sharing them (`Arc<Vec<Op>>` on both
/// `HistoryEntry` and `VersionNode`) would cost ~0 while shared. That is a
/// change to `HistoryEntry`'s shape and belongs to the round that owns it.
fn replay_pays_off(replay_charge: usize, image_charge: usize) -> bool {
    replay_charge < image_charge
}

/// Deep bytes a stored op list retains: the enum slots plus every heap
/// vector/blob hanging off them.
///
/// EXHAUSTIVE match over `Op`, deliberately (same discipline as
/// `ChangeSet::from_ops`): a new variant must fail to compile here until
/// somebody decides what it weighs.
///
/// PARITY WITH `charge_of` IS STRUCTURAL, NOT REMEMBERED (fix round 1):
/// this number is compared against an image charge in [`replay_pays_off`],
/// so the two must weigh a row the same way. Both sides call the SAME
/// `snapshot::{track,clip,midi_clip}_heap` helpers, so tightening one
/// tightens both. `charge_of` counting a row's `String` heap while this
/// function charged it at its slot biased the comparison toward
/// replay-only — the dangerous direction, since replay-only is the side
/// that can cost 2x.
pub fn ops_bytes(ops: &[Op]) -> usize {
    use super::snapshot::{clip_heap, midi_clip_heap, track_heap};
    use std::mem::size_of;
    let mut bytes = std::mem::size_of_val(ops);
    for op in ops {
        bytes += match op {
            Op::Set { object, from, to, .. } => {
                objectref_heap(object) + json_bytes(from) + json_bytes(to)
            }
            Op::TrackAdd { track, clips, clip_indices, .. }
            | Op::TrackRemove { track, clips, clip_indices, .. } => {
                track_heap(track)
                    + clips.len() * size_of::<crate::audio::types::Clip>()
                    + clips.iter().map(clip_heap).sum::<usize>()
                    + clip_indices.len() * size_of::<usize>()
            }
            Op::ClipAdd { clip, .. } | Op::ClipRemove { clip, .. } => clip_heap(clip),
            Op::TempoSet { events, meter, .. } => {
                events.len() * size_of::<crate::midi::TempoEvent>()
                    + meter.len() * size_of::<crate::midi::MeterEvent>()
            }
            Op::MidiClipAdd { clip, .. } | Op::MidiClipRemove { clip, .. } => {
                midi_clip_heap(clip) + clip.notes.len() * size_of::<crate::midi::MidiNote>()
            }
            Op::MidiSetNotes { clip, notes, .. } => {
                clip.as_str().len() + notes.len() * size_of::<crate::midi::MidiNote>()
            }
            Op::AutomationSetLane { lane, .. } => lane.as_ref().map_or(0, |l| {
                l.points.len() * size_of::<crate::plugins::automation::AutomationPoint>()
            }),
            // Plugin rows are charged at their slot on BOTH sides
            // (`charge_of`'s `instances.len() * size_of`), so no row heap
            // here either — parity, not an oversight.
            Op::PluginAdd { .. } => 0,
            Op::PluginRemove { state, params, .. } => {
                state.as_ref().map_or(0, |b| b.len())
                    + params.len() * size_of::<crate::plugins::ParamInfo>()
            }
            Op::PluginSetState { instance, state, .. } => instance.len() + state.len(),
        };
    }
    bytes
}

/// The id a `Set` addresses — heap the stored op really holds.
fn objectref_heap(object: &super::op::ObjectRef) -> usize {
    use super::op::ObjectRef;
    match object {
        ObjectRef::Track(id) => id.as_str().len(),
        ObjectRef::Clip(id) | ObjectRef::MidiClip(id) => id.as_str().len(),
        ObjectRef::Transport => 0,
        ObjectRef::Plugin(id) => id.len(),
    }
}

/// Heap bytes of a JSON payload (an op's `from`/`to`). Numbers and bools
/// live in the enum slot; strings, arrays and objects do not.
fn json_bytes(v: &serde_json::Value) -> usize {
    use std::mem::size_of;
    match v {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 0,
        serde_json::Value::String(s) => s.len(),
        serde_json::Value::Array(a) => {
            a.len() * size_of::<serde_json::Value>() + a.iter().map(json_bytes).sum::<usize>()
        }
        serde_json::Value::Object(o) => o
            .iter()
            .map(|(k, val)| k.len() + size_of::<serde_json::Value>() + json_bytes(val))
            .sum(),
    }
}

// ---------------------------------------------------------------------------
// Janitor
// ---------------------------------------------------------------------------

/// Off-thread drop sink. MEASURED why (dossier 06): the worst retire-queue
/// drop was 83.8 ms — never on a committing or UI-facing thread.
///
/// The thread does exactly one thing: receive and drop. It takes NO lock in
/// this system, so it can block nothing; the only thing it can delay is its
/// own queue.
pub struct Janitor {
    /// Boxed so tests can post a drop probe through the same channel the
    /// real loads use, without a test-only variant leaking into
    /// [`VersionNode`].
    tx: std::sync::mpsc::Sender<Box<dyn Send>>,
}

impl Default for Janitor {
    fn default() -> Self {
        Self::spawn()
    }
}

impl Janitor {
    /// Spawns the named thread ("aura-janitor"); dropping the last sender
    /// (i.e. dropping the `HistoryLog`) ends it.
    pub fn spawn() -> Janitor {
        let (tx, rx) = std::sync::mpsc::channel::<Box<dyn Send>>();
        let spawned = std::thread::Builder::new()
            .name("aura-janitor".into())
            .spawn(move || while rx.recv().is_ok() {});
        if let Err(e) = spawned {
            log::warn!("vergraph: janitor thread unavailable ({e}); evictions drop inline");
        }
        Janitor { tx }
    }

    /// Hand a load over. NEVER blocks (unbounded channel — eviction batches
    /// are rare and the janitor only drops). An empty load costs nothing and
    /// is not sent. If the thread is gone the load drops HERE rather than
    /// leaking, which is the one case a committing thread pays the drop.
    pub fn dispose(&self, load: Vec<VersionNode>) {
        if load.is_empty() {
            return;
        }
        self.dispose_any(Box::new(load));
    }

    /// The same door for anything droppable — the seam the janitor's own
    /// test uses to observe WHICH thread ran the `Drop`.
    pub fn dispose_any(&self, load: Box<dyn Send>) {
        if let Err(std::sync::mpsc::SendError(load)) = self.tx.send(load) {
            log::warn!("vergraph: janitor gone; dropping load inline");
            drop(load);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::Store;
    use crate::control::op::{ObjectRef, PropPath, TxMeta};
    use crate::control::session::Session;
    use crate::ids::{ContentId, LaneId, NoteId};
    use crate::midi::types::{MidiClip, MidiNote};
    use crate::midi::MidiStore;
    use parking_lot::Mutex;

    fn track(id: &str) -> crate::audio::types::TrackState {
        crate::audio::types::TrackState {
            id: id.into(),
            name: format!("Track {id}"),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    fn midi_clip(id: &str, track_id: &str, notes: Vec<MidiNote>) -> MidiClip {
        MidiClip {
            id: id.into(),
            track_id: track_id.into(),
            name: format!("MIDI {id}"),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            notes,
            next_note_id: 1,
            content_id: ContentId::mint(),
            lane_id: LaneId::default_for_track(track_id),
            content_length_ticks: None,
        }
    }

    fn note(tick: u32, key: u8) -> MidiNote {
        MidiNote { tick, length_ticks: 480, key, velocity: 100, channel: 0, note_id: NoteId(0) }
    }

    fn notes(n: usize) -> Vec<MidiNote> {
        (0..n).map(|i| note(i as u32 * 10, 60)).collect()
    }

    fn session_with_one_clip() -> Mutex<Session> {
        let mut store = Store::default();
        store.tracks.push(track("t-1"));
        let mut midi = MidiStore::default();
        midi.clips.push(midi_clip("mc-a", "t-1", vec![]));
        Mutex::new(Session::new(store, midi))
    }

    /// A `Committed` with a chosen charge, for the classifier and evictor
    /// tests that care about bytes rather than content.
    fn committed_with_charge(rev: u64, charge: usize, ops: Vec<Op>) -> Committed {
        Committed {
            rev,
            epoch: 0,
            inverses: ops.clone(),
            ops,
            effect: crate::control::session::EngineEffect::default(),
            meta: TxMeta::user("t"),
            snapshot: Arc::new(SessionSnapshot::empty()),
            snapshot_charge: charge,
        }
    }

    fn set_gain(track: &str, to: f64) -> Op {
        Op::Set {
            object: ObjectRef::Track(track.into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(to),
        }
    }

    #[test]
    fn a_small_batch_materializes_and_a_whole_clip_rewrite_goes_replay_only() {
        let mut g = VersionGraph::new();
        // The FRONT node is always materialized (it is the only possible
        // replay base), so seed one before testing the classifier.
        g.record(&committed_with_charge(1, 1024, vec![set_gain("t-1", -3.0)]));
        assert_eq!(g.stats().materialized, 1);

        // 1 KB own-created bytes: materialized.
        g.record(&committed_with_charge(2, 1024, vec![set_gain("t-1", -6.0)]));
        let s = g.stats();
        assert_eq!((s.nodes, s.materialized, s.replay_only), (2, 2, 0));

        // 100 KB own-created bytes — the measured whole-clip class — with an
        // op list SMALLER than the image charge: replay-only.
        let cheap_ops = vec![Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(50) }];
        g.record(&committed_with_charge(3, 100 * 1024, cheap_ops));
        let s = g.stats();
        assert_eq!((s.nodes, s.materialized, s.replay_only), (3, 2, 1));
        assert!(
            s.retained_bytes < 1024 + 1024 + 100 * 1024,
            "a replay-only node charges what it STORES, not the image it avoided"
        );
    }

    /// The FRONT node is a replay base or the chain has none, so it
    /// materializes whatever it costs — and a chain rooted at a replay-only
    /// node would be a chain nothing in it can be read from.
    #[test]
    fn the_front_node_materializes_however_expensive_it_is() {
        let mut g = VersionGraph::new();
        let cheap = vec![Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) }];
        g.record(&committed_with_charge(1, 100 * 1024 * 1024, cheap));
        assert_eq!(g.stats().materialized, 1, "nothing can replay from an empty chain");
        assert!(g.replay_plan(1).is_some(), "so the first rev is readable");
    }

    /// `rev` is assigned under the session lock; this sink is reached after
    /// it is released, so two committing threads can arrive INVERTED. The
    /// chain is rev-ordered, not arrival-ordered (L-4's discipline), because
    /// replay walks it forward.
    #[test]
    fn an_out_of_order_arrival_is_inserted_in_rev_order() {
        let m = session_with_one_clip();
        let mut g = VersionGraph::new();
        let base = Session::transact(&m, TxMeta::user("base"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) })
        })
        .unwrap();
        g.record(&base);

        let mut later = Vec::new();
        for n in [40usize, 80] {
            let mut c = Session::transact(&m, TxMeta::user("rewrite"), |tx| {
                tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(n) })
            })
            .unwrap();
            c.snapshot_charge = 10 * 1024 * 1024; // force replay-only
            later.push(c);
        }
        // The NEWER batch reaches the sink first.
        let (rev_a, rev_b) = (later[0].rev, later[1].rev);
        g.record(&later[1]);
        g.record(&later[0]);
        assert_eq!(g.stats().oldest_rev, Some(base.rev));
        assert_eq!(g.stats().newest_rev, Some(rev_b), "rev order, not arrival order");
        assert!(g.replay_plan(rev_a).is_some());

        let live = m.lock().published_handle().lock().clone();
        assert_eq!(
            canonical(&g.materialize(rev_b).expect("materializable")),
            canonical(&live),
            "replay applies the two batches in REV order — inverted, the 40-note \
             rewrite would land last and the document would be wrong"
        );
    }

    /// THE MEASUREMENT, pinned as a rule. Track C measured a multi-clip
    /// paste landing as ONE history entry of ~200 KB of ops. Its ops carry
    /// every note by value and its inverses carry them again, while the
    /// image charge is one Arc-shared copy — so replay-only would COST more
    /// than it saves, and the classifier must keep it materialized even
    /// though its charge is far over the 64 KB threshold.
    #[test]
    fn a_bulk_paste_stays_materialized_because_replay_only_would_cost_more() {
        let mut g = VersionGraph::new();
        g.record(&committed_with_charge(1, 1024, vec![set_gain("t-1", -3.0)]));

        // 20 clips x 200 notes: the paste shape.
        let paste: Vec<Op> = (0..20)
            .map(|i| Op::MidiClipAdd { clip: midi_clip(&format!("mc-{i}"), "t-1", notes(200)), index: i })
            .collect();
        let image_charge = 20 * (std::mem::size_of::<MidiClip>() + 200 * std::mem::size_of::<MidiNote>());
        let replay_charge = ops_bytes(&paste) * 2; // ops + inverses
        assert!(
            replay_charge > image_charge,
            "the premise: ops+inverses ({replay_charge}) really do exceed the image charge \
             ({image_charge}) for the paste shape — otherwise this test proves nothing"
        );
        assert!(image_charge > REPLAY_ONLY_THRESHOLD, "and the batch really is over the threshold");

        g.record(&committed_with_charge(2, image_charge, paste));
        let s = g.stats();
        assert_eq!(
            (s.materialized, s.replay_only),
            (2, 0),
            "the bulk paste must NOT go replay-only — that would store more bytes, not fewer"
        );
    }

    /// I-2, pinned as a LIMITATION rather than documented away: transport
    /// writes commit transiently, so they bump `rev` and leave no node — the
    /// chain has gaps no `apply_replay` step can carry. A materialized rev
    /// therefore reproduces CONTENT exactly and `transport` only as of its
    /// replay base. This test exists so that stops being a surprise: if a
    /// later round teaches the graph about transport, it fails and someone
    /// reads the doc that says why.
    #[test]
    fn a_materialized_rev_carries_the_replay_bases_transport_not_the_revs() {
        let m = session_with_one_clip();
        let mut g = VersionGraph::new();

        let base = Session::transact(&m, TxMeta::user("base"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) })
        })
        .unwrap();
        g.record(&base);
        assert!(!base.snapshot.transport.loop_enabled, "the base really has loop off");

        // A TRANSIENT transport write: it commits, it bumps `rev`, and the
        // sink never offers it to the graph (`!meta.transient` at the call
        // site) — so no node exists for it.
        Session::transact(&m, TxMeta::user("loop").transient(), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Transport,
                path: PropPath::LoopEnabled,
                from: serde_json::json!(false),
                to: serde_json::json!(true),
            })
        })
        .unwrap();

        // ...and then an ordinary content batch, forced replay-only.
        let mut later = Session::transact(&m, TxMeta::user("rewrite"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(40) })
        })
        .unwrap();
        later.snapshot_charge = 10 * 1024 * 1024;
        let rev = later.rev;
        g.record(&later);
        assert_eq!(g.stats().replay_only, 1, "the rev under test really is replay-only");

        let live = m.lock().published_handle().lock().clone();
        assert!(live.transport.loop_enabled, "the live document really has loop ON");
        let image = g.materialize(rev).expect("materializable");
        assert_eq!(canonical(&image), canonical(&live), "CONTENT is reproduced exactly");
        assert!(
            !image.transport.loop_enabled,
            "...but transport is the replay BASE's — a caller must not restore it \
             (see ReplayPlan::run)"
        );
    }

    /// `replay_pays_off` compares an image charge with an ops charge, so the
    /// two must weigh a row the same way — otherwise tightening one side
    /// moves the classification boundary with nobody deciding to move it.
    /// The image half of this pair is
    /// `snapshot::tests::charge_of_counts_a_rows_string_heap_and_not_just_its_slot`,
    /// which asserts the SAME delta for the same row.
    #[test]
    fn the_ops_side_weighs_a_rows_string_heap_exactly_as_the_image_side_does() {
        let with_path = |path: &str| {
            let mut clip = crate::audio::types::testutil::test_clip("c-1", "t-1");
            clip.source_path = path.to_string();
            ops_bytes(&[Op::ClipAdd { clip, index: 0 }])
        };
        let short = with_path("a.wav");
        let long = with_path(&format!("{}.wav", "d/".repeat(400)));
        assert_eq!(
            long - short,
            800 + 4 - 5,
            "the extra path bytes are charged on the ops side too, one for one"
        );
    }

    #[test]
    fn materializing_a_replay_only_rev_replays_from_the_nearest_ancestor() {
        let m = session_with_one_clip();
        let mut g = VersionGraph::new();

        // A base version, materialized (front node).
        let c0 = Session::transact(&m, TxMeta::user("base"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) })
        })
        .unwrap();
        g.record(&c0);

        // Two whole-clip rewrites, FORCED replay-only by a threshold-sized
        // image charge with a small op list.
        let mut expected_rev = 0;
        for n in [40usize, 80] {
            let c = Session::transact(&m, TxMeta::user("rewrite"), |tx| {
                tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(n) })
            })
            .unwrap();
            expected_rev = c.rev;
            let mut forced = c;
            forced.snapshot_charge = 10 * 1024 * 1024; // way over the threshold
            g.record(&forced);
        }
        let s = g.stats();
        assert_eq!((s.nodes, s.materialized, s.replay_only), (3, 1, 2));

        // The oracle: the same document, driven normally.
        let live = m.lock().published_handle().lock().clone();
        let replayed = g.materialize(expected_rev).expect("materializable");
        assert_eq!(replayed.rev, expected_rev);
        assert_eq!(
            canonical(&replayed),
            canonical(&live),
            "replaying from the nearest materialized ancestor reproduces the document"
        );
        assert_eq!(replayed.midi.clips[0].notes.len(), 80, "and it really is the LATEST content");
    }

    fn canonical(s: &SessionSnapshot) -> String {
        serde_json::to_string(&serde_json::json!({
            "tracks": &*s.tracks,
            "clips": &*s.clips,
            "ppq": s.midi.ppq,
            "tempo": &*s.midi.tempo_events,
            "meter": &*s.midi.meter_events,
            "midiClips": s.midi.clips.iter().map(|c| c.as_ref()).collect::<Vec<_>>(),
            "automation": &*s.automation,
            "plugins": s.plugins.instances,
        }))
        .unwrap()
    }

    #[test]
    fn eviction_respects_the_bytes_ceiling_and_the_steps_floor() {
        // Ceiling 10 KB, floor 3 steps.
        let mut g = VersionGraph::with_limits(10 * 1024, 3);
        let mut evicted_total = 0;
        for rev in 1..=6u64 {
            evicted_total += g.record(&committed_with_charge(rev, 4 * 1024, vec![set_gain("t", rev as f64)])).len();
        }
        let s = g.stats();
        assert_eq!(s.nodes, 3, "eviction stops AT the floor, never below it");
        assert_eq!(evicted_total, 3, "and the three oldest were handed out for disposal");
        assert_eq!(s.oldest_rev, Some(4), "the OLDEST go first");
        assert_eq!(s.newest_rev, Some(6));
        assert_eq!(s.retained_bytes, 3 * 4 * 1024, "retained_bytes tracks what is left");
        assert!(
            s.retained_bytes > g.budget_bytes,
            "the floor OUTRANKS the budget: 3 x 4 KB is over the 10 KB ceiling and stays"
        );

        // With room under the floor, eviction goes exactly as far as the
        // budget needs and no further.
        let mut g = VersionGraph::with_limits(10 * 1024, 1);
        for rev in 1..=4u64 {
            g.record(&committed_with_charge(rev, 4 * 1024, vec![set_gain("t", rev as f64)]));
        }
        let s = g.stats();
        assert_eq!(s.nodes, 2, "two 4 KB nodes fit under 10 KB; a third does not");
        assert_eq!(s.oldest_rev, Some(3));
    }

    /// The invariant eviction must not break: the retained chain always
    /// starts at a materialized base, so every retained rev is still
    /// materializable. Evicting a base out from under its replay-only tail
    /// would leave nodes the graph still counts but can no longer read.
    #[test]
    fn eviction_never_leaves_a_replay_only_node_without_its_base() {
        let mut g = VersionGraph::with_limits(4 * 1024, 1);
        let cheap = vec![Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) }];
        // Materialized base, then two replay-only nodes, then a new base.
        g.record(&committed_with_charge(1, 2 * 1024, cheap.clone()));
        g.record(&committed_with_charge(2, 100 * 1024, cheap.clone()));
        g.record(&committed_with_charge(3, 100 * 1024, cheap.clone()));
        assert_eq!(g.stats().replay_only, 2);
        // This one is small, so it materializes — and it is the first legal
        // cut point. Everything before it goes, or nothing does.
        g.record(&committed_with_charge(4, 2 * 1024, cheap));

        let s = g.stats();
        assert_eq!(s.oldest_rev, Some(4), "the cut landed ON the new materialized base");
        assert_eq!(s.nodes, 1);
        assert!(
            g.replay_plan(4).is_some(),
            "and what survived is still materializable"
        );
        assert!(g.replay_plan(3).is_none(), "what was evicted is honestly gone");
    }

    #[test]
    fn a_document_swap_drains_the_whole_chain_for_the_janitor() {
        let mut g = VersionGraph::new();
        g.record(&committed_with_charge(1, 1024, vec![set_gain("t", 1.0)]));
        g.record(&committed_with_charge(2, 1024, vec![set_gain("t", 2.0)]));
        let load = g.clear(7);
        assert_eq!(load.len(), 2, "every node is handed out, none dropped here");
        assert_eq!(g.stats(), VersionStats::default());

        // ...and the chain now belongs to the NEW epoch: a batch from the
        // old one is recorded nowhere.
        assert!(g.record(&committed_with_charge(3, 1024, vec![set_gain("t", 3.0)])).is_empty());
        assert_eq!(g.stats().nodes, 0, "a stale-epoch batch reaches no node");
    }

    /// A genuine PREFIX EVICTION with a survivor: what the drop frees is
    /// the evicted node's own-created content — provided no surviving image
    /// reuses it, which here it does not, because both versions rewrite the
    /// same clip and so each mints its own content Arc.
    ///
    /// (This used to drive `clear()`, which drains every node — it measured
    /// a total drain rather than the eviction it is named for. Fix round 1.)
    #[test]
    fn evicting_a_node_frees_its_own_created_content_and_nothing_shared() {
        let m = session_with_one_clip();
        // Two ~16 KB nodes do not fit under 20 KB; the floor allows the cut.
        let mut g = VersionGraph::with_limits(20 * 1024, 1);

        let rewrite = |g: &mut VersionGraph| {
            let c = Session::transact(&m, TxMeta::user("rewrite"), |tx| {
                tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1_000) })
            })
            .unwrap();
            let own = c.snapshot.midi.clips[0].clone();
            let tracks = c.snapshot.tracks.clone();
            let evicted = g.record(&c);
            (own, tracks, evicted)
        };

        let (own_1, _, evicted) = rewrite(&mut g);
        assert!(evicted.is_empty(), "one node fits under the budget");
        let (own_2, shared_tracks, evicted) = rewrite(&mut g);

        // THE PREFIX EVICTION: rev 1 is cut, rev 2 survives.
        assert_eq!(evicted.len(), 1);
        assert_eq!(g.stats().nodes, 1, "and a survivor really is left behind");
        assert!(
            Arc::strong_count(&own_1) > 1,
            "the load still holds rev 1's content, so the drop below IS the release"
        );
        drop(evicted);
        assert_eq!(
            Arc::strong_count(&own_1),
            1,
            "the evicted node's own-created clip content is freed — no survivor reuses it"
        );
        assert!(
            Arc::strong_count(&own_2) > 1,
            "...and the survivor's own content is untouched"
        );
        assert!(
            Arc::strong_count(&shared_tracks) > 1,
            "...as is the track list SHARED with the live document"
        );
    }

    /// I-1, pinned as a LIMITATION, because the module doc used to claim the
    /// opposite: `retained_bytes` is a LOWER BOUND. Structural sharing runs
    /// forward — a later image reuses an earlier one's untouched content
    /// Arcs — so evicting the creator of those bytes can free NOTHING while
    /// the graph subtracts the full charge.
    #[test]
    fn evicting_a_node_frees_only_what_no_survivor_reuses() {
        let m = session_with_one_clip();
        // Budget 1 B, floor 2: no cut is legal until there are three nodes,
        // and then it takes exactly rev 1.
        let mut g = VersionGraph::with_limits(1, 2);

        let v1 = Session::transact(&m, TxMeta::user("1000 notes"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1_000) })
        })
        .unwrap();
        let own = v1.snapshot.midi.clips[0].clone();
        let v1_charge = v1.snapshot_charge;
        assert!(v1_charge > 16_000, "v1 really created ~16 KB, got {v1_charge}");
        assert!(g.record(&v1).is_empty(), "the floor forbids a cut this early");
        drop(v1);

        // v2 touches only a TRACK, so its capture REUSES v1's clip content.
        let v2 = Session::transact(&m, TxMeta::user("gain"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::Gain,
                from: serde_json::Value::Null,
                to: serde_json::json!(-6.0),
            })
        })
        .unwrap();
        assert!(
            Arc::ptr_eq(&v2.snapshot.midi.clips[0], &own),
            "v2's image REUSES v1's content Arc — without that this test proves nothing"
        );
        assert!(g.record(&v2).is_empty());
        drop(v2);

        // v3 rewrites the clip, so the LIVE document stops holding v1's
        // content: the only remaining holders are the two images. This is
        // also the record that makes a cut legal.
        let v3 = Session::transact(&m, TxMeta::user("rewrite"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: notes(1) })
        })
        .unwrap();
        let evicted = g.record(&v3);
        drop(v3);

        assert_eq!(evicted.len(), 1, "rev 1 was evicted");
        drop(evicted);
        assert!(
            Arc::strong_count(&own) > 1,
            "THE POINT: v1's node is gone and its content is STILL HELD — by v2's \
             surviving image, which reuses it. The eviction freed none of it."
        );
        let reported = g.stats().retained_bytes;
        assert!(
            reported < v1_charge,
            "...while the graph reports {reported} bytes retained, having subtracted \
             all {v1_charge} of v1's charge. retained_bytes is a LOWER BOUND (I-1)."
        );
    }

    /// Ruling F-10's pin: the replay-only mechanism excludes no landed
    /// structural op, and that exclusion rule is vacuous only for as long as
    /// every structural op carries its minted ids on the wire. If one ever
    /// stopped, replay would mint DIFFERENT ids than the original commit and
    /// a materialized rev would diverge from the document it claims to be.
    #[test]
    fn every_landed_structural_op_carries_its_minted_ids() {
        let clip = crate::audio::types::testutil::test_clip("c-x", "t-x");
        let mc = midi_clip("mc-x", "t-x", vec![]);
        let row = crate::plugins::PluginInstanceInfo {
            id: "p-x".into(),
            uid: "stub:p-x".into(),
            name: "Stub".into(),
            format: "stub".into(),
            status: "stub".into(),
            track_id: None,
        };
        let cases: Vec<(Op, Vec<&str>)> = vec![
            (
                Op::TrackAdd { track: track("t-x"), index: 0, clips: vec![clip.clone()], clip_indices: vec![0] },
                vec!["t-x", "c-x"],
            ),
            (
                Op::TrackRemove { track: track("t-x"), index: 0, clips: vec![clip.clone()], clip_indices: vec![0] },
                vec!["t-x", "c-x"],
            ),
            (Op::ClipAdd { clip: clip.clone(), index: 0 }, vec!["c-x", "t-x"]),
            (Op::ClipRemove { clip, index: 0 }, vec!["c-x", "t-x"]),
            (Op::MidiClipAdd { clip: mc.clone(), index: 0 }, vec!["mc-x", "t-x"]),
            (Op::MidiClipRemove { clip: mc, index: 0 }, vec!["mc-x", "t-x"]),
            (Op::PluginAdd { row: row.clone(), index: 0 }, vec!["p-x"]),
            (Op::PluginRemove { row, index: 0, state: None, params: vec![] }, vec!["p-x"]),
        ];
        for (op, ids) in cases {
            let wire = serde_json::to_string(&op).expect("op serializes");
            for id in ids {
                assert!(
                    wire.contains(&format!("\"{id}\"")),
                    "structural op {wire} must carry the minted id {id} — replay mints nothing"
                );
            }
        }
    }

    /// Evicted loads are dropped on the janitor thread, never on the thread
    /// that committed (dossier 06: an 83.8 ms drop).
    #[test]
    fn evicted_loads_are_dropped_on_the_janitor_thread() {
        use std::sync::mpsc;

        struct Probe(mpsc::Sender<String>);
        impl Drop for Probe {
            fn drop(&mut self) {
                let name = std::thread::current().name().unwrap_or("<unnamed>").to_string();
                let _ = self.0.send(name);
            }
        }

        let (tx, rx) = mpsc::channel();
        let janitor = Janitor::spawn();

        // A REAL node holding a REAL image, plus a probe posted through the
        // same door the evictor uses.
        let mut g = VersionGraph::with_limits(1024, 0);
        let snap = Arc::new(SessionSnapshot::empty());
        let mut c = committed_with_charge(1, 4096, vec![set_gain("t", 1.0)]);
        c.snapshot = snap.clone();
        let load = {
            g.record(&c);
            let evicted = g.clear(0);
            assert_eq!(evicted.len(), 1);
            evicted
        };
        drop(c);
        assert_eq!(Arc::strong_count(&snap), 2, "this thread and the load hold it");
        janitor.dispose(load);
        janitor.dispose_any(Box::new(Probe(tx)));

        let name = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the load was dropped");
        assert_eq!(name, "aura-janitor", "the DROP ran on the janitor thread");
        // The real node was queued first, so by the time the probe's Drop
        // ran the node was already gone.
        assert_eq!(
            Arc::strong_count(&snap),
            1,
            "the evicted image was released without this thread ever dropping it"
        );
    }
}

