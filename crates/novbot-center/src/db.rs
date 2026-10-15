// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{mysql::MySqlPoolOptions, MySql, Pool, Row};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Claimed queue rows older than this are eligible to send again.
pub const PENDING_DISPATCH_ACK_TIMEOUT: Duration = Duration::from_secs(60);

/// One claimed `pending_dispatches` row. `params_json` is canonical JSON text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedDispatch {
    pub id: i64,
    pub run_id: String,
    pub spec_id: String,
    pub params_json: String,
}

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
    /// Boot-reported pairs from Register. Not the effective tag map.
    pub labels: Value,
    /// Effective tags: center, else valid node, else system.
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    /// Source of each effective tag (`center`, `node`, or `system`).
    #[serde(default)]
    pub tag_sources: BTreeMap<String, String>,
    /// Invalid node-declared pairs. They are not part of `tags`.
    #[serde(default)]
    pub invalid_tag_count: i64,
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
            (
                "005_skills_desired.sql",
                include_str!("../migrations/005_skills_desired.sql"),
            ),
            (
                "006_node_tags.sql",
                include_str!("../migrations/006_node_tags.sql"),
            ),
            (
                "007_target_resolutions.sql",
                include_str!("../migrations/007_target_resolutions.sql"),
            ),
            (
                "008_pending_dispatch_inflight.sql",
                include_str!("../migrations/008_pending_dispatch_inflight.sql"),
            ),
        ] {
            if name == "008_pending_dispatch_inflight.sql" {
                self.migrate_pending_inflight(sql).await?;
                continue;
            }
            for stmt in split_sql(sql) {
                sqlx::query(stmt)
                    .execute(&self.pool)
                    .await
                    .with_context(|| format!("migrate {name}: {}", &stmt[..stmt.len().min(60)]))?;
            }
        }
        crate::tags::backfill(self)
            .await
            .context("backfill node tags")?;
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

    /// Whether Register (`upsert_node`) has already created this node.
    ///
    /// `PUT /v1/nodes/:id/config` returns 404 when this is false. It must not
    /// insert a placeholder `nodes` row for an id that has never registered.
    pub async fn node_exists(&self, node_id: &str) -> Result<bool> {
        let row = sqlx::query("SELECT 1 FROM nodes WHERE node_id = ?")
            .bind(node_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
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
        let tag_rows =
            sqlx::query("SELECT node_id, tag_key, tag_value, source FROM node_tags_effective")
                .fetch_all(&self.pool)
                .await?;
        let mut grouped: HashMap<String, Vec<(String, String, String)>> = HashMap::new();
        for row in tag_rows {
            let node_id: String = row.try_get("node_id")?;
            grouped.entry(node_id).or_default().push((
                mysql_text(&row, "tag_key")?,
                mysql_text(&row, "tag_value")?,
                row.try_get("source")?,
            ));
        }
        let invalid_rows = sqlx::query(
            "SELECT node_id, COUNT(*) AS n FROM node_tags WHERE source = 'node' AND valid = 0 GROUP BY node_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut invalid_counts: HashMap<String, i64> = HashMap::new();
        for row in invalid_rows {
            invalid_counts.insert(row.try_get("node_id")?, row.try_get("n")?);
        }
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let labels_raw: String = r.try_get("labels_json")?;
            let node_id: String = r.try_get("node_id")?;
            let mut bucket = grouped.remove(&node_id).unwrap_or_default();
            bucket.sort_by_key(|(_, _, source)| match source.as_str() {
                "system" => 0,
                "node" => 1,
                "center" => 2,
                _ => 3,
            });
            let mut tags = BTreeMap::new();
            let mut tag_sources = BTreeMap::new();
            for (key, value, source) in bucket {
                tags.insert(key.clone(), value);
                tag_sources.insert(key, source);
            }
            out.push(NodeRow {
                invalid_tag_count: invalid_counts.get(&node_id).copied().unwrap_or(0),
                node_id,
                hostname: r.try_get("hostname")?,
                version: r.try_get("version")?,
                labels: serde_json::from_str(&labels_raw)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
                tags,
                tag_sources,
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

    /// Claim available or stale in-flight rows. Does not delete them.
    pub async fn claim_pending_dispatches(
        &self,
        node_id: &str,
        ack_timeout: Duration,
    ) -> Result<Vec<ClaimedDispatch>> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(ack_timeout).unwrap_or_else(|_| chrono::Duration::seconds(60));
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            r#"
            SELECT id, run_id, spec_id, params_json
            FROM pending_dispatches
            WHERE node_id = ?
              AND (in_flight_at IS NULL OR in_flight_at < ?)
            ORDER BY id ASC
            FOR UPDATE
            "#,
        )
        .bind(node_id)
        .bind(cutoff)
        .fetch_all(&mut *tx)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: i64 = r.try_get("id")?;
            sqlx::query(
                r#"
                UPDATE pending_dispatches
                SET in_flight_at = CURRENT_TIMESTAMP(3),
                    attempt_count = attempt_count + 1
                WHERE id = ?
                "#,
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            let run_id: String = r.try_get("run_id")?;
            let spec_id: String = r.try_get("spec_id")?;
            let params_raw: String = r.try_get("params_json")?;
            let params: Value = serde_json::from_str(&params_raw)
                .unwrap_or_else(|_| Value::Object(Default::default()));
            out.push(ClaimedDispatch {
                id,
                run_id,
                spec_id,
                params_json: serde_json::to_string(&params)?,
            });
        }
        tx.commit().await?;
        Ok(out)
    }

    /// Drop the queue row after the node reports a result for this run.
    pub async fn ack_pending_dispatch(&self, node_id: &str, run_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM pending_dispatches WHERE node_id = ? AND run_id = ?")
            .bind(node_id)
            .bind(run_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Clear `in_flight_at` so the rows can be claimed again. Keeps `attempt_count`.
    pub async fn requeue_pending_dispatches(&self, ids: &[i64]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE pending_dispatches SET in_flight_at = NULL WHERE id IN ({placeholders})"
        );
        let mut q = sqlx::query(&sql);
        for id in ids {
            q = q.bind(id);
        }
        q.execute(&self.pool).await?;
        Ok(())
    }

    /// Session ended or a new Register replaced the stream. Make in-flight rows available.
    pub async fn requeue_in_flight_for_node(&self, node_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE pending_dispatches
            SET in_flight_at = NULL
            WHERE node_id = ? AND in_flight_at IS NOT NULL
            "#,
        )
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn count_pending_dispatches(&self, node_id: &str) -> Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_dispatches WHERE node_id = ?",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    /// Rows that are not currently claimed (`in_flight_at` is NULL).
    pub async fn count_pending_available(&self, node_id: &str) -> Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pending_dispatches WHERE node_id = ? AND in_flight_at IS NULL",
        )
        .bind(node_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    async fn migrate_pending_inflight(&self, sql: &str) -> Result<()> {
        let exists: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM information_schema.COLUMNS
            WHERE TABLE_SCHEMA = DATABASE()
              AND TABLE_NAME = 'pending_dispatches'
              AND COLUMN_NAME = 'in_flight_at'
            "#,
        )
        .fetch_one(&self.pool)
        .await?;
        if exists > 0 {
            return Ok(());
        }
        for stmt in split_sql(sql) {
            // Text protocol: MySQL error 1295 rejects DDL on prepared statements.
            sqlx::raw_sql(stmt)
                .execute(&self.pool)
                .await
                .with_context(|| {
                    format!(
                        "migrate 008_pending_dispatch_inflight.sql: {}",
                        &stmt[..stmt.len().min(60)]
                    )
                })?;
        }
        Ok(())
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

/// `utf8mb4_bin` columns arrive as `VARBINARY`. Accept either that or a string.
pub(crate) fn mysql_text(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<String, sqlx::Error> {
    if let Ok(value) = row.try_get::<String, _>(column) {
        return Ok(value);
    }
    let bytes: Vec<u8> = row.try_get(column)?;
    String::from_utf8(bytes).map_err(|err| sqlx::Error::Decode(Box::new(err)))
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

    #[test]
    fn uploads_allowed_requires_packet_larger_than_16_mib() {
        assert!(!uploads_allowed(16 * 1024 * 1024));
        assert!(uploads_allowed(16 * 1024 * 1024 + 1));
        assert!(uploads_allowed(64 * 1024 * 1024));
    }

    async fn attempt_count(db: &Db, id: i64) -> i32 {
        sqlx::query_scalar("SELECT attempt_count FROM pending_dispatches WHERE id = ?")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .expect("attempt_count")
    }

    #[tokio::test]
    async fn claim_ack_requeue_and_stale_reclaim() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let node = format!("claim-{}", uuid::Uuid::new_v4());
        db.upsert_node(&node, "host", "test", &HashMap::new())
            .await
            .expect("upsert");
        let id = db
            .enqueue_dispatch(&node, "run-1", "spec", "{}")
            .await
            .expect("enqueue");
        assert_eq!(db.count_pending_available(&node).await.unwrap(), 1);

        let claimed = db
            .claim_pending_dispatches(&node, PENDING_DISPATCH_ACK_TIMEOUT)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);
        assert_eq!(claimed[0].run_id, "run-1");
        assert_eq!(db.count_pending_dispatches(&node).await.unwrap(), 1);
        assert_eq!(db.count_pending_available(&node).await.unwrap(), 0);
        assert_eq!(attempt_count(&db, id).await, 1);
        let again = db
            .claim_pending_dispatches(&node, PENDING_DISPATCH_ACK_TIMEOUT)
            .await
            .unwrap();
        assert!(again.is_empty(), "fresh in-flight row is not reclaimed");

        db.requeue_pending_dispatches(&[id]).await.expect("requeue");
        assert_eq!(db.count_pending_available(&node).await.unwrap(), 1);
        let claimed = db
            .claim_pending_dispatches(&node, PENDING_DISPATCH_ACK_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(attempt_count(&db, id).await, 2);

        sqlx::query(
            "UPDATE pending_dispatches SET in_flight_at = DATE_SUB(NOW(3), INTERVAL 120 SECOND) WHERE id = ?",
        )
        .bind(id)
        .execute(db.pool())
        .await
        .unwrap();
        let reclaimed = db
            .claim_pending_dispatches(&node, PENDING_DISPATCH_ACK_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(attempt_count(&db, id).await, 3);

        db.requeue_in_flight_for_node(&node).await.unwrap();
        assert_eq!(db.count_pending_available(&node).await.unwrap(), 1);
        db.ack_pending_dispatch(&node, "run-1").await.unwrap();
        assert_eq!(db.count_pending_dispatches(&node).await.unwrap(), 0);
        db.ack_pending_dispatch(&node, "missing").await.unwrap();
    }
}
