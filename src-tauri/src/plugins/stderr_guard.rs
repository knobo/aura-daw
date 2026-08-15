//! Guards the process's fd 2 (stderr) against chatty native plugins.
//!
//! ZynAddSubFX's LV2 wrapper calls blocking libc I/O (`fprintf(stderr, ...)`)
//! from inside its own `run()`/`process()` — which [`super::lv2_host`] calls
//! directly on AURA's real-time audio thread (`Lv2Node::process`, `instance
//! .run(...)`). Hosted headless (no native UI attached), Zyn's internal
//! state-sync queue never drains and it logs "Sending key 'state' to UI
//! failed, out of space" on every DSP block — a blocking write, on the RT
//! thread, every callback.
//!
//! [`install`] replaces fd 2 with the write end of a pipe (via `dup2`) and
//! hands the read end to a dedicated background thread that continuously
//! drains it and re-emits deduplicated lines to the real stderr. Because the
//! pipe always has a reader, writes into it never block on a full terminal
//! or pipe buffer; because repeats collapse to a single summary line, a
//! spamming plugin no longer floods the console. This is process-wide (any
//! native code writing to fd 2, not just LV2 plugins) and installed once,
//! lazily, before any plugin world is built — never on the RT thread itself.

use std::sync::Once;

static INSTALL: Once = Once::new();

/// Idempotent, cheap to call repeatedly; only the first call installs the
/// guard. Call before instantiating any native plugin. Never call this from
/// the RT audio thread — it does a handful of syscalls and spawns a thread.
pub fn install() {
    INSTALL.call_once(|| {
        #[cfg(unix)]
        if let Err(e) = unix::try_install() {
            log::warn!("stderr_guard: not installed ({e}); chatty plugins may spam/block stderr");
        }
    });
}

/// Collapses runs of identical lines into the first occurrence plus a
/// periodic "repeated N times" summary. Pure and independently testable —
/// no fds, no threads.
struct Deduper {
    last: Option<String>,
    repeats: u64,
}

/// What a [`Deduper`] wants written for one incoming line.
#[derive(Debug, PartialEq, Eq)]
enum Forward {
    /// Emit this line as-is.
    Line(String),
    /// A repeat of the last line — emit a periodic summary instead (rare),
    /// or nothing (the common case).
    Summary(String),
    Suppress,
}

/// Summaries land every this-many repeats, so a permanent spammer still
/// surfaces occasionally without flooding the real stderr.
const SUMMARY_EVERY: u64 = 200;

impl Deduper {
    fn new() -> Self {
        Self { last: None, repeats: 0 }
    }

    fn on_line(&mut self, line: &str) -> Forward {
        if self.last.as_deref() == Some(line) {
            self.repeats += 1;
            if self.repeats % SUMMARY_EVERY == 0 {
                return Forward::Summary(format!(
                    "(previous line repeated {} times so far) {line}",
                    self.repeats
                ));
            }
            return Forward::Suppress;
        }
        self.last = Some(line.to_string());
        self.repeats = 0;
        Forward::Line(line.to_string())
    }
}

#[cfg(unix)]
mod unix {
    use super::Deduper;
    use std::fs::File;
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::FromRawFd;
    use std::os::raw::c_int;

    // Declared locally rather than pulling in the `libc` crate (Cargo.toml
    // is frozen — see its header comment): std already links libc on Unix,
    // so these well-known, stable-ABI syscalls resolve without extra deps.
    extern "C" {
        fn pipe(fds: *mut c_int) -> c_int;
        fn dup(oldfd: c_int) -> c_int;
        fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    }

    const STDERR_FD: c_int = 2;

