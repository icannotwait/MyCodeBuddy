### Task 1: Persist immutable optional orchestration bindings

**Dependencies:** The completed 2026-08-16 routing increment supplies canonical Task keys and route metadata. This Task introduces only generic durable identity; no caller can send a binding until Task 2.

**Risk:** `high` because `migration_destructive_persistence` is active for the existing `delegation_task_runs` schema. Broad production surface, multiple ownership modules, and shared interfaces total 3; the hard trigger independently forces high.

**Files:**

- Create: `src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/delegation_task_run.rs`
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs` for reserving literals and `None` at all current request-fingerprint call sites until Task 2 exposes the field
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/attention.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` only for the three legacy `delegation_task_run::Model` literal expressions; Task 6 retains ownership of later warning logic
- Create: `src-tauri/tests/fixtures/orchestration_binding_v1.json`
- Modify: `src-tauri/tests/completion_protocol_v2.rs`
- Modify: `src-tauri/tests/completion_transport_parity.rs`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Test: inline `#[cfg(test)]` modules in the migration, `types.rs`, and `run_store.rs`, the shared JSON corpus, and compile coverage for every listed literal owner
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-report.md` (do not stage)

**Interfaces:**

- Consumes: existing `delegation_task_run::Model`, `ReservingRunInsert`, `PersistedRun`, `insert_reserving_txn`, lifecycle update methods, and the seven-string `request_fingerprint` behavior.
- Produces: `OrchestrationBindingV1`, `Option<OrchestrationBindingV1>` on reserving/persisted run values, the four exact database columns/triggers/index, and `request_fingerprint(tool_name: &str, task_text: &str, work_unit_key: Option<&str>, replaces_task_id: Option<&str>, replacement_reason: Option<&str>, target_task_id: Option<&str>, route_fingerprint_hex: &str, orchestration_binding: Option<&OrchestrationBindingV1>) -> String` with backward-compatible unbound bytes.
- Invariant for later Tasks: `DelegationRequest` and `ContinueDelegationRequest` do not yet expose the field; every currently admitted call passes `None` and behaves byte-for-byte as before.

- [ ] **Step 1: Write focused migration and value-object tests**

Add tests named with the prefix `delegation_orchestration_bindings_`. The first test opens through the prior migration, inserts a legacy run, applies the new migration, and proves all four new values remain SQL `NULL`. The remaining tests prove exact column types/nullability, exact `idx_dtr_parent_orchestration_created_task` column order, all-null/all-set acceptance, partial insert rejection by `trg_dtr_orchestration_binding_shape`, post-insert add/change/clear rejection by `trg_dtr_orchestration_binding_immutable`, and a status-only update succeeding without changing the binding.

Create `src-tauri/tests/fixtures/orchestration_binding_v1.json` as the only cross-language binding grammar corpus. Its exact top-level shape is `{ "schema_version": 1, "cases": [{ "name": STRING, "valid": BOOLEAN, "value": JSON }] }`, with no other top-level or case keys and unique names. Valid cases are `minimum` (`namespace: "a"`, generation 1, 64 lowercase zero hex), `maximum` (`namespace: "a123456789012345678901234567890123456789012345678901234567890123"`, generation 4294967295, 64 lowercase `f` hex), and `brainstorm_to_delivery` (the exact workflow namespace and published lowercase Design fingerprint). Invalid cases are named `null`, `non_object`, `missing_schema_version`, `missing_namespace`, `missing_generation`, `missing_route_fingerprint`, `extra_field`, `wrong_schema_version`, `schema_version_string`, `namespace_number`, `generation_string`, `fingerprint_number`, `generation_zero`, `generation_overflow`, `namespace_empty`, `namespace_65_bytes`, `namespace_uppercase`, `namespace_underscore`, `fingerprint_uppercase_hex`, `fingerprint_wrong_length`, and `fingerprint_wrong_prefix`. Give each invalid case exactly the single named defect relative to the valid minimum object, except the four missing-field cases and `extra_field` whose names define their exact structural mutation.

Load that file with `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/orchestration_binding_v1.json"))` in the `OrchestrationBindingV1` tests. Assert corpus schema/name uniqueness before iterating it, then assert every valid value deserializes and passes semantic validation while every invalid value fails. Tasks 2 and 4 must consume this same file; they must not duplicate the grammar vectors in Rust or JavaScript tables.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrchestrationBindingV1 {
    pub schema_version: u32,
    pub namespace: String,
    pub generation: u32,
    pub route_fingerprint: String,
}

