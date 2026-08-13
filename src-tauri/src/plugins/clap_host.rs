//! Real CLAP hosting (phase 3, zone P1) — clack-host over the plugin main
//! thread ([`host::plugin_main`]) plus the RT node ([`ClapNode`]) behind the
//! `LiveInstrument` seam (ARCHITECTURE §15).
//!
//! Thread map (CLAP spec brackets):
//! * `[main-thread]` — entry load, `PluginInstance::new/activate/deactivate`,
//!   params enumeration/flush, state save/load, `on_main_thread` callbacks:
//!   all inside closures shipped to the `aura-plugin-host` thread, which owns
//!   every `PluginEntry` + `PluginInstance` in its [`ClapWorld`] slot.
//! * `[audio-thread]` — `start_processing`/`process`/`stop_processing` run
//!   wherever the node runs: the engine RT thread during playback, the
//!   engine control thread when a node is dropped (stop before hand-back —
//!   the one deliberate spec bend, matching common host practice).
//! * Control→RT parameter traffic uses a wait-free rtrb ring
//!   (`sampler_engine::NoteCmd` pattern): [`set_params`] pushes on the main
//!   thread, [`ClapNode::process`] drains into sample-time 0 events.
//!
//! Lifecycle: `instantiate` (uid → entry/factory/instance, port + param
//! negotiation, status "active") → `activate_node(rate)` (activate + move
//! the audio processor into a [`ClapNode`]) → node dropped on rebuild/unbind
//! (processor posted back, instance deactivated, ready to re-activate — a
//! rate change re-activates this way; the registry key carries the rate) →
//! `remove` (graveyard until an outstanding node returns its processor, so
//! instances are always destroyed cleanly on the main thread).

use std::collections::HashMap;
use std::ffi::CString;

use clack_extensions::audio_ports::{AudioPortFlags, AudioPortInfoBuffer, PluginAudioPorts};
use clack_extensions::latency::PluginLatency;
use clack_extensions::note_ports::{NoteDialect, NotePortInfoBuffer, PluginNotePorts};
use clack_extensions::params::{ParamInfoFlags, ParamInfoBuffer, PluginParams};
use clack_extensions::state::PluginState as PluginStateExt;
use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent, ParamValueEvent};
use clack_host::events::Match;
use clack_host::prelude::*;
use clack_host::utils::Cookie;

use crate::audio::dsp::{AudioProcessor as AuraAudioProcessor, LiveInstrument, ProcessBlock};
use crate::audio::rt::MAX_LIVE_BLOCK;
use crate::midi::synth::BlockNoteEvent;

use super::descriptor::ParamInfo;
use super::host::{plugin_main, MainCtx};
use super::ParamChange;

/// Max note events buffered per block (mirror of the other live nodes).
const MAX_NODE_EVENTS: usize = 256;
/// Param ring capacity (control -> RT).
const PARAM_RING_CAP: usize = 256;

const SLOT: &str = "clap";

// ---------------------------------------------------------------------------
// Host handlers (clack)
// ---------------------------------------------------------------------------

struct AuraShared {
    callback_requested: std::sync::atomic::AtomicBool,
}

