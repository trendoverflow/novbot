// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node daemon: Register → PullConfig (memory) → schedule probes → ReportResult + retry spool + single-file egress.
//! Control.Session disconnects reconnect with exponential backoff; the process stays alive (M12).

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use futures::StreamExt;
use novbot_core::{
    due_specs, parse_schedules_json, parse_specs_json, run_probe, write_egress_result,
    PendingReport, RetrySpool, Schedule, Spec,
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
    /// Center gRPC endpoint, e.g. http://127.0.0.1:50051
    #[arg(long, env = "NOVBOT_CENTER_GRPC")]
    center_grpc: String,

    /// Stable node id. If omitted, generated and persisted under --data-dir.
    #[arg(long, env = "NOVBOT_NODE_ID")]
    node_id: Option<String>,

    /// Local data directory (retry spool + last_result.json + node_id). Not business config.
    #[arg(long, env = "NOVBOT_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,

    /// Optional bootstrap token for Register.
    #[arg(long, env = "NOVBOT_BOOTSTRAP_TOKEN")]
    bootstrap_token: Option<String>,

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
    tokio::fs::create_dir_all(&args.data_dir).await?;
    let node_id = resolve_node_id(&args).await?;
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".into());

    tracing::info!(%node_id, center = %args.center_grpc, "starting novbot-node");

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
                if let Err(e) = run_due(&node_id, &state, &spool, &data_dir, &out, None).await {
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
        match run_session(
            &args,
            &node_id,
            &hostname,
            &state,
            &spool,
            &session_out,
        )
        .await
        {
            Ok(()) => {
                tracing::warn!("control stream closed; will reconnect");
                backoff = RECONNECT_MIN;
            }
            Err(e) => {
                tracing::error!(error = %e, "session error; will reconnect");
            }
        }
        *session_out.write().await = None;
        tracing::info!(secs = backoff.as_secs(), "reconnect backoff");
        tokio::time::sleep(backoff).await;
        backoff = (backoff.saturating_mul(2)).min(RECONNECT_MAX);
    }
}

async fn run_session(
    args: &Args,
    node_id: &str,
    hostname: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    session_out: &SessionOut,
) -> Result<()> {
    let mut client = ControlClient::connect(args.center_grpc.clone())
        .await
        .with_context(|| format!("connect {}", args.center_grpc))?;

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
            bootstrap_token: args.bootstrap_token.clone().unwrap_or_default(),
            labels: HashMap::new(),
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
        if let Err(e) = on_server(node_id, state, spool, &args.data_dir, session_out, msg).await {
            tracing::warn!(error = %e, "handle server message failed");
        }
    }

    Ok(())
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

async fn resolve_node_id(args: &Args) -> Result<String> {
    if let Some(id) = &args.node_id {
        return Ok(id.clone());
    }
    let path = args.data_dir.join("node_id");
    if path.exists() {
        let id = tokio::fs::read_to_string(&path).await?;
        let id = id.trim().to_string();
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let id = format!("node-{}", Uuid::new_v4());
    tokio::fs::write(&path, &id).await?;
    Ok(id)
}

async fn on_server(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
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
            run_dispatch(node_id, state, spool, data_dir, out, &d).await?;
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
) -> Result<()> {
    let specs = parse_specs_json(specs_json)?;
    let schedules = parse_schedules_json(schedules_json)?;
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
        let (status, payload) = match run_probe(&spec).await {
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
    let (status, payload) = match run_probe(&spec).await {
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
