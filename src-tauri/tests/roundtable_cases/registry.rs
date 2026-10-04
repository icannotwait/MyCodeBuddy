//! P07d roundtable session registry.
//!
//! Qualification keeps a temporary store. Ordinary discovery, recovery, and
//! import consult it. The production `internal_agent_sessions` purpose column
//! stays `title | translate`.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use codeg_lib::auto_title::InternalAgentSessionRegistry;
use codeg_lib::commands::conversations::{
    filter_internal_summaries, recover_stale_session_for_test, reject_internal_detail,
};
use codeg_lib::db::entities::internal_agent_session::{self, InternalAgentSessionPurpose};
use codeg_lib::db::service::{conversation_service, import_service};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::models::{AgentType, ConversationDetail, ConversationSummary};
use codeg_lib::parsers::{AgentParser, ParseError, RecoveryQuery};
use codeg_lib::roundtable::{
    downgrade_is_silent_compatible, ExternalId, InternalBindingRecord, QualificationHarness,
    RoundtableSessionRegistry,
};
use roundtable_protocol::{BindingId, IncarnationId, RoomId};
use sea_orm::{ConnectionTrait, DatabaseBackend, EntityTrait, QueryTrait, Statement};

fn id_text(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn id<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    id_text(n).parse().expect("id")
}

fn summary(id: &str, folder: &Path) -> ConversationSummary {
    ConversationSummary {
        id: id.to_string(),
        agent_type: AgentType::Codex,
        folder_path: Some(folder.to_string_lossy().into_owned()),
        folder_name: Some("member".to_string()),
        title: Some(id.to_string()),
        started_at: DateTime::<Utc>::from_timestamp_millis(1_700_000_100_000).expect("time"),
        ended_at: None,
        message_count: 1,
        model: None,
        git_branch: None,
        parent_id: None,
        parent_tool_use_id: None,
        delegation_call_id: None,
    }
}

fn detail(row: ConversationSummary) -> ConversationDetail {
    ConversationDetail {
        summary: row,
        turns: Vec::new(),
        session_stats: None,
        transcript_watermark: None,
    }
}

fn legacy_purpose(purpose: InternalAgentSessionPurpose) -> &'static str {
    match purpose {
        InternalAgentSessionPurpose::Title => "title",
        InternalAgentSessionPurpose::Translate => "translate",
    }
}

struct RankedParser {
    rows: Vec<ConversationSummary>,
}

impl AgentParser for RankedParser {
    fn list_conversations(&self) -> Result<Vec<ConversationSummary>, ParseError> {
        Ok(self.rows.clone())
    }

    fn get_conversation(&self, conversation_id: &str) -> Result<ConversationDetail, ParseError> {
        self.rows
            .iter()
            .find(|row| row.id == conversation_id)
            .cloned()
            .map(detail)
            .ok_or_else(|| ParseError::ConversationNotFound(conversation_id.to_string()))
    }

    fn recover_conversation(
        &self,
        query: &RecoveryQuery<'_>,
        accept: &dyn Fn(&ConversationSummary) -> bool,
    ) -> Result<Option<ConversationDetail>, ParseError> {
        let mut best: Option<&ConversationSummary> = None;
        for row in &self.rows {
            if !accept(row) || !query.cwd_matches(row) || query.skew(row) > query.max_skew {
                continue;
            }
            match best {
                Some(current) if query.skew(row) >= query.skew(current) => {}
                _ => best = Some(row),
            }
        }
        Ok(best.cloned().map(detail))
    }
}

