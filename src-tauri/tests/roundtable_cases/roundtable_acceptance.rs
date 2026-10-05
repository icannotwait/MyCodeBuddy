//! P12 accept, close, and publish. A lost acknowledgement must not admit again.
//! Staged input is accepted only inside the committed transaction.

use std::str::FromStr;
use std::sync::Arc;

use tempfile::TempDir;

use codeg_lib::roundtable::{
    migrate_roundtable, open_roundtable_store, AcceptInput, AcceptStep, CloseInput, FakeClock,
    NewAttempt, NewBinding, NewManifest, NewPhase, NewRoom, NewSpeaker, NewSubmission, NewTurn,
    PublishInput, RoundtableStore,
};
use roundtable_protocol::{
    project, CleanupProof, Epoch, Fence, Hash256, IncarnationId, ManifestId, MessageId, PhaseId,
    PhaseKind, PhaseRefV1, PhaseState, ProcessTreeProof, ProjectionBodyV1, ProjectionId,
    PublishedMessageRef, Revision, RoomAggregate, RoomId, RoomState, RuntimeTurnCompleted, Seq,
    SpeakerId, ToolBarrierV1,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::json;

use crate::roundtable_support::{open_pool, scalar_i64};

fn uid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{:012x}", n)
}

fn typed<T: FromStr>(n: u32) -> T {
    uid(n).parse().unwrap_or_else(|_| panic!("id {n}"))
}

#[allow(dead_code)]
struct Seat {
    ordinal: i64,
    role: String,
    state: String,
    speaker: String,
    turn: String,
    attempt: String,
    binding: String,
    incarnation: String,
}

struct Ready {
    _dir: TempDir,
    conn: DatabaseConnection,
    store: RoundtableStore,
    clock: Arc<FakeClock>,
    room: RoomId,
    phase: PhaseId,
    seats: Vec<Seat>,
    candidate_id: String,
    context: Hash256,
    policy: Hash256,
}

