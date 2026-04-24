use std::path::{Path, PathBuf};

use ingot_agent_protocol::adapter::{AgentAdapter, AgentError};
use ingot_agent_protocol::request::AgentRequest;
use ingot_agent_protocol::response::{
    AgentOutputChannel, AgentOutputChunk, AgentOutputKind, AgentOutputSegmentDraft,
    AgentOutputStatus, AgentResponse,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::{result_from_text, subprocess};

#[derive(Debug, Clone)]
pub struct CodexCliAdapter {
    command: subprocess::AdapterCommandConfig,
}

impl CodexCliAdapter {
    pub fn new(
        cli_path: impl Into<PathBuf>,
        model: impl Into<ingot_domain::agent_model::AgentModel>,
    ) -> Self {
        Self {
            command: subprocess::AdapterCommandConfig::new(cli_path, model),
        }
    }

    fn build_exec_args(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
        schema_file: &subprocess::TempFile,
        response_file: &subprocess::TempFile,
    ) -> Result<Vec<String>, AgentError> {
        let sandbox = if request.may_mutate {
            "danger-full-access"
        } else {
            "read-only"
        };

        Ok(vec![
            "exec".into(),
            "--sandbox".into(),
            sandbox.into(),
            "-C".into(),
            working_dir.to_string_lossy().into_owned(),
            "-m".into(),
            self.command.model().to_string(),
            "--json".into(),
            "--output-schema".into(),
            schema_file.cli_arg("schema")?,
            "--output-last-message".into(),
            response_file.cli_arg("response")?,
            "-".into(),
        ])
    }

    pub async fn launch_with_output(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
        output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    ) -> Result<AgentResponse, AgentError> {
        subprocess::launch_adapter_with_schema_and_result_files(
            &self.command,
            request,
            working_dir,
            subprocess::SchemaResultFiles {
                adapter_name: "codex",
                result_file_stem: ".ingot-codex-last-message",
                result_file_extension: "txt",
            },
            |schema_file, response_file| {
                self.build_exec_args(request, working_dir, schema_file, response_file)
            },
            output_tx,
            Some(Box::new(CodexJsonOutputParser)),
            |_output, response_file| async move {
                let final_message = response_file.read_to_string().await.ok();
                if final_message
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    warn!(
                        response_path = %response_file.path().display(),
                        "codex did not produce a last-message payload"
                    );
                }
                Ok(final_message.as_deref().map(result_from_text))
            },
        )
        .await
    }
}

struct CodexJsonOutputParser;

impl subprocess::StdoutOutputParser for CodexJsonOutputParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentOutputSegmentDraft> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let value = match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(value) => value,
            Err(_) => {
                return vec![AgentOutputSegmentDraft::text(
                    AgentOutputKind::Text,
                    trimmed,
                )];
            }
        };

        parse_codex_event(&value)
    }
}

fn parse_codex_event(value: &serde_json::Value) -> Vec<AgentOutputSegmentDraft> {
    let Some(event_type) = value.get("type").and_then(|value| value.as_str()) else {
        return vec![raw_fallback(
            "codex_output_without_type",
            value.clone(),
            Some("Unrecognized Codex JSON output".into()),
        )];
    };

    match event_type {
        "thread.started" | "thread.completed" | "turn.started" => Vec::new(),
        "turn.completed" => vec![lifecycle_segment(
            "Turn completed",
            value
                .get("summary")
                .and_then(|summary| summary.as_str())
                .map(ToOwned::to_owned),
            Some(AgentOutputStatus::Completed),
            serde_json::json!({
                "provider": "codex",
                "provider_event_type": event_type,
                "usage": value.get("usage").cloned()
            }),
        )],
        "turn.failed" | "error" => vec![lifecycle_segment(
            "Run failed",
            value
                .get("message")
                .and_then(|message| message.as_str())
                .or_else(|| value.get("error").and_then(|error| error.as_str()))
                .map(ToOwned::to_owned),
            Some(AgentOutputStatus::Failed),
            serde_json::json!({
                "provider": "codex",
                "provider_event_type": event_type
            }),
        )],
        "item.started" | "item.completed" | "item.failed" => {
            parse_codex_item_event(event_type, value.get("item").unwrap_or(value))
        }
        _ => vec![raw_fallback(
            event_type,
            value.clone(),
            Some(format!("Unrecognized Codex event: {event_type}")),
        )],
    }
}

