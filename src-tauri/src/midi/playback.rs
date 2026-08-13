//! MIDI playback integration with the audio engine — the LIVE instrument
//! node seam (phase 3, ARCHITECTURE §15; supersedes the phase-2 control-side
//! pre-render).
//!
//! Strategy: every audible midi track becomes an [`RtTrack`] carrying a
//! [`LiveSource`] — a shared [`LiveNodeCell`] (PolySynth fallback,
//! SamplerNode when the track's `instrument_id` resolves in the bank, plugin
//! node for `plugin:<instanceId>` refs once zones P1/P2 land) plus the
//! snapshot's pre-scheduled note events. Ticks are converted to absolute
//! engine samples HERE on the control thread via [`TempoMap`]; the RT thread
//! only ever slices the sorted event list into block-relative
//! [`BlockNoteEvent`]s (`mixer::render_live`), never sees ticks, and never
//! allocates. The §2.1 contract and the §10.1 RCU swap discipline hold; node
//! STATE (voices, plugin instances) survives graph swaps because successive
//! snapshots share the node cell through [`LiveNodeRegistry`].
//!
//! `engine.rs::rebuild` holds the session lock directly (store + midi behind
//! one guard) and calls [`append_from`] with both fields; everything else
//! lives here. Cross-cutting callers that don't already hold a session
//! handle (e.g. [`super::notify_project_opened`]) reach it through a
//! process-global registered by `MidiState::shared()` during app setup
//! (lib.rs) so no frozen signatures change.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::audio::dsp::LiveInstrument;
use crate::audio::rt::{LiveNodeCell, LiveSource, RtTrack, MAX_LIVE_BLOCK};
use crate::audio::sampler::SamplerBank;
use crate::audio::sampler_engine::SamplerNode;
use crate::audio::types::{Store, TrackState};
use crate::control::Session;

use super::schedule::{self, AbsNoteEvent};
use super::synth::PolySynth;
use super::tempo::TempoMap;
use super::MidiStore;

use crate::audio::dsp::AudioProcessor;

static PLAYBACK_SESSION: OnceLock<Arc<Mutex<Session>>> = OnceLock::new();

/// Register the app's session for cross-cutting reaches (e.g. midi resync
/// on project open) that run outside the engine's own held lock. Called
/// (indirectly) once from lib.rs setup via `MidiState::shared()`. First
/// registration wins; tests use [`append_from`] directly and never touch
/// the global.
pub fn register_store(session: Arc<Mutex<Session>>) {
    let _ = PLAYBACK_SESSION.set(session);
}

/// The registered app-wide session, if any (None in unit tests).
pub fn registered_store() -> Option<&'static Arc<Mutex<Session>>> {
    PLAYBACK_SESSION.get()
}

// ---------------------------------------------------------------------------
// Live-node registry (control-thread state, owned by engine::Control)
// ---------------------------------------------------------------------------

/// Live instrument nodes keyed by track id, shared across successive graph
/// snapshots so voice/plugin state survives rebuilds. `key` describes what
/// the node was built from (`"synth@48000"`, `"sampler:<id>@48000"`,
/// `"plugin:<instanceId>@48000"`); a key change (instrument rebound, engine
/// rate change) replaces the node — the retired cell is freed control-side
/// when the last snapshot referencing it is dropped.
#[derive(Default)]
pub struct LiveNodeRegistry {
    entries: HashMap<String, (String, Arc<LiveNodeCell>)>,
}

impl LiveNodeRegistry {
    /// Reuse the track's node when `key` matches, otherwise build one with
    /// `make` (called only when needed — plugin instantiation is expensive).
    /// `make` returning None (e.g. an unknown plugin instance) leaves the
    /// registry untouched and returns None so the caller can fall back.
    pub fn resolve_with(
        &mut self,
        track_id: &str,
        key: &str,
        make: impl FnOnce() -> Option<Box<dyn LiveInstrument>>,
    ) -> Option<Arc<LiveNodeCell>> {
        if let Some((k, cell)) = self.entries.get(track_id) {
            if k == key {
                return Some(cell.clone());
            }
        }
        let cell = LiveNodeCell::new(make()?);
        self.entries
            .insert(track_id.to_string(), (key.to_string(), cell.clone()));
        Some(cell)
    }

