# Automatic Titles via Direct Completion API

Date: 2026-07-22

Status: Design approved; revised after document review r2 (fail-closed keyring/DB write)

## Summary

Replace the ACP-based automatic conversation title runner with a dedicated
OpenAI-compatible **non-streaming** chat completion call. Title generation is
configured with three fields only (`api_url`, `api_key`, `model`). When all
three are non-empty, automatic titles are On; otherwise Off.

Document translation currently shares the automatic-title agent setting. That
coupling is removed: translation keeps an independent ACP agent selector under
a new settings key. The title path no longer spawns agents or registers
**new** internal title sessions (existing `InternalSessionPurpose::Title` rows
remain filterable).

## Goals

- Generate automatic titles with a single `POST …/chat/completions` request
  (`stream: false`), without spawning an ACP agent CLI.
- Minimum user configuration: API base URL, API key, and model id.
- Keep the existing durable job lifecycle (enroll, claim, retry, finalize,
  deadline sweep, `auto_title_finalized`, title lock rules).
- Completely replace the title-agent picker; ignore legacy `auto_title_agent`
  for title On/Off and title attempts.
- Split document translation onto its own agent setting so translation remains
  available after the title path leaves ACP.
- Preserve desktop (Tauri) and server (Axum) parity through shared cores.
- Never let a leftover job from the ACP-title era send conversation text to a
  newly configured HTTP endpoint.

## Non-goals

- Anthropic Messages API, multi-protocol auto-detection, or SSE streaming titles.
- Reusing `model_provider` rows or agent env credentials for titles.
- Per-project title overrides or multiple title endpoints.
- A “test connection” button in v1.
- Migrating document translation to HTTP completion (stays ACP).
- Changing when titles trigger, context composition (first user + first usable
  assistant), retry count, or native-title vs generated-title precedence
  beyond swapping the runner and On predicate.
- Deleting agent CLI session files or changing `InternalAgentSessionRegistry`
  filtering for historical Title-purpose rows.
- Full OS-level encryption of SQLite; secrets use the existing keyring_store
  pattern (see Security).

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Implementation path | Replace production `HiddenAgentRunner` with `DirectCompletionTitleRunner`; keep coordinator |
| Protocol | OpenAI-compatible `chat/completions` only, `stream: false` |
| Config | Dedicated `api_url` + `api_key` + `model` (scheme B) |
| On/Off | All three fields non-empty ⇒ On; any empty ⇒ Off (no separate toggle) |
| Legacy title agent | Ignore for titles; no migration from agent → API |
| Document translate | Independent `document_translate_agent` (ACP); UI split from titles |
| API key UX | Password field + explicit **Clear key** control; wire uses discriminated `api_key_update` |
| Key on get | `auto_title_api_key_set: bool` only; never plaintext |
| Config while running | Snapshot credentials at claim; any title-config write cancels active runners after commit |
| Partial assistant text | Keep `ConnectionManager` + `ManagerPartialSource` for deadline sweep only |
| HTTP client | Lazy construction after proxy init; injectable transport for tests |

## Settings Model

### Settings document (GET / event payload)

Exact JSON field names (snake_case, shared by Tauri invoke result and Axum JSON):

```json
{
  "auto_title_api_url": "",
  "auto_title_api_key_set": false,
  "auto_title_model": "",
  "document_translate_agent": null,
  "reference_search_limit": 50,
  "revision": 0
}
```

Rust mirror:

```rust
pub struct ConversationExperienceSettings {
    pub auto_title_api_url: String,
    pub auto_title_api_key_set: bool,
    pub auto_title_model: String,
    pub document_translate_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}
```

Unknown fields on request: ignore (serde default / deny not required).
Event `conversation-experience-settings://changed` payload is this full
document only — never includes the API key secret.

Title enabled predicate (enroll, claim; UI “enabled” uses the same):

```rust
fn auto_title_enabled(
    url: &str,
    key_present: bool,
    model: &str,
    config_barrier: bool,
) -> bool {
    !config_barrier
        && !url.trim().is_empty()
        && key_present
        && !model.trim().is_empty()
}
```

