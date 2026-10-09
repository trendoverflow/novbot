// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! D10.21 on three in-process nodes.
//!
//! `orb-arm-1`, `orb-arm-2`, and `orb-arm-3` are rows plus `ControlClient`
//! sessions. This does not talk to OrbStack, does not deploy a demo, and does
//! not close D10.13.
//!
//! Hosts without `/proc` would return an `io` error from the live collectors.
//! The test installs a process-wide observe fixture so all five skills return
//! `ok`, and clears that fixture on drop. Built-in Spec ids `cpu`, `memory`,
//! and `disk-root` still run through `run_probe` on this process.

use crate::db::{self, Db};
use crate::grpc::ControlSvc;
use crate::http::{router, AppState};
use crate::hub::Hub;
use crate::hub_pack;
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use novbot_core::{run_probe, ProbePolicy, Spec, SpecKind};
use novbot_node::skills::{DesiredSet, GrpcArtifactSource, InstallLimits, SkillHost, ABI, CATALOG};
use novbot_node::{apply_dispatch_overlay, execute_spec, ObserveFixture};
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::transport::Channel;
use tower::ServiceExt;

const RESP_LIMIT: usize = 8 * 1024 * 1024;
const NODES: [&str; 3] = ["orb-arm-1", "orb-arm-2", "orb-arm-3"];
const SKILLS: [&str; 5] = [
    "cpu-usage",
    "memory-usage",
    "disk-usage",
    "process-top",
    "net-interface-traffic",
];
const SECRET: &str = "d10-21-cmdline-do-not-leak";

#[tokio::test]
#[cfg_attr(
    novbot_test_db_missing,
    ignore = "NOVBOT_TEST_DATABASE_URL is unset or empty"
)]
async fn three_node_observability_skills_install_and_run() {
    let db = db::connect_test_db()
        .await
        .expect("NOVBOT_TEST_DATABASE_URL");
    let suite = uuid::Uuid::new_v4().simple().to_string();
    let _clear_fixture = install_observe_fixture();

    sqlx::query("DELETE FROM nodes WHERE node_id IN (?, ?, ?)")
        .bind(NODES[0])
        .bind(NODES[1])
        .bind(NODES[2])
        .execute(db.pool())
        .await
        .expect("delete prior orb-arm rows");

    let center = start_center(db.clone()).await;
    let posted = publish_examples(&db, &center.app).await;
    let mut nodes = Vec::new();
    for id in NODES {
        let mut node = prepare_node(id, &center.endpoint).await;
        boot(&mut node, &center.endpoint, &center.hub, &suite).await;
        nodes.push(node);
    }
    for id in NODES {
        put_specs(&center.app, id).await;
    }
    for name in SKILLS {
        let installed = install_skill(&center.app, name, &posted[&name.to_string()], &suite).await;
        let mut targets = node_ids_of(&installed);
        targets.sort();
        assert_eq!(targets, NODES.map(str::to_string).to_vec(), "{installed}");
        for id in NODES {
            assert_eq!(outcome_of(&installed, id), "pending", "{name} {installed}");
        }
    }
    for node in &mut nodes {
        let desired = recv_full_desired(node.inbound()).await;
        let before = node.host.wasm_run_entries();
        let snap = node.host.reconcile(DesiredSet::from_proto(&desired)).await;
        assert_eq!(
            node.host.wasm_run_entries(),
            before,
            "reconcile must not run the skill"
        );
        let mut names: Vec<_> = snap.skills.iter().map(|skill| skill.name.clone()).collect();
        let mut expected = SKILLS.map(str::to_string).to_vec();
        names.sort();
        expected.sort();
        assert_eq!(names, expected, "{snap:?}");
        for skill in &snap.skills {
            assert_eq!(skill.state, "installed", "{snap:?}");
            assert_eq!(skill.version, "1.0.0", "{snap:?}");
        }
        send_report(node, &snap).await;
        assert_center_installed(&center.app, node.id).await;
    }
    for node in &mut nodes {
        for name in SKILLS {
            let (status, payload) = dispatch_run(&center.app, node, name).await;
            assert_skill_ok(node.id, name, &status, &payload);
        }
    }
    assert_builtins_still_run().await;
}

