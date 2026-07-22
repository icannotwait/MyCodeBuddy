# Automatic Titles via Direct Completion API

Date: 2026-07-22

Status: Design approved; written specification awaiting final review

## Summary

Replace the ACP-based automatic conversation title runner with a dedicated
OpenAI-compatible **non-streaming** chat completion call. Title generation is
configured with three fields only (`api_url`, `api_key`, `model`). When all
three are non-empty, automatic titles are On; otherwise Off.

Document translation currently shares the automatic-title agent setting. That
coupling is removed: translation keeps an independent ACP agent selector under
a new settings key. The title path no longer spawns agents or registers
internal title sessions.

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
  behavior for non-title purposes (e.g. document translate).

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Implementation path | Replace `HiddenAgentRunner` with `DirectCompletionTitleRunner`; keep coordinator |
| Protocol | OpenAI-compatible `chat/completions` only, `stream: false` |
| Config | Dedicated `api_url` + `api_key` + `model` (scheme B) |
| On/Off | All three fields non-empty ⇒ On; any empty ⇒ Off (no separate toggle) |
| Legacy title agent | Ignore for titles; no migration from agent → API |
| Document translate | Independent `document_translate_agent` (ACP); UI split from titles |
| API key UX | Password field; empty on save keeps existing key; get uses `api_key_set` + mask, not plaintext echo |
| Key display on get | Prefer `api_key_set: bool` (and optional masked placeholder); never require clients to round-trip secrets |

## Settings Model

### Settings document

```rust
pub struct ConversationExperienceSettings {
    /// Non-empty trimmed values only when configured.
    pub auto_title_api_url: String,
    /// Never return plaintext on get; use `auto_title_api_key_set` instead.
    pub auto_title_api_key_set: bool,
    pub auto_title_model: String,
    /// Independent ACP agent for document translation (Off = None).
    pub document_translate_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}
```

Title enabled predicate (shared by enroll, claim, and UI status copy):

```rust
fn auto_title_enabled(url: &str, key_present: bool, model: &str) -> bool {
    !url.trim().is_empty() && key_present && !model.trim().is_empty()
}
```

At runtime, `key_present` means a non-empty stored secret (or a non-empty
incoming key on the same write).

### Persistence keys (`app_metadata`)

| Key | Content |
| --- | --- |
| `conversation_experience.auto_title_api_url` | Base URL string |
| `conversation_experience.auto_title_api_key` | Secret string (empty = cleared) |
| `conversation_experience.auto_title_model` | Model id string |
| `conversation_experience.document_translate_agent` | JSON `AgentType` or empty = Off |
| `conversation_experience.reference_search_limit` | unchanged |
| `conversation_experience.revision` | unchanged monotonic revision |

Legacy key `conversation_experience.auto_title_agent`:

- **Titles:** never read for On/Off or attempts.
- **Document translate:** if `document_translate_agent` is unset/empty, loaders
  may fall back to the legacy key once for compatibility; first explicit save
  of the translate agent should write the new key. Titles must not re-enable
  from this key.

### Commands / HTTP

Replace `set_auto_title_agent` with title-config APIs that share cores:

- `get_conversation_experience_settings` — return the new document shape.
- `set_auto_title_api_config` — body: `{ api_url, api_key: Option|null, model }`.
  - `api_key: null` or omitted with “keep” semantics: leave stored key unchanged.
  - `api_key: ""` (explicit empty string): clear stored key.
  - Validate `api_url` is non-empty only when enabling is intended is **not**
    required field-by-field for partial drafts: allow saving incomplete config
    (results in Off). When URL is non-empty, require `http://` or `https://`
    after trim; reject other schemes.
  - On transition **On → Off** (enabled becomes false after write): delete all
    pending title jobs in the same transaction and cancel active runners
    (same policy as current agent Off).
  - On transition **Off → On** or credential change while On: do not
    retroactively enroll historical conversations (unchanged product rule).
- `set_document_translate_agent` — same validation as former title-agent save
  (base agent, enabled + available), independent of title API config.
- `set_reference_search_limit` — unchanged.

Each setter increments `revision`, returns the full document, and broadcasts
`conversation-experience-settings://changed`.

Frontend store and types mirror the new fields; remove `auto_title_agent` from
the conversation-experience settings type.

## Architecture

### Keep

- `AutoTitleCoordinator` (permits, claim, cancel_all, deadline sweep, wake).
- Job table and conversation flags (`auto_title_finalized`, `title_locked`).
- Enrollment on new live conversations when title config is enabled.
- Prompt capture / first usable turn → ready job.
- `build_title_prompt`, `normalize_generated_title`.
- Finalize path and conversation upsert events after commit.

### Replace

| Component | Change |
| --- | --- |
| `TitleAgentRunner` impl | Production uses `DirectCompletionTitleRunner` |
| `HiddenAgentRunner` | Remove from production wiring; delete or confine to tests if unused |
| `AutoTitleClaim` / `AutoTitleAttempt` | Drop `agent: AgentType`; runner loads API config itself or receives a small `AutoTitleApiConfig` snapshot on claim |
| On predicate | `load_auto_title_api_config` / enabled helper instead of `load_auto_title_agent_from` |
| Internal title sessions | Title runs no longer spawn connections or register `InternalSessionPurpose::Title` |

