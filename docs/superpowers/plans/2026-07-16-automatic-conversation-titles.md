# Automatic Conversation Titles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate one durable, locale-aware title for each newly created root or delegated conversation after its first usable successful response, without exposing the title runner or weakening manual-title precedence.

**Architecture:** A migration adds the generated-title guard, durable job queue, and internal-session exclusion registry. Prompt admission captures bounded visible context and attaches an immutable completion sidecar only to the in-process lifecycle delivery; an `AutoTitleCoordinator` claims ready jobs after acquiring one of two permits and runs the selected base agent through an isolated `EventEmitter::Noop` connection. A revisioned shared settings document and transport-neutral frontend store keep desktop, server, workspace, and settings windows converged.

**Tech Stack:** Rust 2021, SeaORM/SQLite, Tokio, `tokio-util::CancellationToken`, Tauri 2, Axum, React 19, TypeScript strict mode, Zustand, Vitest, next-intl.

## Global Constraints

- Automatic titles apply only to new live Codeg root and delegated conversations; historical rows, raw-session imports, and fork-history siblings never receive jobs.
- `auto_title_agent` is one global enabled and available base `AgentType`, or `None` for Off; delegation profiles and fallback agents are excluded.
- The trigger is the first distinct non-empty `TurnComplete { stop_reason: "end_turn" }`; abnormal, cancelled, refused, token-limited, or empty turns do not consume an attempt.
- Context is only the first visible task and first usable visible final response; each is capped at 4,000 Unicode scalar values as 2,995 prefix + `\n...\n` + 1,000 suffix.
- A manual rename always wins, and `auto_title_finalized = true` prevents every later native CLI title refresh.
- Attempt one may retry exactly once, only after the target conversation's next distinct usable response; attempt two is terminal.
- Internal title launches use a Codeg-owned temporary directory, `EventEmitter::Noop`, no Codeg MCP injection, no interactive permission path, a 90-second total deadline, and at most two concurrent attempts.
- Internal agent sessions remain hidden from every Codeg parser-backed list, detail, folder, statistic, sidebar, refresh, and import path by registry ID, discovery lease, and reserved-root fallback; raw CLI files are not deleted.
- `EventEnvelope`, the private connection stream, recent-event replay ring, webview event, and WebSocket payload retain their existing Rust and serialized shape. Only an `InternalEventEnvelope` wrapper on `InternalEventBus` may retain the completion sidecar.
- Settings persist the two user fields under separate `app_metadata` keys and one monotonic revision key; setters modify only their own field and return the full post-write document.
- All conversation-experience setter wrappers share one process-local mutation gate from persistence through runtime side effects and event emission, so an older committed request cannot cancel work or change a runtime limit after a newer request has already applied.
- Both Tauri commands and Axum handlers call the same cores, and every production behavior begins with a focused failing test.
- Frontend files use no semicolons, two spaces, trailing commas, and `@/*` imports; Rust remains valid in desktop and `--no-default-features` server builds.

---

## Sequencing Note

This plan owns the shared `ConversationExperienceSettings` persistence, combined getter, revision event, process-local mutation gate, and frontend store. It implements both persisted fields so independent writes can be tested, but exposes only the automatic-title setter in this plan. The incremental-reference-search plan consumes these exact interfaces and adds the reference-limit runtime effect, endpoint, and numeric control through the same gate.

## File Map

- `src-tauri/src/db/migration/m20260716_000001_auto_title.rs`: generated-title column, durable job table, internal-session table, constraints, indexes, and migration tests.
- `src-tauri/src/db/migration/mod.rs`: registers the migration after `m20260630_000001_conversation_parent_id_index`.
- `src-tauri/src/db/entities/auto_title_job.rs`: SeaORM model for durable job state.
- `src-tauri/src/db/entities/internal_agent_session.rs`: SeaORM model for permanently excluded external sessions.
- `src-tauri/src/db/entities/conversation.rs`: `auto_title_finalized` persistence field.
- `src-tauri/src/db/entities/mod.rs`: exports the two new entities.
- `src-tauri/src/models/conversation.rs`: mirrors `auto_title_finalized` in DB summaries used by fork/title tests.
- `src-tauri/src/commands/conversation_experience.rs`: revisioned settings persistence, shared mutation gate, validation, commands, and global event emission.
- `src-tauri/src/commands/mod.rs`: exports the settings command module.
- `src-tauri/src/auto_title/types.rs`: job state, prompt-capture, turn-snapshot, claim, attempt, and result types.
- `src-tauri/src/auto_title/context.rs`: safe prompt projection, reference-label folding, scalar bounding, title prompt, and output normalization.
- `src-tauri/src/auto_title/service.rs`: enrollment, capture, completion transition, claims, recovery, failure, cancellation, and atomic title commit.
- `src-tauri/src/auto_title/internal_sessions.rs`: discovery lease, in-memory/persisted ID filter, reserved-root path filter, and identity registration.
- `src-tauri/src/auto_title/runner.rs`: runner trait plus isolated ACP implementation.
- `src-tauri/src/auto_title/coordinator.rs`: two-permit durable worker, active cancellation map, startup recovery, and commit retry.
- `src-tauri/src/auto_title/mod.rs`: focused public exports.
- `src-tauri/src/lib.rs`: module export, desktop managed states, startup recovery, lifecycle argument, and Tauri command registration.
- `src-tauri/src/app_state.rs`: server/test `AutoTitleCoordinator` and `InternalAgentSessionRegistry` handles.
- `src-tauri/src/bin/codeg_server.rs`: server construction, recovery, and lifecycle wiring.
- `src-tauri/src/web/mod.rs`: carries the same registry/coordinator handles into the embedded Axum `AppState` built by desktop mode.
- `src-tauri/src/acp/session_state.rs`: connection purpose, effective locale, opaque turn token, and immutable completion snapshot creation.
- `src-tauri/src/acp/event_stream.rs`: assertions that public replay entries remain ordinary `EventEnvelope` values.
- `src-tauri/src/acp/internal_bus.rs`: `InternalEventEnvelope`, backward-compatible sidecar-free sends, and sidecar delivery tests.
- `src-tauri/src/web/event_bridge.rs`: public envelope creation plus internal-only sidecar delivery during `emit_with_state`.
- `src-tauri/src/acp/lifecycle.rs`: atomic status/job transition, coordinator notification, and sidecar-only broker/title consumption.
- `src-tauri/src/acp/manager.rs`: launch purpose, admission ordering, prompt capture, private stream subscription, fork preservation, and cancellation hooks.
- `src-tauri/src/acp/connection.rs`: skips Codeg MCP/background watchers for internal title launches and declines interactive permission requests.
- `src-tauri/src/commands/acp.rs`: Tauri prompt wire fields plus root launch locale/context.
- `src-tauri/src/web/handlers/acp.rs`: Axum prompt wire fields plus root launch locale/context.
- `src-tauri/src/acp/delegation/broker.rs`: delegated task capture and inherited parent locale.
- `src-tauri/src/automation/engine.rs`: automation-root enrollment coverage and visible prompt capture.
- `src-tauri/src/chat_channel/session_commands.rs`: database-aware sends and channel locale capture.
- `src-tauri/src/chat_channel/session_event_subscriber.rs`: database-aware follow-up sends and channel locale capture.
- `src-tauri/src/db/service/conversation_service.rs`: transactional enrollment, manual/delete cancellation, native-title guard, and generated-title projection.
- `src-tauri/src/db/service/import_service.rs`: exclusion filter and explicit non-enrollment for raw imports.
- `src-tauri/src/commands/conversations.rs`: parser exclusion, rename/delete coordinator cancellation, and title upsert orchestration.
- `src-tauri/src/web/handlers/conversations.rs`: passes the internal-session registry and coordinator to shared cores.
- `src-tauri/src/web/handlers/conversation_experience.rs`: HTTP settings mirror.
- `src-tauri/src/web/handlers/mod.rs`: exports the settings handler.
- `src-tauri/src/web/router.rs`: settings getter/setter routes.
- `src/lib/types.ts`: settings/event mirrors and prompt options.
- `src/lib/api.ts`: transport-neutral settings calls and `visibleText`/`locale` ACP prompt payload.
- `src/lib/api.test.ts`: exact ACP prompt payload projection into the transport call.
- `src/lib/tauri.ts`: direct Tauri parity wrappers.
- `src/contexts/acp-connections-context.tsx`: forwards visible text and effective locale with every UI prompt.
- `src/contexts/acp-connections-context.test.tsx`: action-to-API prompt-context forwarding coverage.
- `src/contexts/app-workspace-context.tsx`: bootstraps the shared conversation-experience store in normal workspace windows.
- `src/contexts/app-workspace-context.test.tsx`: keeps provider event tests isolated while asserting the new bootstrap hook is mounted.
- `src/hooks/use-connection-lifecycle.ts`: passes the complete `PromptDraft` display projection to the connection action.
- `src/hooks/use-connection-lifecycle.test.ts`: verifies display text and effective locale survive the lifecycle action boundary.
- `src/stores/conversation-experience-store.ts`: revision-gated shared snapshot, event subscription, and title setter.
- `src/stores/conversation-experience-store.test.ts`: response/event ordering and backend-reset tests.
- `src/components/settings/conversation-experience-settings.tsx`: Off/base-agent picker and unavailable saved-value state.
- `src/components/settings/conversation-experience-settings.test.tsx`: availability filtering, save, and stale-event behavior.
- `src/components/settings/general-settings.tsx`: mounts the compact conversation-experience group.
- `src/i18n/messages/ar.json`: Arabic automatic-title setting copy.
- `src/i18n/messages/de.json`: German automatic-title setting copy.
- `src/i18n/messages/en.json`: English automatic-title setting copy.
- `src/i18n/messages/es.json`: Spanish automatic-title setting copy.
- `src/i18n/messages/fr.json`: French automatic-title setting copy.
- `src/i18n/messages/ja.json`: Japanese automatic-title setting copy.
- `src/i18n/messages/ko.json`: Korean automatic-title setting copy.
- `src/i18n/messages/pt.json`: Portuguese automatic-title setting copy.
- `src/i18n/messages/zh-CN.json`: Simplified Chinese automatic-title setting copy.
- `src/i18n/messages/zh-TW.json`: Traditional Chinese automatic-title setting copy.
- `src-tauri/tests/api_integration.rs`: HTTP/Tauri-core parity and background title completion integration coverage.
- `src-tauri/tests/delegation_columns.rs`: updates its exhaustive conversation entity initializer for the generated-title guard.

---

### Task 1: Add the Durable Automatic-Title Schema

**Files:**
- Create: `src-tauri/src/db/migration/m20260716_000001_auto_title.rs`
- Create: `src-tauri/src/db/entities/auto_title_job.rs`
- Create: `src-tauri/src/db/entities/internal_agent_session.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/mod.rs`
- Modify: `src-tauri/src/db/entities/conversation.rs`
- Modify: `src-tauri/src/models/conversation.rs`
- Modify: `src-tauri/src/db/service/conversation_service.rs`
- Modify: `src-tauri/src/db/service/import_service.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/tests/delegation_columns.rs`

**Interfaces:**
- Produces: `conversation::Model.auto_title_finalized: bool`
- Produces: `auto_title_job::Model` keyed by `conversation_id: i32`
- Produces: `internal_agent_session::Model` keyed by `(agent_type: String, external_id: String)`
- Preserves: all pre-existing conversations have no job row and are therefore ineligible even though the new boolean defaults to `false`

- [ ] **Step 1: Write the failing migration test and register the migration**

Add the module after the current final migration and drive `up` against a minimal conversation table:

```rust
#[tokio::test]
async fn up_adds_guard_jobs_and_internal_session_registry() {
    let conn = Database::connect("sqlite::memory:").await.expect("database");
    conn.execute_unprepared("PRAGMA foreign_keys=ON")
        .await
        .expect("foreign keys");
    conn.execute_unprepared(
        "CREATE TABLE conversation (id INTEGER PRIMARY KEY, title_locked BOOLEAN NOT NULL DEFAULT 0)",
    )
    .await
    .expect("conversation table");
    conn.execute_unprepared("INSERT INTO conversation (id) VALUES (7)")
        .await
        .expect("legacy row");

    Migration.up(&SchemaManager::new(&conn)).await.expect("migration");

    let columns = conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA table_info(conversation)".to_owned(),
        ))
        .await
        .expect("columns");
    assert!(columns.iter().any(|row| {
        row.try_get::<String>("", "name").ok().as_deref()
            == Some("auto_title_finalized")
    }));
    let finalized: bool = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT auto_title_finalized FROM conversation WHERE id = 7".to_owned(),
        ))
        .await
        .expect("legacy guard query")
        .expect("legacy row")
        .try_get("", "auto_title_finalized")
        .expect("legacy guard");
    assert!(!finalized);
    let legacy_job_count: i64 = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM auto_title_jobs".to_owned(),
        ))
        .await
        .expect("job count query")
        .expect("job count row")
        .try_get("", "count")
        .expect("count");
    assert_eq!(legacy_job_count, 0);
}
```

- [ ] **Step 2: Run the focused migration test and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils up_adds_guard_jobs_and_internal_session_registry
```

Expected: FAIL because `auto_title_finalized`, `auto_title_jobs`, and `internal_agent_sessions` do not exist.

- [ ] **Step 3: Implement the migration and exact SeaORM mirrors**

Create the schema with the four durable states and database bounds:

```rust
manager
    .alter_table(
        Table::alter()
            .table(Conversation::Table)
            .add_column(
                ColumnDef::new(Conversation::AutoTitleFinalized)
                    .boolean()
                    .not_null()
                    .default(false),
            )
            .to_owned(),
    )
    .await?;

