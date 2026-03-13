use chrono::{DateTime, Utc};
use ingot_domain::item::{
    ApprovalState, Classification, EscalationState, Item, LifecycleState, OriginKind, ParkingState,
    Priority,
};
use ingot_domain::project::Project;
use ingot_domain::revision::{ApprovalPolicy, ItemRevision};
use ingot_workflow::step::DELIVERY_V1_STEPS;
use serde_json::{Map, Value, json};

const DELIVERY_WORKFLOW_VERSION: &str = "delivery:v1";

#[derive(Debug, Clone)]
pub struct CreateItemInput {
    pub classification: Classification,
    pub priority: Priority,
    pub labels: Vec<String>,
    pub operator_notes: Option<String>,
    pub title: String,
    pub description: String,
    pub acceptance_criteria: String,
    pub target_ref: String,
    pub approval_policy: ApprovalPolicy,
    pub candidate_rework_budget: u32,
    pub integration_rework_budget: u32,
    pub seed_commit_oid: String,
    pub seed_target_commit_oid: Option<String>,
}

pub fn create_manual_item(
    project: &Project,
    input: CreateItemInput,
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
        seed_commit_oid,
        seed_target_commit_oid,
    } = input;
    let approval_state = approval_state_for_policy(approval_policy);

    let item = Item {
        id: item_id,
        project_id: project.id,
        classification,
        workflow_version: DELIVERY_WORKFLOW_VERSION.into(),
        lifecycle_state: LifecycleState::Open,
        parking_state: ParkingState::Active,
        done_reason: None,
        resolution_source: None,
        approval_state,
        escalation_state: EscalationState::None,
        escalation_reason: None,
        current_revision_id: revision_id,
        origin_kind: OriginKind::Manual,
        origin_finding_id: None,
        priority,
        labels,
        operator_notes,
        created_at,
        updated_at: created_at,
        closed_at: None,
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
        seed_commit_oid,
        seed_target_commit_oid,
        supersedes_revision_id: None,
        created_at,
    };

    (item, revision)
}

pub fn normalize_target_ref(target_ref: &str) -> String {
    if target_ref.starts_with("refs/") {
        target_ref.into()
    } else {
        format!("refs/heads/{target_ref}")
    }
}

pub fn approval_state_for_policy(approval_policy: ApprovalPolicy) -> ApprovalState {
    match approval_policy {
        ApprovalPolicy::Required => ApprovalState::NotRequested,
        ApprovalPolicy::NotRequired => ApprovalState::NotRequired,
    }
}

pub fn default_policy_snapshot(
    approval_policy: ApprovalPolicy,
    candidate_rework_budget: u32,
    integration_rework_budget: u32,
) -> Value {
    json!({
        "workflow_version": DELIVERY_WORKFLOW_VERSION,
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
    use ingot_domain::ids::ProjectId;
    use ingot_domain::item::{ApprovalState, Classification, Priority};
    use ingot_domain::project::Project;
    use ingot_domain::revision::ApprovalPolicy;
    use serde_json::json;
    use uuid::Uuid;

    use super::{
        CreateItemInput, create_manual_item, normalize_target_ref,
        rework_budgets_from_policy_snapshot,
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
                seed_commit_oid: "seed".into(),
                seed_target_commit_oid: Some("target".into()),
            },
            created_at,
        );

        assert_eq!(item.approval_state, ApprovalState::NotRequested);
        assert_eq!(revision.item_id, item.id);
        assert_eq!(revision.revision_no, 1);
        assert_eq!(revision.target_ref, "refs/heads/main");
        assert_eq!(revision.seed_commit_oid, "seed");
        assert_eq!(revision.seed_target_commit_oid.as_deref(), Some("target"));
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
        assert_eq!(normalize_target_ref("main"), "refs/heads/main");
        assert_eq!(
            normalize_target_ref("refs/heads/release"),
            "refs/heads/release"
        );
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
