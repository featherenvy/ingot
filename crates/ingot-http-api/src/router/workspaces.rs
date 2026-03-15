use super::items::append_activity;
use super::*;
use super::support::*;

pub(super) async fn reset_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(&state, &project).await?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;

    let expected_head = workspace.head_commit_oid.clone().ok_or_else(|| {
        ApiError::from(UseCaseError::Internal(
            "workspace missing head_commit_oid".into(),
        ))
    })?;
    let now = Utc::now();
    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id,
        operation_kind: OperationKind::ResetWorkspace,
        entity_type: GitEntityType::Workspace,
        entity_id: workspace.id.to_string(),
        workspace_id: Some(workspace.id),
        ref_name: workspace.workspace_ref.clone(),
        expected_old_oid: workspace.head_commit_oid.clone(),
        new_oid: Some(expected_head.clone()),
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: now,
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;

    match workspace.kind {
        WorkspaceKind::Authoring | WorkspaceKind::Integration => {
            git(
                FsPath::new(&workspace.path),
                &["reset", "--hard", &expected_head],
            )
            .await
            .map_err(git_to_internal)?;
            git(FsPath::new(&workspace.path), &["clean", "-fd"])
                .await
                .map_err(git_to_internal)?;
            if let Some(workspace_ref) = workspace.workspace_ref.as_deref() {
                ingot_git::commands::git(
                    paths.mirror_git_dir.as_path(),
                    &["update-ref", workspace_ref, &expected_head],
                )
                .await
                .map_err(git_to_internal)?;
            }
        }
        WorkspaceKind::Review => {
            provision_review_workspace(
                paths.mirror_git_dir.as_path(),
                FsPath::new(&workspace.path),
                &expected_head,
            )
            .await
            .map_err(workspace_to_api_error)?;
        }
    }

    workspace.status = WorkspaceStatus::Ready;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;

    Ok(Json(workspace))
}

pub(super) async fn abandon_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;
    workspace.status = WorkspaceStatus::Abandoned;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(workspace))
}

pub(super) async fn remove_workspace_route(
    State(state): State<AppState>,
    Path((project_id, workspace_id)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let workspace_id = parse_id::<WorkspaceId>(&workspace_id, "workspace")?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(&state, &project).await?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;
    let mut workspace = state
        .db
        .get_workspace(workspace_id)
        .await
        .map_err(repo_to_internal)?;
    if workspace.project_id != project_id {
        return Err(ApiError::NotFound {
            code: "workspace_not_found",
            message: "Workspace not found".into(),
        });
    }
    ensure_workspace_not_busy(&workspace)?;
    workspace.status = WorkspaceStatus::Removing;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;

    if PathBuf::from(&workspace.path).exists() {
        remove_workspace(paths.mirror_git_dir.as_path(), FsPath::new(&workspace.path))
            .await
            .map_err(workspace_to_api_error)?;
    }
    if let Some(workspace_ref) = workspace.workspace_ref.as_deref() {
        let mirror_ref_exists = resolve_ref_oid(paths.mirror_git_dir.as_path(), workspace_ref)
            .await
            .map_err(git_to_internal)?
            .is_some();
        if mirror_ref_exists {
            let now = Utc::now();
            let mut operation = GitOperation {
                id: ingot_domain::ids::GitOperationId::new(),
                project_id,
                operation_kind: OperationKind::RemoveWorkspaceRef,
                entity_type: GitEntityType::Workspace,
                entity_id: workspace.id.to_string(),
                workspace_id: Some(workspace.id),
                ref_name: Some(workspace_ref.into()),
                expected_old_oid: workspace.head_commit_oid.clone(),
                new_oid: None,
                commit_oid: None,
                status: GitOperationStatus::Planned,
                metadata: None,
                created_at: now,
                completed_at: None,
            };
            state
                .db
                .create_git_operation(&operation)
                .await
                .map_err(repo_to_internal)?;
            append_activity(
            &state,
            project_id,
            ActivityEventType::GitOperationPlanned,
            "git_operation",
            operation.id,
            serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
        )
        .await?;
            delete_ref(paths.mirror_git_dir.as_path(), workspace_ref)
                .await
                .map_err(git_to_internal)?;
            operation.status = GitOperationStatus::Applied;
            operation.completed_at = Some(Utc::now());
            state
                .db
                .update_git_operation(&operation)
                .await
                .map_err(repo_to_internal)?;
        }
    }

    workspace.status = WorkspaceStatus::Abandoned;
    workspace.current_job_id = None;
    workspace.updated_at = Utc::now();
    state
        .db
        .update_workspace(&workspace)
        .await
        .map_err(repo_to_internal)?;
    Ok(Json(workspace))
}

