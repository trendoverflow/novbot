// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::db::{Db, NodeConfig, NodeRow, ResultRow};
use crate::hub::Hub;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use novbot_proto::{server_message, Dispatch, ServerMessage};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub hub: Hub,
    /// Env NOVBOT_LICENSE_KEY — if set, center is licensed (stub).
    pub license_env: Option<String>,
    /// Env NOVBOT_API_TOKEN — when set, HTTP `/v1` requires `Authorization: Bearer <token>`.
    pub api_token: Option<String>,
}

pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/nodes", get(list_nodes))
        .route("/nodes/{id}/config", get(get_config).put(put_config))
        .route(
            "/nodes/{id}/schedules",
            get(get_schedules).put(put_schedules),
        )
        .route("/nodes/{id}/dispatch", post(dispatch_command))
        .route("/results", get(list_results))
        .route("/license", get(get_license).put(put_license))
        .route("/skills", get(list_skills))
        .route("/fleet/skill-groups", get(list_skill_groups))
        .route("/fleet/skill-groups/push", post(push_skill_group))
        .route("/tokens", get(tokens_status).post(create_token))
        // EE / report paths — closed without license (M7 stub).
        .route("/ee/reports", get(ee_reports))
        .route("/ee/reports/{id}", get(ee_report_by_id))
        .route_layer(from_fn_with_state(state.clone(), require_api_token));

    Router::new()
        .route("/health", get(health))
        .nest("/v1", v1)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"ok": true, "service": "novbot-center"}))
}

async fn list_skills() -> impl IntoResponse {
    Json(serde_json::json!({
        "skills": novbot_core::list_skills(),
        "note": "Invoke via Spec kind skill / mcp_tool, or POST /v1/nodes/:id/dispatch",
    }))
}

async fn require_api_token(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Result<axum::response::Response, ApiError> {
    let Some(expected) = st
        .api_token
        .as_ref()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    else {
        return Ok(next.run(req).await);
    };

    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim() == expected)
        .unwrap_or(false);

    if !authorized {
        return Err(ApiError::unauthorized(
            "missing or invalid Authorization Bearer token (NOVBOT_API_TOKEN)",
        ));
    }
    Ok(next.run(req).await)
}

#[derive(Clone, Copy)]
struct SkillGroupDef {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    skills: &'static [&'static str],
}

fn skill_group_registry() -> &'static [SkillGroupDef] {
    &[
        SkillGroupDef {
            id: "host-basics",
            name: "Host basics",
            description: "host_info + echo (demo fleet push)",
            skills: &["host_info", "echo"],
        },
        SkillGroupDef {
            id: "env-sample",
            name: "Env sample",
            description: "env_get only",
            skills: &["env_get"],
        },
    ]
}

async fn list_skill_groups() -> impl IntoResponse {
    let groups: Vec<_> = skill_group_registry()
        .iter()
        .map(|g| {
            serde_json::json!({
                "id": g.id,
                "name": g.name,
                "description": g.description,
                "skills": g.skills,
            })
        })
        .collect();
    Json(serde_json::json!({
        "groups": groups,
        "note": "POST /v1/fleet/skill-groups/push with {group_id, node_ids[]} dispatches matching skill specs per node",
    }))
}

#[derive(Debug, Deserialize)]
pub struct PushSkillGroupBody {
    pub group_id: String,
    pub node_ids: Vec<String>,
}

fn resolve_spec_id_for_skill(specs: &Value, skill: &str) -> String {
    if let Some(arr) = specs.as_array() {
        for spec in arr {
            let kind = spec.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let id = spec.get("id").and_then(|i| i.as_str()).unwrap_or("");
            if kind == "skill" {
                let skill_name = spec
                    .get("params")
                    .and_then(|p| p.get("skill"))
                    .and_then(|s| s.as_str());
                if skill_name == Some(skill) && !id.is_empty() {
                    return id.to_string();
                }
            }
            if id == skill {
                return skill.to_string();
            }
        }
    }
    skill.to_string()
}

async fn push_skill_group(
    State(st): State<AppState>,
    Json(body): Json<PushSkillGroupBody>,
) -> Result<impl IntoResponse, ApiError> {
    let group = skill_group_registry()
        .iter()
        .find(|g| g.id == body.group_id)
        .ok_or_else(|| ApiError::not_found(format!("skill group '{}' not found", body.group_id)))?;

    if body.node_ids.is_empty() {
        return Err(ApiError::bad_request("node_ids must not be empty"));
    }

    let mut results = Vec::new();
    for node_id in &body.node_ids {
        let cfg = match st.db.get_config(node_id).await? {
            Some(c) => c,
            None => {
                for skill in group.skills {
                    results.push(serde_json::json!({
                        "node_id": node_id,
                        "skill": skill,
                        "status": "error",
                        "error": "node config not found; PUT config first",
                    }));
                }
                continue;
            }
        };
        let specs: Value =
            serde_json::from_str(&cfg.specs_json).unwrap_or_else(|_| Value::Array(vec![]));

        for skill in group.skills {
            let spec_id = resolve_spec_id_for_skill(&specs, skill);
            match dispatch_to_node(&st, node_id, &spec_id, None, None).await {
                Ok((run_id, delivered)) => results.push(serde_json::json!({
                    "node_id": node_id,
                    "skill": skill,
                    "spec_id": spec_id,
                    "run_id": run_id,
                    "status": "ok",
                    "delivered": delivered,
                })),
                Err(e) => results.push(serde_json::json!({
                    "node_id": node_id,
                    "skill": skill,
                    "spec_id": spec_id,
                    "status": "error",
                    "error": e.message,
                })),
            }
        }
    }

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "accepted": true,
            "group_id": group.id,
            "group_name": group.name,
            "skills": group.skills,
            "results": results,
        })),
    ))
}

