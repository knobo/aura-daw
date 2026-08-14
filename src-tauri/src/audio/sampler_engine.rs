//! Sampler playback engine — OWNED BY THE SAMPLER AGENT (phase 2, zone D).
//!
//! Control-plane half: [`compile`] turns a parsed [`SfzInstrument`] into an
//! immutable [`CompiledInstrument`] — every sample file decoded (hound WAV,
//! whole-file preload v1) and resampled to the ENGINE rate at LOAD time.
//! Nothing here happens on the RT thread.
//!
//! RT half: [`SamplerNode`] implements [`AudioProcessor`] over a preallocated
//! [`VoicePool`] (64 voices, oldest stealing) and consumes note input from
//! two RT-safe sources:
//!
//! 1. **Block events** ([`BlockNoteEvent`], the frozen zone-C contract in
//!    `midi/synth.rs`): the graph renderer calls
//!    [`SamplerNode::set_block_events`] with pre-converted sample-offset
//!    events before `process` — sample-accurate note starts inside the block.
//! 2. **A wait-free command queue** ([`NoteCmd`] over rtrb): control-plane
//!    triggered notes (`sampler_preview_note`, MCP audition) with an
//!    auto-release hold time.
//!
//! `process` ADDS into the buffer (an instrument node fills the zeroed
//! per-track buffer it is handed) and obeys the §2.1 contract: no alloc, no
//! locks, no syscalls — all state is preallocated in `new`/`prepare`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::dsp::{linear_resample, AudioProcessor, ProcessBlock};
use super::engine::load_wav;
use super::mixer::db_to_linear;
use super::sampler::{resolve_sample_path, SfzInstrument};
use super::sampler_voice::{CompiledRegion, SampleData, VoicePool, HOLD_MANUAL};
use crate::midi::synth::BlockNoteEvent;

pub use super::sampler::MAX_VOICES;

/// Fixed capacity of the per-block note-event buffer.
pub const MAX_BLOCK_EVENTS: usize = 256;

// ---------------------------------------------------------------------------
// Compiled instrument (immutable snapshot shared with the RT thread)
// ---------------------------------------------------------------------------

/// A fully loaded instrument: regions + decoded sample data at `rate`.
/// Immutable after `compile`; cheap to clone (samples are `Arc`-shared).
#[derive(Clone)]
pub struct CompiledInstrument {
    pub id: String,
    pub name: String,
    /// Rate the samples were resampled to at load time (engine rate).
    pub rate: u32,
    pub regions: Arc<Vec<CompiledRegion>>,
}

impl CompiledInstrument {
    pub fn key_range(&self) -> (u8, u8) {
        (
            self.regions.iter().map(|r| r.lokey).min().unwrap_or(0),
            self.regions.iter().map(|r| r.hikey).max().unwrap_or(127),
        )
    }
}

/// SFZ `pan` (-100..100) to per-channel linear gains, center = unity on both
/// (simple linear law — full volume at center keeps region gain math obvious;
/// track-level pan uses the constant-power law in `mixer`).
fn sfz_pan_gains(pan: f32) -> (f32, f32) {
    let p = (pan / 100.0).clamp(-1.0, 1.0);
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}

