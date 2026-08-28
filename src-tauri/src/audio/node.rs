//! `MixNode`: the audio graph compiler's one input type (V-3).
//!
//! Tracks, buses and (a future cut's) players all compile to a `MixNode`.
//! Compiler code (`bus::compile_routing`, `insert::compile_inserts`) is
//! written against `&[MixNode]` and never learns a new producer exists —
//! that is the point of this indirection: V-2 rules a player OUT of the
//! timeline's `TrackState`, so without a shared compile-time shape every
//! mixer-graph function would need a second, parallel path for it.
//!
//! `MixNode` is a compile-time value only. It is never constructed from
//! JSON and never serialized back to it — the document stays exactly
//! `TrackState`/`Clip`/etc (see `docs/ipc-schemas`); this type exists
//! solely so the compiler can stop reading `TrackState` directly. **It
//! must never grow a timeline field** (`clips`, `armed`,
//! `automation_mode`, `instrument_id`, `color`, `group`, ...) — a `Player`
//! producer (V2) has none of those, and giving `MixNode` one would leak
//! timeline concerns back into the one place V-2 keeps them out of.

use crate::audio::types::{InsertSlot, SendSlot, TrackState};
use crate::ids::TrackId;

/// What kind of strip a `MixNode` compiles to. `Automation` exists only so
/// [`mix_nodes`] can be TOTAL over a document's tracks — an automation
/// track drives bindings, not audio, and is filtered out downstream via
/// [`MixNode::takes_mixer_slot`] exactly as `types::is_mixer_track` filters
/// it today. A future `Player` producer (V2) adds a fourth kind; that is
/// also why [`MixNode::id`] is the node's own identity rather than "the
/// track's id" — a player's `MixNode` will not have a `TrackState` behind
/// it at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixNodeKind {
    /// Everything that is not a bus and not an automation track — the
    /// mixer graph's own notion of "source", matching `From<&TrackState>`
    /// below: `"audio"`/`"midi"` map here, and so does any unrecognised
    /// `kind` string (a hand-edited or future-format track), on the same
    /// "don't silently drop it from the mix" logic `is_mixer_track` uses.
    ///
    /// This is WIDER than `types::is_source_track` (`"audio" | "midi"`
    /// only, used by `plugins/mod.rs` and `control/session.rs` to decide
    /// what may carry clips and an instrument) — `kind == Source` does NOT
    /// mean "may carry clips and an instrument". Reach for
    /// `types::is_source_track` for that question; reach for this variant
    /// only for "does this node take a mixer slot but isn't a bus".
    Source,
    /// A return strip (Plan G2): no source, fed by other nodes' sends.
    Bus,
    /// Drives bindings, renders no audio, takes no mixer slot.
    Automation,
    /// A Plan V player: a strip with its own playhead and its own source,
    /// owned by no track (V-2). Takes a mixer slot and is not a bus, so
    /// every routing and insert decision treats it exactly like a source —
    /// which is the point. What separates it from `Source` downstream is
    /// only the clock its slot reads (`audio::clock`) and the fact that the
    /// offline bounce never compiles one (V-15).
    Player,
}

/// One strip in the compile-time mixer graph — everything `bus::compile_routing`
/// and `insert::compile_inserts` need, and nothing a timeline object needs.
/// See the module doc for why that split is load-bearing.
pub struct MixNode {
    /// The node's own identity. For a `Track`/`Bus` producer this is the
    /// `TrackState::id` it was built from; kept separate from "the track's
    /// id" because a `Player` producer (V2) has no track behind it.
    pub id: TrackId,
    pub kind: MixNodeKind,
    /// Fader gain in dB (-inf encoded as -160.0), carried verbatim from the
    /// producer — see `TrackState::gain_db`.
    pub gain_db: f64,
    /// -1.0 (hard left) .. 1.0 (hard right), carried verbatim.
    pub pan: f64,
    pub muted: bool,
    pub soloed: bool,
    /// Ordered insert-FX slots, carried verbatim (`TrackState::inserts`).
    pub inserts: Vec<InsertSlot>,
    /// Send edges into bus nodes, carried verbatim (`TrackState::sends`).
    pub sends: Vec<SendSlot>,
    /// Where this node's fader output goes: `None` = master. Carried
    /// verbatim (`TrackState::output`).
    pub output: Option<TrackId>,
}

impl From<&TrackState> for MixNode {
    /// `kind` maps from `TrackState::kind` with the SAME rule
    /// `types::is_mixer_track` uses today: anything other than
    /// `"automation"` takes a mixer slot, so `"audio"`/`"midi"` map to
    /// `Source`, `"bus"` maps to `Bus`, and an unrecognised string falls
    /// back to `Source` rather than silently dropping out of the mix —
    /// exactly how a hand-edited or future-format `kind` behaves today.
    fn from(t: &TrackState) -> Self {
        let kind = match t.kind.as_str() {
            "automation" => MixNodeKind::Automation,
            "bus" => MixNodeKind::Bus,
            _ => MixNodeKind::Source,
        };
        MixNode {
            id: t.id.clone(),
            kind,
            gain_db: t.gain_db,
            pan: t.pan,
            muted: t.muted,
            soloed: t.soloed,
            inserts: t.inserts.clone(),
            sends: t.sends.clone(),
            output: t.output.clone(),
        }
    }
}

