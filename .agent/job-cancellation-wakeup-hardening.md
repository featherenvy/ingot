# Job cancellation wakeup hardening

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agent/PLANS.md` and must be maintained in accordance with that file.

## Purpose / Big Picture

After this change, cancelling a running job will stop its long-running process promptly instead of waiting up to the heartbeat interval before the runtime notices. The visible result is that `/cancel` no longer releases a workspace while a daemon validation shell command or agent-backed job can keep mutating files for another five seconds. You will be able to see the fix by running focused runtime tests with the default or a deliberately long heartbeat interval and observing that cancellation still stops output quickly.

## Progress

- [x] (2026-03-19 19:07Z) Re-read `.agent/PLANS.md`, inspected the current daemon-validation cancellation path in `crates/ingot-agent-runtime/src/lib.rs`, and confirmed that cancellation is still observed only on heartbeat ticks.
- [x] (2026-03-19 19:09Z) Inspected adjacent code in `crates/ingot-usecases/src/notify.rs` and `crates/ingot-http-api/src/router/mod.rs`, and confirmed that the repository already has a dispatcher wakeup primitive plus write-request middleware that notifies after successful `/cancel` requests.
- [x] (2026-03-19 19:12Z) Identified a deeper constraint: `DispatchNotify` currently wraps `tokio::sync::Notify`, which wakes only one waiter, so reusing it directly for both the supervisor loop and running job loops would still drop prompt-cancel wakeups.
- [x] (2026-03-19 19:24Z) Deep-read the adjacent runtime, daemon, and test files and corrected the implementation strategy: `apps/ingot-daemon/src/main.rs` is the shared wiring point for the single `DispatchNotify` instance, and an agent-side cancellation regression must observe released capacity or a second launch, not `JobStatus::Cancelled`, because `cancel_job()` writes that status synchronously.
- [x] (2026-03-19 19:23Z) Reworked `crates/ingot-usecases/src/notify.rs` to use a broadcast-safe `watch`-backed listener model with `DispatchNotify::subscribe()` while preserving `DispatchNotify::default()`, `Clone`, `notify()`, and a compatibility `notified()` helper for single-waiter call sites.
- [x] (2026-03-19 19:23Z) Wired persistent notification listeners into `JobDispatcher::run_forever()`, `run_with_heartbeats()`, and `run_harness_command_with_heartbeats()` so cancellation wakeups no longer depend on `heartbeat_interval`.
- [x] (2026-03-19 19:23Z) Updated `run_forever_cancels_daemon_only_validation_command` to use a five-second heartbeat and an explicit `h.dispatch_notify.notify()` after `cancel_job(...)`, matching the production router wakeup path.
- [x] (2026-03-19 19:23Z) Added `run_forever_starts_next_job_after_running_job_cancellation` in `crates/ingot-agent-runtime/tests/dispatch.rs` to prove prompt permit release for cancelled agent-backed work under `max_concurrent_jobs = 1`.
- [x] (2026-03-19 19:23Z) Ran focused runtime and cross-crate validation: `cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_cancels_daemon_only_validation_command -- --exact`, `cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_after_running_job_cancellation -- --exact`, `cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_on_joinset_completion -- --exact`, `cargo test -p ingot-agent-runtime --test dispatch run_forever_refreshes_heartbeat_while_job_is_running -- --exact`, `cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_refreshes_heartbeat_for_daemon_only_validation_job -- --exact`, `cargo test -p ingot-agent-runtime --test auto_dispatch`, `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-http-api --test job_routes cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment -- --exact`, and `cargo check -p ingot-daemon`.
- [x] (2026-03-19 19:26Z) Removed the temporary `DispatchNotify::notified()` compatibility shim so the notifier API now exposes only the broadcast-safe `subscribe()` listener path.

## Surprises & Discoveries

- Observation: the current daemon-validation fix still polls for cancellation on the heartbeat loop, not on a dedicated wakeup.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` waits on `child.wait()`, timeout, and `ticker.tick()` in `run_harness_command_with_heartbeats()`. The `Cancelled` status check sits only inside the ticker branch.

