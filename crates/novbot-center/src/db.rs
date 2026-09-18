// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{mysql::MySqlPoolOptions, MySql, Pool, Row};
use std::collections::HashMap;

#[derive(Clone)]
pub struct Db {
    pool: Pool<MySql>,
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
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<()> {
        let sql = include_str!("../migrations/001_init.sql");
        for stmt in split_sql(sql) {
            sqlx::query(stmt)
                .execute(&self.pool)
                .await
                .with_context(|| format!("migrate: {}", &stmt[..stmt.len().min(60)]))?;
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
            VALUES (?, ?, ?, CAST(? AS JSON), CURRENT_TIMESTAMP(3))
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
            VALUES (?, 1, CAST('[]' AS JSON), CAST('[]' AS JSON))
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
            VALUES (?, 1, CAST(? AS JSON), CAST(? AS JSON))
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
            SET schedules_json = CAST(? AS JSON),
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
            VALUES (?, ?, ?, ?, CAST(? AS JSON), ?)
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

fn split_sql(sql: &str) -> Vec<&str> {
    sql.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}