async fn ready(
    salt: u32,
    seats: &[(&str, i64, &str)],
    expected: i64,
    quorum: i64,
    start_ms: u64,
) -> Ready {
    let (dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let store = open_roundtable_store(conn.clone()).await.expect("store");
    let clock = Arc::new(FakeClock::new(start_ms));
    store.set_clock(clock.clone());
    let context = Hash256::sha256(b"context");
    let policy = Hash256::sha256(b"policy");
    let candidate_id = Hash256::sha256(b"result").to_hex();
    let room = uid(salt);
    let phase = uid(salt + 1);
    let manifest = uid(salt + 2);
    let principal = uid(salt + 3);
    store
        .insert_room(&NewRoom {
            room_id: room.clone(),
            principal_id: principal,
            status: "running".to_string(),
            config_ref: "cfg".to_string(),
            revision: 1,
            run_epoch: 1,
            boot_epoch: 1,
            last_seq: 0,
            remaining_active_ms: 1_800_000,
            blocked_reason: None,
            result_quality: None,
        })
        .await
        .expect("room");
    store
        .insert_manifest(&NewManifest {
            room_id: room.clone(),
            manifest_id: manifest.clone(),
            version: 1,
            manifest_hash: Hash256::sha256(b"manifest").to_hex(),
            body_json: "{}".to_string(),
        })
        .await
        .expect("manifest");
    store
        .insert_phase(&NewPhase {
            room_id: room.clone(),
            phase_id: phase.clone(),
            phase_index: 0,
            revision: 1,
            status: "running".to_string(),
            snapshot_ref: "proposal".to_string(),
            snapshot_hash: Hash256::sha256(b"snapshot").to_hex(),
            expected,
            quorum,
            remaining_ms: 1_800_000,
            manifest_id: Some(manifest),
        })
        .await
        .expect("phase");
    store
        .assign_current_phase(&room, 1, &phase)
        .await
        .expect("current");
    let mut built = Vec::new();
    for (offset, (role, ordinal, state)) in seats.iter().enumerate() {
        let base = salt + 10 + (offset as u32) * 10;
        let speaker = uid(base);
        let turn = uid(base + 1);
        let attempt = uid(base + 2);
        let binding = uid(base + 3);
        let incarnation = uid(base + 4);
        store
            .insert_speaker(&NewSpeaker {
                room_id: room.clone(),
                speaker_id: speaker.clone(),
                ordinal: *ordinal,
                role: (*role).to_string(),
                provider_ref: "provider".to_string(),
                model_id: "model".to_string(),
                model_snapshot_json: "{}".to_string(),
            })
            .await
            .expect("speaker");
        store
            .insert_binding(&NewBinding {
                room_id: room.clone(),
                binding_id: binding.clone(),
                speaker_id: speaker.clone(),
                generation: 1,
                incarnation: incarnation.clone(),
                external_session_id: format!("ext-{offset}"),
                policy_ref: policy.to_hex(),
                certificate_ref: "cert".to_string(),
                context_state: "fresh".to_string(),
                state: "ready".to_string(),
            })
            .await
            .expect("binding");
        store
            .insert_turn(&NewTurn {
                room_id: room.clone(),
                turn_id: turn.clone(),
                phase_id: phase.clone(),
                speaker_id: speaker.clone(),
                status: "open".to_string(),
                admitted_attempt_count: 1,
            })
            .await
            .expect("turn");
        store
            .insert_attempt(&NewAttempt {
                room_id: room.clone(),
                attempt_id: attempt.clone(),
                turn_id: turn.clone(),
                attempt_no: 1,
                binding_id: binding.clone(),
                fence: 1,
                dispatch_state: "sent".to_string(),
                state: (*state).to_string(),
                prompt_hash: Hash256::sha256(b"prompt").to_hex(),
                delivery_hash: context.to_hex(),
                cleanup_state: "pending".to_string(),
                residual_remote_work: 0,
            })
            .await
            .expect("attempt");
        if *state == "validating" {
            store
                .insert_submission(&NewSubmission {
                    room_id: room.clone(),
                    attempt_id: attempt.clone(),
                    submission_id: "sub-1".to_string(),
                    payload_hash: candidate_id.clone(),
                    validation_errors_json: "[]".to_string(),
                    receipt_json: Some("{}".to_string()),
                    sealed: 1,
                })
                .await
                .expect("submission");
        }
        built.push(Seat {
            ordinal: *ordinal,
            role: (*role).to_string(),
            state: (*state).to_string(),
            speaker,
            turn,
            attempt,
            binding,
            incarnation,
        });
    }
    Ready {
        _dir: dir,
        conn,
        store,
        clock,
        room: typed(salt),
        phase: typed(salt + 1),
        seats: built,
        candidate_id,
        context,
        policy,
    }
}

fn input(ready: &Ready, seat: &Seat, deadline_mono: u64) -> AcceptInput {
    let fence = Fence {
        boot_epoch: Epoch(1),
        run_epoch: Epoch(1),
        phase_id: ready.phase,
        phase_revision: Revision(1),
        attempt_id: seat.attempt.parse().expect("attempt"),
        binding_id: seat.binding.parse().expect("binding"),
        incarnation: seat.incarnation.parse().expect("incarnation"),
        context_hash: ready.context,
        policy_hash: ready.policy,
    };
    AcceptInput {
        room_id: ready.room,
        completion: RuntimeTurnCompleted {
            fence: fence.clone(),
            finish_reason: "completed".to_string(),
            ingress_watermark: Seq(1),
            tool_barrier: ToolBarrierV1 { drained: true },
            candidate_id: Some(ready.candidate_id.clone()),
        },
        candidate_id: ready.candidate_id.clone(),
        fence,
        deadline_mono,
        cleanup: CleanupProof {
            process: ProcessTreeProof {
                instance_id: "instance".to_string(),
                incarnation: seat.incarnation.parse().expect("incarnation"),
                process_tree_empty: true,
            },
            mailbox_empty: true,
            tools_drained: true,
            ingress_drained: true,
        },
        gate_held: true,
    }
}

async fn texts(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    let rows = conn
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_string(),
        ))
        .await
        .expect("rows");
    rows.into_iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("text"))
        .collect()
}

