//! End-to-end regression test for the bug report *"I patch a MIDI track to
//! Hydrogen and the drum machine plays nothing"*.
//!
//! Everything the app does is in the loop here — a real audio engine, a real
//! `ControlPlane`, a real committed MIDI clip, a real route through
//! `set_midi_track_route`, a real `midir` connection — and the assertions are
//! on the actual bytes a virtual ALSA-seq input receives. Kept as a whole
//! rather than split into units because the defect it pins down lived in none
//! of the pieces: every layer was individually correct, and the note's MIDI
//! channel was simply dropped at the seam between them.
//!
//! The document under test is the shape the Composer's groove generator
//! produces: General MIDI drum keys on **channel 10** (0-based 9). A drum
//! machine listens there; before this test existed those notes went out on
//! channel 1, which is why nothing played.

use super::*;

struct NullEvents;
impl crate::audio::engine::EventSink for NullEvents {
    fn emit(&self, _e: &str, _p: serde_json::Value) {}
}

/// Everything the fixture hands back: the wire log, the plane, and the ids.
struct Rig {
    seen: std::sync::Arc<PlMutex<Vec<Vec<u8>>>>,
    cp: Arc<crate::control::ControlPlane>,
    port_id: String,
    dir: std::path::PathBuf,
    /// Kept alive for the length of the test — dropping it closes the port.
    _conn: midir::MidiInputConnection<()>,
}

