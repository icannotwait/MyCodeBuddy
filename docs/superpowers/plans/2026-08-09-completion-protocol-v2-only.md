# Completion Protocol V2-Only Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `(completion_protocol_version=2, completion_protocol_mode=v2_enforce)` the only writable workflow completion protocol while preserving protocol-v1 workflows as immutable historical reads.

**Architecture:** The workflow module owns a fixed v2 identity and one pair-based mutation guard used before every workflow write or linked root side effect. Public creation, settlement, admission, terminal handling, and database triggers all fail closed; historical projection remains read-only, and standalone non-workflow delegation keeps its existing Card display behavior. Rollout, shadow, restart, settings, metrics, transport, and UI surfaces are removed only after their consumers have moved to the fixed contract.

**Tech Stack:** Rust 2021, Tauri 2, Axum, SeaORM/SQLite, Tokio, Next.js 16 static export, React 19, TypeScript strict, Vitest, next-intl, pnpm.

**Requirements Baseline:** `docs/superpowers/specs/2026-08-09-completion-protocol-v2-only-design.md`, verified SHA-256 `61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`.

## Global Constraints

- The only writable identity is exactly version `2` plus mode `v2_enforce`.
- `require_v2_mutation(version, mode)` allows only `(2, v2_enforce)`; every version `1` pair, including `(1, v2_enforce)`, returns `legacy_completion_protocol_read_only`; every other pair returns `unsupported_completion_protocol`.
- New workflow creation never accepts agent, profile, environment, request, or caller-supplied protocol selection.
- `CODEG_COMPLETION_PROTOCOL_MODE` and `CODEG_COMPLETION_PROTOCOL_OVERRIDES` are removed surfaces. Desktop and server startup reject either variable even when its value is `v2_enforce`, using `completion_protocol_configuration_removed`; server exit code is `2`.
- Historical v1 rows, Cards, transcripts, restart-context rows, and existing predecessor/successor links remain stored and readable. They are not upgraded, copied, deleted, or given automatic successors.
- No workflow-bound error may invoke v1 Card authority, shadow comparison, successor creation, or Card re-emission. Protocol-v2 semantic inputs remain ordered as `complete_work`, explicit conclusion line, eligible bounded-report conclusion, then typed user adjudication.
- Standalone delegation without a workflow run binding retains its existing display/Card-summary behavior.
- A dangling or missing workflow header claimed by a terminal run is permanently classified as `unsupported_completion_protocol`. Unknown pairs and actual header enum/deserialization/type-conversion failures use the same code. Connection acquisition, connection, query, execution, and other database infrastructure failures retain their existing persistence classification; SQLite busy/locked failures retain the existing retry rail. The durable run row, wait result, and emitted task/run event must all carry the exact protocol code for a permanent header failure, and no `PendingTerminalRetry` is installed for that failure.
- `CompletionProtocolWorkflowProjection.creation_mode` remains required and always equals the persisted `completion_protocol_mode`; there is no separate creation-mode column.
- `CompletionProtocolMode::V1` and `CompletionProtocolMode::V2Shadow` remain only as historical SeaORM read values.
- Test-only historical fixtures are available only under `cfg(any(test, feature = "test-utils"))` and seed legacy rows before the v2-only trigger migration.
- Every numbered task follows test first, implementation, automated verification, producer commit, independent review, then admission of the next task. There is no human UAT between tasks.
- Normal-risk Task 10 is test-only. If it exposes a production gap, stop Task 10 and reopen the owning high-risk Task with its Codex implementer and independent Codex plus Grok reviewers; only after that high-risk fix is committed and approved may a fresh Task 10 admission continue.
- Task 11 commits every branch-tracked mutation, including delivery evidence, before Final reviewer admission. Both Final reviewers approve the same clean frozen `HEAD`; their platform reports/cards are authoritative, and no commit occurs after approval.
- Use the repository's PowerShell shell and the full-memory commands below unless the execution environment is explicitly declared low-memory under `AGENTS.md`.
- Never stage, rewrite, or revert an unrelated or pre-existing user change while following this plan.

---

## Risk Policy And Task Routing Matrix

Policy version: `b2d_task_risk_v1`.

Hard triggers always produce `high`: `concurrency_lifecycle`, `security_trust_boundary`, `migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`, `update_rollback`. Soft signals are `cross_runtime_or_process=2` and one point each for `broad_production_surface`, `multiple_ownership_modules`, `shared_interface`, `dependency_or_build`, and `multi_layer_without_test_seam`; a soft total of at least `3` is `high`, otherwise `normal`. High tasks use a Codex implementer who is neither this Plan Author nor either reviewer, followed by independent Codex and Grok reviewers. Normal tasks use a Grok implementer followed by an independent Codex reviewer.