- Observation: the same cancellation-latency pattern already exists in the agent-backed path.
  Evidence: `crates/ingot-agent-runtime/src/lib.rs` checks `JobStatus::Cancelled` only inside the ticker branch of `run_with_heartbeats()`.

- Observation: the repository already emits a dispatcher wakeup after successful write requests, including `/cancel`.
  Evidence: `crates/ingot-http-api/src/router/mod.rs` applies `dispatch_notify_layer`, and that middleware calls `state.dispatch_notify.notify()` on successful `POST`, `PUT`, `PATCH`, and `DELETE` responses.

- Observation: the existing wakeup primitive cannot safely be shared by multiple waiters because it is built on `tokio::sync::Notify`.
  Evidence: `crates/ingot-usecases/src/notify.rs` stores `Arc<Notify>` and exposes only `notify_one()` semantics through `DispatchNotify::notify()`.

- Observation: the current daemon-validation regression masks the production problem by setting a tiny heartbeat interval.
  Evidence: `crates/ingot-agent-runtime/tests/auto_dispatch.rs` configures `heartbeat_interval = Duration::from_millis(20)` in `run_forever_cancels_daemon_only_validation_command`, so it never exercises the default five-second cancellation lag from `DispatcherConfig::new()`.

- Observation: only the runtime currently calls `DispatchNotify::notified()`, but the shared `DispatchNotify` instance is constructed in the daemon binary and passed to both the runtime and HTTP router.
  Evidence: `apps/ingot-daemon/src/main.rs` creates one `DispatchNotify::default()`, clones it into `JobDispatcher::new(...)`, and passes the original into `ingot_http_api::build_router_with_project_locks_and_state_root(...)`.

- Observation: an agent-side cancellation test cannot use `JobStatus::Cancelled` as the promptness signal because the cancellation API writes that status before the runner stops.
  Evidence: `crates/ingot-usecases/src/job_lifecycle.rs` calls `finish_non_success(...)` before returning, and `crates/ingot-agent-runtime/tests/dispatch.rs` already has `BlockingRunner` plus queue-capacity tests that can observe when a cancelled running task actually releases its permit.

- Observation: the runtime heartbeat regression in `crates/ingot-agent-runtime/tests/dispatch.rs` became flaky under the full test binary once the notifier refactor added a new wakeup source, even though the behavior was still correct.
  Evidence: `cargo test -p ingot-agent-runtime --test dispatch` initially timed out in `run_forever_refreshes_heartbeat_while_job_is_running`, but rerunning the exact test with logging showed the heartbeat still refreshed; widening the test’s wait bounds restored stable full-binary coverage.

## Decision Log

- Decision: solve prompt cancellation by reusing the existing dispatcher wakeup channel instead of adding a separate cancellation-only subsystem.
  Rationale: the HTTP layer already wakes the dispatcher after successful cancellation requests, so the missing behavior is multi-waiter delivery and consumption inside running job loops, not the absence of any signal.
  Date/Author: 2026-03-19 / Codex

- Decision: broaden the fix to both long-running execution paths, not only daemon-only validation.
  Rationale: `run_with_heartbeats()` and `run_harness_command_with_heartbeats()` share the same heartbeat-bound cancellation bug, so fixing only one path would leave the same user-visible lag in agent-backed jobs.
  Date/Author: 2026-03-19 / Codex

- Decision: treat `DispatchNotify` as the abstraction boundary and change its internals to support subscriptions or generations for multiple listeners.
  Rationale: the rest of the repository already passes `DispatchNotify` through the daemon and HTTP app state. Upgrading that type is less invasive and more coherent than inventing a second cross-crate signaling object.
  Date/Author: 2026-03-19 / Codex

- Decision: prove agent-side prompt cancellation by observing supervisor capacity release instead of checking job status.
  Rationale: `cancel_job()` sets the database state to `Cancelled` synchronously, so status changes do not prove that the running Tokio task was aborted promptly. A second queued job launching under `max_concurrent_jobs = 1` is an observable signal that the cancelled task released its permit.
  Date/Author: 2026-03-19 / Codex

