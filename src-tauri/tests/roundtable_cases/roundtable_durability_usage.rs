//! Usage totals and diagnostics stay inside their own records.

use std::sync::Arc;

use async_trait::async_trait;
use roundtable_protocol::{
    fold_measurement, CleanupProof, ErrorCode, MeasureSemantics, MeasurementV1, ProcessTreeProof,
    UsageState,
};

use codeg_lib::roundtable::{
    archive_late_measurement, seal_diagnostic, DbIdentity, DiagnosticInput, ParticipantRuntime,
    PreparedRoundtableConnection, RoundtableLaunch, RoundtableService, RuntimeIdentity,
    ServiceConfig,
};

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
    usage = fold_measurement(
        &usage,
        sample("a", Some(10), Some(1), "token", true, true, false),
    );
    usage = fold_measurement(
        &usage,
        sample("b", Some(15), Some(2), "token", true, true, false),
    );
    usage = fold_measurement(
        &usage,
        sample("c", Some(15), Some(3), "token", true, true, false),
    );
    assert_eq!(usage.confirmed_output_tokens, Some(15));

    let mut ordered = UsageState::empty();
    ordered = fold_measurement(
        &ordered,
        sample("s2", Some(15), Some(2), "token", true, true, false),
    );
    ordered = fold_measurement(
        &ordered,
        sample("s1", Some(10), Some(1), "token", true, true, false),
    );
    ordered = fold_measurement(
        &ordered,
        sample("s3", Some(20), Some(3), "token", true, true, false),
    );
    assert_eq!(ordered.confirmed_output_tokens, Some(20));

    let mut uncertain = fold_measurement(
        &UsageState::empty(),
        sample("high", Some(20), None, "token", true, true, false),
    );
    uncertain = fold_measurement(
        &uncertain,
        sample("low", Some(10), None, "token", true, true, false),
    );
    assert!(uncertain.uncertain);
    assert_eq!(uncertain.confirmed_output_tokens, Some(20));

    let mut billed = usage.clone();
    let mut occupancy = sample("occ", Some(1000), Some(4), "occupancy", true, false, false);
    occupancy.counter_id = "occupancy".to_string();
    billed = fold_measurement(&billed, occupancy);
    assert_eq!(billed.confirmed_output_tokens, Some(15));

    let unknown = fold_measurement(
        &billed,
        sample("missing", None, Some(5), "token", true, true, false),
    );
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
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let (dir, conn, store, attempt) = super::roundtable_acceptance::usage_fixture().await;
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
    archive_late_measurement(
        &service,
        attempt,
        sample("late", Some(7), Some(1), "token", true, true, false),
    )
    .await
    .expect("archive");
    archive_late_measurement(
        &service,
        attempt,
        sample("late", Some(7), Some(1), "token", true, true, false),
    )
    .await
    .expect("idempotent archive");
    let archived = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count, MAX(ledger_seq) AS version FROM rt_usage_archive",
        ))
        .await
        .expect("archive query")
        .expect("archive row");
    assert_eq!(archived.try_get::<i64>("", "count").unwrap(), 1);
    assert_eq!(archived.try_get::<i64>("", "version").unwrap(), 1);
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

