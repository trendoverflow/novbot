// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Small component that drives the skill host. Params JSON selects the call.

wit_bindgen::generate!({
    world: "skill",
    path: "../wit",
});

use crate::novbot::skill::fs;
use crate::novbot::skill::net;
use crate::novbot::skill::sys;
use crate::novbot::skill::types::HostError;

struct SkillGuest;

impl Guest for SkillGuest {
    fn run(params_json: String) -> Result<String, String> {
        let params: serde_json::Value =
            serde_json::from_str(&params_json).map_err(|err| err.to_string())?;
        let op = params
            .get("op")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match op {
            "read" => {
                let path = field(&params, "path")?;
                let max_bytes = params
                    .get("max_bytes")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(4096) as u32;
                let bytes = fs::read(&path, max_bytes).map_err(host_err)?;
                Ok(String::from_utf8_lossy(&bytes).into_owned())
            }
            "stat" => {
                let path = field(&params, "path")?;
                let stat = fs::stat(&path).map_err(host_err)?;
                Ok(format!(
                    "exists={} kind={} size={}",
                    stat.exists, stat.kind, stat.size
                ))
            }
            "list" => {
                let dir = field(&params, "dir")?;
                let max_depth = params
                    .get("max_depth")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(1) as u8;
                let max_entries = params
                    .get("max_entries")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(100) as u32;
                let entries = fs::list(&dir, max_depth, max_entries).map_err(host_err)?;
                Ok(entries.join("\n"))
            }
            "ports" => {
                let sockets = net::listening_ports().map_err(host_err)?;
                Ok(format!("ports:{}", sockets.len()))
            }
            "info" => sys::info().map_err(host_err),
            "time_sync" => sys::time_sync().map_err(host_err),
            "env" => {
                let key = field(&params, "key")?;
                match sys::env_get(&key).map_err(host_err)? {
                    Some(value) => Ok(value),
                    None => Ok(String::new()),
                }
            }
            "log" => {
                let message = field(&params, "message")?;
                let level = params
                    .get("level")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(1) as u8;
                crate::novbot::skill::log::log(level, &message);
                Ok("logged".to_string())
            }
            "wasi_fs" => {
                let path = field(&params, "path")?;
                match std::fs::read(path) {
                    Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
                    Err(err) => Err(format!("wasi_fs: {err}")),
                }
            }
            "deny_loop" => {
                let path = field(&params, "path")?;
                for _ in 0..20 {
                    let _ = fs::read(&path, 32);
                }
                Ok("finished".to_string())
            }
            "spin" => {
                // Side effect keeps the loop in the wasm so epoch interruption can fire.
                let mut n: u64 = 1;
                loop {
                    n = n.wrapping_mul(3).wrapping_add(1);
                    if n == 0 {
                        break;
                    }
                }
                Ok(n.to_string())
            }
            other => Err(format!("unknown op: {other}")),
        }
    }
}

fn field(params: &serde_json::Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("missing {key}"))
}

fn host_err(err: HostError) -> String {
    match err {
        HostError::Denied(denial) => {
            format!("denied:{}:{}", denial.capability, denial.reason)
        }
        HostError::NotFound(message) => format!("not_found:{message}"),
        HostError::Io(message) => format!("io:{message}"),
        HostError::LimitExceeded(message) => format!("limit:{message}"),
        HostError::Invalid(message) => format!("invalid:{message}"),
    }
}

export!(SkillGuest);
