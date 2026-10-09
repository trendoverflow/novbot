// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Skill bundles stored in MySQL. Seeded bundles are read-only.
//! Fleet skill-group routes read the same rows.

use crate::artifact::mysql_is_duplicate;
use crate::db::Db;
use crate::hub::Hub;
use crate::skill_catalog::is_builtin_name;
use crate::skill_desired::{
    accepted_hash, apply_installs, highest_published, published_versions, DesiredError,
    InstallPlan, PerNode, Selection, SelectorBody, TargetView,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::ops::DerefMut;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BundleItemView {
    pub skill_name: String,
    pub version_req: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BundleView {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub builtin: bool,
    pub items: Vec<BundleItemView>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BundleList {
    pub items: Vec<BundleView>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BundleItemIn {
    pub skill_name: String,
    pub version_req: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BundleCreate {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub items: Vec<BundleItemIn>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BundleReplace {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub items: Vec<BundleItemIn>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BundleInstallRequest {
    #[serde(default)]
    pub node_ids: Vec<String>,
    #[serde(default)]
    pub selector: Option<SelectorBody>,
    #[serde(default)]
    pub accepted_capabilities: HashMap<String, String>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct BundleOperation {
    pub skill_name: String,
    pub operation_id: Option<String>,
    pub per_node: Vec<PerNode>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BundleInstallResponse {
    pub operations: Vec<BundleOperation>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct FleetGroup {
    pub id: String,
    pub name: String,
    pub description: String,
    pub skills: Vec<String>,
}

pub(crate) async fn list_bundles(db: &Db) -> Result<BundleList, DesiredError> {
    Ok(BundleList {
        items: load_bundles(db, None).await?,
    })
}

pub(crate) async fn get_bundle(db: &Db, id: &str) -> Result<BundleView, DesiredError> {
    load_bundles(db, Some(id))
        .await?
        .into_iter()
        .next()
        .ok_or(DesiredError::BundleUnknown)
}

pub(crate) async fn create_bundle(db: &Db, body: BundleCreate) -> Result<BundleView, DesiredError> {
    let id = body.id.trim();
    if !valid_bundle_id(id) {
        return Err(DesiredError::InvalidBundle("bundle id is invalid".into()));
    }
    let name = validate_name(&body.name)?;
    let description = normalize_description(body.description);
    let items = validate_items(&body.items)?;
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    let inserted = sqlx::query(
        "INSERT INTO skill_bundles (id, name, description, builtin) VALUES (?, ?, ?, 0)",
    )
    .bind(id)
    .bind(&name)
    .bind(&description)
    .execute(tx.deref_mut())
    .await;
    match inserted {
        Err(err) if mysql_is_duplicate(&err) => return Err(DesiredError::BundleExists),
        Err(err) => return Err(db_err(err)),
        Ok(_) => {}
    }
    insert_items(&mut tx, id, &items).await?;
    tx.commit().await.map_err(db_err)?;
    get_bundle(db, id).await
}

pub(crate) async fn replace_bundle(
    db: &Db,
    id: &str,
    body: BundleReplace,
) -> Result<BundleView, DesiredError> {
    let name = validate_name(&body.name)?;
    let description = normalize_description(body.description);
    let items = validate_items(&body.items)?;
    let existing = get_bundle(db, id).await?;
    if existing.builtin {
        return Err(DesiredError::BuiltinReadOnly);
    }
    let mut tx = db.pool().begin().await.map_err(db_err)?;
    sqlx::query("UPDATE skill_bundles SET name = ?, description = ? WHERE id = ? AND builtin = 0")
        .bind(&name)
        .bind(&description)
        .bind(id)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    sqlx::query("DELETE FROM skill_bundle_items WHERE bundle_id = ?")
        .bind(id)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    insert_items(&mut tx, id, &items).await?;
    tx.commit().await.map_err(db_err)?;
    get_bundle(db, id).await
}

pub(crate) async fn delete_bundle(db: &Db, id: &str) -> Result<(), DesiredError> {
    let existing = get_bundle(db, id).await?;
    if existing.builtin {
        return Err(DesiredError::BuiltinReadOnly);
    }
    let result = sqlx::query("DELETE FROM skill_bundles WHERE id = ? AND builtin = 0")
        .bind(id)
        .execute(db.pool())
        .await
        .map_err(db_err)?;
    if result.rows_affected() == 0 {
        return Err(DesiredError::BundleUnknown);
    }
    Ok(())
}

pub(crate) async fn install_bundle(
    db: &Db,
    hub: &Hub,
    id: &str,
    body: BundleInstallRequest,
) -> Result<BundleInstallResponse, DesiredError> {
    let bundle = get_bundle(db, id).await?;
    let mut plans = Vec::new();
    for item in &bundle.items {
        if is_builtin_name(&item.skill_name) {
            continue;
        }
        let versions = published_versions(db, &item.skill_name).await?;
        let Some(best) = highest_published(&item.version_req, &versions) else {
            return Err(DesiredError::VersionUnknown);
        };
        let capabilities_sha256 = accepted_hash(
            &best.capabilities_sha256,
            body.accepted_capabilities
                .get(&item.skill_name)
                .map(String::as_str),
        )?;
        plans.push(InstallPlan {
            skill_name: item.skill_name.clone(),
            version: best.version.clone(),
            sha256: best.sha256.clone(),
            capabilities_sha256,
            min_node_version: best.min_node_version.clone(),
            platforms: best.platforms.clone(),
        });
    }
    let selection = Selection::new(body.node_ids, body.selector);
    let batch = apply_installs(db, hub, plans, selection, body.dry_run).await?;
    let mut by_name: HashMap<String, (Option<String>, Vec<PerNode>)> = batch
        .changes
        .into_iter()
        .map(|change| (change.skill_name, (change.operation_id, change.per_node)))
        .collect();
    let mut operations = Vec::with_capacity(bundle.items.len());
    for item in bundle.items {
        if let Some((operation_id, per_node)) = by_name.remove(&item.skill_name) {
            operations.push(BundleOperation {
                skill_name: item.skill_name,
                operation_id,
                per_node,
            });
            continue;
        }
        if is_builtin_name(&item.skill_name) {
            operations.push(BundleOperation {
                skill_name: item.skill_name,
                operation_id: None,
                per_node: builtin_per_node(&batch.targets),
            });
        }
    }
    Ok(BundleInstallResponse { operations })
}

pub(crate) async fn list_fleet_groups(db: &Db) -> Result<Vec<FleetGroup>, DesiredError> {
    Ok(load_bundles(db, None)
        .await?
        .into_iter()
        .map(to_fleet_group)
        .collect())
}

pub(crate) async fn fleet_group(db: &Db, id: &str) -> Result<FleetGroup, DesiredError> {
    Ok(to_fleet_group(get_bundle(db, id).await?))
}

fn to_fleet_group(bundle: BundleView) -> FleetGroup {
    FleetGroup {
        id: bundle.id,
        name: bundle.name,
        description: bundle.description.unwrap_or_default(),
        skills: bundle
            .items
            .into_iter()
            .map(|item| item.skill_name)
            .collect(),
    }
}

fn builtin_per_node(targets: &[TargetView]) -> Vec<PerNode> {
    targets
        .iter()
        .map(|target| PerNode {
            node_id: target.node_id.clone(),
            outcome: if target.exists {
                "already_installed".to_string()
            } else {
                "node_unknown".to_string()
            },
            generation: target.generation,
        })
        .collect()
}

async fn load_bundles(db: &Db, id: Option<&str>) -> Result<Vec<BundleView>, DesiredError> {
    let bundle_rows = match id {
        Some(id) => sqlx::query(
            "SELECT id, name, description, builtin FROM skill_bundles WHERE id = ? ORDER BY id",
        )
        .bind(id)
        .fetch_all(db.pool())
        .await
        .map_err(db_err)?,
        None => sqlx::query("SELECT id, name, description, builtin FROM skill_bundles ORDER BY id")
            .fetch_all(db.pool())
            .await
            .map_err(db_err)?,
    };
    let item_rows = match id {
        Some(id) => sqlx::query(
            r#"SELECT bundle_id, skill_name, version_req
                   FROM skill_bundle_items WHERE bundle_id = ?
                   ORDER BY skill_name"#,
        )
        .bind(id)
        .fetch_all(db.pool())
        .await
        .map_err(db_err)?,
        None => sqlx::query(
            r#"SELECT bundle_id, skill_name, version_req
                   FROM skill_bundle_items
                   ORDER BY bundle_id, skill_name"#,
        )
        .fetch_all(db.pool())
        .await
        .map_err(db_err)?,
    };
    let mut items_by_bundle: HashMap<String, Vec<BundleItemView>> = HashMap::new();
    for row in item_rows {
        let bundle_id: String = row.try_get("bundle_id").map_err(db_err)?;
        items_by_bundle
            .entry(bundle_id)
            .or_default()
            .push(BundleItemView {
                skill_name: row.try_get("skill_name").map_err(db_err)?,
                version_req: row.try_get("version_req").map_err(db_err)?,
            });
    }
    let mut bundles = Vec::with_capacity(bundle_rows.len());
    for row in bundle_rows {
        let id: String = row.try_get("id").map_err(db_err)?;
        let description: Option<String> = row.try_get("description").map_err(db_err)?;
        bundles.push(BundleView {
            items: items_by_bundle.remove(&id).unwrap_or_default(),
            id,
            name: row.try_get("name").map_err(db_err)?,
            description: description.filter(|value| !value.trim().is_empty()),
            builtin: flag(&row)?,
        });
    }
    Ok(bundles)
}

async fn insert_items(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    bundle_id: &str,
    items: &[(String, String)],
) -> Result<(), DesiredError> {
    for (skill_name, version_req) in items {
        sqlx::query(
            "INSERT INTO skill_bundle_items (bundle_id, skill_name, version_req) VALUES (?, ?, ?)",
        )
        .bind(bundle_id)
        .bind(skill_name)
        .bind(version_req)
        .execute(tx.deref_mut())
        .await
        .map_err(db_err)?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String, DesiredError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 255 {
        return Err(DesiredError::InvalidBundle("bundle name is invalid".into()));
    }
    Ok(name.to_string())
}

fn normalize_description(description: Option<String>) -> Option<String> {
    description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_items(items: &[BundleItemIn]) -> Result<Vec<(String, String)>, DesiredError> {
    if items.is_empty() {
        return Err(DesiredError::InvalidBundle(
            "bundle items must not be empty".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let skill_name = item.skill_name.trim();
        let version_req = item.version_req.trim();
        if skill_name.is_empty()
            || skill_name.len() > 63
            || version_req.is_empty()
            || version_req.len() > 64
        {
            return Err(DesiredError::InvalidBundle("bundle item is invalid".into()));
        }
        if !seen.insert(skill_name.to_string()) {
            return Err(DesiredError::InvalidBundle("duplicate skill_name".into()));
        }
        out.push((skill_name.to_string(), version_req.to_string()));
    }
    Ok(out)
}

fn valid_bundle_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if !(2..=63).contains(&bytes.len()) || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn flag(row: &sqlx::mysql::MySqlRow) -> Result<bool, DesiredError> {
    if let Ok(value) = row.try_get::<bool, _>("builtin") {
        return Ok(value);
    }
    if let Ok(value) = row.try_get::<i8, _>("builtin") {
        return Ok(value != 0);
    }
    if let Ok(value) = row.try_get::<i64, _>("builtin") {
        return Ok(value != 0);
    }
    Err(DesiredError::Internal(anyhow::anyhow!(
        "builtin column has an unexpected type"
    )))
}

fn db_err(err: sqlx::Error) -> DesiredError {
    DesiredError::Internal(err.into())
}
