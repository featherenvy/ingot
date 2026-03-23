use std::os::unix::fs::PermissionsExt;

use ingot_agent_adapters::claude_code::ClaudeCodeCliAdapter;
use ingot_agent_adapters::codex::CodexCliAdapter;
use ingot_agent_protocol::adapter::AgentAdapter;
use ingot_agent_protocol::request::AgentRequest;

fn request(may_mutate: bool) -> AgentRequest {
    AgentRequest {
        prompt: "Implement the change".into(),
        working_dir: "/tmp/repo".into(),
        may_mutate,
        timeout_seconds: Some(60),
        output_schema: None,
    }
}

#[tokio::test]
async fn codex_launch_closes_stdin_after_writing_prompt() {
    let root = std::env::temp_dir().join(format!("ingot-codex-adapter-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("create temp dir");

    let cli_path = root.join("fake-codex.sh");
    std::fs::write(
        &cli_path,
        r#"#!/bin/sh
output_path=""
working_dir=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-C" ]; then
    shift
    working_dir="$1"
  fi
  if [ "$1" = "--output-last-message" ]; then
    shift
    output_path="$1"
  fi
  shift
done
[ -n "$working_dir" ] && cd "$working_dir"
cat > "$PWD/stdin.txt"
printf '{"summary":"ok","validation":null}\n' > "$output_path"
"#,
    )
    .expect("write fake cli");
    let mut permissions = std::fs::metadata(&cli_path)
        .expect("fake cli metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&cli_path, permissions).expect("chmod fake cli");

    let adapter = CodexCliAdapter::new(cli_path.clone(), "gpt-5");
    let request = AgentRequest {
        working_dir: root.clone(),
        ..request(true)
    };

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        adapter.launch(&request, &root),
    )
    .await
    .expect("launch timed out")
    .expect("launch response");

    assert_eq!(response.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(root.join("stdin.txt")).expect("stdin capture"),
        request.prompt
    );
    assert_eq!(
        response.result,
        Some(serde_json::json!({ "summary": "ok", "validation": null }))
    );
}

#[tokio::test]
async fn claude_code_launch_closes_stdin_after_writing_prompt() {
    let root = std::env::temp_dir().join(format!("ingot-claude-adapter-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).expect("create temp dir");

    let cli_path = root.join("fake-claude.sh");
    std::fs::write(
        &cli_path,
        r#"#!/bin/sh
# Capture stdin to a file
cat > "$PWD/stdin.txt"
# Emit a Claude --print JSON envelope on stdout
cat <<'EOF'
{"type":"result","subtype":"success","is_error":false,"result":{"summary":"ok","validation":null},"duration_ms":100}
EOF
"#,
    )
    .expect("write fake cli");
    let mut permissions = std::fs::metadata(&cli_path)
        .expect("fake cli metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&cli_path, permissions).expect("chmod fake cli");

    let adapter = ClaudeCodeCliAdapter::new(cli_path.clone(), "claude-sonnet-4-6");
    let request = AgentRequest {
        working_dir: root.clone(),
        ..request(true)
    };

    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        adapter.launch(&request, &root),
    )
    .await
    .expect("launch timed out")
    .expect("launch response");

    assert_eq!(response.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(root.join("stdin.txt")).expect("stdin capture"),
        request.prompt
    );
    assert_eq!(
        response.result,
        Some(serde_json::json!({"summary": "ok", "validation": null}))
    );

    let _ = std::fs::remove_dir_all(root);
}
