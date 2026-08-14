//! Offline (faster-than-realtime) master rendering — wave-1D (song export).
//!
//! Promotes the headless-render machinery the tests already rely on
//! (`midi::playback::tests::render_graph`, `control::tests::
//! seeded_demo_clips_render_nonzero_audio`) to production code: build the
//! SAME `RtGraph` the transport plays (audio clips decoded via
//! `engine::load_wav` + `dsp::linear_resample`, midi tracks as LIVE
//! instrument nodes via `midi::playback::append_from`) and pump it through
//! the REAL `mixer::render` block loop — no cpal device, no RT thread.
//!
//! Everything here is exclusively owned by the calling (export) thread: the
//! graph is built from FRESH live-node cells (a private `LiveNodeRegistry`,
//! never the engine's), so the `LiveNodeCell` safety contract ("one thread,
//! one snapshot") holds and the render is deterministic — two renders of the
//! same snapshot are bit-exact.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use super::dsp::linear_resample;
use super::engine::load_wav;
use super::mixer;
use super::rt::{ParamTable, RtClip, RtClipData, RtGraph, RtTrack, FLAG_MUTE, FLAG_SOLO};
use super::sampler::SamplerBank;
use super::transport::LoopSpec;
use super::types::{derive_slots, Store};
use crate::midi::playback::{append_from, LiveNodeRegistry};
use crate::midi::MidiStore;

/// Block size for offline rendering (matches `MAX_LIVE_BLOCK`, the largest
/// contiguous run a live node processes). Fixed so renders are reproducible
/// regardless of the device's callback size.
pub const OFFLINE_BLOCK: usize = super::rt::MAX_LIVE_BLOCK;

/// A freshly built, exclusively owned render graph (its parameter table
/// mirroring the store's gain/pan/mute/solo lives ON the graph — round-2
/// §2.4, semantics rule 4 — not as a parallel field) plus the song end.
pub struct OfflineGraph {
    pub graph: RtGraph,
    /// Last audible sample: max of clip ends and the last live note edge
    /// (note-offs included). 0 for an empty song. The release tail is the
    /// caller's business.
    pub end_samples: u64,
}

