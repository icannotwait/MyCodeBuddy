# Auto-Title Direct Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace ACP automatic title generation with OpenAI-compatible `stream: false` chat completions using dedicated `api_url` / `api_key` / `model` settings, and split document translation onto its own agent setting.

**Architecture:** Keep `AutoTitleCoordinator` job lifecycle. Swap production `HiddenAgentRunner` for `DirectCompletionTitleRunner` over an injectable `TitleHttpTransport`. Persist URL/model/barrier/gen/fp in `app_metadata`; store the API key in `keyring_store` with tri-state reads and SHA-256 fingerprint binding. Document translate reads `document_translate_agent` with absent-only legacy fallback.

**Tech Stack:** Rust (SeaORM, reqwest, tokio, keyring_store), Tauri + Axum shared cores, Next.js/React settings UI, next-intl, vitest.

**Spec:** `docs/superpowers/specs/2026-07-22-auto-title-direct-completion-design.md` (post doc-review r8).

## Global Constraints

- Protocol: OpenAI-compatible `POST …/chat/completions` only; `"stream": false`; `temperature: 0`; `max_tokens: 128`.
- On when: non-empty trimmed url + key Present + non-empty model + `config_barrier == false` + live key fp matches stored fp.
- Secrets: never in GET/events/logs/Debug; account id `auto_title_api_key`.
- Fail-closed barrier/gen/fp write sequence as in the spec.
- **cancel_all** after every barrier raise / re-raise / path that leaves barrier set or gen bump that invalidates runners (not only success Off).
- One-shot purge all `auto_title_jobs` before recover (`auto_title_jobs_purged_for_api_v1`).
- Jobs bind `config_gen` (`i64` storage of monotonic generation; write uses checked convert from `u64`, panic/err on overflow past `i64::MAX`).
- Lazy HTTP client after `init_proxy_from_db`; injectable transport for tests.
- Keep `ConnectionManager` for `ManagerPartialSource` only.
- Remove `set_auto_title_agent` in the same change set that lands FE callers (Tasks 2+6 ship together or FE first in same PR sequence without green main broken — prefer **Task 2 backend + Task 6 frontend as sequential commits in one SDD wave without leaving main unbuildable overnight**; if split, Task 2 keeps a deprecated no-op shim only until Task 6 — **prefer single combined Task 2b** below).
- Local commits only; no push/PR.

## Dependency order

```text
Task 1 → Task 2 (settings BE) → Task 3 (migration/purge)
       → Task 4 (types + direct runner + enroll/claim; single compile unit)
       → Task 5 (FE settings + translate consumer)
       → Task 6 (integration + full verify gate)
```

---

### Task 1: Keyring tri-state + title key helpers

**Files:**
- Modify: `src-tauri/src/keyring_store.rs` (process mutex on all server tokens.json read/write; atomic temp+rename publish)
- Create: `src-tauri/src/auto_title/title_key.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Test: unit tests in those modules

**Produces:**
```rust
pub enum TitleKeyState { Present(String), Absent, Unavailable }
// manual Debug: Present => "Present(***)"
pub const TITLE_API_KEY_ACCOUNT: &str = "auto_title_api_key";
pub fn title_key_fingerprint(secret: &str) -> String; // hex_lower(SHA-256(utf8))
pub fn get_title_api_key() -> TitleKeyState;
pub fn set_title_api_key(secret: &str) -> Result<(), String>;
pub fn delete_title_api_key() -> Result<(), String>;
```

- [ ] Failing tests: fp stable; Debug redacts secret; Unavailable on read error; mutex/atomic prevents truncated JSON on concurrent read
- [ ] Implement
- [ ] `cargo test` targeted
- [ ] Commit `feat(auto-title): tri-state title API key and fingerprint helpers`

---

### Task 2: Settings BE + fail-closed write + translate agent loader

**Files:**
- `src-tauri/src/commands/conversation_experience.rs`
- `src-tauri/src/web/handlers/conversation_experience.rs`
- `src-tauri/src/web/router.rs`, `src-tauri/src/lib.rs`
- `src-tauri/src/document_translate/service.rs`
- URL helper may live in `auto_title/title_settings.rs` or commands module:
  - `normalize_and_validate_api_url(raw: &str) -> Result<String, AppCommandError>`
  - trim; parse; scheme http/https only; reject userinfo; strip query/fragment; store origin+path

**Produces GET document (exact fields):**
```rust
pub struct ConversationExperienceSettings {
    pub auto_title_api_url: String,
    pub auto_title_api_key_set: bool,
    pub auto_title_model: String,
    pub auto_title_config_barrier: bool,
    pub document_translate_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}
