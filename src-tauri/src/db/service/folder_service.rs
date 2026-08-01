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

/// Normalize a path for folder `UNIQUE(path)` storage and lookup.
///
/// Strips Windows extended-length prefixes (`\\?\`, `//?/`, and UNC forms) so
/// the same physical directory is not registered twice (`D:\proj` vs
/// `\\?\D:\proj`). Does **not** canonicalize or change casing — those would
/// break missing paths and non-Windows trees.
pub fn normalize_folder_storage_path(path: &str) -> String {
    let t = path.trim();
    if let Some(rest) = t.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return format!(r"\\{unc}");
        }
        if let Some(unc) = rest.strip_prefix("UNC/") {
            return format!(r"\\{unc}");
        }
        return rest.to_string();
    }
    if let Some(rest) = t.strip_prefix("//?/") {
        if let Some(unc) = rest.strip_prefix("UNC/") {
            return format!("//{unc}");
        }
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return format!("//{unc}");
        }
        return rest.to_string();
    }
    t.to_string()
}

/// Alternate path spellings that may already exist as legacy rows (before
/// storage normalization). Used only for lookup — new inserts always write
/// [`normalize_folder_storage_path`].
fn folder_path_lookup_aliases(storage_path: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    // Legacy extended-length form for drive paths (`C:\...` → `\\?\C:\...`).
    let bytes = storage_path.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[1] == b':' && !storage_path.starts_with(r"\\");
    if is_drive && !storage_path.starts_with(r"\\?\") {
        aliases.push(format!(r"\\?\{storage_path}"));
    }
    // Forward-slash extended form.
    if is_drive && !storage_path.starts_with("//?/") {
        let fwd = storage_path.replace('\\', "/");
        aliases.push(format!("//?/{fwd}"));
    }
    aliases
}

async fn find_folder_row_by_path(
    conn: &DatabaseConnection,
    storage_path: &str,
) -> Result<Option<folder::Model>, DbError> {
    if let Some(row) = folder::Entity::find()
        .filter(folder::Column::Path.eq(storage_path))
        .one(conn)
        .await?
    {
        return Ok(Some(row));
    }
    for alt in folder_path_lookup_aliases(storage_path) {
        if let Some(row) = folder::Entity::find()
            .filter(folder::Column::Path.eq(&alt))
            .one(conn)
            .await?
        {
            return Ok(Some(row));
        }
    }
    Ok(None)
}

fn to_entry(m: folder::Model) -> FolderHistoryEntry {
    FolderHistoryEntry {
        id: m.id,
        path: m.path,
        name: m.name,
        last_opened_at: m.last_opened_at,
    }
}

fn parse_agent_type(s: &Option<String>) -> Option<AgentType> {
    s.as_deref().and_then(AgentType::from_wire)
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

/// How [`ensure_folder_inner`] writes the `parent_id` column. The two callers want
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

/// Visibility mode for path registration / open.
///
/// Replaces a bare `open: bool` which could be misread as ForceClosed. Delegation
/// and other FK-only registration use [`RegistrationOnly`]; explicit user open
/// paths use [`ForceOpen`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureFolderMode {
    /// Register or revive a path without changing workspace membership of an
    /// already-live row. Existing live `is_open` is **preserved**; new and
    /// soft-deleted→revived rows get `is_open = false` and do not masquerade as
    /// user-open timestamps.
    RegistrationOnly,
    /// Explicit open (user open, worktree mint, add-to-workspace): always set
    /// `is_open = true` and update open timestamps as today.
    ForceOpen,
}

/// Ensure a folder row exists for `path` with the given visibility mode.
///
/// See [`EnsureFolderMode`] for RegistrationOnly vs ForceOpen write semantics.
pub async fn ensure_folder(
    conn: &DatabaseConnection,
    path: &str,
    mode: EnsureFolderMode,
) -> Result<FolderHistoryEntry, DbError> {
    ensure_folder_inner(conn, path, ParentWrite::Preserve, mode).await
}

/// Force-open a folder path (user open / history add). Equivalent to
/// [`ensure_folder`] with [`EnsureFolderMode::ForceOpen`].
pub async fn add_folder(
    conn: &DatabaseConnection,
    path: &str,
) -> Result<FolderHistoryEntry, DbError> {
    ensure_folder_inner(
        conn,
        path,
        ParentWrite::Preserve,
        EnsureFolderMode::ForceOpen,
    )
    .await
}

/// Like [`add_folder`] but authoritatively sets `parent_id` — the *root* folder
/// this path was created under (used by the worktree flow so a worktree folder
/// remembers its originating repo folder). The value is written on both insert
/// and reopen, so it always reflects the latest worktree relationship and never
/// a stale one. Always ForceOpen (user-visible worktree open).
pub async fn add_folder_with_parent(
    conn: &DatabaseConnection,
    path: &str,
    parent_id: Option<i32>,
) -> Result<FolderHistoryEntry, DbError> {
    ensure_folder_inner(
        conn,
        path,
        ParentWrite::Set(parent_id),
        EnsureFolderMode::ForceOpen,
    )
    .await
}