#[test]
fn delegation_orchestration_bindings_reject_partial_insert_and_every_update() {
    // Execute raw INSERT/UPDATE statements against a fully migrated in-memory DB.
    // Assert the two exact trigger names in each SQLite error.
}
```

- [ ] **Step 2: Run the migration/value tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib delegation_orchestration_bindings_ -- --nocapture
```

Expected: at least one test executes and fails because the four columns, migration registration, triggers, index, and strict Rust value object do not exist.

- [ ] **Step 3: Implement the no-backfill migration and SeaORM identity fields**

Register the new migration immediately after `m20260811_000001_simple_workflows`. Its `up` path executes the following contract in order: add four nullable columns, create the shape trigger, create the immutable-update trigger, then create `idx_dtr_parent_orchestration_created_task` with the exact ordered columns `(parent_conversation_id, orchestration_namespace, created_at, task_id)`. The shape trigger accepts exactly zero or four non-null values. The immutable trigger uses SQLite `IS NOT` comparisons so null-to-value, value-to-null, and value-to-different-value all abort. Its `down` path drops the named index and triggers before dropping the four columns.

```sql
CREATE TRIGGER trg_dtr_orchestration_binding_shape
BEFORE INSERT ON delegation_task_runs
WHEN (NEW.orchestration_schema_version IS NOT NULL) +
     (NEW.orchestration_namespace IS NOT NULL) +
     (NEW.orchestration_generation IS NOT NULL) +
     (NEW.orchestration_route_fingerprint IS NOT NULL) NOT IN (0, 4)
BEGIN
  SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_shape');
END;

CREATE TRIGGER trg_dtr_orchestration_binding_immutable
BEFORE UPDATE OF orchestration_schema_version,
                 orchestration_namespace,
                 orchestration_generation,
                 orchestration_route_fingerprint
ON delegation_task_runs
WHEN OLD.orchestration_schema_version IS NOT NEW.orchestration_schema_version
  OR OLD.orchestration_namespace IS NOT NEW.orchestration_namespace
  OR OLD.orchestration_generation IS NOT NEW.orchestration_generation
  OR OLD.orchestration_route_fingerprint IS NOT NEW.orchestration_route_fingerprint
BEGIN
  SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_immutable');
END;
```

Mirror the four nullable fields in `delegation_task_run::Model`. Do not add defaults, guessed values, or a data update. Every existing legacy literal expression must explicitly initialize all four fields to `None`:

```rust
orchestration_schema_version: None,
orchestration_namespace: None,
orchestration_generation: None,
orchestration_route_fingerprint: None,
```

- [ ] **Step 4: Write focused store and fingerprint tests**

Add `durable_binding_` run-store tests that prove:

- `ReservingRunInsert` writes a valid binding atomically with `status = reserving` and `PersistedRun` reconstructs it;
- an injected/forced insert transaction error leaves no run and no partial columns;
- all existing promote, status, terminal-settle, and runtime-stat update paths leave the four columns byte-for-byte unchanged;
- direct SQL attempts to mutate the binding fail while ordinary lifecycle changes succeed;
- the existing unbound seven-string test vectors retain their exact digests;
- a bound call hashes the exact 12-string v2 array, different generation/fingerprint values separate, and an exact retry matches.

Add a separate `durable_binding_lifecycle_identity_` fault-injection matrix. Seed a bound reserving row with non-default `agent_type: "custom:binding-fixture"` and `profile_id: "profile-binding-fixture"`. Exercise reserving promotion, pre-admission terminalization, normal terminal settlement, cancellation/cleanup, runtime-stat writes, and completion/projection updates with each path's existing one-shot transaction fault; add a focused test-only post-write failure hook only where no fault seam exists. After both the forced rollback and a successful retry, raw-select and byte-compare `agent_type`, `profile_id`, and all four orchestration columns to the original insert. Name the tests with that exact prefix and keep the hooks under `#[cfg(any(test, feature = "test-utils"))]`.

Use this branch in the fingerprint implementation; do not alter the unbound array:

```rust
let fields = match orchestration_binding {
    None => vec![
        tool_name.to_owned(),
        task_nfc,
        work_unit_key.unwrap_or("").to_owned(),
        replaces_task_id.unwrap_or("").to_owned(),
        replacement_reason.unwrap_or("").to_owned(),
        target_task_id.unwrap_or("").to_owned(),
        route.to_owned(),
    ],
    Some(binding) => vec![
        "delegation-request-v2".to_owned(),
        tool_name.to_owned(),
        task_nfc,
        work_unit_key.unwrap_or("").to_owned(),
        replaces_task_id.unwrap_or("").to_owned(),
        replacement_reason.unwrap_or("").to_owned(),
        target_task_id.unwrap_or("").to_owned(),
        route.to_owned(),
        binding.schema_version.to_string(),
        binding.namespace.clone(),
        binding.generation.to_string(),
        binding.route_fingerprint.clone(),
    ],
};
```

