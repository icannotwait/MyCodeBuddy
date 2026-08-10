# Task 8 Report: Backend Rollout, Settings, And Obsolete Metrics Removal

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX AND GROK REVIEW PENDING**

- Work unit: `task|8|implementer|codex|none`
- Scope: Completion Protocol V2-Only plan Task 8 only
- Baseline HEAD: `dda2f4f4ee7fc840d1a2803d8550adfb434686f2`
- Producer commit: `1f8da1184a59f985ea510576430952be7f997a8f`
- Task 9 frontend work: not started

## Implementation

- Deleted rollout configuration, selection/source types, profile-key parsing,
  rollout window decisions, and the rollout-only listener/manager constructors.
- Removed rollout state from desktop, server, embedded web, `AppState`, and
  test bootstrap paths.
- Removed the Tauri command, Axum handler, and HTTP route for completion
  protocol settings.
- Removed creation-mode, shadow comparison, rollout-window/decision, and
  restart-outcome telemetry, including their producers and serialized fields.
- Removed shadow comparison execution and its obsolete tests.
- Retained the fixed v2 identity and Task 2 startup rejection based solely on
  removed environment-variable presence.
- Retained completion intent-source, resolution/outcome, evidence/decision,
  attention/outbox, artifact-recovery, continuation, and final-state metrics.
- Retained `CompletionRootWakeQueue`, attention outbox replay, and automatic
  root wake for a valid persisted v2 workflow.
- Did not modify frontend code or historical protocol read behavior.

## TDD Evidence

The new transport absence test failed before implementation because
`POST /api/get_completion_protocol_settings` returned `200` with rollout data.
After route removal it reaches the repository's authenticated unknown-command
fallback and returns typed `501 not_implemented`; the Tauri registration is
also absent.

The new metrics JSON test failed before implementation because
`creation_modes` remained serialized. After cleanup, all obsolete metric
families are absent while the retained v2 metric families and representative
counters remain present.

The positive replay regression persists a real `(2, v2_enforce)` workflow,
inserts a typed completion-resolution outbox row, drains it through a recording
`CompletionRootWakeQueue`, and observes exactly one root wake with one dispatch
attempt and non-null delivery acknowledgement.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils completion_rollout_surface_is_absent`
  - Pass: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils completion_metrics_v2_only`
  - Pass: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils valid_v2_attention_outbox_replay_wakes_root_once_and_acknowledges_delivery`
  - Pass: 1 passed, 0 failed.
- Focused retained metrics integration and listener fixed-v2 store-guard tests
  - Pass: 2 passed, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server`
  - Pass.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp`
  - Pass.
- Plan-exact forbidden-symbol search
  - Pass: no rollout, settings, shadow, or restart metric symbols remain in
    `src-tauri/src` or `src-tauri/tests`.
- Retention search
  - Pass: root-wake and v2 intent, decision/evidence, attention/outbox,
    artifact, continuation, and typed outcome metrics remain.
- `git diff --check`, producer allowlist, and cached diff checks
  - Pass: producer commit contains exactly 14 declared Task 8 files.

The desktop build continues to emit the existing zero-byte `codeg-mcp` sidecar
packaging warning. It is outside this producer diff. The full Rust suite and
Clippy matrix are reserved for later plan tasks; Task 8 ran its specified
focused tests and three runtime checks.

## Producer Commit

- `1f8da1184a59f985ea510576430952be7f997a8f` -
  `refactor: remove completion protocol rollout state`

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Removed backend completion rollout/settings state plus shadow/restart telemetry while retaining fixed-v2 intent, evidence, attention, artifact, typed outcome metrics, env rejection, and root-wake replay.","commits":[{"sha":"1f8da1184a59f985ea510576430952be7f997a8f","subject":"refactor: remove completion protocol rollout state"}],"tests":{"status":"passed","passed":5,"failed":0,"summary":"Five focused transport, metrics, listener, and root-wake regressions plus desktop, server, and codeg-mcp checks passed; removal and retention gates were clean."},"concerns":["The existing zero-byte codeg-mcp sidecar packaging warning remains outside this diff.","Independent Codex and Grok review is pending before Task 9."],"report_file":".superpowers/sdd/task-8-report.md"}
-->
