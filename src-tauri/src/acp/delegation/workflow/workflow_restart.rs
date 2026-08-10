//! Historical completion-protocol relationship and context reads.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::db::entities::{delegation_workflow, delegation_workflow_restart_context};

use super::types::{CompletionProtocolWorkflowProjection, LegacyWorkflowLink};

/// Load an already-persisted historical request context without mutating it.
pub async fn load_historical_workflow_context<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<Option<delegation_workflow_restart_context::Model>, sea_orm::DbErr> {
    delegation_workflow_restart_context::Entity::find_by_id(conversation_id)
        .one(conn)
        .await
}

pub(crate) async fn completion_protocol_projection<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<CompletionProtocolWorkflowProjection, sea_orm::DbErr> {
    let legacy_source = match header.legacy_source_workflow_id.as_deref() {
        Some(source_id) => delegation_workflow::Entity::find_by_id(source_id)
            .one(conn)
            .await?
            .map(|source| LegacyWorkflowLink {
                workflow_id: source.workflow_id,
                conversation_id: source.parent_conversation_id,
            }),
        None => None,
    };
    let v2_successor = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(header.workflow_id.clone()))
        .one(conn)
        .await?
        .map(|successor| LegacyWorkflowLink {
            workflow_id: successor.workflow_id,
            conversation_id: successor.parent_conversation_id,
        });

    Ok(CompletionProtocolWorkflowProjection {
        version: header.completion_protocol_version,
        mode: header.completion_protocol_mode.clone(),
        creation_mode: header.completion_protocol_mode.clone(),
        legacy_source,
        v2_successor,
        read_only_reason: (header.completion_protocol_version == 1)
            .then(|| "legacy_completion_protocol_read_only".to_string()),
        // V2 attention-driven wake is emitted independently by the completion outbox.
        automatic_root_wake: false,
    })
}
