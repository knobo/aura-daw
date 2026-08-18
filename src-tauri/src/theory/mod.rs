//! `theory` — the Composer's music-theory library.
//!
//! **Purity contract (ruling H-5, and the reason this module exists as its own
//! zone):** nothing in here may `use tauri`, `parking_lot`, `std::fs`,
//! `crate::control` or `crate::audio`. No locks, no I/O, no globals, no
//! `thread_rng`. Every entry point is a pure function of its arguments, and
//! every generator takes an explicit `seed: u64`, so the same call always
//! produces the same notes (ruling H-4).
//!
//! That is not purism. It is what makes the theory suite fast enough to run on
//! every save, makes a generator's output testable without a project, and
//! keeps the library reusable by a future notation surface, a sidecar or a
//! WASM build. The one stateful seam is `crate::control::composer`, which is
//! where `Session`, ops and commands live.
//!
//! ONE dependency on the rest of the crate is deliberate: the generators emit
//! `crate::midi::MidiNote`, the tauri-free wire/document note type. Inventing a
//! parallel note struct here would buy nothing but a conversion at every call
//! site, and `midi::types` is itself pure (serde only).
//!
//! Reading order: [`tpc`] (spelling) → [`scale`] (keys) → [`chord`] →
//! [`analysis`] (function) → [`circle`] / [`palette`] (the two user-facing
//! views) → [`metre`] → the generators ([`progression`], [`voicing`],
//! [`melody`], [`groove`]) → [`harmony`] (the document they all read).
//!
//! Product doc: `docs/backlog/composer-assistant.md`.
//! Plan: `docs/superpowers/plans/2026-08-17-plan-h-composer.md`.

pub mod analysis;
pub mod bass;
pub mod chord;
pub mod circle;
pub mod groove;
pub mod harmony;
pub mod melody;
pub mod metre;
pub mod palette;
pub mod progression;
pub mod rng;
pub mod scale;
pub mod tpc;
pub mod voicing;

pub use analysis::{analyze, Analysis, Function};
pub use chord::{diatonic_chord, diatonic_chords, Chord, ChordQuality};
pub use harmony::{ChordSpan, HarmonyDoc, KeySpan};
pub use bass::{bass_notes, BassOptions, BassStyle};
pub use groove::{groove_notes, Genre, GrooveOptions};
pub use melody::{melody_notes, MelodyOptions};
pub use metre::{Grid, Meter};
pub use palette::{palette, NoteRole, Palette};
pub use progression::{Plan, Progression, Slot, SCHEMAS};
pub use scale::{Key, ScaleType};
pub use tpc::{Pitch, Tpc};
pub use voicing::{chord_notes, ChordRhythm, VoicingOptions, VoicingStyle};

/// One thing a generator did, and why it did it — the teaching payload
/// (ruling H-6). Annotations are returned WITH a command result for display;
/// they are never document state, never persisted, and never a field on a
/// clip. A generator that cannot explain itself is not finished.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotation {
    /// Tick span this explains, relative to the clip it belongs to.
    pub tick: u64,
    pub length_ticks: u64,
    /// Short label for a chip or a badge: `"V7"`, `"tresillo"`, `"sequence"`.
    pub label: String,
    /// One sentence, concrete about the notes involved.
    pub why: String,
}

impl Annotation {
    pub fn new(tick: u64, length_ticks: u64, label: impl Into<String>, why: impl Into<String>) -> Self {
        Self { tick, length_ticks, label: label.into(), why: why.into() }
    }
}

