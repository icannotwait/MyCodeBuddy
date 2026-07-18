# Session Awaiting Reply and Relative Time Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show an unmistakable, race-safe awaiting-reply signal for unseen user-facing root-turn completions while making sidebar relative time always reflect backend latest activity with minute precision below ten hours.

**Architecture:** Persist one nullable UUID generation token on each conversation and clear it with an expected-token compare-and-set, so a delayed view acknowledgement cannot erase a newer completion. Carry eligibility per prompt into `TurnComplete`, update status/token/time atomically, publish one authoritative global conversation state patch, and clear only from a visibility-aware active-conversation observer. Keep `pending_review` as the lifecycle status and derive the red awaiting-reply presentation from `pending_review + token + not selected`.

**Tech Stack:** Rust 2021, SeaORM/SQLite, Tokio, Tauri 2, Axum, serde, UUID v4, TypeScript strict mode, React 19, Next.js 16 static export, Zustand, next-intl, Tailwind CSS v4, Vitest, Testing Library.

## Global Constraints

- Sidebar relative time always displays backend `conversation.updated_at`; sort mode changes ordering only.
- For `1h <= elapsed < 10h`, append unpadded remaining minutes only when nonzero: `3h`, `3h5m`, `9h59m`.
- Persist `conversation.awaiting_reply_token TEXT NULL`; do not persist a redundant boolean.
- Generate each eligible completion token with `uuid::Uuid::new_v4().to_string()`; no new dependency is required because `uuid` v4 is already enabled.
- The first qualifying view on any client clears the token globally for every client sharing SQLite. Per-client read receipts remain out of scope.
- A prompt must carry an explicit per-turn `mark_awaiting_reply` value. UI root turns use `true`; automation, chat-channel, and delegation turns use `false`.
- Changing attention eligibility must not change mandatory profile-route registration; chat-channel prompts keep their current route behavior while passing `mark_awaiting_reply=false`.
- Only an eligible root `end_turn` that wins `InProgress -> PendingReview` may create a token.
- Status, token, and backend `updated_at` change atomically. Manual status writes, prompt start, failures, Completed, and Cancelled clear the token.
- Clear accepts the observed token and never changes status or `updated_at`. A stale token is a successful no-op that returns current backend state.
- `get_folder_conversation` and every other read endpoint remain free of awaiting-reply clear side effects.
- `conversation://changed` is the only app-workspace/sidebar state stream. Per-connection status events must not also patch the workspace store.
- Root and child `conversation://changed` consumers use the same `state` patch shape; child seed buffering preserves status, token, and backend `updated_at` together.
- In tile mode only the active tile counts as viewed. Automations route, hidden/unfocused documents, and a maximized file pane do not count.
- Awaiting reply is gated defensively on `status === "pending_review"`; running and cancelled presentation always wins.
- Add `statusAwaitingReplyBadge` to all ten locale files: `ar`, `de`, `en`, `es`, `fr`, `ja`, `ko`, `pt`, `zh-CN`, and `zh-TW`.
- Keep Next.js static export compatibility; add no dynamic route.
- Preserve unrelated dirty-worktree changes. Before each commit, inspect and stage only the named paths or task-owned hunks.

## File Map

- `src-tauri/src/db/migration/m20260716_000001_conversation_awaiting_reply_token.rs`: nullable token migration and legacy-row test.
- `src-tauri/src/db/entities/conversation.rs`, `src-tauri/src/models/conversation.rs`, `src/lib/types.ts`: persisted and wire contracts.
- `src-tauri/src/db/service/conversation_service.rs`: atomic status transitions, end-turn CAS, state patches, and token clear CAS.
- `src-tauri/src/acp/connection.rs`, `src-tauri/src/acp/manager.rs`, `src-tauri/src/acp/types.rs`: per-prompt eligibility through emitted `TurnComplete`.
- `src-tauri/src/acp/lifecycle.rs`: CAS-driven completion/failure ownership.
- `src-tauri/src/web/event_bridge.rs`, `src-tauri/src/commands/conversations.rs`: authoritative global state event.
- `src-tauri/src/web/handlers/conversations.rs`, `src-tauri/src/web/router.rs`, `src-tauri/src/lib.rs`: shared clear operation exposed through Axum and Tauri.
- `src/stores/app-workspace-store.ts`, `src/contexts/app-workspace-context.tsx`: exact state-patch application with stable stats.
- `src/stores/tab-store.ts`, `src/hooks/use-subsession-sync.ts`: exact child state-patch application, including in-flight seed buffering.
- `src/components/conversations/conversation-awaiting-reply-clearer.tsx`: qualifying-view observer and expected-token acknowledgement.
- `src/components/conversations/sidebar-conversation-grouping.ts`, `sidebar-conversation-list.tsx`, `sidebar-conversation-card.tsx`: time source/format and visual state.
- `src/i18n/messages/*.json`: required accessible awaiting-reply label.

---

### Task 1: Persist the Token and Define Shared State Types

**Files:**
- Create: `src-tauri/src/db/migration/m20260716_000001_conversation_awaiting_reply_token.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/conversation.rs`
- Modify: `src-tauri/src/models/conversation.rs`
- Modify: `src-tauri/src/models/mod.rs`
- Modify: `src-tauri/src/db/service/conversation_service.rs`
- Modify: `src-tauri/src/db/service/import_service.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/components/conversations/active-session-details.test.ts`
- Modify: `src/components/conversations/session-details-dialog.test.tsx`
- Modify: `src/components/conversations/sidebar-conversation-card.test.tsx`
- Modify: `src/components/conversations/sidebar-conversation-grouping.test.ts`
- Modify: `src/components/conversations/sidebar-conversation-list.test.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/contexts/conversation-runtime-context.test.tsx`
- Modify: `src/contexts/tab-context.test.tsx`
- Modify: `src/hooks/use-subsession-sync.test.ts`
- Modify: `src/lib/export-conversation.test.ts`
- Modify: `src/stores/app-workspace-store.test.ts`
- Modify: `src/stores/background-overlay.test.ts`
- Modify: `src/stores/conversation-runtime-store.test.ts`
- Test: inline migration test in the new migration file

**Interfaces:**
- Produces DB/entity field: `awaiting_reply_token: Option<String>`.
- Produces Rust wire type: `ConversationStatePatch { id, status, awaiting_reply_token, updated_at }`.
- Produces TypeScript types: required `DbConversationSummary.awaiting_reply_token: string | null` and `ConversationStatePatch`.
- Preserves historical/default behavior: every existing/new row starts with `NULL`.

- [ ] **Step 1: Write and register the failing migration regression test first**

Create the migration file with the test module before implementing `up`:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn up_adds_nullable_awaiting_reply_token_defaulting_null() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared(
            "CREATE TABLE conversation (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT)",
        )
        .await
        .expect("create stub table");
        conn.execute_unprepared("INSERT INTO conversation (title) VALUES ('legacy')")
            .await
            .expect("insert legacy row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT awaiting_reply_token FROM conversation".to_owned(),
            ))
            .await
            .expect("query token")
            .expect("legacy row");
        let token: Option<String> = row
            .try_get("", "awaiting_reply_token")
            .expect("token column");
        assert!(token.is_none());
    }
}
```

Also add `mod m20260716_000001_conversation_awaiting_reply_token;` and append `Box::new(m20260716_000001_conversation_awaiting_reply_token::Migration)` to `Migrator::migrations()` now, so the RED command compiles this file.

- [ ] **Step 2: Run the focused test and verify RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils up_adds_nullable_awaiting_reply_token_defaulting_null --lib
```

Expected: compilation fails because `MigrationTrait` is not implemented for the new migration.

- [ ] **Step 3: Implement the registered migration**

Add the exact migration body:

```rust
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(ColumnDef::new(Conversation::AwaitingReplyToken).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::AwaitingReplyToken)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    AwaitingReplyToken,
}
```

Keep the module and migration-list entries added in Step 1 at the end of their respective lists.

- [ ] **Step 4: Add the entity, summary, and compact state-patch contracts**

