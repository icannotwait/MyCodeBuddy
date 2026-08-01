use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_manifest_revisions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub manifest_revision: i64,
    pub manifest_state: String,
    #[sea_orm(column_type = "Text")]
    pub document_json: String,
    pub document_digest: String,
    pub revision_kind: Option<String>,
    pub source_manifest_revision: Option<i64>,
    pub recovery_authorization_id: Option<String>,
    pub transition_reason_code: Option<String>,
    pub consumer_correlation_id: Option<String>,
    pub graph_revision: Option<i64>,
    pub recovery_source_state_fingerprint: Option<String>,
    pub recovery_risk_class: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
