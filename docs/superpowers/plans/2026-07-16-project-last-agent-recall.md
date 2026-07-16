# Project Last Agent Recall Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remember the Agent used by the latest successfully created normal root conversation in each project, and use it as the next draft's fallback without overwriting the explicit project default.

**Architecture:** Persist implicit recency in `folder.last_agent_type` and expose it through `FolderDetail`. Keep `create_conversation_core` as the generic primitive used by automation and tests; add a shared project-create core used only by the Tauri and Axum normal-create wrappers, which records recency after insertion and returns the fresh folder for the existing `folder://changed` channel. Extend the pure frontend resolver with the recent candidate and retain the current provisional correction behavior until the fresh enabled/available Agent list can validate it.

**Tech Stack:** SeaORM migrations/entities/services, Rust Tauri commands, Axum handlers, React 19, TypeScript, Zustand, Vitest, Testing Library.

## Global Constraints

- `folder.last_agent_type` is nullable text and never replaces `folder.default_agent_type`.
- Only a successfully inserted normal project root conversation records recency.
- Draft selection, opening old history, delegated children, imports, automations, hidden Chat folders, and failed creates do not record recency.
- A recency write failure after insertion is warning-only; the create request remains successful.
- Selection priority is explicit folder default, explicitly requested active-conversation inheritance, project recent Agent, first enabled/available sorted Agent, then the existing hard fallback.
- Before the Agent registry is fresh, a recent Agent is provisional; once fresh, it is usable only when present in `sortedTypes`.
- Invalid persisted Agent strings deserialize as no recency and are not deleted.
- Successful project creation emits a fresh `folder://changed` upsert so all clients converge; reconnect refresh remains the dropped-event backstop.
- Preserve unrelated worktree changes and stage only files named by each task.

---

### Task 1: Persist and Project Folder Agent Recency

**Files:**
- Create: `src-tauri/src/db/migration/m20260716_000002_folder_last_agent.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/folder.rs`
- Modify: `src-tauri/src/db/service/folder_service.rs`
- Modify: `src-tauri/src/models/folder.rs`
- Modify: `src-tauri/src/commands/folders.rs`
- Modify: `src-tauri/src/web/event_bridge.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/stores/folder-derivation-decoupling.test.ts`
- Modify: `src/lib/branch-switch.test.ts`
- Modify: `src/contexts/tab-context.test.tsx`
- Modify: `src/components/chat/conversation-context-bar.test.tsx`
- Modify: `src/components/conversations/sidebar-conversation-list.test.tsx`

**Interfaces:**
- Produces: `folder::Model.last_agent_type: Option<String>`
- Produces: `FolderDetail.last_agent_type: Option<AgentType>` in Rust.
- Produces: `FolderDetail.last_agent_type: AgentType | null` in TypeScript.
- Produces: `folder_service::update_folder_last_agent(&DatabaseConnection, i32, AgentType) -> Result<Option<FolderDetail>, DbError>`.

- [ ] **Step 1: Create and register a no-op migration with a failing upgrade test**

Create `m20260716_000002_folder_last_agent.rs`:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Folder {
    Table,
    LastAgentType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn existing_folders_migrate_without_implicit_recency() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE folder (id INTEGER PRIMARY KEY, name TEXT NOT NULL)".to_string(),
        ))
        .await
        .expect("create old schema");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO folder (id, name) VALUES (1, 'repo')".to_string(),
        ))
        .await
        .expect("seed old row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT last_agent_type FROM folder WHERE id = 1".to_string(),
            ))
            .await
            .expect("query migrated row")
            .expect("migrated row");
        let last_agent_type: Option<String> = row
            .try_get("", "last_agent_type")
            .expect("read last_agent_type");
        assert_eq!(last_agent_type, None);
    }
}
```

Register it after the current final migration in `db/migration/mod.rs`. When the
thinking-visibility plan has already landed, that is
`m20260716_000001_agent_show_thinking`; otherwise it is the existing final
migration. This plan does not import or depend on the thinking migration:

```rust
mod m20260716_000002_folder_last_agent;
```

```rust
Box::new(m20260716_000002_folder_last_agent::Migration),
```

- [ ] **Step 2: Run the migration test and verify the missing column failure**

```powershell
cd src-tauri
cargo test --features test-utils existing_folders_migrate_without_implicit_recency
```

Expected: FAIL while reading `last_agent_type` because the no-op migration did
not add the column.

- [ ] **Step 3: Implement the nullable column and make the migration test pass**

Replace the no-op methods with:

```rust
async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Folder::Table)
                .add_column(ColumnDef::new(Folder::LastAgentType).text().null())
                .to_owned(),
        )
        .await
}

