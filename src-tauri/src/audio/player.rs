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

/// How a press behaves.
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

/// When a press takes effect (V3). `Off` is V2's behaviour and the
/// default: the press sounds now.
///
/// Every other value names a musical division of the ARRANGEMENT's grid,
/// so a quantized pad only has a grid to land on while the transport is
/// running — ruling V-21 says such a press fires immediately when it is
/// not. `Whole` and `Bar` are both here and are not the same thing: a
/// whole note is four quarters wherever the meter sits, a bar is what the
/// meter says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Quantize {
    #[default]
    Off,
    Sixteenth,
    Eighth,
    Quarter,
    Whole,
    Bar,
}

impl Quantize {
    /// The grid in QUARTER notes, or `None` for [`Quantize::Off`].
    /// `Bar` answers `None` too — its length is the meter's, which this
    /// enum does not carry; the caller resolves it against the tempo map
    /// (`ControlPlane::quantize_target`).
    pub fn quarters(self) -> Option<f64> {
        match self {
            Quantize::Off | Quantize::Bar => None,
            Quantize::Sixteenth => Some(0.25),
            Quantize::Eighth => Some(0.5),
            Quantize::Quarter => Some(1.0),
            Quantize::Whole => Some(4.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Trigger {
    pub mode: TriggerMode,
    #[serde(default)]
    pub quantize: Quantize,
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
    /// V3: pads sharing a group cut each other, classic hi-hat (owner
    /// answer to design §8 Q2 — one group per pad, not arbitrary sets).
    /// `None` is "chokes nothing and is choked by nothing", which is what
    /// every player migrated from V2 has.
    #[serde(default)]
    pub choke_group: Option<u8>,
    /// V3: how much of the press's velocity reaches the output, 0 (a press
    /// is a press, always unity) to 1 (full range). Default 1.0, so a pad
    /// fired from hardware is velocity-sensitive without being configured
    /// — and a UI press, which carries [`FULL_VELOCITY`], is unity at any
    /// depth, which is why V2's tests did not move.
    #[serde(default = "default_velocity_to_gain")]
    pub velocity_to_gain: f64,
    #[serde(default)]
    pub node: PlayerNode,
}

fn default_velocity_to_gain() -> f64 {
    1.0
}

/// The velocity a press with nothing to say about velocity carries — a
/// mouse click on a pad, or any V2-era caller. Gains unity at every depth.
pub const FULL_VELOCITY: u8 = 127;

impl Player {
    pub fn new(id: PlayerId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            source: PlayerSource::None,
            raw: false,
            trigger: Trigger::default(),
            choke_group: None,
            velocity_to_gain: 1.0,
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

    /// Linear gain for a press at `velocity` (V-18).
    ///
    /// Squared rather than linear-in-amplitude: `v/127` alone spends most
    /// of a pad's travel in the top few dB and makes a soft press barely
    /// quieter, which is why samplers reach for a curve here.
    ///
    /// It applies whatever `raw` says. Velocity is a property of the
    /// PRESS, not of the chain the press feeds, so V-6 has no opinion on
    /// it and V-16's "unity" reads as unity at [`FULL_VELOCITY`] — which
    /// is the velocity the owner's ear-check press carries.
    ///
    /// `velocity_to_gain` is the depth: at 0 this returns 1.0 for every
    /// velocity, at 1 the full curve. Out-of-range document values are
    /// clamped rather than rejected — a stored depth is not a press, and
    /// silencing a pad because a file carried 1.5 is the worse failure.
    pub fn gain_for_velocity(&self, velocity: u8) -> f64 {
        let depth = self.velocity_to_gain.clamp(0.0, 1.0);
        let v = f64::from(velocity.min(FULL_VELOCITY)) / f64::from(FULL_VELOCITY);
        1.0 - depth * (1.0 - v * v)
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

    /// The wire form the frontend sends to `player_set_trigger_mode`. The
    /// default's `"oneShot"` is covered above; these two are the modes that
    /// command exists to make reachable, and a symmetric Rust round trip
    /// cannot see a rename — `#[serde(rename = "GATE_XX")]` on `Gate` leaves
    /// every Rust test green and breaks only the TS caller, at runtime.
    #[test]
    fn the_other_trigger_modes_have_the_wire_names_the_design_specifies() {
        assert_eq!(serde_json::to_value(TriggerMode::Gate).unwrap(), "gate");
        assert_eq!(serde_json::to_value(TriggerMode::Loop).unwrap(), "loop");
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

    /// A project written by V2 has none of V3's three fields, and must open
    /// behaving exactly as it did: no choke group, no quantize, and a
    /// velocity depth of 1.0 — which is unity for the UI press V2 shipped.
    #[test]
    fn a_v2_player_deserializes_with_v3_defaults() {
        let v2 = serde_json::json!({
            "id": "p1",
            "name": "PAD 1",
            "source": { "kind": "none" },
            "raw": false,
            "trigger": { "mode": "oneShot" },
            "node": { "gainDb": 0.0, "pan": 0.0 }
        });
        let p: Player = serde_json::from_value(v2).unwrap();
        assert_eq!(p.choke_group, None);
        assert_eq!(p.trigger.quantize, Quantize::Off);
        assert_eq!(p.velocity_to_gain, 1.0);
        assert_eq!(p.gain_for_velocity(FULL_VELOCITY), 1.0);
    }

    /// The wire names the TypeScript caller sends. A symmetric Rust round
    /// trip cannot see a rename, which is the trap `TriggerMode`'s own test
    /// was written against.
    #[test]
    fn the_v3_fields_have_the_wire_names_the_design_specifies() {
        let mut p = Player::new(PlayerId::from("p1"), "HAT");
        p.choke_group = Some(1);
        p.velocity_to_gain = 0.5;
        p.trigger.quantize = Quantize::Sixteenth;
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["chokeGroup"], 1);
        assert_eq!(v["velocityToGain"], 0.5);
        assert_eq!(v["trigger"]["quantize"], "sixteenth");
        assert_eq!(serde_json::from_value::<Player>(v).unwrap(), p);
        for (q, wire) in [
            (Quantize::Off, "off"),
            (Quantize::Eighth, "eighth"),
            (Quantize::Quarter, "quarter"),
            (Quantize::Whole, "whole"),
            (Quantize::Bar, "bar"),
        ] {
            assert_eq!(serde_json::to_value(q).unwrap(), wire);
        }
    }

    /// `Bar` and `Off` both answer `None`, for opposite reasons, and the
    /// caller must not confuse them: `Off` means "do not wait", `Bar` means
    /// "ask the meter". The distinction lives in `quantize_target`, and this
    /// pins the half that lives here.
    #[test]
    fn quantize_grids_are_in_quarter_notes_and_bar_defers_to_the_meter() {
        assert_eq!(Quantize::Off.quarters(), None);
        assert_eq!(Quantize::Bar.quarters(), None);
        assert_eq!(Quantize::Sixteenth.quarters(), Some(0.25));
        assert_eq!(Quantize::Eighth.quarters(), Some(0.5));
        assert_eq!(Quantize::Quarter.quarters(), Some(1.0));
        assert_eq!(Quantize::Whole.quarters(), Some(4.0));
    }

    /// V-18. Full velocity is unity at every depth — that is what keeps the
    /// V-16 ear-check exact — and depth 0 is unity at every velocity.
    #[test]
    fn velocity_gain_is_unity_at_full_press_and_at_zero_depth() {
        let mut p = Player::new(PlayerId::from("p1"), "PAD");
        assert_eq!(p.gain_for_velocity(FULL_VELOCITY), 1.0);
        p.velocity_to_gain = 0.0;
        assert_eq!(p.gain_for_velocity(1), 1.0);
        assert_eq!(p.gain_for_velocity(64), 1.0);
    }

    /// The curve is monotone and squared: half velocity at full depth is a
    /// quarter of the amplitude, not half. A linear-in-amplitude
    /// implementation passes a monotonicity check and fails this.
    #[test]
    fn velocity_gain_is_squared_and_monotone() {
        let mut p = Player::new(PlayerId::from("p1"), "PAD");
        p.velocity_to_gain = 1.0;
        let half = p.gain_for_velocity(64);
        assert!((half - (64.0f64 / 127.0).powi(2)).abs() < 1e-12, "{half}");
        let mut prev = -1.0;
        for v in 0..=127u8 {
            let g = p.gain_for_velocity(v);
            assert!(g > prev, "velocity {v} did not increase the gain");
            prev = g;
        }
    }

    /// A depth a hand-edited file could carry must not silence the pad or
    /// make it louder than unity.
    #[test]
    fn an_out_of_range_depth_is_clamped_not_rejected() {
        let mut p = Player::new(PlayerId::from("p1"), "PAD");
        p.velocity_to_gain = 1.5;
        assert_eq!(p.gain_for_velocity(0), p.gain_for_velocity(0).clamp(0.0, 1.0));
        assert_eq!(p.gain_for_velocity(FULL_VELOCITY), 1.0);
        p.velocity_to_gain = -2.0;
        assert_eq!(p.gain_for_velocity(0), 1.0);
    }
}
