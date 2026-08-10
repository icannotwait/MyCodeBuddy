# Task 6 Review — Grok (HIGH dual reviewer)

- **Work unit:** Independent Task 6 HIGH reviewer (Grok)
- **reviewed_task_id:** `1d31bab1-5b08-42cc-8f85-4903a239a65b`
- **Producer code commit:** `83c27aa13a4e83383b1cfa28d615210e90e44cda`
- **HEAD with report:** `6f88aa6ab8319eb38d99baca38e1f2cf2f4636d9`
- **Plan:** `docs/superpowers/plans/2026-08-09-completion-protocol-v2-only.md` — Task 6
- **Implementer report:** `.superpowers\sdd\task-6-report.md`
- **Reviewer:** Grok
- **Mode:** code review only (no implementation)

## Verdict

**`approve_with_minors`**

Task 6 removes every backend restart writer, dispatcher, transport/wire variant, Tauri/Axum/MCP surface, and restart-only error/payload family, while reducing `workflow_restart.rs` to historical relationship/context reads. Historical projection for persisted `v1` and `v2_shadow` now always emits immutable `legacy_completion_protocol_read_only` with `creation_mode == mode`, navigable links, exact Card bytes, and context-row reads. Exactly five `WORKFLOW_V2_TOOLS` remain, with `request_recovery_authorization` retained. No Task 7 migration/trigger work landed.

Residual items are non-blocking test-strength notes and intentionally deferred Task 8/9 surfaces. Nothing reopens the Task 6 backend deletion contract.

## Spec compliance (Task 6 only)

| Requirement | Status | Evidence |
| --- | --- | --- |
| Delete `restart_legacy_workflow` and writer helpers | Pass | Writers, successor creation, capture/backfill, and `restart_legacy_workflow_if_enforced` removed; `workflow_restart.rs` is read-only projection + context loader |
| Delete MCP schema/dispatch, broker transport, listener branch | Pass | Tool schema entry gone; `BrokerRestartLegacyWorkflowRequest` / `BrokerMessage::RestartLegacyWorkflow` / companion dispatch / listener process path deleted |
| Delete Tauri command, Axum route/handler, DTOs | Pass | `commands/workflow_completion.rs`, `lib.rs` registration, web handler/route, `RestartLegacyWorkflowRequest` removed |
| Delete restart error family / wire payloads | Pass | `LegacyCompletionProtocolRestart*`, `AcpError::LegacyCompletionProtocolRestart`, `AppErrorCode::LegacyCompletionProtocolRestartRequired`, `LegacyWorkflowRestartProjection/Context`, `successor_conversation_id` error detail gone |
| Preserve historical migration/table/entity/column | Pass | `m20260806_000004_legacy_restart_context`, entity, `legacy_source_workflow_id` retained; no migration deleted |
| Projection: version/mode/`creation_mode` equal mode | Pass | `completion_protocol_projection` clones mode into `creation_mode`; test asserts for `V1` and `V2Shadow` |
| Projection: immutable `legacy_completion_protocol_read_only` on version 1 | Pass | Reason keyed on `completion_protocol_version == 1`, not successor presence (old link-gated `restart_required` reason removed) |
| Projection: both relationship directions | Pass | Source → `v2_successor`; successor → `legacy_source` |
| Projection: `automatic_root_wake` false here | Pass | Hardcoded `false`; matches pre-Task-6 projection and completion-outbox ownership of v2 wake |
| Card bytes + context reads retained | Pass | Historical test seeds context row + Card JSON and asserts exact stored bytes + display summary |
| Five workflow tools + recovery authorization | Pass | `WORKFLOW_V2_TOOLS` length 5 exact list; catalog test asserts recovery tool remains |
| No Task 7 scope creep | Pass | No `m20260809*`, no `trg_delegation_workflows_*`, no freeze/insert trigger helpers |
| Strict absence search over `src-tauri` | Pass | Forbidden symbols: no matches in `src-tauri/src` and `src-tauri/tests` |

### Deletion / retention map

```text
REMOVED (backend writers/wire/API/errors)
  restart_legacy_workflow(_core|_if_enforced|_authenticated_core)
  capture_original_request_context + manager/broker call sites
  automatic listener restart_legacy_if_required / process_restart_legacy_workflow
  BrokerRestartLegacyWorkflowRequest + transport round-trip
  tool_schema restart_legacy_workflow
  Tauri command + Axum /restart_legacy_workflow
  LegacyWorkflowRestartProjection/Context DTOs
  LegacyCompletionProtocolRestartRequired/Invalid (+ not_required detail)
  AcpError::LegacyCompletionProtocolRestart
  AppErrorCode::LegacyCompletionProtocolRestartRequired
  CompletionProtocolSelectionSource::LegacyRestart
  orphaned parsers::parser_for_agent (sole consumer was deleted backfill)

RETAINED (historical reads)
  migration + table delegation_workflow_restart_contexts
  entity + load_historical_workflow_context
  column legacy_source_workflow_id
  CompletionProtocolWorkflowProjection fields
  workflow_restart::completion_protocol_projection
    version/mode/creation_mode(=mode)
    legacy_source / v2_successor links
    read_only_reason := version==1 ? legacy_completion_protocol_read_only
    automatic_root_wake := false

RETAINED (root catalog)
  WORKFLOW_V2_TOOLS = 5 tools (no restart)
  request_recovery_authorization (shared root/coordination)

DEFERRED BY PLAN (not Task 6 defects)
  Task 8: CompletionRestartOutcome / record_completion_restart / restart_outcomes metrics
  Task 9: frontend restartLegacyWorkflow / types / web-transport / graph-panel button
```

