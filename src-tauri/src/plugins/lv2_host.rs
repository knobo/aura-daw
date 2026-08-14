//! LV2 hosting through livi/lilv — zone P2 (PHASE3-PLAN, ARCHITECTURE §15),
//! merged onto the SHARED plugin main thread by the phase-3 architect round.
//!
//! ## Threading model (as built)
//!
//! * **One plugin main thread.** The interim `aura-lv2-host` thread is gone:
//!   all LV2 FFI state — the [`livi::World`] (lilv thread affinity), every
//!   registered instance's control half, and the state:interface shadow
//!   instances — lives in the `"lv2"` slot of [`host::plugin_main`]'s
//!   `MainCtx`, exactly like CLAP's `"clap"` slot (contract §2.2: never a
//!   second FFI thread per format). Requests are closures shipped through
//!   `PluginMainThread::run`/`post` — the unified thread's request protocol.
//! * **Worker spec**: livi wires `worker:schedule` so a plugin's `work()`
//!   runs via [`livi::WorkerManager::run_workers`] — driven by a `MainCtx`
//!   ticker (~30 ms idle cadence; livi's own 100 ms fallback thread also
//!   runs it, both are non-RT). Work RESPONSES are delivered inside
//!   `Instance::run` per the worker spec, handled by livi without allocation.
//! * **RT half**: [`Lv2Node`] implements [`LiveInstrument`] behind the
//!   §15.1 seam. EVERYTHING is preallocated on the plugin main thread at
//!   build: atom sequences, per-port audio/CV buffers, the param ring.
//!   `process` does zero alloc/lock/syscall — MIDI is pushed into the
//!   preallocated `LV2AtomSequence`, `run` connects ports to the
//!   preallocated buffers, errors are counted (not logged) on the RT path.
//!
//! ## Parameters
//!
//! LV2 control-input ports are the param surface: `ParamId = control-port
//! index` (frozen contract §2.5). `plugin_set_param` lands here as a posted
//! closure: the host mirrors values (applied to future nodes) and forwards
//! through a wait-free rtrb ring to the live node, which applies them via
//! `set_control_input` (bounded binary search + clamp + pointer connect).
//!
//! ## State (zone P4 bridge, wired by the architect round)
//!
//! [`Lv2Host::save_state`]/[`Lv2Host::load_state`] run zone P4's raw-lilv
//! `state:interface` glue (`state::lv2_state_blob` /
//! `state::apply_lv2_state_blob`) on the plugin main thread against a lazy
//! **shadow instance** per registration (state calls are main-thread-only
//! and must not touch the RT instance). A loaded blob is remembered and
//! re-applied to every future RT node, so restored Zyn patches are audible.
//!
//! Failure policy: unknown URIs, feature-rejected bundles (livi drops
//! plugins with unsupported required features at world load) and
//! instantiation errors surface as `Err(String)` — callers fall back
//! (registry: instance removed; engine: PolySynth) and AURA never crashes.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::audio::dsp::{AudioProcessor, LiveInstrument, ProcessBlock};
use crate::audio::rt::MAX_LIVE_BLOCK;
use crate::midi::synth::BlockNoteEvent;

use super::descriptor::ParamInfo;
use super::host::{plugin_main, MainCtx};
use super::state::StateBlob;

/// Byte capacity of each preallocated atom sequence (MIDI in / atom out).
/// One 3-byte MIDI event occupies 24 padded bytes -> ~340 events per block.
const ATOM_CAPACITY: usize = 8 * 1024;
/// Capacity of the control->RT param ring (one slot per pending change).
const PARAM_RING_CAPACITY: usize = 1024;
/// Sample rate used for the state shadow instance (LV2 state is
/// rate-independent; 48 kHz is a safe host default).
const SHADOW_RATE: f64 = 48_000.0;

/// The zone's `MainCtx` slot tag (each format owns its tag — contract §2.2).
const SLOT: &str = "lv2";

/// One queued parameter change: (control-port index, plugin-unit value).
type ParamChange = (u32, f32);

// ---------------------------------------------------------------------------
// Main-thread state (lives in the plugin main thread's "lv2" slot)
// ---------------------------------------------------------------------------

/// Slot contents: the lilv world is built lazily on the plugin main thread
/// the first time an LV2 request actually needs it (a world load walks every
/// installed bundle — not a price fire-and-forget callers should pay).
#[derive(Default)]
struct Lv2World {
    inner: Option<WorldInner>,
}

struct WorldInner {
    /// Kept alive for the lilv objects `plugin` handles borrow from.
    _world: livi::World,
    features: Arc<livi::Features>,
    instances: HashMap<String, HostInstance>,
}

