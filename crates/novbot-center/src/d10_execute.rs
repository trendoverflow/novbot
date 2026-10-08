// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! D10.4 and D10.8 on one in-process node. Not the three-node lifecycle.

#[path = "../../novbot-node/src/hub_pack.rs"]
mod hub_pack;

use crate::db;
use crate::grpc::ControlSvc;
use crate::http::{router, AppState};
use crate::hub::Hub;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use novbot_core::{ProbePolicy, Spec, SpecKind};
use novbot_node::skills::{DesiredSet, GrpcArtifactSource, InstallLimits, SkillHost, ABI, CATALOG};
use novbot_node::{apply_dispatch_overlay, execute_spec};
use novbot_proto::control_client::ControlClient;
use novbot_proto::control_server::ControlServer;
use novbot_proto::{
    client_message, server_message, ClientMessage, Dispatch, NodeReady, PullConfigRequest,
    RegisterRequest, ReportResultRequest, ServerMessage,
};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tower::ServiceExt;

const RESP_LIMIT: usize = 8 * 1024 * 1024;

#[tokio::test]
async fn installed_skill_dispatch_is_ok_and_audits_denials() {
    if hub_pack::relaunch_if_os_release_missing(
        "d10_execute::installed_skill_dispatch_is_ok_and_audits_denials",
    ) {
        return;
    }
    let Some(db) = db::connect_test_db().await else {
        return;
    };
    let pid = std::process::id();
    let node_id = format!("d10-exec-{}", uuid::Uuid::new_v4());
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

    let os_bytes = hub_pack::os_release_package();
    let cap_bytes = hub_pack::cap_violation_package();
    let policy_bytes = hub_pack::cap_policy_package();
    let posted_os = upload(&app, os_bytes).await;
    let posted_cap = upload(&app, cap_bytes).await;
    let posted_policy = upload(&app, policy_bytes).await;

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
    let _ = recv(&mut inbound).await;
    tx.send(ClientMessage {
        request_id: "ready".into(),
        body: Some(client_message::Body::NodeReady(NodeReady {
            node_id: node_id.clone(),
            config_generation: 0,
            skill_set_generation: 0,
        })),
    })
    .await
    .expect("ready");
    let _ = recv(&mut inbound).await;
    assert!(hub.is_connected(&node_id));

    let specs = json!([
        {"id": "os-release", "kind": "skill", "params": {"skill": "os-release-check", "version": "^1.0"}},
        {"id": "cap-violation", "kind": "skill", "params": {"skill": "cap-violation-test", "version": "=1.0.0"}},
        {"id": "cap-policy", "kind": "skill", "params": {"skill": "cap-violation-policy", "version": "=1.0.0"}}
    ]);
    let (status, bytes) = oneshot(
        &app,
        json_request(
            "PUT",
            &format!("/v1/nodes/{node_id}/config"),
            &json!({"specs": specs}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    install(&app, "os-release-check", "1.0.0", &posted_os, &node_id).await;
    install(&app, "cap-violation-test", "1.0.0", &posted_cap, &node_id).await;
    install(
        &app,
        "cap-violation-policy",
        "1.0.0",
        &posted_policy,
        &node_id,
    )
    .await;
    let desired = recv_desired_with(
        &mut inbound,
        &[
            "os-release-check",
            "cap-violation-test",
            "cap-violation-policy",
        ],
    )
    .await;

    let data = tempfile::tempdir().expect("data dir");
    let host = SkillHost::open(
        data.path(),
        Arc::new(GrpcArtifactSource::new(endpoint, node_id.clone())),
        fast_limits(),
    )
    .await
    .expect("skill host");
    let snap = host.reconcile(DesiredSet::from_proto(&desired)).await;
    for skill in &snap.skills {
        assert_eq!(skill.state, "installed", "{snap:?}");
    }
    assert_eq!(
        host.active_version("os-release-check").as_deref(),
        Some("1.0.0")
    );
    assert_eq!(host.wasm_run_entries(), 0, "install must not execute");

    let release = hub_pack::host_os_release();
    let want_id = hub_pack::os_field(&release, "ID");
    let want_version = hub_pack::os_field(&release, "VERSION_ID");
    let os = dispatch_run(
        &app,
        &mut inbound,
        &tx,
        &host,
        data.path(),
        &node_id,
        "os-release",
        None,
    )
    .await;
    assert_eq!(os.0, "ok", "{os:?}");
    assert_eq!(os.1["skill"]["version"], "1.0.0");
    assert_eq!(os.1["ID"], want_id);
    assert_eq!(os.1["VERSION_ID"], want_version);
    assert_eq!(os.1["hostname"], hub_pack::host_name());
    assert_eq!(std::process::id(), pid);

    let (status, bytes) = oneshot(
        &app,
        json_request(
            "POST",
            &format!("/v1/nodes/{node_id}/dispatch"),
            &json!({"spec_id": "no-such-spec"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let missing: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(missing["code"], "spec_unknown");
    let queued: i64 = sqlx::query(
        "SELECT COUNT(*) AS n FROM pending_dispatches WHERE node_id = ? AND spec_id = 'no-such-spec'",
    )
    .bind(&node_id)
    .fetch_one(db.pool())
    .await
    .unwrap()
    .try_get("n")
    .unwrap();
    assert_eq!(queued, 0);

    let shadow = dispatch_run(
        &app,
        &mut inbound,
        &tx,
        &host,
        data.path(),
        &node_id,
        "cap-violation",
        None,
    )
    .await;
    assert_denial(&shadow, "fs.read", "out_of_scope");
    assert_ne!(shadow.1["denials"][0]["reason"], "policy_denied");

    let undeclared = dispatch_run(
        &app,
        &mut inbound,
        &tx,
        &host,
        data.path(),
        &node_id,
        "cap-violation",
        Some(json!({"arguments": {"mode": "undeclared"}})),
    )
    .await;
    assert_denial(
        &undeclared,
        "net.listening_ports.read",
        "undeclared_capability",
    );

    let policy = dispatch_run(
        &app,
        &mut inbound,
        &tx,
        &host,
        data.path(),
        &node_id,
        "cap-policy",
        None,
    )
    .await;
    assert_denial(&policy, "fs.read", "policy_denied");

    let shadow_run = shadow.2.clone();
    let shadow_payload = serde_json::to_string(&shadow.1).unwrap();
    send_result(
        &tx,
        &node_id,
        &shadow_run,
        "cap-violation",
        "error",
        &shadow_payload,
    )
    .await;
    assert_report_accepted(&mut inbound).await;

    let again = dispatch_run(
        &app,
        &mut inbound,
        &tx,
        &host,
        data.path(),
        &node_id,
        "os-release",
        None,
    )
    .await;
    assert_eq!(again.0, "ok", "{again:?}");
    assert_eq!(again.1["ID"], want_id);
    assert_eq!(std::process::id(), pid);

    let stored = get_json(&app, &format!("/v1/results?node_id={node_id}&limit=20")).await;
    let rows = stored.as_array().expect("results");
    let os_row = rows
        .iter()
        .find(|row| row["run_id"] == os.2 && row["spec_id"] == "os-release")
        .expect("stored os-release result");
    assert_eq!(os_row["status"], "ok");
    assert_eq!(os_row["payload"]["skill"]["version"], "1.0.0");
    assert_eq!(os_row["payload"]["ID"], want_id);

    let audit = get_json(
        &app,
        &format!("/v1/audit-events?action=skill.capability_denied&node_id={node_id}&limit=20"),
    )
    .await;
    let events = audit["items"].as_array().expect("audit items");
    assert_eq!(events.len(), 3, "{audit}");
    let mut runs = Vec::new();
    for event in events {
        assert_eq!(event["actor_type"], "node");
        assert_eq!(event["actor_id"], node_id);
        assert_eq!(event["target_type"], "skill");
        let detail = &event["detail"];
        assert!(detail["run_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(detail["spec_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert!(detail["denials"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert!(detail["skill"]["name"]
            .as_str()
            .is_some_and(|name| !name.is_empty()));
        assert!(detail["skill"]["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()));
        assert_eq!(detail["skill"]["sha256"].as_str().unwrap().len(), 64);
        assert!(detail.get("partial").is_none());
        runs.push(detail["run_id"].as_str().unwrap().to_string());
    }
    assert_eq!(runs.iter().filter(|id| *id == &shadow_run).count(), 1);
    assert_eq!(std::process::id(), pid);
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_run(
    app: &axum::Router,
    inbound: &mut tonic::Streaming<ServerMessage>,
    tx: &mpsc::Sender<ClientMessage>,
    host: &SkillHost,
    data: &std::path::Path,
    node_id: &str,
    spec_id: &str,
    params: Option<Value>,
) -> (String, Value, String) {
    let body = json!({"spec_id": spec_id, "params": params.unwrap_or_else(|| json!({}))});
    let (status, bytes) = oneshot(
        app,
        json_request("POST", &format!("/v1/nodes/{node_id}/dispatch"), &body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let accepted: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(accepted["delivered"], "live", "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_string();
    let dispatch = recv_dispatch(inbound, spec_id, &run_id).await;
    let mut spec = spec_by_id(spec_id);
    apply_dispatch_overlay(&mut spec, &dispatch.params_json);
    let (status, payload) = execute_spec(&spec, &ProbePolicy::default(), host, data, &run_id).await;
    let payload_json = serde_json::to_string(&payload).unwrap();
    send_result(tx, node_id, &run_id, spec_id, &status, &payload_json).await;
    assert_report_accepted(inbound).await;
    (status, payload, run_id)
}

fn spec_by_id(id: &str) -> Spec {
    let (skill, version) = match id {
        "os-release" => ("os-release-check", "^1.0"),
        "cap-violation" => ("cap-violation-test", "=1.0.0"),
        "cap-policy" => ("cap-violation-policy", "=1.0.0"),
        other => panic!("unknown spec {other}"),
    };
    Spec {
        id: id.to_string(),
        kind: SpecKind::Skill,
        params: json!({"skill": skill, "version": version}),
        threshold: None,
    }
}

fn assert_denial(outcome: &(String, Value, String), capability: &str, reason: &str) {
    assert_eq!(outcome.0, "error", "{outcome:?}");
    assert_eq!(
        outcome.1["error"]["code"], "capability_denied",
        "{outcome:?}"
    );
    let denial = &outcome.1["denials"][0];
    assert_eq!(denial["capability"], capability, "{outcome:?}");
    assert_eq!(denial["reason"], reason, "{outcome:?}");
    if capability == "fs.read" {
        let target = denial["target"].as_str().unwrap_or("");
        assert!(
            target == "/etc/shadow" || target == "/private/etc/shadow",
            "{outcome:?}"
        );
        let text = outcome.1.to_string();
        if let Ok(bytes) =
            std::fs::read("/etc/shadow").or_else(|_| std::fs::read("/private/etc/shadow"))
        {
            if bytes.len() >= 8 {
                let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(32)]);
                let snippet = snippet.trim();
                if snippet.len() >= 8 {
                    assert!(!text.contains(snippet), "shadow bytes leaked");
                }
            }
        }
    }
}

async fn install(app: &axum::Router, name: &str, version: &str, posted: &Value, node_id: &str) {
    let (status, bytes) = oneshot(
        app,
        json_request(
            "POST",
            &format!("/v1/skills/{name}/install"),
            &json!({
                "version": version,
                "node_ids": [node_id],
                "accepted_capabilities_sha256": posted["capabilities_sha256"],
            }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

async fn send_result(
    tx: &mpsc::Sender<ClientMessage>,
    node_id: &str,
    run_id: &str,
    spec_id: &str,
    status: &str,
    payload_json: &str,
) {
    tx.send(ClientMessage {
        request_id: uuid::Uuid::new_v4().to_string(),
        body: Some(client_message::Body::ReportResult(ReportResultRequest {
            node_id: node_id.to_string(),
            run_id: run_id.to_string(),
            spec_id: spec_id.to_string(),
            status: status.to_string(),
            payload_json: payload_json.to_string(),
            observed_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        })),
    })
    .await
    .expect("report result");
}

async fn assert_report_accepted(inbound: &mut tonic::Streaming<ServerMessage>) {
    let msg = recv(inbound).await;
    match msg.body {
        Some(server_message::Body::ReportResult(resp)) => assert!(resp.accepted, "{resp:?}"),
        other => panic!("expected ReportResult, got {other:?}"),
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

async fn upload(app: &axum::Router, bytes: Vec<u8>) -> Value {
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

fn json_request(method: &str, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn get_json(app: &axum::Router, uri: &str) -> Value {
    let (status, bytes) = oneshot(
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

async fn oneshot(app: &axum::Router, request: Request<Body>) -> (StatusCode, axum::body::Bytes) {
    let response = app.clone().oneshot(request).await.expect("router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), RESP_LIMIT)
        .await
        .expect("body");
    (status, bytes)
}

async fn recv(inbound: &mut tonic::Streaming<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(30), inbound.message())
        .await
        .expect("timeout")
        .expect("stream")
        .expect("message")
}

async fn recv_dispatch(
    inbound: &mut tonic::Streaming<ServerMessage>,
    spec_id: &str,
    run_id: &str,
) -> Dispatch {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for Dispatch {spec_id}");
        let msg = tokio::time::timeout(left, inbound.message())
            .await
            .expect("timeout")
            .expect("stream")
            .expect("message");
        if let Some(server_message::Body::Dispatch(dispatch)) = msg.body {
            if dispatch.spec_id == spec_id && dispatch.run_id == run_id {
                return dispatch;
            }
        }
    }
}

async fn recv_desired_with(
    inbound: &mut tonic::Streaming<ServerMessage>,
    names: &[&str],
) -> novbot_proto::DesiredSkills {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "timed out waiting for desired skills {names:?}"
        );
        let msg = tokio::time::timeout(left, inbound.message())
            .await
            .expect("timeout")
            .expect("stream")
            .expect("message");
        if let Some(server_message::Body::DesiredSkills(desired)) = msg.body {
            if names
                .iter()
                .all(|name| desired.skills.iter().any(|skill| skill.name == *name))
            {
                return desired;
            }
        }
    }
}
