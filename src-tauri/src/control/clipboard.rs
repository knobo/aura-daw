//! The `application/x-aura-clips` clipboard payload: what a multi-clip copy
//! puts on the OS clipboard, and what a paste turns back into document rows.
//!
//! MIDI clips travel BY VALUE (their notes are the content, and they are
//! small); audio clips travel BY REFERENCE (a wav is megabytes — the
//! clipboard is not an asset store). See the plan's scope ruling B for the
//! three-step asset resolution a paste performs, and scope ruling C for why
//! the MIME name rides inside the envelope instead of being an OS clipboard
//! flavor.
//!
//! WIRE DOMAIN (fix round 1, Critical-1): every tick<->sample number in
//! [`AuraClipsPayload`] — `anchor_samples`/`anchor_ticks` and each clip's
//! `offset_from_anchor_*` — is computed against
//! [`crate::midi::NOMINAL_SAMPLE_RATE`] (48kHz), the SAME nominal map
//! `ProjectSnapshot`'s section table is built from
//! (`midi::build_tempo_map_state`), never the live device rate
//! (`SharedRt::sample_rate`). The document must convert identically
//! regardless of which output device happens to be open — a copy made
//! while a 96kHz interface is attached must produce the exact same anchor
//! math as one made at 44.1kHz. Task 9's paste MUST resolve this payload's
//! ticks/samples through the same nominal map, not the engine rate.
//!
//! TRUST BOUNDARY (fix round 1). A payload arrives as text off the OS
//! clipboard, so nothing in it is a guarantee — not note ids, not paths, not
//! ppq, not sample windows. `clips_copy` producing well-formed payloads is a
//! property of AURA's own copy, never of the input `clips_paste` receives.
//! Everything the payload asserts is re-derived or refused in Phase 2,
//! BEFORE the transaction opens, so no payload can be fatal to the rest of
//! a paste.
//!
//! Known limitations, deliberate:
//! - A file copied into the project by Phase 2 is NOT removed when the paste
//!   is undone (and, if the commit itself fails, is orphaned with no clip
//!   referencing it). Undo restores the document, not the asset directory.
//! - Paste-to-new-tracks can produce two rows named `"<name> copy"` when one
//!   source row is split by kind. Unreachable from an AURA-produced payload.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::op::{Op, TxMeta};
use super::ops;
use super::ControlPlane;
use crate::midi::{TempoMap, NOMINAL_SAMPLE_RATE};

pub const AURA_CLIPS_MIME: &str = "application/x-aura-clips";
pub const AURA_CLIPS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuraClipsPayload {
    /// Always `AURA_CLIPS_MIME`. The MIME name rides INSIDE the envelope
    /// because the OS clipboard slot reachable from a Tauri v2/WebKitGTK
    /// app is plain text — arbitrary flavors are not available (scope
    /// ruling C).
    pub mime: String,
    pub schema_version: u32,
    /// The anchor: the leftmost selected clip's start, in BOTH units, so a
    /// paste can offset audio in samples and MIDI in ticks without
    /// re-deriving either.
    pub anchor_samples: u64,
    pub anchor_ticks: u64,
    pub ppq: u32,
    /// Absolute path of the source project's .aura dir, or null when the
    /// source session had none. Diagnostic + the base for resolving
    /// `source_path` on the other side.
    pub source_project_dir: Option<String>,
    pub clips: Vec<ClipboardClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ClipboardClip {
    Audio {
        name: String,
        source_track_id: String,
        source_track_name: String,
        /// Signed: a clip left of the anchor is impossible (the anchor IS
        /// the leftmost) but the type stays signed so a future anchor rule
        /// does not need a format change.
        offset_from_anchor_samples: i64,
        length_samples: u64,
        /// Project-relative, POSIX separators — resolved against the target
        /// project first.
        source_path: String,
        /// Absolute path at copy time; null when no project was open.
        source_abs_path: Option<String>,
        source_channels: u16,
        source_sample_rate: u32,
        source_length_samples: u64,
        offset_samples: u64,
        gain_db: f64,
        fade_in_samples: u64,
        fade_out_samples: u64,
    },
    Midi {
        name: String,
        source_track_id: String,
        source_track_name: String,
        offset_from_anchor_ticks: i64,
        length_ticks: u64,
        content_length_ticks: Option<u64>,
        /// By VALUE, with every `note_id` zeroed to the mint sentinel —
        /// a copy mints fresh ids (round-2 §2.1, ADR 0001).
        notes: Vec<crate::midi::types::MidiNote>,
    },
}

impl ClipboardClip {
    /// The clip's name — what a [`SkippedClip`] report names it by.
    fn name(&self) -> &str {
        match self {
            ClipboardClip::Audio { name, .. } | ClipboardClip::Midi { name, .. } => name,
        }
    }

    /// The id of the track the clip was copied FROM: a paste's default
    /// target (requirement 2 — clips land back on their own rows) and the
    /// grouping key for paste-to-new-tracks.
    fn source_track_id(&self) -> &str {
        match self {
            ClipboardClip::Audio { source_track_id, .. }
            | ClipboardClip::Midi { source_track_id, .. } => source_track_id,
        }
    }

    fn source_track_name(&self) -> &str {
        match self {
            ClipboardClip::Audio { source_track_name, .. }
            | ClipboardClip::Midi { source_track_name, .. } => source_track_name,
        }
    }

    fn is_midi(&self) -> bool {
        matches!(self, ClipboardClip::Midi { .. })
    }

    /// The `TrackState::kind` this clip can live on. A track is single-kind
    /// everywhere else in the document (`control/import.rs:250` refuses an
    /// audio clip on a non-audio row, `control/hum.rs:201` a MIDI clip on a
    /// non-MIDI one); a paste is the one path a FOREIGN document can reach,
    /// so it enforces both halves of that rule rather than only the MIDI one.
    fn required_track_kind(&self) -> &'static str {
        if self.is_midi() {
            "midi"
        } else {
            "audio"
        }
    }
}

