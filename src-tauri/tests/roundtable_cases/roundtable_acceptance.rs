//! P12 accept, close, and publish. A lost acknowledgement must not admit again.
//! Staged input is accepted only inside the committed transaction.

use std::str::FromStr;
use std::sync::Arc;

use tempfile::TempDir;

use codeg_lib::roundtable::{
    migrate_roundtable, open_roundtable_store, AcceptInput, AcceptStep, CloseInput, FakeClock,
    NewAttempt, NewBinding, NewManifest, NewPhase, NewRoom, NewSpeaker, NewTurn, PublishInput,
    RoundtableStore,
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
    ready_with_pool(salt, seats, expected, quorum, start_ms, 1).await
}

async fn ready_with_pool(
    salt: u32,
    seats: &[(&str, i64, &str)],
    expected: i64,
    quorum: i64,
    start_ms: u64,
    connections: u32,
) -> Ready {
    let (dir, conn) = open_pool(connections).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let store = open_roundtable_store(conn.clone()).await.expect("store");
    let clock = Arc::new(FakeClock::new(start_ms));
    store.set_clock(clock.clone());
    let context = Hash256::sha256(b"context");
    let policy = Hash256::sha256(b"policy");
    let candidate = roundtable_protocol::canonical_bytes(&json!({"kind":"proposal","summary":"summary","claims":[{"local_key":"c1","text":"actual claim","evidence_aliases":[],"confidence":"high"}]})).unwrap();
    let candidate_id = Hash256::sha256(&candidate).to_hex();
    let room = uid(salt);
    let phase = uid(salt + 1);
    let manifest = uid(salt + 2);
    let principal = uid(salt + 3);
    store
        .insert_room(&NewRoom {
            room_id: room.clone(),
            principal_id: principal.clone(),
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
            manifest_id: Some(manifest.clone()),
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
            use codeg_lib::roundtable::ToolStore;
            use codeg_lib::roundtable::{
                DurableToolStore, ObjectStore, ReservationLedger, ToolSession,
            };
            use roundtable_protocol::{PublishedHistory, ResultScope, VisibleAliases};
            let scope = ResultScope {
                phase_kind: PhaseKind::Proposal,
                speaker_id: speaker.parse().unwrap(),
                aliases: VisibleAliases::default(),
                mandatory_targets: vec![],
                published: PublishedHistory::default(),
                quota_bytes: 8192,
            };
            let validated = roundtable_protocol::validate_result(&candidate, &scope).unwrap();
            let objects = ObjectStore::open(
                dir.path().join("objects"),
                Arc::new(ReservationLedger::new(1_000_000)),
                principal.parse().unwrap(),
            )
            .unwrap();
            let tools = DurableToolStore::open(
                store.clone(),
                objects,
                ToolSession {
                    room_id: room.clone(),
                    attempt_id: attempt.clone(),
                    speaker_id: speaker.clone(),
                    phase_id: phase.clone(),
                    phase_revision: 1,
                    manifest_id: manifest.clone(),
                    workspace_snapshot_id: uid(salt + 4),
                    manifest_hash: Hash256::sha256(b"manifest").to_hex(),
                },
                scope,
            );
            tools
                .submit_canonical(&"sub-1".parse().unwrap(), &validated, &candidate)
                .await
                .expect("real durable submission");
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
        assert_eq!(after_failure.accepted_messages, 0);
        ready.store.set_accept_fault(None);
        ready.clock.set_utc("2020-01-01T00:00:00Z");
        ready.store.accept(accept.clone()).await.expect("accept");
        let after_success = counts(&ready.conn).await;
        assert_eq!(after_success.accepted_messages, 1);
        // Acceptance stages every message, including the moderator's. Only
        // closing and publication append a published membership version.
        let visibility=scalar_text(&ready.conn,"SELECT visibility FROM rt_message_memberships ORDER BY membership_version DESC LIMIT 1").await;
        assert_eq!(visibility, "staged");
        ready
            .store
            .close_phase(CloseInput {
                room_id: ready.room,
                phase_id: ready.phase,
                fence: accept.fence.clone(),
                deadline_mono: 1000,
                cancel_unfinished: false,
            })
            .await
            .unwrap();
        ready
            .store
            .publish(PublishInput {
                room_id: ready.room,
                phase_id: ready.phase,
                fence: accept.fence.clone(),
                deadline_mono: 1000,
                cleanup_confirmed: true,
            })
            .await
            .unwrap();
        let visibility=scalar_text(&ready.conn,"SELECT visibility FROM rt_message_memberships ORDER BY membership_version DESC LIMIT 1").await;
        assert_eq!(visibility, "published");
        ready.clock.set(50_000).expect("forward");
        let replay = ready.store.accept(accept).await.expect("replay");
        let again = counts(&ready.conn).await;
        assert_eq!(again.accepted_messages, 1);
        let admitted = scalar_i64(&ready.conn, "SELECT admitted_attempt_count FROM rt_turns").await;
        assert_eq!(admitted, 1);
        let events = texts(&ready.conn, "SELECT event_id FROM rt_events").await;
        let projection_versions = texts(
            &ready.conn,
            "SELECT projection_id FROM rt_projection_versions",
        )
        .await;
        assert_eq!(events.len(), projection_versions.len());
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
    let closed = ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: input(&ready, &ready.seats[0], 1000).fence,
            deadline_mono: 1000,
            cancel_unfinished: false,
        })
        .await
        .expect("local closing may proceed at the expired model deadline");
    ready.clock.set(1002).unwrap();
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
    assert_eq!(published_ordinals, vec![0, 1, 2]);
    let events = texts(&ready.conn, "SELECT event_id FROM rt_events").await;
    let projection_versions = texts(
        &ready.conn,
        "SELECT projection_id FROM rt_projection_versions",
    )
    .await;
    assert_eq!(events.len(), projection_versions.len());
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

#[tokio::test]
async fn storage_fix_closing_keeps_unlaunched_frozen_member_absent() {
    use roundtable_protocol::{OrderedMemberV1, PhaseSnapshotV1, SafeInt, ToolQuotaV1};
    let ready = ready(
        12500,
        &[("member", 0, "validating"), ("member", 1, "validating")],
        3,
        2,
        10,
    )
    .await;
    for seat in &ready.seats {
        ready.store.accept(input(&ready, seat, 1000)).await.unwrap();
    }
    let absent: SpeakerId = typed(12580);
    // The fourth room member is intentionally not in this phase's frozen set.
    for (speaker, ordinal) in [(absent, 2), (typed(12581), 3)] {
        ready
            .store
            .insert_speaker(&NewSpeaker {
                room_id: ready.room.to_string(),
                speaker_id: speaker.to_string(),
                ordinal,
                role: "member".into(),
                provider_ref: "provider".into(),
                model_id: "model".into(),
                model_snapshot_json: "{}".into(),
            })
            .await
            .unwrap();
    }
    let members = ready
        .seats
        .iter()
        .map(|seat| seat.speaker.parse::<SpeakerId>().unwrap())
        .chain(std::iter::once(absent))
        .enumerate()
        .map(|(ordinal, speaker_id)| OrderedMemberV1 {
            ordinal: ordinal as u32,
            speaker_id,
            participant_id: speaker_id.to_string().parse().unwrap(),
        })
        .collect();
    let snapshot = PhaseSnapshotV1 {
        schema_version: 1,
        phase_id: ready.phase,
        phase_index: 0,
        revision: Revision(1),
        kind: PhaseKind::Proposal,
        critique_round: None,
        config_version: Revision(1),
        question_version: Revision(1),
        interjection_version: Revision(1),
        source_manifest_id: typed(12502),
        source_manifest_hash: Hash256::sha256(b"manifest"),
        published_messages: vec![],
        members,
        mandatory_targets: vec![],
        policy_hash: ready.policy,
        output_byte_limit: SafeInt(8192),
        tool_quota: ToolQuotaV1 {
            per_call_bytes: SafeInt(8192),
            per_attempt_bytes: SafeInt(32768),
        },
    };
    ready.conn.execute(Statement::from_sql_and_values(DatabaseBackend::Sqlite,
        "INSERT INTO rt_phase_contexts(room_id,phase_id,snapshot_json,context_json) VALUES(?,?,?,'{}')",
        [ready.room.to_string().into(),ready.phase.to_string().into(),serde_json::to_string(&snapshot).unwrap().into()])).await.unwrap();
    let close = CloseInput {
        room_id: ready.room,
        phase_id: ready.phase,
        fence: input(&ready, &ready.seats[0], 1000).fence,
        deadline_mono: 1000,
        cancel_unfinished: false,
    };
    assert!(
        !ready.store.close_phase(close.clone()).await.unwrap().frozen,
        "quorum cannot close before the unlaunched slot terminates or times out"
    );
    ready.clock.set(1000).unwrap();
    assert!(ready.store.close_phase(close).await.unwrap().frozen);
    let frozen: serde_json::Value = serde_json::from_str(
        &scalar_text(&ready.conn, "SELECT body_json FROM rt_closing_sets").await,
    )
    .unwrap();
    let slots = frozen["slots"].as_array().unwrap();
    assert_eq!(slots.len(), 3);
    let missing = slots
        .iter()
        .find(|slot| slot["speaker_id"] == absent.to_string())
        .unwrap();
    assert_eq!(missing["outcome"], "absent");
    for field in ["turn_id", "attempt_id", "message_id", "body_hash"] {
        assert!(missing[field].is_null());
    }
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_turns").await,
        2
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT expected FROM rt_phases").await,
        3
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT quorum FROM rt_phases").await,
        2
    );
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
    let admitted = scalar_i64(&ready.conn, "SELECT admitted_attempt_count FROM rt_turns").await;
    assert_eq!(admitted, 1);
    let after_success = counts(&ready.conn).await;
    assert_eq!(after_success.accepted_messages, 1);
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
    assert_eq!(after_failure.accepted_messages, 0);
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
    paused
        .conn
        .execute_unprepared("UPDATE rt_phases SET snapshot_ref='synthesis'")
        .await
        .unwrap();
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
        messages: Vec::new(),
        evidence_manifests: Vec::new(),
        replay: Default::default(),
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

#[tokio::test]
async fn storage_fix_accept_uses_canonical_candidate() {
    let mut ready = ready(8100, &[("member", 0, "validating")], 1, 1, 10).await;
    let candidate = roundtable_protocol::canonical_bytes(&json!({
        "kind":"proposal", "summary":"summary", "claims":[{
            "local_key":"c1", "text":"actual claim", "evidence_aliases":[], "confidence":"high"
        }]
    }))
    .unwrap();
    ready.candidate_id = Hash256::sha256(&candidate).to_hex();
    ready
        .conn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE rt_submissions SET candidate_ref = ?, payload_hash = ?",
            vec![
                String::from_utf8(candidate.clone()).unwrap().into(),
                ready.candidate_id.clone().into(),
            ],
        ))
        .await
        .unwrap();
    ready
        .store
        .accept(input(&ready, &ready.seats[0], 1000))
        .await
        .unwrap();
    let body = scalar_text(&ready.conn, "SELECT body_json FROM rt_messages").await;
    assert_eq!(body.as_bytes(), candidate);
    assert_eq!(
        scalar_text(&ready.conn, "SELECT statement FROM rt_claims").await,
        "actual claim"
    );
    assert_eq!(
        Hash256::sha256(body.as_bytes()).to_hex(),
        ready.candidate_id
    );
}

