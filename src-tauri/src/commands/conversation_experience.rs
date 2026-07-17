//! Conversation-experience settings persistence (automatic titles + reference search).
//!
//! Persisted cores and the mutation gate live here. Task 9 wrappers hold the
//! gate through cancel_all + event emission so an older Off cannot race a newer On.

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::NotSet, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, Set,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::auto_title::AutoTitleCoordinator;
use crate::commands::acp::acp_get_agent_status_core;
use crate::db::entities::app_metadata;
use crate::db::entities::auto_title_job;
use crate::db::error::DbError;
use crate::db::service::app_metadata_service;
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::web::event_bridge::{emit_event, EventEmitter};

pub const KEY_AUTO_TITLE_AGENT: &str = "conversation_experience.auto_title_agent";
pub const KEY_REFERENCE_SEARCH_LIMIT: &str = "conversation_experience.reference_search_limit";
pub const KEY_SETTINGS_REVISION: &str = "conversation_experience.revision";
pub const DEFAULT_REFERENCE_SEARCH_LIMIT: u16 = 50;
pub const MIN_REFERENCE_SEARCH_LIMIT: u16 = 10;
pub const MAX_REFERENCE_SEARCH_LIMIT: u16 = 500;
pub const CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT: &str =
    "conversation-experience-settings://changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationExperienceSettings {
    pub auto_title_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}

#[derive(Default)]
pub struct ConversationExperienceMutationGate {
    inner: tokio::sync::Mutex<()>,
}

impl ConversationExperienceMutationGate {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.lock().await
    }
}

fn clamp_reference_search_limit(limit: u16) -> u16 {
    limit.clamp(MIN_REFERENCE_SEARCH_LIMIT, MAX_REFERENCE_SEARCH_LIMIT)
}

fn parse_revision(raw: Option<&str>) -> u64 {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return 0;
    };
    if raw.chars().all(|c| c.is_ascii_digit()) {
        raw.parse::<u64>().unwrap_or(0)
    } else {
        0
    }
}

fn parse_reference_search_limit(raw: Option<&str>) -> u16 {
    let Some(raw) = raw else {
        return DEFAULT_REFERENCE_SEARCH_LIMIT;
    };
    match raw.parse::<u16>() {
        Ok(value) => clamp_reference_search_limit(value),
        Err(_) => DEFAULT_REFERENCE_SEARCH_LIMIT,
    }
}

/// Load the automatic-title agent from `app_metadata`. Missing, empty (Off),
/// invalid JSON, and unknown enum values all resolve to `None`. Corrupt
/// non-empty values log a warning. Returns `DbError` for genuine database failures.
pub async fn load_auto_title_agent_from<C: ConnectionTrait>(
    conn: &C,
) -> Result<Option<AgentType>, DbError> {
    let Some(raw) = app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_AGENT).await? else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<AgentType>(&raw) {
        Ok(agent) => Ok(Some(agent)),
        Err(error) => {
            tracing::warn!(
                key = KEY_AUTO_TITLE_AGENT,
                value = %raw,
                error = %error,
                "corrupt automatic title agent setting; treating as Off"
            );
            Ok(None)
        }
    }
}

/// Load the full conversation-experience settings document. Generic over
/// connection so enrollment, claims, and write transactions can call it with
/// either `&DatabaseConnection` or `&DatabaseTransaction` and propagate `DbError`.
pub async fn load_settings_from<C: ConnectionTrait>(
    conn: &C,
) -> Result<ConversationExperienceSettings, DbError> {
    let auto_title_agent = load_auto_title_agent_from(conn).await?;
    let reference_raw =
        app_metadata_service::get_value_conn(conn, KEY_REFERENCE_SEARCH_LIMIT).await?;
    let revision_raw = app_metadata_service::get_value_conn(conn, KEY_SETTINGS_REVISION).await?;

    Ok(ConversationExperienceSettings {
        auto_title_agent,
        reference_search_limit: parse_reference_search_limit(reference_raw.as_deref()),
        revision: parse_revision(revision_raw.as_deref()),
    })
}

pub async fn get_conversation_experience_settings_core(
    conn: &DatabaseConnection,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    load_settings_from(conn)
        .await
        .map_err(AppCommandError::from)
}