fn parse_codex_item_event(
    event_type: &str,
    item: &serde_json::Value,
) -> Vec<AgentOutputSegmentDraft> {
    let item_type = item
        .get("type")
        .or_else(|| item.get("item_type"))
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let status = match event_type {
        "item.started" => Some(AgentOutputStatus::InProgress),
        "item.completed" => Some(AgentOutputStatus::Completed),
        "item.failed" => Some(AgentOutputStatus::Failed),
        _ => None,
    };

    match item_type {
        "agent_message" | "assistant_message" => {
            let text = item
                .get("text")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| item.get("content").and_then(content_text));

            if let Some(text) = text {
                vec![AgentOutputSegmentDraft {
                    channel: AgentOutputChannel::Primary,
                    kind: AgentOutputKind::Text,
                    status,
                    title: None,
                    text: Some(text),
                    data: Some(serde_json::json!({
                        "provider": "codex",
                        "provider_event_type": event_type,
                        "provider_item_type": item_type
                    })),
                }]
            } else {
                vec![raw_fallback(
                    event_type,
                    item.clone(),
                    Some("Codex message event did not include display text".into()),
                )]
            }
        }
        "reasoning" => vec![AgentOutputSegmentDraft {
            channel: AgentOutputChannel::Primary,
            kind: AgentOutputKind::Progress,
            status,
            title: Some("Reasoning".into()),
            text: item
                .get("summary")
                .and_then(|value| value.as_str())
                .or_else(|| item.get("text").and_then(|value| value.as_str()))
                .map(ToOwned::to_owned),
            data: Some(serde_json::json!({
                "provider": "codex",
                "provider_event_type": event_type,
                "provider_item_type": item_type
            })),
        }],
        "command_execution" => vec![AgentOutputSegmentDraft {
            channel: AgentOutputChannel::Primary,
            kind: if matches!(
                status,
                Some(AgentOutputStatus::Completed) | Some(AgentOutputStatus::Failed)
            ) {
                AgentOutputKind::ToolResult
            } else {
                AgentOutputKind::ToolCall
            },
            status,
            title: Some("Command execution".into()),
            text: item
                .get("command")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned),
            data: Some(serde_json::json!({
                "provider": "codex",
                "provider_event_type": event_type,
                "provider_item_type": item_type,
                "command": item.get("command").cloned(),
                "exit_code": item.get("exit_code").cloned()
            })),
        }],
        "web_search" | "mcp_tool_call" | "tool_call" => vec![AgentOutputSegmentDraft {
            channel: AgentOutputChannel::Primary,
            kind: AgentOutputKind::ToolCall,
            status,
            title: Some(item_type.replace('_', " ")),
            text: item
                .get("tool_name")
                .and_then(|value| value.as_str())
                .or_else(|| item.get("query").and_then(|value| value.as_str()))
                .map(ToOwned::to_owned),
            data: Some(serde_json::json!({
                "provider": "codex",
                "provider_event_type": event_type,
                "provider_item_type": item_type,
                "tool_name": item.get("tool_name").cloned(),
                "query": item.get("query").cloned()
            })),
        }],
        "mcp_tool_result" | "tool_result" | "file_change" | "file_changes" | "plan_update" => {
            vec![AgentOutputSegmentDraft {
                channel: AgentOutputChannel::Primary,
                kind: match item_type {
                    "plan_update" => AgentOutputKind::Progress,
                    "file_change" | "file_changes" => AgentOutputKind::Progress,
                    _ => AgentOutputKind::ToolResult,
                },
                status,
                title: Some(item_type.replace('_', " ")),
                text: item
                    .get("text")
                    .and_then(|value| value.as_str())
                    .or_else(|| item.get("summary").and_then(|value| value.as_str()))
                    .map(ToOwned::to_owned),
                data: Some(serde_json::json!({
                    "provider": "codex",
                    "provider_event_type": event_type,
                    "provider_item_type": item_type,
                    "paths": item.get("paths").cloned()
                })),
            }]
        }
        _ => vec![raw_fallback(
            event_type,
            item.clone(),
            Some(format!("Unrecognized Codex item type: {item_type}")),
        )],
    }
}