async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
    manager
        .alter_table(
            Table::alter()
                .table(Folder::Table)
                .drop_column(Folder::LastAgentType)
                .to_owned(),
        )
        .await
}
```

Run:

```powershell
cd src-tauri
cargo test --features test-utils existing_folders_migrate_without_implicit_recency
```

Expected: PASS with `last_agent_type == None` for the pre-upgrade row.

- [ ] **Step 4: Add a failing service round-trip and validation test**

Append to `folder_service.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::{add_chat_folder, get_folder_by_id, update_folder_last_agent};
    use crate::db::entities::folder;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

    #[tokio::test]
    async fn last_agent_round_trips_only_for_regular_folders() {
        let db = fresh_in_memory_db().await;
        let regular_id = seed_folder(&db, "/tmp/codeg-last-agent").await;
        let chat = add_chat_folder(&db.conn, "/tmp/codeg-chat-last-agent")
            .await
            .expect("create chat folder");

        let updated = update_folder_last_agent(
            &db.conn,
            regular_id,
            AgentType::Codex,
        )
        .await
        .expect("update regular folder")
        .expect("regular folder detail");
        assert_eq!(updated.last_agent_type, Some(AgentType::Codex));
        assert_eq!(updated.default_agent_type, None);

        let chat_update = update_folder_last_agent(
            &db.conn,
            chat.id,
            AgentType::Gemini,
        )
        .await
        .expect("ignore chat folder");
        assert!(chat_update.is_none());
        let chat_after = get_folder_by_id(&db.conn, chat.id)
            .await
            .expect("read chat folder")
            .expect("chat folder");
        assert_eq!(chat_after.last_agent_type, None);

        let row = folder::Entity::find_by_id(regular_id)
            .one(&db.conn)
            .await
            .expect("read raw folder")
            .expect("raw folder");
        let mut active = row.into_active_model();
        active.last_agent_type = Set(Some("future_agent".to_string()));
        active.update(&db.conn).await.expect("write invalid value");
        let invalid = get_folder_by_id(&db.conn, regular_id)
            .await
            .expect("read invalid projection")
            .expect("regular folder");
        assert_eq!(invalid.last_agent_type, None);
    }
}
```

- [ ] **Step 5: Run the service test and confirm the new field/API are absent**

```powershell
cd src-tauri
cargo test --features test-utils last_agent_round_trips_only_for_regular_folders
```

Expected: compilation fails because the entity field, projection field, and
`update_folder_last_agent` do not exist.

- [ ] **Step 6: Extend the entity, folder projection, constructors, and service**

Add after `default_agent_type` in `db/entities/folder.rs`:

```rust
pub last_agent_type: Option<String>,
```

Add after `default_agent_type` in `models/folder.rs`:

```rust
pub last_agent_type: Option<AgentType>,
```

Update `to_detail` in `folder_service.rs`:

```rust
fn to_detail(m: folder::Model) -> FolderDetail {
    let default_agent_type = parse_agent_type(&m.default_agent_type);
    let last_agent_type = parse_agent_type(&m.last_agent_type);
    FolderDetail {
        id: m.id,
        name: m.name,
        path: m.path,
        git_branch: m.git_branch,
        default_agent_type,
        last_agent_type,
        last_opened_at: m.last_opened_at,
        sort_order: m.sort_order,
        color: m.color,
        parent_id: m.parent_id,
        kind: m.kind,
    }
}
```

Add `last_agent_type: Set(None)` after `default_agent_type` in both folder
`ActiveModel` literals (`add_folder_inner` and `add_chat_folder`).

Add this service beside `update_folder_default_agent`:

```rust
pub async fn update_folder_last_agent(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
) -> Result<Option<FolderDetail>, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .one(conn)
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let value = serde_json::to_value(agent_type)
        .map_err(|e| DbError::Migration(format!("agent_type serialize failed: {e}")))?;
    let serialized = value
        .as_str()
        .ok_or_else(|| DbError::Migration("agent_type did not serialize as text".to_string()))?
        .to_string();

    let mut active = row.into_active_model();
    active.last_agent_type = Set(Some(serialized));
    active.updated_at = Set(Utc::now());
    let updated = active.update(conn).await?;
    Ok(Some(to_detail(updated)))
}
```

- [ ] **Step 7: Extend the TypeScript model and all complete fixtures**

Add after `default_agent_type` in `src/lib/types.ts`:

```typescript
last_agent_type: AgentType | null
```

Add `last_agent_type: null` after `default_agent_type` in every complete
`FolderDetail` fixture in the files listed for this task. Add the same Rust
field to the two explicit `FolderDetail` literals in `commands/folders.rs` and
`web/event_bridge.rs`:

```rust
last_agent_type: None,
```

- [ ] **Step 8: Format and run focused backend and frontend type checks**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils existing_folders_migrate_without_implicit_recency
cargo test --features test-utils last_agent_round_trips_only_for_regular_folders
cargo fmt --check
```

