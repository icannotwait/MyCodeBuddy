//! Physical roundtable schema for design v1.1 §10.1.
//!
//! Every table uses the `rt_` prefix. Business columns and same-room foreign
//! keys stay in SQL. Immutable message bodies, receipts, and projection
//! documents may be JSON. There is no owner-lease, outbox, or approval table.
//! The recorded durability is process-crash recovery only; this module does
//! not set `synchronous`.

use sea_orm::{ConnectionTrait, DbErr};

/// Durability the roundtable migration records. `PowerLoss` is classified from
/// a connection profile only. This migration never selects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DurabilityProfile {
    /// `synchronous=NORMAL`. A process crash can recover committed transactions.
    ProcessCrashRecovery,
    /// `FULL` or `EXTRA` synchronous mode, plus a filesystem fault test this
    /// task does not perform and does not claim.
    PowerLoss,
}

pub const RECORDED_DURABILITY: &str = "process_crash_recovery";
pub const LOGICAL_MODEL: &str = "v1.1-10.1";

/// Creation order is parent-before-child. Drop uses the reverse.
pub const TABLES: &[&str] = &[
    "rt_rooms",
    "rt_speakers",
    "rt_source_manifests",
    "rt_phases",
    "rt_phase_contexts",
    "rt_bindings",
    "rt_turns",
    "rt_attempts",
    "rt_deliveries",
    "rt_submissions",
    "rt_submission_scopes",
    "rt_messages",
    "rt_message_memberships",
    "rt_claims",
    "rt_claim_ids",
    "rt_responses",
    "rt_response_details",
    "rt_position_changes",
    "rt_evidence",
    "rt_message_evidence",
    "rt_budget_reservations",
    "rt_control_operations",
    "rt_control_inputs",
    "rt_closing_sets",
    "rt_active_time_leases",
    "rt_metric_counters",
    "rt_user_inputs",
    "rt_commands",
    "rt_projection_versions",
    "rt_events",
    "rt_page_manifests",
    "rt_page_manifest_entries",
    "rt_measurements",
    "rt_usage_archive",
    "rt_diagnostics",
    "rt_launch_intents",
    "rt_internal_bindings",
    "rt_schema_meta",
];