enum SettingsFieldMutation {
    AutoTitleAgent(Option<AgentType>),
    ReferenceSearchLimit(u16),
}

async fn apply_field_mutation(
    txn: &sea_orm::DatabaseTransaction,
    mutation: SettingsFieldMutation,
) -> Result<(), AppCommandError> {
    match mutation {
        SettingsFieldMutation::AutoTitleAgent(agent) => {
            if agent.is_none() {
                auto_title_job::Entity::delete_many()
                    .exec(txn)
                    .await
                    .map_err(|error| AppCommandError::from(DbError::from(error)))?;
            }

            let stored_agent = agent
                .map(|value| serde_json::to_string(&value))
                .transpose()
                .map_err(|error| {
                    AppCommandError::new(
                        AppErrorCode::DatabaseError,
                        "Failed to serialize automatic title agent",
                    )
                    .with_detail(error.to_string())
                })?
                .unwrap_or_default();

            app_metadata_service::upsert_value(txn, KEY_AUTO_TITLE_AGENT, &stored_agent)
                .await
                .map_err(AppCommandError::from)?;
        }
        SettingsFieldMutation::ReferenceSearchLimit(limit) => {
            let stored = clamp_reference_search_limit(limit).to_string();
            app_metadata_service::upsert_value(txn, KEY_REFERENCE_SEARCH_LIMIT, &stored)
                .await
                .map_err(AppCommandError::from)?;
        }
    }
    Ok(())
}

/// Write-first revision advance + single field mutation inside one transaction.
///
/// 1. Insert the revision row at `0` with `ON CONFLICT(key) DO NOTHING`.
/// 2. Unconditionally advance revision with the signed-64-bit-safe CASE update.
/// 3. Write only the target field.
/// 4. Read the full document from the same transaction and commit.
async fn write_settings_field(
    conn: &DatabaseConnection,
    mutation: SettingsFieldMutation,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let txn = conn
        .begin()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    let now = Utc::now();
    app_metadata::Entity::insert(app_metadata::ActiveModel {
        id: NotSet,
        key: Set(KEY_SETTINGS_REVISION.to_string()),
        value: Set("0".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: NotSet,
    })
    .on_conflict(
        OnConflict::column(app_metadata::Column::Key)
            .do_nothing()
            .to_owned(),
    )
    .do_nothing()
    .exec(&txn)
    .await
    .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    let updated_at = now.to_rfc3339();
    let result = txn
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
UPDATE app_metadata
SET value = CASE
        WHEN value <> ''
          AND value NOT GLOB '*[^0-9]*'
          AND length(value) <= 19
          AND CAST(value AS INTEGER) BETWEEN 0 AND 9223372036854775806
        THEN CAST(CAST(value AS INTEGER) + 1 AS TEXT)
        ELSE '1'
    END,
    updated_at = ?,
    deleted_at = NULL
WHERE key = ?
  AND value <> '9223372036854775807'
"#,
            [updated_at.into(), KEY_SETTINGS_REVISION.into()],
        ))
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    if result.rows_affected() != 1 {
        return Err(AppCommandError::new(
            AppErrorCode::DatabaseError,
            "Conversation experience settings revision exhausted",
        ));
    }

    apply_field_mutation(&txn, mutation).await?;

    let saved = load_settings_from(&txn)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;
    Ok(saved)
}

pub async fn set_auto_title_agent_persisted_core(
    db: &AppDatabase,
    agent: Option<AgentType>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    if let Some(agent_type) = agent {
        let status = acp_get_agent_status_core(agent_type, db)
            .await
            .map_err(|error| {
                AppCommandError::new(
                    AppErrorCode::ConfigurationInvalid,
                    "Automatic title agent is unavailable",
                )
                .with_detail(error.to_string())
            })?;
        if !status.enabled || !status.available {
            return Err(AppCommandError::new(
                AppErrorCode::ConfigurationInvalid,
                "Automatic title agent is unavailable",
            ));
        }
    }

    write_settings_field(
        &db.conn,
        SettingsFieldMutation::AutoTitleAgent(agent),
    )
    .await
}

pub async fn set_reference_search_limit_persisted_core(
    conn: &DatabaseConnection,
    limit: u16,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    write_settings_field(
        conn,
        SettingsFieldMutation::ReferenceSearchLimit(limit),
    )
    .await
}

