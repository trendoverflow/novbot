// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::skill::{invoke_skill, skill_from_params};
use crate::spec::{Spec, SpecKind};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
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
        SpecKind::ComplianceSshd => probe_compliance_sshd(spec),
        SpecKind::ComplianceListeningPorts => probe_compliance_listening_ports(spec),
        SpecKind::ComplianceWorldWritable => probe_compliance_world_writable(spec),
        SpecKind::ComplianceNtp => probe_compliance_ntp(spec).await,
        SpecKind::ComplianceRebootRequired => probe_compliance_reboot_required(spec),
        SpecKind::Exec => probe_exec(spec).await,
        SpecKind::Skill => {
            let (name, args) = skill_from_params(false, &spec.params)?;
            invoke_skill(&name, &args)
        }
        SpecKind::McpTool => {
            let (name, args) = skill_from_params(true, &spec.params)?;
            invoke_skill(&name, &args)
        }
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
            "check": "compliance_path",
            "path": path,
            "exists": exists,
            "must_exist": must_exist,
            "passed": ok,
        }),
    })
}

fn probe_compliance_sshd(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let path = spec
        .params
        .get("config_path")
        .and_then(|v| v.as_str())
        .unwrap_or("/etc/ssh/sshd_config");
    let expect_permit_root = spec
        .params
        .get("permit_root_login")
        .and_then(|v| v.as_str())
        .unwrap_or("no");
    let expect_password_auth = spec
        .params
        .get("password_authentication")
        .and_then(|v| v.as_str())
        .unwrap_or("no");

    if !Path::new(path).exists() {
        return Ok(ProbeOutcome {
            status: "fail",
            payload: json!({
                "check": "compliance_sshd",
                "config_path": path,
                "exists": false,
                "passed": false,
                "findings": ["sshd_config not found"],
            }),
        });
    }

    let content = fs::read_to_string(path)?;
    let permit_root = sshd_directive(&content, "PermitRootLogin");
    let password_auth = sshd_directive(&content, "PasswordAuthentication");

    let mut findings = Vec::new();
    let mut passed = true;

    match &permit_root {
        Some(v) if eq_ignore_ascii(v, expect_permit_root) => {}
        Some(v) => {
            passed = false;
            findings.push(format!(
                "PermitRootLogin={v} (expected {expect_permit_root})"
            ));
        }
        None => {
            // OpenSSH default is prohibit-password / yes depending on version; treat missing as soft fail vs expected "no".
            if expect_permit_root.eq_ignore_ascii_case("no") {
                passed = false;
                findings.push("PermitRootLogin not set (default may allow root login)".into());
            }
        }
    }

    match &password_auth {
        Some(v) if eq_ignore_ascii(v, expect_password_auth) => {}
        Some(v) => {
            passed = false;
            findings.push(format!(
                "PasswordAuthentication={v} (expected {expect_password_auth})"
            ));
        }
        None => {
            if expect_password_auth.eq_ignore_ascii_case("no") {
                passed = false;
                findings.push("PasswordAuthentication not set (default is often yes)".into());
            }
        }
    }

    Ok(ProbeOutcome {
        status: if passed { "ok" } else { "fail" },
        payload: json!({
            "check": "compliance_sshd",
            "config_path": path,
            "permit_root_login": permit_root,
            "password_authentication": password_auth,
            "expected": {
                "permit_root_login": expect_permit_root,
                "password_authentication": expect_password_auth,
            },
            "passed": passed,
            "findings": findings,
        }),
    })
}

fn sshd_directive(content: &str, key: &str) -> Option<String> {
    let mut last = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let k = parts.next()?;
        if k.eq_ignore_ascii_case(key) {
            if let Some(v) = parts.next() {
                last = Some(v.to_string());
            }
        }
    }
    last
}

fn eq_ignore_ascii(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn probe_compliance_listening_ports(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let max = spec
        .params
        .get("max_sample")
        .and_then(|v| v.as_u64())
        .unwrap_or(32)
        .clamp(1, 256) as usize;
    let mut ports: Vec<u16> = Vec::new();
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = fs::read_to_string(path) {
            for port in parse_proc_net_tcp_listen_ports(&text) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    ports.sort_unstable();
    let total = ports.len();
    ports.truncate(max);

    // Optional expect_absent list: fail if any of those ports are listening.
    let mut findings = Vec::new();
    let mut passed = true;
    if let Some(arr) = spec.params.get("expect_absent").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(p) = v.as_u64().map(|n| n as u16) {
                if ports.contains(&p) || /* check full set before truncate - use contains on sampled; also re-check */
                   parse_all_listen_contains(p)
                {
                    passed = false;
                    findings.push(format!("port {p} is listening but expect_absent"));
                }
            }
        }
    }

    Ok(ProbeOutcome {
        status: if passed { "ok" } else { "fail" },
        payload: json!({
            "check": "compliance_listening_ports",
            "listen_port_count": total,
            "sample_ports": ports,
            "passed": passed,
            "findings": findings,
        }),
    })
}

