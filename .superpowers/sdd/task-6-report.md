# Task 6 Report — Auto-title direct completion (integration + full verify)

**STATUS:** DONE_WITH_CONCERNS  
**Branch:** `main`  
**BASE (pre-task HEAD):** `c4f61ceeabc84ab0917d80871639d95133cb2f79`  
**HEAD:** `b8b69e7f06015919df770a37975bbaafeda53fb9`  
**Pushed:** no (local only)

## Commits (this task / gate work since BASE)

| SHA | Message |
| --- | --- |
| `94e350a6` | (includes) extended `api_integration` HTTP round-trip for set shapes / secrets / translate independence |
| `9c6e5c5f` | `test(auto-title): migrate enrollment fixtures to API-config enable helper` |
| `b8b69e7f` | `test: auto-title API integration and verification` |

Note: `880429a3` (UI save guard) landed between BASE and Task 6 fixture work; out of Task 6 scope but present on the branch.

## Must-assert coverage

| Requirement | Evidence | Status |
| --- | --- | --- |
| New get/set shapes; revision | `tests/api_integration.rs::conversation_experience_settings_http_round_trip` — GET defaults; `POST /set_auto_title_api_config` with `{ keep: true }` returns URL/model/key_set/barrier/revision | **PASS** |
| No secret in body/events | Same test asserts no `auto_title_api_key` / `api_key` / `api_key_update` on GET/SET; body string must not contain `sk-http-round-trip`. Concurrent gate test asserts event payloads omit secrets | **PASS** |
| Old `set_auto_title_agent` unavailable | `POST /api/set_auto_title_agent` → **501** + `code: not_implemented` (project web fallback for unregistered routes; not bare 404) | **PASS** |
| Translate independent of title URL | After title set, `set_document_translate_agent` Off keeps URL/model/key_set; revision still bumps | **PASS** |
| `InternalSessionPurpose::Title` deserializes/filters | Existing `auto_title::internal_sessions` unit tests (9) register/persist/filter Title purpose; entity `string_value = "title"` | **PASS** |

## Leftover fixes applied (verify gate)

1. **Enrollment fixtures still used legacy `KEY_AUTO_TITLE_AGENT`**  
   Added `enable_title_api_for_test` (+ SuiteGuard) and migrated ACP/manager, automation, chat channel, conversation_service, import_service enrollment tests.  
   Commit: `9c6e5c5f`.

2. **Clippy `-D warnings` blockers in auto-title / server keyring**  
   - `#[allow(dead_code)]` on claim fail-closed hooks  
   - `#[allow(clippy::too_many_arguments)]` on coordinator test constructor  
   - needless `return` cleanup in claim config match  
   - `Default` derive for `ApiKeyUpdate`  
   - `read_tokens_map()?` in server-mode delete paths (`keyring_store.rs`)  
   Commits: `9c6e5c5f`, `b8b69e7f`.

3. **`pnpm build` TS error**  
   `SetAutoTitleApiConfigParams` not assignable to `Record<string, unknown>` — cast at transport boundary in `src/lib/api.ts`.  
   Commit: `b8b69e7f`.

## Full verify gate

| Command | Result | Notes |
| --- | --- | --- |
| `pnpm eslint .` | **FAIL (scoped as pre-existing)** | ~118833 prettier `Delete ␍` CRLF errors across the tree on Windows; **not** introduced by Task 6. Auto-title scoped FE files pass when linted alone. |
| `pnpm test` | **PASS** | Vitest full suite (prior run: 274 files / 3556 tests) |
| `pnpm build` | **PASS** | After `api.ts` cast fix; Next.js 16 static export, 32 routes |
| `cargo check` | **PASS** | Desktop default features |
| `cargo test --features test-utils` | **PASS** | After enrollment fixture migration (was 12 lib failures on legacy agent enable) |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | **PASS** | After clippy cleanups |
| `cargo check --no-default-features --bin codeg-server` | **PASS** | |
| `cargo test --no-default-features --bin codeg-server --lib` | **PASS** | |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | **PASS** | After keyring `?` + auto-title clippy fixes |
| `cargo check --no-default-features --bin codeg-mcp` | **PASS** | |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | **PASS** | |

### First-run failures (before fixes) — for the record

- **12 lib tests** expected auto-title enrollment when only legacy `conversation_experience.auto_title_agent` was set (ignored by new On predicate). Fixed via API enable helper.  
- **Clippy:** dead_code hooks, too_many_arguments, needless_return, derivable_impls, question_mark.  
- **Build:** `SetAutoTitleApiConfigParams` index signature.  
- **ESLint full tree:** CRLF prettier noise (pre-existing on this Windows checkout).

### Logs

- Frontend first pass: `.superpowers/sdd/task-6-verify-log.txt`  
- Cargo first pass: `.superpowers/sdd/task-6-cargo-log.txt`  
- Cargo re-verify: `.superpowers/sdd/task-6-cargo-log2.txt`  
- Build re-verify: `.superpowers/sdd/task-6-build-out2.txt`

## Integration tests (focused)

```text
cargo test --features test-utils --test api_integration conversation_experience_settings_http_round_trip
→ ok

cargo test --features test-utils internal_sessions --lib
→ 9 passed
```

## Concerns

1. **`pnpm eslint .` still fails workspace-wide** on Windows due to CRLF/prettier (`Delete ␍`) mass noise. Treat as environment/checkout line-ending issue, not an auto-title regression. CI on LF-normalized trees may still pass.  
2. Enrollment tests now require exclusive `SuiteGuard` + override queue discipline (FIFO); parallel suites remain exclusive by design.  
3. Intermediate commit `880429a3` is UI work outside Task 6; present on the same branch history since BASE.

## Checklist

- [x] Integration tests covering must-assert items  
- [x] Full verify gate run and reported (eslint scoped exception documented)  
- [x] Commits for test/code changes (local only; no push/PR)

## Fix (r2 Important #1 — desktop invoke regression)

**Finding:** Production `set_auto_title_api_config` has `rename_all = "snake_case"` but no desktop invoke regression sending the FE payload shape.

**Change:**
- Enable `tauri/test` under `test-utils` for `tauri::test::{mock_builder, get_ipc_response}`.
- Add test-only wire probe `set_auto_title_api_config_ipc_wire_probe` with the same FE arg names + `rename_all = "snake_case"` + `TauriApiKeyUpdateArg` (MockRuntime cannot host the production command because `EventEmitter::Tauri` is `AppHandle<Wry>`).
- Invoke regression covers:
  1. Exact FE shape `{ api_url, api_key_update: { keep: true }, model }` succeeds
  2. Omitted `api_key_update` deserializes as Keep
- Source pin asserts production command keeps `rename_all = "snake_case"`.

**Commands / results:**

```text
cargo test --features test-utils --lib desktop_ipc
→ ok: 2 passed
  desktop_ipc_fe_snake_case_payload_succeeds
  production_command_pins_snake_case_rename_all

cargo clippy --all-targets --features test-utils -- -D warnings
→ ok
```

**Concerns:** Production Wry-bound command is not invoked under MockRuntime; wire probe + source pin cover the FE snake_case contract and Keep-on-omit CommandArg path.
