//! In-memory admission races.
//!
//! Cooperative failpoints order the attempts. Nothing here sleeps, waits for
//! a process, or proves the later product gate.

#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use roundtable_protocol::{
    deadline_still_open, AdmissionEvent, Checkpoint, DispatchState, ErrorCode, FakePromptQueue,
    FakeRoom, MonoMs, RoomLog, TaskPoll,
};

#[test]
fn stop_before_gate_rejects_old_enqueue() {
    let (mut room, fake, fence) = world(1_000);
    let mut attempt = room.start_admission(fence, "prompt-a");
    assert!(matches!(
        attempt.drive_until(&mut room, &fake, Checkpoint::Registered),
        TaskPoll::Yielded(Checkpoint::Registered)
    ));
    assert!(room
        .registered_incarnations()
        .iter()
        .any(|item| { item.id == attempt.incarnation() && !item.process_returned }));
    assert!(!room.process_returned(&attempt.incarnation()));

    let mut stop = room.start_stop();
    assert!(matches!(stop.poll(&mut room), TaskPoll::StopCommitted));
    assert!(room.stop_committed());
    assert!(room.cleanup_targets().contains(&attempt.incarnation()));
    assert!(!room.process_returned(&attempt.incarnation()));

    let outcome = attempt.drive_to_end(&mut room, &fake);
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 0);
    assert!(!outcome.enqueued);
    assert!(!outcome.admitting_committed);
    assert!(!outcome.prompt_consumed);
    assert!(outcome.launch_counted);
    assert_eq!(outcome.dispatch, DispatchState::NotDispatched);
    assert_eq!(room.admitted_count(), 0);
    assert_eq!(room.new_admissions_after_stop(), 0);
    let error = outcome.error.expect("stop wins before the gate");
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.reason.as_deref(), Some("stale_fence"));
    assert!(!attempt.events().contains(&AdmissionEvent::Enqueued));
    assert!(!attempt.events().contains(&AdmissionEvent::Admitted));
    assert_eq!(room.log(), &[RoomLog::StopCommitted, RoomLog::GateReleased]);
}

#[test]
fn enqueue_inside_gate_precedes_stop_commit() {
    let (mut room, fake, fence) = world(1_000);
    let mut attempt = room.start_admission(fence, "prompt-a");
    assert!(matches!(
        attempt.drive_until(&mut room, &fake, Checkpoint::BeforeEnqueue),
        TaskPoll::Yielded(Checkpoint::BeforeEnqueue)
    ));
    assert!(room.gate_held());
    assert_eq!(
        attempt.events(),
        &[
            AdmissionEvent::Reserved,
            AdmissionEvent::Registered,
            AdmissionEvent::Ready,
            AdmissionEvent::GateAcquired,
            AdmissionEvent::AdmittingCommitted,
        ]
    );
    assert_eq!(fake.try_enqueue_calls(), 0);

    let mut stop = room.start_stop();
    assert!(matches!(stop.poll(&mut room), TaskPoll::BlockedOnGate));
    assert!(!room.stop_committed());
    assert!(room.gate_held());

    let outcome = attempt.drive_to_end(&mut room, &fake);
    assert!(outcome.enqueued);
    assert!(outcome.admitting_committed);
    assert!(outcome.prompt_consumed);
    assert_eq!(outcome.dispatch, DispatchState::Admitted);
    assert!(outcome.error.is_none());
    assert_eq!(fake.enqueued_count(), 1);
    assert!(!room.gate_held());
    assert_eq!(
        attempt.events(),
        &[
            AdmissionEvent::Reserved,
            AdmissionEvent::Registered,
            AdmissionEvent::Ready,
            AdmissionEvent::GateAcquired,
            AdmissionEvent::AdmittingCommitted,
            AdmissionEvent::Enqueued,
            AdmissionEvent::Admitted,
        ]
    );

    assert!(matches!(stop.poll(&mut room), TaskPoll::StopCommitted));
    assert!(room.stop_committed());
    assert!(room.residual_remote_work());
    assert_eq!(room.admitted_count(), 1);
    assert_eq!(room.new_admissions_after_stop(), 0);
    assert_eq!(
        room.log(),
        &[
            RoomLog::PromptEnqueued,
            RoomLog::GateReleased,
            RoomLog::StopCommitted,
        ]
    );

    let later = support::fence(2);
    let mut retry = room.start_admission(later, "prompt-after-stop");
    let rejected = retry.drive_to_end(&mut room, &fake);
    assert!(!rejected.enqueued);
    assert_eq!(rejected.error.unwrap().code, ErrorCode::InvalidState);
    assert_eq!(fake.enqueued_count(), 1);
    assert_eq!(room.admitted_count(), 1);
    assert_eq!(room.new_admissions_after_stop(), 0);
    assert!(room.residual_remote_work());
}

#[test]
fn full_queue_is_queue_rejected_and_admitted_count_unchanged() {
    let (mut room, fake, fence) = world(1_000);
    fake.reject_all();
    let mut attempt = room.start_admission(fence, "prompt-full");
    let outcome = attempt.drive_to_end(&mut room, &fake);
    let error = outcome.error.expect("full queue");
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.reason.as_deref(), Some("queue_rejected"));
    assert_eq!(room.admitted_count(), 0);
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 1);
    assert!(outcome.admitting_committed);
    assert!(!outcome.enqueued);
    assert!(!outcome.prompt_consumed);
    assert!(outcome.launch_counted);
    assert_eq!(outcome.dispatch, DispatchState::NotDispatched);
    assert!(!room.residual_remote_work());
}