impl<'a> SharedHandler<'a> for AuraShared {
    fn request_restart(&self) {
        // v1: the engine re-activates on rate changes anyway; a plugin-side
        // restart request is honored on the next node cycle.
    }
    fn request_process(&self) {
        // Instrument nodes are always processed while their track is live.
    }
    fn request_callback(&self) {
        self.callback_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

struct AuraClapHost;

impl HostHandlers for AuraClapHost {
    type Shared<'a> = AuraShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
}

fn host_info() -> HostInfo {
    HostInfo::new("AURA", "AURA", "https://aura.invalid", env!("CARGO_PKG_VERSION"))
        .expect("static host info")
}

// ---------------------------------------------------------------------------
// Main-thread world (lives in the plugin main thread's "clap" slot)
// ---------------------------------------------------------------------------

/// Negotiated port layout (fixed at instantiation).
#[derive(Clone, Default)]
struct PortLayout {
    /// Channel count per audio input port (fed silence — instruments only).
    inputs: Vec<usize>,
    /// Channel count per audio output port.
    outputs: Vec<usize>,
    /// Index of the main stereo output port in `outputs`.
    main_out: usize,
    /// Note input port INDEX (CLAP events address ports by index).
    note_port: u16,
    /// True = the port only understands raw MIDI 1.0 (no CLAP note events).
    midi_dialect: bool,
}

struct Hosted {
    instance: PluginInstance<AuraClapHost>,
    layout: PortLayout,
    /// Param metadata + last known values (mirrored into the registry).
    params: Vec<ParamInfo>,
    /// Param id -> plugin cookie (raw pointer as usize; only ever turned
    /// back into a pointer inside events delivered to the same plugin).
    cookies: HashMap<u32, usize>,
    /// Producer of the control->RT param ring while a node is out.
    param_tx: Option<rtrb::Producer<ParamCmd>>,
    /// True while a `ClapNode` holds the audio processor.
    node_out: bool,
}

#[derive(Default)]
struct ClapWorld {
    entries: HashMap<String, PluginEntry>,
    hosted: HashMap<String, Hosted>,
    /// Removed instances whose node still holds the audio processor; they
    /// are destroyed when the processor comes home.
    graveyard: HashMap<String, Hosted>,
}

fn world(ctx: &mut MainCtx) -> &mut ClapWorld {
    ctx.slot_mut::<ClapWorld>(SLOT)
}

/// Ensure the periodic `on_main_thread` callback pump is registered.
fn ensure_ticker(ctx: &mut MainCtx) {
    ctx.add_ticker(
        SLOT,
        Box::new(|ctx| {
            let w = world(ctx);
            for hosted in w.hosted.values_mut().chain(w.graveyard.values_mut()) {
                let requested = hosted.instance.access_shared_handler(|s| {
                    s.callback_requested
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                });
                if requested {
                    hosted.instance.call_on_main_thread_callback();
                }
            }
        }),
    );
}

fn parse_clap_uid(uid: &str) -> Result<(String, String), String> {
    let rest = uid
        .strip_prefix("clap:")
        .ok_or_else(|| format!("not a clap uid: {uid}"))?;
    // First '#' separates the bundle path from the plugin id: plugin ids may
    // themselves contain '#' (Cardinal: `studio.kx.distrho.cardinal#synth`),
    // so split at the first one, not the last.
    let (path, id) = rest
        .split_once('#')
        .ok_or_else(|| format!("malformed clap uid (missing #id): {uid}"))?;
    Ok((path.to_string(), id.to_string()))
}

impl ClapWorld {
    fn entry(&mut self, path: &str) -> Result<&PluginEntry, String> {
        if !self.entries.contains_key(path) {
            // SAFETY: loading a CLAP bundle runs foreign init code — the
            // in-process risk documented in ARCHITECTURE §15.3 (the bundle
            // was already vetted by the sacrificial scan subprocess).
            let entry = unsafe { PluginEntry::load(path) }
                .map_err(|e| format!("failed to load {path}: {e}"))?;
            self.entries.insert(path.to_string(), entry);
        }
        Ok(self.entries.get(path).expect("just inserted"))
    }

    fn hosted_mut(&mut self, instance_id: &str) -> Result<&mut Hosted, String> {
        self.hosted
            .get_mut(instance_id)
            .ok_or_else(|| format!("clap: unknown instance {instance_id}"))
    }

    fn instantiate(&mut self, instance_id: &str, uid: &str) -> Result<Vec<ParamInfo>, String> {
        let (path, clap_id) = parse_clap_uid(uid)?;
        let entry = self.entry(&path)?.clone();
        let c_id = CString::new(clap_id.clone()).map_err(|_| "bad plugin id".to_string())?;
        let mut instance = PluginInstance::<AuraClapHost>::new(
            |_| AuraShared { callback_requested: std::sync::atomic::AtomicBool::new(false) },
            |_| (),
            &entry,
            &c_id,
            &host_info(),
        )
        .map_err(|e| format!("{clap_id}: instantiation failed: {e}"))?;

        let layout = negotiate_ports(&mut instance, &clap_id)?;
        let (params, cookies) = enumerate_params(&mut instance);

        self.hosted.insert(
            instance_id.to_string(),
            Hosted { instance, layout, params: params.clone(), cookies, param_tx: None, node_out: false },
        );
        log::info!("clap: instantiated {clap_id} as {instance_id} ({} params)", params.len());
        Ok(params)
    }

    fn activate_node(&mut self, instance_id: &str, rate: u32) -> Result<ClapNode, String> {
        let hosted = self.hosted_mut(instance_id)?;
        if hosted.node_out {
            return Err(format!(
                "clap: instance {instance_id} already has a live node out (old node not yet retired)"
            ));
        }
        let config = PluginAudioConfiguration {
            sample_rate: rate as f64,
            min_frames_count: 1,
            max_frames_count: MAX_LIVE_BLOCK as u32,
        };
        let processor = hosted
            .instance
            .activate(|_, _| (), config)
            .map_err(|e| format!("clap: activation failed for {instance_id}: {e}"))?;

        // Latency ext: read + log (PDC lands with the graph compiler round).
        if let Some(lat) = hosted.instance.plugin_shared_handle().get_extension::<PluginLatency>()
        {
            let samples = lat.get(&mut hosted.instance.plugin_handle());
            if samples > 0 {
                log::info!("clap: {instance_id} reports {samples} samples latency (uncompensated in v1)");
            }
        }

        let (param_tx, param_rx) = rtrb::RingBuffer::new(PARAM_RING_CAP);
        hosted.param_tx = Some(param_tx);
        hosted.node_out = true;

        let layout = hosted.layout.clone();
        let total_in: usize = layout.inputs.iter().sum::<usize>().max(1);
        let total_out: usize = layout.outputs.iter().sum::<usize>().max(1);
        let alloc = |ports: &[usize]| -> Vec<Vec<Vec<f32>>> {
            ports
                .iter()
                .map(|&ch| (0..ch).map(|_| vec![0.0f32; MAX_LIVE_BLOCK]).collect())
                .collect()
        };
        Ok(ClapNode {
            proc: Some(processor.into()),
            in_ports: AudioPorts::with_capacity(total_in, layout.inputs.len()),
            out_ports: AudioPorts::with_capacity(total_out, layout.outputs.len()),
            in_bufs: alloc(&layout.inputs),
            out_bufs: alloc(&layout.outputs),
            ev_in: EventBuffer::with_capacity(MAX_NODE_EVENTS + PARAM_RING_CAP + 8),
            ev_out: EventBuffer::with_capacity(MAX_NODE_EVENTS),
            notes: [BlockNoteEvent { offset: 0, key: 0, velocity: 0 }; MAX_NODE_EVENTS],
            n_notes: 0,
            param_rx,
            pending_all_off: false,
            steady: 0,
            errors: 0,
            layout,
            instance_id: instance_id.to_string(),
        })
    }

    /// A node handed its (stopped) audio processor back — deactivate, and
    /// finish burying the instance if it was removed meanwhile.
    fn return_processor(
        &mut self,
        instance_id: &str,
        stopped: StoppedPluginAudioProcessor<AuraClapHost>,
    ) {
        if let Some(hosted) = self.hosted.get_mut(instance_id) {
            hosted.instance.deactivate(stopped);
            hosted.node_out = false;
            hosted.param_tx = None;
        } else if let Some(mut hosted) = self.graveyard.remove(instance_id) {
            hosted.instance.deactivate(stopped);
            log::info!("clap: destroyed removed instance {instance_id} after node retirement");
        }
        // else: unknown — the processor Arc is simply dropped (clack keeps
        // this memory-safe; the instance leaks by design in that edge).
    }

    fn remove(&mut self, instance_id: &str) -> Result<(), String> {
        let hosted = self
            .hosted
            .remove(instance_id)
            .ok_or_else(|| format!("clap: unknown instance {instance_id}"))?;
        if hosted.node_out {
            // Node still holds the processor; destroy when it returns.
            self.graveyard.insert(instance_id.to_string(), hosted);
        }
        Ok(())
    }

    fn set_params(&mut self, instance_id: &str, changes: &[ParamChange]) -> Result<Vec<ParamInfo>, String> {
        let hosted = self.hosted_mut(instance_id)?;
        let mut flushed: Vec<ParamValueEvent> = Vec::new();
        for ch in changes {
            let Some(meta) = hosted.params.iter_mut().find(|p| p.id == ch.id) else {
                log::warn!("clap: {instance_id} has no param {}", ch.id);
                continue;
            };
            let value = ch.value.clamp(meta.min, meta.max);
            meta.value = value;
            let Some(clap_id) = ClapId::from_raw(ch.id) else { continue };
            let cookie = Cookie::from_raw(
                hosted.cookies.get(&ch.id).copied().unwrap_or(0) as *mut std::ffi::c_void,
            );
            let ev = ParamValueEvent::new(0, clap_id, Pckn::match_all(), value, cookie);
            match hosted.param_tx.as_mut() {
                Some(tx) => {
                    if tx
                        .push(ParamCmd {
                            id: ch.id,
                            value,
                            cookie: cookie.as_raw() as usize,
                        })
                        .is_err()
                    {
                        log::warn!("clap: param ring full for {instance_id}; change dropped");
                    }
                }
                None => flushed.push(ev),
            }
        }
        if !flushed.is_empty() {
            if let Some(params_ext) =
                hosted.instance.plugin_shared_handle().get_extension::<PluginParams>()
            {
                let mut buf = EventBuffer::with_capacity(flushed.len());
                for ev in &flushed {
                    buf.push(ev);
                }
                let input = InputEvents::from_buffer(&buf);
                let mut out_buf = EventBuffer::new();
                let mut output = OutputEvents::from_buffer(&mut out_buf);
                if let Some(mut handle) = hosted.instance.inactive_plugin_handle() {
                    params_ext.flush(&mut handle, &input, &mut output);
                }
            }
        }
        Ok(hosted.params.clone())
    }

    fn get_params(&mut self, instance_id: &str) -> Result<Vec<ParamInfo>, String> {
        let hosted = self.hosted_mut(instance_id)?;
        // Refresh current values from the plugin (main-thread safe even
        // while active) so plugin-side changes reach the generic UI.
        if let Some(params_ext) =
            hosted.instance.plugin_shared_handle().get_extension::<PluginParams>()
        {
            let mut handle = hosted.instance.plugin_handle();
            for p in hosted.params.iter_mut() {
                if let Some(clap_id) = ClapId::from_raw(p.id) {
                    if let Some(v) = params_ext.get_value(&mut handle, clap_id) {
                        p.value = v;
                    }
                }
            }
        }
        Ok(hosted.params.clone())
    }

    fn save_state(&mut self, instance_id: &str) -> Result<Vec<u8>, String> {
        let hosted = self.hosted_mut(instance_id)?;
        let ext = hosted
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginStateExt>()
            .ok_or_else(|| format!("clap: {instance_id} has no state extension"))?;
        let mut out = Vec::new();
        ext.save(&mut hosted.instance.plugin_handle(), &mut out)
            .map_err(|e| format!("clap: state save failed for {instance_id}: {e}"))?;
        Ok(out)
    }

    fn load_state(&mut self, instance_id: &str, data: &[u8]) -> Result<(), String> {
        let hosted = self.hosted_mut(instance_id)?;
        let ext = hosted
            .instance
            .plugin_shared_handle()
            .get_extension::<PluginStateExt>()
            .ok_or_else(|| format!("clap: {instance_id} has no state extension"))?;
        let mut reader = std::io::Cursor::new(data);
        ext.load(&mut hosted.instance.plugin_handle(), &mut reader)
            .map_err(|e| format!("clap: state load failed for {instance_id}: {e}"))?;
        Ok(())
    }
}

/// Audio + note port negotiation: v1 hosts mono/stereo-out instruments with
/// a note input (mono is up-mixed — the node always delivers stereo to the
/// graph); anything else is rejected politely (clear error, no crash).
fn negotiate_ports(
    instance: &mut PluginInstance<AuraClapHost>,
    clap_id: &str,
) -> Result<PortLayout, String> {
    let audio_ext = instance
        .plugin_shared_handle()
        .get_extension::<PluginAudioPorts>()
        .ok_or_else(|| format!("{clap_id}: no audio-ports extension"))?;
    let note_ext = instance
        .plugin_shared_handle()
        .get_extension::<PluginNotePorts>();

    let mut handle = instance.plugin_handle();
    let mut buf = AudioPortInfoBuffer::new();
    let n_out = audio_ext.count(&mut handle, false);
    let n_in = audio_ext.count(&mut handle, true);
    let mut outputs = Vec::new();
    let mut main_out = None;
    for i in 0..n_out {
        if let Some(info) = audio_ext.get(&mut handle, i, false, &mut buf) {
            if info.flags.contains(AudioPortFlags::IS_MAIN) && main_out.is_none() {
                main_out = Some(i as usize);
            }
            outputs.push(info.channel_count as usize);
        } else {
            outputs.push(0);
        }
    }
    let main_out = main_out.unwrap_or(0);
    match outputs.get(main_out) {
        // Stereo passes through; mono is up-mixed to both scratch channels
        // (the node always delivers stereo to the graph).
        Some(1) | Some(2) => {}
        Some(n) => {
            return Err(format!(
                "{clap_id}: v1 hosts mono/stereo-out instruments only (main output has {n} channels)"
            ))
        }
        None => return Err(format!("{clap_id}: no audio output port")),
    }
    let mut inputs = Vec::new();
    for i in 0..n_in {
        let ch = audio_ext
            .get(&mut handle, i, true, &mut buf)
            .map(|info| info.channel_count as usize)
            .unwrap_or(0);
        inputs.push(ch);
    }

    let note_ext = note_ext.ok_or_else(|| {
        format!("{clap_id}: no note-ports extension — not hostable as an instrument")
    })?;
    let n_note_in = note_ext.count(&mut handle, true);
    if n_note_in == 0 {
        return Err(format!("{clap_id}: no note input port — not an instrument"));
    }
    let mut nbuf = NotePortInfoBuffer::new();
    let midi_dialect = match note_ext.get(&mut handle, 0, true, &mut nbuf) {
        Some(info) if info.supported_dialects.supports(NoteDialect::Clap) => false,
        Some(info) if info.supported_dialects.supports(NoteDialect::Midi) => true,
        Some(_) => {
            return Err(format!("{clap_id}: note port supports neither CLAP nor MIDI dialect"))
        }
        // Port info unreadable: assume the universal default (CLAP dialect).
        None => false,
    };

    Ok(PortLayout { inputs, outputs, main_out, note_port: 0, midi_dialect })
}

/// Enumerate the params extension into wire `ParamInfo`s + cookie map.
fn enumerate_params(
    instance: &mut PluginInstance<AuraClapHost>,
) -> (Vec<ParamInfo>, HashMap<u32, usize>) {
    let Some(params_ext) = instance.plugin_shared_handle().get_extension::<PluginParams>() else {
        return (Vec::new(), HashMap::new());
    };
    let mut handle = instance.plugin_handle();
    let mut out = Vec::new();
    let mut cookies = HashMap::new();
    let mut buf = ParamInfoBuffer::new();
    let count = params_ext.count(&mut handle);
    for i in 0..count {
        let Some(info) = params_ext.get_info(&mut handle, i, &mut buf) else { continue };
        let id = info.id.get();
        let steps = if info.flags.contains(ParamInfoFlags::IS_STEPPED) {
            ((info.max_value - info.min_value).abs().round() as u32).saturating_add(1)
        } else {
            0
        };
        let name = String::from_utf8_lossy(info.name).into_owned();
        let (min, max, default) = (info.min_value, info.max_value, info.default_value);
        cookies.insert(id, info.cookie.as_raw() as usize);
        let value = params_ext
            .get_value(&mut handle, info.id)
            .unwrap_or(default);
        out.push(ParamInfo { id, name, min, max, default, value, steps });
    }
    (out, cookies)
}

// ---------------------------------------------------------------------------
// Control-facing API (thin wrappers posting onto the plugin main thread)
// ---------------------------------------------------------------------------

/// Instantiate `uid` under `instance_id`. Returns the real parameter list
/// (registry mirror). Fails politely on unloadable bundles / non-stereo /
/// note-less plugins.
pub fn instantiate(instance_id: &str, uid: &str) -> Result<Vec<ParamInfo>, String> {
    let (id, uid) = (instance_id.to_string(), uid.to_string());
    plugin_main().run(move |ctx| {
        ensure_ticker(ctx);
        world(ctx).instantiate(&id, &uid)
    })?
}

/// Destroy an instance (deferred while its node is still out).
pub fn remove(instance_id: &str) -> Result<(), String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).remove(&id))?
}

