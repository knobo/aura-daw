//! AMEV binary event chunks — the on-disk format for MIDI/automation events.
//!
//! project.json v2 references these from `patterns[].eventsRef` /
//! `midiClips[].eventsRef` as `events/<id>.bin` so JSON never carries event
//! arrays (SCALABILITY §3: 10^6 notes must not be a JSON parse).
//!
//! Wire format (little-endian, same design language as AWTF waveform tiles):
//!
//! ```text
//! [magic   u32 = 0x414D4556 "AMEV"]
//! [version u16 = 1] [columnMask u16 = 0x0001 (core columns)]
//! [ppq     u32]
//! [count   u32]
//! then count x 16-byte records:
//!   [tick u32] [duration u32] [kind u8] [key u8] [velocity u8] [channel u8]
//!   [value f32]   // automation point value; 0.0 for notes
//! ```
//!
//! `kind`: 0 = note, 1 = automation point. Future columns (per-note
//! expression / MPE) extend via `columnMask` bits + appended column blocks —
//! old readers skip unknown columns, never break (avoids re-creating D-06).
//!
//! After `count` 16-byte core records, zero or more APPENDED COLUMN BLOCKS,
//! one per columnMask bit above 0x0001, in ascending bit order:
//!   [colBit u16] [byteLen u32] [payload: byteLen bytes]
//! Readers skip blocks whose colBit they do not know via byteLen — that is
//! what makes future columns non-breaking (the D-06 rule, binary edition).
//! COLUMNS_NOTE_ID (0x0002) payload:
//!   [next_note_id u32] [count x note_id u32]   (byteLen = 4 + 4*count)
//! One note_id per RECORD, in record order — 0 for non-note (kind != 0)
//! records. `count` is the record count, and records are kind-tagged; a
//! mixed chunk must not misalign ids against the note subset. [M-1]
//! AMEV_VERSION stays 1: v1 chunks without the column read tolerantly (note
//! ids minted 1..=count by record ordinal on load, watermark count+1); every
//! write includes it.
//! Reader rules (normative) [M-2, M-3, M-4]:
//! * the columnMask is ADVISORY: unknown bits are ignored, unknown blocks
//!   are skipped by byteLen, and no reader may error on an unrecognised bit.
//!   The note-id column is "present" iff a 0x0002 block was CONSUMED; a mask
//!   bit set with no matching block -> Err; a block whose bit is not in the
//!   mask is read anyway (blocks are self-describing).
//! * byteLen > bytes remaining -> Err (truncated chunk). Remaining bytes are
//!   computed with saturating_sub — never a bare subtraction.
//! * the same colBit twice, or a colBit <= the previous one -> Err.
//! * expected sizes (4 + 4*count, count*16) are computed in u64, and
//!   count*16 <= remaining bytes is required BEFORE allocating.
//! * a watermark <= some present id is REPAIRED, not rejected:
//!   next_note_id = max(next_note_id, max_present_id + 1) + log::warn!.
//!   Err is reserved for structural corruption — a semantic Err would feed
//!   persist.rs's degrade-to-empty path and destroy notes on the next save.

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use super::types::MidiNote;
use crate::ids::NoteId;

pub const AMEV_MAGIC: u32 = 0x414D_4556;
pub const AMEV_VERSION: u16 = 1;
pub const COLUMNS_CORE: u16 = 0x0001;
pub const COLUMNS_NOTE_ID: u16 = 0x0002;
pub const KIND_NOTE: u8 = 0;
pub const KIND_AUTOMATION: u8 = 1;

/// Encode note events into an AMEV chunk, including the note-id column
/// (watermark + one id per record).
pub fn encode_notes(ppq: u32, notes: &[MidiNote], next_note_id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + notes.len() * 16 + 6 + 4 + notes.len() * 4);
    out.write_u32::<LittleEndian>(AMEV_MAGIC).unwrap();
    out.write_u16::<LittleEndian>(AMEV_VERSION).unwrap();
    out.write_u16::<LittleEndian>(COLUMNS_CORE | COLUMNS_NOTE_ID).unwrap();
    out.write_u32::<LittleEndian>(ppq).unwrap();
    out.write_u32::<LittleEndian>(notes.len() as u32).unwrap();
    for n in notes {
        out.write_u32::<LittleEndian>(n.tick).unwrap();
        out.write_u32::<LittleEndian>(n.length_ticks).unwrap();
        out.push(KIND_NOTE);
        out.push(n.key);
        out.push(n.velocity);
        out.push(n.channel);
        out.write_f32::<LittleEndian>(0.0).unwrap();
    }
    // Appended COLUMNS_NOTE_ID block.
    out.write_u16::<LittleEndian>(COLUMNS_NOTE_ID).unwrap();
    let byte_len = 4u32 + 4 * notes.len() as u32;
    out.write_u32::<LittleEndian>(byte_len).unwrap();
    out.write_u32::<LittleEndian>(next_note_id).unwrap();
    for n in notes {
        out.write_u32::<LittleEndian>(n.note_id.0).unwrap();
    }
    out
}

