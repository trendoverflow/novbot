// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Desired-set delivery and `SkillStateReport` persistence.
//!
//! Nodes with an empty `skill_abi` are not sent `DesiredSkills`. Package bytes
//! move only through `FetchArtifact`. Fetch tickets are never logged.

use crate::db::Db;
use crate::hub::Hub;
use crate::skill_desired::{self, DesiredError};
use novbot_proto::{
    server_message, ArtifactChunk, DesiredSkills, InstalledSkill, ServerMessage, SkillPolicy,
    SkillRef, SkillStateReport,
};
use sqlx::Row;
use std::collections::HashMap;
use std::ops::DerefMut;

pub(crate) const CHUNK_BYTES: usize = 256 * 1024;

const QUOTA: i64 = 256 * 1024 * 1024;

pub(crate) fn rejection_log(reason: &str) -> String {
    format!("fetch ticket rejected reason={reason}")
}

pub(crate) async fn generation(db: &Db, node_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query("SELECT generation FROM node_skill_sets WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(db.pool())
        .await?;
    match row {
        Some(row) => row.try_get("generation"),
        None => Ok(0),
    }
}

pub(crate) async fn build_desired(
    db: &Db,
    hub: &Hub,
    node_id: &str,
) -> Result<DesiredSkills, sqlx::Error> {
    let generation = generation(db, node_id).await?;
    let rows = sqlx::query(
        r#"
        SELECT d.skill_name, d.version, d.sha256, v.abi, v.size_bytes, v.capabilities_sha256
        FROM node_skills_desired d
        INNER JOIN skills s ON s.name = d.skill_name
        INNER JOIN skill_versions v ON v.skill_id = s.id AND v.version = d.version
        WHERE d.node_id = ?
        ORDER BY d.skill_name
        "#,
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await?;
    let mut skills = Vec::with_capacity(rows.len());
    for row in rows {
        let sha256: String = row.try_get("sha256")?;
        let sha256 = sha256.trim().to_ascii_lowercase();
        let capabilities: String = row.try_get("capabilities_sha256")?;
        let ticket = hub.issue_ticket(node_id, &sha256);
        skills.push(SkillRef {
            name: row.try_get("skill_name")?,
            version: row.try_get("version")?,
            sha256,
            size_bytes: row.try_get("size_bytes")?,
            abi: row.try_get("abi")?,
            capabilities_sha256: capabilities.trim().to_string(),
            signature_envelope: signature_envelope(db, node_id, &row).await?,
            fetch_ticket: ticket,
        });
    }
    Ok(DesiredSkills {
        generation,
        skills,
        policy: Some(default_policy()),
    })
}

fn default_policy() -> SkillPolicy {
    SkillPolicy {
        max_concurrent_runs: 4,
        keep_previous_versions: 2,
        store_quota_bytes: QUOTA,
        max_timeout_ms: 30_000,
        max_memory_mb: 64,
    }
}

async fn signature_envelope(
    db: &Db,
    _node_id: &str,
    _row: &sqlx::mysql::MySqlRow,
) -> Result<Vec<u8>, sqlx::Error> {
    // OSS `skill_versions` has no signature column. Read it only when a later
    // migration added one, and send empty bytes otherwise.
    let present = sqlx::query(
        r#"
        SELECT COUNT(*) AS n
        FROM information_schema.COLUMNS
        WHERE TABLE_SCHEMA = DATABASE()
          AND TABLE_NAME = 'skill_versions'
          AND COLUMN_NAME = 'signature_json'
        "#,
    )
    .fetch_one(db.pool())
    .await?;
    let n: i64 = present.try_get("n")?;
    if n == 0 {
        return Ok(Vec::new());
    }
    let row = sqlx::query(
        r#"
        SELECT CAST(signature_json AS CHAR) AS envelope
        FROM skill_versions v
        INNER JOIN skills s ON s.id = v.skill_id
        INNER JOIN node_skills_desired d ON d.skill_name = s.name AND d.version = v.version
        WHERE d.node_id = ? AND d.skill_name = ? AND d.version = ?
        "#,
    )
    .bind(_node_id)
    .bind(_row.try_get::<String, _>("skill_name").unwrap_or_default())
    .bind(_row.try_get::<String, _>("version").unwrap_or_default())
    .fetch_optional(db.pool())
    .await?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let text: Option<String> = row.try_get("envelope")?;
    Ok(text.unwrap_or_default().into_bytes())
}

pub(crate) fn desired_message(node_id: &str, desired: DesiredSkills) -> ServerMessage {
    tracing::info!(
        node_id,
        generation = desired.generation,
        skills = desired.skills.len(),
        "sending desired skills"
    );
    ServerMessage {
        request_id: uuid::Uuid::new_v4().to_string(),
        body: Some(server_message::Body::DesiredSkills(desired)),
    }
}

pub(crate) async fn push_pending(db: &Db, hub: &Hub, node_ids: &[String]) {
    let mut unique = Vec::new();
    for node_id in node_ids {
        if unique.iter().any(|id: &String| id == node_id) {
            continue;
        }
        unique.push(node_id.clone());
    }
    for node_id in unique {
        if hub.skill_abi(&node_id).unwrap_or_default().is_empty() {
            continue;
        }
        if !hub.is_connected(&node_id) {
            continue;
        }
        match build_desired(db, hub, &node_id).await {
            Ok(desired) => {
                let msg = desired_message(&node_id, desired);
                hub.send(&node_id, msg).await;
            }
            Err(err) => {
                tracing::warn!(node_id, error = %err, "desired skills push failed");
            }
        }
    }
}

pub(crate) async fn apply_report(db: &Db, report: SkillStateReport) -> Result<(), DesiredError> {
    let mut tx = db
        .pool()
        .begin()
        .await
        .map_err(|err| DesiredError::Internal(err.into()))?;
    skill_desired::lock_audit_chain(&mut tx).await?;
    let existing = sqlx::query(
        r#"
        SELECT skill_name, version, sha256, state, reason, applied_generation
        FROM node_skills_actual
        WHERE node_id = ?
        "#,
    )
    .bind(&report.node_id)
    .fetch_all(tx.deref_mut())
    .await
    .map_err(|err| DesiredError::Internal(err.into()))?;
    let mut max_generation = 0i64;
    let mut prior: HashMap<String, (String, String, String, String)> = HashMap::new();
    for row in existing {
        let generation: i64 = row
            .try_get("applied_generation")
            .map_err(|err| DesiredError::Internal(err.into()))?;
        max_generation = max_generation.max(generation);
        let name: String = row
            .try_get("skill_name")
            .map_err(|err| DesiredError::Internal(err.into()))?;
        let reason: Option<String> = row
            .try_get("reason")
            .map_err(|err| DesiredError::Internal(err.into()))?;
        let sha: String = row
            .try_get("sha256")
            .map_err(|err| DesiredError::Internal(err.into()))?;
        prior.insert(
            name,
            (
                row.try_get("version")
                    .map_err(|err| DesiredError::Internal(err.into()))?,
                sha.trim().to_ascii_lowercase(),
                row.try_get("state")
                    .map_err(|err| DesiredError::Internal(err.into()))?,
                reason.unwrap_or_default(),
            ),
        );
    }
    if report.applied_generation < max_generation {
        tx.rollback()
            .await
            .map_err(|err| DesiredError::Internal(err.into()))?;
        return Ok(());
    }
    sqlx::query("DELETE FROM node_skills_actual WHERE node_id = ?")
        .bind(&report.node_id)
        .execute(tx.deref_mut())
        .await
        .map_err(|err| DesiredError::Internal(err.into()))?;
    for skill in &report.skills {
        let previous = serde_json::to_string(&skill.previous_sha256)
            .map_err(|err| DesiredError::Internal(err.into()))?;
        let reason = if skill.reason.is_empty() {
            None
        } else {
            Some(skill.reason.as_str())
        };
        let detail = if skill.reason_detail.is_empty() {
            None
        } else {
            Some(skill.reason_detail.as_str())
        };
        sqlx::query(
            r#"
            INSERT INTO node_skills_actual (
                node_id, skill_name, version, sha256, state, reason, reason_detail,
                attempts, previous_json, applied_generation, reported_at
            ) VALUES (
                ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP(3)
            )
            "#,
        )
        .bind(&report.node_id)
        .bind(&skill.name)
        .bind(&skill.version)
        .bind(&skill.sha256)
        .bind(&skill.state)
        .bind(reason)
        .bind(detail)
        .bind(skill.attempts)
        .bind(previous)
        .bind(report.applied_generation)
        .execute(tx.deref_mut())
        .await
        .map_err(|err| DesiredError::Internal(err.into()))?;
        maybe_audit(
            &mut tx,
            &report.node_id,
            report.applied_generation,
            skill,
            prior.get(&skill.name),
        )
        .await?;
    }
    tx.commit()
        .await
        .map_err(|err| DesiredError::Internal(err.into()))?;
    Ok(())
}

async fn maybe_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    node_id: &str,
    generation: i64,
    skill: &InstalledSkill,
    prior: Option<&(String, String, String, String)>,
) -> Result<(), DesiredError> {
    let action = match skill.state.as_str() {
        "installed" => "skill.installed",
        "failed" => "skill.failed",
        _ => return Ok(()),
    };
    let sha = skill.sha256.trim().to_ascii_lowercase();
    if let Some((version, prior_sha, state, reason)) = prior {
        if version == &skill.version
            && prior_sha == &sha
            && state == &skill.state
            && reason == &skill.reason
        {
            return Ok(());
        }
    }
    let detail = serde_json::json!({
        "version": skill.version,
        "sha256": sha,
        "state": skill.state,
        "reason": skill.reason,
        "applied_generation": generation,
        "attempts": skill.attempts,
    });
    let detail_json =
        serde_json::to_string(&detail).map_err(|err| DesiredError::Internal(err.into()))?;
    skill_desired::append_node_audit(tx, node_id, action, &skill.name, &detail_json).await
}

pub(crate) fn artifact_chunks(
    bytes: &[u8],
    offset: i64,
) -> Result<Vec<ArtifactChunk>, &'static str> {
    if offset < 0 {
        return Err("offset past artifact");
    }
    let start = offset as usize;
    if start > bytes.len() {
        return Err("offset past artifact");
    }
    let total = bytes.len() as i64;
    if start == bytes.len() {
        return Ok(vec![ArtifactChunk {
            offset,
            data: Vec::new(),
            total_size: total,
        }]);
    }
    let mut chunks = Vec::new();
    let mut cursor = start;
    while cursor < bytes.len() {
        let end = (cursor + CHUNK_BYTES).min(bytes.len());
        chunks.push(ArtifactChunk {
            offset: cursor as i64,
            data: bytes[cursor..end].to_vec(),
            total_size: total,
        });
        cursor = end;
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::{artifact_chunks, rejection_log};
    use sha2::{Digest, Sha256};

    #[test]
    fn rejection_log_has_no_ticket_slot() {
        let ticket = "0123456789abcdef0123456789abcdef";
        let line = rejection_log("mismatch");
        assert!(!line.contains(ticket));
        assert_eq!(line, "fetch ticket rejected reason=mismatch");
    }

    #[test]
    fn chunks_resume_from_offset_and_hash() {
        let bytes: Vec<u8> = (0..300_000).map(|i| (i % 251) as u8).collect();
        let full = artifact_chunks(&bytes, 0).unwrap();
        assert!(full.len() > 1);
        let mut joined = Vec::new();
        for (index, chunk) in full.iter().enumerate() {
            assert_eq!(chunk.offset, joined.len() as i64);
            assert_eq!(chunk.total_size, bytes.len() as i64);
            if index + 1 != full.len() {
                assert_eq!(chunk.data.len(), super::CHUNK_BYTES);
            }
            joined.extend_from_slice(&chunk.data);
        }
        assert_eq!(joined, bytes);
        let offset = 100_000i64;
        let tail = artifact_chunks(&bytes, offset).unwrap();
        let mut resumed = bytes[..offset as usize].to_vec();
        for chunk in &tail {
            resumed.extend_from_slice(&chunk.data);
        }
        assert_eq!(resumed, bytes);
        let digest = hex::encode(Sha256::digest(&resumed));
        assert_eq!(digest, hex::encode(Sha256::digest(&bytes)));
    }
}
