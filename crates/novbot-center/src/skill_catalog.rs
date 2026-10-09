// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Skills Hub catalog: validate an `.nbskill` package and store one immutable version.
//!
//! Built-in skills stay in `novbot_core::list_skills()` and are not inserted here.
//! `name@version` is immutable: the same bytes register once (HTTP 200 on retry);
//! a different payload for that pair is rejected and does not overwrite the blob.

use crate::artifact::{put_artifact, sha256_hex, ArtifactStore};
use crate::db::{Db, PACKAGE_MAX_BYTES};
use flate2::read::GzDecoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{MySqlConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read};
use std::ops::DerefMut;
use std::path::{Component, Path};

const CONTENT_TYPE: &str = "application/vnd.novbot.skill";
const ABI: &str = "novbot:skill@1";
const SIGNATURE_NONE: &str = "none";
const EXTRACT_MAX: u64 = 32 * 1024 * 1024;
const WASM_MAX: u64 = 12 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const RESERVED_NAMES: &[&str] = &["host_info", "echo", "env_get"];

#[derive(Debug)]
pub(crate) enum CatalogError {
    UploadsDisabled { max_allowed_packet: u64 },
    PackageTooLarge,
    InvalidArchive(String),
    InvalidManifest(String),
    InvalidWasm(String),
    HashMismatch,
    NameReserved,
    VersionExists,
    CapabilityUnsupported(String),
    CapabilityInvalid(String),
    SkillUnknown,
    VersionUnknown,
    Internal(anyhow::Error),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UploadsDisabled { max_allowed_packet } => {
                write!(
                    f,
                    "uploads disabled (max_allowed_packet={max_allowed_packet})"
                )
            }
            Self::PackageTooLarge => write!(f, "package too large"),
            Self::InvalidArchive(msg)
            | Self::InvalidManifest(msg)
            | Self::InvalidWasm(msg)
            | Self::CapabilityUnsupported(msg)
            | Self::CapabilityInvalid(msg) => write!(f, "{msg}"),
            Self::HashMismatch => write!(f, "hash mismatch"),
            Self::NameReserved => write!(f, "name reserved"),
            Self::VersionExists => write!(f, "version exists"),
            Self::SkillUnknown => write!(f, "skill unknown"),
            Self::VersionUnknown => write!(f, "version unknown"),
            Self::Internal(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CatalogError {}

fn invalid_archive(msg: impl Into<String>) -> CatalogError {
    CatalogError::InvalidArchive(msg.into())
}

fn internal(err: impl Into<anyhow::Error>) -> CatalogError {
    CatalogError::Internal(err.into())
}

fn db_err(err: sqlx::Error, ctx: &'static str) -> CatalogError {
    CatalogError::Internal(anyhow::Error::from(err).context(ctx))
}

pub(crate) fn package_exceeds_limit(len: usize) -> bool {
    len as u64 > PACKAGE_MAX_BYTES
}

fn is_skill_content_type(value: &str) -> bool {
    value
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case(CONTENT_TYPE)
}

fn sha_header_matches(actual: &str, header: &str) -> bool {
    let header = header.trim();
    header.len() == actual.len() && header.eq_ignore_ascii_case(actual)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CapabilityView {
    pub grant: String,
    pub name: String,
    pub scope: Option<String>,
    pub risk: String,
    pub description: String,
    pub reason: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct PublishBody {
    name: String,
    version: String,
    sha256: String,
    capabilities: Vec<CapabilityView>,
    capabilities_sha256: String,
    signature_status: String,
    lint_warnings: Vec<String>,
}

pub(crate) struct RegisterOutcome {
    pub created: bool,
    pub body: PublishBody,
}

#[derive(Serialize)]
pub(crate) struct HubSkillItem {
    name: String,
    source: String,
    display_name: String,
    description: String,
    latest_version: String,
    sha256: String,
    capabilities: Vec<CapabilityView>,
}

#[derive(Serialize)]
pub(crate) struct SkillDetail {
    name: String,
    source: String,
    display_name: String,
    description: String,
    publisher: Option<String>,
    latest_version: Option<String>,
    sha256: Option<String>,
    capabilities: Vec<CapabilityView>,
    versions: Vec<VersionSummary>,
}

#[derive(Serialize)]
pub(crate) struct VersionSummary {
    version: String,
    sha256: String,
    signature_status: String,
    status: String,
}

#[derive(Serialize)]
pub(crate) struct VersionDetail {
    name: String,
    version: String,
    sha256: String,
    size_bytes: i64,
    content_type: String,
    abi: String,
    signature_status: String,
    status: String,
    capabilities: Vec<CapabilityView>,
    capabilities_sha256: String,
    lint_warnings: Vec<String>,
    display_name: String,
    description: String,
    publisher: Option<String>,
    min_node_version: Option<String>,
    platforms: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestToml {
    schema_version: i64,
    name: String,
    version: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    min_node_version: Option<String>,
    runtime: RuntimeToml,
    files: BTreeMap<String, String>,
    #[serde(default)]
    capabilities: Vec<CapabilityToml>,
}

#[derive(Debug, Deserialize)]
struct RuntimeToml {
    kind: String,
    abi: String,
}

#[derive(Debug, Deserialize)]
struct CapabilityToml {
    name: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ComplianceDoc {
    #[serde(default)]
    lint_warnings: Vec<String>,
}

#[derive(Debug)]
struct ParsedPackage {
    name: String,
    version: String,
    semver_major: i32,
    semver_minor: i32,
    semver_patch: i32,
    prerelease: Option<String>,
    display_name: String,
    description: String,
    publisher: Option<String>,
    platforms_json: String,
    min_node_version: Option<String>,
    manifest_toml: String,
    manifest_json: String,
    params_schema_json: String,
    output_schema_json: Option<String>,
    capabilities: Vec<CapabilityView>,
    capabilities_json: String,
    capabilities_sha256: String,
    compliance_json: String,
    lint_warnings: Vec<String>,
    signature_status: String,
    sha256: String,
    size_bytes: i64,
}

enum CapKind {
    FsPath,
    Env,
    Bare,
}

struct CapSpec {
    kind: CapKind,
    risk: &'static str,
    bare: &'static str,
}

pub(crate) async fn register_package(
    db: &Db,
    content_type: Option<&str>,
    sha256_header: Option<&str>,
    body: &[u8],
) -> Result<RegisterOutcome, CatalogError> {
    if !db.uploads_enabled() {
        return Err(CatalogError::UploadsDisabled {
            max_allowed_packet: db.max_allowed_packet(),
        });
    }
    // Size is checked before the archive is parsed so a non-archive body still
    // reports package_too_large.
    if package_exceeds_limit(body.len()) {
        return Err(CatalogError::PackageTooLarge);
    }
    match content_type {
        Some(value) if is_skill_content_type(value) => {}
        _ => {
            return Err(invalid_archive(
                "Content-Type must be application/vnd.novbot.skill",
            ));
        }
    }
    let actual = sha256_hex(body);
    if let Some(header) = sha256_header {
        if !sha_header_matches(&actual, header) {
            return Err(CatalogError::HashMismatch);
        }
    }
    let parsed = parse_package(body)?;
    persist(db, body, &parsed).await
}

pub(crate) async fn list_hub_items(db: &Db) -> Result<Vec<HubSkillItem>, CatalogError> {
    let rows = sqlx::query(
        r#"
        SELECT s.name, s.display_name, s.description, s.source,
               v.version, v.sha256, v.capabilities_json
        FROM skills s
        INNER JOIN skill_versions v ON v.skill_id = s.id
        WHERE s.source = 'hub'
        ORDER BY s.name ASC, v.id ASC
        "#,
    )
    .fetch_all(db.pool())
    .await
    .map_err(|err| db_err(err, "list hub skills"))?;

    struct Acc {
        display_name: String,
        description: String,
        source: String,
        latest: Version,
        latest_version: String,
        sha256: String,
        capabilities: Vec<CapabilityView>,
    }

    let mut grouped: BTreeMap<String, Acc> = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get("name").map_err(internal)?;
        let version_str: String = row.try_get("version").map_err(internal)?;
        let version = Version::parse(&version_str).map_err(internal)?;
        let sha256 = trim_hex(row.try_get("sha256").map_err(internal)?);
        let caps_raw: String = row.try_get("capabilities_json").map_err(internal)?;
        let capabilities = parse_capabilities(&caps_raw)?;
        let display_name: String = row.try_get("display_name").map_err(internal)?;
        let description: Option<String> = row.try_get("description").map_err(internal)?;
        let source: String = row.try_get("source").map_err(internal)?;
        match grouped.get_mut(&name) {
            Some(acc) if version > acc.latest => {
                acc.latest = version;
                acc.latest_version = version_str;
                acc.sha256 = sha256;
                acc.capabilities = capabilities;
                acc.display_name = display_name;
                acc.description = description.unwrap_or_default();
                acc.source = source;
            }
            Some(_) => {}
            None => {
                grouped.insert(
                    name,
                    Acc {
                        display_name,
                        description: description.unwrap_or_default(),
                        source,
                        latest: version,
                        latest_version: version_str,
                        sha256,
                        capabilities,
                    },
                );
            }
        }
    }

    Ok(grouped
        .into_iter()
        .map(|(name, acc)| HubSkillItem {
            name,
            source: acc.source,
            display_name: acc.display_name,
            description: acc.description,
            latest_version: acc.latest_version,
            sha256: acc.sha256,
            capabilities: acc.capabilities,
        })
        .collect())
}

pub(crate) fn is_builtin_name(name: &str) -> bool {
    novbot_core::list_skills().contains(&name)
}

pub(crate) fn builtin_list_items() -> Vec<HubSkillItem> {
    novbot_core::list_skills()
        .into_iter()
        .map(|name| HubSkillItem {
            name: name.to_string(),
            source: "builtin".to_string(),
            display_name: name.to_string(),
            description: String::new(),
            latest_version: String::new(),
            sha256: String::new(),
            capabilities: Vec::new(),
        })
        .collect()
}

pub(crate) fn builtin_detail(name: &str) -> Option<SkillDetail> {
    if !is_builtin_name(name) {
        return None;
    }
    Some(SkillDetail {
        name: name.to_string(),
        source: "builtin".to_string(),
        display_name: name.to_string(),
        description: String::new(),
        publisher: None,
        latest_version: None,
        sha256: None,
        capabilities: Vec::new(),
        versions: Vec::new(),
    })
}

pub(crate) async fn get_skill(db: &Db, name: &str) -> Result<SkillDetail, CatalogError> {
    let skill = load_skill(db, name).await?;
    let versions = load_versions(db, skill.id).await?;
    let latest = versions.iter().max_by(|a, b| a.semver.cmp(&b.semver));
    let mut summaries: Vec<VersionSummary> = versions
        .iter()
        .map(|version| VersionSummary {
            version: version.version.clone(),
            sha256: version.sha256.clone(),
            signature_status: version.signature_status.clone(),
            status: version.status.clone(),
        })
        .collect();
    summaries.sort_by(|a, b| {
        let left = Version::parse(&a.version).ok();
        let right = Version::parse(&b.version).ok();
        right.cmp(&left)
    });
    Ok(SkillDetail {
        name: skill.name,
        source: skill.source,
        display_name: skill.display_name,
        description: skill.description,
        publisher: skill.publisher,
        latest_version: latest.map(|version| version.version.clone()),
        sha256: latest.map(|version| version.sha256.clone()),
        capabilities: latest
            .map(|version| version.capabilities.clone())
            .unwrap_or_default(),
        versions: summaries,
    })
}

pub(crate) async fn get_version(
    db: &Db,
    name: &str,
    version: &str,
) -> Result<VersionDetail, CatalogError> {
    let skill = load_skill(db, name).await?;
    let versions = load_versions(db, skill.id).await?;
    let found = versions
        .into_iter()
        .find(|row| row.version == version)
        .ok_or(CatalogError::VersionUnknown)?;
    Ok(VersionDetail {
        name: skill.name,
        version: found.version,
        sha256: found.sha256,
        size_bytes: found.size_bytes,
        content_type: found.content_type,
        abi: found.abi,
        signature_status: found.signature_status,
        status: found.status,
        capabilities: found.capabilities,
        capabilities_sha256: found.capabilities_sha256,
        lint_warnings: found.lint_warnings,
        display_name: skill.display_name,
        description: skill.description,
        publisher: skill.publisher,
        min_node_version: found.min_node_version,
        platforms: found.platforms,
    })
}

pub(crate) async fn read_package(
    db: &Db,
    name: &str,
    version: &str,
) -> Result<Vec<u8>, CatalogError> {
    let detail = get_version(db, name, version).await?;
    ArtifactStore::get(db, &detail.sha256)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal(anyhow::anyhow!("artifact {} is missing", detail.sha256)))
}

fn parse_package(body: &[u8]) -> Result<ParsedPackage, CatalogError> {
    let files = unpack(body)?;
    let wasm = files
        .get("module.wasm")
        .ok_or_else(|| CatalogError::InvalidWasm("module.wasm is missing".into()))?;
    validate_wasm(wasm)?;
    let manifest_toml = std::str::from_utf8(
        files
            .get("skill.toml")
            .ok_or_else(|| invalid_archive("skill.toml is required"))?,
    )
    .map_err(|_| invalid_archive("skill.toml is not utf-8"))?
    .to_string();
    let params_schema_json = require_json_text(&files, "schema/params.json")?.to_string();
    let output_schema_json = optional_json_text(&files, "schema/output.json")?;

    let manifest: ManifestToml = toml::from_str(&manifest_toml)
        .map_err(|err| CatalogError::InvalidManifest(format!("skill.toml: {err}")))?;
    if manifest.schema_version != 1 {
        return Err(invalid_archive("schema_version must be 1"));
    }
    if RESERVED_NAMES.contains(&manifest.name.as_str()) {
        return Err(CatalogError::NameReserved);
    }
    if !valid_skill_name(&manifest.name) {
        return Err(invalid_archive(format!(
            "skill name is invalid: {}",
            manifest.name
        )));
    }
    let semver = Version::parse(&manifest.version)
        .map_err(|err| invalid_archive(format!("version is not semver: {err}")))?;
    let version = semver.to_string();
    if version.len() > 128 {
        return Err(invalid_archive("version is too long"));
    }
    let semver_major =
        i32::try_from(semver.major).map_err(|_| invalid_archive("semver major is out of range"))?;
    let semver_minor =
        i32::try_from(semver.minor).map_err(|_| invalid_archive("semver minor is out of range"))?;
    let semver_patch =
        i32::try_from(semver.patch).map_err(|_| invalid_archive("semver patch is out of range"))?;
    let prerelease = if semver.pre.is_empty() {
        None
    } else {
        let pre = semver.pre.to_string();
        if pre.len() > 128 {
            return Err(invalid_archive("prerelease is too long"));
        }
        Some(pre)
    };
    if manifest.runtime.kind != "wasm" {
        return Err(invalid_archive("runtime.kind must be wasm"));
    }
    if manifest.runtime.abi != ABI {
        return Err(invalid_archive("runtime.abi must be novbot:skill@1"));
    }

    let mut declared = BTreeMap::new();
    for (path, hash) in &manifest.files {
        let normalized = normalize_rel_str(path)?;
        if declared.insert(normalized, hash.clone()).is_some() {
            return Err(invalid_archive(format!("duplicate [files] entry: {path}")));
        }
    }
    verify_files(&files, &declared)?;

    let mut capabilities = Vec::with_capacity(manifest.capabilities.len());
    for cap in &manifest.capabilities {
        capabilities.push(validate_capability(cap)?);
    }
    let lint_warnings = collect_lint_warnings(&capabilities);
    let grants: Vec<&str> = capabilities.iter().map(|cap| cap.grant.as_str()).collect();
    let grant_json = serde_json::to_string(&grants).map_err(internal)?;
    let capabilities_sha256 = sha256_hex(grant_json.as_bytes());
    let capabilities_json = serde_json::to_string(&capabilities).map_err(internal)?;

    let display_name = manifest
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(manifest.name.as_str())
        .to_string();
    if display_name.chars().count() > 255 {
        return Err(invalid_archive("display_name is too long"));
    }
    let description = manifest.description.unwrap_or_default();
    let publisher = manifest
        .publisher
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if publisher
        .as_ref()
        .is_some_and(|value| value.chars().count() > 255)
    {
        return Err(invalid_archive("publisher is too long"));
    }
    let min_node_version = manifest
        .min_node_version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if min_node_version
        .as_ref()
        .is_some_and(|value| value.len() > 64)
    {
        return Err(invalid_archive("min_node_version is too long"));
    }
    for platform in &manifest.platforms {
        if platform.is_empty() || platform.len() > 128 {
            return Err(invalid_archive("platforms entry is invalid"));
        }
    }

    let platforms_json = serde_json::to_string(&manifest.platforms).map_err(internal)?;
    let compliance_json = serde_json::to_string(&serde_json::json!({
        "lint_warnings": &lint_warnings,
    }))
    .map_err(internal)?;
    let manifest_json = serde_json::to_string(&serde_json::json!({
        "schema_version": 1,
        "name": &manifest.name,
        "version": &version,
        "display_name": &display_name,
        "description": &description,
        "publisher": &publisher,
        "runtime": {"kind": "wasm", "abi": ABI},
        "files": &declared,
        "capabilities": &capabilities,
        "platforms": &manifest.platforms,
        "min_node_version": &min_node_version,
    }))
    .map_err(internal)?;

    let sha256 = sha256_hex(body);
    let size_bytes =
        i64::try_from(body.len()).map_err(|_| invalid_archive("package is too large"))?;

    Ok(ParsedPackage {
        name: manifest.name,
        version,
        semver_major,
        semver_minor,
        semver_patch,
        prerelease,
        display_name,
        description,
        publisher,
        platforms_json,
        min_node_version,
        manifest_toml,
        manifest_json,
        params_schema_json,
        output_schema_json,
        capabilities,
        capabilities_json,
        capabilities_sha256,
        compliance_json,
        lint_warnings,
        signature_status: SIGNATURE_NONE.to_string(),
        sha256,
        size_bytes,
    })
}

fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, CatalogError> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|err| invalid_archive(format!("archive: {err}")))?;
    let mut files = BTreeMap::new();
    let mut count = 0usize;
    let mut total = 0u64;
    for entry in entries {
        let mut entry = entry.map_err(|err| invalid_archive(format!("archive entry: {err}")))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(invalid_archive("archive has more than 256 entries"));
        }
        let kind = entry.header().entry_type();
        let directory = match kind {
            tar::EntryType::Regular => false,
            tar::EntryType::Directory => true,
            tar::EntryType::Symlink | tar::EntryType::Link => {
                return Err(invalid_archive(
                    "archive contains a symbolic link or hard link",
                ));
            }
            tar::EntryType::Block | tar::EntryType::Char | tar::EntryType::Fifo => {
                return Err(invalid_archive("archive contains a device or fifo entry"));
            }
            _ => {
                return Err(invalid_archive("archive contains an unsupported entry"));
            }
        };
        let raw = entry
            .path()
            .map_err(|err| invalid_archive(format!("archive path: {err}")))?;
        let path = normalize_rel_path(&raw)?;
        if directory {
            if path != "schema" {
                return Err(invalid_archive(format!("unexpected directory: {path}")));
            }
            continue;
        }
        if !is_allowed_file(&path) {
            return Err(invalid_archive(format!("unexpected archive entry: {path}")));
        }
        let declared = entry
            .header()
            .size()
            .map_err(|err| invalid_archive(format!("entry size: {err}")))?;
        if path == "module.wasm" && declared > WASM_MAX {
            return Err(invalid_archive("module.wasm exceeds 12 MiB"));
        }
        if declared > EXTRACT_MAX || total.saturating_add(declared) > EXTRACT_MAX {
            return Err(invalid_archive("extracted archive exceeds 32 MiB"));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|err| invalid_archive(format!("read entry: {err}")))?;
        if path == "module.wasm" && data.len() as u64 > WASM_MAX {
            return Err(invalid_archive("module.wasm exceeds 12 MiB"));
        }
        total += data.len() as u64;
        if total > EXTRACT_MAX {
            return Err(invalid_archive("extracted archive exceeds 32 MiB"));
        }
        if files.insert(path.clone(), data).is_some() {
            return Err(invalid_archive(format!("duplicate archive entry: {path}")));
        }
    }
    Ok(files)
}

fn is_allowed_file(path: &str) -> bool {
    matches!(
        path,
        "skill.toml"
            | "module.wasm"
            | "schema/params.json"
            | "schema/output.json"
            | "README.md"
            | "LICENSE"
    )
}

fn validate_wasm(bytes: &[u8]) -> Result<(), CatalogError> {
    if bytes.is_empty() {
        return Err(CatalogError::InvalidWasm("module.wasm is empty".into()));
    }
    if bytes.len() < 4 || &bytes[..4] != b"\0asm" {
        return Err(CatalogError::InvalidWasm(
            "module.wasm does not start with wasm magic".into(),
        ));
    }
    Ok(())
}

fn require_json_text<'a>(
    files: &'a BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<&'a str, CatalogError> {
    let bytes = files
        .get(path)
        .ok_or_else(|| invalid_archive(format!("{path} is required")))?;
    json_text(path, bytes)
}

fn optional_json_text(
    files: &BTreeMap<String, Vec<u8>>,
    path: &str,
) -> Result<Option<String>, CatalogError> {
    let Some(bytes) = files.get(path) else {
        return Ok(None);
    };
    Ok(Some(json_text(path, bytes)?.to_string()))
}

fn json_text<'a>(path: &str, bytes: &'a [u8]) -> Result<&'a str, CatalogError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| invalid_archive(format!("{path} is not utf-8")))?;
    serde_json::from_str::<Value>(text).map_err(|err| invalid_archive(format!("{path}: {err}")))?;
    Ok(text)
}

