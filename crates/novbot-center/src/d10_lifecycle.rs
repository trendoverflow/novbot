// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! D10.2, D10.3, D10.5, D10.6, D10.9, and D10.10 on three in-process nodes.
//!
//! `orb-arm-1`, `orb-arm-2`, and `orb-arm-3` are rows plus `ControlClient`
//! sessions. This does not talk to OrbStack.

use crate::db::{self, Db};
use crate::grpc::ControlSvc;
use crate::http::{router, AppState};
use crate::hub::Hub;
use crate::hub_pack;
use async_trait::async_trait;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use novbot_core::{ProbePolicy, Spec, SpecKind};
use novbot_node::skills::{
    ArtifactSource, DesiredSet, Fetched, GrpcArtifactSource, InstallLimits, SkillHost,
    SkillSnapshot, ABI, CATALOG,
};
use novbot_node::{apply_dispatch_overlay, execute_spec};
use novbot_proto::control_client::ControlClient;
use novbot_proto::control_server::ControlServer;
use novbot_proto::{
    client_message, server_message, ClientMessage, Dispatch, InstalledSkill, NodeReady,
    PullConfigRequest, PullSkillsRequest, RegisterRequest, ReportResultRequest, ServerMessage,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, HashMap};
use std::io::{Cursor, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Channel;
use tower::ServiceExt;

const RESP_LIMIT: usize = 8 * 1024 * 1024;
const QUIET: Duration = Duration::from_millis(500);
const ARM1: &str = "orb-arm-1";
const ARM2: &str = "orb-arm-2";
const ARM3: &str = "orb-arm-3";
const SKILL: &str = "os-release-check";
const SPEC_ID: &str = "os-release";

/// Isolates `role=db` from leftover nodes in the shared test database.
static SUITE_MARK: Mutex<String> = Mutex::new(String::new());

#[tokio::test]
#[cfg_attr(
    novbot_test_db_missing,
    ignore = "NOVBOT_TEST_DATABASE_URL is unset or empty"
)]
async fn three_node_lifecycle_covers_install_through_reboot() {
    let db = db::connect_test_db()
        .await
        .expect("NOVBOT_TEST_DATABASE_URL");
    *SUITE_MARK.lock().expect("suite mark") = uuid::Uuid::new_v4().simple().to_string();
    let release = prepare_os_release();
    let _clear_fixture = ClearOsReleaseFixture;

    sqlx::query("DELETE FROM nodes WHERE node_id IN (?, ?, ?)")
        .bind(ARM1)
        .bind(ARM2)
        .bind(ARM3)
        .execute(db.pool())
        .await
        .expect("delete prior node rows");
    let audit_mark = audit_max_id(&db).await;

    let hub = Hub::new();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let incoming = TcpListenerStream::new(listener);
    let svc = ControlSvc::new(db.clone(), hub.clone(), None, Duration::from_secs(120));
    tokio::spawn(async move {
        let _ = tonic::transport::Server::builder()
            .add_service(ControlServer::new(svc))
            .serve_with_incoming(incoming)
            .await;
    });
    let endpoint = format!("http://{addr}");
    let app = router(AppState {
        db: db.clone(),
        hub: hub.clone(),
        api_token: None,
        license_env: None,
    });

    // The committed 1.0.0 fixture is the D10.7 tamper package. Its guest is not
    // the example, so a run returns `unknown op`. D10.5 has to execute 1.0.0.
    let v1_bytes = hub_pack::os_release_package();
    replace_version_if_different(&db, SKILL, "1.0.0", &v1_bytes).await;
    let v1 = upload(&app, v1_bytes).await;
    assert_eq!(v1["name"], SKILL);
    assert_eq!(v1["version"], "1.0.0");
    let sha_v1 = v1["sha256"].as_str().unwrap().trim().to_string();
    let cap_v1 = v1["capabilities_sha256"]
        .as_str()
        .unwrap()
        .trim()
        .to_string();
    let v11 = upload(&app, hub_pack::os_release_110_package()).await;
    assert_eq!(v11["name"], SKILL);
    assert_eq!(v11["version"], "1.1.0");
    let sha_v11 = v11["sha256"].as_str().unwrap().trim().to_string();
    let cap_v11 = v11["capabilities_sha256"]
        .as_str()
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(sha_v1, sha_v11);

    let mut nodes = Vec::new();
    for (id, role) in [(ARM1, "app"), (ARM2, "app"), (ARM3, "db")] {
        let mut node = prepare_node(id, role, &endpoint).await;
        boot(&mut node, &endpoint, &hub).await;
        assert_eq!(wasm(node.host()), 0);
        nodes.push(node);
    }
    put_spec(&app, ARM1).await;
    put_spec(&app, ARM3).await;

    // D10.2: hand-picked orb-arm-1 union label role=db. orb-arm-2 is neither.
    let installed = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.0.0",
            "node_ids": [ARM1],
            "selector": {"labels": {"role": "db", "suite": suite_mark()}},
            "accepted_capabilities_sha256": cap_v1,
        }),
    )
    .await;
    let targets = node_ids_of(&installed);
    assert_eq!(
        targets,
        vec![ARM1.to_string(), ARM3.to_string()],
        "{installed}"
    );
    assert!(
        !targets.iter().any(|id| id == ARM2),
        "label selector must not select orb-arm-2: {installed}"
    );
    assert_eq!(outcome_of(&installed, ARM1), "pending");
    assert_eq!(outcome_of(&installed, ARM3), "pending");
    assert_eq!(generation_in(&installed, ARM1), 1);
    assert_eq!(generation_in(&installed, ARM3), 1);
    let first_op = installed["operation_id"].as_str().unwrap().to_string();
    assert_eq!(
        operation_hash(&db, &first_op).await,
        cap_v1.to_ascii_lowercase()
    );
    assert_eq!(
        desired_sha(&db, ARM1).await.as_deref(),
        Some(sha_v1.as_str())
    );
    assert_eq!(
        desired_sha(&db, ARM3).await.as_deref(),
        Some(sha_v1.as_str())
    );
    assert!(desired_of(&db, ARM2).await.is_none());

    let desired_arm1 = recv_desired(nodes[0].inbound()).await;
    let desired_arm3 = recv_desired(nodes[2].inbound()).await;
    assert_eq!(desired_arm1.skills.len(), 1);
    assert_eq!(desired_arm1.skills[0].version, "1.0.0");
    assert_eq!(desired_arm1.skills[0].sha256, sha_v1);
    assert_eq!(
        desired_arm1.policy.as_ref().unwrap().keep_previous_versions,
        2
    );
    assert_eq!(desired_arm3.skills[0].sha256, sha_v1);

    let wrong_hash = "ab".repeat(32);
    let wrong_ops_before = count_where(
        &db,
        "SELECT COUNT(*) AS n FROM skill_operations WHERE capabilities_sha256 = ?",
        &[&wrong_hash],
    )
    .await;
    let (status, wrong) = exchange_json(
        &app,
        "POST",
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.0.0",
            "node_ids": [ARM1],
            "selector": {"labels": {"role": "db", "suite": suite_mark()}},
            "accepted_capabilities_sha256": wrong_hash,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{wrong}");
    assert_eq!(wrong["code"], "capabilities_changed");
    assert_eq!(
        count_where(
            &db,
            "SELECT COUNT(*) AS n FROM skill_operations WHERE capabilities_sha256 = ?",
            &[&wrong_hash],
        )
        .await,
        wrong_ops_before,
        "rejected install must not write an operation"
    );
    assert_eq!(
        desired_op(&db, ARM1).await.as_deref(),
        Some(first_op.as_str())
    );
    assert_eq!(
        desired_op(&db, ARM3).await.as_deref(),
        Some(first_op.as_str())
    );
    assert!(desired_of(&db, ARM2).await.is_none());
    assert_eq!(generation_of(&db, ARM1).await, 1);
    assert_eq!(generation_of(&db, ARM3).await, 1);

    // Same version again: another operation and audit row, same desired generation.
    let again = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.0.0",
            "node_ids": [ARM1],
            "accepted_capabilities_sha256": cap_v1,
        }),
    )
    .await;
    assert_eq!(outcome_of(&again, ARM1), "already_installed", "{again}");
    assert_eq!(generation_in(&again, ARM1), 1);
    let again_op = again["operation_id"].as_str().unwrap().to_string();
    assert_ne!(again_op, first_op);
    assert_eq!(
        operation_hash(&db, &again_op).await,
        cap_v1.to_ascii_lowercase()
    );
    assert_eq!(
        desired_op(&db, ARM1).await.as_deref(),
        Some(first_op.as_str())
    );
    assert_eq!(desired_version(&db, ARM1).await.as_deref(), Some("1.0.0"));
    assert_eq!(
        desired_sha(&db, ARM1).await.as_deref(),
        Some(sha_v1.as_str())
    );
    assert_eq!(generation_of(&db, ARM1).await, 1);
    assert_eq!(
        count_where(
            &db,
            "SELECT COUNT(*) AS n FROM audit_events WHERE target_id = ? AND detail_json LIKE ?",
            &[SKILL, &format!("%{again_op}%")],
        )
        .await,
        1,
        "same-version reinstall still audits"
    );
    assert_quiet(nodes[0].inbound(), "reinstall must not push DesiredSkills").await;
    assert_quiet(nodes[2].inbound(), "reinstall must not push DesiredSkills").await;

    let fetches_before = fetch_count(&nodes[1], &sha_v1);
    assert_quiet(
        nodes[1].inbound(),
        "orb-arm-2 must not be pushed a desired set",
    )
    .await;
    let snap1 = reconcile_report(&mut nodes[0], &desired_arm1).await;
    let snap3 = reconcile_report(&mut nodes[2], &desired_arm3).await;
    assert_eq!(snap1.skills[0].state, "installed", "{snap1:?}");
    assert_eq!(snap1.skills[0].version, "1.0.0");
    assert_eq!(snap3.skills[0].state, "installed", "{snap3:?}");
    assert_eq!(
        nodes[0].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        nodes[2].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(wasm(nodes[0].host()), 0);
    assert_eq!(wasm(nodes[2].host()), 0);
    assert!(fetch_count(&nodes[0], &sha_v1) >= 1);
    assert!(fetch_count(&nodes[2], &sha_v1) >= 1);
    assert_eq!(fetch_count(&nodes[1], &sha_v1), fetches_before);
    assert!(nodes[1].host().snapshot().skills.is_empty());
    assert!(actual_map(&db, ARM2).await.is_empty());
    assert_no_arm2_audit(&db, audit_mark).await;

    // D10.3
    assert_installed_version(&app, ARM1, "1.0.0").await;
    assert_installed_version(&app, ARM3, "1.0.0").await;
    assert_skill_absent(&app, ARM2).await;

    // D10.5 upgrade to the packed 1.1.0 example.
    let upgraded = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.1.0",
            "node_ids": [ARM1, ARM3],
            "accepted_capabilities_sha256": cap_v11,
        }),
    )
    .await;
    assert_eq!(outcome_of(&upgraded, ARM1), "pending", "{upgraded}");
    assert_eq!(outcome_of(&upgraded, ARM3), "pending", "{upgraded}");
    let desired_up_1 = recv_desired(nodes[0].inbound()).await;
    let desired_up_3 = recv_desired(nodes[2].inbound()).await;
    assert_eq!(desired_up_1.skills[0].version, "1.1.0");
    assert_eq!(desired_up_1.skills[0].sha256, sha_v11);
    assert_eq!(
        desired_up_1.policy.as_ref().unwrap().keep_previous_versions,
        2
    );
    let up1 = reconcile_report(&mut nodes[0], &desired_up_1).await;
    let up3 = reconcile_report(&mut nodes[2], &desired_up_3).await;
    assert_eq!(up1.skills[0].state, "installed", "{up1:?}");
    assert_eq!(up1.skills[0].version, "1.1.0");
    assert_eq!(up3.skills[0].version, "1.1.0");
    assert_eq!(wasm(nodes[0].host()), 0);
    assert_eq!(wasm(nodes[2].host()), 0);
    assert_installed_version(&app, ARM1, "1.1.0").await;
    assert_installed_version(&app, ARM3, "1.1.0").await;
    let ran1 = run_skill(&app, &mut nodes[0], &release).await;
    let ran3 = run_skill(&app, &mut nodes[2], &release).await;
    assert_eq!(ran1.0, "ok", "{ran1:?}");
    assert_eq!(ran3.0, "ok", "{ran3:?}");
    assert_eq!(ran1.1["skill"]["version"], "1.1.0");
    assert_eq!(ran3.1["skill"]["version"], "1.1.0");
    assert_eq!(ran1.1["skill"]["name"], SKILL);
    assert_pretty(&ran1.1, &release, true);
    assert_pretty(&ran3.1, &release, true);
    assert_eq!(ran1.1["ID"], release.id);
    assert_eq!(wasm(nodes[0].host()), 1);
    assert_eq!(wasm(nodes[2].host()), 1);

    // Rollback is a desired-set change. 1.0.0 is still under store/.
    assert!(nodes[0]
        .host()
        .store_path(&sha_v1)
        .join(".verified")
        .is_file());
    assert!(nodes[2]
        .host()
        .store_path(&sha_v1)
        .join(".verified")
        .is_file());
    let fetch_v1_arm1 = fetch_count(&nodes[0], &sha_v1);
    let fetch_v1_arm3 = fetch_count(&nodes[2], &sha_v1);
    let rolled = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/rollback"),
        &json!({"node_ids": [ARM1, ARM3], "to_version": "1.0.0"}),
    )
    .await;
    assert_eq!(outcome_of(&rolled, ARM1), "pending", "{rolled}");
    assert_eq!(outcome_of(&rolled, ARM3), "pending", "{rolled}");
    let desired_back_1 = recv_desired(nodes[0].inbound()).await;
    let desired_back_3 = recv_desired(nodes[2].inbound()).await;
    assert_eq!(desired_back_1.skills[0].version, "1.0.0");
    assert_eq!(desired_back_1.skills[0].sha256, sha_v1);
    let back1 = reconcile_report(&mut nodes[0], &desired_back_1).await;
    let back3 = reconcile_report(&mut nodes[2], &desired_back_3).await;
    assert_eq!(back1.skills[0].state, "installed", "{back1:?}");
    assert_eq!(back1.skills[0].version, "1.0.0");
    assert_eq!(back3.skills[0].version, "1.0.0");
    assert_eq!(fetch_count(&nodes[0], &sha_v1), fetch_v1_arm1);
    assert_eq!(fetch_count(&nodes[2], &sha_v1), fetch_v1_arm3);
    assert_eq!(wasm(nodes[0].host()), 1);
    assert_eq!(wasm(nodes[2].host()), 1);
    assert_installed_version(&app, ARM1, "1.0.0").await;
    assert_installed_version(&app, ARM3, "1.0.0").await;
    let old1 = run_skill(&app, &mut nodes[0], &release).await;
    let old3 = run_skill(&app, &mut nodes[2], &release).await;
    assert_eq!(old1.0, "ok", "{old1:?}");
    assert_eq!(old3.0, "ok", "{old3:?}");
    assert_eq!(old1.1["skill"]["version"], "1.0.0");
    assert_eq!(old3.1["skill"]["version"], "1.0.0");
    assert_pretty(&old1.1, &release, false);
    assert_pretty(&old3.1, &release, false);
    assert_eq!(wasm(nodes[0].host()), 2);
    assert_eq!(wasm(nodes[2].host()), 2);

    // Failed install of a non-fixture version keeps the active skill.
    let tamper_version = format!("2.0.{}", (uuid::Uuid::new_v4().as_u128() % 80_000) + 10_000);
    let tamper_bytes = retarget_version(
        &hub_pack::os_release_110_package(),
        "1.1.0",
        &tamper_version,
    );
    let tamper = upload(&app, tamper_bytes).await;
    assert_eq!(tamper["version"], tamper_version);
    let sha_bad = tamper["sha256"].as_str().unwrap().trim().to_string();
    let cap_bad = tamper["capabilities_sha256"]
        .as_str()
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(sha_bad, sha_v1);
    assert_ne!(sha_bad, sha_v11);
    flip_artifact_byte(&db, &sha_bad).await;
    let wasm_before_bad = wasm(nodes[0].host());
    let bad_fetches = fetch_count(&nodes[0], &sha_bad);
    let bad_install = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": tamper_version,
            "node_ids": [ARM1],
            "accepted_capabilities_sha256": cap_bad,
        }),
    )
    .await;
    assert_eq!(outcome_of(&bad_install, ARM1), "pending", "{bad_install}");
    let desired_bad = recv_desired(nodes[0].inbound()).await;
    assert_eq!(desired_bad.skills[0].version, tamper_version);
    let failed = reconcile_report(&mut nodes[0], &desired_bad).await;
    assert_eq!(failed.skills[0].state, "failed", "{failed:?}");
    assert_eq!(failed.skills[0].reason, "hash_mismatch");
    assert_eq!(
        nodes[0].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(wasm(nodes[0].host()), wasm_before_bad);
    assert!(fetch_count(&nodes[0], &sha_bad) > bad_fetches);
    assert_node_state(&app, ARM1, &tamper_version, "failed").await;

    let fetch_after_fail = fetch_count(&nodes[0], &sha_v1);
    let restored = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/rollback"),
        &json!({"node_ids": [ARM1], "to_version": "1.0.0"}),
    )
    .await;
    assert_eq!(outcome_of(&restored, ARM1), "pending", "{restored}");
    let desired_restored = recv_desired(nodes[0].inbound()).await;
    let restored_snap = reconcile_report(&mut nodes[0], &desired_restored).await;
    assert_eq!(restored_snap.skills[0].state, "installed");
    assert_eq!(restored_snap.skills[0].version, "1.0.0");
    assert_eq!(
        nodes[0].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(fetch_count(&nodes[0], &sha_v1), fetch_after_fail);
    assert_eq!(wasm(nodes[0].host()), wasm_before_bad);
    assert_installed_version(&app, ARM1, "1.0.0").await;
    let (deleted, deleted_body) = exchange(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/v1/skills/{SKILL}/versions/{tamper_version}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        deleted,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&deleted_body)
    );

    // D10.6: queue the upgrade and the dispatch while orb-arm-3 is down.
    let wasm3 = wasm(nodes[2].host());
    let fetches_down = fetch_count(&nodes[2], &sha_v11);
    let gen3 = generation_of(&db, ARM3).await;
    drop_session(&mut nodes[2]);
    wait_disconnected(&hub, ARM3).await;
    assert!(!hub.is_connected(ARM3));
    let queued = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.1.0",
            "node_ids": [ARM3],
            "accepted_capabilities_sha256": cap_v11,
        }),
    )
    .await;
    assert_eq!(outcome_of(&queued, ARM3), "queued", "{queued}");
    assert_eq!(generation_in(&queued, ARM3), gen3 + 1);
    assert_eq!(generation_of(&db, ARM3).await, gen3 + 1);
    assert_eq!(desired_version(&db, ARM3).await.as_deref(), Some("1.1.0"));
    assert_eq!(
        desired_sha(&db, ARM3).await.as_deref(),
        Some(sha_v11.as_str())
    );
    assert_eq!(fetch_count(&nodes[2], &sha_v11), fetches_down);
    assert_eq!(
        nodes[2].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );

    let (status, accepted) = exchange_json(
        &app,
        "POST",
        &format!("/v1/nodes/{ARM3}/dispatch"),
        &json!({"spec_id": SPEC_ID}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(accepted["delivered"], "queued", "{accepted}");
    let queued_run = accepted["run_id"].as_str().unwrap().to_string();
    assert_eq!(pending_count(&db, ARM3).await, 1);
    assert_eq!(fetch_count(&nodes[2], &sha_v11), fetches_down);

    // Reconnect in node order. The dispatch frame waits for NodeReady.
    nodes[2].conn = Some(open_session(&endpoint).await);
    register(&mut nodes[2]).await;
    assert!(!hub.is_connected(ARM3), "Register must not flush the queue");
    let desired_queued = pull_skills(&mut nodes[2]).await;
    assert_eq!(desired_queued.skills[0].version, "1.1.0");
    assert_eq!(desired_queued.skills[0].sha256, sha_v11);
    let installed_while_booting = reconcile_report(&mut nodes[2], &desired_queued).await;
    assert_eq!(installed_while_booting.skills[0].state, "installed");
    assert_eq!(installed_while_booting.skills[0].version, "1.1.0");
    assert_eq!(
        nodes[2].host().active_version(SKILL).as_deref(),
        Some("1.1.0")
    );
    assert_eq!(
        wasm(nodes[2].host()),
        wasm3,
        "install must not run the skill"
    );
    let config_gen = pull_config(&mut nodes[2]).await;
    assert_quiet(
        nodes[2].inbound(),
        "dispatch must not arrive before NodeReady",
    )
    .await;
    assert_eq!(pending_count(&db, ARM3).await, 1);
    assert!(!hub.is_connected(ARM3));
    send_ready(
        &mut nodes[2],
        config_gen,
        installed_while_booting.applied_generation,
    )
    .await;
    let dispatch = recv_dispatch_then_ack(nodes[2].inbound(), SPEC_ID, &queued_run).await;
    assert!(hub.is_connected(ARM3));
    assert_eq!(pending_count(&db, ARM3).await, 0);
    let offline_run = execute_dispatch(&mut nodes[2], &dispatch, &release).await;
    assert_eq!(offline_run.0, "ok", "{offline_run:?}");
    assert_eq!(offline_run.1["skill"]["version"], "1.1.0");
    assert_pretty(&offline_run.1, &release, true);
    assert_eq!(wasm(nodes[2].host()), wasm3 + 1);
    report_result(
        &mut nodes[2],
        &offline_run.2,
        SPEC_ID,
        &offline_run.0,
        &offline_run.1,
    )
    .await;
    assert_installed_version(&app, ARM3, "1.1.0").await;

    // D10.9
    let gen_before_uninstall = generation_of(&db, ARM3).await;
    let (status, blocked) = exchange_json(
        &app,
        "POST",
        &format!("/v1/skills/{SKILL}/uninstall"),
        &json!({"node_ids": [ARM3], "force": false}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{blocked}");
    assert_eq!(blocked["code"], "skill_in_use");
    assert!(blocked["spec_ids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == SPEC_ID));
    assert_eq!(desired_version(&db, ARM3).await.as_deref(), Some("1.1.0"));
    assert_eq!(generation_of(&db, ARM3).await, gen_before_uninstall);
    assert_quiet(nodes[2].inbound(), "skill_in_use must not push").await;

    let forced = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/uninstall"),
        &json!({"node_ids": [ARM3], "force": true}),
    )
    .await;
    assert_eq!(outcome_of(&forced, ARM3), "pending", "{forced}");
    assert_eq!(generation_in(&forced, ARM3), gen_before_uninstall + 1);
    assert!(desired_of(&db, ARM3).await.is_none());
    let desired_gone = recv_desired(nodes[2].inbound()).await;
    assert!(desired_gone.skills.iter().all(|skill| skill.name != SKILL));
    let removed = reconcile_report(&mut nodes[2], &desired_gone).await;
    assert!(removed.skills.iter().all(|skill| skill.name != SKILL));
    assert!(nodes[2].host().active_version(SKILL).is_none());
    assert_eq!(wasm(nodes[2].host()), wasm3 + 1);
    assert_skill_absent(&app, ARM3).await;

    let (status, missing_accept) = exchange_json(
        &app,
        "POST",
        &format!("/v1/nodes/{ARM3}/dispatch"),
        &json!({"spec_id": SPEC_ID}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{missing_accept}");
    assert_eq!(missing_accept["delivered"], "live");
    let missing_run = missing_accept["run_id"].as_str().unwrap().to_string();
    let missing_dispatch = recv_dispatch(nodes[2].inbound(), SPEC_ID, &missing_run).await;
    let missing = execute_dispatch(&mut nodes[2], &missing_dispatch, &release).await;
    assert_eq!(missing.0, "error", "{missing:?}");
    assert_eq!(missing.1["error"]["code"], "skill_not_installed");
    assert_eq!(
        wasm(nodes[2].host()),
        wasm3 + 1,
        "missing skill must not enter run"
    );
    report_result(&mut nodes[2], &missing.2, SPEC_ID, &missing.0, &missing.1).await;

    // Give orb-arm-2 a different desired set so the three snapshots differ.
    let arm2 = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/install"),
        &json!({
            "version": "1.1.0",
            "node_ids": [ARM2],
            "accepted_capabilities_sha256": cap_v11,
        }),
    )
    .await;
    assert_eq!(outcome_of(&arm2, ARM2), "pending", "{arm2}");
    let desired_arm2 = recv_desired(nodes[1].inbound()).await;
    assert_eq!(desired_arm2.skills[0].version, "1.1.0");
    let snap2 = reconcile_report(&mut nodes[1], &desired_arm2).await;
    assert_eq!(snap2.skills[0].state, "installed");
    assert_eq!(wasm(nodes[1].host()), 0);

    let map1 = desired_map(&db, ARM1).await;
    let map2 = desired_map(&db, ARM2).await;
    let map3 = desired_map(&db, ARM3).await;
    assert_eq!(
        map1.get(SKILL).map(|(version, _)| version.as_str()),
        Some("1.0.0")
    );
    assert_eq!(
        map2.get(SKILL).map(|(version, _)| version.as_str()),
        Some("1.1.0")
    );
    assert!(map3.is_empty(), "{map3:?}");
    assert_ne!(map1, map2);
    assert_ne!(map1, map3);
    assert_ne!(map2, map3);

    // D10.10: restart while these three desired sets are still in place.
    // orb-arm-1 is os-release-check 1.0.0, orb-arm-2 is 1.1.0, orb-arm-3 is empty.
    for node in &mut nodes {
        drop_session(node);
    }
    for id in [ARM1, ARM2, ARM3] {
        wait_disconnected(&hub, id).await;
        assert!(!hub.is_connected(id));
    }
    assert_eq!(desired_map(&db, ARM1).await, map1);
    assert_eq!(desired_map(&db, ARM2).await, map2);
    assert_eq!(desired_map(&db, ARM3).await, map3);

    for node in &mut nodes {
        reopen(node, &endpoint).await;
        assert_eq!(wasm(node.host()), 0);
        boot(node, &endpoint, &hub).await;
        assert_eq!(wasm(node.host()), 0, "restart reconcile must not run wasm");
        assert_converged(&db, node).await;
    }
    assert_eq!(desired_map(&db, ARM1).await, map1);
    assert_eq!(desired_map(&db, ARM2).await, map2);
    assert_eq!(desired_map(&db, ARM3).await, map3);
    assert_eq!(
        nodes[0].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(
        nodes[1].host().active_version(SKILL).as_deref(),
        Some("1.1.0")
    );
    assert!(nodes[2].host().active_version(SKILL).is_none());
    assert_installed_version(&app, ARM1, "1.0.0").await;
    assert_installed_version(&app, ARM2, "1.1.0").await;
    assert_skill_absent(&app, ARM3).await;
    assert!(hub.is_connected(ARM1));
    assert!(hub.is_connected(ARM2));
    assert!(hub.is_connected(ARM3));

    // Later step: while one node is down, remove the skill from its desired set.
    let fetch_arm1 = fetch_count(&nodes[0], &sha_v1);
    let wasm_arm1 = wasm(nodes[0].host());
    drop_session(&mut nodes[0]);
    wait_disconnected(&hub, ARM1).await;
    assert!(!hub.is_connected(ARM1));
    let removed_offline = post_json(
        &app,
        &format!("/v1/skills/{SKILL}/uninstall"),
        &json!({"node_ids": [ARM1], "force": true}),
    )
    .await;
    assert_eq!(
        outcome_of(&removed_offline, ARM1),
        "queued",
        "{removed_offline}"
    );
    assert!(desired_of(&db, ARM1).await.is_none());
    assert_eq!(fetch_count(&nodes[0], &sha_v1), fetch_arm1);
    assert_eq!(wasm(nodes[0].host()), wasm_arm1);
    assert_eq!(
        nodes[0].host().active_version(SKILL).as_deref(),
        Some("1.0.0")
    );
    assert_eq!(desired_map(&db, ARM2).await, map2);
    assert_eq!(desired_map(&db, ARM3).await, map3);

    reopen(&mut nodes[0], &endpoint).await;
    assert_eq!(wasm(nodes[0].host()), 0);
    boot(&mut nodes[0], &endpoint, &hub).await;
    assert_eq!(
        wasm(nodes[0].host()),
        0,
        "restart reconcile must not run wasm"
    );
    assert_converged(&db, &nodes[0]).await;
    assert!(nodes[0].host().active_version(SKILL).is_none());
    assert!(nodes[0]
        .host()
        .snapshot()
        .skills
        .iter()
        .all(|skill| skill.name != SKILL));
    assert!(!desired_map(&db, ARM1).await.contains_key(SKILL));
    assert!(!actual_map(&db, ARM1).await.contains_key(SKILL));
    assert_skill_absent(&app, ARM1).await;
    assert_eq!(desired_map(&db, ARM2).await, map2);
    assert_eq!(desired_map(&db, ARM3).await, map3);
    assert_eq!(
        nodes[1].host().active_version(SKILL).as_deref(),
        Some("1.1.0")
    );
    assert!(nodes[2].host().active_version(SKILL).is_none());
    assert!(hub.is_connected(ARM1));
    assert!(hub.is_connected(ARM2));
    assert!(hub.is_connected(ARM3));

    // Put the D10.7 fixture back so a later run of that test is not version_exists.
    let fixture_v1 = fixture("os-release-check-1.0.0.nbskill");
    replace_version_if_different(&db, SKILL, "1.0.0", &fixture_v1).await;
    let restored = upload(&app, fixture_v1).await;
    assert_eq!(restored["version"], "1.0.0");
}

struct ReleaseFile {
    id: String,
    pretty: String,
}

struct ClearOsReleaseFixture;

impl Drop for ClearOsReleaseFixture {
    fn drop(&mut self) {
        novbot_node::set_missing_os_release_fixture(None);
    }
}

fn prepare_os_release() -> ReleaseFile {
    let mut release = hub_pack::prepare_os_release();
    if hub_pack::os_field(&release.text, "PRETTY_NAME").is_empty() {
        extend_pretty_name(&mut release);
    }
    if let Some(path) = release.fixture.clone() {
        novbot_node::set_missing_os_release_fixture(Some(path));
    }
    ReleaseFile {
        id: hub_pack::os_field(&release.text, "ID"),
        pretty: hub_pack::os_field(&release.text, "PRETTY_NAME"),
    }
}

fn extend_pretty_name(release: &mut hub_pack::OsReleaseFile) {
    let Some(path) = release.fixture.clone() else {
        return;
    };
    if !release.text.ends_with('\n') {
        release.text.push('\n');
    }
    release.text.push_str("PRETTY_NAME=\"NovBot Test\"\n");
    std::fs::write(path, &release.text).expect("extend os-release fixture");
}

struct Node {
    id: &'static str,
    role: &'static str,
    data: tempfile::TempDir,
    calls: Arc<Mutex<Vec<(String, u64)>>>,
    host: Option<SkillHost>,
    conn: Option<Conn>,
}

struct Conn {
    tx: mpsc::Sender<ClientMessage>,
    inbound: tonic::Streaming<ServerMessage>,
    _client: ControlClient<Channel>,
}

impl Node {
    fn host(&self) -> &SkillHost {
        self.host.as_ref().expect("skill host")
    }

    fn inbound(&mut self) -> &mut tonic::Streaming<ServerMessage> {
        &mut self.conn.as_mut().expect("session").inbound
    }
}

struct FetchLog {
    inner: GrpcArtifactSource,
    calls: Arc<Mutex<Vec<(String, u64)>>>,
}

#[async_trait]
impl ArtifactSource for FetchLog {
    async fn fetch(
        &self,
        sha256: &str,
        fetch_ticket: &str,
        offset: u64,
    ) -> Result<Fetched, String> {
        self.calls
            .lock()
            .expect("fetch log")
            .push((sha256.to_string(), offset));
        self.inner.fetch(sha256, fetch_ticket, offset).await
    }
}

async fn prepare_node(id: &'static str, role: &'static str, endpoint: &str) -> Node {
    let data = tempfile::tempdir().expect("data dir");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let host = open_host(data.path(), endpoint, id, &calls).await;
    Node {
        id,
        role,
        data,
        calls,
        host: Some(host),
        conn: None,
    }
}

async fn open_host(
    data: &std::path::Path,
    endpoint: &str,
    node_id: &str,
    calls: &Arc<Mutex<Vec<(String, u64)>>>,
) -> SkillHost {
    let source = Arc::new(FetchLog {
        inner: GrpcArtifactSource::new(endpoint, node_id),
        calls: Arc::clone(calls),
    });
    SkillHost::open(data, source, fast_limits())
        .await
        .expect("skill host")
}

async fn reopen(node: &mut Node, endpoint: &str) {
    node.conn.take();
    let calls = Arc::clone(&node.calls);
    let id = node.id;
    let path = node.data.path().to_path_buf();
    node.host.take();
    node.host = Some(open_host(&path, endpoint, id, &calls).await);
}

async fn open_session(endpoint: &str) -> Conn {
    let mut client = ControlClient::connect(endpoint.to_string())
        .await
        .expect("client");
    let (tx, rx) = mpsc::channel(64);
    let inbound = client
        .session(ReceiverStream::new(rx))
        .await
        .expect("session")
        .into_inner();
    Conn {
        tx,
        inbound,
        _client: client,
    }
}

fn drop_session(node: &mut Node) {
    node.conn.take();
}

async fn wait_disconnected(hub: &Hub, node_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while hub.is_connected(node_id) {
        if Instant::now() > deadline {
            panic!("{node_id} stayed connected");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn boot(node: &mut Node, endpoint: &str, hub: &Hub) {
    node.conn = Some(open_session(endpoint).await);
    register(node).await;
    assert!(
        !hub.is_connected(node.id),
        "Register must not mark {} ready",
        node.id
    );
    let desired = pull_skills(node).await;
    let snap = reconcile_report(node, &desired).await;
    let config_gen = pull_config(node).await;
    send_ready(node, config_gen, snap.applied_generation).await;
    expect_ack(node).await;
    assert!(hub.is_connected(node.id), "{}", node.id);
}

async fn register(node: &mut Node) {
    let installed = node.host().register_installed();
    let generation = node.host().applied_generation();
    let id = node.id;
    let role = node.role;
    let conn = node.conn.as_mut().expect("session");
    conn.tx
        .send(register_msg(id, role, installed, generation))
        .await
        .expect("register");
    let msg = recv(&mut conn.inbound).await;
    match msg.body {
        Some(server_message::Body::Register(resp)) => assert!(resp.accepted, "{resp:?}"),
        other => panic!("expected Register, got {other:?}"),
    }
}

async fn pull_skills(node: &mut Node) -> novbot_proto::DesiredSkills {
    let known = node.host().applied_generation();
    let id = node.id.to_string();
    let conn = node.conn.as_mut().expect("session");
    conn.tx
        .send(ClientMessage {
            request_id: format!("pull-skills-{id}"),
            body: Some(client_message::Body::PullSkills(PullSkillsRequest {
                node_id: id,
                known_generation: known,
            })),
        })
        .await
        .expect("pull skills");
    let msg = recv(&mut conn.inbound).await;
    match msg.body {
        Some(server_message::Body::DesiredSkills(desired)) => {
            assert!(
                desired
                    .skills
                    .iter()
                    .all(|skill| !skill.fetch_ticket.is_empty()),
                "missing fetch ticket: {desired:?}"
            );
            assert_eq!(
                desired
                    .policy
                    .as_ref()
                    .map(|policy| policy.keep_previous_versions),
                Some(2)
            );
            desired
        }
        Some(server_message::Body::Dispatch(dispatch)) => {
            panic!("dispatch before NodeReady: {dispatch:?}")
        }
        other => panic!("expected DesiredSkills, got {other:?}"),
    }
}

async fn reconcile_report(node: &mut Node, desired: &novbot_proto::DesiredSkills) -> SkillSnapshot {
    let before = wasm(node.host());
    let snap = node.host().reconcile(DesiredSet::from_proto(desired)).await;
    assert_eq!(wasm(node.host()), before, "reconcile must not enter run");
    let id = node.id;
    send_report(&node.conn.as_ref().expect("session").tx, id, &snap).await;
    expect_ack(node).await;
    snap
}

async fn pull_config(node: &mut Node) -> i64 {
    let id = node.id.to_string();
    let conn = node.conn.as_mut().expect("session");
    conn.tx
        .send(ClientMessage {
            request_id: format!("pull-config-{id}"),
            body: Some(client_message::Body::PullConfig(PullConfigRequest {
                node_id: id,
                known_generation: 0,
            })),
        })
        .await
        .expect("pull config");
    let msg = recv(&mut conn.inbound).await;
    match msg.body {
        Some(server_message::Body::PullConfig(resp)) => resp.config_generation,
        Some(server_message::Body::Dispatch(dispatch)) => {
            panic!("dispatch before NodeReady: {dispatch:?}")
        }
        other => panic!("expected PullConfig, got {other:?}"),
    }
}

fn send_ready_msg(node_id: &str, config_generation: i64, skill_generation: i64) -> ClientMessage {
    ClientMessage {
        request_id: format!("ready-{node_id}"),
        body: Some(client_message::Body::NodeReady(NodeReady {
            node_id: node_id.to_string(),
            config_generation,
            skill_set_generation: skill_generation,
        })),
    }
}

async fn send_ready(node: &mut Node, config_generation: i64, skill_generation: i64) {
    let id = node.id;
    node.conn
        .as_ref()
        .expect("session")
        .tx
        .send(send_ready_msg(id, config_generation, skill_generation))
        .await
        .expect("ready");
}

async fn run_skill(
    app: &axum::Router,
    node: &mut Node,
    release: &ReleaseFile,
) -> (String, Value, String) {
    let (status, accepted) = exchange_json(
        app,
        "POST",
        &format!("/v1/nodes/{}/dispatch", node.id),
        &json!({"spec_id": SPEC_ID}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    assert_eq!(accepted["delivered"], "live", "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_string();
    let dispatch = recv_dispatch(node.inbound(), SPEC_ID, &run_id).await;
    let outcome = execute_dispatch(node, &dispatch, release).await;
    report_result(node, &outcome.2, SPEC_ID, &outcome.0, &outcome.1).await;
    outcome
}

async fn execute_dispatch(
    node: &mut Node,
    dispatch: &Dispatch,
    _release: &ReleaseFile,
) -> (String, Value, String) {
    let mut spec = os_spec();
    apply_dispatch_overlay(&mut spec, &dispatch.params_json);
    let data = node.data.path().to_path_buf();
    let host = node.host();
    let (status, payload) = execute_spec(
        &spec,
        &ProbePolicy::default(),
        host,
        &data,
        &dispatch.run_id,
    )
    .await;
    (status, payload, dispatch.run_id.clone())
}

async fn report_result(
    node: &mut Node,
    run_id: &str,
    spec_id: &str,
    status: &str,
    payload: &Value,
) {
    let id = node.id.to_string();
    let payload_json = serde_json::to_string(payload).unwrap();
    node.conn
        .as_ref()
        .expect("session")
        .tx
        .send(ClientMessage {
            request_id: format!("result-{run_id}"),
            body: Some(client_message::Body::ReportResult(ReportResultRequest {
                node_id: id,
                run_id: run_id.to_string(),
                spec_id: spec_id.to_string(),
                status: status.to_string(),
                payload_json,
                observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            })),
        })
        .await
        .expect("report result");
    let msg = recv(node.inbound()).await;
    match msg.body {
        Some(server_message::Body::ReportResult(resp)) => assert!(resp.accepted, "{resp:?}"),
        other => panic!("expected ReportResult, got {other:?}"),
    }
}

fn os_spec() -> Spec {
    Spec {
        id: SPEC_ID.to_string(),
        kind: SpecKind::Skill,
        params: json!({"skill": SKILL, "version": "^1.0"}),
        threshold: None,
    }
}

fn assert_pretty(payload: &Value, release: &ReleaseFile, present: bool) {
    if present {
        assert!(payload.get("PRETTY_NAME").is_some(), "{payload}");
        if !release.pretty.is_empty() {
            assert_eq!(payload["PRETTY_NAME"], release.pretty);
        }
    } else {
        assert!(payload.get("PRETTY_NAME").is_none(), "{payload}");
    }
}

fn wasm(host: &SkillHost) -> u64 {
    host.wasm_run_entries()
}

fn fetch_count(node: &Node, sha: &str) -> usize {
    node.calls
        .lock()
        .expect("fetch log")
        .iter()
        .filter(|(item, _)| item == sha)
        .count()
}

async fn assert_converged(db: &Db, node: &Node) {
    let desired = desired_map(db, node.id).await;
    let actual = actual_map(db, node.id).await;
    assert_eq!(actual, desired, "{}", node.id);
    let snap = node.host().snapshot();
    let mut reported = BTreeMap::new();
    for skill in &snap.skills {
        assert_eq!(skill.state, "installed", "{snap:?}");
        reported.insert(
            skill.name.clone(),
            (skill.version.clone(), skill.sha256.clone()),
        );
    }
    assert_eq!(reported, desired, "{}", node.id);
    assert_eq!(snap.applied_generation, generation_of(db, node.id).await);
}

fn suite_mark() -> String {
    SUITE_MARK.lock().expect("suite mark").clone()
}

fn labels_for(role: &str) -> HashMap<String, String> {
    let platform = novbot_node::skills::current_platform();
    let (os, arch) = platform.split_once('/').expect("os/arch platform");
    HashMap::from([
        ("role".to_string(), role.to_string()),
        ("os".to_string(), os.to_string()),
        ("arch".to_string(), arch.to_string()),
        ("suite".to_string(), suite_mark()),
    ])
}

fn register_msg(
    node_id: &str,
    role: &str,
    installed: Vec<InstalledSkill>,
    generation: i64,
) -> ClientMessage {
    ClientMessage {
        request_id: format!("reg-{node_id}-{generation}"),
        body: Some(client_message::Body::Register(RegisterRequest {
            node_id: node_id.to_string(),
            hostname: node_id.to_string(),
            version: "0.1.0".into(),
            bootstrap_token: String::new(),
            labels: labels_for(role),
            skill_abi: ABI.into(),
            capabilities_supported: CATALOG.iter().map(|cap| (*cap).to_string()).collect(),
            skill_set_generation: generation,
            installed,
            supports_ready: true,
        })),
    }
}

fn fast_limits() -> InstallLimits {
    InstallLimits {
        max_attempts: 2,
        max_elapsed: Duration::from_secs(600),
        min_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        ready_bound: Duration::from_millis(20),
        sleeper: Arc::new(|_| Box::pin(async {})),
    }
}

async fn replace_version_if_different(db: &Db, name: &str, version: &str, bytes: &[u8]) {
    let sha = hex_sha256(bytes);
    let existing: Option<String> = sqlx::query(
        r#"
        SELECT v.sha256
        FROM skill_versions v
        INNER JOIN skills s ON s.id = v.skill_id
        WHERE s.name = ? AND v.version = ?
        "#,
    )
    .bind(name)
    .bind(version)
    .fetch_optional(db.pool())
    .await
    .expect("lookup skill version")
    .map(|row| row.try_get::<String, _>("sha256").expect("sha256"));
    let Some(existing) = existing else {
        return;
    };
    if existing.eq_ignore_ascii_case(&sha) {
        return;
    }
    sqlx::query("DELETE FROM node_skills_desired WHERE skill_name = ? AND version = ?")
        .bind(name)
        .bind(version)
        .execute(db.pool())
        .await
        .expect("clear desired version");
    sqlx::query("DELETE FROM node_skills_actual WHERE skill_name = ? AND version = ?")
        .bind(name)
        .bind(version)
        .execute(db.pool())
        .await
        .expect("clear actual version");
    sqlx::query(
        r#"
        DELETE v FROM skill_versions v
        INNER JOIN skills s ON s.id = v.skill_id
        WHERE s.name = ? AND v.version = ?
        "#,
    )
    .bind(name)
    .bind(version)
    .execute(db.pool())
    .await
    .expect("delete conflicting version");
    sqlx::query(
        r#"
        DELETE FROM skill_artifacts
        WHERE sha256 = ?
          AND NOT EXISTS (SELECT 1 FROM skill_versions WHERE sha256 = ?)
        "#,
    )
    .bind(&existing)
    .bind(&existing)
    .execute(db.pool())
    .await
    .expect("delete unused artifact");
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

async fn upload(app: &axum::Router, bytes: Vec<u8>) -> Value {
    let (status, body) = exchange(
        app,
        Request::builder()
            .method("POST")
            .uri("/v1/skills")
            .header("content-type", "application/vnd.novbot.skill")
            .body(Body::from(bytes))
            .unwrap(),
    )
    .await;
    assert!(
        status == StatusCode::CREATED || status == StatusCode::OK,
        "{} {}",
        status,
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body).expect("upload json")
}

async fn put_spec(app: &axum::Router, node_id: &str) {
    let body = json!({
        "specs": [{
            "id": SPEC_ID,
            "kind": "skill",
            "params": {"skill": SKILL, "version": "^1.0"}
        }],
        "schedules": []
    });
    let (status, bytes) = exchange(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/nodes/{node_id}/config"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

async fn post_json(app: &axum::Router, uri: &str, body: &Value) -> Value {
    let (status, value) = exchange_json(app, "POST", uri, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    value
}

async fn exchange_json(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let (status, bytes) = exchange(
        app,
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap(),
    )
    .await;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
    };
    (status, value)
}

async fn exchange(app: &axum::Router, request: Request<Body>) -> (StatusCode, axum::body::Bytes) {
    let response = app.clone().oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), RESP_LIMIT)
        .await
        .expect("body");
    (status, bytes)
}

fn outcome_of<'a>(body: &'a Value, node_id: &str) -> &'a str {
    node_of(body, node_id)["outcome"].as_str().unwrap_or("")
}

fn generation_in(body: &Value, node_id: &str) -> i64 {
    node_of(body, node_id)["generation"].as_i64().unwrap_or(-1)
}

fn node_of<'a>(body: &'a Value, node_id: &str) -> &'a Value {
    body["per_node"]
        .as_array()
        .expect("per_node")
        .iter()
        .find(|item| item["node_id"] == node_id)
        .unwrap_or_else(|| panic!("missing {node_id} in {body}"))
}

fn node_ids_of(body: &Value) -> Vec<String> {
    body["per_node"]
        .as_array()
        .expect("per_node")
        .iter()
        .map(|item| item["node_id"].as_str().unwrap_or("").to_string())
        .collect()
}

async fn assert_installed_version(app: &axum::Router, node_id: &str, version: &str) {
    assert_node_state(app, node_id, version, "installed").await;
}

async fn assert_node_state(app: &axum::Router, node_id: &str, version: &str, state: &str) {
    let body = get_json(app, &format!("/v1/nodes/{node_id}/skills")).await;
    let item = hub_item(&body).unwrap_or_else(|| panic!("no hub skill on {node_id}: {body}"));
    assert_eq!(item["state"], state, "{item}");
    assert_eq!(item["desired_version"], version, "{item}");
    assert_eq!(item["actual_version"], version, "{item}");
}

async fn assert_skill_absent(app: &axum::Router, node_id: &str) {
    let body = get_json(app, &format!("/v1/nodes/{node_id}/skills")).await;
    assert!(
        hub_item(&body).is_none(),
        "{node_id} still lists {SKILL}: {body}"
    );
}

fn hub_item(body: &Value) -> Option<&Value> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == SKILL && item["source"] == "hub")
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let (status, bytes) = exchange(
        app,
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("json")
}

async fn recv(inbound: &mut tonic::Streaming<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(30), inbound.message())
        .await
        .expect("timeout")
        .expect("stream")
        .expect("message")
}

async fn recv_desired(
    inbound: &mut tonic::Streaming<ServerMessage>,
) -> novbot_proto::DesiredSkills {
    let msg = recv(inbound).await;
    match msg.body {
        Some(server_message::Body::DesiredSkills(desired)) => {
            assert!(desired
                .skills
                .iter()
                .all(|skill| !skill.fetch_ticket.is_empty()));
            desired
        }
        other => panic!("expected DesiredSkills, got {other:?}"),
    }
}

async fn recv_dispatch(
    inbound: &mut tonic::Streaming<ServerMessage>,
    spec_id: &str,
    run_id: &str,
) -> Dispatch {
    let msg = recv(inbound).await;
    match msg.body {
        Some(server_message::Body::Dispatch(dispatch)) => {
            assert_eq!(dispatch.spec_id, spec_id);
            assert_eq!(dispatch.run_id, run_id);
            dispatch
        }
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

async fn recv_dispatch_then_ack(
    inbound: &mut tonic::Streaming<ServerMessage>,
    spec_id: &str,
    run_id: &str,
) -> Dispatch {
    let dispatch = recv_dispatch(inbound, spec_id, run_id).await;
    expect_ack_inbound(inbound).await;
    dispatch
}

async fn expect_ack(node: &mut Node) {
    expect_ack_inbound(node.inbound()).await;
}

async fn expect_ack_inbound(inbound: &mut tonic::Streaming<ServerMessage>) {
    let msg = recv(inbound).await;
    match msg.body {
        Some(server_message::Body::Ack(ack)) => assert!(ack.ok, "{ack:?}"),
        Some(server_message::Body::Dispatch(dispatch)) => {
            panic!("dispatch arrived where an ack was expected: {dispatch:?}")
        }
        other => panic!("expected Ack, got {other:?}"),
    }
}

async fn assert_quiet(inbound: &mut tonic::Streaming<ServerMessage>, what: &str) {
    match tokio::time::timeout(QUIET, inbound.message()).await {
        Err(_) => {}
        Ok(Ok(None)) => panic!("{what}: stream closed"),
        Ok(Ok(Some(msg))) => panic!("{what}: unexpected {msg:?}"),
        Ok(Err(err)) => panic!("{what}: {err}"),
    }
}

async fn send_report(tx: &mpsc::Sender<ClientMessage>, node_id: &str, snap: &SkillSnapshot) {
    tx.send(ClientMessage {
        request_id: format!("report-{node_id}-{}", snap.applied_generation),
        body: Some(client_message::Body::SkillState(snap.to_report(node_id))),
    })
    .await
    .expect("report");
}

async fn generation_of(db: &Db, node_id: &str) -> i64 {
    let row = sqlx::query("SELECT generation FROM node_skill_sets WHERE node_id = ?")
        .bind(node_id)
        .fetch_optional(db.pool())
        .await
        .expect("generation");
    match row {
        Some(row) => row.try_get("generation").unwrap(),
        None => 0,
    }
}

async fn desired_of(db: &Db, node_id: &str) -> Option<(String, String, String)> {
    let row = sqlx::query(
        "SELECT version, sha256, operation_id FROM node_skills_desired WHERE node_id = ? AND skill_name = ?",
    )
    .bind(node_id)
    .bind(SKILL)
    .fetch_optional(db.pool())
    .await
    .expect("desired");
    row.map(|row| {
        let version: String = row.try_get("version").unwrap();
        let sha: String = row.try_get("sha256").unwrap();
        let operation_id: Option<String> = row.try_get("operation_id").unwrap();
        (
            version,
            sha.trim().to_ascii_lowercase(),
            operation_id.unwrap_or_default(),
        )
    })
}

async fn desired_version(db: &Db, node_id: &str) -> Option<String> {
    desired_of(db, node_id).await.map(|(version, _, _)| version)
}

async fn desired_sha(db: &Db, node_id: &str) -> Option<String> {
    desired_of(db, node_id).await.map(|(_, sha, _)| sha)
}

async fn desired_op(db: &Db, node_id: &str) -> Option<String> {
    desired_of(db, node_id).await.map(|(_, _, op)| op)
}

async fn desired_map(db: &Db, node_id: &str) -> BTreeMap<String, (String, String)> {
    let rows = sqlx::query(
        "SELECT skill_name, version, sha256 FROM node_skills_desired WHERE node_id = ? ORDER BY skill_name",
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await
    .expect("desired map");
    rows_to_map(rows)
}

async fn actual_map(db: &Db, node_id: &str) -> BTreeMap<String, (String, String)> {
    let rows = sqlx::query(
        "SELECT skill_name, version, sha256 FROM node_skills_actual WHERE node_id = ? AND state = 'installed' ORDER BY skill_name",
    )
    .bind(node_id)
    .fetch_all(db.pool())
    .await
    .expect("actual map");
    rows_to_map(rows)
}

fn rows_to_map(rows: Vec<sqlx::mysql::MySqlRow>) -> BTreeMap<String, (String, String)> {
    let mut map = BTreeMap::new();
    for row in rows {
        let name: String = row.try_get("skill_name").unwrap();
        let version: String = row.try_get("version").unwrap();
        let sha: String = row.try_get("sha256").unwrap();
        map.insert(name, (version, sha.trim().to_ascii_lowercase()));
    }
    map
}

async fn operation_hash(db: &Db, id: &str) -> String {
    let row = sqlx::query("SELECT capabilities_sha256 FROM skill_operations WHERE id = ?")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .expect("operation");
    let value: Option<String> = row.try_get("capabilities_sha256").unwrap();
    value.unwrap_or_default().trim().to_ascii_lowercase()
}

async fn count_where(db: &Db, sql: &str, binds: &[&str]) -> i64 {
    let mut query = sqlx::query(sql);
    for bind in binds {
        query = query.bind(*bind);
    }
    query
        .fetch_one(db.pool())
        .await
        .expect("count")
        .try_get("n")
        .unwrap()
}

async fn pending_count(db: &Db, node_id: &str) -> i64 {
    count_where(
        db,
        "SELECT COUNT(*) AS n FROM pending_dispatches WHERE node_id = ?",
        &[node_id],
    )
    .await
}

async fn audit_max_id(db: &Db) -> i64 {
    sqlx::query("SELECT COALESCE(MAX(id), 0) AS n FROM audit_events")
        .fetch_one(db.pool())
        .await
        .expect("audit mark")
        .try_get("n")
        .unwrap()
}

async fn assert_no_arm2_audit(db: &Db, after_id: i64) {
    let rows = sqlx::query(
        "SELECT node_id, action, detail_json FROM audit_events WHERE id > ? AND target_id = ?",
    )
    .bind(after_id)
    .bind(SKILL)
    .fetch_all(db.pool())
    .await
    .expect("audit");
    for row in rows {
        let node_id: Option<String> = row.try_get("node_id").unwrap();
        let action: String = row.try_get("action").unwrap();
        let detail: String = row.try_get("detail_json").unwrap();
        assert_ne!(node_id.as_deref(), Some(ARM2), "{action} {detail}");
        assert!(
            !detail.contains(ARM2),
            "orb-arm-2 appeared in {action}: {detail}"
        );
    }
}

async fn flip_artifact_byte(db: &Db, sha256: &str) {
    let mut data: Vec<u8> = sqlx::query("SELECT data FROM skill_artifacts WHERE sha256 = ?")
        .bind(sha256)
        .fetch_one(db.pool())
        .await
        .expect("artifact")
        .try_get("data")
        .expect("data");
    assert!(!data.is_empty());
    data[0] ^= 0x01;
    sqlx::query("UPDATE skill_artifacts SET data = ? WHERE sha256 = ?")
        .bind(data)
        .bind(sha256)
        .execute(db.pool())
        .await
        .expect("flip");
}

fn retarget_version(bytes: &[u8], from: &str, to: &str) -> Vec<u8> {
    let mut files = unpack_nbskill(bytes);
    let toml = String::from_utf8(files.get("skill.toml").expect("skill.toml").clone())
        .expect("skill.toml utf-8");
    let needle = format!("version = \"{from}\"");
    assert!(toml.contains(&needle), "packed manifest has no {needle}");
    let rewritten = toml.replacen(&needle, &format!("version = \"{to}\""), 1);
    assert!(!rewritten.contains(&needle));
    files.insert("skill.toml".into(), rewritten.into_bytes());
    pack_nbskill(&files)
}

fn unpack_nbskill(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Cursor::new(bytes)));
    let mut files = BTreeMap::new();
    for entry in archive.entries().expect("archive") {
        let mut entry = entry.expect("archive entry");
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let raw = entry.path().expect("archive path");
        let path = raw.to_string_lossy();
        let path = path
            .trim_end_matches('/')
            .trim_start_matches("./")
            .to_string();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).expect("read archive entry");
        files.insert(path, data);
    }
    files
}

fn pack_nbskill(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut entries: Vec<(&str, Option<&[u8]>)> = Vec::new();
    if files.keys().any(|path| path.starts_with("schema/")) {
        entries.push(("schema", None));
    }
    for (path, data) in files {
        entries.push((path.as_str(), Some(data.as_slice())));
    }
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        for (path, data) in &entries {
            let mut header = tar::Header::new_gnu();
            match data {
                None => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_mode(0o755);
                    header.set_size(0);
                }
                Some(data) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_mode(0o644);
                    header.set_size(data.len() as u64);
                }
            }
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            let body: &[u8] = data.unwrap_or(&[]);
            builder
                .append_data(&mut header, path, body)
                .expect("tar entry");
        }
        builder.finish().expect("tar finish");
    }
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).expect("gzip");
    encoder.finish().expect("gzip finish")
}
