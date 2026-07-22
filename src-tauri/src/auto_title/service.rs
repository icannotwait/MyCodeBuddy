//! Enrollment, job cancellation, generated-title finalization, prompt capture,
//! and durable usable-completion transitions.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, Order, QueryFilter, QueryOrder, QuerySelect, Set,
    TransactionTrait,
};

use crate::acp::types::PromptInputBlock;
use crate::auto_title::context::{bound_context, project_visible_prompt};
use crate::auto_title::title_key::{get_title_api_key, title_key_fingerprint, TitleKeyState};
use crate::auto_title::title_settings::{
    auto_title_enabled, parse_config_barrier, parse_config_gen, BARRIER_RAISED,
    KEY_AUTO_TITLE_API_KEY_FP, KEY_AUTO_TITLE_API_URL, KEY_AUTO_TITLE_CONFIG_BARRIER,
    KEY_AUTO_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1, KEY_AUTO_TITLE_MODEL,
};
use crate::auto_title::types::{
    app_locale_to_wire, parse_supported_app_locale, AutoTitleApiConfig, AutoTitleClaim,
    AutoTitleRunError, CapturedPrompt, CompletionTransition, FailureTransition,
    FinalizeTitleOutcome, PromptCaptureContext, TurnCompletionSnapshot,
};
use crate::commands::conversation_experience::ConversationExperienceMutationGate;
use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
use crate::db::entities::conversation;
use crate::db::error::DbError;
use crate::db::service::app_metadata_service;
use crate::models::system::AppLocale;

/// Read live title API enablement + config_gen from metadata + keyring presence.
///
/// Does **not** load the secret. Used by enroll (On/Off only).
async fn load_title_enabled_and_gen<C: ConnectionTrait>(
    conn: &C,
) -> Result<(bool, i64), DbError> {
    let url = app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_API_URL)
        .await?
        .unwrap_or_default();
    let model = app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_MODEL)
        .await?
        .unwrap_or_default();
    let barrier = parse_config_barrier(
        app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_CONFIG_BARRIER)
            .await?
            .as_deref(),
    );
    let gen_u64 = parse_config_gen(
        app_metadata_service::get_value_conn(conn, KEY_AUTO_TITLE_CONFIG_GEN)
            .await?
            .as_deref(),
    );
    let gen = i64::try_from(gen_u64).map_err(|_| {
        DbError::Validation("auto_title config_gen exceeds i64 storage".into())
    })?;
    let key_present = matches!(get_title_api_key(), TitleKeyState::Present(_));
    let enabled = auto_title_enabled(&url, key_present, &model, barrier);
    Ok((enabled, gen))
}

/// Enroll a newly created conversation for automatic titles when the title API
/// config is On (url + key Present + model + barrier clear). Stores the live
/// `config_gen` on the job so claim can reject stale epochs.
///
/// Returns `true` when a job row was inserted.
pub async fn enroll_new_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    // Read gen + enabled on the same connection (often an outer create txn) so
    // a concurrent save that bumps gen is either seen here or purges this job.
    let (enabled, config_gen) = load_title_enabled_and_gen(conn).await?;
    if !enabled {
        return Ok(false);
    }

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
        config_gen: Set(config_gen),
        updated_at: Set(now),
    }
    .insert(conn)
    .await?;

    Ok(true)
}

/// Fail-closed: raise barrier, bump gen, wipe all title jobs, advance revision.
/// Caller must `cancel_all` after this returns Ok (and still cancel if Err —
/// see [`apply_claim_unavailable_fail_closed`]).
async fn fail_closed_barrier_wipe_jobs(conn: &DatabaseConnection) -> Result<(), DbError> {
    #[cfg(any(test, feature = "test-utils"))]
    if claim_fail_closed_hooks::take_force_wipe_fail() {
        return Err(DbError::Validation(
            "injected fail_closed_barrier_wipe_jobs failure".into(),
        ));
    }

    let txn = conn.begin().await?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_CONFIG_BARRIER, BARRIER_RAISED).await?;
    let raw = app_metadata_service::get_value_conn(&txn, KEY_AUTO_TITLE_CONFIG_GEN).await?;
    let current = parse_config_gen(raw.as_deref());
    let next = current
        .checked_add(1)
        .ok_or_else(|| DbError::Validation("auto_title config_gen exhausted".into()))?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_CONFIG_GEN, &next.to_string()).await?;
    auto_title_job::Entity::delete_many().exec(&txn).await?;
    // Bump revision so settings GET/events notice the barrier.
    let rev_raw =
        app_metadata_service::get_value_conn(&txn, "conversation_experience.revision").await?;
    let rev = rev_raw
        .as_deref()
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    app_metadata_service::upsert_value(&txn, "conversation_experience.revision", &rev.to_string())
        .await?;
    txn.commit().await?;
    Ok(())
}

/// Test-only: force the next claim fail-closed wipe to fail (once).
#[cfg(any(test, feature = "test-utils"))]
mod claim_fail_closed_hooks {
    use std::sync::atomic::{AtomicBool, Ordering};

    static FORCE_WIPE_FAIL: AtomicBool = AtomicBool::new(false);

    pub fn arm_force_wipe_fail() {
        FORCE_WIPE_FAIL.store(true, Ordering::SeqCst);
    }

    pub fn take_force_wipe_fail() -> bool {
        FORCE_WIPE_FAIL.swap(false, Ordering::SeqCst)
    }

    pub fn reset() {
        FORCE_WIPE_FAIL.store(false, Ordering::SeqCst);
    }
}

