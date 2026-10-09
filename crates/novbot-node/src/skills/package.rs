// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `.nbskill` extract and manifest checks (T1.2). The node re-checks every package.

use super::InstallError;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path};
use tar::{Builder, EntryType};

pub const PACKAGE_MAX: u64 = 16 * 1024 * 1024;
pub const EXTRACT_MAX: u64 = 32 * 1024 * 1024;
pub const ENTRY_MAX: usize = 256;
pub const WASM_MAX: u64 = 12 * 1024 * 1024;
pub const ABI: &str = "novbot:skill@1";

/// Install catalog for this node. `proc.cmdline.read` is not included.
pub const CATALOG: &[&str] = &[
    "fs.read",
    "fs.stat",
    "fs.list",
    "net.listening_ports.read",
    "sys.info.read",
    "sys.time_sync.read",
    "env.read",
    "sys.metrics.read",
    "proc.list.read",
    "net.interfaces.read",
];

#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub abi: String,
    pub platforms: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub memory_mb: Option<u64>,
    pub files: BTreeMap<String, String>,
    pub grants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub files: BTreeMap<String, Vec<u8>>,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn current_platform() -> String {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{os}/{arch}")
}

pub fn open_package(
    bytes: &[u8],
    expect_name: &str,
    expect_version: &str,
) -> Result<Package, InstallError> {
    load_package_inner(bytes, Some((expect_name, expect_version)))
}

/// Open an archive and validate it against the manifest stored inside.
pub fn load_package(bytes: &[u8]) -> Result<Package, InstallError> {
    load_package_inner(bytes, None)
}

fn load_package_inner(
    bytes: &[u8],
    expected: Option<(&str, &str)>,
) -> Result<Package, InstallError> {
    if bytes.len() as u64 > PACKAGE_MAX {
        return Err(fail("invalid_archive", "package exceeds 16 MiB"));
    }
    let files = unpack(bytes)?;
    let toml_bytes = files
        .get("skill.toml")
        .ok_or_else(|| fail("invalid_manifest", "skill.toml is required"))?;
    let toml_text = std::str::from_utf8(toml_bytes)
        .map_err(|_| fail("invalid_manifest", "skill.toml is not utf-8"))?;
    let manifest = parse_manifest(toml_text)?;
    if let Some((expect_name, expect_version)) = expected {
        if manifest.name != expect_name || manifest.version != expect_version {
            return Err(fail(
                "invalid_manifest",
                format!(
                    "manifest {}@{} does not match desired {}@{}",
                    manifest.name, manifest.version, expect_name, expect_version
                ),
            ));
        }
    }
    if manifest.abi != ABI {
        return Err(fail(
            "abi_unsupported",
            format!("manifest abi {} is not {ABI}", manifest.abi),
        ));
    }
    verify_file_hashes(&files, &manifest.files)?;
    if !files.contains_key("module.wasm") {
        return Err(fail("invalid_manifest", "module.wasm is required"));
    }
    for grant in &manifest.grants {
        let name = grant.split_once(':').map(|(name, _)| name).unwrap_or(grant);
        if !CATALOG.contains(&name) {
            return Err(fail(
                "capability_unsupported",
                format!("grant {grant} is not in this node's catalog"),
            ));
        }
    }
    Ok(Package { manifest, files })
}

pub fn module_hash_from_toml(toml_text: &str) -> Result<String, InstallError> {
    let manifest = parse_manifest(toml_text)?;
    manifest
        .files
        .get("module.wasm")
        .cloned()
        .ok_or_else(|| fail("invalid_manifest", "module.wasm hash is missing"))
}

fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, InstallError> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|err| fail("invalid_archive", format!("archive: {err}")))?;
    let mut files = BTreeMap::new();
    let mut count = 0usize;
    let mut total = 0u64;
    for entry in entries {
        let mut entry =
            entry.map_err(|err| fail("invalid_archive", format!("archive entry: {err}")))?;
        count += 1;
        if count > ENTRY_MAX {
            return Err(fail("invalid_archive", "archive has more than 256 entries"));
        }
        let kind = entry.header().entry_type();
        let directory = match kind {
            EntryType::Regular | EntryType::Continuous => false,
            EntryType::Directory => true,
            EntryType::Symlink | EntryType::Link => {
                return Err(fail(
                    "invalid_archive",
                    "archive contains a symbolic link or hard link",
                ));
            }
            EntryType::Block | EntryType::Char | EntryType::Fifo => {
                return Err(fail(
                    "invalid_archive",
                    "archive contains a device or fifo entry",
                ));
            }
            _ => {
                return Err(fail(
                    "invalid_archive",
                    "archive contains an unsupported entry",
                ));
            }
        };
        let raw = entry
            .path()
            .map_err(|err| fail("invalid_archive", format!("archive path: {err}")))?;
        let path = normalize_rel(&raw)?;
        if directory {
            continue;
        }
        if path.ends_with(".cwasm") {
            return Err(fail(
                "invalid_archive",
                "archive contains a precompiled cwasm entry",
            ));
        }
        let declared = entry
            .header()
            .size()
            .map_err(|err| fail("invalid_archive", format!("entry size: {err}")))?;
        if path == "module.wasm" && declared > WASM_MAX {
            return Err(fail("invalid_archive", "module.wasm exceeds 12 MiB"));
        }
        if declared > EXTRACT_MAX || total.saturating_add(declared) > EXTRACT_MAX {
            return Err(fail("invalid_archive", "extracted archive exceeds 32 MiB"));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|err| fail("invalid_archive", format!("read entry: {err}")))?;
        if path == "module.wasm" && data.len() as u64 > WASM_MAX {
            return Err(fail("invalid_archive", "module.wasm exceeds 12 MiB"));
        }
        total += data.len() as u64;
        if total > EXTRACT_MAX {
            return Err(fail("invalid_archive", "extracted archive exceeds 32 MiB"));
        }
        if files.insert(path.clone(), data).is_some() {
            return Err(fail(
                "invalid_archive",
                format!("duplicate archive entry: {path}"),
            ));
        }
    }
    Ok(files)
}

