// Copyright 2026 TrendOverflow / NovHub
// SPDX-License-Identifier: Apache-2.0

use crate::db::{Db, NodeConfig, NodeRow, ResultRow};
use crate::hub::Hub;
use crate::skill_bundles;
use crate::skill_catalog::{self, HubSkillItem, SkillDetail, VersionDetail};
use crate::skill_desired;
use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use novbot_proto::{server_message, Dispatch, ServerMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

/// Axum's default body limit is 2 MiB. The upload route accepts 16 MiB + 1 MiB so a
/// body of 16 MiB + 1 byte reaches the handler and returns `package_too_large`.
const UPLOAD_BODY_LIMIT: usize = crate::db::PACKAGE_MAX_BYTES as usize + 1024 * 1024;

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
        .route("/nodes/{id}/skills", get(get_node_skills))
        .route("/results", get(list_results))
        .route("/license", get(get_license).put(put_license))
        .route(
            "/skills",
            get(list_skills)
                .post(upload_skill)
                .layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route("/skills/{name}", get(get_skill))
        .route("/skills/{name}/versions/{version}", get(get_skill_version))
        .route(
            "/skills/{name}/versions/{version}/package",
            get(get_skill_package),
        )
        .route("/skills/{name}/install", post(install_skill))
        .route("/skills/{name}/rollback", post(rollback_skill))
        .route("/skills/{name}/uninstall", post(uninstall_skill))
        .route("/skill-operations", get(list_skill_operations))
        .route("/skill-operations/{id}", get(get_skill_operation))
        .route("/skill-capabilities", get(list_skill_capabilities))
        .route("/audit-events", get(list_audit_events))
        .route(
            "/skill-bundles",
            get(list_skill_bundles).post(create_skill_bundle),
        )
        .route(
            "/skill-bundles/{id}",
            get(get_skill_bundle)
                .put(replace_skill_bundle)
                .delete(delete_skill_bundle),
        )
        .route("/skill-bundles/{id}/install", post(install_skill_bundle))
        .route("/skill-bundles/{id}/run", post(run_skill_bundle))
        .route("/skill-imports", any(license_stub))
        .route("/skill-imports/{id}", any(license_stub))
        .route("/fleet/skill-groups", get(list_skill_groups))
        .route("/fleet/skill-groups/push", post(push_skill_group))
        .route("/tokens", get(tokens_status).post(create_token))
        // EE / report paths — closed without license (M7 stub).
        .route("/ee/reports", get(ee_reports))
        .route("/ee/reports/{id}", get(ee_report_by_id))
        .route("/ee/skills/approvals", any(license_stub))
        .route("/ee/skills/rollouts", any(license_stub))
        .route("/ee/skills/registries", any(license_stub))
        .route("/ee/skills/trusted-keys", any(license_stub))
        .route("/ee/audit/export", any(license_stub))
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

#[derive(Serialize)]
struct ListSkillsBody {
    skills: Vec<&'static str>,
    items: Vec<HubSkillItem>,
    next_page_token: Option<&'static str>,
    note: &'static str,
}

struct SkillUploadBody(Bytes);

impl<S> FromRequest<S> for SkillUploadBody
where
    S: Send + Sync,
{
    type Rejection = BytesRejection;

    async fn from_request(mut req: Request, state: &S) -> Result<Self, Self::Rejection> {
        DefaultBodyLimit::max(UPLOAD_BODY_LIMIT).apply(&mut req);
        Ok(Self(Bytes::from_request(req, state).await?))
    }
}

async fn list_skills(State(st): State<AppState>) -> Result<Json<ListSkillsBody>, ApiError> {
    let mut items = skill_catalog::list_hub_items(&st.db).await?;
    items.extend(skill_catalog::builtin_list_items());
    Ok(Json(ListSkillsBody {
        skills: novbot_core::list_skills(),
        items,
        next_page_token: None,
        note: "Invoke via Spec kind skill / mcp_tool, or POST /v1/nodes/:id/dispatch",
    }))
}

async fn upload_skill(
    State(st): State<AppState>,
    headers: HeaderMap,
    SkillUploadBody(body): SkillUploadBody,
) -> Result<Response, ApiError> {
    let content_type = match headers.get(header::CONTENT_TYPE) {
        None => None,
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => {
                return Err(ApiError::coded(
                    StatusCode::BAD_REQUEST,
                    "invalid_archive",
                    "Content-Type must be application/vnd.novbot.skill",
                ));
            }
        },
    };
    let sha_header = match headers.get("x-novbot-sha256") {
        None => None,
        Some(value) => match value.to_str() {
            Ok(value) => Some(value),
            Err(_) => {
                return Err(ApiError::coded(
                    StatusCode::BAD_REQUEST,
                    "hash_mismatch",
                    "X-Novbot-Sha256 does not match the package body",
                ));
            }
        },
    };
    let outcome = skill_catalog::register_package(&st.db, content_type, sha_header, &body).await?;
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(outcome.body)).into_response())
}

