# Task 5 Code Review Report (Codex, HIGH)

## Review Identity

- `reviewed_task_id`: `7c63eb27-e8eb-4cb6-9808-a1787a31dbea`
- `lineage_task_id`: `20149d71`
- Producer commit: `d145b2c2b7a1811d4c11905935227625e0849e44`
- Review base: `20ddf3e78094d0ea6df9b50b8f2e1d009576a84b`
- Scope: Plan Task 5 and the approved terminal fail-closed host surface
- Mode: review only; no implementation
- Verified design digest:
  `sha256:61780e516676ca31f2dc2226d3b70bff67920b566d4fe28dc06d6d81a3295efa`
- Verified plan digest:
  `sha256:e59e90636265fe6f11c284a1da5e09d5752b04db25c42b142ad3981aaeb15255`

## Verdict

`request_changes`

## Findings

### Critical

None.

### Important

#### T5-CODEX-I1: Connection-availability lookup failures bypass the bounded terminal retry

Task 5 requires transient database lookup to remain
`TaskStoreError::Transient`, explicitly including connection-availability
errors (`plan.md:667,726`). The new terminal mappers instead classify only
SQLite busy/locked text as transient. `map_db_err` turns every other `DbErr`
into `Permanent` (`src-tauri/src/acp/delegation/run_store.rs:952-958`), and
`terminal_protocol_store_error` similarly turns every non-busy
`WorkflowStoreError::Persistence` into `Permanent`
(`run_store.rs:972-981`). These two mappers cover the run-binding query and
the typed header load respectively (`run_store.rs:994-1008`).

Consequently, `DbErr::ConnectionAcquire(Timeout | ConnectionClosed)` and a
closed connection are not retried by
`terminal_completion_protocol_with_retry`, whose retry predicate accepts only
`TaskStoreError::Transient` (`broker.rs:7894-7911`). This conflicts with the
existing workflow error contract, where `WorkflowStoreError::Persistence` is
retryable, and can prematurely convert a temporary pool outage into a terminal
`persistence_error`.

Preserve typed connection-acquire/connection availability before stringifying
the `DbErr`, and map retryable header `Persistence` to the transient rail. Add
focused mapper tests for pool timeout, pool closed, closed connection, busy,
locked, and a genuinely permanent query/decode error.

#### T5-CODEX-I2: Transactional protocol rejection can emit a stale conversation status

The broker snapshots the producer's requested `conversation_status` before
settlement (`src-tauri/src/acp/delegation/broker.rs:7931-7933`). The new
transactional authority check can then replace that disposition with a failed
protocol terminal and persist `ConversationStatus::Cancelled`
(`src-tauri/src/acp/delegation/run_store.rs:4318-4333`). The returned report
correctly reflects the durable failed row, but the winning publication path
still passes the pre-check snapshot to `publish_terminal_meta_and_event`
(`broker.rs:8078-8100`), which emits it as `ConversationStatusChanged`
(`broker.rs:8277-8290`).

If the pre-read sees v2 for a successful producer and the header becomes v1,
unsupported, corrupt, or dangling before the settlement transaction, durable
run/conversation state becomes `Failed`/`Cancelled` while the live status
event says `PendingReview`. This is the exact check/use window the in-transaction
reclassification is intended to close, and it can leave the frontend on a
state contradicted by durable authority.

Return the authoritative conversation status from the settlement transaction
or derive publication status from the persisted winning report. Add a gated
race regression that changes the header after terminal pre-read but before the
CAS and asserts durable row, conversation projection, wait report,
`ConversationStatusChanged`, and terminal event all agree.

#### T5-CODEX-I3: The pre-spawn MCP-binding adapter erases stable protocol errors

`load_workflow_child_mcp_binding` now correctly returns a typed
`CompleteWorkError::Protocol` for historical, inconsistent, corrupt, or
dangling headers (`src-tauri/src/acp/delegation/workflow/admission.rs:349-375`).
The production `RunStore` adapter immediately converts that error to `String`
(`src-tauri/src/acp/delegation/run_store.rs:2224-2238`). The broker then stores
it as `WorkflowLaunchLoadError::WorkflowBinding(String)` and assigns only a
caller-supplied fallback code (`src-tauri/src/acp/delegation/broker.rs:2505-2517`):
`spawn_failed` for first dispatch (`broker.rs:4425-4455`) and
`admission_failed` for continuation (`broker.rs:9883-9913`).

