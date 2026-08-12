//! Sidecar job manager: registry, external-process supervision, and the
//! NDJSON stdout protocol (docs/ARCHITECTURE.md §6).
//!
//! Tauri-free (pure tokio + serde) so it can be compiled and unit-tested
//! without the Tauri/WebKit stack; `mod.rs` bridges it onto the frozen
//! `#[tauri::command]` surface and fans events out to the per-job
//! `Channel<SidecarEvent>` plus the `sidecar://*` app events.
//!
//! Protocol, one JSON object per stdout line:
//!   {"type":"progress","progress":0.42,"stage":"separating"}
//!   {"type":"log","line":"..."}                       (optional)
//!   {"type":"done","result":{...}}                    (terminal)
//!   {"type":"error","message":"..."}                  (terminal)
//! Anything unparseable is forwarded as a Log event. stderr lines become Log
//! events and feed the error-message tail.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, Semaphore};

use super::types::{JobKind, JobState, SidecarEvent, SidecarJobStatus};

/// Receives every event of one job. Production (`mod.rs`) sends to the
/// submit-time Channel and re-emits app events; tests use a collecting
/// closure. Must be cheap and non-blocking.
pub type EventSink = Arc<dyn Fn(SidecarEvent) + Send + Sync>;

/// How many stderr lines to keep for error messages.
const STDERR_TAIL_LINES: usize = 20;

/// Terminal jobs (done/error/cancelled) are kept for status queries, but only
/// this many and no longer than [`TERMINAL_JOB_TTL`]. Swept opportunistically
/// on `submit`/`list` so the registry cannot grow without bound.
const MAX_TERMINAL_JOBS: usize = 200;
/// Terminal jobs older than this are evicted.
const TERMINAL_JOB_TTL: Duration = Duration::from_secs(60 * 60);

/// What to launch for a job.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub kind: JobKind,
    /// Program to execute (e.g. resolved `python3`).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// SIGTERM → SIGKILL grace period on cancel.
    pub grace: Duration,
    /// Per-job environment overrides on top of the inherited env:
    /// `(name, Some(value))` sets, `(name, None)` removes. Cloud-worker
    /// secrets (e.g. `ELEVENLABS_API_KEY`) stay env-only — never in `args`.
    pub env: Vec<(String, Option<String>)>,
}

struct JobEntry {
    status: SidecarJobStatus,
    cancel: Arc<Notify>,
    cancel_requested: Arc<AtomicBool>,
    /// When the job reached a terminal state (eviction clock). `None` while
    /// queued/running — such entries are never evicted.
    finished_at: Option<Instant>,
}

/// Registry + supervisor. One instance lives in `SidecarState`.
pub struct JobManager {
    jobs: Mutex<HashMap<String, JobEntry>>,
    slots: Arc<Semaphore>,
    /// Minimum interval between forwarded worker progress events
    /// (ARCHITECTURE.md caps sidecar progress at <= 10 Hz).
    progress_interval: Duration,
}

