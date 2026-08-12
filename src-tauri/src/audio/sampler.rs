//! Sampler instrument — OWNED BY THE SAMPLER/INSTRUMENT-BUILDER AGENT
//! (phase 2, zone D). Scaffolded by the architect (round 2).
//!
//! # AURA SFZ SUBSET v1 (FROZEN — build against this, both zones)
//!
//! The sampler LOADS this subset; the Stable Audio Open instrument-builder
//! sidecar GENERATES it. Both agents code against this list so they can build
//! in parallel. Anything outside the subset is ignored with a warning (never
//! an error — Postel, like D-06).
//!
//! Headers: `<control>`, `<global>`, `<group>`, `<region>`. Opcodes defined
//! in `<global>`/`<group>` are inherited by regions (region wins).
//!
//! | Opcode            | Meaning                            | Default |
//! |-------------------|------------------------------------|---------|
//! | `default_path`    | (`<control>`) sample dir prefix    | ""      |
//! | `sample`          | wav/flac path (rel. to sfz file)   | required|
//! | `key`             | shorthand: lokey=hikey=pitch_keycenter | —   |
//! | `lokey`,`hikey`   | key range, 0..=127 or note names   | 0/127   |
//! | `pitch_keycenter` | root key, 0..=127 or note names    | 60      |
//! | `lovel`,`hivel`   | velocity range 1..=127             | 1/127   |
//! | `tune`            | cents, -100..=100                  | 0       |
//! | `transpose`       | semitones, -24..=24                | 0       |
//! | `volume`          | region gain dB, -144..=6           | 0       |
//! | `pan`             | -100..=100                         | 0       |
//! | `offset`          | sample start offset (frames)       | 0       |
//! | `loop_mode`       | `no_loop` \| `loop_continuous`     | no_loop |
//! | `loop_start`,`loop_end` | loop points (frames)         | 0/0     |
//! | `ampeg_attack`    | seconds, 0..=10                    | 0.001   |
//! | `ampeg_release`   | seconds, 0..=10                    | 0.05    |
//!
//! Note names: `c-1`..`g9`, `#` sharps only (e.g. `c4` = 60, `f#3` = 54).
//!
//! # Engine integration contract (frozen)
//!
//! * A loaded instrument compiles into an immutable `SamplerNode` snapshot
//!   (zones + preloaded/streamed sample data) installed on a `kind:"midi"`
//!   track by the usual RCU build-and-swap — never by mutating a running
//!   graph. Voice pool ([`MAX_VOICES`]) preallocated in `prepare`.
//! * Note events reach the node as sample-offset events within the block
//!   (see `crate::midi::synth::BlockNoteEvent`); the RT thread never maps
//!   ticks, parses SFZ, or touches the filesystem.
//! * Pitch mapping: playback rate = 2^((key - pitch_keycenter + transpose
//!   + tune/100) / 12), linear-interpolated resampling (v1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Preallocated voice pool size per sampler instance.
pub const MAX_VOICES: usize = 64;

/// One region/zone after parsing + inheritance resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SfzRegion {
    /// Sample path relative to the .sfz file (default_path applied).
    pub sample: String,
    pub lokey: u8,
    pub hikey: u8,
    pub pitch_keycenter: u8,
    pub lovel: u8,
    pub hivel: u8,
    /// Cents, -100..=100.
    pub tune: i16,
    /// Semitones, -24..=24.
    pub transpose: i8,
    /// dB.
    pub volume: f32,
    /// -100..=100 (SFZ convention).
    pub pan: f32,
    pub offset: u64,
    /// true = loop_continuous.
    pub looped: bool,
    pub loop_start: u64,
    pub loop_end: u64,
    pub ampeg_attack: f32,
    pub ampeg_release: f32,
}

impl Default for SfzRegion {
    fn default() -> Self {
        Self {
            sample: String::new(),
            lokey: 0,
            hikey: 127,
            pitch_keycenter: 60,
            lovel: 1,
            hivel: 127,
            tune: 0,
            transpose: 0,
            volume: 0.0,
            pan: 0.0,
            offset: 0,
            looped: false,
            loop_start: 0,
            loop_end: 0,
            ampeg_attack: 0.001,
            ampeg_release: 0.05,
        }
    }
}

/// A parsed instrument (control-plane object; the RT-side compiled form is
/// the owning agent's `SamplerNode`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SfzInstrument {
    pub name: String,
    /// Absolute path of the source .sfz file.
    pub sfz_path: String,
    pub regions: Vec<SfzRegion>,
}