`InternalAgentSessionRegistry` remains for document translate and any other
internal purposes.

### Direct completion runner

```text
claim → build_title_prompt(locale, user, assistant)
     → POST {endpoint} stream:false
     → extract choices[0].message.content
     → normalize_generated_title
     → Ok(title) | Err(AutoTitleRunError)
```

**Endpoint normalization**

1. Trim whitespace.
2. Strip trailing `/`.
3. If path already ends with `/chat/completions`, use as-is.
4. Else append `/chat/completions`.

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

**Timeouts / cancel**

- Overall HTTP timeout ≈ 30s (tighter than the old 90s ACP spawn budget is fine).
- Honor `CancellationToken`: abort the in-flight request when cancelled.
- Coordinator overall job policy (retries, deadline sweep) stays as today unless
  a constant must shrink for the faster path; do not invent a second retry policy.

**Response parsing**

- Prefer `choices[0].message.content` as a string.
- If `content` is an array of parts (some gateways), concatenate `text` fields.
- Missing choices, null content, or whitespace-only after normalize →
  `EmptyOutput`.

**Error mapping**

| Condition | `AutoTitleRunError` |
| --- | --- |
| Cancelled token | `Cancelled` |
| Wall timeout / client timeout | `Timeout` |
| Config missing at run start | `Unavailable` |
| HTTP 401 / 403 | `Unavailable` |
| HTTP other 4xx/5xx, transport, invalid JSON | `AbnormalStop(message)` |
| Empty usable title | `EmptyOutput` |

Map coordinator retry behavior using existing `record_attempt_failure` rules.
Do not introduce interactive permission/question errors on this path.

**Security**

- Never log `api_key` or full `Authorization` headers.
- Prefer structured logs with status code and conversation id only.

### Production wiring

`build_production_coordinator` constructs `DirectCompletionTitleRunner` with
`AppDatabase` (and a shared `reqwest::Client` if the process already has one;
otherwise a dedicated client with the process proxy config). No
`ConnectionManager` dependency for titles.

## UI

### Automatic titles (General → Conversation experience)

Replace the agent `<Select>` with:

1. API Base URL text field.
2. API Key password field (placeholder when `api_key_set`).
3. Model text field.
4. Helper text: enabled when all three are set; otherwise Off.
5. Save action calling `set_auto_title_api_config`.

Remove title-side copy that claims the title agent is used for document
translation.

### Document translation

New control group:

1. Agent select: Off + enabled/available base agents (same availability rules
   as the former title agent picker).
2. Disclosure: document text is sent to that agent/provider; Codeg hides
   internal sessions from its lists but does not delete the CLI’s storage.

### i18n

Update all ten locale catalogs: new labels for URL/key/model, enable status,
save errors, document-translate agent strings; remove or repurpose obsolete
title-agent-only strings that would mislead.

## Data flow (title)

```text
New conversation + config enabled
  → enroll job (awaiting_turn)

Linked prompt capture
  → first_user_text + locale

Usable end_turn on target conversation
  → first_assistant_text; job ready

Coordinator claim
  → DirectCompletionTitleRunner.run
  → HTTP completion (stream:false)
  → normalize
  → finalize_generated_title / fail+retry
```

## Testing

| Layer | Cases |
| --- | --- |
| URL helper | trailing slash, already full path, empty |
| Content extract | string content, array parts, missing |
| Error map | 401, 500, timeout, cancel, empty |
| Enabled predicate | all combinations of empty fields |
| Runner (mock HTTP) | success title; 401; empty; timeout; cancel |
| Service | enroll only when enabled; clearing key deletes pending jobs |
| API integration | set/get revision; key not echoed; translate agent independent |
| FE | form keep-key-on-empty; status copy; translate select isolated |
| Translate | reads `document_translate_agent` (+ legacy fallback); not title URL |

## Migration checklist

1. Ship new metadata keys and APIs.
2. Stop reading `auto_title_agent` for title enrollment/claim.
3. Document translate: new key + legacy fallback.
4. UI switch; update i18n.
5. Remove production `HiddenAgentRunner` wiring for titles.
6. Update integration tests that call `set_auto_title_agent`.

## Open implementation notes (non-blocking)

- Whether `set_auto_title_api_config` is one combined setter (preferred) vs
  three independent setters: **one combined setter** for atomic enable/disable
  and single revision bump.
- Exact JSON field names on the wire should match existing snake_case transport
  conventions in this repo (`api_url`, `api_key`, `model`, `api_key_set`).
- `AutoTitleRunError::Spawn` / `Identity` / `Registry` / `Interactive` become
  unused on the title path; keep variants if still referenced by tests or map
  unused arms only in the old runner deletion PR—avoid drive-by enum churn
  unless compile forces it.

## Spec self-review

- No TBD/TODO placeholders left for product decisions.
- Title path and translate path are explicitly split; no dual-use agent for titles.
- Scope is one implementation plan: settings + runner swap + UI + tests.
- Ambiguity resolved: empty key on save keeps secret; explicit `""` clears;
  incomplete config allowed and means Off.