#[tokio::test]
async fn discovery_before_registration_stays_hidden() {
    let harness = QualificationHarness::open("qualification-only");
    assert!(harness.registry_directory().is_dir());
    assert!(
        !harness.session_registry().is_roundtable(
            AgentType::Codex,
            &ExternalId::from("ordinary-session"),
            Path::new("D:/ordinary-work"),
        ),
        "an empty qualification store must not hide ordinary work"
    );

    let store_dir = tempfile::tempdir().expect("store");
    let registry = RoundtableSessionRegistry::temporary(store_dir.path()).expect("registry");
    let root = tempfile::tempdir().expect("root");
    let hidden_path = root.path().join("member");
    let ordinary_path = store_dir.path().join("ordinary-work");
    let root_lease = registry.reserve_root(root.path().to_path_buf());
    let discovery = registry.begin_discovery(AgentType::Codex).await;
    assert!(registry.discovery_lease_held(AgentType::Codex));
    assert!(registry.is_roundtable(
        AgentType::Codex,
        &ExternalId::from("rt-before-ack"),
        &hidden_path,
    ));
    assert!(!registry.is_roundtable(
        AgentType::Codex,
        &ExternalId::from("ordinary-session"),
        &ordinary_path,
    ));

    let db = fresh_in_memory_db().await;
    let internal_dir = tempfile::tempdir().expect("internal");
    let internal = InternalAgentSessionRegistry::empty(db.conn.clone(), internal_dir.path())
        .expect("internal registry");
    let (_, filter) = internal.shared_filter().await.expect("filter before attach");
    let hidden = summary("rt-before-ack", &hidden_path);
    let ordinary = summary("ordinary-session", &ordinary_path);
    let visible = filter_internal_summaries(
        vec![
            (AgentType::Codex, hidden.clone()),
            (AgentType::Codex, ordinary.clone()),
        ],
        &filter,
    );
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].1.id, "ordinary-session");
    assert!(reject_internal_detail(
        AgentType::Codex,
        "rt-before-ack",
        detail(hidden.clone()),
        &filter,
    )
    .is_err());

    let folder_id = seed_folder(&db, ordinary_path.to_string_lossy().as_ref()).await;
    assert!(
        !import_service::import_one_accepted_for_test(
            &db.conn,
            folder_id,
            &AgentType::Codex,
            &hidden,
        )
        .await
        .expect("skip hidden import")
    );
    assert!(
        import_service::import_one_accepted_for_test(
            &db.conn,
            folder_id,
            &AgentType::Codex,
            &ordinary,
        )
        .await
        .expect("import ordinary")
    );

    registry.attach_discovery(&internal);
    let pending = {
        let internal = Arc::clone(&internal);
        tokio::spawn(async move { internal.shared_filter().await })
    };
    let blocked = tokio::time::timeout(Duration::from_millis(200), pending).await;
    assert!(
        blocked.is_err(),
        "discovery lease must hold ordinary scans until register ack"
    );

    let rows = internal_agent_session::Entity::find()
        .all(&db.conn)
        .await
        .expect("internal rows");
    assert!(
        rows.is_empty(),
        "pre-ack hiding must not write internal_agent_sessions"
    );

    drop(discovery);
    let (_, after_lease) = internal.shared_filter().await.expect("scan after lease");
    assert!(after_lease.contains(
        AgentType::Codex,
        Some("rt-before-ack"),
        Some(hidden_path.to_string_lossy().as_ref()),
    ));
    drop(root_lease);
    assert!(!registry.is_roundtable(
        AgentType::Codex,
        &ExternalId::from("rt-before-ack"),
        &hidden_path,
    ));
}

#[tokio::test]
async fn restart_parser_and_direct_lookup_respect_registry() {
    let store_dir = tempfile::tempdir().expect("store");
    let root = tempfile::tempdir().expect("root");
    let outside = store_dir.path().join("reopened-outside-root");
    let registry = RoundtableSessionRegistry::temporary(store_dir.path()).expect("registry");
    let root_lease = registry.reserve_root(root.path().to_path_buf());
    let discovery = registry.begin_discovery(AgentType::Codex).await;
    let external = ExternalId::from("rt-registered");
    registry
        .register(InternalBindingRecord {
            room_id: id::<RoomId>(5),
            binding_id: id::<BindingId>(3),
            incarnation: id::<IncarnationId>(4),
            agent: AgentType::Codex,
            external_id: external.clone(),
            reserved_root: root.path().to_path_buf(),
        })
        .expect("register ack");
    let window = registry.open_observer(id::<RoomId>(5)).expect("observer");
    window.close();
    assert!(registry.run_is_active(id::<RoomId>(5)));
    drop(discovery);
    drop(root_lease);
    drop(registry);

    let restarted =
        RoundtableSessionRegistry::temporary(store_dir.path()).expect("restarted registry");
    assert!(
        restarted.run_is_active(id::<RoomId>(5)),
        "closing the observer window must not stop the run"
    );
    let bound = restarted.registered(id::<RoomId>(5)).expect("binding");
    assert_eq!(bound.binding_id, id::<BindingId>(3));
    assert_eq!(bound.incarnation, id::<IncarnationId>(4));
    assert_eq!(bound.room_id, id::<RoomId>(5));
    assert_eq!(bound.external_id.as_str(), "rt-registered");
    assert_eq!(bound.reserved_root, root.path());
    assert!(restarted.is_roundtable(AgentType::Codex, &external, &outside));

    let db = fresh_in_memory_db().await;
    let internal_dir = tempfile::tempdir().expect("internal");
    let internal = InternalAgentSessionRegistry::empty(db.conn.clone(), internal_dir.path())
        .expect("internal registry");
    let (_, filter) = internal.shared_filter().await.expect("filter");
    let hidden = summary("rt-registered", &outside);
    let mut ordinary = summary("ordinary-visible", &outside);
    ordinary.started_at = ordinary
        .started_at
        .checked_add_signed(chrono::Duration::seconds(90))
        .expect("later");
    let visible = filter_internal_summaries(
        vec![
            (AgentType::Codex, hidden.clone()),
            (AgentType::Codex, ordinary.clone()),
        ],
        &filter,
    );
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].1.id, "ordinary-visible");
    assert!(reject_internal_detail(
        AgentType::Codex,
        "rt-registered",
        detail(hidden.clone()),
        &filter,
    )
    .is_err());

    let parser = RankedParser {
        rows: vec![hidden, ordinary.clone()],
    };
    let recovered = recover_stale_session_for_test(
        &parser,
        AgentType::Codex,
        ordinary.folder_path.as_deref(),
        DateTime::<Utc>::from_timestamp_millis(1_700_000_100_000).expect("time"),
        &filter,
    )
    .expect("legal session");
    assert_eq!(recovered.summary.id, "ordinary-visible");
}

