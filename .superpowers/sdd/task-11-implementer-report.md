# Task 11 Implementer Report

## Result

Implemented typed completion attention, authenticated adjudication, artifact
retry, Design self-review resolution, completion-family reconciliation, and
durable outbox replay for desktop and server runtimes. The consolidated review
fix additionally closes completion attention in the production root-deletion
transaction and removes request-asserted root ownership from all completion
mutation DTOs.

## TDD Evidence

RED was established before implementation:

- `cargo test --lib typed_completion_attention -- --list` listed one test.
- `cargo test --lib typed_completion_attention -- --nocapture` failed because
  generic child-question cleanup decoded a completion row as a child question.
- `cargo test --features test-utils --test completion_transport_parity attention -- --list`
  listed two tests.
- `cargo test --features test-utils --test completion_transport_parity attention -- --nocapture`
  failed because the completion handler module and routes were absent.

Review-fix RED was established independently for each Important finding:

- `typed_completion_attention_root_deletion_path_closes_as_workflow_deleted`
  failed because the production deletion path left the row `open`.
- `typed_completion_attention_reconcile_closes_already_deleted_root` failed
  because restart reconciliation retained attention owned by a soft-deleted
  root.
- The focused transport target failed to compile against the intended I2
  contract: `CompletionMutationContext` and `AuthenticatedApplication` were
  absent, DTOs still required `parent_conversation_id`, and shared cores did
  not accept authenticated context.
- Follow-up security-review RED required the production, rather than test-only,
  root scope: the transport test failed because no snapshot-issued capability
  registry/header existed, and the desktop label test failed because no
  fail-closed label authorizer existed.

The plan's unqualified `cargo test typed_completion_attention` was narrowed to
`cargo test --lib typed_completion_attention`: the unqualified command builds
unrelated integration targets, including `api_integration.rs`, which requires
the `test-utils` feature. This keeps Task 11 verification focused as required.

Final GREEN verification after the consolidated fixes:

- `cargo test --lib typed_completion_attention -- --list`: 10 tests listed.
- `cargo test --lib typed_completion_attention -- --nocapture`: 10 passed,
  0 failed.
- `cargo test --features test-utils --test completion_transport_parity attention -- --list`:
  5 tests listed.
- `cargo test --features test-utils --test completion_transport_parity attention -- --nocapture`:
  5 passed, 0 failed.
- `cargo test --lib completion_context_for_desktop_window_fails_closed_by_label -- --nocapture`:
  1 passed, 0 failed.
- `cargo test --lib web::handlers::workflow_graph::tests::http_snapshot -- --nocapture`:
  2 passed, 0 failed.
- `cargo check --no-default-features --features server --bin codeg-server`:
  passed without warnings.
- `git diff --check`: passed.

No full workspace test, clippy, or frontend suite was run; Task 18/Final owns
broad verification.

## Implementation

- Completion-family rows use versioned typed payloads/resolutions and the
  exact six-field `CompletionAttentionCas`.
- Root conversation soft-delete and completion-family resolution now share one
  database transaction. Startup reconciliation also detects an already
  soft-deleted owning root and closes its open rows as `workflow_deleted`.
- Free-form Broker replies remain child-question-only and reject completion
  rows with `attention_kind_mismatch`.
- Public mutation DTOs contain CAS plus outcome only (CAS only for artifact
  retry). Non-serializable authenticated contexts carry actor and root scope;
  an authenticated Web snapshot issues one opaque process-local capability for
  its durable root, and the real bearer middleware resolves that capability
  before the Axum mutation handler runs. A global bearer without the root
  capability is denied. Tauri permits only the explicit `main` operator window
  or a matching `conversation-N` popout and rejects auxiliary/malformed labels.
  Shared cores then enforce durable root ownership, kind, role-legal outcomes,
  stale CAS rejection, same-outcome replay, and different-outcome conflict.
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
