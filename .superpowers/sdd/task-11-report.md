# Task 11 Final Integrator Report

## Identity

- Work unit: `task|11|implementer|codex|none`
- Branch: `feat/completion-protocol-v2-only`
- Prior rejected freeze: `a813a60a955e560cecea14ed5c7b4477eedc211f`
- Reviewer-requested repair: `c4ec7aa3ff5d0a438aad19476c186fdb19d80b25`
- Implementation base: `190c1e141de84460c01130b4da42713a14362759`
- Delivery evidence:
  `.superpowers/sdd/completion-protocol-v2-only-delivery-report.md`

## Result

Task 11 closed `T11-CODEX-I1` / `T11-GROK-I1`. The exact desktop
library, full desktop, and server library commands now exit 0. Frontend,
desktop/server/MCP check and strict Clippy, banned-symbol removal, and focused
completion suites also pass. This integrator does not issue a Final verdict;
the new clean HEAD is for independent dual Final re-review.

## Original 103-Failure Inventory

The first captured desktop library run reported 4,183 passed, 103 failed, and
1 ignored. The 103 unique failures grouped as follows:

| Module | Count |
| --- | ---: |
| `acp::delegation::run_store` | 3 |
| `acp::delegation::workflow::admission` | 45 |
| `acp::delegation::workflow::completion_evidence` | 1 |
| `acp::delegation::workflow::project` | 6 |
| `acp::delegation::workflow::recovery_tests` | 1 |
| `acp::delegation::workflow::store` | 47 |
| **Total** | **103** |

<details>
<summary>Exact captured failing test identifiers</summary>