| Index | Title | Files/modules | Hard triggers evidence | Soft signals evidence | Soft total | Final level and reason | Implementer | Reviewer set | Policy version |
| --- | --- | --- | --- | --- | ---: | --- | --- | --- | --- |
| 1 | Fixed identity, guard, and stable errors | workflow types/errors; app/ACP/web mappings | `public_compatibility`: defines stable wire codes | `shared_interface`=1 | 1 | **high**, hard trigger | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 2 | Startup rejection and fixed creation | desktop, server, app state, listener, workflow store | `public_compatibility`: removes env selection behavior | `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1 | 5 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 3 | V2-only settlement contract | MCP schema, companion transport, listener, store | `public_compatibility`: narrows a public tool request | `cross_runtime_or_process`=2, `multiple_ownership_modules`=1, `shared_interface`=1 | 4 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 4 | Mutation, recovery, delivery, and root fences | workflow store/evidence/admission, manager, automation, chat channel | `security_trust_boundary`: authorization and pre-side-effect admission fences | `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1 | 3 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 5 | Task admission and terminal fail-closed rail | admission, run store, broker, completion evidence | `concurrency_lifecycle` and `security_trust_boundary`: child launch and terminal CAS/retry ownership | `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1 | 5 | **high**, hard triggers and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 6 | Legacy restart removal and historical projection | restart module, MCP/HTTP/Tauri surfaces, projection | `public_compatibility`: deletes restart APIs and payloads | `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1 | 4 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 7 | SQLite v2-only triggers and historical fixtures | migrations, test helpers, migration integration tests | `migration_destructive_persistence`: write/freeze triggers and rollback | `shared_interface`=1, `dependency_or_build`=1 | 2 | **high**, hard trigger | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 8 | Backend rollout/settings/metrics cleanup | app state, desktop/server/web startup, commands, metrics | `public_compatibility`: removes settings API and metrics fields | `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1 | 5 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 9 | Frontend API, controls, types, and translations | API/transport, graph/settings UI, ten locales | `public_compatibility`: removes user actions and frontend API types | `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1 | 3 | **high**, hard trigger and soft threshold | Codex task implementer | independent Codex + Grok | `b2d_task_risk_v1` |
| 10 | Pre-final test-only aggregate contract audit | two Rust integration test files only | none | `broad_production_surface`=1, `multiple_ownership_modules`=1 | 2 | **normal**, soft total is below 3 and production mutation is forbidden | Grok task implementer | independent Codex | `b2d_task_risk_v1` |
| 11 | Final verification, delivery-evidence commit, then dual review | all touched surfaces; delivery report | none | `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `dependency_or_build`=1 | 5 | **high**, soft threshold | Codex final integrator | independent Codex + Grok | `b2d_task_risk_v1` |

Risk distribution: **10 high, 1 normal**.

## File Structure

| Path | Responsibility after delivery | Tasks |
| --- | --- | --- |
| `src-tauri/src/acp/delegation/workflow/types.rs` | Fixed v2 constructor, historical projection types, no rollout/selection types | 1, 2, 6, 8 |
| `src-tauri/src/acp/delegation/workflow/error.rs` | Shared protocol guard and stable workflow codes | 1, 4, 6 |
| `src-tauri/src/acp/delegation/workflow/mod.rs` | Export only v2 write APIs and historical read APIs | 1, 3, 6, 8 |
| `src-tauri/src/app_error.rs` | Stable Tauri/HTTP app error variants | 1, 6 |
| `src-tauri/src/acp/error.rs` | ACP mapping without restart-successor payloads | 1, 4, 6 |
| `src-tauri/src/web/handlers/error.rs` | HTTP status mapping for stable protocol/configuration errors | 1, 6 |
| `src-tauri/src/acp/delegation/workflow/store.rs` | Fixed publication, v2-only settlement, guarded recovery and delivery | 2, 3, 4 |
| `src-tauri/src/acp/delegation/listener.rs` | V2-only MCP dispatch and guarded recovery authorization | 2, 3, 4, 6, 8 |
| `src-tauri/src/acp/delegation/tool_schema.json` | V2 settlement schema and no restart tool | 3, 6 |
| `src-tauri/src/acp/delegation/transport.rs` | V2-only broker DTOs | 3, 6 |
| `src-tauri/src/acp/delegation/companion.rs` | Parse only remaining workflow tools | 3, 6 |
| `src-tauri/src/acp/delegation/workflow/completion_evidence.rs` | Guard decision, self-review, artifact, and terminal mutations | 4, 5 |
| `src-tauri/src/acp/delegation/workflow/admission.rs` | Guard dispatch and complete-work binding | 4, 5 |
| `src-tauri/src/acp/delegation/run_store.rs` | Typed terminal protocol lookup and durable terminal failure | 5 |
| `src-tauri/src/acp/delegation/store.rs` | Preserve stable protocol code through task-store reporting | 5 |
| `src-tauri/src/acp/delegation/broker.rs` | Separate standalone, v2, protocol rejection, and transient retry rails | 5, 6, 8 |
| `src-tauri/src/acp/manager.rs` | Linked root v2 admission before side effects | 4, 6, 8 |
| `src-tauri/src/automation/engine.rs` | Automation prompts use the same root admission fence | 4 |
| `src-tauri/src/chat_channel/session_commands.rs` | Chat-channel prompts use the same root admission fence | 4 |
| `src-tauri/src/acp/delegation/workflow/workflow_restart.rs` | Historical link/context reads only; all restart writers removed | 6 |
| `src-tauri/src/acp/delegation/workflow/project.rs` | Historical read-only projection and existing links | 6 |
| `src-tauri/src/commands/workflow_completion.rs` | Remaining completion mutations only | 4, 6, 8 |
| `src-tauri/src/web/handlers/workflow_completion.rs` | Remaining completion HTTP handlers only | 4, 6, 8 |
| `src-tauri/src/web/router.rs` | No restart or completion-settings route | 6, 8 |
| `src-tauri/src/lib.rs` | Desktop startup rejection and no removed commands/state | 2, 6, 8 |
| `src-tauri/src/server_bin/main.rs` | Server startup rejection with exit code 2 | 2, 8 |
| `src-tauri/src/web/mod.rs` | Web runtime state without rollout configuration | 2, 8 |
| `src-tauri/src/app_state.rs` | Shared state without rollout configuration | 2, 8 |
| `src-tauri/src/acp/delegation/metrics.rs` | V2 intent/evidence/attention metrics only; retain root-wake queue | 6, 8 |
| `src-tauri/src/db/migration/m20260809_000001_completion_protocol_v2_only.rs` | Insert and immutable-field triggers, trigger-only rollback | 7 |
| `src-tauri/src/db/migration/mod.rs` | Register the new forward migration last | 7 |
| `src-tauri/src/db/test_helpers.rs` | Migration-aware legacy fixture under test/test-utils cfg | 7 |
| `src-tauri/tests/completion_protocol_migrations.rs` | Up/down trigger and historical preservation tests | 7 |
| `src-tauri/tests/completion_protocol_v2.rs` | Fixed creation, mutation matrix, terminal, and aggregate contract tests | 1-6, 10 |
| `src-tauri/tests/completion_transport_parity.rs` | MCP/transport/error surface parity and absence assertions | 3, 6, 8, 10 |
| `src/lib/api.ts` | No restart or completion-settings client | 9 |
| `src/lib/api.test.ts` | Frontend API allowlist regression tests | 9 |
| `src/lib/types.ts` | Historical protocol projection without restart/settings DTOs | 9 |
| `src/lib/transport/web-transport.ts` | No restart/settings command mapping | 9 |
| `src/lib/transport/web-transport.test.ts` | Removed-command rejection tests | 9 |
| `src/components/chat/workflow-graph-panel.tsx` | Read-only v1 notice and links, no mutation controls | 9 |
| `src/components/chat/workflow-overlay.test.tsx` | V1 read-only and v2 control regression tests | 9 |
| `src/components/settings/delegation-settings.tsx` | Delegation settings without completion rollout status | 9 |
| `src/components/settings/delegation-settings.test.tsx` | Settings load no longer requests or renders rollout status | 9 |
| `src/i18n/messages/ar.json` | Remove restart/rollout copy; retain v1 read-only/link copy | 9 |
| `src/i18n/messages/de.json` | Same locale contract | 9 |
| `src/i18n/messages/en.json` | Same locale contract | 9 |
| `src/i18n/messages/es.json` | Same locale contract | 9 |
| `src/i18n/messages/fr.json` | Same locale contract | 9 |
| `src/i18n/messages/ja.json` | Same locale contract | 9 |
| `src/i18n/messages/ko.json` | Same locale contract | 9 |
| `src/i18n/messages/pt.json` | Same locale contract | 9 |
| `src/i18n/messages/zh-CN.json` | Same locale contract | 9 |
| `src/i18n/messages/zh-TW.json` | Same locale contract | 9 |
| `.superpowers/sdd/completion-protocol-v2-only-delivery-report.md` | Final command and review-input evidence committed before Final admission | 11 |

## Shared Interfaces

Tasks must keep these names and signatures consistent:

```rust
pub const CURRENT_COMPLETION_PROTOCOL_VERSION: i64 = 2;

pub fn current_completion_protocol_mode() -> CompletionProtocolMode {
    CompletionProtocolMode::V2Enforce
}

pub fn require_v2_mutation(
    version: i64,
    mode: &CompletionProtocolMode,
) -> Result<(), WorkflowStoreError>;

pub async fn publish_workflow_manifest_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: PublishWorkflowRequest,
) -> Result<PublishResult, WorkflowStoreError>;

pub async fn settle_workflow_gate_v2_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowV2Request,
) -> Result<SettleResult, WorkflowStoreError>;
```

The stable post-change protocol codes are:

```text
legacy_completion_protocol_read_only
unsupported_completion_protocol
completion_instruction_binding_failed
completion_protocol_configuration_removed
```

`WorkflowStoreError`, `CompletionMutationError`, `CompleteWorkError`, `TaskStoreError`, `AcpError`, `AppErrorCode`, HTTP responses, wait reports, and emitted task/run events must preserve those strings rather than parsing messages.

## Design Traceability

| Design requirement | Implemented by | Proved by |
| --- | --- | --- |
| Fixed current identity and no production selection | Tasks 1, 2, 8 | fixed-creation tests and final source search |
| Reject removed environment configuration | Tasks 2, 8 | desktop/server startup tests for both variables and all old values |
| V2-only publication and frozen revision identity | Tasks 2, 7 | publication tests plus insert/update triggers |
| One shared pair guard | Tasks 1, 4, 5 | pair table covering `(2,v2_enforce)`, all v1, `(2,v1)`, `(2,v2_shadow)` |
| Recovery-authorization and root-prompt fences | Task 4 | unchanged durable counts and no prompt/route/transcript side effects |
| Corrupt/undecodable non-terminal headers fail closed | Task 4 | publication, recovery mutation, and linked-root tests return `unsupported_completion_protocol` with zero side effects |
| V2-only settlement schema | Task 3 | JSON schema/DTO unknown-field and absence tests |
| Guarded task admission and child MCP binding | Task 5 | launch-count, binding, canonical instruction, and feature tests |
| Terminal fail-closed, no Card/shadow fallback | Task 5 | durable/wait/event code parity and retry-queue assertions |
| Historical reads and relationship links | Tasks 6, 9 | backend projection and frontend rendering tests |
| No restart writers or APIs | Tasks 6, 9 | reference searches and transport/UI tests |
| Insert/freeze database enforcement | Task 7 | migration up/down matrix |
| No rollout/settings/shadow/restart metrics | Tasks 8, 9 | snapshots, API tests, and source searches |
| Preserve v2 semantic channels and standalone behavior | Tasks 5, 10 | aggregate terminal contract tests |
| D-CODEX-M3 dangling-header stable code | Tasks 5, 10 | row/wait/event all equal `unsupported_completion_protocol` |
| D-CODEX-M4 expanded negative mutation matrix | Tasks 4, 5 | self-review, final delivery, `complete_work`, and inconsistent-pair tests |
| D-GROK-M1 `creation_mode` parent ruling | Tasks 6, 9, 10 | always present and equal to persisted `mode` |

### Task 1: Add Fixed Identity, Shared Guard, And Stable Errors

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: stable wire errors are `public_compatibility`. Soft evidence: `shared_interface`=1, total 1.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/acp/error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`

**Interfaces:**
- Produces `CURRENT_COMPLETION_PROTOCOL_VERSION`, `current_completion_protocol_mode`, and `require_v2_mutation` exactly as declared in Shared Interfaces.
- Produces typed stable variants for read-only v1, unsupported pairs, instruction binding failure, and removed configuration.
- Temporarily leaves rollout and restart-family definitions available for existing consumers; Tasks 6 and 8 delete them after callers are gone.

- [ ] **Step 1: Write the exhaustive guard tests before implementation**

Add a table-driven unit test beside `WorkflowStoreError` and a public-code mapping test in `completion_protocol_v2.rs`:

```rust
#[test]
fn require_v2_mutation_classifies_all_protocol_pairs() {
    use CompletionProtocolMode::{V1, V2Enforce, V2Shadow};

    assert_eq!(require_v2_mutation(2, &V2Enforce), Ok(()));
    for mode in [V1, V2Shadow, V2Enforce] {
        let error = require_v2_mutation(1, &mode).unwrap_err();
        assert_eq!(error.code(), "legacy_completion_protocol_read_only");
        assert!(!error.is_retryable());
    }
    for mode in [V1, V2Shadow] {
        let error = require_v2_mutation(2, &mode).unwrap_err();
        assert_eq!(error.code(), "unsupported_completion_protocol");
        assert!(!error.is_retryable());
    }
}
```

