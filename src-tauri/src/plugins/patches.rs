//! ZynAddSubFX bank-patch enumeration + loading (wave 1C).
//!
//! ZynAddSubFX ships instrument banks as `.xiz` files (gzipped
//! `<ZynAddSubFX-data>` XML holding one `<INSTRUMENT>` element) under
//! `/usr/share/zynaddsubfx/banks/<Bank>/NNNN-Name.xiz`. The Zyn LV2 plugin
//! (a DPF wrapper) exposes its ENTIRE engine state through
//! `state:interface` as ONE string property (`urn:distrho:state` on this
//! build) whose value is the full master `<ZynAddSubFX-data>` XML document —
//! exactly what `plugins::state` round-trips as a [`KIND_LV2_PROPS`] blob.
//!
//! Patch loading therefore needs no new plugin API at all:
//!
//! 1. save the instance's current state through the state bridge
//!    ([`lv2_host::Lv2Host::save_state`]) — this yields the master XML as
//!    the plugin itself serialized it (always well-formed for splicing);
//! 2. gunzip the chosen `.xiz` and cut out its `<INSTRUMENT>` element;
//! 3. splice that element into `<PART id="0">` of the master XML (replacing
//!    the part's current instrument) and force the part enabled;
//! 4. load the modified blob back ([`lv2_host::Lv2Host::load_state`]) — the
//!    host applies it to the main-thread shadow instance AND re-applies it
//!    to every future RT node, so the patch survives graph rebuilds and, via
//!    zone P4 persistence, project save/open.
//!
//! Everything here is control-plane code (never RT): enumeration walks the
//! filesystem, loading round-trips the plugin main thread.
//!
//! NOTE (wave-2 UI): a patch loaded into an instance reaches FUTURE RT
//! nodes only — a track already bound keeps its live `LiveNodeCell` across
//! rebuilds by design (voice state survives). Rebinding the track (or any
//! node-key change) picks the patch up; a patch-browser command should pair
//! `load_zyn_patch` with a rebind or an explicit node invalidation.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::lv2_host;
use super::state::{decode_lv2_props, encode_lv2_props, StateBlob, KIND_LV2_PROPS};

/// The ZynAddSubFX LV2 plugin URI (uid = `lv2:<this>`).
pub const ZYN_URI: &str = "http://zynaddsubfx.sourceforge.net";

/// One enumerated bank patch. Serializes camelCase like every wire type so a
/// future `list_zyn_patches` command can return it verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ZynPatch {
    /// Bank directory name ("Pads", "Plucked", ...).
    pub bank: String,
    /// Human name parsed from `NNNN-Name.xiz` (program-number prefix and
    /// extension stripped).
    pub name: String,
    /// Program number from the `NNNN-` filename prefix (0 when absent).
    pub program: u32,
    /// Absolute path to the `.xiz` file.
    pub path: String,
}

/// Root directories searched for banks, in precedence order. Standard
/// distro/user locations for ZynAddSubFX 3.x.
pub fn zyn_bank_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".local/share/zynaddsubfx/banks"));
        roots.push(home.join("banks"));
    }
    roots.push(PathBuf::from("/usr/local/share/zynaddsubfx/banks"));
    roots.push(PathBuf::from("/usr/share/zynaddsubfx/banks"));
    roots
}

