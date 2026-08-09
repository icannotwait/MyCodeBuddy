# Task 6 Independent Codex Review

## Review Identity

- Reviewed task ID: `1d31bab1-5b08-42cc-8f85-4903a239a65b`
- Producer commit: `83c27aa13a4e83383b1cfa28d615210e90e44cda`
- Report HEAD: `6f88aa6ab8319eb38d99baca38e1f2cf2f4636d9`
- Plan: `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md`, Task 6
- Review mode: independent HIGH task review; no production changes

## Verdict

**approve**

Critical: 0. Important: 0. Minor: 0.

## Findings

No findings.

## Contract Review

- The Task 6 forbidden symbol set has no matches under `src-tauri/src` or
  `src-tauri/tests`. The MCP catalog/dispatcher, broker transport/listener,
  Tauri registration, Axum route/handler, restart DTOs, successor payload, and
  restart-family error variants/mappings are removed.
- No production restart-context or successor-link writer remains. The old
  restart-context migration/entity, existing `legacy_source_workflow_id`
  column, relationship queries, and historical context-row read remain.
- Historical `(1, v1)` and `(1, v2_shadow)` projection preserves persisted
  `version` and `mode`; `creation_mode` is required and equals `mode`;
  `read_only_reason` is `legacy_completion_protocol_read_only`;
  `automatic_root_wake` is false; and both `legacy_source` and `v2_successor`
  directions remain readable.
- The historical projection regression parses the stored Card for display and
  then verifies the original whitespace-sensitive Card JSON string remains
  byte-for-byte unchanged in `card_summary_json`.
- `WORKFLOW_V2_TOOLS` is exactly the required five-tool set, and
  `request_recovery_authorization` remains advertised as the shared root
  coordination tool.
- The producer diff contains no database migration/entity changes and no Task 7
  trigger work. The additional `parsers/mod.rs` deletion removes the orphaned
  parser factory whose only consumer was the deleted restart-context backfill,
  so it remains within Task 6 behavior.
- Frontend restart surfaces and restart/rollout metrics were not treated as
  Task 6 findings because the reviewed plan explicitly assigns them to Tasks 9
  and 8 respectively.

## Verification Evidence

All commands below were run against HEAD `6f88aa6ab8319eb38d99baca38e1f2cf2f4636d9`:

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils legacy_restart_surface_is_absent`
  - Passed: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_projection`
  - Passed: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_v2_root_catalog_agrees_with_local_capabilities`
  - Passed: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_root_resume_is_rejected_before_side_effects`
  - Passed: 1 passed, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Passed.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server`
  - Passed.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp`
  - Passed.
- Strict forbidden/retained symbol searches, producer diff scope checks,
  no-writer searches, and `git diff --check`
  - Passed.

The desktop commands emitted the pre-existing warning that the ignored
`codeg-mcp` sidecar is a zero-byte placeholder. It is outside the reviewed
diff. Full repository test and Clippy suites were not rerun for this focused
Task 6 review.

## Review Card

```json
{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 6 removes backend restart writers/surfaces and preserves v1/v2_shadow projection, Card bytes, historical reads, five workflow tools, and recovery authorization; focused tests and runtime checks pass.","report_file":".superpowers/sdd/task-6-review-codex-report.md"}
```
