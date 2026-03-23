use std::path::{Path, PathBuf};

use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentProvider, AgentStatus};
use ingot_domain::agent_model::AgentModel;
use ingot_domain::ids::AgentId;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

pub const DEFAULT_CODEX_SLUG: &str = "codex";
pub const DEFAULT_CODEX_NAME: &str = "Codex";
pub const DEFAULT_CODEX_PROVIDER: AgentProvider = AgentProvider::OpenAi;
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.4";
pub const DEFAULT_CODEX_CLI_PATH: &str = "codex";

pub const DEFAULT_CLAUDE_CODE_SLUG: &str = "claude-code";
pub const DEFAULT_CLAUDE_CODE_NAME: &str = "Claude Code";
pub const DEFAULT_CLAUDE_CODE_PROVIDER: AgentProvider = AgentProvider::Anthropic;
pub const DEFAULT_CLAUDE_CODE_MODEL: &str = "claude-sonnet-4-6";
pub const DEFAULT_CLAUDE_CODE_CLI_PATH: &str = "claude";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn default_agent_capabilities(adapter_kind: AdapterKind) -> Vec<AgentCapability> {
    match adapter_kind {
        AdapterKind::Codex => vec![
            AgentCapability::ReadOnlyJobs,
            AgentCapability::MutatingJobs,
            AgentCapability::StructuredOutput,
        ],
        AdapterKind::ClaudeCode => vec![
            AgentCapability::ReadOnlyJobs,
            AgentCapability::MutatingJobs,
            AgentCapability::StructuredOutput,
        ],
    }
}

pub fn bootstrap_codex_agent() -> Agent {
    bootstrap_codex_agent_with(DEFAULT_CODEX_CLI_PATH, DEFAULT_CODEX_MODEL)
}

pub fn bootstrap_codex_agent_with(
    cli_path: impl Into<PathBuf>,
    model: impl Into<AgentModel>,
) -> Agent {
    Agent {
        id: AgentId::new(),
        slug: DEFAULT_CODEX_SLUG.into(),
        name: DEFAULT_CODEX_NAME.into(),
        adapter_kind: AdapterKind::Codex,
        provider: DEFAULT_CODEX_PROVIDER,
        model: model.into(),
        cli_path: cli_path.into(),
        capabilities: default_agent_capabilities(AdapterKind::Codex),
        health_check: None,
        status: AgentStatus::Probing,
    }
}

pub fn bootstrap_claude_code_agent() -> Agent {
    bootstrap_claude_code_agent_with(DEFAULT_CLAUDE_CODE_CLI_PATH, DEFAULT_CLAUDE_CODE_MODEL)
}

pub fn bootstrap_claude_code_agent_with(
    cli_path: impl Into<PathBuf>,
    model: impl Into<AgentModel>,
) -> Agent {
    Agent {
        id: AgentId::new(),
        slug: DEFAULT_CLAUDE_CODE_SLUG.into(),
        name: DEFAULT_CLAUDE_CODE_NAME.into(),
        adapter_kind: AdapterKind::ClaudeCode,
        provider: DEFAULT_CLAUDE_CODE_PROVIDER,
        model: model.into(),
        cli_path: cli_path.into(),
        capabilities: default_agent_capabilities(AdapterKind::ClaudeCode),
        health_check: None,
        status: AgentStatus::Probing,
    }
}

pub async fn probe_and_apply(agent: &mut Agent) {
    probe_and_apply_with_timeout(agent, PROBE_TIMEOUT).await;
}

pub async fn probe_and_apply_with_timeout(agent: &mut Agent, timeout_duration: Duration) {
    agent.status = AgentStatus::Probing;
    agent.health_check = None;

    match probe_agent_cli(agent, timeout_duration).await {
        Ok(message) => {
            agent.status = AgentStatus::Available;
            agent.health_check = Some(if message.is_empty() {
                "probe ok".into()
            } else {
                message
            });
        }
        Err(message) => {
            agent.status = AgentStatus::Unavailable;
            agent.health_check = Some(message);
        }
    }
}

async fn probe_agent_cli(agent: &Agent, timeout_duration: Duration) -> Result<String, String> {
    match agent.adapter_kind {
        AdapterKind::Codex => probe_codex_cli(&agent.cli_path, timeout_duration).await,
        AdapterKind::ClaudeCode => probe_claude_code_cli(&agent.cli_path, timeout_duration).await,
    }
}

