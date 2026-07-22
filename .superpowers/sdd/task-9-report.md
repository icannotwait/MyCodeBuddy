# Task 9 Report - Delegation Session Reuse Integration and Verification

## Status

Implemented Task 9 integration coverage and the minimal continuation-report
fix uncovered by the incarnation-mismatch fixture. The task-owned tests and
all static build/lint gates pass. Broad library test runs did not complete;
their liveness issue is recorded below rather than treated as a pass.

## Changes

- Added `delegation_session_reuse_integration.rs` with 15 in-memory contract
  fixtures covering session reuse, recovery, replacement, budget, migration,
  summary, and snapshot behavior.
- Extended `MockSpawner` with recorded resume arguments and queued explicit
  resume results so tests can prove `ResumeExistingOnly` behavior and force a
  returned-incarnation mismatch.
- Preserved `task_id` and `continued_from_task_id` on all failures and
  parent-end cancellations after a continuation has durably reserved its run.
  Those terminal runs are now queryable and card-correlatable from the
  immediate error response.
- Cleared six pre-existing test-only Clippy failures required by the brief's
  `--all-targets -D warnings` gate.

## Coverage Matrix

| Brief bullet | Task 9 fixture | Existing focused coverage |
| --- | --- | --- |
| 800: 3 children, 12 runs | `shape_800_three_reviewers_four_rounds_twelve_runs` | Run-store generation/projection units |
| 832: interrupted recovery on same child | `shape_832_unexpected_interrupt_new_run_same_child` | Broker continuation admission units |
| 835: replacement child, old blocked, new continuable | `shape_835_replacement_supersedes_original_child` | `replacement_*` run-store units |
| Nine Skill-forward routes | `skill_forward_routing_invariants_nine_scenarios` reads required markers from `.agents/skills/brainstorm-to-delivery/SKILL.md` | Skill policy plus fixture route isolation |
| Concurrent double continue | `concurrent_double_continue_one_winner_busy_thread` | Run-store unique-thread fence units |
| ResumeExistingOnly and no new child | `resume_existing_only_reuses_session_and_records_resume_call` | Broker continue acknowledgement unit |
| Resume incarnation mismatch -> unresumable | `resume_existing_only_connection_id_mismatch_is_unresumable` | `broker::pre_bootstrap_handoff_refuse_settles_unresumable_before_spawn_returns` |
| Missing external session is fail-closed | `resume_existing_only_missing_external_id_is_not_continuable` | Continue eligibility decision units |
| Migration collision, preview redaction, summary non-exposure | `migration_collision_unique_parent_tool_losers_null_key`; `task_preview_redaction_and_summary_not_in_parent_mcp_report` | `delegation_task_runs_migration::{duplicate_call_id_keeps_newest_non_deleted_only,duplicate_parent_tool_use_id_losers_history_only_with_legacy}`; card-summary units |
| Pre-admission redispatch and replacement retry | `pre_admission_host_restarted_reserving_inherits_and_allows_redispatch`; `pre_admission_replacement_retry_does_not_charge_until_running` | `run_store::replacement_admission_checks_reason_and_charges_only_on_running` |
| Budget race and no refund after running | `budget_race_allows_one_winner_for_final_unexpected_continue_slot`; `budget_no_refund_after_running_and_cap` | `run_store::{concurrent_promote_races_one_budget_winner,post_running_cancel_fail_do_not_refund_charged_counter}` |
| Parent end after durable continuation reservation | Broker units `continue_parent_cancel_between_reserve_commit_and_handoff_never_spawns`; `continue_parent_cancel_after_post_reserve_check_before_handoff_never_spawns` | Both now assert returned new and predecessor task ids |
| Desktop and web snapshot DTOs | `desktop_and_web_snapshot_dto_share_core_and_immutability` | `tests/delegation_run_snapshot.rs` |

## Verification

| Command | Result |
| --- | --- |
| `cargo test --features test-utils --test delegation_session_reuse_integration` | PASS: 15 passed |
| `cargo test --features test-utils --lib concurrent_promote_races_one_budget_winner` | PASS: 1 passed, 2631 filtered |
| `cargo test --features test-utils --lib pre_bootstrap_handoff_refuse_settles_unresumable_before_spawn_returns` | PASS: 1 passed, 2631 filtered |
| `cargo test --features test-utils --lib continue_parent_cancel_between_reserve_commit_and_handoff_never_spawns` | PASS: 1 passed, 2631 filtered |
| `cargo test --features test-utils --lib continue_parent_cancel_after_post_reserve_check_before_handoff_never_spawns` | PASS: 1 passed, 2631 filtered |
| `cargo clippy --lib --features test-utils -- -D warnings` | PASS (also covered by the all-targets pass below) |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | PASS |
| `cargo check --no-default-features --bin codeg-server` | PASS |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | PASS |
| `cargo check --no-default-features --bin codeg-mcp` | PASS |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | PASS |
| `rustfmt --edition 2021 --check tests/delegation_session_reuse_integration.rs` | PASS |
| `cargo test --features test-utils` | DID NOT COMPLETE: timed out after 904 seconds without a test failure emitted |
| `cargo test --no-default-features --bin codeg-server --lib` | DID NOT COMPLETE: stopped after more than 9 minutes with the same broad lib-test liveness pattern |
| `pnpm test`, `pnpm eslint .`, `pnpm build` | N/A: no frontend files changed |

## Concerns

- The broad Rust lib runner has a pre-existing liveness issue in this worktree:
  an older full-suite process was already stuck in the same `codeg_lib` test
  binary. Only the task-owned process trees were stopped; unrelated processes
  were left untouched.
- Cargo reports the existing sidecar-placeholder build warning and the
  dependency future-incompatibility warning for `proc-macro-error2`; neither
  is a new lint failure and all requested Clippy gates pass.
- `cargo fmt --check` reports repository-wide formatting drift in unrelated
  files. The task-owned integration fixture itself passes `rustfmt --check`.
