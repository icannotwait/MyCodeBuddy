use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set,
};

use crate::db::entities::loop_inbox_item::{self, InboxKind, InboxStatus};
use crate::db::entities::loop_issue;
use crate::db::error::DbError;
use crate::models::loops::LoopInboxItemRow;

fn to_row(m: loop_inbox_item::Model, issue_seq: i32) -> LoopInboxItemRow {
    LoopInboxItemRow {
        id: m.id,
        issue_id: m.issue_id,
        issue_seq,
        iteration_id: m.iteration_id,
        kind: m.kind,
        subject_key: m.subject_key,
        payload: serde_json::from_str(&m.payload).unwrap_or(serde_json::Value::Null),
        status: m.status,
        created_at: m.created_at,
    }
}

/// Insert a pending inbox item, or return the existing pending one with the same
/// `(issue_id, kind, subject_key)` — recovery and repeated ticks must not stack
/// duplicate cards (also guarded by `uniq_inbox_pending`).
pub async fn upsert_inbox(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    issue_id: i32,
    iteration_id: Option<i32>,
    kind: InboxKind,
    subject_key: &str,
    payload: serde_json::Value,
) -> Result<loop_inbox_item::Model, DbError> {
    if let Some(existing) = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::IssueId.eq(issue_id))
        .filter(loop_inbox_item::Column::Kind.eq(kind))
        .filter(loop_inbox_item::Column::SubjectKey.eq(subject_key))
        .filter(loop_inbox_item::Column::Status.eq(InboxStatus::Pending))
        .one(conn)
        .await?
    {
        return Ok(existing);
    }
    Ok(loop_inbox_item::ActiveModel {
        space_id: Set(space_id),
        issue_id: Set(issue_id),
        iteration_id: Set(iteration_id),
        kind: Set(kind),
        subject_key: Set(subject_key.to_string()),
        payload: Set(payload.to_string()),
        status: Set(InboxStatus::Pending),
        resolution: Set(None),
        created_at: Set(Utc::now()),
        handled_at: Set(None),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

pub async fn list_inbox(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    status: Option<InboxStatus>,
) -> Result<Vec<LoopInboxItemRow>, DbError> {
    let seqs: HashMap<i32, i32> = loop_issue::Entity::find()
        .filter(loop_issue::Column::SpaceId.eq(space_id))
        .all(conn)
        .await?
        .into_iter()
        .map(|i| (i.id, i.seq_no))
        .collect();
    let mut query = loop_inbox_item::Entity::find()
        .filter(loop_inbox_item::Column::SpaceId.eq(space_id))
        .order_by_desc(loop_inbox_item::Column::Id);
    if let Some(status) = status {
        query = query.filter(loop_inbox_item::Column::Status.eq(status));
    }
    Ok(query
        .all(conn)
        .await?
        .into_iter()
        .map(|m| {
            let seq = *seqs.get(&m.issue_id).unwrap_or(&0);
            to_row(m, seq)
        })
        .collect())
}

/// Fetch a single inbox item by id — used by the command layer to guard a
/// dismiss to informational cards before marking it handled.
pub async fn get_inbox(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
) -> Result<Option<loop_inbox_item::Model>, DbError> {
    Ok(loop_inbox_item::Entity::find_by_id(id).one(conn).await?)
}

pub async fn handle_inbox(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
    resolution: serde_json::Value,
) -> Result<(), DbError> {
    let row = loop_inbox_item::Entity::find_by_id(id)
        .one(conn)
        .await?
        .ok_or_else(|| {
            DbError::Database(sea_orm::DbErr::RecordNotFound(format!("loop_inbox_item {id}")))
        })?;
    let mut active = row.into_active_model();
    active.status = Set(InboxStatus::Handled);
    active.resolution = Set(Some(resolution.to_string()));
    active.handled_at = Set(Some(Utc::now()));
    active.update(conn).await?;
    Ok(())
}
