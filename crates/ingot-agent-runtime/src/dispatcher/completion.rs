use super::*;

impl JobDispatcher {
    pub(super) async fn fail_run(
        &self,
        prepared: &PreparedRun,
        outcome_class: OutcomeClass,
        error_code: &'static str,
        error_message: Option<String>,
    ) -> Result<(), RuntimeError> {
        self.finalize_workspace_after_failure(prepared).await?;

        let status = match outcome_class {
            OutcomeClass::Cancelled => JobStatus::Cancelled,
            OutcomeClass::TransientFailure
            | OutcomeClass::TerminalFailure
            | OutcomeClass::ProtocolViolation => JobStatus::Failed,
            OutcomeClass::Clean | OutcomeClass::Findings => JobStatus::Failed,
        };
        let escalation_reason = failure_escalation_reason(&prepared.job, outcome_class);

        let error_message_log = error_message.as_deref().unwrap_or("").to_string();
        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: prepared.job.id,
                item_id: prepared.item.id,
                expected_item_revision_id: prepared.job.item_revision_id,
                status,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message,
                escalation_reason,
            })
            .await?;
        let event_type = if outcome_class == OutcomeClass::Cancelled {
            ActivityEventType::JobCancelled
        } else {
            ActivityEventType::JobFailed
        };
        self.append_activity(
            prepared.project.id,
            event_type,
            "job",
            prepared.job.id.to_string(),
            serde_json::json!({ "item_id": prepared.item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                prepared.project.id,
                ActivityEventType::ItemEscalated,
                "item",
                prepared.item.id.to_string(),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }

        self.refresh_revision_context(prepared).await?;
        warn!(
            job_id = %prepared.job.id,
            outcome_class = ?outcome_class,
            error_code,
            error_message = %error_message_log,
            "job failed"
        );

        Ok(())
    }

    pub(super) async fn fail_job_preparation(
        &self,
        job: &Job,
        item: &ingot_domain::item::Item,
        project: &Project,
        error_code: &'static str,
        error_message: String,
    ) -> Result<(), RuntimeError> {
        let outcome_class = OutcomeClass::TerminalFailure;
        let escalation_reason = failure_escalation_reason(job, outcome_class);

        self.db
            .finish_job_non_success(FinishJobNonSuccessParams {
                job_id: job.id,
                item_id: item.id,
                expected_item_revision_id: job.item_revision_id,
                status: JobStatus::Failed,
                outcome_class: Some(outcome_class),
                error_code: Some(error_code.into()),
                error_message: Some(error_message.clone()),
                escalation_reason,
            })
            .await?;
        self.append_activity(
            project.id,
            ActivityEventType::JobFailed,
            "job",
            job.id.to_string(),
            serde_json::json!({ "item_id": item.id, "error_code": error_code }),
        )
        .await?;
        if let Some(escalation_reason) = escalation_reason {
            self.append_activity(
                project.id,
                ActivityEventType::ItemEscalated,
                "item",
                item.id.to_string(),
                serde_json::json!({ "reason": escalation_reason }),
            )
            .await?;
        }
        self.refresh_revision_context_for_ids(
            project.id,
            item.id,
            job.item_revision_id,
            Some(job.id),
        )
        .await?;
        warn!(
            job_id = %job.id,
            error_code,
            error_message = %error_message,
            "job failed during preparation"
        );
        Ok(())
    }

    pub(super) async fn append_escalation_cleared_activity_if_needed(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        if !prepared.item.escalation.is_escalated() {
            return Ok(());
        }

        let item = self.db.get_item(prepared.item.id).await?;
        if item.current_revision_id != prepared.job.item_revision_id
            || item.escalation.is_escalated()
        {
            return Ok(());
        }

        self.append_activity(
            prepared.project.id,
            ActivityEventType::ItemEscalationCleared,
            "item",
            prepared.item.id.to_string(),
            serde_json::json!({ "reason": "successful_retry", "job_id": prepared.job.id }),
        )
        .await?;

        Ok(())
    }

    pub(super) async fn refresh_revision_context(
        &self,
        prepared: &PreparedRun,
    ) -> Result<(), RuntimeError> {
        self.refresh_revision_context_for_ids(
            prepared.project.id,
            prepared.item.id,
            prepared.revision.id,
            Some(prepared.job.id),
        )
        .await
    }

    pub(super) async fn refresh_revision_context_for_ids(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        item_id: ingot_domain::ids::ItemId,
        revision_id: ingot_domain::ids::ItemRevisionId,
        updated_from_job_id: Option<ingot_domain::ids::JobId>,
    ) -> Result<(), RuntimeError> {
        let project = self.db.get_project(project_id).await?;
        let item = self.db.get_item(item_id).await?;
        let revision = self.db.get_revision(revision_id).await?;
        let jobs = self.db.list_jobs_by_item(item.id).await?;
        let authoring_head_commit_oid = self
            .current_authoring_head_for_revision_with_workspace(&revision, &jobs)
            .await?;
        let authoring_base_commit_oid = self.effective_authoring_base_commit_oid(&revision).await?;
        let changed_paths = if let (Some(base_commit_oid), Some(head_commit_oid)) = (
            authoring_base_commit_oid.as_deref(),
            authoring_head_commit_oid.as_deref(),
        ) {
            changed_paths_between(
                self.project_paths(&project).mirror_git_dir.as_path(),
                base_commit_oid,
                head_commit_oid,
            )
            .await?
        } else {
            Vec::new()
        };
        let context = rebuild_revision_context(
            &item,
            &revision,
            &jobs,
            authoring_head_commit_oid,
            changed_paths,
            updated_from_job_id,
            Utc::now(),
        );
        self.db.upsert_revision_context(&context).await?;
        Ok(())
    }

    pub(super) async fn current_authoring_head_for_revision_with_workspace(
        &self,
        revision: &ItemRevision,
        jobs: &[Job],
    ) -> Result<Option<String>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::current_authoring_head_for_revision_with_workspace(
                revision,
                jobs,
                workspace.as_ref(),
            ),
        )
    }

    pub(super) async fn effective_authoring_base_commit_oid(
        &self,
        revision: &ItemRevision,
    ) -> Result<Option<String>, RuntimeError> {
        let workspace = self
            .db
            .find_authoring_workspace_for_revision(revision.id)
            .await?;
        Ok(
            ingot_usecases::dispatch::effective_authoring_base_commit_oid(
                revision,
                workspace.as_ref(),
            ),
        )
    }

    pub(super) fn complete_job_service(
        &self,
    ) -> CompleteJobService<Database, GitJobCompletionPort, ProjectLocks> {
        CompleteJobService::new(
            self.db.clone(),
            GitJobCompletionPort,
            self.project_locks.clone(),
        )
    }

    pub(super) async fn append_activity(
        &self,
        project_id: ingot_domain::ids::ProjectId,
        event_type: ActivityEventType,
        entity_type: &str,
        entity_id: String,
        payload: serde_json::Value,
    ) -> Result<(), RuntimeError> {
        self.db
            .append_activity(&Activity {
                id: ingot_domain::ids::ActivityId::new(),
                project_id,
                event_type,
                entity_type: entity_type.into(),
                entity_id,
                payload,
                created_at: Utc::now(),
            })
            .await?;
        Ok(())
    }
}

pub(super) fn failure_escalation_reason(
    job: &Job,
    outcome_class: OutcomeClass,
) -> Option<EscalationReason> {
    ingot_usecases::dispatch::failure_escalation_reason(job, outcome_class)
}

pub(super) fn should_clear_item_escalation_on_success(
    item: &ingot_domain::item::Item,
    job: &Job,
) -> bool {
    ingot_usecases::dispatch::should_clear_item_escalation_on_success(item, job)
}

pub(super) fn is_closure_relevant_job(job: &Job) -> bool {
    ingot_usecases::dispatch::is_closure_relevant_job(job)
}