fn verify_file_hashes(
    files: &BTreeMap<String, Vec<u8>>,
    declared: &BTreeMap<String, String>,
) -> Result<(), InstallError> {
    let mut remaining: Vec<&str> = files
        .keys()
        .filter(|path| path.as_str() != "skill.toml")
        .map(String::as_str)
        .collect();
    for (path, hash) in declared {
        if !is_lower_hex64(hash) {
            return Err(fail(
                "invalid_manifest",
                format!("file hash must be lowercase sha256 hex: {path}"),
            ));
        }
        let Some(data) = files.get(path) else {
            return Err(fail(
                "invalid_archive",
                format!("[files] entry is not in the archive: {path}"),
            ));
        };
        if sha256_hex(data) != *hash {
            return Err(fail(
                "invalid_archive",
                format!("file hash mismatch: {path}"),
            ));
        }
        remaining.retain(|item| *item != path);
    }
    if !remaining.is_empty() {
        return Err(fail(
            "invalid_archive",
            format!(
                "archive entries missing from [files]: {}",
                remaining.join(", ")
            ),
        ));
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct ManifestToml {
    schema_version: i64,
    name: String,
    version: String,
    #[serde(default)]
    platforms: Vec<String>,
    runtime: RuntimeToml,
    files: BTreeMap<String, String>,
    #[serde(default)]
    capabilities: Vec<CapabilityToml>,
}

#[derive(Debug, serde::Deserialize)]
struct RuntimeToml {
    kind: String,
    abi: String,
    #[serde(default)]
    limits: LimitsToml,
}

#[derive(Debug, Default, serde::Deserialize)]
struct LimitsToml {
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    memory_mb: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
struct CapabilityToml {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    grant: Option<String>,
}

pub fn manifest_from_str(text: &str) -> Result<Manifest, InstallError> {
    parse_manifest(text)
}

fn parse_manifest(text: &str) -> Result<Manifest, InstallError> {
    let raw: ManifestToml = toml::from_str(text)
        .map_err(|err| fail("invalid_manifest", format!("skill.toml: {err}")))?;
    if raw.schema_version != 1 {
        return Err(fail("invalid_manifest", "schema_version must be 1"));
    }
    if raw.runtime.kind != "wasm" {
        return Err(fail("invalid_manifest", "runtime.kind must be wasm"));
    }
    if raw.name.trim().is_empty() || raw.version.trim().is_empty() {
        return Err(fail("invalid_manifest", "name and version are required"));
    }
    let mut files = BTreeMap::new();
    for (path, hash) in raw.files {
        let normalized = normalize_rel(Path::new(&path))?;
        if files.insert(normalized, hash).is_some() {
            return Err(fail(
                "invalid_manifest",
                format!("duplicate [files] entry: {path}"),
            ));
        }
    }
    let mut grants = Vec::with_capacity(raw.capabilities.len());
    for cap in raw.capabilities {
        grants.push(grant_string(cap)?);
    }
    Ok(Manifest {
        name: raw.name,
        version: raw.version,
        abi: raw.runtime.abi,
        platforms: raw.platforms,
        timeout_ms: raw.runtime.limits.timeout_ms,
        memory_mb: raw.runtime.limits.memory_mb,
        files,
        grants,
    })
}

fn grant_string(cap: CapabilityToml) -> Result<String, InstallError> {
    if let Some(grant) = cap.grant.filter(|value| !value.is_empty()) {
        return Ok(grant);
    }
    let Some(name) = cap.name.filter(|value| !value.is_empty()) else {
        return Err(fail("invalid_manifest", "capability is missing a name"));
    };
    match cap.scope.filter(|value| !value.is_empty()) {
        Some(scope) => Ok(format!("{name}:{scope}")),
        None => Ok(name),
    }
}

fn normalize_rel(path: &Path) -> Result<String, InstallError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(fail("invalid_archive", "archive path is absolute or empty"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| fail("invalid_archive", "archive path is not utf-8"))?;
                if part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains('\\')
                    || part.contains('\0')
                {
                    return Err(fail("invalid_archive", "archive path is invalid"));
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(fail("invalid_archive", "archive path contains '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(fail("invalid_archive", "archive path is absolute"));
            }
        }
    }
    if parts.is_empty() {
        return Err(fail("invalid_archive", "archive path is empty"));
    }
    Ok(parts.join("/"))
}

fn is_lower_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn fail(reason: &'static str, detail: impl Into<String>) -> InstallError {
    InstallError {
        reason,
        detail: detail.into(),
    }
}

#[derive(Debug, Clone)]
pub struct PackSpec {
    pub name: String,
    pub version: String,
    pub wasm: Vec<u8>,
    pub grants: Vec<(String, Option<String>)>,
    pub platforms: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub memory_mb: Option<u64>,
}

/// Deterministic gzip tar used by tests and the D10.7 fixtures.
pub fn pack_skill(spec: &PackSpec) -> Vec<u8> {
    let params = br#"{"type":"object"}"#;
    let wasm_hash = sha256_hex(&spec.wasm);
    let params_hash = sha256_hex(params);
    let mut capabilities = String::new();
    for (name, scope) in &spec.grants {
        capabilities.push_str("[[capabilities]]\n");
        capabilities.push_str(&format!("name = {}\n", toml_string(name)));
        if let Some(scope) = scope {
            capabilities.push_str(&format!("scope = {}\n", toml_string(scope)));
        }
        capabilities.push('\n');
    }
    let mut platforms = String::new();
    if !spec.platforms.is_empty() {
        let items: Vec<String> = spec.platforms.iter().map(|p| toml_string(p)).collect();
        platforms = format!("platforms = [{}]\n", items.join(", "));
    }
    let mut limits = String::new();
    if spec.timeout_ms.is_some() || spec.memory_mb.is_some() {
        limits.push_str("\n[runtime.limits]\n");
        if let Some(timeout) = spec.timeout_ms {
            limits.push_str(&format!("timeout_ms = {timeout}\n"));
        }
        if let Some(memory) = spec.memory_mb {
            limits.push_str(&format!("memory_mb = {memory}\n"));
        }
    }
    let manifest = format!(
        "schema_version = 1\nname = {name}\nversion = {version}\n{platforms}\n[runtime]\nkind = \"wasm\"\nabi = \"novbot:skill@1\"\n{limits}\n[files]\n\"module.wasm\" = {wasm_hash}\n\"schema/params.json\" = {params_hash}\n\n{capabilities}",
        name = toml_string(&spec.name),
        version = toml_string(&spec.version),
        wasm_hash = toml_string(&wasm_hash),
        params_hash = toml_string(&params_hash),
    );
    let mut entries = vec![
        ("skill.toml".to_string(), manifest.into_bytes()),
        ("module.wasm".to_string(), spec.wasm.clone()),
        ("schema/params.json".to_string(), params.to_vec()),
    ];
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut tar_bytes = Vec::new();
    {
        let mut builder = Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "schema", std::io::empty())
            .expect("schema dir");
        for (path, data) in &entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(data.len() as u64);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder
                .append_data(&mut header, path, data.as_slice())
                .expect("tar entry");
        }
        builder.finish().expect("tar finish");
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("gzip finish")
}

fn toml_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::CATALOG;

    #[test]
    fn observability_names_are_in_the_node_catalog() {
        assert!(CATALOG.contains(&"sys.metrics.read"));
        assert!(CATALOG.contains(&"proc.list.read"));
        assert!(CATALOG.contains(&"net.interfaces.read"));
        assert!(!CATALOG.contains(&"proc.cmdline.read"));
        assert!(!CATALOG.contains(&"net.connections.read"));
    }
}
