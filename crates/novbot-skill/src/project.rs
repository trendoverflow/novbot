// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Build, test, and pack one skill project.
//!
//! Skill builds always use `<project>/target`. An inherited `CARGO_TARGET_DIR`
//! from a workspace `cargo test` would otherwise mix every skill into one
//! directory.

use crate::archive;
use crate::manifest;
use anyhow::Context;
use novbot_node::skills::sha256_hex;
use novbot_skill_runtime::{run, RunRequest, SkillRuntime};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub struct Packed {
    pub name: String,
    pub version: String,
    pub sha256: String,
}

pub fn build(project: &Path) -> anyhow::Result<PathBuf> {
    let status = cargo(project)
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .status()
        .context("spawn cargo")?;
    if !status.success() {
        anyhow::bail!("cargo build --release --target wasm32-wasip2 failed");
    }
    let produced = produced_wasm(project)?;
    let dest = module_path(project);
    if produced != dest {
        fs::copy(&produced, &dest)
            .with_context(|| format!("copy {} to {}", produced.display(), dest.display()))?;
    }
    let bytes = fs::read(&dest).with_context(|| format!("read {}", dest.display()))?;
    ensure_component(&bytes)?;
    Ok(dest)
}

pub fn test_project(
    project: &Path,
    params_json: &str,
    fixture_root: Option<&Path>,
) -> anyhow::Result<novbot_skill_runtime::RunOutput> {
    let manifest = manifest::load_file(&project.join("skill.toml"))?;
    let wasm = fs::read(module_path(project)).with_context(|| {
        format!(
            "read {} (run novbot-skill build first)",
            module_path(project).display()
        )
    })?;
    // Keep the staged tree alive until `run` returns. `data_dir` stays unset:
    // the runtime treats it as a denylist, checked after scope match.
    let staged = match fixture_root {
        Some(root) => Some(stage_fixture(root)?),
        None => None,
    };
    let _staged_root = staged.as_ref().map(|dir| dir.path().to_path_buf());
    let grants = manifest.grants();
    let grant_refs: Vec<&str> = grants.iter().map(String::as_str).collect();
    let params = if params_json.trim().is_empty() {
        "{}".to_string()
    } else {
        params_json.to_string()
    };
    let output = run(
        runtime()?,
        RunRequest {
            component_bytes: &wasm,
            grants: &grant_refs,
            params_json: &params,
            data_dir: None,
            timeout: manifest.timeout_ms.map(Duration::from_millis),
            memory_bytes: None,
        },
    );
    drop(staged);
    Ok(output)
}

pub fn pack(project: &Path) -> anyhow::Result<Packed> {
    let manifest = manifest::load_file(&project.join("skill.toml"))?;
    if manifest.name.contains('/')
        || manifest.version.contains('/')
        || manifest.version.contains('\\')
    {
        anyhow::bail!("name or version is not a safe file name");
    }
    let wasm_path = module_path(project);
    let wasm = fs::read(&wasm_path).with_context(|| {
        format!(
            "read {} (run novbot-skill build first)",
            wasm_path.display()
        )
    })?;
    if !wasm.starts_with(b"\0asm") {
        anyhow::bail!("module.wasm is not a wasm binary");
    }
    let mut files = BTreeMap::new();
    files.insert("module.wasm".to_string(), wasm);
    files.insert(
        "schema/params.json".to_string(),
        read_json_file(&project.join("schema/params.json"))?,
    );
    for optional in ["schema/output.json", "README.md", "LICENSE"] {
        let path = project.join(optional);
        if !path.exists() {
            continue;
        }
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        if meta.file_type().is_symlink() {
            anyhow::bail!("{} is a symlink", path.display());
        }
        if !meta.is_file() {
            continue;
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        if optional.ends_with(".json") {
            serde_json::from_slice::<Value>(&bytes)
                .with_context(|| format!("{} is not json", path.display()))?;
        }
        files.insert(optional.to_string(), bytes);
    }
    let mut hashes = BTreeMap::new();
    for (path, bytes) in &files {
        hashes.insert(path.clone(), sha256_hex(bytes));
    }
    let rendered = manifest::render(&manifest, &hashes);
    fs::write(project.join("skill.toml"), &rendered).context("write skill.toml")?;
    let bytes = archive::pack(&rendered, &files).context("pack archive")?;
    let sha256 = sha256_hex(&bytes);
    let file_name = format!("{}-{}.nbskill", manifest.name, manifest.version);
    let package_path = project.join(&file_name);
    let sha_path = project.join(format!("{file_name}.sha256"));
    fs::write(&package_path, &bytes)
        .with_context(|| format!("write {}", package_path.display()))?;
    fs::write(&sha_path, format!("{sha256}\n"))
        .with_context(|| format!("write {}", sha_path.display()))?;
    Ok(Packed {
        name: manifest.name,
        version: manifest.version,
        sha256,
    })
}

/// Copy `root` into a new temporary directory.
///
/// Callers must not pass that directory as `RunRequest.data_dir`.
pub fn stage_fixture(root: &Path) -> anyhow::Result<tempfile::TempDir> {
    if !root.is_dir() {
        anyhow::bail!("{} is not a directory", root.display());
    }
    let staged = tempfile::tempdir().context("fixture temp dir")?;
    copy_tree(root, staged.path())?;
    Ok(staged)
}

fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let entry = entry?;
        let path = entry.path();
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("stat {}", path.display()))?;
        let name = entry.file_name();
        let dest = dst.join(&name);
        if meta.file_type().is_symlink() {
            anyhow::bail!("{} is a symlink", path.display());
        } else if meta.is_dir() {
            fs::create_dir_all(&dest)?;
            copy_tree(&path, &dest)?;
        } else if meta.is_file() {
            fs::copy(&path, &dest).with_context(|| format!("copy {}", path.display()))?;
        }
    }
    Ok(())
}