struct Center {
    app: axum::Router,
    endpoint: String,
    hub: Hub,
}

struct Node {
    id: &'static str,
    data: tempfile::TempDir,
    host: SkillHost,
    conn: Option<Conn>,
}

struct Conn {
    tx: mpsc::Sender<ClientMessage>,
    inbound: tonic::Streaming<ServerMessage>,
    _client: ControlClient<Channel>,
}

impl Node {
    fn inbound(&mut self) -> &mut tonic::Streaming<ServerMessage> {
        &mut self.conn.as_mut().expect("session").inbound
    }
}

struct ClearObserveFixture;

impl Drop for ClearObserveFixture {
    fn drop(&mut self) {
        novbot_node::set_observe_fixture(None);
    }
}

fn install_observe_fixture() -> ClearObserveFixture {
    novbot_node::set_observe_fixture(Some(ObserveFixture {
        metrics: Some(metrics_fixture()),
        processes: Some(processes_fixture()),
        interfaces: Some(interfaces_fixture()),
    }));
    ClearObserveFixture
}

fn metrics_fixture() -> String {
    json!({
        "cpu": {
            "usage_percent": 12.5,
            "load_avg_1m": 0.4,
            "load_avg_5m": 0.3,
            "load_avg_15m": 0.2
        },
        "memory": {
            "total_bytes": 8_589_934_592u64,
            "used_bytes": 1000,
            "available_bytes": 2000,
            "swap_total_bytes": 3000,
            "swap_used_bytes": 4
        },
        "disks": [{
            "mount": "/",
            "fstype": "ext4",
            "total_bytes": 5000,
            "used_bytes": 1500,
            "available_bytes": 3500,
            "inodes_total": 100,
            "inodes_used": 10,
            "inodes_available": 90
        }],
        "disk_io": []
    })
    .to_string()
}

fn processes_fixture() -> String {
    json!({
        "processes": [{
            "pid": 7,
            "ppid": 1,
            "name": "d10-init",
            "uid": 0,
            "user": "root",
            "state": "S",
            "cpu_ticks": 10,
            "cpu_percent": 1.25,
            "memory_bytes": 4096,
            "start_time_unix_ms": 1_700_000_000_000u64,
            "cmdline": SECRET,
            "command_line": SECRET,
            "argv": [SECRET]
        }]
    })
    .to_string()
}

fn interfaces_fixture() -> String {
    json!({
        "interfaces": [{
            "name": "d10-eth0",
            "addresses": ["192.0.2.21"],
            "operstate": "up",
            "up": true,
            "rx_bytes": 111,
            "rx_packets": 11,
            "rx_errors": 0,
            "tx_bytes": 222,
            "tx_packets": 22,
            "tx_errors": 0
        }]
    })
    .to_string()
}

async fn start_center(db: Db) -> Center {
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
    Center {
        app: router(AppState {
            db,
            hub: hub.clone(),
            api_token: None,
            license_env: None,
        }),
        endpoint: format!("http://{addr}"),
        hub,
    }
}

