# Task 5 Report — `admission_failed` / `admission_unknown` recovery surface + warnings

**Branch:** `feat/delegation-promote-reliability`  
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`  
**Date:** 2026-07-26  
**Implementer:** Grok  
**Base HEAD:** `8c8d593e` (Tasks 1–4 complete)  
**Prior partial:** `bbb56bd5` (enum constants / allow-list / cold-report snippet only — incomplete)

## Status

**COMPLETE** — explicit replacement recovery surface for `admission_failed` / `admission_unknown`, lineage supersession fence, forge matrix, budget semantics, continue deny-list, cold + replacement-ack duplicate-execution warnings.

## Summary

### Surfaces

| Surface | Change |
| --- | --- |
| `tool_schema.json` | Enum already had codes (partial); description documents **explicit replacement only** (never `continue_delegation`) on tool + `replacement_reason` |
| `companion.rs` | `LEGACY_DELEGATE_DESCRIPTION` + tools/list tests assert enum values and description text |
| `listener.rs` | Allow-list already accepted both codes (from partial) |
| `run_store.rs` | Matchers (`error_code` + `reached_running_at IS NULL`); continue deny-list; **lineage supersession fence** |
| `types.rs` | Shared `ADMISSION_UNKNOWN_DUPLICATE_EXECUTION_WARNING`; cold failed reports use it |
| `broker.rs` | Successful `replacement_reason = admission_unknown` running ack appends the same warning |

### Lineage supersession

A source with `replaced_task_id` edges is **superseded** when any successor is:

- still active (`reserving` / `running`), or  
- has `reached_running_at IS NOT NULL` (budget charged).

Pure pre-admission aborts (terminal + never reached running) do **not** supersede, so the Skill may retry the same source linkage without consuming budget.

### Budget

- Failed replacement admit does not charge.  
- Reserving does not charge.  
- Exactly one successful promote charges lineage + work-unit replacement counters.

### Continue / unresumable

- `is_revision_eligible_failure` deny-lists both codes (defense in depth vs `reached_running` drift).  
- Decision-table tests assert non-continuable for both codes with `reached_running` true and false.  
- Codes use dedicated `replacement_reason` values — not represented as `unresumable`.

## Named tests

| Test | Result |
| --- | --- |
| `replacement_admission_failed_matches_only_lineage_latest_never_running` | PASS |
| `replacement_admission_unknown_matches_only_lineage_latest_never_running` | PASS |
| `replacement_admission_superseded_source_is_rejected` | PASS |
| `replacement_admission_forge_matrix_rejects_ineligible_sources` | PASS |
| `replacement_admission_failed_budget_only_on_successful_promote` | PASS |
| `admission_codes_are_not_revision_eligible_or_unresumable` | PASS |
| `continue_eligibility_decision_table_obeys_precedence_and_recovery_rules` (deny-list) | PASS |
| `cold_message_failed_admission_unknown_includes_duplicate_execution_warning` | PASS |
| `replacement_admission_unknown_ack_includes_duplicate_execution_warning` | PASS |
| `tools_list_exposes_continue_and_replacement_inputs` (enum + desc) | PASS |

Forge matrix covers: completed / running / reached-running / stale / mismatched-agent / incomplete-snapshot.

## Verify commands

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils --lib replacement_ -- --nocapture
# 25 passed

cargo test --features test-utils --lib admission_ -- --nocapture
# 39 passed

cargo test --features test-utils --lib cold_message -- --nocapture
# 4 passed

cargo check
# Finished ok
```

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/tool_schema.json` | Description: explicit-replacement-only recovery |
| `src-tauri/src/acp/delegation/companion.rs` | Legacy description + tools/list assertions |
| `src-tauri/src/acp/delegation/run_store.rs` | Supersession fence + full recovery matrix tests |
| `src-tauri/src/acp/delegation/types.rs` | Warning constant + cold-message test |
| `src-tauri/src/acp/delegation/broker.rs` | Replacement ack warning + broker integration test |
| `src-tauri/src/acp/delegation/listener.rs` | No new edits this commit (partial already allow-listed) |

## Commits

| Hash | Message |
| --- | --- |
| `6b50a100` | `feat(delegation): admission_failed/unknown explicit replacement recovery` |

## Concerns / residual

1. **Task 6 still pending** — startup reconcile bound → `admission_unknown` settlement is **not** implemented here; only recovery surface / matching / warnings for the durable codes.  
2. Supersession is checked before budget; second replace of a promoted source returns `InvalidReplacement(superseded)` rather than `BudgetExhausted` (more precise). Existing budget-exhaust tests that use a **fresh** terminal source under an exhausted lineage still pass.  
3. Incomplete-snapshot forge case asserts reason mismatch when launch fields are stripped and error_code is not admission_*; legitimate admission sources with full snapshots match.

## Out of scope (Tasks 6–8)

- Bound/unbound startup reconcile split  
- Metrics / timestamps  
- Full end-to-end verification wave  
