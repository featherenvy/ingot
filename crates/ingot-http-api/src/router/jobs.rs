use super::items::{append_activity, current_authoring_head_for_revision_with_workspace, effective_authoring_base_commit_oid, ensure_authoring_workspace, hydrate_convergence_validity, read_optional_json, read_optional_text};
use super::*;
use super::support::*;
use super::types::*;

pub(super) async fn dispatch_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id)): Path<(String, String)>,
    maybe_request: Option<Json<DispatchJobRequest>>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
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

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }

    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences =
        hydrate_convergence_validity(paths.mirror_git_dir.as_path(), convergences).await?;
    let command = DispatchJobCommand {
        step_id: maybe_request.and_then(|Json(request)| request.step_id),
    };
    let mut job = dispatch_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
        command,
    )?;
    let mut precreated_authoring_workspace = None;
    let pending_investigation_ref = bind_dispatch_subjects_if_needed(
        &state,
        &project,
        &current_revision,
        &jobs,
        &mut job,
        &mut precreated_authoring_workspace,
    )
    .await?;

    if let Err(error) = state.db.create_job(&job).await {
        cleanup_failed_dispatch_side_effects(
            &state,
            &project,
            precreated_authoring_workspace.as_ref(),
            pending_investigation_ref
                .as_ref()
                .map(|pending| pending.ref_name.as_str()),
        )
        .await;
        return Err(repo_to_internal(error));
    }
    apply_pending_investigation_ref_or_cleanup(
        &state,
        &project,
        job.id,
        pending_investigation_ref.as_ref(),
        precreated_authoring_workspace.as_ref(),
    )
    .await?;

    if let Some(workspace) = precreated_authoring_workspace {
        link_job_to_workspace_or_cleanup(
            &state,
            &project,
            &mut job,
            workspace,
            pending_investigation_ref.as_ref(),
            true,
        )
        .await?;
    } else if job.workspace_kind == WorkspaceKind::Authoring {
        let had_existing_workspace = state
            .db
            .find_authoring_workspace_for_revision(current_revision.id)
            .await
            .map_err(repo_to_internal)?
            .is_some();
        let workspace =
            ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
        link_job_to_workspace_or_cleanup(
            &state,
            &project,
            &mut job,
            workspace,
            pending_investigation_ref.as_ref(),
            !had_existing_workspace,
        )
        .await?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        "job",
        job.id,
        serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(job)))
}

pub(super) fn investigation_ref_name(job_id: JobId) -> String {
    format!("refs/ingot/investigations/{job_id}")
}

pub(super) fn should_fill_candidate_subject_from_workspace(step_id: &str) -> bool {
    matches!(
        step_id,
        step::REVIEW_INCREMENTAL_INITIAL
            | step::REVIEW_CANDIDATE_INITIAL
            | step::REVIEW_CANDIDATE_REPAIR
            | step::VALIDATE_CANDIDATE_INITIAL
            | step::VALIDATE_CANDIDATE_REPAIR
            | step::REVIEW_AFTER_INTEGRATION_REPAIR
            | step::VALIDATE_AFTER_INTEGRATION_REPAIR
            | step::INVESTIGATE_ITEM
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PendingInvestigationRef {
    pub(super) ref_name: String,
    pub(super) commit_oid: String,
}

pub(super) async fn bind_dispatch_subjects_if_needed(
    state: &AppState,
    project: &Project,
    revision: &ItemRevision,
    jobs: &[Job],
    job: &mut Job,
    precreated_authoring_workspace: &mut Option<Workspace>,
) -> Result<Option<PendingInvestigationRef>, ApiError> {
    let paths = refresh_project_mirror(state, project).await?;
    let repo_path = paths.mirror_git_dir.as_path();

    if job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ingot_domain::job::ExecutionPermission::MayMutate
        && job.job_input.head_commit_oid().is_none()
    {
        let resolved_head = resolve_ref_oid(repo_path, &revision.target_ref)
            .await
            .map_err(git_to_internal)?
            .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.clone()))?;
        job.job_input = ingot_domain::job::JobInput::authoring_head(resolved_head);
        let workspace = ensure_authoring_workspace(state, project, revision, job).await?;
        *precreated_authoring_workspace = Some(workspace);
        return Ok(None);
    }

    let mut base_commit_oid = job.job_input.base_commit_oid().map(ToOwned::to_owned);
    let mut head_commit_oid = job.job_input.head_commit_oid().map(ToOwned::to_owned);

    if should_fill_candidate_subject_from_workspace(&job.step_id) {
        if base_commit_oid.is_none() {
            base_commit_oid = effective_authoring_base_commit_oid(state, revision).await?;
        }
        if head_commit_oid.is_none() {
            head_commit_oid =
                current_authoring_head_for_revision_with_workspace(state, revision, jobs).await?;
        }
        if let (Some(base_commit_oid), Some(head_commit_oid)) =
            (base_commit_oid.clone(), head_commit_oid.clone())
        {
            job.job_input =
                ingot_domain::job::JobInput::candidate_subject(base_commit_oid, head_commit_oid);
            return Ok(None);
        }
    }

    if job.step_id == step::INVESTIGATE_ITEM
        && (base_commit_oid.is_none() || head_commit_oid.is_none())
    {
        if let Some(seed_commit_oid) = revision.seed_commit_oid.clone() {
            job.job_input = ingot_domain::job::JobInput::candidate_subject(
                seed_commit_oid.clone(),
                seed_commit_oid,
            );
            return Ok(None);
        }

        let resolved_head = resolve_ref_oid(repo_path, &revision.target_ref)
            .await
            .map_err(git_to_internal)?
            .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.clone()))?;
        let ref_name = investigation_ref_name(job.id);
        job.job_input = ingot_domain::job::JobInput::candidate_subject(
            resolved_head.clone(),
            resolved_head.clone(),
        );
        return Ok(Some(PendingInvestigationRef {
            ref_name,
            commit_oid: resolved_head,
        }));
    }

    if should_fill_candidate_subject_from_workspace(&job.step_id)
        && !(base_commit_oid.is_some() && head_commit_oid.is_some())
    {
        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Incomplete candidate subject for step: {}",
            job.step_id
        ))
        .into());
    }

    Ok(None)
}