impl ControlPlane {
    /// Build the `application/x-aura-clips` payload for an explicit set of
    /// clips. A pure READ: locks the session once, resolves every id,
    /// computes the anchor, and returns — no commit, no op, no rev bump, no
    /// lazy resync/`ensure_project`. Breaking that would violate the Figma
    /// invariant (§7-test-4): copy must never be observable as a document
    /// edit.
    ///
    /// Unknown ids are rejected outright (`Err("unknown clip: {id}")` on the
    /// first miss) rather than silently skipped — a copy silently dropping
    /// part of what the frontend asked for would be a worse surprise than a
    /// loud error, and the frontend only ever asks for ids it just read from
    /// a live selection.
    pub fn clips_copy(
        &self,
        audio_clip_ids: Vec<String>,
        midi_clip_ids: Vec<String>,
    ) -> Result<AuraClipsPayload, String> {
        let session = self.session.lock();
        let store = &session.store;
        let midi = &session.midi;

        let audio_clips: Vec<&crate::audio::types::Clip> = audio_clip_ids
            .iter()
            .map(|id| {
                store
                    .clips
                    .iter()
                    .find(|c| c.id.as_str() == id.as_str())
                    .ok_or_else(|| format!("unknown clip: {id}"))
            })
            .collect::<Result<_, String>>()?;
        let midi_clips: Vec<&crate::midi::types::MidiClip> = midi_clip_ids
            .iter()
            .map(|id| {
                midi.clips
                    .iter()
                    .find(|c| c.id.as_str() == id.as_str())
                    .ok_or_else(|| format!("unknown clip: {id}"))
            })
            .collect::<Result<_, String>>()?;

        // Same tick<->sample bijection `ProjectSnapshot`'s section table is
        // built from (round-2 §3.3) — ADR 0006: the frontend does no time
        // math, so the anchor's cross-unit resolution happens here. Fixed
        // (nominal) rate, NOT the live device rate (fix round 1,
        // Critical-1): a copy's anchor/offset numbers must not depend on
        // which audio interface happens to be attached — see this module's
        // WIRE DOMAIN doc above.
        let tempo = TempoMap::new(midi.ppq, midi.tempo_events.clone(), NOMINAL_SAMPLE_RATE)
            .map_err(|e| format!("clips_copy: tempo map: {e}"))?;

        let midi_starts_samples: Vec<u64> =
            midi_clips.iter().map(|c| tempo.tick_to_samples(c.timeline_start_ticks)).collect();

        let anchor_samples = audio_clips
            .iter()
            .map(|c| c.timeline_start_samples)
            .chain(midi_starts_samples.iter().copied())
            .min()
            .unwrap_or(0);
        let anchor_ticks = tempo.samples_to_tick(anchor_samples);

        let track_name = |track_id: &crate::ids::TrackId| -> String {
            store.tracks.iter().find(|t| &t.id == track_id).map(|t| t.name.clone()).unwrap_or_default()
        };

        let mut clips = Vec::with_capacity(audio_clips.len() + midi_clips.len());
        for c in &audio_clips {
            let source_abs_path =
                store.project_dir.as_ref().map(|d| d.join(&c.source_path).to_string_lossy().into_owned());
            clips.push(ClipboardClip::Audio {
                name: c.name.clone(),
                source_track_id: c.track_id.to_string(),
                source_track_name: track_name(&c.track_id),
                offset_from_anchor_samples: c.timeline_start_samples as i64 - anchor_samples as i64,
                length_samples: c.length_samples,
                source_path: c.source_path.clone(),
                source_abs_path,
                source_channels: c.source_channels,
                source_sample_rate: c.source_sample_rate,
                source_length_samples: c.source_length_samples,
                offset_samples: c.offset_samples,
                gain_db: c.gain_db,
                fade_in_samples: c.fade_in_samples,
                fade_out_samples: c.fade_out_samples,
            });
        }
        for c in &midi_clips {
            let notes = c
                .notes
                .iter()
                .cloned()
                .map(|mut n| {
                    n.note_id = crate::ids::NoteId(0);
                    n
                })
                .collect();
            clips.push(ClipboardClip::Midi {
                name: c.name.clone(),
                source_track_id: c.track_id.to_string(),
                source_track_name: track_name(&c.track_id),
                offset_from_anchor_ticks: c.timeline_start_ticks as i64 - anchor_ticks as i64,
                length_ticks: c.length_ticks,
                content_length_ticks: c.content_length_ticks,
                notes,
            });
        }

        Ok(AuraClipsPayload {
            mime: AURA_CLIPS_MIME.to_string(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples,
            anchor_ticks,
            ppq: midi.ppq,
            source_project_dir: store.project_dir.as_ref().map(|d| d.display().to_string()),
            clips,
        })
    }
}

// ---------------------------------------------------------------------------
// Paste
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteRequest {
    pub payload: AuraClipsPayload,
    /// Where the anchor lands: the playhead, in samples.
    pub at_samples: u64,
    /// Paste-to-new-tracks: one fresh track per DISTINCT source track,
    /// named "<sourceTrackName> copy", clips retargeted to the new rows.
    #[serde(default)]
    pub to_new_tracks: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteResult {
    pub audio_clips: Vec<crate::audio::types::Clip>,
    pub midi_clips: Vec<crate::midi::types::MidiClip>,
    pub created_tracks: Vec<crate::audio::types::TrackState>,
    pub skipped: Vec<SkippedClip>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedClip {
    pub name: String,
    pub reason: String,
}

/// What [`resolve_audio_asset`] decided about ONE audio clip's file, all of
/// it settled BEFORE the transaction opens (scope ruling B, round-2 §4.4:
/// prepare outside, commit inside — no I/O ever runs under the session lock).
#[derive(Debug, Clone)]
enum ResolvedAsset {
    /// The path already resolves inside this project: reference it in place,
    /// no bytes moved.
    InPlace(String),
    /// The file was copied into `<project>/audio/` under a fresh name; this
    /// is the new project-relative POSIX path.
    Copied(String),
}

/// One clip that survived validation, paired with whatever Phase 2 had to
/// settle for it. Owns its `ClipboardClip` (moved out of the payload) so the
/// transaction closure never indexes back into a parallel vec.
struct Planned {
    clip: ClipboardClip,
    /// `Some` for audio clips only — MIDI travels by value, it has no asset.
    /// The project-relative path the clip will reference, and the `SourceId`
    /// it will carry (see [`ControlPlane::clips_paste`]'s Phase 2 for why the
    /// id is decided out here rather than left to `assign_source_ids`).
    audio: Option<(String, crate::ids::SourceId)>,
}

/// Ticks are ppq-RELATIVE. ppq is read from `project.json`
/// (`midi/persist.rs:320`, `unwrap_or(DEFAULT_PPQ)`) and is also RUNTIME-
/// MUTABLE through `Op::TempoSet` (`control/session.rs:612`), so a payload
/// off the OS clipboard is not structurally at this session's resolution —
/// and this session's resolution is not even fixed for the life of a paste. Rescale rather than refuse: ppq is a resolution unit, not a
/// musical difference, exactly as the nominal-rate map converts samples
/// without refusing a foreign device rate. Exact whenever one ppq divides
/// the other (the 480/960 case that actually occurs); otherwise rounded to
/// the nearest tick — at 960 ppq / 120 bpm / 48 kHz a tick is 25 samples, so
/// the worst case is half of that, ~12 samples (~0.26 ms). Inaudible, but
/// NOT sub-sample.
///
/// `None` on overflow — the caller skips that clip rather than wrapping a
/// note into the far future.
fn rescale_ticks(value: u64, from_ppq: u32, to_ppq: u32) -> Option<u64> {
    if from_ppq == to_ppq {
        return Some(value);
    }
    let from = from_ppq as u128;
    let scaled = (value as u128) * (to_ppq as u128) + from / 2;
    u64::try_from(scaled / from).ok()
}

/// A negative placement is impossible on AURA's timeline (time starts at 0),
/// so a paste near the very start clamps rather than wrapping around u64.
#[inline]
fn clamp0(v: i64) -> u64 {
    v.max(0) as u64
}

/// Scope ruling B's three-step asset resolution for ONE audio clip, run
/// OUTSIDE the transaction. `Err` is the human-readable skip reason: a clip
/// this machine cannot resolve is reported, never fatal to the rest of the
/// paste and never silently dropped.
///
/// 1. `<project>/<source_path>` exists → reference it in place (a
///    same-project paste moves no bytes; the CALLER then gives the clip the
///    `SourceId` this project already uses for that path, so the engine's
///    per-source decode cache is shared rather than filled twice).
/// 2. else the payload's absolute path exists AND a project is open → copy
///    into `<project>/audio/<uuid>-<file name>` (never overwriting) and use
///    the new project-relative path.
/// 3. else skip, with the reason that actually applied.
///
/// The tail case — no absolute path in the payload and NEITHER session has a
/// project dir — is not a failure: there is no project to resolve against on
/// either side (a scratch, unsaved document), so the relative reference is
/// exactly as good here as it was there and travels verbatim. An absolute
/// path in the payload, by contrast, is a positive claim about a file, and a
/// claim that does not hold is reported.
fn resolve_audio_asset(
    project_dir: Option<&Path>,
    payload_project_dir: Option<&str>,
    name: &str,
    source_path: &str,
    source_abs_path: Option<&str>,
) -> Result<ResolvedAsset, String> {
    // `source_path` arrives from the OS clipboard, so it is attacker-
    // controlled. `Path::join` lets an ABSOLUTE path replace the base
    // entirely and a `..` segment walks out of the project, and a clip
    // carrying either gets an empty `SourceId` from the project's own L-1
    // normalization — which the engine's H-3 rule then warn-skips, leaving a
    // permanently SILENT clip that was never reported as skipped. Refuse it
    // here, through the project's own normalizer so the two rules cannot
    // drift apart.
    let source_path = &crate::audio::project::normalize_source_path(source_path)
        .map_err(|e| format!("unusable source path for {name}: {e}"))?;
    if let Some(dir) = project_dir {
        if dir.join(source_path).is_file() {
            return Ok(ResolvedAsset::InPlace(source_path.to_string()));
        }
    }
    if let Some(abs) = source_abs_path {
        let src = Path::new(abs);
        if !src.is_file() {
            return Err(format!("missing audio source: {abs}"));
        }
        let Some(dir) = project_dir else {
            return Err(format!("no project open — cannot import {name}"));
        };
        let audio_dir = dir.join("audio");
        std::fs::create_dir_all(&audio_dir)
            .map_err(|e| format!("cannot import {name}: {e}"))?;
        let file_name = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "clip.wav".into());
        let unique = format!("{}-{}", uuid::Uuid::new_v4(), file_name);
        let dst = audio_dir.join(&unique);
        if dst.exists() {
            // Practically unreachable (a fresh uuid), but a copy must never
            // overwrite an asset another clip is already playing.
            return Err(format!("cannot import {name}: {} already exists", dst.display()));
        }
        std::fs::copy(src, &dst).map_err(|e| format!("cannot import {name}: {e}"))?;
        return Ok(ResolvedAsset::Copied(format!("audio/{unique}")));
    }
    if project_dir.is_none() && payload_project_dir.is_none() {
        return Ok(ResolvedAsset::InPlace(source_path.to_string()));
    }
    Err(format!("missing audio source: {source_path}"))
}

impl ControlPlane {
    /// Paste an `application/x-aura-clips` payload with its anchor at
    /// `at_samples` — scope ruling A: ONE `commit`, hence ONE `Committed` and
    /// ONE `HistoryEntry`, composing the EXISTING ops (`add_track_tx`'s
    /// `Op::TrackAdd`, `Op::ClipAdd`, `Op::MidiClipAdd`). No new op kind, no
    /// `OP_FORMAT_VERSION` bump: one Ctrl+Z takes the whole paste — clips AND
    /// any tracks it created — back out together.
    ///
    /// Three phases, and the phase boundary is the point:
    ///
    /// 1. **Validate the envelope** — no lock, no I/O.
    /// 2. **Resolve** — a short lock for the store snapshot, then track
    ///    validation and scope ruling B's asset resolution (the only I/O in
    ///    the whole method) with NO lock held. A clip that cannot be placed
    ///    is moved to `skipped` here, with the reason.
    /// 3. **Commit** — one transaction that only builds rows and applies
    ///    ops. Everything it could have failed on was decided in phase 2, so
    ///    the closure cannot panic and (barring a concurrent document swap)
    ///    cannot fail: no unwrap, no indexing, no I/O inside.
    ///
    /// A paste where EVERY clip skipped commits nothing at all (no rev bump,
    /// no history entry) and is still `Ok` — the user asked for a paste and
    /// gets a precise report of why nothing landed.
    pub fn clips_paste(&self, req: PasteRequest) -> Result<PasteResult, String> {
        let PasteRequest { payload, at_samples, to_new_tracks } = req;

        // ---- Phase 1: the envelope ---------------------------------------
        if payload.mime != AURA_CLIPS_MIME {
            return Err(format!(
                "not an AURA clips payload: {:?} (want {AURA_CLIPS_MIME})",
                payload.mime
            ));
        }
        if payload.schema_version > AURA_CLIPS_SCHEMA_VERSION {
            return Err(format!(
                "clipboard payload schema {} is newer than this build understands ({AURA_CLIPS_SCHEMA_VERSION})",
                payload.schema_version
            ));
        }
        // Every tick in the payload is relative to this; 0 is not a
        // resolution, it is a divide-by-zero waiting in `rescale_ticks`.
        if payload.ppq == 0 {
            return Err("clipboard payload declares ppq 0".to_string());
        }

        // ---- Phase 2: resolve, outside the transaction -------------------
        // One short lock for everything the resolution needs; dropped before
        // any filesystem call (round-2 §4.4).
        let (project_dir, tracks, mut known_sources) = {
            let session = self.session.lock();
            let tracks: Vec<(String, String)> = session
                .store
                .tracks
                .iter()
                .map(|t| (t.id.to_string(), t.kind.clone()))
                .collect();
            // Every (path -> SourceId) this project already knows. The
            // engine's decode cache is keyed by SOURCE, not by clip, so an
            // in-place paste that minted a second id for the same file would
            // decode and cache that wav twice.
            // Keyed by the NORMALIZED path, because that is what
            // `resolve_audio_asset` returns and therefore what the lookup
            // compares against — a stored `"audio/./x.wav"` must not miss and
            // mint a second id for a file this project already has. A stored
            // path that will not normalize (legacy, L-1) is dropped: it could
            // never match a normalized key anyway.
            let known_sources: Vec<(String, crate::ids::SourceId)> = session
                .store
                .clips
                .iter()
                .filter(|c| !c.source_id.as_str().is_empty())
                .filter_map(|c| {
                    crate::audio::project::normalize_source_path(&c.source_path)
                        .ok()
                        .map(|path| (path, c.source_id.clone()))
                })
                .collect();
            (session.store.project_dir.clone(), tracks, known_sources)
        };
        let at_samples_i = i64::try_from(at_samples).unwrap_or(i64::MAX);

        let payload_dir = payload.source_project_dir;
        let payload_ppq = payload.ppq;
        let mut skipped: Vec<SkippedClip> = Vec::new();
        let mut planned: Vec<Planned> = Vec::new();
        for clip in payload.clips {
            // Target-track validation (skipped for paste-to-new-tracks: the
            // tracks do not exist yet, and a track this paste creates itself
            // cannot fail to match).
            if !to_new_tracks {
                let src = clip.source_track_id();
                match tracks.iter().find(|(id, _)| id == src) {
                    None => {
                        skipped.push(SkippedClip {
                            name: clip.name().to_string(),
                            reason: format!("source track {src} is not in this project"),
                        });
                        continue;
                    }
                    Some((_, kind)) if kind != clip.required_track_kind() => {
                        skipped.push(SkippedClip {
                            name: clip.name().to_string(),
                            reason: format!(
                                "track {src} is kind \"{kind}\", but this clip needs a \"{}\" track",
                                clip.required_track_kind()
                            ),
                        });
                        continue;
                    }
                    Some(_) => {}
                }
            }
            let mut clip = clip;
            // Asset resolution (audio only) — the one place this method
            // touches the filesystem, and it is outside the transaction.
            let audio = match &mut clip {
                ClipboardClip::Audio {
                    name, source_path, source_abs_path, offset_samples, length_samples, ..
                } => {
                    // `offset_samples` reaches the RT thread as
                    // `clip.offset + rel` (`audio/mixer.rs:60`): a payload
                    // claiming an absurd window is a debug panic ON THE AUDIO
                    // THREAD and a wrap in release. Refused at the boundary.
                    if offset_samples.checked_add(*length_samples).is_none() {
                        skipped.push(SkippedClip {
                            name: name.clone(),
                            reason: format!(
                                "sample window out of range: offset {offset_samples} + length {length_samples}"
                            ),
                        });
                        continue;
                    }
                    match resolve_audio_asset(
                        project_dir.as_deref(),
                        payload_dir.as_deref(),
                        name,
                        source_path,
                        source_abs_path.as_deref(),
                    ) {
                        // In place: inherit the identity this project already
                        // uses for that file (including one an earlier clip
                        // in THIS paste just established), so the wav is
                        // decoded once. A file copied in is a genuinely new
                        // asset and gets its own id. Either way the id is
                        // non-empty, so `assign_source_ids` — a legacy-LOAD
                        // migration that mints UUIDv5-of-path, not the
                        // identity live clips carry — is never involved.
                        Ok(ResolvedAsset::InPlace(path)) => {
                            let id = known_sources
                                .iter()
                                .find(|(p, _)| p == &path)
                                .map(|(_, id)| id.clone())
                                .unwrap_or_else(|| {
                                    let id = crate::ids::SourceId::mint();
                                    known_sources.push((path.clone(), id.clone()));
                                    id
                                });
                            Some((path, id))
                        }
                        Ok(ResolvedAsset::Copied(path)) => {
                            Some((path, crate::ids::SourceId::mint()))
                        }
                        Err(reason) => {
                            skipped.push(SkippedClip { name: name.clone(), reason });
                            continue;
                        }
                    }
                }
                ClipboardClip::Midi { notes, .. } => {
                    // PASTE IS THE TRUST BOUNDARY. `clips_copy` zeroes every
                    // note id, but a payload off the OS clipboard need not
                    // have come from `clips_copy` at all: duplicate ids would
                    // make `ensure_note_ids` FAIL the whole transaction (one
                    // bad clip taking every good one with it), and kept ids
                    // with a fresh watermark would let a later mint collide
                    // with a live note (ADR 0001). Re-mint unconditionally.
                    //
                    // Note ids are ppq-independent, so this belongs out here.
                    // The tick RESCALE does not: see the Phase 3 MIDI arm.
                    for n in notes.iter_mut() {
                        n.note_id = crate::ids::NoteId(0);
                    }
                    None
                }
            };
            planned.push(Planned { clip, audio });
        }

        // Paste-to-new-tracks: one row per DISTINCT source track, in
        // first-seen order. Built from the SURVIVING clips only, so a
        // skipped clip never leaves an empty track behind.
        //
        // The grouping key is (source track, kind), not the source track
        // alone: a track is single-kind everywhere the app itself writes
        // one, so for any payload AURA produced this is the plan's "one per
        // distinct source track" exactly — but a foreign or hand-edited
        // payload claiming both kinds on one row would otherwise force one
        // kind on the new track and strand the other half in `skipped`.
        let mut groups: Vec<(String, bool, String, String)> = Vec::new();
        if to_new_tracks {
            for p in &planned {
                let (src, midi) = (p.clip.source_track_id(), p.clip.is_midi());
                if !groups.iter().any(|(id, m, _, _)| id == src && *m == midi) {
                    groups.push((
                        src.to_string(),
                        midi,
                        format!("{} copy", p.clip.source_track_name()),
                        p.clip.required_track_kind().to_string(),
                    ));
                }
            }
        }

        let mut result =
            PasteResult { audio_clips: vec![], midi_clips: vec![], created_tracks: vec![], skipped };
        if planned.is_empty() {
            // Nothing to place: no transaction at all — no rev bump, no
            // history entry, just the report.
            return Ok(result);
        }

        // ---- Phase 3: ONE transaction ------------------------------------
        self.commit(TxMeta::user("paste clips"), |tx| {
            // Derived INSIDE the transaction: tempo events are user-editable
            // at runtime, so a map built from the Phase-2 snapshot could place
            // MIDI clips by a tempo the document no longer has. (`ppq` above
            // is safe to read outside: it changes only when a project is
            // LOADED, which replaces the whole session.)
            //
            // NOMINAL rate, never the live device rate — this module's WIRE
            // DOMAIN note: the payload's tick/sample numbers were built
            // against the 48kHz nominal map, so the paste must resolve them
            // through the same one or a copy made on one interface would land
            // elsewhere on another.
            // ONE read, feeding BOTH the tick map and the payload rescale
            // below. They must never come from two reads: `Op::TempoSet`
            // writes `session.midi.ppq` (`control/session.rs:612`) from the
            // live `set_tempo_map` command, so a paste that decided "no
            // rescale needed" against one ppq and then converted
            // `at_samples` against another would place every clip at the
            // wrong musical position, silently and with no skip report.
            let tx_ppq = tx.midi().ppq;
            let tempo =
                TempoMap::new(tx_ppq, tx.midi().tempo_events.clone(), NOMINAL_SAMPLE_RATE)
                    .map_err(|e| format!("clips_paste: tempo map: {e}"))?;
            let at_ticks = i64::try_from(tempo.samples_to_tick(at_samples)).unwrap_or(i64::MAX);

            let mut created: Vec<(String, bool, crate::ids::TrackId)> = Vec::new();
            for (src, midi, name, kind) in groups {
                let track = ops::add_track_tx(tx, Some(name), Some(kind))?;
                created.push((src, midi, track.id.clone()));
                result.created_tracks.push(track);
            }
            for Planned { clip, audio } in planned {
                let target = if to_new_tracks {
                    match created
                        .iter()
                        .find(|(src, midi, _)| src == clip.source_track_id() && *midi == clip.is_midi())
                    {
                        Some((_, _, id)) => id.clone(),
                        None => {
                            // Unreachable: every surviving clip seeded a
                            // group above. Reported rather than unwrapped —
                            // a transaction closure must not panic.
                            result.skipped.push(SkippedClip {
                                name: clip.name().to_string(),
                                reason: format!(
                                    "no new track was created for source track {}",
                                    clip.source_track_id()
                                ),
                            });
                            continue;
                        }
                    }
                } else {
                    crate::ids::TrackId::from(clip.source_track_id())
                };
                // Re-validate against the IN-TRANSACTION view, not phase 2's
                // snapshot: the lock was released in between, so the track
                // could in principle have been removed by another commit.
                let Some(track) = tx.store().tracks.iter().find(|t| t.id == target) else {
                    result.skipped.push(SkippedClip {
                        name: clip.name().to_string(),
                        reason: format!("source track {target} is not in this project"),
                    });
                    continue;
                };
                if track.kind != clip.required_track_kind() {
                    result.skipped.push(SkippedClip {
                        name: clip.name().to_string(),
                        reason: format!(
                            "track {target} is kind \"{}\", but this clip needs a \"{}\" track",
                            track.kind,
                            clip.required_track_kind()
                        ),
                    });
                    continue;
                }
                let lane_id = crate::ids::LaneId::default_for_track(target.as_str());
                match clip {
                    ClipboardClip::Audio {
                        name,
                        offset_from_anchor_samples,
                        length_samples,
                        source_channels,
                        source_sample_rate,
                        source_length_samples,
                        offset_samples,
                        gain_db,
                        fade_in_samples,
                        fade_out_samples,
                        ..
                    } => {
                        let new_clip = crate::audio::types::Clip {
                            id: crate::ids::ClipId::mint(),
                            track_id: target,
                            name,
                            // Phase 2 settled both, for every audio clip that
                            // got this far; the fallback keeps the closure
                            // panic-free either way.
                            source_path: audio.as_ref().map(|(p, _)| p.clone()).unwrap_or_default(),
                            source_id: audio
                                .as_ref()
                                .map(|(_, id)| id.clone())
                                .unwrap_or_else(crate::ids::SourceId::mint),
                            source_channels,
                            source_sample_rate,
                            source_length_samples,
                            timeline_start_samples: clamp0(
                                at_samples_i.saturating_add(offset_from_anchor_samples),
                            ),
                            offset_samples,
                            length_samples,
                            gain_db,
                            fade_in_samples,
                            fade_out_samples,
                            // A paste is a COPY, never an instance (round-2
                            // §2.1): fresh clip id, fresh ContentId.
                            content_id: crate::ids::ContentId::mint(),
                            lane_id,
                        };
                        let index = tx.store().clips.len();
                        tx.apply(Op::ClipAdd { clip: new_clip.clone(), index })?;
                        result.audio_clips.push(new_clip);
                    }
                    ClipboardClip::Midi {
                        name,
                        mut offset_from_anchor_ticks,
                        mut length_ticks,
                        mut content_length_ticks,
                        mut notes,
                        ..
                    } => {
                        // Ticks are ppq-relative, so the rescale has to use
                        // the SAME resolution `tempo` was built from — hence
                        // in here, against `tx_ppq`, not against a pre-lock
                        // snapshot. Pure arithmetic: no I/O, no panic, legal
                        // inside `transact`.
                        if payload_ppq != tx_ppq {
                            let rescaled = (|| {
                                let sign = offset_from_anchor_ticks.signum();
                                let off = rescale_ticks(
                                    offset_from_anchor_ticks.unsigned_abs(),
                                    payload_ppq,
                                    tx_ppq,
                                )?;
                                offset_from_anchor_ticks = i64::try_from(off).ok()? * sign;
                                length_ticks = rescale_ticks(length_ticks, payload_ppq, tx_ppq)?;
                                if let Some(c) = content_length_ticks.as_mut() {
                                    *c = rescale_ticks(*c, payload_ppq, tx_ppq)?;
                                }
                                for n in notes.iter_mut() {
                                    n.tick = u32::try_from(rescale_ticks(
                                        n.tick as u64,
                                        payload_ppq,
                                        tx_ppq,
                                    )?)
                                    .ok()?;
                                    n.length_ticks = u32::try_from(rescale_ticks(
                                        n.length_ticks as u64,
                                        payload_ppq,
                                        tx_ppq,
                                    )?)
                                    .ok()?;
                                }
                                Some(())
                            })();
                            if rescaled.is_none() {
                                result.skipped.push(SkippedClip {
                                    name,
                                    reason: format!(
                                        "ticks do not fit this project's resolution (payload ppq {payload_ppq}, project ppq {tx_ppq})"
                                    ),
                                });
                                continue;
                            }
                        }
                        let mut new_clip = crate::midi::types::MidiClip {
                            id: crate::ids::ClipId::mint(),
                            track_id: target,
                            name,
                            timeline_start_ticks: clamp0(
                                at_ticks.saturating_add(offset_from_anchor_ticks),
                            ),
                            length_ticks,
                            notes,
                            // Phase 2 zeroed every incoming id, so
                            // `ensure_note_ids` below mints the whole clip
                            // from 1 and this watermark is correct by
                            // construction.
                            next_note_id: 1,
                            content_id: crate::ids::ContentId::mint(),
                            lane_id,
                            content_length_ticks,
                        };
                        // Ids minted BEFORE the apply, so the op the journal
                        // records carries the final ones — the exact
                        // `hum.rs::commit_hum_clip` sequence.
                        new_clip.ensure_note_ids()?;
                        let index = tx.midi().clips.len();
                        tx.apply(Op::MidiClipAdd { clip: new_clip.clone(), index })?;
                        result.midi_clips.push(new_clip);
                    }
                }
            }
            Ok(())
        })?;
        Ok(result)
    }
}

/// Build the `application/x-aura-clips` payload for an explicit set of
/// clips — a pure READ. Selection lives in the frontend; the backend is
/// only ever told WHICH clips, never "what is selected". Additive command.
#[tauri::command]
pub fn clips_copy(
    audio_clip_ids: Vec<String>,
    midi_clip_ids: Vec<String>,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<AuraClipsPayload, String> {
    control.clips_copy(audio_clip_ids, midi_clip_ids)
}

/// Paste an `application/x-aura-clips` payload at `atSamples` — ONE
/// transaction (one undo step) composing the existing TrackAdd/ClipAdd/
/// MidiClipAdd ops. Clips whose track or audio source cannot be resolved on
/// this machine are reported in `skipped`, never silently dropped and never
/// fatal to the rest of the paste. Additive command.
#[tauri::command]
pub fn clips_paste(
    request: PasteRequest,
    control: State<'_, Arc<ControlPlane>>,
) -> Result<PasteResult, String> {
    control.clips_paste(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::op::{Op, TxMeta};

    /// Reuses control/mod.rs's harness — a ControlPlane over a test engine
    /// double with the named tracks already slotted.
    ///
    /// `t-1` and `t-3` are AUDIO tracks, `t-2` a MIDI one. The shared harness
    /// builds every row as kind `"audio"` (`testutil::test_track`), so the midi
    /// kind is set here, right after construction: paste has to be able to both
    /// LAND a clip on a matching row and REFUSE one aimed at the other kind, in
    /// both directions, which needs two audio rows and one midi row.
    /// Nothing else in this module depends on the kinds (`derive_slots` slots
    /// every row regardless of kind, and `Op::MidiClipAdd` does no kind
    /// validation of its own — that rule lives in the callers).
    fn plane() -> crate::control::ControlPlane {
        let (cp, _rx, _events) =
            crate::control::tests::test_plane_with_tracks(&["t-1", "t-2", "t-3"]);
        // Fixture setup writes the kind straight into the store rather than
        // through an op. That is fine HERE (it happens before any history
        // exists, so nothing can undo past it) but it is not a pattern to
        // copy into production code, where every field change is an op.
        cp.session().lock().store.tracks[1].kind = "midi".into();
        cp
    }

    #[test]
    fn copy_offsets_are_relative_to_the_leftmost_clip() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut b = crate::audio::types::testutil::test_clip("a-2", "t-3");
            b.timeline_start_samples = 48_000;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::ClipAdd { clip: b, index: 1 })
        })
        .unwrap();

        let p = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();
        assert_eq!(p.mime, AURA_CLIPS_MIME);
        assert_eq!(p.schema_version, AURA_CLIPS_SCHEMA_VERSION);
        assert_eq!(p.anchor_samples, 48_000, "the anchor is the leftmost clip");
        let offsets: Vec<i64> = p
            .clips
            .iter()
            .map(|c| match c {
                ClipboardClip::Audio { offset_from_anchor_samples, .. } => *offset_from_anchor_samples,
                _ => panic!("expected audio"),
            })
            .collect();
        assert_eq!(offsets, vec![48_000, 0], "offsets preserve the arrangement's shape");
    }

