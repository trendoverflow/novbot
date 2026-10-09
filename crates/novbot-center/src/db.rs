// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{mysql::MySqlPoolOptions, MySql, Pool, Row};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Largest accepted `.nbskill` body. `max_allowed_packet` must be strictly larger:
/// the INSERT packet is the blob plus the SQL text.
pub const PACKAGE_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Uploads are allowed only when MySQL `max_allowed_packet` exceeds 16 MiB.
pub fn uploads_allowed(max_allowed_packet: u64) -> bool {
    max_allowed_packet > PACKAGE_MAX_BYTES
}

#[derive(Clone)]
pub struct Db {
    pool: Pool<MySql>,
    uploads_enabled: Arc<AtomicBool>,
    max_allowed_packet: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRow {
    pub node_id: String,
    pub hostname: String,
    pub version: String,
    pub labels: Value,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    pub node_id: String,
    pub config_generation: i64,
    pub specs_json: String,
    pub schedules_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultRow {
    pub id: i64,
    pub node_id: String,
    pub run_id: String,
    pub spec_id: String,
    pub status: String,
    pub payload: Value,
    pub observed_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

impl Db {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .context("connect mysql")?;
        Ok(Self {
            pool,
            uploads_enabled: Arc::new(AtomicBool::new(false)),
            max_allowed_packet: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(crate) fn pool(&self) -> &Pool<MySql> {
        &self.pool
    }

    pub fn uploads_enabled(&self) -> bool {
        self.uploads_enabled.load(Ordering::Relaxed)
    }

    pub fn max_allowed_packet(&self) -> u64 {
        self.max_allowed_packet.load(Ordering::Relaxed)
    }

    /// Read `@@GLOBAL.max_allowed_packet` and cache whether uploads may run.
    ///
    /// A packet that does not exceed 16 MiB disables uploads. It does not stop the process.
    pub async fn check_max_allowed_packet(&self) -> Result<()> {
        let value = read_max_allowed_packet(&self.pool).await?;
        let allowed = uploads_allowed(value);
        self.max_allowed_packet.store(value, Ordering::Relaxed);
        self.uploads_enabled.store(allowed, Ordering::Relaxed);
        if allowed {
            tracing::info!(max_allowed_packet = value, "skill uploads enabled");
        } else {
            tracing::error!(
                max_allowed_packet = value,
                limit = PACKAGE_MAX_BYTES,
                "skill uploads disabled: max_allowed_packet must be greater than 16 MiB"
            );
        }
        Ok(())
    }

    pub async fn migrate(&self) -> Result<()> {
        for (name, sql) in [
            ("001_init.sql", include_str!("../migrations/001_init.sql")),
            (
                "002_license_dispatch.sql",
                include_str!("../migrations/002_license_dispatch.sql"),
            ),
            (
                "003_json_to_longtext.sql",
                include_str!("../migrations/003_json_to_longtext.sql"),
            ),
            (
                "004_skill_catalog.sql",
                include_str!("../migrations/004_skill_catalog.sql"),
            ),
        ] {
            for stmt in split_sql(sql) {
                sqlx::query(stmt)
                    .execute(&self.pool)
                    .await
                    .with_context(|| format!("migrate {name}: {}", &stmt[..stmt.len().min(60)]))?;
            }
        }
        Ok(())
    }

    pub async fn upsert_node(
        &self,
        node_id: &str,
        hostname: &str,
        version: &str,
        labels: &HashMap<String, String>,
    ) -> Result<()> {
        let labels_json = serde_json::to_string(labels)?;
        sqlx::query(
            r#"
            INSERT INTO nodes (node_id, hostname, version, labels_json, last_seen_at)
            VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP(3))
            ON DUPLICATE KEY UPDATE
              hostname = VALUES(hostname),
              version = VALUES(version),
              labels_json = VALUES(labels_json),
              last_seen_at = CURRENT_TIMESTAMP(3)
            "#,
        )
        .bind(node_id)
        .bind(hostname)
        .bind(version)
        .bind(labels_json)
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            INSERT IGNORE INTO node_configs (node_id, config_generation, specs_json, schedules_json)
            VALUES (?, 1, '[]', '[]')
            "#,
        )
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Insert an empty `nodes` row when `node_id` is unknown.
    ///
    /// `node_configs.node_id` references `nodes`. `INSERT IGNORE` creates that
    /// parent row (hostname "", version "", labels `{}`, `last_seen_at` NULL)
    /// and does not change hostname, version, labels, or `last_seen_at` when
    /// the node has already registered.
    pub async fn ensure_node_placeholder(&self, node_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT IGNORE INTO nodes (node_id, hostname, version, labels_json, last_seen_at)
            VALUES (?, '', '', '{}', NULL)
            "#,
        )
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_node(&self, node_id: &str) -> Result<()> {
        sqlx::query("UPDATE nodes SET last_seen_at = CURRENT_TIMESTAMP(3) WHERE node_id = ?")
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn list_nodes(&self) -> Result<Vec<NodeRow>> {
        let rows = sqlx::query(
            "SELECT node_id, hostname, version, labels_json, last_seen_at FROM nodes ORDER BY node_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let labels_raw: String = r.try_get("labels_json")?;
            out.push(NodeRow {
                node_id: r.try_get("node_id")?,
                hostname: r.try_get("hostname")?,
                version: r.try_get("version")?,
                labels: serde_json::from_str(&labels_raw)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
                last_seen_at: r.try_get("last_seen_at")?,
            });
        }
        Ok(out)
    }

    pub async fn get_config(&self, node_id: &str) -> Result<Option<NodeConfig>> {
        let row = sqlx::query(
            "SELECT node_id, config_generation, specs_json, schedules_json FROM node_configs WHERE node_id = ?",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(decode_config(r)?),
            None => None,
        })
    }

    pub async fn put_config(
        &self,
        node_id: &str,
        specs_json: &str,
        schedules_json: Option<&str>,
    ) -> Result<NodeConfig> {
        let _: Value = serde_json::from_str(specs_json).context("specs_json")?;
        let schedules = match schedules_json {
            Some(s) => {
                let _: Value = serde_json::from_str(s).context("schedules_json")?;
                s.to_string()
            }
            None => self
                .get_config(node_id)
                .await?
                .map(|c| c.schedules_json)
                .unwrap_or_else(|| "[]".into()),
        };

        sqlx::query(
            r#"
            INSERT INTO node_configs (node_id, config_generation, specs_json, schedules_json)
            VALUES (?, 1, ?, ?)
            ON DUPLICATE KEY UPDATE
              config_generation = config_generation + 1,
              specs_json = VALUES(specs_json),
              schedules_json = VALUES(schedules_json)
            "#,
        )
        .bind(node_id)
        .bind(specs_json)
        .bind(&schedules)
        .execute(&self.pool)
        .await?;

        self.get_config(node_id)
            .await?
            .context("config missing after put")
    }

    pub async fn put_schedules(&self, node_id: &str, schedules_json: &str) -> Result<NodeConfig> {
        let _: Value = serde_json::from_str(schedules_json).context("schedules_json")?;
        let res = sqlx::query(
            r#"
            UPDATE node_configs
            SET schedules_json = ?,
                config_generation = config_generation + 1
            WHERE node_id = ?
            "#,
        )
        .bind(schedules_json)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            anyhow::bail!("node config not found; register node first");
        }
        self.get_config(node_id)
            .await?
            .context("config missing after schedule put")
    }

    pub async fn insert_result(
        &self,
        node_id: &str,
        run_id: &str,
        spec_id: &str,
        status: &str,
        payload_json: &str,
        observed_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO results (node_id, run_id, spec_id, status, payload_json, observed_at)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(node_id)
        .bind(run_id)
        .bind(spec_id)
        .bind(status)
        .bind(payload_json)
        .bind(observed_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_results(&self, node_id: Option<&str>, limit: i64) -> Result<Vec<ResultRow>> {
        let limit = limit.clamp(1, 500);
        let rows = if let Some(nid) = node_id {
            sqlx::query(
                r#"
                SELECT id, node_id, run_id, spec_id, status, payload_json, observed_at, received_at
                FROM results WHERE node_id = ? ORDER BY id DESC LIMIT ?
                "#,
            )
            .bind(nid)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT id, node_id, run_id, spec_id, status, payload_json, observed_at, received_at
                FROM results ORDER BY id DESC LIMIT ?
                "#,
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let payload_raw: String = r.try_get("payload_json")?;
            out.push(ResultRow {
                id: r.try_get("id")?,
                node_id: r.try_get("node_id")?,
                run_id: r.try_get("run_id")?,
                spec_id: r.try_get("spec_id")?,
                status: r.try_get("status")?,
                payload: serde_json::from_str(&payload_raw).unwrap_or(Value::Null),
                observed_at: r.try_get("observed_at")?,
                received_at: r.try_get("received_at")?,
            });
        }
        Ok(out)
    }

    pub async fn enqueue_dispatch(
        &self,
        node_id: &str,
        run_id: &str,
        spec_id: &str,
        params_json: &str,
    ) -> Result<i64> {
        let _: Value = serde_json::from_str(params_json).context("params_json")?;
        let res = sqlx::query(
            r#"
            INSERT INTO pending_dispatches (node_id, run_id, spec_id, params_json)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(node_id)
        .bind(run_id)
        .bind(spec_id)
        .bind(params_json)
        .execute(&self.pool)
        .await?;
        Ok(res.last_insert_id() as i64)
    }

    pub async fn take_pending_dispatches(
        &self,
        node_id: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let rows = sqlx::query(
            r#"
            SELECT id, run_id, spec_id, params_json
            FROM pending_dispatches WHERE node_id = ? ORDER BY id ASC
            "#,
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        let mut ids = Vec::new();
        for r in rows {
            let id: i64 = r.try_get("id")?;
            ids.push(id);
            let run_id: String = r.try_get("run_id")?;
            let spec_id: String = r.try_get("spec_id")?;
            let params_raw: String = r.try_get("params_json")?;
            let params: Value = serde_json::from_str(&params_raw)
                .unwrap_or_else(|_| Value::Object(Default::default()));
            out.push((run_id, spec_id, serde_json::to_string(&params)?));
        }
        for id in ids {
            sqlx::query("DELETE FROM pending_dispatches WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(out)
    }

    pub async fn get_license_key(&self) -> Result<String> {
        let row = sqlx::query("SELECT license_key FROM license_state WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => r.try_get::<String, _>("license_key").unwrap_or_default(),
            None => String::new(),
        })
    }

    pub async fn set_license_key(&self, key: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO license_state (id, license_key) VALUES (1, ?)
            ON DUPLICATE KEY UPDATE license_key = VALUES(license_key)
            "#,
        )
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn is_licensed(&self, env_key: Option<&str>) -> Result<bool> {
        if let Some(k) = env_key {
            if !k.is_empty() {
                return Ok(true);
            }
        }
        let key = self.get_license_key().await?;
        Ok(!key.trim().is_empty())
    }

    pub async fn config_generation(&self, node_id: &str) -> Result<i64> {
        Ok(self
            .get_config(node_id)
            .await?
            .map(|c| c.config_generation)
            .unwrap_or(0))
    }
}

fn decode_config(r: sqlx::mysql::MySqlRow) -> Result<NodeConfig> {
    let specs_raw: String = r.try_get("specs_json")?;
    let schedules_raw: String = r.try_get("schedules_json")?;
    let specs: Value = serde_json::from_str(&specs_raw).unwrap_or_else(|_| Value::Array(vec![]));
    let schedules: Value =
        serde_json::from_str(&schedules_raw).unwrap_or_else(|_| Value::Array(vec![]));
    Ok(NodeConfig {
        node_id: r.try_get("node_id")?,
        config_generation: r.try_get("config_generation")?,
        specs_json: serde_json::to_string(&specs)?,
        schedules_json: serde_json::to_string(&schedules)?,
    })
}

async fn read_max_allowed_packet(pool: &Pool<MySql>) -> Result<u64> {
    let row = sqlx::query("SELECT @@GLOBAL.max_allowed_packet AS max_allowed_packet")
        .fetch_one(pool)
        .await
        .context("read @@GLOBAL.max_allowed_packet")?;
    mysql_u64(&row, "max_allowed_packet").context("parse @@GLOBAL.max_allowed_packet")
}

fn mysql_u64(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<u64> {
    if let Ok(value) = row.try_get::<i64, _>(column) {
        return u64::try_from(value).context("negative integer");
    }
    if let Ok(value) = row.try_get::<u64, _>(column) {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<u32, _>(column) {
        return Ok(u64::from(value));
    }
    if let Ok(value) = row.try_get::<i32, _>(column) {
        return u64::try_from(value).context("negative integer");
    }
    if let Ok(value) = row.try_get::<String, _>(column) {
        return value.trim().parse().context("integer string");
    }
    if let Ok(value) = row.try_get::<Vec<u8>, _>(column) {
        let text = String::from_utf8(value).context("utf-8")?;
        return text.trim().parse().context("integer bytes");
    }
    anyhow::bail!("unsupported max_allowed_packet type")
}

fn split_sql(sql: &str) -> Vec<&str> {
    sql.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Connect and migrate for `#[cfg(test)]` MySQL tests.
///
/// Returns `None` (after a one-line skip notice) when `NOVBOT_TEST_DATABASE_URL`
/// is unset or empty. Migration DDL is serialized: these tests run in parallel
/// against one database, and concurrent `ALTER` deadlocks on MySQL.
#[cfg(test)]
pub(crate) async fn connect_test_db() -> Option<Db> {
    let url = std::env::var("NOVBOT_TEST_DATABASE_URL").unwrap_or_default();
    if url.trim().is_empty() {
        eprintln!("skip: NOVBOT_TEST_DATABASE_URL is unset or empty");
        return None;
    }
    static MIGRATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = MIGRATE.lock().await;
    let db = Db::connect(url.trim())
        .await
        .expect("connect NOVBOT_TEST_DATABASE_URL");
    db.migrate().await.expect("migrate");
    db.check_max_allowed_packet()
        .await
        .expect("max_allowed_packet");
    Some(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn uploads_allowed_requires_packet_larger_than_16_mib() {
        assert!(!uploads_allowed(16 * 1024 * 1024));
        assert!(uploads_allowed(16 * 1024 * 1024 + 1));
        assert!(uploads_allowed(64 * 1024 * 1024));
    }

    async fn fetch_node(db: &Db, id: &str) -> NodeRow {
        db.list_nodes()
            .await
            .expect("list nodes")
            .into_iter()
            .find(|n| n.node_id == id)
            .unwrap_or_else(|| panic!("missing node {id}"))
    }

    #[tokio::test]
    async fn ensure_node_placeholder_is_noop_for_existing_node() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = format!("t16-{}", uuid::Uuid::new_v4());
        let mut labels = HashMap::new();
        labels.insert("env".to_string(), "prod".to_string());
        labels.insert("role".to_string(), "api".to_string());
        db.upsert_node(&id, "host-c", "0.2.0", &labels)
            .await
            .expect("upsert");

        let before = fetch_node(&db, &id).await;
        assert_eq!(before.hostname, "host-c");
        assert_eq!(before.version, "0.2.0");
        assert_eq!(
            before.labels,
            serde_json::json!({"env": "prod", "role": "api"})
        );
        assert!(before.last_seen_at.is_some());

        // A timestamp write would move last_seen_at; INSERT IGNORE must not.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        db.ensure_node_placeholder(&id).await.expect("placeholder");

        let after = fetch_node(&db, &id).await;
        assert_eq!(after.hostname, before.hostname);
        assert_eq!(after.version, before.version);
        assert_eq!(after.labels, before.labels);
        assert_eq!(after.last_seen_at, before.last_seen_at);
    }
}
