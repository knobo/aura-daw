//! `APTF` — the pitch track a take carries on disk.
//!
//! Derived data, not document state: `cache/pitch/<clipId>.bin`, a sibling
//! of the waveform tiles in `cache/waveforms/<clipId>` and keyed the same
//! way (`docs/backlog/pitch-as-data.md`, decision (c)). Deleting it costs a
//! re-analysis, nothing more; it never enters the op log, the journal or
//! undo.
//!
//! ## Layout (little-endian)
//!
//! | bytes | field |
//! |---|---|
//! | 4 | magic `APTF` |
//! | 2 | `u16` version |
//! | 1 | provenance ([`PitchProvenance`]) |
//! | 1 | reserved, written 0 |
//! | 4 | `u32` sample rate the positions are in |
//! | 4 | `u32` hop, in those samples |
//! | 8 | `u64` position of the FIRST frame |
//! | 4 | `u32` frame count |
//! | 12n | `(f32 hz, f32 clarity, f32 rms)` per frame |
//!
//! Two derivations, both deliberate:
//!
//! * **`voiced` is not stored** — it is `hz > 0.0` on read, which is why an
//!   unvoiced frame must be written with `hz = 0.0`.
//! * **`sample` is not stored per frame** — it is `first_sample + i * hop`.
//!   The header carries `first_sample` because the analyser timestamps the
//!   CENTRE of its window: the first frame sits ~15 ms in, and deriving
//!   positions from a bare `i * hop` would slide the whole curve early by
//!   that much. Reading snaps every frame onto that grid; the analyser's own
//!   timestamps wobble a few samples around it, because its integer
//!   position maths re-anchors at every chunk boundary. A few samples is
//!   under 0.2 ms against a 10 ms hop, and the grid is the honest shape of
//!   a fixed-hop analysis.
//!
//! ## Provenance, and why it is in v1
//!
//! The live fold in the recorder uses the CAUSAL median — a live detector
//! cannot look ahead, so the curve lags `hum_to_midi.py` by the median
//! half-kernel (20 ms), documented and deliberate. An offline pass over the
//! same audio can use the centred one and is strictly better. Scoring does
//! not care; pitch correction will. One byte now, a format version later.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::pitch::PitchFrame;

pub const PITCH_MAGIC: &[u8; 4] = b"APTF";
pub const PITCH_VERSION: u16 = 1;
/// Header size in bytes; frames start here.
const HEADER_LEN: usize = 28;
/// Bytes per frame: hz, clarity, rms.
const FRAME_LEN: usize = 12;

/// How a pitch track was produced. A consumer that needs the best available
/// curve (pitch correction) can tell whether it is holding one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PitchProvenance {
    /// The causal median — what a live detector can produce, and what the
    /// recorder fold and today's offline re-analysis both use. Lags the
    /// centred one by the median half-kernel, 20 ms.
    CausalMedian,
    /// The centred median: strictly better, only possible offline, and not
    /// implemented yet (Stage A of the auto-tune path is its first
    /// consumer). The byte exists now so that pass does not need a v2.
    CentredMedian,
}

impl PitchProvenance {
    fn to_byte(self) -> u8 {
        match self {
            PitchProvenance::CausalMedian => 0,
            PitchProvenance::CentredMedian => 1,
        }
    }

    fn from_byte(b: u8) -> Result<Self, String> {
        match b {
            0 => Ok(PitchProvenance::CausalMedian),
            1 => Ok(PitchProvenance::CentredMedian),
            other => Err(format!("unknown pitch-track provenance {other}")),
        }
    }
}

/// A take's pitch curve. `frames[i].sample` is `first_sample + i * hop`, in
/// the take's OWN sample rate — the caller maps it onto the timeline.
#[derive(Clone, Debug)]
pub struct PitchTrack {
    pub rate: u32,
    pub hop: u32,
    pub first_sample: u64,
    pub provenance: PitchProvenance,
    pub frames: Vec<PitchFrame>,
}

impl PitchTrack {
    /// Take the anchor from the frames themselves. They must be evenly
    /// spaced by `hop` — both producers (the recorder fold and
    /// `analyze_samples`) drive the analyser without dropping a frame, which
    /// is what makes that true.
    pub fn from_frames(rate: u32, hop: u32, provenance: PitchProvenance, frames: Vec<PitchFrame>) -> Self {
        let first_sample = frames.first().map(|f| f.sample).unwrap_or(0);
        Self { rate, hop, first_sample, provenance, frames }
    }
}

