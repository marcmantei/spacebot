//! REST API handlers for the dynamic project registry.

use super::state::ApiState;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::registry::store::RegistryRepo;
use crate::registry::sync::{SyncResult, SyncStatus, sync_registry};

// ---------------------------------------------------------------------------
// Query / request types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct AgentQuery {
    agent_id: String,
}

#[derive(Deserialize)]
pub(super) struct RepoListQuery {
    agent_id: String,
    #[serde(default)]
    enabled_only: bool,
}

#[derive(Deserialize)]
pub(super) struct RepoQuery {
    agent_id: String,
    full_name: String,
}

#[derive(Deserialize)]
pub(super) struct UpdateRepoOverridesBody {
    agent_id: String,
    full_name: String,
    /// Set to `Some(Some("model"))` to set, `Some(None)` to clear, `None` to leave unchanged.
    worker_model: Option<Option<String>>,
    enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub(super) struct RepoListResponse {
    repos: Vec<RegistryRepo>,
    total: usize,
}

#[derive(Serialize)]
pub(super) struct SyncResponse {
    result: SyncResult,
}

#[derive(Serialize)]
pub(super) struct StatusResponse {
    status: SyncStatus,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/registry/repos — list all registry repos for an agent.
pub(super) async fn list_registry_repos(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RepoListQuery>,
) -> Result<Json<RepoListResponse>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repos = store
        .list_repos(&query.agent_id, query.enabled_only)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let total = repos.len();
    Ok(Json(RepoListResponse { repos, total }))
}

/// GET /api/registry/repos/detail — get a single repo by full_name.
pub(super) async fn get_registry_repo(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<RepoQuery>,
) -> Result<Json<RegistryRepo>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .get_by_full_name(&query.agent_id, &query.full_name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(repo))
}

/// PUT /api/registry/repos/overrides — update per-repo overrides.
pub(super) async fn update_repo_overrides(
    State(state): State<Arc<ApiState>>,
    Json(body): Json<UpdateRepoOverridesBody>,
) -> Result<Json<RegistryRepo>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&body.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let repo = store
        .set_overrides(
            &body.agent_id,
            &body.full_name,
            body.worker_model,
            body.enabled,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(repo))
}

/// POST /api/registry/sync — trigger a manual sync.
pub(super) async fn trigger_sync(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<SyncResponse>, StatusCode> {
    let stores = state.registry_stores.load();
    let store = stores.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let configs = state.runtime_configs.load();
    let runtime_config = configs.get(&query.agent_id).ok_or(StatusCode::NOT_FOUND)?;

    let registry_config = runtime_config.registry.load();

    if !registry_config.enabled {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Update sync status
    let status_map = state.registry_sync_status.load();
    if let Some(status) = status_map.get(&query.agent_id) {
        status.store(Arc::new(SyncStatus::Syncing));
    }

    let result = sync_registry(store, &query.agent_id, &registry_config)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Update sync status
    if let Some(status) = status_map.get(&query.agent_id) {
        status.store(Arc::new(SyncStatus::Completed {
            at: chrono::Utc::now().to_rfc3339(),
            result: result.clone(),
        }));
    }

    Ok(Json(SyncResponse { result }))
}

/// GET /api/registry/status — get current sync status.
pub(super) async fn registry_status(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<AgentQuery>,
) -> Result<Json<StatusResponse>, StatusCode> {
    let status_map = state.registry_sync_status.load();
    let status = status_map
        .get(&query.agent_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let current = status.load().as_ref().clone();
    Ok(Json(StatusResponse { status: current }))
}
