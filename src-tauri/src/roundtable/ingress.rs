//! Private ACP ingress and the completion boundary.
//!
//! Each incarnation has one ordered ACP sequence. MCP handlers are a separate
//! channel and converge only through the admitted set captured while the gate
//! is held. The barrier wait reuses the P05 coordinator: it runs after the
//! gate is released, so the actor can still answer and accept stop. A
//! `BarrierFact` cannot accept a candidate.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};

use roundtable_protocol::{
    complete_turn, late_submit, AcpUpdate, Actor, ActorEffect, BindingId, BindingLedger,
    CandidateReceipt, CompletionBarrier, CompletionGate, CompletionState, CompletionWait,
    ErrorCode, Fence, FinishKind, GateTraceEvent, HandlerId, IncarnationId, RtResult,
    RuntimeTurnCompleted, UpdateDisposition,
};

use crate::acp::types::AcpEvent;
use crate::auto_title::ConnectionPurpose;
use crate::roundtable::runtime::PrivateRuntimeSink;

use super::rt_error;

/// Raw ACP class. This is not a derived roundtable projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressKind {
    Update,
    Permission,
    Lifecycle,
}

/// One ordered raw event for a single incarnation.
#[derive(Debug, Clone)]
pub struct RuntimeIngress {
    pub fence: Fence,
    pub turn_generation: u64,
    pub ingress_seq: u64,
    pub kind: IngressKind,
    /// The ACP event before any public projection. Not a derived summary.
    pub raw: AcpEvent,
}

/// Prompt response has become the completion marker. Closing admission is
/// separate from accepting the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionMarker {
    pub fence: Fence,
    pub turn_generation: u64,
    pub ingress_watermark: u64,
    pub prompt_response_seq: u64,
    pub finish: FinishKind,
}

/// Fence, the handler set captured at close, and the ACP watermark.
///
/// Holding a fact does not accept a candidate and does not drain the barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarrierFact {
    pub fence: Fence,
    pub admitted_handlers: BTreeSet<HandlerId>,
    pub acp_watermark: u64,
}

struct BoundRoute {
    sink: Arc<dyn PrivateRuntimeSink>,
    fence: Fence,
    turn_generation: u64,
}

pub(crate) struct PrivateRawEmit {
    pub connection_id: String,
    pub turn_generation: Option<u64>,
    pub event: AcpEvent,
}

fn routes() -> &'static Mutex<HashMap<String, BoundRoute>> {
    static ROUTES: OnceLock<Mutex<HashMap<String, BoundRoute>>> = OnceLock::new();
    ROUTES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sequences() -> &'static Mutex<HashMap<IncarnationId, u64>> {
    static SEQUENCES: OnceLock<Mutex<HashMap<IncarnationId, u64>>> = OnceLock::new();
    SEQUENCES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn alloc_seq(incarnation: IncarnationId) -> RtResult<u64> {
    let mut map = sequences().lock().unwrap_or_else(|err| err.into_inner());
    let next = map.entry(incarnation).or_insert(1);
    let seq = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "ingress_overflow"))?;
    Ok(seq)
}

fn classify(event: &AcpEvent) -> IngressKind {
    match event {
        AcpEvent::PermissionRequest { .. }
        | AcpEvent::PermissionResolved { .. }
        | AcpEvent::PermissionQueueDepth { .. } => IngressKind::Permission,
        AcpEvent::SessionStarted { .. }
        | AcpEvent::TurnComplete { .. }
        | AcpEvent::StatusChanged { .. }
        | AcpEvent::Error { .. }
        | AcpEvent::ConversationLinked { .. }
        | AcpEvent::ConversationStatusChanged { .. }
        | AcpEvent::TranscriptRolledOver { .. } => IngressKind::Lifecycle,
        _ => IngressKind::Update,
    }
}

pub(crate) fn is_roundtable_purpose(purpose: ConnectionPurpose) -> bool {
    purpose == ConnectionPurpose::Roundtable
}

pub(crate) fn route_attribution(connection_id: &str) -> Option<(Fence, u64)> {
    let routes = routes().lock().unwrap_or_else(|err| err.into_inner());
    routes
        .get(connection_id)
        .map(|route| (route.fence.clone(), route.turn_generation))
}

/// Bind one incarnation's ordered ACP ingress. Replacing a connection keeps
/// the incarnation's existing sequence.
pub fn bind_private_ingress(
    connection_id: impl Into<String>,
    sink: Arc<dyn PrivateRuntimeSink>,
    fence: Fence,
    turn_generation: u64,
) {
    let mut routes = routes().lock().unwrap_or_else(|err| err.into_inner());
    routes.insert(
        connection_id.into(),
        BoundRoute {
            sink,
            fence,
            turn_generation,
        },
    );
}

