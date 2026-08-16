//! Plan F Task 5: `SessionSnapshot` — the COW substrate every version node,
//! rebuild source, and scratch session is built on.
//!
//! Every successful `Session::transact` captures an immutable,
//! Arc-structurally-shared image of the document and publishes it INSIDE the
//! session lock (see `Session::published`); every sanctioned non-op mutation
//! site — the epoch swaps, the adopt installs, R-1's pending-state seed —
//! republishes via `Session::republish_full` (each site carries a greppable
//! `// snapshot republish:` marker; that enumeration is the correctness
//! root, and `tests/snapshot_store.rs`'s equivalence sweep is the net).
//!
//! Downstream consumers: Task 6 (engine rebuild reads the published image
//! lock-free), Task 7 (version nodes hold `Committed::snapshot` and classify
//! on `Committed::snapshot_charge`), Task 8 (panic rollback restores from
//! the last good image), Task 9 (scratch sessions replay from an image).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::audio::types::{Clip, TrackState, TransportState};
use crate::control::op::{ObjectRef, Op};
use crate::control::session::{PluginDoc, Session};
use crate::ids::ClipId;
use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent};
use crate::plugins::automation::AutomationLane;

/// Immutable, Arc-structurally-shared image of the DOCUMENT (content
/// fields only — bookkeeping like `midi.dirty`, `midi.loaded_dir` and
/// `plugins.dirty_state` is deliberately excluded from the equivalence
/// contract; `plugins` is carried whole for simplicity but its
/// `dirty_state` field is advisory in a snapshot). Cloning is O(1).
#[derive(Clone)]
pub struct SessionSnapshot {
    pub rev: u64,
    pub epoch: u64,
    pub transport: TransportState, // small: cloned per capture
    pub project_dir: Option<PathBuf>,
    pub project_name: Option<String>,
    pub created_at: Option<String>,
    pub tracks: Arc<Vec<TrackState>>,
    pub clips: Arc<Vec<Clip>>, // audio placements
    pub midi: MidiSnapshot,
    pub automation: Arc<Vec<AutomationLane>>,
    /// Track F graph. Captured wholesale (the doc is small relative to
    /// MIDI content); a later round can split it per-curve if charge
    /// accounting needs it.
    pub modulation: Arc<crate::modulation::ModulationDoc>,
    pub plugins: Arc<PluginDoc>,
}

/// Per-pattern granularity (ruling F-1): the clip LIST re-allocates on
/// structural change (a Vec of pointers — cheap), but each clip's content
/// is its own Arc, reused untouched across versions that didn't edit it.
#[derive(Clone, PartialEq)]
pub struct MidiSnapshot {
    pub ppq: u32,
    pub tempo_events: Arc<Vec<TempoEvent>>,
    pub meter_events: Arc<Vec<MeterEvent>>,
    pub clips: Arc<Vec<Arc<MidiClip>>>,
    pub launch_maps: Arc<Vec<crate::midi::launch::LaunchMap>>,
}

impl MidiSnapshot {
    /// Standalone image of a LIVE `MidiStore`, for the callers that hold the
    /// document rather than a published image — the offline bounce and the
    /// playback unit tests. Structural sharing is not available to them (a
    /// live store owns its clips inline), so this allocates one Arc per
    /// clip; both callers are one-shot next to a full decode, so the cost is
    /// noise. `engine::rebuild` never calls this: it reads the image the
    /// committer already published.
    pub fn from_store(midi: &crate::midi::MidiStore) -> Self {
        Self {
            ppq: midi.ppq,
            tempo_events: Arc::new(midi.tempo_events.clone()),
            meter_events: Arc::new(midi.meter_events.clone()),
            clips: Arc::new(midi.clips.iter().cloned().map(Arc::new).collect()),
            launch_maps: Arc::new(midi.launch_maps.clone()),
        }
    }
}

/// Which parts of the document a batch touched — derived from the folded
/// ops, so capture re-allocates ONLY what changed. `all()` is the epoch/
/// non-op-writer path.
#[derive(Default, Debug, PartialEq)]
pub struct ChangeSet {
    pub transport: bool,
    pub tracks: bool,
    pub clips: bool,
    pub midi_meta: bool,      // ppq / tempo_events / meter_events
    pub midi_structure: bool, // clip list add/remove (re-derive whole list)
    pub midi_clips: BTreeSet<ClipId>, // per-clip content rewrites
    pub launch_maps: bool,    // named launchers + bindings + drive clips
    pub automation: bool,
    pub modulation: bool,
    pub plugins: bool,
    pub project_meta: bool, // dir/name/created_at
    /// Set ONLY by [`ChangeSet::all`] (never by `from_ops`): forces every
    /// midi clip's content Arc to re-derive too, not just the list.
    /// Reuse-by-id is sound WITHIN one document (`from_ops` flags every
    /// content touch), but the epoch/non-op-writer path replaces the
    /// document wholesale, and a newly adopted document may reuse a clip id
    /// with entirely different content — so a full capture must never trust
    /// the previous image's per-clip Arcs.
    pub midi_clips_all: bool,
}

