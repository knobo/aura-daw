//! The curated MCP tool surface (names FROZEN — ARCHITECTURE §12.2).
//!
//! Every handler is a thin wrapper over [`crate::control::ControlPlane`]
//! methods — NEVER over Tauri commands. Params/results are the same wire
//! shapes as the corresponding schemas so an MCP agent and the UI observe
//! identical state.
//!
//! | Tool                | ControlPlane call        | Kind        |
//! |---------------------|--------------------------|-------------|
//! | `get_project_state` | `project_state()`        | read-only   |
//! | `read_meters`       | `read_meters()`          | read-only — the agent's "ears": latest 60 Hz peak/RMS frame |
//! | `get_job_status`    | `job_status()/list_jobs()` | read-only |
//! | `create_project`    | `create_project()`       | destructive |
//! | `add_track`         | `add_track()`            | destructive |
//! | `set_track_mix`     | `set_track_mix(batch)`   | destructive |
//! | `transport_control` | `transport(action)`      | destructive (playback side effects) |
//! | `record_take`       | `start_recording()/stop_recording()` | destructive |
//! | `import_audio_clip` | `import_audio_clip()`    | destructive |
//! | `run_sidecar_job`   | `run_sidecar_job()`      | destructive (spawns processes / cloud calls) |

pub const GET_PROJECT_STATE: &str = "get_project_state";
pub const READ_METERS: &str = "read_meters";
pub const GET_JOB_STATUS: &str = "get_job_status";
pub const CREATE_PROJECT: &str = "create_project";
pub const ADD_TRACK: &str = "add_track";
pub const SET_TRACK_MIX: &str = "set_track_mix";
pub const TRANSPORT_CONTROL: &str = "transport_control";
pub const RECORD_TAKE: &str = "record_take";
pub const IMPORT_AUDIO_CLIP: &str = "import_audio_clip";
pub const RUN_SIDECAR_JOB: &str = "run_sidecar_job";

/// Complete tool roster. Policy denies anything not listed here.
pub const ALL_TOOLS: &[&str] = &[
    GET_PROJECT_STATE,
    READ_METERS,
    GET_JOB_STATUS,
    CREATE_PROJECT,
    ADD_TRACK,
    SET_TRACK_MIX,
    TRANSPORT_CONTROL,
    RECORD_TAKE,
    IMPORT_AUDIO_CLIP,
    RUN_SIDECAR_JOB,
];

/// Tools that never mutate state nor spawn work — callable even in
/// `readOnly` policy mode.
pub const READ_ONLY_TOOLS: &[&str] = &[GET_PROJECT_STATE, READ_METERS, GET_JOB_STATUS];
