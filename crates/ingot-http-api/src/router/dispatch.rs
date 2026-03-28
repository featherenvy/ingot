use super::infra_ports::HttpInfraAdapter;
use super::item_projection::{ItemRuntimeSnapshot, load_item_runtime_snapshot};
use super::items::{
    current_authoring_head_for_revision_with_workspace, effective_authoring_base_commit_oid,
    ensure_authoring_workspace,
};
use super::support::*;
use super::types::*;
use super::*;
use ingot_usecases::dispatch::{PendingInvestigationRef, investigation_ref_name};
use ingot_usecases::job::{DispatchJobCommand, dispatch_job, retry_job};

pub(super) async fn dispatch_item_job(
    State(state): State<AppState>,
    ApiPath(ProjectItemPathParams {
        project_id,
        item_id,
    }): ApiPath<ProjectItemPathParams>,
    maybe_request: Option<Json<DispatchJobRequest>>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
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

    let ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    } = load_item_runtime_snapshot(&state, paths.mirror_git_dir.as_path(), &item).await?;
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

    let infra = HttpInfraAdapter::new(&state);
    persist_dispatched_job(
        &state,
        &infra,
        project_id,
        &job,
        pending_investigation_ref.as_ref(),
        precreated_authoring_workspace.as_ref(),
    )
    .await?;

    if precreated_authoring_workspace.is_none() && job.workspace_kind == WorkspaceKind::Authoring {
        let _ = ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        ActivitySubject::Job(job.id),
        serde_json::json!({ "item_id": item.id, "step_id": job.step_id, "dispatch_origin": "operator" }),
    )
    .await?;

    Ok((StatusCode::CREATED, Json(job)))
}

pub(super) fn should_fill_candidate_subject_from_workspace(
    step_id: ingot_domain::step_id::StepId,
) -> bool {
    ingot_usecases::dispatch::should_fill_candidate_subject_from_workspace(step_id)
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
    let fills_candidate_subject = should_fill_candidate_subject_from_workspace(job.step_id);

    if job.workspace_kind == WorkspaceKind::Authoring
        && job.execution_permission == ingot_domain::job::ExecutionPermission::MayMutate
        && job.job_input.head_commit_oid().is_none()
    {
        let resolved_head = resolve_ref_oid(repo_path, &revision.target_ref)
            .await
            .map_err(git_to_internal)?
            .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.to_string()))?;
        job.job_input = ingot_domain::job::JobInput::authoring_head(resolved_head);
        let workspace = ensure_authoring_workspace(state, project, revision, job).await?;
        *precreated_authoring_workspace = Some(workspace);
        return Ok(None);
    }

    let mut base_commit_oid = job.job_input.base_commit_oid().cloned();
    let mut head_commit_oid = job.job_input.head_commit_oid().cloned();

    if fills_candidate_subject {
        if base_commit_oid.is_none() {
            base_commit_oid = effective_authoring_base_commit_oid(state, revision).await?;
        }
        if head_commit_oid.is_none() {
            head_commit_oid =
                current_authoring_head_for_revision_with_workspace(state, revision, jobs).await?;
        }
        if let (Some(base_commit_oid), Some(head_commit_oid)) =
            (base_commit_oid.as_ref(), head_commit_oid.as_ref())
        {
            job.job_input = ingot_domain::job::JobInput::candidate_subject(
                base_commit_oid.clone(),
                head_commit_oid.clone(),
            );
            return Ok(None);
        }
    }

    if job.step_id == step::INVESTIGATE_ITEM
        && (base_commit_oid.is_none() || head_commit_oid.is_none())
    {
        if let Some(seed_commit_oid) = revision.seed.seed_commit_oid() {
            job.job_input = ingot_domain::job::JobInput::candidate_subject(
                seed_commit_oid.clone(),
                seed_commit_oid.clone(),
            );
            return Ok(None);
        }

        let resolved_head = resolve_ref_oid(repo_path, &revision.target_ref)
            .await
            .map_err(git_to_internal)?
            .ok_or_else(|| UseCaseError::TargetRefUnresolved(revision.target_ref.to_string()))?;
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

    if fills_candidate_subject && !(base_commit_oid.is_some() && head_commit_oid.is_some()) {
        return Err(UseCaseError::IllegalStepDispatch(format!(
            "Incomplete candidate subject for step: {}",
            job.step_id
        ))
        .into());
    }

    Ok(None)
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
    let paths = refresh_project_mirror(state, project).await?;
    let ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    } = load_item_runtime_snapshot(state, paths.mirror_git_dir.as_path(), &item).await?;

    let job = ingot_usecases::dispatch::auto_dispatch_review(
        &state.db,
        &state.db,
        &state.db,
        project,
        &item,
        &current_revision,
        &jobs,
        &findings,
        &convergences,
    )
    .await?;

    Ok(job)
}