Add to `conversation::Model`:

```rust
pub awaiting_reply_token: Option<String>,
```

Insert this always-serialized field immediately after `status` in `DbConversationSummary`:

```rust
pub awaiting_reply_token: Option<String>,
```

Add the compact patch after `DbConversationSummary`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ConversationStatePatch {
    pub id: i32,
    pub status: String,
    pub awaiting_reply_token: Option<String>,
    pub updated_at: DateTime<Utc>,
}
```

Re-export `ConversationStatePatch` from `models/mod.rs`. Add the TypeScript mirror next to `DbConversationSummary`:

```typescript
export interface ConversationStatePatch {
  id: number
  status: string
  awaiting_reply_token: string | null
  updated_at: string
}
```

Add this required field to `DbConversationSummary`:

```typescript
awaiting_reply_token: string | null
```

- [ ] **Step 5: Initialize and map the field everywhere a row/summary is constructed**

Add this field to all four production `conversation::ActiveModel` literals in `conversation_service.rs`, `import_service.rs`, and the fork-sibling path in `acp/manager.rs`:

```rust
awaiting_reply_token: Set(None),
```

Add this mapper field in `conv_to_summary` and the Rust `summary_child` fixture:

```rust
awaiting_reply_token: r.awaiting_reply_token,
```

```rust
awaiting_reply_token: None,
```

Add the exact default to the `DbConversationSummary` fixture builders in the fourteen test files listed under **Files**:

```typescript
awaiting_reply_token: null,
```

Do not add the field to the production `updateConversationLocal({ pinned_at: ... })` patch in `sidebar-conversation-list.tsx`; that object is not a summary. Run `pnpm exec tsc --noEmit` to find any additional required summary literal and add the same explicit null default rather than weakening the production field to optional.

- [ ] **Step 6: Run migration, Rust type, and TypeScript type checks**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils up_adds_nullable_awaiting_reply_token_defaulting_null --lib
cargo check
cd ..
pnpm exec tsc --noEmit
```

Expected: the migration test passes and both Rust and TypeScript compile with every constructor carrying the required nullable field.

- [ ] **Step 7: Commit the schema and shared types**

```powershell
git diff --check -- src-tauri/src/db/migration src-tauri/src/db/entities/conversation.rs src-tauri/src/models src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/commands/conversations.rs src/lib/types.ts src
git add src-tauri/src/db/migration/m20260716_000001_conversation_awaiting_reply_token.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/conversation.rs src-tauri/src/models/conversation.rs src-tauri/src/models/mod.rs src/lib/types.ts
git add -p -- src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/commands/conversations.rs
git add -p -- src/components/conversations/active-session-details.test.ts src/components/conversations/session-details-dialog.test.tsx src/components/conversations/sidebar-conversation-card.test.tsx src/components/conversations/sidebar-conversation-grouping.test.ts src/components/conversations/sidebar-conversation-list.test.tsx src/components/message/message-list-view.test.tsx src/contexts/app-workspace-context.test.tsx src/contexts/conversation-runtime-context.test.tsx src/contexts/tab-context.test.tsx src/hooks/use-subsession-sync.test.ts src/lib/export-conversation.test.ts src/stores/app-workspace-store.test.ts src/stores/background-overlay.test.ts src/stores/conversation-runtime-store.test.ts
git commit -m "feat(conversations): persist awaiting reply generation"
```

Expected: only schema/type additions and constructor fixture updates are committed.

---

### Task 2: Add Atomic Status, Completion, and Clear Services

**Files:**
- Modify: `src-tauri/src/db/service/conversation_service.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Test: inline tests in `src-tauri/src/db/service/conversation_service.rs`

**Interfaces:**
- Produces: `update_status_with_patch(...) -> Result<ConversationStatePatch, DbError>`.
- Produces: `update_status_if_with_patch(...) -> Result<Option<ConversationStatePatch>, DbError>`.
- Produces: `finish_end_turn_if_in_progress(..., mark_awaiting_reply: bool) -> Result<Option<ConversationStatePatch>, DbError>`.
- Produces: `clear_awaiting_reply(..., expected_token: &str) -> Result<ClearAwaitingReplyOutcome, DbError>`.
- Preserves compatibility wrappers: existing `update_status -> Result<(), DbError>` and `update_status_if -> Result<bool, DbError>` delegate to the patch-returning helpers.

- [ ] **Step 1: Add failing atomicity and stale-clear tests**

Add these tests to the existing service test module:

```rust
#[tokio::test]
async fn awaiting_reply_eligible_root_end_turn_sets_one_generation_atomically() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-root").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("create root");

    let patch = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .expect("finish")
        .expect("CAS changed row");
    assert_eq!(patch.status, "pending_review");
    assert!(patch.awaiting_reply_token.is_some());

    let duplicate = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .expect("duplicate finish");
    assert!(duplicate.is_none(), "duplicate end_turn must lose the CAS");
}

#[tokio::test]
async fn awaiting_reply_background_root_and_child_never_get_a_generation() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-ineligible").await;
    let root = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("root");
    let root_patch = finish_end_turn_if_in_progress(&db.conn, root.id, false)
        .await
        .expect("background finish")
        .expect("root transition");
    assert!(root_patch.awaiting_reply_token.is_none());

    let (parent_id, child_id) = seed_parent_with_child(&db.conn, folder).await;
    let child_patch = finish_end_turn_if_in_progress(&db.conn, child_id, true)
        .await
        .expect("child finish")
        .expect("child transition");
    assert_eq!(child_patch.status, "pending_review");
    assert!(child_patch.awaiting_reply_token.is_none());
    assert_ne!(parent_id, child_id);
}

#[tokio::test]
async fn awaiting_reply_terminal_status_wins_over_delayed_end_turn() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-terminal-race").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("root");
    update_status(&db.conn, row.id, ConversationStatus::Completed)
        .await
        .expect("complete first");

    assert!(finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .expect("delayed finish")
        .is_none());
    let current = get_by_id(&db.conn, row.id).await.expect("current row");
    assert_eq!(current.status, "completed");
    assert!(current.awaiting_reply_token.is_none());
}

#[tokio::test]
async fn awaiting_reply_stale_clear_cannot_remove_a_newer_generation() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-stale-clear").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("root");
    let first = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .unwrap()
        .unwrap();
    let token_a = first.awaiting_reply_token.expect("token A");

    update_status(&db.conn, row.id, ConversationStatus::InProgress)
        .await
        .expect("next prompt");
    let second = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .unwrap()
        .unwrap();
    let token_b = second.awaiting_reply_token.clone().expect("token B");
    assert_ne!(token_a, token_b);

    let stale = clear_awaiting_reply(&db.conn, row.id, &token_a)
        .await
        .expect("stale clear");
    assert!(!stale.changed);
    assert_eq!(stale.patch.awaiting_reply_token.as_deref(), Some(token_b.as_str()));
}

#[tokio::test]
async fn awaiting_reply_matching_clear_preserves_status_and_updated_at() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-clear").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("root");
    let before = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .unwrap()
        .unwrap();
    let token = before.awaiting_reply_token.clone().unwrap();

    let cleared = clear_awaiting_reply(&db.conn, row.id, &token)
        .await
        .expect("clear");
    assert!(cleared.changed);
    assert_eq!(cleared.patch.status, "pending_review");
    assert!(cleared.patch.awaiting_reply_token.is_none());
    assert_eq!(cleared.patch.updated_at, before.updated_at);
}

#[tokio::test]
async fn awaiting_reply_metadata_preserves_token_but_manual_status_clears_it() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/await-metadata").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .expect("root");
    let marked = finish_end_turn_if_in_progress(&db.conn, row.id, true)
        .await
        .unwrap()
        .unwrap();
    let token = marked.awaiting_reply_token.clone().unwrap();

    update_title(&db.conn, row.id, "renamed".into()).await.unwrap();
    update_pin(&db.conn, row.id, true).await.unwrap();
    update_external_id(&db.conn, row.id, "external-1".into())
        .await
        .unwrap();
    assert_eq!(
        get_by_id(&db.conn, row.id)
            .await
            .unwrap()
            .awaiting_reply_token
            .as_deref(),
        Some(token.as_str())
    );

    update_status(&db.conn, row.id, ConversationStatus::PendingReview)
        .await
        .expect("manual review status");
    assert!(get_by_id(&db.conn, row.id)
        .await
        .unwrap()
        .awaiting_reply_token
        .is_none());
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