/// Activate at `rate` and hand out the RT node. ENGINE CONTROL THREAD ONLY
/// (blocks briefly on the plugin main thread).
pub fn activate_node(instance_id: &str, rate: u32) -> Result<Box<dyn LiveInstrument>, String> {
    let id = instance_id.to_string();
    let node = plugin_main().run(move |ctx| world(ctx).activate_node(&id, rate))??;
    Ok(Box::new(node))
}

/// Batched param set: mirrors values + forwards through the RT ring (active
/// node) or a main-thread flush (inactive). Returns the updated list.
pub fn set_params(instance_id: &str, changes: Vec<ParamChange>) -> Result<Vec<ParamInfo>, String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).set_params(&id, &changes))?
}

/// Current param list with values refreshed from the plugin.
pub fn get_params(instance_id: &str) -> Result<Vec<ParamInfo>, String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).get_params(&id))?
}

/// True when the plugin main thread hosts `instance_id` as a CLAP instance
/// (the state bridge's format probe — no registry lock involved).
pub fn has_instance(instance_id: &str) -> Result<bool, String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).hosted.contains_key(&id))
}

/// Opaque state blob via the CLAP state extension (persisted by zone P4).
pub fn save_state(instance_id: &str) -> Result<Vec<u8>, String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).save_state(&id))?
}