impl ChangeSet {
    /// Everything re-derives — the epoch/non-op-writer path
    /// (`Session::republish_full`).
    pub fn all() -> Self {
        Self {
            transport: true,
            tracks: true,
            clips: true,
            midi_meta: true,
            midi_structure: true,
            midi_clips: BTreeSet::new(),
            launch_maps: true,
            automation: true,
            modulation: true,
            plugins: true,
            project_meta: true,
            midi_clips_all: true,
        }
    }

    /// Op → touched parts. EXHAUSTIVE match over `Op` (a new variant is a
    /// compile error here — this table is the capture correctness root):
    /// Set{Track,_}→tracks · Set{Clip,_}→clips · Set{MidiClip,_}→that clip
    /// (placement fields live on the row, so the row's Arc re-derives)
    /// · Set{Transport,_}→transport · Set{Plugin,_}→plugins
    /// · TrackAdd/TrackRemove→tracks+clips+modulation+automation
///   (`apply_raw` writes the graph and re-derives lanes)
    /// · ClipAdd/ClipRemove→clips · TempoSet→midi_meta+transport (bpm mirror)
    /// · MidiClipAdd/MidiClipRemove→midi_structure
    /// · MidiSetNotes{clip,..}→midi_clips.insert(clip)
    /// · LaunchBindingSet/LaunchDriveSet/LaunchMapSet→launch_maps
    /// · AutomationSetLane→automation · PluginAdd/PluginRemove/PluginSetState→plugins
    pub fn from_ops(ops: &[Op]) -> Self {
        let mut cs = ChangeSet::default();
        for op in ops {
            // No wildcard arm anywhere below — a new `Op` (or `ObjectRef`)
            // variant must fail to compile until its mapping is decided.
            match op {
                Op::Set { object, .. } => match object {
                    ObjectRef::Track(_) => cs.tracks = true,
                    ObjectRef::Clip(_) => cs.clips = true,
                    ObjectRef::MidiClip(id) => {
                        cs.midi_clips.insert(id.clone());
                    }
                    ObjectRef::Transport => cs.transport = true,
                    ObjectRef::Plugin(_) => cs.plugins = true,
                },
                Op::TrackAdd { .. } | Op::TrackRemove { .. } => {
                    cs.tracks = true;
                    cs.clips = true; // a track op carries/removes its clips
                    // apply_raw also writes session.modulation and, when
                    // that set is non-empty, sync_derived_lanes. Always
                    // re-derive: a no-op is one Arc.
                    cs.modulation = true;
                    cs.automation = true;
                }
                Op::ClipAdd { .. } | Op::ClipRemove { .. } => cs.clips = true,
                Op::TempoSet { .. } => {
                    cs.midi_meta = true;
                    cs.transport = true; // store.transport.tempo_bpm mirror
                }
                // DEVIATION from the plan's table, which maps these to
                // `midi_structure` alone: per-clip reuse is keyed by clip ID,
                // so a fold that retires an id and re-introduces it with new
                // content would reuse the RETIRED content. Flagging the id
                // costs one clip's re-derive (a fresh add re-derives anyway)
                // and makes the charge honest for an undo-of-remove.
                Op::MidiClipAdd { clip, .. } | Op::MidiClipRemove { clip, .. } => {
                    cs.midi_structure = true;
                    cs.midi_clips.insert(clip.id.clone());
                }
                Op::MidiSetNotes { clip, .. } => {
                    cs.midi_clips.insert(clip.clone());
                }
                Op::LaunchBindingSet { .. }
                | Op::LaunchDriveSet { .. }
                | Op::LaunchMapSet { .. } => {
                    cs.launch_maps = true;
                }
                Op::AutomationSetLane { .. } => {
                    cs.automation = true;
                    cs.modulation = true; // facade writes the graph too
                }
                Op::ModulationSetCurve { .. }
                | Op::ModulationSetBinding { .. }
                | Op::AutomationClipSet { .. } => {
                    cs.modulation = true;
                    cs.automation = true; // sync_derived_lanes rewrites lanes
                }
                Op::PluginAdd { .. } | Op::PluginRemove { .. } | Op::PluginSetState { .. } => {
                    cs.plugins = true
                }
            }
        }
        cs
    }

    /// True when the midi clip LIST must re-derive (any structural change,
    /// any per-clip rewrite, or a full capture).
    fn midi_list_rederives(&self) -> bool {
        self.midi_structure || self.midi_clips_all || !self.midi_clips.is_empty()
    }
}

