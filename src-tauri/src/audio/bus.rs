//! Bus/return routing compiled onto the mixer graph (Plan G2).
//!
//! A `kind: "bus"` track is a **return**: no clips, no instrument, only an
//! insert chain fed by other tracks' [sends](crate::audio::types::SendSlot).
//! That is how every DAW makes a shared space — one convolution reverb, many
//! sources, one room — instead of N instances of the same plugin producing N
//! subtly different rooms at N times the CPU.
//!
//! This module is the pure compile step both graph builders use
//! (`engine::rebuild` and the offline bounce), so live playback and export
//! agree on the topology by construction rather than by two implementations
//! that happen to match today.
//!
//! # Where the delays go, and why there are two of them
//!
//! A return that carries a lookahead limiter comes back LATE. Compensating
//! for that is not "delay the track" — a track's signal both continues to
//! the master and leaves through its sends, and those two copies need
//! different amounts of waiting:
//!
//! ```text
//!   track ─inserts─ pdc ─┬─────────── master_pdc ── fader ──► master
//!                        └─send──► [bus ─inserts─ bus.pdc─ fader] ──► master
//! ```
//!
//! * `RtTrack::pdc` (Plan G1) aligns the SOURCES with each other. It sits
//!   before the taps, so every sender reaches every bus at the same latency
//!   — which is exactly what lets one delay line per bus be enough, instead
//!   of one per edge.
//! * `RtBus::pdc` pads a short return up to the slowest return.
//! * `RtTrack::master_pdc` makes the DRY path wait for the returns. It sits
//!   after the taps, so it delays only what goes straight to the master.
//!
//! With no buses in the project, `master_delay` is 0 and no second delay
//! line is built: the strip is byte-for-byte the pre-G2 strip.
//!
//! # What G2 does not compile
//!
//! Bus-to-bus edges. A cycle is only legal through an explicit one-block
//! delay node (`SCALABILITY` §1), and without that rule a reverb feeding a
//! delay feeding the reverb is an infinite loop in the schedule, not a
//! sound. A send whose destination is not a bus track is DROPPED here (the
//! document keeps the row so undo and the UI still see it).

use std::collections::HashMap;

use crate::audio::insert::InsertNode;
use crate::audio::pdc::{compile_pdc, DelayLine};
use crate::audio::rt::{RtBus, RtSend};
use crate::audio::types::{is_bus_track, is_source_track, TrackState};
use crate::ids::TrackId;

/// One send edge as PLANNED — still carrying its document identity.
///
/// The engine assembles the graph against the published image S and then
/// re-keys it onto the live document L's slot maps, so an edge cannot be
/// finished until L is read: `RtSend::amount` is an index into L's
/// `ParamTable`, and only `SendSlot::id` survives that translation. The RT
/// struct stays index-only; the string lives here, on the control side.
pub struct PlannedSend {
    pub id: String,
    pub bus: usize,
    pub pre_fader: bool,
}

impl PlannedSend {
    /// Finish the edge against a `derive_send_slots` map. `None` when the
    /// live document no longer has this send — the edge is then dropped,
    /// which is correct: a rebuild for that removal is already queued.
    pub fn resolve(&self, send_slots: &HashMap<String, usize>) -> Option<RtSend> {
        Some(RtSend {
            bus: self.bus,
            amount: *send_slots.get(&self.id)?,
            pre_fader: self.pre_fader,
        })
    }
}

/// What one rebuild's routing compiles to. Everything here is derived fresh
/// per rebuild from document order, exactly like `derive_slots` — there is
/// no stored allocation state to go stale.
pub struct RoutingPlan {
    /// One entry per bus track that owns a mixer slot, in document order.
    pub buses: Vec<RtBus>,
    /// The document id behind each entry in `buses`, same order — what the
    /// caller re-keys `RtBus::slot` with once the live slot map is read.
    pub bus_ids: Vec<TrackId>,
    /// Planned send edges, keyed by the SOURCE track. Tracks with no
    /// (surviving) send are absent.
    pub sends: HashMap<TrackId, Vec<PlannedSend>>,
    /// Per-mixer-slot source alignment delay (`pdc::compile_pdc` over the
    /// SOURCE tracks only — a bus's insert latency is a return-path cost and
    /// must not drag the sources back).
    pub track_pdc: Vec<usize>,
    /// Samples the direct-to-master path of every source track owes the
    /// returns. 0 when no bus reports latency, which is the common case.
    pub master_delay: usize,
}

