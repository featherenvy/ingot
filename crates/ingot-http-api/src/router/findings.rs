use ingot_usecases::finding::{BatchPromoteInput, batch_promote_findings};
use ingot_usecases::item::next_sort_key_after;

use super::deps::*;
use super::dispatch::auto_dispatch_projected_review_job_locked;
use super::support::{
    activity::append_activity,
    errors::{repo_to_finding, repo_to_internal, repo_to_item, repo_to_project},
    path::ApiPath,
    sort_key::next_project_sort_key,
};
use super::types::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/findings/{finding_id}", get(get_finding))
        .route(
            "/api/findings/{finding_id}/triage",
            post(triage_item_finding),
        )
        .route(
            "/api/findings/{finding_id}/promote",
            post(promote_item_from_finding),
        )
        .route(
            "/api/findings/{finding_id}/dismiss",
            post(dismiss_item_finding),
        )
        .route(
            "/api/projects/{project_id}/findings/batch-promote",
            post(batch_promote_findings_handler),
        )
}

pub(super) async fn get_finding(
    State(state): State<AppState>,
    ApiPath(FindingPathParams { finding_id }): ApiPath<FindingPathParams>,
) -> Result<Json<Finding>, ApiError> {
    let finding = state
        .db
        .get_finding(finding_id)
        .await
        .map_err(repo_to_finding)?;
    Ok(Json(finding))
}

#[derive(Debug)]
pub(super) struct AppliedFindingTriage {
    finding: Finding,
    linked_item: Option<Item>,
    linked_revision: Option<ItemRevision>,
}

pub(super) async fn triage_item_finding(
    State(state): State<AppState>,
    ApiPath(FindingPathParams { finding_id }): ApiPath<FindingPathParams>,
    request: TriageFindingRequest,
) -> Result<Json<Finding>, ApiError> {
    let applied = apply_finding_triage(&state, finding_id, request).await?;
    Ok(Json(applied.finding))
}

pub(super) async fn dismiss_item_finding(
    State(state): State<AppState>,
    ApiPath(FindingPathParams { finding_id }): ApiPath<FindingPathParams>,
    Json(request): Json<DismissFindingRequest>,
) -> Result<Json<Finding>, ApiError> {
    let applied = apply_finding_triage(
        &state,
        finding_id,
        TriageFindingRequest {
            triage_state: FindingTriageState::DismissedInvalid,
            triage_note: Some(request.dismissal_reason),
            linked_item_id: None,
            target_ref: None,
            approval_policy: None,
        },
    )
    .await?;
    Ok(Json(applied.finding))
}

pub(super) async fn promote_item_from_finding(
    State(state): State<AppState>,
    ApiPath(FindingPathParams { finding_id }): ApiPath<FindingPathParams>,
    maybe_request: Option<Json<PromoteFindingRequest>>,
) -> Result<Json<PromoteFindingResponse>, ApiError> {
    let request = maybe_request
        .map(|Json(request)| TriageFindingRequest {
            triage_state: FindingTriageState::Backlog,
            triage_note: None,
            linked_item_id: None,
            target_ref: request.target_ref,
            approval_policy: request.approval_policy,
        })
        .unwrap_or(TriageFindingRequest {
            triage_state: FindingTriageState::Backlog,
            triage_note: None,
            linked_item_id: None,
            target_ref: None,
            approval_policy: None,
        });
    let applied = apply_finding_triage(&state, finding_id, request).await?;
    let item = applied.linked_item.ok_or_else(|| ApiError::Conflict {
        code: "linked_item_missing",
        message: "Backlog promotion did not create a linked item".into(),
    })?;
    let current_revision = applied.linked_revision.ok_or_else(|| ApiError::Conflict {
        code: "linked_revision_missing",
        message: "Backlog promotion did not create a linked revision".into(),
    })?;

    Ok(Json(PromoteFindingResponse {
        item,
        current_revision,
        finding: applied.finding,
    }))
}

