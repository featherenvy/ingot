use std::time::Duration;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// A project-level harness profile declaring verification commands and repo-local skills.
///
/// Read live from `<repo>/.ingot/harness.toml` at execution time. Not frozen into revisions.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessProfile {
    pub commands: Vec<HarnessCommand>,
    pub skills: HarnessSkills,
}

#[derive(Debug, Clone, Serialize)]
pub struct HarnessCommand {
    pub name: String,
    pub run: String,
    #[serde(serialize_with = "serialize_duration")]
    pub timeout: Duration,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HarnessSkills {
    pub paths: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessProfileError {
    #[error("invalid TOML: {0}")]
    InvalidToml(#[from] toml::de::Error),
    #[error("invalid duration: {0}")]
    InvalidDuration(String),
}

impl HarnessProfile {
    /// Parse a harness profile from TOML content.
    pub fn from_toml(content: &str) -> Result<Self, HarnessProfileError> {
        let raw: HarnessProfileToml = toml::from_str(content)?;
        let commands = raw
            .commands
            .unwrap_or_default()
            .into_iter()
            .map(|(name, command)| {
                Ok(HarnessCommand {
                    name,
                    run: command.run,
                    timeout: parse_timeout(command.timeout.as_deref())?,
                })
            })
            .collect::<Result<Vec<_>, HarnessProfileError>>()?;
        let skills = HarnessSkills {
            paths: raw
                .skills
                .and_then(|skills| skills.paths)
                .unwrap_or_default(),
        };
        Ok(Self { commands, skills })
    }
}

// ── TOML raw deserialization structs ──

#[derive(Deserialize)]
struct HarnessProfileToml {
    commands: Option<IndexMap<String, CommandToml>>,
    skills: Option<SkillsToml>,
}

#[derive(Deserialize)]
struct CommandToml {
    run: String,
    timeout: Option<String>,
}

#[derive(Deserialize)]
struct SkillsToml {
    paths: Option<Vec<String>>,
}

// ── Duration parsing ──

/// Parse a simple duration string: `30s`, `5m`, `2h`.
pub fn parse_duration(s: &str) -> Result<Duration, HarnessProfileError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(HarnessProfileError::InvalidDuration(
            "empty duration".into(),
        ));
    }

    let (digits, suffix) = s.split_at(s.len() - 1);
    let value: u64 = digits
        .parse()
        .map_err(|_| HarnessProfileError::InvalidDuration(s.into()))?;

    let seconds = match suffix {
        "s" => value,
        "m" => value * 60,
        "h" => value * 3600,
        _ => return Err(HarnessProfileError::InvalidDuration(s.into())),
    };

    Ok(Duration::from_secs(seconds))
}

fn parse_timeout(timeout: Option<&str>) -> Result<Duration, HarnessProfileError> {
    timeout
        .map(parse_duration)
        .transpose()
        .map(|value| value.unwrap_or(DEFAULT_COMMAND_TIMEOUT))
}

fn serialize_duration<S: serde::Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    let secs = d.as_secs();
    let formatted = if secs % 3600 == 0 && secs > 0 {
        format!("{}h", secs / 3600)
    } else if secs % 60 == 0 && secs > 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    };
    s.serialize_str(&formatted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_profile() {
        let toml = r#"
[commands.build]
run = "make build"
timeout = "5m"

[commands.test]
run = "make test"
timeout = "10m"

[commands.lint]
run = "make lint"
timeout = "2m"

[skills]
paths = [".ingot/skills/*.md"]
"#;
        let profile = HarnessProfile::from_toml(toml).unwrap();
        assert_eq!(profile.commands.len(), 3);
        assert_eq!(profile.commands[0].name, "build");
        assert_eq!(profile.commands[0].run, "make build");
        assert_eq!(profile.commands[0].timeout, Duration::from_secs(300));
        assert_eq!(profile.commands[1].name, "test");
        assert_eq!(profile.commands[1].timeout, Duration::from_secs(600));
        assert_eq!(profile.commands[2].name, "lint");
        assert_eq!(profile.commands[2].timeout, Duration::from_secs(120));
        assert_eq!(profile.skills.paths, vec![".ingot/skills/*.md"]);
    }

    #[test]
    fn parse_empty_profile() {
        let profile = HarnessProfile::from_toml("").unwrap();
        assert!(profile.commands.is_empty());
        assert!(profile.skills.paths.is_empty());
    }

    #[test]
    fn parse_commands_only() {
        let toml = r#"
[commands.check]
run = "cargo check"
"#;
        let profile = HarnessProfile::from_toml(toml).unwrap();
        assert_eq!(profile.commands.len(), 1);
        assert_eq!(profile.commands[0].name, "check");
        // Default timeout of 5m
        assert_eq!(profile.commands[0].timeout, Duration::from_secs(300));
    }

    #[test]
    fn parse_duration_variants() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn parse_duration_invalid() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("5x").is_err());
    }

    #[test]
    fn invalid_toml_returns_error() {
        assert!(HarnessProfile::from_toml("[invalid").is_err());
    }

    #[test]
    fn preserves_declaration_order() {
        let toml = r#"
[commands.lint]
run = "make lint"

[commands.build]
run = "make build"

[commands.test]
run = "make test"
"#;
        let profile = HarnessProfile::from_toml(toml).unwrap();
        assert_eq!(profile.commands[0].name, "lint");
        assert_eq!(profile.commands[1].name, "build");
        assert_eq!(profile.commands[2].name, "test");
    }

    #[test]
    fn default_is_empty() {
        let profile = HarnessProfile::default();
        assert!(profile.commands.is_empty());
        assert!(profile.skills.paths.is_empty());
    }
}