fn is_unique_path_violation(err: &sea_orm::DbErr) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("unique constraint failed")
        || msg.contains("unique constraint")
        || msg.contains("2067") // SQLITE_CONSTRAINT_UNIQUE
        || msg.contains("1555") // SQLITE_CONSTRAINT_PRIMARYKEY
}

/// Test-only: per-path remaining skip-find budget for UNIQUE recovery tests.
///
/// Keyed by folder `path` so only matching [`ensure_folder`] / [`add_folder`]
/// calls consume budget. Unguarded parallel work on other paths cannot steal
/// another test's skips. Use [`ForceSkipExistingGuard`] so Drop always clears
/// the path entry (including panic).
#[cfg(test)]
static SKIP_EXISTING_BY_PATH: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, usize>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn skip_existing_map() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, usize>> {
    SKIP_EXISTING_BY_PATH
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Consume one skip for `path` if budget remains. Other paths are unaffected.
#[cfg(test)]
fn take_force_skip_existing(path: &str) -> bool {
    let mut map = skip_existing_map();
    match map.get_mut(path) {
        Some(n) if *n > 0 => {
            *n -= 1;
            if *n == 0 {
                map.remove(path);
            }
            true
        }
        _ => false,
    }
}

/// RAII arm of the path-scoped UNIQUE-recovery skip inject.
///
/// - Only `ensure_folder`/`add_folder` calls for this exact `path` skip find
/// - Parallel unguarded calls for other paths never consume this budget
/// - Drop removes the path entry so panics cannot leak armed state
#[cfg(test)]
pub struct ForceSkipExistingGuard {
    path: String,
}

#[cfg(test)]
impl ForceSkipExistingGuard {
    /// Arm the next `n` find-skips for `path` only.
    pub fn arm(path: impl Into<String>, n: usize) -> Self {
        let path = path.into();
        skip_existing_map().insert(path.clone(), n);
        Self { path }
    }
}

#[cfg(test)]
impl Drop for ForceSkipExistingGuard {
    fn drop(&mut self) {
        skip_existing_map().remove(&self.path);
    }
}

/// Arm path-scoped skip-existing race for tests. Keep the guard live for the
/// full armed interval (including concurrent tasks that consume this path).
#[cfg(test)]
pub fn force_add_folder_skip_existing_for_test(path: &str, n: usize) -> ForceSkipExistingGuard {
    ForceSkipExistingGuard::arm(path, n)
}

/// Test helper: remaining skip budget for `path` (0 if unarmed).
#[cfg(test)]
fn remaining_skip_budget_for_test(path: &str) -> usize {
    skip_existing_map().get(path).copied().unwrap_or(0)
}

/// Test-only: when armed with a folder id, the next
/// [`close_folder_if_no_live_conversations`] call inserts one live conversation
/// for that folder **immediately before** the conditional UPDATE — a
/// deterministic race hook proving live rows cannot lose to a successful close.
#[cfg(test)]
static FORCE_LIVE_INSERT_BEFORE_CLOSE: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(0);

/// Arm the pre-close live-insert race for tests (0 = disarmed).
#[cfg(test)]
pub fn force_live_insert_before_close_for_test(folder_id: i32) {
    FORCE_LIVE_INSERT_BEFORE_CLOSE.store(folder_id, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
fn take_force_live_insert_before_close() -> Option<i32> {
    use std::sync::atomic::Ordering;
    let id = FORCE_LIVE_INSERT_BEFORE_CLOSE.swap(0, Ordering::SeqCst);
    if id == 0 {
        None
    } else {
        Some(id)
    }
}

/// Apply mode-specific write to an existing path row (live or soft-deleted).
///
/// `storage_path` is the canonical path from [`normalize_folder_storage_path`].
/// When the row still carries a legacy extended-length spelling and no other
/// row owns `storage_path`, rewrite `path` so future lookups hit the canonical
/// key.
async fn apply_existing_folder_row(
    conn: &DatabaseConnection,
    row: folder::Model,
    storage_path: &str,
    name: String,
    now: chrono::DateTime<Utc>,
    parent: ParentWrite,
    mode: EnsureFolderMode,
) -> Result<folder::Model, DbError> {
    let path_rewrite = if row.path != storage_path {
        // Only rewrite when the canonical key is free (no other row).
        folder::Entity::find()
            .filter(folder::Column::Path.eq(storage_path))
            .filter(folder::Column::Id.ne(row.id))
            .one(conn)
            .await?
            .is_none()
    } else {
        false
    };

    match mode {
        EnsureFolderMode::ForceOpen => {
            let mut active = row.into_active_model();
            active.name = Set(name);
            active.last_opened_at = Set(now);
            active.updated_at = Set(now);
            active.deleted_at = Set(None);
            active.is_open = Set(true);
            if path_rewrite {
                active.path = Set(storage_path.to_string());
            }
            // Plain reopen leaves the relationship as-is; the worktree flow writes
            // the authoritative value (including NULL) so it can never go stale.
            if let ParentWrite::Set(parent_id) = parent {
                active.parent_id = Set(parent_id);
            }
            Ok(active.update(conn).await?)
        }
        EnsureFolderMode::RegistrationOnly => {
            let was_deleted = row.deleted_at.is_some();
            if !was_deleted {
                // Live existing: preserve is_open and last_opened_at. Only touch
                // name / parent / path / updated_at when something actually changes.
                let parent_change = match parent {
                    ParentWrite::Preserve => false,
                    ParentWrite::Set(pid) => row.parent_id != pid,
                };
                let name_change = row.name != name;
                if !parent_change && !name_change && !path_rewrite {
                    return Ok(row);
                }
                let mut active = row.into_active_model();
                if name_change {
                    active.name = Set(name);
                }
                if path_rewrite {
                    active.path = Set(storage_path.to_string());
                }
                if let ParentWrite::Set(parent_id) = parent {
                    active.parent_id = Set(parent_id);
                }
                active.updated_at = Set(now);
                return Ok(active.update(conn).await?);
            }
            // Soft-deleted → revive closed; do not bump last_opened_at.
            let mut active = row.into_active_model();
            active.name = Set(name);
            active.deleted_at = Set(None);
            active.is_open = Set(false);
            active.updated_at = Set(now);
            if path_rewrite {
                active.path = Set(storage_path.to_string());
            }
            if let ParentWrite::Set(parent_id) = parent {
                active.parent_id = Set(parent_id);
            }
            Ok(active.update(conn).await?)
        }
    }
}

async fn ensure_folder_inner(
    conn: &DatabaseConnection,
    path: &str,
    parent: ParentWrite,
    mode: EnsureFolderMode,
) -> Result<FolderHistoryEntry, DbError> {
    let path = normalize_folder_storage_path(path);
    let now = Utc::now();
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let existing = find_folder_row_by_path(conn, &path).await?;

    #[cfg(test)]
    let existing = if take_force_skip_existing(&path) {
        None
    } else {
        existing
    };

    let model = if let Some(row) = existing {
        apply_existing_folder_row(conn, row, &path, name, now, parent, mode).await?
    } else {
        let max_order = folder::Entity::find()
            .order_by_desc(folder::Column::SortOrder)
            .one(conn)
            .await?
            .map(|m| m.sort_order)
            .unwrap_or(0);
        // RegistrationOnly inserts closed; ForceOpen inserts open with open timestamps.
        let is_open = matches!(mode, EnsureFolderMode::ForceOpen);
        let active = folder::ActiveModel {
            id: NotSet,
            name: Set(name.clone()),
            path: Set(path.clone()),
            git_branch: Set(None),
            default_agent_type: Set(None),
            last_agent_type: Set(None),
            // Column is NOT NULL; RegistrationOnly still needs a value but
            // is_open=false is the visibility signal (not a user open).
            last_opened_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            is_open: Set(is_open),
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
            // re-apply mode semantics on the winner instead of surfacing error.
            Err(e) if is_unique_path_violation(&e) => {
                let winner = find_folder_row_by_path(conn, &path)
                    .await?
                    .ok_or_else(|| DbError::from(e))?;
                apply_existing_folder_row(conn, winner, &path, name, now, parent, mode).await?
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
    let path = normalize_folder_storage_path(path);
    let row = find_folder_row_by_path(conn, &path).await?;
    // Soft-delete only live (non-deleted) rows; alias lookup may surface a
    // soft-deleted twin — ignore those.
    let Some(row) = row.filter(|r| r.deleted_at.is_none()) else {
        return Ok(());
    };
    let mut active = row.into_active_model();
    active.deleted_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(conn).await?;
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
///
/// Includes delegation children. For sidebar-aligned emptiness use
/// [`count_sidebar_root_conversations_for_folder`].
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

/// Count conversations that keep a regular folder visible in the workspace
/// sidebar folder groups: live roots only (`parent_id IS NULL`), excluding
/// `chat` / `loop` kinds (those never render under a folder header).
pub async fn count_sidebar_root_conversations_for_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<u64, DbError> {
    use crate::db::entities::conversation;
    use crate::db::entities::conversation::ConversationKind;
    use sea_orm::PaginatorTrait;

    let n = conversation::Entity::find()
        .filter(conversation::Column::FolderId.eq(folder_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::ParentId.is_null())
        .filter(conversation::Column::Kind.ne(ConversationKind::Chat))
        .filter(conversation::Column::Kind.ne(ConversationKind::Loop))
        .count(conn)
        .await?;
    Ok(n)
}

/// SQL predicate: no sidebar-visible root conversations on this folder.
/// Matches [`count_sidebar_root_conversations_for_folder`] / list_all defaults
/// (`include_children = false`, no chat/loop under folder headers).
const NO_SIDEBAR_ROOT_CONVERSATIONS_SQL: &str = "NOT EXISTS (\
    SELECT 1 FROM conversation c \
    WHERE c.folder_id = folder.id \
      AND c.deleted_at IS NULL \
      AND c.parent_id IS NULL \
      AND c.kind NOT IN ('chat', 'loop')\
)";

/// Visibility-only auto-close for one folder when it is still open, regular,
/// not soft-deleted, and has **no sidebar-visible root conversations**.
///
/// Delegation children alone do **not** keep the folder open — the sidebar
/// never lists them under the folder header (`include_children = false`), so
/// counting them left worktree folders stuck as "暂无会话".
///
/// Atomic: a single conditional `UPDATE` with `NOT EXISTS` sidebar roots.
/// Returns `true` only when this statement flipped `is_open` true→false.
/// No-op (`false`) for missing, chat kind, already closed, soft-deleted, or
/// folders with at least one root conversation — never touches `deleted_at`.
pub async fn close_folder_if_no_live_conversations(
    conn: &DatabaseConnection,
    folder_id: i32,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;

    // Deterministic race hook (tests only): insert a live **root** conversation
    // after the call starts but before the atomic UPDATE evaluates NOT EXISTS.
    #[cfg(test)]
    if let Some(hook_id) = take_force_live_insert_before_close() {
        if hook_id == folder_id {
            crate::db::service::conversation_service::create(
                conn,
                folder_id,
                AgentType::ClaudeCode,
                None,
                None,
            )
            .await?;
        }
    }

    let now = Utc::now();
    let result = folder::Entity::update_many()
        .col_expr(folder::Column::IsOpen, Expr::value(false))
        .col_expr(folder::Column::UpdatedAt, Expr::value(now))
        .filter(folder::Column::Id.eq(folder_id))
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::Kind.eq(FolderKind::Regular))
        .filter(folder::Column::IsOpen.eq(true))
        .filter(Expr::cust(NO_SIDEBAR_ROOT_CONVERSATIONS_SQL))
        .exec(conn)
        .await?;

    Ok(result.rows_affected == 1)
}

/// Bulk reconcile: close every open regular folder with no sidebar-visible
/// root conversations. Returns the ids that were closed (caller derives count).
///
/// Each close uses the same atomic [`close_folder_if_no_live_conversations`]
/// primitive, not a pre-count then set.
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
        close_open_folders_with_no_live_conversations, count_live_conversations_for_folder,
        count_sidebar_root_conversations_for_folder, ensure_folder,
        force_add_folder_skip_existing_for_test, force_live_insert_before_close_for_test,
        get_folder_by_id, list_open_folder_details, list_open_folders,
        normalize_folder_storage_path, remaining_skip_budget_for_test, set_folder_open,
        update_folder_last_agent, EnsureFolderMode,
    };
    use crate::db::entities::folder;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
    use std::sync::Arc;
    use tokio::sync::Barrier;

    async fn raw_folder(conn: &sea_orm::DatabaseConnection, id: i32) -> folder::Model {
        folder::Entity::find_by_id(id)
            .one(conn)
            .await
            .expect("query")
            .expect("folder row")
    }

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

    /// Sequencing proof for the startup readiness barrier: after bulk reconcile
    /// completes, `list_open_folder_details` (the client open-list surface) must
    /// not include any regular folder with zero sidebar-visible root conversations.
    #[tokio::test]
    async fn reconcile_then_list_open_folder_details_has_no_empty_regular() {
        let db = fresh_in_memory_db().await;
        let empty_id = seed_folder(&db, "/tmp/codeg-barrier-empty").await;
        let kept_id = seed_folder(&db, "/tmp/codeg-barrier-kept").await;
        seed_conversation(&db, kept_id, AgentType::ClaudeCode).await;

        let closed = close_open_folders_with_no_live_conversations(&db.conn)
            .await
            .expect("barrier reconcile completes");
        assert_eq!(closed, vec![empty_id]);

        let details = list_open_folder_details(&db.conn)
            .await
            .expect("list_open_folder_details after barrier");
        let ids: Vec<i32> = details.iter().map(|d| d.id).collect();
        assert!(!ids.contains(&empty_id));
        assert!(ids.contains(&kept_id));
        for d in details {
            let roots = count_sidebar_root_conversations_for_folder(&db.conn, d.id)
                .await
                .expect("count roots");
            assert!(
                roots > 0,
                "open detail id={} must have sidebar root convs",
                d.id
            );
        }
    }

    #[tokio::test]
    async fn close_folder_with_only_delegate_children_is_closed() {
        use crate::acp::delegation::spawner::DelegationLink;

        let db = fresh_in_memory_db().await;
        let parent_folder = seed_folder(&db, "/tmp/codeg-parent-repo").await;
        let worktree_folder = seed_folder(&db, r"\\?\D:\codeg-worktree-only-children").await;
        let parent_conv = seed_conversation(&db, parent_folder, AgentType::ClaudeCode).await;

        conversation_service::create_with_delegation(
            &db.conn,
            worktree_folder,
            AgentType::ClaudeCode,
            Some("child review".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_conv,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "dc-1".into(),
            }),
        )
        .await
        .expect("delegate child");

        assert_eq!(
            count_live_conversations_for_folder(&db.conn, worktree_folder)
                .await
                .unwrap(),
            1,
            "child rows still count as live"
        );
        assert_eq!(
            count_sidebar_root_conversations_for_folder(&db.conn, worktree_folder)
                .await
                .unwrap(),
            0,
            "no sidebar roots"
        );

        assert!(
            close_folder_if_no_live_conversations(&db.conn, worktree_folder)
                .await
                .unwrap(),
            "delegate-only folder must auto-close"
        );
        let row = raw_folder(&db.conn, worktree_folder).await;
        assert!(!row.is_open);
        // Parent with a root stays open.
        assert!(
            !close_folder_if_no_live_conversations(&db.conn, parent_folder)
                .await
                .unwrap()
        );
    }

    #[test]
    fn normalize_folder_storage_path_strips_extended_prefix() {
        assert_eq!(
            normalize_folder_storage_path(r"\\?\D:\MyCodeBuddy"),
            r"D:\MyCodeBuddy"
        );
        assert_eq!(
            normalize_folder_storage_path(r"\\?\UNC\server\share\proj"),
            r"\\server\share\proj"
        );
        assert_eq!(normalize_folder_storage_path("//?/C:/work"), "C:/work");
        assert_eq!(
            normalize_folder_storage_path(r"D:\MyCodeBuddy"),
            r"D:\MyCodeBuddy"
        );
    }

    #[tokio::test]
    async fn add_folder_collapses_extended_length_alias_to_existing() {
        let db = fresh_in_memory_db().await;
        let first = add_folder(&db.conn, r"D:\codeg-alias-collapse")
            .await
            .expect("add plain");
        let second = add_folder(&db.conn, r"\\?\D:\codeg-alias-collapse")
            .await
            .expect("add extended");
        assert_eq!(
            first.id, second.id,
            "extended-length path must not mint a second folder row"
        );
        let row = raw_folder(&db.conn, first.id).await;
        assert_eq!(row.path, r"D:\codeg-alias-collapse");
    }

    #[tokio::test]
    async fn add_folder_rewrites_legacy_extended_path_when_safe() {
        let db = fresh_in_memory_db().await;
        // Insert legacy spelling without going through normalize (direct SQL path
        // would be heavy; use seed then force path via ActiveModel).
        let id = seed_folder(&db, r"D:\codeg-legacy-rewrite-tmp").await;
        let mut active = raw_folder(&db.conn, id).await.into_active_model();
        active.path = Set(r"\\?\D:\codeg-legacy-rewrite".to_string());
        active.update(&db.conn).await.expect("legacy path");

        let opened = add_folder(&db.conn, r"\\?\D:\codeg-legacy-rewrite")
            .await
            .expect("open via extended");
        assert_eq!(opened.id, id);
        let row = raw_folder(&db.conn, id).await;
        assert_eq!(
            row.path, r"D:\codeg-legacy-rewrite",
            "safe rewrite to storage-normalized path"
        );
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
        let raw = raw_folder(&db.conn, chat.id).await;
        assert!(raw.is_open, "chat is_open must remain true");
        assert!(raw.deleted_at.is_none(), "chat must not be soft-deleted");
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
    async fn close_folder_if_no_live_conversations_noops_without_touching_deleted_at() {
        let db = fresh_in_memory_db().await;

        // --- already closed ---
        let closed_id = seed_folder(&db, "/tmp/codeg-already-closed").await;
        set_folder_open(&db.conn, closed_id, false)
            .await
            .expect("close");
        let before = raw_folder(&db.conn, closed_id).await;
        assert!(!before.is_open);
        assert!(before.deleted_at.is_none());
        assert!(!close_folder_if_no_live_conversations(&db.conn, closed_id)
            .await
            .unwrap());
        let after = raw_folder(&db.conn, closed_id).await;
        assert!(!after.is_open);
        assert_eq!(after.deleted_at, before.deleted_at);

        // --- missing id ---
        assert!(!close_folder_if_no_live_conversations(&db.conn, 9_999_999)
            .await
            .unwrap());

        // --- chat kind: false, is_open unchanged, deleted_at unchanged ---
        let chat = add_chat_folder(&db.conn, "/tmp/codeg-chat-noop")
            .await
            .expect("chat");
        let chat_before = raw_folder(&db.conn, chat.id).await;
        assert!(chat_before.is_open);
        assert!(chat_before.deleted_at.is_none());
        assert!(!close_folder_if_no_live_conversations(&db.conn, chat.id)
            .await
            .unwrap());
        let chat_after = raw_folder(&db.conn, chat.id).await;
        assert!(
            chat_after.is_open,
            "chat is_open must stay true after no-op close"
        );
        assert_eq!(chat_after.deleted_at, chat_before.deleted_at);

        // --- soft-deleted regular folder ---
        let deleted_id = seed_folder(&db, "/tmp/codeg-soft-deleted").await;
        let mut del = raw_folder(&db.conn, deleted_id).await.into_active_model();
        let del_at = Utc::now();
        del.deleted_at = Set(Some(del_at));
        del.is_open = Set(true); // still marked open but soft-deleted
        del.update(&db.conn).await.expect("soft-delete folder");
        let del_before = raw_folder(&db.conn, deleted_id).await;
        assert!(del_before.deleted_at.is_some());
        assert!(!close_folder_if_no_live_conversations(&db.conn, deleted_id)
            .await
            .unwrap());
        let del_after = raw_folder(&db.conn, deleted_id).await;
        assert_eq!(
            del_after.deleted_at.map(|t| t.timestamp()),
            del_before.deleted_at.map(|t| t.timestamp()),
            "deleted_at must not change on no-op close"
        );
        assert!(del_after.is_open, "soft-deleted row is_open left alone");

        // --- non-empty (live conversation) ---
        let nonempty_id = seed_folder(&db, "/tmp/codeg-nonempty").await;
        seed_conversation(&db, nonempty_id, AgentType::ClaudeCode).await;
        let ne_before = raw_folder(&db.conn, nonempty_id).await;
        assert!(ne_before.is_open);
        assert!(ne_before.deleted_at.is_none());
        assert!(
            !close_folder_if_no_live_conversations(&db.conn, nonempty_id)
                .await
                .unwrap()
        );
        let ne_after = raw_folder(&db.conn, nonempty_id).await;
        assert!(ne_after.is_open, "non-empty folder must stay open");
        assert_eq!(ne_after.deleted_at, ne_before.deleted_at);
    }

    #[tokio::test]
    async fn close_folder_if_no_live_conversations_loses_to_live_insert_race_hook() {
        let db = fresh_in_memory_db().await;
        let id = seed_folder(&db, "/tmp/codeg-race-hook").await;
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            0
        );

        // Insert lands after call entry, before the atomic UPDATE NOT EXISTS.
        force_live_insert_before_close_for_test(id);
        let closed = close_folder_if_no_live_conversations(&db.conn, id)
            .await
            .expect("close");
        force_live_insert_before_close_for_test(0); // disarm if unused

        assert!(
            !closed,
            "live insert before UPDATE must prevent a successful close"
        );
        let row = raw_folder(&db.conn, id).await;
        assert!(row.is_open, "folder must remain open when race insert wins");
        assert!(row.deleted_at.is_none());
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn close_folder_if_no_live_conversations_concurrent_live_insert() {
        // Concurrent insert + close: successful close must never leave a
        // still-open folder; live present with open stays open (insert won).
        for _ in 0..8 {
            let db = Arc::new(fresh_in_memory_db().await);
            let id = seed_folder(&db, "/tmp/codeg-concurrent-close").await;
            let barrier = Arc::new(Barrier::new(2));

            let b1 = barrier.clone();
            let db1 = db.clone();
            let close_task = tokio::spawn(async move {
                b1.wait().await;
                close_folder_if_no_live_conversations(&db1.conn, id).await
            });
            let b2 = barrier.clone();
            let db2 = db.clone();
            let insert_task = tokio::spawn(async move {
                b2.wait().await;
                seed_conversation(&db2, id, AgentType::ClaudeCode).await
            });

            let closed = close_task.await.expect("join close").expect("close result");
            let _conv_id = insert_task.await.expect("join insert");

            let row = raw_folder(&db.conn, id).await;
            let live = count_live_conversations_for_folder(&db.conn, id)
                .await
                .expect("count");
            assert!(live >= 1, "insert must have landed");
            assert!(row.deleted_at.is_none(), "deleted_at never touched");
            if closed {
                assert!(!row.is_open, "successful close must flip is_open false");
            } else {
                assert!(
                    row.is_open,
                    "failed close (live present at UPDATE) must leave open"
                );
            }
        }
    }

    #[tokio::test]
    async fn count_live_conversations_for_folder_live_vs_soft_deleted() {
        let db = fresh_in_memory_db().await;
        let id = seed_folder(&db, "/tmp/codeg-count-live").await;
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            0
        );

        let live_a = seed_conversation(&db, id, AgentType::ClaudeCode).await;
        let live_b = seed_conversation(&db, id, AgentType::Codex).await;
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            2
        );

        conversation_service::soft_delete(&db.conn, live_a)
            .await
            .expect("soft delete one");
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            1,
            "soft-deleted conversation must not count as live"
        );

        conversation_service::soft_delete(&db.conn, live_b)
            .await
            .expect("soft delete remaining");
        assert_eq!(
            count_live_conversations_for_folder(&db.conn, id)
                .await
                .unwrap(),
            0
        );

        // Soft-deleted-only folder is eligible for visibility close.
        assert!(close_folder_if_no_live_conversations(&db.conn, id)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn concurrent_add_folder_same_path_converges_without_unique_error() {
        let db = Arc::new(fresh_in_memory_db().await);
        let path = "/tmp/codeg-folder-unique-race";
        // Force both callers past find → INSERT so one hits UNIQUE recovery.
        // Path-keyed: only this path's ensure/add calls consume budget.
        let _skip = force_add_folder_skip_existing_for_test(path, 2);
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
        drop(_skip);
    }

    /// Deterministic UNIQUE recovery: pre-insert the winner, then force
    /// `add_folder` to skip find and re-insert so the recovery branch runs.
    #[tokio::test]
    async fn add_folder_recovers_from_unique_constraint_after_forced_skip_find() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-folder-unique-recovery";
        let first = add_folder(&db.conn, path).await.expect("seed winner");
        let _skip = force_add_folder_skip_existing_for_test(path, 1);
        let second = add_folder(&db.conn, path)
            .await
            .expect("UNIQUE recovery must reopen winner");
        drop(_skip);
        assert_eq!(first.id, second.id);
        assert_eq!(second.path, path);
        let rows = folder::Entity::find()
            .filter(folder::Column::Path.eq(path))
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
    }

    // --- ensure_folder RegistrationOnly matrix (Task 8) ---

    #[tokio::test]
    async fn registration_only_new_path_is_closed() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-new";
        let entry = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("register");
        let row = raw_folder(&db.conn, entry.id).await;
        assert!(!row.is_open, "new RegistrationOnly row must stay closed");
        assert!(row.deleted_at.is_none());
        assert_eq!(row.path, path);
    }

    #[tokio::test]
    async fn registration_only_preserves_existing_open() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-preserve-open";
        let id = seed_folder(&db, path).await;
        let before = raw_folder(&db.conn, id).await;
        assert!(before.is_open);
        let opened_at = before.last_opened_at;

        let entry = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("register");
        assert_eq!(entry.id, id);
        let after = raw_folder(&db.conn, id).await;
        assert!(after.is_open, "must not force-close an already-open row");
        assert_eq!(
            after.last_opened_at.timestamp_millis(),
            opened_at.timestamp_millis(),
            "must not masquerade as a user open (last_opened_at)"
        );
    }

    #[tokio::test]
    async fn registration_only_preserves_existing_closed() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-preserve-closed";
        let id = seed_folder(&db, path).await;
        set_folder_open(&db.conn, id, false).await.expect("close");
        let before = raw_folder(&db.conn, id).await;
        assert!(!before.is_open);
        let opened_at = before.last_opened_at;

        ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("register");
        let after = raw_folder(&db.conn, id).await;
        assert!(!after.is_open, "must not force-open a closed row");
        assert_eq!(
            after.last_opened_at.timestamp_millis(),
            opened_at.timestamp_millis(),
            "must not bump last_opened_at for RegistrationOnly"
        );
    }

    #[tokio::test]
    async fn registration_only_revives_soft_deleted_closed_without_open_timestamps() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-revive";
        let id = seed_folder(&db, path).await;
        let mut active = raw_folder(&db.conn, id).await.into_active_model();
        let past = Utc::now() - chrono::Duration::days(7);
        active.last_opened_at = Set(past);
        active.is_open = Set(true); // stale open flag on deleted row
        active.deleted_at = Set(Some(Utc::now()));
        active.update(&db.conn).await.expect("soft-delete");

        let entry = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("revive");
        assert_eq!(entry.id, id);
        let after = raw_folder(&db.conn, id).await;
        assert!(after.deleted_at.is_none(), "must clear soft-delete");
        assert!(
            !after.is_open,
            "revived RegistrationOnly row must be closed"
        );
        assert_eq!(
            after.last_opened_at.timestamp_millis(),
            past.timestamp_millis(),
            "must not masquerade revival as user open timestamps"
        );
    }

    #[tokio::test]
    async fn registration_only_recovers_from_unique_constraint_without_opening() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-unique-recovery";
        // Winner inserted closed via RegistrationOnly.
        let first = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("seed winner");
        assert!(!raw_folder(&db.conn, first.id).await.is_open);
        let _skip = force_add_folder_skip_existing_for_test(path, 1);
        let second = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("UNIQUE recovery must re-resolve winner");
        drop(_skip);
        assert_eq!(first.id, second.id);
        let row = raw_folder(&db.conn, first.id).await;
        assert!(
            !row.is_open,
            "UNIQUE recovery under RegistrationOnly must not ForceOpen"
        );
        let rows = folder::Entity::find()
            .filter(folder::Column::Path.eq(path))
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn concurrent_registration_only_same_path_converges_closed() {
        let db = Arc::new(fresh_in_memory_db().await);
        let path = "/tmp/codeg-reg-only-concurrent";
        let _skip = force_add_folder_skip_existing_for_test(path, 2);
        let barrier = Arc::new(Barrier::new(2));
        let b1 = barrier.clone();
        let db1 = db.clone();
        let t1 = tokio::spawn(async move {
            b1.wait().await;
            ensure_folder(&db1.conn, path, EnsureFolderMode::RegistrationOnly).await
        });
        let b2 = barrier.clone();
        let db2 = db.clone();
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            ensure_folder(&db2.conn, path, EnsureFolderMode::RegistrationOnly).await
        });
        let (a, b) = tokio::join!(t1, t2);
        let a = a.expect("join a").expect("reg a");
        let b = b.expect("join b").expect("reg b");
        assert_eq!(a.id, b.id);
        let row = raw_folder(&db.conn, a.id).await;
        assert!(!row.is_open, "concurrent RegistrationOnly must stay closed");
        drop(_skip);
    }

    /// Path-keyed inject: unguarded ensure_folder on another path must not
    /// consume the armed path's skip budget (parallel-cargo safety).
    #[tokio::test]
    async fn skip_existing_inject_is_path_scoped_not_stolen_by_other_paths() {
        let db = fresh_in_memory_db().await;
        let armed = "/tmp/codeg-skip-inject-armed";
        let other = "/tmp/codeg-skip-inject-other";
        let _skip = force_add_folder_skip_existing_for_test(armed, 2);
        assert_eq!(remaining_skip_budget_for_test(armed), 2);

        // Distraction traffic on a different path (would steal a process-global
        // counter; must leave armed budget untouched).
        ensure_folder(&db.conn, other, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("other path");
        ensure_folder(&db.conn, other, EnsureFolderMode::ForceOpen)
            .await
            .expect("other path again");
        assert_eq!(
            remaining_skip_budget_for_test(armed),
            2,
            "other-path ensure/add must not steal path-keyed skip budget"
        );

        // Seed winner, then force UNIQUE recovery — each armed-path call may
        // consume one skip when find would otherwise hit the row.
        let first = ensure_folder(&db.conn, armed, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("seed armed (consumes one skip on fresh insert path)");
        assert_eq!(remaining_skip_budget_for_test(armed), 1);
        let second = ensure_folder(&db.conn, armed, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("UNIQUE recovery with remaining path-scoped skip");
        assert_eq!(remaining_skip_budget_for_test(armed), 0);
        assert_eq!(first.id, second.id);
        drop(_skip);
        assert_eq!(remaining_skip_budget_for_test(armed), 0);
    }

    #[tokio::test]
    async fn force_open_still_opens_new_and_closed_rows() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-force-open-new";
        let entry = ensure_folder(&db.conn, path, EnsureFolderMode::ForceOpen)
            .await
            .expect("force open new");
        assert!(raw_folder(&db.conn, entry.id).await.is_open);

        set_folder_open(&db.conn, entry.id, false)
            .await
            .expect("close");
        ensure_folder(&db.conn, path, EnsureFolderMode::ForceOpen)
            .await
            .expect("force re-open");
        assert!(
            raw_folder(&db.conn, entry.id).await.is_open,
            "ForceOpen must re-open a closed row"
        );
    }

    /// Service-level composition only. Production call sites are covered by
    /// `manager_legacy_delegation_child_keeps_working_dir_folder_closed` and
    /// `durable_reserve_registers_working_dir_folder_closed`.
    #[tokio::test]
    async fn registration_only_does_not_open_when_hidden_child_would_use_path() {
        let db = fresh_in_memory_db().await;
        let path = "/tmp/codeg-reg-only-hidden-child";
        let entry = ensure_folder(&db.conn, path, EnsureFolderMode::RegistrationOnly)
            .await
            .expect("register working_dir");
        let _child = conversation_service::create(
            &db.conn,
            entry.id,
            AgentType::ClaudeCode,
            Some("delegated task".into()),
            None,
        )
        .await
        .expect("hidden child conversation");
        let row = raw_folder(&db.conn, entry.id).await;
        assert!(
            !row.is_open,
            "creating a hidden child must not ForceOpen the folder"
        );
    }

    #[tokio::test]
    async fn last_agent_round_trips_only_for_regular_folders() {
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
