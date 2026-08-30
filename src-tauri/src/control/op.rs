//! Op vocabulary and transaction metadata for session mutations.

/// The op wire-format version every journal line carries as `"v"`.
///
/// **2 — state blobs are base64, not JSON number arrays** (whole-branch
/// review, I-5). `serde_json` renders a `Vec<u8>` as `[104,101,108,...]`:
/// roughly a 4x expansion, and the two fields carrying one
/// ([`Op::PluginRemove::state`], [`Op::PluginSetState::state`]) hold real
/// plugin state — a Zynaddsubfx patch is routinely hundreds of kilobytes.
/// Every patch load and every plugin removal therefore wrote a
/// multi-megabyte NDJSON line, synchronously flushed, on the command
/// thread, and kept the same blob in its `HistoryEntry`.
///
/// WHY THIS WAS THE MOMENT, and why the change is not additive-compatible:
/// `OP_FORMAT_VERSION` became load-bearing when Plan E Task 17 wrote the
/// first journal line, but the journal is still WRITE-ONLY — nothing reads
/// it back until Plan F gives it a reader. So the entire population of
/// version-1 data is logs no code will ever parse, and the correct move is
/// the clean one: change the encoding outright and bump the version, rather
/// than carry a dual-shape reader forever for data that has no readers.
/// That window closes permanently the moment Plan F ships a replayer.
/// After that, a change of this kind costs a migration.
///
/// CORRECTION (ADR 0007, Plan F Task 9): the window IS CLOSED. The paragraph
/// above is kept as the record of why version 2 shipped without a dual-shape
/// reader, but `control::replay` now reads this format back, and its reader
/// gates on this constant: a batch line at any other version is skipped and
/// counted, never guessed at. From here, a breaking change to any `Op`
/// variant's wire form needs a bump AND a reader that understands both
/// shapes.
///
/// The same bump covers L-1 (`Op::PluginRemove::params` is now actually
/// read on apply) — two format edits under one version, while the format
/// is one day old, instead of two migrations later.
///
/// Additive fields with `#[serde(default)]` remain non-breaking and do NOT
/// need a bump (`clip_indices`, `transient`, `params` all arrived that way
/// at version 1). Anything else does, plus a reader that understands both
/// shapes.
pub const OP_FORMAT_VERSION: u16 = 2;

