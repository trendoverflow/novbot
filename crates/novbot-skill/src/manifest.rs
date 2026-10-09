// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Flat `skill.toml` read and canonical rewrite.
//!
//! The center and the node both accept top-level `name` / `version`, `[runtime]`,
//! `[files]`, and `[[capabilities]]` with `name` and optional `scope`.

use anyhow::Context;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub publisher: Option<String>,
    pub platforms: Vec<String>,
    pub min_node_version: Option<String>,
    pub timeout_ms: Option<u64>,
    pub memory_mb: Option<u64>,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    pub scope: Option<String>,
    pub reason: Option<String>,
}

impl Capability {
    pub fn grant(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{}:{scope}", self.name),
            None => self.name.clone(),
        }
    }
}

impl Manifest {
    pub fn grants(&self) -> Vec<String> {
        self.capabilities.iter().map(Capability::grant).collect()
    }
}

#[derive(Debug, Deserialize)]
struct RawManifest {
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
    runtime: RawRuntime,
    #[serde(default)]
    capabilities: Vec<RawCapability>,
}

#[derive(Debug, Deserialize)]
struct RawRuntime {
    kind: String,
    abi: String,
    #[serde(default)]
    limits: RawLimits,
}

#[derive(Debug, Default, Deserialize)]
struct RawLimits {
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    memory_mb: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct RawCapability {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    grant: Option<String>,
}

pub fn load_file(path: &Path) -> anyhow::Result<Manifest> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse(&text)
}

pub fn parse(text: &str) -> anyhow::Result<Manifest> {
    let raw: RawManifest = toml::from_str(text).context("skill.toml")?;
    if raw.schema_version != 1 {
        anyhow::bail!("schema_version must be 1");
    }
    if raw.runtime.kind != "wasm" {
        anyhow::bail!("runtime.kind must be wasm");
    }
    if raw.runtime.abi != novbot_node::skills::ABI {
        anyhow::bail!("runtime.abi must be {}", novbot_node::skills::ABI);
    }
    let name = raw.name.trim().to_string();
    let version = raw.version.trim().to_string();
    if name.is_empty() || version.is_empty() {
        anyhow::bail!("name and version are required");
    }
    let mut capabilities = Vec::with_capacity(raw.capabilities.len());
    for cap in raw.capabilities {
        capabilities.push(normalize_capability(cap)?);
    }
    Ok(Manifest {
        name,
        version,
        display_name: nonempty(raw.display_name),
        description: nonempty(raw.description),
        publisher: nonempty(raw.publisher),
        platforms: raw.platforms,
        min_node_version: nonempty(raw.min_node_version),
        timeout_ms: raw.runtime.limits.timeout_ms,
        memory_mb: raw.runtime.limits.memory_mb,
        capabilities,
    })
}

fn normalize_capability(raw: RawCapability) -> anyhow::Result<Capability> {
    let preferred = if let Some(grant) = nonempty(raw.grant) {
        grant
    } else if let Some(name) = nonempty(raw.name) {
        match nonempty(raw.scope) {
            Some(scope) => format!("{name}:{scope}"),
            None => name,
        }
    } else {
        anyhow::bail!("capability is missing a name");
    };
    let (name, scope) = match preferred.split_once(':') {
        Some((name, scope)) if !name.is_empty() && !scope.is_empty() => {
            (name.to_string(), Some(scope.to_string()))
        }
        Some(_) => anyhow::bail!("capability grant is malformed"),
        None => (preferred, None),
    };
    Ok(Capability {
        name,
        scope,
        reason: nonempty(raw.reason),
    })
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

pub fn render(manifest: &Manifest, files: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    out.push_str("schema_version = 1\n");
    push_str(&mut out, "name", &manifest.name);
    push_str(&mut out, "version", &manifest.version);
    if let Some(display_name) = &manifest.display_name {
        push_str(&mut out, "display_name", display_name);
    }
    if let Some(description) = &manifest.description {
        push_str(&mut out, "description", description);
    }
    if let Some(publisher) = &manifest.publisher {
        push_str(&mut out, "publisher", publisher);
    }
    if let Some(min_node_version) = &manifest.min_node_version {
        push_str(&mut out, "min_node_version", min_node_version);
    }
    if !manifest.platforms.is_empty() {
        let items: Vec<String> = manifest
            .platforms
            .iter()
            .map(|item| toml_string(item))
            .collect();
        out.push_str(&format!("platforms = [{}]\n", items.join(", ")));
    }
    out.push_str("\n[runtime]\nkind = \"wasm\"\nabi = \"novbot:skill@1\"\n");
    if manifest.timeout_ms.is_some() || manifest.memory_mb.is_some() {
        out.push_str("\n[runtime.limits]\n");
        if let Some(timeout_ms) = manifest.timeout_ms {
            out.push_str(&format!("timeout_ms = {timeout_ms}\n"));
        }
        if let Some(memory_mb) = manifest.memory_mb {
            out.push_str(&format!("memory_mb = {memory_mb}\n"));
        }
    }
    out.push_str("\n[files]\n");
    for (path, hash) in files {
        out.push_str(&format!("{} = {}\n", toml_string(path), toml_string(hash)));
    }
    out.push('\n');
    for cap in &manifest.capabilities {
        out.push_str("[[capabilities]]\n");
        push_str(&mut out, "name", &cap.name);
        if let Some(scope) = &cap.scope {
            push_str(&mut out, "scope", scope);
        }
        if let Some(reason) = &cap.reason {
            push_str(&mut out, "reason", reason);
        }
        out.push('\n');
    }
    out
}

fn push_str(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(" = ");
    out.push_str(&toml_string(value));
    out.push('\n');
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
