//! The MCP `ServerHandler`: the 10 frozen AURA tools as thin wrappers over
//! [`crate::control::ControlPlane`] (never the Tauri IPC layer).
//!
//! Every call — read-only included — goes through the policy engine first
//! (`McpPolicy::decide`); `Confirm` parks the call on the pending-confirmation
//! registry until `mcp_confirm_pending` resolves it or the timeout denies it
//! (ARCHITECTURE §12.3).
//!
//! Param structs are serde mirrors of the wire shapes in `docs/ipc-schemas/`
//! (camelCase, unknown fields ignored); the corresponding `control` types
//! don't derive `JsonSchema` (frozen files), so the mirrors here carry the
//! schema derivation and map 1:1 onto the control-plane types.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::Duration;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ErrorCode, ErrorData, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::json;

use super::policy::Decision;
use super::tools;
use super::{McpShared, PendingConfirmation};
use crate::control::{ControlPlane, ImportClipRequest, TransportAction};
use crate::control::ops::TrackMixChange;

// ---------------------------------------------------------------------------
// Tool parameter mirrors (wire shapes per docs/ipc-schemas/, camelCase)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetJobStatusParams {
    /// Job id returned by run_sidecar_job. Omit to list all known jobs.
    #[serde(default)]
    pub job_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateProjectParams {
    /// Absolute path of the directory the project folder is created in.
    pub parent_dir: String,
    /// Project name (also the folder name).
    pub name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AddTrackParams {
    /// Track name; defaults to "Track N".
    #[serde(default)]
    pub name: Option<String>,
    /// "audio" (default) or "midi".
    #[serde(default)]
    pub kind: Option<String>,
}

/// One batched mix change; omitted fields stay unchanged
/// (mirror of track-state.schema.json / control::ops::TrackMixChange).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MixChangeParams {
    pub track_id: String,
    /// Gain in dB, clamped to [-160, 24].
    #[serde(default)]
    pub gain_db: Option<f64>,
    /// Stereo pan, clamped to [-1, 1].
    #[serde(default)]
    pub pan: Option<f64>,
    #[serde(default)]
    pub muted: Option<bool>,
    #[serde(default)]
    pub soloed: Option<bool>,
    #[serde(default)]
    pub armed: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SetTrackMixParams {
    /// Applied atomically: one unknown track id fails the whole batch.
    pub changes: Vec<MixChangeParams>,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TransportVerb {
    Play,
    Stop,
    Seek,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TransportControlParams {
    /// "play", "stop" (stopping while recording finalizes the take), or "seek".
    pub action: TransportVerb,
    /// Target position in samples — required for "seek".
    #[serde(default)]
    pub position_samples: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RecordVerb {
    Start,
    Stop,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecordTakeParams {
    /// "start" begins recording, "stop" finalizes the take and returns clips.
    pub action: RecordVerb,
    /// Tracks to record on "start"; defaults to the armed tracks.
    #[serde(default)]
    pub track_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportAudioClipParams {
    /// Absolute path to a wav/flac file, copied into the project.
    pub path: String,
    /// Target track; omit to create a new audio track named after the file.
    #[serde(default)]
    pub track_id: Option<String>,
    /// Timeline placement in samples; defaults to 0.
    #[serde(default)]
    pub at_samples: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunSidecarJobParams {
    /// Job kind wire string: aceStepGenerate, aceStepRepaint,
    /// aceStepAudio2Audio, elevenLabsMusic, amtInfill or stableAudioSfz.
    pub kind: String,
    /// Kind-specific params object (sidecar-job-v2.schema.json).
    #[serde(default)]
    pub params: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// One handler instance serves one MCP session; internals are shared Arcs.
#[derive(Clone)]
pub struct AuraMcpHandler {
    control: Arc<ControlPlane>,
    shared: Arc<McpShared>,
    tool_router: ToolRouter<Self>,
}

impl AuraMcpHandler {
    pub fn new(control: Arc<ControlPlane>, shared: Arc<McpShared>) -> Self {
        Self { control, shared, tool_router: Self::tool_router() }
    }

    /// Policy gate, run before EVERY tool body. `Confirm` parks the call on
    /// the pending registry (resolved by the `mcp_confirm_pending` command /
    /// UI dialog) and times out to deny.
    async fn gate(&self, tool: &str, summary: String) -> Result<(), ErrorData> {
        let decision = self.shared.policy.lock().decide(tool);
        match decision {
            Decision::Allow => Ok(()),
            Decision::Deny => Err(ErrorData::new(
                ErrorCode::INVALID_REQUEST,
                format!("tool '{tool}' is denied by the AURA MCP policy"),
                Some(json!({ "reason": "policy" })),
            )),
            Decision::Confirm => {
                let pending = PendingConfirmation {
                    id: uuid::Uuid::new_v4().to_string(),
                    tool: tool.to_string(),
                    summary,
                    requested_at: chrono::Utc::now().to_rfc3339(),
                };
                let id = pending.id.clone();
                let (tx, rx) = tokio::sync::oneshot::channel();
                self.shared.pending.lock().insert(id.clone(), pending.clone());
                self.shared.waiters.lock().insert(id.clone(), tx);
                if let Some(emit) = self.shared.confirm_emit.lock().as_ref() {
                    emit(&pending);
                }
                let timeout =
                    Duration::from_millis(self.shared.confirm_timeout_ms.load(Relaxed));
                let outcome = tokio::time::timeout(timeout, rx).await;
                self.shared.pending.lock().remove(&id);
                self.shared.waiters.lock().remove(&id);
                match outcome {
                    Ok(Ok(true)) => Ok(()),
                    Ok(Ok(false)) => Err(ErrorData::new(
                        ErrorCode::INVALID_REQUEST,
                        format!("tool '{tool}' was denied by the user"),
                        Some(json!({ "reason": "userDenied", "confirmationId": id })),
                    )),
                    Ok(Err(_)) => Err(ErrorData::new(
                        ErrorCode::INTERNAL_ERROR,
                        format!("confirmation for tool '{tool}' was dropped"),
                        None,
                    )),
                    Err(_) => Err(ErrorData::new(
                        ErrorCode::INVALID_REQUEST,
                        format!(
                            "tool '{tool}' timed out awaiting user confirmation ({}s)",
                            timeout.as_secs_f64()
                        ),
                        Some(json!({ "reason": "confirmTimeout", "confirmationId": id })),
                    )),
                }
            }
        }
    }

    /// Run a (possibly blocking) control-plane call off the async runtime.
    async fn blocking<T, F>(&self, f: F) -> Result<Result<T, String>, ErrorData>
    where
        T: Send + 'static,
        F: FnOnce(Arc<ControlPlane>) -> Result<T, String> + Send + 'static,
    {
        let control = self.control.clone();
        tokio::task::spawn_blocking(move || f(control)).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("task join error: {e}"), None)
        })
    }
}

/// Successful structured result (with text fallback, added by rmcp).
fn ok_json<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let v = serde_json::to_value(value).map_err(|e| {
        ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("serialize result: {e}"), None)
    })?;
    Ok(CallToolResult::structured(v))
}

/// Tool-level failure (`isError: true`) — the agent sees the message.
fn tool_err(msg: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg.into())]))
}