pub(super) async fn batch_promote_findings_handler(
    State(state): State<AppState>,
    ApiPath(ProjectPathParams { project_id }): ApiPath<ProjectPathParams>,
    Json(request): Json<BatchPromoteFindingsRequest>,
) -> Result<Json<BatchPromoteFindingsResponse>, ApiError> {
    if request.finding_ids.is_empty() {
        return Ok(Json(BatchPromoteFindingsResponse {
            promoted: vec![],
            skipped: vec![],
        }));
    }

    let _project = state
        .db
        .get_project(project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project_id)
        .await;

    // Load the first finding to determine the source item/revision.
    let first_finding = state
        .db
        .get_finding(request.finding_ids[0])
        .await
        .map_err(repo_to_finding)?;
    let source_item = state
        .db
        .get_item(first_finding.source_item_id)
        .await
        .map_err(repo_to_item)?;
    if source_item.project_id != project_id {
        return Err(UseCaseError::ItemNotFound.into());
    }
    let source_revision = state
        .db
        .get_revision(first_finding.source_item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let findings = state
        .db
        .list_findings_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;
    let source_jobs = state
        .db
        .list_jobs_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;

    // Generate sequential sort keys for new items.
    let base_sort_key = next_project_sort_key(&state, project_id).await?;
    let mut last_sort_key = base_sort_key;
    let mut sort_key_fn = || {
        let key = next_sort_key_after(Some(&last_sort_key));
        last_sort_key = key.clone();
        key
    };

    let output = batch_promote_findings(
        &findings,
        &source_item,
        &source_revision,
        &source_jobs,
        BatchPromoteInput {
            finding_ids: request.finding_ids,
        },
        &mut sort_key_fn,
    )?;

    // Persist all promoted findings.
    for result in &output.promoted {
        state
            .db
            .link_backlog_finding(
                &result.triaged_finding,
                &result.linked_item,
                &result.linked_revision,
                None,
            )
            .await
            .map_err(repo_to_internal)?;

        append_activity(
            &state,
            project_id,
            ActivityEventType::FindingTriaged,
            ActivitySubject::Finding(result.finding_id),
            serde_json::json!({
                "item_id": source_item.id,
                "triage_state": result.triaged_finding.triage.state(),
                "linked_item_id": result.linked_item.id,
                "batch": true,
            }),
        )
        .await?;
    }

    let response = BatchPromoteFindingsResponse {
        promoted: output
            .promoted
            .into_iter()
            .map(|r| PromotedFindingResult {
                finding_id: r.finding_id,
                item: r.linked_item,
                current_revision: r.linked_revision,
            })
            .collect(),
        skipped: output
            .skipped
            .into_iter()
            .map(|s| SkippedFindingResult {
                finding_id: s.finding_id,
                reason: s.reason,
            })
            .collect(),
    };

    Ok(Json(response))
}

pub(super) async fn apply_finding_triage(
    state: &AppState,
    finding_id: FindingId,
    request: TriageFindingRequest,
) -> Result<AppliedFindingTriage, ApiError> {
    let finding = state
        .db
        .get_finding(finding_id)
        .await
        .map_err(repo_to_finding)?;
    let source_item = state
        .db
        .get_item(finding.source_item_id)
        .await
        .map_err(repo_to_item)?;
    let source_revision = state
        .db
        .get_revision(finding.source_item_revision_id)
        .await
        .map_err(repo_to_internal)?;
    let project = state
        .db
        .get_project(source_item.project_id)
        .await
        .map_err(repo_to_project)?;
    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;

    let detached_origin_item_id =
        find_detached_origin_item_id(state, &finding, request.linked_item_id).await?;

    let applied = match request.triage_state {
        FindingTriageState::Backlog => {
            ensure_finding_subject_reachable(state, &project, &finding).await?;
            if let Some(linked_item_id) = request.linked_item_id {
                let linked_item =
                    load_linked_item_for_finding(state, &source_item, linked_item_id).await?;
                if linked_item.id == source_item.id {
                    return Err(ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                        "backlog triage must link to a different item".into(),
                    )));
                }
                let triaged = triage_finding(
                    &finding,
                    TriageFindingInput {
                        triage_state: FindingTriageState::Backlog,
                        triage_note: request.triage_note,
                        linked_item_id: Some(linked_item.id),
                    },
                )?;
                state
                    .db
                    .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                    .await
                    .map_err(repo_to_internal)?;
                AppliedFindingTriage {
                    finding: triaged,
                    linked_item: Some(linked_item),
                    linked_revision: None,
                }
            } else {
                let overrides = BacklogFindingOverrides {
                    target_ref: request.target_ref,
                    approval_policy: request.approval_policy,
                };
                let source_jobs = state
                    .db
                    .list_jobs_by_item(source_item.id)
                    .await
                    .map_err(repo_to_internal)?;
                let promotion_overrides = promotion_overrides_for_finding(&finding, &source_jobs);
                let sort_key = next_project_sort_key(state, source_item.project_id).await?;
                let (linked_item, linked_revision, triaged) = backlog_finding_with_promotion(
                    &finding,
                    &source_item,
                    &source_revision,
                    overrides,
                    sort_key,
                    request.triage_note,
                    promotion_overrides,
                )?;
                state
                    .db
                    .link_backlog_finding(
                        &triaged,
                        &linked_item,
                        &linked_revision,
                        detached_origin_item_id,
                    )
                    .await
                    .map_err(repo_to_internal)?;
                AppliedFindingTriage {
                    finding: triaged,
                    linked_item: Some(linked_item),
                    linked_revision: Some(linked_revision),
                }
            }
        }
        FindingTriageState::Duplicate => {
            let linked_item_id = request.linked_item_id.ok_or_else(|| {
                ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                    "duplicate triage requires linked_item_id".into(),
                ))
            })?;
            let linked_item =
                load_linked_item_for_finding(state, &source_item, linked_item_id).await?;
            if linked_item.id == source_item.id {
                return Err(ApiError::UseCase(UseCaseError::InvalidFindingTriage(
                    "duplicate triage must link to a different item".into(),
                )));
            }
            let triaged = triage_finding(
                &finding,
                TriageFindingInput {
                    triage_state: FindingTriageState::Duplicate,
                    triage_note: request.triage_note,
                    linked_item_id: Some(linked_item.id),
                },
            )?;
            state
                .db
                .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                .await
                .map_err(repo_to_internal)?;
            AppliedFindingTriage {
                finding: triaged,
                linked_item: Some(linked_item),
                linked_revision: None,
            }
        }
        _ => {
            let triaged = triage_finding(
                &finding,
                TriageFindingInput {
                    triage_state: request.triage_state,
                    triage_note: request.triage_note,
                    linked_item_id: request.linked_item_id,
                },
            )?;
            state
                .db
                .triage_finding_with_origin_detached(&triaged, detached_origin_item_id)
                .await
                .map_err(repo_to_internal)?;
            AppliedFindingTriage {
                finding: triaged,
                linked_item: None,
                linked_revision: None,
            }
        }
    };
    maybe_enter_approval_after_finding_triage(
        state,
        &source_item,
        &source_revision,
        &applied.finding,
    )
    .await?;
    let infra = state.infra();
    ingot_usecases::dispatch::maybe_cleanup_investigation_ref(
        &state.db,
        &state.db,
        &state.db,
        &infra,
        source_item.project_id,
        &applied.finding,
    )
    .await?;

    append_activity(
        state,
        source_item.project_id,
        ActivityEventType::FindingTriaged,
        ActivitySubject::Finding(applied.finding.id),
        serde_json::json!({
            "item_id": source_item.id,
            "triage_state": applied.finding.triage.state(),
            "linked_item_id": applied.finding.triage.linked_item_id(),
        }),
    )
    .await?;
    if step::is_closure_relevant_review_step(applied.finding.source_step_id) {
        if let Err(error) =
            auto_dispatch_projected_review_job_locked(state, &project, source_item.id).await
        {
            warn!(
                ?error,
                project_id = %source_item.project_id,
                item_id = %source_item.id,
                finding_id = %applied.finding.id,
                "projected review auto-dispatch failed after finding triage"
            );
        }
    }

    Ok(applied)
}

