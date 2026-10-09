// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Print a package manifest, its composed grants, and the component imports.

use anyhow::Context;
use novbot_node::skills::load_package;
use std::path::Path;

pub fn inspect(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let package = load_package(&bytes).with_context(|| format!("open {}", path.display()))?;
    let manifest = std::str::from_utf8(
        package
            .files
            .get("skill.toml")
            .context("skill.toml is missing")?,
    )
    .context("skill.toml is not utf-8")?;
    let wasm = package
        .files
        .get("module.wasm")
        .context("module.wasm is missing")?;
    let imports = component_imports(wasm)?;
    let mut out = String::new();
    out.push_str("manifest:\n");
    out.push_str(manifest);
    if !manifest.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("grants:\n");
    for grant in &package.manifest.grants {
        out.push_str("- ");
        out.push_str(grant);
        out.push('\n');
    }
    out.push_str("imports:\n");
    for import in &imports {
        out.push_str("- ");
        out.push_str(import);
        out.push('\n');
    }
    if imports_net(&imports) && !declares_net(&package.manifest.grants) {
        out.push_str("lint: module imports net but declares no net.* grant\n");
    }
    Ok(out)
}

fn component_imports(wasm: &[u8]) -> anyhow::Result<Vec<String>> {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    let engine =
        wasmtime::Engine::new(&config).map_err(|err| anyhow::anyhow!("wasm engine: {err}"))?;
    let component = wasmtime::component::Component::new(&engine, wasm)
        .map_err(|err| anyhow::anyhow!("module.wasm is not a wasm component: {err}"))?;
    let mut names: Vec<String> = component
        .component_type()
        .imports(&engine)
        .map(|(name, _)| name.to_string())
        .collect();
    names.sort();
    Ok(names)
}

fn imports_net(imports: &[String]) -> bool {
    imports.iter().any(|name| name.contains("novbot:skill/net"))
}

fn declares_net(grants: &[String]) -> bool {
    grants.iter().any(|grant| {
        let name = grant.split_once(':').map(|(name, _)| name).unwrap_or(grant);
        name.starts_with("net.")
    })
}
