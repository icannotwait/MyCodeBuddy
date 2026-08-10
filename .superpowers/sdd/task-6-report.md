# Task 6 Report: Remove Legacy Restart Writers

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX/GROK REVIEW PENDING**

- Work unit: `task|6|implementer|codex|none`
- Scope: Completion Protocol V2-Only plan Task 6 only
- Baseline HEAD: `459da9d02c85db04bbe85a2555a9147f85963bd9`
- Producer commit: `83c27aa13a4e83383b1cfa28d615210e90e44cda`
- Task 7+: not started

## Implementation

- Removed the legacy restart writer, successor creation transaction, original
  request capture/backfill writers, parser fallback, and automatic
  pre-delegation restart checks.
- Removed restart MCP schema/dispatch, broker transport/listener variants,
  Tauri command registration, Axum route/handler, request/response DTOs, and
  restart-only ACP/application/workflow errors.
- Reduced `workflow_restart.rs` to historical relationship projection and a
  read-only restart-context loader. No code writes the historical context table.
- Preserved `legacy_source_workflow_id`, the historical restart migration/table
  and entity, existing predecessor/successor links, and context-row reads.
- Preserved projection fields `version`, `mode`, required `creation_mode`,
  `legacy_source`, `v2_successor`, `read_only_reason`, and
  `automatic_root_wake`. `creation_mode` is derived from and equals persisted
  `mode`; every version-1 header projects
  `legacy_completion_protocol_read_only` regardless of link presence.
- Kept `automatic_root_wake` false in this projection, matching its existing
  behavior; v2 attention-driven wake remains owned by the completion outbox.
- Kept exactly five `WORKFLOW_V2_TOOLS` and the shared root coordination tool
  `request_recovery_authorization`.
- Removed the now-orphaned private `parser_for_agent` helper; its sole consumer
  was the deleted historical context backfill writer.

`project.rs` required no textual edit: its existing projection call now uses
the reduced read-only helper. No historical database migration or table was
modified or deleted.

## TDD Evidence

Before production deletion, the new tests failed because:

- `restart_legacy_workflow` was still present in public/backend surfaces.
- Historical projection returned
  `legacy_completion_protocol_restart_required` instead of the immutable
  read-only reason.
- `WORKFLOW_V2_TOOLS` still contained six tools.

After implementation, the historical projection regression covers both
persisted `v1` and `v2_shadow` rows. It verifies exact stored Card JSON byte
preservation after projection, displayed Card summary, both relationship
directions, required/equal `creation_mode`, immutable read-only reason,
`automatic_root_wake`, and historical context-row reads.

## Verification

All commands below were rerun after the final source-format adjustment.

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils legacy_restart_surface_is_absent`
  - Pass: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_projection`
  - Pass: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_v2_root_catalog_agrees_with_local_capabilities`
  - Pass: 1 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_root_resume_is_rejected_before_side_effects`
  - Pass: 1 passed, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server`
  - Pass.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp`
  - Pass.
- Strict forbidden-symbol search over `src-tauri/src` and `src-tauri/tests`
  - Pass: no restart writer/API/error/payload symbols remain.
- Retained read/tool searches
  - Pass: historical table/relationship/context references, exact five workflow
    tools, and recovery authorization remain.
- Restart-context writer/backfill search
  - Pass: no production writer/backfill references remain.
- `rustfmt --edition 2021 --check` over Task 6 modified Rust files except
  `lib.rs`
  - Pass. The Task 6 `lib.rs` change is a one-line Tauri registration deletion.
- `git diff --check` and `git diff --cached --check`
  - Pass before the producer commit.

## Concerns

- Full-repository `cargo fmt --check` remains red on pre-existing unrelated
  formatting in `connection.rs`, `launch_snapshot.rs`,
  `document_translate/service.rs`, `lib.rs`, and `window_diagnostics.rs`. Those
  unrelated files/hunks were not reformatted; scoped Task 6 formatting passed.
- Desktop test/check commands emit the existing warning that the ignored
  `codeg-mcp` sidecar is a zero-byte placeholder. It is not part of the diff.
- Independent Codex and Grok review is pending before Task 7 admission.

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Removed legacy restart writers and public dispatch/error surfaces while preserving v1/v2_shadow history, exact Card bytes, relationship/context reads, five workflow tools, and recovery authorization.","commits":[{"sha":"83c27aa13a4e83383b1cfa28d615210e90e44cda","subject":"refactor: remove legacy workflow restart writes"}],"tests":{"status":"passed","passed":4,"failed":0,"summary":"Four focused regressions and desktop/server/codeg-mcp checks passed, along with strict absence/retention, scoped rustfmt, and diff gates."},"concerns":["Full cargo fmt --check finds pre-existing unrelated formatting outside the Task 6 changes; scoped Task 6 rustfmt passed.","Desktop checks emit the existing zero-byte codeg-mcp sidecar warning.","Independent Codex and Grok review is pending before Task 7."],"report_file":".superpowers/sdd/task-6-report.md"}
-->
