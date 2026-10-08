// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Owned copies of the WIT records and the T4.6 finding object.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `fs.stat` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStat {
    pub exists: bool,
    pub kind: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub mtime_unix_ms: i64,
}

/// One listening socket from `net.listening_ports`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenSocket {
    pub proto: String,
    pub addr: String,
    pub port: u16,
    pub uid: u32,
}

/// `pass`, `warn`, `fail`, or `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Pass,
    Warn,
    Fail,
    Error,
}

/// T4.6 finding. Serialized with `id`, `status`, `severity`, `evidence`, and `remediation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub status: FindingStatus,
    pub severity: String,
    pub evidence: Value,
    pub remediation: String,
}

impl Finding {
    pub fn new(id: impl Into<String>, status: FindingStatus, severity: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status,
            severity: severity.into(),
            evidence: Value::Object(Default::default()),
            remediation: String::new(),
        }
    }

    pub fn evidence(mut self, evidence: Value) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn remediation(mut self, remediation: impl Into<String>) -> Self {
        self.remediation = remediation.into();
        self
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|err| err.to_string())
    }
}

/// Serialize findings as a JSON array. That string is a valid `run` result.
pub fn findings_json(findings: &[Finding]) -> Result<String, String> {
    serde_json::to_string(findings).map_err(|err| err.to_string())
}
