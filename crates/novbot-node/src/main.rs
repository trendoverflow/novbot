// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node daemon: Register → PullConfig (memory) → schedule probes → ReportResult + retry spool + single-file egress.
//! Control.Session disconnects reconnect with exponential backoff; the process stays alive (M12).

mod config;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use config::Secret;
use futures::StreamExt;
use novbot_core::{
    due_specs, parse_schedules_json, parse_specs_json, run_probe, write_egress_result,
    PendingReport, ProbePolicy, RetrySpool, Schedule, Spec, SpecKind,
};
use novbot_proto::control_client::ControlClient;
use novbot_proto::{
    client_message, server_message, ClientMessage, Dispatch, HeartbeatRequest, PullConfigRequest,
    RegisterRequest, ReportResultRequest, ServerMessage,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

#[derive(Debug, Parser)]
#[command(name = "novbot-node", about = "NovBot node daemon")]
struct Args {
    /// Center gRPC endpoint (env: NOVBOT_CENTER_GRPC), for example http://127.0.0.1:50051.
    #[arg(long)]
    center_grpc: Option<String>,

    /// Stable node id (env: NOVBOT_NODE_ID). If unset everywhere, loaded from or generated and persisted under --data-dir.
    #[arg(long)]
    node_id: Option<String>,

    /// Optional bootstrap token for Register (env: NOVBOT_BOOTSTRAP_TOKEN).
    #[arg(long)]
    bootstrap_token: Option<Secret>,

    /// TOML file with center_grpc, node_id, and bootstrap_token only (env: NOVBOT_CONFIG).
    #[arg(long, env = "NOVBOT_CONFIG")]
    config: Option<PathBuf>,

    /// Allow exec Specs to run shell commands as this user (env: NOVBOT_ALLOW_EXEC). Off by default; not a config-file key.
    #[arg(long)]
    allow_exec: bool,

    /// Local data directory (retry spool + last_result.json + node_id). Not business config.
    #[arg(long, env = "NOVBOT_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,

    /// Node labels sent on Register, as comma-separated key=value pairs (e.g. "role=db,env=demo").
    #[arg(long, env = "NOVBOT_NODE_LABELS", default_value = "")]
    labels: String,

    #[arg(long, default_value = "15")]
    heartbeat_secs: u64,

    #[arg(long, default_value = "5")]
    scheduler_tick_secs: u64,

    #[arg(long, default_value = "10")]
    retry_tick_secs: u64,
}

#[derive(Default)]
struct RuntimeState {
    specs: HashMap<String, Spec>,
    schedules: Vec<Schedule>,
    config_generation: i64,
    last_run: HashMap<String, u64>,
}

/// Live Session outbound. `None` while disconnected — ReportResult goes to the retry spool.
type SessionOut = Arc<RwLock<Option<mpsc::Sender<ClientMessage>>>>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();
    let cfg = match load_node_config(&args) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("novbot-node: {err}");
            std::process::exit(2);
        }
    };
    let (bootstrap_token, bootstrap_token_source) = match &cfg.bootstrap_token {
        Some(token) => ("set", token.source.to_string()),
        None => ("unset", String::new()),
    };
    let config_file = cfg
        .config_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    tracing::info!(
        center = %cfg.center_grpc.value,
        center_source = %cfg.center_grpc.source,
        node_id = %cfg.node_id.value,
        node_id_source = %cfg.node_id.source,
        bootstrap_token,
        bootstrap_token_source = %bootstrap_token_source,
        config_file = %config_file,
        "starting novbot-node"
    );
    if cfg.allow_exec.value {
        tracing::warn!(
            allow_exec = true,
            allow_exec_source = %cfg.allow_exec.source,
            "exec Spec kind ENABLED: center can run shell commands on this node as the service user"
        );
    } else {
        tracing::warn!(
            allow_exec = false,
            allow_exec_source = %cfg.allow_exec.source,
            "exec Spec kind disabled; exec runs will report failed/exec_disabled (enable node-locally with --allow-exec or NOVBOT_ALLOW_EXEC=true)"
        );
    }
    let policy = ProbePolicy {
        allow_exec: cfg.allow_exec.value,
    };
    resolve_center_dns(&cfg.center_grpc.value).await;

    tokio::fs::create_dir_all(&args.data_dir).await?;
    let node_id = cfg.node_id.value.clone();
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".into());

    let state = Arc::new(RwLock::new(RuntimeState::default()));
    let spool = Arc::new(RetrySpool::new(&args.data_dir));
    let session_out: SessionOut = Arc::new(RwLock::new(None));

    {
        let out = session_out.clone();
        let node_id = node_id.clone();
        let state = state.clone();
        let every = Duration::from_secs(args.heartbeat_secs.max(5));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                let gen = state.read().await.config_generation;
                let _ = try_send(
                    &out,
                    ClientMessage {
                        request_id: Uuid::new_v4().to_string(),
                        body: Some(client_message::Body::Heartbeat(HeartbeatRequest {
                            node_id: node_id.clone(),
                            config_generation: gen,
                        })),
                    },
                )
                .await;
            }
        });
    }

    {
        let out = session_out.clone();
        let node_id = node_id.clone();
        let state = state.clone();
        let spool = spool.clone();
        let data_dir = args.data_dir.clone();
        let every = Duration::from_secs(args.scheduler_tick_secs.max(1));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                if let Err(e) =
                    run_due(&node_id, &state, &spool, &data_dir, &out, None, policy).await
                {
                    tracing::warn!(error = %e, "scheduler tick failed");
                }
            }
        });
    }

    {
        let out = session_out.clone();
        let spool = spool.clone();
        let every = Duration::from_secs(args.retry_tick_secs.max(2));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                if let Err(e) = flush_spool(&spool, &out).await {
                    tracing::warn!(error = %e, "retry flush failed");
                }
            }
        });
    }

    let mut backoff = RECONNECT_MIN;
    loop {
        match run_session(&args, &cfg, &hostname, &state, &spool, &session_out, policy).await {
            Ok(()) => {
                tracing::warn!("control stream closed; will reconnect");
                backoff = RECONNECT_MIN;
            }
            Err(e) => {
                tracing::error!(
                    center = %cfg.center_grpc.value,
                    error = %format!("{e:#}"),
                    "center session failed; will reconnect"
                );
            }
        }
        *session_out.write().await = None;
        tracing::info!(secs = backoff.as_secs(), "reconnect backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff.saturating_mul(2)).min(RECONNECT_MAX);
    }
}

