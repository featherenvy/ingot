use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use ingot_domain::activity::ActivityEventType;
use ingot_domain::ids::ProjectId;
use ingot_domain::item::{Classification, Priority};
use ingot_domain::ports::ProjectMutationLockPort;
use ingot_domain::project::Project;
use ingot_domain::revision::AuthoringBaseSeed;
use ingot_git::commands::resolve_ref_oid;
use ingot_usecases::UseCaseError;
use ingot_usecases::item::{CreateItemInput, create_manual_item, normalize_target_ref};

use crate::error::ApiError;
use crate::router::{
    AppState, append_activity, ensure_git_valid_target_ref, git_to_internal, load_effective_config,
    parse_config_approval_policy, repo_to_internal, repo_to_project_mutation,
    resolve_default_branch,
};

#[derive(Debug, Deserialize)]
pub struct CreateDemoProjectRequest {
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DemoProjectResponse {
    pub project: Project,
    pub items_created: usize,
}

const SAMPLE_ITEMS: &[(&str, &str, &str)] = &[
    // 1. Get both processes running — nothing else works until this is done.
    (
        "001 — Scaffold Express + React monorepo",
        "Set up the monorepo structure. Create server/ with an Express app (package.json with express, better-sqlite3, cors; index.js entry point with cors and json middleware, listening on port 3001). Create client/ with a React + Vite app (package.json with react, react-dom, vite, @vitejs/plugin-react; vite.config.js proxying /api to :3001; index.html; src/main.jsx; src/App.jsx rendering a placeholder heading). Add a root README.md explaining how to start both sides.",
        "- Running `node server/index.js` starts the Express server on port 3001\n- Running `npx vite` inside client/ starts the dev server and renders the placeholder\n- client/vite.config.js proxies /api requests to localhost:3001\n- README.md describes the project and startup steps",
    ),
    // 2. Database first — the API and UI both depend on a schema.
    (
        "002 — Add SQLite schema for companies and contacts",
        "Create server/db.js using better-sqlite3. Define two tables: companies (id integer primary key, name text not null, domain text, industry text, created_at text default current_timestamp) and contacts (id integer primary key, first_name text not null, last_name text not null, email text not null unique, phone text, company_id integer references companies(id), role text, created_at text default current_timestamp). Run CREATE TABLE IF NOT EXISTS on import. Export the db instance.",
        "- require('./db') returns a better-sqlite3 instance without errors\n- companies table exists with columns: id, name, domain, industry, created_at\n- contacts table exists with columns: id, first_name, last_name, email, phone, company_id, role, created_at\n- contacts.company_id is a foreign key to companies.id\n- contacts.email has a unique constraint",
    ),
    // 3. Companies API + UI first — simpler entity with no FK dependencies.
    (
        "003 — Companies CRUD: API endpoints and list page",
        "Expose company management end-to-end. Server: add routes GET /api/companies (list, sorted by name), POST /api/companies (create, name required — return 400 if missing, 201 on success), PUT /api/companies/:id (update, 404 if not found), DELETE /api/companies/:id (404 if not found). Client: add a CompaniesPage with a table (columns: name, domain, industry), an Add Company form (name required, domain and industry optional), and inline edit/delete on each row. Wire it into App.jsx as the default view.",
        "- GET /api/companies returns a JSON array sorted by name\n- POST /api/companies without name returns 400\n- CompaniesPage renders the table and add form\n- Creating a company via the form adds it to the table without a page reload\n- Edit pre-fills current values; delete prompts for confirmation\n- The app loads CompaniesPage by default",
    ),
    // 4. Contacts API + UI — can now reference companies via FK and dropdown.
    (
        "004 — Contacts CRUD: API endpoints and list page",
        "Expose contact management end-to-end. Server: add routes GET /api/contacts (list with company name joined via LEFT JOIN, sorted by last_name), POST /api/contacts (create, first_name + last_name + email required — 400 if missing, 201 on success), PUT /api/contacts/:id (update, 404 if not found), DELETE /api/contacts/:id (404 if not found). Client: add a ContactsPage with a table (columns: name, email, phone, company, role), an Add Contact form with a company dropdown populated from GET /api/companies, and inline edit/delete on each row. Add navigation between CompaniesPage and ContactsPage.",
        "- GET /api/contacts returns contacts with a company_name field from the join\n- POST /api/contacts without required fields returns 400\n- ContactsPage renders the table with a company column\n- The add form includes a dropdown listing existing companies\n- Navigation links switch between CompaniesPage and ContactsPage\n- Edit and delete work the same as on CompaniesPage",
    ),
    // 5. Link the two entities together in the UI.
    (
        "005 — Company detail page with associated contacts",
        "Add a CompanyDetailPage that loads a single company and lists its contacts. Clicking a company name in the companies table navigates to /companies/:id. The detail page shows the company's name, domain, and industry at the top, followed by a table of contacts belonging to that company (reuse the contacts table component with a company_id filter). Add a server endpoint GET /api/companies/:id that returns 404 if not found. Include a back link to the companies list.",
        "- Clicking a company name in the companies table navigates to the detail page\n- CompanyDetailPage shows the company's name, domain, and industry\n- A contacts table below lists only contacts for that company\n- GET /api/companies/:id returns 404 for an unknown id\n- A back link returns to the companies list",
    ),
    // 6. Cross-cutting: search and filter across the contacts list.
    (
        "006 — Add search and company filter to contacts list",
        "Enhance the contacts list with server-side filtering. Add optional query params to GET /api/contacts: search (WHERE first_name || last_name || email LIKE '%…%') and company_id (WHERE company_id = ?). In ContactsPage, add a search text input and a company dropdown filter above the table. Changing either control re-fetches with the updated query params. Show an empty state message when no contacts match. Filters should be clearable.",
        "- GET /api/contacts?search=jane returns contacts matching name or email\n- GET /api/contacts?company_id=2 returns contacts for that company\n- Both params can be combined\n- The search input and company dropdown appear above the contacts table\n- Changing a filter re-fetches and updates the table\n- An empty state message shows when no contacts match\n- Each filter can be cleared independently",
    ),
];

fn init_demo_repo(project_dir: &std::path::Path) -> Result<(), ApiError> {
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
    run_git(&["commit", "--allow-empty", "-m", "Initial empty commit"])?;
    Ok(())
}

pub async fn create_demo_project(
    State(state): State<AppState>,
    Json(request): Json<CreateDemoProjectRequest>,
) -> Result<(StatusCode, Json<DemoProjectResponse>), ApiError> {
    let slug = request
        .name
        .as_deref()
        .unwrap_or("mini-crm")
        .trim()
        .to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
    let slug = if slug.is_empty() {
        "mini-crm".to_string()
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

    init_demo_repo(&project_dir)?;

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
        name: "Demo: Mini CRM".to_string(),
        path: path.display().to_string(),
        default_branch,
        color: "#10b981".to_string(),
        created_at: now,
        updated_at: now,
    };

    state
        .db
        .create_project(&project)
        .await
        .map_err(repo_to_project_mutation)?;

    // Create sample items
    let config = load_effective_config(Some(&project))?;
    let configured_approval_policy = parse_config_approval_policy(&config)?;
    let target_ref = normalize_target_ref(&project.default_branch)?;
    ensure_git_valid_target_ref(&target_ref).await?;
    let repo_path = std::path::Path::new(&project.path);
    let resolved_target_head = resolve_ref_oid(repo_path, &target_ref)
        .await
        .map_err(git_to_internal)?
        .ok_or_else(|| UseCaseError::TargetRefUnresolved(target_ref.clone()))?;

    let _guard = state
        .project_locks
        .acquire_project_mutation(project.id)
        .await;

    let mut items_created = 0;
    for &(title, description, criteria) in SAMPLE_ITEMS {
        let (item, revision) = create_manual_item(
            &project,
            CreateItemInput {
                classification: Classification::Change,
                priority: Priority::Major,
                labels: vec![],
                operator_notes: None,
                title: title.to_string(),
                description: description.to_string(),
                acceptance_criteria: criteria.to_string(),
                target_ref: target_ref.clone(),
                approval_policy: configured_approval_policy,
                candidate_rework_budget: config.defaults.candidate_rework_budget,
                integration_rework_budget: config.defaults.integration_rework_budget,
                seed: AuthoringBaseSeed::Implicit {
                    seed_target_commit_oid: resolved_target_head.clone(),
                },
            },
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
            "item",
            item.id,
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
