//! Standard MIDI file (.mid) import/export over `midly` — interop surface
//! for the tick timeline (and a convenient wire format for external tools).
//!
//! Pure, tauri-free. Positions are ticks; the ONLY unit conversion here is
//! PPQ rescaling between the file's division and the project PPQ (D-02:
//! never seconds, never samples).

use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use super::types::{MidiClip, MidiNote, TempoEvent};

/// Result of importing a .mid file: one clip per SMF track that carries
/// notes (`track_id` left empty — the caller assigns placement), plus the
/// merged tempo map. All ticks are rescaled to `ppq`.
#[derive(Debug, Clone)]
pub struct ImportedMidi {
    pub ppq: u32,
    pub tempo_events: Vec<TempoEvent>,
    pub clips: Vec<MidiClip>,
    /// True when the file carried at least one explicit tempo meta event
    /// (false = `tempo_events` is just the synthesized 120 bpm default, so
    /// callers should keep the project's existing tempo map).
    pub explicit_tempo: bool,
}

#[inline]
fn rescale(tick: u64, from_ppq: u32, to_ppq: u32) -> u64 {
    if from_ppq == to_ppq || from_ppq == 0 {
        return tick;
    }
    (tick as u128 * to_ppq as u128 + from_ppq as u128 / 2) as u64 / from_ppq as u64
}

#[inline]
fn bpm_to_us_per_qn(bpm: f64) -> u32 {
    (60_000_000.0 / bpm).round().clamp(1.0, 16_777_215.0) as u32
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

/// Serialize a tempo map + clips to a format-1 SMF. Each clip becomes one
/// track; note positions are absolute (`timelineStartTicks + note.tick`).
pub fn export_smf(
    ppq: u32,
    tempo_events: &[TempoEvent],
    clips: &[MidiClip],
) -> Result<Vec<u8>, String> {
    if ppq == 0 || ppq > u15::max_value().as_int() as u32 {
        return Err(format!("ppq {ppq} not representable in SMF (1..=32767)"));
    }
    let header = Header::new(Format::Parallel, Timing::Metrical(u15::new(ppq as u16)));
    let mut smf = Smf::new(header);

    // Track 0: tempo map.
    let mut tempo_track: Vec<TrackEvent> = Vec::new();
    let mut prev = 0u64;
    for e in tempo_events {
        let delta = delta28(e.tick.saturating_sub(prev))?;
        prev = e.tick;
        tempo_track.push(TrackEvent {
            delta,
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(bpm_to_us_per_qn(e.bpm)))),
        });
    }
    tempo_track.push(end_of_track());
    smf.tracks.push(tempo_track);

    // One track per clip.
    for clip in clips {
        // (abs_tick, order 0=off 1=on, key, vel, channel)
        let mut edges: Vec<(u64, u8, u8, u8, u8)> = Vec::with_capacity(clip.notes.len() * 2);
        for n in &clip.notes {
            if n.velocity == 0 || n.length_ticks == 0 {
                continue;
            }
            let on = clip.timeline_start_ticks + n.tick as u64;
            let off = on + n.length_ticks as u64;
            edges.push((on, 1, n.key, n.velocity, n.channel));
            edges.push((off, 0, n.key, 0, n.channel));
        }
        edges.sort_by_key(|&(t, order, key, ..)| (t, order, key));

        let mut track: Vec<TrackEvent> = Vec::new();
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(clip.name.as_bytes())),
        });
        let mut prev = 0u64;
        for (tick, order, key, vel, ch) in edges {
            let delta = delta28(tick.saturating_sub(prev))?;
            prev = tick;
            let message = if order == 1 {
                MidiMessage::NoteOn { key: u7::new(key.min(127)), vel: u7::new(vel.min(127)) }
            } else {
                MidiMessage::NoteOff { key: u7::new(key.min(127)), vel: u7::new(0) }
            };
            track.push(TrackEvent {
                delta,
                kind: TrackEventKind::Midi { channel: u4::new(ch.min(15)), message },
            });
        }
        track.push(end_of_track());
        smf.tracks.push(track);
    }

    let mut out = Vec::new();
    smf.write_std(&mut out).map_err(|e| format!("write SMF: {e}"))?;
    Ok(out)
}

fn end_of_track() -> TrackEvent<'static> {
    TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) }
}

