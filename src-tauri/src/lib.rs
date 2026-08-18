// =============================================================================
// AURA — application library root.
//
// FROZEN FILE — owned by Agent 1 (architecture). Phase-2 agents must NOT edit
// this file. It wires up the full, pre-agreed command surface; the actual
// implementations live in the module directories (ownership zones in
// docs/PHASE2-PLAN.md):
//
//   src/audio/     — audio engine, transport, recording, meters, waveforms
//                    (+ sampler scaffold: sampler zone D)
//   src/control/   — SHARED CONTROL PLANE (ARCHITECTURE §11): the seam both
//                    Tauri commands and MCP tools call. Architect-owned;
//                    import.rs duties delegated to zone B.
//   src/sidecars/  — job manager + workers (zone B adds generation kinds)
//   src/midi/      — tick timeline, tempo map, MIDI clips, synth (zone C)
//   src/mcp/       — embedded MCP server, policy, auth (zone A)
//
// Modules expose:
//   * `#[tauri::command]` functions listed in `generate_handler!` below
//     (names are frozen; signatures/bodies may evolve inside the modules),
//   * a `State` type constructed via `Default` and managed here,
//   * an `init(&AppHandle)` hook called from `setup` for module-owned startup.
//
// If a module needs a new command registered, request it via report — do not
// edit this file.
// =============================================================================

pub mod audio;
pub mod control;
pub mod ids;
// Library & browser panel backend (Track E: directory scan, default library
// root, file audition). Self-contained; wired here purely additively, the
// same carve-out `midi_input` takes. Not part of the frozen phase-2 command
// surface described above.
pub mod library;
pub mod mcp;
pub mod midi;
// Hardware MIDI input (slice 1: enumerate/select/activity — see module doc).
// Self-contained; wired here purely additively per the midi-input-ports
// branch's task spec. Not part of the frozen phase-2 command surface above.
pub mod midi_input;
// MIDI output: transport sync (clock/Start/Stop/Continue/SPP) + note-out to
// external gear (slice 2, Task 6/7). Engine-free per ruling 3 — pure state
// machine (Task 6) driven by the `aura-midi-out` thread (Task 7).
pub mod midi_out;
// OS clipboard text slot for cross-instance clip copy/paste (Track C, Task
// 10). Self-contained; additive per the same carve-out midi_input takes.
pub mod osclipboard;
pub mod modulation;
pub mod plugins;
pub mod sidecars;
pub mod theme;
// The Composer's music-theory library (Plan H1). PURE: no tauri, no locks,
// no I/O, no state — see the purity contract in `theory/mod.rs`. The
// stateful seam that calls it is `control::composer`.
pub mod theory;
pub mod time;

use std::sync::Arc;