/// Decoded AMEV note chunk: notes plus the persisted note-id watermark
/// (upgraded on read for v1/columnless chunks — see the module doc).
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedNotes {
    pub ppq: u32,
    pub notes: Vec<MidiNote>,
    pub next_note_id: u32,
}

/// Decode an AMEV chunk. Automation records are skipped (they get their own
/// accessor when automation lands). Implements the module doc's normative
/// reader rules for the appended note-id column.
pub fn decode_notes(bytes: &[u8]) -> Result<DecodedNotes, String> {
    let mut r = std::io::Cursor::new(bytes);
    let magic = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    if magic != AMEV_MAGIC {
        return Err(format!("not an AMEV chunk (magic {magic:#010x})"));
    }
    let version = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    if version > AMEV_VERSION {
        return Err(format!("AMEV version {version} is newer than supported {AMEV_VERSION}"));
    }
    // The mask is advisory for UNKNOWN bits (never rejected, never required
    // to match a present block) — but for a bit this reader DOES know
    // (COLUMNS_NOTE_ID), a mask claiming the column is present with no
    // matching block consumed is structural corruption, not tolerance
    // territory; checked once the block walk below has run.
    let mask = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    let ppq = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    let count = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;

    // Core records: (tick, duration, kind, key, velocity, channel), and
    // whether each record index is a note (for note-id alignment).
    let count_u64 = count as u64;
    let core_bytes = count_u64.saturating_mul(16);
    if core_bytes > bytes.len() as u64 {
        return Err(format!(
            "AMEV chunk truncated: {count} records need {core_bytes} bytes, chunk has {}",
            bytes.len()
        ));
    }
    // `note_record_indices[i]` is the RECORD ordinal that produced
    // `notes[i]` — the note-id column is indexed by record ordinal (0 for
    // non-note records), so applying it to `notes` needs this mapping
    // rather than a straight zip (M-1: a mixed chunk must not misalign ids
    // against the note subset).
    let mut notes = Vec::with_capacity(count as usize);
    let mut note_record_indices: Vec<u32> = Vec::with_capacity(count as usize);
    for record_index in 0..count {
        let tick = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
        let duration = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
        let mut rest = [0u8; 4];
        std::io::Read::read_exact(&mut r, &mut rest).map_err(|e| e.to_string())?;
        let _value = r.read_f32::<LittleEndian>().map_err(|e| e.to_string())?;
        if rest[0] == KIND_NOTE {
            notes.push(MidiNote {
                tick,
                length_ticks: duration,
                key: rest[1],
                velocity: rest[2],
                channel: rest[3],
                note_id: NoteId(0), // filled in below once the column (if any) is read
            });
            note_record_indices.push(record_index);
        }
    }

    // Appended column blocks: walk until fewer than 6 bytes remain
    // (colBit u16 + byteLen u32 is the minimum header).
    let mut note_ids: Option<Vec<u32>> = None; // one per note, in record order
    let mut next_note_id: Option<u32> = None;
    let mut last_bit: i32 = -1;
    let mut bits_consumed: u16 = 0; // every colBit actually seen as a block, known or not
    loop {
        let remaining = (bytes.len() as u64).saturating_sub(r.position());
        if remaining < 6 {
            // Fewer than 6 trailing bytes can't be a block header; ignoring
            // this slack (rather than erroring on it) is intentional, not a
            // gap — reviewed and accepted (Minor).
            break;
        }
        let col_bit = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
        let byte_len = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
        let remaining_after_header = (bytes.len() as u64).saturating_sub(r.position());
        if byte_len as u64 > remaining_after_header {
            return Err(format!(
                "AMEV chunk truncated: column 0x{col_bit:04x} declares {byte_len} bytes, {remaining_after_header} remain"
            ));
        }
        if col_bit as i32 <= last_bit {
            return Err(format!(
                "AMEV column blocks out of order or duplicated: 0x{col_bit:04x} after 0x{last_bit:04x}"
            ));
        }
        last_bit = col_bit as i32;
        bits_consumed |= col_bit;

        if col_bit == COLUMNS_NOTE_ID {
            // Sized by the RECORD count, not the note count (M-1).
            let expected = 4u64 + 4 * count_u64;
            if byte_len as u64 != expected {
                return Err(format!(
                    "AMEV note-id column: expected {expected} bytes for {count} record(s), got {byte_len}"
                ));
            }
            let watermark = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
            let mut ids = Vec::with_capacity(count as usize);
            for _ in 0..count {
                ids.push(r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?);
            }
            next_note_id = Some(watermark);
            note_ids = Some(ids);
        } else {
            // Unknown column: skip by byteLen (self-describing, D-06).
            let mut buf = vec![0u8; byte_len as usize];
            std::io::Read::read_exact(&mut r, &mut buf).map_err(|e| e.to_string())?;
        }
    }

    // Reviewer finding 1 (CRITICAL): a mask bit set with no matching block
    // consumed is structural corruption (a truncated/stripped block with a
    // stale mask), never silently tolerated — this is the general form of
    // the module doc's rule, not special-cased to COLUMNS_NOTE_ID. Bits
    // above COLUMNS_CORE only, since COLUMNS_CORE is the fixed-format
    // header+records themselves, not an appended block.
    let missing_blocks = mask & !COLUMNS_CORE & !bits_consumed;
    if missing_blocks != 0 {
        return Err(format!(
            "AMEV mask 0x{mask:04x} claims column(s) 0x{missing_blocks:04x} but no matching block was found"
        ));
    }

    let next_note_id = match (note_ids, next_note_id) {
        (Some(ids), Some(watermark)) => {
            // Take ids[record_index] at the point each note is applied —
            // never a straight zip against `notes` (record-ordinal rule).
            for (n, &record_index) in notes.iter_mut().zip(&note_record_indices) {
                n.note_id = NoteId(ids[record_index as usize]);
            }
            let max_present = notes.iter().map(|n| n.note_id.0).max().unwrap_or(0);
            if watermark <= max_present && max_present > 0 {
                log::warn!(
                    "AMEV chunk: note-id watermark {watermark} <= max present id {max_present}; repairing to {}",
                    max_present + 1
                );
                max_present + 1
            } else {
                watermark
            }
        }
        _ => {
            // No note-id column consumed: v1 tolerant upgrade — mint
            // 1..=count by record ordinal, watermark count+1.
            let mut next = 1u32;
            for n in notes.iter_mut() {
                n.note_id = NoteId(next);
                next += 1;
            }
            next
        }
    };

    Ok(DecodedNotes { ppq, notes, next_note_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amev_roundtrip() {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(1) },
            MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(2) },
            MidiNote { tick: 720, length_ticks: 240, key: 67, velocity: 80, channel: 1, note_id: NoteId(3) },
        ];
        let bytes = encode_notes(960, &notes, 4);
        assert_eq!(bytes.len(), 16 + 3 * 16 + 6 + 4 + 3 * 4);
        let d = decode_notes(&bytes).unwrap();
        assert_eq!(d.ppq, 960);
        assert_eq!(d.notes, notes);
        assert_eq!(d.next_note_id, 4);
    }

    /// SCALABILITY §3: 10^5 notes must round-trip exactly (and fast) through
    /// the binary chunk — this is the "millions of events never touch JSON"
    /// guarantee at a tenth of the target scale per chunk.
    #[test]
    fn amev_roundtrip_at_100k_notes() {
        let notes: Vec<MidiNote> = (0..100_000u32)
            .map(|i| MidiNote {
                tick: i * 13,
                length_ticks: 1 + (i % 3840),
                key: (i % 128) as u8,
                velocity: (1 + i % 127) as u8,
                channel: (i % 16) as u8,
                note_id: NoteId(i + 1),
            })
            .collect();
        let bytes = encode_notes(960, &notes, 100_001);
        assert_eq!(bytes.len(), 16 + 100_000 * 16 + 6 + 4 + 100_000 * 4, "2 000 010 bytes");
        let d = decode_notes(&bytes).unwrap();
        assert_eq!(d.ppq, 960);
        assert_eq!(d.notes.len(), notes.len());
        assert_eq!(d.notes, notes);
        assert_eq!(d.next_note_id, 100_001);
    }

    #[test]
    fn amev_rejects_garbage_and_newer_versions() {
        assert!(decode_notes(&[0u8; 8]).is_err());
        let mut bytes = encode_notes(960, &[], 1);
        bytes[4] = 0xFF; // version -> huge
        assert!(decode_notes(&bytes).is_err());
    }

    #[test]
    fn amev_note_id_column_round_trips() {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(7) },
            MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(9) },
        ];
        let bytes = encode_notes(960, &notes, 10);
        let d = decode_notes(&bytes).unwrap();
        assert_eq!(d.ppq, 960);
        assert_eq!(d.notes, notes, "ids survive the chunk");
        assert_eq!(d.next_note_id, 10, "watermark persisted in the header block");
    }

    #[test]
    fn amev_v1_chunk_without_column_reads_tolerantly() {
        // A hand-built v1 chunk: mask = COLUMNS_CORE only, no appended blocks.
        // Loader mints 1..=count and watermark count+1 (upgrade-on-read).
        let legacy = {
            let notes = vec![
                MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(0) },
                MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(0) },
            ];
            let mut b = encode_notes(960, &notes, 3);
            // Strip the appended block and clear the mask bit to fake a v1 file:
            b.truncate(16 + 2 * 16);
            b[6] = (COLUMNS_CORE & 0xFF) as u8;
            b[7] = (COLUMNS_CORE >> 8) as u8;
            b
        };
        let d = decode_notes(&legacy).unwrap();
        assert_eq!(d.notes[0].note_id, NoteId(1));
        assert_eq!(d.notes[1].note_id, NoteId(2));
        assert_eq!(d.next_note_id, 3);
    }

    /// Reviewer finding 1 (CRITICAL): a mask claiming COLUMNS_NOTE_ID is
    /// present but whose block is missing (truncated away, mask left
    /// stale) must Err — NOT silently fall through to the v1-upgrade path
    /// and mint fresh sequential ids, which would mask structural
    /// corruption as a normal legacy chunk.
    #[test]
    fn amev_mask_claims_note_id_column_but_block_missing_is_rejected() {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0, note_id: NoteId(1) },
            MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0, note_id: NoteId(2) },
        ];
        let mut bytes = encode_notes(960, &notes, 3);
        // Truncate away the appended note-id block WITHOUT clearing the mask
        // bit (unlike the v1-tolerant test above, which clears it) — the
        // mask still claims 0x0002 is present.
        bytes.truncate(16 + 2 * 16);
        let mask = u16::from_le_bytes([bytes[6], bytes[7]]);
        assert_eq!(mask & COLUMNS_NOTE_ID, COLUMNS_NOTE_ID, "mask bit left set");
        let err = decode_notes(&bytes).unwrap_err();
        assert!(err.contains("COLUMNS_NOTE_ID") || err.contains("0002"), "{err}");
    }

    #[test]
    fn amev_unknown_column_blocks_are_skipped() {
        let notes = vec![MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 1, channel: 0, note_id: NoteId(1) }];
        let mut bytes = encode_notes(960, &notes, 2);
        // Append a fictional future column (bit 0x0004) and set its mask bit.
        bytes.extend_from_slice(&0x0004u16.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        let mask = u16::from_le_bytes([bytes[6], bytes[7]]) | 0x0004;
        bytes[6] = (mask & 0xFF) as u8;
        bytes[7] = (mask >> 8) as u8;
        let d = decode_notes(&bytes).unwrap();
        assert_eq!(d.notes, notes, "unknown trailing column ignored, known ones kept");
    }

    /// C-2/normative rule: a watermark at or below a present id is REPAIRED
    /// upward (never rejected) — a semantic Err here would feed persist.rs's
    /// degrade-to-empty path and destroy notes on the next save.
    #[test]
    fn amev_bad_watermark_is_repaired_upward_not_rejected() {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 1, channel: 0, note_id: NoteId(1) },
            MidiNote { tick: 1, length_ticks: 1, key: 61, velocity: 1, channel: 0, note_id: NoteId(5) },
        ];
        // Watermark of 3 is <= the present id 5 — must repair to 6, not Err.
        let bytes = encode_notes(960, &notes, 3);
        let d = decode_notes(&bytes).unwrap();
        assert_eq!(d.next_note_id, 6, "repaired to max_present_id + 1");
        assert_eq!(d.notes, notes, "notes themselves are untouched");
    }

    #[test]
    fn amev_truncated_note_id_column_is_rejected() {
        let notes = vec![MidiNote { tick: 0, length_ticks: 1, key: 60, velocity: 1, channel: 0, note_id: NoteId(1) }];
        let mut bytes = encode_notes(960, &notes, 2);
        // Layout of the trailing block: [colBit u16][byteLen u32][watermark
        // u32][id u32...]. Corrupt byteLen's MSB (little-endian) to claim far
        // more bytes than remain — structural corruption, correctly rejected
        // (unlike a merely-low watermark value, which is repaired instead).
        let len = bytes.len();
        bytes[len - 9] = 0xFF;
        assert!(decode_notes(&bytes).is_err());
    }
}
