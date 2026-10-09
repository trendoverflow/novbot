// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! SH-6 execution tests. These do not need MySQL.

#[path = "hub_pack.rs"]
mod hub_pack;

use crate::execute::execute_spec;
use crate::skills::{
    current_platform, pack_skill, sha256_hex, ArtifactSource, DesiredSet, DesiredSkill, Fetched,
    InstallLimits, PackSpec, SkillHost, SkillPolicy, ABI,
};
use async_trait::async_trait;
use novbot_core::{ProbePolicy, Spec, SpecKind};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GUEST: &[u8] = include_bytes!("../../novbot-skill-runtime/guest/skill.wasm");

struct MapSource {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

#[async_trait]
impl ArtifactSource for MapSource {
    async fn fetch(
        &self,
        sha256: &str,
        fetch_ticket: &str,
        offset: u64,
    ) -> Result<Fetched, String> {
        if fetch_ticket.is_empty() {
            return Err("missing fetch ticket".into());
        }
        let bytes = self
            .files
            .lock()
            .unwrap()
            .get(sha256)
            .cloned()
            .ok_or_else(|| "artifact missing".to_string())?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        Ok(Fetched {
            total_size: bytes.len() as u64,
            data: bytes[start..].to_vec(),
        })
    }
}

fn limits() -> InstallLimits {
    InstallLimits {
        max_attempts: 2,
        max_elapsed: Duration::from_secs(600),
        min_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        ready_bound: Duration::from_millis(20),
        sleeper: Arc::new(|_| Box::pin(async {})),
    }
}

async fn install(packages: &[Vec<u8>], max_concurrent_runs: u32) -> (tempfile::TempDir, SkillHost) {
    let source = Arc::new(MapSource {
        files: Mutex::new(BTreeMap::new()),
    });
    let mut skills = Vec::new();
    for bytes in packages {
        let package = crate::skills::load_package(bytes).expect("package");
        let sha = sha256_hex(bytes);
        source
            .files
            .lock()
            .unwrap()
            .insert(sha.clone(), bytes.clone());
        skills.push(DesiredSkill {
            name: package.manifest.name,
            version: package.manifest.version,
            sha256: sha.clone(),
            size_bytes: bytes.len() as u64,
            abi: ABI.to_string(),
            capabilities_sha256: String::new(),
            signature_envelope: Vec::new(),
            fetch_ticket: format!("ticket-{sha}"),
        });
    }
    let dir = tempfile::tempdir().expect("data dir");
    let host = SkillHost::open(dir.path(), source, limits())
        .await
        .expect("skill host");
    let before = host.wasm_run_entries();
    let snap = host
        .reconcile(DesiredSet {
            generation: 1,
            policy: SkillPolicy {
                max_concurrent_runs,
                ..SkillPolicy::default()
            },
            skills,
        })
        .await;
    for skill in &snap.skills {
        assert_eq!(skill.state, "installed", "{snap:?}");
    }
    assert_eq!(
        host.wasm_run_entries(),
        before,
        "install must not execute the skill"
    );
    (dir, host)
}

fn spec(id: &str, kind: SpecKind, params: Value) -> Spec {
    Spec {
        id: id.to_string(),
        kind,
        params,
        threshold: None,
    }
}

fn code(payload: &Value) -> &str {
    payload["error"]["code"].as_str().unwrap_or("")
}

fn spin_package() -> Vec<u8> {
    pack_skill(&PackSpec {
        name: "spin-check".into(),
        version: "0.1.0".into(),
        wasm: GUEST.to_vec(),
        grants: vec![("sys.info.read".into(), None)],
        platforms: vec![current_platform()],
        timeout_ms: Some(400),
        memory_mb: Some(32),
    })
}

#[tokio::test]
async fn stored_timeout_and_permit_gate_the_wasm_run() {
    let (dir, host) = install(&[spin_package()], 1).await;
    let host = Arc::new(host);
    assert_eq!(host.installed_timeout_ms("spin-check"), Some(400));
    assert_eq!(
        host.installed_memory_bytes("spin-check"),
        Some(32 * 1024 * 1024)
    );

    let mismatch = execute_spec(
        &spec(
            "spin",
            SpecKind::Skill,
            json!({"skill": "spin-check", "version": "=9.9.9", "arguments": {"op": "spin"}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "mismatch",
    )
    .await;
    assert_eq!(mismatch.0, "error");
    assert_eq!(code(&mismatch.1), "version_mismatch");
    assert_eq!(host.wasm_run_entries(), 0);

    let missing = execute_spec(
        &spec("gone", SpecKind::Skill, json!({"skill": "not-installed"})),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "missing",
    )
    .await;
    assert_eq!(missing.0, "error");
    assert_eq!(code(&missing.1), "skill_not_installed");
    assert!(missing.1.get("skill").is_none());
    assert_eq!(host.wasm_run_entries(), 0);

    let permit = host.try_acquire_run().expect("the only slot");
    let builtin = execute_spec(
        &spec("host", SpecKind::Skill, json!({"skill": "host_info"})),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "builtin",
    )
    .await;
    assert_eq!(builtin.0, "ok", "{builtin:?}");
    let echo = execute_spec(
        &spec(
            "echo",
            SpecKind::McpTool,
            json!({"tool": "echo", "arguments": {"text": "hi"}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "echo",
    )
    .await;
    assert_eq!(echo.0, "ok", "{echo:?}");
    assert_eq!(host.wasm_run_entries(), 0, "built-ins must not enter run");
    drop(permit);

    let started = Instant::now();
    let before = host.wasm_run_entries();
    let spinning = {
        let host = Arc::clone(&host);
        let data = dir.path().to_path_buf();
        tokio::spawn(async move {
            execute_spec(
                &spec(
                    "spin",
                    SpecKind::Skill,
                    json!({"skill": "spin-check", "version": "^0.1", "arguments": {"op": "spin"}}),
                ),
                &ProbePolicy::default(),
                &host,
                &data,
                "spin",
            )
            .await
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while host.wasm_run_entries() == before {
        assert!(Instant::now() < deadline, "spin did not enter run");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let overlapped = execute_spec(
        &spec(
            "spin-2",
            SpecKind::Skill,
            json!({"skill": "spin-check", "arguments": {"op": "info"}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "overlap",
    )
    .await;
    assert_eq!(overlapped.0, "error", "{overlapped:?}");
    assert_eq!(code(&overlapped.1), "resource_limit");
    assert_eq!(
        host.wasm_run_entries(),
        before + 1,
        "a busy slot must not start another wasm run"
    );
    let finished = spinning.await.expect("spin task");
    assert_eq!(finished.0, "error", "{finished:?}");
    assert_eq!(code(&finished.1), "exec_timeout");
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "stored timeout was not applied: {:?}",
        started.elapsed()
    );
    assert_eq!(finished.1["skill"]["version"], "0.1.0");

    let denied = execute_spec(
        &spec(
            "ports",
            SpecKind::Skill,
            json!({"skill": "spin-check", "arguments": {"op": "ports"}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "ports",
    )
    .await;
    assert_eq!(denied.0, "error", "{denied:?}");
    assert_eq!(code(&denied.1), "capability_denied");
    assert_eq!(
        denied.1["denials"][0]["capability"],
        "net.listening_ports.read"
    );
    assert_eq!(denied.1["denials"][0]["reason"], "undeclared_capability");

    let again = execute_spec(
        &spec(
            "info",
            SpecKind::McpTool,
            json!({"tool": "spin-check", "arguments": {"op": "info"}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "info",
    )
    .await;
    assert_eq!(again.0, "ok", "{again:?}");
    assert_ne!(code(&again.1), "resource_limit");
    assert!(again.1["hostname"]
        .as_str()
        .is_some_and(|name| !name.is_empty()));
    assert_eq!(host.wasm_run_entries(), before + 3);
}

#[tokio::test]
async fn installed_examples_report_ok_and_denials() {
    let release = hub_pack::prepare_os_release();
    if let Some(path) = release.fixture.clone() {
        novbot_skill_runtime::set_missing_os_release_fixture(Some(path));
    }
    let _clear_fixture = ClearOsReleaseFixture;
    let packages = vec![
        hub_pack::os_release_package(),
        hub_pack::cap_violation_package(),
        hub_pack::cap_policy_package(),
    ];
    let (dir, host) = install(&packages, 1).await;
    let want_id = hub_pack::os_field(&release.text, "ID");
    let want_version = hub_pack::os_field(&release.text, "VERSION_ID");
    assert!(
        !want_id.is_empty(),
        "os-release ID is empty: {}",
        release.text
    );
    assert!(
        !want_version.is_empty(),
        "os-release VERSION_ID is empty: {}",
        release.text
    );

    let permit = host.try_acquire_run().expect("slot");
    let bad = execute_spec(
        &spec(
            "cap",
            SpecKind::Skill,
            json!({"skill": "cap-violation-test", "version": "=1.0.0", "arguments": {"mode": 8675309}}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "bad-params",
    )
    .await;
    drop(permit);
    assert_eq!(bad.0, "error", "{bad:?}");
    assert_eq!(code(&bad.1), "invalid_params");
    let pointers = bad.1["error"]["pointers"].as_array().expect("pointers");
    assert!(
        pointers.iter().any(|item| item.as_str() == Some("/mode")),
        "{bad:?}"
    );
    assert!(!bad.1.to_string().contains("8675309"), "{bad:?}");
    assert_eq!(host.wasm_run_entries(), 0);

    let os = run_os(&host, dir.path(), "^1.0").await;
    assert_eq!(os.0, "ok", "{os:?}");
    assert_eq!(os.1["skill"]["name"], "os-release-check");
    assert_eq!(os.1["skill"]["version"], "1.0.0");
    assert_eq!(os.1["skill"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(os.1["ID"], want_id);
    assert_eq!(os.1["VERSION_ID"], want_version);
    assert_eq!(os.1["hostname"], hub_pack::host_name());
    assert!(os.1.get("denials").is_none());

    let shadow = execute_spec(
        &spec(
            "cap",
            SpecKind::Skill,
            json!({"skill": "cap-violation-test", "version": "=1.0.0"}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "shadow",
    )
    .await;
    assert_denial(&shadow, "fs.read", "out_of_scope");
    assert_ne!(shadow.1["denials"][0]["reason"], "policy_denied");

    let mut undeclared_spec = spec(
        "cap",
        SpecKind::Skill,
        json!({"skill": "cap-violation-test", "version": "=1.0.0"}),
    );
    crate::execute::apply_dispatch_overlay(
        &mut undeclared_spec,
        r#"{"arguments":{"mode":"undeclared"}}"#,
    );
    let undeclared = execute_spec(
        &undeclared_spec,
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "undeclared",
    )
    .await;
    assert_denial(
        &undeclared,
        "net.listening_ports.read",
        "undeclared_capability",
    );

    let policy = execute_spec(
        &spec(
            "policy",
            SpecKind::Skill,
            json!({"skill": "cap-violation-policy", "version": "=1.0.0"}),
        ),
        &ProbePolicy::default(),
        &host,
        dir.path(),
        "policy",
    )
    .await;
    assert_denial(&policy, "fs.read", "policy_denied");

    let again = run_os(&host, dir.path(), "^1.0").await;
    assert_eq!(again.0, "ok", "{again:?}");
    assert_eq!(again.1["ID"], want_id);
    assert_eq!(host.wasm_run_entries(), 5);
}

struct ClearOsReleaseFixture;

impl Drop for ClearOsReleaseFixture {
    fn drop(&mut self) {
        novbot_skill_runtime::set_missing_os_release_fixture(None);
    }
}

async fn run_os(host: &SkillHost, data: &Path, version: &str) -> (String, Value) {
    execute_spec(
        &spec(
            "os",
            SpecKind::Skill,
            json!({"skill": "os-release-check", "version": version}),
        ),
        &ProbePolicy::default(),
        host,
        data,
        "os",
    )
    .await
}

fn assert_denial(outcome: &(String, Value), capability: &str, reason: &str) {
    assert_eq!(outcome.0, "error", "{outcome:?}");
    assert_eq!(code(&outcome.1), "capability_denied", "{outcome:?}");
    let denial = &outcome.1["denials"][0];
    assert_eq!(denial["capability"], capability, "{outcome:?}");
    assert_eq!(denial["reason"], reason, "{outcome:?}");
    assert!(denial["at_ms"].is_number(), "{outcome:?}");
    if capability == "fs.read" {
        let target = denial["target"].as_str().unwrap_or("");
        assert!(
            target == "/etc/shadow" || target == "/private/etc/shadow",
            "{outcome:?}"
        );
        assert_no_shadow(&outcome.1);
    }
}

fn assert_no_shadow(payload: &Value) {
    let text = payload.to_string();
    let bytes = std::fs::read("/etc/shadow")
        .or_else(|_| std::fs::read("/private/etc/shadow"))
        .ok();
    if let Some(bytes) = bytes {
        if bytes.len() >= 8 {
            let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(32)]);
            let snippet = snippet.trim();
            if snippet.len() >= 8 {
                assert!(!text.contains(snippet), "shadow bytes leaked into {text}");
            }
        }
    }
}
