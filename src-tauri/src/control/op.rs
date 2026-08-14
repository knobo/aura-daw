//! Op vocabulary and transaction metadata for session mutations.

pub const OP_FORMAT_VERSION: u16 = 1;

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
}

/// Property paths are a closed enum, not strings — renaming a variant is a
/// compile error at every use site, which is the §4.6 anti-drift rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PropPath {
    Gain, Pan, Muted, Soloed, Armed,
    /// Track: sampler-bank id or `plugin:<instanceId>`, `None` = unbound.
    /// Wire form: JSON string or `null` (not an `Option`-shaped object).
    InstrumentId,
    /// Clip: timeline placement position, in samples.
    TimelineStartSamples,
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

        // TrackAdd with real TrackState: verify wire form and round-trip.
        let track = crate::audio::types::TrackState {
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