impl From<&crate::audio::player::Player> for MixNode {
    /// V-6 lives HERE, once, rather than in every consumer: a raw player
    /// emits a node with no chain, no sends, unity gain, centre pan and a
    /// master output. `compile_inserts` and `compile_routing` then produce
    /// nothing for it without ever testing a `raw` flag, and the mixer's
    /// fader sees 0 dB because that is what the document compiled to.
    fn from(p: &crate::audio::player::Player) -> Self {
        if p.raw {
            return MixNode {
                id: TrackId::from(p.id.as_str()),
                kind: MixNodeKind::Player,
                gain_db: 0.0,
                pan: 0.0,
                muted: false,
                soloed: false,
                inserts: Vec::new(),
                sends: Vec::new(),
                output: None,
            };
        }
        MixNode {
            id: TrackId::from(p.id.as_str()),
            kind: MixNodeKind::Player,
            gain_db: p.node.gain_db,
            pan: p.node.pan,
            muted: p.node.muted,
            // A player is never soloed: solo is an arrangement gesture over
            // tracks, and a pad that goes silent because someone soloed a
            // track is the deck cutting out mid-performance.
            soloed: false,
            inserts: p.node.inserts.clone(),
            sends: p.node.sends.clone(),
            output: p.node.output.clone(),
        }
    }
}

impl MixNode {
    /// The `MixNode` equivalent of `types::is_bus_track`: a return strip,
    /// fed only by other nodes' sends.
    pub fn is_bus(&self) -> bool {
        self.kind == MixNodeKind::Bus
    }

    /// The `MixNode` equivalent of `types::is_mixer_track`: true for
    /// everything except `Automation`, which drives bindings and renders no
    /// audio.
    pub fn takes_mixer_slot(&self) -> bool {
        self.kind != MixNodeKind::Automation
    }
}

/// Compile every track to a `MixNode`, TOTAL and ORDER-PRESERVING: one node
/// per input track, in document order, nothing filtered — including
/// automation tracks (see [`MixNodeKind::Automation`]) and duplicate ids.
///
/// This is load-bearing, not incidental: a later compiler zips this output
/// against `Store::tracks` by position, so the length and order here must
/// match the input exactly, one-to-one.
pub fn mix_nodes(tracks: &[TrackState]) -> Vec<MixNode> {
    tracks.iter().map(MixNode::from).collect()
}