/// Folds interleaved audio into a pitch curve as it goes — the same shape
/// as [`PyramidBuilder`](super::waveform::PyramidBuilder), so the recorder's
/// writer thread can drive both from one drain of the ring.
///
/// Positions are TAKE-LOCAL, in the take's own sample rate: frame 0 is the
/// analyser's first window centre measured from the take's first sample.
/// Mapping onto the timeline is the caller's job, because only it knows the
/// clip's placement and the project rate.
pub struct PitchFolder {
    dec: super::decimate::Decimator,
    analyzer: super::pitch::PitchAnalyzer,
    /// Decimated 8 kHz mono, refilled per push.
    mono: Vec<f32>,
    /// The analyser's output, drained after every push: it stops appending
    /// at 2048 frames (a 20 s cap that exists for the RT path), so a long
    /// take must not accumulate in it.
    scratch: Vec<PitchFrame>,
    out: Vec<PitchFrame>,
    /// Samples that arrived without completing a frame, carried to the next
    /// push. A ring drain can split mid-frame; the decimator cannot.
    partial: Vec<f32>,
    channels: usize,
    rate: u32,
    /// Take-local position, in frames (per channel), of the next sample.
    position: u64,
}

impl PitchFolder {
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            dec: super::decimate::Decimator::new(rate),
            analyzer: super::pitch::PitchAnalyzer::new(),
            mono: Vec::new(),
            scratch: Vec::new(),
            out: Vec::new(),
            partial: Vec::new(),
            channels: channels.max(1) as usize,
            rate,
            position: 0,
        }
    }

    /// The hop between frames, in the take's samples.
    pub fn hop(&self) -> u32 {
        ((self.rate as f32) * super::yin::FRAME_HOP_S) as u32
    }

    pub fn push_interleaved(&mut self, input: &[f32]) {
        if input.is_empty() {
            return;
        }
        // `partial` is normally empty; joining through it costs one memcpy
        // on a disk-writer thread and removes a whole class of bug.
        let mut buf = std::mem::take(&mut self.partial);
        buf.extend_from_slice(input);
        let whole = (buf.len() / self.channels) * self.channels;
        if whole > 0 {
            let mut mono = std::mem::take(&mut self.mono);
            mono.clear();
            self.dec.process(&buf[..whole], self.channels, &mut mono);
            if !mono.is_empty() {
                let mut scratch = std::mem::take(&mut self.scratch);
                scratch.clear();
                self.analyzer.push(&mono, self.position, self.rate, &mut scratch);
                self.out.extend(scratch.iter().copied());
                self.scratch = scratch;
            }
            self.mono = mono;
            self.position += (whole / self.channels) as u64;
            buf.drain(..whole);
        }
        self.partial = buf;
    }

    pub fn finish(self) -> PitchTrack {
        let hop = ((self.rate as f32) * super::yin::FRAME_HOP_S) as u32;
        PitchTrack::from_frames(self.rate, hop.max(1), PitchProvenance::CausalMedian, self.out)
    }
}

/// Analyse a whole buffer at once — the offline path (`pitch_analyze_clip`)
/// for takes recorded before this existed, or imported audio.
///
/// The curve is still [`PitchProvenance::CausalMedian`]: this reuses the
/// live detector, and being offline is not the same as being centred.
pub fn analyze_interleaved(input: &[f32], channels: u16, rate: u32) -> PitchTrack {
    let mut folder = PitchFolder::new(rate, channels);
    // Chunked so the analyser's internal buffers behave exactly as they do
    // under the recorder, rather than seeing one enormous push.
    for chunk in input.chunks(channels.max(1) as usize * 4096) {
        folder.push_interleaved(chunk);
    }
    folder.finish()
}

/// `cache/pitch/<clipId>.bin`, the sibling of `Store::cache_dir_for`.
pub fn track_path(project_dir: &Path, clip_id: &str) -> PathBuf {
    project_dir.join("cache").join("pitch").join(format!("{clip_id}.bin"))
}