/// Wire summary returned by `sampler_load_instrument` / `sampler_list_instruments`
/// (mirrors docs/ipc-schemas/sampler-instrument.schema.json `instrumentInfo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstrumentInfo {
    pub id: String,
    pub name: String,
    pub sfz_path: String,
    pub region_count: usize,
    pub key_low: u8,
    pub key_high: u8,
}

/// Loaded instruments, keyed by instrument id (control-plane only).
/// `instruments` holds the parsed source form; `compiled` the playback form
/// (samples decoded/resampled at load time — see `sampler_engine::compile`).
#[derive(Default)]
pub struct SamplerBank {
    pub instruments: HashMap<String, SfzInstrument>,
    pub compiled: HashMap<String, std::sync::Arc<super::sampler_engine::CompiledInstrument>>,
}

impl SamplerBank {
    pub fn info(&self, id: &str) -> Option<InstrumentInfo> {
        self.instruments.get(id).map(|i| instrument_info(id, i))
    }

    /// Playback form for an instrument id (used by preview and, once zone
    /// C's midi-track sources land, by the engine graph build).
    pub fn compiled(
        &self,
        id: &str,
    ) -> Option<std::sync::Arc<super::sampler_engine::CompiledInstrument>> {
        self.compiled.get(id).cloned()
    }

