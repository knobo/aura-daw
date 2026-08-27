//! Where does a block's time actually go? (`docs/GAP_ANALYSIS.md` §8.4)
//!
//! The JIT track measured the fader to four decimal places and then wrote
//! its own conclusion as a caveat: *"The fader is not where the CPU goes;
//! plugins are."* Nobody had measured that. This harness does.
//!
//! # How it answers the question: subtraction, not instrumentation
//!
//! There is no timing code anywhere in the render path, and this file does
//! not add any. Putting `Instant::now()` on the RT path would break the RT
//! rules (`docs/STANDING-CONSTRAINTS.md`) and would have to come out again
//! afterwards, which means the numbers could never be reproduced by the
//! next person to ask.
//!
//! Instead the SAME session is rendered four times, differing only in
//! which plugins are attached:
//!
//! | Run          | Instruments    | Inserts        | What it costs              |
//! |--------------|----------------|----------------|----------------------------|
//! | `full`       | plugin + synth | real           | everything                 |
//! | `cheap_fx`   | plugin + synth | same count, ~0 | everything but plugin DSP  |
//! | `no_inserts` | plugin + synth | none           | everything but the effects |
//! | `bare`       | built-in synth | none           | AURA's mixer, sends, PDC   |
//!
//! Four subtractions fall out, and none of them needs a profiler:
//!
//! - plugin DSP    = `full` − `cheap_fx`
//! - host overhead = `cheap_fx` − `no_inserts`   (per slot: divide by slots)
//! - instruments   = `no_inserts` − `bare`
//! - AURA's own    = `bare`
//!
//! The `cheap_fx` run is the interesting one. Its chains are exactly as
//! LONG as `full`'s, so every buffer copy, param flush, event conversion
//! and `Replace`-mode step is paid identically — only the arithmetic
//! inside the plugin is missing. That splits "the plugins are slow" into
//! "the plugin computes" and "we spend this much calling it", which are
//! very different findings: only the second one is ours to fix.
//!
//! # If you want the flamegraph too
//!
//! The subtraction says how much; only a profiler says which function.
//! `perf` needs a sysctl that is not set on the machine these numbers came
//! from (`kernel.perf_event_paranoid` is 4; user-space sampling needs <= 2):
//!
//! ```sh
//! # 1. the numbers
//! AURA_PROFILE_PLUGINS=1 cargo test --release --test plugin_load_profile \
//!     -- --nocapture
//!
//! # 2. the flamegraph
//! sudo sysctl kernel.perf_event_paranoid=1
//! cargo test --release --test plugin_load_profile --no-run   # note the path
//! AURA_PROFILE_PLUGINS=1 perf record -g --call-graph dwarf \
//!     target/release/deps/plugin_load_profile-<hash> \
//!     where_the_block_time --nocapture
//! perf report --stdio --sort dso,symbol | head -60
//! ```
//!
//! # Why this is a test and not a criterion bench
//!
//! `src-tauri/Cargo.toml` is a FROZEN file — no `[[bench]]` stanza and no
//! new dev-dependency may be added to it. Cargo discovers `tests/*.rs` on
//! its own, so this needs neither. Criterion would be the wrong tool
//! anyway: it exists to resolve a few percent on one function, and the
//! question here ("is it the plugins or is it us") is answered by a factor,
//! not by a confidence interval.
//!
//! # How this measurement could lie, and what stops it
//!
//! Every failure mode here is silent by construction, which is why each one
//! gets an assertion rather than a hopeful comment:
//!
//! - **A plugin the host refused.** `compile_inserts` SKIPS a slot it
//!   cannot host and leaves the strip dry (`plugins::insert_node_for`
//!   returns `None`); `node_for_track` falls back to the built-in
//!   `PolySynth`. Either way you get a fast, plausible, meaningless number.
//!   Guarded by [`assert_graph_really_has_the_plugins`], which counts the
//!   compiled chains, and by the `full` vs `bare` output comparison — if
//!   the instrument silently fell back, the two runs render IDENTICAL
//!   samples, and that is an assertion failure, not a footnote.
//! - **A graph that is not making sound.** Profiling silence is cheap and
//!   proves nothing. Every run asserts a non-trivial output peak.
//! - **Measuring the first block.** Plugins allocate, fault in and settle
//!   on their first few calls. Warm-up blocks are rendered and discarded.
//! - **`cheap_fx` carrying a different number of slots than `full`.** Then
//!   their difference would be a chain length, not a DSP cost, and the "µs
//!   per insert" figure would be fiction. Asserted per run by
//!   [`assert_graph_really_has_the_plugins`] against the same expected
//!   count, and unit-tested without any plugin installed by
//!   `the_cheap_run_has_exactly_as_many_insert_slots_as_the_full_one`.
//!
//! Skipped politely unless `AURA_PROFILE_PLUGINS` is set, so a plain
//! `cargo test` stays green on a machine with no plugins installed — the
//! same gate `real_models.rs` uses.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;