`config_barrier == true` (metadata value `"1"`) **always disables** titles for
enroll and claim, even if url/key/model look complete. GET may expose
`auto_title_config_barrier: bool` (default false) so the UI can show “save
incomplete / re-enter key” when stuck; optional for v1 if status copy can
derive from `!enabled && url/model set`.

### Persistence

| Storage | Key / account | Content |
| --- | --- | --- |
| `app_metadata` | `conversation_experience.auto_title_api_url` | Trimmed base URL string |
| `app_metadata` | `conversation_experience.auto_title_model` | Trimmed model id |
| **keyring_store** | account id `auto_title_api_key` | Secret API key (same desktop OS keyring / server `tokens.json` pattern as chat-channel tokens) |
| `app_metadata` | `conversation_experience.document_translate_agent` | JSON `AgentType` when set; **absent** vs **empty string** distinguished (see below) |
| `app_metadata` | `conversation_experience.reference_search_limit` | unchanged |
| `app_metadata` | `conversation_experience.revision` | unchanged |
| `app_metadata` | `conversation_experience.auto_title_jobs_purged_for_api_v1` | one-shot upgrade flag `"1"` after job purge |
| `app_metadata` | `conversation_experience.auto_title_config_barrier` | `"1"` while title API config write is in-flight or compensation is uncertain; **forces Off** for enroll/claim until cleared after verified DB+keyring agreement |

`model_provider.api_key` remains plaintext SQLite for agent providers; title
API key deliberately uses keyring_store so title secrets are not a new
plaintext class beyond that pattern’s existing residual risk (server file
permissions / OS keyring).

### Legacy `conversation_experience.auto_title_agent`

- **Titles:** never read for On/Off, enroll, or claim.
- **Document translate loader precedence:**
  1. If metadata key `document_translate_agent` is **absent** → fall back to
     legacy `auto_title_agent` (parse rules identical to today’s title agent
     loader: empty/missing legacy ⇒ Off; corrupt ⇒ warn + Off).
  2. If key is **present** and value is empty string after load → **explicit
     Off** (do not fall back).
  3. If key is present and non-empty → parse as `AgentType`; corrupt JSON ⇒
     warn + Off (do not fall back).
  4. First successful `set_document_translate_agent` always writes the new key
     (including Off as empty string), so subsequent loads never need legacy.

### One-shot job purge (upgrade)

Before any title claim after this feature ships:

1. If `auto_title_jobs_purged_for_api_v1` is not `"1"`, delete **all** rows in
   `auto_title_jobs` (every state: `awaiting_turn`, `ready`, `running`,
   `retry_wait`) in one transaction and set the flag to `"1"`.
2. Run this during coordinator `recover_and_start` (and any server/desktop
   path that starts the coordinator) **before** `recover_interrupted_jobs`.
3. Rationale: leftover ACP-era jobs must never send historical conversation
   text to a user-configured HTTP endpoint after upgrade. No retroactive
   re-enrollment.

After purge, only conversations created while API config is enabled get new
jobs (existing enroll rule).

### Commands / HTTP wire contracts

#### `get_conversation_experience_settings`

- Request: empty / `{}`.
- Response: `ConversationExperienceSettings` as above.

#### `set_auto_title_api_config`

Request JSON (exact):

```json
{
  "api_url": "https://api.openai.com/v1",
  "api_key_update": { "keep": true },
  "model": "gpt-4o-mini"
}
```

`api_key_update` is a tagged object (exactly one variant):

| Variant | JSON | Effect |
| --- | --- | --- |
| Keep | `{ "keep": true }` | Leave keyring secret unchanged |
| Set | `{ "set": "<secret>" }` | Store non-empty secret (reject empty `set`) |
| Clear | `{ "clear": true }` | Delete keyring secret |

Also accept omitted `api_key_update` as **Keep** for convenience.

`api_url` / `model`: always required keys (may be `""`). Trim on write.
Incomplete config is allowed and results in Off.

When non-empty `api_url` after trim: parse as URL; require scheme `http` or
`https`; **reject** userinfo, and reject if parse fails. Query and fragment:
**strip** for storage (persist origin + path only) so suffix logic cannot
corrupt `?tenant=` URLs; if the operator needs a path prefix, they put it in
the path (e.g. `/v1`). Document that gateway tenant headers are out of scope
for v1.