```powershell
pnpm exec tsc --noEmit
```

Expected: both Rust tests, formatting, and TypeScript checking pass. Invalid
stored Agent text projects as `null`; the explicit default remains unchanged.

- [ ] **Step 9: Commit the persisted folder projection**

```powershell
git add src-tauri/src/db/migration/m20260716_000002_folder_last_agent.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/folder.rs src-tauri/src/db/service/folder_service.rs src-tauri/src/models/folder.rs src-tauri/src/commands/folders.rs src-tauri/src/web/event_bridge.rs src/lib/types.ts src/contexts/app-workspace-context.test.tsx src/stores/folder-derivation-decoupling.test.ts src/lib/branch-switch.test.ts src/contexts/tab-context.test.tsx src/components/chat/conversation-context-bar.test.tsx src/components/conversations/sidebar-conversation-list.test.tsx
git commit -m "feat(folders): persist recent conversation agent"
```

---

### Task 2: Record Recency Only in the Normal Project Create Flow

**Files:**
- Modify: `src-tauri/src/commands/conversations.rs:931,1021,1043`
- Modify: `src-tauri/src/web/handlers/conversations.rs:181`

**Interfaces:**
- Preserves: `create_conversation_core(&DatabaseConnection, i32, AgentType, Option<String>) -> Result<i32, AppCommandError>` as the generic non-recording primitive.
- Produces: `ProjectConversationCreateResult { conversation_id: i32, updated_folder: Option<FolderDetail> }`.
- Produces: `create_project_conversation_core(&DatabaseConnection, i32, AgentType, Option<String>) -> Result<ProjectConversationCreateResult, AppCommandError>`.
- Produces: `emit_project_conversation_created(&EventEmitter, &DatabaseConnection, &ProjectConversationCreateResult) -> Future<Output = ()>`.

- [ ] **Step 1: Add failing tests for success, ordering, exclusions, and warning-only failure**

Add these tests inside the existing `commands/conversations.rs` test module:

```rust
#[tokio::test]
async fn project_create_records_only_after_insert_and_last_write_wins() {
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-project-agent").await;

    let first = create_project_conversation_core(
        &db.conn,
        folder_id,
        AgentType::ClaudeCode,
        Some("first".to_string()),
    )
    .await
    .expect("first project create");
    assert!(first.conversation_id > 0);
    assert_eq!(
        first
            .updated_folder
            .as_ref()
            .and_then(|folder| folder.last_agent_type),
        Some(AgentType::ClaudeCode)
    );

    let second = create_project_conversation_core(
        &db.conn,
        folder_id,
        AgentType::Codex,
        Some("second".to_string()),
    )
    .await
    .expect("second project create");
    assert!(second.conversation_id > first.conversation_id);
    let folder = folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .expect("read folder")
        .expect("folder");
    assert_eq!(folder.last_agent_type, Some(AgentType::Codex));
    assert_eq!(folder.default_agent_type, None);
}

#[tokio::test]
async fn project_create_succeeds_when_recency_write_fails() {
    use sea_orm::ConnectionTrait;

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-project-agent-write-fail").await;
    db.conn
        .execute_unprepared(
            "CREATE TRIGGER reject_last_agent_update \
             BEFORE UPDATE OF last_agent_type ON folder \
             BEGIN SELECT RAISE(FAIL, 'forced recency failure'); END",
        )
        .await
        .expect("install update trigger");

    let created = create_project_conversation_core(
        &db.conn,
        folder_id,
        AgentType::Codex,
        None,
    )
    .await
    .expect("conversation creation must remain successful");

    assert!(created.conversation_id > 0);
    assert!(created.updated_folder.is_none());
    let summary = conversation_service::get_by_id(&db.conn, created.conversation_id)
        .await
        .expect("conversation was inserted");
    assert_eq!(summary.agent_type, AgentType::Codex);
}

#[tokio::test]
async fn failed_project_insert_does_not_change_recency() {
    use sea_orm::ConnectionTrait;

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-project-agent-insert-fail").await;
    folder_service::update_folder_last_agent(
        &db.conn,
        folder_id,
        AgentType::ClaudeCode,
    )
    .await
    .expect("seed recency");
    db.conn
        .execute_unprepared(
            "CREATE TRIGGER reject_conversation_insert \
             BEFORE INSERT ON conversation \
             BEGIN SELECT RAISE(FAIL, 'forced insert failure'); END",
        )
        .await
        .expect("install insert trigger");

    let result = create_project_conversation_core(
        &db.conn,
        folder_id,
        AgentType::Gemini,
        None,
    )
    .await;
    assert!(result.is_err());

    let folder = folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .expect("read folder")
        .expect("folder");
    assert_eq!(folder.last_agent_type, Some(AgentType::ClaudeCode));
}

#[tokio::test]
async fn generic_create_does_not_record_project_recency() {
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-non-project-create").await;
    folder_service::update_folder_last_agent(
        &db.conn,
        folder_id,
        AgentType::ClaudeCode,
    )
    .await
    .expect("seed recency");

    create_conversation_core(&db.conn, folder_id, AgentType::Gemini, None)
        .await
        .expect("generic create");

    let folder = folder_service::get_folder_by_id(&db.conn, folder_id)
        .await
        .expect("read folder")
        .expect("folder");
    assert_eq!(folder.last_agent_type, Some(AgentType::ClaudeCode));
}

#[tokio::test]
async fn project_create_emits_conversation_and_fresh_folder_upserts() {
    use crate::web::event_bridge::{
        WebEventBroadcaster, CONVERSATION_CHANGED_EVENT, FOLDER_CHANGED_EVENT,
    };
    use std::sync::Arc;

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-project-create-events").await;
    let broadcaster = Arc::new(WebEventBroadcaster::new());
    let emitter = EventEmitter::test_web_only(broadcaster.clone());
    let mut rx = broadcaster.subscribe();
    let created = create_project_conversation_core(
        &db.conn,
        folder_id,
        AgentType::Codex,
        None,
    )
    .await
    .expect("project create");

    emit_project_conversation_created(&emitter, &db.conn, &created).await;

    let events = [
        rx.try_recv().expect("first upsert"),
        rx.try_recv().expect("second upsert"),
    ];
    assert!(events
        .iter()
        .any(|event| event.channel == CONVERSATION_CHANGED_EVENT));
    let folder_event = events
        .iter()
        .find(|event| event.channel == FOLDER_CHANGED_EVENT)
        .expect("folder upsert");
    assert_eq!(folder_event.payload["kind"], "upsert");
    assert_eq!(folder_event.payload["folder"]["id"], folder_id);
    assert_eq!(folder_event.payload["folder"]["last_agent_type"], "codex");
}
```

