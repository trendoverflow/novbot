// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `net-interface-traffic` 1.0.0. Returns counters from `net.interfaces`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let raw = novbot_skill_sdk::net::interfaces().map_err(|err| err.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|err| format!("net.interfaces: {err}"))?;
    Ok(serde_json::json!({
        "interfaces": parsed["interfaces"].clone(),
    }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn returns_interface_counters() {
        let mut host = MockHost::new();
        host.interfaces = serde_json::json!({
            "interfaces": [{
                "name": "eth0",
                "addresses": ["192.0.2.10"],
                "operstate": "up",
                "up": true,
                "rx_bytes": 1000,
                "rx_packets": 10,
                "rx_errors": 1,
                "tx_bytes": 2000,
                "tx_packets": 20,
                "tx_errors": 2
            }]
        })
        .to_string();
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["interfaces"][0]["name"], "eth0");
        assert_eq!(value["interfaces"][0]["addresses"][0], "192.0.2.10");
        assert_eq!(value["interfaces"][0]["up"], true);
        assert_eq!(value["interfaces"][0]["rx_bytes"], 1000);
        assert_eq!(value["interfaces"][0]["tx_bytes"], 2000);
        assert_eq!(value["interfaces"][0]["rx_errors"], 1);
        assert_eq!(value["interfaces"][0]["tx_errors"], 2);
        assert!(value["interfaces"][0].get("connections").is_none());
        assert!(host.calls.iter().any(|call| call == "net.interfaces"));
    }
}
