// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Test-only packing of the example skills.
//!
//! Included from the node tests and the center D10 test. Uses only `std` so
//! both crates can compile it. Packed bytes stay in a temp cache, never in git.

use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_SALT: &str = "sh6-pack-v1";
const PLATFORMS: &str =
    "platforms = [\"darwin/arm64\", \"darwin/amd64\", \"linux/arm64\", \"linux/amd64\"]\n";

pub fn os_release_package() -> Vec<u8> {
    pack_example("os-release-check", false)
}

/// Packed `examples/os-release-check-1.1.0`. The skill name stays `os-release-check`.
///
/// Other includers of this file do not call it. `dead_code` stays allowed so
/// those copies still compile under `-D warnings`.
#[allow(dead_code)]
pub fn os_release_110_package() -> Vec<u8> {
    pack_example("os-release-check-1.1.0", false)
}

pub fn cap_violation_package() -> Vec<u8> {
    pack_example("cap-violation-test", false)
}

pub fn cap_policy_package() -> Vec<u8> {
    pack_example("cap-violation-test", true)
}

/// Text `os-release-check` should report, plus a temp fixture when the host
/// has no `/etc/os-release`. The caller installs `fixture` on the runtime.
pub struct OsReleaseFile {
    pub text: String,
    pub fixture: Option<PathBuf>,
}

pub fn prepare_os_release() -> OsReleaseFile {
    for path in ["/etc/os-release", "/private/etc/os-release"] {
        if fs::metadata(path).is_ok() {
            let text = fs::read_to_string(path).unwrap_or_else(|err| panic!("read {path}: {err}"));
            return OsReleaseFile {
                text,
                fixture: None,
            };
        }
    }
    let path = std::env::temp_dir().join(format!("novbot-os-release-{}.txt", std::process::id()));
    let text = "ID=novbot\nVERSION_ID=\"26.6\"\nNAME=\"NovBot Test\"\n";
    fs::write(&path, text).expect("write os-release fixture");
    OsReleaseFile {
        text: text.to_string(),
        fixture: Some(path),
    }
}

