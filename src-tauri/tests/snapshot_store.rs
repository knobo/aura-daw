//! Plan F Task 5 — THE EQUIVALENCE SWEEP.
//!
//! `Session::published` claims a property, not a hope: **when no lock is
//! held, the published `SessionSnapshot` is content-equal to the live
//! document.** Every successful `transact` upholds it by capturing INSIDE
//! the session lock; every sanctioned non-op writer upholds it with a
//! `// snapshot republish:` call under its own write lock. That enumeration
//! of writers is the correctness root — and a hand-maintained enumeration is
//! exactly the kind of thing that silently goes stale.
//!
//! So this test drives a REAL `ControlPlane` through one op of EVERY family
//! plus EVERY epoch function, and after each step asserts canonical-
//! serialized equality between the live document (read under a short lock)
//! and the image in the published slot. A writer that forgets to republish
//! fails here, at the step that forgot.
//!
//! Fixture and oracle style deliberately mirror `tests/figma_invariant.rs`
//! (real headless `ControlPlane`, local independent fixture, canonical JSON
//! snapshot) — this directory's convention.
//!
//! WHAT THE ORACLE MASKS, and why (the equivalence contract's exact edges):
//! * `midi.dirty` / `midi.loaded_dir` — BOOKKEEPING, deliberately not in
//!   `SessionSnapshot` at all. `execute_persist` flips `dirty` outside any
//!   transaction and must not have to republish for it: it describes the
//!   document's relationship to DISK, not the document.
//! * `plugins.dirty_state` — same category, but with a wrinkle:
//!   `SessionSnapshot` carries `PluginDoc` WHOLE (simplicity), so the field
//!   is physically present in the image and is documented there as advisory.
//!   `execute_persist` clears ids from the live set without republishing, so
//!   the image's copy legitimately goes stale. Masked on both sides.
//! * `next_note_id` per midi clip — scope ruling 3 / ADR 0001. Live and
//!   image are read at the same instant here so they always agree today;
//!   masked anyway to match the house oracle (`figma_invariant.rs`,
//!   `channel_properties.rs`) so this test never becomes the one place a
//!   monotonic watermark can fail a comparison.
//!
//! WHAT THIS SWEEP DOES AND DOES NOT PIN — measured, not guessed. Every one
//! of the ELEVEN `// snapshot republish:` sites was deleted in turn and this
//! test re-run (Task 10: `try_seed_zyn_demo_instruments`'s two direct-write
//! sites — the push and its rollback arm — died with the function itself;
//! R-3 closed, the demo's Zyn bootstrap is ops in the seed transaction now,
//! so there is nothing left there to republish). NINE of the eleven turn it
//! RED at the exact step that lost the call. The two that do not, and what
//! covers them instead:
//!
//! MARKED CORRECTION (ADR 0007, Plan F Task 11): this paragraph said TEN and
//! "eight of the ten" through Task 10. Both were wrong, and the error is
//! older than Task 10's subtraction: there were THIRTEEN marked sites before
//! it, not twelve, so removing two leaves eleven. The RED tally also missed
//! `plugins::state::adopt_open_project`'s install (`state.rs`), which IS
//! driven RED by this sweep's unreadable-automation step below. Re-counted
//! for this correction by two independently shaped audits that agree on
//! eleven: reading every `// snapshot republish:` marker, and an
//! ACCESSOR-anchored audit (`iter_mut(` / `get_mut(` / `entry(` /
//! `values_mut(` / `first_mut(` / `last_mut(` on a document collection, plus
//! every fn taking `&mut Session`/`Store`/`MidiStore`/`PluginDoc`/
//! `AutomationDoc`) that traced each production mutable borrow either into
//! `apply_raw` (captured by `transact`) or into one of the eleven.
//! * `Committer::apply_instantiate_writeback` (R-4, `control/mod.rs`) —
//!   unreachable from here BY CONSTRUCTION: this sweep uses `format: "stub"`
//!   rows precisely so no host round-trip happens, and that writeback only
//!   runs on a real host's `Instantiate` result. Pinned instead by
//!   `control`'s `instantiate_writeback_lands_when_the_epoch_is_unchanged`,
//!   where deleting the republish is measured to fail.
//! * `plugins::state::reactivate_restored_with` — unreachable for the same
//!   reason and then some (it needs a live plugin host, not just a real
//!   format). Pinned instead by `plugins::state`'s
//!   `zyn_state_roundtrips_through_real_project_save_open`, where deleting
//!   the republish is measured to fail. KNOWN GAP: that test SKIPS at
//!   runtime when zynaddsubfx-lv2 is absent, so on a machine without it this
//!   site has no coverage at all while looking covered. Reaching it without
//!   a host would need a host-injection seam in `plugins::state`, which is
//!   production API surface this task deliberately does not add.
//!
//! Three sites USED to be unpinnable-looking and are not. They are each
//! followed, inside the same command, by `plugins::automation::
//! adopt_open_project`, which republishes on both its `Ok` arms and so
//! restored equivalence before this test could next take the lock. Both
//! shadows are removed by construction rather than documented away:
//! * `create_project_at`'s and `open_project_epoch`'s swap republishes are
//!   exercised on a sub-fixture that never registers the adoption seams, so
//!   both cascades bail at their first line and the swap republish is the
//!   command's only one.
//! * `plugins::state::adopt_open_project`'s install republish is exercised
//!   through a `project.json` whose `automation` field is not an array —
//!   `load_lanes` returns `Err` and that arm does NOT republish. A real
//!   production path, not a contrivance.
//!
//! Each of the three is measured RED when its republish is deleted, and each
//! step carries an anti-vacuity assertion that the content really changed.
//!
//! Everything else — tracks, audio clips, midi rows AND their notes,
//! tempo/meter maps, ppq, automation lanes, the plugin rows, the param
//! mirror, the parked state blobs, project meta, the whole transport
//! mirror, `rev` and `epoch` — is under strict byte-identical scrutiny.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;