manager
    .create_table(
        Table::create()
            .table(AutoTitleJob::Table)
            .col(
                ColumnDef::new(AutoTitleJob::ConversationId)
                    .integer()
                    .not_null()
                    .primary_key(),
            )
            .col(ColumnDef::new(AutoTitleJob::State).string().not_null())
            .col(ColumnDef::new(AutoTitleJob::Attempts).integer().not_null().default(0))
            .col(ColumnDef::new(AutoTitleJob::FirstUserText).text().null())
            .col(ColumnDef::new(AutoTitleJob::FirstAssistantText).text().null())
            .col(ColumnDef::new(AutoTitleJob::Locale).string().null())
            .col(ColumnDef::new(AutoTitleJob::UsableTurnSeq).integer().not_null().default(0))
            .col(ColumnDef::new(AutoTitleJob::AttemptTurnSeq).integer().not_null().default(0))
            .col(ColumnDef::new(AutoTitleJob::LastUsableTurnToken).string().null())
            .col(ColumnDef::new(AutoTitleJob::UpdatedAt).timestamp_with_time_zone().not_null())
            .foreign_key(
                ForeignKey::create()
                    .from(AutoTitleJob::Table, AutoTitleJob::ConversationId)
                    .to(Conversation::Table, Conversation::Id)
                    .on_delete(ForeignKeyAction::Cascade),
            )
            .check(Expr::col(AutoTitleJob::State).is_in([
                "awaiting_turn",
                "ready",
                "running",
                "retry_wait",
            ]))
            .check(Expr::col(AutoTitleJob::Attempts).gte(0))
            .check(Expr::col(AutoTitleJob::Attempts).lte(2))
            .check(Expr::col(AutoTitleJob::UsableTurnSeq).gte(0))
            .check(Expr::col(AutoTitleJob::AttemptTurnSeq).gte(0))
            .to_owned(),
    )
    .await?;
```

Add the `(state, updated_at, conversation_id)` index. Create `internal_agent_sessions(agent_type TEXT NOT NULL, external_id TEXT NOT NULL, purpose TEXT NOT NULL CHECK (purpose IN ('title')), created_at TIMESTAMP NOT NULL, PRIMARY KEY (agent_type, external_id))`. Implement `down` in reverse dependency order: drop `internal_agent_sessions`, drop `auto_title_jobs`, then drop `conversation.auto_title_finalized`. Mirror job state with a `DeriveActiveEnum` whose values are exactly `awaiting_turn`, `ready`, `running`, and `retry_wait`.

Replace the current `create_inner` initializer with the complete field list below. Add `auto_title_finalized: Set(false)` to the production and test `conversation::ActiveModel` literals in `import_service.rs`, the fork-sibling literal in `acp/manager.rs`, and the exhaustive literal in `src-tauri/tests/delegation_columns.rs`. Add the new summary field to `commands/conversations.rs::summary_child`; model-to-active conversions receive the field automatically and need no manual initializer:

```rust
let model = conversation::ActiveModel {
    id: NotSet,
    folder_id: Set(folder_id),
    title: Set(title),
    title_locked: Set(false),
    auto_title_finalized: Set(false),
    agent_type: Set(at_str),
    status: Set(conversation::ConversationStatus::InProgress),
    kind: Set(kind),
    model: Set(None),
    git_branch: Set(git_branch),
    external_id: Set(None),
    parent_id: Set(parent_id),
    parent_tool_use_id: Set(parent_tool_use_id),
    delegation_call_id: Set(delegation_call_id),
    message_count: Set(0),
    created_at: Set(now),
    updated_at: Set(now),
    deleted_at: Set(None),
    pinned_at: Set(None),
};
```

Add `auto_title_finalized: bool` to `DbConversationSummary` and set it explicitly in `conv_to_summary`; fork-specific copying is completed in Task 3. Extend the migration test to read the legacy row's value as `false`, inspect both table definitions and the queue index, reject state `done`, reject attempts `3`/negative sequence values, prove the composite internal-session key rejects a duplicate, prove deleting conversation `7` cascades its job, and run `Migration.down` to verify all three schema additions are removed. Define these assertions in the migration file itself; no external fixture is required.

- [ ] **Step 4: Run migration/entity tests and both compile modes**

Run:

```powershell
cd src-tauri
cargo test --features test-utils up_adds_guard_jobs_and_internal_session_registry
cargo test --features test-utils db::migration
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; the legacy row has no job, invalid job attempts/states are rejected by SQLite, and both runtimes compile.

- [ ] **Step 5: Commit the schema**

```powershell
git add src-tauri/src/db/migration/m20260716_000001_auto_title.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/auto_title_job.rs src-tauri/src/db/entities/internal_agent_session.rs src-tauri/src/db/entities/conversation.rs src-tauri/src/db/entities/mod.rs src-tauri/src/models/conversation.rs src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/commands/conversations.rs src-tauri/tests/delegation_columns.rs
git commit -m "feat(titles): add durable automatic title schema"
```

---

### Task 2: Persist the Revisioned Conversation-Experience Settings

**Files:**
- Create: `src-tauri/src/commands/conversation_experience.rs`
- Modify: `src-tauri/src/commands/mod.rs`

**Interfaces:**
- Produces: `ConversationExperienceSettings { auto_title_agent: Option<AgentType>, reference_search_limit: u16, revision: u64 }`
- Produces: `async fn get_conversation_experience_settings_core(conn: &DatabaseConnection) -> Result<ConversationExperienceSettings, AppCommandError>`
- Produces: `async fn load_auto_title_agent_from<C: ConnectionTrait>(conn: &C) -> Result<Option<AgentType>, DbError>`, which maps a missing/empty/corrupt value to `None`; command boundaries convert only genuine database failures with `AppCommandError::from`
- Produces: `async fn set_auto_title_agent_persisted_core(db: &AppDatabase, agent: Option<AgentType>) -> Result<ConversationExperienceSettings, AppCommandError>`
- Produces: `async fn set_reference_search_limit_persisted_core(conn: &DatabaseConnection, limit: u16) -> Result<ConversationExperienceSettings, AppCommandError>`
- Produces: `ConversationExperienceMutationGate::lock(&self) -> tokio::sync::MutexGuard<'_, ()>`; command wrappers hold this process-local gate across persistence, runtime effects, and event emission, while the persisted cores remain independently callable for transactional service tests
- Produces: `CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT = "conversation-experience-settings://changed"`

- [ ] **Step 1: Write failing settings isolation and validation tests**

```rust
#[tokio::test]
async fn independent_setters_preserve_the_other_field_and_advance_revision() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;

    let first = set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
        .await
        .expect("title agent");
    let second = set_reference_search_limit_persisted_core(&db.conn, 73)
        .await
        .expect("search limit");

    assert_eq!(first.revision, 1);
    assert_eq!(second.revision, 2);
    assert_eq!(second.auto_title_agent, Some(AgentType::ClaudeCode));
    assert_eq!(second.reference_search_limit, 73);
}

#[tokio::test]
async fn title_agent_must_be_enabled_and_available() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    crate::commands::acp::acp_list_agents_core(&db)
        .await
        .expect("seed agent settings");
    crate::db::service::agent_setting_service::update(
        &db.conn,
        AgentType::ClaudeCode,
        crate::db::service::agent_setting_service::AgentSettingsUpdate {
            enabled: false,
            env_json: None,
            model_provider_id: None,
        },
    )
    .await
    .expect("disable agent");
    let error = set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
        .await
        .expect_err("disabled agent");
    assert!(matches!(error.code, AppErrorCode::ConfigurationInvalid));
}
```

In the same test module, add `concurrent_independent_setters_serialize_revision_without_losing_either_field`: create a `TempDir`, open the database through `crate::db::init_database(temp.path(), "settings-concurrency-test")` so the test uses the production five-connection WAL pool and busy timeout, start one enabled-agent write and one limit write with `tokio::join!`, require both to succeed, sort their returned revisions and assert `[1, 2]`, then load the document and assert both values plus revision `2`. Keep the `TempDir` alive through every assertion. This test is the guard for the write-first revision algorithm below; `fresh_in_memory_db()` may serialize on one pooled connection and is not a valid substitute, and two sequential calls do not exercise the race.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils independent_setters_preserve_the_other_field_and_advance_revision
cargo test --features test-utils title_agent_must_be_enabled_and_available
```

Expected: FAIL because the settings type and cores are undefined.

- [ ] **Step 3: Implement separate-key transactional setters**

Use these exact keys and defaults:

```rust
pub const KEY_AUTO_TITLE_AGENT: &str = "conversation_experience.auto_title_agent";
pub const KEY_REFERENCE_SEARCH_LIMIT: &str = "conversation_experience.reference_search_limit";
pub const KEY_SETTINGS_REVISION: &str = "conversation_experience.revision";
pub const DEFAULT_REFERENCE_SEARCH_LIMIT: u16 = 50;
pub const MIN_REFERENCE_SEARCH_LIMIT: u16 = 10;
pub const MAX_REFERENCE_SEARCH_LIMIT: u16 = 500;
pub const CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT: &str =
    "conversation-experience-settings://changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationExperienceSettings {
    pub auto_title_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}

#[derive(Default)]
pub struct ConversationExperienceMutationGate {
    inner: tokio::sync::Mutex<()>,
}

impl ConversationExperienceMutationGate {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.lock().await
    }
}
```

Validate `Some(agent)` with `acp_get_agent_status_core(agent, db)` and require both `enabled` and `available` before opening the write transaction. Map its `AcpError` explicitly to `AppCommandError::new(AppErrorCode::ConfigurationInvalid, "Automatic title agent is unavailable").with_detail(error.to_string())`; there is no `From<AcpError>` conversion to use with `?`. Both setters use one shared `write_settings_field` helper. Its first statement inserts the revision row at `0` with all required `app_metadata` timestamps and `ON CONFLICT(key) DO NOTHING`; its second statement is the first unconditional write against an existing row and atomically advances the revision. Use a `CASE` expression that accepts only a non-empty all-decimal value in `0..=9223372036854775806`, increments that value, and writes `1` for every other corrupt value. Add `WHERE key = ? AND value <> '9223372036854775807'`, clear `deleted_at`, and require exactly one affected row; zero rows means signed-64-bit revision exhaustion and returns `DatabaseError`. This write-first sequence acquires SQLite's writer lock before any settings read, so simultaneous setters cannot both derive the same revision or hit `SQLITE_BUSY_SNAPSHOT`.

Use this exact update expression after the insert (bind RFC3339 `updated_at`, then `KEY_SETTINGS_REVISION`):

```sql
UPDATE app_metadata
SET value = CASE
        WHEN value <> ''
          AND value NOT GLOB '*[^0-9]*'
          AND length(value) <= 19
          AND CAST(value AS INTEGER) BETWEEN 0 AND 9223372036854775806
        THEN CAST(CAST(value AS INTEGER) + 1 AS TEXT)
        ELSE '1'
    END,
    updated_at = ?,
    deleted_at = NULL
WHERE key = ?
  AND value <> '9223372036854775807'
```

After the revision write, write only the target field, read the complete document through `async fn load_settings_from<C: ConnectionTrait>(conn: &C) -> Result<ConversationExperienceSettings, DbError>` using the same transaction, and commit. `load_settings_from` delegates title parsing to `load_auto_title_agent_from`; an absent key, the Off sentinel `""`, invalid JSON, or an unknown enum value all resolve to `None` with a warning for corrupt non-empty data. Keep both generic readers at the database layer so enrollment, claims, and transactions can propagate `DbError` directly; `get_conversation_experience_settings_core` and the two persisted setters map that error only at their public `AppCommandError` boundary. The production connection's busy timeout serializes a concurrent writer at the first write; do not perform a read before that write. For Off, delete every pending job in that transaction:

```rust
if agent.is_none() {
    auto_title_job::Entity::delete_many().exec(&txn).await?;
}
let stored_agent = agent
    .map(|value| serde_json::to_string(&value))
    .transpose()
    .map_err(|error| {
        AppCommandError::new(
            AppErrorCode::DatabaseError,
            "Failed to serialize automatic title agent",
        )
        .with_detail(error.to_string())
    })?
    .unwrap_or_default();
app_metadata_service::upsert_value(
    &txn,
    KEY_AUTO_TITLE_AGENT,
    &stored_agent,
)
.await?;
let saved = load_settings_from(&txn).await?;
txn.commit().await?;
```

Represent Off as an empty value, parse unknown/corrupt agent values as `None` with a warning, clamp the reference limit on both read and write, and leave gate acquisition, event emission, and runtime cancellation to the command wrapper added in Task 9. The persisted cores must not acquire the gate themselves because enrollment and focused transaction tests call the database-layer readers/setters directly. Add unit tests for defaults `(None, 50, 0)`, corrupt persisted values, lower/upper limit clamps, revision overflow, and Off deleting both `awaiting_turn` and `running` rows atomically.

- [ ] **Step 4: Verify settings behavior**

Run:

```powershell
cd src-tauri
cargo test --features test-utils commands::conversation_experience::tests
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; defaults are `(None, 50, 0)`, writes advance revision once, and stale independent controls cannot overwrite one another.

- [ ] **Step 5: Commit the settings persistence**

```powershell
git add src-tauri/src/commands/conversation_experience.rs src-tauri/src/commands/mod.rs
git commit -m "feat(settings): persist conversation experience settings"
```

---

### Task 3: Enroll New Conversations and Enforce Title Precedence Atomically

**Files:**
- Create: `src-tauri/src/auto_title/mod.rs`
- Create: `src-tauri/src/auto_title/types.rs`
- Create: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/db/service/conversation_service.rs`
- Modify: `src-tauri/src/db/service/import_service.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/src/automation/engine.rs`

**Interfaces:**
- Produces: `async fn enroll_new_conversation<C: ConnectionTrait>(conn: &C, conversation_id: i32, now: DateTime<Utc>) -> Result<bool, DbError>`
- Produces: `async fn cancel_job<C: ConnectionTrait>(conn: &C, conversation_id: i32) -> Result<bool, DbError>`
- Produces: `async fn finalize_generated_title(conn: &DatabaseConnection, claim: &AutoTitleClaim, title: &str) -> Result<FinalizeTitleOutcome, DbError>`
- Produces: `AutoTitleClaim { conversation_id: i32, attempt: i32, agent: AgentType, first_user_text: String, first_assistant_text: String, locale: AppLocale, attempt_turn_seq: i32 }`
- Produces: `FinalizeTitleOutcome::{Committed, Cancelled}`; only `Committed` may trigger the post-commit conversation upsert in Task 8
- Changes: `async fn conversation_service::update_title(conn: &DatabaseConnection, conversation_id: i32, title: String) -> Result<bool, DbError>` and `async fn soft_delete(conn: &DatabaseConnection, conversation_id: i32) -> Result<bool, DbError>`, where `true` means a pending job was removed and active work must be cancelled after commit
- Preserves: `refresh_auto_title` changes neither `updated_at` nor a finalized generated title

- [ ] **Step 1: Write failing enrollment, import, race, and fork tests**

```rust
#[tokio::test]
async fn enabled_creation_enrolls_root_and_delegate() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    crate::db::service::app_metadata_service::upsert_value(
        &db.conn,
        KEY_AUTO_TITLE_AGENT,
        &serde_json::to_string(&AgentType::Codex).unwrap(),
    )
    .await
    .unwrap();
    let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-enrollment").await;
    let root = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
        .await
        .expect("root");
    let child = create_with_delegation(
        &db.conn,
        folder,
        AgentType::Gemini,
        Some("child".into()),
        None,
        Some(crate::acp::delegation::spawner::DelegationLink {
            parent_conversation_id: root.id,
            parent_tool_use_id: "tool-1".into(),
            delegation_call_id: "call-1".into(),
        }),
    )
    .await
    .expect("child");

    assert!(auto_title_job::Entity::find_by_id(root.id).one(&db.conn).await.unwrap().is_some());
    assert!(auto_title_job::Entity::find_by_id(child.id).one(&db.conn).await.unwrap().is_some());
}