fn verify_files(
    files: &BTreeMap<String, Vec<u8>>,
    declared: &BTreeMap<String, String>,
) -> Result<(), CatalogError> {
    let mut remaining: BTreeSet<String> = files
        .keys()
        .filter(|path| path.as_str() != "skill.toml")
        .cloned()
        .collect();
    for (path, hash) in declared {
        if !is_lower_hex64(hash) {
            return Err(invalid_archive(format!(
                "file hash must be lowercase sha256 hex: {path}"
            )));
        }
        let Some(data) = files.get(path) else {
            return Err(invalid_archive(format!(
                "[files] entry is not in the archive: {path}"
            )));
        };
        if sha256_hex(data) != *hash {
            return Err(invalid_archive(format!("file hash mismatch: {path}")));
        }
        remaining.remove(path);
    }
    if !remaining.is_empty() {
        let names = remaining.into_iter().collect::<Vec<_>>().join(", ");
        return Err(invalid_archive(format!(
            "archive entries missing from [files]: {names}"
        )));
    }
    Ok(())
}

fn is_lower_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn normalize_rel_str(path: &str) -> Result<String, CatalogError> {
    if path.contains('\0') {
        return Err(invalid_archive(format!("invalid [files] path: {path}")));
    }
    normalize_rel_path(Path::new(path))
        .map_err(|_| invalid_archive(format!("invalid [files] path: {path}")))
}

