use ingot_config::IngotConfig;
use ingot_config::loader::load_config;
use ingot_config::paths::{global_config_path, project_config_path};
use ingot_domain::project::Project;

use crate::error::ApiError;

pub(crate) fn load_effective_config(project: Option<&Project>) -> Result<IngotConfig, ApiError> {
    let project_path = project.map(|project| project_config_path(&project.path));
    load_config(global_config_path().as_path(), project_path.as_deref()).map_err(|error| {
        ApiError::BadRequest {
            code: "config_invalid",
            message: error.to_string(),
        }
    })
}