pub fn load_state(instance_id: &str, data: Vec<u8>) -> Result<(), String> {
    let id = instance_id.to_string();
    plugin_main().run(move |ctx| world(ctx).load_state(&id, &data))?
}

// ---------------------------------------------------------------------------
// The RT node
// ---------------------------------------------------------------------------

/// Control -> RT parameter command (POD, wait-free ring).
#[derive(Debug, Clone, Copy)]
struct ParamCmd {
    id: u32,
    value: f64,
    /// Plugin param cookie as usize (raw pointer; delivered back to the
    /// same plugin inside the param event).
    cookie: usize,
}

/// A live CLAP instrument node. Owns the started audio processor plus every
/// buffer `process` needs — preallocated at activation on the plugin main
/// thread; the RT path never allocates or locks (§2.1).
pub struct ClapNode {
    proc: Option<PluginAudioProcessor<AuraClapHost>>,
    in_ports: AudioPorts,
    out_ports: AudioPorts,
    /// Silent input feeds, per port per channel (instruments only).
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    ev_in: EventBuffer,
    ev_out: EventBuffer,
    notes: [BlockNoteEvent; MAX_NODE_EVENTS],
    n_notes: usize,
    param_rx: rtrb::Consumer<ParamCmd>,
    pending_all_off: bool,
    steady: u64,
    /// Process-error count (RT-safe diagnostics; logged on drop).
    errors: u64,
    layout: PortLayout,
    instance_id: String,
}