#[tokio::test]
async fn storage_fix_publish_rejects_stopped_room() {
    let ready = ready(8200, &[("member", 0, "validating")], 1, 1, 10).await;
    let accept = input(&ready, &ready.seats[0], 1000);
    ready.store.accept(accept.clone()).await.unwrap();
    ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: accept.fence.clone(),
            deadline_mono: 1000,
            cancel_unfinished: false,
        })
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET status='stopped',run_epoch=run_epoch+1")
        .await
        .unwrap();
    assert!(ready
        .store
        .publish(PublishInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: accept.fence,
            deadline_mono: 1000,
            cleanup_confirmed: true
        })
        .await
        .is_err());
    assert_eq!(
        scalar_text(&ready.conn, "SELECT status FROM rt_phases").await,
        "closing"
    );
}

#[tokio::test]
async fn storage_fix_old_attempt_does_not_close_open_turn() {
    let ready = ready_open().await;
    ready.conn.execute(Statement::from_sql_and_values(DatabaseBackend::Sqlite,
        "INSERT INTO rt_attempts SELECT room_id, ?, turn_id, 0, binding_id, fence, dispatch_state,
         'failed', admitted_at, finished_at, finish_reason, prompt_hash, delivery_hash,
         'confirmed', diagnostic_ref, residual_remote_work FROM rt_attempts WHERE attempt_id=?",
        vec![uid(8900).into(),ready.seats[0].attempt.clone().into()])).await.unwrap();
    let closed = ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: input(&ready, &ready.seats[0], 1000).fence,
            deadline_mono: 1000,
            cancel_unfinished: false,
        })
        .await
        .unwrap();
    assert!(
        !closed.frozen,
        "a historical failure must not count as another terminal turn"
    );
}