- [ ] **Step 2: Run the focused tests and confirm the project core is missing**

```powershell
cd src-tauri
cargo test --features test-utils project_create_records_only_after_insert_and_last_write_wins
cargo test --features test-utils project_create_succeeds_when_recency_write_fails
cargo test --features test-utils failed_project_insert_does_not_change_recency
cargo test --features test-utils generic_create_does_not_record_project_recency
cargo test --features test-utils project_create_emits_conversation_and_fresh_folder_upserts
```

Expected: compilation fails because `ProjectConversationCreateResult`,
`create_project_conversation_core`, and the shared emitter do not exist.

- [ ] **Step 3: Add the normal-project create core after the generic primitive**

Keep `create_conversation_core` unchanged. Add immediately after it:

```rust
#[derive(Debug, Clone)]
pub struct ProjectConversationCreateResult {
    pub conversation_id: i32,
    pub updated_folder: Option<FolderDetail>,
}

pub async fn create_project_conversation_core(
    conn: &sea_orm::DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
) -> Result<ProjectConversationCreateResult, AppCommandError> {
    let conversation_id =
        create_conversation_core(conn, folder_id, agent_type, title).await?;
    let updated_folder = match folder_service::update_folder_last_agent(
        conn,
        folder_id,
        agent_type,
    )
    .await
    {
        Ok(folder) => folder,
        Err(error) => {
            tracing::warn!(
                "[conversations] created {conversation_id}, but failed to update \
                 folder {folder_id} recent agent: {error}"
            );
            None
        }
    };

    Ok(ProjectConversationCreateResult {
        conversation_id,
        updated_folder,
    })
}
```

This ordering is mandatory: do not write recency before
`create_conversation_core` returns successfully.

- [ ] **Step 4: Add the shared post-create emitter**

Add beside `emit_conversation_upsert`:

```rust
pub(crate) async fn emit_project_conversation_created(
    emitter: &EventEmitter,
    conn: &sea_orm::DatabaseConnection,
    created: &ProjectConversationCreateResult,
) {
    emit_conversation_upsert(emitter, conn, created.conversation_id).await;
    if let Some(folder) = created.updated_folder.clone() {
        crate::commands::folders::emit_folder_upsert(emitter, folder);
    }
}
```

The helper intentionally emits no folder event when the optional recency write
failed; reconnect/folder refresh remains the recovery path.

- [ ] **Step 5: Route the Tauri and Axum normal-create wrappers through the project core**

Replace the Tauri wrapper body with:

```rust
let created =
    create_project_conversation_core(&db.conn, folder_id, agent_type, title).await?;
emit_project_conversation_created(&EventEmitter::Tauri(app), &db.conn, &created).await;
Ok(created.conversation_id)
```

Replace the Axum handler body with:

```rust
let db = &state.db;
let created = conv_commands::create_project_conversation_core(
    &db.conn,
    params.folder_id,
    params.agent_type,
    params.title,
)
.await?;
conv_commands::emit_project_conversation_created(
    &state.emitter,
    &db.conn,
    &created,
)
.await;
Ok(Json(created.conversation_id))
```

Do not change the automation engine's import or call: it must continue using
`create_conversation_core`. Chat mode continues using
`create_chat_conversation_core`; delegation continues using
`conversation_service::create_with_delegation`; import paths continue using
their import services.

- [ ] **Step 6: Run focused tests and audit excluded production call sites**