    /// Drop entries for tracks that no longer render live.
    pub fn retain_tracks(&mut self, live: &HashSet<String>) {
        self.entries.retain(|id, _| live.contains(id));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The node key currently registered for a track (test/diagnostic aid).
    pub fn key_of(&self, track_id: &str) -> Option<&str> {
        self.entries.get(track_id).map(|(k, _)| k.as_str())
    }
}

// ---------------------------------------------------------------------------
// Engine hook
// ---------------------------------------------------------------------------

/// Core of the engine's `rebuild` live-track step (called from
/// `audio::engine::Control::rebuild` with the session lock held, on the
/// engine control thread — never the RT thread). Appends one live `RtTrack`
/// per audible midi track. `bank` = None means "no sampler instruments
/// available" (instrument-bound tracks fall back to PolySynth).
pub fn append_from(
    midi: &MidiStore,
    store: &Store,
    rate: u32,
    bank: Option<&SamplerBank>,
    nodes: &mut LiveNodeRegistry,
    out: &mut Vec<RtTrack>,
) {
    let mut live_ids: HashSet<String> = HashSet::new();
    if rate != 0 && !midi.clips.is_empty() {
        match TempoMap::new(midi.ppq, midi.tempo_events.clone(), rate) {
            Ok(map) => {
                for t in store.tracks.iter().filter(|t| t.kind == "midi") {
                    let Some(&slot) = store.slots.get(&t.id) else { continue };
                    let events = track_events(midi, &t.id, &map);
                    if events.is_empty() {
                        continue;
                    }
                    let node = node_for_track(t, bank, rate, nodes);
                    live_ids.insert(t.id.clone());
                    out.push(RtTrack {
                        slot,
                        clips: Vec::new(),
                        live: Some(LiveSource { node, events: Arc::new(events) }),
                    });
                }
            }
            Err(e) => log::warn!("midi playback: invalid tempo map ({e}); midi muted"),
        }
    }
    // Tracks that stopped rendering live release their nodes (freed here on
    // the control thread once the last snapshot referencing them retires).
    nodes.retain_tracks(&live_ids);
}

/// All of a track's clip notes as absolute-sample note edges, merged and
/// sorted (offs before ons at equal positions).
fn track_events(midi: &MidiStore, track_id: &str, map: &TempoMap) -> Vec<AbsNoteEvent> {
    let mut events: Vec<AbsNoteEvent> = Vec::new();
    for c in midi
        .clips
        .iter()
        .filter(|c| c.track_id == track_id && !c.notes.is_empty())
    {
        events.extend(schedule::clip_events(c, map));
    }
    events.sort_by_key(|e| (e.sample, e.velocity));
    events
}

/// Resolve the live node for a midi track. Fallback ladder:
/// `plugin:<id>` ref -> plugin node (zone P1/P2; silent stub today) ->
/// sampler instrument -> built-in PolySynth. Never returns "no node": MIDI
/// stays audible exactly as in phase 2.
fn node_for_track(
    t: &TrackState,
    bank: Option<&SamplerBank>,
    rate: u32,
    nodes: &mut LiveNodeRegistry,
) -> Arc<LiveNodeCell> {
    if let Some(id) = t.instrument_id.as_deref() {
        if let Some(pid) = id.strip_prefix("plugin:") {
            let key = format!("plugin:{pid}@{rate}");
            if let Some(cell) =
                nodes.resolve_with(&t.id, &key, || crate::plugins::live_node_for(pid, rate))
            {
                return cell;
            }
            log::warn!(
                "midi playback: track {} plugin instance {pid} is unavailable; using PolySynth",
                t.id
            );
        } else if let Some(compiled) = bank.and_then(|b| b.compiled(id)) {
            let key = format!("sampler:{id}@{rate}");
            if let Some(cell) = nodes.resolve_with(&t.id, &key, move || {
                let mut node = SamplerNode::new((*compiled).clone());
                node.prepare(rate, MAX_LIVE_BLOCK);
                Some(Box::new(node))
            }) {
                return cell;
            }
        } else {
            log::warn!(
                "midi playback: track {} instrument {id} is not loaded; using PolySynth",
                t.id
            );
        }
    }
    let key = format!("synth@{rate}");
    nodes
        .resolve_with(&t.id, &key, || {
            let mut synth = PolySynth::new();
            synth.prepare(rate, MAX_LIVE_BLOCK);
            Some(Box::new(synth))
        })
        .expect("PolySynth construction is infallible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer;
    use crate::audio::rt::{ParamTable, RtGraph};
    use crate::audio::transport::LoopSpec;
    use crate::audio::types::TrackState;
    use crate::midi::types::{MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};

    fn midi_store_with(clips: Vec<MidiClip>) -> MidiStore {
        MidiStore {
            ppq: DEFAULT_PPQ,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            clips,
            loaded_dir: None,
        }
    }

    fn track(id: &str, kind: &str) -> TrackState {
        TrackState {
            id: id.into(),
            name: id.into(),
            kind: kind.into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
        }
    }

    fn clip(track_id: &str, start_ticks: u64, len_ticks: u64, notes: Vec<MidiNote>) -> MidiClip {
        MidiClip {
            id: uuid::Uuid::new_v4().to_string(),
            track_id: track_id.into(),
            name: "c".into(),
            timeline_start_ticks: start_ticks,
            length_ticks: len_ticks,
            notes,
        }
    }

    /// Render `frames` of a live graph headlessly through the REAL RT path
    /// (`mixer::render`, block-sized chunks) and return the interleaved
    /// stereo output.
    fn render_graph(tracks: Vec<RtTrack>, frames: usize, rate: u32) -> Vec<f32> {
        let mut g = RtGraph::new(tracks);
        let params = ParamTable::default();
        let mut out = vec![0.0f32; frames * 2];
        let mut pos = 0u64;
        for chunk in out.chunks_mut(512 * 2) {
            mixer::render(&mut g, &params, pos, &LoopSpec::OFF, chunk, 2, rate, false);
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

    /// THE seam test: a midi track becomes a live RtTrack whose PolySynth
    /// node renders audible, correctly-placed audio through the real
    /// `mixer::render` path — headless, no device, no tauri.
    #[test]
    fn midi_tracks_become_live_rt_tracks_with_audible_render() {
        let mut store = Store::default();
        store.alloc_slot("m1");
        store.tracks.push(track("m1", "midi"));
        store.alloc_slot("a1");
        store.tracks.push(track("a1", "audio"));

        let midi = midi_store_with(vec![clip(
            "m1",
            960, // one beat in @120bpm/48k = sample 24000
            960,
            vec![MidiNote { tick: 0, length_ticks: 480, key: 69, velocity: 100, channel: 0 }],
        )]);

        let mut nodes = LiveNodeRegistry::default();
        let mut out = Vec::new();
        append_from(&midi, &store, 48_000, None, &mut nodes, &mut out);
        assert_eq!(out.len(), 1, "only the midi track renders");
        assert_eq!(out[0].slot, *store.slots.get("m1").unwrap());
        let live = out[0].live.as_ref().expect("live source attached");
        // Events converted tick->sample on the control side.
        assert_eq!(live.events[0].sample, 24_000);
        assert_eq!(live.events[0].velocity, 100);
        assert_eq!(live.events[1].sample, 24_000 + 12_000, "off after 480 ticks");
        assert_eq!(nodes.key_of("m1"), Some("synth@48000"));

        let audio = mono_of(&render_graph(out, 48_000, 48_000));
        assert_eq!(peak(&audio[..23_900]), 0.0, "silent before the note");
        assert!(peak(&audio[24_500..30_000]) > 0.02, "note audible at placement");
        // 80 ms release after the off at 36000 -> silent again by 44000.
        assert_eq!(peak(&audio[44_000..]), 0.0, "silence after release");
    }

    #[test]
    fn audio_tracks_and_empty_clips_are_skipped() {
        let mut store = Store::default();
        store.alloc_slot("a1");
        store.tracks.push(track("a1", "audio"));
        store.alloc_slot("m1");
        store.tracks.push(track("m1", "midi"));
        let midi = midi_store_with(vec![
            clip("a1", 0, 960, vec![MidiNote { tick: 0, length_ticks: 10, key: 60, velocity: 100, channel: 0 }]),
            clip("m1", 0, 960, vec![]), // no notes
        ]);
        let mut nodes = LiveNodeRegistry::default();
        let mut out = Vec::new();
        append_from(&midi, &store, 48_000, None, &mut nodes, &mut out);
        assert!(out.is_empty());
        assert!(nodes.is_empty(), "no nodes retained for non-live tracks");
    }

    /// Node identity survives rebuilds (same key -> same cell) and is
    /// replaced when the binding key changes; removed tracks are pruned.
    #[test]
    fn registry_reuses_and_prunes_nodes_across_rebuilds() {
        let mut store = Store::default();
        store.alloc_slot("m1");
        store.tracks.push(track("m1", "midi"));
        let midi = midi_store_with(vec![clip(
            "m1",
            0,
            960,
            vec![MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 90, channel: 0 }],
        )]);
        let mut nodes = LiveNodeRegistry::default();
        let mut out1 = Vec::new();
        append_from(&midi, &store, 48_000, None, &mut nodes, &mut out1);
        let cell1 = out1[0].live.as_ref().unwrap().node.clone();
        let mut out2 = Vec::new();
        append_from(&midi, &store, 48_000, None, &mut nodes, &mut out2);
        let cell2 = out2[0].live.as_ref().unwrap().node.clone();
        assert!(Arc::ptr_eq(&cell1, &cell2), "rebuild reuses the same node");

        // Rate change -> new key -> new node.
        let mut out3 = Vec::new();
        append_from(&midi, &store, 44_100, None, &mut nodes, &mut out3);
        let cell3 = out3[0].live.as_ref().unwrap().node.clone();
        assert!(!Arc::ptr_eq(&cell1, &cell3), "rate change replaces the node");

        // Track gone -> registry pruned.
        let empty = midi_store_with(vec![]);
        let mut out4 = Vec::new();
        append_from(&empty, &store, 48_000, None, &mut nodes, &mut out4);
        assert!(out4.is_empty());
        assert!(nodes.is_empty(), "stale nodes pruned");
    }

    // ---- sampler instrument routing (live through the graph) --------------

    use crate::audio::sampler::SamplerBank;
    use crate::audio::sampler_engine::CompiledInstrument;
    use crate::audio::sampler_voice::testutil::{estimate_freq, region, sine_sample};

    /// Bank with one compiled instrument: a mono sine at `freq` Hz rooted at
    /// key 69 across the whole key range.
    fn bank_with(id: &str, freq: f64, rate: u32) -> SamplerBank {
        let compiled = CompiledInstrument {
            id: id.into(),
            name: id.into(),
            rate,
            regions: Arc::new(vec![region(sine_sample(freq, rate, 1.0, 0.8))]),
        };
        let mut bank = SamplerBank::default();
        bank.compiled.insert(id.to_string(), Arc::new(compiled));
        bank
    }

    /// A midi track with a bound + loaded instrument renders LIVE through
    /// the SAMPLER node (pitch = the sample's 330 Hz sine at its root key),
    /// while an unbound midi track keeps the PolySynth fallback (440 Hz for
    /// A4) — both through the real `mixer::render` graph path, headless.
    #[test]
    fn instrument_tracks_use_sampler_and_unbound_tracks_use_polysynth() {
        const RATE: u32 = 48_000;
        let mut store = Store::default();
        store.alloc_slot("sampled");
        let mut t_sampled = track("sampled", "midi");
        t_sampled.instrument_id = Some("inst-330".into());
        store.tracks.push(t_sampled);
        store.alloc_slot("fallback");
        store.tracks.push(track("fallback", "midi"));

        // A4 (key 69) for half a second on both tracks.
        let note = MidiNote { tick: 0, length_ticks: 960, key: 69, velocity: 110, channel: 0 };
        let midi = midi_store_with(vec![
            clip("sampled", 0, 960, vec![note.clone()]),
            clip("fallback", 0, 960, vec![note]),
        ]);
        let bank = bank_with("inst-330", 330.0, RATE);

        let mut nodes = LiveNodeRegistry::default();
        let mut out = Vec::new();
        append_from(&midi, &store, RATE, Some(&bank), &mut nodes, &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(nodes.key_of("sampled"), Some("sampler:inst-330@48000"));
        assert_eq!(nodes.key_of("fallback"), Some("synth@48000"));

        // Render each track's graph separately to isolate the signals.
        let (t_s, t_f) = {
            let mut it = out.into_iter();
            (it.next().unwrap(), it.next().unwrap())
        };
        let sampled = mono_of(&render_graph(vec![t_s], 24_000, RATE));
        let fallback = mono_of(&render_graph(vec![t_f], 24_000, RATE));
        assert!(peak(&sampled) > 0.05, "sampler render audible");
        assert!(peak(&fallback) > 0.05, "synth render audible");
        let f_sampled = estimate_freq(&sampled[1000..20_000], RATE, 100.0, 800.0);
        let f_fallback = estimate_freq(&fallback[1000..20_000], RATE, 100.0, 800.0);
        assert!(
            (f_sampled - 330.0).abs() / 330.0 < 0.02,
            "sampler pitch: got {f_sampled:.1} Hz, want ~330 Hz"
        );
        assert!(
            (f_fallback - 440.0).abs() / 440.0 < 0.02,
            "polysynth pitch: got {f_fallback:.1} Hz, want ~440 Hz"
        );
    }

    /// An instrument id that is not loaded in the bank must never mute the
    /// track: the PolySynth fallback renders instead. Same guarantee for a
    /// `plugin:` ref with no live plugin backing it (zones P1/P2 not landed).
    #[test]
    fn unknown_instrument_and_plugin_refs_fall_back_to_polysynth() {
        const RATE: u32 = 48_000;
        for id in ["ghost-instrument", "plugin:00000000-0000-4000-8000-000000000000"] {
            let mut store = Store::default();
            store.alloc_slot("m1");
            let mut t = track("m1", "midi");
            t.instrument_id = Some(id.into());
            store.tracks.push(t);
            let midi = midi_store_with(vec![clip(
                "m1",
                0,
                960,
                vec![MidiNote { tick: 0, length_ticks: 960, key: 69, velocity: 100, channel: 0 }],
            )]);
            let bank = SamplerBank::default();
            let mut nodes = LiveNodeRegistry::default();
            let mut out = Vec::new();
            append_from(&midi, &store, RATE, Some(&bank), &mut nodes, &mut out);
            assert_eq!(out.len(), 1, "track still renders ({id})");
            let mono = mono_of(&render_graph(out, 24_000, RATE));
            let f = estimate_freq(&mono[1000..20_000], RATE, 100.0, 800.0);
            assert!(
                (f - 440.0).abs() / 440.0 < 0.02,
                "fallback synth pitch for {id}, got {f:.1}"
            );
        }
    }

    /// FULL integration path with zero ML deps: simulate-generated .sfz ->
    /// load + compile into a bank -> midi track bound to it -> LIVE sampler
    /// node in the RCU graph -> audible, correctly pitched audio out of
    /// `mixer::render`. Headless (no device, no tauri).
    #[test]
    fn simulate_built_instrument_renders_midi_track_audibly_through_graph() {
        use std::process::Command;
        const RATE: u32 = 48_000;
        let script = match std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.join("sidecars/stable_audio_sfz_worker.py"))
        {
            Some(p) if p.exists() => p,
            _ => {
                eprintln!("skipping: sidecars dir not found");
                return;
            }
        };
        let Some(python) = ["python3", "python"]
            .iter()
            .find(|p| Command::new(p).arg("--version").output().is_ok())
        else {
            eprintln!("skipping: python not available");
            return;
        };
        let dir = std::env::temp_dir().join(format!(
            "aura-midi-sampler-e2e-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let params = serde_json::json!({
            "prompt": "soft plucked strings",
            "name": "GraphPluck",
            "outputDir": dir.to_string_lossy(),
            "keys": [60],
            "velocityLayers": 1,
        });
        let out_run = Command::new(python)
            .arg(&script)
            .arg("--simulate")
            .arg("--params-json")
            .arg(params.to_string())
            .output()
            .expect("run worker");
        assert!(out_run.status.success(), "{}", String::from_utf8_lossy(&out_run.stderr));
        let done: serde_json::Value = String::from_utf8_lossy(&out_run.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|v: &serde_json::Value| v["type"] == "done")
            .expect("done event");
        let sfz_path = std::path::PathBuf::from(done["result"]["sfzPath"].as_str().unwrap());

        // Load + compile exactly like `sampler_load_instrument` does.
        let instr = crate::audio::sampler::load_instrument(&sfz_path, None).unwrap();
        let compiled = crate::audio::sampler_engine::compile("gen-1", &instr, RATE).unwrap();
        let mut bank = SamplerBank::default();
        bank.compiled.insert("gen-1".into(), Arc::new(compiled));
        bank.instruments.insert("gen-1".into(), instr);

        // Midi track bound to the generated instrument, playing C4.
        let mut store = Store::default();
        store.alloc_slot("m1");
        let mut t = track("m1", "midi");
        t.instrument_id = Some("gen-1".into());
        store.tracks.push(t);
        let midi = midi_store_with(vec![clip(
            "m1",
            960,
            1920,
            vec![MidiNote { tick: 0, length_ticks: 1920, key: 60, velocity: 100, channel: 0 }],
        )]);

        let mut nodes = LiveNodeRegistry::default();
        let mut out = Vec::new();
        append_from(&midi, &store, RATE, Some(&bank), &mut nodes, &mut out);
        assert_eq!(out.len(), 1);
        let live = out[0].live.as_ref().unwrap();
        assert_eq!(live.events[0].sample, 24_000, "tick placement preserved");

        let mono = mono_of(&render_graph(out, 72_000, RATE));
        assert_eq!(peak(&mono[..23_900]), 0.0, "silent before the clip");
        assert!(peak(&mono[24_500..]) > 0.02, "generated instrument audible");
        // Simulate samples are pitched sines: C4 ≈ 261.63 Hz.
        let f = estimate_freq(&mono[26_000..54_000], RATE, 100.0, 600.0);
        assert!(
            (f - 261.626).abs() / 261.626 < 0.02,
            "graph render pitch: got {f:.2} Hz, want ~261.63 Hz"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
