// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::spec::{Spec, SpecKind};
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use sysinfo::{Disks, System};
use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("unsupported or invalid params: {0}")]
    Params(String),
    #[error("probe failed: {0}")]
    Failed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub status: &'static str,
    pub payload: Value,
}

pub async fn run_probe(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    match spec.kind {
        SpecKind::Cpu => Ok(probe_cpu()),
        SpecKind::Memory => Ok(probe_memory()),
        SpecKind::Disk => probe_disk(spec),
        SpecKind::CompliancePath => probe_compliance_path(spec),
        SpecKind::Exec => probe_exec(spec).await,
    }
}

fn probe_cpu() -> ProbeOutcome {
    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(Duration::from_millis(50));
    sys.refresh_cpu_usage();
    let usage: f32 = sys.global_cpu_usage();
    ProbeOutcome {
        status: "ok",
        payload: json!({
            "cpu_usage_percent": usage,
            "cpus": sys.cpus().len(),
        }),
    }
}

fn probe_memory() -> ProbeOutcome {
    let mut sys = System::new();
    sys.refresh_memory();
    ProbeOutcome {
        status: "ok",
        payload: json!({
            "total_bytes": sys.total_memory(),
            "used_bytes": sys.used_memory(),
            "available_bytes": sys.available_memory(),
        }),
    }
}

fn probe_disk(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let mount = spec
        .params
        .get("mount")
        .and_then(|v| v.as_str())
        .unwrap_or("/");
    let disks = Disks::new_with_refreshed_list();
    let found = disks.list().iter().find(|d| {
        d.mount_point()
            .to_str()
            .map(|p| p == mount)
            .unwrap_or(false)
    });
    match found {
        Some(d) => {
            let total = d.total_space();
            let avail = d.available_space();
            Ok(ProbeOutcome {
                status: "ok",
                payload: json!({
                    "mount": mount,
                    "total_bytes": total,
                    "available_bytes": avail,
                    "used_bytes": total.saturating_sub(avail),
                    "name": d.name().to_string_lossy(),
                }),
            })
        }
        None => Err(ProbeError::Failed(format!("mount not found: {mount}"))),
    }
}

fn probe_compliance_path(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let path = spec
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProbeError::Params("path required".into()))?;
    let must_exist = spec
        .params
        .get("must_exist")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let exists = Path::new(path).exists();
    let ok = exists == must_exist;
    Ok(ProbeOutcome {
        status: if ok { "ok" } else { "fail" },
        payload: json!({
            "path": path,
            "exists": exists,
            "must_exist": must_exist,
            "passed": ok,
        }),
    })
}

async fn probe_exec(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let cmdline = spec
        .params
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProbeError::Params("command required".into()))?;
    let timeout_ms = spec
        .params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(5_000)
        .min(30_000);

    let fut = Command::new("sh")
        .arg("-c")
        .arg(cmdline)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();

    let output = timeout(Duration::from_millis(timeout_ms), fut)
        .await
        .map_err(|_| ProbeError::Failed("exec timed out".into()))??;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(ProbeOutcome {
        status: if output.status.success() {
            "ok"
        } else {
            "fail"
        },
        payload: json!({
            "exit_code": output.status.code(),
            "stdout": truncate(&stdout, 8192),
            "stderr": truncate(&stderr, 4096),
        }),
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn memory_probe_ok() {
        let spec = Spec {
            id: "mem".into(),
            kind: SpecKind::Memory,
            params: json!({}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        assert_eq!(out.status, "ok");
        assert!(out.payload.get("total_bytes").is_some());
    }

    #[tokio::test]
    async fn compliance_path_root() {
        let spec = Spec {
            id: "root-exists".into(),
            kind: SpecKind::CompliancePath,
            params: json!({"path": "/", "must_exist": true}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        assert_eq!(out.status, "ok");
    }
}
