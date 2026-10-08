//! P09b constrained roundtable schema.
//!
//! The pool profile is the P09a connection profile. Process-crash recovery
//! (`synchronous=NORMAL`) and power-loss durability are separate. These tests
//! were written for a later run; this task does not execute them.

use std::path::Path;
use std::str::FromStr;

use codeg_lib::db::{
    migration::Migrator, roundtable_migration_registered, ConnectionProfile,
    ConnectionProfileReport, ROUNDTABLE_MIGRATION_NAME,
};
use codeg_lib::models::AgentType;
use codeg_lib::roundtable::{
    apply_roundtable_schema, durability_from_report, migrate_roundtable, open_roundtable_store,
    promises_power_loss, roundtable_table_names, verify_connection_profile, DbIdentity,
    DurabilityProfile, ExecutionGate, ExecutionPolicy, LaunchIntent, LaunchIntentStore, NewAttempt,
    NewBinding, NewClaim, NewCommand, NewEvent, NewEvidence, NewManifest, NewMessage, NewPhase,
    NewRoom, NewSpeaker, NewSubmission, NewTurn, RegistryStore, RoundtableStore, SandboxInstance,
    StoredBinding,
};
use roundtable_protocol::{BindingId, Epoch, ErrorCode, Hash256, IncarnationId, RoomId};
use sea_orm::{ConnectionTrait, TransactionTrait};
use sea_orm_migration::MigratorTrait;

use crate::roundtable_support::{
    install_legacy_sessions, legacy_session_visible, open_pool, rt_table_count, scalar_i64,
    scalar_text, table_sql,
};

struct TurnLookup<'a> {
    store: &'a RoundtableStore,
    turn_id: &'a str,
}

async fn count_active_attempts(turn: TurnLookup<'_>) -> i64 {
    turn.store
        .count_active_attempts(turn.turn_id)
        .await
        .expect("count active attempts")
}

struct Graph {
    room_id: String,
    speaker_id: String,
    phase_id: String,
    binding_id: String,
    turn_id: String,
    attempt_id: String,
    message_id: String,
    evidence_id: String,
}