#[tokio::test]
async fn manual_rename_and_generated_commit_have_atomic_precedence() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-precedence").await;
    let conversation = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        None,
        None,
    )
    .await
    .unwrap();
    seed_running_job(&db.conn, conversation.id, 1).await;
    assert!(conversation_service::update_title(&db.conn, conversation.id, "Manual".into())
        .await
        .expect("rename"));
    let claim = AutoTitleClaim {
        conversation_id: conversation.id,
        attempt: 1,
        agent: AgentType::Codex,
        first_user_text: "task".into(),
        first_assistant_text: "answer".into(),
        locale: AppLocale::En,
        attempt_turn_seq: 1,
    };
    let outcome = finalize_generated_title(&db.conn, &claim, "Generated")
        .await
        .expect("late result");
    assert_eq!(outcome, FinalizeTitleOutcome::Cancelled);
    let saved = conversation::Entity::find_by_id(conversation.id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.title.as_deref(), Some("Manual"));
}
```

Define `seed_running_job` in `auto_title::service::tests`; it inserts one `auto_title_job::ActiveModel` with `state = Running`, the supplied attempt count, `first_user_text = "task"`, `first_assistant_text = "answer"`, `locale = "en"`, `usable_turn_seq = attempt_turn_seq = 1`, token `turn-1`, and `updated_at = Utc::now()`. Add an import-service-local test beside the existing private `import_one` tests: enable the metadata key, import one synthetic raw summary through `import_one`, resolve its row by external ID, and assert no job exists. Do not make `import_one` public solely for this test.

Add a fork assertion that both rows copy `auto_title_finalized = true`, the existing job stays on the live row, and the sibling gets no job. Add explicit creation-path cases for `create`, `create_chat`, and `create_with_delegation` in their owning service/command tests; each must produce exactly one job while enabled. In `automation::engine::tests`, add `automation_root_creation_enrolls_auto_title`: drive the private root-creation branch through its existing engine fixture and assert its `create_conversation_core` result also has exactly one job, without adding a second enrollment call in automation code. Add `creation_racing_disable_leaves_no_job_when_off`: use `init_database` with a live `TempDir` and the production pooled WAL configuration, race a live create against `set_auto_title_agent_persisted_core(None)`, and assert the final Off document and zero job rows regardless of transaction order.

- [ ] **Step 2: Run the focused service tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils enabled_creation_enrolls_root_and_delegate
cargo test --features test-utils imported_raw_session_never_enrolls_auto_title
cargo test --features test-utils manual_rename_and_generated_commit_have_atomic_precedence
cargo test --features test-utils fork_preserves_generated_title_guard_without_enrolling_sibling
cargo test --features test-utils automation_root_creation_enrolls_auto_title
```

Expected: FAIL because creation is not transactional with settings/job enrollment and generated precedence is absent.

- [ ] **Step 3: Implement enrollment and terminal title transactions**

Change `create_inner` to own one transaction and call enrollment before commit. Use the complete initializer from Task 1; the only new transaction lines are:

```rust
let txn = conn.begin().await?;
let model = conversation::ActiveModel {
    id: NotSet,
    folder_id: Set(folder_id),
    title: Set(title),
    title_locked: Set(false),
    auto_title_finalized: Set(false),
    agent_type: Set(at_str),
    status: Set(conversation::ConversationStatus::InProgress),
    kind: Set(kind),
    model: Set(None),
    git_branch: Set(git_branch),
    external_id: Set(None),
    parent_id: Set(parent_id),
    parent_tool_use_id: Set(parent_tool_use_id),
    delegation_call_id: Set(delegation_call_id),
    message_count: Set(0),
    created_at: Set(now),
    updated_at: Set(now),
    deleted_at: Set(None),
    pinned_at: Set(None),
}
    .insert(&txn)
    .await?;
enroll_new_conversation(&txn, model.id, now).await?;
txn.commit().await?;
Ok(model)
```

`enroll_new_conversation` calls Task 2's `load_auto_title_agent_from` inside that transaction and inserts exactly one `AwaitingTurn` row only when the parsed value is `Some`. Do not test only for metadata-row presence: Off is persisted as an existing empty row, so presence-based enrollment would incorrectly enroll every Off conversation. Keep raw import on its direct `ActiveModel` path so it never calls enrollment.

Manual rename and soft delete must update/delete in one transaction:

```rust
let txn = conn.begin().await?;
let changed = conversation::Entity::update_many()
    .col_expr(conversation::Column::Title, Expr::value(title))
    .col_expr(conversation::Column::TitleLocked, Expr::value(true))
    .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
    .filter(conversation::Column::Id.eq(conversation_id))
    .filter(conversation::Column::DeletedAt.is_null())
    .exec(&txn)
    .await?;
if changed.rows_affected == 0 {
    return Err(DbError::Migration(format!("Conversation not found: {conversation_id}")));
}
let removed = cancel_job(&txn, conversation_id).await?;
txn.commit().await?;
Ok(removed)
```

`finalize_generated_title` conditionally matches the exact running attempt plus `deleted_at IS NULL`, `title_locked = false`, and `auto_title_finalized = false`; it writes title/finalized without touching `updated_at` and deletes the job in the same transaction. Add `AutoTitleFinalized.eq(false)` to `refresh_auto_title`.

The service return-type change must compile before the coordinator exists. In this task, keep the public command-core contracts as `Result<(), AppCommandError>` by mapping the committed service result with `.map(|_| ())` in both `update_conversation_title_core` and `delete_conversation_core`; do not accidentally return `Result<bool, _>` from either function. Task 8 deliberately replaces that temporary discard with coordinator-aware orchestration after `AutoTitleCoordinator` exists. Test-only direct callers in `import_service.rs`, `acp/manager.rs`, `acp/lifecycle.rs`, and `conversation_service.rs` may continue ignoring the successful `bool` value.

In `persist_fork_outcome`, capture `current.auto_title_finalized` before conversion, retain it on the live row, copy it to the sibling, and do not insert a sibling job.

- [ ] **Step 4: Verify creation and precedence paths**

Run:

```powershell
cd src-tauri
cargo test --features test-utils auto_title::service::tests
cargo test --features test-utils db::service::conversation_service::tests
cargo test --features test-utils db::service::import_service::tests
cargo test --features test-utils automation::engine::tests
cargo test --features test-utils fork_
```

Expected: all commands exit 0; every live creation path enrolls only while enabled, imports remain historical, and manual/native/fork behavior matches the predicates.

- [ ] **Step 5: Commit enrollment and precedence**

```powershell
git add src-tauri/src/auto_title/mod.rs src-tauri/src/auto_title/types.rs src-tauri/src/auto_title/service.rs src-tauri/src/lib.rs src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/commands/conversations.rs src-tauri/src/automation/engine.rs
git commit -m "feat(titles): enroll conversations and enforce precedence"
```

---

### Task 4: Capture Bounded Visible Prompt Context at Admission

**Files:**
- Create: `src-tauri/src/auto_title/context.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Modify: `src-tauri/src/auto_title/types.rs`
- Modify: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src-tauri/src/chat_channel/session_commands.rs`
- Modify: `src-tauri/src/chat_channel/session_event_subscriber.rs`

**Interfaces:**
- Produces: `ConnectionPurpose::{User, Delegation, InternalProbe, InternalTitle}`
- Produces: `ConnectionLaunchContext { purpose: ConnectionPurpose, inherited_locale: Option<AppLocale> }`
- Produces: `PromptCaptureContext { visible_text: Option<String>, locale: Option<AppLocale> }`
- Produces: `CapturedPrompt { visible_text: String, locale: AppLocale }`
- Produces: `PromptCaptureContext::new(visible_text: Option<String>, locale: Option<AppLocale>) -> Self`
- Produces: `parse_supported_app_locale(value: Option<&str>) -> Option<AppLocale>` with exact matches for the ten snake-case wire identifiers
- Produces: `capture_prompt_context<C: ConnectionTrait>(conn: &C, conversation_id: i32, blocks: &[PromptInputBlock], capture: Option<&PromptCaptureContext>, fallback_locale: AppLocale) -> Result<CapturedPrompt, DbError>`
- Changes: `async fn ConnectionManager::send_prompt(&self, db: &AppDatabase, conn_id: &str, blocks: Vec<PromptInputBlock>, capture: Option<PromptCaptureContext>) -> Result<(), AcpError>`
- Changes: `async fn send_prompt_linked(&self, db: &AppDatabase, conn_id: &str, blocks: Vec<PromptInputBlock>, folder_id: Option<i32>, conversation_id: Option<i32>, delegation: Option<DelegationLink>, capture: Option<PromptCaptureContext>) -> Result<Option<i32>, AcpError>`
- Changes: `async fn send_prompt_linked_with_message_id(&self, db: &AppDatabase, conn_id: &str, blocks: Vec<PromptInputBlock>, folder_id: Option<i32>, conversation_id: Option<i32>, delegation: Option<DelegationLink>, client_message_id: Option<String>, capture: Option<PromptCaptureContext>) -> Result<Option<i32>, AcpError>`
- Produces: `pub(crate) async fn send_prompt_unlinked_internal(&self, conn_id: &str, blocks: Vec<PromptInputBlock>) -> Result<(), AcpError>` which verifies `purpose` is `InternalProbe` or `InternalTitle` and bypasses title capture; crate visibility is required because Task 7's runner lives outside `acp::manager`

- [ ] **Step 1: Write failing sanitizer and admission-order tests**

```rust
#[test]
fn fallback_projection_keeps_labels_and_drops_private_payloads() {
    let blocks = vec![
        PromptInputBlock::Text {
            text: "Codeg mandatory delegation route: profile_id=\"x\"\n".into(),
        },
        PromptInputBlock::ResourceLink {
            uri: "file:///repo/README.md".into(),
            name: "README.md".into(),
            mime_type: Some("text/markdown".into()),
            description: None,
        },
        PromptInputBlock::Resource {
            uri: "file:///repo/secret.txt".into(),
            mime_type: Some("text/plain".into()),
            text: Some("SECRET-BYTES".into()),
            blob: Some("BASE64".into()),
        },
        PromptInputBlock::Image {
            data: "IMAGE-BYTES".into(),
            mime_type: "image/png".into(),
            uri: Some("file:///repo/screen.png".into()),
        },
    ];
    let visible = project_visible_prompt(&blocks);
    assert_eq!(visible, "README.md\nsecret.txt");
    assert!(!visible.contains("SECRET-BYTES"));
    assert!(!visible.contains("IMAGE-BYTES"));
}

#[tokio::test]
async fn capture_failure_prevents_enqueue_and_fast_completion_cannot_win() {
    let fixture = prompt_admission_fixture().await;
    fixture.fail_next_capture_transaction().await;
    let result = fixture.manager.send_prompt(
        &fixture.db,
        &fixture.connection_id,
        one_text_block(),
        Some(PromptCaptureContext::new(Some("visible".into()), Some(AppLocale::ZhCn))),
    ).await;
    assert!(result.is_err());
    assert!(fixture.command_receiver.try_recv().is_err());
    assert!(fixture.state.read().await.active_turn.is_none());
}
```

Define `PromptAdmissionFixture` and `async fn prompt_admission_fixture() -> PromptAdmissionFixture` inside `acp::manager::tests` by reusing the existing `ConnectionManager::insert_test_connection_live` helper, which returns the live command receiver; obtain its state through the manager's existing `get_state` accessor. The fixture owns `AppDatabase`, manager, connection ID, state, and command receiver. Its async `fail_next_capture_transaction` executes `CREATE TRIGGER fail_title_capture BEFORE UPDATE ON auto_title_jobs BEGIN SELECT RAISE(ABORT, 'capture failure'); END`; the test performs only one send and drops the database afterward, so no production failure hook is introduced. Define `one_text_block()` locally as one `PromptInputBlock::Text`.

Add named cases `cancelled_while_reserving_stages_no_title_context`, `accepted_prompt_persists_capture_before_immediate_completion`, `linked_and_already_linked_sends_share_capture_once`, and `chat_first_send_adopts_precreated_conversation_before_capture`. The cancellation case fills the bounded command channel, starts a send, aborts it while `reserve()` is pending, then verifies both the job and `active_turn` are unchanged. The immediate-completion case receives the queued command and emits completion without any delay, then verifies the persisted task predates the usable completion. The chat case drives the existing `SessionStarted` kickoff path and proves its pre-created conversation ID is installed on `SessionState` before the shared hook runs. Also test the 4,000-scalar split, first-task write-once behavior, locale refresh across every surviving job state (`awaiting_turn`, `ready`, `running`, and `retry_wait`), reserve failure, and delegated locale inheritance.

