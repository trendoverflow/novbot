// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::db::Db;
use crate::hub::Hub;
use anyhow::Result;
use chrono::{TimeZone, Utc};
use futures::Stream;
use novbot_proto::control_server::{Control, ControlServer};
use novbot_proto::{
    client_message, server_message, Ack, ClientMessage, Dispatch, ErrorResponse, HeartbeatResponse,
    PullConfigResponse, RegisterResponse, ReportResultResponse, ServerMessage,
};
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct ControlSvc {
    db: Db,
    hub: Hub,
    bootstrap_token: Option<String>,
    ready_timeout: Duration,
}

impl ControlSvc {
    pub fn new(db: Db, hub: Hub, bootstrap_token: Option<String>, ready_timeout: Duration) -> Self {
        Self {
            db,
            hub,
            bootstrap_token,
            ready_timeout,
        }
    }
}

/// Gates queued-dispatch delivery for one control session.
///
/// - Unbound: not ready, no deadline.
/// - Bound (`on_bound`): the session has a node id, from Register or from
///   Heartbeat / PullConfig / ReportResult when Register never arrived. The node
///   is not Hub-registered, so live HTTP dispatches enqueue, and rows in
///   `pending_dispatches` stay there. Deadline is `now + ready_timeout`.
/// - Ready: `on_node_ready` with `acked_gen >= center_gen`, or `on_timeout` at
///   or after the deadline. Both return true only on that transition so the
///   caller registers the node and flushes once. `deadline()` is then `None`.
///   A lower acked generation stays bound (the node pulls again and retries).
///
/// Rebinding to a different node id is a new gate; the session replaces this value.
struct ReadyGate {
    timeout: Duration,
    bound_at: Option<Instant>,
    ready: bool,
}

