use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_outbox_events")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: String,
    pub workflow_id: String,
    pub graph_revision: i64,
    pub event_kind: String,
    pub subject_key: String,
    #[sea_orm(column_type = "Text")]
    pub payload_json: String,
    pub dispatch_attempts: i64,
    pub created_at: DateTimeUtc,
    pub delivered_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