struct Counts {
    accepted_messages: i64,
}

async fn counts(conn: &DatabaseConnection) -> Counts {
    Counts {
        accepted_messages: scalar_i64(conn, "SELECT COUNT(*) FROM rt_messages").await,
    }
}

#[tokio::test]
async fn acceptance_rolls_back_every_semantic_row() {
    let steps = [
        AcceptStep::Message,
        AcceptStep::Claims,
        AcceptStep::Responses,
        AcceptStep::PositionChanges,
        AcceptStep::Evidence,
        AcceptStep::Budget,
        AcceptStep::Revision,
        AcceptStep::Projection,
        AcceptStep::Event,
        AcceptStep::Commit,
    ];
    for (index, step) in steps.into_iter().enumerate() {
        let ready = ready(
            1000 + index as u32 * 100,
            &[("moderator", 0, "validating")],
            1,
            1,
            10,
        )
        .await;
        let seat = &ready.seats[0];
        let accept = input(&ready, seat, 1_000);
        ready.store.set_accept_fault(Some(step));
        let failed = ready.store.accept(accept.clone()).await;
        assert!(failed.is_err(), "{step:?}");
        let after_failure = counts(&ready.conn).await;
        assert_eq!(after_failure.accepted_messages,0);
        ready.store.set_accept_fault(None);
        ready.clock.set_utc("2020-01-01T00:00:00Z");
        ready.store.accept(accept.clone()).await.expect("accept");
        let after_success = counts(&ready.conn).await;
        assert_eq!(after_success.accepted_messages,1);
        let visibility = scalar_text(
            &ready.conn,
            "SELECT visibility FROM rt_message_memberships",
        )
        .await;
        assert_eq!(visibility, "published");
        ready.clock.set(50_000).expect("forward");
        let replay = ready.store.accept(accept).await.expect("replay");
        let again = counts(&ready.conn).await;
        assert_eq!(again.accepted_messages,1);
        let admitted = scalar_i64(
            &ready.conn,
            "SELECT admitted_attempt_count FROM rt_turns",
        )
        .await;
        assert_eq!(admitted, 1);
        let events = texts(&ready.conn, "SELECT event_id FROM rt_events").await;
        let projection_versions =
            texts(&ready.conn, "SELECT projection_id FROM rt_projection_versions").await;
        assert_eq!(events.len(),projection_versions.len());
        let loaded = ready
            .store
            .projection(&ready.room, Some(&replay.id))
            .await
            .expect("projection");
        assert_eq!(loaded.projection_ref, replay);
        assert_eq!(loaded.body.room_id, ready.room);
    }
    let bare = FakeClock::new(10);
    assert!(bare.set(9).is_err());
}