- [ ] **Step 2: Run focused context/admission tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils fallback_projection_keeps_labels_and_drops_private_payloads
cargo test --features test-utils capture_failure_prevents_enqueue_and_fast_completion_cannot_win
cargo test --features test-utils bounded_context_keeps_2995_marker_and_1000_suffix
```

Expected: FAIL because prompt capture, launch purpose, and active-turn metadata are undefined.

- [ ] **Step 3: Implement normalization and the reserve-capture-send critical path**

Use scalar-safe bounding and the existing Rust reference-link fold:

```rust
pub fn bound_context(text: &str) -> String {
    let folded = crate::parsers::fold_reference_links(text);
    let chars: Vec<char> = folded.chars().collect();
    if chars.len() <= 4_000 {
        return folded;
    }
    let mut bounded = String::with_capacity(folded.len());
    bounded.extend(chars[..2_995].iter());
    bounded.push_str("\n...\n");
    bounded.extend(chars[chars.len() - 1_000..].iter());
    bounded
}
```

Drop a text block as internal only when every non-empty line begins with the structured mandatory-route prefix. Project `ResourceLink` to `name`, project embedded `Resource` to a URI-derived basename, and ignore image data plus embedded `text`/`blob`.

Store this per-turn state on `SessionState`:

```rust
#[derive(Debug, Clone)]
pub struct ActiveTurnContext {
    pub token: String,
    pub locale: AppLocale,
}

pub purpose: ConnectionPurpose,
pub effective_locale: AppLocale,
pub active_turn: Option<ActiveTurnContext>,
```

Resolve locale in this exact order: a valid explicit capture locale; inherited connection locale; persisted `SystemLanguageSettings.language`; `AppLocale::En`. Initialize root connections from the persisted language rather than blindly using `AppLocale::En`; channel callers still override it with their channel locale. The Tauri command and Axum request structs accept locale as `Option<String>`, then call `parse_supported_app_locale`; using `Option<AppLocale>` at the wire boundary would reject an unknown older-client value before the documented fallback can run. Match only `en`, `zh_cn`, `zh_tw`, `ja`, `ko`, `es`, `de`, `fr`, `pt`, and `ar`, and test unknown/mixed-case values returning `None`. `Some(visible_text)`, including an empty string, is authoritative and never falls back to wire blocks. Whenever a job row still exists, update its locale on every accepted prompt regardless of state; write `first_user_text` only when null. This ensures a ready job delayed by permit pressure and either retry state use the latest originating interface locale without replacing the first task.

Refactor the shared enqueue so the only blocking point before capture is command-channel reservation. Under the per-connection prompt lock, bypass the hook only for an unlinked connection or an internal purpose. Otherwise hold the state write guard across the short capture transaction, then perform no await before `permit.send`:

```rust
let permit = cmd_tx.reserve().await.map_err(|_| AcpError::ProcessExited)?;
let mut state = state_arc.write().await;
if state.turn_in_flight {
    return Err(AcpError::TurnInProgress);
}
let captured = capture_prompt_context(
    &db.conn,
    conversation_id,
    &blocks,
    capture.as_ref(),
    state.effective_locale,
)
.await
.map_err(|error| AcpError::protocol(error.to_string()))?;
let token = uuid::Uuid::new_v4().to_string();
state.effective_locale = captured.locale;
state.active_turn = Some(ActiveTurnContext { token, locale: captured.locale });
state.turn_in_flight = true;
permit.send(ConnectionCommand::Prompt { blocks, user_message });
```

Retain the existing mandatory-profile-route update in the same synchronous tail between `turn_in_flight = true` and `permit.send`; root prompts apply the precomputed IDs and delegated/internal prompts do not. There must still be no `.await` in that tail.

Do not use a context-free default for production roots: before spawn, UI and automation entry points load `SystemLanguageSettings.language` and pass `ConnectionLaunchContext { purpose: User, inherited_locale: Some(language) }`; chat-channel roots pass their configured message language after conversion to `AppLocale`, falling back to the same stored language. Pass `Delegation` plus the parent's latest locale from `ConnectionManagerSpawner`, `InternalProbe` from `probe_agent_options`, and reserve `InternalTitle` for Task 7. A test-only `Default` may remain English. Update every `send_prompt` caller to pass `&AppDatabase`; delegated sends pass the broker task as explicit visible text.

The chat channel currently creates its conversation row before spawn but calls unlinked `send_prompt`, so merely adding a database argument would skip capture forever. Change its initial `SessionStarted` kickoff, deferred kickoff, and follow-up paths to call `send_prompt_linked` with the existing conversation and folder IDs. The first call takes Branch A and emits `ConversationLinked`; later calls are idempotent already-linked sends. Build a short-lived `AppDatabase { conn: db.clone() }` where the subscriber currently owns only `DatabaseConnection`, resolve the conversation's authoritative folder ID rather than assuming the sender's current folder, and pass the exact channel text/locale in `PromptCaptureContext`. Update the Tauri command and Axum handler wire structs in this task to accept optional `visibleText`/`locale`; deserialize locale as `Option<String>` and apply the lossy parser so older clients compile and run before Task 9 starts sending the fields.

- [ ] **Step 4: Verify every producer and admission race**

Run:

```powershell
cd src-tauri
cargo test --features test-utils auto_title::context::tests
cargo test --features test-utils acp::manager::tests::send_prompt
cargo test --features test-utils delegated_child_inherits_parent_effective_locale
cargo check
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

Expected: all commands exit 0; rejected sends stage no context, accepted sends persist capture before enqueue, and every producer compiles with the new database-aware signature.

- [ ] **Step 5: Commit prompt capture**

```powershell
git add src-tauri/src/auto_title/context.rs src-tauri/src/auto_title/mod.rs src-tauri/src/auto_title/types.rs src-tauri/src/auto_title/service.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/connection.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/automation/engine.rs src-tauri/src/chat_channel/session_commands.rs src-tauri/src/chat_channel/session_event_subscriber.rs
git commit -m "feat(titles): capture visible prompt context at admission"
```

---

### Task 5: Deliver Immutable Turn Completion Sidecars to Lifecycle

**Files:**
- Modify: `src-tauri/src/auto_title/types.rs`
- Modify: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/event_stream.rs`
- Modify: `src-tauri/src/acp/internal_bus.rs`
- Modify: `src-tauri/src/web/event_bridge.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/db/service/conversation_service.rs`

**Interfaces:**
- Produces: `TurnCompletionSnapshot { conversation_id: i32, turn_token: String, locale: AppLocale, final_text: Arc<str> }`
- Produces: `InternalEventEnvelope { event: Arc<EventEnvelope>, completion: Option<Arc<TurnCompletionSnapshot>> }`; `EventEnvelope` itself is unchanged, and `InternalEventEnvelope` implements `Deref<Target = EventEnvelope>`
- Changes: `InternalEventBus` stores `broadcast::Sender<Arc<InternalEventEnvelope>>` and `subscribe()` returns `broadcast::Receiver<Arc<InternalEventEnvelope>>`; the outer `Arc` preserves every existing consumer's `envelope_arc.as_ref()` call while the inner event `Arc` remains the one public envelope shared with replay/transport
- Preserves: `InternalEventBus::send(&self, event: Arc<EventEnvelope>)` wraps a sidecar-free internal event for existing direct producers/tests; `InternalEventBus::send_with_completion(&self, event: Arc<EventEnvelope>, completion: Option<Arc<TurnCompletionSnapshot>>)` is used only by the shared event-bridge emit core, and both methods return `()`
- Produces: `async fn apply_usable_completion(txn: &DatabaseTransaction, snapshot: &TurnCompletionSnapshot, stop_reason: &str) -> Result<CompletionTransition, DbError>`
- Produces: `CompletionTransition { usable_turn_seq: i32, became_ready: bool }`
- Changes: the lifecycle bus worker consumes `internal.completion` and never re-reads `SessionState.last_assistant_text`, locale, token, or conversation ID for a completed turn

- [ ] **Step 1: Write failing sidecar isolation and idempotency tests**

```rust
#[tokio::test]
async fn completion_sidecar_is_internal_only_and_event_owned() {
    let fixture = completion_fixture().await;
    let mut stream = fixture.private_stream();
    let mut bus = fixture.internal_bus.subscribe();
    fixture.emit_end_turn("first answer", "token-1", AppLocale::Fr).await;
    fixture.overwrite_live_state("second answer", "token-2", AppLocale::Ja).await;

    let public = stream.recv().await.expect("public event");
    let internal = bus.recv().await.expect("internal event");
    assert_eq!(internal.completion.as_ref().expect("sidecar").turn_token, "token-1");
    assert_eq!(internal.completion.as_ref().expect("sidecar").final_text.as_ref(), "first answer");
    assert_eq!(internal.event.seq, public.seq);
    assert!(!serde_json::to_string(&public).expect("json").contains("token-1"));
    assert!(!serde_json::to_string(&internal.event).expect("json").contains("token-1"));
}

#[tokio::test]
async fn duplicate_turn_token_changes_the_job_once() {
    let fixture = awaiting_job_fixture().await;
    let snapshot = fixture.snapshot("same-token", "answer");
    let first = fixture.apply_completion(&snapshot).await;
    let second = fixture.apply_completion(&snapshot).await;
    assert_eq!(first.usable_turn_seq, 1);
    assert_eq!(second.usable_turn_seq, 1);
    assert!(!second.became_ready);
}
```

Define `CompletionFixture` plus `async fn completion_fixture() -> CompletionFixture` in `web::event_bridge::tests` from a real `SessionState`, its private `ConnectionEventStream`, and a test `InternalEventBus`; `emit_end_turn` first installs `ActiveTurnContext` plus `last_assistant_text` through ordinary content events, then emits `TurnComplete`. Define `AwaitingJobFixture` plus `async fn awaiting_job_fixture() -> AwaitingJobFixture` in `auto_title::service::tests` with one migrated in-memory DB, one enrolled conversation/job, `snapshot(token, answer)`, and `apply_completion`; every helper is local to the module named here.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils completion_sidecar_is_internal_only_and_event_owned
cargo test --features test-utils duplicate_turn_token_changes_the_job_once
```

Expected: FAIL because envelopes have no completion sidecar and lifecycle re-reads mutable state.

- [ ] **Step 3: Split public and internal envelopes under the state lock**

Make `SessionState::apply_event` return a snapshot only for `TurnComplete`, after assembling `last_assistant_text` and before clearing `active_turn`. Existing callers may ignore the return value. Leave `EventEnvelope` unchanged and build exactly one public envelope:

```rust
let completion = match (&payload, s.apply_event(&payload)) {
    (AcpEvent::TurnComplete { .. }, Some(snapshot)) => Some(Arc::new(snapshot)),
    _ => None,
};
s.event_seq += 1;
let public = Arc::new(EventEnvelope {
    seq: s.event_seq,
    connection_id: s.connection_id.clone(),
    payload,
});
```

Implement snapshot creation in `emit_with_state_gated`, the actual shared core called by both gated and ordinary emits; `emit_with_state` remains its thin wrapper. Push/send only `public` to the replay ring, private stream, desktop delivery, and WebSocket-facing paths. For `EventEmitter::Tauri` and `WebOnly`, call `InternalEventBus::send_with_completion(Arc::clone(&public), completion)`; pet/chat/automation consumers keep their current `Arc`/`as_ref()` shapes and obtain read-only event fields through `Deref`, but the lifecycle mailbox retains the wrapper. `EventEmitter::Noop` still publishes the ordinary public envelope to its private connection stream, but sends neither internal-bus nor transport delivery.

Change every explicit lifecycle dispatcher/worker channel type together: `connection_worker_loop` takes `mpsc::Receiver<Arc<InternalEventEnvelope>>`, the worker map stores `mpsc::Sender<Arc<InternalEventEnvelope>>`, and worker creation uses that same generic. Add `handle_internal_event`, which delegates non-completion behavior to the existing handler but passes the wrapper sidecar into the `TurnComplete` path. Keep the existing sidecar-free `handle_event(&EventEnvelope)` entry for its current direct unit tests. For a sidecar-bearing completion, start one transaction, apply the existing status transition and `apply_usable_completion`, and commit before status emission or broker completion. Make the status service primitive generic over `ConnectionTrait` so it accepts the same transaction. `apply_usable_completion` accepts only `end_turn` plus `!snapshot.final_text.trim().is_empty()`, passes the trimmed text through Task 4's `bound_context`, compares `last_usable_turn_token`, increments `usable_turn_seq` once, and writes `first_assistant_text` only when it is still absent. It moves `awaiting_turn`/eligible `retry_wait` to `ready` and updates the job locale from the event-owned snapshot. Thus both stored context fields use the same reference-link folding and exact 4,000-scalar cap. Return `became_ready`; Task 8 wires that result to the coordinator after the coordinator exists.

Forward the same `Arc<str>` sidecar text to the delegation broker after commit, and use `snapshot.conversation_id` even if the live connection has already disconnected. Production `TurnComplete` handling must not fall back to mutable session state when a sidecar is absent; sidecar-free direct tests get status behavior and an empty broker result only. Add named cases `two_queued_completions_keep_distinct_sidecars`, `abnormal_and_empty_completions_leave_job_awaiting`, and `completion_sidecar_never_enters_recent_event_replay` so the Step 4 claims are directly exercised rather than inferred from one overwrite test.

- [ ] **Step 4: Verify lifecycle isolation, rapid turns, and status behavior**

Run:

```powershell
cd src-tauri
cargo test --features test-utils completion_sidecar
cargo test --features test-utils duplicate_turn_token
cargo test --features test-utils lifecycle
cargo test --features test-utils event_stream
cargo test --features test-utils internal_bus
```

Expected: all commands exit 0; two queued rapid turns retain distinct snapshots, public JSON is unchanged, and abnormal/empty turns leave the job untouched.

- [ ] **Step 5: Commit immutable lifecycle delivery**

```powershell
git add src-tauri/src/auto_title/types.rs src-tauri/src/auto_title/service.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/event_stream.rs src-tauri/src/acp/internal_bus.rs src-tauri/src/web/event_bridge.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/db/service/conversation_service.rs
git commit -m "feat(titles): attach immutable completion lifecycle sidecars"
```

---

### Task 6: Hide Internal Agent Sessions from Every Codeg Discovery Path

**Files:**
- Create: `src-tauri/src/auto_title/internal_sessions.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/bin/codeg_server.rs`
- Modify: `src-tauri/src/web/mod.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/src/web/handlers/conversations.rs`
- Modify: `src-tauri/src/db/service/import_service.rs`