/// `#[serde(with = "b64")]` for a `Vec<u8>` field: standard base64 with
/// padding on the wire, bytes in memory. See [`OP_FORMAT_VERSION`].
pub(crate) mod b64 {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// The same, for an `Option<Vec<u8>>`: `null` stays `null` (an instance
/// with no host state to save), `Some` becomes a base64 string.
pub(crate) mod b64_opt {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match bytes {
            Some(b) => s.serialize_str(&STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        let s = Option::<String>::deserialize(d)?;
        match s {
            Some(s) => STANDARD
                .decode(s.as_bytes())
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Op {
    /// Property-addressed change (round-2 §4: Set { object, path, from, to }).
    Set { object: ObjectRef, path: PropPath, from: serde_json::Value, to: serde_json::Value },
    /// Structural: create a track (payload = full row, so inverse is Remove).
    /// `clips` are the track's clips to (re)insert alongside the row — a
    /// fresh `add_track` call passes an empty vec; an undo of a
    /// `TrackRemove` carries back exactly the clips that were removed with
    /// it (clips are document state, not effect-layer bookkeeping — round-2
    /// fix, controller ruling: a track op must carry its own clips so an
    /// undo never resurrects an empty track). `clip_indices` are the
    /// pre-removal positions the clips held in `store.clips`, in ascending
    /// order — when its length matches `clips.len()`, `apply_raw` reinserts
    /// each clip at its recorded index instead of appending, restoring the
    /// original interleaving byte-identically (Plan A carry-forward).
    /// `#[serde(default)]` so pre-Plan-B payloads without the field still
    /// deserialize (OP_FORMAT_VERSION stays 1).
    TrackAdd {
        track: crate::audio::types::TrackState,
        index: usize,
        clips: Vec<crate::audio::types::Clip>,
        #[serde(default)]
        clip_indices: Vec<usize>,
        /// Automation clips restored with this row (undo of TrackRemove on
        /// an automation track). Fresh `add_track` leaves this empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        automation_clips: Vec<crate::modulation::AutomationClip>,
        /// Bindings sourced from this automation track, same restore path.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bindings: Vec<crate::modulation::Binding>,
    },
    /// Structural: remove a track. Payload kept so the inverse can restore
    /// identity + row byte-identically (gate test 2 arrives in Plan B).
    /// `clips` is advisory (like `index`) — `apply_raw` collects the track's
    /// actual clips from store truth, ignoring the caller's value here.
    /// `clip_indices` is likewise advisory — store truth (computed by
    /// `apply_raw` from the removal) wins.
    TrackRemove {
        track: crate::audio::types::TrackState,
        index: usize,
        clips: Vec<crate::audio::types::Clip>,
        #[serde(default)]
        clip_indices: Vec<usize>,
    },
    /// Structural: insert an audio clip (payload = full row; inverse =
    /// ClipRemove) — Plan E Task 3 (round-2 inventory row 3, "most common
    /// editing gesture reaches no backend channel at all").
    ClipAdd { clip: crate::audio::types::Clip, index: usize },
    /// Structural: remove an audio clip; `clip`/`index` advisory beyond
    /// `clip.id` — store truth wins, mirroring `TrackRemove`.
    ClipRemove { clip: crate::audio::types::Clip, index: usize },
    /// Plan E Task 5: atomic tempo/meter replacement, including the
    /// `transport.tempo_bpm` mirror (= first event's bpm), which today is a
    /// second non-atomic phase (round-2 inventory row 4). Inverse carries
    /// the previous triple.
    TempoSet {
        ppq: u32,
        events: Vec<crate::midi::types::TempoEvent>,
        meter: Vec<crate::midi::types::MeterEvent>,
    },
    /// Structural: insert a MIDI clip (payload = full row incl. content_id/
    /// lane_id/next_note_id; inverse = MidiClipRemove). Duplicate-id guard
    /// like ClipAdd. Restoring a removed clip's row restores its watermark
    /// — correct: that watermark was the clip's own (ADR 0001).
    MidiClipAdd { clip: crate::midi::types::MidiClip, index: usize },
    /// Structural: remove a MIDI clip; `clip`/`index` advisory beyond
    /// `clip.id` — store truth wins, mirroring `ClipRemove`.
    MidiClipRemove { clip: crate::midi::types::MidiClip, index: usize },
    /// §4.4 value-replacement wrapper form: whole-notes replace, diffed
    /// against store truth server-side, coalescable by (kind, object,
    /// actor). `notes` may carry noteId 0 (mint) or live ids (keep) —
    /// `assign_incoming_note_ids`'s keep-rule applies inside `apply_raw`.
    /// Inverse = MidiSetNotes with the previous notes (all live ids). The
    /// watermark advances monotonically and is NOT rewound by the inverse
    /// (scope ruling 3, ADR 0001).
    MidiSetNotes { clip: crate::ids::ClipId, notes: Vec<crate::midi::types::MidiNote> },
    /// §4.4 value-replacement wrapper (Plan E Task 10): the automation lane
    /// analogue of `MidiSetNotes` — one op replaces (or deletes) ONE lane's
    /// full point set; an edit gesture is one lane write, never one op per
    /// point (D-03). `key` addresses the lane by its `AutomationLane::id` —
    /// a plain string, not a struct key, so no new `ObjectRef` variant is
    /// needed. `lane: None` deletes the lane at `key` (a no-op, not an
    /// error, if it doesn't exist); `Some` upserts (replaces in place if
    /// `key` already names a lane, else inserts). Inverse carries the
    /// previous lane at `key` (or `None` if it didn't exist before this op).
    AutomationSetLane { key: String, lane: Option<crate::plugins::automation::AutomationLane> },
    /// Track F: upsert/delete one curve in `session.modulation` by `key`
    /// (`Curve::id`). Same §4.4 value-replacement shape as
    /// `AutomationSetLane` — `None` deletes (no-op if absent); `Some`
    /// upserts. Inverse carries previous store truth. Additive; does NOT
    /// bump `OP_FORMAT_VERSION`.
    ModulationSetCurve { key: String, curve: Option<crate::modulation::Curve> },
    /// Track F: upsert/delete one binding in `session.modulation` by `key`
    /// (`Binding::id`). Invalid bindings are rejected before any mutation.
    ModulationSetBinding { key: String, binding: Option<crate::modulation::Binding> },
    /// Track F: upsert/delete one automation-track clip by `key`
    /// (`AutomationClip::id`).
    AutomationClipSet { key: String, clip: Option<crate::modulation::AutomationClip> },
    /// Plan E Task 9 (round-2 inventory rows 12-15): register a plugin
    /// instance row in the document. Prepare-outside: the host instance
    /// already exists (instantiation round-trip BEFORE the tx, so a plugin
    /// the host rejects never reaches the document) — this op only
    /// registers the row. Also used as `PluginRemove`'s computed inverse
    /// (undo of a remove): in that case the host instance is gone (the
    /// forward remove destroyed it), so `apply_raw` always folds in the
    /// `host_forward` effect this case needs (`Instantiate` + `LoadState`
    /// when a state blob is on file) — a redundant-but-safe re-derive for
    /// the prepare-outside path too (`commit`'s executor skips the actual
    /// host call when the id is already live, see its doc).
    PluginAdd { row: crate::plugins::PluginInstanceInfo, index: usize },
    /// Restore-from-blob (§4.4): `state` is the APST-encoded blob captured
    /// via the host's `save_state` BEFORE teardown (the same self-describing
    /// bytes `plugins::state` writes to `<project>/plugins/<id>.state` —
    /// `None` when the instance had no host state to save, e.g. a stub or a
    /// plugin without a state interface). The inverse is `PluginAdd` with
    /// the row + this blob parked in `pending_state`; undo re-instantiates
    /// through the host state machine (`host_forward`, executed after the
    /// lock).
    ///
    /// `params` (additive, `#[serde(default)]`) is the row's param mirror
    /// at removal time — self-describing for a journaled/replayed op the
    /// same way `TrackRemove`'s `clips` is (Task 9 review round 1,
    /// Important-1). `Op::PluginAdd`'s shape is fixed at `{row, index}` (no
    /// params slot), so undo can't carry the mirror through the inverse op
    /// itself; `apply_raw`'s `PluginRemove` arm instead PARKS the mirror in
    /// `session.plugins.params` (never clearing it on removal, mirroring
    /// `pending_state`'s same "survive the row's absence" treatment) so a
    /// later `PluginAdd` for the SAME id (undo) finds it still there. A
    /// genuinely permanent removal (never undone) leaves a small, bounded
    /// orphaned entry — acceptable, GC'd wholesale on the next
    /// `restore_into_session`/project open.
    ///
    /// L-1 CLOSED (whole-branch review): the parked mirror is an IN-MEMORY
    /// fact, so a COLD REPLAY — a fresh process applying journal lines,
    /// with nothing parked anywhere — used to restore the row without its
    /// params, and this field was captured but never read. `apply_raw` now
    /// SEEDS the mirror from this field when the in-memory one is absent or
    /// empty, which makes the op self-describing in fact and not just in
    /// intent. In-process nothing changes: the parked mirror is already
    /// there and wins.
    PluginRemove {
        row: crate::plugins::PluginInstanceInfo,
        index: usize,
        /// Base64 on the wire from `OP_FORMAT_VERSION` 2 (I-5) — see that
        /// constant's doc for why a plain `Vec<u8>` was not viable here.
        #[serde(with = "b64_opt")]
        state: Option<Vec<u8>>,
        #[serde(default)]
        params: Vec<crate::plugins::ParamInfo>,
    },
    /// Full-state write (zyn patch loads): `state` is the APST-encoded blob
    /// AFTER the host load (the row's new pending_state truth); the inverse
    /// carries the PREVIOUS blob (captured via host `save_state` before the
    /// load) so the patch load is durable AND undoable (closes round-2
    /// inventory row 14).
    PluginSetState {
        instance: String,
        /// Base64 on the wire from `OP_FORMAT_VERSION` 2 (I-5) — this is
        /// the field that made the old encoding expensive: every
        /// `zyn_load_patch` wrote its whole patch, number by JSON number,
        /// into the journal.
        #[serde(with = "b64")]
        state: Vec<u8>,
    },
    /// Upsert/delete one MIDI launch binding by `id`. `None` deletes
    /// (no-op if absent). Additive; does not bump `OP_FORMAT_VERSION`.
    LaunchBindingSet {
        #[serde(default)]
        map_id: String,
        id: String,
        binding: Option<crate::midi::launch::LaunchBinding>,
    },
    /// Plan H1 (ruling H-4): atomic replacement of the Composer's harmony
    /// document — `Op::TempoSet`'s shape, and for the same reason. A chord
    /// region and the key it is analysed in are ONE edit: writing them
    /// separately would let a commit land a ♭VII in a key that has not
    /// arrived yet, and would make the inverse of a modulation two ops that
    /// can be undone apart. Region counts are tens, so whole-list replacement
    /// is right; a keyed upsert would buy nothing and fight folding.
    ///
    /// Additive: does NOT bump `OP_FORMAT_VERSION` (stays 2). Keys and chords
    /// ride the wire as strings (`"C ionian"`, `"Cmaj7"`) — see
    /// `theory::harmony` for why a persisted format spells them out.
    HarmonySet {
        keys: Vec<crate::theory::KeySpan>,
        chords: Vec<crate::theory::ChordSpan>,
    },
    /// Toggle whether a MIDI clip's notes drive a named launcher.
    LaunchDriveSet {
        #[serde(default)]
        map_id: String,
        clip_id: String,
        on: bool,
    },
    /// Upsert/delete one named launcher (`None` deletes).
    LaunchMapSet {
        id: String,
        map: Option<crate::midi::launch::LaunchMap>,
    },
    /// Plan G1: append/insert one insert-FX slot on a track. Inverse is
    /// `InsertRemove` with store-truth index. Additive; does not bump
    /// `OP_FORMAT_VERSION`.
    InsertAdd {
        track_id: crate::ids::TrackId,
        slot: crate::audio::types::InsertSlot,
        index: usize,
    },
    /// Plan G1: remove one insert-FX slot by `slot.id` (store truth wins
    /// over advisory `index`). Inverse is `InsertAdd` at the store-truth
    /// position.
    InsertRemove {
        track_id: crate::ids::TrackId,
        slot: crate::audio::types::InsertSlot,
        index: usize,
    },
    /// Plan G1: move one slot on a track. `from` is advisory; store truth
    /// is the current position of `slot_id`. Inverse swaps the applied
    /// `from`/`to` so undo restores order.
    InsertReorder {
        track_id: crate::ids::TrackId,
        slot_id: String,
        from: usize,
        to: usize,
    },
    /// Plan G1: set a slot's bypass flag (rebuild in G1 — G-5). Inverse
    /// carries the previous store-truth bool.
    InsertSetBypass {
        track_id: crate::ids::TrackId,
        slot_id: String,
        bypassed: bool,
    },
    /// Plan G2: append/insert one send edge on a track. Inverse is
    /// `SendRemove` with the store-truth index. Additive; does not bump
    /// `OP_FORMAT_VERSION`.
    SendAdd {
        track_id: crate::ids::TrackId,
        slot: crate::audio::types::SendSlot,
        index: usize,
    },
    /// Plan G2: remove one send edge by `slot.id` (store truth wins over
    /// advisory `index`). Inverse is `SendAdd` at the store-truth position.
    SendRemove {
        track_id: crate::ids::TrackId,
        slot: crate::audio::types::SendSlot,
        index: usize,
    },
    /// Plan G2: set a send's amount. A MIX change, not a structural one —
    /// it resolves to an atomic store in `ParamTable::send_amount` and
    /// schedules NO rebuild (§10). Inverse carries the previous dB value.
    SendSetAmount {
        track_id: crate::ids::TrackId,
        send_id: String,
        amount_db: f64,
    },
    /// Plan G2: point a track's OUTPUT at a bus, or back at the master
    /// (`None`). Structural — it moves an edge in the graph. Inverse
    /// carries the previous destination.
    ///
    /// This is a MOVE, not a copy: unlike `SendAdd`, the track stops
    /// reaching the master. See `audio::bus`'s module doc for why the two
    /// are separate ops rather than one with a flag.
    TrackSetOutput {
        track_id: crate::ids::TrackId,
        output: Option<crate::ids::TrackId>,
    },
    /// Plan G2: move a send's tap between post-fader (default) and
    /// pre-fader. STRUCTURAL — it changes where the wire leaves the strip —
    /// so unlike the amount it rebuilds. Inverse carries the previous bool.
    SendSetPreFader {
        track_id: crate::ids::TrackId,
        send_id: String,
        pre_fader: bool,
    },
    /// Lanes UX: reorder the whole track list in one op. `order` is the
    /// complete new display order and MUST be a permutation of the store's
    /// current track ids — `apply_raw` rejects anything else rather than
    /// dropping or duplicating rows, because track order is not cosmetic:
    /// `audio::types::derive_slots` numbers the RT mixer slots FROM display
    /// order, so a partial order would silently re-point live param slots.
    ///
    /// Whole-list (not `{from, to}`) on purpose: it is idempotent, it is
    /// its own inverse shape (the previous order), and a drag that moves a
    /// track into a group can also renormalize group contiguity in the same
    /// commit — one undo step for what the user experienced as one drag.
    ///
    /// Structural: sets `rebuild`, so the next `GraphTables` renumbers
    /// slots. Additive; does not bump `OP_FORMAT_VERSION`.
    TrackReorder { order: Vec<crate::ids::TrackId> },
    /// Structural: create a player (payload = the full row, so the inverse
    /// is `PlayerRemove`). Plan V, ruling V-1. Additive; does not bump
    /// `OP_FORMAT_VERSION`.
    PlayerAdd { player: crate::audio::player::Player, index: usize },
    /// Structural: remove a player. `player` is advisory beyond `player.id`
    /// and `index` is advisory — store truth wins, mirroring `TrackRemove`.
    PlayerRemove { player: crate::audio::player::Player, index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "family", content = "id", rename_all = "camelCase")]
pub enum ObjectRef {
    Track(crate::ids::TrackId),
    Clip(crate::ids::ClipId),
    MidiClip(crate::ids::ClipId),
    /// Plan E Task 12: the transport is a singleton, not an id-addressed
    /// row — a unit variant. Adjacently-tagged serde omits the `id` field
    /// entirely for a unit variant, so the wire form is exactly
    /// `{"family":"transport"}` (no stray `"id":null`).
    Transport,
    /// Plugin instance id (`plugins::PluginInstanceInfo::id`).
    Plugin(String),
    /// A Plan V player (V-1). Its own family, not `Track`: a player is not
    /// a track (V-2), and `ObjectRef` is what disambiguates which document
    /// list a `PropPath` addresses.
    Player(crate::ids::PlayerId),
}

/// Property paths are a closed enum, not strings — renaming a variant is a
/// compile error at every use site, which is the §4.6 anti-drift rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PropPath {
    Gain, Pan, Muted, Soloed, Armed, AutomationMode,
    /// Track: sampler-bank id or `plugin:<instanceId>`, `None` = unbound.
    /// Wire form: JSON string or `null` (not an `Option`-shaped object).
    InstrumentId,
    /// MidiClip **or** Track: display name (wire: JSON string). Trimmed on
    /// write and rejected when empty — a nameless row is unaddressable in
    /// the UI, and the write side is the validation authority so the
    /// inverse observes the post-trim value (mirroring `LengthTicks`'s
    /// clamp rule). The Track arm is the lanes-UX rename path; it reuses
    /// this variant rather than minting a `TrackName` precisely because
    /// `ObjectRef` already disambiguates which row is being named.
    Name,
    /// Track: lane-group membership — the group's display NAME, or `null`
    /// for ungrouped (wire: JSON string or `null`, like `InstrumentId`).
    /// Trimmed on write; an empty string normalizes to `null` so
    /// "ungrouped" has exactly one representation in the store. Purely a
    /// display grouping — see `TrackState::group`.
    Group,
    /// Clip: timeline placement position, in samples.
    TimelineStartSamples,
    /// Clip: placement length, in samples (Plan E Task 8 — LoopJam's
    /// region-replacement trims need to shrink/reposition an EXISTING
    /// clip row in place, the audio counterpart of MidiClip's
    /// `LengthTicks`).
    LengthSamples,
    /// Clip: offset into the source audio, in samples (Plan E Task 8 —
    /// paired with `LengthSamples`/`TimelineStartSamples` so a clip whose
    /// HEAD is cut can be repositioned in place instead of a
    /// remove-then-fresh-id-add).
    OffsetSamples,
    /// MidiClip: timeline placement position, in ticks.
    TimelineStartTicks,
    /// MidiClip: placement length, in ticks. Clamped to >= 1 on write (the
    /// write side is the clamp authority, mirroring the gain-clamp
    /// round-trip rule — Plan A) so the inverse observes the post-clamp
    /// value, never the caller's raw (possibly zero) `to`.
    LengthTicks,
    /// MidiClip: content (loop/native) length in ticks — ADR 0004's content
    /// half of the content/placement split. Wire form is a JSON number or
    /// `null` (mirrors `MidiClip::content_length_ticks: Option<u64>`);
    /// `null` clears back to "same as `LengthTicks`".
    ContentLengthTicks,
    /// MidiClip: placement transpose in semitones (wire: JSON integer).
    /// Plan F Task 12 — the RESULTS.md 5.4× lever: transpose-class
    /// gestures MUST address this field, never rewrite notes.
    TransposeSemitones,
    /// MidiClip: placement velocity offset (wire: JSON integer). Same
    /// routing rule as `TransposeSemitones`.
    VelocityOffset,
    /// Transport: `"playing"|"stopped"|"recording"` (wire: JSON string).
    /// Plan E Task 12 (inventory rows 28-29): the property-addressed mirror
    /// of `ControlPlane::transport`'s direct `session.store.transport.state`
    /// writes.
    TransportState,
    /// Transport: loop-region enabled flag (wire: JSON bool).
    LoopEnabled,
    /// Transport: loop-region start, in samples (wire: JSON non-negative
    /// integer).
    LoopStartSamples,
    /// Transport: loop-region end, in samples (wire: JSON non-negative
    /// integer).
    LoopEndSamples,
    /// Transport: stop-at-end policy flag (wire: JSON bool).
    StopAtEnd,
    /// Transport: engine sample rate (wire: JSON non-negative integer,
    /// `u32`).
    SampleRate,
    /// Plugin: one parameter, addressed by its stable per-plugin id (CLAP
    /// param id / LV2 control-port index — the same id
    /// `plugins::ParamChange::id` carries). Wire form: `to`/`from` are JSON
    /// numbers (the param's value). Coalescable (§4.4): a knob drag folds
    /// to net `Set`s per (instance, index), same as `Gain`.
    Param { index: u32 },
    /// Player: V-6's absolute raw flag (wire: JSON bool).
    Raw,
    /// Player: `"oneShot" | "gate" | "loop"` (wire: JSON string).
    TriggerMode,
    /// Player: `"off" | "sixteenth" | "eighth" | "quarter" | "whole" |
    /// "bar"` (wire: JSON string). V3.
    Quantize,
    /// Player: the choke group, `0..=255` or JSON `null` for none. V3.
    ChokeGroup,
    /// Player: velocity-to-gain depth, `0.0..=1.0` (wire: JSON number). V3.
    VelocityToGain,
    /// Player: the whole tagged `PlayerSource` object. One path rather than
    /// three, because changing a pad's source changes `kind` and its fields
    /// together — two ops could leave a `midiClip` source carrying an
    /// `audioClip`'s id between them.
    PlayerSource,
}

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
    /// RT/document-visible, but excluded from history and journal (round-2
    /// §4.4) — transport/gesture mid-flight commits. Additive field:
    /// `#[serde(default)]` so pre-existing payloads without it still
    /// deserialize, defaulting to `false`.
    #[serde(default)]
    pub transient: bool,
}

impl TxMeta {
    /// A fresh correlation id + `label`, attributed to a human through the
    /// Tauri IPC surface. Every migrated A-slice command builds its own
    /// `TxMeta` at the call site (Task 7) rather than defaulting one deep
    /// inside `ControlPlane`, so the actor is always an explicit choice.
    pub fn user(label: impl Into<String>) -> Self {
        Self { actor: Actor::User, run: uuid::Uuid::new_v4().to_string(), label: label.into(), transient: false }
    }

