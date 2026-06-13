use chrono::Utc;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};

use crate::db::entities::loop_validation_run;
use crate::db::error::DbError;
use crate::models::loops::LoopValidationRunRow;

fn to_row(m: loop_validation_run::Model) -> LoopValidationRunRow {
    LoopValidationRunRow {
        id: m.id,
        task_artifact_id: m.task_artifact_id,
        iteration_id: m.iteration_id,
        commands: serde_json::from_str(&m.commands).unwrap_or_default(),
        exit_codes: serde_json::from_str(&m.exit_codes).unwrap_or_default(),
        passed: m.passed,
        created_at: m.created_at,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn record_validation_run(
    conn: &sea_orm::DatabaseConnection,
    space_id: i32,
    issue_id: i32,
    task_artifact_id: i32,
    iteration_id: Option<i32>,
    commands: &[String],
    exit_codes: &[i32],
    output: &str,
    passed: bool,
) -> Result<loop_validation_run::Model, DbError> {
    Ok(loop_validation_run::ActiveModel {
        space_id: Set(space_id),
        issue_id: Set(issue_id),
        task_artifact_id: Set(task_artifact_id),
        iteration_id: Set(iteration_id),
        commands: Set(serde_json::to_string(commands).unwrap_or_else(|_| "[]".to_string())),
        exit_codes: Set(serde_json::to_string(exit_codes).unwrap_or_else(|_| "[]".to_string())),
        output: Set(output.to_string()),
        passed: Set(passed),
        created_at: Set(Utc::now()),
        ..Default::default()
    }
    .insert(conn)
    .await?)
}

pub async fn list_for_task(
    conn: &sea_orm::DatabaseConnection,
    task_artifact_id: i32,
) -> Result<Vec<LoopValidationRunRow>, DbError> {
    Ok(loop_validation_run::Entity::find()
        .filter(loop_validation_run::Column::TaskArtifactId.eq(task_artifact_id))
        .order_by_desc(loop_validation_run::Column::Id)
        .all(conn)
        .await?
        .into_iter()
        .map(to_row)
        .collect())
}

/// The most recent run for a task, as the full entity (the `LoopValidationRunRow`
/// DTO omits `output`, which the implement briefing needs to feed a failure
/// back to the next attempt).
pub async fn latest_for_task(
    conn: &sea_orm::DatabaseConnection,
    task_artifact_id: i32,
) -> Result<Option<loop_validation_run::Model>, DbError> {
    Ok(loop_validation_run::Entity::find()
        .filter(loop_validation_run::Column::TaskArtifactId.eq(task_artifact_id))
        .order_by_desc(loop_validation_run::Column::Id)
        .one(conn)
        .await?)
}
