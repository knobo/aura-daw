//! Embedded MCP streamable-HTTP server (phase 2, zone A).
//!
//! Architecture (see also `http1.rs` for why rmcp's tower layer is not used
//! directly — `Cargo.toml` is frozen without an HTTP server crate):
//!
//! * TCP accept loop bound to **127.0.0.1 only** (`policy.port`, default
//!   41717; `0` = ephemeral, used by tests).
//! * Every request: Origin validation ([`super::auth::origin_allowed`]) and
//!   constant-time bearer-token check ([`super::auth::token_matches`])
//!   BEFORE any dispatch (ARCHITECTURE §12.3).
//! * MCP semantics come from rmcp's tested service core: each MCP session is
//!   one `rmcp::serve_server(AuraMcpHandler, …)` task attached to an
//!   in-process duplex pipe (newline-delimited JSON-RPC, rmcp's AsyncRw
//!   transport). The HTTP layer ferries streamable-HTTP POST bodies into the
//!   pipe and routes responses back by JSON-RPC id (`Mcp-Session-Id` header
//!   per session, `202 Accepted` for notifications, `DELETE` terminates a
//!   session — per the MCP streamable-HTTP spec; the optional standalone GET
//!   SSE stream is not offered, which the spec allows via 405).
//! * Tool policy/confirm gating lives in the handler (`handler.rs`), not
//!   here: transport auth first, then policy, then the control plane.

use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore};

use super::handler::AuraMcpHandler;
use super::{auth, http1, McpShared};
use crate::control::ControlPlane;

