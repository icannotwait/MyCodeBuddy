use chrono::Utc;
use sea_orm::DatabaseConnection;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, Set, Statement,
};

use crate::db::entities::folder;
use crate::db::entities::folder::FolderKind;
use crate::db::error::DbError;
use crate::models::agent::AgentType;
use crate::models::{FolderDetail, FolderHistoryEntry};

/// Theme color sentinel stored in the DB. The frontend leaves the folder group
/// unscoped so it inherits the app-wide appearance theme color.
pub const DEFAULT_FOLDER_COLOR: &str = "inherit";

fn to_entry(m: folder::Model) -> FolderHistoryEntry {
    FolderHistoryEntry {
        id: m.id,
        path: m.path,
        name: m.name,
        last_opened_at: m.last_opened_at,
    }
}

fn parse_agent_type(s: &Option<String>) -> Option<AgentType> {
    s.as_deref()
        .and_then(|v| serde_json::from_value(serde_json::Value::String(v.to_string())).ok())
}

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
        alias: m.alias,
    }
}

pub async fn get_folder_by_id(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<Option<FolderDetail>, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;

    Ok(row.map(to_detail))
}

/// How [`add_folder_inner`] writes the `parent_id` column. The two callers want
/// different semantics on reopen of an existing path, which a bare `Option<i32>`
/// could not express (it conflates "no parent" with "don't touch the parent").
enum ParentWrite {
    /// Plain open: leave an existing row's `parent_id` untouched (insert NULL).
    /// A plain reopen must never clear a worktree's recorded root.
    Preserve,
    /// Worktree open: write this exact value on both insert and reopen — so the
    /// stored relationship always reflects the latest call (including `None` to
    /// demote to a top-level folder) and can never go stale.
    Set(Option<i32>),
}

pub async fn add_folder(
    conn: &DatabaseConnection,
    path: &str,
) -> Result<FolderHistoryEntry, DbError> {
    add_folder_inner(conn, path, ParentWrite::Preserve).await
}

/// Like [`add_folder`] but authoritatively sets `parent_id` — the *root* folder
/// this path was created under (used by the worktree flow so a worktree folder
/// remembers its originating repo folder). The value is written on both insert
/// and reopen, so it always reflects the latest worktree relationship and never
/// a stale one.
pub async fn add_folder_with_parent(
    conn: &DatabaseConnection,
    path: &str,
    parent_id: Option<i32>,
) -> Result<FolderHistoryEntry, DbError> {
    add_folder_inner(conn, path, ParentWrite::Set(parent_id)).await
}

fn is_unique_path_violation(err: &sea_orm::DbErr) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("unique constraint failed")
        || msg.contains("unique constraint")
        || msg.contains("2067") // SQLITE_CONSTRAINT_UNIQUE
        || msg.contains("1555") // SQLITE_CONSTRAINT_PRIMARYKEY
}

/// Test-only: next N `add_folder` calls skip a successful existing-row find so
/// concurrent callers both reach INSERT and exercise UNIQUE recovery.
#[cfg(test)]
static FORCE_ADD_FOLDER_SKIP_EXISTING: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn take_force_skip_existing() -> bool {
    use std::sync::atomic::Ordering;
    FORCE_ADD_FOLDER_SKIP_EXISTING
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            if n > 0 {
                Some(n - 1)
            } else {
                None
            }
        })
        .is_ok()
}

/// Arm the skip-existing race for tests (see `FORCE_ADD_FOLDER_SKIP_EXISTING`).
#[cfg(test)]
pub fn force_add_folder_skip_existing_for_test(n: usize) {
    FORCE_ADD_FOLDER_SKIP_EXISTING.store(n, std::sync::atomic::Ordering::SeqCst);
}

