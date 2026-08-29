//! `Player`: a pad that is an instrument (Plan V, ruling V-1).
//!
//! A player is a DOCUMENT object — it lives in `Store::players`, mutates
//! through `Op`s and is undoable — and it is NOT a track (V-2): no lanes,
//! no arm, no absolute placements, and no presence in the offline bounce
//! (V-15). What it shares with a track is the mixer strip, and only that:
//! `From<&Player> for MixNode` in `audio::node` is the whole of the
//! overlap, which is why nothing in `compile_inserts` / `compile_routing`
//! learns that players exist.
//!
//! This module is serde and small predicates. No engine, no RT, no locks.

use serde::{Deserialize, Serialize};

use crate::audio::types::{InsertSlot, SendSlot};
use crate::ids::{ClipId, PlayerId, TrackId};

/// What a player sounds when it is fired.
///
/// `AudioClip` and `MidiClip` both name a PLACEMENT (`ClipId`), not a
/// content object: the placement is what carries trim (`offset`, `len`) and
/// the source binding, and V-16 defines raw playback in exactly those
/// terms. Content-keyed sources are V4's business, once envelopes are
/// evaluated at the player's own position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PlayerSource {
    /// An audio clip. With `Player::raw` set this is V-16's bit-exact path.
    AudioClip { clip_id: ClipId },
    /// A MIDI clip plus the instrument that renders it. `instrument_id`
    /// follows `TrackState::instrument_id`'s vocabulary — a sampler-bank id
    /// or `plugin:<instanceId>` — and `None` means the player is silent
    /// until one is bound. The instance it names is owned by NO track:
    /// `PluginInstanceInfo::track_id` is already optional.
    MidiClip {
        clip_id: ClipId,
        #[serde(default)]
        instrument_id: Option<String>,
    },
    /// A pad that carries knobs and nothing else (R5). Renders silence.
    #[default]
    None,
}

/// How a press behaves. V3 adds `quantize`, `chokeGroup` and
/// `velocityToGain` to this struct as `#[serde(default)]` fields, which is
/// additive and needs no format bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum TriggerMode {
    /// The press is a trigger; the source plays to its end.
    #[default]
    OneShot,
    /// Sounds while held; release cuts it.
    Gate,
    /// Repeats from the start until stopped.
    Loop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub mode: TriggerMode,
}

/// The mixer strip a player owns — the same shape a track's compiles to,
/// which is the whole reuse argument (design §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerNode {
    pub gain_db: f64,
    pub pan: f64,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub inserts: Vec<InsertSlot>,
    #[serde(default)]
    pub sends: Vec<SendSlot>,
    /// `None` = master, exactly like `TrackState::output`.
    #[serde(default)]
    pub output: Option<TrackId>,
}

impl Default for PlayerNode {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            inserts: Vec::new(),
            sends: Vec::new(),
            output: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    #[serde(default)]
    pub source: PlayerSource,
    /// V-6: absolute. See [`Player::chain_applies`].
    #[serde(default)]
    pub raw: bool,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    pub node: PlayerNode,
}

impl Player {
    pub fn new(id: PlayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            source: PlayerSource::None,
            raw: false,
            trigger: Trigger::default(),
            node: PlayerNode::default(),
        }
    }

    /// V-6: a raw player's chain does not apply, whatever the document
    /// stores. The fields are kept rather than cleared so unticking `raw`
    /// restores what the user had — the flag is the authority, not the
    /// absence of data.
    pub fn chain_applies(&self) -> bool {
        !self.raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire form is the document form: camelCase, tagged source, and a
    /// default player is the smallest thing that can exist — `source: none`,
    /// not raw, one-shot, unity node straight to master.
    #[test]
    fn default_player_round_trips_as_camel_case_json() {
        let p = Player::new(PlayerId::from("p1"), "PAD 1");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["id"], "p1");
        assert_eq!(v["name"], "PAD 1");
        assert_eq!(v["source"]["kind"], "none");
        assert_eq!(v["raw"], false);
        assert_eq!(v["trigger"]["mode"], "oneShot");
        assert_eq!(v["node"]["gainDb"], 0.0);
        assert_eq!(v["node"]["pan"], 0.0);
        assert_eq!(v["node"]["output"], serde_json::Value::Null);
        let back: Player = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn audio_source_round_trips() {
        let mut p = Player::new(PlayerId::from("p1"), "KICK");
        p.source = PlayerSource::AudioClip { clip_id: ClipId::from("c1") };
        p.raw = true;
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"]["kind"], "audioClip");
        assert_eq!(v["source"]["clipId"], "c1");
        assert_eq!(serde_json::from_value::<Player>(v).unwrap(), p);
    }

    #[test]
    fn midi_source_round_trips() {
        let mut p = Player::new(PlayerId::from("p2"), "PAD");
        p.source = PlayerSource::MidiClip {
            clip_id: ClipId::from("mc1"),
            instrument_id: Some("plugin:i1".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["source"]["kind"], "midiClip");
        assert_eq!(v["source"]["clipId"], "mc1");
        assert_eq!(v["source"]["instrumentId"], "plugin:i1");
        assert_eq!(serde_json::from_value::<Player>(v).unwrap(), p);
    }

    /// V-6 is a property of the DOCUMENT too, not only of what the graph
    /// emits: a raw player answers "does my chain apply" with no.
    #[test]
    fn raw_reports_no_chain_whatever_the_node_says() {
        let mut p = Player::new(PlayerId::from("p1"), "RAW");
        p.raw = true;
        p.node.gain_db = -6.0;
        p.node.inserts.push(InsertSlot {
            id: "i1".into(),
            instance_id: "x".into(),
            bypassed: false,
        });
        assert!(!p.chain_applies());
        p.raw = false;
        assert!(p.chain_applies());
    }
}
