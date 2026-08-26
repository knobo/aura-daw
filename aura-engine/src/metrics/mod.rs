//! RT verification and telemetry: an allocation counter that makes "no
//! allocation on the audio thread" a testable claim, and per-block latency /
//! xrun accounting.

pub mod alloc;
pub mod telemetry;

pub use alloc::{assert_no_alloc, counting_is_active, measure, AllocCounter, AllocStats};
pub use telemetry::{Snapshot, Telemetry};
