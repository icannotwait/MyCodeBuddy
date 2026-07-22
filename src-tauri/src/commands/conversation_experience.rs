//! Conversation-experience settings persistence (automatic titles + reference search).
//!
//! Persisted cores and the mutation gate live here. Task 9 wrappers hold the
//! gate through cancel_all + event emission so an older Off cannot race a newer On.
//!
//! Title config uses fail-closed barrier/gen/fp write sequence (design r8).

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveValue::NotSet, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, Set,
    Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::auto_title::title_key::{
    delete_title_api_key, get_title_api_key, set_title_api_key, title_key_fingerprint, TitleKeyState,
};
use crate::auto_title::title_settings::{
    normalize_and_validate_api_url, parse_config_barrier, parse_config_gen, ApiKeyUpdate,
    BARRIER_RAISED, KEY_AUTO_TITLE_API_KEY_FP, KEY_AUTO_TITLE_API_URL, KEY_AUTO_TITLE_CONFIG_BARRIER,
    KEY_AUTO_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_MODEL, KEY_DOCUMENT_TRANSLATE_AGENT,
};
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

// Re-export metadata keys used by later tasks / callers.
pub use crate::auto_title::title_settings::{
    KEY_AUTO_TITLE_API_KEY_FP as KEY_TITLE_API_KEY_FP,
    KEY_AUTO_TITLE_API_URL as KEY_TITLE_API_URL, KEY_AUTO_TITLE_CONFIG_BARRIER as KEY_TITLE_BARRIER,
    KEY_AUTO_TITLE_CONFIG_GEN as KEY_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_MODEL as KEY_TITLE_MODEL,
    KEY_DOCUMENT_TRANSLATE_AGENT as KEY_DOC_TRANSLATE_AGENT,
};

/// GET / event payload for conversation-experience settings (no API key secret).
///
/// Legacy `auto_title_agent` is no longer on the wire after Task 5 FE cutover.
/// Document translate still may fall back to the legacy metadata key via
/// [`load_document_translate_agent_from`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationExperienceSettings {
    pub auto_title_api_url: String,
    pub auto_title_api_key_set: bool,
    pub auto_title_model: String,
    pub auto_title_config_barrier: bool,
    pub document_translate_agent: Option<AgentType>,
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

fn config_error(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::ConfigurationInvalid, message)
}

fn db_error_msg(message: impl Into<String>) -> AppCommandError {
    AppCommandError::new(AppErrorCode::DatabaseError, message)
}

/// Load the automatic-title agent from `app_metadata`. Missing, empty (Off),
/// invalid JSON, and unknown enum values all resolve to `None`. Corrupt
/// non-empty values log a warning. Returns `DbError` for genuine database failures.
///
/// Still used by enroll/claim (until Task 4) and as legacy fallback for
/// document-translate when the new key is absent.
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

/// Load document-translate agent with absent-only legacy fallback.
///
/// 1. New key **absent** → fall back to legacy `auto_title_agent`.
/// 2. New key **present** empty → explicit Off (no fallback).
/// 3. New key present non-empty → parse; corrupt ⇒ warn + Off (no fallback).
pub async fn load_document_translate_agent_from<C: ConnectionTrait>(
    conn: &C,
) -> Result<Option<AgentType>, DbError> {
    match app_metadata_service::get_value_conn(conn, KEY_DOCUMENT_TRANSLATE_AGENT).await? {
        None => load_auto_title_agent_from(conn).await,
        Some(raw) if raw.is_empty() => Ok(None),
        Some(raw) => match serde_json::from_str::<AgentType>(&raw) {
            Ok(agent) => Ok(Some(agent)),
            Err(error) => {
                tracing::warn!(
                    key = KEY_DOCUMENT_TRANSLATE_AGENT,
                    value = %raw,
                    error = %error,
                    "corrupt document translate agent setting; treating as Off"
                );
                Ok(None)
            }
        },
    }
}