impl SessionSnapshot {
    /// The pre-first-publish placeholder `Session::new` starts from — every
    /// field empty. Never observable through the published slot: `Session`'s
    /// constructors replace it with a real full capture before returning.
    pub(crate) fn empty() -> Self {
        Self {
            rev: 0,
            epoch: 0,
            transport: TransportState::default(),
            project_dir: None,
            project_name: None,
            created_at: None,
            tracks: Arc::new(Vec::new()),
            clips: Arc::new(Vec::new()),
            midi: MidiSnapshot {
                ppq: 0,
                tempo_events: Arc::new(Vec::new()),
                meter_events: Arc::new(Vec::new()),
                clips: Arc::new(Vec::new()),
                launch_maps: Arc::new(Vec::new()),
            },
            automation: Arc::new(Vec::new()),
            modulation: Arc::new(crate::modulation::ModulationDoc::default()),
            plugins: Arc::new(PluginDoc::default()),
        }
    }

    /// Capture `session`'s document against the previous image, re-allocating
    /// only the parts `changed` flags; everything else reuses `prev`'s Arcs
    /// untouched (that reuse is exactly what `charge_of` does NOT charge).
    ///
    /// The small always-cloned fields (`transport`, project meta) are copied
    /// from live truth unconditionally — they are cheap, and copying truth
    /// can never go stale; the flags on `changed` still gate their CHARGE.
    ///
    /// The per-clip reuse in the midi list is by id — sound because
    /// `ChangeSet::from_ops` is exhaustive (every op that touches a clip's
    /// row flags it) and the full-capture path (`midi_clips_all`) never
    /// reuses. The lookup goes through a map built ONCE per capture (Task 7:
    /// it was a linear scan per clip, O(n²) on a structural rebuild, and
    /// this runs INSIDE the session lock on every commit — a bulk paste is
    /// exactly the shape that makes clip counts stop being small).
    pub(crate) fn capture(session: &Session, prev: &SessionSnapshot, changed: &ChangeSet) -> Self {
        let reusable: std::collections::HashMap<&ClipId, &Arc<MidiClip>> =
            if changed.midi_clips_all || !changed.midi_list_rederives() {
                std::collections::HashMap::new()
            } else {
                prev.midi.clips.iter().map(|c| (&c.id, c)).collect()
            };
        let midi = MidiSnapshot {
            ppq: session.midi.ppq,
            tempo_events: if changed.midi_meta {
                Arc::new(session.midi.tempo_events.clone())
            } else {
                prev.midi.tempo_events.clone()
            },
            meter_events: if changed.midi_meta {
                Arc::new(session.midi.meter_events.clone())
            } else {
                prev.midi.meter_events.clone()
            },
            clips: if changed.midi_list_rederives() {
                Arc::new(
                    session
                        .midi
                        .clips
                        .iter()
                        .map(|c| {
                            if !changed.midi_clips_all && !changed.midi_clips.contains(&c.id) {
                                if let Some(reuse) = reusable.get(&c.id) {
                                    return (*reuse).clone();
                                }
                            }
                            Arc::new(c.clone())
                        })
                        .collect(),
                )
            } else {
                prev.midi.clips.clone()
            },
            launch_maps: if changed.launch_maps {
                Arc::new(session.midi.launch_maps.clone())
            } else {
                prev.midi.launch_maps.clone()
            },
        };
        Self {
            rev: session.rev,
            epoch: session.epoch,
            transport: session.store.transport.clone(),
            project_dir: session.store.project_dir.clone(),
            project_name: session.store.project_name.clone(),
            created_at: session.store.created_at.clone(),
            tracks: if changed.tracks {
                Arc::new(session.store.tracks.clone())
            } else {
                prev.tracks.clone()
            },
            clips: if changed.clips {
                Arc::new(session.store.clips.clone())
            } else {
                prev.clips.clone()
            },
            midi,
            automation: if changed.automation {
                Arc::new(session.automation.lanes.clone())
            } else {
                prev.automation.clone()
            },
            modulation: if changed.modulation {
                Arc::new(session.modulation.clone())
            } else {
                prev.modulation.clone()
            },
            plugins: if changed.plugins {
                Arc::new(session.plugins.clone())
            } else {
                prev.plugins.clone()
            },
        }
    }
}

