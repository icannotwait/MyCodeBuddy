# Task 12 report

Status: TASK_REVIEW_APPROVED

## Acceptance RED and GREEN

The reconstructed session-2566 fixture first failed because the blocked
workflow had no current Plan Author run binding. After adding the exact current
Author/reviewer evidence, `cargo test session_2566 --lib -- --nocapture`
listed and passed 1 test. It proves direct publication cannot unblock,
authorized state-only recovery advances revision 8 to 9 without changing Plan
structure or retired bindings, and Task 1 admission remains available.

The delegation fixture then failed after the authorized continue was consumed,
the resume failed durably, and replacement was admitted: the replacement did
not inherit the consumed recovery provenance. Commit `ca7a4c43` fixed the
RunStore replacement path; the exact test subsequently listed and passed 1.

Full integration testing exposed a second production inconsistency: a pure
pre-admission replacement abort did not supersede admission transactionally,
but public recovery projection treated any replacement edge as superseding.
Commit `370a4689` centralized constant-query supersession semantics so retry,
status, and authorization projection agree while preserving transitive
supersession for an abort that itself has a successor.

## Task 12 commits

- `86708ac7` `fix(test): expose office-watch fixtures to unit tests`
- `ca7a4c43` `fix: inherit recovery provenance across replacement`
- `34c8c1e9` `fix: restore non-test recovery compilation`
- `4163cc03` `test: verify authorized recovery end to end`
- `23c1da18` `fix: make lint reproducible on Windows`
- `b2cf4320` `test: update disconnect provenance expectations`
- `6a5c6c20` `test: update integration fixtures for recovery fields`
- `370a4689` `fix: align recovery integration contracts`
- `1bbadcdc` `test: update recovery matrix expectations`
- `f610d9dd` `fix: satisfy strict recovery clippy gates`

## Focused verification

- Task 12 acceptance fixtures: 2/2 passed.
- `delegation_session_reuse_integration`: 19/19 passed.
- `replacement_admission`: 14/14 passed.
- `authorized_delegation_recovery`: 11/11 passed.
- `delegation_recovery`: 24/24 passed.
- `delegate_access_api`: 6/6 passed.
- `git diff --check`: passed.

## Full validation matrix

Frontend, from repository root:

- `pnpm eslint .`: exit 0 with 23 existing warnings.
- `pnpm test`: exit 0.
- `pnpm build`: exit 0.

Desktop Rust, from `src-tauri/`:

- `cargo check`: passed.
- `cargo test --features test-utils`: 3896 passed, 1 ignored; all integration
  binaries passed.
- `cargo clippy --all-targets --features test-utils -- -D warnings`: passed.

Server Rust:

- `cargo check --no-default-features --features server --bin codeg-server`:
  passed.
- `cargo test --no-default-features --features server --bin codeg-server --lib`:
  3820 passed, 1 ignored; server bin target passed with no tests.
- `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`:
  passed.

Collaboration sidecar:

- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`: passed.

Known non-blocking diagnostics are the development-only missing codeg-mcp
sidecar placeholder warning in desktop builds and Cargo's upstream
`proc-macro-error2` future-incompatibility warning. Whole-tree
`cargo fmt --check` is outside the required matrix and still reports unrelated
pre-existing formatting drift; Task 12 formatted only owned Rust paths.

## Reviews

- Task 12 review: spec compliant and quality approved; no Critical,
  Important, or Minor findings. The reviewer could not verify the reported
  full matrix from the diff package, so the controller retained the direct
  command evidence above as the authoritative verification record.
- Whole-branch review: pending.

## Whole-branch Important fix wave (2026-08-01)

Implementation commit:

- `143f27c610e2bff5e684c5fdab99ab57a7fbb150` `fix: close authorized recovery review findings`

### RED evidence

Rust commands below ran from `src-tauri/`; other commands ran from the
repository root.

- `cargo test delegation_recovery_policy --lib -- --nocapture`: RED, 15
  selected; 13 passed and 2 intended regressions failed (completed rows with
  non-protected errors, and missing structural resume identity).
- `cargo test typed_transport_cleanup_projects_automatic_unexpected_continue --lib -- --nocapture`:
  RED, 1 selected and 1 intended failure because cleanup persisted
  `parent_disconnected` instead of typed unexpected transport loss.
- `cargo test exact_replay_survives_ordinary_later_workflow_activity --lib -- --nocapture`:
  RED, 1 selected and 1 intended `WorkflowRecoveryConflict` failure after
  later ordinary workflow activity.
- `pnpm test -- src/components/chat/ask-question-card.test.tsx`: RED, 29
  selected; 28 passed and 1 intended recovery-card presentation failure.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  RED, 256 selected; 252 passed and 4 intended post-token-negation mutations
  failed.
- `cargo test recovery_tool_contract --lib -- --nocapture`: RED, 6 selected;
  2 passed and 4 intended structured-result contract regressions failed.

### GREEN list gates and tests

Every filtered Rust GREEN was preceded by the identical filter with
`-- --list`, and each list was nonzero:

- `cargo test delegation_recovery_policy --lib -- --list`: 15 tests listed.
- `cargo test delegation_recovery_policy --lib -- --nocapture`: 15 passed.
- `cargo test typed_transport_cleanup_projects_automatic_unexpected_continue --lib -- --list`:
  1 test listed.
- `cargo test typed_transport_cleanup_projects_automatic_unexpected_continue --lib -- --nocapture`:
  1 passed.
- `cargo test authorized_workflow_recovery --lib -- --list`: 10 tests listed.
- `cargo test authorized_workflow_recovery --lib -- --nocapture`: 10 passed.
- `cargo test recovery_tool_contract --lib -- --list`: 8 tests listed.
- `cargo test recovery_tool_contract --lib -- --nocapture`: 8 passed.
- `cargo test recovery_authorization --lib -- --list`: 19 tests listed.
- `cargo test recovery_authorization --lib -- --nocapture`: 19 passed.
- `cargo test --test delegation_recovery_migration recovery_migration_preserves_existing_workflow_and_run_bytes -- --list`:
  1 test listed.
- `cargo test --test delegation_recovery_migration recovery_migration_preserves_existing_workflow_and_run_bytes -- --nocapture`:
  1 passed.

Additional focused GREEN verification:

- `pnpm test -- src/components/chat/ask-question-card.test.tsx`: 1 file and
  29 tests passed.
- `pnpm eslint src/components/chat/ask-question-card.tsx src/components/chat/ask-question-card.test.tsx src/lib/types.ts`:
  exit 0 with no findings.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  12 suites and 256 tests passed.
- `cargo check`: passed.
- `cargo check --no-default-features --features server --bin codeg-server`:
  passed.
- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --lib --features test-utils -- -D warnings`: passed.
- `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`:
  passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`:
  passed.
- `rustfmt --edition 2021 --check src/acp/delegation/broker.rs src/acp/delegation/companion.rs src/acp/delegation/listener.rs src/acp/delegation/recovery_policy.rs src/acp/delegation/workflow/store.rs src/acp/recovery_authorization/mod.rs src/acp/recovery_authorization/service.rs src/acp/recovery_authorization/types.rs src/acp/termination.rs src/db/entities/delegation_workflow_manifest_revision.rs src/db/migration/m20260730_000001_recovery_authorizations.rs tests/delegation_recovery_migration.rs`:
  passed.
- `git diff --check`: passed.

The only diagnostics were the already documented development sidecar
placeholder warning and Cargo's upstream `proc-macro-error2`
future-incompatibility warning. The approved Designs and implementation plan
were not modified. Controller whole-branch review remains pending.

## Whole-branch Important fix wave, round 2 (2026-08-01)

Implementation commit:

- `f8481fd18896b0cd6184354546fe8d6e3edbd71d` `fix recovery replay and validator hardening`

### RED evidence

Rust commands below ran from `src-tauri/`; validator commands ran from the
repository root.

- `cargo test modern_pre_spawn_parent_loss --lib -- --nocapture`: RED, 2
  selected, 0 passed, and 2 intended failures. Checkpoint #1 returned
  `parent_disconnected` instead of `transport_disconnected`; checkpoint #2
  persisted `parent_disconnected` instead of `transport_disconnected`.
- `cargo test exact_replay_survives_later_task_admission_and_active_run --lib -- --nocapture`:
  RED, 1 selected, 0 passed, and 1 intended `WorkflowRecoveryConflict` after
  normal Task admission advanced the graph and left a reserving run active.
- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  RED, 262 selected; 258 passed and 4 intended failures. The two exact
  adversarial status/challenge suffixes bypassed `B2D-R001`, and the two
  safe-negative-only clauses were incorrectly counted as affirmative.

### GREEN list gates and tests

Every filtered Rust GREEN was preceded by the identical filter with
`-- --list`, and each list was nonzero:

- `cargo test modern_pre_spawn_parent_loss --lib -- --list`: 2 tests listed.
- `cargo test modern_pre_spawn_parent_loss --lib -- --nocapture`: 2 passed,
  3901 filtered out. Both real pre-spawn checkpoints cover typed transport,
  process, and session loss plus intentional frontend disconnect.
- `cargo test exact_replay_survives_later_task_admission_and_active_run --lib -- --list`:
  1 test listed.
- `cargo test exact_replay_survives_later_task_admission_and_active_run --lib -- --nocapture`:
  1 passed, 3902 filtered out.
- `cargo test authorized_workflow_recovery --lib -- --list`: 12 tests listed.
- `cargo test authorized_workflow_recovery --lib -- --nocapture`: 12 passed,
  3892 filtered out. This includes immutable receipt/revision tamper rejection
  and nullable pre-upgrade revision-evidence replay compatibility.
- `cargo test --test delegation_recovery_migration recovery_migration_preserves_existing_workflow_and_run_bytes -- --list`:
  1 test listed.
- `cargo test --test delegation_recovery_migration recovery_migration_preserves_existing_workflow_and_run_bytes -- --nocapture`:
  1 passed, 2 filtered out.
- `cargo test --test delegation_recovery_migration recovery_migration_adds_one_active_challenge_and_provenance_columns -- --list`:
  1 test listed.
- `cargo test --test delegation_recovery_migration recovery_migration_adds_one_active_challenge_and_provenance_columns -- --nocapture`:
  1 passed, 2 filtered out.

Additional focused GREEN verification:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  12 suites and 262 tests passed.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  36 checks passed, 0 failures.
- Direct production-validator probe against the real `SKILL.md`: all 4 exact
  adversarial clauses were rejected with `B2D-R001` (4/4).
- `cargo check`: passed.
- `cargo check --no-default-features --features server --bin codeg-server`:
  passed.
- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --lib --features test-utils -- -D warnings`: passed.
- `cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings`:
  passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`:
  passed.
- `rustfmt --edition 2021 --check src/acp/delegation/broker.rs src/acp/delegation/workflow/store.rs src/acp/delegation/workflow/recovery_tests.rs src/db/entities/delegation_workflow_manifest_revision.rs src/db/migration/m20260730_000001_recovery_authorizations.rs tests/delegation_recovery_migration.rs`:
  passed.
- `git diff --check`: passed.

The only diagnostics were the already documented development sidecar
placeholder warning and Cargo's upstream `proc-macro-error2`
future-incompatibility warning. The approved Designs and implementation plan
were not modified. Controller whole-branch review remains pending.
