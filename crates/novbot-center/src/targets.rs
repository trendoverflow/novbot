// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Shared target resolution, snapshot rows, and the server-side prod guard.
//!
//! Submit order: validate, resolve, expected mismatch, empty, size, prod guard,
//! then one transaction that freezes the list and the feature rows.

use crate::artifact::sha256_hex;
use crate::db::Db;
use crate::hub::Hub;
use chrono::{DateTime, Utc};
use novbot_core::tags::{self, SelectorParse, MAX_TARGETS};
use novbot_proto::{server_message, Dispatch, ServerMessage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{MySqlConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::DerefMut;
use uuid::Uuid;

const ACTOR_TYPE: &str = "api_token";
const ACTOR_ID: &str = "api_token";

#[derive(Debug)]
pub(crate) enum TargetError {
    Invalid(String),
    Selector(tags::SelectorError),
    Unknown(Vec<String>),
    Changed {
        added: Vec<String>,
        removed: Vec<String>,
    },
    Empty,
    TooMany,
    Prod {
        prod_node_ids: Vec<String>,
        prod_node_count: usize,
    },
    Timing(String),
    ScheduleUnknown,
    OperationUnknown,
    Internal(anyhow::Error),
}

fn db_err(err: sqlx::Error) -> TargetError {
    TargetError::Internal(err.into())
}

fn json_err(err: serde_json::Error) -> TargetError {
    TargetError::Internal(err.into())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ConfirmProd {
    pub node_count: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct TargetBody {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub node_ids: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_node_ids: Vec<String>,
    #[serde(default)]
    pub expected_node_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NodeHit {
    pub node_id: String,
    pub hostname: String,
    pub online: bool,
    pub registered: bool,
    pub prod: bool,
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolutionView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub selector: Option<String>,
    pub resolved_at: DateTime<Utc>,
    pub node_ids: Vec<String>,
    pub excluded_node_ids: Vec<String>,
    pub nodes: Vec<NodeHit>,
    pub count: usize,
    pub offline_count: usize,
    pub prod_node_ids: Vec<String>,
    pub prod_node_count: usize,
    pub resolution_sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Frozen {
    pub selector: Option<String>,
    pub picked_node_ids: Option<Vec<String>>,
    pub excluded_node_ids: Vec<String>,
    pub node_ids: Vec<String>,
    pub nodes: Vec<NodeHit>,
    pub prod_node_ids: Vec<String>,
    pub resolution_sha256: String,
    pub resolved_at: DateTime<Utc>,
    pub prod_confirmed: bool,
}

impl Frozen {
    fn view(&self) -> ResolutionView {
        ResolutionView {
            id: None,
            selector: self.selector.clone(),
            resolved_at: self.resolved_at,
            node_ids: self.node_ids.clone(),
            excluded_node_ids: self.excluded_node_ids.clone(),
            nodes: self.nodes.clone(),
            count: self.node_ids.len(),
            offline_count: self.nodes.iter().filter(|node| !node.online).count(),
            prod_node_ids: self.prod_node_ids.clone(),
            prod_node_count: self.prod_node_ids.len(),
            resolution_sha256: self.resolution_sha256.clone(),
        }
    }
}

#[derive(Clone)]
struct InventoryNode {
    node_id: String,
    hostname: String,
    version: String,
    last_seen_at: Option<DateTime<Utc>>,
    tags: BTreeMap<String, String>,
    prod: bool,
}

enum Mode {
    Selector(SelectorParse),
    Nodes(Vec<String>),
}

struct ParsedTarget {
    mode: Mode,
    selector: Option<String>,
    picked: Option<Vec<String>>,
    excluded: Vec<String>,
    expected: Option<Vec<String>>,
}

pub(crate) fn has_mode(target: &TargetBody) -> bool {
    let selector = target
        .selector
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    selector || target.node_ids.is_some()
}

pub(crate) fn coalesce_target(
    target: Option<TargetBody>,
    node_ids: Vec<String>,
) -> Result<TargetBody, TargetError> {
    if let Some(target) = target {
        if has_mode(&target) {
            return Ok(target);
        }
    }
    if node_ids.iter().any(|id| !id.trim().is_empty()) {
        return Ok(TargetBody {
            selector: None,
            node_ids: Some(node_ids),
            exclude_node_ids: Vec::new(),
            expected_node_ids: None,
        });
    }
    Err(TargetError::Invalid(
        "target must set exactly one of selector or node_ids".into(),
    ))
}

fn sorted_unique(ids: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut out: Vec<String> = ids
        .into_iter()
        .map(|id| id.as_ref().trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn parse_target(body: &TargetBody) -> Result<ParsedTarget, TargetError> {
    let selector = body
        .selector
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let picked = body.node_ids.as_ref();
    let excluded = sorted_unique(body.exclude_node_ids.iter().map(String::as_str));
    let expected = body
        .expected_node_ids
        .as_ref()
        .map(|ids| sorted_unique(ids));
    match (selector, picked) {
        (Some(_), Some(_)) | (None, None) => Err(TargetError::Invalid(
            "target must set exactly one of selector or node_ids".into(),
        )),
        (Some(raw), None) => {
            let parsed = tags::parse_selector(raw).map_err(TargetError::Selector)?;
            if matches!(parsed, SelectorParse::Empty) {
                return Err(TargetError::Invalid(
                    "target must set exactly one of selector or node_ids".into(),
                ));
            }
            let canonical = match &parsed {
                SelectorParse::All => Some("*".to_string()),
                SelectorParse::Filter(selector) => Some(selector.canonical.clone()),
                SelectorParse::Empty => None,
            };
            Ok(ParsedTarget {
                mode: Mode::Selector(parsed),
                selector: canonical,
                picked: None,
                excluded,
                expected,
            })
        }
        (None, Some(ids)) => {
            let ids = sorted_unique(ids.iter().map(String::as_str));
            Ok(ParsedTarget {
                mode: Mode::Nodes(ids.clone()),
                selector: None,
                picked: Some(ids),
                excluded,
                expected,
            })
        }
    }
}

fn diff_ids(expected: &[String], live: &[String]) -> (Vec<String>, Vec<String>) {
    let expected_set: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    let live_set: BTreeSet<&str> = live.iter().map(String::as_str).collect();
    let added = live
        .iter()
        .filter(|id| !expected_set.contains(id.as_str()))
        .cloned()
        .collect();
    let removed = expected
        .iter()
        .filter(|id| !live_set.contains(id.as_str()))
        .cloned()
        .collect();
    (added, removed)
}

#[cfg(test)]
fn same_ids(left: &[String], right: &[String]) -> bool {
    let left = sorted_unique(left);
    let right = sorted_unique(right);
    left == right
}

fn ensure_size(count: usize) -> Result<(), TargetError> {
    if count > MAX_TARGETS {
        Err(TargetError::TooMany)
    } else {
        Ok(())
    }
}

fn sha_of(node_ids: &[String]) -> String {
    let encoded = serde_json::to_string(node_ids).unwrap_or_else(|_| "[]".into());
    sha256_hex(encoded.as_bytes())
}

fn registered(version: &str, last_seen_at: Option<DateTime<Utc>>) -> bool {
    if last_seen_at.is_some() {
        return true;
    }
    !version.is_empty() && version != "http-admin"
}

async fn load_inventory(db: &Db) -> Result<Vec<InventoryNode>, TargetError> {
    let rows =
        sqlx::query("SELECT node_id, hostname, version, last_seen_at FROM nodes ORDER BY node_id")
            .fetch_all(db.pool())
            .await
            .map_err(db_err)?;
    let tag_rows = sqlx::query("SELECT node_id, tag_key, tag_value FROM node_tags_effective")
        .fetch_all(db.pool())
        .await
        .map_err(db_err)?;
    let mut tags: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for row in tag_rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        let key = crate::db::mysql_text(&row, "tag_key").map_err(db_err)?;
        let value = crate::db::mysql_text(&row, "tag_value").map_err(db_err)?;
        tags.entry(node_id).or_default().insert(key, value);
    }
    let invalid = sqlx::query(
        "SELECT node_id, tag_key, tag_value FROM node_tags WHERE source = 'node' AND valid = 0",
    )
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut invalid_prod = BTreeSet::new();
    for row in invalid {
        let key = crate::db::mysql_text(&row, "tag_key").map_err(db_err)?;
        let value = crate::db::mysql_text(&row, "tag_value").map_err(db_err)?;
        if key.eq_ignore_ascii_case("env") && value.eq_ignore_ascii_case("prod") {
            invalid_prod.insert(row.try_get::<String, _>("node_id").map_err(db_err)?);
        }
    }
    let mut nodes = Vec::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        let node_tags = tags.remove(&node_id).unwrap_or_default();
        let effective_prod = node_tags.get("env").is_some_and(|value| value == "prod");
        nodes.push(InventoryNode {
            prod: effective_prod || invalid_prod.contains(&node_id),
            tags: node_tags,
            hostname: row.try_get("hostname").map_err(db_err)?,
            version: row.try_get("version").map_err(db_err)?,
            last_seen_at: row.try_get("last_seen_at").map_err(db_err)?,
            node_id,
        });
    }
    Ok(nodes)
}

fn hits(nodes: &[&InventoryNode], hub: &Hub) -> Vec<NodeHit> {
    nodes
        .iter()
        .map(|node| NodeHit {
            node_id: node.node_id.clone(),
            hostname: node.hostname.clone(),
            online: hub.is_connected(&node.node_id),
            registered: registered(&node.version, node.last_seen_at),
            prod: node.prod,
            tags: node.tags.clone(),
        })
        .collect()
}

fn resolve_parsed<'a>(
    inventory: &'a [InventoryNode],
    parsed: &ParsedTarget,
) -> Result<Vec<&'a InventoryNode>, TargetError> {
    let excluded: BTreeSet<&str> = parsed.excluded.iter().map(String::as_str).collect();
    let mut matched: Vec<&InventoryNode> = match &parsed.mode {
        Mode::Selector(SelectorParse::All) => inventory
            .iter()
            .filter(|node| !excluded.contains(node.node_id.as_str()))
            .collect(),
        Mode::Selector(SelectorParse::Filter(selector)) => inventory
            .iter()
            .filter(|node| {
                !excluded.contains(node.node_id.as_str())
                    && tags::selector_matches(selector, &node.tags)
            })
            .collect(),
        Mode::Selector(SelectorParse::Empty) => {
            return Err(TargetError::Invalid(
                "target must set exactly one of selector or node_ids".into(),
            ));
        }
        Mode::Nodes(ids) => {
            let by_id: BTreeMap<&str, &InventoryNode> = inventory
                .iter()
                .map(|node| (node.node_id.as_str(), node))
                .collect();
            let mut unknown = Vec::new();
            let mut out = Vec::new();
            for id in ids {
                match by_id.get(id.as_str()) {
                    Some(node) if !excluded.contains(id.as_str()) => out.push(*node),
                    Some(_) => {}
                    None => unknown.push(id.clone()),
                }
            }
            if !unknown.is_empty() {
                unknown.sort();
                return Err(TargetError::Unknown(unknown));
            }
            out
        }
    };
    matched.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    matched.dedup_by(|left, right| left.node_id == right.node_id);
    Ok(matched)
}

fn freeze(
    parsed: &ParsedTarget,
    matched: &[&InventoryNode],
    hub: &Hub,
    confirm: Option<&ConfirmProd>,
) -> Frozen {
    let nodes = hits(matched, hub);
    let node_ids: Vec<String> = nodes.iter().map(|node| node.node_id.clone()).collect();
    let prod_node_ids: Vec<String> = nodes
        .iter()
        .filter(|node| node.prod)
        .map(|node| node.node_id.clone())
        .collect();
    let prod_confirmed = !prod_node_ids.is_empty()
        && confirm.map(|item| item.node_count) == Some(prod_node_ids.len() as u64);
    Frozen {
        selector: parsed.selector.clone(),
        picked_node_ids: parsed.picked.clone(),
        excluded_node_ids: parsed.excluded.clone(),
        resolution_sha256: sha_of(&node_ids),
        resolved_at: Utc::now(),
        prod_confirmed,
        prod_node_ids,
        node_ids,
        nodes,
    }
}

pub(crate) async fn preview(
    db: &Db,
    hub: &Hub,
    target: &TargetBody,
) -> Result<ResolutionView, TargetError> {
    let parsed = parse_target(target)?;
    let inventory = load_inventory(db).await?;
    let matched = resolve_parsed(&inventory, &parsed)?;
    ensure_size(matched.len())?;
    Ok(freeze(&parsed, &matched, hub, None).view())
}

/// Resolve and run the submit checks. Audits a refusal. Does not freeze a row.
pub(crate) async fn check_submit(
    db: &Db,
    hub: &Hub,
    target: &TargetBody,
    confirm: Option<&ConfirmProd>,
    kind: &str,
    target_id: &str,
) -> Result<Frozen, TargetError> {
    let parsed = match parse_target(target) {
        Ok(parsed) => parsed,
        Err(err) => return Err(err),
    };
    let inventory = load_inventory(db).await?;
    let matched = match resolve_parsed(&inventory, &parsed) {
        Ok(matched) => matched,
        Err(err) => return Err(err),
    };
    let frozen = freeze(&parsed, &matched, hub, confirm);
    if let Some(expected) = &parsed.expected {
        let (added, removed) = diff_ids(expected, &frozen.node_ids);
        if !added.is_empty() || !removed.is_empty() {
            refuse(
                db,
                kind,
                target_id,
                "target_changed",
                parsed.selector.as_deref(),
                &frozen.prod_node_ids,
            )
            .await?;
            return Err(TargetError::Changed { added, removed });
        }
    }
    if frozen.node_ids.is_empty() {
        return Err(TargetError::Empty);
    }
    if let Err(err) = ensure_size(frozen.node_ids.len()) {
        refuse(
            db,
            kind,
            target_id,
            "too_many_targets",
            parsed.selector.as_deref(),
            &frozen.prod_node_ids,
        )
        .await?;
        return Err(err);
    }
    if !frozen.prod_node_ids.is_empty() && !frozen.prod_confirmed {
        refuse(
            db,
            kind,
            target_id,
            "prod_confirmation_required",
            parsed.selector.as_deref(),
            &frozen.prod_node_ids,
        )
        .await?;
        return Err(TargetError::Prod {
            prod_node_count: frozen.prod_node_ids.len(),
            prod_node_ids: frozen.prod_node_ids.clone(),
        });
    }
    Ok(frozen)
}

pub(crate) async fn ensure_prod_confirm(
    db: &Db,
    node_ids: &[String],
    confirm: Option<&ConfirmProd>,
    kind: &str,
    target_id: &str,
) -> Result<(), TargetError> {
    let prod = prod_ids_among(db, node_ids).await?;
    if prod.is_empty() {
        return Ok(());
    }
    if confirm.map(|item| item.node_count) != Some(prod.len() as u64) {
        refuse(
            db,
            kind,
            target_id,
            "prod_confirmation_required",
            None,
            &prod,
        )
        .await?;
        return Err(TargetError::Prod {
            prod_node_count: prod.len(),
            prod_node_ids: prod,
        });
    }
    Ok(())
}

async fn prod_ids_among(db: &Db, node_ids: &[String]) -> Result<Vec<String>, TargetError> {
    let want: BTreeSet<&str> = node_ids.iter().map(String::as_str).collect();
    let inventory = load_inventory(db).await?;
    let mut prod: Vec<String> = inventory
        .into_iter()
        .filter(|node| node.prod && want.contains(node.node_id.as_str()))
        .map(|node| node.node_id)
        .collect();
    prod.sort();
    Ok(prod)
}

async fn refuse(
    db: &Db,
    kind: &str,
    target_id: &str,
    code: &str,
    selector: Option<&str>,
    prod_node_ids: &[String],
) -> Result<(), TargetError> {
    let detail = json!({
        "code": code,
        "selector": selector,
        "prod_node_ids": prod_node_ids,
    });
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    audit_event(
        tx.deref_mut(),
        "target.refused",
        kind,
        target_id,
        None,
        &detail,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    Ok(())
}

pub(crate) async fn insert_resolution(
    conn: &mut MySqlConnection,
    kind: &str,
    ref_id: &str,
    frozen: &Frozen,
    supersedes_id: Option<&str>,
) -> Result<String, TargetError> {
    let id = Uuid::new_v4().to_string();
    let picked = match &frozen.picked_node_ids {
        Some(ids) => Some(serde_json::to_string(ids).map_err(json_err)?),
        None => None,
    };
    let excluded = serde_json::to_string(&frozen.excluded_node_ids).map_err(json_err)?;
    let node_ids = serde_json::to_string(&frozen.node_ids).map_err(json_err)?;
    let prod_ids = serde_json::to_string(&frozen.prod_node_ids).map_err(json_err)?;
    sqlx::query(
        r#"
        INSERT INTO target_resolutions (
            id, kind, ref_id, selector, picked_node_ids_json, excluded_node_ids_json,
            node_ids_json, node_count, prod_node_ids_json, prod_confirmed,
            resolution_sha256, resolved_at, supersedes_id, actor
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(&id)
    .bind(kind)
    .bind(ref_id)
    .bind(&frozen.selector)
    .bind(&picked)
    .bind(&excluded)
    .bind(&node_ids)
    .bind(frozen.node_ids.len() as i32)
    .bind(&prod_ids)
    .bind(i8::from(frozen.prod_confirmed))
    .bind(&frozen.resolution_sha256)
    .bind(frozen.resolved_at)
    .bind(supersedes_id)
    .bind(ACTOR_ID)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    let detail = json!({
        "resolution_id": &id,
        "selector": frozen.selector,
        "picked_node_ids": frozen.picked_node_ids,
        "excluded_node_ids": frozen.excluded_node_ids,
        "node_ids": frozen.node_ids,
        "prod_node_ids": frozen.prod_node_ids,
        "prod_confirmed": frozen.prod_confirmed,
        "resolution_sha256": frozen.resolution_sha256,
    });
    audit_event(conn, "target.resolved", kind, ref_id, None, &detail).await?;
    Ok(id)
}

pub(crate) async fn store_resolution(
    db: &Db,
    kind: &str,
    ref_id: &str,
    frozen: &Frozen,
    supersedes_id: Option<&str>,
) -> Result<String, TargetError> {
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let id = insert_resolution(tx.deref_mut(), kind, ref_id, frozen, supersedes_id).await?;
    tx.commit().await.map_err(db_err)?;
    Ok(id)
}

async fn audit_event(
    conn: &mut MySqlConnection,
    action: &str,
    target_type: &str,
    target_id: &str,
    node_id: Option<&str>,
    detail: &Value,
) -> Result<(), TargetError> {
    lock_chain(conn).await?;
    let detail_json = serde_json::to_string(detail).map_err(json_err)?;
    let prev = sqlx::query("SELECT `hash` FROM audit_events ORDER BY id DESC LIMIT 1")
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    let prev_hash = match prev {
        Some(row) => row.try_get::<String, _>("hash").map_err(db_err)?,
        None => String::new(),
    };
    let hash = sha256_hex(format!("{prev_hash}\n{action}\n{target_id}\n{detail_json}").as_bytes());
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
    .bind(ACTOR_TYPE)
    .bind(ACTOR_ID)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(node_id)
    .bind(&detail_json)
    .bind(stored_prev)
    .bind(hash)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

async fn lock_chain(conn: &mut MySqlConnection) -> Result<(), TargetError> {
    sqlx::query("SELECT id FROM license_state WHERE id = 1 FOR UPDATE")
        .fetch_optional(&mut *conn)
        .await
        .map_err(db_err)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub(crate) struct DispatchRequest {
    pub target: TargetBody,
    pub spec_id: String,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub confirm_prod: Option<ConfirmProd>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DispatchRun {
    pub node_id: String,
    pub run_id: String,
    pub delivered: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct Warning {
    pub node_id: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DispatchOutcome {
    pub operation_id: String,
    pub resolution: ResolutionView,
    pub per_node: Vec<DispatchRun>,
    pub warnings: Vec<Warning>,
}

pub(crate) async fn dispatch_target(
    db: &Db,
    hub: &Hub,
    body: DispatchRequest,
) -> Result<DispatchOutcome, TargetError> {
    let spec_id = body.spec_id.trim();
    if spec_id.is_empty() {
        return Err(TargetError::Invalid("spec_id is required".into()));
    }
    let frozen = check_submit(
        db,
        hub,
        &body.target,
        body.confirm_prod.as_ref(),
        "dispatch",
        spec_id,
    )
    .await?;
    let params = body
        .params
        .unwrap_or_else(|| Value::Object(Default::default()));
    let params_json = serde_json::to_string(&params).map_err(json_err)?;
    let operation_id = Uuid::new_v4().to_string();
    let mut planned = Vec::with_capacity(frozen.node_ids.len());
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let resolution_id =
        insert_resolution(tx.deref_mut(), "dispatch", &operation_id, &frozen, None).await?;
    sqlx::query(
        r#"
        INSERT INTO dispatch_operations (id, spec_id, params_json, resolution_id, actor, created_at)
        VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3))
        "#,
    )
    .bind(&operation_id)
    .bind(spec_id)
    .bind(&params_json)
    .bind(&resolution_id)
    .bind(ACTOR_ID)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    for node_id in &frozen.node_ids {
        let run_id = Uuid::new_v4().to_string();
        sqlx::query(
            r#"
            INSERT INTO pending_dispatches (node_id, run_id, spec_id, params_json)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(node_id)
        .bind(&run_id)
        .bind(spec_id)
        .bind(&params_json)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
        sqlx::query(
            r#"
            INSERT INTO dispatch_operation_runs (operation_id, node_id, run_id, delivered, error)
            VALUES (?, ?, ?, 'queued', NULL)
            "#,
        )
        .bind(&operation_id)
        .bind(node_id)
        .bind(&run_id)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
        let detail = json!({
            "operation_id": &operation_id,
            "resolution_id": &resolution_id,
            "spec_id": spec_id,
            "run_id": &run_id,
        });
        audit_event(
            tx.deref_mut(),
            "dispatch.requested",
            "node",
            node_id,
            Some(node_id),
            &detail,
        )
        .await?;
        planned.push((node_id.clone(), run_id));
    }
    tx.commit().await.map_err(db_err)?;

    let mut per_node = Vec::with_capacity(planned.len());
    for (node_id, run_id) in planned {
        let live = hub
            .send(
                &node_id,
                ServerMessage {
                    request_id: Uuid::new_v4().to_string(),
                    body: Some(server_message::Body::Dispatch(Dispatch {
                        run_id: run_id.clone(),
                        spec_id: spec_id.to_string(),
                        params_json: params_json.clone(),
                    })),
                },
            )
            .await;
        let delivered = if live { "live" } else { "queued" };
        if live {
            let _ = sqlx::query("DELETE FROM pending_dispatches WHERE node_id = ? AND run_id = ?")
                .bind(&node_id)
                .bind(&run_id)
                .execute(db.pool())
                .await;
            let _ = sqlx::query(
                "UPDATE dispatch_operation_runs SET delivered = 'live' WHERE operation_id = ? AND node_id = ?",
            )
            .bind(&operation_id)
            .bind(&node_id)
            .execute(db.pool())
            .await;
        }
        per_node.push(DispatchRun {
            node_id,
            run_id,
            delivered: delivered.to_string(),
        });
    }
    let mut warnings = Vec::new();
    for node_id in &frozen.node_ids {
        let present = match db.get_config(node_id).await {
            Ok(Some(cfg)) => spec_present(&cfg.specs_json, spec_id),
            _ => false,
        };
        if !present {
            warnings.push(Warning {
                node_id: node_id.clone(),
                code: "spec_missing".into(),
            });
        }
    }
    let mut resolution = frozen.view();
    resolution.id = Some(resolution_id);
    Ok(DispatchOutcome {
        operation_id,
        resolution,
        per_node,
        warnings,
    })
}

fn spec_present(specs_json: &str, spec_id: &str) -> bool {
    serde_json::from_str::<Value>(specs_json)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .is_some_and(|specs| {
            specs
                .iter()
                .any(|spec| spec.get("id").and_then(Value::as_str) == Some(spec_id))
        })
}

#[derive(Debug, Serialize)]
pub(crate) struct DispatchOperationView {
    pub id: String,
    pub spec_id: String,
    pub params: Value,
    pub resolution: ResolutionView,
    pub actor: String,
    pub created_at: DateTime<Utc>,
    pub per_node: Vec<DispatchRun>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DispatchOperationList {
    pub items: Vec<DispatchOperationView>,
}

pub(crate) async fn list_dispatch_operations(
    db: &Db,
) -> Result<DispatchOperationList, TargetError> {
    let rows = sqlx::query(
        r#"
        SELECT id, spec_id, params_json, resolution_id, actor, created_at
        FROM dispatch_operations
        ORDER BY created_at DESC
        LIMIT 100
        "#,
    )
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(dispatch_operation_from_row(db, &row).await?);
    }
    Ok(DispatchOperationList { items })
}

pub(crate) async fn get_dispatch_operation(
    db: &Db,
    id: &str,
) -> Result<DispatchOperationView, TargetError> {
    let row = sqlx::query(
        r#"
        SELECT id, spec_id, params_json, resolution_id, actor, created_at
        FROM dispatch_operations WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(db_err)?
    .ok_or(TargetError::OperationUnknown)?;
    dispatch_operation_from_row(db, &row).await
}

async fn dispatch_operation_from_row(
    db: &Db,
    row: &sqlx::mysql::MySqlRow,
) -> Result<DispatchOperationView, TargetError> {
    let id: String = row.try_get("id").map_err(db_err)?;
    let resolution_id: String = row.try_get("resolution_id").map_err(db_err)?;
    let params_json: String = row.try_get("params_json").map_err(db_err)?;
    let runs = sqlx::query(
        r#"
        SELECT node_id, run_id, delivered
        FROM dispatch_operation_runs
        WHERE operation_id = ?
        ORDER BY node_id
        "#,
    )
    .bind(&id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut per_node = Vec::with_capacity(runs.len());
    for run in runs {
        per_node.push(DispatchRun {
            node_id: run.try_get("node_id").map_err(db_err)?,
            run_id: run.try_get("run_id").map_err(db_err)?,
            delivered: run.try_get("delivered").map_err(db_err)?,
        });
    }
    Ok(DispatchOperationView {
        spec_id: row.try_get("spec_id").map_err(db_err)?,
        params: serde_json::from_str(&params_json).unwrap_or(Value::Object(Default::default())),
        resolution: load_resolution_view(db, &resolution_id).await?,
        actor: row.try_get("actor").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
        id,
        per_node,
    })
}

async fn load_resolution_view(db: &Db, id: &str) -> Result<ResolutionView, TargetError> {
    let row = sqlx::query(
        r#"
        SELECT id, selector, excluded_node_ids_json, node_ids_json, prod_node_ids_json,
               resolution_sha256, resolved_at
        FROM target_resolutions WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(db_err)?
    .ok_or_else(|| TargetError::Internal(anyhow::anyhow!("resolution missing")))?;
    let node_ids = parse_string_list(&row.try_get::<String, _>("node_ids_json").map_err(db_err)?);
    let excluded = parse_string_list(
        &row.try_get::<String, _>("excluded_node_ids_json")
            .map_err(db_err)?,
    );
    let prod = parse_string_list(
        &row.try_get::<String, _>("prod_node_ids_json")
            .map_err(db_err)?,
    );
    let selector: Option<String> = row.try_get("selector").map_err(db_err)?;
    let inventory = load_inventory(db).await?;
    let by_id: BTreeMap<&str, &InventoryNode> = inventory
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect();
    let nodes = node_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect::<Vec<_>>();
    let mut view = freeze(
        &ParsedTarget {
            mode: Mode::Nodes(node_ids.clone()),
            selector: selector.clone(),
            picked: None,
            excluded: excluded.clone(),
            expected: None,
        },
        &nodes,
        &Hub::new(),
        None,
    )
    .view();
    // The frozen hash and time are the stored snapshot, not a fresh resolve.
    view.id = Some(row.try_get::<String, _>("id").map_err(db_err)?);
    view.selector = selector.filter(|value| !value.is_empty());
    view.node_ids = node_ids;
    view.excluded_node_ids = excluded;
    view.prod_node_ids = prod.clone();
    view.prod_node_count = prod.len();
    view.count = view.node_ids.len();
    view.resolution_sha256 = row.try_get("resolution_sha256").map_err(db_err)?;
    view.resolved_at = row.try_get("resolved_at").map_err(db_err)?;
    Ok(view)
}

fn parse_string_list(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateScheduleRequest {
    pub target: TargetBody,
    pub spec_id: String,
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub confirm_prod: Option<ConfirmProd>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScheduleNodeState {
    pub node_id: String,
    pub state: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScheduleCreateResponse {
    pub id: String,
    pub resolution: ResolutionView,
    pub per_node: Vec<ScheduleNodeState>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ScheduleView {
    pub id: String,
    pub spec_id: String,
    pub interval_secs: Option<u64>,
    pub cron: Option<String>,
    pub enabled: bool,
    pub resolution_id: String,
    pub selector: Option<String>,
    pub node_ids: Vec<String>,
    pub resolved_at: DateTime<Utc>,
    pub nodes: Vec<ScheduleNodeState>,
    pub drift: NodeDiff,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct NodeDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ScheduleList {
    pub items: Vec<ScheduleView>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PatchScheduleRequest {
    #[serde(default)]
    pub interval_secs: Option<u64>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub clear_interval: bool,
    #[serde(default)]
    pub clear_cron: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReResolveRequest {
    pub dry_run: bool,
    #[serde(default)]
    pub expected_diff: Option<NodeDiff>,
    #[serde(default)]
    pub confirm_prod: Option<ConfirmProd>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ReResolveResponse {
    pub dry_run: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub prod_node_ids: Vec<String>,
    pub prod_node_count: usize,
    pub per_node: Vec<ScheduleNodeState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_id: Option<String>,
    pub warnings: Vec<Warning>,
}

struct Timing {
    interval_secs: Option<u64>,
    cron: Option<String>,
}

fn validate_timing(interval_secs: Option<u64>, cron: Option<&str>) -> Result<Timing, TargetError> {
    let cron = cron
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if interval_secs.is_none() && cron.is_none() {
        return Err(TargetError::Timing(
            "interval_secs or cron is required".into(),
        ));
    }
    if interval_secs == Some(0) {
        return Err(TargetError::Timing(
            "interval_secs must be greater than 0".into(),
        ));
    }
    if let Some(expr) = &cron {
        if expr.split_whitespace().count() != 5 {
            return Err(TargetError::Timing("cron must be 5 fields".into()));
        }
    }
    Ok(Timing {
        interval_secs,
        cron,
    })
}

pub(crate) async fn create_schedule(
    db: &Db,
    hub: &Hub,
    body: CreateScheduleRequest,
) -> Result<ScheduleCreateResponse, TargetError> {
    let spec_id = body.spec_id.trim();
    if spec_id.is_empty() {
        return Err(TargetError::Invalid("spec_id is required".into()));
    }
    let timing = validate_timing(body.interval_secs, body.cron.as_deref())?;
    let frozen = check_submit(
        db,
        hub,
        &body.target,
        body.confirm_prod.as_ref(),
        "schedule",
        spec_id,
    )
    .await?;
    let schedule_id = Uuid::new_v4().to_string();
    let origin = origin_of(&schedule_id);
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let resolution_id =
        insert_resolution(tx.deref_mut(), "schedule", &schedule_id, &frozen, None).await?;
    sqlx::query(
        r#"
        INSERT INTO target_schedules (
            id, spec_id, interval_secs, cron, enabled, resolution_id, `follow`, created_by
        ) VALUES (?, ?, ?, ?, ?, ?, 0, ?)
        "#,
    )
    .bind(&schedule_id)
    .bind(spec_id)
    .bind(timing.interval_secs.map(|value| value as i64))
    .bind(&timing.cron)
    .bind(i8::from(body.enabled))
    .bind(&resolution_id)
    .bind(ACTOR_ID)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    let mut per_node = Vec::new();
    let mut warnings = Vec::new();
    for node_id in &frozen.node_ids {
        let (entries, specs_json) = lock_config(tx.deref_mut(), node_id).await?;
        if !spec_present(&specs_json, spec_id) {
            warnings.push(Warning {
                node_id: node_id.clone(),
                code: "spec_missing".into(),
            });
        }
        let state = if has_spec(&entries, spec_id) {
            "skipped_exists"
        } else {
            let mut next = entries;
            next.push(schedule_entry(
                spec_id,
                timing.interval_secs,
                timing.cron.as_deref(),
                body.enabled,
                &origin,
            ));
            write_schedules(tx.deref_mut(), node_id, &next).await?;
            "active"
        };
        sqlx::query(
            r#"
            INSERT INTO target_schedule_nodes (schedule_id, node_id, state)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(&schedule_id)
        .bind(node_id)
        .bind(state)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
        per_node.push(ScheduleNodeState {
            node_id: node_id.clone(),
            state: state.to_string(),
        });
    }
    let detail = json!({
        "spec_id": spec_id,
        "interval_secs": timing.interval_secs,
        "cron": timing.cron,
        "enabled": body.enabled,
        "resolution_id": &resolution_id,
        "added": frozen.node_ids,
        "removed": [],
    });
    audit_event(
        tx.deref_mut(),
        "schedule.created",
        "schedule",
        &schedule_id,
        None,
        &detail,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    let mut resolution = frozen.view();
    resolution.id = Some(resolution_id);
    Ok(ScheduleCreateResponse {
        id: schedule_id,
        resolution,
        per_node,
        warnings,
    })
}

fn origin_of(schedule_id: &str) -> String {
    format!("sch_{schedule_id}")
}

fn schedule_entry(
    spec_id: &str,
    interval_secs: Option<u64>,
    cron: Option<&str>,
    enabled: bool,
    origin: &str,
) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("spec_id".into(), json!(spec_id));
    if let Some(interval) = interval_secs {
        map.insert("interval_secs".into(), json!(interval));
    }
    if let Some(cron) = cron {
        map.insert("cron".into(), json!(cron));
    }
    map.insert("enabled".into(), json!(enabled));
    map.insert("origin".into(), json!(origin));
    Value::Object(map)
}

fn has_spec(entries: &[Value], spec_id: &str) -> bool {
    entries
        .iter()
        .any(|entry| entry.get("spec_id").and_then(Value::as_str) == Some(spec_id))
}

fn parse_entries(raw: &str) -> Vec<Value> {
    serde_json::from_str::<Vec<Value>>(raw).unwrap_or_default()
}

async fn lock_config(
    conn: &mut MySqlConnection,
    node_id: &str,
) -> Result<(Vec<Value>, String), TargetError> {
    let row = sqlx::query(
        "SELECT specs_json, schedules_json FROM node_configs WHERE node_id = ? FOR UPDATE",
    )
    .bind(node_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db_err)?;
    let row = match row {
        Some(row) => row,
        None => {
            sqlx::query(
                r#"
                INSERT IGNORE INTO node_configs (node_id, config_generation, specs_json, schedules_json)
                VALUES (?, 1, '[]', '[]')
                "#,
            )
            .bind(node_id)
            .execute(&mut *conn)
            .await
            .map_err(db_err)?;
            sqlx::query(
                "SELECT specs_json, schedules_json FROM node_configs WHERE node_id = ? FOR UPDATE",
            )
            .bind(node_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(db_err)?
        }
    };
    let schedules: String = row.try_get("schedules_json").map_err(db_err)?;
    let specs: String = row.try_get("specs_json").map_err(db_err)?;
    Ok((parse_entries(&schedules), specs))
}

async fn write_schedules(
    conn: &mut MySqlConnection,
    node_id: &str,
    entries: &[Value],
) -> Result<(), TargetError> {
    let raw = serde_json::to_string(entries).map_err(json_err)?;
    sqlx::query(
        r#"
        UPDATE node_configs
        SET schedules_json = ?, config_generation = config_generation + 1
        WHERE node_id = ?
        "#,
    )
    .bind(raw)
    .bind(node_id)
    .execute(&mut *conn)
    .await
    .map_err(db_err)?;
    Ok(())
}

pub(crate) async fn list_schedules(db: &Db, hub: &Hub) -> Result<ScheduleList, TargetError> {
    let rows = sqlx::query(
        r#"
        SELECT id FROM target_schedules ORDER BY created_at DESC LIMIT 100
        "#,
    )
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id").map_err(db_err)?;
        if let Ok(view) = get_schedule(db, hub, &id).await {
            items.push(view);
        }
    }
    Ok(ScheduleList { items })
}

pub(crate) async fn get_schedule(
    db: &Db,
    hub: &Hub,
    id: &str,
) -> Result<ScheduleView, TargetError> {
    refresh_removed_states(db, id).await?;
    load_schedule_view(db, hub, id).await
}

async fn load_schedule_view(db: &Db, hub: &Hub, id: &str) -> Result<ScheduleView, TargetError> {
    let row = sqlx::query(
        r#"
        SELECT s.id, s.spec_id, s.interval_secs, s.cron, s.enabled, s.resolution_id,
               r.selector, r.picked_node_ids_json, r.excluded_node_ids_json, r.node_ids_json,
               r.resolved_at
        FROM target_schedules s
        JOIN target_resolutions r ON r.id = s.resolution_id
        WHERE s.id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(db_err)?
    .ok_or(TargetError::ScheduleUnknown)?;
    let node_rows = sqlx::query(
        "SELECT node_id, state FROM target_schedule_nodes WHERE schedule_id = ? ORDER BY node_id",
    )
    .bind(id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let nodes = node_rows
        .iter()
        .map(|row| {
            Ok(ScheduleNodeState {
                node_id: row.try_get("node_id").map_err(db_err)?,
                state: row.try_get("state").map_err(db_err)?,
            })
        })
        .collect::<Result<Vec<_>, TargetError>>()?;
    let stored = parse_string_list(&row.try_get::<String, _>("node_ids_json").map_err(db_err)?);
    let selector: Option<String> = row.try_get("selector").map_err(db_err)?;
    let picked_raw: Option<String> = row.try_get("picked_node_ids_json").map_err(db_err)?;
    let excluded = parse_string_list(
        &row.try_get::<String, _>("excluded_node_ids_json")
            .map_err(db_err)?,
    );
    let drift = current_drift(
        db,
        hub,
        selector.as_deref(),
        picked_raw.as_deref(),
        &excluded,
        &stored,
    )
    .await?;
    let interval: Option<i64> = row.try_get("interval_secs").map_err(db_err)?;
    let cron: Option<String> = row.try_get("cron").map_err(db_err)?;
    Ok(ScheduleView {
        id: row.try_get("id").map_err(db_err)?,
        spec_id: row.try_get("spec_id").map_err(db_err)?,
        interval_secs: interval.and_then(|value| u64::try_from(value).ok()),
        cron: cron.filter(|value| !value.is_empty()),
        enabled: flag(&row, "enabled")?,
        resolution_id: row.try_get("resolution_id").map_err(db_err)?,
        selector: selector.filter(|value| !value.is_empty()),
        node_ids: stored,
        resolved_at: row.try_get("resolved_at").map_err(db_err)?,
        nodes,
        drift,
    })
}

fn flag(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<bool, TargetError> {
    if let Ok(value) = row.try_get::<bool, _>(column) {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<i8, _>(column) {
        return Ok(value != 0);
    }
    let value: i64 = row.try_get(column).map_err(db_err)?;
    Ok(value != 0)
}

async fn current_drift(
    db: &Db,
    hub: &Hub,
    selector: Option<&str>,
    picked_raw: Option<&str>,
    excluded: &[String],
    stored: &[String],
) -> Result<NodeDiff, TargetError> {
    let live = rematch(db, hub, selector, picked_raw, excluded).await?;
    let (added, removed) = diff_ids(stored, &live);
    Ok(NodeDiff { added, removed })
}

async fn rematch(
    db: &Db,
    hub: &Hub,
    selector: Option<&str>,
    picked_raw: Option<&str>,
    excluded: &[String],
) -> Result<Vec<String>, TargetError> {
    let _ = hub;
    let body = if let Some(selector) = selector.filter(|value| !value.is_empty()) {
        TargetBody {
            selector: Some(selector.to_string()),
            node_ids: None,
            exclude_node_ids: excluded.to_vec(),
            expected_node_ids: None,
        }
    } else if let Some(raw) = picked_raw {
        TargetBody {
            selector: None,
            node_ids: Some(parse_string_list(raw)),
            exclude_node_ids: excluded.to_vec(),
            expected_node_ids: None,
        }
    } else {
        return Ok(Vec::new());
    };
    let parsed = parse_target(&body)?;
    let inventory = load_inventory(db).await?;
    // A picked node that disappeared is drift, not a 404 on read.
    let matched = match &parsed.mode {
        Mode::Nodes(ids) => {
            let excluded: BTreeSet<&str> = parsed.excluded.iter().map(String::as_str).collect();
            let mut live = Vec::new();
            for id in ids {
                if excluded.contains(id.as_str()) {
                    continue;
                }
                if inventory.iter().any(|node| node.node_id == *id) {
                    live.push(id.clone());
                }
            }
            live
        }
        _ => resolve_parsed(&inventory, &parsed)?
            .into_iter()
            .map(|node| node.node_id.clone())
            .collect(),
    };
    Ok(matched)
}

async fn refresh_removed_states(db: &Db, schedule_id: &str) -> Result<(), TargetError> {
    let origin = origin_of(schedule_id);
    let rows = sqlx::query(
        "SELECT node_id FROM target_schedule_nodes WHERE schedule_id = ? AND state = 'active'",
    )
    .bind(schedule_id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    for row in rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        let cfg = db
            .get_config(&node_id)
            .await
            .map_err(|err| TargetError::Internal(err))?;
        let entries = cfg
            .as_ref()
            .map(|cfg| parse_entries(&cfg.schedules_json))
            .unwrap_or_default();
        let still = entries
            .iter()
            .any(|entry| entry.get("origin").and_then(Value::as_str) == Some(origin.as_str()));
        if !still {
            sqlx::query(
                r#"
                UPDATE target_schedule_nodes
                SET state = 'removed_by_node_edit'
                WHERE schedule_id = ? AND node_id = ? AND state = 'active'
                "#,
            )
            .bind(schedule_id)
            .bind(&node_id)
            .execute(db.pool())
            .await
            .map_err(db_err)?;
        }
    }
    Ok(())
}

pub(crate) async fn note_node_schedule_edit(
    db: &Db,
    node_id: &str,
    schedules_json: &str,
) -> Result<(), TargetError> {
    let entries = parse_entries(schedules_json);
    let rows = sqlx::query(
        r#"
        SELECT schedule_id FROM target_schedule_nodes
        WHERE node_id = ? AND state = 'active'
        "#,
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    for row in rows {
        let schedule_id: String = row.try_get("schedule_id").map_err(db_err)?;
        let origin = origin_of(&schedule_id);
        let still = entries
            .iter()
            .any(|entry| entry.get("origin").and_then(Value::as_str) == Some(origin.as_str()));
        if !still {
            sqlx::query(
                r#"
                UPDATE target_schedule_nodes
                SET state = 'removed_by_node_edit'
                WHERE schedule_id = ? AND node_id = ?
                "#,
            )
            .bind(&schedule_id)
            .bind(node_id)
            .execute(db.pool())
            .await
            .map_err(db_err)?;
        }
    }
    Ok(())
}

pub(crate) async fn patch_schedule(
    db: &Db,
    hub: &Hub,
    id: &str,
    body: PatchScheduleRequest,
) -> Result<ScheduleView, TargetError> {
    let current = load_schedule_view(db, hub, id).await?;
    let interval = if body.clear_interval {
        None
    } else {
        body.interval_secs.or(current.interval_secs)
    };
    let cron = if body.clear_cron {
        None
    } else {
        body.cron
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or(current.cron.clone())
    };
    let timing = validate_timing(interval, cron.as_deref())?;
    let enabled = body.enabled.unwrap_or(current.enabled);
    let origin = origin_of(id);
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    sqlx::query(
        r#"
        UPDATE target_schedules
        SET interval_secs = ?, cron = ?, enabled = ?
        WHERE id = ?
        "#,
    )
    .bind(timing.interval_secs.map(|value| value as i64))
    .bind(&timing.cron)
    .bind(i8::from(enabled))
    .bind(id)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    for node in &current.nodes {
        if node.state != "active" {
            continue;
        }
        let (mut entries, _) = lock_config(tx.deref_mut(), &node.node_id).await?;
        let mut changed = false;
        for entry in &mut entries {
            if entry.get("origin").and_then(Value::as_str) != Some(origin.as_str()) {
                continue;
            }
            if let Some(object) = entry.as_object_mut() {
                match timing.interval_secs {
                    Some(interval) => {
                        object.insert("interval_secs".into(), json!(interval));
                    }
                    None => {
                        object.remove("interval_secs");
                    }
                }
                match &timing.cron {
                    Some(cron) => {
                        object.insert("cron".into(), json!(cron));
                    }
                    None => {
                        object.remove("cron");
                    }
                }
                object.insert("enabled".into(), json!(enabled));
                changed = true;
            }
        }
        if changed {
            write_schedules(tx.deref_mut(), &node.node_id, &entries).await?;
        }
    }
    let detail = json!({
        "spec_id": current.spec_id,
        "interval_secs": timing.interval_secs,
        "cron": timing.cron,
        "enabled": enabled,
        "resolution_id": current.resolution_id,
        "added": [],
        "removed": [],
    });
    audit_event(
        tx.deref_mut(),
        "schedule.updated",
        "schedule",
        id,
        None,
        &detail,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    load_schedule_view(db, hub, id).await
}

pub(crate) async fn delete_schedule(db: &Db, id: &str) -> Result<(), TargetError> {
    let row = sqlx::query("SELECT spec_id, resolution_id FROM target_schedules WHERE id = ?")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .map_err(db_err)?
        .ok_or(TargetError::ScheduleUnknown)?;
    let spec_id: String = row.try_get("spec_id").map_err(db_err)?;
    let resolution_id: String = row.try_get("resolution_id").map_err(db_err)?;
    let nodes = sqlx::query("SELECT node_id FROM target_schedule_nodes WHERE schedule_id = ?")
        .bind(id)
        .fetch_all(db.pool())
        .await
        .map_err(db_err)?;
    let origin = origin_of(id);
    let mut removed = Vec::new();
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    for node in nodes {
        let node_id: String = node.try_get("node_id").map_err(db_err)?;
        let (entries, _) = lock_config(tx.deref_mut(), &node_id).await?;
        let next: Vec<Value> = entries
            .into_iter()
            .filter(|entry| entry.get("origin").and_then(Value::as_str) != Some(origin.as_str()))
            .collect();
        write_schedules(tx.deref_mut(), &node_id, &next).await?;
        removed.push(node_id);
    }
    sqlx::query("DELETE FROM target_schedules WHERE id = ?")
        .bind(id)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let detail = json!({
        "spec_id": spec_id,
        "resolution_id": resolution_id,
        "added": [],
        "removed": removed,
    });
    audit_event(
        tx.deref_mut(),
        "schedule.deleted",
        "schedule",
        id,
        None,
        &detail,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    Ok(())
}

pub(crate) async fn re_resolve_schedule(
    db: &Db,
    hub: &Hub,
    id: &str,
    body: ReResolveRequest,
) -> Result<ReResolveResponse, TargetError> {
    let current = load_schedule_view(db, hub, id).await?;
    let row = sqlx::query(
        r#"
        SELECT r.selector, r.picked_node_ids_json, r.excluded_node_ids_json, r.id
        FROM target_schedules s
        JOIN target_resolutions r ON r.id = s.resolution_id
        WHERE s.id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(db_err)?
    .ok_or(TargetError::ScheduleUnknown)?;
    let selector: Option<String> = row.try_get("selector").map_err(db_err)?;
    let picked_raw: Option<String> = row.try_get("picked_node_ids_json").map_err(db_err)?;
    let excluded = parse_string_list(
        &row.try_get::<String, _>("excluded_node_ids_json")
            .map_err(db_err)?,
    );
    let old_resolution: String = row.try_get("id").map_err(db_err)?;
    let live = rematch(
        db,
        hub,
        selector.as_deref(),
        picked_raw.as_deref(),
        &excluded,
    )
    .await?;
    let (added, removed) = diff_ids(&current.node_ids, &live);
    if let Some(expected) = &body.expected_diff {
        let expected_added = sorted_unique(&expected.added);
        let expected_removed = sorted_unique(&expected.removed);
        if expected_added != added || expected_removed != removed {
            refuse(
                db,
                "schedule",
                id,
                "target_changed",
                selector.as_deref(),
                &[],
            )
            .await?;
            return Err(TargetError::Changed { added, removed });
        }
    }
    ensure_size(live.len())?;
    let inventory = load_inventory(db).await?;
    let prod_node_ids: Vec<String> = inventory
        .iter()
        .filter(|node| node.prod && live.iter().any(|id| id == &node.node_id))
        .map(|node| node.node_id.clone())
        .collect();
    if body.dry_run {
        return Ok(ReResolveResponse {
            dry_run: true,
            added: added.clone(),
            removed: removed.clone(),
            prod_node_count: prod_node_ids.len(),
            prod_node_ids,
            per_node: project_states(&current, &live, &added),
            resolution_id: None,
            warnings: Vec::new(),
        });
    }
    if !prod_node_ids.is_empty()
        && body.confirm_prod.as_ref().map(|item| item.node_count)
            != Some(prod_node_ids.len() as u64)
    {
        refuse(
            db,
            "schedule",
            id,
            "prod_confirmation_required",
            selector.as_deref(),
            &prod_node_ids,
        )
        .await?;
        return Err(TargetError::Prod {
            prod_node_count: prod_node_ids.len(),
            prod_node_ids,
        });
    }
    let parsed = if let Some(selector) = selector.clone().filter(|value| !value.is_empty()) {
        parse_target(&TargetBody {
            selector: Some(selector),
            node_ids: None,
            exclude_node_ids: excluded.clone(),
            expected_node_ids: Some(live.clone()),
        })?
    } else {
        parse_target(&TargetBody {
            selector: None,
            node_ids: picked_raw
                .as_deref()
                .map(parse_string_list)
                .or_else(|| Some(live.clone())),
            exclude_node_ids: excluded.clone(),
            expected_node_ids: Some(live.clone()),
        })?
    };
    let matched = resolve_nodes_lenient(&inventory, &parsed, &live);
    let mut frozen = freeze(&parsed, &matched, hub, body.confirm_prod.as_ref());
    frozen.node_ids = live.clone();
    frozen.prod_node_ids = prod_node_ids.clone();
    frozen.prod_confirmed = !prod_node_ids.is_empty();
    frozen.resolution_sha256 = sha_of(&live);
    let origin = origin_of(id);
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let resolution_id = insert_resolution(
        tx.deref_mut(),
        "schedule",
        id,
        &frozen,
        Some(&old_resolution),
    )
    .await?;
    sqlx::query("UPDATE target_schedules SET resolution_id = ? WHERE id = ?")
        .bind(&resolution_id)
        .bind(id)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let mut warnings = Vec::new();
    let mut per_node = Vec::new();
    for node_id in &removed {
        let (entries, _) = lock_config(tx.deref_mut(), node_id).await?;
        let next: Vec<Value> = entries
            .into_iter()
            .filter(|entry| entry.get("origin").and_then(Value::as_str) != Some(origin.as_str()))
            .collect();
        write_schedules(tx.deref_mut(), node_id, &next).await?;
        sqlx::query("DELETE FROM target_schedule_nodes WHERE schedule_id = ? AND node_id = ?")
            .bind(id)
            .bind(node_id)
            .execute(tx.deref_mut())
            .await
            .map_err(db_err)?;
        per_node.push(ScheduleNodeState {
            node_id: node_id.clone(),
            state: "removed".into(),
        });
    }
    for node_id in &live {
        if removed.iter().any(|id| id == node_id) {
            continue;
        }
        if added.iter().any(|id| id == node_id) {
            let (entries, specs_json) = lock_config(tx.deref_mut(), node_id).await?;
            if !spec_present(&specs_json, &current.spec_id) {
                warnings.push(Warning {
                    node_id: node_id.clone(),
                    code: "spec_missing".into(),
                });
            }
            let state = if has_spec(&entries, &current.spec_id) {
                "skipped_exists"
            } else {
                let mut next = entries;
                next.push(schedule_entry(
                    &current.spec_id,
                    current.interval_secs,
                    current.cron.as_deref(),
                    current.enabled,
                    &origin,
                ));
                write_schedules(tx.deref_mut(), node_id, &next).await?;
                "active"
            };
            sqlx::query(
                r#"
                INSERT INTO target_schedule_nodes (schedule_id, node_id, state)
                VALUES (?, ?, ?)
                ON DUPLICATE KEY UPDATE state = VALUES(state)
                "#,
            )
            .bind(id)
            .bind(node_id)
            .bind(state)
            .execute(tx.deref_mut())
            .await
            .map_err(db_err)?;
            per_node.push(ScheduleNodeState {
                node_id: node_id.clone(),
                state: state.to_string(),
            });
        } else if let Some(existing) = current.nodes.iter().find(|node| node.node_id == *node_id) {
            per_node.push(existing.clone());
        }
    }
    per_node.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    let detail = json!({
        "spec_id": current.spec_id,
        "interval_secs": current.interval_secs,
        "cron": current.cron,
        "resolution_id": &resolution_id,
        "added": added,
        "removed": removed,
    });
    audit_event(
        tx.deref_mut(),
        "schedule.re_resolved",
        "schedule",
        id,
        None,
        &detail,
    )
    .await?;
    tx.commit().await.map_err(db_err)?;
    Ok(ReResolveResponse {
        dry_run: false,
        prod_node_count: prod_node_ids.len(),
        prod_node_ids,
        resolution_id: Some(resolution_id),
        added,
        removed,
        per_node,
        warnings,
    })
}

fn resolve_nodes_lenient<'a>(
    inventory: &'a [InventoryNode],
    _parsed: &ParsedTarget,
    live: &[String],
) -> Vec<&'a InventoryNode> {
    let by_id: BTreeMap<&str, &InventoryNode> = inventory
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect();
    live.iter()
        .filter_map(|id| by_id.get(id.as_str()).copied())
        .collect()
}

fn project_states(
    current: &ScheduleView,
    live: &[String],
    added: &[String],
) -> Vec<ScheduleNodeState> {
    let mut out = Vec::new();
    for node_id in live {
        if added.iter().any(|id| id == node_id) {
            out.push(ScheduleNodeState {
                node_id: node_id.clone(),
                state: "active".into(),
            });
        } else if let Some(existing) = current.nodes.iter().find(|node| node.node_id == *node_id) {
            out.push(existing.clone());
        } else {
            out.push(ScheduleNodeState {
                node_id: node_id.clone(),
                state: "active".into(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limit_is_five_thousand() {
        assert!(ensure_size(MAX_TARGETS).is_ok());
        assert!(matches!(
            ensure_size(MAX_TARGETS + 1),
            Err(TargetError::TooMany)
        ));
    }

    #[test]
    fn diff_reports_added_and_removed() {
        let (added, removed) = diff_ids(&["a".into(), "b".into()], &["b".into(), "c".into()]);
        assert_eq!(added, vec!["c".to_string()]);
        assert_eq!(removed, vec!["a".to_string()]);
        assert!(same_ids(
            &["b".into(), "a".into()],
            &["a".into(), "b".into()]
        ));
    }

    #[test]
    fn exactly_one_of_selector_or_node_ids() {
        let both = TargetBody {
            selector: Some("role=db".into()),
            node_ids: Some(vec!["n".into()]),
            exclude_node_ids: Vec::new(),
            expected_node_ids: None,
        };
        assert!(parse_target(&both).is_err());
        let neither = TargetBody {
            selector: None,
            node_ids: None,
            exclude_node_ids: Vec::new(),
            expected_node_ids: None,
        };
        assert!(parse_target(&neither).is_err());
        let picked = TargetBody {
            selector: None,
            node_ids: Some(vec!["b".into(), "a".into(), "a".into()]),
            exclude_node_ids: vec!["b".into()],
            expected_node_ids: None,
        };
        let parsed = parse_target(&picked).expect("pick");
        assert_eq!(
            parsed.picked.unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use crate::db::connect_test_db;
    use crate::http::{router, AppState};
    use crate::hub::Hub;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use sqlx::Row;
    use std::collections::HashMap;
    use tower::ServiceExt;

    async fn send(
        db: crate::db::Db,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        format!("ntb-{prefix}-{}", Uuid::new_v4().simple())
    }

    fn tag_value() -> String {
        format!("t{}", Uuid::new_v4().simple())
    }

    async fn reg(db: &crate::db::Db, id: &str, tags: &[(&str, &str)]) {
        let map = tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        crate::tags::register_node(db, id, "host", "0.1.0", &map, "linux", "arm64")
            .await
            .expect("register");
    }

    fn ids(body: &Value) -> Vec<String> {
        body.get("node_ids")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    }

    async fn resolution_count(db: &crate::db::Db, selector: &str) -> i64 {
        let row = sqlx::query("SELECT COUNT(*) AS n FROM target_resolutions WHERE selector = ?")
            .bind(selector)
            .fetch_one(db.pool())
            .await
            .expect("count");
        row.try_get("n").expect("n")
    }

    #[tokio::test]
    async fn resolve_selector_node_ids_and_exclude() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let tid = tag_value();
        let selector = format!("tid={tid}");
        let alpha = nid("a");
        let beta = nid("b");
        reg(&db, &alpha, &[("tid", &tid), ("role", "db")]).await;
        reg(&db, &beta, &[("tid", &tid), ("role", "web")]).await;

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({ "target": { "selector": selector } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), vec![alpha.clone(), beta.clone()]);
        assert_eq!(body["count"], 2);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({ "target": { "selector": selector, "exclude_node_ids": [beta] } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), vec![alpha.clone()]);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({
                "target": { "node_ids": [beta, alpha], "exclude_node_ids": [alpha] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(ids(&body), vec![beta.clone()]);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({ "target": { "node_ids": [format!("missing-{}", tid)] } })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(code(&body), "node_unknown");

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({ "target": { "selector": selector, "node_ids": [alpha] } })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(code(&body), "target_invalid");

        let missing = format!("tid={}", tag_value());
        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/targets/resolve",
            Some(json!({ "target": { "selector": missing } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["count"], 0);
    }

    #[tokio::test]
    async fn expected_mismatch_writes_nothing_and_empty_is_unprocessable() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let tid = tag_value();
        let selector = format!("tid={tid}");
        let alpha = nid("a");
        let beta = nid("b");
        reg(&db, &alpha, &[("tid", &tid)]).await;
        reg(&db, &beta, &[("tid", &tid)]).await;

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({
                "spec_id": "cpu",
                "target": { "selector": selector, "expected_node_ids": [alpha] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(code(&body), "target_changed");
        assert_eq!(body["added"], json!([beta]));
        assert_eq!(body["removed"], json!([]));
        assert_eq!(resolution_count(&db, &selector).await, 0);

        let empty = format!("tid={}", tag_value());
        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({ "spec_id": "cpu", "target": { "selector": empty } })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(code(&body), "no_target_nodes");
        assert_eq!(resolution_count(&db, &empty).await, 0);
    }

    #[tokio::test]
    async fn prod_confirm_guards_target_and_single_node_dispatch() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let tid = tag_value();
        let selector = format!("tid={tid}");
        let prod = nid("prod");
        let plain = nid("plain");
        reg(&db, &prod, &[("tid", &tid), ("env", "prod")]).await;
        reg(&db, &plain, &[("role", "web")]).await;

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({ "spec_id": "cpu", "target": { "selector": selector } })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(code(&body), "prod_confirmation_required");
        assert_eq!(body["prod_node_count"], 1);
        assert_eq!(resolution_count(&db, &selector).await, 0);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({
                "spec_id": "cpu",
                "target": { "selector": selector },
                "confirm_prod": { "node_count": 9 }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(resolution_count(&db, &selector).await, 0);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({
                "spec_id": "cpu",
                "target": { "selector": selector },
                "confirm_prod": { "node_count": 1 }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(ids(&body["resolution"]), vec![prod.clone()]);
        assert!(body["operation_id"].as_str().is_some());
        assert_eq!(resolution_count(&db, &selector).await, 1);

        for id in [&prod, &plain] {
            let (status, body) = send(
                db.clone(),
                "PUT",
                &format!("/v1/nodes/{id}/config"),
                Some(json!({ "specs": [{ "id": "cpu", "kind": "probe" }] })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{id} {body}");
        }

        let (status, body) = send(
            db.clone(),
            "POST",
            &format!("/v1/nodes/{prod}/dispatch"),
            Some(json!({ "spec_id": "cpu" })),
        )
        .await;
        assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
        assert_eq!(code(&body), "prod_confirmation_required");

        let (status, body) = send(
            db.clone(),
            "POST",
            &format!("/v1/nodes/{prod}/dispatch"),
            Some(json!({ "spec_id": "cpu", "confirm_prod": { "node_count": 1 } })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["node_id"], prod);
        assert!(body["run_id"].as_str().is_some());

        let (status, body) = send(
            db.clone(),
            "POST",
            &format!("/v1/nodes/{plain}/dispatch"),
            Some(json!({ "spec_id": "cpu" })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }

    #[tokio::test]
    async fn dispatch_fleet_and_schedule_freeze_the_list() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let tid = tag_value();
        let selector = format!("tid={tid}");
        let alpha = nid("a");
        let beta = nid("b");
        reg(&db, &alpha, &[("tid", &tid)]).await;

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/dispatch",
            Some(json!({ "spec_id": "cpu", "target": { "selector": selector } })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let operation_id = body["operation_id"]
            .as_str()
            .expect("operation")
            .to_string();
        assert_eq!(ids(&body["resolution"]), vec![alpha.clone()]);

        let (status, stored) = send(
            db.clone(),
            "GET",
            &format!("/v1/dispatch-operations/{operation_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{stored}");
        assert_eq!(ids(&stored["resolution"]), vec![alpha.clone()]);

        let (status, body) = send(
            db.clone(),
            "POST",
            "/v1/fleet/skill-groups/push",
            Some(json!({
                "group_id": "host-basics",
                "target": { "selector": selector }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(ids(&body["resolution"]), vec![alpha.clone()]);
        assert!(body["resolution"]["id"].as_str().is_some());

        let (status, created) = send(
            db.clone(),
            "POST",
            "/v1/schedules",
            Some(json!({
                "spec_id": "cpu",
                "interval_secs": 60,
                "target": { "selector": selector }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let schedule_id = created["id"].as_str().expect("schedule").to_string();
        assert_eq!(ids(&created["resolution"]), vec![alpha.clone()]);

        reg(&db, &beta, &[("tid", &tid)]).await;
        let (status, view) = send(
            db.clone(),
            "GET",
            &format!("/v1/schedules/{schedule_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert!(view["resolved_at"].as_str().is_some());
        assert_eq!(view["drift"]["added"], json!([beta]));
        assert_eq!(view["drift"]["removed"], json!([]));

        let (status, dry) = send(
            db.clone(),
            "POST",
            &format!("/v1/schedules/{schedule_id}/re-resolve"),
            Some(json!({ "dry_run": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{dry}");
        assert_eq!(dry["dry_run"], true);
        assert_eq!(dry["added"], json!([beta]));

        let (status, applied) = send(
            db.clone(),
            "POST",
            &format!("/v1/schedules/{schedule_id}/re-resolve"),
            Some(json!({
                "dry_run": false,
                "expected_diff": { "added": [beta], "removed": [] }
            })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{applied}");
        assert!(applied["resolution_id"].as_str().is_some());

        let (status, view) = send(
            db.clone(),
            "GET",
            &format!("/v1/schedules/{schedule_id}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        let mut frozen = ids(&view);
        frozen.sort();
        let mut expect = vec![alpha, beta];
        expect.sort();
        assert_eq!(frozen, expect);
        assert_eq!(view["drift"]["added"], json!([]));
    }
}
