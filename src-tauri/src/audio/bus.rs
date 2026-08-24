//! Bus/return routing compiled onto the mixer graph (Plan G2).
//!
//! A `kind: "bus"` track is a strip with no source of its own. Two different
//! wires reach it, and the difference between them is the whole product:
//!
//! * a **send** is a COPY. The source keeps going wherever it was going, and
//!   a scaled duplicate arrives at the bus. This is how a shared space
//!   works — one convolution reverb, twenty sources, one room, with every
//!   source still heard dry.
//! * an **output** is a MOVE. The track stops reaching the master and goes
//!   through the bus instead. This is a submix: a drum bus, a group, "these
//!   eight things get compressed together".
//!
//! Conflating them is the mistake this module exists to avoid. A send with
//! no dry signal is not an output (the fader is spent holding the dry down),
//! and an output with a wet/dry knob is not a send (there is one instance of
//! the effect per group, not one shared by everybody).
//!
//! This is the pure compile step both graph builders use (`engine::rebuild`
//! and the offline bounce), so live playback and export agree on the
//! topology by construction rather than by two implementations that happen
//! to match today.
//!
//! # The graph
//!
//! Nodes are source tracks and buses. Every node has exactly ONE output edge
//! (a bus, or the master) and any number of send edges into buses. Buses may
//! feed other buses through either kind of edge — a drum bus into a mix bus
//! is the ordinary case — so the compiler topologically sorts them and the
//! renderer walks that order. A CYCLE is illegal: it is only meaningful
//! through an explicit one-block delay node (`SCALABILITY` §1) and there
//! isn't one, so the control plane rejects an edge that would close a loop
//! and this compiler drops any that slipped through a hand-edited file.
//!
//! # Delay compensation
//!
//! Latency is compensated PER EDGE, because one node's copies can owe
//! different amounts of waiting: a track's dry path may have to wait for a
//! slow reverb return that its own send is feeding.
//!
//! ```text
//!   track ─inserts─ pdc ─┬─ send ─(edge delay)─────────► [bus] ─┐
//!                        └─ fader ─(out delay)──────────────────┴─► master
//! ```
//!
//! * `RtTrack::pdc` (Plan G1) aligns the SOURCES with each other and sits
//!   before every tap, so each source leaves at the same time `T_src`. That
//!   is what makes most edge delays zero.
//! * Each sink `s` has an arrival target `T_in(s)`: the latest its inputs
//!   can be ready. Every edge into `s` is padded up to it.
//! * A bus is then ready at `T_in(b) + its own insert latency`, which feeds
//!   the same rule one level down.
//!
//! Targets are computed from DECLARED latency (counting bypassed inserts)
//! and the padding from APPLIED latency (counting only active ones), so
//! toggling bypass never moves the mix — the same G-5 split `pdc::compile_pdc`
//! documents, generalised from one level to a DAG.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::audio::insert::InsertNode;
use crate::audio::pdc::{compile_pdc, DelayLine};
use crate::audio::rt::{RtBus, RtSend};
use crate::audio::types::{is_bus_track, is_mixer_track, TrackState};
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
    /// Samples this edge waits so it reaches the bus in step with the bus's
    /// other inputs. Usually 0 — see the module doc.
    pub delay: usize,
}

impl PlannedSend {
    /// Finish the edge against a `derive_send_slots` map and a block size.
    /// `None` when the live document no longer has this send — the edge is
    /// then dropped, which is correct: a rebuild for that removal is
    /// already queued.
    pub fn resolve(&self, send_slots: &HashMap<String, usize>, max_block: usize) -> Option<RtSend> {
        Some(RtSend {
            bus: self.bus,
            amount: *send_slots.get(&self.id)?,
            pre_fader: self.pre_fader,
            delay: (self.delay > 0).then(|| DelayLine::new(self.delay, max_block, 2)),
        })
    }
}