/// Load the full conversation-experience settings document. Generic over
/// connection so enrollment, claims, and write transactions can call it with
/// either `&DatabaseConnection` or `&DatabaseTransaction` and propagate `DbError`.
pub async fn load_settings_from<C: ConnectionTrait>(
    conn: &C,
) -> Result<ConversationExperienceSettings, DbError> {
    let api_url = app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_API_URL)
        .await?
        .unwrap_or_default();
    let model = app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_MODEL)
        .await?
        .unwrap_or_default();
    let barrier_raw =
        app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_CONFIG_BARRIER).await?;
    let document_translate_agent = load_document_translate_agent_from(conn).await?;
    let reference_raw =
        app_metadata_service::get_value_conn(conn, KEY_REFERENCE_SEARCH_LIMIT).await?;
    let revision_raw = app_metadata_service::get_value_conn(conn, KEY_SETTINGS_REVISION).await?;

    let key_set = matches!(get_title_api_key(), TitleKeyState::Present(_));

    Ok(ConversationExperienceSettings {
        auto_title_api_url: api_url,
        auto_title_api_key_set: key_set,
        auto_title_model: model,
        auto_title_config_barrier: parse_config_barrier(barrier_raw.as_deref()),
        document_translate_agent,
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
    /// Document translate agent; Off stores present-empty. Does not wipe title jobs.
    DocumentTranslateAgent(Option<AgentType>),
    ReferenceSearchLimit(u16),
}

async fn apply_field_mutation(
    txn: &sea_orm::DatabaseTransaction,
    mutation: SettingsFieldMutation,
) -> Result<(), AppCommandError> {
    match mutation {
        SettingsFieldMutation::DocumentTranslateAgent(agent) => {
            let stored_agent = agent
                .map(|value| serde_json::to_string(&value))
                .transpose()
                .map_err(|error| {
                    AppCommandError::new(
                        AppErrorCode::DatabaseError,
                        "Failed to serialize document translate agent",
                    )
                    .with_detail(error.to_string())
                })?
                .unwrap_or_default();

            app_metadata_service::upsert_value(txn, KEY_DOCUMENT_TRANSLATE_AGENT, &stored_agent)
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

/// Ensure revision row exists then unconditionally advance it (signed-64-safe CASE).
async fn advance_revision_in_txn(
    txn: &sea_orm::DatabaseTransaction,
) -> Result<(), AppCommandError> {
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
    .exec(txn)
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
    Ok(())
}

/// Write-first revision advance + single field mutation inside one transaction.
async fn write_settings_field(
    conn: &DatabaseConnection,
    mutation: SettingsFieldMutation,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let txn = conn
        .begin()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    advance_revision_in_txn(&txn).await?;
    apply_field_mutation(&txn, mutation).await?;

    let saved = load_settings_from(&txn)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;
    Ok(saved)
}

/// Read config gen, bump by 1, write back. Call inside an open transaction.
async fn bump_config_gen_in_txn(txn: &sea_orm::DatabaseTransaction) -> Result<u64, AppCommandError> {
    let raw = app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_CONFIG_GEN)
        .await
        .map_err(AppCommandError::from)?;
    let current = parse_config_gen(raw.as_deref());
    let next = current
        .checked_add(1)
        .ok_or_else(|| db_error_msg("Automatic title config generation exhausted"))?;
    app_metadata_service::upsert_value(txn, KEY_AUTO_TITLE_CONFIG_GEN, &next.to_string())
        .await
        .map_err(AppCommandError::from)?;
    Ok(next)
}

/// Test hooks for ambiguous-commit fail-closed coverage (Err after durable persist).
#[cfg(any(test, feature = "test-utils"))]
mod barrier_commit_hooks {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    static FAIL_NEXT_RAISE_AS_AMBIGUOUS: AtomicBool = AtomicBool::new(false);
    static FAIL_NEXT_SUCCESS_AS_AMBIGUOUS: AtomicBool = AtomicBool::new(false);
    /// `-1` disabled; `0` fail this raise cleanly; `n>0` skip n raises then fail.
    static RAISE_CLEAN_FAIL_SKIPS: AtomicIsize = AtomicIsize::new(-1);

    /// Armed from unit tests (server-mode filters); keep available under test-utils.
    #[allow(dead_code)]
    pub fn reset() {
        FAIL_NEXT_RAISE_AS_AMBIGUOUS.store(false, Ordering::SeqCst);
        FAIL_NEXT_SUCCESS_AS_AMBIGUOUS.store(false, Ordering::SeqCst);
        RAISE_CLEAN_FAIL_SKIPS.store(-1, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn fail_next_raise_as_ambiguous() {
        FAIL_NEXT_RAISE_AS_AMBIGUOUS.store(true, Ordering::SeqCst);
    }

    #[allow(dead_code)]
    pub fn fail_next_success_as_ambiguous() {
        FAIL_NEXT_SUCCESS_AS_AMBIGUOUS.store(true, Ordering::SeqCst);
    }

    /// After `skips` successful raises, the next `raise_barrier_wipe_jobs` fails
    /// *before* any durable write (barrier stays clear). Used to cover compensating
    /// raise failure after an ambiguous success commit.
    #[allow(dead_code)]
    pub fn fail_raise_clean_after_skips(skips: usize) {
        RAISE_CLEAN_FAIL_SKIPS.store(skips as isize, Ordering::SeqCst);
    }

    pub(super) fn take_fail_raise_as_ambiguous() -> bool {
        FAIL_NEXT_RAISE_AS_AMBIGUOUS.swap(false, Ordering::SeqCst)
    }

    pub(super) fn take_fail_success_as_ambiguous() -> bool {
        FAIL_NEXT_SUCCESS_AS_AMBIGUOUS.swap(false, Ordering::SeqCst)
    }

    /// Returns true when this raise should fail cleanly (no barrier write).
    pub(super) fn take_fail_raise_clean() -> bool {
        let n = RAISE_CLEAN_FAIL_SKIPS.load(Ordering::SeqCst);
        if n < 0 {
            return false;
        }
        if n == 0 {
            RAISE_CLEAN_FAIL_SKIPS.store(-1, Ordering::SeqCst);
            return true;
        }
        RAISE_CLEAN_FAIL_SKIPS.store(n - 1, Ordering::SeqCst);
        false
    }
}

/// Raise barrier, bump gen, wipe all title jobs, advance revision (one txn).
///
/// On commit `Err`, re-read is the caller's responsibility: if the barrier was
/// still persisted (ambiguous Err-after-persist), callers must `cancel_all`.
async fn raise_barrier_wipe_jobs(
    conn: &DatabaseConnection,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    // Simulate clean raise failure before any durable write (test-only).
    #[cfg(any(test, feature = "test-utils"))]
    if barrier_commit_hooks::take_fail_raise_clean() {
        return Err(db_error_msg(
            "injected clean barrier raise failure (not persisted)",
        ));
    }

    let txn = conn
        .begin()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_CONFIG_BARRIER, BARRIER_RAISED)
        .await
        .map_err(AppCommandError::from)?;
    bump_config_gen_in_txn(&txn).await?;
    auto_title_job::Entity::delete_many()
        .exec(&txn)
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;
    advance_revision_in_txn(&txn).await?;

    let saved = load_settings_from(&txn)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    // Simulate commit reporting Err after durable barrier/gen/wipe persist.
    #[cfg(any(test, feature = "test-utils"))]
    if barrier_commit_hooks::take_fail_raise_as_ambiguous() {
        return Err(db_error_msg(
            "injected ambiguous barrier raise commit (persisted)",
        ));
    }

    Ok(saved)
}

/// Atomic success: write url/model/fp, clear barrier, bump gen + revision.
async fn commit_verified_title_config(
    conn: &DatabaseConnection,
    next_url: &str,
    next_model: &str,
    expected_fp: &str,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let txn = conn
        .begin()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_API_URL, next_url)
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_MODEL, next_model)
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_API_KEY_FP, expected_fp)
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
        .await
        .map_err(AppCommandError::from)?;
    bump_config_gen_in_txn(&txn).await?;
    advance_revision_in_txn(&txn).await?;

    let saved = load_settings_from(&txn)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    // Simulate commit reporting Err after durable url/model/fp/clear-barrier.
    #[cfg(any(test, feature = "test-utils"))]
    if barrier_commit_hooks::take_fail_success_as_ambiguous() {
        return Err(db_error_msg(
            "injected ambiguous success commit (persisted)",
        ));
    }

    Ok(saved)
}

/// After a barrier-related commit reports Err: re-read durable state and
/// `cancel_all` when the barrier is set or re-read fails (fail-closed).
///
/// Returns the durable snapshot when re-read succeeds (for emit).
async fn cancel_after_ambiguous_barrier_commit(
    conn: &DatabaseConnection,
    coordinator: &AutoTitleCoordinator,
) -> Option<ConversationExperienceSettings> {
    match load_settings_from(conn).await {
        Ok(snapshot) if snapshot.auto_title_config_barrier => {
            coordinator.cancel_all().await;
            Some(snapshot)
        }
        Ok(snapshot) => {
            // Barrier clear — clean rollback of raise, or success cleared it.
            Some(snapshot)
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "failed to re-read settings after ambiguous barrier commit; cancel_all fail-closed"
            );
            coordinator.cancel_all().await;
            None
        }
    }
}

/// Raise barrier + wipe + gen, then `cancel_all` on success. On commit Err,
/// re-read durable barrier; if set (or re-read fails), still `cancel_all`.
async fn raise_barrier_wipe_jobs_and_cancel(
    conn: &DatabaseConnection,
    coordinator: &AutoTitleCoordinator,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    match raise_barrier_wipe_jobs(conn).await {
        Ok(saved) => {
            coordinator.cancel_all().await;
            Ok(saved)
        }
        Err(error) => {
            let _ = cancel_after_ambiguous_barrier_commit(conn, coordinator).await;
            Err(error)
        }
    }
}

/// Best-effort force `auto_title_enabled` false when the barrier cannot be raised.
///
/// Tries keyring delete (logs and continues on failure). **Always** writes a
/// durable Off condition in DB independent of keyring outcome: raise barrier,
/// clear url/model/fp, wipe jobs, bump gen/revision. Any one of barrier or
/// empty url/model breaks the enabled predicate even if the secret remains.
async fn force_title_config_off_best_effort(
    conn: &DatabaseConnection,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    if let Err(error) = delete_title_api_key() {
        tracing::error!(
            error = %error,
            "failed to delete title API key while forcing title config Off"
        );
    }

    let txn = conn
        .begin()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;

    // Durable Off independent of keyring: barrier + empty triple metadata.
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_CONFIG_BARRIER, BARRIER_RAISED)
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_API_URL, "")
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_MODEL, "")
        .await
        .map_err(AppCommandError::from)?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_API_KEY_FP, "")
        .await
        .map_err(AppCommandError::from)?;
    auto_title_job::Entity::delete_many()
        .exec(&txn)
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;
    bump_config_gen_in_txn(&txn).await?;
    advance_revision_in_txn(&txn).await?;

    let saved = load_settings_from(&txn)
        .await
        .map_err(AppCommandError::from)?;
    txn.commit()
        .await
        .map_err(|error| AppCommandError::from(DbError::from(error)))?;
    Ok(saved)
}

