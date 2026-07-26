use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Immutable document-gate cycle outcome.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum GateSettlementOutcome {
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "changes_requested")]
    ChangesRequested,
    #[sea_orm(string_value = "blocked")]
    Blocked,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_gate_settlements")]
pub struct Model {
    /// Composite PK with gate_id + gate_cycle (matches idx_dwgs_gate_cycle).
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_cycle: i64,
    pub manifest_revision: i64,
    /// Header structural_revision at settle time (plan clock audit).
    pub structural_revision: i64,
    /// Design or plan fingerprint covered by this settlement (gate-kind-specific).
    pub content_fingerprint: String,
    pub outcome: GateSettlementOutcome,
    pub critical_count: i64,
    pub important_count: i64,
    pub minor_count: i64,
    #[sea_orm(column_type = "Text")]
    pub summary: String,
    pub graph_revision_at_settle: i64,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
