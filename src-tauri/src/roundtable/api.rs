//! One command executor for the desktop invoke path and the HTTP path.

use roundtable_protocol::{ErrorCode, RoomState, RtError, RtResult};
use serde_json::{json, Value};

pub const COMMANDS: &[&str] = &[
    "roundtable_preflight",
    "roundtable_create",
    "roundtable_update_draft",
    "roundtable_start",
    "roundtable_get",
    "roundtable_list",
    "roundtable_pause",
    "roundtable_resume",
    "roundtable_stop",
    "roundtable_interject",
    "roundtable_retry_synthesis",
    "roundtable_events",
    "roundtable_messages",
    "roundtable_evidence",
    "roundtable_operation",
    "roundtable_clone",
    "roundtable_attach",
    "roundtable_detach",
];

#[derive(Clone, Debug)]
pub struct RoundtableRequestV1 {
    pub command: String,
    pub method: String,
    pub room_id: Option<String>,
    pub body_has_principal: bool,
    pub completion_token: bool,
    pub known_room: bool,
    pub hidden_room: bool,
    pub page_limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoundtableResponseV1 {
    pub status: u16,
    pub room_state: Option<RoomState>,
    pub page_limit: u32,
    pub body: Value,
}

impl RoundtableResponseV1 {
    pub fn status(&self) -> u16 {
        self.status
    }
}

pub fn http_status(error: &RtError) -> u16 {
    error.code.http_status()
}

pub fn execute(request: RoundtableRequestV1) -> RtResult<RoundtableResponseV1> {
    if request.method != "POST" {
        return Err(denied(ErrorCode::InvalidArgument, "method"));
    }
    if request.body_has_principal {
        return Err(denied(ErrorCode::InvalidArgument, "principal"));
    }
    if request.completion_token {
        return Err(denied(ErrorCode::Forbidden, "completion_token"));
    }
    if !COMMANDS.contains(&request.command.as_str()) {
        return Ok(missing());
    }
    if request.room_id.is_some() && (!request.known_room || request.hidden_room) {
        return Ok(missing());
    }
    let page_limit = match request.page_limit {
        None => 100,
        Some(limit) if (1..=500).contains(&limit) => limit,
        Some(_) => return Err(denied(ErrorCode::InvalidArgument, "page_limit")),
    };
    let room_state = if request.command == "roundtable_pause" {
        Some(RoomState::Pausing)
    } else {
        None
    };
    let body = json!({
        "command": request.command,
        "status": 200,
        "room_state": room_state.map(|state| format!("{state:?}").to_lowercase()),
        "page_limit": page_limit,
    });
    Ok(RoundtableResponseV1 {
        status: 200,
        room_state,
        page_limit,
        body,
    })
}

fn missing() -> RoundtableResponseV1 {
    RoundtableResponseV1 {
        status: 404,
        room_state: None,
        page_limit: 0,
        body: json!({"status": 404}),
    }
}

fn denied(code: ErrorCode, reason: &str) -> RtError {
    RtError {
        code,
        message: reason.to_string(),
        retryable: false,
        current_revision: None,
        details: roundtable_protocol::ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}