    #[test]
    fn copy_strips_note_ids_to_the_mint_sentinel() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::control::tests::dummy_midi_clip("t-1");
            c.notes = vec![crate::midi::types::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: 60,
                velocity: 100,
                channel: 0,
                note_id: crate::ids::NoteId(7),
            }];
            c.next_note_id = 8;
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let id = cp.session().lock().midi.clips[0].id.to_string();

        let p = cp.clips_copy(vec![], vec![id]).unwrap();
        match &p.clips[0] {
            ClipboardClip::Midi { notes, .. } => {
                assert_eq!(notes.len(), 1);
                assert_eq!(
                    notes[0].note_id.0, 0,
                    "a copy carries the mint sentinel — ids are never reused (ADR 0001)"
                );
                assert_eq!(notes[0].key, 60, "the musical content travels by value");
            }
            _ => panic!("expected midi"),
        }
    }

    #[test]
    fn copy_of_a_mixed_selection_anchors_on_the_earliest_clip_in_either_store() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut m = crate::control::tests::dummy_midi_clip("t-2");
            m.timeline_start_ticks = 0; // tick 0 = sample 0, earlier than the audio clip
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd { clip: m, index: 0 })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();

        let p = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        assert_eq!(p.anchor_samples, 0);
        assert_eq!(p.anchor_ticks, 0);
        let audio_off = p.clips.iter().find_map(|c| match c {
            ClipboardClip::Audio { offset_from_anchor_samples, .. } => Some(*offset_from_anchor_samples),
            _ => None,
        });
        assert_eq!(audio_off, Some(96_000));
    }

    /// Fix round 1, Critical-1 regression: the anchor/offset math must be
    /// computed against the NOMINAL 48kHz wire domain (`NOMINAL_SAMPLE_RATE`,
    /// same as `ProjectSnapshot`'s section table), never the live device
    /// rate. A prior version built the conversion `TempoMap` from
    /// `self.shared.sample_rate` — invisible with the test harness's default
    /// 48kHz rate, so this test deliberately overrides it to something else
    /// (96kHz) to unmask the bug class: at 96kHz, ticks convert to DOUBLE
    /// the nominal sample count, which would skew a mixed audio+MIDI
    /// selection's anchor by the rate ratio. The expected numbers below are
    /// the nominal-domain ones (tick 960 @ 120bpm/960ppq/48kHz = 24_000
    /// samples, matching `tempo::tests::v1_equivalent_single_entry_map`) —
    /// they must hold regardless of the device rate override.
    #[test]
    fn copy_anchor_math_is_independent_of_the_live_device_rate() {
        let cp = plane();
        // Override the live device rate AFTER construction — same private
        // field `self.shared` access `control::clipboard` already relies on
        // for production code (this test module is a descendant of
        // `control`, same as the impl above).
        cp.shared.sample_rate.store(96_000, std::sync::atomic::Ordering::Relaxed);

        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut m = crate::control::tests::dummy_midi_clip("t-2");
            m.timeline_start_ticks = 960; // nominal: 24_000 samples @120bpm/960ppq/48kHz
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd { clip: m, index: 0 })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();

        let p = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        assert_eq!(p.anchor_samples, 24_000, "nominal-rate conversion of tick 960, not the 96kHz device rate's 48_000");
        assert_eq!(p.anchor_ticks, 960);
        let audio_off = p.clips.iter().find_map(|c| match c {
            ClipboardClip::Audio { offset_from_anchor_samples, .. } => Some(*offset_from_anchor_samples),
            _ => None,
        });
        assert_eq!(audio_off, Some(72_000), "96_000 - the nominal 24_000 anchor, never negative/skewed");
        let midi_off = p.clips.iter().find_map(|c| match c {
            ClipboardClip::Midi { offset_from_anchor_ticks, .. } => Some(*offset_from_anchor_ticks),
            _ => None,
        });
        assert_eq!(midi_off, Some(0));
    }

    #[test]
    fn copy_rejects_an_unknown_clip_id() {
        let cp = plane();
        let err = cp.clips_copy(vec!["nope".into()], vec![]).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn copy_of_nothing_is_an_empty_payload_not_an_error() {
        let cp = plane();
        let p = cp.clips_copy(vec![], vec![]).unwrap();
        assert!(p.clips.is_empty());
        assert_eq!(p.anchor_samples, 0);
    }

    #[test]
    fn copy_mutates_nothing_and_creates_no_history_step() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })
        })
        .unwrap();
        let (undo_before, redo_before) = cp.history_depths();
        let rev_before = cp.session().lock().rev;

        let _ = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();

        assert_eq!(cp.history_depths(), (undo_before, redo_before), "copy is a READ");
        assert_eq!(cp.session().lock().rev, rev_before, "copy bumps no revision");
    }

    #[test]
    fn payload_round_trips_through_json_in_camel_case() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })
        })
        .unwrap();
        let p = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();
        let s = serde_json::to_string(&p).unwrap();
        eprintln!("AuraClipsPayload wire form: {s}");
        assert!(s.contains("\"schemaVersion\":1"), "{s}");
        assert!(s.contains("\"kind\":\"audio\""), "{s}");
        assert!(s.contains("\"offsetFromAnchorSamples\""), "{s}");
        let back: AuraClipsPayload = serde_json::from_str(&s).unwrap();
        assert_eq!(back.clips.len(), 1);
    }

    // ---- paste (Task 9) ---------------------------------------------------

    fn paste_req(payload: AuraClipsPayload, at_samples: u64, to_new_tracks: bool) -> PasteRequest {
        PasteRequest { payload, at_samples, to_new_tracks }
    }

    /// The plan's central claim: a mixed paste is ONE transaction and ONE
    /// undo entry, and Ctrl+Z takes every pasted clip back out together.
    #[test]
    fn paste_of_a_mixed_selection_is_one_history_entry_that_undoes_wholly() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 0;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let payload = cp.clips_copy(vec!["a-1".into()], vec![mid.clone()]).unwrap();
        let (undo_before, _) = cp.history_depths();

        let res = cp.clips_paste(paste_req(payload, 96_000, false)).unwrap();
        assert_eq!(res.audio_clips.len(), 1);
        assert_eq!(res.midi_clips.len(), 1);
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(
            cp.history_depths().0,
            undo_before + 1,
            "a whole paste is exactly ONE undo step"
        );
        {
            let s = cp.session().lock();
            assert_eq!(s.store.clips.len(), 2);
            assert_eq!(s.midi.clips.len(), 2);
        }

        cp.undo().unwrap();
        let s = cp.session().lock();
        assert_eq!(
            s.store.clips.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
            vec!["a-1".to_string()],
            "one Ctrl+Z removed the whole paste and left the ORIGINAL row, not merely one row"
        );
        assert_eq!(
            s.midi.clips.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
            vec![mid]
        );
    }

    /// Requirement 2, verbatim: every clip lands at the playhead WITH its
    /// individual offset intact, each on the SAME track it came from.
    #[test]
    fn paste_preserves_per_clip_offsets_and_source_tracks() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 48_000;
            let mut b = crate::audio::types::testutil::test_clip("a-2", "t-3");
            b.timeline_start_samples = 72_000;
            tx.apply(Op::ClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::ClipAdd { clip: b, index: 1 })
        })
        .unwrap();
        let payload = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 480_000, false)).unwrap();
        let mut placed: Vec<(String, u64)> = res
            .audio_clips
            .iter()
            .map(|c| (c.track_id.to_string(), c.timeline_start_samples))
            .collect();
        placed.sort();
        assert_eq!(
            placed,
            vec![("t-1".to_string(), 480_000), ("t-3".to_string(), 504_000)],
            "the 24000-sample gap survives the paste, and each clip stayed on its track"
        );
    }

    /// Fresh identities: a paste is a COPY, never an instance (round-2
    /// §2.1's remint rule; instancing is this plan's explicit non-goal).
    #[test]
    fn paste_mints_fresh_clip_content_and_note_ids() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::control::tests::dummy_midi_clip("t-2");
            c.notes = vec![crate::midi::types::MidiNote {
                tick: 0,
                length_ticks: 480,
                key: 64,
                velocity: 90,
                channel: 0,
                note_id: crate::ids::NoteId(3),
            }];
            c.next_note_id = 4;
            tx.apply(Op::MidiClipAdd { clip: c, index: 0 })
        })
        .unwrap();
        let (src_id, src_content) = {
            let s = cp.session().lock();
            (s.midi.clips[0].id.to_string(), s.midi.clips[0].content_id.clone())
        };
        let payload = cp.clips_copy(vec![], vec![src_id.clone()]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        let pasted = &res.midi_clips[0];
        assert_ne!(pasted.id.to_string(), src_id, "fresh clip id");
        assert_ne!(pasted.content_id, src_content, "fresh ContentId — a copy, not an instance");
        assert_eq!(pasted.notes.len(), 1);
        assert!(pasted.notes[0].note_id.0 > 0, "the mint sentinel was resolved to a real id");
        assert!(
            pasted.next_note_id > pasted.notes[0].note_id.0,
            "the watermark is past every minted id"
        );
    }

    /// Requirement 3: paste onto NEW tracks — one fresh track per distinct
    /// source track, in the SAME transaction, so undo removes the tracks too.
    #[test]
    fn paste_to_new_tracks_creates_one_track_per_source_track_in_the_same_transaction() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-2", "t-1"),
                index: 1,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-3", "t-2"),
                index: 2,
            })
        })
        .unwrap();
        let payload = cp
            .clips_copy(vec!["a-1".into(), "a-2".into(), "a-3".into()], vec![])
            .unwrap();
        let tracks_before: Vec<String> =
            cp.session().lock().store.tracks.iter().map(|t| t.id.to_string()).collect();
        let (undo_before, _) = cp.history_depths();

        let res = cp.clips_paste(paste_req(payload, 0, true)).unwrap();
        assert_eq!(res.created_tracks.len(), 2, "two distinct source tracks -> two new tracks");
        assert_eq!(cp.session().lock().store.tracks.len(), tracks_before.len() + 2);
        // the two clips from t-1 share ONE new track
        let new_ids: Vec<String> = res.created_tracks.iter().map(|t| t.id.to_string()).collect();
        assert!(res.audio_clips.iter().all(|c| new_ids.contains(&c.track_id.to_string())));
        assert_eq!(cp.history_depths().0, undo_before + 1, "still ONE undo step");

        cp.undo().unwrap();
        let s = cp.session().lock();
        assert_eq!(
            s.store.tracks.iter().map(|t| t.id.to_string()).collect::<Vec<_>>(),
            tracks_before,
            "undo removes the tracks the paste created — the ORIGINAL rows survive, by identity"
        );
        assert_eq!(
            s.store.clips.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
            vec!["a-1".to_string(), "a-2".into(), "a-3".into()],
            "and every original clip is still there"
        );
    }

    /// Scope ruling B: a MIDI clip whose source track is gone is skipped and
    /// reported — it does not fail the paste, and it does not silently vanish.
    #[test]
    fn paste_skips_a_clip_whose_source_track_is_missing_and_reports_it() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let mut payload = cp.clips_copy(vec![], vec![mid]).unwrap();
        // rewrite the source track to one this session does not have
        if let ClipboardClip::Midi { source_track_id, .. } = &mut payload.clips[0] {
            *source_track_id = "gone".into();
        }

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.midi_clips.is_empty(), "the clip did not land somewhere else instead");
        assert_eq!(cp.session().lock().midi.clips.len(), 1, "and nothing reached the document");
        assert_eq!(res.skipped.len(), 1);
        // Named specifically enough to tell this reason apart from the
        // wrong-KIND reason, which also says "track".
        assert_eq!(res.skipped[0].name, "riff", "{:?}", res.skipped[0]);
        assert!(
            res.skipped[0].reason.contains("gone") && res.skipped[0].reason.contains("not in this project"),
            "{:?}",
            res.skipped[0]
        );
    }

    /// A MIDI clip may not land on an audio track — the same rule
    /// `midi_add_clip_core` enforces, applied per clip rather than fatally.
    #[test]
    fn paste_skips_a_midi_clip_targeting_an_audio_track() {
        let cp = plane(); // t-1 is the AUDIO track of this fixture
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-1"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let payload = cp.clips_copy(vec![], vec![mid]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.midi_clips.is_empty());
        assert_eq!(res.skipped.len(), 1);
        assert!(res.skipped[0].reason.contains("midi"), "{:?}", res.skipped[0]);
    }

    /// The other half of the kind rule, which the plan's own test list only
    /// covered in the MIDI direction: an AUDIO clip may not land on a MIDI
    /// track either. `control/import.rs:250` refuses exactly this when the
    /// app itself adds an audio clip; a paste is the one path a foreign
    /// document can reach, so it must refuse it too rather than writing a
    /// row the rest of the app cannot produce.
    #[test]
    fn paste_skips_an_audio_clip_targeting_a_midi_track() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-2"),
                index: 0,
            })
        })
        .unwrap();
        let payload = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.audio_clips.is_empty());
        assert_eq!(cp.session().lock().store.clips.len(), 1, "nothing reached the document");
        assert_eq!(res.skipped.len(), 1);
        assert!(
            res.skipped[0].reason.contains("needs a \"audio\" track"),
            "{:?}",
            res.skipped[0]
        );
    }

    /// Paste-to-new-tracks is grouped by (source track, KIND), so a foreign
    /// payload claiming both kinds on one source row gets one new track of
    /// each instead of stranding half the clips in `skipped`.
    #[test]
    fn paste_to_new_tracks_splits_a_mixed_kind_source_row_by_kind() {
        let cp = plane();
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![
                ClipboardClip::Audio {
                    name: "wave".into(),
                    source_track_id: "foreign".into(),
                    source_track_name: "Foreign".into(),
                    offset_from_anchor_samples: 0,
                    length_samples: 1_000,
                    source_path: "audio/x.wav".into(),
                    source_abs_path: None,
                    source_channels: 2,
                    source_sample_rate: 48_000,
                    source_length_samples: 1_000,
                    offset_samples: 0,
                    gain_db: 0.0,
                    fade_in_samples: 0,
                    fade_out_samples: 0,
                },
                ClipboardClip::Midi {
                    name: "notes".into(),
                    source_track_id: "foreign".into(),
                    source_track_name: "Foreign".into(),
                    offset_from_anchor_ticks: 0,
                    length_ticks: 960,
                    content_length_ticks: None,
                    notes: vec![],
                },
            ],
        };

        let res = cp.clips_paste(paste_req(payload, 0, true)).unwrap();
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(res.created_tracks.len(), 2);
        let kinds: Vec<&str> = res.created_tracks.iter().map(|t| t.kind.as_str()).collect();
        assert_eq!(kinds, vec!["audio", "midi"]);
        assert_eq!(res.audio_clips[0].track_id, res.created_tracks[0].id);
        assert_eq!(res.midi_clips[0].track_id, res.created_tracks[1].id);
    }

    /// A session with a project dir on disk, plus the seeded audio clip's
    /// own `SourceId` — what the `InPlace` reuse rule has to match.
    fn plane_with_project(dir: &std::path::Path) -> crate::control::ControlPlane {
        let cp = plane();
        cp.session().lock().store.project_dir = Some(dir.to_path_buf());
        cp
    }

    fn hand_payload(ppq: u32, clips: Vec<ClipboardClip>) -> AuraClipsPayload {
        AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq,
            source_project_dir: None,
            clips,
        }
    }

    fn hand_audio(name: &str, track: &str, source_path: &str) -> ClipboardClip {
        ClipboardClip::Audio {
            name: name.into(),
            source_track_id: track.into(),
            source_track_name: track.into(),
            offset_from_anchor_samples: 0,
            length_samples: 1_000,
            source_path: source_path.into(),
            source_abs_path: None,
            source_channels: 2,
            source_sample_rate: 48_000,
            source_length_samples: 1_000,
            offset_samples: 0,
            gain_db: 0.0,
            fade_in_samples: 0,
            fade_out_samples: 0,
        }
    }

    fn note(tick: u32, length_ticks: u32, id: u32) -> crate::midi::types::MidiNote {
        crate::midi::types::MidiNote {
            tick,
            length_ticks,
            key: 60,
            velocity: 100,
            channel: 0,
            note_id: crate::ids::NoteId(id),
        }
    }

    /// C-1: a FOREIGN payload need not have gone through `clips_copy`, so it
    /// can carry duplicate note ids. `ensure_note_ids` errors on those — and
    /// it runs inside the commit closure, so one bad clip used to abort the
    /// WHOLE paste, taking every good clip with it. Paste is the trust
    /// boundary: incoming ids are re-minted, so no payload can be fatal.
    #[test]
    fn paste_of_a_payload_with_duplicate_note_ids_still_lands_every_clip() {
        let cp = plane();
        let payload = hand_payload(
            960,
            vec![
                ClipboardClip::Midi {
                    name: "bad".into(),
                    source_track_id: "t-2".into(),
                    source_track_name: "T2".into(),
                    offset_from_anchor_ticks: 0,
                    length_ticks: 960,
                    content_length_ticks: None,
                    notes: vec![note(0, 480, 5), note(480, 480, 5)],
                },
                ClipboardClip::Midi {
                    name: "good".into(),
                    source_track_id: "t-2".into(),
                    source_track_name: "T2".into(),
                    offset_from_anchor_ticks: 0,
                    length_ticks: 960,
                    content_length_ticks: None,
                    notes: vec![note(0, 480, 0)],
                },
            ],
        );

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(res.midi_clips.len(), 2, "the good clip is not taken down with the bad one");
        let bad = &res.midi_clips[0];
        let ids: Vec<u32> = bad.notes.iter().map(|n| n.note_id.0).collect();
        assert_eq!(ids, vec![1, 2], "both duplicates were re-minted, distinct");
        assert_eq!(bad.next_note_id, 3);
    }

    /// C-2: incoming note ids are never trusted into the document. Keeping
    /// them while hardcoding `next_note_id: 1` put the watermark BEHIND live
    /// ids, so `assign_incoming_note_ids` would later mint a duplicate of a
    /// note already in the same clip (ADR 0001).
    #[test]
    fn paste_rebases_incoming_note_ids_and_the_watermark() {
        let cp = plane();
        let payload = hand_payload(
            960,
            vec![ClipboardClip::Midi {
                name: "kept-ids".into(),
                source_track_id: "t-2".into(),
                source_track_name: "T2".into(),
                offset_from_anchor_ticks: 0,
                length_ticks: 960,
                content_length_ticks: None,
                notes: vec![note(0, 240, 7), note(240, 240, 9)],
            }],
        );

        let pasted = cp.clips_paste(paste_req(payload, 0, false)).unwrap().midi_clips.remove(0);
        let ids: Vec<u32> = pasted.notes.iter().map(|n| n.note_id.0).collect();
        assert_eq!(ids, vec![1, 2], "the payload's 7 and 9 did not enter the document");
        assert!(
            pasted.next_note_id > ids.iter().copied().max().unwrap(),
            "the watermark is ahead of every live id, so the next mint cannot collide"
        );
    }

    /// C-3: `source_path` is attacker-controlled. An absolute path makes
    /// `Path::join` discard the project base entirely, and a `..` segment
    /// walks out of it — both are exactly what `normalize_source_path`
    /// (L-1) refuses, and a clip carrying one gets an empty `SourceId` and
    /// renders SILENT forever under the engine's H-3 warn-skip. A silent
    /// clip with no skip report is the worst possible outcome.
    #[test]
    fn resolve_audio_asset_refuses_a_source_path_that_escapes_the_project() {
        let root = std::env::temp_dir().join(format!("aura-paste-esc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for bad in ["../secret.wav", "/etc/passwd", "audio/../../secret.wav", "C:\\secret.wav"] {
            let err = resolve_audio_asset(Some(&root), None, "s", bad, None).unwrap_err();
            assert!(err.contains("source path"), "{bad}: {err}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paste_skips_a_clip_whose_source_path_escapes_the_project() {
        let cp = plane();
        let payload = hand_payload(960, vec![hand_audio("evil", "t-1", "../../secret.wav")]);

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.audio_clips.is_empty());
        assert_eq!(cp.session().lock().store.clips.len(), 0, "nothing reached project.json");
        assert_eq!(res.skipped.len(), 1);
        assert!(res.skipped[0].reason.contains("source path"), "{:?}", res.skipped[0]);
    }

    /// I-1: the same file must not be decoded and cached twice. An in-place
    /// paste inherits the existing clip's `SourceId` (the engine's decode
    /// cache is keyed by source, not by clip), while a file copied in is a
    /// genuinely new asset and gets its own.
    #[test]
    fn paste_in_place_reuses_the_existing_source_id_and_a_copy_gets_a_fresh_one() {
        let root = std::env::temp_dir().join(format!("aura-paste-srcid-{}", uuid::Uuid::new_v4()));
        let proj = root.join("B.aura");
        std::fs::create_dir_all(proj.join("audio")).unwrap();
        std::fs::write(proj.join("audio").join("take.wav"), b"RIFF").unwrap();
        let foreign = root.join("A.aura").join("audio");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("other.wav"), b"RIFF2").unwrap();

        let cp = plane_with_project(&proj);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::audio::types::testutil::test_clip("a-1", "t-1");
            c.source_path = "audio/take.wav".into();
            c.source_id = crate::ids::SourceId("the-original-source".into());
            tx.apply(Op::ClipAdd { clip: c, index: 0 })
        })
        .unwrap();

        let in_place = cp.clips_copy(vec!["a-1".into()], vec![]).unwrap();
        let res = cp.clips_paste(paste_req(in_place, 0, false)).unwrap();
        assert_eq!(
            res.audio_clips[0].source_id.as_str(),
            "the-original-source",
            "an in-place paste shares the source identity, so the wav is decoded once"
        );

        let mut foreign_payload =
            hand_payload(960, vec![hand_audio("other", "t-1", "audio/other.wav")]);
        if let ClipboardClip::Audio { source_abs_path, .. } = &mut foreign_payload.clips[0] {
            *source_abs_path = Some(foreign.join("other.wav").to_string_lossy().into_owned());
        }
        let res2 = cp.clips_paste(paste_req(foreign_payload, 0, false)).unwrap();
        assert!(res2.skipped.is_empty(), "{:?}", res2.skipped);
        let copied = &res2.audio_clips[0];
        assert!(!copied.source_id.as_str().is_empty(), "never an empty SourceId (H-3 silence)");
        assert_ne!(copied.source_id.as_str(), "the-original-source", "a new file is a new asset");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two clips in ONE paste referencing the same file share the id the
    /// first of them established — the dedup must not depend on a matching
    /// clip already being in the store.
    #[test]
    fn paste_gives_two_clips_of_the_same_file_one_source_id() {
        let cp = plane();
        let payload = hand_payload(
            960,
            vec![hand_audio("one", "t-1", "audio/x.wav"), hand_audio("two", "t-1", "audio/x.wav")],
        );

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(res.audio_clips.len(), 2);
        assert_eq!(
            res.audio_clips[0].source_id, res.audio_clips[1].source_id,
            "one file, one source identity — the engine decodes it once"
        );
        assert!(!res.audio_clips[0].source_id.as_str().is_empty());
    }

    /// M-N3: the store's paths and the resolver's paths must be compared in
    /// the SAME normal form, or a project that happens to hold a
    /// non-canonical path mints a second `SourceId` for a file it already
    /// has — the double decode I-1 exists to prevent.
    #[test]
    fn paste_matches_an_existing_source_through_a_non_canonical_stored_path() {
        let root = std::env::temp_dir().join(format!("aura-paste-norm-{}", uuid::Uuid::new_v4()));
        let proj = root.join("P.aura");
        std::fs::create_dir_all(proj.join("audio")).unwrap();
        std::fs::write(proj.join("audio").join("x.wav"), b"RIFF").unwrap();

        let cp = plane_with_project(&proj);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut c = crate::audio::types::testutil::test_clip("a-1", "t-1");
            c.source_path = "audio/./x.wav".into();
            c.source_id = crate::ids::SourceId("already-known".into());
            tx.apply(Op::ClipAdd { clip: c, index: 0 })
        })
        .unwrap();

        let payload = hand_payload(960, vec![hand_audio("dup", "t-1", "audio/x.wav")]);
        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(
            res.audio_clips[0].source_id.as_str(),
            "already-known",
            "the same file, however the stored path spelled it, is one source"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// I-2: ticks are ppq-RELATIVE, and ppq comes from `project.json`
    /// (`midi/persist.rs:320`), so it is not structurally 960. A payload
    /// from a ppq-480 project is rescaled into this session's resolution
    /// rather than landing at half its intended offsets, silently.
    #[test]
    fn paste_rescales_ticks_from_a_payload_with_a_different_ppq() {
        let cp = plane();
        assert_eq!(cp.session().lock().midi.ppq, 960, "fixture precondition");
        let payload = hand_payload(
            480,
            vec![ClipboardClip::Midi {
                name: "half-res".into(),
                source_track_id: "t-2".into(),
                source_track_name: "T2".into(),
                offset_from_anchor_ticks: 480,
                length_ticks: 480,
                content_length_ticks: Some(240),
                notes: vec![note(120, 240, 0)],
            }],
        );

        let pasted = cp.clips_paste(paste_req(payload, 0, false)).unwrap().midi_clips.remove(0);
        assert_eq!(pasted.timeline_start_ticks, 960, "one beat at ppq 480 is one beat at 960");
        assert_eq!(pasted.length_ticks, 960);
        assert_eq!(pasted.content_length_ticks, Some(480));
        assert_eq!(pasted.notes[0].tick, 240);
        assert_eq!(pasted.notes[0].length_ticks, 480);
    }

    /// The ppq the rescale uses and the ppq the tick map is built from must
    /// be ONE value, read once inside the transaction. `Op::TempoSet` writes
    /// `session.midi.ppq` (`control/session.rs:612`) from the live
    /// `set_tempo_map` command, so ppq is runtime-mutable, and two reads that
    /// disagreed would place every clip at the wrong musical position with no
    /// skip report. Here the session's resolution is moved through the REAL
    /// op path first, so the whole paste runs at a non-default ppq.
    #[test]
    fn paste_rescales_against_the_same_ppq_the_tick_map_is_built_from() {
        let cp = plane();
        crate::midi::set_tempo_map_core(
            &cp,
            Some(480),
            vec![crate::midi::types::TempoEvent { tick: 0, bpm: 120.0 }],
        )
        .unwrap();
        assert_eq!(cp.session().lock().midi.ppq, 480, "the op really moved the resolution");

        let payload = hand_payload(
            960,
            vec![ClipboardClip::Midi {
                name: "beat".into(),
                source_track_id: "t-2".into(),
                source_track_name: "T2".into(),
                offset_from_anchor_ticks: 960,
                length_ticks: 960,
                content_length_ticks: Some(960),
                notes: vec![note(480, 480, 0)],
            }],
        );

        let pasted = cp.clips_paste(paste_req(payload, 0, false)).unwrap().midi_clips.remove(0);
        assert_eq!(
            pasted.timeline_start_ticks, 480,
            "one beat past the anchor, expressed in THIS session's 480 ppq"
        );
        assert_eq!(pasted.length_ticks, 480);
        assert_eq!(pasted.content_length_ticks, Some(480));
        assert_eq!(pasted.notes[0].tick, 240);
        assert_eq!(pasted.notes[0].length_ticks, 240);
    }

    #[test]
    fn paste_rejects_a_payload_with_a_zero_ppq() {
        let cp = plane();
        let err = cp.clips_paste(paste_req(hand_payload(0, vec![]), 0, false)).unwrap_err();
        assert!(err.contains("ppq"), "{err}");
    }

    /// I-4: the Phase-2 kind check is the one that keeps a fully-rejected
    /// paste from opening a transaction at all. Phase 3 would catch the same
    /// clips, but only after the commit opened and bumped `rev`.
    #[test]
    fn paste_where_every_clip_is_kind_rejected_opens_no_transaction() {
        let cp = plane();
        let rev_before = cp.session().lock().rev;
        let (undo_before, _) = cp.history_depths();
        let payload = hand_payload(960, vec![hand_audio("wave", "t-2", "audio/x.wav")]);

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(cp.session().lock().rev, rev_before, "no transaction opened, so no rev bump");
        assert_eq!(cp.history_depths().0, undo_before);
    }

    /// M-4: `offset_samples` is attacker-controlled and reaches the RT
    /// thread as `clip.offset + rel` (`audio/mixer.rs:60`) — a debug panic
    /// ON THE AUDIO THREAD, a wrap in release. Refused at the boundary.
    #[test]
    fn paste_skips_an_audio_clip_whose_sample_window_would_overflow() {
        let cp = plane();
        let mut payload = hand_payload(960, vec![hand_audio("huge", "t-1", "audio/x.wav")]);
        if let ClipboardClip::Audio { offset_samples, length_samples, .. } = &mut payload.clips[0] {
            *offset_samples = u64::MAX;
            *length_samples = 10;
        }

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.audio_clips.is_empty());
        assert_eq!(res.skipped.len(), 1);
        assert!(res.skipped[0].reason.contains("sample window"), "{:?}", res.skipped[0]);
    }

    /// I-3: undo of a PARTIAL paste (some clips skipped) restores exactly
    /// the original rows — by identity, not by count.
    #[test]
    fn undo_of_a_partial_paste_restores_exactly_the_original_rows() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let before: (Vec<String>, Vec<String>) = {
            let s = cp.session().lock();
            (
                s.store.clips.iter().map(|c| c.id.to_string()).collect(),
                s.midi.clips.iter().map(|c| c.id.to_string()).collect(),
            )
        };
        let mut payload = hand_payload(
            960,
            vec![
                hand_audio("ok", "t-1", "audio/x.wav"),
                hand_audio("doomed", "t-1", "../escape.wav"),
            ],
        );
        payload.clips.push(ClipboardClip::Midi {
            name: "notes".into(),
            source_track_id: "t-2".into(),
            source_track_name: "T2".into(),
            offset_from_anchor_ticks: 0,
            length_ticks: 960,
            content_length_ticks: None,
            notes: vec![],
        });

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.skipped.len(), 1, "{:?}", res.skipped);
        assert_eq!(res.audio_clips.len(), 1);
        assert_eq!(res.midi_clips.len(), 1);

        cp.undo().unwrap();
        let s = cp.session().lock();
        assert_eq!(
            (
                s.store.clips.iter().map(|c| c.id.to_string()).collect::<Vec<_>>(),
                s.midi.clips.iter().map(|c| c.id.to_string()).collect::<Vec<_>>()
            ),
            before,
            "one Ctrl+Z restored the exact rows that were there, not merely the same count"
        );
    }

    /// I-3: N clips landing on ONE track still undo as one step, and the
    /// surviving rows are the originals.
    #[test]
    fn undo_of_many_clips_on_one_track_restores_the_originals_by_identity() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-2", "t-1"),
                index: 1,
            })
        })
        .unwrap();
        let payload = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();
        let (undo_before, _) = cp.history_depths();

        let res = cp.clips_paste(paste_req(payload, 96_000, false)).unwrap();
        assert_eq!(res.audio_clips.len(), 2);
        assert_eq!(cp.history_depths().0, undo_before + 1);

        cp.undo().unwrap();
        let ids: Vec<String> =
            cp.session().lock().store.clips.iter().map(|c| c.id.to_string()).collect();
        assert_eq!(ids, vec!["a-1".to_string(), "a-2".into()]);
    }

    /// A payload with an audio clip this machine cannot resolve is skipped;
    /// the MIDI clips in the same payload still land, in the same one
    /// transaction (scope ruling B).
    #[test]
    fn paste_skips_an_unresolvable_audio_clip_but_still_lands_the_midi_clips() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::MidiClipAdd {
                clip: crate::control::tests::dummy_midi_clip("t-2"),
                index: 0,
            })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let mut payload = cp.clips_copy(vec!["a-1".into()], vec![mid]).unwrap();
        // this session has no project dir, so `source_abs_path` cannot save it
        if let Some(ClipboardClip::Audio { source_abs_path, source_path, .. }) =
            payload.clips.iter_mut().find(|c| matches!(c, ClipboardClip::Audio { .. }))
        {
            *source_abs_path = Some("/definitely/not/here.wav".into());
            *source_path = "audio/definitely-not-here.wav".into();
        }

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.audio_clips.len(), 0);
        assert_eq!(res.midi_clips.len(), 1, "the MIDI half still landed");
        {
            let s = cp.session().lock();
            assert_eq!(s.store.clips.len(), 1, "no audio clip pointing at a file that is not there");
            assert_eq!(s.midi.clips.len(), 2, "the same ONE transaction still committed the MIDI half");
        }
        assert_eq!(res.skipped.len(), 1);
        assert!(
            res.skipped[0].reason.contains("audio source") || res.skipped[0].reason.contains("no project"),
            "{:?}",
            res.skipped[0]
        );
    }

    /// A paste where everything skipped commits nothing — and is not an
    /// error, because the user asked for a paste and got a precise report.
    #[test]
    fn paste_where_everything_skips_commits_no_transaction() {
        let cp = plane();
        let (undo_before, _) = cp.history_depths();
        let rev_before = cp.session().lock().rev;
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![ClipboardClip::Midi {
                name: "orphan".into(),
                source_track_id: "gone".into(),
                source_track_name: "Gone".into(),
                offset_from_anchor_ticks: 0,
                length_ticks: 960,
                content_length_ticks: None,
                notes: vec![],
            }],
        };

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(cp.history_depths().0, undo_before, "nothing committed");
        assert_eq!(cp.session().lock().rev, rev_before);
    }

    /// A payload from a future schema version is refused with a message a
    /// human can act on, rather than deserializing into nonsense.
    #[test]
    fn paste_rejects_a_newer_schema_version() {
        let cp = plane();
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION + 1,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![],
        };
        let err = cp.clips_paste(paste_req(payload, 0, false)).unwrap_err();
        assert!(err.contains("schema"), "{err}");
    }

    /// Requirement 2 for the MIDI half: the tick gap between two copied
    /// clips survives the paste. (The audio half is pinned by
    /// `paste_preserves_per_clip_offsets_and_source_tracks`; only ticks go
    /// through the tempo map, so they need their own pin.)
    #[test]
    fn paste_preserves_per_clip_tick_offsets() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::control::tests::dummy_midi_clip("t-2");
            a.timeline_start_ticks = 480;
            let mut b = crate::control::tests::dummy_midi_clip("t-2");
            b.id = crate::ids::ClipId::from("m-2");
            b.timeline_start_ticks = 1_440;
            tx.apply(Op::MidiClipAdd { clip: a, index: 0 })?;
            tx.apply(Op::MidiClipAdd { clip: b, index: 1 })
        })
        .unwrap();
        let ids: Vec<String> =
            cp.session().lock().midi.clips.iter().map(|c| c.id.to_string()).collect();
        let payload = cp.clips_copy(vec![], ids).unwrap();

        // anchor = tick 480 = 12_000 nominal samples; pasting at 24_000
        // samples (tick 960) puts the pair at 960 and 1_920.
        let res = cp.clips_paste(paste_req(payload, 24_000, false)).unwrap();
        let starts: Vec<u64> = res.midi_clips.iter().map(|c| c.timeline_start_ticks).collect();
        assert_eq!(starts, vec![960, 1_920], "the 960-tick gap survives the paste");
    }

    /// The envelope's other half: a blob that is not ours at all. Refused
    /// before any lock, any I/O and any commit.
    #[test]
    fn paste_rejects_a_foreign_mime() {
        let cp = plane();
        let (undo_before, _) = cp.history_depths();
        let mut payload = cp.clips_copy(vec![], vec![]).unwrap();
        payload.mime = "text/plain".into();
        let err = cp.clips_paste(paste_req(payload, 0, false)).unwrap_err();
        assert!(err.contains("not an AURA clips payload"), "{err}");
        assert!(err.contains("text/plain"), "the message names what was found: {err}");
        assert_eq!(cp.history_depths().0, undo_before, "a refused envelope commits nothing");
    }

    /// An empty payload is a no-op, not an error and not an empty undo step:
    /// pasting an empty clipboard must not put a phantom entry in the
    /// history the user then has to Ctrl+Z past.
    #[test]
    fn paste_of_an_empty_payload_commits_nothing_and_is_not_an_error() {
        let cp = plane();
        let (undo_before, _) = cp.history_depths();
        let rev_before = cp.session().lock().rev;
        let payload = cp.clips_copy(vec![], vec![]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 480_000, false)).unwrap();
        assert!(res.audio_clips.is_empty());
        assert!(res.midi_clips.is_empty());
        assert!(res.created_tracks.is_empty());
        assert!(res.skipped.is_empty(), "nothing was asked for, so nothing was skipped");
        assert_eq!(cp.history_depths().0, undo_before);
        assert_eq!(cp.session().lock().rev, rev_before);
    }

    /// Time starts at 0. A payload carrying negative offsets — which a
    /// future anchor rule, or a hand-edited clipboard, can produce — must
    /// clamp, never wrap around `u64` into the far future.
    #[test]
    fn paste_clamps_a_negative_placement_to_zero_instead_of_wrapping() {
        let cp = plane();
        let payload = AuraClipsPayload {
            mime: AURA_CLIPS_MIME.into(),
            schema_version: AURA_CLIPS_SCHEMA_VERSION,
            anchor_samples: 0,
            anchor_ticks: 0,
            ppq: 960,
            source_project_dir: None,
            clips: vec![
                ClipboardClip::Audio {
                    name: "early".into(),
                    source_track_id: "t-1".into(),
                    source_track_name: "T1".into(),
                    offset_from_anchor_samples: -48_000,
                    length_samples: 1_000,
                    source_path: "audio/x.wav".into(),
                    source_abs_path: None,
                    source_channels: 2,
                    source_sample_rate: 48_000,
                    source_length_samples: 1_000,
                    offset_samples: 0,
                    gain_db: 0.0,
                    fade_in_samples: 0,
                    fade_out_samples: 0,
                },
                ClipboardClip::Midi {
                    name: "early-midi".into(),
                    source_track_id: "t-2".into(),
                    source_track_name: "T2".into(),
                    offset_from_anchor_ticks: -960,
                    length_ticks: 960,
                    content_length_ticks: None,
                    notes: vec![],
                },
            ],
        };

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        assert!(res.skipped.is_empty(), "{:?}", res.skipped);
        assert_eq!(res.audio_clips[0].timeline_start_samples, 0);
        assert_eq!(res.midi_clips[0].timeline_start_ticks, 0);
    }

    /// The wire-domain rule Task 8's fix round established, on the PASTE
    /// side: the playhead's sample position converts to ticks through the
    /// NOMINAL 48kHz map — the one `ProjectSnapshot`'s section table is
    /// built from — never through whatever interface happens to be open.
    /// The device rate is overridden to 96kHz here precisely to unmask that
    /// bug class: an engine-rate map would resolve sample 24_000 to tick
    /// 480 instead of the nominal 960, so a copy made on one machine would
    /// land somewhere else on another.
    #[test]
    fn paste_tick_conversion_is_independent_of_the_live_device_rate() {
        let cp = plane();
        cp.shared.sample_rate.store(96_000, std::sync::atomic::Ordering::Relaxed);
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut m = crate::control::tests::dummy_midi_clip("t-2");
            m.timeline_start_ticks = 0;
            tx.apply(Op::MidiClipAdd { clip: m, index: 0 })
        })
        .unwrap();
        let mid = cp.session().lock().midi.clips[0].id.to_string();
        let payload = cp.clips_copy(vec![], vec![mid]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 24_000, false)).unwrap();
        assert_eq!(
            res.midi_clips[0].timeline_start_ticks, 960,
            "nominal 48kHz conversion of sample 24_000, not the 96kHz device rate's 480"
        );
    }

    /// The insert index comes from the IN-TRANSACTION view, so two clips in
    /// ONE paste do not both claim the same slot and land reversed.
    #[test]
    fn paste_appends_clips_in_payload_order() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-1", "t-1"),
                index: 0,
            })?;
            tx.apply(Op::ClipAdd {
                clip: crate::audio::types::testutil::test_clip("a-2", "t-1"),
                index: 1,
            })
        })
        .unwrap();
        let payload = cp.clips_copy(vec!["a-1".into(), "a-2".into()], vec![]).unwrap();

        let res = cp.clips_paste(paste_req(payload, 0, false)).unwrap();
        let pasted: Vec<String> = res.audio_clips.iter().map(|c| c.id.to_string()).collect();
        let stored: Vec<String> =
            cp.session().lock().store.clips.iter().map(|c| c.id.to_string()).collect();
        assert_eq!(stored, vec!["a-1".to_string(), "a-2".into(), pasted[0].clone(), pasted[1].clone()]);
    }

    /// A truncated or garbage clipboard string never reaches `clips_paste`
    /// at all: the typed envelope is the gate, and it refuses rather than
    /// filling missing fields with defaults.
    #[test]
    fn a_truncated_or_garbage_payload_fails_to_decode() {
        assert!(serde_json::from_str::<AuraClipsPayload>("{\"mime\":\"application/x-aur").is_err());
        assert!(serde_json::from_str::<AuraClipsPayload>("not json at all").is_err());
        // A well-formed envelope missing `clips` is refused too — a paste
        // must never silently become a no-op because a field was dropped.
        assert!(serde_json::from_str::<AuraClipsPayload>(
            "{\"mime\":\"application/x-aura-clips\",\"schemaVersion\":1,\"anchorSamples\":0,\
             \"anchorTicks\":0,\"ppq\":960,\"sourceProjectDir\":null}"
        )
        .is_err());
    }

    /// A payload from ANOTHER project whose file this machine has no
    /// absolute path for is skipped with the path that was tried — the
    /// cross-machine case (the clipboard string travelled, the wav did not).
    #[test]
    fn resolve_audio_asset_skips_a_foreign_reference_with_no_absolute_path() {
        let root = std::env::temp_dir().join(format!("aura-paste-nopath-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let err = resolve_audio_asset(Some(&root), Some("/elsewhere/A.aura"), "take", "audio/take.wav", None)
            .unwrap_err();
        assert!(err.contains("missing audio source"), "{err}");
        assert!(err.contains("audio/take.wav"), "the message names the path that was tried: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Scope ruling B's middle step, the one the nine ControlPlane tests
    /// above cannot reach (their harness has no project dir): a payload from
    /// ANOTHER project resolves through `sourceAbsPath` and is COPIED into
    /// this project's `audio/` under a fresh name — bytes intact, the
    /// original untouched, and the returned path project-relative POSIX.
    /// Step 1 (already inside this project) wins over the copy, and the
    /// copy never overwrites an existing asset (fresh uuid per import).
    #[test]
    fn resolve_audio_asset_copies_a_foreign_file_into_the_project_and_prefers_in_place() {
        let root = std::env::temp_dir()
            .join(format!("aura-paste-asset-{}", uuid::Uuid::new_v4()));
        let (src_proj, dst_proj) = (root.join("A.aura"), root.join("B.aura"));
        std::fs::create_dir_all(src_proj.join("audio")).unwrap();
        std::fs::create_dir_all(dst_proj.join("audio")).unwrap();
        let src_file = src_proj.join("audio").join("take.wav");
        std::fs::write(&src_file, b"RIFF-not-really").unwrap();
        let abs = src_file.to_string_lossy().into_owned();

        // Step 2: not in THIS project, but the absolute path resolves.
        let copied = resolve_audio_asset(
            Some(&dst_proj),
            Some(&src_proj.to_string_lossy()),
            "take",
            "audio/take.wav",
            Some(&abs),
        )
        .unwrap();
        let rel = match &copied {
            ResolvedAsset::Copied(p) => p.clone(),
            other => panic!("expected a copy, got {other:?}"),
        };
        assert!(rel.starts_with("audio/"), "project-relative POSIX path: {rel}");
        assert!(rel.ends_with("-take.wav"), "the original file name survives: {rel}");
        assert_eq!(
            std::fs::read(dst_proj.join(&rel)).unwrap(),
            b"RIFF-not-really",
            "bytes copied verbatim"
        );
        assert!(src_file.is_file(), "the source project is left untouched");

        // A second import of the same file does NOT overwrite the first.
        let again = resolve_audio_asset(
            Some(&dst_proj),
            None,
            "take",
            "audio/take.wav",
            Some(&abs),
        )
        .unwrap();
        match &again {
            ResolvedAsset::Copied(p) => assert_ne!(p, &rel, "a fresh name per import"),
            other => panic!("expected a copy, got {other:?}"),
        }

        // Step 1 wins once the path resolves inside this project: reference
        // in place, no second copy.
        let in_place =
            resolve_audio_asset(Some(&dst_proj), None, "take", &rel, Some(&abs)).unwrap();
        assert!(
            matches!(&in_place, ResolvedAsset::InPlace(p) if p == &rel),
            "{in_place:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
