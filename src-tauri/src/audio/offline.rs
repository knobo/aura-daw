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
use super::rt::{ParamTable, RtClip, RtClipData, RtGraph, RtTrack, TrackRamps, FLAG_MUTE, FLAG_SOLO};
use super::sampler::SamplerBank;
use super::transport::LoopSpec;
use super::types::{derive_slots, mixer_slot_count, Store};
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
/// TRACK ramps (gain + pan) are compiled here exactly as `Control::rebuild`
/// compiles them (`Control::compile_automation`), so a bounce follows the
/// same curves playback does.
///
/// KNOWN DIVERGENCE, PLUGIN-PARAM LANES ONLY (Track D). Plugin-param lanes
/// are NOT evaluated by a bounce, and the value the export captures is
/// whatever the live host instance happens to hold — most recently, whatever
/// the last playthrough's `ParamAutomationDriver` left there. So the same
/// project can bounce differently after a playthrough than from a fresh
/// launch. Why it is not fixed here, precisely:
/// - The values are not ours to place. Plugin params live INSIDE the host
///   instance; the only way to set one is a host round-trip
///   (`plugins::forward_param_to_host`). Offline nodes are fresh
///   `LiveNodeCell`s but they call the SAME process-wide host instance the
///   live engine and the param panel use — there is exactly one copy of a
///   plugin instance in the process.
/// - So driving them during a bounce would write the export's automation
///   into the user's live plugin: a background bounce would move the knobs
///   under the open panel, and would fight the engine's own driver if the
///   transport is rolling. That is a worse bug than the one it fixes.
/// - A correct fix needs a bounce to own PRIVATE plugin instances: a second
///   instantiation per automated instance, seeded from the live one's
///   `save_state`, params driven per render block, and disposed at the end.
///   That is a `clap_host`/`lv2_host` API addition (instantiate-for-render +
///   per-block param application) plus an offline driver tick, i.e. the
///   node-graph round's plugin-node work (round-2 §8) — not a close-out
///   change.
///
/// Until then: a bounce reproduces plugin automation only by accident.
/// Recorded in `docs/SIDE-CHANNEL-INVENTORY.md` and in the Track D handoff.
pub fn build_graph(
    store: &Store,
    midi: &MidiStore,
    plugins: &crate::control::session::PluginDoc,
    automation: &crate::control::session::AutomationDoc,
    modulation: &crate::modulation::ModulationDoc,
    bank: Option<&SamplerBank>,
    rate: u32,
) -> OfflineGraph {
    let slots = derive_slots(&store.tracks);
    // Finding 4: `ParamTable::default()` is a fixed 64-slot table (kept only
    // for tests that poke slots without sizing explicitly). Production
    // graphs must size PER-GRAPH to the mixer-slot count (round-2 §2.4;
    // automation tracks take no slot) — using the default here silently
    // dropped every track at slot >= 64 from offline export.
    let n_slots = mixer_slot_count(&store.tracks);
    let send_slots = crate::audio::types::derive_send_slots(&store.tracks);
    let params = Arc::new(ParamTable::with_slots_and_sends(
        n_slots,
        crate::audio::types::send_slot_count(&store.tracks),
    ));
    let mut tracks: Vec<RtTrack> = Vec::with_capacity(n_slots);
    for t in &store.tracks {
        let Some(&slot) = slots.get(&t.id) else { continue };
        params.set_gain_linear(slot, mixer::db_to_linear(t.gain_db));
        params.set_pan(slot, t.pan as f32);
        params.set_flag(slot, FLAG_MUTE, t.muted);
        params.set_flag(slot, FLAG_SOLO, t.soloed);
        for snd in &t.sends {
            let Some(&idx) = send_slots.get(&snd.id) else { continue };
            params.set_send_amount_linear(idx, mixer::db_to_linear(snd.amount_db));
        }
        if crate::audio::types::is_bus_track(t) {
            // Fed by sends, compiled into `RtGraph::buses` below — no
            // source row (Plan G2), exactly as in `engine::rebuild`.
            continue;
        }
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
    append_from(
        &crate::control::snapshot::MidiSnapshot::from_store(midi),
        &store.tracks,
        &store.clips,
        plugins,
        &slots,
        rate,
        bank,
        // Deliberately EMPTY, unlike `engine::rebuild`: a hardware MIDI-out
        // route silences a track's internal instrument in the LIVE graph, so
        // you hear the external device instead of a doubled voice — but it
        // must not change a bounce. Routing is per-machine app config that
        // never travels with the project (ruling 10), so honouring it here
        // would make `export_song` render differently on the machine with the
        // synth plugged in than anywhere else, from the same project file.
        // Keeping the external device out of the bounce is what the audio
        // RETURN is for (`docs/midi-output.md` recipe 2 step 6): once a
        // return clip lands on the track, the audio-clip rule above skips the
        // internal instrument here too. To leave a routed part out of the
        // master entirely, MUTE the track — mute is document state and does
        // travel.
        &crate::midi_out::RoutedOut::default(),
        &mut nodes,
        &mut tracks,
    );

    // INSERT FX + ROUTING (Plan G1 Task 8 / G2). The bounce walks the same
    // strip the live engine does — inserts, the two compensating delays,
    // sends and bus returns — because an export that silently drops the
    // reverb is not the song the user heard.
    //
    // The nodes come from the SAME process-wide plugin instances the live
    // engine uses (`plugins::insert_node_for`), which is the pre-existing
    // arrangement for offline INSTRUMENT nodes a few lines above, with the
    // same caveat this module's header already documents: a bounce sees
    // whatever param values the host instance currently holds.
    //
    // KNOWN HOLE, inherited from that arrangement and now widened to
    // effects: a format host refuses a SECOND live node for an instance
    // that already has one out (`clap_host`'s `node_out` guard). While the
    // engine is holding a plugin, this bounce cannot build its own node for
    // it, and the slot renders DRY. There is no silent-failure path for
    // that here — it is logged loudly below — but the honest fix is
    // per-bounce PRIVATE instances (`clap_host`/`lv2_host` API work, the
    // node-graph round), not a workaround in this file: reusing the live
    // node would let a bounce and the RT thread process the same plugin
    // concurrently.
    let mut insert_nodes = crate::audio::insert::InsertNodeRegistry::default();
    let (mut chains, failed) =
        crate::audio::insert::compile_inserts(&store.tracks, plugins, rate, &mut insert_nodes);
    if !failed.is_empty() {
        log::warn!(
            "offline render: {} insert instance(s) could not be hosted for this bounce and \
             render DRY ({}). A plugin the live engine is holding cannot be activated twice — \
             stop the engine and bounce again for a wet export.",
            failed.len(),
            failed.join(", ")
        );
    }
    let plan = crate::audio::bus::compile_routing(
        &store.tracks,
        &slots,
        &mut chains,
        n_slots,
        crate::audio::rt::MAX_LIVE_BLOCK,
    );
    let row_for = |tracks: &[RtTrack], slot: usize| -> Option<usize> {
        tracks
            .iter()
            .position(|r| r.slot == slot && r.live.is_some())
            .or_else(|| tracks.iter().position(|r| r.slot == slot))
    };
    for t in &store.tracks {
        let Some(&slot) = slots.get(&t.id) else { continue };
        let Some(i) = row_for(&tracks, slot) else { continue };
        if let Some(chain) = chains.remove(&t.id) {
            if !chain.is_empty() {
                tracks[i].inserts = chain;
            }
        }
        if let Some(edges) = plan.sends.get(&t.id) {
            tracks[i].sends = edges
                .iter()
                .filter_map(|e| e.resolve(&send_slots, crate::audio::rt::MAX_LIVE_BLOCK))
                .collect();
        }
    }
    for (slot, &delay) in plan.track_pdc.iter().enumerate() {
        let Some(i) = row_for(&tracks, slot) else { continue };
        if delay > 0 {
            tracks[i].pdc = Some(crate::audio::pdc::DelayLine::new(
                delay,
                crate::audio::rt::MAX_LIVE_BLOCK,
                2,
            ));
        }
        let out_delay = plan.out_delay.get(slot).copied().unwrap_or(0);
        if out_delay > 0 {
            tracks[i].out_pdc = Some(crate::audio::pdc::DelayLine::new(
                out_delay,
                crate::audio::rt::MAX_LIVE_BLOCK,
                2,
            ));
        }
        tracks[i].output = plan.output.get(slot).copied().flatten();
    }

    // A bus sends like any other node; its edges land on the return strip.
    let mut buses = plan.buses;
    for (bi, id) in plan.bus_ids.iter().enumerate() {
        if let Some(edges) = plan.sends.get(id) {
            buses[bi].sends = edges
                .iter()
                .filter_map(|e| e.resolve(&send_slots, crate::audio::rt::MAX_LIVE_BLOCK))
                .collect();
        }
    }

    let end_samples = song_end(&tracks);
    let mut graph = RtGraph::with_buses(tracks, buses, 0, params);
    // RCU: attach the table BEFORE the graph is handed to the renderer,
    // matching `engine::rebuild` (Track D ruling 1).
    graph.set_track_ramps(compile_track_ramps(
        automation,
        modulation,
        midi,
        store,
        plugins,
        &slots,
        n_slots,
        rate,
    ));
    OfflineGraph { graph, end_samples }
}

/// Compile this snapshot's track ramps — the offline twin of
/// `engine::Control::compile_automation`, same rules: no lanes and no
/// modulation (or no usable tempo map) means an empty table of exactly
/// `n_slots` entries, and the table is sized by the MIXER-SLOT COUNT, not by
/// `slots.len()` — duplicate track ids collapse in the slot map, and a short
/// table would silently unramp the highest slots. Automation tracks are not
/// mixer slots.
fn compile_track_ramps(
    automation: &crate::control::session::AutomationDoc,
    modulation: &crate::modulation::ModulationDoc,
    midi: &MidiStore,
    store: &Store,
    plugins: &crate::control::session::PluginDoc,
    slots: &std::collections::HashMap<crate::ids::TrackId, usize>,
    n_slots: usize,
    rate: u32,
) -> Vec<TrackRamps> {
    let none = || (0..n_slots).map(|_| TrackRamps::default()).collect();
    if automation.lanes.is_empty() && modulation.is_empty() {
        return none();
    }
    let Ok(map) = crate::midi::TempoMap::new(midi.ppq, midi.tempo_events.clone(), rate) else {
        return none();
    };
    super::engine::compile_track_ramps(
        &automation.lanes,
        modulation,
        store,
        plugins,
        &midi.clips,
        slots,
        n_slots,
        &map,
    )
    .0
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
    use crate::audio::types::AutomationMode;
    use crate::audio::types::TrackState;
    use crate::ids::NoteId;
    use crate::midi::types::{MeterEvent, TempoEvent};
    use crate::midi::{MidiClip, MidiNote};

    fn track(id: &str, kind: &str) -> TrackState {
        TrackState {
            sends: Vec::new(),
            output: None,
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
            inserts: Vec::new(),
            group: None,
            automation_mode: AutomationMode::Read,
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
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![arp, groove],
            launch_maps: Vec::new(),
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
            let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), &Default::default(), &Default::default(), None, RATE);
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

    /// Plan G2: the BOUNCE walks the same routing the live engine does. A
    /// unity post-fader send into an empty return doubles the material — if
    /// export ignored sends, the exported song would be missing exactly the
    /// reverb the user mixed with.
    #[test]
    fn a_send_into_an_empty_bus_reaches_the_offline_bounce() {
        const RATE: u32 = 48_000;
        let (store, midi) = demo_project();
        let (mut routed, _) = demo_project();
        let render_all = |store: &Store| {
            let mut og = build_graph(
                store,
                &midi,
                &crate::control::session::PluginDoc::default(),
                &Default::default(),
                &Default::default(),
                None,
                RATE,
            );
            render(&mut og.graph, 0, og.end_samples, RATE, 1.0, &mut |_, _| {})
        };
        let dry = render_all(&store);

        routed.tracks.push(track("verb", "bus"));
        for t in routed.tracks.iter_mut().filter(|t| t.kind == "midi") {
            t.sends.push(crate::audio::types::SendSlot {
                id: format!("snd-{}", t.id.as_str()),
                dest: "verb".into(),
                amount_db: 0.0,
                pre_fader: false,
            });
        }
        let wet = render_all(&routed);

        assert_eq!(wet.len(), dry.len());
        assert!(peak(&dry) > 0.05, "the dry bounce is audible to begin with");
        for (i, (w, d)) in wet.iter().zip(dry.iter()).enumerate() {
            assert!(
                (w - 2.0 * d).abs() < 1e-5,
                "sample {i}: dry {d} + an equally loud return should be {}, got {w}",
                2.0 * d
            );
        }
    }

    /// Plan G2: output routing reaches the bounce too. A routed track goes
    /// through the bus and NOT to the master, so the exported mix has one
    /// copy — the same thing the live engine does.
    #[test]
    fn a_routed_track_reaches_the_bounce_through_its_bus_only() {
        const RATE: u32 = 48_000;
        let (store, midi) = demo_project();
        let (mut routed, _) = demo_project();
        let render_all = |store: &Store| {
            let mut og = build_graph(
                store,
                &midi,
                &crate::control::session::PluginDoc::default(),
                &Default::default(),
                &Default::default(),
                None,
                RATE,
            );
            render(&mut og.graph, 0, og.end_samples, RATE, 1.0, &mut |_, _| {})
        };
        let direct = render_all(&store);

        routed.tracks.push(track("group", "bus"));
        for t in routed.tracks.iter_mut().filter(|t| t.kind == "midi") {
            t.output = Some("group".into());
        }
        let grouped = render_all(&routed);

        assert!(peak(&direct) > 0.05, "the direct bounce is audible to begin with");
        for (i, (g, d)) in grouped.iter().zip(direct.iter()).enumerate() {
            assert!(
                (g - d).abs() < 1e-5,
                "sample {i}: routing through an empty bus changes nothing, got {g} vs {d}"
            );
        }

        // And muting the bus takes the whole group with it — proof the
        // signal really is going through it rather than around it.
        if let Some(b) = routed.tracks.iter_mut().find(|t| t.kind == "bus") {
            b.muted = true;
        }
        let silenced = render_all(&routed);
        assert_eq!(peak(&silenced), 0.0, "muting the group bus silences the bounce");
    }

    /// A bus takes a mixer slot but NO source row — it is fed by sends. Two
    /// rows for one slot would put a second writer on its meter lane.
    #[test]
    fn a_bus_track_gets_a_strip_but_no_source_row() {
        const RATE: u32 = 48_000;
        let mut store = Store::default();
        store.tracks.push(track("a", "audio"));
        store.tracks.push(track("verb", "bus"));
        let og = build_graph(
            &store,
            &MidiStore::default(),
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &Default::default(),
            None,
            RATE,
        );
        assert_eq!(og.graph.buses.len(), 1, "the bus compiles to a return strip");
        assert_eq!(og.graph.buses[0].slot, 1);
        assert_eq!(og.graph.tracks.len(), 1, "and to no source row");
        assert_eq!(og.graph.tracks[0].slot, 0);
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
            harmony: Default::default(),
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
                transpose_semitones: 0,
                velocity_offset: 0,
            }],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), &Default::default(), &Default::default(), None, RATE);
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
        let mut og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), &Default::default(), &Default::default(), None, RATE);
        assert_eq!(og.end_samples, 600, "clip end = start + len");
        let out = render(&mut og.graph, 0, 700, RATE, 0.5, &mut |_, _| {});
        let center = 0.5 * std::f32::consts::FRAC_1_SQRT_2 * 0.5; // sample * pan * master
        assert_eq!(out[0], 0.0, "silent before the clip");
        assert!((out[150 * 2] - center).abs() < 1e-6, "clip audible, panned, master-scaled");
        assert!((out[150 * 2 + 1] - center).abs() < 1e-6);
        assert_eq!(out[650 * 2], 0.0, "silent past the clip");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Track D close-out: a track-gain lane must shape the BOUNCE exactly as
    /// it shapes playback ("what you hear is what you export"). A constant
    /// clip under a 1.0 -> 0.0 fade lane must leave the mix ramping linearly;
    /// before this, `build_graph` published a graph with no `gain_ramps` and
    /// the export came out at full level throughout.
    #[test]
    fn build_graph_applies_track_gain_automation_to_the_bounce() {
        const RATE: u32 = 48_000;
        const LEN: u64 = 4_000; // ppq 960 @120bpm -> 25 samples/tick -> 160 ticks
        let dir = std::env::temp_dir().join(format!(
            "aura-offline-auto-{}-{}",
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
        for _ in 0..LEN {
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
            source_length_samples: LEN,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: LEN,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("a1"),
        });
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let automation = crate::control::session::AutomationDoc {
            lanes: vec![crate::plugins::automation::AutomationLane {
                id: "l1".into(),
                target_node: "track:a1".into(),
                param_id: crate::plugins::automation::TRACK_PARAM_GAIN,
                points: vec![
                    crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 },
                    crate::plugins::automation::AutomationPoint { tick: 160, value: 0.0 },
                ],
            }],
        };
        let mut og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &automation,
            &Default::default(),
            None,
            RATE,
        );
        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        // sample * centre pan, times the lane's linear fade at that frame.
        let unramped = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        for i in [0u64, 1_000, 2_000, 3_000] {
            let want = unramped * (1.0 - i as f32 / LEN as f32);
            assert!(
                (out[i as usize * 2] - want).abs() < 1e-6,
                "frame {i}: bounce must follow the lane — got {}, want {want}",
                out[i as usize * 2]
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `Off` mode must bypass a track's gain lane entirely — not just mute
    /// its effect, but leave no ramp in the compiled table at all, exactly
    /// as if the lane didn't exist. `compile_track_ramps` (`engine.rs`) is
    /// the ONE function both this offline bounce path and the live rebuild
    /// path funnel through, so proving it here proves both agree.
    #[test]
    fn build_graph_skips_a_gain_lane_when_the_track_is_off() {
        const RATE: u32 = 48_000;
        let mut store = Store::default();
        store.tracks.push(TrackState {
            sends: Vec::new(),
            output: None,
            automation_mode: AutomationMode::Off,
            ..track("t-1", "audio")
        });
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let automation = crate::control::session::AutomationDoc {
            lanes: vec![crate::plugins::automation::AutomationLane {
                id: "l1".into(),
                target_node: "track:t-1".into(),
                param_id: crate::plugins::automation::TRACK_PARAM_GAIN,
                points: vec![
                    crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 },
                    crate::plugins::automation::AutomationPoint { tick: 160, value: 0.0 },
                ],
            }],
        };
        let og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &automation,
            &Default::default(),
            None,
            RATE,
        );
        assert!(
            og.graph.track_ramps[0].gain.is_none(),
            "Off must skip the lane entirely — no ramp entry, same as if it didn't exist"
        );
    }

    /// Parity guard for the test above: `Read` (the default mode) must NOT
    /// be affected by the `Off` filter in `compile_track_ramps` — the lane
    /// still compiles into a ramp exactly as before this change.
    #[test]
    fn build_graph_still_applies_a_gain_lane_when_the_track_is_read() {
        const RATE: u32 = 48_000;
        let mut store = Store::default();
        store.tracks.push(TrackState {
            sends: Vec::new(),
            output: None,
            automation_mode: AutomationMode::Read,
            ..track("t-1", "audio")
        });
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let automation = crate::control::session::AutomationDoc {
            lanes: vec![crate::plugins::automation::AutomationLane {
                id: "l1".into(),
                target_node: "track:t-1".into(),
                param_id: crate::plugins::automation::TRACK_PARAM_GAIN,
                points: vec![
                    crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 },
                    crate::plugins::automation::AutomationPoint { tick: 160, value: 0.0 },
                ],
            }],
        };
        let og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &automation,
            &Default::default(),
            None,
            RATE,
        );
        assert!(
            og.graph.track_ramps[0].gain.is_some(),
            "Read (default) must still compile the lane into a ramp"
        );
    }

    /// The ramp table is sized by the TRACK COUNT, not by `slots.len()`.
    /// `derive_slots` keys by track id, so two tracks sharing an id collapse
    /// to one map entry pointing at the LAST index — sizing by the map would
    /// make that very slot out of range and silently drop its ramp.
    #[test]
    fn offline_ramp_table_is_sized_by_track_count_not_slot_map() {
        let store = Store {
            tracks: vec![track("dup", "audio"), track("dup", "audio")],
            ..Store::default()
        };
        let slots = derive_slots(&store.tracks);
        assert_eq!(slots.len(), 1, "duplicate ids collapse in the slot map");
        assert_eq!(slots["dup"], 1, "…onto the LAST index");
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            ..MidiStore::default()
        };
        let automation = crate::control::session::AutomationDoc {
            lanes: vec![crate::plugins::automation::AutomationLane {
                id: "l1".into(),
                target_node: "track:dup".into(),
                param_id: crate::plugins::automation::TRACK_PARAM_GAIN,
                points: vec![
                    crate::plugins::automation::AutomationPoint { tick: 0, value: 1.0 },
                    crate::plugins::automation::AutomationPoint { tick: 160, value: 0.0 },
                ],
            }],
        };
        let ramps = compile_track_ramps(
            &automation,
            &Default::default(),
            &midi,
            &store,
            &crate::control::session::PluginDoc::default(),
            &slots,
            store.tracks.len(),
            48_000,
        );
        assert_eq!(ramps.len(), 2, "one entry per TRACK");
        assert!(ramps[1].gain.is_some(), "slot 1's ramp survives");
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
        let og = build_graph(&store, &midi, &crate::control::session::PluginDoc::default(), &Default::default(), &Default::default(), None, RATE);
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

    /// offline::build_graph compiles the same ramp table playback uses
    /// (design §6.3), so a pan binding shapes the bounce.
    #[test]
    fn pan_automation_survives_a_bounce() {
        use crate::modulation::model::{
            Binding, BindingMode, Curve, Domain, Range, Source, TargetRef, TrackParam,
        };
        use crate::modulation::ModulationDoc;
        use crate::plugins::automation::AutomationPoint;

        const RATE: u32 = 48_000;
        const LEN: u64 = 4_000; // ppq 960 @120bpm -> 25 samples/tick -> 160 ticks
        let dir = std::env::temp_dir().join(format!(
            "aura-offline-pan-{}-{}",
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
        for _ in 0..LEN {
            w.write_sample(1.0f32).unwrap();
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
            source_length_samples: LEN,
            timeline_start_samples: 0,
            offset_samples: 0,
            length_samples: LEN,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
            content_id: crate::ids::ContentId::mint(),
            lane_id: crate::ids::LaneId::default_for_track("a1"),
        });
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        let mut modulation = ModulationDoc::default();
        modulation.curves.push(Curve {
            id: "cur".into(),
            name: "cur".into(),
            length_ticks: None,
            points: vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: 160, value: 1.0 },
            ],
        });
        modulation.bindings.push(Binding {
            id: "b".into(),
            source: Source::Curve { curve_id: "cur".into() },
            target: TargetRef::TrackParam {
                track_id: "a1".into(),
                param: TrackParam::Pan,
            },
            mode: BindingMode::Absolute,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        });

        let mut og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &modulation,
            None,
            RATE,
        );
        assert!(
            og.graph.track_ramps.first().and_then(|t| t.pan.as_ref()).is_some(),
            "build_graph must compile the pan binding into the ramp table"
        );
        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        let first_l = out[0].abs();
        let first_r = out[1].abs();
        let last_l = out[(LEN as usize - 1) * 2].abs();
        let last_r = out[(LEN as usize - 1) * 2 + 1].abs();
        assert!(first_l > first_r, "bounce starts left: L={first_l} R={first_r}");
        assert!(last_r > last_l, "bounce ends right: L={last_l} R={last_r}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Constant-tone WAV + store/midi scaffolding for the automation-track
    /// bounce tests. ppq 960 @ 120 bpm → 25 samples/tick, so `len` samples
    /// is `len / 25` ticks.
    fn auto_bounce_harness(
        len: u64,
        tracks: &[(&str, &str)],
        clip_tracks: &[&str],
    ) -> (std::path::PathBuf, Store, MidiStore) {
        const RATE: u32 = 48_000;
        let dir = std::env::temp_dir().join(format!(
            "aura-offline-autotrk-{}-{}",
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
        for _ in 0..len {
            w.write_sample(0.5f32).unwrap();
        }
        w.finalize().unwrap();

        let mut store = Store::default();
        store.project_dir = Some(dir.clone());
        for (id, kind) in tracks {
            store.tracks.push(track(id, kind));
        }
        for (i, tid) in clip_tracks.iter().enumerate() {
            store.clips.push(crate::audio::types::Clip {
                id: format!("c{i}").into(),
                track_id: (*tid).into(),
                name: format!("c{i}"),
                source_path: "audio/c1.wav".into(),
                source_id: crate::ids::SourceId::default(),
                source_channels: 1,
                source_sample_rate: RATE,
                source_length_samples: len,
                timeline_start_samples: 0,
                offset_samples: 0,
                length_samples: len,
                gain_db: 0.0,
                fade_in_samples: 0,
                fade_out_samples: 0,
                content_id: crate::ids::ContentId::mint(),
                lane_id: crate::ids::LaneId::default_for_track(tid),
            });
        }
        let midi = MidiStore {
            harmony: Default::default(),
            ppq: 960,
            tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
            meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
            clips: vec![],
            launch_maps: Vec::new(),
            loaded_dir: None,
            dirty: false,
        };
        (dir, store, midi)
    }

    fn gain_binding(id: &str, auto_track: &str, target_track: &str) -> crate::modulation::Binding {
        use crate::modulation::model::{Binding, BindingMode, Domain, Range, Source, TargetRef, TrackParam};
        Binding {
            id: id.into(),
            source: Source::AutomationTrack { track_id: auto_track.into() },
            target: TargetRef::TrackParam {
                track_id: target_track.into(),
                param: TrackParam::Gain,
            },
            mode: BindingMode::Multiply,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        }
    }

    /// An automation track is not a mixer voice: no slot, no RtTrack, and a
    /// clip sitting on it is silent. Inserting it between two audio tracks
    /// must not shift their slots (the same contract `types::derive_slots`
    /// pins directly).
    #[test]
    fn an_automation_track_takes_no_mixer_slot_and_renders_no_audio() {
        const RATE: u32 = 48_000;
        const LEN: u64 = 4_000;
        let (dir, store, midi) = auto_bounce_harness(
            LEN,
            &[("a1", "audio"), ("auto", "automation"), ("a2", "audio")],
            &["auto"],
        );
        let slots = derive_slots(&store.tracks);
        assert!(
            !slots.contains_key("auto"),
            "derive_slots must skip kind:automation"
        );
        assert_eq!(slots["a1"], 0, "a1 keeps slot 0 with an auto track after it");
        assert_eq!(
            slots["a2"], 1,
            "a2 must stay at slot 1 — a middle automation track must not shift it to 2"
        );

        let og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &Default::default(),
            None,
            RATE,
        );
        assert_eq!(
            og.graph.tracks.len(),
            2,
            "automation tracks are not assembled into the mixer graph"
        );
        assert!(
            og.graph.tracks.iter().all(|t| t.slot == 0 || t.slot == 1),
            "mixer rows only occupy the two audio slots"
        );
        assert_eq!(
            og.graph.params.len(),
            2,
            "ParamTable is sized by mixer slots, not store track count"
        );

        let mut og = og;
        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak < 1e-6,
            "a clip on an automation track must be silent, peak={peak}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Offline render: two audio tracks, one curve, two bindings from one
    /// automation track — both amplitudes follow the same fade.
    #[test]
    fn one_automation_track_bound_to_two_targets_moves_both() {
        use crate::modulation::model::{AutomationClip, AutomationPoint, Curve};
        use crate::modulation::ModulationDoc;

        const RATE: u32 = 48_000;
        const LEN: u64 = 4_000; // 160 ticks @ 25 samples/tick
        let (dir, store, midi) = auto_bounce_harness(
            LEN,
            &[("a1", "audio"), ("a2", "audio"), ("auto", "automation")],
            &["a1", "a2"],
        );
        let mut modulation = ModulationDoc::default();
        modulation.curves.push(Curve {
            id: "cur".into(),
            name: "cur".into(),
            length_ticks: Some(160),
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                // tick 160 == clip end is outside the half-open span and
                // would be dropped; the last audible sample of the fade is
                // the tick before.
                AutomationPoint { tick: 159, value: 0.0 },
            ],
        });
        modulation.automation_clips.push(AutomationClip {
            id: "acl".into(),
            track_id: "auto".into(),
            curve_id: "cur".into(),
            timeline_start_ticks: 0,
            length_ticks: 160,
            content_length_ticks: None,
        });
        modulation.bindings.push(gain_binding("b1", "auto", "a1"));
        modulation.bindings.push(gain_binding("b2", "auto", "a2"));

        let mut og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &modulation,
            None,
            RATE,
        );
        assert!(
            og.graph.track_ramps.get(0).and_then(|t| t.gain.as_ref()).is_some(),
            "first target has a gain ramp"
        );
        assert!(
            og.graph.track_ramps.get(1).and_then(|t| t.gain.as_ref()).is_some(),
            "second target has a gain ramp"
        );

        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        // Two identical centre-panned 0.5 clips, both multiplied by the
        // same 1→0 fade (last point at tick 159 = sample 3975): the mix
        // is 2 × (0.5 * centre * ramp). One track moving would leave
        // ~unramped extra and miss these values by ~0.18.
        let last = 159.0 * 25.0; // samples
        let unramped = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        for i in [0u64, 1_000, 2_000, 3_000] {
            let ramp = 1.0 - i as f32 / last;
            let want = 2.0 * unramped * ramp;
            let got = out[i as usize * 2];
            assert!(
                (got - want).abs() < 1e-4,
                "frame {i}: both targets must follow the curve — got {got}, want {want}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Design §5: outside every automation clip the binding contributes
    /// nothing — the target falls back to its document value, NOT a held
    /// last-point. A clip that silences the first half must leave the
    /// second half at the fader, not at 0.
    #[test]
    fn outside_every_automation_clip_the_target_falls_back_to_its_document_value() {
        use crate::modulation::model::{AutomationClip, AutomationPoint, Curve};
        use crate::modulation::ModulationDoc;

        const RATE: u32 = 48_000;
        const LEN: u64 = 4_000;
        let (dir, store, midi) = auto_bounce_harness(
            LEN,
            &[("a1", "audio"), ("auto", "automation")],
            &["a1"],
        );
        let mut modulation = ModulationDoc::default();
        modulation.curves.push(Curve {
            id: "cur".into(),
            name: "cur".into(),
            length_ticks: None,
            points: vec![
                AutomationPoint { tick: 0, value: 0.0 },
                AutomationPoint { tick: 79, value: 0.0 },
            ],
        });
        // Clip covers only the first 80 ticks (2000 samples).
        modulation.automation_clips.push(AutomationClip {
            id: "acl".into(),
            track_id: "auto".into(),
            curve_id: "cur".into(),
            timeline_start_ticks: 0,
            length_ticks: 80,
            content_length_ticks: None,
        });
        modulation.bindings.push(gain_binding("b", "auto", "a1"));

        let mut og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &modulation,
            None,
            RATE,
        );
        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        let document = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
        let inside = out[1_000 * 2].abs();
        let outside = out[3_000 * 2].abs();
        assert!(
            inside < 1e-5,
            "inside the clip the curve silences the track: {inside}"
        );
        assert!(
            (outside - document).abs() < 1e-5,
            "past the clip the target is the document value {document}, not a held 0: {outside}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A 1-period envelope under a 4-period looping MidiClip: the bounce at
    /// the same offset in "bar" 1 and "bar" 3 must match (design §11).
    #[test]
    fn a_clip_envelope_loops_the_same_shape_in_every_repeat() {
        use crate::modulation::model::{
            AutomationPoint, Binding, BindingMode, Curve, Domain, Range, Source, TargetRef,
            TrackParam,
        };
        use crate::modulation::ModulationDoc;

        const RATE: u32 = 48_000;
        // 160 ticks/period × 4 periods × 25 samples/tick
        const PERIOD: u64 = 160;
        const LEN: u64 = PERIOD * 4 * 25;
        let (dir, store, mut midi) = auto_bounce_harness(LEN, &[("a1", "audio")], &["a1"]);
        midi.clips.push(crate::midi::MidiClip {
            id: "mc".into(),
            track_id: "a1".into(),
            name: "mc".into(),
            timeline_start_ticks: 0,
            length_ticks: PERIOD * 4,
            notes: Vec::new(),
            next_note_id: 1,
            content_id: "con".into(),
            lane_id: crate::ids::LaneId::default_for_track("a1"),
            content_length_ticks: Some(PERIOD),
            transpose_semitones: 0,
            velocity_offset: 0,
        });
        let mut modulation = ModulationDoc::default();
        modulation.curves.push(Curve {
            id: "cur".into(),
            name: "cur".into(),
            length_ticks: Some(PERIOD),
            points: vec![
                AutomationPoint { tick: 0, value: 1.0 },
                AutomationPoint { tick: 80, value: 0.0 },
            ],
        });
        modulation.bindings.push(Binding {
            id: "b".into(),
            source: Source::ClipEnvelope { content_id: "con".into(), curve_id: "cur".into() },
            target: TargetRef::SelfTrackParam { param: TrackParam::Gain },
            mode: BindingMode::Multiply,
            depth: 1.0,
            range: Range::default(),
            domain: Domain::Normalized,
            range_snapshot: None,
            enabled: true,
        });

        let mut og = build_graph(
            &store,
            &midi,
            &crate::control::session::PluginDoc::default(),
            &Default::default(),
            &modulation,
            None,
            RATE,
        );
        let out = render(&mut og.graph, 0, LEN, RATE, 1.0, &mut |_, _| {});
        // offset 40 ticks into the period = sample 1000; third repeat starts
        // at tick 320 = sample 8000.
        let bar1 = out[1_000 * 2].abs();
        let bar3 = out[9_000 * 2].abs();
        assert!(
            (bar1 - bar3).abs() < 1e-4,
            "bar 1 and bar 3 of a looping clip envelope must match: {bar1} vs {bar3}"
        );
        assert!(bar1 > 1e-4, "the sampled offset is mid-ramp, not silent: {bar1}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