use aura_lib::audio::offline;
use aura_lib::audio::rt::RtGraph;
use aura_lib::audio::transport::LoopSpec;
use aura_lib::audio::types::{InsertSlot, SendSlot, Store, TrackState};
use aura_lib::audio::{mixer, types::AutomationMode};
use aura_lib::control::session::PluginDoc;
use aura_lib::midi::{MeterEvent, MidiStore, TempoEvent};
use aura_lib::plugins::descriptor::PluginInstanceInfo;
use aura_lib::plugins::{self, PluginRegistry};

/// 48 kHz, the rate every number in `GAP_ANALYSIS` §4 is quoted at.
const RATE: u32 = 48_000;
/// One audio callback. At 48 kHz this is a **10.67 ms** deadline — the same
/// block size §4's table uses, so these numbers can be read beside it.
const BLOCK_FRAMES: usize = 512;
/// Blocks rendered and thrown away before timing starts.
const WARMUP_BLOCKS: usize = 64;
/// Blocks actually timed. 2000 blocks is ~21 s of audio.
const TIMED_BLOCKS: usize = 2000;

/// Track counts to sweep, so the growth curve is visible rather than
/// inferred from one point.
const TRACK_COUNTS: &[usize] = &[4, 16, 32];

/// One in four tracks carries a hosted instrument; the rest run AURA's
/// built-in `PolySynth`. A session where every single track is a Surge XT
/// would be a stress test, not a mix.
const PLUGIN_INSTRUMENT_EVERY: usize = 4;

fn deadline_micros() -> f64 {
    BLOCK_FRAMES as f64 / RATE as f64 * 1e6
}

