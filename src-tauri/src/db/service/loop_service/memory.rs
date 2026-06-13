use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, Set,
};

use crate::db::entities::loop_artifact_revision::ActorKind;
use crate::db::entities::loop_iteration::Stage;
use crate::db::entities::loop_memory::{self, MemoryKind, MemoryStatus};
use crate::db::error::DbError;
use crate::models::loops::LoopMemoryRow;

pub fn to_row(m: loop_memory::Model) -> LoopMemoryRow {
    LoopMemoryRow {
        id: m.id,
        kind: m.kind,
        source: m.source,
        title: m.title,
        content: m.content,
        status: m.status,
        created_at: m.created_at,
        updated_at: m.updated_at,
    }
}

/// Memory kinds injected for a given stage (briefing §4.8 matrix). `constitution`
/// is handled separately by the briefing assembler, so it is never returned here.
fn kinds_for_stage(stage: Stage) -> Vec<MemoryKind> {
    use MemoryKind::*;
    match stage {
        Stage::Triage => vec![Constraint, Preference],
        Stage::Refine | Stage::Design => vec![Constraint, Decision, Preference],
        Stage::Plan => vec![Decision, Constraint],
        Stage::Implement => vec![Pitfall, Preference, Constraint],
        Stage::Review => vec![Constraint, Decision, Preference, Pitfall],
        // finalize summarizes; reuse the review-wide set.
        Stage::Finalize => vec![Constraint, Decision, Preference, Pitfall],
    }
}

pub async fn create_memory(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    kind: MemoryKind,
    source: ActorKind,
    title: &str,
    content: &str,
) -> Result<loop_memory::Model, DbError> {
    let now = Utc::now();
    Ok(loop_memory::ActiveModel {
        space_id: Set(space_id),
        kind: Set(kind),
        source: Set(source),
        title: Set(title.to_string()),
        content: Set(content.to_string()),
        status: Set(MemoryStatus::Active),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

pub async fn update_memory(
    conn: &sea_orm::DatabaseConnection,
    id: i32,
    title: &str,
    content: &str,
    status: MemoryStatus,
) -> Result<(), DbError> {
    let row = loop_memory::Entity::find_by_id(id)
        .one(conn)
        .await?
        .ok_or_else(|| {
            DbError::Database(sea_orm::DbErr::RecordNotFound(format!("loop_memory {id}")))
        })?;
    let mut active = row.into_active_model();
    active.title = Set(title.to_string());
    active.content = Set(content.to_string());
    active.status = Set(status);
    active.updated_at = Set(Utc::now());
    active.update(conn).await?;
    Ok(())
}

pub async fn delete_memory(conn: &sea_orm::DatabaseConnection, id: i32) -> Result<(), DbError> {
    loop_memory::Entity::delete_by_id(id).exec(conn).await?;
    Ok(())
}

pub async fn list_memory(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
) -> Result<Vec<LoopMemoryRow>, DbError> {
    Ok(loop_memory::Entity::find()
        .filter(loop_memory::Column::SpaceId.eq(space_id))
        .order_by_desc(loop_memory::Column::Id)
        .all(conn)
        .await?
        .into_iter()
        .map(to_row)
        .collect())
}

/// Active memories to inject for `stage` (excludes constitution).
pub async fn list_active_for_stage(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    stage: Stage,
) -> Result<Vec<loop_memory::Model>, DbError> {
    Ok(loop_memory::Entity::find()
        .filter(loop_memory::Column::SpaceId.eq(space_id))
        .filter(loop_memory::Column::Status.eq(MemoryStatus::Active))
        .filter(loop_memory::Column::Kind.is_in(kinds_for_stage(stage)))
        .order_by_asc(loop_memory::Column::Id)
        .all(conn)
        .await?)
}

/// The space constitution memories (always injected first by the briefing).
pub async fn list_constitution(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
) -> Result<Vec<loop_memory::Model>, DbError> {
    Ok(loop_memory::Entity::find()
        .filter(loop_memory::Column::SpaceId.eq(space_id))
        .filter(loop_memory::Column::Status.eq(MemoryStatus::Active))
        .filter(loop_memory::Column::Kind.eq(MemoryKind::Constitution))
        .order_by_asc(loop_memory::Column::Id)
        .all(conn)
        .await?)
}
