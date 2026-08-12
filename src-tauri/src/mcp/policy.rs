//! MCP policy layer (tauri-free, testable): what the connected agent may do.
//! Wire mirror: docs/ipc-schemas/mcp-policy.schema.json.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::tools;

/// Default port for the embedded MCP server (loopback only).
pub const DEFAULT_PORT: u16 = 41717;

/// Global policy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PolicyMode {
    /// Only read-only tools are callable.
    ReadOnly,
    /// Read-only tools run freely; destructive tools require a per-call
    /// confirm in the AURA UI (mcp_confirm_pending).
    ConfirmDestructive,
    /// Everything runs without confirmation (explicit user opt-in).
    Full,
}

/// Per-tool override of the global mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolRule {
    Allow,
    Confirm,
    Deny,
}

/// What the policy engine answers for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Queue a confirmation request in the UI; run only on approval.
    Confirm,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpPolicy {
    pub mode: PolicyMode,
    /// Tool-name -> rule, overriding `mode` (e.g. always deny record_take).
    #[serde(default)]
    pub tool_overrides: HashMap<String, ToolRule>,
    /// TCP port on 127.0.0.1. The bind address is NOT configurable.
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for McpPolicy {
    fn default() -> Self {
        Self {
            mode: PolicyMode::ConfirmDestructive,
            tool_overrides: HashMap::new(),
            port: DEFAULT_PORT,
        }
    }
}

impl McpPolicy {
    /// Decide a tool call. Unknown tool names are always denied.
    pub fn decide(&self, tool: &str) -> Decision {
        if !tools::ALL_TOOLS.contains(&tool) {
            return Decision::Deny;
        }
        if let Some(rule) = self.tool_overrides.get(tool) {
            return match rule {
                ToolRule::Allow => Decision::Allow,
                ToolRule::Confirm => Decision::Confirm,
                ToolRule::Deny => Decision::Deny,
            };
        }
        let destructive = !tools::READ_ONLY_TOOLS.contains(&tool);
        match (self.mode, destructive) {
            (_, false) => Decision::Allow,
            (PolicyMode::ReadOnly, true) => Decision::Deny,
            (PolicyMode::ConfirmDestructive, true) => Decision::Confirm,
            (PolicyMode::Full, true) => Decision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_confirms_destructive_allows_reads() {
        let p = McpPolicy::default();
        assert_eq!(p.decide(tools::GET_PROJECT_STATE), Decision::Allow);
        assert_eq!(p.decide(tools::READ_METERS), Decision::Allow);
        assert_eq!(p.decide(tools::ADD_TRACK), Decision::Confirm);
        assert_eq!(p.decide(tools::RECORD_TAKE), Decision::Confirm);
        assert_eq!(p.decide(tools::GET_JOB_STATUS), Decision::Allow);
    }

    #[test]
    fn read_only_mode_denies_all_mutations() {
        let p = McpPolicy { mode: PolicyMode::ReadOnly, ..Default::default() };
        assert_eq!(p.decide(tools::GET_PROJECT_STATE), Decision::Allow);
        for t in tools::ALL_TOOLS {
            if !tools::READ_ONLY_TOOLS.contains(t) {
                assert_eq!(p.decide(t), Decision::Deny, "tool {t}");
            }
        }
    }

    #[test]
    fn overrides_beat_mode_and_unknown_tools_are_denied() {
        let mut p = McpPolicy { mode: PolicyMode::Full, ..Default::default() };
        p.tool_overrides.insert(tools::RECORD_TAKE.into(), ToolRule::Deny);
        assert_eq!(p.decide(tools::RECORD_TAKE), Decision::Deny);
        assert_eq!(p.decide(tools::SET_TRACK_MIX), Decision::Allow);
        assert_eq!(p.decide("format_disk"), Decision::Deny);
    }
}
