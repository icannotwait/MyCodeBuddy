# Task 2 Report: Binding Transport and Lineage Admission

## Status

Complete in commit `43c63745d501a2619b151522d550bcdf0450f931`
(`feat(delegation): enforce binding lineage`). The report is intentionally
untracked and was not included in the commit.

## Observed RED

- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture`
  executed 2 tests and failed 2 before the implementation. The published
  schemas omitted `orchestration_binding`, and the listener did not reject the
  shared corpus's present invalid values, including explicit JSON `null`.
- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture`
  executed 4 tests and failed 4 before the implementation. First dispatch did
  not persist the binding, and continuation/replacement did not inherit or
  enforce source lineage.

## Implemented Contract

- Added optional `orchestration_binding` transport fields to both delegation
  request types. Omission remains `None`; every present malformed or
  semantically invalid value returns `orchestration_binding_invalid`.
- Added the same strict `$defs` v1 binding object to both MCP input schemas:
  four required fields, no unknown fields, exact schema version, namespace,
  generation, and SHA-256 bounds, with no JSON `null` alternative.
- Revalidated bindings at both direct broker entry points before depth, child,
  spawn, or resume activity.
- Added stable `orchestration_binding_invalid` and
  `orchestration_binding_lineage_mismatch` error paths, including the distinct
  task-store mismatch mapping.
- First dispatch fingerprints and persists its supplied binding in the
  reserving transaction. Unbound requests retain Task 1's seven-string
  fingerprint bytes.
- Continuation resolves the source binding before recovery/resume work, uses
  the inherited effective binding in its fingerprint, repeats lineage
  comparison under the writer transaction, and copies the binding into the
  reserving insert.
- Replacement resolves lineage before provisional child creation, fingerprints
  the effective binding, repeats comparison under the writer transaction before
  eligibility/authorization/budget work, and overwrites the insert with the
  source binding.

## Shared Corpus and Side Effects

Schema, listener, and semantic validation all load the unchanged shared fixture
`src-tauri/tests/fixtures/orchestration_binding_v1.json`. Both schemas agree
with every fixture case. The listener accepts omission and all shared valid
objects, while every shared invalid value produces exactly
`orchestration_binding_invalid` with unchanged mock spawn/resume counts.

The lineage matrix covers bound omission inheritance, exact explicit matches,
all four changed fields, unbound omission, and rejected bound/unbound
conversion. Rejected continuations create no reserving row. Rejected
replacements leave spawn counts and provisional child counts unchanged. The
replacement mismatch test then reuses the same recovery authorization in an
exact request, proving the rejected call did not consume it. These lineage
checks precede eligibility mutation, recovery authorization consumption,
counter preflight/charge, and process activity in the writer paths.

Omitted continuation and replacement bindings persist the exact source binding.
Continuation replay with an omitted inherited binding is idempotent. First,
continue, and replacement fingerprints use their effective binding, so a
different effective identity cannot alias under one parent tool-use ID.

## Literal Scans

The final word-boundary request-literal scan found exactly:

```text
src-tauri/src/acp/connection.rs
src-tauri/src/acp/delegation/broker.rs
src-tauri/src/acp/delegation/listener.rs
src-tauri/src/acp/delegation/run_store.rs
src-tauri/src/acp/delegation/workflow/recovery_tests.rs
src-tauri/src/acp/lifecycle.rs
src-tauri/tests/completion_protocol_v2.rs
src-tauri/tests/delegation_session_reuse_integration.rs
```

The final `ContinueRunAdmission` literal scan found exactly:

```text
src-tauri/src/acp/delegation/broker.rs
src-tauri/src/acp/delegation/run_store.rs
src-tauri/src/acp/delegation/workflow/completion_evidence.rs
```

All legacy literals explicitly set the new field or fields to `None`; focused
binding tests use exact values. The all-target test compilation passed, so no
request or admission literal owner is missing the new fields.

## Verification

- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture`
  - 2 passed, 0 failed.
- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture`
  - 4 passed, 0 failed.
- `cargo test --no-default-features --features server,test-utils --lib request_fingerprint_ -- --nocapture`
  - 2 passed, 0 failed; the unbound seven-string digests remain exact and the
    bound v2 twelve-string array is covered.
- `cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture`
  - 1 passed, 0 failed; printed Grok tools/list JSONL size is `7669` bytes and
    the unchanged `7_680`/`7680` assertion contract passes.
- `cargo test --no-default-features --features server,test-utils --lib`
  - 4630 passed, 0 failed, 1 ignored.
- `cargo check --no-default-features --features server,test-utils --tests`
  - passed.
- `cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp`
  - passed.
- `git diff --check`
  - passed before commit.

## Concerns

- The Grok fixed stdio budget has only 11 bytes of remaining headroom after the
  required nested schemas and retained guidance. Any later catalog growth must
  preserve the unchanged 7680-byte contract deliberately.
- macOS test linking emitted the existing oversized `__eh_frame` compact-unwind
  warning; it did not affect test or check results.

## Fix Round 1: Broker Continuation Side-Effect Proof

Completed in commit `344d2ab99fabbf0c7e62d14bfb852f8272ce0f9c`
(`test(delegation): prove continuation binding fence`). The report remains
unstaged and outside the commit.

Added one broker-level regression test covering both required recovery cases:

- a bound legacy `parent_disconnected` source rejects a changed namespace;
- an unbound legacy `parent_disconnected` source rejects a supplied binding;
- both return exactly `orchestration_binding_lineage_mismatch`, leave
  `MockSpawner::resume_args` and `spawn_args` unchanged, and create no durable
  row for the rejected parent tool-use ID;
- the same approved continuation authorization then admits an exact binding for
  the bound source or an omitted binding for the unbound source;
- each successful retry performs exactly one resume, reuses the source child,
  records the reused authorization, and persists the exact source binding.

### Mutation RED

The production fence already passed the new test. To demonstrate test
sensitivity, `inherited_binding` was temporarily mutated to ignore the supplied
binding in both broker and writer checks. The exact new test command executed
1 test and failed 1: the bound mismatch reached resume and returned
`unresumable` instead of the expected
`orchestration_binding_lineage_mismatch`. The mutation was restored before the
final verification and commit.

```text
cargo test --no-default-features --features server,test-utils --lib acp::delegation::broker::tests::orchestration_binding_lineage_continue_mismatch_precedes_admission_and_resume -- --exact --nocapture
running 1 test
test ...orchestration_binding_lineage_continue_mismatch_precedes_admission_and_resume ... FAILED
test result: FAILED. 0 passed; 1 failed; 4631 filtered out
```

### Final Verification

```text
cargo test --no-default-features --features server,test-utils --lib orchestration_binding_lineage_ -- --nocapture
running 5 tests
test result: ok. 5 passed; 0 failed; 4627 filtered out

cargo test --no-default-features --features server,test-utils --lib orchestration_binding_transport_ -- --nocapture
running 2 tests
test result: ok. 2 passed; 0 failed; 4630 filtered out

cargo test --no-default-features --features server,test-utils --lib request_fingerprint_ -- --nocapture
running 2 tests
test result: ok. 2 passed; 0 failed; 4630 filtered out

git diff --check
passed before commit
```

### Retained Concerns

- The Grok fixed stdio budget still has only 11 bytes of remaining headroom.
- macOS linking still emits the existing oversized `__eh_frame` compact-unwind
  warning; all covering tests executed successfully.