fn read_json_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice::<Value>(&bytes)
        .with_context(|| format!("{} is not json", path.display()))?;
    Ok(bytes)
}

fn module_path(project: &Path) -> PathBuf {
    release_dir(project).join("module.wasm")
}

fn release_dir(project: &Path) -> PathBuf {
    project.join("target/wasm32-wasip2/release")
}

fn produced_wasm(project: &Path) -> anyhow::Result<PathBuf> {
    let name = lib_file_name(project)?;
    let path = release_dir(project).join(name);
    if path.is_file() {
        return Ok(path);
    }
    anyhow::bail!(
        "missing {} after cargo build (expected the cdylib)",
        path.display()
    );
}

fn lib_file_name(project: &Path) -> anyhow::Result<String> {
    let text = fs::read_to_string(project.join("Cargo.toml")).context("read Cargo.toml")?;
    let cargo: CargoToml = toml::from_str(&text).context("Cargo.toml")?;
    let name = cargo
        .lib
        .and_then(|lib| lib.name)
        .unwrap_or(cargo.package.name)
        .replace('-', "_");
    Ok(format!("{name}.wasm"))
}

#[derive(Debug, Deserialize)]
struct CargoToml {
    package: CargoPackage,
    #[serde(default)]
    lib: Option<CargoLib>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
}

#[derive(Debug, Deserialize)]
struct CargoLib {
    #[serde(default)]
    name: Option<String>,
}

fn cargo(project: &Path) -> Command {
    let program = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(program);
    cmd.current_dir(project);
    cmd.env_remove("CARGO_TARGET_DIR");
    cmd.env_remove("RUSTFLAGS");
    cmd.env_remove("CARGO_ENCODED_RUSTFLAGS");
    cmd
}

fn ensure_component(bytes: &[u8]) -> anyhow::Result<()> {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    let engine = wasmtime::Engine::new(&config)?;
    wasmtime::component::Component::new(&engine, bytes)
        .map_err(|err| anyhow::anyhow!("cargo output is not a wasm component: {err}"))?;
    Ok(())
}

fn runtime() -> anyhow::Result<&'static SkillRuntime> {
    use std::sync::OnceLock;
    static RUNTIME: OnceLock<SkillRuntime> = OnceLock::new();
    if RUNTIME.get().is_none() {
        let created = SkillRuntime::new().map_err(|err| anyhow::anyhow!("skill runtime: {err}"))?;
        let _ = RUNTIME.set(created);
    }
    Ok(RUNTIME.get().expect("skill runtime"))
}

#[cfg(test)]
mod tests {
    use super::{module_path, pack, release_dir, stage_fixture, test_project};
    use crate::inspect;
    use novbot_node::skills::sha256_hex;
    use novbot_skill_runtime::{run, DenialReason, RunRequest, SkillRuntime};
    use std::fs;
    use std::path::Path;

    const GUEST: &[u8] = include_bytes!("../../novbot-skill-runtime/guest/skill.wasm");

