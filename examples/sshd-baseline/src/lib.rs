// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `sshd-baseline` 1.0.0. Last matching directive wins. Comment lines are skipped.
//!
//! The parser is local to this skill. It does not change the built-in probes.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

const CONFIG: &str = "/etc/ssh/sshd_config";

fn check(params: serde_json::Value) -> Result<serde_json::Value, String> {
    let expect_root = str_param(&params, "permit_root_login", "no");
    let expect_password = str_param(&params, "password_authentication", "no");
    let text = match novbot_skill_sdk::fs::read(CONFIG, 256 * 1024) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            let finding = novbot_skill_sdk::Finding::new(
                "ssh.config",
                novbot_skill_sdk::FindingStatus::Error,
                "high",
            )
            .evidence(serde_json::json!({ "path": CONFIG, "error": err.to_string() }))
            .remediation("Grant fs.read for /etc/ssh/sshd_config and retry.");
            return Ok(serde_json::json!({
                "config_path": CONFIG,
                "permit_root_login": serde_json::Value::Null,
                "password_authentication": serde_json::Value::Null,
                "findings": [finding],
            }));
        }
    };
    let permit = directive(&text, "PermitRootLogin");
    let password = directive(&text, "PasswordAuthentication");
    let root = compare(
        "ssh.permit_root_login",
        "PermitRootLogin",
        permit.as_deref(),
        &expect_root,
    );
    let pass = compare(
        "ssh.password_authentication",
        "PasswordAuthentication",
        password.as_deref(),
        &expect_password,
    );
    Ok(serde_json::json!({
        "config_path": CONFIG,
        "permit_root_login": permit,
        "password_authentication": password,
        "findings": [root, pass],
    }))
}

fn str_param(params: &serde_json::Value, key: &str, default: &str) -> String {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or(default)
        .to_string()
}

/// Last non-comment assignment wins. A missing value does not clear an earlier one.
fn directive(content: &str, key: &str) -> Option<String> {
    let mut last = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(found) = parts.next() else {
            continue;
        };
        if found.eq_ignore_ascii_case(key) {
            if let Some(value) = parts.next() {
                last = Some(value.to_string());
            }
        }
    }
    last
}

fn compare(id: &str, key: &str, actual: Option<&str>, expected: &str) -> novbot_skill_sdk::Finding {
    let evidence = serde_json::json!({
        "key": key,
        "value": actual,
        "expected": expected,
    });
    match actual {
        Some(value) if value.eq_ignore_ascii_case(expected) => {
            novbot_skill_sdk::Finding::new(id, novbot_skill_sdk::FindingStatus::Pass, "high")
                .evidence(evidence)
                .remediation("No action required.")
        }
        Some(_) => {
            novbot_skill_sdk::Finding::new(id, novbot_skill_sdk::FindingStatus::Fail, "high")
                .evidence(evidence)
                .remediation(format!("Set {key} {expected}."))
        }
        None if expected.eq_ignore_ascii_case("no") => {
            novbot_skill_sdk::Finding::new(id, novbot_skill_sdk::FindingStatus::Fail, "high")
                .evidence(evidence)
                .remediation(format!("Set {key} {expected}."))
        }
        None => novbot_skill_sdk::Finding::new(id, novbot_skill_sdk::FindingStatus::Pass, "high")
            .evidence(evidence)
            .remediation("No action required."),
    }
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::{check, directive};
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn directive_last_wins_and_skips_comments() {
        let cfg = "# PermitRootLogin yes\nPermitRootLogin prohibit-password\nPermitRootLogin no\n";
        assert_eq!(directive(cfg, "PermitRootLogin").as_deref(), Some("no"));
    }

    #[test]
    fn matching_no_is_a_pass() {
        let mut host = MockHost::new();
        host.insert_file(
            "/etc/ssh/sshd_config",
            b"PermitRootLogin yes\nPermitRootLogin no\nPasswordAuthentication no\n",
        );
        let (value, _) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["permit_root_login"], "no");
        assert_eq!(value["password_authentication"], "no");
        assert_eq!(value["findings"][0]["status"], "pass");
        assert_eq!(value["findings"][1]["status"], "pass");
        assert_eq!(value["findings"][0]["id"], "ssh.permit_root_login");
    }

    #[test]
    fn missing_expected_no_is_a_fail() {
        let mut host = MockHost::new();
        host.insert_file("/etc/ssh/sshd_config", b"# PermitRootLogin no\n");
        let (value, _) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert!(value["permit_root_login"].is_null());
        assert_eq!(value["findings"][0]["status"], "fail");
        assert_eq!(value["findings"][1]["status"], "fail");
    }
}
