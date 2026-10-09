// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `cpu-usage` 1.0.0. Returns CPU usage and load from `sys.metrics`.
//!
//! The skill id is distinct from the built-in Spec id `cpu`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let raw = novbot_skill_sdk::sys::metrics().map_err(|err| err.to_string())?;
    let metrics: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("sys.metrics: {err}"))?;
    let cpu = &metrics["cpu"];
    Ok(serde_json::json!({
        "cpu_usage_percent": cpu["usage_percent"],
        "load_avg_1m": cpu["load_avg_1m"],
        "load_avg_5m": cpu["load_avg_5m"],
        "load_avg_15m": cpu["load_avg_15m"],
        "memory": metrics["memory"],
        "disks": metrics["disks"],
    }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn returns_cpu_load_and_forwards_memory_and_disks() {
        let mut host = MockHost::new();
        host.metrics = serde_json::json!({
            "cpu": {
                "usage_percent": 12.5,
                "load_avg_1m": 0.5,
                "load_avg_5m": 0.25,
                "load_avg_15m": 0.1
            },
            "memory": {"total_bytes": 100, "used_bytes": 40, "available_bytes": 60},
            "disks": [{"mount": "/", "total_bytes": 1000, "used_bytes": 100, "available_bytes": 900}],
            "disk_io": []
        })
        .to_string();
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["cpu_usage_percent"], 12.5);
        assert_eq!(value["load_avg_1m"], 0.5);
        assert_eq!(value["load_avg_5m"], 0.25);
        assert_eq!(value["load_avg_15m"], 0.1);
        assert_eq!(value["memory"]["total_bytes"], 100);
        assert_eq!(value["disks"][0]["mount"], "/");
        assert!(host.calls.iter().any(|call| call == "sys.metrics"));
    }
}
