// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Run one configured Spec.
//!
//! Built-in `host_info`, `echo`, and `env_get` stay in-process and do not take
//! a Hub run slot. Any other `skill` or `mcp_tool` name is an installed
//! component on [`SkillHost`].

use crate::skills::{HubRun, SkillHost};
use novbot_core::{list_skills, run_probe, skill_from_params, ProbePolicy, Spec, SpecKind};
use serde_json::{json, Value};
use std::path::Path;

/// Merge a dispatch `params_json` object into `spec.params`.
///
/// Invalid JSON is ignored, matching the previous dispatch overlay.
pub fn apply_dispatch_overlay(spec: &mut Spec, params_json: &str) {
    if params_json.trim().is_empty() {
        return;
    }
    let Ok(overlay) = serde_json::from_str::<Value>(params_json) else {
        return;
    };
    let Some(obj) = overlay.as_object() else {
        return;
    };
    if !spec.params.is_object() {
        spec.params = json!({});
    }
    let Some(base) = spec.params.as_object_mut() else {
        return;
    };
    for (key, value) in obj {
        base.insert(key.clone(), value.clone());
    }
}

/// Execute `spec` on this process.
///
/// `run_id` is logged with a capability denial and is not passed to the guest.
pub async fn execute_spec(
    spec: &Spec,
    policy: &ProbePolicy,
    skills: &SkillHost,
    data_dir: &Path,
    run_id: &str,
) -> (String, Value) {
    let outcome = match spec.kind {
        SpecKind::Skill | SpecKind::McpTool => {
            let mcp = spec.kind == SpecKind::McpTool;
            match skill_from_params(mcp, &spec.params) {
                Ok((name, _)) if !is_builtin(&name) => {
                    skills
                        .run_hub(HubRun {
                            name,
                            version: spec.params.get("version").cloned(),
                            arguments: hub_arguments(&spec.params),
                            data_dir: data_dir.to_path_buf(),
                        })
                        .await
                }
                _ => probe_outcome(spec, policy).await,
            }
        }
        _ => probe_outcome(spec, policy).await,
    };
    log_denials(run_id, &outcome.1);
    outcome
}

fn is_builtin(name: &str) -> bool {
    list_skills().contains(&name)
}

async fn probe_outcome(spec: &Spec, policy: &ProbePolicy) -> (String, Value) {
    match run_probe(spec, policy).await {
        Ok(out) => (out.status.to_string(), out.payload),
        Err(err) => ("error".to_string(), json!({ "error": err.to_string() })),
    }
}

/// Skill arguments are `params.arguments` when that key is present.
///
/// Otherwise top-level params except the skill name and the version guard are
/// the arguments. `params.version` is not an argument.
fn hub_arguments(params: &Value) -> Value {
    if let Some(arguments) = params.get("arguments") {
        return arguments.clone();
    }
    let Some(obj) = params.as_object() else {
        return json!({});
    };
    let mut arguments = serde_json::Map::new();
    for (key, value) in obj {
        if matches!(key.as_str(), "skill" | "tool" | "version" | "arguments") {
            continue;
        }
        arguments.insert(key.clone(), value.clone());
    }
    Value::Object(arguments)
}

fn log_denials(run_id: &str, payload: &Value) {
    let Some(denials) = payload.get("denials").and_then(Value::as_array) else {
        return;
    };
    let Some(first) = denials.first() else {
        return;
    };
    let skill = payload
        .get("skill")
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    // `tracing`'s field parser treats `Value` as its own trait, so the
    // serde_json method has to be resolved before the macro.
    let capability = first
        .get("capability")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = first.get("target").and_then(Value::as_str).unwrap_or("");
    let reason = first.get("reason").and_then(Value::as_str).unwrap_or("");
    let denials = denials.len();
    tracing::warn!(
        run_id,
        skill,
        capability,
        target,
        reason,
        denials,
        "skill capability denied"
    );
}