#[test]
fn crash_after_admitting_commit_does_not_retry() {
    let (mut room, fake, fence) = world(1_000);
    let mut attempt = room.start_admission(fence.clone(), "prompt-crash");
    assert!(matches!(
        attempt.drive_until(&mut room, &fake, Checkpoint::BeforeEnqueue),
        TaskPoll::Yielded(Checkpoint::BeforeEnqueue)
    ));
    assert!(room.gate_held());
    attempt.crash_after_commit(&mut room);
    assert!(!room.gate_held());
    assert_eq!(room.dispatch(&fence.attempt_id), DispatchState::Uncertain);
    assert!(room.resend_forbidden());
    assert_eq!(room.prompts_consumed(), 1);
    assert_eq!(room.admitted_count(), 0);
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 0);
    assert!(attempt
        .events()
        .contains(&AdmissionEvent::AdmittingCommitted));
    assert!(!attempt.events().contains(&AdmissionEvent::Enqueued));

    let mut retry = room.start_admission(fence.clone(), "prompt-crash");
    let rejected = retry.drive_to_end(&mut room, &fake);
    assert!(!rejected.enqueued);
    assert!(!rejected.launch_counted);
    let error = rejected.error.expect("no retry");
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.reason.as_deref(), Some("uncertain_admission"));
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 0);
    assert_eq!(room.prompts_consumed(), 1);
    assert_eq!(room.admitted_count(), 0);
    assert_eq!(room.dispatch(&fence.attempt_id), DispatchState::Uncertain);
}

#[test]
fn deadline_expired_during_admitting_commit_rejects_enqueue() {
    let (mut room, fake, fence) = world(40);
    let mut attempt = room.start_admission(fence.clone(), "prompt-deadline");
    attempt.pause_during_commit();
    assert!(matches!(
        attempt.drive_until(&mut room, &fake, Checkpoint::AdmittingCommit),
        TaskPoll::Yielded(Checkpoint::AdmittingCommit)
    ));
    assert!(room.gate_held());
    assert!(!room.admitting_durable(&fence.attempt_id));
    let sampled = room.decision_sample().expect("decision sample");
    assert!(deadline_still_open(sampled, room.deadline()));
    assert_eq!(fake.try_enqueue_calls(), 0);

    room.advance_to(room.deadline());
    let outcome = attempt.drive_to_end(&mut room, &fake);
    let error = outcome.error.expect("resampled deadline");
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.reason.as_deref(), Some("deadline_reached"));
    assert!(outcome.admitting_committed);
    assert!(!outcome.enqueued);
    assert!(!outcome.prompt_consumed);
    assert!(outcome.launch_counted);
    assert_eq!(outcome.dispatch, DispatchState::NotDispatched);
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 0);
    assert_eq!(room.admitted_count(), 0);
    assert!(!room.gate_held());
    assert!(attempt
        .events()
        .contains(&AdmissionEvent::AdmittingCommitted));
    assert!(!attempt.events().contains(&AdmissionEvent::Enqueued));
}

#[test]
fn disabled_gate_rejects_ready_attempt_before_enqueue() {
    let (mut room, fake, fence) = world(1_000);
    let mut attempt = room.start_admission(fence, "prompt-ready");
    assert!(matches!(
        attempt.drive_until(&mut room, &fake, Checkpoint::GateAcquired),
        TaskPoll::Yielded(Checkpoint::GateAcquired)
    ));
    assert!(attempt.ready_seen());
    assert!(room.gate_held());
    assert!(room.gate_enabled());
    room.disable_gate();
    let outcome = attempt.drive_to_end(&mut room, &fake);
    let error = outcome.error.expect("disabled gate");
    assert_eq!(error.code, ErrorCode::InvalidState);
    assert_eq!(error.details.reason.as_deref(), Some("gate_disabled"));
    assert!(outcome.ready_seen);
    assert!(!outcome.enqueued);
    assert!(!outcome.admitting_committed);
    assert!(!outcome.prompt_consumed);
    assert!(outcome.launch_counted);
    assert_eq!(outcome.dispatch, DispatchState::NotDispatched);
    assert_eq!(fake.enqueued_count(), 0);
    assert_eq!(fake.try_enqueue_calls(), 0);
    assert_eq!(room.admitted_count(), 0);
    assert!(attempt.events().contains(&AdmissionEvent::Ready));
    assert!(!attempt.events().contains(&AdmissionEvent::Enqueued));
    assert!(!attempt
        .events()
        .contains(&AdmissionEvent::AdmittingCommitted));
}

fn world(deadline: u64) -> (FakeRoom, FakePromptQueue, roundtable_protocol::Fence) {
    let fence = fence_for("attempt-a");
    let room = FakeRoom::new(MonoMs(0), MonoMs(deadline));
    (room, FakePromptQueue::new(), fence)
}

fn fence_for(label: &str) -> roundtable_protocol::Fence {
    let mut fence = support::fence(1);
    fence.attempt_id = support::id(label);
    fence.binding_id = support::id(&format!("binding-{label}"));
    fence.incarnation = support::id(&format!("incarnation-{label}"));
    fence
}