fn content_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(text) => Some(text.to_owned()),
        serde_json::Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        _ => None,
    }
}

fn lifecycle_segment(
    title: impl Into<String>,
    text: Option<String>,
    status: Option<AgentOutputStatus>,
    data: serde_json::Value,
) -> AgentOutputSegmentDraft {
    AgentOutputSegmentDraft {
        channel: AgentOutputChannel::Diagnostic,
        kind: AgentOutputKind::Lifecycle,
        status,
        title: Some(title.into()),
        text,
        data: Some(data),
    }
}

fn raw_fallback(
    event_type: &str,
    raw: serde_json::Value,
    text: Option<String>,
) -> AgentOutputSegmentDraft {
    AgentOutputSegmentDraft {
        channel: AgentOutputChannel::Diagnostic,
        kind: AgentOutputKind::RawFallback,
        status: Some(AgentOutputStatus::Unknown),
        title: Some("Provider event".into()),
        text,
        data: Some(serde_json::json!({
            "provider": "codex",
            "provider_event_type": event_type,
            "raw": raw
        })),
    }
}

impl AgentAdapter for CodexCliAdapter {
    async fn launch(
        &self,
        request: &AgentRequest,
        working_dir: &Path,
    ) -> Result<AgentResponse, AgentError> {
        self.launch_with_output(request, working_dir, None).await
    }

    async fn cancel(&self, pid: u32) -> Result<(), AgentError> {
        subprocess::cancel_process_group(pid).await
    }
}

#[cfg(test)]
mod tests {
    use crate::subprocess::StdoutOutputParser;

    use super::*;

    fn request(may_mutate: bool) -> AgentRequest {
        AgentRequest {
            prompt: "Implement the change".into(),
            working_dir: "/tmp/repo".into(),
            may_mutate,
            timeout_seconds: Some(60),
            output_schema: None,
        }
    }

    #[test]
    fn build_exec_args_match_current_codex_exec_flags() {
        let adapter = CodexCliAdapter::new("codex", "gpt-5");
        let working_dir = Path::new("/tmp/repo");
        let schema_file = subprocess::TempFile::from_path("/tmp/schema.json");
        let response_file = subprocess::TempFile::from_path("/tmp/last-message.json");
        let args = adapter
            .build_exec_args(&request(true), working_dir, &schema_file, &response_file)
            .expect("build args");

        assert!(args.iter().all(|arg| arg != "--ask-for-approval"));
        assert!(args.iter().all(|arg| arg != "-o"));
        assert!(args.iter().all(|arg| arg != "--config"));
        assert!(args.iter().any(|arg| arg == "--output-last-message"));
        assert_eq!(
            args,
            vec![
                "exec",
                "--sandbox",
                "danger-full-access",
                "-C",
                "/tmp/repo",
                "-m",
                "gpt-5",
                "--json",
                "--output-schema",
                "/tmp/schema.json",
                "--output-last-message",
                "/tmp/last-message.json",
                "-",
            ]
        );
    }

