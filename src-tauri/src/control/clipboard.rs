//! The `application/x-aura-clips` clipboard payload: what a multi-clip copy
//! puts on the OS clipboard, and what a paste turns back into document rows.
//!
//! MIDI clips travel BY VALUE (their notes are the content, and they are
//! small); audio clips travel BY REFERENCE (a wav is megabytes — the
//! clipboard is not an asset store). See the plan's scope ruling B for the
//! three-step asset resolution a paste performs, and scope ruling C for why
//! the MIME name rides inside the envelope instead of being an OS clipboard
//! flavor.

use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::ControlPlane;
use crate::midi::TempoMap;

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
        // math, so the anchor's cross-unit resolution happens here.
        let rate = self.shared.sample_rate.load(Relaxed);
        let tempo = TempoMap::new(midi.ppq, midi.tempo_events.clone(), rate)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::op::{Op, TxMeta};

    /// Reuses control/mod.rs's harness — a ControlPlane over a test engine
    /// double with the named tracks already slotted.
    fn plane() -> crate::control::ControlPlane {
        let (cp, _rx, _events) = crate::control::tests::test_plane_with_tracks(&["t-1", "t-2"]);
        cp
    }

    #[test]
    fn copy_offsets_are_relative_to_the_leftmost_clip() {
        let cp = plane();
        cp.commit(TxMeta::user("seed"), |tx| {
            let mut a = crate::audio::types::testutil::test_clip("a-1", "t-1");
            a.timeline_start_samples = 96_000;
            let mut b = crate::audio::types::testutil::test_clip("a-2", "t-2");
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
}
