// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `memory-usage` 1.0.0. Returns memory and swap from `sys.metrics`.
//!
//! The skill id is distinct from the built-in Spec id `memory`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let raw = novbot_skill_sdk::sys::metrics().map_err(|err| err.to_string())?;
    let metrics: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("sys.metrics: {err}"))?;
    let memory = metrics
        .get("memory")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut out = serde_json::json!({
        "total_bytes": memory["total_bytes"],
        "used_bytes": memory["used_bytes"],
        "available_bytes": memory["available_bytes"],
    });
    for key in ["swap_total_bytes", "swap_used_bytes"] {
        if let Some(value) = memory.get(key) {
            if !value.is_null() {
                out[key] = value.clone();
            }
        }
    }
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn returns_memory_totals_and_swap_when_present() {
        let mut host = MockHost::new();
        host.metrics = serde_json::json!({
            "cpu": {"usage_percent": 1.0},
            "memory": {
                "total_bytes": 1000,
                "used_bytes": 400,
                "available_bytes": 600,
                "swap_total_bytes": 200,
                "swap_used_bytes": 50
            },
            "disks": [],
            "disk_io": []
        })
        .to_string();
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["total_bytes"], 1000);
        assert_eq!(value["used_bytes"], 400);
        assert_eq!(value["available_bytes"], 600);
        assert_eq!(value["swap_total_bytes"], 200);
        assert_eq!(value["swap_used_bytes"], 50);
        assert!(value.get("cpu_usage_percent").is_none());
        assert!(host.calls.iter().any(|call| call == "sys.metrics"));
    }

    #[test]
    fn omits_swap_when_the_host_has_none() {
        let mut host = MockHost::new();
        host.metrics = serde_json::json!({
            "memory": {
                "total_bytes": 10,
                "used_bytes": 4,
                "available_bytes": 6
            }
        })
        .to_string();
        let (value, _) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["total_bytes"], 10);
        assert!(value.get("swap_total_bytes").is_none());
        assert!(value.get("swap_used_bytes").is_none());
    }
}