**Interfaces:**
- Produces: `async fn InternalAgentSessionRegistry::load(conn: DatabaseConnection, data_dir: &Path) -> Result<Arc<Self>, DbError>`
- Produces under `#[cfg(any(test, feature = "test-utils"))]`: synchronous `InternalAgentSessionRegistry::new_empty_for_test(conn: DatabaseConnection, data_dir: &Path) -> Result<Arc<Self>, DbError>` for a freshly migrated fixture with no pre-existing internal-session rows
- Produces: `async fn exclusive_discovery_lease(&self) -> OwnedRwLockWriteGuard<()>`
- Produces: `async fn shared_filter(&self) -> Result<(OwnedRwLockReadGuard<()>, InternalSessionFilter), DbError>`
- Produces: `async fn register_with_lease(&self, lease: &mut OwnedRwLockWriteGuard<()>, agent_type: AgentType, external_id: &str, purpose: InternalSessionPurpose) -> Result<(), DbError>` and `async fn register(&self, agent_type: AgentType, external_id: &str, purpose: InternalSessionPurpose) -> Result<(), DbError>`, where the latter briefly acquires its own exclusive lease after the runner's 15-second lease budget has expired
- Produces: `InternalSessionPurpose::Title`, serialized to the table value `"title"`
- Produces: `InternalSessionFilter::contains(agent_type: AgentType, external_id: Option<&str>, working_dir: Option<&str>) -> bool`
- Produces: parser-backed cores `list_conversations_core`, `get_conversation_core`, `list_folders_core`, `get_stats_core`, and `get_sidebar_data_core`, each taking `&InternalAgentSessionRegistry`; these are refactors of the currently state-less commands, not references to pre-existing symbols
- Changes: `get_folder_conversation_core(conn, registry, conversation_id)` and `get_folder_conversation_with_live_core(conn, manager, emitter, registry, conversation_id)` take the same registry and hold a shared discovery lease across direct parse plus Cline/Gemini list fallback
- Changes: `async fn import_local_conversations_core(conn: &DatabaseConnection, emitter: &EventEmitter, registry: &InternalAgentSessionRegistry, folder_id: i32) -> Result<ImportResult, AppCommandError>` and the underlying import service receive the same filter/lease boundary

- [ ] **Step 1: Write failing ID/path/lease coverage tests**

```rust
#[tokio::test]
async fn persisted_id_and_reserved_root_hide_sessions_after_restart() {
    let fixture = registry_fixture().await;
    fixture.registry.register(AgentType::Codex, "hidden-id", InternalSessionPurpose::Title)
        .await
        .expect("register");
    let restarted = InternalAgentSessionRegistry::load(
        fixture.db.conn.clone(),
        fixture.data_dir.path(),
    )
    .await
    .expect("reload");
    let (_, filter) = restarted.shared_filter().await.expect("filter");
    assert!(filter.contains(AgentType::Codex, Some("hidden-id"), None));
    assert!(filter.contains(
        AgentType::Gemini,
        None,
        Some(&fixture.reserved_root.join("orphan").to_string_lossy()),
    ));
}

#[tokio::test]
async fn parser_lists_details_stats_and_import_share_one_filter() {
    let fixture = parser_exclusion_fixture().await;
    let listed = fixture.list_raw().await;
    assert!(listed.iter().all(|row| row.id != "hidden-id"));
    assert!(fixture.get_raw("hidden-id").await.is_err());
    assert_eq!(fixture.stats().await.total_conversations, 1);
    assert_eq!(fixture.import().await.imported, 1);
}
```

Define `RegistryFixture` plus `async fn registry_fixture() -> RegistryFixture` in `auto_title::internal_sessions::tests` with `AppDatabase`, `TempDir`, reserved-root path, and loaded registry. There is no existing all-parser injection seam in `commands::conversations`, so add three small shared boundaries there: `filter_internal_summaries(rows: Vec<(AgentType, ConversationSummary)>, filter: &InternalSessionFilter) -> Vec<(AgentType, ConversationSummary)>`, `reject_internal_detail(agent_type, conversation_id, detail, filter) -> Result<ConversationDetail, AppCommandError>`, and `select_folder_time_fallback(rows, folder_path, started_at, filter) -> Option<ConversationSummary>`. Production parser cores, folder-detail direct/fallback parsing, and import scanning must call these exact helpers before search/aggregation/import. Define `ParserExclusionFixture` in `commands::conversations::tests` with two synthetic rows, one normal and one registered as internal; its list/folder/stats/sidebar methods run the same filter and production aggregation helpers, its detail method runs `reject_internal_detail`, its fallback method proves the closer internal row cannot beat the normal row after filtering, and the import-service test passes the filtered rows through the same private `import_one` loop. Expected import count `1` refers only to the normal summary. This is a data seam around the shared filtering boundary, not a fake claim that the hard-coded parser constructors are injectable.

- [ ] **Step 2: Run the focused exclusion tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils persisted_id_and_reserved_root_hide_sessions_after_restart
cargo test --features test-utils parser_lists_details_stats_and_import_share_one_filter
```

Expected: FAIL because discovery has no registry or shared lease.

- [ ] **Step 3: Implement one registry/filter boundary and route every raw scan through it**

Use an `Arc<tokio::sync::RwLock<()>>` for discovery ordering. Store the current IDs behind a separate lock in the registry; both registration methods insert into that set first and then persist idempotently under an exclusive discovery lease, while `shared_filter` takes the shared lease and clones an immutable `Arc<HashSet<_>>` snapshot. Leave the in-memory exclusion in place if persistence fails, return the error so the runner sends no prompt, and rely on the reserved-root fallback after restart. A duplicate `(agent_type, external_id)` with the same title purpose succeeds without creating another row. Persist `agent_type` by calling `serde_json::to_value(agent_type)`, matching `Value::String(value)`, and storing that inner unquoted string; the exact stored values are `"claude_code"`, `"codex"`, `"open_code"`, `"gemini"`, `"cline"`, `"hermes"`, `"code_buddy"`, `"kimi_code"`, `"pi"`, and `"grok"`. On load, wrap the database string as `Value::String` and deserialize `AgentType` through serde; never use `AgentType::to_string()`, whose human-facing labels differ. `register_with_lease` reuses the runner's existing guard; `register` acquires a short new guard only after the long handshake lease was released. Do not put a permanently immutable set directly on the registry, because post-start registrations must become visible. The reserved parent is `<data_dir>/internal/title-runs`; create and canonicalize that parent once, but compare lexically normalized absolute child records even after run directories are removed. Reuse the repository's separator/extended-prefix normalization and its Windows ASCII case-folding convention; reject `..` traversal before prefix comparison.

```rust
#[derive(Clone)]
pub struct InternalSessionFilter {
    ids: Arc<HashSet<(AgentType, String)>>,
    reserved_root: PathBuf,
}

impl InternalSessionFilter {
    pub fn contains(
        &self,
        agent_type: AgentType,
        external_id: Option<&str>,
        working_dir: Option<&str>,
    ) -> bool {
        external_id.is_some_and(|id| self.ids.contains(&(agent_type, id.to_owned())))
            || working_dir.is_some_and(|path| is_lexically_below(path, &self.reserved_root))
    }
}
```

Refactor the current parser-backed `list_conversations`, `get_conversation`, `list_folders`, `get_stats`, and `get_sidebar_data` commands into the exact cores named above. Acquire the shared lease before `spawn_blocking`, move both guard and filter into the closure, and run `filter_internal_summaries` or `reject_internal_detail` before search, folder, or statistic aggregation. Give both folder-conversation cores the registry parameter too: their direct `parser.get_conversation` result passes through `reject_internal_detail`, and the Cline/Gemini `parser.list_conversations` recovery passes through `select_folder_time_fallback` before it may choose the closest timestamp; the subsequent fallback `parser.get_conversation` detail is rejected through the same helper before use. Keep the shared guard alive through the entire blocking direct/fallback parse. The DB-backed `list_all_conversations_core` is not a raw parser scan and does not need the lease. Change `import_local_conversations_core` and `import_service::import_local_conversations` so the parser scan receives the same filter and holds the shared guard until scanning/filtering finishes; do not implement parser-specific exceptions. Update the existing `import_local_conversations_core_missing_folder_errors` test with a live `TempDir` and `InternalAgentSessionRegistry::new_empty_for_test(db.conn.clone(), data_dir.path())`, then pass `registry.as_ref()` to the new core signature; this pre-existing direct caller must compile in Task 6 even though its missing-folder branch never scans. Add one `inert_internal_session_registry(db, data_dir)` helper in `commands::conversations::tests` and pass it to every existing direct folder-conversation core call so round-trip/title/live-correlation tests retain their production signature.

For list/import rows, pass `summary.id` as the external ID and `summary.folder_path` as the recorded working directory. For direct raw detail, parse first under the shared lease, then reject with `AppErrorCode::NotFound` when either the requested `(agent_type, conversation_id)` or the returned detail's working directory is internal. Tauri commands receive the managed `Arc<InternalAgentSessionRegistry>` and Axum handlers read the same instance from `AppState`.

Construct one registry in desktop setup and server startup with async `load`; desktop setup may use its existing `tauri::async_runtime::block_on` initialization boundary. Keep the existing synchronous `AppState::new_for_test` signature and construct its empty registry with `new_empty_for_test`; changing that constructor to async would force unrelated integration and handler tests to change and is unnecessary because its fixture database is freshly migrated. Pass the registry through Tauri managed state and Axum `Extension<Arc<AppState>>`. Update the embedded-server `AppState` literal in `web/mod.rs` to clone the desktop-managed registry, so desktop HTTP handlers and direct Tauri commands cannot observe different exclusion sets. Tests that seed persisted internal-session rows before constructing a registry must call async `load` explicitly, as `persisted_id_and_reserved_root_hide_sessions_after_restart` already does.

- [ ] **Step 4: Verify all discovery surfaces and both runtimes**

Run:

```powershell
cd src-tauri
cargo test --features test-utils auto_title::internal_sessions::tests
cargo test --features test-utils commands::conversations
cargo test --features test-utils db::service::import_service
cargo test --features test-utils --test api_integration parser
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; excluded IDs and reserved-root sessions are absent from list/detail/folder/stat/sidebar/import results before and after registry reload.

- [ ] **Step 5: Commit internal-session exclusion**

```powershell
git add src-tauri/src/auto_title/internal_sessions.rs src-tauri/src/auto_title/mod.rs src-tauri/src/app_state.rs src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs src-tauri/src/web/mod.rs src-tauri/src/commands/conversations.rs src-tauri/src/web/handlers/conversations.rs src-tauri/src/db/service/import_service.rs
git commit -m "feat(titles): quarantine internal agent sessions"
```

---

### Task 7: Run the Selected Agent in an Isolated Hidden Connection

**Files:**
- Create: `src-tauri/src/auto_title/runner.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Modify: `src-tauri/src/auto_title/types.rs`
- Modify: `src-tauri/src/auto_title/context.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/session_state.rs`

**Interfaces:**
- Produces: `#[async_trait] trait TitleAgentRunner: Send + Sync { async fn run(&self, attempt: AutoTitleAttempt, cancellation: CancellationToken) -> Result<String, AutoTitleRunError>; }`
- Produces: `AutoTitleAttempt { conversation_id: i32, attempt: i32, agent: AgentType, locale: AppLocale, first_user_text: String, first_assistant_text: String }`
- Produces: `AutoTitleRunError::{Cancelled, Unavailable, Spawn, Identity, Registry, Interactive, Timeout, AbnormalStop, EmptyOutput}`
- Produces: crate-private `TitleConnectionDriver` with `spawn_internal_title(agent, working_dir, launch_inputs, locale)`, `identity_and_subscribe(conn_id)`, `send_internal(conn_id, blocks)`, and `disconnect(conn_id)` async methods
- Produces: `ManagerTitleConnectionDriver { manager: Arc<ConnectionManager> }` and `pub(crate) fn ManagerTitleConnectionDriver::new(manager: Arc<ConnectionManager>) -> Self`; the production driver calls the exact manager APIs and always spawns with `EventEmitter::Noop`, no preferred mode/config, and `ConnectionPurpose::InternalTitle`
- Produces: `HiddenAgentRunner { db: Arc<AppDatabase>, driver: Arc<dyn TitleConnectionDriver>, registry: Arc<InternalAgentSessionRegistry>, data_dir: PathBuf }` and `pub(crate) fn HiddenAgentRunner::new(db: Arc<AppDatabase>, driver: Arc<dyn TitleConnectionDriver>, registry: Arc<InternalAgentSessionRegistry>, data_dir: PathBuf) -> Self`
- Produces: `async fn ConnectionManager::identity_and_subscribe(&self, conn_id: &str) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError>` that snapshots both values under one state read lock
- Produces: `normalize_generated_title(raw: &str) -> Option<String>` capped at 80 Unicode scalars
- Guarantees: cancellation is observed during status/config loading, spawn, identity wait, prompt send, and completion wait; once a connection ID exists, every cancelled path disconnects it and removes only its per-run directory

The driver contract is exact and remains crate-private:

```rust
#[async_trait]
pub(crate) trait TitleConnectionDriver: Send + Sync {
    async fn spawn_internal_title(
        &self,
        agent: AgentType,
        working_dir: PathBuf,
        launch_inputs: AcpLaunchInputs,
        locale: AppLocale,
    ) -> Result<String, AcpError>;
    async fn identity_and_subscribe(
        &self,
        conn_id: &str,
    ) -> Result<(Option<String>, broadcast::Receiver<Arc<EventEnvelope>>), AcpError>;
    async fn send_internal(
        &self,
        conn_id: &str,
        blocks: Vec<PromptInputBlock>,
    ) -> Result<(), AcpError>;
    async fn disconnect(&self, conn_id: &str) -> Result<(), AcpError>;
}
```

- [ ] **Step 1: Write failing fake-connection runner tests**