use aura_lib::audio::engine::{self, EventSink};
use aura_lib::audio::rt::{GraphTables, SharedRt};
use aura_lib::audio::types::{Clip, Store};
use aura_lib::control::op::{ObjectRef, Op, PropPath, TxMeta};
use aura_lib::control::snapshot::SessionSnapshot;
use aura_lib::control::{Committer, ControlPlane, EventEmitter, Session, TransportAction};
use aura_lib::ids::{ContentId, LaneId, NoteId, SourceId};
use aura_lib::midi::{MeterEvent, MidiClip, MidiNote, MidiStore, TempoEvent};
use aura_lib::plugins::automation::{AutomationLane, AutomationPoint};
use aura_lib::plugins::PluginInstanceInfo;
use aura_lib::sidecars::jobs::JobManager;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct NullEvents;
impl EventSink for NullEvents {
    fn emit(&self, _event: &str, _payload: serde_json::Value) {}
}

/// A real, headless `ControlPlane` — plus the `Session` handle, which this
/// test needs directly (the published slot is reached through `Session`, and
/// the live half must be read under the SAME lock acquisition to be a
/// meaningful comparison).
fn fixture() -> (Arc<ControlPlane>, Arc<Mutex<Session>>, engine::EngineHandle) {
    let (cp, session, eng, _log) = fixture_with_log();
    (cp, session, eng)
}

/// The same fixture, plus the `HistoryLog` handle — the version graph lives
/// behind it, and Task 7's test needs to read its stats.
fn fixture_with_log(
) -> (Arc<ControlPlane>, Arc<Mutex<Session>>, engine::EngineHandle, Arc<aura_lib::control::HistoryLog>) {
    let shared = Arc::new(SharedRt::default());
    let tables = GraphTables::empty();
    let session = Arc::new(Mutex::new(Session::new(Store::default(), MidiStore::default())));
    let log = Arc::new(aura_lib::control::HistoryLog::new());
    let log2 = log.clone();
    let committer = Committer::new(
        session.clone(),
        shared.clone(),
        tables.clone(),
        Arc::new(Box::new(|_: &str, _: serde_json::Value| {}) as EventEmitter),
        log.clone(),
    );
    let gesture = Arc::new(aura_lib::control::GestureState::new());
    let eng = engine::start(
        shared.clone(),
        tables.clone(),
        session.clone(),
        Box::new(NullEvents),
        committer,
        gesture.clone(),
    );
    let cp = Arc::new(ControlPlane::new(
        session.clone(),
        shared,
        tables,
        eng.clone(),
        Arc::new(JobManager::new(2, std::time::Duration::ZERO)),
        Box::new(|_e, _p| {}),
        log,
        gesture,
    ));
    (cp, session, eng, log2)
}

