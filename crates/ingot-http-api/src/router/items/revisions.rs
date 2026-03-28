use std::path::Path as FsPath;

use chrono::Utc;
use ingot_domain::commit_oid::CommitOid;
use ingot_domain::git_ref::GitRef;
use ingot_domain::item::Item;
use ingot_domain::job::Job;
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed, ItemRevision};
use ingot_git::commands::{is_commit_reachable_from_any_ref, resolve_ref_oid};
use ingot_usecases::UseCaseError;
use ingot_usecases::item::{
    default_policy_snapshot, default_template_map_snapshot, normalize_target_ref,
    rework_budgets_from_policy_snapshot,
};

use crate::error::ApiError;
use crate::router::AppState;
use crate::router::support::{
    ensure_git_valid_target_ref, git_to_internal, refresh_project_mirror,
};
use crate::router::types::ReviseItemRequest;

use super::current_authoring_head_for_revision_with_workspace;

pub(in crate::router) async fn build_superseding_revision(
    state: &AppState,
    project: &Project,
    item: &Item,
    current_revision: &ItemRevision,
    jobs: &[Job],
    request: ReviseItemRequest,
) -> Result<ItemRevision, ApiError> {
    let target_ref = normalize_target_ref(
        request
            .target_ref
            .as_ref()
            .map(GitRef::as_str)
            .unwrap_or(current_revision.target_ref.as_str()),
    )?;
    ensure_git_valid_target_ref(target_ref.as_str()).await?;
    let paths = refresh_project_mirror(state, project).await?;
    let repo_path = paths.mirror_git_dir.as_path();
    let derived_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.to_string()))?;

    let requested_seed_commit_oid =
        validate_seed_commit_oid(repo_path, request.seed_commit_oid).await?;
    let seed_commit_oid = match requested_seed_commit_oid {
        Some(seed_commit_oid) => Some(seed_commit_oid),
        None => current_authoring_head_for_revision_with_workspace(state, current_revision, jobs)
            .await?
            .or_else(|| current_revision.seed.seed_commit_oid().cloned()),
    };
    let seed_target_commit_oid = resolve_seed_target_commit_oid(
        repo_path,
        request.seed_target_commit_oid,
        derived_target_head,
    )
    .await?;
    let seed = AuthoringBaseSeed::from_parts(seed_commit_oid, seed_target_commit_oid);
    let approval_policy = request
        .approval_policy
        .unwrap_or(current_revision.approval_policy);
    let policy_snapshot = build_superseding_policy_snapshot(current_revision, approval_policy);

    Ok(ItemRevision {
        id: ingot_domain::ids::ItemRevisionId::new(),
        item_id: item.id,
        revision_no: current_revision.revision_no + 1,
        title: request.title.unwrap_or(current_revision.title.clone()),
        description: request
            .description
            .unwrap_or(current_revision.description.clone()),
        acceptance_criteria: request
            .acceptance_criteria
            .unwrap_or(current_revision.acceptance_criteria.clone()),
        target_ref,
        approval_policy,
        policy_snapshot,
        template_map_snapshot: default_template_map_snapshot(),
        seed,
        supersedes_revision_id: Some(current_revision.id),
        created_at: Utc::now(),
    })
}

fn build_superseding_policy_snapshot(
    current_revision: &ItemRevision,
    approval_policy: ApprovalPolicy,
) -> serde_json::Value {
    match rework_budgets_from_policy_snapshot(&current_revision.policy_snapshot) {
        Some((candidate_rework_budget, integration_rework_budget)) => default_policy_snapshot(
            approval_policy,
            candidate_rework_budget,
            integration_rework_budget,
        ),
        None => {
            let mut policy_snapshot = current_revision.policy_snapshot.clone();
            if let Some(object) = policy_snapshot.as_object_mut() {
                object.insert(
                    "approval_policy".into(),
                    serde_json::to_value(approval_policy)
                        .expect("approval policy should serialize into JSON"),
                );
            }
            policy_snapshot
        }
    }
}

pub(in crate::router) async fn validate_seed_commit_oid(
    repo_path: &FsPath,
    seed_commit_oid: Option<CommitOid>,
) -> Result<Option<CommitOid>, ApiError> {
    match seed_commit_oid {
        Some(seed_commit_oid) => {
            ensure_reachable_seed(repo_path, "seed_commit_oid", &seed_commit_oid).await?;
            Ok(Some(seed_commit_oid))
        }
        None => Ok(None),
    }
}

pub(in crate::router) async fn resolve_seed_target_commit_oid(
    repo_path: &FsPath,
    seed_target_commit_oid: Option<CommitOid>,
    default_seed_target_commit_oid: CommitOid,
) -> Result<CommitOid, ApiError> {
    match seed_target_commit_oid {
        Some(seed_target_commit_oid) => {
            ensure_reachable_seed(repo_path, "seed_target_commit_oid", &seed_target_commit_oid)
                .await?;
            Ok(seed_target_commit_oid)
        }
        None => Ok(default_seed_target_commit_oid),
    }
}

async fn ensure_reachable_seed(
    repo_path: &FsPath,
    seed_name: &str,
    commit_oid: &CommitOid,
) -> Result<(), ApiError> {
    let reachable = is_commit_reachable_from_any_ref(repo_path, commit_oid)
        .await
        .map_err(git_to_internal)?;

    if !reachable {
        return Err(UseCaseError::RevisionSeedUnreachable(seed_name.into()).into());
    }

    Ok(())
}