- Decision: remove `DispatchNotify::notified()` entirely once the runtime migration to `subscribe()` landed cleanly.
  Rationale: the compatibility shim reintroduced a misleading single-waiter-looking API surface even though the repository no longer needed it. Keeping only `subscribe()` makes the multi-listener contract explicit and avoids future misuse.
  Date/Author: 2026-03-19 / Codex

## Outcomes & Retrospective

The implementation landed as planned. `DispatchNotify` is now listener-based and safe for multiple concurrent waiters, `run_forever()` uses a persistent listener instead of repeated single-waiter waits, and both long-running execution loops now react to prompt cancellation notifications without waiting for the next heartbeat tick. The temporary compatibility shim has also been removed, so `subscribe()` is now the only wait-side API. The new long-heartbeat daemon regression and the new agent-capacity regression both pass, and the nearby JoinSet, heartbeat, HTTP cancel-route, and daemon wiring checks remained green after the refactor.

One small adjustment was needed during validation: the existing runtime heartbeat regression in `crates/ingot-agent-runtime/tests/dispatch.rs` had a too-tight two-second window and became flaky under the full test binary after adding the extra notification wakeup path. Widening that test’s wait bounds kept the intended behavior check while making the binary stable again.

## Context and Orientation

`apps/ingot-daemon/src/main.rs` is the wiring point that matters for this bug. It constructs one `DispatchNotify` value, clones it into `JobDispatcher::new(...)`, and passes the same shared value into `ingot_http_api::build_router_with_project_locks_and_state_root(...)`. That shared object is why a successful HTTP `/cancel` request can, in principle, wake the runtime promptly without adding a second signaling subsystem.

`crates/ingot-agent-runtime/src/lib.rs` owns the background dispatcher. `JobDispatcher::run_forever()` drains queued work, launches jobs into a Tokio `JoinSet`, and then sleeps until either a running task finishes, a write request wakes it, or the fallback poll interval elapses. In this file, “heartbeat” means a periodic timer used to refresh the database lease for a running job and to sample whether an operator has cancelled it. The default heartbeat interval is defined by `DispatcherConfig::new()` and is currently five seconds.

Two runtime functions matter for this bug. `run_with_heartbeats()` supervises agent-backed jobs. It spawns the configured agent runner, then waits on the agent task, the job timeout, and the heartbeat ticker. `run_harness_command_with_heartbeats()` does the same for daemon-only validation shell commands. Both functions only check the job row for `JobStatus::Cancelled` inside their heartbeat branch, so cancellation is sampled on a timer instead of being woken immediately.

`crates/ingot-usecases/src/notify.rs` defines `DispatchNotify`, and `crates/ingot-usecases/src/lib.rs` re-exports it for the rest of the workspace. `crates/ingot-http-api/src/lib.rs` then re-exports it again. Today the type wraps `tokio::sync::Notify`, which is safe for one waiter but not for multiple independent listeners that all need to observe the same event. `crates/ingot-http-api/src/router/mod.rs` applies `dispatch_notify_layer` to the entire router, which means a successful `/api/.../cancel` call already triggers `DispatchNotify::notify()`. The missing piece is that the running job loops do not have a broadcast-safe way to listen for that same event without stealing it from the supervisor. Only `crates/ingot-agent-runtime/src/lib.rs` currently calls `.notified()`, so the behavioral refactor is localized even though the type itself is widely constructed.

That “widely constructed” detail matters in practice. `apps/ingot-daemon/src/main.rs`, `crates/ingot-http-api/src/router/test_helpers.rs`, `crates/ingot-http-api/tests/common/mod.rs`, and `crates/ingot-agent-runtime/tests/common/mod.rs` all create `DispatchNotify::default()` values and clone them into routers or dispatchers. The implementation can change internally, but `Default`, `Clone`, and cheap shared-state cloning must remain stable so those call sites do not need unrelated rewrites.

`crates/ingot-http-api/src/router/jobs.rs` implements `cancel_item_job()`. It calls `ingot_usecases::job_lifecycle::cancel_job()` to flip the job state to `Cancelled`, release the workspace, and append activity. The router itself does not manually notify because the middleware does that for all successful write requests. The plan below relies on that existing behavior and therefore must not remove or bypass that middleware wakeup.