The bound vector has the v2 domain tag plus the existing seven positions plus four binding strings, for 12 total strings as shown in the approved Design. Name the test so this count cannot silently regress.

- [ ] **Step 5: Run the store tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib
```

Expected: FAIL for the intended missing binding fields/fingerprint branch; this unfiltered RED command avoids a zero-test filtered result if the new API first fails compilation.

- [ ] **Step 6: Persist the binding in the reserving insert and map it back out**

Add one optional binding field to `ReservingRunInsert` and `PersistedRun`. In `insert_reserving_txn`, populate all four ActiveModel fields from one validated `Option`; in `model_to_persisted_run`, accept either all-null or all-set and reject an impossible partial row as unreadable. Do not touch the binding columns in `promote_running`, `settle_pre_admission_failure_if_owned`, `settle_terminal`, cancellation, runtime-stat, completion, or projection updates.

Update every existing compatibility literal rather than assuming Rust supplies an omitted optional field. The revision scan found `ReservingRunInsert` literals in exactly these files: `connection.rs`, `attention.rs`, `broker.rs`, `listener.rs`, `run_store.rs`, `store.rs`, `workflow/admission.rs`, `workflow/completion_evidence.rs`, `workflow/recovery_tests.rs`, `workflow/store.rs`, `completion_protocol_v2.rs`, `completion_transport_parity.rs`, and `delegation_session_reuse_integration.rs`. Add `orchestration_binding: None` to every old literal and reserve non-null values for the new focused tests.

The same scan found `request_fingerprint` calls in exactly `broker.rs`, `run_store.rs`, `store.rs`, and `delegation_session_reuse_integration.rs`. Add a final `None` to every old call. This is only source compatibility for the new function signature; Task 2 replaces broker admission values with the effective request/source binding. Re-run both scans before GREEN and fail the Task report checklist if any literal or call remains outside these owned files.

`PersistedRun` literals occur only in `broker.rs` and `run_store.rs`; add `orchestration_binding: None` to legacy test literals while `model_to_persisted_run` supplies the real mapped value. Include this third scan in the same pre-GREEN checklist.

Run and record a fourth, complete SeaORM Model-literal scan before GREEN:

```bash
rg -n -U 'delegation_task_run::Model\s*\{' src-tauri/src src-tauri/tests
rg -n 'delegation_task_run::\{[^}]*Model|delegation_task_run::Model[[:space:]]+as|type[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*.*delegation_task_run::Model' src-tauri/src src-tauri/tests
```

At this Plan baseline, the qualified scan has six textual matches. Three are actual literal expressions in `workflow/project.rs` (`finished_a`, `finished_b`, and `open_c`); explicitly add the four `None` fields to all three, including the two struct-update expressions rather than relying on inheritance. The other three matches in `listener.rs`, `run_store.rs`, and `workflow/completion_evidence.rs` are function return types whose following brace opens the function body, not Model constructors. The alias/import scan finds no alias of `delegation_task_run::Model` and therefore no unqualified literal owner. Inspect and classify every match in the Task report; if the branch changes before implementation, add every newly discovered literal owner to this Task and its commit before running GREEN.

- [ ] **Step 7: Run Task 1 GREEN and shared-core checks**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib delegation_orchestration_bindings_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib durable_binding_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib durable_binding_lifecycle_identity_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib
cargo check --no-default-features --features server,test-utils --tests
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: each filtered command reports at least one executed test and PASS; the full library passes with every direct SeaORM Model literal naming the four nullable fields; every integration-test target containing a compatibility literal compiles; the shared library, server binary, and MCP companion compile without `tauri-runtime`.

- [ ] **Step 8: Commit Task 1**

```bash
git add -- src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/delegation_task_run.rs src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/attention.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/delegation/workflow/admission.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/workflow/recovery_tests.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/src/acp/delegation/workflow/project.rs src-tauri/tests/fixtures/orchestration_binding_v1.json src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/completion_transport_parity.rs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(delegation): persist orchestration bindings"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-1-report.md` with migration SQL facts, no-backfill evidence, trigger/index results, shared-corpus counts, all four compatibility scans including the classified SeaORM Model scan, unbound and bound fingerprint vectors, lifecycle identity fault/rollback results, exact test counts/outcomes, commit hash, and retained concerns. Do not stage it.

---

