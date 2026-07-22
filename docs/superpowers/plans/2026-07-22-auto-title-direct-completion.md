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
- Fail-closed barrier/gen/fp write sequence as in the spec; no mixed endpoint+key claimable state.
- One-shot purge all `auto_title_jobs` before recover (`auto_title_jobs_purged_for_api_v1`).
- Jobs bind `config_gen`; claim rejects mismatch.
- Lazy HTTP client after `init_proxy_from_db`; injectable transport for tests.
- Keep `ConnectionManager` for `ManagerPartialSource` only; title runner has no ACP spawn.
- Remove `set_auto_title_agent`; FE+BE co-shipped.
- Parent/implementer: no push/PR; local commits only; TDD where feasible.
- Do not edit unrelated user files (e.g. other staged design docs).

## File map

| Path | Responsibility |
| --- | --- |
| `src-tauri/src/keyring_store.rs` | Tri-state get; mutex; atomic tokens.json write |
| `src-tauri/src/auto_title/title_key.rs` (new) | TitleKeyState, fp, account constants |
| `src-tauri/src/auto_title/http.rs` (new) | URL normalize, response extract, TitleHttpTransport, DirectCompletionTitleRunner |
| `src-tauri/src/auto_title/types.rs` | Drop agent from claim/attempt; AutoTitleApiConfig |
| `src-tauri/src/auto_title/service.rs` | enroll/claim enabled + gen + fp; purge |
| `src-tauri/src/auto_title/coordinator.rs` | wire DirectCompletionTitleRunner; purge on start |
| `src-tauri/src/auto_title/runner.rs` | remove or stop exporting production HiddenAgentRunner |
| `src-tauri/src/commands/conversation_experience.rs` | new settings document + setters |
| `src-tauri/src/web/handlers/conversation_experience.rs` | HTTP parity |
| `src-tauri/src/db/entities/auto_title_job.rs` + migration | `config_gen` column |
| `src-tauri/src/document_translate/service.rs` | load document_translate_agent |
| `src-tauri/src/lib.rs` / `bin/codeg_server.rs` | register commands; proxy order |
| `src/lib/types.ts`, `src/lib/api.ts` | wire types + API |
| `src/stores/conversation-experience-store.ts` | store actions |
| `src/components/settings/conversation-experience-settings.tsx` | UI |
| `src/i18n/messages/*.json` | 10 locales |
| Integration tests under `src-tauri/tests/` | API revision, no secret echo |

---

### Task 1: Keyring tri-state + title key helpers

**Files:**
- Modify: `src-tauri/src/keyring_store.rs`
- Create: `src-tauri/src/auto_title/title_key.rs`
- Modify: `src-tauri/src/auto_title/mod.rs` (export as needed)
- Test: unit tests in those modules

**Interfaces:**
- Produces:
  - `pub enum TitleKeyState { Present(String), Absent, Unavailable }`
  - `pub const TITLE_API_KEY_ACCOUNT: &str = "auto_title_api_key";`
  - `pub fn title_key_fingerprint(secret: &str) -> String` // hex lower SHA-256
  - `pub fn get_title_api_key() -> TitleKeyState`
  - `pub fn set_title_api_key(secret: &str) -> Result<(), String>`
  - `pub fn delete_title_api_key() -> Result<(), String>`
- Keyring: server path mutex on all read/write; prefer write temp+rename; `get_token` may stay Option for callers, but title uses new API that maps errors to Unavailable

- [ ] **Step 1: Write failing tests** for fingerprint stability, Present/Absent/Unavailable mapping (injectable or server-mode file), and concurrent write does not yield truncated JSON for a locked reader (if hard, test mutex serializes write+read).

- [ ] **Step 2: Implement helpers + keyring hardening**

- [ ] **Step 3: Run** `cargo test --features test-utils -p codeg-lib title_key keyring_store` (adjust package name to actual crate) from `src-tauri/`. Expected: pass.