/// Compile bus strips and send edges for one rebuild.
///
/// `chains` is the output of [`crate::audio::insert::compile_inserts`]; the
/// BUS entries are MOVED OUT of it into [`RoutingPlan::buses`], so what the
/// caller has left is exactly the chains that still need attaching to track
/// rows. `n_slots` sizes the per-slot PDC vector, and `max_block` sizes the
/// delay lines (they are allocated here, on the control thread).
pub fn compile_routing(
    tracks: &[TrackState],
    slots: &HashMap<TrackId, usize>,
    chains: &mut HashMap<TrackId, Vec<InsertNode>>,
    n_slots: usize,
    max_block: usize,
) -> RoutingPlan {
    // ---- bus strips, in document order ---------------------------------
    let mut buses: Vec<RtBus> = Vec::new();
    let mut bus_ids: Vec<TrackId> = Vec::new();
    let mut bus_index: HashMap<TrackId, usize> = HashMap::new();
    // (declared, applied) insert latency per bus, paired by index with
    // `buses` — the same distinction `compile_pdc` documents: a BYPASSED
    // insert still declares its latency (so toggling bypass does not move
    // the mix) but does not apply any.
    let mut bus_lat: Vec<(usize, usize)> = Vec::new();
    for t in tracks.iter().filter(|t| is_bus_track(t)) {
        let Some(&slot) = slots.get(&t.id) else { continue };
        let inserts = chains.remove(&t.id).unwrap_or_default();
        let (declared, applied) = inserts.iter().fold((0usize, 0usize), |(d, a), n| {
            (d + n.latency, a + if n.bypassed { 0 } else { n.latency })
        });
        bus_index.insert(t.id.clone(), buses.len());
        bus_ids.push(t.id.clone());
        bus_lat.push((declared, applied));
        buses.push(RtBus { slot, inserts, pdc: None, win: Default::default() });
    }

    // The slowest DECLARED return sets the target every other return — and
    // every dry path — waits for.
    let master_delay = bus_lat.iter().map(|(d, _)| *d).max().unwrap_or(0);
    for (bus, (_, applied)) in buses.iter_mut().zip(bus_lat.iter()) {
        let delay = master_delay.saturating_sub(*applied);
        if delay > 0 {
            bus.pdc = Some(DelayLine::new(delay, max_block, 2));
        }
    }

    // ---- source alignment (Plan G1's PDC, over source tracks only) ------
    let mut declared = vec![0usize; n_slots];
    let mut applied = vec![0usize; n_slots];
    for t in tracks.iter().filter(|t| is_source_track(t)) {
        let Some(&slot) = slots.get(&t.id) else { continue };
        let Some(chain) = chains.get(&t.id) else { continue };
        if slot >= n_slots {
            continue;
        }
        let (d, a) = chain.iter().fold((0usize, 0usize), |(d, a), n| {
            (d + n.latency, a + if n.bypassed { 0 } else { n.latency })
        });
        declared[slot] = d;
        applied[slot] = a;
    }
    let track_pdc = compile_pdc(&declared, &applied);

    // ---- send edges ------------------------------------------------------
    let mut sends: HashMap<TrackId, Vec<PlannedSend>> = HashMap::new();
    for t in tracks.iter().filter(|t| is_source_track(t)) {
        if t.sends.is_empty() || !slots.contains_key(&t.id) {
            continue;
        }
        let mut compiled = Vec::with_capacity(t.sends.len());
        for s in &t.sends {
            let Some(&bus) = bus_index.get(&s.dest) else {
                // Destination is missing, or is not a bus. The document row
                // stays (undo and the UI still need it); the WIRE does not.
                continue;
            };
            compiled.push(PlannedSend { id: s.id.clone(), bus, pre_fader: s.pre_fader });
        }
        if !compiled.is_empty() {
            sends.insert(t.id.clone(), compiled);
        }
    }

    RoutingPlan { buses, bus_ids, sends, track_pdc, master_delay }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::{derive_send_slots, derive_slots, SendSlot};

    fn track(id: &str, kind: &str) -> TrackState {
        let mut t = crate::control::ops::add_track(
            &mut crate::audio::types::Store::default(),
            Some(id.into()),
            Some(kind.into()),
        )
        .unwrap();
        t.id = id.into();
        t
    }

    fn send(id: &str, dest: &str) -> SendSlot {
        SendSlot { id: id.into(), dest: dest.into(), amount_db: 0.0, pre_fader: false }
    }

    #[test]
    fn a_send_to_a_bus_compiles_to_an_edge_and_a_strip() {
        let mut a = track("a", "audio");
        a.sends.push(send("s1", "b"));
        let tracks = vec![a, track("b", "bus")];
        let slots = derive_slots(&tracks);
        let send_slots = derive_send_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.buses.len(), 1);
        assert_eq!(plan.buses[0].slot, slots["b"]);
        let edges = &plan.sends["a"];
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].bus, 0);
        assert_eq!(edges[0].resolve(&send_slots).unwrap().amount, send_slots["s1"]);
        assert_eq!(plan.master_delay, 0, "no insert latency anywhere");
    }

    #[test]
    fn a_send_pointing_at_a_non_bus_track_compiles_to_nothing() {
        let mut a = track("a", "audio");
        a.sends.push(send("s1", "c"));
        let tracks = vec![a, track("c", "audio")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert!(plan.buses.is_empty());
        assert!(plan.sends.is_empty(), "the row survives in the document, the wire does not");
    }

    fn latency_chain(latency: usize, bypassed: bool) -> Vec<InsertNode> {
        use crate::audio::insert::{InsertNodeCell, LatencyDummy};
        vec![InsertNode {
            slot_id: "s".into(),
            instance_id: "i".into(),
            bypassed,
            latency,
            proc: InsertNodeCell::new(Box::new(LatencyDummy::new(latency))),
        }]
    }

    /// The two delays do different jobs and must not be conflated: the DRY
    /// path waits for the slowest RETURN, and a short return waits for the
    /// slow one. A bus's insert latency must not leak into the SOURCE
    /// alignment — doing that would delay the sends as well, and the return
    /// would never catch up with the dry signal.
    #[test]
    fn the_dry_path_waits_for_the_slowest_return_and_short_returns_wait_too() {
        let tracks = vec![track("a", "audio"), track("slow", "bus"), track("fast", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("slow".into(), latency_chain(256, false));
        chains.insert("fast".into(), latency_chain(64, false));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.master_delay, 256, "the dry path waits for the slowest return");
        let slow = plan.buses[0].pdc.as_ref().map_or(0, |d| d.delay());
        let fast = plan.buses[1].pdc.as_ref().map_or(0, |d| d.delay());
        assert_eq!(slow, 0, "the slowest return sets the target, it does not wait");
        assert_eq!(fast, 256 - 64, "the short return waits for the difference");
        assert!(
            plan.track_pdc.iter().all(|&d| d == 0),
            "a bus's latency must not enter the SOURCE alignment: {:?}",
            plan.track_pdc
        );
    }

    /// G-5, on the return side: a bypassed insert still DECLARES its
    /// latency, so toggling bypass does not move the return in time — the
    /// delay line supplies the whole gap instead of the plugin.
    #[test]
    fn a_bypassed_bus_insert_still_holds_the_alignment_target() {
        let tracks = vec![track("a", "audio"), track("b", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("b".into(), latency_chain(256, true));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.master_delay, 256, "declared latency sets the target");
        assert_eq!(
            plan.buses[0].pdc.as_ref().map_or(0, |d| d.delay()),
            256,
            "the plugin is skipped, so the delay line owes the whole 256"
        );
    }

    /// The insert chains of BUS tracks leave `chains` — they belong to the
    /// return strip now, and a caller that also attached them to a track row
    /// would run the same processor twice per block (G-7).
    #[test]
    fn a_bus_chain_is_moved_out_of_the_caller_s_map() {
        let tracks = vec![track("a", "audio"), track("b", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("a".into(), latency_chain(0, false));
        chains.insert("b".into(), latency_chain(0, false));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.buses[0].inserts.len(), 1, "the bus keeps its chain");
        assert!(!chains.contains_key(&crate::ids::TrackId::from("b")), "and only the bus's");
        assert!(chains.contains_key(&crate::ids::TrackId::from("a")));
    }

    /// A bus never sends, even if a hand-edited project says it does — G2
    /// ships no bus-to-bus edge, and compiling one would be a cycle risk.
    #[test]
    fn a_bus_send_row_is_not_compiled() {
        let mut b1 = track("b1", "bus");
        b1.sends.push(send("s1", "b2"));
        let tracks = vec![b1, track("b2", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.buses.len(), 2);
        assert!(plan.sends.is_empty());
    }
}