#[tokio::test]
async fn storage_fix_control_ack_survives_store_reopen() {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    let ready = ready(9100, &[("member", 0, "failed")], 1, 1, 10).await;
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET cleanup_state='confirmed'")
        .await
        .unwrap();
    let actor = ActorContext::from_trusted_entry(
        typed(9103),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "test".into(),
        },
    );
    let request = ControlRequest {
        room_id: ready.room,
        request_id: typed(9190),
        expected_revision: Revision(1),
        kind: ControlKind::Stop,
        input: json!({"reason":"stop"}),
        force_latest: false,
    };
    let first = ready.store.request_control(&actor, &request).await.unwrap();
    let reopened = open_roundtable_store(ready.conn.clone()).await.unwrap();
    assert_eq!(
        reopened.request_control(&actor, &request).await.unwrap(),
        first
    );
    let done = reopened
        .advance_durable_control(&actor, ready.room, first.operation_id.unwrap(), false)
        .await
        .unwrap();
    assert_eq!(done.status, RoomState::Stopped);
    assert_eq!(
        reopened.request_control(&actor, &request).await.unwrap(),
        first
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_control_operations").await,
        1
    );
}

pub(crate) async fn usage_fixture() -> (
    TempDir,
    DatabaseConnection,
    RoundtableStore,
    roundtable_protocol::AttemptId,
) {
    let ready = ready(
        9300,
        &[("member", 0, "failed"), ("member", 1, "failed")],
        2,
        1,
        10,
    )
    .await;
    // Product reads decode config_ref as a real configuration, unlike the
    // smaller acceptance-only fixtures that deliberately use an opaque token.
    ready
        .conn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE rt_rooms SET config_ref=?",
            [serde_json::to_string(&resume_config()).unwrap().into()],
        ))
        .await
        .unwrap();
    let attempt = ready.seats[0].attempt.parse().unwrap();
    (ready._dir, ready.conn, ready.store, attempt)
}

