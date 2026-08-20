//! Insert-FX nodes compiled onto the mixer strip (Plan G1 Task 5 + Task 7).
//!
//! Document slots (`InsertSlot`) become [`InsertNode`]s. The mixer walks
//! them after the source sum and before the fader. Real CLAP/LV2 effect
//! processors are hosted via [`crate::plugins::insert_node_for`] (Task 7);
//! instances that cannot be hosted are skipped (dry strip).

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::audio::dsp::AudioProcessor;
#[cfg(test)]
use crate::audio::dsp::{Effect, ProcessBlock};
use crate::audio::types::TrackState;
use crate::control::session::PluginDoc;
use crate::ids::TrackId;

/// One compiled insert on a track, in document order.
pub struct InsertNode {
    pub slot_id: String,
    pub instance_id: String,
    pub bypassed: bool,
    pub latency: usize,
    pub proc: Arc<InsertNodeCell>,
}

/// An insert processor shared BETWEEN SUCCESSIVE GRAPH SNAPSHOTS.
///
/// Why a cell: node state (plugin memory, delay tails) must survive RCU
/// graph swaps — recreating a plugin per rebuild is unacceptable.
/// Successive snapshots therefore hold `Arc`s to the SAME cell; the
/// control thread's node registry (`InsertNodeRegistry`) holds one more.
///
/// Safety contract (why the unsafe impls are sound):
/// * Only the RT thread calls [`InsertNodeCell::rt_mut`], and it renders
///   exactly ONE graph snapshot at a time — the old snapshot is retired
///   in the same callback invocation that adopts its successor, so no two
///   renders can alias the node.
/// * The control thread constructs and `prepare`s the node BEFORE the
///   first snapshot referencing it is published, and never touches it
///   again while any snapshot references it (parameter traffic goes
///   through atomics or wait-free queues owned by the node).
/// * Deallocation happens on the control thread when the last `Arc` drops
///   (retired snapshots are dropped control-side; so is the registry).
pub struct InsertNodeCell(UnsafeCell<Box<dyn AudioProcessor>>);

// SAFETY: see the contract above — access is exclusive by construction
// (single active snapshot + single RT thread), not by type-system proof.
unsafe impl Send for InsertNodeCell {}
unsafe impl Sync for InsertNodeCell {}

impl InsertNodeCell {
    pub fn new(node: Box<dyn AudioProcessor>) -> Arc<Self> {
        Arc::new(Self(UnsafeCell::new(node)))
    }

    /// RT-thread access during render. See the safety contract on the type.
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn rt_mut(&self) -> &mut dyn AudioProcessor {
        &mut **self.0.get()
    }
}

/// Insert processors keyed by `instance_id`, shared across successive graph
/// snapshots so plugin memory / delay tails survive rebuilds. `key` describes
/// what the node was built from (`insert:<id>@{rate}#{state_rev}!{status}`);
/// a key change replaces the node — the retired cell is freed control-side
/// when the last snapshot referencing it is dropped.
///
/// Task 5 does not instantiate CLAP/LV2: [`compile_inserts`] reuses a
/// matching registry entry or skips. Task 7 fills `resolve_with`'s `make`.
#[derive(Default)]
pub struct InsertNodeRegistry {
    /// `instance_id -> (key, latency, cell)`. Latency is read once at build
    /// time (the node's `latency_samples()`) and cached alongside the cell so
    /// `compile_inserts` can fill `InsertNode::latency` without touching the
    /// cell (which would race the RT thread once a snapshot holds it).
    entries: HashMap<String, (String, usize, Arc<InsertNodeCell>)>,
}

impl InsertNodeRegistry {
    /// Reuse the instance's node when `key` matches, otherwise build one
    /// with `make` (called only when needed). `make` returning None leaves
    /// the registry untouched and returns None so the caller can skip.
    pub fn resolve_with(
        &mut self,
        instance_id: &str,
        key: &str,
        make: impl FnOnce() -> Option<Box<dyn AudioProcessor>>,
    ) -> Option<Arc<InsertNodeCell>> {
        if let Some((k, _, cell)) = self.entries.get(instance_id) {
            if k == key {
                return Some(cell.clone());
            }
        }
        let node = make()?;
        let latency = node.latency_samples();
        let cell = InsertNodeCell::new(node);
        self.entries
            .insert(instance_id.to_string(), (key.to_string(), latency, cell.clone()));
        Some(cell)
    }

    /// The cached latency for an instance (0 when unknown). Read on the
    /// control thread after `resolve_with`, before the graph is published.
    pub fn latency_of(&self, instance_id: &str) -> usize {
        self.entries
            .get(instance_id)
            .map(|(_, lat, _)| *lat)
            .unwrap_or(0)
    }

