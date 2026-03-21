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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use ingot_domain::revision::ApprovalPolicy;

    use super::{ConfigError, load_config};

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ingot-config-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn load_config_parses_typed_approval_policy() {
        let dir = temp_dir("valid");
        let config_path = dir.join("config.yml");
        fs::write(
            &config_path,
            "defaults:\n  candidate_rework_budget: 7\n  integration_rework_budget: 9\n  approval_policy: not_required\n  overflow_strategy: truncate\n",
        )
        .expect("write config");

        let config = load_config(&config_path, None).expect("load config");

        assert_eq!(config.defaults.approval_policy, ApprovalPolicy::NotRequired);
        assert_eq!(config.defaults.candidate_rework_budget, 7);
        assert_eq!(config.defaults.integration_rework_budget, 9);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn load_config_rejects_invalid_approval_policy() {
        let dir = temp_dir("invalid");
        let config_path = dir.join("config.yml");
        fs::write(
            &config_path,
            "defaults:\n  approval_policy: later\n  overflow_strategy: truncate\n",
        )
        .expect("write config");

        let error = load_config(&config_path, None).expect_err("invalid approval_policy");

        match error {
            ConfigError::Parse(message) => assert!(message.contains("approval_policy")),
            other => panic!("expected parse error, got {other:?}"),
        }

        let _ = fs::remove_dir_all(dir);
    }
}
