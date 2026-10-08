// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

//! Node daemon: Register → PullSkills → reconcile → SkillStateReport → PullConfig → NodeReady
//! → schedule probes → ReportResult + retry spool + single-file egress.
//! Control.Session disconnects reconnect with exponential backoff; the process stays alive (M12).

mod config;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use config::Secret;
use futures::StreamExt;
use novbot_core::{
    due_specs, parse_schedules_json, parse_specs_json, write_egress_result, PendingReport,
    ProbePolicy, RetrySpool, Schedule, Spec, SpecKind,
};
use novbot_node::skills::{DesiredSet, GrpcArtifactSource, InstallLimits, SkillHost, ABI, CATALOG};
use novbot_node::{apply_dispatch_overlay, execute_spec};
use novbot_proto::control_client::ControlClient;
use novbot_proto::{
    client_message, server_message, ClientMessage, Dispatch, HeartbeatRequest, NodeReady,
    PullConfigRequest, PullSkillsRequest, RegisterRequest, ReportResultRequest, ServerMessage,
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
    /// True after this process has applied a config. Not inferred from
    /// `config_generation`: generation 0 can be a real applied config.
    config_loaded: bool,
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
    warn_plaintext_skills(&cfg.center_grpc.value);

    tokio::fs::create_dir_all(&args.data_dir).await?;
    let skill_source = Arc::new(GrpcArtifactSource::new(
        cfg.center_grpc.value.uri.clone(),
        cfg.node_id.value.clone(),
    ));
    let skills = Arc::new(
        SkillHost::open(&args.data_dir, skill_source, InstallLimits::default())
            .await
            .context("open skill store")?,
    );
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
        let skills = skills.clone();
        let every = Duration::from_secs(args.heartbeat_secs.max(5));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                let gen = state.read().await.config_generation;
                let skill_gen = skills.applied_generation();
                let _ = try_send(
                    &out,
                    ClientMessage {
                        request_id: Uuid::new_v4().to_string(),
                        body: Some(client_message::Body::Heartbeat(HeartbeatRequest {
                            node_id: node_id.clone(),
                            config_generation: gen,
                            skill_set_generation: skill_gen,
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
        let skills = skills.clone();
        let every = Duration::from_secs(args.scheduler_tick_secs.max(1));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(every);
            loop {
                ticker.tick().await;
                if let Err(e) = run_due(
                    &node_id, &state, &spool, &data_dir, &out, None, &skills, policy,
                )
                .await
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
        match run_session(
            &args,
            &cfg,
            &hostname,
            &state,
            &spool,
            &session_out,
            &skills,
            policy,
        )
        .await
        {
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

fn warn_plaintext_skills(addr: &config::CenterAddr) {
    let loopback = matches!(
        addr.host.as_str(),
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    );
    if addr.uri.starts_with("http://") && !loopback {
        tracing::warn!(
            address = %addr,
            "skill packages travel on plaintext gRPC to a non-loopback center"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    args: &Args,
    cfg: &config::NodeConfig,
    hostname: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    session_out: &SessionOut,
    skills: &Arc<SkillHost>,
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
            skill_abi: ABI.into(),
            capabilities_supported: CATALOG.iter().map(|cap| (*cap).to_string()).collect(),
            skill_set_generation: skills.applied_generation(),
            installed: skills.register_installed(),
            supports_ready: true,
        })),
    })
    .await
    .context("send Register")?;

    tx.send(ClientMessage {
        request_id: Uuid::new_v4().to_string(),
        body: Some(client_message::Body::PullSkills(PullSkillsRequest {
            node_id: node_id.to_string(),
            known_generation: skills.applied_generation(),
        })),
    })
    .await
    .context("send PullSkills")?;

    // Flush any reports queued while disconnected.
    if let Err(e) = flush_spool(spool, session_out).await {
        tracing::warn!(error = %e, "post-reconnect spool flush failed");
    }

    tracing::info!("Control.Session connected");

    let mut pulled_config = false;
    while let Some(frame) = inbound.next().await {
        let msg = match frame {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "stream error");
                break;
            }
        };
        match msg.body {
            Some(server_message::Body::DesiredSkills(desired)) => {
                let boot = !pulled_config;
                drive_skills(skills, node_id, desired, session_out, boot).await;
                if boot {
                    send_pull_config(node_id, state, session_out).await;
                    pulled_config = true;
                }
            }
            Some(server_message::Body::Error(err)) if !pulled_config => {
                tracing::warn!(code = %err.code, "skill pull failed; continuing to config");
                send_pull_config(node_id, state, session_out).await;
                pulled_config = true;
            }
            body => {
                if let Err(e) = on_server(
                    node_id,
                    state,
                    spool,
                    &args.data_dir,
                    session_out,
                    skills,
                    policy,
                    ServerMessage {
                        request_id: msg.request_id,
                        body,
                    },
                )
                .await
                {
                    tracing::warn!(error = %e, "handle server message failed");
                }
            }
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

async fn send_pull_config(node_id: &str, state: &Arc<RwLock<RuntimeState>>, out: &SessionOut) {
    let known = state.read().await.config_generation;
    let _ = try_send(
        out,
        ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::PullConfig(PullConfigRequest {
                node_id: node_id.to_string(),
                known_generation: known,
            })),
        },
    )
    .await;
}

async fn drive_skills(
    skills: &Arc<SkillHost>,
    node_id: &str,
    desired: novbot_proto::DesiredSkills,
    out: &SessionOut,
    boot: bool,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let host = skills.clone();
    let set = DesiredSet::from_proto(&desired);
    tokio::spawn(async move {
        let snap = host.reconcile(set).await;
        let _ = tx.send(snap).await;
    });
    if !boot {
        let out = out.clone();
        let node_id = node_id.to_string();
        tokio::spawn(async move {
            if let Some(snap) = rx.recv().await {
                send_skill_report(&out, &node_id, &snap).await;
            }
        });
        return;
    }
    let bound = skills.ready_bound();
    match tokio::time::timeout(bound, rx.recv()).await {
        Ok(Some(snap)) => send_skill_report(out, node_id, &snap).await,
        _ => {
            let partial = skills.snapshot();
            send_skill_report(out, node_id, &partial).await;
            let out = out.clone();
            let node_id = node_id.to_string();
            tokio::spawn(async move {
                if let Some(snap) = rx.recv().await {
                    send_skill_report(&out, &node_id, &snap).await;
                }
            });
        }
    }
}

async fn send_skill_report(
    out: &SessionOut,
    node_id: &str,
    snap: &novbot_node::skills::SkillSnapshot,
) {
    let _ = try_send(
        out,
        ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::SkillState(snap.to_report(node_id))),
        },
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn on_server(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    skills: &SkillHost,
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
                send_pull_config(node_id, state, out).await;
            }
            if skills.should_pull(resp.center_skill_set_generation) {
                let _ = try_send(
                    out,
                    ClientMessage {
                        request_id: Uuid::new_v4().to_string(),
                        body: Some(client_message::Body::PullSkills(PullSkillsRequest {
                            node_id: node_id.to_string(),
                            known_generation: skills.applied_generation(),
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
            send_node_ready(node_id, state, skills, out).await;
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
            send_node_ready(node_id, state, skills, out).await;
        }
        server_message::Body::PushSchedule(push) => {
            let mut st = state.write().await;
            st.schedules = parse_schedules_json(&push.schedules_json)?;
            st.config_generation = push.config_generation;
            tracing::info!(generation = push.config_generation, "schedules pushed");
        }
        server_message::Body::Dispatch(d) => {
            run_dispatch(node_id, state, spool, data_dir, out, skills, &d, policy).await?;
        }
        server_message::Body::DesiredSkills(_)
        | server_message::Body::ReportResult(_)
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
    st.config_loaded = true;
    tracing::info!(
        generation,
        specs = st.specs.len(),
        schedules = st.schedules.len(),
        "config applied in memory"
    );
    Ok(())
}

async fn send_node_ready(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    skills: &SkillHost,
    out: &SessionOut,
) {
    let generation = state.read().await.config_generation;
    let skill_set_generation = skills.applied_generation();
    let sent = try_send(
        out,
        ClientMessage {
            request_id: Uuid::new_v4().to_string(),
            body: Some(client_message::Body::NodeReady(NodeReady {
                node_id: node_id.to_string(),
                config_generation: generation,
                skill_set_generation,
            })),
        },
    )
    .await;
    if sent {
        tracing::info!(generation, "config applied; sent NodeReady");
    } else {
        tracing::warn!(
            generation,
            "config applied; NodeReady not sent (session not connected)"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_due(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    only_spec: Option<&str>,
    skills: &SkillHost,
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
        let (status, payload) = execute_spec(&spec, &policy, skills, data_dir, &run_id).await;
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

#[allow(clippy::too_many_arguments)]
async fn run_dispatch(
    node_id: &str,
    state: &Arc<RwLock<RuntimeState>>,
    spool: &Arc<RetrySpool>,
    data_dir: &PathBuf,
    out: &SessionOut,
    skills: &SkillHost,
    d: &Dispatch,
    policy: ProbePolicy,
) -> Result<()> {
    // A dispatch that cannot run still reports failed (same shape as exec_disabled)
    // so the run is never dropped.
    let run_id = if d.run_id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        d.run_id.clone()
    };
    let prep = {
        let st = state.read().await;
        if !st.config_loaded {
            Err("config_not_ready")
        } else if let Some(spec) = st.specs.get(&d.spec_id).cloned() {
            Ok(spec)
        } else {
            Err("spec_not_found")
        }
    };
    let (spec_id, status, payload, observed_at) = match prep {
        Err(reason) => {
            tracing::warn!(
                run_id = %run_id,
                spec_id = %d.spec_id,
                %reason,
                "dispatch cannot run"
            );
            (
                d.spec_id.clone(),
                "failed".to_string(),
                json!({
                    "reason": reason,
                    "spec_id": d.spec_id,
                }),
                Utc::now().timestamp_millis(),
            )
        }
        Ok(mut spec) => {
            apply_dispatch_overlay(&mut spec, &d.params_json);
            let observed_at = Utc::now().timestamp_millis();
            let (status, payload) = execute_spec(&spec, &policy, skills, data_dir, &run_id).await;
            (spec.id, status, payload, observed_at)
        }
    };
    let payload_json = serde_json::to_string(&payload)?;
    let egress = json!({
        "node_id": node_id,
        "run_id": run_id,
        "spec_id": spec_id,
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
        spec_id: spec_id.clone(),
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
            spec_id,
            status,
            payload_json,
            observed_at_unix_ms: observed_at,
        })),
    };
    send_report_or_spool(out, spool, msg, pending).await?;
    Ok(())
}

#[cfg(test)]
struct IdleSource;

#[cfg(test)]
#[async_trait::async_trait]
impl novbot_node::skills::ArtifactSource for IdleSource {
    async fn fetch(
        &self,
        _: &str,
        _: &str,
        _: u64,
    ) -> std::result::Result<novbot_node::skills::Fetched, String> {
        Err("no artifact".into())
    }
}

#[cfg(test)]
async fn idle_skills(data_dir: &std::path::Path) -> SkillHost {
    SkillHost::open(
        data_dir,
        std::sync::Arc::new(IdleSource),
        InstallLimits::default(),
    )
    .await
    .expect("skill host")
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
    use super::{apply_config, idle_skills, run_dispatch, run_due, Args, RuntimeState, SessionOut};
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
        let skills = idle_skills(&data_dir).await;
        let result = run_due(
            "node-1",
            &state,
            &spool,
            &data_dir,
            &out,
            None,
            &skills,
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
        let skills = idle_skills(&data_dir).await;
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
            &skills,
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

#[cfg(test)]
mod dispatch_ready_tests {
    use super::{apply_config, idle_skills, on_server, run_dispatch, RuntimeState, SessionOut};
    use novbot_core::{ProbePolicy, RetrySpool};
    use novbot_proto::{
        client_message, server_message, Dispatch, PullConfigResponse, ServerMessage,
    };
    use std::sync::Arc;
    use tokio::sync::{mpsc, RwLock};

    fn skill_specs(spec_id: &str) -> String {
        serde_json::json!([{
            "id": spec_id,
            "kind": "skill",
            "params": {"skill": "host_info"}
        }])
        .to_string()
    }

    fn assert_failed_payload(payload_json: &str, reason: &str, spec_id: &str) {
        let payload: serde_json::Value = serde_json::from_str(payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({ "reason": reason, "spec_id": spec_id })
        );
    }

    struct Harness {
        _data: tempfile::TempDir,
        data_dir: std::path::PathBuf,
        spool: Arc<RetrySpool>,
        state: Arc<RwLock<RuntimeState>>,
        out: SessionOut,
    }

    fn harness() -> Harness {
        let data = tempfile::tempdir().unwrap();
        let data_dir = data.path().to_path_buf();
        let spool = Arc::new(RetrySpool::new(&data_dir));
        let state = Arc::new(RwLock::new(RuntimeState::default()));
        let out: SessionOut = Arc::new(RwLock::new(None));
        Harness {
            _data: data,
            data_dir,
            spool,
            state,
            out,
        }
    }

    #[tokio::test]
    async fn dispatch_before_config_reports_config_not_ready() {
        let h = harness();
        let dispatch = Dispatch {
            run_id: "run-keep".into(),
            spec_id: "host-skill".into(),
            params_json: "{}".into(),
        };
        let skills = idle_skills(&h.data_dir).await;
        let result = run_dispatch(
            "node-1",
            &h.state,
            &h.spool,
            &h.data_dir,
            &h.out,
            &skills,
            &dispatch,
            ProbePolicy::default(),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let items = h.spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert_eq!(items[0].run_id, "run-keep");
        assert_eq!(items[0].spec_id, "host-skill");
        assert_failed_payload(&items[0].payload_json, "config_not_ready", "host-skill");

        let egress: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(h.data_dir.join("last_result.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(egress["source"], "dispatch");
        assert_eq!(egress["status"], "failed");
        assert_eq!(egress["run_id"], "run-keep");
        assert_eq!(egress["payload"]["reason"], "config_not_ready");
    }

    #[tokio::test]
    async fn dispatch_unknown_spec_reports_spec_not_found() {
        let h = harness();
        apply_config(
            &h.state,
            1,
            &skill_specs("host-skill"),
            "[]",
            ProbePolicy::default(),
        )
        .await
        .unwrap();

        let dispatch = Dispatch {
            run_id: "run-missing".into(),
            spec_id: "other-spec".into(),
            params_json: "{}".into(),
        };
        let skills = idle_skills(&h.data_dir).await;
        let result = run_dispatch(
            "node-1",
            &h.state,
            &h.spool,
            &h.data_dir,
            &h.out,
            &skills,
            &dispatch,
            ProbePolicy::default(),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let items = h.spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "failed");
        assert_eq!(items[0].run_id, "run-missing");
        assert_eq!(items[0].spec_id, "other-spec");
        assert_failed_payload(&items[0].payload_json, "spec_not_found", "other-spec");
    }

    #[tokio::test]
    async fn pull_config_sends_node_ready_after_apply() {
        let h = harness();
        let (tx, mut rx) = mpsc::channel(4);
        *h.out.write().await = Some(tx);
        let msg = ServerMessage {
            request_id: "pull-1".into(),
            body: Some(server_message::Body::PullConfig(PullConfigResponse {
                config_generation: 7,
                specs_json: skill_specs("host-skill"),
                schedules_json: "[]".into(),
            })),
        };
        let skills = idle_skills(&h.data_dir).await;
        on_server(
            "node-1",
            &h.state,
            &h.spool,
            &h.data_dir,
            &h.out,
            &skills,
            ProbePolicy::default(),
            msg,
        )
        .await
        .unwrap();

        {
            let st = h.state.read().await;
            assert!(st.config_loaded);
            assert_eq!(st.config_generation, 7);
            assert!(st.specs.contains_key("host-skill"));
        }

        let sent = rx.try_recv().expect("NodeReady");
        assert!(rx.try_recv().is_err(), "unexpected extra client message");
        match sent.body {
            Some(client_message::Body::NodeReady(ready)) => {
                assert_eq!(ready.node_id, "node-1");
                assert_eq!(ready.config_generation, 7);
            }
            other => panic!("expected NodeReady, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn queued_dispatch_after_config_runs_ok() {
        let h = harness();
        apply_config(
            &h.state,
            2,
            &skill_specs("host-skill"),
            "[]",
            ProbePolicy::default(),
        )
        .await
        .unwrap();

        let dispatch = Dispatch {
            run_id: "run-ok".into(),
            spec_id: "host-skill".into(),
            params_json: String::new(),
        };
        let skills = idle_skills(&h.data_dir).await;
        let result = run_dispatch(
            "node-1",
            &h.state,
            &h.spool,
            &h.data_dir,
            &h.out,
            &skills,
            &dispatch,
            ProbePolicy::default(),
        )
        .await;
        assert!(result.is_ok(), "{result:?}");

        let items = h.spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "ok");
        assert_eq!(items[0].run_id, "run-ok");
        assert_eq!(items[0].spec_id, "host-skill");
    }

    #[tokio::test]
    async fn dispatch_installed_skill_lands_on_the_spool() {
        use novbot_node::skills::{
            current_platform, pack_skill, sha256_hex, ArtifactSource, DesiredSet, DesiredSkill,
            Fetched, InstallLimits, PackSpec, SkillPolicy, ABI,
        };
        use std::collections::BTreeMap;
        use std::sync::Mutex;

        const GUEST: &[u8] = include_bytes!("../../novbot-skill-runtime/guest/skill.wasm");

        struct BytesSource {
            files: Mutex<BTreeMap<String, Vec<u8>>>,
        }

        #[async_trait::async_trait]
        impl ArtifactSource for BytesSource {
            async fn fetch(
                &self,
                sha256: &str,
                ticket: &str,
                offset: u64,
            ) -> Result<Fetched, String> {
                if ticket.is_empty() {
                    return Err("missing fetch ticket".into());
                }
                let bytes = self
                    .files
                    .lock()
                    .unwrap()
                    .get(sha256)
                    .cloned()
                    .ok_or_else(|| "artifact missing".to_string())?;
                let start = usize::try_from(offset)
                    .unwrap_or(usize::MAX)
                    .min(bytes.len());
                Ok(Fetched {
                    total_size: bytes.len() as u64,
                    data: bytes[start..].to_vec(),
                })
            }
        }

        let bytes = pack_skill(&PackSpec {
            name: "guest-info".into(),
            version: "1.0.0".into(),
            wasm: GUEST.to_vec(),
            grants: vec![("sys.info.read".into(), None)],
            platforms: vec![current_platform()],
            timeout_ms: Some(5_000),
            memory_mb: Some(32),
        });
        let sha = sha256_hex(&bytes);
        let source = std::sync::Arc::new(BytesSource {
            files: Mutex::new(BTreeMap::from([(sha.clone(), bytes.clone())])),
        });
        let h = harness();
        let skills = novbot_node::skills::SkillHost::open(
            &h.data_dir,
            source,
            InstallLimits {
                max_attempts: 2,
                max_elapsed: std::time::Duration::from_secs(120),
                min_backoff: std::time::Duration::ZERO,
                max_backoff: std::time::Duration::ZERO,
                ready_bound: std::time::Duration::from_millis(20),
                sleeper: std::sync::Arc::new(|_| Box::pin(async {})),
            },
        )
        .await
        .expect("skill host");
        let snap = skills
            .reconcile(DesiredSet {
                generation: 1,
                policy: SkillPolicy::default(),
                skills: vec![DesiredSkill {
                    name: "guest-info".into(),
                    version: "1.0.0".into(),
                    sha256: sha.clone(),
                    size_bytes: bytes.len() as u64,
                    abi: ABI.into(),
                    capabilities_sha256: String::new(),
                    signature_envelope: Vec::new(),
                    fetch_ticket: format!("ticket-{sha}"),
                }],
            })
            .await;
        assert_eq!(snap.skills[0].state, "installed", "{snap:?}");
        assert_eq!(skills.wasm_run_entries(), 0);

        apply_config(
            &h.state,
            3,
            &serde_json::json!([{
                "id": "guest-skill",
                "kind": "skill",
                "params": {"skill": "guest-info", "arguments": {"op": "info"}}
            }])
            .to_string(),
            "[]",
            ProbePolicy::default(),
        )
        .await
        .unwrap();
        let dispatch = Dispatch {
            run_id: "run-guest".into(),
            spec_id: "guest-skill".into(),
            params_json: String::new(),
        };
        run_dispatch(
            "node-1",
            &h.state,
            &h.spool,
            &h.data_dir,
            &h.out,
            &skills,
            &dispatch,
            ProbePolicy::default(),
        )
        .await
        .unwrap();
        let items = h.spool.drain().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "ok");
        assert_eq!(items[0].run_id, "run-guest");
        let payload: serde_json::Value = serde_json::from_str(&items[0].payload_json).unwrap();
        assert_eq!(payload["skill"]["name"], "guest-info");
        assert_eq!(payload["skill"]["version"], "1.0.0");
        assert!(payload["hostname"]
            .as_str()
            .is_some_and(|name| !name.is_empty()));
        assert_eq!(skills.wasm_run_entries(), 1);
    }
}