/// What one rebuild's routing compiles to. Everything here is derived fresh
/// per rebuild from document order, exactly like `derive_slots` — there is
/// no stored allocation state to go stale.
pub struct RoutingPlan {
    /// One entry per bus track that owns a mixer slot, in TOPOLOGICAL order:
    /// a bus that feeds another comes first, so the renderer's single
    /// forward pass is enough. `RtBus::sends` is left EMPTY here and filled
    /// from `sends` once the live send-slot map is known.
    pub buses: Vec<RtBus>,
    /// The document id behind each entry in `buses`, same order — what the
    /// caller re-keys `RtBus::slot` with once the live slot map is read, and
    /// how it tells a bus's planned sends from a track's.
    pub bus_ids: Vec<TrackId>,
    /// Planned send edges for EVERY node, keyed by the source's track id.
    /// Nodes with no (surviving) send are absent.
    pub sends: HashMap<TrackId, Vec<PlannedSend>>,
    /// Per-mixer-slot source alignment delay (`pdc::compile_pdc` over the
    /// SOURCE tracks only — a bus's insert latency is a downstream cost and
    /// must not drag the sources back).
    pub track_pdc: Vec<usize>,
    /// Per-mixer-slot delay on a SOURCE track's output path, applied after
    /// its send taps so only what continues to the destination waits.
    pub out_delay: Vec<usize>,
    /// Per-mixer-slot output destination for a SOURCE track: an index into
    /// `buses`, or `None` for the master.
    pub output: Vec<Option<usize>>,
}

/// A node in the compile-time graph: a source track, or a bus.
struct Node {
    id: TrackId,
    /// `Some(bus index)` or `None` for the master.
    output: Option<usize>,
    /// Bus indices this node sends copies into.
    send_targets: Vec<usize>,
}

