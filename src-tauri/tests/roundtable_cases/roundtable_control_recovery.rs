//! Closing controls stay idempotent across a crash.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use roundtable_protocol::{
    BlockedReason, ControlKind, ControlStep, ErrorCode, OperationId, PhaseId, PhaseKind, PrincipalId,
    ProcessTreeProof, RoomId, RoomState,
};
use sea_orm::DatabaseConnection;

use codeg_lib::db::{open_configured_sqlite, DbOpenOptions};
use codeg_lib::roundtable::{
    apply_command, migrate_roundtable, next_phase, open_roundtable_store, pause_again,
    recover_service, recovery_action, retry_synthesis, ControlBook, DbIdentity, IsolationProvider,
    LaunchIntent, MatrixRow, MutationCommandV1, ParticipantRuntime, PreparedRoundtableConnection,
    PreparedSandbox, RecoveryAction, RecoveryState, RoomActor, RoundtableLaunch, RoundtableService,
    RuntimeIdentity, SandboxInstance, SandboxPlan, ServiceConfig, ServiceReadiness,
};

fn id(nibble: char) -> String {
    format!("00000000-0000-4000-8000-00000000000{nibble}")
}

fn typed<T: FromStr>(nibble: char) -> T
where
    T::Err: std::fmt::Debug,
{
    id(nibble).parse().expect("id")
}

fn command(
    room: RoomId,
    request: char,
    hash: &str,
    kind: ControlKind,
    budget_ok: bool,
    consent: bool,
) -> MutationCommandV1 {
    MutationCommandV1 {
        room_id: room,
        principal: typed('1'),
        api_major: 1,
        request_id: typed(request),
        canonical_hash: hash.to_string(),
        kind,
        recovery_consent: consent,
        budget_ok,
        hash_changed: false,
        requested_phase: PhaseKind::Critique,
        actual_phase: typed('a'),
    }
}

fn book(nibble: char) -> ControlBook {
    ControlBook::new(typed(nibble), typed('b'), typed('c'))
}

#[test]
fn closing_control_matrix_at_every_crash_point() {
    let mut ledger = book('d');
    let original = ledger.operation_id().clone();
    let steps = [
        ControlStep::Requested,
        ControlStep::Revoking,
        ControlStep::Cleaning,
        ControlStep::Applying,
        ControlStep::Done,
    ];
    for row in MatrixRow::ALL {
        for step in steps {
            let state = RecoveryState {
                row,
                step,
                supervisor_online: true,
                quarantine_pending: false,
            };
            let action = recovery_action(&state).expect("action");
            let outcome = ledger.reopen(row, step);
            match row {
                MatrixRow::NoControl => {
                    assert!(outcome.publish_frozen);
                    assert_eq!(action, RecoveryAction::PublishFrozen);
                }
                MatrixRow::Pause => {
                    assert!(!outcome.fill_slot);
                    assert_eq!(action, RecoveryAction::KeepSet);
                }
                MatrixRow::Restart => {
                    assert!(!outcome.publish_frozen);
                    assert_eq!(action, RecoveryAction::RestartSuccessor);
                }
                MatrixRow::Stop => {
                    assert_eq!(outcome.status, RoomState::Stopped);
                    assert_eq!(action, RecoveryAction::StopOnly);
                }
                MatrixRow::RunningCrash => {
                    assert_eq!(outcome.status, RoomState::Paused);
                    assert_eq!(action, RecoveryAction::PauseForRecovery);
                    let _ = BlockedReason::RecoveryRequired;
                }
                MatrixRow::Published => {
                    assert_eq!(outcome.published_repeats, 0);
                    assert_eq!(action, RecoveryAction::DoNotRepublish);
                }
                MatrixRow::Stopping => {
                    assert!(outcome.continues_cleanup);
                    assert_eq!(action, RecoveryAction::ContinueCleanup);
                }
                MatrixRow::LostAck => {
                    assert_eq!(outcome.resumed_operation.as_ref(), Some(&original));
                    assert_eq!(action, RecoveryAction::ResumeOperation);
                }
            }
        }
    }
    let successor_count = ledger.successor_count;
    assert_eq!(successor_count, 1);
    let published_old_revision_after_stop = ledger.published_old_revision_after_stop;
    assert_eq!(published_old_revision_after_stop, 0);

    let moderator = "moderator".to_string();
    let retry_synthesis_admitted_speakers = retry_synthesis(&mut ledger, false).expect("retry");
    assert_eq!(retry_synthesis_admitted_speakers, vec![moderator]);
    let changed = retry_synthesis(&mut ledger, true).expect_err("changed hash");
    assert_eq!(changed.code, ErrorCode::InvalidState);

    let before_restart = ledger.snapshot.clone();
    let actor = RoomActor::new(typed('d'), 1);
    let rejected = ledger
        .apply(
            &actor,
            command(typed('d'), '2', "restart", ControlKind::RestartCurrent, false, false),
        )
        .expect_err("budget");
    assert_eq!(rejected.code, ErrorCode::InsufficientBudget);
    let budget_rejected_snapshot = ledger.snapshot.clone();
    assert_eq!(budget_rejected_snapshot, before_restart);

    let phase = next_phase(&ledger, PhaseKind::Synthesis).expect_err("synthesis");
    assert_eq!(phase.code, ErrorCode::NoNextPhase);
    let actual: PhaseId = typed('c');
    assert_eq!(next_phase(&ledger, PhaseKind::Proposal).expect("actual"), actual);

    assert!(pause_again(&mut ledger).is_ok());
    let exhausted = pause_again(&mut ledger).expect_err("quorum");
    assert_eq!(exhausted.code, ErrorCode::CannotReachQuorum);

    let quiet = ledger
        .apply(
            &actor,
            command(typed('d'), '3', "quiet", ControlKind::Pause, true, false),
        )
        .expect("no prompt");
    assert!(quiet.accepted);
    assert_eq!(ledger.prompts(), 0);
    ledger.note_uncommitted_processing();
    assert_eq!(ledger.operations(), 1);
}