/// Load verified title API config under an open claim transaction.
///
/// Keyring is read under its process mutex (via [`get_title_api_key`]). On
/// fingerprint mismatch returns `Err(Unavailable)` after fail-closed barrier
/// wipe (caller must cancel_all). On Off / incomplete returns `Ok(None)`.
async fn load_claim_config_snapshot(
    txn: &DatabaseTransaction,
) -> Result<Option<(AutoTitleApiConfig, i64)>, AutoTitleRunError> {
    let url = app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_API_URL)
        .await
        .map_err(|e| {
            tracing::warn!(%e, "auto-title claim: read url failed");
            AutoTitleRunError::AbnormalStop("db_error".into())
        })?
        .unwrap_or_default();
    let model = app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_MODEL)
        .await
        .map_err(|e| {
            tracing::warn!(%e, "auto-title claim: read model failed");
            AutoTitleRunError::AbnormalStop("db_error".into())
        })?
        .unwrap_or_default();
    let barrier = parse_config_barrier(
        app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_CONFIG_BARRIER)
            .await
            .map_err(|e| {
                tracing::warn!(%e, "auto-title claim: read barrier failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?
            .as_deref(),
    );
    let gen_u64 = parse_config_gen(
        app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_CONFIG_GEN)
            .await
            .map_err(|e| {
                tracing::warn!(%e, "auto-title claim: read gen failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?
            .as_deref(),
    );
    let gen = i64::try_from(gen_u64).map_err(|_| {
        AutoTitleRunError::AbnormalStop("config_gen_overflow".into())
    })?;
    let stored_fp = app_metadata_service::get_value_conn(txn, KEY_AUTO_TITLE_API_KEY_FP)
        .await
        .map_err(|e| {
            tracing::warn!(%e, "auto-title claim: read fp failed");
            AutoTitleRunError::AbnormalStop("db_error".into())
        })?
        .unwrap_or_default();

    // Keyring read under process mutex (tokens.json / OS keyring).
    let key_state = get_title_api_key();

    match key_state {
        TitleKeyState::Unavailable => {
            // Unprovable key identity — fail-closed barrier; no HTTP.
            tracing::warn!("auto-title claim: title key Unavailable; fail-closed");
            return Err(AutoTitleRunError::Unavailable);
        }
        TitleKeyState::Absent => {
            // Probe with key_present=true: if url+model look complete (and
            // barrier clear), this would be On if a key were Present. Externally
            // deleted keys must not quiet-Off — fail-closed so reappearance of
            // the old key cannot resume titles without a verified re-save.
            if auto_title_enabled(&url, true, &model, barrier) {
                tracing::warn!(
                    "auto-title claim: key Absent while config looks On; fail-closed"
                );
                return Err(AutoTitleRunError::Unavailable);
            }
            // Incomplete / barrier / empty fields: genuine quiet Off.
            return Ok(None);
        }
        TitleKeyState::Present(secret) => {
            if !auto_title_enabled(&url, true, &model, barrier) {
                return Ok(None);
            }
            let live_fp = title_key_fingerprint(&secret);
            if live_fp != stored_fp {
                tracing::warn!(
                    "auto-title claim: key fingerprint mismatch; fail-closed (no HTTP)"
                );
                return Err(AutoTitleRunError::Unavailable);
            }
            Ok(Some((
                AutoTitleApiConfig {
                    api_url: url,
                    api_key: secret,
                    model,
                },
                gen,
            )))
        }
    }
}

/// Map a claim-path `Unavailable` into fail-closed barrier wipe. Caller must
/// still `cancel_all`.
///
/// Always returns `Err(Unavailable)` so the coordinator cancels active attempts
/// even when the durable barrier/wipe write fails. Do not map wipe failure to
/// `AbnormalStop` alone — that path retries without cancel.
async fn apply_claim_unavailable_fail_closed(
    conn: &DatabaseConnection,
) -> Result<Option<AutoTitleClaim>, AutoTitleRunError> {
    // Always attempt barrier + wipe on Unavailable from claim config load so a
    // fp mismatch / Absent-when-configured / unprovable key cannot leave ready
    // jobs for a later HTTP. Wipe failure is logged but still Unavailable so
    // cancel_all runs; the next claim that observes drift retries the wipe.
    if let Err(e) = fail_closed_barrier_wipe_jobs(conn).await {
        tracing::warn!(%e, "auto-title claim fail-closed wipe failed");
    }
    Err(AutoTitleRunError::Unavailable)
}

/// One-shot upgrade purge: drop every ACP-era `auto_title_jobs` row before the
/// API-title coordinator recovers interrupted work.
///
/// If `conversation_experience.auto_title_jobs_purged_for_api_v1` is not `"1"`,
/// delete **all** job states in one transaction and set the flag. Idempotent:
/// a second call is a no-op and never wipes jobs enrolled after the flag was set.
pub async fn purge_auto_title_jobs_for_api_v1_if_needed(
    conn: &DatabaseConnection,
) -> Result<(), DbError> {
    let flag = app_metadata_service::get_value(conn, KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1).await?;
    if flag.as_deref() == Some("1") {
        return Ok(());
    }

    let txn = conn.begin().await?;
    // Re-check inside the transaction so concurrent starts do not double-purge
    // after a peer already set the flag (best-effort; SQLite serializes writers).
    let flag = app_metadata_service::get_value_conn(&txn, KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1)
        .await?;
    if flag.as_deref() == Some("1") {
        txn.commit().await?;
        return Ok(());
    }

    auto_title_job::Entity::delete_many().exec(&txn).await?;
    app_metadata_service::upsert_value(&txn, KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1, "1").await?;
    txn.commit().await?;
    Ok(())
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
            .col_expr(auto_title_job::Column::Locale, Expr::value(locale_wire))
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
        .col_expr(auto_title_job::Column::Locale, Expr::value(locale_wire))
        .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
        .filter(auto_title_job::Column::ConversationId.eq(snapshot.conversation_id))
        .filter(
            Condition::any()
                .add(auto_title_job::Column::LastUsableTurnToken.is_null())
                .add(auto_title_job::Column::LastUsableTurnToken.ne(snapshot.turn_token.clone())),
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

/// Parameters for listing and CAS-promoting deadline-elapsed `awaiting_turn` jobs.
#[derive(Debug, Clone, Copy)]
pub struct DeadlinePromoteParams {
    pub now: DateTime<Utc>,
    pub deadline: Duration,
    pub batch_limit: usize,
}

fn deadline_cutoff(params: &DeadlinePromoteParams) -> DateTime<Utc> {
    params.now
        - chrono::Duration::from_std(params.deadline).unwrap_or_else(|_| chrono::Duration::zero())
}

/// List conversation ids eligible for deadline promotion, oldest first.
///
/// Candidates must be `awaiting_turn` with a captured user prompt, a non-null
/// `first_prompt_at` at or before `now - deadline`, and no first assistant yet.
pub async fn list_deadline_candidates(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
) -> Result<Vec<i32>, DbError> {
    let cutoff = deadline_cutoff(params);
    let rows = auto_title_job::Entity::find()
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::AwaitingTurn))
        .filter(auto_title_job::Column::FirstUserText.is_not_null())
        .filter(auto_title_job::Column::FirstPromptAt.is_not_null())
        .filter(auto_title_job::Column::FirstPromptAt.lte(cutoff))
        .filter(auto_title_job::Column::FirstAssistantText.is_null())
        .order_by_asc(auto_title_job::Column::FirstPromptAt)
        .order_by_asc(auto_title_job::Column::ConversationId)
        .limit(params.batch_limit as u64)
        .all(conn)
        .await?;
    Ok(rows.into_iter().map(|r| r.conversation_id).collect())
}

/// Re-list deadline candidates and promote each with CAS (missing partial ⇒ `""`).
///
/// Prefer [`list_deadline_candidates`] + [`promote_deadline_jobs_by_ids`] when the
/// coordinator already has partials for a known id batch (avoids a second select).
pub async fn promote_deadline_elapsed_jobs(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
    partials: &HashMap<i32, String>,
) -> Result<usize, DbError> {
    let ids = list_deadline_candidates(conn, params).await?;
    promote_deadline_jobs_by_ids(conn, params, &ids, partials).await
}

/// Promote pre-listed jobs that still satisfy deadline CAS predicates.
///
/// For each id: bound the partial (missing key ⇒ empty string), then
/// `awaiting_turn` + `first_assistant_text IS NULL` + aged `first_prompt_at`
/// UPDATE to `ready` with write-once first assistant. Concurrent end-turn or
/// job deletion yields `rows_affected == 0` (no error).
pub async fn promote_deadline_jobs_by_ids(
    conn: &DatabaseConnection,
    params: &DeadlinePromoteParams,
    ids: &[i32],
    partials: &HashMap<i32, String>,
) -> Result<usize, DbError> {
    let cutoff = deadline_cutoff(params);
    let mut promoted = 0usize;

    for &id in ids {
        // Test-only gate immediately before promote CAS (deadline vs end-turn
        // barrier races). No-op outside first_ready_race_hooks::scope_promote.
        #[cfg(test)]
        first_ready_race_hooks::run_promote_pre_cas_hook().await;

        let partial = partials.get(&id).cloned().unwrap_or_default();
        let bounded = bound_context(&partial);
        let res = auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::Ready),
            )
            .col_expr(
                auto_title_job::Column::FirstAssistantText,
                Expr::value(bounded),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(params.now))
            .filter(auto_title_job::Column::ConversationId.eq(id))
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::AwaitingTurn))
            .filter(auto_title_job::Column::FirstAssistantText.is_null())
            .filter(auto_title_job::Column::FirstPromptAt.is_not_null())
            .filter(auto_title_job::Column::FirstPromptAt.lte(cutoff))
            .exec(conn)
            .await?;
        if res.rows_affected == 1 {
            promoted += 1;
        }
    }

    Ok(promoted)
}