fn tmp_parent(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "aura-snapstore-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---------------------------------------------------------------------------
// The canonical oracle — ONE shape, rendered from either side
// ---------------------------------------------------------------------------

/// Canonical JSON for the LIVE document. Deliberately the same field set,
/// in the same order, as [`image_json`] — the two functions are a matched
/// pair and must be edited together (a field added to `SessionSnapshot`
/// without a line here would simply not be swept).
fn live_json(s: &Session) -> serde_json::Value {
    doc_json(
        s.rev(),
        s.epoch(),
        &s.store.transport,
        &s.store.project_dir,
        &s.store.project_name,
        &s.store.created_at,
        &s.store.tracks,
        &s.store.clips,
        s.midi.ppq,
        &s.midi.tempo_events,
        &s.midi.meter_events,
        &s.midi.clips.iter().collect::<Vec<_>>(),
        &s.midi.launch_maps,
        &s.automation.lanes,
        &s.modulation,
        &s.plugins.instances,
        &s.plugins.params,
        &s.plugins.pending_state,
    )
}

/// Canonical JSON for the PUBLISHED image — same shape as [`live_json`].
fn image_json(snap: &SessionSnapshot) -> serde_json::Value {
    doc_json(
        snap.rev,
        snap.epoch,
        &snap.transport,
        &snap.project_dir,
        &snap.project_name,
        &snap.created_at,
        &snap.tracks,
        &snap.clips,
        snap.midi.ppq,
        &snap.midi.tempo_events,
        &snap.midi.meter_events,
        &snap.midi.clips.iter().map(|c| c.as_ref()).collect::<Vec<_>>(),
        &snap.midi.launch_maps,
        &snap.automation,
        &snap.modulation,
        &snap.plugins.instances,
        &snap.plugins.params,
        &snap.plugins.pending_state,
    )
}

#[allow(clippy::too_many_arguments)]
fn doc_json(
    rev: u64,
    epoch: u64,
    transport: &aura_lib::audio::types::TransportState,
    project_dir: &Option<std::path::PathBuf>,
    project_name: &Option<String>,
    created_at: &Option<String>,
    tracks: &[aura_lib::audio::types::TrackState],
    clips: &[Clip],
    ppq: u32,
    tempo_events: &[TempoEvent],
    meter_events: &[MeterEvent],
    midi_clips: &[&MidiClip],
    launch_maps: &[aura_lib::midi::launch::LaunchMap],
    automation: &[AutomationLane],
    modulation: &aura_lib::modulation::ModulationDoc,
    plugin_rows: &[PluginInstanceInfo],
    plugin_params: &std::collections::HashMap<String, Vec<aura_lib::plugins::ParamInfo>>,
    pending_state: &std::collections::HashMap<String, Vec<u8>>,
) -> serde_json::Value {
    // `next_note_id` masked (see the module doc); every other midi field,
    // notes and their `noteId`s included, stays exact.
    let midi_clips: Vec<serde_json::Value> = midi_clips
        .iter()
        .map(|c| {
            let mut v = serde_json::to_value(c).expect("MidiClip serializes");
            if let Some(o) = v.as_object_mut() {
                o.insert("nextNoteId".into(), serde_json::json!(0));
            }
            v
        })
        .collect();
    // HashMaps -> BTreeMaps: iteration order is not part of the document.
    let params: BTreeMap<&String, &Vec<aura_lib::plugins::ParamInfo>> =
        plugin_params.iter().collect();
    let pending: BTreeMap<&String, &Vec<u8>> = pending_state.iter().collect();
    // NOTE: `dirty_state` is absent by design — bookkeeping (module doc).
    serde_json::json!({
        "rev": rev,
        "epoch": epoch,
        "transport": transport,
        "projectDir": project_dir,
        "projectName": project_name,
        "createdAt": created_at,
        "tracks": tracks,
        "clips": clips,
        "ppq": ppq,
        "tempoEvents": tempo_events,
        "meterEvents": meter_events,
        "midiClips": midi_clips,
        "launchMaps": launch_maps,
        "automation": automation,
        "modulation": {
            "curves": &modulation.curves,
            "bindings": &modulation.bindings,
            "automationClips": &modulation.automation_clips,
        },
        "plugins": plugin_rows,
        "pluginParams": params,
        "pendingState": pending,
    })
}

/// THE ASSERTION. Reads the live document and the published image under ONE
/// session-lock acquisition — the exact instant the contract talks about
/// ("equal when no lock is held" is only checkable from inside the lock, and
/// checking it from inside is strictly stronger).
#[track_caller]
fn assert_published_matches_live(session: &Mutex<Session>, step: &str) {
    let (live, image) = {
        let s = session.lock();
        // Leaf lock, pointer clone — the same access pattern the engine uses.
        let image = s.published_handle().lock().clone();
        (live_json(&s), image_json(&image))
    };
    if live != image {
        panic!(
            "PUBLISHED SNAPSHOT DIVERGED FROM THE LIVE DOCUMENT after step: {step}\n\
             This means a writer mutated the document without publishing. Find it and\n\
             give it a `// snapshot republish:` call under its own write lock.\n\
             --- live ----\n{}\n--- published ----\n{}",
            serde_json::to_string_pretty(&live).unwrap_or_default(),
            serde_json::to_string_pretty(&image).unwrap_or_default(),
        );
    }
}

// ---------------------------------------------------------------------------
// Op payload builders (mirroring figma_invariant.rs's, same stub-host trick)
// ---------------------------------------------------------------------------

fn audio_clip(id: &str, track_id: &str) -> Clip {
    Clip {
        id: id.into(),
        track_id: track_id.into(),
        name: format!("Clip {id}"),
        source_path: format!("audio/{id}.wav"),
        source_id: SourceId::default(),
        source_channels: 2,
        source_sample_rate: 48_000,
        source_length_samples: 48_000,
        timeline_start_samples: 0,
        offset_samples: 0,
        length_samples: 48_000,
        gain_db: 0.0,
        fade_in_samples: 0,
        fade_out_samples: 0,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
    }
}

fn midi_clip(id: &str, track_id: &str) -> MidiClip {
    MidiClip {
        id: id.into(),
        track_id: track_id.into(),
        name: format!("MIDI {id}"),
        timeline_start_ticks: 0,
        length_ticks: 3840,
        notes: vec![],
        next_note_id: 1,
        content_id: ContentId::mint(),
        lane_id: LaneId::default_for_track(track_id),
        content_length_ticks: None,
        transpose_semitones: 0,
        velocity_offset: 0,
    }
}

fn note(tick: u32, key: u8) -> MidiNote {
    MidiNote { tick, length_ticks: 480, key, velocity: 100, channel: 0, note_id: NoteId(0) }
}

/// `format: "stub"` = the host stub every plugin-family test in this
/// directory uses: `execute_host_forward`'s arms are documented no-ops for a
/// format that is neither "clap" nor "lv2", while the DOCUMENT half (row,
/// param mirror, state blob) travels through the ops exactly as for a real
/// host — which is all this test is about.
fn plugin_row(id: &str, track_id: &str) -> PluginInstanceInfo {
    PluginInstanceInfo {
        id: id.into(),
        uid: format!("stub:{id}"),
        name: format!("Stub {id}"),
        format: "stub".into(),
        status: "stub".into(),
        track_id: Some(track_id.into()),
    }
}

fn set_track(track_id: &str, path: PropPath, to: serde_json::Value) -> Op {
    Op::Set { object: ObjectRef::Track(track_id.into()), path, from: serde_json::Value::Null, to }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

#[test]
fn published_snapshot_tracks_the_live_document_across_every_op_family_and_epoch_fn() {
    let parent = tmp_parent("sweep");

    // ===== THE TWO SUB-FIXTURE EPOCH FNS, FIRST ===========================
    // `ensure_project_epoch` needs a session with NO project open, and
    // `seed_demo_project` wants a virgin one — so both get their own
    // fixture. They run FIRST, deliberately, and this ordering is
    // load-bearing: the main sweep below REGISTERS its session in the
    // process-global adoption seams (`midi::playback::register_store` /
    // `plugins::automation::register_session`, both `OnceLock`s where first
    // registration wins). An epoch fn on a sub-fixture running after that
    // registration would send its `adopt_open_project` cascade into the
    // MAIN session — the mid-air-shared-global hazard `plugins::state`'s
    // own tests document. Running them before any registration keeps each
    // sub-fixture's adopt inert and self-contained.

    // EPOCH FN: ensure — `ensure_project_epoch` -> `ensure_default_project`.
    {
        let (cp2, session2, eng2) = fixture();
        assert_published_matches_live(&session2, "second fixture birth");
        cp2.ensure_project_epoch().expect("ensure project");
        assert_published_matches_live(&session2, "ensure_project_epoch (ensure_default_project swap)");
        eng2.send(aura_lib::audio::engine::ControlMsg::Shutdown);
    }

    // EPOCH FN: seed demo — Task 10 (R-3 closed): the demo bootstrap no
    // longer writes the document directly anywhere; every row (tracks,
    // clips, and — when Zyn is available — plugin rows) lands through the
    // seed's one commit, so this is really a plain commit-equivalence check
    // now, kept for regression coverage of the whole seed path.
    {
        let (cp3, session3, eng3) = fixture();
        cp3.seed_demo_project().expect("seed demo project");
        assert_published_matches_live(&session3, "seed_demo_project");
        eng3.send(aura_lib::audio::engine::ControlMsg::Shutdown);
    }

    // EPOCH FNS: create + open, WITH THE ADOPT CASCADES DELIBERATELY INERT.
    // `create_project_at` and `open_project_epoch` each republish their own
    // swap block, and then BOTH `adopt_open_project` cascades republish
    // again a few statements later, still inside the same command. On the
    // registered main fixture below that shadows the swap republishes
    // completely: deleting either one leaves this test green, so it would
    // be pinning nothing. Here the seams are unregistered (this fixture
    // runs before any `register_*`), both cascades bail at their first
    // line, and the swap republish is the ONLY one in the command — which
    // makes its deletion observable. Same technique the `ensure_project_
    // epoch` fixture above already relies on.
    {
        let (cp4, session4, eng4) = fixture();
        cp4.create_project(parent.to_str().unwrap(), "Unshadowed").expect("create project");
        cp4.commit(TxMeta::user("add tracks"), |tx| {
            aura_lib::control::ops::add_track_tx(tx, Some("A".into()), Some("audio".into()))?;
            aura_lib::control::ops::add_track_tx(tx, Some("B".into()), Some("midi".into()))?;
            Ok(())
        })
        .expect("add tracks");
        // Save-As rather than leaning on the commit's own auto-persist: a
        // `TrackAdd`'s `PersistEffect` does not write `project.json`, so the
        // tracks would not be on disk for the open below to restore.
        let populated_dir = {
            let (_p, dir) = aura_lib::audio::project::create(&parent, "UnshadowedSaved", 48_000, 120.0)
                .expect("mint dir");
            cp4.save_project_as_epoch(&dir).expect("save as");
            dir
        };
        // The swap CLEARS tracks and clips, so this is a real content change
        // — the earlier `create_project` on a virgin session was not.
        cp4.create_project(parent.to_str().unwrap(), "UnshadowedBlank").expect("create blank");
        assert!(
            session4.lock().store.tracks.is_empty(),
            "the swap really cleared the tracks — otherwise nothing is being pinned"
        );
        assert_published_matches_live(&session4, "create_project_at swap (adopt cascades inert)");
        // And the open brings them back, again as the command's only
        // republish.
        cp4.open_project_epoch(&populated_dir).expect("open populated project");
        assert_eq!(
            session4.lock().store.tracks.len(),
            2,
            "the open really restored the tracks — otherwise nothing is being pinned"
        );
        assert_published_matches_live(&session4, "open_project_epoch swap (adopt cascades inert)");
        eng4.send(aura_lib::audio::engine::ControlMsg::Shutdown);
    }

    // ===== THE MAIN FIXTURE ===============================================
    let (cp, session, eng) = fixture();
    // Register it in both adoption seams, so `create_project_at`'s and
    // `open_project_epoch`'s `plugins::state`/`plugins::automation`
    // `adopt_open_project` cascades are LIVE rather than inert no-ops —
    // those installs are two of the enumerated republish sites, and an
    // inert adopt would sweep them vacuously.
    aura_lib::midi::playback::register_store(session.clone());
    aura_lib::plugins::automation::register_session(session.clone());

    // Step 0: birth. `Session::new` runs the first full capture itself, so
    // the invariant holds before anything at all has happened.
    assert_published_matches_live(&session, "Session::new (birth)");

    // ===== EPOCH FN 1: create ==============================================
    cp.create_project(parent.to_str().unwrap(), "Sweep").expect("create project");
    assert_published_matches_live(&session, "create_project (create_project_at swap)");

    // ===== OP FAMILIES ====================================================
    // TrackAdd (x2, via the tx-tier helper — the production path).
    let mut audio_track = String::new();
    let mut midi_track = String::new();
    cp.commit(TxMeta::user("add tracks"), |tx| {
        let a = aura_lib::control::ops::add_track_tx(tx, Some("Audio".into()), Some("audio".into()))?;
        let m = aura_lib::control::ops::add_track_tx(tx, Some("Keys".into()), Some("midi".into()))?;
        audio_track = a.id.to_string();
        midi_track = m.id.to_string();
        Ok(())
    })
    .expect("add tracks");
    assert_published_matches_live(&session, "Op::TrackAdd");

    // Set{Track,_}
    cp.commit(TxMeta::user("mix"), |tx| {
        tx.apply(set_track(&audio_track, PropPath::Gain, serde_json::json!(-6.0)))?;
        tx.apply(set_track(&midi_track, PropPath::Muted, serde_json::json!(true)))
    })
    .expect("mix");
    assert_published_matches_live(&session, "Op::Set{Track,_}");

    // ClipAdd
    cp.commit(TxMeta::user("add clip"), |tx| {
        tx.apply(Op::ClipAdd { clip: audio_clip("c-sweep", &audio_track), index: 0 })
    })
    .expect("clip add");
    assert_published_matches_live(&session, "Op::ClipAdd");

    // Set{Clip,_}
    cp.commit(TxMeta::user("trim clip"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Clip("c-sweep".into()),
            path: PropPath::LengthSamples,
            from: serde_json::Value::Null,
            to: serde_json::json!(24_000u64),
        })
    })
    .expect("trim clip");
    assert_published_matches_live(&session, "Op::Set{Clip,_}");

    // MidiClipAdd
    cp.commit(TxMeta::user("add midi clip"), |tx| {
        tx.apply(Op::MidiClipAdd { clip: midi_clip("mc-sweep", &midi_track), index: 0 })
    })
    .expect("midi clip add");
    assert_published_matches_live(&session, "Op::MidiClipAdd");

    // MidiSetNotes — the per-clip-Arc path (ruling F-1's granularity).
    cp.commit(TxMeta::user("set midi notes"), |tx| {
        tx.apply(Op::MidiSetNotes {
            clip: "mc-sweep".into(),
            notes: vec![note(0, 60), note(480, 64), note(960, 67)],
        })
    })
    .expect("midi set notes");
    assert_published_matches_live(&session, "Op::MidiSetNotes");

    // Set{MidiClip,_} — placement lives on the row, so the row's Arc must
    // re-derive even though no note changed.
    cp.commit(TxMeta::user("midi clip bounds"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::MidiClip("mc-sweep".into()),
            path: PropPath::TimelineStartTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(1920u64),
        })?;
        tx.apply(Op::Set {
            object: ObjectRef::MidiClip("mc-sweep".into()),
            path: PropPath::LengthTicks,
            from: serde_json::Value::Null,
            to: serde_json::json!(7680u64),
        })
    })
    .expect("midi clip bounds");
    assert_published_matches_live(&session, "Op::Set{MidiClip,_}");

    // TempoSet — midi_meta AND the transport.tempo_bpm mirror.
    cp.commit(TxMeta::agent("set_tempo_map", "tempo"), |tx| {
        tx.apply(Op::TempoSet {
            ppq: 960,
            events: vec![TempoEvent { tick: 0, bpm: 128.0 }, TempoEvent { tick: 3840, bpm: 96.0 }],
            meter: vec![MeterEvent { tick: 0, num: 3, den: 4 }],
        })
    })
    .expect("tempo set");
    assert_published_matches_live(&session, "Op::TempoSet");

    // Launch* — named launchers live on the midi document and the image.
    cp.commit(TxMeta::user("add launcher"), |tx| {
        tx.apply(Op::LaunchMapSet {
            id: "default".into(),
            map: Some(aura_lib::midi::launch::LaunchMap::default_map()),
        })
    })
    .expect("launch map set");
    assert_published_matches_live(&session, "Op::LaunchMapSet");
    cp.commit(TxMeta::user("set launch binding"), |tx| {
        tx.apply(Op::LaunchBindingSet {
            map_id: "default".into(),
            id: "lb-sweep".into(),
            binding: Some(aura_lib::midi::launch::LaunchBinding {
                id: "lb-sweep".into(),
                name: "Scene 1".into(),
                note: 60,
                channel: None,
                quantize: Default::default(),
                target: aura_lib::midi::launch::LaunchTarget::Region {
                    start_ticks: 0,
                    length_ticks: 960,
                    track_ids: vec![audio_track.clone()],
                },
            }),
        })
    })
    .expect("launch binding set");
    assert_published_matches_live(&session, "Op::LaunchBindingSet");
    cp.commit(TxMeta::user("drive launch"), |tx| {
        tx.apply(Op::LaunchDriveSet {
            map_id: "default".into(),
            clip_id: "mc-sweep".into(),
            on: true,
        })
    })
    .expect("launch drive set");
    assert_published_matches_live(&session, "Op::LaunchDriveSet");

    // AutomationSetLane (upsert)
    cp.commit(TxMeta::user("automation"), |tx| {
        tx.apply(Op::AutomationSetLane {
            key: "lane-sweep".into(),
            lane: Some(AutomationLane {
                id: "lane-sweep".into(),
                target_node: format!("track:{audio_track}"),
                param_id: 0,
                points: vec![
                    AutomationPoint { tick: 0, value: 0.0 },
                    AutomationPoint { tick: 1920, value: 1.0 },
                ],
            }),
        })
    })
    .expect("automation set");
    assert_published_matches_live(&session, "Op::AutomationSetLane (upsert)");

    // PluginAdd
    cp.commit(TxMeta::user("add plugin"), |tx| {
        tx.apply(Op::PluginAdd { row: plugin_row("p-sweep", &midi_track), index: 0 })
    })
    .expect("plugin add");
    assert_published_matches_live(&session, "Op::PluginAdd");

    // Set{Plugin,_}
    cp.commit(TxMeta::user("plugin param"), |tx| {
        tx.apply(Op::Set {
            object: ObjectRef::Plugin("p-sweep".into()),
            path: PropPath::Param { index: 3 },
            from: serde_json::Value::Null,
            to: serde_json::json!(0.75),
        })
    })
    .expect("plugin param");
    assert_published_matches_live(&session, "Op::Set{Plugin,_}");

    // PluginSetState
    cp.commit(TxMeta::user("plugin patch"), |tx| {
        tx.apply(Op::PluginSetState { instance: "p-sweep".into(), state: vec![7u8; 32] })
    })
    .expect("plugin set state");
    assert_published_matches_live(&session, "Op::PluginSetState");

    // PluginRemove
    cp.commit(TxMeta::user("add plugin 2"), |tx| {
        tx.apply(Op::PluginAdd { row: plugin_row("p-sweep2", &audio_track), index: 1 })
    })
    .expect("plugin add 2");
    cp.commit(TxMeta::user("remove plugin 2"), |tx| {
        tx.apply(Op::PluginRemove {
            row: plugin_row("p-sweep2", &audio_track),
            index: 1,
            state: Some(vec![9u8, 9]),
            params: vec![],
        })
    })
    .expect("plugin remove 2");
    assert_published_matches_live(&session, "Op::PluginRemove");

    // AutomationSetLane (delete arm) — then immediately re-add a DURABLE
    // lane. That second lane is not redundant: it must still be in the
    // document at save-as/open time so `plugins::automation::
    // adopt_open_project` takes its `Ok(Some(lanes))` arm on the way back
    // in. Without it the open lands on the `Ok(None)` clear arm against an
    // already-empty lane list, and that republish site would be swept
    // vacuously — a no-op write publishes an identical image whether or not
    // anyone remembered to republish.
    cp.commit(TxMeta::user("automation delete"), |tx| {
        tx.apply(Op::AutomationSetLane { key: "lane-sweep".into(), lane: None })
    })
    .expect("automation delete");
    assert_published_matches_live(&session, "Op::AutomationSetLane (delete)");
    cp.commit(TxMeta::user("automation durable"), |tx| {
        tx.apply(Op::AutomationSetLane {
            key: "lane-durable".into(),
            lane: Some(AutomationLane {
                id: "lane-durable".into(),
                target_node: format!("track:{audio_track}"),
                param_id: 1,
                points: vec![
                    AutomationPoint { tick: 0, value: 0.25 },
                    AutomationPoint { tick: 960, value: 0.75 },
                ],
            }),
        })
    })
    .expect("automation durable");
    assert_published_matches_live(&session, "Op::AutomationSetLane (durable re-add)");

    // ModulationSetCurve — flags both modulation and derived automation.
    cp.commit(TxMeta::user("set curve"), |tx| {
        tx.apply(Op::ModulationSetCurve {
            key: "cur-sweep".into(),
            curve: Some(aura_lib::modulation::Curve {
                id: "cur-sweep".into(),
                name: "sweep".into(),
                length_ticks: None,
                points: vec![
                    AutomationPoint { tick: 0, value: 0.0 },
                    AutomationPoint { tick: 480, value: 1.0 },
                ],
            }),
        })
    })
    .expect("set curve");
    assert_published_matches_live(&session, "Op::ModulationSetCurve");

    // TrackRemove of a track that still owns an automation clip (no
    // prior clip-delete). from_ops must flag modulation+automation.
    let auto_track = {
        let mut id = String::new();
        cp.commit(TxMeta::system("add auto track"), |tx| {
            let t = aura_lib::control::ops::add_track_tx(
                tx,
                Some("Auto".into()),
                Some("automation".into()),
            )?;
            id = t.id.to_string();
            Ok(())
        })
        .expect("add auto track");
        id
    };
    cp.commit(TxMeta::user("place auto clip"), |tx| {
        tx.apply(Op::AutomationClipSet {
            key: "acl-sweep".into(),
            clip: Some(aura_lib::modulation::AutomationClip {
                id: "acl-sweep".into(),
                track_id: auto_track.clone(),
                curve_id: "cur-sweep".into(),
                timeline_start_ticks: 0,
                length_ticks: 1920,
                content_length_ticks: None,
            }),
        })
    })
    .expect("place auto clip");
    assert_published_matches_live(&session, "Op::AutomationClipSet");
    let doomed_auto = cp
        .project_state()
        .tracks
        .into_iter()
        .find(|t| t.id == auto_track)
        .expect("auto track exists");
    cp.commit(TxMeta::system("remove auto track"), |tx| {
        tx.apply(Op::TrackRemove {
            track: doomed_auto,
            index: 0,
            clips: vec![],
            clip_indices: vec![],
        })
    })
    .expect("remove auto track");
    assert_published_matches_live(&session, "Op::TrackRemove (still owned automation clip)");

    // MidiClipRemove
    cp.commit(TxMeta::user("remove midi clip"), |tx| {
        tx.apply(Op::MidiClipRemove { clip: midi_clip("mc-sweep", &midi_track), index: 0 })
    })
    .expect("midi clip remove");
    assert_published_matches_live(&session, "Op::MidiClipRemove");

    // ClipRemove
    cp.commit(TxMeta::user("remove clip"), |tx| {
        tx.apply(Op::ClipRemove { clip: audio_clip("c-sweep", &audio_track), index: 0 })
    })
    .expect("clip remove");
    assert_published_matches_live(&session, "Op::ClipRemove");

    // TrackRemove — carries its clips, so `clips` must re-derive too.
    let doomed_row = {
        let mut id = String::new();
        cp.commit(TxMeta::system("add scratch track"), |tx| {
            let t =
                aura_lib::control::ops::add_track_tx(tx, Some("Scratch".into()), Some("audio".into()))?;
            id = t.id.to_string();
            Ok(())
        })
        .expect("add scratch track");
        cp.project_state().tracks.into_iter().find(|t| t.id == id).expect("scratch track exists")
    };
    cp.commit(TxMeta::system("remove scratch track"), |tx| {
        tx.apply(Op::TrackRemove { track: doomed_row, index: 0, clips: vec![], clip_indices: vec![] })
    })
    .expect("remove scratch track");
    assert_published_matches_live(&session, "Op::TrackRemove");

    // Set{Transport,_} via the TRANSIENT path — transient batches capture
    // too (the engine's rebuild must see the transport mirrors), which is
    // exactly what this step pins. No `Play`: a running transport lets the
    // engine's own auto-stop commit land mid-test and makes the comparison
    // depend on wall-clock timing rather than on the invariant.
    cp.transport(TransportAction::SetLoop { enabled: true, start_samples: 0, end_samples: 48_000 })
        .expect("set loop");
    assert_published_matches_live(&session, "Op::Set{Transport,_} (transient batch)");
    cp.transport(TransportAction::SetStopAtEnd { enabled: true }).expect("set stop at end");
    assert_published_matches_live(&session, "Op::Set{Transport,_} (stop at end)");

    // ===== R-1: the non-op plugin writer ==================================
    cp.set_plugin_pending_state("p-sweep", vec![3u8; 16]);
    assert_published_matches_live(&session, "set_plugin_pending_state (R-1)");

    // ===== THE Err ARM: a rolled-back batch publishes NOTHING =============
    let before = { session.lock().published_handle().lock().clone() };
    let err = cp.commit(TxMeta::user("doomed"), |tx| {
        tx.apply(set_track(&audio_track, PropPath::Gain, serde_json::json!(-12.0)))?;
        Err("deliberate failure".into())
    });
    assert!(err.is_err(), "the doomed batch must fail");
    let after = { session.lock().published_handle().lock().clone() };
    assert!(
        Arc::ptr_eq(&before, &after),
        "the Err/rollback arm must publish NOTHING — the inverses restored the \
         document, so the previous image still matches it"
    );
    // ...and it genuinely still matches, which is the point of publishing
    // nothing rather than the absence of a call being merely tolerated.
    assert_published_matches_live(&session, "rolled-back batch (Err arm)");

    // ===== EPOCH FN 2: save-as ============================================
    // `save_project_as` refuses when a project is already open (that is
    // plain `save_project` territory) — and one IS open, since the sweep
    // above needed a real project on disk. So mint the destination the same
    // way `save_project_as` does and call the epoch fn directly: it is the
    // half that does the swap, which is the half under test.
    let saved_dir = {
        let (_p, dir) =
            aura_lib::audio::project::create(&parent, "SweepSaved", 48_000, 120.0).expect("mint dir");
        cp.save_project_as_epoch(&dir).expect("save as epoch");
        dir
    };
    assert_published_matches_live(&session, "save_project_as_epoch (swap)");

    // ===== EPOCH FN 3: create, OVER A POPULATED DOCUMENT ==================
    // The `create_project` at the top of the sweep ran against a virgin
    // session, which made it a VACUOUS check of `create_project_at`'s
    // republish: it cleared an already-empty track/clip list, and project
    // meta is copied from live truth on every capture whether or not the
    // flag is set — so deleting that republish changed nothing observable.
    // (Measured: the site survived deletion with the sweep still green.)
    // Run it again now that the document has tracks, a lane and a plugin
    // row, so the swap is a real content change — and so both adopt
    // cascades hit their "the live document disagrees with the new dir"
    // arms: automation's `Ok(None)` CLEAR arm against a non-empty lane
    // list, and the plugin install against a non-empty plugin doc.
    cp.create_project(parent.to_str().unwrap(), "SweepBlank").expect("create blank project");
    assert_published_matches_live(
        &session,
        "create_project over a populated document (+ both adopt_open_project clear arms)",
    );

    // ===== EPOCH FN 4: open ===============================================
    // Open the saved project from the BLANK one — a real cold open where
    // every part comes back: store swap, eager midi adopt, then
    // `plugins::state`/`plugins::automation` `adopt_open_project` after the
    // lock drops, each installing content the live document does not have
    // (the plugin row, the durable lane), so each republish is load-bearing.
    // Opening `saved_dir` straight after saving it would compare a document
    // against itself and sweep all three sites vacuously.
    cp.open_project_epoch(&saved_dir).expect("open project");
    assert_published_matches_live(&session, "open_project_epoch (+ both adopt_open_project)");
    {
        let s = session.lock();
        assert!(!s.store.tracks.is_empty(), "the open really restored the tracks");
        assert!(!s.automation.lanes.is_empty(), "and the durable lane — otherwise the adopt sites sweep vacuously");
        assert!(!s.plugins.instances.is_empty(), "and the plugin row");
    }

    // ===== EPOCH FN 5: open with the automation file unreadable ===========
    // `plugins::state::adopt_open_project`'s install republish is the last
    // one still shadowed: `plugins::automation::adopt_open_project` runs
    // right after it and republishes on both its `Ok` arms. Its `Err` arm
    // does NOT — and that is a real production path, not a contrivance:
    // `load_lanes` returns `Err` for a `project.json` whose `automation`
    // field is not an array. Blank the document first so the plugin install
    // is a genuine content change, then open with the field corrupted, and
    // the plugin install's republish is the command's only one.
    cp.create_project(parent.to_str().unwrap(), "SweepBlank2").expect("create second blank");
    assert!(
        session.lock().plugins.instances.is_empty(),
        "the blank swap cleared the plugin doc — the install below must therefore change it"
    );
    {
        let f = saved_dir.join("project.json");
        let mut root: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&f).expect("read project.json")).expect("parse");
        let obj = root.as_object_mut().expect("object");
        // Track F writes `modulation{}` and drops `automation[]`. Corrupt
        // both so adopt cannot fall back to the v4 graph (that would fill
        // the lanes and re-shadow the plugin-install republish this step
        // pins). `automation: "nope"` is still the Err arm `load_lanes`
        // takes; removing `modulation` is what makes the v4 loader miss.
        obj.insert("automation".into(), serde_json::json!("nope"));
        obj.remove("modulation");
        std::fs::write(&f, serde_json::to_vec_pretty(&root).unwrap()).expect("write project.json");
    }
    cp.open_project_epoch(&saved_dir).expect("open project with a corrupt automation field");
    assert_published_matches_live(
        &session,
        "open_project_epoch with an unreadable automation field (plugins adopt unshadowed)",
    );
    {
        let s = session.lock();
        assert!(
            !s.plugins.instances.is_empty(),
            "the plugin install really ran — otherwise this step pins nothing"
        );
        assert!(
            s.automation.lanes.is_empty(),
            "and the automation adopt really took its non-republishing Err arm — if it \
             had adopted lanes, its own republish would be shadowing the install again"
        );
    }

    eng.send(aura_lib::audio::engine::ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}

