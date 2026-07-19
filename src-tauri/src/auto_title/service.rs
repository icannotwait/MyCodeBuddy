//! Enrollment, job cancellation, generated-title finalization, prompt capture,
//! and durable usable-completion transitions.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, Order, QueryFilter, QueryOrder, Set, TransactionTrait,
};

use crate::acp::types::PromptInputBlock;
use crate::auto_title::context::{bound_context, project_visible_prompt};
use crate::auto_title::types::{
    app_locale_to_wire, parse_supported_app_locale, AutoTitleClaim, CapturedPrompt,
    CompletionTransition, FailureTransition, FinalizeTitleOutcome, PromptCaptureContext,
    TurnCompletionSnapshot,
};
use crate::commands::conversation_experience::load_auto_title_agent_from;
use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
use crate::db::entities::conversation;
use crate::db::error::DbError;
use crate::models::agent::AgentType;
use crate::models::system::AppLocale;

/// Enroll a newly created conversation for automatic titles when the setting
/// is On. Reads the agent through [`load_auto_title_agent_from`] so the Off
/// sentinel (`""`) is not treated as enabled. Returns `true` when a job row
/// was inserted.
pub async fn enroll_new_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let Some(_agent) = load_auto_title_agent_from(conn).await? else {
        return Ok(false);
    };

    auto_title_job::ActiveModel {
        conversation_id: Set(conversation_id),
        state: Set(AutoTitleJobState::AwaitingTurn),
        attempts: Set(0),
        first_user_text: Set(None),
        first_assistant_text: Set(None),
        first_prompt_at: Set(None),
        locale: Set(None),
        usable_turn_seq: Set(0),
        attempt_turn_seq: Set(0),
        last_usable_turn_token: Set(None),
        updated_at: Set(now),
    }
    .insert(conn)
    .await?;

    Ok(true)
}

/// Delete the auto-title job for `conversation_id` if present. Returns `true`
/// when a row was removed (callers must cancel in-flight work after commit).
pub async fn cancel_job<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<bool, DbError> {
    let result = auto_title_job::Entity::delete_by_id(conversation_id)
        .exec(conn)
        .await?;
    Ok(result.rows_affected > 0)
}

/// Atomically commit a generated title for the exact running claim, or cancel
/// when the conversation is locked/finalized/deleted or the job no longer
/// matches. Never bumps `updated_at`.
pub async fn finalize_generated_title(
    conn: &DatabaseConnection,
    claim: &AutoTitleClaim,
    title: &str,
) -> Result<FinalizeTitleOutcome, DbError> {
    let txn = conn.begin().await?;

    let job = auto_title_job::Entity::find_by_id(claim.conversation_id)
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
        .filter(auto_title_job::Column::Attempts.eq(claim.attempt))
        .filter(auto_title_job::Column::AttemptTurnSeq.eq(claim.attempt_turn_seq))
        .one(&txn)
        .await?;

    if job.is_none() {
        txn.commit().await?;
        return Ok(FinalizeTitleOutcome::Cancelled);
    }

    let updated = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .col_expr(conversation::Column::AutoTitleFinalized, Expr::value(true))
        .filter(conversation::Column::Id.eq(claim.conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::AutoTitleFinalized.eq(false))
        .exec(&txn)
        .await?;

    if updated.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(FinalizeTitleOutcome::Cancelled);
    }

    auto_title_job::Entity::delete_by_id(claim.conversation_id)
        .exec(&txn)
        .await?;

    txn.commit().await?;
    Ok(FinalizeTitleOutcome::Committed)
}

/// Capture bounded visible prompt context for an accepted linked prompt.
///
/// - `Some(visible_text)` (including empty) is authoritative and never falls
///   back to wire blocks; `None`/absent uses the privacy-safe projection.
/// - Locale prefers an explicit capture locale, else `fallback_locale`.
/// - When a job row still exists, writes `first_user_text` + `first_prompt_at`
///   once via a CAS update (both columns still NULL). Concurrent losers and
///   later captures only refresh locale (any surviving job state).
pub async fn capture_prompt_context<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    blocks: &[PromptInputBlock],
    capture: Option<&PromptCaptureContext>,
    fallback_locale: AppLocale,
) -> Result<CapturedPrompt, DbError> {
    let raw_visible = match capture.and_then(|c| c.visible_text.as_ref()) {
        Some(text) => text.clone(),
        None => project_visible_prompt(blocks),
    };
    let visible_text = bound_context(&raw_visible);
    let locale = capture.and_then(|c| c.locale).unwrap_or(fallback_locale);

    // Conditional first-fields write: only when both are still NULL.
    let now = Utc::now();
    let locale_wire = app_locale_to_wire(locale).to_string();
    let first_write = auto_title_job::Entity::update_many()
        .col_expr(
            auto_title_job::Column::FirstUserText,
            Expr::value(visible_text.clone()),
        )
        .col_expr(auto_title_job::Column::FirstPromptAt, Expr::value(now))
        .col_expr(
            auto_title_job::Column::Locale,
            Expr::value(locale_wire.clone()),
        )
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
        .filter(auto_title_job::Column::FirstUserText.is_null())
        .filter(auto_title_job::Column::FirstPromptAt.is_null())
        .exec(conn)
        .await?;

    if first_write.rows_affected == 0 {
        // Job may exist with first fields set (or be absent): refresh locale only.
        // When no job row exists both updates affect 0 rows — fine.
        auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::Locale,
                Expr::value(locale_wire),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
            .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
            .exec(conn)
            .await?;
    }

    Ok(CapturedPrompt {
        visible_text,
        locale,
    })
}

/// Apply a usable turn completion to the auto-title job inside an open transaction.
///
/// Only `end_turn` with non-empty trimmed final text advances the job. Duplicate
/// turn tokens are full no-ops (no seq bump, no locale thrash). Progress uses
/// atomic `usable_turn_seq = usable_turn_seq + 1` with a token guard so concurrent
/// distinct tokens cannot lose increments via stale RMW.
///
/// First-assistant is write-once (`awaiting_turn` + `first_assistant_text IS NULL`).
/// Deadline snapshots (`Some(partial)` / `Some("")`) are never refined. `retry_wait
/// → ready` advances state without touching `first_assistant_text`.
pub async fn apply_usable_completion(
    txn: &DatabaseTransaction,
    snapshot: &TurnCompletionSnapshot,
    stop_reason: &str,
) -> Result<CompletionTransition, DbError> {
    // 0) Early exit if stop_reason unusable or final_text empty.
    if stop_reason != "end_turn" || snapshot.final_text.trim().is_empty() {
        let job = auto_title_job::Entity::find_by_id(snapshot.conversation_id)
            .one(txn)
            .await?;
        return Ok(CompletionTransition {
            usable_turn_seq: job.map(|j| j.usable_turn_seq).unwrap_or(0),
            became_ready: false,
        });
    }

    let now = Utc::now();
    let locale_wire = app_locale_to_wire(snapshot.locale).to_string();
    let bounded = bound_context(snapshot.final_text.trim());

    // Test-only gate before any usable-completion write so a concurrent deadline
    // promote can commit first (SQLite blocks promote if this txn already wrote).
    // Task-local: parallel tests cannot steal the barrier.
    #[cfg(test)]
    first_ready_race_hooks::run_completion_pre_write_hook().await;

    // 1) Atomic progress (token idempotent) — any live job state.
    let progress = auto_title_job::Entity::update_many()
        .col_expr(
            auto_title_job::Column::UsableTurnSeq,
            Expr::col(auto_title_job::Column::UsableTurnSeq).add(1),
        )
        .col_expr(
            auto_title_job::Column::LastUsableTurnToken,
            Expr::value(snapshot.turn_token.clone()),
        )
        .col_expr(
            auto_title_job::Column::Locale,
            Expr::value(locale_wire),
        )
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(snapshot.conversation_id))
        .filter(
            Condition::any()
                .add(auto_title_job::Column::LastUsableTurnToken.is_null())
                .add(
                    auto_title_job::Column::LastUsableTurnToken
                        .ne(snapshot.turn_token.clone()),
                ),
        )
        .exec(txn)
        .await?;

    if progress.rows_affected == 0 {
        // Duplicate token or missing job — full no-op (no first-ready side effects).
        let job = auto_title_job::Entity::find_by_id(snapshot.conversation_id)
            .one(txn)
            .await?;
        return Ok(CompletionTransition {
            usable_turn_seq: job.map(|j| j.usable_turn_seq).unwrap_or(0),
            became_ready: false,
        });
    }

    // 2) First-ready from awaiting_turn (write-once assistant; shared guard with
    //    deadline promote so end-turn cannot refine a deadline snapshot).
    let first_ready = auto_title_job::Entity::update_many()
        .col_expr(
            auto_title_job::Column::FirstAssistantText,
            Expr::value(bounded),
        )
        .col_expr(
            auto_title_job::Column::State,
            Expr::value(AutoTitleJobState::Ready),
        )
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(snapshot.conversation_id))
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::AwaitingTurn))
        .filter(auto_title_job::Column::FirstAssistantText.is_null())
        .exec(txn)
        .await?;

    let mut became_ready = first_ready.rows_affected == 1;

    // 3) retry_wait → ready WITHOUT touching first_assistant_text.
    let retry_ready = auto_title_job::Entity::update_many()
        .col_expr(
            auto_title_job::Column::State,
            Expr::value(AutoTitleJobState::Ready),
        )
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(snapshot.conversation_id))
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::RetryWait))
        .exec(txn)
        .await?;
    became_ready |= retry_ready.rows_affected == 1;

    // 4) Read back usable_turn_seq after atomic progress.
    let job = auto_title_job::Entity::find_by_id(snapshot.conversation_id)
        .one(txn)
        .await?
        .ok_or_else(|| {
            DbError::Validation(
                "auto-title job disappeared after usable completion progress".into(),
            )
        })?;

    Ok(CompletionTransition {
        usable_turn_seq: job.usable_turn_seq,
        became_ready,
    })
}