- [ ] **Step 4: Commit** `feat(auto-title): tri-state title API key and fingerprint helpers`

---

### Task 2: Settings document + fail-closed `set_auto_title_api_config` + translate agent split

**Files:**
- Modify: `src-tauri/src/commands/conversation_experience.rs`
- Modify: `src-tauri/src/web/handlers/conversation_experience.rs`
- Modify: `src-tauri/src/web/router.rs` (routes)
- Modify: `src-tauri/src/lib.rs` (tauri command list)
- Modify: `src-tauri/src/document_translate/service.rs`
- Test: existing conversation_experience unit tests + new cases

**Interfaces:**
- Produces GET document:
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
- `set_auto_title_api_config` request:
```rust
pub enum ApiKeyUpdate {
    Keep,
    Set(String),
    Clear,
}
// serde: { "keep": true } | { "set": "..." } | { "clear": true }
pub struct SetAutoTitleApiConfigRequest {
    pub api_url: String,
    pub api_key_update: Option<ApiKeyUpdate>, // None = Keep
    pub model: String,
}
```
- Metadata keys per spec.
- `load_document_translate_agent_from`: absent new key → legacy `auto_title_agent`; present empty → Off.
- `auto_title_enabled(...)` shared helper used by enroll later.
- Remove `set_auto_title_agent` command/handler; replace tests.
- Implement barrier/gen/fp write sequence from spec (keyring verify under barrier, atomic clear with fp).
- Mutation gate + cancel_all on success path when required.

- [ ] **Step 1: Failing tests** for wire shape, Keep/Set/Clear, no secret on get/event, barrier forces Off, translate absent vs empty, legacy ignored for titles.

- [ ] **Step 2: Implement persistence + commands + web handlers**

- [ ] **Step 3: Run** targeted cargo tests for conversation_experience + document_translate loader.

- [ ] **Step 4: Commit** `feat(settings): auto-title API config and document translate agent`

---

### Task 3: Job `config_gen` migration + one-shot purge

**Files:**
- Create migration under `src-tauri/src/db/migration/`
- Modify: `src-tauri/src/db/entities/auto_title_job.rs`
- Modify: `src-tauri/src/auto_title/service.rs` / coordinator `recover_and_start`
- Test: purge once, flag set, second start no wipe of new jobs incorrectly

**Interfaces:**
- Column `config_gen: i64` (or i32) NOT NULL default 0
- `purge_auto_title_jobs_for_api_v1_if_needed(conn) -> Result<(), DbError>`
- Call **before** `recover_interrupted_jobs`

- [ ] **Step 1: Tests** for purge + flag idempotency

- [ ] **Step 2: Migration + purge helper + wire into recover_and_start**

- [ ] **Step 3: cargo test purge/migration related**

- [ ] **Step 4: Commit** `feat(auto-title): config_gen column and API-era job purge`

---

### Task 4: Enroll/claim use API enabled + gen + fp

**Files:**
- Modify: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/auto_title/types.rs` (`AutoTitleClaim`/`AutoTitleAttempt` drop `agent`, add config snapshot or load at claim)
- Modify coordinator claim path to pass snapshot
- Tests: enroll only when enabled; claim rejects bad gen/fp; Off deletes all job states

**Interfaces:**
```rust
pub struct AutoTitleApiConfig {
    pub api_url: String,
    pub api_key: String, // redacted Debug
    pub model: String,
}
pub struct AutoTitleAttempt {
    pub conversation_id: i32,
    pub attempt: i32,
    pub locale: AppLocale,
    pub first_user_text: String,
    pub first_assistant_text: String,
    pub config: AutoTitleApiConfig,
}
```

- [ ] **Step 1: Update tests that set auto_title_agent to set API config instead**

- [ ] **Step 2: Implement enroll/claim/load config snapshot**

- [ ] **Step 3: cargo test auto_title service/coordinator with fakes**

- [ ] **Step 4: Commit** `feat(auto-title): enroll and claim against API config epoch`

---

### Task 5: DirectCompletionTitleRunner + HTTP transport

**Files:**
- Create: `src-tauri/src/auto_title/http.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`, `coordinator.rs` `build_production_coordinator`
- Modify: `src-tauri/src/lib.rs` / `codeg_server.rs` if client factory timing needs change
- Deprecate production use of HiddenAgentRunner for titles

**Interfaces:**
```rust
pub struct TitleHttpResponse { pub status: u16, pub body: Vec<u8> }
pub enum TitleHttpError { Timeout, Cancelled, Transport, /* no URL in Display */ }

