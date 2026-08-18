# Task 1 Primary Review

## Verdict

**APPROVED**

Counts: **0 Critical, 0 Important, 0 Minor**.

- Spec compliance: **PASS**
- Task quality: **PASS**

## Findings

### Critical

None.

### Important

None.

### Minor

None.

## Spec Compliance

The reviewed range `db8c14c3..457f536c` satisfies Task 1's binding
constraints:

- The migration adds exactly four nullable orchestration columns without a
  default or backfill, registers immediately after the Simple migration, and
  creates the required named shape trigger, immutability trigger, and ordered
  lookup index. The down path removes the index and triggers before the
  columns.
- `OrchestrationBindingV1` enforces the required v1 schema, namespace,
  generation, fingerprint, and unknown-field rules. Its tests load the single
  shared 24-case JSON corpus and verify its exact container/case shapes and
  unique required names.
- `ReservingRunInsert` writes a validated optional binding as four fields in
  the reserving transaction. `PersistedRun` reconstructs all-set rows,
  preserves all-null legacy rows, and rejects partial or semantically invalid
  stored bindings as unreadable.
- The binding columns occur only in the insert and read mapping; lifecycle
  update models do not write them. The fault matrix byte-compares
  `agent_type`, `profile_id`, and all four orchestration columns across
  rollback and retry for promotion, pre-admission settlement, terminal
  settlement, cleanup, runtime statistics, and final projection.
- The unbound fingerprint branch retains the legacy seven-string JSON array
  and exact published digests. The bound branch matches the Design's exact
  12-string v2 array and separates generation and route-fingerprint changes.
- Every current production fingerprint admission call passes `None`, and
  legacy reserving/persisted literals explicitly initialize the new optional
  value. Neither `DelegationRequest` nor `ContinueDelegationRequest` exposes
  `orchestration_binding` in this Task.
- The range changes `workflow/project.rs` only to update the three specified
  model literals. It adds no Simple manifest, platform Gate, or Task 6 warning
  behavior.

The producer's RED chronology and full 4,624-test library result are recorded
in `task-1-report.md`; the final range cannot independently prove chronology,
and the full library suite was intentionally not rerun under the primary
review brief. This is not a compliance blocker because the focused behavior
and required compile surfaces were independently verified below.

## Verification

Fresh reviewer verification, all with
`--no-default-features --features server,test-utils`:

- `cargo test --lib delegation_orchestration_bindings_ -- --nocapture`:
  **5 passed, 0 failed**.
- `cargo test --lib durable_binding_ -- --nocapture`:
  **6 passed, 0 failed**.
- `cargo test --lib durable_binding_lifecycle_identity_ -- --nocapture`:
  **1 passed, 0 failed**.
- `cargo check --tests`: **passed**.
- `cargo check --lib --bin codeg-server --bin codeg-mcp`: **passed**.
- `git diff --check db8c14c3..457f536c`: **passed**.

The focused library tests emitted only the producer-reported macOS linker
warning about the large `__eh_frame` compact-unwind table; it did not affect
linking or execution.

## Assessment

The implementation is narrowly scoped, preserves all required compatibility
surfaces, and supplies proportionate migration, persistence, fingerprint, and
fault-injection coverage for this high-risk Task. No changes are required
before Task 2 consumes the durable binding interface.