/// Claim the oldest ready job: `ready → running`, increment attempts, snapshot
/// the configured agent. When the setting is Off, delete all ready orphans and
/// return `None` so the worker does not spin.
///
/// Claim rules for Ready rows:
/// - empty / missing `first_user_text` → delete and continue
/// - `first_assistant_text == Some("")` (or any `Some`) → claimable
/// - `first_assistant_text == None` → invalid Ready; delete and continue
///
/// Each attempt begins a fresh transaction for select + CAS. A lost claim CAS
/// always `rollback`s and loops (never reuses a dirty snapshot under one open
/// txn). `attempt_turn_seq` is set from the row's current `usable_turn_seq` in
/// the same UPDATE so a concurrent usable-turn advance cannot pair a stale
/// attempt with a newer sequence.
///
/// Transient SQLite contention (busy / locked / snapshot) on the claim CAS is
/// retried with a bound; permanent write failures propagate as `Err` so the
/// coordinator drain can back off instead of hanging forever.
pub async fn claim_next_ready(
    conn: &DatabaseConnection,
) -> Result<Option<AutoTitleClaim>, DbError> {
    /// Initial try + retries for snapshot/busy on the ready→running upgrade.
    const CLAIM_CAS_TRANSIENT_MAX_ATTEMPTS: u32 = 8;

    let mut transient_cas_failures: u32 = 0;

    loop {
        let txn = conn.begin().await?;

        let Some(agent) = load_auto_title_agent_from(&txn).await? else {
            auto_title_job::Entity::delete_many()
                .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
                .exec(&txn)
                .await?;
            txn.commit().await?;
            return Ok(None);
        };

        let candidate = auto_title_job::Entity::find()
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .order_by(auto_title_job::Column::UpdatedAt, Order::Asc)
            .order_by(auto_title_job::Column::ConversationId, Order::Asc)
            .one(&txn)
            .await?;

        let Some(job) = candidate else {
            txn.commit().await?;
            return Ok(None);
        };

        let first_user = job.first_user_text.clone().unwrap_or_default();
        if first_user.trim().is_empty() {
            // Empty / missing user — unusable Ready row.
            auto_title_job::Entity::delete_by_id(job.conversation_id)
                .exec(&txn)
                .await?;
            txn.commit().await?;
            // Progress made; transient CAS budget applies only to consecutive failures.
            transient_cas_failures = 0;
            continue;
        }

        // Do not use unwrap_or_default(): None is invalid on Ready; Some("") is claimable.
        let Some(first_assistant) = job.first_assistant_text.clone() else {
            auto_title_job::Entity::delete_by_id(job.conversation_id)
                .exec(&txn)
                .await?;
            txn.commit().await?;
            transient_cas_failures = 0;
            continue;
        };

        let locale = match parse_supported_app_locale(job.locale.as_deref()) {
            Some(locale) => locale,
            None => {
                if job.locale.is_some() {
                    tracing::warn!(
                        conversation_id = job.conversation_id,
                        locale = ?job.locale,
                        "corrupt auto-title job locale; falling back to English"
                    );
                }
                AppLocale::En
            }
        };

        // Test-only gate between select and CAS (usable_turn_seq race barrier).
        // Scoped via task-local so parallel tests cannot steal the hook.
        #[cfg(test)]
        claim_test_hooks::run_pre_cas_hook().await;

        // Atomic claim: attempt_turn_seq := usable_turn_seq on the same row
        // version being transitioned to running (no stale observed-seq write).
        let updated = match auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::Running),
            )
            .col_expr(
                auto_title_job::Column::Attempts,
                Expr::col(auto_title_job::Column::Attempts).add(1).into(),
            )
            .col_expr(
                auto_title_job::Column::AttemptTurnSeq,
                Expr::col(auto_title_job::Column::UsableTurnSeq).into(),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(auto_title_job::Column::ConversationId.eq(job.conversation_id))
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .exec(&txn)
            .await
        {
            Ok(result) => {
                transient_cas_failures = 0;
                result
            }
            Err(error) => {
                let _ = txn.rollback().await;
                if !is_transient_claim_cas_error(&error) {
                    tracing::warn!(
                        conversation_id = job.conversation_id,
                        %error,
                        "auto-title claim CAS failed with non-retryable error"
                    );
                    return Err(DbError::Database(error));
                }
                transient_cas_failures = transient_cas_failures.saturating_add(1);
                if transient_cas_failures >= CLAIM_CAS_TRANSIENT_MAX_ATTEMPTS {
                    tracing::warn!(
                        conversation_id = job.conversation_id,
                        attempts = transient_cas_failures,
                        %error,
                        "auto-title claim CAS exhausted transient retries"
                    );
                    return Err(DbError::Database(error));
                }
                // Concurrent writer may force SQLite snapshot/busy failure on
                // the upgrade to write. Retry with a fresh begin.
                tracing::debug!(
                    conversation_id = job.conversation_id,
                    attempt = transient_cas_failures,
                    %error,
                    "auto-title claim CAS transient failure; retrying with fresh transaction"
                );
                continue;
            }
        };

        if updated.rows_affected != 1 {
            // Lost the race (another claimer won) — fresh txn, re-select.
            txn.rollback().await?;
            continue;
        }

        let claimed = auto_title_job::Entity::find_by_id(job.conversation_id)
            .one(&txn)
            .await?
            .ok_or_else(|| {
                DbError::Validation(
                    "auto-title claim disappeared after successful ready→running CAS".into(),
                )
            })?;

        txn.commit().await?;
        return Ok(Some(AutoTitleClaim {
            conversation_id: claimed.conversation_id,
            attempt: claimed.attempts,
            agent,
            first_user_text: first_user,
            first_assistant_text: first_assistant,
            locale,
            attempt_turn_seq: claimed.attempt_turn_seq,
        }));
    }
}