impl ClapNode {
    #[inline]
    fn push_note(&mut self, ev: &BlockNoteEvent, frames: usize) {
        let time = ev.offset.min(frames.saturating_sub(1) as u32);
        let port = self.layout.note_port;
        if self.layout.midi_dialect {
            let msg = if ev.velocity > 0 {
                [0x90, ev.key, ev.velocity]
            } else {
                [0x80, ev.key, 64]
            };
            self.ev_in.push(&MidiEvent::new(time, port, msg));
        } else {
            let pckn = Pckn::new(port, 0u16, ev.key as u16, Match::All);
            if ev.velocity > 0 {
                self.ev_in
                    .push(&NoteOnEvent::new(time, pckn, ev.velocity as f64 / 127.0));
            } else {
                self.ev_in.push(&NoteOffEvent::new(time, pckn, 0.0));
            }
        }
    }

    pub fn process_errors(&self) -> u64 {
        self.errors
    }
}

impl AuraAudioProcessor for ClapNode {
    fn prepare(&mut self, _sample_rate: u32, _max_block: usize) {
        // Rate and block size are fixed at activation (the registry key
        // carries the rate; a rate change builds a new node).
    }

    /// ADDS the plugin's main stereo output into the zeroed scratch. §2.1:
    /// all buffers preallocated; event buffers are cleared, not shrunk.
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        let frames = io.frames().min(MAX_LIVE_BLOCK);
        if frames == 0 || io.channels < 2 {
            self.n_notes = 0;
            return;
        }