```

**ApiKeyUpdate serde** (custom or `#[serde(untagged)]` objects):
- `{ "keep": true }` → Keep
- `{ "set": "<nonempty>" }` → Set (reject empty set string)
- `{ "clear": true }` → Clear
- omitted / null field → Keep

**set_auto_title_api_config cancel obligations (mandatory):**
1. After barrier raise + gen bump + job wipe **commits** → `cancel_all`
2. Preflight Unavailable → barrier raise path + `cancel_all` + error
3. Keyring Set/Clear failure after barrier → leave barrier + `cancel_all` + error
4. Verify mismatch / unprovable / post-commit drift → re-raise barrier + wipe + gen + `cancel_all` + error
5. Success with field/enabled/job change → `cancel_all` as needed
6. Test: Set fails after barrier committed → no runner HTTP continues (use fake runner / cancel token observation)

**Write sequence:** full design r8 steps (verify key under barrier, atomic url/model/fp + clear barrier).

**Translate:** `load_document_translate_agent_from` — absent new key → legacy; present empty → Off; present agent → agent.

**Remove** `set_auto_title_agent` from router/lib **only if** Task 5 lands in the same delivery train before any release; if Task 2 merges alone, leave handler temporarily returning configuration_invalid pointing to new API — **prefer completing Task 5 immediately after Task 2 in SDD without long gap**.

**Named tests:** Keep/Set/Clear; no secret on get; URL validation matrix; barrier Off; translate absent/empty/legacy; preflight Unavailable; verify mismatch Keep/Set; cancel after barrier.

- [ ] Tests first
- [ ] Implement
- [ ] cargo test conversation_experience + document_translate loader
- [ ] Commit `feat(settings): auto-title API config and document translate agent`

---

### Task 3: Migration `config_gen` + one-shot purge

**Files:**
- Create: `src-tauri/src/db/migration/mYYYYMMDD_HHMMSS_auto_title_job_config_gen.rs`
- Modify: `src-tauri/src/db/migration/mod.rs` (**register Migrator**)
- Modify: entity `auto_title_job.rs` — `pub config_gen: i64`
- `purge_auto_title_jobs_for_api_v1_if_needed` before `recover_interrupted_jobs`

**Storage policy:** metadata gen as decimal `u64` string; job column `i64`; on write `i64::try_from(gen).map_err(...)` — never silent truncate.

- [ ] Tests: register migrator up; purge once; second start keeps new jobs after re-enroll
- [ ] Implement
- [ ] Commit `feat(auto-title): config_gen column and API-era job purge`

---

### Task 4: Direct runner + type migration + enroll/claim (single compile unit)

**Why combined:** Dropping `agent` from attempt breaks `HiddenAgentRunner` tests; ship type change with DirectCompletionTitleRunner and claim snapshot in one task.

**Files:**
- Create: `src-tauri/src/auto_title/http.rs`
- Modify: `types.rs`, `service.rs`, `coordinator.rs`, `runner.rs` (remove production HiddenAgentRunner wiring; delete or cfg-test only if unused)
- `build_production_coordinator`: `DirectCompletionTitleRunner` + lazy transport after proxy
- Claim interface (exact):

```rust
// claim_next_ready returns claim that already includes config snapshot
pub struct AutoTitleClaim {
    pub conversation_id: i32,
    pub attempt: i32,
    pub first_user_text: String,
    pub first_assistant_text: String,
    pub locale: AppLocale,
    pub attempt_turn_seq: i32,
    pub config: AutoTitleApiConfig, // loaded under claim txn + keyring mutex; redacted Debug
    pub config_gen: i64,
}

pub struct AutoTitleAttempt { /* from claim fields including config */ }

// On fp mismatch during claim load:
// set barrier, wipe jobs, gen+=1, cancel_all, return no claim / Unavailable — no HTTP
```

