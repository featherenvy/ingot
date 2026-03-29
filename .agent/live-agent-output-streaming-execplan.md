# Stream Live Agent Output From Runtime To UI

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with [.agent/PLANS.md](/Users/aa/Documents/ingot/.agent/PLANS.md).

## Purpose / Big Picture

After this change, a running agent job should stream its stdout and stderr into the UI while the subprocess is still running. An operator should be able to open the Jobs page, select a running job, and watch output appear incrementally instead of waiting until completion. The daemon should also keep writing `stdout.log` and `stderr.log` during execution so reconnects and refreshes can recover the current log state from disk.

## Progress

- [x] (2026-03-29 17:37Z) Created and claimed `bd` issue `ingot-4zl` for live runtime-to-UI agent output streaming.
- [x] (2026-03-29 17:45Z) Inspected the current execution path and confirmed the buffering bottleneck: adapter subprocess output is collected in memory, runtime artifacts are written only after completion, and the UI only fetches whole log files.
- [x] (2026-03-29 17:52Z) Revised the design to use typed internal events, typed output-stream metadata, and explicit `tokio` channel roles (`mpsc` for per-job chunk delivery, `broadcast` for UI fan-out) instead of stringly internal payloads.
- [x] (2026-03-29 18:02Z) Added `UiEventBus` plus typed `UiEvent`, `JobLogChunkEvent`, and `OutputStream` contracts and threaded the shared bus through daemon startup, runtime constructors, and HTTP app state.
- [x] (2026-03-29 18:10Z) Streamed stdout/stderr chunks from the adapter/runtime boundary into append-only log files and published typed `job_log_chunk` events while the job is still running.
- [x] (2026-03-29 18:16Z) Added a real `/api/ws` route that serializes typed internal events into the existing websocket envelope and forwards both entity-change and live log chunk events.
- [x] (2026-03-29 18:19Z) Updated the UI websocket client to patch cached `job-logs` queries in place on `job_log_chunk` events and invalidate all log caches on websocket sequence gaps.
- [x] (2026-03-29 18:27Z) Added focused backend and frontend tests and ran the relevant Rust and UI validation commands successfully.
- [ ] Close `ingot-4zl`, rebase if needed, push `bd` and Git changes, and verify the branch is up to date with origin.

## Surprises & Discoveries

- Observation: The UI already attempts to connect to `/api/ws`, but the HTTP API crate does not currently define that route.
  Evidence: `ui/src/stores/connection.ts` opens `ws://.../api/ws`, while `crates/ingot-http-api/src/router/core.rs` and `crates/ingot-http-api/src/router/app.rs` expose no websocket route.

- Observation: The adapter already sees subprocess output incrementally before completion, but it discards that advantage by aggregating everything into one final `String`.
  Evidence: `crates/ingot-agent-adapters/src/subprocess.rs` spawns stdout/stderr collection tasks immediately, then waits for them and returns only the final aggregated `SubprocessOutput`.

- Observation: The current subprocess collector is line-based, so “live” delivery still waits for newline boundaries.
  Evidence: `crates/ingot-agent-adapters/src/subprocess.rs` still uses `BufReader::lines()`, and the runtime pump now writes and publishes one chunk per completed line with a trailing newline restored.

## Decision Log

- Decision: Use one shared process-local event bus for both live job log chunks and UI cache-invalidation events.
  Rationale: The daemon and HTTP server live in the same process in `apps/ingot-daemon/src/main.rs`, so a shared in-memory broadcast channel is the simplest long-term design. It avoids polling and keeps the UI on one websocket transport.
  Date/Author: 2026-03-29 / Codex

- Decision: Keep the UI log panel on the existing React Query path and patch `job-logs` cache entries from websocket events instead of introducing a separate log-specific client store.
  Rationale: The Jobs page already reads `GET /api/jobs/:id/logs`. Updating that cache in place preserves the existing page flow, makes reconnect recovery simple, and limits new client state.
  Date/Author: 2026-03-29 / Codex

- Decision: Treat JSON websocket messages as an HTTP-edge representation only and keep runtime-to-router communication strongly typed.
  Rationale: Rust gives better compiler guarantees if the daemon uses enums and typed structs such as `UiEvent`, `JobLogChunkEvent`, and `OutputStream` internally. This avoids stringly protocol drift and makes the websocket serializer a narrow boundary concern instead of a cross-cutting internal format.
  Date/Author: 2026-03-29 / Codex

- Decision: Use `tokio::sync::mpsc` for per-job output chunks and `tokio::sync::broadcast` for daemon-to-UI event fan-out.
  Rationale: Per-job output is a point-to-point stream from the runner into the runtime sink, which matches `mpsc`. Browser subscribers are multi-consumer listeners that should each receive the same UI event stream, which matches `broadcast`.
  Date/Author: 2026-03-29 / Codex

## Outcomes & Retrospective