pub fn os_field(text: &str, key: &str) -> String {
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

pub fn host_name() -> String {
    let out = Command::new("uname").arg("-n").output().expect("uname");
    assert!(out.status.success(), "uname failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn pack_example(example: &str, policy_shadow: bool) -> Vec<u8> {
    let workspace = workspace_root();
    let source = workspace.join("examples").join(example);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    CACHE_SALT.hash(&mut hasher);
    example.hash(&mut hasher);
    policy_shadow.hash(&mut hasher);
    hash_tree(&source, &mut hasher);
    let key = format!("{:016x}", hasher.finish());
    let cache_dir = PathBuf::from("/tmp/novbot-sh6-cache");
    fs::create_dir_all(&cache_dir).expect("cache dir");
    let cached = cache_dir.join(format!("{key}.nbskill"));
    if let Ok(bytes) = fs::read(&cached) {
        if !bytes.is_empty() {
            return bytes;
        }
    }
    with_cache_lock(|| {
        if let Ok(bytes) = fs::read(&cached) {
            if !bytes.is_empty() {
                return bytes;
            }
        }
        let bytes = build_package(&workspace, &source, policy_shadow);
        let tmp = cache_dir.join(format!("{key}.tmp"));
        fs::write(&tmp, &bytes).expect("write pack cache");
        fs::rename(&tmp, &cached).expect("rename pack cache");
        bytes
    })
}

fn build_package(workspace: &Path, source: &Path, policy_shadow: bool) -> Vec<u8> {
    let project = tempfile_dir();
    let _cleanup = DirGuard(project.clone());
    copy_project(source, &project);
    let sdk = workspace.join("crates/novbot-skill-sdk");
    let cargo_path = project.join("Cargo.toml");
    let cargo = fs::read_to_string(&cargo_path).expect("Cargo.toml");
    let cargo = cargo.replace(
        "../../crates/novbot-skill-sdk",
        sdk.to_str().expect("sdk path"),
    );
    fs::write(&cargo_path, cargo).expect("rewrite Cargo.toml");
    let manifest_path = project.join("skill.toml");
    let mut manifest = fs::read_to_string(&manifest_path).expect("skill.toml");
    manifest = ensure_platforms(&manifest);
    if policy_shadow {
        manifest = policy_manifest(&manifest);
    }
    fs::write(&manifest_path, &manifest).expect("rewrite skill.toml");
    let cli = skill_cli(workspace);
    let built = Command::new(&cli)
        .arg("build")
        .current_dir(&project)
        .output()
        .expect("novbot-skill build");
    assert!(
        built.status.success(),
        "novbot-skill build failed\n{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let packed = Command::new(&cli)
        .arg("pack")
        .current_dir(&project)
        .output()
        .expect("novbot-skill pack");
    assert!(
        packed.status.success(),
        "novbot-skill pack failed\n{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let name = skill_name_from(&manifest);
    let version = skill_version_from(&manifest);
    let archive = project.join(format!("{name}-{version}.nbskill"));
    fs::read(&archive).unwrap_or_else(|err| panic!("read {}: {err}", archive.display()))
}

fn skill_cli(workspace: &Path) -> PathBuf {
    existing_skill_cli(workspace).unwrap_or_else(|| {
        panic!(
            "missing {} — build it with cargo build -p novbot-skill --bin novbot-skill",
            workspace.join("target/debug/novbot-skill").display()
        )
    })
}

fn existing_skill_cli(workspace: &Path) -> Option<PathBuf> {
    let mut bases = Vec::new();
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        bases.push(PathBuf::from(dir));
    }
    bases.push(workspace.join("target"));
    bases.into_iter().find_map(|base| {
        let bin = base.join("debug/novbot-skill");
        bin.is_file().then_some(bin)
    })
}

fn ensure_platforms(toml: &str) -> String {
    if toml
        .lines()
        .any(|line| line.trim_start().starts_with("platforms"))
    {
        return toml.to_string();
    }
    if let Some(idx) = toml.find("version = ") {
        if let Some(end) = toml[idx..].find('\n') {
            let at = idx + end + 1;
            let mut out = String::new();
            out.push_str(&toml[..at]);
            out.push_str(PLATFORMS);
            out.push_str(&toml[at..]);
            return out;
        }
    }
    format!("{PLATFORMS}{toml}")
}

fn policy_manifest(toml: &str) -> String {
    let prefix = toml
        .split("[[capabilities]]")
        .next()
        .expect("capabilities")
        .replace(
            "name = \"cap-violation-test\"",
            "name = \"cap-violation-policy\"",
        );
    format!(
        "{prefix}[[capabilities]]\nname = \"fs.read\"\nscope = \"/etc/shadow\"\nreason = \"Declared so the denylist can refuse the read\"\n\n[[capabilities]]\nname = \"fs.read\"\nscope = \"/private/etc/shadow\"\nreason = \"Canonical shadow path when /etc is a symlink\"\n"
    )
}

fn skill_name_from(toml: &str) -> String {
    toml_string_field(toml, "name")
}

fn skill_version_from(toml: &str) -> String {
    toml_string_field(toml, "version")
}

fn toml_string_field(toml: &str, key: &str) -> String {
    let prefix = format!("{key} = \"");
    for line in toml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(&prefix) {
            let value = rest.split('"').next().unwrap_or("");
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    panic!("skill.toml missing {key}");
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn copy_project(source: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("project dir");
    copy_tree(source, dest);
}

fn copy_tree(source: &Path, dest: &Path) {
    for entry in
        fs::read_dir(source).unwrap_or_else(|err| panic!("read {}: {err}", source.display()))
    {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == "target" || name_str.ends_with(".nbskill") || name_str.ends_with(".sha256") {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        let meta = fs::symlink_metadata(&from).expect("stat");
        if meta.file_type().is_symlink() {
            panic!("{} is a symlink", from.display());
        } else if meta.is_dir() {
            fs::create_dir_all(&to).expect("mkdir");
            copy_tree(&from, &to);
        } else if meta.is_file() {
            fs::copy(&from, &to).unwrap_or_else(|err| panic!("copy {}: {err}", from.display()));
        }
    }
}

fn hash_tree(dir: &Path, hasher: &mut impl Hasher) {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read {}: {err}", dir.display()))
        .map(|entry| entry.expect("entry").path())
        .collect();
    names.sort();
    for path in names {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "target" || name.ends_with(".nbskill") || name.ends_with(".sha256") {
            continue;
        }
        name.hash(hasher);
        let meta = fs::symlink_metadata(&path).expect("stat");
        if meta.is_dir() {
            hash_tree(&path, hasher);
        } else if meta.is_file() {
            let bytes = fs::read(&path).expect("read");
            bytes.hash(hasher);
        }
    }
}

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn with_cache_lock<T>(body: impl FnOnce() -> T) -> T {
    let lock = PathBuf::from("/tmp/novbot-sh6-cache.lock");
    let started = Instant::now();
    loop {
        if fs::create_dir(&lock).is_ok() {
            let _guard = DirGuard(lock.clone());
            return body();
        }
        if let Ok(meta) = fs::metadata(&lock) {
            if meta
                .modified()
                .ok()
                .and_then(|time| time.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(30 * 60))
            {
                let _ = fs::remove_dir(&lock);
                continue;
            }
        }
        if started.elapsed() > Duration::from_secs(30 * 60) {
            panic!("timed out waiting for the skill pack lock");
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn tempfile_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path =
        std::env::temp_dir().join(format!("novbot-skill-pack-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("temp project");
    path
}