impl ReadyGate {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            bound_at: None,
            ready: false,
        }
    }

    fn on_bound(&mut self, now: Instant) {
        if self.ready || self.bound_at.is_some() {
            return;
        }
        self.bound_at = Some(now);
    }

    /// True only on the transition to ready, which is the signal to flush.
    fn on_node_ready(&mut self, acked_gen: i64, center_gen: i64) -> bool {
        if self.ready || acked_gen < center_gen {
            return false;
        }
        self.ready = true;
        true
    }

    fn deadline(&self) -> Option<Instant> {
        if self.ready {
            None
        } else {
            self.bound_at.map(|t| t + self.timeout)
        }
    }

    /// True only on the transition to ready at or after [`Self::deadline`].
    fn on_timeout(&mut self, now: Instant) -> bool {
        if self.ready {
            return false;
        }
        match self.deadline() {
            Some(deadline) if now >= deadline => {
                self.ready = true;
                true
            }
            _ => false,
        }
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

type OutStream = Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl Control for ControlSvc {
    type SessionStream = OutStream;

    async fn session(
        &self,
        request: Request<tonic::Streaming<ClientMessage>>,
    ) -> Result<Response<Self::SessionStream>, Status> {
        let mut inbound = request.into_inner();
        let (tx, rx) = mpsc::channel::<Result<ServerMessage, Status>>(64);
        let (push_tx, mut push_rx) = mpsc::channel::<ServerMessage>(64);
        let db = self.db.clone();
        let hub = self.hub.clone();
        let expected = self.bootstrap_token.clone();
        let ready_timeout = self.ready_timeout;

        tokio::spawn(async move {
            // Bound until NodeReady or ready_timeout: not in the Hub, queue not flushed.
            // See ReadyGate. Once ready, each client message also drains the queue.
            let mut bound_node: Option<String> = None;
            let mut gate = ReadyGate::new(ready_timeout);
            loop {
                let timer_deadline = gate.deadline();
                tokio::select! {
                    frame = inbound.next() => {
                        let Some(frame) = frame else { break; };
                        let msg = match frame {
                            Ok(m) => m,
                            Err(e) => {
                                let _ = tx.send(Err(e)).await;
                                break;
                            }
                        };
                        let node_ready = match msg.body.as_ref() {
                            Some(client_message::Body::NodeReady(ready)) => {
                                Some((ready.node_id.clone(), ready.config_generation))
                            }
                            _ => None,
                        };
                        let node_hint = client_node_id(&msg);
                        let (reply, node_id) =
                            handle(&db, expected.as_deref(), msg, node_hint.as_deref()).await;

                        if let Some(nid) = node_id {
                            if bound_node.as_deref() != Some(nid.as_str()) {
                                if let Some(old) = bound_node.take() {
                                    hub.unregister(&old, &push_tx).await;
                                }
                                bound_node = Some(nid);
                                gate = ReadyGate::new(ready_timeout);
                                gate.on_bound(Instant::now());
                            }
                        }

                        let mut flushed_on_transition = false;
                        if let Some((ready_nid, acked_gen)) = node_ready {
                            match bound_node.as_deref() {
                                Some(bound) if bound == ready_nid => {
                                    flushed_on_transition = apply_node_ready(
                                        &db,
                                        &hub,
                                        &mut gate,
                                        bound,
                                        acked_gen,
                                        &push_tx,
                                        &tx,
                                    )
                                    .await;
                                }
                                Some(bound) => {
                                    tracing::warn!(
                                        bound_node = %bound,
                                        node_id = %ready_nid,
                                        "ignoring NodeReady for a different node id"
                                    );
                                }
                                None => {
                                    tracing::warn!(
                                        node_id = %ready_nid,
                                        "ignoring NodeReady from a session that has not registered"
                                    );
                                }
                            }
                        }

                        if gate.is_ready() && !flushed_on_transition {
                            if let Some(nid) = bound_node.clone() {
                                flush_pending(&db, &nid, &tx).await;
                            }
                        }

                        if tx.send(Ok(reply)).await.is_err() {
                            break;
                        }
                    }
                    push = push_rx.recv() => {
                        let Some(msg) = push else { break; };
                        if tx.send(Ok(msg)).await.is_err() {
                            break;
                        }
                    }
                    _ = wait_ready_deadline(timer_deadline) => {
                        let now = match timer_deadline {
                            Some(due) => Instant::now().max(due),
                            None => Instant::now(),
                        };
                        if gate.on_timeout(now) {
                            if let Some(nid) = bound_node.clone() {
                                flush_for_timeout(&hub, &db, &nid, &push_tx, &tx).await;
                            }
                        }
                    }
                }
            }
            if let Some(nid) = bound_node {
                hub.unregister(&nid, &push_tx).await;
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

fn client_node_id(msg: &ClientMessage) -> Option<String> {
    match msg.body.as_ref()? {
        client_message::Body::Register(r) => Some(r.node_id.clone()),
        client_message::Body::Heartbeat(r) => Some(r.node_id.clone()),
        client_message::Body::PullConfig(r) => Some(r.node_id.clone()),
        client_message::Body::ReportResult(r) => Some(r.node_id.clone()),
        // NodeReady must not bind a session. The loop applies it only when already bound.
        client_message::Body::NodeReady(_) => None,
        client_message::Body::Ack(_) => None,
    }
}

async fn handle(
    db: &Db,
    expected_token: Option<&str>,
    msg: ClientMessage,
    _node_hint: Option<&str>,
) -> (ServerMessage, Option<String>) {
    let request_id = msg.request_id;
    let Some(body) = msg.body else {
        return (err_msg(request_id, "empty", "missing body"), None);
    };

    match body {
        client_message::Body::Register(req) => {
            let node_id = req.node_id.clone();
            if let Some(tok) = expected_token {
                if !tok.is_empty() && req.bootstrap_token != tok {
                    return (
                        ServerMessage {
                            request_id,
                            body: Some(server_message::Body::Register(RegisterResponse {
                                node_id: req.node_id,
                                accepted: false,
                                message: "invalid bootstrap token".into(),
                            })),
                        },
                        None,
                    );
                }
            }
            let labels: std::collections::HashMap<String, String> =
                req.labels.into_iter().collect();
            let (accepted, message) = match db
                .upsert_node(&req.node_id, &req.hostname, &req.version, &labels)
                .await
            {
                Ok(()) => (true, "registered".to_string()),
                Err(e) => (false, e.to_string()),
            };
            let bound = if accepted {
                Some(node_id.clone())
            } else {
                None
            };
            (
                ServerMessage {
                    request_id,
                    body: Some(server_message::Body::Register(RegisterResponse {
                        node_id: req.node_id,
                        accepted,
                        message,
                    })),
                },
                bound,
            )
        }
        client_message::Body::Heartbeat(req) => {
            let node_id = req.node_id.clone();
            let _ = db.touch_node(&req.node_id).await;
            let center_config_generation = db.config_generation(&req.node_id).await.unwrap_or(0);
            (
                ServerMessage {
                    request_id,
                    body: Some(server_message::Body::Heartbeat(HeartbeatResponse {
                        ok: true,
                        center_config_generation,
                    })),
                },
                Some(node_id),
            )
        }
        client_message::Body::PullConfig(req) => {
            let node_id = req.node_id.clone();
            let _ = db.touch_node(&req.node_id).await;
            let reply = match db.get_config(&req.node_id).await {
                Ok(Some(cfg)) => ServerMessage {
                    request_id,
                    body: Some(server_message::Body::PullConfig(PullConfigResponse {
                        config_generation: cfg.config_generation,
                        specs_json: cfg.specs_json,
                        schedules_json: cfg.schedules_json,
                    })),
                },
                Ok(None) => ServerMessage {
                    request_id,
                    body: Some(server_message::Body::PullConfig(PullConfigResponse {
                        config_generation: 0,
                        specs_json: "[]".into(),
                        schedules_json: "[]".into(),
                    })),
                },
                Err(e) => err_msg(request_id, "db", &e.to_string()),
            };
            (reply, Some(node_id))
        }
        client_message::Body::ReportResult(req) => {
            let node_id = req.node_id.clone();
            let observed = Utc
                .timestamp_millis_opt(req.observed_at_unix_ms)
                .single()
                .unwrap_or_else(Utc::now);
            let accepted = match db
                .insert_result(
                    &req.node_id,
                    &req.run_id,
                    &req.spec_id,
                    &req.status,
                    &req.payload_json,
                    observed,
                )
                .await
            {
                Ok(()) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "insert_result failed");
                    false
                }
            };
            (
                ServerMessage {
                    request_id,
                    body: Some(server_message::Body::ReportResult(ReportResultResponse {
                        accepted,
                    })),
                },
                Some(node_id),
            )
        }
        client_message::Body::NodeReady(_) => (
            ServerMessage {
                request_id: request_id.clone(),
                body: Some(server_message::Body::Ack(Ack {
                    of_request_id: request_id,
                    ok: true,
                    message: "ok".into(),
                })),
            },
            None,
        ),
        client_message::Body::Ack(a) => (
            ServerMessage {
                request_id,
                body: Some(server_message::Body::Ack(Ack {
                    of_request_id: a.of_request_id,
                    ok: true,
                    message: "ok".into(),
                })),
            },
            None,
        ),
    }
}

async fn wait_ready_deadline(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn flush_pending(
    db: &Db,
    node_id: &str,
    tx: &mpsc::Sender<Result<ServerMessage, Status>>,
) -> usize {
    let pending = match db.take_pending_dispatches(node_id).await {
        Ok(pending) => pending,
        Err(e) => {
            tracing::warn!(error = %e, %node_id, "take_pending_dispatches failed");
            return 0;
        }
    };
    let mut flushed = 0;
    for (run_id, spec_id, params_json) in pending {
        let dispatch = ServerMessage {
            request_id: uuid::Uuid::new_v4().to_string(),
            body: Some(server_message::Body::Dispatch(Dispatch {
                run_id,
                spec_id,
                params_json,
            })),
        };
        if tx.send(Ok(dispatch)).await.is_err() {
            break;
        }
        flushed += 1;
    }
    flushed
}

/// Hub registration happens before the queue is taken so a live HTTP dispatch
/// during the flush is delivered instead of racing ahead of config.
async fn activate_ready(
    hub: &Hub,
    db: &Db,
    node_id: &str,
    push_tx: &mpsc::Sender<ServerMessage>,
    tx: &mpsc::Sender<Result<ServerMessage, Status>>,
) -> usize {
    hub.register(node_id.to_string(), push_tx.clone()).await;
    flush_pending(db, node_id, tx).await
}

async fn flush_for_timeout(
    hub: &Hub,
    db: &Db,
    node_id: &str,
    push_tx: &mpsc::Sender<ServerMessage>,
    tx: &mpsc::Sender<Result<ServerMessage, Status>>,
) {
    let flushed = activate_ready(hub, db, node_id, push_tx, tx).await;
    tracing::warn!(
        node_id,
        flushed,
        "node did not send NodeReady in time; flushing queued dispatches"
    );
}

async fn apply_node_ready(
    db: &Db,
    hub: &Hub,
    gate: &mut ReadyGate,
    node_id: &str,
    acked_gen: i64,
    push_tx: &mpsc::Sender<ServerMessage>,
    tx: &mpsc::Sender<Result<ServerMessage, Status>>,
) -> bool {
    let center_gen = db.config_generation(node_id).await.unwrap_or(0);
    let was_ready = gate.is_ready();
    if gate.on_node_ready(acked_gen, center_gen) {
        let flushed = activate_ready(hub, db, node_id, push_tx, tx).await;
        tracing::info!(
            node_id,
            acked_generation = acked_gen,
            flushed,
            "node ready; flushed queued dispatches"
        );
        true
    } else if !was_ready {
        tracing::info!(
            node_id,
            acked_generation = acked_gen,
            center_generation = center_gen,
            "NodeReady generation is behind center; staying not ready"
        );
        false
    } else {
        false
    }
}

fn err_msg(request_id: String, code: &str, message: &str) -> ServerMessage {
    ServerMessage {
        request_id,
        body: Some(server_message::Body::Error(ErrorResponse {
            code: code.into(),
            message: message.into(),
        })),
    }
}

pub async fn serve(
    addr: SocketAddr,
    db: Db,
    hub: Hub,
    bootstrap_token: Option<String>,
    ready_timeout: Duration,
) -> Result<()> {
    let svc = ControlSvc::new(db, hub, bootstrap_token, ready_timeout);
    tracing::info!(%addr, "gRPC Control listening");
    tonic::transport::Server::builder()
        .add_service(ControlServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ControlSvc, ReadyGate};
    use crate::db::Db;
    use crate::hub::Hub;
    use novbot_proto::control_client::ControlClient;
    use novbot_proto::control_server::ControlServer;
    use novbot_proto::{
        client_message, server_message, ClientMessage, Dispatch, NodeReady, PullConfigRequest,
        RegisterRequest, ServerMessage,
    };
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
    use tokio_stream::StreamExt;

    #[test]
    fn bind_is_not_ready_and_starts_deadline() {
        let timeout = Duration::from_secs(120);
        let mut gate = ReadyGate::new(timeout);
        assert!(!gate.is_ready());
        assert!(gate.deadline().is_none());
        assert!(!gate.on_timeout(Instant::now()));

        let now = Instant::now();
        gate.on_bound(now);
        assert!(!gate.is_ready());
        assert_eq!(gate.deadline(), Some(now + timeout));
        // A later message for the same node must not restart the wait.
        gate.on_bound(now + Duration::from_secs(50));
        assert_eq!(gate.deadline(), Some(now + timeout));
    }

    #[test]
    fn node_ready_with_current_or_newer_generation_flushes_once() {
        let now = Instant::now();

        let mut equal = ReadyGate::new(Duration::from_secs(120));
        equal.on_bound(now);
        assert!(equal.on_node_ready(5, 5));
        assert!(equal.is_ready());
        assert!(equal.deadline().is_none());
        assert!(!equal.on_node_ready(5, 5));
        assert!(!equal.on_node_ready(9, 5));

        let mut newer = ReadyGate::new(Duration::from_secs(120));
        newer.on_bound(now);
        assert!(newer.on_node_ready(6, 5));
        assert!(newer.is_ready());
        assert!(!newer.on_node_ready(6, 5));
    }

    #[test]
    fn node_ready_with_lower_generation_stays_not_ready() {
        let timeout = Duration::from_secs(120);
        let now = Instant::now();
        let mut gate = ReadyGate::new(timeout);
        gate.on_bound(now);
        assert!(!gate.on_node_ready(4, 5));
        assert!(!gate.is_ready());
        assert_eq!(gate.deadline(), Some(now + timeout));
        assert!(gate.on_node_ready(5, 5));
        assert!(!gate.on_node_ready(5, 5));
    }

    #[test]
    fn timeout_before_deadline_is_not_ready() {
        let timeout = Duration::from_secs(10);
        let mut gate = ReadyGate::new(timeout);
        let now = Instant::now();
        gate.on_bound(now);
        assert!(!gate.on_timeout(now));
        assert!(!gate.on_timeout(now + Duration::from_secs(9)));
        let deadline = gate.deadline().expect("deadline");
        assert!(!gate.on_timeout(deadline - Duration::from_nanos(1)));
        assert!(!gate.is_ready());
        assert_eq!(gate.deadline(), Some(deadline));
    }

    #[test]
    fn timeout_at_or_after_deadline_is_ready() {
        let timeout = Duration::from_secs(10);
        let now = Instant::now();

        let mut at = ReadyGate::new(timeout);
        at.on_bound(now);
        let deadline = at.deadline().expect("deadline");
        assert!(at.on_timeout(deadline));
        assert!(at.is_ready());
        assert!(at.deadline().is_none());
        assert!(!at.on_timeout(deadline + Duration::from_secs(1)));

        let mut after = ReadyGate::new(timeout);
        after.on_bound(now);
        assert!(after.on_timeout(now + timeout + Duration::from_millis(1)));
        assert!(after.is_ready());
        assert!(!after.on_timeout(now + timeout + Duration::from_secs(30)));
    }

    fn migrate_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    struct Running {
        addr: SocketAddr,
        db: Db,
        hub: Hub,
        node_id: String,
    }

    struct Opened {
        _client: ControlClient<tonic::transport::Channel>,
        tx: mpsc::Sender<ClientMessage>,
        inbound: tonic::Streaming<ServerMessage>,
    }

    async fn running(test_name: &str, ready_timeout: Duration) -> Option<Running> {
        let url = std::env::var("NOVBOT_TEST_DATABASE_URL").unwrap_or_default();
        if url.trim().is_empty() {
            eprintln!("skipping {test_name}: NOVBOT_TEST_DATABASE_URL is unset or empty");
            return None;
        }
        let db = Db::connect(url.trim())
            .await
            .expect("connect NOVBOT_TEST_DATABASE_URL");
        {
            let _guard = migrate_lock().lock().await;
            db.migrate().await.expect("migrate");
        }
        let hub = Hub::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let incoming = TcpListenerStream::new(listener);
        let svc = ControlSvc::new(db.clone(), hub.clone(), None, ready_timeout);
        tokio::spawn(async move {
            if let Err(error) = tonic::transport::Server::builder()
                .add_service(ControlServer::new(svc))
                .serve_with_incoming(incoming)
                .await
            {
                tracing::error!(%error, "test control server exited");
            }
        });
        Some(Running {
            addr,
            db,
            hub,
            node_id: format!("t12-{}", uuid::Uuid::new_v4()),
        })
    }

    async fn open_session(addr: SocketAddr) -> Opened {
        let endpoint = format!("http://{addr}");
        let mut last_err = String::new();
        for _ in 0..50 {
            match ControlClient::connect(endpoint.clone()).await {
                Ok(mut client) => {
                    let (tx, rx) = mpsc::channel(32);
                    match client.session(ReceiverStream::new(rx)).await {
                        Ok(response) => {
                            return Opened {
                                _client: client,
                                tx,
                                inbound: response.into_inner(),
                            };
                        }
                        Err(e) => last_err = e.to_string(),
                    }
                }
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("open session {endpoint}: {last_err}");
    }

    fn register_msg(node_id: &str) -> ClientMessage {
        ClientMessage {
            request_id: "reg".into(),
            body: Some(client_message::Body::Register(RegisterRequest {
                node_id: node_id.to_string(),
                hostname: "test-host".into(),
                version: "test".into(),
                bootstrap_token: String::new(),
                labels: HashMap::new(),
            })),
        }
    }

    fn pull_msg(node_id: &str) -> ClientMessage {
        ClientMessage {
            request_id: "pull".into(),
            body: Some(client_message::Body::PullConfig(PullConfigRequest {
                node_id: node_id.to_string(),
                known_generation: 0,
            })),
        }
    }

    fn ready_msg(node_id: &str, generation: i64) -> ClientMessage {
        ClientMessage {
            request_id: format!("ready-{generation}"),
            body: Some(client_message::Body::NodeReady(NodeReady {
                node_id: node_id.to_string(),
                config_generation: generation,
            })),
        }
    }

    async fn seed_queued(db: &Db, node_id: &str, run_id: &str) -> i64 {
        db.upsert_node(node_id, "host", "test", &HashMap::new())
            .await
            .expect("upsert");
        let cfg = db
            .put_config(
                node_id,
                r#"[{"id":"host-skill","kind":"skill","params":{"skill":"host_info"}}]"#,
                Some("[]"),
            )
            .await
            .expect("put_config");
        assert!(cfg.config_generation >= 1);
        db.enqueue_dispatch(node_id, run_id, "host-skill", "{}")
            .await
            .expect("enqueue");
        cfg.config_generation
    }

    async fn recv(inbound: &mut tonic::Streaming<ServerMessage>) -> ServerMessage {
        match tokio::time::timeout(Duration::from_secs(5), inbound.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => panic!("stream error: {e}"),
            Ok(None) => panic!("stream closed"),
            Err(_) => panic!("timed out waiting for server message"),
        }
    }

    fn expect_register(msg: ServerMessage) {
        match msg.body {
            Some(server_message::Body::Register(resp)) => {
                assert!(resp.accepted, "{}", resp.message);
            }
            other => panic!("expected RegisterResponse, got {other:?}"),
        }
    }

    fn expect_pull(msg: ServerMessage, generation: i64) {
        match msg.body {
            Some(server_message::Body::PullConfig(resp)) => {
                assert_eq!(resp.config_generation, generation);
            }
            other => panic!("expected PullConfigResponse, got {other:?}"),
        }
    }

    fn is_dispatch(msg: &ServerMessage) -> bool {
        matches!(msg.body, Some(server_message::Body::Dispatch(_)))
    }

    async fn assert_no_dispatch_for(inbound: &mut tonic::Streaming<ServerMessage>, dur: Duration) {
        let start = Instant::now();
        while start.elapsed() < dur {
            let remain = dur.saturating_sub(start.elapsed());
            if remain.is_zero() {
                break;
            }
            match tokio::time::timeout(remain, inbound.next()).await {
                Ok(Some(Ok(msg))) => {
                    assert!(
                        !is_dispatch(&msg),
                        "unexpected dispatch before the node was ready: {msg:?}"
                    );
                }
                Ok(Some(Err(e))) => panic!("stream error: {e}"),
                Ok(None) => panic!("stream closed"),
                Err(_) => break,
            }
        }
    }

    async fn recv_dispatch(
        inbound: &mut tonic::Streaming<ServerMessage>,
        dur: Duration,
    ) -> Dispatch {
        let start = Instant::now();
        while start.elapsed() < dur {
            let remain = dur.saturating_sub(start.elapsed());
            if remain.is_zero() {
                break;
            }
            match tokio::time::timeout(remain, inbound.next()).await {
                Ok(Some(Ok(msg))) => {
                    if let Some(server_message::Body::Dispatch(dispatch)) = msg.body {
                        return dispatch;
                    }
                }
                Ok(Some(Err(e))) => panic!("stream error: {e}"),
                Ok(None) => panic!("stream closed"),
                Err(_) => break,
            }
        }
        panic!("no dispatch within {dur:?}");
    }

    #[tokio::test]
    async fn queued_dispatch_waits_for_node_ready() {
        let Some(srv) = running(
            "queued_dispatch_waits_for_node_ready",
            Duration::from_secs(30),
        )
        .await
        else {
            return;
        };
        let gen = seed_queued(&srv.db, &srv.node_id, "R").await;
        let mut opened = open_session(srv.addr).await;
        let mut order = Vec::new();

        opened
            .tx
            .send(register_msg(&srv.node_id))
            .await
            .expect("send register");
        expect_register(recv(&mut opened.inbound).await);
        order.push("register");

        opened
            .tx
            .send(pull_msg(&srv.node_id))
            .await
            .expect("send pull");
        expect_pull(recv(&mut opened.inbound).await, gen);
        order.push("pull");

        assert_no_dispatch_for(&mut opened.inbound, Duration::from_millis(500)).await;

        opened
            .tx
            .send(ready_msg(&srv.node_id, gen))
            .await
            .expect("send ready");
        let dispatch = recv_dispatch(&mut opened.inbound, Duration::from_secs(2)).await;
        assert_eq!(dispatch.run_id, "R");
        assert_eq!(dispatch.spec_id, "host-skill");
        order.push("dispatch");
        assert_eq!(order, ["register", "pull", "dispatch"]);

        let left = srv
            .db
            .take_pending_dispatches(&srv.node_id)
            .await
            .expect("take");
        assert!(left.is_empty(), "pending row still present: {left:?}");

        let live = srv
            .hub
            .send(
                &srv.node_id,
                ServerMessage {
                    request_id: "live-after".into(),
                    body: Some(server_message::Body::Dispatch(Dispatch {
                        run_id: "live-after".into(),
                        spec_id: "host-skill".into(),
                        params_json: "{}".into(),
                    })),
                },
            )
            .await;
        assert!(live, "hub should deliver once the session is ready");
    }

    #[tokio::test]
    async fn node_ready_with_stale_generation_does_not_flush() {
        let Some(srv) = running(
            "node_ready_with_stale_generation_does_not_flush",
            Duration::from_secs(30),
        )
        .await
        else {
            return;
        };
        let gen = seed_queued(&srv.db, &srv.node_id, "R").await;
        assert!(gen >= 1);
        let mut opened = open_session(srv.addr).await;

        opened
            .tx
            .send(register_msg(&srv.node_id))
            .await
            .expect("send register");
        expect_register(recv(&mut opened.inbound).await);

        opened
            .tx
            .send(ready_msg(&srv.node_id, gen - 1))
            .await
            .expect("send stale ready");
        assert_no_dispatch_for(&mut opened.inbound, Duration::from_millis(500)).await;

        opened
            .tx
            .send(ready_msg(&srv.node_id, gen))
            .await
            .expect("send current ready");
        let dispatch = recv_dispatch(&mut opened.inbound, Duration::from_secs(2)).await;
        assert_eq!(dispatch.run_id, "R");
        assert_eq!(dispatch.spec_id, "host-skill");

        let left = srv
            .db
            .take_pending_dispatches(&srv.node_id)
            .await
            .expect("take");
        assert!(left.is_empty(), "pending row still present: {left:?}");
    }

    #[tokio::test]
    async fn queued_dispatch_flushed_after_ready_timeout() {
        let Some(srv) = running(
            "queued_dispatch_flushed_after_ready_timeout",
            Duration::from_secs(1),
        )
        .await
        else {
            return;
        };
        seed_queued(&srv.db, &srv.node_id, "R").await;
        let mut opened = open_session(srv.addr).await;

        let started = Instant::now();
        opened
            .tx
            .send(register_msg(&srv.node_id))
            .await
            .expect("send register");
        let dispatch = recv_dispatch(&mut opened.inbound, Duration::from_secs(5)).await;
        let elapsed = started.elapsed();
        assert_eq!(dispatch.run_id, "R");
        assert_eq!(dispatch.spec_id, "host-skill");
        // The 1s timer starts when Register is accepted, just after this send.
        assert!(
            elapsed >= Duration::from_millis(900),
            "dispatch flushed too early ({elapsed:?})"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "dispatch flushed too late ({elapsed:?})"
        );

        let left = srv
            .db
            .take_pending_dispatches(&srv.node_id)
            .await
            .expect("take");
        assert!(left.is_empty(), "pending row still present: {left:?}");
    }

    #[tokio::test]
    async fn live_dispatch_before_ready_is_queued() {
        let Some(srv) = running(
            "live_dispatch_before_ready_is_queued",
            Duration::from_secs(30),
        )
        .await
        else {
            return;
        };
        let mut opened = open_session(srv.addr).await;
        opened
            .tx
            .send(register_msg(&srv.node_id))
            .await
            .expect("send register");
        expect_register(recv(&mut opened.inbound).await);

        let live = srv
            .hub
            .send(
                &srv.node_id,
                ServerMessage {
                    request_id: "live-before".into(),
                    body: Some(server_message::Body::Dispatch(Dispatch {
                        run_id: "live-before".into(),
                        spec_id: "host-skill".into(),
                        params_json: "{}".into(),
                    })),
                },
            )
            .await;
        assert!(
            !live,
            "hub send must fail while the session is bound but not ready"
        );
    }
}
