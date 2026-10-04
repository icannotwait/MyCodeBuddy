//! In-memory admission order.
//!
//! The fixed order is reserved, register incarnation, ready, hold the gate,
//! commit admitting, synchronous non-blocking `try_enqueue`, then admitted.
//! Failpoints yield at `reserved`, `registered`, `gate_acquired`,
//! `before_enqueue`, and the fake admitting commit. The mutex only guards the
//! holder flag and is never kept across a yield, so a stop cannot deadlock
//! the poller. Ready is not authorization. This is not a product gate.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::budget::{deadline_still_open, invalid_state};
use crate::model::{
    AttemptId, Epoch, Fence, IncarnationId, InternalReason, MonoMs, RtError, RtResult,
};

pub trait LocalPromptQueue {
    fn try_enqueue(&self, prompt: AdmittedPrompt) -> Result<(), QueueReject>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPrompt {
    pub fence: Fence,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueReject;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionState {
    pub fence: Fence,
    pub deadline: MonoMs,
    pub ready: bool,
    pub gate_enabled: bool,
    pub admission_closed: bool,
}

pub fn check_admission(current: &AdmissionState, fence: &Fence, now: MonoMs) -> RtResult<()> {
    if !current.gate_enabled {
        return Err(invalid_state("gate_disabled"));
    }
    if &current.fence != fence {
        return Err(RtError::from_reason(InternalReason::StaleFence));
    }
    if current.admission_closed {
        return Err(RtError::from_reason(InternalReason::AttemptClosed));
    }
    if !deadline_still_open(now, current.deadline) {
        return Err(invalid_state("deadline_reached"));
    }
    if !current.ready {
        return Err(invalid_state("not_ready"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchState {
    Reserved,
    Ready,
    Admitting,
    Admitted,
    NotDispatched,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    Reserved,
    Registered,
    GateAcquired,
    AdmittingCommit,
    BeforeEnqueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionEvent {
    Reserved,
    Registered,
    Ready,
    GateAcquired,
    AdmittingCommitted,
    Enqueued,
    Admitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomLog {
    PromptEnqueued,
    GateReleased,
    StopCommitted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionOutcome {
    pub enqueued: bool,
    pub admitting_committed: bool,
    pub prompt_consumed: bool,
    pub launch_counted: bool,
    pub ready_seen: bool,
    pub dispatch: DispatchState,
    pub error: Option<RtError>,
}

#[derive(Debug)]
pub enum TaskPoll {
    Yielded(Checkpoint),
    BlockedOnGate,
    StopCommitted,
    Completed(AdmissionOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredIncarnation {
    pub id: IncarnationId,
    pub process_returned: bool,
}

struct GateInner {
    holder: Option<u64>,
}

enum Phase {
    Start,
    Reserved,
    Registered,
    GateHeld,
    Committing,
    BeforeEnqueue,
    Done,
}

#[derive(Debug)]
struct QueueInner {
    accepted: Vec<AdmittedPrompt>,
    calls: usize,
    reject: bool,
}

#[derive(Debug)]
pub struct FakePromptQueue {
    inner: Mutex<QueueInner>,
}

impl FakePromptQueue {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(QueueInner {
                accepted: Vec::new(),
                calls: 0,
                reject: false,
            }),
        }
    }

    pub fn reject_all(&self) {
        self.inner
            .try_lock()
            .expect("queue mutex is never held across a call")
            .reject = true;
    }

    pub fn enqueued_count(&self) -> usize {
        self.inner
            .try_lock()
            .expect("queue mutex is never held across a call")
            .accepted
            .len()
    }

    pub fn try_enqueue_calls(&self) -> usize {
        self.inner
            .try_lock()
            .expect("queue mutex is never held across a call")
            .calls
    }
}

impl Default for FakePromptQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalPromptQueue for FakePromptQueue {
    fn try_enqueue(&self, prompt: AdmittedPrompt) -> Result<(), QueueReject> {
        let mut inner = self
            .inner
            .try_lock()
            .expect("queue mutex is never held across a call");
        inner.calls = inner.calls.checked_add(1).expect("enqueue calls");
        if inner.reject {
            return Err(QueueReject);
        }
        inner.accepted.push(prompt);
        Ok(())
    }
}

pub struct FakeRoom {
    now: MonoMs,
    deadline: MonoMs,
    current_fence: Option<Fence>,
    gate_enabled: bool,
    admission_closed: bool,
    stop_committed: bool,
    residual_remote_work: bool,
    resend_forbidden: bool,
    admitted_count: u64,
    admitted_at_stop: Option<u64>,
    prompts_consumed: u64,
    next_id: u64,
    decision_sample: Option<MonoMs>,
    registered: Vec<RegisteredIncarnation>,
    cleanup_targets: Vec<IncarnationId>,
    durable: BTreeMap<AttemptId, bool>,
    dispatches: BTreeMap<AttemptId, DispatchState>,
    log: Vec<RoomLog>,
    gate: Mutex<GateInner>,
}

impl FakeRoom {
    pub fn new(now: MonoMs, deadline: MonoMs) -> Self {
        Self {
            now,
            deadline,
            current_fence: None,
            gate_enabled: true,
            admission_closed: false,
            stop_committed: false,
            residual_remote_work: false,
            resend_forbidden: false,
            admitted_count: 0,
            admitted_at_stop: None,
            prompts_consumed: 0,
            next_id: 0,
            decision_sample: None,
            registered: Vec::new(),
            cleanup_targets: Vec::new(),
            durable: BTreeMap::new(),
            dispatches: BTreeMap::new(),
            log: Vec::new(),
            gate: Mutex::new(GateInner { holder: None }),
        }
    }

    pub fn start_admission(&mut self, fence: Fence, prompt: impl Into<String>) -> AdmissionTask {
        self.next_id = self.next_id.checked_add(1).expect("task id");
        if self.current_fence.is_none() {
            self.current_fence = Some(fence.clone());
        }
        AdmissionTask {
            id: self.next_id,
            fence,
            prompt: prompt.into(),
            phase: Phase::Start,
            pause_during_commit: false,
            commit_paused: false,
            ready: false,
            launch_counted: false,
            admitting_committed: false,
            events: Vec::new(),
            outcome: None,
        }
    }

    pub fn start_stop(&mut self) -> StopTask {
        self.next_id = self.next_id.checked_add(1).expect("task id");
        StopTask {
            id: self.next_id,
            done: false,
        }
    }

    pub fn deadline(&self) -> MonoMs {
        self.deadline
    }

    pub fn advance_to(&mut self, now: MonoMs) {
        assert!(now.0 >= self.now.0, "clock is monotonic");
        self.now = now;
    }

    pub fn decision_sample(&self) -> Option<MonoMs> {
        self.decision_sample
    }

    pub fn gate_held(&self) -> bool {
        self.with_gate(|gate| gate.holder.is_some())
    }

    pub fn gate_enabled(&self) -> bool {
        self.gate_enabled
    }

    pub fn disable_gate(&mut self) {
        self.gate_enabled = false;
    }

    pub fn stop_committed(&self) -> bool {
        self.stop_committed
    }

    pub fn admitted_count(&self) -> u64 {
        self.admitted_count
    }

    pub fn new_admissions_after_stop(&self) -> u64 {
        self.admitted_at_stop
            .map(|prior| self.admitted_count.saturating_sub(prior))
            .unwrap_or(0)
    }

    pub fn residual_remote_work(&self) -> bool {
        self.residual_remote_work
    }

    pub fn resend_forbidden(&self) -> bool {
        self.resend_forbidden
    }

    pub fn prompts_consumed(&self) -> u64 {
        self.prompts_consumed
    }

    pub fn dispatch(&self, attempt: &AttemptId) -> DispatchState {
        self.dispatches
            .get(attempt)
            .copied()
            .unwrap_or(DispatchState::NotDispatched)
    }

    pub fn admitting_durable(&self, attempt: &AttemptId) -> bool {
        self.durable.get(attempt).copied().unwrap_or(false)
    }

    pub fn registered_incarnations(&self) -> &[RegisteredIncarnation] {
        &self.registered
    }

    pub fn process_returned(&self, id: &IncarnationId) -> bool {
        self.registered
            .iter()
            .find(|item| item.id == *id)
            .is_some_and(|item| item.process_returned)
    }

    pub fn cleanup_targets(&self) -> &[IncarnationId] {
        &self.cleanup_targets
    }

    pub fn log(&self) -> &[RoomLog] {
        &self.log
    }

    fn now(&self) -> MonoMs {
        self.now
    }

    fn view(&self, ready: bool) -> AdmissionState {
        AdmissionState {
            fence: self
                .current_fence
                .clone()
                .expect("admission fence is bound"),
            deadline: self.deadline,
            ready,
            gate_enabled: self.gate_enabled,
            admission_closed: self.admission_closed,
        }
    }

    fn try_acquire(&self, id: u64) -> bool {
        self.with_gate(|gate| {
            if gate.holder.is_some() {
                return false;
            }
            gate.holder = Some(id);
            true
        })
    }

    fn release(&mut self, id: u64, record: bool) {
        self.with_gate(|gate| {
            assert_eq!(gate.holder, Some(id), "only the holder releases the gate");
            gate.holder = None;
        });
        if record {
            self.log.push(RoomLog::GateReleased);
        }
    }

    fn with_gate<T>(&self, body: impl FnOnce(&mut GateInner) -> T) -> T {
        let mut guard = self
            .gate
            .try_lock()
            .expect("gate mutex is never held across a poll");
        body(&mut guard)
    }

    fn set_dispatch(&mut self, attempt: AttemptId, state: DispatchState) {
        if self.resend_forbidden && self.dispatches.get(&attempt) == Some(&DispatchState::Uncertain)
        {
            return;
        }
        self.dispatches.insert(attempt, state);
    }

    fn register_incarnation(&mut self, id: IncarnationId) {
        if self.registered.iter().any(|item| item.id == id) {
            return;
        }
        self.registered.push(RegisteredIncarnation {
            id,
            process_returned: false,
        });
    }

    fn set_decision_sample(&mut self, now: MonoMs) {
        self.decision_sample = Some(now);
    }

    fn mark_admitting_durable(&mut self, attempt: AttemptId) {
        self.durable.insert(attempt, true);
        self.set_dispatch(attempt, DispatchState::Admitting);
    }

    fn note_enqueued(&mut self) {
        self.admitted_count = self.admitted_count.checked_add(1).expect("admitted count");
        self.prompts_consumed = self
            .prompts_consumed
            .checked_add(1)
            .expect("consumed prompts");
        self.residual_remote_work = true;
        self.log.push(RoomLog::PromptEnqueued);
    }

    fn commit_stop(&mut self) {
        self.stop_committed = true;
        self.admission_closed = true;
        self.admitted_at_stop = Some(self.admitted_count);
        if let Some(fence) = &mut self.current_fence {
            let next = fence.run_epoch.0.checked_add(1).expect("run epoch");
            fence.run_epoch = Epoch(next);
        }
        self.cleanup_targets = self.registered.iter().map(|item| item.id).collect();
        self.log.push(RoomLog::StopCommitted);
    }
}

pub struct AdmissionTask {
    id: u64,
    fence: Fence,
    prompt: String,
    phase: Phase,
    pause_during_commit: bool,
    commit_paused: bool,
    ready: bool,
    launch_counted: bool,
    admitting_committed: bool,
    events: Vec<AdmissionEvent>,
    outcome: Option<AdmissionOutcome>,
}

impl AdmissionTask {
    pub fn pause_during_commit(&mut self) {
        self.pause_during_commit = true;
    }

    pub fn ready_seen(&self) -> bool {
        self.ready
    }

    pub fn incarnation(&self) -> IncarnationId {
        self.fence.incarnation
    }

    pub fn events(&self) -> &[AdmissionEvent] {
        &self.events
    }

    pub fn drive_until(
        &mut self,
        room: &mut FakeRoom,
        queue: &impl LocalPromptQueue,
        checkpoint: Checkpoint,
    ) -> TaskPoll {
        for _ in 0..16 {
            let polled = self.poll(room, queue);
            match polled {
                TaskPoll::Yielded(point) if point == checkpoint => {
                    return TaskPoll::Yielded(point);
                }
                TaskPoll::Yielded(_) => {}
                TaskPoll::BlockedOnGate => {
                    panic!("admission blocked on the gate; waiting here would deadlock the test");
                }
                TaskPoll::Completed(_) => panic!("admission finished before {checkpoint:?}"),
                TaskPoll::StopCommitted => panic!("admission cannot commit stop"),
            }
        }
        panic!("admission did not reach {checkpoint:?}");
    }

    pub fn drive_to_end(
        &mut self,
        room: &mut FakeRoom,
        queue: &impl LocalPromptQueue,
    ) -> AdmissionOutcome {
        for _ in 0..16 {
            match self.poll(room, queue) {
                TaskPoll::Yielded(_) => {}
                TaskPoll::BlockedOnGate => {
                    panic!("admission blocked on the gate; waiting here would deadlock the test");
                }
                TaskPoll::Completed(outcome) => return outcome,
                TaskPoll::StopCommitted => panic!("admission cannot commit stop"),
            }
        }
        panic!("admission did not finish");
    }

    pub fn crash_after_commit(&mut self, room: &mut FakeRoom) {
        assert!(
            matches!(self.phase, Phase::BeforeEnqueue),
            "crash is only defined after the admitting commit returns"
        );
        room.set_dispatch(self.fence.attempt_id, DispatchState::Uncertain);
        room.prompts_consumed = room
            .prompts_consumed
            .checked_add(1)
            .expect("consumed prompts");
        room.resend_forbidden = true;
        room.release(self.id, true);
        self.outcome = Some(AdmissionOutcome {
            enqueued: false,
            admitting_committed: true,
            prompt_consumed: true,
            launch_counted: self.launch_counted,
            ready_seen: self.ready,
            dispatch: DispatchState::Uncertain,
            error: None,
        });
        self.phase = Phase::Done;
    }

    fn poll(&mut self, room: &mut FakeRoom, queue: &impl LocalPromptQueue) -> TaskPoll {
        match self.phase {
            Phase::Start => self.poll_start(room),
            Phase::Reserved => self.poll_reserved(room),
            Phase::Registered => self.poll_registered(room),
            Phase::GateHeld => self.poll_gate(room),
            Phase::Committing => self.finish_commit(room),
            Phase::BeforeEnqueue => self.poll_enqueue(room, queue),
            Phase::Done => TaskPoll::Completed(self.outcome.clone().expect("completed outcome")),
        }
    }

    fn poll_start(&mut self, room: &mut FakeRoom) -> TaskPoll {
        if room.resend_forbidden {
            return self.finish(
                room,
                AdmissionOutcome {
                    enqueued: false,
                    admitting_committed: false,
                    prompt_consumed: false,
                    launch_counted: false,
                    ready_seen: false,
                    dispatch: DispatchState::NotDispatched,
                    error: Some(invalid_state("uncertain_admission")),
                },
                false,
            );
        }
        room.set_dispatch(self.fence.attempt_id, DispatchState::Reserved);
        self.events.push(AdmissionEvent::Reserved);
        self.phase = Phase::Reserved;
        TaskPoll::Yielded(Checkpoint::Reserved)
    }

    fn poll_reserved(&mut self, room: &mut FakeRoom) -> TaskPoll {
        room.register_incarnation(self.fence.incarnation);
        self.launch_counted = true;
        self.events.push(AdmissionEvent::Registered);
        self.phase = Phase::Registered;
        TaskPoll::Yielded(Checkpoint::Registered)
    }

    fn poll_registered(&mut self, room: &mut FakeRoom) -> TaskPoll {
        if !self.ready {
            self.ready = true;
            self.events.push(AdmissionEvent::Ready);
            room.set_dispatch(self.fence.attempt_id, DispatchState::Ready);
        }
        if !room.try_acquire(self.id) {
            return TaskPoll::BlockedOnGate;
        }
        self.events.push(AdmissionEvent::GateAcquired);
        self.phase = Phase::GateHeld;
        TaskPoll::Yielded(Checkpoint::GateAcquired)
    }

    fn poll_gate(&mut self, room: &mut FakeRoom) -> TaskPoll {
        let now = room.now();
        let view = room.view(self.ready);
        if let Err(error) = check_admission(&view, &self.fence, now) {
            return self.fail(room, error, false);
        }
        room.set_decision_sample(now);
        if self.pause_during_commit && !self.commit_paused {
            self.commit_paused = true;
            self.phase = Phase::Committing;
            return TaskPoll::Yielded(Checkpoint::AdmittingCommit);
        }
        self.finish_commit(room)
    }

    fn finish_commit(&mut self, room: &mut FakeRoom) -> TaskPoll {
        room.mark_admitting_durable(self.fence.attempt_id);
        self.admitting_committed = true;
        self.events.push(AdmissionEvent::AdmittingCommitted);
        self.phase = Phase::BeforeEnqueue;
        TaskPoll::Yielded(Checkpoint::BeforeEnqueue)
    }

    fn poll_enqueue(&mut self, room: &mut FakeRoom, queue: &impl LocalPromptQueue) -> TaskPoll {
        let now = room.now();
        let view = room.view(self.ready);
        if let Err(error) = check_admission(&view, &self.fence, now) {
            return self.fail(room, error, true);
        }
        let prompt = AdmittedPrompt {
            fence: self.fence.clone(),
            prompt: self.prompt.clone(),
        };
        match queue.try_enqueue(prompt) {
            Ok(()) => {
                room.note_enqueued();
                room.set_dispatch(self.fence.attempt_id, DispatchState::Admitted);
                self.events.push(AdmissionEvent::Enqueued);
                self.events.push(AdmissionEvent::Admitted);
                let outcome = AdmissionOutcome {
                    enqueued: true,
                    admitting_committed: true,
                    prompt_consumed: true,
                    launch_counted: self.launch_counted,
                    ready_seen: self.ready,
                    dispatch: DispatchState::Admitted,
                    error: None,
                };
                self.finish(room, outcome, true)
            }
            Err(QueueReject) => self.fail(
                room,
                RtError::from_reason(InternalReason::QueueRejected),
                true,
            ),
        }
    }

    fn fail(&mut self, room: &mut FakeRoom, error: RtError, after_commit: bool) -> TaskPoll {
        let outcome = AdmissionOutcome {
            enqueued: false,
            admitting_committed: after_commit || self.admitting_committed,
            prompt_consumed: false,
            launch_counted: self.launch_counted,
            ready_seen: self.ready,
            dispatch: DispatchState::NotDispatched,
            error: Some(error),
        };
        room.set_dispatch(self.fence.attempt_id, DispatchState::NotDispatched);
        self.finish(room, outcome, true)
    }

    fn finish(
        &mut self,
        room: &mut FakeRoom,
        outcome: AdmissionOutcome,
        release: bool,
    ) -> TaskPoll {
        if release
            && !matches!(
                self.phase,
                Phase::Start | Phase::Reserved | Phase::Registered
            )
        {
            room.release(self.id, true);
        }
        self.outcome = Some(outcome.clone());
        self.phase = Phase::Done;
        TaskPoll::Completed(outcome)
    }
}

pub struct StopTask {
    id: u64,
    done: bool,
}

impl StopTask {
    pub fn poll(&mut self, room: &mut FakeRoom) -> TaskPoll {
        if self.done {
            return TaskPoll::StopCommitted;
        }
        if !room.try_acquire(self.id) {
            return TaskPoll::BlockedOnGate;
        }
        room.commit_stop();
        room.release(self.id, false);
        self.done = true;
        TaskPoll::StopCommitted
    }
}