/// After a failed compensating raise: if barrier still clear (or unreadable),
/// force Off via durable barrier + clear url/model/fp + wipe + keyring delete;
/// always cancel live work.
async fn force_off_after_failed_raise(
    conn: &DatabaseConnection,
    coordinator: &AutoTitleCoordinator,
    emitter: &EventEmitter,
) {
    match load_settings_from(conn).await {
        Ok(snap) if snap.auto_title_config_barrier => {
            // Ambiguous raise actually persisted — barrier alone forces Off.
            coordinator.cancel_all().await;
            emit_event(
                emitter,
                CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                snap,
            );
        }
        Ok(_) | Err(_) => {
            // Barrier still clear (or re-read failed): break the enabled triple.
            match force_title_config_off_best_effort(conn).await {
                Ok(saved) => {
                    coordinator.cancel_all().await;
                    emit_event(
                        emitter,
                        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                        saved,
                    );
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "failed to force title config Off after compensating raise failure; cancel_all fail-closed"
                    );
                    coordinator.cancel_all().await;
                }
            }
        }
    }
}

/// Success-transaction failure path (design r8 fail-closed):
/// always `cancel_all` first; prefer re-raising the barrier; if raise fails and
/// barrier is still clear, force Off (barrier + clear url/model/fp + wipe +
/// best-effort keyring delete) so enabled is false even when key delete fails.
async fn recover_ambiguous_success_commit(
    conn: &DatabaseConnection,
    coordinator: &AutoTitleCoordinator,
    emitter: &EventEmitter,
) {
    // Live work must stop even if durable recovery fails mid-way.
    coordinator.cancel_all().await;

    match load_settings_from(conn).await {
        Ok(snapshot) if snapshot.auto_title_config_barrier => {
            // Barrier still raised — jobs wiped with it; already cancelled.
            emit_event(
                emitter,
                CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                snapshot,
            );
        }
        Ok(_snapshot) => {
            // Barrier clear after ambiguous success: url/model/fp may have
            // persisted with barrier cleared. Prefer re-raise; else force Off.
            match raise_barrier_wipe_jobs(conn).await {
                Ok(saved) => {
                    coordinator.cancel_all().await;
                    emit_event(
                        emitter,
                        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                        saved,
                    );
                }
                Err(error) => {
                    tracing::error!(
                        error = %error,
                        "compensating barrier raise failed after ambiguous success; force Off"
                    );
                    force_off_after_failed_raise(conn, coordinator, emitter).await;
                }
            }
        }
        Err(error) => {
            tracing::error!(
                error = %error,
                "failed to re-read settings after ambiguous success commit; re-raise fail-closed"
            );
            match raise_barrier_wipe_jobs(conn).await {
                Ok(saved) => {
                    coordinator.cancel_all().await;
                    emit_event(
                        emitter,
                        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                        saved,
                    );
                }
                Err(raise_error) => {
                    tracing::error!(
                        error = %raise_error,
                        "compensating barrier raise failed after re-read error; force Off"
                    );
                    force_off_after_failed_raise(conn, coordinator, emitter).await;
                }
            }
        }
    }
}

fn verify_keyring_identity(
    update: &ApiKeyUpdate,
    preflight: &TitleKeyState,
    verified: &TitleKeyState,
) -> Result<String, AppCommandError> {
    match (update, preflight, verified) {
        (ApiKeyUpdate::Set(expected), _, TitleKeyState::Present(s)) if s == expected => {
            Ok(title_key_fingerprint(s))
        }
        (ApiKeyUpdate::Clear, _, TitleKeyState::Absent) => Ok(String::new()),
        (ApiKeyUpdate::Keep, TitleKeyState::Present(old), TitleKeyState::Present(s))
            if s == old =>
        {
            Ok(title_key_fingerprint(s))
        }
        (ApiKeyUpdate::Keep, TitleKeyState::Absent, TitleKeyState::Absent) => Ok(String::new()),
        (_, _, TitleKeyState::Unavailable) => Err(config_error(
            "Automatic title API key store is unavailable",
        )),
        _ => Err(config_error(
            "Automatic title API key verification failed",
        )),
    }
}

fn compensate_keyring(preflight: &TitleKeyState) {
    match preflight {
        TitleKeyState::Present(old) => {
            if let Err(error) = set_title_api_key(old) {
                tracing::error!(
                    error = %error,
                    "failed to restore title API key after verify failure"
                );
            }
        }
        TitleKeyState::Absent => {
            if let Err(error) = delete_title_api_key() {
                tracing::error!(
                    error = %error,
                    "failed to clear title API key after verify failure"
                );
            }
        }
        TitleKeyState::Unavailable => {}
    }
}

/// Fail-closed title API config write (design r8).
pub async fn set_auto_title_api_config_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    coordinator: &AutoTitleCoordinator,
    mutation_gate: &ConversationExperienceMutationGate,
    api_url: String,
    api_key_update: ApiKeyUpdate,
    model: String,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let _mutation_guard = mutation_gate.lock().await;

    let next_url = if api_url.trim().is_empty() {
        String::new()
    } else {
        normalize_and_validate_api_url(&api_url)?
    };
    let next_model = model.trim().to_string();

    // Preflight: read key tri-state (must not map errors to Absent).
    let preflight = get_title_api_key();
    if matches!(preflight, TitleKeyState::Unavailable) {
        let saved = raise_barrier_wipe_jobs_and_cancel(&db.conn, coordinator).await?;
        emit_event(
            emitter,
            CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
            saved.clone(),
        );
        return Err(config_error(
            "Automatic title API key store is unavailable",
        ));
    }

    // Step 6: raise barrier + gen + wipe jobs, then cancel_all.
    // On ambiguous commit Err-after-persist, still cancel when barrier is set.
    let after_barrier = raise_barrier_wipe_jobs_and_cancel(&db.conn, coordinator).await?;
    emit_event(
        emitter,
        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
        after_barrier,
    );

    // Step 7: apply keyring action.
    match &api_key_update {
        ApiKeyUpdate::Keep => {}
        ApiKeyUpdate::Set(secret) => {
            if let Err(error) = set_title_api_key(secret) {
                let saved = load_settings_from(&db.conn)
                    .await
                    .map_err(AppCommandError::from)?;
                coordinator.cancel_all().await;
                emit_event(
                    emitter,
                    CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                    saved,
                );
                return Err(config_error("Failed to store automatic title API key")
                    .with_detail(error));
            }
        }
        ApiKeyUpdate::Clear => {
            if let Err(error) = delete_title_api_key() {
                let saved = load_settings_from(&db.conn)
                    .await
                    .map_err(AppCommandError::from)?;
                coordinator.cancel_all().await;
                emit_event(
                    emitter,
                    CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                    saved,
                );
                return Err(config_error("Failed to clear automatic title API key")
                    .with_detail(error));
            }
        }
    }

    // Step 8: verify keyring identity while barrier still set.
    let verified = get_title_api_key();
    let expected_fp = match verify_keyring_identity(&api_key_update, &preflight, &verified) {
        Ok(fp) => fp,
        Err(error) => {
            // url/model not yet written → compensate keyring to preflight.
            compensate_keyring(&preflight);
            let saved = load_settings_from(&db.conn)
                .await
                .map_err(AppCommandError::from)?;
            coordinator.cancel_all().await;
            emit_event(
                emitter,
                CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                saved,
            );
            return Err(error);
        }
    };

    // Step 9: atomic success transaction.
    let saved = match commit_verified_title_config(
        &db.conn,
        &next_url,
        &next_model,
        &expected_fp,
    )
    .await
    {
        Ok(s) => s,
        Err(error) => {
            // Ambiguous / failed commit: re-read durable barrier/url/model/fp;
            // prefer barrier set; always cancel_all when barrier set or jobs wiped.
            recover_ambiguous_success_commit(&db.conn, coordinator, emitter).await;
            return Err(error);
        }
    };

    // Step 10: post-commit re-verify (belt-and-suspenders).
    let live = get_title_api_key();
    let live_fp = match &live {
        TitleKeyState::Present(s) => title_key_fingerprint(s),
        TitleKeyState::Absent => String::new(),
        TitleKeyState::Unavailable => {
            let saved = raise_barrier_wipe_jobs_and_cancel(&db.conn, coordinator).await?;
            emit_event(
                emitter,
                CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
                saved,
            );
            return Err(config_error(
                "Automatic title API key store is unavailable after save",
            ));
        }
    };
    if live_fp != expected_fp {
        let saved = raise_barrier_wipe_jobs_and_cancel(&db.conn, coordinator).await?;
        emit_event(
            emitter,
            CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
            saved,
        );
        return Err(config_error(
            "Automatic title API key drifted after save",
        ));
    }

    // Step 11: success — cancel again if needed (barrier path already cancelled).
    coordinator.cancel_all().await;
    emit_event(
        emitter,
        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
        saved.clone(),
    );
    Ok(saved)
}

