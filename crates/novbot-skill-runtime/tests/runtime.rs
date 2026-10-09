// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Runtime tests instantiate the committed guest component.

use novbot_skill_runtime::{run, RunRequest, SkillRuntime};
use serde_json::json;
use std::path::Path;
use std::sync::OnceLock;

const GUEST: &[u8] = include_bytes!("../guest/skill.wasm");

fn runtime() -> &'static SkillRuntime {
    static RUNTIME: OnceLock<SkillRuntime> = OnceLock::new();
    RUNTIME.get_or_init(|| SkillRuntime::new().expect("engine"))
}

fn invoke(
    grants: &[&str],
    params: serde_json::Value,
    data_dir: Option<&Path>,
) -> serde_json::Value {
    let params_json = params.to_string();
    let output = run(
        runtime(),
        RunRequest {
            component_bytes: GUEST,
            grants,
            params_json: &params_json,
            data_dir,
            timeout: None,
            memory_bytes: None,
        },
    );
    serde_json::to_value(&output).expect("json")
}

fn assert_absent(json: &serde_json::Value, secret: &str) {
    let text = json.to_string();
    assert!(
        !text.contains(secret),
        "secret leaked into runtime json: {text}"
    );
}

#[test]
fn happy_path_reads_granted_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("allowed.txt");
    let secret = "happy-path-bytes";
    std::fs::write(&file, secret).unwrap();
    let grant = format!("fs.read:{}", file.display());
    let json = invoke(
        &[&grant],
        json!({"op": "read", "path": file.display().to_string(), "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "ok");
    assert!(json["denials"].as_array().unwrap().is_empty());
    assert_eq!(json["error"], serde_json::Value::Null);
    assert!(json["output"].as_str().unwrap().contains(secret));
}

#[test]
fn out_of_scope() {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("allowed.txt");
    let other = dir.path().join("other.txt");
    std::fs::write(&allowed, "allowed-bytes").unwrap();
    let secret = "OUT_OF_SCOPE_OTHER_BYTES";
    std::fs::write(&other, secret).unwrap();
    let grant = format!("fs.read:{}", allowed.display());
    let json = invoke(
        &[&grant],
        json!({"op": "read", "path": other.display().to_string(), "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["reason"], "out_of_scope");
    assert_eq!(json["denials"][0]["capability"], "fs.read");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn undeclared_capability() {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("allowed.txt");
    std::fs::write(&allowed, "allowed-bytes").unwrap();
    let grant = format!("fs.read:{}", allowed.display());
    let json = invoke(&[&grant], json!({"op": "ports"}), None);
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["capability"], "net.listening_ports.read");
    assert_eq!(json["denials"][0]["reason"], "undeclared_capability");
}

#[test]
fn policy_denied() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("secret.txt");
    let secret = "DATA_DIR_SECRET_BYTES";
    std::fs::write(&file, secret).unwrap();
    let grant = format!("fs.read:{}", file.display());
    let json = invoke(
        &[&grant],
        json!({"op": "read", "path": file.display().to_string(), "max_bytes": 4096}),
        Some(data.path()),
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["reason"], "policy_denied");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn policy_denied_env() {
    let secret = "policy-denied-env-value-9f3a";
    std::env::set_var("NOVBOT_TEST_TOKEN", secret);
    let json = invoke(
        &["env.read:NOVBOT_TEST_TOKEN"],
        json!({"op": "env", "key": "NOVBOT_TEST_TOKEN"}),
        None,
    );
    std::env::remove_var("NOVBOT_TEST_TOKEN");
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["capability"], "env.read");
    assert_eq!(json["denials"][0]["reason"], "policy_denied");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn dotdot_does_not_escape() {
    let root = tempfile::tempdir().unwrap();
    let grant_dir = root.path().join("grant");
    std::fs::create_dir_all(grant_dir.join("subdir")).unwrap();
    let outside = root.path().join("outside.txt");
    let secret = "DOTDOT_OUTSIDE_BYTES";
    std::fs::write(&outside, secret).unwrap();
    let grant = format!("fs.read:{}/**", grant_dir.display());
    let raw = format!("{}/subdir/../../outside.txt", grant_dir.display());
    let json = invoke(
        &[&grant],
        json!({"op": "read", "path": raw, "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["reason"], "out_of_scope");
    assert_absent(&json, secret);
}

#[test]
fn symlink_does_not_escape() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("in");
    std::fs::create_dir_all(&inside).unwrap();
    let outside = root.path().join("outside.txt");
    let secret = "SYMLINK_OUTSIDE_BYTES";
    std::fs::write(&outside, secret).unwrap();
    std::os::unix::fs::symlink(&outside, inside.join("link")).unwrap();
    let grant = format!("fs.read:{}/**", inside.display());
    let link = inside.join("link");
    let json = invoke(
        &[&grant],
        json!({"op": "read", "path": link.display().to_string(), "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["reason"], "out_of_scope");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn undeclared_returns_no_partial_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    let secret = "EMPTY_GRANT_SECRET_BYTES";
    std::fs::write(&file, secret).unwrap();
    let json = invoke(
        &[],
        json!({"op": "read", "path": file.display().to_string(), "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["reason"], "undeclared_capability");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn unknown_grant_does_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    let secret = "UNKNOWN_GRANT_SECRET_BYTES";
    std::fs::write(&file, secret).unwrap();
    let grant = format!("fs.read:{}", file.display());
    let json = invoke(
        &[&grant, "proc.cmdline.read"],
        json!({"op": "read", "path": file.display().to_string(), "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_unsupported");
    assert!(json["denials"].as_array().unwrap().is_empty());
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, secret);
}

#[test]
fn observability_grants_are_supported() {
    let json = invoke(
        &[
            "sys.info.read",
            "sys.metrics.read",
            "proc.list.read",
            "net.interfaces.read",
        ],
        json!({"op": "info"}),
        None,
    );
    assert_eq!(json["status"], "ok", "{json}");
}

#[test]
fn shadow_declared_is_policy_denied() {
    let json = invoke(
        &["fs.read:/etc/shadow"],
        json!({"op": "read", "path": "/etc/shadow", "max_bytes": 4096}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["capability"], "fs.read");
    assert_eq!(json["denials"][0]["reason"], "policy_denied");
    assert_ne!(json["denials"][0]["reason"], "out_of_scope");
    assert_eq!(json["output"], serde_json::Value::Null);
    assert_eq!(json["partial"], serde_json::Value::Null);
}

#[test]
fn denial_limit_traps_the_guest() {
    let dir = tempfile::tempdir().unwrap();
    let allowed = dir.path().join("allowed.txt");
    std::fs::write(&allowed, "ok").unwrap();
    let grant = format!("fs.read:{}", allowed.display());
    let outside = dir.path().join("outside.txt");
    std::fs::write(&outside, "LIMIT_SECRET_BYTES").unwrap();
    let json = invoke(
        &[&grant],
        json!({"op": "deny_loop", "path": outside.display().to_string()}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"].as_array().unwrap().len(), 16);
    assert_eq!(json["partial"], serde_json::Value::Null);
    assert_absent(&json, "LIMIT_SECRET_BYTES");
}

#[test]
fn wasi_filesystem_is_undeclared() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("secret.txt");
    let secret = "WASI_FS_SECRET_BYTES";
    std::fs::write(&file, secret).unwrap();
    let json = invoke(
        &["sys.info.read"],
        json!({"op": "wasi_fs", "path": file.display().to_string()}),
        None,
    );
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["code"], "capability_denied");
    assert_eq!(json["denials"][0]["capability"], "wasi:filesystem");
    assert_eq!(json["denials"][0]["reason"], "undeclared_capability");
    assert_absent(&json, secret);
}

#[test]
fn spinning_guest_hits_exec_timeout() {
    let started = std::time::Instant::now();
    let params = json!({"op": "spin"}).to_string();
    let output = run(
        runtime(),
        RunRequest {
            component_bytes: GUEST,
            grants: &[],
            params_json: &params,
            data_dir: None,
            timeout: Some(std::time::Duration::from_millis(30)),
            memory_bytes: None,
        },
    );
    assert_eq!(output.status, "error");
    assert_eq!(
        output.error.as_ref().map(|err| err.code.as_str()),
        Some("exec_timeout")
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "timeout took {:?}",
        started.elapsed()
    );
}

#[test]
fn memory_cap_is_enforced() {
    let params = json!({"op": "info"}).to_string();
    let output = run(
        runtime(),
        RunRequest {
            component_bytes: GUEST,
            grants: &["sys.info.read"],
            params_json: &params,
            data_dir: None,
            timeout: None,
            memory_bytes: Some(1),
        },
    );
    assert_eq!(output.status, "error");
    assert_eq!(
        output.error.as_ref().map(|err| err.code.as_str()),
        Some("resource_limit")
    );
}

#[test]
fn compile_rejects_serialized_cwasm() {
    let rt = runtime();
    let cwasm = rt.compile_cwasm(GUEST).expect("compile guest");
    assert!(cwasm.len() > 8);
    let rejected = rt.compile_cwasm(&cwasm).expect_err("cwasm is not input");
    assert!(rejected.starts_with("compile_failed"), "{rejected}");
}