- [ ] **Step 2: Run the new tests and confirm the expected failure**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils require_v2_mutation_classifies_all_protocol_pairs
```

Expected: compilation fails because `require_v2_mutation` and the new stable variants do not exist.

- [ ] **Step 3: Add the fixed constructor and pair-based guard**

Use the SeaORM enum only as a value type:

```rust
pub const CURRENT_COMPLETION_PROTOCOL_VERSION: i64 = 2;

pub fn current_completion_protocol_mode() -> CompletionProtocolMode {
    CompletionProtocolMode::V2Enforce
}

pub fn require_v2_mutation(
    version: i64,
    mode: &CompletionProtocolMode,
) -> Result<(), WorkflowStoreError> {
    if version == 2 && mode == &CompletionProtocolMode::V2Enforce {
        return Ok(());
    }
    if version == 1 {
        return Err(WorkflowStoreError::LegacyCompletionProtocolReadOnly);
    }
    Err(WorkflowStoreError::UnsupportedCompletionProtocol {
        version,
        mode: mode.clone(),
    })
}
```

Make `WorkflowStoreError::code()` return the exact strings and keep both protocol errors non-retryable. Do not collapse them into `workflow_invalid` or `workflow_persistence_failure`.

- [ ] **Step 4: Add typed mappings at every existing public error boundary**

Add `AppErrorCode` variants serialized in snake case and map them without message inspection:

```rust
LegacyCompletionProtocolReadOnly,
UnsupportedCompletionProtocol,
CompletionInstructionBindingFailed,
CompletionProtocolConfigurationRemoved,
```

Map read-only/unsupported to HTTP `409 Conflict`, instruction binding to `409 Conflict`, and removed configuration to `400 Bad Request`. Add direct unit assertions for `WorkflowStoreError::code()`, `AcpError::code()`, serialized `AppCommandError.code`, and HTTP status.

- [ ] **Step 5: Verify the focused Rust surfaces**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils require_v2_mutation
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils stable_protocol_error_codes
cargo check --manifest-path src-tauri/Cargo.toml
```

Expected: all selected tests pass and desktop `cargo check` succeeds.

- [ ] **Step 6: Commit the producer change**

```powershell
git add src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/app_error.rs src-tauri/src/acp/error.rs src-tauri/src/web/handlers/error.rs src-tauri/tests/completion_protocol_v2.rs
git commit -m "feat: define v2-only completion protocol guard"
```

- [ ] **Step 7: Complete the task review gate before Task 2**

Give the commit hash and Task 1 diff to the independent Codex and Grok reviewers. Both must confirm exact pair classification, stable mappings, and non-retryability. Resolve each finding with a failing regression test, a focused fix, rerun Step 5, and a new producer commit; re-review until both approve.

### Task 2: Reject Removed Configuration And Make Creation Fixed V2

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: environment and creation behavior are `public_compatibility`. Soft evidence: `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1, total 5.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/server_bin/main.rs`
- Modify: `src-tauri/src/web/mod.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`
- Test: `src-tauri/tests/completion_transport_parity.rs`

**Interfaces:**
- Consumes the fixed identity and guard from Task 1.
- Keeps only `publish_workflow_manifest_core`; deletes `publish_workflow_manifest_with_selection_core`.
- Adds `reject_removed_completion_protocol_configuration() -> Result<(), CompletionProtocolConfigurationRemoved>` as the single desktop/server startup preflight.
- Existing rollout structures may remain read-only until Task 8 so intermediate settings code compiles, but publication and listener code must not consult them.

- [ ] **Step 1: Add failing creation and startup tests**

Add tests that publish with several agent/profile contexts through production entry points and query the row:

```rust
assert_eq!(row.completion_protocol_version, 2);
assert_eq!(row.completion_protocol_mode, CompletionProtocolMode::V2Enforce);
```

Publish a revision and assert both persisted fields remain unchanged. Add serial environment tests for each variable with values `v1`, `v2_shadow`, and `v2_enforce`; every value must return code `completion_protocol_configuration_removed`. Put the shared preflight tests in the library and server exit-code tests beside the server startup function in `server_bin/main.rs`; assert exit code `2` and an operator message naming the variable to remove.

- [ ] **Step 2: Confirm the tests fail under selectable creation**

