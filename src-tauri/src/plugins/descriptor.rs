//! Plugin descriptor / instance / parameter wire types (phase 3).
//!
//! JSON mirrors: `docs/ipc-schemas/plugin-descriptor.schema.json` and
//! `plugin-state.schema.json` (camelCase on the wire; emitter contracts, no
//! `additionalProperties:false` — D-06 discipline). Pure data, tauri-free.

use serde::{Deserialize, Serialize};

/// Stable plugin identity across scans and project saves.
///
/// * CLAP: `clap:<bundle-path>#<clap-plugin-id>` (the clap id is globally
///   unique per spec; the path disambiguates duplicate installs).
/// * LV2: `lv2:<plugin-uri>` (LV2 URIs are the canonical identity).
pub fn clap_uid(bundle_path: &str, clap_id: &str) -> String {
    format!("clap:{bundle_path}#{clap_id}")
}

pub fn lv2_uid(uri: &str) -> String {
    format!("lv2:{uri}")
}

/// Mirrors plugin-descriptor.schema.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDescriptor {
    /// Stable uid (`clap:...#...` / `lv2:...`).
    pub uid: String,
    /// "clap" | "lv2"
    pub format: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Bundle path (CLAP) or bundle dir (LV2) — informational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// True when the plugin is an instrument (note input -> audio out);
    /// false for effects. AURA v1 hosts instruments on midi tracks.
    pub is_instrument: bool,
    pub audio_inputs: u32,
    pub audio_outputs: u32,
    /// Accepts note/MIDI events (CLAP note ports / LV2 atom midi input).
    pub has_note_input: bool,
    /// Format-reported categories/classes ("Instrument Plugin", "synthesizer", ...).
    #[serde(default)]
    pub categories: Vec<String>,
}

/// One plugin parameter (generic-UI + automation contract).
/// Mirrors plugin-state.schema.json#/$defs/param.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParamInfo {
    /// Stable per-plugin parameter id (CLAP param id / LV2 control-port index).
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub value: f64,
    /// Discrete step count (0 = continuous).
    #[serde(default)]
    pub steps: u32,
}

/// A live plugin instance registered in the host.
/// Mirrors plugin-state.schema.json#/$defs/instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstanceInfo {
    /// Instance uuid; a track binds it via `instrumentId = "plugin:<id>"`.
    pub id: String,
    pub uid: String,
    pub name: String,
    pub format: String,
    /// "stub" (registered, DSP not hosted yet — renders silence) |
    /// "active" (zones P1/P2: audible) | "crashed" (isolation evolution).
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uids_are_stable_and_prefixed() {
        assert_eq!(
            clap_uid("/usr/lib/clap/ZamComp.clap", "com.zamaudio.ZamComp"),
            "clap:/usr/lib/clap/ZamComp.clap#com.zamaudio.ZamComp"
        );
        assert_eq!(
            lv2_uid("http://zynaddsubfx.sourceforge.net"),
            "lv2:http://zynaddsubfx.sourceforge.net"
        );
    }

    #[test]
    fn wire_format_is_camel_case_and_reader_tolerant() {
        let d = PluginDescriptor {
            uid: "lv2:urn:x".into(),
            format: "lv2".into(),
            name: "X".into(),
            vendor: None,
            version: None,
            path: None,
            is_instrument: true,
            audio_inputs: 0,
            audio_outputs: 2,
            has_note_input: true,
            categories: vec!["Instrument Plugin".into()],
        };
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("isInstrument").is_some());
        assert!(v.get("hasNoteInput").is_some());
        // Reader ignores unknown fields (D-06).
        let with_extra = serde_json::json!({
            "uid": "lv2:urn:y", "format": "lv2", "name": "Y",
            "isInstrument": false, "audioInputs": 2, "audioOutputs": 2,
            "hasNoteInput": false, "futureField": 42
        });
        let parsed: PluginDescriptor = serde_json::from_value(with_extra).unwrap();
        assert_eq!(parsed.name, "Y");
    }
}