The implementation now uses a single shared websocket stream for both live `job_log_chunk` events and entity-change invalidation events. The runtime persists stdout and stderr incrementally while a job runs, the HTTP API exposes `/api/ws`, and the UI appends log chunks directly into the existing `job-logs` React Query cache. The remaining session-completion work is administrative: close the tracked issue, push both Git and `bd`, and verify the remote branch state.

## Context and Orientation

The daemon process is started in `apps/ingot-daemon/src/main.rs`. It creates the SQLite database, starts the background `JobDispatcher` from `crates/ingot-agent-runtime`, and builds the Axum HTTP router from `crates/ingot-http-api`.

The runtime job execution loop lives in `crates/ingot-agent-runtime/src/execution.rs`. That module prepares a job, launches an `AgentRunner`, waits for the subprocess to finish, and then persists output artifacts through helper methods defined in `crates/ingot-agent-runtime/src/lib.rs`.

The adapter subprocess helper lives in `crates/ingot-agent-adapters/src/subprocess.rs`. It currently reads stdout and stderr from the child process into memory and returns an `AgentResponse` with complete `stdout` and `stderr` strings only after the process exits.

The HTTP route that serves job logs lives in `crates/ingot-http-api/src/router/jobs.rs`. It reads `prompt.txt`, `stdout.log`, `stderr.log`, and `result.json` from disk and returns them as one JSON response. This route already provides the right recovery surface for refreshes and reconnects once the runtime writes log files incrementally.

The UI websocket client lives in `ui/src/stores/connection.ts`. It currently assumes a `WsEvent` JSON payload and only invalidates React Query caches. The Jobs page in `ui/src/pages/JobsPage.tsx` reads `jobLogsQuery(selectedJobId)` and renders the prompt, stdout, stderr, and result blocks from that query.

In this plan, “runtime boundary” means the handoff between the job dispatcher and the subprocess-backed `AgentRunner`: this is the earliest shared point where output can be persisted and broadcast before the run completes.

In this plan, “typed internal event” means a Rust enum or struct that carries concrete domain identifiers such as `ProjectId` and `JobId`, plus typed metadata such as `OutputStream`, instead of anonymous `serde_json::Value` blobs or raw string tags. The type system should enforce the distinction between stdout and stderr, between entity invalidation and log chunks, and between internal events and their websocket JSON form.

## Plan of Work

First, add a reusable UI event bus in a shared crate that both the runtime and HTTP API can depend on without creating a new dependency cycle. The event bus should assign a monotonically increasing sequence number to each event and expose a `tokio::sync::broadcast` subscription API suitable for one websocket connection per browser tab. Its internal payload should be a typed `UiEvent` enum with at least two variants: a generic entity-change event for query invalidation and a job-log-chunk event for live stdout/stderr updates.

Next, thread that event bus through daemon startup. `apps/ingot-daemon/src/main.rs` should create one event bus instance and pass it to both the `JobDispatcher` and the HTTP router. To minimize churn in tests and call sites, keep the existing convenience constructors and add explicit constructors or wrappers where shared events are needed.

Then, update the runtime execution path so a running agent job can emit output chunks before completion. The `AgentRunner` trait and the CLI adapter path should accept an optional `tokio::sync::mpsc::Sender<AgentOutputChunk>`. `AgentOutputChunk` should be a typed struct containing an `OutputStream` enum (`Stdout` or `Stderr`) and the chunk text. The adapter subprocess collector should forward each chunk as it arrives while still building the final aggregated `AgentResponse`. The dispatcher should create a dedicated `JobLogPump`-style helper that owns append handles, writes chunks to the correct log file as they arrive, and publishes a typed `UiEvent::JobLogChunk` message containing the project id, job id, stream, and text chunk. Keep the final artifact write on completion so non-streaming test runners still produce complete log files.

After that, add a real websocket route to the HTTP API. The route should subscribe to the shared event bus, map each typed internal event into the existing JSON websocket envelope, and forward it over `/api/ws`. For entity-change invalidation, reuse the same flat event envelope that the UI already expects: `seq`, `event`, `project_id`, `entity_type`, `entity_id`, and `payload`. For live logs, add a new websocket event kind such as `job_log_chunk` whose payload includes the typed stream serialized as `stdout` or `stderr`. When activity is appended through the shared helpers, publish a typed entity-change event so the existing cache invalidation behavior finally has a backend source.

Finally, update the UI connection store. Add a stable `jobLogs` query key, handle `job_log_chunk` events by patching the cached log response for that job with `queryClient.setQueryData`, and invalidate all cached job-log queries on a websocket sequence gap. The Jobs page should keep using `useQuery(jobLogsQuery(selectedJobId))`; once the cache is patched in place, the page will re-render live without a dedicated second store.

## Concrete Steps

Work from `/Users/aa/Documents/ingot`.