Run:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils fixed_v2_creation
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils removed_completion_protocol_environment
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features "server,test-utils" --bin codeg-server removed_completion_protocol_environment
```

Expected: new workflows still default to v1 or startup accepts at least one removed configuration value.

- [ ] **Step 3: Replace selection-taking publication with fixed insertion**

Make `publish_workflow_manifest_core` call its transaction directly and pass only fixed values on new-row insertion:

```rust
let protocol_version = CURRENT_COMPLETION_PROTOCOL_VERSION;
let protocol_mode = current_completion_protocol_mode();
publish_in_txn(
    txn,
    parent_conversation_id,
    &normalized,
    &document_digest,
    now,
    protocol_version,
    protocol_mode,
)
.await
```

When `publish_in_txn` loads an existing row, call `require_v2_mutation` before revision comparison or any update. Delete `publish_workflow_manifest_with_selection_core`, selection parameters, listener agent/profile selection, and production v1 constructors from publication paths. Do not change protocol fields on revision.

- [ ] **Step 4: Install one startup preflight in desktop and server paths**

The helper checks variable presence, not value validity:

```rust
for name in [
    "CODEG_COMPLETION_PROTOCOL_MODE",
    "CODEG_COMPLETION_PROTOCOL_OVERRIDES",
] {
    if std::env::var_os(name).is_some() {
        return Err(CompletionProtocolConfigurationRemoved { variable: name });
    }
}
Ok(())
```

Call it before constructing shared state in `lib.rs` and `server_bin/main.rs`. Server startup logs `completion_protocol_configuration_removed`, names the variable, and returns process code `2`. Test constructors no longer install a v1 default used by publication.

- [ ] **Step 5: Prove production publication has no selection input**

Run:

```powershell
$hits = rg -n "publish_workflow_manifest_with_selection_core|select_completion_protocol\(" src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/workflow/store.rs
if ($LASTEXITCODE -eq 0) { $hits; throw "production publication still selects a completion protocol" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
```

Expected: `rg` returns exit code `1`, meaning no matches in those production files.

- [ ] **Step 6: Verify both runtimes and creation behavior**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils fixed_v2_creation
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils removed_completion_protocol_environment
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features "server,test-utils" --bin codeg-server removed_completion_protocol_environment
cargo check --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server
```

Expected: all focused tests and both checks pass.

- [ ] **Step 7: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/app_state.rs src-tauri/src/lib.rs src-tauri/src/server_bin/main.rs src-tauri/src/web/mod.rs src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs
git commit -m "feat: create only completion protocol v2 workflows"
```

Independent Codex and Grok reviewers must approve fixed insertion, frozen revisions, variable-presence rejection, server code 2, and absence of selection from production publication. Fix findings test-first and repeat review before Task 3.

### Task 3: Narrow Settlement To The V2 Tool Contract

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: the MCP request is `public_compatibility`. Soft evidence: `cross_runtime_or_process`=2, `multiple_ownership_modules`=1, `shared_interface`=1, total 4.

**Files:**
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Test: `src-tauri/tests/completion_transport_parity.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`

**Interfaces:**
- Consumes `settle_workflow_gate_v2_core` and `require_v2_mutation`.
- `BrokerSettleWorkflowRequest` retains `token`, `workflow_id`, `gate_id`, `expected_graph_revision`, optional `expected_review_round`, optional `expected_gate_cycle`, optional `expected_outcome`, optional `recovery_authorization_id`, and `summary`.
- Removes public `manifest_revision`, `gate_cycle`, legacy `outcome`, and legacy `evidence`.
- Deletes public `settle_workflow_gate_core`; a private derived-settlement input may remain inside `store.rs` for v2 gate reduction.

- [ ] **Step 1: Write schema and deserialization failures first**

Parse `tool_schema.json`, locate `settle_workflow_gate`, and assert:

```rust
for removed in ["manifest_revision", "gate_cycle", "outcome", "evidence"] {
    assert!(properties.get(removed).is_none(), "legacy field {removed} remains");
}
for retained in [
    "workflow_id",
    "gate_id",
    "expected_graph_revision",
    "summary",
] {
    assert!(properties.get(retained).is_some(), "v2 field {retained} missing");
}
```

Deserialize a broker request containing each removed field and assert unknown-field rejection. Add Design/Plan conditional tests for the retained expected round/outcome fields.

- [ ] **Step 2: Run the parity tests and observe failure**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils settle_workflow_gate_v2_only_schema
```

Expected: the schema and DTO still accept legacy properties.

- [ ] **Step 3: Remove legacy fields and the version branch**

Set `#[serde(deny_unknown_fields)]` on `BrokerSettleWorkflowRequest`, remove the four fields, and have the listener build only `SettleWorkflowV2Request`. The listener must call only `settle_workflow_gate_v2_core`. In the store, guard the loaded header before the v2 self-review preflight and transaction. Remove the public v1 settlement function and its export; rename any private legacy-shaped helper to describe derived internal state, not a public protocol.

- [ ] **Step 4: Preserve conditional v2 validation**

Keep gate-kind requirements explicit in the companion/listener parser:

```rust
match gate_kind {
    DocumentGateKind::Design => require_expected_outcome(&request)?,
    DocumentGateKind::Plan => require_expected_review_round(&request)?,
}
```

Unknown fields must fail before dispatch, and model arguments still cannot carry workflow, task, node, role, gate-cycle authority, or protocol identity beyond the root tool's explicit workflow/gate CAS fields.

- [ ] **Step 5: Verify schema, transport, and store behavior**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils settle_workflow_gate
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils v2_settlement
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp
```

Expected: all tests pass and `codeg-mcp` compiles.

- [ ] **Step 6: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/transport.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/tests/completion_transport_parity.rs src-tauri/tests/completion_protocol_v2.rs
git commit -m "feat: expose only v2 workflow settlement"
```

Independent Codex and Grok reviewers verify the exact schema subtraction, unknown-field rejection, conditional v2 requirements, shared guard, and absence of a production v1 settle call. Resolve findings before Task 4.

### Task 4: Fence Every Mutation, Recovery Authorization, Delivery, And Root Prompt

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: these are `security_trust_boundary` checks. Soft evidence: `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1, total 3.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src-tauri/src/chat_channel/session_commands.rs`
- Modify: `src-tauri/src/commands/workflow_completion.rs`
- Modify: `src-tauri/src/web/handlers/workflow_completion.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`

**Interfaces:**
- Consumes `require_v2_mutation` before a write transaction and again in transaction-critical helpers.
- Adds protocol-preserving variants to `CompletionMutationError` and `CompleteWorkError`, each returning the original stable code.
- Adds `load_completion_protocol_header` and `load_completion_protocol_for_conversation` as typed header loaders. Only enum/header conversion failures produced while decoding those header columns become `UnsupportedCompletionProtocolHeader`; a missing required header and an unknown version are handled as explicit protocol failures by their callers. `DbErr::ConnectionAcquire`, `DbErr::Conn`, `DbErr::Query`, `DbErr::Exec`, and every other database/infrastructure variant remain the existing persistence error. SQLite busy/locked persistence failures retain the existing retry classification.
- Adds one manager-owned linked-root preflight used by foreground, linked background, automation, and chat-channel prompt paths.

```rust
pub async fn load_completion_protocol_header<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
) -> Result<Option<(i64, CompletionProtocolMode)>, WorkflowStoreError>;

pub async fn load_completion_protocol_for_conversation(
    db: &AppDatabase,
    conversation_id: i32,
) -> Result<Option<(i64, CompletionProtocolMode)>, WorkflowStoreError>;
```

- [ ] **Step 1: Build a negative mutation snapshot fixture**

The fixture records before/after workflow revision, gate state, settlements, attentions, run bindings, child-spawn count, authorization rows, user questions, prompt queue length, transcript count, route state, and workflow graph revision. Seed each rejected pair `(1,v1)`, `(1,v2_shadow)`, `(1,v2_enforce)`, `(2,v1)`, and `(2,v2_shadow)` and assert the correct shared-guard code plus an identical snapshot.

Also create two corruption fixtures by inserting a valid header, enabling `PRAGMA ignore_check_constraints = ON` only on that private test connection, updating one row to version `99` and another row to mode `corrupt_mode`, then immediately restoring `PRAGMA ignore_check_constraints = OFF`. These rows exercise permanent typed loading failures that occur before an enum-valued guard can run.

Add focused loader/error-injection fixtures that preserve the original `DbErr` until classification. Cover an undecodable enum value, SQLite busy/locked, connection-pool acquisition timeout, a closed connection, and a non-type query failure. Use typed SeaORM errors or the repository's database test hooks, not message text fabricated to resemble an error variant.

- [ ] **Step 2: Add the full mutation matrix as failing tests**

Exercise all of these production operations:

```text
publish revision
settle Design and Plan gates
recover workflow
request recovery authorization for workflow subject
request recovery authorization for a workflow-bound task
resolve completion decision
resolve Design self-review decision
retry/resolve completion artifact
guard current Final delivery
guard task Final delivery
accept complete_work
manager foreground linked prompt
manager linked background prompt
automation prompt
chat-channel prompt
Tauri and Axum entry points that reach those paths
```

For version 1 expect `legacy_completion_protocol_read_only`; for the two inconsistent v2 pairs expect `unsupported_completion_protocol`. This explicitly closes D-CODEX-M4.

For both the unknown-version and undecodable-mode fixtures, exercise publication revision, `recover_workflow_core` as a direct mutation boundary, and manager linked-root admission. Each must return `unsupported_completion_protocol` and preserve the complete before/after snapshot. The root cases must enqueue no prompt, append no transcript, change no status/route, emit no link event, capture no context, and start no process.

In the focused classification test, require the undecodable enum/header value to return `unsupported_completion_protocol`, be non-retryable, and preserve the full side-effect snapshot. Require busy/locked to remain the existing persistence code and retryable. Require pool timeout/acquisition, closed-connection, and non-type query failures to remain persistence errors rather than `unsupported_completion_protocol`; preserve their pre-existing retryability rather than assigning retryability in the header mapper.

- [ ] **Step 3: Run the matrix and confirm existing side effects**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_mutation_matrix
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils corrupt_header_nonterminal_fences
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils header_db_error_classification
```

Expected: at least self-review, final delivery, `complete_work`, recovery authorization, root prompt, or corrupt header loading reaches its old path or returns a generic code. The focused error-classification test also exposes any string-based mapper that collapses pool, closed-connection, or query failures into `unsupported_completion_protocol`.

- [ ] **Step 4: Guard transaction-critical completion functions**

Load the owning workflow from the attention, run binding, or request, then call the shared guard before mutation. Preserve the code structurally:

```rust
require_v2_mutation(
    workflow.completion_protocol_version,
    &workflow.completion_protocol_mode,
)
.map_err(|error| CompletionMutationError::Protocol {
    code: error.code(),
    message: error.to_string(),
})?;
```

Apply this to completion decision, Design self-review, artifact retry/resolution, final-delivery guards, recovery, and `accept_complete_work_txn`. Guard again inside a direct transaction helper when bypass would otherwise be possible.

Centralize permanent-versus-transient header error mapping instead of letting SeaORM enum decoding escape as a generic database error:

```rust
fn map_completion_protocol_header_db_error(error: sea_orm::DbErr) -> WorkflowStoreError {
    match error {
        sea_orm::DbErr::Type(message) => {
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(message)
        }
        error @ sea_orm::DbErr::TryIntoErr { .. } => {
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(error.to_string())
        }
        other => WorkflowStoreError::Persistence(other.to_string()),
    }
}
```

Invoke this mapper only around the narrowly selected header-column decode, and match the original `DbErr` before any `to_string()` call. Do not route a wider database operation through it. `UnsupportedCompletionProtocolHeader` returns `unsupported_completion_protocol` from `code()` and is non-retryable. `ConnectionAcquire`, `Conn`, `Query`, `Exec`, and unlisted variants take the `Persistence` arm; the existing persistence classifier, outside this mapper, continues to recognize busy/locked for retry. Publication, recovery, recovery-authorization lookup, completion mutations, and root admission must use the typed loader so a permanent corrupt mode cannot bypass the stable mapping.

- [ ] **Step 5: Guard recovery authorization before prepare**

In the listener, resolve a workflow subject directly and a task subject through its workflow run binding. Call the guard before `RecoveryAuthorizationService::prepare`, before question registration, and before attention binding. Return unchanged standalone behavior for a task with no workflow binding.

- [ ] **Step 6: Replace auto-restart with one linked-root admission fence**

Inside the manager's existing prompt lock, after resolving the effective conversation id but before hydration, linking, transcript/status writes, events, routes, or send, load an owned/bound workflow and apply:

```rust
match load_completion_protocol_for_conversation(db, conversation_id).await
    .map_err(AcpError::from)?
{
    None => Ok(()),
    Some((version, mode)) => require_v2_mutation(version, &mode).map_err(AcpError::from),
}
```

Use this same manager path from automation and chat-channel consumers; do not add separate permissive checks. Newly attached and already attached sessions must both pass it.

- [ ] **Step 7: Verify no rejected operation changes state**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_mutation_matrix
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils corrupt_header_nonterminal_fences
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils header_db_error_classification
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils root_prompt_protocol_fence
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils recovery_authorization_protocol_fence
```

Expected: all pair and corruption rows return exact stable codes, every before/after snapshot is equal, and standalone recovery authorization remains passing. Undecodable enum data alone maps to the non-retryable unsupported code; busy/locked remains retryable persistence, while pool timeout/acquisition, closed-connection, and non-type query failures remain their existing persistence errors.

- [ ] **Step 8: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/manager.rs src-tauri/src/automation/engine.rs src-tauri/src/chat_channel/session_commands.rs src-tauri/src/commands/workflow_completion.rs src-tauri/src/web/handlers/workflow_completion.rs src-tauri/tests/completion_protocol_v2.rs
git commit -m "fix: fence legacy workflow mutations"
```

Independent Codex and Grok reviewers inspect ordering relative to every side effect, the complete inconsistent-pair matrix, and permanent header decode mapping on publication, recovery, and root admission. They must confirm the mapper matches the original typed `DbErr`, accepts only header `Type`/`TryIntoErr` conversion failures as unsupported, preserves infrastructure errors as persistence, and retains busy/locked retry behavior. Any missed boundary requires a regression test and a new reviewed commit before Task 5.

### Task 5: Enforce V2 Admission And Typed Terminal Failure

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: `concurrency_lifecycle` and `security_trust_boundary`. Soft evidence: `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1, total 5.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`

**Interfaces:**
- Successful workflow admission persists `WorkflowChildMcpBinding { protocol_version: 2, .. }`, the canonical instruction scope, and exposes child-only feature `completion_v2`.
- `terminal_completion_protocol(task_id)` returns `Standalone` or `V2`; protocol rejection is a typed `TaskStoreError::WorkflowAdmission { code, message }`; transient database lookup remains `TaskStoreError::Transient`.
- Missing header for an existing workflow binding maps to `unsupported_completion_protocol`, never generic permanent/persistence error.

- [ ] **Step 1: Add failing admission tests before changing launch code**

For each v1/inconsistent pair and a dangling workflow binding, call first dispatch, continue, and replacement. Capture budget reservations, inserted run rows, process spawns, prompts, and MCP features. Assert zero new side effects and stable code. For valid v2 assert:

```rust
assert_eq!(binding.protocol_version, 2);
assert_eq!(child_prompt.matches(CANONICAL_COMPLETION_INSTRUCTION).count(), 1);
assert_eq!(features, ["completion_v2"]);
assert!(child_tools.contains("complete_work"));
```

Assert root, standalone, unbound, and historical v1 children do not receive `complete_work`.

- [ ] **Step 2: Add terminal disposition tests including D-CODEX-M3**

Cover v1, `(2,v1)`, `(2,v2_shadow)`, dangling/missing workflow header, corrupt/unknown header deserialization, transient lookup then success, transient lookup exhaustion, valid v2 semantic channels, and standalone Card display. For every permanent protocol rejection assert:

```rust
assert_eq!(durable_row.status, "failed");
assert_eq!(durable_row.delegation_error_code.as_deref(), Some(expected_code));
assert_eq!(wait_report.error_code.as_deref(), Some(expected_code));
assert_eq!(terminal_event.error_code.as_deref(), Some(expected_code));
assert!(!retry_registry.contains(task_id).await);
assert_eq!(card_parser_calls.load(Ordering::SeqCst), 0);
assert_eq!(shadow_comparator_calls.load(Ordering::SeqCst), 0);
assert_eq!(semantic_write_counts, SemanticWriteCounts::default());
```

Set `expected_code = "unsupported_completion_protocol"` for a dangling/missing header and for every permanent header decode/corruption failure. This is the exact D-CODEX-M3 ruling.

- [ ] **Step 3: Run focused tests and observe the old fallback/retry behavior**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils workflow_admission_requires_v2
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils terminal_protocol_failure_is_typed
```

Expected: an invalid workflow can launch without a v2 binding, becomes `persistence_error`, enters `PendingTerminalRetry`, or reaches Card/shadow logic.

- [ ] **Step 4: Move the guard before admission side effects**

In `admit_workflow_run_txn`, load the header and call `require_v2_mutation` before budget authorization or run-binding insertion. Build the completion instruction scope before launch; map construction failures to `completion_instruction_binding_failed`. Commit the run binding, scope, canonical prompt suffix, and child-only MCP feature as one admitted outcome.

In `load_workflow_child_mcp_binding`, treat an existing run binding with no workflow header as `unsupported_completion_protocol`, and guard the pair before returning a binding.

- [ ] **Step 5: Introduce a typed terminal protocol classification**

Replace the raw optional pair with:

```rust
pub enum TerminalCompletionProtocol {
    Standalone,
    V2,
}
```

Return `Standalone` only when no workflow run binding exists. For a binding, missing header returns `TaskStoreError::WorkflowAdmission` with code `unsupported_completion_protocol`; a loaded header is checked by `require_v2_mutation`. Map permanent row-decode, unknown-enum, and corrupt-header errors to the same typed unsupported code. Database busy, locked, and connection-availability errors remain typed transient errors.

- [ ] **Step 6: Split broker terminal processing before parsing output**

`prepare_terminal_for_workflow` must branch in this order:

```rust
match runs.terminal_completion_protocol(task_id).await {
    Ok(TerminalCompletionProtocol::Standalone) => prepare_standalone_card(...),
    Ok(TerminalCompletionProtocol::V2) => prepare_v2_completion(...),
    Err(TaskStoreError::WorkflowAdmission { code, message }) => {
        prepare_typed_protocol_failure(code, message)
    }
    Err(TaskStoreError::Transient(message)) => prepare_transient_lookup_retry(message),
    Err(error) => prepare_typed_terminal_store_failure(error),
}
```

Settle typed protocol failure durably as `failed` with its stable code, then publish the same stored report to waiters and events. Do not pass this branch to the persistence retry registry. Retain bounded retry only for transient lookup failures and keep semantic writes blocked until lookup succeeds.

- [ ] **Step 7: Preserve every valid v2 semantic input**

Keep tests for `complete_work`, explicit terminal conclusion, eligible bounded-report conclusion, ambiguity attention, and typed user adjudication. Ensure each writes platform-generated v2 evidence and reduces gates through existing v2 logic. Confirm standalone output still creates its display summary.

- [ ] **Step 8: Verify admission, terminal, and retry behavior**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils workflow_admission_requires_v2
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils terminal_protocol_failure_is_typed
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils completion_v2_semantic_inputs
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils pending_terminal_retry
```

Expected: row/wait/event code parity passes, no permanent protocol failure is queued, transient retry tests still pass, all v2 inputs pass, and standalone display behavior passes.

- [ ] **Step 9: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/tests/completion_protocol_v2.rs
git commit -m "fix: fail closed on workflow terminal protocol errors"
```

Independent Codex and Grok reviewers must trace launch ordering, terminal CAS ownership, durable/wait/event code equality, retry classification, and the standalone boundary. Fix each finding test-first before Task 6.

### Task 6: Remove Legacy Restart Writers And Preserve Historical Projection

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: deletion of restart APIs is `public_compatibility`. Soft evidence: `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, total 4.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/workflow_restart.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/commands/workflow_completion.rs`
- Modify: `src-tauri/src/web/handlers/workflow_completion.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/acp/error.rs`
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`
- Test: `src-tauri/tests/completion_transport_parity.rs`

**Interfaces:**
- Keeps historical projection reads for `legacy_source`, `v2_successor`, version, mode, `creation_mode`, read-only reason, and automatic root wake.
- Deletes restart commands, DTOs, writer helpers, successor payloads, restart error family, and restart-context capture/backfill writers.
- Keeps the old migration/table, existing rows, relationship columns, and read queries.

- [ ] **Step 1: Write absence and historical-read tests**

Assert `restart_legacy_workflow` is absent from MCP catalog/dispatcher, broker messages, Tauri commands, Axum routes, and serialized errors. Seed historical linked workflows before database triggers exist, preserve the exact stored Card JSON bytes/display projection, and assert the protocol projection:

```rust
assert_eq!(projection.version, 1);
assert_eq!(projection.mode, persisted_mode);
assert_eq!(projection.creation_mode, persisted_mode);
assert_eq!(
    projection.read_only_reason.as_deref(),
    Some("legacy_completion_protocol_read_only")
);
assert!(!projection.automatic_root_wake);
assert_eq!(projection.legacy_source, expected_source_link);
assert_eq!(projection.v2_successor, expected_successor_link);
```

Run this for persisted `v1` and `v2_shadow`. The required and equal `creation_mode` assertion closes D-GROK-M1.

Add a positive catalog assertion so restart removal cannot erase the remaining root capability:

```rust
assert_eq!(
    WORKFLOW_V2_TOOLS,
    &[
        "get_workflow_capabilities",
        "get_workflow_state",
        "recover_workflow",
        "publish_workflow_manifest",
        "settle_workflow_gate",
    ]
);
assert!(root_tool_names.contains(&"request_recovery_authorization".to_string()));
```

The root `workflow_v2` catalog classification and local capability projection must still pass with this exact five-tool workflow set; `request_recovery_authorization` remains the existing shared root/coordination tool.

- [ ] **Step 2: Confirm the current restart surfaces fail the tests**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils legacy_restart_surface_is_absent
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_projection
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_v2_root_catalog_agrees_with_local_capabilities
```

Expected: restart remains callable or restart-only DTO/error fields remain.

- [ ] **Step 3: Delete every restart writer and dispatcher**

Remove `restart_legacy_workflow`, `restart_legacy_workflow_if_enforced`, `capture_original_request_context`, context backfill writers, successor creation, automatic pre-operation restart checks, broker transport variants, listener branches, tool schema entry, Tauri command registration, Axum route/handler, and their request/response DTOs.

Delete `LegacyWorkflowRestartProjection`, public restart-context payloads, `successor_conversation_id` error detail, `LegacyCompletionProtocolRestartRequired/Invalid/NotRequired`, and `AcpError::LegacyCompletionProtocolRestart`. Do not delete historical database migrations or tables.

- [ ] **Step 4: Reduce `workflow_restart.rs` to historical reads**

Retain or rename only private read helpers that load stored context/relationships for graph projection. Compute the protocol projection directly from the persisted header:

```rust
CompletionProtocolWorkflowProjection {
    version: header.completion_protocol_version,
    mode: header.completion_protocol_mode.clone(),
    creation_mode: header.completion_protocol_mode.clone(),
    legacy_source,
    v2_successor,
    read_only_reason: (header.completion_protocol_version == 1)
        .then(|| "legacy_completion_protocol_read_only".to_string()),
    automatic_root_wake: header.completion_protocol_version == 2
        && existing_v2_root_wake_condition,
}
```

Existing v1 links remain navigable even when a successor exists; `read_only_reason` never depends on link presence.

- [ ] **Step 5: Run strict backend reference searches**

```powershell
$hits = rg -n "restart_legacy_workflow|LegacyWorkflowRestartProjection|LegacyCompletionProtocolRestart|legacy_completion_protocol_restart_(required|invalid|not_required)|successor_conversation_id|capture_original_request_context" src-tauri/src src-tauri/tests
if ($LASTEXITCODE -eq 0) { $hits; throw "legacy restart production surface remains" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
```

Expected: no matches. Separately prove `legacy_source_workflow_id`, `delegation_workflow_restart_contexts`, `legacy_source`, and `v2_successor` still have read-side references, and prove all five retained root workflow tools remain in `WORKFLOW_V2_TOOLS`.

- [ ] **Step 6: Verify transport and projection behavior**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils legacy_restart_surface_is_absent
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical_protocol_projection
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_v2_root_catalog_agrees_with_local_capabilities
cargo check --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp
```

Expected: tests and all three runtime checks pass.

- [ ] **Step 7: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/workflow/workflow_restart.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/error.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/transport.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/manager.rs src-tauri/src/commands/workflow_completion.rs src-tauri/src/web/handlers/workflow_completion.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src-tauri/src/acp/error.rs src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs
git commit -m "refactor: remove legacy workflow restart writes"
```

Independent Codex and Grok reviewers verify no writer or wire payload remains, while old rows, context tables, exact persisted mode, required/equal `creation_mode`, and relationship links still read correctly. Fix and re-review before Task 7.

### Task 7: Add SQLite Insert And Freeze Triggers

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: `migration_destructive_persistence`. Soft evidence: `shared_interface`=1 and `dependency_or_build`=1, total 2.

**Files:**
- Create: `src-tauri/src/db/migration/m20260809_000001_completion_protocol_v2_only.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/test_helpers.rs`
- Modify: `src-tauri/tests/completion_protocol_migrations.rs`
- Modify: `src-tauri/tests/completion_protocol_v2.rs`

**Interfaces:**
- Creates triggers `trg_delegation_workflows_v2_only_insert`, `trg_delegation_workflows_protocol_frozen`, and `trg_delegation_workflows_legacy_source_frozen`.
- Adds a test/test-utils-only helper that migrates through the predecessor, seeds historical rows, then migrates to latest.
- Production and ordinary fresh test databases never disable or drop the triggers.

- [ ] **Step 1: Write migration tests before registering the migration**

Use a `BeforeCompletionProtocolV2Only` migrator that contains every migration through `m20260806_000004_legacy_restart_context`. Seed historical v1 and linked rows there, then run the full `Migrator`. Assert:

```text
historical v1 remains readable with unchanged fields
omitted protocol columns are rejected
explicit (1,v1) and (1,v2_shadow) inserts are rejected
(2,v1) and (2,v2_shadow) inserts are rejected
exact (2,v2_enforce) insert succeeds
v2 insert with non-null legacy_source_workflow_id is rejected
protocol UPDATE is rejected for historical and current rows
an UPDATE that changes graph_revision/updated_at while re-SETting identical protocol values succeeds for historical and current rows
an UPDATE that changes only non-protocol columns succeeds for historical and current rows
legacy_source UPDATE rejects NULL-to-value, value-to-NULL, value-to-different
historical unchanged links survive up and down
deleting a historical parent conversation after trigger installation succeeds and cascades its workflow and dependent rows
down removes only the three triggers and changes no rows
```

- [ ] **Step 2: Run migration tests and confirm they fail**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_migrations --features test-utils v2_only_trigger
```

Expected: post-migration invalid inserts and updates still succeed or the migration module is absent.

- [ ] **Step 3: Implement exact trigger SQL**

Use `execute_unprepared` in `up`:

```sql
CREATE TRIGGER trg_delegation_workflows_v2_only_insert
BEFORE INSERT ON delegation_workflows
WHEN NEW.completion_protocol_version <> 2
  OR NEW.completion_protocol_mode <> 'v2_enforce'
  OR NEW.legacy_source_workflow_id IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'completion_protocol_v2_only');
