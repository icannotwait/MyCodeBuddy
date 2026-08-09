use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::watch;

use crate::abi::{CodegEuiFrame, LifecycleState, CODEG_EUI_API_VERSION};
use crate::commands::Operation;
use crate::live::Projection;
use crate::perf::wall_timestamp_rfc3339;
use crate::{CODEG_EUI_COMPLETION_CAPACITY, CODEG_EUI_ERR_INTERNAL, CODEG_EUI_ERR_QUEUE_FULL};

pub const CODEG_EUI_COMPLETION_OK: u32 = CompletionStatus::Ok as u32;
pub const CODEG_EUI_COMPLETION_ERROR: u32 = CompletionStatus::Error as u32;
pub const CODEG_EUI_COMPLETION_STALE: u32 = CompletionStatus::Stale as u32;
pub const CODEG_EUI_COMPLETION_CANCELLED: u32 = CompletionStatus::Cancelled as u32;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionStatus {
    Ok = 0,
    Error = 1,
    Stale = 2,
    Cancelled = 3,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiSlice {
    pub ptr: *const u8,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiSessionSummary {
    pub conversation_id: i32,
    pub _reserved: u32,
    pub title: CodegEuiSlice,
    pub agent: CodegEuiSlice,
    pub updated_at_ms: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CodegEuiCompletion {
    pub request_id: u64,
    pub op: u32,
    pub status: u32,
    pub result_payload: CodegEuiSlice,
    pub error: CodegEuiSlice,
}

#[derive(Clone, Debug, Default)]
pub struct OwnedSessionSummary {
    pub conversation_id: i32,
    pub title: Vec<u8>,
    pub agent: Vec<u8>,
    pub updated_at_ms: i64,
}

pub(crate) enum ModelUpdate {
    Workspace {
        sessions: Vec<OwnedSessionSummary>,
    },
    Selection {
        sessions: Vec<OwnedSessionSummary>,
        connection_id: Vec<u8>,
        transcript_json: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct OwnedCompletion {
    pub request_id: NonZeroU64,
    pub op: Operation,
    pub status: CompletionStatus,
    pub result_payload: Vec<u8>,
    pub error: Vec<u8>,
}

impl OwnedCompletion {
    pub(crate) fn ok(request_id: NonZeroU64, op: Operation, payload: Vec<u8>) -> Self {
        Self {
            request_id,
            op,
            status: CompletionStatus::Ok,
            result_payload: payload,
            error: Vec::new(),
        }
    }

    pub(crate) fn error(request_id: NonZeroU64, op: Operation, error: String) -> Self {
        Self {
            request_id,
            op,
            status: CompletionStatus::Error,
            result_payload: Vec::new(),
            error: error.into_bytes(),
        }
    }
}

#[derive(Clone, Copy)]
struct AcceptedRequest {
    op: Operation,
    selection_epoch: u64,
}

#[derive(Default)]
struct CompletionLedger {
    accepted: HashSet<NonZeroU64>,
    accepted_metadata: HashMap<NonZeroU64, AcceptedRequest>,
    ready: VecDeque<OwnedCompletion>,
    reserved: usize,
}

impl CompletionLedger {
    fn reserve(
        &mut self,
        request_id: NonZeroU64,
        op: Operation,
        selection_epoch: u64,
    ) -> Result<(), i32> {
        if self.reserved >= CODEG_EUI_COMPLETION_CAPACITY {
            return Err(CODEG_EUI_ERR_QUEUE_FULL);
        }
        assert!(self.accepted.insert(request_id), "request ID reused");
        assert!(
            self.accepted_metadata
                .insert(
                    request_id,
                    AcceptedRequest {
                        op,
                        selection_epoch,
                    },
                )
                .is_none(),
            "request metadata reused"
        );
        self.reserved += 1;
        Ok(())
    }

    fn terminalize(
        &mut self,
        current_selection_epoch: u64,
        captured_selection_epoch: u64,
        mut completion: OwnedCompletion,
    ) {
        assert!(
            self.accepted.remove(&completion.request_id),
            "accepted request terminalized more than once"
        );
        self.accepted_metadata.remove(&completion.request_id);
        if completion.status != CompletionStatus::Cancelled
            && captured_selection_epoch != current_selection_epoch
        {
            completion.status = CompletionStatus::Stale;
        }
        self.ready.push_back(completion);
    }

    fn cancel_all(&mut self) {
        let accepted = self
            .accepted_metadata
            .iter()
            .map(|(request_id, metadata)| (*request_id, *metadata))
            .collect::<Vec<_>>();
        for (request_id, metadata) in accepted {
            self.terminalize(
                metadata.selection_epoch,
                metadata.selection_epoch,
                OwnedCompletion {
                    request_id,
                    op: metadata.op,
                    status: CompletionStatus::Cancelled,
                    result_payload: Vec::new(),
                    error: Vec::new(),
                },
            );
        }
    }

    fn commit_ready(&mut self, count: usize) {
        assert!(count <= self.ready.len(), "completion commit out of range");
        self.ready.drain(..count);
        self.reserved -= count;
    }
}

#[derive(Default)]
struct ModelState {
    selection_epoch: u64,
    sessions: Vec<OwnedSessionSummary>,
    connection_id: Vec<u8>,
    event_seq: u64,
    transcript_json: Vec<u8>,
    live_assistant: Vec<u8>,
    assistant_generation: u64,
    transcript_generation: u64,
    stream_active: bool,
    needs_resync: bool,
    error_strip: Vec<u8>,
    t0_ns: u64,
    t_first_token_ns: u64,
    t_end_ns: u64,
    ledger: CompletionLedger,
}

#[derive(Clone)]
pub struct SharedModel {
    state: Arc<Mutex<ModelState>>,
    selection_tx: watch::Sender<u64>,
}

impl Default for SharedModel {
    fn default() -> Self {
        let (selection_tx, _) = watch::channel(0);
        Self {
            state: Arc::new(Mutex::new(ModelState::default())),
            selection_tx,
        }
    }
}

impl SharedModel {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, ModelState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn selection_epoch(&self) -> u64 {
        self.lock().selection_epoch
    }

    pub(crate) fn reserve(
        &self,
        request_id: NonZeroU64,
        op: Operation,
        selection_epoch: u64,
    ) -> Result<(), i32> {
        let mut state = self.lock();
        let changes_selection = op.changes_selection();
        let captured_epoch = if changes_selection {
            state
                .selection_epoch
                .checked_add(1)
                .ok_or(CODEG_EUI_ERR_INTERNAL)?
        } else {
            selection_epoch
        };
        state.ledger.reserve(request_id, op, captured_epoch)?;
        if changes_selection {
            state.selection_epoch = captured_epoch;
            if op == Operation::SetWorkspace {
                state.sessions.clear();
            }
            state.connection_id.clear();
            state.event_seq = 0;
            state.transcript_json.clear();
            state.live_assistant.clear();
            state.assistant_generation = 0;
            state.transcript_generation = 0;
            state.stream_active = false;
            state.needs_resync = false;
            state.t0_ns = 0;
            state.t_first_token_ns = 0;
            state.t_end_ns = 0;
            self.selection_tx.send_replace(captured_epoch);
        }
        Ok(())
    }

    pub(crate) fn terminalize(&self, captured_selection_epoch: u64, completion: OwnedCompletion) {
        let _ = self.terminalize_with_update(captured_selection_epoch, completion, None);
    }

    pub(crate) fn terminalize_with_update(
        &self,
        captured_selection_epoch: u64,
        completion: OwnedCompletion,
        update: Option<ModelUpdate>,
    ) -> bool {
        let mut state = self.lock();
        let current_selection_epoch = state.selection_epoch;
        let is_current = captured_selection_epoch == current_selection_epoch;
        if is_current {
            match update {
                Some(ModelUpdate::Workspace { sessions }) => {
                    state.sessions = sessions;
                }
                Some(ModelUpdate::Selection {
                    sessions,
                    connection_id,
                    transcript_json,
                }) => {
                    state.sessions = sessions;
                    state.connection_id = connection_id;
                    state.transcript_json = transcript_json;
                    state.transcript_generation = state.transcript_generation.saturating_add(1);
                }
                None => {}
            }
        }
        state.ledger.terminalize(
            current_selection_epoch,
            captured_selection_epoch,
            completion,
        );
        is_current
    }

    pub(crate) fn cancel_all(&self) {
        self.lock().ledger.cancel_all();
    }

    pub(crate) fn record_send_accepted(&self, t0_ns: u64) {
        let mut state = self.lock();
        state.t0_ns = t0_ns;
        state.t_first_token_ns = 0;
        state.t_end_ns = 0;
    }

    pub(crate) fn record_sent_user_turn(
        &self,
        selection_epoch: u64,
        request_id: NonZeroU64,
        text: Vec<u8>,
    ) -> bool {
        let Ok(text) = String::from_utf8(text) else {
            return false;
        };
        let mut state = self.lock();
        if state.selection_epoch != selection_epoch || state.connection_id.is_empty() {
            return false;
        }
        let mut transcript = parse_transcript(&state.transcript_json);
        let blocks = serde_json::json!([{"type": "text", "text": text}]);
        if transcript.iter().rev().any(|turn| {
            turn.get("role").and_then(serde_json::Value::as_str) == Some("user")
                && turn.get("blocks") == Some(&blocks)
        }) {
            return true;
        }
        transcript.push(serde_json::json!({
            "id": format!("eui-request-{request_id}"),
            "role": "user",
            "blocks": blocks,
            "timestamp": wall_timestamp_rfc3339(),
        }));
        let Ok(bytes) = serde_json::to_vec(&transcript) else {
            return false;
        };
        state.transcript_json = bytes;
        state.transcript_generation = state.transcript_generation.saturating_add(1);
        true
    }

    pub(crate) fn selection_receiver(&self) -> watch::Receiver<u64> {
        self.selection_tx.subscribe()
    }

    pub(crate) fn seed_projection(
        &self,
        selection_epoch: u64,
        connection_id: &str,
        projection: &mut Projection,
    ) -> bool {
        let mut state = self.lock();
        if state.selection_epoch != selection_epoch {
            return false;
        }
        if state.connection_id.is_empty() {
            state.connection_id = connection_id.as_bytes().to_vec();
        } else if state.connection_id.as_slice() != connection_id.as_bytes() {
            return false;
        }
        projection
            .transcript_json
            .clone_from(&state.transcript_json);
        projection.assistant_generation = state.assistant_generation;
        projection.transcript_generation = state.transcript_generation;
        true
    }

    pub(crate) fn sync_projection_transcript(
        &self,
        selection_epoch: u64,
        connection_id: &str,
        projection: &mut Projection,
    ) -> bool {
        let state = self.lock();
        if state.selection_epoch != selection_epoch
            || state.connection_id.as_slice() != connection_id.as_bytes()
        {
            return false;
        }
        if state.transcript_generation > projection.transcript_generation {
            projection
                .transcript_json
                .clone_from(&state.transcript_json);
            projection.transcript_generation = state.transcript_generation;
        }
        true
    }

    pub(crate) fn apply_live_projection(
        &self,
        selection_epoch: u64,
        projection: &Projection,
        observed_at_ns: u64,
    ) -> bool {
        let mut state = self.lock();
        if state.selection_epoch != selection_epoch
            || state.connection_id.as_slice() != projection.connection_id.as_bytes()
        {
            return false;
        }
        state.event_seq = projection.event_seq;
        state.live_assistant = projection.live_assistant.as_bytes().to_vec();
        state.stream_active = projection.stream_active;
        state.needs_resync = projection.needs_resync;
        state.error_strip = projection.error_strip.as_bytes().to_vec();
        state.assistant_generation = projection.assistant_generation;
        if projection.transcript_generation >= state.transcript_generation {
            state
                .transcript_json
                .clone_from(&projection.transcript_json);
            state.transcript_generation = projection.transcript_generation;
        }
        if state.t0_ns != 0 && state.t_first_token_ns == 0 {
            if projection.t_first_token_ns != 0 {
                state.t_first_token_ns = projection.t_first_token_ns.max(state.t0_ns);
            } else if !projection.live_assistant.is_empty() {
                state.t_first_token_ns = observed_at_ns;
            }
        }
        if projection.t_end_ns >= state.t0_ns && projection.t_end_ns != 0 {
            state.t_end_ns = projection.t_end_ns;
        }
        true
    }

    pub(crate) fn set_live_error(
        &self,
        selection_epoch: u64,
        connection_id: &str,
        message: String,
        ended_at_ns: u64,
    ) -> bool {
        let mut state = self.lock();
        if state.selection_epoch != selection_epoch
            || state.connection_id.as_slice() != connection_id.as_bytes()
        {
            return false;
        }
        state.error_strip = message.into_bytes();
        state.stream_active = false;
        state.t_end_ns = ended_at_ns;
        true
    }

    pub fn set_error_strip(&self, message: Vec<u8>) {
        self.lock().error_strip = message;
    }

    pub(crate) fn build_frame(
        &self,
        stopping: bool,
        worker_quiesced: &AtomicBool,
    ) -> (OwnedFrame, bool) {
        let mut state = self.lock();
        let shutdown_ready =
            stopping && worker_quiesced.load(Ordering::Acquire) && state.ledger.accepted.is_empty();
        let snapshot = ModelSnapshot {
            selection_epoch: state.selection_epoch,
            sessions: state.sessions.clone(),
            connection_id: state.connection_id.clone(),
            event_seq: state.event_seq,
            transcript_json: state.transcript_json.clone(),
            live_assistant: state.live_assistant.clone(),
            stream_active: state.stream_active,
            needs_resync: state.needs_resync,
            error_strip: state.error_strip.clone(),
            completions: state.ledger.ready.iter().cloned().collect(),
            t0_ns: state.t0_ns,
            t_first_token_ns: state.t_first_token_ns,
            t_end_ns: state.t_end_ns,
        };
        let completion_count = snapshot.completions.len();
        let frame = OwnedFrame::new(snapshot);
        state.ledger.commit_ready(completion_count);
        (frame, shutdown_ready)
    }
}

fn parse_transcript(bytes: &[u8]) -> Vec<serde_json::Value> {
    if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}

struct ModelSnapshot {
    selection_epoch: u64,
    sessions: Vec<OwnedSessionSummary>,
    connection_id: Vec<u8>,
    event_seq: u64,
    transcript_json: Vec<u8>,
    live_assistant: Vec<u8>,
    stream_active: bool,
    needs_resync: bool,
    error_strip: Vec<u8>,
    completions: Vec<OwnedCompletion>,
    t0_ns: u64,
    t_first_token_ns: u64,
    t_end_ns: u64,
}

pub(crate) struct OwnedFrame {
    selection_epoch: u64,
    _sessions: Vec<OwnedSessionSummary>,
    session_views: Vec<CodegEuiSessionSummary>,
    connection_id: Vec<u8>,
    event_seq: u64,
    transcript_json: Vec<u8>,
    live_assistant: Vec<u8>,
    stream_active: bool,
    needs_resync: bool,
    error_strip: Vec<u8>,
    _completions: Vec<OwnedCompletion>,
    completion_views: Vec<CodegEuiCompletion>,
    t0_ns: u64,
    t_first_token_ns: u64,
    t_end_ns: u64,
}

// The raw pointers in the C views point only into heap allocations owned by
// this frame. Moving the frame does not move those allocations, and public
// access remains restricted to the captured UI thread.
unsafe impl Send for OwnedFrame {}

impl OwnedFrame {
    fn new(snapshot: ModelSnapshot) -> Self {
        let session_views = snapshot
            .sessions
            .iter()
            .map(|session| CodegEuiSessionSummary {
                conversation_id: session.conversation_id,
                _reserved: 0,
                title: slice(&session.title),
                agent: slice(&session.agent),
                updated_at_ms: session.updated_at_ms,
            })
            .collect();
        let completion_views = snapshot
            .completions
            .iter()
            .map(|completion| CodegEuiCompletion {
                request_id: completion.request_id.get(),
                op: completion.op as u32,
                status: completion.status as u32,
                result_payload: slice(&completion.result_payload),
                error: slice(&completion.error),
            })
            .collect();

        Self {
            selection_epoch: snapshot.selection_epoch,
            _sessions: snapshot.sessions,
            session_views,
            connection_id: snapshot.connection_id,
            event_seq: snapshot.event_seq,
            transcript_json: snapshot.transcript_json,
            live_assistant: snapshot.live_assistant,
            stream_active: snapshot.stream_active,
            needs_resync: snapshot.needs_resync,
            error_strip: snapshot.error_strip,
            _completions: snapshot.completions,
            completion_views,
            t0_ns: snapshot.t0_ns,
            t_first_token_ns: snapshot.t_first_token_ns,
            t_end_ns: snapshot.t_end_ns,
        }
    }

    pub(crate) fn as_abi(
        &self,
        lifecycle: LifecycleState,
        generation: u64,
        shutdown_ready: bool,
    ) -> CodegEuiFrame {
        CodegEuiFrame {
            api_version: CODEG_EUI_API_VERSION,
            lifecycle_state: lifecycle as u32,
            generation,
            selection_epoch: self.selection_epoch,
            sessions: ptr_or_null(&self.session_views),
            sessions_len: self.session_views.len(),
            connection_id: slice(&self.connection_id),
            event_seq: self.event_seq,
            transcript_json: slice(&self.transcript_json),
            live_assistant: slice(&self.live_assistant),
            stream_active: u8::from(self.stream_active),
            needs_resync: u8::from(self.needs_resync),
            shutdown_ready: u8::from(shutdown_ready),
            _reserved: [0; 5],
            error_strip: slice(&self.error_strip),
            completions: ptr_or_null(&self.completion_views),
            completions_len: self.completion_views.len(),
            t0_ns: self.t0_ns,
            t_first_token_ns: self.t_first_token_ns,
            t_end_ns: self.t_end_ns,
        }
    }
}

fn slice(bytes: &[u8]) -> CodegEuiSlice {
    CodegEuiSlice {
        ptr: if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr()
        },
        len: bytes.len(),
    }
}

fn ptr_or_null<T>(values: &[T]) -> *const T {
    if values.is_empty() {
        std::ptr::null()
    } else {
        values.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::sync::atomic::AtomicBool;

    use super::{CompletionStatus, OwnedCompletion, SharedModel};
    use crate::abi::LifecycleState;
    use crate::commands::Operation;
    use crate::live::Projection;

    #[test]
    fn accepted_workspace_and_session_changes_advance_the_selection_epoch() {
        let model = SharedModel::new();

        model
            .reserve(
                NonZeroU64::new(1).unwrap(),
                Operation::SetWorkspace,
                model.selection_epoch(),
            )
            .unwrap();
        assert_eq!(model.selection_epoch(), 1);

        model
            .reserve(
                NonZeroU64::new(2).unwrap(),
                Operation::CreateSession,
                model.selection_epoch(),
            )
            .unwrap();
        assert_eq!(model.selection_epoch(), 2);

        model
            .reserve(
                NonZeroU64::new(3).unwrap(),
                Operation::SelectSession,
                model.selection_epoch(),
            )
            .unwrap();
        assert_eq!(model.selection_epoch(), 3);
    }

    #[test]
    fn selection_changes_mark_one_terminal_completion_stale() {
        let model = SharedModel::new();
        let request_id = NonZeroU64::new(1).unwrap();
        model
            .reserve(request_id, Operation::SendUserMessage, 0)
            .unwrap();
        model.lock().selection_epoch = 1;
        model.terminalize(
            0,
            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
        );

        assert_eq!(
            model.lock().ledger.ready.front().unwrap().status,
            CompletionStatus::Stale
        );
    }

    #[test]
    #[should_panic(expected = "accepted request terminalized more than once")]
    fn duplicate_terminalization_is_rejected() {
        let model = SharedModel::new();
        let request_id = NonZeroU64::new(1).unwrap();
        model
            .reserve(request_id, Operation::SendUserMessage, 0)
            .unwrap();
        model.terminalize(
            0,
            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
        );
        model.terminalize(
            0,
            OwnedCompletion::ok(request_id, Operation::SendUserMessage, Vec::new()),
        );
    }

    #[test]
    fn live_markers_and_resync_visibility_are_frame_backed() {
        let model = SharedModel::new();
        model.lock().connection_id = b"c1".to_vec();
        model.record_send_accepted(100);
        let mut projection = Projection {
            connection_id: "c1".to_string(),
            event_seq: 1,
            live_assistant: "hello".to_string(),
            assistant_generation: 1,
            stream_active: true,
            ..Projection::default()
        };

        assert!(model.apply_live_projection(0, &projection, 150));
        let (first, _) = model.build_frame(false, &AtomicBool::new(false));
        let first = first.as_abi(LifecycleState::Running, 1, false);
        assert_eq!(first.t_first_token_ns, 150);
        assert_eq!(first.t_end_ns, 0);

        projection.needs_resync = true;
        assert!(model.apply_live_projection(0, &projection, 160));
        let (resyncing, _) = model.build_frame(false, &AtomicBool::new(false));
        let resyncing = resyncing.as_abi(LifecycleState::Running, 2, false);
        assert_eq!(resyncing.needs_resync, 1);
        assert_eq!(resyncing.t_first_token_ns, 150);

        projection.needs_resync = false;
        projection.event_seq = 2;
        projection.t_end_ns = 200;
        projection.stream_active = false;
        assert!(model.apply_live_projection(0, &projection, 200));
        let (complete, _) = model.build_frame(false, &AtomicBool::new(false));
        let complete = complete.as_abi(LifecycleState::Running, 3, false);
        assert_eq!(complete.needs_resync, 0);
        assert_eq!(complete.t_first_token_ns, 150);
        assert_eq!(complete.t_end_ns, 200);
    }

    #[test]
    fn old_live_projection_cannot_overwrite_a_new_selection() {
        let model = SharedModel::new();
        model.lock().connection_id = b"old".to_vec();
        let projection = Projection {
            connection_id: "old".to_string(),
            event_seq: 9,
            live_assistant: "stale".to_string(),
            assistant_generation: 1,
            ..Projection::default()
        };
        let selection_request = NonZeroU64::new(91).unwrap();

        model
            .reserve(selection_request, Operation::SetWorkspace, 0)
            .unwrap();

        assert!(!model.apply_live_projection(0, &projection, 50));
        let (frame, _) = model.build_frame(false, &AtomicBool::new(false));
        let frame = frame.as_abi(LifecycleState::Running, 1, false);
        assert_eq!(frame.event_seq, 0);
        assert_eq!(frame.live_assistant.len, 0);
    }
}
