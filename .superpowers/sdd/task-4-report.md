# Task 4 report — DirectCompletionTitleRunner + type migration + enroll/claim

**STATUS:** DONE  
**Branch:** `main`  
**Commit:** `5ba04c48` — `feat(auto-title): direct completion runner and API-config claims`

## Scope delivered

1. **`auto_title/http.rs` (new)**
   - `TitleHttpTransport` + `TitleHttpResponse` / `TitleHttpError` (safe messages only)
   - `LazyReqwestTitleTransport` — `new()` does **not** build `reqwest::Client`; first `post_json` does (proxy-safe after `init_proxy_from_db`)
   - `normalize_chat_completions_url`, `extract_completion_content`
   - `DirectCompletionTitleRunner` — OpenAI-compatible `stream: false`, `temperature: 0`, `max_tokens: 128`
   - Error map: cancel/timeout/401|403→Unavailable/empty→EmptyOutput/other HTTP→`http_status=N` only

2. **Type migration**
   - `AutoTitleApiConfig` with redacted `Debug` (never Serialize)
   - `AutoTitleClaim` / `AutoTitleAttempt`: **drop `agent`**, add `config` + `config_gen`
   - `AutoTitleAttempt::from_claim`

3. **Enroll**
   - On when `auto_title_enabled` (url + key Present + model + barrier clear)
   - Inserts job with **live** `config_gen` from metadata (same connection as create txn)

4. **Claim**
   - `claim_next_ready_with_config(conn, mutation_gate)` holds gate for whole snapshot
   - Keyring read under process mutex (via `get_title_api_key`); fp match required
   - fp mismatch / unprovable key → barrier raise + wipe jobs + gen bump → `Err(Unavailable)` (no HTTP); coordinator `cancel_all`
   - Stale `config_gen` ready rows deleted one-by-one (no blanket write upgrade before CAS — avoids WAL race deadlock)
   - Result: `Ok(None)` / `Ok(Some(claim))` / `Err(Unavailable|AbnormalStop("db_error"))`

5. **Wiring**
   - `build_production_coordinator(db, cm, chat, emitter, mutation_gate)` → DirectCompletion + lazy transport
   - Desktop `lib.rs` / `codeg_server.rs` / `AppState::new_for_test*`: **one** `ConversationExperienceMutationGate` Arc shared by settings + coordinator
   - Production `HiddenAgentRunner` wiring removed; ACP runner + tests kept under `cfg(any(test, feature = "test-utils"))`

## Files

| Path | Change |
| --- | --- |
| `src-tauri/src/auto_title/http.rs` | **new** transport + DirectCompletionTitleRunner + tests |
| `src-tauri/src/auto_title/types.rs` | AutoTitleApiConfig; Claim/Attempt without agent |
| `src-tauri/src/auto_title/service.rs` | enroll gen; claim_next_ready_with_config; named tests |
| `src-tauri/src/auto_title/coordinator.rs` | DirectCompletion wiring; gate field; Unavailable → cancel_all |
| `src-tauri/src/auto_title/runner.rs` | HiddenAgentRunner cfg test-only; export build_title_prompt |
| `src-tauri/src/auto_title/mod.rs` | re-exports |
| `src-tauri/src/lib.rs` | shared gate + coordinator |
| `src-tauri/src/bin/codeg_server.rs` | shared gate + coordinator (after proxy init) |
| `src-tauri/src/app_state.rs` | test AppState shares gate with coordinator |
| `src-tauri/src/document_translate/runner.rs` | doc comment only |

## Tests run

```text
cd src-tauri
cargo test --features test-utils --lib auto_title:: -- --test-threads=1
cargo check
cargo check --no-default-features --bin codeg-server
```