fn parse_all_listen_contains(port: u16) -> bool {
    for path in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = fs::read_to_string(path) {
            if parse_proc_net_tcp_listen_ports(&text).contains(&port) {
                return true;
            }
        }
    }
    false
}

/// Parse /proc/net/tcp{,6}: local_address is IP:PORT hex; state 0A = LISTEN.
fn parse_proc_net_tcp_listen_ports(text: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        // state
        if !cols[3].eq_ignore_ascii_case("0A") {
            continue;
        }
        let local = cols[1];
        if let Some((_, port_hex)) = local.rsplit_once(':') {
            if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                out.push(port);
            }
        }
    }
    out
}

fn probe_compliance_world_writable(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let root = spec
        .params
        .get("root")
        .and_then(|v| v.as_str())
        .unwrap_or("/etc");
    let max_findings = spec
        .params
        .get("max_findings")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 500) as usize;
    let max_entries = spec
        .params
        .get("max_entries")
        .and_then(|v| v.as_u64())
        .unwrap_or(5_000)
        .clamp(100, 50_000) as usize;

    let root_path = PathBuf::from(root);
    if !root_path.exists() {
        return Ok(ProbeOutcome {
            status: "fail",
            payload: json!({
                "check": "compliance_world_writable",
                "root": root,
                "exists": false,
                "passed": false,
                "findings": [format!("root path missing: {root}")],
            }),
        });
    }

    let mut findings: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    walk_world_writable(
        &root_path,
        max_entries,
        max_findings,
        &mut scanned,
        &mut findings,
    )?;

    let passed = findings.is_empty();
    Ok(ProbeOutcome {
        status: if passed { "ok" } else { "fail" },
        payload: json!({
            "check": "compliance_world_writable",
            "root": root,
            "scanned_entries": scanned,
            "world_writable": findings,
            "passed": passed,
            "findings": if passed {
                Vec::<String>::new()
            } else {
                findings.iter().map(|p| format!("world-writable: {p}")).collect::<Vec<_>>()
            },
        }),
    })
}

fn walk_world_writable(
    dir: &Path,
    max_entries: usize,
    max_findings: usize,
    scanned: &mut usize,
    findings: &mut Vec<String>,
) -> Result<(), ProbeError> {
    if findings.len() >= max_findings || *scanned >= max_entries {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()), // permission skipped
    };
    for entry in entries.flatten() {
        if findings.len() >= max_findings || *scanned >= max_entries {
            break;
        }
        *scanned += 1;
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = meta.permissions().mode();
            if mode & 0o002 != 0 {
                findings.push(path.display().to_string());
            }
        }
        if meta.is_dir() {
            walk_world_writable(&path, max_entries, max_findings, scanned, findings)?;
        }
    }
    Ok(())
}

async fn probe_compliance_ntp(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let timeout_ms = spec
        .params
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .unwrap_or(3_000)
        .min(10_000);

    // Prefer timedatectl; fall back to chronyc tracking.
    if let Ok(out) = run_cmd_capture(
        "timedatectl",
        &["show", "-p", "NTPSynchronized", "--value"],
        timeout_ms,
    )
    .await
    {
        let synced = out.stdout.trim().eq_ignore_ascii_case("yes");
        return Ok(ProbeOutcome {
            status: if synced { "ok" } else { "fail" },
            payload: json!({
                "check": "compliance_ntp",
                "method": "timedatectl",
                "ntp_synchronized": synced,
                "raw": truncate(&out.stdout, 512),
                "passed": synced,
                "findings": if synced { vec![] } else { vec!["NTP not synchronized"] },
            }),
        });
    }

    if let Ok(out) = run_cmd_capture("chronyc", &["tracking"], timeout_ms).await {
        let ok = out.status_success
            && (out.stdout.contains("Normal")
                || out.stdout.to_ascii_lowercase().contains("leap status"));
        // chronyc "Leap status     : Normal" indicates sync health roughly.
        let leap_normal = out.stdout.lines().any(|l| {
            let l = l.to_ascii_lowercase();
            l.contains("leap status") && l.contains("normal")
        });
        let passed = ok && leap_normal;
        return Ok(ProbeOutcome {
            status: if passed { "ok" } else { "fail" },
            payload: json!({
                "check": "compliance_ntp",
                "method": "chronyc",
                "leap_normal": leap_normal,
                "raw": truncate(&out.stdout, 1024),
                "passed": passed,
                "findings": if passed { vec![] } else { vec!["chronyc tracking not Normal / unavailable"] },
            }),
        });
    }

    Ok(ProbeOutcome {
        status: "fail",
        payload: json!({
            "check": "compliance_ntp",
            "method": "none",
            "passed": false,
            "findings": ["neither timedatectl nor chronyc available"],
        }),
    })
}