/// Host-side state per registered instance.
struct HostInstance {
    plugin: livi::Plugin,
    /// Current param values (port index -> plugin-unit value); applied to
    /// every future node at build so params survive node rebuilds.
    values: HashMap<u32, f32>,
    /// Producer half of the live node's param ring (None until a node
    /// exists; replaced when the engine rebuilds the node).
    param_tx: Option<rtrb::Producer<ParamChange>>,
    /// Lazy main-thread instance for `state:interface` save/load (the RT
    /// node's instance is off-limits to state calls).
    shadow: Option<livi::Instance>,
    /// Last blob applied via [`Lv2Host::load_state`]; re-applied to every
    /// future RT node so restored patches actually sound.
    loaded_blob: Option<StateBlob>,
}

/// The slot's world, built on first use (PLUGIN MAIN THREAD ONLY).
fn world_mut(ctx: &mut MainCtx) -> &mut WorldInner {
    ensure_ticker(ctx);
    let slot = ctx.slot_mut::<Lv2World>(SLOT);
    if slot.inner.is_none() {
        let world = livi::World::new();
        let features = world.build_features(livi::FeaturesBuilder {
            min_block_length: 1,
            max_block_length: MAX_LIVE_BLOCK,
        });
        log::info!(
            "lv2 host: world ready ({} hostable plugins)",
            world.iter_plugins().len()
        );
        slot.inner = Some(WorldInner {
            _world: world,
            features,
            instances: HashMap::new(),
        });
    }
    slot.inner.as_mut().expect("just built")
}

/// LV2 worker pump on the shared main-thread tick (worker spec: scheduled
/// work runs here, never on the RT thread).
fn ensure_ticker(ctx: &mut MainCtx) {
    ctx.add_ticker(
        SLOT,
        Box::new(|ctx| {
            if let Some(inner) = ctx.slot_mut::<Lv2World>(SLOT).inner.as_ref() {
                inner.features.worker_manager().run_workers();
            }
        }),
    );
}

// ---------------------------------------------------------------------------
// Control-plane handle (thin wrappers posting onto the plugin main thread)
// ---------------------------------------------------------------------------

/// Control-plane handle for LV2 requests on the shared plugin main thread.
pub struct Lv2Host;

static HOST: OnceLock<Lv2Host> = OnceLock::new();

/// The process-wide LV2 host handle. The lilv world itself still loads
/// lazily, on the plugin main thread, on the first request that needs it.
pub fn global() -> &'static Lv2Host {
    HOST.get_or_init(|| Lv2Host)
}

/// The host if [`global`] was already called — for callers (remove/set-param
/// paths) that must not boot a lilv world as a side effect.
pub fn try_global() -> Option<&'static Lv2Host> {
    HOST.get()
}

impl Lv2Host {
    /// Register `instance_id` against `uid` (`"lv2:<uri>"` or a bare URI).
    /// Returns the plugin's real control-port parameter list.
    pub fn register_instance(
        &self,
        instance_id: &str,
        uid: &str,
    ) -> Result<Vec<ParamInfo>, String> {
        let uri = uid.strip_prefix("lv2:").unwrap_or(uid).to_string();
        let instance_id = instance_id.to_string();
        plugin_main().run(move |ctx| register(world_mut(ctx), instance_id, &uri))?
    }

    /// Fire-and-forget: drop the host-side registration.
    pub fn unregister_instance(&self, instance_id: &str) {
        let instance_id = instance_id.to_string();
        plugin_main().post(move |ctx| {
            // Never boots the world just to forget an id.
            if let Some(inner) = ctx.slot_mut::<Lv2World>(SLOT).inner.as_mut() {
                inner.instances.remove(&instance_id);
            }
        });
    }

    /// Fire-and-forget: apply parameter changes (values in plugin units,
    /// already clamped by the registry against the real ranges).
    pub fn set_params(&self, instance_id: &str, changes: Vec<ParamChange>) {
        if changes.is_empty() {
            return;
        }
        let instance_id = instance_id.to_string();
        plugin_main().post(move |ctx| {
            let Some(inner) = ctx.slot_mut::<Lv2World>(SLOT).inner.as_mut() else { return };
            let Some(inst) = inner.instances.get_mut(&instance_id) else { return };
            for (id, value) in changes {
                inst.values.insert(id, value);
                if let Some(tx) = inst.param_tx.as_mut() {
                    // Ring full = the RT node is behind; the host-side value
                    // mirror still wins on the next node build.
                    let _ = tx.push((id, value));
                }
            }
        });
    }

    /// Instantiate an RT-ready node for a registered instance. Called from
    /// the engine control thread during rebuild (may block briefly on the
    /// plugin main thread — contract 3; never called on the RT thread).
    pub fn make_node(
        &self,
        instance_id: &str,
        rate: u32,
    ) -> Result<Box<dyn LiveInstrument>, String> {
        let instance_id = instance_id.to_string();
        plugin_main().run(move |ctx| make_node(world_mut(ctx), &instance_id, rate))?
    }