pub(super) async fn apply_pending_investigation_ref_or_cleanup(
    state: &AppState,
    project: &Project,
    job_id: JobId,
    pending_investigation_ref: Option<&PendingInvestigationRef>,
    precreated_authoring_workspace: Option<&Workspace>,
) -> Result<(), ApiError> {
    let Some(pending_investigation_ref) = pending_investigation_ref else {
        return Ok(());
    };
    if let Err(error) = plan_and_apply_investigation_ref(
        state,
        project.id,
        job_id.to_string(),
        &pending_investigation_ref.ref_name,
        &pending_investigation_ref.commit_oid,
    )
    .await
    {
        cleanup_failed_dispatch_side_effects(
            state,
            project,
            precreated_authoring_workspace,
            Some(&pending_investigation_ref.ref_name),
        )
        .await;
        let _ = sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job_id.to_string())
            .execute(&state.db.pool)
            .await;
        return Err(error);
    }
    Ok(())
}

pub(super) async fn link_job_to_workspace_or_cleanup(
    state: &AppState,
    project: &Project,
    job: &mut Job,
    workspace: Workspace,
    investigation_ref_name: Option<&PendingInvestigationRef>,
    cleanup_workspace_on_failure: bool,
) -> Result<(), ApiError> {
    job.workspace_id = Some(workspace.id);
    if let Err(error) = state.db.update_job(job).await {
        cleanup_failed_dispatch_side_effects(
            state,
            project,
            cleanup_workspace_on_failure.then_some(&workspace),
            investigation_ref_name.map(|pending| pending.ref_name.as_str()),
        )
        .await;
        let _ = sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job.id.to_string())
            .execute(&state.db.pool)
            .await;
        return Err(repo_to_internal(error));
    }
    Ok(())
}

pub(super) async fn cleanup_failed_dispatch_side_effects(
    state: &AppState,
    project: &Project,
    precreated_authoring_workspace: Option<&Workspace>,
    investigation_ref_name: Option<&str>,
) {
    let mirror_paths = refresh_project_mirror(state, project).await.ok();

    if let Some(workspace) = precreated_authoring_workspace {
        if let Some(paths) = mirror_paths.as_ref() {
            let _ = ingot_workspace::remove_workspace(
                paths.mirror_git_dir.as_path(),
                FsPath::new(&workspace.path),
            )
            .await;
            if let Some(workspace_ref) = workspace.workspace_ref.as_deref() {
                let _ = delete_ref(paths.mirror_git_dir.as_path(), workspace_ref).await;
            }
        }
        let _ = sqlx::query("DELETE FROM workspaces WHERE id = ?")
            .bind(workspace.id.to_string())
            .execute(&state.db.pool)
            .await;
    }

    if let Some(ref_name) = investigation_ref_name {
        if let Some(paths) = mirror_paths.as_ref() {
            let _ = delete_ref(paths.mirror_git_dir.as_path(), ref_name).await;
        }
        let _ = sqlx::query(
            "DELETE FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(ref_name)
        .execute(&state.db.pool)
        .await;
    }
}

pub(super) async fn plan_and_apply_investigation_ref(
    state: &AppState,
    project_id: ProjectId,
    entity_id: String,
    ref_name: &str,
    commit_oid: &str,
) -> Result<(), ApiError> {
    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id,
        operation_kind: OperationKind::CreateInvestigationRef,
        entity_type: GitEntityType::Job,
        entity_id,
        workspace_id: None,
        ref_name: Some(ref_name.into()),
        expected_old_oid: None,
        new_oid: Some(commit_oid.into()),
        commit_oid: Some(commit_oid.into()),
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: Utc::now(),
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project_id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(state, &project).await?;
    update_ref(paths.mirror_git_dir.as_path(), ref_name, commit_oid)
        .await
        .map_err(git_to_internal)?;
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    Ok(())
}

pub(super) async fn maybe_cleanup_investigation_ref(
    state: &AppState,
    project_id: ProjectId,
    finding: &Finding,
) -> Result<(), ApiError> {
    if finding.source_step_id != step::INVESTIGATE_ITEM
        || finding.source_subject_kind != ingot_domain::finding::FindingSubjectKind::Candidate
    {
        return Ok(());
    }

    let remaining_unresolved = state
        .db
        .list_findings_by_item(finding.source_item_id)
        .await
        .map_err(repo_to_internal)?
        .into_iter()
        .any(|candidate| {
            candidate.source_job_id == finding.source_job_id
                && candidate.triage_state.is_unresolved()
        });
    if remaining_unresolved {
        return Ok(());
    }

    let ref_name = investigation_ref_name(finding.source_job_id);
    let project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(state, &project).await?;
    let existing_oid = resolve_ref_oid(paths.mirror_git_dir.as_path(), &ref_name)
        .await
        .map_err(git_to_internal)?;
    let Some(existing_oid) = existing_oid else {
        return Ok(());
    };

    let mut operation = GitOperation {
        id: ingot_domain::ids::GitOperationId::new(),
        project_id,
        operation_kind: OperationKind::RemoveInvestigationRef,
        entity_type: GitEntityType::Job,
        entity_id: finding.source_job_id.to_string(),
        workspace_id: None,
        ref_name: Some(ref_name.clone()),
        expected_old_oid: Some(existing_oid),
        new_oid: None,
        commit_oid: None,
        status: GitOperationStatus::Planned,
        metadata: None,
        created_at: Utc::now(),
        completed_at: None,
    };
    state
        .db
        .create_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    append_activity(
        state,
        project_id,
        ActivityEventType::GitOperationPlanned,
        "git_operation",
        operation.id,
        serde_json::json!({ "operation_kind": operation.operation_kind, "entity_id": operation.entity_id }),
    )
    .await?;
    delete_ref(paths.mirror_git_dir.as_path(), &ref_name)
        .await
        .map_err(git_to_internal)?;
    operation.status = GitOperationStatus::Applied;
    operation.completed_at = Some(Utc::now());
    state
        .db
        .update_git_operation(&operation)
        .await
        .map_err(repo_to_internal)?;
    Ok(())
}

pub(super) async fn auto_dispatch_projected_review_job(
    state: &AppState,
    project: &Project,
    item_id: ItemId,
) -> Result<Option<Job>, ApiError> {
    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;
    auto_dispatch_projected_review_job_locked(state, project, item_id).await
}

pub(super) async fn auto_dispatch_projected_review_job_locked(
    state: &AppState,
    project: &Project,
    item_id: ItemId,
) -> Result<Option<Job>, ApiError> {
    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let paths = refresh_project_mirror(state, project).await?;
    let convergences =
        hydrate_convergence_validity(paths.mirror_git_dir.as_path(), convergences).await?;
    let evaluation =
        Evaluator::new().evaluate(&item, &current_revision, &jobs, &findings, &convergences);
    let Some(step_id) = evaluation.dispatchable_step_id.as_deref() else {
        return Ok(None);
    };

    if !step::is_closure_relevant_review_step(step_id) {
        return Ok(None);
    }

    let mut job = dispatch_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
        DispatchJobCommand {
            step_id: Some(step_id.to_string()),
        },
    )?;
    let mut precreated_authoring_workspace = None;
    let pending_investigation_ref = bind_dispatch_subjects_if_needed(
        state,
        project,
        &current_revision,
        &jobs,
        &mut job,
        &mut precreated_authoring_workspace,
    )
    .await?;
    if let Err(error) = state.db.create_job(&job).await {
        cleanup_failed_dispatch_side_effects(
            state,
            project,
            precreated_authoring_workspace.as_ref(),
            pending_investigation_ref
                .as_ref()
                .map(|pending| pending.ref_name.as_str()),
        )
        .await;
        return Err(repo_to_internal(error));
    }
    apply_pending_investigation_ref_or_cleanup(
        state,
        project,
        job.id,
        pending_investigation_ref.as_ref(),
        precreated_authoring_workspace.as_ref(),
    )
    .await?;
    append_activity(
        state,
        project.id,
        ActivityEventType::JobDispatched,
        "job",
        job.id,
        serde_json::json!({ "item_id": item.id, "step_id": job.step_id }),
    )
    .await?;

    Ok(Some(job))
}