pub(super) async fn find_detached_origin_item_id(
    state: &AppState,
    finding: &Finding,
    next_linked_item_id: Option<ItemId>,
) -> Result<Option<ItemId>, ApiError> {
    let Some(current_linked_item_id) = finding.triage.linked_item_id() else {
        return Ok(None);
    };
    if finding.triage.state() != FindingTriageState::Backlog {
        return Ok(None);
    }
    if next_linked_item_id == Some(current_linked_item_id) {
        return Ok(None);
    }

    let linked_item = state
        .db
        .get_item(current_linked_item_id)
        .await
        .map_err(repo_to_internal)?;

    if linked_item.origin.is_promoted_finding()
        && linked_item.origin.finding_id() == Some(finding.id)
    {
        Ok(Some(linked_item.id))
    } else {
        Ok(None)
    }
}

pub(super) async fn load_linked_item_for_finding(
    state: &AppState,
    source_item: &Item,
    linked_item_id: ItemId,
) -> Result<Item, ApiError> {
    let linked_item = state
        .db
        .get_item(linked_item_id)
        .await
        .map_err(|error| match error {
            RepositoryError::NotFound => ApiError::UseCase(UseCaseError::LinkedItemNotFound),
            other => repo_to_internal(other),
        })?;

    if linked_item.project_id != source_item.project_id {
        return Err(UseCaseError::LinkedItemProjectMismatch.into());
    }

    Ok(linked_item)
}

