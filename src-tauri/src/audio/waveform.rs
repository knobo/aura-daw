//! Waveform min/max tile pyramids ("audio mipmaps") and the binary AWTF wire
//! format served by `get_waveform_tile` (see docs/ARCHITECTURE.md §2.5).
//!
//! * LOD n bins cover `2^(8+n)` SOURCE samples (lod 0 = 256 samples/bin).
//! * A tile is `BINS_PER_TILE` (1024) bins.
//! * Pyramids are cached per clip under
//!   `<project>/cache/waveforms/<clipId>/lod<N>.bin`.
//!
//! Cache file layout (little-endian): `[magic u32 "AWPY"][channels u16]
//! [reserved u16][bins u32]` then per-channel planar `bins × (min i16, max i16)`.
//!
//! AWTF wire layout (little-endian): `[magic u32 "AWTF"][version u16]
//! [channels u16][lod u32][tileIndex u64][bins u32]` then per-channel planar
//! `bins × (min i16, max i16)`.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

/// lod 0 bin width = 2^BIN_SHIFT source samples.
pub const BIN_SHIFT: u32 = 8;
pub const BINS_PER_TILE: usize = 1024;
pub const MAX_LOD: u32 = 16;
pub const AWTF_MAGIC: u32 = 0x4157_5446; // "AWTF"
pub const AWTF_VERSION: u16 = 1;
pub const PYR_MAGIC: u32 = 0x4157_5059; // "AWPY"

#[inline]
fn to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Streaming builder: fold interleaved f32 samples into lod-0 bins as they
/// arrive (used by the disk-writer thread during recording), then derive the
/// higher LODs in `finish`.
pub struct PyramidBuilder {
    channels: usize,
    bin_size: usize,
    /// Samples already folded into the current (open) bin.
    filled: usize,
    /// Per-channel running (min, max) of the open bin.
    cur: Vec<(f32, f32)>,
    /// Per-channel planar lod-0 bins.
    lod0: Vec<Vec<(i16, i16)>>,
}

impl PyramidBuilder {
    pub fn new(channels: usize) -> Self {
        let channels = channels.max(1);
        Self {
            channels,
            bin_size: 1 << BIN_SHIFT,
            filled: 0,
            cur: vec![(f32::MAX, f32::MIN); channels],
            lod0: vec![Vec::new(); channels],
        }
    }

    fn close_bin(&mut self) {
        for ch in 0..self.channels {
            let (mn, mx) = self.cur[ch];
            let (mn, mx) = if mn > mx { (0.0, 0.0) } else { (mn, mx) };
            self.lod0[ch].push((to_i16(mn), to_i16(mx)));
            self.cur[ch] = (f32::MAX, f32::MIN);
        }
        self.filled = 0;
    }

    /// Push interleaved samples (`len` must be a multiple of `channels`).
    pub fn push_interleaved(&mut self, samples: &[f32]) {
        for frame in samples.chunks_exact(self.channels) {
            for (ch, &s) in frame.iter().enumerate() {
                let (mn, mx) = self.cur[ch];
                self.cur[ch] = (mn.min(s), mx.max(s));
            }
            self.filled += 1;
            if self.filled == self.bin_size {
                self.close_bin();
            }
        }
    }

    pub fn finish(mut self) -> Pyramid {
        if self.filled > 0 {
            self.close_bin();
        }
        let mut levels: Vec<Vec<Vec<(i16, i16)>>> = vec![self.lod0];
        loop {
            let prev = levels.last().unwrap();
            let bins = prev[0].len();
            if bins <= 1 || levels.len() as u32 > MAX_LOD {
                break;
            }
            let next: Vec<Vec<(i16, i16)>> = prev
                .iter()
                .map(|chan| {
                    chan.chunks(2)
                        .map(|pair| {
                            let (mut mn, mut mx) = pair[0];
                            if let Some(&(m2, x2)) = pair.get(1) {
                                mn = mn.min(m2);
                                mx = mx.max(x2);
                            }
                            (mn, mx)
                        })
                        .collect()
                })
                .collect();
            levels.push(next);
        }
        Pyramid { channels: self.channels as u16, levels }
    }
}

/// A complete LOD pyramid: `levels[lod][channel][bin] = (min, max)`.
pub struct Pyramid {
    pub channels: u16,
    pub levels: Vec<Vec<Vec<(i16, i16)>>>,
}

impl Pyramid {
    /// Build from a fully-loaded interleaved buffer (imports / project open).
    pub fn from_interleaved(samples: &[f32], channels: usize) -> Self {
        let mut b = PyramidBuilder::new(channels);
        b.push_interleaved(samples);
        b.finish()
    }