pub(super) async fn retry_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id, job_id)): Path<(String, String, String)>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
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

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let current_revision = state
        .db
        .get_revision(item.current_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let previous_job = jobs
        .iter()
        .find(|job| job.id == job_id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        })?;
    let convergences = state
        .db
        .list_convergences_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let convergences =
        hydrate_convergence_validity(paths.mirror_git_dir.as_path(), convergences).await?;

    let mut job = retry_job(
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
        &previous_job,
    )?;
    let mut precreated_authoring_workspace = None;
    let pending_investigation_ref = bind_dispatch_subjects_if_needed(
        &state,
        &project,
        &current_revision,
        &jobs,
        &mut job,
        &mut precreated_authoring_workspace,
    )
    .await?;
    if let Err(error) = state.db.create_job(&job).await {
        cleanup_failed_dispatch_side_effects(
            &state,
            &project,
            precreated_authoring_workspace.as_ref(),
            pending_investigation_ref
                .as_ref()
                .map(|pending| pending.ref_name.as_str()),
        )
        .await;
        return Err(repo_to_internal(error));
    }
    apply_pending_investigation_ref_or_cleanup(
        &state,
        &project,
        job.id,
        pending_investigation_ref.as_ref(),
        precreated_authoring_workspace.as_ref(),
    )
    .await?;
    if let Some(workspace) = precreated_authoring_workspace {
        link_job_to_workspace_or_cleanup(
            &state,
            &project,
            &mut job,
            workspace,
            pending_investigation_ref.as_ref(),
            true,
        )
        .await?;
    } else if job.workspace_kind == WorkspaceKind::Authoring {
        let had_existing_workspace = state
            .db
            .find_authoring_workspace_for_revision(current_revision.id)
            .await
            .map_err(repo_to_internal)?
            .is_some();
        let workspace =
            ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
        link_job_to_workspace_or_cleanup(
            &state,
            &project,
            &mut job,
            workspace,
            pending_investigation_ref.as_ref(),
            !had_existing_workspace,
        )
        .await?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        "job",
        job.id,
        serde_json::json!({
            "item_id": item.id,
            "step_id": job.step_id,
            "supersedes_job_id": previous_job.id,
            "retry_no": job.retry_no
        }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(job)))
}

pub(super) async fn cancel_item_job(
    State(state): State<AppState>,
    Path((project_id, item_id, job_id)): Path<(String, String, String)>,
) -> Result<Json<()>, ApiError> {
    let project_id = parse_id::<ProjectId>(&project_id, "project")?;
    let item_id = parse_id::<ItemId>(&item_id, "item")?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    let item = state.db.get_item(item_id).await.map_err(repo_to_item)?;
    if item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.item_id != item.id {
        return Err(ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        });
    }
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job cancellation does not match the current item revision".into(),
        )
        .into());
    }

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Cancelled,
            outcome_class: Some(OutcomeClass::Cancelled),
            error_code: Some("operator_cancelled"),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(repo_to_job_failure)?;

    if let Some(workspace_id) = job.workspace_id {
        let mut workspace = state
            .db
            .get_workspace(workspace_id)
            .await
            .map_err(repo_to_internal)?;
        workspace.current_job_id = None;
        if workspace.status == ingot_domain::workspace::WorkspaceStatus::Busy {
            workspace.status = ingot_domain::workspace::WorkspaceStatus::Ready;
        }
        workspace.updated_at = Utc::now();
        state
            .db
            .update_workspace(&workspace)
            .await
            .map_err(repo_to_internal)?;
    }

    refresh_revision_context_for_job(&state, job.id).await?;
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobCancelled,
        "job",
        job.id,
        serde_json::json!({ "item_id": item.id }),
    )
    .await?;

    Ok(Json(()))
}