#[tokio::test]
async fn storage_fix_late_usage_cursor_and_attempt_scope() {
    use roundtable_protocol::{ActorContext, ClientIdentity, ClientKind, OperatorScope};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    use serde_json::json;

    let (dir, conn, store, first) = super::roundtable_acceptance::usage_fixture().await;
    let room = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT room_id,principal_id FROM rt_rooms",
        ))
        .await
        .unwrap()
        .unwrap();
    let room_id: String = room.try_get("", "room_id").unwrap();
    let principal: String = room.try_get("", "principal_id").unwrap();
    let actor = ActorContext::from_trusted_entry(
        principal.parse().unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "usage-review".into(),
        },
    );
    let other = conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT attempt_id FROM rt_attempts WHERE attempt_id<>?",
            [first.to_string().into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let second = other
        .try_get::<String>("", "attempt_id")
        .unwrap()
        .parse()
        .unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("usage-scope-review").unwrap(),
            discover: None,
        },
        store,
        Arc::new(IdleRuntime),
    )
    .await
    .unwrap();
    conn.execute(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO rt_measurements(room_id,measurement_id,ledger_seq,sampled_active_ms,sampled_at_utc,kind) VALUES(?,'time-100',100,1000,'2026-10-05T00:00:00Z','active')",
        [room_id.clone().into()])).await.unwrap();
    let before = service
        .execute_command(
            &actor,
            "roundtable_get",
            json!({"room_id":room_id,"read":{"usage":{}}}),
        )
        .await
        .unwrap();
    assert_eq!(before["usage_version"], "100");
    archive_late_measurement(
        &service,
        first,
        sample("same-dedupe", Some(7), Some(1), "token", true, true, false),
    )
    .await
    .unwrap();
    let first_usage = service
        .execute_command(
            &actor,
            "roundtable_get",
            json!({"room_id":room_id,"read":{"usage":{"after_usage_version":"100"}}}),
        )
        .await
        .unwrap();
    assert_eq!(first_usage["usage_version"], "101");
    assert!(!first_usage["measurements"].as_array().unwrap().is_empty());
    assert_eq!(first_usage["confirmed_output_tokens"], 7);
    for _ in 0..2 {
        archive_late_measurement(
            &service,
            second,
            sample("same-dedupe", Some(9), Some(1), "token", true, true, false),
        )
        .await
        .unwrap();
    }
    let both = service
        .execute_command(
            &actor,
            "roundtable_get",
            json!({"room_id":room_id,"read":{"usage":{"after_usage_version":"101"}}}),
        )
        .await
        .unwrap();
    assert_eq!(both["usage_version"], "102");
    assert!(!both["measurements"].as_array().unwrap().is_empty());
    assert_eq!(both["confirmed_output_tokens"], 16);
    let unchanged = service
        .execute_command(
            &actor,
            "roundtable_get",
            json!({"room_id":room_id,"read":{"usage":{"after_usage_version":"102"}}}),
        )
        .await
        .unwrap();
    assert_eq!(unchanged["usage_version"], "102");
    assert!(unchanged["measurements"].as_array().unwrap().is_empty());
    assert_eq!(unchanged["confirmed_output_tokens"], 16);
}

#[tokio::test]
async fn storage_fix_diagnostic_utf8_boundaries() {
    let result = seal_diagnostic(DiagnosticInput {
        assistant: "中".repeat(30_000),
        thinking: None,
        secret: None,
    })
    .await
    .unwrap();
    assert!(result.truncated);
    assert!(result.text.len() <= 65_536);
    assert!(result.text.chars().all(|ch| ch == '中'));
}

#[tokio::test]
async fn storage_fix_shutdown_projection_contains_confirmed_cleanup() {
    use codeg_lib::roundtable::LaunchIntent;
    use roundtable_protocol::{Epoch, Hash256, RoomState};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let (dir, conn, store, _) = super::roundtable_acceptance::usage_fixture().await;
    let identity = DbIdentity::new("shutdown-projection-review").unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: identity.clone(),
            discover: None,
        },
        store.clone(),
        Arc::new(IdleRuntime),
    )
    .await
    .unwrap();
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "UPDATE rt_attempts SET state='active',cleanup_state='pending'",
    ))
    .await
    .unwrap();
    let bindings = conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT room_id,incarnation FROM rt_bindings",
        ))
        .await
        .unwrap();
    let room = bindings[0]
        .try_get::<String>("", "room_id")
        .unwrap()
        .parse()
        .unwrap();
    for binding in bindings {
        let incarnation = binding
            .try_get::<String>("", "incarnation")
            .unwrap()
            .parse()
            .unwrap();
        store
            .record_launch_for_test(LaunchIntent {
                db: identity.clone(),
                boot_epoch: Epoch(1),
                incarnation,
                owner_label: format!("shutdown-review-{incarnation}"),
                image_digest: "sha256:review-image".into(),
                plan_hash: Hash256::sha256(b"shutdown-plan"),
                spawned: None,
                reaped: false,
            })
            .await
            .unwrap();
    }
    service.shutdown().await.unwrap();
    let projection = store.projection(&room, None).await.unwrap();
    assert_eq!(projection.body.status, RoomState::Paused);
    assert_eq!(projection.body.replay.attempts.len(), 2);
    for attempt in &projection.body.replay.attempts {
        assert_eq!(attempt.state, "interrupted");
        assert_eq!(attempt.cleanup_state, "confirmed");
    }
    for binding in &projection.body.replay.bindings {
        assert_eq!(binding.state, "retired");
        assert_eq!(binding.retire_reason.as_deref(), Some("cleanup"));
    }
}