```powershell
cd src-tauri
cargo fmt
cargo test --features test-utils project_create_records_only_after_insert_and_last_write_wins
cargo test --features test-utils project_create_succeeds_when_recency_write_fails
cargo test --features test-utils failed_project_insert_does_not_change_recency
cargo test --features test-utils generic_create_does_not_record_project_recency
cargo test --features test-utils project_create_emits_conversation_and_fresh_folder_upserts
cargo fmt --check
```

Run from the repository root:

```powershell
rg -n "create_project_conversation_core" src-tauri/src/automation src-tauri/src/acp/delegation src-tauri/src/db/service/import_service.rs src-tauri/src/pets/codex_import.rs src-tauri/src/commands/delegation.rs
```

Expected: all tests and formatting pass. The audit command reports no matches,
confirming automation, delegation, and import code cannot update recency through
the project-only core.

- [ ] **Step 7: Compile both runtime wrappers**

```powershell
cd src-tauri
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: desktop and server modes compile against the same shared project core
and post-create emitter.

- [ ] **Step 8: Commit the recording and broadcast path**

```powershell
git add src-tauri/src/commands/conversations.rs src-tauri/src/web/handlers/conversations.rs
git commit -m "feat(conversations): remember project recent agent"
```

---

### Task 3: Resolve New Drafts with Project Recency

**Files:**
- Create: `src/lib/resolve-default-agent.test.ts`
- Modify: `src/lib/resolve-default-agent.ts`
- Modify: `src/stores/tab-store.ts:149,393,704,1277`
- Modify: `src/contexts/tab-context.tsx:220`
- Modify: `src/contexts/tab-context.test.tsx`
- Modify: `src/hooks/use-switch-to-branch.ts:121,138,159`
- Modify: `src/contexts/app-workspace-context.test.tsx:342`

**Interfaces:**
- Extends: `ResolveDefaultAgentInput.folderRecent: AgentType | null`.
- Extends: `openNewConversationTab(..., options).folderRecentAgent?: AgentType | null` for just-opened folder snapshots.
- Preserves: `ResolveDefaultAgentResult { agentType: AgentType; provisional: boolean }`.

- [ ] **Step 1: Add pure failing priority and hydration tests**

Create `src/lib/resolve-default-agent.test.ts`:

```typescript
import { describe, expect, it } from "vitest"
import { resolveDefaultAgent, type ResolveDefaultAgentInput } from "./resolve-default-agent"

const base: ResolveDefaultAgentInput = {
  folderDefault: null,
  inherit: null,
  folderRecent: null,
  sortedTypes: ["codex", "gemini"],
  fresh: true,
}