    /// Same, attributed to the named MCP tool call.
    pub fn agent(tool: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            actor: Actor::Agent { tool: tool.into() },
            run: uuid::Uuid::new_v4().to_string(),
            label: label.into(),
            transient: false,
        }
    }

    /// Same, attributed to an automated system process (e.g. a sidecar
    /// job's post-processing hook, not a direct user/agent request).
    pub fn system(label: impl Into<String>) -> Self {
        Self { actor: Actor::System, run: uuid::Uuid::new_v4().to_string(), label: label.into(), transient: false }
    }

    /// Same, attributed to the engine/RT side itself (e.g. an auto-stop at
    /// the end of material) — today this is only ever built as a literal
    /// struct in tests; this constructor gives non-test callers the same
    /// explicit-actor discipline `user`/`agent`/`system` already have.
    pub fn engine(label: impl Into<String>) -> Self {
        Self { actor: Actor::Engine, run: uuid::Uuid::new_v4().to_string(), label: label.into(), transient: false }
    }

    /// Builder: mark this transaction transient — RT/document-visible, but
    /// excluded from history and journal (round-2 §4.4).
    pub fn transient(mut self) -> Self {
        self.transient = true;
        self
    }
}

/// Shared by `control::mod`'s and `control::session`'s test modules.
#[cfg(test)]
pub(crate) mod testutil {
    use super::{ObjectRef, Op, PropPath};

