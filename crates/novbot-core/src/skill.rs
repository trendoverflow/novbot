// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! In-process skill / MCP-style tool registry (OSS MVP).
//! DispatchCommand and scheduled SpecKind::Skill / SpecKind::McpTool invoke these.

use crate::probe::{ProbeError, ProbeOutcome};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Invoke a named skill/tool. `arguments` is free-form JSON object.
pub fn invoke_skill(name: &str, arguments: &Value) -> Result<ProbeOutcome, ProbeError> {
    match name {
        "host_info" => Ok(skill_host_info()),
        "echo" => Ok(skill_echo(arguments)),
        "env_get" => skill_env_get(arguments),
        other => Err(ProbeError::Params(format!(
            "unknown skill/tool: {other} (known: host_info, echo, env_get)"
        ))),
    }
}

fn skill_host_info() -> ProbeOutcome {
    let hostname = hostname_string();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let family = std::env::consts::FAMILY;
    ProbeOutcome {
        status: "ok",
        payload: json!({
            "skill": "host_info",
            "hostname": hostname,
            "os": os,
            "arch": arch,
            "family": family,
        }),
    }
}

fn skill_echo(arguments: &Value) -> ProbeOutcome {
    ProbeOutcome {
        status: "ok",
        payload: json!({
            "skill": "echo",
            "arguments": arguments.clone(),
        }),
    }
}

fn skill_env_get(arguments: &Value) -> Result<ProbeOutcome, ProbeError> {
    let key = arguments
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProbeError::Params("env_get requires arguments.key".into()))?;
    // Deny secrets-looking keys in OSS stub.
    let lower = key.to_ascii_lowercase();
    if lower.contains("secret")
        || lower.contains("password")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.ends_with("_key")
    {
        return Ok(ProbeOutcome {
            status: "fail",
            payload: json!({
                "skill": "env_get",
                "key": key,
                "error": "refused: key looks sensitive",
            }),
        });
    }
    let value = std::env::var(key).ok();
    Ok(ProbeOutcome {
        status: if value.is_some() { "ok" } else { "fail" },
        payload: json!({
            "skill": "env_get",
            "key": key,
            "present": value.is_some(),
            "value": value,
        }),
    })
}

fn hostname_string() -> String {
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into())
}

/// List built-in skill/tool names (for docs / introspection).
pub fn list_skills() -> Vec<&'static str> {
    vec!["host_info", "echo", "env_get"]
}

/// Resolve skill name + arguments from a Spec params object.
pub fn skill_from_params(kind_is_mcp: bool, params: &Value) -> Result<(String, Value), ProbeError> {
    if kind_is_mcp {
        let tool = params
            .get("tool")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProbeError::Params("mcp_tool requires params.tool".into()))?;
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        Ok((tool.to_string(), arguments))
    } else {
        let skill = params
            .get("skill")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProbeError::Params("skill requires params.skill".into()))?;
        let mut arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        // Allow top-level params except skill/arguments to flow into arguments.
        if let Some(obj) = params.as_object() {
            if arguments.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                let mut map = HashMap::new();
                for (k, v) in obj {
                    if k != "skill" && k != "arguments" {
                        map.insert(k.clone(), v.clone());
                    }
                }
                if !map.is_empty() {
                    arguments = Value::Object(map.into_iter().collect());
                }
            }
        }
        Ok((skill.to_string(), arguments))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_info_ok() {
        let out = invoke_skill("host_info", &json!({})).unwrap();
        assert_eq!(out.status, "ok");
        assert!(out.payload.get("os").is_some());
    }

    #[test]
    fn echo_ok() {
        let out = invoke_skill("echo", &json!({"text": "hi"})).unwrap();
        assert_eq!(out.status, "ok");
    }

    #[test]
    fn unknown_skill_err() {
        assert!(invoke_skill("nope", &json!({})).is_err());
    }
}
