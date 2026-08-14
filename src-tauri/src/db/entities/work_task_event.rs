use sea_orm::entity::prelude::*;

/// Append-only progress timeline of a work task ("how it advanced"). Rows are
/// written in the SAME transaction as the state transition they record, so the
/// timeline can never diverge from the task row. Rendered directly by the task
/// detail sheet.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub task_id: i32,
    /// created / status_changed / config_effective / agent_progress /
    /// agent_verdict / merge_attempt / merge_conflict / cleanup_failed /
    /// resume_fallback / user_action / diff_stat
    pub kind: String,
    /// user | engine | agent
    pub actor: String,
    /// Kind-specific JSON payload (e.g. `{from, to}` for status_changed).
    #[sea_orm(column_type = "Text")]
    pub payload: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::work_task::Entity",
        from = "Column::TaskId",
        to = "super::work_task::Column::Id"
    )]
    Task,
}

impl Related<super::work_task::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Task.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