async fn probe_codex_cli(cli_path: &Path, timeout_duration: Duration) -> Result<String, String> {
    let output = run_probe_command(
        cli_path,
        ["exec", "--help"],
        timeout_duration,
        "codex exec --help",
    )
    .await?;

    let combined = combined_output(&output.stdout, &output.stderr);
    if output.status.success() {
        validate_codex_exec_probe(&combined)
    } else if combined.is_empty() {
        Err(format!(
            "{} exited with status {}",
            cli_path.display(),
            output.status
        ))
    } else {
        Err(combined)
    }
}

async fn probe_claude_code_cli(
    cli_path: &Path,
    timeout_duration: Duration,
) -> Result<String, String> {
    let output = run_probe_command(cli_path, ["--help"], timeout_duration, "claude --help").await?;

    let combined = combined_output(&output.stdout, &output.stderr);
    if output.status.success() {
        validate_claude_code_help_probe(&combined)
    } else if combined.is_empty() {
        Err(format!(
            "{} exited with status {}",
            cli_path.display(),
            output.status
        ))
    } else {
        Err(combined)
    }
}

fn validate_claude_code_help_probe(output: &str) -> Result<String, String> {
    let required_flags = ["--print", "--output-format", "--json-schema", "--model"];
    let missing_flags = required_flags
        .iter()
        .filter(|flag| !output.contains(**flag))
        .copied()
        .collect::<Vec<_>>();
    if !missing_flags.is_empty() {
        return Err(format!(
            "claude is missing required flags: {}",
            missing_flags.join(", ")
        ));
    }

    Ok("claude help ok".into())
}

async fn run_probe_command<const N: usize>(
    cli_path: &Path,
    args: [&str; N],
    timeout_duration: Duration,
    probe_label: &str,
) -> Result<std::process::Output, String> {
    timeout(timeout_duration, Command::new(cli_path).args(args).output())
        .await
        .map_err(|_| {
            format!(
                "{probe_label} timed out after {}s",
                timeout_duration.as_secs()
            )
        })?
        .map_err(|error| error.to_string())
}

fn combined_output(stdout: &[u8], stderr: &[u8]) -> String {
    if stdout.is_empty() {
        String::from_utf8_lossy(stderr).trim().to_string()
    } else {
        String::from_utf8_lossy(stdout).trim().to_string()
    }
}

fn validate_codex_exec_probe(output: &str) -> Result<String, String> {
    let required_flags = [
        "--config",
        "--sandbox",
        "--output-schema",
        "--output-last-message",
        "--json",
    ];
    let missing_flags = required_flags
        .iter()
        .filter(|flag| !output.contains(**flag))
        .copied()
        .collect::<Vec<_>>();
    if !missing_flags.is_empty() {
        return Err(format!(
            "codex exec is missing required flags: {}",
            missing_flags.join(", ")
        ));
    }

    Ok("codex exec help ok".into())
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_CLAUDE_CODE_CLI_PATH, DEFAULT_CLAUDE_CODE_MODEL, DEFAULT_CLAUDE_CODE_PROVIDER,
        DEFAULT_CODEX_CLI_PATH, DEFAULT_CODEX_MODEL, DEFAULT_CODEX_PROVIDER,
        bootstrap_claude_code_agent, bootstrap_codex_agent, default_agent_capabilities,
    };
    use std::path::PathBuf;

    use ingot_domain::agent::{AdapterKind, AgentCapability};

    #[test]
    fn bootstrap_codex_agent_uses_product_defaults() {
        let agent = bootstrap_codex_agent();

        assert_eq!(agent.slug, "codex");
        assert_eq!(agent.name, "Codex");
        assert_eq!(agent.provider, DEFAULT_CODEX_PROVIDER);
        assert_eq!(agent.model, DEFAULT_CODEX_MODEL);
        assert_eq!(agent.cli_path, PathBuf::from(DEFAULT_CODEX_CLI_PATH));
        assert_eq!(
            agent.capabilities,
            default_agent_capabilities(AdapterKind::Codex)
        );
    }

    #[test]
    fn bootstrap_claude_code_agent_uses_product_defaults() {
        let agent = bootstrap_claude_code_agent();

        assert_eq!(agent.slug, "claude-code");
        assert_eq!(agent.name, "Claude Code");
        assert_eq!(agent.provider, DEFAULT_CLAUDE_CODE_PROVIDER);
        assert_eq!(agent.model, DEFAULT_CLAUDE_CODE_MODEL);
        assert_eq!(agent.cli_path, PathBuf::from(DEFAULT_CLAUDE_CODE_CLI_PATH));
        assert_eq!(
            agent.capabilities,
            default_agent_capabilities(AdapterKind::ClaudeCode)
        );
        assert!(agent.capabilities.contains(&AgentCapability::MutatingJobs));
    }
}
