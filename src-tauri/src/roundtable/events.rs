//! Private room subscriptions. Global session channels never carry room frames.

use std::collections::BTreeMap;

use roundtable_protocol::{
    ActorContext, DurableEnvelopeV1, ErrorCode, Hash256, ProjectionV1, RtError, RtResult, SpeakerId,
};

use super::authorization::{authorize_room, RoomDirectory};

#[derive(Clone, Debug, Default)]
pub struct SubscriptionHub {
    frames: BTreeMap<String, Vec<String>>,
    previews: BTreeMap<String, (u64, u64)>,
    incarnation: u64,
}

impl SubscriptionHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attach(
        &mut self,
        actor: &ActorContext,
        room: &roundtable_protocol::RoomId,
        directory: &RoomDirectory,
        sink: &str,
    ) -> RtResult<()> {
        authorize_room(actor, room, directory)?;
        self.frames.entry(sink.to_string()).or_default();
        Ok(())
    }

    pub fn detach(&mut self, sink: &str) {
        self.frames.remove(sink);
    }

    pub fn publish(&mut self, sink: &str, frame: String) {
        if let Some(frames) = self.frames.get_mut(sink) {
            frames.push(frame);
        }
    }

    pub fn frames_for(&self, sink: &str) -> Vec<String> {
        self.frames.get(sink).cloned().unwrap_or_default()
    }

    pub fn note_preview(&mut self, speaker: &str, first: u64, last: u64) {
        self.previews.insert(speaker.to_string(), (first, last));
    }

    pub fn preview_bounds(&self, speaker: &str) -> Option<(u64, u64)> {
        self.previews.get(speaker).copied()
    }

    pub fn admit_incarnation(&mut self, incarnation: u64) -> bool {
        if incarnation < self.incarnation {
            return false;
        }
        self.incarnation = incarnation;
        true
    }
}

pub fn moderator_preview_allowed(speaker: Option<&SpeakerId>, participant: Option<&str>) -> bool {
    speaker.is_some() && participant.is_none()
}

pub fn apply_projection(
    current_seq: &mut u64,
    envelope: &DurableEnvelopeV1,
    fetched: &ProjectionV1,
) -> RtResult<ProjectionV1> {
    if envelope.schema_version != 1
        || envelope.object_ref.content_hash != fetched.projection_ref.hash
    {
        return Err(RtError {
            code: ErrorCode::InvalidState,
            message: "projection cause is missing".to_string(),
            retryable: false,
            current_revision: None,
            details: roundtable_protocol::ErrorDetails {
                reason: Some("unknown_event".to_string()),
                field_errors: Vec::new(),
            },
        });
    }
    *current_seq = current_seq.saturating_add(1);
    Ok(fetched.clone())
}

pub fn projection_hash(projection: &ProjectionV1) -> Hash256 {
    projection.projection_ref.hash
}

/// Shared by private WebSocket and targeted desktop delivery.
pub fn advance_private_watermark(seq: &mut u64, snapshot: &serde_json::Value) -> RtResult<bool> {
    let next = serde_json::from_value::<roundtable_protocol::Seq>(
        snapshot["projection"]["body"]["last_seq"].clone(),
    )
    .map_err(|_| super::rt_error(ErrorCode::InvalidArgument, "subscription_sequence"))?
    .0;
    if next <= *seq {
        return Ok(false);
    }
    *seq = next;
    Ok(true)
}