SSRF / egress: Codeg treats the URL as an **operator-chosen trusted endpoint**
(same class as model providers). No allowlist in v1. Prefer `https` in UI
copy; `http` remains allowed for private LAN gateways. Server deployments
must not expose settings mutation to untrusted clients without their own
auth (`CODEG_TOKEN`).

On write (fail-closed, single mutation-gate critical section):

Cross-store rule: `app_metadata` and `keyring_store` are not one ACID
transaction. A durable **`auto_title_config_barrier`** forces enroll/claim Off
whenever the write is in progress or recovery cannot prove DB+keyring
agreement. No claim may run against a mixed endpoint+key pair.

**Precondition for Keep:** reading the old keyring value for restore is only
needed on Set/Clear. If Set/Clear requires the old secret for compensation
and the old secret is **unreadable**, set barrier + wipe jobs + return error
**before** any keyring mutation (operator must Clear or Set again).

Canonical sequence:

1. Under `ConversationExperienceMutationGate` for the entire operation.
2. Read old url, model, key presence; for Set/Clear also try to read old
   secret for compensation. If Set/Clear and old secret is required for
   restore but unreadable: DB-set barrier=`"1"`, delete all title jobs, bump
   revision, cancel_all, emit event (enabled false), return error — **stop**.
3. Compute `next_url`, `next_model`, keyring action, and intended
   `new_enabled` (ignoring barrier for “intent”; barrier always forces Off
   until cleared).
4. **Raise barrier first (DB transaction):** set
   `auto_title_config_barrier = "1"`; delete **all** `auto_title_jobs` rows;
   commit. From this point enroll/claim are Off regardless of triple.
   - If this fails: return error; no keyring change.
5. Apply keyring Keep / Set / Clear.
   - On keyring failure: leave barrier set; cancel_all; emit/get shows
     barrier Off path; return error. Operator re-saves or Clears.
6. DB transaction for intended config:
   - Write url + model.
   - If intended `new_enabled == false` after this write would still be Off
     even without barrier, jobs already wiped in step 4 (ok).
   - **Clear barrier** only in this same transaction after url/model writes.
   - Bump revision; commit.
7. **Commit outcome resolution:**
   - If commit **reports failure**: leave barrier set (do not clear); attempt
     keyring compensation (restore old secret or delete new); if compensation
     fails, leave barrier set and prefer keyring delete of the new secret so
     the triple cannot enable. cancel_all; emit barrier state; return error.
   - If commit **reports success**: **verify** by re-reading url, model,
     key presence, and barrier absence. Only if they match the intended
     post-write state may the operation return Ok. If verification fails
     (including ambiguous “commit error but data present” races): re-set
     barrier=`"1"`, wipe jobs, cancel_all, attempt fail-closed keyring
     (delete if verification cannot confirm keep), emit, return error.
8. On verified success only: cancel_all if fields/enabled/jobs changed;
   broadcast event without secret; return document.
9. Never retroactively enroll historical conversations on Off→On.
10. Process start: if barrier is `"1"`, skip title enroll/claim until a
    successful verified save clears it (or an explicit Clear-key + empty
    url/model save that verifies Off and clears barrier).
11. Late finalize always uses `claim_is_still_running` / cancel guards.

Invariant tests (required):

- Keyring Set fails after barrier raised → barrier remains; no claim.
- DB commit fails after keyring Set → barrier remains; compensation or
  delete; no claim with mixed pair.
- Ambiguous commit (verify mismatch) → barrier re-asserted; no claim.
- Unreadable old key before Set → barrier + stop before keyring mutate.
- Success → barrier cleared; enroll/claim only when triple complete.

#### `set_document_translate_agent`

- Request: `{ "agent": null | "<AgentType>" }` (same shape as former
  `set_auto_title_agent`).
- Validates base agent enabled+available when non-null.
- Writes new key only; does not touch title API fields.
- Does **not** purge title jobs.

#### `set_reference_search_limit`

Unchanged.

