//! Lock-free state exchange between the control plane and the audio thread.

pub mod triple_buffer;

pub use triple_buffer::{from_slots, triple_buffer, Input, Output};