/// Decode + resample every region sample and build the immutable compiled
/// form. Control-plane only (file IO + allocation). Sample files are
/// deduplicated: N regions over one file share one decoded buffer.
pub fn compile(
    id: &str,
    instrument: &SfzInstrument,
    engine_rate: u32,
) -> Result<CompiledInstrument, String> {
    let engine_rate = if engine_rate == 0 { 48_000 } else { engine_rate };
    let sfz_path = Path::new(&instrument.sfz_path);
    // path -> (decoded engine-rate sample, engine_rate / file_rate).
    let mut cache: HashMap<PathBuf, (Arc<SampleData>, f64)> = HashMap::new();
    let mut regions = Vec::with_capacity(instrument.regions.len());

    for r in &instrument.regions {
        let path = resolve_sample_path(sfz_path, &r.sample);
        let (sample, frame_ratio) = match cache.get(&path) {
            Some(entry) => entry.clone(),
            None => {
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if ext == "flac" {
                    return Err(format!(
                        "{}: FLAC samples are not supported yet (SFZ subset v1 loader is WAV-only)",
                        path.display()
                    ));
                }
                let (channels, file_rate, data) = load_wav(&path)?;
                // Resample to the engine rate NOW (load time, control side) —
                // the RT thread only ever plays engine-rate data.
                let data = linear_resample(&data, channels as usize, file_rate, engine_rate);
                let ratio = engine_rate as f64 / file_rate.max(1) as f64;
                let s = Arc::new(SampleData { channels, rate: engine_rate, data });
                cache.insert(path.clone(), (s.clone(), ratio));
                (s, ratio)
            }
        };
        // Frame-based opcodes (offset/loop points) were authored against the
        // FILE rate; rescale them onto the resampled data.
        let scale = |frames: u64| -> u64 { (frames as f64 * frame_ratio).round() as u64 };
        let (offset, loop_start, loop_end) = (scale(r.offset), scale(r.loop_start), scale(r.loop_end));
        let vol = db_to_linear(r.volume as f64);
        let (pl, pr) = sfz_pan_gains(r.pan);
        regions.push(CompiledRegion {
            lokey: r.lokey,
            hikey: r.hikey,
            lovel: r.lovel,
            hivel: r.hivel,
            pitch_keycenter: r.pitch_keycenter,
            tune: r.tune,
            transpose: r.transpose,
            gain_l: vol * pl,
            gain_r: vol * pr,
            offset,
            looped: r.looped,
            loop_start,
            loop_end,
            ampeg_attack: r.ampeg_attack,
            ampeg_release: r.ampeg_release,
            sample,
        });
    }
    if regions.is_empty() {
        return Err(format!("instrument {} has no regions", instrument.name));
    }
    Ok(CompiledInstrument {
        id: id.to_string(),
        name: instrument.name.clone(),
        rate: engine_rate,
        regions: Arc::new(regions),
    })
}

/// Load + compile an .sfz and register it in `bank` under a fresh id — the
/// shared body of the `sampler_load_instrument` command and the post-job
/// auto-register hook (finished `stableAudioSfz` jobs). Control-plane only.
pub fn load_into_bank(
    bank: &parking_lot::Mutex<super::sampler::SamplerBank>,
    sfz_path: &Path,
    name: Option<String>,
    engine_rate: u32,
) -> Result<super::sampler::InstrumentInfo, String> {
    let instrument = super::sampler::load_instrument(sfz_path, name)?;
    let id = uuid::Uuid::new_v4().to_string();
    let info = super::sampler::instrument_info(&id, &instrument);
    let compiled = compile(&id, &instrument, engine_rate)?;
    let mut b = bank.lock();
    b.instruments.insert(id.clone(), instrument);
    b.compiled.insert(id, Arc::new(compiled));
    Ok(info)
}

/// [`load_into_bank`] against the app-wide registered bank (None before
/// `audio::init` — e.g. in unit tests, which pass a bank explicitly).
pub fn load_into_registered_bank(
    sfz_path: &Path,
    name: Option<String>,
    engine_rate: u32,
) -> Result<super::sampler::InstrumentInfo, String> {
    let bank = super::sampler::registered_bank()
        .ok_or("sampler bank is not initialized (app not started)")?;
    load_into_bank(bank, sfz_path, name, engine_rate)
}

// ---------------------------------------------------------------------------
// SamplerNode — the RT instrument node
// ---------------------------------------------------------------------------

/// Control-plane -> RT note command (wait-free rtrb queue). POD.
#[derive(Debug, Clone, Copy)]
pub enum NoteCmd {
    /// Start a note; `hold_frames` > 0 schedules an automatic release after
    /// that many frames ([`HOLD_MANUAL`] = wait for an explicit `Off`).
    On { key: u8, velocity: u8, hold_frames: u64 },
    Off { key: u8 },
    AllOff,
}

/// Polyphonic sampler instrument node. One per midi track (or preview
/// player). Immutable instrument snapshot + preallocated voices; installed
/// into the render path by RCU build-and-swap — never mutated concurrently.
pub struct SamplerNode {
    instrument: CompiledInstrument,
    pool: VoicePool,
    events: [BlockNoteEvent; MAX_BLOCK_EVENTS],
    n_events: usize,
    cmds: Option<rtrb::Consumer<NoteCmd>>,
    rate: u32,
}

impl SamplerNode {
    /// Control-plane construction; allocates the full voice pool up front.
    pub fn new(instrument: CompiledInstrument) -> Self {
        let rate = instrument.rate;
        Self {
            instrument,
            pool: VoicePool::new(MAX_VOICES),
            events: [BlockNoteEvent { offset: 0, key: 0, velocity: 0 }; MAX_BLOCK_EVENTS],
            n_events: 0,
            cmds: None,
            rate,
        }
    }

    pub fn instrument(&self) -> &CompiledInstrument {
        &self.instrument
    }