/// Deep bytes of every part `changed` re-allocated — the version node's
/// charge (own-created bytes, RESULTS.md's accounting unit). Deterministic
/// and documented: a re-derived collection charges
/// `len * size_of::<Element>()` plus, for midi clips, `notes.len() *
/// size_of::<MidiNote>()` per rebuilt clip Arc; `plugins` charges rows +
/// params + pending_state byte lengths. An approximation of allocator
/// truth, exact enough for classification (the threshold is 64 KB and the
/// error is bounded by struct padding plus inline heap fields — strings and
/// small inner vecs — that `size_of` doesn't see).
///
/// TASK 7 CLOSED THE STRING GAP (Task 5 carried it forward as accepted: a
/// row's `String` fields charged their 24-byte slot, not their heap bytes).
/// It has to be closed because this number DECIDES the 64 KB replay-only
/// classification: a `Clip` carries a `source_path` that is routinely
/// longer than its slot, and a row-heavy batch under-charged by tens of
/// percent lands on the wrong side of the threshold. Note counts, the other
/// half of a big charge, were never affected — `MidiNote` is inline.
pub fn charge_of(next: &SessionSnapshot, changed: &ChangeSet) -> usize {
    use std::mem::size_of;
    let mut charge = 0usize;
    if changed.transport {
        charge += size_of::<TransportState>();
    }
    if changed.project_meta {
        charge += next.project_dir.as_ref().map_or(0, |d| d.as_os_str().len());
        charge += next.project_name.as_ref().map_or(0, String::len);
        charge += next.created_at.as_ref().map_or(0, String::len);
    }
    if changed.tracks {
        charge += next.tracks.len() * size_of::<TrackState>();
        charge += next.tracks.iter().map(track_heap).sum::<usize>();
    }
    if changed.clips {
        charge += next.clips.len() * size_of::<Clip>();
        charge += next.clips.iter().map(clip_heap).sum::<usize>();
    }
    if changed.midi_meta {
        charge += next.midi.tempo_events.len() * size_of::<TempoEvent>();
        charge += next.midi.meter_events.len() * size_of::<MeterEvent>();
    }
    if changed.launch_maps {
        charge += next.midi.launch_maps.len() * size_of::<crate::midi::launch::LaunchMap>();
        charge += next.midi.launch_maps.iter().map(launch_map_heap).sum::<usize>();
    }
    if changed.midi_list_rederives() {
        // The list itself: a Vec of pointers.
        charge += next.midi.clips.len() * size_of::<Arc<MidiClip>>();
        // The rebuilt clip Arcs: row + note content.
        for clip in next.midi.clips.iter() {
            if changed.midi_clips_all || changed.midi_clips.contains(&clip.id) {
                charge += size_of::<MidiClip>()
                    + midi_clip_heap(clip)
                    + clip.notes.len() * size_of::<MidiNote>();
            }
        }
    }
    if changed.automation {
        charge += next.automation.len() * size_of::<AutomationLane>();
        charge += next.automation.iter().map(automation_lane_heap).sum::<usize>();
    }
    if changed.modulation {
        charge += size_of::<crate::modulation::ModulationDoc>();
        charge += next.modulation.curves.iter().map(curve_heap).sum::<usize>();
        charge += next.modulation.bindings.iter().map(binding_heap).sum::<usize>();
        charge += next
            .modulation
            .automation_clips
            .iter()
            .map(automation_clip_heap)
            .sum::<usize>();
    }
    if changed.plugins {
        charge += next.plugins.instances.len() * size_of::<crate::plugins::PluginInstanceInfo>();
        for params in next.plugins.params.values() {
            charge += params.len() * size_of::<crate::plugins::ParamInfo>();
        }
        for bytes in next.plugins.pending_state.values() {
            charge += bytes.len();
        }
    }
    charge
}

/// Heap bytes of a row's `String`/id fields (see [`charge_of`]).
///
/// `pub(crate)` because `vergraph::ops_bytes` weighs the SAME rows on the
/// other side of `replay_pays_off`'s comparison, and two hand-matched
/// rules would drift the moment one is tightened. Sharing the helper makes
/// parity structural instead of remembered.
pub(crate) fn track_heap(t: &TrackState) -> usize {
    t.id.as_str().len()
        + t.name.len()
        + t.kind.len()
        + t.color.len()
        + t.instrument_id.as_ref().map_or(0, String::len)
}

pub(crate) fn clip_heap(c: &Clip) -> usize {
    c.id.as_str().len()
        + c.track_id.as_str().len()
        + c.name.len()
        + c.source_path.len()
        + c.source_id.as_str().len()
}

pub(crate) fn midi_clip_heap(c: &MidiClip) -> usize {
    c.id.as_str().len() + c.track_id.as_str().len() + c.name.len() + c.lane_id.as_str().len()
}

pub(crate) fn automation_lane_heap(l: &AutomationLane) -> usize {
    use crate::plugins::automation::AutomationPoint;
    use std::mem::size_of;
    l.id.len()
        + l.target_node.len()
        + l.points.len() * size_of::<AutomationPoint>()
}

pub(crate) fn curve_heap(c: &crate::modulation::Curve) -> usize {
    use crate::plugins::automation::AutomationPoint;
    use std::mem::size_of;
    size_of::<crate::modulation::Curve>()
        + c.id.len()
        + c.name.len()
        + c.points.len() * size_of::<AutomationPoint>()
}

pub(crate) fn binding_heap(b: &crate::modulation::Binding) -> usize {
    use std::mem::size_of;
    size_of::<crate::modulation::Binding>() + b.id.len()
}

pub(crate) fn automation_clip_heap(c: &crate::modulation::AutomationClip) -> usize {
    use std::mem::size_of;
    size_of::<crate::modulation::AutomationClip>()
        + c.id.len()
        + c.track_id.len()
        + c.curve_id.len()
}

pub(crate) fn launch_map_heap(m: &crate::midi::launch::LaunchMap) -> usize {
    m.id.len()
        + m.name.len()
        + m.drive_clip_ids.iter().map(String::len).sum::<usize>()
        + m.bindings.iter().map(launch_binding_heap).sum::<usize>()
}