        // 1. Events for this block, sorted by time: all-off + params at 0,
        //    then the block's note edges (they arrive offset-sorted).
        self.ev_in.clear();
        self.ev_out.clear();
        if self.pending_all_off {
            self.pending_all_off = false;
            if self.layout.midi_dialect {
                // All-notes-off on the channel we play on (CC 123).
                self.ev_in
                    .push(&MidiEvent::new(0, self.layout.note_port, [0xB0, 123, 0]));
            } else {
                self.ev_in.push(&NoteOffEvent::new(0, Pckn::match_all(), 0.0));
            }
        }
        while let Ok(cmd) = self.param_rx.pop() {
            if let Some(clap_id) = ClapId::from_raw(cmd.id) {
                self.ev_in.push(&ParamValueEvent::new(
                    0,
                    clap_id,
                    Pckn::match_all(),
                    cmd.value,
                    Cookie::from_raw(cmd.cookie as *mut std::ffi::c_void),
                ));
            }
        }
        for i in 0..self.n_notes {
            let ev = self.notes[i];
            self.push_note(&ev, frames);
        }
        self.n_notes = 0;

        // 2. Audio buffers (cheap wrappers over the preallocated storage).
        let in_audio = self.in_ports.with_input_buffers(self.in_bufs.iter_mut().map(|chans| {
            AudioPortBuffer {
                latency: 0,
                channels: AudioPortBufferType::f32_input_only(
                    chans
                        .iter_mut()
                        .map(|c| InputChannel::constant(&mut c[..frames])),
                ),
            }
        }));
        let mut out_audio =
            self.out_ports
                .with_output_buffers(self.out_bufs.iter_mut().map(|chans| AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        chans.iter_mut().map(|c| &mut c[..frames]),
                    ),
                }));

        // 3. Run the plugin (start_processing lazily, on the audio thread
        //    as the spec demands).
        let Some(proc_) = self.proc.as_mut() else { return };
        let started = match proc_.ensure_processing_started() {
            Ok(s) => s,
            Err(_) => {
                self.errors += 1;
                return;
            }
        };
        let input_events = InputEvents::from_buffer(&self.ev_in);
        let mut output_events = OutputEvents::from_buffer(&mut self.ev_out);
        match started.process(
            &in_audio,
            &mut out_audio,
            &input_events,
            &mut output_events,
            Some(self.steady),
            None,
        ) {
            Ok(_status) => {
                // 4. Mix the main output into the interleaved stereo
                //    scratch (mono ports feed both channels).
                if let Some(chans) = self.out_bufs.get(self.layout.main_out) {
                    if chans.len() >= 2 {
                        let (l, r) = (&chans[0], &chans[1]);
                        for i in 0..frames {
                            io.samples[i * 2] += l[i];
                            io.samples[i * 2 + 1] += r[i];
                        }
                    } else if let Some(m) = chans.first() {
                        for i in 0..frames {
                            io.samples[i * 2] += m[i];
                            io.samples[i * 2 + 1] += m[i];
                        }
                    }
                }
            }
            Err(_) => self.errors += 1,
        }
        self.steady = self.steady.wrapping_add(frames as u64);
    }

    fn reset(&mut self) {
        self.pending_all_off = true;
    }
}

impl LiveInstrument for ClapNode {
    fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        if self.n_notes >= MAX_NODE_EVENTS {
            return false;
        }
        self.notes[self.n_notes] = ev;
        self.n_notes += 1;
        true
    }

    fn all_notes_off(&mut self) {
        self.pending_all_off = true;
    }
}