#[tokio::test]
async fn storage_fix_recovery_publishes_frozen_set_once_without_new_attempts() {
    let ready = ready(9500, &[("member", 0, "validating")], 1, 1, 10).await;
    let accepted = input(&ready, &ready.seats[0], 1000);
    ready.store.accept(accepted.clone()).await.unwrap();
    ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: accepted.fence,
            deadline_mono: 1000,
            cancel_unfinished: false,
        })
        .await
        .unwrap();
    let original = scalar_text(&ready.conn, "SELECT body_json FROM rt_closing_sets").await;
    ready.store.recover_durable(2).await.unwrap();
    assert_eq!(
        scalar_text(&ready.conn, "SELECT status FROM rt_rooms").await,
        "paused"
    );
    assert_eq!(
        scalar_text(&ready.conn, "SELECT blocked_reason FROM rt_rooms").await,
        "recovery_required"
    );
    assert_eq!(
        ready
            .store
            .projection(&ready.room, None)
            .await
            .unwrap()
            .body
            .status,
        RoomState::Paused
    );
    ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .expect("explicit resume may schedule the next phase");
    assert_eq!(scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_rooms WHERE status IN ('running','pausing','stopping','recovering')").await, 0);
    assert_eq!(
        scalar_text(&ready.conn, "SELECT status FROM rt_phases").await,
        "published"
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_attempts").await,
        1
    );
    assert_eq!(
        scalar_text(&ready.conn, "SELECT body_json FROM rt_closing_sets").await,
        original
    );
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT recovery_boot_epoch FROM rt_closing_sets"
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT COUNT(*) FROM rt_control_operations WHERE kind='recover'"
        )
        .await,
        1
    );
    ready.store.recover_durable(3).await.unwrap();
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT COUNT(*) FROM rt_events WHERE cause='publish'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn storage_fix_recovery_completes_stop_and_replays_original_ack() {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    let ready = ready(9700, &[("member", 0, "validating")], 1, 1, 10).await;
    let actor = ActorContext::from_trusted_entry(
        typed(9703),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "test".into(),
        },
    );
    let request = ControlRequest {
        room_id: ready.room,
        request_id: typed(9790),
        expected_revision: Revision(1),
        kind: ControlKind::Stop,
        input: json!({}),
        force_latest: false,
    };
    let ack = ready.store.request_control(&actor, &request).await.unwrap();
    ready.store.recover_durable(2).await.unwrap();
    assert_eq!(
        scalar_text(&ready.conn, "SELECT status FROM rt_rooms").await,
        "stopped"
    );
    assert_eq!(
        scalar_text(&ready.conn, "SELECT step FROM rt_control_operations").await,
        "done"
    );
    assert_eq!(
        ready.store.request_control(&actor, &request).await.unwrap(),
        ack
    );
}

#[tokio::test]
async fn storage_fix_timeout_freezes_but_still_requires_cleanup() {
    let ready = ready_open().await;
    ready.clock.set(5000).unwrap();
    let fence = input(&ready, &ready.seats[0], 5000).fence;
    let closed = ready
        .store
        .close_phase(CloseInput {
            room_id: ready.room,
            phase_id: ready.phase,
            fence: fence.clone(),
            deadline_mono: 5000,
            cancel_unfinished: false,
        })
        .await
        .unwrap();
    assert!(closed.frozen);
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT COUNT(*) FROM rt_attempts WHERE state='timed_out' AND cleanup_state='cleaning'"
        )
        .await,
        1
    );
    let publish = PublishInput {
        room_id: ready.room,
        phase_id: ready.phase,
        fence,
        deadline_mono: 5000,
        cleanup_confirmed: true,
    };
    assert_eq!(
        ready
            .store
            .publish(publish.clone())
            .await
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("cleanup_incomplete")
    );
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET cleanup_state='confirmed'")
        .await
        .unwrap();
    ready.store.publish(publish).await.unwrap();
}