async fn publish_examples(db: &Db, app: &axum::Router) -> BTreeMap<String, String> {
    let mut posted = BTreeMap::new();
    for name in SKILLS {
        let bytes = hub_pack::example_package(name);
        replace_version_if_different(db, name, "1.0.0", &bytes).await;
        let body = upload(app, bytes).await;
        assert_eq!(body["name"], name, "{body}");
        assert_eq!(body["version"], "1.0.0", "{body}");
        let cap = body["capabilities_sha256"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        assert_eq!(cap.len(), 64, "{name} {body}");
        posted.insert(name.to_string(), cap);
    }
    posted
}

async fn prepare_node(id: &'static str, endpoint: &str) -> Node {
    let data = tempfile::tempdir().expect("data dir");
    let source = Arc::new(GrpcArtifactSource::new(endpoint, id));
    let host = SkillHost::open(data.path(), source, fast_limits())
        .await
        .expect("skill host");
    Node {
        id,
        data,
        host,
        conn: None,
    }
}

async fn boot(node: &mut Node, endpoint: &str, hub: &Hub, suite: &str) {
    node.conn = Some(open_session(endpoint).await);
    let installed = node.host.register_installed();
    let generation = node.host.applied_generation();
    let id = node.id;
    {
        let conn = node.conn.as_mut().expect("session");
        conn.tx
            .send(register_msg(id, suite, installed, generation))
            .await
            .expect("register");
        let msg = recv(&mut conn.inbound).await;
        match msg.body {
            Some(server_message::Body::Register(resp)) => assert!(resp.accepted, "{resp:?}"),
            other => panic!("expected Register, got {other:?}"),
        }
    }
    let known = node.host.applied_generation();
    let id = node.id.to_string();
    let config_generation = {
        let conn = node.conn.as_mut().expect("session");
        conn.tx
            .send(ClientMessage {
                request_id: format!("pull-skills-{id}"),
                body: Some(client_message::Body::PullSkills(PullSkillsRequest {
                    node_id: id.clone(),
                    known_generation: known,
                })),
            })
            .await
            .expect("pull skills");
        let msg = recv(&mut conn.inbound).await;
        match msg.body {
            Some(server_message::Body::DesiredSkills(desired)) => {
                assert!(desired.skills.is_empty(), "{desired:?}");
            }
            other => panic!("expected empty DesiredSkills, got {other:?}"),
        }
        conn.tx
            .send(ClientMessage {
                request_id: format!("pull-config-{id}"),
                body: Some(client_message::Body::PullConfig(PullConfigRequest {
                    node_id: id.clone(),
                    known_generation: 0,
                })),
            })
            .await
            .expect("pull config");
        match recv(&mut conn.inbound).await.body {
            Some(server_message::Body::PullConfig(resp)) => resp.config_generation,
            other => panic!("expected PullConfig, got {other:?}"),
        }
    };
    {
        let conn = node.conn.as_mut().expect("session");
        conn.tx
            .send(ClientMessage {
                request_id: format!("ready-{id}"),
                body: Some(client_message::Body::NodeReady(NodeReady {
                    node_id: id,
                    config_generation,
                    skill_set_generation: generation,
                })),
            })
            .await
            .expect("ready");
    }
    expect_ack(node).await;
    assert!(hub.is_connected(node.id), "{}", node.id);
}

fn register_msg(
    node_id: &str,
    suite: &str,
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
            labels: labels_for(suite),
            skill_abi: ABI.into(),
            capabilities_supported: CATALOG.iter().map(|cap| (*cap).to_string()).collect(),
            skill_set_generation: generation,
            installed,
            supports_ready: true,
        })),
    }
}

fn labels_for(suite: &str) -> HashMap<String, String> {
    let platform = novbot_node::skills::current_platform();
    let (os, arch) = platform.split_once('/').expect("os/arch platform");
    HashMap::from([
        ("role".to_string(), "observe".to_string()),
        ("os".to_string(), os.to_string()),
        ("arch".to_string(), arch.to_string()),
        ("suite".to_string(), suite.to_string()),
    ])
}

async fn put_specs(app: &axum::Router, node_id: &str) {
    let specs: Vec<Value> = SKILLS
        .iter()
        .map(|name| {
            json!({
                "id": name,
                "kind": "skill",
                "params": {"skill": name, "version": "=1.0.0"}
            })
        })
        .collect();
    let (status, bytes) = exchange(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/nodes/{node_id}/config"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({
                    "specs": specs,
                    "schedules": []
                }))
                .unwrap(),
            ))
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

