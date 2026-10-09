//! Short roundtable transactions.
//!
//! Methods commit or roll back before returning. They do not wait on a model
//! or a process, and they use ordinary short transactions. Same-room consistency
//! is a composite foreign key or a conditional update. The product execution
//! gate is not enabled here. Pool pragmas come from the P09a connection profile;
//! `synchronous` stays whatever that profile already set, which is `NORMAL`.

use std::future::Future;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use roundtable_protocol::{Epoch, ErrorCode, Hash256, IncarnationId, RtResult};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr, QueryResult,
    Statement, TransactionTrait, TryGetable, Value,
};

use super::clock::{AcceptFaults, AcceptStep, LockGate, MonoClock, SystemMono};
use super::registry::{RegistryStore, StoredBinding};
use super::sandbox::{DbIdentity, LaunchIntent, LaunchIntentStore, SandboxInstance};
use super::{rt_error, schema};
use crate::db::{self, ConnectionProfileReport};
use crate::models::AgentType;

pub use schema::DurabilityProfile;

const ACTIVE_STATES: &str =
    "'reserved','launching','admitting','admitted','streaming','validating','active','uncertain'";

#[derive(Clone)]
pub struct RoundtableStore {
    conn: DatabaseConnection,
    clock: Arc<Mutex<Arc<dyn MonoClock>>>,
    faults: Arc<Mutex<AcceptFaults>>,
}

