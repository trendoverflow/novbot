// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `disk-usage` 1.0.0. Returns per-mount usage from `sys.metrics` `disks`.
//!
//! The skill id is distinct from the built-in Spec id `disk-root`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let raw = novbot_skill_sdk::sys::metrics().map_err(|err| err.to_string())?;
    let metrics: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("sys.metrics: {err}"))?;
    let disks = metrics
        .get("disks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    Ok(serde_json::json!({ "disks": disks }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn returns_mounts_including_root() {
        let mut host = MockHost::new();
        host.metrics = serde_json::json!({
            "cpu": {"usage_percent": 1.0},
            "memory": {"total_bytes": 1},
            "disks": [
                {
                    "mount": "/",
                    "fstype": "ext4",
                    "total_bytes": 1000,
                    "used_bytes": 250,
                    "available_bytes": 750,
                    "inodes_total": 100,
                    "inodes_used": 10,
                    "inodes_available": 90
                },
                {
                    "mount": "/var",
                    "fstype": "ext4",
                    "total_bytes": 500,
                    "used_bytes": 100,
                    "available_bytes": 400
                }
            ],
            "disk_io": []
        })
        .to_string();
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        let disks = value["disks"].as_array().expect("disks");
        assert_eq!(disks.len(), 2);
        assert!(disks.iter().any(|disk| disk["mount"] == "/"), "{value}");
        assert_eq!(disks[0]["total_bytes"], 1000);
        assert_eq!(disks[1]["mount"], "/var");
        assert!(value.get("cpu_usage_percent").is_none());
        assert!(host.calls.iter().any(|call| call == "sys.metrics"));
    }
}
