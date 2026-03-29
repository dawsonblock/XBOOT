use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::api::handlers::{AppState, ErrorResponse};
use crate::pool::{PoolEvent, PoolStatusSnapshot, RecycleScope};

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn check_admin_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let verifier = match &state.admin_api_key_verifier {
        Some(verifier) => verifier,
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    error: "admin API disabled".into(),
                    request_id: None,
                }),
            ));
        }
    };

    match extract_api_key(headers) {
        Some(key) => match verifier.verify(&key) {
            Ok(record) => Ok(record.id),
            Err(_) => Err((
                StatusCode::UNAUTHORIZED,
                Json(ErrorResponse {
                    error: "Invalid admin API key".into(),
                    request_id: None,
                }),
            )),
        },
        None => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "Missing Authorization header".into(),
                request_id: None,
            }),
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct ScaleRequest {
    pub targets: HashMap<String, usize>,
}

#[derive(Debug, Deserialize)]
pub struct RecycleRequest {
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default = "default_recycle_scope")]
    pub scope: String,
    #[serde(default)]
    pub reason: Option<String>,
}

fn default_recycle_scope() -> String {
    "idle".into()
}

#[derive(Debug, Deserialize, Default)]
pub struct EventQuery {
    #[serde(default = "default_event_limit")]
    pub limit: usize,
}

fn default_event_limit() -> usize {
    50
}

#[derive(Debug, Serialize)]
pub struct PoolEventsResponse {
    pub events: Vec<PoolEvent>,
}

#[derive(Debug, Serialize)]
pub struct RecycleResponse {
    pub recycled: HashMap<String, usize>,
    pub pool: PoolStatusSnapshot,
}

pub async fn pool_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = check_admin_auth(&state, &headers) {
        return error.into_response();
    }
    (StatusCode::OK, Json(state.pool.status())).into_response()
}

pub async fn scale_pool_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ScaleRequest>,
) -> impl IntoResponse {
    if let Err(error) = check_admin_auth(&state, &headers) {
        return error.into_response();
    }
    match state.pool.set_targets(&req.targets) {
        Ok(status) => (StatusCode::OK, Json(status)).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
                request_id: None,
            }),
        )
            .into_response(),
    }
}

pub async fn recycle_pool_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<RecycleRequest>,
) -> impl IntoResponse {
    if let Err(error) = check_admin_auth(&state, &headers) {
        return error.into_response();
    }
    let scope = match req.scope.as_str() {
        "idle" => RecycleScope::Idle,
        "all" => RecycleScope::All,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: format!("unsupported recycle scope {}", other),
                    request_id: None,
                }),
            )
                .into_response();
        }
    };
    match state.pool.recycle_languages(
        &req.languages,
        scope,
        req.reason.as_deref().unwrap_or("manual"),
    ) {
        Ok(recycled) => (
            StatusCode::OK,
            Json(RecycleResponse {
                recycled,
                pool: state.pool.status(),
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: error.to_string(),
                request_id: None,
            }),
        )
            .into_response(),
    }
}

pub async fn pool_events_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<EventQuery>,
) -> impl IntoResponse {
    if let Err(error) = check_admin_auth(&state, &headers) {
        return error.into_response();
    }
    (
        StatusCode::OK,
        Json(PoolEventsResponse {
            events: state.pool.events(query.limit),
        }),
    )
        .into_response()
}