pub(super) async fn assign_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<AssignJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let agent_id = parse_id::<AgentId>(&request.agent_id, "agent")?;
    let mut job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.status == JobStatus::Assigned {
        return Ok(Json(job));
    }
    if job.status != JobStatus::Queued {
        return Err(ApiError::Conflict {
            code: "job_not_assignable",
            message: "Only queued jobs can be assigned".into(),
        });
    }
    if job.workspace_kind != WorkspaceKind::Authoring {
        return Err(ApiError::BadRequest {
            code: "unsupported_workspace_kind",
            message: "This milestone only provisions authoring workspaces".into(),
        });
    }

    let agent = state.db.get_agent(agent_id).await.map_err(repo_to_agent)?;
    if agent.status != AgentStatus::Available {
        return Err(ApiError::Conflict {
            code: "agent_unavailable",
            message: "Agent is not available".into(),
        });
    }

    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if item.current_revision_id != job.item_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job assignment does not match the current item revision".into(),
        )
        .into());
    }
    let revision = state
        .db
        .get_revision(job.item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(job.project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;
    let workspace = ensure_authoring_workspace(&state, &project, &revision, &job).await?;

    job.status = JobStatus::Assigned;
    job.workspace_id = Some(workspace.id);
    job.agent_id = Some(agent.id);
    state.db.update_job(&job).await.map_err(repo_to_internal)?;

    Ok(Json(job))
}

pub(super) async fn start_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<StartJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let mut job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if job.status == JobStatus::Running {
        return Ok(Json(job));
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(job.project_id)
        .await;
    let lease_expires_at =
        Utc::now() + chrono::Duration::seconds(request.lease_duration_seconds.unwrap_or(1800));
    state
        .db
        .start_job_execution(StartJobExecutionParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            workspace_id: job.workspace_id,
            agent_id: job.agent_id,
            lease_owner_id: &request.lease_owner_id,
            process_pid: request.process_pid,
            lease_expires_at,
        })
        .await
        .map_err(repo_to_job_failure)?;
    job = state.db.get_job(job.id).await.map_err(repo_to_internal)?;
    Ok(Json(job))
}

pub(super) async fn heartbeat_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<HeartbeatJobRequest>,
) -> Result<Json<Job>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(job.project_id)
        .await;
    let lease_expires_at =
        Utc::now() + chrono::Duration::seconds(request.lease_duration_seconds.unwrap_or(1800));
    state
        .db
        .heartbeat_job_execution(
            job.id,
            item.id,
            job.item_revision_id,
            &request.lease_owner_id,
            lease_expires_at,
        )
        .await
        .map_err(repo_to_job_failure)?;
    let job = state.db.get_job(job.id).await.map_err(repo_to_internal)?;
    Ok(Json(job))
}

pub(super) async fn get_job_logs(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<JobLogsResponse>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let logs_dir = logs_root(state.state_root.as_path()).join(job_id.to_string());

    let prompt = read_optional_text(logs_dir.join("prompt.txt")).await?;
    let stdout = read_optional_text(logs_dir.join("stdout.log")).await?;
    let stderr = read_optional_text(logs_dir.join("stderr.log")).await?;
    let result = read_optional_json(logs_dir.join("result.json")).await?;

    Ok(Json(JobLogsResponse {
        prompt,
        stdout,
        stderr,
        result,
    }))
}

pub(super) async fn complete_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<CompleteJobRequest>,
) -> Result<Json<CompleteJobResponse>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let prior_job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let prior_item = state
        .db
        .get_item(prior_job.item_id)
        .await
        .map_err(repo_to_item)?;
    let project = state
        .db
        .get_project(prior_job.project_id)
        .await
        .map_err(repo_to_project)?;
    refresh_project_mirror(&state, &project).await?;
    let result = state
        .complete_job_service
        .execute(CompleteJobCommand {
            job_id,
            outcome_class: request.outcome_class,
            result_schema_version: request.result_schema_version,
            result_payload: request.result_payload,
            output_commit_oid: request.output_commit_oid,
        })
        .await
        .map_err(complete_job_error_to_api_error)?;
    refresh_revision_context_for_job(&state, job_id).await?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobCompleted,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "outcome": job.outcome_class }),
    )
    .await?;
    if prior_item.escalation_state == ingot_domain::item::EscalationState::OperatorRequired
        && item.current_revision_id == job.item_revision_id
        && item.escalation_state == ingot_domain::item::EscalationState::None
        && item.escalation_reason.is_none()
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            item.id,
            serde_json::json!({ "reason": "successful_retry", "job_id": job.id }),
        )
        .await?;
    }
    if job.step_id == "validate_integrated"
        && job.outcome_class == Some(OutcomeClass::Clean)
        && item.approval_state == ApprovalState::Pending
    {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ApprovalRequested,
            "item",
            item.id,
            serde_json::json!({ "job_id": job.id }),
        )
        .await?;
    }
    if let Err(error) = auto_dispatch_projected_review_job(&state, &project, item.id).await {
        warn!(
            ?error,
            project_id = %project.id,
            item_id = %item.id,
            job_id = %job.id,
            "projected review auto-dispatch failed after job completion"
        );
    }

    Ok(Json(CompleteJobResponse {
        finding_count: result.finding_count,
    }))
}

