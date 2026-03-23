use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Duration;

use ingot_agent_adapters::registry::{
    bootstrap_claude_code_agent_with, bootstrap_codex_agent_with, probe_and_apply,
    probe_and_apply_with_timeout,
};
use ingot_domain::agent::AgentStatus;

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

#[tokio::test]
async fn probe_and_apply_marks_codex_available_when_probe_succeeds() {
    let root = temp_test_root("codex-probe-ok");
    let fake_codex = write_script(
        &root,
        "fake-codex.sh",
        "#!/bin/sh\necho '--config --sandbox --output-schema --output-last-message --json'\n",
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
async fn probe_and_apply_marks_codex_unavailable_when_config_flag_is_missing() {
    let root = temp_test_root("codex-probe-missing-config");
    let fake_codex = write_script(
        &root,
        "fake-codex.sh",
        "#!/bin/sh\necho '--sandbox --output-schema --output-last-message --json'\n",
    );
    let mut agent = bootstrap_codex_agent_with(fake_codex, "gpt-5.4");

    probe_and_apply(&mut agent).await;

    assert_eq!(agent.status, AgentStatus::Unavailable);
    assert!(
        agent
            .health_check
            .as_deref()
            .is_some_and(|message| message.contains("--config"))
    );
    let _ = fs::remove_dir_all(root);
}

#[tokio::test]
async fn probe_and_apply_marks_codex_available_without_forced_service_tier_override() {
    let root = temp_test_root("codex-probe-no-service-tier-override");
    let fake_codex = write_script(
        &root,
        "fake-codex.sh",
        r#"#!/bin/sh
if [ "$1" = "exec" ] && [ "$2" = "--help" ] && [ "$#" -eq 2 ]; then
  echo '--config --sandbox --output-schema --output-last-message --json'
  exit 0
fi
echo 'unexpected probe invocation' >&2
exit 1
"#,
    );
    let mut agent = bootstrap_codex_agent_with(fake_codex, "gpt-5.4");

    probe_and_apply(&mut agent).await;

    assert_eq!(agent.status, AgentStatus::Available);
    assert_eq!(agent.health_check.as_deref(), Some("codex exec help ok"));
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
