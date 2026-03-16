use std::path::Path as FsPath;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use ingot_domain::ids::ProjectId;
use ingot_domain::project::Project;
use ingot_git::GitJobCompletionPort;
use ingot_git::project_repo::project_repo_paths;
use ingot_test_support::fixtures::ProjectBuilder;
use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::{CompleteJobService, ProjectLocks};
use uuid::Uuid;

use super::AppState;

pub(super) async fn test_app_state() -> AppState {
    let db = migrated_test_db("ingot-http-api-test").await;
    let state_root = std::env::temp_dir().join(format!("ingot-http-api-state-{}", Uuid::now_v7()));
    let resolver_state_root = state_root.clone();
    AppState {
        db: db.clone(),
        complete_job_service: CompleteJobService::with_repo_path_resolver(
            db,
            GitJobCompletionPort,
            ProjectLocks::default(),
            Arc::new(move |project: &Project| {
                project_repo_paths(
                    resolver_state_root.as_path(),
                    project.id,
                    FsPath::new(&project.path),
                )
                .mirror_git_dir
            }),
        ),
        project_locks: ProjectLocks::default(),
        state_root,
    }
}

pub(super) fn test_project(path: PathBuf) -> Project {
    ProjectBuilder::new(path)
        .id(ProjectId::from_uuid(Uuid::nil()))
        .name("Test")
        .created_at(Utc::now())
        .build()
}