```text
acp::delegation::run_store::tests::task5_author_summary_stamps_plan_digest_on_binding
acp::delegation::run_store::tests::task5_plan_and_task_reviewers_reject_blank_report_paths
acp::delegation::run_store::tests::task5_workflow_terminal_summaries_are_role_aware
acp::delegation::workflow::admission::tests::agent_profile_mismatch_reject
acp::delegation::workflow::admission::tests::all_role_instruction_scope_admission_derives_material_from_durable_sources
acp::delegation::workflow::admission::tests::all_role_instruction_scope_admission_rejects_malformed_durable_plan_before_binding
acp::delegation::workflow::admission::tests::completion_artifact_contract_final_reviewer_binds_only_delivered_head
acp::delegation::workflow::admission::tests::completion_artifact_contract_producer_admission_is_clean_and_persists_baseline
acp::delegation::workflow::admission::tests::completion_artifact_contract_task_reviewer_rejects_commit_drift_before_binding
acp::delegation::workflow::admission::tests::completion_artifact_contract_terminal_producer_uses_durable_noop_policy
acp::delegation::workflow::admission::tests::completion_artifact_contract_terminal_reviewer_revalidates_clean_bound_head
acp::delegation::workflow::admission::tests::completion_v2_shared_validator_admission_ignores_legacy_card_projection
acp::delegation::workflow::admission::tests::continue_retained_observed_after_plan_revision
acp::delegation::workflow::admission::tests::final_early_reject
acp::delegation::workflow::admission::tests::final_first_pass_stamps_branch_tip_digest
acp::delegation::workflow::admission::tests::final_fixer_admits_after_report_file_card_reharvest
acp::delegation::workflow::admission::tests::final_fixer_before_non_pass_reject
acp::delegation::workflow::admission::tests::final_fixer_rejects_when_reviewer_only_failed
acp::delegation::workflow::admission::tests::newer_nonterminal_blocks_older_terminal_evidence
acp::delegation::workflow::admission::tests::pre_admission_settle_projects_workflow_transition_after_commit
acp::delegation::workflow::admission::tests::promote_running_projects_workflow_transition_after_commit
acp::delegation::workflow::admission::tests::provisional_abandon_bumps_clock
acp::delegation::workflow::admission::tests::re_review_before_fixer_pass_reject
acp::delegation::workflow::admission::tests::re_review_continue_with_no_fixer_rejects
acp::delegation::workflow::admission::tests::routed_cohort_freezes_before_reviewer_producer_readiness
acp::delegation::workflow::admission::tests::second_plan_republication_replaces_stale_corrective_authorization
acp::delegation::workflow::admission::tests::task14_final_artifact_recovery_keeps_pre_read_snapshot
acp::delegation::workflow::admission::tests::task14_final_completion_mints_immutable_package_before_fixer_admission
acp::delegation::workflow::admission::tests::task14_final_nonpass_without_context_opens_decision_without_package
acp::delegation::workflow::admission::tests::task14_fix_prior_final_reviewer_terminal_snapshot_is_reused
acp::delegation::workflow::admission::tests::task14_fix2_final_partial_round_retains_required_nonpass_sibling
acp::delegation::workflow::admission::tests::task14_fix2_plan_authorizes_corrective_round_before_reviewer_admission
acp::delegation::workflow::admission::tests::task5_different_task_work_units_cannot_share_child_conversation
acp::delegation::workflow::admission::tests::task5_empty_profile_does_not_match_unprofiled_route
acp::delegation::workflow::admission::tests::task5_high_risk_reviewers_cannot_share_child_and_route_freezes_three_nodes
acp::delegation::workflow::admission::tests::task5_implementer_and_task_reviewer_cannot_share_child_conversation
acp::delegation::workflow::admission::tests::task5_plan_author_admits_on_skeleton_before_plan_digest_exists
acp::delegation::workflow::admission::tests::task5_plan_author_continuation_reuses_its_own_conversation
acp::delegation::workflow::admission::tests::task5_plan_reviewer_requires_latest_author_and_stamps_exact_plan
acp::delegation::workflow::admission::tests::task5_policy_revision_is_allowed_before_admission_but_frozen_afterward
acp::delegation::workflow::admission::tests::task5_route_requires_exact_profile_identity
acp::delegation::workflow::admission::tests::task5_task_and_final_work_units_cannot_share_child_conversation
acp::delegation::workflow::admission::tests::task5_task_reviewer_requires_completed_producer_artifact_digest
acp::delegation::workflow::admission::tests::task6_active_final_evidence_ignores_retired_fixer_binding
acp::delegation::workflow::admission::tests::task6_active_final_evidence_ignores_retired_reviewer_binding
acp::delegation::workflow::admission::tests::terminal_settle_projects_workflow_transition_after_commit
acp::delegation::workflow::admission::tests::unexpected_continue_final_reviewer_allowed_without_fixer
acp::delegation::workflow::admission::tests::wrong_key_reject
acp::delegation::workflow::completion_evidence::tests::typed_completion_attention_design_self_review_is_typed_and_replayable
acp::delegation::workflow::project::tests::changes_requested_opens_next_cycle_without_recounting_settled
acp::delegation::workflow::project::tests::completion_v2_review_fixes_projection_reopens_same_lineage_new_round
acp::delegation::workflow::project::tests::project_manifest_overlay_no_work_unit_key
acp::delegation::workflow::project::tests::projection_b13_stale_reviewer_not_completed
acp::delegation::workflow::project::tests::stale_content_fingerprint_runs_do_not_count_after_plan_rewrite
acp::delegation::workflow::project::tests::task6_high_route_counts_strict_and_and_invalidates_both_old_approvals
acp::delegation::workflow::recovery_tests::session_2566_blocked_workflow_recovers_in_place_to_task_one_admission
acp::delegation::workflow::store::tests::a2_stale_artifact_digest_rejected
acp::delegation::workflow::store::tests::a2_stale_manifest_revision_on_run_binding_rejected
acp::delegation::workflow::store::tests::approval_rejected_with_nonzero_critical_important
acp::delegation::workflow::store::tests::authorized_workflow_recovery::exact_replay_survives_later_task_admission_and_active_run
acp::delegation::workflow::store::tests::authorized_workflow_recovery::generic_recover_receipt_cannot_satisfy_reset_plan_lineage
acp::delegation::workflow::store::tests::authorized_workflow_recovery::lineage_reset_can_atomically_end_estimated_or_approved_and_can_remain_blocked
acp::delegation::workflow::store::tests::authorized_workflow_recovery::lineage_reset_requires_exact_reason_receipt_and_persists_provenance
acp::delegation::workflow::store::tests::authorized_workflow_recovery::recover_workflow_derives_target_and_consumes_receipt_with_state_only_revision
acp::delegation::workflow::store::tests::authorized_workflow_recovery::recovery_rejects_active_run_changed_revision_stale_gate_and_frozen_contradiction_without_consuming
acp::delegation::workflow::store::tests::authorized_workflow_recovery::rejection_events_suppress_corrupt_persisted_causes
acp::delegation::workflow::store::tests::authorized_workflow_recovery::workflow_recovery_events_exclude_plan_contents_prompts_and_display_reason
acp::delegation::workflow::store::tests::completion_artifact_contract_final_delivery_drift_reopens_full_final_review
acp::delegation::workflow::store::tests::cross_parent_reject_on_get_and_settle
acp::delegation::workflow::store::tests::cycle_n_plus_1_rejects_cycle_n_runs
acp::delegation::workflow::store::tests::external_design_gate_reduces_current_validated_reviewer_evidence
acp::delegation::workflow::store::tests::failed_reviewer_cannot_approve
acp::delegation::workflow::store::tests::index_recovery_sources_cover_each_required_plan_reviewer
acp::delegation::workflow::store::tests::index_routes_use_manifest_authority_and_durable_gate_state
acp::delegation::workflow::store::tests::material_republish_uses_current_plan_gate_cohort_through_omission
acp::delegation::workflow::store::tests::settle_before_all_reviewers_rejected
acp::delegation::workflow::store::tests::settle_happy_path_idempotent_and_conflict
acp::delegation::workflow::store::tests::settle_rejects_negative_finding_counts
acp::delegation::workflow::store::tests::summary_oversize_reject
acp::delegation::workflow::store::tests::task4_historical_current_fingerprint_approval_is_terminal
acp::delegation::workflow::store::tests::task4_latest_plan_reviewer_binding_is_required
acp::delegation::workflow::store::tests::task4_parent_supplied_lineage_reset_reason_fails_closed
acp::delegation::workflow::store::tests::task4_plan_approval_derives_open_findings_and_reentry_fails_closed
acp::delegation::workflow::store::tests::task4_plan_gate_rename_cannot_reset_or_hide_lineage
acp::delegation::workflow::store::tests::task4_plan_initial_round_persists_derived_state_and_index_recovery
acp::delegation::workflow::store::tests::task4_plan_reducer_requires_infrastructure_successful_reviewer_evidence
acp::delegation::workflow::store::tests::task4_plan_replay_compares_all_structured_evidence
acp::delegation::workflow::store::tests::task4_plan_reviewers_must_cover_same_author_task_and_digest
acp::delegation::workflow::store::tests::task4_plan_stagnation_rewrite_then_user_decision_blocks
acp::delegation::workflow::store::tests::task4_required_subset_publish_invalidates_stale_gate_runs
acp::delegation::workflow::store::tests::task4_retired_plan_author_evidence_fails_closed
acp::delegation::workflow::store::tests::task4_scoped_round_uses_active_owner_subset_and_material_requires_cohort
acp::delegation::workflow::store::tests::task4_stale_approved_fingerprint_allows_material_reapproval
acp::delegation::workflow::store::tests::workflow_manifest_v2_author_card_digest_mismatch_has_typed_marker
acp::delegation::workflow::store::tests::workflow_state_authority::approval_while_blocked_persists_gate_evidence_without_unblocking
acp::delegation::workflow::store::tests::workflow_state_authority::blocked_settlement_records_typed_cause_in_a_state_only_revision
acp::delegation::workflow::store::tests::workflow_state_authority::nonblocked_plan_approval_atomically_appends_approved_state_only_revision
acp::delegation::workflow::store::tests::workflow_state_authority::task7_corrupt_plan_evidence_blocks_approved_recovery
acp::delegation::workflow::store::tests::workflow_state_authority::task7_exact_current_historical_approval_survives_later_other_plan_round
acp::delegation::workflow::store::tests::workflow_state_authority::task7_recovery_fingerprint_excludes_real_nondurable_loader_inputs
acp::delegation::workflow::store::tests::workflow_v2_typed_error_real_producers_artifact_digest
acp::delegation::workflow::store::tests::workflow_v2_typed_error_real_producers_reviewed_task_stale
acp::delegation::workflow::store::tests::zero_reviewer_design_self_review_settle
```