/// Build the offline graph for a project snapshot at `rate` — the exact
/// mirror of `engine::Control::rebuild` + `ensure_loaded`, but with a private
/// sample cache and FRESH live nodes (never the engine's shared cells).
/// Unreadable clip sources are skipped with a warning, matching playback
/// (what you hear is what you export). Slots are derived fresh from display
/// order (round-2 §2.4) — this graph is exclusively owned, so there is no
/// cross-generation aliasing concern to begin with.
///
/// KNOWN DIVERGENCE, BOTH HALVES (Track D, automation audible): no
/// `AutomationDoc` reaches here, so an export ignores automation that live
/// playback obeys.
/// - Track-gain lanes: `rebuild` attaches compiled ramps to its graph
///   (`RtGraph::set_gain_ramps`); this graph has none, so an exported track
///   plays at its fader value throughout.
/// - Plugin-param lanes: worse than absent. They are driven HOST-side by the
///   engine's `ParamAutomationDriver`, and the offline nodes talk to those
///   same host instances — so an export inherits whatever value the last live
///   playthrough left in the plugin. Bouncing a project after playing it and
///   bouncing it from a fresh launch can produce different WAVs.
///
/// Closing it needs the lanes in `ExportSnapshot`, one `compile_gain_ramps`
/// call, and a decision about how a bounce evaluates plugin params (the live
/// driver's ≤2 ms tick has no meaning offline). A Task 10 item, deliberately
/// not smuggled into the engine task's diff.
pub fn build_graph(
    store: &Store,
    midi: &MidiStore,
    plugins: &crate::control::session::PluginDoc,
    bank: Option<&SamplerBank>,
    rate: u32,
) -> OfflineGraph {
    let slots = derive_slots(&store.tracks);
    // Finding 4: `ParamTable::default()` is a fixed 64-slot table (kept only
    // for tests that poke slots without sizing explicitly). Production
    // graphs must size PER-GRAPH to the actual track count (round-2 §2.4) —
    // using the default here silently dropped every track at slot >= 64 from
    // offline export.
    let params = Arc::new(ParamTable::with_slots(store.tracks.len()));
    let mut tracks: Vec<RtTrack> = Vec::with_capacity(store.tracks.len());
    for t in &store.tracks {
        let slot = slots[&t.id];
        params.set_gain_linear(slot, mixer::db_to_linear(t.gain_db));
        params.set_pan(slot, t.pan as f32);
        params.set_flag(slot, FLAG_MUTE, t.muted);
        params.set_flag(slot, FLAG_SOLO, t.soloed);
        let clips: Vec<RtClip> = store
            .clips
            .iter()
            .filter(|c| c.track_id == t.id)
            .filter_map(|c| {
                let path = store.abs_path(&c.source_path)?;
                match load_wav(&path) {
                    Ok((channels, file_rate, samples)) => {
                        let data =
                            linear_resample(&samples, channels as usize, file_rate, rate);
                        Some(RtClip {
                            start: c.timeline_start_samples,
                            offset: c.offset_samples,
                            len: c.length_samples,
                            gain: mixer::db_to_linear(c.gain_db),
                            fade_in: c.fade_in_samples,
                            fade_out: c.fade_out_samples,
                            samples: Arc::new(RtClipData { channels, data }),
                        })
                    }
                    Err(e) => {
                        log::warn!("offline render: cannot load {}: {e}", path.display());
                        None
                    }
                }
            })
            .collect();
        tracks.push(RtTrack::clips(slot, clips));
    }
    params.any_solo.store(store.any_solo(), Relaxed);

    // Midi tracks as LIVE instrument nodes — a private registry, so the
    // cells are fresh (deterministic voice state) and exclusively ours.
    let mut nodes = LiveNodeRegistry::default();
    append_from(midi, store, plugins, &slots, rate, bank, &mut nodes, &mut tracks);

    let end_samples = song_end(&tracks);
    OfflineGraph { graph: RtGraph::new(tracks, 0, params), end_samples }
}

/// Last audible sample of a track set: clip `start + len` and the last
/// pre-scheduled live event (the sorted event list ends with the final
/// note-off edge).
pub fn song_end(tracks: &[RtTrack]) -> u64 {
    let mut end = 0u64;
    for t in tracks {
        for c in &t.clips {
            end = end.max(c.start.saturating_add(c.len));
        }
        if let Some(live) = &t.live {
            if let Some(ev) = live.events.last() {
                end = end.max(ev.sample);
            }
        }
    }
    end
}