    pub fn list(&self) -> Vec<InstrumentInfo> {
        let mut v: Vec<_> =
            self.instruments.iter().map(|(id, i)| instrument_info(id, i)).collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

// ---------------------------------------------------------------------------
// Engine-visible bank registration (architect merge, phase-2 integration).
// The engine control thread (graph rebuild) and the post-job instrument
// auto-register hook both need the app's SamplerBank without threading it
// through frozen signatures — same pattern as midi::playback::register_store.
// ---------------------------------------------------------------------------

static ENGINE_BANK: std::sync::OnceLock<std::sync::Arc<parking_lot::Mutex<SamplerBank>>> =
    std::sync::OnceLock::new();

/// Register the app-wide sampler bank for engine rebuilds and post-job
/// instrument loading. Called once from `audio::init`; first registration
/// wins. Unit tests construct banks directly and never touch the global.
pub fn register_bank(bank: std::sync::Arc<parking_lot::Mutex<SamplerBank>>) {
    let _ = ENGINE_BANK.set(bank);
}

/// The registered app-wide sampler bank, if any (None in unit tests).
pub fn registered_bank() -> Option<&'static std::sync::Arc<parking_lot::Mutex<SamplerBank>>> {
    ENGINE_BANK.get()
}

pub fn instrument_info(id: &str, i: &SfzInstrument) -> InstrumentInfo {
    InstrumentInfo {
        id: id.to_string(),
        name: i.name.clone(),
        sfz_path: i.sfz_path.clone(),
        region_count: i.regions.len(),
        key_low: i.regions.iter().map(|r| r.lokey).min().unwrap_or(0),
        key_high: i.regions.iter().map(|r| r.hikey).max().unwrap_or(127),
    }
}

// ---------------------------------------------------------------------------
// SFZ subset parser (real implementation — freezes the subset executably)
// ---------------------------------------------------------------------------

/// Parse SFZ text (subset above). `sfz_dir` resolves nothing here — sample
/// paths stay relative; the loader resolves them against the .sfz location.
/// Unknown opcodes/headers are ignored. Regions without a `sample` are
/// dropped. `//` starts a comment.
pub fn parse_sfz(text: &str) -> Result<Vec<SfzRegion>, String> {
    #[derive(Clone, Default)]
    struct Scope(HashMap<String, String>);

    let mut default_path = String::new();
    let mut global = Scope::default();
    let mut group = Scope::default();
    // (group snapshot at region creation, region opcodes)
    let mut regions: Vec<(Scope, Scope)> = Vec::new();
    // 0 = control, 1 = global, 2 = group, 3 = region
    let mut section: u8 = 1;

    for raw_line in text.lines() {
        let line = match raw_line.find("//") {
            Some(i) => &raw_line[..i],
            None => raw_line,
        };
        for token in tokenize(line) {
            match token {
                Token::Header(h) => match h.as_str() {
                    "control" => section = 0,
                    "global" => section = 1,
                    "group" => {
                        section = 2;
                        group = Scope::default();
                    }
                    "region" => {
                        section = 3;
                        regions.push((group.clone(), Scope::default()));
                    }
                    _ => section = 255, // unknown header: ignore its opcodes
                },
                Token::Opcode(k, v) => match section {
                    0 => {
                        if k == "default_path" {
                            default_path = v;
                        }
                    }
                    1 => {
                        global.0.insert(k, v);
                    }
                    2 => {
                        group.0.insert(k, v);
                    }
                    3 => {
                        if let Some((_, r)) = regions.last_mut() {
                            r.0.insert(k, v);
                        }
                    }
                    _ => {}
                },
            }
        }
    }

    let mut out = Vec::new();
    for (region_group, r) in regions {
        // Inheritance: global < group (as of region creation) < region.
        let mut ops = global.0.clone();
        ops.extend(region_group.0);
        ops.extend(r.0);
        let Some(sample) = ops.get("sample") else { continue };
        let mut reg = SfzRegion {
            sample: join_sample_path(&default_path, sample),
            ..SfzRegion::default()
        };
        if let Some(v) = ops.get("key") {
            let k = parse_key(v)?;
            reg.lokey = k;
            reg.hikey = k;
            reg.pitch_keycenter = k;
        }
        if let Some(v) = ops.get("lokey") {
            reg.lokey = parse_key(v)?;
        }
        if let Some(v) = ops.get("hikey") {
            reg.hikey = parse_key(v)?;
        }
        if let Some(v) = ops.get("pitch_keycenter") {
            reg.pitch_keycenter = parse_key(v)?;
        }
        if let Some(v) = ops.get("lovel") {
            reg.lovel = parse_num::<u8>(v, "lovel")?.clamp(1, 127);
        }
        if let Some(v) = ops.get("hivel") {
            reg.hivel = parse_num::<u8>(v, "hivel")?.clamp(1, 127);
        }
        if let Some(v) = ops.get("tune") {
            reg.tune = parse_num::<i16>(v, "tune")?.clamp(-100, 100);
        }
        if let Some(v) = ops.get("transpose") {
            reg.transpose = parse_num::<i8>(v, "transpose")?.clamp(-24, 24);
        }
        if let Some(v) = ops.get("volume") {
            reg.volume = parse_num::<f32>(v, "volume")?.clamp(-144.0, 6.0);
        }
        if let Some(v) = ops.get("pan") {
            reg.pan = parse_num::<f32>(v, "pan")?.clamp(-100.0, 100.0);
        }
        if let Some(v) = ops.get("offset") {
            reg.offset = parse_num::<u64>(v, "offset")?;
        }
        if let Some(v) = ops.get("loop_mode") {
            reg.looped = v == "loop_continuous";
        }
        if let Some(v) = ops.get("loop_start") {
            reg.loop_start = parse_num::<u64>(v, "loop_start")?;
        }
        if let Some(v) = ops.get("loop_end") {
            reg.loop_end = parse_num::<u64>(v, "loop_end")?;
        }
        if let Some(v) = ops.get("ampeg_attack") {
            reg.ampeg_attack = parse_num::<f32>(v, "ampeg_attack")?.clamp(0.0, 10.0);
        }
        if let Some(v) = ops.get("ampeg_release") {
            reg.ampeg_release = parse_num::<f32>(v, "ampeg_release")?.clamp(0.0, 10.0);
        }
        if reg.lokey > reg.hikey {
            return Err(format!("region lokey {} > hikey {}", reg.lokey, reg.hikey));
        }
        out.push(reg);
    }
    Ok(out)
}

/// Load + parse an instrument from an .sfz file on disk.
pub fn load_instrument(sfz_path: &Path, name: Option<String>) -> Result<SfzInstrument, String> {
    let text = std::fs::read_to_string(sfz_path)
        .map_err(|e| format!("read {}: {e}", sfz_path.display()))?;
    let regions = parse_sfz(&text)?;
    if regions.is_empty() {
        return Err(format!("no playable regions in {}", sfz_path.display()));
    }
    let name = name.unwrap_or_else(|| {
        sfz_path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Instrument".into())
    });
    Ok(SfzInstrument {
        name,
        sfz_path: sfz_path.to_string_lossy().into_owned(),
        regions,
    })
}

/// Resolve a region's sample path against the .sfz file location.
pub fn resolve_sample_path(sfz_path: &Path, sample: &str) -> PathBuf {
    let rel = sample.replace('\\', "/");
    sfz_path.parent().unwrap_or(Path::new(".")).join(rel)
}

enum Token {
    Header(String),
    Opcode(String, String),
}

/// SFZ lines mix `<header>` markers and `key=value` pairs; values may contain
/// spaces (sample paths) and run until the next `key=` or `<`.
fn tokenize(line: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = line.trim();
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix('<') {
            match stripped.find('>') {
                Some(end) => {
                    out.push(Token::Header(stripped[..end].trim().to_lowercase()));
                    rest = stripped[end + 1..].trim_start();
                }
                None => break,
            }
            continue;
        }
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_lowercase();
        let after = &rest[eq + 1..];
        // Value ends at the next " key=" boundary or '<'.
        let mut end = after.len();
        for (i, _) in after.match_indices(|c: char| c.is_whitespace()) {
            let tail = after[i..].trim_start();
            if tail.starts_with('<')
                || tail.split_whitespace().next().is_some_and(|w| w.contains('='))
            {
                end = i;
                break;
            }
        }
        out.push(Token::Opcode(key, after[..end].trim().to_string()));
        rest = after[end..].trim_start();
    }
    out
}