#[tokio::test]
async fn storage_fix_restart_input_is_queued_and_total_quota_precedes_revocation() {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    let ready = ready(9900, &[("member", 0, "failed")], 1, 1, 10).await;
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../docs/roundtable/fixtures/config.json"
    ))
    .unwrap();
    let hex = fixture["cases"][0]["canonical_utf8_hex"].as_str().unwrap();
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let mut config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    config["quotas"]["interjection_byte_limit"] = json!(5);
    ready
        .conn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE rt_rooms SET config_ref=?",
            vec![config.to_string().into()],
        ))
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET cleanup_state='confirmed'")
        .await
        .unwrap();
    let actor = ActorContext::from_trusted_entry(
        typed(9903),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "test".into(),
        },
    );
    let request = ControlRequest {
        room_id: ready.room,
        request_id: typed(9990),
        expected_revision: Revision(1),
        kind: ControlKind::RestartCurrent,
        input: json!({"text":"hello"}),
        force_latest: false,
    };
    let ack = ready.store.request_control(&actor, &request).await.unwrap();
    let done = ready
        .store
        .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
        .await
        .unwrap();
    assert_eq!(
        scalar_text(&ready.conn, "SELECT state FROM rt_user_inputs").await,
        "queued"
    );
    assert_eq!(done.status, RoomState::Paused);
    assert_eq!(
        scalar_text(
            &ready.conn,
            "SELECT state FROM rt_budget_reservations WHERE purpose='restart'"
        )
        .await,
        "released"
    );
    let rejected = ready
        .store
        .request_control(
            &actor,
            &ControlRequest {
                request_id: typed(9991),
                expected_revision: done.revision,
                input: json!({"text":"x"}),
                ..request
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        rejected.details.reason.as_deref(),
        Some("interjection_bytes")
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_control_operations").await,
        1
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT run_epoch FROM rt_rooms").await,
        2
    );
}

fn resume_config() -> roundtable_protocol::RoundtableConfigV1 {
    serde_json::from_value(json!({
        "schema_version":1,"topic":"resume","workspace_id":"test","source_refs":[],
        "participants":[
            {"ordinal":0,"role":"reviewer","provider_ref":"provider:1"},
            {"ordinal":1,"role":"critic","provider_ref":"provider:2"}
        ],"moderator_ordinal":0,"strategy":{"type":"phased_rounds","version":1,"critique_rounds":0},
        "concurrency":2,"strict_snapshot_v1":true,
        "budgets":{"room_budget":"1000","phase_budget":"1000"},
        "timeouts":{"attempt_timeout":"100"},
        "quotas":{"output_byte_limit":8192,"input_byte_limit":16384,"interjection_byte_limit":16384}
    }))
    .unwrap()
}

#[tokio::test]
async fn storage_fix_retry_synthesis_reserves_one_slot_from_room_not_old_phase() {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    for (salt, old_remaining) in [(11600, 0), (11800, 1000)] {
        let ready = ready(salt, &[("moderator", 2, "failed")], 1, 1, 10).await;
        ready.conn.execute(Statement::from_sql_and_values(DatabaseBackend::Sqlite,
            "UPDATE rt_rooms SET status='paused',blocked_reason='synthesis_failed',remaining_active_ms=100,config_ref=?",
            [serde_json::to_string(&resume_config()).unwrap().into()])).await.unwrap();
        ready
            .conn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "UPDATE rt_phases SET phase_index=1,snapshot_ref='synthesis',remaining_ms=?",
                [old_remaining.into()],
            ))
            .await
            .unwrap();
        ready
            .conn
            .execute_unprepared("UPDATE rt_attempts SET cleanup_state='confirmed'")
            .await
            .unwrap();
        let actor = ActorContext::from_trusted_entry(
            typed(salt + 3),
            OperatorScope::SingleOperator,
            ClientIdentity {
                kind: ClientKind::Web,
                session_ref: "retry-budget".into(),
            },
        );
        let request = ControlRequest {
            room_id: ready.room,
            request_id: typed(salt + 90),
            expected_revision: Revision(1),
            kind: ControlKind::RetrySynthesis,
            input: json!({}),
            force_latest: false,
        };
        let ack = ready.store.request_control(&actor, &request).await.unwrap();
        assert_eq!(
            scalar_i64(
                &ready.conn,
                "SELECT amount_ms FROM rt_budget_reservations WHERE purpose='restart'"
            )
            .await,
            100
        );
        let done = ready
            .store
            .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
            .await
            .unwrap();
        assert_eq!(done.status, RoomState::Paused);
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
            100
        );
        assert_eq!(
            scalar_i64(
                &ready.conn,
                "SELECT remaining_ms FROM rt_phases WHERE status='ready'"
            )
            .await,
            100
        );
        assert_eq!(
            scalar_i64(
                &ready.conn,
                "SELECT remaining_ms FROM rt_phases WHERE status='superseded'"
            )
            .await,
            old_remaining
        );
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_attempts").await,
            1
        );
        assert_eq!(
            scalar_i64(
                &ready.conn,
                "SELECT COUNT(DISTINCT snapshot_hash) FROM rt_phases"
            )
            .await,
            1
        );
        ready
            .store
            .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
            .await
            .unwrap();
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_phases").await,
            2
        );
    }
}