fn load_node_config(args: &Args) -> Result<config::NodeConfig, config::ConfigError> {
    let file = match &args.config {
        Some(path) => Some((path.clone(), config::load_file_config(path)?)),
        None => None,
    };
    config::resolve(
        config::CliValues {
            center_grpc: args.center_grpc.clone(),
            node_id: args.node_id.clone(),
            bootstrap_token: args.bootstrap_token.clone(),
            allow_exec: args.allow_exec,
        },
        |key| std::env::var(key).ok(),
        file,
        &args.data_dir,
    )
}

async fn resolve_center_dns(addr: &config::CenterAddr) {
    match tokio::net::lookup_host((addr.host.as_str(), addr.port)).await {
        Ok(lookup) => {
            let ips: Vec<String> = lookup.map(|socket| socket.ip().to_string()).collect();
            tracing::info!(address = %addr, ?ips, "center address resolved");
        }
        Err(error) => {
            tracing::warn!(address = %addr, %error, "center DNS lookup failed");
        }
    }
}

async fn run_session(
    args: &Args,
    cfg: &config::NodeConfig,
    hostname: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    session_out: &SessionOut,
    policy: ProbePolicy,
) -> Result<()> {
    let center = cfg.center_grpc.value.uri.clone();
    let node_id = cfg.node_id.value.as_str();
    let mut client = ControlClient::connect(center.clone())
        .await
        .with_context(|| format!("connect {center}"))?;

    let (tx, rx) = mpsc::channel::<ClientMessage>(64);
    *session_out.write().await = Some(tx.clone());

    let outbound = ReceiverStream::new(rx);
    let mut inbound = client
        .session(outbound)
        .await
        .context("open Session stream")?
        .into_inner();

    tx.send(ClientMessage {
        request_id: Uuid::new_v4().to_string(),
        body: Some(client_message::Body::Register(RegisterRequest {
            node_id: node_id.to_string(),
            hostname: hostname.to_string(),
            version: VERSION.into(),
            bootstrap_token: cfg
                .bootstrap_token
                .as_ref()
                .map(|token| token.value.expose().to_string())
                .unwrap_or_default(),
            labels: parse_labels(&args.labels),
        })),
    })
    .await
    .context("send Register")?;

    let known = state.read().await.config_generation;
    tx.send(ClientMessage {
        request_id: Uuid::new_v4().to_string(),
        body: Some(client_message::Body::PullConfig(PullConfigRequest {
            node_id: node_id.to_string(),
            known_generation: known,
        })),
    })
    .await
    .context("send PullConfig")?;

    // Flush any reports queued while disconnected.
    if let Err(e) = flush_spool(spool, session_out).await {
        tracing::warn!(error = %e, "post-reconnect spool flush failed");
    }

    tracing::info!("Control.Session connected");

    while let Some(frame) = inbound.next().await {
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "stream error");
                break;
            }
        };
        if let Err(e) = on_server(
            node_id,
            state,
            spool,
            &args.data_dir,
            session_out,
            policy,
            msg,
        )
        .await
        {
            tracing::warn!(error = %e, "handle server message failed");
        }
    }

    Ok(())
}

