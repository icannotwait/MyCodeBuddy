//! In-memory completion barrier.
//!
//! Closing MCP admission captures the already admitted handlers while the gate
//! is held, then releases the gate before any wait. A prompt response is not
//! enough. `complete_turn` requires a normal finish, one staged candidate, and
//! a converged barrier. Nothing here waits for a network, a process, or an
//! MCP reply. This is not a product gate.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::budget::invalid_state;
use crate::model::{
    AttemptId, BindingId, CandidateReceipt, CandidateState, Fence, IncarnationId, InternalReason,
    RtError, RtResult, RuntimeTurnCompleted, Seq, ToolBarrierV1,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HandlerId(String);

impl HandlerId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishKind {
    Normal,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateDisposition {
    Applied,
    Dropped,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpUpdate {
    pub seq: u64,
    pub attempt_id: AttemptId,
    pub binding_id: BindingId,
    pub incarnation: IncarnationId,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionBarrier {
    pub pending_tools: BTreeSet<HandlerId>,
    pub ingress_watermark: u64,
    pub ingress_applied: u64,
    pub mcp_closed: bool,
}

#[derive(Debug, Default, Clone)]
pub struct BindingLedger {
    retired: BTreeSet<BindingId>,
}

impl BindingLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reusable(&self, binding: &BindingId) -> bool {
        !self.retired.contains(binding)
    }

    fn retire(&mut self, binding: BindingId) {
        self.retired.insert(binding);
    }
}

#[derive(Debug, Clone)]
struct HandlerProgress {
    finished: bool,
}

#[derive(Debug, Clone)]
pub struct CompletionState {
    fence: Fence,
    finish: Option<FinishKind>,
    sealed: Option<CandidateReceipt>,
    mcp_open: bool,
    handlers: BTreeMap<HandlerId, HandlerProgress>,
    applied_through: u64,
    buffered: BTreeSet<u64>,
    prompt_response: Option<u64>,
    binding_retired: bool,
}

impl CompletionState {
    pub fn open(fence: Fence) -> Self {
        Self {
            fence,
            finish: None,
            sealed: None,
            mcp_open: true,
            handlers: BTreeMap::new(),
            applied_through: 0,
            buffered: BTreeSet::new(),
            prompt_response: None,
            binding_retired: false,
        }
    }

    pub fn fence(&self) -> &Fence {
        &self.fence
    }

    pub fn binding_retired(&self) -> bool {
        self.binding_retired
    }

    pub fn claim(ledger: &BindingLedger, binding: &BindingId) -> RtResult<()> {
        if ledger.reusable(binding) {
            Ok(())
        } else {
            Err(invalid_state("binding_retired"))
        }
    }

    pub fn admit_handler(&mut self, handler: HandlerId) -> RtResult<()> {
        if !self.mcp_open {
            return Err(RtError::from_reason(InternalReason::AttemptClosed));
        }
        if self.handlers.contains_key(&handler) {
            return Err(invalid_state("handler_already_admitted"));
        }
        self.handlers
            .insert(handler, HandlerProgress { finished: false });
        Ok(())
    }

    pub fn finish_handler(&mut self, handler: &HandlerId) {
        if let Some(progress) = self.handlers.get_mut(handler) {
            progress.finished = true;
        }
    }

    pub fn note_prompt_response(&mut self, seq: u64) {
        self.prompt_response = Some(seq);
    }

    pub fn set_normal_finish(&mut self) {
        self.finish = Some(FinishKind::Normal);
    }

    pub fn set_finish(&mut self, kind: FinishKind) {
        self.finish = Some(kind);
    }

    pub fn clear_candidate(&mut self) {
        self.sealed = None;
    }

    pub fn stage_candidate(&mut self, receipt: CandidateReceipt) -> RtResult<()> {
        if receipt.state != CandidateState::Staged {
            return Err(invalid_state("candidate_not_staged"));
        }
        if let Some(existing) = &self.sealed {
            if existing == &receipt {
                return Ok(());
            }
            return Err(RtError::from_reason(InternalReason::ResultAlreadySealed));
        }
        self.sealed = Some(receipt);
        Ok(())
    }

    pub fn apply_update(
        &mut self,
        update: AcpUpdate,
        ledger: &mut BindingLedger,
    ) -> UpdateDisposition {
        let attributed = update.attempt_id == self.fence.attempt_id
            && update.binding_id == self.fence.binding_id
            && update.incarnation == self.fence.incarnation;
        if !attributed {
            if update.cancelled {
                self.binding_retired = true;
                ledger.retire(self.fence.binding_id);
                return UpdateDisposition::Retired;
            }
            return UpdateDisposition::Dropped;
        }
        self.observe_seq(update.seq);
        UpdateDisposition::Applied
    }

    fn observe_seq(&mut self, seq: u64) {
        if seq <= self.applied_through {
            return;
        }
        self.buffered.insert(seq);
        while self
            .buffered
            .remove(&(self.applied_through.saturating_add(1)))
        {
            self.applied_through = self.applied_through.saturating_add(1);
        }
    }
}

pub fn close_mcp_admission(
    state: &mut CompletionState,
    ingress_watermark: u64,
) -> CompletionBarrier {
    state.mcp_open = false;
    let pending_tools = state
        .handlers
        .iter()
        .filter(|(_, progress)| !progress.finished)
        .map(|(id, _)| id.clone())
        .collect();
    CompletionBarrier {
        pending_tools,
        ingress_watermark,
        ingress_applied: state.applied_through,
        mcp_closed: true,
    }
}

pub fn refresh_barrier(state: &CompletionState, barrier: &mut CompletionBarrier) {
    barrier
        .pending_tools
        .retain(|id| match state.handlers.get(id) {
            Some(progress) => !progress.finished,
            None => true,
        });
    barrier.ingress_applied = state.applied_through;
}

pub fn late_submit(state: &CompletionState, handler: &HandlerId) -> RtResult<()> {
    if state.handlers.contains_key(handler) {
        return Ok(());
    }
    if !state.mcp_open {
        return Err(RtError::from_reason(InternalReason::AttemptClosed));
    }
    Err(invalid_state("not_admitted"))
}

pub fn complete_turn(
    state: &CompletionState,
    barrier: &CompletionBarrier,
) -> RtResult<RuntimeTurnCompleted> {
    if !converged(barrier) {
        return Err(invalid_state("barrier_pending"));
    }
    if state.prompt_response.is_none() {
        return Err(invalid_state("response_missing"));
    }
    if state.finish != Some(FinishKind::Normal) {
        return Err(invalid_state("not_normal_finish"));
    }
    let Some(receipt) = &state.sealed else {
        return Err(invalid_state("no_submission"));
    };
    if receipt.state != CandidateState::Staged {
        return Err(invalid_state("candidate_not_staged"));
    }
    Ok(RuntimeTurnCompleted {
        fence: state.fence.clone(),
        finish_reason: "normal".to_string(),
        ingress_watermark: Seq(barrier.ingress_watermark),
        tool_barrier: ToolBarrierV1 { drained: true },
        candidate_id: Some(receipt.candidate_id.clone()),
    })
}

fn converged(barrier: &CompletionBarrier) -> bool {
    barrier.mcp_closed
        && barrier.pending_tools.is_empty()
        && barrier.ingress_applied >= barrier.ingress_watermark
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateTraceEvent {
    Acquired,
    ClosedMcp,
    CapturedHandlers,
    Released,
    Waited,
}

#[derive(Debug, Clone)]
struct GateFlag(Arc<Mutex<bool>>);

impl GateFlag {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(false)))
    }

    fn held(&self) -> bool {
        *self
            .0
            .try_lock()
            .expect("completion gate mutex is never held across a poll")
    }

    fn enter(&self) {
        let mut held = self
            .0
            .try_lock()
            .expect("completion gate mutex is never held across a poll");
        assert!(!*held, "completion close already holds the gate");
        *held = true;
    }

    fn leave(&self) {
        let mut held = self
            .0
            .try_lock()
            .expect("completion gate mutex is never held across a poll");
        assert!(*held, "completion close does not hold the gate");
        *held = false;
    }
}

#[derive(Debug, Clone)]
pub struct CompletionGate {
    flag: GateFlag,
}

impl CompletionGate {
    pub fn new() -> Self {
        Self {
            flag: GateFlag::new(),
        }
    }

    pub fn held(&self) -> bool {
        self.flag.held()
    }

    fn enter(&self) {
        self.flag.enter();
    }

    fn leave(&self) {
        self.flag.leave();
    }
}

impl Default for CompletionGate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct CompletionWait {
    flag: GateFlag,
    barrier: CompletionBarrier,
    waiting: bool,
    waited_inside_gate: bool,
    trace: Vec<GateTraceEvent>,
    /// `gate.held()` immediately after MCP close, before the gate is released.
    sample: bool,
}

impl CompletionWait {
    pub fn start(gate: &mut CompletionGate, state: &mut CompletionState, watermark: u64) -> Self {
        let mut trace = Vec::new();
        gate.enter();
        trace.push(GateTraceEvent::Acquired);
        let barrier = close_mcp_admission(state, watermark);
        let sample = gate.held();
        if sample {
            trace.push(GateTraceEvent::ClosedMcp);
            trace.push(GateTraceEvent::CapturedHandlers);
        }
        // Waited is recorded only after leave, once the flag is false.
        gate.leave();
        trace.push(GateTraceEvent::Released);
        let waiting = !converged(&barrier);
        Self {
            flag: gate.flag.clone(),
            barrier,
            waiting,
            waited_inside_gate: false,
            trace,
            sample,
        }
    }

    pub fn barrier(&self) -> &CompletionBarrier {
        &self.barrier
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting
    }

    pub fn gate_held(&self) -> bool {
        self.flag.held()
    }

    /// Whether the gate was held at MCP close, before release.
    pub fn sample(&self) -> bool {
        self.sample
    }

    pub fn waited_inside_gate(&self) -> bool {
        self.waited_inside_gate
    }

    pub fn trace(&self) -> &[GateTraceEvent] {
        &self.trace
    }

    pub fn poll(&mut self, state: &CompletionState) -> bool {
        if self.gate_held() {
            self.waited_inside_gate = true;
        }
        refresh_barrier(state, &mut self.barrier);
        self.trace.push(GateTraceEvent::Waited);
        self.waiting = !converged(&self.barrier);
        self.waiting
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorEffect {
    Replied(HandlerId),
    Stopped,
}

#[derive(Debug)]
enum ActorMessage {
    Reply(HandlerId),
    Stop,
}

#[derive(Debug)]
pub struct Actor {
    inbox: VecDeque<ActorMessage>,
    replies: usize,
    stops: u64,
    flag: GateFlag,
}

impl Actor {
    pub fn new(gate: &CompletionGate) -> Self {
        Self {
            inbox: VecDeque::new(),
            replies: 0,
            stops: 0,
            flag: gate.flag.clone(),
        }
    }

    /// Reads the completion gate flag shared with close and wait.
    pub fn gate_held(&self) -> bool {
        self.flag.held()
    }

    pub fn request_reply(&mut self, handler: HandlerId) {
        self.inbox.push_back(ActorMessage::Reply(handler));
    }

    pub fn request_stop(&mut self) {
        self.inbox.push_back(ActorMessage::Stop);
    }

    pub fn handle_one(&mut self) -> Option<ActorEffect> {
        match self.inbox.pop_front() {
            Some(ActorMessage::Reply(handler)) => {
                self.replies = self.replies.checked_add(1).expect("replies");
                Some(ActorEffect::Replied(handler))
            }
            Some(ActorMessage::Stop) => {
                self.stops = self.stops.checked_add(1).expect("stops");
                Some(ActorEffect::Stopped)
            }
            None => None,
        }
    }

    pub fn stops_handled(&self) -> u64 {
        self.stops
    }

    pub fn reply_count(&self) -> usize {
        self.replies
    }
}
