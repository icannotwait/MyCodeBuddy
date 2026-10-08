//! Command edges that must not invent a second phase or a second speaker.

use roundtable_protocol::{ErrorCode, PhaseId, PhaseKind, RtResult};

use super::control::ControlBook;
use super::rt_error;

pub fn next_phase(book: &ControlBook, requested: PhaseKind) -> RtResult<PhaseId> {
    book.next_phase(requested)
}

pub fn pause_again(book: &mut ControlBook) -> RtResult<()> {
    book.pause_again()
}

pub fn retry_synthesis(book: &mut ControlBook, hash_changed: bool) -> RtResult<Vec<String>> {
    if hash_changed {
        return Err(rt_error(ErrorCode::InvalidState, "restart_required"));
    }
    book.retry_synthesis(false)
}