Thus a protocol/header change after the admission transaction but before the
pre-spawn binding load still aborts launch, but loses the required
`legacy_completion_protocol_read_only` or
`unsupported_completion_protocol` code. The direct listener test validates
the typed inner helper, not this production adapter boundary.

Keep the structured error through `RunStore` and `WorkflowLaunchLoadError`,
including its stable code and persistence/transient distinction. Add first,
continue, and replacement race tests at the existing pre-spawn checkpoint and
assert the stable protocol code plus zero process/prompt/MCP-feature side
effects.

### Minor

#### T5-CODEX-M1: The plan-mandated launch and terminal host matrices are incomplete

Plan Task 5 requires every v1/inconsistent pair and a dangling binding through
first dispatch, continue, and replacement, with budget/run/spawn/prompt/feature
snapshots (`plan.md:672-681`). It also requires every permanent terminal case
to prove durable/wait/event code equality, zero Card/shadow calls, zero semantic
writes, and no retry (`plan.md:685-695`).

The new integration admission test covers all decodable rejected pairs only at
the direct first-dispatch store boundary
(`src-tauri/tests/completion_protocol_v2.rs:805-882`). Broker launch coverage
uses only historical `(1,v1)` for all three variants and dangling only for
continue (`src-tauri/src/acp/delegation/broker.rs:35127-35296`). Likewise, the
terminal classifier covers all pairs/corruption, but full host-surface parity
is asserted only for `(1,v1)`, dangling, and injected transient exhaustion
(`broker.rs:31043-31268`); there are no parser/shadow invocation counters or
complete semantic-write snapshots.

Complete the declared matrices while fixing the Important findings. In this
high-risk concurrency task, classifier-only tests are not a substitute for
the production broker surface.

## Positive Observations

- Exact-v2 admission is now checked before the admission transaction can
  commit a run binding, and rejected reservations roll back.
- Permanent protocol rejection is rechecked inside the terminal settlement
  transaction, clears stale Card/completion/remediation authority, and avoids
  workflow semantic side effects.
- The normal permanent v1 and dangling paths preserve durable/wait/terminal
  event error-code parity and do not install `PendingTerminalRetry`.
- Standalone Card behavior and the existing v2 semantic-input integration test
  remain green.

## Verification Evidence

Fresh commands run against producer `d145b2c2b7a1811d4c11905935227625e0849e44`:

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils` | Pass: 34 passed, 0 failed |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils pending_terminal_retry` | Pass: 1 passed, 0 failed, 4278 filtered out |

These passing tests confirm the checked-in cases, but they do not exercise the
connection-availability classification, the transaction-time status-event
race, or the typed pre-spawn binding adapter described above. Cargo emitted
the existing zero-byte `codeg-mcp` sidecar warning; it did not affect either
test result.

## Conclusion

**request_changes** - The main fail-closed direction is sound, but Task 5 is
not ready to pass its high-risk gate. Preserve connection-availability retry,
publish transaction-authoritative conversation status, and retain stable
protocol codes through the pre-spawn MCP-binding adapter. Close those findings
test-first and complete the missing host-surface matrices before Task 6.

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"7c63eb27-e8eb-4cb6-9808-a1787a31dbea","lineage_task_id":"20149d71","producer_commit":"d145b2c2b7a1811d4c11905935227625e0849e44","verdict":"request_changes","critical":0,"important":3,"minor":1,"summary":"Task 5 fails closed on the covered protocol cases, but connection-availability errors bypass transient retry, transaction-time protocol rejection can publish a stale conversation status, and the pre-spawn MCP-binding adapter erases stable protocol codes; the required launch/terminal host matrices are also incomplete.","report_file":".superpowers/sdd/task-5-review-codex-report.md"}
-->