/// Compile bus strips, send edges and output routing for one rebuild.
///
/// `chains` is the output of [`crate::audio::insert::compile_inserts`]; the
/// BUS entries are MOVED OUT of it into [`RoutingPlan::buses`], so what the
/// caller has left is exactly the chains that still need attaching to track
/// rows. `n_slots` sizes the per-slot vectors and `max_block` the delay
/// lines (allocated here, on the control thread).
pub fn compile_routing(
    tracks: &[TrackState],
    slots: &HashMap<TrackId, usize>,
    chains: &mut HashMap<TrackId, Vec<InsertNode>>,
    n_slots: usize,
    max_block: usize,
) -> RoutingPlan {
    // ---- bus identity, in document order for now ------------------------
    let bus_docs: Vec<&TrackState> = tracks
        .iter()
        .filter(|t| is_bus_track(t) && slots.contains_key(&t.id))
        .collect();
    let doc_index: HashMap<&TrackId, usize> =
        bus_docs.iter().enumerate().map(|(i, t)| (&t.id, i)).collect();

    // ---- the edge graph, over BOTH edge kinds ---------------------------
    // A destination that is not a bus (missing, or a plain track someone
    // hand-edited in) is dropped to the master rather than guessed at.
    let resolve = |dest: Option<&TrackId>| dest.and_then(|d| doc_index.get(d).copied());
    let mut nodes: Vec<Node> = Vec::new();
    let mut node_of_bus: Vec<usize> = vec![usize::MAX; bus_docs.len()];
    for t in tracks.iter().filter(|t| is_mixer_track(t) && slots.contains_key(&t.id)) {
        if is_bus_track(t) {
            node_of_bus[doc_index[&t.id]] = nodes.len();
        }
        nodes.push(Node {
            id: t.id.clone(),
            output: resolve(t.output.as_ref()),
            send_targets: t.sends.iter().filter_map(|s| resolve(Some(&s.dest))).collect(),
        });
    }
    // A node may not feed itself, whatever the file says.
    for (n, node) in nodes.iter_mut().enumerate() {
        if let Some(b) = node.output {
            if node_of_bus.get(b).copied() == Some(n) {
                node.output = None;
            }
        }
        node.send_targets.retain(|&b| node_of_bus.get(b).copied() != Some(n));
    }

    let order = topological_order(&bus_docs, &nodes, &node_of_bus);
    // Document index -> position in the compiled (topological) `buses` vec.
    let mut pos_of_doc = vec![0usize; bus_docs.len()];
    for (pos, &doc) in order.iter().enumerate() {
        pos_of_doc[doc] = pos;
    }

    // ---- latency bookkeeping --------------------------------------------
    let latency = |chain: Option<&Vec<InsertNode>>| -> (usize, usize) {
        chain.map_or((0, 0), |c| {
            c.iter().fold((0usize, 0usize), |(d, a), n| {
                (d + n.latency, a + if n.bypassed { 0 } else { n.latency })
            })
        })
    };

    // Source alignment (Plan G1's PDC) over the source tracks only.
    let mut declared = vec![0usize; n_slots];
    let mut applied = vec![0usize; n_slots];
    for t in tracks.iter().filter(|t| is_mixer_track(t) && !is_bus_track(t)) {
        let Some(&slot) = slots.get(&t.id) else { continue };
        if slot >= n_slots {
            continue;
        }
        let (d, a) = latency(chains.get(&t.id));
        declared[slot] = d;
        applied[slot] = a;
    }
    let track_pdc = compile_pdc(&declared, &applied);
    // Every source leaves its strip at this latency, taps included.
    let t_src = declared.iter().copied().max().unwrap_or(0);

    // Walk the buses in topological order, deriving each one's input target
    // from the producers that reach it. `t_src` is the floor: any source
    // input arrives there, and a bus with no inputs at all needs a value
    // that cannot make a downstream target too small.
    let mut t_in = vec![t_src; bus_docs.len()]; // by DOC index
    let mut ready_decl = vec![0usize; bus_docs.len()];
    let mut ready_appl = vec![0usize; bus_docs.len()];
    let mut bus_lat = vec![(0usize, 0usize); bus_docs.len()];
    for &doc in &order {
        let (d, a) = latency(chains.get(&bus_docs[doc].id));
        bus_lat[doc] = (d, a);
        ready_decl[doc] = t_in[doc] + d;
        ready_appl[doc] = t_in[doc] + a;
        // Push this bus's readiness onto everything it feeds. Topological
        // order guarantees those targets are still open.
        let n = node_of_bus[doc];
        if n != usize::MAX {
            let node = &nodes[n];
            for &dest in node.output.iter().chain(node.send_targets.iter()) {
                t_in[dest] = t_in[dest].max(ready_decl[doc]);
            }
        }
    }
    // The master's target: the latest of everything that reaches it.
    let mut t_master = 0usize;
    for node in &nodes {
        if node.output.is_none() {
            let doc = doc_index.get(&node.id).copied();
            t_master = t_master.max(match doc {
                Some(d) => ready_decl[d],
                None => t_src,
            });
        }
    }
    // The waiting each sink imposes on an input that is ready at `ready`.
    let target_of = |dest: Option<usize>| dest.map_or(t_master, |d| t_in[d]);

    // ---- build the compiled bus strips, in topological order ------------
    let mut buses: Vec<RtBus> = Vec::with_capacity(order.len());
    let mut bus_ids: Vec<TrackId> = Vec::with_capacity(order.len());
    for &doc in &order {
        let t = bus_docs[doc];
        let slot = slots[&t.id];
        let inserts = chains.remove(&t.id).unwrap_or_default();
        let n = node_of_bus[doc];
        let output = nodes.get(n).and_then(|nd| nd.output).map(|d| pos_of_doc[d]);
        let delay = target_of(nodes.get(n).and_then(|nd| nd.output))
            .saturating_sub(ready_appl[doc]);
        buses.push(RtBus {
            slot,
            inserts,
            sends: Vec::new(),
            output,
            out_pdc: (delay > 0).then(|| DelayLine::new(delay, max_block, 2)),
            win: Default::default(),
        });
        bus_ids.push(t.id.clone());
    }

    // ---- per-node output routing and edge delays ------------------------
    let mut out_delay = vec![0usize; n_slots];
    let mut output = vec![None; n_slots];
    let mut sends: HashMap<TrackId, Vec<PlannedSend>> = HashMap::new();
    for node in &nodes {
        let is_bus = doc_index.contains_key(&node.id);
        let ready = match doc_index.get(&node.id) {
            Some(&doc) => ready_appl[doc],
            None => t_src,
        };
        if !is_bus {
            let slot = slots[&node.id];
            if slot < n_slots {
                output[slot] = node.output.map(|d| pos_of_doc[d]);
                out_delay[slot] = target_of(node.output).saturating_sub(ready);
            }
        }
        // Send edges, in document order, matched back to their rows.
        let Some(doc_track) = tracks.iter().find(|t| t.id == node.id) else { continue };
        let mut planned = Vec::with_capacity(doc_track.sends.len());
        for s in &doc_track.sends {
            let Some(&dest) = doc_index.get(&s.dest) else {
                // Destination missing, not a bus, or this node itself. The
                // document row stays (undo and the UI still need it); the
                // WIRE does not.
                continue;
            };
            if node_of_bus.get(dest).copied() == Some(nodes.iter().position(|n| n.id == node.id).unwrap_or(usize::MAX)) {
                continue;
            }
            if !node.send_targets.contains(&dest) {
                continue; // dropped as a self-edge or a cycle
            }
            planned.push(PlannedSend {
                id: s.id.clone(),
                bus: pos_of_doc[dest],
                pre_fader: s.pre_fader,
                delay: t_in[dest].saturating_sub(ready),
            });
        }
        if !planned.is_empty() {
            sends.insert(node.id.clone(), planned);
        }
    }

    RoutingPlan { buses, bus_ids, sends, track_pdc, out_delay, output }
}

