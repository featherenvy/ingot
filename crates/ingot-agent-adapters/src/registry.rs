use std::path::{Path, PathBuf};

use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentProvider, AgentStatus};
use ingot_domain::agent_model::AgentModel;
use ingot_domain::ids::AgentId;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_AGENT_CAPABILITIES: [AgentCapability; 4] = [
    AgentCapability::ReadOnlyJobs,
    AgentCapability::MutatingJobs,
    AgentCapability::StructuredOutput,
    AgentCapability::StreamingProgress,
];

#[derive(Debug, Clone, Copy)]
struct AdapterRegistryDescriptor {
    kind: AdapterKind,
    default_slug: &'static str,
    default_name: &'static str,
    default_provider: AgentProvider,
    default_model: &'static str,
    default_cli_path: &'static str,
    help_probe_args: &'static [&'static str],
    required_flags: &'static [&'static str],
    required_flags_subject: &'static str,
    success_message: &'static str,
}

const CODEX_DESCRIPTOR: AdapterRegistryDescriptor = AdapterRegistryDescriptor {
    kind: AdapterKind::Codex,
    default_slug: "codex",
    default_name: "Codex",
    default_provider: AgentProvider::OpenAi,
    default_model: "gpt-5.4",
    default_cli_path: "codex",
    help_probe_args: &["exec", "--help"],
    required_flags: &[
        "--config",
        "--sandbox",
        "--output-schema",
        "--output-last-message",
        "--json",
    ],
    required_flags_subject: "codex exec",
    success_message: "codex exec help ok",
};

const CLAUDE_CODE_DESCRIPTOR: AdapterRegistryDescriptor = AdapterRegistryDescriptor {
    kind: AdapterKind::ClaudeCode,
    default_slug: "claude-code",
    default_name: "Claude Code",
    default_provider: AgentProvider::Anthropic,
    default_model: "claude-sonnet-4-6",
    default_cli_path: "claude",
    help_probe_args: &["--help"],
    required_flags: &["--print", "--output-format", "--json-schema", "--model"],
    required_flags_subject: "claude",
    success_message: "claude help ok",
};

const ADAPTER_REGISTRY_DESCRIPTORS: [AdapterRegistryDescriptor; 2] =
    [CODEX_DESCRIPTOR, CLAUDE_CODE_DESCRIPTOR];

pub const DEFAULT_CODEX_SLUG: &str = CODEX_DESCRIPTOR.default_slug;
pub const DEFAULT_CODEX_NAME: &str = CODEX_DESCRIPTOR.default_name;
pub const DEFAULT_CODEX_PROVIDER: AgentProvider = CODEX_DESCRIPTOR.default_provider;
pub const DEFAULT_CODEX_MODEL: &str = CODEX_DESCRIPTOR.default_model;
pub const DEFAULT_CODEX_CLI_PATH: &str = CODEX_DESCRIPTOR.default_cli_path;

pub const DEFAULT_CLAUDE_CODE_SLUG: &str = CLAUDE_CODE_DESCRIPTOR.default_slug;
pub const DEFAULT_CLAUDE_CODE_NAME: &str = CLAUDE_CODE_DESCRIPTOR.default_name;
pub const DEFAULT_CLAUDE_CODE_PROVIDER: AgentProvider = CLAUDE_CODE_DESCRIPTOR.default_provider;
pub const DEFAULT_CLAUDE_CODE_MODEL: &str = CLAUDE_CODE_DESCRIPTOR.default_model;
pub const DEFAULT_CLAUDE_CODE_CLI_PATH: &str = CLAUDE_CODE_DESCRIPTOR.default_cli_path;

fn registry_descriptor(adapter_kind: AdapterKind) -> &'static AdapterRegistryDescriptor {
    ADAPTER_REGISTRY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.kind == adapter_kind)
        .unwrap_or_else(|| panic!("missing registry descriptor for adapter kind: {adapter_kind:?}"))
}

pub fn default_agent_capabilities(adapter_kind: AdapterKind) -> Vec<AgentCapability> {
    let _ = registry_descriptor(adapter_kind);
    DEFAULT_AGENT_CAPABILITIES.to_vec()
}

pub fn bootstrap_codex_agent() -> Agent {
    bootstrap_agent(AdapterKind::Codex)
}