/// Duplex pipe capacity between the HTTP shim and an rmcp session task.
const PIPE_CAPACITY: usize = 256 * 1024;
/// Longest JSON-RPC line accepted from the rmcp side (project snapshots).
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Max concurrent MCP sessions.
const MAX_SESSIONS: usize = 64;
/// Sessions idle longer than this are reaped.
const IDLE_SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long the `initialize` round-trip may take.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a tool call may take end-to-end (covers the 60 s confirm flow).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Max concurrently accepted TCP connections (slow-loris/socket-exhaustion
/// cap; connections beyond it are dropped at accept).
const MAX_CONNECTIONS: usize = 128;
/// How long reading one request (head + body) may take. Also reaps idle
/// keep-alive connections — clients simply reconnect.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Transport-level DoS limits (constants in production, overridable by tests).
#[derive(Clone, Copy)]
struct ConnLimits {
    max_connections: usize,
    read_timeout: Duration,
}

impl Default for ConnLimits {
    fn default() -> Self {
        Self { max_connections: MAX_CONNECTIONS, read_timeout: READ_TIMEOUT }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// One MCP session: an rmcp server task on the far side of a duplex pipe.
struct Session {
    /// Client→server direction of the pipe (JSON-RPC lines).
    write: tokio::sync::Mutex<WriteHalf<DuplexStream>>,
    /// In-flight request id → waiter for the matching response.
    waiters: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    last_used: Mutex<Instant>,
}

impl Session {
    async fn send_line(&self, v: &Value) -> Result<(), String> {
        let mut line = v.to_string().into_bytes();
        line.push(b'\n');
        let mut w = self.write.lock().await;
        w.write_all(&line).await.map_err(|e| format!("session pipe: {e}"))?;
        w.flush().await.map_err(|e| format!("session pipe: {e}"))
    }

    /// Close the client→server pipe: the rmcp task sees EOF and quits, which
    /// closes the other direction and ends the reader task.
    async fn close(&self) {
        let _ = self.write.lock().await.shutdown().await;
    }

    fn touch(&self) {
        *self.last_used.lock() = Instant::now();
    }
}

struct Ctx {
    control: Arc<ControlPlane>,
    shared: Arc<McpShared>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    limits: ConnLimits,
}

/// Spawn the rmcp service + response-reader tasks for a new session.
fn create_session(ctx: &Arc<Ctx>) -> Result<(String, Arc<Session>), http1::Response> {
    let mut sessions = ctx.sessions.lock();
    if sessions.len() >= MAX_SESSIONS {
        return Err(http1::Response::text(503, "Service Unavailable", "too many MCP sessions"));
    }
    let (client_io, server_io) = tokio::io::duplex(PIPE_CAPACITY);
    let (read_half, write_half) = tokio::io::split(client_io);
    let session = Arc::new(Session {
        write: tokio::sync::Mutex::new(write_half),
        waiters: Mutex::new(HashMap::new()),
        last_used: Mutex::new(Instant::now()),
    });
    let id = uuid::Uuid::new_v4().to_string();
    sessions.insert(id.clone(), session.clone());
    drop(sessions);

    let handler = AuraMcpHandler::new(ctx.control.clone(), ctx.shared.clone());
    let sid = id.clone();
    tokio::spawn(async move {
        match rmcp::serve_server(handler, server_io).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => log::debug!("mcp: session {sid} ended during init: {e}"),
        }
    });
    tokio::spawn(session_reader(id.clone(), session.clone(), ctx.clone(), read_half));
    Ok((id, session))
}

/// Read server→client JSON-RPC lines and route them: responses wake the
/// matching HTTP waiter; server→client requests get a method-not-found reply
/// (this bridge never advertises client capabilities); notifications drop.
async fn session_reader(
    id: String,
    session: Arc<Session>,
    ctx: Arc<Ctx>,
    read_half: ReadHalf<DuplexStream>,
) {
    let mut reader = BufReader::new(read_half);
    let mut line: Vec<u8> = Vec::with_capacity(1024);
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > MAX_LINE_BYTES => {
                log::warn!("mcp: session {id}: oversized message, closing");
                break;
            }
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
        let has_id = v.get("id").is_some();
        let has_method = v.get("method").is_some();
        if has_method && has_id {
            // Server-initiated request (sampling etc.) — not supported here.
            let reply = json!({
                "jsonrpc": "2.0",
                "id": v["id"],
                "error": { "code": -32601, "message":
                    "the AURA HTTP bridge does not support server-to-client requests" }
            });
            let _ = session.send_line(&reply).await;
        } else if has_id {
            let key = v["id"].to_string();
            let waiter = session.waiters.lock().remove(&key);
            if let Some(tx) = waiter {
                let _ = tx.send(v);
            }
        }
        // Bare notifications (logging/progress) have no HTTP channel: drop.
    }
    ctx.sessions.lock().remove(&id);
    session.waiters.lock().clear(); // wake pending HTTP requests with an error
}

/// Send one JSON-RPC request into the session and await its response.
async fn roundtrip(
    session: &Arc<Session>,
    msg: &Value,
    timeout: Duration,
) -> Result<Value, http1::Response> {
    let key = msg["id"].to_string();
    let (tx, rx) = oneshot::channel();
    {
        use std::collections::hash_map::Entry;
        let mut waiters = session.waiters.lock();
        match waiters.entry(key.clone()) {
            // A request with this id is already in flight on this session;
            // registering a second waiter would silently drop the first.
            // Keep the first and reject the duplicate.
            Entry::Occupied(_) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "id": msg["id"],
                    "error": { "code": -32600, "message":
                        "duplicate in-flight JSON-RPC request id on this session" }
                });
                return Err(http1::Response::json(409, "Conflict", &err));
            }
            Entry::Vacant(e) => {
                e.insert(tx);
            }
        }
    }
    if let Err(e) = session.send_line(msg).await {
        session.waiters.lock().remove(&key);
        return Err(http1::Response::text(502, "Bad Gateway", e));
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(resp)) => Ok(resp),
        Ok(Err(_)) => Err(http1::Response::text(502, "Bad Gateway", "MCP session closed")),
        Err(_) => {
            session.waiters.lock().remove(&key);
            Err(http1::Response::text(504, "Gateway Timeout", "MCP response timed out"))
        }
    }
}

// ---------------------------------------------------------------------------
// HTTP dispatch
// ---------------------------------------------------------------------------

const SESSION_HEADER: &str = "mcp-session-id";

