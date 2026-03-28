use std::future::{Future, ready};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use ingot_domain::activity::Activity;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::convergence_queue::{ConvergenceQueueEntry, ConvergenceQueueEntryStatus};
use ingot_domain::git_operation::GitOperation;
use ingot_domain::ids::{ConvergenceId, ItemId, ItemRevisionId, ProjectId};
use ingot_domain::item::ApprovalState;
use ingot_domain::job::Job;
use ingot_domain::ports::ConvergenceQueuePrepareContext;
use ingot_domain::project::Project;
use ingot_domain::revision::ItemRevision;
use ingot_test_support::fixtures::{
    ConvergenceBuilder, ItemBuilder, JobBuilder, ProjectBuilder, RevisionBuilder,
};
use ingot_test_support::git::unique_temp_path;
use uuid::Uuid;

use crate::UseCaseError;

use super::types::{
    ApprovalFinalizeReadiness, CheckoutFinalizationReadiness, ConvergenceApprovalContext,
    ConvergenceCommandPort, ConvergenceSystemActionPort, FinalizationTarget,
    FinalizePreparedTrigger, FinalizeTargetRefResult, PreparedConvergenceFinalizePort,
    RejectApprovalContext, RejectApprovalTeardown, SystemActionItemState, SystemActionProjectState,
};

#[derive(Clone)]
pub(super) struct FakePort {
    pub(super) queue_prepare_context: Arc<Mutex<Option<ConvergenceQueuePrepareContext>>>,
    pub(super) approval_context: Arc<Mutex<Option<ConvergenceApprovalContext>>>,
    pub(super) projects: Arc<Mutex<Vec<SystemActionProjectState>>>,
    pub(super) calls: Arc<Mutex<Vec<String>>>,
    pub(super) auto_finalize_progress: bool,
    pub(super) checkout_finalization_readiness: CheckoutFinalizationReadiness,
    pub(super) finalize_target_ref_result: FinalizeTargetRefResult,
    pub(super) apply_successful_finalization_should_fail: bool,
}

impl FakePort {
    pub(super) fn default_approval_context() -> ConvergenceApprovalContext {
        let nil = Uuid::nil();
        ConvergenceApprovalContext {
            project: ProjectBuilder::new(unique_temp_path("ingot-convergence-approve"))
                .id(ProjectId::from_uuid(nil))
                .build(),
            item: ItemBuilder::new(ProjectId::from_uuid(nil), ItemRevisionId::from_uuid(nil))
                .id(ItemId::from_uuid(nil))
                .approval_state(ApprovalState::Pending)
                .build(),
            revision: RevisionBuilder::new(ItemId::from_uuid(nil))
                .id(ItemRevisionId::from_uuid(nil))
                .explicit_seed("abc123")
                .build(),
            has_active_job: false,
            has_active_convergence: false,
            finalize_readiness: ApprovalFinalizeReadiness::Ready {
                convergence: Box::new(
                    ConvergenceBuilder::new(
                        ProjectId::from_uuid(nil),
                        ItemId::from_uuid(nil),
                        ItemRevisionId::from_uuid(nil),
                    )
                    .id(ConvergenceId::from_uuid(Uuid::nil()))
                    .status(ConvergenceStatus::Prepared)
                    .target_head_valid(true)
                    .created_at(Utc::now())
                    .build(),
                ),
                queue_entry: ConvergenceQueueEntry {
                    id: ingot_domain::ids::ConvergenceQueueEntryId::from_uuid(Uuid::nil()),
                    project_id: ProjectId::from_uuid(Uuid::nil()),
                    item_id: ItemId::from_uuid(Uuid::nil()),
                    item_revision_id: ItemRevisionId::from_uuid(Uuid::nil()),
                    target_ref: "refs/heads/main".into(),
                    status: ConvergenceQueueEntryStatus::Head,
                    head_acquired_at: Some(Utc::now()),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    released_at: None,
                },
            },
        }
    }

    pub(super) fn with_projects(projects: Vec<SystemActionProjectState>) -> Self {
        Self {
            queue_prepare_context: Arc::new(Mutex::new(None)),
            approval_context: Arc::new(Mutex::new(Some(Self::default_approval_context()))),
            projects: Arc::new(Mutex::new(projects)),
            calls: Arc::new(Mutex::new(Vec::new())),
            auto_finalize_progress: true,
            checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
            finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
            apply_successful_finalization_should_fail: false,
        }
    }

    pub(super) fn with_queue_prepare_context(context: ConvergenceQueuePrepareContext) -> Self {
        Self {
            queue_prepare_context: Arc::new(Mutex::new(Some(context))),
            approval_context: Arc::new(Mutex::new(Some(Self::default_approval_context()))),
            projects: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
            auto_finalize_progress: true,
            checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
            finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
            apply_successful_finalization_should_fail: false,
        }
    }