/// Settings setter wrapper: holds the shared mutation gate through the
/// committed cancellation decision and settings event so a delayed older Off
/// cannot cancel work enrolled after a newer On.
pub async fn set_auto_title_agent_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    coordinator: &AutoTitleCoordinator,
    mutation_gate: &ConversationExperienceMutationGate,
    agent: Option<AgentType>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let _mutation_guard = mutation_gate.lock().await;
    let saved = set_auto_title_agent_persisted_core(db, agent).await?;
    if saved.auto_title_agent.is_none() {
        coordinator.cancel_all().await;
    }
    emit_event(
        emitter,
        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
        saved.clone(),
    );
    Ok(saved)
}

// -------- Tauri commands -----------------------------------------------------

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_conversation_experience_settings(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        get_conversation_experience_settings_core(&db.conn).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_auto_title_agent(
    agent: Option<AgentType>,
    #[cfg(feature = "tauri-runtime")] app: tauri::AppHandle,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")]
    coordinator: tauri::State<'_, std::sync::Arc<AutoTitleCoordinator>>,
    #[cfg(feature = "tauri-runtime")]
    mutation_gate: tauri::State<'_, std::sync::Arc<ConversationExperienceMutationGate>>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let emitter = EventEmitter::Tauri(app);
        set_auto_title_agent_core(&db, &emitter, &coordinator, &mutation_gate, agent).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = agent;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    use crate::app_error::AppErrorCode;
    use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
    use crate::db::service::app_metadata_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;

    #[tokio::test]
    async fn independent_setters_preserve_the_other_field_and_advance_revision() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;

        let first = set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
            .await
            .expect("title agent");
        let second = set_reference_search_limit_persisted_core(&db.conn, 73)
            .await
            .expect("search limit");

        assert_eq!(first.revision, 1);
        assert_eq!(second.revision, 2);
        assert_eq!(second.auto_title_agent, Some(AgentType::ClaudeCode));
        assert_eq!(second.reference_search_limit, 73);
    }

    #[tokio::test]
    async fn title_agent_must_be_enabled_and_available() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        crate::commands::acp::acp_list_agents_core(&db)
            .await
            .expect("seed agent settings");
        crate::db::service::agent_setting_service::update(
            &db.conn,
            AgentType::ClaudeCode,
            crate::db::service::agent_setting_service::AgentSettingsUpdate {
                enabled: false,
                env_json: None,
                model_provider_id: None,
            },
        )
        .await
        .expect("disable agent");
        let error = set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
            .await
            .expect_err("disabled agent");
        assert!(matches!(error.code, AppErrorCode::ConfigurationInvalid));
    }

    #[tokio::test]
    async fn concurrent_independent_setters_serialize_revision_without_losing_either_field() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db = crate::db::init_database(temp.path(), "settings-concurrency-test")
            .await
            .expect("open pooled WAL database");

        let (agent_result, limit_result) = tokio::join!(
            set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode)),
            set_reference_search_limit_persisted_core(&db.conn, 73),
        );

        let agent_settings = agent_result.expect("agent write");
        let limit_settings = limit_result.expect("limit write");
        let mut revisions = [agent_settings.revision, limit_settings.revision];
        revisions.sort_unstable();
        assert_eq!(revisions, [1, 2]);

        let loaded = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("load document");
        assert_eq!(loaded.auto_title_agent, Some(AgentType::ClaudeCode));
        assert_eq!(loaded.reference_search_limit, 73);
        assert_eq!(loaded.revision, 2);

        // Keep TempDir alive through every assertion.
        drop(temp);
    }

    #[tokio::test]
    async fn defaults_are_off_agent_limit_50_revision_0() {
        let db = fresh_in_memory_db().await;
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("defaults");
        assert_eq!(
            settings,
            ConversationExperienceSettings {
                auto_title_agent: None,
                reference_search_limit: DEFAULT_REFERENCE_SEARCH_LIMIT,
                revision: 0,
            }
        );
    }

    #[tokio::test]
    async fn corrupt_agent_and_limit_values_resolve_to_safe_defaults() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, "not-a-valid-agent")
            .await
            .expect("corrupt agent");
        app_metadata_service::upsert_value(&db.conn, KEY_REFERENCE_SEARCH_LIMIT, "nope")
            .await
            .expect("corrupt limit");
        app_metadata_service::upsert_value(&db.conn, KEY_SETTINGS_REVISION, "xyz")
            .await
            .expect("corrupt revision");

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("load corrupt");
        assert_eq!(settings.auto_title_agent, None);
        assert_eq!(
            settings.reference_search_limit,
            DEFAULT_REFERENCE_SEARCH_LIMIT
        );
        assert_eq!(settings.revision, 0);
    }

    #[tokio::test]
    async fn reference_limit_clamps_on_write_and_read() {
        let db = fresh_in_memory_db().await;

        let low = set_reference_search_limit_persisted_core(&db.conn, 1)
            .await
            .expect("clamp low write");
        assert_eq!(low.reference_search_limit, MIN_REFERENCE_SEARCH_LIMIT);

        let high = set_reference_search_limit_persisted_core(&db.conn, 9_999)
            .await
            .expect("clamp high write");
        assert_eq!(high.reference_search_limit, MAX_REFERENCE_SEARCH_LIMIT);
        assert_eq!(high.revision, 2);

        app_metadata_service::upsert_value(&db.conn, KEY_REFERENCE_SEARCH_LIMIT, "5")
            .await
            .expect("store below min");
        let read_low = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("read low");
        assert_eq!(
            read_low.reference_search_limit,
            MIN_REFERENCE_SEARCH_LIMIT
        );

        app_metadata_service::upsert_value(&db.conn, KEY_REFERENCE_SEARCH_LIMIT, "900")
            .await
            .expect("store above max");
        let read_high = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("read high");
        assert_eq!(
            read_high.reference_search_limit,
            MAX_REFERENCE_SEARCH_LIMIT
        );
    }

    #[tokio::test]
    async fn corrupt_revision_resets_to_one_on_next_write() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_SETTINGS_REVISION, "not-a-number")
            .await
            .expect("corrupt revision");

        let settings = set_reference_search_limit_persisted_core(&db.conn, 42)
            .await
            .expect("write after corrupt revision");
        assert_eq!(settings.revision, 1);
        assert_eq!(settings.reference_search_limit, 42);
    }

    #[tokio::test]
    async fn revision_overflow_returns_database_error() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_SETTINGS_REVISION,
            "9223372036854775807",
        )
        .await
        .expect("max signed revision");

        let error = set_reference_search_limit_persisted_core(&db.conn, 50)
            .await
            .expect_err("revision exhausted");
        assert!(matches!(error.code, AppErrorCode::DatabaseError));
    }

    #[tokio::test]
    async fn turning_title_agent_off_deletes_pending_jobs_atomically() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/auto-title-off").await;
        let awaiting_id = seed_conversation(&db, folder_id, AgentType::ClaudeCode).await;
        let running_id = seed_conversation(&db, folder_id, AgentType::Codex).await;

        let now = Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(awaiting_id),
            state: Set(AutoTitleJobState::AwaitingTurn),
            attempts: Set(0),
            first_user_text: Set(None),
            first_assistant_text: Set(None),
            locale: Set(None),
            usable_turn_seq: Set(0),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(None),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("awaiting job");
        auto_title_job::ActiveModel {
            conversation_id: Set(running_id),
            state: Set(AutoTitleJobState::Running),
            attempts: Set(1),
            first_user_text: Set(Some("hello".into())),
            first_assistant_text: Set(Some("world".into())),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(1),
            attempt_turn_seq: Set(1),
            last_usable_turn_token: Set(Some("tok".into())),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("running job");

        set_auto_title_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
            .await
            .expect("enable title agent");
        assert_eq!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("count before off")
                .len(),
            2
        );

        let off = set_auto_title_agent_persisted_core(&db, None)
            .await
            .expect("turn off");
        assert_eq!(off.auto_title_agent, None);
        assert_eq!(off.revision, 2);
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("count after off")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn empty_agent_value_is_off_sentinel() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, "")
            .await
            .expect("empty off sentinel");
        let agent = load_auto_title_agent_from(&db.conn)
            .await
            .expect("load empty");
        assert_eq!(agent, None);
    }
}
