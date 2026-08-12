use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "simple_workflows")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub parent_conversation_id: i32,
    pub plan_rel_path: String,
    pub progress_rel_path: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conversation::Entity",
        from = "Column::ParentConversationId",
        to = "super::conversation::Column::Id",
        on_delete = "Cascade"
    )]
    ParentConversation,
}

impl Related<super::conversation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ParentConversation.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