    /// Write all levels to `<dir>/lod<N>.bin` (dir is created).
    pub fn write_dir(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        for (lod, chans) in self.levels.iter().enumerate() {
            let bins = chans[0].len() as u32;
            let mut buf =
                Vec::with_capacity(12 + self.channels as usize * bins as usize * 4);
            buf.extend_from_slice(&PYR_MAGIC.to_le_bytes());
            buf.extend_from_slice(&self.channels.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&bins.to_le_bytes());
            for chan in chans {
                for &(mn, mx) in chan {
                    buf.extend_from_slice(&mn.to_le_bytes());
                    buf.extend_from_slice(&mx.to_le_bytes());
                }
            }
            let mut f = fs::File::create(dir.join(format!("lod{lod}.bin")))?;
            f.write_all(&buf)?;
        }
        Ok(())
    }
}

/// True if a pyramid cache already exists for this clip.
pub fn pyramid_exists(dir: &Path) -> bool {
    dir.join("lod0.bin").is_file()
}

/// Read one tile from the cache. Returns `Ok(None)` when the LOD file does
/// not exist; a tile index past the end yields zero bins.
pub fn read_tile(
    dir: &Path,
    lod: u32,
    tile_index: u64,
) -> io::Result<Option<(u16, Vec<(i16, i16)>)>> {
    let path = dir.join(format!("lod{lod}.bin"));
    let mut bytes = Vec::new();
    match fs::File::open(&path) {
        Ok(mut f) => f.read_to_end(&mut bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if bytes.len() < 12 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pyramid file too short"));
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != PYR_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad pyramid magic"));
    }
    let channels = u16::from_le_bytes(bytes[4..6].try_into().unwrap()).max(1);
    let bins = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let expected = 12 + channels as usize * bins * 4;
    if bytes.len() < expected {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pyramid file truncated"));
    }

    let start = (tile_index as usize).saturating_mul(BINS_PER_TILE).min(bins);
    let end = (start + BINS_PER_TILE).min(bins);
    let tile_bins = end - start;
    let mut data = Vec::with_capacity(channels as usize * tile_bins);
    for ch in 0..channels as usize {
        let chan_base = 12 + ch * bins * 4;
        for b in start..end {
            let o = chan_base + b * 4;
            let mn = i16::from_le_bytes(bytes[o..o + 2].try_into().unwrap());
            let mx = i16::from_le_bytes(bytes[o + 2..o + 4].try_into().unwrap());
            data.push((mn, mx));
        }
    }
    Ok(Some((channels, data)))
}

/// Encode a tile in the AWTF wire format. `data` is per-channel planar,
/// `channels * bins` entries.
pub fn encode_tile(channels: u16, lod: u32, tile_index: u64, data: &[(i16, i16)]) -> Vec<u8> {
    let channels = channels.max(1);
    let bins = (data.len() / channels as usize) as u32;
    let mut buf = Vec::with_capacity(24 + data.len() * 4);
    buf.extend_from_slice(&AWTF_MAGIC.to_le_bytes());
    buf.extend_from_slice(&AWTF_VERSION.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&lod.to_le_bytes());
    buf.extend_from_slice(&tile_index.to_le_bytes());
    buf.extend_from_slice(&bins.to_le_bytes());
    for &(mn, mx) in data {
        buf.extend_from_slice(&mn.to_le_bytes());
        buf.extend_from_slice(&mx.to_le_bytes());
    }
    buf
}

/// A valid AWTF tile with zero bins (unknown clip / missing LOD).
pub fn empty_tile(channels: u16, lod: u32, tile_index: u64) -> Vec<u8> {
    encode_tile(channels, lod, tile_index, &[])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir()
            .join(format!("aura-wf-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    /// Parse an AWTF byte buffer back into header + data (test helper).
    fn parse_awtf(buf: &[u8]) -> (u32, u16, u16, u32, u64, u32, Vec<(i16, i16)>) {
        let magic = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let version = u16::from_le_bytes(buf[4..6].try_into().unwrap());
        let channels = u16::from_le_bytes(buf[6..8].try_into().unwrap());
        let lod = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let tile = u64::from_le_bytes(buf[12..20].try_into().unwrap());
        let bins = u32::from_le_bytes(buf[20..24].try_into().unwrap());
        let mut data = Vec::new();
        for o in (24..buf.len()).step_by(4) {
            data.push((
                i16::from_le_bytes(buf[o..o + 2].try_into().unwrap()),
                i16::from_le_bytes(buf[o + 2..o + 4].try_into().unwrap()),
            ));
        }
        (magic, version, channels, lod, tile, bins, data)
    }

    #[test]
    fn lod0_bins_capture_min_max() {
        // 512 mono samples: first 256 ramp 0..1, next 256 constant -0.5
        let mut s: Vec<f32> = (0..256).map(|i| i as f32 / 255.0).collect();
        s.extend(std::iter::repeat(-0.5).take(256));
        let p = Pyramid::from_interleaved(&s, 1);
        assert_eq!(p.levels[0][0].len(), 2);
        let (mn0, mx0) = p.levels[0][0][0];
        assert_eq!(mn0, 0);
        assert_eq!(mx0, 32767);
        let (mn1, mx1) = p.levels[0][0][1];
        assert_eq!(mn1, to_i16(-0.5));
        assert_eq!(mx1, to_i16(-0.5));
        // lod1 combines both bins
        assert_eq!(p.levels[1][0].len(), 1);
        assert_eq!(p.levels[1][0][0], (to_i16(-0.5), 32767));
    }

    #[test]
    fn partial_trailing_bin_is_flushed() {
        let s = vec![0.25f32; 300]; // 1 full bin + 44 leftover
        let p = Pyramid::from_interleaved(&s, 1);
        assert_eq!(p.levels[0][0].len(), 2);
        assert_eq!(p.levels[0][0][1], (to_i16(0.25), to_i16(0.25)));
    }

    #[test]
    fn stereo_channels_stay_planar() {
        // L = +0.5, R = -0.5, 256 frames
        let mut s = Vec::new();
        for _ in 0..256 {
            s.push(0.5);
            s.push(-0.5);
        }
        let p = Pyramid::from_interleaved(&s, 2);
        assert_eq!(p.channels, 2);
        assert_eq!(p.levels[0][0][0], (to_i16(0.5), to_i16(0.5)));
        assert_eq!(p.levels[0][1][0], (to_i16(-0.5), to_i16(-0.5)));
    }

    #[test]
    fn pyramid_level_count_matches_data_size() {
        // 256 * 16 samples -> 16 lod0 bins -> levels: 16,8,4,2,1 = 5 levels
        let s = vec![0.1f32; 256 * 16];
        let p = Pyramid::from_interleaved(&s, 1);
        assert_eq!(p.levels.len(), 5);
        assert_eq!(p.levels.last().unwrap()[0].len(), 1);
    }

    #[test]
    fn streaming_builder_matches_batch() {
        let s: Vec<f32> = (0..2048).map(|i| ((i as f32) * 0.01).sin()).collect();
        let batch = Pyramid::from_interleaved(&s, 1);
        let mut b = PyramidBuilder::new(1);
        for chunk in s.chunks(100) {
            b.push_interleaved(chunk);
        }
        let streamed = b.finish();
        assert_eq!(batch.levels.len(), streamed.levels.len());
        assert_eq!(batch.levels[0][0], streamed.levels[0][0]);
    }

    #[test]
    fn write_read_tile_roundtrip() {
        let dir = tmp_dir("roundtrip");
        // 3000 bins worth of mono audio at lod0: 3000*256 samples of a ramp
        let n = 3000 * 256;
        let s: Vec<f32> = (0..n).map(|i| (i as f32 / n as f32) * 2.0 - 1.0).collect();
        let p = Pyramid::from_interleaved(&s, 1);
        p.write_dir(&dir).unwrap();
        assert!(pyramid_exists(&dir));

        // tile 0 has 1024 bins, tile 2 has 3000-2048 = 952 bins
        let (ch, t0) = read_tile(&dir, 0, 0).unwrap().unwrap();
        assert_eq!(ch, 1);
        assert_eq!(t0.len(), 1024);
        let (_, t2) = read_tile(&dir, 0, 2).unwrap().unwrap();
        assert_eq!(t2.len(), 952);
        // tile past the end: zero bins
        let (_, t9) = read_tile(&dir, 0, 9).unwrap().unwrap();
        assert_eq!(t9.len(), 0);
        // ramp: min/max increase monotonically across bins
        assert!(t0[0].0 < t0[1023].0);
        assert_eq!(t0, {
            let full: Vec<(i16, i16)> = p.levels[0][0][0..1024].to_vec();
            full
        });
        // 3000 bins halve down to 1 at lod 12; lod 13 was never written
        assert!(read_tile(&dir, 12, 0).unwrap().is_some());
        assert!(read_tile(&dir, 13, 0).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn awtf_encoding_roundtrip() {
        let data = vec![(-100i16, 200i16), (-300, 400), (-1, 1), (-2, 2)];
        let buf = encode_tile(2, 3, 7, &data);
        assert_eq!(buf.len(), 24 + 16);
        let (magic, ver, ch, lod, tile, bins, parsed) = parse_awtf(&buf);
        assert_eq!(magic, AWTF_MAGIC);
        assert_eq!(ver, 1);
        assert_eq!(ch, 2);
        assert_eq!(lod, 3);
        assert_eq!(tile, 7);
        assert_eq!(bins, 2, "4 entries / 2 channels");
        assert_eq!(parsed, data);
    }

    #[test]
    fn empty_tile_has_valid_header() {
        let buf = empty_tile(2, 5, 11);
        assert_eq!(buf.len(), 24);
        let (magic, _, ch, lod, tile, bins, _) = parse_awtf(&buf);
        assert_eq!((magic, ch, lod, tile, bins), (AWTF_MAGIC, 2, 5, 11, 0));
    }
}