const STATEMENTS: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS rt_metric_counters(room_id TEXT NOT NULL,key TEXT NOT NULL,value INTEGER NOT NULL,PRIMARY KEY(room_id,key),FOREIGN KEY(room_id) REFERENCES rt_rooms(room_id))",
    "CREATE TABLE IF NOT EXISTS rt_active_time_leases(room_id TEXT NOT NULL PRIMARY KEY, lease_id TEXT NOT NULL, boot_epoch INTEGER NOT NULL, run_epoch INTEGER NOT NULL, phase_id TEXT, prepaid_ms INTEGER NOT NULL CHECK(prepaid_ms>=0 AND prepaid_ms<=10000), phase_prepaid_ms INTEGER NOT NULL DEFAULT 0 CHECK(phase_prepaid_ms>=0 AND phase_prepaid_ms<=10000), last_sample_mono INTEGER NOT NULL, FOREIGN KEY(room_id) REFERENCES rt_rooms(room_id), FOREIGN KEY(room_id,phase_id) REFERENCES rt_phases(room_id,phase_id))",
    "CREATE TABLE IF NOT EXISTS rt_closing_sets (room_id TEXT NOT NULL, phase_id TEXT NOT NULL, body_json TEXT NOT NULL, recovery_boot_epoch INTEGER, recovery_operation_id TEXT, PRIMARY KEY(room_id,phase_id), FOREIGN KEY(room_id,phase_id) REFERENCES rt_phases(room_id,phase_id), FOREIGN KEY(room_id,recovery_operation_id) REFERENCES rt_control_operations(room_id,operation_id))",
    "CREATE TABLE IF NOT EXISTS rt_phase_contexts (room_id TEXT NOT NULL, phase_id TEXT NOT NULL, snapshot_json TEXT NOT NULL, context_json TEXT NOT NULL, PRIMARY KEY(room_id,phase_id), FOREIGN KEY(room_id,phase_id) REFERENCES rt_phases(room_id,phase_id))",
    "CREATE TABLE IF NOT EXISTS rt_deliveries (room_id TEXT NOT NULL, attempt_id TEXT NOT NULL, body_json TEXT NOT NULL, prompt_utf8 TEXT NOT NULL, PRIMARY KEY(room_id,attempt_id), FOREIGN KEY(room_id,attempt_id) REFERENCES rt_attempts(room_id,attempt_id))",
    "CREATE TABLE IF NOT EXISTS rt_usage_archive (
        room_id TEXT NOT NULL, measurement_id TEXT NOT NULL, attempt_id TEXT NOT NULL,
        ledger_seq INTEGER NOT NULL, body_json TEXT NOT NULL,
        PRIMARY KEY(room_id,measurement_id),
        FOREIGN KEY(room_id,attempt_id) REFERENCES rt_attempts(room_id,attempt_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_response_details (
        room_id TEXT NOT NULL, response_id TEXT NOT NULL, target_response_id TEXT, body_json TEXT NOT NULL,
        PRIMARY KEY(room_id,response_id),
        FOREIGN KEY(room_id,response_id) REFERENCES rt_responses(room_id,response_id),
        FOREIGN KEY(room_id,target_response_id) REFERENCES rt_responses(room_id,response_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_submission_scopes (
        room_id TEXT NOT NULL, attempt_id TEXT NOT NULL, scope_json TEXT NOT NULL,
        PRIMARY KEY(room_id, attempt_id),
        FOREIGN KEY(room_id, attempt_id) REFERENCES rt_attempts(room_id, attempt_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_claim_ids (
        room_id TEXT NOT NULL, claim_id TEXT NOT NULL, message_id TEXT NOT NULL, local_key TEXT NOT NULL,
        PRIMARY KEY(room_id, claim_id), UNIQUE(room_id,message_id,local_key),
        FOREIGN KEY(room_id,message_id,local_key) REFERENCES rt_claims(room_id,message_id,local_key)
    )",
    "CREATE TABLE IF NOT EXISTS rt_control_inputs (
        room_id TEXT NOT NULL, operation_id TEXT NOT NULL, input_json TEXT NOT NULL,
        PRIMARY KEY(room_id,operation_id),
        FOREIGN KEY(room_id,operation_id) REFERENCES rt_control_operations(room_id,operation_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_rooms (
        room_id TEXT NOT NULL,
        principal_id TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN (
            'draft','ready','running','pausing','paused','recovering',
            'stopping','stopped','failed','completed'
        )),
        config_ref TEXT NOT NULL,
        revision INTEGER NOT NULL,
        run_epoch INTEGER NOT NULL,
        boot_epoch INTEGER NOT NULL,
        current_phase_id TEXT,
        active_control_id TEXT,
        last_seq INTEGER NOT NULL,
        remaining_active_ms INTEGER NOT NULL,
        blocked_reason TEXT,
        result_quality TEXT,
        PRIMARY KEY (room_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_speakers (
        room_id TEXT NOT NULL,
        speaker_id TEXT NOT NULL CHECK (length(speaker_id) > 0),
        ordinal INTEGER NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('moderator','member')),
        provider_ref TEXT NOT NULL,
        model_id TEXT NOT NULL,
        model_snapshot_json TEXT NOT NULL,
        PRIMARY KEY (room_id, speaker_id),
        UNIQUE (room_id, ordinal),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS rt_speakers_one_moderator
        ON rt_speakers (room_id) WHERE role = 'moderator'",
    "CREATE TABLE IF NOT EXISTS rt_source_manifests (
        room_id TEXT NOT NULL,
        manifest_id TEXT NOT NULL,
        version INTEGER NOT NULL,
        manifest_hash TEXT NOT NULL,
        body_json TEXT NOT NULL,
        PRIMARY KEY (room_id, manifest_id),
        UNIQUE (room_id, version),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_phases (
        room_id TEXT NOT NULL,
        phase_id TEXT NOT NULL,
        phase_index INTEGER NOT NULL,
        revision INTEGER NOT NULL,
        status TEXT NOT NULL CHECK (status IN (
            'ready','running','closing','published','failed','superseded'
        )),
        snapshot_ref TEXT NOT NULL,
        snapshot_hash TEXT NOT NULL,
        expected INTEGER NOT NULL,
        quorum INTEGER NOT NULL,
        remaining_ms INTEGER NOT NULL,
        closing_set_ref TEXT,
        published_seq INTEGER,
        manifest_id TEXT,
        PRIMARY KEY (room_id, phase_id),
        UNIQUE (room_id, phase_index, revision),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id),
        FOREIGN KEY (room_id, manifest_id)
            REFERENCES rt_source_manifests (room_id, manifest_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_bindings (
        room_id TEXT NOT NULL,
        binding_id TEXT NOT NULL,
        speaker_id TEXT NOT NULL CHECK (length(speaker_id) > 0),
        generation INTEGER NOT NULL,
        incarnation TEXT NOT NULL,
        external_session_id TEXT NOT NULL,
        policy_ref TEXT NOT NULL,
        certificate_ref TEXT NOT NULL,
        context_state TEXT NOT NULL,
        state TEXT NOT NULL,
        retire_reason TEXT,
        PRIMARY KEY (room_id, binding_id),
        UNIQUE (room_id, speaker_id, generation),
        FOREIGN KEY (room_id, speaker_id) REFERENCES rt_speakers (room_id, speaker_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_turns (
        room_id TEXT NOT NULL,
        turn_id TEXT NOT NULL,
        phase_id TEXT NOT NULL,
        speaker_id TEXT NOT NULL CHECK (length(speaker_id) > 0),
        status TEXT NOT NULL CHECK (length(status) > 0),
        accepted_attempt_id TEXT,
        admitted_attempt_count INTEGER NOT NULL,
        PRIMARY KEY (room_id, turn_id),
        UNIQUE (turn_id),
        UNIQUE (room_id, phase_id, speaker_id),
        FOREIGN KEY (room_id, phase_id) REFERENCES rt_phases (room_id, phase_id),
        FOREIGN KEY (room_id, speaker_id) REFERENCES rt_speakers (room_id, speaker_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_attempts (
        room_id TEXT NOT NULL,
        attempt_id TEXT NOT NULL,
        turn_id TEXT NOT NULL,
        attempt_no INTEGER NOT NULL,
        binding_id TEXT NOT NULL,
        fence INTEGER NOT NULL,
        dispatch_state TEXT NOT NULL CHECK (dispatch_state IN ('unsent','sent','unknown')),
        state TEXT NOT NULL CHECK (state IN (
            'reserved','launching','admitting','admitted','streaming','validating',
            'accepted','invalid','failed','timed_out','interrupted','uncertain','active'
        )),
        admitted_at TEXT,
        finished_at TEXT,
        finish_reason TEXT,
        prompt_hash TEXT NOT NULL,
        delivery_hash TEXT NOT NULL,
        cleanup_state TEXT NOT NULL CHECK (cleanup_state IN ('pending','cleaning','confirmed')),
        diagnostic_ref TEXT,
        residual_remote_work INTEGER NOT NULL CHECK (residual_remote_work IN (0, 1)),
        PRIMARY KEY (room_id, attempt_id),
        UNIQUE (room_id, turn_id, attempt_no),
        FOREIGN KEY (room_id, turn_id) REFERENCES rt_turns (room_id, turn_id),
        FOREIGN KEY (room_id, binding_id) REFERENCES rt_bindings (room_id, binding_id)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS rt_attempts_one_active_or_uncertain
        ON rt_attempts (room_id, turn_id)
        WHERE state IN (
            'reserved','launching','admitting','admitted','streaming','validating',
            'active','uncertain'
        )",
    "CREATE UNIQUE INDEX IF NOT EXISTS rt_attempts_one_accepted
        ON rt_attempts (room_id, turn_id)
        WHERE state = 'accepted'",
    "CREATE TABLE IF NOT EXISTS rt_submissions (
        room_id TEXT NOT NULL,
        attempt_id TEXT NOT NULL,
        submission_id TEXT NOT NULL,
        payload_hash TEXT NOT NULL,
        candidate_ref TEXT,
        validation_errors_json TEXT NOT NULL,
        receipt_json TEXT,
        sealed INTEGER NOT NULL CHECK (sealed IN (0, 1)),
        PRIMARY KEY (room_id, attempt_id, submission_id),
        FOREIGN KEY (room_id, attempt_id) REFERENCES rt_attempts (room_id, attempt_id)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS rt_submissions_one_sealed
        ON rt_submissions (room_id, attempt_id)
        WHERE sealed = 1",
    "CREATE TABLE IF NOT EXISTS rt_messages (
        room_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        attempt_id TEXT NOT NULL,
        speaker_id TEXT NOT NULL CHECK (length(speaker_id) > 0),
        body_json TEXT NOT NULL,
        body_hash TEXT NOT NULL,
        PRIMARY KEY (room_id, message_id),
        UNIQUE (room_id, attempt_id),
        FOREIGN KEY (room_id, attempt_id) REFERENCES rt_attempts (room_id, attempt_id),
        FOREIGN KEY (room_id, speaker_id) REFERENCES rt_speakers (room_id, speaker_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_message_memberships (
        room_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        membership_version INTEGER NOT NULL,
        visibility TEXT NOT NULL CHECK (visibility IN ('staged','published','void')),
        published_seq INTEGER,
        PRIMARY KEY (room_id, message_id, membership_version),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_claims (
        room_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        local_key TEXT NOT NULL CHECK (length(local_key) > 0),
        statement TEXT NOT NULL,
        PRIMARY KEY (room_id, message_id, local_key),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_responses (
        room_id TEXT NOT NULL,
        response_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        target_message_id TEXT NOT NULL,
        target_local_key TEXT NOT NULL,
        stance TEXT NOT NULL,
        PRIMARY KEY (room_id, response_id),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id),
        FOREIGN KEY (room_id, target_message_id, target_local_key)
            REFERENCES rt_claims (room_id, message_id, local_key)
    )",
    "CREATE TABLE IF NOT EXISTS rt_position_changes (
        room_id TEXT NOT NULL,
        position_change_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        target_message_id TEXT NOT NULL,
        target_local_key TEXT NOT NULL,
        from_position TEXT NOT NULL,
        to_position TEXT NOT NULL,
        PRIMARY KEY (room_id, position_change_id),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id),
        FOREIGN KEY (room_id, target_message_id, target_local_key)
            REFERENCES rt_claims (room_id, message_id, local_key)
    )",
    "CREATE TABLE IF NOT EXISTS rt_evidence (
        room_id TEXT NOT NULL,
        evidence_id TEXT NOT NULL,
        manifest_id TEXT NOT NULL,
        owner_speaker_id TEXT NOT NULL CHECK (length(owner_speaker_id) > 0),
        content_hash TEXT NOT NULL,
        attribution TEXT NOT NULL,
        publish_seq INTEGER,
        body_json TEXT,
        PRIMARY KEY (room_id, evidence_id),
        FOREIGN KEY (room_id, manifest_id)
            REFERENCES rt_source_manifests (room_id, manifest_id),
        FOREIGN KEY (room_id, owner_speaker_id)
            REFERENCES rt_speakers (room_id, speaker_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_message_evidence (
        room_id TEXT NOT NULL,
        message_id TEXT NOT NULL,
        evidence_id TEXT NOT NULL,
        PRIMARY KEY (room_id, message_id, evidence_id),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id),
        FOREIGN KEY (room_id, evidence_id) REFERENCES rt_evidence (room_id, evidence_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_budget_reservations (
        room_id TEXT NOT NULL,
        reservation_id TEXT NOT NULL,
        principal_id TEXT NOT NULL,
        purpose TEXT NOT NULL,
        amount_ms INTEGER NOT NULL,
        amount_bytes INTEGER NOT NULL,
        state TEXT NOT NULL CHECK (length(state) > 0),
        PRIMARY KEY (room_id, reservation_id),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_control_operations (
        room_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN (
            'pause','stop','restart_current','retry_synthesis','recover'
        )),
        target_phase_id TEXT,
        target_revision INTEGER,
        requested_epoch INTEGER NOT NULL,
        step TEXT NOT NULL CHECK (step IN (
            'requested','revoking','cleaning','applying','done','blocked'
        )),
        status TEXT NOT NULL CHECK (length(status) > 0),
        blocked_reason TEXT,
        superseded_by TEXT,
        result_ref TEXT,
        budget_reservation_id TEXT,
        successor_phase_id TEXT,
        PRIMARY KEY (room_id, operation_id),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id),
        FOREIGN KEY (room_id, target_phase_id) REFERENCES rt_phases (room_id, phase_id),
        FOREIGN KEY (room_id, successor_phase_id) REFERENCES rt_phases (room_id, phase_id),
        FOREIGN KEY (room_id, budget_reservation_id)
            REFERENCES rt_budget_reservations (room_id, reservation_id),
        FOREIGN KEY (room_id, superseded_by)
            REFERENCES rt_control_operations (room_id, operation_id)
    )",
    "CREATE UNIQUE INDEX IF NOT EXISTS rt_control_one_open_non_stop
        ON rt_control_operations (room_id)
        WHERE kind <> 'stop' AND step NOT IN ('done','blocked')",
    "CREATE TABLE IF NOT EXISTS rt_user_inputs (
        room_id TEXT NOT NULL,
        input_id TEXT NOT NULL,
        text TEXT NOT NULL,
        mode TEXT NOT NULL CHECK (mode IN ('next_phase','restart_current')),
        accepted_seq INTEGER NOT NULL,
        target_phase_index INTEGER NOT NULL,
        applied_phase_id TEXT,
        applied_seq INTEGER,
        state TEXT NOT NULL CHECK (length(state) > 0),
        PRIMARY KEY (room_id, input_id),
        UNIQUE (room_id, accepted_seq),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id),
        FOREIGN KEY (room_id, applied_phase_id) REFERENCES rt_phases (room_id, phase_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_commands (
        principal_id TEXT NOT NULL,
        api_major INTEGER NOT NULL,
        request_id TEXT NOT NULL,
        canonical_request_hash TEXT NOT NULL,
        method TEXT NOT NULL,
        room_id TEXT NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('processing','completed')),
        immutable_response TEXT,
        operation_id TEXT,
        PRIMARY KEY (principal_id, api_major, request_id),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id),
        FOREIGN KEY (room_id, operation_id)
            REFERENCES rt_control_operations (room_id, operation_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_projection_versions (
        room_id TEXT NOT NULL,
        projection_id TEXT NOT NULL,
        seq INTEGER NOT NULL,
        projection_hash TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        body_json TEXT NOT NULL,
        PRIMARY KEY (room_id, projection_id),
        UNIQUE (room_id, seq),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_events (
        room_id TEXT NOT NULL,
        seq INTEGER NOT NULL,
        event_id TEXT NOT NULL,
        schema_version INTEGER NOT NULL,
        resulting_revision INTEGER NOT NULL,
        run_epoch INTEGER NOT NULL,
        boot_epoch INTEGER NOT NULL,
        occurred_at TEXT NOT NULL,
        projection_id TEXT NOT NULL,
        projection_hash TEXT NOT NULL,
        cause TEXT NOT NULL CHECK (cause IN (
            'create','config','start','attempt','accept','close',
            'publish','input','control','recovery','terminal'
        )),
        changed_entities_json TEXT NOT NULL,
        PRIMARY KEY (room_id, seq),
        UNIQUE (event_id),
        FOREIGN KEY (room_id, projection_id)
            REFERENCES rt_projection_versions (room_id, projection_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_page_manifests (
        room_id TEXT NOT NULL,
        manifest_id TEXT NOT NULL,
        projection_id TEXT NOT NULL,
        high_water_seq INTEGER NOT NULL,
        PRIMARY KEY (room_id, manifest_id),
        FOREIGN KEY (room_id, projection_id)
            REFERENCES rt_projection_versions (room_id, projection_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_page_manifest_entries (
        room_id TEXT NOT NULL,
        manifest_id TEXT NOT NULL,
        entry_offset INTEGER NOT NULL,
        message_id TEXT NOT NULL,
        body_hash TEXT NOT NULL,
        membership_version INTEGER NOT NULL,
        visibility TEXT NOT NULL CHECK (visibility IN ('staged','published','void')),
        staged INTEGER NOT NULL CHECK (staged IN (0, 1)),
        PRIMARY KEY (room_id, manifest_id, entry_offset),
        FOREIGN KEY (room_id, manifest_id)
            REFERENCES rt_page_manifests (room_id, manifest_id),
        FOREIGN KEY (room_id, message_id) REFERENCES rt_messages (room_id, message_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_measurements (
        room_id TEXT NOT NULL,
        measurement_id TEXT NOT NULL,
        ledger_seq INTEGER NOT NULL,
        sampled_active_ms INTEGER NOT NULL,
        sampled_at_utc TEXT NOT NULL,
        kind TEXT NOT NULL,
        PRIMARY KEY (room_id, measurement_id),
        UNIQUE (room_id, ledger_seq, kind),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_diagnostics (
        room_id TEXT NOT NULL,
        diagnostic_id TEXT NOT NULL,
        attempt_id TEXT,
        finish_reason TEXT NOT NULL,
        error_class TEXT NOT NULL,
        assistant_prefix TEXT NOT NULL,
        assistant_suffix TEXT NOT NULL,
        total_bytes INTEGER NOT NULL,
        stream_hash TEXT,
        truncated INTEGER NOT NULL CHECK (truncated IN (0, 1)),
        ingress_seq INTEGER NOT NULL,
        cleanup_result TEXT NOT NULL,
        redacted INTEGER NOT NULL CHECK (redacted IN (0, 1)),
        byte_len INTEGER NOT NULL,
        PRIMARY KEY (room_id, diagnostic_id),
        FOREIGN KEY (room_id, attempt_id) REFERENCES rt_attempts (room_id, attempt_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_launch_intents (
        incarnation TEXT NOT NULL,
        db_identity TEXT NOT NULL,
        boot_epoch INTEGER NOT NULL,
        owner_label TEXT NOT NULL,
        image_digest TEXT NOT NULL,
        plan_hash TEXT NOT NULL,
        reaped INTEGER NOT NULL CHECK (reaped IN (0, 1)),
        spawned INTEGER NOT NULL CHECK (spawned IN (0, 1)),
        runtime_id TEXT,
        spawned_boot_epoch INTEGER,
        spawned_image_digest TEXT,
        cgroup_handle TEXT,
        spawned_owner_label TEXT,
        PRIMARY KEY (incarnation),
        CHECK (
            (
                spawned = 0
                AND runtime_id IS NULL
                AND spawned_boot_epoch IS NULL
                AND spawned_image_digest IS NULL
                AND cgroup_handle IS NULL
                AND spawned_owner_label IS NULL
            )
            OR (
                spawned = 1
                AND runtime_id IS NOT NULL
                AND spawned_boot_epoch IS NOT NULL
                AND spawned_image_digest IS NOT NULL
                AND cgroup_handle IS NOT NULL
                AND spawned_owner_label IS NOT NULL
            )
        )
    )",
    "CREATE TABLE IF NOT EXISTS rt_internal_bindings (
        agent_type TEXT NOT NULL,
        external_id TEXT NOT NULL,
        room_id TEXT NOT NULL,
        binding_id TEXT NOT NULL,
        incarnation TEXT NOT NULL,
        reserved_root TEXT NOT NULL,
        running INTEGER NOT NULL CHECK (running IN (0, 1)),
        PRIMARY KEY (agent_type, external_id),
        FOREIGN KEY (room_id) REFERENCES rt_rooms (room_id),
        FOREIGN KEY (room_id, binding_id) REFERENCES rt_bindings (room_id, binding_id)
    )",
    "CREATE TABLE IF NOT EXISTS rt_schema_meta (
        singleton INTEGER NOT NULL CHECK (singleton = 1),
        logical_model TEXT NOT NULL,
        durability TEXT NOT NULL CHECK (durability = 'process_crash_recovery'),
        coordinator_boot INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (singleton)
    )",
    "INSERT OR IGNORE INTO rt_schema_meta (singleton, logical_model, durability)
        VALUES (1, 'v1.1-10.1', 'process_crash_recovery')",
];

pub fn roundtable_table_names() -> &'static [&'static str] {
    TABLES
}

/// Idempotent schema body shared by the registered migration and
/// [`super::migrate_roundtable`]. Callers that need atomicity wrap this in an
/// ordinary short transaction.
pub async fn apply_roundtable_schema(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
    for statement in STATEMENTS {
        conn.execute_unprepared(statement).await?;
    }
    // Existing installations used one reservation for both clocks. Preserve
    // that reservation exactly while separating room cleanup time from phase
    // execution time. This helper also runs at normal database startup.
    let columns = conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "PRAGMA table_info(rt_active_time_leases)".to_owned(),
        ))
        .await?;
    if !columns.iter().any(|column| {
        column.try_get_by_index::<String>(1).ok().as_deref() == Some("phase_prepaid_ms")
    }) {
        conn.execute_unprepared("ALTER TABLE rt_active_time_leases ADD COLUMN phase_prepaid_ms INTEGER NOT NULL DEFAULT 0 CHECK(phase_prepaid_ms>=0 AND phase_prepaid_ms<=1000)").await?;
        conn.execute_unprepared("UPDATE rt_active_time_leases SET phase_prepaid_ms=CASE WHEN phase_id IS NULL THEN 0 ELSE prepaid_ms END").await?;
    }
    widen_prepaid_cap(conn).await
}

/// The renew-ahead slice reserves up to `PREPAID_SLICE_MS` per sample. Older
/// databases capped a reservation at 1000ms in a CHECK constraint, which
/// SQLite cannot alter in place, so the (transient, one row per running room)
/// lease table is rebuilt with every row preserved. Older binaries only ever
/// write <=1000ms, so the wider cap is rollback-safe.
async fn widen_prepaid_cap(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
    let sql = conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='rt_active_time_leases'"
                .to_owned(),
        ))
        .await?
        .and_then(|row| row.try_get_by_index::<String>(0).ok())
        .unwrap_or_default();
    if !sql.contains("prepaid_ms<=1000)") {
        return Ok(());
    }
    let create = STATEMENTS
        .iter()
        .find(|statement| statement.contains("TABLE IF NOT EXISTS rt_active_time_leases("))
        .expect("lease table statement")
        .replace(
            "TABLE IF NOT EXISTS rt_active_time_leases(",
            "TABLE rt_active_time_leases_widened(",
        );
    let columns = "room_id,lease_id,boot_epoch,run_epoch,phase_id,prepaid_ms,phase_prepaid_ms,last_sample_mono";
    conn.execute_unprepared("SAVEPOINT rt_widen_prepaid_cap")
        .await?;
    let rebuilt = async {
        conn.execute_unprepared(&create).await?;
        conn.execute_unprepared(&format!(
            "INSERT INTO rt_active_time_leases_widened({columns}) SELECT {columns} FROM rt_active_time_leases"
        ))
        .await?;
        conn.execute_unprepared("DROP TABLE rt_active_time_leases")
            .await?;
        conn.execute_unprepared(
            "ALTER TABLE rt_active_time_leases_widened RENAME TO rt_active_time_leases",
        )
        .await?;
        Ok::<_, DbErr>(())
    }
    .await;
    match rebuilt {
        Ok(()) => {
            conn.execute_unprepared("RELEASE rt_widen_prepaid_cap")
                .await?;
            Ok(())
        }
        Err(error) => {
            let _ = conn
                .execute_unprepared("ROLLBACK TO rt_widen_prepaid_cap")
                .await;
            let _ = conn
                .execute_unprepared("RELEASE rt_widen_prepaid_cap")
                .await;
            Err(error)
        }
    }
}

pub async fn drop_roundtable_schema(conn: &impl ConnectionTrait) -> Result<(), DbErr> {
    for table in TABLES.iter().rev() {
        conn.execute_unprepared(&format!("DROP TABLE IF EXISTS {table}"))
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod prepaid_cap_tests {
    use super::*;

    #[test]
    fn schema_cap_matches_the_prepaid_slice() {
        let lease = STATEMENTS
            .iter()
            .find(|statement| statement.contains("TABLE IF NOT EXISTS rt_active_time_leases("))
            .unwrap();
        let cap = format!(
            "prepaid_ms<={})",
            crate::roundtable::resources::PREPAID_SLICE_MS
        );
        assert!(lease.contains(&cap), "{lease}");
    }

    #[tokio::test]
    async fn an_old_one_second_cap_is_widened_with_rows_preserved() {
        let conn = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        apply_roundtable_schema(&conn).await.unwrap();
        // Recreate the pre-widening table exactly as older installs have it.
        let old = STATEMENTS
            .iter()
            .find(|statement| statement.contains("TABLE IF NOT EXISTS rt_active_time_leases("))
            .unwrap()
            .replace("<=10000)", "<=1000)");
        conn.execute_unprepared("PRAGMA foreign_keys=OFF")
            .await
            .unwrap();
        conn.execute_unprepared("DROP TABLE rt_active_time_leases")
            .await
            .unwrap();
        conn.execute_unprepared(&old).await.unwrap();
        conn.execute_unprepared("INSERT INTO rt_active_time_leases(room_id,lease_id,boot_epoch,run_epoch,phase_id,prepaid_ms,phase_prepaid_ms,last_sample_mono) VALUES('r','l',1,2,NULL,900,0,77)").await.unwrap();
        assert!(conn
            .execute_unprepared("UPDATE rt_active_time_leases SET prepaid_ms=5000")
            .await
            .is_err());
        apply_roundtable_schema(&conn).await.unwrap();
        conn.execute_unprepared(
            "UPDATE rt_active_time_leases SET prepaid_ms=10000, phase_prepaid_ms=10000",
        )
        .await
        .unwrap();
        let row = conn
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT lease_id,run_epoch,last_sample_mono FROM rt_active_time_leases".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by_index::<String>(0).unwrap(), "l");
        assert_eq!(row.try_get_by_index::<i64>(1).unwrap(), 2);
        assert_eq!(row.try_get_by_index::<i64>(2).unwrap(), 77);
        assert!(conn
            .execute_unprepared("UPDATE rt_active_time_leases SET prepaid_ms=10001")
            .await
            .is_err());
        // Idempotent on the next startup.
        apply_roundtable_schema(&conn).await.unwrap();
    }
}