/// Buses in an order where every producer precedes what it feeds (Kahn).
///
/// A bus caught in a CYCLE has no valid position, so it is appended after
/// the sorted prefix and its outgoing edges are already gone from `nodes`
/// by the caller's self-edge filter — the renderer then treats it as a
/// terminal strip. The control plane rejects cycles before they get here;
/// this is the defence for a hand-edited `project.json`, not the policy.
fn topological_order(
    bus_docs: &[&TrackState],
    nodes: &[Node],
    node_of_bus: &[usize],
) -> Vec<usize> {
    let n = bus_docs.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indegree = vec![0usize; n];
    for (doc, &node_idx) in node_of_bus.iter().enumerate() {
        let Some(node) = nodes.get(node_idx) else { continue };
        let mut seen = HashSet::new();
        for &dest in node.output.iter().chain(node.send_targets.iter()) {
            if dest == doc || !seen.insert(dest) {
                continue;
            }
            edges[doc].push(dest);
            indegree[dest] += 1;
        }
    }
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut order = Vec::with_capacity(n);
    while let Some(doc) = queue.pop_front() {
        order.push(doc);
        for &next in &edges[doc] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                queue.push_back(next);
            }
        }
    }
    // Whatever a cycle left behind, in document order, so the output is a
    // permutation of the input no matter what the file said.
    for doc in 0..n {
        if !order.contains(&doc) {
            order.push(doc);
        }
    }
    order
}