```powershell
cd src-tauri
cargo test --features test-utils awaiting_reply --lib
```

Expected: compilation fails because the patch-returning transition and clear helpers do not exist.

- [ ] **Step 3: Implement exact state-patch conversion and status wrappers**

Add these internal/public shapes:

```rust
#[derive(Debug, Clone)]
pub struct ClearAwaitingReplyOutcome {
    pub patch: ConversationStatePatch,
    pub changed: bool,
}

fn status_string(status: &conversation::ConversationStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}"))
}

fn state_patch(
    id: i32,
    status: conversation::ConversationStatus,
    awaiting_reply_token: Option<String>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> ConversationStatePatch {
    ConversationStatePatch {
        id,
        status: status_string(&status),
        awaiting_reply_token,
        updated_at,
    }
}
```

Implement `update_status_with_patch` by loading the row, setting the requested status, setting `awaiting_reply_token = Set(None)`, setting one backend `now`, updating, and returning a patch. Keep the existing API as a wrapper:

```rust
pub async fn update_status(
    conn: &DatabaseConnection,
    conversation_id: i32,
    status: conversation::ConversationStatus,
) -> Result<(), DbError> {
    update_status_with_patch(conn, conversation_id, status)
        .await
        .map(|_| ())
}
```

Implement `update_status_if_with_patch` with one `UPDATE ... WHERE status = expected`, clearing the token and using one captured `now`. Keep `update_status_if` as:

```rust
pub async fn update_status_if(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected: conversation::ConversationStatus,
    new_status: conversation::ConversationStatus,
) -> Result<bool, DbError> {
    Ok(update_status_if_with_patch(conn, conversation_id, expected, new_status)
        .await?
        .is_some())
}
```

- [ ] **Step 4: Implement end-turn and expected-token CAS helpers**

Use one immutable `parent_id` read followed by one conditional update:

```rust
pub async fn finish_end_turn_if_in_progress(
    conn: &DatabaseConnection,
    conversation_id: i32,
    mark_awaiting_reply: bool,
) -> Result<Option<ConversationStatePatch>, DbError> {
    use sea_orm::sea_query::Expr;

    let row = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let token = (row.parent_id.is_none() && mark_awaiting_reply)
        .then(|| uuid::Uuid::new_v4().to_string());
    let now = Utc::now();
    let result = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::Status,
            Expr::value(conversation::ConversationStatus::PendingReview),
        )
        .col_expr(
            conversation::Column::AwaitingReplyToken,
            Expr::value(token.clone()),
        )
        .col_expr(conversation::Column::UpdatedAt, Expr::value(now))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::Status.eq(conversation::ConversationStatus::InProgress))
        .exec(conn)
        .await?;
    Ok((result.rows_affected == 1).then(|| {
        state_patch(
            conversation_id,
            conversation::ConversationStatus::PendingReview,
            token,
            now,
        )
    }))
}
```

Implement clear with `AwaitingReplyToken.eq(expected_token)`, write only `NULL`, then fetch current row and return `ClearAwaitingReplyOutcome`. The fetch occurs after both match and mismatch, so callers always receive current backend state.

- [ ] **Step 5: Route the automation direct status write through the service**

Replace `AutomationEngine::cancel_conversation`'s direct `ActiveModel.status = Set(...)` with `update_status_with_patch`. Keep its existing full-summary upsert after a successful write. This ensures even rare automation-launch cancellation clears a stale token atomically.

- [ ] **Step 6: Run the focused service suite and strict Rust checks**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils awaiting_reply --lib
cargo test --features test-utils conversation_service::tests --lib
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all new CAS tests pass; existing status and conversation-service tests remain green; Clippy reports no warnings.

- [ ] **Step 7: Commit the atomic service layer**

```powershell
git add -p -- src-tauri/src/db/service/conversation_service.rs src-tauri/src/automation/engine.rs
git commit -m "feat(conversations): add atomic awaiting reply state"
```

---

### Task 3: Carry Eligibility Per Prompt and Use Lifecycle CAS

**Files:**
- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src-tauri/src/chat_channel/session_commands.rs`
- Modify: `src-tauri/src/chat_channel/session_event_subscriber.rs`
- Modify: all Rust test/perf fixtures constructing `AcpEvent::TurnComplete`
- Modify: `src/lib/types.ts`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Modify: `src/contexts/select-transcript-apply-events.test.ts`
- Test: existing inline tests in `acp/types.rs`, `acp/manager.rs`, and `acp/lifecycle.rs`

**Interfaces:**
- Extends `ConnectionCommand::Prompt` and `AcpEvent::TurnComplete` with required `mark_awaiting_reply: bool`.
- Produces `send_prompt_background(...)` and `send_prompt_linked_background(...)` wrappers.
- UI prompt wrappers use `true`; delegation derives `false`; automation/chat call background wrappers explicitly.
- Lifecycle consumes the event field and calls the Task 2 end-turn CAS.

- [ ] **Step 1: Add failing wire, command-policy, and lifecycle tests**

Extend the existing `TurnComplete` serde test:

```rust
let event = AcpEvent::TurnComplete {
    session_id: "session-1".into(),
    stop_reason: "end_turn".into(),
    agent_type: "codex".into(),
    mark_awaiting_reply: true,
};
let json = serde_json::to_value(&event).unwrap();
assert_eq!(json["mark_awaiting_reply"], true);
```

Add manager tests using `insert_test_connection_live` and the returned command receiver:

```rust
#[tokio::test]
async fn prompt_wrappers_encode_user_facing_and_background_attention() {
    let mgr = ConnectionManager::new();
    let mut rx = mgr
        .insert_test_connection_live(
            "policy-conn",
            AgentType::Codex,
            None,
            EventEmitter::Noop,
        )
        .await;

    mgr.send_prompt("policy-conn", one_text_block())
        .await
        .expect("UI prompt");
    let ConnectionCommand::Prompt { mark_awaiting_reply, .. } = rx.recv().await.unwrap() else {
        panic!("expected prompt command");
    };
    assert!(mark_awaiting_reply);

    {
        let state = mgr.get_state("policy-conn").await.unwrap();
        state.write().await.turn_in_flight = false;
    }
    mgr.send_prompt_background("policy-conn", one_text_block())
        .await
        .expect("background prompt");
    let ConnectionCommand::Prompt { mark_awaiting_reply, .. } = rx.recv().await.unwrap() else {
        panic!("expected background prompt command");
    };
    assert!(!mark_awaiting_reply);
}
```

Add lifecycle variants of the existing root end-turn test: `mark_awaiting_reply=true` yields a token, `false` leaves it null, a child stays null, and a row pre-set to Completed remains Completed.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cd src-tauri
cargo test --features test-utils prompt_wrappers_encode_user_facing_and_background_attention --lib
cargo test --features test-utils handle_event_updates_conversation_status_on_turn_complete --lib
```

Expected: compilation fails because prompt/TurnComplete policy fields and background wrappers are missing.

- [ ] **Step 3: Extend the command and event payloads**

Replace the `Prompt` variant with this exact shape:

```rust
Prompt {
    blocks: Vec<PromptInputBlock>,
    user_message: Option<(String, Vec<UserMessageBlock>)>,
    mark_awaiting_reply: bool,
},
```

```rust
TurnComplete {
    session_id: String,
    stop_reason: String,
    agent_type: String,
    mark_awaiting_reply: bool,
},
```

Add the required TypeScript event field:

```typescript
type: "turn_complete"
session_id: string
stop_reason: string
mark_awaiting_reply: boolean
```