/// Push one raw event onto the incarnation's single sequence.
///
/// A roundtable session with no bound route is dropped. Callers must not
/// publish it on the legacy bus or the global web broadcast.
pub(crate) fn deliver_private(raw: PrivateRawEmit) -> RtResult<()> {
    let (sink, fence, bound_generation) = {
        let routes = routes().lock().unwrap_or_else(|err| err.into_inner());
        let Some(route) = routes.get(&raw.connection_id) else {
            tracing::warn!(
                connection_id = %raw.connection_id,
                "roundtable raw event dropped; no private ingress is bound"
            );
            return Ok(());
        };
        (
            Arc::clone(&route.sink),
            route.fence.clone(),
            route.turn_generation,
        )
    };
    let ingress_seq = alloc_seq(fence.incarnation)?;
    sink.push(RuntimeIngress {
        fence,
        turn_generation: raw.turn_generation.unwrap_or(bound_generation),
        ingress_seq,
        kind: classify(&raw.event),
        raw: raw.event,
    })
}

/// `None` means the event was taken by the private sink and must not be published.
pub(crate) async fn divert_if_roundtable(
    state: &Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
    event: AcpEvent,
) -> Option<AcpEvent> {
    let raw = {
        let guard = state.read().await;
        if !is_roundtable_purpose(guard.purpose) {
            return Some(event);
        }
        PrivateRawEmit {
            connection_id: guard.connection_id.clone(),
            turn_generation: guard.active_turn_generation,
            event,
        }
    };
    let _ = deliver_private(raw);
    None
}

pub(crate) async fn is_roundtable_session(
    state: &Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
) -> bool {
    is_roundtable_purpose(state.read().await.purpose)
}

pub(crate) async fn deliver_from_state(
    state: &Arc<tokio::sync::RwLock<crate::acp::SessionState>>,
    event: AcpEvent,
) {
    let raw = {
        let guard = state.read().await;
        PrivateRawEmit {
            connection_id: guard.connection_id.clone(),
            turn_generation: guard.active_turn_generation,
            event,
        }
    };
    let _ = deliver_private(raw);
}

/// Runtime completion boundary. MCP admission closes inside the P05 gate.
/// The wait, including reap of the admitted handlers, is the caller's poll
/// after the gate is released.
pub struct CompletionCoordinator {
    state: CompletionState,
    gate: CompletionGate,
    ledger: BindingLedger,
    wait: Option<CompletionWait>,
    actor: Actor,
    captured: Option<BTreeSet<HandlerId>>,
    staged: Option<CandidateReceipt>,
    applied: u64,
    buffered: BTreeSet<u64>,
    /// `CompletionWait::start` closes MCP admission before it releases the gate.
    closed_inside_gate: bool,
    service_turn: Option<u64>,
    service_failures: Vec<super::capabilities::FailureObservation>,
}

impl CompletionCoordinator {
    pub fn open(fence: Fence) -> Self {
        Self::open_with_ledger(fence, BindingLedger::new())
    }

    pub fn open_with_ledger(fence: Fence, ledger: BindingLedger) -> Self {
        let gate = CompletionGate::new();
        let actor = Actor::new(&gate);
        Self {
            state: CompletionState::open(fence),
            gate,
            ledger,
            wait: None,
            actor,
            captured: None,
            staged: None,
            applied: 0,
            buffered: BTreeSet::new(),
            closed_inside_gate: false,
            service_turn: None,
            service_failures: Vec::new(),
        }
    }

    pub fn admit_handler(&mut self, handler: HandlerId) -> RtResult<()> {
        self.state.admit_handler(handler)
    }

    pub fn finish_handler(&mut self, handler: &HandlerId) {
        self.state.finish_handler(handler);
    }

    pub fn stage_candidate(&mut self, receipt: CandidateReceipt) -> RtResult<()> {
        self.state.stage_candidate(receipt.clone())?;
        self.staged = Some(receipt);
        Ok(())
    }

    pub fn staged_candidate(&self) -> Option<&CandidateReceipt> {
        self.staged.as_ref()
    }

    pub fn apply_update(&mut self, update: AcpUpdate) -> UpdateDisposition {
        let seq = update.seq;
        let disposition = self.state.apply_update(update, &mut self.ledger);
        if disposition == UpdateDisposition::Applied {
            self.observe(seq);
        }
        disposition
    }

    pub fn applied_ingress(&self) -> u64 {
        self.applied
    }

    pub fn claim(&self, binding: &BindingId) -> RtResult<()> {
        CompletionState::claim(&self.ledger, binding)
    }

    pub fn ledger_snapshot(&self) -> BindingLedger {
        self.ledger.clone()
    }

    /// Close MCP admission and capture the admitted handlers while the gate
    /// is held, then release it. Does not wait and does not accept.
    pub fn begin(&mut self, marker: CompletionMarker) -> CompletionBarrier {
        if marker.fence != *self.state.fence() {
            return CompletionBarrier {
                pending_tools: BTreeSet::new(),
                ingress_watermark: marker.ingress_watermark,
                ingress_applied: self.applied,
                mcp_closed: false,
            };
        }
        self.service_turn = Some(marker.turn_generation);
        self.state.note_prompt_response(marker.prompt_response_seq);
        self.state.set_finish(marker.finish);
        let wait = CompletionWait::start(&mut self.gate, &mut self.state, marker.ingress_watermark);
        // start() samples the close while it still holds the gate, then releases.
        self.closed_inside_gate = true;
        self.captured = Some(wait.barrier().pending_tools.clone());
        let barrier = wait.barrier().clone();
        self.wait = Some(wait);
        barrier
    }

