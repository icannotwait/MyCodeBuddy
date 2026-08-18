# Task 1 Report: Durable Orchestration Binding Persistence

## Result

- Status: DONE
- Commit: `457f536cb4c1731098f62752650aa54ebefeaf76`
- Commit subject: `feat(delegation): persist orchestration bindings`
- Scope: Task 1 only. `DelegationRequest` and `ContinueDelegationRequest` do not expose `orchestration_binding`.

## RED Evidence

- Step 2 command: `cargo test --no-default-features --features server,test-utils --lib delegation_orchestration_bindings_ -- --nocapture`
  - 5 tests executed and the command failed as intended before the migration/value implementation existed.
  - Failures identified the missing registered migration, columns/index/triggers, no-backfill behavior, and strict value validation.
- Step 5 command: `cargo test --no-default-features --features server,test-utils --lib`
  - Compilation failed as intended before test execution because the binding fields and extended fingerprint API did not exist.
  - This command was deliberately unfiltered, so the compile-time RED did not create a zero-test filtered success.

## Migration Contract

- Registered `m20260817_000001_delegation_orchestration_bindings` immediately after `m20260811_000001_simple_workflows`.
- Added exactly four nullable columns to `delegation_task_runs`:
  - `orchestration_schema_version INTEGER NULL`
  - `orchestration_namespace TEXT NULL`
  - `orchestration_generation INTEGER NULL`
  - `orchestration_route_fingerprint TEXT NULL`
- The migration performs no `UPDATE`, default assignment, guessed binding, or other backfill. A row inserted through the prior migration retained four SQL `NULL` values after upgrade.
- Created `trg_dtr_orchestration_binding_shape` after the columns. All-null and all-set inserts succeeded; a partial insert failed with the exact trigger name.
- Created `trg_dtr_orchestration_binding_immutable` with SQLite `IS NOT` comparisons. Post-insert add, change, and clear attempts failed with the exact trigger name. A status-only update succeeded and left all binding bytes unchanged.
- Created `idx_dtr_parent_orchestration_created_task` last, with exact order `(parent_conversation_id, orchestration_namespace, created_at, task_id)`.
- Down migration removes the index and both triggers before dropping the four columns.

## Value And Persistence Contract

- Added strict `OrchestrationBindingV1` with `deny_unknown_fields`, schema version `1`, lowercase `[a-z][a-z0-9-]{0,63}` namespace semantics, generation `1..=u32::MAX`, and lowercase `sha256:` plus 64 hex digits.
- The shared JSON corpus has the exact top-level/case shapes, 24 unique cases, 3 valid cases, and 21 invalid cases. All cases matched their expected acceptance result.
- `ReservingRunInsert` validates one optional binding and writes all four columns in the reserving transaction.
- `PersistedRun` reconstructs all-set rows, preserves all-null legacy rows as `None`, and treats impossible partial rows as unreadable.
- A forced insert failure left no durable run and therefore no partial binding columns.
- Lifecycle update models do not write any orchestration column.

## Fingerprint Compatibility

- Unbound requests retain the existing seven-string canonical JSON array exactly.
- Delegate vector: `55687507f1ed929a92190fb1e1039e422dd219d2238a4b1e10a6968c32e557f4`.
- Continue vector: `f9487ae94c8b94155514942226be54829c3f5043fdf587d3c33886b01f04a97f`.
- Bound requests use the exact 12-string v2 array: domain tag, the legacy seven positions, then the four binding strings.
- Bound vector: `aca47c464009a8f26bd36e0611b17f62cb7ed7942a387e38e878cf87087ff172`.
- Exact retries matched; changed generation and changed route fingerprint both separated.

## Lifecycle Identity Matrix

The focused matrix seeded `agent_type = "custom:binding-fixture"`, `profile_id = "profile-binding-fixture"`, and a non-null binding. It byte-compared those two identity values plus all four orchestration columns after a forced rollback and after a successful retry for each path:

- reserving promotion, using the existing transient after-claim fault rail;
- pre-admission terminalization, using a focused test-only post-write fault;
- normal terminal settlement, using the existing terminal transaction fault;
- cancellation/cleanup reconciliation, using the existing terminal transaction fault;
- runtime-stat writes, using a focused test-only post-write fault;
- completion/projection settlement, using the existing terminal transaction fault with final runtime stats.

Every rollback and successful retry preserved the original insert-fixed identity bytes.

## Compatibility Scans

- `ReservingRunInsert`: 52 textual matches across exactly the 13 expected owner files: `connection.rs`, `attention.rs`, `broker.rs`, `listener.rs`, `run_store.rs`, `store.rs`, `workflow/admission.rs`, `workflow/completion_evidence.rs`, `workflow/recovery_tests.rs`, `workflow/store.rs`, `completion_protocol_v2.rs`, `completion_transport_parity.rs`, and `delegation_session_reuse_integration.rs`. The scan includes declarations/return signatures; every legacy constructor explicitly supplies `orchestration_binding: None`, and focused tests alone use `Some`.
- `request_fingerprint`: 33 textual matches across exactly `broker.rs`, `run_store.rs`, `store.rs`, and `delegation_session_reuse_integration.rs`. The scan includes the definition; every legacy call supplies the eighth `None` argument.
- `PersistedRun`: 7 textual matches across exactly `broker.rs` and `run_store.rs`. The actual literals are one in `broker.rs` and two in `run_store.rs`; legacy literals use `None`, while `model_to_persisted_run` supplies the mapped durable value.
- Qualified `delegation_task_run::Model`: 6 matches. The matches in `listener.rs`, `run_store.rs`, and `workflow/completion_evidence.rs` are function return types. The three actual literals (`finished_a`, `finished_b`, and `open_c`) are in `workflow/project.rs` and explicitly initialize all four columns to `None`, including both struct-update expressions. The alias/import scan returned 0 matches.
- DTO audit: neither `DelegationRequest` nor `ContinueDelegationRequest` contains `orchestration_binding`.

## Historical Fixture Compatibility

Historical completion-protocol tests intentionally create a schema before later migrations but still exercise current run-store writes. A `#[cfg(test)]` helper applies this independent Task 1 migration out of order and records its migration version so later fixture advancement cannot apply it twice. Only historical fixtures in Task 1-owned files call the helper; production migration behavior is unchanged. The previously failing historical completion evidence, broker, and listener paths pass in the full library run.

## GREEN Evidence

All commands ran from `src-tauri/` and used `--no-default-features --features server,test-utils`:

- `cargo test ... --lib delegation_orchestration_bindings_ -- --nocapture`: 5 passed, 0 failed, 4620 filtered out.
- `cargo test ... --lib durable_binding_ -- --nocapture`: 6 passed, 0 failed, 4619 filtered out.
- `cargo test ... --lib durable_binding_lifecycle_identity_ -- --nocapture`: 1 passed, 0 failed, 4624 filtered out.
- `cargo test ... --lib`: 4624 passed, 0 failed, 1 ignored, 4625 total.
- `cargo check ... --tests`: passed.
- `cargo check ... --lib --bin codeg-server --bin codeg-mcp`: passed.
- `git diff --check` and staged `git diff --cached --check`: passed.
- Independent code review found no Critical or Important issues; the reviewer reran the 5 migration/value tests, the 6 durable binding tests, and the integration-target check successfully.

## Concerns

- Non-blocking environment warning: macOS `ld` reported that the `__eh_frame` section was too large for compact unwind offsets while linking the large library test binary. Tests still linked and passed; both `cargo check` commands completed without warnings.