async fn get_skill(
    State(st): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<SkillDetail>, ApiError> {
    if let Some(detail) = skill_catalog::builtin_detail(&name) {
        return Ok(Json(detail));
    }
    Ok(Json(skill_catalog::get_skill(&st.db, &name).await?))
}

async fn get_skill_version(
    State(st): State<AppState>,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<VersionDetail>, ApiError> {
    Ok(Json(
        skill_catalog::get_version(&st.db, &name, &version).await?,
    ))
}

async fn get_skill_package(
    State(st): State<AppState>,
    Path((name, version)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let bytes = skill_catalog::read_package(&st.db, &name, &version).await?;
    Ok(([(header::CONTENT_TYPE, CONTENT_TYPE_SKILL)], bytes).into_response())
}

async fn install_skill(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<skill_desired::InstallRequest>,
) -> Result<(StatusCode, Json<skill_desired::ChangeResponse>), ApiError> {
    let response = skill_desired::install(&st.db, &st.hub, &name, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn rollback_skill(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<skill_desired::RollbackRequest>,
) -> Result<(StatusCode, Json<skill_desired::ChangeResponse>), ApiError> {
    let response = skill_desired::rollback(&st.db, &st.hub, &name, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn uninstall_skill(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<skill_desired::UninstallRequest>,
) -> Result<(StatusCode, Json<skill_desired::ChangeResponse>), ApiError> {
    let response = skill_desired::uninstall(&st.db, &st.hub, &name, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

async fn list_skill_operations(
    State(st): State<AppState>,
    Query(query): Query<skill_desired::ListLimit>,
) -> Result<Json<skill_desired::OperationList>, ApiError> {
    Ok(Json(
        skill_desired::list_operations(&st.db, query.limit).await?,
    ))
}

async fn get_skill_operation(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<skill_desired::OperationView>, ApiError> {
    Ok(Json(skill_desired::get_operation(&st.db, &id).await?))
}

async fn get_node_skills(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<skill_desired::NodeSkillsResponse>, ApiError> {
    Ok(Json(
        skill_desired::node_skills(&st.db, &st.hub, &id).await?,
    ))
}

async fn list_skill_capabilities() -> Json<skill_desired::CapabilityCatalog> {
    Json(skill_desired::capability_catalog())
}

async fn list_audit_events(
    State(st): State<AppState>,
    Query(query): Query<skill_desired::AuditQuery>,
) -> Result<Json<skill_desired::AuditList>, ApiError> {
    Ok(Json(skill_desired::list_audit(&st.db, query).await?))
}

async fn list_skill_bundles(
    State(st): State<AppState>,
) -> Result<Json<skill_bundles::BundleList>, ApiError> {
    Ok(Json(skill_bundles::list_bundles(&st.db).await?))
}

async fn create_skill_bundle(
    State(st): State<AppState>,
    Json(body): Json<skill_bundles::BundleCreate>,
) -> Result<(StatusCode, Json<skill_bundles::BundleView>), ApiError> {
    let bundle = skill_bundles::create_bundle(&st.db, body).await?;
    Ok((StatusCode::CREATED, Json(bundle)))
}

async fn get_skill_bundle(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<skill_bundles::BundleView>, ApiError> {
    Ok(Json(skill_bundles::get_bundle(&st.db, &id).await?))
}

async fn replace_skill_bundle(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<skill_bundles::BundleReplace>,
) -> Result<Json<skill_bundles::BundleView>, ApiError> {
    Ok(Json(
        skill_bundles::replace_bundle(&st.db, &id, body).await?,
    ))
}

async fn delete_skill_bundle(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    skill_bundles::delete_bundle(&st.db, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn install_skill_bundle(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<skill_bundles::BundleInstallRequest>,
) -> Result<(StatusCode, Json<skill_bundles::BundleInstallResponse>), ApiError> {
    let response = skill_bundles::install_bundle(&st.db, &st.hub, &id, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[derive(Debug, Deserialize)]
struct RunBundleBody {
    node_ids: Vec<String>,
}

async fn run_skill_bundle(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RunBundleBody>,
) -> Result<impl IntoResponse, ApiError> {
    let group = skill_bundles::fleet_group(&st.db, &id).await?;
    if body.node_ids.is_empty() {
        return Err(ApiError::bad_request("node_ids must not be empty"));
    }
    let results = dispatch_named_skills(&st, &group.skills, &body.node_ids).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "accepted": true,
            "bundle_id": group.id,
            "group_id": group.id,
            "group_name": group.name,
            "skills": group.skills,
            "results": results,
        })),
    ))
}

async fn license_stub() -> Result<Json<Value>, ApiError> {
    Err(ApiError::coded(
        StatusCode::PAYMENT_REQUIRED,
        "license_required",
        "license required",
    ))
}

const CONTENT_TYPE_SKILL: &str = "application/vnd.novbot.skill";

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

async fn list_skill_groups(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let groups = skill_bundles::list_fleet_groups(&st.db).await?;
    Ok(Json(serde_json::json!({
        "groups": groups,
        "note": "POST /v1/fleet/skill-groups/push with {group_id, node_ids[]} dispatches matching skill specs per node",
    })))
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
    let group = skill_bundles::fleet_group(&st.db, &body.group_id).await?;
    if body.node_ids.is_empty() {
        return Err(ApiError::bad_request("node_ids must not be empty"));
    }
    let results = dispatch_named_skills(&st, &group.skills, &body.node_ids).await?;
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

async fn dispatch_named_skills(
    st: &AppState,
    skills: &[String],
    node_ids: &[String],
) -> Result<Vec<Value>, ApiError> {
    let mut results = Vec::new();
    for node_id in node_ids {
        let cfg = match st.db.get_config(node_id).await? {
            Some(config) => config,
            None => {
                for skill in skills {
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
        for skill in skills {
            let spec_id = resolve_spec_id_for_skill(&specs, skill);
            match dispatch_to_node(st, node_id, &spec_id, None, None).await {
                Ok((run_id, delivered)) => results.push(serde_json::json!({
                    "node_id": node_id,
                    "skill": skill,
                    "spec_id": spec_id,
                    "run_id": run_id,
                    "status": "ok",
                    "delivered": delivered,
                })),
                Err(err) => results.push(serde_json::json!({
                    "node_id": node_id,
                    "skill": skill,
                    "spec_id": spec_id,
                    "status": "error",
                    "error": err.message,
                })),
            }
        }
    }
    Ok(results)
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
    // The nodes row must exist for the node_configs FK. Do not upsert:
    // that overwrites hostname, version, labels, and last_seen_at.
    st.db.ensure_node_placeholder(&id).await?;
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
    let params_json = serde_json::to_string(&params.unwrap_or(Value::Object(Default::default())))?;

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
    let cfg = st
        .db
        .get_config(&id)
        .await?
        .ok_or_else(|| ApiError::not_found("node config not found; PUT config first"))?;
    if !spec_present(&cfg.specs_json, &body.spec_id) {
        return Err(ApiError::coded(
            StatusCode::NOT_FOUND,
            "spec_unknown",
            "unknown spec",
        ));
    }

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

fn spec_present(specs_json: &str, spec_id: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(specs_json) else {
        return false;
    };
    let Some(specs) = value.as_array() else {
        return false;
    };
    specs
        .iter()
        .any(|spec| spec.get("id").and_then(Value::as_str) == Some(spec_id))
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
    code: Option<String>,
    extra: Option<Value>,
}

impl ApiError {
    fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            code: Some(code.to_string()),
            extra: None,
        }
    }

    fn with_extra(mut self, extra: Value) -> Self {
        self.extra = Some(extra);
        self
    }

    fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
            code: None,
            extra: None,
        }
    }

    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
            code: None,
            extra: None,
        }
    }

    fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
            code: None,
            extra: None,
        }
    }

    fn license_required(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYMENT_REQUIRED,
            message: msg.into(),
            code: None,
            extra: None,
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: e.to_string(),
            code: None,
            extra: None,
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: e.to_string(),
            code: None,
            extra: None,
        }
    }
}

impl From<skill_catalog::CatalogError> for ApiError {
    fn from(err: skill_catalog::CatalogError) -> Self {
        use skill_catalog::CatalogError::*;
        match err {
            UploadsDisabled { max_allowed_packet } => ApiError::coded(
                StatusCode::SERVICE_UNAVAILABLE,
                "uploads_disabled",
                format!(
                    "uploads disabled: MySQL max_allowed_packet is {max_allowed_packet} bytes, which does not exceed 16 MiB"
                ),
            ),
            PackageTooLarge => ApiError::coded(
                StatusCode::PAYLOAD_TOO_LARGE,
                "package_too_large",
                "package exceeds 16 MiB",
            ),
            InvalidArchive(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "invalid_archive", message)
            }
            InvalidManifest(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "invalid_manifest", message)
            }
            InvalidWasm(message) => ApiError::coded(StatusCode::BAD_REQUEST, "invalid_wasm", message),
            HashMismatch => ApiError::coded(
                StatusCode::BAD_REQUEST,
                "hash_mismatch",
                "X-Novbot-Sha256 does not match the package body",
            ),
            NameReserved => ApiError::coded(
                StatusCode::CONFLICT,
                "name_reserved",
                "skill name is reserved",
            ),
            VersionExists => ApiError::coded(
                StatusCode::CONFLICT,
                "version_exists",
                "skill version already exists with a different package",
            ),
            CapabilityUnsupported(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "capability_unsupported", message)
            }
            CapabilityInvalid(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "capability_invalid", message)
            }
            SkillUnknown => {
                ApiError::coded(StatusCode::NOT_FOUND, "skill_unknown", "unknown skill")
            }
            VersionUnknown => ApiError::coded(
                StatusCode::NOT_FOUND,
                "version_unknown",
                "unknown skill version",
            ),
            Internal(err) => ApiError::from(err),
        }
    }
}

impl From<skill_desired::DesiredError> for ApiError {
    fn from(err: skill_desired::DesiredError) -> Self {
        use skill_desired::DesiredError::*;
        match err {
            SkillUnknown => {
                ApiError::coded(StatusCode::NOT_FOUND, "skill_unknown", "unknown skill")
            }
            BuiltinReadOnly => ApiError::coded(
                StatusCode::CONFLICT,
                "builtin_read_only",
                "built-in resource is read-only",
            ),
            VersionUnknown => ApiError::coded(
                StatusCode::NOT_FOUND,
                "version_unknown",
                "unknown skill version",
            ),
            VersionNotInstallable => ApiError::coded(
                StatusCode::CONFLICT,
                "version_not_installable",
                "skill version is not installable",
            ),
            CapabilitiesChanged => ApiError::coded(
                StatusCode::CONFLICT,
                "capabilities_changed",
                "accepted capabilities hash does not match",
            ),
            NoTargetNodes => ApiError::coded(
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_target_nodes",
                "no target nodes",
            ),
            NoPreviousVersion => ApiError::coded(
                StatusCode::CONFLICT,
                "no_previous_version",
                "no previous version",
            ),
            SkillInUse {
                spec_ids,
                schedule_ids,
            } => ApiError::coded(
                StatusCode::CONFLICT,
                "skill_in_use",
                "skill is referenced by a spec",
            )
            .with_extra(serde_json::json!({
                "spec_ids": spec_ids,
                "schedule_ids": schedule_ids,
            })),
            NodeUnknown => ApiError::coded(StatusCode::NOT_FOUND, "node_unknown", "unknown node"),
            OperationUnknown => ApiError::coded(
                StatusCode::NOT_FOUND,
                "operation_unknown",
                "unknown operation",
            ),
            BundleUnknown => {
                ApiError::coded(StatusCode::NOT_FOUND, "bundle_unknown", "unknown bundle")
            }
            BundleExists => ApiError::coded(
                StatusCode::CONFLICT,
                "bundle_exists",
                "bundle already exists",
            ),
            InvalidBundle(message) => {
                ApiError::coded(StatusCode::BAD_REQUEST, "invalid_bundle", message)
            }
            BadRequest(message) => ApiError::bad_request(message),
            Internal(err) => ApiError::from(err),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let mut body = serde_json::json!({
            "error": self.message,
            "license_required": self.status == StatusCode::PAYMENT_REQUIRED,
            "code": self.code,
        });
        if let (Some(object), Some(extra)) = (body.as_object_mut(), self.extra) {
            if let Some(fields) = extra.as_object() {
                for (key, value) in fields {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{router, AppState};
    use crate::db::{connect_test_db, Db, NodeConfig, NodeRow};
    use crate::hub::Hub;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use std::collections::HashMap;
    use tower::ServiceExt;

    async fn fetch_node(db: &Db, id: &str) -> NodeRow {
        db.list_nodes()
            .await
            .expect("list nodes")
            .into_iter()
            .find(|n| n.node_id == id)
            .unwrap_or_else(|| panic!("missing node {id}"))
    }

    fn parse_json(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).unwrap_or_else(|e| panic!("json ({e}): {raw}"))
    }

    /// PUT /v1/nodes/:id/config through `router()`, the handler's real path.
    async fn http_put_config(db: Db, id: &str, specs: &serde_json::Value) -> NodeConfig {
        let app = router(AppState {
            db,
            hub: Hub::new(),
            api_token: None,
            license_env: None,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/v1/nodes/{id}/config"))
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "specs": specs }).to_string(),
                    ))
                    .expect("build request"),
            )
            .await
            .expect("router oneshot");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        assert_eq!(
            status,
            StatusCode::OK,
            "PUT /v1/nodes/{id}/config: {}",
            String::from_utf8_lossy(&bytes)
        );
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "decode NodeConfig ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }

    #[tokio::test]
    async fn put_config_keeps_registration_metadata() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = format!("t16-{}", uuid::Uuid::new_v4());
        let mut labels = HashMap::new();
        labels.insert("env".to_string(), "demo".to_string());
        labels.insert("role".to_string(), "app".to_string());
        db.upsert_node(&id, "host-a", "0.1.0", &labels)
            .await
            .expect("upsert");

        let before = fetch_node(&db, &id).await;
        let before_gen = db
            .get_config(&id)
            .await
            .expect("get config")
            .expect("config row")
            .config_generation;

        // A registration rewrite would bump last_seen_at as well as the labels.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let specs = serde_json::json!([
            {
                "id": "echo-1",
                "kind": "skill",
                "params": {"skill": "echo", "message": "hi"}
            }
        ]);
        let cfg = http_put_config(db.clone(), &id, &specs).await;

        let after = fetch_node(&db, &id).await;
        assert_eq!(after.hostname, "host-a");
        assert_eq!(after.version, "0.1.0");
        assert_eq!(
            after.labels,
            serde_json::json!({"env": "demo", "role": "app"})
        );
        assert_eq!(after.last_seen_at, before.last_seen_at);

        assert_eq!(cfg.config_generation, before_gen + 1);
        assert_eq!(cfg.node_id, id);
        assert_eq!(parse_json(&cfg.specs_json), specs);

        let stored = db.get_config(&id).await.expect("get config").expect("row");
        assert_eq!(stored.config_generation, before_gen + 1);
        assert_eq!(parse_json(&stored.specs_json), specs);
    }

    #[tokio::test]
    async fn put_config_unknown_node_creates_placeholder_then_register_fills() {
        let Some(db) = connect_test_db().await else {
            return;
        };
        let id = format!("t16-{}", uuid::Uuid::new_v4());
        let specs = serde_json::json!([
            {"id": "disk", "kind": "probe", "params": {"mount": "/"}}
        ]);
        let cfg = http_put_config(db.clone(), &id, &specs).await;
        assert_eq!(cfg.config_generation, 1);
        assert_eq!(cfg.node_id, id);
        assert_eq!(parse_json(&cfg.specs_json), specs);

        let placeholder = fetch_node(&db, &id).await;
        assert_eq!(placeholder.hostname, "");
        assert_eq!(placeholder.version, "");
        assert_eq!(placeholder.labels, serde_json::json!({}));
        assert_eq!(placeholder.last_seen_at, None);

        let mut labels = HashMap::new();
        labels.insert("role".to_string(), "db".to_string());
        db.upsert_node(&id, "host-b", "0.1.0", &labels)
            .await
            .expect("register");

        let registered = fetch_node(&db, &id).await;
        assert_eq!(registered.hostname, "host-b");
        assert_eq!(registered.version, "0.1.0");
        assert_eq!(registered.labels, serde_json::json!({"role": "db"}));

        let stored = db.get_config(&id).await.expect("get config").expect("row");
        assert_eq!(stored.config_generation, 1);
        assert_eq!(parse_json(&stored.specs_json), specs);
    }
}
