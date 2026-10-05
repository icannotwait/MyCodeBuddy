//! Pure projection of an already loaded room aggregate.
//!
//! The hash covers [`ProjectionBodyV1`] only. The store assigns the id and
//! still has to prove that message and evidence rows exist.

use crate::canonical::canonical_bytes;
use crate::{
    Hash256, InternalReason, ManifestId, MessageId, ProjectionBodyV1, ProjectionId, ProjectionRef,
    ProjectionV1, PublishedMessageRef, RtError, RtResult,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomAggregate {
    pub projection_id: ProjectionId,
    pub body: ProjectionBodyV1,
    pub messages: Vec<PublishedMessageRef>,
    pub evidence_manifests: Vec<ManifestId>,
    pub required_message_ids: Vec<MessageId>,
    pub required_evidence_manifests: Vec<ManifestId>,
}

pub fn project(state: &RoomAggregate) -> RtResult<ProjectionV1> {
    for required in &state.required_message_ids {
        if !state
            .messages
            .iter()
            .any(|message| message.message_id == *required)
        {
            return Err(RtError::from_reason(InternalReason::MissingField));
        }
    }
    for required in &state.required_evidence_manifests {
        if !state
            .evidence_manifests
            .iter()
            .any(|manifest| manifest == required)
        {
            return Err(RtError::from_reason(InternalReason::MissingField));
        }
    }
    let body_bytes = canonical_bytes(&state.body)?;
    Ok(ProjectionV1 {
        projection_ref: ProjectionRef {
            id: state.projection_id,
            hash: Hash256::sha256(&body_bytes),
        },
        body: state.body.clone(),
    })
}
