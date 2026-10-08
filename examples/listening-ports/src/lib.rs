// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `listening-ports` 1.0.0. Returns sockets from `net.listening-ports`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let sockets = novbot_skill_sdk::net::listening_ports().map_err(|err| err.to_string())?;
    Ok(serde_json::json!({ "sockets": sockets }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};
    use novbot_skill_sdk::ListenSocket;

    #[test]
    fn returns_sockets() {
        let mut host = MockHost::new();
        host.ports.push(ListenSocket {
            proto: "tcp".into(),
            addr: "127.0.0.1".into(),
            port: 22,
            uid: 0,
        });
        let (value, host) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["sockets"][0]["proto"], "tcp");
        assert_eq!(value["sockets"][0]["addr"], "127.0.0.1");
        assert_eq!(value["sockets"][0]["port"], 22);
        assert_eq!(value["sockets"][0]["uid"], 0);
        assert!(host.calls.iter().any(|call| call == "net.listening_ports"));
    }
}