#[tokio::test]
async fn storage_fix_restart_preserves_future_minimum_and_rejects_short_room_before_revoke() {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    let ready = ready(12000, &[("member", 0, "failed")], 2, 2, 10).await;
    ready
        .conn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "UPDATE rt_rooms SET remaining_active_ms=199,config_ref=?",
            [serde_json::to_string(&resume_config()).unwrap().into()],
        ))
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET remaining_ms=0")
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET cleanup_state='confirmed'")
        .await
        .unwrap();
    let actor = ActorContext::from_trusted_entry(
        typed(12003),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "restart-budget".into(),
        },
    );
    let request = ControlRequest {
        room_id: ready.room,
        request_id: typed(12090),
        expected_revision: Revision(1),
        kind: ControlKind::RestartCurrent,
        input: json!({"text":"replacement"}),
        force_latest: false,
    };
    assert_eq!(
        ready
            .store
            .request_control(&actor, &request)
            .await
            .unwrap_err()
            .code,
        roundtable_protocol::ErrorCode::InsufficientBudget
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT run_epoch FROM rt_rooms").await,
        1
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_control_operations").await,
        0
    );
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=200")
        .await
        .unwrap();
    let ack = ready.store.request_control(&actor, &request).await.unwrap();
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT amount_ms FROM rt_budget_reservations WHERE purpose='restart'"
        )
        .await,
        200
    );
    ready
        .store
        .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT remaining_ms FROM rt_phases WHERE status='ready'"
        )
        .await,
        100
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        200
    );
    ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .unwrap();
}

async fn accepted_paused_room(salt: u32) -> Ready {
    use codeg_lib::roundtable::ControlRequest;
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    let ready = ready(
        salt,
        &[("member", 0, "validating"), ("member", 1, "validating")],
        2,
        2,
        10,
    )
    .await;
    for seat in &ready.seats {
        ready.store.accept(input(&ready, seat, 1000)).await.unwrap();
    }
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_turns WHERE status='valid' AND accepted_attempt_id IS NOT NULL").await,
        2
    );
    let actor = ActorContext::from_trusted_entry(
        typed(salt + 3),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "accepted-resume".into(),
        },
    );
    let ack = ready
        .store
        .request_control(
            &actor,
            &ControlRequest {
                room_id: ready.room,
                request_id: typed(salt + 90),
                expected_revision: Revision(
                    scalar_i64(&ready.conn, "SELECT revision FROM rt_rooms").await as u64,
                ),
                kind: ControlKind::Pause,
                input: json!({}),
                force_latest: false,
            },
        )
        .await
        .unwrap();
    let paused = ready
        .store
        .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
        .await
        .unwrap();
    assert_eq!(paused.status, RoomState::Paused);
    ready
}

#[tokio::test]
async fn storage_fix_resume_keeps_accepted_and_frozen_slots_without_repayment() {
    let ready = accepted_paused_room(11000).await;
    let retained = texts(
        &ready.conn,
        "SELECT accepted_attempt_id FROM rt_turns ORDER BY turn_id",
    )
    .await;
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET remaining_ms=0")
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=100")
        .await
        .unwrap();
    ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET status='closing'")
        .await
        .unwrap();
    ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        100
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_attempts").await,
        2
    );
    assert_eq!(
        texts(
            &ready.conn,
            "SELECT accepted_attempt_id FROM rt_turns ORDER BY turn_id"
        )
        .await,
        retained
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_messages").await,
        2
    );
    assert_eq!(
        scalar_text(&ready.conn, "SELECT snapshot_ref FROM rt_phases").await,
        "proposal"
    );
}

#[tokio::test]
async fn storage_fix_resume_keeps_real_accepted_slots_after_attempt_limit() {
    let ready = accepted_paused_room(11100).await;
    ready
        .conn
        .execute_unprepared("UPDATE rt_turns SET admitted_attempt_count=2")
        .await
        .unwrap();
    ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .unwrap();
    assert_eq!(scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_turns WHERE status='valid' AND accepted_attempt_id IS NOT NULL").await, 2);
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_attempts").await,
        2
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_messages").await,
        2
    );
}

#[tokio::test]
async fn storage_fix_resume_rejects_unreachable_quorum_after_exhausted_slot() {
    let ready = ready(
        11200,
        &[("member", 0, "failed"), ("member", 1, "failed")],
        2,
        2,
        10,
    )
    .await;
    ready.conn.execute_unprepared("UPDATE rt_turns SET admitted_attempt_count=2 WHERE speaker_id=(SELECT speaker_id FROM rt_speakers WHERE ordinal=0)").await.unwrap();
    let error = ready
        .store
        .validate_resume_for_test(&ready.room, &resume_config())
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        roundtable_protocol::ErrorCode::CannotReachQuorum
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT run_epoch FROM rt_rooms").await,
        1
    );
}