fn from_control<T: serde::Serialize>(res: Result<T, String>) -> Result<CallToolResult, ErrorData> {
    match res {
        Ok(v) => ok_json(&v),
        Err(e) => tool_err(e),
    }
}

#[tool_router]
impl AuraMcpHandler {
    #[tool(
        name = "get_project_state",
        description = "Full AURA project snapshot: transport, tracks, audio clips, MIDI clips, tempo map.",
        annotations(
            title = "Get project state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_project_state(&self) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::GET_PROJECT_STATE, "Read the full project state".into()).await?;
        ok_json(&self.control.project_state())
    }

    #[tool(
        name = "read_meters",
        description = "Latest ~60 Hz peak/RMS meter frame per track + master — the agent's ears. `frame` is null until the engine has produced one.",
        annotations(
            title = "Read meters",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn read_meters(&self) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::READ_METERS, "Read the current audio meters".into()).await?;
        ok_json(&json!({ "frame": self.control.read_meters() }))
    }

    #[tool(
        name = "get_job_status",
        description = "Status of one sidecar job (jobId) or all known jobs (no args). Poll this after run_sidecar_job.",
        annotations(
            title = "Get job status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_job_status(
        &self,
        Parameters(p): Parameters<GetJobStatusParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::GET_JOB_STATUS, "Read sidecar job status".into()).await?;
        match p.job_id {
            Some(id) => from_control(self.control.job_status(&id).map(|job| json!({ "job": job }))),
            None => ok_json(&json!({ "jobs": self.control.list_jobs() })),
        }
    }

    #[tool(
        name = "create_project",
        description = "Create a new .aura project directory and switch to it. Clears the current clip list.",
        annotations(
            title = "Create project",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_project(
        &self,
        Parameters(p): Parameters<CreateProjectParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.gate(
            tools::CREATE_PROJECT,
            format!("Create project '{}' in {}", p.name, p.parent_dir),
        )
        .await?;
        let res = self
            .blocking(move |c| c.create_project(&p.parent_dir, &p.name))
            .await?;
        from_control(res)
    }

    #[tool(
        name = "add_track",
        description = "Add a track ('audio' or 'midi') and return its state.",
        annotations(
            title = "Add track",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn add_track(
        &self,
        Parameters(p): Parameters<AddTrackParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let label = p.name.clone().unwrap_or_else(|| "(auto-named)".into());
        let kind = p.kind.clone().unwrap_or_else(|| "audio".into());
        self.gate(tools::ADD_TRACK, format!("Add {kind} track {label}")).await?;
        let res = self.blocking(move |c| c.add_track(p.name, p.kind)).await?;
        from_control(res)
    }

    #[tool(
        name = "set_track_mix",
        description = "Batched mixer changes (gainDb/pan/muted/soloed/armed per track), applied atomically. Returns the updated tracks.",
        annotations(
            title = "Set track mix",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_track_mix(
        &self,
        Parameters(p): Parameters<SetTrackMixParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::SET_TRACK_MIX, format!("Change mix on {} track(s)", p.changes.len()))
            .await?;
        let changes: Vec<TrackMixChange> = p
            .changes
            .into_iter()
            .map(|c| TrackMixChange {
                track_id: c.track_id,
                gain_db: c.gain_db,
                pan: c.pan,
                muted: c.muted,
                soloed: c.soloed,
                armed: c.armed,
            })
            .collect();
        let res = self.blocking(move |c| c.set_track_mix(changes)).await?;
        from_control(res.map(|tracks| json!({ "tracks": tracks })))
    }

    #[tool(
        name = "transport_control",
        description = "Drive the transport: play, stop, or seek (positionSamples). Returns the transport state.",
        annotations(
            title = "Transport control",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn transport_control(
        &self,
        Parameters(p): Parameters<TransportControlParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let action = match p.action {
            TransportVerb::Play => TransportAction::Play,
            TransportVerb::Stop => TransportAction::Stop,
            TransportVerb::Seek => {
                let position_samples = p.position_samples.ok_or_else(|| {
                    ErrorData::new(
                        ErrorCode::INVALID_PARAMS,
                        "action 'seek' requires positionSamples",
                        None,
                    )
                })?;
                TransportAction::Seek { position_samples }
            }
        };
        let summary = match &action {
            TransportAction::Play => "Transport: play".to_string(),
            TransportAction::Stop => "Transport: stop".to_string(),
            TransportAction::Seek { position_samples } => {
                format!("Transport: seek to sample {position_samples}")
            }
            // Not reachable from the frozen MCP verb roster; loop state is
            // visible to agents through get_project_state.
            TransportAction::SetLoop { enabled, start_samples, end_samples } => format!(
                "Transport: loop {} [{start_samples}, {end_samples})",
                if *enabled { "on" } else { "off" }
            ),
            TransportAction::SetStopAtEnd { enabled } => format!(
                "Transport: stop-at-end {}",
                if *enabled { "on" } else { "off" }
            ),
        };
        self.gate(tools::TRANSPORT_CONTROL, summary).await?;
        let res = self.blocking(move |c| c.transport(action)).await?;
        from_control(res)
    }

    #[tool(
        name = "record_take",
        description = "action 'start' begins recording (optionally on trackIds), 'stop' finalizes the take and returns the recorded clips.",
        annotations(
            title = "Record take",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn record_take(
        &self,
        Parameters(p): Parameters<RecordTakeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        match p.action {
            RecordVerb::Start => {
                self.gate(tools::RECORD_TAKE, "Start recording".into()).await?;
                let ids = p.track_ids;
                let res = self.blocking(move |c| c.start_recording(ids)).await?;
                from_control(res)
            }
            RecordVerb::Stop => {
                self.gate(tools::RECORD_TAKE, "Stop recording and keep the take".into())
                    .await?;
                let res = self.blocking(move |c| c.stop_recording()).await?;
                from_control(res.map(|clips| json!({ "clips": clips })))
            }
        }
    }

    #[tool(
        name = "import_audio_clip",
        description = "Import a wav/flac file as a clip: copied into the project, placed at atSamples (default 0); omit trackId to create a track.",
        annotations(
            title = "Import audio clip",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn import_audio_clip(
        &self,
        Parameters(p): Parameters<ImportAudioClipParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::IMPORT_AUDIO_CLIP, format!("Import audio file {}", p.path))
            .await?;
        let req = ImportClipRequest {
            path: p.path,
            track_id: p.track_id,
            at_samples: p.at_samples,
        };
        let res = self.blocking(move |c| c.import_audio_clip(req)).await?;
        from_control(res)
    }

    #[tool(
        name = "run_sidecar_job",
        description = "Start a generation/analysis sidecar job (aceStepGenerate, aceStepRepaint, aceStepAudio2Audio, elevenLabsMusic, amtInfill, stableAudioSfz). Returns { jobId }; poll get_job_status.",
        annotations(
            title = "Run sidecar job",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = true
        )
    )]
    async fn run_sidecar_job(
        &self,
        Parameters(p): Parameters<RunSidecarJobParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.gate(tools::RUN_SIDECAR_JOB, format!("Run sidecar job '{}'", p.kind))
            .await?;
        let params = if p.params.is_null() { json!({}) } else { p.params };
        // No per-job Tauri channel for MCP callers (agents poll
        // get_job_status), but the UI still needs to SEE agent-launched jobs:
        // fan progress/done/error out as the standard `sidecar://*` app
        // events, exactly like UI-launched jobs (JOBS indicator parity).
        let sink: crate::sidecars::jobs::EventSink = self.control.app_event_sink();
        let res = self
            .blocking(move |c| c.run_sidecar_job(&p.kind, params, sink))
            .await?;
        from_control(res.map(|job_id| json!({ "jobId": job_id })))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AuraMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "AURA digital audio workstation control surface. Read-only tools \
             (get_project_state, read_meters, get_job_status) observe the session; \
             the remaining tools mutate the project and may require the user to \
             confirm each call in the AURA UI, depending on the active policy. \
             read_meters is your ears: poll it while the transport plays.",
        )
    }
}