    /// Create the control->RT note-command queue and return the producer end
    /// (control plane keeps it; the node drains it in `process`). Call once,
    /// on the control thread, before the node is installed.
    pub fn command_queue(&mut self, capacity: usize) -> rtrb::Producer<NoteCmd> {
        let (tx, rx) = rtrb::RingBuffer::new(capacity.max(8));
        self.cmds = Some(rx);
        tx
    }

    /// Feed this block's note events (frozen zone-C contract: sample-offset
    /// [`BlockNoteEvent`]s, non-decreasing `offset`, velocity 0 = note off).
    /// RT-safe: copies into the preallocated buffer; overflow is dropped.
    pub fn set_block_events(&mut self, events: &[BlockNoteEvent]) {
        let n = events.len().min(MAX_BLOCK_EVENTS);
        self.events[..n].copy_from_slice(&events[..n]);
        self.n_events = n;
    }

    /// Append ONE block event (live-node seam; same buffer as
    /// [`SamplerNode::set_block_events`]). RT-safe; `false` on overflow.
    pub fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        if self.n_events >= MAX_BLOCK_EVENTS {
            return false;
        }
        self.events[self.n_events] = ev;
        self.n_events += 1;
        true
    }

    pub fn active_voices(&self) -> usize {
        self.pool.active_count()
    }

    #[inline]
    fn apply_event(&mut self, key: u8, velocity: u8, hold: u64) {
        if velocity == 0 {
            self.pool.note_off(key);
        } else {
            self.pool
                .note_on(&self.instrument.regions, key, velocity, self.rate, hold);
        }
    }
}

impl AudioProcessor for SamplerNode {
    fn prepare(&mut self, sample_rate: u32, _max_block: usize) {
        // Voices are already preallocated; just adopt the engine rate. When
        // it differs from the compiled rate the pitch ratio folds the
        // difference in (SampleData.rate / out_rate), so playback stays in
        // tune even across a device switch before a reload.
        self.rate = sample_rate.max(1);
    }

    /// ADDS the instrument's voices into `io.samples`. RT-safe.
    fn process(&mut self, io: &mut ProcessBlock<'_>) {
        let frames = io.frames();
        let out_ch = io.channels.max(1);
        self.rate = io.sample_rate.max(1);

        // Control-plane commands land at block start (offset 0). The queue
        // is temporarily taken to satisfy the borrow checker; `Option` swap
        // moves a fixed-size struct — no allocation.
        if let Some(mut rx) = self.cmds.take() {
            while let Ok(cmd) = rx.pop() {
                match cmd {
                    NoteCmd::On { key, velocity, hold_frames } => {
                        let hold = if hold_frames == 0 { HOLD_MANUAL } else { hold_frames };
                        self.apply_event(key, velocity.max(1), hold);
                    }
                    NoteCmd::Off { key } => self.pool.note_off(key),
                    NoteCmd::AllOff => self.pool.release_all(),
                }
            }
            self.cmds = Some(rx);
        }

        // Sample-accurate block events: render segments between offsets.
        let mut cursor = 0usize;
        for i in 0..self.n_events {
            let e = self.events[i];
            let off = (e.offset as usize).min(frames).max(cursor);
            if off > cursor {
                self.pool
                    .render(&self.instrument.regions, io.samples, out_ch, cursor, off);
                cursor = off;
            }
            self.apply_event(e.key, e.velocity, HOLD_MANUAL);
        }
        self.pool
            .render(&self.instrument.regions, io.samples, out_ch, cursor, frames);
        self.n_events = 0;
    }

    fn reset(&mut self) {
        self.pool.kill_all();
        self.n_events = 0;
    }
}

/// Live-node seam (phase 3, ARCHITECTURE §15): the sampler is an in-graph
/// instrument behind the same contract as PolySynth and plugin nodes.
impl crate::audio::dsp::LiveInstrument for SamplerNode {
    fn queue_event(&mut self, ev: BlockNoteEvent) -> bool {
        SamplerNode::queue_event(self, ev)
    }