#[tokio::test]
async fn idempotency_processing_and_stop_supersession() {
    let room: RoomId = typed('e');
    let actor = RoomActor::new(room.clone(), 1);
    let principal: PrincipalId = typed('1');
    let first = command(room.clone(), '4', "same", ControlKind::Pause, true, true);
    let original = apply_command(&actor, principal.clone(), first.clone())
        .await
        .expect("first");
    let replay = apply_command(&actor, principal.clone(), first.clone())
        .await
        .expect("replay");
    assert_eq!(replay.operation_id, original.operation_id);
    assert_eq!(replay.request_id, original.request_id);
    let mut changed = first.clone();
    changed.canonical_hash = "different".to_string();
    let conflict = apply_command(&actor, principal.clone(), changed)
        .await
        .expect_err("hash");
    assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);
    assert_eq!(conflict.code.http_status(), 409);

    let restart_room: RoomId = typed('f');
    let restart_actor = RoomActor::new(restart_room.clone(), 1);
    apply_command(
        &restart_actor,
        principal.clone(),
        command(restart_room.clone(), '5', "r1", ControlKind::RestartCurrent, true, true),
    )
    .await
    .expect("restart");
    let second = apply_command(
        &restart_actor,
        principal.clone(),
        command(restart_room.clone(), '6', "r2", ControlKind::RestartCurrent, true, true),
    )
    .await
    .expect_err("second restart");
    assert_eq!(second.code, ErrorCode::ControlInProgress);
    apply_command(
        &restart_actor,
        principal.clone(),
        command(restart_room.clone(), '7', "stop", ControlKind::Stop, true, true),
    )
    .await
    .expect("stop covers");
    let covered = apply_command(
        &restart_actor,
        principal,
        command(restart_room, '8', "pause", ControlKind::Pause, true, true),
    )
    .await
    .expect_err("cannot cover stop");
    assert_eq!(covered.code, ErrorCode::ControlInProgress);

    let operation: OperationId = "00000000-0000-4000-8000-0000000000c1".parse().expect("op");
    let advanced = codeg_lib::roundtable::advance_control(&restart_actor, operation)
        .await
        .expect("advance");
    assert_eq!(advanced.step, ControlStep::Revoking);
}

fn unavailable() -> roundtable_protocol::RtError {
    roundtable_protocol::RtError {
        code: ErrorCode::RuntimeUnavailable,
        message: "unused".to_string(),
        retryable: false,
        current_revision: None,
        details: roundtable_protocol::ErrorDetails {
            reason: None,
            field_errors: Vec::new(),
        },
    }
}

struct EmptyDiscovery;

#[async_trait]
impl IsolationProvider for EmptyDiscovery {
    async fn prepare(&self, _plan: &SandboxPlan) -> roundtable_protocol::RtResult<PreparedSandbox> {
        Err(unavailable())
    }

    async fn spawn(
        &self,
        _prepared: &PreparedSandbox,
        _intent: &LaunchIntent,
    ) -> roundtable_protocol::RtResult<SandboxInstance> {
        Err(unavailable())
    }

    async fn discover_owned(&self, _db: &DbIdentity) -> roundtable_protocol::RtResult<Vec<SandboxInstance>> {
        Ok(Vec::new())
    }

    async fn reap(&self, _instance: &SandboxInstance) -> roundtable_protocol::RtResult<ProcessTreeProof> {
        Err(unavailable())
    }
}

struct IdleRuntime;

#[async_trait]
impl ParticipantRuntime for IdleRuntime {
    async fn prepare(
        &self,
        _launch: RoundtableLaunch,
    ) -> roundtable_protocol::RtResult<PreparedRoundtableConnection> {
        Err(unavailable())
    }

    async fn cancel_and_reap(
        &self,
        identity: RuntimeIdentity,
    ) -> roundtable_protocol::RtResult<roundtable_protocol::CleanupProof> {
        Ok(roundtable_protocol::CleanupProof {
            process: ProcessTreeProof {
                instance_id: "instance".to_string(),
                incarnation: identity.incarnation,
                process_tree_empty: true,
            },
            mailbox_empty: true,
            tools_drained: true,
            ingress_drained: true,
        })
    }
}

async fn open_db(dir: &Path) -> DatabaseConnection {
    let path = dir.join("roundtable.db");
    let url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(path.to_string_lossy().as_ref())
    );
    let conn = open_configured_sqlite(&DbOpenOptions {
        url,
        max_connections: 1,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    })
    .await
    .expect("sqlite");
    migrate_roundtable(&conn).await.expect("migrate");
    conn
}

#[tokio::test]
async fn recover_service_waits_for_a_still_supervisor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let runtime: Arc<dyn ParticipantRuntime> = Arc::new(IdleRuntime);
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().to_path_buf(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("roundtable-service-db").expect("identity"),
            discover: Some(Arc::new(EmptyDiscovery)),
        },
        store,
        runtime,
    )
    .await
    .expect("open");
    assert!(matches!(service.readiness(), ServiceReadiness::Recovering));
    service.set_supervisor_online(false);
    let waiting = recover_service(&service).await.expect("wait");
    assert!(!waiting.ready);
    assert!(matches!(service.readiness(), ServiceReadiness::Recovering));
    service.set_supervisor_online(true);
    let ready = recover_service(&service).await.expect("ready");
    assert!(ready.ready);
    assert!(matches!(service.readiness(), ServiceReadiness::Ready));
}
