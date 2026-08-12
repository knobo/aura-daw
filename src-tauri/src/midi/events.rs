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

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use super::types::MidiNote;

pub const AMEV_MAGIC: u32 = 0x414D_4556;
pub const AMEV_VERSION: u16 = 1;
pub const COLUMNS_CORE: u16 = 0x0001;
pub const KIND_NOTE: u8 = 0;
pub const KIND_AUTOMATION: u8 = 1;

/// Encode note events into an AMEV chunk.
pub fn encode_notes(ppq: u32, notes: &[MidiNote]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + notes.len() * 16);
    out.write_u32::<LittleEndian>(AMEV_MAGIC).unwrap();
    out.write_u16::<LittleEndian>(AMEV_VERSION).unwrap();
    out.write_u16::<LittleEndian>(COLUMNS_CORE).unwrap();
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
    out
}

/// Decode an AMEV chunk; returns `(ppq, notes)`. Automation records are
/// skipped (they get their own accessor when automation lands).
pub fn decode_notes(bytes: &[u8]) -> Result<(u32, Vec<MidiNote>), String> {
    let mut r = std::io::Cursor::new(bytes);
    let magic = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    if magic != AMEV_MAGIC {
        return Err(format!("not an AMEV chunk (magic {magic:#010x})"));
    }
    let version = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    if version > AMEV_VERSION {
        return Err(format!("AMEV version {version} is newer than supported {AMEV_VERSION}"));
    }
    let _columns = r.read_u16::<LittleEndian>().map_err(|e| e.to_string())?;
    let ppq = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    let count = r.read_u32::<LittleEndian>().map_err(|e| e.to_string())?;
    let mut notes = Vec::with_capacity(count as usize);
    for _ in 0..count {
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
            });
        }
    }
    Ok((ppq, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amev_roundtrip() {
        let notes = vec![
            MidiNote { tick: 0, length_ticks: 480, key: 60, velocity: 100, channel: 0 },
            MidiNote { tick: 480, length_ticks: 240, key: 64, velocity: 90, channel: 0 },
            MidiNote { tick: 720, length_ticks: 240, key: 67, velocity: 80, channel: 1 },
        ];
        let bytes = encode_notes(960, &notes);
        assert_eq!(bytes.len(), 16 + 3 * 16);
        let (ppq, decoded) = decode_notes(&bytes).unwrap();
        assert_eq!(ppq, 960);
        assert_eq!(decoded, notes);
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
            })
            .collect();
        let bytes = encode_notes(960, &notes);
        assert_eq!(bytes.len(), 16 + 100_000 * 16);
        let (ppq, decoded) = decode_notes(&bytes).unwrap();
        assert_eq!(ppq, 960);
        assert_eq!(decoded.len(), notes.len());
        assert_eq!(decoded, notes);
    }

    #[test]
    fn amev_rejects_garbage_and_newer_versions() {
        assert!(decode_notes(&[0u8; 8]).is_err());
        let mut bytes = encode_notes(960, &[]);
        bytes[4] = 0xFF; // version -> huge
        assert!(decode_notes(&bytes).is_err());
    }
}
