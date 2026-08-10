# Task 4 Code Review Report (Codex)

## Review Identity

- `reviewed_task_id`: `aac0bcf4-09ac-482e-af17-9d2a2f28a960`
- Producer commit: `7b826557fe38fca115dfadd65c10b2eb0da54abf`
- Review base: `87279ef9519b83c72ab3d59e63c02c2b18af4df9`
- Scope: Plan Task 4 and the approved Completion Protocol V2-Only Design
- Reviewer role: independent high-risk Codex reviewer
- Verified design digest:
  `sha256:61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`

## Verdict

`request_changes`

## Findings

### Critical

None.

### Important

#### T4-CODEX-I1: Root admission can miss a durable workflow binding and fail open

`load_completion_protocol_for_conversation` first returns the workflow owned
by the conversation. If none exists, it selects one task run ordered only by
`created_at`, then looks up the binding for that one task
(`src-tauri/src/acp/delegation/workflow/store.rs:4946-4976`). If the selected
run is unbound, it returns `None` even when another durable run for the same
conversation has a workflow binding. It also ignores a bound workflow whenever
the conversation owns another workflow.

The run schema explicitly allows multiple generations for one child
conversation; the uniqueness key is `(child_conversation_id, generation)`.
The Design fence applies to a conversation that "owns or is bound to" a
workflow, not only to the first association chosen by this query. The manager
treats `None` as an unlinked conversation and proceeds with prompt admission
(`src-tauri/src/acp/manager.rs:2587-2594`). A terminal child with an older v1
binding plus a newer unbound run, or a v2-owned workflow plus a v1 run binding,
can therefore resume through the root path instead of returning
`legacy_completion_protocol_read_only`.

Resolve all applicable durable associations, with an explicit deterministic
authority rule, and fail closed if any authoritative binding cannot produce a
header. Add regressions for multiple child generations, latest-unbound masking,
and owned-plus-bound protocol conflicts.

#### T4-CODEX-I2: The mutating Design preflight still loses corrupt-header classification inside its transaction

Task 3 added an in-transaction pair guard to
`prepare_v2_design_self_review`, but the helper still loads the complete
`delegation_workflow::Model` through generic `db_err` before that guard
(`src-tauri/src/acp/delegation/workflow/store.rs:3323-3348`). A corrupt enum
value therefore fails model decoding as retryable `Persistence`; the new typed
header mapper is never reached. The outer typed check does not close this
check/use window because the preflight opens a later transaction, and the
protocol freeze trigger is intentionally deferred to Task 7.

The same helper also converts every error from
`open_design_self_review_decision_txn`, including its new structural
`CompletionMutationError::Protocol`, back into `WorkflowStoreError::Persistence`
(`store.rs:3550-3552`). A header changed to a corrupt mode after the final outer
check therefore returns `workflow_persistence_failure` on a retry rail instead
of the required non-retryable `unsupported_completion_protocol`. The
transaction rolls back its writes, but the stable classification contract is
still violated.

Use `load_completion_protocol_header` inside the preflight transaction before
loading the full model, and preserve protocol variants when mapping the nested
completion error. Extend the existing preflight race regression with a corrupt
mode and assert exact code, retryability, and unchanged gate/binding/attention
rows.

### Minor

#### T4-CODEX-M1: The required cross-surface root and full side-effect matrix is not present

Plan Task 4 explicitly requires automation, chat-channel, Tauri, and Axum root
entry tests and a snapshot including gate state, child spawns, questions,
prompt queue, transcript, and route state (`plan.md:533-559`). The new
`root_prompt_protocol_fence` test calls only the manager foreground/background
methods (`src-tauri/tests/completion_protocol_v2.rs:3498-3587`), while the
general mutation snapshot omits several required surfaces. Static inspection
shows the current wrappers converge on the manager, but the high-risk plan
requires regression evidence at those public boundaries. Add the named entry
tests and complete the zero-side-effect snapshot while closing the Important
findings.

## Review Summary

The producer correctly applies the exact pair guard to publication,
settlement, recovery, recovery authorization, completion decisions, artifact
retry/resolution, Final delivery, and `complete_work`. Authorization checks
precede protocol disclosure on the reviewed direct mutation paths, corrupt
header mapping is narrow to typed header decoding, and focused pair matrices
pass.

Approval is blocked by the root association lookup's fail-open behavior and by
the remaining corrupt-mode classification gap in the mutating Design
preflight. The missing plan-mandated public-entry regressions are retained as a
separate test-completeness issue.

## Verification Evidence

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils`
  passed: 30 tests, 0 failures.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils historical_protocol_mutation_matrix`
  passed: 2 tests, 0 failures.
- Focused library filters for `recovery_authorization_protocol_fence`,
  `header_db_error_classification`, and
  `completion_protocol_mutations_preserve_stable_app_error_codes` passed: 3
  tests, 0 failures.
- `cargo check --manifest-path src-tauri/Cargo.toml` passed.
- `git diff 87279ef9 7b826557 --check` passed.

The full library suite was not claimed green. The producer report records 100
known Task 2/3 fixture failures outside the focused Task 4 checks. Cargo also
emitted the existing warning that the local `codeg-mcp` sidecar is a zero-byte
placeholder; it did not affect the commands above.

Conclusion: request_changes

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"aac0bcf4-09ac-482e-af17-9d2a2f28a960","producer_commit":"7b826557fe38fca115dfadd65c10b2eb0da54abf","verdict":"request_changes","critical":0,"important":2,"minor":1,"summary":"Task 4's focused 35-test verification passes, but root admission can miss an existing workflow binding and fail open, while the mutating Design preflight can still collapse a concurrent corrupt header into retryable persistence; required public-entry side-effect regressions are also incomplete.","report_file":".superpowers/sdd/task-4-review-codex-report.md"}
-->
