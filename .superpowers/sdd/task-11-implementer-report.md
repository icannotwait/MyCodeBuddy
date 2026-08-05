# Task 11 Implementer Report

## Result

Implemented typed completion attention, authenticated adjudication, artifact
retry, Design self-review resolution, completion-family reconciliation, and
durable outbox replay for desktop and server runtimes.

## TDD Evidence

RED was established before implementation:

- `cargo test --lib typed_completion_attention -- --list` listed one test.
- `cargo test --lib typed_completion_attention -- --nocapture` failed because
  generic child-question cleanup decoded a completion row as a child question.
- `cargo test --features test-utils --test completion_transport_parity attention -- --list`
  listed two tests.
- `cargo test --features test-utils --test completion_transport_parity attention -- --nocapture`
  failed because the completion handler module and routes were absent.

The plan's unqualified `cargo test typed_completion_attention` was narrowed to
`cargo test --lib typed_completion_attention`: the unqualified command builds
unrelated integration targets, including `api_integration.rs`, which requires
the `test-utils` feature. This keeps Task 11 verification focused as required.

Final GREEN verification:

- `cargo test --lib typed_completion_attention -- --list`: 8 tests listed.
- `cargo test --lib typed_completion_attention -- --nocapture`: 8 passed,
  0 failed.
- `cargo test --features test-utils --test completion_transport_parity attention -- --list`:
  3 tests listed.
- `cargo test --features test-utils --test completion_transport_parity attention -- --nocapture`:
  3 passed, 0 failed.
- `cargo check --no-default-features --features server --bin codeg-server`:
  passed without warnings.
- `git diff --check`: passed.

No full workspace test, clippy, or frontend suite was run; Task 18/Final owns
broad verification.

## Implementation

- Completion-family rows use versioned typed payloads/resolutions and the
  exact six-field `CompletionAttentionCas`.
- Free-form Broker replies remain child-question-only and reject completion
  rows with `attention_kind_mismatch`.
- Authenticated application mutations enforce durable root ownership, kind,
  role-legal outcomes, stale CAS rejection, same-outcome replay, and
  different-outcome conflict.
- User adjudication materializes evidence or opens artifact recovery in one
  transaction. Design self-review resolves platform-owned Design-root
  bindings without accepting a request actor identity.
- Successful decisions and artifact retries enqueue
  `completion_decision_resolved` after graph revision changes. The outbox
  dispatcher processes creation order, increments attempts, optionally calls
  an event-id-deduplicated root wake queue, emits, and only then marks delivery.
- Startup/periodic dispatcher scans reconcile completion-family attention:
  current rows survive restart and Parent/Broker teardown; stale rows become
  `superseded`; the latest unresolved terminal subject is reopened when its
  typed payload can be rebuilt. Recoverable workflow `Blocked` is retained.
- Explicit workflow cleanup resolves all completion-family rows with
  `workflow_terminated` or `workflow_deleted` and is idempotent when no rows
  remain.
- Desktop and authenticated Axum routes share the same core functions and DTOs.

## Task 10 Minor M1

Implemented because the plan assigns retry-owned scope invalidation to Task
11. Artifact retry records the bounded phase/dimension metric only after the
transaction commits a new supersession. Replaying an already-superseded retry
does not double-count.

## Focused Coverage

Coverage includes lifecycle kind isolation, foreign ownership, free-form kind
rejection, every required CAS field, stale CAS, invalid role outcome, success,
same-outcome replay, conflict, artifact recovery/retry, invalidation metrics,
Design self-review, `Blocked` retention, explicit termination, post-commit
outbox replay, delivery ordering, root-wake dedupe, startup retention and stale
replacement, authenticated HTTP middleware, and core/HTTP error parity.

## Worktree Hygiene

The pre-existing changes in `.superpowers/sdd/progress.md`, `connection.rs`,
`companion.rs`, `launch_snapshot.rs`, `workflow/project.rs`, and the
`publish*.json`/manifest files were not staged or modified for Task 11.

## Concerns

None within Task 11 scope. Broad cross-task integration remains assigned to
Task 18/Final.
