//! `plugins::lv2_host` when the `lv2` feature is OFF (Windows builds).
//!
//! Same public surface as the real [`super::lv2_host`], with every entry
//! point degrading to "this build hosts no LV2". That is deliberate: the LV2
//! call sites are spread across `plugins::mod`, `plugins::state`,
//! `plugins::patches` and `control::mod`, and gating each one would put a
//! `#[cfg]` on a dozen match arms that all mean the same thing. One stub
//! module keeps the cfg in exactly two places — `plugins/mod.rs`'s `mod`
//! declaration and `scan::scan_lv2`.
//!
//! Consequences on a stub build: `scan_lv2` finds nothing, so no `"lv2"`
//! descriptor ever reaches the UI and none of the register/instantiate paths
//! are hit in practice. The error strings below only surface if a project
//! saved on Linux (whose `plugins[]` rows carry `format: "lv2"`) is opened
//! here — and that path is already designed to keep the row, its params and
//! its state blob while rendering silence (see `state`'s data-safety rules).

use crate::audio::dsp::LiveInstrument;

use super::descriptor::ParamInfo;
use super::state::StateBlob;

/// Mirrors the real host's `(control-port index, plugin-unit value)` pair.
type ParamChange = (u32, f32);

const UNAVAILABLE: &str = "LV2 hosting is not compiled into this build (CLAP only)";

pub struct Lv2Host;

/// Always the same unit handle — there is no world to boot.
pub fn global() -> &'static Lv2Host {
    &Lv2Host
}

/// Always `None`: callers that use this specifically to avoid booting a lilv
/// world get the "nothing to do" answer, which is the truth here.
pub fn try_global() -> Option<&'static Lv2Host> {
    None
}

impl Lv2Host {
    pub fn register_instance(
        &self,
        _instance_id: &str,
        _uid: &str,
    ) -> Result<Vec<ParamInfo>, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn register_instance_effect(
        &self,
        _instance_id: &str,
        _uid: &str,
    ) -> Result<Vec<ParamInfo>, String> {
        Err(UNAVAILABLE.into())
    }

    pub fn latency_samples(&self, _instance_id: &str) -> usize {
        0
    }

    pub fn unregister_instance(&self, _instance_id: &str) {}

    pub fn set_params(&self, _instance_id: &str, _changes: Vec<ParamChange>) {}

    pub fn make_node(
        &self,
        _instance_id: &str,
        _rate: u32,
    ) -> Result<Box<dyn LiveInstrument>, String> {
        Err(UNAVAILABLE.into())
    }

    /// `Ok(false)`, not an error: "is this id registered here" has a correct
    /// answer on a stub build, and callers branch on it (`control`'s
    /// `HostForward::Instantiate`) rather than treating it as a failure.
    pub fn has_instance(&self, _instance_id: &str) -> Result<bool, String> {
        Ok(false)
    }

    pub fn get_params(&self, _instance_id: &str) -> Result<Vec<ParamInfo>, String> {
        Err(UNAVAILABLE.into())
    }

    /// `Ok(None)` = "no state interface here", which routes save through the
    /// param-snapshot fallback instead of failing the save.
    pub fn save_state(&self, _instance_id: &str) -> Result<Option<StateBlob>, String> {
        Ok(None)
    }

    /// An error, not a silent success: a `KIND_LV2_PROPS` blob that cannot be
    /// delivered must be logged by the caller, and the blob is preserved on
    /// disk either way (`state`'s corrupt/undeliverable-blob rule).
    pub fn load_state(&self, _instance_id: &str, _blob: StateBlob) -> Result<(), String> {
        Err(UNAVAILABLE.into())
    }
}
