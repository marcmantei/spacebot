use super::state::ApiState;
use crate::notifications::{NewNotification, NotificationKind, NotificationSeverity};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::process::Command as TokioCommand;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
pub(super) struct TaskListQuery {
    /// Convenience filter: matches tasks where owner OR assigned equals this value.
    #[serde(default)]
    agent_id: Option<String>,
    /// Filter by owner agent. Optional.
    #[serde(default)]
    owner_agent_id: Option<String>,
    /// Filter by assigned agent. Optional.
    #[serde(default)]
    assigned_agent_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
    #[serde(default = "default_task_limit")]
    limit: i64,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateTaskRequest {
    /// Agent that owns (created) this task.
    owner_agent_id: String,
    /// Agent assigned to execute. Defaults to `owner_agent_id`.
    #[serde(default)]
    assigned_agent_id: Option<String>,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    subtasks: Vec<crate::tasks::TaskSubtask>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    source_memory_id: Option<String>,
    #[serde(default)]
    created_by: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct UpdateTaskRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assigned_agent_id: Option<String>,
    #[serde(default)]
    subtasks: Option<Vec<crate::tasks::TaskSubtask>>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    complete_subtask: Option<usize>,
    #[serde(default)]
    worker_id: Option<String>,
    #[serde(default)]
    approved_by: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ApproveRequest {
    #[serde(default)]
    approved_by: Option<String>,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct AssignRequest {
    assigned_agent_id: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskListResponse {
    tasks: Vec<crate::tasks::Task>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskResponse {
    task: crate::tasks::Task,
}

#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct TaskActionResponse {
    success: bool,
    message: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn default_task_limit() -> i64 {
    100
}

/// Extract the global task store, returning 503 if not yet initialized.
fn get_task_store(state: &ApiState) -> Result<Arc<crate::tasks::TaskStore>, StatusCode> {
    state
        .task_store
        .load()
        .as_ref()
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

fn parse_status(value: Option<&str>) -> Result<Option<crate::tasks::TaskStatus>, StatusCode> {
    match value {
        None => Ok(None),
        Some(value) => Ok(Some(
            crate::tasks::TaskStatus::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
        )),
    }
}

fn parse_priority(value: Option<&str>) -> Result<Option<crate::tasks::TaskPriority>, StatusCode> {
    match value {
        None => Ok(None),
        Some(value) => Ok(Some(
            crate::tasks::TaskPriority::parse(value).ok_or(StatusCode::BAD_REQUEST)?,
        )),
    }
}

fn emit_task_event(state: &ApiState, task: &crate::tasks::Task, action: &str) {
    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.assigned_agent_id.clone(),
            task_number: task.task_number,
            status: task.status.to_string(),
            action: action.to_string(),
        })
        .ok();
}

/// Emit a task_approval notification when a task enters the pending_approval state.
fn maybe_emit_approval_notification(state: &ApiState, task: &crate::tasks::Task) {
    if task.status != crate::tasks::TaskStatus::PendingApproval {
        return;
    }
    state.emit_notification(NewNotification {
        kind: NotificationKind::TaskApproval,
        severity: NotificationSeverity::Info,
        title: task.title.clone(),
        body: task.description.clone(),
        agent_id: Some(task.assigned_agent_id.clone()),
        related_entity_type: Some("task".to_string()),
        related_entity_id: Some(task.task_number.to_string()),
        action_url: Some(format!("/tasks/{}", task.task_number)),
        metadata: None,
    });
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /tasks` — list tasks with optional filters.
#[utoipa::path(
    get,
    path = "/tasks",
    params(TaskListQuery),
    responses(
        (status = 200, body = TaskListResponse),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn list_tasks(
    State(state): State<Arc<ApiState>>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<TaskListResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = parse_status(query.status.as_deref())?;
    let priority = parse_priority(query.priority.as_deref())?;

    let tasks = store
        .list(crate::tasks::TaskListFilter {
            agent_id: query.agent_id,
            owner_agent_id: query.owner_agent_id,
            assigned_agent_id: query.assigned_agent_id,
            status,
            priority,
            created_by: query.created_by,
            limit: Some(query.limit.clamp(1, 500)),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to list tasks");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(TaskListResponse { tasks }))
}

/// `GET /tasks/{number}` — get a task by globally unique number.
#[utoipa::path(
    get,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn get_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks` — create a task.
#[utoipa::path(
    post,
    path = "/tasks",
    request_body = CreateTaskRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 400, description = "Invalid request"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn create_task(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<CreateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = crate::tasks::TaskStatus::PendingApproval;
    let priority =
        parse_priority(request.priority.as_deref())?.unwrap_or(crate::tasks::TaskPriority::Medium);

    let assigned = request
        .assigned_agent_id
        .unwrap_or_else(|| request.owner_agent_id.clone());

    let task = store
        .create(crate::tasks::CreateTaskInput {
            owner_agent_id: request.owner_agent_id,
            assigned_agent_id: assigned,
            title: request.title,
            description: request.description,
            status,
            priority,
            subtasks: request.subtasks,
            metadata: request.metadata.unwrap_or_else(|| serde_json::json!({})),
            source_memory_id: request.source_memory_id,
            created_by: request.created_by.unwrap_or_else(|| "human".to_string()),
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to create task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    emit_task_event(&state, &task, "created");
    maybe_emit_approval_notification(&state, &task);
    Ok(Json(TaskResponse { task }))
}

/// `PUT /tasks/{number}` — update a task.
#[utoipa::path(
    put,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = UpdateTaskRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 400, description = "Invalid request"),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn update_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<UpdateTaskRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let status = parse_status(request.status.as_deref())?;
    let priority = parse_priority(request.priority.as_deref())?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                title: request.title,
                description: request.description,
                status,
                priority,
                assigned_agent_id: request.assigned_agent_id,
                subtasks: request.subtasks,
                metadata: request.metadata,
                worker_id: request.worker_id,
                clear_worker_id: false,
                approved_by: request.approved_by,
                complete_subtask: request.complete_subtask,
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to update task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    maybe_emit_approval_notification(&state, &task);
    Ok(Json(TaskResponse { task }))
}

/// `DELETE /tasks/{number}` — delete a task.
#[utoipa::path(
    delete,
    path = "/tasks/{number}",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    responses(
        (status = 200, body = TaskActionResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn delete_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
) -> Result<Json<TaskActionResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    // Fetch before delete so we can emit an event with the correct agent_id.
    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task for deletion");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let deleted = store.delete(number).await.map_err(|error| {
        tracing::warn!(%error, task_number = number, "failed to delete task");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if !deleted {
        return Err(StatusCode::NOT_FOUND);
    }

    state
        .event_tx
        .send(super::state::ApiEvent::TaskUpdated {
            agent_id: task.assigned_agent_id,
            task_number: number,
            status: "deleted".to_string(),
            action: "deleted".to_string(),
        })
        .ok();

    Ok(Json(TaskActionResponse {
        success: true,
        message: format!("Task #{number} deleted"),
    }))
}

/// `POST /tasks/{number}/approve` — approve a task (move to ready).
#[utoipa::path(
    post,
    path = "/tasks/{number}/approve",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = ApproveRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn approve_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<ApproveRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to approve task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    // Auto-dismiss any pending task_approval notification for this task.
    if let Some(store) = state.notification_store.load().as_ref().clone()
        && let Err(error) = store
            .dismiss_by_entity("task_approval", "task", &number.to_string())
            .await
    {
        tracing::warn!(%error, task_number = number, "failed to auto-dismiss approval notification");
    }
    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks/{number}/execute` — move a task to ready for execution.
/// Tasks already in `ready` or `in_progress` are returned as-is.
#[utoipa::path(
    post,
    path = "/tasks/{number}/execute",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = ApproveRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 409, description = "Task pending approval"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn execute_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<ApproveRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let current = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task for execution");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    if matches!(
        current.status,
        crate::tasks::TaskStatus::Ready | crate::tasks::TaskStatus::InProgress
    ) {
        return Ok(Json(TaskResponse { task: current }));
    }

    // Reject pending_approval tasks — they must be approved first.
    if current.status == crate::tasks::TaskStatus::PendingApproval {
        return Err(StatusCode::CONFLICT);
    }

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                status: Some(crate::tasks::TaskStatus::Ready),
                approved_by: request.approved_by,
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to execute task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

/// `POST /tasks/{number}/assign` — reassign a task to a different agent.
#[utoipa::path(
    post,
    path = "/tasks/{number}/assign",
    params(
        ("number" = i64, Path, description = "Task number"),
    ),
    request_body = AssignRequest,
    responses(
        (status = 200, body = TaskResponse),
        (status = 404, description = "Task not found"),
        (status = 503, description = "Task store not initialized"),
    ),
    tag = "tasks",
)]
pub(super) async fn assign_task(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<AssignRequest>,
) -> Result<Json<TaskResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                assigned_agent_id: Some(request.assigned_agent_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to assign task");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &task, "updated");
    Ok(Json(TaskResponse { task }))
}

#[derive(Serialize)]
pub(super) struct TaskDiffResponse {
    task_number: i64,
    diff: String,
    branch: Option<String>,
    worktree: Option<String>,
}

/// Get the git diff for a task. Reads `worktree` and `branch` from task
/// metadata to determine which diff to show.
pub(super) async fn task_diff(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,

) -> Result<Json<TaskDiffResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, task_number = number, "failed to get task for diff");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let worktree_path = task
        .metadata
        .get("worktree")
        .and_then(|v| v.as_str())
        .map(String::from);
    let branch = task
        .metadata
        .get("branch")
        .and_then(|v| v.as_str())
        .map(String::from);

    let diff = if let Some(ref wt) = worktree_path {
        let output = TokioCommand::new("git")
            .args(["diff", "HEAD"])
            .current_dir(wt)
            .output()
            .await
            .map_err(|error| {
                tracing::warn!(%error, worktree = %wt, "failed to run git diff in worktree");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        String::from_utf8_lossy(&output.stdout).to_string()
    } else if let Some(ref br) = branch {
        let output = TokioCommand::new("git")
            .args(["diff", &format!("main...{br}")])
            .output()
            .await
            .map_err(|error| {
                tracing::warn!(%error, branch = %br, "failed to run git diff for branch");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        String::new()
    };

    Ok(Json(TaskDiffResponse {
        task_number: number,
        diff,
        branch,
        worktree: worktree_path,
    }))
}

#[derive(Deserialize)]
pub(super) struct WorktreeRequest {
    agent_id: String,
    #[serde(default)]
    base_branch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct WorktreeResponse {
    task_number: i64,
    branch: String,
    worktree_path: String,
    created: bool,
}

/// Create a git worktree for a task. Creates a new branch `task/{number}` and
/// a worktree directory. Stores the path and branch in task metadata.
pub(super) async fn create_worktree(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,
    Json(request): Json<WorktreeRequest>,
) -> Result<Json<WorktreeResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "failed to get task for worktree creation");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Check if worktree already exists
    if let Some(existing) = task.metadata.get("worktree")
        && let Some(path) = existing.as_str()
    {
        let branch = task
            .metadata
            .get("branch")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        return Ok(Json(WorktreeResponse {
            task_number: number,
            branch,
            worktree_path: path.to_string(),
            created: false,
        }));
    }

    let branch_name = format!("task/{number}");
    let base = request.base_branch.as_deref().unwrap_or("main");

    // Determine worktree directory (next to the repo)
    let repo_root = TokioCommand::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to find git root");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let root = String::from_utf8_lossy(&repo_root.stdout)
        .trim()
        .to_string();
    let worktree_dir = format!("{}/../.spacebot-worktrees/task-{number}", root);

    // Create branch from base
    let branch_result = TokioCommand::new("git")
        .args(["branch", &branch_name, base])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to create branch");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !branch_result.status.success() {
        let stderr = String::from_utf8_lossy(&branch_result.stderr);
        // Branch may already exist, which is fine
        if !stderr.contains("already exists") {
            tracing::warn!(%stderr, "git branch creation failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }

    // Create worktree
    let wt_result = TokioCommand::new("git")
        .args(["worktree", "add", &worktree_dir, &branch_name])
        .output()
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to create worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if !wt_result.status.success() {
        let stderr = String::from_utf8_lossy(&wt_result.stderr);
        tracing::warn!(%stderr, "git worktree add failed");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Update task metadata with worktree info
    let mut metadata = task.metadata;
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("worktree".to_string(), serde_json::json!(worktree_dir));
        obj.insert("branch".to_string(), serde_json::json!(branch_name));
    }

    let updated_task = store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to update task metadata with worktree");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    emit_task_event(&state, &updated_task, "updated");

    Ok(Json(WorktreeResponse {
        task_number: number,
        branch: branch_name,
        worktree_path: worktree_dir,
        created: true,
    }))
}

/// Remove a task's git worktree and optionally delete the branch.
pub(super) async fn delete_worktree(
    State(state): State<Arc<ApiState>>,
    Path(number): Path<i64>,

) -> Result<Json<TaskActionResponse>, StatusCode> {
    let store = get_task_store(&state)?;

    let task = store
        .get_by_number(number)
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to get task for worktree deletion");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;

    let worktree_path = task
        .metadata
        .get("worktree")
        .and_then(|v| v.as_str())
        .ok_or(StatusCode::NOT_FOUND)?
        .to_string();

    // Remove worktree
    let _ = TokioCommand::new("git")
        .args(["worktree", "remove", &worktree_path, "--force"])
        .output()
        .await;

    // Clean metadata
    let mut metadata = task.metadata;
    if let Some(obj) = metadata.as_object_mut() {
        obj.remove("worktree");
        // Keep branch reference for history
    }

    store
        .update(
            number,
            crate::tasks::UpdateTaskInput {
                metadata: Some(metadata),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!(%e, "failed to update task after worktree removal");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(TaskActionResponse {
        success: true,
        message: format!("Worktree removed for task #{number}"),
    }))
}