#[tokio::test]
async fn storage_fix_resume_checks_new_concurrency_room_phase_and_attempt_budget() {
    let ready = ready(
        11400,
        &[("member", 0, "failed"), ("member", 1, "failed")],
        2,
        2,
        10,
    )
    .await;
    ready
        .conn
        .execute_unprepared("UPDATE rt_turns SET admitted_attempt_count=0")
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET dispatch_state='unsent'")
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET remaining_ms=100")
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=200")
        .await
        .unwrap();
    let mut config = resume_config();
    ready
        .store
        .validate_resume_for_test(&ready.room, &config)
        .await
        .unwrap();
    config.concurrency = 1;
    assert_eq!(
        ready
            .store
            .validate_resume_for_test(&ready.room, &config)
            .await
            .unwrap_err()
            .code,
        roundtable_protocol::ErrorCode::InsufficientBudget
    );
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET remaining_ms=200")
        .await
        .unwrap();
    assert_eq!(
        ready
            .store
            .validate_resume_for_test(&ready.room, &config)
            .await
            .unwrap_err()
            .code,
        roundtable_protocol::ErrorCode::InsufficientBudget
    );
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=300")
        .await
        .unwrap();
    ready
        .store
        .validate_resume_for_test(&ready.room, &config)
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_attempts SET dispatch_state='sent'")
        .await
        .unwrap();
    assert_eq!(
        ready
            .store
            .validate_resume_for_test(&ready.room, &config)
            .await
            .unwrap_err()
            .code,
        roundtable_protocol::ErrorCode::InsufficientBudget
    );
}

#[tokio::test]
async fn storage_fix_active_budget_prepays_room_once_and_refunds_only_known_unused() {
    use codeg_lib::roundtable::ActiveBudgetLease;
    let ready = ready(10100, &[("member", 0, "failed")], 1, 1, 10).await;
    let mut lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_000
    );
    assert!(
        ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
            .await
            .is_err()
    );
    ready.clock.set(1010).unwrap();
    let sampled = lease.checkpoint().await.unwrap();
    assert_eq!(sampled.remaining_room_ms.0, 1_798_000);
    assert_eq!(sampled.prepaid_until.0, 2010);
    ready.clock.set(1410).unwrap();
    let settled = lease.finish().await.unwrap();
    assert_eq!(settled.remaining_room_ms.0, 1_798_600);
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_ms FROM rt_phases").await,
        1_798_600
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_events").await,
        0
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_measurements").await,
        3
    );
    assert!(lease.finish().await.is_err());
}

#[tokio::test]
async fn storage_fix_checkpoint_after_multi_second_stall_charges_gap_without_exhausting_room() {
    use codeg_lib::roundtable::{ActiveBudgetLease, ExecutionLease};
    let start_ms = 10_u64;
    let ready = ready(10150, &[("member", 0, "failed")], 1, 1, start_ms).await;
    let mut lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    let permission = ExecutionLease::issue(start_ms, lease.prepaid_until().saturating_sub(start_ms));
    assert_eq!(permission.prepaid_until(), start_ms + 1_000);
    // Simulate a dual-crun / SQLite stall longer than the one-second prepaid slice.
    let after_stall = start_ms + 3_500;
    ready.clock.set(after_stall).unwrap();
    let sampled = lease.checkpoint().await.unwrap();
    assert!(
        sampled.remaining_room_ms.0 > 1_700_000,
        "room wall budget must remain after charging the stall gap"
    );
    assert_eq!(sampled.prepaid_until.0, after_stall + 1_000);
    assert!(
        permission.renew_accounted(after_stall, sampled.prepaid_until.0),
        "accounted renew must recover the local execution lease after the stall"
    );
    assert_eq!(
        permission.admit_enqueue(after_stall),
        1,
        "turn admission must succeed after renewing past the stall"
    );
    lease.finish().await.unwrap();
    assert!(scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await > 1_700_000);
}

#[tokio::test]
async fn storage_fix_crash_retains_prepaid_slice_and_old_owner_cannot_refund_new_lease() {
    use codeg_lib::roundtable::ActiveBudgetLease;
    let ready = ready(10300, &[("member", 0, "failed")], 1, 1, 10).await;
    let mut old = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    ready.clock.set(410).unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET boot_epoch=2,run_epoch=2")
        .await
        .unwrap();
    let mut current = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(2), Epoch(2))
        .await
        .unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_798_000
    );
    assert!(old.finish().await.is_err());
    current.finish().await.unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_000
    );
}

#[tokio::test]
async fn storage_fix_control_epoch_can_settle_its_own_proven_unused_slice() {
    use codeg_lib::roundtable::ActiveBudgetLease;
    let ready = ready(10500, &[("member", 0, "failed")], 1, 1, 10).await;
    let mut lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    ready.clock.set(260).unwrap();
    ready
        .conn
        .execute_unprepared("UPDATE rt_rooms SET run_epoch=2,status='paused'")
        .await
        .unwrap();
    assert!(lease.checkpoint().await.is_err());
    lease.finish().await.unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_750
    );
}

#[tokio::test]
async fn lifecycle_two_accepts_wait_for_sqlite_writer_without_snapshot_upgrade() {
    use sea_orm::TransactionTrait;
    let ready = ready_with_pool(
        11000,
        &[("member", 0, "validating"), ("member", 1, "validating")],
        2,
        2,
        10,
        5,
    )
    .await;
    let writer = ready.conn.begin().await.unwrap();
    writer
        .execute_unprepared("UPDATE rt_rooms SET remaining_active_ms=remaining_active_ms")
        .await
        .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut tasks = Vec::new();
    for seat in &ready.seats {
        let store = ready.store.clone();
        let accept = input(&ready, seat, 1000);
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.accept(accept).await
        }));
    }
    barrier.wait().await;
    // Both consumers start while another WAL writer owns the lock. Deferred
    // read-first transactions establish stale snapshots and fail to upgrade.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    writer.commit().await.unwrap();
    for task in tasks {
        task.await
            .unwrap()
            .expect("both accepted without model retry");
    }
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT COUNT(*) FROM rt_attempts WHERE state='accepted'"
        )
        .await,
        2
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_messages").await,
        2
    );
}

