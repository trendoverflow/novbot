// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! CLI acceptance tests. Wasm builds run in one test so they do not overlap.

use std::fs;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let deps = exe.parent().expect("deps directory");
    let profile = deps.parent().expect("target profile directory");
    profile.join("novbot-skill")
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn output_text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_no_token(output: &Output, token: &str) {
    let text = output_text(output);
    assert!(
        !text.contains(token),
        "command output contained the API token"
    );
}

#[test]
fn help_does_not_print_the_token() {
    let token = "sh5-token-do-not-print";
    let top = Command::new(bin())
        .arg("--help")
        .env("NOVBOT_API_TOKEN", token)
        .output()
        .expect("spawn novbot-skill");
    assert!(top.status.success(), "{}", output_text(&top));
    assert_no_token(&top, token);
    let publish = Command::new(bin())
        .args(["publish", "--help"])
        .env("NOVBOT_API_TOKEN", token)
        .output()
        .expect("spawn novbot-skill");
    assert!(publish.status.success(), "{}", output_text(&publish));
    assert_no_token(&publish, token);
    let text = output_text(&publish);
    assert!(text.contains("NOVBOT_API_TOKEN"));
    assert!(!text.contains("--dry-run"));
}

#[test]
fn publish_error_does_not_print_the_token() {
    let token = "sh5-token-do-not-print";
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("b.nbskill"), b"not-a-package").unwrap();
    fs::write(dir.path().join("a.nbskill"), b"also-not").unwrap();
    fs::write(dir.path().join("skip.sha256"), b"nope\n").unwrap();
    let output = Command::new(bin())
        .args([
            "publish",
            dir.path().to_str().unwrap(),
            "--center",
            "http://127.0.0.1:1",
            "--token",
            token,
        ])
        .env_remove("NOVBOT_API_TOKEN")
        .output()
        .expect("spawn novbot-skill");
    assert!(!output.status.success());
    assert_no_token(&output, token);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let errors: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with("error "))
        .collect();
    assert_eq!(errors.len(), 2, "{stdout}");
    assert!(errors[0].contains("a.nbskill"));
    assert!(errors[1].contains("b.nbskill"));
    assert!(errors.iter().all(|line| !line.contains('\n')));
}

#[test]
fn examples_are_distinct_and_cap_violations_match() {
    let examples = examples_dir();
    let projects = [
        "os-release-check",
        "os-release-check-1.1.0",
        "sshd-baseline",
        "listening-ports",
        "cap-violation-test",
        "cpu-usage",
        "process-top",
        "net-interface-traffic",
    ];
    let mut clean = Clean::default();
    for name in projects {
        clean.track(&examples.join(name));
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        for name in projects {
            let project = examples.join(name);
            host_test(&project);
            skill(&project, &["build"]);
            skill(&project, &["pack"]);
        }
        assert_distinct_os_release(&examples);
        assert_os_release_imports_skip_net(&examples);
        assert_cap_violation(&examples.join("cap-violation-test"));
        assert_component_loads(&examples.join("listening-ports"), &[]);
        assert_component_loads(&examples.join("sshd-baseline"), &[]);
        assert_component_loads(&examples.join("os-release-check"), &[]);
        assert_component_loads(&examples.join("os-release-check-1.1.0"), &[]);
        assert_observability_examples(&examples);
        publish_packed_does_not_print_token(&examples.join("listening-ports"));
        scaffold_builds();
    }));
    drop(clean);
    if let Err(payload) = result {
        resume_unwind(payload);
    }
}

