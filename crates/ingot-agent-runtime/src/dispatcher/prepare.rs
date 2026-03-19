use super::*;
use crate::dispatcher::prompt::{
    built_in_template, load_harness_profile, resolve_harness_prompt_context, template_digest,
};

impl JobDispatcher {
    pub(super) async fn hydrate_convergences(
        &self,
        project: &Project,
        convergences: Vec<Convergence>,
    ) -> Result<Vec<Convergence>, RuntimeError> {
        if convergences.is_empty() {
            return Ok(convergences);
        }

        let paths = self.refresh_project_mirror(project).await?;
        let mut hydrated = Vec::with_capacity(convergences.len());
        for mut convergence in convergences {
            convergence.target_head_valid = self
                .compute_target_head_valid(paths.mirror_git_dir.as_path(), &convergence)
                .await?;
            hydrated.push(convergence);
        }
        Ok(hydrated)
    }

    async fn compute_target_head_valid(
        &self,
        repo_path: &Path,
        convergence: &Convergence,
    ) -> Result<Option<bool>, RuntimeError> {
        let resolved = resolve_ref_oid(repo_path, &convergence.target_ref).await?;
        Ok(convergence.target_head_valid_for_resolved_oid(resolved.as_deref()))
    }

    pub(super) async fn prepare_run(
        &self,
        queued_job: Job,
    ) -> Result<PrepareRunOutcome, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let mut job = self.db.get_job(queued_job.id).await?;
        if job.state.status() != JobStatus::Queued || !is_supported_runtime_job(&job) {
            return Ok(PrepareRunOutcome::NotPrepared);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(PrepareRunOutcome::NotPrepared);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let harness_prompt = match resolve_harness_prompt_context(Path::new(&project.path)) {
            Ok(context) => context,
            Err(error) => {
                self.fail_job_preparation(
                    &job,
                    &item,
                    &project,
                    error.error_code(),
                    error.to_string(),
                )
                .await?;
                return Ok(PrepareRunOutcome::FailedBeforeLaunch);
            }
        };
        let paths = self.refresh_project_mirror(&project).await?;
        let Some(agent) = self.select_agent(&job).await? else {
            debug!(
                job_id = %job.id,
                step_id = %job.step_id,
                "queued job is waiting for a compatible available agent"
            );
            return Ok(PrepareRunOutcome::NotPrepared);
        };
        let now = Utc::now();
        let (workspace, workspace_lifecycle, workspace_exists) = self
            .prepare_workspace(
                &project,
                paths.mirror_git_dir.as_path(),
                &paths.worktree_root,
                &revision,
                &job,
                now,
            )
            .await?;
        let mut workspace = workspace;
        let original_head_commit_oid = workspace
            .state
            .head_commit_oid()
            .map(ToOwned::to_owned)
            .ok_or_else(|| RuntimeError::InvalidState("workspace missing head".into()))?;

        workspace.attach_job(job.id, now);
        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        let template = built_in_template(&job.phase_template_slug, &job.step_id);
        let prompt = self
            .assemble_prompt(&job, &item, &revision, template, &harness_prompt)
            .await?;
        job.assign(
            JobAssignment::new(workspace.id)
                .with_agent(agent.id)
                .with_prompt_snapshot(prompt.clone())
                .with_phase_template_digest(template_digest(template)),
        );
        self.db.update_job(&job).await?;

        info!(
            job_id = %job.id,
            workspace_id = %workspace.id,
            agent_id = %agent.id,
            step_id = %job.step_id,
            project_id = %project.id,
            item_id = %item.id,
            "prepared job execution"
        );

        Ok(PrepareRunOutcome::Prepared(Box::new(PreparedRun {
            job,
            item,
            revision,
            project,
            canonical_repo_path: paths.mirror_git_dir,
            agent,
            workspace,
            original_head_commit_oid,
            prompt,
            workspace_lifecycle,
        })))
    }