END;

CREATE TRIGGER trg_delegation_workflows_protocol_frozen
BEFORE UPDATE OF completion_protocol_version, completion_protocol_mode
ON delegation_workflows
WHEN NEW.completion_protocol_version IS NOT OLD.completion_protocol_version
  OR NEW.completion_protocol_mode IS NOT OLD.completion_protocol_mode
BEGIN
  SELECT RAISE(ABORT, 'completion_protocol_frozen');
END;

CREATE TRIGGER trg_delegation_workflows_legacy_source_frozen
BEFORE UPDATE OF legacy_source_workflow_id ON delegation_workflows
WHEN NOT (NEW.legacy_source_workflow_id IS OLD.legacy_source_workflow_id)
BEGIN
  SELECT RAISE(ABORT, 'legacy_source_workflow_frozen');
END;
```

`down` issues only three `DROP TRIGGER IF EXISTS` statements. Do not edit the 2026-08-04 migrations or rewrite rows.

Reproduce SeaORM's full-model update shape in a positive test by explicitly including unchanged protocol columns in the `SET` list:

```sql
UPDATE delegation_workflows
SET graph_revision = graph_revision + 1,
    updated_at = '2026-08-09T00:00:00Z',
    completion_protocol_version = completion_protocol_version,
    completion_protocol_mode = completion_protocol_mode
