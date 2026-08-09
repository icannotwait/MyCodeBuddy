use std::num::NonZeroU64;

use crate::model::SharedModel;
use crate::runtime::RuntimeOwner;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    SetWorkspace = 1,
    CreateSession = 2,
    SelectSession = 3,
    SendUserMessage = 4,
    CancelActiveTurn = 5,
    GetAgentSettings = 6,
    SetAgentSettings = 7,
    ProbeAgent = 8,
}

pub(crate) enum CommandPayload {
    Empty,
    Utf8(Vec<u8>),
    SelectSession(i32),
    AgentSettings {
        agent: Vec<u8>,
        json: Vec<u8>,
    },
    Blocked,
    #[cfg(test)]
    Error(String),
    #[cfg(test)]
    Panic,
}

pub(crate) struct RuntimeCommand {
    pub request_id: NonZeroU64,
    pub selection_epoch: u64,
    pub op: Operation,
    pub payload: CommandPayload,
}

pub(crate) fn enqueue(
    runtime: &RuntimeOwner,
    model: &SharedModel,
    op: Operation,
    payload: CommandPayload,
) -> Result<NonZeroU64, i32> {
    runtime.enqueue(model, op, payload)
}