    pub fn late_submit(&self, handler: &HandlerId) -> RtResult<()> {
        late_submit(&self.state, handler)
    }

    pub fn gate_held(&self) -> bool {
        self.gate.held()
    }

    pub fn close_sampled_inside_gate(&self) -> bool {
        self.closed_inside_gate && !self.gate.held()
    }

    pub fn waited_inside_gate(&self) -> bool {
        self.wait
            .as_ref()
            .is_some_and(|wait| wait.waited_inside_gate())
    }

    pub fn is_waiting(&self) -> bool {
        self.wait.as_ref().is_some_and(|wait| wait.is_waiting())
    }

    pub fn gate_trace(&self) -> Vec<GateTraceEvent> {
        self.wait
            .as_ref()
            .map(|wait| wait.trace().to_vec())
            .unwrap_or_default()
    }

    pub fn request_reply(&mut self, handler: HandlerId) {
        self.actor.request_reply(handler);
    }

    pub fn request_stop(&mut self) {
        self.actor.request_stop();
    }

    pub fn handle_actor(&mut self) -> Option<ActorEffect> {
        self.actor.handle_one()
    }

    pub fn stops_handled(&self) -> u64 {
        self.actor.stops_handled()
    }

    pub fn reply_count(&self) -> usize {
        self.actor.reply_count()
    }

    /// One step of the controlled wait. The actor is not blocked by this poll.
    pub fn poll_barrier(&mut self) -> bool {
        if self.wait.is_none() {
            return true;
        }
        let state = self.state.clone();
        self.wait.as_mut().expect("barrier").poll(&state)
    }

    /// Decide the turn only after the captured handlers and ACP watermark
    /// have drained. The fact is checked, not treated as acceptance.
    pub fn on_barrier_drained(&mut self, fact: BarrierFact) -> RtResult<RuntimeTurnCompleted> {
        let manifest = super::capabilities::sealed_service_manifest();
        super::capabilities::verify_service_manifest(
            &manifest,
            &super::capabilities::ManifestExtras::default(),
        )?;
        let Some(captured) = self.captured.clone() else {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_missing"));
        };
        let Some(watermark) = self
            .wait
            .as_ref()
            .map(|wait| wait.barrier().ingress_watermark)
        else {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_missing"));
        };
        if fact.fence != *self.state.fence()
            || fact.admitted_handlers != captured
            || fact.acp_watermark != watermark
        {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_fact_mismatch"));
        }
        let state = self.state.clone();
        let waiting = self.wait.as_mut().expect("barrier").poll(&state);
        if waiting {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_pending"));
        }
        let barrier = self.wait.as_ref().expect("barrier").barrier().clone();
        let completed = complete_turn(&self.state, &barrier)?;
        debug_assert_ne!(completed.finish_reason, "accepted");
        Ok(completed)
    }

    pub fn observe_service_failure(
        &mut self,
        observation: super::capabilities::FailureObservation,
    ) {
        self.service_failures.push(observation);
    }

    /// Decide only after ingress and admitted handlers drain. Acceptance is
    /// never implied by `end_turn` or by a staged candidate.
    pub fn adjudicate_service_turn(
        &mut self,
        fact: BarrierFact,
    ) -> RtResult<super::capabilities::ServiceTurnDecision> {
        let manifest = super::capabilities::sealed_service_manifest();
        super::capabilities::verify_service_manifest(
            &manifest,
            &super::capabilities::ManifestExtras::default(),
        )?;
        let Some(captured) = self.captured.clone() else {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_missing"));
        };
        let Some(watermark) = self
            .wait
            .as_ref()
            .map(|wait| wait.barrier().ingress_watermark)
        else {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_missing"));
        };
        if fact.fence != *self.state.fence()
            || fact.admitted_handlers != captured
            || fact.acp_watermark != watermark
        {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_fact_mismatch"));
        }
        let state = self.state.clone();
        let waiting = self.wait.as_mut().expect("barrier").poll(&state);
        if waiting {
            return Err(rt_error(ErrorCode::InvalidState, "barrier_pending"));
        }
        let barrier = self.wait.as_ref().expect("barrier").barrier().clone();
        let completed = complete_turn(&self.state, &barrier);
        let staged = self
            .staged_candidate()
            .map(|receipt| receipt.candidate_id.clone());
        let turn = self.service_turn.unwrap_or(0);
        Ok(match &completed {
            Ok(done) => super::capabilities::decide_service_turn(
                &fact.fence,
                turn,
                &self.service_failures,
                Some(done),
                None,
                staged,
            ),
            Err(error) => super::capabilities::decide_service_turn(
                &fact.fence,
                turn,
                &self.service_failures,
                None,
                error.details.reason.as_deref(),
                staged,
            ),
        })
    }

    fn observe(&mut self, seq: u64) {
        if seq <= self.applied {
            return;
        }
        self.buffered.insert(seq);
        while self.buffered.remove(&(self.applied.saturating_add(1))) {
            self.applied = self.applied.saturating_add(1);
        }
    }
}
