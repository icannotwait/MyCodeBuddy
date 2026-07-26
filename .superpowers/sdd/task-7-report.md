# Task 7 Report — Per-generation timestamps (accept path) + metrics

**Branch:** `feat/delegation-promote-reliability`  
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`  
**Date:** 2026-07-27  
**Implementer:** Grok  
**Base HEAD:** `d76dd7e7` (Tasks 1–6 complete)

## Status

**COMPLETE** — accept path samples `prompt_accepted_at` without post-send conversation lookup; live runtime rebase + promote use the same timestamp; accepted metrics (count + by-agent) for all generations with exactly-once task_id dedupe; promote/admission/settlement counter maps with documented pairing; interned audit codes; all named tests green.

## Summary

### Accept-path timestamps

| Before | After |
| --- | --- |
| `AcceptedDelegationPrompt.started_at` read from `conversation.delegation_started_at` after enqueue | `prompt_accepted_at` sampled at accept success (`Utc::now()`) |
| Missing/unreadable row timestamp failed acceptance | No conversation timestamp lookup; stale gen-1 values ignored |
| Gen-2 could inherit gen-1 projection start | Each generation carries its own accept sample into promote + rebase |

Promote transaction (Task 1) still persists:
- `run.started_at = prompt_accepted_at`
- `run.reached_running_at = max(now, prompt_accepted_at)`
- conversation `delegation_started_at = prompt_accepted_at` under generation fence

Live runtime projector rebases to the same accept sample before running publication (gen-1 and continuation).

### Metrics

| Counter / map | Semantics |
| --- | --- |
| `accepted_count` + `accepted_by_agent` | Durable `reserving → running` generations (including continuation); exactly-once per `task_id` via process-local set |
| `promote_retries` | `busy`, `locked`, `busy_snapshot` from attempt meta; `busy_snapshot` only when extended 517 classified |
| `promote_failures` | `cas`, `budget`, `busy_exhausted`, `permanent` |
| `admission_failed_by_agent` | On `admission_failed` settlement paths (not budget) |
| `settlement_retry_enqueued` / `settlement_retry_exhausted` | New owner after exhaust → both; existing owner → exhausted only; immediate settle success fence clear → neither |

Snapshot serde keeps existing fields; new maps/counters use `#[serde(default)]` empty defaults.

### Audit constants

Interned `&'static str` codes: `ADMISSION_FAILED_CODE`, `BUDGET_EXHAUSTED_CODE`, `ADMISSION_UNKNOWN_CODE`, `SPAWN_FAILED_CODE`, promote-failure labels, plus `PROMOTE_LOG_REQUIRED_FIELDS` / forbidden secret keys helper.

## Named tests

| Test | Result |
| --- | --- |
| `gen1_gen2_distinct_prompt_accepted_at` | PASS |
| `run_projection_runtime_share_prompt_accepted_at` | PASS |
| `reached_running_at_ge_started_at` | PASS |
| `stale_gen1_conversation_timestamp_not_reread` | PASS |
| `continuation_increments_accepted_count_and_by_agent` | PASS |
| `idempotent_promote_no_double_accepted_metric` | PASS |
| `commit_reread_success_emits_accepted_exactly_once` | PASS |
| `promote_failures_labels_cas_budget_busy_exhausted_permanent` | PASS |
| `admission_failed_by_agent_increments_on_admission_failed` | PASS |
| `settlement_retry_counter_pairing_new_vs_existing_owner` | PASS |
| `busy_snapshot_metric_only_on_extended_517` | PASS |
| `metrics_snapshot_default_empty_maps_serde` | PASS |
| `structured_promote_logs_include_required_fields_exclude_secrets` | PASS |

## Verify commands

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils --lib accepted_ -- --nocapture
# 14 passed (includes Task 7 accept/timestamp/metric broker tests)

cargo test --features test-utils --lib metrics -- --nocapture
# 33 passed (includes all new metrics unit tests)

cargo test --features test-utils --lib prompt_accepted -- --nocapture
# 2 passed

cargo test --features test-utils --lib promote_failures -- --nocapture
# 1 passed

cargo test --features test-utils --lib admission_failed_by_agent -- --nocapture
# 1 passed

cargo test --features test-utils --lib settlement_retry -- --nocapture
# 2 passed

cargo test --features test-utils --lib structured_promote -- --nocapture
# 1 passed

cargo test --features test-utils --lib commit_reread -- --nocapture
# 1 passed

cargo test --features test-utils --lib reached_running_at -- --nocapture
# 2 passed

cargo test --features test-utils --lib stale_gen1 -- --nocapture
# 1 passed

cargo test --features test-utils --lib busy_snapshot -- --nocapture
# 3 passed

cargo check
# Finished ok
```

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/spawner.rs` | `AcceptedDelegationPrompt.prompt_accepted_at`; docs — no post-send row lookup |
| `src-tauri/src/acp/manager.rs` | Sample accept time; `stale_gen1_conversation_timestamp_not_reread` |
| `src-tauri/src/acp/delegation/metrics.rs` | Maps/counters, exactly-once `record_accepted_for_task`, audit constants, unit tests |
| `src-tauri/src/acp/delegation/broker.rs` | Wire metrics on promote/settlement; continuation accepted; timestamp e2e tests |

## Commits

| Hash | Message |
| --- | --- |
| `01fe4032` | `feat(delegation): per-generation accept timestamps and admission metrics` |

## Concerns / residual

- Accepted-metric dedupe is **process-local** (`HashSet` of task ids). Host restart clears the set; a restarted process that re-observes an already-running durable row could count again if a success path re-emits. In practice post-restart reconcile does not re-admit already-running gens as new promotes.
- Settlement-retry metric pairing is instrumented on the post-accept admission settle path; other settle bootstrap owners may still use older paths without these counters (Task 4 bootstrap has separate terminal-metrics fencing).
- Structured promote log field coverage is asserted via a pure helper + existing tracing field set on promote outcomes; not a full tracing-subscriber capture of every emit site.
- Task 8 (full verification matrix) not started.

## Out of scope (confirmed)

- No Task 8 residual fixes
- No frontend card redesign
- No historic row migration
- No automatic prompt replay
