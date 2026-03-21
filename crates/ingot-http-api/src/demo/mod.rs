pub(crate) mod catalog;
mod finance_tracker;
mod mini_crm;

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::ids::ProjectId;
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::AuthoringBaseSeed;
use ingot_git::commands::resolve_ref_oid;
use ingot_usecases::UseCaseError;
use ingot_usecases::item::{
    CreateItemInput, create_manual_item, next_sort_key_after, normalize_target_ref,
};

use crate::error::ApiError;
use crate::router::{
    AppState, append_activity, ensure_git_valid_target_ref, git_to_internal, load_effective_config,
    repo_to_internal, repo_to_project_mutation, resolve_default_branch,
};

use catalog::{DEMO_CATALOG, find_template};

#[derive(Debug, Deserialize)]
pub struct CreateDemoProjectRequest {
    pub name: Option<String>,
    pub template: Option<String>,
    pub stack: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DemoProjectResponse {
    pub project: Project,
    pub items_created: usize,
}

#[derive(Debug, Serialize)]
pub struct DemoCatalogResponse {
    pub templates: Vec<DemoTemplateSummary>,
}

#[derive(Debug, Serialize)]
pub struct DemoTemplateSummary {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub item_count: usize,
    pub stacks: Vec<DemoStackSummary>,
}

#[derive(Debug, Serialize)]
pub struct DemoStackSummary {
    pub slug: String,
    pub label: String,
}

fn init_demo_repo(project_dir: &std::path::Path, seed_readme: &str) -> Result<(), ApiError> {
    let run_git = |args: &[&str]| -> Result<(), ApiError> {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(project_dir)
            .output()
            .map_err(|e| ApiError::from(UseCaseError::Internal(format!("git failed: {e}"))))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ApiError::from(UseCaseError::Internal(format!(
                "git {}: {stderr}",
                args[0]
            ))));
        }
        Ok(())
    };

    run_git(&["init"])?;
    run_git(&["branch", "-M", "main"])?;
    run_git(&["config", "user.name", "Ingot Demo"])?;
    run_git(&["config", "user.email", "demo@ingot.local"])?;

    std::fs::write(project_dir.join("README.md"), seed_readme).map_err(|e| {
        ApiError::from(UseCaseError::Internal(format!(
            "Failed to write README.md: {e}"
        )))
    })?;

    run_git(&["add", "."])?;
    run_git(&["commit", "-m", "Initial project setup"])?;
    Ok(())
}

pub async fn get_demo_catalog() -> Json<DemoCatalogResponse> {
    Json(DemoCatalogResponse {
        templates: DEMO_CATALOG
            .iter()
            .map(|t| DemoTemplateSummary {
                slug: t.slug.to_string(),
                name: t.name.to_string(),
                description: t.description.to_string(),
                color: t.color.to_string(),
                item_count: t.items.len(),
                stacks: t
                    .stacks
                    .iter()
                    .map(|s| DemoStackSummary {
                        slug: s.slug.to_string(),
                        label: s.label.to_string(),
                    })
                    .collect(),
            })
            .collect(),
    })
}

pub async fn create_demo_project(
    State(state): State<AppState>,
    Json(request): Json<CreateDemoProjectRequest>,
) -> Result<(StatusCode, Json<DemoProjectResponse>), ApiError> {
    let template_slug = request.template.as_deref().unwrap_or("mini-crm");
    let template = find_template(template_slug).ok_or(ApiError::BadRequest {
        code: "invalid_demo_template",
        message: format!("Unknown demo template: {template_slug}"),
    })?;

    let stack_slug = request.stack.as_deref().unwrap_or(template.stacks[0].slug);
    let stack = template
        .find_stack(stack_slug)
        .ok_or(ApiError::BadRequest {
            code: "invalid_demo_stack",
            message: format!("Unknown stack '{stack_slug}' for template '{template_slug}'"),
        })?;

    let slug = request
        .name
        .as_deref()
        .unwrap_or(template.slug)
        .trim()
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
    let slug = if slug.is_empty() {
        template.slug.to_string()
    } else {
        slug
    };

    let home = std::env::var("HOME").map(PathBuf::from).map_err(|_| {
        ApiError::from(UseCaseError::Internal(
            "Cannot determine home directory".into(),
        ))
    })?;
    let documents = home.join("Documents");
    let base = if documents.is_dir() { documents } else { home };
    let project_dir = base.join(format!("ingot-demo-{slug}"));

    if project_dir.exists() {
        return Err(ApiError::BadRequest {
            code: "demo_path_exists",
            message: format!("Directory already exists: {}", project_dir.display()),
        });
    }

    std::fs::create_dir_all(&project_dir).map_err(|e| {
        ApiError::from(UseCaseError::Internal(format!(
            "Failed to create directory: {e}"
        )))
    })?;

    init_demo_repo(&project_dir, stack.seed_readme)?;

    // Register the project
    let path = std::fs::canonicalize(&project_dir).map_err(|e| {
        ApiError::from(UseCaseError::Internal(format!(
            "Failed to canonicalize: {e}"
        )))
    })?;
    let default_branch = resolve_default_branch(&path, Some("main")).await?;
    let now = Utc::now();
    let project = Project {
        id: ProjectId::new(),
        name: format!("Demo: {}", template.name),
        path: path.clone(),
        default_branch,
        color: template.color.to_string(),
        execution_mode: ingot_domain::project::ExecutionMode::default(),
        created_at: now,
        updated_at: now,
    };

    state
        .db
        .create_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;

    // Create items from template
    let config = load_effective_config(Some(&project))?;
    let configured_approval_policy = config.defaults.approval_policy;
    let target_ref = normalize_target_ref(project.default_branch.as_str())?;
    ensure_git_valid_target_ref(target_ref.as_str()).await?;
    let repo_path = &project.path;
    let resolved_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.to_string()))?;

    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;

    let mut items_created = 0;
    let mut previous_sort_key: Option<String> = None;
    for item_def in template.items.iter() {
        let sort_key = next_sort_key_after(previous_sort_key.as_deref());
        previous_sort_key = Some(sort_key.clone());

        let (item, revision) = create_manual_item(
            &project,
            CreateItemInput {
                classification: item_def.classification,
                priority: item_def.priority,
                labels: item_def.labels.iter().map(|s| s.to_string()).collect(),
                operator_notes: None,
                title: item_def.title.to_string(),
                description: item_def.description.to_string(),
                acceptance_criteria: item_def.acceptance_criteria.to_string(),
                target_ref: target_ref.clone(),
                approval_policy: configured_approval_policy,
                candidate_rework_budget: config.defaults.candidate_rework_budget,
                integration_rework_budget: config.defaults.integration_rework_budget,
                seed: AuthoringBaseSeed::Implicit {
                    seed_target_commit_oid: resolved_target_head.clone(),
                },
            },
            sort_key,
            Utc::now(),
        );

        state
            .db
            .create_item_with_revision(&item, &revision)
            .await
            .map_err(repo_to_internal)?;
        append_activity(
            &state,
            project.id,
            ActivityEventType::ItemCreated,
            ActivitySubject::Item(item.id),
            serde_json::json!({ "revision_id": revision.id }),
        )
        .await?;
        items_created += 1;
    }

    Ok((
        StatusCode::CREATED,
        Json(DemoProjectResponse {
            project,
            items_created,
        }),
    ))
}