#### Compatibility for `set_auto_title_agent`

Remove the command/handler. Desktop and static-export server ship FE+BE
together. Remote clients still calling the old method receive method-not-found
/ 404; no temporary alias. Document in release notes.

### Frontend key UX

- Password input never pre-filled with secret.
- Unchanged + Save with `api_key_update: { keep: true }` when user did not
  type a new key and did not press Clear.
- Typing a new key → `{ set: "<value>" }` on Save.
- Explicit **Clear key** control sets a local `keyCleared` flag → Save sends
  `{ clear: true }` (blank password alone does **not** clear).

## Architecture

### Keep

- `AutoTitleCoordinator` (permits, claim, cancel_all, deadline sweep, wake).
- `ConnectionManager` **only** as input to `ManagerPartialSource` for deadline
  partial assistant text — not used by the title runner.
- Job table and conversation flags (`auto_title_finalized`, `title_locked`).
- Enrollment when title API config is enabled.
- Prompt capture / first usable turn → ready job.
- `build_title_prompt`, `normalize_generated_title`.
- Finalize path and claim-still-running checks so cancelled/stale attempts
  cannot finalize.
- `InternalSessionPurpose::Title` enum variant and hide filters (no new
  registrations from title runner).

### Replace

| Component | Change |
| --- | --- |
| Production runner | `DirectCompletionTitleRunner` |
| `HiddenAgentRunner` | Remove from production wiring; delete if unused after translate-only paths checked |
| `AutoTitleClaim` / `AutoTitleAttempt` | Drop `agent: AgentType`; carry or load `AutoTitleApiConfig` snapshot |
| On predicate | `auto_title_enabled` + keyring presence instead of `load_auto_title_agent_from` |

### Private config type

```rust
/// Never Serialize/Debug the key field.
pub struct AutoTitleApiConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
}

impl AutoTitleApiConfig {
    pub fn is_enabled(&self) -> bool { /* three non-empty after trim */ }
}
```

Implement `Debug` manually that redacts `api_key` as `"***"`.

**Claim boundary (chosen):** coordinator loads config under gate/claim
transaction, builds `AutoTitleAttempt { …, config: AutoTitleApiConfig }`
**snapshot**, then `runner.run(attempt, cancel)`. Mid-flight settings changes
cancel the attempt via `cancel_all`; a late success must fail
`claim_is_still_running` / finalize guards so it cannot write.

### Direct completion runner

```text
claim snapshot → build_title_prompt
              → POST {endpoint} stream:false
              → extract choices[0].message.content
              → normalize_generated_title
              → Ok(title) | Err(AutoTitleRunError)
```

**Endpoint normalization** (after URL parse validation on save; runner still
defensive-normalize):

1. Trim; parse URL; reject userinfo; use scheme/host/port/path only.
2. Strip trailing `/` on path.
3. If path ends with `/chat/completions`, use as-is.
4. Else append `/chat/completions`.
5. Never put the full URL with key into errors/logs.

**Request body (v1 fixed defaults)**

```json
{
  "model": "<model>",
  "stream": false,
  "temperature": 0,
  "max_tokens": 128,
  "messages": [
    { "role": "user", "content": "<prompt>" }
  ]
}
```

Headers: `Authorization: Bearer <api_key>`, `Content-Type: application/json`.

**HTTP client / proxy**

- Do **not** build a process-global `reqwest::Client` inside
  `build_production_coordinator` at the current desktop site that runs
  **before** `init_proxy_from_db`.
- Use a lazy client factory: first request (or first runner construct after
  app setup) creates the client so proxy env is already applied; or pass a
  `Arc<dyn TitleHttpTransport>` created after proxy init in both desktop
  `lib.rs` setup and `codeg_server` (server already inits proxy before
  coordinator — keep that order).
- Trait for tests:

```rust
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
```

**Timeouts / cancel**

- HTTP timeout 30s.
- CancellationToken aborts the in-flight request.
- Coordinator retry/deadline policy unchanged.

**Response parsing**

- `choices[0].message.content` string, or concatenate `text` from array parts.
- Missing/empty after normalize → `EmptyOutput`.

