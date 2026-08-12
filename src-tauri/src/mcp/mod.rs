//! AURA MCP module — OWNED BY THE MCP AGENT (phase 2, zone A).
//! See ARCHITECTURE §12.
//!
//! Layout:
//! * [`tools`]   — the frozen curated tool roster + read-only classification.
//! * [`policy`]  — policy engine (readOnly / confirmDestructive / full,
//!                 per-tool overrides). Wire: mcp-policy.schema.json.
//! * [`auth`]    — session token + Origin validation (loopback only).
//! * [`http1`]   — minimal HTTP/1.1 layer (frozen Cargo.toml has no HTTP
//!                 server crate; see the module header for the rationale).
//! * [`handler`] — the rmcp `ServerHandler`: 10 tools over `ControlPlane`,
//!                 policy-gated, confirm flow.
//! * [`server`]  — the embedded streamable-HTTP server (sessions, lifecycle).
//! * this file   — managed [`McpState`] + `#[tauri::command]` glue
//!                 (`mcp_get_status`, `mcp_set_policy`, `mcp_confirm_pending`).

pub mod auth;
pub mod handler;
pub mod http1;
pub mod policy;
pub mod server;
pub mod tools;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::State;

pub use policy::{Decision, McpPolicy, PolicyMode};

/// One tool call parked awaiting user confirmation (policy said Confirm).
/// Wire: mcp-policy.schema.json#/$defs/pendingConfirmation — also the payload
/// of the `mcp://confirm-requested` app event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingConfirmation {
    pub id: String,
    pub tool: String,
    pub summary: String,
    pub requested_at: String,
}

/// Emits the `mcp://confirm-requested` app event (installed by [`init`];
/// tests install their own capture/auto-approve closure).
pub type ConfirmEmitter = Box<dyn Fn(&PendingConfirmation) + Send + Sync>;

/// Default confirmation timeout (ARCHITECTURE §12.3: timeout ⇒ deny).
pub const CONFIRM_TIMEOUT_MS: u64 = 60_000;

/// Shared MCP state (also handed to the server task, hence the split from
/// the Tauri-managed wrapper).
pub struct McpShared {
    pub token: String,
    pub policy: Mutex<McpPolicy>,
    pub running: AtomicBool,
    /// Confirmations awaiting the user, surfaced via `mcp_get_status`.
    pub pending: Mutex<HashMap<String, PendingConfirmation>>,
    /// Wakers for the parked tool calls (id-parallel with `pending`).
    pub waiters: Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    /// `mcp://confirm-requested` emitter; None until [`init`] installs it.
    pub confirm_emit: Mutex<Option<ConfirmEmitter>>,
    /// Confirm-flow timeout in ms (tests shrink it).
    pub confirm_timeout_ms: AtomicU64,
    /// Actual bound port while running (0 otherwise) — policy.port may be 0
    /// (ephemeral) or apply only on next start.
    pub bound_port: AtomicU16,
}

impl McpShared {
    /// Resolve a parked call: approve ⇒ it runs, else it is refused. Shared
    /// by the `mcp_confirm_pending` command and tests.
    pub fn resolve_pending(&self, confirmation_id: &str, approve: bool) -> Result<(), String> {
        let removed = self.pending.lock().remove(confirmation_id);
        let waiter = self.waiters.lock().remove(confirmation_id);
        if removed.is_none() && waiter.is_none() {
            return Err(format!("unknown confirmation: {confirmation_id}"));
        }
        if let Some(tx) = waiter {
            // Receiver may already have timed out; that resolved to deny.
            let _ = tx.send(approve);
        }
        Ok(())
    }
}

/// Managed by lib.rs; relies only on the type name + `Default`.
pub struct McpState {
    pub shared: Arc<McpShared>,
    /// Running server handle (None = not started / failed to bind).
    pub server: Mutex<Option<server::ServerHandle>>,
}

impl Default for McpState {
    fn default() -> Self {
        Self {
            shared: Arc::new(McpShared {
                token: auth::generate_token(),
                policy: Mutex::new(McpPolicy::default()),
                running: AtomicBool::new(false),
                pending: Mutex::new(HashMap::new()),
                waiters: Mutex::new(HashMap::new()),
                confirm_emit: Mutex::new(None),
                confirm_timeout_ms: AtomicU64::new(CONFIRM_TIMEOUT_MS),
                bound_port: AtomicU16::new(0),
            }),
            server: Mutex::new(None),
        }
    }
}

/// Module init hook, called once from Tauri's `setup` (lib.rs), AFTER the
/// `Arc<ControlPlane>` is managed. Installs the confirm-event emitter, writes
/// the session token (0600) for local clients, and starts the server. A bind
/// failure disables MCP for the session but never aborts app startup.
pub fn init(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    use tauri::{Emitter, Manager};
    let state = app.state::<McpState>();
    let shared = state.shared.clone();
    let control = app.state::<Arc<crate::control::ControlPlane>>().inner().clone();

    let emit_handle = app.clone();
    *shared.confirm_emit.lock() = Some(Box::new(move |pending| {
        match serde_json::to_value(pending) {
            Ok(payload) => {
                if let Err(e) = emit_handle.emit("mcp://confirm-requested", payload) {
                    log::warn!("mcp: emit confirm-requested: {e}");
                }
            }
            Err(e) => log::warn!("mcp: serialize confirm-requested: {e}"),
        }
    }));

    match auth::write_token_file(&shared.token) {
        Ok(path) => log::info!(
            "mcp: session token {}… (full token written 0600 to {})",
            auth::token_fingerprint(&shared.token),
            path.display()
        ),
        Err(e) => log::warn!("mcp: could not write token file: {e}"),
    }

    match server::start(control, shared.clone()) {
        Ok((handle, accept_loop)) => {
            log::info!(
                "mcp: streamable-HTTP server on http://127.0.0.1:{}/mcp",
                handle.port()
            );
            *state.server.lock() = Some(handle);
            tauri::async_runtime::spawn(accept_loop);
        }
        Err(e) => log::error!("mcp: server failed to start: {e} (MCP disabled this session)"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands (names frozen, registered in lib.rs)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub running: bool,
    pub port: u16,
    /// First 8 chars of the session token (full token is never sent to JS;
    /// local clients read it from the 0600 token file instead).
    pub token_fingerprint: String,
    pub policy: McpPolicy,
    pub pending: Vec<PendingConfirmation>,
}

#[tauri::command]
pub fn mcp_get_status(state: State<'_, McpState>) -> Result<McpStatus, String> {
    let s = &state.shared;
    let running = s.running.load(Relaxed);
    let bound = s.bound_port.load(Relaxed);
    Ok(McpStatus {
        running,
        port: if running && bound != 0 { bound } else { s.policy.lock().port },
        token_fingerprint: auth::token_fingerprint(&s.token),
        policy: s.policy.lock().clone(),
        pending: s.pending.lock().values().cloned().collect(),
    })
}

/// Replace the policy (port changes take effect on next server start).
#[tauri::command]
pub fn mcp_set_policy(policy: McpPolicy, state: State<'_, McpState>) -> Result<McpPolicy, String> {
    *state.shared.policy.lock() = policy.clone();
    Ok(policy)
}

/// Resolve a parked destructive tool call (approve = run it, else refuse).
#[tauri::command]
pub fn mcp_confirm_pending(
    confirmation_id: String,
    approve: bool,
    state: State<'_, McpState>,
) -> Result<(), String> {
    state.shared.resolve_pending(&confirmation_id, approve)
}
