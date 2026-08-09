# Task 8 Independent Codex Review

## Findings

### Minor: T8-CODEX-M1 - Retained v2 metrics are still labeled as shadow code

`src-tauri/src/acp/delegation/broker.rs:31723` still names its retained resolver
metrics regression `shadow_completion_observation_records_bounded_resolver_metrics`,
and `src-tauri/src/acp/delegation/run_store.rs:2100` still describes
`terminal_completion_resolver_context` as a "metrics-only v2 shadow resolver."
The comparator, its invocation, and all shadow metric state are removed, so this
is not residual shadow execution. However, the stale symbol/comment conflicts
with Task 8's full shadow-concept removal and the producer report's claim that
no shadow symbols remain. Rename both references to describe retained v2
completion metrics.

Critical: 0. Important: 0. Minor: 1.

## Verdict

`approve_with_minors`

## Review Identity

- `reviewed_task_id`: `65627b1a-e054-4f8d-ac38-a28670518e8a`
- Producer commit: `1f8da1184a59f985ea510576430952be7f997a8f`
- Reviewed HEAD/report commit: `fc302e8f697416ba5f1ca60116592f22630aee7a`
- Scope: Plan Task 8, independent HIGH review only; no production changes

## Contract Review

- The plan-exact forbidden-symbol gate has no matches under `src-tauri/src` or
  `src-tauri/tests`. Rollout configuration/selection/profile parsing, settings
  state and APIs, shadow comparison execution and fields, rollout windows and
  decisions, and restart-outcome telemetry are removed.
- The authenticated Axum settings endpoint now reaches the typed unknown-command
  fallback (`501 not_implemented`), and the matching Tauri command is absent
  from registration. Desktop, server, embedded-web, listener, manager, and test
  state no longer carry rollout configuration.
- The metrics snapshot omits `default_mode`, `profile_overrides`,
  `creation_modes`, `shadow_differences`, `rollout_windows`,
  `rollout_decisions`, and `restart_outcomes`. Retained v2 resolution/tool,
  intent/decision, evidence/artifact/scope, attention/outbox, continuation, and
  final-state producers remain wired.
- The broker no longer compares a legacy Card outcome with v2 resolution or
  records shadow samples. Historical `V2Shadow` values remain only where needed
  for persisted read/error fixtures, as required by the design.
- `CompletionRootWakeQueue` and the outbox dispatch hook remain. The positive
  regression persists a fixed-v2 workflow and typed completion-resolution
  event, invokes the recording queue exactly once, increments dispatch attempts
  once, and records delivery acknowledgement.
- The producer commit changes only backend Rust and backend tests. No Task 9
  frontend file is included.

## Verification Evidence

Fresh verification at reviewed HEAD:

- `completion_rollout_surface_is_absent`: 1 passed, 0 failed.
- `completion_metrics_v2_only`: 1 passed, 0 failed.
- `valid_v2_attention_outbox_replay_wakes_root_once_and_acknowledges_delivery`:
  1 passed, 0 failed.
- `completion_protocol_metrics_` retained-metrics filter: 2 passed, 0 failed.
- Desktop, server, and `codeg-mcp` `cargo check` commands: all exited 0.
- Plan-exact forbidden-symbol gate, producer `git diff --check`, commit scope,
  and frontend-diff audit: clean.

The desktop commands emitted the existing zero-byte `codeg-mcp` sidecar
warning. It is outside the producer diff and did not affect verification.

Conclusion: approve_with_minors

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"65627b1a-e054-4f8d-ac38-a28670518e8a","producer_commit":"1f8da1184a59f985ea510576430952be7f997a8f","verdict":"approve_with_minors","critical":0,"important":0,"minor":1,"summary":"Task 8 removes backend rollout/settings and shadow/restart metrics, retains v2 metrics and root-wake replay, and leaves one nonblocking stale shadow test/comment label; focused tests and runtime checks pass.","report_file":".superpowers/sdd/task-8-review-codex-report.md"}
-->