**Error mapping (safe messages only)**

| Condition | Error | Message content allowed |
| --- | --- | --- |
| Cancel | `Cancelled` | unit |
| Timeout | `Timeout` | unit |
| Config incomplete at run | `Unavailable` | unit |
| HTTP 401/403 | `Unavailable` | unit |
| Other HTTP | `AbnormalStop` | `"http_status=<code>"` only |
| Transport / JSON parse | `AbnormalStop` | short fixed labels: `"transport_error"`, `"invalid_json"` — **no** URL, body, prompt, or `reqwest` Display |
| Empty title | `EmptyOutput` | unit |

Never log Authorization, api_key, prompt, or response body. Structured logs:
conversation_id, http status, error kind only.

### Production wiring

`build_production_coordinator` takes (or builds after proxy) an
`Arc<dyn TitleHttpTransport>` and `AppDatabase`. No ConnectionManager on the
runner. Coordinator still receives ConnectionManager for partial source.

## UI

### Automatic titles

1. API Base URL text field.
2. API Key password field + **Clear key** control when `api_key_set`.
3. Model text field.
4. Status: enabled iff three fields complete.
5. **Disclosure (new, separate from translate):** first user message and first
   usable assistant reply are sent to the configured endpoint for title
   generation; use a trusted provider; prefer HTTPS.
6. Save → `set_auto_title_api_config`.

### Document translation

1. Agent select Off + enabled/available base agents.
2. ACP disclosure only (existing wording, retargeted off title agent).

### i18n

All ten locales: new title API labels, clear-key, enable status, title HTTP
disclosure, translate agent labels; remove misleading shared-agent copy.

## Data flow (title)

```text
Upgrade start → one-shot purge all auto_title_jobs
New conversation + API config enabled → enroll
Linked prompt → first_user_text + locale
Usable end_turn → ready
Claim → snapshot AutoTitleApiConfig → HTTP stream:false → normalize → finalize
```

## Testing

| Layer | Cases |
| --- | --- |
| URL helper | trailing slash, full path, strip query/fragment, reject userinfo |
| Content extract | string, array parts, missing |
| Error map | 401, 500, timeout, cancel, empty; assert message has no URL/key |
| Enabled predicate | empty field combinations |
| Runner (mock transport) | success; 401; empty; timeout; cancel aborts |
| Keyring set/clear/keep | persistence without echoing secret on get |
| api_key_update wire | keep / set / clear |
| Upgrade purge | all job states deleted once; flag set; no second purge wipe of new jobs incorrectly |
| Translate loader | absent→legacy; present empty→Off; present agent→agent; no title enable from legacy |
| Service | enroll only when enabled; Off deletes all job states + cancel |
| Proxy/lazy client | client not built before proxy init in desktop wiring (unit or wiring test) |
| API integration | revision; no secret in event; translate independent |
| FE | clear-key vs keep; status copy; disclosures split |
| Registry | Title purpose still deserializes / filters |

## Migration checklist

1. Keyring + metadata keys; settings APIs; remove `set_auto_title_agent`.
2. One-shot job purge before recover.
3. Title enroll/claim use API enabled predicate.
4. Document translate new key + absent-only legacy fallback.
5. Direct runner + lazy transport; drop production HiddenAgentRunner for titles.
6. UI + i18n.
7. Update integration tests.

## Spec self-review (post doc-review fixes)

- Translate Off vs legacy fallback: **absent vs present-empty** rule is explicit.
- Upgrade: one-shot full job table purge before recover.
- Secrets: keyring_store; events never carry key; redacted Debug; safe errors.
- set_auto_title_api_config: keyring first for Set/Clear; DB second; compensate
  keyring on DB failure; prefer Off over mixed endpoint+old key.
- Claim snapshot + cancel on config write.
- Clear-key UI + `api_key_update` discriminant.
- Exact GET/event field names vs setter request names documented.
- URL parse + strip query/fragment; trusted-operator SSRF posture.
- Lazy client / proxy order; injectable transport.
- ConnectionManager retained for partial source only.
- InternalSessionPurpose::Title retained for historical rows.
- Old command removed without alias (FE+BE co-shipped).