fn assert_distinct_os_release(examples: &Path) {
    let first = load_packed(
        &examples.join("os-release-check"),
        "os-release-check-1.0.0.nbskill",
    );
    let second = load_packed(
        &examples.join("os-release-check-1.1.0"),
        "os-release-check-1.1.0.nbskill",
    );
    assert_eq!(first.manifest.name, "os-release-check");
    assert_eq!(second.manifest.name, "os-release-check");
    assert_eq!(first.manifest.version, "1.0.0");
    assert_eq!(second.manifest.version, "1.1.0");
    assert_ne!(
        first.manifest.files.get("module.wasm"),
        second.manifest.files.get("module.wasm")
    );
    assert_eq!(
        first.manifest.grants,
        vec![
            "fs.read:/etc/os-release".to_string(),
            "sys.info.read".to_string()
        ]
    );
    assert!(!first.manifest.files.contains_key("skill.toml"));
}

fn assert_os_release_imports_skip_net(examples: &Path) {
    let output = skill(
        &examples.join("os-release-check"),
        &["inspect", "os-release-check-1.0.0.nbskill"],
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("grants:\n- fs.read:/etc/os-release\n"),
        "{text}"
    );
    assert!(text.contains("- sys.info.read\n"), "{text}");
    assert!(
        !text.contains("novbot:skill/net"),
        "os-release-check imported net:\n{text}"
    );
    assert!(!text.contains("lint:"), "{text}");
}

fn assert_cap_violation(project: &Path) {
    let output = skill(project, &["inspect", "cap-violation-test-1.0.0.nbskill"]);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("lint: module imports net but declares no net.* grant\n"),
        "{text}"
    );
    assert!(text.contains("novbot:skill/net"), "{text}");

    let shadow = run_json(project, r#"{"mode":"shadow"}"#);
    assert_eq!(shadow["status"], "error", "{shadow}");
    assert_eq!(shadow["error"]["code"], "capability_denied");
    assert_eq!(shadow["denials"][0]["capability"], "fs.read");
    assert_eq!(shadow["denials"][0]["reason"], "out_of_scope");
    let target = shadow["denials"][0]["target"].as_str().unwrap();
    assert!(
        target == "/etc/shadow" || target == "/private/etc/shadow",
        "{target}"
    );
    assert!(shadow["output"].is_null());

    let undeclared = run_json(project, r#"{"mode":"undeclared"}"#);
    assert_eq!(undeclared["status"], "error", "{undeclared}");
    assert_eq!(undeclared["error"]["code"], "capability_denied");
    assert_eq!(
        undeclared["denials"][0]["capability"],
        "net.listening_ports.read"
    );
    assert_eq!(undeclared["denials"][0]["reason"], "undeclared_capability");
}

fn assert_observability_examples(examples: &Path) {
    let cases = [
        ("cpu-usage", "cpu-usage-1.0.0.nbskill", "sys.metrics.read"),
        ("process-top", "process-top-1.0.0.nbskill", "proc.list.read"),
        (
            "net-interface-traffic",
            "net-interface-traffic-1.0.0.nbskill",
            "net.interfaces.read",
        ),
    ];
    for (dir, file, grant) in cases {
        let project = examples.join(dir);
        let package = load_packed(&project, file);
        assert_eq!(package.manifest.name, dir);
        assert_eq!(package.manifest.version, "1.0.0");
        assert_eq!(package.manifest.grants, vec![grant.to_string()]);
        let output = skill(&project, &["test"]);
        let value: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("run json");
        let code = value["error"]["code"].as_str().unwrap_or("");
        assert_ne!(code, "capability_unsupported", "{value}");
        assert_ne!(code, "invalid_component", "{value}");
        assert!(value["denials"].as_array().unwrap().is_empty(), "{value}");
        if value["status"] == "ok" {
            let payload: serde_json::Value =
                serde_json::from_str(value["output"].as_str().unwrap_or("")).expect("guest json");
            if dir == "process-top" {
                let text = payload.to_string();
                assert!(!text.contains("cmdline"), "{text}");
                assert!(!text.contains("command_line"), "{text}");
                assert!(payload["by_cpu"].is_array(), "{payload}");
            }
            if dir == "cpu-usage" {
                assert!(payload.get("cpu_usage_percent").is_some(), "{payload}");
            }
            if dir == "net-interface-traffic" {
                assert!(payload["interfaces"].is_array(), "{payload}");
            }
        } else {
            assert_eq!(value["error"]["code"], "guest_error", "{value}");
            let message = value["error"]["message"].as_str().unwrap_or("");
            assert!(message.starts_with("io:"), "{value}");
        }
        let inspected = skill(&project, &["inspect", file]);
        let text = String::from_utf8_lossy(&inspected.stdout);
        assert!(!text.contains("lint:"), "{text}");
        assert!(text.contains(grant), "{text}");
    }
}

fn assert_component_loads(project: &Path, extra: &[&str]) {
    let mut args = vec!["test"];
    args.extend_from_slice(extra);
    let output = skill(project, &args);
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("run json");
    if value["status"] == "error" {
        assert_ne!(value["error"]["code"], "invalid_component", "{value}");
    }
}

fn publish_packed_does_not_print_token(project: &Path) {
    let token = "sh5-token-do-not-print";
    let mut package = None;
    for entry in fs::read_dir(project).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("nbskill") {
            package = Some(path);
            break;
        }
    }
    let package = package.expect("packed nbskill");
    let output = Command::new(bin())
        .args([
            "publish",
            package.to_str().unwrap(),
            "--center",
            "http://127.0.0.1:1",
            "--token",
            token,
        ])
        .env_remove("NOVBOT_API_TOKEN")
        .output()
        .expect("spawn publish");
    assert!(!output.status.success());
    assert_no_token(&output, token);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("error "), "{stdout}");
    assert_eq!(stdout.lines().count(), 1, "{stdout}");
}