WHERE workflow_id = ?;
```

Run that assertion for a migrated historical v1 row and an exact `(2,v2_enforce)` row. It must succeed, while assignments that actually change version or mode must abort. Also seed at least one manifest/run/evidence dependent row for a historical parent, delete the parent `conversation`, and assert the workflow plus dependent rows are gone through the existing foreign-key cascades; the new triggers must not intercept `DELETE`.

- [ ] **Step 4: Make historical test fixtures migration-aware**

Add these definitions under `cfg(any(test, feature = "test-utils"))`:

```rust
pub struct HistoricalWorkflowSeed {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub version: i64,
    pub mode: CompletionProtocolMode,
    pub legacy_source_workflow_id: Option<String>,
}

pub async fn historical_completion_protocol_db(
    seeds: &[HistoricalWorkflowSeed],
) -> AppDatabase
```

The helper opens one in-memory connection, applies the predecessor migrator, inserts supplied historical rows and links, then applies only remaining migrations. Update all tests that need v1 rows to call `historical_completion_protocol_db(&seeds).await`. Never insert v1 after latest migration and never drop triggers on a shared fully migrated connection.

- [ ] **Step 5: Verify migration and historical test suites**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_migrations --features test-utils v2_only_trigger
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils historical
cargo check --manifest-path src-tauri/Cargo.toml
```

Expected: the complete trigger matrix passes, unchanged protocol re-SETs and non-protocol updates remain writable, historical parent deletion cascades normally, historical mutation tests use predecessor seeding, and desktop check passes.

- [ ] **Step 6: Commit and pass dual review**

```powershell
git add src-tauri/src/db/migration/m20260809_000001_completion_protocol_v2_only.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/test_helpers.rs src-tauri/tests/completion_protocol_migrations.rs src-tauri/tests/completion_protocol_v2.rs
git commit -m "feat: enforce completion protocol v2 in sqlite"
```

Independent Codex and Grok reviewers verify trigger null/default semantics, null-safe `NEW IS NOT OLD` value-change predicates, positive SeaORM-shaped updates, historical delete/cascade behavior, unchanged historical rows, rollback scope, and fixture migration order. Resolve every migration finding before Task 8.

### Task 8: Remove Backend Rollout, Settings, Shadow, And Restart Metrics

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: settings and metric schemas are `public_compatibility`. Soft evidence: `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1, total 5.

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/types.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/server_bin/main.rs`
- Modify: `src-tauri/src/web/mod.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/metrics.rs`
- Modify: `src-tauri/src/commands/workflow_completion.rs`
- Modify: `src-tauri/src/web/handlers/workflow_completion.rs`
- Modify: `src-tauri/src/web/router.rs`
- Test: `src-tauri/tests/completion_protocol_v2.rs`
- Test: `src-tauri/tests/completion_transport_parity.rs`

**Interfaces:**
- Deletes `CompletionProtocolRolloutConfig`, `CompletionProtocolSelection`, `CompletionProtocolSelectionSource`, `select_completion_protocol`, profile-key parsing, rollout windows/decisions, and old constructors.
- Deletes `get_completion_protocol_settings` Tauri/HTTP APIs and state injection.
- Deletes shadow comparison and restart-outcome metrics while retaining v2 intent source, evidence, attention, artifact recovery, typed outcome metrics, and `CompletionRootWakeQueue`.

