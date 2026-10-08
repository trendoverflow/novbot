// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Per-node desired skill set.
//!
//! Install, upgrade, rollback, and uninstall update `node_skills_desired` and
//! `node_skill_sets.generation` in one transaction with a skill operation and
//! an audit event. A connected node is `pending`; otherwise the change is
//! `queued`. This module does not send gRPC messages.

use crate::artifact::sha256_hex;
use crate::db::Db;
use crate::hub::Hub;
use crate::skill_catalog::is_builtin_name;
use chrono::{DateTime, Utc};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{MySql, Row, Transaction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::DerefMut;
use uuid::Uuid;

const ACTOR: &str = "api_token";

#[derive(Debug)]
pub(crate) enum DesiredError {
    SkillUnknown,
    BuiltinReadOnly,
    VersionUnknown,
    VersionNotInstallable,
    CapabilitiesChanged,
    NoTargetNodes,
    NoPreviousVersion,
    SkillInUse {
        spec_ids: Vec<String>,
        schedule_ids: Vec<String>,
    },
    NodeUnknown,
    OperationUnknown,
    BundleUnknown,
    BundleExists,
    InvalidBundle(String),
    BadRequest(String),
    Internal(anyhow::Error),
}

fn db_err(err: sqlx::Error) -> DesiredError {
    DesiredError::Internal(err.into())
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SelectorBody {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Selection {
    pub node_ids: Vec<String>,
    pub selector: Option<BTreeMap<String, String>>,
}

impl Selection {
    pub(crate) fn new(node_ids: Vec<String>, selector: Option<SelectorBody>) -> Self {
        Self {
            node_ids: dedupe(node_ids),
            selector: selector.map(|body| body.labels),
        }
    }

    fn is_blank(&self) -> bool {
        self.node_ids.is_empty() && self.selector.is_none()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct InstallRequest {
    pub version: String,
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub selector: Option<SelectorBody>,
    #[serde(default)]
    pub accepted_capabilities_sha256: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RollbackRequest {
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub selector: Option<SelectorBody>,
    #[serde(default)]
    pub to_version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UninstallRequest {
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub selector: Option<SelectorBody>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PerNode {
    pub node_id: String,
    pub outcome: String,
    pub generation: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChangeResponse {
    pub operation_id: Option<String>,
    pub dry_run: bool,
    pub per_node: Vec<PerNode>,
}

#[derive(Debug, Clone)]
pub(crate) struct InstallPlan {
    pub skill_name: String,
    pub version: String,
    pub sha256: String,
    pub capabilities_sha256: String,
    pub min_node_version: Option<String>,
    pub platforms: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct SkillChange {
    pub skill_name: String,
    pub operation_id: Option<String>,
    pub per_node: Vec<PerNode>,
}

#[derive(Debug, Clone)]
pub(crate) struct TargetView {
    pub node_id: String,
    pub exists: bool,
    pub generation: i64,
}

pub(crate) struct BatchResult {
    pub changes: Vec<SkillChange>,
    pub targets: Vec<TargetView>,
}

#[derive(Debug, Clone)]
pub(crate) struct VersionRecord {
    pub version: String,
    pub sha256: String,
    pub status: String,
    pub min_node_version: Option<String>,
    pub platforms: Vec<String>,
    pub capabilities_sha256: String,
    pub semver: Option<Version>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListLimit {
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AuditQuery {
    pub action: Option<String>,
    pub node_id: Option<String>,
    pub since: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OperationView {
    pub id: String,
    #[serde(rename = "type")]
    pub op_type: String,
    pub skill_name: String,
    pub version: Option<String>,
    pub per_node: Value,
    pub capabilities_sha256: Option<String>,
    pub force: bool,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub(crate) struct OperationList {
    pub items: Vec<OperationView>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditView {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: String,
    pub node_id: Option<String>,
    pub detail: Value,
    pub prev_hash: Option<String>,
    pub hash: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct AuditList {
    pub items: Vec<AuditView>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NodeSkillsResponse {
    pub node_id: String,
    pub generation: i64,
    pub applied_generation: i64,
    pub items: Vec<NodeSkillItem>,
}

#[derive(Debug, Serialize)]
pub(crate) struct NodeSkillItem {
    pub name: String,
    pub source: String,
    pub state: String,
    pub desired_version: Option<String>,
    pub actual_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CapabilityCatalog {
    pub items: Vec<CapabilityInfo>,
}

#[derive(Debug, Serialize)]
pub(crate) struct CapabilityInfo {
    pub name: &'static str,
    pub scope: Option<&'static str>,
    pub risk: &'static str,
    pub description: &'static str,
}

type Tx<'a> = Transaction<'a, MySql>;

#[derive(Clone)]
struct LiveNode {
    node_id: String,
    agent_version: String,
    labels: BTreeMap<String, String>,
}

#[derive(Clone)]
struct DesiredSnap {
    version: String,
    sha256: String,
}

#[derive(Clone)]
enum Slot {
    Missing(String),
    Node(LiveNode),
}

impl Slot {
    fn id(&self) -> &str {
        match self {
            Self::Missing(id) => id,
            Self::Node(node) => &node.node_id,
        }
    }

    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing(_))
    }
}

enum Action {
    Upsert { version: String, sha256: String },
    Delete,
}

struct NodePlan {
    node_id: String,
    outcome: String,
    generation: i64,
    action: Option<Action>,
    other_version: bool,
}

pub(crate) async fn install(
    db: &Db,
    hub: &Hub,
    name: &str,
    body: InstallRequest,
) -> Result<ChangeResponse, DesiredError> {
    if is_builtin_name(name) {
        return Err(DesiredError::BuiltinReadOnly);
    }
    let versions = require_versions(db, name).await?;
    let version = versions
        .iter()
        .find(|row| row.version == body.version)
        .ok_or(DesiredError::VersionUnknown)?;
    if matches!(version.status.as_str(), "deprecated" | "yanked") {
        return Err(DesiredError::VersionNotInstallable);
    }
    let capabilities_sha256 = accepted_hash(
        &version.capabilities_sha256,
        body.accepted_capabilities_sha256.as_deref(),
    )?;
    let plan = InstallPlan {
        skill_name: name.to_string(),
        version: version.version.clone(),
        sha256: version.sha256.clone(),
        capabilities_sha256,
        min_node_version: version.min_node_version.clone(),
        platforms: version.platforms.clone(),
    };
    let selection = Selection::new(body.node_ids, body.selector);
    let batch = apply_installs(db, hub, vec![plan], selection, body.dry_run).await?;
    let change = batch
        .changes
        .into_iter()
        .next()
        .ok_or_else(|| DesiredError::Internal(anyhow::anyhow!("install plan missing")))?;
    Ok(ChangeResponse {
        operation_id: change.operation_id,
        dry_run: body.dry_run,
        per_node: change.per_node,
    })
}

pub(crate) async fn rollback(
    db: &Db,
    hub: &Hub,
    name: &str,
    body: RollbackRequest,
) -> Result<ChangeResponse, DesiredError> {
    if is_builtin_name(name) {
        return Err(DesiredError::BuiltinReadOnly);
    }
    let versions = require_versions(db, name).await?;
    let explicit = match body
        .to_version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(version) => {
            let found = versions
                .iter()
                .find(|row| row.version == version)
                .ok_or(DesiredError::VersionUnknown)?;
            if found.status == "yanked" {
                return Err(DesiredError::VersionNotInstallable);
            }
            Some(found.clone())
        }
        None => None,
    };
    let selection = Selection::new(body.node_ids, body.selector);
    let (mut tx, slots) = prepare(db, &selection).await?;
    if slots.iter().all(Slot::is_missing) {
        let per_node = missing_per_node(&slots);
        tx.rollback().await.map_err(db_err)?;
        return Ok(ChangeResponse {
            operation_id: None,
            dry_run: false,
            per_node,
        });
    }
    let desired = load_desired_map(&mut tx, name).await?;
    let gens = load_generation_map(&mut tx).await?;
    let plans: Vec<NodePlan> = slots
        .iter()
        .map(|slot| classify_rollback(slot, &desired, &gens, explicit.as_ref(), &versions, hub))
        .collect();
    if explicit.is_none()
        && plans
            .iter()
            .filter(|plan| plan.outcome != "node_unknown")
            .all(|plan| plan.outcome == "no_previous_version")
    {
        tx.rollback().await.map_err(db_err)?;
        return Err(DesiredError::NoPreviousVersion);
    }
    let version = rollback_version(explicit.as_ref(), &plans);
    let operation_id = persist(
        &mut tx,
        name,
        "rollback",
        version.as_deref(),
        None,
        false,
        &plans,
    )
    .await?;
    let per_node = per_nodes(&plans);
    tx.commit().await.map_err(db_err)?;
    Ok(ChangeResponse {
        operation_id: Some(operation_id),
        dry_run: false,
        per_node,
    })
}

pub(crate) async fn uninstall(
    db: &Db,
    hub: &Hub,
    name: &str,
    body: UninstallRequest,
) -> Result<ChangeResponse, DesiredError> {
    if is_builtin_name(name) {
        return Err(DesiredError::BuiltinReadOnly);
    }
    require_versions(db, name).await?;
    let selection = Selection::new(body.node_ids, body.selector);
    let (mut tx, slots) = prepare(db, &selection).await?;
    if slots.iter().all(Slot::is_missing) {
        let per_node = missing_per_node(&slots);
        tx.rollback().await.map_err(db_err)?;
        return Ok(ChangeResponse {
            operation_id: None,
            dry_run: false,
            per_node,
        });
    }
    if !body.force {
        let (spec_ids, schedule_ids) = skill_in_use(&mut tx, &slots, name).await?;
        if !spec_ids.is_empty() {
            tx.rollback().await.map_err(db_err)?;
            return Err(DesiredError::SkillInUse {
                spec_ids,
                schedule_ids,
            });
        }
    }
    let desired = load_desired_map(&mut tx, name).await?;
    let gens = load_generation_map(&mut tx).await?;
    let plans: Vec<NodePlan> = slots
        .iter()
        .map(|slot| classify_uninstall(slot, &desired, &gens, hub))
        .collect();
    let operation_id = persist(&mut tx, name, "uninstall", None, None, body.force, &plans).await?;
    let per_node = per_nodes(&plans);
    tx.commit().await.map_err(db_err)?;
    Ok(ChangeResponse {
        operation_id: Some(operation_id),
        dry_run: false,
        per_node,
    })
}

pub(crate) async fn apply_installs(
    db: &Db,
    hub: &Hub,
    plans: Vec<InstallPlan>,
    selection: Selection,
    dry_run: bool,
) -> Result<BatchResult, DesiredError> {
    let (mut tx, slots) = prepare(db, &selection).await?;
    let read_only = dry_run || slots.iter().all(Slot::is_missing) || plans.is_empty();
    let changes = if read_only {
        preview_installs(&mut tx, hub, &plans, &slots).await?
    } else {
        write_installs(&mut tx, hub, &plans, &slots).await?
    };
    let targets = target_views(&mut tx, &slots).await?;
    if read_only {
        tx.rollback().await.map_err(db_err)?;
    } else {
        tx.commit().await.map_err(db_err)?;
    }
    Ok(BatchResult { changes, targets })
}

pub(crate) async fn published_versions(
    db: &Db,
    name: &str,
) -> Result<Vec<VersionRecord>, DesiredError> {
    let Some(skill_id) = skill_id(db, name).await? else {
        return Ok(Vec::new());
    };
    load_versions(db, skill_id).await
}

pub(crate) fn highest_published<'a>(
    req: &str,
    versions: &'a [VersionRecord],
) -> Option<&'a VersionRecord> {
    versions
        .iter()
        .filter(|row| {
            row.status == "published"
                && row
                    .semver
                    .as_ref()
                    .is_some_and(|semver| version_matches(req, semver))
        })
        .max_by(|left, right| left.semver.cmp(&right.semver))
}

pub(crate) fn version_matches(req: &str, version: &Version) -> bool {
    let req = req.trim();
    if req.is_empty() {
        return false;
    }
    if req == "*" {
        return true;
    }
    if let Ok(exact) = Version::parse(req) {
        return version == &exact;
    }
    VersionReq::parse(req).is_ok_and(|parsed| parsed.matches(version))
}

pub(crate) fn accepted_hash(stored: &str, provided: Option<&str>) -> Result<String, DesiredError> {
    let stored_norm = stored.trim().to_ascii_lowercase();
    let Some(provided) = provided.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(DesiredError::CapabilitiesChanged);
    };
    if !provided.eq_ignore_ascii_case(&stored_norm) {
        return Err(DesiredError::CapabilitiesChanged);
    }
    Ok(stored_norm)
}

pub(crate) fn capability_catalog() -> CapabilityCatalog {
    CapabilityCatalog {
        items: vec![
            cap("fs.read", Some("path"), "medium", "Read files."),
            cap("fs.stat", Some("path"), "low", "Stat files."),
            cap("fs.list", Some("path"), "medium", "List directory entries."),
            cap(
                "env.read",
                Some("env"),
                "high",
                "Read environment variables.",
            ),
            cap(
                "net.listening_ports.read",
                None,
                "medium",
                "Read listening network ports.",
            ),
            cap("sys.info.read", None, "low", "Read system information."),
            cap(
                "sys.time_sync.read",
                None,
                "low",
                "Read time synchronization status.",
            ),
            cap("sys.metrics.read", None, "low", "Read system metrics."),
            cap("proc.list.read", None, "medium", "List processes."),
            cap(
                "net.interfaces.read",
                None,
                "low",
                "Read network interfaces.",
            ),
        ],
    }
}

pub(crate) async fn list_operations(
    db: &Db,
    limit: Option<i64>,
) -> Result<OperationList, DesiredError> {
    let limit = clamp_limit(limit);
    let rows = sqlx::query(
        r#"
        SELECT id, `type` AS op_type, skill_name, version, targets_json,
               capabilities_sha256, force_flag, actor, created_at
        FROM skill_operations
        ORDER BY created_at DESC, id DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(operation_view(&row)?);
    }
    Ok(OperationList { items })
}

pub(crate) async fn get_operation(db: &Db, id: &str) -> Result<OperationView, DesiredError> {
    let row = sqlx::query(
        r#"
        SELECT id, `type` AS op_type, skill_name, version, targets_json,
               capabilities_sha256, force_flag, actor, created_at
        FROM skill_operations
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await
    .map_err(db_err)?
    .ok_or(DesiredError::OperationUnknown)?;
    operation_view(&row)
}

pub(crate) async fn list_audit(db: &Db, query: AuditQuery) -> Result<AuditList, DesiredError> {
    let limit = clamp_limit(query.limit);
    let since = match query.since.as_deref() {
        Some(raw) => Some(parse_since(raw)?),
        None => None,
    };
    let mut sql = String::from(
        r#"SELECT id, `at`, actor_type, actor_id, action, target_type, target_id, node_id, detail_json, prev_hash, `hash`
           FROM audit_events WHERE 1=1"#,
    );
    if query.action.is_some() {
        sql.push_str(" AND action = ?");
    }
    if query.node_id.is_some() {
        sql.push_str(" AND node_id = ?");
    }
    if since.is_some() {
        sql.push_str(" AND `at` >= ?");
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    let mut statement = sqlx::query(&sql);
    if let Some(action) = &query.action {
        statement = statement.bind(action);
    }
    if let Some(node_id) = &query.node_id {
        statement = statement.bind(node_id);
    }
    if let Some(since) = since {
        statement = statement.bind(since);
    }
    statement = statement.bind(limit);
    let rows = statement.fetch_all(db.pool()).await.map_err(db_err)?;
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        items.push(audit_view(&row)?);
    }
    Ok(AuditList { items })
}

pub(crate) async fn node_skills(
    db: &Db,
    hub: &Hub,
    node_id: &str,
) -> Result<NodeSkillsResponse, DesiredError> {
    let exists = sqlx::query("SELECT node_id FROM nodes WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(db.pool())
        .await
        .map_err(db_err)?;
    if exists.is_none() {
        return Err(DesiredError::NodeUnknown);
    }
    let generation = sqlx::query("SELECT generation FROM node_skill_sets WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(db.pool())
        .await
        .map_err(db_err)?
        .map(|row| row.try_get::<i64, _>("generation"))
        .transpose()
        .map_err(db_err)?
        .unwrap_or(0);
    let applied = sqlx::query(
        "SELECT MAX(applied_generation) AS applied FROM node_skills_actual WHERE node_id = ?",
    )
    .bind(node_id)
    .fetch_one(db.pool())
    .await
    .map_err(db_err)?;
    let applied_generation: Option<i64> = applied.try_get("applied").map_err(db_err)?;
    let desired_rows = sqlx::query(
        "SELECT skill_name, version, sha256 FROM node_skills_desired WHERE node_id = ? ORDER BY skill_name",
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let actual_rows = sqlx::query(
        "SELECT skill_name, version, sha256, state FROM node_skills_actual WHERE node_id = ?",
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut actuals = HashMap::new();
    for row in actual_rows {
        let skill_name: String = row.try_get("skill_name").map_err(db_err)?;
        actuals.insert(
            skill_name,
            ActualSnap {
                version: row.try_get("version").map_err(db_err)?,
                sha256: trim_text(row.try_get("sha256").map_err(db_err)?),
                state: row.try_get("state").map_err(db_err)?,
            },
        );
    }
    let connected = hub.is_connected(node_id);
    let mut items = Vec::new();
    for name in novbot_core::list_skills() {
        items.push(NodeSkillItem {
            name: name.to_string(),
            source: "builtin".to_string(),
            state: "installed".to_string(),
            desired_version: None,
            actual_version: None,
        });
    }
    for row in desired_rows {
        let name: String = row.try_get("skill_name").map_err(db_err)?;
        let version: String = row.try_get("version").map_err(db_err)?;
        let sha256 = trim_text(row.try_get("sha256").map_err(db_err)?);
        let actual = actuals.get(&name);
        let state = derived_state(connected, &sha256, actual);
        items.push(NodeSkillItem {
            name,
            source: "hub".to_string(),
            state: state.to_string(),
            desired_version: Some(version),
            actual_version: actual.map(|row| row.version.clone()),
        });
    }
    Ok(NodeSkillsResponse {
        node_id: node_id.to_string(),
        generation,
        applied_generation: applied_generation.unwrap_or(0),
        items,
    })
}

struct ActualSnap {
    version: String,
    sha256: String,
    state: String,
}

fn derived_state(connected: bool, desired_sha: &str, actual: Option<&ActualSnap>) -> &'static str {
    if let Some(actual) = actual {
        if actual.state == "installed" && actual.sha256.eq_ignore_ascii_case(desired_sha) {
            return "installed";
        }
    }
    if connected {
        "pending"
    } else {
        "queued"
    }
}

async fn require_versions(db: &Db, name: &str) -> Result<Vec<VersionRecord>, DesiredError> {
    let Some(skill_id) = skill_id(db, name).await? else {
        return Err(DesiredError::SkillUnknown);
    };
    load_versions(db, skill_id).await
}

async fn skill_id(db: &Db, name: &str) -> Result<Option<i64>, DesiredError> {
    let row = sqlx::query("SELECT id FROM skills WHERE name = ?")
        .bind(name)
        .fetch_optional(db.pool())
        .await
        .map_err(db_err)?;
    match row {
        Some(row) => Ok(Some(row.try_get("id").map_err(db_err)?)),
        None => Ok(None),
    }
}

async fn load_versions(db: &Db, skill_id: i64) -> Result<Vec<VersionRecord>, DesiredError> {
    let rows = sqlx::query(
        r#"
        SELECT version, sha256, status, min_node_version, platforms_json, capabilities_sha256
        FROM skill_versions
        WHERE skill_id = ?
        "#,
    )
    .bind(skill_id)
    .fetch_all(db.pool())
    .await
    .map_err(db_err)?;
    let mut versions = Vec::with_capacity(rows.len());
    for row in rows {
        versions.push(version_record(&row)?);
    }
    Ok(versions)
}

fn version_record(row: &sqlx::mysql::MySqlRow) -> Result<VersionRecord, DesiredError> {
    let version: String = row.try_get("version").map_err(db_err)?;
    let min_node_version: Option<String> = row.try_get("min_node_version").map_err(db_err)?;
    let platforms_json: Option<String> = row.try_get("platforms_json").map_err(db_err)?;
    Ok(VersionRecord {
        semver: Version::parse(&version).ok(),
        version,
        sha256: trim_text(row.try_get("sha256").map_err(db_err)?),
        status: row.try_get("status").map_err(db_err)?,
        min_node_version: nonempty(min_node_version),
        platforms: parse_string_list(platforms_json),
        capabilities_sha256: trim_text(row.try_get("capabilities_sha256").map_err(db_err)?),
    })
}

async fn prepare<'a>(
    db: &'a Db,
    selection: &Selection,
) -> Result<(Tx<'a>, Vec<Slot>), DesiredError> {
    if selection.is_blank() {
        return Err(DesiredError::NoTargetNodes);
    }
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let live = load_live_nodes(&mut tx).await?;
    let slots = resolve(&live, selection);
    if slots.is_empty() {
        tx.rollback().await.map_err(db_err)?;
        return Err(DesiredError::NoTargetNodes);
    }
    Ok((tx, slots))
}

async fn load_live_nodes(tx: &mut Tx<'_>) -> Result<Vec<LiveNode>, DesiredError> {
    let rows = sqlx::query("SELECT node_id, version, labels_json FROM nodes")
        .fetch_all(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let mut nodes = Vec::with_capacity(rows.len());
    for row in rows {
        let labels_json: String = row.try_get("labels_json").map_err(db_err)?;
        nodes.push(LiveNode {
            node_id: row.try_get("node_id").map_err(db_err)?,
            agent_version: row.try_get("version").map_err(db_err)?,
            labels: parse_labels(&labels_json),
        });
    }
    Ok(nodes)
}

fn resolve(nodes: &[LiveNode], selection: &Selection) -> Vec<Slot> {
    let by_id: HashMap<&str, &LiveNode> = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect();
    let mut seen = HashSet::new();
    let mut slots = Vec::new();
    for id in &selection.node_ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        match by_id.get(id.as_str()) {
            Some(node) => slots.push(Slot::Node((*node).clone())),
            None => slots.push(Slot::Missing(id.clone())),
        }
    }
    if let Some(labels) = &selection.selector {
        let mut extra: Vec<&LiveNode> = nodes
            .iter()
            .filter(|node| !seen.contains(&node.node_id) && labels_match(&node.labels, labels))
            .collect();
        extra.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        for node in extra {
            seen.insert(node.node_id.clone());
            slots.push(Slot::Node(node.clone()));
        }
    }
    slots
}

async fn preview_installs(
    tx: &mut Tx<'_>,
    hub: &Hub,
    plans: &[InstallPlan],
    slots: &[Slot],
) -> Result<Vec<SkillChange>, DesiredError> {
    let mut changes = Vec::with_capacity(plans.len());
    for plan in plans {
        let desired = load_desired_map(tx, &plan.skill_name).await?;
        let gens = load_generation_map(tx).await?;
        let node_plans: Vec<NodePlan> = slots
            .iter()
            .map(|slot| classify_install(slot, &desired, &gens, plan, hub, false))
            .collect();
        changes.push(SkillChange {
            skill_name: plan.skill_name.clone(),
            operation_id: None,
            per_node: per_nodes(&node_plans),
        });
    }
    Ok(changes)
}

async fn write_installs(
    tx: &mut Tx<'_>,
    hub: &Hub,
    plans: &[InstallPlan],
    slots: &[Slot],
) -> Result<Vec<SkillChange>, DesiredError> {
    let mut changes = Vec::with_capacity(plans.len());
    for plan in plans {
        let desired = load_desired_map(tx, &plan.skill_name).await?;
        let gens = load_generation_map(tx).await?;
        let node_plans: Vec<NodePlan> = slots
            .iter()
            .map(|slot| classify_install(slot, &desired, &gens, plan, hub, true))
            .collect();
        let op_type = if node_plans.iter().any(|plan| plan.other_version) {
            "upgrade"
        } else {
            "install"
        };
        let operation_id = if node_plans.iter().any(|plan| plan.outcome != "node_unknown") {
            Some(
                persist(
                    tx,
                    &plan.skill_name,
                    op_type,
                    Some(&plan.version),
                    Some(&plan.capabilities_sha256),
                    false,
                    &node_plans,
                )
                .await?,
            )
        } else {
            None
        };
        changes.push(SkillChange {
            skill_name: plan.skill_name.clone(),
            operation_id,
            per_node: per_nodes(&node_plans),
        });
    }
    Ok(changes)
}

fn classify_install(
    slot: &Slot,
    desired_map: &HashMap<String, DesiredSnap>,
    gens: &HashMap<String, i64>,
    plan: &InstallPlan,
    hub: &Hub,
    write: bool,
) -> NodePlan {
    let Slot::Node(node) = slot else {
        return missing_plan(slot.id());
    };
    let desired = desired_map.get(&node.node_id);
    let current = gens.get(&node.node_id).copied().unwrap_or(0);
    if node_too_old(&node.agent_version, plan.min_node_version.as_deref()) {
        return hold(node, "node_too_old", current, desired, &plan.version);
    }
    if platform_unsupported(&node.labels, &plan.platforms) {
        return hold(
            node,
            "platform_unsupported",
            current,
            desired,
            &plan.version,
        );
    }
    if desired.is_some_and(|row| same_desired(row, &plan.version, &plan.sha256)) {
        return hold(node, "already_installed", current, desired, &plan.version);
    }
    write_plan(
        node,
        current,
        desired,
        &plan.version,
        hub,
        write,
        Action::Upsert {
            version: plan.version.clone(),
            sha256: plan.sha256.clone(),
        },
    )
}

fn classify_rollback(
    slot: &Slot,
    desired_map: &HashMap<String, DesiredSnap>,
    gens: &HashMap<String, i64>,
    explicit: Option<&VersionRecord>,
    versions: &[VersionRecord],
    hub: &Hub,
) -> NodePlan {
    let Slot::Node(node) = slot else {
        return missing_plan(slot.id());
    };
    let desired = desired_map.get(&node.node_id);
    let current = gens.get(&node.node_id).copied().unwrap_or(0);
    let target = if let Some(explicit) = explicit {
        explicit
    } else {
        let Some(desired_row) = desired else {
            return hold(node, "no_previous_version", current, desired, "");
        };
        let Some(current_version) = Version::parse(&desired_row.version).ok() else {
            return hold(node, "no_previous_version", current, desired, "");
        };
        let Some(previous) = previous_version(versions, &current_version) else {
            return hold(node, "no_previous_version", current, desired, "");
        };
        previous
    };
    if node_too_old(&node.agent_version, target.min_node_version.as_deref()) {
        return hold(node, "node_too_old", current, desired, &target.version);
    }
    if platform_unsupported(&node.labels, &target.platforms) {
        return hold(
            node,
            "platform_unsupported",
            current,
            desired,
            &target.version,
        );
    }
    if desired.is_some_and(|row| same_desired(row, &target.version, &target.sha256)) {
        return hold(node, "already_installed", current, desired, &target.version);
    }
    write_plan(
        node,
        current,
        desired,
        &target.version,
        hub,
        true,
        Action::Upsert {
            version: target.version.clone(),
            sha256: target.sha256.clone(),
        },
    )
}

fn classify_uninstall(
    slot: &Slot,
    desired_map: &HashMap<String, DesiredSnap>,
    gens: &HashMap<String, i64>,
    hub: &Hub,
) -> NodePlan {
    let Slot::Node(node) = slot else {
        return missing_plan(slot.id());
    };
    let desired = desired_map.get(&node.node_id);
    let current = gens.get(&node.node_id).copied().unwrap_or(0);
    if desired.is_none() {
        return hold(node, "already_installed", current, desired, "");
    }
    write_plan(node, current, desired, "", hub, true, Action::Delete)
}

fn missing_plan(node_id: &str) -> NodePlan {
    NodePlan {
        node_id: node_id.to_string(),
        outcome: "node_unknown".to_string(),
        generation: 0,
        action: None,
        other_version: false,
    }
}

fn hold(
    node: &LiveNode,
    outcome: &str,
    generation: i64,
    desired: Option<&DesiredSnap>,
    target_version: &str,
) -> NodePlan {
    NodePlan {
        node_id: node.node_id.clone(),
        outcome: outcome.to_string(),
        generation,
        action: None,
        other_version: had_other(desired, target_version),
    }
}

fn write_plan(
    node: &LiveNode,
    current: i64,
    desired: Option<&DesiredSnap>,
    target_version: &str,
    hub: &Hub,
    write: bool,
    action: Action,
) -> NodePlan {
    let outcome = if hub.is_connected(&node.node_id) {
        "pending"
    } else {
        "queued"
    };
    NodePlan {
        node_id: node.node_id.clone(),
        outcome: outcome.to_string(),
        generation: if write { current + 1 } else { current },
        action: if write { Some(action) } else { None },
        other_version: had_other(desired, target_version),
    }
}

fn had_other(desired: Option<&DesiredSnap>, target_version: &str) -> bool {
    desired.is_some_and(|row| row.version != target_version)
}

fn same_desired(desired: &DesiredSnap, version: &str, sha256: &str) -> bool {
    desired.version == version && desired.sha256.eq_ignore_ascii_case(sha256.trim())
}

fn previous_version<'a>(
    versions: &'a [VersionRecord],
    current: &Version,
) -> Option<&'a VersionRecord> {
    versions
        .iter()
        .filter(|row| {
            matches!(row.status.as_str(), "published" | "deprecated")
                && row.semver.as_ref().is_some_and(|semver| semver < current)
        })
        .max_by(|left, right| left.semver.cmp(&right.semver))
}

fn rollback_version(explicit: Option<&VersionRecord>, plans: &[NodePlan]) -> Option<String> {
    if let Some(explicit) = explicit {
        return Some(explicit.version.clone());
    }
    let mut found: Option<String> = None;
    for plan in plans {
        if let Some(Action::Upsert { version, .. }) = &plan.action {
            match &found {
                None => found = Some(version.clone()),
                Some(previous) if previous != version => return None,
                Some(_) => {}
            }
        }
    }
    found
}

async fn persist(
    tx: &mut Tx<'_>,
    skill_name: &str,
    op_type: &str,
    version: Option<&str>,
    capabilities_sha256: Option<&str>,
    force: bool,
    plans: &[NodePlan],
) -> Result<String, DesiredError> {
    lock_chain(tx).await?;
    let operation_id = Uuid::new_v4().to_string();
    for plan in plans {
        match &plan.action {
            Some(Action::Upsert { version, sha256 }) => {
                set_generation(tx, &plan.node_id, plan.generation).await?;
                upsert_desired(
                    tx,
                    &plan.node_id,
                    skill_name,
                    version,
                    sha256,
                    &operation_id,
                )
                .await?;
            }
            Some(Action::Delete) => {
                let removed = delete_desired(tx, &plan.node_id, skill_name).await?;
                if removed > 0 {
                    set_generation(tx, &plan.node_id, plan.generation).await?;
                }
            }
            None => {}
        }
    }
    let per_node = per_nodes(plans);
    let targets_json =
        serde_json::to_string(&per_node).map_err(|err| DesiredError::Internal(err.into()))?;
    sqlx::query(
        r#"
        INSERT INTO skill_operations (
            id, `type`, skill_name, version, targets_json, capabilities_sha256,
            force_flag, actor, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3))
        "#,
    )
    .bind(&operation_id)
    .bind(op_type)
    .bind(skill_name)
    .bind(version)
    .bind(&targets_json)
    .bind(capabilities_sha256)
    .bind(i8::from(force))
    .bind(ACTOR)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    let detail = serde_json::json!({
        "operation_id": &operation_id,
        "skill_name": skill_name,
        "version": version,
        "per_node": per_node,
    });
    let detail_json =
        serde_json::to_string(&detail).map_err(|err| DesiredError::Internal(err.into()))?;
    append_audit(
        tx,
        audit_action(op_type),
        skill_name,
        single_node_id(plans),
        &detail_json,
    )
    .await?;
    Ok(operation_id)
}

async fn lock_chain(tx: &mut Tx<'_>) -> Result<(), DesiredError> {
    // Serialize the audit hash chain on the single license row. The row is not modified.
    sqlx::query("SELECT id FROM license_state WHERE id = 1 FOR UPDATE")
        .fetch_optional(tx.deref_mut())
        .await
        .map_err(db_err)?;
    Ok(())
}

async fn set_generation(
    tx: &mut Tx<'_>,
    node_id: &str,
    generation: i64,
) -> Result<(), DesiredError> {
    sqlx::query(
        r#"
        INSERT INTO node_skill_sets (node_id, generation, updated_at)
        VALUES (?, ?, CURRENT_TIMESTAMP(3))
        ON DUPLICATE KEY UPDATE
          generation = VALUES(generation),
          updated_at = CURRENT_TIMESTAMP(3)
        "#,
    )
    .bind(node_id)
    .bind(generation)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    Ok(())
}

async fn upsert_desired(
    tx: &mut Tx<'_>,
    node_id: &str,
    skill_name: &str,
    version: &str,
    sha256: &str,
    operation_id: &str,
) -> Result<(), DesiredError> {
    sqlx::query(
        r#"
        INSERT INTO node_skills_desired (
            node_id, skill_name, version, sha256, operation_id, requested_by, requested_at
        ) VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3))
        ON DUPLICATE KEY UPDATE
          version = VALUES(version),
          sha256 = VALUES(sha256),
          operation_id = VALUES(operation_id),
          requested_by = VALUES(requested_by),
          requested_at = CURRENT_TIMESTAMP(3)
        "#,
    )
    .bind(node_id)
    .bind(skill_name)
    .bind(version)
    .bind(sha256)
    .bind(operation_id)
    .bind(ACTOR)
    .execute(tx.deref_mut())
    .await
    .map_err(db_err)?;
    Ok(())
}

async fn delete_desired(
    tx: &mut Tx<'_>,
    node_id: &str,
    skill_name: &str,
) -> Result<u64, DesiredError> {
    let result =
        sqlx::query("DELETE FROM node_skills_desired WHERE node_id = ? AND skill_name = ?")
            .bind(node_id)
            .bind(skill_name)
            .execute(tx.deref_mut())
            .await
            .map_err(db_err)?;
    Ok(result.rows_affected())
}

async fn append_audit(
    tx: &mut Tx<'_>,
    action: &str,
    target_id: &str,
    node_id: Option<String>,
    detail_json: &str,
) -> Result<(), DesiredError> {
    let prev = sqlx::query("SELECT `hash` FROM audit_events ORDER BY id DESC LIMIT 1")
        .fetch_optional(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let prev_hash = match prev {
        Some(row) => trim_text(row.try_get::<String, _>("hash").map_err(db_err)?),
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
            CURRENT_TIMESTAMP(3), ?, ?, ?, 'skill', ?, ?, ?, ?, ?
        )
        "#,
    )
    .bind(ACTOR)
    .bind(ACTOR)
    .bind(action)
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
    let material = format!("{prev}\n{action}\n{target_id}\n{detail}");
    sha256_hex(material.as_bytes())
}

fn audit_action(op_type: &str) -> &'static str {
    match op_type {
        "upgrade" => "skill.upgrade",
        "rollback" => "skill.rollback",
        "uninstall" => "skill.uninstall",
        _ => "skill.install",
    }
}

async fn load_desired_map(
    tx: &mut Tx<'_>,
    skill_name: &str,
) -> Result<HashMap<String, DesiredSnap>, DesiredError> {
    let rows = sqlx::query(
        "SELECT node_id, version, sha256 FROM node_skills_desired WHERE skill_name = ?",
    )
    .bind(skill_name)
    .fetch_all(tx.deref_mut())
    .await
    .map_err(db_err)?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        map.insert(
            node_id,
            DesiredSnap {
                version: row.try_get("version").map_err(db_err)?,
                sha256: trim_text(row.try_get("sha256").map_err(db_err)?),
            },
        );
    }
    Ok(map)
}

async fn load_generation_map(tx: &mut Tx<'_>) -> Result<HashMap<String, i64>, DesiredError> {
    let rows = sqlx::query("SELECT node_id, generation FROM node_skill_sets")
        .fetch_all(tx.deref_mut())
        .await
        .map_err(db_err)?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        let node_id: String = row.try_get("node_id").map_err(db_err)?;
        let generation: i64 = row.try_get("generation").map_err(db_err)?;
        map.insert(node_id, generation);
    }
    Ok(map)
}

async fn target_views(tx: &mut Tx<'_>, slots: &[Slot]) -> Result<Vec<TargetView>, DesiredError> {
    let gens = load_generation_map(tx).await?;
    Ok(slots
        .iter()
        .map(|slot| match slot {
            Slot::Missing(id) => TargetView {
                node_id: id.clone(),
                exists: false,
                generation: 0,
            },
            Slot::Node(node) => TargetView {
                node_id: node.node_id.clone(),
                exists: true,
                generation: gens.get(&node.node_id).copied().unwrap_or(0),
            },
        })
        .collect())
}

async fn skill_in_use(
    tx: &mut Tx<'_>,
    slots: &[Slot],
    skill_name: &str,
) -> Result<(Vec<String>, Vec<String>), DesiredError> {
    let mut spec_ids = Vec::new();
    let mut schedules = Vec::new();
    for slot in slots {
        let Slot::Node(node) = slot else {
            continue;
        };
        let row =
            sqlx::query("SELECT specs_json, schedules_json FROM node_configs WHERE node_id = ?")
                .bind(&node.node_id)
                .fetch_optional(tx.deref_mut())
                .await
                .map_err(db_err)?;
        let Some(row) = row else {
            continue;
        };
        let specs_json: String = row.try_get("specs_json").map_err(db_err)?;
        let schedules_json: String = row.try_get("schedules_json").map_err(db_err)?;
        collect_spec_ids(&specs_json, skill_name, &mut spec_ids);
        schedules.push(schedules_json);
    }
    let mut schedule_ids = Vec::new();
    for schedules_json in &schedules {
        collect_schedule_ids(schedules_json, &spec_ids, &mut schedule_ids);
    }
    Ok((spec_ids, schedule_ids))
}

fn collect_spec_ids(specs_json: &str, skill_name: &str, spec_ids: &mut Vec<String>) {
    let Ok(value) = serde_json::from_str::<Value>(specs_json) else {
        return;
    };
    let Some(specs) = value.as_array() else {
        return;
    };
    for spec in specs {
        if spec.get("kind").and_then(Value::as_str) != Some("skill") {
            continue;
        }
        if spec
            .get("params")
            .and_then(|params| params.get("skill"))
            .and_then(Value::as_str)
            != Some(skill_name)
        {
            continue;
        }
        if let Some(id) = spec.get("id").and_then(Value::as_str) {
            push_unique(spec_ids, id.to_string());
        }
    }
}

fn collect_schedule_ids(schedules_json: &str, spec_ids: &[String], schedule_ids: &mut Vec<String>) {
    let Ok(value) = serde_json::from_str::<Value>(schedules_json) else {
        return;
    };
    let Some(schedules) = value.as_array() else {
        return;
    };
    for schedule in schedules {
        let Some(spec_id) = schedule.get("spec_id").and_then(Value::as_str) else {
            continue;
        };
        if !spec_ids.iter().any(|id| id == spec_id) {
            continue;
        }
        if let Some(id) = schedule
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        {
            push_unique(schedule_ids, id.to_string());
        } else {
            push_unique(schedule_ids, spec_id.to_string());
        }
    }
}

fn push_unique(out: &mut Vec<String>, value: String) {
    if !value.is_empty() && !out.iter().any(|existing| existing == &value) {
        out.push(value);
    }
}

fn per_nodes(plans: &[NodePlan]) -> Vec<PerNode> {
    plans
        .iter()
        .map(|plan| PerNode {
            node_id: plan.node_id.clone(),
            outcome: plan.outcome.clone(),
            generation: plan.generation,
        })
        .collect()
}

fn missing_per_node(slots: &[Slot]) -> Vec<PerNode> {
    slots
        .iter()
        .map(|slot| PerNode {
            node_id: slot.id().to_string(),
            outcome: "node_unknown".to_string(),
            generation: 0,
        })
        .collect()
}

fn single_node_id(plans: &[NodePlan]) -> Option<String> {
    let mut found = None;
    for plan in plans {
        if plan.outcome == "node_unknown" {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(plan.node_id.clone());
    }
    found
}

fn operation_view(row: &sqlx::mysql::MySqlRow) -> Result<OperationView, DesiredError> {
    let targets_json: String = row.try_get("targets_json").map_err(db_err)?;
    let per_node = serde_json::from_str(&targets_json).unwrap_or(Value::Array(Vec::new()));
    let capabilities: Option<String> = row.try_get("capabilities_sha256").map_err(db_err)?;
    Ok(OperationView {
        id: row.try_get("id").map_err(db_err)?,
        op_type: row.try_get("op_type").map_err(db_err)?,
        skill_name: row.try_get("skill_name").map_err(db_err)?,
        version: row.try_get("version").map_err(db_err)?,
        per_node,
        capabilities_sha256: capabilities
            .map(trim_text)
            .filter(|value| !value.is_empty()),
        force: flag(row, "force_flag")?,
        actor: row.try_get("actor").map_err(db_err)?,
        created_at: row.try_get("created_at").map_err(db_err)?,
    })
}

fn audit_view(row: &sqlx::mysql::MySqlRow) -> Result<AuditView, DesiredError> {
    let detail_json: String = row.try_get("detail_json").map_err(db_err)?;
    let detail = serde_json::from_str(&detail_json).unwrap_or(Value::String(detail_json));
    let prev: Option<String> = row.try_get("prev_hash").map_err(db_err)?;
    let node_id: Option<String> = row.try_get("node_id").map_err(db_err)?;
    Ok(AuditView {
        id: row.try_get("id").map_err(db_err)?,
        at: row.try_get("at").map_err(db_err)?,
        actor_type: row.try_get("actor_type").map_err(db_err)?,
        actor_id: row.try_get("actor_id").map_err(db_err)?,
        action: row.try_get("action").map_err(db_err)?,
        target_type: row.try_get("target_type").map_err(db_err)?,
        target_id: row.try_get("target_id").map_err(db_err)?,
        node_id: node_id.filter(|value| !value.is_empty()),
        detail,
        prev_hash: prev.map(trim_text).filter(|value| !value.is_empty()),
        hash: trim_text(row.try_get("hash").map_err(db_err)?),
    })
}

fn flag(row: &sqlx::mysql::MySqlRow, column: &str) -> Result<bool, DesiredError> {
    if let Ok(value) = row.try_get::<bool, _>(column) {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<i8, _>(column) {
        return Ok(value != 0);
    }
    if let Ok(value) = row.try_get::<i64, _>(column) {
        return Ok(value != 0);
    }
    Err(DesiredError::Internal(anyhow::anyhow!(
        "column {column} is not a flag"
    )))
}

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

fn parse_since(raw: &str) -> Result<DateTime<Utc>, DesiredError> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| DesiredError::BadRequest("since must be RFC3339".into()))
}

pub(crate) fn node_too_old(node_version: &str, min_node_version: Option<&str>) -> bool {
    let Some(min_raw) = min_node_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let node_raw = node_version.trim();
    if node_raw.is_empty() {
        return false;
    }
    let (Ok(node), Ok(min)) = (Version::parse(node_raw), Version::parse(min_raw)) else {
        return false;
    };
    node < min
}

pub(crate) fn platform_unsupported(
    labels: &BTreeMap<String, String>,
    platforms: &[String],
) -> bool {
    if platforms.is_empty() {
        return false;
    }
    let Some(os) = labels
        .get("os")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let Some(arch) = labels
        .get("arch")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let slash = format!("{os}/{arch}");
    let dash = format!("{os}-{arch}");
    !platforms
        .iter()
        .any(|platform| platform == &slash || platform == &dash)
}

pub(crate) fn labels_match(
    node: &BTreeMap<String, String>,
    want: &BTreeMap<String, String>,
) -> bool {
    want.iter()
        .all(|(key, value)| node.get(key).is_some_and(|have| have == value))
}

fn parse_labels(raw: &str) -> BTreeMap<String, String> {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return BTreeMap::new();
    };
    let Some(object) = value.as_object() else {
        return BTreeMap::new();
    };
    object
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_string())))
        .collect()
}

fn parse_string_list(raw: Option<String>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    serde_json::from_str(trimmed).unwrap_or_default()
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn trim_text(value: String) -> String {
    value.trim().to_string()
}

fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ids {
        if seen.insert(id.clone()) {
            out.push(id);
        }
    }
    out
}

fn cap(
    name: &'static str,
    scope: Option<&'static str>,
    risk: &'static str,
    description: &'static str,
) -> CapabilityInfo {
    CapabilityInfo {
        name,
        scope,
        risk,
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        audit_hash, labels_match, node_too_old, platform_unsupported, version_matches,
        VersionRecord,
    };
    use semver::Version;
    use std::collections::BTreeMap;

    fn labels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn version_req_matches_star_exact_caret_and_tilde() {
        let v100 = Version::parse("1.0.0").unwrap();
        let v110 = Version::parse("1.1.0").unwrap();
        let v123 = Version::parse("1.2.3").unwrap();
        let v124 = Version::parse("1.2.4").unwrap();
        let v200 = Version::parse("2.0.0").unwrap();
        assert!(version_matches("*", &v200));
        assert!(version_matches("1.0.0", &v100));
        assert!(!version_matches("1.0.0", &v110));
        assert!(version_matches("=1.0.0", &v100));
        assert!(!version_matches("=1.0.0", &v110));
        assert!(version_matches("^1.0", &v110));
        assert!(!version_matches("^1.0", &v200));
        assert!(version_matches("~1.2.3", &v123));
        assert!(version_matches("~1.2.3", &v124));
        assert!(!version_matches("~1.2.3", &v200));

        let versions = [
            record("1.0.0", "published"),
            record("1.2.0", "published"),
            record("2.0.0", "published"),
            record("1.9.0", "yanked"),
        ];
        let best = super::highest_published("^1.0", &versions).unwrap();
        assert_eq!(best.version, "1.2.0");
        assert!(super::highest_published("=9.9.9", &versions).is_none());
    }

    fn record(version: &str, status: &str) -> VersionRecord {
        VersionRecord {
            version: version.to_string(),
            sha256: String::new(),
            status: status.to_string(),
            min_node_version: None,
            platforms: Vec::new(),
            capabilities_sha256: String::new(),
            semver: Version::parse(version).ok(),
        }
    }

    #[test]
    fn platform_and_min_version_rules() {
        let linux = labels(&[("os", "linux"), ("arch", "amd64"), ("role", "db")]);
        assert!(!platform_unsupported(&linux, &[]));
        assert!(!platform_unsupported(&linux, &["linux/amd64".to_string()]));
        assert!(!platform_unsupported(&linux, &["linux-amd64".to_string()]));
        assert!(platform_unsupported(&linux, &["linux/arm64".to_string()]));
        assert!(!platform_unsupported(
            &labels(&[("os", "linux")]),
            &["linux/amd64".to_string()]
        ));
        assert!(!platform_unsupported(
            &labels(&[("os", ""), ("arch", "amd64")]),
            &["linux/amd64".to_string()]
        ));

        assert!(node_too_old("1.1.0", Some("1.2.0")));
        assert!(!node_too_old("1.2.0", Some("1.2.0")));
        assert!(!node_too_old("1.3.0", Some("1.2.0")));
        assert!(!node_too_old("", Some("1.2.0")));
        assert!(!node_too_old("1.0.0", Some("")));
        assert!(!node_too_old("1.0.0", None));
        assert!(!node_too_old("latest", Some("1.2.0")));
        assert!(!node_too_old("1.0.0", Some("latest")));
    }

    #[test]
    fn selector_labels_require_every_pair() {
        let node = labels(&[("role", "db"), ("env", "prod")]);
        assert!(labels_match(&node, &BTreeMap::new()));
        assert!(labels_match(&node, &labels(&[("role", "db")])));
        assert!(!labels_match(&node, &labels(&[("role", "app")])));
        assert!(labels_match(
            &node,
            &labels(&[("role", "db"), ("env", "prod")])
        ));
        assert!(!labels_match(
            &node,
            &labels(&[("role", "db"), ("env", "dev")])
        ));
    }

    #[test]
    fn audit_hash_uses_prev_action_target_and_detail() {
        let detail = "{\"operation_id\":\"op\"}";
        let hash = audit_hash("", "skill.install", "demo", detail);
        let expect =
            crate::artifact::sha256_hex(format!("\nskill.install\ndemo\n{detail}").as_bytes());
        assert_eq!(hash, expect);
        assert_eq!(hash.len(), 64);
        let next = audit_hash(&hash, "skill.upgrade", "demo", detail);
        let expect_next = crate::artifact::sha256_hex(
            format!("{hash}\nskill.upgrade\ndemo\n{detail}").as_bytes(),
        );
        assert_eq!(next, expect_next);
    }
}

#[cfg(test)]
mod http_tests {
    use crate::artifact::sha256_hex;
    use crate::db::connect_test_db;
    use crate::http::{router, AppState};
    use crate::hub::Hub;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use sqlx::Row;
    use std::collections::HashMap;
    use std::io::Write;
    use tower::ServiceExt;

    const PARAMS_JSON: &[u8] = br#"{"type":"object"}"#;
    const WASM: &[u8] = b"\0asm\x01\x00\x00\x00";

    struct Published {
        name: String,
        version: String,
        sha256: String,
        capabilities_sha256: String,
    }

    fn app(db: &crate::db::Db) -> AppState {
        AppState {
            db: db.clone(),
            hub: Hub::new(),
            api_token: None,
            license_env: None,
        }
    }

    async fn call(state: AppState, request: Request<Body>) -> (StatusCode, axum::body::Bytes) {
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("response body");
        (status, bytes)
    }

    fn expect_json(status: StatusCode, bytes: &[u8], expected: StatusCode) -> Value {
        assert_eq!(status, expected, "{}", String::from_utf8_lossy(bytes));
        if bytes.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(bytes)
            .unwrap_or_else(|err| panic!("json ({err}): {}", String::from_utf8_lossy(bytes)))
    }

    fn json_req(method: &str, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).expect("json")))
            .expect("request")
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }

    fn post_package(bytes: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/skills")
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/vnd.novbot.skill",
            )
            .body(Body::from(bytes))
            .expect("request")
    }

    fn gzip_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).expect("gzip");
        encoder.finish().expect("gzip finish")
    }

    fn append_file(builder: &mut tar::Builder<&mut Vec<u8>>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(data.len() as u64);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, path, data)
            .expect("tar append");
    }

    fn build_nbskill(name: &str, version: &str) -> Vec<u8> {
        let params_hash = sha256_hex(PARAMS_JSON);
        let wasm_hash = sha256_hex(WASM);
        let manifest = format!(
            "schema_version = 1\nname = \"{name}\"\nversion = \"{version}\"\ndisplay_name = \"{name}\"\ndescription = \"test skill\"\n\n[runtime]\nkind = \"wasm\"\nabi = \"novbot:skill@1\"\n\n[files]\n\"module.wasm\" = \"{wasm_hash}\"\n\"schema/params.json\" = \"{params_hash}\"\n"
        );
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            append_file(&mut builder, "skill.toml", manifest.as_bytes());
            append_file(&mut builder, "module.wasm", WASM);
            append_file(&mut builder, "schema/params.json", PARAMS_JSON);
            builder.finish().expect("tar finish");
        }
        gzip_bytes(&tar_bytes)
    }

    fn unique(prefix: &str) -> String {
        format!("{prefix}{}", uuid::Uuid::new_v4())
    }

    async fn publish(db: &crate::db::Db, name: &str, version: &str) -> Published {
        let (status, bytes) = call(app(db), post_package(build_nbskill(name, version))).await;
        let body = expect_json(status, &bytes, StatusCode::CREATED);
        Published {
            name: name.to_string(),
            version: version.to_string(),
            sha256: body["sha256"].as_str().expect("sha256").to_string(),
            capabilities_sha256: body["capabilities_sha256"]
                .as_str()
                .expect("capabilities_sha256")
                .to_string(),
        }
    }

    async fn add_node(db: &crate::db::Db, labels: &[(&str, &str)]) -> String {
        let node_id = unique("n");
        let map = labels
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<HashMap<_, _>>();
        db.upsert_node(&node_id, "host", "", &map)
            .await
            .expect("upsert node");
        node_id
    }

    async fn count_where(db: &crate::db::Db, sql: &str, binds: &[&str]) -> i64 {
        let mut query = sqlx::query(sql);
        for bind in binds {
            query = query.bind(*bind);
        }
        let row = query.fetch_one(db.pool()).await.expect("count");
        row.try_get("n").expect("n")
    }

    async fn desired_row(
        db: &crate::db::Db,
        node_id: &str,
        skill: &str,
    ) -> Option<(String, String, String)> {
        let row = sqlx::query(
            "SELECT version, sha256, operation_id FROM node_skills_desired WHERE node_id = ? AND skill_name = ?",
        )
        .bind(node_id)
        .bind(skill)
        .fetch_optional(db.pool())
        .await
        .expect("desired");
        row.map(|row| {
            let version: String = row.try_get("version").expect("version");
            let sha: String = row.try_get("sha256").expect("sha");
            let operation_id: Option<String> = row.try_get("operation_id").expect("operation");
            (
                version,
                sha.trim().to_string(),
                operation_id.unwrap_or_default(),
            )
        })
    }

    async fn generation_of(db: &crate::db::Db, node_id: &str) -> i64 {
        let row = sqlx::query("SELECT generation FROM node_skill_sets WHERE node_id = ?")
            .bind(node_id)
            .fetch_optional(db.pool())
            .await
            .expect("generation");
        match row {
            Some(row) => row.try_get("generation").expect("generation"),
            None => 0,
        }
    }

    fn node_outcome<'a>(body: &'a Value, node_id: &str) -> &'a Value {
        body["per_node"]
            .as_array()
            .expect("per_node")
            .iter()
            .find(|item| item["node_id"] == node_id)
            .unwrap_or_else(|| panic!("missing {node_id} in {body}"))
    }

    fn install_body(skill: &Published, node_ids: Vec<String>, selector: Option<Value>) -> Value {
        json!({
            "version": skill.version,
            "node_ids": node_ids,
            "selector": selector,
            "accepted_capabilities_sha256": skill.capabilities_sha256,
            "dry_run": false,
        })
    }

    async fn post_install(
        state: AppState,
        skill: &Published,
        node_ids: Vec<String>,
        selector: Option<Value>,
    ) -> (StatusCode, axum::body::Bytes) {
        let uri = format!("/v1/skills/{}/install", skill.name);
        call(
            state,
            json_req("POST", &uri, install_body(skill, node_ids, selector)),
        )
        .await
    }

    #[tokio::test]
    async fn install_offline_node_is_queued_and_bumps_generation() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let (status, bytes) = post_install(app(&db), &skill, vec![node_id.clone()], None).await;
        let body = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(body["dry_run"], false);
        let outcome = node_outcome(&body, &node_id);
        assert_eq!(outcome["outcome"], "queued");
        assert_eq!(outcome["generation"], 1);
        let operation_id = body["operation_id"].as_str().expect("operation id");
        let (version, sha, stored_op) = desired_row(&db, &node_id, &skill.name)
            .await
            .expect("desired row");
        assert_eq!(version, "1.0.0");
        assert_eq!(sha, skill.sha256);
        assert_eq!(stored_op, operation_id);
        assert_eq!(generation_of(&db, &node_id).await, 1);
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) AS n FROM skill_operations WHERE skill_name = ?",
                &[&skill.name],
            )
            .await,
            1
        );
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) AS n FROM audit_events WHERE target_id = ? AND action = 'skill.install'",
                &[&skill.name],
            )
            .await,
            1
        );
        let op_type: String =
            sqlx::query("SELECT `type` AS op_type FROM skill_operations WHERE id = ?")
                .bind(operation_id)
                .fetch_one(db.pool())
                .await
                .expect("operation")
                .try_get("op_type")
                .expect("type");
        assert_eq!(op_type, "install");

        let (status, bytes) = post_install(app(&db), &skill, vec![node_id.clone()], None).await;
        let again = expect_json(status, &bytes, StatusCode::ACCEPTED);
        let outcome = node_outcome(&again, &node_id);
        assert_eq!(outcome["outcome"], "already_installed");
        assert_eq!(outcome["generation"], 1);
        assert_eq!(generation_of(&db, &node_id).await, 1);
        let (_, _, stored_op) = desired_row(&db, &node_id, &skill.name)
            .await
            .expect("desired row");
        assert_eq!(stored_op, operation_id);
    }

    #[tokio::test]
    async fn install_connected_node_is_pending() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let hub = Hub::new();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        hub.register(node_id.clone(), tx).await;
        let state = AppState {
            db: db.clone(),
            hub,
            api_token: None,
            license_env: None,
        };
        let (status, bytes) = post_install(state, &skill, vec![node_id.clone()], None).await;
        let body = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(node_outcome(&body, &node_id)["outcome"], "pending");
        drop(rx);
    }

    #[tokio::test]
    async fn install_capabilities_hash_mismatch_is_409() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let mut body = install_body(&skill, vec![node_id.clone()], None);
        body["accepted_capabilities_sha256"] = json!("0".repeat(64));
        let (status, bytes) = call(
            app(&db),
            json_req("POST", &format!("/v1/skills/{name}/install"), body),
        )
        .await;
        let payload = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(payload["code"], "capabilities_changed");
        assert!(desired_row(&db, &node_id, &skill.name).await.is_none());
    }

    #[tokio::test]
    async fn install_selector_matches_labels_only() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let tid = unique("tid");
        let db_node = add_node(&db, &[("role", "db"), ("tid", &tid)]).await;
        let app_node = add_node(&db, &[("role", "app"), ("tid", &tid)]).await;
        let selector = json!({"labels": {"role": "db", "tid": tid}});
        let (status, bytes) = post_install(app(&db), &skill, Vec::new(), Some(selector)).await;
        let body = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(node_outcome(&body, &db_node)["outcome"], "queued");
        assert_eq!(node_outcome(&body, &db_node)["generation"], 1);
        assert!(body["per_node"]
            .as_array()
            .expect("per_node")
            .iter()
            .all(|item| item["node_id"] != app_node));
        assert!(desired_row(&db, &db_node, &skill.name).await.is_some());
        assert!(desired_row(&db, &app_node, &skill.name).await.is_none());
    }

    #[tokio::test]
    async fn install_deprecated_is_version_not_installable() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        sqlx::query(
            r#"UPDATE skill_versions v
               INNER JOIN skills s ON s.id = v.skill_id
               SET v.status = 'deprecated'
               WHERE s.name = ? AND v.version = ?"#,
        )
        .bind(&skill.name)
        .bind(&skill.version)
        .execute(db.pool())
        .await
        .expect("deprecate");
        let (status, bytes) = post_install(app(&db), &skill, vec![node_id.clone()], None).await;
        let body = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(body["code"], "version_not_installable");
        assert!(desired_row(&db, &node_id, &skill.name).await.is_none());
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) AS n FROM skill_operations WHERE skill_name = ?",
                &[&skill.name],
            )
            .await,
            0
        );
    }

    #[tokio::test]
    async fn rollback_to_previous_version() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let older = publish(&db, &name, "1.0.0").await;
        let newer = publish(&db, &name, "1.1.0").await;
        let node_id = add_node(&db, &[]).await;
        let (status, bytes) = post_install(app(&db), &newer, vec![node_id.clone()], None).await;
        expect_json(status, &bytes, StatusCode::ACCEPTED);
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/skills/{name}/rollback"),
                json!({"node_ids": [node_id]}),
            ),
        )
        .await;
        let body = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(node_outcome(&body, &node_id)["outcome"], "queued");
        let (version, sha, _) = desired_row(&db, &node_id, &name).await.expect("desired");
        assert_eq!(version, "1.0.0");
        assert_eq!(sha, older.sha256);
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) AS n FROM skill_operations WHERE skill_name = ? AND `type` = 'rollback'",
                &[&name],
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn uninstall_without_force_is_skill_in_use() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let (status, bytes) = post_install(app(&db), &skill, vec![node_id.clone()], None).await;
        expect_json(status, &bytes, StatusCode::ACCEPTED);
        let spec_id = unique("spec");
        let schedule_id = unique("sched");
        let (status, bytes) = call(
            app(&db),
            json_req(
                "PUT",
                &format!("/v1/nodes/{node_id}/config"),
                json!({
                    "specs": [{
                        "id": spec_id,
                        "kind": "skill",
                        "params": {"skill": skill.name}
                    }],
                    "schedules": [{"id": schedule_id, "spec_id": spec_id}]
                }),
            ),
        )
        .await;
        expect_json(status, &bytes, StatusCode::OK);
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/skills/{name}/uninstall"),
                json!({"node_ids": [node_id], "force": false}),
            ),
        )
        .await;
        let blocked = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(blocked["code"], "skill_in_use");
        assert!(blocked["spec_ids"]
            .as_array()
            .expect("spec_ids")
            .iter()
            .any(|id| id == &spec_id));
        assert!(blocked["schedule_ids"]
            .as_array()
            .expect("schedule_ids")
            .iter()
            .any(|id| id == &schedule_id));
        assert!(desired_row(&db, &node_id, &name).await.is_some());
        assert_eq!(generation_of(&db, &node_id).await, 1);

        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/skills/{name}/uninstall"),
                json!({"node_ids": [node_id], "force": true}),
            ),
        )
        .await;
        let forced = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(node_outcome(&forced, &node_id)["outcome"], "queued");
        assert_eq!(node_outcome(&forced, &node_id)["generation"], 2);
        assert!(desired_row(&db, &node_id, &name).await.is_none());
        assert_eq!(generation_of(&db, &node_id).await, 2);
    }

    #[tokio::test]
    async fn uninstall_builtin_is_read_only() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                "/v1/skills/host_info/uninstall",
                json!({"force": true}),
            ),
        )
        .await;
        let body = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(body["code"], "builtin_read_only");
    }

    #[tokio::test]
    async fn bundle_crud_and_seed_is_read_only() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let (status, bytes) = call(app(&db), get_req("/v1/skill-bundles")).await;
        let listed = expect_json(status, &bytes, StatusCode::OK);
        let items = listed["items"].as_array().expect("items");
        let host = items
            .iter()
            .find(|item| item["id"] == "host-basics")
            .expect("host-basics");
        assert_eq!(host["builtin"], true);
        assert!(items.iter().any(|item| item["id"] == "env-sample"));
        let (status, bytes) = call(app(&db), delete_req("/v1/skill-bundles/host-basics")).await;
        let blocked = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(blocked["code"], "builtin_read_only");

        let id = unique("b");
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                "/v1/skill-bundles",
                json!({
                    "id": id,
                    "name": "Temp",
                    "description": "temp bundle",
                    "items": [{"skill_name": "echo", "version_req": "*"}]
                }),
            ),
        )
        .await;
        let created = expect_json(status, &bytes, StatusCode::CREATED);
        assert_eq!(created["id"], id);
        let (status, bytes) = call(
            app(&db),
            json_req(
                "PUT",
                &format!("/v1/skill-bundles/{id}"),
                json!({
                    "name": "Temp renamed",
                    "description": "changed",
                    "items": [{"skill_name": "host_info", "version_req": "*"}]
                }),
            ),
        )
        .await;
        let replaced = expect_json(status, &bytes, StatusCode::OK);
        assert_eq!(replaced["name"], "Temp renamed");
        assert_eq!(replaced["items"][0]["skill_name"], "host_info");
        let (status, bytes) = call(app(&db), delete_req(&format!("/v1/skill-bundles/{id}"))).await;
        assert_eq!(
            status,
            StatusCode::NO_CONTENT,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let (status, bytes) = call(app(&db), get_req(&format!("/v1/skill-bundles/{id}"))).await;
        let missing = expect_json(status, &bytes, StatusCode::NOT_FOUND);
        assert_eq!(missing["code"], "bundle_unknown");
    }

    #[tokio::test]
    async fn bundle_install_checks_each_capability_hash() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let bundle_id = unique("b");
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                "/v1/skill-bundles",
                json!({
                    "id": bundle_id,
                    "name": "One skill",
                    "description": "hub skill",
                    "items": [{"skill_name": skill.name, "version_req": "*"}]
                }),
            ),
        )
        .await;
        expect_json(status, &bytes, StatusCode::CREATED);
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/skill-bundles/{bundle_id}/install"),
                json!({
                    "node_ids": [node_id],
                    "accepted_capabilities": {skill.name.clone(): "0".repeat(64)}
                }),
            ),
        )
        .await;
        let wrong = expect_json(status, &bytes, StatusCode::CONFLICT);
        assert_eq!(wrong["code"], "capabilities_changed");
        assert!(desired_row(&db, &node_id, &skill.name).await.is_none());

        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/skill-bundles/{bundle_id}/install"),
                json!({
                    "node_ids": [node_id],
                    "accepted_capabilities": {skill.name.clone(): skill.capabilities_sha256}
                }),
            ),
        )
        .await;
        let ok = expect_json(status, &bytes, StatusCode::ACCEPTED);
        let op = &ok["operations"][0];
        assert_eq!(op["skill_name"], skill.name);
        assert!(op["operation_id"].is_string());
        assert_eq!(node_outcome(op, &node_id)["outcome"], "queued");
        let (_, sha, _) = desired_row(&db, &node_id, &skill.name)
            .await
            .expect("desired");
        assert_eq!(sha, skill.sha256);
    }

    #[tokio::test]
    async fn dispatch_unknown_spec_is_spec_unknown() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let node_id = unique("n");
        let (status, bytes) = call(
            app(&db),
            json_req(
                "PUT",
                &format!("/v1/nodes/{node_id}/config"),
                json!({"specs": [{"id": "cpu", "kind": "probe", "params": {}}]}),
            ),
        )
        .await;
        expect_json(status, &bytes, StatusCode::OK);
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/nodes/{node_id}/dispatch"),
                json!({"spec_id": "no-such"}),
            ),
        )
        .await;
        let missing = expect_json(status, &bytes, StatusCode::NOT_FOUND);
        assert_eq!(missing["code"], "spec_unknown");
        assert_eq!(
            count_where(
                &db,
                "SELECT COUNT(*) AS n FROM pending_dispatches WHERE node_id = ? AND spec_id = 'no-such'",
                &[&node_id],
            )
            .await,
            0
        );
        let (status, bytes) = call(
            app(&db),
            json_req(
                "POST",
                &format!("/v1/nodes/{node_id}/dispatch"),
                json!({"spec_id": "cpu"}),
            ),
        )
        .await;
        let accepted = expect_json(status, &bytes, StatusCode::ACCEPTED);
        assert_eq!(accepted["accepted"], true);
        assert_eq!(accepted["spec_id"], "cpu");
    }

    #[tokio::test]
    async fn license_stubs_are_402() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let state = AppState {
            db: db.clone(),
            hub: Hub::new(),
            api_token: None,
            license_env: Some("licensed".to_string()),
        };
        let (status, bytes) = call(
            state.clone(),
            json_req("POST", "/v1/skill-imports", json!({})),
        )
        .await;
        let imports = expect_json(status, &bytes, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(imports["code"], "license_required");
        assert_eq!(imports["license_required"], true);
        let (status, bytes) = call(state, get_req("/v1/ee/audit/export")).await;
        let export = expect_json(status, &bytes, StatusCode::PAYMENT_REQUIRED);
        assert_eq!(export["code"], "license_required");
        assert_eq!(export["license_required"], true);
    }

    #[tokio::test]
    async fn skills_routes_require_bearer() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let state = AppState {
            db: db.clone(),
            hub: Hub::new(),
            api_token: Some("secret".to_string()),
            license_env: None,
        };
        let (status, _) = call(state.clone(), get_req("/v1/skills")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(state, get_req("/v1/skill-bundles")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn fleet_skill_groups_still_list_seeds() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let (status, bytes) = call(app(&db), get_req("/v1/fleet/skill-groups")).await;
        let body = expect_json(status, &bytes, StatusCode::OK);
        assert!(body["note"].as_str().unwrap_or("").contains("skill-groups"));
        let groups = body["groups"].as_array().expect("groups");
        let host = groups
            .iter()
            .find(|group| group["id"] == "host-basics")
            .expect("host-basics");
        let skills = host["skills"].as_array().expect("skills");
        assert!(skills.iter().any(|skill| skill == "host_info"));
        assert!(skills.iter().any(|skill| skill == "echo"));
        assert!(groups.iter().any(|group| group["id"] == "env-sample"));
    }

    #[tokio::test]
    async fn get_node_skills_shows_queued_desired() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let name = unique("t");
        let skill = publish(&db, &name, "1.0.0").await;
        let node_id = add_node(&db, &[]).await;
        let (status, bytes) = post_install(app(&db), &skill, vec![node_id.clone()], None).await;
        expect_json(status, &bytes, StatusCode::ACCEPTED);
        let (status, bytes) = call(app(&db), get_req(&format!("/v1/nodes/{node_id}/skills"))).await;
        let body = expect_json(status, &bytes, StatusCode::OK);
        assert_eq!(body["node_id"], node_id);
        assert_eq!(body["generation"], 1);
        assert_eq!(body["applied_generation"], 0);
        let items = body["items"].as_array().expect("items");
        let host = items
            .iter()
            .find(|item| item["name"] == "host_info")
            .expect("host_info");
        assert_eq!(host["source"], "builtin");
        assert_eq!(host["state"], "installed");
        let desired = items
            .iter()
            .find(|item| item["name"] == skill.name)
            .expect("desired skill");
        assert_eq!(desired["state"], "queued");
        assert_eq!(desired["source"], "hub");
        assert_eq!(desired["desired_version"], "1.0.0");
    }

    fn delete_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .expect("request")
    }
}
