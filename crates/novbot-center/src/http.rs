// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::db::{Db, NodeConfig, NodeRow, ResultRow};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/nodes/{id}/config", get(get_config).put(put_config))
        .route(
            "/v1/nodes/{id}/schedules",
            get(get_schedules).put(put_schedules),
        )
        .route("/v1/results", get(list_results))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"ok": true, "service": "novbot-center"}))
}

async fn list_nodes(State(st): State<AppState>) -> Result<Json<Vec<NodeRow>>, ApiError> {
    Ok(Json(st.db.list_nodes().await?))
}

async fn get_config(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<NodeConfig>, ApiError> {
    st.db
        .get_config(&id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("node config not found"))
}

#[derive(Debug, Deserialize)]
pub struct PutConfigBody {
    pub specs: Value,
    #[serde(default)]
    pub schedules: Option<Value>,
}

async fn put_config(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PutConfigBody>,
) -> Result<Json<NodeConfig>, ApiError> {
    let _ = st
        .db
        .upsert_node(&id, "", "http-admin", &HashMap::new())
        .await;
    let specs_json = serde_json::to_string(&body.specs)?;
    let schedules_json = body
        .schedules
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let cfg = st
        .db
        .put_config(&id, &specs_json, schedules_json.as_deref())
        .await?;
    Ok(Json(cfg))
}

async fn get_schedules(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let cfg = st
        .db
        .get_config(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("node config not found"))?;
    let schedules: Value =
        serde_json::from_str(&cfg.schedules_json).unwrap_or_else(|_| Value::Array(vec![]));
    Ok(Json(serde_json::json!({
        "node_id": id,
        "config_generation": cfg.config_generation,
        "schedules": schedules,
    })))
}

#[derive(Debug, Deserialize)]
pub struct PutSchedulesBody {
    pub schedules: Value,
}

async fn put_schedules(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PutSchedulesBody>,
) -> Result<Json<NodeConfig>, ApiError> {
    let schedules_json = serde_json::to_string(&body.schedules)?;
    let cfg = st.db.put_schedules(&id, &schedules_json).await?;
    Ok(Json(cfg))
}

#[derive(Debug, Deserialize)]
pub struct ResultsQuery {
    pub node_id: Option<String>,
    pub limit: Option<i64>,
}

async fn list_results(
    State(st): State<AppState>,
    Query(q): Query<ResultsQuery>,
) -> Result<Json<Vec<ResultRow>>, ApiError> {
    Ok(Json(
        st.db
            .list_results(q.node_id.as_deref(), q.limit.unwrap_or(50))
            .await?,
    ))
}

pub async fn serve(addr: SocketAddr, db: Db) -> anyhow::Result<()> {
    let app = router(AppState { db });
    tracing::info!(%addr, "HTTP API listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({"error": self.message})),
        )
            .into_response()
    }
}