fn normalize_rel_path(path: &Path) -> Result<String, CatalogError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(invalid_archive("archive path is absolute or empty"));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part
                    .to_str()
                    .ok_or_else(|| invalid_archive("archive path is not utf-8"))?;
                if part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains('\\')
                    || part.contains('\0')
                {
                    return Err(invalid_archive("archive path is invalid"));
                }
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(invalid_archive("archive path contains '..'"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_archive("archive path is absolute"));
            }
        }
    }
    if parts.is_empty() {
        return Err(invalid_archive("archive path is empty"));
    }
    Ok(parts.join("/"))
}

fn valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if !(2..=63).contains(&bytes.len()) || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_segment(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn valid_cap_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let (Some(first), Some(second)) = (parts.next(), parts.next()) else {
        return false;
    };
    if !valid_segment(first) || !valid_segment(second) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(third) => parts.next().is_none() && valid_segment(third),
    }
}

fn cap_spec(name: &str) -> Option<CapSpec> {
    Some(match name {
        "fs.read" => CapSpec {
            kind: CapKind::FsPath,
            risk: "medium",
            bare: "Read files",
        },
        "fs.stat" => CapSpec {
            kind: CapKind::FsPath,
            risk: "low",
            bare: "Stat files",
        },
        "fs.list" => CapSpec {
            kind: CapKind::FsPath,
            risk: "medium",
            bare: "List directory entries",
        },
        "env.read" => CapSpec {
            kind: CapKind::Env,
            risk: "high",
            bare: "Read environment variables",
        },
        "net.listening_ports.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "medium",
            bare: "Read listening network ports",
        },
        "sys.info.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "low",
            bare: "Read system information",
        },
        "sys.time_sync.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "low",
            bare: "Read time synchronization status",
        },
        "sys.metrics.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "low",
            bare: "Read system metrics",
        },
        "proc.list.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "medium",
            bare: "List processes",
        },
        "net.interfaces.read" => CapSpec {
            kind: CapKind::Bare,
            risk: "low",
            bare: "Read network interfaces",
        },
        _ => return None,
    })
}

