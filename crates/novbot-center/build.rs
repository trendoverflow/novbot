// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

fn main() {
    println!("cargo:rerun-if-env-changed=NOVBOT_TEST_DATABASE_URL");
    println!("cargo:rustc-check-cfg=cfg(novbot_test_db_missing)");
    let url = std::env::var("NOVBOT_TEST_DATABASE_URL").unwrap_or_default();
    if url.trim().is_empty() {
        println!("cargo:rustc-cfg=novbot_test_db_missing");
    }

    emit_git_sha();
}

/// `NOVBOT_GIT_SHA` pins the embedded commit (CI, OrbStack, manual builds).
/// Otherwise this is `git rev-parse HEAD`, or `unknown` when git is unavailable.
fn emit_git_sha() {
    println!("cargo:rerun-if-env-changed=NOVBOT_GIT_SHA");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let sha = match std::env::var("NOVBOT_GIT_SHA") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => {
            watch_git_head(&manifest_dir);
            git_head(&manifest_dir)
        }
    };
    println!("cargo:rustc-env=NOVBOT_GIT_SHA={sha}");
}

fn watch_git_head(manifest_dir: &str) {
    let Some(head) = git_path(manifest_dir, "HEAD") else {
        return;
    };
    println!("cargo:rerun-if-changed={head}");
    let Ok(text) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(refname) = text.trim().strip_prefix("ref: ") else {
        return;
    };
    if let Some(path) = git_path(manifest_dir, refname.trim()) {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_path(manifest_dir: &str, spec: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", spec])
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rel = String::from_utf8(output.stdout).ok()?;
    let rel = rel.trim();
    if rel.is_empty() {
        return None;
    }
    let path = std::path::Path::new(rel);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::path::Path::new(manifest_dir).join(path)
    };
    Some(full.display().to_string())
}

fn git_head(manifest_dir: &str) -> String {
    let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(manifest_dir)
        .output()
    else {
        return "unknown".into();
    };
    if !output.status.success() {
        return "unknown".into();
    }
    let Ok(sha) = String::from_utf8(output.stdout) else {
        return "unknown".into();
    };
    let sha = sha.trim();
    if sha.is_empty() {
        "unknown".into()
    } else {
        sha.to_string()
    }
}