async fn reopen_folder_row(
    conn: &DatabaseConnection,
    row: folder::Model,
    name: String,
    now: chrono::DateTime<Utc>,
    parent: ParentWrite,
) -> Result<folder::Model, DbError> {
    let mut active = row.into_active_model();
    active.name = Set(name);
    active.last_opened_at = Set(now);
    active.updated_at = Set(now);
    active.deleted_at = Set(None);
    active.is_open = Set(true);
    // Plain reopen leaves the relationship as-is; the worktree flow writes
    // the authoritative value (including NULL) so it can never go stale.
    if let ParentWrite::Set(parent_id) = parent {
        active.parent_id = Set(parent_id);
    }
    Ok(active.update(conn).await?)
}

async fn add_folder_inner(
    conn: &DatabaseConnection,
    path: &str,
    parent: ParentWrite,
) -> Result<FolderHistoryEntry, DbError> {
    let now = Utc::now();
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());

    let existing = folder::Entity::find()
        .filter(folder::Column::Path.eq(path))
        .one(conn)
        .await?;

    #[cfg(test)]
    let existing = if take_force_skip_existing() {
        None
    } else {
        existing
    };

    let model = if let Some(row) = existing {
        reopen_folder_row(conn, row, name, now, parent).await?
    } else {
        let max_order = folder::Entity::find()
            .order_by_desc(folder::Column::SortOrder)
            .one(conn)
            .await?
            .map(|m| m.sort_order)
            .unwrap_or(0);
        let active = folder::ActiveModel {
            id: NotSet,
            name: Set(name.clone()),
            path: Set(path.to_string()),
            git_branch: Set(None),
            default_agent_type: Set(None),
            last_agent_type: Set(None),
            last_opened_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            is_open: Set(true),
            sort_order: Set(max_order + 1),
            color: Set(DEFAULT_FOLDER_COLOR.to_string()),
            parent_id: Set(match parent {
                ParentWrite::Preserve => None,
                ParentWrite::Set(parent_id) => parent_id,
            }),
            kind: Set(FolderKind::Regular),
            alias: Set(None),
        };
        match active.insert(conn).await {
            Ok(model) => model,
            // Concurrent open of the same path: loser lost the UNIQUE race —
            // reopen the winner instead of surfacing spawn_failed to callers.
            Err(e) if is_unique_path_violation(&e) => {
                let winner = folder::Entity::find()
                    .filter(folder::Column::Path.eq(path))
                    .one(conn)
                    .await?
                    .ok_or_else(|| DbError::from(e))?;
                reopen_folder_row(conn, winner, name, now, parent).await?
            }
            Err(e) => return Err(e.into()),
        }
    };

    Ok(to_entry(model))
}

/// Create a dedicated hidden folder backing a single chat-mode conversation.
///
/// Unlike [`add_folder`], the display name is a fixed sentinel ("Chat") rather
/// than derived from the path, and `kind = chat` is set so the frontend routes
/// this folder's conversations to the sidebar "Chat" group and hides
/// folder-bound chrome. `path` is a freshly generated per-conversation scratch dir, so it
/// never collides on the `UNIQUE(path)` constraint. Returns the full
/// [`FolderDetail`] so the caller can hand it straight to the frontend.
pub async fn add_chat_folder(
    conn: &DatabaseConnection,
    path: &str,
) -> Result<FolderDetail, DbError> {
    let now = Utc::now();
    let max_order = folder::Entity::find()
        .order_by_desc(folder::Column::SortOrder)
        .one(conn)
        .await?
        .map(|m| m.sort_order)
        .unwrap_or(0);
    let active = folder::ActiveModel {
        id: NotSet,
        name: Set("Chat".to_string()),
        path: Set(path.to_string()),
        git_branch: Set(None),
        default_agent_type: Set(None),
        last_agent_type: Set(None),
        last_opened_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        is_open: Set(true),
        sort_order: Set(max_order + 1),
        color: Set(DEFAULT_FOLDER_COLOR.to_string()),
        parent_id: Set(None),
        kind: Set(FolderKind::Chat),
        alias: Set(None),
    };
    let model = active.insert(conn).await?;
    Ok(to_detail(model))
}