impl Default for JobManager {
    fn default() -> Self {
        let max = std::env::var("AURA_SIDECAR_MAX_JOBS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(2);
        Self::new(max, Duration::from_millis(100))
    }
}

impl JobManager {
    pub fn new(max_concurrent: usize, progress_interval: Duration) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(max_concurrent.max(1))),
            progress_interval,
        }
    }

    /// Register a job and spawn its supervisor task. Returns the job UUID
    /// immediately. Must be called from within a tokio runtime (async
    /// commands run on Tauri's tokio runtime).
    pub fn submit(self: &Arc<Self>, spec: JobSpec, sink: EventSink) -> String {
        self.sweep_terminal();
        let job_id = uuid::Uuid::new_v4().to_string();
        let cancel = Arc::new(Notify::new());
        let cancel_requested = Arc::new(AtomicBool::new(false));
        self.jobs.lock().insert(
            job_id.clone(),
            JobEntry {
                status: SidecarJobStatus {
                    job_id: job_id.clone(),
                    kind: spec.kind.as_str().to_string(),
                    state: JobState::Queued.as_str().to_string(),
                    progress: 0.0,
                    stage: "queued".to_string(),
                    result: None,
                    error: None,
                },
                cancel: Arc::clone(&cancel),
                cancel_requested: Arc::clone(&cancel_requested),
                finished_at: None,
            },
        );
        sink(SidecarEvent::Progress {
            job_id: job_id.clone(),
            progress: 0.0,
            stage: "queued".to_string(),
        });
        let mgr = Arc::clone(self);
        let id = job_id.clone();
        tokio::spawn(async move {
            mgr.run_job(id, spec, sink, cancel, cancel_requested).await;
        });
        job_id
    }

    pub fn status(&self, job_id: &str) -> Result<SidecarJobStatus, String> {
        self.jobs
            .lock()
            .get(job_id)
            .map(|e| e.status.clone())
            .ok_or_else(|| format!("unknown job: {job_id}"))
    }

    pub fn list(&self) -> Vec<SidecarJobStatus> {
        self.sweep_terminal();
        self.jobs.lock().values().map(|e| e.status.clone()).collect()
    }

    /// Evict terminal jobs past the TTL, then the oldest beyond the count
    /// cap. Queued/running jobs are never evicted.
    fn sweep_terminal(&self) {
        self.sweep_terminal_with(MAX_TERMINAL_JOBS, TERMINAL_JOB_TTL);
    }

    fn sweep_terminal_with(&self, max_terminal: usize, ttl: Duration) {
        let now = Instant::now();
        let mut jobs = self.jobs.lock();
        jobs.retain(|_, e| e.finished_at.map_or(true, |t| now.duration_since(t) <= ttl));
        let mut terminal: Vec<(String, Instant)> = jobs
            .iter()
            .filter_map(|(id, e)| e.finished_at.map(|t| (id.clone(), t)))
            .collect();
        let excess = terminal.len().saturating_sub(max_terminal);
        if excess > 0 {
            terminal.sort_by_key(|&(_, t)| t);
            for (id, _) in terminal.into_iter().take(excess) {
                jobs.remove(&id);
            }
        }
    }

    /// Request cancellation. No-op (Ok) if the job already finished; the
    /// supervisor sends SIGTERM and escalates to SIGKILL after the grace
    /// period.
    pub fn cancel(&self, job_id: &str) -> Result<(), String> {
        let jobs = self.jobs.lock();
        let entry = jobs
            .get(job_id)
            .ok_or_else(|| format!("unknown job: {job_id}"))?;
        if JobState::is_terminal(&entry.status.state) {
            return Ok(());
        }
        entry.cancel_requested.store(true, Ordering::SeqCst);
        entry.cancel.notify_one();
        Ok(())
    }

    // -- supervisor ---------------------------------------------------------

    async fn run_job(
        self: Arc<Self>,
        job_id: String,
        spec: JobSpec,
        sink: EventSink,
        cancel: Arc<Notify>,
        cancel_requested: Arc<AtomicBool>,
    ) {
        if cancel_requested.load(Ordering::SeqCst) {
            self.finish_cancelled(&job_id, &sink);
            return;
        }

        // Wait for a free concurrency slot; cancel while queued aborts.
        let _permit = tokio::select! {
            p = Arc::clone(&self.slots).acquire_owned() => match p {
                Ok(p) => p,
                Err(_) => return, // semaphore closed: shutting down
            },
            _ = cancel.notified() => {
                self.finish_cancelled(&job_id, &sink);
                return;
            }
        };

        self.set_stage(&job_id, JobState::Running, 0.0, "starting");
        sink(SidecarEvent::Progress {
            job_id: job_id.clone(),
            progress: 0.0,
            stage: "starting".to_string(),
        });

        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in &spec.env {
            match value {
                Some(v) => {
                    cmd.env(name, v);
                }
                None => {
                    cmd.env_remove(name);
                }
            }
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.finish_error(
                    &job_id,
                    &sink,
                    format!(
                        "failed to launch sidecar `{}`: {e}",
                        spec.program.display()
                    ),
                );
                return;
            }
        };

        // stderr → Log events + tail buffer for error messages.
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = child.stderr.take().map(|stderr| {
            let tail = Arc::clone(&stderr_tail);
            let sink = Arc::clone(&sink);
            let id = job_id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    {
                        let mut t = tail.lock();
                        t.push_back(line.clone());
                        while t.len() > STDERR_TAIL_LINES {
                            t.pop_front();
                        }
                    }
                    sink(SidecarEvent::Log { job_id: id.clone(), line });
                }
            })
        });

        // stdout NDJSON loop, interruptible by cancel.
        let mut protocol_terminal: Option<Result<serde_json::Value, String>> = None;
        let mut cancelled = false;
        let mut last_progress_emit: Option<Instant> = None;
        if let Some(stdout) = child.stdout.take() {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    _ = cancel.notified() => {
                        cancelled = true;
                        Self::terminate(&mut child, spec.grace).await;
                        break;
                    }
                    line = lines.next_line() => match line {
                        Ok(Some(line)) => self.handle_stdout_line(
                            &job_id,
                            &sink,
                            &line,
                            &mut last_progress_emit,
                            &mut protocol_terminal,
                        ),
                        _ => break, // EOF or read error: child is exiting
                    }
                }
            }
        }

        // Reap the child (still cancellable while we wait for exit).
        let exit = if cancelled {
            let _ = child.wait().await;
            None
        } else {
            tokio::select! {
                st = child.wait() => st.ok(),
                _ = cancel.notified() => {
                    cancelled = true;
                    Self::terminate(&mut child, spec.grace).await;
                    let _ = child.wait().await;
                    None
                }
            }
        };

        // Let the stderr reader drain (EOF follows child exit) so error
        // messages can include the tail.
        if let Some(t) = stderr_task {
            let _ = tokio::time::timeout(Duration::from_secs(1), t).await;
        }

        if cancelled {
            self.finish_cancelled(&job_id, &sink);
            return;
        }

        let tail = {
            let t = stderr_tail.lock();
            t.iter().cloned().collect::<Vec<_>>().join("\n")
        };
        match protocol_terminal {
            Some(Ok(result)) => self.finish_done(&job_id, &sink, result),
            Some(Err(message)) => self.finish_error(&job_id, &sink, message),
            None => {
                let base = match exit {
                    Some(st) if st.success() => {
                        "sidecar exited without emitting a result".to_string()
                    }
                    Some(st) => format!("sidecar failed ({st})"),
                    None => "sidecar terminated unexpectedly".to_string(),
                };
                let message = if tail.is_empty() {
                    base
                } else {
                    format!("{base}\n{tail}")
                };
                self.finish_error(&job_id, &sink, message);
            }
        }
    }

    /// Polite termination: SIGTERM, wait `grace`, then SIGKILL.
    async fn terminate(child: &mut Child, grace: Duration) {
        #[cfg(unix)]
        {
            if let Some(pid) = child.id() {
                // No libc/nix dependency: use the coreutils `kill` binary.
                let _ = std::process::Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .status();
                if tokio::time::timeout(grace, child.wait()).await.is_ok() {
                    return;
                }
            }
        }
        #[cfg(not(unix))]
        let _ = grace;
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    fn handle_stdout_line(
        &self,
        job_id: &str,
        sink: &EventSink,
        line: &str,
        last_progress_emit: &mut Option<Instant>,
        protocol_terminal: &mut Option<Result<serde_json::Value, String>>,
    ) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        // After a terminal protocol line, remaining output is just log noise.
        if protocol_terminal.is_some() {
            sink(SidecarEvent::Log {
                job_id: job_id.to_string(),
                line: line.to_string(),
            });
            return;
        }
        let parsed: Option<serde_json::Value> = serde_json::from_str(trimmed).ok();
        let Some(v) = parsed else {
            sink(SidecarEvent::Log {
                job_id: job_id.to_string(),
                line: line.to_string(),
            });
            return;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("progress") => {
                let progress = v
                    .get("progress")
                    .and_then(|p| p.as_f64())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0);
                let stage = v
                    .get("stage")
                    .and_then(|s| s.as_str())
                    .unwrap_or("working")
                    .to_string();
                // Registry always reflects the latest progress...
                self.set_stage(job_id, JobState::Running, progress, &stage);
                // ...but forwarded events are throttled to <= 10 Hz.
                let now = Instant::now();
                let due = last_progress_emit
                    .map_or(true, |t| now.duration_since(t) >= self.progress_interval);
                if due {
                    *last_progress_emit = Some(now);
                    sink(SidecarEvent::Progress {
                        job_id: job_id.to_string(),
                        progress,
                        stage,
                    });
                }
            }
            Some("log") => {
                let text = v
                    .get("line")
                    .and_then(|l| l.as_str())
                    .unwrap_or(trimmed)
                    .to_string();
                sink(SidecarEvent::Log {
                    job_id: job_id.to_string(),
                    line: text,
                });
            }
            Some("done") => {
                let result = v.get("result").cloned().unwrap_or(serde_json::json!({}));
                *protocol_terminal = Some(Ok(result));
            }
            Some("error") => {
                let message = v
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown sidecar error")
                    .to_string();
                *protocol_terminal = Some(Err(message));
            }
            _ => sink(SidecarEvent::Log {
                job_id: job_id.to_string(),
                line: line.to_string(),
            }),
        }
    }

    // -- registry updates (never call the sink while holding the lock) ------

    fn set_stage(&self, job_id: &str, state: JobState, progress: f64, stage: &str) {
        if let Some(e) = self.jobs.lock().get_mut(job_id) {
            if JobState::is_terminal(&e.status.state) {
                return;
            }
            e.status.state = state.as_str().to_string();
            e.status.progress = progress;
            e.status.stage = stage.to_string();
        }
    }

    /// Transition to a terminal state exactly once. Returns false if the job
    /// is unknown or already terminal.
    fn finalize(
        &self,
        job_id: &str,
        state: JobState,
        f: impl FnOnce(&mut SidecarJobStatus),
    ) -> bool {
        let mut jobs = self.jobs.lock();
        let Some(e) = jobs.get_mut(job_id) else {
            return false;
        };
        if JobState::is_terminal(&e.status.state) {
            return false;
        }
        e.status.state = state.as_str().to_string();
        e.finished_at = Some(Instant::now());
        f(&mut e.status);
        true
    }

    fn finish_done(&self, job_id: &str, sink: &EventSink, result: serde_json::Value) {
        let emitted = self.finalize(job_id, JobState::Done, |s| {
            s.progress = 1.0;
            s.stage = "done".to_string();
            s.result = Some(result.clone());
        });
        if emitted {
            sink(SidecarEvent::Done {
                job_id: job_id.to_string(),
                result,
            });
        }
    }

    fn finish_error(&self, job_id: &str, sink: &EventSink, message: String) {
        let emitted = self.finalize(job_id, JobState::Error, |s| {
            s.stage = "error".to_string();
            s.error = Some(message.clone());
        });
        if emitted {
            sink(SidecarEvent::Error {
                job_id: job_id.to_string(),
                message,
            });
        }
    }

    fn finish_cancelled(&self, job_id: &str, sink: &EventSink) {
        let emitted = self.finalize(job_id, JobState::Cancelled, |s| {
            s.stage = "cancelled".to_string();
        });
        if emitted {
            sink(SidecarEvent::Log {
                job_id: job_id.to_string(),
                line: "job cancelled".to_string(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tool resolution
// ---------------------------------------------------------------------------

/// Find a Python 3 interpreter on PATH.
pub fn resolve_python() -> Result<PathBuf, String> {
    for name in ["python3", "python"] {
        if let Some(p) = which(name) {
            return Ok(p);
        }
    }
    Err("python3 not found on PATH — install Python 3 to run sidecar jobs (Demucs / Whisper wrappers)".to_string())
}

/// Resolve a wrapper script from the repo/app `sidecars/` directory.
/// Order: $AURA_SIDECAR_DIR, next to the executable (and up to three parents,
/// covering `src-tauri/target/{debug,release}`), `sidecars/` in the cwd or
/// any ancestor, then the compile-time repo layout (dev builds).
pub fn resolve_script(name: &str) -> Result<PathBuf, String> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("AURA_SIDECAR_DIR") {
        candidates.push(PathBuf::from(dir).join(name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(mut dir) = exe.parent().map(Path::to_path_buf) {
            for _ in 0..4 {
                candidates.push(dir.join("sidecars").join(name));
                if !dir.pop() {
                    break;
                }
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir: Option<&Path> = Some(cwd.as_path());
        while let Some(d) = dir {
            candidates.push(d.join("sidecars").join(name));
            dir = d.parent();
        }
    }
    // Dev fallback: <repo>/src-tauri (compile-time) → <repo>/sidecars.
    candidates.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sidecars")
            .join(name),
    );
    for c in &candidates {
        if c.is_file() {
            return Ok(c.canonicalize().unwrap_or_else(|_| c.clone()));
        }
    }
    Err(format!(
        "sidecar script `{name}` not found — set AURA_SIDECAR_DIR to the directory containing the wrapper scripts"
    ))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

// ---------------------------------------------------------------------------
// Tests — run without demucs/whisper/python: fake sidecars are /bin/sh
// scripts emitting scripted NDJSON. (On this machine `cargo test` of the full
// crate is blocked by missing system webkit packages; the same tests compile
// and run in a plain tokio harness, see the final report.)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    fn test_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("aura-sidecar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn sh_spec(script: &Path) -> JobSpec {
        JobSpec {
            kind: JobKind::StemSplit,
            program: PathBuf::from("/bin/sh"),
            args: vec![script.to_string_lossy().into_owned()],
            grace: Duration::from_millis(500),
            env: Vec::new(),
        }
    }

    fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<SidecarEvent>>>) {
        let events: Arc<Mutex<Vec<SidecarEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let e = Arc::clone(&events);
        (Arc::new(move |ev| e.lock().push(ev)), events)
    }

    async fn wait_terminal(
        mgr: &Arc<JobManager>,
        job_id: &str,
        timeout: Duration,
    ) -> SidecarJobStatus {
        let deadline = Instant::now() + timeout;
        loop {
            let st = mgr.status(job_id).expect("job registered");
            if JobState::is_terminal(&st.state) {
                return st;
            }
            assert!(Instant::now() < deadline, "job did not finish in time: {st:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn manager() -> Arc<JobManager> {
        // Throttle off so tests see every progress event.
        Arc::new(JobManager::new(2, Duration::ZERO))
    }

    #[tokio::test]
    async fn success_flow_streams_progress_and_done() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "ok.sh",
            r#"#!/bin/sh
echo '{"type":"progress","progress":0.25,"stage":"separating"}'
echo 'plain text noise'
echo '{"type":"progress","progress":0.9,"stage":"writing stems"}'
echo '{"type":"done","result":{"kind":"stemSplit","stems":{"vocals":"/tmp/v.wav"}}}'
"#,
        );
        let mgr = manager();
        let (sink, events) = collecting_sink();
        let id = mgr.submit(sh_spec(&script), sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(10)).await;

        assert_eq!(st.state, "done");
        assert_eq!(st.progress, 1.0);
        let result = st.result.expect("result present");
        assert_eq!(result["kind"], "stemSplit");
        assert_eq!(result["stems"]["vocals"], "/tmp/v.wav");
        assert!(st.error.is_none());

        let evs = events.lock();
        assert!(matches!(evs.last(), Some(SidecarEvent::Done { .. })));
        assert!(evs.iter().any(
            |e| matches!(e, SidecarEvent::Progress { progress, stage, .. } if *progress == 0.9 && stage == "writing stems")
        ));
        assert!(evs.iter().any(
            |e| matches!(e, SidecarEvent::Log { line, .. } if line == "plain text noise")
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn protocol_error_line_fails_job() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "err.sh",
            r#"#!/bin/sh
echo '{"type":"progress","progress":0.1,"stage":"loading model"}'
echo '{"type":"error","message":"demucs is not installed - pip install demucs"}'
exit 3
"#,
        );
        let mgr = manager();
        let (sink, events) = collecting_sink();
        let id = mgr.submit(sh_spec(&script), sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(10)).await;

        assert_eq!(st.state, "error");
        assert!(st.error.as_deref().unwrap().contains("pip install demucs"));
        assert!(events
            .lock()
            .iter()
            .any(|e| matches!(e, SidecarEvent::Error { message, .. } if message.contains("pip install"))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn crash_without_result_reports_stderr_tail() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "crash.sh",
            r#"#!/bin/sh
echo 'kaboom: traceback line' >&2
exit 7
"#,
        );
        let mgr = manager();
        let (sink, events) = collecting_sink();
        let id = mgr.submit(sh_spec(&script), sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(10)).await;

        assert_eq!(st.state, "error");
        let msg = st.error.unwrap();
        assert!(msg.contains("kaboom"), "stderr tail missing: {msg}");
        // stderr also arrived as a Log event
        assert!(events
            .lock()
            .iter()
            .any(|e| matches!(e, SidecarEvent::Log { line, .. } if line.contains("kaboom"))));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn clean_exit_without_done_is_an_error() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "silent.sh",
            "#!/bin/sh\necho '{\"type\":\"progress\",\"progress\":0.5,\"stage\":\"x\"}'\nexit 0\n",
        );
        let mgr = manager();
        let (sink, _events) = collecting_sink();
        let id = mgr.submit(sh_spec(&script), sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(10)).await;
        assert_eq!(st.state, "error");
        assert!(st.error.unwrap().contains("without emitting a result"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancel_kills_running_job() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "slow.sh",
            r#"#!/bin/sh
echo '{"type":"progress","progress":0.1,"stage":"separating"}'
sleep 30
echo '{"type":"done","result":{}}'
"#,
        );
        let mgr = manager();
        let (sink, _events) = collecting_sink();
        let id = mgr.submit(sh_spec(&script), sink);

        // Wait until it is actually running (first progress recorded).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let st = mgr.status(&id).unwrap();
            if st.state == "running" && st.progress >= 0.1 {
                break;
            }
            assert!(Instant::now() < deadline, "never reached running: {st:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let t0 = Instant::now();
        mgr.cancel(&id).unwrap();
        let st = wait_terminal(&mgr, &id, Duration::from_secs(5)).await;
        assert_eq!(st.state, "cancelled");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "cancel took too long: {:?}",
            t0.elapsed()
        );
        // Cancelling again is a no-op, not an error.
        mgr.cancel(&id).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn concurrency_limit_queues_second_job() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "busy.sh",
            r#"#!/bin/sh
echo '{"type":"progress","progress":0.5,"stage":"working"}'
sleep 0.5
echo '{"type":"done","result":{"kind":"stemSplit","stems":{}}}'
"#,
        );
        let mgr = Arc::new(JobManager::new(1, Duration::ZERO));
        let (sink1, _) = collecting_sink();
        let (sink2, _) = collecting_sink();
        let id1 = mgr.submit(sh_spec(&script), sink1);
        let id2 = mgr.submit(sh_spec(&script), sink2);

        // Give the first job time to grab the slot.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let states: Vec<String> = vec![
            mgr.status(&id1).unwrap().state,
            mgr.status(&id2).unwrap().state,
        ];
        assert!(
            states.contains(&"running".to_string()) && states.contains(&"queued".to_string()),
            "expected one running + one queued, got {states:?}"
        );

        let st1 = wait_terminal(&mgr, &id1, Duration::from_secs(10)).await;
        let st2 = wait_terminal(&mgr, &id2, Duration::from_secs(10)).await;
        assert_eq!(st1.state, "done");
        assert_eq!(st2.state, "done");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancel_while_queued_never_starts_process() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "hold.sh",
            "#!/bin/sh\nsleep 2\necho '{\"type\":\"done\",\"result\":{}}'\n",
        );
        let mgr = Arc::new(JobManager::new(1, Duration::ZERO));
        let (sink1, _) = collecting_sink();
        let (sink2, _) = collecting_sink();
        let id1 = mgr.submit(sh_spec(&script), sink1);
        let id2 = mgr.submit(sh_spec(&script), sink2);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(mgr.status(&id2).unwrap().state, "queued");
        mgr.cancel(&id2).unwrap();
        let st2 = wait_terminal(&mgr, &id2, Duration::from_secs(2)).await;
        assert_eq!(st2.state, "cancelled");
        // First job unaffected.
        let st1 = wait_terminal(&mgr, &id1, Duration::from_secs(10)).await;
        assert_eq!(st1.state, "done");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn missing_program_is_structured_error() {
        let mgr = manager();
        let (sink, events) = collecting_sink();
        let spec = JobSpec {
            kind: JobKind::Transcribe,
            program: PathBuf::from("/nonexistent/bin/python3"),
            args: vec![],
            grace: Duration::from_millis(100),
            env: Vec::new(),
        };
        let id = mgr.submit(spec, sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(5)).await;
        assert_eq!(st.state, "error");
        assert!(st.error.unwrap().contains("failed to launch"));
        assert!(matches!(
            events.lock().last(),
            Some(SidecarEvent::Error { .. })
        ));
    }

    #[tokio::test]
    async fn job_env_overrides_set_and_remove_variables() {
        let dir = test_dir();
        let script = write_script(
            &dir,
            "env.sh",
            r#"#!/bin/sh
echo "{\"type\":\"done\",\"result\":{\"set\":\"$AURA_TEST_ENV_SET\",\"removed\":\"${AURA_TEST_ENV_REMOVED:-gone}\"}}"
"#,
        );
        std::env::set_var("AURA_TEST_ENV_REMOVED", "still-here");
        let mut spec = sh_spec(&script);
        spec.env = vec![
            ("AURA_TEST_ENV_SET".into(), Some("yes".into())),
            ("AURA_TEST_ENV_REMOVED".into(), None),
        ];
        let mgr = manager();
        let (sink, _events) = collecting_sink();
        let id = mgr.submit(spec, sink);
        let st = wait_terminal(&mgr, &id, Duration::from_secs(10)).await;
        std::env::remove_var("AURA_TEST_ENV_REMOVED");
        assert_eq!(st.state, "done");
        let result = st.result.unwrap();
        assert_eq!(result["set"], "yes");
        assert_eq!(result["removed"], "gone", "env_remove beats inherited env");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Insert a fake registry entry directly (no process) for eviction tests.
    fn insert_entry(mgr: &JobManager, id: &str, state: JobState, age: Duration) {
        let terminal = JobState::is_terminal(state.as_str());
        mgr.jobs.lock().insert(
            id.to_string(),
            JobEntry {
                status: SidecarJobStatus {
                    job_id: id.to_string(),
                    kind: "stemSplit".to_string(),
                    state: state.as_str().to_string(),
                    progress: if terminal { 1.0 } else { 0.5 },
                    stage: state.as_str().to_string(),
                    result: None,
                    error: None,
                },
                cancel: Arc::new(Notify::new()),
                cancel_requested: Arc::new(AtomicBool::new(false)),
                finished_at: terminal.then(|| Instant::now() - age),
            },
        );
    }

    #[tokio::test]
    async fn sweep_evicts_old_and_excess_terminal_jobs_only() {
        let mgr = manager();
        insert_entry(&mgr, "old", JobState::Done, Duration::from_millis(100));
        insert_entry(&mgr, "new-1", JobState::Error, Duration::from_millis(30));
        insert_entry(&mgr, "new-2", JobState::Cancelled, Duration::from_millis(20));
        insert_entry(&mgr, "new-3", JobState::Done, Duration::from_millis(10));
        // Non-terminal entries must never be evicted, whatever their age.
        insert_entry(&mgr, "running", JobState::Running, Duration::ZERO);

        // TTL sweep: only `old` is past a 50 ms TTL.
        mgr.sweep_terminal_with(200, Duration::from_millis(50));
        assert!(mgr.status("old").is_err(), "past-TTL job evicted");
        assert!(mgr.status("new-1").is_ok());

        // Count sweep: cap 2 evicts the oldest terminal entry only.
        mgr.sweep_terminal_with(2, Duration::from_secs(3600));
        assert!(mgr.status("new-1").is_err(), "oldest beyond cap evicted");
        assert!(mgr.status("new-2").is_ok());
        assert!(mgr.status("new-3").is_ok());
        assert!(mgr.status("running").is_ok(), "running job survives sweeps");
    }

    #[tokio::test]
    async fn submit_and_list_evict_terminal_backlog_beyond_cap() {
        let mgr = manager();
        let extra = 5usize;
        for i in 0..(MAX_TERMINAL_JOBS + extra) {
            // Older ids get larger ages so eviction order is deterministic.
            let age = Duration::from_millis((MAX_TERMINAL_JOBS + extra - i) as u64);
            insert_entry(&mgr, &format!("job-{i}"), JobState::Done, age);
        }

        let (sink, _events) = collecting_sink();
        let spec = JobSpec {
            kind: JobKind::Transcribe,
            program: PathBuf::from("/nonexistent/bin/python3"),
            args: vec![],
            grace: Duration::from_millis(100),
            env: Vec::new(),
        };
        let id = mgr.submit(spec, sink);

        // submit swept the oldest `extra` terminal jobs before inserting.
        for i in 0..extra {
            assert!(mgr.status(&format!("job-{i}")).is_err(), "job-{i} evicted");
        }
        assert!(mgr.status(&format!("job-{extra}")).is_ok());
        assert!(mgr.status(&id).is_ok());
        assert_eq!(mgr.jobs.lock().len(), MAX_TERMINAL_JOBS + 1);

        // Once the new job is terminal too, list() sweeps back to the cap —
        // evicting the oldest, keeping the recently finished job queryable.
        let _ = wait_terminal(&mgr, &id, Duration::from_secs(5)).await;
        assert_eq!(mgr.list().len(), MAX_TERMINAL_JOBS);
        assert!(mgr.status(&id).is_ok(), "recently finished job still queryable");
        assert!(mgr.status(&format!("job-{extra}")).is_err(), "oldest evicted at cap");
    }

    #[tokio::test]
    async fn unknown_job_ids_are_errors() {
        let mgr = manager();
        assert!(mgr.status("nope").is_err());
        assert!(mgr.cancel("nope").is_err());
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn event_wire_format_matches_schema() {
        let ev = SidecarEvent::Progress {
            job_id: "j1".into(),
            progress: 0.42,
            stage: "separating".into(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "progress");
        assert_eq!(v["jobId"], "j1");
        assert_eq!(v["progress"], 0.42);
        assert_eq!(v["stage"], "separating");

        let ev = SidecarEvent::Error {
            job_id: "j2".into(),
            message: "boom".into(),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["message"], "boom");
    }

    #[test]
    fn resolve_script_honors_env_override() {
        let dir = test_dir();
        write_script(&dir, "fake_wrapper.py", "print('hi')\n");
        std::env::set_var("AURA_SIDECAR_DIR", &dir);
        let found = resolve_script("fake_wrapper.py").expect("resolved via env");
        assert!(found.ends_with("fake_wrapper.py"));
        std::env::remove_var("AURA_SIDECAR_DIR");
        let _ = std::fs::remove_dir_all(dir);
    }
}
