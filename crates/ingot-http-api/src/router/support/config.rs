use ingot_config::IngotConfig;
use ingot_config::loader::load_config;
use ingot_config::paths::global_config_path as shared_global_config_path;
use ingot_domain::project::Project;

use crate::error::ApiError;

fn global_config_path() -> std::path::PathBuf {
    shared_global_config_path()
}

fn project_config_path(project: &Project) -> std::path::PathBuf {
    project.path.join(".ingot").join("config.yml")
}

pub(crate) fn load_effective_config(project: Option<&Project>) -> Result<IngotConfig, ApiError> {
    let project_path = project.map(project_config_path);
    load_config(global_config_path().as_path(), project_path.as_deref()).map_err(|error| {
        ApiError::BadRequest {
            code: "config_invalid",
            message: error.to_string(),
        }
    })
}