/// Parse "k=v,k2=v2" into a label map. Blank entries and entries without '=' or with an empty key are ignored.
fn parse_labels(raw: &str) -> HashMap<String, String> {
    raw.split(',')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            Some((k.to_string(), v.trim().to_string()))
        })
        .collect()
}

async fn try_send(out: &SessionOut, msg: ClientMessage) -> bool {
    let tx = out.read().await.clone();
    if let Some(tx) = tx {
        return tx.send(msg).await.is_ok();
    }
    false
}

async fn send_report_or_spool(
    out: &SessionOut,
    spool: &RetrySpool,
    msg: ClientMessage,
    pending: PendingReport,
) -> Result<()> {
    if try_send(out, msg).await {
        return Ok(());
    }
    let run_id = pending.run_id.clone();
    spool.enqueue(pending).await?;
    tracing::warn!(%run_id, "ReportResult queued to retry spool");
    Ok(())
}

async fn on_server(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    policy: ProbePolicy,
    msg: ServerMessage,
) -> Result<()> {
    let Some(body) = msg.body else {
        return Ok(());
    };
    match body {
        server_message::Body::Register(resp) => {
            if resp.accepted {
                tracing::info!(node_id = %resp.node_id, "registered");
            } else {
                tracing::error!(message = %resp.message, "register rejected");
            }
        }
        server_message::Body::Heartbeat(resp) => {
            let local = state.read().await.config_generation;
            if resp.center_config_generation > local {
                let _ = try_send(
                    out,
                    ClientMessage {
                        request_id: Uuid::new_v4().to_string(),
                        body: Some(client_message::Body::PullConfig(PullConfigRequest {
                            node_id: node_id.to_string(),
                            known_generation: local,
                        })),
                    },
                )
                .await;
            }
        }
        server_message::Body::PullConfig(resp) => {
            apply_config(
                state,
                resp.config_generation,
                &resp.specs_json,
                &resp.schedules_json,
                policy,
            )
            .await?;
        }
        server_message::Body::PushConfig(push) => {
            let schedules_json = {
                let st = state.read().await;
                serde_json::to_string(&st.schedules).unwrap_or_else(|_| "[]".into())
            };
            apply_config(
                state,
                push.config_generation,
                &push.specs_json,
                &schedules_json,
                policy,
            )
            .await?;
        }
        server_message::Body::PushSchedule(push) => {
            let mut st = state.write().await;
            st.schedules = parse_schedules_json(&push.schedules_json)?;
            st.config_generation = push.config_generation;
            tracing::info!(generation = push.config_generation, "schedules pushed");
        }
        server_message::Body::Dispatch(d) => {
            run_dispatch(node_id, state, spool, data_dir, out, &d, policy).await?;
        }
        server_message::Body::ReportResult(_)
        | server_message::Body::Ack(_)
        | server_message::Body::Error(_) => {}
    }
    Ok(())
}

async fn apply_config(
    state: &Arc<RwLock<RuntimeState>>,
    generation: i64,
    specs_json: &str,
    schedules_json: &str,
    policy: ProbePolicy,
) -> Result<()> {
    let specs = parse_specs_json(specs_json)?;
    let schedules = parse_schedules_json(schedules_json)?;
    if !policy.allow_exec {
        let exec_ids: Vec<&str> = specs
            .iter()
            .filter(|spec| spec.kind == SpecKind::Exec)
            .map(|spec| spec.id.as_str())
            .collect();
        if !exec_ids.is_empty() {
            tracing::warn!(
                count = exec_ids.len(),
                spec_ids = ?exec_ids,
                "config contains exec Specs but exec is disabled on this node; they will report failed/exec_disabled"
            );
        }
    }
    let mut st = state.write().await;
    st.specs = specs.into_iter().map(|s| (s.id.clone(), s)).collect();
    st.schedules = schedules;
    st.config_generation = generation;
    tracing::info!(
        generation,
        specs = st.specs.len(),
        schedules = st.schedules.len(),
        "config applied in memory"
    );
    Ok(())
}