```rust
#[tokio::test]
async fn runner_registers_identity_before_sending_and_returns_clean_title() {
    let fixture = hidden_runner_fixture().await;
    fixture.agent.emit_session_started_before_subscription("internal-1");
    fixture.agent.finish_with("## \"  修复 README   搜索  \"\nexplanation");

    let title = fixture.runner.run(fixture.attempt(AppLocale::ZhCn), CancellationToken::new())
        .await
        .expect("title");

    assert_eq!(title, "修复 README 搜索");
    let (_, filter) = fixture.registry.shared_filter().await.expect("filter");
    assert!(filter.contains(AgentType::Codex, Some("internal-1"), None));
    assert!(fixture.agent.prompt_was_sent_after_registration());
    assert_eq!(fixture.transport_event_count(), 0);
    assert_eq!(fixture.lifecycle_event_count(), 0);
}

#[tokio::test]
async fn permission_abnormal_stop_and_registry_failure_are_attempt_failures() {
    for scenario in [
        Scenario::Permission,
        Scenario::Refusal,
        Scenario::Disconnect,
        Scenario::MalformedOutput,
    ] {
        let fixture = hidden_runner_fixture_for(scenario).await;
        assert!(fixture.runner.run(fixture.attempt(AppLocale::En), CancellationToken::new()).await.is_err());
        assert!(fixture.was_disconnected());
    }
    let registry_failure = hidden_runner_fixture_for(Scenario::RegistryFailure).await;
    assert!(registry_failure
        .runner
        .run(registry_failure.attempt(AppLocale::En), CancellationToken::new())
        .await
        .is_err());
    assert_eq!(registry_failure.prompt_count(), 0);
    assert!(registry_failure.was_disconnected());
}
```

Define `HiddenRunnerFixture`, `Scenario`, `FakeTitleConnectionDriver`, `async fn hidden_runner_fixture() -> HiddenRunnerFixture`, and `async fn hidden_runner_fixture_for(Scenario) -> HiddenRunnerFixture` in `auto_title::runner::tests`. `FakeTitleConnectionDriver::spawn_internal_title` inserts a live test connection through the existing `ConnectionManager::insert_test_connection_live` helper and returns its deterministic ID instead of launching a process; its other trait methods delegate to the manager while recording spawn/register/prompt/disconnect order. The fixture emits `SessionStarted`, content, permission/question, and `TurnComplete` through `emit_with_state` and uses a real in-memory internal-session registry. `emit_session_started_before_subscription` emits immediately after fake spawn and before `identity_and_subscribe`; the assertion therefore exercises the state snapshot branch rather than a queued test shortcut. No nonexistent global process hook or real external CLI is used.

Add `overall_timeout_is_shared_across_spawn_handshake_and_completion` with paused Tokio time and explicit phase gates: advance 40 seconds before releasing spawn, another 40 before delivering `SessionStarted`, then leave completion blocked and advance 10 seconds. Require `AutoTitleRunError::Timeout` and a recorded disconnect at total second 90; a mistakenly renewed per-phase deadline would remain pending. Add `slow_handshake_releases_discovery_lease_at_15_seconds_but_sends_only_after_registration` with paused Tokio time: hold `SessionStarted` past 15 seconds, prove a shared discovery lease becomes acquirable and prompt count stays zero, emit the ID before 90 seconds, then prove durable registration precedes the one prompt. Add `spawn_and_registry_failures_leave_reserved_root_sessions_filtered` to exercise both fallback paths.

Add `cancellation_interrupts_each_runner_phase_and_disconnects_after_spawn` as a table-driven test over blocked status/config, spawn, identity, prompt-send, and completion gates. Cancel the supplied token at each gate. Before `spawn_internal_title` returns, require no registered connection and no prompt; after it returns, require exactly one disconnect, no later prompt/output acceptance, release of any held discovery lease, and best-effort removal of only the fixture's per-run directory. The fake spawn gate must model the production cancellation boundary: cancelling while the manager is still building an agent returns no connection ID and creates no manager entry, while cancellation after the driver returns is owned by the runner cleanup guard. Add `blocked_disconnect_cleanup_is_bounded_and_releases_the_attempt` with paused time: let execution reach completion/timeout, block the fake driver's disconnect forever, advance the five-second cleanup budget, and require the runner future to settle with its original outcome and its per-run directory cleanup attempted. A stuck disconnect must not retain the coordinator's attempt permit indefinitely.

- [ ] **Step 2: Run runner tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils runner_registers_identity_before_sending_and_returns_clean_title
cargo test --features test-utils permission_abnormal_stop_and_registry_failure_are_attempt_failures
cargo test --features test-utils overall_timeout_is_shared_across_spawn_handshake_and_completion
cargo test --features test-utils slow_handshake_releases_discovery_lease_at_15_seconds_but_sends_only_after_registration
cargo test --features test-utils cancellation_interrupts_each_runner_phase_and_disconnects_after_spawn
cargo test --features test-utils blocked_disconnect_cleanup_is_bounded_and_releases_the_attempt
```

Expected: FAIL because no hidden runner or launch-purpose isolation exists.

- [ ] **Step 3: Implement the isolated runner and output contract**

At `run` entry, compute one `Instant::now() + Duration::from_secs(90)` deadline and use that same deadline for status/config loading, spawn, identity registration, prompt, completion, disconnect-triggered failure, and output collection; no phase receives a fresh 90-second budget. Wrap every cancellable phase in one helper that selects between `cancellation.cancelled()`, `timeout_at(overall_deadline, phase)`, and the phase result. Cancellation maps only to `AutoTitleRunError::Cancelled`, timeout only to `Timeout`, and neither path starts a later phase. Keep the connection ID in an outer cleanup guard as soon as spawn returns, so cancellation/timeout during identity, registration, send, or completion always disconnects exactly once; before spawn returns there is no manager entry to disconnect because the production manager performs no await after inserting the connection and before returning its ID. Dropping the spawn future before that insertion is therefore the production cancellation boundary. The discovery lease is also guard-owned and is dropped on every early return. Cleanup runs outside the already-expired overall deadline: wrap `driver.disconnect(conn_id)` in a separate five-second `timeout`, log timeout/error without replacing the runner's original result, then best-effort remove only the per-run directory. The existing manager disconnect awaits a command-channel send and is not itself a time bound, so do not await it unboundedly after the 90-second execution deadline.

Call `acp_get_agent_status_core(attempt.agent, &db)` and fail when unavailable or disabled. Build launch inputs with `terminal_context::build_acp_launch_inputs(&db, attempt.agent, None, &data_dir)`, create `<reserved_root>/<uuid>`, and call `TitleConnectionDriver::spawn_internal_title`. Construct the production driver with `Arc::new(connection_manager.clone_ref())`, which shares the existing manager internals; do not change `AppState.connection_manager` to `Arc<ConnectionManager>` merely for this runner. `ManagerTitleConnectionDriver` uses the fixed non-window owner label `"internal:auto-title"`, supplies `preferred_mode_id = None` and an empty preferred-config map so the agent-advertised defaults win, and passes:

```rust
ConnectionLaunchContext {
    purpose: ConnectionPurpose::InternalTitle,
    inherited_locale: Some(attempt.locale),
}
```

For `InternalTitle`, pass no delegation snapshot/Codeg MCP injection, do not start background transcript watchers, decline every ACP permission or question request, use no conversation ID, and use `EventEmitter::Noop`. Also skip `TerminalPromptContext::append_once` for this purpose so the exact title prompt below is not silently prefixed with Codeg terminal instructions. The production driver's `send_internal` calls Task 4's crate-visible `send_prompt_unlinked_internal`; the ordinary database-aware send path must reject an `InternalTitle` purpose.

Take the exclusive discovery lease immediately before spawn. Under one `SessionState` read lock, snapshot `external_id` and subscribe to the private stream. If the ID is absent, await `SessionStarted`; before the 15-second deadline call `register_with_lease`, then drop the guard. Race the lease with that deadline: when it expires, drop only the long-held guard, continue the identity wait inside the overall 90-second `timeout_at`, and later call the short `register` method. Never send before durable registration.

Render this exact semantic prompt with bounded fields:

```text
Return only one concise conversation title in <locale>.
Do not use tools. Do not add Markdown, quotes, a prefix, or an explanation.

Task:
<first_user_text>

Final response:
<first_assistant_text>
```

Replace `<locale>` with these fixed language names, not the serde identifier: `en -> English`, `zh_cn -> Simplified Chinese`, `zh_tw -> Traditional Chinese`, `ja -> Japanese`, `ko -> Korean`, `es -> Spanish`, `de -> German`, `fr -> French`, `pt -> Portuguese`, and `ar -> Arabic`. Unit-test all ten mappings so a locale cannot silently fall back to English.

Accept only `end_turn`, collect only `ContentDelta` text from the private stream, and treat any permission/question event, protocol error, disconnect, or non-`end_turn` completion as failure. Then take the first non-empty line, remove one heading/list prefix and one paired outer quote/backtick/emphasis layer, remove non-whitespace control characters, collapse whitespace, and truncate to 80 scalars. Always disconnect and best-effort remove only the per-run working directory.

- [ ] **Step 4: Verify runner isolation and normalization**

Run:

```powershell
cd src-tauri
cargo test --features test-utils auto_title::runner::tests
cargo test --features test-utils internal_title
cargo test --features test-utils noop
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; session-start races are recovered, interactive/abnormal paths fail, no global event receives runner traffic, and cleanup never touches external CLI storage.

- [ ] **Step 5: Commit the hidden runner**

```powershell
git add src-tauri/src/auto_title/runner.rs src-tauri/src/auto_title/mod.rs src-tauri/src/auto_title/types.rs src-tauri/src/auto_title/context.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/session_state.rs
git commit -m "feat(titles): add isolated hidden title runner"
```

---

### Task 8: Coordinate Durable Claims, Retry, Recovery, and Cancellation

**Files:**
- Create: `src-tauri/src/auto_title/coordinator.rs`
- Modify: `src-tauri/src/auto_title/mod.rs`
- Modify: `src-tauri/src/auto_title/service.rs`
- Modify: `src-tauri/src/auto_title/types.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/bin/codeg_server.rs`
- Modify: `src-tauri/src/web/mod.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/src/web/handlers/conversations.rs`

**Interfaces:**
- Produces: `AutoTitleCoordinator::new(db: Arc<AppDatabase>, runner: Arc<dyn TitleAgentRunner>, emitter: EventEmitter) -> Arc<Self>`
- Produces: `fn notify_ready(&self)`, `async fn recover_and_start(&self) -> Result<(), DbError>`, `async fn cancel_conversation(&self, conversation_id: i32)`, and `async fn cancel_all(&self)`
- Produces: `async fn claim_next_ready(conn: &DatabaseConnection) -> Result<Option<AutoTitleClaim>, DbError>`
- Produces: `async fn record_attempt_failure(conn: &DatabaseConnection, claim: &AutoTitleClaim) -> Result<FailureTransition, DbError>`
- Produces: `FailureTransition::{Ready, RetryWait, Exhausted, Cancelled}` and `async fn claim_is_still_running(conn: &DatabaseConnection, claim: &AutoTitleClaim) -> Result<bool, DbError>`
- Produces: `async fn recover_interrupted_jobs(conn: &DatabaseConnection) -> Result<(), DbError>`
- Produces: claim-scoped active registrations `ActiveTitleAttempt { attempt: i32, cancellation: CancellationToken }`; `unregister_active(conversation_id, attempt)` removes the map entry only when both fields still identify that attempt
- Produces: `async fn settle_attempt_failure_with_retry(&self, claim: &AutoTitleClaim) -> FailureTransition`, which retries only the failure-state transaction and never invokes the title runner again
- Changes: `update_conversation_title_core(conn: &DatabaseConnection, coordinator: &AutoTitleCoordinator, conversation_id: i32, title: String) -> Result<(), AppCommandError>`, `delete_conversation_core(conn: &DatabaseConnection, coordinator: &AutoTitleCoordinator, conversation_id: i32) -> Result<(), AppCommandError>`, and `delete_conversation_with_cleanup_core(emitter: &EventEmitter, conn: &DatabaseConnection, coordinator: &AutoTitleCoordinator, conversation_id: i32) -> Result<(), AppCommandError>` as `async fn`s
- Produces under `#[cfg(any(test, feature = "test-utils"))]`: `AutoTitleCoordinator::new_inert_for_test(conn: DatabaseConnection) -> Arc<Self>`, backed by a runner that panics if called
- Produces under `#[cfg(any(test, feature = "test-utils"))]`: `AppState::new_for_test_with_title_runner(db: AppDatabase, data_dir: PathBuf, runner: Arc<dyn TitleAgentRunner>) -> Self`; ordinary `new_for_test` delegates to the normal test default, while Task 9 uses this constructor for deterministic integration coverage
- Wires: one `Arc<ConversationExperienceMutationGate>` into server `AppState`, desktop managed state, embedded-server state, and both test constructors; Task 9 and the reference-search plan must use this same instance rather than constructing per-handler locks

- [ ] **Step 1: Write failing state-machine and permit-order tests with a fake runner**

```rust
#[tokio::test]
async fn first_failure_waits_for_next_turn_and_second_failure_deletes_job() {
    let fixture = coordinator_fixture(FakeRunner::fail_twice()).await;
    fixture.make_ready(7, 1).await;
    fixture.coordinator.notify_ready();
    fixture.wait_for_state(7, AutoTitleJobState::RetryWait).await;
    assert_eq!(fixture.attempts(7).await, 1);

    fixture.complete_target_turn(7, "turn-2").await;
    fixture.wait_for_job_deleted(7).await;
    assert_eq!(fixture.runner.call_count(), 2);
}

#[tokio::test]
async fn ready_jobs_wait_for_capacity_before_claiming() {
    let fixture = coordinator_fixture(FakeRunner::blocked()).await;
    fixture.make_three_ready_jobs().await;
    fixture.coordinator.notify_ready();
    fixture.wait_for_running_count(2).await;
    assert_eq!(fixture.ready_count().await, 1);
    assert_eq!(fixture.unclaimed_ready_attempts().await, vec![0]);
}

#[tokio::test]
async fn interrupted_attempt_recovery_counts_started_work() {
    let fixture = recovery_fixture().await;
    fixture.seed_running(1, 1, 1, 2).await;
    fixture.seed_running(2, 1, 1, 1).await;
    fixture.seed_running(3, 2, 2, 2).await;
    recover_interrupted_jobs(&fixture.db.conn).await.expect("recover");
    assert_eq!(fixture.state(1).await, Some(AutoTitleJobState::Ready));
    assert_eq!(fixture.state(2).await, Some(AutoTitleJobState::RetryWait));
    assert_eq!(fixture.state(3).await, None);
}

#[tokio::test]
async fn attempt_one_cleanup_cannot_unregister_attempt_two() {
    let fixture = coordinator_fixture(FakeRunner::first_fails_second_blocks()).await;
    fixture.make_ready(7, 1).await;
    fixture.pause_attempt_cleanup(7, 1).await;
    fixture.coordinator.notify_ready();
    fixture.wait_for_state(7, AutoTitleJobState::Ready).await;
    fixture.coordinator.notify_ready(); // Simulates any unrelated queue wake.
    fixture.wait_for_active_attempt(7, 2).await;
    fixture.release_attempt_cleanup(7, 1).await;
    fixture.manual_rename(7, "Manual").await;
    assert!(fixture.runner.attempt_two_was_cancelled());
}

#[tokio::test]
async fn failure_transition_db_retry_does_not_rerun_the_model_or_leak_active_state() {
    let fixture = coordinator_fixture(FakeRunner::fail_once()).await;
    fixture.fail_failure_transition_commits().await;
    fixture.make_ready(9, 1).await;
    fixture.coordinator.notify_ready();
    fixture.wait_for_runner_calls(1).await;
    fixture.allow_failure_transition_commits().await;
    fixture.wait_for_state(9, AutoTitleJobState::RetryWait).await;
    assert_eq!(fixture.runner.call_count(), 1);
    assert!(!fixture.has_active_registration(9).await);
}
```