    async fn select_agent(&self, job: &Job) -> Result<Option<Agent>, RuntimeError> {
        let mut agents = self
            .db
            .list_agents()
            .await?
            .into_iter()
            .filter(|agent| agent.status == AgentStatus::Available)
            .filter(|agent| agent.adapter_kind == AdapterKind::Codex)
            .filter(|agent| supports_job(agent, job))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.slug.cmp(&right.slug));
        Ok(agents.into_iter().next())
    }

    async fn prepare_workspace(
        &self,
        project: &Project,
        repo_path: &Path,
        workspace_root: &Path,
        revision: &ItemRevision,
        job: &Job,
        now: chrono::DateTime<Utc>,
    ) -> Result<(Workspace, WorkspaceLifecycle, bool), RuntimeError> {
        match (job.workspace_kind, job.execution_permission) {
            (WorkspaceKind::Authoring, _) => {
                let existing_workspace = self
                    .db
                    .find_authoring_workspace_for_revision(revision.id)
                    .await?;
                let workspace_exists = existing_workspace.is_some();
                let workspace = ensure_authoring_workspace_state(
                    existing_workspace,
                    project.id,
                    repo_path,
                    workspace_root,
                    revision,
                    job,
                    now,
                )
                .await?;
                Ok((
                    workspace,
                    WorkspaceLifecycle::PersistentAuthoring,
                    workspace_exists,
                ))
            }
            (
                WorkspaceKind::Integration,
                ExecutionPermission::MustNotMutate | ExecutionPermission::DaemonOnly,
            ) => {
                let workspace_id = self
                    .integration_workspace_id_for_job(job, revision.id)
                    .await?;
                let existing_workspace = self.db.get_workspace(workspace_id).await?;
                let workspace_exists = true;
                let expected_head_commit_oid = job
                    .job_input
                    .head_commit_oid()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        RuntimeError::InvalidState("integration jobs require job_input head".into())
                    })?;
                let workspace_ref = existing_workspace.workspace_ref.clone().ok_or_else(|| {
                    RuntimeError::InvalidState("integration workspace missing workspace_ref".into())
                })?;
                let provisioned = provision_integration_workspace(
                    repo_path,
                    Path::new(&existing_workspace.path),
                    &workspace_ref,
                    &expected_head_commit_oid,
                )
                .await?;
                let mut workspace = existing_workspace;
                workspace.path = provisioned.workspace_path.display().to_string();
                workspace.workspace_ref = Some(provisioned.workspace_ref);
                workspace.mark_ready_with_head(provisioned.head_commit_oid, now);
                Ok((
                    workspace,
                    WorkspaceLifecycle::PersistentIntegration,
                    workspace_exists,
                ))
            }
            (WorkspaceKind::Review, ExecutionPermission::MustNotMutate) => {
                let head_commit_oid = job
                    .job_input
                    .head_commit_oid()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        RuntimeError::InvalidState("review jobs require job_input head".into())
                    })?;
                let workspace_id = WorkspaceId::new();
                let workspace_path = workspace_root.join(workspace_id.to_string());
                let provisioned =
                    provision_review_workspace(repo_path, &workspace_path, &head_commit_oid)
                        .await?;
                let workspace = Workspace {
                    id: workspace_id,
                    project_id: project.id,
                    kind: WorkspaceKind::Review,
                    strategy: WorkspaceStrategy::Worktree,
                    path: provisioned.workspace_path.display().to_string(),
                    created_for_revision_id: Some(revision.id),
                    parent_workspace_id: None,
                    target_ref: None,
                    workspace_ref: None,
                    retention_policy: RetentionPolicy::Ephemeral,
                    created_at: now,
                    updated_at: now,
                    state: WorkspaceState::Ready {
                        commits: WorkspaceCommitState::new(
                            job.job_input
                                .base_commit_oid()
                                .map(ToOwned::to_owned)
                                .unwrap_or_default(),
                            provisioned.head_commit_oid,
                        ),
                    },
                };
                Ok((workspace, WorkspaceLifecycle::EphemeralReview, false))
            }
            _ => Err(RuntimeError::InvalidState(format!(
                "unsupported runtime workspace kind {:?} for step {}",
                job.workspace_kind, job.step_id
            ))),
        }
    }

    async fn integration_workspace_id_for_job(
        &self,
        job: &Job,
        revision_id: ingot_domain::ids::ItemRevisionId,
    ) -> Result<WorkspaceId, RuntimeError> {
        if let Some(workspace_id) = job.state.workspace_id() {
            return Ok(workspace_id);
        }

        if job.execution_permission != ExecutionPermission::DaemonOnly {
            return Err(RuntimeError::InvalidState(
                "integration jobs require a provisioned integration workspace".into(),
            ));
        }

        self.db
            .find_prepared_convergence_for_revision(revision_id)
            .await?
            .and_then(|convergence| convergence.state.integration_workspace_id())
            .ok_or_else(|| {
                RuntimeError::InvalidState(
                    "integration jobs require a provisioned integration workspace".into(),
                )
            })
    }

    pub(super) async fn prepare_harness_validation(
        &self,
        queued_job: Job,
    ) -> Result<PrepareHarnessValidationOutcome, RuntimeError> {
        let _guard = self
            .project_locks
            .acquire_project_mutation(queued_job.project_id)
            .await;

        let mut job = self.db.get_job(queued_job.id).await?;
        if job.state.status() != JobStatus::Queued || !is_daemon_only_validation(&job) {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let item = self.db.get_item(job.item_id).await?;
        if item.current_revision_id != job.item_revision_id {
            return Ok(PrepareHarnessValidationOutcome::NotPrepared);
        }

        let revision = self.db.get_revision(job.item_revision_id).await?;
        let project = self.db.get_project(job.project_id).await?;
        let harness = match load_harness_profile(Path::new(&project.path)) {
            Ok(harness) => harness,
            Err(error) => {
                self.fail_job_preparation(
                    &job,
                    &item,
                    &project,
                    error.error_code(),
                    error.to_string(),
                )
                .await?;
                return Ok(PrepareHarnessValidationOutcome::FailedBeforeLaunch);
            }
        };

        let paths = self.refresh_project_mirror(&project).await?;
        let now = Utc::now();
        let (workspace, workspace_exists) = match job.workspace_kind {
            WorkspaceKind::Authoring | WorkspaceKind::Integration => {
                let (workspace, _lifecycle, workspace_exists) = self
                    .prepare_workspace(
                        &project,
                        paths.mirror_git_dir.as_path(),
                        &paths.worktree_root,
                        &revision,
                        &job,
                        now,
                    )
                    .await?;
                (workspace, workspace_exists)
            }
            _ => {
                return Err(RuntimeError::InvalidState(format!(
                    "unsupported workspace kind {:?} for harness validation",
                    job.workspace_kind
                )));
            }
        };

        if workspace_exists {
            self.db.update_workspace(&workspace).await?;
        } else {
            self.db.create_workspace(&workspace).await?;
        }

        job.assign(JobAssignment::new(workspace.id));
        self.db.update_job(&job).await?;
        self.db
            .start_job_execution(StartJobExecutionParams {
                job_id: job.id,
                item_id: job.item_id,
                expected_item_revision_id: job.item_revision_id,
                workspace_id: Some(workspace.id),
                agent_id: None,
                lease_owner_id: self.lease_owner_id.clone(),
                process_pid: None,
                lease_expires_at: now + ChronoDuration::minutes(30),
            })
            .await?;

        Ok(PrepareHarnessValidationOutcome::Prepared(Box::new(
            PreparedHarnessValidation {
                harness,
                job_id: job.id,
                item_id: job.item_id,
                project_id: project.id,
                revision_id: job.item_revision_id,
                workspace_id: workspace.id,
                workspace_path: PathBuf::from(&workspace.path),
                step_id: job.step_id.clone(),
            },
        )))
    }
}