    #[test]
    fn pack_roundtrip_opens_and_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        fs::create_dir_all(project.join("schema")).unwrap();
        fs::create_dir_all(release_dir(project)).unwrap();
        fs::write(module_path(project), GUEST).unwrap();
        fs::write(
            project.join("schema/params.json"),
            br#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#,
        )
        .unwrap();
        fs::write(
            project.join("skill.toml"),
            r#"
schema_version = 1
name = "pack-roundtrip"
version = "0.1.0"
description = "roundtrip"

[runtime]
kind = "wasm"
abi = "novbot:skill@1"

[runtime.limits]
timeout_ms = 5000

[files]

[[capabilities]]
name = "fs.read"
scope = "/etc/os-release"
reason = "read os release"
"#,
        )
        .unwrap();
        let packed = pack(project).unwrap();
        let package_path = project.join("pack-roundtrip-0.1.0.nbskill");
        let package_bytes = fs::read(&package_path).unwrap();
        let package = novbot_node::skills::load_package(&package_bytes).unwrap();
        assert_eq!(package.manifest.name, "pack-roundtrip");
        assert_eq!(package.manifest.version, "0.1.0");
        assert_eq!(
            package.manifest.grants,
            vec!["fs.read:/etc/os-release".to_string()]
        );
        assert_eq!(package.manifest.timeout_ms, Some(5000));
        assert!(!package.manifest.files.contains_key("skill.toml"));
        assert_eq!(
            package
                .manifest
                .files
                .get("module.wasm")
                .map(String::as_str),
            Some(sha256_hex(GUEST).as_str())
        );
        let again = pack(project).unwrap();
        assert_eq!(package_bytes, fs::read(&package_path).unwrap());
        assert_eq!(packed.sha256, again.sha256);
        let sha_text =
            fs::read_to_string(project.join("pack-roundtrip-0.1.0.nbskill.sha256")).unwrap();
        assert_eq!(sha_text, format!("{}\n", packed.sha256));
        let opened =
            novbot_node::skills::open_package(&package_bytes, "pack-roundtrip", "0.1.0").unwrap();
        assert_eq!(opened.manifest.version, "0.1.0");
    }

    #[test]
    fn fixture_root_is_not_passed_as_data_dir() {
        // `data_dir` is a denylist applied after an in-scope match. Passing the
        // fixture there turns this read into `policy_denied`. `--fixture-root`
        // stages a temp copy and leaves `data_dir` unset, so the granted read
        // still returns the file.
        let fixture = tempfile::tempdir().unwrap();
        let note = fixture.path().join("note.txt");
        fs::write(&note, b"fixture-ok").unwrap();
        let staged = stage_fixture(fixture.path()).unwrap();
        assert_eq!(
            fs::read(staged.path().join("note.txt")).unwrap(),
            b"fixture-ok"
        );

        let project = tempfile::tempdir().unwrap();
        fs::create_dir_all(release_dir(project.path())).unwrap();
        fs::write(module_path(project.path()), GUEST).unwrap();
        let grant_path = note.display().to_string();
        fs::write(
            project.path().join("skill.toml"),
            format!(
                "schema_version = 1\nname = \"fixture-read\"\nversion = \"0.1.0\"\n\n[runtime]\nkind = \"wasm\"\nabi = \"novbot:skill@1\"\n\n[files]\n\n[[capabilities]]\nname = \"fs.read\"\nscope = \"{grant_path}\"\n"
            ),
        )
        .unwrap();
        let params = serde_json::json!({
            "op": "read",
            "path": grant_path,
            "max_bytes": 64
        })
        .to_string();
        let output = test_project(project.path(), &params, Some(fixture.path())).unwrap();
        assert_eq!(output.status, "ok");
        assert!(output.denials.is_empty());
        assert!(output.output.unwrap().contains("fixture-ok"));

        let runtime = SkillRuntime::new().unwrap();
        let grant = format!("fs.read:{grant_path}");
        let grants = [grant.as_str()];
        let denied = run(
            &runtime,
            RunRequest {
                component_bytes: GUEST,
                grants: &grants,
                params_json: &params,
                data_dir: Some(fixture.path()),
                timeout: None,
                memory_bytes: None,
            },
        );
        assert_eq!(denied.status, "error");
        assert_eq!(denied.denials[0].reason, DenialReason::PolicyDenied);
        assert!(denied.output.is_none());
        let text = serde_json::to_string(&denied).unwrap();
        assert!(!text.contains("fixture-ok"));
    }

    #[test]
    fn inspect_prints_grants_and_lints_net() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        fs::create_dir_all(project.join("schema")).unwrap();
        fs::create_dir_all(release_dir(project)).unwrap();
        fs::write(module_path(project), GUEST).unwrap();
        fs::write(project.join("schema/params.json"), br#"{"type":"object"}"#).unwrap();
        fs::write(
            project.join("skill.toml"),
            r#"
schema_version = 1
name = "inspect-lint"
version = "0.1.0"

[runtime]
kind = "wasm"
abi = "novbot:skill@1"

[files]

[[capabilities]]
name = "fs.read"
scope = "/etc/os-release"
"#,
        )
        .unwrap();
        pack(project).unwrap();
        let text = inspect::inspect(&project.join("inspect-lint-0.1.0.nbskill")).unwrap();
        assert!(text.contains("grants:\n- fs.read:/etc/os-release\n"));
        assert!(text.contains("\n- novbot:skill/net@1.0.0\n") || text.contains("novbot:skill/net"));
        assert!(text.contains("lint: module imports net but declares no net.* grant\n"));
    }

    #[test]
    fn center_fixtures_stay_byte_for_byte() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../novbot-center/fixtures");
        let current = fs::read(root.join("os-release-check-1.0.0.nbskill")).unwrap();
        let tamper = fs::read(root.join("os-release-check-1.1.1.nbskill")).unwrap();
        assert_eq!(
            sha256_hex(&current),
            "b6cb8d7a529937622c561cf47e7937c1ec5fa48e777f39181beb57f6bd3643f1"
        );
        assert_eq!(
            sha256_hex(&tamper),
            "cfba2171f2ce179b41214ece6248921372db6e526a564f679ecacb0019887d91"
        );
    }
}
