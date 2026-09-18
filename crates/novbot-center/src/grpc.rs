// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::db::Db;
use anyhow::Result;
use chrono::{TimeZone, Utc};
use futures::Stream;
use novbot_proto::control_server::{Control, ControlServer};
use novbot_proto::{
    client_message, server_message, Ack, ClientMessage, ErrorResponse, HeartbeatResponse,
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
    bootstrap_token: Option<String>,
}

impl ControlSvc {
    pub fn new(db: Db, bootstrap_token: Option<String>) -> Self {
        Self {
            db,
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
        let db = self.db.clone();
        let expected = self.bootstrap_token.clone();

        tokio::spawn(async move {
            while let Some(frame) = inbound.next().await {
                let msg = match frame {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                };
                let reply = handle(&db, expected.as_deref(), msg).await;
                if tx.send(Ok(reply)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
}

async fn handle(db: &Db, expected_token: Option<&str>, msg: ClientMessage) -> ServerMessage {
    let request_id = msg.request_id;
    let Some(body) = msg.body else {
        return err_msg(request_id, "empty", "missing body");
    };

    match body {
        client_message::Body::Register(req) => {
            if let Some(tok) = expected_token {
                if !tok.is_empty() && req.bootstrap_token != tok {
                    return ServerMessage {
                        request_id,
                        body: Some(server_message::Body::Register(RegisterResponse {
                            node_id: req.node_id,
                            accepted: false,
                            message: "invalid bootstrap token".into(),
                        })),
                    };
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
            ServerMessage {
                request_id,
                body: Some(server_message::Body::Register(RegisterResponse {
                    node_id: req.node_id,
                    accepted,
                    message,
                })),
            }
        }
        client_message::Body::Heartbeat(req) => {
            let _ = db.touch_node(&req.node_id).await;
            let center_config_generation = db.config_generation(&req.node_id).await.unwrap_or(0);
            ServerMessage {
                request_id,
                body: Some(server_message::Body::Heartbeat(HeartbeatResponse {
                    ok: true,
                    center_config_generation,
                })),
            }
        }
        client_message::Body::PullConfig(req) => {
            let _ = db.touch_node(&req.node_id).await;
            match db.get_config(&req.node_id).await {
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
            }
        }
        client_message::Body::ReportResult(req) => {
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
            ServerMessage {
                request_id,
                body: Some(server_message::Body::ReportResult(ReportResultResponse {
                    accepted,
                })),
            }
        }
        client_message::Body::Ack(a) => ServerMessage {
            request_id,
            body: Some(server_message::Body::Ack(Ack {
                of_request_id: a.of_request_id,
                ok: true,
                message: "ok".into(),
            })),
        },
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

pub async fn serve(addr: SocketAddr, db: Db, bootstrap_token: Option<String>) -> Result<()> {
    let svc = ControlSvc::new(db, bootstrap_token);
    tracing::info!(%addr, "gRPC Control listening");
    tonic::transport::Server::builder()
        .add_service(ControlServer::new(svc))
        .serve(addr)
        .await?;
    Ok(())
}
