// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node tag storage, Register replacement (TT-1), and center tag CRUD.
//!
//! Register replaces only `system` and `node` rows. Center rows survive
//! Register and `PUT /v1/nodes/{id}/config`.

use crate::artifact::sha256_hex;
use crate::db::Db;
use chrono::{DateTime, Utc};
use novbot_core::tags::{
    self, classify_declared_tags, normalize_system_value, validate_center_tag, CenterTagError,
    Selector, SelectorParse, MAX_CENTER_TAGS, MAX_TARGETS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{MySql, MySqlConnection, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::DerefMut;
use uuid::Uuid;

const ACTOR_TYPE: &str = "api_token";
const ACTOR_ID: &str = "api_token";

#[derive(Debug)]
pub(crate) enum TagError {
    KeyInvalid,
    ValueInvalid,
    KeyReserved,
    LimitExceeded,
    NodeUnknown,
    TagNotFound,
    TagNotCenter,
    TagConflict,
    Selector(tags::SelectorError),
    Prod {
        prod_node_ids: Vec<String>,
        prod_node_count: usize,
    },
    TooManyTargets,
    BadRequest(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for TagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.public_message())
    }
}

impl TagError {
    fn public_message(&self) -> &str {
        match self {
            TagError::KeyInvalid => "tag key is invalid",
            TagError::ValueInvalid => "tag value is invalid",
            TagError::KeyReserved => "tag key is reserved",
            TagError::LimitExceeded => "center tag limit exceeded",
            TagError::NodeUnknown => "unknown node",
            TagError::TagNotFound => "tag not found",
            TagError::TagNotCenter => "tag is not a center tag",
            TagError::TagConflict => "center tag value does not match if_value",
            TagError::Selector(err) => err.message.as_str(),
            TagError::Prod { .. } => "prod confirmation required",
            TagError::TooManyTargets => "too many targets",
            TagError::BadRequest(message) => message.as_str(),
            TagError::Internal(_) => "internal error",
        }
    }
}

fn db_err(err: sqlx::Error) -> TagError {
    TagError::Internal(err.into())
}

fn center_err(err: CenterTagError) -> TagError {
    match err {
        CenterTagError::KeyInvalid => TagError::KeyInvalid,
        CenterTagError::ValueInvalid => TagError::ValueInvalid,
        CenterTagError::KeyReserved => TagError::KeyReserved,
    }
}

#[derive(Debug, Clone)]
struct StoredTag {
    key: String,
    value: String,
    source: String,
    valid: bool,
    invalid_reason: Option<String>,
    updated_at: DateTime<Utc>,
    updated_by: String,
}

#[derive(Debug, Clone)]
struct Effective {
    value: String,
    source: String,
    overridden: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConfirmProd {
    pub node_count: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PutTagBody {
    pub value: String,
    #[serde(default)]
    pub if_value: Option<String>,
    #[serde(default)]
    pub confirm_prod: Option<ConfirmProd>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TagMutation {
    pub node_id: String,
    pub key: String,
    pub value: String,
    pub source: &'static str,
    pub overridden: bool,
    pub node_value: Option<String>,
    pub effective_value: String,
}

#[derive(Debug)]
pub(crate) struct SetTagResult {
    pub created: bool,
    pub tag: TagMutation,
}

#[derive(Debug, Serialize)]
pub(crate) struct TagRowView {
    pub key: String,
    pub value: String,
    pub source: String,
    pub effective: bool,
    pub overridden: bool,
    pub node_value: Option<String>,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct NodeTags {
    pub node_id: String,
    pub tags: Vec<TagRowView>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TagValueStat {
    pub value: String,
    pub nodes: i64,
    pub sources: BTreeMap<String, i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct TagKeyStat {
    pub key: String,
    pub values: Vec<TagValueStat>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RareValue {
    pub key: String,
    pub value: String,
    pub nodes: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct InvalidTag {
    pub node_id: String,
    pub key: String,
    pub value: String,
    pub invalid_reason: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TagOverview {
    pub keys: Vec<TagKeyStat>,
    pub missing: BTreeMap<String, i64>,
    pub rare_values: Vec<RareValue>,
    pub invalid: Vec<InvalidTag>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BulkBody {
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub add: BTreeMap<String, String>,
    #[serde(default)]
    pub remove: Vec<String>,
    pub dry_run: bool,
    #[serde(default)]
    pub confirm_prod: Option<ConfirmProd>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BulkItem {
    pub key: String,
    pub action: String,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BulkNode {
    pub node_id: String,
    pub results: Vec<BulkItem>,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct BulkCounts {
    pub added: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub not_changed_boot_reported: usize,
    pub not_found: usize,
    pub error: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct BulkResponse {
    pub operation_id: Option<String>,
    pub dry_run: bool,
    pub prod_node_ids: Vec<String>,
    pub prod_node_count: usize,
    pub counts: BulkCounts,
    pub per_node: Vec<BulkNode>,
}

struct PlanItem {
    key: String,
    action: &'static str,
    old_value: Option<String>,
    new_value: Option<String>,
    error: Option<String>,
    effective_before: Option<String>,
    effective_after: Option<String>,
    node_value: Option<String>,
    write: Option<String>,
}

struct NodePlan {
    node_id: String,
    found: bool,
    prod: bool,
    items: Vec<PlanItem>,
}

type Sig = BTreeMap<String, (String, bool, Option<String>)>;

/// Blank and `*` select every node. Anything else is a filter.
pub(crate) fn parse_list_selector(raw: Option<&str>) -> Result<Option<Selector>, TagError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    match tags::parse_selector(raw).map_err(TagError::Selector)? {
        SelectorParse::Empty | SelectorParse::All => Ok(None),
        SelectorParse::Filter(selector) => Ok(Some(selector)),
    }
}

pub(crate) async fn register_node(
    db: &Db,
    node_id: &str,
    hostname: &str,
    version: &str,
    declared: &HashMap<String, String>,
    os: &str,
    arch: &str,
) -> anyhow::Result<()> {
    let mut waits = 0u32;
    loop {
        match register_node_once(db, node_id, hostname, version, declared, os, arch).await {
            Ok(()) => return Ok(()),
            Err(err) if waits < 4 && is_deadlock(&err) => {
                waits += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * u64::from(waits))).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn register_node_once(
    db: &Db,
    node_id: &str,
    hostname: &str,
    version: &str,
    declared: &HashMap<String, String>,
    os: &str,
    arch: &str,
) -> anyhow::Result<()> {
    let classified = classify_declared_tags(declared);
    let mut tx = db.pool().begin().await?;
    let labels_json = serde_json::to_string(declared)?;
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
    .bind(&labels_json)
    .execute(tx.deref_mut())
    .await?;
    sqlx::query(
        r#"
        INSERT IGNORE INTO node_configs (node_id, config_generation, specs_json, schedules_json)
        VALUES (?, 1, '[]', '[]')
        "#,
    )
    .bind(node_id)
    .execute(tx.deref_mut())
    .await?;

    let existing = fetch_tags(tx.deref_mut(), node_id)
        .await
        .map_err(anyhow_tag)?;
    let old_sig = node_sig(&existing);
    sqlx::query("DELETE FROM node_tags WHERE node_id = ? AND source IN ('node', 'system')")
        .bind(node_id)
        .execute(tx.deref_mut())
        .await?;
    for tag in &classified {
        insert_tag(
            tx.deref_mut(),
            node_id,
            "node",
            &tag.key,
            &tag.value,
            tag.valid,
            tag.invalid_reason,
            "node",
        )
        .await?;
    }
    for (key, raw) in system_pairs(version, os, arch) {
        insert_tag(
            tx.deref_mut(),
            node_id,
            "system",
            key,
            &raw,
            true,
            None,
            "system",
        )
        .await?;
    }
    let new_sig = declared_sig(&classified);
    if let Some(detail) = node_reported_detail(&old_sig, &new_sig) {
        let detail_json = serde_json::to_string(&detail)?;
        append_audit(
            &mut tx,
            "node",
            "node",
            "tag.node_reported_changed",
            "node",
            node_id,
            Some(node_id),
            &detail_json,
        )
        .await
        .map_err(anyhow_tag)?;
    }
    tx.commit().await?;
    Ok(())
}

/// Idempotent startup backfill from `labels_json` and `version` (T4.4).
pub(crate) async fn backfill(db: &Db) -> anyhow::Result<()> {
    let rows = sqlx::query("SELECT node_id, version, labels_json FROM nodes")
        .fetch_all(db.pool())
        .await?;
    for row in rows {
        let node_id: String = row.try_get("node_id")?;
        let version: String = row.try_get("version")?;
        let labels_json: String = row.try_get("labels_json")?;
        backfill_one(db, &node_id, &version, &labels_json).await?;
    }
    Ok(())
}

async fn backfill_one(
    db: &Db,
    node_id: &str,
    version: &str,
    labels_json: &str,
) -> anyhow::Result<()> {
    let mut waits = 0u32;
    loop {
        match backfill_one_tx(db, node_id, version, labels_json).await {
            Ok(()) => return Ok(()),
            Err(err) if waits < 4 && is_deadlock(&err) => {
                waits += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20 * u64::from(waits))).await;
            }
            Err(err) => return Err(err),
        }
    }
}

async fn backfill_one_tx(
    db: &Db,
    node_id: &str,
    version: &str,
    labels_json: &str,
) -> anyhow::Result<()> {
    let mut tx = db.pool().begin().await?;
    let node_rows: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM node_tags WHERE node_id = ? AND source = 'node'")
            .bind(node_id)
            .fetch_one(tx.deref_mut())
            .await?
            .try_get("n")?;
    if node_rows == 0 {
        let declared = parse_label_object(labels_json);
        if !declared.is_empty() {
            for tag in classify_declared_tags(&declared) {
                insert_tag(
                    tx.deref_mut(),
                    node_id,
                    "node",
                    &tag.key,
                    &tag.value,
                    tag.valid,
                    tag.invalid_reason,
                    "migration",
                )
                .await?;
            }
        }
    }
    let sys_rows: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM node_tags WHERE node_id = ? AND source = 'system' AND tag_key = 'sys.version'",
    )
    .bind(node_id)
    .fetch_one(tx.deref_mut())
    .await?
    .try_get("n")?;
    if sys_rows == 0 && !version.is_empty() && version != "http-admin" {
        if let Some(value) = normalize_system_value(version) {
            insert_tag(
                tx.deref_mut(),
                node_id,
                "system",
                "sys.version",
                &value,
                true,
                None,
                "migration",
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn node_tags(db: &Db, node_id: &str) -> Result<NodeTags, TagError> {
    let mut conn = acquire(db).await?;
    if !node_exists(&mut conn, node_id).await? {
        return Err(TagError::NodeUnknown);
    }
    let rows = fetch_tags(&mut conn, node_id).await?;
    Ok(NodeTags {
        node_id: node_id.to_string(),
        tags: view_rows(&rows),
    })
}

pub(crate) async fn set_center_tag(
    db: &Db,
    node_id: &str,
    key: &str,
    body: PutTagBody,
) -> Result<SetTagResult, TagError> {
    validate_center_tag(key, &body.value).map_err(center_err)?;
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    if !lock_node(tx.deref_mut(), node_id).await? {
        return Err(TagError::NodeUnknown);
    }
    let rows = fetch_tags(tx.deref_mut(), node_id).await?;
    let current = center_value(&rows, key);
    if let Some(expected) = &body.if_value {
        if current.as_deref() != Some(expected.as_str()) {
            return Err(TagError::TagConflict);
        }
    }
    let created = current.is_none();
    if created && center_count(&rows) >= MAX_CENTER_TAGS {
        return Err(TagError::LimitExceeded);
    }
    let overlay = BTreeMap::from([(key.to_string(), Some(body.value.clone()))]);
    ensure_single_prod(node_id, &rows, &overlay, body.confirm_prod.as_ref())?;
    insert_tag(
        tx.deref_mut(),
        node_id,
        "center",
        key,
        &body.value,
        true,
        None,
        ACTOR_ID,
    )
    .await
    .map_err(anyhow_tag_to_tag)?;
    let before = effective_of(&rows);
    let after = effective_of(&apply_overlay(&rows, &overlay));
    let node_value = valid_node_value(&rows, key);
    let prod_confirmed = counts_as_prod(&rows) || counts_as_prod(&apply_overlay(&rows, &overlay));
    let detail = json!({
        "key": key,
        "old_value": current,
        "new_value": body.value,
        "effective_before": before.get(key).map(|item| &item.value),
        "effective_after": after.get(key).map(|item| &item.value),
        "node_value": node_value,
        "operation_id": Value::Null,
        "prod_confirmed": prod_confirmed,
    });
    let detail_json =
        serde_json::to_string(&detail).map_err(|err| TagError::Internal(err.into()))?;
    append_audit(
        &mut tx,
        ACTOR_TYPE,
        ACTOR_ID,
        "tag.set",
        "node",
        node_id,
        Some(node_id),
        &detail_json,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    let eff = after.get(key);
    Ok(SetTagResult {
        created,
        tag: TagMutation {
            node_id: node_id.to_string(),
            key: key.to_string(),
            value: body.value,
            source: "center",
            overridden: eff.is_some_and(|item| item.overridden),
            node_value,
            effective_value: eff.map(|item| item.value.clone()).unwrap_or_default(),
        },
    })
}

pub(crate) async fn delete_center_tag(
    db: &Db,
    node_id: &str,
    key: &str,
    confirm_prod_count: Option<u64>,
) -> Result<(), TagError> {
    if let Err(err) = validate_center_tag(key, "x") {
        return Err(center_err(match err {
            CenterTagError::ValueInvalid => CenterTagError::KeyInvalid,
            other => other,
        }));
    }
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    if !lock_node(tx.deref_mut(), node_id).await? {
        return Err(TagError::TagNotFound);
    }
    let rows = fetch_tags(tx.deref_mut(), node_id).await?;
    let has_key = rows.iter().any(|row| row.key == key);
    let current = center_value(&rows, key);
    if current.is_none() {
        return Err(if has_key {
            TagError::TagNotCenter
        } else {
            TagError::TagNotFound
        });
    }
    let overlay = BTreeMap::from([(key.to_string(), None)]);
    let confirm = confirm_prod_count.map(|node_count| ConfirmProd { node_count });
    ensure_single_prod(node_id, &rows, &overlay, confirm.as_ref())?;
    sqlx::query("DELETE FROM node_tags WHERE node_id = ? AND source = 'center' AND tag_key = ?")
        .bind(node_id)
        .bind(key)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let after = effective_of(&apply_overlay(&rows, &overlay));
    let prod_confirmed = counts_as_prod(&rows) || counts_as_prod(&apply_overlay(&rows, &overlay));
    let detail = json!({
        "key": key,
        "old_value": current,
        "effective_after": after.get(key).map(|item| &item.value),
        "operation_id": Value::Null,
        "prod_confirmed": prod_confirmed,
    });
    let detail_json =
        serde_json::to_string(&detail).map_err(|err| TagError::Internal(err.into()))?;
    append_audit(
        &mut tx,
        ACTOR_TYPE,
        ACTOR_ID,
        "tag.removed",
        "node",
        node_id,
        Some(node_id),
        &detail_json,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    Ok(())
}

pub(crate) async fn bulk(db: &Db, body: BulkBody) -> Result<BulkResponse, TagError> {
    let add = body.add;
    let remove = normalize_remove(&body.remove)?;
    validate_bulk_keys(&add, &remove)?;
    let ids = normalize_ids(&body.node_ids)?;
    if body.dry_run {
        let mut conn = acquire(db).await?;
        let plans = load_plans(&mut conn, &ids, &add, &remove, false).await?;
        return Ok(bulk_response(None, true, &plans));
    }
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let plans = load_plans(tx.deref_mut(), &ids, &add, &remove, true).await?;
    let prod_ids = prod_ids_of(&plans);
    ensure_prod(&prod_ids, body.confirm_prod.as_ref())?;
    let operation_id = Uuid::new_v4().to_string();
    let prod_confirmed = !prod_ids.is_empty();
    for plan in &plans {
        apply_plan(tx.deref_mut(), plan, &operation_id, prod_confirmed).await?;
    }
    let response = bulk_response(Some(operation_id.clone()), false, &plans);
    let detail = json!({
        "resolution_id": Value::Null,
        "add": add,
        "remove": remove,
        "counts": &response.counts,
        "prod_confirmed": prod_confirmed,
    });
    let detail_json =
        serde_json::to_string(&detail).map_err(|err| TagError::Internal(err.into()))?;
    append_audit(
        &mut tx,
        ACTOR_TYPE,
        ACTOR_ID,
        "tag.bulk_applied",
        "tag_bulk",
        &operation_id,
        None,
        &detail_json,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    Ok(response)
}

pub(crate) async fn overview(db: &Db) -> Result<TagOverview, TagError> {
    let mut conn = acquire(db).await?;
    let node_rows = sqlx::query("SELECT node_id FROM nodes")
        .fetch_all(&mut *conn)
        .await
        .map_err(db_err)?;
    let mut node_ids = Vec::with_capacity(node_rows.len());
    for row in node_rows {
        node_ids.push(row.try_get::<String, _>("node_id").map_err(db_err)?);
    }
    let tag_rows = sqlx::query(
        r#"SELECT node_id, source, tag_key, tag_value, valid, invalid_reason
           FROM node_tags"#,
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(db_err)?;
    let mut by_node: BTreeMap<String, Vec<StoredTag>> = BTreeMap::new();
    let mut invalid = Vec::new();
    let now = Utc::now();
    for row in tag_rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        let source: String = row.try_get("source").map_err(db_err)?;
        let key = crate::db::mysql_text(&row, "tag_key").map_err(db_err)?;
        let value = crate::db::mysql_text(&row, "tag_value").map_err(db_err)?;
        let valid = read_bool(&row, "valid")?;
        let invalid_reason: Option<String> = row.try_get("invalid_reason").map_err(db_err)?;
        if source == "node" && !valid {
            invalid.push(InvalidTag {
                node_id: node_id.clone(),
                key: key.clone(),
                value: value.clone(),
                invalid_reason: invalid_reason.clone().unwrap_or_default(),
            });
        }
        by_node.entry(node_id).or_default().push(StoredTag {
            key,
            value,
            source,
            valid,
            invalid_reason,
            updated_at: now,
            updated_by: String::new(),
        });
    }
    let mut counts: BTreeMap<(String, String), (i64, BTreeMap<String, i64>)> = BTreeMap::new();
    let mut missing_env = 0i64;
    let mut missing_role = 0i64;
    for node_id in &node_ids {
        let empty = Vec::new();
        let rows = by_node.get(node_id).unwrap_or(&empty);
        let effective = effective_of(rows);
        if !effective.contains_key("env") {
            missing_env += 1;
        }
        if !effective.contains_key("role") {
            missing_role += 1;
        }
        for (key, item) in effective {
            let entry = counts
                .entry((key, item.value))
                .or_insert_with(|| (0, BTreeMap::new()));
            entry.0 += 1;
            *entry.1.entry(item.source).or_insert(0) += 1;
        }
    }
    let mut grouped: BTreeMap<String, Vec<TagValueStat>> = BTreeMap::new();
    let mut rare = Vec::new();
    for ((key, value), (nodes, sources)) in counts {
        if nodes == 1 {
            rare.push(RareValue {
                key: key.clone(),
                value: value.clone(),
                nodes,
            });
        }
        grouped.entry(key).or_default().push(TagValueStat {
            value,
            nodes,
            sources,
        });
    }
    let keys = grouped
        .into_iter()
        .map(|(key, mut values)| {
            values.sort_by(|a, b| a.value.cmp(&b.value));
            TagKeyStat { key, values }
        })
        .collect();
    invalid.sort_by(|a, b| {
        a.node_id
            .cmp(&b.node_id)
            .then(a.key.cmp(&b.key))
            .then(a.value.cmp(&b.value))
    });
    rare.sort_by(|a, b| a.key.cmp(&b.key).then(a.value.cmp(&b.value)));
    let mut missing = BTreeMap::new();
    missing.insert("env".into(), missing_env);
    missing.insert("role".into(), missing_role);
    Ok(TagOverview {
        keys,
        missing,
        rare_values: rare,
        invalid,
    })
}

fn system_pairs(version: &str, os: &str, arch: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(value) = normalize_system_value(version) {
        out.push(("sys.version", value));
    }
    if let Some(value) = normalize_system_value(os) {
        out.push(("sys.os", value));
    }
    if let Some(value) = normalize_system_value(arch) {
        out.push(("sys.arch", value));
    }
    out
}

fn parse_label_object(raw: &str) -> HashMap<String, String> {
    let Ok(Value::Object(map)) = serde_json::from_str(raw) else {
        return HashMap::new();
    };
    map.into_iter()
        .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_string())))
        .collect()
}

fn node_sig(rows: &[StoredTag]) -> Sig {
    rows.iter()
        .filter(|row| row.source == "node")
        .map(|row| {
            (
                row.key.clone(),
                (row.value.clone(), row.valid, row.invalid_reason.clone()),
            )
        })
        .collect()
}

fn declared_sig(tags: &[tags::DeclaredTag]) -> Sig {
    tags.iter()
        .map(|tag| {
            (
                tag.key.clone(),
                (
                    tag.value.clone(),
                    tag.valid,
                    tag.invalid_reason.map(str::to_string),
                ),
            )
        })
        .collect()
}

fn node_reported_detail(old: &Sig, new: &Sig) -> Option<Value> {
    if old == new {
        return None;
    }
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut removed = Vec::new();
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(old.keys().map(String::as_str));
    keys.extend(new.keys().map(String::as_str));
    for key in keys {
        match (old.get(key), new.get(key)) {
            (None, Some((value, valid, reason))) => added.push(json!({
                "key": key,
                "value": value,
                "valid": valid,
                "invalid_reason": reason,
            })),
            (Some((value, valid, reason)), None) => removed.push(json!({
                "key": key,
                "value": value,
                "valid": valid,
                "invalid_reason": reason,
            })),
            (Some(before), Some(after)) if before != after => changed.push(json!({
                "key": key,
                "old_value": before.0,
                "new_value": after.0,
                "valid": after.1,
                "invalid_reason": after.2,
            })),
            _ => {}
        }
    }
    let invalid: Vec<_> = new
        .iter()
        .filter(|(_, (_, valid, _))| !valid)
        .map(|(key, (value, _, reason))| {
            json!({
                "key": key,
                "value": value,
                "invalid_reason": reason,
            })
        })
        .collect();
    Some(json!({
        "added": added,
        "changed": changed,
        "removed": removed,
        "invalid": invalid,
    }))
}

fn effective_of(rows: &[StoredTag]) -> BTreeMap<String, Effective> {
    let mut keys = BTreeSet::new();
    for row in rows {
        if row.valid {
            keys.insert(row.key.clone());
        }
    }
    let mut out = BTreeMap::new();
    for key in keys {
        let center = find_source(rows, &key, "center");
        let node = find_source(rows, &key, "node");
        let system = find_source(rows, &key, "system");
        let Some(chosen) = center.or(node).or(system) else {
            continue;
        };
        let node_value = node.map(|row| row.value.clone());
        out.insert(
            key,
            Effective {
                value: chosen.value.clone(),
                source: chosen.source.clone(),
                overridden: chosen.source == "center" && node_value.is_some(),
            },
        );
    }
    out
}

fn find_source<'a>(rows: &'a [StoredTag], key: &str, source: &str) -> Option<&'a StoredTag> {
    rows.iter()
        .find(|row| row.valid && row.key == key && row.source == source)
}

fn valid_node_value(rows: &[StoredTag], key: &str) -> Option<String> {
    find_source(rows, key, "node").map(|row| row.value.clone())
}

fn center_value(rows: &[StoredTag], key: &str) -> Option<String> {
    rows.iter()
        .find(|row| row.source == "center" && row.key == key)
        .map(|row| row.value.clone())
}

fn center_count(rows: &[StoredTag]) -> usize {
    rows.iter().filter(|row| row.source == "center").count()
}

fn counts_as_prod(rows: &[StoredTag]) -> bool {
    let invalid_prod = rows.iter().any(|row| {
        row.source == "node"
            && !row.valid
            && row.key.eq_ignore_ascii_case("env")
            && row.value.eq_ignore_ascii_case("prod")
    });
    if invalid_prod {
        return true;
    }
    effective_of(rows)
        .get("env")
        .is_some_and(|item| item.value == "prod")
}

fn apply_overlay(rows: &[StoredTag], overlay: &BTreeMap<String, Option<String>>) -> Vec<StoredTag> {
    let mut out = rows.to_vec();
    for (key, value) in overlay {
        out.retain(|row| !(row.source == "center" && row.key == *key));
        if let Some(value) = value {
            out.push(StoredTag {
                key: key.clone(),
                value: value.clone(),
                source: "center".into(),
                valid: true,
                invalid_reason: None,
                updated_at: Utc::now(),
                updated_by: ACTOR_ID.into(),
            });
        }
    }
    out
}

fn ensure_single_prod(
    node_id: &str,
    rows: &[StoredTag],
    overlay: &BTreeMap<String, Option<String>>,
    confirm: Option<&ConfirmProd>,
) -> Result<(), TagError> {
    let prod = counts_as_prod(rows) || counts_as_prod(&apply_overlay(rows, overlay));
    if !prod {
        return Ok(());
    }
    ensure_prod(&[node_id.to_string()], confirm)
}

fn ensure_prod(ids: &[String], confirm: Option<&ConfirmProd>) -> Result<(), TagError> {
    if ids.is_empty() {
        return Ok(());
    }
    if confirm.map(|item| item.node_count) != Some(ids.len() as u64) {
        return Err(TagError::Prod {
            prod_node_ids: ids.to_vec(),
            prod_node_count: ids.len(),
        });
    }
    Ok(())
}

fn view_rows(rows: &[StoredTag]) -> Vec<TagRowView> {
    let effective = effective_of(rows);
    let mut views: Vec<TagRowView> = rows
        .iter()
        .map(|row| {
            let eff = effective.get(&row.key);
            let node_value = valid_node_value(rows, &row.key);
            let is_effective = row.valid
                && eff.is_some_and(|item| item.source == row.source && item.value == row.value);
            TagRowView {
                key: row.key.clone(),
                value: row.value.clone(),
                source: row.source.clone(),
                effective: is_effective,
                overridden: row.source == "center" && node_value.is_some(),
                node_value,
                valid: row.valid,
                invalid_reason: row.invalid_reason.clone(),
                updated_at: row.updated_at,
                updated_by: row.updated_by.clone(),
            }
        })
        .collect();
    views.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then(source_rank(&a.source).cmp(&source_rank(&b.source)))
    });
    views
}

fn source_rank(source: &str) -> u8 {
    match source {
        "center" => 0,
        "node" => 1,
        "system" => 2,
        _ => 9,
    }
}

fn normalize_ids(ids: &[String]) -> Result<Vec<String>, TagError> {
    let mut out: Vec<String> = ids
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    out.sort();
    out.dedup();
    if out.is_empty() {
        return Err(TagError::BadRequest("node_ids is required".into()));
    }
    if out.len() > MAX_TARGETS {
        return Err(TagError::TooManyTargets);
    }
    Ok(out)
}

fn normalize_remove(keys: &[String]) -> Result<Vec<String>, TagError> {
    let mut out: Vec<String> = keys
        .iter()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

fn validate_bulk_keys(add: &BTreeMap<String, String>, remove: &[String]) -> Result<(), TagError> {
    if add.is_empty() && remove.is_empty() {
        return Err(TagError::BadRequest("add or remove is required".into()));
    }
    for (key, value) in add {
        validate_center_tag(key, value).map_err(center_err)?;
    }
    for key in remove {
        validate_center_tag(key, "x").map_err(center_err)?;
        if add.contains_key(key) {
            return Err(TagError::BadRequest(format!(
                "key `{key}` is in both add and remove"
            )));
        }
    }
    Ok(())
}

fn plan_node(
    rows: &[StoredTag],
    node_id: &str,
    found: bool,
    add: &BTreeMap<String, String>,
    remove: &[String],
) -> NodePlan {
    if !found {
        return NodePlan {
            node_id: node_id.to_string(),
            found: false,
            prod: false,
            items: vec![PlanItem {
                key: String::new(),
                action: "not_found",
                old_value: None,
                new_value: None,
                error: None,
                effective_before: None,
                effective_after: None,
                node_value: None,
                write: None,
            }],
        };
    }
    let before = effective_of(rows);
    let mut center_keys: BTreeSet<String> = rows
        .iter()
        .filter(|row| row.source == "center")
        .map(|row| row.key.clone())
        .collect();
    let mut items = Vec::new();
    let mut overlay: BTreeMap<String, Option<String>> = BTreeMap::new();
    for key in remove {
        let old = center_value(rows, key);
        if let Some(old) = old {
            center_keys.remove(key);
            overlay.insert(key.clone(), None);
            items.push(PlanItem {
                key: key.clone(),
                action: "removed",
                old_value: Some(old),
                new_value: None,
                error: None,
                effective_before: None,
                effective_after: None,
                node_value: valid_node_value(rows, key),
                write: None,
            });
        } else if rows
            .iter()
            .any(|row| row.key == *key && row.source == "node")
        {
            items.push(PlanItem {
                key: key.clone(),
                action: "not_changed_boot_reported",
                old_value: rows
                    .iter()
                    .find(|row| row.key == *key && row.source == "node")
                    .map(|row| row.value.clone()),
                new_value: None,
                error: None,
                effective_before: before.get(key).map(|item| item.value.clone()),
                effective_after: before.get(key).map(|item| item.value.clone()),
                node_value: valid_node_value(rows, key),
                write: None,
            });
        } else {
            items.push(PlanItem {
                key: key.clone(),
                action: "not_found",
                old_value: None,
                new_value: None,
                error: None,
                effective_before: None,
                effective_after: None,
                node_value: None,
                write: None,
            });
        }
    }
    for (key, value) in add {
        let old = center_value(rows, key);
        match old {
            Some(old) if old == *value => items.push(PlanItem {
                key: key.clone(),
                action: "unchanged",
                old_value: Some(old),
                new_value: Some(value.clone()),
                error: None,
                effective_before: before.get(key).map(|item| item.value.clone()),
                effective_after: before.get(key).map(|item| item.value.clone()),
                node_value: valid_node_value(rows, key),
                write: None,
            }),
            Some(old) => {
                overlay.insert(key.clone(), Some(value.clone()));
                items.push(PlanItem {
                    key: key.clone(),
                    action: "changed",
                    old_value: Some(old),
                    new_value: Some(value.clone()),
                    error: None,
                    effective_before: None,
                    effective_after: None,
                    node_value: valid_node_value(rows, key),
                    write: Some(value.clone()),
                });
            }
            None if center_keys.len() >= MAX_CENTER_TAGS => items.push(PlanItem {
                key: key.clone(),
                action: "error",
                old_value: None,
                new_value: Some(value.clone()),
                error: Some("tag_limit_exceeded".into()),
                effective_before: before.get(key).map(|item| item.value.clone()),
                effective_after: before.get(key).map(|item| item.value.clone()),
                node_value: valid_node_value(rows, key),
                write: None,
            }),
            None => {
                center_keys.insert(key.clone());
                overlay.insert(key.clone(), Some(value.clone()));
                items.push(PlanItem {
                    key: key.clone(),
                    action: "added",
                    old_value: None,
                    new_value: Some(value.clone()),
                    error: None,
                    effective_before: None,
                    effective_after: None,
                    node_value: valid_node_value(rows, key),
                    write: Some(value.clone()),
                });
            }
        }
    }
    let after = effective_of(&apply_overlay(rows, &overlay));
    for item in &mut items {
        if item.action == "added" || item.action == "changed" || item.action == "removed" {
            item.effective_before = before.get(&item.key).map(|eff| eff.value.clone());
            item.effective_after = after.get(&item.key).map(|eff| eff.value.clone());
        }
    }
    let prod = counts_as_prod(rows) || counts_as_prod(&apply_overlay(rows, &overlay));
    NodePlan {
        node_id: node_id.to_string(),
        found: true,
        prod,
        items,
    }
}

async fn load_plans(
    conn: &mut MySqlConnection,
    ids: &[String],
    add: &BTreeMap<String, String>,
    remove: &[String],
    lock: bool,
) -> Result<Vec<NodePlan>, TagError> {
    let mut plans = Vec::with_capacity(ids.len());
    for id in ids {
        let found = if lock {
            lock_node(conn, id).await?
        } else {
            node_exists(conn, id).await?
        };
        let rows = if found {
            fetch_tags(conn, id).await?
        } else {
            Vec::new()
        };
        plans.push(plan_node(&rows, id, found, add, remove));
    }
    Ok(plans)
}

fn prod_ids_of(plans: &[NodePlan]) -> Vec<String> {
    plans
        .iter()
        .filter(|plan| plan.prod)
        .map(|plan| plan.node_id.clone())
        .collect()
}

fn bulk_response(operation_id: Option<String>, dry_run: bool, plans: &[NodePlan]) -> BulkResponse {
    let mut counts = BulkCounts::default();
    let mut per_node = Vec::with_capacity(plans.len());
    for plan in plans {
        let mut results = Vec::with_capacity(plan.items.len());
        for item in &plan.items {
            match item.action {
                "added" => counts.added += 1,
                "changed" => counts.changed += 1,
                "unchanged" => counts.unchanged += 1,
                "removed" => counts.removed += 1,
                "not_changed_boot_reported" => counts.not_changed_boot_reported += 1,
                "not_found" => counts.not_found += 1,
                "error" => counts.error += 1,
                _ => {}
            }
            results.push(BulkItem {
                key: item.key.clone(),
                action: item.action.to_string(),
                old_value: item.old_value.clone(),
                new_value: item.new_value.clone(),
                error: item.error.clone(),
            });
        }
        per_node.push(BulkNode {
            node_id: plan.node_id.clone(),
            results,
        });
    }
    let prod_node_ids = prod_ids_of(plans);
    let prod_node_count = prod_node_ids.len();
    BulkResponse {
        operation_id,
        dry_run,
        prod_node_ids,
        prod_node_count,
        counts,
        per_node,
    }
}

async fn apply_plan(
    conn: &mut MySqlConnection,
    plan: &NodePlan,
    operation_id: &str,
    prod_confirmed: bool,
) -> Result<(), TagError> {
    if !plan.found {
        return Ok(());
    }
    for item in &plan.items {
        match item.action {
            "added" | "changed" => {
                let Some(value) = &item.write else {
                    continue;
                };
                insert_tag(
                    conn,
                    &plan.node_id,
                    "center",
                    &item.key,
                    value,
                    true,
                    None,
                    ACTOR_ID,
                )
                .await
                .map_err(anyhow_tag_to_tag)?;
                let detail = json!({
                    "key": item.key,
                    "old_value": item.old_value,
                    "new_value": value,
                    "effective_before": item.effective_before,
                    "effective_after": item.effective_after,
                    "node_value": item.node_value,
                    "operation_id": operation_id,
                    "prod_confirmed": prod_confirmed,
                });
                audit_on(
                    conn,
                    "tag.set",
                    &plan.node_id,
                    &serde_json::to_string(&detail)
                        .map_err(|err| TagError::Internal(err.into()))?,
                )
                .await?;
            }
            "removed" => {
                sqlx::query(
                    "DELETE FROM node_tags WHERE node_id = ? AND source = 'center' AND tag_key = ?",
                )
                .bind(&plan.node_id)
                .bind(&item.key)
                .execute(&mut *conn)
                .await
                .map_err(db_err)?;
                let detail = json!({
                    "key": item.key,
                    "old_value": item.old_value,
                    "effective_after": item.effective_after,
                    "operation_id": operation_id,
                    "prod_confirmed": prod_confirmed,
                });
                audit_on(
                    conn,
                    "tag.removed",
                    &plan.node_id,
                    &serde_json::to_string(&detail)
                        .map_err(|err| TagError::Internal(err.into()))?,
                )
                .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn audit_on(
    conn: &mut MySqlConnection,
    action: &str,
    node_id: &str,
    detail_json: &str,
) -> Result<(), TagError> {
    let prev = sqlx::query("SELECT `hash` FROM audit_events ORDER BY id DESC LIMIT 1")
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    let prev_hash = match prev {
        Some(row) => row.try_get::<String, _>("hash").map_err(db_err)?,
        None => String::new(),
    };
    let hash = audit_hash(&prev_hash, action, node_id, detail_json);
    let stored_prev = if prev_hash.is_empty() {
        None
    } else {
        Some(prev_hash)
    };
    sqlx::query(
        r#"
        INSERT INTO audit_events (
            `at`, actor_type, actor_id, action, target_type, target_id, node_id,
            detail_json, prev_hash, `hash`
        ) VALUES (
            CURRENT_TIMESTAMP(3), ?, ?, ?, 'node', ?, ?, ?, ?, ?
        )
        "#,
    )
    .bind(ACTOR_TYPE)
    .bind(ACTOR_ID)
    .bind(action)
    .bind(node_id)
    .bind(node_id)
    .bind(detail_json)
    .bind(stored_prev)
    .bind(hash)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

async fn acquire(db: &Db) -> Result<sqlx::pool::PoolConnection<MySql>, TagError> {
    db.pool().acquire().await.map_err(db_err)
}

async fn node_exists(conn: &mut MySqlConnection, node_id: &str) -> Result<bool, TagError> {
    let row = sqlx::query("SELECT node_id FROM nodes WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    Ok(row.is_some())
}

async fn lock_node(conn: &mut MySqlConnection, node_id: &str) -> Result<bool, TagError> {
    let row = sqlx::query("SELECT node_id FROM nodes WHERE node_id = ? FOR UPDATE")
        .bind(node_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    Ok(row.is_some())
}

async fn fetch_tags(conn: &mut MySqlConnection, node_id: &str) -> Result<Vec<StoredTag>, TagError> {
    let rows = sqlx::query(
        r#"SELECT source, tag_key, tag_value, valid, invalid_reason, updated_at, updated_by
           FROM node_tags WHERE node_id = ?"#,
    )
    .bind(node_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(db_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(StoredTag {
            key: crate::db::mysql_text(&row, "tag_key").map_err(db_err)?,
            value: crate::db::mysql_text(&row, "tag_value").map_err(db_err)?,
            source: row.try_get("source").map_err(db_err)?,
            valid: read_bool(&row, "valid")?,
            invalid_reason: row.try_get("invalid_reason").map_err(db_err)?,
            updated_at: row.try_get("updated_at").map_err(db_err)?,
            updated_by: row.try_get("updated_by").map_err(db_err)?,
        });
    }
    Ok(out)
}

fn read_bool(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<bool, TagError> {
    if let Ok(value) = row.try_get::<bool, _>(column) {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<i8, _>(column) {
        return Ok(value != 0);
    }
    let value: i64 = row.try_get(column).map_err(db_err)?;
    Ok(value != 0)
}

async fn insert_tag(
    conn: &mut MySqlConnection,
    node_id: &str,
    source: &str,
    key: &str,
    value: &str,
    valid: bool,
    invalid_reason: Option<&str>,
    updated_by: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO node_tags (
            node_id, source, tag_key, tag_value, valid, invalid_reason, updated_by
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
        ON DUPLICATE KEY UPDATE
          tag_value = VALUES(tag_value),
          valid = VALUES(valid),
          invalid_reason = VALUES(invalid_reason),
          updated_by = VALUES(updated_by)
        "#,
    )
    .bind(node_id)
    .bind(source)
    .bind(key)
    .bind(value)
    .bind(if valid { 1i8 } else { 0i8 })
    .bind(invalid_reason)
    .bind(updated_by)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn append_audit(
    tx: &mut Transaction<'_, MySql>,
    actor_type: &str,
    actor_id: &str,
    action: &str,
    target_type: &str,
    target_id: &str,
    node_id: Option<&str>,
    detail_json: &str,
) -> Result<(), TagError> {
    let prev = sqlx::query("SELECT `hash` FROM audit_events ORDER BY id DESC LIMIT 1")
        .fetch_optional(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let prev_hash = match prev {
        Some(row) => row.try_get::<String, _>("hash").map_err(db_err)?,
        None => String::new(),
    };
    let hash = audit_hash(&prev_hash, action, target_id, detail_json);
    let stored_prev = if prev_hash.is_empty() {
        None
    } else {
        Some(prev_hash)
    };
    sqlx::query(
        r#"
        INSERT INTO audit_events (
            `at`, actor_type, actor_id, action, target_type, target_id, node_id,
            detail_json, prev_hash, `hash`
        ) VALUES (
            CURRENT_TIMESTAMP(3), ?, ?, ?, ?, ?, ?, ?, ?, ?
        )
        "#,
    )
    .bind(actor_type)
    .bind(actor_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(node_id)
    .bind(detail_json)
    .bind(stored_prev)
    .bind(hash)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    Ok(())
}

fn audit_hash(prev: &str, action: &str, target_id: &str, detail: &str) -> String {
    sha256_hex(format!("{prev}\n{action}\n{target_id}\n{detail}").as_bytes())
}

fn anyhow_tag(err: TagError) -> anyhow::Error {
    match err {
        TagError::Internal(inner) => inner,
        other => anyhow::anyhow!("{other}"),
    }
}

fn is_deadlock(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let text = cause.to_string();
        text.contains("Deadlock") || text.contains("1213")
    })
}

fn anyhow_tag_to_tag(err: anyhow::Error) -> TagError {
    TagError::Internal(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::connect_test_db;
    use crate::http::{router, AppState};
    use crate::hub::Hub;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn send(db: Db, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let app = router(AppState {
            db,
            hub: Hub::new(),
            license_env: None,
            api_token: None,
        });
        let builder = Request::builder().method(method).uri(uri);
        let request = if let Some(body) = body {
            builder
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        } else {
            builder.body(Body::empty()).unwrap()
        };
        let response = app.oneshot(request).await.expect("oneshot");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&bytes);
        let json = if text.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.into_owned()))
        };
        (status, json)
    }

    fn code(body: &Value) -> &str {
        body.get("code").and_then(Value::as_str).unwrap_or("")
    }

    fn nid(prefix: &str) -> String {
        format!("nta-{prefix}-{}", Uuid::new_v4())
    }

    async fn reg(db: &Db, id: &str, version: &str, tags: &[(&str, &str)], os: &str, arch: &str) {
        let map = tags
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        register_node(db, id, "host", version, &map, os, arch)
            .await
            .expect("register");
    }

    fn ids_of(body: &Value) -> Vec<String> {
        body.as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|row| {
                row.get("node_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[tokio::test]
    async fn format_reserved_limit_and_delete_codes() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = nid("fmt");
        reg(&db, &id, "0.1.0", &[("role", "db")], "", "").await;

        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/Env"),
            Some(json!({"value": "prod"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "tag_key_invalid");

        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/env"),
            Some(json!({"value": "Prod"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "tag_value_invalid");

        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/sys.os"),
            Some(json!({"value": "linux"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "tag_key_reserved");

        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/sys"),
            Some(json!({"value": "nope"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "tag_key_reserved");

        for i in 0..32 {
            let (status, _) = send(
                db.clone(),
                "PUT",
                &format!("/v1/nodes/{id}/tags/k{i:02}"),
                Some(json!({"value": "v"})),
            )
            .await;
            assert!(status.is_success(), "k{i:02} {status}");
        }
        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/k32"),
            Some(json!({"value": "v"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "tag_limit_exceeded");

        let (status, _) = send(
            db.clone(),
            "DELETE",
            &format!("/v1/nodes/{id}/tags/k00"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, body) = send(
            db.clone(),
            "DELETE",
            &format!("/v1/nodes/{id}/tags/k00"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code(&body), "tag_not_found");
        let (status, body) = send(
            db.clone(),
            "DELETE",
            &format!("/v1/nodes/{id}/tags/role"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "tag_not_center");
    }

    #[tokio::test]
    async fn register_override_labels_system_and_put_config() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = nid("reg");
        reg(
            &db,
            &id,
            "0.3.0+git.ab12",
            &[("role", "app"), ("Env", "Prod")],
            "Linux",
            "aarch64",
        )
        .await;

        let (status, list) = send(db.clone(), "GET", "/v1/nodes", None).await;
        assert_eq!(status, StatusCode::OK);
        let row = list
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["node_id"] == id)
            .unwrap();
        assert_eq!(row["labels"]["role"], "app");
        assert_eq!(row["labels"]["Env"], "Prod");
        assert_eq!(row["tags"]["role"], "app");
        assert_eq!(row["tags"]["sys.version"], "0.3.0_git.ab12");
        assert_eq!(row["tags"]["sys.os"], "linux");
        assert_eq!(row["tags"]["sys.arch"], "aarch64");
        assert!(row["tags"].get("Env").is_none());
        assert!(row["invalid_tag_count"].as_i64().unwrap() >= 1);

        let (status, tags_body) =
            send(db.clone(), "GET", &format!("/v1/nodes/{id}/tags"), None).await;
        assert_eq!(status, StatusCode::OK);
        let env = tags_body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["key"] == "Env")
            .unwrap();
        assert_eq!(env["valid"], false);
        assert_eq!(env["invalid_reason"], "key_invalid");
        assert_eq!(env["effective"], false);

        let (status, put) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/role"),
            Some(json!({"value": "web", "confirm_prod": {"node_count": 1}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{put}");
        assert_eq!(put["overridden"], true);
        assert_eq!(put["node_value"], "app");
        assert_eq!(put["effective_value"], "web");

        let (status, cfg) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/config"),
            Some(json!({"specs": []})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{cfg}");

        let (_, tags_body) = send(db.clone(), "GET", &format!("/v1/nodes/{id}/tags"), None).await;
        let rows = tags_body["tags"].as_array().unwrap();
        let center = rows
            .iter()
            .find(|row| row["source"] == "center" && row["key"] == "role")
            .unwrap();
        assert_eq!(center["value"], "web");
        assert_eq!(center["effective"], true);
        assert_eq!(center["node_value"], "app");
        let node = rows
            .iter()
            .find(|row| row["source"] == "node" && row["key"] == "role")
            .unwrap();
        assert_eq!(node["value"], "app");
        assert_eq!(node["effective"], false);
        assert!(rows
            .iter()
            .any(|row| row["key"] == "sys.version" && row["source"] == "system"));

        reg(
            &db,
            &id,
            "0.1.0",
            &[("role", "db"), ("env", "demo")],
            "",
            "",
        )
        .await;
        let (_, tags_body) = send(db.clone(), "GET", &format!("/v1/nodes/{id}/tags"), None).await;
        let rows = tags_body["tags"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|row| row["source"] == "center" && row["key"] == "role" && row["value"] == "web"));
        assert!(rows
            .iter()
            .any(|row| row["source"] == "node" && row["key"] == "role" && row["value"] == "db"));
        assert!(rows
            .iter()
            .any(|row| row["key"] == "sys.version" && row["value"] == "0.1.0"));
        assert!(!rows.iter().any(|row| row["key"] == "sys.os"));
        assert!(!rows.iter().any(|row| row["key"] == "sys.arch"));

        let (_, list) = send(db.clone(), "GET", "/v1/nodes", None).await;
        let row = list
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["node_id"] == id)
            .unwrap();
        assert_eq!(row["labels"]["role"], "db");
        assert_eq!(row["labels"]["env"], "demo");
        assert_eq!(row["tags"]["role"], "web");
        assert_eq!(row["tags"]["env"], "demo");
        assert_eq!(row["tag_sources"]["role"], "center");

        let n: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM audit_events WHERE node_id = ? AND action = 'tag.set'",
        )
        .bind(&id)
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
        assert!(n >= 1, "tag.set audit");
        let n: i64 = sqlx::query(
            "SELECT COUNT(*) AS n FROM audit_events WHERE node_id = ? AND action = 'tag.node_reported_changed'",
        )
        .bind(&id)
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
        assert!(n >= 1, "node reported audit");
    }

    #[tokio::test]
    async fn selector_filters_effective_tags() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let db_node = nid("sel-db");
        let web = nid("sel-web");
        let bare = nid("sel-bare");
        reg(
            &db,
            &db_node,
            "0.1.0",
            &[("role", "db"), ("env", "prod"), ("team", "platform")],
            "",
            "",
        )
        .await;
        reg(
            &db,
            &web,
            "0.1.0",
            &[("role", "web"), ("env", "dev")],
            "",
            "",
        )
        .await;
        reg(&db, &bare, "0.1.0", &[("role", "db")], "", "").await;

        let cases = [
            (
                "role=db",
                vec![db_node.as_str(), bare.as_str()],
                vec![web.as_str()],
            ),
            (
                "team",
                vec![db_node.as_str()],
                vec![web.as_str(), bare.as_str()],
            ),
            (
                "!team",
                vec![web.as_str(), bare.as_str()],
                vec![db_node.as_str()],
            ),
            (
                "env in (dev,test)",
                vec![web.as_str()],
                vec![db_node.as_str(), bare.as_str()],
            ),
            (
                "env!=prod",
                vec![web.as_str(), bare.as_str()],
                vec![db_node.as_str()],
            ),
            (
                "role=db,env=prod",
                vec![db_node.as_str()],
                vec![web.as_str(), bare.as_str()],
            ),
        ];
        for (selector, want, skip) in cases {
            let (status, body) = send(
                db.clone(),
                "GET",
                &format!("/v1/nodes?selector={}", urlencoding_min(selector)),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{selector} {body}");
            let got = ids_of(&body);
            for id in want {
                assert!(got.iter().any(|item| item == id), "{selector} missing {id}");
            }
            for id in skip {
                assert!(
                    !got.iter().any(|item| item == id),
                    "{selector} included {id}"
                );
            }
        }

        let (status, body) = send(
            db.clone(),
            "GET",
            &format!("/v1/nodes?selector={}", urlencoding_min("env=dev,env=test")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code(&body), "selector_invalid");
        assert!(body["error"].as_str().unwrap().contains("in"), "{body}");
    }

    fn urlencoding_min(raw: &str) -> String {
        let mut out = String::new();
        for b in raw.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    #[tokio::test]
    async fn bulk_dry_run_does_not_write_and_apply_does() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let a = nid("bulk-a");
        let b = nid("bulk-b");
        reg(&db, &a, "0.1.0", &[("role", "db")], "", "").await;
        reg(&db, &b, "0.1.0", &[("role", "web")], "", "").await;
        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{a}/tags/team"),
            Some(json!({"value": "platform"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, dry) = send(
            db.clone(),
            "POST",
            "/v1/tags/bulk",
            Some(json!({
                "node_ids": [a, b],
                "add": {"team": "platform"},
                "dry_run": true
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{dry}");
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["operation_id"], Value::Null);
        assert_eq!(dry["counts"]["unchanged"], 1);
        assert_eq!(dry["counts"]["added"], 1);
        let (_, tags_b) = send(db.clone(), "GET", &format!("/v1/nodes/{b}/tags"), None).await;
        assert!(!tags_b["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == "team"));

        let (status, removed) = send(
            db.clone(),
            "POST",
            "/v1/tags/bulk",
            Some(json!({
                "node_ids": [a, b],
                "remove": ["role"],
                "dry_run": true
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{removed}");
        assert_eq!(removed["counts"]["not_changed_boot_reported"], 2);

        let (status, applied) = send(
            db.clone(),
            "POST",
            "/v1/tags/bulk",
            Some(json!({
                "node_ids": [b],
                "add": {"team": "sre"},
                "dry_run": false
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{applied}");
        assert!(applied["operation_id"].as_str().is_some());
        assert_eq!(applied["counts"]["added"], 1);
        let (_, tags_b) = send(db.clone(), "GET", &format!("/v1/nodes/{b}/tags"), None).await;
        assert!(tags_b["tags"].as_array().unwrap().iter().any(|row| {
            row["key"] == "team" && row["source"] == "center" && row["value"] == "sre"
        }));
    }

    #[tokio::test]
    async fn prod_guard_on_tag_mutate_only() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = nid("prod");
        reg(&db, &id, "0.1.0", &[("env", "demo")], "", "").await;
        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/env"),
            Some(json!({"value": "prod"})),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(code(&body), "prod_confirmation_required");
        assert_eq!(body["prod_node_count"], 1);

        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/env"),
            Some(json!({"value": "prod", "confirm_prod": {"node_count": 1}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/role"),
            Some(json!({"value": "db"})),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);

        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/role"),
            Some(json!({"value": "db", "confirm_prod": {"node_count": 1}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let (status, _) = send(
            db.clone(),
            "DELETE",
            &format!("/v1/nodes/{id}/tags/env?confirm_prod_count=1"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/team"),
            Some(json!({"value": "platform"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let invalid = nid("inv");
        reg(&db, &invalid, "0.1.0", &[("ENV", "Prod")], "", "").await;
        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{invalid}/tags/team"),
            Some(json!({"value": "a"})),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(code(&body), "prod_confirmation_required");
    }

    #[tokio::test]
    async fn backfill_seeds_labels_and_skips_http_admin() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = nid("bf");
        let mut labels = HashMap::new();
        labels.insert("role".into(), "db".into());
        db.upsert_node(&id, "host", "1.2.3", &labels)
            .await
            .expect("upsert");
        backfill(&db).await.expect("backfill");
        backfill(&db).await.expect("backfill again");
        let (_, body) = send(db.clone(), "GET", &format!("/v1/nodes/{id}/tags"), None).await;
        let rows = body["tags"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| row["key"] == "role" && row["source"] == "node")
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["key"] == "sys.version" && row["value"] == "1.2.3")
                .count(),
            1
        );

        let admin = nid("admin");
        db.upsert_node(&admin, "", "http-admin", &HashMap::new())
            .await
            .expect("upsert admin");
        backfill(&db).await.expect("backfill admin");
        let (_, body) = send(db.clone(), "GET", &format!("/v1/nodes/{admin}/tags"), None).await;
        assert!(!body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["key"] == "sys.version"));
    }

    #[tokio::test]
    async fn tag_overview_and_ee_stub() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let (status, body) = send(db.clone(), "GET", "/v1/tags", None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["missing"]["env"].is_number());
        assert!(body["missing"]["role"].is_number());
        assert!(body["keys"].is_array());

        let (status, body) = send(db.clone(), "GET", "/v1/ee/tag-policies", None).await;
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(code(&body), "license_required");
        assert_eq!(body["license_required"], true);
    }

    #[tokio::test]
    async fn if_value_conflict() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = nid("if");
        reg(&db, &id, "0.1.0", &[], "", "").await;
        let (status, _) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/team"),
            Some(json!({"value": "platform"})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, body) = send(
            db.clone(),
            "PUT",
            &format!("/v1/nodes/{id}/tags/team"),
            Some(json!({"value": "sre", "if_value": "other"})),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code(&body), "tag_conflict");
    }
}
