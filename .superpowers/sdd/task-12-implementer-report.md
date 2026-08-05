# Task 12 Implementer Report

## Result

Implemented protocol-v2 completion evidence revalidation across admission,
settlement/store, execution gates, projection, and recovery policy. Continue
and replacement admissions now share one fail-closed transaction fence before
authorization, receipt, counter, budget, or run-insertion side effects.

## TDD Evidence

RED was established before implementation. The focused tests and intermediate
compilation runs exposed the intended missing behavior:

- The shared persisted-evidence loader did not exist, so the consumer fixture
  could not compile against one v2 validator.
- Admission, gates, projection, and recovery still observed Card fields,
  summary/count columns, or legacy fingerprints.
- Continue had no completion-recovery fence, and replacement bypassed the
  first fence placement.
- A failed durable-context rebuild returned `Ok(())` instead of failing closed
  as `completion_artifact_unavailable`.
- Admission projected valid v2 evidence as protocol 1.
- Idempotent replay of a v2 settlement failed on null legacy finding counts
  with `settlement is missing critical count`.
- Project settlement selection had no v2 gate-lineage matcher.

The implementation followed those failures to GREEN. Fresh final verification:

- `cargo test --lib completion_v2_shared_validator -- --list`: 6 tests listed.
- `cargo test --lib completion_v2_shared_validator -- --nocapture`: 6 passed,
  0 failed.
- `cargo test --lib completion_recovery_fence -- --list`: 1 test listed.
- `cargo test --lib completion_recovery_fence -- --nocapture`: 1 passed,
  0 failed.
- `git diff --check`: passed.

The recovery filter deliberately exercises the two durable store entry
classes, continue and replacement, for both `needs_decision` and
`artifact_recovery`. Listener replay, recovery-authorized paths, fresh-delegate
replacement, explicit replacement, and Broker resume all converge on these
same two first writer transactions, so the fence cannot be bypassed by a
higher-level entry path. The test also proves no new run is inserted and that a
context-rebuild failure fails closed.

No full workspace test, Clippy, frontend suite, or build was run; Task 18/Final
owns broad verification. Cargo emitted the existing warning that the packaged
`codeg-mcp` sidecar is absent and a build placeholder was used; it introduced
no tracked or untracked worktree change.

## Implementation

- `load_validated_completion_evidence` rebuilds the current durable v2
  admission context, verifies binding fields, resolves the current platform
  artifact, and delegates semantic validation exclusively to Task 9's
  `validate_completion_evidence`.
- Current selected reviewers require the active review round. An unselected
  Plan sibling may retain an older round only under the same gate lineage and
  when a later localized-change settlement proves the preserved scope.
- Admission stamps producer/reviewer coverage from validated v2 evidence.
  Legacy Card harvesting and Card/count checks remain explicit v1 behavior.
- Store settlement rejects unresolved attention states, loads the complete
  required v2 evidence set, persists evidence identities, and replays v2 rows
  without requiring legacy finding-count columns.
- Task/Final evaluation consumes resolved, role-legal outcomes and validated
  artifact/producer coverage in v2 while preserving the prior v1 evaluator.
- Projection derives v2 node and gate state from validated evidence and
  current gate lineage rather than malformed Cards or legacy fingerprints.
- Recovery snapshots and fingerprints use validated v2 outcome, scope,
  lineage, settlement, and attention state while keeping serialized v1
  fingerprint bytes unchanged through serde skip rules.
- `ensure_task_completion_recovery_not_fenced_txn` is called inside the first
  replacement and continue write transactions before ownership lookup,
  recovery authorization consumption, counter selection, budget reservation,
  or insertion. Durable read/context failures map fail-closed to
  `completion_artifact_unavailable`; current completion decisions map to
  `completion_decision_required`.

## Scope Decisions

The plan's file list was illustrative at the transaction and scope-helper
boundaries. Task 12 necessarily also modifies:

- `run_store.rs`, which owns the first durable continue/replacement
  transactions where the pre-side-effect fence must live.
- `evidence_scope.rs`, which reconstructs persisted selected-round and
  same-lineage sibling scope for the sole validator.
- `workflow/mod.rs`, which exports the shared validator and fence interfaces.

No `broker.rs` or `recovery_tests.rs` change was needed because all discovered
Broker/listener/recovery entry paths converge on the two fenced `RunStore`
transactions, and the focused test lives beside the durable completion fixture
it exercises.

`get_workflow_state_core` retains its older direct Task-gate projection. Task
13 explicitly owns execution-gate reduction and request conversion; changing
that additional path here would pull the next task into Task 12 without a
Task-12 failure requiring it.

## Worktree Hygiene

The pre-existing changes in `.superpowers/sdd/progress.md`, `connection.rs`,
`companion.rs`, `launch_snapshot.rs`, and `workflow/project.rs`, plus the
untracked `publish*.json` and manifest files, remain user-owned. Only Task 12's
behavioral hunks from `workflow/project.rs` will be staged; its pre-existing
formatting-only hunks will remain unstaged.

## Concerns

None within Task 12 scope.
