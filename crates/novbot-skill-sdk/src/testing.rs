// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! In-process host for `cargo test` of guest logic.
//!
//! This module is not built for `wasm32`. It does not link the skill runtime.

use crate::{FileStat, HostError, ListenSocket};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;

thread_local! {
    static HOST: RefCell<Option<MockHost>> = const { RefCell::new(None) };
}

/// Scripted answers for the host imports.
#[derive(Debug, Clone)]
pub struct MockHost {
    pub files: BTreeMap<String, Vec<u8>>,
    pub lists: BTreeMap<String, Vec<String>>,
    pub ports: Vec<ListenSocket>,
    pub info: String,
    pub time_sync: String,
    pub metrics: String,
    pub processes: String,
    pub interfaces: String,
    pub env: BTreeMap<String, String>,
    pub logs: Vec<(u8, String)>,
    pub calls: Vec<String>,
    denies: BTreeMap<(String, String), HostError>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            files: BTreeMap::new(),
            lists: BTreeMap::new(),
            ports: Vec::new(),
            info: r#"{"hostname":"mock-host"}"#.to_string(),
            time_sync: r#"{"synced":false,"source":null,"offset_ms":null}"#.to_string(),
            metrics: r#"{"cpu":{"usage_percent":0,"load_avg_1m":null,"load_avg_5m":null,"load_avg_15m":null},"memory":{},"disks":[],"disk_io":[]}"#.to_string(),
            processes: r#"{"processes":[]}"#.to_string(),
            interfaces: r#"{"interfaces":[]}"#.to_string(),
            env: BTreeMap::new(),
            logs: Vec::new(),
            calls: Vec::new(),
            denies: BTreeMap::new(),
        }
    }
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_file(&mut self, path: impl Into<String>, bytes: impl AsRef<[u8]>) {
        self.files.insert(path.into(), bytes.as_ref().to_vec());
    }

    /// Make one host call return `denied`. `target` is empty for a bare import.
    pub fn deny(
        &mut self,
        capability: impl Into<String>,
        target: impl Into<String>,
        reason: impl Into<String>,
    ) {
        let capability = capability.into();
        let target = target.into();
        let reason = reason.into();
        self.denies.insert(
            (capability.clone(), target.clone()),
            HostError::Denied {
                capability,
                target,
                reason,
            },
        );
    }

    pub(crate) fn read(&mut self, path: &str, max_bytes: u32) -> Result<Vec<u8>, HostError> {
        self.calls.push(format!("fs.read {path}"));
        if let Some(err) = self.denies.get(&("fs.read".to_string(), path.to_string())) {
            return Err(err.clone());
        }
        match self.files.get(path) {
            Some(bytes) => {
                let end = (max_bytes as usize).min(bytes.len());
                Ok(bytes[..end].to_vec())
            }
            None => Err(HostError::NotFound(path.to_string())),
        }
    }

    pub(crate) fn stat(&mut self, path: &str) -> Result<FileStat, HostError> {
        self.calls.push(format!("fs.stat {path}"));
        if let Some(err) = self.denies.get(&("fs.stat".to_string(), path.to_string())) {
            return Err(err.clone());
        }
        if let Some(bytes) = self.files.get(path) {
            return Ok(FileStat {
                exists: true,
                kind: "file".to_string(),
                mode: 0o644,
                uid: 0,
                gid: 0,
                size: bytes.len() as u64,
                mtime_unix_ms: 0,
            });
        }
        Ok(FileStat {
            exists: false,
            kind: "other".to_string(),
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            mtime_unix_ms: 0,
        })
    }

    pub(crate) fn list(
        &mut self,
        dir: &str,
        _max_depth: u8,
        max_entries: u32,
    ) -> Result<Vec<String>, HostError> {
        self.calls.push(format!("fs.list {dir}"));
        if let Some(err) = self.denies.get(&("fs.list".to_string(), dir.to_string())) {
            return Err(err.clone());
        }
        let Some(entries) = self.lists.get(dir) else {
            return Err(HostError::NotFound(dir.to_string()));
        };
        let end = (max_entries as usize).min(entries.len());
        Ok(entries[..end].to_vec())
    }

    pub(crate) fn listening_ports(&mut self) -> Result<Vec<ListenSocket>, HostError> {
        self.calls.push("net.listening_ports".to_string());
        if let Some(err) = self
            .denies
            .get(&("net.listening_ports.read".to_string(), String::new()))
        {
            return Err(err.clone());
        }
        Ok(self.ports.clone())
    }

    pub(crate) fn interfaces(&mut self) -> Result<String, HostError> {
        self.calls.push("net.interfaces".to_string());
        self.bare("net.interfaces.read", self.interfaces.clone())
    }

    pub(crate) fn info(&mut self) -> Result<String, HostError> {
        self.calls.push("sys.info".to_string());
        self.bare("sys.info.read", self.info.clone())
    }

    pub(crate) fn time_sync(&mut self) -> Result<String, HostError> {
        self.calls.push("sys.time_sync".to_string());
        self.bare("sys.time_sync.read", self.time_sync.clone())
    }

    pub(crate) fn metrics(&mut self) -> Result<String, HostError> {
        self.calls.push("sys.metrics".to_string());
        self.bare("sys.metrics.read", self.metrics.clone())
    }

    pub(crate) fn processes(&mut self) -> Result<String, HostError> {
        self.calls.push("proc.processes".to_string());
        self.bare("proc.list.read", self.processes.clone())
    }

    pub(crate) fn env_get(&mut self, key: &str) -> Result<Option<String>, HostError> {
        self.calls.push(format!("env.get {key}"));
        if let Some(err) = self.denies.get(&("env.read".to_string(), key.to_string())) {
            return Err(err.clone());
        }
        Ok(self.env.get(key).cloned())
    }

    pub(crate) fn log(&mut self, level: u8, message: &str) {
        self.calls.push(format!("log {level}"));
        self.logs.push((level, message.to_string()));
    }

    fn bare(&self, capability: &str, value: String) -> Result<String, HostError> {
        if let Some(err) = self.denies.get(&(capability.to_string(), String::new())) {
            return Err(err.clone());
        }
        Ok(value)
    }
}

pub(crate) fn call<T>(
    f: impl FnOnce(&mut MockHost) -> Result<T, HostError>,
) -> Result<T, HostError> {
    HOST.with(|cell| {
        let mut slot = cell.borrow_mut();
        let Some(host) = slot.as_mut() else {
            return Err(HostError::Io("mock host is not installed".into()));
        };
        f(host)
    })
}

pub(crate) fn log(level: u8, message: &str) {
    let _ = call(|host| {
        host.log(level, message);
        Ok(())
    });
}

/// Install `host` for the closure, then return it with any recorded calls.
pub fn with_host<R>(host: MockHost, body: impl FnOnce() -> R) -> (R, MockHost) {
    HOST.with(|cell| *cell.borrow_mut() = Some(host));
    let result = std::panic::catch_unwind(AssertUnwindSafe(body));
    let host = HOST.with(|cell| cell.borrow_mut().take());
    match result {
        Ok(value) => (value, host.expect("mock host")),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}