fn scaffold_builds() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .current_dir(dir.path())
        .args(["new", "sample-skill"])
        .output()
        .expect("spawn new");
    assert!(output.status.success(), "{}", output_text(&output));
    let project = dir.path().join("sample-skill");
    assert!(project.join("schema/params.json").is_file());
    assert!(project.join("README.md").is_file());
    host_test(&project);
    skill(&project, &["build"]);
}

fn run_json(project: &Path, params: &str) -> serde_json::Value {
    let output = skill(project, &["test", "--params", params]);
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("run json")
}

fn load_packed(project: &Path, file: &str) -> novbot_node::skills::Package {
    let bytes = fs::read(project.join(file)).unwrap();
    novbot_node::skills::load_package(&bytes)
        .unwrap_or_else(|err| panic!("open {file}: {} {}", err.reason, err.detail))
}

fn host_test(project: &Path) {
    let output = cargo(project)
        .arg("test")
        .output()
        .expect("spawn cargo test");
    assert!(
        output.status.success(),
        "cargo test {} failed\n{}",
        project.display(),
        output_text(&output)
    );
}

fn skill(project: &Path, args: &[&str]) -> Output {
    let output = Command::new(bin())
        .current_dir(project)
        .args(args)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .output()
        .expect("spawn novbot-skill");
    assert!(
        output.status.success(),
        "novbot-skill {} failed in {}\n{}",
        args.join(" "),
        project.display(),
        output_text(&output)
    );
    output
}

fn cargo(project: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO"));
    cmd.current_dir(project)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    cmd
}

#[derive(Default)]
struct Clean {
    projects: Vec<(PathBuf, String)>,
}

impl Clean {
    fn track(&mut self, project: &Path) {
        let original = fs::read_to_string(project.join("skill.toml"))
            .unwrap_or_else(|_| panic!("read {}", project.display()));
        self.projects.push((project.to_path_buf(), original));
    }
}

impl Drop for Clean {
    fn drop(&mut self) {
        for (project, original) in &self.projects {
            let _ = fs::write(project.join("skill.toml"), original);
            let Ok(entries) = fs::read_dir(project) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if name.ends_with(".nbskill") || name.ends_with(".sha256") {
                    let _ = fs::remove_file(path);
                }
            }
        }
    }
}
