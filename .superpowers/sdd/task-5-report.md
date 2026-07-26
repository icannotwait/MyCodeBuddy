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
- has `reached_running_at IS NOT NULL` (budget charged), or  
- terminal with `admission_failed` / `admission_unknown` (even when  
  `reached_running_at IS NULL` — not a proven pure pre-send abort), or  
- a pure pre-admission abort that itself has a further replacement successor  
  (transitive `A ← B ← C`).

**Pure pre-admission abort** = terminal failed/canceled + never reached running  
+ **not** admission recovery codes. Only a pure abort that left **no** successor  
may be ignored so the Skill can retry the same source linkage without budget charge.

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
| `b74da6c3` | `fix(delegation): Task 5 lineage supersession and admission matchers` |
| `ce3d907f` | `fix(delegation): scope Task 5 snapshot guard to admission reasons` |

## Fix round 1 (Codex review)

**Status:** ADDRESSED — Critical ×1 + Important ×4

### Critical

1. **Lineage supersession across replacement edges**  
   - `replacement_source_is_superseded_txn` no longer treats all never-promoted terminals as pure pre-send aborts.  
   - Pure pre-admission abort = terminal failed/canceled + `reached_running_at IS NULL` + **not** `admission_failed`/`admission_unknown`.  
   - Pure abort supersedes only when it itself has a further replacement successor (A←B←C).  
   - Terminal post-accept `admission_*` successors always supersede.  
   - Tests: `replacement_admission_terminal_post_accept_successor_supersedes_source`, `replacement_admission_transitive_lineage_supersedes_source`.

### Important

1. **`status = Failed` required** for both admission matchers; forged completed/canceled + matching code + NULL reached_running rejected (`replacement_admission_requires_failed_status`).  
2. **Incomplete snapshot guard** via `launch_snapshot_from_run` + `snapshot_is_complete` on every replacement source; forge matrix strips snapshot only while keeping failed/admission_*/NULL reached_running valid.  
3. **Admission codes never match `unresumable`** even when workspace/route/external missing (`replacement_admission_codes_do_not_match_unresumable` + matcher unit asserts).  
4. **Idempotent acks**: `decorate_admission_unknown_replacement_ack` shared by fresh running path and `gen1_idempotent_ack` (from persisted `replacement_reason`); test `replacement_admission_unknown_idempotent_ack_includes_duplicate_execution_warning`.

### Verify (fix round 1)

```powershell
cargo test --features test-utils --lib replacement_ -- --nocapture   # 30 passed
cargo test --features test-utils --lib admission_ -- --nocapture     # 44 passed
cargo test --features test-utils --lib cold_message -- --nocapture   # 4 passed
cargo check                                                          # ok
```

## Fix round 2 (re-review residual)

**Status:** ADDRESSED — Important ×1 (snapshot scope) + Minor ×1 (report wording)

1. **Snapshot completeness scoped to admission reasons only**  
   The `launch_snapshot_from_run` / `snapshot_is_complete` guard runs only when  
   `replacement_reason` is `admission_failed` or `admission_unknown`. Established  
   `unresumable` matching still accepts missing workspace/route.  
   Regression: `replacement_unresumable_allows_missing_route_without_snapshot_guard`.

2. **Report supersession summary** updated to the final admission-aware /  
   transitive rule (no longer documents round-0 active/reached-running-only wording).

### Verify (fix round 2)

```powershell
cargo test --features test-utils --lib replacement_ -- --nocapture   # 31 passed
cargo test --features test-utils --lib admission_ -- --nocapture     # 44 passed
cargo test --features test-utils --lib cold_message -- --nocapture   # 4 passed
cargo check                                                          # ok
```

## Concerns / residual

1. **Task 6 still pending** — startup reconcile bound → `admission_unknown` settlement is **not** implemented here.  
2. Supersession is checked before budget; second replace of a promoted / admission-terminal successor returns `InvalidReplacement(superseded)`.  
3. Pure pre-admission abort retry of the original source remains allowed when the abort left no successor (budget still uncharged until promote).

## Out of scope (Tasks 6–8)

- Bound/unbound startup reconcile split  
- Metrics / timestamps  
- Full end-to-end verification wave  