fn probe_compliance_reboot_required(spec: &Spec) -> Result<ProbeOutcome, ProbeError> {
    let path = spec
        .params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("/var/run/reboot-required");
    let expect_absent = spec
        .params
        .get("expect_absent")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let exists = Path::new(path).exists();
    let passed = if expect_absent { !exists } else { exists };
    let mut findings = Vec::new();
    if !passed && exists {
        findings.push("reboot required marker present".to_string());
        if let Ok(pkgs) = fs::read_to_string(format!("{path}.pkgs")) {
            findings.push(format!("packages: {}", truncate(pkgs.trim(), 512)));
        }
    }
    Ok(ProbeOutcome {
        status: if passed { "ok" } else { "fail" },
        payload: json!({
            "check": "compliance_reboot_required",
            "path": path,
            "exists": exists,
            "expect_absent": expect_absent,
            "passed": passed,
            "findings": findings,
        }),
    })
}

struct CmdOut {
    stdout: String,
    status_success: bool,
}

async fn run_cmd_capture(bin: &str, args: &[&str], timeout_ms: u64) -> Result<CmdOut, ProbeError> {
    let fut = Command::new(bin)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = timeout(Duration::from_millis(timeout_ms), fut)
        .await
        .map_err(|_| ProbeError::Failed(format!("{bin} timed out")))??;
    Ok(CmdOut {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        status_success: output.status.success(),
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

    #[tokio::test]
    async fn skill_host_info_via_spec() {
        let spec = Spec {
            id: "hi".into(),
            kind: SpecKind::Skill,
            params: json!({"skill": "host_info"}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        assert_eq!(out.status, "ok");
        assert_eq!(out.payload["skill"], "host_info");
    }

    #[tokio::test]
    async fn mcp_tool_echo() {
        let spec = Spec {
            id: "echo".into(),
            kind: SpecKind::McpTool,
            params: json!({"tool": "echo", "arguments": {"text": "ping"}}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        assert_eq!(out.status, "ok");
    }

    #[test]
    fn parse_proc_net_listen() {
        let sample = "  sl  local_address rem_address   st\n   0: 00000000:0016 00000000:0000 0A\n";
        let ports = parse_proc_net_tcp_listen_ports(sample);
        assert_eq!(ports, vec![0x16]); // 22
    }

    #[test]
    fn sshd_directive_last_wins() {
        let cfg = "# PermitRootLogin yes\nPermitRootLogin prohibit-password\nPermitRootLogin no\n";
        assert_eq!(
            sshd_directive(cfg, "PermitRootLogin").as_deref(),
            Some("no")
        );
    }

    #[tokio::test]
    async fn listening_ports_runs() {
        let spec = Spec {
            id: "ports".into(),
            kind: SpecKind::ComplianceListeningPorts,
            params: json!({"max_sample": 8}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        assert!(out.payload.get("sample_ports").is_some());
    }

    #[tokio::test]
    async fn world_writable_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let dirty = dir.path().join("dirty");
        fs::write(&dirty, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dirty).unwrap().permissions();
            perms.set_mode(0o666);
            fs::set_permissions(&dirty, perms).unwrap();
        }
        let spec = Spec {
            id: "ww".into(),
            kind: SpecKind::ComplianceWorldWritable,
            params: json!({"root": dir.path().to_string_lossy(), "max_findings": 10}),
            threshold: None,
        };
        let out = run_probe(&spec).await.unwrap();
        #[cfg(unix)]
        assert_eq!(out.status, "fail");
    }
}