1. Create the shared event bus module and export it from the chosen crate.
2. Update daemon startup and constructors so one event bus instance is shared between runtime and HTTP server.
3. Extend the runtime and adapter launch path to accept a streaming sender, write incremental stdout/stderr artifacts, and publish live chunk events.
4. Add `/api/ws` and publish entity-change events wherever shared activity append helpers are used.
5. Update the UI websocket client and query keys to patch `job-logs` cache entries from `job_log_chunk` events.
6. Add tests for runtime event emission, websocket delivery, and frontend cache patching.
7. Run the relevant validation commands, update this plan with the results, then complete the issue and push all changes.

## Validation and Acceptance

Acceptance is behavioral:

Run the Rust and UI tests added for this feature and expect them to pass. As implemented, the validated commands are:

    cargo check
    cargo test -p ingot-agent-adapters
    cargo test -p ingot-usecases ui_events::tests::
    cargo test -p ingot-http-api serialize_job_log_chunk_includes_stream_and_text
    cargo test -p ingot-agent-runtime running_job_streams_stdout_to_disk_and_ui_before_completion
    (cd ui && bun x vitest run src/test/connection-store.test.ts)
    (cd ui && bun x biome check src/api/queries.ts src/stores/connection.ts src/test/connection-store.test.ts)

Start the daemon and UI with `make dev`, dispatch a job that produces multiple stdout lines with delays, open the Jobs page, select that running job, and observe that new stdout or stderr text appears before the job reaches `completed`.

Refresh the browser while the job is still running. The log panel should recover the already-written portion of the log from `GET /api/jobs/:id/logs` and then continue appending new websocket chunks as they arrive.

If the websocket connection drops and reconnects after missing messages, the UI should invalidate cached `job-logs` queries and recover the full current log from disk on the next fetch instead of showing a truncated partial buffer.

## Idempotence and Recovery

The event bus is process-local and additive. Restarting the daemon should simply recreate the bus and allow fresh websocket subscriptions; persisted `stdout.log` and `stderr.log` files remain the recovery source for any in-flight or completed job.

Incremental artifact writes are append-only during execution. If the websocket path fails while development is in progress, the existing `GET /api/jobs/:id/logs` route should still return the full current contents of the log files, which makes debugging safe and repeatable.

## Artifacts and Notes

Key source files for this change:

- `apps/ingot-daemon/src/main.rs`
- `crates/ingot-usecases/src/notify.rs` or a sibling shared event module
- `crates/ingot-agent-runtime/src/lib.rs`
- `crates/ingot-agent-runtime/src/execution.rs`
- `crates/ingot-agent-adapters/src/lib.rs`
- `crates/ingot-agent-adapters/src/subprocess.rs`
- `crates/ingot-http-api/src/router/app.rs`
- `crates/ingot-http-api/src/router/core.rs` or a new websocket router module
- `crates/ingot-http-api/src/router/support/activity.rs`
- `ui/src/api/queries.ts`
- `ui/src/stores/connection.ts`
- `ui/src/pages/JobsPage.tsx`

Revision note: created this ExecPlan after confirming that live log streaming requires coordinated changes across runtime, HTTP transport, and UI cache handling rather than a local page-only patch.

## Interfaces and Dependencies

Define the internal event bus in a shared crate with types along these lines:

    pub enum UiEvent {
        EntityChanged(EntityChangedEvent),
        JobLogChunk(JobLogChunkEvent),
    }

    pub struct EntityChangedEvent {
        pub seq: u64,
        pub project_id: ProjectId,
        pub event_type: ActivityEventType,
        pub subject: ActivitySubject,
        pub payload: serde_json::Value,
    }

    pub struct JobLogChunkEvent {
        pub seq: u64,
        pub project_id: ProjectId,
        pub job_id: JobId,
        pub stream: OutputStream,
        pub chunk: String,
    }

    pub enum OutputStream {
        Stdout,
        Stderr,
    }

Use `tokio::sync::broadcast` inside the shared event bus:

    pub struct UiEventBus { ... }

    impl UiEventBus {
        pub fn publish_entity_changed(...) -> UiEvent;
        pub fn publish_job_log_chunk(...) -> UiEvent;
        pub fn subscribe(&self) -> broadcast::Receiver<UiEvent>;
    }

Define the runtime boundary chunk type in the agent protocol or runtime crate:

    pub struct AgentOutputChunk {
        pub stream: OutputStream,
        pub chunk: String,
    }

Update the `AgentRunner` trait so implementations can stream chunks:

    fn launch<'a>(
        &'a self,
        agent: &'a Agent,
        request: &'a AgentRequest,
        working_dir: &'a Path,
        output_tx: Option<mpsc::Sender<AgentOutputChunk>>,
    ) -> AgentLaunchFuture<'a>;

The websocket route should be the only place that turns these typed events into the UI wire format. Keep internal logic on strongly typed IDs, enums, and structs until that final serialization step.

Revision note: revised the plan to rely more heavily on Rust’s type system and explicit `tokio` channel roles instead of loosely typed internal event payloads.

Revision note: updated the plan after implementation to record the shipped typed-event design, the line-buffered streaming limitation, and the exact validation commands that passed.
