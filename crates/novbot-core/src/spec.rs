// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Config-driven probe definition (center is authority; node holds in memory).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Spec {
    pub id: String,
    pub kind: SpecKind,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub threshold: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpecKind {
    Cpu,
    Memory,
    Disk,
    /// Thin compliance: assert a path exists.
    CompliancePath,
    /// sshd_config checks (PermitRootLogin, PasswordAuthentication).
    ComplianceSshd,
    /// Sample listening TCP ports from /proc/net/tcp{,6}.
    ComplianceListeningPorts,
    /// World-writable paths under a root (default /etc), bounded walk.
    ComplianceWorldWritable,
    /// NTP / time sync status when tools exist.
    ComplianceNtp,
    /// Reboot-required marker (e.g. /var/run/reboot-required).
    ComplianceRebootRequired,
    /// Escape hatch: bounded shell command.
    Exec,
    /// Named in-process skill (DispatchCommand / schedule).
    Skill,
    /// MCP-style tool invocation (same registry as Skill; args under params.arguments).
    McpTool,
}

pub fn parse_specs_json(raw: &str) -> Result<Vec<Spec>, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_specs() {
        let raw = r#"[{"id":"cpu","kind":"cpu"},{"id":"disk-root","kind":"disk","params":{"mount":"/"}},{"id":"skill1","kind":"skill","params":{"skill":"host_info"}},{"id":"mcp1","kind":"mcp_tool","params":{"tool":"echo","arguments":{"text":"hi"}}}]"#;
        let specs = parse_specs_json(raw).unwrap();
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[0].kind, SpecKind::Cpu);
        assert_eq!(specs[2].kind, SpecKind::Skill);
        assert_eq!(specs[3].kind, SpecKind::McpTool);
    }
}