    /// True when the host thread has a registration for `instance_id`
    /// (never boots the world).
    pub fn has_instance(&self, instance_id: &str) -> Result<bool, String> {
        let instance_id = instance_id.to_string();
        plugin_main().run(move |ctx| {
            ctx.slot_mut::<Lv2World>(SLOT)
                .inner
                .as_ref()
                .is_some_and(|inner| inner.instances.contains_key(&instance_id))
        })
    }

    /// Serialize the instance's `state:interface` state on the plugin main
    /// thread (zone P4's raw-lilv glue). `Ok(None)` = the plugin implements
    /// no state interface (callers fall back to the param snapshot).
    pub fn save_state(&self, instance_id: &str) -> Result<Option<StateBlob>, String> {
        let instance_id = instance_id.to_string();
        plugin_main().run(move |ctx| save_state(world_mut(ctx), &instance_id))?
    }

    /// Restore a `KIND_LV2_PROPS` blob into the instance: applied to the
    /// main-thread shadow instance now, and re-applied to every future RT
    /// node built for this registration.
    pub fn load_state(&self, instance_id: &str, blob: StateBlob) -> Result<(), String> {
        let instance_id = instance_id.to_string();
        plugin_main().run(move |ctx| load_state(world_mut(ctx), &instance_id, blob))?
    }
}

// ---------------------------------------------------------------------------
// Main-thread request bodies
// ---------------------------------------------------------------------------

fn register(
    inner: &mut WorldInner,
    instance_id: String,
    uri: &str,
) -> Result<Vec<ParamInfo>, String> {
    let plugin = inner._world.plugin_by_uri(uri).ok_or_else(|| {
        format!(
            "LV2 plugin not available: {uri} (not installed, or its required \
             features are unsupported by the host)"
        )
    })?;
    let counts = *plugin.port_counts();
    if counts.audio_outputs == 0 {
        return Err(format!("{uri}: no audio outputs — cannot host as an instrument"));
    }
    let params = control_port_params(&plugin);
    let values = params.iter().map(|p| (p.id, p.value as f32)).collect();
    inner.instances.insert(
        instance_id,
        HostInstance { plugin, values, param_tx: None, shadow: None, loaded_blob: None },
    );
    Ok(params)
}

/// Real `ParamInfo` list from the plugin's control-input ports
/// (`ParamId = port index`, contract §2.5). Missing range metadata falls
/// back to a sane 0..=1-style span around the default.
fn control_port_params(plugin: &livi::Plugin) -> Vec<ParamInfo> {
    plugin
        .ports_with_type(livi::PortType::ControlInput)
        .map(|p| {
            let default = p.default_value as f64;
            let min = p.min_value.map(f64::from).unwrap_or_else(|| default.min(0.0));
            let max = p.max_value.map(f64::from).unwrap_or_else(|| default.max(1.0));
            let max = if max > min { max } else { min + 1.0 };
            ParamInfo {
                id: p.index.0 as u32,
                name: p.name.clone(),
                min,
                max,
                default: default.clamp(min, max),
                value: default.clamp(min, max),
                steps: 0,
            }
        })
        .collect()
}

fn make_node(
    inner: &mut WorldInner,
    instance_id: &str,
    rate: u32,
) -> Result<Box<dyn LiveInstrument>, String> {
    let WorldInner { features, instances, .. } = inner;
    let inst = instances
        .get_mut(instance_id)
        .ok_or_else(|| format!("LV2 instance not registered with the host: {instance_id}"))?;
    if rate == 0 {
        return Err("engine sample rate is 0".into());
    }
    // SAFETY: running third-party plugin code — the documented v1 in-process
    // risk (ARCHITECTURE §15.3). Instantiation happens on the plugin main
    // thread.
    let mut instance = unsafe { inst.plugin.instantiate(features.clone(), rate as f64) }
        .map_err(|e| format!("LV2 instantiation failed for {}: {e}", inst.plugin.uri()))?;
    // Restore persisted plugin state (Zyn patches!) into the fresh RT
    // instance BEFORE params, then re-apply the instance's current param
    // values so node rebuilds keep the user's settings.
    if let Some(blob) = &inst.loaded_blob {
        if let Err(e) = super::state::apply_lv2_state_blob(&mut instance, features, blob) {
            log::warn!(
                "lv2 host: state restore into new node for {instance_id} failed ({e}); \
                 rendering without it"
            );
        }
    }
    for (&id, &value) in &inst.values {
        instance.set_control_input(livi::PortIndex(id as usize), value);
    }
    let (param_tx, param_rx) = rtrb::RingBuffer::new(PARAM_RING_CAPACITY);
    inst.param_tx = Some(param_tx);
    let node = Lv2Node::new(instance, features, param_rx);
    log::info!(
        "lv2 host: node ready for instance {instance_id} ({} @ {rate} Hz)",
        inst.plugin.uri()
    );
    Ok(Box::new(node))
}