fn delta28(d: u64) -> Result<u28, String> {
    if d > u28::max_value().as_int() as u64 {
        return Err(format!("delta time {d} exceeds SMF range"));
    }
    Ok(u28::new(d as u32))
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// Parse a .mid file into clips + tempo map at `target_ppq`.
/// SMPTE-timed files are rejected (musical time only, D-02).
pub fn import_smf(bytes: &[u8], target_ppq: u32) -> Result<ImportedMidi, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("parse SMF: {e}"))?;
    let file_ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as u32,
        Timing::Timecode(..) => {
            return Err("SMPTE-timed MIDI files are not supported (musical time only)".into())
        }
    };
    if file_ppq == 0 {
        return Err("SMF division of 0".into());
    }

    let mut tempo: Vec<TempoEvent> = Vec::new();
    let mut clips: Vec<MidiClip> = Vec::new();

    for (ti, track) in smf.tracks.iter().enumerate() {
        let mut abs = 0u64;
        let mut name: Option<String> = None;
        // FIFO of pending note-ons per (channel, key).
        let mut pending: std::collections::HashMap<(u8, u8), Vec<(u64, u8)>> =
            std::collections::HashMap::new();
        let mut notes: Vec<MidiNote> = Vec::new();
        let mut track_end = 0u64;

        for ev in track {
            abs += ev.delta.as_int() as u64;
            track_end = track_end.max(abs);
            match &ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    let bpm = 60_000_000.0 / (us.as_int().max(1) as f64);
                    tempo.push(TempoEvent { tick: rescale(abs, file_ppq, target_ppq), bpm });
                }
                TrackEventKind::Meta(MetaMessage::TrackName(b)) => {
                    if name.is_none() {
                        name = Some(String::from_utf8_lossy(b).into_owned());
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            pending
                                .entry((ch, key.as_int()))
                                .or_default()
                                .push((abs, vel.as_int()));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            if let Some(stack) = pending.get_mut(&(ch, key.as_int())) {
                                if let Some((on, vel)) = if stack.is_empty() {
                                    None
                                } else {
                                    Some(stack.remove(0))
                                } {
                                    push_note(
                                        &mut notes, on, abs, key.as_int(), vel, ch, file_ppq,
                                        target_ppq,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Close any hanging notes at track end.
        for ((ch, key), stack) in pending {
            for (on, vel) in stack {
                push_note(&mut notes, on, track_end.max(on + 1), key, vel, ch, file_ppq, target_ppq);
            }
        }
        if notes.is_empty() {
            continue;
        }
        notes.sort_by_key(|n| (n.tick, n.key));
        let length = notes
            .iter()
            .map(|n| n.tick as u64 + n.length_ticks as u64)
            .max()
            .unwrap_or(1)
            .max(1);
        clips.push(MidiClip {
            id: uuid::Uuid::new_v4().to_string(),
            track_id: String::new(),
            name: name.unwrap_or_else(|| format!("Imported track {}", ti + 1)),
            timeline_start_ticks: 0,
            length_ticks: length,
            notes,
        });
    }

    // Merge/normalize the tempo map: sorted, unique ticks (last wins),
    // first entry at tick 0 (MIDI default 120 bpm applies before the first
    // tempo event).
    tempo.sort_by_key(|e| e.tick);
    let explicit_tempo = tempo.iter().any(|e| e.bpm.is_finite() && e.bpm > 0.0);
    let mut merged: Vec<TempoEvent> = Vec::new();
    for e in tempo {
        if !(e.bpm.is_finite() && e.bpm > 0.0) {
            continue;
        }
        match merged.last_mut() {
            Some(last) if last.tick == e.tick => *last = e,
            _ => merged.push(e),
        }
    }
    if merged.first().map(|e| e.tick != 0).unwrap_or(true) {
        merged.insert(0, TempoEvent { tick: 0, bpm: 120.0 });
    }

    Ok(ImportedMidi { ppq: target_ppq, tempo_events: merged, clips, explicit_tempo })
}

#[allow(clippy::too_many_arguments)]
fn push_note(
    notes: &mut Vec<MidiNote>,
    on: u64,
    off: u64,
    key: u8,
    vel: u8,
    ch: u8,
    file_ppq: u32,
    target_ppq: u32,
) {
    let tick = rescale(on, file_ppq, target_ppq);
    let len = rescale(off.max(on + 1), file_ppq, target_ppq).saturating_sub(tick).max(1);
    notes.push(MidiNote {
        tick: tick.min(u32::MAX as u64) as u32,
        length_ticks: len.min(u32::MAX as u64) as u32,
        key,
        velocity: vel.max(1),
        channel: ch,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(tick: u32, len: u32, key: u8, vel: u8, ch: u8) -> MidiNote {
        MidiNote { tick, length_ticks: len, key, velocity: vel, channel: ch }
    }

    #[test]
    fn export_import_roundtrip_preserves_notes_and_tempo() {
        let tempo = vec![
            TempoEvent { tick: 0, bpm: 120.0 },
            TempoEvent { tick: 3840, bpm: 90.0 },
        ];
        let clip = MidiClip {
            id: "c".into(),
            track_id: "t".into(),
            name: "Lead".into(),
            timeline_start_ticks: 0,
            length_ticks: 7680,
            notes: vec![
                note(0, 480, 60, 100, 0),
                note(480, 240, 64, 90, 1),
                note(480, 960, 67, 80, 0), // overlapping
                note(4000, 100, 72, 127, 15),
            ],
        };
        let bytes = export_smf(960, &tempo, &[clip.clone()]).unwrap();
        let imp = import_smf(&bytes, 960).unwrap();
        assert_eq!(imp.clips.len(), 1);
        assert_eq!(imp.clips[0].name, "Lead");
        assert_eq!(imp.clips[0].notes, clip.notes);
        assert_eq!(imp.tempo_events.len(), 2);
        assert_eq!(imp.tempo_events[0].tick, 0);
        assert!((imp.tempo_events[0].bpm - 120.0).abs() < 0.01);
        assert_eq!(imp.tempo_events[1].tick, 3840);
        assert!((imp.tempo_events[1].bpm - 90.0).abs() < 0.01);
        assert!(imp.explicit_tempo, "file carried real tempo meta events");
    }

    #[test]
    fn clip_timeline_offset_becomes_absolute_ticks() {
        let clip = MidiClip {
            id: "c".into(),
            track_id: "t".into(),
            name: "n".into(),
            timeline_start_ticks: 960,
            length_ticks: 960,
            notes: vec![note(0, 480, 60, 100, 0)],
        };
        let bytes =
            export_smf(960, &[TempoEvent { tick: 0, bpm: 120.0 }], &[clip]).unwrap();
        let imp = import_smf(&bytes, 960).unwrap();
        assert_eq!(imp.clips[0].notes[0].tick, 960, "absolute position in the file");
    }

    #[test]
    fn ppq_rescaling_on_import() {
        let clip = MidiClip {
            id: "c".into(),
            track_id: "t".into(),
            name: "n".into(),
            timeline_start_ticks: 0,
            length_ticks: 960,
            notes: vec![note(240, 480, 60, 100, 0)],
        };
        // Export at 480 ppq, import at 960 -> ticks double.
        let bytes = export_smf(480, &[TempoEvent { tick: 0, bpm: 120.0 }], &[clip]).unwrap();
        let imp = import_smf(&bytes, 960).unwrap();
        assert_eq!(imp.clips[0].notes[0].tick, 480);
        assert_eq!(imp.clips[0].notes[0].length_ticks, 960);
    }

    #[test]
    fn same_key_retrigger_pairs_fifo_and_default_tempo_inserted() {
        // Two overlapping notes on the SAME key/channel: FIFO pairing.
        let clip = MidiClip {
            id: "c".into(),
            track_id: "t".into(),
            name: "n".into(),
            timeline_start_ticks: 0,
            length_ticks: 1920,
            notes: vec![note(0, 960, 60, 100, 0), note(480, 960, 60, 90, 0)],
        };
        // No tempo events at all -> import synthesizes 120 bpm at tick 0.
        let bytes = export_smf(960, &[], &[clip]).unwrap();
        let imp = import_smf(&bytes, 960).unwrap();
        assert_eq!(imp.tempo_events, vec![TempoEvent { tick: 0, bpm: 120.0 }]);
        assert!(!imp.explicit_tempo, "synthesized default is flagged as such");
        let notes = &imp.clips[0].notes;
        assert_eq!(notes.len(), 2);
        // FIFO: first-on pairs with first-off -> lengths 960 each preserved.
        assert_eq!(notes[0], note(0, 960, 60, 100, 0));
        assert_eq!(notes[1], note(480, 960, 60, 90, 0));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(import_smf(b"not a midi file", 960).is_err());
        assert!(export_smf(0, &[], &[]).is_err());
        assert!(export_smf(40_000, &[], &[]).is_err());
    }
}