Set `mark_awaiting_reply: false` on synthetic/test/perf fixtures unless the test specifically represents a UI root turn.

- [ ] **Step 4: Thread policy through manager admission and connection completion**

Add `mark_awaiting_reply: bool` to the end of `send_prompt_inner`, set it on `ConnectionCommand::Prompt`, destructure it in the connection loop, and copy it into every real `TurnComplete` emitted for that prompt.

Keep the existing UI wrapper and add the background twin:

```rust
pub async fn send_prompt_background(
    &self,
    conn_id: &str,
    blocks: Vec<PromptInputBlock>,
) -> Result<(), AcpError> {
    let prompt_lock = self.clone_prompt_lock(conn_id).await?;
    let _guard = prompt_lock.lock_owned().await;
    self.send_prompt_inner(conn_id, blocks, None, true, false).await
}
```

The existing `send_prompt` calls `send_prompt_inner(..., true, true)`. The first boolean remains mandatory profile-route registration, so the chat-channel background wrapper deliberately keeps it `true` and changes only attention eligibility. Introduce one private linked implementation accepting the policy, keep UI `send_prompt_linked_with_message_id` passing `delegation.is_none()`, and expose:

```rust
pub async fn send_prompt_linked_background(
    &self,
    db: &AppDatabase,
    conn_id: &str,
    blocks: Vec<PromptInputBlock>,
    folder_id: Option<i32>,
    conversation_id: Option<i32>,
) -> Result<Option<i32>, AcpError>
```

It delegates to the private linked implementation with no delegation, no client message id, the existing root mandatory-route behavior, and `mark_awaiting_reply=false`.

- [ ] **Step 5: Mark non-UI production callers explicitly**

- Change the automation engine's `send_prompt_linked_with_message_id(..., None)` call to `send_prompt_linked_background(...)`.
- Change all three chat-channel `send_prompt(...)` calls in `session_commands.rs` and `session_event_subscriber.rs` to `send_prompt_background(...)`.
- Keep UI Tauri/Axum send handlers on `send_prompt_linked_with_message_id`.
- Keep delegation calls false through their non-null delegation argument.
- Do not change mandatory profile-route registration for chat-channel or automation prompts while changing their attention policy.

Run this audit after editing:

```powershell
rg -n "send_prompt\(|send_prompt_linked_with_message_id\(" src-tauri/src/automation src-tauri/src/chat_channel
```

Expected: no automation/chat production call uses the user-facing wrappers.

- [ ] **Step 6: Replace lifecycle's unconditional completion write with CAS helpers**

Change the lifecycle match arm pattern to bind the new field:

```rust
AcpEvent::TurnComplete {
    stop_reason,
    mark_awaiting_reply,
    ..
}
```

Use that pattern on the existing `=>` arm. For `end_turn`, call `finish_end_turn_if_in_progress`. For refusal/max-token/unknown/empty, call `update_status_if_with_patch(InProgress, Cancelled)`. Emit the existing per-connection `ConversationStatusChanged` only when the helper returns `Some(patch)`. Keep broker forwarding outside the CAS branch so delegation completion settlement remains unchanged.

- [ ] **Step 7: Run focused and affected backend/frontend tests**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils prompt_wrappers_encode_user_facing_and_background_attention --lib
cargo test --features test-utils acp::types::tests --lib
cargo test --features test-utils acp::lifecycle::tests --lib
cargo test --features test-utils chat_channel::session_event_subscriber::tests --lib
cd ..
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx src/contexts/select-transcript-apply-events.test.ts
```

Expected: policy propagation, lifecycle CAS, chat background behavior, serde, and TypeScript event fixtures all pass.

- [ ] **Step 8: Commit per-turn eligibility and lifecycle ownership**

```powershell
git add -p -- src-tauri/src/acp/types.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/automation/engine.rs src-tauri/src/chat_channel/session_commands.rs src-tauri/src/chat_channel/session_event_subscriber.rs src-tauri/src/acp/perf_fixture.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/desktop_event_batcher.rs src-tauri/src/chat_channel/event_subscriber.rs src-tauri/src/pet_sessions.rs src-tauri/src/pet_state_mapper.rs src/lib/types.ts src/contexts/acp-connections-context.test.tsx src/contexts/select-transcript-apply-events.test.ts
git commit -m "feat(acp): carry awaiting reply policy per turn"
```

---

### Task 4: Publish One Authoritative Global Conversation State Event

**Files:**
- Modify: `src-tauri/src/web/event_bridge.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src-tauri/src/chat_channel/session_event_subscriber.rs`
- Modify: `src-tauri/src/chat_channel/session_commands.rs`
- Test: inline tests in `src-tauri/src/web/event_bridge.rs`, `src-tauri/src/acp/manager.rs`, and `src-tauri/src/acp/lifecycle.rs`

**Interfaces:**
- Replaces `ConversationChange::Status { id, status }` with `ConversationChange::State { patch: ConversationStatePatch }`.
- Produces `emit_conversation_state(emitter, patch)`.
- Removes automatic global bridging from `emit_with_state`.
- Requires each successful DB transition to emit exactly its returned backend patch.

- [ ] **Step 1: Write failing event-shape and no-duplicate tests**

Add `conversation_state_event_serializes_patch` beside the existing emitter tests in `commands/conversations.rs`:

```rust
let patch = ConversationStatePatch {
    id: 42,
    status: "pending_review".into(),
    awaiting_reply_token: Some("token-42".into()),
    updated_at: chrono::Utc::now(),
};
emit_conversation_state(&emitter, patch.clone());
let event = rx.try_recv().expect("global state event");
assert_eq!(event.channel, CONVERSATION_CHANGED_EVENT);
assert_eq!(event.payload["kind"], "state");
assert_eq!(event.payload["patch"]["id"], 42);
assert_eq!(event.payload["patch"]["awaiting_reply_token"], "token-42");
```

In `web/event_bridge.rs`, add `conversation_status_changed_does_not_bridge_globally`: call `emit_with_state` with `ConversationStatusChanged` and assert the global broadcaster receives no `conversation://changed` event. Add manager/lifecycle tests asserting each successful status transition emits exactly one `state` event containing backend `updated_at` and token.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cd src-tauri
cargo test --features test-utils conversation_state_event --lib
cargo test --features test-utils conversation_status_changed_does_not_bridge_globally --lib
```

Expected: tests fail because the `State` variant/emitter do not exist and `emit_with_state` still bridges status automatically.

- [ ] **Step 3: Define the state variant and explicit emitter**

Use this Rust event shape:

```rust
pub enum ConversationChange {
    Upsert { summary: Box<crate::models::DbConversationSummary> },
    Deleted { id: i32 },
    State { patch: crate::models::ConversationStatePatch },
}
```

Add beside `emit_conversation_upsert`:

```rust
pub(crate) fn emit_conversation_state(
    emitter: &EventEmitter,
    patch: ConversationStatePatch,
) {
    emit_event(
        emitter,
        CONVERSATION_CHANGED_EVENT,
        ConversationChange::State { patch },
    );
}
```

Delete the `if let AcpEvent::ConversationStatusChanged` global bridge at the end of `emit_with_state` and update its comments/tests. Per-connection delivery remains unchanged.

- [ ] **Step 4: Emit returned patches at every status owner**

At each DB transition, keep the ordering `DB write -> per-connection status event when applicable -> global state patch`:

```rust
if let Some(patch) = conversation_service::update_status_if_with_patch(
    db,
    conversation_id,
    ConversationStatus::InProgress,
    ConversationStatus::Cancelled,
).await? {
    emit_with_state(
        &state,
        &emitter,
        AcpEvent::ConversationStatusChanged {
            conversation_id,
            status: ConversationStatus::Cancelled,
        },
    ).await;
    crate::commands::conversations::emit_conversation_state(&emitter, patch);
}
```

Use `update_status_with_patch` and explicit global state emission for:

- prompt start
- prompt-send rollback to Cancelled
- user cancel CAS
- lifecycle `end_turn` / failure / terminal disconnect
- chat-channel Completed/Cancelled writes
- automation cancellation writes

For chat-channel code, clone the connection emitter with `get_state_and_emitter(connection_id)` before emitting. Remove or consolidate redundant status writes in `session_commands::handle_cancel` so one user cancel cannot produce two patches.

- [ ] **Step 5: Audit all status writes for token clearing and global convergence**

```powershell
rg -n "\.status\s*=\s*Set\(|update_status\(|update_status_if\(|update_status_with_patch|update_status_if_with_patch" src-tauri/src --glob '!vendor/**'
```

Expected: every production conversation-status write either emits its returned `ConversationStatePatch`, is followed by an existing full-summary upsert for a rare metadata path, or is a test-only setup. No direct production `ActiveModel.status = Set(...)` remains.

- [ ] **Step 6: Run event, lifecycle, manager, chat, and automation tests**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils web::event_bridge::tests --lib
cargo test --features test-utils acp::manager::tests --lib
cargo test --features test-utils acp::lifecycle::tests --lib
cargo test --features test-utils chat_channel::session_event_subscriber::tests --lib
cargo test --features test-utils automation::engine::tests --lib
```