pub(super) async fn fail_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<FailJobRequest>,
) -> Result<Json<()>, ApiError> {
    let status = failure_status(request.outcome_class)?;
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job failure does not match the current item revision".into(),
        )
        .into());
    }
    let escalation_reason = failure_escalation_reason(&job, request.outcome_class);

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status,
            outcome_class: Some(request.outcome_class),
            error_code: request.error_code.as_deref(),
            error_message: request.error_message.as_deref(),
            escalation_reason,
        })
        .await
        .map_err(repo_to_job_failure)?;
    refresh_revision_context_for_job(&state, job.id).await?;
    if escalation_reason.is_some() {
        append_activity(
            &state,
            job.project_id,
            ActivityEventType::ItemEscalated,
            "item",
            item.id,
            serde_json::json!({ "reason": escalation_reason }),
        )
        .await?;
    }
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobFailed,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "error_code": request.error_code }),
    )
    .await?;

    Ok(Json(()))
}

pub(super) async fn expire_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<()>, ApiError> {
    let job_id = parse_id::<JobId>(&job_id, "job")?;
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    if !job.status.is_active() {
        return Err(UseCaseError::JobNotActive.into());
    }
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    if job.item_revision_id != item.current_revision_id {
        return Err(UseCaseError::ProtocolViolation(
            "job expiration does not match the current item revision".into(),
        )
        .into());
    }

    state
        .db
        .finish_job_non_success(FinishJobNonSuccessParams {
            job_id: job.id,
            item_id: item.id,
            expected_item_revision_id: job.item_revision_id,
            status: JobStatus::Expired,
            outcome_class: Some(OutcomeClass::TransientFailure),
            error_code: Some("job_expired"),
            error_message: None,
            escalation_reason: None,
        })
        .await
        .map_err(repo_to_job_expiration)?;
    refresh_revision_context_for_job(&state, job.id).await?;
    append_activity(
        &state,
        job.project_id,
        ActivityEventType::JobFailed,
        "job",
        job.id,
        serde_json::json!({ "item_id": job.item_id, "error_code": "job_expired" }),
    )
    .await?;

    Ok(Json(()))
}

