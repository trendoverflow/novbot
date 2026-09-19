// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! File-backed JSONL report retry spool. Report failures must not crash the node process.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReport {
    pub node_id: String,
    pub run_id: String,
    pub spec_id: String,
    pub status: String,
    pub payload_json: String,
    pub observed_at_unix_ms: i64,
    pub attempts: u32,
}

pub struct RetrySpool {
    path: PathBuf,
}

impl RetrySpool {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            path: data_dir.as_ref().join("report_retry.jsonl"),
        }
    }

    pub async fn enqueue(&self, mut item: PendingReport) -> Result<()> {
        item.attempts = item.attempts.max(1);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .with_context(|| format!("open spool {}", self.path.display()))?;
        let line = serde_json::to_string(&item)?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    pub async fn drain(&self) -> Result<Vec<PendingReport>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let f = File::open(&self.path).await?;
        let mut lines = BufReader::new(f).lines();
        let mut items = Vec::new();
        while let Some(line) = lines.next_line().await? {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<PendingReport>(line) {
                Ok(item) => items.push(item),
                Err(e) => tracing::warn!(error = %e, "skip corrupt spool line"),
            }
        }
        let _ = fs::write(&self.path, b"").await;
        Ok(items)
    }
}

/// OSS single-file egress: write/replace last_result.json under data_dir.
pub async fn write_egress_result(
    data_dir: impl AsRef<Path>,
    value: &serde_json::Value,
) -> Result<PathBuf> {
    let dir = data_dir.as_ref();
    fs::create_dir_all(dir).await?;
    let path = dir.join("last_result.json");
    let tmp = dir.join("last_result.json.tmp");
    let body = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, &body).await?;
    fs::rename(&tmp, &path).await?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spool_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let spool = RetrySpool::new(dir.path());
        spool
            .enqueue(PendingReport {
                node_id: "n1".into(),
                run_id: "r1".into(),
                spec_id: "cpu".into(),
                status: "ok".into(),
                payload_json: "{}".into(),
                observed_at_unix_ms: 1,
                attempts: 1,
            })
            .await
            .unwrap();
        let items = spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].spec_id, "cpu");
        assert!(spool.drain().await.unwrap().is_empty());
    }
}
