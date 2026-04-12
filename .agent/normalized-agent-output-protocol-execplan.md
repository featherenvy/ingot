# Normalize Agent Output For A Shared UI Protocol

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, the UI will no longer receive raw provider-specific JSON when an agent job is running or when a user reloads persisted job logs. Instead, the backend will parse Codex and Claude output into one shared transcript schema and emit that schema over both WebSocket and `GET /api/jobs/:id/logs`. Operators should be able to watch a running job, reload the page, and see the same normalized output model regardless of which provider executed the work.

## Progress

- [x] (2026-04-12 12:11Z) Created and claimed `bd` issue `ingot-f1a` for backend-owned agent output normalization.
- [x] (2026-04-12 12:11Z) Re-read the existing live-streaming implementation and confirmed the remaining leak: the runtime emits raw stdout/stderr lines, `/api/jobs/:id/logs` returns raw `stdout`, `stderr`, and `result`, and the UI appends those raw strings directly.
- [x] (2026-04-12 12:11Z) Performed provider research on current Codex and Claude structured-output and streaming behavior to anchor the protocol design in current upstream docs.
- [x] (2026-04-12 12:17Z) Added shared transcript types, result wrappers, and draft segment support in `crates/ingot-agent-protocol`.
- [x] (2026-04-12 12:18Z) Implemented Codex and Claude stdout parsers in `crates/ingot-agent-adapters`, including drift-tolerant fallback handling and parser fixture tests.
- [x] (2026-04-12 12:19Z) Replaced raw runtime chunk fan-out with normalized `JobOutputDeltaEvent` events and append-only `output.jsonl` persistence while keeping raw artifacts.
- [x] (2026-04-12 12:20Z) Updated `/api/jobs/:id/logs` and `/api/ws` to serve the shared transcript schema and added `/api/jobs/:id/logs/raw` for debug access.
- [x] (2026-04-12 12:21Z) Updated the UI domain types, websocket cache patching, and Jobs page rendering to consume normalized transcript output.
- [x] (2026-04-12 12:23Z) Ran targeted validation: `cargo test -p ingot-agent-adapters`, `cargo test -p ingot-usecases ui_events::tests:: -- --nocapture`, `cargo test -p ingot-agent-runtime running_job_streams_stdout_to_disk_and_ui_before_completion -- --nocapture`, `cargo test -p ingot-http-api logs_route_returns_normalized_output_and_raw_route_returns_raw_artifacts -- --nocapture`, `cargo test -p ingot-http-api serialize_job_output_delta_includes_segment_payload -- --nocapture`, `cargo check`, `cd ui && bunx vitest run src/test/connection-store.test.ts src/test/jobs-page.test.tsx`, `cd ui && bunx tsc --noEmit`, and `cd ui && bunx biome check src/types/domain.ts src/stores/connection.ts src/pages/JobsPage.tsx src/test/connection-store.test.ts src/test/jobs-page.test.tsx`.
- [ ] Close `ingot-f1a`, rebase if needed, push `bd` and Git changes, and verify the branch is up to date with origin.

## Surprises & Discoveries

- Observation: Final structured job results are already normalized in the backend, but live transcript output is not.
  Evidence: `crates/ingot-agent-runtime/src/execution.rs` streams `AgentOutputChunk { stream, chunk }` into UI events, while `crates/ingot-agent-runtime/src/execution.rs` and `crates/ingot-agent-runtime/src/lib.rs` separately persist canonical `result_payload` through `ingot-agent-protocol::report`.

- Observation: Codex’s documented JSON stream is richer than the current adapter uses, and upstream event naming has already drifted.
  Evidence: OpenAI’s Codex non-interactive docs describe JSONL events `thread.*`, `turn.*`, and `item.*`, while public upstream issues document changes such as `assistant_message` to `agent_message` and raw JSON unexpectedly appearing on stdout.

- Observation: Claude’s final structured result is authoritative, but its live `stream-json` transcript contains transport and progress envelopes that should not become part of the UI contract directly.
  Evidence: The Claude CLI and SDK docs describe an `init` envelope, streamed message events, and a final `result` object exposing `structured_output` or retry/error status.

- Observation: Persisted normalized transcript segments need their own per-job ordering instead of reusing websocket sequence numbers.
  Evidence: WebSocket `seq` is global to the daemon process, while persisted `output.jsonl` needs stable, replayable ordering for a single job. The runtime now assigns a separate `segment.sequence` when writing each normalized delta.

- Observation: Retaining raw stdout/stderr alongside normalized output is still useful even after protocol normalization.
  Evidence: The new `/api/jobs/:id/logs/raw` route and `output.jsonl` test fixture proved that normalized UI payloads and debug-only raw artifacts can coexist without the UI depending on provider-private formats.

