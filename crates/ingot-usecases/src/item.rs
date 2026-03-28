use chrono::{DateTime, Utc};
use ingot_domain::item::{
    ApprovalState, Classification, Escalation, Item, Lifecycle, Origin, ParkingState, Priority,
    WorkflowVersion,
};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed, ItemRevision};
use ingot_workflow::step::DELIVERY_V1_STEPS;
use serde_json::{Map, Value, json};

use ingot_domain::git_ref::GitRef;

use fractional_index::FractionalIndex;

use crate::UseCaseError;

pub fn next_sort_key_after(previous_sort_key: Option<&str>) -> String {
    previous_sort_key
        .map(|sort_key| {
            FractionalIndex::new_after(&FractionalIndex::from_string(sort_key).unwrap_or_default())
        })
        .unwrap_or_default()
        .to_string()
}

/// Returns a sort key that comes after all existing items (append to end).
pub fn next_sort_key(items: &[Item]) -> String {
    next_sort_key_after(
        items
            .iter()
            .max_by_key(|item| &item.sort_key)
            .map(|item| item.sort_key.as_str()),
    )
}

#[derive(Debug, Clone)]
pub struct CreateItemInput {
    pub classification: Classification,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub operator_notes: Option<String>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub target_ref: GitRef,
    pub approval_policy: ApprovalPolicy,
    pub candidate_rework_budget: u32,
    pub integration_rework_budget: u32,
    pub seed: AuthoringBaseSeed,
}

pub fn create_manual_item(
    project: &Project,
    input: CreateItemInput,
    sort_key: String,
    created_at: DateTime<Utc>,
) -> (Item, ItemRevision) {
    let item_id = ingot_domain::ids::ItemId::new();
    let revision_id = ingot_domain::ids::ItemRevisionId::new();
    let CreateItemInput {
        classification,
        priority,
        labels,
        operator_notes,
        title,
        description,
        acceptance_criteria,
        target_ref,
        approval_policy,
        candidate_rework_budget,
        integration_rework_budget,
        seed,
    } = input;
    let approval_state = approval_state_for_policy(approval_policy);

    let item = Item {
        id: item_id,
        project_id: project.id,
        classification,
        workflow_version: WorkflowVersion::DeliveryV1,
        lifecycle: Lifecycle::Open,
        parking_state: ParkingState::Active,
        approval_state,
        escalation: Escalation::None,
        current_revision_id: revision_id,
        origin: Origin::Manual,
        priority,
        labels,
        operator_notes,
        sort_key,
        created_at,
        updated_at: created_at,
    };

    let revision = ItemRevision {
        id: revision_id,
        item_id,
        revision_no: 1,
        title,
        description,
        acceptance_criteria,
        target_ref,
        approval_policy,
        policy_snapshot: default_policy_snapshot(
            approval_policy,
            candidate_rework_budget,
            integration_rework_budget,
        ),
        template_map_snapshot: default_template_map_snapshot(),
        seed,
        supersedes_revision_id: None,
        created_at,
    };

    (item, revision)
}

pub fn normalize_target_ref(target_ref: &str) -> Result<GitRef, UseCaseError> {
    if let Some(branch_name) = target_ref.strip_prefix("refs/heads/") {
        validate_branch_name(target_ref, branch_name)?;
        return Ok(GitRef::new(target_ref));
    }

    if target_ref.starts_with("refs/") {
        return Err(UseCaseError::InvalidTargetRef(target_ref.into()));
    }

    validate_branch_name(target_ref, target_ref)
        .map(|branch_name| GitRef::new(format!("refs/heads/{branch_name}")))
}

pub fn approval_state_for_policy(approval_policy: ApprovalPolicy) -> ApprovalState {
    match approval_policy {
        ApprovalPolicy::Required => ApprovalState::NotRequested,
        ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
    }
}

/// Approval state when requirements are met and approval should be
/// requested (or auto-approved if not required).
pub fn pending_approval_state(approval_policy: ApprovalPolicy) -> ApprovalState {
    match approval_policy {
        ApprovalPolicy::Required => ApprovalState::Pending,
        ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
    }
}

pub fn default_policy_snapshot(
    approval_policy: ApprovalPolicy,
    candidate_rework_budget: u32,
    integration_rework_budget: u32,
) -> Value {
    json!({
        "workflow_version": WorkflowVersion::DeliveryV1,
        "approval_policy": approval_policy,
        "candidate_rework_budget": candidate_rework_budget,
        "integration_rework_budget": integration_rework_budget,
    })
}

pub fn default_template_map_snapshot() -> Value {
    let map = DELIVERY_V1_STEPS
        .iter()
        .filter_map(|step| {
            step.default_template_slug
                .map(|slug| (step.step_id.to_string(), Value::String(slug.to_string())))
        })
        .collect::<Map<String, Value>>();

    Value::Object(map)
}

fn validate_branch_name(original: &str, branch_name: &str) -> Result<String, UseCaseError> {
    if branch_name.is_empty() {
        return Err(UseCaseError::InvalidTargetRef(original.into()));
    }
    Ok(branch_name.into())
}