#[tokio::test]
async fn close_freezes_and_publishes_ordinal() {
    let ready = ready(
        2000,
        &[
            ("moderator", 0, "validating"),
            ("member", 1, "validating"),
            ("member", 2, "validating"),
        ],
        3,
        2,
        10,
    )
    .await;
    for ordinal in [2_i64, 0, 1] {
        let seat = ready
            .seats
            .iter()
            .find(|seat| seat.ordinal == ordinal)
            .expect("seat");
        ready
            .store
            .accept(input(&ready, seat, 1_000))
            .await
            .expect("accept");
    }
    ready.clock.set(1_000).expect("deadline");
    let early = ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: input(&ready, &ready.seats[0], 1_000).fence,
            deadline_mono: 1_000,
            cancel_unfinished: false,
        })
        .await;
    assert!(early.is_err());
    let status = scalar_text(&ready.conn, "SELECT status FROM rt_phases").await;
    assert_eq!(status, "running");
    let closed = ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: input(&ready, &ready.seats[0], 1_001).fence,
            deadline_mono: 1_001,
            cancel_unfinished: false,
        })
        .await
        .expect("close");
    assert!(closed.frozen);
    assert_eq!(closed.phase_status, "closing");
    ready
        .store
        .publish(PublishInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: input(&ready, &ready.seats[0], 1_001).fence,
            deadline_mono: 1_001,
            cleanup_confirmed: true,
        })
        .await
        .expect("publish");
    let ordinals = texts(
        &ready.conn,
        "SELECT CAST(s.ordinal AS TEXT) FROM rt_message_memberships m
         JOIN rt_messages msg ON msg.room_id = m.room_id AND msg.message_id = m.message_id
         JOIN rt_speakers s ON s.room_id = msg.room_id AND s.speaker_id = msg.speaker_id
         WHERE m.visibility = 'published' ORDER BY s.ordinal ASC",
    )
    .await;
    let published_ordinals: Vec<i32> = ordinals.iter().map(|text| text.parse().unwrap()).collect();
    assert_eq!(published_ordinals,vec![0,1,2]);
    let events = texts(&ready.conn, "SELECT event_id FROM rt_events").await;
    let projection_versions =
        texts(&ready.conn, "SELECT projection_id FROM rt_projection_versions").await;
    assert_eq!(events.len(),projection_versions.len());
    ready
        .store
        .projection(&ready.room, None)
        .await
        .expect("latest");

    let open = ready_open().await;
    let hanging = open
        .store
        .close_phase(CloseInput {
            room_id: open.room,
            phase_id: open.phase,
            fence: input(&open, &open.seats[0], 5_000).fence,
            deadline_mono: 5_000,
            cancel_unfinished: true,
        })
        .await
        .expect("still running");
    assert!(!hanging.frozen);
    assert_eq!(hanging.phase_status, "running");
    let cancels = scalar_i64(
        &open.conn,
        "SELECT COUNT(*) FROM rt_control_operations WHERE kind = 'stop'",
    )
    .await;
    assert_eq!(cancels, 1);
}

async fn ready_open() -> Ready {
    let ready = ready(
        3000,
        &[
            ("moderator", 0, "validating"),
            ("member", 1, "validating"),
            ("member", 2, "validating"),
        ],
        3,
        2,
        10,
    )
    .await;
    for ordinal in [0_i64, 1] {
        let seat = ready
            .seats
            .iter()
            .find(|seat| seat.ordinal == ordinal)
            .expect("seat");
        ready
            .store
            .accept(input(&ready, seat, 5_000))
            .await
            .expect("accept");
    }
    ready
}

#[tokio::test]
async fn commit_without_ui_ack_does_not_rerun() {
    let ready = ready(4000, &[("moderator", 0, "validating")], 1, 1, 40).await;
    ready.clock.jump_on_call(2, 500);
    let first = ready
        .store
        .accept(input(&ready, &ready.seats[0], 100))
        .await
        .expect("commit");
    let body = scalar_text(&ready.conn, "SELECT changed_entities_json FROM rt_events").await;
    assert!(body.contains("\"decision_mono\":40"), "{body}");
    assert!(body.contains("\"commit_mono\":500"), "{body}");
    drop(first.clone());
    ready.clock.set(5_000).expect("later");
    let replay = ready
        .store
        .accept(input(&ready, &ready.seats[0], 100))
        .await
        .expect("replay");
    assert_eq!(replay, first);
    let admitted = scalar_i64(
        &ready.conn,
        "SELECT admitted_attempt_count FROM rt_turns",
    )
    .await;
    assert_eq!(admitted, 1);
    let after_success = counts(&ready.conn).await;
    assert_eq!(after_success.accepted_messages,1);
}

#[tokio::test]
async fn blocked_transaction_samples_deadline_after_lock() {
    let ready = ready(5000, &[("moderator", 0, "validating")], 1, 1, 10).await;
    let gate = ready.store.arm_lock_gate();
    let store = ready.store.clone();
    let accept = input(&ready, &ready.seats[0], 100);
    let task = tokio::spawn(async move { store.accept(accept).await });
    while !gate.is_waiting() {
        tokio::task::yield_now().await;
    }
    ready.clock.set(100).expect("reach deadline");
    gate.release();
    let err = task.await.expect("join").expect_err("deadline");
    assert_eq!(err.details.reason.as_deref(), Some("deadline_exceeded"));
    let after_failure = counts(&ready.conn).await;
    assert_eq!(after_failure.accepted_messages,0);
}