pub fn write_track(path: &Path, track: &PitchTrack) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("pitch cache dir: {e}"))?;
    }
    let mut buf = Vec::with_capacity(HEADER_LEN + track.frames.len() * FRAME_LEN);
    buf.extend_from_slice(PITCH_MAGIC);
    buf.extend_from_slice(&PITCH_VERSION.to_le_bytes());
    buf.push(track.provenance.to_byte());
    buf.push(0); // reserved
    buf.extend_from_slice(&track.rate.to_le_bytes());
    buf.extend_from_slice(&track.hop.to_le_bytes());
    buf.extend_from_slice(&track.first_sample.to_le_bytes());
    buf.extend_from_slice(&(track.frames.len() as u32).to_le_bytes());
    for f in &track.frames {
        // An unvoiced frame writes hz = 0.0 — that zero IS the voiced flag.
        let hz = if f.voiced { f.hz } else { 0.0 };
        buf.extend_from_slice(&hz.to_le_bytes());
        buf.extend_from_slice(&f.clarity.to_le_bytes());
        buf.extend_from_slice(&f.rms.to_le_bytes());
    }
    // Write whole, then rename: a half-written cache file that still parses
    // would be indistinguishable from a short take.
    let tmp = path.with_extension("bin.tmp");
    {
        let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        file.write_all(&buf).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        file.sync_all().ok();
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

pub fn read_track(path: &Path) -> Result<PitchTrack, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() < HEADER_LEN || &bytes[0..4] != PITCH_MAGIC {
        return Err(format!("{} is not a pitch track", path.display()));
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    if version != PITCH_VERSION {
        return Err(format!("pitch track version {version}, expected {PITCH_VERSION}"));
    }
    let provenance = PitchProvenance::from_byte(bytes[6])?;
    let rate = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
    let hop = u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes"));
    let first_sample = u64::from_le_bytes(bytes[16..24].try_into().expect("8 bytes"));
    let count = u32::from_le_bytes(bytes[24..28].try_into().expect("4 bytes")) as usize;
    if hop == 0 {
        return Err("pitch track hop is zero".into());
    }
    if bytes.len() < HEADER_LEN + count * FRAME_LEN {
        return Err(format!(
            "pitch track is truncated: {} bytes for {count} frames",
            bytes.len()
        ));
    }
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER_LEN + i * FRAME_LEN;
        let hz = f32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
        let clarity = f32::from_le_bytes(bytes[at + 4..at + 8].try_into().expect("4 bytes"));
        let rms = f32::from_le_bytes(bytes[at + 8..at + 12].try_into().expect("4 bytes"));
        let voiced = hz > 0.0;
        frames.push(PitchFrame {
            sample: first_sample + (i as u64) * hop as u64,
            hz,
            midi: if voiced { super::yin::hz_to_midi(hz) } else { 0.0 },
            clarity,
            rms,
            voiced,
        });
    }
    Ok(PitchTrack { rate, hop, first_sample, provenance, frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-pitch-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn frame(sample: u64, hz: f32) -> PitchFrame {
        let voiced = hz > 0.0;
        PitchFrame {
            sample,
            hz,
            midi: if voiced { super::super::yin::hz_to_midi(hz) } else { 0.0 },
            clarity: if voiced { 0.9 } else { 0.0 },
            rms: if voiced { 0.2 } else { 0.0 },
            voiced,
        }
    }

    #[test]
    fn round_trips_a_track() {
        let dir = tmp_dir("round-trip");
        let path = dir.join("round-trip.bin");
        let frames = vec![frame(120, 220.0), frame(600, 0.0)];
        let track = PitchTrack::from_frames(48_000, 480, PitchProvenance::CausalMedian, frames);
        write_track(&path, &track).unwrap();
        let back = read_track(&path).unwrap();
        assert_eq!((back.rate, back.hop), (48_000, 480));
        assert_eq!(back.frames.len(), 2);
        assert!((back.frames[0].hz - 220.0).abs() < 1e-3);
        assert!((back.frames[0].midi - 57.0).abs() < 1e-3, "midi is derived, not stored");
        assert!(back.frames[0].voiced);
        assert!(!back.frames[1].voiced, "hz == 0 IS the unvoiced flag");
        // Positions are derived from the anchor plus the hop, not stored.
        assert_eq!(back.frames[0].sample, 120);
        assert_eq!(back.frames[1].sample, 600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The analyser timestamps the centre of its window, so the first frame
    /// is ~15 ms in. Dropping that anchor would slide every take's curve
    /// early by the same amount and nobody would see it in a waveform.
    #[test]
    fn the_first_frames_offset_survives() {
        let dir = tmp_dir("anchor");
        let path = dir.join("anchor.bin");
        let frames = vec![frame(661, 220.0), frame(1141, 221.0)];
        let track = PitchTrack::from_frames(44_100, 480, PitchProvenance::CausalMedian, frames);
        write_track(&path, &track).unwrap();
        let back = read_track(&path).unwrap();
        assert_eq!(back.first_sample, 661);
        assert_eq!(back.frames[0].sample, 661);
        assert_eq!(back.frames[1].sample, 1141);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decision (a) of `docs/backlog/pitch-as-data.md`: a consumer must be
    /// able to tell the live curve from a re-analysed one.
    #[test]
    fn provenance_survives_the_round_trip() {
        let dir = tmp_dir("provenance");
        let path = dir.join("p.bin");
        let track = PitchTrack::from_frames(
            48_000,
            480,
            PitchProvenance::CentredMedian,
            vec![frame(0, 110.0)],
        );
        write_track(&path, &track).unwrap();
        assert_eq!(read_track(&path).unwrap().provenance, PitchProvenance::CentredMedian);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_foreign_or_truncated_file() {
        let dir = tmp_dir("bad");
        let bad = dir.join("bad.bin");
        std::fs::write(&bad, b"NOPE").unwrap();
        assert!(read_track(&bad).is_err(), "a foreign magic must be rejected, not misread");

        // A valid header that claims more frames than the file holds.
        let path = dir.join("short.bin");
        let track = PitchTrack::from_frames(
            48_000,
            480,
            PitchProvenance::CausalMedian,
            vec![frame(0, 220.0), frame(480, 220.0)],
        );
        write_track(&path, &track).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 6);
        std::fs::write(&path, &bytes).unwrap();
        assert!(read_track(&path).is_err(), "a truncated tail must not read as a short take");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_future_version_is_refused_rather_than_misread() {
        let dir = tmp_dir("version");
        let path = dir.join("v9.bin");
        let track =
            PitchTrack::from_frames(48_000, 480, PitchProvenance::CausalMedian, vec![frame(0, 220.0)]);
        write_track(&path, &track).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[4] = 9;
        std::fs::write(&path, &bytes).unwrap();
        let err = read_track(&path).unwrap_err();
        assert!(err.contains("version 9"), "got {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Interleaved sine, `seconds` long, at the given rate.
    fn tone(hz: f32, seconds: f32, rate: u32, channels: u16) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        let mut out = Vec::with_capacity(n * channels as usize);
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5;
            for _ in 0..channels {
                out.push(s);
            }
        }
        out
    }

    #[test]
    fn the_folder_hears_a_tone_and_spaces_its_frames_by_the_hop() {
        let track = analyze_interleaved(&tone(220.0, 1.0, 48_000, 1), 1, 48_000);
        assert_eq!(track.hop, 480, "10 ms at 48 kHz");
        assert!(track.frames.len() > 80, "about 100 frames in a second, got {}", track.frames.len());
        let voiced: Vec<&PitchFrame> = track.frames.iter().filter(|f| f.voiced).collect();
        assert!(voiced.len() * 10 > track.frames.len() * 8, "a steady tone is voiced throughout");
        let mid = voiced[voiced.len() / 2];
        assert!((mid.hz - 220.0).abs() < 2.0, "detected {} Hz", mid.hz);
        for pair in track.frames.windows(2) {
            let step = pair[1].sample - pair[0].sample;
            // Not exactly the hop: the analyser re-anchors its integer
            // timestamp maths at every push, so a chunk boundary can shift a
            // frame by a few samples. A few samples is under 0.2 ms against a
            // 10 ms hop — the file snaps them back onto the grid.
            assert!(step.abs_diff(track.hop as u64) <= 8, "step {step} vs hop {}", track.hop);
        }
        assert!(track.first_sample > 0, "the first frame sits at its window centre");
    }

    /// A ring drain hands over whatever is in the buffer, which can split a
    /// stereo frame down the middle. If the folder let that shift the
    /// channel phase, every sample after it would be interleaved wrongly
    /// and the pitch would be garbage from that point on.
    #[test]
    fn a_push_split_mid_frame_matches_one_whole_push() {
        let audio = tone(196.0, 0.5, 44_100, 2);
        let whole = analyze_interleaved(&audio, 2, 44_100);

        let mut folder = PitchFolder::new(44_100, 2);
        // Deliberately odd, so every boundary lands mid-frame.
        for chunk in audio.chunks(777) {
            folder.push_interleaved(chunk);
        }
        let split = folder.finish();
        assert_eq!(split.frames.len(), whole.frames.len());
        for (a, b) in split.frames.iter().zip(&whole.frames) {
            assert!(a.sample.abs_diff(b.sample) <= 8, "{} vs {}", a.sample, b.sample);
            assert!((a.hz - b.hz).abs() < 1.0, "{} vs {}", a.hz, b.hz);
        }
    }

    #[test]
    fn a_folded_take_round_trips_through_the_file() {
        let dir = tmp_dir("fold");
        let path = dir.join("fold.bin");
        let track = analyze_interleaved(&tone(330.0, 0.4, 48_000, 1), 1, 48_000);
        write_track(&path, &track).unwrap();
        let back = read_track(&path).unwrap();
        assert_eq!(back.frames.len(), track.frames.len());
        assert_eq!(back.provenance, PitchProvenance::CausalMedian);
        for (a, b) in back.frames.iter().zip(&track.frames) {
            assert!(a.sample.abs_diff(b.sample) <= 8, "positions snap to the grid");
            assert_eq!(a.voiced, b.voiced);
            assert!((a.hz - b.hz).abs() < 1e-3);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(read_track(std::path::Path::new("/nonexistent/aura/pitch.bin")).is_err());
    }

    #[test]
    fn an_empty_track_round_trips() {
        let dir = tmp_dir("empty");
        let path = dir.join("empty.bin");
        let track = PitchTrack::from_frames(48_000, 480, PitchProvenance::CausalMedian, vec![]);
        write_track(&path, &track).unwrap();
        let back = read_track(&path).unwrap();
        assert!(back.frames.is_empty());
        assert_eq!(back.first_sample, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

}