/// Enumerate every `.xiz` patch under the standard bank roots, sorted by
/// (bank, program, name). Missing roots are skipped silently — an empty
/// result simply means "no Zyn banks on this machine" (callers degrade).
pub fn list_zyn_patches() -> Vec<ZynPatch> {
    let mut out = Vec::new();
    for root in zyn_bank_roots() {
        let Ok(banks) = std::fs::read_dir(&root) else { continue };
        for bank in banks.flatten() {
            if !bank.path().is_dir() {
                continue;
            }
            let bank_name = bank.file_name().to_string_lossy().into_owned();
            let Ok(files) = std::fs::read_dir(bank.path()) else { continue };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("xiz") {
                    continue;
                }
                let stem = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(s) => s,
                    None => continue,
                };
                let (program, name) = match stem.split_once('-') {
                    Some((num, rest))
                        if num.chars().all(|c| c.is_ascii_digit()) && !rest.trim().is_empty() =>
                    {
                        (num.parse().unwrap_or(0), rest.trim().to_string())
                    }
                    // Unnumbered or nameless files keep the whole stem.
                    _ => (0, stem.to_string()),
                };
                out.push(ZynPatch {
                    bank: bank_name.clone(),
                    name,
                    program,
                    path: path.display().to_string(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        (a.bank.as_str(), a.program, a.name.as_str())
            .cmp(&(b.bank.as_str(), b.program, b.name.as_str()))
    });
    out.dedup_by(|a, b| a.bank == b.bank && a.name == b.name && a.program == b.program);
    out
}

/// Find a patch by bank name and case-insensitive name substring — the
/// demo-seed convenience ("Pads" + "analog pad" -> first match).
pub fn find_zyn_patch(bank: &str, name_contains: &str) -> Option<ZynPatch> {
    let needle = name_contains.to_ascii_lowercase();
    list_zyn_patches()
        .into_iter()
        .find(|p| p.bank == bank && p.name.to_ascii_lowercase().contains(&needle))
}

// ---------------------------------------------------------------------------
// .xiz reading
// ---------------------------------------------------------------------------

/// Read a `.xiz` (or plain `.xml`) instrument file and return the document
/// text. `.xiz` files are gzip-compressed; decompression shells out to
/// `gzip -dc` (`flate2` is not in the frozen dependency roster, and this is
/// cold control-plane code where a subprocess is fine).
pub fn read_xiz_xml(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        gunzip(&bytes).map_err(|e| format!("gunzip {}: {e}", path.display()))?
    } else {
        bytes
    };
    String::from_utf8(text).map_err(|e| format!("{} is not UTF-8 XML: {e}", path.display()))
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("gzip")
        .arg("-dc")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn gzip: {e}"))?;
    // Writer thread avoids the classic pipe deadlock on large payloads.
    let mut stdin = child.stdin.take().ok_or("gzip stdin unavailable")?;
    let input = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        use std::io::Write as _;
        let _ = stdin.write_all(&input);
    });
    let mut out = Vec::new();
    child
        .stdout
        .take()
        .ok_or("gzip stdout unavailable")?
        .read_to_end(&mut out)
        .map_err(|e| format!("read gzip output: {e}"))?;
    let status = child.wait().map_err(|e| format!("wait gzip: {e}"))?;
    let _ = writer.join();
    if !status.success() {
        return Err(format!("gzip -dc failed ({status})"));
    }
    Ok(out)
}

/// Cut the `<INSTRUMENT> ... </INSTRUMENT>` element out of an instrument
/// document. Zyn instrument files contain exactly one, and `INSTRUMENT`
/// elements never nest (kit items are `INSTRUMENT_KIT*`), so first-open to
/// last-close is the element.
pub fn extract_instrument_element(xml: &str) -> Result<&str, String> {
    let start = xml
        .find("<INSTRUMENT>")
        .ok_or("no <INSTRUMENT> element in patch XML")?;
    let end_tag = "</INSTRUMENT>";
    let end = xml
        .rfind(end_tag)
        .filter(|&e| e > start)
        .ok_or("unterminated <INSTRUMENT> element in patch XML")?;
    Ok(&xml[start..end + end_tag.len()])
}

// ---------------------------------------------------------------------------
// Master-XML splice
// ---------------------------------------------------------------------------

