//! Sacrificial CLAP scan subprocess (zone P1 — pays debt D-11's scan half;
//! PHASE3-PLAN contract 8).
//!
//! Reading a CLAP bundle's descriptors means `dlopen`ing foreign code — a
//! malformed bundle can crash the process. So `plugin_scan` never dlopens in
//! AURA itself: it re-executes the current binary with `AURA_SCAN_WORKER=1`
//! (guard at the top of `main`), hands it the bundle list, and reads NDJSON
//! descriptors off its stdout. When the worker dies mid-bundle (segfault,
//! abort, hang), the parent notes which bundle it was chewing on (each is
//! announced with a `scanning` line before the dlopen), skips it, respawns
//! for the remaining bundles, and AURA keeps running.
//!
//! Wire protocol (one JSON object per line on the worker's stdout; unknown
//! lines are ignored so the worker can share stdout with test harnesses):
//!
//! ```text
//! {"type":"scanning","bundle":"/usr/lib/clap/X.clap"}   // before dlopen
//! {"type":"descriptor","descriptor":{...}}              // 0..n per bundle
//! {"type":"error","bundle":"...","message":"..."}       // soft failure
//! {"type":"done"}                                       // clean end
//! ```

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::descriptor::PluginDescriptor;
use super::scan;

/// Env flag marking a worker invocation (checked at the top of `main`).
pub const ENV_WORKER: &str = "AURA_SCAN_WORKER";
/// Newline-separated bundle list handed to the worker.
pub const ENV_BUNDLES: &str = "AURA_SCAN_BUNDLES";

/// Per-spawn hang timeout: if the worker produces no line for this long it
/// is killed and its current bundle skipped.
const LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// True when this process was started as a scan worker.
pub fn is_worker_invocation() -> bool {
    std::env::var(ENV_WORKER).as_deref() == Ok("1")
}

/// Worker-side body: scan each bundle from [`ENV_BUNDLES`], speak NDJSON on
/// stdout. Every line is flushed so a crash loses nothing already scanned.
/// Returns the process exit code.
pub fn worker_main() -> i32 {
    let Ok(list) = std::env::var(ENV_BUNDLES) else {
        eprintln!("scan worker: {ENV_BUNDLES} not set");
        return 2;
    };
    let stdout = std::io::stdout();
    let emit = |v: serde_json::Value| {
        let mut lock = stdout.lock();
        let _ = writeln!(lock, "{v}");
        let _ = lock.flush();
    };
    for path in list.lines().map(str::trim).filter(|s| !s.is_empty()) {
        emit(serde_json::json!({"type": "scanning", "bundle": path}));
        match scan::scan_clap_bundle(Path::new(path)) {
            Ok(descs) => {
                for d in descs {
                    emit(serde_json::json!({"type": "descriptor", "descriptor": d}));
                }
            }
            Err(e) => emit(serde_json::json!({"type": "error", "bundle": path, "message": e})),
        }
    }
    emit(serde_json::json!({"type": "done"}));
    0
}