## Decision Log

- Decision: Keep the final structured job result separate from the transcript schema.
  Rationale: Ingot already relies on `result_schema_version` plus `result_payload` for workflow behavior, revision context, and finding extraction. The new transcript protocol should complement that contract, not replace it.
  Date/Author: 2026-04-12 / Codex

- Decision: Preserve raw provider artifacts for debugging, but move them behind a debug-only API instead of keeping them in the main UI contract.
  Rationale: Provider formats drift and are useful for diagnosis, but exposing them to normal UI code recreates the frontend/backend drift risk this feature is meant to remove.
  Date/Author: 2026-04-12 / Codex

- Decision: Normalize only product-level semantics such as text, progress, tool activity, lifecycle, and fallback records, not full provider-fidelity transcripts.
  Rationale: The stable requirement is a provider-agnostic operator UI. Full provider fidelity would make the shared protocol too coupled to unstable upstream event sets.
  Date/Author: 2026-04-12 / Codex

## Outcomes & Retrospective

The code now ships one backend-owned transcript contract across live streaming and persisted reloads. `ingot-agent-protocol` defines the shared transcript and structured-result types, the adapters normalize Codex and Claude output before it reaches the runtime, the runtime writes `output.jsonl` and emits `JobOutputDeltaEvent`, the HTTP API exposes normalized `/logs` plus debug-only `/logs/raw`, and the UI renders a provider-agnostic `Output` tab instead of raw stdout/stderr.

The validation set passed after formatting. Remaining work for session completion is administrative only: close `ingot-f1a`, push the `bd` state, push the Git branch, and verify the local branch is up to date with origin.

## Context and Orientation

The current live-streaming path was added earlier and lives across four places. `crates/ingot-agent-adapters/src/subprocess.rs` reads provider stdout/stderr line by line and emits `AgentOutputChunk`. `crates/ingot-agent-runtime/src/execution.rs` writes those raw chunks to `stdout.log` and `stderr.log` and publishes `UiEvent::JobLogChunk`. `crates/ingot-http-api/src/router/ws.rs` serializes that event as `job_log_chunk`. `ui/src/stores/connection.ts` appends those raw strings directly into the cached `job-logs` query result, which the Jobs page then renders as plain text.

The existing persisted job-output route lives in `crates/ingot-http-api/src/router/jobs.rs`. It reads `prompt.txt`, `stdout.log`, `stderr.log`, and `result.json` from disk and returns them as `JobLogsResponse` defined in `crates/ingot-http-api/src/router/types.rs`. The corresponding client type is `JobLogs` in `ui/src/types/domain.ts`.

The canonical final report contract already lives in `crates/ingot-agent-protocol/src/report.rs`. Report-producing jobs must complete with `result_schema_version` plus `result_payload`, and multiple use-case modules assume those values are provider-neutral.

In this plan, “transcript” means the operator-visible running log for a job. It includes text the operator should read while the job is running, progress and lifecycle markers, tool activity summaries, and fallback records for unknown provider frames. It does not mean the canonical final report payload that the workflow consumes after completion.

## Plan of Work

Start in `crates/ingot-agent-protocol` by adding transcript types that the rest of the system can share. Define a document-level type for persisted job output and a segment-level type for incremental deltas. Each segment should carry a stable kind, a stable channel, an optional status, optional user-visible text, and a small metadata bag. The schema must be open enough to survive provider drift without changing the UI contract, but constrained enough that the UI never has to inspect provider-native event names to render normal output.

Next, update `crates/ingot-agent-adapters` to parse live provider output before it reaches the runtime. Codex should parse JSONL from `--json` and normalize known event families such as `item.started`, `item.completed`, and `turn.completed`. Claude should parse `stream-json`, ignore `init`, treat the final `result` envelope as authoritative for completion status and structured output, and normalize any useful live assistant or tool activity into transcript segments. Unknown or drifted provider events must become a backend-owned `raw_fallback` segment instead of leaking through as raw provider JSON.

Then, replace the raw runtime streaming path. Instead of `JobLogChunkEvent`, the runtime should publish normalized transcript deltas and append them to a new `output.jsonl` artifact. Keep writing `stdout.log`, `stderr.log`, and `result.json` so diagnosis and historical inspection still work, but stop treating those raw files as the primary UI protocol.

After that, update the HTTP and WebSocket layers. `GET /api/jobs/:id/logs` should return the normalized transcript plus final structured result wrapper, while a new raw debug route should expose the raw files. The WebSocket route should emit one normalized transcript segment per live update. Both HTTP and WS must use the same shared types so the UI sees the same model on initial load, refresh, and live updates.