    /// Drop entries for instances that no longer sit on any insert chain.
    pub fn retain_instances(&mut self, live: &HashSet<String>) {
        self.entries.retain(|id, _| live.contains(id));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The node key currently registered for an instance (test/diagnostic aid).
    pub fn key_of(&self, instance_id: &str) -> Option<&str> {
        self.entries.get(instance_id).map(|(k, _, _)| k.as_str())
    }
}

/// Compile document insert slots to per-track [`InsertNode`] chains.
///
/// Unknown `instance_id`s (not in `PluginDoc`) and instances that cannot be
/// hosted (inactive, or the host cannot build a node — see
/// [`crate::plugins::insert_node_for`]) are skipped. Nodes are cached in
/// `nodes` by `instance_id` + a key carrying the rate/state-rev/status so a
/// rebuilt graph reuses the same processor (delay tails, plugin memory) and
/// a state or rate change retires the old one.
///
/// The second return value lists instance ids that are **marked `"active"`
/// in the document but whose node could NOT be built** (host failure) — the
/// strip plays dry and the caller must surface this to the user (e.g. flip
/// the row to `"crashed"`) rather than leaving a misleading green "active".
pub fn compile_inserts(
    tracks: &[TrackState],
    plugins: &PluginDoc,
    rate: u32,
    nodes: &mut InsertNodeRegistry,
) -> (HashMap<TrackId, Vec<InsertNode>>, Vec<String>) {
    let mut out = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut failed: Vec<String> = Vec::new();
    for t in tracks {
        let mut chain = Vec::new();
        for slot in &t.inserts {
            if used.contains(&slot.instance_id) {
                // G-7 (one instance, one slot): the control plane rejects a
                // duplicate instance_id in Op::InsertAdd, but a hand-edited
                // or future-format project.json isn't schema-guaranteed
                // unique. Compiling the same node twice would run its
                // process() twice per block on the RT thread — skip instead.
                continue;
            }
            let Some(row) = plugins.instances.iter().find(|r| r.id == slot.instance_id) else {
                continue;
            };
            let rev = plugins.state_rev.get(&slot.instance_id).copied().unwrap_or(0);
            let key = format!("insert:{}@{rate}#{rev}!{}", slot.instance_id, row.status);
            let Some(proc) = nodes.resolve_with(&slot.instance_id, &key, || {
                crate::plugins::insert_node_for(plugins, &slot.instance_id, rate)
            }) else {
                // Active-but-unhosted: the instance claims to be live but the
                // host couldn't produce a node — a real, user-visible failure.
                if row.status == "active" {
                    failed.push(slot.instance_id.clone());
                }
                continue;
            };
            let latency = nodes.latency_of(&slot.instance_id);
            used.insert(slot.instance_id.clone());
            chain.push(InsertNode {
                slot_id: slot.id.clone(),
                instance_id: slot.instance_id.clone(),
                bypassed: slot.bypassed,
                latency,
                proc,
            });
        }
        if !chain.is_empty() {
            out.insert(t.id.clone(), chain);
        }
    }
    nodes.retain_instances(&used);
    (out, failed)
}

/// Test double: each sample *= 0.5. Mixer bypass is [`InsertNode::bypassed`],
/// not this flag — `process` always halves.
#[cfg(test)]
pub struct GainHalfEffect {
    pub bypassed: bool,
}

#[cfg(test)]
impl AudioProcessor for GainHalfEffect {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        for s in io.samples.iter_mut() {
            *s *= 0.5;
        }
    }
    fn reset(&mut self) {}
}

#[cfg(test)]
impl Effect for GainHalfEffect {
    fn effect_id(&self) -> &str {
        "aura.test.gainhalf"
    }
    fn display_name(&self) -> &str {
        "Gain Half"
    }
    fn set_bypassed(&mut self, bypassed: bool) {
        self.bypassed = bypassed;
    }
    fn bypassed(&self) -> bool {
        self.bypassed
    }
}

/// Test double: an insert that reports latency and actually delays by it
/// (Plan G1 Task 6) — a stand-in for a lookahead plugin (limiter, linear-
/// phase EQ) whose `process()` genuinely shifts audio, so PDC tests exercise
/// the same "the plugin already ate the delay" shape a real host would.
#[cfg(test)]
pub struct LatencyDummy {
    latency: usize,
    delay: Option<crate::audio::pdc::DelayLine>,
}

#[cfg(test)]
impl LatencyDummy {
    pub fn new(latency: usize) -> Self {
        Self { latency, delay: None }
    }
}

#[cfg(test)]
impl AudioProcessor for LatencyDummy {
    fn prepare(&mut self, _sample_rate: u32, max_block: usize) {
        self.delay = Some(crate::audio::pdc::DelayLine::new(self.latency, max_block, 2));
    }
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        if let Some(d) = self.delay.as_mut() {
            d.process(io.samples);
        }
    }
    fn reset(&mut self) {}
    fn latency_samples(&self) -> usize {
        self.latency
    }
}

