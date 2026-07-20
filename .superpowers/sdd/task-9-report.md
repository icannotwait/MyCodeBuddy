# Task 9 Report: End-to-end / integration hardening

## Status

**DONE**

## Commits

| SHA | Message |
| --- | --- |
| `06000815852e02ded1beaaccbdd8e9209f2c32ef` | test(auto-title): complete deadline sweep coverage |

Short: `06000815`

Branch: `feat/auto-title-deadline-sweep`

Prior Tasks 1–8 already covered cases 1–15. Task 9 filled gaps **#16** and **#17**.

## Files changed

| Path | Change |
| --- | --- |
| `src-tauri/src/auto_title/service.rs` | **Modify** — `legacy_null_first_prompt_at_end_turn_only` (#16) |
| `src-tauri/src/auto_title/coordinator.rs` | **Modify** — `full_path_capture_promote_claim_finalize` (#17) |

No production logic changes. Optional `api_integration.rs` test not needed (module-level integration sufficient).

## ActiveModel audit

Grep `first_user_text: Set` under `src-tauri/` — all exhaustive `auto_title_job::ActiveModel { ... }` sites set `first_prompt_at` (production enroll + all test seeds in service/coordinator/manager/conversation_experience). No missing fields.

## Coverage checklist mapping

| # | Case | Test location | Status |
| --- | --- | --- | --- |
| 1 | Capture write-once first_prompt_at | `service::capture_sets_first_user_and_first_prompt_at_once`, `first_user_text_is_write_once_across_subsequent_captures` | ✅ existing |
| 2 | Two-connection capture: one first-field writer | `service::concurrent_captures_only_one_writes_first_fields` (**WAL**) | ✅ existing |
| 3 | Promote age ≥ deadline with partial / empty | `service::promote_deadline_ready_with_partial_and_empty` | ✅ existing |
| 4 | Young job not promoted | `service::promote_skips_young_and_retry_wait_and_null_prompt_at` | ✅ existing |
| 5 | retry_wait / ready / running not promoted | same as #4 | ✅ existing |
| 6 | Deadline vs end-turn **both orders** | `service::concurrent_end_turn_and_deadline_both_orders_wal` (**WAL barrier**) | ✅ existing |
| 7 | Two distinct tokens advance `usable_turn_seq` twice | `service::two_distinct_usable_tokens_advance_seq_twice` (**WAL**) | ✅ existing |
| 8 | Claim `Some("")` ok; `None` deleted | `service::claim_accepts_empty_assistant_some_empty_string`, `claim_deletes_ready_with_none_assistant` | ✅ existing |
| 9 | Claim lost-race: completion between read and CAS | `service::claim_retries_after_usable_turn_seq_changes_between_read_and_cas` (**WAL barrier**) | ✅ existing |
| 10 | Select candidates then delete/Off before promote CAS | `service::promote_select_then_delete_before_cas_is_noop` | ✅ existing |
| 11 | `live_message = None` clears stale; helper equality | `session_state::turn_complete_clears_stale_when_live_message_is_none`, `turn_complete_matches_visible_assistant_text_helper`, `visible_assistant_text_*` | ✅ existing |
| 12 | Multi-connection newest live + id tie-break | `partial_source::picks_newest_live_message_among_matches`, `equal_started_at_tie_breaks_by_connection_id_ascending` | ✅ existing |
| 13 | Immediate startup sweep; fail-once continues; lost-wake re-notify | `coordinator::startup_runs_immediate_sweep_before_interval`, `sweep_continues_after_transient_failure`, `lost_wake_ready_row_is_renotified_and_claimed` | ✅ existing (short real intervals; no `start_paused` on Windows/sqlx) |
| 14 | Double `recover_and_start` → one notify + one sweeper | `coordinator::double_recover_and_start_single_notification_and_sweep_loops` | ✅ existing |
| 15 | Migration last + index column order + down preserves queue index | `m20260719_…::up_adds_first_prompt_at_and_deadline_index`, `migrator_registers_deadline_migration_last` | ✅ existing |
| 16 | Legacy NULL `first_prompt_at` never deadline-promoted; end-turn still works | **NEW** `service::legacy_null_first_prompt_at_end_turn_only` (+ partial skip in #4; migration NULL seed) | ✅ added |
| 17 | Full path capture → promote → claim → finalize (mock runner) | **NEW** `coordinator::full_path_capture_promote_claim_finalize` | ✅ added |

### New test intent

**#16 `legacy_null_first_prompt_at_end_turn_only`**
- Seeds upgraded legacy row (`first_user_text` set, `first_prompt_at` NULL, aged-enough wall clock irrelevant).
- Asserts not in `list_deadline_candidates` and forced `promote_deadline_jobs_by_ids` is 0.
- End-turn still → Ready with assistant snapshot; does **not** invent `first_prompt_at`.
- Subsequent capture does **not** backfill `first_prompt_at` / rewrite `first_user_text`; locale still refreshes.

**#17 `full_path_capture_promote_claim_finalize`**
- Enroll → real `capture_prompt_context` stamps first fields → coordinator with `deadline: ZERO` + `FixedPartialSource` → immediate sweep promote → claim → `FakeRunner` → job deleted, title set, `auto_title_finalized`.

## Verification

```powershell
cd src-tauri
cargo test --features test-utils auto_title
# 98 passed (lib) + 1 api_integration auto_title test

cargo test --features test-utils visible_assistant
# 3 passed

cargo check
# ok

cargo check --no-default-features --bin codeg-server
# ok
```

Clippy (`-D warnings`) not required as a hard gate per job note (may fail on pre-existing build.rs baseline). Not re-run as a blocker.

## Concerns

1. **#13 uses short real intervals**, not `start_paused` (Task 8 Windows/sqlx `PoolTimedOut` note) — still green with generous 2s timeouts.
2. **Full path uses `deadline: ZERO`** to avoid wall-clock aging; production 300s path is covered by service promote age tests (#3/#4).
3. **No new HTTP integration test** — coordinator + service module integration is sufficient for the plan’s preferred path.
4. **Service suites remain green** after #16/#17; no production behavior changed.

---

## Codex finding fix (checklist #7)

**Finding:** `two_distinct_usable_tokens_advance_seq_twice` used two WAL connections but lacked a barrier/pre-write hook, so the two `apply_usable_completion` paths could fully serialize and fail to prove atomic `usable_turn_seq` increment under true concurrent updates.

**Fix (test-only; production code untouched):**
- Park both apply paths at the existing task-local `first_ready_race_hooks` **completion pre-write** gate.
- Shared `tokio::sync::Barrier(2)` releases only after **both** reach the gate, forcing concurrent progress UPDATEs with distinct tokens.
- Seed Ready job at `usable_turn_seq = 0`; assert final `usable_turn_seq == 2`, reported seqs `{1, 2}`, gate counter == 2, timeouts bound the handshake.

**File:** `src-tauri/src/auto_title/service.rs` only.

### Verification

```powershell
cd src-tauri
cargo test --features test-utils two_distinct_usable_tokens_advance_seq_twice
# 1 passed

cargo test --features test-utils auto_title
# 98 passed (lib) + 1 api_integration
```
