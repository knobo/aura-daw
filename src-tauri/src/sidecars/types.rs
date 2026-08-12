//! Wire types for the sidecar IPC surface (mirrors
//! `docs/ipc-schemas/sidecar-job.schema.json`, camelCase on the wire).
//!
//! Deliberately Tauri-free so the job manager (`jobs.rs`) can be compiled and
//! unit-tested without the Tauri/WebKit stack.

use serde::{Deserialize, Serialize};

/// Job kind discriminator. v1 kinds: `"stemSplit"` | `"transcribe"`; phase-2
/// kinds (submitted via the open-kind `sidecar_run_job` command, debt D-07)
/// are listed below. Consumers MUST treat unknown kind strings generically
/// (progress + log only) — see sidecar-job-v2.schema.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    StemSplit,
    Transcribe,
    /// ACE-Step local music generation (text -> audio).
    AceStepGenerate,
    /// ACE-Step inpainting/repaint of a region of existing audio.
    AceStepRepaint,
    /// ACE-Step audio2audio restyling.
    AceStepAudio2Audio,
    /// ElevenLabs Music API (cloud) — worker script does the HTTP calls, so
    /// cloud jobs share the exact NDJSON lifecycle of local ones.
    ElevenLabsMusic,
    /// Anticipatory Music Transformer conditioned MIDI infilling.
    AmtInfill,
    /// Stable Audio Open multisample generation -> SFZ instrument.
    StableAudioSfz,
    /// Hum/voice recording -> MIDI melody (hum-to-song pipeline, basic-pitch
    /// style monophonic transcription tuned for hummed input).
    HumToMidi,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::StemSplit => "stemSplit",
            JobKind::Transcribe => "transcribe",
            JobKind::AceStepGenerate => "aceStepGenerate",
            JobKind::AceStepRepaint => "aceStepRepaint",
            JobKind::AceStepAudio2Audio => "aceStepAudio2Audio",
            JobKind::ElevenLabsMusic => "elevenLabsMusic",
            JobKind::AmtInfill => "amtInfill",
            JobKind::StableAudioSfz => "stableAudioSfz",
            JobKind::HumToMidi => "humToMidi",
        }
    }

    /// Parse a wire kind string. None for unknown kinds — the applier rejects
    /// them (observers must instead tolerate unknowns, per D-06/D-07).
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "stemSplit" => JobKind::StemSplit,
            "transcribe" => JobKind::Transcribe,
            "aceStepGenerate" => JobKind::AceStepGenerate,
            "aceStepRepaint" => JobKind::AceStepRepaint,
            "aceStepAudio2Audio" => JobKind::AceStepAudio2Audio,
            "elevenLabsMusic" => JobKind::ElevenLabsMusic,
            "amtInfill" => JobKind::AmtInfill,
            "stableAudioSfz" => JobKind::StableAudioSfz,
            "humToMidi" => JobKind::HumToMidi,
            _ => return None,
        })
    }
}

/// Lifecycle state (`"queued"|"running"|"done"|"error"|"cancelled"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Done => "done",
            JobState::Error => "error",
            JobState::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(name: &str) -> bool {
        matches!(name, "done" | "error" | "cancelled")
    }
}

/// Mirrors `submitStemSplit` in the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StemSplitRequest {
    /// Absolute path to the source audio file (wav/flac/mp3).
    pub input_path: String,
    /// Demucs model name, e.g. "htdemucs".
    pub model: Option<String>,
    /// Requested stems; default all: vocals/drums/bass/other.
    pub stems: Option<Vec<String>>,
    /// Output directory; defaults next to the input (`<dir>/stems/<name>/`).
    pub output_dir: Option<String>,
}

/// Mirrors `submitTranscribe` in the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscribeRequest {
    /// Absolute path to the source audio file (16 kHz mono is prepared
    /// internally if needed).
    pub input_path: String,
    /// whisper.cpp model file name/path, e.g. "ggml-base.en.bin".
    pub model: Option<String>,
    /// ISO-639-1 language hint, e.g. "en", "no"; None = auto-detect.
    pub language: Option<String>,
    /// Also extract per-word pitch estimates (vocal-to-pitch path).
    pub with_pitch: Option<bool>,
}

/// Streamed per-job event. Mirrors `event` in the schema; also the payload of
/// the `sidecar://progress|done|error` app events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase", tag = "type")]
pub enum SidecarEvent {
    /// Periodic progress: fraction in [0.0, 1.0], free-form stage label.
    Progress {
        job_id: String,
        progress: f64,
        stage: String,
    },
    /// A line of sidecar log output (stderr / non-protocol stdout), for UI
    /// consoles. Channel-only; not re-emitted as an app event.
    Log { job_id: String, line: String },
    /// Terminal success; `result` matches the job kind's result schema.
    Done {
        job_id: String,
        result: serde_json::Value,
    },
    /// Terminal failure.
    Error { job_id: String, message: String },
}

/// Mirrors `status` in the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarJobStatus {
    pub job_id: String,
    /// "stemSplit" | "transcribe"
    pub kind: String,
    /// "queued" | "running" | "done" | "error" | "cancelled"
    pub state: String,
    /// [0.0, 1.0]
    pub progress: f64,
    pub stage: String,
    /// Present when state == "done".
    pub result: Option<serde_json::Value>,
    /// Present when state == "error".
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_kind_wire_roundtrip_and_unknown_rejection() {
        for k in [
            JobKind::StemSplit,
            JobKind::Transcribe,
            JobKind::AceStepGenerate,
            JobKind::AceStepRepaint,
            JobKind::AceStepAudio2Audio,
            JobKind::ElevenLabsMusic,
            JobKind::AmtInfill,
            JobKind::StableAudioSfz,
            JobKind::HumToMidi,
        ] {
            assert_eq!(JobKind::from_wire(k.as_str()), Some(k));
        }
        assert_eq!(JobKind::from_wire("mysteryModel"), None);
    }
}
