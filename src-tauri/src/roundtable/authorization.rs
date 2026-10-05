//! Room authorization. An unauthenticated sink receives nothing.

use roundtable_protocol::{ActorContext, ErrorCode, PrincipalId, RoomId, RtError, RtResult};

#[derive(Clone, Debug)]
pub struct RoomGrant {
    pub room_id: RoomId,
    pub principal: PrincipalId,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub struct RoomDirectory {
    pub rooms: Vec<RoomGrant>,
}

pub fn authorize_room(actor: &ActorContext, room: &RoomId, directory: &RoomDirectory) -> RtResult<()> {
    let grant = directory
        .rooms
        .iter()
        .find(|grant| &grant.room_id == room && grant.principal == actor.principal_id());
    if grant.is_some_and(|grant| !grant.hidden) {
        Ok(())
    } else {
        Err(RtError {
            code: ErrorCode::Forbidden,
            message: "room is not visible".to_string(),
            retryable: false,
            current_revision: None,
            details: roundtable_protocol::ErrorDetails {
                reason: None,
                field_errors: Vec::new(),
            },
        })
    }
}
