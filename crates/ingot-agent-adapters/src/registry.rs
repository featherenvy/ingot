use std::path::{Path, PathBuf};

use ingot_domain::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use ingot_domain::ids::AgentId;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

pub const DEFAULT_CODEX_SLUG: &str = "codex";
pub const DEFAULT_CODEX_NAME: &str = "Codex";
pub const DEFAULT_CODEX_PROVIDER: &str = "openai";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.4";
pub const DEFAULT_CODEX_CLI_PATH: &str = "codex";

pub const DEFAULT_CLAUDE_CODE_SLUG: &str = "claude-code";
pub const DEFAULT_CLAUDE_CODE_NAME: &str = "Claude Code";
pub const DEFAULT_CLAUDE_CODE_PROVIDER: &str = "anthropic";
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

pub fn bootstrap_codex_agent_with(cli_path: impl Into<PathBuf>, model: impl Into<String>) -> Agent {
    Agent {
        id: AgentId::new(),
        slug: DEFAULT_CODEX_SLUG.into(),
        name: DEFAULT_CODEX_NAME.into(),
        adapter_kind: AdapterKind::Codex,
        provider: DEFAULT_CODEX_PROVIDER.into(),
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
    model: impl Into<String>,
) -> Agent {
    Agent {
        id: AgentId::new(),
        slug: DEFAULT_CLAUDE_CODE_SLUG.into(),
        name: DEFAULT_CLAUDE_CODE_NAME.into(),
        adapter_kind: AdapterKind::ClaudeCode,
        provider: DEFAULT_CLAUDE_CODE_PROVIDER.into(),
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

async fn probe_and_apply_with_timeout(agent: &mut Agent, timeout_duration: Duration) {
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
        DEFAULT_CLAUDE_CODE_CLI_PATH, DEFAULT_CLAUDE_CODE_MODEL, DEFAULT_CODEX_CLI_PATH,
        DEFAULT_CODEX_MODEL, bootstrap_claude_code_agent, bootstrap_claude_code_agent_with,
        bootstrap_codex_agent, bootstrap_codex_agent_with, default_agent_capabilities,
        probe_and_apply, probe_and_apply_with_timeout,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::time::Duration;

    use ingot_domain::agent::{AdapterKind, AgentCapability, AgentStatus};

    #[test]
    fn bootstrap_codex_agent_uses_product_defaults() {
        let agent = bootstrap_codex_agent();

        assert_eq!(agent.slug, "codex");
        assert_eq!(agent.name, "Codex");
        assert_eq!(agent.provider, "openai");
        assert_eq!(agent.model, DEFAULT_CODEX_MODEL);
        assert_eq!(agent.cli_path, PathBuf::from(DEFAULT_CODEX_CLI_PATH));
        assert_eq!(
            agent.capabilities,
            default_agent_capabilities(AdapterKind::Codex)
        );
    }

    #[tokio::test]
    async fn probe_and_apply_marks_codex_available_when_probe_succeeds() {
        let root = temp_test_root("codex-probe-ok");
        let fake_codex = write_script(
            &root,
            "fake-codex.sh",
            "#!/bin/sh\necho '--sandbox --output-schema --output-last-message --json'\n",
        );
        let mut agent = bootstrap_codex_agent_with(fake_codex, "gpt-5.4");

        probe_and_apply(&mut agent).await;

        assert_eq!(agent.status, AgentStatus::Available);
        assert_eq!(agent.health_check.as_deref(), Some("codex exec help ok"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_and_apply_marks_codex_unavailable_when_required_flags_are_missing() {
        let root = temp_test_root("codex-probe-bad");
        let fake_codex = write_script(&root, "fake-codex.sh", "#!/bin/sh\necho '--json'\n");
        let mut agent = bootstrap_codex_agent_with(fake_codex, "gpt-5.4");

        probe_and_apply(&mut agent).await;

        assert_eq!(agent.status, AgentStatus::Unavailable);
        assert!(
            agent
                .health_check
                .as_deref()
                .is_some_and(|message| message.contains("--sandbox"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_and_apply_marks_codex_unavailable_when_probe_times_out() {
        let root = temp_test_root("codex-probe-timeout");
        let fake_codex = write_script(&root, "fake-codex.sh", "#!/bin/sh\nsleep 1\n");
        let mut agent = bootstrap_codex_agent_with(fake_codex, "gpt-5.4");

        probe_and_apply_with_timeout(&mut agent, Duration::from_millis(50)).await;

        assert_eq!(agent.status, AgentStatus::Unavailable);
        assert!(
            agent
                .health_check
                .as_deref()
                .is_some_and(|message| message.contains("timed out"))
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bootstrap_claude_code_agent_uses_product_defaults() {
        let agent = bootstrap_claude_code_agent();

        assert_eq!(agent.slug, "claude-code");
        assert_eq!(agent.name, "Claude Code");
        assert_eq!(agent.provider, "anthropic");
        assert_eq!(agent.model, DEFAULT_CLAUDE_CODE_MODEL);
        assert_eq!(agent.cli_path, PathBuf::from(DEFAULT_CLAUDE_CODE_CLI_PATH));
        assert_eq!(
            agent.capabilities,
            default_agent_capabilities(AdapterKind::ClaudeCode)
        );
        assert!(agent.capabilities.contains(&AgentCapability::MutatingJobs));
    }

    #[tokio::test]
    async fn probe_and_apply_marks_claude_code_available_when_probe_succeeds() {
        let root = temp_test_root("claude-probe-ok");
        let fake_claude = write_script(
            &root,
            "fake-claude.sh",
            "#!/bin/sh\necho '--print --output-format --json-schema --model'\n",
        );
        let mut agent = bootstrap_claude_code_agent_with(fake_claude, "claude-sonnet-4-6");

        probe_and_apply(&mut agent).await;

        assert_eq!(agent.status, AgentStatus::Available);
        assert_eq!(agent.health_check.as_deref(), Some("claude help ok"));
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn probe_and_apply_marks_claude_code_unavailable_when_required_flags_are_missing() {
        let root = temp_test_root("claude-probe-bad");
        let fake_claude = write_script(&root, "fake-claude.sh", "#!/bin/sh\necho '--model'\n");
        let mut agent = bootstrap_claude_code_agent_with(fake_claude, "claude-sonnet-4-6");

        probe_and_apply(&mut agent).await;

        assert_eq!(agent.status, AgentStatus::Unavailable);
        assert!(
            agent
                .health_check
                .as_deref()
                .is_some_and(|message| message.contains("--print"))
        );
        let _ = fs::remove_dir_all(root);
    }

    fn write_script(root: &Path, name: &str, body: &str) -> PathBuf {
        fs::create_dir_all(root).expect("create test root");
        let path = root.join(name);
        fs::write(&path, body).expect("write script");
        let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("chmod script");
        path
    }

    fn temp_test_root(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("ingot-{label}-{unique}"))
    }
}