/// True for SQLite contention / snapshot errors that may clear on a fresh txn.
fn is_transient_claim_cas_error(error: &sea_orm::DbErr) -> bool {
    let lower = error.to_string().to_ascii_lowercase();
    lower.contains("database is locked")
        || lower.contains("database is busy")
        || lower.contains("sqlite_busy")
        || lower.contains("sqlite_locked")
        || lower.contains("busy_snapshot")
        || lower.contains("code: 5")
        || lower.contains("code: 6")
        || lower.contains("code: 517")
        // SQLite "cannot commit transaction - SQL statements in progress" style
        // snapshot races sometimes surface with "snapshot" wording only.
        || lower.contains("snapshot")
}

/// Test-only hooks for deterministic claim races (select → CAS barrier).
///
/// Hook state is **task-local** (not process-global), so a parallel test's
/// `claim_next_ready` cannot steal another test's barrier. Install via
/// [`claim_test_hooks::scope`] on the same task that runs the claim.
#[cfg(test)]
mod claim_test_hooks {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    type Hook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

    tokio::task_local! {
        static PRE_CAS: Hook;
    }

    /// Run `fut` with `hook` installed for this task only.
    pub async fn scope<F, T>(hook: Hook, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        PRE_CAS.scope(hook, fut).await
    }

    pub async fn run_pre_cas_hook() {
        let Ok(hook) = PRE_CAS.try_with(Clone::clone) else {
            return;
        };
        hook().await;
    }
}

/// Test-only hooks for deterministic deadline-promote vs usable-completion races.
///
/// Both hooks are **task-local** so parallel tests cannot steal barriers.
/// - Completion hook runs **before any write** in `apply_usable_completion` so a
///   concurrent promote is not blocked by an open write transaction.
/// - Promote hook runs immediately before the promote CAS UPDATE.
#[cfg(test)]
mod first_ready_race_hooks {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    type Hook = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

    tokio::task_local! {
        static COMPLETION_PRE_WRITE: Hook;
        static PROMOTE_PRE_CAS: Hook;
    }

    pub async fn scope_completion<F, T>(hook: Hook, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        COMPLETION_PRE_WRITE.scope(hook, fut).await
    }

    pub async fn scope_promote<F, T>(hook: Hook, fut: F) -> T
    where
        F: Future<Output = T>,
    {
        PROMOTE_PRE_CAS.scope(hook, fut).await
    }

    pub async fn run_completion_pre_write_hook() {
        let Ok(hook) = COMPLETION_PRE_WRITE.try_with(Clone::clone) else {
            return;
        };
        hook().await;
    }

    pub async fn run_promote_pre_cas_hook() {
        let Ok(hook) = PROMOTE_PRE_CAS.try_with(Clone::clone) else {
            return;
        };
        hook().await;
    }
}

/// True while the exact claim still owns a `running` job row (not cancelled /
/// renamed / superseded).
pub async fn claim_is_still_running(
    conn: &DatabaseConnection,
    claim: &AutoTitleClaim,
) -> Result<bool, DbError> {
    let job = auto_title_job::Entity::find_by_id(claim.conversation_id)
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
        .filter(auto_title_job::Column::Attempts.eq(claim.attempt))
        .filter(auto_title_job::Column::AttemptTurnSeq.eq(claim.attempt_turn_seq))
        .one(conn)
        .await?;
    Ok(job.is_some())
}

/// Record a failed attempt for the exact claim. Attempt one becomes `ready` if
/// a newer usable turn already exists, else `retry_wait`. Attempt two deletes.
pub async fn record_attempt_failure(
    conn: &DatabaseConnection,
    claim: &AutoTitleClaim,
) -> Result<FailureTransition, DbError> {
    let txn = conn.begin().await?;
    let job = auto_title_job::Entity::find_by_id(claim.conversation_id)
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
        .filter(auto_title_job::Column::Attempts.eq(claim.attempt))
        .filter(auto_title_job::Column::AttemptTurnSeq.eq(claim.attempt_turn_seq))
        .one(&txn)
        .await?;

    let Some(job) = job else {
        txn.commit().await?;
        return Ok(FailureTransition::Cancelled);
    };

    if claim.attempt >= 2 {
        auto_title_job::Entity::delete_by_id(claim.conversation_id)
            .exec(&txn)
            .await?;
        txn.commit().await?;
        return Ok(FailureTransition::Exhausted);
    }

    let next = if job.usable_turn_seq > job.attempt_turn_seq {
        FailureTransition::Ready
    } else {
        FailureTransition::RetryWait
    };

    let mut active: auto_title_job::ActiveModel = job.into();
    active.state = Set(match next {
        FailureTransition::Ready => AutoTitleJobState::Ready,
        FailureTransition::RetryWait => AutoTitleJobState::RetryWait,
        _ => unreachable!(),
    });
    active.updated_at = Set(Utc::now());
    active.update(&txn).await?;
    txn.commit().await?;
    Ok(next)
}