/// Ensure the registration's main-thread shadow instance exists (with any
/// loaded blob applied) and return it.
fn ensure_shadow<'a>(
    features: &Arc<livi::Features>,
    inst: &'a mut HostInstance,
) -> Result<&'a mut livi::Instance, String> {
    if inst.shadow.is_none() {
        // SAFETY: third-party plugin code on the owning (main) thread — the
        // documented v1 in-process risk.
        let mut shadow = unsafe { inst.plugin.instantiate(features.clone(), SHADOW_RATE) }
            .map_err(|e| format!("LV2 state instantiation failed for {}: {e}", inst.plugin.uri()))?;
        if let Some(blob) = &inst.loaded_blob {
            super::state::apply_lv2_state_blob(&mut shadow, features, blob)?;
        }
        inst.shadow = Some(shadow);
    }
    Ok(inst.shadow.as_mut().expect("just built"))
}

fn save_state(inner: &mut WorldInner, instance_id: &str) -> Result<Option<StateBlob>, String> {
    let WorldInner { features, instances, .. } = inner;
    let inst = instances
        .get_mut(instance_id)
        .ok_or_else(|| format!("LV2 instance not registered with the host: {instance_id}"))?;
    let shadow = ensure_shadow(features, inst)?;
    super::state::lv2_state_blob(shadow, features)
}

fn load_state(inner: &mut WorldInner, instance_id: &str, blob: StateBlob) -> Result<(), String> {
    let WorldInner { features, instances, .. } = inner;
    let inst = instances
        .get_mut(instance_id)
        .ok_or_else(|| format!("LV2 instance not registered with the host: {instance_id}"))?;
    // Remember the blob FIRST so future RT nodes pick it up even when the
    // shadow apply below fails (never lose restore data).
    inst.loaded_blob = Some(blob.clone());
    let shadow = ensure_shadow(features, inst)?;
    super::state::apply_lv2_state_blob(shadow, features, &blob)
}

// ---------------------------------------------------------------------------
// RT node
// ---------------------------------------------------------------------------

/// Encode one [`BlockNoteEvent`] as channel-0 MIDI (velocity 0 = note off,
/// per the frozen `LiveInstrument` contract).
#[inline]
fn midi_for_event(ev: &BlockNoteEvent) -> [u8; 3] {
    if ev.velocity == 0 {
        [0x80, ev.key & 0x7f, 0x40]
    } else {
        [0x90, ev.key & 0x7f, ev.velocity & 0x7f]
    }
}

/// MIDI CC 123 "All Notes Off" (channel 0) — release, not kill.
const ALL_NOTES_OFF: [u8; 3] = [0xB0, 123, 0];

/// A live LV2 instrument node behind the §15.1 seam. Built fully
/// preallocated on the plugin main thread; `process`/`queue_event`/
/// `all_notes_off` run on the RT thread under the §2.1 contract.
pub struct Lv2Node {
    instance: livi::Instance,
    midi_urid: Urid,
    /// Atom-sequence inputs; `[0]` carries this block's MIDI, the rest stay
    /// empty (cleared once at build).
    atom_in: Vec<livi::event::LV2AtomSequence>,
    atom_out: Vec<livi::event::LV2AtomSequence>,
    /// Deinterleaved per-port buffers, `MAX_LIVE_BLOCK` frames each.
    audio_in: Vec<Vec<f32>>,
    audio_out: Vec<Vec<f32>>,
    cv_in: Vec<Vec<f32>>,
    cv_out: Vec<Vec<f32>>,
    /// Wait-free control->RT parameter changes.
    param_rx: rtrb::Consumer<ParamChange>,
    dropped_events: u64,
    run_errors: u64,
}

/// `lv2_raw::LV2Urid` (a plain `u32`) — livi exposes it only through
/// function signatures, so we alias it locally.
type Urid = u32;

impl Lv2Node {
    /// Build on the PLUGIN MAIN thread: all allocation happens here, never
    /// in `process`.
    fn new(
        instance: livi::Instance,
        features: &std::sync::Arc<livi::Features>,
        param_rx: rtrb::Consumer<ParamChange>,
    ) -> Self {
        let counts = instance.port_counts();
        let buffers = |n: usize| vec![vec![0.0f32; MAX_LIVE_BLOCK]; n];
        let sequences = |n: usize| -> Vec<livi::event::LV2AtomSequence> {
            (0..n)
                .map(|_| livi::event::LV2AtomSequence::new(features, ATOM_CAPACITY))
                .collect()
        };
        Self {
            midi_urid: features.midi_urid(),
            atom_in: sequences(counts.atom_sequence_inputs),
            atom_out: sequences(counts.atom_sequence_outputs),
            audio_in: buffers(counts.audio_inputs),
            audio_out: buffers(counts.audio_outputs),
            cv_in: buffers(counts.cv_inputs),
            cv_out: buffers(counts.cv_outputs),
            param_rx,
            dropped_events: 0,
            run_errors: 0,
            instance,
        }
    }