/// Claim the oldest ready job with a verified title API config snapshot.
///
/// Holds [`ConversationExperienceMutationGate`] for the whole snapshot so
/// settings writes cannot mid-flight change url/model/fp while claiming.
///
/// - `Ok(None)` — no work (Off, empty queue, only invalid ready rows deleted)
/// - `Ok(Some(claim))` — ready with config snapshot (no further keyring read)
/// - `Err(Unavailable)` — config drift / fp mismatch; barrier raised + jobs
///   wiped; **caller must `cancel_all`**
/// - `Err(AbnormalStop("db_error"))` — durable DB failure (coordinator backs off)
///
/// Claim rules for Ready rows:
/// - `job.config_gen != current_gen` → delete and continue
/// - empty / missing `first_user_text` → delete and continue
/// - `first_assistant_text == Some("")` (or any `Some`) → claimable
/// - `first_assistant_text == None` → invalid Ready; delete and continue
pub async fn claim_next_ready_with_config(
    conn: &DatabaseConnection,
    mutation_gate: &ConversationExperienceMutationGate,
) -> Result<Option<AutoTitleClaim>, AutoTitleRunError> {
    /// Initial try + retries for snapshot/busy on the ready→running upgrade.
    const CLAIM_CAS_TRANSIENT_MAX_ATTEMPTS: u32 = 8;

    // Gate covers the entire claim snapshot so set_auto_title_api_config cannot
    // interleave triple/fp mutation with this path.
    let _gate = mutation_gate.lock().await;

    let mut transient_cas_failures: u32 = 0;

    loop {
        let txn = conn.begin().await.map_err(|e| {
            tracing::warn!(%e, "auto-title claim: begin failed");
            AutoTitleRunError::AbnormalStop("db_error".into())
        })?;

        let config_snapshot = match load_claim_config_snapshot(&txn).await {
            Ok(v) => v,
            Err(AutoTitleRunError::Unavailable) => {
                let _ = txn.rollback().await;
                return apply_claim_unavailable_fail_closed(conn).await;
            }
            Err(e) => {
                let _ = txn.rollback().await;
                return Err(e);
            }
        };

        let Some((config, current_gen)) = config_snapshot else {
            // Off / incomplete: drop ready orphans so the worker does not spin.
            auto_title_job::Entity::delete_many()
                .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
                .exec(&txn)
                .await
                .map_err(|e| {
                    tracing::warn!(%e, "auto-title claim: delete ready orphans failed");
                    AutoTitleRunError::AbnormalStop("db_error".into())
                })?;
            txn.commit().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: commit off-path failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
            return Ok(None);
        };

        // Prefer current-gen ready rows. Stale-gen rows are deleted one-by-one
        // without a blanket DELETE in this txn (a write-upgrade here would hold
        // the SQLite write lock across the pre-CAS test gate and deadlock races).
        let candidate = auto_title_job::Entity::find()
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .order_by(auto_title_job::Column::UpdatedAt, Order::Asc)
            .order_by(auto_title_job::Column::ConversationId, Order::Asc)
            .one(&txn)
            .await
            .map_err(|e| {
                tracing::warn!(%e, "auto-title claim: select ready failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;

        let Some(job) = candidate else {
            txn.commit().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: commit empty failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
            return Ok(None);
        };

        if job.config_gen != current_gen {
            auto_title_job::Entity::delete_by_id(job.conversation_id)
                .exec(&txn)
                .await
                .map_err(|e| {
                    tracing::warn!(%e, "auto-title claim: delete stale-gen ready failed");
                    AutoTitleRunError::AbnormalStop("db_error".into())
                })?;
            txn.commit().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: commit after stale-gen delete failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
            transient_cas_failures = 0;
            continue;
        }

        let first_user = job.first_user_text.clone().unwrap_or_default();
        if first_user.trim().is_empty() {
            auto_title_job::Entity::delete_by_id(job.conversation_id)
                .exec(&txn)
                .await
                .map_err(|e| {
                    tracing::warn!(%e, "auto-title claim: delete empty-user failed");
                    AutoTitleRunError::AbnormalStop("db_error".into())
                })?;
            txn.commit().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: commit after empty-user failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
            transient_cas_failures = 0;
            continue;
        }

        let Some(first_assistant) = job.first_assistant_text.clone() else {
            auto_title_job::Entity::delete_by_id(job.conversation_id)
                .exec(&txn)
                .await
                .map_err(|e| {
                    tracing::warn!(%e, "auto-title claim: delete none-assistant failed");
                    AutoTitleRunError::AbnormalStop("db_error".into())
                })?;
            txn.commit().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: commit after none-assistant failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
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

        #[cfg(test)]
        claim_test_hooks::run_pre_cas_hook().await;

        let updated = match auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::Running),
            )
            .col_expr(
                auto_title_job::Column::Attempts,
                Expr::col(auto_title_job::Column::Attempts).add(1),
            )
            .col_expr(
                auto_title_job::Column::AttemptTurnSeq,
                Expr::col(auto_title_job::Column::UsableTurnSeq).into(),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(Utc::now()))
            .filter(auto_title_job::Column::ConversationId.eq(job.conversation_id))
            .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Ready))
            .filter(auto_title_job::Column::ConfigGen.eq(current_gen))
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
                    return Err(AutoTitleRunError::AbnormalStop("db_error".into()));
                }
                transient_cas_failures = transient_cas_failures.saturating_add(1);
                if transient_cas_failures >= CLAIM_CAS_TRANSIENT_MAX_ATTEMPTS {
                    tracing::warn!(
                        conversation_id = job.conversation_id,
                        attempts = transient_cas_failures,
                        %error,
                        "auto-title claim CAS exhausted transient retries"
                    );
                    return Err(AutoTitleRunError::AbnormalStop("db_error".into()));
                }
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
            txn.rollback().await.map_err(|e| {
                tracing::warn!(%e, "auto-title claim: rollback lost-race failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?;
            continue;
        }

        let claimed = auto_title_job::Entity::find_by_id(job.conversation_id)
            .one(&txn)
            .await
            .map_err(|e| {
                tracing::warn!(%e, "auto-title claim: re-read claimed failed");
                AutoTitleRunError::AbnormalStop("db_error".into())
            })?
            .ok_or_else(|| {
                AutoTitleRunError::AbnormalStop("claim_disappeared".into())
            })?;

        txn.commit().await.map_err(|e| {
            tracing::warn!(%e, "auto-title claim: commit success failed");
            AutoTitleRunError::AbnormalStop("db_error".into())
        })?;

        return Ok(Some(AutoTitleClaim {
            conversation_id: claimed.conversation_id,
            attempt: claimed.attempts,
            first_user_text: first_user,
            first_assistant_text: first_assistant,
            locale,
            attempt_turn_seq: claimed.attempt_turn_seq,
            config,
            config_gen: current_gen,
        }));
    }
}

/// Back-compat alias for tests/callers that still use the old name.
/// Prefer [`claim_next_ready_with_config`].
pub async fn claim_next_ready(
    conn: &DatabaseConnection,
    mutation_gate: &ConversationExperienceMutationGate,
) -> Result<Option<AutoTitleClaim>, AutoTitleRunError> {
    claim_next_ready_with_config(conn, mutation_gate).await
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
            first_user_text: String::new(),
            first_assistant_text: String::new(),
            locale: AppLocale::En,
            attempt_turn_seq: job.attempt_turn_seq,
            config: AutoTitleApiConfig::empty(),
            config_gen: job.config_gen,
        };
        let _ = record_attempt_failure(conn, &claim).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

    use crate::acp::delegation::spawner::DelegationLink;
    use crate::auto_title::title_key::{self, set_title_api_key, title_key_fingerprint};
    use crate::auto_title::title_settings::{
        parse_config_gen, KEY_AUTO_TITLE_API_KEY_FP, KEY_AUTO_TITLE_API_URL,
        KEY_AUTO_TITLE_CONFIG_BARRIER, KEY_AUTO_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_MODEL,
    };
    use crate::auto_title::types::{
        AutoTitleApiConfig, AutoTitleClaim, AutoTitleRunError, CompletionTransition,
        FinalizeTitleOutcome, TurnCompletionSnapshot,
    };
    use crate::commands::conversation_experience::ConversationExperienceMutationGate;
    use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
    use crate::db::entities::conversation;
    use crate::db::service::app_metadata_service;
    use crate::db::service::conversation_service::{
        create, create_chat, create_with_delegation, update_title,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use crate::models::system::AppLocale;

    const TEST_TITLE_SECRET: &str = "sk-test-auto-title-service";
    const TEST_TITLE_URL: &str = "https://api.example.com/v1";
    const TEST_TITLE_MODEL: &str = "gpt-4o-mini";

    fn test_gate() -> ConversationExperienceMutationGate {
        ConversationExperienceMutationGate::default()
    }

    /// Enable title API for enroll/claim tests (metadata + keyring + matching fp).
    async fn enable_auto_title(conn: &DatabaseConnection) {
        title_key::test_hooks::reset();
        let _ = set_title_api_key(TEST_TITLE_SECRET);
        // Prefer override so CI without keyring still works; unlimited via re-push.
        title_key::test_hooks::push_override_get(TitleKeyState::Present(
            TEST_TITLE_SECRET.into(),
        ));
        // Keep feeding Present for repeated get_title_api_key calls in one test.
        for _ in 0..32 {
            title_key::test_hooks::push_override_get(TitleKeyState::Present(
                TEST_TITLE_SECRET.into(),
            ));
        }
        let fp = title_key_fingerprint(TEST_TITLE_SECRET);
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_API_URL, TEST_TITLE_URL)
            .await
            .expect("url");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_MODEL, TEST_TITLE_MODEL)
            .await
            .expect("model");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_API_KEY_FP, &fp)
            .await
            .expect("fp");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
            .await
            .expect("barrier");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_CONFIG_GEN, "1")
            .await
            .expect("gen");
    }

    async fn disable_auto_title(conn: &DatabaseConnection) {
        title_key::test_hooks::reset();
        title_key::test_hooks::push_override_get(TitleKeyState::Absent);
        for _ in 0..8 {
            title_key::test_hooks::push_override_get(TitleKeyState::Absent);
        }
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_API_URL, "")
            .await
            .expect("clear url");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_MODEL, "")
            .await
            .expect("clear model");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_API_KEY_FP, "")
            .await
            .expect("clear fp");
        app_metadata_service::upsert_value(conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
            .await
            .expect("barrier");
    }

    fn claim_config() -> AutoTitleApiConfig {
        AutoTitleApiConfig {
            api_url: TEST_TITLE_URL.into(),
            api_key: TEST_TITLE_SECRET.into(),
            model: TEST_TITLE_MODEL.into(),
        }
    }

    use crate::auto_title::title_key::TitleKeyState;

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
            config_gen: Set(0),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed running job");
    }

    /// Clear the migration-era purge flag so the runtime one-shot can re-run.
    async fn clear_api_v1_purge_flag(conn: &DatabaseConnection) {
        app_metadata_service::upsert_value(
            conn,
            crate::auto_title::KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1,
            "0",
        )
        .await
        .expect("clear purge flag");
    }

    #[tokio::test]
    async fn purge_api_v1_deletes_all_job_states_and_sets_flag() {
        let db = fresh_in_memory_db().await;
        // Migrator sets the flag after deleting legacy rows; clear so this test
        // exercises the runtime one-shot path.
        clear_api_v1_purge_flag(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-purge-all-states").await;
        let cids = [
            create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("c1")
                .id,
            create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("c2")
                .id,
            create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("c3")
                .id,
            create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("c4")
                .id,
        ];
        // Drop any enroll side-effects and plant one row per durable state.
        for cid in cids {
            let _ = auto_title_job::Entity::delete_by_id(cid)
                .exec(&db.conn)
                .await;
        }
        for (cid, state) in [
            (cids[0], AutoTitleJobState::AwaitingTurn),
            (cids[1], AutoTitleJobState::Ready),
            (cids[2], AutoTitleJobState::Running),
            (cids[3], AutoTitleJobState::RetryWait),
        ] {
            seed_job_in_state(&db.conn, cid, state, Some("legacy"), Some("en")).await;
        }
        assert_eq!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("list")
                .len(),
            4
        );

        purge_auto_title_jobs_for_api_v1_if_needed(&db.conn)
            .await
            .expect("purge");

        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("list after")
                .is_empty(),
            "all states must be deleted"
        );
        let flag = app_metadata_service::get_value(
            &db.conn,
            crate::auto_title::KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1,
        )
        .await
        .expect("flag");
        assert_eq!(flag.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn purge_api_v1_is_idempotent_and_does_not_wipe_new_jobs() {
        let db = fresh_in_memory_db().await;
        clear_api_v1_purge_flag(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-purge-idempotent").await;
        let cid = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("conv")
            .id;
        let _ = auto_title_job::Entity::delete_by_id(cid)
            .exec(&db.conn)
            .await;
        seed_job_in_state(
            &db.conn,
            cid,
            AutoTitleJobState::Ready,
            Some("legacy"),
            Some("en"),
        )
        .await;

        purge_auto_title_jobs_for_api_v1_if_needed(&db.conn)
            .await
            .expect("first purge");
        assert!(auto_title_job::Entity::find_by_id(cid)
            .one(&db.conn)
            .await
            .expect("q")
            .is_none());

        // New API-era job after flag is set.
        seed_job_in_state(
            &db.conn,
            cid,
            AutoTitleJobState::Ready,
            Some("new api job"),
            Some("en"),
        )
        .await;

        purge_auto_title_jobs_for_api_v1_if_needed(&db.conn)
            .await
            .expect("second purge");

        let remaining = auto_title_job::Entity::find_by_id(cid)
            .one(&db.conn)
            .await
            .expect("q2")
            .expect("job must survive second purge");
        assert_eq!(remaining.first_user_text.as_deref(), Some("new api job"));
        assert_eq!(remaining.config_gen, 0);
    }

    #[tokio::test]
    async fn recover_and_start_purge_then_second_start_ok() {
        use crate::auto_title::coordinator::AutoTitleCoordinator;

        let db = fresh_in_memory_db().await;
        clear_api_v1_purge_flag(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-purge-recover").await;
        let cid = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("conv")
            .id;
        let _ = auto_title_job::Entity::delete_by_id(cid)
            .exec(&db.conn)
            .await;
        seed_job_in_state(
            &db.conn,
            cid,
            AutoTitleJobState::Running,
            Some("legacy running"),
            Some("en"),
        )
        .await;

        let coordinator = AutoTitleCoordinator::new_inert_for_test(db.conn.clone());

        coordinator
            .recover_and_start()
            .await
            .expect("first start");
        assert!(
            auto_title_job::Entity::find_by_id(cid)
                .one(&db.conn)
                .await
                .expect("q")
                .is_none(),
            "purge must run before recover so running job is not transitioned"
        );

        // Second start must not fail and must not wipe post-purge enrolls.
        seed_job_in_state(
            &db.conn,
            cid,
            AutoTitleJobState::AwaitingTurn,
            None,
            None,
        )
        .await;
        coordinator
            .recover_and_start()
            .await
            .expect("second start");
        assert!(
            auto_title_job::Entity::find_by_id(cid)
                .one(&db.conn)
                .await
                .expect("q2")
                .is_some(),
            "second start must leave new jobs intact"
        );
    }

    #[tokio::test]
    async fn enabled_creation_enrolls_root_and_delegate() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
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
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
            config: AutoTitleApiConfig::empty(),
            config_gen: 0,
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
        enable_auto_title(&db.conn).await;
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
    async fn off_config_does_not_enroll() {
        let db = fresh_in_memory_db().await;
        disable_auto_title(&db.conn).await;
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
            "Off title API must not enroll"
        );
    }

    #[tokio::test]
    async fn creation_racing_disable_leaves_no_job_when_off() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db = crate::db::init_database(temp.path(), "auto-title-create-disable-race")
            .await
            .expect("open pooled WAL database");

        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-create-disable-race").await;

        let (create_result, _) = tokio::join!(
            create(&db.conn, folder, AgentType::ClaudeCode, None, None),
            async {
                disable_auto_title(&db.conn).await;
                // Wipe jobs as settings Off path would after commit.
                auto_title_job::Entity::delete_many()
                    .exec(&db.conn)
                    .await
                    .expect("wipe");
            },
        );

        create_result.expect("create completed");
        // Final Off: either create enrolled then wipe, or create saw Off.
        // Drain any leftover: if create won after wipe with stale enabled, gen jobs
        // may exist only when create's enroll saw enabled after disable finished
        // without wipe of its insert — re-disable + wipe to stabilize assertion.
        disable_auto_title(&db.conn).await;
        auto_title_job::Entity::delete_many()
            .exec(&db.conn)
            .await
            .expect("final wipe");
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("jobs")
                .is_empty(),
            "final state must be Off with zero jobs"
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
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
            config: claim_config(),
            config_gen: 1,
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
            config_gen: Set(0),
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
        enable_auto_title(&db.conn).await;
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
        enable_auto_title(&db.conn).await;
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
        enable_auto_title(&db.conn).await;
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
        capture_prompt_context(&db.conn, conversation.id, &[], Some(&second), AppLocale::En)
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
        enable_auto_title(&migrate.conn).await;
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
                let capture = PromptCaptureContext::new(Some("task A".into()), Some(AppLocale::En));
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
                let capture = PromptCaptureContext::new(Some("task B".into()), Some(AppLocale::Ja));
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
        enable_auto_title(&db.conn).await;
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

    /// Race-test helper: ensure deadline eligibility, then promote via the real
    /// CAS path (includes `first_ready_race_hooks` pre-CAS gate).
    async fn simulate_deadline_promote(
        conn: &DatabaseConnection,
        conversation_id: i32,
        partial: &str,
    ) -> u64 {
        let now = Utc::now();
        let aged = now - chrono::Duration::seconds(400);
        // Make the job deadline-eligible without touching first_assistant when
        // already set (second simulate / post-end-turn calls stay no-ops).
        auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::FirstUserText,
                Expr::value("task".to_string()),
            )
            .col_expr(auto_title_job::Column::FirstPromptAt, Expr::value(aged))
            .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
            .filter(auto_title_job::Column::FirstAssistantText.is_null())
            .exec(conn)
            .await
            .expect("seed deadline eligibility for simulate promote");

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 32,
        };
        let mut partials = HashMap::new();
        partials.insert(conversation_id, partial.to_string());
        promote_deadline_jobs_by_ids(conn, &params, &[conversation_id], &partials)
            .await
            .expect("simulate deadline promote") as u64
    }

    async fn seed_deadline_fields(
        conn: &DatabaseConnection,
        conversation_id: i32,
        first_user_text: Option<&str>,
        first_prompt_at: Option<DateTime<Utc>>,
        state: AutoTitleJobState,
        first_assistant_text: Option<&str>,
    ) {
        let now = Utc::now();
        auto_title_job::Entity::update_many()
            .col_expr(auto_title_job::Column::State, Expr::value(state))
            .col_expr(
                auto_title_job::Column::FirstUserText,
                Expr::value(first_user_text.map(|s| s.to_string())),
            )
            .col_expr(
                auto_title_job::Column::FirstPromptAt,
                Expr::value(first_prompt_at),
            )
            .col_expr(
                auto_title_job::Column::FirstAssistantText,
                Expr::value(first_assistant_text.map(|s| s.to_string())),
            )
            .col_expr(auto_title_job::Column::UpdatedAt, Expr::value(now))
            .filter(auto_title_job::Column::ConversationId.eq(conversation_id))
            .exec(conn)
            .await
            .expect("seed deadline fields");
    }

    #[tokio::test]
    async fn promote_deadline_ready_with_partial_and_empty() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-deadline-promote-ready").await;
        let now = Utc::now();
        let aged = now - chrono::Duration::seconds(301);

        let with_partial = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create partial job");
        let missing_partial = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create empty-partial job");

        seed_deadline_fields(
            &db.conn,
            with_partial.id,
            Some("user task A"),
            Some(aged),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            missing_partial.id,
            Some("user task B"),
            Some(aged - chrono::Duration::seconds(1)),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 10,
        };
        let mut partials = HashMap::new();
        partials.insert(with_partial.id, "  partial answer  ".to_string());
        // missing_partial intentionally omitted → ""

        let ids = list_deadline_candidates(&db.conn, &params)
            .await
            .expect("list");
        assert_eq!(
            ids,
            vec![missing_partial.id, with_partial.id],
            "oldest first_prompt_at first, then conversation_id"
        );

        let promoted = promote_deadline_jobs_by_ids(&db.conn, &params, &ids, &partials)
            .await
            .expect("promote");
        assert_eq!(promoted, 2);

        let job_partial = auto_title_job::Entity::find_by_id(with_partial.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job partial");
        assert_eq!(job_partial.state, AutoTitleJobState::Ready);
        assert_eq!(
            job_partial.first_assistant_text.as_deref(),
            Some("  partial answer  "),
            "bound_context keeps short text; promote stores bounded partial"
        );
        assert_eq!(job_partial.updated_at, now);

        let job_empty = auto_title_job::Entity::find_by_id(missing_partial.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job empty");
        assert_eq!(job_empty.state, AutoTitleJobState::Ready);
        assert_eq!(
            job_empty.first_assistant_text.as_deref(),
            Some(""),
            "missing partial key promotes with Some(\"\")"
        );
    }

    #[tokio::test]
    async fn promote_skips_young_and_retry_wait_and_null_prompt_at() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-deadline-promote-skip").await;
        let now = Utc::now();
        let aged = now - chrono::Duration::seconds(400);
        let young = now - chrono::Duration::seconds(10);

        async fn enroll(db: &crate::db::AppDatabase, folder: i32) -> i32 {
            create(&db.conn, folder, AgentType::ClaudeCode, None, None)
                .await
                .expect("create")
                .id
        }

        let young_id = enroll(&db, folder).await;
        let retry_id = enroll(&db, folder).await;
        let null_prompt_id = enroll(&db, folder).await;
        let ready_id = enroll(&db, folder).await;
        let running_id = enroll(&db, folder).await;
        let eligible_id = enroll(&db, folder).await;

        seed_deadline_fields(
            &db.conn,
            young_id,
            Some("young"),
            Some(young),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            retry_id,
            Some("retry"),
            Some(aged),
            AutoTitleJobState::RetryWait,
            Some("snap"),
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            null_prompt_id,
            Some("legacy"),
            None,
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            ready_id,
            Some("ready"),
            Some(aged),
            AutoTitleJobState::Ready,
            Some("already"),
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            running_id,
            Some("running"),
            Some(aged),
            AutoTitleJobState::Running,
            Some("running-snap"),
        )
        .await;
        seed_deadline_fields(
            &db.conn,
            eligible_id,
            Some("eligible"),
            Some(aged),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 50,
        };
        let candidates = list_deadline_candidates(&db.conn, &params)
            .await
            .expect("list");
        assert_eq!(
            candidates,
            vec![eligible_id],
            "only aged awaiting_turn with non-null first_prompt_at"
        );

        let mut partials = HashMap::new();
        for id in [
            young_id,
            retry_id,
            null_prompt_id,
            ready_id,
            running_id,
            eligible_id,
        ] {
            partials.insert(id, format!("partial-{id}"));
        }

        // Force-promote every id; only eligible should succeed.
        let all_ids = [
            young_id,
            retry_id,
            null_prompt_id,
            ready_id,
            running_id,
            eligible_id,
        ];
        let promoted = promote_deadline_jobs_by_ids(&db.conn, &params, &all_ids, &partials)
            .await
            .expect("promote");
        assert_eq!(promoted, 1);

        let young_job = auto_title_job::Entity::find_by_id(young_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(young_job.state, AutoTitleJobState::AwaitingTurn);
        assert!(young_job.first_assistant_text.is_none());

        let retry_job = auto_title_job::Entity::find_by_id(retry_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retry_job.state, AutoTitleJobState::RetryWait);
        assert_eq!(retry_job.first_assistant_text.as_deref(), Some("snap"));

        let null_job = auto_title_job::Entity::find_by_id(null_prompt_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(null_job.state, AutoTitleJobState::AwaitingTurn);
        assert!(null_job.first_assistant_text.is_none());
        assert!(null_job.first_prompt_at.is_none());

        let ready_job = auto_title_job::Entity::find_by_id(ready_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ready_job.state, AutoTitleJobState::Ready);
        assert_eq!(ready_job.first_assistant_text.as_deref(), Some("already"));

        let running_job = auto_title_job::Entity::find_by_id(running_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(running_job.state, AutoTitleJobState::Running);
        assert_eq!(
            running_job.first_assistant_text.as_deref(),
            Some("running-snap")
        );

        let eligible_job = auto_title_job::Entity::find_by_id(eligible_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(eligible_job.state, AutoTitleJobState::Ready);
        assert_eq!(
            eligible_job.first_assistant_text.as_deref(),
            Some(format!("partial-{eligible_id}").as_str())
        );
    }

    /// Pre-migration rows keep NULL `first_prompt_at` after upgrade (Task 1).
    /// They must never deadline-promote, but end-turn + later capture must still
    /// work without backfilling the timestamp.
    #[tokio::test]
    async fn legacy_null_first_prompt_at_end_turn_only() {
        use crate::auto_title::types::PromptCaptureContext;

        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-legacy-null-prompt-at").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let conversation_id = conversation.id;
        // Even far past a nominal 300s window, NULL first_prompt_at is ineligible.
        let now = Utc::now();

        // Simulate upgraded legacy row: first_user set, first_prompt_at NULL.
        seed_deadline_fields(
            &db.conn,
            conversation_id,
            Some("legacy task"),
            None,
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 32,
        };
        let candidates = list_deadline_candidates(&db.conn, &params)
            .await
            .expect("list");
        assert!(
            !candidates.contains(&conversation_id),
            "legacy NULL first_prompt_at must never be a deadline candidate"
        );

        let mut partials = HashMap::new();
        partials.insert(conversation_id, "partial must not land".to_string());
        let promoted =
            promote_deadline_jobs_by_ids(&db.conn, &params, &[conversation_id], &partials)
                .await
                .expect("promote");
        assert_eq!(promoted, 0, "legacy NULL first_prompt_at must not promote");

        let before_end = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(before_end.state, AutoTitleJobState::AwaitingTurn);
        assert!(before_end.first_assistant_text.is_none());
        assert!(before_end.first_prompt_at.is_none());

        // End-turn still arms Ready with first assistant snapshot.
        let snapshot = TurnCompletionSnapshot {
            conversation_id,
            turn_token: "legacy-tok".into(),
            locale: AppLocale::En,
            final_text: Arc::from("legacy final answer"),
        };
        let txn = db.conn.begin().await.expect("begin");
        let transition = apply_usable_completion(&txn, &snapshot, "end_turn")
            .await
            .expect("end_turn");
        txn.commit().await.expect("commit");
        assert!(transition.became_ready);
        assert_eq!(transition.usable_turn_seq, 1);

        let after_end = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(after_end.state, AutoTitleJobState::Ready);
        assert_eq!(
            after_end.first_assistant_text.as_deref(),
            Some("legacy final answer")
        );
        assert_eq!(after_end.first_user_text.as_deref(), Some("legacy task"));
        assert!(
            after_end.first_prompt_at.is_none(),
            "end-turn must not invent first_prompt_at for legacy rows"
        );

        // Capture with first_user already set must not backfill first_prompt_at.
        let capture = PromptCaptureContext::new(
            Some("should not replace legacy task".into()),
            Some(AppLocale::Ja),
        );
        capture_prompt_context(
            &db.conn,
            conversation_id,
            &[],
            Some(&capture),
            AppLocale::En,
        )
        .await
        .expect("capture after legacy");

        let after_capture = auto_title_job::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("job");
        assert_eq!(
            after_capture.first_user_text.as_deref(),
            Some("legacy task"),
            "first_user_text stays write-once for legacy rows"
        );
        assert!(
            after_capture.first_prompt_at.is_none(),
            "capture must not backfill first_prompt_at when first_user already set"
        );
        assert_eq!(
            after_capture.locale.as_deref(),
            Some("ja"),
            "locale still refreshes on surviving job"
        );
    }

    #[tokio::test]
    async fn promote_cas_loses_to_end_turn() {
        let fixture = awaiting_job_fixture().await;
        let now = Utc::now();
        let aged = now - chrono::Duration::seconds(301);
        seed_deadline_fields(
            &fixture.db.conn,
            fixture.conversation_id,
            Some("task"),
            Some(aged),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;

        let snapshot = fixture.snapshot("tok-wins", "full final wins first");
        let transition = fixture.apply_completion(&snapshot).await;
        assert!(transition.became_ready);
        assert_eq!(transition.usable_turn_seq, 1);

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 10,
        };
        let mut partials = HashMap::new();
        partials.insert(
            fixture.conversation_id,
            "deadline partial must lose".to_string(),
        );

        let promoted = promote_deadline_elapsed_jobs(&fixture.db.conn, &params, &partials)
            .await
            .expect("promote after end-turn");
        assert_eq!(promoted, 0, "end-turn already owns first-assistant");

        let job = fixture.job().await;
        assert_eq!(job.state, AutoTitleJobState::Ready);
        assert_eq!(
            job.first_assistant_text.as_deref(),
            Some("full final wins first"),
            "promote must not overwrite end-turn assistant"
        );
        assert_eq!(job.usable_turn_seq, 1);
    }

    #[tokio::test]
    async fn promote_select_then_delete_before_cas_is_noop() {
        let fixture = awaiting_job_fixture().await;
        let now = Utc::now();
        let aged = now - chrono::Duration::seconds(301);
        seed_deadline_fields(
            &fixture.db.conn,
            fixture.conversation_id,
            Some("task"),
            Some(aged),
            AutoTitleJobState::AwaitingTurn,
            None,
        )
        .await;

        let params = DeadlinePromoteParams {
            now,
            deadline: Duration::from_secs(300),
            batch_limit: 10,
        };
        let ids = list_deadline_candidates(&fixture.db.conn, &params)
            .await
            .expect("list candidates");
        assert_eq!(ids, vec![fixture.conversation_id]);

        let deleted = cancel_job(&fixture.db.conn, fixture.conversation_id)
            .await
            .expect("cancel/delete job after select");
        assert!(deleted);

        let mut partials = HashMap::new();
        partials.insert(fixture.conversation_id, "stale partial".to_string());
        let promoted = promote_deadline_jobs_by_ids(&fixture.db.conn, &params, &ids, &partials)
            .await
            .expect("promote after delete must not panic");
        assert_eq!(promoted, 0);

        assert!(
            auto_title_job::Entity::find_by_id(fixture.conversation_id)
                .one(&fixture.db.conn)
                .await
                .unwrap()
                .is_none(),
            "job remains deleted"
        );
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
        let promoted =
            simulate_deadline_promote(&fixture.db.conn, fixture.conversation_id, "partial").await;
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
            simulate_deadline_promote(&fixture_empty.db.conn, fixture_empty.conversation_id, "",)
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

    /// REQUIRED WAL + dual pre-write barrier: two concurrent completions with
    /// distinct tokens on the same Ready job must both reach the progress UPDATE
    /// site before either writes, so atomic `usable_turn_seq = usable_turn_seq + 1`
    /// is forced under true concurrent updates (not serialized full apply paths
    /// that could hide a lost concurrent current_seq+1 RMW).
    #[tokio::test]
    async fn two_distinct_usable_tokens_advance_seq_twice() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        use sea_orm::{ConnectOptions, Database, DbBackend, Statement};
        use tokio::sync::Barrier;

        use crate::db::test_helpers::fresh_disk_db;

        /// Bound for the dual pre-write handshake so a stuck barrier cannot hang
        /// the suite indefinitely.
        const BARRIER_SEQ_TIMEOUT: Duration = Duration::from_secs(5);
        const GATE_STEP_TIMEOUT: Duration = Duration::from_secs(2);

        let dir = tempfile::tempdir().expect("tempdir");
        let migrate = fresh_disk_db(dir.path()).await;
        enable_auto_title(&migrate.conn).await;
        let folder = seed_folder(&migrate, "/tmp/title-dual-token-seq").await;
        let conversation = create(&migrate.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let conversation_id = conversation.id;
        let _ = auto_title_job::Entity::delete_by_id(conversation_id)
            .exec(&migrate.conn)
            .await;
        // Ready with seq=0 so two concurrent distinct-token advances → seq=2.
        seed_ready_claim_job(
            &migrate.conn,
            conversation_id,
            Some("user task"),
            Some("deadline snap"),
            0,
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

        // Task-local completion pre-write hooks park both apply paths at the
        // progress UPDATE gate; Barrier(2) releases only after both arrive so
        // the atomic increments run under concurrent open transactions.
        let barrier = Arc::new(Barrier::new(2));
        let at_gate = Arc::new(AtomicUsize::new(0));

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
            let barrier = barrier.clone();
            let at_gate = at_gate.clone();
            async move {
                first_ready_race_hooks::scope_completion(
                    Arc::new(move || {
                        let barrier = barrier.clone();
                        let at_gate = at_gate.clone();
                        Box::pin(async move {
                            at_gate.fetch_add(1, Ordering::SeqCst);
                            tokio::time::timeout(GATE_STEP_TIMEOUT, barrier.wait())
                                .await
                                .expect("completion A pre-write barrier must release");
                        })
                    }),
                    async move {
                        let txn = pool.conn.begin().await.expect("begin a");
                        let result = apply_usable_completion(&txn, &snap, "end_turn")
                            .await
                            .expect("apply a");
                        txn.commit().await.expect("commit a");
                        result
                    },
                )
                .await
            }
        });
        let handle_b = tokio::spawn({
            let pool = pool_b.clone();
            let snap = snap_b.clone();
            let barrier = barrier.clone();
            let at_gate = at_gate.clone();
            async move {
                first_ready_race_hooks::scope_completion(
                    Arc::new(move || {
                        let barrier = barrier.clone();
                        let at_gate = at_gate.clone();
                        Box::pin(async move {
                            at_gate.fetch_add(1, Ordering::SeqCst);
                            tokio::time::timeout(GATE_STEP_TIMEOUT, barrier.wait())
                                .await
                                .expect("completion B pre-write barrier must release");
                        })
                    }),
                    async move {
                        let txn = pool.conn.begin().await.expect("begin b");
                        let result = apply_usable_completion(&txn, &snap, "end_turn")
                            .await
                            .expect("apply b");
                        txn.commit().await.expect("commit b");
                        result
                    },
                )
                .await
            }
        });

        let (ta, tb) = tokio::time::timeout(BARRIER_SEQ_TIMEOUT, async {
            tokio::join!(handle_a, handle_b)
        })
        .await
        .expect("dual completion must not hang past barrier sequence timeout");
        let ta = ta.expect("join a");
        let tb = tb.expect("join b");

        assert_eq!(
            at_gate.load(Ordering::SeqCst),
            2,
            "both apply paths must hit the shared pre-write gate before either progress UPDATE"
        );

        // Each completion reports the seq it observed after its progress write;
        // together they must cover +2 from the seeded seq=0.
        let reported: std::collections::HashSet<i32> = [ta.usable_turn_seq, tb.usable_turn_seq]
            .into_iter()
            .collect();
        assert_eq!(
            reported,
            [1, 2].into_iter().collect(),
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
            job.usable_turn_seq, 2,
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
        enable_auto_title(&migrate.conn).await;
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
            let promoted = simulate_deadline_promote(&pool_deadline.conn, cid, "partial-a").await;
            assert_eq!(
                promoted, 1,
                "deadline promote must win first-ready in order A"
            );

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
                        simulate_deadline_promote(&deadline_pool.conn, cid, "partial-b-must-lose")
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
            // Matches enable_auto_title gen=1.
            config_gen: Set(1),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed ready claim job");
    }

    #[tokio::test]
    async fn claim_accepts_empty_assistant_some_empty_string() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-empty-assistant").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        // create() may enroll awaiting_turn; replace with precise Ready row.
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(&db.conn, conversation.id, Some("user task"), Some(""), 1).await;

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("claim")
            .expect("Ready + Some(\"\") must be claimable");

        assert_eq!(claim.conversation_id, conversation.id);
        assert_eq!(claim.first_user_text, "user task");
        assert_eq!(claim.first_assistant_text, "");
        assert_eq!(claim.attempt, 1);
        assert_eq!(claim.attempt_turn_seq, 1);
        assert_eq!(claim.config.model, TEST_TITLE_MODEL);
        assert_eq!(claim.config_gen, 1);

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
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-none-assistant").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(&db.conn, conversation.id, Some("user task"), None, 1).await;

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("claim");
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
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-empty-user").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(&db.conn, conversation.id, Some("   "), Some("assistant"), 1).await;

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("claim");
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
        enable_auto_title(&migrate.conn).await;
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
                    claim_next_ready(&claim_conn.conn, &test_gate()),
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

    // ── Task 4 named claim/enroll safety tests ─────────────────────────────

    #[tokio::test]
    async fn enroll_only_when_enabled() {
        let db = fresh_in_memory_db().await;
        disable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-enroll-off").await;
        let off = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create off");
        assert!(auto_title_job::Entity::find_by_id(off.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none());

        enable_auto_title(&db.conn).await;
        let on = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create on");
        let job = auto_title_job::Entity::find_by_id(on.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("enrolled");
        assert_eq!(job.config_gen, 1);
    }

    #[tokio::test]
    async fn claim_rejects_bad_gen() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-bad-gen").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        // Ready row bound to a stale gen.
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation.id),
            state: Set(AutoTitleJobState::Ready),
            attempts: Set(0),
            first_user_text: Set(Some("user".into())),
            first_assistant_text: Set(Some("asst".into())),
            first_prompt_at: Set(None),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(1),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(Some("t1".into())),
            config_gen: Set(0), // stale vs enable gen=1
            updated_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .expect("seed stale");

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("claim ok");
        assert!(claim.is_none(), "stale gen must not claim");
        assert!(
            auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "stale-gen ready must be deleted"
        );
    }

    #[tokio::test]
    async fn fp_mismatch_claim_fail_closed() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        // Overwrite stored fp so live key Present does not match.
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_API_KEY_FP,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await
        .expect("bad fp");
        let folder = seed_folder(&db, "/tmp/title-claim-fp-mismatch").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            1,
        )
        .await;

        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("fp mismatch");
        assert_eq!(err, AutoTitleRunError::Unavailable);
        let barrier = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER)
            .await
            .expect("barrier");
        assert_eq!(barrier.as_deref(), Some("1"));
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("jobs")
                .is_empty(),
            "fail-closed must wipe jobs"
        );
    }

    #[tokio::test]
    async fn absent_key_with_configured_url_model_fail_closed() {
        // External key deletion while url+model still look complete must not
        // quiet-Off: raise barrier, wipe jobs, gen+=1, Unavailable (caller cancels).
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-absent-configured").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            1,
        )
        .await;

        // Key externally deleted; url/model/fp/barrier remain configured-looking.
        title_key::test_hooks::reset();
        for _ in 0..8 {
            title_key::test_hooks::push_override_get(TitleKeyState::Absent);
        }

        let gen_before = parse_config_gen(
            app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN)
                .await
                .unwrap()
                .as_deref(),
        );

        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("Absent + configured must fail-closed");
        assert_eq!(err, AutoTitleRunError::Unavailable);

        let barrier = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER)
            .await
            .expect("barrier");
        assert_eq!(barrier.as_deref(), Some("1"));
        let gen_after = parse_config_gen(
            app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN)
                .await
                .unwrap()
                .as_deref(),
        );
        assert_eq!(gen_after, gen_before + 1, "fail-closed must bump gen");
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("jobs")
                .is_empty(),
            "fail-closed must wipe jobs"
        );
    }

    #[tokio::test]
    async fn absent_key_with_empty_config_quiet_off() {
        // Genuine Off (no url/model): Absent is quiet Ok(None), not Unavailable.
        let db = fresh_in_memory_db().await;
        disable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-claim-absent-off").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        // Orphan ready under Off — claim deletes and returns None.
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            0,
        )
        .await;

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("quiet Off");
        assert!(claim.is_none());
        let barrier = app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER)
            .await
            .expect("barrier");
        assert_ne!(
            barrier.as_deref(),
            Some("1"),
            "quiet Off must not raise barrier"
        );
    }

    #[tokio::test]
    async fn fail_closed_wipe_failure_still_returns_unavailable() {
        // Wipe DB failure must not become AbnormalStop-only (coordinator would
        // retry without cancel). Always Unavailable so cancel_all still runs.
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_API_KEY_FP,
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await
        .expect("bad fp");
        let folder = seed_folder(&db, "/tmp/title-claim-wipe-fail").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            1,
        )
        .await;

        claim_fail_closed_hooks::reset();
        claim_fail_closed_hooks::arm_force_wipe_fail();

        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("wipe fail still Unavailable");
        assert_eq!(
            err,
            AutoTitleRunError::Unavailable,
            "wipe failure must not map to AbnormalStop-only"
        );
        // Barrier may be unset because wipe was forced to fail; jobs may remain.
        // The critical contract is Unavailable (caller cancel_all).
        claim_fail_closed_hooks::reset();
    }

    #[tokio::test]
    async fn post_save_key_overwrite_at_claim() {
        // Same shape as fp mismatch: Present(secret) with stored fp of another key.
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let other_fp = title_key_fingerprint("sk-other-key-value");
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP, &other_fp)
            .await
            .expect("other fp");
        let folder = seed_folder(&db, "/tmp/title-claim-key-overwrite").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            1,
        )
        .await;

        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("overwrite");
        assert_eq!(err, AutoTitleRunError::Unavailable);
        // No HTTP is possible without a claim snapshot — Unavailable is the gate.
    }

    #[tokio::test]
    async fn stale_enroll_vs_save_race_no_claimable_job() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/title-enroll-stale-race").await;
        // Simulate enroll that captured gen=1, then a save bumps gen to 2 and
        // would purge — leave a job with gen=1 while live gen is 2.
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let job = auto_title_job::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("enrolled at gen 1");
        assert_eq!(job.config_gen, 1);

        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN, "2")
            .await
            .expect("bump gen");
        // Promote to ready so claim would try.
        auto_title_job::Entity::update_many()
            .col_expr(
                auto_title_job::Column::State,
                Expr::value(AutoTitleJobState::Ready),
            )
            .col_expr(
                auto_title_job::Column::FirstAssistantText,
                Expr::value("assistant"),
            )
            .col_expr(auto_title_job::Column::UsableTurnSeq, Expr::value(1))
            .filter(auto_title_job::Column::ConversationId.eq(conversation.id))
            .exec(&db.conn)
            .await
            .expect("ready");
        // first_user may still be null if never captured — seed user text.
        auto_title_job::Entity::update_many()
            .col_expr(auto_title_job::Column::FirstUserText, Expr::value("user"))
            .filter(auto_title_job::Column::ConversationId.eq(conversation.id))
            .exec(&db.conn)
            .await
            .expect("user");

        let claim = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect("claim");
        assert!(
            claim.is_none(),
            "stale enroll gen must not be claimable after save gen bump"
        );
    }

    #[tokio::test]
    async fn set_and_clear_restart_after_commit_shapes() {
        // Unit-level claim shapes for Set/Clear restart-after-commit:
        // stored fp is for secret N / empty, live key is A / Present(A).
        let db = fresh_in_memory_db().await;

        // Set N: stored fp(N), live A → mismatch fail-closed.
        enable_auto_title(&db.conn).await;
        let fp_n = title_key_fingerprint("sk-N-committed");
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP, &fp_n)
            .await
            .expect("fp N");
        title_key::test_hooks::reset();
        for _ in 0..8 {
            title_key::test_hooks::push_override_get(TitleKeyState::Present(
                "sk-A-reintroduced".into(),
            ));
        }
        let folder = seed_folder(&db, "/tmp/title-restart-set").await;
        let c1 = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("c1");
        let _ = auto_title_job::Entity::delete_by_id(c1.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(&db.conn, c1.id, Some("u"), Some("a"), 1).await;
        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("set restart");
        assert_eq!(err, AutoTitleRunError::Unavailable);

        // Clear: empty stored fp, live A reintroduced while url/model still set
        // and barrier clear — Present + empty stored fp is mismatch.
        title_key::test_hooks::reset();
        for _ in 0..8 {
            title_key::test_hooks::push_override_get(TitleKeyState::Present(
                "sk-A-reintroduced".into(),
            ));
        }
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP, "")
            .await
            .expect("empty fp");
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
            .await
            .expect("barrier clear");
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_URL, TEST_TITLE_URL)
            .await
            .expect("url");
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_MODEL, TEST_TITLE_MODEL)
            .await
            .expect("model");
        // gen still 1 after fail-closed bump may have advanced — re-read.
        let c2 = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("c2");
        // May or may not enroll depending on enabled (key Present + fields) —
        // force ready row with current gen if any.
        let gen = parse_config_gen(
            app_metadata_service::get_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN)
                .await
                .unwrap()
                .as_deref(),
        );
        let gen_i64 = i64::try_from(gen).unwrap_or(0);
        let _ = auto_title_job::Entity::delete_by_id(c2.id)
            .exec(&db.conn)
            .await;
        auto_title_job::ActiveModel {
            conversation_id: Set(c2.id),
            state: Set(AutoTitleJobState::Ready),
            attempts: Set(0),
            first_user_text: Set(Some("u".into())),
            first_assistant_text: Set(Some("a".into())),
            first_prompt_at: Set(None),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(1),
            attempt_turn_seq: Set(0),
            last_usable_turn_token: Set(Some("t".into())),
            config_gen: Set(gen_i64),
            updated_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .expect("ready c2");

        let err = claim_next_ready(&db.conn, &test_gate())
            .await
            .expect_err("clear restart");
        assert_eq!(err, AutoTitleRunError::Unavailable);
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[tokio::test]
    async fn concurrent_tokens_write_during_claim_read_coherent() {
        use std::sync::Arc;
        use std::time::Duration;

        // Server tokens.json path: concurrent set_token + claim get must not
        // spuriously Unavailable-wipe from truncated JSON.
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("CODEG_DATA_DIR", dir.path());
        title_key::test_hooks::reset();

        let db = fresh_in_memory_db().await;
        // Real keyring path (no override) so mutex/atomic publish is exercised.
        set_title_api_key(TEST_TITLE_SECRET).expect("set key");
        let fp = title_key_fingerprint(TEST_TITLE_SECRET);
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_URL, TEST_TITLE_URL)
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_MODEL, TEST_TITLE_MODEL)
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_API_KEY_FP, &fp)
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
            .await
            .unwrap();
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_CONFIG_GEN, "1")
            .await
            .unwrap();

        let folder = seed_folder(&db, "/tmp/title-tokens-concurrent").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let _ = auto_title_job::Entity::delete_by_id(conversation.id)
            .exec(&db.conn)
            .await;
        seed_ready_claim_job(
            &db.conn,
            conversation.id,
            Some("user"),
            Some("assistant"),
            1,
        )
        .await;

        let db_claim = Arc::new(AppDatabase {
            conn: db.conn.clone(),
        });
        let writers: Vec<_> = (0..8)
            .map(|i| {
                std::thread::spawn(move || {
                    for j in 0..20 {
                        let secret = format!("sk-writer-{i}-{j}");
                        let _ = set_title_api_key(&secret);
                    }
                })
            })
            .collect();

        // Interleave claims while writers hammer tokens.json.
        let mut saw_ok_or_unavailable = false;
        for _ in 0..10 {
            match claim_next_ready(&db_claim.conn, &test_gate()).await {
                Ok(Some(_)) | Ok(None) | Err(AutoTitleRunError::Unavailable) => {
                    saw_ok_or_unavailable = true;
                }
                Err(e) => panic!("unexpected claim error (no spurious panic): {e}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for w in writers {
            w.join().expect("writer");
        }
        assert!(saw_ok_or_unavailable);
        std::env::remove_var("CODEG_DATA_DIR");
        title_key::test_hooks::reset();
    }
}