| Suite | Result |
| --- | --- |
| `auto_title::` (126) | **ok** |
| includes `http::` (11) | normalize/extract/401/empty/timeout/cancel/safe errors/lazy client |
| includes claim/enroll named | enroll_only_when_enabled; claim_rejects_bad_gen; fp_mismatch_claim_fail_closed; post_save_key_overwrite_at_claim; stale_enroll_vs_save_race; set_and_clear_restart_after_commit_shapes; AutoTitleApiConfig Debug redacts |
| `concurrent_tokens_write_during_claim_read_coherent` | server-mode only (`not(tauri-runtime)`) — not run under default desktop feature set |
| production `cargo check` / `codeg-server` | **ok** |

## Concerns / follow-ups

1. **Concurrent tokens.json claim test** is `#[cfg(not(feature = "tauri-runtime"))]` — run under server/lib features if needed for full coverage of mutex/atomic publish during claim.
2. **Claim holds mutation_gate** for the entire claim loop iteration (including CAS retries). Settings writes serialize behind claim; intentional fail-closed, watch for latency if claim CAS retries spike.
3. **Keyring test hooks** push finite Present overrides in unit tests; long claim loops could exhaust and hit real keyring — 32–64 pushes used; suite is green.
4. **Task 5** still owns FE cutover + delete `set_auto_title_agent`.
5. **Task 6** full verify gate (eslint/pnpm/clippy -D warnings) not run here.

---

# Task 4 review-fix — Important findings (no Critical)

**STATUS:** DONE  
**Branch:** `main`  
**Commit:** `87ad8b7e` — `fix(auto-title): fail-closed Absent claim and desktop proxy order`  
**Review:** `.superpowers/sdd/task-4-review.md`

## Fixes

### Important 1 — Desktop proxy before title coordinator

`src-tauri/src/lib.rs`: moved `init_proxy_from_db` **before** `build_production_coordinator` + `recover_and_start`. Recovered ready jobs can no longer build the lazy `reqwest::Client` with stale (pre-proxy) process env. Server order was already correct.

### Important 2 — Absent key fail-closed when configured-looking

`load_claim_config_snapshot` Absent branch now probes with `auto_title_enabled(&url, true, &model, barrier)` (key_present=true). If url+model look complete and barrier is clear, returns `Err(Unavailable)` → barrier raise + wipe + gen+=1. Quiet `Ok(None)` only when config is genuinely incomplete/off. Reappearance of an old key cannot resume titles without a verified re-save.

### Important 3 — cancel independent of barrier wipe persist

`apply_claim_unavailable_fail_closed` always returns `Err(Unavailable)` even when `fail_closed_barrier_wipe_jobs` fails (logged, not mapped to `AbnormalStop`). Coordinator `Unavailable` path always `cancel_all`. Wipe is retried on the next claim that observes the same drift (sweep re-notifies remaining ready rows).

## Tests added

| Test | Asserts |
| --- | --- |
| `absent_key_with_configured_url_model_fail_closed` | Absent + url/model On → Unavailable, barrier=1, gen+=1, jobs wiped |
| `absent_key_with_empty_config_quiet_off` | Absent + empty config → Ok(None), no barrier |
| `fail_closed_wipe_failure_still_returns_unavailable` | Forced wipe fail → Unavailable (not AbnormalStop) |
| `claim_unavailable_cancels_active_attempts` | Coordinator inject Unavailable → active blocked runner cancelled |

## Verification

```text
cd src-tauri
cargo test --features test-utils --lib auto_title:: -- --test-threads=1
# 130 passed; 0 failed
```

## Files

| Path | Change |
| --- | --- |
| `src-tauri/src/lib.rs` | proxy init before coordinator recover_and_start |
| `src-tauri/src/auto_title/service.rs` | Absent fail-closed; wipe-fail → Unavailable; tests + wipe-fail hook |
| `src-tauri/src/auto_title/coordinator.rs` | claim_unavailable_cancels_active_attempts test |

---

# Task 4 re-review r2 fix — Clear quiet Off + concurrent tokens

**STATUS:** DONE  
**Branch:** `main`  
**Review:** `.superpowers/sdd/task-4-review-r2.md`

## Fixes

### Important 1 — Verified Clear is quiet Off, not config drift