Expected: state events carry exact backend values and no duplicate global status event is produced.

- [ ] **Step 7: Commit the authoritative event path**

```powershell
git add -p -- src-tauri/src/web/event_bridge.rs src-tauri/src/commands/conversations.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/automation/engine.rs src-tauri/src/chat_channel/session_event_subscriber.rs src-tauri/src/chat_channel/session_commands.rs
git commit -m "feat(conversations): broadcast authoritative state patches"
```

---

### Task 5: Expose Expected-Token Clear Through Tauri and Axum

**Files:**
- Modify: `src-tauri/src/commands/conversations.rs`
- Modify: `src-tauri/src/web/handlers/conversations.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: inline tests in `src-tauri/src/commands/conversations.rs`

**Interfaces:**
- Produces shared core: `clear_awaiting_reply_core(conn, emitter, conversation_id, expected_token) -> Result<ConversationStatePatch, AppCommandError>`.
- Produces Tauri/Axum command name: `clear_awaiting_reply`.
- Input wire shape: `{ conversationId: number, expectedToken: string }`.
- Output wire shape: `ConversationStatePatch`.

- [ ] **Step 1: Write failing clear-core event and read-only getter tests**

Add command tests that:

1. Seed a root with an eligible end-turn token.
2. Call `clear_awaiting_reply_core` with that token.
3. Assert returned token is null, `updated_at` is unchanged, and one `conversation://changed` `state` event is emitted.
4. Call again with the stale token and assert no second event.
5. Seed another token, call `get_folder_conversation_core`, and assert the token remains unchanged.

Use this core assertion shape:

```rust
let cleared = clear_awaiting_reply_core(&db.conn, &emitter, id, token.clone())
    .await
    .expect("clear");
assert!(cleared.awaiting_reply_token.is_none());
assert_eq!(cleared.updated_at, before.updated_at);
let event = rx.try_recv().expect("state event");
assert_eq!(event.payload["kind"], "state");
assert_eq!(event.payload["patch"]["awaiting_reply_token"], serde_json::Value::Null);
```

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cd src-tauri
cargo test --features test-utils clear_awaiting_reply_core --lib
cargo test --features test-utils get_folder_conversation_does_not_clear_awaiting_reply --lib
```

Expected: compilation fails because the command core does not exist.

- [ ] **Step 3: Implement the shared core and Tauri command**

```rust
pub async fn clear_awaiting_reply_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    conversation_id: i32,
    expected_token: String,
) -> Result<ConversationStatePatch, AppCommandError> {
    let outcome = conversation_service::clear_awaiting_reply(
        conn,
        conversation_id,
        &expected_token,
    )
    .await
    .map_err(AppCommandError::from)?;
    if outcome.changed {
        emit_conversation_state(emitter, outcome.patch.clone());
    }
    Ok(outcome.patch)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn clear_awaiting_reply(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
    expected_token: String,
) -> Result<ConversationStatePatch, AppCommandError> {
    clear_awaiting_reply_core(
        &db.conn,
        &EventEmitter::Tauri(app),
        conversation_id,
        expected_token,
    )
    .await
}
```

Register `conversations::clear_awaiting_reply` in `tauri::generate_handler!`.

- [ ] **Step 4: Implement the Axum twin and route**

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearAwaitingReplyParams {
    pub conversation_id: i32,
    pub expected_token: String,
}

pub async fn clear_awaiting_reply(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ClearAwaitingReplyParams>,
) -> Result<Json<ConversationStatePatch>, AppCommandError> {
    Ok(Json(
        conv_commands::clear_awaiting_reply_core(
            &state.db.conn,
            &state.emitter,
            params.conversation_id,
            params.expected_token,
        )
        .await?,
    ))
}
```

Add `POST /clear_awaiting_reply` beside the other conversation routes. Web transport needs no command map because it already posts to `/api/${command}`.

- [ ] **Step 5: Run core tests and compile both runtimes**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils clear_awaiting_reply_core --lib
cargo test --features test-utils get_folder_conversation_does_not_clear_awaiting_reply --lib
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: clear behavior tests pass and both Tauri and Axum registrations compile.

- [ ] **Step 6: Commit the explicit clear API**

```powershell
git add -p -- src-tauri/src/commands/conversations.rs src-tauri/src/web/handlers/conversations.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs
git commit -m "feat(conversations): expose awaiting reply acknowledgement"
```

---

### Task 6: Make Global State Patches the Sole Frontend Workspace Authority

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/stores/app-workspace-store.ts`
- Modify: `src/stores/app-workspace-store.test.ts`
- Modify: `src/stores/tab-store.ts`
- Modify: `src/hooks/use-subsession-sync.ts`
- Modify: `src/hooks/use-subsession-sync.test.ts`
- Modify: `src/contexts/app-workspace-context.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/contexts/tab-context.test.tsx`
- Modify: `src/app/workspace/layout.tsx`

**Interfaces:**
- Extends `ConversationChange` with `{ kind: "state"; patch: ConversationStatePatch }` and removes `kind: "status"`.
- Produces store action `applyConversationStatePatch(patch): void`.
- Migrates both child-session consumers from `status` to `state`; child summaries merge `status`, `awaiting_reply_token`, and backend `updated_at` atomically.
- Buffers a full child state patch while a child seed fetch is in flight, so the live patch wins over the fetched snapshot.
- Removes `ConversationStatusEventBridge` as an app-workspace mutation source.
- Preserves stats object identity because state patches cannot change count, Agent type, or message count.

- [ ] **Step 1: Add failing store exactness and event convergence tests**

Add to `app-workspace-store.test.ts`:

```typescript
it("applies backend conversation state without inventing updated_at", () => {
  const store = useAppWorkspaceStore.getState()
  store.applyConversationUpsert(makeSummary({
    id: 1,
    status: "in_progress",
    awaiting_reply_token: null,
    updated_at: "2026-07-16T01:00:00.000Z",
  }))
  const statsBefore = useAppWorkspaceStore.getState().stats

  store.applyConversationStatePatch({
    id: 1,
    status: "pending_review",
    awaiting_reply_token: "generation-b",
    updated_at: "2026-07-16T02:03:04.000Z",
  })

  const state = useAppWorkspaceStore.getState()
  expect(state.conversations[0]).toMatchObject({
    status: "pending_review",
    awaiting_reply_token: "generation-b",
    updated_at: "2026-07-16T02:03:04.000Z",
  })
  expect(state.stats).toBe(statsBefore)
})

it("ignores a state patch for an unknown conversation", () => {
  const before = useAppWorkspaceStore.getState()
  before.applyConversationStatePatch({
    id: 999,
    status: "pending_review",
    awaiting_reply_token: "unknown",
    updated_at: "2026-07-16T02:03:04.000Z",
  })
  expect(useAppWorkspaceStore.getState().conversations).toBe(before.conversations)
})
```

Add an `AppWorkspaceProvider` test that emits `{kind:"state", patch}` and observes all three fields update. Delete bridge-only test scaffolding and imports together with `ConversationStatusEventBridge`; the Task 4 backend no-duplicate test and Task 9 source audit prove ACP status is no longer a workspace path.

Update the existing child event tests in `use-subsession-sync.test.ts` and `tab-context.test.tsx` to emit `{ kind: "state", patch }`. Assert that a loaded child receives `status`, `awaiting_reply_token`, and exact backend `updated_at`, unknown child ids remain identity-preserving no-ops, and a state patch buffered during an in-flight child seed wins over all three older fetched fields.

- [ ] **Step 2: Run focused frontend tests and verify RED**

```powershell
pnpm exec vitest run src/stores/app-workspace-store.test.ts src/contexts/app-workspace-context.test.tsx src/hooks/use-subsession-sync.test.ts src/contexts/tab-context.test.tsx
```

Expected: tests fail because the state type/action do not exist, the ACP bridge still patches status, and child consumers still expect the removed status-only shape.

- [ ] **Step 3: Implement the exact store action**

Add to the state interface and store:

```typescript
applyConversationStatePatch: (patch: ConversationStatePatch) => void
```

```typescript
applyConversationStatePatch: (patch) => {
  const prev = get().conversations
  const index = prev.findIndex((conversation) => conversation.id === patch.id)
  if (index < 0) return
  const next = prev.slice()
  next[index] = {
    ...next[index],
    status: patch.status,
    awaiting_reply_token: patch.awaiting_reply_token,
    updated_at: patch.updated_at,
  }
  set({ conversations: next, stats: get().stats })
},
```

Do not route this through `updateConversationLocal`; that method synthesizes client `updated_at` for optimistic UI operations.

- [ ] **Step 4: Switch the provider to one global stream and remove the ACP bridge**

Update the global subscriber branch:

```typescript
if (change.kind === "upsert") {
  store.applyConversationUpsert(change.summary)
} else if (change.kind === "deleted") {
  store.applyConversationRemove(change.id)
} else {
  store.applyConversationStatePatch(change.patch)
}
```

Delete `ConversationStatusEventBridge`, remove its `useAcpEvent` import, remove its layout import/render, and adjust the context test mock comment. Keep ACP status handling inside the connection runtime untouched.

Update `tab-store.handleChildConversationChange` and `use-subsession-sync` to branch on `kind === "state"` and merge `change.patch.status`, `change.patch.awaiting_reply_token`, and `change.patch.updated_at` into a known child summary. In `tab-store`, replace the seed buffer's status string with a full `ConversationStatePatch`; apply that patch after the buffered/full summary choice so the newest state wins during an in-flight fetch. Preserve deleted-event terminal precedence and unknown-id no-op behavior.

- [ ] **Step 5: Run focused tests, typecheck, and lint**

```powershell
pnpm exec vitest run src/stores/app-workspace-store.test.ts src/contexts/app-workspace-context.test.tsx src/hooks/use-subsession-sync.test.ts src/contexts/tab-context.test.tsx
pnpm exec tsc --noEmit
pnpm eslint src/lib/types.ts src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts src/stores/tab-store.ts src/hooks/use-subsession-sync.ts src/hooks/use-subsession-sync.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/contexts/tab-context.test.tsx src/app/workspace/layout.tsx
```

Expected: global state events update root and child tabs/sidebar with exact backend state, buffered child patches win over seed fetches, stats references remain stable, and ACP events no longer duplicate the mutation.

- [ ] **Step 6: Commit the frontend state authority**

```powershell
git add -p -- src/lib/types.ts src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts src/stores/tab-store.ts src/hooks/use-subsession-sync.ts src/hooks/use-subsession-sync.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/contexts/tab-context.test.tsx src/app/workspace/layout.tsx
git commit -m "feat(conversations): apply authoritative state patches"
```

---

### Task 7: Clear Only from a Qualifying Active View

**Files:**
- Create: `src/components/conversations/conversation-awaiting-reply-clearer.tsx`
- Create: `src/components/conversations/conversation-awaiting-reply-clearer.test.tsx`
- Modify: `src/lib/api.ts`
- Modify: `src/app/workspace/layout.tsx`

**Interfaces:**
- Produces API `clearAwaitingReply(conversationId: number, expectedToken: string): Promise<ConversationStatePatch>`.
- Produces always-mounted `ConversationAwaitingReplyClearer` inside `WorkspaceProvider`, `TabProvider`, and `WorkbenchRouteProvider`.
- Qualifying inputs: hydrated active persisted tab, token, conversations route, non-maximized file pane, visible/focused document.
- Deduplicates requests by exact `(conversationId, token)` and applies every returned patch.

- [ ] **Step 1: Add failing API/visibility observer tests**

Create tests with mocked `clearAwaitingReply`, `useWorkbenchRoute`, and `useWorkspaceView`, plus real Zustand stores. Cover these exact cases:

```typescript
it("clears the active token once while the conversation is genuinely visible", async () => {
  seedActiveConversation({ id: 7, awaiting_reply_token: "generation-7" })
  documentHasFocus.mockReturnValue(true)
  render(<ConversationAwaitingReplyClearer />)

  await waitFor(() =>
    expect(clearAwaitingReply).toHaveBeenCalledWith(7, "generation-7")
  )
  expect(clearAwaitingReply).toHaveBeenCalledTimes(1)
  expect(useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token)
    .toBeNull()
})

it.each([
  ["automations route", { isConversations: false, filesMaximized: false, visible: true, focused: true }],
  ["maximized files", { isConversations: true, filesMaximized: true, visible: true, focused: true }],
  ["hidden document", { isConversations: true, filesMaximized: false, visible: false, focused: true }],
  ["unfocused document", { isConversations: true, filesMaximized: false, visible: true, focused: false }],
])("does not clear while %s", async (_label, state) => {
  seedActiveConversation({ id: 8, awaiting_reply_token: "generation-8" })
  routeMock.isConversations = state.isConversations
  workspaceMock.filesMaximized = state.filesMaximized
  setDocumentVisibility(state.visible ? "visible" : "hidden")
  documentHasFocus.mockReturnValue(state.focused)
  render(<ConversationAwaitingReplyClearer />)
  await Promise.resolve()
  expect(clearAwaitingReply).not.toHaveBeenCalled()
})

it("does not clear a token owned by an inactive tile", async () => {
  seedTiledConversations({ activeId: 8, inactiveId: 9, inactiveToken: "generation-9" })
  documentHasFocus.mockReturnValue(true)
  render(<ConversationAwaitingReplyClearer />)
  await Promise.resolve()
  expect(clearAwaitingReply).not.toHaveBeenCalled()
})

it("acknowledges a newer token with its own CAS after a stale clear loses", async () => {
  seedActiveConversation({ id: 9, awaiting_reply_token: "generation-a" })
  clearAwaitingReply
    .mockResolvedValueOnce({
      id: 9,
      status: "pending_review",
      awaiting_reply_token: "generation-b",
      updated_at: "2026-07-16T02:00:00.000Z",
    })
    .mockResolvedValueOnce({
      id: 9,
      status: "pending_review",
      awaiting_reply_token: null,
      updated_at: "2026-07-16T02:00:00.000Z",
    })
  render(<ConversationAwaitingReplyClearer />)
  await waitFor(() => {
    expect(clearAwaitingReply).toHaveBeenNthCalledWith(1, 9, "generation-a")
    expect(clearAwaitingReply).toHaveBeenNthCalledWith(2, 9, "generation-b")
  })
  expect(useAppWorkspaceStore.getState().conversations[0].awaiting_reply_token)
    .toBeNull()
})
```

The test helper sets `tabsHydrated=true`, inserts one active tab with `conversationId`, inserts its summary, stubs `document.visibilityState` to `visible`, and makes the clear mock return a null-token patch by default. `seedTiledConversations` enables tile mode, makes only `activeId` the active tab, and gives only the visible inactive tab a token.

- [ ] **Step 2: Run the new test and verify RED**

```powershell
pnpm exec vitest run src/components/conversations/conversation-awaiting-reply-clearer.test.tsx
```

Expected: module resolution fails because the clearer and API function do not exist.

- [ ] **Step 3: Add the transport-agnostic frontend API**

```typescript
export async function clearAwaitingReply(
  conversationId: number,
  expectedToken: string
): Promise<ConversationStatePatch> {
  return getTransport().call("clear_awaiting_reply", {
    conversationId,
    expectedToken,
  })
}
```

Import `ConversationStatePatch` as a type in `lib/api.ts`.

- [ ] **Step 4: Implement a document-activity external store**

Use `useSyncExternalStore` so focus/visibility transitions trigger a render without polling:

```typescript
function subscribeDocumentActivity(notify: () => void): () => void {
  window.addEventListener("focus", notify)
  window.addEventListener("blur", notify)
  document.addEventListener("visibilitychange", notify)
  return () => {
    window.removeEventListener("focus", notify)
    window.removeEventListener("blur", notify)
    document.removeEventListener("visibilitychange", notify)
  }
}

function getDocumentActivity(): boolean {
  return document.visibilityState === "visible" && document.hasFocus()
}

function getServerDocumentActivity(): boolean {
  return false
}
```

- [ ] **Step 5: Implement the clearer with exact-token deduplication**

Start the new file with these imports:

```typescript
"use client"

import {
  useEffect,
  useReducer,
  useRef,
  useSyncExternalStore,
} from "react"
import { useShallow } from "zustand/react/shallow"
import { useWorkbenchRoute } from "@/contexts/workbench-route-context"
import { useWorkspaceView } from "@/contexts/workspace-context"
import { useTabStore } from "@/contexts/tab-context"
import { clearAwaitingReply } from "@/lib/api"
import { onTransportReconnect } from "@/lib/platform"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
```

Use narrow selectors and the low-frequency workspace view hook:

```typescript
export function ConversationAwaitingReplyClearer() {
  const { activeTabId, tabsHydrated, activeConversationId } = useTabStore(
    useShallow((state) => {
      const active = state.tabs.find((tab) => tab.id === state.activeTabId)
      return {
        activeTabId: state.activeTabId,
        tabsHydrated: state.tabsHydrated,
        activeConversationId: active?.conversationId ?? null,
      }
    })
  )
  const token = useAppWorkspaceStore((state) =>
    activeConversationId == null
      ? null
      : (state.conversations.find((item) => item.id === activeConversationId)
          ?.awaiting_reply_token ?? null)
  )
  const applyPatch = useAppWorkspaceStore(
    (state) => state.applyConversationStatePatch
  )
  const { isConversations } = useWorkbenchRoute()
  const { filesMaximized } = useWorkspaceView()
  const documentActive = useSyncExternalStore(
    subscribeDocumentActivity,
    getDocumentActivity,
    getServerDocumentActivity
  )
  const inFlight = useRef(new Set<string>())
  const [retryEpoch, requestRetry] = useReducer((value: number) => value + 1, 0)

  useEffect(() => {
    const offReconnect = onTransportReconnect(requestRetry)
    return () => offReconnect?.()
  }, [])

  useEffect(() => {
    if (!tabsHydrated || !activeTabId || activeConversationId == null || !token) return
    if (!isConversations || filesMaximized || !documentActive) return
    const key = `${activeConversationId}:${token}`
    if (inFlight.current.has(key)) return
    inFlight.current.add(key)
    void clearAwaitingReply(activeConversationId, token)
      .then(applyPatch)
      .catch((error) => {
        console.warn("[AwaitingReply] clear failed", error)
      })
      .finally(() => {
        inFlight.current.delete(key)
      })
  }, [
    activeConversationId,
    activeTabId,
    applyPatch,
    documentActive,
    filesMaximized,
    isConversations,
    retryEpoch,
    tabsHydrated,
    token,
  ])

  return null
}
```

The counter intentionally appears only in the clear effect dependency array: reconnect requests one retry without changing persisted state. Do not schedule a tight timer retry.

- [ ] **Step 6: Mount the clearer inside all required providers**

Render it immediately inside `WorkbenchRouteProvider`, after `WorkbenchRouteConversationSync`, where `WorkspaceProvider` and `TabProvider` are already ancestors:

```tsx
<WorkbenchRouteConversationSync />
<ConversationAwaitingReplyClearer />
```

This placement lets sidebar/search/deep-link/tab switches converge through one observer rather than adding calls to every open path.

- [ ] **Step 7: Run observer tests, related provider tests, typecheck, and lint**

```powershell
pnpm exec vitest run src/components/conversations/conversation-awaiting-reply-clearer.test.tsx src/contexts/tab-context.test.tsx src/contexts/app-workspace-context.test.tsx
pnpm exec tsc --noEmit
pnpm eslint src/components/conversations/conversation-awaiting-reply-clearer.tsx src/components/conversations/conversation-awaiting-reply-clearer.test.tsx src/lib/api.ts src/app/workspace/layout.tsx
```

Expected: visibility gates, stale response behavior, provider placement, types, and lint all pass.

- [ ] **Step 8: Commit the qualifying-view acknowledgement**

```powershell
git add src/components/conversations/conversation-awaiting-reply-clearer.tsx src/components/conversations/conversation-awaiting-reply-clearer.test.tsx
git add -p -- src/lib/api.ts src/app/workspace/layout.tsx
git commit -m "feat(conversations): clear awaiting reply on focused view"
```

---

### Task 8: Fix Relative Time and Render Awaiting/Cancelled Presentation

**Files:**
- Modify: `src/components/conversations/sidebar-conversation-grouping.ts`
- Modify: `src/components/conversations/sidebar-conversation-grouping.test.ts`
- Modify: `src/components/conversations/sidebar-conversation-list.tsx`
- Modify: `src/components/conversations/sidebar-conversation-list.test.tsx`
- Modify: `src/components/conversations/sidebar-conversation-card.tsx`
- Modify: `src/components/conversations/sidebar-conversation-card.test.tsx`
- Modify: `src/lib/types.ts`
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
- Keeps `formatRelative(iso: string, now: number): string` and changes only the `<10h` branch.
- Sidebar card derives awaiting state from status, token, and selection; no extra prop.
- Cancelled global status color becomes gray while its right-side X remains destructive red.
- Accessible label is required and localized.

- [ ] **Step 1: Expand relative-time and display-source tests first**

Replace the compact bucket test with a table:

```typescript
it.each([
  [30_000, "now"],
  [5 * MINUTE, "5m"],
  [59 * MINUTE, "59m"],
  [60 * MINUTE, "1h"],
  [61 * MINUTE, "1h1m"],
  [(3 * 60 + 5) * MINUTE, "3h5m"],
  [(3 * 60 + 25) * MINUTE, "3h25m"],
  [(9 * 60 + 59) * MINUTE, "9h59m"],
  [10 * 60 * MINUTE, "10h"],
  [(23 * 60 + 59) * MINUTE, "23h"],
  [24 * 60 * MINUTE, "1d"],
  [2 * 24 * 60 * MINUTE, "2d"],
])("formats %i milliseconds as %s", (elapsed, label) => {
  expect(formatRelative(new Date(now - elapsed).toISOString(), now)).toBe(label)
})
```

Keep invalid/future/determinism tests. Add a list test with `sortMode="created"`, an old `created_at`, and a five-minute-old `updated_at`; assert the rendered meta is `5m`, proving created sort does not select the label source.

- [ ] **Step 2: Add failing awaiting and cancelled card tests**

Add:

```tsx
it("renders required awaiting reply chrome only for unselected pending review", () => {
  const awaiting = {
    ...conv(10),
    status: "pending_review",
    awaiting_reply_token: "generation-10",
  }
  const { getByTitle, container } = renderCard(awaiting)
  expect(getByTitle("Awaiting your reply")).toHaveClass(
    "text-destructive",
    "font-medium"
  )
  expect(container.querySelector(".bg-destructive")).not.toBeNull()
  expect(getByTitle("Awaiting your reply")).toHaveTextContent("5m")
})

it("gives cancelled precedence over a malformed stale token", () => {
  const cancelled = {
    ...conv(11),
    status: "cancelled",
    awaiting_reply_token: "stale",
  }
  const { getByTitle, queryByTitle, container } = renderCard(cancelled)
  expect(getByTitle("Cancelled")).toBeInTheDocument()
  expect(queryByTitle("Awaiting your reply")).toBeNull()
  expect(container.querySelector(".bg-gray-400")).not.toBeNull()
})
```

Add a selected-card test proving the awaiting title/classes are absent.

- [ ] **Step 3: Run focused UI tests and verify RED**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-conversation-grouping.test.ts src/components/conversations/sidebar-conversation-list.test.tsx src/components/conversations/sidebar-conversation-card.test.tsx
```

Expected: the new hour/minute, updated-at source, awaiting presentation, cancelled color, and accessible-label assertions fail.

- [ ] **Step 4: Implement the relative-time branch and updated-at source**

Use this exact hour branch:

```typescript
const h = Math.floor(m / 60)
if (h < 24) {
  const remainingMinutes = m % 60
  if (h < 10 && remainingMinutes > 0) {
    return `${h}h${remainingMinutes}m`
  }
  return `${h}h`
}
```

Replace the list card prop with:

```tsx
timeLabel={formatRelative(conv.updated_at, now)}
```

Do not alter created/updated comparators or sort controls.

- [ ] **Step 5: Implement defensive presentation precedence and accessibility**

Add:

```typescript
const showAwaitingReply =
  status === "pending_review" &&
  conversation.awaiting_reply_token !== null &&
  !isSelected
```

Pass `showAwaitingReply && "bg-destructive"` after existing status-dot classes so `cn`/Tailwind merge selects red only for the eligible state. On the time span use:

```tsx
title={
  showAwaitingReply ? tSidebar("statusAwaitingReplyBadge") : undefined
}
```

```tsx
className={cn(
  "relative shrink-0 tabular-nums text-[0.71875rem]",
  showAwaitingReply
    ? "font-medium text-destructive"
    : isSelected
      ? "font-medium text-muted-foreground"
      : "font-normal text-muted-foreground/70"
)}
```

Inside that span, before the visible time, add:

```tsx
{showAwaitingReply && (
  <span className="sr-only">
    {tSidebar("statusAwaitingReplyBadge")}: {" "}
  </span>
)}
```

Change only the cancelled entry in `STATUS_COLORS`:

```typescript
cancelled: "bg-gray-400 dark:bg-gray-500",
```

- [ ] **Step 6: Add all ten locale strings exactly**

Insert after `statusCancelledBadge` in each `Folder.sidebar` object:

```text
ar:    "statusAwaitingReplyBadge": "بانتظار ردك"
de:    "statusAwaitingReplyBadge": "Wartet auf deine Antwort"
en:    "statusAwaitingReplyBadge": "Awaiting your reply"
es:    "statusAwaitingReplyBadge": "Esperando tu respuesta"
fr:    "statusAwaitingReplyBadge": "En attente de votre réponse"
ja:    "statusAwaitingReplyBadge": "返信待ち"
ko:    "statusAwaitingReplyBadge": "답변 대기 중"
pt:    "statusAwaitingReplyBadge": "Aguardando sua resposta"
zh-CN: "statusAwaitingReplyBadge": "待回复"
zh-TW: "statusAwaitingReplyBadge": "待回覆"
```

Retain `pending_review` labels unchanged.

- [ ] **Step 7: Run focused tests, JSON parsing, typecheck, and lint**

```powershell
pnpm exec vitest run src/components/conversations/sidebar-conversation-grouping.test.ts src/components/conversations/sidebar-conversation-list.test.tsx src/components/conversations/sidebar-conversation-card.test.tsx
Get-ChildItem src/i18n/messages/*.json | ForEach-Object { Get-Content -Raw $_.FullName | ConvertFrom-Json | Out-Null }
pnpm exec tsc --noEmit
pnpm eslint src/components/conversations/sidebar-conversation-grouping.ts src/components/conversations/sidebar-conversation-grouping.test.ts src/components/conversations/sidebar-conversation-list.tsx src/components/conversations/sidebar-conversation-list.test.tsx src/components/conversations/sidebar-conversation-card.tsx src/components/conversations/sidebar-conversation-card.test.tsx src/lib/types.ts
```

Expected: all boundary, source, card, accessibility, locale JSON, type, and lint checks pass.

- [ ] **Step 8: Commit time and presentation**

```powershell
git add -p -- src/components/conversations/sidebar-conversation-grouping.ts src/components/conversations/sidebar-conversation-grouping.test.ts src/components/conversations/sidebar-conversation-list.tsx src/components/conversations/sidebar-conversation-list.test.tsx src/components/conversations/sidebar-conversation-card.tsx src/components/conversations/sidebar-conversation-card.test.tsx src/lib/types.ts
git add src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/en.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json
git commit -m "feat(sidebar): show unseen agent completions"
```

---

### Task 9: Verify Both Runtimes and the End-to-End Attention Contract

**Files:**
- No new files.
- Verify every path changed in Tasks 1-8.

**Interfaces:**
- Verifies migration/model parity, per-turn eligibility, CAS behavior, global events, both APIs, frontend visibility, UI precedence, all locales, and static export as one releasable feature.

- [ ] **Step 1: Run complete frontend checks**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: ESLint, the complete Vitest suite, and Next.js static export pass with zero failures.

- [ ] **Step 2: Run desktop Rust checks**

```powershell
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: desktop compilation, all tests, and strict Clippy pass.

- [ ] **Step 3: Run server and MCP companion checks**

```powershell
cd src-tauri
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: server and companion configurations compile and lint without warnings; server library tests pass.

- [ ] **Step 4: Run the race and source audit one final time**

```powershell
rg -n "\.status\s*=\s*Set\(|update_status\(|update_status_if\(" src-tauri/src --glob '!vendor/**'
rg -n "send_prompt\(|send_prompt_linked_with_message_id\(" src-tauri/src/automation src-tauri/src/chat_channel
rg -n "formatRelative\(.*created_at|sortMode ===.*created_at" src/components/conversations
rg -n "ConversationStatusEventBridge|\| \{ kind: \"status\"; id: number; status: string \}" src/lib/types.ts src/contexts src/hooks src/stores
rg -n "change\.kind === \"status\"" src/contexts src/hooks src/stores
```

Expected:

- no unowned production status write bypasses token clearing/event convergence;
- automation/chat use only background prompt wrappers;
- no sidebar label reads `created_at`;
- no duplicate workspace ACP status bridge or legacy `ConversationChange` status variant/consumer remains; unrelated domain types such as delegation action `kind: "status"` are outside this audit.

- [ ] **Step 5: Perform the manual multi-client smoke matrix**

1. Start one desktop client and one browser client on the same backend/database.
2. Complete two UI root turns; verify both rows show red dot/time in both clients.
3. Open one row in the browser; verify it clears in desktop and browser while the other remains red.
4. Start a second turn, capture token A, complete again to token B, submit a delayed clear using A, and verify B remains.
5. Complete while the window is unfocused and while Automations is active; verify the marker remains until the conversation route/tab is focused.
6. Enable tile mode; verify an inactive tile remains marked until activated.
7. Run one automation, chat-channel turn, and delegation child; verify none receives awaiting-reply styling.
8. Switch sidebar sort to Created; verify order changes while the displayed time still tracks latest activity.

- [ ] **Step 6: Inspect the final scoped diff**

```powershell
git diff --check
git status --short
```

Expected: no whitespace errors. Feature files are clean or intentionally staged; unrelated pre-existing changes remain untouched.

- [ ] **Step 7: Route any verification failure back to its owning task**

If a command in Steps 1-4 fails, stop this task, return to the earliest task that owns the failing behavior, add or correct its focused regression test, rerun that task's GREEN command, and create the task-scoped commit named there. Then restart Task 9 from Step 1. Do not create a generic verification commit or an empty commit.
