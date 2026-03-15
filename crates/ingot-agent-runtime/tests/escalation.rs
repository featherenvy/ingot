use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use ingot_agent_runtime::AgentRunner;
use ingot_domain::agent::Agent;
use ingot_domain::item::EscalationState;
use ingot_domain::job::{JobInput, JobStatus, OutcomeClass, OutputArtifactKind};
use ingot_domain::revision::ItemRevision;
use ingot_git::commands::head_oid;

mod common;
use common::*;

use ingot_agent_protocol::adapter::AgentError;
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::AgentResponse;
use ingot_domain::activity::ActivityEventType;
use ingot_domain::item::EscalationReason;
use ingot_workflow::{Evaluator, step};

#[tokio::test]
async fn runtime_terminal_failure_escalates_closure_relevant_item() {
    struct FailingRunner;

    impl AgentRunner for FailingRunner {
        fn launch<'a>(
            &'a self,
            _agent: &'a Agent,
            _request: &'a AgentRequest,
            _working_dir: &'a Path,
        ) -> Pin<Box<dyn Future<Output = Result<AgentResponse, AgentError>> + Send + 'a>> {
            Box::pin(async move { Err(AgentError::ProcessError("boom".into())) })
        }
    }

    let h = TestHarness::new(Arc::new(FailingRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        ..test_item(h.project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
        ..test_revision(item_id, &seed_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    let job = test_authoring_job(h.project.id, item_id, revision_id, &seed_commit);
    h.db.create_job(&job).await.expect("create job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_item = h.db.get_item(item_id).await.expect("item");
    assert_eq!(
        updated_item.escalation_state,
        EscalationState::OperatorRequired
    );
    assert_eq!(
        updated_item.escalation_reason,
        Some(EscalationReason::StepFailed)
    );
}

#[tokio::test]
async fn successful_authoring_retry_clears_escalation_and_reopens_review_dispatch() {
    let h = TestHarness::new(Arc::new(FakeRunner)).await;
    h.register_mutating_agent().await;

    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let seed_commit = head_oid(&h.repo_path).await.expect("seed head");

    let item = ingot_domain::item::Item {
        id: item_id,
        current_revision_id: revision_id,
        escalation_state: EscalationState::OperatorRequired,
        escalation_reason: Some(EscalationReason::StepFailed),
        ..test_item(h.project.id, revision_id)
    };
    let revision = ItemRevision {
        id: revision_id,
        item_id,
        template_map_snapshot: serde_json::json!({ "author_initial": "author-initial" }),
        ..test_revision(item_id, &seed_commit)
    };
    h.db.create_item_with_revision(&item, &revision)
        .await
        .expect("create item");

    // Create the failed job
    let failed_job_id = ingot_domain::ids::JobId::new();
    let created_at = chrono::Utc::now();
    let failed_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .id(failed_job_id)
        .status(JobStatus::Failed)
        .outcome_class(OutcomeClass::TerminalFailure)
        .error_code("step_failed")
        .error_message("first attempt failed")
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(&seed_commit))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .started_at(created_at)
        .ended_at(created_at)
        .build();
    h.db.create_job(&failed_job)
        .await
        .expect("create failed job");

    // Create the retry job
    let retry_job = JobBuilder::new(h.project.id, item_id, revision_id, "author_initial")
        .supersedes_job_id(failed_job_id)
        .retry_no(1)
        .phase_template_slug("author-initial")
        .job_input(JobInput::authoring_head(&seed_commit))
        .output_artifact_kind(OutputArtifactKind::Commit)
        .build();
    h.db.create_job(&retry_job)
        .await
        .expect("create retry job");

    assert!(h.dispatcher.tick().await.expect("tick should run"));

    let updated_item = h.db.get_item(item_id).await.expect("item");
    assert_eq!(updated_item.escalation_state, EscalationState::None);
    assert_eq!(updated_item.escalation_reason, None);

    let jobs = h.db.list_jobs_by_item(item_id).await.expect("jobs");
    let evaluation = Evaluator::new().evaluate(&updated_item, &revision, &jobs, &[], &[]);
    assert_eq!(evaluation.dispatchable_step_id, None);
    let review_job = jobs
        .iter()
        .find(|job| job.step_id == step::REVIEW_INCREMENTAL_INITIAL)
        .expect("auto-dispatched review job");
    assert_eq!(review_job.status, JobStatus::Queued);

    let activity =
        h.db.list_activity_by_project(h.project.id, 20, 0)
            .await
            .expect("activity");
    assert!(activity.iter().any(|entry| {
        entry.event_type == ActivityEventType::ItemEscalationCleared
            && entry.entity_id == item_id.to_string()
    }));
}