/// How to launch a worker process (overridable so tests can reroute through
/// the libtest harness binary).
#[derive(Clone, Debug)]
pub struct WorkerCommand {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

impl WorkerCommand {
    /// The production launch: re-execute the current binary; the
    /// `AURA_SCAN_WORKER=1` guard at the top of `main` routes it into
    /// [`worker_main`].
    pub fn current_exe() -> Option<Self> {
        std::env::current_exe()
            .ok()
            .map(|exe| Self { exe, args: Vec::new() })
    }
}

/// Parent-side scan of the standard CLAP paths through the sacrificial
/// worker. Falls back to the in-process scan (with a warning) only when a
/// worker cannot be spawned at all (e.g. `current_exe` unavailable).
pub fn scan_clap_subprocess(roots: &[PathBuf]) -> Vec<PluginDescriptor> {
    let bundles = scan::find_clap_bundles(roots);
    if bundles.is_empty() {
        return Vec::new();
    }
    if is_worker_invocation() {
        // We ARE the worker (defensive: never spawn recursively).
        return scan::scan_clap_paths(roots);
    }
    match WorkerCommand::current_exe() {
        Some(cmd) => scan_with_worker(&bundles, &cmd),
        None => {
            log::warn!("plugin scan: cannot resolve current exe; falling back to in-process CLAP scan");
            scan::scan_clap_paths(roots)
        }
    }
}

/// Scan `bundles` through worker subprocesses, respawning past any bundle
/// that kills or hangs the worker. Never panics, never takes AURA down.
pub fn scan_with_worker(bundles: &[PathBuf], cmd: &WorkerCommand) -> Vec<PluginDescriptor> {
    let mut out: Vec<PluginDescriptor> = Vec::new();
    let mut queue: Vec<String> = bundles
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    // Each round makes progress (>=1 bundle scanned or skipped) or aborts,
    // so this terminates after at most len rounds.
    while !queue.is_empty() {
        let spawned = Command::new(&cmd.exe)
            .args(&cmd.args)
            .env(ENV_WORKER, "1")
            .env(ENV_BUNDLES, queue.join("\n"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                log::warn!("plugin scan: failed to spawn scan worker ({e}); scanning in-process");
                out.extend(scan::scan_clap_bundles_in_process(
                    &queue.iter().map(PathBuf::from).collect::<Vec<_>>(),
                ));
                return out;
            }
        };

        // Reader thread streams lines; the parent enforces the hang timeout.
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = crossbeam_channel::unbounded::<String>();
        std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        let mut scanning: Option<String> = None;
        let mut done = false;
        let mut timed_out = false;
        loop {
            match rx.recv_timeout(LINE_TIMEOUT) {
                Ok(line) => {
                    let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                        continue; // harness noise etc.
                    };
                    match v.get("type").and_then(|t| t.as_str()) {
                        Some("scanning") => {
                            scanning = v
                                .get("bundle")
                                .and_then(|b| b.as_str())
                                .map(str::to_string);
                        }
                        Some("descriptor") => {
                            if let Some(d) = v.get("descriptor") {
                                match serde_json::from_value::<PluginDescriptor>(d.clone()) {
                                    Ok(d) => out.push(d),
                                    Err(e) => log::warn!("plugin scan: bad descriptor line: {e}"),
                                }
                            }
                        }
                        Some("error") => log::warn!(
                            "plugin scan (clap worker): {}: {}",
                            v.get("bundle").and_then(|b| b.as_str()).unwrap_or("?"),
                            v.get("message").and_then(|m| m.as_str()).unwrap_or("?")
                        ),
                        Some("done") => {
                            done = true;
                        }
                        _ => {}
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    timed_out = true;
                    break;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break, // EOF
            }
        }
        if timed_out {
            let _ = child.kill();
        }
        let _ = child.wait();

        if done {
            break;
        }
        // Worker died (or hung) mid-scan: drop everything up to AND
        // including the bundle it announced last, then respawn.
        match scanning {
            Some(bad) => {
                log::warn!(
                    "plugin scan: worker {} on {bad}; bundle skipped",
                    if timed_out { "hung" } else { "died" }
                );
                match queue.iter().position(|b| *b == bad) {
                    Some(pos) => queue.drain(..=pos),
                    None => break, // shouldn't happen; avoid looping
                };
            }
            None => {
                log::warn!("plugin scan: worker made no progress; aborting CLAP scan");
                break;
            }
        }
    }
    out
}

/// Worker command for TEST binaries: spawns this test binary filtered down
/// to the hidden `worker_entry` test (the libtest harness has no
/// `AURA_SCAN_WORKER` guard in `main`, so plain re-execution would run the
/// whole suite). Lets other test modules scan third-party CLAP bundles
/// through a sacrificial child instead of dlopening them into the shared
/// test process (Cardinal's exit-time teardown corrupts the heap — observed
/// intermittently killing full-suite runs when scanned in-process).
#[cfg(test)]
pub fn test_worker_command() -> WorkerCommand {
    WorkerCommand {
        exe: std::env::current_exe().expect("test exe"),
        args: vec![
            "--exact".into(),
            "plugins::scan_worker::tests::worker_entry".into(),
            "--nocapture".into(),
        ],
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::plugins::scan::{clap_search_paths, find_clap_bundles};

    /// HIDDEN WORKER ENTRY for subprocess tests: when the test binary is
    /// re-executed with `--exact <this test> --nocapture` and the worker env
    /// set, this "test" becomes the worker body (the real binary routes via
    /// the guard in main.rs instead). Without the env it is a no-op.
    #[test]
    fn worker_entry() {
        if is_worker_invocation() {
            assert_eq!(worker_main(), 0);
        }
    }

    /// Launch THIS test binary as the worker through the hidden entry.
    fn test_worker_cmd() -> WorkerCommand {
        super::test_worker_command()
    }

    /// The worker subprocess yields the same descriptors as the in-process
    /// scan (gated on installed CLAP bundles).
    #[test]
    fn worker_scan_matches_in_process_scan() {
        let roots = clap_search_paths();
        // The in-process side of this comparison must not dlopen Cardinal:
        // its exit-time teardown corrupts the heap and intermittently kills
        // the whole test binary (see `test_worker_command` docs). Compare on
        // the remaining bundles; Cardinal itself is still scanned by the
        // worker-only tests and probed in `plugins::patches`.
        let bundles: Vec<_> = find_clap_bundles(&roots)
            .into_iter()
            .filter(|b| !b.to_string_lossy().contains("Cardinal"))
            .collect();
        if bundles.is_empty() {
            eprintln!("skipping: no CLAP bundles installed");
            return;
        }
        let mut in_proc: Vec<String> = scan::scan_clap_bundles_in_process(&bundles)
            .into_iter()
            .map(|d| d.uid)
            .collect();
        let mut via_worker: Vec<String> = scan_with_worker(&bundles, &test_worker_cmd())
            .into_iter()
            .map(|d| d.uid)
            .collect();
        in_proc.sort();
        via_worker.sort();
        assert!(!via_worker.is_empty(), "worker scan found descriptors");
        assert_eq!(via_worker, in_proc, "worker and in-process scans agree");
    }

    /// CRASH CONTAINMENT (the point of the subprocess): a bundle whose mere
    /// dlopen aborts the process must neither take AURA down nor prevent
    /// later bundles from being scanned.
    #[test]
    fn worker_scan_survives_crashing_and_garbage_bundles() {
        let dir = std::env::temp_dir().join(format!(
            "aura-scan-crash-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. Garbage: not a loadable library (soft error inside the worker).
        let garbage = dir.join("garbage.clap");
        std::fs::write(&garbage, b"this is not an ELF").unwrap();

        // 2. Evil: a real shared object whose constructor aborts on dlopen.
        //    Requires a C compiler; skipped (with note) when unavailable.
        let evil = dir.join("evil.clap");
        let evil_built = {
            let src = dir.join("evil.c");
            std::fs::write(
                &src,
                "#include <stdlib.h>\n__attribute__((constructor)) static void boom(void){ abort(); }\n",
            )
            .unwrap();
            std::process::Command::new("cc")
                .args(["-shared", "-fPIC", "-o"])
                .arg(&evil)
                .arg(&src)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !evil_built {
            eprintln!("note: no C compiler; crash-containment covers garbage bundle only");
        }

        // 3. A real bundle AFTER the evil one, to prove the scan resumes.
        let real_src = find_clap_bundles(&clap_search_paths()).into_iter().next();
        let real = real_src.as_ref().map(|src| {
            let dst = dir.join("real.clap");
            std::fs::copy(src, &dst).unwrap();
            dst
        });

        let mut bundles = vec![garbage.clone()];
        if evil_built {
            bundles.push(evil.clone());
        }
        if let Some(r) = &real {
            bundles.push(r.clone());
        }

        let descs = scan_with_worker(&bundles, &test_worker_cmd());
        // Still alive — containment worked. Garbage/evil yielded nothing:
        assert!(
            descs.iter().all(|d| !d.uid.contains("garbage.clap") && !d.uid.contains("evil.clap")),
            "bad bundles yield no descriptors"
        );
        if let Some(r) = &real {
            let r_str = r.to_string_lossy();
            assert!(
                descs.iter().any(|d| d.uid.contains(r_str.as_ref())),
                "scan resumed past the crashing bundle and read {r_str}; got {:?}",
                descs.iter().map(|d| &d.uid).collect::<Vec<_>>()
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