#[tokio::test]
async fn phase_failed_and_moderator_failure() {
    let short = ready(
        6000,
        &[("moderator", 0, "validating"), ("member", 1, "invalid")],
        2,
        2,
        10,
    )
    .await;
    short
        .store
        .accept(input(&short, &short.seats[0], 1_000))
        .await
        .expect("moderator");
    let failed = short
        .store
        .close_phase(CloseInput {
            room_id: short.room,
            phase_id: short.phase,
            fence: input(&short, &short.seats[0], 1_000).fence,
            deadline_mono: 1_000,
            cancel_unfinished: false,
        })
        .await
        .expect("phase failed");
    assert_eq!(failed.phase_status, "failed");
    assert_eq!(failed.room_status, "running");
    assert_eq!(failed.closing_set_ref.as_deref(), Some("phase_failed"));

    let paused = ready(
        7000,
        &[("moderator", 0, "failed"), ("member", 1, "validating")],
        2,
        2,
        10,
    )
    .await;
    paused
        .store
        .accept(input(&paused, &paused.seats[1], 1_000))
        .await
        .expect("member");
    let closed = paused
        .store
        .close_phase(CloseInput {
            room_id: paused.room,
            phase_id: paused.phase,
            fence: input(&paused, &paused.seats[1], 1_000).fence,
            deadline_mono: 1_000,
            cancel_unfinished: false,
        })
        .await
        .expect("pause");
    assert_eq!(closed.room_status, "paused");
    assert_eq!(closed.blocked_reason.as_deref(), Some("synthesis_failed"));
    assert_eq!(closed.phase_status, "failed");
}

#[tokio::test]
async fn project_rejects_a_missing_message_ref() {
    let id = typed::<ProjectionId>(1);
    let message = typed::<MessageId>(2);
    let body = ProjectionBodyV1 {
        schema_version: 1,
        room_id: typed::<RoomId>(3),
        revision: Revision(1),
        run_epoch: Epoch(1),
        last_seq: Seq(1),
        status: RoomState::Running,
        blocked_reason: None,
        config_hash: Hash256::sha256(b"cfg"),
        moderator_speaker_id: Some(typed::<SpeakerId>(4)),
        phase_refs: vec![PhaseRefV1 {
            phase_id: typed::<PhaseId>(5),
            index: 0,
            revision: Revision(1),
            kind: PhaseKind::Proposal,
            state: PhaseState::Running,
        }],
        ledger_seq: Seq(1),
        sampled_active_ms: roundtable_protocol::DurationMs(0),
        sampled_at_utc: "2026-10-05T00:00:00Z".to_string(),
    };
    let err = project(&RoomAggregate {
        projection_id: id,
        body: body.clone(),
        messages: vec![],
        evidence_manifests: vec![typed::<ManifestId>(6)],
        required_message_ids: vec![message],
        required_evidence_manifests: vec![],
    })
    .expect_err("missing message");
    assert_eq!(err.details.reason.as_deref(), Some("missing_field"));
    let projected = project(&RoomAggregate {
        projection_id: id,
        body: body.clone(),
        messages: vec![PublishedMessageRef {
            message_id: message,
            hash: Hash256::sha256(b"body"),
        }],
        evidence_manifests: vec![],
        required_message_ids: vec![message],
        required_evidence_manifests: vec![],
    })
    .expect("project");
    let again = project(&RoomAggregate {
        projection_id: id,
        body,
        messages: vec![PublishedMessageRef {
            message_id: message,
            hash: Hash256::sha256(b"body"),
        }],
        evidence_manifests: vec![],
        required_message_ids: vec![message],
        required_evidence_manifests: vec![],
    })
    .expect("stable");
    assert_eq!(projected.projection_ref.hash, again.projection_ref.hash);
    let _ = json!({"stable": true});
    let _ = IncarnationId::from_str(&uid(9));
}

async fn scalar_text(conn: &DatabaseConnection, sql: &str) -> String {
    texts(conn, sql).await.into_iter().next().expect("text")
}
