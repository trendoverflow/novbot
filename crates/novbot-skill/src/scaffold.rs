// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! `novbot-skill new` writes a cdylib project that targets `wasm32-wasip2`.

use anyhow::Context;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub fn write(cwd: &Path, name: &str) -> anyhow::Result<()> {
    if !valid_skill_name(name) {
        anyhow::bail!(
            "skill name must be 2-63 characters, start with a lowercase letter, and use only lowercase letters, digits, and hyphens"
        );
    }
    let project = cwd.join(name);
    if project.exists() {
        anyhow::bail!("{} already exists", project.display());
    }
    let sdk = find_sdk()
        .context("novbot-skill-sdk was not found; run novbot-skill new from a NovBot checkout")?;
    fs::create_dir_all(project.join("src")).context("create project")?;
    fs::create_dir_all(project.join("schema"))?;
    let sdk_path = relative_path(&project, &sdk);
    fs::write(project.join("Cargo.toml"), cargo_toml(name, &sdk_path))?;
    fs::write(project.join("src/lib.rs"), LIB_RS)?;
    fs::write(project.join("skill.toml"), skill_toml(name))?;
    fs::write(project.join("schema/params.json"), PARAMS)?;
    fs::write(project.join("README.md"), readme(name))?;
    Ok(())
}

fn valid_skill_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (2..=63).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn find_sdk() -> Option<PathBuf> {
    let mut starts = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    for start in starts {
        let mut dir = start;
        loop {
            let candidate = dir.join("crates/novbot-skill-sdk/Cargo.toml");
            if candidate.is_file() {
                return candidate.parent().map(Path::to_path_buf);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

fn relative_path(from_dir: &Path, to: &Path) -> String {
    let from = from_dir
        .canonicalize()
        .unwrap_or_else(|_| from_dir.to_path_buf());
    let to = to.canonicalize().unwrap_or_else(|_| to.to_path_buf());
    let from_parts: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let mut shared = 0;
    while shared < from_parts.len()
        && shared < to_parts.len()
        && from_parts[shared] == to_parts[shared]
    {
        shared += 1;
    }
    let mut out = PathBuf::new();
    for _ in shared..from_parts.len() {
        out.push("..");
    }
    for component in &to_parts[shared..] {
        match component {
            Component::Normal(part) => out.push(part),
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => out.push(".."),
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        ".".to_string()
    } else {
        out.to_string_lossy().replace('\\', "/")
    }
}

fn cargo_toml(name: &str, sdk: &str) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"
license = "Apache-2.0"
publish = false
description = "NovBot skill"

[workspace]

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
novbot-skill-sdk = {{ path = "{sdk}" }}
serde_json = "1"

[profile.release]
opt-level = 1
lto = false
codegen-units = 16
panic = "abort"
"#,
        sdk = toml_escape(sdk),
    )
}

fn skill_toml(name: &str) -> String {
    format!(
        r#"schema_version = 1
name = "{name}"
version = "0.1.0"
display_name = "{name}"
description = "Sample NovBot skill"

[runtime]
kind = "wasm"
abi = "novbot:skill@1"

[runtime.limits]
timeout_ms = 5000
memory_mb = 32

[files]

[[capabilities]]
name = "fs.read"
scope = "/etc/os-release"
reason = "Read the OS release file"
"#
    )
}

fn readme(name: &str) -> String {
    format!(
        r#"# {name}

Sample NovBot skill. `cargo test` runs the check against `testing::MockHost` on the host.

From this directory:

```
novbot-skill build
novbot-skill test --params '{{"path":"/etc/os-release"}}'
novbot-skill pack
novbot-skill inspect {name}-0.1.0.nbskill
novbot-skill publish {name}-0.1.0.nbskill --center http://127.0.0.1:8080
```

`build` runs `cargo build --release --target wasm32-wasip2` and writes a component at `target/wasm32-wasip2/release/module.wasm`.

`test` loads that component with `novbot_skill_runtime::run` and the grants in `skill.toml`. When `timeout_ms` is set, it is passed as `RunRequest.timeout`. `--fixture-root` copies a directory into a temporary directory and does not pass it as `data_dir`, because `data_dir` is a denylist.

`publish` reads a token from `--token` or `NOVBOT_API_TOKEN` and sends it as `Authorization: Bearer`. The token is not printed. A directory argument uploads each `*.nbskill` on its own.

This CLI does not install or run the package on a node.
"#
    )
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

const PARAMS: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "properties": {
    "path": { "type": "string" }
  },
  "additionalProperties": false
}
"#;

const LIB_RS: &str = r#"// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Sample skill. Host tests use `testing::MockHost`.

#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

fn check(params: serde_json::Value) -> Result<serde_json::Value, String> {
    let path = params
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("/etc/os-release");
    let finding = match novbot_skill_sdk::fs::read(path, 4096) {
        Ok(bytes) => novbot_skill_sdk::Finding::new(
            "sample.read",
            novbot_skill_sdk::FindingStatus::Pass,
            "info",
        )
        .evidence(serde_json::json!({ "path": path, "bytes": bytes.len() }))
        .remediation("No action required."),
        Err(err) => novbot_skill_sdk::Finding::new(
            "sample.read",
            novbot_skill_sdk::FindingStatus::Error,
            "info",
        )
        .evidence(serde_json::json!({ "path": path, "error": err.to_string() }))
        .remediation("Grant fs.read for the path and retry."),
    };
    Ok(serde_json::json!({ "findings": [finding] }))
}

#[cfg(target_arch = "wasm32")]
novbot_skill_sdk::main!(check);

#[cfg(test)]
mod tests {
    use super::check;
    use novbot_skill_sdk::testing::{with_host, MockHost};

    #[test]
    fn mock_host_reads_os_release() {
        let mut host = MockHost::new();
        host.insert_file("/etc/os-release", b"ID=test\n");
        let (value, _) = with_host(host, || check(serde_json::json!({})).unwrap());
        assert_eq!(value["findings"][0]["status"], "pass");
        assert_eq!(value["findings"][0]["evidence"]["bytes"], 8);
    }
}
"#;