**Claim path:** single function `claim_next_ready_with_config(conn) -> Option<AutoTitleClaim>` that:
1. Begins txn
2. Reads enabled (barrier, url, model, gen, fp)
3. Reads TitleKeyState under mutex; checks Present + fp match
4. Claims job where `config_gen == current_gen`
5. Builds AutoTitleApiConfig snapshot
6. Commits

**Enroll:** same txn reads gen + enabled; insert job with that gen; conditional recheck.

**HTTP:**
```rust
#[async_trait]
pub trait TitleHttpTransport: Send + Sync {
    async fn post_json(&self, url: &str, bearer: &str, body: &serde_json::Value, cancel: &CancellationToken)
        -> Result<TitleHttpResponse, TitleHttpError>;
}
```
Safe errors only. Mock tests. Lazy proxy wiring test (client not constructed before proxy init — document factory).

**Named tests:** normalize/extract; 401/empty/timeout/cancel; enroll only when enabled; claim rejects bad gen; fp mismatch claim fail-closed; stale enroll vs save race; Clear+Set restart-after-commit shapes (unit with injectable stores if needed); concurrent tokens read during claim.

- [ ] Tests + implement as one unit
- [ ] cargo test auto_title (lib) with test-utils
- [ ] Commit `feat(auto-title): direct completion runner and API-config claims`

---

### Task 5: Frontend settings + translate consumer + i18n

**Files:**
- `src/lib/types.ts`, `src/lib/api.ts`
- `src/stores/conversation-experience-store.ts` (+tests)
- `src/components/settings/conversation-experience-settings.tsx` (+tests)
- `src/components/files/file-workspace-tab-bar.tsx` (+tests) — gate translate on `document_translate_agent`
- All 10 `src/i18n/messages/*.json`
- Any remaining `auto_title_agent` FE references

**UX acceptance:**
- Barrier true → show “configuration incomplete — re-save or re-enter key” (i18n key)
- Blank password + Save → Keep (not clear)
- Explicit Clear control → Clear
- Title HTTP disclosure separate from translate ACP disclosure

- [ ] Tests first (store, settings, file-workspace-tab-bar)
- [ ] Implement UI + i18n
- [ ] `pnpm test` affected + eslint touched
- [ ] Commit `feat(ui): auto-title API settings and translate agent split`

---

### Task 6: Integration + full verification gate

**Files:**
- `src-tauri/tests/api_integration.rs`
- Any leftover references

**Must assert:**
- New get/set shapes; revision; no secret in body/events
- Old `set_auto_title_agent` → 404/method-not-found
- Translate independent of title URL
- Historical `InternalSessionPurpose::Title` still deserializes/filters (unit or existing registry test)

**Full verify (run and report output):**
```text
pnpm eslint .
pnpm test
pnpm build
cd src-tauri
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```
(Scope down only if a command is known unrelated-failing pre-existing; report.)

- [ ] Integration tests
- [ ] Full verify gate
- [ ] Commit `test: auto-title API integration and verification`

---

## Safety matrix assignment

| Case | Task |
| --- | --- |
| Tri-state + mutex + Debug redaction | 1 |
| URL validate on set | 2 |
| cancel_all after barrier / error paths | 2 |
| preflight Unavailable, verify mismatch | 2 |
| config_gen migration + purge | 3 |
| claim snapshot + fp mismatch + enroll race | 4 |
| HTTP runner + lazy proxy | 4 |
| Set/Clear restart-after-commit | 4 (unit) |
| Barrier UX, Clear vs Keep password | 5 |
| file-workspace-tab-bar translate agent | 5 |
| Integration + full suite | 6 |

## Plan self-review

- Spec fail-closed cancel and claim snapshot assigned explicitly.
- Task 4 is one compile unit for type+runner.
- Migrator registration required.
- FE consumer file-workspace-tab-bar in Task 5.
- Full verify gate in Task 6.
- Serde for ApiKeyUpdate specified.
- No TBD.