/// Render `frames` frames starting at absolute timeline position `start`
/// through the real graph path in fixed [`OFFLINE_BLOCK`] chunks. Returns
/// interleaved stereo f32 (`frames * 2` samples) with `master_gain` applied
/// to the summed bus. The first block carries the discontinuity flag (the
/// automation seam gets the absolute base position; a mid-song start never
/// inherits stale ramp cursors). Loop-region bounces are expressed as
/// `start`/`frames` — the render itself is always linear (`LoopSpec::OFF`).
///
/// `on_progress(done_frames, total_frames)` fires after every block.
pub fn render(
    graph: &mut RtGraph,
    start: u64,
    frames: u64,
    rate: u32,
    master_gain: f32,
    on_progress: &mut dyn FnMut(u64, u64),
) -> Vec<f32> {
    let mut out = vec![0.0f32; frames as usize * 2];
    let mut pos = start;
    let mut done = 0u64;
    let mut discontinuity = true;
    for chunk in out.chunks_mut(OFFLINE_BLOCK * 2) {
        mixer::render(graph, pos, &LoopSpec::OFF, chunk, 2, rate, discontinuity, None);
        discontinuity = false;
        let n = (chunk.len() / 2) as u64;
        pos += n;
        done += n;
        on_progress(done, frames);
    }
    if (master_gain - 1.0).abs() > f32::EPSILON {
        for s in &mut out {
            *s *= master_gain;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::types::TrackState;
    use crate::ids::NoteId;
    use crate::midi::types::{MeterEvent, TempoEvent};
    use crate::midi::{MidiClip, MidiNote};

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

    /// The seeded demo project as a (Store, MidiStore) pair — the same
    /// content `ControlPlane::seed_demo_project` produces.
    fn demo_project() -> (Store, MidiStore) {
        let mut store = Store::default();
        for id in ["keys", "bass"] {
            store.tracks.push(track(id, "midi"));
        }
        let (arp, groove) = crate::control::demo_seed_clips("keys", "bass", 960);
        let midi = MidiStore {
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![arp, groove],
            loaded_dir: None,
            dirty: false,
        };
        (store, midi)
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Full-song offline render of the seeded demo: non-zero, correct length
    /// (song end + tail), and DETERMINISTIC — two independent build+render
    /// passes are bit-exact.
    #[test]
    fn demo_song_offline_render_is_audible_correct_length_and_deterministic() {
        const RATE: u32 = 48_000;
        let (store, midi) = demo_project();
        let render_once = || {
            let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), None, RATE);
            // Engine-rebuild parity: one (empty) clip track per store track
            // plus one LIVE track per audible midi track.
            let live = og.graph.tracks.iter().filter(|t| t.live.is_some()).count();
            assert_eq!(live, 2, "both midi tracks render live");
            // Demo song: 4 bars @120bpm, ppq 960 -> last arp note-off at
            // tick 15336 -> 15336 * 25 samples/tick = 383400.
            assert_eq!(og.end_samples, 383_400, "song end = last note-off");
            let frames = og.end_samples + RATE as u64 / 2; // 0.5 s tail
            let mut blocks = 0u64;
            let out = render(
                &mut og.graph,
                0,
                frames,
                RATE,
                1.0,
                &mut |_d, t| {
                    blocks += 1;
                    assert_eq!(t, frames);
                },
            );
            assert_eq!(out.len(), frames as usize * 2, "length exact");
            assert_eq!(blocks, frames.div_ceil(OFFLINE_BLOCK as u64));
            out
        };
        let a = render_once();
        let b = render_once();
        assert!(peak(&a) > 0.05, "seeded song renders audibly offline");
        assert_eq!(a, b, "offline render is bit-exact across runs");
        // The tail after the last release is silent (no hung voices).
        let tail_start = (383_400 + 10_000) * 2;
        assert_eq!(peak(&a[tail_start..]), 0.0, "release tail decays to silence");
    }

    /// Region rendering semantics: length is exact, notes starting BEFORE the
    /// region start are not (re)triggered, notes inside it are audible.
    #[test]
    fn region_render_has_exact_length_and_skips_earlier_note_ons() {
        const RATE: u32 = 48_000;
        let mut store = Store::default();
        store.tracks.push(track("m1", "midi"));
        // ppq 960 @120bpm -> 25 samples/tick. Note A: on at tick 0 (sample 0),
        // long. Note B: on at tick 1200 (sample 30000).
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 800, key: 60, velocity: 100, channel: 0, note_id: NoteId(1) },
            MidiNote { tick: 1200, length_ticks: 400, key: 72, velocity: 100, channel: 0, note_id: NoteId(2) },
        ];
        let midi = MidiStore {
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![MidiClip {
                id: "c1".into(),
                track_id: "m1".into(),
                name: "c".into(),
                timeline_start_ticks: 0,
                length_ticks: 1920,
                notes,
                next_note_id: 3,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track("m1"),
                content_length_ticks: None,
            }],
            loaded_dir: None,
            dirty: false,
        };
        let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), None, RATE);
        // Region [24000, 44000): starts after note A's on (skipped) and
        // before note B's on at 30000 (audible).
        let (start, end) = (24_000u64, 44_000u64);
        let out = render(
            &mut og.graph,
            start,
            end - start,
            RATE,
            1.0,
            &mut |_, _| {},
        );
        assert_eq!(out.len(), (end - start) as usize * 2, "region length exact");
        let mono: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert_eq!(peak(&mono[..5_500]), 0.0, "note-on before the region never fires");
        assert!(peak(&mono[6_500..]) > 0.02, "note inside the region is audible");
    }

    /// Audio-clip decode path: a WAV on disk reaches the offline mix with
    /// clip placement, clip gain and the track's constant-power pan applied —
    /// and the master gain scales the bus.
    #[test]
    fn audio_clips_are_decoded_placed_and_scaled() {
        const RATE: u32 = 48_000;
        let dir = std::env::temp_dir().join(format!(
            "aura-offline-clip-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("audio")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(dir.join("audio/c1.wav"), spec).unwrap();
        for _ in 0..1_000 {
            w.write_sample(0.5f32).unwrap();
        }
        w.finalize().unwrap();

        let mut store = Store::default();
        store.project_dir = Some(dir.clone());
        store.tracks.push(track("a1", "audio"));
        store.clips.push(crate::audio::types::Clip {
            id: "c1".into(),
            track_id: "a1".into(),
            name: "c1".into(),
            source_path: "audio/c1.wav".into(),
            source_id: crate::ids::SourceId::default(),
            source_channels: 1,
            source_sample_rate: RATE,
            source_length_samples: 1_000,
            timeline_start_samples: 100,
            offset_samples: 0,
            length_samples: 500,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("a1"),
        });
        let midi = MidiStore { clips: vec![], ..MidiStore::default() };
        let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), None, RATE);
        assert_eq!(og.end_samples, 600, "clip end = start + len");
        let out = render(&mut og.graph, 0, 700, RATE, 0.5, &mut |_, _| {});
        let center = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * 0.5; // sample * pan * master
        assert_eq!(out[0], 0.0, "silent before the clip");
        assert!((out[150 * 2] - center).abs() < 1e-6, "clip audible, panned, master-scaled");
        assert!((out[150 * 2 + 1] - center).abs() < 1e-6);
        assert_eq!(out[650 * 2], 0.0, "silent past the clip");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Finding 4: `build_graph` used to allocate a fixed 64-slot
    /// `ParamTable::default()` regardless of track count — a project with
    /// more than 64 tracks silently lost every track at slot >= 64 from
    /// offline export (writes to out-of-range slots are dropped, and
    /// `og.graph.tracks` still has an `RtTrack` per store track, but its
    /// params would be UNSET past slot 63). Assert the param table is sized
    /// to the ACTUAL track count, not the historical cap.
    #[test]
    fn build_graph_sizes_params_for_more_than_sixty_four_tracks() {
        const RATE: u32 = 48_000;
        const N: usize = 80; // > the old fixed 64-slot default
        let mut store = Store::default();
        for i in 0..N {
            store.tracks.push(track(&format!("t{i}"), "audio"));
        }
        let midi = MidiStore::default();
        let og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), None, RATE);
        assert_eq!(og.graph.tracks.len(), N, "one RtTrack per store track");
        assert_eq!(
            og.graph.params.len(),
            N,
            "param table must be sized for every track, not capped at the old 64-slot default"
        );
        // The last track's slot (79, unreachable under the old fixed-64
        // table) must actually take a param write — proof the table isn't
        // silently dropping it.
        let last_slot = derive_slots(&store.tracks)[&store.tracks[N - 1].id];
        og.graph.params.set_gain_linear(last_slot, 0.25);
        assert_eq!(og.graph.params.gain[last_slot].load(std::sync::atomic::Ordering::Relaxed), 0.25f32.to_bits());
    }
}
