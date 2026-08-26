//! An allocation-counting global allocator, so a test can state *zero
//! allocations* about an RT call instead of hoping.
//!
//! `jit.md` asked for the `assert_no_alloc` crate. This is ~60 lines with no
//! dependency and does strictly more, which matters for two reasons:
//!
//! * `assert_no_alloc` was last published in 2021 and only *trips* — it can
//!   tell you an allocation happened, not how many, and not how many bytes.
//!   "The JIT kernel allocates 3 times per block" and "it allocates" need
//!   very different fixes, and a regression from 0 to 1 is the one worth
//!   catching.
//! * Counting is composable with benchmarks: the same numbers can be asserted
//!   in a test and reported next to a timing.
//!
//! Counters are **per thread** and never synchronize, so a test measuring the
//! audio-thread call is not perturbed by whatever the harness threads do.
//! `Cell` with a `const` initialiser is deliberate: the allocator itself must
//! not allocate, and a thread-local with a destructor would be reached again
//! *during* TLS teardown, where the dealloc path would find it already
//! dropped.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static DEALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

/// Wraps the system allocator and counts. Register it in a binary that wants
/// the numbers:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: aura_engine::metrics::AllocCounter = aura_engine::metrics::AllocCounter::new();
/// ```
///
/// Deliberately NOT registered by this library: a `#[global_allocator]` is a
/// whole-program decision, and a crate that makes it for its dependents is a
/// crate you cannot link into an app that made its own.
pub struct AllocCounter;

impl AllocCounter {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for AllocCounter {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl GlobalAlloc for AllocCounter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump(&ALLOCS, 1);
        bump(&BYTES, layout.size() as u64);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        bump(&DEALLOCS, 1);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // A realloc is an allocation as far as the RT contract cares: it is
        // exactly what `Vec::push` past capacity does, and it can block in
        // the allocator just as hard as a fresh `alloc`.
        bump(&ALLOCS, 1);
        bump(&BYTES, new_size as u64);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// `Cell::set` through a `LocalKey`, tolerating TLS teardown: `try_with`
/// returns `Err` once the key is being destroyed, and a lost count there is
/// strictly better than a panic inside `dealloc`.
fn bump(key: &'static std::thread::LocalKey<Cell<u64>>, by: u64) {
    let _ = key.try_with(|c| c.set(c.get().wrapping_add(by)));
}

/// What one thread has allocated so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocStats {
    pub allocs: u64,
    pub deallocs: u64,
    pub bytes: u64,
}

impl AllocStats {
    /// Counters for the calling thread, right now.
    pub fn here() -> Self {
        Self {
            allocs: ALLOCS.try_with(Cell::get).unwrap_or(0),
            deallocs: DEALLOCS.try_with(Cell::get).unwrap_or(0),
            bytes: BYTES.try_with(Cell::get).unwrap_or(0),
        }
    }

    /// What happened between `self` (taken earlier) and now.
    pub fn since(&self) -> Self {
        let now = Self::here();
        Self {
            allocs: now.allocs - self.allocs,
            deallocs: now.deallocs - self.deallocs,
            bytes: now.bytes - self.bytes,
        }
    }
}

/// Run `f`, and report what it allocated on this thread.
///
/// The return value is handed back so a measured call can still be checked
/// for correctness — a kernel that allocates nothing because it did nothing
/// is not the result anyone wants.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, AllocStats) {
    let before = AllocStats::here();
    let out = f();
    (out, before.since())
}

/// Run `f` and panic if it allocated or freed anything. The RT gate.
///
/// Only meaningful in a binary that registered [`AllocCounter`]; in one that
/// did not, every count is zero and this passes vacuously — so the test that
/// registers it is where the guarantee lives, and
/// [`counting_is_active`] is how a test proves the guard is armed.
pub fn assert_no_alloc<R>(what: &str, f: impl FnOnce() -> R) -> R {
    let (out, stats) = measure(f);
    assert_eq!(
        (stats.allocs, stats.deallocs),
        (0, 0),
        "{what} touched the allocator: {} allocs / {} frees / {} bytes",
        stats.allocs,
        stats.deallocs,
        stats.bytes
    );
    out
}

/// Whether [`AllocCounter`] is actually the registered global allocator —
/// i.e. whether [`assert_no_alloc`] can fail at all.
///
/// Without this, the RT gate is indistinguishable from a test that forgot to
/// register the allocator, which is the failure mode that makes an
/// allocation guard worse than none.
pub fn counting_is_active() -> bool {
    // `black_box`, and it is load-bearing rather than defensive: without it
    // LLVM deletes this allocation as dead in a release build, the probe
    // reports zero, and `the_guard_is_actually_armed` FAILS — the one test
    // whose job is proving the suite can fail was the only one that broke
    // under `--release`, which is exactly the build an RT allocation check
    // would want to run in.
    let (boxed, stats) = measure(|| std::hint::black_box(Box::new([0u8; 64])));
    std::hint::black_box(&boxed);
    stats.allocs > 0
}