Finally, update the UI. Replace raw `stdout`/`stderr` cache patching with transcript-segment patching in `ui/src/stores/connection.ts`, update the domain types and query types, and rework the Jobs page to render one transcript panel plus prompt and result tabs. The new renderer must branch only on shared transcript kinds such as `text`, `progress`, `tool_call`, `tool_result`, `lifecycle`, and `raw_fallback`.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

1. Add transcript protocol types and serde support to `crates/ingot-agent-protocol`.
2. Implement provider parsers and parser tests in `crates/ingot-agent-adapters`.
3. Update runtime event types, streaming pump, and persisted artifacts in `crates/ingot-agent-runtime` and `crates/ingot-usecases`.
4. Update HTTP DTOs and routes in `crates/ingot-http-api`.
5. Update UI domain types, connection store, Jobs page, and UI tests in `ui/`.
6. Run focused Rust and UI tests, then broaden to the relevant crate and app gates.
7. Update this ExecPlan, close the `bd` issue, rebase and push both `bd` and Git state, and verify the branch is up to date with origin.

## Validation and Acceptance

Acceptance is behavioral:

Start the daemon and UI with `make dev`, dispatch both a Codex job and a Claude job that emit visible intermediate activity, and open the Jobs page. The `Output` panel should render normalized transcript segments without raw provider JSON blobs. Reloading the page during execution should recover the same transcript from `GET /api/jobs/:id/logs` and then continue appending live WebSocket deltas.

Parser tests must prove that current provider fixtures are normalized correctly, including documented Codex JSONL events, Claude `stream-json` `result` envelopes, unknown provider events, and structured-output failure cases. HTTP and WebSocket tests must prove that the UI-facing payload shape is provider-agnostic.

## Idempotence and Recovery

The new normalized transcript artifact is append-only and should be safe to rebuild by replaying parsed deltas. Raw provider artifacts remain on disk, so parser bugs can be diagnosed without rerunning a job. If a WebSocket sequence gap occurs, the UI should recover from the normalized HTTP route rather than from raw stdout/stderr strings.

## Artifacts and Notes

Likely files for this feature:

- `crates/ingot-agent-protocol/src/lib.rs`
- `crates/ingot-agent-protocol/src/response.rs`
- `crates/ingot-agent-adapters/src/subprocess.rs`
- `crates/ingot-agent-adapters/src/codex.rs`
- `crates/ingot-agent-adapters/src/claude_code.rs`
- `crates/ingot-agent-runtime/src/execution.rs`
- `crates/ingot-agent-runtime/src/lib.rs`
- `crates/ingot-usecases/src/ui_events.rs`
- `crates/ingot-http-api/src/router/ws.rs`
- `crates/ingot-http-api/src/router/jobs.rs`
- `crates/ingot-http-api/src/router/types.rs`
- `ui/src/types/domain.ts`
- `ui/src/stores/connection.ts`
- `ui/src/pages/JobsPage.tsx`

Revision note: created this ExecPlan when moving from design into implementation so the provider research, protocol boundaries, and rollout assumptions are recorded in-repo before code changes begin.

Revision note: updated after implementation to record the shipped transcript schema, the raw debug route, the per-job segment sequencing decision, and the exact validation commands that passed.

## Interfaces and Dependencies

The shared transcript surface should look roughly like this:

    pub struct AgentOutputDocument {
        pub schema_version: String,
        pub segments: Vec<AgentOutputSegment>,
    }

    pub struct AgentOutputSegment {
        pub sequence: u64,
        pub channel: AgentOutputChannel,
        pub kind: AgentOutputKind,
        pub status: Option<AgentOutputStatus>,
        pub title: Option<String>,
        pub text: Option<String>,
        pub data: Option<serde_json::Value>,
    }

    pub enum AgentOutputChannel {
        Primary,
        Diagnostic,
    }

    pub enum AgentOutputKind {
        Text,
        Progress,
        ToolCall,
        ToolResult,
        Lifecycle,
        RawFallback,
    }

    pub enum AgentOutputStatus {
        InProgress,
        Completed,
        Failed,
        Unknown,
    }

The runtime-facing UI event should become:

    pub struct JobOutputDeltaEvent {
        pub project_id: ProjectId,
        pub job_id: JobId,
        pub segment: AgentOutputSegment,
    }

The primary HTTP route should return:

    pub struct JobOutputResponse {
        pub prompt: Option<String>,
        pub output: AgentOutputDocument,
        pub result: Option<JobStructuredResult>,
    }

    pub struct JobStructuredResult {
        pub schema_version: Option<String>,
        pub payload: serde_json::Value,
    }

The raw debug route should keep exposing raw artifacts separately so the UI never mixes normalized and provider-native shapes in one contract.
