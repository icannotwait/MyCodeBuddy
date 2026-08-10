# Task 1 Code Review Report (Codex)

## Review Identity

- `reviewed_task_id`: `b954d688-e237-493d-b1e0-31df91323c1b`
- Producer commit: `017954713566ddcbfd274f099055ddce022e2d01`
- Review base: `190c1e141de84460c01130b4da42713a14362759`
- Scope: Plan Task 1 and the approved Completion Protocol V2-Only Design
- Reviewer role: independent high-risk Codex reviewer

## Verdict

`approve`

## Findings

- Critical: none.
- Important: none.
- Minor: none.

## Review Summary

The producer commit implements the Task 1 contract without starting later
tasks. The workflow module exposes the exact fixed version and mode identity,
and `require_v2_mutation` accepts only `(2, v2_enforce)`. Every version-1 pair
maps to `legacy_completion_protocol_read_only`; all remaining tested pairs map
to `unsupported_completion_protocol`; both rejection classes are
non-retryable.

The four post-change error codes are represented as typed ACP/app errors and
preserved structurally rather than by message inspection. ACP serialization,
listener workflow-error JSON, snake_case app serialization, and HTTP status
mapping match the Plan: protocol read-only, unsupported protocol, and
instruction binding failures use HTTP 409, while removed configuration uses
HTTP 400. Existing rollout and restart-family definitions remain available as
Task 1 explicitly requires until their later removal tasks.

The review range contains one producer commit. Its only extra file beyond the
Task 1 staging list is `listener.rs`, where the two new exhaustive
`WorkflowStoreError` cases preserve the stable MCP wire codes; this is a
necessary compile and public-boundary update within Task 1 scope.

## Verification Evidence

- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils require_v2_mutation`
  passed: 1 test, 0 failures.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils stable_protocol_error_codes`
  passed: 1 test, 0 failures.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils stable_completion_protocol`
  passed: 2 tests, 0 failures.
- `cargo check --manifest-path src-tauri/Cargo.toml` passed.
- `git diff 01795471^ 01795471 --check` passed.

Cargo emitted the existing warning that the local `codeg-mcp` sidecar is a
zero-byte placeholder; it did not affect the focused tests or desktop check
and is outside the producer diff.

Conclusion: approve

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"b954d688-e237-493d-b1e0-31df91323c1b","producer_commit":"017954713566ddcbfd274f099055ddce022e2d01","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 1 exactly defines the fixed v2 identity, exhaustive non-retryable pair guard, and stable typed public error mappings; focused tests and desktop cargo check pass.","report_file":".superpowers/sdd/task-1-review-codex-report.md"}
-->
