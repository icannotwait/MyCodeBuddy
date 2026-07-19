//! Enrollment, job cancellation, generated-title finalization, prompt capture,
//! and durable usable-completion transitions.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, Order, QueryFilter, QueryOrder, Set, TransactionTrait,
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
/// turn tokens are idempotent. Moves `awaiting_turn` / `retry_wait` → `ready`
/// and writes write-once `first_assistant_text` through [`bound_context`].
pub async fn apply_usable_completion(
    txn: &DatabaseTransaction,
    snapshot: &TurnCompletionSnapshot,
    stop_reason: &str,
) -> Result<CompletionTransition, DbError> {
    let job = auto_title_job::Entity::find_by_id(snapshot.conversation_id)
        .one(txn)
        .await?;

    let Some(job) = job else {
        return Ok(CompletionTransition {
            usable_turn_seq: 0,
            became_ready: false,
        });
    };

    let current_seq = job.usable_turn_seq;

    if stop_reason != "end_turn" || snapshot.final_text.trim().is_empty() {
        return Ok(CompletionTransition {
            usable_turn_seq: current_seq,
            became_ready: false,
        });
    }

    if job.last_usable_turn_token.as_deref() == Some(snapshot.turn_token.as_str()) {
        return Ok(CompletionTransition {
            usable_turn_seq: current_seq,
            became_ready: false,
        });
    }

    let bounded = bound_context(snapshot.final_text.trim());
    let new_seq = current_seq + 1;
    let became_ready = matches!(
        job.state,
        AutoTitleJobState::AwaitingTurn | AutoTitleJobState::RetryWait
    );

    let mut active: auto_title_job::ActiveModel = job.clone().into();
    active.usable_turn_seq = Set(new_seq);
    active.last_usable_turn_token = Set(Some(snapshot.turn_token.clone()));
    if job.first_assistant_text.is_none() {
        active.first_assistant_text = Set(Some(bounded));
    }
    active.locale = Set(Some(app_locale_to_wire(snapshot.locale).to_string()));
    if became_ready {
        active.state = Set(AutoTitleJobState::Ready);
    }
    active.updated_at = Set(Utc::now());
    active.update(txn).await?;

    Ok(CompletionTransition {
        usable_turn_seq: new_seq,
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