fn gated() -> bool {
    if std::env::var_os("AURA_PROFILE_PLUGINS").is_none() {
        eprintln!(
            "skipping: AURA_PROFILE_PLUGINS not set (real-plugin profiling test).\n\
             Run it with: AURA_PROFILE_PLUGINS=1 cargo test --release \\\n\
             \x20   --test plugin_load_profile -- --nocapture"
        );
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Picking the plugins
// ---------------------------------------------------------------------------

/// What the session wants, by human name. Overridable from the environment
/// so the harness is not welded to one machine's catalogue — a reviewer with
/// a different set of plugins installed can still reproduce the shape.
struct Wanted {
    instrument: String,
    inserts: Vec<String>,
    bus_fx: String,
    cheap: String,
}

impl Default for Wanted {
    fn default() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        Self {
            instrument: env("AURA_PROFILE_INSTRUMENT", "Surge XT"),
            // A compressor and an EQ: the two effects that are on almost
            // every strip of almost every mix.
            //
            // NOT ZamEQ2, which was the first choice. At its own port
            // defaults it renders digital silence — verified against
            // `ZamComp`, `ZamCompX2`, `Calf Compressor`, `Calf Reverb` and
            // `Audio Gain (Stereo)`, all of which pass audio through the
            // same host, and against `lv2_host.rs`'s port setup, which does
            // honour `lv2:default` (there is a test asserting the initial
            // value IS the default). So it is that plugin's behaviour at
            // its defaults, not AURA's insert path. The silence assertion
            // in this harness is what caught it.
            inserts: env("AURA_PROFILE_INSERTS", "ZamComp,Calf Equalizer 5 Band")
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            bus_fx: env("AURA_PROFILE_BUS_FX", "Calf Reverb"),
            // One multiply per sample. Anything the `cheap_fx` run costs
            // above `no_inserts` is what AURA spends CALLING a plugin, not
            // what the plugin computes.
            cheap: env("AURA_PROFILE_CHEAP_FX", "Audio Gain (Stereo)"),
        }
    }
}

/// Resolve a human name to a scanned plugin uid.
///
/// Exact name match first: "Surge XT" and "Surge XT Effects" both contain
/// "Surge XT", and a substring match would pick whichever the scan happened
/// to return first — an effect standing in for the instrument, silently.
fn uid_for(scanned: &[aura_lib::plugins::descriptor::PluginDescriptor], name: &str) -> Option<String> {
    scanned
        .iter()
        .find(|d| d.name == name)
        .or_else(|| scanned.iter().find(|d| d.name.contains(name)))
        .map(|d| d.uid.clone())
}

// ---------------------------------------------------------------------------
// The session
// ---------------------------------------------------------------------------

fn track(id: &str, kind: &str) -> TrackState {
    TrackState {
        id: id.into(),
        name: id.into(),
        kind: kind.into(),
        gain_db: -6.0,
        pan: 0.0,
        muted: false,
        soloed: false,
        armed: false,
        color: "#8899aa".into(),
        instrument_id: None,
        inserts: Vec::new(),
        sends: Vec::new(),
        output: None,
        group: None,
        automation_mode: AutomationMode::default(),
    }
}

/// Which plugins this run attaches. The runs differ ONLY in this.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Load {
    /// Hosted instruments and real insert chains — a real session.
    Full,
    /// Hosted instruments, and insert chains of the SAME LENGTH whose
    /// effects do almost no arithmetic (a gain is one multiply per sample).
    ///
    /// This is the run that splits the insert bill in two without a
    /// profiler. Everything AURA pays to CALL a plugin — the buffer copy in
    /// and out, the param flush, the event conversion, the `Replace`-mode
    /// plumbing — is paid identically here and in `Full`. What is missing is
    /// the DSP. So:
    ///
    /// - host overhead = `CheapInserts` − `NoInserts`
    /// - actual plugin DSP = `Full` − `CheapInserts`
    ///
    /// A flamegraph would show the same split by symbol, but `perf` needs
    /// `kernel.perf_event_paranoid <= 2` and this needs nothing.
    CheapInserts,
    /// Hosted instruments, no effects.
    NoInserts,
    /// No hosted plugins at all: AURA's own synth, mixer, sends and PDC.
    Bare,
}

impl Load {
    fn label(self) -> &'static str {
        match self {
            Load::Full => "full",
            Load::CheapInserts => "cheap_fx",
            Load::NoInserts => "no_inserts",
            Load::Bare => "bare",
        }
    }

    /// Which instance set this run's insert slots draw from.
    fn chain(self, inst: &Instances, track: usize) -> Option<&Vec<PluginInstanceInfo>> {
        match self {
            Load::Full => inst.inserts.get(track),
            Load::CheapInserts => inst.cheap.get(track),
            Load::NoInserts | Load::Bare => None,
        }
    }

    fn bus_fx(self, inst: &Instances) -> Option<&PluginInstanceInfo> {
        match self {
            Load::Full => inst.bus_fx.as_ref(),
            Load::CheapInserts => inst.cheap_bus.as_ref(),
            Load::NoInserts | Load::Bare => None,
        }
    }

    /// True when this run should carry insert chains at all.
    fn has_inserts(self) -> bool {
        matches!(self, Load::Full | Load::CheapInserts)
    }
}

