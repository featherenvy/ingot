use super::items::append_activity;
use super::support::*;
use super::*;

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

    let expected_head = workspace.state.head_commit_oid().cloned().ok_or_else(|| {
        ApiError::from(UseCaseError::Internal(
            "workspace missing head_commit_oid".into(),
        ))
    })?;
    let now = Utc::now();
    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id,
        entity_id: workspace.id.to_string(),
        payload: OperationPayload::ResetWorkspace {
            workspace_id: workspace.id,
            ref_name: workspace.workspace_ref.clone(),
            expected_old_oid: workspace.state.head_commit_oid().cloned(),
            new_oid: expected_head.clone(),
        },
        status: GitOperationStatus::Planned,
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
        ActivityEntityType::GitOperation,
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity_id }),
    )
    .await?;

    match workspace.kind {
        WorkspaceKind::Authoring | WorkspaceKind::Integration => {
            git(
                FsPath::new(&workspace.path),
                &["reset", "--hard", expected_head.as_str()],
            )
            .await
            .map_err(git_to_internal)?;
            git(FsPath::new(&workspace.path), &["clean", "-fd"])
                .await
                .map_err(git_to_internal)?;
            if let Some(workspace_ref) = workspace.workspace_ref.as_ref() {
                ingot_git::commands::git(
                    paths.mirror_git_dir.as_path(),
                    &["update-ref", workspace_ref.as_str(), expected_head.as_str()],
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

    workspace.mark_ready_with_head(expected_head.clone(), Utc::now());
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
    let workspace = state
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
    let workspace = ingot_usecases::workspace::abandon_workspace(&state.db, &workspace).await?;
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
    let workspace = state
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

    // Phase 1: mark as Removing (DB)
    let workspace =
        ingot_usecases::workspace::plan_workspace_removal(&state.db, &workspace).await?;

    // Phase 2: filesystem cleanup (infrastructure)
    if PathBuf::from(&workspace.path).exists() {
        remove_workspace(paths.mirror_git_dir.as_path(), FsPath::new(&workspace.path))
            .await
            .map_err(workspace_to_api_error)?;
    }
    if let Some(workspace_ref) = workspace.workspace_ref.as_ref() {
        let current_ref_oid = resolve_ref_oid(paths.mirror_git_dir.as_path(), workspace_ref)
            .await
            .map_err(git_to_internal)?;
        if let Some(expected_old_oid) = current_ref_oid {
            let now = Utc::now();
            let mut operation = GitOperation {
                id: ingot_domain::ids::GitOperationId::new(),
                project_id,
                entity_id: workspace.id.to_string(),
                payload: OperationPayload::RemoveWorkspaceRef {
                    workspace_id: workspace.id,
                    ref_name: workspace_ref.clone(),
                    expected_old_oid,
                },
                status: GitOperationStatus::Planned,
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
                ActivityEntityType::GitOperation,
                operation.id,
                serde_json::json!({ "operation_kind": operation.operation_kind(), "entity_id": operation.entity_id }),
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

    // Phase 3: finalize to Abandoned (DB)
    let workspace =
        ingot_usecases::workspace::finalize_workspace_removal(&state.db, &workspace).await?;
    Ok(Json(workspace))
}