Define `FakeRunner`, `CoordinatorFixture`, `async fn coordinator_fixture(FakeRunner) -> CoordinatorFixture`, and `async fn recovery_fixture() -> CoordinatorFixture` in `auto_title::coordinator::tests`. `FakeRunner` owns an atomic call count plus a channel of scripted `Result<String, AutoTitleRunError>`/blocked steps and observes cancellation tokens. `CoordinatorFixture` owns the migrated DB, fake runner, coordinator, and explicit polling helpers bounded by `tokio::time::timeout(Duration::from_secs(2), ...)`; `make_ready`, `complete_target_turn`, and recovery seeds write the exact job columns from Task 1. Do not use unbounded sleeps. `recovery_fixture` returns the same fixture without starting the worker.

Add seven named cases in the same module: `new_usable_turn_while_attempt_one_runs_makes_failure_immediately_ready`, `database_commit_retry_reuses_one_runner_output`, `disable_between_claim_and_registration_cancels_without_running`, `rename_between_claim_and_registration_cancels_without_running`, `rename_while_runner_is_blocked_cancels_and_late_output_loses`, `orphan_ready_jobs_are_removed_when_setting_is_off`, and `claim_database_error_does_not_terminate_notification_worker`. Use barriers/channels in `FakeRunner` and the coordinator's test-only pre-registration gate to place each race deterministically; do not rely on scheduler timing. The orphan test seeds a ready row without a configured agent, notifies the worker, and requires deletion with zero runner calls. For the claim-error case, add a `#[cfg(test)]` one-shot hook immediately before the production `claim_next_ready` call that returns an injected `DbError` once. Use paused time, observe the first failed claim with zero runner calls, send several ordinary wake hints during the backoff and prove they cause no extra claim calls, advance through the one scheduled retry, and require the same ready row to run without another external notification. Compile the hook out of production.

- [ ] **Step 2: Run coordinator tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils first_failure_waits_for_next_turn_and_second_failure_deletes_job
cargo test --features test-utils ready_jobs_wait_for_capacity_before_claiming
cargo test --features test-utils attempt_one_cleanup_cannot_unregister_attempt_two
cargo test --features test-utils failure_transition_db_retry_does_not_rerun_the_model_or_leak_active_state
cargo test --features test-utils interrupted_attempt_recovery_counts_started_work
cargo test --features test-utils new_usable_turn_while_attempt_one_runs_makes_failure_immediately_ready
cargo test --features test-utils database_commit_retry_reuses_one_runner_output
cargo test --features test-utils disable_between_claim_and_registration_cancels_without_running
cargo test --features test-utils rename_between_claim_and_registration_cancels_without_running
cargo test --features test-utils rename_while_runner_is_blocked_cancels_and_late_output_loses
cargo test --features test-utils orphan_ready_jobs_are_removed_when_setting_is_off
cargo test --features test-utils claim_database_error_does_not_terminate_notification_worker
```

Expected: FAIL because claims, recovery, and retry scheduling are undefined.

- [ ] **Step 3: Implement the durable worker and cancel-after-commit orchestration**

Use an unbounded notification channel only as a wake-up hint; the database remains the queue. Maintain a replaceable process-wide cancellation root for Off plus a map of per-conversation child tokens. The drain loop acquires an owned permit before `claim_next_ready`, captures a child of the current Off root before claiming, and releases the permit only after failure/cancellation or committed success:

```rust
loop {
    let permit = self.attempts.clone().acquire_owned().await.expect("semaphore");
    let off_token = self.current_off_root().await.child_token();
    let claim = match claim_next_ready(&self.db.conn).await {
        Ok(Some(claim)) => {
            claim_error_backoff.reset();
            claim
        }
        Ok(None) => {
            claim_error_backoff.reset();
            drop(permit);
            break;
        }
        Err(error) => {
            tracing::warn!(%error, "ready title claim failed");
            drop(permit);
            self.schedule_unique_delayed_wake(claim_error_backoff.next_delay());
            break;
        }
    };
    let cancellation = self
        .register_active(claim.conversation_id, claim.attempt, off_token)
        .await;
    let still_running = match claim_is_still_running(&self.db.conn, &claim).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(conversation_id = claim.conversation_id, %error, "title claim recheck failed");
            let this = Arc::clone(self);
            tokio::spawn(async move {
                let _permit = permit;
                let transition = this.settle_attempt_failure_with_retry(&claim).await;
                this.unregister_active(claim.conversation_id, claim.attempt).await;
                if transition == FailureTransition::Ready {
                    this.notify_ready();
                }
            });
            continue;
        }
    };
    if cancellation.is_cancelled() || !still_running {
        self.unregister_active(claim.conversation_id, claim.attempt).await;
        drop(permit);
        continue;
    }
    let this = Arc::clone(self);
    tokio::spawn(async move {
        let _permit = permit;
        this.run_claim(claim, cancellation).await;
    });
}
```

The conditional claim orders by `(updated_at, conversation_id)`, changes `ready -> running`, increments attempts, sets `attempt_turn_seq = usable_turn_seq`, and snapshots the currently configured agent into `AutoTitleClaim`. Load that setting inside the claim transaction; if it resolves to Off/corrupt `None`, delete the orphaned jobs and return no claim so a manually corrupted database cannot spin forever on an unclaimable ready row. Parse the persisted job locale through Task 4's exact locale parser and use `AppLocale::En` only for a missing/corrupt legacy value, with a warning. If another worker wins the conditional update, loop to the next candidate while retaining the same permit. Attempt-one failure becomes `ready` immediately when `usable_turn_seq > attempt_turn_seq`, otherwise `retry_wait`; attempt-two failure deletes the job. `settle_attempt_failure_with_retry` retries `record_attempt_failure` after 100 ms, 500 ms, then every 5 seconds until the exact claim transitions or disappears; it retains the permit/output state but never reruns the model. Use it for runner failures and for an operational error from the post-registration `claim_is_still_running` check. After the failure transaction commits, `run_claim` conditionally unregisters its own `(conversation_id, attempt)` and calls `notify_ready()` only for `FailureTransition::Ready`, so a usable target turn that arrived during attempt one starts attempt two without waiting for an unrelated future notification.

On valid output, retry only the database transaction after 100 ms, 500 ms, then every 5 seconds while the exact job/claim remains active. Do not rerun the model. A conditional zero-row finalization stops immediately as cancellation/lost precedence. Apply the same backoff discipline to the failure transition as described above; a transient database error must not leave a `running` row or active-map entry stranded until process restart.

The notification worker itself remains alive across `claim_next_ready` database errors: log the error, release the unclaimed permit, schedule one delayed wake, break only the current drain, and return to its receive loop. Keep one worker-owned claim-error backoff with delays 100 ms, 500 ms, then 5 seconds; reset it after any successful claim query. Use an atomic/task handle so only one delayed wake is outstanding, and while that retry is pending consume/coalesce ordinary channel hints without starting another drain. The delayed wake clears the pending flag immediately before the next claim attempt; queued hints are wake hints only and do not multiply attempts. This database-maintenance retry is distinct from the product's prohibited timer-based second model attempt; no title runner starts until a durable ready claim succeeds. The drain function therefore returns `()` and handles claim errors internally; it must not retain a `Result`/`?` path that terminates the notification task.

At each production startup, build the runtime in dependency order after the database, data directory, `ConnectionManager`, `InternalAgentSessionRegistry`, and emitter exist. `AppDatabase` itself is not `Clone`, so make one coordinator/runner handle with `Arc::new(AppDatabase { conn: db.conn.clone() })` while `AppState.db` remains the existing owned value:

```rust
let title_db = Arc::new(AppDatabase { conn: db.conn.clone() });
let title_driver: Arc<dyn TitleConnectionDriver> = Arc::new(
    ManagerTitleConnectionDriver::new(Arc::new(connection_manager.clone_ref())),
);
let title_runner: Arc<dyn TitleAgentRunner> = Arc::new(HiddenAgentRunner::new(
    Arc::clone(&title_db),
    title_driver,
    Arc::clone(&internal_agent_session_registry),
    data_dir.clone(),
));
let auto_title_coordinator = AutoTitleCoordinator::new(
    title_db,
    title_runner,
    emitter.clone(),
);
auto_title_coordinator.recover_and_start().await?;
```

Use the existing synchronous/block-on setup boundary where desktop initialization cannot `await`; server startup awaits directly. Recover interrupted jobs before starting the receiver loop, then notify existing ready rows. Add this one coordinator and one `Arc<ConversationExperienceMutationGate>` to server `AppState` and desktop managed state. The embedded-server `AppState` literal in `web/mod.rs` clones the desktop-managed coordinator, internal-session registry, and mutation gate rather than constructing a second worker/runner, so Tauri and HTTP calls share active cancellation maps and setting order. Test constructors use the supplied fake/inert runner and never construct a production manager driver.

Add the coordinator to the lifecycle subscriber; when Task 5's committed transition reports `became_ready`, call `notify_ready` without awaiting a model request. Replace Task 3's temporary `.map(|_| ())` in the title/delete command cores: retain the committed `removed_job` boolean, call `cancel_conversation(conversation_id).await` only when it is true, then return `Ok(())`. Give the two Tauri wrappers `tauri::State<'_, Arc<AutoTitleCoordinator>>`, pass `coordinator.inner().as_ref()` into the shared cores, and have both Axum handlers pass `state.auto_title_coordinator.as_ref()`. Thread the same reference through `delete_conversation_with_cleanup_core`. Off calls `cancel_all` after its persisted transaction commits; changing one agent to another does not cancel active claims and affects only later claims.

Implement `AutoTitleCoordinator::new_inert_for_test` with an internal `InertTitleAgentRunner` whose `run` method panics, then use it as the ordinary synchronous `AppState::new_for_test` default; only the Task 9 integration constructor substitutes its supplied runner. In `commands::conversations::tests`, add `fn inert_title_coordinator(db: &AppDatabase) -> Arc<AutoTitleCoordinator>` calling that constructor and pass `coordinator.as_ref()` at every existing direct call to `update_conversation_title_core`, `delete_conversation_core`, and `delete_conversation_with_cleanup_core`. This includes the chat-folder cleanup, tab-cleanup/barrier, round-trip, soft-delete/upsert, and child-delete tests already in the file. In `acp::lifecycle::tests`, add the same module-local helper and pass the inert coordinator to every existing `lifecycle_subscriber_task` construction; sidecar-free tests must compile without starting title work. Do not make either production coordinator parameter optional merely to preserve old test call syntax.

Close both cancellation registration races. `cancel_all` cancels and replaces the Off root, so a disable between claim and active-map insertion still cancels that claim. For rename/delete, insert `ActiveTitleAttempt { attempt: claim.attempt, cancellation }` into the active map before `claim_is_still_running`; a mutation committed before insertion makes that recheck fail, while a mutation committed afterward finds and cancels the map entry. Every cleanup path calls `unregister_active(conversation_id, attempt)`, which checks the stored attempt before removal. This claim-scoped compare is required even if ordinary cleanup happens before `notify_ready`: an unrelated ready notification can observe the database's newly ready attempt two in the interval after the failure transaction commits and replace the map entry. The Step 1 pre-registration, blocked-runner, and attempt-overlap cases cover all three windows. On successful finalization, emit exactly one existing conversation upsert after commit; no retry attempt emits before commit.

- [ ] **Step 4: Verify state machine, cancellation races, and startup wiring**

Run:

```powershell
cd src-tauri
cargo test --features test-utils auto_title::coordinator::tests
cargo test --features test-utils manual_rename_cancels_active_title
cargo test --features test-utils disabling_titles_cancels_all_and_late_result_loses
cargo test --features test-utils soft_delete_cancels_active_title
cargo test --features test-utils lifecycle
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; no more than two attempts/outputs are retained, retries require the specified target turn, and late results cannot cross a committed cancellation or title predicate.

- [ ] **Step 5: Commit the coordinator**

```powershell
git add src-tauri/src/auto_title/coordinator.rs src-tauri/src/auto_title/mod.rs src-tauri/src/auto_title/service.rs src-tauri/src/auto_title/types.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/app_state.rs src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs src-tauri/src/web/mod.rs src-tauri/src/commands/conversations.rs src-tauri/src/web/handlers/conversations.rs
git commit -m "feat(titles): coordinate durable title generation"
```

---

### Task 9: Expose Settings, Send Visible UI Context, and Verify End to End

**Files:**
- Create: `src-tauri/src/web/handlers/conversation_experience.rs`
- Create: `src/stores/conversation-experience-store.ts`
- Create: `src/stores/conversation-experience-store.test.ts`
- Create: `src/components/settings/conversation-experience-settings.tsx`
- Create: `src/components/settings/conversation-experience-settings.test.tsx`
- Modify: `src-tauri/src/commands/conversation_experience.rs`
- Modify: `src-tauri/src/web/handlers/mod.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/tests/api_integration.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Create: `src/lib/api.test.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/contexts/app-workspace-context.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/hooks/use-connection-lifecycle.ts`
- Modify: `src/hooks/use-connection-lifecycle.test.ts`
- Modify: `src/components/settings/general-settings.tsx`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Produces: Tauri/Axum `get_conversation_experience_settings` and `set_auto_title_agent`
- Produces: `getConversationExperienceSettings()` and `setAutoTitleAgent(agent: AgentType | null)`
- Produces: `useConversationExperienceStore` with revision-gated `applySnapshot`, idempotent `initialize`, force-fetching `refresh`, and `setAutoTitleAgent`
- Produces: `set_auto_title_agent_core(db, emitter, coordinator, mutation_gate, agent) -> Result<ConversationExperienceSettings, AppCommandError>`, which holds the shared mutation gate through the committed cancellation decision and settings event
- Changes: `acpPrompt(connectionId, blocks, folderId, conversationId, clientMessageId, context: AcpPromptContext)` where `AcpPromptContext = { visibleText: string | null; locale: AppLocale | null }`
- Changes: `AcpActionsValue.sendPrompt` accepts `opts.promptContext?: AcpPromptContext`; it always supplies the API's required final context, using both-null only for a non-lifecycle legacy caller
- Produces: settings UI with only `Off` plus enabled-and-available base agents, while retaining an unavailable saved value as a disabled labeled row