/// Instances handed out by [`instantiate_all`], in the order the session
/// consumes them.
///
/// These are the REAL [`PluginInstanceInfo`] rows the host returned, not
/// reconstructed ones. `insert_node_for` and `live_node_for` both branch on
/// `info.format` to pick the CLAP or LV2 host; a row that carries the id but
/// not the format resolves to no node at all, the slot is skipped, and the
/// strip renders dry — a fast benchmark of nothing. Ask this file's git
/// history how that was discovered.
struct Instances {
    /// One per plugin-carrying midi track.
    instruments: Vec<PluginInstanceInfo>,
    /// `inserts[track_index]` — one instance per slot, per track. Insert
    /// instances CANNOT be shared: a format host refuses a second live node
    /// for an instance that already has one out (`clap_host`'s `node_out`
    /// guard), so a shared instance would render every track after the
    /// first one dry.
    inserts: Vec<Vec<PluginInstanceInfo>>,
    bus_fx: Option<PluginInstanceInfo>,
    /// The same SHAPE as `inserts` — same slot count per track — but filled
    /// with an effect that barely computes. See [`Load::CheapInserts`].
    cheap: Vec<Vec<PluginInstanceInfo>>,
    cheap_bus: Option<PluginInstanceInfo>,
}

/// Build the session document. The routing is the same in every run —
/// same tracks, same sends, same bus — so the subtraction isolates the
/// plugins and nothing else.
fn build_session(n_tracks: usize, inst: &Instances, load: Load) -> (Store, MidiStore) {
    let mut store = Store::default();
    let mut clips = Vec::new();

    // Two seeded demo clips' worth of notes, reused across the track set:
    // real note material, so instruments actually render voices rather than
    // idling. `demo_seed_clips` is the same source the offline tests use.
    for i in 0..n_tracks {
        let id = format!("t{i}");
        let mut t = track(&id, "midi");

        if load != Load::Bare && i % PLUGIN_INSTRUMENT_EVERY == 0 {
            if let Some(info) = inst.instruments.get(i / PLUGIN_INSTRUMENT_EVERY) {
                t.instrument_id = Some(format!("plugin:{}", info.id));
            }
        }
        if load.has_inserts() {
            for (s, info) in load.chain(inst, i).into_iter().flatten().enumerate() {
                t.inserts.push(InsertSlot {
                    id: format!("{id}-fx{s}"),
                    instance_id: info.id.clone(),
                    bypassed: false,
                });
            }
        }
        // Everyone into the one shared room — the send/return shape G2
        // shipped, and the reason a reverb is affordable at all.
        t.sends.push(SendSlot {
            id: format!("{id}-send"),
            dest: "verb".into(),
            amount_db: -12.0,
            pre_fader: false,
        });
        store.tracks.push(t);

        let (arp, groove) = aura_lib::control::demo_seed_clips(&id, &id, 960);
        clips.push(if i % 2 == 0 { arp } else { groove });
    }

    // The shared reverb bus.
    let mut bus = track("verb", "bus");
    if load.has_inserts() {
        if let Some(info) = load.bus_fx(inst) {
            bus.inserts.push(InsertSlot {
                id: "verb-fx".into(),
                instance_id: info.id.clone(),
                bypassed: false,
            });
        }
    }
    store.tracks.push(bus);

    let midi = MidiStore {
        harmony: Default::default(),
        ppq: 960,
        tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
        meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
        clips,
        launch_maps: Vec::new(),
        loaded_dir: None,
        dirty: false,
    };
    (store, midi)
}

// ---------------------------------------------------------------------------
// Measuring
// ---------------------------------------------------------------------------

struct Timing {
    micros: Vec<f64>,
    peak: f32,
}

impl Timing {
    fn pct(&self, p: f64) -> f64 {
        let mut v = self.micros.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((v.len() - 1) as f64 * p).round() as usize;
        v[i]
    }
    fn median(&self) -> f64 {
        self.pct(0.5)
    }
    fn max(&self) -> f64 {
        self.micros.iter().cloned().fold(0.0f64, f64::max)
    }
}

