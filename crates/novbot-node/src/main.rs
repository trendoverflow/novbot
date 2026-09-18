// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node daemon: Register → PullConfig (memory) → schedule probes → ReportResult + retry spool + single-file egress.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;
use futures::StreamExt;
use novbot_core::{
    due_specs, parse_schedules_json, parse_specs_json, run_probe, write_egress_result,
    PendingReport, RetrySpool, Schedule, Spec,
};
use novbot_proto::control_client::ControlClient;
use novbot_proto::{
    client_message, server_message, ClientMessage, HeartbeatRequest, PullConfigRequest,
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
    let (outbound_tx, outbound_rx) = mpsc::channel::<ClientMessage>(64);
    let outbound_tx = Arc::new(outbound_tx);

    let mut client = ControlClient::connect(args.center_grpc.clone())
        .await
        .with_context(|| format!("connect {}", args.center_grpc))?;

    let outbound = ReceiverStream::new(outbound_rx);
    let mut inbound = client
        .session(outbound)
        .await
        .context("open Session stream")?
        .into_inner();

    outbound_tx
        .send(ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::Register(RegisterRequest {
                node_id: node_id.clone(),
                hostname,
                version: VERSION.into(),
                bootstrap_token: args.bootstrap_token.clone().unwrap_or_default(),
                labels: HashMap::new(),
            })),
        })
        .await?;

    outbound_tx
        .send(ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::PullConfig(PullConfigRequest {
                node_id: node_id.clone(),
                known_generation: 0,
            })),
        })
        .await?;

    {
        let tx = outbound_tx.clone();
        let node_id = node_id.clone();
        let state = state.clone();
        let every = Duration::from_secs(args.heartbeat_secs.max(5));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                let gen = state.read().await.config_generation;
                let _ = tx
                    .send(ClientMessage {
                        request_id: Uuid::new_v4().to_string(),
                        body: Some(client_message::Body::Heartbeat(HeartbeatRequest {
                            node_id: node_id.clone(),
                            config_generation: gen,
                        })),
                    })
                    .await;
            }
        });
    }

    {
        let tx = outbound_tx.clone();
        let node_id = node_id.clone();
        let state = state.clone();
        let spool = spool.clone();
        let data_dir = args.data_dir.clone();
        let every = Duration::from_secs(args.scheduler_tick_secs.max(1));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                if let Err(e) = run_due(&node_id, &state, &spool, &data_dir, &tx, None).await {
                    tracing::warn!(error = %e, "scheduler tick failed");
                }
            }
        });
    }

    {
        let tx = outbound_tx.clone();
        let spool = spool.clone();
        let every = Duration::from_secs(args.retry_tick_secs.max(2));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                if let Err(e) = flush_spool(&spool, &tx).await {
                    tracing::warn!(error = %e, "retry flush failed");
                }
            }
        });
    }

    while let Some(frame) = inbound.next().await {
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "stream error");
                break;
            }
        };
        if let Err(e) = on_server(&node_id, &state, &spool, &args.data_dir, &outbound_tx, msg).await
        {
            tracing::warn!(error = %e, "handle server message failed");
        }
    }

    bail!("control stream closed")
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
    tx: &mpsc::Sender<ClientMessage>,
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
                tx.send(ClientMessage {
                    request_id: Uuid::new_v4().to_string(),
                    body: Some(client_message::Body::PullConfig(PullConfigRequest {
                        node_id: node_id.to_string(),
                        known_generation: local,
                    })),
                })
                .await?;
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
            run_due(
                node_id,
                state,
                spool,
                data_dir,
                tx,
                Some(d.spec_id.as_str()),
            )
            .await?;
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
    tx: &mpsc::Sender<ClientMessage>,
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

        let msg = ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::ReportResult(ReportResultRequest {
                node_id: node_id.to_string(),
                run_id: run_id.clone(),
                spec_id: spec.id.clone(),
                status: status.clone(),
                payload_json: payload_json.clone(),
                observed_at_unix_ms: observed_at,
            })),
        };
        if tx.send(msg).await.is_err() {
            spool
                .enqueue(PendingReport {
                    node_id: node_id.to_string(),
                    run_id,
                    spec_id: spec.id,
                    status,
                    payload_json,
                    observed_at_unix_ms: observed_at,
                    attempts: 1,
                })
                .await?;
        }
    }
    Ok(())
}

async fn flush_spool(spool: &RetrySpool, tx: &mpsc::Sender<ClientMessage>) -> Result<()> {
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
        if tx.send(msg).await.is_err() {
            item.attempts = item.attempts.saturating_add(1);
            spool.enqueue(item).await?;
            break;
        }
    }
    Ok(())
}