#[derive(Clone, Debug)]
pub struct NewRoom {
    pub room_id: String,
    pub principal_id: String,
    pub status: String,
    pub config_ref: String,
    pub revision: i64,
    pub run_epoch: i64,
    pub boot_epoch: i64,
    pub last_seq: i64,
    pub remaining_active_ms: i64,
    pub blocked_reason: Option<String>,
    pub result_quality: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewSpeaker {
    pub room_id: String,
    pub speaker_id: String,
    pub ordinal: i64,
    pub role: String,
    pub provider_ref: String,
    pub model_id: String,
    pub model_snapshot_json: String,
}

#[derive(Clone, Debug)]
pub struct NewManifest {
    pub room_id: String,
    pub manifest_id: String,
    pub version: i64,
    pub manifest_hash: String,
    pub body_json: String,
}

#[derive(Clone, Debug)]
pub struct NewPhase {
    pub room_id: String,
    pub phase_id: String,
    pub phase_index: i64,
    pub revision: i64,
    pub status: String,
    pub snapshot_ref: String,
    pub snapshot_hash: String,
    pub expected: i64,
    pub quorum: i64,
    pub remaining_ms: i64,
    pub manifest_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewBinding {
    pub room_id: String,
    pub binding_id: String,
    pub speaker_id: String,
    pub generation: i64,
    pub incarnation: String,
    pub external_session_id: String,
    pub policy_ref: String,
    pub certificate_ref: String,
    pub context_state: String,
    pub state: String,
}

#[derive(Clone, Debug)]
pub struct NewTurn {
    pub room_id: String,
    pub turn_id: String,
    pub phase_id: String,
    pub speaker_id: String,
    pub status: String,
    pub admitted_attempt_count: i64,
}

#[derive(Clone, Debug)]
pub struct NewAttempt {
    pub room_id: String,
    pub attempt_id: String,
    pub turn_id: String,
    pub attempt_no: i64,
    pub binding_id: String,
    pub fence: i64,
    pub dispatch_state: String,
    pub state: String,
    pub prompt_hash: String,
    pub delivery_hash: String,
    pub cleanup_state: String,
    pub residual_remote_work: i64,
}

#[derive(Clone, Debug)]
pub struct NewSubmission {
    pub room_id: String,
    pub attempt_id: String,
    pub submission_id: String,
    pub payload_hash: String,
    pub validation_errors_json: String,
    pub receipt_json: Option<String>,
    pub sealed: i64,
}

#[derive(Clone, Debug)]
pub struct NewMessage {
    pub room_id: String,
    pub message_id: String,
    pub attempt_id: String,
    pub speaker_id: String,
    pub body_json: String,
    pub body_hash: String,
}

#[derive(Clone, Debug)]
pub struct NewClaim {
    pub room_id: String,
    pub message_id: String,
    pub local_key: String,
    pub statement: String,
}

#[derive(Clone, Debug)]
pub struct NewEvidence {
    pub room_id: String,
    pub evidence_id: String,
    pub manifest_id: String,
    pub owner_speaker_id: String,
    pub content_hash: String,
    pub attribution: String,
}

#[derive(Clone, Debug)]
pub struct NewEvent {
    pub room_id: String,
    pub seq: i64,
    pub event_id: String,
    pub schema_version: i64,
    pub resulting_revision: i64,
    pub run_epoch: i64,
    pub boot_epoch: i64,
    pub occurred_at: String,
    pub projection_id: String,
    pub projection_hash: String,
    pub cause: String,
    pub changed_entities_json: String,
    pub projection_body_json: String,
}

#[derive(Clone, Debug)]
pub struct NewCommand {
    pub principal_id: String,
    pub api_major: i64,
    pub request_id: String,
    pub canonical_request_hash: String,
    pub method: String,
    pub room_id: String,
    pub status: String,
    pub immutable_response: Option<String>,
}

/// Open a store on an already configured pool. Missing roundtable tables refuse
/// startup. This does not migrate and does not enable the product gate.
pub async fn open_roundtable_store(conn: DatabaseConnection) -> RtResult<RoundtableStore> {
    ensure_ready(&conn).await?;
    Ok(RoundtableStore {
        conn,
        clock: Arc::new(Mutex::new(Arc::new(SystemMono::new()))),
        faults: Arc::new(Mutex::new(AcceptFaults::default())),
    })
}

impl RoundtableStore {
    /// Acquire SQLite's writer reservation before establishing a read snapshot.
    /// WAL permits concurrent readers, but a deferred read snapshot cannot be
    /// upgraded after another writer commits (SQLITE_BUSY_SNAPSHOT). The no-op
    /// write takes that reservation with the pool's bounded busy timeout.
    pub(crate) async fn write_transaction(&self) -> RtResult<DatabaseTransaction> {
        let txn = self.conn.begin().await.map_err(storage_err)?;
        if let Err(error) = exec(
            &txn,
            "UPDATE rt_rooms SET revision=revision WHERE 0",
            vec![],
        )
        .await
        {
            let _ = txn.rollback().await;
            return Err(error);
        }
        Ok(txn)
    }
}

/// Apply the registered roundtable schema in one short transaction.
///
/// Repeating the call is idempotent. A statement failure rolls the transaction
/// back, so a failed migration does not leave a partial roundtable schema and
/// does not rewrite ordinary session tables.
pub async fn migrate_roundtable(conn: &DatabaseConnection) -> RtResult<()> {
    let txn = conn.begin().await.map_err(migration_err)?;
    if let Err(err) = schema::apply_roundtable_schema(&txn).await {
        let _ = txn.rollback().await;
        return Err(migration_err(err));
    }
    txn.commit().await.map_err(migration_err)?;
    Ok(())
}

/// Read the P09a profile. This does not set pragmas and does not change
/// `synchronous` from `NORMAL` to `FULL`.
pub async fn verify_connection_profile(
    pool: &DatabaseConnection,
) -> RtResult<ConnectionProfileReport> {
    db::verify_connection_profile(pool).await
}

pub fn durability_from_report(report: &ConnectionProfileReport) -> RtResult<DurabilityProfile> {
    if report.connections.is_empty() {
        return Err(rt_error(
            ErrorCode::StorageUnavailable,
            "connection_profile",
        ));
    }
    if report
        .connections
        .iter()
        .all(|connection| connection.synchronous == "NORMAL")
    {
        return Ok(DurabilityProfile::ProcessCrashRecovery);
    }
    if report
        .connections
        .iter()
        .all(|connection| connection.synchronous == "FULL" || connection.synchronous == "EXTRA")
    {
        return Ok(DurabilityProfile::PowerLoss);
    }
    Err(rt_error(
        ErrorCode::StorageUnavailable,
        "connection_profile",
    ))
}

pub fn promises_power_loss(profile: DurabilityProfile) -> bool {
    matches!(profile, DurabilityProfile::PowerLoss)
}

impl RoundtableStore {
    /// Shared pool for roundtable modules. Callers use ordinary transactions.
    /// This does not set pragmas and does not change `synchronous`.
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.conn
    }

    pub async fn durability(&self) -> RtResult<DurabilityProfile> {
        durability_from_report(&verify_connection_profile(&self.conn).await?)
    }

