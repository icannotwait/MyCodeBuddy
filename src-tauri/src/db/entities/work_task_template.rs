use sea_orm::entity::prelude::*;

/// Reusable work-task blueprint: a display name plus the title seed and the
/// same JSON `WorkTaskConfig` a task row stores (prompt blocks, display text,
/// optional agent override). Global — the target folder is chosen when a task
/// is created from the template. Hard-deleted; nothing references templates.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_template")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub title: String,
    #[sea_orm(column_type = "Text")]
    pub config: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
