use chrono::Utc;
use sea_orm::DatabaseConnection;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait,
    IntoActiveModel, QueryFilter, QueryOrder, Set, Statement,
};

use crate::db::entities::folder;
use crate::db::error::DbError;
use crate::models::agent::AgentType;
use crate::models::{FolderDetail, FolderHistoryEntry};

/// Sentinel stored in the DB that the frontend resolves to
/// `var(--sidebar-foreground)` — the theme-aware text color. New folders
/// start with this neutral swatch until the user picks a palette color.
pub const DEFAULT_FOLDER_COLOR: &str = "foreground";

fn to_entry(m: folder::Model) -> FolderHistoryEntry {
    FolderHistoryEntry {
        id: m.id,
        path: m.path,
        name: m.name,
        last_opened_at: m.last_opened_at,
        connection_id: m.connection_id,
        remote_path: m.remote_path,
    }
}

fn parse_agent_type(s: &Option<String>) -> Option<AgentType> {
    s.as_deref()
        .and_then(|v| serde_json::from_value(serde_json::Value::String(v.to_string())).ok())
}

fn to_detail(m: folder::Model) -> FolderDetail {
    let default_agent_type = parse_agent_type(&m.default_agent_type);
    FolderDetail {
        id: m.id,
        name: m.name,
        path: m.path,
        git_branch: m.git_branch,
        default_agent_type,
        last_opened_at: m.last_opened_at,
        sort_order: m.sort_order,
        color: m.color,
        connection_id: m.connection_id,
        remote_path: m.remote_path,
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

pub async fn add_folder(
    conn: &DatabaseConnection,
    path: &str,
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

    let model = if let Some(row) = existing {
        let mut active = row.into_active_model();
        active.name = Set(name);
        active.last_opened_at = Set(now);
        active.updated_at = Set(now);
        active.deleted_at = Set(None);
        active.is_open = Set(true);
        active.update(conn).await?
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
            last_opened_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            is_open: Set(true),
            sort_order: Set(max_order + 1),
            color: Set(DEFAULT_FOLDER_COLOR.to_string()),
            connection_id: Set(None),
            remote_path: Set(None),
        };
        active.insert(conn).await?
    };

    Ok(to_entry(model))
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

pub async fn list_folders(conn: &DatabaseConnection) -> Result<Vec<FolderHistoryEntry>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
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

pub async fn list_open_folders(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderHistoryEntry>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::IsOpen.eq(true))
        .order_by_desc(folder::Column::LastOpenedAt)
        .all(conn)
        .await?;

    Ok(rows.into_iter().map(to_entry).collect())
}

pub async fn list_open_folder_details(
    conn: &DatabaseConnection,
) -> Result<Vec<FolderDetail>, DbError> {
    let rows = folder::Entity::find()
        .filter(folder::Column::DeletedAt.is_null())
        .filter(folder::Column::IsOpen.eq(true))
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

/// Insert (or refresh) a remote folder row keyed by the synthetic
/// `ssh://<connection_id><remote_path>` path. On hit we bump
/// `last_opened_at` and re-open the folder; on miss we create a new row
/// with the connection metadata and let the path's UNIQUE constraint
/// enforce one-row-per-(connection, remote_path).
pub async fn upsert_remote_folder(
    conn: &DatabaseConnection,
    connection_id: &str,
    remote_path: &str,
) -> Result<FolderDetail, DbError> {
    use crate::models::folder::{folder_name_from_remote_path, synthetic_remote_path};

    let synthetic = synthetic_remote_path(connection_id, remote_path);
    let display_name = folder_name_from_remote_path(remote_path);
    let now = Utc::now();

    let existing = folder::Entity::find()
        .filter(folder::Column::Path.eq(synthetic.clone()))
        .one(conn)
        .await?;

    let model = if let Some(row) = existing {
        let mut active = row.into_active_model();
        active.name = Set(display_name);
        active.last_opened_at = Set(now);
        active.updated_at = Set(now);
        active.deleted_at = Set(None);
        active.is_open = Set(true);
        active.connection_id = Set(Some(connection_id.to_string()));
        active.remote_path = Set(Some(remote_path.trim().to_string()));
        active.update(conn).await?
    } else {
        let max_order = folder::Entity::find()
            .order_by_desc(folder::Column::SortOrder)
            .one(conn)
            .await?
            .map(|m| m.sort_order)
            .unwrap_or(0);
        let active = folder::ActiveModel {
            id: NotSet,
            name: Set(display_name),
            path: Set(synthetic),
            git_branch: Set(None),
            default_agent_type: Set(None),
            last_opened_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            is_open: Set(true),
            sort_order: Set(max_order + 1),
            color: Set(DEFAULT_FOLDER_COLOR.to_string()),
            connection_id: Set(Some(connection_id.to_string())),
            remote_path: Set(Some(remote_path.trim().to_string())),
        };
        active.insert(conn).await?
    };

    Ok(to_detail(model))
}