`crates/ingot-agent-runtime/tests/common/mod.rs` already contains `BlockingRunner`, which is the shared test double for a long-running agent-backed job. `crates/ingot-agent-runtime/tests/dispatch.rs` already contains the surrounding `run_forever_*` capacity tests plus `stop_background_dispatcher()`, and `crates/ingot-agent-runtime/tests/auto_dispatch.rs` already contains the daemon-only validation cancellation regression. Those existing files and helpers are the right places to prove prompt cancellation after this change; do not invent a parallel test harness unless the current helpers truly block the implementation.

## Milestones

### Milestone 1: Make dispatcher wakeups safe for more than one waiter

At the end of this milestone, the shared `DispatchNotify` type will still be cheap to clone and cheap to call from the HTTP middleware, but it will no longer lose wakeups when both the supervisor and one or more running job loops are listening. The proof for this milestone is that the workspace still compiles and the runtime can switch its wait loops to per-listener state without changing any `DispatchNotify::default()` or clone-based router/dispatcher construction in `apps/ingot-daemon/src/main.rs`, `crates/ingot-http-api/src/router/test_helpers.rs`, `crates/ingot-http-api/tests/common/mod.rs`, or `crates/ingot-agent-runtime/tests/common/mod.rs`.

### Milestone 2: Teach both running-job loops to wake on cancellation notifications

At the end of this milestone, both `run_with_heartbeats()` and `run_harness_command_with_heartbeats()` will react to a successful `/cancel` as soon as the shared notifier fires, instead of waiting for the next heartbeat tick. The heartbeat timer will remain in place only for lease refresh and as a fallback for non-HTTP callers that change state without notifying. The proof for this milestone is that new long-heartbeat regressions fail before the code change and pass after it.

### Milestone 3: Prove prompt cancellation with existing runtime test scaffolding

At the end of this milestone, the daemon-validation regression will use a five-second or longer heartbeat and still stop the shell promptly after `cancel_job(...)` plus `h.dispatch_notify.notify()`. A new agent-backed regression will show that a cancelled running job releases `max_concurrent_jobs = 1` capacity quickly enough for a second queued job to launch before the five-second heartbeat fires. The proof for this milestone is the exact-name test commands in `auto_dispatch.rs` and `dispatch.rs`.

## Plan of Work

Start by changing `crates/ingot-usecases/src/notify.rs` so `DispatchNotify` can support more than one concurrent waiter. The cleanest repository-local shape is to keep `DispatchNotify::notify()` as the single write-side API, but replace the internal `Arc<Notify>` with a broadcast-safe mechanism. A `tokio::sync::watch` channel carrying a monotonically increasing generation number is a good fit because each waiter can own its own receiver and independently observe that a new notification happened. Add a companion listener type, for example `DispatchNotifyListener`, that wraps the per-subscriber receiver and exposes an async `notified()` method. Update the doc comments in `notify.rs` to describe listener-based waiting instead of the current single `DispatchNotify::notified()` wording. Keep `DispatchNotify::default()` and `DispatchNotify::notify()` stable so `apps/ingot-daemon/src/main.rs`, the HTTP router, and the various test harness constructors do not need conceptual changes.

Once `DispatchNotify` can produce independent listeners, update `crates/ingot-agent-runtime/src/lib.rs` to subscribe once for the supervisor loop in `run_forever()`, and separately subscribe inside each long-running execution path. In `run_with_heartbeats()`, add the notification listener as a fourth wakeup source beside agent completion, timeout, and heartbeat. On notification, reload the job row; if the job is cancelled, abort the agent handle immediately and return the same cancellation outcome the function already uses today. If the wakeup came from an unrelated write request, do nothing except continue the loop. Mirror this shape in `run_harness_command_with_heartbeats()`: wait on child completion, timeout, heartbeat, and notification; when a notification arrives, reload the job row and, if cancelled, terminate the shell process group immediately instead of waiting for the next heartbeat.