    pub(crate) async fn bump_coordinator_boot(&self) -> RtResult<u64> {
        let txn = self.write_transaction().await?;
        let result = async {
            exec(
                &txn,
                "UPDATE rt_schema_meta SET coordinator_boot = coordinator_boot + 1 WHERE singleton = 1",
                vec![],
            )
            .await?;
            let boot = query_i64(
                &txn,
                "SELECT coordinator_boot FROM rt_schema_meta WHERE singleton = 1",
                vec![],
            )
            .await?;
            u64::try_from(boot).map_err(|_| rt_error(ErrorCode::InvalidArgument, "boot_epoch"))
        }
        .await;
        finish(txn, result.map(|_| ())).await?;
        self.coordinator_boot().await
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn record_launch_for_test(
        &self,
        intent: super::sandbox::LaunchIntent,
    ) -> RtResult<()> {
        self.record_launch(intent).await
    }

    pub(crate) async fn coordinator_boot(&self) -> RtResult<u64> {
        let boot = query_i64(
            &self.conn,
            "SELECT coordinator_boot FROM rt_schema_meta WHERE singleton = 1",
            vec![],
        )
        .await?;
        u64::try_from(boot).map_err(|_| rt_error(ErrorCode::InvalidArgument, "boot_epoch"))
    }

    pub async fn recorded_durability(&self) -> RtResult<DurabilityProfile> {
        let recorded = query_text(
            &self.conn,
            "SELECT durability FROM rt_schema_meta WHERE singleton = 1",
            vec![],
        )
        .await?;
        if recorded == schema::RECORDED_DURABILITY {
            Ok(DurabilityProfile::ProcessCrashRecovery)
        } else {
            Err(rt_error(
                ErrorCode::StorageUnavailable,
                "roundtable_durability",
            ))
        }
    }

    pub async fn insert_room(&self, row: &NewRoom) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_rooms (
                room_id, principal_id, status, config_ref, revision, run_epoch, boot_epoch,
                current_phase_id, active_control_id, last_seq, remaining_active_ms,
                blocked_reason, result_quality
            ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.principal_id),
                text(&row.status),
                text(&row.config_ref),
                num(row.revision),
                num(row.run_epoch),
                num(row.boot_epoch),
                num(row.last_seq),
                num(row.remaining_active_ms),
                opt(row.blocked_reason.as_deref()),
                opt(row.result_quality.as_deref()),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_speaker(&self, row: &NewSpeaker) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_speakers (
                room_id, speaker_id, ordinal, role, provider_ref, model_id, model_snapshot_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.speaker_id),
                num(row.ordinal),
                text(&row.role),
                text(&row.provider_ref),
                text(&row.model_id),
                text(&row.model_snapshot_json),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_manifest(&self, row: &NewManifest) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_source_manifests (
                room_id, manifest_id, version, manifest_hash, body_json
            ) VALUES (?, ?, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.manifest_id),
                num(row.version),
                text(&row.manifest_hash),
                text(&row.body_json),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_phase(&self, row: &NewPhase) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_phases (
                room_id, phase_id, phase_index, revision, status, snapshot_ref, snapshot_hash,
                expected, quorum, remaining_ms, closing_set_ref, published_seq, manifest_id
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
            vec![
                text(&row.room_id),
                text(&row.phase_id),
                num(row.phase_index),
                num(row.revision),
                text(&row.status),
                text(&row.snapshot_ref),
                text(&row.snapshot_hash),
                num(row.expected),
                num(row.quorum),
                num(row.remaining_ms),
                opt(row.manifest_id.as_deref()),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_binding(&self, row: &NewBinding) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_bindings (
                room_id, binding_id, speaker_id, generation, incarnation, external_session_id,
                policy_ref, certificate_ref, context_state, state, retire_reason
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
            vec![
                text(&row.room_id),
                text(&row.binding_id),
                text(&row.speaker_id),
                num(row.generation),
                text(&row.incarnation),
                text(&row.external_session_id),
                text(&row.policy_ref),
                text(&row.certificate_ref),
                text(&row.context_state),
                text(&row.state),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_turn(&self, row: &NewTurn) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_turns (
                room_id, turn_id, phase_id, speaker_id, status, accepted_attempt_id,
                admitted_attempt_count
            ) VALUES (?, ?, ?, ?, ?, NULL, ?)",
            vec![
                text(&row.room_id),
                text(&row.turn_id),
                text(&row.phase_id),
                text(&row.speaker_id),
                text(&row.status),
                num(row.admitted_attempt_count),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_attempt(&self, row: &NewAttempt) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_attempts (
                room_id, attempt_id, turn_id, attempt_no, binding_id, fence, dispatch_state,
                state, admitted_at, finished_at, finish_reason, prompt_hash, delivery_hash,
                cleanup_state, diagnostic_ref, residual_remote_work
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, NULL, ?, ?, ?, NULL, ?)",
            vec![
                text(&row.room_id),
                text(&row.attempt_id),
                text(&row.turn_id),
                num(row.attempt_no),
                text(&row.binding_id),
                num(row.fence),
                text(&row.dispatch_state),
                text(&row.state),
                text(&row.prompt_hash),
                text(&row.delivery_hash),
                text(&row.cleanup_state),
                num(row.residual_remote_work),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_submission(&self, row: &NewSubmission) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_submissions (
                room_id, attempt_id, submission_id, payload_hash, candidate_ref,
                validation_errors_json, receipt_json, sealed
            ) VALUES (?, ?, ?, ?, NULL, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.attempt_id),
                text(&row.submission_id),
                text(&row.payload_hash),
                text(&row.validation_errors_json),
                opt(row.receipt_json.as_deref()),
                num(row.sealed),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_message(&self, row: &NewMessage) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_messages (
                room_id, message_id, attempt_id, speaker_id, body_json, body_hash
            ) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.message_id),
                text(&row.attempt_id),
                text(&row.speaker_id),
                text(&row.body_json),
                text(&row.body_hash),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_claim(&self, row: &NewClaim) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_claims (room_id, message_id, local_key, statement) VALUES (?, ?, ?, ?)",
            vec![
                text(&row.room_id),
                text(&row.message_id),
                text(&row.local_key),
                text(&row.statement),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_evidence(&self, row: &NewEvidence) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_evidence (
                room_id, evidence_id, manifest_id, owner_speaker_id, content_hash,
                attribution, publish_seq, body_json
            ) VALUES (?, ?, ?, ?, ?, ?, NULL, NULL)",
            vec![
                text(&row.room_id),
                text(&row.evidence_id),
                text(&row.manifest_id),
                text(&row.owner_speaker_id),
                text(&row.content_hash),
                text(&row.attribution),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn link_message_evidence(
        &self,
        room_id: &str,
        message_id: &str,
        evidence_id: &str,
    ) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_message_evidence (room_id, message_id, evidence_id) VALUES (?, ?, ?)",
            vec![text(room_id), text(message_id), text(evidence_id)],
        )
        .await?;
        Ok(())
    }

    pub async fn insert_event(&self, row: &NewEvent) -> RtResult<()> {
        let txn = self.write_transaction().await?;
        let result = insert_event_in(&txn, row).await;
        finish(txn, result).await
    }

    pub async fn insert_command(&self, row: &NewCommand) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_commands (
                principal_id, api_major, request_id, canonical_request_hash, method, room_id,
                status, immutable_response, operation_id
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)",
            vec![
                text(&row.principal_id),
                num(row.api_major),
                text(&row.request_id),
                text(&row.canonical_request_hash),
                text(&row.method),
                text(&row.room_id),
                text(&row.status),
                opt(row.immutable_response.as_deref()),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn count_active_attempts(&self, turn_id: &str) -> RtResult<i64> {
        query_i64(
            &self.conn,
            &format!(
                "SELECT COUNT(*) FROM rt_attempts WHERE turn_id = ? AND state IN ({ACTIVE_STATES})"
            ),
            vec![text(turn_id)],
        )
        .await
    }

    /// Point the room at a phase only when that phase row is in the same room
    /// and the revision still matches. Uses an ordinary short transaction.
    pub async fn assign_current_phase(
        &self,
        room_id: &str,
        expected_revision: i64,
        phase_id: &str,
    ) -> RtResult<()> {
        let txn = self.write_transaction().await?;
        let updated = exec(
            &txn,
            "UPDATE rt_rooms
             SET current_phase_id = ?
             WHERE room_id = ?
               AND revision = ?
               AND EXISTS (
                   SELECT 1 FROM rt_phases WHERE room_id = ? AND phase_id = ?
               )",
            vec![
                text(phase_id),
                text(room_id),
                num(expected_revision),
                text(room_id),
                text(phase_id),
            ],
        )
        .await?;
        if updated != 1 {
            let _ = txn.rollback().await;
            return Err(rt_error(ErrorCode::InvalidArgument, "phase_room_mismatch"));
        }
        txn.commit().await.map_err(storage_err)?;
        Ok(())
    }

    /// Record the accepted attempt only when it belongs to this turn and room.
    pub async fn assign_accepted_attempt(
        &self,
        room_id: &str,
        turn_id: &str,
        attempt_id: &str,
    ) -> RtResult<()> {
        let txn = self.write_transaction().await?;
        let updated = exec(
            &txn,
            "UPDATE rt_turns
             SET accepted_attempt_id = ?
             WHERE room_id = ?
               AND turn_id = ?
               AND accepted_attempt_id IS NULL
               AND EXISTS (
                   SELECT 1 FROM rt_attempts
                   WHERE room_id = ?
                     AND turn_id = ?
                     AND attempt_id = ?
                     AND state = 'accepted'
               )",
            vec![
                text(attempt_id),
                text(room_id),
                text(turn_id),
                text(room_id),
                text(turn_id),
                text(attempt_id),
            ],
        )
        .await?;
        if updated != 1 {
            let _ = txn.rollback().await;
            return Err(rt_error(
                ErrorCode::InvalidArgument,
                "attempt_room_mismatch",
            ));
        }
        txn.commit().await.map_err(storage_err)?;
        Ok(())
    }

    pub fn set_clock(&self, clock: Arc<dyn MonoClock>) {
        *self.clock.lock().expect("clock") = clock;
    }

    pub fn set_accept_fault(&self, step: Option<AcceptStep>) {
        self.faults.lock().expect("faults").fail_step = step;
    }

    pub fn arm_lock_gate(&self) -> Arc<LockGate> {
        let gate = LockGate::new();
        self.faults.lock().expect("faults").lock_gate = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_accept_writer_gate(&self) -> Arc<LockGate> {
        let gate = LockGate::new();
        self.faults.lock().expect("faults").writer_gate = Some(gate.clone());
        gate
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn take_accept_writer_gate(&self) -> Option<Arc<LockGate>> {
        self.faults.lock().expect("faults").writer_gate.take()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_completed_run_gate(&self) -> Arc<LockGate> {
        let gate = LockGate::new();
        self.faults.lock().expect("faults").completed_run_gate = Some(gate.clone());
        gate
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn take_completed_run_gate(&self) -> Option<Arc<LockGate>> {
        self.faults
            .lock()
            .expect("faults")
            .completed_run_gate
            .take()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_completion_observation_gate(&self) -> Arc<LockGate> {
        let gate = LockGate::new();
        self.faults
            .lock()
            .expect("faults")
            .completion_observation_gate = Some(gate.clone());
        gate
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn take_completion_observation_gate(&self) -> Option<Arc<LockGate>> {
        self.faults
            .lock()
            .expect("faults")
            .completion_observation_gate
            .take()
    }

    /// Host suspend excluded from active time since the clock started.
    pub(crate) fn host_suspended_ms(&self) -> u64 {
        let clock = Arc::clone(&self.clock.lock().expect("clock"));
        clock.suspended_ms()
    }

    pub(crate) fn clock_sample(&self) -> (u64, String) {
        let clock = Arc::clone(&self.clock.lock().expect("clock"));
        (clock.now_ms(), clock.utc())
    }

    pub(crate) fn fault_snapshot(&self) -> AcceptFaults {
        self.faults.lock().expect("faults").clone()
    }
}

impl LaunchIntentStore for RoundtableStore {
    fn record(&self, intent: &LaunchIntent) -> RtResult<()> {
        let intent = intent.clone();
        block_on_db(async move { self.record_launch(intent).await })
    }

    fn mark_spawned(&self, incarnation: IncarnationId, instance: &SandboxInstance) -> RtResult<()> {
        let instance = instance.clone();
        block_on_db(async move { self.mark_launch_spawned(incarnation, instance).await })
    }

    fn list_unreaped(&self) -> RtResult<Vec<LaunchIntent>> {
        block_on_db(async move { self.list_unreaped_launches().await })
    }

    fn mark_reaped(&self, incarnation: IncarnationId) -> RtResult<()> {
        block_on_db(async move { self.mark_launch_reaped(incarnation).await })
    }
}

impl RegistryStore for RoundtableStore {
    fn load(&self) -> RtResult<Vec<StoredBinding>> {
        block_on_db(async move { self.load_bindings().await })
    }

    fn upsert(&self, row: &StoredBinding) -> RtResult<()> {
        let row = row.clone();
        block_on_db(async move { self.upsert_binding(row).await })
    }
}

impl RoundtableStore {
    pub(crate) async fn record_launch(&self, intent: LaunchIntent) -> RtResult<()> {
        let txn = self.write_transaction().await?;
        let result = async {
            let incarnation = intent.incarnation.to_string();
            let inserted = exec(
                &txn,
                "INSERT INTO rt_launch_intents (
                    incarnation, db_identity, boot_epoch, owner_label, image_digest, plan_hash,
                    reaped, spawned, runtime_id, spawned_boot_epoch, spawned_image_digest,
                    cgroup_handle, spawned_owner_label
                )
                SELECT ?, ?, ?, ?, ?, ?, 0, 0, NULL, NULL, NULL, NULL, NULL
                WHERE NOT EXISTS (
                    SELECT 1 FROM rt_launch_intents WHERE incarnation = ?
                )",
                vec![
                    text(&incarnation),
                    text(intent.db.canonical()),
                    num(i64_from_u64(intent.boot_epoch.0)?),
                    text(&intent.owner_label),
                    text(&intent.image_digest),
                    text(&intent.plan_hash.to_hex()),
                    text(&incarnation),
                ],
            )
            .await?;
            if inserted == 1 {
                return Ok(());
            }
            let existing = required_intent(&txn, &incarnation).await?;
            if same_launch_identity(&existing, &intent) {
                Ok(())
            } else {
                Err(rt_error(ErrorCode::InvalidArgument, "launch_conflict"))
            }
        }
        .await;
        finish(txn, result).await
    }

    pub(crate) async fn mark_launch_spawned(
        &self,
        incarnation: IncarnationId,
        instance: SandboxInstance,
    ) -> RtResult<()> {
        if instance.incarnation != incarnation {
            return Err(rt_error(ErrorCode::InvalidArgument, "launch_incarnation"));
        }
        let txn = self.write_transaction().await?;
        let key = incarnation.to_string();
        let result = async {
            let updated = exec(
                &txn,
                "UPDATE rt_launch_intents
                 SET spawned = 1,
                     runtime_id = ?,
                     spawned_boot_epoch = ?,
                     spawned_image_digest = ?,
                     cgroup_handle = ?,
                     spawned_owner_label = ?
                 WHERE incarnation = ?
                   AND (
                       spawned = 0
                       OR (
                           runtime_id = ?
                           AND spawned_boot_epoch = ?
                           AND spawned_image_digest = ?
                           AND cgroup_handle = ?
                           AND spawned_owner_label = ?
                       )
                   )",
                vec![
                    text(&instance.runtime_id),
                    num(i64_from_u64(instance.boot_epoch.0)?),
                    text(&instance.image_digest),
                    text(&instance.cgroup_handle),
                    text(&instance.owner_label),
                    text(&key),
                    text(&instance.runtime_id),
                    num(i64_from_u64(instance.boot_epoch.0)?),
                    text(&instance.image_digest),
                    text(&instance.cgroup_handle),
                    text(&instance.owner_label),
                ],
            )
            .await?;
            if updated == 1 {
                return Ok(());
            }
            let existing = required_intent(&txn, &key).await?;
            if existing.spawned.as_ref() == Some(&instance) {
                Ok(())
            } else {
                Err(rt_error(ErrorCode::InvalidArgument, "launch_conflict"))
            }
        }
        .await;
        finish(txn, result).await
    }

    pub(crate) async fn list_unreaped_launches(&self) -> RtResult<Vec<LaunchIntent>> {
        let rows = self
            .conn
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT incarnation, db_identity, boot_epoch, owner_label, image_digest, plan_hash,
                        reaped, spawned, runtime_id, spawned_boot_epoch, spawned_image_digest,
                        cgroup_handle, spawned_owner_label
                 FROM rt_launch_intents
                 WHERE reaped = 0
                 ORDER BY incarnation"
                    .to_owned(),
            ))
            .await
            .map_err(storage_err)?;
        rows.iter().map(intent_from_row).collect()
    }

    pub(crate) async fn mark_launch_reaped(&self, incarnation: IncarnationId) -> RtResult<()> {
        let txn = self.write_transaction().await?;
        let key = incarnation.to_string();
        let result = async {
            let updated = exec(
                &txn,
                "UPDATE rt_launch_intents SET reaped = 1 WHERE incarnation = ? AND reaped = 0",
                vec![text(&key)],
            )
            .await?;
            if updated == 1 {
                return Ok(());
            }
            let existing = required_intent(&txn, &key).await?;
            if existing.reaped {
                Ok(())
            } else {
                Err(rt_error(ErrorCode::InvalidArgument, "launch_conflict"))
            }
        }
        .await;
        finish(txn, result).await
    }

    pub(crate) async fn load_bindings(&self) -> RtResult<Vec<StoredBinding>> {
        let rows = self
            .conn
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT agent_type, external_id, room_id, binding_id, incarnation, reserved_root, running
                 FROM rt_internal_bindings
                 ORDER BY agent_type, external_id"
                    .to_owned(),
            ))
            .await
            .map_err(storage_err)?;
        rows.iter().map(binding_from_row).collect()
    }

    pub(crate) async fn upsert_binding(&self, row: StoredBinding) -> RtResult<()> {
        exec(
            &self.conn,
            "INSERT INTO rt_internal_bindings (
                agent_type, external_id, room_id, binding_id, incarnation, reserved_root, running
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT (agent_type, external_id) DO UPDATE SET
                room_id = excluded.room_id,
                binding_id = excluded.binding_id,
                incarnation = excluded.incarnation,
                reserved_root = excluded.reserved_root,
                running = excluded.running",
            vec![
                text(&row.agent.as_wire()),
                text(&row.external_id),
                text(&row.room_id.to_string()),
                text(&row.binding_id.to_string()),
                text(&row.incarnation.to_string()),
                text(&row.reserved_root.to_string_lossy()),
                num(if row.running { 1 } else { 0 }),
            ],
        )
        .await?;
        Ok(())
    }
}

async fn insert_event_in(conn: &DatabaseTransaction, row: &NewEvent) -> RtResult<()> {
    exec(
        conn,
        "INSERT INTO rt_projection_versions (
            room_id, projection_id, seq, projection_hash, schema_version, body_json
        ) VALUES (?, ?, ?, ?, ?, ?)",
        vec![
            text(&row.room_id),
            text(&row.projection_id),
            num(row.seq),
            text(&row.projection_hash),
            num(row.schema_version),
            text(&row.projection_body_json),
        ],
    )
    .await?;
    exec(
        conn,
        "INSERT INTO rt_events (
            room_id, seq, event_id, schema_version, resulting_revision, run_epoch, boot_epoch,
            occurred_at, projection_id, projection_hash, cause, changed_entities_json
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            text(&row.room_id),
            num(row.seq),
            text(&row.event_id),
            num(row.schema_version),
            num(row.resulting_revision),
            num(row.run_epoch),
            num(row.boot_epoch),
            text(&row.occurred_at),
            text(&row.projection_id),
            text(&row.projection_hash),
            text(&row.cause),
            text(&row.changed_entities_json),
        ],
    )
    .await?;
    Ok(())
}

async fn ensure_ready(conn: &DatabaseConnection) -> RtResult<()> {
    let present = table_names(conn).await?;
    for required in schema::TABLES {
        if !present.iter().any(|name| name == required) {
            return Err(rt_error(
                ErrorCode::StorageUnavailable,
                "roundtable_schema_missing",
            ));
        }
    }
    let durability = query_text(
        conn,
        "SELECT durability FROM rt_schema_meta WHERE singleton = 1",
        vec![],
    )
    .await?;
    if durability != schema::RECORDED_DURABILITY {
        return Err(rt_error(
            ErrorCode::StorageUnavailable,
            "roundtable_durability",
        ));
    }
    Ok(())
}

async fn table_names(conn: &impl ConnectionTrait) -> RtResult<Vec<String>> {
    let rows = conn
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name GLOB 'rt_*'".to_owned(),
        ))
        .await
        .map_err(storage_err)?;
    rows.iter()
        .map(|row| {
            row.try_get_by_index::<String>(0)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_schema"))
        })
        .collect()
}

async fn required_intent(conn: &impl ConnectionTrait, incarnation: &str) -> RtResult<LaunchIntent> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT incarnation, db_identity, boot_epoch, owner_label, image_digest, plan_hash,
                    reaped, spawned, runtime_id, spawned_boot_epoch, spawned_image_digest,
                    cgroup_handle, spawned_owner_label
             FROM rt_launch_intents WHERE incarnation = ?",
            vec![text(incarnation)],
        ))
        .await
        .map_err(storage_err)?
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "launch_missing"))?;
    intent_from_row(&row)
}