fn validate_capability(cap: &CapabilityToml) -> Result<CapabilityView, CatalogError> {
    if !valid_cap_name(&cap.name) {
        return Err(CatalogError::CapabilityInvalid(format!(
            "capability name is invalid: {}",
            cap.name
        )));
    }
    let spec = cap_spec(&cap.name).ok_or_else(|| {
        CatalogError::CapabilityUnsupported(format!("capability not supported: {}", cap.name))
    })?;
    let scope = cap.scope.as_deref();
    let scope = match (&spec.kind, scope) {
        (CapKind::FsPath, Some(scope)) => {
            if !valid_fs_scope(scope) {
                return Err(CatalogError::CapabilityInvalid(format!(
                    "capability scope is not an absolute path: {scope}"
                )));
            }
            Some(scope)
        }
        (CapKind::Env, Some(scope)) => {
            if !valid_env_scope(scope) {
                return Err(CatalogError::CapabilityInvalid(format!(
                    "env.read scope must match [A-Z_][A-Z0-9_]* with an optional trailing *: {scope}"
                )));
            }
            Some(scope)
        }
        (CapKind::Bare, Some(_)) => {
            return Err(CatalogError::CapabilityInvalid(format!(
                "capability {} does not take a scope",
                cap.name
            )));
        }
        (CapKind::FsPath | CapKind::Env, None) => {
            return Err(CatalogError::CapabilityInvalid(format!(
                "capability {} requires a scope",
                cap.name
            )));
        }
        (CapKind::Bare, None) => None,
    };
    let grant = match scope {
        Some(scope) => format!("{}:{scope}", cap.name),
        None => cap.name.clone(),
    };
    let description = match (cap.name.as_str(), scope) {
        ("fs.read", Some(scope)) => format!("Read files at {scope}."),
        ("fs.stat", Some(scope)) => format!("Stat files at {scope}."),
        ("fs.list", Some(scope)) => format!("List directory entries at {scope}."),
        ("env.read", Some(scope)) => format!("Read environment variables matching {scope}."),
        _ => format!("{}.", spec.bare),
    };
    Ok(CapabilityView {
        grant,
        name: cap.name.clone(),
        scope: scope.map(str::to_string),
        risk: spec.risk.to_string(),
        description,
        reason: cap.reason.clone(),
    })
}

fn valid_fs_scope(scope: &str) -> bool {
    if scope.is_empty()
        || !scope.starts_with('/')
        || scope.contains('~')
        || scope.contains('\\')
        || scope.chars().any(char::is_control)
    {
        return false;
    }
    let parts: Vec<&str> = scope.split('/').collect();
    if !matches!(parts.first(), Some(&"")) {
        return false;
    }
    for (index, part) in parts.iter().enumerate().skip(1) {
        if *part == ".." || *part == "." {
            return false;
        }
        if part.is_empty() && index != parts.len() - 1 {
            return false;
        }
    }
    true
}

fn valid_env_scope(scope: &str) -> bool {
    let core = scope.strip_suffix('*').unwrap_or(scope);
    let mut chars = core.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_uppercase() || ch == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn sensitive_fs_scope(scope: &str) -> bool {
    let trimmed = scope.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    path == "/etc/shadow"
        || path == "/etc/gshadow"
        || path == "/etc/ssl/private"
        || path.starts_with("/etc/ssl/private/")
        || path == "/root"
        || path.starts_with("/root/")
}

fn collect_lint_warnings(capabilities: &[CapabilityView]) -> Vec<String> {
    let mut warnings = Vec::new();
    for cap in capabilities {
        if !matches!(cap.name.as_str(), "fs.read" | "fs.stat" | "fs.list") {
            continue;
        }
        let Some(scope) = cap.scope.as_deref() else {
            continue;
        };
        if sensitive_fs_scope(scope) {
            warnings.push(format!("{} scope {scope} is a sensitive path", cap.name));
        }
    }
    warnings
}

enum InsertedSkill {
    Id(i64),
    Duplicate,
}

async fn persist(
    db: &Db,
    body: &[u8],
    parsed: &ParsedPackage,
) -> Result<RegisterOutcome, CatalogError> {
    let mut tx = db.pool().begin().await.map_err(internal)?;
    let locked = lock_skill(tx.deref_mut(), &parsed.name).await?;
    let skill_id = match locked {
        Some(id) => id,
        None => {
            let inserted = insert_skill(tx.deref_mut(), parsed).await?;
            match inserted {
                InsertedSkill::Id(id) => {
                    write_version(tx.deref_mut(), id, body, parsed).await?;
                    tx.commit().await.map_err(internal)?;
                    return Ok(outcome(true, parsed));
                }
                InsertedSkill::Duplicate => lock_skill(tx.deref_mut(), &parsed.name)
                    .await?
                    .ok_or_else(|| {
                        internal(anyhow::anyhow!("skill row missing after duplicate insert"))
                    })?,
            }
        }
    };

    if let Some(existing) = lock_version_sha(tx.deref_mut(), skill_id, &parsed.version).await? {
        tx.rollback().await.map_err(internal)?;
        if existing == parsed.sha256 {
            ensure_artifact(db, &parsed.sha256, body).await?;
            return Ok(outcome(false, parsed));
        }
        return Err(CatalogError::VersionExists);
    }

    update_skill(tx.deref_mut(), skill_id, parsed).await?;
    write_version(tx.deref_mut(), skill_id, body, parsed).await?;
    tx.commit().await.map_err(internal)?;
    Ok(outcome(true, parsed))
}

fn outcome(created: bool, parsed: &ParsedPackage) -> RegisterOutcome {
    RegisterOutcome {
        created,
        body: PublishBody {
            name: parsed.name.clone(),
            version: parsed.version.clone(),
            sha256: parsed.sha256.clone(),
            capabilities: parsed.capabilities.clone(),
            capabilities_sha256: parsed.capabilities_sha256.clone(),
            signature_status: parsed.signature_status.clone(),
            lint_warnings: parsed.lint_warnings.clone(),
        },
    }
}

async fn ensure_artifact(db: &Db, sha256: &str, body: &[u8]) -> Result<(), CatalogError> {
    if ArtifactStore::exists(db, sha256).await.map_err(internal)? {
        return Ok(());
    }
    ArtifactStore::put(db, sha256, body)
        .await
        .map_err(internal)?;
    Ok(())
}

async fn write_version(
    conn: &mut MySqlConnection,
    skill_id: i64,
    body: &[u8],
    parsed: &ParsedPackage,
) -> Result<(), CatalogError> {
    put_artifact(conn, &parsed.sha256, body)
        .await
        .map_err(internal)?;
    insert_version(conn, skill_id, parsed).await
}

async fn lock_skill(conn: &mut MySqlConnection, name: &str) -> Result<Option<i64>, CatalogError> {
    let row = sqlx::query("SELECT id FROM skills WHERE name = ? FOR UPDATE")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|err| db_err(err, "lock skill"))?;
    match row {
        Some(row) => Ok(Some(row.try_get("id").map_err(internal)?)),
        None => Ok(None),
    }
}