    /// `from` is intentionally `Null`: it's advisory only, `apply_raw`
    /// re-reads truth from the store and ignores the caller's `from`.
    pub fn set_gain(track_id: &str, to: f64) -> Op {
        Op::Set {
            object: ObjectRef::Track(track_id.into()),
            path: PropPath::Gain,
            from: serde_json::Value::Null,
            to: serde_json::json!(to),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::AutomationMode;

    #[test]
    fn op_serde_round_trips_and_carries_version() {
        assert_eq!(OP_FORMAT_VERSION, 2, "I-5: base64 state blobs bumped the wire format");
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

        // TrackAdd with real TrackState: verify wire form and round-trip.
        let track = crate::audio::types::TrackState {
            sends: Vec::new(),
            output: None,
            id: "t-2".into(),
            name: "Audio Track".into(),
            kind: "audio".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
            inserts: Vec::new(),
            group: None,
            automation_mode: AutomationMode::Read,
        };
        let clip = crate::audio::types::Clip {
            id: "c-1".into(),
            track_id: "t-2".into(),
            name: "clip".into(),
            source_path: "audio/c-1.wav".into(),
            source_id: crate::ids::SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 48_000,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 48_000,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t-2"),
        };
        let track_add = Op::TrackAdd {
            track: track.clone(), index: 0, clips: vec![clip.clone()], clip_indices: vec![0],
            automation_clips: vec![], bindings: vec![],
        };
        let s = serde_json::to_string(&track_add).unwrap();
        // Print once to verify camelCase tag form: should be "trackAdd", and
        // that `clips` is a true array on the wire (not dropped/renamed).
        eprintln!("TrackAdd wire form: {}", s);
        assert!(s.contains("\"kind\":\"trackAdd\""), "wire form was: {s}");
        assert!(s.contains("\"clips\":[{"), "clips must serialize as an array of objects, was: {s}");
        assert!(s.contains("\"id\":\"c-1\""), "clip fields must be present, was: {s}");
        assert!(s.contains("\"clipIndices\":[0]"), "clip_indices must serialize camelCase, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, track_add);

        // TrackRemove with real TrackState: verify wire form and round-trip.
        let track_remove = Op::TrackRemove {
            track, index: 0, clips: vec![clip], clip_indices: vec![0],
        };
        let s = serde_json::to_string(&track_remove).unwrap();
        // Print once to verify camelCase tag form: should be "trackRemove"
        eprintln!("TrackRemove wire form: {}", s);
        assert!(s.contains("\"kind\":\"trackRemove\""), "wire form was: {s}");
        assert!(s.contains("\"clips\":[{"), "clips must serialize as an array of objects, was: {s}");
        assert!(s.contains("\"clipIndices\":[0]"), "clip_indices must serialize camelCase, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, track_remove);

        // Compat: a legacy payload without `clipIndices` still deserializes
        // (additive field, `#[serde(default)]`, OP_FORMAT_VERSION stays 1).
        let legacy = r##"{"kind":"trackRemove","track":{"id":"t-2","name":"Audio Track","kind":"audio","gainDb":0.0,"pan":0.0,"muted":false,"soloed":false,"armed":false,"color":"#7c9cff"},"index":0,"clips":[]}"##;
        let parsed: Op = serde_json::from_str(legacy).expect("legacy payload without clipIndices parses");
        assert!(matches!(parsed, Op::TrackRemove { ref clip_indices, .. } if clip_indices.is_empty()));

        // Plan E Task 3: ClipAdd/ClipRemove wire form, and the new
        // TimelineStartSamples path on a Set.
        let clip2 = crate::audio::types::Clip {
            id: "c-2".into(),
            track_id: "t-2".into(),
            name: "clip2".into(),
            source_path: "audio/c-2.wav".into(),
            source_id: crate::ids::SourceId::default(),
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 48_000,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 48_000,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t-2"),
        };
        let clip_add = Op::ClipAdd { clip: clip2.clone(), index: 0 };
        let s = serde_json::to_string(&clip_add).unwrap();
        eprintln!("ClipAdd wire form: {}", s);
        assert!(s.contains("\"kind\":\"clipAdd\""), "wire form was: {s}");
        assert!(s.contains("\"id\":\"c-2\""), "clip fields must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, clip_add);

        let clip_remove = Op::ClipRemove { clip: clip2.clone(), index: 0 };
        let s = serde_json::to_string(&clip_remove).unwrap();
        eprintln!("ClipRemove wire form: {}", s);
        assert!(s.contains("\"kind\":\"clipRemove\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, clip_remove);

        let move_op = Op::Set {
            object: ObjectRef::Clip("c-2".into()),
            path: PropPath::TimelineStartSamples,
            from: serde_json::json!(0u64),
            to: serde_json::json!(48_000u64),
        };
        let s = serde_json::to_string(&move_op).unwrap();
        eprintln!("Set{{Clip,TimelineStartSamples}} wire form: {}", s);
        assert!(s.contains("\"path\":\"timelineStartSamples\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, move_op);

        // Plan E Task 8: LengthSamples/OffsetSamples camelCase wire names
        // (LoopJam's in-place region trims).
        for (path, expected) in
            [(PropPath::LengthSamples, "lengthSamples"), (PropPath::OffsetSamples, "offsetSamples")]
        {
            let s = serde_json::to_string(&path).unwrap();
            assert_eq!(s, format!("\"{expected}\""), "wire form was: {s}");
            let back: PropPath = serde_json::from_str(&s).unwrap();
            assert_eq!(back, path);
        }

        // Plan E Task 5: TempoSet, MidiClipAdd/MidiSetNotes wire form, and
        // the new ContentLengthTicks path on a Set.
        let tempo_set = Op::TempoSet {
            ppq: 960,
            events: vec![crate::midi::types::TempoEvent { tick: 0, bpm: 140.0 }],
            meter: vec![crate::midi::types::MeterEvent { tick: 0, num: 3, den: 4 }],
        };
        let s = serde_json::to_string(&tempo_set).unwrap();
        eprintln!("TempoSet wire form: {}", s);
        assert!(s.contains("\"kind\":\"tempoSet\""), "wire form was: {s}");
        assert!(s.contains("\"bpm\":140.0"), "tempo events must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, tempo_set);

        // Plan H1: `HarmonySet`'s wire form is a PERSISTED format (it rides
        // journal.ndjson), and its whole design bet is that keys and chords
        // ride as spelled strings — so the round trip has to preserve a
        // spelling that a pitch-class encoding would have destroyed.
        let harmony_set = Op::HarmonySet {
            keys: vec![crate::theory::KeySpan {
                tick: 0,
                key: crate::theory::Key::parse("Gb major").unwrap(),
            }],
            chords: vec![crate::theory::ChordSpan::new(
                0,
                3840,
                crate::theory::Chord::parse("Cbmaj7").unwrap(),
            )],
        };
        let s = serde_json::to_string(&harmony_set).unwrap();
        eprintln!("HarmonySet wire form: {}", s);
        assert!(s.contains("\"kind\":\"harmonySet\""), "wire form was: {s}");
        assert!(s.contains("\"Gb ionian\""), "keys spell themselves: {s}");
        assert!(s.contains("\"Cbmaj7\""), "…and so do chords, Cb not B: {s}");
        assert!(s.contains("\"lengthTicks\":3840"), "camelCase on the wire: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, harmony_set);

        let midi_clip = crate::midi::types::MidiClip {
            id: "mc-1".into(),
            track_id: "t-2".into(),
            name: "midi clip".into(),
            timeline_start_ticks: 0,
            length_ticks: 3840,
            notes: vec![],
            next_note_id: 1,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("t-2"),
            content_length_ticks: None,
            transpose_semitones: 0,
            velocity_offset: 0,
        };
        let midi_clip_add = Op::MidiClipAdd { clip: midi_clip.clone(), index: 0 };
        let s = serde_json::to_string(&midi_clip_add).unwrap();
        eprintln!("MidiClipAdd wire form: {}", s);
        assert!(s.contains("\"kind\":\"midiClipAdd\""), "wire form was: {s}");
        assert!(s.contains("\"id\":\"mc-1\""), "clip fields must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, midi_clip_add);

        let midi_clip_remove = Op::MidiClipRemove { clip: midi_clip, index: 0 };
        let s = serde_json::to_string(&midi_clip_remove).unwrap();
        eprintln!("MidiClipRemove wire form: {}", s);
        assert!(s.contains("\"kind\":\"midiClipRemove\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, midi_clip_remove);

        let midi_set_notes = Op::MidiSetNotes {
            clip: "mc-1".into(),
            notes: vec![crate::midi::types::MidiNote {
                tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0,
                note_id: crate::ids::NoteId(1),
            }],
        };
        let s = serde_json::to_string(&midi_set_notes).unwrap();
        eprintln!("MidiSetNotes wire form: {}", s);
        assert!(s.contains("\"kind\":\"midiSetNotes\""), "wire form was: {s}");
        assert!(s.contains("\"noteId\":1"), "note fields must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, midi_set_notes);

        let content_length_op = Op::Set {
            object: ObjectRef::MidiClip("mc-1".into()),
            path: PropPath::ContentLengthTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(1920u64),
        };
        let s = serde_json::to_string(&content_length_op).unwrap();
        eprintln!("Set{{MidiClip,ContentLengthTicks}} wire form: {}", s);
        assert!(s.contains("\"path\":\"contentLengthTicks\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, content_length_op);

        // Plan E Task 12: ObjectRef::Transport is a unit variant — the
        // adjacently-tagged wire form must be exactly `{"family":"transport"}`,
        // with no stray `"id"` key (unlike Track/Clip/MidiClip, which carry one).
        let s = serde_json::to_string(&ObjectRef::Transport).unwrap();
        eprintln!("ObjectRef::Transport wire form: {}", s);
        assert_eq!(s, r#"{"family":"transport"}"#, "wire form was: {s}");
        let back: ObjectRef = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ObjectRef::Transport);

        // Set{Transport, TransportState} round-trips, including the new
        // transport PropPath variants' camelCase wire names.
        let transport_state_op = Op::Set {
            object: ObjectRef::Transport,
            path: PropPath::TransportState,
            from: serde_json::json!("stopped"),
            to: serde_json::json!("playing"),
        };
        let s = serde_json::to_string(&transport_state_op).unwrap();
        eprintln!("Set{{Transport,TransportState}} wire form: {}", s);
        assert!(s.contains("\"family\":\"transport\""), "wire form was: {s}");
        assert!(s.contains("\"path\":\"transportState\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, transport_state_op);

        // The remaining transport paths' camelCase wire names.
        for (path, expected) in [
            (PropPath::LoopEnabled, "loopEnabled"),
            (PropPath::LoopStartSamples, "loopStartSamples"),
            (PropPath::LoopEndSamples, "loopEndSamples"),
            (PropPath::StopAtEnd, "stopAtEnd"),
            (PropPath::SampleRate, "sampleRate"),
        ] {
            let s = serde_json::to_string(&path).unwrap();
            assert_eq!(s, format!("\"{expected}\""), "wire form was: {s}");
            let back: PropPath = serde_json::from_str(&s).unwrap();
            assert_eq!(back, path);
        }
        // Plan E Task 10: AutomationSetLane wire form, both a lane payload
        // (upsert) and `lane: null` (delete).
        let lane = crate::plugins::automation::AutomationLane {
            id: "a-1".into(),
            target_node: "track:t-1".into(),
            param_id: 0,
            points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
        };
        let automation_set = Op::AutomationSetLane { key: "a-1".into(), lane: Some(lane.clone()) };
        let s = serde_json::to_string(&automation_set).unwrap();
        eprintln!("AutomationSetLane wire form: {}", s);
        assert!(s.contains("\"kind\":\"automationSetLane\""), "wire form was: {s}");
        assert!(s.contains("\"targetNode\":\"track:t-1\""), "lane fields must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, automation_set);

        let automation_delete = Op::AutomationSetLane { key: "a-1".into(), lane: None };
        let s = serde_json::to_string(&automation_delete).unwrap();
        eprintln!("AutomationSetLane (delete) wire form: {}", s);
        assert!(s.contains("\"lane\":null"), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, automation_delete);

        // Track F: modulation op wire forms (additive; OP_FORMAT_VERSION stays 2).
        let curve = crate::modulation::Curve {
            id: "cur-1".into(),
            name: "fade".into(),
            length_ticks: None,
            points: vec![crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 }],
        };
        let set_curve = Op::ModulationSetCurve { key: "cur-1".into(), curve: Some(curve.clone()) };
        let s = serde_json::to_string(&set_curve).unwrap();
        eprintln!("ModulationSetCurve wire form: {}", s);
        assert!(s.contains("\"kind\":\"modulationSetCurve\""), "wire form was: {s}");
        assert!(s.contains("\"points\""), "curve points must be present, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, set_curve);

        let set_binding = Op::ModulationSetBinding {
            key: "bnd-1".into(),
            binding: Some(crate::modulation::Binding {
                id: "bnd-1".into(),
                source: crate::modulation::Source::Curve { curve_id: "cur-1".into() },
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
            }),
        };
        let s = serde_json::to_string(&set_binding).unwrap();
        eprintln!("ModulationSetBinding wire form: {}", s);
        assert!(s.contains("\"kind\":\"modulationSetBinding\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, set_binding);

        let clip_set = Op::AutomationClipSet {
            key: "acl-1".into(),
            clip: Some(crate::modulation::AutomationClip {
                id: "acl-1".into(),
                track_id: "t-auto".into(),
                curve_id: "cur-1".into(),
                timeline_start_ticks: 0,
                length_ticks: 3840,
                content_length_ticks: None,
            }),
        };
        let s = serde_json::to_string(&clip_set).unwrap();
        eprintln!("AutomationClipSet wire form: {}", s);
        assert!(s.contains("\"kind\":\"automationClipSet\""), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, clip_set);

        let curve_delete = Op::ModulationSetCurve { key: "cur-1".into(), curve: None };
        let s = serde_json::to_string(&curve_delete).unwrap();
        assert!(s.contains("\"curve\":null"), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, curve_delete);
    }

    /// I-5 (whole-branch review), the reason `OP_FORMAT_VERSION` is 2: the
    /// two state-carrying fields must reach the wire as base64 STRINGS, not
    /// as JSON number arrays, and must round-trip byte-for-byte.
    #[test]
    fn plugin_state_blobs_serialize_as_base64_strings_not_number_arrays() {
        let row = crate::plugins::PluginInstanceInfo {
            id: "p-1".into(),
            uid: "clap:zyn".into(),
            name: "Zyn".into(),
            format: "clap".into(),
            status: "active".into(),
            track_id: Some("t-1".into()),
        };
        // "hello" — the exact bytes the review's `[104,101,108,108,111]`
        // example expands to under the old encoding.
        let blob = b"hello".to_vec();

        let set_state = Op::PluginSetState { instance: "p-1".into(), state: blob.clone() };
        let s = serde_json::to_string(&set_state).unwrap();
        eprintln!("PluginSetState wire form: {}", s);
        assert!(s.contains("\"state\":\"aGVsbG8=\""), "state must be base64, was: {s}");
        assert!(!s.contains("[104,101"), "the JSON number-array form must be gone: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, set_state, "base64 round-trips byte-for-byte");

        let remove = Op::PluginRemove {
            row: row.clone(),
            index: 0,
            state: Some(blob.clone()),
            params: vec![],
        };
        let s = serde_json::to_string(&remove).unwrap();
        eprintln!("PluginRemove wire form: {}", s);
        assert!(s.contains("\"state\":\"aGVsbG8=\""), "state must be base64, was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, remove);

        // `None` stays `null` — an instance with no host state to save is
        // not the same as one whose state is the empty blob.
        let removed_stateless = Op::PluginRemove { row, index: 0, state: None, params: vec![] };
        let s = serde_json::to_string(&removed_stateless).unwrap();
        assert!(s.contains("\"state\":null"), "wire form was: {s}");
        let back: Op = serde_json::from_str(&s).unwrap();
        assert_eq!(back, removed_stateless);

        // The size claim, made concrete: 4x is not rhetorical.
        let big: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let b64_len = serde_json::to_string(&Op::PluginSetState {
            instance: "p-1".into(),
            state: big.clone(),
        })
        .unwrap()
        .len();
        let array_len = serde_json::to_string(&serde_json::json!(big)).unwrap().len();
        assert!(
            b64_len * 2 < array_len,
            "base64 must be dramatically smaller than the number array ({b64_len} vs {array_len})"
        );
    }

    #[test]
    fn meta_requires_actor_and_run() {
        // actor/run are non-optional by construction: TxMeta has no Default
        // and no Option fields — this test locks the shape and verifies all
        // required fields serialize and deserialize correctly.
        let m = TxMeta { actor: Actor::Engine, run: "r-1".into(), label: "auto-stop".into(), transient: false };
        let s = serde_json::to_string(&m).unwrap();
        // Print once to verify actual wire form
        eprintln!("TxMeta wire form: {}", s);
        // Verify exact key:value pairs for all required fields
        assert!(s.contains("\"actor\":\"engine\""), "actor field must serialize as key:value, was: {s}");
        assert!(s.contains("\"run\":\"r-1\""), "run field must serialize as key:value, was: {s}");
        assert!(s.contains("\"label\":\"auto-stop\""), "label field must serialize as key:value, was: {s}");
        // transient defaults false and round-trips explicitly.
        assert!(s.contains("\"transient\":false"), "transient field must serialize as key:value, was: {s}");
        // Round-trip and assert equality
        let back: TxMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m, "TxMeta must deserialize to the original value");

        // Additive-field compat: a legacy payload without `transient` still
        // deserializes (`#[serde(default)]`), defaulting to false.
        let legacy = r#"{"actor":"engine","run":"r-1","label":"auto-stop"}"#;
        let parsed: TxMeta = serde_json::from_str(legacy).expect("legacy payload without transient parses");
        assert!(!parsed.transient, "missing transient defaults to false");
    }

    #[test]
    fn engine_meta_constructor_stamps_actor_engine() {
        let m = TxMeta::engine("auto-stop");
        assert_eq!(m.actor, Actor::Engine);
        assert!(!m.transient);
        assert!(m.transient().transient);
    }
}