pub(super) fn is_supported_runtime_job(job: &Job) -> bool {
    matches!(
        (
            job.workspace_kind,
            job.execution_permission,
            job.output_artifact_kind,
        ),
        (
            WorkspaceKind::Authoring,
            ExecutionPermission::MayMutate,
            OutputArtifactKind::Commit
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Review | WorkspaceKind::Integration,
            ExecutionPermission::MustNotMutate,
            OutputArtifactKind::ReviewReport
                | OutputArtifactKind::ValidationReport
                | OutputArtifactKind::FindingReport,
        ) | (
            WorkspaceKind::Authoring | WorkspaceKind::Integration,
            ExecutionPermission::DaemonOnly,
            OutputArtifactKind::ValidationReport,
        )
    )
}

fn supports_job(agent: &Agent, job: &Job) -> bool {
    if job.execution_permission == ExecutionPermission::DaemonOnly
        || !agent
            .capabilities
            .contains(&AgentCapability::StructuredOutput)
    {
        return false;
    }

    match job.execution_permission {
        ExecutionPermission::MayMutate => {
            agent.capabilities.contains(&AgentCapability::MutatingJobs)
        }
        ExecutionPermission::MustNotMutate => {
            agent.capabilities.contains(&AgentCapability::ReadOnlyJobs)
        }
        ExecutionPermission::DaemonOnly => unreachable!("daemon-only jobs are filtered above"),
    }
}

pub(super) fn is_daemon_only_validation(job: &Job) -> bool {
    job.execution_permission == ExecutionPermission::DaemonOnly
        && job.phase_kind == PhaseKind::Validate
}