- [ ] **Step 1: Write failing transport, store, UI, and end-to-end tests**

```ts
it("drops reordered settings responses and events", () => {
  const store = useConversationExperienceStore.getState()
  store.applySnapshot({
    auto_title_agent: "codex",
    reference_search_limit: 50,
    revision: 4,
  })
  store.applySnapshot({
    auto_title_agent: null,
    reference_search_limit: 50,
    revision: 3,
  })
  expect(useConversationExperienceStore.getState().settings?.revision).toBe(4)
  expect(
    useConversationExperienceStore.getState().settings?.auto_title_agent
  ).toBe("codex")
})

it("sends displayText and the effective app locale with the ACP prompt", async () => {
  await acpPrompt("connection", [{ type: "text", text: "wire" }], 1, 2, "m1", {
    visibleText: "README.md task",
    locale: "zh_cn",
  })
  expect(mockTransport.call).toHaveBeenCalledWith("acp_prompt", {
    connectionId: "connection",
    blocks: [{ type: "text", text: "wire" }],
    folderId: 1,
    conversationId: 2,
    clientMessageId: "m1",
    visibleText: "README.md task",
    locale: "zh_cn",
  })
})
```

Place the transport assertion above in new `src/lib/api.test.ts`; hoist a `getTransport` mock whose `call` is `mockTransport.call`, import `acpPrompt` only after the mock declaration, and reset it before each test. In `use-connection-lifecycle.test.ts`, add `handle_send_forwards_display_text_and_effective_locale`: set the effective locale mock to `"zh_cn"`, send `{ blocks: [{ type: "text", text: "wire" }], displayText: "README.md task" }`, and assert the bound connection action receives the existing IDs plus `promptContext: { visibleText: "README.md task", locale: "zh_cn" }`. In `acp-connections-context.test.tsx`, add `send_prompt_forwards_prompt_context_to_api`: seed one connected entry, invoke the action with that prompt context, and assert the `acpPrompt` mock receives it as the required sixth argument. Keep null context fallback coverage for any pre-existing direct action test. In `app-workspace-context.test.tsx`, mock `useConversationExperienceBootstrap` as a hoisted spy/no-op and assert it is called on provider mount; this prevents its additional settings-event subscription from overwriting the test's intentionally narrow conversation/folder handler capture.

In `src-tauri/tests/api_integration.rs`, construct state with Task 8's `AppState::new_for_test_with_title_runner`, start its lifecycle subscriber/coordinator, create a root and delegated child, emit usable completion sidecars with no attached clients, and assert each title updates once without changing `updated_at`. The injected fake returns deterministic titles and call counts; the test never launches an external CLI.

Add `concurrent_auto_title_saves_hold_the_gate_through_off_cancellation`: under `#[cfg(test)]`, let `AutoTitleCoordinator` expose a one-shot `pause_next_cancel_all_before_effect()` hook whose arrival handle resolves immediately before it replaces/cancels the root and whose release handle lets that call continue. Start an Off wrapper call, wait for that arrival (so its transaction has committed), then start an On wrapper call and assert with `tokio::time::timeout(Duration::from_millis(50), &mut on_task)` that On is still pending. Release Off, await both in order, and assert their revisions/events are monotonic with On last. Finally create and complete a newly eligible conversation and prove its runner is not cancelled. The test fails if the mutation gate is released at commit, because On can complete while Off is paused and the late Off effect can then cancel newer work. Keep the hook compiled out of production. This test is specifically about the post-commit side effect; the Task 3 creation/disable race continues to cover database enrollment.

- [ ] **Step 2: Run focused frontend/backend tests and confirm RED**

Run:

```powershell
pnpm test -- src/lib/api.test.ts src/stores/conversation-experience-store.test.ts src/components/settings/conversation-experience-settings.test.tsx src/contexts/acp-connections-context.test.tsx src/contexts/app-workspace-context.test.tsx src/hooks/use-connection-lifecycle.test.ts
cd src-tauri
cargo test --features test-utils --test api_integration automatic_title
```

Expected: FAIL because settings transports/store/UI and prompt display fields are absent.

- [ ] **Step 3: Add shared commands, revision events, frontend store, and UI context**

The title setter wrapper calls the persisted core, then `coordinator.cancel_all()` only for Off, emits the full saved document, and returns it:

```rust
pub async fn set_auto_title_agent_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    coordinator: &AutoTitleCoordinator,
    mutation_gate: &ConversationExperienceMutationGate,
    agent: Option<AgentType>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let _mutation_guard = mutation_gate.lock().await;
    let saved = set_auto_title_agent_persisted_core(db, agent).await?;
    if saved.auto_title_agent.is_none() {
        coordinator.cancel_all().await;
    }
    emit_event(emitter, CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT, saved.clone());
    Ok(saved)
}
```

The gate covers validation, the write transaction, `cancel_all`, and synchronous event emission. Do not release it immediately after the database commit: that recreates the race where a delayed older Off request cancels a claim enrolled after a newer On commit. Tauri receives the desktop-managed gate as `tauri::State<'_, Arc<ConversationExperienceMutationGate>>`; Axum reads the identical `state.conversation_experience_mutation_gate` instance.

Register the two Tauri commands and Axum routes `/get_conversation_experience_settings` and `/set_auto_title_agent`. Add matching TypeScript types/constants and a Zustand store that applies only `snapshot.revision > current.revision` and registers a backend-scoped reset. `initialize()` installs one event subscription plus reconnect callback and owns one initial in-flight getter; repeated calls are idempotent. `refresh()` always fetches a snapshot, coalesces only a currently in-flight refresh, retains the last good snapshot on failure, and is what reconnect invokes. `setAutoTitleAgent()` applies the returned full document through the same gate. Export `useConversationExperienceBootstrap()` and mount it in `AppWorkspaceProvider` as well as the settings section, with ref-counted initialization so multiple consumers create one event subscription and final release disposes it.

Extend ACP prompt options without changing `PromptDraft`:

```ts
export interface AcpPromptContext {
  visibleText: string | null
  locale: AppLocale | null
}
```

`use-connection-lifecycle.ts` passes `draft.displayText` and `getCurrentEffectiveAppLocale()` as `opts.promptContext` through the connection action. `AcpActionsValue.sendPrompt` forwards that object to `acpPrompt`; when an older direct caller omits it, supply `{ visibleText: null, locale: null }` rather than reconstructing display text from wire blocks in the frontend. API/Tauri payloads flatten the required context into `visibleText` and `locale`, keep the TypeScript `AppLocale` type, while both Rust wire boundaries deserialize `locale: Option<String>`, call Task 4's lossy `parse_supported_app_locale`, and build `PromptCaptureContext`.

Mount `ConversationExperienceSettingsSection` before delegation settings. Use `useAcpAgents`, show Off, filter choices by `enabled && available`, and retain a currently saved unavailable agent as a disabled `"<name> (Unavailable)"` option. Do not list delegation profiles.

Merge these exact values under `GeneralSettings` in each catalog; keep `{agent}` and `{message}` unchanged for `next-intl` interpolation:

```json
{
  "en": {
    "conversationExperienceTitle": "Conversation experience",
    "conversationExperienceDescription": "Configure automatic titles and reference search behavior.",
    "autoTitleAgent": "Automatic title agent",
    "autoTitleOff": "Off",
    "autoTitleUnavailable": "{agent} (Unavailable)",
    "autoTitleSaveFailed": "Failed to save automatic title agent: {message}",
    "autoTitleLoading": "Loading automatic title settings..."
  },
  "ar": {
    "conversationExperienceTitle": "تجربة المحادثة",
    "conversationExperienceDescription": "اضبط العناوين التلقائية وسلوك البحث عن المراجع.",
    "autoTitleAgent": "وكيل العنوان التلقائي",
    "autoTitleOff": "إيقاف",
    "autoTitleUnavailable": "{agent} (غير متاح)",
    "autoTitleSaveFailed": "تعذر حفظ وكيل العنوان التلقائي: {message}",
    "autoTitleLoading": "جار تحميل إعدادات العنوان التلقائي..."
  },
  "de": {
    "conversationExperienceTitle": "Konversationserlebnis",
    "conversationExperienceDescription": "Automatische Titel und das Verhalten der Referenzsuche konfigurieren.",
    "autoTitleAgent": "Agent für automatische Titel",
    "autoTitleOff": "Aus",
    "autoTitleUnavailable": "{agent} (Nicht verfügbar)",
    "autoTitleSaveFailed": "Agent für automatische Titel konnte nicht gespeichert werden: {message}",
    "autoTitleLoading": "Einstellungen für automatische Titel werden geladen..."
  },
  "es": {
    "conversationExperienceTitle": "Experiencia de conversación",
    "conversationExperienceDescription": "Configura los títulos automáticos y el comportamiento de la búsqueda de referencias.",
    "autoTitleAgent": "Agente de títulos automáticos",
    "autoTitleOff": "Desactivado",
    "autoTitleUnavailable": "{agent} (No disponible)",
    "autoTitleSaveFailed": "No se pudo guardar el agente de títulos automáticos: {message}",
    "autoTitleLoading": "Cargando la configuración de títulos automáticos..."
  },
  "fr": {
    "conversationExperienceTitle": "Expérience de conversation",
    "conversationExperienceDescription": "Configurez les titres automatiques et le comportement de la recherche de références.",
    "autoTitleAgent": "Agent de titre automatique",
    "autoTitleOff": "Désactivé",
    "autoTitleUnavailable": "{agent} (Indisponible)",
    "autoTitleSaveFailed": "Impossible d'enregistrer l'agent de titre automatique : {message}",
    "autoTitleLoading": "Chargement des paramètres de titre automatique..."
  },
  "ja": {
    "conversationExperienceTitle": "会話エクスペリエンス",
    "conversationExperienceDescription": "自動タイトルと参照検索の動作を設定します。",
    "autoTitleAgent": "自動タイトルエージェント",
    "autoTitleOff": "オフ",
    "autoTitleUnavailable": "{agent}（利用不可）",
    "autoTitleSaveFailed": "自動タイトルエージェントを保存できませんでした: {message}",
    "autoTitleLoading": "自動タイトル設定を読み込んでいます..."
  },
  "ko": {
    "conversationExperienceTitle": "대화 환경",
    "conversationExperienceDescription": "자동 제목 및 참조 검색 동작을 설정합니다.",
    "autoTitleAgent": "자동 제목 에이전트",
    "autoTitleOff": "끄기",
    "autoTitleUnavailable": "{agent} (사용할 수 없음)",
    "autoTitleSaveFailed": "자동 제목 에이전트를 저장하지 못했습니다: {message}",
    "autoTitleLoading": "자동 제목 설정을 불러오는 중..."
  },
  "pt": {
    "conversationExperienceTitle": "Experiência de conversa",
    "conversationExperienceDescription": "Configure títulos automáticos e o comportamento da pesquisa de referências.",
    "autoTitleAgent": "Agente de títulos automáticos",
    "autoTitleOff": "Desativado",
    "autoTitleUnavailable": "{agent} (Indisponível)",
    "autoTitleSaveFailed": "Falha ao salvar o agente de títulos automáticos: {message}",
    "autoTitleLoading": "Carregando configurações de títulos automáticos..."
  },
  "zh-CN": {
    "conversationExperienceTitle": "对话体验",
    "conversationExperienceDescription": "配置自动标题和引用搜索行为。",
    "autoTitleAgent": "自动标题智能体",
    "autoTitleOff": "关闭",
    "autoTitleUnavailable": "{agent}（不可用）",
    "autoTitleSaveFailed": "保存自动标题智能体失败：{message}",
    "autoTitleLoading": "正在加载自动标题设置..."
  },
  "zh-TW": {
    "conversationExperienceTitle": "對話體驗",
    "conversationExperienceDescription": "設定自動標題與參照搜尋行為。",
    "autoTitleAgent": "自動標題智慧體",
    "autoTitleOff": "關閉",
    "autoTitleUnavailable": "{agent}（無法使用）",
    "autoTitleSaveFailed": "儲存自動標題智慧體失敗：{message}",
    "autoTitleLoading": "正在載入自動標題設定..."
  }
}
```

- [ ] **Step 4: Run focused tests, all locale checks, and complete repository verification**

Run:

```powershell
pnpm eslint .
pnpm test
pnpm build
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command exits 0; desktop/server settings converge by revision, background root/delegated titles appear through the existing conversation upsert, manual/native precedence holds, and no internal session appears in Codeg discovery.

- [ ] **Step 5: Commit the complete automatic-title surface**

```powershell
git add src-tauri/src/web/handlers/conversation_experience.rs src-tauri/src/commands/conversation_experience.rs src-tauri/src/web/handlers/mod.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src-tauri/tests/api_integration.rs src/lib/types.ts src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts src/stores/conversation-experience-store.ts src/stores/conversation-experience-store.test.ts src/components/settings/conversation-experience-settings.tsx src/components/settings/conversation-experience-settings.test.tsx src/components/settings/general-settings.tsx src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/en.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json
git commit -m "feat(titles): expose automatic conversation titles"
```
