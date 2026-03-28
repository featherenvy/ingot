// Job preparation pipeline: run configuration, workspace provisioning, prompt assembly.

use std::path::Path;

use chrono::Utc;
use ingot_domain::agent::{Agent, AgentStatus};
use ingot_domain::finding::FindingTriageState;
use ingot_domain::ids::WorkspaceId;
use ingot_domain::job::{
    ExecutionPermission, Job, JobAssignment, JobInput, JobStatus, OutcomeClass, OutputArtifactKind,
};
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::AgentRouting;
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::{
    RetentionPolicy, Workspace, WorkspaceCommitState, WorkspaceKind, WorkspaceState,
    WorkspaceStrategy,
};
use ingot_git::commands::resolve_ref_oid;
use ingot_workflow::step;
use ingot_workspace::{
    ensure_authoring_workspace_state, provision_integration_workspace, provision_review_workspace,
};
use tracing::{debug, info};

use crate::{
    HarnessPromptContext, JobDispatcher, PrepareRunOutcome, PreparedRun, RuntimeError,
    WorkspaceLifecycle, built_in_template, format_revision_context, is_closure_relevant_job,
    is_supported_runtime_job, report, resolve_harness_prompt_context, supports_job,
    template_digest,
};

impl JobDispatcher {
    pub(crate) async fn prepare_run(
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
        let harness_prompt = match resolve_harness_prompt_context(&project.path) {
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
        job = self
            .rebind_implicit_author_initial_job_if_needed(
                job,
                &revision,
                paths.mirror_git_dir.as_path(),
            )
            .await?;
        let Some(agent) = self
            .select_agent(&job, project.agent_routing.as_ref())
            .await?
        else {
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

        let template = built_in_template(&job.phase_template_slug, job.step_id);
        let phase_template_digest = template_digest(template);
        let prompt = match self
            .assemble_prompt(&job, &item, &revision, template, &harness_prompt)
            .await
        {
            Ok(prompt) => prompt,
            Err(error) => {
                self.cleanup_unclaimed_prepared_workspace(
                    job.project_id,
                    job.id,
                    &workspace,
                    workspace_lifecycle,
                    &original_head_commit_oid,
                    paths.mirror_git_dir.as_path(),
                )
                .await?;
                return Err(error);
            }
        };
        let assignment = JobAssignment::new(workspace.id)
            .with_agent(agent.id)
            .with_prompt_snapshot(prompt.clone())
            .with_phase_template_digest(phase_template_digest);

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
            assignment,
            workspace,
            original_head_commit_oid,
            prompt,
            workspace_lifecycle,
        })))
    }

    async fn rebind_implicit_author_initial_job_if_needed(
        &self,
        mut job: Job,
        revision: &ItemRevision,
        repo_path: &Path,
    ) -> Result<Job, RuntimeError> {
        if job.step_id != step::AUTHOR_INITIAL
            || job.workspace_kind != WorkspaceKind::Authoring
            || job.execution_permission != ExecutionPermission::MayMutate
            || revision.seed.is_explicit()
        {
            return Ok(job);
        }

        if self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?
            .is_some()
        {
            return Ok(job);
        }

        let resolved_head = resolve_ref_oid(repo_path, &revision.target_ref)
            .await?
            .ok_or_else(|| RuntimeError::InvalidState("target ref unresolved".into()))?;

        if job.job_input.head_commit_oid() == Some(&resolved_head) {
            return Ok(job);
        }

        job.job_input = JobInput::authoring_head(resolved_head.clone());
        self.db.update_job(&job).await?;

        info!(
            job_id = %job.id,
            revision_id = %revision.id,
            rebound_head_commit_oid = %resolved_head,
            "rebound implicit author_initial head from current target ref"
        );

        Ok(job)
    }

    async fn select_agent(
        &self,
        job: &Job,
        routing: Option<&AgentRouting>,
    ) -> Result<Option<Agent>, RuntimeError> {
        let mut agents = self
            .db
            .list_agents()
            .await?
            .into_iter()
            .filter(|agent| agent.status == AgentStatus::Available)
            .filter(|agent| supports_job(agent, job))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.slug.cmp(&right.slug));

        let preferred_slug = routing.and_then(|r| r.preferred_slug(job.phase_kind));
        if let Some(slug) = preferred_slug {
            if let Some(pos) = agents.iter().position(|a| a.slug == slug) {
                return Ok(Some(agents.swap_remove(pos)));
            }
            debug!(
                job_id = %job.id,
                preferred_slug = slug,
                "preferred agent not available, falling back"
            );
        }

        Ok(agents.into_iter().next())
    }

    pub(crate) async fn prepare_workspace(
        &self,
        project: &ingot_domain::project::Project,
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
                workspace.path = provisioned.workspace_path.clone();
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
                    path: provisioned.workspace_path.clone(),
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
                                .cloned()
                                .unwrap_or_else(|| provisioned.head_commit_oid.clone()),
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

    async fn assemble_prompt(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        revision: &ItemRevision,
        template: &str,
        harness_prompt: &HarnessPromptContext,
    ) -> Result<String, RuntimeError> {
        let revision_context = self.db.get_revision_context(revision.id).await?;
        let context_block = format_revision_context(revision_context.as_ref());
        let workspace_kind = match job.workspace_kind {
            WorkspaceKind::Authoring => "authoring",
            WorkspaceKind::Review => "review",
            WorkspaceKind::Integration => "integration",
        };
        let execution = match job.execution_permission {
            ExecutionPermission::MayMutate => "may_mutate",
            ExecutionPermission::MustNotMutate => "must_not_mutate",
            ExecutionPermission::DaemonOnly => "daemon_only",
        };

        let mut prompt = format!(
            "Revision contract:\n- Item ID: {}\n- Revision: {}\n- Title: {}\n- Description: {}\n- Acceptance criteria: {}\n- Target ref: {}\n- Approval policy: {:?}\n\nWorkflow step:\n- Step: {}\n- Template: {}\n- Workspace: {}\n- Execution: {}\n",
            item.id,
            revision.revision_no,
            revision.title,
            revision.description,
            revision.acceptance_criteria,
            revision.target_ref,
            revision.approval_policy,
            job.step_id,
            job.phase_template_slug,
            workspace_kind,
            execution,
        );

        if let Some(base) = job.job_input.base_commit_oid() {
            prompt.push_str(&format!("- Input base commit: {base}\n"));
        }
        if let Some(head) = job.job_input.head_commit_oid() {
            prompt.push_str(&format!("- Input head commit: {head}\n"));
        }

        prompt.push_str(&format!(
            "\nTemplate prompt:\n{}\n\nRevision context:\n{}\n\n",
            template, context_block
        ));

        if matches!(
            job.step_id,
            StepId::RepairCandidate | StepId::RepairAfterIntegration
        ) {
            let jobs = self.db.list_jobs_by_item(item.id).await?;
            let findings = self.db.list_findings_by_item(item.id).await?;
            let latest_closure_findings_job = jobs
                .iter()
                .filter(|candidate| candidate.item_revision_id == revision.id)
                .filter(|candidate| candidate.state.status().is_terminal())
                .filter(|candidate| candidate.state.outcome_class() == Some(OutcomeClass::Findings))
                .filter(|candidate| is_closure_relevant_job(candidate))
                .max_by_key(|candidate| (candidate.state.ended_at(), candidate.created_at));

            if let Some(latest_job) = latest_closure_findings_job {
                let scoped_findings = findings
                    .iter()
                    .filter(|finding| finding.source_item_revision_id == revision.id)
                    .filter(|finding| finding.source_job_id == latest_job.id)
                    .collect::<Vec<_>>();
                let fix_now_findings = scoped_findings
                    .iter()
                    .filter(|finding| finding.triage.state() == FindingTriageState::FixNow)
                    .collect::<Vec<_>>();
                let accepted_findings = scoped_findings
                    .iter()
                    .filter(|finding| !finding.triage.blocks_closure())
                    .collect::<Vec<_>>();

                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push_str("Finding triage for this repair:\n");
                }
                if !fix_now_findings.is_empty() {
                    prompt.push_str("- Fix now findings:\n");
                    for finding in &fix_now_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} ({:?})\n",
                            finding.code, finding.summary, finding.severity
                        ));
                    }
                }
                if !accepted_findings.is_empty() {
                    prompt.push_str("- Already triaged as non-blocking for this attempt:\n");
                    for finding in &accepted_findings {
                        prompt.push_str(&format!(
                            "  - [{}] {} => {:?}\n",
                            finding.code,
                            finding.summary,
                            finding.triage.state()
                        ));
                    }
                }
                if !fix_now_findings.is_empty() || !accepted_findings.is_empty() {
                    prompt.push('\n');
                }
            }
        }

        match job.output_artifact_kind {
            OutputArtifactKind::Commit => {
                prompt.push_str(
                    "Protocol:\n- Edit files inside the current repository to satisfy the revision contract.\n- You may run local validation commands when useful.\n- Do not create commits, amend commits, rebase, merge, cherry-pick, or move refs.\n- Leave all changes unstaged or staged in the working tree; Ingot will create the canonical commit.\n- Return a structured object with keys `summary` and `validation`; set `validation` to null when no validation was run.\n",
                );
            }
            OutputArtifactKind::ReviewReport
            | OutputArtifactKind::ValidationReport
            | OutputArtifactKind::FindingReport => {
                prompt.push_str(
                    "Protocol:\n- Do not modify files, create commits, rebase, merge, cherry-pick, or move refs.\n- Inspect the current workspace subject and produce only the canonical structured report for this step.\n- Any non-core data must go under `extensions`.\n",
                );
                prompt.push_str(report::prompt_suffix(job.output_artifact_kind));
            }
            OutputArtifactKind::None => {
                prompt.push_str("Protocol:\n- No output artifact is expected for this step.\n");
            }
        }

        if !harness_prompt.commands.is_empty() {
            prompt.push_str("\nAvailable verification commands:\n");
            for cmd in &harness_prompt.commands {
                prompt.push_str(&format!("- `{}`: `{}`\n", cmd.name, cmd.run));
            }
        }
        if !harness_prompt.skills.is_empty() {
            prompt.push_str("\nRepo-local skills available:\n");
            for skill in &harness_prompt.skills {
                prompt.push_str(&format!(
                    "\nSkill file: {}\n{}\n",
                    skill.relative_path, skill.contents
                ));
            }
        }

        Ok(prompt)
    }
}
