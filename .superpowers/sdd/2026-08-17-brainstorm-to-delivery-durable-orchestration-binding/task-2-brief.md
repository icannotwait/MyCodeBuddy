### Task 2: Enforce binding transport and lineage admission

**Dependencies:** Task 1 provides the validated binding type, immutable columns, reserving persistence, and fingerprint branch. This Task is the only writer-facing transport/admission switch.

**Risk:** `high` because both `concurrency_lifecycle` and `public_compatibility` hard triggers are active. Cross-process transport, broad production surface, multiple ownership modules, and a shared request interface total 5; either hard trigger independently forces high.

**Files:**

- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/store.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- Modify: `src-tauri/tests/completion_protocol_v2.rs`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Test fixture: `src-tauri/tests/fixtures/orchestration_binding_v1.json` from Task 1, loaded unchanged by schema, listener, and semantic validation tests
- Test: inline Rust unit/integration-style tests in the same files plus compile coverage for every request/admission literal owner
- Report: `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-2-report.md` (do not stage)

**Interfaces:**

- Consumes: `OrchestrationBindingV1`, `ReservingRunInsert::orchestration_binding`, `PersistedRun::orchestration_binding`, and the v1/v2 fingerprint function from Task 1.
- Produces: optional `orchestration_binding` on `DelegationRequest` and `ContinueDelegationRequest`; listener parser `parse_orchestration_binding`; `TaskStoreError::OrchestrationBindingLineageMismatch`; `DelegationError::{OrchestrationBindingInvalid, OrchestrationBindingLineageMismatch}`; exact inheritance for continuation/replacement.
- Ordering guarantee for Task 3: a reserving row's actual Agent/profile and effective binding are final before the query can observe it.

- [ ] **Step 1: Write raw-schema and listener RED tests**

Load every case from `src-tauri/tests/fixtures/orchestration_binding_v1.json`; do not create another transport grammar table. For each case, inject its `value` into raw `delegate_to_agent` and `continue_delegation` arguments and validate the same candidate against both published MCP input schemas. Every shared valid case must pass schema, listener deserialization, and `OrchestrationBindingV1` semantic validation. Every shared invalid case must fail all three with exactly `orchestration_binding_invalid`, and the mock spawner/resumer must record zero child side effects. Test omitted `orchestration_binding` separately as the backward-compatible `None` case; explicit JSON null remains the shared invalid `null` case.

```rust
let input = json!({
    "agent_type": "grok",
    "task": "bound first dispatch",
    "correlation_id": "binding-listener-red",
    "orchestration_binding": {
        "schema_version": 1,
        "namespace": "brainstorm-to-delivery",
        "generation": 1,
        "route_fingerprint": format!("sha256:{}", "a".repeat(64))
    }
});
```

- [ ] **Step 2: Run parser/schema tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture
```

Expected: at least one test executes and fails because the listener currently ignores the field and the MCP schemas do not publish it.

- [ ] **Step 3: Expose strict optional binding inputs and stable errors**

Add the same `$defs`-style object shape to both delegation tool schemas with `additionalProperties: false`, four required fields, exact integer/string limits, and no JSON `null` alternative. Keep the containing request optional so omitted old calls remain valid.

Parse the raw field before depth, recovery, child, or resume work. `parse_orchestration_binding` returns `Ok(None)` only when the property is absent; every present non-object or invalid object maps to `orchestration_binding_invalid`. Validate again at direct broker entry so non-listener callers cannot bypass the contract.

Map the two new `DelegationError` variants in `DelegationTaskReport::from_err` to the exact stable codes, and map the store mismatch variant through `store_err_to_delegation_error` without collapsing it into `not_continuable` or `invalid_replacement`.

- [ ] **Step 4: Write first/continue/replacement lineage RED tests**

Add broker/run-store tests for this complete matrix:

- first dispatch: omitted remains unbound; valid binding persists before spawn; invalid direct-broker binding rejects before depth/spawn;
- continue from bound source: omitted inherits; exact explicit match succeeds; changed one of any four fields rejects;
- continue from unbound source: omitted stays unbound; a supplied binding rejects conversion;
- replacement from bound source: omitted and exact supplied values create a generation-1 replacement with the exact source binding;
- replacement from unbound source: omitted stays unbound; supplied binding rejects conversion;
- every mismatch occurs before child allocation/resume, replacement eligibility changes, recovery authorization consumption, counter preflight/charge, or process spawn;
- inherited binding participates in continue/replacement idempotency even when omitted by the caller;
- different effective bindings cannot alias under one parent tool use ID.

Use test gates/counters already present in broker and run store. Assert the rejected replacement leaves no provisional child conversation and its authorization remains consumable by the subsequent exact call.

- [ ] **Step 5: Run lineage tests and observe RED**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
```