describe("resolveDefaultAgent project recency", () => {
  it("keeps the explicit folder default highest", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderDefault: "claude_code",
        inherit: "open_code",
        folderRecent: "gemini",
      })
    ).toEqual({ agentType: "claude_code", provisional: false })
  })

  it("keeps explicitly requested conversation inheritance above recency", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        inherit: "open_code",
        folderRecent: "gemini",
      })
    ).toEqual({ agentType: "open_code", provisional: false })
  })

  it("uses an available recent agent after fresh hydration", () => {
    expect(
      resolveDefaultAgent({ ...base, folderRecent: "gemini" })
    ).toEqual({ agentType: "gemini", provisional: false })
  })

  it("returns recent recency provisionally before hydration", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderRecent: "gemini",
        sortedTypes: [],
        fresh: false,
      })
    ).toEqual({ agentType: "gemini", provisional: true })
  })

  it("corrects unavailable recency to the first fresh sorted agent", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        folderRecent: "gemini",
        sortedTypes: ["codex"],
      })
    ).toEqual({ agentType: "codex", provisional: false })
  })

  it("uses saved Agent order when the folder has no recency", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        sortedTypes: ["open_code", "codex"],
      })
    ).toEqual({ agentType: "open_code", provisional: false })
  })

  it("keeps the existing hard fallback provisional on a cold empty list", () => {
    expect(
      resolveDefaultAgent({
        ...base,
        sortedTypes: [],
        fresh: false,
      })
    ).toEqual({ agentType: "codex", provisional: true })
  })
})
```

- [ ] **Step 2: Run the resolver tests and confirm the input is missing**

```powershell
pnpm exec vitest run src/lib/resolve-default-agent.test.ts
```

Expected: TypeScript compilation fails because `folderRecent` is not part of
`ResolveDefaultAgentInput`.

- [ ] **Step 3: Implement the exact five-level resolver**

Add this field after `inherit`:

```typescript
/** Agent used by the latest normally-created root conversation in this folder. */
folderRecent: AgentType | null
```

Replace the resolver body with:

```typescript
const { folderDefault, inherit, folderRecent, sortedTypes, fresh } = input
if (folderDefault) {
  return { agentType: folderDefault, provisional: false }
}
if (inherit) {
  return { agentType: inherit, provisional: false }
}
if (folderRecent) {
  if (!fresh) {
    return { agentType: folderRecent, provisional: true }
  }
  if (sortedTypes.includes(folderRecent)) {
    return { agentType: folderRecent, provisional: false }
  }
}
if (sortedTypes.length > 0) {
  return { agentType: sortedTypes[0], provisional: !fresh }
}
return { agentType: AGENT_DISPLAY_ORDER[0], provisional: !fresh }
```

Update the function comment to list `folderRecent` between `inherit` and
`sortedTypes[0]`, and explain that fresh hydration validates it rather than
deleting it.

- [ ] **Step 4: Feed folder recency through the tab-store boundary**

Replace `resolveAgentForFolder` with:

```typescript
function resolveAgentForFolder(
  folderId: number,
  inherit: AgentType | null,
  folderDefaultOverride?: AgentType | null,
  folderRecentOverride?: AgentType | null
): { agentType: AgentType; provisional: boolean } {
  const folder = useAppWorkspaceStore
    .getState()
    .folders.find((item) => item.id === folderId)
  const folderDefault =
    folderDefaultOverride !== undefined
      ? folderDefaultOverride
      : (folder?.default_agent_type ?? null)
  const folderRecent =
    folderRecentOverride !== undefined
      ? folderRecentOverride
      : (folder?.last_agent_type ?? null)
  return resolveDefaultAgent({
    folderDefault,
    inherit,
    folderRecent,
    sortedTypes: runtime.sortedAvailableAgents,
    fresh: runtime.agentsFresh,
  })
}
```

Extend `openNewConversationTab`'s option declaration in `tab-store.ts`:

```typescript
folderRecentAgent?: AgentType | null
```

Use the existing indexed Agent type in `tab-context.tsx`:

```typescript
folderRecentAgent?: TabItem["agentType"] | null
```

Use the fourth argument in `openNewConversationTab`:

```typescript
const { agentType: targetAgent, provisional } = resolveAgentForFolder(
  folderId,
  inherit,
  options?.folderDefaultAgent,
  options?.folderRecentAgent
)
```

Do not add recency to `openChatModeTab`: it passes folder id `0` and explicit
inheritance remains its existing behavior.

- [ ] **Step 5: Preserve complete just-opened folder snapshots in branch navigation**

At all three `use-switch-to-branch.ts` calls that already pass
`folderDefaultAgent`, add the corresponding recent value:

```typescript
folderRecentAgent: target.last_agent_type,
```

```typescript
folderRecentAgent: detail.last_agent_type,
```

```typescript
folderRecentAgent: root.last_agent_type,
```

Inheritance remains stronger because these branch actions explicitly set
`inheritFromActive: true`; the recent value is used only when no confirmed
active-conversation Agent can be inherited.

- [ ] **Step 6: Add tab-store integration and cross-client folder-event tests**

Replace the fixed Agent hook mock in `tab-context.test.tsx` with mutable hoisted
state:

```tsx
const agentRegistryMock = vi.hoisted(() => ({
  sortedTypes: ["codex"] as AgentType[],
  fresh: true,
}))