async fn tokens_status(State(st): State<AppState>) -> impl IntoResponse {
    let required = st
        .api_token
        .as_ref()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false);
    Json(serde_json::json!({
        "auth_required": required,
        "stub": true,
        "note": "Tokens are client-side Bearer values. When NOVBOT_API_TOKEN is set, /v1 requires a matching Authorization header. POST /v1/tokens mints a paste-ready value (not stored server-side).",
    }))
}

async fn create_token() -> impl IntoResponse {
    let token = format!("nb_{}", Uuid::new_v4());
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "token": token,
            "stub": true,
            "note": "Paste into console Settings. To enforce, set center NOVBOT_API_TOKEN to the same value.",
        })),
    )
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
pub struct DispatchBody {
    /// Spec id to run (must exist in node config for skill/mcp/probe).
    pub spec_id: String,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub run_id: Option<String>,
}

async fn dispatch_to_node(
    st: &AppState,
    node_id: &str,
    spec_id: &str,
    params: Option<Value>,
    run_id: Option<String>,
) -> Result<(String, &'static str), ApiError> {
    let run_id = run_id.unwrap_or_else(|| Uuid::new_v4().to_string());
    let params_json =
        serde_json::to_string(&params.unwrap_or(Value::Object(Default::default())))?;

    let msg = ServerMessage {
        request_id: Uuid::new_v4().to_string(),
        body: Some(server_message::Body::Dispatch(Dispatch {
            run_id: run_id.clone(),
            spec_id: spec_id.to_string(),
            params_json: params_json.clone(),
        })),
    };

    let live = st.hub.send(node_id, msg).await;
    if !live {
        st.db
            .enqueue_dispatch(node_id, &run_id, spec_id, &params_json)
            .await?;
    }

    Ok((run_id, if live { "live" } else { "queued" }))
}

async fn dispatch_command(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DispatchBody>,
) -> Result<impl IntoResponse, ApiError> {
    let _ = st
        .db
        .get_config(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("node config not found; PUT config first"))?;

    let (run_id, delivered) =
        dispatch_to_node(&st, &id, &body.spec_id, body.params, body.run_id).await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "accepted": true,
            "node_id": id,
            "run_id": run_id,
            "spec_id": body.spec_id,
            "delivered": delivered,
        })),
    ))
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

async fn licensed(st: &AppState) -> Result<bool, ApiError> {
    Ok(st.db.is_licensed(st.license_env.as_deref()).await?)
}

async fn get_license(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let key = st.db.get_license_key().await?;
    let env_set = st
        .license_env
        .as_ref()
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let licensed = env_set || !key.trim().is_empty();
    Ok(Json(serde_json::json!({
        "licensed": licensed,
        "source": if env_set { "env" } else if !key.is_empty() { "db" } else { "none" },
        "stub": true,
        "note": "EE report paths require a license (NOVBOT_LICENSE_KEY or PUT /v1/license)",
    })))
}

#[derive(Debug, Deserialize)]
pub struct PutLicenseBody {
    pub key: String,
}

async fn put_license(
    State(st): State<AppState>,
    Json(body): Json<PutLicenseBody>,
) -> Result<Json<Value>, ApiError> {
    st.db.set_license_key(body.key.trim()).await?;
    let licensed = st.db.is_licensed(st.license_env.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "licensed": licensed,
        "stub": true,
    })))
}

async fn ee_reports(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    if !licensed(&st).await? {
        return Err(ApiError::license_required(
            "EE reports require a license (stub gate)",
        ));
    }
    Ok(Json(serde_json::json!({
        "reports": [],
        "note": "EE report packs live in novbot-enterprise; OSS returns empty list when licensed",
    })))
}

async fn ee_report_by_id(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !licensed(&st).await? {
        return Err(ApiError::license_required(
            "EE reports require a license (stub gate)",
        ));
    }
    Err(ApiError::not_found(format!(
        "EE report '{id}' not available in OSS (enterprise pack)"
    )))
}

pub async fn serve(
    addr: SocketAddr,
    db: Db,
    hub: Hub,
    license_env: Option<String>,
    api_token: Option<String>,
) -> anyhow::Result<()> {
    let app = router(AppState {
        db,
        hub,
        license_env,
        api_token,
    });
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

    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }

    fn license_required(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYMENT_REQUIRED,
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
            Json(serde_json::json!({
                "error": self.message,
                "license_required": self.status == StatusCode::PAYMENT_REQUIRED,
            })),
        )
            .into_response()
    }
}