async fn insert_skill(
    conn: &mut MySqlConnection,
    parsed: &ParsedPackage,
) -> Result<InsertedSkill, CatalogError> {
    let result = sqlx::query(
        r#"
        INSERT INTO skills (name, display_name, description, source, publisher, tags_json, created_by)
        VALUES (?, ?, ?, 'hub', ?, NULL, NULL)
        "#,
    )
    .bind(&parsed.name)
    .bind(&parsed.display_name)
    .bind(&parsed.description)
    .bind(&parsed.publisher)
    .execute(&mut *conn)
    .await;
    match result {
        Ok(done) => Ok(InsertedSkill::Id(done.last_insert_id() as i64)),
        Err(err) if crate::artifact::mysql_is_duplicate(&err) => Ok(InsertedSkill::Duplicate),
        Err(err) => Err(db_err(err, "insert skill")),
    }
}

async fn update_skill(
    conn: &mut MySqlConnection,
    skill_id: i64,
    parsed: &ParsedPackage,
) -> Result<(), CatalogError> {
    sqlx::query("UPDATE skills SET display_name = ?, description = ?, publisher = ? WHERE id = ?")
        .bind(&parsed.display_name)
        .bind(&parsed.description)
        .bind(&parsed.publisher)
        .bind(skill_id)
        .execute(&mut *conn)
        .await
        .map_err(|err| db_err(err, "update skill"))?;
    Ok(())
}

async fn lock_version_sha(
    conn: &mut MySqlConnection,
    skill_id: i64,
    version: &str,
) -> Result<Option<String>, CatalogError> {
    let row = sqlx::query(
        "SELECT sha256 FROM skill_versions WHERE skill_id = ? AND version = ? FOR UPDATE",
    )
    .bind(skill_id)
    .bind(version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|err| db_err(err, "lock skill version"))?;
    match row {
        Some(row) => Ok(Some(trim_hex(row.try_get("sha256").map_err(internal)?))),
        None => Ok(None),
    }
}

async fn insert_version(
    conn: &mut MySqlConnection,
    skill_id: i64,
    parsed: &ParsedPackage,
) -> Result<(), CatalogError> {
    sqlx::query(
        r#"
        INSERT INTO skill_versions (
            skill_id, version, semver_major, semver_minor, semver_patch, prerelease,
            sha256, size_bytes, content_type, abi, platforms_json, min_node_version,
            manifest_toml, manifest_json, params_schema_json, output_schema_json,
            capabilities_json, capabilities_sha256, compliance_json,
            signature_status, status
        ) VALUES (
            ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?,
            'none', 'published'
        )
        "#,
    )
    .bind(skill_id)
    .bind(&parsed.version)
    .bind(parsed.semver_major)
    .bind(parsed.semver_minor)
    .bind(parsed.semver_patch)
    .bind(&parsed.prerelease)
    .bind(&parsed.sha256)
    .bind(parsed.size_bytes)
    .bind(CONTENT_TYPE)
    .bind(ABI)
    .bind(&parsed.platforms_json)
    .bind(&parsed.min_node_version)
    .bind(&parsed.manifest_toml)
    .bind(&parsed.manifest_json)
    .bind(&parsed.params_schema_json)
    .bind(&parsed.output_schema_json)
    .bind(&parsed.capabilities_json)
    .bind(&parsed.capabilities_sha256)
    .bind(&parsed.compliance_json)
    .execute(&mut *conn)
    .await
    .map_err(|err| db_err(err, "insert skill version"))?;
    Ok(())
}

struct SkillHead {
    id: i64,
    name: String,
    display_name: String,
    description: String,
    source: String,
    publisher: Option<String>,
}

struct StoredVersion {
    version: String,
    semver: Version,
    sha256: String,
    size_bytes: i64,
    content_type: String,
    abi: String,
    platforms: Vec<String>,
    min_node_version: Option<String>,
    capabilities: Vec<CapabilityView>,
    capabilities_sha256: String,
    lint_warnings: Vec<String>,
    signature_status: String,
    status: String,
}

async fn load_skill(db: &Db, name: &str) -> Result<SkillHead, CatalogError> {
    let row = sqlx::query(
        r#"
        SELECT id, name, display_name, description, source, publisher
        FROM skills WHERE name = ?
        "#,
    )
    .bind(name)
    .fetch_optional(db.pool())
    .await
    .map_err(|err| db_err(err, "load skill"))?;
    let Some(row) = row else {
        return Err(CatalogError::SkillUnknown);
    };
    let description: Option<String> = row.try_get("description").map_err(internal)?;
    Ok(SkillHead {
        id: row.try_get("id").map_err(internal)?,
        name: row.try_get("name").map_err(internal)?,
        display_name: row.try_get("display_name").map_err(internal)?,
        description: description.unwrap_or_default(),
        source: row.try_get("source").map_err(internal)?,
        publisher: row.try_get("publisher").map_err(internal)?,
    })
}

async fn load_versions(db: &Db, skill_id: i64) -> Result<Vec<StoredVersion>, CatalogError> {
    let rows = sqlx::query(
        r#"
        SELECT version, sha256, size_bytes, content_type, abi, platforms_json,
               min_node_version, capabilities_json, capabilities_sha256, compliance_json,
               signature_status, status
        FROM skill_versions WHERE skill_id = ?
        "#,
    )
    .bind(skill_id)
    .fetch_all(db.pool())
    .await
    .map_err(|err| db_err(err, "load skill versions"))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let version: String = row.try_get("version").map_err(internal)?;
        let semver = Version::parse(&version).map_err(internal)?;
        let platforms_raw: Option<String> = row.try_get("platforms_json").map_err(internal)?;
        let caps_raw: String = row.try_get("capabilities_json").map_err(internal)?;
        let compliance_raw: Option<String> = row.try_get("compliance_json").map_err(internal)?;
        out.push(StoredVersion {
            version,
            semver,
            sha256: trim_hex(row.try_get("sha256").map_err(internal)?),
            size_bytes: row.try_get("size_bytes").map_err(internal)?,
            content_type: row.try_get("content_type").map_err(internal)?,
            abi: row.try_get("abi").map_err(internal)?,
            platforms: parse_platforms(platforms_raw)?,
            min_node_version: row.try_get("min_node_version").map_err(internal)?,
            capabilities: parse_capabilities(&caps_raw)?,
            capabilities_sha256: trim_hex(row.try_get("capabilities_sha256").map_err(internal)?),
            lint_warnings: parse_lint(compliance_raw)?,
            signature_status: row.try_get("signature_status").map_err(internal)?,
            status: row.try_get("status").map_err(internal)?,
        });
    }
    Ok(out)
}

