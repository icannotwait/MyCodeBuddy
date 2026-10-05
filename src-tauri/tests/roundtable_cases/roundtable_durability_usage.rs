//! Usage totals and diagnostics stay inside their own records.

use std::sync::Arc;

use async_trait::async_trait;
use roundtable_protocol::{
    fold_measurement, AttemptId, CleanupProof, ErrorCode, MeasureSemantics, MeasurementV1,
    ProcessTreeProof, UsageState,
};

use codeg_lib::roundtable::{
    archive_late_measurement, migrate_roundtable, open_roundtable_store, seal_diagnostic,
    DbIdentity, DiagnosticInput, ParticipantRuntime, PreparedRoundtableConnection,
    RoundtableLaunch, RoundtableService, RuntimeIdentity, ServiceConfig,
};

use crate::roundtable_support::open_pool;

fn sample(
    dedupe: &str,
    value: Option<u64>,
    seq: Option<u64>,
    unit: &str,
    attributed: bool,
    billable: bool,
    trusted_reset: bool,
) -> MeasurementV1 {
    MeasurementV1 {
        unit: unit.to_string(),
        source: "provider".to_string(),
        scope: "attempt".to_string(),
        semantics: MeasureSemantics::Cumulative,
        counter_id: "output_tokens".to_string(),
        epoch: 1,
        seq,
        dedupe: dedupe.to_string(),
        value,
        observed_at: "2026-10-05T00:00:00Z".to_string(),
        attributed,
        trusted_reset,
        billable,
    }
}

#[test]
fn usage_scope_dedup_and_reset() {
    let mut usage = UsageState::empty();
    usage = fold_measurement(&usage, sample("a", Some(10), Some(1), "token", true, true, false));
    usage = fold_measurement(&usage, sample("b", Some(15), Some(2), "token", true, true, false));
    usage = fold_measurement(&usage, sample("c", Some(15), Some(3), "token", true, true, false));
    assert_eq!(usage.confirmed_output_tokens, Some(15));

    let mut ordered = UsageState::empty();
    ordered = fold_measurement(&ordered, sample("s2", Some(15), Some(2), "token", true, true, false));
    ordered = fold_measurement(&ordered, sample("s1", Some(10), Some(1), "token", true, true, false));
    ordered = fold_measurement(&ordered, sample("s3", Some(20), Some(3), "token", true, true, false));
    assert_eq!(ordered.confirmed_output_tokens, Some(20));

    let mut uncertain = fold_measurement(
        &UsageState::empty(),
        sample("high", Some(20), None, "token", true, true, false),
    );
    uncertain = fold_measurement(&uncertain, sample("low", Some(10), None, "token", true, true, false));
    assert!(uncertain.uncertain);
    assert_eq!(uncertain.confirmed_output_tokens, Some(20));

    let mut billed = usage.clone();
    let mut occupancy = sample("occ", Some(1000), Some(4), "occupancy", true, false, false);
    occupancy.counter_id = "occupancy".to_string();
    billed = fold_measurement(&billed, occupancy);
    assert_eq!(billed.confirmed_output_tokens, Some(15));

    let unknown = fold_measurement(&billed, sample("missing", None, Some(5), "token", true, true, false));
    assert!(unknown.unknown_total);

    let unattributed = fold_measurement(
        &unknown,
        sample("other", Some(999), Some(6), "token", false, true, false),
    );
    assert_eq!(unattributed.confirmed_output_tokens, Some(15));
}

struct IdleRuntime;

#[async_trait]
impl ParticipantRuntime for IdleRuntime {
    async fn prepare(
        &self,
        _launch: RoundtableLaunch,
    ) -> roundtable_protocol::RtResult<PreparedRoundtableConnection> {
        Err(roundtable_protocol::RtError {
            code: ErrorCode::RuntimeUnavailable,
            message: "unused".to_string(),
            retryable: false,
            current_revision: None,
            details: roundtable_protocol::ErrorDetails {
                reason: None,
                field_errors: Vec::new(),
            },
        })
    }

    async fn cancel_and_reap(
        &self,
        identity: RuntimeIdentity,
    ) -> roundtable_protocol::RtResult<CleanupProof> {
        Ok(CleanupProof {
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

#[tokio::test]
async fn late_usage_does_not_move_the_room() {
    let (dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let store = open_roundtable_store(conn).await.expect("store");
    let runtime: Arc<dyn ParticipantRuntime> = Arc::new(IdleRuntime);
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().to_path_buf(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("roundtable-usage-db").expect("identity"),
            discover: None,
        },
        store,
        runtime,
    )
    .await
    .expect("open");
    let room_before = service.room_meter();
    let attempt: AttemptId = "00000000-0000-4000-8000-0000000000aa"
        .parse()
        .expect("attempt");
    archive_late_measurement(
        &service,
        attempt,
        sample("late", Some(7), Some(1), "token", true, true, false),
    )
    .await
    .expect("archive");
    let room_after_late_usage = service.room_meter();
    assert_eq!(room_after_late_usage, room_before);
}

#[tokio::test]
async fn diagnostic_is_bounded_and_redacted() {
    let secret = "sk-live-secret";
    let thinking = "chain-of-thought";
    let assistant = format!("{secret}{}", "a".repeat(70_000));
    let diagnostic = seal_diagnostic(DiagnosticInput {
        assistant,
        thinking: Some(thinking.to_string()),
        secret: Some(secret.to_string()),
    })
    .await
    .expect("seal");
    assert!(diagnostic.assistant_excerpt_bytes <= 65536);
    assert!(diagnostic.truncated);
    assert!(diagnostic.total_bytes > 65536);
    assert!(!diagnostic.text.contains(secret));
    assert!(!diagnostic.text.contains(thinking));
    assert!(diagnostic.text.contains("[redacted]"));
}
