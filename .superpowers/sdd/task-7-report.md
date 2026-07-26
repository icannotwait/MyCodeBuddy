# Task 7 Report — Per-generation timestamps (accept path) + metrics

**Branch:** `feat/delegation-promote-reliability`
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`
**Date:** 2026-07-27
**Implementer:** Grok
**Base HEAD:** `d76dd7e7` (Tasks 1–6 complete)

## Status

**COMPLETE** (fix round 1 applied) — accept path samples `prompt_accepted_at` without post-send conversation lookup; live runtime rebase + promote use the same timestamp; accepted metrics (count + by-agent) for all generations with exactly-once task_id dedupe; promote/admission/settlement counter maps with documented pairing; interned audit codes wired into production audit mappings; production structured promote logs with tracing-subscriber capture tests.

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
| `settlement_retry_enqueued` / `settlement_retry_exhausted` | New owner after exhaust (incl. reacquire/forced-put) → both; existing owner → exhausted only; immediate settle success fence clear → neither |

Snapshot serde keeps existing fields; new maps/counters use `#[serde(default)]` empty defaults.

### Audit constants

Interned `&'static str` codes via `intern_terminal_error_code`: `ADMISSION_FAILED_CODE`, `BUDGET_EXHAUSTED_CODE`, `ADMISSION_UNKNOWN_CODE`, `SPAWN_FAILED_CODE` (+ existing terminal codes). Used by both terminal audit mappings in broker.

### Structured logs

Production `emit_promote_structured_log` carries required fields (task_id, generation, agent_type label, admission_class, attempt, sqlite codes, failure_class). Aggregate broker-side logs after the promote retry loop (and settlement exhaust) — per-attempt logging stays in `run_store` outside the Task 7 file map. No raw `DbErr` / free-form promote messages.

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
| `settlement_retry_reacquire_owner_pairs_enqueued_and_exhausted` | PASS |
| `busy_snapshot_metric_only_on_extended_517` | PASS |
| `metrics_snapshot_default_empty_maps_serde` | PASS |
| `structured_promote_logs_include_required_fields_exclude_secrets` | PASS (tracing-subscriber capture) |
| `intern_terminal_error_code_covers_admission_budget_spawn` | PASS |

## Verify commands

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils --lib accepted_ -- --nocapture
# 14 passed

cargo test --features test-utils --lib metrics -- --nocapture
# 34 passed

cargo test --features test-utils --lib settlement_retry -- --nocapture
# 3 passed (includes reacquire race)

cargo test --features test-utils --lib structured_promote -- --nocapture
# 1 passed

cargo test --features test-utils --lib intern_terminal -- --nocapture
# 1 passed

cargo check
# Finished ok
```

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/spawner.rs` | `AcceptedDelegationPrompt.prompt_accepted_at`; docs — no post-send row lookup |
| `src-tauri/src/acp/manager.rs` | Sample accept time immediately on Ok(Some(cid)); stale-gen1 test |
| `src-tauri/src/acp/delegation/metrics.rs` | Maps/counters, emit helper, intern codes, tracing capture tests |
| `src-tauri/src/acp/delegation/broker.rs` | Wire metrics/logs/audit; settlement ownership through reacquire; tests |

## Commits

| Hash | Message |
| --- | --- |
| `01fe4032` | `feat(delegation): per-generation accept timestamps and admission metrics` |
| `a91a7121` | `docs(delegation): Task 7 accept timestamps and admission metrics report` |
| `8de146cf` | `fix(delegation): Task 7 metrics ownership audit and structured logs` |

## Concerns / residual

- Accepted-metric dedupe is **process-local** (`HashSet` of task ids). Host restart clears the set.
- Per-attempt promote logs in `run_store` remain minimal (outside Task 7 file map); broker aggregate logs satisfy the required field contract.
- Task 8 (full verification matrix) not started.

## Out of scope (confirmed)

- No Task 8 residual fixes
- No frontend card redesign
- No historic row migration
- No automatic prompt replay

---

## Fix round 1 (Codex review)

**Review:** `.superpowers/sdd/task-7-review.md` — CHANGES REQUIRED on Issues 1–3; Issues 4–5 fixed while touching accept path / docs.

### Issue 1 — Structured logs (Important)

- Added production `emit_promote_structured_log` with full required field set; broker outcome/failure/settlement exhaust paths use it.
- Removed free-form `error = %message` / raw `DbErr` from promote failure and settlement exhaust logs.
- **Logging policy:** aggregate broker logs after the promote retry loop (file-map constrained). Per-attempt `run_store` logs not amended (would need plan amendment).
- Replaced helper-only assertion with tracing-subscriber capture over the real emitter (`structured_promote_logs_include_required_fields_exclude_secrets`).

### Issue 2 — Settlement retry enqueued undercount on reacquire (Important)

- Carried `settlement_retry_owner` through initial put, reacquire claim, forced put, and freeze put.
- Exhaust pairing uses the effective owner flag (new owner after reacquire → enqueued + exhausted).
- Broker race test: `settlement_retry_reacquire_owner_pairs_enqueued_and_exhausted`.

### Issue 3 — Audit codes not wired (Important)

- `intern_terminal_error_code` maps admission/budget/unknown/spawn (+ prior terminal codes).
- Both terminal audit construction sites (`settle_task` winner + `finalize_durable_settlement`) use it.
- Test: `intern_terminal_error_code_covers_admission_budget_spawn` asserts production audit records for each new code.

### Issue 4 — Sample timing (Minor)

- `prompt_accepted_at` sampled immediately on `Ok(Some(cid))` before watchdog state lock await.

### Issue 5 — Diff hygiene (Minor)

- Removed Markdown trailing hard-break spaces from this report.

### Verify (fix round 1)

```powershell
cargo test --features test-utils --lib accepted_ -- --nocapture   # 14
cargo test --features test-utils --lib metrics -- --nocapture     # 34
cargo test --features test-utils --lib settlement_retry -- --nocapture  # 3
cargo test --features test-utils --lib structured_promote -- --nocapture # 1
cargo test --features test-utils --lib intern_terminal -- --nocapture    # 1
cargo check  # ok
```
