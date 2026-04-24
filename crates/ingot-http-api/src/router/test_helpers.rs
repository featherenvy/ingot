use std::path::PathBuf;

use chrono::Utc;
use ingot_domain::ids::ProjectId;
use ingot_domain::project::Project;
use ingot_domain::test_support::ProjectBuilder;
use ingot_test_support::env::temp_state_root;
use ingot_test_support::sqlite::migrated_test_db;
use ingot_usecases::{DispatchNotify, ProjectLocks, UiEventBus};
use uuid::Uuid;

use super::AppState;

pub(super) async fn test_app_state() -> AppState {
    let db = migrated_test_db("ingot-http-api-test").await;
    let state_root = temp_state_root("ingot-http-api-state");
    AppState::new(
        db,
        ProjectLocks::default(),
        state_root,
        DispatchNotify::default(),
        UiEventBus::default(),
    )
}

pub(super) fn test_project(path: PathBuf) -> Project {
    ProjectBuilder::new(path)
        .id(ProjectId::from_uuid(Uuid::nil()))
        .name("Test")
        .created_at(Utc::now())
        .build()
}