pub(super) async fn maybe_enter_approval_after_finding_triage(
    state: &AppState,
    source_item: &Item,
    source_revision: &ItemRevision,
    finding: &Finding,
) -> Result<(), ApiError> {
    if finding.source_step_id != ingot_domain::step_id::StepId::ValidateIntegrated
        || source_item.current_revision_id != source_revision.id
    {
        return Ok(());
    }

    let jobs = state
        .db
        .list_jobs_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;
    let latest_closure_findings_job =
        ingot_usecases::dispatch::latest_closure_findings_job(&jobs, source_revision.id);

    let Some(latest_job) = latest_closure_findings_job else {
        return Ok(());
    };
    if latest_job.id != finding.source_job_id {
        return Ok(());
    }

    let findings = state
        .db
        .list_findings_by_item(source_item.id)
        .await
        .map_err(repo_to_internal)?;
    let latest_job_findings = findings
        .iter()
        .filter(|row| row.source_item_revision_id == source_revision.id)
        .filter(|row| row.source_job_id == latest_job.id)
        .collect::<Vec<_>>();

    if latest_job_findings.is_empty()
        || latest_job_findings.iter().any(|row| {
            row.triage.is_unresolved() || row.triage.state() == FindingTriageState::FixNow
        })
    {
        return Ok(());
    }

    let mut item = state
        .db
        .get_item(source_item.id)
        .await
        .map_err(repo_to_item)?;
    let next_approval_state =
        ingot_usecases::item::pending_approval_state(source_revision.approval_policy);
    if item.approval_state != next_approval_state {
        item.approval_state = next_approval_state;
        item.updated_at = Utc::now();
        state
            .db
            .update_item(&item)
            .await
            .map_err(repo_to_internal)?;

        if next_approval_state == ApprovalState::Pending {
            append_activity(
                state,
                item.project_id,
                ActivityEventType::ApprovalRequested,
                ActivitySubject::Item(item.id),
                serde_json::json!({ "source": "finding_triage" }),
            )
            .await?;
        }
    }

    Ok(())
}