fn intent_from_row(row: &QueryResult) -> RtResult<LaunchIntent> {
    let incarnation = column::<String>(row, 0)?;
    let db_identity = column::<String>(row, 1)?;
    let boot_epoch = column::<i64>(row, 2)?;
    let owner_label = column::<String>(row, 3)?;
    let image_digest = column::<String>(row, 4)?;
    let plan_hash = column::<String>(row, 5)?;
    let reaped = column::<i64>(row, 6)?;
    let spawned = column::<i64>(row, 7)?;
    let runtime_id = column::<Option<String>>(row, 8)?;
    let spawned_boot = column::<Option<i64>>(row, 9)?;
    let spawned_image = column::<Option<String>>(row, 10)?;
    let cgroup_handle = column::<Option<String>>(row, 11)?;
    let spawned_owner = column::<Option<String>>(row, 12)?;
    let spawned = if spawned == 0 {
        None
    } else {
        Some(SandboxInstance {
            runtime_id: required_text(runtime_id)?,
            incarnation: parse_id(&incarnation)?,
            boot_epoch: Epoch(u64_from_i64(required_i64(spawned_boot)?)?),
            image_digest: required_text(spawned_image)?,
            cgroup_handle: required_text(cgroup_handle)?,
            owner_label: required_text(spawned_owner)?,
        })
    };
    Ok(LaunchIntent {
        db: DbIdentity::new(db_identity)?,
        boot_epoch: Epoch(u64_from_i64(boot_epoch)?),
        incarnation: parse_id(&incarnation)?,
        owner_label,
        image_digest,
        plan_hash: parse_hash(&plan_hash)?,
        spawned,
        reaped: reaped != 0,
    })
}

