use std::path::Path;

use crate::schema::IngotConfig;

/// Load and merge config from global defaults and project-level overrides.
pub fn load_config(
    global_path: &Path,
    project_path: Option<&Path>,
) -> Result<IngotConfig, ConfigError> {
    let mut config = if global_path.exists() {
        let contents =
            std::fs::read_to_string(global_path).map_err(|e| ConfigError::Io(e.to_string()))?;
        serde_yml::from_str(&contents).map_err(|e| ConfigError::Parse(e.to_string()))?
    } else {
        IngotConfig::default()
    };

    if let Some(project_path) = project_path {
        if project_path.exists() {
            let contents = std::fs::read_to_string(project_path)
                .map_err(|e| ConfigError::Io(e.to_string()))?;
            let project_config: IngotConfig =
                serde_yml::from_str(&contents).map_err(|e| ConfigError::Parse(e.to_string()))?;
            // Project config overrides global defaults
            config = project_config;
        }
    }

    Ok(config)
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("parse error: {0}")]
    Parse(String),
}