Keep the heartbeat timer in both loops, but narrow its responsibility to lease refresh and fallback cancellation sampling. This ensures the runtime still behaves correctly for non-HTTP callers that mutate job state without emitting a dispatch notification, while production `/cancel` stops waiting on the five-second timer. Do not “solve” the issue by shrinking `heartbeat_interval`; that would only mask the bug and increase database heartbeat traffic for every running job.

After the runtime code is updated, strengthen the regressions. In `crates/ingot-agent-runtime/tests/auto_dispatch.rs`, keep the existing `run_forever_cancels_daemon_only_validation_command` test name, change it to use `heartbeat_interval = Duration::from_secs(5)` or larger, and call `h.dispatch_notify.notify()` immediately after `job_lifecycle::cancel_job(...)` so the test matches the production router path. The acceptance condition is that the marker file stops growing within a short timeout that is materially smaller than the heartbeat interval. In `crates/ingot-agent-runtime/tests/dispatch.rs`, add a new regression named `run_forever_starts_next_job_after_running_job_cancellation`. Reuse `BlockingRunner`, `create_supervised_authoring_job(...)`, and `stop_background_dispatcher(...)`: create two queued authoring jobs with `max_concurrent_jobs = 1` and `heartbeat_interval = Duration::from_secs(5)`, wait for the first launch, cancel the running job through `job_lifecycle::cancel_job(...)`, emit `h.dispatch_notify.notify()`, and then assert `runner.wait_for_launches(2, Duration::from_millis(500))` succeeds. That second launch is the observable proof that the cancelled running task released its permit before the next heartbeat.

Finally, re-run the focused runtime test binaries and the targeted exact-name regressions. Because the `DispatchNotify` type is created in `apps/ingot-daemon/src/main.rs` and instantiated in several router and test helpers, also run a binary compile check plus the existing runtime exact-name tests that cover supervisor wakeups and heartbeat refresh. Update this ExecPlan’s `Progress`, `Surprises & Discoveries`, and `Outcomes & Retrospective` sections with the concrete commands and any differences between the planned `DispatchNotify` API and the one actually landed.

## Concrete Steps

From `/Users/aa/Documents/ingot`, make the changes in this order so each step stays verifiable.

1. Edit `crates/ingot-usecases/src/notify.rs` to replace the single-waiter `Notify` implementation with a broadcast-safe `DispatchNotify` plus per-listener subscription API, and update the file’s doc comments so they describe the new waiting model correctly.
2. Edit `crates/ingot-agent-runtime/src/lib.rs` to:
   - subscribe once for the supervisor wait loop in `run_forever()`,
   - subscribe inside `run_with_heartbeats()`,
   - subscribe inside `run_harness_command_with_heartbeats()`,
   - handle unrelated notifications by re-checking job state and continuing,
   - preserve heartbeat-based lease refresh as a fallback.
3. Edit `crates/ingot-agent-runtime/tests/auto_dispatch.rs` first, before the runtime change, so `run_forever_cancels_daemon_only_validation_command` uses a five-second heartbeat and an explicit `h.dispatch_notify.notify()` after cancellation. Run the exact-name test and confirm it fails or hangs before the runtime patch. Then implement the runtime fix and rerun it.
4. Edit `crates/ingot-agent-runtime/tests/dispatch.rs` to add `run_forever_starts_next_job_after_running_job_cancellation`, reusing `BlockingRunner`, `create_supervised_authoring_job(...)`, and `stop_background_dispatcher(...)`. Run that exact-name test before the runtime change if you staged it first; it should fail because the second launch cannot happen until the heartbeat. After the runtime change, rerun it and expect success.
5. After the runtime change, rerun the pre-existing exact-name regressions that guard the nearby behavior you are modifying: `run_forever_starts_next_job_on_joinset_completion`, `run_forever_refreshes_heartbeat_while_job_is_running`, and `run_forever_refreshes_heartbeat_for_daemon_only_validation_job`. These are the most direct proof that the new listener model did not break JoinSet wakeups or lease-refresh behavior while you were fixing prompt cancellation.
6. Only touch `crates/ingot-agent-runtime/tests/common/mod.rs` if the cancellation setup becomes awkward in both files. The existing helpers already cover long-running agent execution and job-status polling, so avoid adding a new shared helper unless the duplication is clearly justified.