</details>

The inventory contains 103 unique names. Several tests were renamed during the
repair because their old names asserted legacy caller authority; the final
library count remains 4,287 total with 4,286 passed and 1 ignored.

## Fixes

- Migrated shared run-store, admission, completion-evidence, project, recovery,
  and workflow-store fixtures to fixed-v2 initialization, admission,
  materialization, and settlement.
- Reused publication-created gate-state rows to remove duplicate primary-key
  inserts.
- Seeded verified Plan/Design artifacts and valid predecessor history where
  current admission requires durable evidence.
- Replaced legacy lineage-reset success assertions with fixed-v2 read-only,
  unconsumed-authorization, and no-event assertions.
- Kept stale completion evidence fail closed by omitting only the invalid run
  from bounded projection; persistence errors still propagate.
- Updated two full-migration invariant fixtures to insert valid current v2
  headers.

## Verification

- Banned-symbol assertion: pass, no matches.
- Frontend: ESLint pass with 0 errors/25 existing warnings; Vitest 5,083/5,083;
  static export 33/33 pages.
- Desktop library: 4,286 passed, 0 failed, 1 ignored.
- Full desktop: exit 0; aggregate target summaries 4,446 passed, 0 failed,
  1 ignored.
- Server command: 4,178 library tests passed, 1 ignored; server-bin test 1/1.
- Desktop/server/MCP check and strict Clippy: all exit 0.
- Focused store: 112/112. Workflow migration: 4/4. Completion targets:
  27/27, 12/12, and 10/10.