#[async_trait]
pub trait TitleHttpTransport: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        bearer: &str,
        body: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> Result<TitleHttpResponse, TitleHttpError>;
}

pub fn normalize_chat_completions_url(raw: &str) -> Result<String, /* safe error */>;
pub fn extract_completion_content(body: &[u8]) -> Option<String>;

pub struct DirectCompletionTitleRunner {
    transport: Arc<dyn TitleHttpTransport>,
}
impl TitleAgentRunner for DirectCompletionTitleRunner { ... }
```

- Safe error mapping per spec.
- Mock transport tests: success, 401, empty, timeout, cancel.
- Production transport: lazy reqwest after proxy; 30s timeout.

- [ ] **Step 1: Unit tests for normalize + extract + error map + mock runner**

- [ ] **Step 2: Implement runner and wire build_production_coordinator**

- [ ] **Step 3: cargo test auto_title http/runner**

- [ ] **Step 4: Commit** `feat(auto-title): direct non-streaming completion runner`

---

### Task 6: Frontend settings + API + i18n

**Files:**
- `src/lib/types.ts`, `src/lib/api.ts`
- `src/stores/conversation-experience-store.ts` (+ tests)
- `src/components/settings/conversation-experience-settings.tsx` (+ tests)
- `src/i18n/messages/{en,zh-CN,zh-TW,ja,ko,es,de,fr,pt,ar}.json`
- Any consumers of `auto_title_agent` in FE tests

**Interfaces:**
- `setAutoTitleApiConfig({ apiUrl, apiKeyUpdate, model })`
- `setDocumentTranslateAgent(agent | null)`
- UI: url, password key, Clear key, model, status, title HTTP disclosure; translate agent select + ACP disclosure

- [ ] **Step 1: Update types/api/store tests (fail then fix)**

- [ ] **Step 2: UI + i18n all 10 locales**

- [ ] **Step 3: `pnpm test` for affected + `pnpm eslint` on touched files**

- [ ] **Step 4: Commit** `feat(ui): auto-title API settings and translate agent split`

---

### Task 7: Integration tests + cleanup

**Files:**
- `src-tauri/tests/api_integration.rs` (and any auto_title agent references)
- Remove dead exports; ensure document_translate still works with new key

- [ ] **Step 1: Rewrite integration tests for new endpoints; assert no secret in JSON**

- [ ] **Step 2: `cargo test` integration subset; `cargo check` desktop + server**

- [ ] **Step 3: Commit** `test: auto-title API config integration and cleanup`

---

## Plan self-review

| Spec area | Task |
| --- | --- |
| Keyring + fp + mutex | 1 |
| Settings wire + barrier write + translate split | 2 |
| Job purge + config_gen column | 3 |
| Enroll/claim epoch + snapshot | 4 |
| Direct HTTP runner | 5 |
| UI/i18n | 6 |
| Integration | 7 |
| Clear crash test | covered under Task 2/4 unit tests as specified in design |

No TBD placeholders. Types consistent across tasks (`AutoTitleApiConfig`, `ApiKeyUpdate`, settings fields).

## Execution

Brainstorm-to-delivery mandates **subagent-driven-development** with Grok implementers and Codex reviewers after this plan is document-reviewed.