#[tokio::test]
async fn lifecycle_controls_preserve_lease_through_real_cleanup() {
    use codeg_lib::roundtable::{ActiveBudgetLease, ControlRequest};
    use roundtable_protocol::{
        ActorContext, ClientIdentity, ClientKind, ControlKind, OperatorScope,
    };
    for kind in [
        ControlKind::Pause,
        ControlKind::Stop,
        ControlKind::RestartCurrent,
    ] {
        let ready = ready(11200, &[("member", 0, "active")], 1, 1, 10).await;
        let mut config = resume_config();
        config.budgets.room_budget = roundtable_protocol::DurationMs(1_800_000);
        ready
            .conn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "UPDATE rt_rooms SET config_ref=?",
                vec![serde_json::to_string(&config).unwrap().into()],
            ))
            .await
            .unwrap();
        let _lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
            .await
            .unwrap();
        ready.clock.set(260).unwrap();
        let actor = ActorContext::from_trusted_entry(
            typed(11203),
            OperatorScope::SingleOperator,
            ClientIdentity {
                kind: ClientKind::Web,
                session_ref: "lease-control".into(),
            },
        );
        let request = ControlRequest {
            room_id: ready.room,
            request_id: typed(11290),
            expected_revision: Revision(1),
            kind,
            input: json!({"text":"Reconsider the current phase"}),
            force_latest: false,
        };
        let ack = ready.store.request_control(&actor, &request).await.unwrap();
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_active_time_leases").await,
            1
        );
        assert!(ready
            .store
            .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), false)
            .await
            .is_err());
        ready.clock.set(610).unwrap();
        ready
            .store
            .advance_durable_control(&actor, ready.room, ack.operation_id.unwrap(), true)
            .await
            .unwrap();
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
            1_799_400
        );
        assert_eq!(
            scalar_i64(&ready.conn, "SELECT COUNT(*) FROM rt_active_time_leases").await,
            0
        );
        assert_eq!(
            scalar_i64(
                &ready.conn,
                "SELECT MAX(sampled_active_ms) FROM rt_measurements WHERE kind='active'"
            )
            .await,
            600
        );
        assert_eq!(
            ready
                .store
                .projection(&ready.room, None)
                .await
                .unwrap()
                .body
                .sampled_active_ms
                .0,
            600
        );
    }
}

#[tokio::test]
async fn lifecycle_phase_budget_expiry_preserves_paid_room_cleanup_time() {
    use codeg_lib::roundtable::ActiveBudgetLease;
    let ready = ready(11400, &[("member", 0, "active")], 1, 1, 10).await;
    ready
        .conn
        .execute_unprepared("UPDATE rt_phases SET remaining_ms=250")
        .await
        .unwrap();
    let mut lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    ready.clock.set(260).unwrap();
    let ledger = lease.checkpoint().await.unwrap();
    assert_eq!(ledger.remaining_phase_ms.0, 0);
    assert!(
        ledger.prepaid_until.0 > ledger.last_sample_mono.0,
        "phase expiry is a close signal, not room exhaustion"
    );
    ready.clock.set(360).unwrap();
    lease.finish().await.unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_ms FROM rt_phases").await,
        0
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_650
    );
}

#[tokio::test]
async fn lifecycle_legacy_prepaid_upgrade_preserves_charge_and_phase_reservation() {
    use codeg_lib::roundtable::ActiveBudgetLease;
    let ready = ready(11600, &[("member", 0, "active")], 1, 1, 10).await;
    let mut lease = ActiveBudgetLease::begin(ready.store.clone(), ready.room, Epoch(1), Epoch(1))
        .await
        .unwrap();
    ready
        .conn
        .execute_unprepared("ALTER TABLE rt_active_time_leases DROP COLUMN phase_prepaid_ms")
        .await
        .unwrap();
    migrate_roundtable(&ready.conn).await.unwrap();
    migrate_roundtable(&ready.conn).await.unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT prepaid_ms FROM rt_active_time_leases").await,
        1000
    );
    assert_eq!(
        scalar_i64(
            &ready.conn,
            "SELECT phase_prepaid_ms FROM rt_active_time_leases"
        )
        .await,
        1000
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_000
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_ms FROM rt_phases").await,
        1_799_000
    );
    ready.clock.set(260).unwrap();
    lease.finish().await.unwrap();
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_active_ms FROM rt_rooms").await,
        1_799_750
    );
    assert_eq!(
        scalar_i64(&ready.conn, "SELECT remaining_ms FROM rt_phases").await,
        1_799_750
    );
}