    fn all_notes_off(&mut self) {
        self.pool.release_all();
        self.n_events = 0;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::sampler::{load_instrument, parse_sfz};
    use super::super::sampler_voice::testutil::estimate_freq;
    use super::*;
    use std::process::Command;

    const RATE: u32 = 48_000;

    fn write_sine_wav(path: &Path, freq: f64, rate: u32, secs: f64) {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        let n = (rate as f64 * secs) as usize;
        for i in 0..n {
            let s = (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin();
            w.write_sample((s * 0.9 * i16::MAX as f64) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "aura-sampler-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// End-to-end: .sfz on disk -> parse -> compile -> SamplerNode renders
    /// the requested pitch (validated by sine analysis of the output).
    #[test]
    fn sfz_load_to_render_pitch_accuracy() {
        let dir = temp_dir("e2e");
        // Source sample: 220 Hz sine AT 44.1 kHz (engine at 48 kHz => the
        // load-time resample + ratio math must both be right).
        write_sine_wav(&dir.join("a3.wav"), 220.0, 44_100, 1.0);
        let sfz = "<control> default_path=.\n<global> ampeg_attack=0.001 ampeg_release=0.05\n\
                   <region> sample=a3.wav lokey=0 hikey=127 pitch_keycenter=a3\n";
        std::fs::write(dir.join("inst.sfz"), sfz).unwrap();

        let instr = load_instrument(&dir.join("inst.sfz"), None).unwrap();
        let compiled = compile("test-id", &instr, RATE).unwrap();
        assert_eq!(compiled.rate, RATE);
        assert_eq!(compiled.key_range(), (0, 127));

        let mut node = SamplerNode::new(compiled);
        node.prepare(RATE, 512);
        // Play E4 (64): 220 * 2^(7/12) ≈ 329.63 Hz.
        node.set_block_events(&[BlockNoteEvent { offset: 0, key: 64, velocity: 120 }]);
        let mut buf = vec![0.0f32; 24_000 * 2];
        // Render in engine-sized blocks to exercise block boundaries.
        for chunk in buf.chunks_mut(512 * 2) {
            let mut io = ProcessBlock { samples: chunk, channels: 2, sample_rate: RATE, steady: None };
            node.process(&mut io);
        }
        let mono: Vec<f32> = buf.iter().step_by(2).copied().collect();
        assert!(mono.iter().any(|s| s.abs() > 0.05), "audible output");
        let est = estimate_freq(&mono[2000..], RATE, 100.0, 800.0);
        let target = 220.0 * (7.0f64 / 12.0).exp2();
        assert!(
            (est - target).abs() / target < 0.01,
            "estimated {est:.2} Hz, want {target:.2} Hz"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn block_events_are_sample_accurate_and_note_off_works() {
        let dir = temp_dir("offset");
        write_sine_wav(&dir.join("s.wav"), 440.0, RATE, 0.5);
        std::fs::write(
            dir.join("i.sfz"),
            "<region> sample=s.wav pitch_keycenter=69 ampeg_attack=0 ampeg_release=0.001",
        )
        .unwrap();
        let compiled =
            compile("t", &load_instrument(&dir.join("i.sfz"), None).unwrap(), RATE).unwrap();
        let mut node = SamplerNode::new(compiled);
        node.prepare(RATE, 256);

        // Note starts at offset 100 within a 256-frame block.
        node.set_block_events(&[BlockNoteEvent { offset: 100, key: 69, velocity: 127 }]);
        let mut buf = vec![0.0f32; 256 * 2];
        let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: RATE, steady: None };
        node.process(&mut io);
        assert!(
            buf[..100 * 2].iter().all(|s| *s == 0.0),
            "silent before the event offset"
        );
        assert!(
            buf[100 * 2..].iter().any(|s| s.abs() > 0.01),
            "sounding after the event offset"
        );

        // velocity 0 == note off (frozen contract) -> voice releases + dies.
        node.set_block_events(&[BlockNoteEvent { offset: 0, key: 69, velocity: 0 }]);
        let mut buf2 = vec![0.0f32; 512 * 2];
        let mut io2 = ProcessBlock { samples: &mut buf2, channels: 2, sample_rate: RATE, steady: None };
        node.process(&mut io2);
        assert_eq!(node.active_voices(), 0, "released after vel-0 event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_queue_drives_notes_with_auto_hold() {
        let dir = temp_dir("cmds");
        write_sine_wav(&dir.join("s.wav"), 440.0, RATE, 1.0);
        std::fs::write(dir.join("i.sfz"), "<region> sample=s.wav ampeg_release=0.01").unwrap();
        let compiled =
            compile("t", &load_instrument(&dir.join("i.sfz"), None).unwrap(), RATE).unwrap();
        let mut node = SamplerNode::new(compiled);
        let mut tx = node.command_queue(16);
        node.prepare(RATE, 512);
        tx.push(NoteCmd::On { key: 60, velocity: 100, hold_frames: 256 }).unwrap();
        let mut buf = vec![0.0f32; 512 * 2];
        let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: RATE, steady: None };
        node.process(&mut io);
        assert!(buf.iter().any(|s| s.abs() > 0.01), "preview note audible");
        assert_eq!(node.active_voices(), 1);
        // Auto-release after 256 frames + 10 ms release: silent well within
        // the next 1024 frames.
        let mut buf2 = vec![0.0f32; 1024 * 2];
        let mut io2 = ProcessBlock { samples: &mut buf2, channels: 2, sample_rate: RATE, steady: None };
        node.process(&mut io2);
        assert_eq!(node.active_voices(), 0, "hold expired, voice released");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compile_dedupes_sample_files_and_applies_volume() {
        let dir = temp_dir("dedupe");
        write_sine_wav(&dir.join("s.wav"), 440.0, RATE, 0.1);
        let sfz = "<global> volume=-6\n\
                   <region> sample=s.wav lokey=0 hikey=63\n\
                   <region> sample=s.wav lokey=64 hikey=127\n";
        std::fs::write(dir.join("i.sfz"), sfz).unwrap();
        let compiled =
            compile("t", &load_instrument(&dir.join("i.sfz"), None).unwrap(), RATE).unwrap();
        assert_eq!(compiled.regions.len(), 2);
        assert!(
            Arc::ptr_eq(&compiled.regions[0].sample, &compiled.regions[1].sample),
            "same file decoded once"
        );
        let expected = db_to_linear(-6.0);
        assert!((compiled.regions[0].gain_l - expected).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- simulate-mode instrument-builder E2E ----

    fn sidecar_script() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .join("sidecars/stable_audio_sfz_worker.py");
        p.exists().then_some(p)
    }

    /// Full flow with zero ML deps: run the worker in --simulate, then load,
    /// compile and RENDER the generated instrument, checking key range and
    /// that a played note is audible at (approximately) the right pitch.
    #[test]
    fn simulate_generated_instrument_is_loadable_and_audible() {
        let Some(script) = sidecar_script() else {
            eprintln!("skipping: sidecars dir not found");
            return;
        };
        let python = ["python3", "python"]
            .iter()
            .find(|p| Command::new(p).arg("--version").output().is_ok())
            .copied();
        let Some(python) = python else {
            eprintln!("skipping: python not available");
            return;
        };
        let dir = temp_dir("simgen");
        let params = serde_json::json!({
            "prompt": "warm electric piano",
            "name": "SimPiano",
            "outputDir": dir.to_string_lossy(),
            "keys": [48, 60, 72],
            "velocityLayers": 2,
        });
        let out = Command::new(python)
            .arg(&script)
            .arg("--simulate")
            .arg("--params-json")
            .arg(params.to_string())
            .output()
            .expect("run worker");
        assert!(
            out.status.success(),
            "worker failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let done: serde_json::Value = stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["type"] == "done")
            .expect("done NDJSON event");
        let result = &done["result"];
        assert_eq!(result["kind"], "stableAudioSfz");
        assert_eq!(result["simulated"], true);
        assert_eq!(result["sampleCount"], 6, "3 keys x 2 layers");
        let sfz_path = PathBuf::from(result["sfzPath"].as_str().unwrap());
        assert!(sfz_path.is_absolute() && sfz_path.exists());

        // The generated file must parse under the SUBSET v1 parser…
        let text = std::fs::read_to_string(&sfz_path).unwrap();
        assert_eq!(parse_sfz(&text).unwrap().len(), 6);

        // …load + compile…
        let instr = load_instrument(&sfz_path, None).unwrap();
        let compiled = compile("sim", &instr, RATE).unwrap();
        assert_eq!(
            compiled.key_range(),
            (0, 127),
            "sampled keys fan out to the full range"
        );

        // …and be AUDIBLE at the right pitch: play C4 (60) => ~261.63 Hz.
        let mut node = SamplerNode::new(compiled);
        node.prepare(RATE, 512);
        node.set_block_events(&[BlockNoteEvent { offset: 0, key: 60, velocity: 100 }]);
        let mut buf = vec![0.0f32; 24_000 * 2];
        for chunk in buf.chunks_mut(512 * 2) {
            let mut io = ProcessBlock { samples: chunk, channels: 2, sample_rate: RATE, steady: None };
            node.process(&mut io);
        }
        let mono: Vec<f32> = buf.iter().step_by(2).copied().collect();
        assert!(mono.iter().any(|s| s.abs() > 0.02), "generated note audible");
        let est = estimate_freq(&mono[2000..20_000], RATE, 100.0, 600.0);
        let target = 261.626;
        assert!(
            (est - target).abs() / target < 0.02,
            "simulate sample pitch: got {est:.2} Hz, want ~{target:.2} Hz"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