    #[test]
    fn build_exec_args_use_read_only_sandbox_for_non_mutating_jobs() {
        let adapter = CodexCliAdapter::new("codex", "gpt-5");
        let working_dir = Path::new("/tmp/repo");
        let schema_file = subprocess::TempFile::from_path("/tmp/schema.json");
        let response_file = subprocess::TempFile::from_path("/tmp/last-message.json");
        let args = adapter
            .build_exec_args(&request(false), working_dir, &schema_file, &response_file)
            .expect("build args");

        let sandbox_idx = args.iter().position(|arg| arg == "--sandbox").unwrap();
        assert_eq!(args[sandbox_idx + 1], "read-only");
    }

    #[test]
    fn build_exec_args_use_launch_working_dir_for_codex_c_flag() {
        let adapter = CodexCliAdapter::new("codex", "gpt-5");
        let launch_working_dir = Path::new("/tmp/launch-dir");
        let schema_file = subprocess::TempFile::from_path("/tmp/schema.json");
        let response_file = subprocess::TempFile::from_path("/tmp/last-message.json");
        let request = AgentRequest {
            working_dir: "/tmp/request-dir".into(),
            ..request(true)
        };
        let args = adapter
            .build_exec_args(&request, launch_working_dir, &schema_file, &response_file)
            .expect("build args");

        let working_dir_idx = args.iter().position(|arg| arg == "-C").unwrap();
        assert_eq!(Path::new(&args[working_dir_idx + 1]), launch_working_dir);
    }

    #[test]
    fn codex_parser_maps_agent_message_events_to_text_segments() {
        let segments = parse_codex_event(&serde_json::json!({
            "type": "item.completed",
            "item": {
                "id": "item_1",
                "type": "agent_message",
                "text": "hello from codex"
            }
        }));

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].kind, AgentOutputKind::Text);
        assert_eq!(segments[0].text.as_deref(), Some("hello from codex"));
    }

    #[test]
    fn codex_parser_accepts_assistant_message_alias() {
        let segments = parse_codex_event(&serde_json::json!({
            "type": "item.completed",
            "item": {
                "id": "item_1",
                "type": "assistant_message",
                "text": "older alias"
            }
        }));

        assert_eq!(segments[0].kind, AgentOutputKind::Text);
        assert_eq!(segments[0].text.as_deref(), Some("older alias"));
    }

    #[test]
    fn codex_parser_maps_command_execution_events_to_tool_segments() {
        let segments = parse_codex_event(&serde_json::json!({
            "type": "item.started",
            "item": {
                "id": "item_1",
                "type": "command_execution",
                "command": "bash -lc ls"
            }
        }));

        assert_eq!(segments[0].kind, AgentOutputKind::ToolCall);
        assert_eq!(segments[0].status, Some(AgentOutputStatus::InProgress));
    }

    #[test]
    fn codex_parser_maps_turn_completed_events_to_lifecycle_segments() {
        let segments = parse_codex_event(&serde_json::json!({
            "type": "turn.completed",
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        }));

        assert_eq!(segments[0].kind, AgentOutputKind::Lifecycle);
        assert_eq!(segments[0].status, Some(AgentOutputStatus::Completed));
    }

    #[test]
    fn codex_parser_falls_back_for_unknown_item_types() {
        let segments = parse_codex_event(&serde_json::json!({
            "type": "item.completed",
            "item": {
                "id": "item_1",
                "type": "mystery_item"
            }
        }));

        assert_eq!(segments[0].kind, AgentOutputKind::RawFallback);
        assert_eq!(segments[0].status, Some(AgentOutputStatus::Unknown));
    }

    #[test]
    fn codex_parser_treats_plain_text_lines_as_text_segments() {
        let mut parser = CodexJsonOutputParser;
        let segments = parser.parse_line("plain text output");

        assert_eq!(segments[0].kind, AgentOutputKind::Text);
        assert_eq!(segments[0].text.as_deref(), Some("plain text output"));
    }
}