use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(audio::AudioState::default())
        .manage(sidecars::SidecarState::default())
        .manage(midi::MidiState::default())
        .manage(Arc::new(midi_input::MidiInputManager::default()))
        .manage(Arc::new(midi_out::MidiOut::default()))
        .manage(mcp::McpState::default())
        .manage(plugins::PluginState::default())
        .setup(|app| {
            let handle = app.handle();
            audio::init(handle)?;
            plugins::init(handle)?;
            sidecars::init(handle)?;

            // Construct the shared control plane (ARCHITECTURE §11) from the
            // initialized modules and manage it: Tauri commands AND the MCP
            // server drive AURA exclusively through this object. Store and
            // midi live behind ONE lock (`Session`) — `AudioState` builds it;
            // `MidiState::shared` wires the same Arc in and registers it with
            // the engine-rebuild hook (playback integration).
            let audio_state = app.state::<audio::AudioState>();
            let (session, shared, tables) = audio_state.control_parts();
            // Plan E Task 17: the ONE history + journal, already held by the
            // engine's own `Committer` (audio::init) — shared, not a second
            // instance, so the engine's non-transient commits are undoable.
            let history_log = audio_state.history_log();
            let engine = audio_state
                .engine_handle()
                .ok_or("audio engine failed to start")?;
            let jobs = app.state::<sidecars::SidecarState>().manager();
            let session = app.state::<midi::MidiState>().shared(session);
            let emitter = handle.clone();
            // Task 7: the `aura-midi-out` thread needs its own clones of the
            // same session/shared-atomics `Arc`s `ControlPlane::new` is
            // about to consume below.
            let midi_out_session = session.clone();
            let midi_out_shared = shared.clone();
            let control_plane = Arc::new(control::ControlPlane::new(
                session,
                shared,
                tables,
                engine.clone(),
                jobs,
                Box::new(move |event, payload| {
                    if let Err(e) = emitter.emit(event, payload) {
                        log::warn!("emit {event}: {e}");
                    }
                }),
                history_log,
            ));
            app.manage(control_plane.clone());
            // MIDI slice 2, Task 5: attach the already-`.manage`d MIDI-input
            // manager so `ControlPlane::select_midi_input_port` can reach it
            // (unit tests' `ControlPlane`s never attach one — see the field's
            // doc for why the port method errors instead of panicking there).
            control_plane
                .attach_midi_input(app.state::<Arc<midi_input::MidiInputManager>>().inner().clone());
            // MIDI slice 2, Task 7: wire the `aura-midi-out` driver the same
            // way — attach its transport-atomics/session handles once, then
            // attach it to the control plane so the MIDI-out routing/port/
            // clock methods can reach it.
            let midi_out = app.state::<Arc<midi_out::MidiOut>>().inner().clone();
            crate::midi::launch::runtime().attach_drive(midi_out_shared.clone(), midi_out_session.clone());
            midi_out.attach(midi_out_session, midi_out_shared);
            control_plane.attach_midi_out(midi_out);

            // MIDI launch: hardware note-on → seek/loop/play. Installed
            // here so the midir callback can fire without knowing the
            // control plane. The callback is the midir thread, not RT.
            {
                let cp = control_plane.clone();
                crate::midi::launch::runtime().install_fire(std::sync::Arc::new(move |cmd| {
                    match cmd {
                        crate::midi::launch::FireCmd::Start(b, origin) => {
                            if let Err(e) = cp.launch_fire_from(&b.id, origin) {
                                log::warn!("launch fire {}: {e}", b.id);
                            }
                        }
                        crate::midi::launch::FireCmd::Release(id) => {
                            cp.stop_drive_launch(&id);
                        }
                    }
                }));
            }

            // Plan E Task 13: the engine's narrow "document birth" closure,
            // installable only now that `ControlPlane` exists — bound over
            // this SAME `Arc<ControlPlane>`, so `start_recording`'s
            // auto-project (audio/engine.rs's `ensure_project`) shares the
            // ONE sanctioned epoch fn instead of duplicating the store-swap
            // + `project://changed` emit engine-side.
            {
                let cp = control_plane.clone();
                engine.install_ensure_project(std::sync::Arc::new(move || cp.ensure_project_epoch()));
            }

            // Loop-jam session driver rides on the shared control plane.
            app.manage(std::sync::Arc::new(control::loopjam::LoopJam::new(
                app.state::<Arc<control::ControlPlane>>().inner().clone(),
            )));

            mcp::init(handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ---- audio: transport ----
            audio::transport_play,
            audio::transport_stop,
            audio::transport_seek,
            audio::transport_set_loop,
            audio::transport_set_stop_at_end,
            audio::get_transport_state,
            // ---- audio: devices ----
            audio::list_input_devices,
            audio::list_output_devices,
            audio::select_input_device,
            audio::select_output_device,
            audio::set_metronome,
            // ---- audio: recording ----
            audio::start_recording,
            audio::stop_recording,
            // ---- audio: metering / waveforms ----
            audio::subscribe_meters,
            audio::get_waveform_tile,
            // ---- audio: pitch coach (additive; new names only) ----
            audio::pitch_listen_start,
            audio::pitch_listen_stop,
            audio::pitch_subscribe,
            audio::pitch_unsubscribe,
            audio::set_rehearse_hold,
            audio::pitch_set_reference,
            audio::pitch_score,
            audio::pitch_track,
            audio::pitch_analyze_clip,
            // ---- audio: tracks ----
            audio::add_track,
            audio::remove_track,
            audio::get_tracks,
            audio::set_track_gain,
            audio::set_track_pan,
            audio::set_track_mute,
            audio::set_track_solo,
            audio::set_track_arm,
            audio::set_track_name,
            audio::set_track_group,
            audio::arrange_lanes,
            // ---- audio: project ----
            audio::create_project,
            audio::open_project,
            audio::save_project,
            audio::save_project_as,
            // ---- audio: sampler (phase 2, zone D) ----
            audio::sampler_load_instrument,
            audio::sampler_list_instruments,
            audio::sampler_preview_note,
            audio::set_track_instrument,
            // ---- control plane (phase 2) ----
            control::get_project_state,
            control::set_track_mix,
            control::move_clip,
            control::move_clips,
            control::clipboard::clips_copy,
            control::clipboard::clips_paste,
            osclipboard::os_clipboard_write_text,
            osclipboard::os_clipboard_read_text,
            control::remove_clip,
            // Additive: "open in external editor" (double-click an audio
            // clip on the timeline).
            control::open_clip_in_external_editor,
            control::gesture_begin,
            control::gesture_end,
            // ---- control plane: undo/redo (Plan E Task 17, additive) ----
            control::undo,
            control::redo,
            control::history_overview,
            control::history_version,
            control::import_audio_clip,
            control::seed_demo_project,
            // ---- control plane: wave 1 features ----
            control::hum::hum_to_song,
            // ---- the Composer (Plan H1, additive) ----
            control::composer::harmony_get,
            control::composer::harmony_set,
            control::composer::composer_palette,
            control::composer::composer_suggest,
            control::composer::composer_generate,
            control::export::export_song,
            control::export::export_job_status,
            control::export::export_capabilities,
            control::import::import_audio_clip_split_stems,
            control::import::split_stems_for_clip,
            control::loopjam::loopjam_evolve,
            control::loopjam::loopjam_cancel,
            control::loopjam::loopjam_status,
            // ---- midi (phase 2, zone C) ----
            midi::set_tempo_map,
            midi::midi_add_clip,
            midi::midi_set_notes,
            midi::midi_set_clip_bounds,
            midi::midi_set_clip_placement,
            midi::midi_rename_clip,
            midi::midi_remove_clip,
            midi::midi_get_clips,
            midi::midi_import_file,
            midi::midi_export_file,
            // ---- midi input: hardware ports (slice 1, midi-input-ports) ----
            midi_input::midi_list_input_ports,
            midi_input::midi_select_input_port,
            midi_input::midi_select_input_track,
            midi_input::midi_input_status,
            // ---- midi output: clock/sync + per-track/per-clip routing ----
            midi_out::midi_list_output_ports,
            midi_out::midi_open_output_port,
            midi_out::midi_close_output_port,
            midi_out::midi_set_output_clock_enabled,
            midi_out::midi_output_status,
            midi_out::midi_set_track_route,
            midi_out::midi_set_track_return,
            midi_out::midi_set_clip_route,
            // ---- MIDI launch map (note → region/clip) ----
            midi::launch::launch_get,
            midi::launch::launch_set,
            midi::launch::launch_set_drive,
            midi::launch::launch_set_drive_focus,
            midi::launch::launch_set_map,
            midi::launch::launch_fire,
            midi::launch::launch_learn_arm,
            midi::launch::launch_learn_take,
            // ---- library & browser (Track E, additive) ----
            library::library_scan,
            library::library_default_root,
            audio::library_audition,
            audio::library_audition_stop,
            // ---- sidecars: jobs ----
            sidecars::sidecar_split_stems,
            sidecars::sidecar_transcribe,
            sidecars::sidecar_run_job,
            sidecars::sidecar_job_status,
            sidecars::sidecar_cancel_job,
            sidecars::sidecar_list_jobs,
            // ---- mcp (phase 2, zone A) ----
            mcp::mcp_get_status,
            mcp::mcp_set_policy,
            mcp::mcp_confirm_pending,
            // ---- plugins (phase 3 — ARCHITECTURE §15) ----
            plugins::plugin_scan,
            plugins::plugin_list,
            plugins::plugin_instantiate,
            plugins::plugin_remove,
            plugins::plugin_get_params,
            plugins::plugin_set_param,
            plugins::insert_add,
            plugins::insert_remove,
            plugins::insert_reorder,
            plugins::insert_set_bypass,
            plugins::patches::zyn_list_patches,
            plugins::patches::zyn_load_patch,
            // ---- automation groundwork (phase 3, zone P4) ----
            plugins::automation::automation_get,
            plugins::automation::automation_set,
            // ---- modulation graph (Track F) ----
            modulation::commands::modulation_get,
            modulation::commands::modulation_set_curve,
            modulation::commands::modulation_set_binding,
            modulation::commands::automation_clip_set,
            // ---- themes ----
            theme::list_user_themes,
            theme::write_user_theme,
            theme::user_themes_dir,
        ])
        // `build` + `run(callback)` rather than `run(context)`: the app has
        // to do something on the way out. See `RunEvent::Exit` below.
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app, event| {
            if matches!(event, tauri::RunEvent::Exit) {
                midi_out::release_on_exit(app.state::<Arc<midi_out::MidiOut>>().inner());
            }
        });
}