    pub(super) fn try_install() -> std::io::Result<()> {
        // Keep a live handle to the real terminal/file stderr currently
        // pointed at by fd 2, so drained lines still land somewhere visible.
        let real_stderr_fd = unsafe { dup(STDERR_FD) };
        if real_stderr_fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut fds = [0 as c_int; 2];
        if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let [read_fd, write_fd] = fds;
        if unsafe { dup2(write_fd, STDERR_FD) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // fd 2 now refers to the same pipe as `write_fd`; the standalone
        // descriptor is redundant (closing it does not close the pipe).
        drop(unsafe { File::from_raw_fd(write_fd) });

        let reader = unsafe { File::from_raw_fd(read_fd) };
        let sink = unsafe { File::from_raw_fd(real_stderr_fd) };
        std::thread::Builder::new()
            .name("stderr-dedup".into())
            .spawn(move || drain_loop(reader, sink))?;
        Ok(())
    }

    fn drain_loop(reader: File, mut sink: File) {
        let mut reader = BufReader::new(reader);
        let mut dedup = Deduper::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // write end closed — process is exiting
                Ok(_) => {
                    let trimmed = line.trim_end_matches('\n');
                    match dedup.on_line(trimmed) {
                        super::Forward::Line(l) => {
                            let _ = writeln!(sink, "{l}");
                        }
                        super::Forward::Summary(s) => {
                            let _ = writeln!(sink, "{s}");
                        }
                        super::Forward::Suppress => {}
                    }
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Deduper, Forward};

    #[test]
    fn distinct_lines_all_forward() {
        let mut d = Deduper::new();
        assert_eq!(d.on_line("a"), Forward::Line("a".into()));
        assert_eq!(d.on_line("b"), Forward::Line("b".into()));
        assert_eq!(d.on_line("c"), Forward::Line("c".into()));
    }

    #[test]
    fn repeats_are_suppressed_until_the_summary_threshold() {
        let mut d = Deduper::new();
        assert_eq!(d.on_line("spam"), Forward::Line("spam".into()));
        for _ in 0..(super::SUMMARY_EVERY - 1) {
            assert_eq!(d.on_line("spam"), Forward::Suppress);
        }
        match d.on_line("spam") {
            Forward::Summary(s) => assert!(s.contains("spam") && s.contains("200")),
            other => panic!("expected a summary, got {other:?}"),
        }
    }

    #[test]
    fn a_new_line_resets_the_repeat_count() {
        let mut d = Deduper::new();
        d.on_line("spam");
        d.on_line("spam");
        d.on_line("spam");
        assert_eq!(d.on_line("different"), Forward::Line("different".into()));
        // back to a fresh run — no leftover count from the previous line
        assert_eq!(d.on_line("different"), Forward::Suppress);
    }

    /// Manual repro/verification harness for the real fd-2 plumbing
    /// (`install`/`drain_loop`), not just the pure `Deduper` logic above:
    /// installs the guard, then hammers stderr with Zyn's exact spam line
    /// from a background thread — the way its `run()` call would, but
    /// without depending on actually reproducing Zyn's own (rare,
    /// non-deterministic per project docs) internal trigger. Run with:
    /// `cargo test --lib stderr_guard_synthetic_spam_is_deduped_not_blocking -- --ignored --nocapture`
    /// and read the terminal: raw repeated lines means the guard isn't
    /// installed/working; one line plus periodic "repeated N times"
    /// summaries means it is. The elapsed time printed at the end also
    /// confirms 20k writes never blocked on a full pipe/terminal buffer.
    #[test]
    #[ignore]
    fn stderr_guard_synthetic_spam_is_deduped_not_blocking() {
        super::install();
        const SPAM: &str = "Sending key 'state' to UI failed, out of space";
        const ITERATIONS: usize = 20_000;
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            eprintln!("{SPAM}");
        }
        eprintln!(
            "repro: {ITERATIONS} identical writes to stderr in {:.3}s — \
             look above for a single \"{SPAM}\" line plus periodic \
             \"repeated N times\" summaries, not {ITERATIONS} raw lines",
            start.elapsed().as_secs_f64()
        );
        // Give the drain thread a moment to flush the last summary before
        // the test process potentially exits.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