/// Persist title API config with an inert coordinator (unit tests only).
#[cfg(any(test, feature = "test-utils"))]
pub async fn set_auto_title_api_config_persisted_core(
    db: &AppDatabase,
    api_url: String,
    api_key_update: ApiKeyUpdate,
    model: String,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
    let gate = ConversationExperienceMutationGate::default();
    set_auto_title_api_config_core(
        db,
        &EventEmitter::Noop,
        &coordinator,
        &gate,
        api_url,
        api_key_update,
        model,
    )
    .await
}

pub async fn set_document_translate_agent_persisted_core(
    db: &AppDatabase,
    agent: Option<AgentType>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    if let Some(agent_type) = agent {
        let status = acp_get_agent_status_core(agent_type, db)
            .await
            .map_err(|error| {
                AppCommandError::new(
                    AppErrorCode::ConfigurationInvalid,
                    "Document translate agent is unavailable",
                )
                .with_detail(error.to_string())
            })?;
        if !status.enabled || !status.available {
            return Err(AppCommandError::new(
                AppErrorCode::ConfigurationInvalid,
                "Document translate agent is unavailable",
            ));
        }
    }

    write_settings_field(
        &db.conn,
        SettingsFieldMutation::DocumentTranslateAgent(agent),
    )
    .await
}

pub async fn set_reference_search_limit_persisted_core(
    conn: &DatabaseConnection,
    limit: u16,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    write_settings_field(conn, SettingsFieldMutation::ReferenceSearchLimit(limit)).await
}

/// Persist document-translate agent; does not touch title API fields or jobs.
pub async fn set_document_translate_agent_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    mutation_gate: &ConversationExperienceMutationGate,
    agent: Option<AgentType>,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let _mutation_guard = mutation_gate.lock().await;
    let saved = set_document_translate_agent_persisted_core(db, agent).await?;
    emit_event(
        emitter,
        CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
        saved.clone(),
    );
    Ok(saved)
}

