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

## Review Fix Package

Resolved all three Important findings from the independent Codex review of
base artifact `80190f07b1e45fb1caf3f65a56c7d9d5c6b27933`.

### TDD RED -> GREEN

- `T12-CODEX-I1` RED: the focused recovery identity test did not compile
  because no exact v2 gate evidence identity or current-settlement selector
  existed. GREEN: same-lineage round-1 settlement identity is rejected for
  round 2 replacement evidence and accepted only for the exact round/task/
  scope set.
- `T12-CODEX-I2` RED: advancing only `current_review_round` left projection's
  `latest_gate_cycle` at `Some(1)`. GREEN: projection now rebuilds the current
  validated evidence identity and returns no current outcome or summary until
  a matching settlement exists.
- `T12-CODEX-I3` RED: the focused Plan payload test did not compile because the
  protocol-gated Plan reducer entry did not exist. GREEN: v2 returns no legacy
  round state even for invalid Parent-supplied findings and lineage-reset
  material, while v1 retains the existing validation error.

### Fix Implementation

- Added one canonical `V2GateEvidenceIdentity` shared by settlement writes,
  projection, and recovery. Matching requires exact lineage, selected round,
  required node IDs, evidence task IDs, individual scope digests, and the
  recomputed aggregate scope digest. Empty, duplicate, partial, or malformed
  identity sets fail closed.
- Recovery constructs the identity only from the full current manifest
  Reviewer set with freshly revalidated evidence. A stale same-lineage
  settlement is not exposed as current approval.
- Projection loads the current gate state and rebuilds the selected-round
  evidence identity. Same-lineage round advancement therefore reopens the
  gate instead of retaining the old settlement overlay.
- Protocol-v2 Plan settlement validates platform-derived Author and Reviewer
  evidence but does not call the v1 finding reducer or count-based outcome
  validator. It persists no legacy Plan ledger, next action, report files,
  reset authorization, or `plan_round_state_v2_json`; Task 14 retains ownership
  of the true v2 reducer. The v1 reducer and persistence path remain explicit.

### Focused Verification

- `cargo test --lib completion_v2_review_fixes -- --list`: 3 tests listed.
- `cargo test --lib completion_v2_review_fixes -- --nocapture`: 3 passed,
  0 failed.
- `cargo test --lib completion_v2_shared_validator -- --list`: 8 tests listed.
- `cargo test --lib completion_v2_shared_validator -- --nocapture`: 8 passed,
  0 failed.
- `cargo test --lib completion_recovery_fence -- --list`: 1 test listed.
- `cargo test --lib completion_recovery_fence -- --nocapture`: 1 passed,
  0 failed.
- `cargo test --lib task4_plan_ -- --list`: 8 v1 Plan tests listed.
- `cargo test --lib task4_plan_ -- --nocapture`: 8 passed, 0 failed.

No full suite, Clippy, frontend test, push, or PR was run. The existing missing
`codeg-mcp` sidecar warning remained nonblocking.