/// [`mix_nodes`] plus the document's players, appended AFTER every track.
///
/// The order is load-bearing twice over. Downstream code zips this against
/// `Store::tracks` by position, so `mix_nodes(tracks)` must remain an exact
/// PREFIX of this — which is also what keeps a track's mixer slot where it
/// was when a player is added, so a fader knob written against the old
/// numbering never lands on a different strip.
pub fn mix_nodes_with_players(
    tracks: &[TrackState],
    players: &[crate::audio::player::Player],
) -> Vec<MixNode> {
    let mut out = mix_nodes(tracks);
    out.extend(players.iter().map(MixNode::from));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::testutil::test_track;
    use crate::audio::types::{InsertSlot, SendSlot};
    use crate::audio::player::Player;
    use crate::ids::PlayerId;

    fn test_player(id: &str) -> Player {
        Player::new(PlayerId::from(id), "PAD")
    }

    #[test]
    fn a_player_compiles_to_a_source_kind_node_carrying_its_own_id() {
        let mut p = test_player("p1");
        p.node.gain_db = -6.0;
        p.node.pan = 0.5;
        p.node.output = Some("bus1".into());
        let n = MixNode::from(&p);
        assert_eq!(n.id, TrackId::from("p1"));
        assert_eq!(n.kind, MixNodeKind::Player);
        assert_eq!(n.gain_db, -6.0);
        assert_eq!(n.pan, 0.5);
        assert_eq!(n.output, Some(TrackId::from("bus1")));
        assert!(n.takes_mixer_slot());
        assert!(!n.is_bus());
    }

    /// V-6, at the one place it can be enforced once for the whole engine:
    /// a raw player emits a node with nothing on it. Everything downstream
    /// — inserts, sends, PDC, the fader — then does the right thing without
    /// knowing the word "raw".
    #[test]
    fn a_raw_player_emits_a_bare_node_whatever_the_document_stores() {
        let mut p = test_player("p1");
        p.raw = true;
        p.node.gain_db = -6.0;
        p.node.pan = -1.0;
        p.node.muted = true;
        p.node.output = Some("bus1".into());
        p.node.inserts.push(InsertSlot {
            id: "i1".into(),
            instance_id: "x".into(),
            bypassed: false,
        });
        p.node.sends.push(SendSlot {
            id: "s1".into(),
            dest: "bus1".into(),
            amount_db: 0.0,
            pre_fader: false,
        });

        let n = MixNode::from(&p);
        assert_eq!(n.gain_db, 0.0, "unity");
        assert_eq!(n.pan, 0.0, "centre");
        assert!(n.inserts.is_empty(), "no chain");
        assert!(n.sends.is_empty(), "no sends");
        assert_eq!(n.output, None, "straight to master");
        assert!(!n.muted, "raw is not silenced by a stale mute");
    }

    #[test]
    fn mix_nodes_with_players_appends_players_after_every_track() {
        let tracks = vec![test_track("a"), {
            let mut t = test_track("b");
            t.kind = "bus".into();
            t
        }];
        let players = vec![test_player("p1"), test_player("p2")];
        let nodes = mix_nodes_with_players(&tracks, &players);

        assert_eq!(nodes.len(), 4);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "p1", "p2"]);
        assert_eq!(
            nodes.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![
                MixNodeKind::Source,
                MixNodeKind::Bus,
                MixNodeKind::Player,
                MixNodeKind::Player,
            ]
        );
    }

    /// The prefix property is what keeps every track's slot where it was:
    /// adding a player must never renumber a track.
    #[test]
    fn mix_nodes_with_players_has_mix_nodes_as_its_prefix() {
        let tracks = vec![test_track("a"), test_track("b"), test_track("c")];
        let plain = mix_nodes(&tracks);
        let with = mix_nodes_with_players(&tracks, &[test_player("p1")]);
        assert_eq!(with.len(), plain.len() + 1);
        for (i, n) in plain.iter().enumerate() {
            assert_eq!(with[i].id, n.id);
            assert_eq!(with[i].kind, n.kind);
        }
    }

    #[test]
    fn kind_maps_audio_and_midi_to_source() {
        let mut t = test_track("t1");
        t.kind = "audio".into();
        assert_eq!(MixNode::from(&t).kind, MixNodeKind::Source);
        t.kind = "midi".into();
        assert_eq!(MixNode::from(&t).kind, MixNodeKind::Source);
    }

    #[test]
    fn kind_maps_bus() {
        let mut t = test_track("t1");
        t.kind = "bus".into();
        let node = MixNode::from(&t);
        assert_eq!(node.kind, MixNodeKind::Bus);
        assert!(node.is_bus());
        assert!(node.takes_mixer_slot());
    }

    #[test]
    fn kind_maps_automation() {
        let mut t = test_track("t1");
        t.kind = "automation".into();
        let node = MixNode::from(&t);
        assert_eq!(node.kind, MixNodeKind::Automation);
        assert!(!node.takes_mixer_slot());
        assert!(!node.is_bus());
    }

    /// Matches `is_mixer_track`'s `!= "automation"` today: an unrecognised
    /// `kind` string still takes a mixer slot, so it must not silently drop
    /// out of the compiled graph.
    #[test]
    fn kind_maps_unknown_string_to_source() {
        let mut t = test_track("t1");
        t.kind = "something-future".into();
        let node = MixNode::from(&t);
        assert_eq!(node.kind, MixNodeKind::Source);
        assert!(node.takes_mixer_slot());
    }

    #[test]
    fn fields_carried_verbatim() {
        let mut t = test_track("t1");
        t.gain_db = -6.0;
        t.pan = 0.25;
        t.muted = true;
        t.soloed = true;
        t.output = Some("bus1".into());
        t.inserts = vec![InsertSlot {
            id: "ins1".into(),
            instance_id: "plugin1".into(),
            bypassed: true,
        }];
        t.sends = vec![SendSlot {
            id: "send1".into(),
            dest: "bus1".into(),
            amount_db: -3.0,
            pre_fader: true,
        }];

        let node = MixNode::from(&t);
        assert_eq!(node.id, t.id);
        assert_eq!(node.gain_db, -6.0);
        assert_eq!(node.pan, 0.25);
        assert!(node.muted);
        assert!(node.soloed);
        assert_eq!(node.output, Some(TrackId::from("bus1")));
        assert_eq!(node.inserts, t.inserts);
        assert_eq!(node.sends, t.sends);
    }

    #[test]
    fn mix_nodes_is_total_and_order_preserving() {
        let tracks = vec![
            test_track("a"),
            {
                let mut t = test_track("b");
                t.kind = "bus".into();
                t
            },
            {
                let mut t = test_track("c");
                t.kind = "automation".into();
                t
            },
            test_track("a"), // duplicate id — still gets its own node
        ];

        let nodes = mix_nodes(&tracks);

        assert_eq!(nodes.len(), tracks.len());
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c", "a"]);
        assert_eq!(
            nodes.iter().map(|n| n.kind).collect::<Vec<_>>(),
            vec![
                MixNodeKind::Source,
                MixNodeKind::Bus,
                MixNodeKind::Automation,
                MixNodeKind::Source,
            ]
        );
    }
}