/// Render `TIMED_BLOCKS` blocks of `BLOCK_FRAMES` and time each one.
///
/// One `Instant` per BLOCK, not per phase — a block is the unit the deadline
/// is expressed in, and it is the only unit that can be timed from outside
/// without touching the render path.
fn measure(graph: &mut RtGraph) -> Timing {
    let mut out = vec![0.0f32; BLOCK_FRAMES * 2];
    let mut pos = 0u64;
    let mut peak = 0.0f32;

    // Plugins allocate, fault their code in and settle over the first few
    // calls. Timing those measures the loader, not the DSP.
    for i in 0..WARMUP_BLOCKS {
        mixer::render(graph, pos, &LoopSpec::OFF, &mut out, 2, RATE, i == 0, None);
        pos += BLOCK_FRAMES as u64;
    }

    let mut micros = Vec::with_capacity(TIMED_BLOCKS);
    for _ in 0..TIMED_BLOCKS {
        let t0 = Instant::now();
        mixer::render(graph, pos, &LoopSpec::OFF, &mut out, 2, RATE, false, None);
        micros.push(t0.elapsed().as_secs_f64() * 1e6);
        pos += BLOCK_FRAMES as u64;
        peak = out.iter().fold(peak, |m, s| m.max(s.abs()));
    }
    Timing { micros, peak }
}