#[tokio::test]
async fn roundtable_constraints_reject_cross_room_graph() {
    assert!(roundtable_migration_registered());
    assert_eq!(ROUNDTABLE_MIGRATION_NAME, "m20261003_000001_roundtable");
    assert!(!ExecutionPolicy::default().enabled);

    let (dir, conn) = open_pool(5).await;
    install_legacy_sessions(&conn).await;
    let legacy_sql = table_sql(&conn, "internal_agent_sessions").await;
    assert!(legacy_sql.contains("'title'"));
    assert!(legacy_sql.contains("'translate'"));
    assert!(!legacy_sql.contains("roundtable"));

    migrate_roundtable(&conn).await.expect("migrate");
    let tables_after_first = rt_table_count(&conn).await;
    let meta_rows = scalar_i64(&conn, "SELECT COUNT(*) FROM rt_schema_meta").await;
    assert_eq!(tables_after_first, roundtable_table_names().len() as i64);
    assert_eq!(meta_rows, 1);
    migrate_roundtable(&conn).await.expect("migrate again");
    assert_eq!(rt_table_count(&conn).await, tables_after_first);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM rt_schema_meta").await,
        meta_rows
    );
    assert_eq!(
        table_sql(&conn, "internal_agent_sessions").await,
        legacy_sql
    );
    assert!(legacy_session_visible(&conn).await);
    let roundtable_purpose = conn
        .execute_unprepared(
            "INSERT INTO internal_agent_sessions (agent_type, external_id, purpose, created_at)
             VALUES ('codex', 'rt-purpose', 'roundtable', '2026-01-01T00:00:00Z')",
        )
        .await;
    assert!(roundtable_purpose.is_err());

    let report = verify_connection_profile(&conn)
        .await
        .expect("connection profile");
    assert_eq!(report.distinct_connections, 5);
    assert!(report
        .connections
        .iter()
        .all(|c| c.foreign_keys && c.busy_timeout_ms == 5000));
    assert!(report
        .connections
        .iter()
        .all(|c| c.synchronous == "NORMAL" && c.journal_mode == "WAL" && c.cache_size == -8000));
    assert!(!dir
        .path()
        .join("roundtable")
        .join("execution-policy.json")
        .exists());
    assert!(!ExecutionGate::open(dir.path()).enabled());

    let names = table_names(&conn).await;
    for banned in [
        "rt_owner_leases",
        "rt_notification_outbox",
        "rt_dispatch_outbox",
        "rt_approvals",
    ] {
        assert!(!names.iter().any(|name| name == banned), "{banned}");
    }
    assert_eq!(
        scalar_text(
            &conn,
            "SELECT name FROM sqlite_master WHERE name = 'rt_attempts_one_active_or_uncertain'"
        )
        .await,
        "rt_attempts_one_active_or_uncertain"
    );
    let events = table_sql(&conn, "rt_events").await;
    assert!(events.contains("PRIMARY KEY (room_id, seq)"));
    assert!(events.contains("UNIQUE (event_id)"));
    assert!(table_sql(&conn, "rt_speakers")
        .await
        .contains("CHECK (length(speaker_id) > 0)"));

    let store = open_roundtable_store(conn.clone())
        .await
        .expect("open store");
    let room_a = seed_room(&store, 1).await;
    let duplicate_ordinal = store
        .insert_speaker(&speaker(&room_a.room_id, &uid(11), 0, "member"))
        .await;
    assert_eq!(
        duplicate_ordinal.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let empty_moderator = store
        .insert_speaker(&speaker(&room_a.room_id, "", 3, "moderator"))
        .await;
    assert_eq!(
        empty_moderator.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let empty_turn_speaker = store
        .insert_turn(&NewTurn {
            room_id: room_a.room_id.clone(),
            turn_id: uid(12),
            phase_id: room_a.phase_id.clone(),
            speaker_id: String::new(),
            status: "open".to_owned(),
            admitted_attempt_count: 0,
        })
        .await;
    assert_eq!(
        empty_turn_speaker.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let duplicate_phase = store
        .insert_phase(&NewPhase {
            phase_id: uid(13),
            ..phase(&room_a.room_id, &uid(14), "")
        })
        .await;
    assert_eq!(
        duplicate_phase.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let duplicate_turn = store
        .insert_turn(&NewTurn {
            turn_id: uid(15),
            ..turn(&room_a)
        })
        .await;
    assert_eq!(duplicate_turn.unwrap_err().code, ErrorCode::InvalidArgument);
    let duplicate_attempt = store
        .insert_attempt(&attempt(&room_a, &uid(16), 1, "accepted"))
        .await;
    assert_eq!(
        duplicate_attempt.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let duplicate_event = store.insert_event(&event(&room_a, 1, &uid(17))).await;
    assert_eq!(
        duplicate_event.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let duplicate_command = store.insert_command(&command(&room_a, "req-1")).await;
    assert_eq!(
        duplicate_command.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let duplicate_submission = store
        .insert_submission(&submission(&room_a, "sub-1", "other-hash"))
        .await;
    assert_eq!(
        duplicate_submission.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let second_live = store
        .insert_attempt(&attempt(&room_a, &uid(18), 2, "uncertain"))
        .await;
    assert_eq!(second_live.unwrap_err().code, ErrorCode::InvalidArgument);
    let active = TurnLookup {
        store: &store,
        turn_id: &room_a.turn_id,
    };
    assert_eq!(count_active_attempts(active).await, 1);

    let room_b = seed_room(&store, 2).await;
    let cross_room_insert = store
        .insert_claim(&NewClaim {
            room_id: room_b.room_id.clone(),
            message_id: room_a.message_id.clone(),
            local_key: "foreign-claim".to_owned(),
            statement: "cross room".to_owned(),
        })
        .await;
    assert_eq!(
        cross_room_insert.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let cross_room_evidence = store
        .link_message_evidence(&room_a.room_id, &room_a.message_id, &room_b.evidence_id)
        .await;
    assert_eq!(
        cross_room_evidence.unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    store
        .assign_current_phase(&room_a.room_id, 1, &room_a.phase_id)
        .await
        .expect("same-room phase");
    let cross_phase = store
        .assign_current_phase(&room_a.room_id, 1, &room_b.phase_id)
        .await;
    assert_eq!(cross_phase.unwrap_err().code, ErrorCode::InvalidArgument);

    failed_roundtable_migration_leaves_old_sessions_readable().await;
}

#[tokio::test]
async fn registered_migration_is_idempotent_and_preserves_old_sessions() {
    let (_dir, conn) = open_pool(1).await;
    Migrator::up(&conn, None)
        .await
        .expect("registered migrator");
    assert!(roundtable_migration_registered());
    let tables = rt_table_count(&conn).await;
    assert_eq!(tables, roundtable_table_names().len() as i64);
    let sessions = table_sql(&conn, "internal_agent_sessions").await;
    assert!(sessions.contains("'title'"));
    assert!(sessions.contains("'translate'"));
    assert!(!sessions.contains("roundtable"));
    conn.execute_unprepared(
        "INSERT INTO internal_agent_sessions (agent_type, external_id, purpose, created_at)
         VALUES ('codex', 'ordinary-session', 'title', '2026-01-01T00:00:00Z')",
    )
    .await
    .expect("ordinary session");
    Migrator::up(&conn, None)
        .await
        .expect("second migrator pass");
    assert_eq!(rt_table_count(&conn).await, tables);
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM rt_schema_meta").await,
        1
    );
    assert!(legacy_session_visible(&conn).await);
    let store = open_roundtable_store(conn.clone())
        .await
        .expect("schema ready");
    assert_eq!(
        store.recorded_durability().await.expect("recorded"),
        DurabilityProfile::ProcessCrashRecovery
    );

    Migrator::down(&conn, Some(1))
        .await
        .expect("roll back roundtable migration");
    assert_eq!(rt_table_count(&conn).await, 0);
    assert!(legacy_session_visible(&conn).await);
    let refused = match open_roundtable_store(conn.clone()).await {
        Ok(_) => panic!("expected storage refusal"),
        Err(err) => err,
    };
    assert_eq!(refused.code, ErrorCode::StorageUnavailable);
    Migrator::up(&conn, None).await.expect("reapply");
    assert_eq!(rt_table_count(&conn).await, tables);
    assert!(legacy_session_visible(&conn).await);
}

#[tokio::test]
async fn process_crash_recovery_profile_stays_normal() {
    let (_dir, conn) = open_pool(5).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let report = verify_connection_profile(&conn).await.expect("profile");
    assert_eq!(
        durability_from_report(&report).expect("classify"),
        DurabilityProfile::ProcessCrashRecovery
    );
    assert!(!promises_power_loss(
        DurabilityProfile::ProcessCrashRecovery
    ));
    assert!(report
        .connections
        .iter()
        .all(|connection| connection.synchronous == "NORMAL"));
    let store = open_roundtable_store(conn).await.expect("store");
    assert_eq!(
        store.durability().await.expect("live"),
        store.recorded_durability().await.expect("recorded")
    );
}

#[tokio::test]
async fn power_loss_durability_is_not_selected() {
    let full = profile_report("FULL");
    let extra = profile_report("EXTRA");
    assert_eq!(
        durability_from_report(&full).expect("full"),
        DurabilityProfile::PowerLoss
    );
    assert_eq!(
        durability_from_report(&extra).expect("extra"),
        DurabilityProfile::PowerLoss
    );
    assert!(promises_power_loss(DurabilityProfile::PowerLoss));
    assert_ne!(
        durability_from_report(&profile_report("NORMAL")).expect("normal"),
        DurabilityProfile::PowerLoss
    );

    let (_dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let live = verify_connection_profile(&conn).await.expect("live");
    assert!(live
        .connections
        .iter()
        .all(|connection| connection.synchronous != "FULL" && connection.synchronous != "EXTRA"));
    let rejected = conn
        .execute_unprepared(
            "UPDATE rt_schema_meta SET durability = 'power_loss' WHERE singleton = 1",
        )
        .await;
    assert!(
        rejected.is_err(),
        "schema must not record power-loss durability"
    );
    assert_eq!(
        scalar_text(&conn, "SELECT durability FROM rt_schema_meta").await,
        "process_crash_recovery"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn rt_launch_intents_and_internal_bindings_round_trip() {
    let (_dir, conn) = open_pool(1).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let store = open_roundtable_store(conn).await.expect("store");
    let graph = seed_room(&store, 4).await;
    let intent = sample_intent();
    LaunchIntentStore::record(&store, &intent).expect("record");
    LaunchIntentStore::record(&store, &intent).expect("idempotent record");
    assert_eq!(
        LaunchIntentStore::list_unreaped(&store)
            .expect("one intent")
            .len(),
        1
    );
    let mut changed = intent.clone();
    changed.image_digest = "sha256:other".to_owned();
    assert_eq!(
        LaunchIntentStore::record(&store, &changed)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let instance = SandboxInstance {
        runtime_id: "oci:roundtable".to_owned(),
        incarnation: intent.incarnation,
        boot_epoch: intent.boot_epoch,
        image_digest: intent.image_digest.clone(),
        cgroup_handle: "codeg:roundtable-db".to_owned(),
        owner_label: intent.owner_label.clone(),
    };
    LaunchIntentStore::mark_spawned(&store, intent.incarnation, &instance).expect("spawn");
    let unreaped = LaunchIntentStore::list_unreaped(&store).expect("list");
    assert_eq!(unreaped.len(), 1);
    assert_eq!(unreaped[0].spawned.as_ref(), Some(&instance));
    LaunchIntentStore::mark_reaped(&store, intent.incarnation).expect("reap");
    assert!(LaunchIntentStore::list_unreaped(&store)
        .expect("empty")
        .is_empty());

    let binding = StoredBinding {
        room_id: parse::<RoomId>(&graph.room_id),
        binding_id: parse::<BindingId>(&graph.binding_id),
        incarnation: intent.incarnation,
        agent: AgentType::Codex,
        external_id: "member-ext".to_owned(),
        reserved_root: Path::new("C:/roundtable/reserved").to_path_buf(),
        running: true,
    };
    RegistryStore::upsert(&store, &binding).expect("registry");
    let loaded = RegistryStore::load(&store).expect("load");
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].external_id, binding.external_id);
    assert_eq!(loaded[0].room_id, binding.room_id);
    assert!(loaded[0].running);
    let mut other_room = binding.clone();
    other_room.external_id = "other-ext".to_owned();
    other_room.room_id = parse::<RoomId>(&uid(99));
    assert_eq!(
        RegistryStore::upsert(&store, &other_room).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn schema_source_does_not_depend_on_begin_immediate_or_full() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let schema = std::fs::read_to_string(root.join("src/roundtable/schema.rs")).expect("schema");
    let store = std::fs::read_to_string(root.join("src/roundtable/store.rs")).expect("store");
    for source in [&schema, &store] {
        assert!(!source.contains("BEGIN IMMEDIATE"));
        assert!(!source.contains("synchronous=FULL"));
        assert!(!source.contains("SqliteSynchronous::Full"));
    }
    assert!(schema.contains("process_crash_recovery"));
    assert!(!schema.contains("CREATE TABLE IF NOT EXISTS rt_notification_outbox"));
    assert!(!schema.contains("CREATE TABLE IF NOT EXISTS rt_owner_leases"));
    assert!(!schema.contains("CREATE TABLE IF NOT EXISTS rt_approvals"));
}

async fn failed_roundtable_migration_leaves_old_sessions_readable() {
    let (dir, conn) = open_pool(1).await;
    install_legacy_sessions(&conn).await;
    let legacy_sql = table_sql(&conn, "internal_agent_sessions").await;
    let txn = conn.begin().await.expect("begin");
    apply_roundtable_schema(&txn)
        .await
        .expect("schema in transaction");
    let broken = txn
        .execute_unprepared(
            "INSERT INTO rt_schema_meta (singleton, logical_model, durability)
             VALUES (2, 'x', 'process_crash_recovery')",
        )
        .await;
    assert!(broken.is_err());
    txn.rollback().await.expect("rollback");
    assert_eq!(rt_table_count(&conn).await, 0);
    assert_eq!(
        table_sql(&conn, "internal_agent_sessions").await,
        legacy_sql
    );
    assert!(legacy_session_visible(&conn).await);
    let refused = match open_roundtable_store(conn).await {
        Ok(_) => panic!("expected storage refusal"),
        Err(err) => err,
    };
    assert_eq!(refused.code, ErrorCode::StorageUnavailable);
    assert!(!ExecutionGate::open(dir.path()).enabled());
}

fn profile_report(synchronous: &str) -> ConnectionProfileReport {
    ConnectionProfileReport {
        distinct_connections: 5,
        connections: (0..5)
            .map(|_| ConnectionProfile {
                foreign_keys: true,
                busy_timeout_ms: 5000,
                journal_mode: "WAL".to_owned(),
                synchronous: synchronous.to_owned(),
                cache_size: -8000,
            })
            .collect(),
    }
}

async fn seed_room(store: &RoundtableStore, slot: u32) -> Graph {
    let base = slot * 20;
    let graph = Graph {
        room_id: uid(base + 1),
        speaker_id: uid(base + 2),
        phase_id: uid(base + 3),
        binding_id: uid(base + 4),
        turn_id: uid(base + 5),
        attempt_id: uid(base + 6),
        message_id: uid(base + 7),
        evidence_id: uid(base + 8),
    };
    let manifest_id = uid(base + 9);
    store
        .insert_room(&NewRoom {
            room_id: graph.room_id.clone(),
            principal_id: uid(base + 10),
            status: "draft".to_owned(),
            config_ref: "config-1".to_owned(),
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
        .insert_speaker(&speaker(&graph.room_id, &graph.speaker_id, 0, "moderator"))
        .await
        .expect("moderator");
    store
        .insert_manifest(&NewManifest {
            room_id: graph.room_id.clone(),
            manifest_id: manifest_id.clone(),
            version: 1,
            manifest_hash: hash_hex(),
            body_json: "{\"files\":[]}".to_owned(),
        })
        .await
        .expect("manifest");
    store
        .insert_phase(&phase(&graph.room_id, &graph.phase_id, &manifest_id))
        .await
        .expect("phase");
    store
        .insert_binding(&NewBinding {
            room_id: graph.room_id.clone(),
            binding_id: graph.binding_id.clone(),
            speaker_id: graph.speaker_id.clone(),
            generation: 1,
            incarnation: uid(base + 11),
            external_session_id: format!("ext-{slot}"),
            policy_ref: "policy".to_owned(),
            certificate_ref: "cert".to_owned(),
            context_state: "sealed".to_owned(),
            state: "active".to_owned(),
        })
        .await
        .expect("binding");
    store.insert_turn(&turn(&graph)).await.expect("turn");
    store
        .insert_attempt(&attempt(&graph, &graph.attempt_id, 1, "active"))
        .await
        .expect("attempt");
    store
        .insert_message(&NewMessage {
            room_id: graph.room_id.clone(),
            message_id: graph.message_id.clone(),
            attempt_id: graph.attempt_id.clone(),
            speaker_id: graph.speaker_id.clone(),
            body_json: "{\"text\":\"body\"}".to_owned(),
            body_hash: hash_hex(),
        })
        .await
        .expect("message");
    store
        .insert_claim(&NewClaim {
            room_id: graph.room_id.clone(),
            message_id: graph.message_id.clone(),
            local_key: "claim-1".to_owned(),
            statement: "a claim".to_owned(),
        })
        .await
        .expect("claim");
    store
        .insert_evidence(&NewEvidence {
            room_id: graph.room_id.clone(),
            evidence_id: graph.evidence_id.clone(),
            manifest_id,
            owner_speaker_id: graph.speaker_id.clone(),
            content_hash: hash_hex(),
            attribution: graph.speaker_id.clone(),
        })
        .await
        .expect("evidence");
    store
        .link_message_evidence(&graph.room_id, &graph.message_id, &graph.evidence_id)
        .await
        .expect("evidence link");
    store
        .insert_submission(&submission(&graph, "sub-1", &hash_hex()))
        .await
        .expect("submission");
    store
        .insert_event(&event(&graph, 1, &uid(base + 12)))
        .await
        .expect("event");
    store
        .insert_command(&command(&graph, &format!("req-{slot}")))
        .await
        .expect("command");
    graph
}

fn speaker(room_id: &str, speaker_id: &str, ordinal: i64, role: &str) -> NewSpeaker {
    NewSpeaker {
        room_id: room_id.to_owned(),
        speaker_id: speaker_id.to_owned(),
        ordinal,
        role: role.to_owned(),
        provider_ref: "provider".to_owned(),
        model_id: "model".to_owned(),
        model_snapshot_json: "{}".to_owned(),
    }
}

fn phase(room_id: &str, phase_id: &str, manifest_id: &str) -> NewPhase {
    NewPhase {
        room_id: room_id.to_owned(),
        phase_id: phase_id.to_owned(),
        phase_index: 0,
        revision: 1,
        status: "ready".to_owned(),
        snapshot_ref: "snap-1".to_owned(),
        snapshot_hash: hash_hex(),
        expected: 2,
        quorum: 2,
        remaining_ms: 450_000,
        manifest_id: if manifest_id.is_empty() {
            None
        } else {
            Some(manifest_id.to_owned())
        },
    }
}

fn turn(graph: &Graph) -> NewTurn {
    NewTurn {
        room_id: graph.room_id.clone(),
        turn_id: graph.turn_id.clone(),
        phase_id: graph.phase_id.clone(),
        speaker_id: graph.speaker_id.clone(),
        status: "open".to_owned(),
        admitted_attempt_count: 0,
    }
}

fn attempt(graph: &Graph, attempt_id: &str, attempt_no: i64, state: &str) -> NewAttempt {
    NewAttempt {
        room_id: graph.room_id.clone(),
        attempt_id: attempt_id.to_owned(),
        turn_id: graph.turn_id.clone(),
        attempt_no,
        binding_id: graph.binding_id.clone(),
        fence: 1,
        dispatch_state: "unknown".to_owned(),
        state: state.to_owned(),
        prompt_hash: hash_hex(),
        delivery_hash: hash_hex(),
        cleanup_state: "pending".to_owned(),
        residual_remote_work: 0,
    }
}

fn submission(graph: &Graph, submission_id: &str, payload_hash: &str) -> NewSubmission {
    NewSubmission {
        room_id: graph.room_id.clone(),
        attempt_id: graph.attempt_id.clone(),
        submission_id: submission_id.to_owned(),
        payload_hash: payload_hash.to_owned(),
        validation_errors_json: "[]".to_owned(),
        receipt_json: Some("{\"sealed\":false}".to_owned()),
        sealed: 0,
    }
}

fn event(graph: &Graph, seq: i64, event_id: &str) -> NewEvent {
    NewEvent {
        room_id: graph.room_id.clone(),
        seq,
        event_id: event_id.to_owned(),
        schema_version: 1,
        resulting_revision: 1,
        run_epoch: 1,
        boot_epoch: 1,
        occurred_at: "2026-10-03T00:00:00Z".to_owned(),
        projection_id: format!("projection-{seq}-{}", graph.room_id),
        projection_hash: hash_hex(),
        cause: "create".to_owned(),
        changed_entities_json: "[]".to_owned(),
        projection_body_json: "{\"schema\":\"projection_v1\"}".to_owned(),
    }
}

fn command(graph: &Graph, request_id: &str) -> NewCommand {
    NewCommand {
        principal_id: "principal-1".to_owned(),
        api_major: 1,
        request_id: request_id.to_owned(),
        canonical_request_hash: hash_hex(),
        method: "roundtable_create".to_owned(),
        room_id: graph.room_id.clone(),
        status: "completed".to_owned(),
        immutable_response: Some("{\"accepted\":true}".to_owned()),
    }
}

fn uid(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn hash_hex() -> String {
    "ab".repeat(32)
}

fn parse<T: FromStr>(text: &str) -> T
where
    <T as FromStr>::Err: std::fmt::Debug,
{
    text.parse().expect("id")
}

fn sample_intent() -> LaunchIntent {
    let incarnation = parse::<IncarnationId>(&uid(70));
    LaunchIntent {
        db: DbIdentity::new("roundtable-db").expect("db"),
        boot_epoch: Epoch(3),
        incarnation,
        owner_label: "db=roundtable-db;boot=3;incarnation=70".to_owned(),
        image_digest: "sha256:image".to_owned(),
        plan_hash: serde_json::from_str::<Hash256>(&format!("\"{}\"", hash_hex())).expect("hash"),
        spawned: None,
        reaped: false,
    }
}

async fn table_names(conn: &sea_orm::DatabaseConnection) -> Vec<String> {
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    let rows = conn
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name GLOB 'rt_*'".to_owned(),
        ))
        .await
        .expect("names");
    rows.iter()
        .map(|row| row.try_get_by_index(0).expect("name"))
        .collect()
}