pub fn bootstrap_codex_agent_with(
    cli_path: impl Into<PathBuf>,
    model: impl Into<AgentModel>,
) -> Agent {
    bootstrap_agent_with(AdapterKind::Codex, cli_path, model)
}

pub fn bootstrap_claude_code_agent() -> Agent {
    bootstrap_agent(AdapterKind::ClaudeCode)
}

pub fn bootstrap_claude_code_agent_with(
    cli_path: impl Into<PathBuf>,
    model: impl Into<AgentModel>,
) -> Agent {
    bootstrap_agent_with(AdapterKind::ClaudeCode, cli_path, model)
}

fn bootstrap_agent(adapter_kind: AdapterKind) -> Agent {
    let descriptor = registry_descriptor(adapter_kind);
    bootstrap_agent_with(
        adapter_kind,
        descriptor.default_cli_path,
        descriptor.default_model,
    )
}

fn bootstrap_agent_with(
    adapter_kind: AdapterKind,
    cli_path: impl Into<PathBuf>,
    model: impl Into<AgentModel>,
) -> Agent {
    let descriptor = registry_descriptor(adapter_kind);
    Agent {
        id: AgentId::new(),
        slug: descriptor.default_slug.into(),
        name: descriptor.default_name.into(),
        adapter_kind,
        provider: descriptor.default_provider,
        model: model.into(),
        cli_path: cli_path.into(),
        capabilities: default_agent_capabilities(adapter_kind),
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
    probe_adapter_cli(
        registry_descriptor(agent.adapter_kind),
        &agent.cli_path,
        timeout_duration,
    )
    .await
}

async fn probe_adapter_cli(
    descriptor: &AdapterRegistryDescriptor,
    cli_path: &Path,
    timeout_duration: Duration,
) -> Result<String, String> {
    let probe_label = format!(
        "{} {}",
        cli_path.display(),
        descriptor.help_probe_args.join(" ")
    );
    let output = run_probe_command(
        cli_path,
        descriptor.help_probe_args,
        timeout_duration,
        &probe_label,
    )
    .await?;

    let combined = combined_output(&output.stdout, &output.stderr);
    if output.status.success() {
        validate_help_probe(descriptor, &combined)
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

async fn run_probe_command(
    cli_path: &Path,
    args: &[&str],
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

fn validate_help_probe(
    descriptor: &AdapterRegistryDescriptor,
    output: &str,
) -> Result<String, String> {
    let missing_flags = descriptor
        .required_flags
        .iter()
        .filter(|flag| !output.contains(**flag))
        .copied()
        .collect::<Vec<_>>();
    if !missing_flags.is_empty() {
        return Err(format!(
            "{} is missing required flags: {}",
            descriptor.required_flags_subject,
            missing_flags.join(", ")
        ));
    }

    Ok(descriptor.success_message.into())
}

#[cfg(test)]
mod tests {
    use super::{
        ADAPTER_REGISTRY_DESCRIPTORS, CLAUDE_CODE_DESCRIPTOR, CODEX_DESCRIPTOR,
        DEFAULT_CLAUDE_CODE_CLI_PATH, DEFAULT_CLAUDE_CODE_MODEL, DEFAULT_CLAUDE_CODE_PROVIDER,
        DEFAULT_CODEX_CLI_PATH, DEFAULT_CODEX_MODEL, DEFAULT_CODEX_PROVIDER,
        bootstrap_claude_code_agent, bootstrap_codex_agent, default_agent_capabilities,
    };
    use std::path::PathBuf;

    use ingot_domain::agent::{AdapterKind, AgentCapability};

    #[test]
    fn registry_descriptors_cover_supported_adapter_kinds() {
        let descriptors = ADAPTER_REGISTRY_DESCRIPTORS
            .iter()
            .map(|descriptor| descriptor.kind)
            .collect::<Vec<_>>();

        assert_eq!(
            descriptors,
            vec![CODEX_DESCRIPTOR.kind, CLAUDE_CODE_DESCRIPTOR.kind]
        );
    }

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
        assert!(
            agent
                .capabilities
                .contains(&AgentCapability::StreamingProgress)
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
        assert!(
            agent
                .capabilities
                .contains(&AgentCapability::StreamingProgress)
        );
    }
}