fn bearer(req: &http1::Request) -> Option<&str> {
    let v = req.header("authorization")?;
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn lookup(ctx: &Ctx, req: &http1::Request) -> Result<Arc<Session>, http1::Response> {
    let Some(id) = req.header(SESSION_HEADER) else {
        return Err(http1::Response::text(400, "Bad Request", "missing Mcp-Session-Id header"));
    };
    match ctx.sessions.lock().get(id) {
        Some(s) => {
            s.touch();
            Ok(s.clone())
        }
        None => Err(http1::Response::text(404, "Not Found", "unknown or terminated MCP session")),
    }
}

async fn handle_request(ctx: &Arc<Ctx>, req: http1::Request) -> http1::Response {
    // 1. Origin validation (DNS-rebinding defense) BEFORE anything else.
    if !auth::origin_allowed(req.header("origin")) {
        return http1::Response::text(403, "Forbidden", "Origin not allowed");
    }
    // 2. Constant-time session-token check.
    if !bearer(&req).is_some_and(|t| auth::token_matches(&ctx.shared.token, t)) {
        return http1::Response::text(401, "Unauthorized", "missing or invalid bearer token")
            .with_header("www-authenticate", "Bearer realm=\"aura-mcp\"");
    }
    match req.path() {
        "/" | "/mcp" | "/mcp/" => {}
        _ => return http1::Response::text(404, "Not Found", "MCP endpoint is /mcp"),
    }
    match req.method.as_str() {
        "POST" => handle_post(ctx, &req).await,
        "DELETE" => handle_delete(ctx, &req).await,
        // No standalone SSE stream is offered; the spec allows 405 here.
        _ => http1::Response::text(405, "Method Not Allowed", "use POST (or DELETE)")
            .with_header("allow", "POST, DELETE"),
    }
}

async fn handle_post(ctx: &Arc<Ctx>, req: &http1::Request) -> http1::Response {
    let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
        return http1::Response::text(400, "Bad Request", "body is not valid JSON");
    };
    if !msg.is_object() {
        return http1::Response::text(400, "Bad Request", "JSON-RPC batching is not supported");
    }
    let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
    let is_request = method.is_some() && msg.get("id").is_some();

    match (method.as_deref(), is_request) {
        (Some("initialize"), true) => {
            let (id, session) = match create_session(ctx) {
                Ok(x) => x,
                Err(resp) => return resp,
            };
            match roundtrip(&session, &msg, INIT_TIMEOUT).await {
                Ok(resp) => http1::Response::json(200, "OK", &resp)
                    .with_header(SESSION_HEADER, id.clone()),
                Err(resp) => {
                    session.close().await;
                    ctx.sessions.lock().remove(&id);
                    resp
                }
            }
        }
        (Some(_), true) => {
            let session = match lookup(ctx, req) {
                Ok(s) => s,
                Err(resp) => return resp,
            };
            match roundtrip(&session, &msg, REQUEST_TIMEOUT).await {
                Ok(resp) => http1::Response::json(200, "OK", &resp),
                Err(resp) => resp,
            }
        }
        // Notification or client→server response: forward if a session is
        // named, then 202 Accepted (streamable-HTTP spec).
        _ => {
            if req.header(SESSION_HEADER).is_some() {
                match lookup(ctx, req) {
                    Ok(session) => {
                        if let Err(e) = session.send_line(&msg).await {
                            return http1::Response::text(502, "Bad Gateway", e);
                        }
                    }
                    Err(resp) => return resp,
                }
            }
            http1::Response::new(202, "Accepted")
        }
    }
}

