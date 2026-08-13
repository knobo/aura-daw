//! Op vocabulary and transaction metadata for session mutations.

pub const OP_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Op {
    /// Property-addressed change (round-2 §4: Set { object, path, from, to }).
    Set { object: ObjectRef, path: PropPath, from: serde_json::Value, to: serde_json::Value },
    /// Structural: create a track (payload = full row, so inverse is Remove).
    TrackAdd { track: crate::audio::types::TrackState, index: usize },
    /// Structural: remove a track. Payload kept so the inverse can restore
    /// identity + row byte-identically (gate test 2 arrives in Plan B).
    TrackRemove { track: crate::audio::types::TrackState, index: usize },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "family", content = "id", rename_all = "camelCase")]
pub enum ObjectRef { Track(String), Clip(String), MidiClip(String) }

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
    }

    #[test]
    fn meta_requires_actor_and_run() {
        // actor/run are non-optional by construction: TxMeta has no Default
        // and no Option fields — this test just locks the shape.
        let m = TxMeta { actor: Actor::Engine, run: "r-1".into(), label: "auto-stop".into() };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("engine") && s.contains("r-1"));
    }
}