fn binding_from_row(row: &QueryResult) -> RtResult<StoredBinding> {
    let agent = column::<String>(row, 0)?;
    let external_id = column::<String>(row, 1)?;
    let room_id = column::<String>(row, 2)?;
    let binding_id = column::<String>(row, 3)?;
    let incarnation = column::<String>(row, 4)?;
    let reserved_root = column::<String>(row, 5)?;
    let running = column::<i64>(row, 6)?;
    Ok(StoredBinding {
        room_id: parse_id(&room_id)?,
        binding_id: parse_id(&binding_id)?,
        incarnation: parse_id(&incarnation)?,
        agent: AgentType::from_wire(&agent)
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "registry_agent"))?,
        external_id,
        reserved_root: PathBuf::from(reserved_root),
        running: running != 0,
    })
}

fn same_launch_identity(existing: &LaunchIntent, intent: &LaunchIntent) -> bool {
    existing.db == intent.db
        && existing.boot_epoch == intent.boot_epoch
        && existing.incarnation == intent.incarnation
        && existing.owner_label == intent.owner_label
        && existing.image_digest == intent.image_digest
        && existing.plan_hash == intent.plan_hash
}

fn parse_id<T: FromStr>(text: &str) -> RtResult<T> {
    text.parse()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_id"))
}