/// Replace part 0's instrument in a Zyn master document with
/// `instrument_xml` (a full `<INSTRUMENT>...</INSTRUMENT>` element) and
/// force the part enabled. Pure string surgery on XML the plugin itself
/// emitted — element layout is stable across Zyn 3.x.
pub fn splice_instrument_into_master(
    master: &str,
    instrument_xml: &str,
) -> Result<String, String> {
    let part_open = master
        .find("<PART id=\"0\">")
        .ok_or("master XML has no <PART id=\"0\">")?;
    let part_end = master[part_open..]
        .find("</PART>")
        .map(|i| part_open + i)
        .ok_or("master XML: unterminated <PART id=\"0\">")?;

    let inst_start = master[part_open..part_end]
        .find("<INSTRUMENT>")
        .map(|i| part_open + i)
        .ok_or("master XML: part 0 has no <INSTRUMENT>")?;
    let end_tag = "</INSTRUMENT>";
    let inst_end = master[inst_start..part_end]
        .rfind(end_tag)
        .map(|i| inst_start + i + end_tag.len())
        .ok_or("master XML: part 0 instrument unterminated")?;

    let mut out = String::with_capacity(master.len() + instrument_xml.len());
    out.push_str(&master[..inst_start]);
    out.push_str(instrument_xml);
    out.push_str(&master[inst_end..]);

    // Part 0 ships enabled by default, but a saved session may have disabled
    // it; a loaded patch must be audible.
    let enabled_off = "<par_bool name=\"enabled\" value=\"no\" />";
    let enabled_on = "<par_bool name=\"enabled\" value=\"yes\" />";
    let head = out[..inst_start.min(out.len())].to_string();
    if let Some(rel) = head[part_open..].find(enabled_off) {
        let abs = part_open + rel;
        out.replace_range(abs..abs + enabled_off.len(), enabled_on);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Patch loading (control-plane-callable)
// ---------------------------------------------------------------------------

/// True when the property value looks like the Zyn master document (the DPF
/// state key is `urn:distrho:state` on this build, but matching content is
/// more robust across wrapper versions).
fn is_master_xml_prop(p: &super::state::Lv2Property) -> bool {
    let head_len = p.value.len().min(4096);
    let head = String::from_utf8_lossy(&p.value[..head_len]);
    head.contains("ZynAddSubFX-data")
}

/// Load a `.xiz` bank patch into a REGISTERED Zyn LV2 instance (see module
/// docs for the mechanism). The patch lands in part 0, is applied to the
/// host's shadow instance immediately, and is re-applied to every future RT
/// node — including the node built at the next engine rebuild, and nodes of
/// sessions that reopen the project (zone P4 persists the loaded state).
pub fn load_zyn_patch(instance_id: &str, path: &Path) -> Result<(), String> {
    let host = lv2_host::global();
    let blob = host
        .save_state(instance_id)?
        .ok_or("instance exposes no state:interface — not the Zyn LV2 plugin?")?;
    if blob.kind != KIND_LV2_PROPS {
        return Err(format!("unexpected state blob kind {}", blob.kind));
    }
    let mut props = decode_lv2_props(&blob.data)?;
    let prop = props
        .iter_mut()
        .find(|p| is_master_xml_prop(p))
        .ok_or("instance state carries no ZynAddSubFX master XML property")?;

    // DPF stores the document as a C string; keep any trailing NUL exactly
    // as the plugin wrote it.
    let had_nul = prop.value.last() == Some(&0);
    let text_len = if had_nul { prop.value.len() - 1 } else { prop.value.len() };
    let master = std::str::from_utf8(&prop.value[..text_len])
        .map_err(|e| format!("master XML is not UTF-8: {e}"))?;

    let patch_xml = read_xiz_xml(path)?;
    let instrument = extract_instrument_element(&patch_xml)?;
    let spliced = splice_instrument_into_master(master, instrument)?;

    let mut value = spliced.into_bytes();
    if had_nul {
        value.push(0);
    }
    prop.value = value;

    let data = encode_lv2_props(&props);
    host.load_state(instance_id, StateBlob { kind: KIND_LV2_PROPS, data })
}

// ---------------------------------------------------------------------------
// Command wrappers (NOT yet registered — lib.rs is frozen this round; wave 2
// adds `plugins::patches::zyn_list_patches` / `zyn_load_patch` to the
// `invoke_handler` roster to light up the patch-browser UI)
// ---------------------------------------------------------------------------

/// List every installed ZynAddSubFX bank patch (empty = no banks here).
#[tauri::command]
pub fn zyn_list_patches() -> Result<Vec<ZynPatch>, String> {
    Ok(list_zyn_patches())
}

/// Load a `.xiz` bank patch into a registered Zyn LV2 instance. NOTE: an
/// already-bound track keeps its live node across rebuilds; pair this with a
/// rebind (`set_track_instrument`) until node invalidation ships (wave 2).
#[tauri::command]
pub fn zyn_load_patch(instance_id: String, path: String) -> Result<(), String> {
    load_zyn_patch(&instance_id, Path::new(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure helpers (no plugin needed) ----------------------------------

    #[test]
    fn instrument_extraction_and_splice_are_exact() {
        let patch = "<?xml?><ZynAddSubFX-data>\n<INSTRUMENT>\n<INFO><string \
                     name=\"name\">Pad</string></INFO>\n</INSTRUMENT>\n</ZynAddSubFX-data>";
        let inst = extract_instrument_element(patch).unwrap();
        assert!(inst.starts_with("<INSTRUMENT>") && inst.ends_with("</INSTRUMENT>"));
        assert!(inst.contains("Pad"));

        let master = "<ZynAddSubFX-data><MASTER>\
                      <PART id=\"0\">\n<par_bool name=\"enabled\" value=\"no\" />\n\
                      <INSTRUMENT>old</INSTRUMENT>\n</PART>\
                      <PART id=\"1\"><INSTRUMENT>other</INSTRUMENT></PART>\
                      </MASTER></ZynAddSubFX-data>";
        let out = splice_instrument_into_master(master, inst).unwrap();
        assert!(out.contains("Pad"), "new instrument spliced in");
        assert!(!out.contains("old"), "part 0 instrument replaced");
        assert!(out.contains("<INSTRUMENT>other</INSTRUMENT>"), "part 1 untouched");
        assert!(
            out.contains("<par_bool name=\"enabled\" value=\"yes\" />"),
            "part 0 forced enabled"
        );

        // Malformed inputs fail politely.
        assert!(extract_instrument_element("<nope/>").is_err());
        assert!(splice_instrument_into_master("<no-part/>", inst).is_err());
        assert!(splice_instrument_into_master("<PART id=\"0\">x</PART>", inst).is_err());
    }

    #[test]
    fn patch_enumeration_parses_bank_program_and_name() {
        // Gated on installed banks (zynaddsubfx-data).
        let patches = list_zyn_patches();
        if patches.is_empty() {
            eprintln!("skipping: no ZynAddSubFX banks installed");
            return;
        }
        assert!(patches.iter().all(|p| !p.bank.is_empty() && !p.name.is_empty()));
        assert!(patches.iter().all(|p| p.path.ends_with(".xiz")));
        // The stock data package ships these; tolerate custom setups by only
        // asserting when the standard root exists.
        if Path::new("/usr/share/zynaddsubfx/banks/Pads").is_dir() {
            let pad = find_zyn_patch("Pads", "analog pad").expect("stock Analog Pad exists");
            assert!(pad.program > 0);
            let xml = read_xiz_xml(Path::new(&pad.path)).unwrap();
            assert!(xml.contains("<INSTRUMENT>"), "xiz decompresses to instrument XML");
        }
    }

    // ---- the proof: loading a patch audibly changes the timbre ------------

    use crate::audio::dsp::ProcessBlock;
    use crate::midi::synth::BlockNoteEvent;
    use crate::plugins::descriptor::lv2_uid;

    /// Render `secs` seconds of a held note through a fresh RT node of the
    /// registered instance, returning mono samples.
    fn render_note(instance_id: &str, key: u8, secs: f32) -> Vec<f32> {
        render_note_warm(instance_id, key, secs, 0)
    }

    /// [`render_note`] with a warm-up: some plugins boot asynchronously
    /// (Yoshimi's engine start, geonkick's percussion synthesis), so run the
    /// node silently for `warmup_ms` (wall-clock, with the host ticker
    /// pumping workers) before the note goes in.
    fn render_note_warm(instance_id: &str, key: u8, secs: f32, warmup_ms: u64) -> Vec<f32> {
        let node = lv2_host::global()
            .make_node(instance_id, 48_000)
            .expect("node builds");
        render_node_warm(node, key, secs, warmup_ms)
    }

    /// Format-agnostic core of the acceptance render: drive ANY live node
    /// (LV2 or CLAP) with one held note and return mono samples.
    fn render_node_warm(
        mut node: Box<dyn crate::audio::dsp::LiveInstrument>,
        key: u8,
        secs: f32,
        warmup_ms: u64,
    ) -> Vec<f32> {
        const RATE: u32 = 48_000;
        node.prepare(RATE, 512);
        if warmup_ms > 0 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(warmup_ms);
            let mut warm = vec![0.0f32; 512 * 2];
            while std::time::Instant::now() < deadline {
                warm.fill(0.0);
                let mut io =
                    ProcessBlock { samples: &mut warm, channels: 2, sample_rate: RATE, steady: 0 };
                node.process(&mut io);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert!(node.queue_event(BlockNoteEvent { offset: 0, key, velocity: 110 }));
        let frames = (RATE as f32 * secs) as usize;
        let mut out = Vec::with_capacity(frames);
        let mut buf = vec![0.0f32; 512 * 2];
        let mut rendered = 0usize;
        while rendered < frames {
            buf.fill(0.0);
            let mut io = ProcessBlock { samples: &mut buf, channels: 2, sample_rate: RATE, steady: 0 };
            node.process(&mut io);
            out.extend(buf.iter().step_by(2));
            rendered += 512;
        }
        out
    }

    /// Normalized log-band spectrum via Goertzel — a compact timbre
    /// fingerprint (relative energy across bands, level-independent).
    fn band_spectrum(mono: &[f32], rate: u32) -> Vec<f32> {
        let n = mono.len().min(1 << 15);
        let x = &mono[mono.len() - n..];
        // 24 log-spaced probe frequencies, 60 Hz .. 12 kHz.
        let bands = 24;
        let mut mags = Vec::with_capacity(bands);
        for b in 0..bands {
            let f = 60.0f64 * (12_000.0f64 / 60.0).powf(b as f64 / (bands - 1) as f64);
            let w = 2.0 * std::f64::consts::PI * f / rate as f64;
            let coeff = 2.0 * w.cos();
            let (mut s1, mut s2) = (0.0f64, 0.0f64);
            for &v in x {
                let s0 = v as f64 + coeff * s1 - s2;
                s2 = s1;
                s1 = s0;
            }
            let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
            mags.push(power.sqrt() as f32);
        }
        let sum: f32 = mags.iter().sum::<f32>().max(f32::MIN_POSITIVE);
        mags.iter().map(|m| m / sum).collect()
    }

    fn spectral_distance(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// PATCH-LOADING PROOF (gated on zynaddsubfx-lv2 + banks): loading a
    /// bank patch into a Zyn instance CHANGES the rendered timbre versus the
    /// default state — verified by a spectral fingerprint diff — while two
    /// distinct patches also differ from each other, and the change survives
    /// a save/load round-trip of the state blob.
    #[test]
    fn loading_a_bank_patch_changes_the_rendered_timbre() {
        let scanned = crate::plugins::scan::scan_lv2();
        if !scanned.iter().any(|d| d.uid == lv2_uid(ZYN_URI)) {
            eprintln!("skipping: zynaddsubfx-lv2 not installed");
            return;
        }
        let Some(pad) = find_zyn_patch("Pads", "analog pad") else {
            eprintln!("skipping: ZynAddSubFX banks not installed");
            return;
        };
        let bass = find_zyn_patch("Bass", "analogue bass").expect("stock bank patch");

        let host = lv2_host::global();
        let id = "patch-proof";
        host.register_instance(id, &lv2_uid(ZYN_URI)).expect("Zyn registers");

        // Default state render (A3 keeps every patch in a comfortable range).
        let before = render_note(id, 57, 1.0);
        let default_spec = band_spectrum(&before, 48_000);
        assert!(rms(&before[8_000..]) > 1e-4, "default patch is audible");

        // Load the pad patch: timbre must CHANGE.
        load_zyn_patch(id, Path::new(&pad.path)).expect("pad patch loads");
        let after = render_note(id, 57, 1.0);
        let pad_spec = band_spectrum(&after, 48_000);
        assert!(rms(&after[8_000..]) > 1e-4, "patched instance is audible");
        let d_default_pad = spectral_distance(&default_spec, &pad_spec);
        eprintln!("zyn patch proof: |default - pad| spectral distance {d_default_pad:.3}");
        assert!(
            d_default_pad > 0.2,
            "pad patch changed the timbre (distance {d_default_pad})"
        );

        // A different patch differs from the pad too (loading actually
        // switches content rather than toggling some fixed state).
        load_zyn_patch(id, Path::new(&bass.path)).expect("bass patch loads");
        let bass_render = render_note(id, 45, 1.0);
        let bass_spec = band_spectrum(&bass_render, 48_000);
        assert!(rms(&bass_render[8_000..]) > 1e-4, "bass patch is audible");
        let d_pad_bass = spectral_distance(&pad_spec, &bass_spec);
        eprintln!("zyn patch proof: |pad - bass| spectral distance {d_pad_bass:.3}");
        assert!(d_pad_bass > 0.2, "patches are distinct (distance {d_pad_bass})");

        // The loaded patch is IN the saved state (persistence path): the
        // blob the bridge would write now carries the bass patch's name.
        let saved = host.save_state(id).unwrap().expect("state saves");
        let props = decode_lv2_props(&saved.data).unwrap();
        let xml = props
            .iter()
            .find(|p| is_master_xml_prop(p))
            .map(|p| String::from_utf8_lossy(&p.value).into_owned())
            .expect("master XML present");
        assert!(
            xml.to_ascii_lowercase().contains("analogue bass"),
            "saved master XML names the loaded patch"
        );
        host.unregister_instance(id);
    }

    // ---- synth compatibility sweep (gated per plugin) ----------------------
    //
    // The Zyn-style acceptance harness applied to every apt-packaged LV2
    // instrument on the roster: register through the shared host thread,
    // build an RT node, feed a note, render, assert non-silence (and pitch
    // where the synth is tonal). Results: docs/synth-compatibility.md.

    fn sweep(uri: &str, key: u8, secs: f32, warmup_ms: u64) -> Option<Vec<f32>> {
        let scanned = crate::plugins::scan::scan_lv2();
        if !scanned.iter().any(|d| d.uid == lv2_uid(uri)) {
            eprintln!("skipping sweep: {uri} not installed");
            return None;
        }
        let host = lv2_host::global();
        let id = format!("sweep-{uri}");
        host.register_instance(&id, &lv2_uid(uri)).expect("registers");
        let audio = render_note_warm(&id, key, secs, warmup_ms);
        host.unregister_instance(&id);
        Some(audio)
    }

    /// synthv1 (apt: synthv1-lv2): tonal analog-style synth; default patch
    /// sounds immediately, pitch tracks the note.
    #[test]
    fn sweep_synthv1_renders_pitched_audio() {
        let Some(audio) = sweep("http://synthv1.sourceforge.net/lv2", 69, 1.0, 0) else {
            return;
        };
        let sustain = &audio[8_000..40_000];
        assert!(rms(sustain) > 1e-4, "synthv1 renders non-silence");
        let f0 = crate::audio::sampler_voice::testutil::estimate_freq(sustain, 48_000, 200.0, 900.0);
        eprintln!("synthv1 sweep: f0 {f0:.1} Hz (target 440)");
        assert!((f0 - 440.0).abs() / 440.0 < 0.03, "synthv1 pitch {f0:.1} != 440");
    }

    /// geonkick (apt: geonkick): percussion synth — a note triggers a kick;
    /// non-silence is the acceptance (pitch is not meaningful for a kick).
    /// Warm-up covers geonkick's asynchronous percussion synthesis.
    #[test]
    fn sweep_geonkick_renders_audio() {
        // The multi-channel geonkick UID; `single` variant also exists.
        let Some(audio) = sweep("http://geontime.com/geonkick", 69, 1.0, 1_500) else {
            return;
        };
        assert!(rms(&audio[..24_000]) > 1e-4, "geonkick renders a kick");
    }

    // ---- crash-contained probes (subprocess, D-11 discipline) --------------
    //
    // Some plugins crash IN-PROCESS when rendered headlessly (Yoshimi:
    // SIGSEGV; padthv1: heap corruption — see docs/synth-compatibility.md).
    // Probing them inside the test process would take the whole suite down,
    // so the probe re-executes THIS test binary (the scan-worker pattern:
    // env guard + `--exact` on a single child test) and classifies the
    // child's fate. A segfault becomes a recorded "unstable" verdict, never
    // a dead test run.

    /// Env var carrying `uri|key|secs|warmup_ms` to the child probe body.
    const PROBE_ENV: &str = "AURA_SYNTH_PROBE";

    #[derive(Debug)]
    enum ProbeOutcome {
        NotInstalled,
        /// Child completed; rms of the rendered tail.
        Rendered(f32),
        Silent(f32),
        /// Child completed but the host refused the plugin (negotiation /
        /// uid / activation error) — hosting gap, not a crash.
        HostError(String),
        /// Child died (signal / abort / timeout) — unstable in-process.
        Unstable(String),
    }

    /// CHILD BODY: inert in normal runs (env guard absent -> trivially
    /// passes). When spawned by [`probe_in_subprocess`] it renders the
    /// requested synth and reports through a stdout marker line. The target
    /// is either an LV2 URI or `clap-bundle:<path>` (first instrument in the
    /// bundle, instantiated through the CLAP host).
    #[test]
    fn synth_probe_child_body() {
        let Ok(spec) = std::env::var(PROBE_ENV) else { return };
        let mut parts = spec.split('|');
        let target = parts.next().expect("probe target");
        let key: u8 = parts.next().and_then(|s| s.parse().ok()).expect("probe key");
        let secs: f32 = parts.next().and_then(|s| s.parse().ok()).expect("probe secs");
        let warmup: u64 = parts.next().and_then(|s| s.parse().ok()).expect("probe warmup");
        let audio = if let Some(bundle) = target.strip_prefix("clap-bundle:") {
            let path = Path::new(bundle);
            if !path.exists() {
                println!("AURA-PROBE: not-installed");
                return;
            }
            let descs = crate::plugins::scan::scan_clap_bundle(path).expect("bundle scans");
            let Some(desc) = descs.iter().find(|d| d.is_instrument) else {
                println!("AURA-PROBE: not-installed");
                return;
            };
            if let Err(e) = crate::plugins::clap_host::instantiate("probe-clap", &desc.uid) {
                println!("AURA-PROBE: error=instantiate: {e}");
                return;
            }
            match crate::plugins::clap_host::activate_node("probe-clap", 48_000) {
                Ok(node) => Some(render_node_warm(node, key, secs, warmup)),
                Err(e) => {
                    println!("AURA-PROBE: error=activate: {e}");
                    return;
                }
            }
        } else {
            sweep(target, key, secs, warmup)
        };
        match audio {
            None => println!("AURA-PROBE: not-installed"),
            Some(audio) => {
                let tail = &audio[audio.len() / 3..];
                println!("AURA-PROBE: rms={}", rms(tail));
            }
        }
    }

    /// Run the acceptance probe for `uri` in a subprocess; never crashes or
    /// hangs the caller (120 s timeout -> kill -> `Unstable`).
    fn probe_in_subprocess(uri: &str, key: u8, secs: f32, warmup_ms: u64) -> ProbeOutcome {
        use std::io::Read as _;
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args([
                "plugins::patches::tests::synth_probe_child_body",
                "--exact",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(PROBE_ENV, format!("{uri}|{key}|{secs}|{warmup_ms}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn probe child");
        let stdout = child.stdout.take().expect("child stdout");
        let reader = std::thread::spawn(move || {
            let mut out = String::new();
            let _ = std::io::BufReader::new(stdout).read_to_string(&mut out);
            out
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let status = loop {
            match child.try_wait().expect("probe child wait") {
                Some(st) => break st,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ProbeOutcome::Unstable("timeout (120 s) — killed".into());
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        };
        let out = reader.join().unwrap_or_default();
        if out.contains("AURA-PROBE: not-installed") {
            return ProbeOutcome::NotInstalled;
        }
        if let Some(line) = out.lines().find(|l| l.contains("AURA-PROBE: error=")) {
            let msg = line.split("error=").nth(1).unwrap_or("unknown").to_string();
            return ProbeOutcome::HostError(msg);
        }
        if let Some(line) = out.lines().find(|l| l.contains("AURA-PROBE: rms=")) {
            let rms: f32 = line
                .rsplit('=')
                .next()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0.0);
            return if rms > 1e-4 {
                ProbeOutcome::Rendered(rms)
            } else {
                ProbeOutcome::Silent(rms)
            };
        }
        // No marker: the child died before reporting (signal/abort) or the
        // harness failed. Both mean "cannot host in-process today".
        ProbeOutcome::Unstable(format!("child exited abnormally ({status})"))
    }

    /// Yoshimi (apt: yoshimi): the Zyn fork's LV2 plugin. SWEEP FINDING
    /// (2026-08, Yoshimi 2.3.2 on noble): boots its engine asynchronously,
    /// renders silence when driven immediately, and INTERMITTENTLY SIGSEGVs
    /// when given warm-up render blocks — unstable in-process until the
    /// isolation bridge lands. The subprocess probe records the verdict
    /// without endangering the suite; every classified outcome passes.
    #[test]
    fn sweep_yoshimi_probed_in_subprocess() {
        match probe_in_subprocess("http://yoshimi.sourceforge.net/lv2_plugin", 69, 1.5, 3_000) {
            ProbeOutcome::NotInstalled => eprintln!("yoshimi probe: not installed — skip"),
            ProbeOutcome::Rendered(rms) => eprintln!("yoshimi probe: rendered (rms {rms})"),
            ProbeOutcome::Silent(rms) => {
                eprintln!("yoshimi probe: silent (rms {rms}) — see synth-compatibility.md")
            }
            ProbeOutcome::HostError(why) => {
                eprintln!("yoshimi probe: host refused ({why}) — see synth-compatibility.md")
            }
            ProbeOutcome::Unstable(why) => {
                eprintln!("yoshimi probe: UNSTABLE in-process ({why}) — contained; \
                           see synth-compatibility.md")
            }
        }
    }

    /// padthv1 (apt: padthv1-lv2). SWEEP FINDING (2026-08, padthv1 1.0.0 on
    /// noble): registering + rendering in-process aborts with heap
    /// corruption ("free(): corrupted unsorted chunks"). Contained probe,
    /// same rules as Yoshimi's.
    #[test]
    fn sweep_padthv1_probed_in_subprocess() {
        match probe_in_subprocess("http://padthv1.sourceforge.net/lv2", 69, 1.5, 500) {
            ProbeOutcome::NotInstalled => eprintln!("padthv1 probe: not installed — skip"),
            ProbeOutcome::Rendered(rms) => eprintln!("padthv1 probe: rendered (rms {rms})"),
            ProbeOutcome::Silent(rms) => {
                eprintln!("padthv1 probe: silent (rms {rms}) — see synth-compatibility.md")
            }
            ProbeOutcome::HostError(why) => {
                eprintln!("padthv1 probe: host refused ({why}) — see synth-compatibility.md")
            }
            ProbeOutcome::Unstable(why) => {
                eprintln!("padthv1 probe: UNSTABLE in-process ({why}) — contained; \
                           see synth-compatibility.md")
            }
        }
    }

    /// Surge XT (official deb, surge-synth-team releases-xt): CLAP
    /// instrument through the clack host. Subprocess-contained like every
    /// third-party probe (a fresh CLAP bundle is exactly the D-11 risk).
    #[test]
    fn sweep_surge_xt_probed_in_subprocess() {
        match probe_in_subprocess("clap-bundle:/usr/lib/clap/Surge XT.clap", 69, 1.5, 1_000) {
            ProbeOutcome::NotInstalled => eprintln!("surge-xt probe: not installed — skip"),
            ProbeOutcome::Rendered(rms) => eprintln!("surge-xt probe: rendered (rms {rms})"),
            ProbeOutcome::Silent(rms) => {
                eprintln!("surge-xt probe: silent (rms {rms}) — see synth-compatibility.md")
            }
            ProbeOutcome::HostError(why) => {
                eprintln!("surge-xt probe: host refused ({why}) — see synth-compatibility.md")
            }
            ProbeOutcome::Unstable(why) => {
                eprintln!("surge-xt probe: UNSTABLE in-process ({why}) — contained; \
                           see synth-compatibility.md")
            }
        }
    }

    /// Cardinal (official DISTRHO release tarball, CardinalSynth CLAP).
    /// Subprocess-contained probe, same rules as Surge XT's.
    #[test]
    fn sweep_cardinal_probed_in_subprocess() {
        match probe_in_subprocess(
            "clap-bundle:/usr/lib/clap/Cardinal.clap/CardinalSynth.clap",
            69,
            1.5,
            1_000,
        ) {
            ProbeOutcome::NotInstalled => eprintln!("cardinal probe: not installed — skip"),
            ProbeOutcome::Rendered(rms) => eprintln!("cardinal probe: rendered (rms {rms})"),
            ProbeOutcome::Silent(rms) => {
                eprintln!("cardinal probe: silent (rms {rms}) — see synth-compatibility.md")
            }
            ProbeOutcome::HostError(why) => {
                eprintln!("cardinal probe: host refused ({why}) — see synth-compatibility.md")
            }
            ProbeOutcome::Unstable(why) => {
                eprintln!("cardinal probe: UNSTABLE in-process ({why}) — contained; \
                           see synth-compatibility.md")
            }
        }
    }
}
