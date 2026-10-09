// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `cap-violation-test` 1.0.0.
//!
//! Default mode reads `/etc/shadow`, which is outside the `/etc/os-release` grant.
//! `mode=undeclared` calls `listening-ports` without `net.listening_ports.read`.
//! The guest reports the host error. It does not invent `denials[]`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(params: serde_json::Value) -> Result<serde_json::Value, String> {
    let mode = params
        .get("mode")
        .and_then(|value| value.as_str())
        .unwrap_or("shadow");
    let error = if mode == "undeclared" {
        match novbot_skill_sdk::net::listening_ports() {
            Ok(sockets) => format!("ok {}", sockets.len()),
            Err(err) => err.to_string(),
        }
    } else {
        match novbot_skill_sdk::fs::read("/etc/shadow", 32) {
            Ok(bytes) => format!("ok {}", bytes.len()),
            Err(err) => err.to_string(),
        }
    };
    Ok(serde_json::json!({
        "mode": mode,
        "error": error,
    }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn default_mode_reads_shadow() {
        let (value, host) = with_host(MockHost::new(), || check(serde_json::json!({})).unwrap());
        assert_eq!(value["mode"], "shadow");
        assert!(host.calls.iter().any(|call| call == "fs.read /etc/shadow"));
        assert!(value["error"].as_str().unwrap().contains("not_found"));
    }

    #[test]
    fn undeclared_mode_calls_listening_ports() {
        let (value, host) = with_host(MockHost::new(), || {
            check(serde_json::json!({"mode": "undeclared"})).unwrap()
        });
        assert_eq!(value["mode"], "undeclared");
        assert!(host.calls.iter().any(|call| call == "net.listening_ports"));
        assert_eq!(value["error"], "ok 0");
    }
}