pub(super) async fn retry_item_job(
    State(state): State<AppState>,
    ApiPath(ProjectItemJobPathParams {
        project_id,
        item_id,
        job_id,
    }): ApiPath<ProjectItemJobPathParams>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
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
    let ItemRuntimeSnapshot {
        current_revision,
        jobs,
        findings,
        convergences,
    } = load_item_runtime_snapshot(&state, paths.mirror_git_dir.as_path(), &item).await?;
    let previous_job = jobs
        .iter()
        .find(|job| job.id == job_id)
        .cloned()
        .ok_or_else(|| ApiError::NotFound {
            code: "job_not_found",
            message: "Job not found".into(),
        })?;

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
    let infra = HttpInfraAdapter::new(&state);
    persist_dispatched_job(
        &state,
        &infra,
        project_id,
        &job,
        pending_investigation_ref.as_ref(),
        precreated_authoring_workspace.as_ref(),
    )
    .await?;
    if precreated_authoring_workspace.is_none() && job.workspace_kind == WorkspaceKind::Authoring {
        let _ = ensure_authoring_workspace(&state, &project, &current_revision, &job).await?;
    }
    append_activity(
        &state,
        project_id,
        ActivityEventType::JobDispatched,
        ActivitySubject::Job(job.id),
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

async fn persist_dispatched_job(
    state: &AppState,
    infra: &HttpInfraAdapter,
    project_id: ProjectId,
    job: &Job,
    pending_investigation_ref: Option<&PendingInvestigationRef>,
    precreated_authoring_workspace: Option<&Workspace>,
) -> Result<(), ApiError> {
    if let Err(error) = state.db.create_job(job).await {
        ingot_usecases::dispatch::cleanup_failed_dispatch(
            &state.db,
            &state.db,
            infra,
            project_id,
            precreated_authoring_workspace,
            pending_investigation_ref.map(|pending| &pending.ref_name),
        )
        .await;
        return Err(repo_to_internal(error));
    }

    ingot_usecases::dispatch::apply_pending_investigation_ref_or_cleanup(
        &state.db,
        &state.db,
        &state.db,
        &state.db,
        infra,
        project_id,
        job.id,
        pending_investigation_ref,
        precreated_authoring_workspace,
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::Utc;
    use ingot_domain::git_operation::{GitOperation, GitOperationStatus, OperationPayload};
    use ingot_domain::ids::{
        GitOperationId, ItemId, ItemRevisionId, JobId, ProjectId, WorkspaceId,
    };
    use ingot_domain::job::{ExecutionPermission, Job, JobInput, OutputArtifactKind, PhaseKind};
    use ingot_domain::workspace::{WorkspaceKind, WorkspaceStatus};
    use ingot_git::commands::resolve_ref_oid;
    use ingot_git::project_repo::{ensure_mirror, project_repo_paths};
    use ingot_test_support::fixtures::{
        ItemBuilder, JobBuilder, RevisionBuilder, WorkspaceBuilder,
    };
    use ingot_test_support::git::{
        git_output as support_git_output, run_git as support_git,
        temp_git_repo as support_temp_git_repo,
    };
    use ingot_usecases::UseCaseError;
    use ingot_workflow::step;
    use uuid::Uuid;

    use crate::error::ApiError;

    use super::super::test_helpers::{test_app_state, test_project};
    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git(path: &std::path::Path, args: &[&str]) {
        support_git(path, args);
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        support_git_output(path, args)
    }

    fn test_job(
        step_id: ingot_domain::step_id::StepId,
        output_artifact_kind: OutputArtifactKind,
    ) -> Job {
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
        .job_input(JobInput::authoring_head(CommitOid::from("head")))
        .output_artifact_kind(output_artifact_kind)
        .build()
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
        let expected_oid = CommitOid::new(&head);
        assert_eq!(job.job_input.base_commit_oid(), Some(&expected_oid));
        assert_eq!(job.job_input.head_commit_oid(), Some(&expected_oid));

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
        .bind(pending_investigation_ref.ref_name.as_str())
        .fetch_one(state.db.raw_pool())
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

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7())).build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .seed_target_commit_oid(Some(head.clone()))
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item with revision");
        let partial_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .id(WorkspaceId::from_uuid(Uuid::now_v7()))
            .created_for_revision_id(revision.id)
            .path(
                state
                    .state_root
                    .join(format!("partial-workspace-{}", Uuid::now_v7()))
                    .display()
                    .to_string(),
            )
            .workspace_ref(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            ))
            .status(WorkspaceStatus::Provisioning)
            .created_at(Utc::now())
            .build();
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
        let expected_oid = CommitOid::new(&head);
        assert_eq!(job.job_input.base_commit_oid(), Some(&expected_oid));
        assert_eq!(job.job_input.head_commit_oid(), Some(&expected_oid));

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

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7())).build();
        let revision = RevisionBuilder::new(item.id)
            .id(item.current_revision_id)
            .seed_target_commit_oid(Some(head.clone()))
            .build();
        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .expect("create item with revision");
        let partial_workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .id(WorkspaceId::from_uuid(Uuid::now_v7()))
            .created_for_revision_id(revision.id)
            .path(
                state
                    .state_root
                    .join(format!("partial-review-workspace-{}", Uuid::now_v7()))
                    .display()
                    .to_string(),
            )
            .workspace_ref(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            ))
            .status(WorkspaceStatus::Provisioning)
            .created_at(Utc::now())
            .build();
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

        let item = ItemBuilder::new(project.id, ItemRevisionId::from_uuid(Uuid::now_v7())).build();
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
        let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .id(workspace_id)
            .path(workspace_path.display().to_string())
            .workspace_ref(workspace_ref.clone())
            .base_commit_oid(head.clone())
            .head_commit_oid(head.clone())
            .status(WorkspaceStatus::Ready)
            .created_at(Utc::now())
            .build();
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
                entity: GitOperationEntityRef::Job(JobId::from_uuid(Uuid::now_v7())),
                payload: OperationPayload::CreateInvestigationRef {
                    ref_name: GitRef::new(&investigation_ref),
                    new_oid: CommitOid::new(&head),
                    commit_oid: Some(CommitOid::new(&head)),
                },
                status: GitOperationStatus::Applied,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
            })
            .await
            .expect("create git operation");

        let infra = super::infra_ports::HttpInfraAdapter::new(&state);
        ingot_usecases::dispatch::cleanup_failed_dispatch(
            &state.db,
            &state.db,
            &infra,
            project.id,
            Some(&workspace),
            Some(&GitRef::new(&investigation_ref)),
        )
        .await;

        assert!(!workspace_path.exists(), "workspace path removed");
        assert_eq!(
            resolve_ref_oid(paths.mirror_git_dir.as_path(), &GitRef::new(&workspace_ref))
                .await
                .expect("resolve workspace ref"),
            None
        );
        assert_eq!(
            resolve_ref_oid(
                paths.mirror_git_dir.as_path(),
                &GitRef::new(&investigation_ref)
            )
            .await
            .expect("resolve investigation ref"),
            None
        );
        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(workspace.id.to_string())
                .fetch_one(state.db.raw_pool())
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 0);
        let op_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(&investigation_ref)
        .fetch_one(state.db.raw_pool())
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

        let workspace = WorkspaceBuilder::new(project.id, WorkspaceKind::Authoring)
            .id(WorkspaceId::from_uuid(Uuid::now_v7()))
            .path(
                state
                    .state_root
                    .join(format!("orphaned-workspace-{}", Uuid::now_v7()))
                    .display()
                    .to_string(),
            )
            .workspace_ref(format!(
                "refs/ingot/workspaces/{}",
                WorkspaceId::from_uuid(Uuid::now_v7())
            ))
            .base_commit_oid("deadbeef".repeat(5))
            .head_commit_oid("deadbeef".repeat(5))
            .status(WorkspaceStatus::Ready)
            .created_at(Utc::now())
            .build();
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
                entity: GitOperationEntityRef::Job(JobId::from_uuid(Uuid::now_v7())),
                payload: OperationPayload::CreateInvestigationRef {
                    ref_name: GitRef::new(&investigation_ref),
                    new_oid: CommitOid::new("deadbeef".repeat(5)),
                    commit_oid: Some(CommitOid::new("deadbeef".repeat(5))),
                },
                status: GitOperationStatus::Applied,
                created_at: Utc::now(),
                completed_at: Some(Utc::now()),
            })
            .await
            .expect("create git operation");

        let infra = super::infra_ports::HttpInfraAdapter::new(&state);
        ingot_usecases::dispatch::cleanup_failed_dispatch(
            &state.db,
            &state.db,
            &infra,
            project.id,
            Some(&workspace),
            Some(&GitRef::new(&investigation_ref)),
        )
        .await;

        let workspace_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspaces WHERE id = ?")
                .bind(workspace.id.to_string())
                .fetch_one(state.db.raw_pool())
                .await
                .expect("workspace count");
        assert_eq!(workspace_count, 0);
        let op_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM git_operations WHERE operation_kind = 'create_investigation_ref' AND ref_name = ?",
        )
        .bind(&investigation_ref)
        .fetch_one(state.db.raw_pool())
        .await
        .expect("operation count");
        assert_eq!(op_count, 0);
    }
}