    pub(super) fn with_approval_context(context: ConvergenceApprovalContext) -> Self {
        Self {
            queue_prepare_context: Arc::new(Mutex::new(None)),
            approval_context: Arc::new(Mutex::new(Some(context))),
            projects: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(Mutex::new(Vec::new())),
            auto_finalize_progress: true,
            checkout_finalization_readiness: CheckoutFinalizationReadiness::Synced,
            finalize_target_ref_result: FinalizeTargetRefResult::UpdatedNow,
            apply_successful_finalization_should_fail: false,
        }
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl ConvergenceCommandPort for FakePort {
    fn load_queue_prepare_context(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceQueuePrepareContext, UseCaseError>> + Send {
        ready(
            self.queue_prepare_context
                .lock()
                .expect("queue prepare lock")
                .clone()
                .ok_or(UseCaseError::Internal(
                    "missing queue prepare context".into(),
                )),
        )
    }

    fn create_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("create_queue:{}", queue_entry.id));
        ready(Ok(()))
    }

    fn update_queue_entry(
        &self,
        queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("update_queue:{}", queue_entry.id));
        ready(Ok(()))
    }

    fn append_activity(
        &self,
        activity: &Activity,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("activity:{:?}", activity.event_type));
        ready(Ok(()))
    }

    fn load_approval_context(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> impl Future<Output = Result<ConvergenceApprovalContext, UseCaseError>> + Send {
        ready(
            self.approval_context
                .lock()
                .expect("approval context lock")
                .clone()
                .ok_or(UseCaseError::Internal("missing approval context".into())),
        )
    }

    fn update_item(
        &self,
        item: &ingot_domain::item::Item,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("update_item:{}", item.id));
        ready(Ok(()))
    }

    fn load_reject_approval_context(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalContext, UseCaseError>> + Send {
        let nil = Uuid::nil();
        ready(Ok(RejectApprovalContext {
            item: ItemBuilder::new(ProjectId::from_uuid(nil), ItemRevisionId::from_uuid(nil))
                .id(ItemId::from_uuid(nil))
                .approval_state(ApprovalState::Pending)
                .build(),
            has_active_job: false,
            has_active_convergence: false,
        }))
    }

    fn teardown_reject_approval(
        &self,
        _project_id: ProjectId,
        _item_id: ItemId,
    ) -> impl Future<Output = Result<RejectApprovalTeardown, UseCaseError>> + Send {
        ready(Ok(RejectApprovalTeardown {
            has_cancelled_convergence: true,
            has_cancelled_queue_entry: true,
            first_cancelled_convergence_id: None,
            first_cancelled_queue_entry_id: None,
        }))
    }

    fn apply_rejected_approval(
        &self,
        item: &ingot_domain::item::Item,
        next_revision: &ItemRevision,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("reject:{}:{}", item.id, next_revision.id));
        ready(Ok(()))
    }
}

impl ConvergenceSystemActionPort for FakePort {
    fn load_system_action_projects(
        &self,
    ) -> impl Future<Output = Result<Vec<SystemActionProjectState>, UseCaseError>> + Send {
        ready(Ok(self.projects.lock().expect("projects lock").clone()))
    }

    fn promote_queue_heads(
        &self,
        project_id: ProjectId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("promote:{project_id}"));
        ready(Ok(()))
    }

    fn prepare_queue_head_convergence(
        &self,
        project: &Project,
        state: &SystemActionItemState,
        _queue_entry: &ConvergenceQueueEntry,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("prepare:{}:{}", project.id, state.item_id));
        ready(Ok(()))
    }

    fn invalidate_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("invalidate:{project_id}:{item_id}"));
        ready(Ok(()))
    }

    fn auto_finalize_prepared_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("finalize:{project_id}:{item_id}"));
        ready(Ok(self.auto_finalize_progress))
    }

    fn auto_queue_convergence(
        &self,
        project_id: ProjectId,
        item_id: ItemId,
    ) -> impl Future<Output = Result<bool, UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("auto_queue:{project_id}:{item_id}"));
        ready(Ok(true))
    }
}

impl PreparedConvergenceFinalizePort for FakePort {
    fn find_or_create_finalize_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<GitOperation, UseCaseError>> + Send {
        self.calls.lock().expect("calls lock").push(format!(
            "find_or_create_op:{}",
            operation.entity.entity_id_string()
        ));
        ready(Ok(operation.clone()))
    }

