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
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "family", content = "id", rename_all = "camelCase")]
pub enum ObjectRef { Track(crate::ids::TrackId), Clip(crate::ids::ClipId), MidiClip(crate::ids::ClipId) }

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

impl TxMeta {
    /// A fresh correlation id + `label`, attributed to a human through the
    /// Tauri IPC surface. Every migrated A-slice command builds its own
    /// `TxMeta` at the call site (Task 7) rather than defaulting one deep
    /// inside `ControlPlane`, so the actor is always an explicit choice.
    pub fn user(label: impl Into<String>) -> Self {
        Self { actor: Actor::User, run: uuid::Uuid::new_v4().to_string(), label: label.into() }
    }

    /// Same, attributed to the named MCP tool call.
    pub fn agent(tool: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            actor: Actor::Agent { tool: tool.into() },
            run: uuid::Uuid::new_v4().to_string(),
            label: label.into(),
        }
    }

    /// Same, attributed to an automated system process (e.g. a sidecar
    /// job's post-processing hook, not a direct user/agent request).
    pub fn system(label: impl Into<String>) -> Self {
        Self { actor: Actor::System, run: uuid::Uuid::new_v4().to_string(), label: label.into() }
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
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 48_000,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: 48_000,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
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
    }

    #[test]
    fn meta_requires_actor_and_run() {
        // actor/run are non-optional by construction: TxMeta has no Default
        // and no Option fields — this test locks the shape and verifies all
        // required fields serialize and deserialize correctly.
        let m = TxMeta { actor: Actor::Engine, run: "r-1".into(), label: "auto-stop".into() };
        let s = serde_json::to_string(&m).unwrap();
        // Print once to verify actual wire form
        eprintln!("TxMeta wire form: {}", s);
        // Verify exact key:value pairs for all required fields
        assert!(s.contains("\"actor\":\"engine\""), "actor field must serialize as key:value, was: {s}");
        assert!(s.contains("\"run\":\"r-1\""), "run field must serialize as key:value, was: {s}");
        assert!(s.contains("\"label\":\"auto-stop\""), "label field must serialize as key:value, was: {s}");
        // Round-trip and assert equality
        let back: TxMeta = serde_json::from_str(&s).unwrap();
        assert_eq!(back, m, "TxMeta must deserialize to the original value");
    }
}