fn parse_num<T: std::str::FromStr>(v: &str, opcode: &str) -> Result<T, String> {
    v.parse::<T>().map_err(|_| format!("invalid {opcode} value: {v}"))
}

/// Key: either a MIDI number or a note name like `c4`, `f#3`, `a-1`.
fn parse_key(v: &str) -> Result<u8, String> {
    if let Ok(n) = v.parse::<i16>() {
        if (0..=127).contains(&n) {
            return Ok(n as u8);
        }
        return Err(format!("key out of range: {v}"));
    }
    let s = v.to_lowercase();
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err("empty key".into());
    }
    let base: i16 = match bytes[0] {
        b'c' => 0,
        b'd' => 2,
        b'e' => 4,
        b'f' => 5,
        b'g' => 7,
        b'a' => 9,
        b'b' => 11,
        _ => return Err(format!("invalid note name: {v}")),
    };
    let mut i = 1;
    let mut semi = base;
    if bytes.get(i) == Some(&b'#') {
        semi += 1;
        i += 1;
    }
    let octave: i16 = s[i..].parse().map_err(|_| format!("invalid note name: {v}"))?;
    let key = (octave + 1) * 12 + semi;
    if !(0..=127).contains(&key) {
        return Err(format!("note out of MIDI range: {v}"));
    }
    Ok(key as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIANO_SFZ: &str = r#"
// AURA test instrument
<control> default_path=samples/
<global> ampeg_release=0.4 volume=-3
<group> lovel=1 hivel=90
<region> sample=c4_soft.wav key=c4
<region> sample=g4_soft.wav lokey=g4 hikey=b4 pitch_keycenter=g4 tune=-5
<group> lovel=91 hivel=127
<region> sample=c4 hard.wav key=60 loop_mode=loop_continuous loop_start=100 loop_end=44000
<region> nosample=ignored.wav
"#;

    #[test]
    fn parses_subset_with_inheritance_and_note_names() {
        let regions = parse_sfz(PIANO_SFZ).unwrap();
        assert_eq!(regions.len(), 3, "region without sample is dropped");
        let r0 = &regions[0];
        assert_eq!(r0.sample, "samples/c4_soft.wav");
        assert_eq!((r0.lokey, r0.hikey, r0.pitch_keycenter), (60, 60, 60));
        assert_eq!((r0.lovel, r0.hivel), (1, 90), "group scope applies");
        assert_eq!(r0.ampeg_release, 0.4, "global scope applies");
        assert_eq!(r0.volume, -3.0);
        let r1 = &regions[1];
        assert_eq!((r1.lokey, r1.hikey, r1.pitch_keycenter), (67, 71, 67));
        assert_eq!(r1.tune, -5);
        let r2 = &regions[2];
        assert_eq!(r2.sample, "samples/c4 hard.wav", "paths may contain spaces");
        assert_eq!((r2.lovel, r2.hivel), (91, 127), "second group resets scope");
        assert!(r2.looped);
        assert_eq!((r2.loop_start, r2.loop_end), (100, 44000));
    }

    #[test]
    fn key_name_parsing() {
        assert_eq!(parse_key("c4").unwrap(), 60);
        assert_eq!(parse_key("f#3").unwrap(), 54);
        assert_eq!(parse_key("a-1").unwrap(), 9);
        assert_eq!(parse_key("g9").unwrap(), 127);
        assert!(parse_key("h2").is_err());
        assert!(parse_key("c99").is_err());
        assert!(parse_key("128").is_err());
    }

    #[test]
    fn rejects_inverted_key_range() {
        let sfz = "<region> sample=a.wav lokey=64 hikey=60";
        assert!(parse_sfz(sfz).is_err());
    }

    #[test]
    fn unknown_opcodes_and_headers_are_ignored() {
        let sfz = "<region> sample=a.wav cutoff=1000 fil_type=lpf_2p\n<curve> v000=1";
        let regions = parse_sfz(sfz).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].sample, "a.wav");
    }
}

fn join_sample_path(default_path: &str, sample: &str) -> String {
    let sample = sample.replace('\\', "/");
    if default_path.is_empty() {
        sample
    } else {
        let mut p = default_path.replace('\\', "/");
        if !p.ends_with('/') {
            p.push('/');
        }
        p + &sample
    }
}