async fn handle_delete(ctx: &Arc<Ctx>, req: &http1::Request) -> http1::Response {
    let Some(id) = req.header(SESSION_HEADER) else {
        return http1::Response::text(400, "Bad Request", "missing Mcp-Session-Id header");
    };
    let removed = ctx.sessions.lock().remove(id);
    match removed {
        Some(session) => {
            session.close().await;
            http1::Response::text(200, "OK", "session terminated")
        }
        None => http1::Response::text(404, "Not Found", "unknown or terminated MCP session"),
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// A started server. Dropping it does NOT stop the server; call [`stop`].
pub struct ServerHandle {
    port: u16,
    stop: watch::Sender<bool>,
    shared: Arc<McpShared>,
}

impl ServerHandle {
    /// Actual bound port (differs from policy.port when that was 0).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stop accepting, close all sessions, clear the running flag. Idempotent.
    pub fn stop(&self) {
        self.shared.running.store(false, Relaxed);
        self.shared.bound_port.store(0, Relaxed);
        let _ = self.stop.send(true);
    }
}

/// Bind 127.0.0.1:`policy.port` and return the handle plus the accept-loop
/// future. The CALLER spawns the future (Tauri: `tauri::async_runtime::spawn`
/// from `mcp::init`; tests: `tokio::spawn`) — this keeps the module
/// tauri-free. Restart = `handle.stop()` then `start` again.
pub fn start(
    control: Arc<ControlPlane>,
    shared: Arc<McpShared>,
) -> Result<(ServerHandle, impl std::future::Future<Output = ()> + Send + 'static), String> {
    start_with(control, shared, ConnLimits::default())
}

/// [`start`] with explicit transport limits (tests use short timeouts).
fn start_with(
    control: Arc<ControlPlane>,
    shared: Arc<McpShared>,
    limits: ConnLimits,
) -> Result<(ServerHandle, impl std::future::Future<Output = ()> + Send + 'static), String> {
    let port = shared.policy.lock().port;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking: {e}"))?;
    let bound = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();
    let (stop_tx, stop_rx) = watch::channel(false);
    shared.running.store(true, Relaxed);
    shared.bound_port.store(bound, Relaxed);
    let ctx = Arc::new(Ctx {
        control,
        shared: shared.clone(),
        sessions: Mutex::new(HashMap::new()),
        limits,
    });
    let fut = run(listener, ctx, stop_rx);
    Ok((ServerHandle { port: bound, stop: stop_tx, shared }, fut))
}

async fn run(listener: std::net::TcpListener, ctx: Arc<Ctx>, mut stop: watch::Receiver<bool>) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(l) => l,
        Err(e) => {
            log::error!("mcp: listener registration failed: {e}");
            ctx.shared.running.store(false, Relaxed);
            ctx.shared.bound_port.store(0, Relaxed);
            return;
        }
    };
    let mut sweep = tokio::time::interval(Duration::from_secs(60));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let conn_slots = Arc::new(Semaphore::new(ctx.limits.max_connections));
    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    break;
                }
            }
            _ = sweep.tick() => sweep_idle_sessions(&ctx).await,
            accepted = listener.accept() => match accepted {
                Ok((stream, _peer)) => match conn_slots.clone().try_acquire_owned() {
                    Ok(permit) => {
                        let _ = stream.set_nodelay(true);
                        tokio::spawn(serve_conn(stream, ctx.clone(), stop.clone(), permit));
                    }
                    // Over the connection cap: shed load immediately without
                    // reading (the socket may itself be a slow-loris).
                    Err(_) => {
                        log::warn!(
                            "mcp: connection limit ({}) reached, dropping connection",
                            ctx.limits.max_connections
                        );
                        drop(stream);
                    }
                },
                Err(e) => {
                    log::warn!("mcp: accept error: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }
    let sessions: Vec<Arc<Session>> = ctx.sessions.lock().drain().map(|(_, s)| s).collect();
    for s in sessions {
        s.close().await;
    }
    // stop() already cleared the flags; also clear here for the error path.
    if *stop.borrow() {
        ctx.shared.running.store(false, Relaxed);
    }
}

async fn sweep_idle_sessions(ctx: &Arc<Ctx>) {
    let now = Instant::now();
    let expired: Vec<(String, Arc<Session>)> = {
        let mut sessions = ctx.sessions.lock();
        let ids: Vec<String> = sessions
            .iter()
            .filter(|(_, s)| now.duration_since(*s.last_used.lock()) > IDLE_SESSION_TIMEOUT)
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| sessions.remove(&id).map(|s| (id, s)))
            .collect()
    };
    for (id, s) in expired {
        log::info!("mcp: reaping idle session {id}");
        s.close().await;
    }
}

async fn serve_conn(
    stream: tokio::net::TcpStream,
    ctx: Arc<Ctx>,
    mut stop: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        if *stop.borrow() {
            break;
        }
        let outcome = tokio::select! {
            _ = stop.changed() => break,
            r = tokio::time::timeout(
                ctx.limits.read_timeout,
                http1::read_request(&mut reader),
            ) => match r {
                Ok(r) => r,
                // Slow-loris (or idle keep-alive): 408 and close.
                Err(_elapsed) => {
                    let resp =
                        http1::Response::text(408, "Request Timeout", "timed out reading request");
                    let _ = resp.write(&mut write_half, true).await;
                    break;
                }
            },
        };
        match outcome {
            Ok(None) => break, // client closed a keep-alive connection
            Ok(Some(req)) => {
                let close = req.wants_close();
                let resp = handle_request(&ctx, req).await;
                if resp.write(&mut write_half, close).await.is_err() || close {
                    break;
                }
            }
            Err(pe) => {
                let resp = http1::Response::text(pe.status, reason(pe.status), pe.message);
                let _ = resp.write(&mut write_half, true).await;
                break;
            }
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        505 => "HTTP Version Not Supported",
        _ => "Error",
    }
}

// ---------------------------------------------------------------------------
// Tests: end-to-end over real HTTP against an in-process ControlPlane
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::policy::{McpPolicy, PolicyMode, ToolRule};
    use crate::mcp::tools;
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64};
    use tokio::io::AsyncReadExt;

    // -- in-process test ControlPlane (headless engine, no Tauri) -----------

    struct NullSink;
    impl crate::audio::engine::EventSink for NullSink {
        fn emit(&self, _event: &str, _payload: serde_json::Value) {}
    }

    fn test_control() -> Arc<ControlPlane> {
        let session = Arc::new(Mutex::new(crate::control::Session::new(
            crate::audio::types::Store::default(),
            crate::midi::MidiStore::default(),
        )));
        let shared_rt = Arc::new(crate::audio::rt::SharedRt::default());
        // A fresh, empty SharedGraphTables (gen 0, no tracks) — must be
        // handed to BOTH `engine::start` and `ControlPlane::new` as the
        // SAME `Arc` (round-2 §2.4).
        let tables = crate::audio::rt::testutil::empty_tables();
        let gesture = Arc::new(crate::control::GestureState::new());
        let engine = crate::audio::engine::start(
            shared_rt.clone(),
            tables.clone(),
            session.clone(),
            Box::new(NullSink),
            crate::control::testutil::test_committer(&session, &shared_rt, &tables),
            gesture.clone(),
        );
        let jobs = Arc::new(crate::sidecars::jobs::JobManager::new(
            2,
            Duration::from_millis(50),
        ));
        Arc::new(ControlPlane::new(
            session,
            shared_rt,
            tables,
            engine,
            jobs,
            Box::new(|_, _| {}),
            std::sync::Arc::new(crate::control::HistoryLog::new()),
            gesture.clone(),
        ))
    }

    fn test_shared(mode: PolicyMode) -> Arc<McpShared> {
        Arc::new(McpShared {
            token: auth::generate_token(),
            policy: Mutex::new(McpPolicy { mode, tool_overrides: HashMap::new(), port: 0 }),
            running: AtomicBool::new(false),
            pending: Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
            confirm_emit: Mutex::new(None),
            confirm_timeout_ms: AtomicU64::new(60_000),
            bound_port: AtomicU16::new(0),
        })
    }

    struct TestServer {
        handle: ServerHandle,
        shared: Arc<McpShared>,
        #[allow(dead_code)]
        control: Arc<ControlPlane>,
        next_id: std::sync::atomic::AtomicU32,
    }

    impl TestServer {
        fn start(mode: PolicyMode) -> Self {
            let control = test_control();
            let shared = test_shared(mode);
            let (handle, fut) = start(control.clone(), shared.clone()).expect("bind");
            tokio::spawn(fut);
            Self { handle, shared, control, next_id: 1.into() }
        }

        fn id(&self) -> u32 {
            self.next_id.fetch_add(1, Relaxed)
        }

        /// Raw HTTP/1.1 request; `connection: close` so we can read to EOF.
        async fn raw(
            &self,
            method: &str,
            path: &str,
            headers: &[(&str, &str)],
            body: &str,
        ) -> (u16, Vec<(String, String)>, String) {
            let mut s = tokio::net::TcpStream::connect(("127.0.0.1", self.handle.port()))
                .await
                .expect("connect");
            let mut head = format!(
                "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (n, v) in headers {
                head.push_str(&format!("{n}: {v}\r\n"));
            }
            head.push_str("\r\n");
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body.as_bytes()).await.unwrap();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf).to_string();
            let (head, body) = text.split_once("\r\n\r\n").expect("response head");
            let mut lines = head.lines();
            let status: u16 = lines
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|c| c.parse().ok())
                .expect("status");
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            (status, headers, body.to_string())
        }

        /// Authenticated POST of one JSON-RPC message.
        async fn post(
            &self,
            session: Option<&str>,
            msg: &Value,
        ) -> (u16, Vec<(String, String)>, String) {
            let auth = format!("Bearer {}", self.shared.token);
            let mut headers: Vec<(&str, &str)> =
                vec![("authorization", &auth), ("content-type", "application/json")];
            if let Some(sid) = session {
                headers.push((SESSION_HEADER, sid));
            }
            self.raw("POST", "/mcp", &headers, &msg.to_string()).await
        }

        /// initialize + notifications/initialized; returns the session id.
        async fn init_session(&self) -> String {
            let init = json!({
                "jsonrpc": "2.0", "id": self.id(), "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "aura-test", "version": "0.0.0" }
                }
            });
            let (status, headers, body) = self.post(None, &init).await;
            assert_eq!(status, 200, "initialize failed: {body}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert!(v["result"]["capabilities"]["tools"].is_object(), "tools capability: {v}");
            let sid = headers
                .iter()
                .find(|(n, _)| n == SESSION_HEADER)
                .map(|(_, v)| v.clone())
                .expect("session id header");
            let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
            let (status, _, _) = self.post(Some(&sid), &note).await;
            assert_eq!(status, 202);
            sid
        }

        async fn call_tool(&self, sid: &str, tool: &str, args: Value) -> Value {
            let msg = json!({
                "jsonrpc": "2.0", "id": self.id(), "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            });
            let (status, _, body) = self.post(Some(sid), &msg).await;
            assert_eq!(status, 200, "tools/call {tool}: {body}");
            serde_json::from_str(&body).unwrap()
        }
    }

    // -- tests --------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handshake_lists_all_tools_with_hints() {
        let ts = TestServer::start(PolicyMode::ConfirmDestructive);
        let sid = ts.init_session().await;
        let (status, _, body) = ts
            .post(Some(&sid), &json!({ "jsonrpc": "2.0", "id": ts.id(), "method": "tools/list" }))
            .await;
        assert_eq!(status, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        let tools_arr = v["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> =
            tools_arr.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names.len(), tools::ALL_TOOLS.len(), "{names:?}");
        for expected in tools::ALL_TOOLS {
            assert!(names.contains(expected), "missing tool {expected}");
        }
        for t in tools_arr {
            let name = t["name"].as_str().unwrap();
            let read_only = tools::READ_ONLY_TOOLS.contains(&name);
            assert_eq!(
                t["annotations"]["readOnlyHint"].as_bool(),
                Some(read_only),
                "readOnlyHint for {name}"
            );
            if !read_only {
                assert_eq!(
                    t["annotations"]["destructiveHint"].as_bool(),
                    Some(true),
                    "destructiveHint for {name}"
                );
            }
            assert!(t["inputSchema"].is_object(), "inputSchema for {name}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejects_bad_token_and_bad_origin() {
        let ts = TestServer::start(PolicyMode::Full);
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2025-06-18", "capabilities": {},
                        "clientInfo": { "name": "x", "version": "0" } } })
        .to_string();

        // No token.
        let (status, _, _) = ts.raw("POST", "/mcp", &[], &init).await;
        assert_eq!(status, 401);
        // Wrong token.
        let (status, _, _) = ts
            .raw("POST", "/mcp", &[("authorization", "Bearer deadbeef")], &init)
            .await;
        assert_eq!(status, 401);
        // Valid token but hostile Origin (DNS rebinding).
        let auth_hdr = format!("Bearer {}", ts.shared.token);
        let (status, _, _) = ts
            .raw(
                "POST",
                "/mcp",
                &[("authorization", &auth_hdr), ("origin", "http://evil.example.com")],
                &init,
            )
            .await;
        assert_eq!(status, 403);
        // Valid token + loopback Origin works.
        let (status, _, _) = ts
            .raw(
                "POST",
                "/mcp",
                &[("authorization", &auth_hdr), ("origin", "http://localhost:5173")],
                &init,
            )
            .await;
        assert_eq!(status, 200);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_only_mode_allows_reads_denies_mutations() {
        let ts = TestServer::start(PolicyMode::ReadOnly);
        let sid = ts.init_session().await;

        let v = ts.call_tool(&sid, "get_project_state", json!({})).await;
        assert!(
            v["result"]["structuredContent"]["tracks"].is_array(),
            "snapshot: {v}"
        );

        let v = ts.call_tool(&sid, "add_track", json!({ "name": "Nope" })).await;
        let err = v["error"]["message"].as_str().unwrap_or_default();
        assert!(err.contains("denied by the AURA MCP policy"), "{v}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_flow_approves_denies_and_emits_event() {
        let ts = TestServer::start(PolicyMode::ConfirmDestructive);
        let sid = ts.init_session().await;

        // Auto-approver standing in for the UI dialog (mcp_confirm_pending).
        let approve = Arc::new(AtomicBool::new(true));
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let shared = ts.shared.clone();
            let approve = approve.clone();
            let seen = seen.clone();
            *ts.shared.confirm_emit.lock() = Some(Box::new(move |pc| {
                seen.lock().push(pc.tool.clone());
                shared.resolve_pending(&pc.id, approve.load(Relaxed)).unwrap();
            }));
        }

        let v = ts.call_tool(&sid, "add_track", json!({ "name": "Drums" })).await;
        assert_eq!(
            v["result"]["structuredContent"]["name"].as_str(),
            Some("Drums"),
            "approved add_track: {v}"
        );
        assert_eq!(seen.lock().as_slice(), &["add_track".to_string()]);

        approve.store(false, Relaxed);
        let v = ts
            .call_tool(&sid, "transport_control", json!({ "action": "play" }))
            .await;
        let err = v["error"]["message"].as_str().unwrap_or_default();
        assert!(err.contains("denied by the user"), "{v}");
        assert!(ts.shared.pending.lock().is_empty(), "pending registry drained");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn confirm_times_out_to_deny() {
        let ts = TestServer::start(PolicyMode::ConfirmDestructive);
        ts.shared.confirm_timeout_ms.store(150, Relaxed);
        let sid = ts.init_session().await;
        let v = ts.call_tool(&sid, "add_track", json!({})).await;
        let err = v["error"]["message"].as_str().unwrap_or_default();
        assert!(err.contains("timed out awaiting user confirmation"), "{v}");
        assert!(ts.shared.pending.lock().is_empty());
        assert!(ts.shared.waiters.lock().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_mode_mutates_seeks_and_reports_jobs() {
        let ts = TestServer::start(PolicyMode::Full);
        let sid = ts.init_session().await;

        // add_track → set_track_mix on the returned id.
        let v = ts.call_tool(&sid, "add_track", json!({ "name": "Bass" })).await;
        let track_id = v["result"]["structuredContent"]["id"]
            .as_str()
            .expect("track id")
            .to_string();
        let v = ts
            .call_tool(
                &sid,
                "set_track_mix",
                json!({ "changes": [{ "trackId": track_id, "gainDb": -6.5, "pan": 0.25 }] }),
            )
            .await;
        let t = &v["result"]["structuredContent"]["tracks"][0];
        assert_eq!(t["gainDb"].as_f64(), Some(-6.5), "{v}");
        assert_eq!(t["pan"].as_f64(), Some(0.25));

        // seek lands in the project snapshot.
        let v = ts
            .call_tool(
                &sid,
                "transport_control",
                json!({ "action": "seek", "positionSamples": 4800 }),
            )
            .await;
        assert_eq!(v["result"]["structuredContent"]["positionSamples"].as_u64(), Some(4800));
        let v = ts.call_tool(&sid, "get_project_state", json!({})).await;
        assert_eq!(
            v["result"]["structuredContent"]["transport"]["positionSamples"].as_u64(),
            Some(4800)
        );

        // seek without positionSamples → invalid params (protocol error).
        let v = ts
            .call_tool(&sid, "transport_control", json!({ "action": "seek" }))
            .await;
        assert!(v["error"]["message"].as_str().unwrap_or_default().contains("positionSamples"));

        // unknown sidecar kind → tool-level error the agent can read.
        let v = ts
            .call_tool(&sid, "run_sidecar_job", json!({ "kind": "nope", "params": {} }))
            .await;
        assert_eq!(v["result"]["isError"].as_bool(), Some(true), "{v}");
        let text = v["result"]["content"][0]["text"].as_str().unwrap_or_default();
        assert!(text.contains("unknown job kind"), "{v}");

        // job registry is reachable (empty list).
        let v = ts.call_tool(&sid, "get_job_status", json!({})).await;
        assert!(v["result"]["structuredContent"]["jobs"].is_array(), "{v}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policy_override_denies_single_tool_in_full_mode() {
        let ts = TestServer::start(PolicyMode::Full);
        ts.shared
            .policy
            .lock()
            .tool_overrides
            .insert(tools::RECORD_TAKE.into(), ToolRule::Deny);
        let sid = ts.init_session().await;
        let v = ts.call_tool(&sid, "record_take", json!({ "action": "start" })).await;
        assert!(
            v["error"]["message"].as_str().unwrap_or_default().contains("denied"),
            "{v}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_meters_hears_the_headless_engine() {
        let ts = TestServer::start(PolicyMode::ReadOnly);
        let sid = ts.init_session().await;
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let v = ts.call_tool(&sid, "read_meters", json!({})).await;
            let frame = &v["result"]["structuredContent"]["frame"];
            if frame.is_object() {
                assert!(frame["master"].is_object(), "{v}");
                break;
            }
            assert!(Instant::now() < deadline, "no meter frame within 3s: {v}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_lifecycle_and_multiple_sessions() {
        let ts = TestServer::start(PolicyMode::ReadOnly);
        let list = json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/list" });

        // Request without a session → 400; bogus session → 404.
        let (status, _, _) = ts.post(None, &list).await;
        assert_eq!(status, 400);
        let (status, _, _) = ts.post(Some("no-such-session"), &list).await;
        assert_eq!(status, 404);

        // Two independent sessions work concurrently.
        let sid_a = ts.init_session().await;
        let sid_b = ts.init_session().await;
        assert_ne!(sid_a, sid_b);
        let (status, _, _) = ts.post(Some(&sid_a), &list).await;
        assert_eq!(status, 200);
        let (status, _, _) = ts.post(Some(&sid_b), &list).await;
        assert_eq!(status, 200);

        // GET (standalone SSE) is not offered.
        let auth_hdr = format!("Bearer {}", ts.shared.token);
        let (status, _, _) = ts.raw("GET", "/mcp", &[("authorization", &auth_hdr)], "").await;
        assert_eq!(status, 405);

        // DELETE terminates exactly that session.
        let (status, _, _) = ts
            .raw(
                "DELETE",
                "/mcp",
                &[("authorization", &auth_hdr), (SESSION_HEADER, &sid_a)],
                "",
            )
            .await;
        assert_eq!(status, 200);
        let (status, _, _) = ts.post(Some(&sid_a), &list).await;
        assert_eq!(status, 404);
        let (status, _, _) = ts.post(Some(&sid_b), &list).await;
        assert_eq!(status, 200, "other session unaffected");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_loris_request_read_times_out_with_408() {
        let control = test_control();
        let shared = test_shared(PolicyMode::ReadOnly);
        let limits =
            ConnLimits { max_connections: 128, read_timeout: Duration::from_millis(200) };
        let (handle, fut) = start_with(control, shared, limits).unwrap();
        tokio::spawn(fut);

        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", handle.port()))
            .await
            .unwrap();
        // Dribble a partial request head and then stall forever.
        s.write_all(b"POST /mcp HTTP/1.1\r\nauthorization: Bea").await.unwrap();
        let t0 = Instant::now();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 408"), "{text}");
        assert!(t0.elapsed() < Duration::from_secs(5), "connection not reaped promptly");
        handle.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connection_cap_sheds_excess_connections() {
        let control = test_control();
        let shared = test_shared(PolicyMode::ReadOnly);
        let limits = ConnLimits { max_connections: 2, read_timeout: Duration::from_secs(30) };
        let (handle, fut) = start_with(control, shared, limits).unwrap();
        tokio::spawn(fut);
        let port = handle.port();

        // Two held (half-sent) connections occupy both slots.
        let mut c1 = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let mut c2 = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        c1.write_all(b"POST /mcp HTT").await.unwrap();
        c2.write_all(b"POST /mcp HTT").await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Third connection is dropped at accept without a response.
        let mut c3 = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let _ = c3
            .write_all(b"POST /mcp HTTP/1.1\r\ncontent-length: 0\r\n\r\n")
            .await;
        let mut buf = Vec::new();
        let res = c3.read_to_end(&mut buf).await;
        assert!(
            res.is_err() || buf.is_empty(),
            "expected shed connection, got: {}",
            String::from_utf8_lossy(&buf)
        );

        // Closing one connection frees its slot for new clients.
        drop(c1);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut c4 = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        c4.write_all(b"POST /mcp HTTP/1.1\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        c4.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.starts_with("HTTP/1.1 401"), "freed slot should serve again: {text}");
        handle.stop();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn duplicate_in_flight_request_id_is_rejected() {
        let ts = Arc::new(TestServer::start(PolicyMode::ConfirmDestructive));
        ts.shared.confirm_timeout_ms.store(1_000, Relaxed);
        // Confirm emitter that never resolves: the first request stays
        // in flight until the confirm timeout denies it.
        *ts.shared.confirm_emit.lock() = Some(Box::new(|_pc| {}));
        let sid = ts.init_session().await;

        let msg = json!({ "jsonrpc": "2.0", "id": 4242, "method": "tools/call",
            "params": { "name": "add_track", "arguments": {} } });
        let first = {
            let ts = ts.clone();
            let sid = sid.clone();
            let msg = msg.clone();
            tokio::spawn(async move { ts.post(Some(&sid), &msg).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Same id while the first is still in flight → rejected, first kept.
        let (status, _, body) = ts.post(Some(&sid), &msg).await;
        assert_eq!(status, 409, "duplicate id must be rejected: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("duplicate in-flight"),
            "{v}"
        );

        // The first request still completes with its own response.
        let (status, _, body) = first.await.unwrap();
        assert_eq!(status, 200, "first request must survive: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("timed out awaiting user confirmation"),
            "{v}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_and_restart_cycle() {
        let control = test_control();
        let shared = test_shared(PolicyMode::ReadOnly);

        let (h1, fut1) = start(control.clone(), shared.clone()).unwrap();
        tokio::spawn(fut1);
        assert!(shared.running.load(Relaxed));
        assert_eq!(shared.bound_port.load(Relaxed), h1.port());

        h1.stop();
        assert!(!shared.running.load(Relaxed));
        assert_eq!(shared.bound_port.load(Relaxed), 0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", h1.port())).await.is_err(),
            "listener released after stop"
        );

        // Restart (fresh ephemeral port) and complete a full handshake.
        let (h2, fut2) = start(control, shared.clone()).unwrap();
        tokio::spawn(fut2);
        assert!(shared.running.load(Relaxed));
        let ts = TestServer {
            handle: h2,
            shared: shared.clone(),
            control: test_control(),
            next_id: 1.into(),
        };
        let sid = ts.init_session().await;
        let (status, _, _) = ts
            .post(Some(&sid), &json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/list" }))
            .await;
        assert_eq!(status, 200);
        ts.handle.stop();
    }
}