    fn finalize_target_ref(
        &self,
        _project: &Project,
        convergence: &Convergence,
    ) -> impl Future<Output = Result<FinalizeTargetRefResult, UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("finalize_target_ref:{}", convergence.id));
        ready(Ok(self.finalize_target_ref_result))
    }

    fn checkout_finalization_readiness(
        &self,
        _project: &Project,
        item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        _prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<CheckoutFinalizationReadiness, UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("checkout_readiness:{}", item.id));
        ready(Ok(self.checkout_finalization_readiness.clone()))
    }

    fn sync_checkout_to_prepared_commit(
        &self,
        _project: &Project,
        revision: &ItemRevision,
        _prepared_commit_oid: &CommitOid,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("sync_checkout:{}", revision.id));
        ready(Ok(()))
    }

    fn update_git_operation(
        &self,
        operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls
            .lock()
            .expect("calls lock")
            .push(format!("update_op:{:?}", operation.status));
        ready(Ok(()))
    }

    fn apply_successful_finalization(
        &self,
        trigger: FinalizePreparedTrigger,
        _project: &Project,
        item: &ingot_domain::item::Item,
        _revision: &ItemRevision,
        target: FinalizationTarget<'_>,
        _operation: &GitOperation,
    ) -> impl Future<Output = Result<(), UseCaseError>> + Send {
        self.calls.lock().expect("calls lock").push(format!(
            "apply_successful_finalization:{trigger:?}:{}:{}",
            item.id, target.convergence.id
        ));
        ready(if self.apply_successful_finalization_should_fail {
            Err(UseCaseError::Internal("boom".into()))
        } else {
            Ok(())
        })
    }
}

pub(super) fn project_state(next_action: &str) -> SystemActionProjectState {
    let created_at = Utc::now();
    let project_id = ProjectId::from_uuid(Uuid::nil());
    let item_id = ItemId::from_uuid(Uuid::nil());
    let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
    let project = ProjectBuilder::new(unique_temp_path("ingot-convergence"))
        .id(project_id)
        .build();
    let revision = RevisionBuilder::new(item_id)
        .id(revision_id)
        .approval_policy(if next_action == "finalize_prepared_convergence" {
            ingot_domain::revision::ApprovalPolicy::NotRequired
        } else {
            ingot_domain::revision::ApprovalPolicy::Required
        })
        .explicit_seed("seed")
        .created_at(created_at)
        .build();
    let approval_state = if next_action == "finalize_prepared_convergence" {
        ApprovalState::NotRequired
    } else {
        ApprovalState::NotRequested
    };
    let item = ItemBuilder::new(project_id, revision_id)
        .id(item_id)
        .approval_state(approval_state)
        .created_at(created_at)
        .build();
    let convergence = ConvergenceBuilder::new(project_id, item_id, revision_id)
        .id(ConvergenceId::from_uuid(Uuid::nil()))
        .status(if next_action == "prepare_convergence" {
            ConvergenceStatus::Failed
        } else {
            ConvergenceStatus::Prepared
        })
        .target_head_valid(next_action != "invalidate_prepared_convergence")
        .created_at(created_at)
        .build();
    let queue_entry = ConvergenceQueueEntry {
        id: ingot_domain::ids::ConvergenceQueueEntryId::from_uuid(Uuid::nil()),
        project_id,
        item_id,
        item_revision_id: revision_id,
        target_ref: "refs/heads/main".into(),
        status: ConvergenceQueueEntryStatus::Head,
        head_acquired_at: Some(created_at),
        created_at,
        updated_at: created_at,
        released_at: None,
    };

    SystemActionProjectState {
        project,
        items: vec![SystemActionItemState {
            item_id,
            item,
            revision,
            jobs: vec![fake_completed_validate_job(next_action)],
            findings: vec![],
            convergences: vec![convergence],
            queue_entry: Some(queue_entry),
        }],
    }
}

pub(super) fn fake_completed_validate_job(next_action: &str) -> Job {
    let created_at = Utc::now();
    let nil = Uuid::nil();
    let step_id = if next_action == "prepare_convergence" {
        "validate_candidate_initial"
    } else {
        "validate_integrated"
    };
    JobBuilder::new(
        ProjectId::from_uuid(nil),
        ItemId::from_uuid(nil),
        ItemRevisionId::from_uuid(nil),
        step_id,
    )
    .status(ingot_domain::job::JobStatus::Completed)
    .outcome_class(ingot_domain::job::OutcomeClass::Clean)
    .phase_kind(ingot_domain::job::PhaseKind::Validate)
    .workspace_kind(ingot_domain::workspace::WorkspaceKind::Integration)
    .execution_permission(ingot_domain::job::ExecutionPermission::MustNotMutate)
    .context_policy(ingot_domain::job::ContextPolicy::ResumeContext)
    .job_input(ingot_domain::job::JobInput::None)
    .output_artifact_kind(ingot_domain::job::OutputArtifactKind::ValidationReport)
    .created_at(created_at)
    .started_at(created_at)
    .ended_at(created_at)
    .build()
}
