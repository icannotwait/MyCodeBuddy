use sea_orm::entity::prelude::*;

/// A directory the user symlinked into a workspace folder to build a
/// multi-folder workspace. `name` is the symlink's file name inside the root,
/// `target_path` the absolute path it points at (stored verbatim, resolved at
/// use time so a moved/remounted target surfaces as broken instead of silently
/// resolving somewhere else).
///
/// Doubles as the authorization record consulted by the workspace path guard —
/// see `crate::folder_links`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "folder_link")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub folder_id: i32,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub target_path: String,
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
