// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! D10.7, one node: a corrupted catalog blob fails install and the previous
//! version stays the active skill. Not the three-node sign-off.

use crate::db::{self, Db};
use crate::grpc::ControlSvc;
use crate::http::{router, AppState};
use crate::hub::Hub;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use novbot_node::skills::{DesiredSet, GrpcArtifactSource, InstallLimits, SkillHost, ABI, CATALOG};
use novbot_proto::control_client::ControlClient;
use novbot_proto::control_server::ControlServer;
use novbot_proto::{
    client_message, server_message, ClientMessage, NodeReady, PullConfigRequest, RegisterRequest,
    ServerMessage,
};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tower::ServiceExt;

const RESP_LIMIT: usize = 8 * 1024 * 1024;

#[tokio::test]
async fn corrupted_upgrade_keeps_previous_then_rollback_deletes_it() {
    let Some(db) = db::connect_test_db().await else {
        return;
    };
    let node_id = format!("d10-{}", uuid::Uuid::new_v4());
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

    let app = router(AppState {
        db: db.clone(),
        hub: hub.clone(),
        api_token: None,
        license_env: None,
    });
    let v1 = fixture("os-release-check-1.0.0.nbskill");
    let v2 = fixture("os-release-check-1.1.1.nbskill");
    let posted_v1 = upload(&app, v1).await;
    let posted_v2 = upload(&app, v2).await;
    let sha_v1 = posted_v1["sha256"].as_str().unwrap().to_string();
    let sha_v2 = posted_v2["sha256"].as_str().unwrap().to_string();
    let cap_v1 = posted_v1["capabilities_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    let cap_v2 = posted_v2["capabilities_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(sha_v1, sha_v2);

    db.upsert_node(&node_id, "d10-host", "0.1.0", &HashMap::new())
        .await
        .expect("upsert node");

    let endpoint = format!("http://{addr}");
    let mut client = ControlClient::connect(endpoint.clone())
        .await
        .expect("client");
    let (tx, rx) = mpsc::channel(32);
    let mut inbound = client
        .session(ReceiverStream::new(rx))
        .await
        .expect("session")
        .into_inner();
    tx.send(register_msg(&node_id)).await.expect("register");
    let registered = recv(&mut inbound).await;
    assert!(
        matches!(registered.body, Some(server_message::Body::Register(ref resp)) if resp.accepted),
        "{registered:?}"
    );
    tx.send(ClientMessage {
        request_id: "pull-config".into(),
        body: Some(client_message::Body::PullConfig(PullConfigRequest {
            node_id: node_id.clone(),
            known_generation: 0,
        })),
    })
    .await
    .expect("pull config");
    let pulled = recv(&mut inbound).await;
    let config_generation = match pulled.body {
        Some(server_message::Body::PullConfig(resp)) => resp.config_generation,
        other => panic!("expected PullConfig, got {other:?}"),
    };
    tx.send(ClientMessage {
        request_id: "ready".into(),
        body: Some(client_message::Body::NodeReady(NodeReady {
            node_id: node_id.clone(),
            config_generation,
            skill_set_generation: 0,
        })),
    })
    .await
    .expect("ready");
    let _ = recv(&mut inbound).await;
    assert!(hub.is_connected(&node_id));

    let data = tempfile::tempdir().expect("data dir");
    let source = Arc::new(GrpcArtifactSource::new(endpoint, node_id.clone()));
    let host = SkillHost::open(data.path(), source.clone(), fast_limits())
        .await
        .expect("skill host");

    let install_v1 = post_json(
        &app,
        "/v1/skills/os-release-check/install",
        &serde_json::json!({
            "version": "1.0.0",
            "node_ids": [node_id],
            "accepted_capabilities_sha256": cap_v1,
        }),
    )
    .await;
    assert_eq!(
        install_v1["per_node"][0]["outcome"], "pending",
        "{install_v1}"
    );
    let desired = recv_desired(&mut inbound).await;
    let snap = host.reconcile(DesiredSet::from_proto(&desired)).await;
    assert_eq!(snap.skills[0].state, "installed", "{snap:?}");
    assert_eq!(snap.skills[0].version, "1.0.0");
    send_report(&tx, &node_id, &snap).await;
    let _ = recv(&mut inbound).await;
    let row = actual_row(&db, &node_id).await;
    assert_eq!(row.0, "1.0.0");
    assert_eq!(row.1, "installed");
    assert_eq!(
        host.active_version("os-release-check").as_deref(),
        Some("1.0.0")
    );

    // A closed port: a second download would fail the reconcile. Same generation must not fetch.
    let restarted = SkillHost::open(
        data.path(),
        Arc::new(GrpcArtifactSource::new(
            "http://127.0.0.1:1",
            node_id.clone(),
        )),
        fast_limits(),
    )
    .await
    .expect("restart");
    let again = restarted.reconcile(DesiredSet::from_proto(&desired)).await;
    assert_eq!(again.applied_generation, snap.applied_generation);
    assert_eq!(again.skills[0].sha256, snap.skills[0].sha256);
    assert_eq!(again.skills[0].state, "installed");
    drop(restarted);
    let restarted = SkillHost::open(data.path(), source, fast_limits())
        .await
        .expect("skill host after restart");

    flip_artifact_byte(&db, &sha_v2).await;
    let install_v2 = post_json(
        &app,
        "/v1/skills/os-release-check/install",
        &serde_json::json!({
            "version": "1.1.1",
            "node_ids": [node_id],
            "accepted_capabilities_sha256": cap_v2,
        }),
    )
    .await;
    assert_eq!(
        install_v2["per_node"][0]["outcome"], "pending",
        "{install_v2}"
    );
    let desired_bad = recv_desired(&mut inbound).await;
    let failed = restarted
        .reconcile(DesiredSet::from_proto(&desired_bad))
        .await;
    assert_eq!(failed.skills[0].state, "failed", "{failed:?}");
    assert_eq!(failed.skills[0].reason, "hash_mismatch");
    assert_eq!(failed.skills[0].version, "1.1.1");
    assert_eq!(
        restarted.active_version("os-release-check").as_deref(),
        Some("1.0.0")
    );
    send_report(&tx, &node_id, &failed).await;
    let _ = recv(&mut inbound).await;
    let row = actual_row(&db, &node_id).await;
    assert_eq!(row.0, "1.1.1");
    assert_eq!(row.1, "failed");
    assert_eq!(row.2.as_deref(), Some("hash_mismatch"));

    let rollback = post_json(
        &app,
        "/v1/skills/os-release-check/rollback",
        &serde_json::json!({
            "node_ids": [node_id],
            "to_version": "1.0.0",
        }),
    )
    .await;
    assert_eq!(rollback["per_node"][0]["outcome"], "pending", "{rollback}");
    let desired_back = recv_desired(&mut inbound).await;
    let restored = restarted
        .reconcile(DesiredSet::from_proto(&desired_back))
        .await;
    assert_eq!(restored.skills[0].state, "installed");
    assert_eq!(restored.skills[0].version, "1.0.0");
    assert_eq!(
        restarted.active_version("os-release-check").as_deref(),
        Some("1.0.0")
    );
    send_report(&tx, &node_id, &restored).await;
    let _ = recv(&mut inbound).await;
    let row = actual_row(&db, &node_id).await;
    assert_eq!(row.0, "1.0.0");
    assert_eq!(row.1, "installed");

    let deleted = oneshot(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/v1/skills/os-release-check/versions/1.1.1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        deleted.0,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&deleted.1)
    );
    let version_left: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM skill_versions WHERE sha256 = ?")
            .bind(&sha_v2)
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
    assert_eq!(version_left, 0);
    let artifact_left: i64 =
        sqlx::query("SELECT COUNT(*) AS n FROM skill_artifacts WHERE sha256 = ?")
            .bind(&sha_v2)
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
    assert_eq!(artifact_left, 0);
    let kept: i64 = sqlx::query("SELECT COUNT(*) AS n FROM skill_versions WHERE sha256 = ?")
        .bind(&sha_v1)
        .fetch_one(db.pool())
        .await
        .unwrap()
        .try_get("n")
        .unwrap();
    assert_eq!(kept, 1);
}

