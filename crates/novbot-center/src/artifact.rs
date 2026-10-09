// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Immutable Hub package bytes in MySQL `LONGBLOB`, deduped by sha256.
//!
//! `put` inserts a row only when that sha256 is absent. It never updates `data`.

use crate::db::Db;
use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use sqlx::{MySqlConnection, Row};
use std::ops::DerefMut;

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn mysql_is_duplicate(err: &sqlx::Error) -> bool {
    err.as_database_error()
        .and_then(|db| db.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .is_some_and(|mysql| mysql.number() == 1062)
}

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    /// Store `bytes` under `sha256`.
    ///
    /// No-op when that sha256 already exists and `size_bytes` matches.
    /// Error when the same sha256 exists with a different size (different bytes).
    async fn put(&self, sha256: &str, bytes: &[u8]) -> Result<()>;

    /// Full stored bytes, or `None` when the sha256 is absent.
    async fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>>;

    async fn exists(&self, sha256: &str) -> Result<bool>;
}

/// Insert on `tx` when `sha256` is absent. Never updates `data`.
pub(crate) async fn put_artifact(
    conn: &mut MySqlConnection,
    sha256: &str,
    bytes: &[u8],
) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual != sha256 {
        anyhow::bail!("artifact sha256 does not match payload bytes");
    }
    let size = i64::try_from(bytes.len()).context("artifact size")?;
    if let Some(existing) = artifact_size(conn, sha256).await? {
        return same_size_or_conflict(sha256, existing, size);
    }

    let inserted =
        sqlx::query("INSERT INTO skill_artifacts (sha256, size_bytes, data) VALUES (?, ?, ?)")
            .bind(sha256)
            .bind(size)
            .bind(bytes)
            .execute(&mut *conn)
            .await;

    match inserted {
        Ok(_) => Ok(()),
        Err(err) if mysql_is_duplicate(&err) => {
            let existing = artifact_size(conn, sha256)
                .await?
                .context("artifact missing after duplicate key")?;
            same_size_or_conflict(sha256, existing, size)
        }
        Err(err) => Err(err.into()),
    }
}

fn same_size_or_conflict(sha256: &str, existing: i64, size: i64) -> Result<()> {
    if existing == size {
        Ok(())
    } else {
        anyhow::bail!("artifact {sha256} already exists with different bytes");
    }
}

async fn artifact_size(conn: &mut MySqlConnection, sha256: &str) -> Result<Option<i64>> {
    let row = sqlx::query("SELECT size_bytes FROM skill_artifacts WHERE sha256 = ? FOR UPDATE")
        .bind(sha256)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some(row) => Ok(Some(row.try_get("size_bytes")?)),
        None => Ok(None),
    }
}

#[async_trait]
impl ArtifactStore for Db {
    async fn put(&self, sha256: &str, bytes: &[u8]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        put_artifact(tx.deref_mut(), sha256, bytes).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn get(&self, sha256: &str) -> Result<Option<Vec<u8>>> {
        let row = sqlx::query("SELECT data FROM skill_artifacts WHERE sha256 = ?")
            .bind(sha256)
            .fetch_optional(self.pool())
            .await?;
        match row {
            Some(row) => Ok(Some(row.try_get("data")?)),
            None => Ok(None),
        }
    }

    async fn exists(&self, sha256: &str) -> Result<bool> {
        let row = sqlx::query("SELECT sha256 FROM skill_artifacts WHERE sha256 = ?")
            .bind(sha256)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.is_some())
    }
}