pub(super) async fn ensure_finding_subject_reachable(
    state: &AppState,
    project: &Project,
    finding: &Finding,
) -> Result<(), ApiError> {
    let infra = state.infra();
    let head_reachable = infra
        .is_commit_reachable_from_project(project, &finding.source_subject_head_commit_oid)
        .await?;

    if !head_reachable {
        return Err(UseCaseError::FindingSubjectUnreachable.into());
    }

    if finding.source_subject_kind == ingot_domain::finding::FindingSubjectKind::Integrated {
        let Some(base_commit_oid) = finding.source_subject_base_commit_oid.as_ref() else {
            return Err(UseCaseError::FindingSubjectUnreachable.into());
        };
        let base_reachable = infra
            .is_commit_reachable_from_project(project, base_commit_oid)
            .await?;

        if !base_reachable {
            return Err(UseCaseError::FindingSubjectUnreachable.into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use chrono::Utc;
    use ingot_domain::finding::FindingSubjectKind;
    use ingot_domain::ids::{ItemId, ItemRevisionId, JobId, ProjectId};
    use ingot_domain::test_support::FindingBuilder;
    use ingot_test_support::git::temp_git_repo as support_temp_git_repo;
    use ingot_usecases::UseCaseError;
    use uuid::Uuid;

    use crate::error::ApiError;

    use super::super::test_helpers::{test_app_state, test_project};

    fn test_finding() -> Finding {
        FindingBuilder::new(
            ProjectId::from_uuid(Uuid::nil()),
            ItemId::from_uuid(Uuid::nil()),
            ItemRevisionId::from_uuid(Uuid::nil()),
            JobId::from_uuid(Uuid::nil()),
        )
        .source_step_id("investigate_item")
        .source_report_schema_version("finding_report:v1")
        .source_finding_key("finding-1")
        .source_subject_base_commit_oid(None::<String>)
        .created_at(Utc::now())
        .build()
    }

    fn temp_git_repo() -> PathBuf {
        support_temp_git_repo("ingot-http-api")
    }

    fn git_output(path: &std::path::Path, args: &[&str]) -> String {
        ingot_test_support::git::git_output(path, args)
    }

    #[tokio::test]
    async fn promotion_rejects_unreachable_subject_commits() {
        let repo = temp_git_repo();
        let project = test_project(repo.clone());
        let mut finding = test_finding();
        finding.source_subject_head_commit_oid = "deadbeef".into();
        let state = test_app_state().await;

        let result = ensure_finding_subject_reachable(&state, &project, &finding).await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::FindingSubjectUnreachable))
        ));
    }

    #[tokio::test]
    async fn candidate_promotions_only_require_head_reachability() {
        let repo = temp_git_repo();
        let project = test_project(repo.clone());
        let head = git_output(&repo, &["rev-parse", "HEAD"]);
        let mut finding = test_finding();
        finding.source_subject_kind = FindingSubjectKind::Candidate;
        finding.source_subject_head_commit_oid = head.into();
        finding.source_subject_base_commit_oid = Some("deadbeef".into());
        let state = test_app_state().await;

        ensure_finding_subject_reachable(&state, &project, &finding)
            .await
            .expect("candidate finding should remain promotable");
    }

    #[tokio::test]
    async fn invalid_repo_paths_surface_internal_errors_during_reachability_checks() {
        let project =
            test_project(std::env::temp_dir().join(format!("not-a-repo-{}", Uuid::now_v7())));
        let finding = test_finding();
        let state = test_app_state().await;

        let result = ensure_finding_subject_reachable(&state, &project, &finding).await;

        assert!(matches!(
            result,
            Err(ApiError::UseCase(UseCaseError::Internal(_)))
        ));
    }
}