Expected: at least one test executes and fails because continuation/replacement currently neither compare nor inherit orchestration identity.

- [ ] **Step 6: Resolve one effective binding before every side effect**

For first dispatch, validate the supplied value at broker entry and pass it into both `request_fingerprint` and `ReservingRunInsert`.

For continuation, load the source for ownership, compute `effective = source` when omitted or require exact equality when supplied, use that effective value in the request fingerprint, and pass the supplied/effective values into `ContinueRunAdmission`. Under the existing writer transaction, reload the source and repeat the comparison before continuability, recovery authorization, budget preflight, and insert; copy the source binding into the new insert.

For replacement, load and compare source identity before provisional child creation. Under `validate_replacement_insert_txn`, repeat equality before recovery eligibility/authorization/budget checks and overwrite the new insert with the source binding. This transaction check is the race backstop even though the database trigger already makes the source immutable.

```rust
fn inherited_binding(
    source: Option<&OrchestrationBindingV1>,
    supplied: Option<&OrchestrationBindingV1>,
) -> Result<Option<OrchestrationBindingV1>, TaskStoreError> {
    match (source, supplied) {
        (Some(source), None) => Ok(Some(source.clone())),
        (Some(source), Some(value)) if source == value => Ok(Some(source.clone())),
        (None, None) => Ok(None),
        _ => Err(TaskStoreError::OrchestrationBindingLineageMismatch),
    }
}
```

Update every existing Rust literal explicitly. The revision's word-boundary scan found `DelegationRequest` or `ContinueDelegationRequest` literals in exactly `connection.rs`, `broker.rs`, `listener.rs`, `run_store.rs`, `workflow/recovery_tests.rs`, `lifecycle.rs`, `completion_protocol_v2.rs`, and `delegation_session_reuse_integration.rs`; add `orchestration_binding: None` to legacy literals and exact values only to focused binding tests. The scan also found `ContinueRunAdmission` literals in exactly `broker.rs`, `run_store.rs`, and `workflow/completion_evidence.rs`; add the new supplied/effective binding fields to each. `types.rs` owns the two request definitions. Re-run both scans before GREEN and record their complete file sets in the Task report.

- [ ] **Step 7: Run Task 2 GREEN and compatibility checks**

From `src-tauri/`:

```bash
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib request_fingerprint_ -- --nocapture
cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture
cargo test --no-default-features --features server,test-utils --lib
cargo check --no-default-features --features server,test-utils --tests
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
```

Expected: every filter executes at least one test and passes; the unchanged `7_680` assertion still passes after nested binding schema growth; the full library passes; all request/admission literal integration targets compile; old omitted-binding request/fingerprint cases remain unchanged; server and companion compile without desktop defaults.

- [ ] **Step 8: Commit Task 2**

```bash
git add -- src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/store.rs src-tauri/src/acp/delegation/run_store.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/workflow/completion_evidence.rs src-tauri/src/acp/delegation/workflow/recovery_tests.rs src-tauri/tests/completion_protocol_v2.rs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(delegation): enforce binding lineage"
```

- [ ] **Step 9: Write the Task report**

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-2-report.md` with shared-corpus schema/listener results, both literal-scan file sets, side-effect counters, continuation/replacement inheritance evidence, fingerprint compatibility, printed Grok JSONL byte count with unchanged `7_680`/`7680` contract, exact commands/counts, commit hash, and retained concerns. Do not stage it.

---