pub(crate) fn launch_binding_heap(b: &crate::midi::launch::LaunchBinding) -> usize {
    b.id.len() + b.name.len() + launch_target_heap(&b.target)
}

fn launch_target_heap(t: &crate::midi::launch::LaunchTarget) -> usize {
    match t {
        crate::midi::launch::LaunchTarget::Region { track_ids, .. } => {
            track_ids.iter().map(String::len).sum()
        }
        crate::midi::launch::LaunchTarget::Clip { clip_id } => clip_id.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::{Store, TrackState};
    use crate::control::op::{ObjectRef, Op, PropPath, TxMeta};
    use crate::ids::{ContentId, LaneId, NoteId};
    use crate::midi::MidiStore;
    use parking_lot::Mutex;

    fn track(id: &str) -> TrackState {
        TrackState {
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
            inserts: Vec::new(),
        }
    }

    fn midi_clip(id: &str, track_id: &str) -> MidiClip {
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
            transpose_semitones: 0,
            velocity_offset: 0,
        }
    }

    fn stub_row(id: &str) -> crate::plugins::PluginInstanceInfo {
        crate::plugins::PluginInstanceInfo {
            id: id.into(),
            uid: format!("stub:{id}"),
            name: format!("Stub {id}"),
            format: "stub".into(),
            status: "stub".into(),
            track_id: None,
        }
    }

    fn note(tick: u32, key: u8) -> MidiNote {
        MidiNote { tick, length_ticks: 480, key, velocity: 100, channel: 0, note_id: NoteId(0) }
    }

    /// Session with 2 tracks + 2 midi clips, published at construction
    /// (`Session::new` runs the first full capture itself).
    fn two_clip_session() -> Mutex<Session> {
        let mut store = Store::default();
        store.tracks.push(track("t-1"));
        store.tracks.push(track("t-2"));
        let mut midi = MidiStore::default();
        midi.clips.push(midi_clip("mc-a", "t-1"));
        midi.clips.push(midi_clip("mc-b", "t-2"));
        Mutex::new(Session::new(store, midi))
    }

    #[test]
    fn capture_reuses_untouched_arcs_and_reallocates_touched_ones() {
        let m = two_clip_session();
        let prev = m.lock().published_handle().lock().clone();

        let c = Session::transact(&m, TxMeta::user("notes on A"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: vec![note(0, 60)] })
        })
        .unwrap();
        let next = c.snapshot;

        // Untouched parts: the very same allocations, pointer-for-pointer.
        assert!(Arc::ptr_eq(&prev.tracks, &next.tracks), "tracks Arc must be reused");
        assert!(Arc::ptr_eq(&prev.clips, &next.clips), "audio clips Arc must be reused");
        assert!(
            Arc::ptr_eq(&prev.midi.tempo_events, &next.midi.tempo_events),
            "tempo events Arc must be reused"
        );
        assert!(
            Arc::ptr_eq(&prev.plugins, &next.plugins),
            "plugin doc Arc must be reused"
        );
        let prev_b = prev.midi.clips.iter().find(|c| c.id == "mc-b").unwrap();
        let next_b = next.midi.clips.iter().find(|c| c.id == "mc-b").unwrap();
        assert!(Arc::ptr_eq(prev_b, next_b), "clip B's content Arc must be reused");

        // Touched parts: fresh allocations.
        assert!(
            !Arc::ptr_eq(&prev.midi.clips, &next.midi.clips),
            "the clip LIST re-derives when any clip's content changed"
        );
        let prev_a = prev.midi.clips.iter().find(|c| c.id == "mc-a").unwrap();
        let next_a = next.midi.clips.iter().find(|c| c.id == "mc-a").unwrap();
        assert!(!Arc::ptr_eq(prev_a, next_a), "clip A's content Arc must be fresh");
        assert_eq!(next_a.notes.len(), 1, "and carry the new notes");

        // The published slot moved to `next` — same image `Committed` carries.
        assert!(Arc::ptr_eq(&m.lock().published_handle().lock().clone(), &next));
    }

    #[test]
    fn a_point_set_charges_under_the_point_cap_and_a_clip_rewrite_charges_its_content() {
        let m = two_clip_session();

        // Set{Track,Gain}: a point change — charge stays under the 8 KB
        // point cap (POINT_CAP sanity, RESULTS.md).
        let c = Session::transact(&m, TxMeta::user("gain"), |tx| {
            tx.apply(Op::Set {
                object: ObjectRef::Track("t-1".into()),
                path: PropPath::Gain,
                from: serde_json::Value::Null,
                to: serde_json::json!(-6.0),
            })
        })
        .unwrap();
        assert!(
            c.snapshot_charge <= 8 * 1024,
            "a point Set must charge under the point cap, got {}",
            c.snapshot_charge
        );
        assert!(c.snapshot_charge > 0, "but a real re-derive is never free");

        // MidiSetNotes with 4 000 notes: charge covers the rewritten content.
        let notes: Vec<MidiNote> = (0..4_000u32).map(|i| note(i * 10, 60)).collect();
        let c = Session::transact(&m, TxMeta::user("big notes"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes })
        })
        .unwrap();
        assert!(
            c.snapshot_charge >= 4_000 * std::mem::size_of::<MidiNote>(),
            "a clip rewrite must charge its note content, got {}",
            c.snapshot_charge
        );
    }

    /// The charge decides the 64 KB replay-only classification, so a row's
    /// heap has to be in it: a `Clip`'s `source_path` is a real path, not a
    /// 24-byte slot, and a clip-heavy batch charged by `size_of` alone lands
    /// on the wrong side of the threshold.
    #[test]
    fn charge_of_counts_a_rows_string_heap_and_not_just_its_slot() {
        let charge_for = |path: &str| {
            let m = two_clip_session();
            let mut clip = crate::audio::types::testutil::test_clip("c-1", "t-1");
            clip.source_path = path.to_string();
            Session::transact(&m, TxMeta::user("add clip"), |tx| {
                tx.apply(Op::ClipAdd { clip: clip.clone(), index: 0 })
            })
            .unwrap()
            .snapshot_charge
        };
        let short = charge_for("a.wav");
        let long = charge_for(&format!("{}.wav", "d/".repeat(400)));
        assert_eq!(
            long - short,
            800 + 4 - 5,
            "the extra path bytes are charged, one for one"
        );
    }

    #[test]
    fn changeset_from_ops_is_exhaustive_and_correct() {
        use std::collections::BTreeSet;
        // One assertion per Op variant against the doc table on `from_ops`.
        // A new variant breaks the `from_ops` match at compile time; this
        // test pins the mapping for the variants that exist today.
        let t = track("t-x");
        let clip = crate::audio::types::testutil::test_clip("c-x", "t-x");
        let mc = midi_clip("mc-x", "t-x");
        let row = crate::plugins::PluginInstanceInfo {
            id: "p-x".into(),
            uid: "stub:p-x".into(),
            name: "Stub".into(),
            format: "stub".into(),
            status: "stub".into(),
            track_id: None,
        };
        let set = |object: ObjectRef, path: PropPath| Op::Set {
            object,
            path,
            from: serde_json::Value::Null,
            to: serde_json::json!(0.0),
        };
        let cs = |ops: &[Op]| ChangeSet::from_ops(ops);
        let clip_ids = |ids: &[&str]| -> BTreeSet<ClipId> {
            ids.iter().map(|s| ClipId::from(*s)).collect()
        };

        // Set{Track,_} → tracks
        let c = cs(&[set(ObjectRef::Track("t-x".into()), PropPath::Gain)]);
        assert_eq!(c, ChangeSet { tracks: true, ..Default::default() });
        // Set{Clip,_} → clips
        let c = cs(&[set(ObjectRef::Clip("c-x".into()), PropPath::TimelineStartSamples)]);
        assert_eq!(c, ChangeSet { clips: true, ..Default::default() });
        // Set{MidiClip,_} → that clip (placement lives on the row)
        let c = cs(&[set(ObjectRef::MidiClip("mc-x".into()), PropPath::LengthTicks)]);
        assert_eq!(c, ChangeSet { midi_clips: clip_ids(&["mc-x"]), ..Default::default() });
        // Set{Transport,_} → transport
        let c = cs(&[set(ObjectRef::Transport, PropPath::LoopEnabled)]);
        assert_eq!(c, ChangeSet { transport: true, ..Default::default() });
        // Set{Plugin,_} → plugins
        let c = cs(&[set(ObjectRef::Plugin("p-x".into()), PropPath::Param { index: 0 })]);
        assert_eq!(c, ChangeSet { plugins: true, ..Default::default() });
        // TrackAdd / TrackRemove → tracks + clips + modulation + automation
        let c = cs(&[Op::TrackAdd { track: t.clone(), index: 0, clips: vec![], clip_indices: vec![], automation_clips: vec![], bindings: vec![] }]);
        assert_eq!(c, ChangeSet { tracks: true, clips: true, modulation: true, automation: true, ..Default::default() });
        let c = cs(&[Op::TrackRemove { track: t, index: 0, clips: vec![], clip_indices: vec![] }]);
        assert_eq!(c, ChangeSet { tracks: true, clips: true, modulation: true, automation: true, ..Default::default() });
        // ClipAdd / ClipRemove → clips
        let c = cs(&[Op::ClipAdd { clip: clip.clone(), index: 0 }]);
        assert_eq!(c, ChangeSet { clips: true, ..Default::default() });
        let c = cs(&[Op::ClipRemove { clip, index: 0 }]);
        assert_eq!(c, ChangeSet { clips: true, ..Default::default() });
        // TempoSet → midi_meta + transport (the store.transport.tempo_bpm mirror)
        let c = cs(&[Op::TempoSet { ppq: 960, events: vec![], meter: vec![] }]);
        assert_eq!(c, ChangeSet { midi_meta: true, transport: true, ..Default::default() });
        // MidiClipAdd / MidiClipRemove → midi_structure + that clip's id
        // (deviation, see `from_ops`).
        let c = cs(&[Op::MidiClipAdd { clip: mc.clone(), index: 0 }]);
        assert_eq!(
            c,
            ChangeSet { midi_structure: true, midi_clips: clip_ids(&["mc-x"]), ..Default::default() }
        );
        let c = cs(&[Op::MidiClipRemove { clip: mc, index: 0 }]);
        assert_eq!(
            c,
            ChangeSet { midi_structure: true, midi_clips: clip_ids(&["mc-x"]), ..Default::default() }
        );
        // MidiSetNotes → midi_clips.insert(clip)
        let c = cs(&[Op::MidiSetNotes { clip: "mc-x".into(), notes: vec![] }]);
        assert_eq!(c, ChangeSet { midi_clips: clip_ids(&["mc-x"]), ..Default::default() });
        // Launch* → launch_maps
        let c = cs(&[Op::LaunchBindingSet {
            map_id: String::new(),
            id: "lb-x".into(),
            binding: None,
        }]);
        assert_eq!(c, ChangeSet { launch_maps: true, ..Default::default() });
        let c = cs(&[Op::LaunchDriveSet {
            map_id: String::new(),
            clip_id: "mc-x".into(),
            on: true,
        }]);
        assert_eq!(c, ChangeSet { launch_maps: true, ..Default::default() });
        let c = cs(&[Op::LaunchMapSet { id: "default".into(), map: None }]);
        assert_eq!(c, ChangeSet { launch_maps: true, ..Default::default() });
        // AutomationSetLane → automation + modulation
        let c = cs(&[Op::AutomationSetLane { key: "a-x".into(), lane: None }]);
        assert_eq!(
            c,
            ChangeSet { automation: true, modulation: true, ..Default::default() }
        );
        // Modulation* / AutomationClipSet → modulation + automation (derived lanes)
        let c = cs(&[Op::ModulationSetCurve { key: "cur-x".into(), curve: None }]);
        assert_eq!(c, ChangeSet { modulation: true, automation: true, ..Default::default() });
        let c = cs(&[Op::ModulationSetBinding { key: "b-x".into(), binding: None }]);
        assert_eq!(c, ChangeSet { modulation: true, automation: true, ..Default::default() });
        let c = cs(&[Op::AutomationClipSet { key: "acl-x".into(), clip: None }]);
        assert_eq!(c, ChangeSet { modulation: true, automation: true, ..Default::default() });
        // PluginAdd / PluginRemove / PluginSetState → plugins
        let c = cs(&[Op::PluginAdd { row: row.clone(), index: 0 }]);
        assert_eq!(c, ChangeSet { plugins: true, ..Default::default() });
        let c = cs(&[Op::PluginRemove { row, index: 0, state: None, params: vec![] }]);
        assert_eq!(c, ChangeSet { plugins: true, ..Default::default() });
        let c = cs(&[Op::PluginSetState { instance: "p-x".into(), state: vec![1] }]);
        assert_eq!(c, ChangeSet { plugins: true, ..Default::default() });
        // And the epoch path: `all()` re-derives everything, including every
        // clip's content (never trusts reuse-by-id across a document swap).
        assert!(ChangeSet::all().midi_clips_all);
        assert!(ChangeSet::all().midi_list_rederives());
    }

    /// Per-clip reuse is keyed by clip ID, so a batch that retires an id and
    /// re-introduces it with different content in the SAME fold would reuse
    /// the retired content — the one way id-keyed reuse can lie.
    #[test]
    fn a_batch_that_replaces_a_clip_id_wholesale_does_not_reuse_the_retired_content() {
        let m = two_clip_session();
        let mut replacement = midi_clip("mc-a", "t-1");
        replacement.notes = vec![note(0, 60)];
        let c = Session::transact(&m, TxMeta::user("replace A"), |tx| {
            tx.apply(Op::MidiClipRemove { clip: midi_clip("mc-a", "t-1"), index: 0 })?;
            tx.apply(Op::MidiClipAdd { clip: replacement, index: 0 })
        })
        .unwrap();
        let a = c.snapshot.midi.clips.iter().find(|c| c.id == "mc-a").unwrap();
        assert_eq!(
            a.notes.len(),
            1,
            "the image must carry the RE-ADDED clip's content, not the removed one's"
        );
    }

    /// `PluginDoc::state_rev` keys the live plugin node cache and its own doc
    /// says it only ever moves forward. A content restore that rewound it
    /// would leave the node built from the OTHER blob rendering — exactly the
    /// silent-stale-sound class PR #25 fixed.
    #[test]
    fn restore_from_snapshot_advances_state_rev_instead_of_rewinding_it() {
        let m = two_clip_session();
        Session::transact(&m, TxMeta::user("add plugin"), |tx| {
            tx.apply(Op::PluginAdd { row: stub_row("p-1"), index: 0 })
        })
        .unwrap();
        let before_load = m.lock().published_handle().lock().clone();
        Session::transact(&m, TxMeta::user("load patch"), |tx| {
            tx.apply(Op::PluginSetState { instance: "p-1".into(), state: vec![7u8; 8] })
        })
        .unwrap();
        assert_eq!(m.lock().plugins.state_rev.get("p-1"), Some(&1), "the load bumped it");

        let mut g = m.lock();
        g.restore_from_snapshot(&before_load);
        assert!(
            !g.plugins.pending_state.contains_key("p-1"),
            "the blob really was rolled back — otherwise this test proves nothing"
        );
        assert!(
            g.plugins.state_rev.get("p-1").copied().unwrap_or(0) >= 2,
            "a restore that CHANGES a blob must retire the live node too, got {:?}",
            g.plugins.state_rev.get("p-1")
        );
    }

    #[test]
    fn failed_transact_publishes_nothing() {
        let m = two_clip_session();
        let before = m.lock().published_handle().lock().clone();
        let r = Session::transact(&m, TxMeta::user("boom"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: vec![note(0, 60)] })?;
            Err("boom".into())
        });
        assert!(r.is_err());
        let after = m.lock().published_handle().lock().clone();
        assert!(
            Arc::ptr_eq(&before, &after),
            "the Err/rollback arm must leave the published image untouched"
        );
    }

    #[test]
    fn from_snapshot_and_restore_from_snapshot_round_trip_the_content() {
        let m = two_clip_session();
        let c = Session::transact(&m, TxMeta::user("notes"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: vec![note(0, 60), note(480, 64)] })
        })
        .unwrap();
        let snap = c.snapshot;

        // A scratch session materialized from the image carries the content,
        // with bookkeeping at defaults.
        let scratch = Session::from_snapshot(&snap);
        assert_eq!(scratch.store.tracks, *snap.tracks);
        assert_eq!(scratch.midi.clips.len(), 2);
        assert_eq!(
            scratch.midi.clips.iter().find(|c| c.id == "mc-a").unwrap().notes.len(),
            2
        );
        assert!(!scratch.midi.dirty);
        assert!(scratch.midi.loaded_dir.is_none());
        assert!(scratch.plugins.dirty_state.is_empty());

        // Restoring the LIVE document from an earlier image overwrites the
        // content but keeps the live rev/epoch (the caller decides those).
        let earlier = {
            let g = m.lock();
            g.published_handle().lock().clone()
        };
        Session::transact(&m, TxMeta::user("more notes"), |tx| {
            tx.apply(Op::MidiSetNotes { clip: "mc-a".into(), notes: vec![note(0, 72)] })
        })
        .unwrap();
        {
            let mut g = m.lock();
            g.restore_from_snapshot(&earlier);
            assert_eq!(
                g.midi.clips.iter().find(|c| c.id == "mc-a").unwrap().notes.len(),
                2,
                "content restored from the image"
            );
            // And the restore republished: published matches the restored doc.
            let published = g.published_handle().lock().clone();
            assert_eq!(
                published.midi.clips.iter().find(|c| c.id == "mc-a").unwrap().notes.len(),
                2
            );
        }
    }

    /// Launch maps are document content: a restore must put them back, and
    /// the published image must carry them so a later rebuild sees the same
    /// drive-clip set the live store has.
    #[test]
    fn restore_from_snapshot_round_trips_launch_maps() {
        let m = two_clip_session();
        Session::transact(&m, TxMeta::user("add launcher"), |tx| {
            tx.apply(Op::LaunchMapSet {
                id: "default".into(),
                map: Some(crate::midi::launch::LaunchMap::default_map()),
            })?;
            tx.apply(Op::LaunchBindingSet {
                map_id: "default".into(),
                id: "lb-1".into(),
                binding: Some(crate::midi::launch::LaunchBinding {
                    id: "lb-1".into(),
                    name: "Scene".into(),
                    note: 60,
                    channel: None,
                    target: crate::midi::launch::LaunchTarget::Clip {
                        clip_id: "mc-a".into(),
                    },
                }),
            })?;
            tx.apply(Op::LaunchDriveSet {
                map_id: "default".into(),
                clip_id: "mc-a".into(),
                on: true,
            })
        })
        .unwrap();
        let with_maps = m.lock().published_handle().lock().clone();
        assert_eq!(with_maps.midi.launch_maps.len(), 1);
        assert_eq!(with_maps.midi.launch_maps[0].drive_clip_ids, vec!["mc-a".to_string()]);

        Session::transact(&m, TxMeta::user("clear drive"), |tx| {
            tx.apply(Op::LaunchDriveSet {
                map_id: "default".into(),
                clip_id: "mc-a".into(),
                on: false,
            })
        })
        .unwrap();
        {
            let mut g = m.lock();
            g.restore_from_snapshot(&with_maps);
            assert_eq!(g.midi.launch_maps[0].drive_clip_ids, vec!["mc-a".to_string()]);
            assert_eq!(
                g.published_handle().lock().midi.launch_maps[0].drive_clip_ids,
                vec!["mc-a".to_string()]
            );
        }
    }
}
