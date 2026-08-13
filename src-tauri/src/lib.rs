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
pub mod mcp;
pub mod midi;
pub mod plugins;
pub mod sidecars;

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
            let engine = audio_state
                .engine_handle()
                .ok_or("audio engine failed to start")?;
            let jobs = app.state::<sidecars::SidecarState>().manager();
            let session = app.state::<midi::MidiState>().shared(session);
            let emitter = handle.clone();
            let control_plane = Arc::new(control::ControlPlane::new(
                session,
                shared,
                tables,
                engine,
                jobs,
                Box::new(move |event, payload| {
                    if let Err(e) = emitter.emit(event, payload) {
                        log::warn!("emit {event}: {e}");
                    }
                }),
            ));
            app.manage(control_plane);

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
            // ---- audio: recording ----
            audio::start_recording,
            audio::stop_recording,
            // ---- audio: metering / waveforms ----
            audio::subscribe_meters,
            audio::get_waveform_tile,
            // ---- audio: tracks ----
            audio::add_track,
            audio::remove_track,
            audio::get_tracks,
            audio::set_track_gain,
            audio::set_track_pan,
            audio::set_track_mute,
            audio::set_track_solo,
            audio::set_track_arm,
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
            control::import_audio_clip,
            control::seed_demo_project,
            // ---- control plane: wave 1 features ----
            control::hum::hum_to_song,
            control::export::export_song,
            control::export::export_job_status,
            control::export::export_capabilities,
            control::import::import_audio_clip_split_stems,
            control::loopjam::loopjam_evolve,
            control::loopjam::loopjam_cancel,
            control::loopjam::loopjam_status,
            // ---- midi (phase 2, zone C) ----
            midi::set_tempo_map,
            midi::midi_add_clip,
            midi::midi_set_notes,
            midi::midi_get_clips,
            midi::midi_import_file,
            midi::midi_export_file,
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
            plugins::patches::zyn_list_patches,
            plugins::patches::zyn_load_patch,
            // ---- automation groundwork (phase 3, zone P4) ----
            plugins::automation::automation_get,
            plugins::automation::automation_set,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