async fn install_skill(app: &axum::Router, name: &str, cap: &str, suite: &str) -> Value {
    let (status, value) = exchange_json(
        app,
        "POST",
        &format!("/v1/skills/{name}/install"),
        &json!({
            "version": "1.0.0",
            "selector": {"labels": {"suite": suite}},
            "accepted_capabilities_sha256": cap,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{name} {value}");
    value
}

async fn send_report(node: &mut Node, snap: &novbot_node::skills::SkillSnapshot) {
    let id = node.id;
    node.conn
        .as_ref()
        .expect("session")
        .tx
        .send(ClientMessage {
            request_id: format!("report-{id}-{}", snap.applied_generation),
            body: Some(client_message::Body::SkillState(snap.to_report(id))),
        })
        .await
        .expect("report");
    expect_ack(node).await;
}

async fn assert_center_installed(app: &axum::Router, node_id: &str) {
    let body = get_json(app, &format!("/v1/nodes/{node_id}/skills")).await;
    let items = body["items"].as_array().expect("items");
    for name in SKILLS {
        let item = items
            .iter()
            .find(|item| item["name"] == name && item["source"] == "hub")
            .unwrap_or_else(|| panic!("{node_id} missing {name}: {body}"));
        assert_eq!(item["state"], "installed", "{item}");
        assert_eq!(item["desired_version"], "1.0.0", "{item}");
        assert_eq!(item["actual_version"], "1.0.0", "{item}");
    }
}

async fn dispatch_run(app: &axum::Router, node: &mut Node, name: &str) -> (String, Value) {
    let (status, accepted) = exchange_json(
        app,
        "POST",
        &format!("/v1/nodes/{}/dispatch", node.id),
        &json!({"spec_id": name}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{name} {accepted}");
    assert_eq!(accepted["delivered"], "live", "{accepted}");
    let run_id = accepted["run_id"].as_str().unwrap().to_string();
    let dispatch = recv_dispatch(node.inbound(), name, &run_id).await;
    let mut spec = skill_spec(name);
    apply_dispatch_overlay(&mut spec, &dispatch.params_json);
    let data = node.data.path().to_path_buf();
    let (status, payload) = execute_spec(
        &spec,
        &ProbePolicy::default(),
        &node.host,
        &data,
        &dispatch.run_id,
    )
    .await;
    report_result(node, &dispatch.run_id, name, &status, &payload).await;
    (status, payload)
}

fn skill_spec(name: &str) -> Spec {
    Spec {
        id: name.to_string(),
        kind: SpecKind::Skill,
        params: json!({"skill": name, "version": "=1.0.0"}),
        threshold: None,
    }
}

fn assert_skill_ok(node_id: &str, name: &str, status: &str, payload: &Value) {
    assert_eq!(status, "ok", "{node_id} {name} {payload}");
    assert_eq!(payload["skill"]["name"], name, "{payload}");
    assert_eq!(payload["skill"]["version"], "1.0.0", "{payload}");
    match name {
        "cpu-usage" => {
            assert_eq!(payload["cpu_usage_percent"], 12.5, "{payload}");
            assert_eq!(payload["load_avg_1m"], 0.4, "{payload}");
            assert_eq!(payload["load_avg_5m"], 0.3, "{payload}");
            assert_eq!(payload["load_avg_15m"], 0.2, "{payload}");
        }
        "memory-usage" => {
            assert_eq!(payload["total_bytes"], 8_589_934_592u64, "{payload}");
            assert_eq!(payload["used_bytes"], 1000, "{payload}");
            assert_eq!(payload["available_bytes"], 2000, "{payload}");
            assert_eq!(payload["swap_total_bytes"], 3000, "{payload}");
            assert_eq!(payload["swap_used_bytes"], 4, "{payload}");
        }
        "disk-usage" => {
            let disks = payload["disks"].as_array().expect("disks");
            assert!(disks.iter().any(|disk| disk["mount"] == "/"), "{payload}");
            assert_eq!(disks[0]["total_bytes"], 5000, "{payload}");
        }
        "process-top" => {
            assert_eq!(payload["by_cpu"][0]["name"], "d10-init", "{payload}");
            assert_eq!(payload["by_cpu"][0]["pid"], 7, "{payload}");
            assert_eq!(payload["by_cpu"][0]["user"], "root", "{payload}");
            assert_eq!(payload["by_cpu"][0]["cpu_percent"], 1.25, "{payload}");
            assert_eq!(payload["by_cpu"][0]["memory_bytes"], 4096, "{payload}");
            assert!(payload["by_memory"].is_array(), "{payload}");
            assert_no_cmdline(payload);
            assert!(!payload.to_string().contains(SECRET), "{payload}");
        }
        "net-interface-traffic" => {
            assert_eq!(payload["interfaces"][0]["name"], "d10-eth0", "{payload}");
            assert_eq!(payload["interfaces"][0]["rx_bytes"], 111, "{payload}");
            assert_eq!(payload["interfaces"][0]["tx_bytes"], 222, "{payload}");
            assert_eq!(payload["interfaces"][0]["rx_packets"], 11, "{payload}");
            assert_eq!(payload["interfaces"][0]["tx_packets"], 22, "{payload}");
        }
        other => panic!("unexpected skill {other}"),
    }
}

fn assert_no_cmdline(value: &Value) {
    const BANNED: &[&str] = &["cmdline", "command_line", "argv", "args"];
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                assert!(!BANNED.contains(&key.as_str()), "{key}");
                assert_no_cmdline(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_cmdline(item);
            }
        }
        _ => {}
    }
}

async fn assert_builtins_still_run() {
    let policy = ProbePolicy::default();
    let specs = [
        Spec {
            id: "cpu".into(),
            kind: SpecKind::Cpu,
            params: json!({}),
            threshold: None,
        },
        Spec {
            id: "memory".into(),
            kind: SpecKind::Memory,
            params: json!({}),
            threshold: None,
        },
        Spec {
            id: "disk-root".into(),
            kind: SpecKind::Disk,
            params: json!({"mount": "/"}),
            threshold: None,
        },
    ];
    for spec in specs {
        assert!(
            !SKILLS.contains(&spec.id.as_str()),
            "built-in id {} collides with a hub skill",
            spec.id
        );
        let out = run_probe(&spec, &policy).await.unwrap();
        assert_eq!(out.status, "ok", "{}", spec.id);
        assert!(out.payload.is_object(), "{}", spec.id);
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

async fn replace_version_if_different(db: &Db, name: &str, version: &str, bytes: &[u8]) {
    let sha = hex::encode(Sha256::digest(bytes));
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
    body["per_node"]
        .as_array()
        .expect("per_node")
        .iter()
        .find(|item| item["node_id"] == node_id)
        .unwrap_or_else(|| panic!("missing {node_id} in {body}"))["outcome"]
        .as_str()
        .unwrap_or("")
}

fn node_ids_of(body: &Value) -> Vec<String> {
    body["per_node"]
        .as_array()
        .expect("per_node")
        .iter()
        .map(|item| item["node_id"].as_str().unwrap_or("").to_string())
        .collect()
}

async fn recv(inbound: &mut tonic::Streaming<ServerMessage>) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(60), inbound.message())
        .await
        .expect("timeout")
        .expect("stream")
        .expect("message")
}

async fn expect_ack(node: &mut Node) {
    let msg = recv(node.inbound()).await;
    match msg.body {
        Some(server_message::Body::Ack(ack)) => assert!(ack.ok, "{ack:?}"),
        other => panic!("expected Ack, got {other:?}"),
    }
}

async fn recv_full_desired(
    inbound: &mut tonic::Streaming<ServerMessage>,
) -> novbot_proto::DesiredSkills {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timed out waiting for five desired skills");
        let msg = tokio::time::timeout(left, inbound.message())
            .await
            .expect("timeout")
            .expect("stream")
            .expect("message");
        if let Some(server_message::Body::DesiredSkills(desired)) = msg.body {
            let ready = SKILLS.iter().all(|name| {
                desired.skills.iter().any(|skill| skill.name == *name)
                    && desired
                        .skills
                        .iter()
                        .any(|skill| skill.name == *name && !skill.fetch_ticket.is_empty())
            });
            if ready {
                assert_eq!(desired.skills.len(), SKILLS.len(), "{desired:?}");
                return desired;
            }
        }
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