/// Would routing `from` into `to` close a loop? Walks the OUTPUT and SEND
/// edges the document already has, starting at `to`, looking for `from`.
///
/// Control-plane guard (`Op::SendAdd`, `Op::TrackSetOutput`). A cycle is
/// only meaningful through an explicit one-block delay node
/// (`SCALABILITY` §1) and there isn't one, so the answer is "reject", not
/// "insert a delay".
pub fn would_cycle(tracks: &[TrackState], from: &TrackId, to: &TrackId) -> bool {
    if from == to {
        return true;
    }
    let by_id: HashMap<&TrackId, &TrackState> = tracks.iter().map(|t| (&t.id, t)).collect();
    let mut seen: HashSet<&TrackId> = HashSet::new();
    let mut stack = vec![to];
    while let Some(id) = stack.pop() {
        if id == from {
            return true;
        }
        if !seen.insert(id) {
            continue;
        }
        let Some(t) = by_id.get(id) else { continue };
        if let Some(out) = t.output.as_ref() {
            stack.push(out);
        }
        for s in &t.sends {
            stack.push(&s.dest);
        }
    }
    false
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

    fn routed(id: &str, kind: &str, output: Option<&str>) -> TrackState {
        let mut t = track(id, kind);
        t.output = output.map(Into::into);
        t
    }

    fn send(id: &str, dest: &str) -> SendSlot {
        SendSlot { id: id.into(), dest: dest.into(), amount_db: 0.0, pre_fader: false }
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

    // ---- send edges ----

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
        assert_eq!(edges[0].resolve(&send_slots, 512).unwrap().amount, send_slots["s1"]);
        assert_eq!(plan.out_delay[slots["a"]], 0, "no insert latency anywhere");
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

    /// A node may not feed ITSELF, whatever a hand-edited file says — that
    /// is a one-node cycle, and there is no delay node to make it legal.
    #[test]
    fn a_self_edge_is_dropped() {
        let mut b = routed("b", "bus", Some("b"));
        b.sends.push(send("s1", "b"));
        let tracks = vec![b];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.buses.len(), 1);
        assert_eq!(plan.buses[0].output, None, "the self-output falls back to the master");
        assert!(plan.sends.is_empty(), "and the self-send is not a wire");
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

    // ---- output routing and bus chaining ----

    #[test]
    fn an_output_edge_compiles_and_a_non_bus_destination_falls_back_to_master() {
        let tracks = vec![
            routed("a", "audio", Some("b")),
            routed("c", "audio", Some("nope")),
            routed("d", "audio", Some("c")),
            track("b", "bus"),
        ];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.output[slots["a"]], Some(0), "a goes through the bus");
        assert_eq!(plan.output[slots["c"]], None, "a missing destination is the master");
        assert_eq!(plan.output[slots["d"]], None, "and so is a destination that is not a bus");
    }

    /// A drum bus into a mix bus: the compiled order must put the producer
    /// first, because the renderer makes ONE forward pass.
    #[test]
    fn buses_come_back_in_topological_order() {
        // Document order is deliberately the WRONG order.
        let tracks = vec![track("mix", "bus"), routed("drums", "bus", Some("mix"))];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(
            plan.bus_ids.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
            ["drums", "mix"],
            "the producer is compiled first whatever the document order"
        );
        assert_eq!(plan.buses[0].output, Some(1), "and points FORWARD at its destination");
        assert_eq!(plan.buses[1].output, None);
    }

    /// A send edge orders the buses too — it is the same graph.
    #[test]
    fn a_bus_to_bus_send_also_orders_the_compile() {
        let mut early = track("early", "bus");
        early.sends.push(send("s1", "late"));
        let tracks = vec![track("late", "bus"), early];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.bus_ids.iter().map(|i| i.as_str()).collect::<Vec<_>>(), ["early", "late"]);
        assert_eq!(plan.sends["early"][0].bus, 1, "forward, into the later strip");
    }

    /// A hand-edited cycle must not hang the compiler or lose a strip. The
    /// control plane rejects cycles; this is the defence for a file that
    /// never went through it.
    #[test]
    fn a_cycle_in_the_document_still_compiles_to_every_bus_once() {
        let tracks = vec![
            routed("x", "bus", Some("y")),
            routed("y", "bus", Some("x")),
            track("z", "bus"),
        ];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        let mut ids: Vec<&str> = plan.bus_ids.iter().map(|i| i.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["x", "y", "z"], "a permutation, no strip dropped or duplicated");
    }

    #[test]
    fn would_cycle_sees_through_both_edge_kinds() {
        let mut a = routed("a", "bus", Some("b"));
        a.sends.push(send("s1", "c"));
        let tracks = vec![a, track("b", "bus"), routed("c", "bus", Some("d")), track("d", "bus")];
        // a -> b (output). Routing b back into a closes the loop.
        assert!(would_cycle(&tracks, &"b".into(), &"a".into()));
        // a -> c (send) -> d. Routing d back into a closes it too.
        assert!(would_cycle(&tracks, &"d".into(), &"a".into()));
        // Nothing reaches b from d, so this one is fine.
        assert!(!would_cycle(&tracks, &"d".into(), &"b".into()));
        // A node may never feed itself.
        assert!(would_cycle(&tracks, &"b".into(), &"b".into()));
    }

    // ---- delay compensation ----

    /// Latency is compensated PER EDGE. A track that both sends into a slow
    /// reverb and goes straight to the master must delay only its OUTPUT
    /// path — delaying the source would take the send with it and the two
    /// would never meet.
    #[test]
    fn the_dry_path_waits_for_a_slow_return_but_the_send_into_it_does_not() {
        let mut a = track("a", "audio");
        a.sends.push(send("s1", "verb"));
        let tracks = vec![a, track("verb", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("verb".into(), latency_chain(256, false));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.out_delay[slots["a"]], 256, "the dry path waits for the return");
        assert_eq!(plan.sends["a"][0].delay, 0, "the copy leaves immediately");
        assert_eq!(
            plan.buses[0].out_pdc.as_ref().map_or(0, |d| d.delay()),
            0,
            "the slowest return sets the target, it does not wait"
        );
        assert!(plan.track_pdc.iter().all(|&d| d == 0));
    }

    #[test]
    fn the_dry_path_waits_for_the_slowest_return_and_short_returns_wait_too() {
        let mut a = track("a", "audio");
        a.sends.push(send("s1", "slow"));
        a.sends.push(send("s2", "fast"));
        let tracks = vec![a, track("slow", "bus"), track("fast", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("slow".into(), latency_chain(256, false));
        chains.insert("fast".into(), latency_chain(64, false));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        let pos = |id: &str| plan.bus_ids.iter().position(|b| b.as_str() == id).unwrap();
        assert_eq!(plan.out_delay[slots["a"]], 256, "the dry path waits for the slowest return");
        assert_eq!(
            plan.buses[pos("slow")].out_pdc.as_ref().map_or(0, |d| d.delay()),
            0,
            "the slowest return sets the target, it does not wait"
        );
        assert_eq!(
            plan.buses[pos("fast")].out_pdc.as_ref().map_or(0, |d| d.delay()),
            256 - 64,
            "the short return waits for the difference"
        );
        assert!(
            plan.track_pdc.iter().all(|&d| d == 0),
            "a bus's latency must not enter the SOURCE alignment: {:?}",
            plan.track_pdc
        );
    }

    /// Down a chain, each hop's target is its own. The track feeding the
    /// slow drum bus must not wait for it — the wait belongs downstream.
    #[test]
    fn a_chain_compensates_at_each_hop_rather_than_all_at_the_top() {
        let tracks = vec![
            routed("kick", "audio", Some("drums")),
            routed("drums", "bus", Some("mix")),
            routed("vox", "audio", Some("mix")),
            track("mix", "bus"),
        ];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("drums".into(), latency_chain(256, false));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        let pos = |id: &str| plan.bus_ids.iter().position(|b| b.as_str() == id).unwrap();
        assert_eq!(plan.out_delay[slots["kick"]], 0, "nothing at the drum bus's input is slow");
        assert_eq!(
            plan.out_delay[slots["vox"]], 256,
            "but at the MIX bus, vox waits for the drum bus's 256"
        );
        assert_eq!(
            plan.buses[pos("drums")].out_pdc.as_ref().map_or(0, |d| d.delay()),
            0,
            "the drum bus is the slow one; it sets the target"
        );
    }

    /// G-5, on the return side: a bypassed insert still DECLARES its
    /// latency, so toggling bypass does not move the return in time — the
    /// delay line supplies the whole gap instead of the plugin.
    #[test]
    fn a_bypassed_bus_insert_still_holds_the_alignment_target() {
        let mut a = track("a", "audio");
        a.sends.push(send("s1", "b"));
        let tracks = vec![a, track("b", "bus")];
        let slots = derive_slots(&tracks);
        let mut chains = HashMap::new();
        chains.insert("b".into(), latency_chain(256, true));
        let plan = compile_routing(&tracks, &slots, &mut chains, slots.len(), 512);
        assert_eq!(plan.out_delay[slots["a"]], 256, "declared latency sets the target");
        assert_eq!(
            plan.buses[0].out_pdc.as_ref().map_or(0, |d| d.delay()),
            256,
            "the plugin is skipped, so the delay line owes the whole 256"
        );
    }
}