fn fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn fast_limits() -> InstallLimits {
    InstallLimits {
        max_attempts: 2,
        max_elapsed: Duration::from_secs(30),
        min_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        ready_bound: Duration::from_millis(20),
        sleeper: Arc::new(|_| Box::pin(async {})),
    }
}

fn register_msg(node_id: &str) -> ClientMessage {
    ClientMessage {
        request_id: "reg".into(),
        body: Some(client_message::Body::Register(RegisterRequest {
            node_id: node_id.to_string(),
            hostname: "d10-host".into(),
            version: "0.1.0".into(),
            bootstrap_token: String::new(),
            labels: HashMap::new(),
            skill_abi: ABI.into(),
            capabilities_supported: CATALOG.iter().map(|cap| (*cap).to_string()).collect(),
            skill_set_generation: 0,
            installed: Vec::new(),
            supports_ready: true,
        })),
    }
}

async fn upload(app: &axum::Router, bytes: Vec<u8>) -> serde_json::Value {
    let (status, body) = oneshot(
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

async fn post_json(app: &axum::Router, uri: &str, body: &serde_json::Value) -> serde_json::Value {
    let (status, bytes) = oneshot(
        app,
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("json")
}

async fn oneshot(app: &axum::Router, request: Request<Body>) -> (StatusCode, axum::body::Bytes) {
    let response = app.clone().oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), RESP_LIMIT)
        .await
        .expect("body");
    (status, bytes)
}

async fn recv(inbound: &mut tonic::Streaming<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(10), inbound.message())
        .await
        .expect("timeout")
        .expect("stream")
        .expect("message")
}

async fn recv_desired(
    inbound: &mut tonic::Streaming<ServerMessage>,
) -> novbot_proto::DesiredSkills {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            panic!("timed out waiting for DesiredSkills");
        }
        let msg = tokio::time::timeout(left, inbound.message())
            .await
            .expect("timeout")
            .expect("stream")
            .expect("message");
        if let Some(server_message::Body::DesiredSkills(desired)) = msg.body {
            assert!(
                desired
                    .skills
                    .iter()
                    .all(|skill| !skill.fetch_ticket.is_empty()),
                "missing fetch ticket"
            );
            return desired;
        }
    }
}

async fn send_report(
    tx: &mpsc::Sender<ClientMessage>,
    node_id: &str,
    snap: &novbot_node::skills::SkillSnapshot,
) {
    tx.send(ClientMessage {
        request_id: uuid::Uuid::new_v4().to_string(),
        body: Some(client_message::Body::SkillState(snap.to_report(node_id))),
    })
    .await
    .expect("report");
}

async fn actual_row(db: &Db, node_id: &str) -> (String, String, Option<String>) {
    let row = sqlx::query(
        "SELECT version, state, reason FROM node_skills_actual WHERE node_id = ? AND skill_name = 'os-release-check'",
    )
    .bind(node_id)
    .fetch_one(db.pool())
    .await
    .expect("actual row");
    (
        row.try_get("version").unwrap(),
        row.try_get("state").unwrap(),
        row.try_get("reason").unwrap(),
    )
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