/// Convert interrupted `running` rows into retry/ready/deleted after restart.
pub async fn recover_interrupted_jobs(conn: &DatabaseConnection) -> Result<(), DbError> {
    let running = auto_title_job::Entity::find()
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
        .all(conn)
        .await?;

    for job in running {
        let claim = AutoTitleClaim {
            conversation_id: job.conversation_id,
            attempt: job.attempts,
            agent: AgentType::ClaudeCode, // agent unused by failure transition
            first_user_text: String::new(),
            first_assistant_text: String::new(),
            locale: AppLocale::En,
            attempt_turn_seq: job.attempt_turn_seq,
        };
        let _ = record_attempt_failure(conn, &claim).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

    use crate::acp::delegation::spawner::DelegationLink;
    use crate::auto_title::types::{
        AutoTitleClaim, CompletionTransition, FinalizeTitleOutcome, TurnCompletionSnapshot,
    };
    use crate::commands::conversation_experience::{
        set_auto_title_agent_persisted_core, KEY_AUTO_TITLE_AGENT,
    };
    use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
    use crate::db::entities::conversation;
    use crate::db::service::app_metadata_service;
    use crate::db::service::conversation_service::{
        create, create_chat, create_with_delegation, update_title,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use crate::models::system::AppLocale;

    async fn seed_running_job(conn: &DatabaseConnection, conversation_id: i32, attempt: i32) {
        let now = Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(AutoTitleJobState::Running),
            attempts: Set(attempt),
            first_user_text: Set(Some("task".into())),
            first_assistant_text: Set(Some("answer".into())),
            first_prompt_at: Set(None),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(1),
            attempt_turn_seq: Set(1),
            last_usable_turn_token: Set(Some("turn-1".into())),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed running job");
    }

    async fn enable_auto_title(conn: &DatabaseConnection, agent: AgentType) {
        app_metadata_service::upsert_value(
            conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&agent).expect("serialize agent"),
        )
        .await
        .expect("enable auto title agent");
    }

    #[tokio::test]
    async fn enabled_creation_enrolls_root_and_delegate() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        crate::db::service::app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).unwrap(),
        )
        .await
        .unwrap();
        let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-enrollment").await;
        let root = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("root");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Gemini,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: root.id,
                parent_tool_use_id: "tool-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("child");

        assert!(auto_title_job::Entity::find_by_id(root.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_some());
        assert!(auto_title_job::Entity::find_by_id(child.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn manual_rename_and_generated_commit_have_atomic_precedence() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-precedence").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .unwrap();
        seed_running_job(&db.conn, conversation.id, 1).await;
        assert!(update_title(&db.conn, conversation.id, "Manual".into())
            .await
            .expect("rename"));
        let claim = AutoTitleClaim {
            conversation_id: conversation.id,
            attempt: 1,
            agent: AgentType::Codex,
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
        };
        let outcome = finalize_generated_title(&db.conn, &claim, "Generated")
            .await
            .expect("late result");
        assert_eq!(outcome, FinalizeTitleOutcome::Cancelled);
        let saved = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.title.as_deref(), Some("Manual"));
    }

    #[tokio::test]
    async fn create_create_chat_and_delegate_each_enroll_exactly_one_job_when_enabled() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-create-paths").await;

        let regular = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let chat = create_chat(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create_chat");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Gemini,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: regular.id,
                parent_tool_use_id: "tu-enroll".into(),
                delegation_call_id: "call-enroll".into(),
            }),
        )
        .await
        .expect("delegate");

        for id in [regular.id, chat.id, child.id] {
            let jobs = auto_title_job::Entity::find_by_id(id)
                .all(&db.conn)
                .await
                .expect("jobs");
            assert_eq!(jobs.len(), 1, "conversation {id} must have exactly one job");
            assert_eq!(jobs[0].state, AutoTitleJobState::AwaitingTurn);
        }

        let total = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("all jobs");
        assert_eq!(total.len(), 3);
    }

    #[tokio::test]
    async fn off_sentinel_does_not_enroll_even_when_metadata_row_exists() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, "")
            .await
            .expect("off sentinel");
        let folder = seed_folder(&db, "/tmp/title-off-sentinel").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        assert!(
            auto_title_job::Entity::find_by_id(row.id)
                .one(&db.conn)
                .await
                .expect("query")
                .is_none(),
            "empty Off sentinel must not enroll"
        );
    }

    #[tokio::test]
    async fn creation_racing_disable_leaves_no_job_when_off() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db = crate::db::init_database(temp.path(), "auto-title-create-disable-race")
            .await
            .expect("open pooled WAL database");

        enable_auto_title(&db.conn, AgentType::ClaudeCode).await;
        let folder = seed_folder(&db, "/tmp/title-create-disable-race").await;

        let (create_result, off_result) = tokio::join!(
            create(&db.conn, folder, AgentType::ClaudeCode, None, None),
            set_auto_title_agent_persisted_core(&db, None),
        );

        create_result.expect("create completed");
        let off = off_result.expect("disable completed");
        assert_eq!(off.auto_title_agent, None);
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("jobs")
                .is_empty(),
            "final state must be Off with zero jobs regardless of transaction order"
        );

        drop(temp);
    }

    #[tokio::test]
    async fn finalize_commits_when_running_claim_matches_and_unlocked() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/title-finalize-ok").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        seed_running_job(&db.conn, conversation.id, 1).await;
        let before = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();

        let claim = AutoTitleClaim {
            conversation_id: conversation.id,
            attempt: 1,
            agent: AgentType::Codex,
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
        };
        let outcome = finalize_generated_title(&db.conn, &claim, "Generated")
            .await
            .expect("finalize");
        assert_eq!(outcome, FinalizeTitleOutcome::Committed);

        let saved = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.title.as_deref(), Some("Generated"));
        assert!(saved.auto_title_finalized);
        assert!(!saved.title_locked);
        assert_eq!(saved.updated_at, before.updated_at);
        assert!(auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none());
    }

    async fn seed_job_in_state(
        conn: &DatabaseConnection,
        conversation_id: i32,
        state: AutoTitleJobState,
        first_user_text: Option<&str>,
        locale: Option<&str>,
    ) {
        let now = Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(state),
            attempts: Set(0),
            first_user_text: Set(first_user_text.map(|s| s.to_string())),
            first_assistant_text: Set(None),
            first_prompt_at: Set(None),
            locale: Set(locale.map(|s| s.to_string())),
            usable_turn_seq: Set(0),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(None),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed job");
    }

    #[tokio::test]
    async fn explicit_some_empty_visible_text_is_authoritative() {
        use crate::acp::types::PromptInputBlock;
        use crate::auto_title::service::capture_prompt_context;
        use crate::auto_title::types::PromptCaptureContext;

        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-empty-auth").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");

        let wire_blocks = vec![PromptInputBlock::Text {
            text: "wire-fallback-must-not-win".into(),
        }];
        let capture = PromptCaptureContext::new(Some(String::new()), Some(AppLocale::ZhCn));
        let captured = capture_prompt_context(
            &db.conn,
            conversation.id,
            &wire_blocks,
            Some(&capture),
            AppLocale::En,
        )
        .await
        .expect("capture");

        assert_eq!(
            captured.visible_text, "",
            "Some(\"\") must not fall back to wire blocks"
        );
        assert_eq!(captured.locale, AppLocale::ZhCn);

        let job = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(job.first_user_text.as_deref(), Some(""));
        assert_eq!(job.locale.as_deref(), Some("zh_cn"));
    }

    #[tokio::test]
    async fn first_user_text_is_write_once_across_subsequent_captures() {
        use crate::acp::types::PromptInputBlock;
        use crate::auto_title::service::capture_prompt_context;
        use crate::auto_title::types::PromptCaptureContext;

        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-write-once").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");

        let first = PromptCaptureContext::new(Some("first task".into()), Some(AppLocale::En));
        capture_prompt_context(&db.conn, conversation.id, &[], Some(&first), AppLocale::En)
            .await
            .expect("first capture");

        let second = PromptCaptureContext::new(Some("second task".into()), Some(AppLocale::Ja));
        let blocks = vec![PromptInputBlock::Text {
            text: "ignored wire".into(),
        }];
        capture_prompt_context(
            &db.conn,
            conversation.id,
            &blocks,
            Some(&second),
            AppLocale::En,
        )
        .await
        .expect("second capture");

        let job = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(job.first_user_text.as_deref(), Some("first task"));
        assert!(
            job.first_prompt_at.is_some(),
            "first capture must stamp first_prompt_at"
        );
        assert_eq!(
            job.locale.as_deref(),
            Some("ja"),
            "locale still refreshes while first text stays"
        );
    }

    #[tokio::test]
    async fn capture_sets_first_user_and_first_prompt_at_once() {
        use crate::auto_title::service::capture_prompt_context;
        use crate::auto_title::types::PromptCaptureContext;

        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-first-prompt-at-once").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");

        let first = PromptCaptureContext::new(Some("task A".into()), Some(AppLocale::En));
        capture_prompt_context(&db.conn, conversation.id, &[], Some(&first), AppLocale::En)
            .await
            .expect("first capture");

        let after_first = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(after_first.first_user_text.as_deref(), Some("task A"));
        let stamped = after_first
            .first_prompt_at
            .expect("first_prompt_at must be set on first capture");

        let second = PromptCaptureContext::new(Some("task B".into()), Some(AppLocale::ZhCn));
        capture_prompt_context(
            &db.conn,
            conversation.id,
            &[],
            Some(&second),
            AppLocale::En,
        )
        .await
        .expect("second capture");

        let after_second = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(after_second.first_user_text.as_deref(), Some("task A"));
        assert_eq!(
            after_second.first_prompt_at,
            Some(stamped),
            "first_prompt_at is write-once across subsequent captures"
        );
        assert_eq!(
            after_second.locale.as_deref(),
            Some("zh_cn"),
            "locale may still refresh when first fields are already set"
        );
    }

    /// Two independent SQLite connections on one WAL file race the first-fields
    /// CAS. Exactly one writer stamps `first_user_text` + `first_prompt_at`.
    #[tokio::test]
    async fn concurrent_captures_only_one_writes_first_fields() {
        use std::sync::Arc;
        use std::time::Duration;

        use sea_orm::{ConnectOptions, Database, DbBackend, Statement};
        use tokio::sync::Barrier;

        use crate::auto_title::service::capture_prompt_context;
        use crate::auto_title::types::PromptCaptureContext;
        use crate::db::test_helpers::fresh_disk_db;

        let dir = tempfile::tempdir().expect("tempdir");
        // Migrate once; reopen as two separate pools on the same WAL file.
        let migrate = fresh_disk_db(dir.path()).await;
        enable_auto_title(&migrate.conn, AgentType::Codex).await;
        let folder = seed_folder(&migrate, "/tmp/title-concurrent-capture").await;
        let conversation = create(&migrate.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let conversation_id = conversation.id;
        // Release the migrator pool so WAL writers are just the two racers.
        migrate.conn.close().await.expect("close migrate pool");

        let path = dir.path().join("source.db");
        async fn open_wal_pool(path: &std::path::Path) -> crate::db::AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal pool");
            for pragma in [
                "PRAGMA journal_mode=WAL;",
                "PRAGMA busy_timeout=5000;",
                "PRAGMA foreign_keys=ON;",
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                    .await
                    .expect("pragma");
            }
            crate::db::AppDatabase { conn }
        }

        let pool_a = Arc::new(open_wal_pool(&path).await);
        let pool_b = Arc::new(open_wal_pool(&path).await);
        let barrier = Arc::new(Barrier::new(2));

        let barrier_a = barrier.clone();
        let barrier_b = barrier.clone();
        let db_a = pool_a.clone();
        let db_b = pool_b.clone();

        let (res_a, res_b) = tokio::join!(
            async move {
                // Barrier immediately before first-fields UPDATE (via capture).
                barrier_a.wait().await;
                let capture =
                    PromptCaptureContext::new(Some("task A".into()), Some(AppLocale::En));
                capture_prompt_context(
                    &db_a.conn,
                    conversation_id,
                    &[],
                    Some(&capture),
                    AppLocale::En,
                )
                .await
            },
            async move {
                barrier_b.wait().await;
                let capture =
                    PromptCaptureContext::new(Some("task B".into()), Some(AppLocale::Ja));
                capture_prompt_context(
                    &db_b.conn,
                    conversation_id,
                    &[],
                    Some(&capture),
                    AppLocale::En,
                )
                .await
            },
        );
        res_a.expect("capture A");
        res_b.expect("capture B");

        let check = open_wal_pool(&path).await;
        let job = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&check.conn)
            .await
            .unwrap()
            .expect("job");

        let first = job
            .first_user_text
            .as_deref()
            .expect("exactly one writer must set first_user_text");
        assert!(
            first == "task A" || first == "task B",
            "first_user_text must equal a winner visible text, got {first:?}"
        );
        assert!(
            job.first_prompt_at.is_some(),
            "first_prompt_at must be set exactly once by the winning writer"
        );
        // Loser always refreshes locale after losing the first-fields CAS, so
        // the durable locale is the non-winner's wire value.
        let expected_locale = if first == "task A" { "ja" } else { "en" };
        assert_eq!(
            job.locale.as_deref(),
            Some(expected_locale),
            "losing concurrent capture may only refresh locale"
        );

        drop(dir);
    }

    #[tokio::test]
    async fn locale_refreshes_for_every_surviving_job_state() {
        use crate::auto_title::service::capture_prompt_context;
        use crate::auto_title::types::PromptCaptureContext;

        let db = fresh_in_memory_db().await;
        // Leave auto-title Off so create() does not enroll; seed precise states.
        let folder = seed_folder(&db, "/tmp/title-locale-refresh").await;

        let states = [
            AutoTitleJobState::AwaitingTurn,
            AutoTitleJobState::Ready,
            AutoTitleJobState::Running,
            AutoTitleJobState::RetryWait,
        ];

        for (idx, state) in states.into_iter().enumerate() {
            let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("create");
            assert!(
                auto_title_job::Entity::find_by_id(conversation.id)
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .is_none(),
                "Off setting must not enroll"
            );

            seed_job_in_state(
                &db.conn,
                conversation.id,
                state.clone(),
                Some("original"),
                Some("en"),
            )
            .await;

            let capture = PromptCaptureContext::new(Some("later".into()), Some(AppLocale::ZhTw));
            capture_prompt_context(
                &db.conn,
                conversation.id,
                &[],
                Some(&capture),
                AppLocale::En,
            )
            .await
            .expect("capture");

            let job = auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .expect("job");
            assert_eq!(
                job.first_user_text.as_deref(),
                Some("original"),
                "state {state:?} idx {idx}: first text write-once"
            );
            assert_eq!(
                job.locale.as_deref(),
                Some("zh_tw"),
                "state {state:?} idx {idx}: locale must refresh"
            );
            assert_eq!(job.state, state);
        }
    }

    struct AwaitingJobFixture {
        db: crate::db::AppDatabase,
        conversation_id: i32,
    }

    async fn awaiting_job_fixture() -> AwaitingJobFixture {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-awaiting-job").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let job = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("enrolled job");
        assert_eq!(job.state, AutoTitleJobState::AwaitingTurn);
        AwaitingJobFixture {
            db,
            conversation_id: conversation.id,
        }
    }

    impl AwaitingJobFixture {
        fn snapshot(&self, token: &str, answer: &str) -> TurnCompletionSnapshot {
            TurnCompletionSnapshot {
                conversation_id: self.conversation_id,
                turn_token: token.to_string(),
                locale: AppLocale::En,
                final_text: Arc::from(answer),
            }
        }

        async fn apply_completion(
            &self,
            snapshot: &TurnCompletionSnapshot,
        ) -> CompletionTransition {
            let txn = self.db.conn.begin().await.expect("begin");
            let result = apply_usable_completion(&txn, snapshot, "end_turn")
                .await
                .expect("apply");
            txn.commit().await.expect("commit");
            result
        }

        async fn apply_completion_with_reason(
            &self,
            snapshot: &TurnCompletionSnapshot,
            stop_reason: &str,
        ) -> CompletionTransition {
            let txn = self.db.conn.begin().await.expect("begin");
            let result = apply_usable_completion(&txn, snapshot, stop_reason)
                .await
                .expect("apply");
            txn.commit().await.expect("commit");
            result
        }

        async fn job(&self) -> auto_title_job::Model {
            auto_title_job::Entity::find_by_id(self.conversation_id)
                .one(&self.db.conn)
                .await
                .unwrap()
                .expect("job")
        }
    }

    #[tokio::test]
    async fn duplicate_turn_token_changes_the_job_once() {
        let fixture = awaiting_job_fixture().await;
        let snapshot = fixture.snapshot("same-token", "answer");
        let first = fixture.apply_completion(&snapshot).await;
        let second = fixture.apply_completion(&snapshot).await;
        assert_eq!(first.usable_turn_seq, 1);
        assert_eq!(second.usable_turn_seq, 1);
        assert!(!second.became_ready);
        assert!(first.became_ready);

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::Ready);
        assert_eq!(job.usable_turn_seq, 1);
        assert_eq!(job.last_usable_turn_token.as_deref(), Some("same-token"));
        assert_eq!(job.first_assistant_text.as_deref(), Some("answer"));
    }

    #[tokio::test]
    async fn abnormal_and_empty_completions_leave_job_awaiting() {
        let fixture = awaiting_job_fixture().await;

        let refusal = fixture.snapshot("tok-refusal", "I refuse");
        let r = fixture
            .apply_completion_with_reason(&refusal, "refusal")
            .await;
        assert_eq!(r.usable_turn_seq, 0);
        assert!(!r.became_ready);

        let empty = fixture.snapshot("tok-empty", "   ");
        let e = fixture.apply_completion(&empty).await;
        assert_eq!(e.usable_turn_seq, 0);
        assert!(!e.became_ready);

        let cancelled = fixture.snapshot("tok-cancel", "partial");
        let c = fixture
            .apply_completion_with_reason(&cancelled, "cancelled")
            .await;
        assert_eq!(c.usable_turn_seq, 0);
        assert!(!c.became_ready);

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::AwaitingTurn);
        assert_eq!(job.usable_turn_seq, 0);
        assert!(job.first_assistant_text.is_none());
        assert!(job.last_usable_turn_token.is_none());
    }

    /// Deadline promote CAS used by completion races (Task 6 owns the real
    /// sweep; tests mirror the required write-once predicates).
    async fn simulate_deadline_promote<C: ConnectionTrait>(
        conn: &C,
        conversation_id: i32,
        partial: &str,
    ) -> u64 {
        // Test-only gate immediately before promote CAS (deadline vs end-turn
        // barrier races). No-op outside first_ready_race_hooks::scope_promote.
        first_ready_race_hooks::run_promote_pre_cas_hook().await;

        let result = auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::Ready),
            )
            .col_expr(
                auto_title_job::Column::FirstAssistantText,
                Expr::value(partial.to_string()),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::AwaitingTurn))
            .filter(auto_title_job::Column::FirstAssistantText.is_null())
            .exec(conn)
            .await
            .expect("simulate deadline promote");
        result.rows_affected
    }

    #[tokio::test]
    async fn end_turn_from_awaiting_sets_assistant_and_ready() {
        let fixture = awaiting_job_fixture().await;
        let snapshot = fixture.snapshot("tok-first", "full final answer");
        let transition = fixture.apply_completion(&snapshot).await;

        assert!(transition.became_ready);
        assert_eq!(transition.usable_turn_seq, 1);

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::Ready);
        assert_eq!(job.usable_turn_seq, 1);
        assert_eq!(job.last_usable_turn_token.as_deref(), Some("tok-first"));
        assert_eq!(
            job.first_assistant_text.as_deref(),
            Some("full final answer")
        );
        assert_eq!(job.locale.as_deref(), Some("en"));
    }

    #[tokio::test]
    async fn end_turn_does_not_overwrite_deadline_assistant_snapshot() {
        let fixture = awaiting_job_fixture().await;
        // Deadline first: write-once Some("partial") into ready.
        let promoted = simulate_deadline_promote(
            &fixture.db.conn,
            fixture.conversation_id,
            "partial",
        )
        .await;
        assert_eq!(promoted, 1, "deadline promote must win first-ready");

        let job_after_deadline = fixture.job().await;
        assert_eq!(job_after_deadline.state, AutoTitleJobState::Ready);
        assert_eq!(
            job_after_deadline.first_assistant_text.as_deref(),
            Some("partial")
        );
        assert_eq!(job_after_deadline.usable_turn_seq, 0);

        // Later usable completion with different final text must advance seq
        // and locale/token, but must not refine the deadline snapshot.
        let snapshot = fixture.snapshot("tok-end", "full final that must not win");
        let transition = fixture.apply_completion(&snapshot).await;
        assert!(!transition.became_ready, "already Ready after deadline");
        assert_eq!(transition.usable_turn_seq, 1);

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::Ready);
        assert_eq!(job.usable_turn_seq, 1);
        assert_eq!(job.last_usable_turn_token.as_deref(), Some("tok-end"));
        assert_eq!(
            job.first_assistant_text.as_deref(),
            Some("partial"),
            "end-turn must not overwrite deadline assistant snapshot"
        );

        // Same rule for deadline empty partial Some("").
        let fixture_empty = awaiting_job_fixture().await;
        assert_eq!(
            simulate_deadline_promote(
                &fixture_empty.db.conn,
                fixture_empty.conversation_id,
                "",
            )
            .await,
            1
        );
        let empty_snap = fixture_empty.snapshot("tok-after-empty", "later full text");
        let t = fixture_empty.apply_completion(&empty_snap).await;
        assert_eq!(t.usable_turn_seq, 1);
        let job_empty = fixture_empty.job().await;
        assert_eq!(
            job_empty.first_assistant_text.as_deref(),
            Some(""),
            "Some(\"\") deadline snapshot is also write-once"
        );
    }

    #[tokio::test]
    async fn retry_wait_becomes_ready_without_replacing_assistant() {
        let fixture = awaiting_job_fixture().await;
        // Seed first snapshot + retry_wait (attempt-1 failure path).
        let now = Utc::now();
        auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::RetryWait),
            )
            .col_expr(
                auto_title_job::Column::FirstAssistantText,
                Expr::value("snap".to_string()),
            )
            .col_expr(
                auto_title_job::Column::FirstUserText,
                Expr::value("task".to_string()),
            )
            .col_expr(auto_title_job::Column::UsableTurnSeq, Expr::value(1))
            .col_expr(
                auto_title_job::Column::LastUsableTurnToken,
                Expr::value("tok-1".to_string()),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
            .filter(auto_title_job::Column::ConversationId.eq(fixture.conversation_id))
            .exec(&fixture.db.conn)
            .await
            .expect("seed retry_wait");

        let snapshot = fixture.snapshot("tok-2", "later turn text must not replace snap");
        let transition = fixture.apply_completion(&snapshot).await;
        assert!(transition.became_ready);
        assert_eq!(transition.usable_turn_seq, 2);

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::Ready);
        assert_eq!(job.usable_turn_seq, 2);
        assert_eq!(job.last_usable_turn_token.as_deref(), Some("tok-2"));
        assert_eq!(
            job.first_assistant_text.as_deref(),
            Some("snap"),
            "retry_wait → ready must not replace first_assistant_text"
        );
    }

    /// REQUIRED: two concurrent completions with distinct tokens on the same
    /// Ready job must advance `usable_turn_seq` by +2 via atomic SQL increment
    /// (not a lost concurrent current_seq+1 RMW).
    #[tokio::test]
    async fn two_distinct_usable_tokens_advance_seq_twice() {
        use std::sync::Arc;
        use std::time::Duration;

        use sea_orm::{ConnectOptions, Database, DbBackend, Statement};

        use crate::db::test_helpers::fresh_disk_db;

        let dir = tempfile::tempdir().expect("tempdir");
        let migrate = fresh_disk_db(dir.path()).await;
        enable_auto_title(&migrate.conn, AgentType::Codex).await;
        let folder = seed_folder(&migrate, "/tmp/title-dual-token-seq").await;
        let conversation = create(&migrate.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let conversation_id = conversation.id;
        let _ = auto_title_job::Entity::delete_by_id(conversation_id)
            .exec(&migrate.conn)
            .await;
        // Already Ready after deadline: seq=1, assistant write-once set.
        seed_ready_claim_job(
            &migrate.conn,
            conversation_id,
            Some("user task"),
            Some("deadline snap"),
            1,
        )
        .await;
        migrate.conn.close().await.expect("close migrate pool");

        let path = dir.path().join("source.db");
        async fn open_wal_pool(path: &std::path::Path) -> crate::db::AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal pool");
            for pragma in [
                "PRAGMA journal_mode=WAL;",
                "PRAGMA busy_timeout=5000;",
                "PRAGMA foreign_keys=ON;",
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                    .await
                    .expect("pragma");
            }
            crate::db::AppDatabase { conn }
        }

        let pool_a = Arc::new(open_wal_pool(&path).await);
        let pool_b = Arc::new(open_wal_pool(&path).await);

        let snap_a = TurnCompletionSnapshot {
            conversation_id,
            turn_token: "tok-a".into(),
            locale: AppLocale::En,
            final_text: Arc::from("later turn a"),
        };
        let snap_b = TurnCompletionSnapshot {
            conversation_id,
            turn_token: "tok-b".into(),
            locale: AppLocale::ZhCn,
            final_text: Arc::from("later turn b"),
        };

        let handle_a = tokio::spawn({
            let pool = pool_a.clone();
            let snap = snap_a.clone();
            async move {
                let txn = pool.conn.begin().await.expect("begin a");
                let result = apply_usable_completion(&txn, &snap, "end_turn")
                    .await
                    .expect("apply a");
                txn.commit().await.expect("commit a");
                result
            }
        });
        let handle_b = tokio::spawn({
            let pool = pool_b.clone();
            let snap = snap_b.clone();
            async move {
                let txn = pool.conn.begin().await.expect("begin b");
                let result = apply_usable_completion(&txn, &snap, "end_turn")
                    .await
                    .expect("apply b");
                txn.commit().await.expect("commit b");
                result
            }
        });

        let (ta, tb) = tokio::join!(handle_a, handle_b);
        let ta = ta.expect("join a");
        let tb = tb.expect("join b");
        // Each completion reports the seq it observed after its progress write;
        // together they must cover +2 from the seeded seq=1.
        let reported: std::collections::HashSet<i32> =
            [ta.usable_turn_seq, tb.usable_turn_seq].into_iter().collect();
        assert_eq!(
            reported,
            [2, 3].into_iter().collect(),
            "concurrent distinct tokens must each advance seq once (got {ta:?} / {tb:?})"
        );
        assert!(!ta.became_ready);
        assert!(!tb.became_ready);

        let job = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&pool_a.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(
            job.usable_turn_seq, 3,
            "usable_turn_seq must become +2, not +1 from lost concurrent RMW"
        );
        assert_eq!(
            job.first_assistant_text.as_deref(),
            Some("deadline snap"),
            "progress must not refine first assistant"
        );
        assert!(
            job.last_usable_turn_token.as_deref() == Some("tok-a")
                || job.last_usable_turn_token.as_deref() == Some("tok-b"),
            "last token must be one of the two concurrent tokens"
        );

        drop(dir);
    }

    /// REQUIRED WAL + barriers: deadline promote vs end-turn in BOTH orders.
    ///
    /// Sequential pre-seed is not enough — each order parks one side at a
    /// task-local pre-write / pre-CAS gate (claim-style) so the other commits
    /// first-assistant, then releases. Exactly one first-assistant snapshot;
    /// job ends Ready without panic or double first-ready corruption.
    #[tokio::test]
    async fn concurrent_end_turn_and_deadline_both_orders_wal() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        use sea_orm::{ConnectOptions, Database, DbBackend, Statement};
        use tokio::sync::Notify;

        use crate::db::test_helpers::fresh_disk_db;

        const BARRIER_SEQ_TIMEOUT: Duration = Duration::from_secs(5);
        const GATE_STEP_TIMEOUT: Duration = Duration::from_secs(2);

        let dir = tempfile::tempdir().expect("tempdir");
        let migrate = fresh_disk_db(dir.path()).await;
        enable_auto_title(&migrate.conn, AgentType::Codex).await;
        let folder = seed_folder(&migrate, "/tmp/title-deadline-vs-endturn").await;

        async fn open_wal_pool(path: &std::path::Path) -> crate::db::AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal pool");
            for pragma in [
                "PRAGMA journal_mode=WAL;",
                "PRAGMA busy_timeout=5000;",
                "PRAGMA foreign_keys=ON;",
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                    .await
                    .expect("pragma");
            }
            crate::db::AppDatabase { conn }
        }

        // Two conversations: one per barrier order (shared WAL file).
        let mut conversation_ids = Vec::new();
        for _ in 0..2 {
            let conversation = create(&migrate.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("create");
            let job = auto_title_job::Entity::find_by_id(conversation.id)
                .one(&migrate.conn)
                .await
                .unwrap()
                .expect("enrolled");
            assert_eq!(job.state, AutoTitleJobState::AwaitingTurn);
            assert!(job.first_assistant_text.is_none());
            conversation_ids.push(conversation.id);
        }
        migrate.conn.close().await.expect("close migrate");

        let path = dir.path().join("source.db");
        let pool_end = Arc::new(open_wal_pool(&path).await);
        let pool_deadline = Arc::new(open_wal_pool(&path).await);

        // ------------------------------------------------------------------
        // Order A: completion parks pre-write; promote commits first-assistant;
        // then completion CAS loses write-once and only advances seq.
        // ------------------------------------------------------------------
        {
            let cid = conversation_ids[0];
            let after_completion_at_gate = Arc::new(Notify::new());
            let allow_completion = Arc::new(Notify::new());
            let gate_armed = Arc::new(AtomicBool::new(true));

            let end_pool = pool_end.clone();
            let after_gate = after_completion_at_gate.clone();
            let allow = allow_completion.clone();
            let armed = gate_armed.clone();
            let end_handle = tokio::spawn(async move {
                first_ready_race_hooks::scope_completion(
                    Arc::new(move || {
                        let after_gate = after_gate.clone();
                        let allow = allow.clone();
                        let armed = armed.clone();
                        Box::pin(async move {
                            if !armed.swap(false, Ordering::SeqCst) {
                                return;
                            }
                            after_gate.notify_one();
                            tokio::time::timeout(GATE_STEP_TIMEOUT, allow.notified())
                                .await
                                .expect("completion pre-write gate must be released");
                        })
                    }),
                    async move {
                        let snap = TurnCompletionSnapshot {
                            conversation_id: cid,
                            turn_token: "tok-order-a".into(),
                            locale: AppLocale::En,
                            final_text: Arc::from("full-a must not win"),
                        };
                        let txn = end_pool.conn.begin().await.expect("begin order a");
                        let result = apply_usable_completion(&txn, &snap, "end_turn")
                            .await
                            .expect("apply order a");
                        txn.commit().await.expect("commit order a");
                        result
                    },
                )
                .await
            });

            // Completion reached pre-write gate (no open write txn yet).
            tokio::time::timeout(GATE_STEP_TIMEOUT, after_completion_at_gate.notified())
                .await
                .expect("completion must reach pre-write gate before barrier timeout");

            // Promote commits first-assistant while completion is parked.
            let promoted =
                simulate_deadline_promote(&pool_deadline.conn, cid, "partial-a").await;
            assert_eq!(promoted, 1, "deadline promote must win first-ready in order A");

            allow_completion.notify_one();

            let transition = tokio::time::timeout(BARRIER_SEQ_TIMEOUT, end_handle)
                .await
                .expect("order A completion must not hang past barrier sequence")
                .expect("join order A completion");
            assert_eq!(transition.usable_turn_seq, 1);
            assert!(
                !transition.became_ready,
                "end-turn must not re-win first-ready after promote"
            );

            let job = auto_title_job::Entity::find_by_id(cid)
                .one(&pool_end.conn)
                .await
                .unwrap()
                .expect("job order a");
            assert_eq!(job.state, AutoTitleJobState::Ready);
            assert_eq!(
                job.first_assistant_text.as_deref(),
                Some("partial-a"),
                "promote first-assistant must win order A"
            );
            assert_eq!(job.usable_turn_seq, 1);
        }

        // ------------------------------------------------------------------
        // Order B: promote parks pre-CAS; completion commits first-assistant;
        // then promote CAS no-ops (rows=0).
        // ------------------------------------------------------------------
        {
            let cid = conversation_ids[1];
            let after_promote_at_gate = Arc::new(Notify::new());
            let allow_promote = Arc::new(Notify::new());
            let gate_armed = Arc::new(AtomicBool::new(true));

            let deadline_pool = pool_deadline.clone();
            let after_gate = after_promote_at_gate.clone();
            let allow = allow_promote.clone();
            let armed = gate_armed.clone();
            let promote_handle = tokio::spawn(async move {
                first_ready_race_hooks::scope_promote(
                    Arc::new(move || {
                        let after_gate = after_gate.clone();
                        let allow = allow.clone();
                        let armed = armed.clone();
                        Box::pin(async move {
                            if !armed.swap(false, Ordering::SeqCst) {
                                return;
                            }
                            after_gate.notify_one();
                            tokio::time::timeout(GATE_STEP_TIMEOUT, allow.notified())
                                .await
                                .expect("promote pre-CAS gate must be released");
                        })
                    }),
                    async move {
                        simulate_deadline_promote(
                            &deadline_pool.conn,
                            cid,
                            "partial-b-must-lose",
                        )
                        .await
                    },
                )
                .await
            });

            // Promote reached pre-CAS gate (no promote write yet).
            tokio::time::timeout(GATE_STEP_TIMEOUT, after_promote_at_gate.notified())
                .await
                .expect("promote must reach pre-CAS gate before barrier timeout");

            // Completion commits first-assistant while promote is parked.
            let snap = TurnCompletionSnapshot {
                conversation_id: cid,
                turn_token: "tok-order-b".into(),
                locale: AppLocale::En,
                final_text: Arc::from("full-b wins first"),
            };
            let txn = pool_end.conn.begin().await.expect("begin order b");
            let transition = apply_usable_completion(&txn, &snap, "end_turn")
                .await
                .expect("apply order b");
            txn.commit().await.expect("commit order b");
            assert!(transition.became_ready);
            assert_eq!(transition.usable_turn_seq, 1);

            allow_promote.notify_one();

            let promoted = tokio::time::timeout(BARRIER_SEQ_TIMEOUT, promote_handle)
                .await
                .expect("order B promote must not hang past barrier sequence")
                .expect("join order B promote");
            assert_eq!(
                promoted, 0,
                "deadline must lose after end-turn first-ready in order B"
            );

            let job = auto_title_job::Entity::find_by_id(cid)
                .one(&pool_deadline.conn)
                .await
                .unwrap()
                .expect("job order b");
            assert_eq!(job.state, AutoTitleJobState::Ready);
            assert_eq!(
                job.first_assistant_text.as_deref(),
                Some("full-b wins first"),
                "completion first-assistant must win order B"
            );
            assert_eq!(job.usable_turn_seq, 1);
        }

        drop(dir);
    }

    async fn seed_ready_claim_job(
        conn: &DatabaseConnection,
        conversation_id: i32,
        first_user_text: Option<&str>,
        first_assistant_text: Option<&str>,
        usable_turn_seq: i32,
    ) {
        let now = Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(AutoTitleJobState::Ready),
            attempts: Set(0),
            first_user_text: Set(first_user_text.map(|s| s.to_string())),
            first_assistant_text: Set(first_assistant_text.map(|s| s.to_string())),
            first_prompt_at: Set(None),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(usable_turn_seq),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(Some(format!("tok-{usable_turn_seq}"))),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed ready claim job");
    }

    #[tokio::test]
    async fn claim_accepts_empty_assistant_some_empty_string() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-claim-empty-assistant").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        // create() may enroll awaiting_turn; replace with precise Ready row.
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user task"),
            Some(""),
            1,
        )
        .await;

        let claim = claim_next_ready(&db.conn)
            .await
            .expect("claim")
            .expect("Ready + Some(\"\") must be claimable");

        assert_eq!(claim.conversation_id, conversation.id);
        assert_eq!(claim.first_user_text, "user task");
        assert_eq!(claim.first_assistant_text, "");
        assert_eq!(claim.attempt, 1);
        assert_eq!(claim.attempt_turn_seq, 1);
        assert_eq!(claim.agent, AgentType::Codex);

        let job = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("running job");
        assert_eq!(job.state, AutoTitleJobState::Running);
        assert_eq!(job.attempts, 1);
        assert_eq!(job.attempt_turn_seq, 1);
        assert_eq!(job.first_assistant_text.as_deref(), Some(""));
    }

    #[tokio::test]
    async fn claim_deletes_ready_with_none_assistant() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-claim-none-assistant").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(&db.conn, conversation.id, Some("user task"), None, 1).await;

        let claim = claim_next_ready(&db.conn).await.expect("claim");
        assert!(
            claim.is_none(),
            "Ready + None assistant must not produce a claim"
        );
        assert!(
            auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "invalid Ready row with None assistant must be deleted"
        );
    }

    #[tokio::test]
    async fn claim_still_deletes_empty_user() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-claim-empty-user").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("   "),
            Some("assistant"),
            1,
        )
        .await;

        let claim = claim_next_ready(&db.conn).await.expect("claim");
        assert!(claim.is_none(), "empty trimmed user must not claim");
        assert!(
            auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "empty-user Ready row must be deleted"
        );
    }

    /// REQUIRED barrier: claim reads Ready with seq=1; a concurrent connection
    /// advances `usable_turn_seq` to 2 before CAS; the claim must not hang and
    /// must return `attempt_turn_seq` matching the row actually claimed.
    #[tokio::test]
    async fn claim_retries_after_usable_turn_seq_changes_between_read_and_cas() {
        use std::sync::Arc;
        use std::time::Duration;

        use sea_orm::{ConnectOptions, Database, DbBackend, Statement};
        use tokio::sync::Notify;

        use crate::db::test_helpers::fresh_disk_db;

        /// Bound for the whole select→advance→CAS handshake so a stuck barrier
        /// cannot hang the suite indefinitely.
        const BARRIER_SEQ_TIMEOUT: Duration = Duration::from_secs(5);
        const GATE_STEP_TIMEOUT: Duration = Duration::from_secs(2);

        let dir = tempfile::tempdir().expect("tempdir");
        let migrate = fresh_disk_db(dir.path()).await;
        enable_auto_title(&migrate.conn, AgentType::Codex).await;
        let folder = seed_folder(&migrate, "/tmp/title-claim-seq-race").await;
        let conversation = create(&migrate.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let conversation_id = conversation.id;
        let _ = auto_title_job::Entity::delete_by_id(conversation_id)
            .exec(&migrate.conn)
            .await;
        seed_ready_claim_job(
            &migrate.conn,
            conversation_id,
            Some("user task"),
            Some("assistant reply"),
            1,
        )
        .await;
        migrate.conn.close().await.expect("close migrate pool");

        let path = dir.path().join("source.db");
        async fn open_wal_pool(path: &std::path::Path) -> crate::db::AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal pool");
            for pragma in [
                "PRAGMA journal_mode=WAL;",
                "PRAGMA busy_timeout=5000;",
                "PRAGMA foreign_keys=ON;",
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                    .await
                    .expect("pragma");
            }
            crate::db::AppDatabase { conn }
        }

        let claim_db = Arc::new(open_wal_pool(&path).await);
        let advance_db = open_wal_pool(&path).await;

        let after_read = Arc::new(Notify::new());
        let allow_cas = Arc::new(Notify::new());
        // One-shot gate: only the first select→CAS path is paused so a concurrent
        // writer can advance usable_turn_seq. Retries after lost/snapshot CAS
        // must not re-block on the same notifies.
        let gate_armed = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let claim_conn = claim_db.clone();
        // Task-local hook: only this claim task sees the barrier (parallel tests
        // cannot steal a process-global slot).
        let claim_handle = tokio::spawn({
            let after_read = after_read.clone();
            let allow_cas = allow_cas.clone();
            let gate_armed = gate_armed.clone();
            async move {
                claim_test_hooks::scope(
                    Arc::new(move || {
                        let after_read = after_read.clone();
                        let allow_cas = allow_cas.clone();
                        let gate_armed = gate_armed.clone();
                        Box::pin(async move {
                            if !gate_armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                                return;
                            }
                            after_read.notify_one();
                            // Bound the wait for the test to release CAS so a
                            // dropped harness cannot leave claim parked forever.
                            tokio::time::timeout(GATE_STEP_TIMEOUT, allow_cas.notified())
                                .await
                                .expect("pre-CAS gate must be released before timeout");
                        })
                    }),
                    claim_next_ready(&claim_conn.conn),
                )
                .await
            }
        });

        // Wait until claim has selected the Ready candidate (seq=1).
        tokio::time::timeout(GATE_STEP_TIMEOUT, after_read.notified())
            .await
            .expect("claim must reach pre-CAS gate before barrier timeout");

        // Concurrent usable-turn progress while still Ready.
        auto_title_job::Entity::update_many()
            .col_expr(auto_title_job::Column::UsableTurnSeq, Expr::value(2))
            .col_expr(
                auto_title_job::Column::LastUsableTurnToken,
                Expr::value("tok-2"),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .exec(&advance_db.conn)
            .await
            .expect("advance usable_turn_seq");

        allow_cas.notify_one();

        let claim_result = tokio::time::timeout(BARRIER_SEQ_TIMEOUT, claim_handle).await;
        let claim = claim_result
            .expect("claim must not hang past barrier sequence timeout")
            .expect("join claim task")
            .expect("claim result")
            .expect("must claim Ready job after seq race");

        let job = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&advance_db.conn)
            .await
            .unwrap()
            .expect("claimed job");

        assert_eq!(job.state, AutoTitleJobState::Running);
        assert_eq!(job.usable_turn_seq, 2);
        assert_eq!(
            claim.attempt_turn_seq, job.attempt_turn_seq,
            "claim snapshot must match durable attempt_turn_seq"
        );
        assert_eq!(
            claim.attempt_turn_seq, job.usable_turn_seq,
            "attempt_turn_seq must track usable_turn_seq at CAS, not the stale read"
        );
        assert_eq!(claim.attempt_turn_seq, 2);
        assert_eq!(claim.attempt, job.attempts);
        assert_eq!(claim.conversation_id, conversation_id);
        assert_eq!(claim.first_assistant_text, "assistant reply");

        drop(dir);
    }
}