// ---------------------------------------------------------------------------
// Plan F Task 7 — the version graph, driven through the real ControlPlane
// ---------------------------------------------------------------------------

/// The graph is fed from ONE sink, so what it holds must follow the same
/// rule the journal and the undo stack follow: one node per NON-transient
/// batch, nothing for a transient one, nothing across a document swap.
///
/// Driven through a real `ControlPlane` rather than `VersionGraph` directly,
/// because the claim under test is about the WIRING — that
/// `commit_with_rebuild`'s single call site reaches the third stream too.
#[test]
fn the_version_graph_gets_one_node_per_non_transient_commit_and_drains_at_a_swap() {
    let parent = tmp_parent("vergraph");
    let (cp, session, eng, log) = fixture_with_log();

    cp.create_project(parent.to_str().unwrap(), "Versions").expect("create project");
    // The create is an epoch boundary: the chain is rooted, empty, and now
    // describes THIS document.
    assert_eq!(log.version_stats().nodes, 0, "an epoch boundary leaves an empty chain");

    let mut track_id = String::new();
    cp.commit(TxMeta::user("add track"), |tx| {
        track_id = aura_lib::control::ops::add_track_tx(tx, Some("Keys".into()), Some("midi".into()))?
            .id
            .to_string();
        Ok(())
    })
    .expect("add track");
    assert_eq!(log.version_stats().nodes, 1, "a non-transient commit is one node");

    // A run of ordinary edits. Each is its own batch, so each is its own
    // node — the graph does NOT coalesce the way the 350 ms undo merge does.
    for db in [-3.0, -6.0, -9.0] {
        cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_track(&track_id, PropPath::Gain, serde_json::json!(db))))
            .expect("set gain");
    }
    let s = log.version_stats();
    assert_eq!(s.nodes, 4);
    assert_eq!(s.materialized, 4, "small batches materialize");
    assert!(s.retained_bytes > 0, "and they charge what they created");
    assert_eq!(s.newest_rev, Some(session.lock().rev()), "the newest node IS the live rev");

    let (overview_stats, items) = log.version_overview();
    assert_eq!(overview_stats, s, "the browser summary and rows share one graph snapshot");
    assert_eq!(items.len(), 4);
    assert_eq!(items[0].label, "gain");
    assert_eq!(items[0].actor, "You");
    assert!(items.windows(2).all(|pair| pair[0].rev > pair[1].rev), "browser rows are newest first");
    assert_eq!(
        items.iter().map(|item| item.charged_bytes).sum::<usize>(),
        s.retained_bytes,
        "row charges explain the graphs charged-byte total",
    );

    // TRANSIENT: a transport batch captures an image (the engine needs it)
    // but is journaled nowhere, undoable nowhere, and retained nowhere.
    let before = log.version_stats();
    cp.transport(TransportAction::SetLoop { enabled: true, start_samples: 0, end_samples: 48_000 })
        .expect("set loop");
    assert!(session.lock().rev() > before.newest_rev.unwrap(), "the transient batch really committed");
    assert_eq!(log.version_stats(), before, "...and left the version graph untouched");

    // An empty batch (a Set whose net from == to) is recorded nowhere
    // either — the same guard, now covering three streams.
    cp.commit(TxMeta::user("gain"), |tx| tx.apply(set_track(&track_id, PropPath::Gain, serde_json::json!(-9.0))))
        .expect("no-op gain");
    assert_eq!(log.version_stats().nodes, 4, "a batch that folded to nothing retains nothing");

    // A MATERIALIZABLE rev: what the graph holds can be read back, and it is
    // the document that rev produced.
    let rev = log.version_stats().newest_rev.unwrap();
    let image = log.materialize_version(rev).expect("the retained rev materializes");
    assert_eq!(image.rev, rev);
    let live_gain = session.lock().store.tracks.iter().find(|t| t.id == track_id).unwrap().gain_db;
    assert_eq!(
        image.tracks.iter().find(|t| t.id == track_id).unwrap().gain_db,
        live_gain,
        "the materialized image is the document, not an empty one"
    );
    assert!(log.materialize_version(rev + 999).is_none(), "an unknown rev materializes nothing");

    // THE DOCUMENT SWAP: every node describes a document nobody can reach.
    cp.create_project(parent.to_str().unwrap(), "Other").expect("create second project");
    assert_eq!(log.version_stats().nodes, 0, "a swap drains the chain");
    assert_eq!(log.version_stats().retained_bytes, 0, "including its byte accounting");
    assert!(log.materialize_version(rev).is_none(), "and the old revs are honestly gone");

    // ...and the guard is not a mute button: the new document records again.
    cp.commit(TxMeta::user("add track"), |tx| {
        aura_lib::control::ops::add_track_tx(tx, Some("New".into()), Some("audio".into()))?;
        Ok(())
    })
    .expect("add track in the new document");
    assert_eq!(log.version_stats().nodes, 1, "the new document's chain starts recording");

    eng.send(aura_lib::audio::engine::ControlMsg::Shutdown);
    let _ = std::fs::remove_dir_all(&parent);
}
