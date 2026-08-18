# Task 2 Primary Review

## Verdict

**CHANGES REQUESTED**

Counts: **0 Critical, 1 Important, 0 Minor**.

- Spec compliance: **FAIL (required verification coverage incomplete)**
- Task quality: **CHANGES REQUESTED**

## Findings

### Critical

None.

### Important

1. **The required continuation mismatch side-effect proof is missing.**
   Task 2 explicitly requires a lineage mismatch to be exercised through the
   broker before resume, recovery-authorization consumption, budget work, or
   any other child/process side effect (`task-2-brief.md:70-81`). The focused
   broker tests cover first dispatch and replacement only
   (`broker.rs:16668`, `broker.rs:16749`). Continuation is tested only by
   calling `RunStore::admit_continue_reserving` and checking that no row was
   inserted (`run_store.rs:6284-6381`), which cannot detect a broker regression
   that resumes the child before store admission and does not prove a supplied
   continuation authorization remains usable after rejection. Add a
   broker-level bound/unbound continuation mismatch test that asserts the
   exact `orchestration_binding_lineage_mismatch` code, unchanged
   `MockSpawner::resume_args`/spawn counters, no new run, and successful reuse
   of the same authorization by the subsequent exact or omitted-binding call.

### Minor

None.

## Spec Compliance

The production implementation otherwise matches the reviewed Task 2 contract:

- Both delegation request DTOs carry an optional binding, both published MCP
  schemas use the same strict all-or-none v1 object, and listener parsing
  distinguishes omission from every present invalid value, including JSON
  `null`.
- Direct broker entry validates malformed bindings before setup work and maps
  the two new stable wire codes distinctly.
- First dispatch fingerprints and reserves the supplied binding; unbound
  requests retain the legacy seven-string request fingerprint.
- Continuation resolves the source binding before completion/capability/
  admission work, fingerprints the effective value, and repeats equality in
  the writer transaction before continuability, recovery authorization,
  budget preflight, and insertion.
- Replacement resolves lineage before provisional child creation and repeats
  the comparison in `validate_replacement_insert_txn`, which overwrites the
  insert with the source binding before eligibility, authorization, budget,
  and insert work.
- Request/admission literal ownership scans match the Task brief after
  excluding the DTO definitions in `types.rs`. The range adds no query tool
  and no desktop-only production behavior.
- The unchanged shared fixture is consumed by schema, listener, and semantic
  tests. The fixed Grok tools/list assertion remains `7_680`/`7680` and the
  measured JSONL line is 7,669 bytes.

The missing broker continuation test is material for this high-risk Task's
`concurrency_lifecycle` trigger. The code ordering currently appears correct,
but the deliverable does not provide the required regression evidence across
the broker/resumer boundary.

## Verification

Fresh reviewer verification, all without default features or
`tauri-runtime`:

- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture`:
  **2 passed, 0 failed**.
- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture`:
  **4 passed, 0 failed**.
- `cargo test --no-default-features --features server,test-utils --lib request_fingerprint_ -- --nocapture`:
  **2 passed, 0 failed**.
- Exact Grok tools/list budget test: **1 passed, 0 failed**, printed **7,669
  bytes**.
- `cargo check --no-default-features --features server,test-utils --tests`:
  **passed**.
- `cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp`:
  **passed**.
- `git diff --check 457f536c..43c63745`: **passed**.

The focused test links emitted the already reported macOS `__eh_frame`
compact-unwind warning; execution was unaffected. Per the review brief, the
full library suite was not rerun.

## Assessment

The transport, inheritance, transaction backstops, error mapping, and
fingerprint compatibility are implemented coherently. Approval is blocked
only on the missing broker-level continuation proof required by the Task's
high-risk side-effect-ordering matrix.