/// A project with one MIDI track carrying a four-hit GM drum bar on channel
/// `note_channel`, a virtual loopback port open, and that track routed to it
/// with `route_channel` as the override. `None` (the production default) means
/// "each note on its own channel".
///
/// Returns `None` when ALSA-seq is not usable in this environment (headless CI
/// without a sequencer), the same skip other tests in this module take.
fn rig(label: &str, note_channel: u8, route_channel: Option<u8>) -> Option<Rig> {
    use crate::audio::rt::testutil::empty_tables;
    use crate::audio::types::Store;
    use crate::control::ControlPlane;
    use crate::ids::NoteId;
    use crate::midi::types::{MidiNote, DEFAULT_PPQ};
    use crate::midi::MidiStore;
    use crate::sidecars::jobs::JobManager;
    use midir::os::unix::VirtualInput;

    let midi_in = midir::MidiInput::new("aura-route-e2e-in").ok()?;
    let seen = std::sync::Arc::new(PlMutex::new(Vec::<Vec<u8>>::new()));
    let sink_seen = seen.clone();
    let port_name = format!("aura-route-e2e-{label}");
    let conn = midi_in
        .create_virtual(
            &port_name,
            move |_, msg, _: &mut ()| sink_seen.lock().push(msg.to_vec()),
            (),
        )
        .ok()?;
    let target = list_output_ports()
        .ok()?
        .into_iter()
        .find(|p| p.name.contains(&port_name))?;

    let shared = Arc::new(SharedRt::default());
    let tables = empty_tables();
    let session = Arc::new(PlMutex::new(crate::control::Session::new(
        Store::default(),
        MidiStore::default(),
    )));
    let engine = crate::audio::engine::start(
        shared.clone(),
        tables.clone(),
        session.clone(),
        Box::new(NullEvents),
        crate::control::testutil::test_committer(&session, &shared, &tables),
    );
    // Event-driven readiness: one synchronous round-trip guarantees the output
    // stream is open and the real sample rate settled before the fixture
    // computes anything from it (same trick `control::loopjam`'s fixture uses;
    // a fixed sleep raced the stream open under full-suite load).
    engine
        .request(|reply| crate::audio::engine::ControlMsg::SelectInput { device_id: None, reply })
        .expect("engine control thread responds");
    let cp = Arc::new(ControlPlane::new(
        session.clone(),
        shared.clone(),
        tables,
        engine,
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        std::sync::Arc::new(crate::control::HistoryLog::new()),
    ));

    let dir = std::env::temp_dir().join(format!("aura-route-e2e-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    cp.create_project(dir.to_str().unwrap(), "Route E2E").unwrap();

    let track = cp
        .add_track(
            Some("Drums".into()),
            Some("midi".into()),
            crate::control::op::TxMeta::user("track"),
        )
        .unwrap();
    let q = DEFAULT_PPQ as u32;
    let clip = crate::midi::midi_add_clip_core(
        &cp,
        track.id.to_string(),
        Some("Groove".into()),
        0,
        DEFAULT_PPQ as u64 * 4,
    )
    .unwrap();
    // Kick, snare, kick, snare — GM keys, so a drum machine has something to
    // map them onto, and two distinct keys so an off cannot be mistaken for
    // another key's.
    let notes: Vec<MidiNote> = (0..4u32)
        .map(|i| MidiNote {
            tick: i * q,
            length_ticks: q / 2,
            key: if i % 2 == 0 { 36 } else { 38 },
            velocity: 100,
            channel: note_channel,
            note_id: NoteId(0),
        })
        .collect();
    crate::midi::midi_set_notes_core(&cp, clip.id.to_string(), notes).unwrap();

    let out = Arc::new(MidiOut::default());
    out.set_routing_path_for_test(dir.join("routing.json"));
    out.attach(session.clone(), shared.clone());
    cp.attach_midi_out(Arc::clone(&out));
    cp.open_midi_output_port(target.id.clone(), crate::control::op::TxMeta::user("open"))
        .unwrap();
    cp.set_midi_track_route(
        track.id.to_string(),
        Some(target.id.clone()),
        route_channel,
        crate::control::op::TxMeta::user("route"),
    )
    .unwrap();
    // One 250 ms snapshot window, so the port thread has actually adopted the
    // route before the transport rolls.
    std::thread::sleep(std::time::Duration::from_millis(400));

    Some(Rig { seen, cp, port_id: target.id, dir, _conn: conn })
}

impl Rig {
    /// Roll the transport far enough to cross all four hits, then stop.
    fn play_a_bar(&self) {
        self.cp.transport(crate::control::TransportAction::Play).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2200));
        self.cp.transport(crate::control::TransportAction::Stop).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    /// Every note-on that reached the wire, as `(channel, key, velocity)`.
    fn note_ons(&self) -> Vec<(u8, u8, u8)> {
        self.seen
            .lock()
            .iter()
            .filter(|m| m.len() == 3 && m[0] & 0xF0 == 0x90 && m[2] > 0)
            .map(|m| (m[0] & 0x0F, m[1], m[2]))
            .collect()
    }

    /// Every note-off that reached the wire, as `(channel, key)`.
    fn note_offs(&self) -> Vec<(u8, u8)> {
        self.seen
            .lock()
            .iter()
            .filter(|m| m.len() == 3 && (m[0] & 0xF0 == 0x80 || (m[0] & 0xF0 == 0x90 && m[2] == 0)))
            .map(|m| (m[0] & 0x0F, m[1]))
            .collect()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self
            .cp
            .close_midi_output_port(self.port_id.clone(), crate::control::op::TxMeta::user("close"));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// THE BUG. A GM drum part (the Composer writes channel 10) routed with the
/// default channel setting must arrive on channel 10, because that is the only
/// channel a drum machine maps to its kit. It used to arrive on channel 1: the
/// route's channel — a `u8` that defaulted to 0 — was stamped over every note,
/// and `AbsNoteEvent` had already dropped the note's own channel anyway.
#[test]
fn gm_drum_notes_reach_the_wire_on_channel_10() {
    let Some(rig) = rig("gm-drums", 9, None) else {
        eprintln!("skipping: ALSA seq unavailable");
        return;
    };
    rig.play_a_bar();

    let ons = rig.note_ons();
    assert!(
        !ons.is_empty(),
        "the routed track sent nothing at all: {:?}",
        rig.seen.lock()
    );
    assert!(
        ons.iter().all(|(ch, _, _)| *ch == 9),
        "every drum hit goes out on channel 10 (0-based 9), not the route's default: {ons:?}"
    );
    assert!(
        ons.iter().any(|(_, key, _)| *key == 36) && ons.iter().any(|(_, key, _)| *key == 38),
        "both GM keys made it out: {ons:?}"
    );
    assert!(
        rig.note_offs().iter().all(|(ch, _)| *ch == 9),
        "releases go out on the same channel the note started on: {:?}",
        rig.note_offs()
    );
}

/// The override still works, and is still the way to drive a mono-timbral
/// synth listening on one fixed channel: forcing channel 3 moves a channel-10
/// drum part onto channel 3 wholesale.
#[test]
fn a_forced_channel_overrides_what_the_clip_says() {
    let Some(rig) = rig("forced", 9, Some(2)) else {
        eprintln!("skipping: ALSA seq unavailable");
        return;
    };
    rig.play_a_bar();

    let ons = rig.note_ons();
    assert!(!ons.is_empty(), "the routed track sent nothing at all");
    assert!(
        ons.iter().all(|(ch, _, _)| *ch == 2),
        "the forced channel wins over the clip's own: {ons:?}"
    );
}

/// A routed track's internal instrument goes quiet — the external device IS
/// the instrument, and AURA doubling it (as pitches, for a drum part) is what
/// masked the drum machine in the report. Asserted through `routed_out()` plus
/// `append_from`, which is the pair `engine::rebuild` actually uses; the graph
/// itself has no public shape to inspect.
#[test]
fn a_routed_track_stops_sounding_its_internal_instrument() {
    use crate::audio::types::{Store, TrackState};
    use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
    use crate::midi::MidiStore;

    let mut store = Store::default();
    for id in ["t-out", "t-in"] {
        store.tracks.push(TrackState {
            id: id.into(),
            name: id.into(),
            kind: "midi".into(),
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            color: "#7c9cff".into(),
            instrument_id: None,
            inserts: Vec::new(),
            group: None,
        });
    }
    let clip = |track: &str| MidiClip {
        id: format!("c-{track}").into(),
        track_id: track.into(),
        name: track.into(),
        timeline_start_ticks: 0,
        length_ticks: DEFAULT_PPQ as u64 * 4,
        notes: vec![MidiNote {
            tick: 0,
            length_ticks: 240,
            key: 60,
            velocity: 100,
            channel: 0,
            note_id: crate::ids::NoteId(1),
        }],
        next_note_id: 2,
        content_id: crate::ids::ContentId::mint(),
        lane_id: crate::ids::LaneId::default_for_track(track),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    };
    let midi = MidiStore {
        ppq: DEFAULT_PPQ,
        tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
        meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
        clips: vec![clip("t-out"), clip("t-in")],
        ..Default::default()
    };
    let snap = crate::control::snapshot::MidiSnapshot::from_store(&midi);
    let slots = crate::audio::types::derive_slots(&store.tracks);
    let doc = crate::control::session::PluginDoc::default();

    let live_tracks = |routed: &RoutedOut| -> Vec<usize> {
        let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
        let mut out = Vec::new();
        crate::midi::playback::append_from(
            &snap, &store.tracks, &store.clips, &doc, &slots, 48_000, None, routed, &mut nodes,
            &mut out,
        );
        out.iter().filter(|t| t.live.is_some()).map(|t| t.slot).collect()
    };

    let unrouted = live_tracks(&RoutedOut::default());
    assert_eq!(unrouted.len(), 2, "both tracks sound internally with no routing");

    let out = MidiOut::default();
    out.set_route(RouteScope::Track("t-out".into()), Some(RouteTarget::from_clip("some-port#0")));
    let routed = out.routed_out();
    assert!(routed.has_track("t-out") && !routed.has_track("t-in"));

    let with_route = live_tracks(&routed);
    let out_slot = slots[&crate::ids::TrackId::from("t-out")];
    let in_slot = slots[&crate::ids::TrackId::from("t-in")];
    assert!(
        !with_route.contains(&out_slot),
        "the routed track has no internal voice left: {with_route:?}"
    );
    assert!(
        with_route.contains(&in_slot),
        "the OTHER midi track is untouched — routing is per track, not global: {with_route:?}"
    );
}

/// A clip-level override takes only that clip out of the internal instrument;
/// the track's other clips keep sounding, exactly as the track's route-level
/// exclusion already worked on the way out.
#[test]
fn a_routed_clip_is_subtracted_from_its_tracks_internal_voice() {
    use crate::audio::types::{Store, TrackState};
    use crate::midi::types::{MeterEvent, MidiClip, MidiNote, TempoEvent, DEFAULT_PPQ};
    use crate::midi::MidiStore;

    let mut store = Store::default();
    store.tracks.push(TrackState {
        id: "t-1".into(),
        name: "t-1".into(),
        kind: "midi".into(),
        gain_db: 0.0,
        pan: 0.0,
        muted: false,
        soloed: false,
        armed: false,
        color: "#7c9cff".into(),
        instrument_id: None,
        inserts: Vec::new(),
        group: None,
    });
    let clip = |id: &str, at: u64, key: u8| MidiClip {
        id: id.into(),
        track_id: "t-1".into(),
        name: id.into(),
        timeline_start_ticks: at,
        length_ticks: DEFAULT_PPQ as u64,
        notes: vec![MidiNote {
            tick: 0,
            length_ticks: 240,
            key,
            velocity: 100,
            channel: 0,
            note_id: crate::ids::NoteId(1),
        }],
        next_note_id: 2,
        content_id: crate::ids::ContentId::mint(),
        lane_id: crate::ids::LaneId::default_for_track("t-1"),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    };
    let midi = MidiStore {
        ppq: DEFAULT_PPQ,
        tempo_events: vec![TempoEvent { tick: 0, bpm: 120.0 }],
        meter_events: vec![MeterEvent { tick: 0, num: 4, den: 4 }],
        clips: vec![clip("c-out", 0, 60), clip("c-in", DEFAULT_PPQ as u64, 67)],
        ..Default::default()
    };
    let snap = crate::control::snapshot::MidiSnapshot::from_store(&midi);
    let slots = crate::audio::types::derive_slots(&store.tracks);
    let doc = crate::control::session::PluginDoc::default();

    let out = MidiOut::default();
    out.set_route(RouteScope::Clip("c-out".into()), Some(RouteTarget::from_clip("some-port#0")));

    let mut nodes = crate::midi::playback::LiveNodeRegistry::default();
    let mut rows = Vec::new();
    crate::midi::playback::append_from(
        &snap,
        &store.tracks,
        &store.clips,
        &doc,
        &slots,
        48_000,
        None,
        &out.routed_out(),
        &mut nodes,
        &mut rows,
    );
    let live = rows
        .iter()
        .find_map(|t| t.live.as_ref())
        .expect("the track still has an internal voice for its unrouted clip");
    let keys: Vec<u8> = live.events.iter().filter(|e| e.velocity > 0).map(|e| e.key).collect();
    assert_eq!(keys, vec![67], "only the routed clip's note is gone: {keys:?}");
}

/// Routing survives the device coming back at a different ALSA sequencer
/// address. `midir` spells an ALSA port `"<client>:<port> <n>:<m>"`, and `<n>`
/// is assigned in connection order — so restarting Hydrogen renamed the port
/// and silently dropped the persisted route. Persisted routing must resolve by
/// the stable part of the name.
#[test]
fn a_persisted_route_survives_a_new_alsa_client_number() {
    use midir::os::unix::VirtualInput;

    let Ok(midi_in) = midir::MidiInput::new("aura-readdr-in") else {
        eprintln!("skipping: ALSA seq unavailable");
        return;
    };
    let Ok(_conn) = midi_in.create_virtual("aura-readdr-loopback", |_, _, _: &mut ()| {}, ()) else {
        eprintln!("skipping: virtual port unavailable");
        return;
    };
    let Ok(ports) = list_output_ports() else {
        eprintln!("skipping: no output enumeration");
        return;
    };
    let Some(target) = ports.into_iter().find(|p| p.name.contains("aura-readdr-loopback")) else {
        eprintln!("skipping: loopback port not visible");
        return;
    };
    // The live name carries this session's address; a persisted file written
    // before the device restarted carries a different one.
    let stable = persist::port_name_key(target.name.as_str());
    if stable == target.name.as_str() {
        eprintln!("skipping: backend does not spell an ALSA address into port names");
        return;
    }
    let stale_name = format!("{stable} 999:0");

    let dir = std::env::temp_dir().join(format!("aura-readdr-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let routing_path = dir.join("routing.json");
    persist::save_to_path(
        &routing_path,
        &persist::RoutingFile {
            ports: HashMap::new(),
            projects: HashMap::from([(
                persist::project_key(&dir),
                persist::ProjectRouting {
                    routes: vec![persist::PersistedRoute {
                        scope: "track".into(),
                        id: "t-1".into(),
                        port_name: stale_name.clone(),
                        channel: None,
                        return_device: None,
                    }],
                    open_ports: vec![stale_name],
                },
            )]),
        },
    );

    let out = MidiOut::default();
    out.set_routing_path_for_test(routing_path);
    out.adopt_project(&dir);

    let routes = out.routes();
    let got = routes.get(&RouteScope::Track("t-1".into()));
    assert_eq!(
        got.map(|t| t.port_id.as_str()),
        Some(target.id.as_str()),
        "the route resolved to the device at its NEW address: {routes:?}"
    );

    out.close_port(&target.id).ok();
    let _ = std::fs::remove_dir_all(&dir);
}