async fn run_due(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    only_spec: Option<&str>,
    policy: ProbePolicy,
) -> Result<()> {
    let specs: Vec<Spec> = {
        let mut st = state.write().await;
        let now = SystemTime::now();
        let due = if let Some(id) = only_spec {
            vec![id.to_string()]
        } else {
            due_specs(&st.schedules, &st.last_run, now)
        };
        let now_secs = now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut selected = Vec::new();
        for id in due {
            if let Some(spec) = st.specs.get(&id) {
                selected.push(spec.clone());
                st.last_run.insert(id, now_secs);
            }
        }
        selected
    };

    for spec in specs {
        let run_id = Uuid::new_v4().to_string();
        let observed_at = Utc::now().timestamp_millis();
        let (status, payload) = match run_probe(&spec, &policy).await {
            Ok(out) => (out.status.to_string(), out.payload),
            Err(e) => ("error".to_string(), json!({"error": e.to_string()})),
        };
        let payload_json = serde_json::to_string(&payload)?;
        let egress = json!({
            "node_id": node_id,
            "run_id": run_id,
            "spec_id": spec.id,
            "status": status,
            "payload": payload,
            "observed_at_unix_ms": observed_at,
        });
        if let Err(e) = write_egress_result(data_dir, &egress).await {
            tracing::warn!(error = %e, "egress write failed");
        }

        let pending = PendingReport {
            node_id: node_id.to_string(),
            run_id: run_id.clone(),
            spec_id: spec.id.clone(),
            status: status.clone(),
            payload_json: payload_json.clone(),
            observed_at_unix_ms: observed_at,
            attempts: 1,
        };
        let msg = ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::ReportResult(ReportResultRequest {
                node_id: node_id.to_string(),
                run_id,
                spec_id: spec.id,
                status,
                payload_json,
                observed_at_unix_ms: observed_at,
            })),
        };
        send_report_or_spool(out, spool, msg, pending).await?;
    }
    Ok(())
}

async fn run_dispatch(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    d: &Dispatch,
    policy: ProbePolicy,
) -> Result<()> {
    let mut spec = {
        let st = state.read().await;
        st.specs
            .get(&d.spec_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("dispatch unknown spec_id: {}", d.spec_id))?
    };
    if !d.params_json.trim().is_empty() {
        if let Ok(overlay) = serde_json::from_str::<serde_json::Value>(&d.params_json) {
            if let Some(obj) = overlay.as_object() {
                if !spec.params.is_object() {
                    spec.params = json!({});
                }
                let base = spec.params.as_object_mut().unwrap();
                for (k, v) in obj {
                    base.insert(k.clone(), v.clone());
                }
            }
        }
    }

    let run_id = if d.run_id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        d.run_id.clone()
    };
    let observed_at = Utc::now().timestamp_millis();
    let (status, payload) = match run_probe(&spec, &policy).await {
        Ok(out) => (out.status.to_string(), out.payload),
        Err(e) => ("error".to_string(), json!({"error": e.to_string()})),
    };
    let payload_json = serde_json::to_string(&payload)?;
    let egress = json!({
        "node_id": node_id,
        "run_id": run_id,
        "spec_id": spec.id,
        "status": status,
        "payload": payload,
        "observed_at_unix_ms": observed_at,
        "source": "dispatch",
    });
    if let Err(e) = write_egress_result(data_dir, &egress).await {
        tracing::warn!(error = %e, "egress write failed");
    }

    let pending = PendingReport {
        node_id: node_id.to_string(),
        run_id: run_id.clone(),
        spec_id: spec.id.clone(),
        status: status.clone(),
        payload_json: payload_json.clone(),
        observed_at_unix_ms: observed_at,
        attempts: 1,
    };
    let msg = ClientMessage {
        request_id: Uuid::new_v4().to_string(),
        body: Some(client_message::Body::ReportResult(ReportResultRequest {
            node_id: node_id.to_string(),
            run_id,
            spec_id: spec.id,
            status,
            payload_json,
            observed_at_unix_ms: observed_at,
        })),
    };
    send_report_or_spool(out, spool, msg, pending).await?;
    Ok(())
}