fn parse_hash(text: &str) -> RtResult<Hash256> {
    serde_json::from_value::<Hash256>(serde_json::Value::String(text.to_owned()))
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "plan_hash"))
}

fn required_text(value: Option<String>) -> RtResult<String> {
    value.ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "launch_spawned"))
}

fn required_i64(value: Option<i64>) -> RtResult<i64> {
    value.ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "launch_spawned"))
}

fn i64_from_u64(value: u64) -> RtResult<i64> {
    i64::try_from(value).map_err(|_| rt_error(ErrorCode::InvalidArgument, "epoch"))
}

fn u64_from_i64(value: i64) -> RtResult<u64> {
    u64::try_from(value).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "epoch"))
}

async fn finish(txn: DatabaseTransaction, result: RtResult<()>) -> RtResult<()> {
    match result {
        Ok(()) => txn.commit().await.map_err(storage_err),
        Err(err) => {
            let _ = txn.rollback().await;
            Err(err)
        }
    }
}

pub(crate) async fn exec(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<u64> {
    conn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .map(|result| result.rows_affected())
    .map_err(constraint_err)
}

pub(crate) async fn query_i64(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<i64> {
    let row = one_row(conn, sql, values).await?;
    column(&row, 0)
}

pub(crate) async fn query_text(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<String> {
    let row = one_row(conn, sql, values).await?;
    column(&row, 0)
}

pub(crate) async fn one_row(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<QueryResult> {
    optional_row(conn, sql, values)
        .await?
        .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "roundtable_row"))
}

pub(crate) async fn optional_row(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<Option<QueryResult>> {
    conn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .map_err(storage_err)
}

pub(crate) async fn rows(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<Value>,
) -> RtResult<Vec<QueryResult>> {
    conn.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .map_err(storage_err)
}

pub(crate) fn column<T: TryGetable>(row: &QueryResult, index: usize) -> RtResult<T> {
    row.try_get_by_index(index)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_column"))
}

pub(crate) fn text(value: &str) -> Value {
    value.into()
}

pub(crate) fn opt(value: Option<&str>) -> Value {
    value.map(ToOwned::to_owned).into()
}

pub(crate) fn num(value: i64) -> Value {
    value.into()
}

fn constraint_err(err: DbErr) -> roundtable_protocol::RtError {
    let rendered = err.to_string();
    if rendered.contains("constraint failed") {
        rt_error(ErrorCode::InvalidArgument, "roundtable_constraint")
    } else {
        storage_err(err)
    }
}

pub(crate) fn storage_err(err: DbErr) -> roundtable_protocol::RtError {
    if is_busy(&err.to_string()) {
        // Another writer held the lock past busy_timeout. Transient: the
        // prepaid monitor retries it until its slice ends.
        return rt_error(ErrorCode::StorageUnavailable, STORAGE_BUSY);
    }
    rt_error(ErrorCode::StorageUnavailable, "roundtable_storage")
}

pub(crate) const STORAGE_BUSY: &str = "roundtable_storage_busy";

fn is_busy(rendered: &str) -> bool {
    rendered.contains("database is locked")
        || rendered.contains("database table is locked")
        || rendered.contains("SQLITE_BUSY")
}

#[cfg(test)]
mod busy_tests {
    #[test]
    fn lock_timeouts_are_classified_as_busy() {
        assert!(super::is_busy(
            "error returned from database: (code: 5) database is locked"
        ));
        assert!(!super::is_busy(
            "UNIQUE constraint failed: rt_rooms.room_id"
        ));
    }
}

fn migration_err(err: DbErr) -> roundtable_protocol::RtError {
    let _ = err;
    rt_error(ErrorCode::StorageUnavailable, "roundtable_migration")
}

fn block_on_db<T>(future: impl Future<Output = T>) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
        Err(_) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("roundtable store runtime");
            runtime.block_on(future)
        }
    }
}
