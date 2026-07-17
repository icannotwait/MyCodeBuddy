use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Why an external agent session was registered as internal-only.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum InternalAgentSessionPurpose {
    #[sea_orm(string_value = "title")]
    Title,
}

/// Registry of external agent sessions spawned for internal purposes (e.g.
/// automatic title generation). Keyed by `(agent_type, external_id)`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "internal_agent_sessions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub agent_type: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub external_id: String,
    pub purpose: InternalAgentSessionPurpose,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