`load_claim_config_snapshot` Absent branch now fail-closes **only when** `stored_fp` is **non-empty** (a verified Set left an expected key identity) **and** url/model look On. Verified Clear keeps url/model, writes empty `auto_title_api_key_fp`, and clears the barrier; Absent + empty fp is `Ok(None)` with no barrier raise / gen bump. External key deletion still fail-closes when fp is non-empty.

Regression: `verified_clear_retained_url_model_claim_quiet_off` — empty fp + retained url/model + Absent → quiet Off, barrier stays clear, gen unchanged.

### Important 2 — Concurrent non-title tokens + compile

- Server-only `concurrent_tokens_write_during_claim_read_coherent` imports `AppDatabase` (fixes E0422 under `--no-default-features`).
- Writers update **other** `tokens.json` accounts via `keyring_store::set_token`, not the title key.
- Asserts claim is `Ok(Some|None)` only (panics on `Unavailable`), barrier not raised, gen unchanged, title key Present with matching fp.

## Verification

```text
cd src-tauri
cargo test --features test-utils --lib auto_title:: -- --test-threads=1
# 131 passed; 0 failed

cargo test --no-default-features --lib auto_title:: -- --test-threads=1
# 134 passed; 0 failed (includes concurrent_tokens_write_during_claim_read_coherent)
```

## Files

| Path | Change |
| --- | --- |
| `src-tauri/src/auto_title/service.rs` | Absent fail-closed gated on non-empty fp; Clear quiet-Off test; concurrent non-title token test |

---

# Task 4 re-review r3 fix — concurrent overlap proof + parallel-safe env

**STATUS:** DONE  
**Branch:** `main`  
**Review:** `.superpowers/sdd/task-4-review-r3.md`

## Fixes

### Important 1 — Prove write/claim overlap

- Added server-mode `keyring_store::write_hold_hooks`: after a successful `set_token` publish, optionally hold `tokens_mutex` until the test releases.
- Rewrote `concurrent_tokens_write_during_claim_read_coherent`:
  - **Phase 1:** arm hold → non-title write completes under the process mutex → release → claim must not spuriously `Unavailable`.
  - **Phase 2:** barrier-started concurrent writers; claims run only after the first successful non-title write; **every** write must `Ok(())` (`writer_count * iters`).
  - No sleep-based races for overlap; barrier + hold hook are deterministic.

### Important 2 — Parallel-safe env / hooks

- Concurrent test uses `temp_env::async_with_vars` for `CODEG_DATA_DIR` (restore on every exit; serializes with other `temp_env` tests).
- Added `title_key::test_hooks::SuiteGuard` (process suite mutex + reset on enter/Drop).
- Service/coordinator enable helpers and hook-using tests hold `SuiteGuard` for the full test body.
- Coordinator fixtures embed `_title_key_suite` so parallel workers cannot steal override queues.
- `enable_auto_title` / `enable_title_api` no longer call `set_title_api_key` (would write into another test’s active `CODEG_DATA_DIR`).
- Title-key file-store unit tests take suite lock inside `temp_env` (same lock order as concurrent).

Product Clear path unchanged (already correct in r2/r3 recheck).

## Verification

```text
cd src-tauri
cargo test --features test-utils --lib auto_title::
# 131 passed; 0 failed (default parallel harness)

cargo test --no-default-features --lib auto_title::
# 134 passed; 0 failed (default parallel harness; includes concurrent_tokens_…)
```

## Files

| Path | Change |
| --- | --- |
| `src-tauri/src/keyring_store.rs` | `write_hold_hooks` for deterministic publish/read under mutex |
| `src-tauri/src/auto_title/title_key.rs` | `SuiteGuard`; title-key store tests take suite lock under `temp_env` |
| `src-tauri/src/auto_title/service.rs` | Concurrent test rewrite; suite lock on hook tests; no ambient store write from enable helper |
| `src-tauri/src/auto_title/coordinator.rs` | Suite lock on fixtures + enable helper override-only |