- [ ] **Step 1: Write backend absence and retained-metric tests**

Assert settings commands/routes are absent, metrics JSON contains no `default_mode`, `profile_overrides`, `creation_modes`, `shadow_differences`, `rollout_windows`, `rollout_decisions`, or `restart_outcomes`, and still contains v2 intent/evidence/attention/artifact fields. Add a root-wake replay test that inserts a valid v2 completion-attention outbox event, drains it through `CompletionRootWakeQueue`, and asserts exactly one root wake plus the existing acknowledged outbox state; this positive path must pass after all restart metrics are removed.

- [ ] **Step 2: Run the tests and observe the obsolete snapshot**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils completion_rollout_surface_is_absent
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils completion_metrics_v2_only
```

Expected: settings and rollout/shadow/restart fields remain.

- [ ] **Step 3: Delete rollout configuration and settings APIs**

Remove the rollout types and helpers from `types.rs`, their exports, `Arc<CompletionProtocolRolloutConfig>` from shared state/listener/manager/web state, startup construction, command registration, handler, and route. The startup variable-presence rejection from Task 2 remains as a small fixed preflight and does not parse values.

- [ ] **Step 4: Remove only obsolete metrics and shadow execution**

Delete profile rollout windows/decisions, creation-mode selection telemetry, shadow comparator invocation, shadow differences, restart outcomes, `record_completion_restart`, and `CompletionRestartOutcome`. Preserve metrics for fixed v2 completion inputs and results. Preserve `CompletionRootWakeQueue`, attention outbox replay, and valid-v2 automatic root-wake behavior.

- [ ] **Step 5: Prove removed backend concepts have no references**

```powershell
$hits = rg -n "CompletionProtocolRolloutConfig|CompletionProtocolSelection(Source)?|select_completion_protocol|completion_protocol_profile_key|ProfileCompletionWindow|RolloutDecision|get_completion_protocol_settings|record_completion_restart|CompletionRestartOutcome|restart_outcomes|shadow_differences|rollout_windows|rollout_decisions" src-tauri/src src-tauri/tests
if ($LASTEXITCODE -eq 0) { $hits; throw "completion rollout or restart metrics remain" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
```

Expected: no matches. A separate search must still find `CompletionRootWakeQueue` and v2 intent/evidence/attention metric names.

- [ ] **Step 6: Verify backend cleanup across runtimes**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils completion_rollout_surface_is_absent
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils completion_metrics_v2_only
cargo check --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp
```

Expected: selected tests and all runtime checks pass.

- [ ] **Step 7: Commit and pass dual review**

```powershell
git add src-tauri/src/acp/delegation/workflow/types.rs src-tauri/src/acp/delegation/workflow/mod.rs src-tauri/src/app_state.rs src-tauri/src/lib.rs src-tauri/src/server_bin/main.rs src-tauri/src/web/mod.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/metrics.rs src-tauri/src/commands/workflow_completion.rs src-tauri/src/web/handlers/workflow_completion.rs src-tauri/src/web/router.rs src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs
git commit -m "refactor: remove completion protocol rollout state"
```

Independent Codex and Grok reviewers confirm full concept removal and explicitly verify that v2 semantic/evidence metrics and non-restart root wake remain. Fix and re-review before Task 9.

### Task 9: Remove Frontend Restart And Rollout Surfaces

**Routing:** high; Codex task implementer; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: frontend API and controls are `public_compatibility`. Soft evidence: `broad_production_surface`=1, `multiple_ownership_modules`=1, `shared_interface`=1, total 3.

**Files:**
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/transport/web-transport.ts`
- Modify: `src/lib/transport/web-transport.test.ts`
- Modify: `src/components/chat/workflow-graph-panel.tsx`
- Modify: `src/components/chat/workflow-overlay.test.tsx`
- Modify: `src/components/settings/delegation-settings.tsx`
- Modify: `src/components/settings/delegation-settings.test.tsx`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Keeps `CompletionProtocolWorkflowProjection` with required `creation_mode`, historical links, read-only reason, and automatic root wake.
- Deletes restart/settings request types and functions, web-transport mappings, settings status component, and v1 workflow mutation buttons.
- Keeps ordinary conversation deletion outside the graph mutation controls.

- [ ] **Step 1: Update tests first for the read-only frontend contract**

Render a v1 graph snapshot with both links and assert the read-only notice and link buttons are present, while restart, resume, settle, recovery, and other workflow mutation controls are absent. Assert the normal conversation delete action remains in its existing owner. Render a v2 snapshot and assert its existing valid controls still work.

In settings tests, expect only delegation settings and profile catalog requests. In API/transport tests, assert `restart_legacy_workflow` and `get_completion_protocol_settings` are rejected as unknown commands.

- [ ] **Step 2: Run frontend tests and confirm old controls remain**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/settings/delegation-settings.test.tsx src/lib/api.test.ts src/lib/transport/web-transport.test.ts
```

Expected: restart/settings calls or controls violate the new assertions.

- [ ] **Step 3: Remove frontend APIs, DTOs, and settings status**

Delete `restartLegacyWorkflow`, `getCompletionProtocolSettings`, `CompletionProtocolSettingsSnapshot`, their transport command mappings, rollout status state/loading, `CompletionProtocolStatus`, and formatting helpers used only by rollout display. The settings load becomes:

```ts
const [settings, catalog] = await Promise.all([
  getDelegationSettings(),
  getDelegationProfileCatalog(),
])
```

- [ ] **Step 4: Make the v1 graph display strictly read-only**

Remove restart/resume callbacks, pending state, errors tied only to those actions, and the corresponding buttons for v1. Keep `completionLegacyReadOnly`, `legacy_source`, and `v2_successor` rendering. Ensure TypeScript requires:

```ts
export interface CompletionProtocolWorkflowProjection {
  version: number
  mode: CompletionProtocolMode
  creation_mode: CompletionProtocolMode
  legacy_source?: LegacyWorkflowLink | null
  v2_successor?: LegacyWorkflowLink | null
  read_only_reason?: string | null
  automatic_root_wake: boolean
}
```

- [ ] **Step 5: Remove only obsolete translations in all ten locales**

Delete restart button, manual resume, completion default mode, creation counts, overrides, samples/minimum, rollout decision, and shadow-difference keys. Retain translations for historical read-only notice, source link, successor link, and valid v2 automatic wake where still rendered.

- [ ] **Step 6: Run reference searches and frontend verification**

```powershell
$hits = rg -n "restartLegacyWorkflow|getCompletionProtocolSettings|CompletionProtocolSettingsSnapshot|completion-protocol-status|completionLegacyRestart|completionManualRootResume|completionDefaultMode|completionShadowDifference|completionRolloutDecision" src
if ($LASTEXITCODE -eq 0) { $hits; throw "removed completion UI surface remains" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/settings/delegation-settings.test.tsx src/lib/api.test.ts src/lib/transport/web-transport.test.ts
pnpm eslint src/lib/api.ts src/lib/types.ts src/lib/transport/web-transport.ts src/components/chat/workflow-graph-panel.tsx src/components/settings/delegation-settings.tsx
```

Expected: source search has no matches, focused tests pass, and ESLint reports no errors.

- [ ] **Step 7: Commit and pass dual review**

```powershell
git add src/lib/api.ts src/lib/api.test.ts src/lib/types.ts src/lib/transport/web-transport.ts src/lib/transport/web-transport.test.ts src/components/chat/workflow-graph-panel.tsx src/components/chat/workflow-overlay.test.tsx src/components/settings/delegation-settings.tsx src/components/settings/delegation-settings.test.tsx src/i18n/messages
git commit -m "refactor: remove legacy completion controls"
```

Independent Codex and Grok reviewers verify v1 has no workflow mutation affordance, links/read-only copy remain, deletion remains available, v2 behavior is unchanged, and all locale JSON stays valid. Resolve findings before Task 10.

### Task 10: Run The Pre-Final Test-Only Aggregate Contract Audit

**Routing:** normal; Grok task implementer; independent Codex reviewer; `b2d_task_risk_v1`. Hard evidence: none. Soft evidence: `broad_production_surface`=1 and `multiple_ownership_modules`=1, total 2.

**Files:**
- Modify: `src-tauri/tests/completion_protocol_v2.rs`
- Modify: `src-tauri/tests/completion_transport_parity.rs`

**Interfaces:**
- Consumes all preceding production contracts.
- Produces one aggregate acceptance test and one cross-surface absence test so final verification cannot silently omit a design requirement.
- Produces no production change. Its admission is valid only while the diff is restricted to the two declared test files.

- [ ] **Step 1: Add an aggregate end-to-end acceptance test**

In one migration-aware test, verify fixed v2 creation, v2 child binding, one v2 semantic completion, historical v1 projection, rejected v1 mutation, dangling terminal code parity, and standalone Card display. Use exact assertions:

```rust
assert_eq!(new_workflow.protocol_pair(), (2, CompletionProtocolMode::V2Enforce));
assert_eq!(historical.creation_mode, historical.mode);
assert_eq!(legacy_mutation.unwrap_err().code(), "legacy_completion_protocol_read_only");
assert_eq!(dangling.row_code, "unsupported_completion_protocol");
assert_eq!(dangling.wait_code, dangling.row_code);
assert_eq!(dangling.event_code, dangling.row_code);
assert!(standalone.card_summary_json.is_some());
```

- [ ] **Step 2: Add a cross-surface source assertion test**

The transport parity test reads only repository-owned source/schema files and fails if removed public symbols are reintroduced. Its banned list is:

```text
restart_legacy_workflow
CompletionProtocolRolloutConfig
CompletionProtocolSelection
select_completion_protocol
get_completion_protocol_settings
legacy_completion_protocol_restart_required
legacy_completion_protocol_restart_invalid
successor_conversation_id
manifest_revision (inside settle_workflow_gate properties only)
gate_cycle (inside settle_workflow_gate properties only)
```

Scope the last two checks to the settle tool object because those names may remain valid in durable internal models.

- [ ] **Step 3: Run the aggregate tests**

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils v2_only_aggregate_acceptance
cargo test --manifest-path src-tauri/Cargo.toml --test completion_transport_parity --features test-utils v2_only_removed_surface_inventory
```

Expected: both pass. If either exposes a production gap, stop without editing production code or committing Task 10. Record the failed assertion, reopen the owning Task 1-9 under its original high-risk Codex implementer plus independent Codex and Grok reviewers, land and approve the test-first production fix there, then restart Task 10 with a fresh normal-risk admission against the new approved `HEAD`.

- [ ] **Step 4: Run the pre-final traceability search**

```powershell
rg -n "CURRENT_COMPLETION_PROTOCOL_VERSION|require_v2_mutation|legacy_completion_protocol_read_only|unsupported_completion_protocol|completion_instruction_binding_failed|completion_protocol_configuration_removed" src-tauri/src src-tauri/tests
rg -n "creation_mode|legacy_source|v2_successor|CompletionRootWakeQueue" src-tauri/src src-tauri/tests src
```

Expected: each required retained concept has production and test references. Review the output against Design Traceability; no row may lack an automated assertion.

- [ ] **Step 5: Commit and pass the normal review route**

```powershell
$allowed = @(
    "src-tauri/tests/completion_protocol_v2.rs",
    "src-tauri/tests/completion_transport_parity.rs"
)
$changed = @(
    git diff --name-only HEAD
    git ls-files --others --exclude-standard
) | Sort-Object -Unique
$unexpected = @($changed | Where-Object { $_ -notin $allowed })
if ($unexpected.Count -gt 0) {
    $unexpected
    throw "Task 10 normal route contains a non-test path"
}
git add src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs
git commit -m "test: audit completion protocol v2-only contract"
```

Before committing, compare the complete worktree to `HEAD`: `git diff --name-only HEAD` covers staged and unstaged tracked changes, and `git ls-files --others --exclude-standard` adds every untracked non-ignored path. Require every resulting Task 10 path to be one of the two declared test files; a staged production file or any untracked non-test file mechanically invalidates the normal-risk admission and must be routed through the owning high Task. The independent Codex reviewer verifies acceptance-criteria coverage, the test-only diff, and the absence of production mutations. Reviewer findings that require production changes reopen the owning high Task; test-only findings remain in Task 10. Complete the normal review gate before Task 11.

### Task 11: Complete Final Verification, Commit Delivery Evidence, Then Freeze For Dual Review

**Routing:** high; Codex final integrator; independent Codex and Grok reviewers; `b2d_task_risk_v1`. Hard evidence: none. Soft evidence: `cross_runtime_or_process`=2, `broad_production_surface`=1, `multiple_ownership_modules`=1, `dependency_or_build`=1, total 5.

**Files:**
- Review: every file listed in File Structure
- Create: `.superpowers/sdd/completion-protocol-v2-only-delivery-report.md`
- Modify only for reviewed regression fixes: files already owned by Tasks 1-10

**Interfaces:**
- Consumes the complete v2-only implementation.
- Produces a branch-tracked verification/delivery record before Final admission, then independent Final approvals for that exact frozen commit. It does not broaden scope or introduce a new completion protocol abstraction.

- [ ] **Step 1: Run final removal assertions before expensive suites**

```powershell
$banned = "restart_legacy_workflow|CompletionProtocolRolloutConfig|CompletionProtocolSelection(Source)?|select_completion_protocol|get_completion_protocol_settings|legacy_completion_protocol_restart_(required|invalid|not_required)|LegacyCompletionProtocolRestart|successor_conversation_id|record_completion_restart|CompletionRestartOutcome|shadow_differences|rollout_windows|rollout_decisions"
$hits = rg -n $banned src-tauri/src src-tauri/tests src
if ($LASTEXITCODE -eq 0) { $hits; throw "v2-only removal assertion failed" }
if ($LASTEXITCODE -ne 1) { exit $LASTEXITCODE }
```

Expected: no matches.

- [ ] **Step 2: Run all frontend verification**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: ESLint exits 0, the complete Vitest suite passes, and Next.js static export completes successfully.

- [ ] **Step 3: Run the full desktop Rust verification**

```powershell
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils
cargo test --manifest-path src-tauri/Cargo.toml --features test-utils
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings
```

Expected: all four commands exit 0 with no Clippy warnings.

- [ ] **Step 4: Run the full server Rust verification**

```powershell
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib
cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib -- -D warnings
```

Expected: server check, tests, and Clippy all exit 0.

- [ ] **Step 5: Run the full MCP companion verification**

```powershell
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp
cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: both commands exit 0 with no warnings.

- [ ] **Step 6: Review the complete diff for scope and invariant coverage**

```powershell
git status --short
git diff --check
$task1Commit = git log --format=%H --grep="feat: define v2-only completion protocol guard" -1
if (-not $task1Commit) { throw "Task 1 producer commit is missing" }
$implementationBase = git rev-parse "$task1Commit^"
git diff --stat "$implementationBase..HEAD"
git log --oneline --decorate -12
```

Expected: no whitespace errors, no generated build output, no unrelated files, and one reviewed commit sequence per task. Confirm old migration files and historical data structures were not rewritten or deleted.

- [ ] **Step 7: Write and commit delivery evidence before Final admission**

Create `.superpowers/sdd/completion-protocol-v2-only-delivery-report.md` containing the implementation commit range, exact command outcomes from Steps 1-5, complete-diff scope result from Step 6, acceptance-criteria checklist, explicit D-CODEX-M3/D-CODEX-M4/D-GROK-M1 results, and the reviewer input manifest. State that Final reviewer verdicts are authoritative only in their platform reports/cards because those outcomes are produced after the branch is frozen and are not committed afterward. Then:

```powershell
git add -f .superpowers/sdd/completion-protocol-v2-only-delivery-report.md
git commit -m "docs: record completion protocol v2-only delivery"
```

Expected: the delivery report is part of `HEAD` before either Final reviewer is admitted.

- [ ] **Step 8: Freeze the candidate HEAD and obtain both Final reviews**

Resolve the exact candidate only after the evidence commit and reject admission unless the full worktree is empty, including untracked files:

```powershell
$worktreeState = @(git status --porcelain)
if ($worktreeState.Count -gt 0) {
    $worktreeState
    throw "Final candidate worktree is not clean"
}
$finalCandidateHead = git rev-parse HEAD
$finalCandidateHead
```

Give both independent Final reviewers `$finalCandidateHead`, the approved design digest, this plan, the complete implementation range, the committed delivery report, all verification output, and the Task Routing Matrix. Require both to inspect protocol construction, mutation fences, admission/terminal concurrency, migration triggers, historical reads and deletion, removed surfaces, retained root tools/root wake/v2 semantic channels, standalone behavior, and all three residual design minors.

Both platform review reports/cards must name and approve the same `$finalCandidateHead`. If either reviewer requests changes, that candidate is rejected: add a failing regression test, make the smallest owning-module fix, rerun the focused task command and every affected command in Steps 1-6, update the delivery report, commit all changes, resolve a new candidate hash, and re-admit both reviewers from scratch. Once both approve, make no further commit or branch-tracked edit; their external platform reports/cards are the authoritative post-freeze review evidence.

## Post-Delivery Residual Work

Human UAT is intentionally deferred until after all automated tasks and final reviews:

1. Start desktop and server processes separately with each removed environment variable present and confirm the operator-facing removal message; confirm server exit code `2`.
2. Open a backup containing a historical `(1,v1)` workflow and a historical `(1,v2_shadow)` workflow. Confirm transcript/graph/Card/link navigation works, the read-only notice is visible, and no restart/resume/settle/recovery control is present.
3. Create a new workflow from each supported agent/profile combination, inspect the persisted pair as `(2,v2_enforce)`, and exercise `complete_work`, conclusion-line, bounded-report, ambiguity, and user-adjudication paths.
4. Run a standalone delegation without a workflow binding and confirm its display Card remains available.
5. Observe production metrics after release to confirm v2 intent/evidence/attention counters and attention-driven root wake continue, with no rollout/shadow/restart fields.

No data backfill, v1 conversion, successor creation, or removal of historical restart tables is part of residual work.