    /// Events dropped because the atom input filled up (diagnostics).
    pub fn dropped_events(&self) -> u64 {
        self.dropped_events
    }

    /// Failed `run` calls (counted, never logged, on the RT path).
    pub fn run_errors(&self) -> u64 {
        self.run_errors
    }

    /// RT-safe push into the block's MIDI sequence.
    #[inline]
    fn push_midi(&mut self, offset: u32, data: &[u8; 3]) -> bool {
        let Some(seq) = self.atom_in.first_mut() else { return false };
        if seq.push_midi_event::<3>(i64::from(offset), self.midi_urid, data).is_err() {
            self.dropped_events += 1;
            return false;
        }
        true
    }
}

impl AudioProcessor for Lv2Node {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {
        // Fully preallocated at build on the plugin main thread; the sample
        // rate is fixed at instantiation (the live-node key carries the
        // rate, so a rate change rebuilds the node through the host).
    }

    /// RT: connect preallocated ports, run the plugin, ADD its output into
    /// the zeroed stereo scratch. No alloc/lock/syscall; errors counted.
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        let frames = io.frames().min(MAX_LIVE_BLOCK);
        if frames == 0 {
            return;
        }
        // Apply pending param changes (wait-free pop; set_control_input is a
        // bounded binary search + clamp + pointer connect).
        while let Ok((id, value)) = self.param_rx.pop() {
            self.instance.set_control_input(livi::PortIndex(id as usize), value);
        }
        let Self { instance, atom_in, atom_out, audio_in, audio_out, cv_in, cv_out, .. } = self;
        let ports = livi::EmptyPortConnections::new()
            .with_audio_inputs(audio_in.iter().map(|b| &b[..frames]))
            .with_audio_outputs(audio_out.iter_mut().map(|b| &mut b[..frames]))
            .with_atom_sequence_inputs(atom_in.iter())
            .with_atom_sequence_outputs(atom_out.iter_mut())
            .with_cv_inputs(cv_in.iter().map(|b| &b[..frames]))
            .with_cv_outputs(cv_out.iter_mut().map(|b| &mut b[..frames]));
        // SAFETY: buffers are preallocated at MAX_LIVE_BLOCK >= frames and
        // port counts mirror the instance's; running plugin code is the
        // documented in-process v1 risk.
        let ok = unsafe { instance.run(frames, ports) }.is_ok();
        if ok {
            // ADD into the scratch (contract: the node sums, the mixer owns
            // gain/pan). Mono plugins are center-duplicated; >2 outs take
            // the first stereo pair.
            let ch = io.channels;
            let (li, ri) = match self.audio_out.len() {
                0 => {
                    self.consume_block();
                    return;
                }
                1 => (0, 0),
                _ => (0, 1),
            };
            for i in 0..frames {
                let l = self.audio_out[li][i];
                let r = self.audio_out[ri][i];
                let base = i * ch;
                if ch >= 2 {
                    io.samples[base] += l;
                    io.samples[base + 1] += r;
                } else {
                    io.samples[base] += 0.5 * (l + r);
                }
            }
        } else {
            self.run_errors += 1;
        }
        self.consume_block();
    }

    fn reset(&mut self) {
        self.all_notes_off();
    }
}

impl Lv2Node {
    /// Clear the block's consumed MIDI input (header rewrite, no alloc).
    #[inline]
    fn consume_block(&mut self) {
        if let Some(seq) = self.atom_in.first_mut() {
            seq.clear();
        }
    }
}