pub async fn update_folder_color(
    conn: &DatabaseConnection,
    folder_id: i32,
    color: &str,
) -> Result<Option<FolderDetail>, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let mut active = row.into_active_model();
    active.color = Set(color.to_string());
    active.updated_at = Set(Utc::now());
    let updated = active.update(conn).await?;
    Ok(Some(to_detail(updated)))
}

/// Sets (or clears) a folder's display alias. `alias = None` clears it; callers
/// are expected to have already normalized empty/whitespace input to `None`.
pub async fn update_folder_alias(
    conn: &DatabaseConnection,
    folder_id: i32,
    alias: Option<String>,
) -> Result<Option<FolderDetail>, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let mut active = row.into_active_model();
    active.alias = Set(alias);
    active.updated_at = Set(Utc::now());
    let updated = active.update(conn).await?;
    Ok(Some(to_detail(updated)))
}

pub async fn update_folder_default_agent(
    conn: &DatabaseConnection,
    folder_id: i32,
    default_agent_type: Option<AgentType>,
) -> Result<Option<FolderDetail>, DbError> {
    let row = folder::Entity::find_by_id(folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    // Serialize AgentType to its snake_case wire form (e.g. "claude_code").
    // Mirrors `parse_agent_type`'s round-trip through serde_json.
    let serialized = default_agent_type
        .map(|t| serde_json::to_value(t).ok())
        .and_then(|v| v.and_then(|val| val.as_str().map(|s| s.to_string())));

    let mut active = row.into_active_model();
    active.default_agent_type = Set(serialized);
    active.updated_at = Set(Utc::now());
    let updated = active.update(conn).await?;
    Ok(Some(to_detail(updated)))
}

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

pub async fn list_folders(conn: &DatabaseConnection) -> Result<Vec<FolderHistoryEntry>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        // Only regular folders are user-facing in folder history / open-folder
        // pickers — hidden chat folders (and future engine-created kinds) are an
        // implementation detail.
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .order_by_desc(folder::Column::LastOpenedAt)
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(to_entry).collect())
}

pub async fn remove_folder(conn: &DatabaseConnection, path: &str) -> Result<(), DbError> {
    let now = Utc::now();
    let row = folder::Entity::find()
        .filter(folder::Column::Path.eq(path))
        .filter(folder::Column::DeletedAt.is_null())
        .one(conn)
        .await?;

    if let Some(row) = row {
        let mut active = row.into_active_model();
        active.deleted_at = Set(Some(now));
        active.updated_at = Set(now);
        active.update(conn).await?;
    }
    Ok(())
}

pub async fn set_folder_open(
    conn: &DatabaseConnection,
    folder_id: i32,
    is_open: bool,
) -> Result<(), DbError> {
    let row = folder::Entity::find_by_id(folder_id).one(conn).await?;

    if let Some(row) = row {
        let mut active = row.into_active_model();
        active.is_open = Set(is_open);
        active.updated_at = Set(Utc::now());
        active.update(conn).await?;
    }
    Ok(())
}