vi.mock("@/hooks/use-sorted-available-agents", () => ({
  useSortedAvailableAgents: () => ({
    sortedTypes: agentRegistryMock.sortedTypes,
    fresh: agentRegistryMock.fresh,
  }),
}))

beforeEach(() => {
  agentRegistryMock.sortedTypes = ["codex"]
  agentRegistryMock.fresh = true
})
```

Add under `TabProvider tab state transitions`:

```tsx
it("uses the folder recent agent for a normal new draft", () => {
  agentRegistryMock.sortedTypes = ["codex", "gemini"]
  const recentFolder: FolderDetail = {
    ...defaultFoldersMock[0],
    default_agent_type: null,
    last_agent_type: "gemini",
  }
  useAppWorkspaceStore.setState({
    folders: [recentFolder],
    allFolders: [recentFolder],
  })
  renderTabs()

  act(() => {
    latestContext?.openNewConversationTab(recentFolder.id, recentFolder.path)
  })

  const draft = latestContext?.tabs.find(
    (tab) => tab.id === latestContext?.activeTabId
  )
  expect(draft?.conversationId).toBeNull()
  expect(draft?.agentType).toBe("gemini")
  expect(draft?.agentTypeProvisional).toBe(false)
})
```

Add under `AppWorkspaceProvider folder://changed sync` in
`app-workspace-context.test.tsx`:

```tsx
it("updates recent agent in both folder lists from a folder upsert", async () => {
  await mountProvider()
  emitFolder({
    kind: "upsert",
    folder: makeFolder({ id: 12, last_agent_type: "gemini" }),
  })

  const state = useAppWorkspaceStore.getState()
  expect(state.folders.find((folder) => folder.id === 12)?.last_agent_type).toBe(
    "gemini"
  )
  expect(
    state.allFolders.find((folder) => folder.id === 12)?.last_agent_type
  ).toBe("gemini")
})
```

- [ ] **Step 7: Run focused resolver, tab, workspace, and branch tests**

```powershell
pnpm exec vitest run src/lib/resolve-default-agent.test.ts src/contexts/tab-context.test.tsx src/contexts/app-workspace-context.test.tsx src/lib/branch-switch.test.ts
pnpm eslint src/lib/resolve-default-agent.ts src/lib/resolve-default-agent.test.ts src/stores/tab-store.ts src/contexts/tab-context.tsx src/contexts/tab-context.test.tsx src/hooks/use-switch-to-branch.ts src/contexts/app-workspace-context.test.tsx
```

Expected: all tests and lint pass. The pure tests prove cold recency is
provisional and fresh unavailable recency corrects to `sortedTypes[0]`.

- [ ] **Step 8: Commit frontend resolution and convergence**

```powershell
git add src/lib/resolve-default-agent.ts src/lib/resolve-default-agent.test.ts src/stores/tab-store.ts src/contexts/tab-context.tsx src/contexts/tab-context.test.tsx src/hooks/use-switch-to-branch.ts src/contexts/app-workspace-context.test.tsx
git commit -m "feat(chat): recall each project's recent agent"
```

---

### Task 4: Verify the Independent Feature Across Both Runtimes

**Files:**
- No new files.
- Verify all files changed in Tasks 1-3.

**Interfaces:**
- Verifies the persisted Rust/TypeScript contract, normal-create boundary, event convergence, and draft resolver as one independently shippable feature.

- [ ] **Step 1: Run the complete frontend checks**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: lint, the complete Vitest suite, and static export build pass.

- [ ] **Step 2: Run desktop Rust tests and strict linting**

```powershell
cd src-tauri
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: all desktop tests and Clippy pass.

- [ ] **Step 3: Run server and MCP companion checks**

```powershell
cd src-tauri
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: server tests, server Clippy, MCP check, and MCP Clippy all pass.

- [ ] **Step 4: Inspect the final scoped diff and commit any verification-only fixes**

```powershell
git diff --check
git status --short
```

Expected: no whitespace errors. Only files named in this plan are staged for
this feature; unrelated working-tree changes remain untouched.
