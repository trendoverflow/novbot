// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! POST raw `.nbskill` bytes to `POST /v1/skills`.
//!
//! The token is sent as `Authorization: Bearer` and is removed from errors.

use anyhow::Context;
use novbot_node::skills::{load_package, sha256_hex};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CONTENT_TYPE: &str = "application/vnd.novbot.skill";

pub fn publish(path: &Path, center: &str, token: Option<String>) -> anyhow::Result<()> {
    let token = token_value(token);
    let url = skills_url(center)?;
    let files = package_paths(path)?;
    let mut failed = false;
    for file in files {
        match publish_one(&url, token.as_deref(), &file) {
            Ok(line) => println!("{line}"),
            Err(err) => {
                failed = true;
                let message = sanitize(&format!("{err:#}"), token.as_deref());
                println!("error {}: {message}", file.display());
            }
        }
    }
    if failed {
        anyhow::bail!("one or more packages failed");
    }
    Ok(())
}

fn publish_one(url: &str, token: Option<&str>, file: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let package = load_package(&bytes).with_context(|| format!("open {}", file.display()))?;
    let sha256 = sha256_hex(&bytes);
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(5))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(60))
        .build();
    let mut request = agent
        .post(url)
        .set("Content-Type", CONTENT_TYPE)
        .set("X-Novbot-Sha256", &sha256);
    if let Some(token) = token {
        let mut header = String::from("Bearer ");
        header.push_str(token);
        request = request.set("Authorization", &header);
    }
    match request.send_bytes(&bytes) {
        Ok(response) => {
            let status = response.status();
            if !(200..300).contains(&status) {
                anyhow::bail!("http {status}");
            }
            Ok(format!(
                "{}@{} {sha256}",
                package.manifest.name, package.manifest.version
            ))
        }
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            anyhow::bail!("http {status}: {}", server_message(&body))
        }
        Err(err) => anyhow::bail!("publish request failed: {err}"),
    }
}

fn server_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|item| item.as_str())
                .map(str::to_string)
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| body.chars().take(300).collect())
}

fn skills_url(center: &str) -> anyhow::Result<String> {
    let base = center.trim().trim_end_matches('/');
    if base.is_empty() {
        anyhow::bail!("center URL is empty");
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        anyhow::bail!("center URL must start with http:// or https://");
    }
    if base.ends_with("/v1/skills") {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/v1/skills"))
    }
}

fn package_paths(path: &Path) -> anyhow::Result<Vec<PathBuf>> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        anyhow::bail!("{} is not a file or directory", path.display());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(path).with_context(|| format!("read {}", path.display()))? {
        let entry = entry?;
        let file = entry.path();
        if file.extension().and_then(|ext| ext.to_str()) == Some("nbskill") && file.is_file() {
            files.push(file);
        }
    }
    files.sort();
    if files.is_empty() {
        anyhow::bail!("no .nbskill files in {}", path.display());
    }
    Ok(files)
}

fn token_value(flag: Option<String>) -> Option<String> {
    flag.or_else(|| std::env::var("NOVBOT_API_TOKEN").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn sanitize(text: &str, token: Option<&str>) -> String {
    let mut text = text.replace(['\n', '\r'], " ");
    if let Some(token) = token.filter(|token| !token.is_empty()) {
        text = text.replace(token, "[redacted]");
    }
    text
}