/// Count live conversations for a folder (`deleted_at IS NULL`).
///
/// Diagnostic / import decision aid only — **never** use this count alone as
/// the auto-close guard (TOCTOU). Close uses
/// [`close_folder_if_no_live_conversations`]'s atomic `NOT EXISTS` UPDATE.
pub async fn count_live_conversations_for_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<u64, DbError> {
    use crate::db::entities::conversation;
    use sea_orm::PaginatorTrait;

    let n = conversation::Entity::find()
        .filter(conversation::Column::FolderId.eq(folder_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .count(conn)
        .await?;
    Ok(n)
}

/// Visibility-only auto-close for one folder when it is still open, regular,
/// not soft-deleted, and has zero live conversations.
///
/// Atomic: a single conditional `UPDATE` with `NOT EXISTS` live conversations.
/// Returns `true` only when this statement flipped `is_open` true→false.
/// No-op (`false`) for missing, chat kind, already closed, soft-deleted, or
/// non-empty folders — never touches `deleted_at`.
pub async fn close_folder_if_no_live_conversations(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;

    let now = Utc::now();
    let result = folder::Entity::update_many()
        .col_expr(folder::Column::IsOpen, Expr::value(false))
        .col_expr(folder::Column::UpdatedAt, Expr::value(now))
        .filter(folder::Column::Id.eq(folder_id))
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .filter(folder::Column::IsOpen.eq(true))
        .filter(Expr::cust(
            "NOT EXISTS (SELECT 1 FROM conversation c \
             WHERE c.folder_id = folder.id AND c.deleted_at IS NULL)",
        ))
        .exec(conn)
        .await?;

    Ok(result.rows_affected == 1)
}

/// Bulk reconcile: close every open regular folder that currently has zero live
/// conversations. Returns the ids that were closed (caller derives count).
///
/// Each close uses the same atomic [`close_folder_if_no_live_conversations`]
/// primitive (`WHERE NOT EXISTS` live), not a pre-count then set.
pub async fn close_open_folders_with_no_live_conversations(
    conn: &DatabaseConnection,
) -> Result<Vec<i32>, DbError> {
    let candidates = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::IsOpen.eq(true))
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .all(conn)
        .await?;

    let mut closed = Vec::new();
    for row in candidates {
        if close_folder_if_no_live_conversations(conn, row.id).await? {
            closed.push(row.id);
        }
    }
    Ok(closed)
}

pub async fn list_open_folders(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderHistoryEntry>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::IsOpen.eq(true))
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .order_by_desc(folder::Column::LastOpenedAt)
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(to_entry).collect())
}

pub async fn list_open_folder_details(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderDetail>, DbError> {
    // Excludes hidden chat folders from the workspace "open folders" surface.
    // `list_all_folder_details` (below) intentionally keeps them so the frontend
    // can still resolve an active chat conversation's cwd / active folder by id.
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::IsOpen.eq(true))
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .order_by_asc(folder::Column::SortOrder)
        .order_by_desc(folder::Column::LastOpenedAt)
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(to_detail).collect())
}

pub async fn list_all_folder_details(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderDetail>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .order_by_asc(folder::Column::SortOrder)
        .order_by_desc(folder::Column::LastOpenedAt)
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(to_detail).collect())
}

/// Paths of all *live* (non-deleted) chat scratch folders. Consumed by the
/// startup orphan-scratch-dir GC to spare directories still bound to a chat
/// conversation, while reclaiming pre-send drafts (no row at all) and
/// post-delete dirs (soft-deleted row → `DeletedAt` set → excluded here).
pub async fn list_live_chat_folder_paths(
    conn: &DatabaseConnection,
) -> Result<Vec<String>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::Kind.eq(FolderKind::Chat))
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(|m| m.path).collect())
}