Run these commands as you validate:

    cd /Users/aa/Documents/ingot
    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_cancels_daemon_only_validation_command -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_after_running_job_cancellation -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_on_joinset_completion -- --exact
    cargo test -p ingot-agent-runtime --test dispatch run_forever_refreshes_heartbeat_while_job_is_running -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_refreshes_heartbeat_for_daemon_only_validation_job -- --exact
    cargo test -p ingot-agent-runtime --test auto_dispatch
    cargo test -p ingot-agent-runtime --test dispatch
    cargo check -p ingot-daemon

Because the `DispatchNotify` type is defined in `ingot-usecases` and shared through the HTTP app state, also run:

    cd /Users/aa/Documents/ingot
    cargo test -p ingot-http-api --test job_routes cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment -- --exact

## Validation and Acceptance

Acceptance is reached when all of the following are true:

1. `run_forever_cancels_daemon_only_validation_command` in `crates/ingot-agent-runtime/tests/auto_dispatch.rs` uses `heartbeat_interval = 5s` or larger, calls `h.dispatch_notify.notify()` after `cancel_job(...)`, and proves the marker file stops growing within a subsecond observation window. That window must be materially smaller than the heartbeat interval so the result cannot be explained by timer polling.
2. `run_forever_starts_next_job_after_running_job_cancellation` in `crates/ingot-agent-runtime/tests/dispatch.rs` proves a cancelled running agent job releases `max_concurrent_jobs = 1` capacity quickly enough for a second queued job to launch within a subsecond timeout. The test must not use `JobStatus::Cancelled` as its promptness signal.
3. Both execution loops still refresh heartbeats on their existing timers. Prove this by keeping `run_forever_refreshes_heartbeat_while_job_is_running` and `run_forever_refreshes_heartbeat_for_daemon_only_validation_job` passing after the notification refactor.
4. The supervisor loop still wakes correctly after successful write requests and JoinSet completions. Prove this by keeping `run_forever_starts_next_job_on_joinset_completion` passing after `run_forever()` switches from direct `DispatchNotify::notified()` waits to a persistent listener.
5. Unrelated `DispatchNotify` wakeups only trigger a state re-check and then continue running; they do not cancel or time out active work spuriously. The exact-name cancellation regressions should be the only tests whose outcome changes across the patch.
6. `cargo test -p ingot-agent-runtime --test auto_dispatch`, `cargo test -p ingot-agent-runtime --test dispatch`, `cargo test -p ingot-http-api --test job_routes cancel_route_marks_active_job_cancelled_and_clears_workspace_attachment -- --exact`, and `cargo check -p ingot-daemon` all pass after the patch.

The exact-name regression tests must fail before the implementation if run with a long heartbeat and pass after the implementation. That “before/after” proof is important because the whole point of this plan is to remove the heartbeat-bound cancellation lag, not merely reshuffle code.

## Idempotence and Recovery

These changes are safe to repeat because they are confined to the in-memory notification primitive, runtime wait loops, and temp-directory-backed tests. No schema migration or persistent data rewrite is involved. If a partial implementation leaves tests failing, revert only the in-progress tracked edits in the affected Rust files and rerun the exact-name tests above. Do not use destructive git resets in a dirty worktree; make small commits or use targeted patch reverts instead.

Because `DispatchNotify` is shared across crates, an API refactor can break compilation in `apps/ingot-daemon`, `ingot-http-api`, or runtime tests before behavior is correct. Treat that as an expected intermediate state. Re-run the exact-name runtime tests after each compile fix rather than trying to jump directly to the full suite.

If you adopt a listener type returned from `DispatchNotify::subscribe()`, decide explicitly whether that type needs a crate-root re-export. The runtime can usually use `let mut listener = self.dispatch_notify.subscribe();` without naming the type, so widening the public API may be unnecessary. Prefer the narrower API surface unless a compile error proves otherwise.

## Artifacts and Notes