#[cfg(test)]
impl Effect for LatencyDummy {
    fn effect_id(&self) -> &str {
        "aura.test.latencydummy"
    }
    fn display_name(&self) -> &str {
        "Latency Dummy"
    }
    fn set_bypassed(&mut self, _bypassed: bool) {}
    fn bypassed(&self) -> bool {
        false
    }
}

/// Test double: each sample *= -1.
#[cfg(test)]
pub struct InvertEffect;

#[cfg(test)]
impl AudioProcessor for InvertEffect {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {}
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        for s in io.samples.iter_mut() {
            *s *= -1.0;
        }
    }
    fn reset(&mut self) {}
}

#[cfg(test)]
impl Effect for InvertEffect {
    fn effect_id(&self) -> &str {
        "aura.test.invert"
    }
    fn display_name(&self) -> &str {
        "Invert"
    }
    fn set_bypassed(&mut self, _bypassed: bool) {}
    fn bypassed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::ProcessBlock;

    #[test]
    fn gain_half_halves_in_place() {
        let mut e = GainHalfEffect { bypassed: false };
        let mut buf = vec![1.0f32, -0.5, 0.25, 0.0];
        let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000, steady: None };
        e.process(&mut io);
        assert_eq!(buf, vec![0.5, -0.25, 0.125, 0.0]);
    }

    #[test]
    fn compile_inserts_skips_unknown_instances() {
        let mut t = crate::audio::types::testutil::test_track("t1");
        t.inserts.push(crate::audio::types::InsertSlot {
            id: "s1".into(),
            instance_id: "missing".into(),
            bypassed: false,
        });
        let mut nodes = InsertNodeRegistry::default();
        let (map, _failed) = compile_inserts(&[t], &PluginDoc::default(), 48_000, &mut nodes);
        assert!(map.is_empty());
        assert!(nodes.is_empty());
    }

    /// G-7 (one instance, one slot): even a document that violates the
    /// invariant (two slots naming the same instance — the control plane
    /// rejects this on `Op::InsertAdd`, but a hand-edited project.json is
    /// not schema-guaranteed unique) must not compile the same node into
    /// the chain twice, or it would run `process()` twice per block.
    #[test]
    fn compile_inserts_dedupes_a_duplicate_instance_id_within_one_track() {
        let mut plugins = PluginDoc::default();
        plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "p-1".into(),
            uid: "clap:/x.clap#fx".into(),
            name: "Fx".into(),
            format: "clap".into(),
            status: "stub".into(),
            track_id: Some("t1".into()),
        });
        let mut nodes = InsertNodeRegistry::default();
        let key = "insert:p-1@48000#0!stub".to_string();
        nodes
            .resolve_with("p-1", &key, || Some(Box::new(InvertEffect)))
            .expect("seed one node for instance p-1");

        let mut t = crate::audio::types::testutil::test_track("t1");
        t.inserts.push(crate::audio::types::InsertSlot {
            id: "s1".into(),
            instance_id: "p-1".into(),
            bypassed: false,
        });
        t.inserts.push(crate::audio::types::InsertSlot {
            id: "s2".into(),
            instance_id: "p-1".into(),
            bypassed: false,
        });

        let (map, _failed) = compile_inserts(&[t], &plugins, 48_000, &mut nodes);
        let chain = map.get("t1").expect("one chain for t1");
        assert_eq!(chain.len(), 1, "duplicate instance_id must compile to one node, not two");
        assert_eq!(chain[0].slot_id, "s1", "first slot in document order wins");
    }

    /// An instance marked `"active"` in the document whose node the host
    /// cannot build (here: an unhostable format) must be REPORTED back to the
    /// caller so it can flip the row to `"crashed"` — the user must see that
    /// the plugin won't work, not a silent dry strip behind a green "active".
    #[test]
    fn compile_inserts_reports_active_instances_whose_node_failed() {
        let mut plugins = PluginDoc::default();
        plugins.instances.push(crate::plugins::PluginInstanceInfo {
            id: "p-1".into(),
            uid: "clap:/x.clap#fx".into(),
            name: "Fx".into(),
            format: "unknown".into(),
            status: "active".into(),
            track_id: Some("t1".into()),
        });
        let mut t = crate::audio::types::testutil::test_track("t1");
        t.inserts.push(crate::audio::types::InsertSlot {
            id: "s1".into(),
            instance_id: "p-1".into(),
            bypassed: false,
        });

        let mut nodes = InsertNodeRegistry::default();
        let (map, failed) = compile_inserts(&[t], &plugins, 48_000, &mut nodes);

        assert!(map.is_empty(), "no node can be built for the unhostable instance");
        assert_eq!(
            failed,
            vec!["p-1".to_string()],
            "an active-but-unhosted instance is reported as failed"
        );
    }
}
