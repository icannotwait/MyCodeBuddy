use sea_orm::entity::prelude::*;

/// Per-folder work-task defaults (one row per folder, lazily created on first
/// save). `config` holds a JSON `WorkTaskFolderSettings`: default agent +
/// mode/config values, auto_process, max_concurrent, merge strategy, and the
/// delete-worktree-after-merge default. Dies with the folder — every read joins
/// the live folder row.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "work_task_settings")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    #[sea_orm(unique)]
    pub folder_id: i32,
    #[sea_orm(column_type = "Text")]
    pub config: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::folder::Entity",
        from = "Column::FolderId",
        to = "super::folder::Column::Id"
    )]
    Folder,
}

impl Related<super::folder::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Folder.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
