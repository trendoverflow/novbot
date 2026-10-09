// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `os-release-check` 1.0.0. Returns `ID`, `VERSION_ID`, and `hostname`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(_params: serde_json::Value) -> Result<serde_json::Value, String> {
    let bytes =
        novbot_skill_sdk::fs::read("/etc/os-release", 64 * 1024).map_err(|err| err.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let info = novbot_skill_sdk::sys::info().map_err(|err| err.to_string())?;
    let info: serde_json::Value =
        serde_json::from_str(&info).map_err(|err| format!("sys.info: {err}"))?;
    let hostname = info
        .get("hostname")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    Ok(serde_json::json!({
        "ID": os_release_value(&text, "ID"),
        "VERSION_ID": os_release_value(&text, "VERSION_ID"),
        "hostname": hostname,
    }))
}

fn os_release_value(text: &str, key: &str) -> String {
    let mut found = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            found = unquote(value.trim());
        }
    }
    found
}

fn unquote(value: &str) -> String {
    let first = value.chars().next();
    let last = value.chars().next_back();
    if value.len() >= 2
        && matches!(
            (first, last),
            (Some('"'), Some('"')) | (Some('\''), Some('\''))
        )
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn reports_id_version_and_hostname_without_pretty_name() {
        let mut host = MockHost::new();
        host.insert_file(
            "/etc/os-release",
            b"ID=demo\nVERSION_ID=\"1.0\"\nPRETTY_NAME=\"Demo OS\"\n",
        );
        host.info = r#"{"hostname":"box"}"#.to_string();
        let (value, _) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["ID"], "demo");
        assert_eq!(value["VERSION_ID"], "1.0");
        assert_eq!(value["hostname"], "box");
        assert!(value.get("PRETTY_NAME").is_none());
    }
}