/// Persist a new reference-search result limit, advance the registry limit
/// epoch (cancelling old-epoch jobs), and broadcast the full settings
/// snapshot. Holds the shared mutation gate through registry application and
/// event emission so an older delayed write cannot restore an obsolete cap.
pub async fn set_reference_search_limit_core(
    conn: &sea_orm::DatabaseConnection,
    emitter: &EventEmitter,
    registry: &crate::reference_search::ReferenceSearchRegistry,
    mutation_gate: &ConversationExperienceMutationGate,
    limit: u16,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    let _mutation_guard = mutation_gate.lock().await;
    let saved = set_reference_search_limit_persisted_core(conn, limit).await?;
    registry.set_limit(saved.reference_search_limit).await;
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

/// Tauri IPC arg for `api_key_update`: omit → Keep; JSON `null` → error.
///
/// Plain `Option<T>` cannot distinguish omit from null (both become `None`).
/// This type peeks the raw invoke payload so desktop matches Axum.
pub struct TauriApiKeyUpdateArg(pub ApiKeyUpdate);

#[cfg(feature = "tauri-runtime")]
impl<'de, R: tauri::Runtime> tauri::ipc::CommandArg<'de, R> for TauriApiKeyUpdateArg {
    fn from_command(
        command: tauri::ipc::CommandItem<'de, R>,
    ) -> Result<Self, tauri::ipc::InvokeError> {
        use serde::de::Error as _;
        use tauri::ipc::InvokeBody;

        let name = command.name;
        let key = command.key;
        match command.message.payload() {
            InvokeBody::Json(map) => match map.get(key) {
                None => Ok(Self(ApiKeyUpdate::Keep)),
                Some(value) if value.is_null() => Err(tauri::Error::InvalidArgs(
                    name,
                    key,
                    serde_json::Error::custom(
                        "api_key_update must not be null; omit the field to Keep",
                    ),
                )
                .into()),
                Some(value) => ApiKeyUpdate::deserialize(value)
                    .map(Self)
                    .map_err(|error| tauri::Error::InvalidArgs(name, key, error).into()),
            },
            InvokeBody::Raw(_) => Err(tauri::Error::InvalidArgs(
                name,
                key,
                serde_json::Error::custom(
                    "api_key_update requires a JSON invoke payload",
                ),
            )
            .into()),
        }
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_auto_title_api_config(
    api_url: String,
    #[allow(unused_variables)] api_key_update: TauriApiKeyUpdateArg,
    model: String,
    #[cfg(feature = "tauri-runtime")] app: tauri::AppHandle,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] coordinator: tauri::State<
        '_,
        std::sync::Arc<AutoTitleCoordinator>,
    >,
    #[cfg(feature = "tauri-runtime")] mutation_gate: tauri::State<
        '_,
        std::sync::Arc<ConversationExperienceMutationGate>,
    >,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let emitter = EventEmitter::Tauri(app);
        set_auto_title_api_config_core(
            &db,
            &emitter,
            &coordinator,
            &mutation_gate,
            api_url,
            api_key_update.0,
            model,
        )
        .await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = (api_url, model, api_key_update);
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_document_translate_agent(
    agent: Option<AgentType>,
    #[cfg(feature = "tauri-runtime")] app: tauri::AppHandle,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] mutation_gate: tauri::State<
        '_,
        std::sync::Arc<ConversationExperienceMutationGate>,
    >,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let emitter = EventEmitter::Tauri(app);
        set_document_translate_agent_core(&db, &emitter, &mutation_gate, agent).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = agent;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_reference_search_limit(
    limit: u16,
    #[cfg(feature = "tauri-runtime")] app: tauri::AppHandle,
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    #[cfg(feature = "tauri-runtime")] registry: tauri::State<
        '_,
        std::sync::Arc<crate::reference_search::ReferenceSearchRegistry>,
    >,
    #[cfg(feature = "tauri-runtime")] mutation_gate: tauri::State<
        '_,
        std::sync::Arc<ConversationExperienceMutationGate>,
    >,
) -> Result<ConversationExperienceSettings, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        let emitter = EventEmitter::Tauri(app);
        set_reference_search_limit_core(&db.conn, &emitter, &registry, &mutation_gate, limit).await
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = limit;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app_error::AppErrorCode;
    use crate::auto_title::title_key::{self, TitleKeyState};
    #[cfg(not(feature = "tauri-runtime"))]
    use crate::auto_title::title_key::title_key_fingerprint;
    #[cfg(not(feature = "tauri-runtime"))]
    use crate::auto_title::title_settings::ApiKeyUpdate;
    #[cfg(not(feature = "tauri-runtime"))]
    use crate::db::entities::auto_title_job;
    #[cfg(not(feature = "tauri-runtime"))]
    use sea_orm::EntityTrait;
    use crate::db::service::app_metadata_service;
    use crate::db::test_helpers::fresh_in_memory_db;
    use crate::models::agent::AgentType;

    fn default_settings() -> ConversationExperienceSettings {
        ConversationExperienceSettings {
            auto_title_api_url: String::new(),
            auto_title_api_key_set: false,
            auto_title_model: String::new(),
            auto_title_config_barrier: false,
            document_translate_agent: None,
            reference_search_limit: DEFAULT_REFERENCE_SEARCH_LIMIT,
            revision: 0,
        }
    }

    #[tokio::test]
    async fn independent_setters_preserve_the_other_field_and_advance_revision() {
        with_settings_isolation(async {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;

        let first = set_document_translate_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
            .await
            .expect("translate agent");
        let second = set_reference_search_limit_persisted_core(&db.conn, 73)
            .await
            .expect("search limit");

        assert_eq!(first.revision, 1);
        assert_eq!(second.revision, 2);
        assert_eq!(
            second.document_translate_agent,
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(second.reference_search_limit, 73);
        // Title API fields untouched.
        assert_eq!(second.auto_title_api_url, "");
        assert!(!second.auto_title_api_key_set);
            }).await;
    }

    #[tokio::test]
    async fn document_translate_agent_must_be_enabled_and_available() {
        with_settings_isolation(async {
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
        let error = set_document_translate_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
            .await
            .expect_err("disabled agent");
        assert!(matches!(error.code, AppErrorCode::ConfigurationInvalid));
            }).await;
    }

    #[tokio::test]
    async fn concurrent_independent_setters_serialize_revision_without_losing_either_field() {
        with_settings_isolation(async {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db = crate::db::init_database(temp.path(), "settings-concurrency-test")
            .await
            .expect("open pooled WAL database");

        let (agent_result, limit_result) = tokio::join!(
            set_document_translate_agent_persisted_core(&db, Some(AgentType::ClaudeCode)),
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
        assert_eq!(
            loaded.document_translate_agent,
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(loaded.reference_search_limit, 73);
        assert_eq!(loaded.revision, 2);

        drop(temp);
            }).await;
    }

    #[tokio::test]
    async fn defaults_are_off_limit_50_revision_0() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("defaults");
        assert_eq!(settings, default_settings());
            }).await;
    }

    #[tokio::test]
    async fn corrupt_agent_and_limit_values_resolve_to_safe_defaults() {
        with_settings_isolation(async {
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
        // Legacy corrupt only affects translate when new key absent.
        assert_eq!(settings.document_translate_agent, None);
        assert_eq!(
            settings.reference_search_limit,
            DEFAULT_REFERENCE_SEARCH_LIMIT
        );
        assert_eq!(settings.revision, 0);
            }).await;
    }

    #[tokio::test]
    async fn reference_limit_clamps_on_write_and_read() {
        with_settings_isolation(async {
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
        assert_eq!(read_low.reference_search_limit, MIN_REFERENCE_SEARCH_LIMIT);

        app_metadata_service::upsert_value(&db.conn, KEY_REFERENCE_SEARCH_LIMIT, "900")
            .await
            .expect("store above max");
        let read_high = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("read high");
        assert_eq!(read_high.reference_search_limit, MAX_REFERENCE_SEARCH_LIMIT);
            }).await;
    }

    #[tokio::test]
    async fn corrupt_revision_resets_to_one_on_next_write() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_SETTINGS_REVISION, "not-a-number")
            .await
            .expect("corrupt revision");

        let settings = set_reference_search_limit_persisted_core(&db.conn, 42)
            .await
            .expect("write after corrupt revision");
        assert_eq!(settings.revision, 1);
        assert_eq!(settings.reference_search_limit, 42);
            }).await;
    }

    #[tokio::test]
    async fn revision_overflow_returns_database_error() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_SETTINGS_REVISION, "9223372036854775807")
            .await
            .expect("max signed revision");

        let error = set_reference_search_limit_persisted_core(&db.conn, 50)
            .await
            .expect_err("revision exhausted");
        assert!(matches!(error.code, AppErrorCode::DatabaseError));
            }).await;
    }

    #[tokio::test]
    async fn empty_agent_value_is_off_sentinel() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, "")
            .await
            .expect("empty off sentinel");
        let agent = load_auto_title_agent_from(&db.conn)
            .await
            .expect("load empty");
        assert_eq!(agent, None);
            }).await;
    }

    // ── Document translate loader ───────────────────────────────────────────

    #[tokio::test]
    async fn translate_loader_absent_falls_back_to_legacy() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let raw = serde_json::to_string(&AgentType::Codex).unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, &raw)
            .await
            .expect("legacy");
        let agent = load_document_translate_agent_from(&db.conn)
            .await
            .expect("load");
        assert_eq!(agent, Some(AgentType::Codex));
            }).await;
    }

    #[tokio::test]
    async fn translate_loader_present_empty_is_explicit_off_no_legacy() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let raw = serde_json::to_string(&AgentType::Codex).unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, &raw)
            .await
            .expect("legacy");
        app_metadata_service::upsert_value(&db.conn, KEY_DOCUMENT_TRANSLATE_AGENT, "")
            .await
            .expect("explicit off");
        let agent = load_document_translate_agent_from(&db.conn)
            .await
            .expect("load");
        assert_eq!(agent, None);
            }).await;
    }

    #[tokio::test]
    async fn translate_loader_present_agent_and_corrupt() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let raw = serde_json::to_string(&AgentType::Gemini).unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_DOCUMENT_TRANSLATE_AGENT, &raw)
            .await
            .expect("agent");
        assert_eq!(
            load_document_translate_agent_from(&db.conn)
                .await
                .expect("load"),
            Some(AgentType::Gemini)
        );

        app_metadata_service::upsert_value(
            &db.conn,
            KEY_DOCUMENT_TRANSLATE_AGENT,
            "not-valid-json",
        )
        .await
        .expect("corrupt");
        // Legacy would be Codex if we fell back — ensure we do not.
        let legacy = serde_json::to_string(&AgentType::Codex).unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, &legacy)
            .await
            .expect("legacy");
        assert_eq!(
            load_document_translate_agent_from(&db.conn)
                .await
                .expect("load corrupt"),
            None
        );
            }).await;
    }

    #[tokio::test]
    async fn set_document_translate_agent_writes_new_key_not_title_fields() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let saved =
            set_document_translate_agent_persisted_core(&db, Some(AgentType::ClaudeCode))
                .await
                .expect("set");
        assert_eq!(
            saved.document_translate_agent,
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(saved.auto_title_api_url, "");
        assert!(!saved.auto_title_api_key_set);

        let off = set_document_translate_agent_persisted_core(&db, None)
            .await
            .expect("off");
        assert_eq!(off.document_translate_agent, None);
        // Key present as empty string (no legacy fallback).
        let raw = app_metadata_service::get_value(&db.conn, KEY_DOCUMENT_TRANSLATE_AGENT)
            .await
            .expect("raw");
        assert_eq!(raw.as_deref(), Some(""));
            }).await;
    }

    // ── Settings / title-key isolation ──────────────────────────────────────
    // Process-global CODEG_DATA_DIR + title_key hooks: same isolation as
    // title_key unit tests and concurrent tokens claim test.
    // Lock order: temp_env first, then SuiteGuard (never reverse).
    //
    // SuiteGuard is exclusive (one active suite). Override hooks only apply on
    // the owning thread (push/allow/fail_next panic without owner; get drains
    // overrides only when is_suite_owner()). Parallel harness threads hit the
    // real keyring and cannot steal the owner's queue even while suite_active.
    // Any test that loads settings or queues overrides must hold SuiteGuard.
    // Server mode also pins an empty temp `CODEG_DATA_DIR` so ambient process
    // env cannot leak a real tokens.json Present into `auto_title_api_key_set`.

    /// Isolated env + exclusive title-key suite lock; restores `CODEG_DATA_DIR`
    /// on every exit path (panic, early return, success).
    async fn with_settings_isolation(body: impl std::future::Future<Output = ()>) {
        #[cfg(not(feature = "tauri-runtime"))]
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let data_dir = dir.path().to_string_lossy().to_string();
            temp_env::async_with_vars(
                [("CODEG_DATA_DIR", Some(data_dir.as_str()))],
                async move {
                    let _suite = title_key::test_hooks::SuiteGuard::enter();
                    barrier_commit_hooks::reset();
                    body.await;
                    barrier_commit_hooks::reset();
                },
            )
            .await;
        }
        #[cfg(feature = "tauri-runtime")]
        {
            let _suite = title_key::test_hooks::SuiteGuard::enter();
            barrier_commit_hooks::reset();
            // OS keyring is process-global; queue Absent so ambient Present cannot
            // leak into auto_title_api_key_set assertions under parallel suites.
            for _ in 0..64 {
                title_key::test_hooks::push_override_get(TitleKeyState::Absent);
            }
            body.await;
            barrier_commit_hooks::reset();
        }
    }

    /// Alias used by title API config tests (server file keyring).
    #[cfg(not(feature = "tauri-runtime"))]
    async fn with_title_config_env(body: impl std::future::Future<Output = ()>) {
        with_settings_isolation(body).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn set_keep_clear_roundtrip_no_secret_on_get() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;
        let set = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example.com/v1".into(),
            ApiKeyUpdate::Set("sk-secret-value".into()),
            "gpt-4o-mini".into(),
        )
        .await
        .expect("set");

        assert_eq!(set.auto_title_api_url, "https://api.example.com/v1");
        assert!(set.auto_title_api_key_set);
        assert_eq!(set.auto_title_model, "gpt-4o-mini");
        assert!(!set.auto_title_config_barrier);
        let json = serde_json::to_string(&set).expect("ser");
        assert!(!json.contains("sk-secret-value"));
        assert!(!json.contains("api_key\""));

        let keep = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example.com/v1".into(),
            ApiKeyUpdate::Keep,
            "gpt-4o-mini".into(),
        )
        .await
        .expect("keep");
        assert!(keep.auto_title_api_key_set);
        match get_title_api_key() {
            TitleKeyState::Present(s) => assert_eq!(s, "sk-secret-value"),
            other => panic!("expected Present, got {other:?}"),
        }

        let clear = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example.com/v1".into(),
            ApiKeyUpdate::Clear,
            "gpt-4o-mini".into(),
        )
        .await
        .expect("clear");
        assert!(!clear.auto_title_api_key_set);
        assert!(!clear.auto_title_config_barrier);
        assert!(matches!(get_title_api_key(), TitleKeyState::Absent));

        let fp = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP)
            .await
            .expect("fp");
        assert_eq!(fp.as_deref(), Some(""));
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn barrier_disables_even_when_fields_look_complete() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example.com/v1".into(),
            ApiKeyUpdate::Set("sk-a".into()),
            "m".into(),
        )
        .await
        .expect("set");

        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER, BARRIER_RAISED)
            .await
            .expect("barrier");
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);
        assert!(settings.auto_title_api_key_set);
        use crate::auto_title::title_settings::auto_title_enabled;
        assert!(!auto_title_enabled(
            &settings.auto_title_api_url,
            settings.auto_title_api_key_set,
            &settings.auto_title_model,
            settings.auto_title_config_barrier,
        ));
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn preflight_unavailable_raises_barrier_no_url_change() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        set_auto_title_api_config_persisted_core(
            &db,
            "https://old.example/v1".into(),
            ApiKeyUpdate::Set("sk-old".into()),
            "old-model".into(),
        )
        .await
        .expect("seed");

        title_key::test_hooks::push_override_get(TitleKeyState::Unavailable);

        let err = set_auto_title_api_config_persisted_core(
            &db,
            "https://new.example/v1".into(),
            ApiKeyUpdate::Keep,
            "new-model".into(),
        )
        .await
        .expect_err("unavailable");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);
        assert_eq!(settings.auto_title_api_url, "https://old.example/v1");
        assert_eq!(settings.auto_title_model, "old-model");

        title_key::test_hooks::reset();
        assert!(matches!(get_title_api_key(), TitleKeyState::Present(_)));
        assert!(settings.auto_title_config_barrier);
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn verify_mismatch_keep_and_set_leave_barrier() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-A".into()),
            "m".into(),
        )
        .await
        .expect("seed");

        // Gets: preflight + barrier load_settings + verify.
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-A".into()));
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-A".into()));
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-B".into()));

        let err = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Keep,
            "m".into(),
        )
        .await
        .expect_err("keep mismatch");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);

        // Set N then verify Present(A≠N): preflight + barrier load + verify.
        title_key::test_hooks::reset();
        set_title_api_key("sk-A").expect("restore A");
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-A".into()));
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-A".into()));
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-A".into()));

        let err = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-N".into()),
            "m".into(),
        )
        .await
        .expect_err("set verify mismatch");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn set_fails_after_barrier_leaves_barrier_and_cancels() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
        let gate = ConversationExperienceMutationGate::default();
        let (arrival, release) = coordinator.pause_next_cancel_all_before_effect().await;

        title_key::test_hooks::fail_next_set();

        let db2 = AppDatabase {
            conn: db.conn.clone(),
        };
        let coord = std::sync::Arc::clone(&coordinator);
        let set_task = tokio::spawn(async move {
            set_auto_title_api_config_core(
                &db2,
                &EventEmitter::Noop,
                &coord,
                &gate,
                "https://api.example/v1".into(),
                ApiKeyUpdate::Set("sk-new".into()),
                "m".into(),
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), arrival)
            .await
            .expect("cancel arrival")
            .expect("oneshot");
        release.send(()).expect("release");

        let err = set_task.await.expect("join").expect_err("set fail");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);
        assert_eq!(settings.auto_title_api_url, "");
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn post_commit_key_drift_re_raises_barrier() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        // Success path gets: preflight, barrier load, verify, commit load, post-commit.
        // Allow 4 real reads; inject drift only on the post-commit check.
        title_key::test_hooks::allow_real_gets(4);
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-other".into()));

        let err = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-N".into()),
            "m".into(),
        )
        .await
        .expect_err("drift");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));
        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(settings.auto_title_config_barrier);
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn success_stores_fp_and_clears_barrier() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let saved = set_auto_title_api_config_persisted_core(
            &db,
            "https://api.example.com/v1?x=1".into(),
            ApiKeyUpdate::Set("sk-fp-test".into()),
            "  model-x  ".into(),
        )
        .await
        .expect("ok");

        assert_eq!(saved.auto_title_api_url, "https://api.example.com/v1");
        assert_eq!(saved.auto_title_model, "model-x");
        assert!(saved.auto_title_api_key_set);
        assert!(!saved.auto_title_config_barrier);

        let fp = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP)
            .await
            .expect("fp")
            .expect("present");
        assert_eq!(fp, title_key_fingerprint("sk-fp-test"));

        let gen = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN)
            .await
            .expect("gen")
            .expect("present");
        assert!(gen.parse::<u64>().unwrap() >= 2);
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn ambiguous_barrier_raise_commit_still_cancels_when_persisted() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
        let gate = ConversationExperienceMutationGate::default();
        let (arrival, release) = coordinator.pause_next_cancel_all_before_effect().await;

        // Step 6 raise commits durably then reports Err.
        barrier_commit_hooks::fail_next_raise_as_ambiguous();

        let db2 = AppDatabase {
            conn: db.conn.clone(),
        };
        let coord = std::sync::Arc::clone(&coordinator);
        let set_task = tokio::spawn(async move {
            set_auto_title_api_config_core(
                &db2,
                &EventEmitter::Noop,
                &coord,
                &gate,
                "https://api.example/v1".into(),
                ApiKeyUpdate::Set("sk-new".into()),
                "m".into(),
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), arrival)
            .await
            .expect("cancel_all after ambiguous barrier raise")
            .expect("oneshot");
        release.send(()).expect("release");

        let err = set_task.await.expect("join").expect_err("ambiguous raise");
        assert!(matches!(err.code, AppErrorCode::DatabaseError));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(
            settings.auto_title_config_barrier,
            "barrier must remain raised after Err-after-persist"
        );
        assert_eq!(settings.auto_title_api_url, "");
        // Keyring must not have been mutated after failed step 6.
        match get_title_api_key() {
            TitleKeyState::Absent => {}
            other => panic!("expected Absent keyring after raise failure, got {other:?}"),
        }
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn ambiguous_success_commit_re_raises_barrier_and_cancels() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
        let gate = ConversationExperienceMutationGate::default();

        // Success path: raise (real) + keyring + verify + success commit ambiguous.
        barrier_commit_hooks::fail_next_success_as_ambiguous();

        // cancel_all is invoked after step-6 raise and again on recovery; arm pause
        // for the first cancel only so the write can complete into recovery.
        let err = set_auto_title_api_config_core(
            &db,
            &EventEmitter::Noop,
            &coordinator,
            &gate,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-ambiguous".into()),
            "m".into(),
        )
        .await
        .expect_err("ambiguous success");
        assert!(matches!(err.code, AppErrorCode::DatabaseError));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        assert!(
            settings.auto_title_config_barrier,
            "fail-closed must re-raise barrier after ambiguous success commit"
        );
            }).await;
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn ambiguous_success_compensating_raise_fail_forces_off() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
        let gate = ConversationExperienceMutationGate::default();

        // Step 6 raise succeeds (1 skip); success commit reports Err after persist
        // (url/model/fp/clear-barrier); recovery raise fails cleanly (barrier stays
        // false). Fail-closed must still leave no claimable enabled state.
        barrier_commit_hooks::fail_next_success_as_ambiguous();
        barrier_commit_hooks::fail_raise_clean_after_skips(1);

        let err = set_auto_title_api_config_core(
            &db,
            &EventEmitter::Noop,
            &coordinator,
            &gate,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-force-off".into()),
            "m".into(),
        )
        .await
        .expect_err("ambiguous success + raise fail");
        assert!(matches!(err.code, AppErrorCode::DatabaseError));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        use crate::auto_title::title_settings::auto_title_enabled;
        assert!(
            !auto_title_enabled(
                &settings.auto_title_api_url,
                settings.auto_title_api_key_set,
                &settings.auto_title_model,
                settings.auto_title_config_barrier,
            ),
            "must not leave claimable enabled state after compensating raise failure"
        );
        assert!(
            settings.auto_title_config_barrier,
            "force-off must raise barrier even when key delete succeeds"
        );
        assert_eq!(settings.auto_title_api_url, "");
        assert_eq!(settings.auto_title_model, "");
        assert!(
            !settings.auto_title_api_key_set,
            "key must be deleted so enabled is false even if barrier is still clear"
        );
        match get_title_api_key() {
            TitleKeyState::Absent => {}
            other => panic!("expected Absent keyring after force-off, got {other:?}"),
        }
        let fp = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP)
            .await
            .expect("fp");
        assert_eq!(
            fp.as_deref(),
            Some(""),
            "stored key fingerprint must be cleared"
        );
        // Jobs must be wiped (no pending claimable work).
        let jobs = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("list jobs");
        assert!(jobs.is_empty());
            }).await;
    }

    /// R3 critical: keyring delete failure must not leave claimable On.
    /// Force-off writes barrier + empty url/model/fp in DB even when delete fails.
    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn ambiguous_success_force_off_key_delete_fail_leaves_enabled_false() {
        with_title_config_env(async {
        let db = fresh_in_memory_db().await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());
        let gate = ConversationExperienceMutationGate::default();

        barrier_commit_hooks::fail_next_success_as_ambiguous();
        barrier_commit_hooks::fail_raise_clean_after_skips(1);
        // Force-off path's delete_title_api_key fails cleanly; key may remain.
        crate::auto_title::title_key::test_hooks::fail_next_delete();

        let err = set_auto_title_api_config_core(
            &db,
            &EventEmitter::Noop,
            &coordinator,
            &gate,
            "https://api.example/v1".into(),
            ApiKeyUpdate::Set("sk-delete-fail".into()),
            "m".into(),
        )
        .await
        .expect_err("ambiguous success + raise fail + key delete fail");
        assert!(matches!(err.code, AppErrorCode::DatabaseError));

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        use crate::auto_title::title_settings::auto_title_enabled;
        assert!(
            !auto_title_enabled(
                &settings.auto_title_api_url,
                settings.auto_title_api_key_set,
                &settings.auto_title_model,
                settings.auto_title_config_barrier,
            ),
            "key-delete failure must not leave claimable enabled state"
        );
        assert!(
            settings.auto_title_config_barrier,
            "barrier must be raised as durable Off independent of keyring"
        );
        assert_eq!(
            settings.auto_title_api_url, "",
            "url must be cleared so enabled is false even with present key"
        );
        assert_eq!(
            settings.auto_title_model, "",
            "model must be cleared so enabled is false even with present key"
        );
        let fp = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP)
            .await
            .expect("fp");
        assert_eq!(fp.as_deref(), Some(""), "fp must be cleared");
        let jobs = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("list jobs");
        assert!(jobs.is_empty(), "jobs must be wiped");
        // Key may still be present in keyring — enabled must still be false.
        assert!(
            settings.auto_title_api_key_set
                || matches!(get_title_api_key(), TitleKeyState::Present(_)),
            "this branch exercises delete failure with a still-present key"
        );
            }).await;
    }

    #[tokio::test]
    async fn get_settings_omits_legacy_auto_title_agent_and_maps_translate_fallback() {
        with_settings_isolation(async {
        let db = fresh_in_memory_db().await;
        let raw = serde_json::to_string(&AgentType::Codex).unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, &raw)
            .await
            .expect("legacy agent metadata");

        let settings = get_conversation_experience_settings_core(&db.conn)
            .await
            .expect("get");
        // Wire document no longer carries auto_title_agent; translate falls back.
        let json = serde_json::to_value(&settings).expect("serialize");
        assert!(json.get("auto_title_agent").is_none());
        assert_eq!(settings.auto_title_api_url, "");
        assert!(!settings.auto_title_api_key_set);
        assert_eq!(settings.document_translate_agent, Some(AgentType::Codex));
            }).await;
    }


    // ── Live registry fixtures (limit epoch + mutation gate) ────────────────

    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use crate::reference_search::matcher::SearchPattern;
    use crate::reference_search::sources::{
        ReferenceSourceCursor, ReferenceSourceFactory, SourcePage,
    };
    use crate::reference_search::types::{
        ReferenceDoneReason, ReferenceSearchPage, ReferenceSearchSource,
        StartReferenceSearchRequest,
    };
    use crate::reference_search::ReferenceSearchRegistry;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

    struct BlockedFactory {
        releases: Arc<tokio::sync::Mutex<Vec<oneshot::Sender<()>>>>,
        started: Arc<std::sync::atomic::AtomicUsize>,
    }

    struct BlockedCursor {
        release_rx: Option<oneshot::Receiver<()>>,
        started: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ReferenceSourceCursor for BlockedCursor {
        async fn next_page(
            &mut self,
            _page_size: usize,
            token: CancellationToken,
        ) -> Result<SourcePage, crate::app_error::AppCommandError> {
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(rx) = self.release_rx.take() {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        return Err(crate::app_error::AppCommandError::new(
                            AppErrorCode::Cancelled,
                            "cancelled",
                        ));
                    }
                    result = rx => {
                        let _ = result;
                    }
                }
            }
            Ok(SourcePage {
                items: Vec::new(),
                source_epoch: None,
                done: true,
                done_reason: Some(ReferenceDoneReason::Exhausted),
            })
        }

        async fn close(&mut self) {}
    }

    #[async_trait]
    impl ReferenceSourceFactory for BlockedFactory {
        async fn open(
            &self,
            _request: &StartReferenceSearchRequest,
            _pattern: SearchPattern,
            _limit: usize,
        ) -> Result<Box<dyn ReferenceSourceCursor>, crate::app_error::AppCommandError> {
            let (tx, rx) = oneshot::channel();
            self.releases.lock().await.push(tx);
            Ok(Box::new(BlockedCursor {
                release_rx: Some(rx),
                started: Arc::clone(&self.started),
            }))
        }
    }

    struct LiveRegistryFixture {
        db: crate::db::AppDatabase,
        emitter: EventEmitter,
        registry: Arc<ReferenceSearchRegistry>,
        mutation_gate: ConversationExperienceMutationGate,
        #[allow(dead_code)]
        broadcaster: Arc<WebEventBroadcaster>,
        settings_rx: tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
        started: Arc<std::sync::atomic::AtomicUsize>,
        #[allow(dead_code)]
        releases: Arc<tokio::sync::Mutex<Vec<oneshot::Sender<()>>>>,
        blocked_job:
            Option<JoinHandle<Result<ReferenceSearchPage, crate::app_error::AppCommandError>>>,
        blocked_request: Option<StartReferenceSearchRequest>,
    }

    async fn live_registry_fixture(limit: u16) -> LiveRegistryFixture {
        let db = fresh_in_memory_db().await;
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let settings_rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster.clone());
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let releases = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let factory = Arc::new(BlockedFactory {
            releases: Arc::clone(&releases),
            started: Arc::clone(&started),
        });
        let registry = ReferenceSearchRegistry::new(limit, factory);
        LiveRegistryFixture {
            db,
            emitter,
            registry,
            mutation_gate: ConversationExperienceMutationGate::default(),
            broadcaster,
            settings_rx,
            started,
            releases,
            blocked_job: None,
            blocked_request: None,
        }
    }

    impl LiveRegistryFixture {
        async fn start_blocked_job(&mut self) {
            let request = StartReferenceSearchRequest {
                search_session_id: Uuid::new_v4().hyphenated().to_string(),
                source_sequence: 1,
                request_id: Uuid::new_v4().hyphenated().to_string(),
                source: ReferenceSearchSource::File,
                query: "blocked".into(),
                workspace_path: Some("/tmp/live-registry".into()),
            };
            let registry = Arc::clone(&self.registry);
            let start = request.clone();
            let handle = tokio::spawn(async move { registry.start(start).await });
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if self.started.load(std::sync::atomic::Ordering::SeqCst) >= 1 {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("blocked job never entered scan");
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            self.blocked_job = Some(handle);
            self.blocked_request = Some(request);
        }

        async fn old_job_cancelled(&mut self) -> bool {
            let Some(handle) = self.blocked_job.take() else {
                return false;
            };
            match tokio::time::timeout(Duration::from_secs(2), handle).await {
                Ok(Ok(Err(error))) => error.code == AppErrorCode::LimitEpochChanged,
                other => {
                    panic!("expected LimitEpochChanged on blocked job, got {other:?}");
                }
            }
        }

        fn last_settings_event(&mut self) -> ConversationExperienceSettings {
            let mut last = None;
            loop {
                match self.settings_rx.try_recv() {
                    Ok(evt) if evt.channel == CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT => {
                        last = Some(
                            serde_json::from_value(evt.payload.as_ref().clone())
                                .expect("settings payload"),
                        );
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            last.expect("settings event")
        }
    }

    #[tokio::test]
    async fn setting_limit_cancels_old_epoch_and_broadcasts_full_snapshot() {
        with_settings_isolation(async {
        let mut fixture = live_registry_fixture(50).await;
        fixture.start_blocked_job().await;
        let saved = set_reference_search_limit_core(
            &fixture.db.conn,
            &fixture.emitter,
            &fixture.registry,
            &fixture.mutation_gate,
            25,
        )
        .await
        .expect("limit");
        assert_eq!(saved.reference_search_limit, 25);
        assert!(fixture.old_job_cancelled().await);
        assert_eq!(fixture.last_settings_event().revision, saved.revision);
            }).await;
    }

    #[tokio::test]
    async fn concurrent_limit_saves_hold_the_gate_through_registry_application() {
        with_settings_isolation(async {
        let mut fixture = live_registry_fixture(50).await;
        let (arrival, release) = fixture
            .registry
            .pause_next_limit_apply_before_effect()
            .await;

        let db = fixture.db.conn.clone();
        let emitter = fixture.emitter.clone();
        let registry = Arc::clone(&fixture.registry);
        let gate = Arc::new(ConversationExperienceMutationGate::default());

        let first = {
            let db = db.clone();
            let emitter = emitter.clone();
            let registry = Arc::clone(&registry);
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                set_reference_search_limit_core(&db, &emitter, &registry, &gate, 20).await
            })
        };

        tokio::time::timeout(Duration::from_secs(2), arrival)
            .await
            .expect("first set_limit arrival")
            .expect("arrival oneshot");

        let mut second = {
            let db = db.clone();
            let emitter = emitter.clone();
            let registry = Arc::clone(&registry);
            let gate = Arc::clone(&gate);
            tokio::spawn(async move {
                set_reference_search_limit_core(&db, &emitter, &registry, &gate, 30).await
            })
        };

        let early = tokio::time::timeout(Duration::from_millis(50), &mut second).await;
        assert!(
            early.is_err(),
            "second limit save must stay pending while first holds the gate through set_limit"
        );

        release.send(()).expect("release first set_limit");

        let first_saved = first.await.expect("join first").expect("first ok");
        let second_saved = second.await.expect("join second").expect("second ok");

        assert_eq!(first_saved.reference_search_limit, 20);
        assert_eq!(second_saved.reference_search_limit, 30);
        assert!(second_saved.revision > first_saved.revision);

        let loaded = get_conversation_experience_settings_core(&db)
            .await
            .expect("load");
        assert_eq!(loaded.reference_search_limit, 30);
        assert_eq!(loaded.revision, second_saved.revision);
        assert_eq!(registry.current_limit().await, 30);
        assert_eq!(registry.current_limit_epoch().await, 2);

        let last_event = fixture.last_settings_event();
        assert_eq!(last_event.revision, second_saved.revision);
        assert_eq!(last_event.reference_search_limit, 30);
            }).await;
    }
}