pub(super) async fn refresh_revision_context_for_job(state: &AppState, job_id: JobId) -> Result<(), ApiError> {
    let job = state.db.get_job(job_id).await.map_err(repo_to_internal)?;
    let item = state.db.get_item(job.item_id).await.map_err(repo_to_item)?;
    let revision = state
        .db
        .get_revision(job.item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(job.project_id)
        .await
        .map_err(repo_to_project)?;
    let paths = refresh_project_mirror(state, &project).await?;
    refresh_revision_context_for_job_like(state, &item, &revision, paths.mirror_git_dir.as_path())
        .await
}

pub(super) async fn refresh_revision_context_for_job_like(
    state: &AppState,
    item: &Item,
    revision: &ItemRevision,
    repo_path: &FsPath,
) -> Result<(), ApiError> {
    let jobs = state
        .db
        .list_jobs_by_item(item.id)
        .await
        .map_err(repo_to_internal)?;
    let authoring_head_commit_oid =
        current_authoring_head_for_revision_with_workspace(state, revision, &jobs).await?;
    let authoring_base_commit_oid = effective_authoring_base_commit_oid(state, revision).await?;
    let changed_paths = if let (Some(base_commit_oid), Some(head_commit_oid)) = (
        authoring_base_commit_oid.as_deref(),
        authoring_head_commit_oid.as_deref(),
    ) {
        changed_paths_between(repo_path, base_commit_oid, head_commit_oid)
            .await
            .map_err(git_to_internal)?
    } else {
        Vec::new()
    };
    let context = rebuild_revision_context(
        item,
        revision,
        &jobs,
        authoring_head_commit_oid,
        changed_paths,
        jobs.first().map(|job| job.id),
        Utc::now(),
    );
    state
        .db
        .upsert_revision_context(&context)
        .await
        .map_err(repo_to_internal)?;
    Ok(())
}

pub(super) fn failure_status(outcome_class: OutcomeClass) -> Result<JobStatus, ApiError> {
    match outcome_class {
        OutcomeClass::TransientFailure
        | OutcomeClass::TerminalFailure
        | OutcomeClass::ProtocolViolation => Ok(JobStatus::Failed),
        OutcomeClass::Cancelled => Ok(JobStatus::Cancelled),
        OutcomeClass::Clean | OutcomeClass::Findings => Err(ApiError::BadRequest {
            code: "invalid_outcome_class",
            message:
                "Failure endpoints only accept transient_failure, terminal_failure, protocol_violation, or cancelled"
                    .into(),
        }),
    }
}

pub(super) fn failure_escalation_reason(job: &Job, outcome_class: OutcomeClass) -> Option<EscalationReason> {
    if !is_closure_relevant_job(job) {
        return None;
    }

    match outcome_class {
        OutcomeClass::TerminalFailure => Some(EscalationReason::StepFailed),
        OutcomeClass::ProtocolViolation => Some(EscalationReason::ProtocolViolation),
        OutcomeClass::Clean
        | OutcomeClass::Findings
        | OutcomeClass::TransientFailure
        | OutcomeClass::Cancelled => None,
    }
}

pub(super) fn is_closure_relevant_job(job: &Job) -> bool {
    matches!(
        ingot_workflow::step::find_step(&job.step_id).map(|step| step.closure_relevance),
        Some(ingot_workflow::ClosureRelevance::ClosureRelevant)
    )
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::Utc;
    
    use ingot_domain::git_operation::{
        GitEntityType, GitOperation, GitOperationStatus, OperationKind,
    };
    use ingot_domain::ids::{
        GitOperationId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId,
    };
    use ingot_domain::job::{
        ExecutionPermission, Job, JobInput, JobStatus, OutcomeClass, OutputArtifactKind, PhaseKind,
    };
    use ingot_domain::project::Project;
    use ingot_domain::workspace::{Workspace, WorkspaceKind, WorkspaceStatus};
    use ingot_git::GitJobCompletionPort;
    use ingot_git::commands::resolve_ref_oid;
    use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
    use ingot_store_sqlite::Database;
    use ingot_test_support::fixtures::{DEFAULT_TEST_TIMESTAMP, ItemBuilder, JobBuilder, RevisionBuilder};
    use ingot_test_support::git::{
        git_output as support_git_output, run_git as support_git,
        temp_git_repo as support_temp_git_repo, write_file as support_write_file,
    };
    use ingot_usecases::{CompleteJobService, ProjectLocks, UseCaseError};
    use ingot_workflow::step;
    use uuid::Uuid;

    use crate::error::ApiError;
    use crate::router::AppState;

    use std::path::Path as FsPath;

    const TS: &str = DEFAULT_TEST_TIMESTAMP;

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git(path: &PathBuf, args: &[&str]) {
        support_git(path, args);
    }

    fn git_output(path: &PathBuf, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    fn write_file(path: &PathBuf, contents: &str) {
        support_write_file(path, contents);
    }

    fn test_project(path: PathBuf) -> Project {
        Project {
            id: ProjectId::from_uuid(Uuid::nil()),
            name: "Test".into(),
            path: path.display().to_string(),
            default_branch: "main".into(),
            color: "#000000".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    async fn test_app_state() -> AppState {
        let db_path =
            std::env::temp_dir().join(format!("ingot-http-api-test-{}.db", Uuid::now_v7()));
        let db = Database::connect(&db_path).await.expect("connect db");
        db.migrate().await.expect("migrate db");
        let state_root =
            std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));
        let resolver_state_root = state_root.clone();
        AppState {
            db: db.clone(),
            complete_job_service: CompleteJobService::with_repo_path_resolver(
                db,
                GitJobCompletionPort,
                ProjectLocks::default(),
                Arc::new(move |project: &Project| {
                    project_repo_paths(
                        resolver_state_root.as_path(),
                        project.id,
                        FsPath::new(&project.path),
                    )
                    .mirror_git_dir
                }),
            ),
            project_locks: ProjectLocks::default(),
            state_root,
        }
    }

    fn test_job(step_id: &str, output_artifact_kind: OutputArtifactKind) -> Job {
        JobBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
            step_id,
        )
        .phase_kind(PhaseKind::Author)
        .workspace_kind(WorkspaceKind::Authoring)
        .execution_permission(ExecutionPermission::MayMutate)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head("head"))
        .output_artifact_kind(output_artifact_kind)
        .build()
    }

    #[test]
    fn failure_status_maps_cancelled_to_cancelled_and_failures_to_failed() {
        assert!(matches!(
            failure_status(OutcomeClass::Cancelled),
            Ok(JobStatus::Cancelled)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::TransientFailure),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::TerminalFailure),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::ProtocolViolation),
            Ok(JobStatus::Failed)
        ));
        assert!(matches!(
            failure_status(OutcomeClass::Clean),
            Err(ApiError::BadRequest {
                code: "invalid_outcome_class",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn bind_dispatch_subjects_if_needed_does_not_persist_investigation_ref_before_job_creation()
     {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let revision = RevisionBuilder::new(ItemId::from_uuid(Uuid::now_v7()))
            .seed_target_commit_oid(Some(head.clone()))
            .build();
        let mut job = test_job(step::INVESTIGATE_ITEM, OutputArtifactKind::FindingReport);
        job.project_id = project.id;
        job.item_revision_id = revision.id;
        job.workspace_kind = WorkspaceKind::Review;
        job.execution_permission = ExecutionPermission::MustNotMutate;
        job.phase_kind = PhaseKind::Investigate;
        job.job_input = ingot_domain::job::JobInput::None;

        let mut precreated_authoring_workspace = None;
        let pending_investigation_ref = bind_dispatch_subjects_if_needed(
            &state,
            &project,
            &revision,
            &[],
            &mut job,
            &mut precreated_authoring_workspace,
        )
        .await
        .expect("bind dispatch subjects");

        let pending_investigation_ref =
            pending_investigation_ref.expect("expected pending investigation ref");
        assert!(precreated_authoring_workspace.is_none());
        assert_eq!(job.job_input.base_commit_oid(), Some(head.as_str()));
        assert_eq!(job.job_input.head_commit_oid(), Some(head.as_str()));

        let paths = project_repo_paths(state.state_root.as_path(), project.id, &repo);
        ensure_mirror(&paths).await.expect("ensure mirror");
        assert_eq!(
            resolve_ref_oid(
                paths.mirror_git_dir.as_path(),
                &pending_investigation_ref.ref_name
            )
            .await
            .expect("resolve investigation ref"),
            None
        );
        let operation_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(&pending_investigation_ref.ref_name)
        .fetch_one(&state.db.pool)
        .await
        .expect("git operation count");
        assert_eq!(operation_count, 0);
    }

    #[tokio::test]
    async fn bind_dispatch_subjects_if_needed_falls_back_when_workspace_subject_is_partial() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7()))
            .build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .seed_target_commit_oid(Some(head.clone()))
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item with revision");
        let partial_workspace = Workspace {
            id: WorkspaceId::from_uuid(Uuid::now_v7()),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
            path: state
                .state_root
                .join(format!("partial-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            )),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: None,
            retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_workspace(&partial_workspace)
            .await
            .expect("create partial workspace");

        let mut job = test_job(step::INVESTIGATE_ITEM, OutputArtifactKind::FindingReport);
        job.project_id = project.id;
        job.item_revision_id = revision.id;
        job.workspace_kind = WorkspaceKind::Review;
        job.execution_permission = ExecutionPermission::MustNotMutate;
        job.phase_kind = PhaseKind::Investigate;
        job.job_input = ingot_domain::job::JobInput::None;

        let mut precreated_authoring_workspace = None;
        let pending_investigation_ref = bind_dispatch_subjects_if_needed(
            &state,
            &project,
            &revision,
            &[],
            &mut job,
            &mut precreated_authoring_workspace,
        )
        .await
        .expect("bind dispatch subjects");

        let pending_investigation_ref =
            pending_investigation_ref.expect("expected pending investigation ref");
        assert!(precreated_authoring_workspace.is_none());
        assert_eq!(job.job_input.base_commit_oid(), Some(head.as_str()));
        assert_eq!(job.job_input.head_commit_oid(), Some(head.as_str()));

        let paths = project_repo_paths(state.state_root.as_path(), project.id, &repo);
        ensure_mirror(&paths).await.expect("ensure mirror");
        assert_eq!(
            resolve_ref_oid(
                paths.mirror_git_dir.as_path(),
                &pending_investigation_ref.ref_name
            )
            .await
            .expect("resolve investigation ref"),
            None
        );
    }

    #[tokio::test]
    async fn bind_dispatch_subjects_if_needed_rejects_partial_review_subject() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7()))
            .build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .seed_target_commit_oid(Some(head.clone()))
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item with revision");
        let partial_workspace = Workspace {
            id: WorkspaceId::from_uuid(Uuid::now_v7()),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
            path: state
                .state_root
                .join(format!("partial-review-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: Some(revision.id),
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            )),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: None,
            retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_workspace(&partial_workspace)
            .await
            .expect("create partial workspace");

        let mut job = test_job(
            step::REVIEW_INCREMENTAL_INITIAL,
            OutputArtifactKind::ReviewReport,
        );
        job.project_id = project.id;
        job.item_revision_id = revision.id;
        job.workspace_kind = WorkspaceKind::Review;
        job.execution_permission = ExecutionPermission::MustNotMutate;
        job.phase_kind = PhaseKind::Review;
        job.job_input = ingot_domain::job::JobInput::None;

        let mut precreated_authoring_workspace = None;
        let result = bind_dispatch_subjects_if_needed(
            &state,
            &project,
            &revision,
            &[],
            &mut job,
            &mut precreated_authoring_workspace,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::IllegalStepDispatch(message)))
                if message.contains("Incomplete candidate subject")
        ));
    }

    #[tokio::test]
    async fn bind_dispatch_subjects_if_needed_rejects_review_subject_when_both_commits_are_missing()
    {
        let repo = temp_git_repo();
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7()))
            .build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item with revision");

        let mut job = test_job(
            step::REVIEW_CANDIDATE_INITIAL,
            OutputArtifactKind::ReviewReport,
        );
        job.project_id = project.id;
        job.item_revision_id = revision.id;
        job.workspace_kind = WorkspaceKind::Review;
        job.execution_permission = ExecutionPermission::MustNotMutate;
        job.phase_kind = PhaseKind::Review;
        job.job_input = ingot_domain::job::JobInput::None;

        let mut precreated_authoring_workspace = None;
        let result = bind_dispatch_subjects_if_needed(
            &state,
            &project,
            &revision,
            &[],
            &mut job,
            &mut precreated_authoring_workspace,
        )
        .await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::IllegalStepDispatch(message)))
                if message.contains("Incomplete candidate subject")
        ));
    }

    #[tokio::test]
    async fn cleanup_failed_dispatch_side_effects_removes_workspace_and_investigation_ref() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let paths = project_repo_paths(state.state_root.as_path(), project.id, &repo);
        ensure_mirror(&paths).await.expect("ensure mirror");

        let workspace_id = WorkspaceId::from_uuid(Uuid::now_v7());
        let workspace_ref = format!("refs/ingot/workspaces/{workspace_id}");
        git(
            &paths.mirror_git_dir,
            &["update-ref", &workspace_ref, &head],
        );
        let workspace_path = state
            .state_root
            .join(format!("cleanup-workspace-{}", Uuid::now_v7()));
        git(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                &workspace_ref,
            ],
        );
        let workspace = Workspace {
            id: workspace_id,
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: None,
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(workspace_ref.clone()),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: Some(head.clone()),
            retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_workspace(&workspace)
            .await
            .expect("create workspace row");

        let investigation_ref = format!(
            "refs/ingot/investigations/{}",
            JobId::from_uuid(Uuid::now_v7())
        );
        git(
            &paths.mirror_git_dir,
            &["update-ref", &investigation_ref, &head],
        );
        state
            .db
            .create_git_operation(&GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                operation_kind: OperationKind::CreateInvestigationRef,
                entity_type: GitEntityType::Job,
                entity_id: JobId::from_uuid(Uuid::now_v7()).to_string(),
                workspace_id: None,
                ref_name: Some(investigation_ref.clone()),
                expected_old_oid: None,
                new_oid: Some(head.clone()),
                commit_oid: Some(head.clone()),
                status: GitOperationStatus::Applied,
                metadata: None,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
            })
            .await
            .expect("create git operation");

        cleanup_failed_dispatch_side_effects(
            &state,
            &project,
            Some(&workspace),
            Some(&investigation_ref),
        )
        .await;

        assert!(!workspace_path.exists(), "workspace path removed");
        assert_eq!(
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &workspace_ref)
                .await
                .expect("resolve workspace ref"),
            None
        );
        assert_eq!(
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &investigation_ref)
                .await
                .expect("resolve investigation ref"),
            None
        );
        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(workspace.id.to_string())
                .fetch_one(&state.db.pool)
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 0);
        let op_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(&investigation_ref)
        .fetch_one(&state.db.pool)
        .await
        .expect("operation count");
        assert_eq!(op_count, 0);
    }

    #[tokio::test]
    async fn cleanup_failed_dispatch_side_effects_deletes_db_rows_when_mirror_refresh_fails() {
        let state = test_app_state().await;
        let missing_repo = state
            .state_root
            .join(format!("missing-repo-{}", Uuid::now_v7()));
        let project = test_project(missing_repo);
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let workspace = Workspace {
            id: WorkspaceId::from_uuid(Uuid::now_v7()),
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
            path: state
                .state_root
                .join(format!("orphaned-workspace-{}", Uuid::now_v7()))
                .display()
                .to_string(),
            created_for_revision_id: None,
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            )),
            base_commit_oid: Some("deadbeef".repeat(5)),
            head_commit_oid: Some("deadbeef".repeat(5)),
            retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_workspace(&workspace)
            .await
            .expect("create workspace row");

        let investigation_ref = format!(
            "refs/ingot/investigations/{}",
            JobId::from_uuid(Uuid::now_v7())
        );
        state
            .db
            .create_git_operation(&GitOperation {
                id: GitOperationId::new(),
                project_id: project.id,
                operation_kind: OperationKind::CreateInvestigationRef,
                entity_type: GitEntityType::Job,
                entity_id: JobId::from_uuid(Uuid::now_v7()).to_string(),
                workspace_id: None,
                ref_name: Some(investigation_ref.clone()),
                expected_old_oid: None,
                new_oid: Some("deadbeef".repeat(5)),
                commit_oid: Some("deadbeef".repeat(5)),
                status: GitOperationStatus::Applied,
                metadata: None,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
            })
            .await
            .expect("create git operation");

        cleanup_failed_dispatch_side_effects(
            &state,
            &project,
            Some(&workspace),
            Some(&investigation_ref),
        )
        .await;

        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(workspace.id.to_string())
                .fetch_one(&state.db.pool)
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 0);
        let op_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(&investigation_ref)
        .fetch_one(&state.db.pool)
        .await
        .expect("operation count");
        assert_eq!(op_count, 0);
    }

    #[tokio::test]
    async fn link_job_to_workspace_or_cleanup_deletes_job_row_on_update_failure() {
        let repo = temp_git_repo();
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let state = test_app_state().await;
        let project = test_project(repo.clone());
        state
            .db
            .create_project(&project)
            .await
            .expect("create project");

        let paths = project_repo_paths(state.state_root.as_path(), project.id, &repo);
        ensure_mirror(&paths).await.expect("ensure mirror");
        let item_id = ItemId::from_uuid(Uuid::now_v7());
        let revision_id = ItemRevisionId::from_uuid(Uuid::now_v7());
        sqlx::query(
            "INSERT INTO items (
                id, project_id, classification, workflow_version, lifecycle_state, parking_state,
                approval_state, escalation_state, current_revision_id, origin_kind, origin_finding_id,
                priority, labels, created_at, updated_at
             ) VALUES (?, ?, 'change', 'delivery:v1', 'open', 'active', 'not_requested', 'none', ?, 'manual', NULL, 'major', '[]', ?, ?)",
        )
        .bind(item_id.to_string())
        .bind(project.id.to_string())
        .bind(revision_id.to_string())
        .bind(TS)
        .bind(TS)
        .execute(&state.db.pool)
        .await
        .expect("insert item");
        sqlx::query(
            "INSERT INTO item_revisions (
                id, item_id, revision_no, title, description, acceptance_criteria, target_ref,
                approval_policy, policy_snapshot, template_map_snapshot, seed_commit_oid,
                seed_target_commit_oid, supersedes_revision_id, created_at
             ) VALUES (?, ?, 1, 'Title', 'Desc', 'AC', 'refs/heads/main', 'required', '{}', '{}', ?, ?, NULL, ?)",
        )
        .bind(revision_id.to_string())
        .bind(item_id.to_string())
        .bind(Some(head.clone()))
        .bind(Some(head.clone()))
        .bind(TS)
        .execute(&state.db.pool)
        .await
        .expect("insert revision");
        let workspace_id = WorkspaceId::from_uuid(Uuid::now_v7());
        let workspace_ref = format!("refs/ingot/workspaces/{workspace_id}");
        git(
            &paths.mirror_git_dir,
            &["update-ref", &workspace_ref, &head],
        );
        let workspace_path = state
            .state_root
            .join(format!("link-job-workspace-{}", Uuid::now_v7()));
        git(
            &paths.mirror_git_dir,
            &[
                "worktree",
                "add",
                "--detach",
                workspace_path.to_str().expect("workspace path"),
                &workspace_ref,
            ],
        );
        let workspace = Workspace {
            id: workspace_id,
            project_id: project.id,
            kind: WorkspaceKind::Authoring,
            strategy: ingot_domain::workspace::WorkspaceStrategy::Worktree,
            path: workspace_path.display().to_string(),
            created_for_revision_id: None,
            parent_workspace_id: None,
            target_ref: Some("refs/heads/main".into()),
            workspace_ref: Some(workspace_ref.clone()),
            base_commit_oid: Some(head.clone()),
            head_commit_oid: Some(head.clone()),
            retention_policy: ingot_domain::workspace::RetentionPolicy::Persistent,
            status: WorkspaceStatus::Ready,
            current_job_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state
            .db
            .create_workspace(&workspace)
            .await
            .expect("create workspace row");

        let mut job = test_job("author_initial", OutputArtifactKind::Commit);
        job.project_id = project.id;
        job.item_id = item_id;
        job.item_revision_id = revision_id;
        state.db.create_job(&job).await.expect("create job row");
        sqlx::query("DELETE FROM jobs WHERE id = ?")
            .bind(job.id.to_string())
            .execute(&state.db.pool)
            .await
            .expect("delete job row");

        let result = link_job_to_workspace_or_cleanup(
            &state,
            &project,
            &mut job,
            workspace.clone(),
            None,
            true,
        )
        .await;
        assert!(result.is_err(), "update failure should propagate");
        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(workspace.id.to_string())
                .fetch_one(&state.db.pool)
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 0);
        let job_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE id = ?")
            .bind(job.id.to_string())
            .fetch_one(&state.db.pool)
            .await
            .expect("job count");
        assert_eq!(job_count, 0);
        assert_eq!(
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &workspace_ref)
                .await
                .expect("resolve workspace ref"),
            None
        );
    }

}