impl Drop for ClapNode {
    /// Runs on the engine control thread when the node cell retires: stop
    /// processing and post the processor home for main-thread deactivation.
    fn drop(&mut self) {
        if self.errors > 0 {
            log::warn!(
                "clap: node for {} had {} process errors",
                self.instance_id,
                self.errors
            );
        }
        if let Some(proc_) = self.proc.take() {
            let stopped = proc_.into_stopped();
            let id = std::mem::take(&mut self.instance_id);
            plugin_main().post(move |ctx| world(ctx).return_processor(&id, stopped));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — run against real installed CLAP plugins when present (dev machine:
// dpf-plugins-clap ships the Kars/Nekobi instruments, zam-plugins ships
// stereo effects); every real-plugin test skips cleanly when none are found.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::scan::{clap_search_paths, find_clap_bundles};
    use crate::plugins::scan_worker;
    use crate::plugins::PluginDescriptor;

    /// Scan installed bundles through the sacrificial worker (production
    /// path) — an in-process scan would dlopen EVERY bundle, and Cardinal's
    /// teardown intermittently corrupts the test process (see
    /// `scan_worker::test_worker_command`). The instrument a test picks is
    /// still loaded in-process by `instantiate`, but only that one bundle.
    fn installed() -> Vec<PluginDescriptor> {
        let bundles = find_clap_bundles(&clap_search_paths());
        if bundles.is_empty() {
            return Vec::new();
        }
        scan_worker::scan_with_worker(&bundles, &scan_worker::test_worker_command())
    }

    /// Preferred known-good open instrument (Nekobi > Kars > any instrument).
    fn instrument_uid() -> Option<String> {
        let all = installed();
        for pick in ["Nekobi", "Kars"] {
            if let Some(d) = all.iter().find(|d| d.is_instrument && d.name.contains(pick)) {
                return Some(d.uid.clone());
            }
        }
        // Never fall back to Cardinal in-process (teardown corruption — see
        // `scan_worker::test_worker_command`); it is probed in a subprocess
        // by `plugins::patches` instead.
        all.into_iter()
            .find(|d| d.is_instrument && !d.uid.to_lowercase().contains("cardinal"))
            .map(|d| d.uid)
    }

    fn render_blocks(node: &mut dyn LiveInstrument, blocks: usize, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(blocks * frames * 2);
        for _ in 0..blocks {
            let mut buf = vec![0.0f32; frames * 2];
            let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: 48_000 };
            node.process(&mut io);
            out.extend_from_slice(&buf);
        }
        out
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Full CLAP lifecycle against a real installed instrument: instantiate
    /// (ports + params negotiated) -> activate -> note on renders audio ->
    /// all_notes_off decays -> node retires -> re-activation at another rate
    /// works -> remove. Skips cleanly when no CLAP instrument is installed.
    #[test]
    fn real_instrument_renders_notes_headlessly() {
        let Some(uid) = instrument_uid() else {
            eprintln!("skipping: no CLAP instrument installed");
            return;
        };
        let id = format!("t-{}", uuid::Uuid::new_v4());
        let params = instantiate(&id, &uid).expect("instantiate");
        assert!(!params.is_empty(), "instrument exposes params via params ext");

        let mut node = activate_node(&id, 48_000).expect("activate");
        node.queue_event(BlockNoteEvent { offset: 0, key: 48, velocity: 110 });
        let audio = render_blocks(node.as_mut(), 20, 512);
        assert!(
            peak(&audio) > 0.005,
            "note-on renders audible audio (peak {})",
            peak(&audio)
        );

        // Release everything, give the tail time to die down.
        node.all_notes_off();
        let mut tail = render_blocks(node.as_mut(), 50, 512);
        let last = tail.split_off(tail.len() - 512 * 2);
        assert!(
            peak(&last) < peak(&audio),
            "all_notes_off decays the output (last-block peak {} vs {})",
            peak(&last),
            peak(&audio)
        );

        // While a node is out, a second activation must fail politely.
        assert!(activate_node(&id, 48_000).is_err(), "double activation rejected");

        // Retire the node; the processor returns home and the instance can
        // re-activate at a different rate (rate-change path).
        drop(node);
        plugin_main().run(|_| ()).unwrap(); // barrier: drain the post queue
        let node2 = activate_node(&id, 44_100).expect("re-activate at new rate");
        drop(node2);
        plugin_main().run(|_| ()).unwrap();
        remove(&id).expect("remove");
        assert!(activate_node(&id, 48_000).is_err(), "removed instance is gone");
    }

    /// Parameter bridge round-trip: values set while INACTIVE go through the
    /// main-thread flush and read back via the plugin's own get_value; values
    /// set while a node is live go through the wait-free ring and land after
    /// a process call. State save/load round-trips through the state ext.
    #[test]
    fn param_and_state_bridge_roundtrip() {
        let Some(uid) = instrument_uid() else {
            eprintln!("skipping: no CLAP instrument installed");
            return;
        };
        let id = format!("t-{}", uuid::Uuid::new_v4());
        let params = instantiate(&id, &uid).expect("instantiate");
        let p = params
            .iter()
            .find(|p| p.max > p.min && p.steps == 0)
            .or_else(|| params.first())
            .expect("has a param")
            .clone();
        let target = (p.min + p.max) / 2.0;

        // Inactive path (flush).
        let updated =
            set_params(&id, vec![ParamChange { id: p.id, value: target }]).expect("set");
        let mirrored = updated.iter().find(|q| q.id == p.id).unwrap().value;
        assert!((mirrored - target).abs() < 1e-9, "mirror carries the clamped value");
        let live = get_params(&id).expect("get");
        let got = live.iter().find(|q| q.id == p.id).unwrap().value;
        assert!(
            (got - target).abs() <= (p.max - p.min) * 0.01 + 1e-6,
            "plugin-side value {} ~ set value {} (inactive flush path)",
            got,
            target
        );

        // Active path (ring -> process).
        let mut node = activate_node(&id, 48_000).expect("activate");
        let target2 = p.min + (p.max - p.min) * 0.75;
        set_params(&id, vec![ParamChange { id: p.id, value: target2 }]).expect("set active");
        let _ = render_blocks(node.as_mut(), 2, 512); // deliver the ring event
        let live = get_params(&id).expect("get");
        let got = live.iter().find(|q| q.id == p.id).unwrap().value;
        assert!(
            (got - target2).abs() <= (p.max - p.min) * 0.01 + 1e-6,
            "plugin-side value {} ~ set value {} (RT ring path)",
            got,
            target2
        );

        // State ext round-trip (DPF plugins implement clap.state).
        match save_state(&id) {
            Ok(blob) => {
                assert!(!blob.is_empty(), "state blob non-empty");
                load_state(&id, blob).expect("state load");
            }
            Err(e) => eprintln!("note: state ext unavailable ({e})"),
        }

        drop(node);
        plugin_main().run(|_| ()).unwrap();
        remove(&id).expect("remove");
    }

    /// Graceful failure: effects (no note port) and unloadable bundles are
    /// rejected with an error — no panic, no zombie state.
    #[test]
    fn rejects_effects_and_bad_bundles_politely() {
        let bad = format!("t-{}", uuid::Uuid::new_v4());
        assert!(instantiate(&bad, "clap:/nonexistent/x.clap#no.such.plugin").is_err());
        assert!(instantiate(&bad, "lv2:not-a-clap-uid").is_err());

        // Skip Cardinal deliberately: negotiation-rejecting it still dlopens
        // the bundle, and Cardinal's teardown corrupts the test process at
        // exit (see `scan_worker::test_worker_command`).
        if let Some(fx) = installed()
            .iter()
            .find(|d| !d.is_instrument && !d.uid.to_lowercase().contains("cardinal"))
        {
            let id = format!("t-{}", uuid::Uuid::new_v4());
            let err = instantiate(&id, &fx.uid).expect_err("effect rejected");
            assert!(
                err.contains("note") || err.contains("stereo") || err.contains("channels"),
                "polite negotiation error, got: {err}"
            );
            assert!(activate_node(&id, 48_000).is_err(), "no zombie instance left behind");
        } else {
            eprintln!("note: no CLAP effect installed; negotiation-reject case skipped");
        }
    }

    /// THE seam test (CLAP flavor of contract 7): a midi track bound to
    /// `plugin:<instance>` renders NON-SILENT audio through the real
    /// `mixer::render` graph path, headless; the registry key carries the
    /// instance + rate.
    #[test]
    fn plugin_bound_midi_track_renders_through_graph() {
        use crate::audio::mixer;
        use crate::audio::rt::{ParamTable, RtGraph, RtTrack};
        use crate::audio::transport::LoopSpec;
        use crate::audio::types::{Store, TrackState};
        use crate::midi::playback::{append_from, LiveNodeRegistry};
        use crate::midi::types::{MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
        use crate::midi::MidiStore;
        use crate::plugins::{
            instantiate_and_activate, register_registry, registered_registry, PluginRegistry,
        };
        use std::sync::Arc;

        let Some(uid) = instrument_uid() else {
            eprintln!("skipping: no CLAP instrument installed");
            return;
        };

        // Global registry (shared with other tests; first registration wins).
        register_registry(Arc::new(parking_lot::Mutex::new(PluginRegistry::default())));
        let registry = registered_registry().unwrap().clone();
        {
            let mut reg = registry.lock();
            let mut scanned = reg.scanned.take().unwrap_or_default();
            scanned.extend(installed());
            reg.scanned = Some(scanned);
        }
        let info = instantiate_and_activate(&registry, &uid).expect("instantiate");
        assert_eq!(info.status, "active", "contract 4: stub -> active");

        // One midi track bound to the plugin instance, playing C3 for a beat.
        let mut store = Store::default();
        let mut t = TrackState {
            id: "m1".into(),
            name: "m1".into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        };
        t.instrument_id = Some(format!("plugin:{}", info.id));
        store.tracks.push(t);
        let midi = MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            clips: vec![MidiClip {
                id: crate::ids::ClipId::mint(),
                track_id: "m1".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: 1920,
                notes: vec![MidiNote {
                    tick: 0,
                    length_ticks: 1920,
                    key: 48,
                    velocity: 110,
                    channel: 0,
                    note_id: crate::ids::NoteId(0),
                }],
                next_note_id: 1,
            }],
            loaded_dir: None,
            dirty: false,
        };

        let mut nodes = LiveNodeRegistry::default();
        let mut tracks: Vec<RtTrack> = Vec::new();
        let slots = crate::audio::types::derive_slots(&store.tracks);
        append_from(&midi, &store, &slots, 48_000, None, &mut nodes, &mut tracks);
        assert_eq!(tracks.len(), 1);
        assert_eq!(
            nodes.key_of("m1"),
            Some(format!("plugin:{}@48000", info.id).as_str()),
            "plugin node keyed by instance + rate"
        );

        // Render 1 s through the REAL RT path.
        let mut g = RtGraph::new(tracks, 1, Arc::new(ParamTable::default()));
        let mut out = vec![0.0f32; 48_000 * 2];
        let mut pos = 0u64;
        for chunk in out.chunks_mut(512 * 2) {
            mixer::render(&mut g, pos, &LoopSpec::OFF, chunk, 2, 48_000, false, None);
            pos += (chunk.len() / 2) as u64;
        }
        assert!(
            peak(&out) > 0.005,
            "plugin-bound midi track is audible through mixer::render (peak {})",
            peak(&out)
        );

        // Retire the graph (returns the processor), then clean up.
        drop(g);
        plugin_main().run(|_| ()).unwrap();
        let _ = registry.lock().remove(&info.id);
        let _ = remove(&info.id);
    }
}
