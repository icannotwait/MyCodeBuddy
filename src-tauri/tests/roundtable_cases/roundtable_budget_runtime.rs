//! Active-room permits and prepaid room time.

use std::str::FromStr;
use std::time::Duration;

use roundtable_protocol::{
    budget_plan, CleanupProof, ErrorCode, IncarnationId, MonoMs, PermitReleaseProof,
    ProcessTreeProof, RoomId, Timeouts,
};
use sea_orm::DatabaseConnection;

use codeg_lib::db::{open_configured_sqlite, DbOpenOptions};
use codeg_lib::roundtable::{
    checkpoint_active, migrate_roundtable, open_roundtable_store, recompute_room_budget,
    AdmissionWindow, BillingSplit, ExecutionLease, ResourceAllocator, RoomClockLedger,
};

fn id(nibble: char) -> String {
    format!("00000000-0000-4000-8000-00000000000{nibble}")
}

fn room(nibble: char) -> RoomId {
    RoomId::from_str(&id(nibble)).expect("room")
}

fn proof_for(incarnation: &IncarnationId, lease_id: &str) -> PermitReleaseProof {
    PermitReleaseProof {
        lease_id: lease_id.to_string(),
        proofs: [(
            *incarnation,
            CleanupProof {
                process: ProcessTreeProof {
                    instance_id: "instance".to_string(),
                    incarnation: *incarnation,
                    process_tree_empty: true,
                },
                mailbox_empty: true,
                tools_drained: true,
                ingress_drained: true,
            },
        )]
        .into(),
    }
}

async fn open_db(dir: &std::path::Path) -> DatabaseConnection {
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

#[test]
fn active_room_permit_is_global_and_atomic() {
    let alloc = ResourceAllocator::new(3);
    let failed = alloc.try_acquire(room('a'), 4);
    assert_eq!(failed.expect_err("over").code, ErrorCode::CapacityLimited);
    let held_slots_after_failed_acquire = alloc.held_slots();
    assert_eq!(held_slots_after_failed_acquire, 0);

    let bundle = alloc.try_resume(room('a'), 3, 1).expect("resume");
    assert_eq!(bundle.slot_count(), 1);
    let other = alloc.try_acquire(room('b'), 1);
    assert_eq!(other.expect_err("other").code, ErrorCode::CapacityLimited);

    let partial = PermitReleaseProof {
        lease_id: bundle.lease_id.clone(),
        proofs: Default::default(),
    };
    assert!(alloc.release(&bundle, &partial).is_err());
    assert_eq!(alloc.held_slots(), 1);
    let incarnation = bundle.incarnations()[0];
    alloc
        .release(&bundle, &proof_for(&incarnation, &bundle.lease_id))
        .expect("reap");
    assert_eq!(alloc.held_slots(), 0);
    assert!(!alloc.quarantined());
}

#[tokio::test]
async fn prepaid_monotonic_slices() {
    let mut ledger = RoomClockLedger::new(1_800_000);
    let deadline = ledger.deadline_ms();
    ledger.observe_parallel_1s();
    ledger.jump_utc_label();
    ledger.note_queue_or_paused(5_000);
    ledger.crash_remainder(400);
    ledger.fail_storage_and_cleanup();
    assert_eq!(ledger.active_ms_after_three_parallel_1s, 1000);
    assert_eq!(ledger.deadline_ms(), deadline);
    assert_eq!(ledger.active_ms(), 1000);
    assert_eq!(ledger.refunded_ms, 0);
    assert!(ledger.prepaid_slice_ms <= 1000);
    assert_eq!(ledger.business_events, 0);
    assert!(ledger.cleaned_up);
    assert_eq!(ledger.admissions, 0);
    assert!(ledger.cleanup_overrun_ms > 0);

    let dir = tempfile::tempdir().expect("tempdir");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let sampled = checkpoint_active(&store, &room('c'), MonoMs(10), 5_000)
        .await
        .expect("checkpoint");
    assert_eq!(sampled.prepaid_until.0, 1_010);
    assert_eq!(ledger.business_events, 0);

    let kept = recompute_room_budget(3, 2, 1, 1_800_000).expect_err("raise");
    assert_eq!(kept.code, ErrorCode::InsufficientBudget);
    let plan = budget_plan(3, 2, 3, Timeouts::default()).expect("plan");
    assert!(plan.room_ms <= MAX_ALLOWANCE);
    assert_ne!(plan.room_ms, 0);

    let mut window = AdmissionWindow::default();
    assert!(window.admit());
    assert!(window.admit());
    window.pause();
    window.pause();
    assert!(!window.admit());
    window.note_launch_failure();
    window.note_launch_failure();
    assert!(window.ended());

    let billing = BillingSplit {
        acp_attempts: 4,
        model_requests: 1,
    };
    assert!(billing.attempts_are_not_requests());
}

const MAX_ALLOWANCE: u64 = 30 * 60 * 1_000;

#[test]
fn checkpoint_db_stall_expires_local_execution_lease() {
    let lease = ExecutionLease::issue(0, 1_000);
    lease.on_storage_wait(1_001);
    assert!(lease.revoked());
    assert_eq!(lease.admit_enqueue(1_001), 0);
    assert_eq!(lease.admit_forward(1_001), 0);
    assert_eq!(lease.admit_tool(1_001), 0);
    assert!(lease.remote_residual());
    assert!(!lease.ui_shows_stopped());
}

#[test]
fn late_checkpoint_ack_cannot_resurrect_expired_lease() {
    let lease = ExecutionLease::issue(0, 1_000);
    lease.on_storage_wait(1_001);
    assert!(!lease.late_ack(lease.generation(), 1_001));
    assert_eq!(lease.admit_enqueue(1_001), 0);
    assert_eq!(lease.admit_forward(1_001), 0);
    assert_eq!(lease.admit_tool(1_001), 0);
}

#[test]
fn single_proof_cannot_release_two_slots() {
    let alloc = ResourceAllocator::new(2);
    let bundle = alloc.try_acquire(room('d'), 2).expect("two");
    let one = bundle.incarnations()[0];
    let err = alloc
        .release(&bundle, &proof_for(&one, &bundle.lease_id))
        .expect_err("partial");
    assert_eq!(err.code, ErrorCode::InvalidState);
    assert!(alloc.quarantined());
    assert_eq!(alloc.held_slots(), 2);
}