pub async fn reorder_folders(conn: &DatabaseConnection, ids: Vec<i32>) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }

    let now = Utc::now();
    let now_str = now.format("%Y-%m-%d %H:%M:%S %:z").to_string();
    let case_expr = ids
        .iter()
        .enumerate()
        .map(|(idx, id)| format!("WHEN {} THEN {}", id, idx + 1))
        .collect::<Vec<_>>()
        .join(" ");
    let id_list = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    let sql = format!(
        "UPDATE folder SET sort_order = CASE id {case_expr} END, updated_at = '{now_str}' WHERE id IN ({id_list})"
    );
    conn.execute(Statement::from_string(DbBackend::Sqlite, sql))
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        add_chat_folder, add_folder, close_folder_if_no_live_conversations,
        close_open_folders_with_no_live_conversations, force_add_folder_skip_existing_for_test,
        get_folder_by_id, list_open_folders, update_folder_last_agent,
    };
    use crate::db::entities::folder;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn close_open_folders_with_no_live_conversations_closes_empty_regular() {
        let db = fresh_in_memory_db().await;
        let empty_id = seed_folder(&db, "/tmp/codeg-empty-open").await;
        let kept_id = seed_folder(&db, "/tmp/codeg-kept-open").await;
        seed_conversation(&db, kept_id, AgentType::ClaudeCode).await;

        let closed = close_open_folders_with_no_live_conversations(&db.conn)
            .await
            .expect("reconcile");
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0], empty_id);

        let open = list_open_folders(&db.conn).await.expect("list");
        let open_ids: Vec<i32> = open.iter().map(|f| f.id).collect();
        assert!(!open_ids.contains(&empty_id));
        assert!(open_ids.contains(&kept_id));
    }

    #[tokio::test]
    async fn close_open_folders_skips_chat_kind() {
        let db = fresh_in_memory_db().await;
        let chat = add_chat_folder(&db.conn, "/tmp/codeg-chat-scratch/x")
            .await
            .expect("chat folder");
        // ensure is_open true (add_chat_folder already opens)
        let closed = close_open_folders_with_no_live_conversations(&db.conn)
            .await
            .expect("reconcile");
        assert!(closed.is_empty());
        let still = get_folder_by_id(&db.conn, chat.id)
            .await
            .expect("get")
            .expect("exists");
        // chat still exists and was not soft-deleted; open flag may remain true
        // (list_open_folder_details excludes chat regardless)
        let _ = still;
    }

    #[tokio::test]
    async fn close_folder_if_no_live_conversations_is_idempotent() {
        let db = fresh_in_memory_db().await;
        let id = seed_folder(&db, "/tmp/codeg-once").await;
        assert!(close_folder_if_no_live_conversations(&db.conn, id)
            .await
            .unwrap());
        assert!(!close_folder_if_no_live_conversations(&db.conn, id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn concurrent_add_folder_same_path_converges_without_unique_error() {
        let db = Arc::new(fresh_in_memory_db().await);
        let path = "/tmp/codeg-folder-unique-race";
        // Force both callers past find → INSERT so one hits UNIQUE recovery.
        force_add_folder_skip_existing_for_test(2);
        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let db1 = db.clone();
        let t1 = tokio::spawn(async move {
            b1.wait().await;
            add_folder(&db1.conn, path).await
        });
        let b2 = barrier.clone();
        let db2 = db.clone();
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            add_folder(&db2.conn, path).await
        });
        let (a, b) = tokio::join!(t1, t2);
        let a = a.expect("join a").expect("add a");
        let b = b.expect("join b").expect("add b");
        assert_eq!(a.id, b.id, "same path must converge to one folder row");
        assert_eq!(a.path, path);
        let rows = folder::Entity::find()
            .filter(folder::Column::Path.eq(path))
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1, "exactly one row for path");
        force_add_folder_skip_existing_for_test(0);
    }

    /// Deterministic UNIQUE recovery: pre-insert the winner, then force
    /// `add_folder` to skip find and re-insert so the recovery branch runs.
    #[tokio::test]
    async fn add_folder_recovers_from_unique_constraint_after_forced_skip_find() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-folder-unique-recovery";
        let first = add_folder(&db.conn, path).await.expect("seed winner");
        force_add_folder_skip_existing_for_test(1);
        let second = add_folder(&db.conn, path)
            .await
            .expect("UNIQUE recovery must reopen winner");
        force_add_folder_skip_existing_for_test(0);
        assert_eq!(first.id, second.id);
        assert_eq!(second.path, path);
        let rows = folder::Entity::find()
            .filter(folder::Column::Path.eq(path))
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn last_agent_round_trips_only_for_regular_folders() {
        force_add_folder_skip_existing_for_test(0);
        let db = fresh_in_memory_db().await;
        let regular_id = seed_folder(&db, "/tmp/codeg-last-agent").await;
        let chat = add_chat_folder(&db.conn, "/tmp/codeg-chat-last-agent")
            .await
            .expect("create chat folder");

        let updated = update_folder_last_agent(&db.conn, regular_id, AgentType::Codex)
            .await
            .expect("update regular folder")
            .expect("regular folder detail");
        assert_eq!(updated.last_agent_type, Some(AgentType::Codex));
        assert_eq!(updated.default_agent_type, None);

        let chat_update = update_folder_last_agent(&db.conn, chat.id, AgentType::Gemini)
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