When implementation is complete, record the most useful evidence here. At minimum include:

    cargo test -p ingot-agent-runtime --test auto_dispatch run_forever_cancels_daemon_only_validation_command -- --exact
    test run_forever_cancels_daemon_only_validation_command ... ok

    cargo test -p ingot-agent-runtime --test dispatch run_forever_starts_next_job_after_running_job_cancellation -- --exact
    test run_forever_starts_next_job_after_running_job_cancellation ... ok

    cargo check -p ingot-daemon
    Finished `dev` profile ... target(s) in ...

If the final implementation uses a different internal name than `DispatchNotifyListener`, update the `Interfaces and Dependencies` section and add a note here explaining the divergence.

## Interfaces and Dependencies

At the end of this work, `crates/ingot-usecases/src/notify.rs` should expose a broadcast-safe notification API. One acceptable target shape is:

    #[derive(Clone)]
    pub struct DispatchNotify { ... }

    impl DispatchNotify {
        pub fn new() -> Self;
        pub fn notify(&self);
        pub fn subscribe(&self) -> DispatchNotifyListener;
    }

    pub struct DispatchNotifyListener { ... }

    impl DispatchNotifyListener {
        pub async fn notified(&mut self);
    }

`DispatchNotify::notify()` must remain cheap to call from HTTP middleware, and `DispatchNotifyListener::notified()` must allow more than one waiter to observe the same event independently. The implementation may use `tokio::sync::watch`, a generation counter, or another repository-local approach with the same semantics, but it must not rely on `Notify::notify_one()` alone. The public wait-side API should stay narrow: `subscribe()` for creating listeners and `DispatchNotifyListener::notified()` for awaiting the next wakeup.

In `crates/ingot-agent-runtime/src/lib.rs`, both `run_with_heartbeats()` and `run_harness_command_with_heartbeats()` must gain a notification-based wakeup branch in their `tokio::select!` loops. The heartbeat branch must remain responsible for `heartbeat_job_execution(...)`. The notification branch must only re-check job state and handle cancellation; it must not emit extra heartbeats. `run_forever()` must also move from calling `self.dispatch_notify.notified()` directly to using a persistent listener created before the outer loop, otherwise the shared notification semantics remain split between two APIs.

No new external dependency should be added unless the existing Tokio primitives cannot express the required broadcast semantics cleanly. Prefer keeping the change inside `tokio` and the repository’s existing types. Because `apps/ingot-daemon/src/main.rs`, `crates/ingot-http-api/src/router/test_helpers.rs`, `crates/ingot-http-api/tests/common/mod.rs`, and `crates/ingot-agent-runtime/tests/common/mod.rs` all construct or clone `DispatchNotify`, the type must stay `Clone`, keep `Default`, and preserve shared-state semantics across clones.

Revision note: created on 2026-03-19 to address the remaining heartbeat-bound cancellation lag after the initial daemon-validation cancellation patch. Research for this plan showed that the true core issue is the single-waiter `DispatchNotify` design, so the plan broadens the fix to the shared notification primitive and both running-job execution paths.

Revision note (2026-03-19, improvement pass): added the missing shared wiring context from `apps/ingot-daemon/src/main.rs`, corrected the HTTP test command syntax, replaced the non-observable agent-cancellation assertion strategy with a concrete “second queued job launches before the next heartbeat” regression, and tightened the validation steps around the existing runtime test helpers and exact-name tests.

Revision note (2026-03-19, second improvement pass): added the remaining constructor call sites that constrain the `DispatchNotify` API (`Default` and `Clone` must stay stable), and expanded validation to explicitly rerun the existing JoinSet and heartbeat regressions that are most likely to break when `run_forever()` and the long-running job loops move to per-listener notification wakeups.

Revision note (2026-03-19, implementation pass): landed the `watch`-backed `DispatchNotify` listener model, moved the runtime wait loops to persistent listeners, added the long-heartbeat daemon cancellation regression plus the agent permit-release regression, and recorded the exact passing validation commands. Also widened the existing runtime heartbeat regression timeout after the notifier refactor exposed a full-binary flake without changing the underlying behavior.

Revision note (2026-03-19, shim-removal pass): removed the temporary `DispatchNotify::notified()` compatibility helper after confirming the runtime had fully migrated to `subscribe()`, so the notifier API now exposes only the broadcast-safe listener path.