pub fn rework_budgets_from_policy_snapshot(policy_snapshot: &Value) -> Option<(u32, u32)> {
    let candidate_rework_budget = policy_snapshot["candidate_rework_budget"].as_u64()?;
    let integration_rework_budget = policy_snapshot["integration_rework_budget"].as_u64()?;

    Some((
        u32::try_from(candidate_rework_budget).ok()?,
        u32::try_from(integration_rework_budget).ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use ingot_domain::commit_oid::CommitOid;
    use ingot_domain::ids::ProjectId;
    use ingot_domain::item::{ApprovalState, Classification, Priority};
    use ingot_domain::project::Project;
    use ingot_domain::revision::{ApprovalPolicy, AuthoringBaseSeed};
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CreateItemInput, create_manual_item, next_sort_key, next_sort_key_after,
        normalize_target_ref, rework_budgets_from_policy_snapshot,
    };

    #[test]
    fn create_manual_item_freezes_defaults_for_initial_revision() {
        let created_at = Utc::now();
        let project = Project {
            id: ProjectId::from_uuid(Uuid::nil()),
            name: "Test".into(),
            path: "/tmp/test".into(),
            default_branch: "main".into(),
            color: "#000".into(),
            execution_mode: ingot_domain::project::ExecutionMode::default(),
            agent_routing: None,
            auto_triage_policy: None,
            created_at,
            updated_at: created_at,
        };

        let (item, revision) = create_manual_item(
            &project,
            CreateItemInput {
                classification: Classification::Change,
                priority: Priority::Major,
                labels: vec!["backend".into()],
                operator_notes: Some("Note".into()),
                title: "Title".into(),
                description: "Description".into(),
                acceptance_criteria: "AC".into(),
                target_ref: "refs/heads/main".into(),
                approval_policy: ApprovalPolicy::Required,
                candidate_rework_budget: 3,
                integration_rework_budget: 4,
                seed: AuthoringBaseSeed::Explicit {
                    seed_commit_oid: "seed".into(),
                    seed_target_commit_oid: "target".into(),
                },
            },
            "80".to_string(),
            created_at,
        );

        assert_eq!(item.approval_state, ApprovalState::NotRequested);
        assert_eq!(revision.item_id, item.id);
        assert_eq!(revision.revision_no, 1);
        assert_eq!(revision.target_ref, "refs/heads/main");
        assert_eq!(
            revision.seed.seed_commit_oid().map(CommitOid::as_str),
            Some("seed")
        );
        assert_eq!(revision.seed.seed_target_commit_oid().as_str(), "target");
        assert_eq!(revision.policy_snapshot["candidate_rework_budget"], 3);
        assert_eq!(revision.policy_snapshot["integration_rework_budget"], 4);
        assert_eq!(
            revision.template_map_snapshot["author_initial"].as_str(),
            Some("author-initial")
        );
        assert_eq!(
            revision.template_map_snapshot["review_incremental_initial"].as_str(),
            Some("review-incremental")
        );
    }

    #[test]
    fn normalize_target_ref_prefixes_branch_names() {
        assert_eq!(
            normalize_target_ref("main").expect("normalize main"),
            "refs/heads/main"
        );
        assert_eq!(
            normalize_target_ref("refs/heads/release").expect("normalize heads ref"),
            "refs/heads/release"
        );
    }

    #[test]
    fn next_sort_key_defaults_without_existing_items() {
        assert_eq!(next_sort_key(&[]), next_sort_key_after(None));
    }

    #[test]
    fn next_sort_key_after_reuses_fractional_index_progression() {
        let first = next_sort_key_after(None);
        let second = next_sort_key_after(Some(&first));

        assert!(second > first);
    }

    #[test]
    fn normalize_target_ref_rejects_non_branch_refs() {
        assert_eq!(
            normalize_target_ref("refs/tags/v1")
                .expect_err("reject tag ref")
                .to_string(),
            "invalid target ref: refs/tags/v1"
        );
        assert_eq!(
            normalize_target_ref("refs/remotes/origin/main")
                .expect_err("reject remote ref")
                .to_string(),
            "invalid target ref: refs/remotes/origin/main"
        );
    }

    #[test]
    fn normalize_target_ref_accepts_valid_branch_names() {
        assert_eq!(
            normalize_target_ref("feature/ref-hardening").expect("normalize nested branch"),
            "refs/heads/feature/ref-hardening"
        );
        assert_eq!(
            normalize_target_ref("release-2026.03").expect("normalize dotted branch"),
            "refs/heads/release-2026.03"
        );
        assert_eq!(
            normalize_target_ref("refs/heads/hotfix_123").expect("normalize full ref"),
            "refs/heads/hotfix_123"
        );
    }

    #[test]
    fn normalize_target_ref_only_rejects_empty_branch_names() {
        for invalid_ref in ["", "refs/heads/"] {
            let error = normalize_target_ref(invalid_ref)
                .err()
                .unwrap_or_else(|| panic!("expected invalid ref: {invalid_ref}"));
            assert_eq!(
                error.to_string(),
                format!("invalid target ref: {invalid_ref}")
            );
        }
    }

    #[test]
    fn rework_budgets_are_read_from_policy_snapshot() {
        let budgets = rework_budgets_from_policy_snapshot(&json!({
            "candidate_rework_budget": 5,
            "integration_rework_budget": 6
        }));

        assert_eq!(budgets, Some((5, 6)));
    }
}
