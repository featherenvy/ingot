use ingot_domain::commit_oid::CommitOid;
use ingot_domain::convergence::{Convergence, ConvergenceStatus};
use ingot_domain::job::{Job, JobInput, JobStatus, OutputArtifactKind};
use ingot_domain::revision::ItemRevision;
use ingot_domain::step_id::StepId;
use ingot_domain::workspace::Workspace;

use crate::UseCaseError;

pub(crate) fn current_authoring_head_for_revision(
    jobs: &[Job],
    revision: &ItemRevision,
) -> Option<CommitOid> {
    successful_commit_oids(jobs, revision)
        .last()
        .cloned()
        .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
}

pub(crate) fn previous_authoring_head_for_revision(
    jobs: &[Job],
    revision: &ItemRevision,
) -> Option<CommitOid> {
    let commit_oids = successful_commit_oids(jobs, revision);
    commit_oids
        .iter()
        .rev()
        .nth(1)
        .cloned()
        .or_else(|| revision.seed.seed_commit_oid().map(ToOwned::to_owned))
}

pub(crate) fn current_authoring_head_for_revision_with_workspace(
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
) -> Option<CommitOid> {
    if let Some(commit_oid) = current_authoring_head_for_revision(jobs, revision) {
        return Some(commit_oid);
    }

    workspace.and_then(|ws| ws.state.head_commit_oid().map(ToOwned::to_owned))
}

pub(crate) fn effective_authoring_base_commit_oid(
    revision: &ItemRevision,
    workspace: Option<&Workspace>,
) -> Option<CommitOid> {
    if let Some(seed_commit_oid) = revision.seed.seed_commit_oid() {
        return Some(seed_commit_oid.to_owned());
    }

    workspace.and_then(|ws| ws.state.base_commit_oid().map(ToOwned::to_owned))
}

pub(crate) fn subject_input_from_range(
    base_commit_oid: Option<CommitOid>,
    head_commit_oid: Option<CommitOid>,
    integrated: bool,
) -> JobInput {
    match (base_commit_oid, head_commit_oid) {
        (Some(base_commit_oid), Some(head_commit_oid)) => {
            if integrated {
                JobInput::integrated_subject(base_commit_oid, head_commit_oid)
            } else {
                JobInput::candidate_subject(base_commit_oid, head_commit_oid)
            }
        }
        _ => JobInput::None,
    }
}

pub(crate) fn job_input_from_prepared_convergence(
    convergence: &Convergence,
    integrated: bool,
) -> JobInput {
    subject_input_from_range(
        convergence
            .state
            .input_target_commit_oid()
            .map(ToOwned::to_owned),
        convergence
            .state
            .prepared_commit_oid()
            .map(ToOwned::to_owned),
        integrated,
    )
}

pub(crate) fn build_candidate_subject_input(
    step_id: StepId,
    input: &JobInput,
    revision: &ItemRevision,
    jobs: &[Job],
    workspace: Option<&Workspace>,
    context: &str,
) -> Result<JobInput, UseCaseError> {
    let base = input
        .base_commit_oid()
        .map(ToOwned::to_owned)
        .or_else(|| effective_authoring_base_commit_oid(revision, workspace));
    let head = input
        .head_commit_oid()
        .map(ToOwned::to_owned)
        .or_else(|| current_authoring_head_for_revision_with_workspace(revision, jobs, workspace));

    match (base, head) {
        (Some(base), Some(head)) => Ok(JobInput::candidate_subject(base, head)),
        _ => Err(UseCaseError::Internal(format!(
            "incomplete candidate subject for {context} {step_id}"
        ))),
    }
}

pub(crate) fn selected_prepared_convergence(
    revision_id: ingot_domain::ids::ItemRevisionId,
    convergences: &[Convergence],
) -> Option<&Convergence> {
    convergences.iter().find(|convergence| {
        convergence.item_revision_id == revision_id
            && convergence.state.status() == ConvergenceStatus::Prepared
    })
}

fn successful_commit_oids(jobs: &[Job], revision: &ItemRevision) -> Vec<CommitOid> {
    let mut commit_jobs = jobs
        .iter()
        .filter(|job| job.item_revision_id == revision.id)
        .filter(|job| job.state.status() == JobStatus::Completed)
        .filter(|job| job.output_artifact_kind == OutputArtifactKind::Commit)
        .filter_map(|job| {
            job.state.output_commit_oid().map(|commit_oid| {
                (
                    (job.state.ended_at(), job.created_at),
                    commit_oid.to_owned(),
                )
            })
        })
        .collect::<Vec<_>>();

    commit_jobs.sort_by_key(|(sort_key, _)| *sort_key);
    commit_jobs
        .into_iter()
        .map(|(_, commit_oid)| commit_oid)
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::ids::{ItemId, ItemRevisionId, ProjectId};
    use ingot_domain::job::{JobStatus, OutcomeClass};
    use ingot_domain::step_id::StepId;
    use ingot_domain::test_support::{JobBuilder, RevisionBuilder, WorkspaceBuilder};
    use ingot_domain::workspace::WorkspaceKind;
    use uuid::Uuid;

    use super::{
        current_authoring_head_for_revision, current_authoring_head_for_revision_with_workspace,
        effective_authoring_base_commit_oid, previous_authoring_head_for_revision,
    };

    fn completed_commit_job(
        project_id: ProjectId,
        item_id: ItemId,
        revision_id: ItemRevisionId,
        commit_oid: &str,
        created_at: chrono::DateTime<chrono::Utc>,
    ) -> ingot_domain::job::Job {
        JobBuilder::new(project_id, item_id, revision_id, StepId::AuthorInitial)
            .status(JobStatus::Completed)
            .outcome_class(OutcomeClass::Clean)
            .output_artifact_kind(ingot_domain::job::OutputArtifactKind::Commit)
            .output_commit_oid(commit_oid)
            .created_at(created_at)
            .started_at(created_at)
            .ended_at(created_at)
            .build()
    }

    #[test]
    fn previous_authoring_head_uses_second_latest_commit() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let project_id = ProjectId::from_uuid(Uuid::nil());
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .explicit_seed("seed")
            .created_at(Utc::now())
            .build();
        let first = completed_commit_job(project_id, item_id, revision_id, "commit-1", Utc::now());
        let second = completed_commit_job(project_id, item_id, revision_id, "commit-2", Utc::now());

        assert_eq!(
            previous_authoring_head_for_revision(&[first, second], &revision),
            Some("commit-1".into())
        );
    }

    #[test]
    fn workspace_head_is_used_when_no_completed_commit_exists() {
        let item_id = ItemId::from_uuid(Uuid::nil());
        let revision_id = ItemRevisionId::from_uuid(Uuid::nil());
        let revision = RevisionBuilder::new(item_id)
            .id(revision_id)
            .seed_commit_oid(None::<String>)
            .seed_target_commit_oid(Some("target".to_string()))
            .build();
        let workspace =
            WorkspaceBuilder::new(ProjectId::from_uuid(Uuid::nil()), WorkspaceKind::Authoring)
                .created_for_revision_id(revision_id)
                .head_commit_oid("workspace-head")
                .base_commit_oid("workspace-base")
                .build();

        assert_eq!(current_authoring_head_for_revision(&[], &revision), None);
        assert_eq!(
            current_authoring_head_for_revision_with_workspace(&revision, &[], Some(&workspace)),
            Some("workspace-head".into())
        );
        assert_eq!(
            effective_authoring_base_commit_oid(&revision, Some(&workspace)),
            Some("workspace-base".into())
        );
    }
}