fn parse_capabilities(raw: &str) -> Result<Vec<CapabilityView>, CatalogError> {
    serde_json::from_str(raw).map_err(internal)
}

fn parse_platforms(raw: Option<String>) -> Result<Vec<String>, CatalogError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    serde_json::from_str(&raw).map_err(internal)
}

fn parse_lint(raw: Option<String>) -> Result<Vec<String>, CatalogError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(Vec::new());
    };
    let doc: ComplianceDoc = serde_json::from_str(&raw).map_err(internal)?;
    Ok(doc.lint_warnings)
}

fn trim_hex(value: String) -> String {
    value.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        is_skill_content_type, package_exceeds_limit, parse_package, sensitive_fs_scope,
        sha_header_matches, valid_env_scope, valid_fs_scope, valid_skill_name, CatalogError,
    };
    use crate::artifact::sha256_hex;
    use crate::db::{connect_test_db, Db, PACKAGE_MAX_BYTES};
    use crate::http::{router, AppState};
    use crate::hub::Hub;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use sqlx::Row;
    use std::io::Write;
    use tower::ServiceExt;

    const RESP_LIMIT: usize = 8 * 1024 * 1024;
    const PARAMS_JSON: &[u8] = br#"{"type":"object"}"#;

    struct GrantSpec {
        name: &'static str,
        scope: Option<&'static str>,
        reason: Option<&'static str>,
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

    fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).expect("gzip");
        encoder.finish().expect("gzip finish")
    }

    fn build_nbskill(name: &str, version: &str, wasm: &[u8], grants: &[GrantSpec]) -> Vec<u8> {
        let params_hash = sha256_hex(PARAMS_JSON);
        let wasm_hash = sha256_hex(wasm);
        let mut capabilities = String::new();
        for grant in grants {
            capabilities.push_str("[[capabilities]]\n");
            capabilities.push_str(&format!("name = {}\n", toml_string(grant.name)));
            if let Some(scope) = grant.scope {
                capabilities.push_str(&format!("scope = {}\n", toml_string(scope)));
            }
            if let Some(reason) = grant.reason {
                capabilities.push_str(&format!("reason = {}\n", toml_string(reason)));
            }
            capabilities.push('\n');
        }
        let manifest = format!(
            "schema_version = 1\nname = {name}\nversion = {version}\ndisplay_name = {display}\ndescription = {desc}\n\n[runtime]\nkind = \"wasm\"\nabi = \"novbot:skill@1\"\n\n[files]\n\"module.wasm\" = {wasm_hash}\n\"schema/params.json\" = {params_hash}\n\n{capabilities}",
            name = toml_string(name),
            version = toml_string(version),
            display = toml_string(name),
            desc = toml_string("test skill"),
            wasm_hash = toml_string(&wasm_hash),
            params_hash = toml_string(&params_hash),
        );
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            append_file(&mut builder, "skill.toml", manifest.as_bytes());
            append_file(&mut builder, "module.wasm", wasm);
            append_file(&mut builder, "schema/params.json", PARAMS_JSON);
            builder.finish().expect("tar finish");
        }
        gzip_bytes(&tar_bytes)
    }

    fn append_file(builder: &mut tar::Builder<&mut Vec<u8>>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, data)
            .expect("tar append");
    }

    fn unique_skill_name() -> String {
        format!("t{}", uuid::Uuid::new_v4())
    }

    async fn oneshot(db: &Db, request: Request<Body>) -> (StatusCode, axum::body::Bytes) {
        let app = router(AppState {
            db: db.clone(),
            hub: Hub::new(),
            api_token: None,
            license_env: None,
        });
        let response = app.oneshot(request).await.expect("router response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), RESP_LIMIT)
            .await
            .expect("response body");
        (status, bytes)
    }

    fn json_body(bytes: &[u8]) -> Value {
        match serde_json::from_slice(bytes) {
            Ok(value) => value,
            Err(err) => panic!("json ({err}): {}", String::from_utf8_lossy(bytes)),
        }
    }

    fn post_package(bytes: Vec<u8>, sha_header: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/v1/skills").header(
            axum::http::header::CONTENT_TYPE,
            "application/vnd.novbot.skill",
        );
        if let Some(sha) = sha_header {
            builder = builder.header("x-novbot-sha256", sha);
        }
        builder.body(Body::from(bytes)).expect("request")
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    async fn count_i64(db: &Db, sql: &str, bind: Option<&str>) -> i64 {
        let mut query = sqlx::query(sql);
        if let Some(value) = bind {
            query = query.bind(value);
        }
        let row = query.fetch_one(db.pool()).await.expect("count");
        row.try_get("n").expect("n")
    }

    fn os_release_grants() -> Vec<GrantSpec> {
        vec![
            GrantSpec {
                name: "fs.read",
                scope: Some("/etc/os-release"),
                reason: Some("Read the OS release file"),
            },
            GrantSpec {
                name: "sys.info.read",
                scope: None,
                reason: Some("Read system information"),
            },
        ]
    }

    #[test]
    fn package_limit_rejects_only_past_16_mib() {
        assert!(!package_exceeds_limit(16 * 1024 * 1024));
        assert!(package_exceeds_limit(16 * 1024 * 1024 + 1));
    }

    #[test]
    fn content_type_ignores_parameters_and_case() {
        assert!(is_skill_content_type("application/vnd.novbot.skill"));
        assert!(is_skill_content_type(
            "Application/vnd.novbot.skill; charset=binary"
        ));
        assert!(!is_skill_content_type("application/gzip"));
        assert!(!is_skill_content_type("application/vnd.novbot.skill-extra"));
    }

    #[test]
    fn sha_header_comparison_trims_and_ignores_case() {
        assert!(sha_header_matches("abc", "ABC"));
        assert!(sha_header_matches("abc", " abc "));
        assert!(!sha_header_matches("abc", "abd"));
    }

    #[test]
    fn skill_name_grammar() {
        assert!(valid_skill_name("ab"));
        assert!(valid_skill_name("os-release-check"));
        assert!(!valid_skill_name("a"));
        assert!(!valid_skill_name("Host"));
        assert!(!valid_skill_name("host_info"));
        assert!(!valid_skill_name("-ab"));
        assert!(valid_skill_name(&format!("t{}", "a".repeat(62))));
        assert!(!valid_skill_name(&format!("t{}", "a".repeat(63))));
    }

    #[test]
    fn env_and_fs_scope_grammar() {
        assert!(valid_env_scope("PATH"));
        assert!(valid_env_scope("_TOKEN"));
        assert!(valid_env_scope("NOVBOT_*"));
        assert!(valid_env_scope("A*"));
        assert!(!valid_env_scope("*"));
        assert!(!valid_env_scope("novbot_*"));
        assert!(!valid_env_scope("A*B"));
        assert!(!valid_env_scope(""));
        assert!(!valid_env_scope("1PATH"));

        assert!(valid_fs_scope("/etc/os-release"));
        assert!(!valid_fs_scope("etc/os-release"));
        assert!(!valid_fs_scope("~/secrets"));
        assert!(!valid_fs_scope("/etc/../shadow"));
        assert!(!valid_fs_scope("/tmp/~"));
        assert!(!valid_fs_scope(""));
    }

    #[test]
    fn sensitive_paths_match_prefixes_not_lookalikes() {
        assert!(sensitive_fs_scope("/etc/shadow"));
        assert!(sensitive_fs_scope("/etc/shadow/"));
        assert!(sensitive_fs_scope("/etc/gshadow"));
        assert!(!sensitive_fs_scope("/etc/shadow-backup"));
        assert!(sensitive_fs_scope("/etc/ssl/private"));
        assert!(sensitive_fs_scope("/etc/ssl/private/key.pem"));
        assert!(!sensitive_fs_scope("/etc/ssl/privateer"));
        assert!(sensitive_fs_scope("/root"));
        assert!(sensitive_fs_scope("/root/"));
        assert!(sensitive_fs_scope("/root/x"));
        assert!(!sensitive_fs_scope("/rootkit"));
        assert!(!sensitive_fs_scope("/etc/os-release"));
    }

    #[test]
    fn parses_os_release_style_package() {
        let wasm = b"\0asm\x01\x00\x00\x00";
        let package = build_nbskill("os-release-check", "1.0.0", wasm, &os_release_grants());
        let parsed = parse_package(&package).expect("parse");
        assert_eq!(parsed.sha256, sha256_hex(&package));
        assert_eq!(parsed.capabilities.len(), 2);
        assert_eq!(parsed.capabilities[0].grant, "fs.read:/etc/os-release");
        assert_eq!(
            parsed.capabilities[0].scope.as_deref(),
            Some("/etc/os-release")
        );
        assert_eq!(parsed.capabilities[0].risk, "medium");
        assert_eq!(parsed.capabilities[1].grant, "sys.info.read");
        assert!(parsed.capabilities[1].scope.is_none());
        assert_eq!(parsed.capabilities[1].risk, "low");
        assert!(parsed.lint_warnings.is_empty());
        assert_eq!(parsed.signature_status, "none");
        let grants = ["fs.read:/etc/os-release", "sys.info.read"];
        let json = serde_json::to_string(&grants).unwrap();
        assert_eq!(json, "[\"fs.read:/etc/os-release\",\"sys.info.read\"]");
        assert_eq!(parsed.capabilities_sha256, sha256_hex(json.as_bytes()));
    }

    #[test]
    fn builtin_name_is_reserved_before_grammar() {
        let package = build_nbskill("host_info", "1.0.0", b"\0asm\x01\x00", &[]);
        assert!(matches!(
            parse_package(&package).unwrap_err(),
            CatalogError::NameReserved
        ));
        let package = build_nbskill("echo", "1.0.0", b"\0asm\x01\x00", &[]);
        assert!(matches!(
            parse_package(&package).unwrap_err(),
            CatalogError::NameReserved
        ));
    }

    #[test]
    fn wasm_without_magic_is_invalid_wasm() {
        let package = build_nbskill("texample", "1.0.0", b"not-wasm", &[]);
        assert!(matches!(
            parse_package(&package).unwrap_err(),
            CatalogError::InvalidWasm(_)
        ));
    }

    #[test]
    fn capability_rules_and_sensitive_lint() {
        let wasm = b"\0asm\x01\x00";
        let unsupported = build_nbskill(
            "texample",
            "1.0.0",
            wasm,
            &[GrantSpec {
                name: "proc.cmdline.read",
                scope: None,
                reason: None,
            }],
        );
        assert!(matches!(
            parse_package(&unsupported).unwrap_err(),
            CatalogError::CapabilityUnsupported(_)
        ));

        let missing_scope = build_nbskill(
            "texample",
            "1.0.0",
            wasm,
            &[GrantSpec {
                name: "fs.read",
                scope: None,
                reason: None,
            }],
        );
        assert!(matches!(
            parse_package(&missing_scope).unwrap_err(),
            CatalogError::CapabilityInvalid(_)
        ));

        let extra_scope = build_nbskill(
            "texample",
            "1.0.0",
            wasm,
            &[GrantSpec {
                name: "sys.info.read",
                scope: Some("/tmp"),
                reason: None,
            }],
        );
        assert!(matches!(
            parse_package(&extra_scope).unwrap_err(),
            CatalogError::CapabilityInvalid(_)
        ));

        let sensitive = build_nbskill(
            "tshadow",
            "1.0.0",
            wasm,
            &[
                GrantSpec {
                    name: "fs.read",
                    scope: Some("/etc/shadow"),
                    reason: Some("audit"),
                },
                GrantSpec {
                    name: "fs.list",
                    scope: Some("/root/secrets"),
                    reason: None,
                },
                GrantSpec {
                    name: "fs.stat",
                    scope: Some("/etc/ssl/private/key.pem"),
                    reason: None,
                },
            ],
        );
        let parsed = parse_package(&sensitive).expect("sensitive paths still publish");
        assert_eq!(parsed.lint_warnings.len(), 3);
        assert_eq!(parsed.signature_status, "none");
    }

    #[test]
    fn symlink_archive_is_invalid() {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_path("module.wasm").expect("path");
            header.set_link_name("other").expect("link");
            header.set_cksum();
            builder
                .append(&header, std::io::empty())
                .expect("append link");
            builder.finish().expect("finish");
        }
        let package = gzip_bytes(&tar_bytes);
        assert!(matches!(
            parse_package(&package).unwrap_err(),
            CatalogError::InvalidArchive(_)
        ));
    }

    #[test]
    fn unreadable_bytes_are_invalid_archive() {
        assert!(matches!(
            parse_package(b"not-an-archive").unwrap_err(),
            CatalogError::InvalidArchive(_)
        ));
    }

    #[test]
    fn non_toml_skill_toml_is_invalid_manifest() {
        let package = build_bad_toml_package();
        assert!(matches!(
            parse_package(&package).unwrap_err(),
            CatalogError::InvalidManifest(_)
        ));
    }

    fn build_bad_toml_package() -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            append_file(&mut builder, "skill.toml", b"this is not = toml :::");
            append_file(&mut builder, "module.wasm", b"\0asm\x01\x00\x00\x00");
            append_file(&mut builder, "schema/params.json", PARAMS_JSON);
            builder.finish().expect("tar finish");
        }
        gzip_bytes(&tar_bytes)
    }

    #[tokio::test]
    async fn post_skill_lists_hub_source_and_capabilities() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique_skill_name();
        let package = build_nbskill(
            &name,
            "1.0.0",
            b"\0asm\x01\x00\x00\x00",
            &os_release_grants(),
        );
        let (status, bytes) = oneshot(&db, post_package(package.clone(), None)).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let posted = json_body(&bytes);
        assert_eq!(posted["name"], name);
        assert_eq!(posted["version"], "1.0.0");
        assert_eq!(posted["sha256"], sha256_hex(&package));
        assert_eq!(posted["signature_status"], "none");
        let posted_text = posted.to_string();
        assert!(posted_text.contains("fs.read:/etc/os-release"));
        assert!(posted_text.contains("sys.info.read"));

        let (status, bytes) = oneshot(&db, get_req("/v1/skills")).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let list = json_body(&bytes);
        assert_eq!(
            list["skills"],
            serde_json::json!(novbot_core::list_skills())
        );
        assert!(list["next_page_token"].is_null());
        let items = list["items"].as_array().expect("items");
        let item = items
            .iter()
            .find(|item| item["name"] == name)
            .expect("hub item");
        assert_eq!(item["source"], "hub");
        assert_eq!(item["sha256"], posted["sha256"]);
        assert_eq!(item["latest_version"], "1.0.0");
        assert_eq!(item["capabilities"], posted["capabilities"]);
        assert!(items
            .iter()
            .all(|item| { matches!(item["source"].as_str(), Some("hub") | Some("builtin")) }));
        for builtin in ["host_info", "echo", "env_get"] {
            let item = items
                .iter()
                .find(|item| item["name"] == builtin)
                .unwrap_or_else(|| panic!("missing builtin {builtin}"));
            assert_eq!(item["source"], "builtin");
            assert_eq!(item["display_name"], builtin);
            assert_eq!(item["description"], "");
            assert_eq!(item["latest_version"], "");
            assert_eq!(item["sha256"], "");
            assert_eq!(item["capabilities"], serde_json::json!([]));
        }

        let (status, bytes) = oneshot(&db, get_req(&format!("/v1/skills/{name}"))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let detail = json_body(&bytes);
        assert_eq!(detail["source"], "hub");
        assert_eq!(detail["sha256"], posted["sha256"]);

        let (status, bytes) =
            oneshot(&db, get_req(&format!("/v1/skills/{name}/versions/1.0.0"))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let version = json_body(&bytes);
        assert_eq!(version["sha256"], posted["sha256"]);
        assert_eq!(version["capabilities"], posted["capabilities"]);

        let (status, bytes) = oneshot(
            &db,
            get_req(&format!("/v1/skills/{name}/versions/1.0.0/package")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes.as_ref(), package.as_slice());

        let non_hub = sqlx::query("SELECT name FROM skills WHERE source <> 'hub'")
            .fetch_all(db.pool())
            .await
            .expect("non-hub");
        assert!(non_hub.is_empty());
        let builtins =
            sqlx::query("SELECT name FROM skills WHERE name IN ('host_info', 'echo', 'env_get')")
                .fetch_all(db.pool())
                .await
                .expect("builtins");
        assert!(builtins.is_empty());

        let (status, bytes) = oneshot(&db, get_req("/v1/skills/not-a-real-skill")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json_body(&bytes)["code"], "skill_unknown");
        let (status, bytes) =
            oneshot(&db, get_req(&format!("/v1/skills/{name}/versions/9.9.9"))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json_body(&bytes)["code"], "version_unknown");
    }

    #[tokio::test]
    async fn post_same_version_different_payload_is_version_exists() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique_skill_name();
        let wasm_a = b"\0asm\x01\x00\x00\x00";
        let mut wasm_b = wasm_a.to_vec();
        wasm_b[4] = 0x02;
        let first = build_nbskill(&name, "1.0.0", wasm_a, &os_release_grants());
        let second = build_nbskill(&name, "1.0.0", &wasm_b, &os_release_grants());
        let (status, bytes) = oneshot(&db, post_package(first.clone(), None)).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let sha_a = sha256_hex(&first);

        let (status, bytes) = oneshot(&db, post_package(second.clone(), None)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let body = json_body(&bytes);
        assert_eq!(body["code"], "version_exists");
        assert_eq!(body["license_required"], false);

        assert_eq!(
            stored_version_sha(&db, &name, "1.0.0").await.as_deref(),
            Some(sha_a.as_str())
        );
        assert_eq!(
            artifact_bytes(&db, &sha_a).await.as_deref(),
            Some(first.as_slice())
        );
        assert!(artifact_bytes(&db, &sha256_hex(&second)).await.is_none());
    }

    #[tokio::test]
    async fn post_identical_reupload_is_200() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique_skill_name();
        let package = build_nbskill(
            &name,
            "1.0.0",
            b"\0asm\x01\x00\x00\x00",
            &os_release_grants(),
        );
        let sha = sha256_hex(&package);
        let (status, _) = oneshot(&db, post_package(package.clone(), None)).await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, bytes) = oneshot(&db, post_package(package, Some(&sha))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let body = json_body(&bytes);
        assert_eq!(body["sha256"], sha);
        assert_eq!(body["name"], name);
        assert_eq!(
            count_i64(
                &db,
                "SELECT COUNT(*) AS n FROM skill_versions WHERE sha256 = ?",
                Some(&sha),
            )
            .await,
            1
        );
        assert_eq!(
            count_i64(
                &db,
                "SELECT COUNT(*) AS n FROM skill_artifacts WHERE sha256 = ?",
                Some(&sha),
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn post_package_larger_than_16_mib_is_rejected() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let body = vec![0u8; 16 * 1024 * 1024 + 1];
        let sha = sha256_hex(&body);
        let oversized_before = oversized_artifact_count(&db).await;
        let (status, bytes) = oneshot(&db, post_package(body, None)).await;
        assert_eq!(
            status,
            StatusCode::PAYLOAD_TOO_LARGE,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let payload = json_body(&bytes);
        assert_eq!(payload["code"], "package_too_large");
        assert_eq!(payload["license_required"], false);
        assert!(artifact_bytes(&db, &sha).await.is_none());
        assert_eq!(oversized_artifact_count(&db).await, oversized_before);
        assert_eq!(oversized_before, 0);
    }

    #[tokio::test]
    async fn post_builtin_name_is_reserved() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let package = build_nbskill("host_info", "1.0.0", b"\0asm\x01\x00\x00\x00", &[]);
        let sha = sha256_hex(&package);
        let (status, bytes) = oneshot(&db, post_package(package, None)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let body = json_body(&bytes);
        assert_eq!(body["code"], "name_reserved");
        assert_eq!(body["license_required"], false);
        assert_eq!(
            count_i64(
                &db,
                "SELECT COUNT(*) AS n FROM skills WHERE name = ?",
                Some("host_info")
            )
            .await,
            0
        );
        assert!(artifact_bytes(&db, &sha).await.is_none());
    }

    #[tokio::test]
    async fn post_sha256_header_mismatch() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique_skill_name();
        let package = build_nbskill(
            &name,
            "1.0.0",
            b"\0asm\x01\x00\x00\x00",
            &os_release_grants(),
        );
        let sha = sha256_hex(&package);
        let wrong = "ab".repeat(32);
        let (status, bytes) = oneshot(&db, post_package(package, Some(&wrong))).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let body = json_body(&bytes);
        assert_eq!(body["code"], "hash_mismatch");
        assert_eq!(body["license_required"], false);
        assert_eq!(
            count_i64(
                &db,
                "SELECT COUNT(*) AS n FROM skills WHERE name = ?",
                Some(&name)
            )
            .await,
            0
        );
        assert!(artifact_bytes(&db, &sha).await.is_none());
    }

    #[tokio::test]
    async fn post_unreadable_package_is_invalid_archive() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let body = b"not-an-archive".to_vec();
        let sha = sha256_hex(&body);
        let (status, bytes) = oneshot(&db, post_package(body, None)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(json_body(&bytes)["code"], "invalid_archive");
        assert!(artifact_bytes(&db, &sha).await.is_none());
    }

    #[tokio::test]
    async fn post_skill_toml_parse_failure_is_invalid_manifest() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let package = build_bad_toml_package();
        let sha = sha256_hex(&package);
        let (status, bytes) = oneshot(&db, post_package(package, None)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(json_body(&bytes)["code"], "invalid_manifest");
        assert!(artifact_bytes(&db, &sha).await.is_none());
    }

    async fn stored_version_sha(db: &Db, name: &str, version: &str) -> Option<String> {
        let row = sqlx::query(
            r#"SELECT v.sha256 AS sha256
               FROM skill_versions v
               INNER JOIN skills s ON s.id = v.skill_id
               WHERE s.name = ? AND v.version = ?"#,
        )
        .bind(name)
        .bind(version)
        .fetch_optional(db.pool())
        .await
        .expect("version sha");
        row.map(|row| {
            let sha: String = row.try_get("sha256").expect("sha");
            sha.trim().to_string()
        })
    }

    async fn artifact_bytes(db: &Db, sha: &str) -> Option<Vec<u8>> {
        let row = sqlx::query("SELECT data FROM skill_artifacts WHERE sha256 = ?")
            .bind(sha)
            .fetch_optional(db.pool())
            .await
            .expect("artifact");
        row.map(|row| row.try_get("data").expect("data"))
    }

    async fn oversized_artifact_count(db: &Db) -> i64 {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM skill_artifacts WHERE size_bytes > ?")
            .bind(PACKAGE_MAX_BYTES as i64)
            .fetch_one(db.pool())
            .await
            .expect("oversized count");
        row.try_get("n").expect("n")
    }
}
