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
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::StreamExt;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct ControlSvc {
    db: Db,
    hub: Hub,
    bootstrap_token: Option<String>,
}

impl ControlSvc {
    pub fn new(db: Db, hub: Hub, bootstrap_token: Option<String>) -> Self {
        Self {
            db,
            hub,
            bootstrap_token,
        }
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

        tokio::spawn(async move {
            let mut bound_node: Option<String> = None;
            loop {
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
                        let node_hint = client_node_id(&msg);
                        let (reply, node_id) =
                            handle(&db, expected.as_deref(), msg, node_hint.as_deref()).await;

                        if let Some(nid) = node_id {
                            if bound_node.as_deref() != Some(nid.as_str()) {
                                if let Some(old) = bound_node.take() {
                                    hub.unregister(&old, &push_tx).await;
                                }
                                hub.register(nid.clone(), push_tx.clone()).await;
                                bound_node = Some(nid.clone());
                            }
                            // Deliver any queued DispatchCommands after handling the client msg.
                            if let Ok(pending) = db.take_pending_dispatches(&nid).await {
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
                                }
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
) -> Result<()> {
    let svc = ControlSvc::new(db, hub, bootstrap_token);
    tracing::info!(%addr, "gRPC Control listening");
    tonic::transport::Server::builder()
        .add_service(ControlServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}