impl LiveInstrument for Lv2Node {
    fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        let data = midi_for_event(&ev);
        self.push_midi(ev.offset, &data)
    }

    fn all_notes_off(&mut self) {
        // Delivered at the start of the next `process` block. CC 123 is a
        // release (voices enter their release phase), not a hard kill.
        self.push_midi(0, &ALL_NOTES_OFF);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::descriptor::lv2_uid;

    const ZYN_URI: &str = "http://zynaddsubfx.sourceforge.net";
    const EPIANO_URI: &str = "http://drobilla.net/plugins/mda/EPiano";

    fn note_on(offset: u32, key: u8, velocity: u8) -> BlockNoteEvent {
        BlockNoteEvent { offset, key, velocity }
    }

    #[test]
    fn midi_encoding_follows_the_velocity_zero_off_contract() {
        assert_eq!(midi_for_event(&note_on(0, 69, 100)), [0x90, 69, 100]);
        assert_eq!(midi_for_event(&note_on(3, 69, 0)), [0x80, 69, 0x40], "vel 0 = off");
        // Out-of-range values are masked into valid MIDI, never UB.
        assert_eq!(midi_for_event(&note_on(0, 200, 255)), [0x90, 200 & 0x7f, 255 & 0x7f]);
    }

    /// The unified-thread contract (architect merge of contract §2.2): LV2
    /// requests are served by the SAME `aura-plugin-host` thread that owns
    /// the CLAP world — no `aura-lv2-host` thread exists in the process.
    #[test]
    fn lv2_runs_on_the_shared_plugin_main_thread() {
        let host = global();
        // Any request runs on the shared thread; verify by name from inside.
        let name = crate::plugins::host::plugin_main()
            .run(|_| std::thread::current().name().map(str::to_owned))
            .unwrap();
        assert_eq!(name.as_deref(), Some("aura-plugin-host"));
        // An LV2 request reaches the same thread (it is the only executor).
        let err = host
            .register_instance("thread-check", "lv2:urn:aura-test:nope")
            .expect_err("unknown uri fails");
        assert!(err.contains("urn:aura-test:nope"));
    }

    /// Robustness: unknown URIs and unregistered instances fail with an
    /// error — no panic, no zombie state, host stays usable.
    #[test]
    fn unknown_uri_and_unregistered_instance_fail_gracefully() {
        let host = global();
        let err = host
            .register_instance("bad-inst", "lv2:urn:aura-test:no-such-plugin")
            .expect_err("unknown URI must fail");
        assert!(err.contains("urn:aura-test:no-such-plugin"), "error names the URI: {err}");
        let err = match host.make_node("never-registered", 48_000) {
            Ok(_) => panic!("make_node for an unregistered instance must fail"),
            Err(e) => e,
        };
        assert!(err.contains("never-registered"), "error names the instance: {err}");
        // Fire-and-forget paths tolerate unknown ids.
        host.unregister_instance("never-registered");
        host.set_params("never-registered", vec![(0, 1.0)]);
        // State paths fail politely for unknown ids.
        assert!(host.save_state("never-registered").is_err());
        assert!(host
            .load_state(
                "never-registered",
                StateBlob { kind: crate::plugins::state::KIND_LV2_PROPS, data: vec![0, 0, 0, 0] },
            )
            .is_err());
        assert_eq!(host.has_instance("never-registered"), Ok(false));
        // The host is still healthy afterwards.
        assert!(host
            .register_instance("still-bad", "lv2:urn:aura-test:also-missing")
            .is_err());
    }

    /// Param contract (§2.5): control-port enumeration with real names and
    /// ranges (`ParamId = port index`), set-before-build applies to the
    /// node, and the node renders audibly — exercised against mda EPiano
    /// when installed.
    #[test]
    fn control_port_params_enumerate_and_node_renders() {
        let host = global();
        let params = match host.register_instance("epiano-test", &lv2_uid(EPIANO_URI)) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("skipping: mda EPiano not installed ({e})");
                return;
            }
        };
        assert!(!params.is_empty(), "EPiano exposes control ports");
        for p in &params {
            assert!(!p.name.is_empty());
            assert!(p.max > p.min, "sane range for {}", p.name);
            assert!(p.default >= p.min && p.default <= p.max);
            assert_eq!(p.value, p.default, "initial value is the default");
        }
        // EPiano's control ports occupy indices 0..12.
        assert_eq!(params.len(), 12);
        assert_eq!(params[0].id, 0);
        assert_eq!(host.has_instance("epiano-test"), Ok(true));

        // Set a param before the node exists: applied at node build.
        host.set_params("epiano-test", vec![(params[0].id, 0.8)]);
        let mut node = host.make_node("epiano-test", 48_000).expect("node builds");

        node.prepare(48_000, 512);
        assert!(node.queue_event(note_on(0, 60, 110)));
        let mut buf = vec![0.0f32; 512 * 2];
        let mut energy = 0.0f32;
        for _ in 0..20 {
            buf.fill(0.0);
            let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000 };
            node.process(&mut io);
            energy += buf.iter().map(|s| s.abs()).sum::<f32>();
        }
        assert!(energy > 0.0, "EPiano renders audible output");

        // Params reach the RUNNING node through the ring without error, and
        // tiny odd-sized blocks are legal (min_block_length = 1).
        host.set_params("epiano-test", vec![(params[0].id, 0.2)]);
        std::thread::sleep(std::time::Duration::from_millis(50)); // main-thread tick
        let mut tiny = vec![0.0f32; 2];
        let mut io = ProcessBlock { samples: &mut tiny, channels: 2, sample_rate: 48_000 };
        node.process(&mut io);
        host.unregister_instance("epiano-test");
    }

    // ---- ZynAddSubFX acceptance (PHASE3-PLAN contract 7 — gates the round) --

    use crate::audio::mixer;
    use crate::audio::rt::{ParamTable, RtGraph, RtTrack};
    use crate::audio::sampler_voice::testutil::estimate_freq;
    use crate::audio::transport::LoopSpec;
    use crate::audio::types::{Store, TrackState};
    use crate::midi::playback::{append_from, LiveNodeRegistry};
    use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
    use crate::midi::MidiStore;
    use crate::plugins::{register_registry, registered_registry, PluginRegistry};
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// The process-global plugin registry used by `plugins::live_node_for`.
    /// First caller registers it; every lv2 test shares it (OnceLock).
    fn shared_registry() -> Arc<Mutex<PluginRegistry>> {
        register_registry(Arc::new(Mutex::new(PluginRegistry::default())));
        registered_registry().expect("registered above").clone()
    }

    fn midi_track(id: &str, instrument_id: Option<String>) -> TrackState {
        TrackState {
            id: id.into(),
            name: id.into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id,
        }
    }

    fn store_with_clip(
        track_id: &str,
        instrument_id: &str,
        note: MidiNote,
        clip_len_ticks: u64,
    ) -> (Store, MidiStore) {
        let mut store = Store::default();
        store.tracks.push(midi_track(track_id, Some(instrument_id.into())));
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![MidiClip {
                id: crate::ids::ClipId::mint(),
                track_id: track_id.into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: clip_len_ticks,
                notes: vec![note],
                next_note_id: 1,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track(track_id),
            }],
            loaded_dir: None,
            dirty: false,
        };
        (store, midi)
    }

    /// Render `frames` from `pos` through the REAL RT path in 512-frame
    /// callback chunks; `discontinuity` applies to the first chunk only.
    fn render_from(
        g: &mut RtGraph,
        mut pos: u64,
        frames: usize,
        rate: u32,
        mut discontinuity: bool,
    ) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * 2];
        for chunk in out.chunks_mut(512 * 2) {
            mixer::render(g, pos, &LoopSpec::OFF, chunk, 2, rate, discontinuity, None);
            discontinuity = false;
            pos += (chunk.len() / 2) as u64;
        }
        out
    }

    fn mono_of(buf: &[f32]) -> Vec<f32> {
        buf.iter().step_by(2).copied().collect()
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Registry-side setup shared by the Zyn tests: real scan -> instantiate
    /// -> host registration -> status "active". Returns the instance id, or
    /// None (skip) when ZynAddSubFX is not installed.
    fn activate_zyn(reg: &Arc<Mutex<PluginRegistry>>) -> Option<String> {
        let scanned = crate::plugins::scan::scan_lv2();
        if !scanned.iter().any(|d| d.uid == lv2_uid(ZYN_URI)) {
            eprintln!("skipping: zynaddsubfx-lv2 not installed");
            return None;
        }
        let info = {
            let mut reg = reg.lock();
            // EXTEND the shared registry's scan results instead of replacing
            // them: this registry is process-global (OnceLock) and the CLAP
            // tests race us — overwriting with an LV2-only list would yank
            // their descriptors out from under them.
            let merged = reg.scanned.get_or_insert_with(Vec::new);
            for d in scanned {
                if !merged.iter().any(|m| m.uid == d.uid) {
                    merged.push(d);
                }
            }
            reg.instantiate(&lv2_uid(ZYN_URI)).expect("Zyn instantiates in the registry")
        };
        assert_eq!(info.status, "stub");
        let params = global()
            .register_instance(&info.id, &info.uid)
            .expect("Zyn registers with the LV2 host");
        let activated = reg.lock().activate(&info.id, params).expect("activate");
        assert_eq!(activated.status, "active", "contract 4: stub -> active");
        Some(info.id)
    }

    /// THE GATING ACCEPTANCE TEST (PHASE3-PLAN §2.7): headless, through the
    /// real engine path — a midi track bound to `plugin:<zyn-instance>`
    /// resolves to a REAL Lv2Node in the RCU graph and `mixer::render`
    /// produces non-silent, correctly PITCHED audio (A4 note -> f0 ≈ 440 Hz),
    /// and the voice stops after its note-off.
    #[test]
    fn zyn_acceptance_midi_track_renders_pitched_audio_headlessly() {
        const RATE: u32 = 48_000;
        let reg = shared_registry();
        let Some(instance_id) = activate_zyn(&reg) else { return };

        // A4 for one beat (0.5 s @ 120 bpm): on at sample 0, off at 24000.
        let note = MidiNote { tick: 0, length_ticks: 960, key: 69, velocity: 110, channel: 0, note_id: crate::ids::NoteId(0) };
        let (store, midi) = store_with_clip("zyn-track", &format!("plugin:{instance_id}"), note, 1920);

        let mut nodes = LiveNodeRegistry::default();
        let mut tracks: Vec<RtTrack> = Vec::new();
        let slots = crate::audio::types::derive_slots(&store.tracks);
        append_from(&midi, &store, &slots, RATE, None, &mut nodes, &mut tracks);
        assert_eq!(tracks.len(), 1);
        // The registry key proves the PLUGIN node resolved (a PolySynth
        // fallback would be keyed "synth@48000" — and would fake the pitch).
        assert_eq!(
            nodes.key_of("zyn-track"),
            Some(format!("plugin:{instance_id}@{RATE}").as_str()),
            "track resolved to the real LV2 node"
        );

        // 3 s render: sustain [0, 24000), release tail afterwards.
        let mut g = RtGraph::new(tracks, 1, Arc::new(ParamTable::default()));
        let audio = mono_of(&render_from(&mut g, 0, 3 * RATE as usize, RATE, false));
        let sustain = &audio[4_000..22_000];
        let sustain_peak = peak(sustain);
        assert!(sustain_peak > 0.01, "Zyn renders non-silent audio (peak {sustain_peak})");

        let f0 = estimate_freq(sustain, RATE, 200.0, 900.0);
        eprintln!("zyn acceptance: sustain peak {sustain_peak:.3}, f0 {f0:.1} Hz (target 440)");
        assert!(
            (f0 - 440.0).abs() / 440.0 < 0.03,
            "Zyn pitch: got {f0:.1} Hz, want ~440 Hz"
        );

        // Voices stop after the off: the last second is far below sustain.
        let tail_peak = peak(&audio[2 * RATE as usize..]);
        assert!(
            tail_peak < sustain_peak * 0.15,
            "voice released after note-off (tail {tail_peak} vs sustain {sustain_peak})"
        );
    }

    /// `all_notes_off` (discontinuity seek) releases a held Zyn voice whose
    /// note-off was skipped — no hung notes (CC 123 path).
    #[test]
    fn zyn_all_notes_off_releases_held_voices() {
        const RATE: u32 = 48_000;
        let reg = shared_registry();
        let Some(instance_id) = activate_zyn(&reg) else { return };

        // Note held far beyond what we render: the off never arrives.
        let note = MidiNote { tick: 0, length_ticks: 96_000, key: 64, velocity: 110, channel: 0, note_id: crate::ids::NoteId(0) };
        let (store, midi) = store_with_clip("zyn-hold", &format!("plugin:{instance_id}"), note, 96_000);

        let mut nodes = LiveNodeRegistry::default();
        let mut tracks: Vec<RtTrack> = Vec::new();
        let slots = crate::audio::types::derive_slots(&store.tracks);
        append_from(&midi, &store, &slots, RATE, None, &mut nodes, &mut tracks);
        assert_eq!(nodes.key_of("zyn-hold"), Some(format!("plugin:{instance_id}@{RATE}").as_str()));
        let mut g = RtGraph::new(tracks, 2, Arc::new(ParamTable::default()));

        let sounding = mono_of(&render_from(&mut g, 0, RATE as usize / 2, RATE, false));
        let held_peak = peak(&sounding[4_000..]);
        assert!(held_peak > 0.01, "held note sounds");

        // Seek far past every event with discontinuity=true -> all_notes_off.
        let after = mono_of(&render_from(&mut g, 40_000_000, 2 * RATE as usize, RATE, true));
        let tail = peak(&after[RATE as usize..]);
        assert!(
            tail < held_peak * 0.1,
            "all_notes_off released the voice (tail {tail} vs held {held_peak})"
        );
    }

    /// Engine-facing failure path: an instance the registry knows but the
    /// host does not (e.g. host state lost) yields None from
    /// `live_node_for` — the caller's PolySynth fallback keeps MIDI audible.
    #[test]
    fn live_node_for_falls_back_when_host_has_no_instance() {
        let reg = shared_registry();
        let orphan = crate::plugins::PluginInstanceInfo {
            id: "orphan-lv2".into(),
            uid: "lv2:urn:aura-test:orphan".into(),
            name: "Orphan".into(),
            format: "lv2".into(),
            status: "active".into(),
            track_id: None,
        };
        reg.lock().instances.insert(orphan.id.clone(), orphan);
        assert!(
            crate::plugins::live_node_for("orphan-lv2", 48_000).is_none(),
            "unresolvable active LV2 instance -> None (PolySynth fallback upstream)"
        );
        reg.lock().instances.remove("orphan-lv2");
    }
}