## Independent verification

Re-ran on this worktree at HEAD `6f88aa6a` (producer `83c27aa1` + report tip):

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils legacy_restart_surface_is_absent -- --exact` | **pass** (1) |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_projection -- --exact` | **pass** (1) |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_v2_root_catalog -- --nocapture` | **pass** (`workflow_v2_root_catalog_agrees_with_local_capabilities`) |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_root_resume_is_rejected_before_side_effects -- --exact` | **pass** (1) |

Static audit:

| Check | Result |
| --- | --- |
| Forbidden restart writer/API/error/payload symbols in `src-tauri/src` + `src-tauri/tests` | **no matches** |
| Retained historical table/column/link reads | **present** (`workflow_restart.rs`, entity, migrations) |
| Exact five `WORKFLOW_V2_TOOLS` + `request_recovery_authorization` | **present** |
| Task 7 trigger/migration symbols | **absent** |
| Producer commit file set vs Task 6 plan | **in scope**; extra `parsers/mod.rs` + `acp/delegation/types.rs` are justified delete-only cleanups of orphaned restart consumers/DTOs |

## Strengths

1. Clean net deletion (~1.7k LOC removed) with a tiny, readable historical projection module.
2. Correct behavioral flip of `read_only_reason` from successor-gated `restart_required` to immutable version-1 `read_only`, which is the Task 6 product contract.
3. Multi-surface absence coverage (MCP/schema, companion, transport, listener, broker, commands, router, error enums) plus a focused historical projection regression for both `v1` and `v2_shadow`.
4. Catalog protection prevents restart removal from silently collapsing the five-tool root set or dropping recovery authorization.
5. No database schema destruction and no Task 7 trigger work.

## Findings

| id | severity | title | blocking |
| --- | --- | --- | --- |
| T6-GROK-M1 | Minor | `legacy_restart_surface_is_absent` source list omits `workflow_restart.rs`; a future reintroduction of writers there would not be caught by that include-list test alone (strict `rg` still covers it) | no |
| T6-GROK-M2 | Minor | `historical_protocol_projection` always seeds a successor link, so “version-1 read-only regardless of link presence” is proven by implementation review rather than an explicit no-link case | no |

No Critical or Important findings.

### Deferred surfaces (documented, not findings against Task 6)

- **Task 8:** `CompletionRestartOutcome`, `record_completion_restart`, and metrics snapshot `restart_outcomes` remain with no production call sites after Task 6; plan Task 8 owns their removal.
- **Task 9:** Frontend `restartLegacyWorkflow`, `LegacyWorkflowRestartProjection`, web-transport mapping, and graph-panel restart control still exist and will 404/fail against the deleted backend until Task 9; plan assigns that public UI/API cleanup explicitly to Task 9.

## Scope notes

- Producer commit `83c27aa1` is pure Task 6 backend deletion + tests; tip `6f88aa6a` only adds `.superpowers/sdd/task-6-report.md`.
- `project.rs` required no textual edit because it already calls `workflow_restart::completion_protocol_projection`.
- Report concern about full-repo `cargo fmt --check` pre-existing reds is out of Task 6 scope; scoped producer formatting claim is accepted without reopening backend behavior.

## Conclusion

Task 6 backend contract is met. Admit Task 7 after dual review agreement; do not expand this task into frontend (Task 9) or metrics (Task 8) cleanup.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve_with_minors","summary":"Task 6 backend restart writers/wire/API/errors removed; v1/v2_shadow historical projection, Card/context/link reads, five tools + recovery auth retained. Two minor test gaps; FE/metrics deferred by plan.","commits":[{"sha":"83c27aa13a4e83383b1cfa28d615210e90e44cda","subject":"refactor: remove legacy workflow restart writes"}],"tests":{"status":"passed","passed":4,"failed":0,"summary":"Re-ran absence, historical projection, root catalog, and historical root-resume tests; all passed. Strict src-tauri forbidden-symbol search clean."},"findings":{"critical":0,"important":0,"minor":2},"report_file":".superpowers/sdd/task-6-review-grok-report.md"}
-->