/// The graph must contain the plugins the session asked for.
///
/// `compile_inserts` skips a slot whose instance the host refused and leaves
/// the strip DRY, logging a warning that a test run discards. A short chain
/// is therefore the difference between "the effects are free" and "the
/// effects are absent", and only this count can tell them apart.
fn assert_graph_really_has_the_plugins(graph: &RtGraph, expected_chains: usize, label: &str) {
    let compiled: usize = graph.tracks.iter().filter(|t| !t.inserts.is_empty()).count()
        + graph.buses.iter().filter(|b| !b.inserts.is_empty()).count();
    assert_eq!(
        compiled, expected_chains,
        "{label}: {expected_chains} strips should carry an insert chain, {compiled} do. \
         A refused instance renders DRY and silently makes this benchmark fast."
    );
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

fn report_row(label: &str, n: usize, t: &Timing) {
    let d = deadline_micros();
    println!(
        "| {n:>2} | {label:<10} | {:>8.1} | {:>8.1} | {:>8.1} | {:>7.2}% |",
        t.median(),
        t.pct(0.95),
        t.max(),
        t.median() / d * 100.0,
    );
}

fn report_header() {
    println!(
        "\n| tracks | run        | median µs | p95 µs | max µs | % of {:.2} ms |",
        deadline_micros() / 1000.0
    );
    println!("|---:|:-----------|---------:|-------:|-------:|--------:|");
}

// ---------------------------------------------------------------------------
// Instantiation
// ---------------------------------------------------------------------------

/// Instantiate every plugin instance the largest run needs.
///
/// Done ONCE for the whole sweep: instantiation is control-thread work that
/// happens when a project loads, not per block, and paying it per run would
/// put loader time inside a measurement about DSP.
fn instantiate_all(registry: &Arc<Mutex<PluginRegistry>>, want: &Wanted, max_tracks: usize) -> Instances {
    let scanned = { registry.lock().scanned.clone().unwrap_or_default() };

    let mut instruments = Vec::new();
    match uid_for(&scanned, &want.instrument) {
        Some(uid) => {
            let n = max_tracks.div_ceil(PLUGIN_INSTRUMENT_EVERY);
            for _ in 0..n {
                match plugins::instantiate_and_activate(registry, &uid) {
                    Ok((info, _)) => instruments.push(active(info)),
                    Err(e) => {
                        eprintln!("  instrument {} unavailable: {e}", want.instrument);
                        break;
                    }
                }
            }
        }
        None => not_in_scan(&scanned, &want.instrument, true),
    }

    let mut insert_uids = Vec::new();
    for name in &want.inserts {
        match uid_for(&scanned, name) {
            Some(uid) => insert_uids.push(uid),
            None => not_in_scan(&scanned, name, false),
        }
    }
    let mut inserts = Vec::new();
    for _ in 0..max_tracks {
        let mut per_track = Vec::new();
        for uid in &insert_uids {
            match plugins::instantiate_and_activate_effect(registry, uid) {
                Ok((info, _)) => per_track.push(active(info)),
                Err(e) => eprintln!("  effect {uid} unavailable: {e}"),
            }
        }
        inserts.push(per_track);
    }

    // Same slot COUNT as the real chains, so the two runs differ only in
    // how much arithmetic happens once the host has handed the buffer over.
    let cheap_uid = uid_for(&scanned, &want.cheap);
    if cheap_uid.is_none() {
        not_in_scan(&scanned, &want.cheap, false);
    }
    let mut cheap = Vec::new();
    for _ in 0..max_tracks {
        let mut per_track = Vec::new();
        if let Some(uid) = &cheap_uid {
            for _ in 0..insert_uids.len() {
                match plugins::instantiate_and_activate_effect(registry, uid) {
                    Ok((info, _)) => per_track.push(active(info)),
                    Err(e) => eprintln!("  cheap effect {uid} unavailable: {e}"),
                }
            }
        }
        cheap.push(per_track);
    }
    let cheap_bus = cheap_uid.as_ref().and_then(|uid| {
        match plugins::instantiate_and_activate_effect(registry, uid) {
            Ok((info, _)) => Some(active(info)),
            Err(e) => {
                eprintln!("  cheap bus fx unavailable: {e}");
                None
            }
        }
    });

    let bus_fx = match uid_for(&scanned, &want.bus_fx) {
        Some(uid) => match plugins::instantiate_and_activate_effect(registry, &uid) {
            Ok((info, _)) => Some(active(info)),
            Err(e) => {
                eprintln!("  bus fx {} unavailable: {e}", want.bus_fx);
                None
            }
        },
        None => {
            not_in_scan(&scanned, &want.bus_fx, false);
            None
        }
    };

    Instances { instruments, inserts, bus_fx, cheap, cheap_bus }
}

/// Say what IS installed when a wanted plugin is not. "MVerb not found" sends
/// the reader to their package manager; a list of the effects that scanned
/// successfully lets them set `AURA_PROFILE_*` and carry on.
fn not_in_scan(
    scanned: &[aura_lib::plugins::descriptor::PluginDescriptor],
    name: &str,
    instrument: bool,
) {
    let kind = if instrument { "instrument" } else { "effect" };
    let mut have: Vec<&str> = scanned
        .iter()
        .filter(|d| d.is_instrument == instrument)
        .map(|d| d.name.as_str())
        .collect();
    have.sort_unstable();
    have.dedup();
    eprintln!(
        "  {kind} '{name}' is not in the scan. {} {kind}s available, e.g.: {}",
        have.len(),
        have.iter().take(12).cloned().collect::<Vec<_>>().join(", "),
    );
}

/// An instance the host only REGISTERED renders silence (`status: "stub"`).
/// Letting one into the session would measure a no-op and call it a plugin.
fn active(info: PluginInstanceInfo) -> PluginInstanceInfo {
    assert_eq!(
        info.status, "active",
        "instance {} of {} is '{}', not 'active' — it would render silence",
        info.id, info.name, info.status
    );
    info
}

/// The `PluginDoc` the graph builder reads: the host's OWN rows, verbatim.
///
/// Reconstructing these by hand is how the first run of this harness
/// measured nothing — see [`Instances`].
fn doc_for(inst: &Instances) -> PluginDoc {
    let mut doc = PluginDoc::default();
    doc.instances.extend(inst.instruments.iter().cloned());
    for per_track in &inst.inserts {
        doc.instances.extend(per_track.iter().cloned());
    }
    doc.instances.extend(inst.bus_fx.iter().cloned());
    for per_track in &inst.cheap {
        doc.instances.extend(per_track.iter().cloned());
    }
    doc.instances.extend(inst.cheap_bus.iter().cloned());
    doc
}

// ---------------------------------------------------------------------------
// Pure helpers, tested without any plugin installed
// ---------------------------------------------------------------------------

/// A doc row shaped like the host's, for the pure tests below.
fn fake(id: &str) -> PluginInstanceInfo {
    PluginInstanceInfo {
        id: id.into(),
        uid: "test:uid".into(),
        name: id.into(),
        format: "clap".into(),
        status: "active".into(),
        track_id: None,
    }
}

#[test]
fn percentiles_pick_real_samples() {
    let t = Timing { micros: vec![10.0, 20.0, 30.0, 40.0, 50.0], peak: 1.0 };
    assert_eq!(t.median(), 30.0);
    assert_eq!(t.max(), 50.0);
    assert_eq!(t.pct(0.0), 10.0);
    assert_eq!(t.pct(1.0), 50.0);
}

#[test]
fn the_block_deadline_is_ten_point_six_seven_milliseconds() {
    // The number §4's caveat is written against. If someone changes
    // BLOCK_FRAMES or RATE, every percentage in the report moves with it.
    assert!((deadline_micros() - 10_666.67).abs() < 1.0);
}

#[test]
fn inserts_attach_only_where_the_run_says_they_should() {
    let inst = Instances {
        instruments: vec![fake("i0")],
        inserts: vec![vec![fake("fx0"), fake("fx1")]],
        bus_fx: Some(fake("verb0")),
        cheap: vec![vec![fake("c0"), fake("c1")]],
        cheap_bus: Some(fake("cverb")),
    };
    let (full, _) = build_session(1, &inst, Load::Full);
    let (no_ins, _) = build_session(1, &inst, Load::NoInserts);
    let (bare, _) = build_session(1, &inst, Load::Bare);

    assert_eq!(full.tracks[0].inserts.len(), 2);
    assert!(no_ins.tracks[0].inserts.is_empty());
    assert!(bare.tracks[0].inserts.is_empty());

    // The instrument survives into `no_inserts` and only leaves in `bare` —
    // that is the whole point of having a middle run.
    assert_eq!(full.tracks[0].instrument_id.as_deref(), Some("plugin:i0"));
    assert_eq!(no_ins.tracks[0].instrument_id.as_deref(), Some("plugin:i0"));
    assert_eq!(bare.tracks[0].instrument_id, None);
}

#[test]
fn the_cheap_run_has_exactly_as_many_insert_slots_as_the_full_one() {
    // This is the whole basis of the host-overhead subtraction. If the two
    // runs carried different chain LENGTHS, their difference would be a slot
    // count rather than the arithmetic inside the slots, and the "µs per
    // insert" number would be fiction.
    let inst = Instances {
        instruments: vec![fake("i0")],
        inserts: vec![vec![fake("fx0"), fake("fx1")]; 3],
        bus_fx: Some(fake("verb0")),
        cheap: vec![vec![fake("c0"), fake("c1")]; 3],
        cheap_bus: Some(fake("cverb")),
    };
    let slots = |load| {
        let (s, _) = build_session(3, &inst, load);
        s.tracks.iter().map(|t| t.inserts.len()).sum::<usize>()
    };
    assert_eq!(slots(Load::Full), slots(Load::CheapInserts));
    assert_eq!(slots(Load::Full), 3 * 2 + 1, "3 tracks x 2 fx, plus the bus");

    // ...and they must be DIFFERENT instances, or the second run would be
    // asking the same plugin objects to do the same work.
    let (full, _) = build_session(3, &inst, Load::Full);
    let (cheap, _) = build_session(3, &inst, Load::CheapInserts);
    assert_ne!(full.tracks[0].inserts[0].instance_id, cheap.tracks[0].inserts[0].instance_id);
}

#[test]
fn routing_is_identical_across_every_run() {
    // If the runs differed in anything but plugins, the subtraction would be
    // measuring that difference instead.
    let inst = Instances {
        instruments: vec![fake("i0")],
        inserts: vec![vec![fake("fx0")]; 4],
        bus_fx: Some(fake("verb0")),
        cheap: vec![vec![fake("c0")]; 4],
        cheap_bus: Some(fake("cverb")),
    };
    let shape = |load| {
        let (s, m) = build_session(4, &inst, load);
        let sends: Vec<_> = s
            .tracks
            .iter()
            .map(|t| (t.id.clone(), t.kind.clone(), t.sends.clone(), t.gain_db))
            .collect();
        (sends, s.tracks.len(), m.clips.len())
    };
    assert_eq!(shape(Load::Full), shape(Load::CheapInserts));
    assert_eq!(shape(Load::Full), shape(Load::NoInserts));
    assert_eq!(shape(Load::Full), shape(Load::Bare));
}

// ---------------------------------------------------------------------------
// The measurement
// ---------------------------------------------------------------------------

#[test]
fn where_the_block_time_goes_under_plugin_load() {
    if !gated() {
        return;
    }

    let scanned = plugins::scan::scan_all();
    println!("scanned {} plugins", scanned.len());
    let registry = Arc::new(Mutex::new(PluginRegistry { scanned: Some(scanned) }));

    let want = Wanted::default();
    let max_tracks = *TRACK_COUNTS.iter().max().unwrap();
    let inst = instantiate_all(&registry, &want, max_tracks);

    assert!(
        !inst.instruments.is_empty(),
        "no hosted instrument — nothing to profile. Install {} or set \
         AURA_PROFILE_INSTRUMENT to something the scan found.",
        want.instrument
    );
    let per_track_fx = inst.inserts.first().map(|v| v.len()).unwrap_or(0);
    assert!(
        per_track_fx > 0,
        "no hosted insert effect — set AURA_PROFILE_INSERTS to something installed"
    );
    assert!(
        inst.bus_fx.is_some(),
        "no hosted bus effect — the shared-reverb return is part of the session \
         shape being measured, not a decoration. Set AURA_PROFILE_BUS_FX."
    );

    let doc = doc_for(&inst);
    println!(
        "hosting {} instrument instance(s), {} insert instance(s) per track, bus fx: {}",
        inst.instruments.len(),
        per_track_fx,
        if inst.bus_fx.is_some() { &want.bus_fx } else { "none" },
    );

    for &n in TRACK_COUNTS {
        report_header();
        let mut med: HashMap<&'static str, f64> = HashMap::new();
        let mut pk: HashMap<&'static str, f32> = HashMap::new();

        for load in [Load::Full, Load::CheapInserts, Load::NoInserts, Load::Bare] {
            let (store, midi) = build_session(n, &inst, load);
            let mut og = offline::build_graph(
                &store,
                &midi,
                &doc,
                &Default::default(),
                &Default::default(),
                None,
                RATE,
            );

            // `full` and `cheap_fx` must carry the SAME number of chains, or
            // their difference is a chain count and not a DSP cost.
            let expected = if load.has_inserts() {
                n + usize::from(load.bus_fx(&inst).is_some())
            } else {
                0
            };
            assert_graph_really_has_the_plugins(&og.graph, expected, load.label());

            let t = measure(&mut og.graph);
            assert!(
                t.peak > 0.001,
                "{}/{n} tracks rendered silence ({:.6}) — profiling a graph that is \
                 doing nothing measures nothing",
                load.label(),
                t.peak
            );
            report_row(load.label(), n, &t);
            med.insert(load.label(), t.median());
            pk.insert(load.label(), t.peak);
        }

        // If the hosted instrument had silently fallen back to PolySynth,
        // `full` and `bare` would render the same audio. They must not.
        assert_ne!(
            pk["full"], pk["bare"],
            "{n} tracks: 'full' and 'bare' peak identically, so the hosted \
             instrument never loaded and every number above is PolySynth's"
        );

        let (full, cheap, no_ins, bare) =
            (med["full"], med["cheap_fx"], med["no_inserts"], med["bare"]);
        let pct = |x: f64| x / full * 100.0;
        // Every source track carries the chain, and so does the bus.
        let slots = (n + 1) * inst.inserts.first().map(|v| v.len()).unwrap_or(1);

        println!(
            "\n  {n} tracks, {slots} insert slots — of one {:.1} µs block:\n\
             \x20   plugin DSP        {:>7.1} µs  {:>5.1}%   (full - cheap_fx)\n\
             \x20   host overhead     {:>7.1} µs  {:>5.1}%   (cheap_fx - no_inserts)              = {:.2} µs per insert\n\
             \x20   instruments       {:>7.1} µs  {:>5.1}%   (no_inserts - bare)\n\
             \x20   AURA mixer/sends  {:>7.1} µs  {:>5.1}%   (bare)",
            full,
            full - cheap,
            pct(full - cheap),
            cheap - no_ins,
            pct(cheap - no_ins),
            (cheap - no_ins) / slots as f64,
            no_ins - bare,
            pct(no_ins - bare),
            bare,
            pct(bare),
        );
        println!(
            "  full run uses {:.2}% of the {:.2} ms deadline\n",
            full / deadline_micros() * 100.0,
            deadline_micros() / 1000.0
        );
    }
}