async fn flush_spool(spool: &RetrySpool, out: &SessionOut) -> Result<()> {
    let connected = out.read().await.is_some();
    if !connected {
        return Ok(());
    }
    let items = spool.drain().await?;
    for mut item in items {
        let msg = ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::ReportResult(ReportResultRequest {
                node_id: item.node_id.clone(),
                run_id: item.run_id.clone(),
                spec_id: item.spec_id.clone(),
                status: item.status.clone(),
                payload_json: item.payload_json.clone(),
                observed_at_unix_ms: item.observed_at_unix_ms,
            })),
        };
        if !try_send(out, msg).await {
            item.attempts = item.attempts.saturating_add(1);
            spool.enqueue(item).await?;
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod label_tests {
    use super::{parse_labels, Args};
    use clap::Parser;

    #[test]
    fn parses_pairs_and_ignores_junk() {
        let m = parse_labels(" role=db , env=demo,,novalue, =x");
        assert_eq!(m.len(), 2);
        assert_eq!(m.get("role").map(String::as_str), Some("db"));
        assert_eq!(m.get("env").map(String::as_str), Some("demo"));
    }

    #[test]
    fn empty_is_empty() {
        assert!(parse_labels("").is_empty());
    }

    #[test]
    fn args_debug_redacts_bootstrap_token() {
        let args = Args::try_parse_from([
            "novbot-node",
            "--center-grpc",
            "http://127.0.0.1:50051",
            "--node-id",
            "n1",
            "--bootstrap-token",
            "tok-123",
        ])
        .unwrap();
        let rendered = format!("{args:?}");
        assert!(!rendered.contains("tok-123"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}

#[cfg(test)]
mod exec_gate_tests {
    use super::{apply_config, run_dispatch, run_due, Args, RuntimeState, SessionOut};
    use clap::Parser;
    use novbot_core::{ProbePolicy, RetrySpool};
    use novbot_proto::Dispatch;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn clap_allow_exec_flag() {
        let enabled = Args::try_parse_from(["novbot-node", "--allow-exec"]).unwrap();
        assert!(enabled.allow_exec);

        let disabled = Args::try_parse_from(["novbot-node"]).unwrap();
        assert!(!disabled.allow_exec);
    }

    fn touch_spec(spec_id: &str, marker: &std::path::Path) -> String {
        serde_json::json!([{
            "id": spec_id,
            "kind": "exec",
            "params": {
                "command": format!("touch {}", marker.display()),
                "allow_exec": true
            }
        }])
        .to_string()
    }

    fn assert_exec_disabled(payload_json: &str, marker: &std::path::Path) {
        let payload: serde_json::Value = serde_json::from_str(payload_json).unwrap();
        assert_eq!(payload["reason"], "exec_disabled");
        assert!(!payload_json.contains("touch"));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn center_config_cannot_enable_exec() {
        let marker_dir = tempfile::tempdir().unwrap();
        let marker = marker_dir.path().join("marker");
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let spool = Arc::new(RetrySpool::new(&data_dir));
        let state = Arc::new(RwLock::new(RuntimeState::default()));
        let spec_id = "exec-touch";
        let schedules_json = serde_json::json!([{
            "spec_id": spec_id,
            "interval_secs": 1
        }])
        .to_string();

        apply_config(
            &state,
            1,
            &touch_spec(spec_id, &marker),
            &schedules_json,
            ProbePolicy::default(),
        )
        .await
        .unwrap();

        let out: SessionOut = Arc::new(RwLock::new(None));
        let result = run_due(
            "node-1",
            &state,
            &spool,
            &data_dir,
            &out,
            None,
            ProbePolicy::default(),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let items = spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert_exec_disabled(&items[0].payload_json, &marker);
    }

    #[tokio::test]
    async fn dispatch_overlay_cannot_enable_exec() {
        let marker_dir = tempfile::tempdir().unwrap();
        let marker = marker_dir.path().join("marker");
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let spool = Arc::new(RetrySpool::new(&data_dir));
        let state = Arc::new(RwLock::new(RuntimeState::default()));
        let spec_id = "exec-touch";

        apply_config(
            &state,
            1,
            &touch_spec(spec_id, &marker),
            "[]",
            ProbePolicy::default(),
        )
        .await
        .unwrap();

        let out: SessionOut = Arc::new(RwLock::new(None));
        let dispatch = Dispatch {
            run_id: "run-1".into(),
            spec_id: spec_id.into(),
            params_json: r#"{"allow_exec":true}"#.into(),
        };
        let result = run_dispatch(
            "node-1",
            &state,
            &spool,
            &data_dir,
            &out,
            &dispatch,
            ProbePolicy::default(),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let items = spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert_exec_disabled(&items[0].payload_json, &marker);
    }
}