The first full-desktop rebuild failed at MSVC PDB creation because drive `D:`
had zero free bytes. `cargo clean -p codeg` removed 69.6 GiB of stale package
artifacts; no source change was attributed to that environmental failure. The
subsequent exact full command is the passing evidence recorded above.

## Scope

Repair commit `c4ec7aa3` changes only nine Task 1-10 owned files. Large changes
are test fixture/helper migrations; the bounded projection rule is the only
production behavior change. No prior migration, database entity, removed
surface, or unrelated module changed. `git diff --check` passes for Task 11,
and no generated output is tracked.

## Handoff

The commit containing this report and the delivery report must have empty
porcelain before independent Codex and Grok Final reviewers are re-admitted.
Both reviewers must inspect the same new hash. No Final verdict is issued by
this integrator.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done","summary":"Task 11 migrated 103 fixed-v2 fixtures, made desktop/server library and full desktop suites green, reran frontend/Rust/removal gates, and prepared a clean new candidate for dual Final re-review.","commits":["c4ec7aa3ff5d0a438aad19476c186fdb19d80b25"],"tests":{"status":"passed","passed":13708,"failed":0,"summary":"Required frontend, full desktop, and server test commands pass; focused store, migration, and completion suites also pass."},"concerns":["Independent Codex and Grok Final re-reviews are pending for the exact new frozen delivery commit."],"report_file":".superpowers/sdd/task-11-report.md"}
-->