#[tokio::test]
async fn baseline_database_ordinary_sessions_still_open() {
    assert!(!downgrade_is_silent_compatible());
    assert_eq!(
        legacy_purpose(InternalAgentSessionPurpose::Title),
        "title"
    );
    assert_eq!(
        legacy_purpose(InternalAgentSessionPurpose::Translate),
        "translate"
    );

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/work/ordinary-baseline").await;
    let created = conversation_service::create(
        &db.conn,
        folder_id,
        AgentType::Codex,
        Some("ordinary".to_string()),
        None,
    )
    .await
    .expect("create");
    let opened = conversation_service::get_by_id(&db.conn, created.id)
        .await
        .expect("open");
    assert_eq!(opened.id, created.id);
    assert_eq!(opened.title.as_deref(), Some("ordinary"));
    let listed = conversation_service::list_by_folder(
        &db.conn,
        folder_id,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("list");
    assert!(listed.iter().any(|row| row.id == created.id));

    let conversation_sql = codeg_lib::db::entities::conversation::Entity::find()
        .build(DatabaseBackend::Sqlite)
        .to_string();
    let internal_sql = internal_agent_session::Entity::find()
        .build(DatabaseBackend::Sqlite)
        .to_string();
    for sql in [&conversation_sql, &internal_sql] {
        let lower = sql.to_ascii_lowercase();
        assert!(!lower.contains("roundtable"), "{sql}");
        assert!(!lower.contains("rt_internal"), "{sql}");
    }

    let internal_columns = column_names(&db.conn, "internal_agent_sessions").await;
    assert_eq!(
        internal_columns,
        vec!["agent_type", "created_at", "external_id", "purpose"]
    );
    for name in column_names(&db.conn, "conversation").await {
        let lower = name.to_ascii_lowercase();
        assert!(!lower.contains("roundtable"), "{name}");
        assert!(!lower.starts_with("rt_"), "{name}");
    }
    let roundtable_tables = db
        .conn
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'rt_%'".to_owned(),
        ))
        .await
        .expect("master");
    // P09b registers the rt_ tables on the shared migrator. Ordinary session
    // queries above still do not mention them, and the purpose check below
    // still rejects a roundtable value.
    let roundtable_names = roundtable_tables
        .iter()
        .map(|row| row.try_get::<String>("", "name").expect("name"))
        .collect::<Vec<_>>();
    assert!(roundtable_names.iter().any(|name| name == "rt_internal_bindings"));
    assert!(roundtable_names.iter().all(|name| name.starts_with("rt_")));

    let definition = db
        .conn
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT sql FROM sqlite_master WHERE name = 'internal_agent_sessions'".to_owned(),
        ))
        .await
        .expect("query")
        .expect("table");
    let sql: String = definition.try_get("", "sql").expect("sql");
    assert!(sql.contains("'title'"));
    assert!(sql.contains("'translate'"));
    assert!(!sql.contains("roundtable"));
    let rejected = db
        .conn
        .execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "INSERT INTO internal_agent_sessions (agent_type, external_id, purpose, created_at) \
             VALUES ('codex', 'rt-row', 'roundtable', '2020-01-01T00:00:00Z')"
                .to_owned(),
        ))
        .await;
    assert!(rejected.is_err(), "purpose check must reject roundtable");

    let store_dir = tempfile::tempdir().expect("store");
    let root = tempfile::tempdir().expect("root");
    let registry = RoundtableSessionRegistry::temporary(store_dir.path()).expect("registry");
    let root_lease = registry.reserve_root(root.path().to_path_buf());
    registry
        .register(InternalBindingRecord {
            room_id: id::<RoomId>(8),
            binding_id: id::<BindingId>(8),
            incarnation: id::<IncarnationId>(8),
            agent: AgentType::Codex,
            external_id: ExternalId::from("rt-not-in-old-table"),
            reserved_root: root.path().to_path_buf(),
        })
        .expect("register outside the old table");
    drop(root_lease);
    let rows = internal_agent_session::Entity::find()
        .all(&db.conn)
        .await
        .expect("rows");
    assert!(rows.is_empty());
    let still_open = conversation_service::get_by_id(&db.conn, created.id)
        .await
        .expect("still open");
    assert_eq!(still_open.id, created.id);
}

async fn column_names(conn: &sea_orm::DatabaseConnection, table: &str) -> Vec<String> {
    let rows = conn
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!("PRAGMA table_info({table})"),
        ))
        .await
        .expect("pragma");
    let mut names = rows
        .iter()
        .map(|row| row.try_get::<String>("", "name").expect("name"))
        .collect::<Vec<_>>();
    names.sort();
    names
}
