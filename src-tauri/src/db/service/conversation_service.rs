use std::collections::HashMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};

use crate::acp::delegation::route::{is_managed_agent, DelegationRoutePolicy};
use crate::auto_title::{cancel_job, enroll_new_conversation};
use crate::db::entities::conversation::ConversationKind;
use crate::db::entities::{conversation, folder};
use crate::db::error::DbError;
use crate::models::{AgentType, ConversationStatePatch, DbConversationSummary};

#[derive(Debug, Clone)]
pub struct ClearAwaitingReplyOutcome {
    pub patch: ConversationStatePatch,
    pub changed: bool,
}

fn status_string(status: &conversation::ConversationStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}"))
}

fn state_patch(
    id: i32,
    status: conversation::ConversationStatus,
    awaiting_reply_token: Option<String>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> ConversationStatePatch {
    ConversationStatePatch {
        id,
        status: status_string(&status),
        awaiting_reply_token,
        updated_at,
    }
}

pub async fn create(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
) -> Result<conversation::Model, DbError> {
    create_with_route_override(conn, folder_id, agent_type, title, git_branch, None).await
}

/// Like [`create`] but persists a root session route override in the same
/// INSERT. Rejects a non-null override for unmanaged Agent types.
pub async fn create_with_route_override(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    delegation_route_override: Option<DelegationRoutePolicy>,
) -> Result<conversation::Model, DbError> {
    if delegation_route_override.is_some() && !is_managed_agent(agent_type) {
        return Err(DbError::Validation(
            "delegation_route_override is only valid for managed agents \
             (Codex, Grok, CodeBuddy, ClaudeCode)"
                .into(),
        ));
    }
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        CreateInnerOptions {
            delegation: None,
            kind: ConversationKind::Regular,
            delegation_route_override,
        },
    )
    .await
}

/// Mirror of [`create`] for folderless chat-mode conversations: identical row
/// shape but `kind = 'chat'`, so the sidebar routes the row to its flat "Chat"
/// section. Callers must pair it with the hidden chat folder created in the
/// same flow (`create_chat_conversation_core`).
pub async fn create_chat(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
) -> Result<conversation::Model, DbError> {
    create_chat_with_route_override(conn, folder_id, agent_type, title, git_branch, None).await
}

/// Like [`create_chat`] but persists a route override in the same INSERT.
pub async fn create_chat_with_route_override(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    delegation_route_override: Option<DelegationRoutePolicy>,
) -> Result<conversation::Model, DbError> {
    if delegation_route_override.is_some() && !is_managed_agent(agent_type) {
        return Err(DbError::Validation(
            "delegation_route_override is only valid for managed agents \
             (Codex, Grok, CodeBuddy, ClaudeCode)"
                .into(),
        ));
    }
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        CreateInnerOptions {
            delegation: None,
            kind: ConversationKind::Chat,
            delegation_route_override,
        },
    )
    .await
}

/// Mirror of [`create`] plus optional delegation linkage. Used by the
/// multi-agent broker when spawning a child sub-session — populates
/// `parent_id` / `parent_tool_use_id` / `delegation_call_id` so the lifecycle
/// subscriber and frontend can rebuild the parent ↔ child binding without
/// inspecting the live broker state. `kind` follows the invariant
/// `delegate ⟺ parent_id set`. Broker children always store a null route
/// override (connection origin forces Codeg later).
pub async fn create_with_delegation(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
) -> Result<conversation::Model, DbError> {
    let kind = if delegation.is_some() {
        ConversationKind::Delegate
    } else {
        ConversationKind::Regular
    };
    create_inner(
        conn,
        folder_id,
        agent_type,
        title,
        git_branch,
        CreateInnerOptions {
            delegation,
            kind,
            delegation_route_override: None,
        },
    )
    .await
}

/// Private options for [`create_inner`]: kind, optional broker linkage, and
/// optional managed route override. Bundled so the private insert helper stays
/// under the clippy argument threshold.
struct CreateInnerOptions {
    delegation: Option<crate::acp::delegation::spawner::DelegationLink>,
    kind: ConversationKind,
    delegation_route_override: Option<DelegationRoutePolicy>,
}

async fn create_inner(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: AgentType,
    title: Option<String>,
    git_branch: Option<String>,
    options: CreateInnerOptions,
) -> Result<conversation::Model, DbError> {
    let CreateInnerOptions {
        delegation,
        kind,
        delegation_route_override,
    } = options;
    let is_delegate = delegation.is_some();
    let rollup_defaults = if is_delegate {
        (
            Some(0_i64),
            Some(0_i64),
            Some("[]".to_string()),
            Some(false),
            None::<i64>,
            None::<i64>,
            Some(false),
        )
    } else {
        (None, None, None, None, None, None, None)
    };
    let (
        delegation_tool_call_count,
        delegation_edit_tool_call_count,
        delegation_touched_files_json,
        delegation_touched_files_truncated,
        delegation_additions,
        delegation_deletions,
        delegation_line_counts_complete,
    ) = rollup_defaults;
    let at_str = serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default();
    let now = Utc::now();
    let (parent_id, parent_tool_use_id, delegation_call_id, task_fields) = match delegation {
        Some(link) => (
            Some(link.parent_conversation_id),
            Some(link.parent_tool_use_id),
            Some(link.delegation_call_id),
            // Accepted boundary: linked delegate rows are born running. Normal
            // root/chat rows keep all four task fields null.
            (
                Some(conversation::DelegationTaskStatus::Running),
                None::<String>,
                Some(now),
                None,
            ),
        ),
        None => (None, None, None, (None, None, None, None)),
    };
    let (
        delegation_task_status,
        delegation_error_code,
        delegation_started_at,
        delegation_finished_at,
    ) = task_fields;
    let txn = conn.begin().await?;
    let model = conversation::ActiveModel {
        id: NotSet,
        folder_id: Set(folder_id),
        title: Set(title),
        title_locked: Set(false),
        auto_title_finalized: Set(false),
        agent_type: Set(at_str),
        status: Set(conversation::ConversationStatus::InProgress),
        kind: Set(kind),
        model: Set(None),
        git_branch: Set(git_branch),
        external_id: Set(None),
        parent_id: Set(parent_id),
        parent_tool_use_id: Set(parent_tool_use_id),
        delegation_call_id: Set(delegation_call_id),
        delegation_route_override: Set(route_policy_to_storage(delegation_route_override)),
        delegation_task_status: Set(delegation_task_status),
        delegation_error_code: Set(delegation_error_code),
        delegation_started_at: Set(delegation_started_at),
        delegation_finished_at: Set(delegation_finished_at),
        delegation_tool_call_count: Set(delegation_tool_call_count),
        delegation_edit_tool_call_count: Set(delegation_edit_tool_call_count),
        delegation_touched_files_json: Set(delegation_touched_files_json),
        delegation_touched_files_truncated: Set(delegation_touched_files_truncated),
        delegation_additions: Set(delegation_additions),
        delegation_deletions: Set(delegation_deletions),
        delegation_line_counts_complete: Set(delegation_line_counts_complete),
        message_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        pinned_at: Set(None),
        awaiting_reply_token: Set(None),
        delegation_run_generation: Set(None),
        last_termination_audit_json: Set(None),

        origin_cwd: Set(None),
    }
    .insert(&txn)
    .await?;
    enroll_new_conversation(&txn, model.id, now).await?;
    txn.commit().await?;
    Ok(model)
}

fn route_policy_to_storage(policy: Option<DelegationRoutePolicy>) -> Option<String> {
    policy.map(|p| match p {
        DelegationRoutePolicy::Codeg => "codeg".to_string(),
        DelegationRoutePolicy::Native => "native".to_string(),
    })
}

/// Map a stored route override string to the typed policy. Malformed legacy
/// values log and become `None` — never panic.
fn parse_route_override(raw: Option<String>) -> Option<DelegationRoutePolicy> {
    match raw.as_deref() {
        None => None,
        Some("codeg") => Some(DelegationRoutePolicy::Codeg),
        Some("native") => Some(DelegationRoutePolicy::Native),
        Some(other) => {
            tracing::warn!(
                "[conversation_service] malformed delegation_route_override {other:?}, treating as None"
            );
            None
        }
    }
}

/// Unconditional status write: sets `status`, clears `awaiting_reply_token`,
/// and bumps `updated_at` atomically. Returns the resulting state patch so
/// callers can broadcast without a second read.
///
/// **Warning:** this is not CAS. Callers that must not clobber a concurrent
/// `TurnComplete` winner should prefer `update_status_if_with_patch` or
/// `finish_end_turn_if_in_progress`. Every write is logged (prev → new) so
/// post-CAS overwrites that leave rows stuck at `in_progress` are diagnosable.
pub async fn update_status_with_patch(
    conn: &DatabaseConnection,
    conversation_id: i32,
    status: conversation::ConversationStatus,
) -> Result<ConversationStatePatch, DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let prev_status = conv.status.clone();
    let now = Utc::now();
    let mut active: conversation::ActiveModel = conv.into();
    active.status = Set(status.clone());
    active.awaiting_reply_token = Set(None);
    active.updated_at = Set(now);
    active.update(conn).await?;
    let prev_s = status_string(&prev_status);
    let new_s = status_string(&status);
    if prev_status != status {
        tracing::info!(
            conversation_id,
            prev_status = %prev_s,
            new_status = %new_s,
            "[conversation_service] status write (unconditional)"
        );
        // High-signal: anything that re-opens a finished turn without going
        // through the intentional send_prompt path is the #394-class bug.
        if matches!(
            (&prev_status, &status),
            (
                conversation::ConversationStatus::PendingReview
                    | conversation::ConversationStatus::Completed
                    | conversation::ConversationStatus::Cancelled,
                conversation::ConversationStatus::InProgress
            )
        ) {
            tracing::warn!(
                conversation_id,
                prev_status = %prev_s,
                new_status = %new_s,
                "[conversation_service][WARN] terminal/pending_review → in_progress \
                 (clears awaiting_reply_token); expected only on a real new prompt"
            );
        }
    } else {
        tracing::debug!(
            conversation_id,
            status = %new_s,
            "[conversation_service] status rewrite (same value; token cleared)"
        );
    }
    Ok(state_patch(conversation_id, status, None, now))
}

pub async fn update_status(
    conn: &DatabaseConnection,
    conversation_id: i32,
    status: conversation::ConversationStatus,
) -> Result<(), DbError> {
    update_status_with_patch(conn, conversation_id, status)
        .await
        .map(|_| ())
}

/// Conditional status transition (CAS): write `new_status` only if the row's
/// current `status` equals `expected`. Returns the state patch when the row was
/// updated, `None` when the CAS lost. Clears `awaiting_reply_token` on a
/// successful write. Used by the lifecycle subscriber on disconnect/error so a
/// concurrent user-driven `completed` (or a prior `pending_review` from
/// `TurnComplete`) cannot be silently overwritten.
pub async fn update_status_if_with_patch<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    expected: conversation::ConversationStatus,
    new_status: conversation::ConversationStatus,
) -> Result<Option<ConversationStatePatch>, DbError> {
    use sea_orm::sea_query::Expr;
    let now = Utc::now();
    let result = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::Status,
            Expr::value(new_status.clone()),
        )
        .col_expr(
            conversation::Column::AwaitingReplyToken,
            Expr::value(Option::<String>::None),
        )
        .col_expr(conversation::Column::UpdatedAt, Expr::value(now))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::Status.eq(expected))
        .exec(conn)
        .await?;
    Ok((result.rows_affected > 0).then(|| state_patch(conversation_id, new_status, None, now)))
}

pub async fn update_status_if<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    expected: conversation::ConversationStatus,
    new_status: conversation::ConversationStatus,
) -> Result<bool, DbError> {
    Ok(
        update_status_if_with_patch(conn, conversation_id, expected, new_status)
            .await?
            .is_some(),
    )
}

/// Atomic end-of-turn CAS: transition `in_progress → pending_review` and
/// optionally mint an `awaiting_reply_token` for eligible root conversations.
/// Returns `None` when the CAS loses (row not in_progress / not found / deleted).
///
/// Token is minted only when `parent_id IS NULL` AND `mark_awaiting_reply`.
/// Child and background transitions never receive a generation.
pub async fn finish_end_turn_if_in_progress<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    mark_awaiting_reply: bool,
) -> Result<Option<ConversationStatePatch>, DbError> {
    use sea_orm::sea_query::Expr;

    let row = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let token =
        (row.parent_id.is_none() && mark_awaiting_reply).then(|| uuid::Uuid::new_v4().to_string());
    let now = Utc::now();
    let result = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::Status,
            Expr::value(conversation::ConversationStatus::PendingReview),
        )
        .col_expr(
            conversation::Column::AwaitingReplyToken,
            Expr::value(token.clone()),
        )
        .col_expr(conversation::Column::UpdatedAt, Expr::value(now))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::Status.eq(conversation::ConversationStatus::InProgress))
        .exec(conn)
        .await?;
    Ok((result.rows_affected == 1).then(|| {
        state_patch(
            conversation_id,
            conversation::ConversationStatus::PendingReview,
            token,
            now,
        )
    }))
}

/// Expected-token CAS clear: write `awaiting_reply_token = NULL` only when the
/// stored token matches `expected_token`. Never mutates `status` or
/// `updated_at`. Always fetches and returns the current backend state so stale
/// clears still surface the live generation.
pub async fn clear_awaiting_reply(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected_token: &str,
) -> Result<ClearAwaitingReplyOutcome, DbError> {
    use sea_orm::sea_query::Expr;

    let result = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::AwaitingReplyToken,
            Expr::value(Option::<String>::None),
        )
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::AwaitingReplyToken.eq(expected_token))
        .exec(conn)
        .await?;
    let changed = result.rows_affected == 1;

    let row = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;

    Ok(ClearAwaitingReplyOutcome {
        patch: state_patch(
            conversation_id,
            row.status,
            row.awaiting_reply_token,
            row.updated_at,
        ),
        changed,
    })
}

/// Manual rename: set the title AND lock it. Once locked, the per-turn
/// auto-title backfill ([`refresh_auto_title`]) leaves this row alone, so the
/// user's hand-picked name survives every subsequent session-file parse.
///
/// Returns `true` when a pending auto-title job was removed and active work
/// must be cancelled after commit (wired in Task 8).
pub async fn update_title(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;

    let txn = conn.begin().await?;
    let changed = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .col_expr(conversation::Column::TitleLocked, Expr::value(true))
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if changed.rows_affected == 0 {
        return Err(DbError::Migration(format!(
            "Conversation not found: {conversation_id}"
        )));
    }
    let removed = cancel_job(&txn, conversation_id).await?;
    txn.commit().await?;
    Ok(removed)
}

/// Auto-derive counterpart to [`update_title`]: write `title` ONLY when the row
/// is not user-locked and the value actually changed. Never sets `title_locked`
/// (the title stays eligible for future auto-refreshes, e.g. when an agent like
/// OpenCode regenerates its own session title) and deliberately does NOT bump
/// `updated_at` — a title backfill is metadata, not user activity, so it must
/// not float the row to the top of a recency-sorted sidebar. Returns `true`
/// when a row was written so the caller can broadcast a sidebar upsert.
///
/// Implemented as a single conditional UPDATE (`... WHERE id = ? AND
/// title_locked = false AND (title IS NULL OR title <> ?)`) so the lock/equality
/// checks and the write are atomic: a manual rename ([`update_title`], which
/// sets `title_locked = true`) that lands between a would-be read and the write
/// can never be clobbered, because the lock predicate is re-evaluated at write
/// time by the database. A non-existent row simply matches nothing (`false`).
pub async fn refresh_auto_title(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let title = title.trim();
    if title.is_empty() {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::AutoTitleFinalized.eq(false))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            sea_orm::Condition::any()
                .add(conversation::Column::Title.is_null())
                .add(conversation::Column::Title.ne(title)),
        )
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// First-prompt seed: write `title` ONLY when the row is unlocked AND still
/// empty. Unlike [`refresh_auto_title`], this will not replace an existing
/// name — a later user prompt must not overwrite the first one, and an
/// agent-generated ACP title that already landed must not be clobbered by
/// the next send. Returns `true` when a row was written so the caller can
/// broadcast a sidebar upsert. Does not bump `updated_at` or set the lock.
pub async fn seed_auto_title_if_empty(
    conn: &DatabaseConnection,
    conversation_id: i32,
    title: String,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let title = title.trim();
    if title.is_empty() {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            sea_orm::Condition::any()
                .add(conversation::Column::Title.is_null())
                .add(conversation::Column::Title.eq("")),
        )
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Lock a row's title WITHOUT rewriting it. For a conversation whose name was
/// typed by the user somewhere else — a work task's title, an automation's name
/// — the seed passed to [`create`] already IS the name; all that's missing is
/// the promise that [`refresh_auto_title`] will keep its hands off it, exactly
/// as it does after a manual rename ([`update_title`]).
///
/// Without this, a task-launched session drifts to whatever the agent's session
/// file parses to — for agents with no title of their own, the first line of the
/// composed prompt — and the board's own name for the work is lost (issue #495).
///
/// Deliberately does NOT touch `title` or bump `updated_at`: like
/// [`refresh_auto_title`], flipping this flag is metadata, not user activity,
/// and must not float the row to the top of a recency-sorted sidebar. A
/// non-existent row simply matches nothing.
///
/// Callers must lock BEFORE broadcasting the row's first sidebar upsert: the
/// conversation id is not knowable to any client until that broadcast, so
/// locking first makes an auto-title backfill on this row impossible rather
/// than merely unlikely.
pub async fn lock_title(conn: &DatabaseConnection, conversation_id: i32) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    conversation::Entity::update_many()
        .col_expr(conversation::Column::TitleLocked, Expr::value(true))
        .filter(conversation::Column::Id.eq(conversation_id))
        .exec(conn)
        .await?;
    Ok(())
}

/// Rename a locked title when its OWNER was renamed: write `new_title` only if
/// the row still carries `expected`. Used when a work task is retitled — the
/// session it produced should follow the card it came from, but only while the
/// two are still in sync.
///
/// The equality guard is the whole point. Both a task-derived title and a hand
/// picked one leave `title_locked = true`, so the flag alone cannot tell them
/// apart; `expected` (the task's PREVIOUS title) can. If the user renamed the
/// conversation themselves, nothing matches and not a byte is written. Same
/// optimistic shape as [`refresh_auto_title`]: one conditional UPDATE, so the
/// comparison happens at write time in the database and a rename landing in
/// between can never be clobbered. Returns `true` when a row was written.
///
/// Leaves `title_locked` alone (already true for every row this can match) and,
/// like its siblings, does not bump `updated_at`.
pub async fn retitle_if_unchanged(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected: &str,
    new_title: &str,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let new_title = new_title.trim();
    if new_title.is_empty() || new_title == expected {
        return Ok(false);
    }
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(new_title))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::Title.eq(expected))
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Conditionally adopt one title discovered from Codex's session index.
///
/// Every field used to select the candidate is re-checked in the UPDATE, plus
/// the title observed by the candidate read. This prevents a delayed refresh
/// from writing an index title after the row was re-pointed, deleted, manually
/// renamed, moved into a deleted folder, or refreshed by another task.
///
/// `kind` is deliberately absent: it is written once at insert and never
/// updated, so the candidate query's filter cannot go stale between the two.
async fn refresh_codex_auto_title_candidate(
    conn: &DatabaseConnection,
    candidate: &conversation::Model,
    title: &str,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;

    let title = title.trim();
    let Some(external_id) = candidate.external_id.as_deref() else {
        return Ok(false);
    };
    if title.is_empty() || candidate.title.as_deref() == Some(title) {
        return Ok(false);
    }

    let old_title = match candidate.title.as_deref() {
        Some(old_title) => conversation::Column::Title.eq(old_title),
        None => conversation::Column::Title.is_null(),
    };
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .filter(conversation::Column::Id.eq(candidate.id))
        .filter(conversation::Column::AgentType.eq(AgentType::Codex.as_wire().into_owned()))
        .filter(conversation::Column::ExternalId.eq(external_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(
            conversation::Column::FolderId.in_subquery(
                sea_orm::sea_query::Query::select()
                    .column(folder::Column::Id)
                    .from(folder::Entity)
                    .and_where(folder::Column::DeletedAt.is_null())
                    .to_owned(),
            ),
        )
        .filter(old_title)
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}

/// Refresh every live, unlocked Codex conversation whose external session id
/// has a title in `titles`. Candidate selection is limited to indexed session
/// ids and chunked below SQLite's bound-variable limit. Each UPDATE atomically
/// re-checks the complete candidate identity via
/// [`refresh_codex_auto_title_candidate`]. Converged rows issue no UPDATE and
/// title refreshes never bump `updated_at`. This is a best-effort reconciliation:
/// a failed chunk or row is logged and skipped while successful row ids are
/// retained for downstream notifications.
///
/// Candidate scope mirrors [`list_all`]'s own visibility rules — a live folder
/// and `kind != 'loop'` — because every refreshed id is broadcast as a sidebar
/// upsert. Refreshing a row this query would never return would push a row the
/// list deliberately hides into every client's sidebar until its next refetch.
///
/// Each candidate keeps its OWN autocommit UPDATE rather than sharing one
/// transaction per chunk: the guarantee callers rely on is that a row that
/// failed (or lost its CAS) never holds back the rows that succeeded, and a
/// chunk-wide transaction would roll those back together.
pub(crate) async fn refresh_codex_auto_titles(
    conn: &DatabaseConnection,
    titles: &HashMap<String, String>,
) -> Vec<i32> {
    if titles.is_empty() {
        return Vec::new();
    }

    const SQLITE_TITLE_QUERY_CHUNK_SIZE: usize = 500;
    let external_ids: Vec<String> = titles
        .iter()
        .filter(|(_, title)| !title.trim().is_empty())
        .map(|(external_id, _)| external_id.clone())
        .collect();
    let mut refreshed = Vec::new();

    for external_id_chunk in external_ids.chunks(SQLITE_TITLE_QUERY_CHUNK_SIZE) {
        let candidates = match conversation::Entity::find()
            .filter(conversation::Column::AgentType.eq(AgentType::Codex.as_wire().into_owned()))
            .filter(conversation::Column::ExternalId.is_in(external_id_chunk.iter().cloned()))
            .filter(conversation::Column::DeletedAt.is_null())
            .filter(conversation::Column::TitleLocked.eq(false))
            .filter(conversation::Column::Kind.ne(ConversationKind::Loop))
            .filter(
                conversation::Column::FolderId.in_subquery(
                    sea_orm::sea_query::Query::select()
                        .column(folder::Column::Id)
                        .from(folder::Entity)
                        .and_where(folder::Column::DeletedAt.is_null())
                        .to_owned(),
                ),
            )
            .order_by_asc(conversation::Column::Id)
            .all(conn)
            .await
        {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    session_count = external_id_chunk.len(),
                    "failed to select Codex title refresh candidates; skipping chunk"
                );
                continue;
            }
        };

        for candidate in candidates {
            let Some(external_id) = candidate.external_id.as_deref() else {
                continue;
            };
            let Some(title) = titles.get(external_id) else {
                continue;
            };
            match refresh_codex_auto_title_candidate(conn, &candidate, title).await {
                Ok(true) => refreshed.push(candidate.id),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    error = %error,
                    conversation_id = candidate.id,
                    external_id,
                    "failed to refresh Codex title candidate; skipping row"
                ),
            }
        }
    }

    refreshed
}

/// Adopt an imported conversation's newest activity from its agent-side
/// transcript: stamp `updated_at` with the session file's own last-activity
/// time (never `now()` — the scan is not the activity) and re-sync
/// `message_count`. Returns `true` when a row was written, so the caller can
/// broadcast a sidebar upsert.
///
/// This is the counterpart to [`refresh_auto_title`] for the OTHER half of a
/// re-import: a session the user kept working on in the agent's own CLI after
/// importing it into codeg. Its title may be unchanged while its activity is
/// hours newer, and `updated_at` is what the sidebar's "recently updated"
/// ordering (and the relative timestamp on each row) reads.
///
/// One conditional UPDATE, guarded so it can never do harm:
/// * `updated_at < activity_at` — strictly forward. A re-import can never move
///   a conversation backwards or re-order an unchanged one, re-running is a
///   no-op, and a turn running live in codeg (which stamps `updated_at =
///   now()`) wins over a transcript tail parsed moments earlier.
/// * `deleted_at IS NULL` — a soft-deleted conversation stays deleted; agent
///   activity must not half-resurrect an invisible row.
/// * `parent_id IS NULL` — delegation children are not sidebar rows and are
///   maintained by the delegation flow.
///
/// Everything else the user owns is left alone: `created_at`, `title` (and its
/// lock), `pinned_at`, `status`, and folder placement.
pub async fn refresh_external_activity(
    conn: &DatabaseConnection,
    conversation_id: i32,
    activity_at: chrono::DateTime<Utc>,
    message_count: u32,
) -> Result<bool, DbError> {
    use sea_orm::sea_query::Expr;
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::UpdatedAt, Expr::value(activity_at))
        .col_expr(
            conversation::Column::MessageCount,
            Expr::value(message_count as i32),
        )
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::ParentId.is_null())
        .filter(conversation::Column::UpdatedAt.lt(activity_at))
        .exec(conn)
        .await?;
    Ok(res.rows_affected > 0)
}
/// Pin or unpin a conversation. Sets `pinned_at = now()` when pinning, `NULL`
/// when unpinning. Only the `pinned_at` column is written — `updated_at` is
/// deliberately left untouched (SeaORM updates only the `Set` field), because
/// pinning is a view preference, not conversation activity, and must not float
/// the row to the top of a recency-sorted sidebar (same reasoning as
/// [`refresh_auto_title`]). The sidebar's "Pinned" section orders by `pinned_at`
/// descending, so a freshly pinned conversation jumps to the top.
pub async fn update_pin(
    conn: &DatabaseConnection,
    conversation_id: i32,
    pinned: bool,
) -> Result<(), DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;
    let mut active: conversation::ActiveModel = conv.into();
    active.pinned_at = Set(pinned.then(Utc::now));
    active.update(conn).await?;
    Ok(())
}

/// Bind an agent session id (`external_id`) to a conversation row WITHOUT ever
/// orphaning the session it was previously bound to.
///
/// Returns `Some(preserved_row_id)` when the previous session had to be moved
/// onto a freshly inserted row to keep it reachable, `None` when the write was
/// an ordinary (re)binding that created nothing.
///
/// Either `Ok` is a promise the caller builds on: **the row is now bound to
/// `external_id`**. `send_prompt_linked` flips the row to `InProgress` and
/// dispatches the prompt on exactly that basis. When the id cannot be taken
/// because another row holds it, this returns [`DbError::Conflict`] rather than
/// a quiet `Ok(None)` — see the conflict guard in the body for why reporting a
/// refusal as success would misfile the user's turn.
///
/// # The invariant
///
/// **After this commits, the previous session's history is still reachable.**
///
/// Stated as a post-condition on purpose. The tempting phrasing ("before
/// re-pointing, some other row must already hold S1") reads as an ordering
/// requirement and leads straight into the unique index below.
///
/// Note "history is reachable", not "some row holds the id". `continues` is
/// what separates the two: it lists the sessions the INCOMING id carries
/// forward (see [`crate::acp::continued_session_ids`]). When the outgoing id
/// is one of them, this is the same conversation moving to a new agent
/// session — its turns are still readable through the new id, so the row just
/// advances and nothing is split off. Passing an empty slice is always safe
/// in the sense that it never loses data; it only risks splitting a
/// continuation into a duplicate conversation.
///
/// Why it matters: the conversation list is built purely from DB rows
/// (`list_all`) — nothing scans the agent's own transcript store. So a session
/// id that no row references is invisible in the UI even though its transcript
/// is intact on disk, and the user reasonably reads that as "my conversation
/// was deleted". That is codeg#500: a connection spawned with `session_id =
/// None` mints a fresh session, then a prompt carrying an existing
/// `conversation_id` adopts that row and the fresh id lands on top of the id
/// the row's whole history hangs off.
///
/// # Why the writes are ordered the way they are
///
/// `idx_conversation_external_agent` is UNIQUE over
/// `(external_id, agent_type)` (see the init migration). So the preserving row
/// can only be inserted AFTER the current row has released S1 — insert-first
/// fails the constraint every time. Both statements share one transaction, so
/// the "S1 released but nobody holds it" window can never be observed by
/// another connection and never commits. (SQLite treats NULLs as distinct, so
/// any number of rows may sit at `external_id IS NULL`.)
///
/// The claim write that opens the transaction is a SELF-ASSIGNMENT
/// (`updated_at = updated_at`). It exists for the same reason as the one in
/// `persist_fork_outcome` — lead with a write so SQLite takes the writer lock
/// immediately instead of a deferred read snapshot it may fail to promote
/// (`SQLITE_BUSY_SNAPSHOT`, surfaced as a bogus "database is locked") — but it
/// must not CHANGE `updated_at`, because the row is read straight afterwards
/// and the preserving row copies that pre-transition timestamp to hold its
/// place in the sidebar's activity ordering. A self-assigning UPDATE still
/// matches the row and still claims the lock.
///
/// The `deleted_at IS NULL` guard is inherited from the previous
/// `update_external_id`: `SessionStarted` writes are not serialized against
/// deletes (deleting only soft-marks the row; the agent stays connected and
/// bound), so a late event must not half-resurrect an invisible row.
#[cfg(test)]
pub async fn update_external_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
    external_id: String,
) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    conversation::Entity::update_many()
        .col_expr(conversation::Column::ExternalId, Expr::value(external_id))
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(())
}

pub async fn bind_external_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
    external_id: &str,
    continues: &[String],
) -> Result<Option<i32>, DbError> {
    use sea_orm::sea_query::Expr;
    use sea_orm::TransactionTrait;

    let external_id = external_id.to_string();
    let continues: Vec<String> = continues.to_vec();
    // The closure below MOVES `external_id`; keep a copy for the conflict
    // message, which is built after the transaction has returned.
    let requested_id = external_id.clone();
    let outcome = conn
        .transaction::<_, BindTxOutcome, sea_orm::DbErr>(|txn| {
            Box::pin(async move {
                let now = Utc::now();

                // Claim the writer lock without disturbing any value.
                let claimed = conversation::Entity::update_many()
                    .col_expr(
                        conversation::Column::UpdatedAt,
                        Expr::col(conversation::Column::UpdatedAt).into(),
                    )
                    .filter(conversation::Column::Id.eq(conversation_id))
                    .filter(conversation::Column::DeletedAt.is_null())
                    .exec(txn)
                    .await?;
                if claimed.rows_affected == 0 {
                    // Gone or soft-deleted. Every caller treats "no live row" as
                    // nothing to do, so this stays Ok — same contract the old
                    // `update_external_id` had.
                    return Ok(BindTxOutcome::Bound(None));
                }

                // Read under the write lock: pristine values, and no other
                // writer can interpose before this transaction finishes.
                let current = conversation::Entity::find_by_id(conversation_id)
                    .one(txn)
                    .await?
                    .ok_or_else(|| {
                        sea_orm::DbErr::Custom(format!("conversation {conversation_id} not found"))
                    })?;

                let previous = current.external_id.clone();
                let repoints_away = matches!(
                    previous.as_deref(),
                    Some(prev) if prev != external_id && !continues.iter().any(|c| c == prev)
                );

                // Conflict guard, covering BOTH write branches below.
                //
                // `idx_conversation_external_agent` is UNIQUE over
                // `(external_id, agent_type)` and — unlike every read in this
                // module — carries no `deleted_at` predicate, so a soft-deleted
                // row still occupies its id. Claiming an id another row holds
                // therefore fails the constraint outright, rolls the whole
                // transaction back, and surfaces a raw "UNIQUE constraint
                // failed" out of `send_prompt_linked`: the user's prompt just
                // errors. Both branches are exposed — the plain bind (a fresh
                // row whose connection resumed a session codeg already has a
                // row for) as much as the split's release.
                //
                // Refuse instead, and change nothing. The invariant still
                // holds, from both ends: this row keeps the session it had, and
                // the incoming session is already reachable through the row
                // that holds it. Taking the id would orphan THAT row's history
                // — the very failure this function exists to prevent — so there
                // is no better move available here, only a louder one.
                //
                // A soft-deleted holder is refused on the same terms rather
                // than having its id taken, matching how the rest of this
                // module treats rows the sidebar never shows (`import_one` and
                // `refresh_existing` return `Skipped` for one instead of
                // rewriting it). Conversations have no restore path, so such a
                // row is inert — but it is inert with the id, and the index
                // counts it.
                //
                // Skipped when this row already holds the id: it is then the
                // holder, so by the same index no other row can be.
                //
                // Surfaced as `DbError::Conflict`, NOT as a quiet `Ok(None)`.
                // The distinction is load-bearing: `Ok` here means "this row is
                // bound to `external_id`", and `send_prompt_linked` goes on to
                // flip the row to `InProgress` and dispatch the prompt on that
                // basis. A refusal reported as success would send the turn into
                // a session the row does not own — it would land in the HOLDER
                // row's transcript while every event named the refused row, so
                // the user's message would move conversations on reload. An
                // `Err` returns before either step, exactly as the raw unique
                // violation used to, and the retry in
                // `lifecycle::handle_event_with_retry` gives up after three
                // attempts.
                if previous.as_deref() != Some(external_id.as_str()) {
                    let holder = conversation::Entity::find()
                        .filter(conversation::Column::ExternalId.eq(external_id.clone()))
                        .filter(conversation::Column::AgentType.eq(current.agent_type.clone()))
                        .filter(conversation::Column::Id.ne(conversation_id))
                        .one(txn)
                        .await?;
                    if let Some(holder) = holder {
                        // Logged here rather than left to the caller: these
                        // fields are what post-hoc diagnosis needs, and the
                        // error string that reaches the user must stay short.
                        tracing::warn!(
                            conversation_id,
                            holder_row_id = holder.id,
                            holder_deleted = holder.deleted_at.is_some(),
                            from_session = previous.as_deref().unwrap_or("<none>"),
                            to_session = %external_id,
                            agent_type = %current.agent_type,
                            "[conversation] refused to bind a session another row \
                             already holds; both histories left where they are"
                        );
                        return Ok(BindTxOutcome::Refused {
                            holder_row_id: holder.id,
                        });
                    }
                }

                // A row that already holds this id, or holds none yet, is an
                // ordinary bind: first bind, resume, or a duplicate
                // SessionStarted. A row whose id the INCOMING session carries
                // forward is one too — same conversation, new agent session,
                // with the earlier turns still readable through the new id (see
                // `continues`). Splitting that would clone the conversation in
                // the sidebar every time a memory-only custom agent restarts.
                // Nothing to preserve in any of these.
                if !repoints_away {
                    let mut active: conversation::ActiveModel = current.into();
                    active.external_id = Set(Some(external_id));
                    active.updated_at = Set(now);
                    active.update(txn).await?;
                    return Ok(BindTxOutcome::Bound(None));
                }

                let previous = previous.expect("repoints_away implies a previous id");

                // Does another live row already carry the outgoing session?
                //
                // Under today's schema it cannot: this row holds `previous`,
                // and the unique index admits exactly one holder per
                // `(external_id, agent_type)`. The cases that look like they
                // produce it do not. Fork writes its sibling INSERT and its
                // re-point in ONE transaction, so `previous` is observable only
                // on this row (before) or on the sibling with this row already
                // advanced (after — and then `repoints_away` is false and we
                // never reach here). A replayed `SessionStarted` is likewise
                // the idempotent branch above.
                //
                // Kept as a guard rather than an assert because the cost is one
                // indexed lookup and the failure it covers is silent data loss:
                // if the index is ever narrowed (a partial `WHERE deleted_at IS
                // NULL` would do it), two rows COULD hold `previous`, and
                // preserving it a second time would then insert a duplicate
                // conversation for history that already has a home.
                let already_preserved = conversation::Entity::find()
                    .filter(conversation::Column::ExternalId.eq(previous.clone()))
                    .filter(conversation::Column::AgentType.eq(current.agent_type.clone()))
                    .filter(conversation::Column::Id.ne(conversation_id))
                    .filter(conversation::Column::DeletedAt.is_null())
                    .one(txn)
                    .await?
                    .is_some();

                // Snapshot what the preserving row inherits BEFORE the re-point
                // consumes `current`.
                let carried = CarriedOverRow::from(&current);

                // Release S1 first — the unique index leaves no other order.
                let mut active: conversation::ActiveModel = current.into();
                active.external_id = Set(Some(external_id.clone()));
                active.updated_at = Set(now);
                active.update(txn).await?;

                if already_preserved {
                    tracing::info!(
                        conversation_id,
                        from_session = %previous,
                        to_session = %external_id,
                        "[conversation] re-pointed a conversation to a new session; the \
                         previous session is already held by another row"
                    );
                    return Ok(BindTxOutcome::Bound(None));
                }

                let agent_type = carried.agent_type.clone();
                let preserved = carried
                    .into_active_model(previous.clone())
                    .insert(txn)
                    .await?;
                // The one signal that this happened at all. Deliberately WARN:
                // every occurrence means a connection bound to a row while
                // holding a session unrelated to that row's history, which is
                // never intentional — the split keeps it from destroying data,
                // but the cause is still worth finding. These five fields are
                // what post-hoc diagnosis actually needs (codeg#500 was
                // reconstructed by hand from WAL dumps for want of them).
                tracing::warn!(
                    conversation_id,
                    preserved_row_id = preserved.id,
                    from_session = %previous,
                    to_session = %external_id,
                    agent_type = %agent_type,
                    "[conversation] session changed under a bound conversation; \
                     preserved the previous session's history on a new row"
                );
                Ok(BindTxOutcome::Bound(Some(preserved.id)))
            })
        })
        .await
        .map_err(|e| match e {
            sea_orm::TransactionError::Connection(e)
            | sea_orm::TransactionError::Transaction(e) => DbError::Database(e),
        })?;
    match outcome {
        BindTxOutcome::Bound(preserved) => Ok(preserved),
        // Raised AFTER the transaction commits rather than by rolling it back:
        // the only statement it ran is the self-assigning claim, which changes
        // no value, so commit and rollback are indistinguishable on disk and
        // committing keeps the writer lock held for the shortest time.
        //
        // The message is what reaches the user through
        // `AcpError::protocol(e.to_string())`, so it names the conflict in
        // terms the sidebar can be read against; the WARN inside the
        // transaction carries the full diagnostic tuple.
        BindTxOutcome::Refused { holder_row_id } => Err(DbError::Conflict(format!(
            "agent session {requested_id} is already bound to conversation \
             {holder_row_id}; refusing to move it onto conversation \
             {conversation_id}"
        ))),
    }
}

/// What [`bind_external_id`]'s transaction concluded.
///
/// A refusal has to leave the closure as a distinct VALUE rather than an early
/// `Ok(None)`, because the two mean opposite things to a caller: `Bound` means
/// the row now holds the requested session and it is safe to build on that,
/// while `Refused` means nothing was written and the operation must be
/// abandoned. It is translated to [`DbError::Conflict`] outside the
/// transaction (a `sea_orm::DbErr` raised inside would be flattened into
/// `DbError::Database` by the `TransactionError` mapping and become
/// indistinguishable from transient contention).
enum BindTxOutcome {
    /// The row holds `external_id`. `Some(id)` when the outgoing session had to
    /// be split onto a new row to stay reachable.
    Bound(Option<i32>),
    /// Another row already holds `(external_id, agent_type)`. Nothing written.
    Refused { holder_row_id: i32 },
}

/// The fields a preserving row inherits from the row it is splitting off from.
///
/// Snapshotted before the re-point because turning the model into an
/// `ActiveModel` consumes it.
struct CarriedOverRow {
    folder_id: i32,
    title: Option<String>,
    title_locked: bool,
    agent_type: String,
    status: conversation::ConversationStatus,
    kind: ConversationKind,
    model: Option<String>,
    git_branch: Option<String>,
    parent_id: Option<i32>,
    message_count: i32,
    created_at: chrono::DateTime<Utc>,
    updated_at: chrono::DateTime<Utc>,
    origin_cwd: Option<String>,
}

impl CarriedOverRow {
    fn from(row: &conversation::Model) -> Self {
        Self {
            folder_id: row.folder_id,
            title: row.title.clone(),
            title_locked: row.title_locked,
            agent_type: row.agent_type.clone(),
            // `InProgress` is deliberately NOT carried over: no agent is
            // attached to the outgoing session any more, so copying it would
            // leave a row spinning in the sidebar forever. This is the same
            // call `persist_fork_outcome` makes for its sibling. A row that
            // already reached a terminal state keeps it — that state is
            // user-visible and still true of the history being preserved.
            status: match row.status {
                conversation::ConversationStatus::InProgress => {
                    conversation::ConversationStatus::PendingReview
                }
                ref settled => settled.clone(),
            },
            kind: row.kind.clone(),
            model: row.model.clone(),
            git_branch: row.git_branch.clone(),
            // Carried so the `kind == Delegate ⟺ parent_id IS NOT NULL`
            // invariant survives, and so a preserved child stays inside its
            // parent's sub-session subtree instead of surfacing as a root.
            parent_id: row.parent_id,
            message_count: row.message_count,
            // Copied, not stamped `now`: the preserving row IS the old
            // conversation, so it must keep its place in the sidebar's
            // activity ordering rather than jumping to the top as if new.
            created_at: row.created_at,
            updated_at: row.updated_at,
            // The Gemini/Cline stale-external-id fallback matches on
            // `origin_cwd ?? folder.path`, so dropping this would break
            // history lookup for a re-parented conversation.
            origin_cwd: row.origin_cwd.clone(),
        }
    }

    fn into_active_model(self, external_id: String) -> conversation::ActiveModel {
        conversation::ActiveModel {
            id: NotSet,
            folder_id: Set(self.folder_id),
            title: Set(self.title),
            title_locked: Set(self.title_locked),
            auto_title_finalized: Set(false),
            agent_type: Set(self.agent_type),
            status: Set(self.status),
            kind: Set(self.kind),
            model: Set(self.model),
            git_branch: Set(self.git_branch),
            external_id: Set(Some(external_id)),
            parent_id: Set(self.parent_id),
            // Deliberately NOT carried: both are close to unique per delegation
            // call, and duplicating them would point the parent's tool-call view
            // at two children for one call.
            parent_tool_use_id: Set(None),
            delegation_call_id: Set(None),
            delegation_route_override: Set(None),
            delegation_task_status: Set(None),
            delegation_error_code: Set(None),
            delegation_started_at: Set(None),
            delegation_finished_at: Set(None),
            delegation_tool_call_count: Set(None),
            delegation_edit_tool_call_count: Set(None),
            delegation_touched_files_json: Set(None),
            delegation_touched_files_truncated: Set(None),
            delegation_additions: Set(None),
            delegation_deletions: Set(None),
            delegation_line_counts_complete: Set(None),
            message_count: Set(self.message_count),
            created_at: Set(self.created_at),
            updated_at: Set(self.updated_at),
            deleted_at: Set(None),
            // Pinning is a view preference attached to the row the user pinned,
            // not to the history.
            pinned_at: Set(None),
            awaiting_reply_token: Set(None),
            delegation_run_generation: Set(None),
            last_termination_audit_json: Set(None),
            origin_cwd: Set(self.origin_cwd),
        }
    }
}

/// Re-point a conversation at a DIFFERENT SPELLING of the session it is already
/// bound to — an alias normalization, not a session change.
///
/// The one caller is the detail load, which resolves an ACP-minted UUID to the
/// branch id the parser actually indexes by (Gemini / Cline) and writes it back
/// so later lookups go direct. Because both ids denote the same session, there
/// is no history to preserve and [`bind_external_id`]'s split would be wrong
/// here — it would manufacture a phantom conversation for the old spelling.
///
/// `expected_old` makes that narrow intent explicit and doubles as a CAS: the
/// caller passes the exact id it resolved FROM, so a concurrent rebind (a
/// `SessionStarted` landing mid-parse) leaves the write matching nothing
/// instead of clobbering the newer binding. A no-match is a silent no-op.
pub async fn renormalize_external_id_alias(
    conn: &DatabaseConnection,
    conversation_id: i32,
    expected_old: Option<&str>,
    external_id: String,
) -> Result<(), DbError> {
    use sea_orm::sea_query::Expr;
    let mut query = conversation::Entity::update_many()
        .col_expr(conversation::Column::ExternalId, Expr::value(external_id))
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null());
    query = match expected_old {
        Some(old) => query.filter(conversation::Column::ExternalId.eq(old)),
        None => query.filter(conversation::Column::ExternalId.is_null()),
    };
    query.exec(conn).await?;
    Ok(())
}

pub async fn reparent_folder_conversations(
    conn: &DatabaseConnection,
    from_folder_id: i32,
    to_folder_id: i32,
    origin_cwd: &str,
) -> Result<u64, DbError> {
    use sea_orm::sea_query::Expr;
    let res = conversation::Entity::update_many()
        .col_expr(conversation::Column::FolderId, Expr::value(to_folder_id))
        .col_expr(
            conversation::Column::OriginCwd,
            Expr::value(Some(origin_cwd.to_string())),
        )
        .col_expr(conversation::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(conversation::Column::FolderId.eq(from_folder_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .exec(conn)
        .await?;
    Ok(res.rows_affected)
}

/// Soft-delete a conversation. Returns `true` when a pending auto-title job
/// was removed and active work must be cancelled after commit.
pub async fn soft_delete(conn: &DatabaseConnection, conversation_id: i32) -> Result<bool, DbError> {
    use crate::db::entities::simple_workflow;
    use sea_orm::sea_query::Expr;

    let txn = conn.begin().await?;
    let changed = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::DeletedAt,
            Expr::value(Some(Utc::now())),
        )
        .filter(conversation::Column::Id.eq(conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .exec(&txn)
        .await?;
    if changed.rows_affected == 0 {
        return Err(DbError::Migration(format!(
            "Conversation not found: {conversation_id}"
        )));
    }
    crate::acp::delegation::workflow::resolve_deleted_conversation_completion_attentions_txn(
        &txn,
        conversation_id,
    )
    .await
    .map_err(|error| DbError::Database(sea_orm::DbErr::Custom(error.to_string())))?;
    simple_workflow::Entity::delete_by_id(conversation_id)
        .exec(&txn)
        .await?;
    let removed = cancel_job(&txn, conversation_id).await?;
    txn.commit().await?;
    Ok(removed)
}

/// Wire-stable row-level code written by provisional Step-1 terminalization.
pub const PROVISIONAL_ADMISSION_REJECTED: &str = "provisional_admission_rejected";

/// Outcome of [`terminalize_provisional_child`] when the conditional UPDATE
/// succeeds or is already in the expected provisional-failed shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionalTerminalizeOutcome {
    /// Row transitioned running → failed + `provisional_admission_rejected`.
    Terminalized,
    /// Already failed/canceled as provisional (idempotent Step 1).
    AlreadyTerminal,
}

/// Outcome of [`soft_delete_provisional_child`] when the guarded predicate
/// matches or the row is already soft-deleted in the expected shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisionalSoftDeleteOutcome {
    SoftDeleted,
    AlreadySoftDeleted,
}

fn provisional_no_run_fence_expr() -> sea_orm::sea_query::SimpleExpr {
    use sea_orm::sea_query::Expr;
    Expr::cust(
        "NOT EXISTS (SELECT 1 FROM delegation_task_runs AS r \
         WHERE r.child_conversation_id = conversation.id)",
    )
}

async fn provisional_child_has_run(
    conn: &DatabaseConnection,
    child_id: i32,
) -> Result<bool, DbError> {
    use crate::db::entities::delegation_task_run;
    Ok(delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ChildConversationId.eq(child_id))
        .one(conn)
        .await?
        .is_some())
}

/// Parent-end codes that may have been projected onto a provisional shell when
/// durable parent-end settle raced gen-1 admit before abandon+compensate.
fn is_parent_end_projected_error_code(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("parent_canceled")
            | Some("parent_turn_failed")
            | Some("join_abandoned")
            | Some("parent_disconnected")
            | Some("parent_ended")
            | Some("canceled")
    )
}

/// Step 1 of provisional compensation: atomic terminalization under a no-run
/// / provisional-shape fence. Never soft-deletes.
///
/// On `rows_affected == 0`, disambiguates:
/// - already provisional-terminal → [`ProvisionalTerminalizeOutcome::AlreadyTerminal`]
/// - parent-end canceled provisional shell (run already abandoned) → rewrite to
///   `provisional_admission_rejected` then success
/// - acquired run → `DbError::Validation` (invariant; caller must not Step 2)
/// - other → `DbError::Validation` / `NotFound`
pub async fn terminalize_provisional_child(
    conn: &DatabaseConnection,
    child_id: i32,
) -> Result<ProvisionalTerminalizeOutcome, DbError> {
    use sea_orm::sea_query::Expr;

    let now = Utc::now();
    let changed = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::DelegationTaskStatus,
            Expr::value(conversation::DelegationTaskStatus::Failed),
        )
        .col_expr(
            conversation::Column::DelegationErrorCode,
            Expr::value(PROVISIONAL_ADMISSION_REJECTED),
        )
        .col_expr(conversation::Column::DelegationFinishedAt, Expr::value(now))
        .filter(conversation::Column::Id.eq(child_id))
        .filter(conversation::Column::Kind.eq(ConversationKind::Delegate))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            conversation::Column::DelegationTaskStatus
                .eq(conversation::DelegationTaskStatus::Running),
        )
        .filter(conversation::Column::DelegationFinishedAt.is_null())
        .filter(conversation::Column::ExternalId.is_null())
        .filter(provisional_no_run_fence_expr())
        .exec(conn)
        .await?;

    if changed.rows_affected > 0 {
        return Ok(ProvisionalTerminalizeOutcome::Terminalized);
    }

    let row = conversation::Entity::find_by_id(child_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("Conversation not found: {child_id}")))?;

    if provisional_child_has_run(conn, child_id).await? {
        return Err(DbError::Validation(format!(
            "provisional terminalize invariant: child {child_id} has an acquired run"
        )));
    }

    let is_provisional_terminal = row.kind == ConversationKind::Delegate
        && row.deleted_at.is_none()
        && row.external_id.is_none()
        && matches!(
            row.delegation_task_status,
            Some(conversation::DelegationTaskStatus::Failed)
                | Some(conversation::DelegationTaskStatus::Canceled)
        )
        && row.delegation_error_code.as_deref() == Some(PROVISIONAL_ADMISSION_REJECTED)
        && row.delegation_finished_at.is_some();

    if is_provisional_terminal {
        return Ok(ProvisionalTerminalizeOutcome::AlreadyTerminal);
    }

    // Durable parent-end settle can project a pure provisional shell to
    // canceled before abandon reclaims the run. With the run gone, rewrite
    // that shell to the provisional rejection code so Step-2 can hide it.
    let is_parent_end_canceled_provisional = row.kind == ConversationKind::Delegate
        && row.deleted_at.is_none()
        && row.external_id.is_none()
        && row.delegation_task_status == Some(conversation::DelegationTaskStatus::Canceled)
        && row.delegation_finished_at.is_some()
        && is_parent_end_projected_error_code(row.delegation_error_code.as_deref());

    if is_parent_end_canceled_provisional {
        let rewritten = conversation::Entity::update_many()
            .col_expr(
                conversation::Column::DelegationTaskStatus,
                Expr::value(conversation::DelegationTaskStatus::Failed),
            )
            .col_expr(
                conversation::Column::DelegationErrorCode,
                Expr::value(PROVISIONAL_ADMISSION_REJECTED),
            )
            .filter(conversation::Column::Id.eq(child_id))
            .filter(conversation::Column::Kind.eq(ConversationKind::Delegate))
            .filter(conversation::Column::DeletedAt.is_null())
            .filter(
                conversation::Column::DelegationTaskStatus
                    .eq(conversation::DelegationTaskStatus::Canceled),
            )
            .filter(conversation::Column::ExternalId.is_null())
            .filter(provisional_no_run_fence_expr())
            .exec(conn)
            .await?;
        if rewritten.rows_affected > 0 {
            return Ok(ProvisionalTerminalizeOutcome::Terminalized);
        }
        // Concurrent rewrite/soft-delete — re-check idempotent provisional shape.
        let again = conversation::Entity::find_by_id(child_id)
            .one(conn)
            .await?
            .ok_or_else(|| DbError::NotFound(format!("Conversation not found: {child_id}")))?;
        if again.kind == ConversationKind::Delegate
            && again.external_id.is_none()
            && matches!(
                again.delegation_task_status,
                Some(conversation::DelegationTaskStatus::Failed)
                    | Some(conversation::DelegationTaskStatus::Canceled)
            )
            && again.delegation_error_code.as_deref() == Some(PROVISIONAL_ADMISSION_REJECTED)
        {
            return Ok(ProvisionalTerminalizeOutcome::AlreadyTerminal);
        }
    }

    Err(DbError::Validation(format!(
        "provisional terminalize unexpected state for child {child_id}: \
         kind={:?} status={:?} error_code={:?} finished_at={:?} external_id={:?} deleted_at={:?}",
        row.kind,
        row.delegation_task_status,
        row.delegation_error_code,
        row.delegation_finished_at,
        row.external_id,
        row.deleted_at
    )))
}

/// Step 2 of provisional compensation: guarded soft-delete retaining the
/// no-run / provisional-failed fence. **Not** a generic id-only soft-delete.
///
/// On zero rows: already soft-deleted expected shape → success; acquired run →
/// invariant validation error (leave visible); other → validation error.
pub async fn soft_delete_provisional_child(
    conn: &DatabaseConnection,
    child_id: i32,
) -> Result<ProvisionalSoftDeleteOutcome, DbError> {
    use sea_orm::sea_query::Expr;

    let now = Utc::now();
    let txn = conn.begin().await?;
    let changed = conversation::Entity::update_many()
        .col_expr(conversation::Column::DeletedAt, Expr::value(Some(now)))
        .filter(conversation::Column::Id.eq(child_id))
        .filter(conversation::Column::Kind.eq(ConversationKind::Delegate))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(
            conversation::Column::DelegationTaskStatus
                .eq(conversation::DelegationTaskStatus::Failed),
        )
        .filter(conversation::Column::DelegationErrorCode.eq(PROVISIONAL_ADMISSION_REJECTED))
        .filter(conversation::Column::ExternalId.is_null())
        .filter(provisional_no_run_fence_expr())
        .exec(&txn)
        .await?;

    if changed.rows_affected > 0 {
        let _removed = cancel_job(&txn, child_id).await?;
        txn.commit().await?;
        return Ok(ProvisionalSoftDeleteOutcome::SoftDeleted);
    }
    // No rows matched — release the empty transaction before follow-up reads.
    txn.commit().await?;

    let row = conversation::Entity::find_by_id(child_id)
        .one(conn)
        .await?
        .ok_or_else(|| DbError::NotFound(format!("Conversation not found: {child_id}")))?;

    if provisional_child_has_run(conn, child_id).await? {
        return Err(DbError::Validation(format!(
            "provisional soft-delete invariant: child {child_id} has an acquired run"
        )));
    }

    let already_hidden_provisional = row.kind == ConversationKind::Delegate
        && row.deleted_at.is_some()
        && row.external_id.is_none()
        && row.delegation_task_status == Some(conversation::DelegationTaskStatus::Failed)
        && row.delegation_error_code.as_deref() == Some(PROVISIONAL_ADMISSION_REJECTED);

    if already_hidden_provisional {
        return Ok(ProvisionalSoftDeleteOutcome::AlreadySoftDeleted);
    }

    Err(DbError::Validation(format!(
        "provisional soft-delete unexpected state for child {child_id}: \
         kind={:?} status={:?} error_code={:?} deleted_at={:?} external_id={:?}",
        row.kind,
        row.delegation_task_status,
        row.delegation_error_code,
        row.deleted_at,
        row.external_id
    )))
}

fn parse_agent_type(s: &str) -> AgentType {
    match AgentType::from_wire(s) {
        Some(at) => at,
        None => {
            // DB has a value the enum does not recognise (manual edit or removed variant).
            // Fall back to ClaudeCode so the row stays readable, but log so resume-as-wrong-agent
            // regressions are traceable.
            tracing::warn!(
                "[conversation_service] unknown agent_type {s:?} in DB, falling back to ClaudeCode"
            );
            AgentType::ClaudeCode
        }
    }
}

fn conv_to_summary(r: conversation::Model) -> DbConversationSummary {
    use crate::acp::delegation::runtime_stats::{
        decode_persisted_runtime_stats, PersistedRuntimeStatsColumns,
    };

    let status = serde_json::to_value(&r.status)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", r.status));
    let conversation_id = r.id;
    let delegation_runtime_stats =
        match decode_persisted_runtime_stats(PersistedRuntimeStatsColumns {
            started_at: r.delegation_started_at,
            finished_at: r.delegation_finished_at,
            tool_call_count: r.delegation_tool_call_count,
            edit_tool_call_count: r.delegation_edit_tool_call_count,
            touched_files_json: r.delegation_touched_files_json.as_deref(),
            touched_files_truncated: r.delegation_touched_files_truncated,
            additions: r.delegation_additions,
            deletions: r.delegation_deletions,
            line_counts_complete: r.delegation_line_counts_complete,
        }) {
            Ok(stats) => stats,
            Err(err) => {
                tracing::warn!(
                    conversation_id,
                    error = %err,
                    "[conversation_service] failed to decode delegation_runtime_stats"
                );
                None
            }
        };
    DbConversationSummary {
        id: r.id,
        folder_id: r.folder_id,
        title: r.title,
        title_locked: r.title_locked,
        auto_title_finalized: r.auto_title_finalized,
        agent_type: parse_agent_type(&r.agent_type),
        status,
        awaiting_reply_token: r.awaiting_reply_token,
        kind: r.kind.clone(),
        model: r.model,
        git_branch: r.git_branch,
        external_id: r.external_id,
        message_count: r.message_count as u32,
        // Pure mapper: `child_count` is backfilled by `fill_child_counts` over
        // the returned set, never queried per-row here.
        child_count: 0,
        created_at: r.created_at,
        updated_at: r.updated_at,
        pinned_at: r.pinned_at,
        parent_id: r.parent_id,
        parent_tool_use_id: r.parent_tool_use_id,
        delegation_call_id: r.delegation_call_id,
        delegation_route_override: parse_route_override(r.delegation_route_override),
        delegation_task_status: r.delegation_task_status,
        delegation_error_code: r.delegation_error_code,
        delegation_started_at: r.delegation_started_at,
        delegation_finished_at: r.delegation_finished_at,
        delegation_runtime_stats,
        // Pure mapper: open attention is bulk-filled by
        // `fill_open_delegation_attention` over the returned set.
        delegation_attention_request: None,
        origin_cwd: r.origin_cwd,
    }
}

/// Attach at most one open attention request per summary that carries a
/// non-empty `delegation_call_id`. ONE SeaORM query over all task ids —
/// never N+1. Ordered by `CreatedAt ASC, RequestId ASC` so the earliest open
/// request wins when duplicates exist.
pub async fn fill_open_delegation_attention(
    conn: &DatabaseConnection,
    summaries: &mut [DbConversationSummary],
) -> Result<(), DbError> {
    use crate::acp::delegation::attention::AttentionRequestSummary;
    use crate::db::entities::delegation_attention_request;

    if summaries.is_empty() {
        return Ok(());
    }
    let task_ids: Vec<String> = summaries
        .iter()
        .filter_map(|s| {
            s.delegation_call_id
                .as_ref()
                .filter(|id| !id.is_empty())
                .cloned()
        })
        .collect();
    if task_ids.is_empty() {
        return Ok(());
    }
    let rows = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::TaskId.is_in(task_ids))
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .order_by_asc(delegation_attention_request::Column::CreatedAt)
        .order_by_asc(delegation_attention_request::Column::RequestId)
        .all(conn)
        .await?;
    let mut by_task: std::collections::HashMap<String, AttentionRequestSummary> =
        std::collections::HashMap::new();
    for row in rows {
        by_task
            .entry(row.task_id.clone())
            .or_insert_with(|| AttentionRequestSummary {
                request_id: row.request_id,
                task_id: row.task_id,
                message: row.message,
                created_at: row.created_at,
            });
    }
    for summary in summaries.iter_mut() {
        if let Some(task_id) = summary.delegation_call_id.as_ref() {
            if let Some(req) = by_task.get(task_id) {
                summary.delegation_attention_request = Some(req.clone());
            }
        }
    }
    Ok(())
}

/// Backfill each summary's `child_count` with its number of direct, non-deleted
/// delegation children using ONE `GROUP BY` aggregate over the whole set (never
/// per-row — no N+1). `child_count > 0` iff `list_children` would return rows
/// (same `parent_id == id AND deleted_at IS NULL` predicate), so the sidebar
/// chevron neither expands to nothing nor hides a real subtree. No-op on an
/// empty slice (avoids an `IN ()`).
async fn fill_child_counts(
    conn: &DatabaseConnection,
    summaries: &mut [DbConversationSummary],
) -> Result<(), DbError> {
    if summaries.is_empty() {
        return Ok(());
    }
    let ids: Vec<i32> = summaries.iter().map(|s| s.id).collect();
    let pairs: Vec<(Option<i32>, i64)> = conversation::Entity::find()
        .select_only()
        .column(conversation::Column::ParentId)
        .column_as(conversation::Column::Id.count(), "cnt")
        .filter(conversation::Column::ParentId.is_in(ids))
        .filter(conversation::Column::DeletedAt.is_null())
        .group_by(conversation::Column::ParentId)
        .into_tuple()
        .all(conn)
        .await?;
    let mut counts: std::collections::HashMap<i32, u32> =
        std::collections::HashMap::with_capacity(pairs.len());
    for (parent_id, cnt) in pairs {
        if let Some(pid) = parent_id {
            counts.insert(pid, cnt.max(0) as u32);
        }
    }
    for s in summaries.iter_mut() {
        s.child_count = counts.get(&s.id).copied().unwrap_or(0);
    }
    Ok(())
}

pub async fn get_by_id(
    conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<DbConversationSummary, DbError> {
    let conv = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?
        .ok_or_else(|| DbError::Migration(format!("Conversation not found: {conversation_id}")))?;

    let mut summary = conv_to_summary(conv);
    fill_child_counts(conn, std::slice::from_mut(&mut summary)).await?;
    Ok(summary)
}

/// Look up a child conversation by its `delegation_call_id` (the broker's
/// `task_id`). Returns `Ok(None)` when no row matches — used by the broker's
/// `ChildStatusLookup` DB fallback to recover a delegation task's terminal
/// status after its in-memory result was evicted from the completed-cache.
/// Unlike [`get_by_id`] this never errors hard on "not found": a missing row
/// is a legitimate "unknown task" answer.
pub async fn get_by_delegation_call_id(
    conn: &DatabaseConnection,
    delegation_call_id: &str,
) -> Result<Option<DbConversationSummary>, DbError> {
    let conv = conversation::Entity::find()
        .filter(conversation::Column::DelegationCallId.eq(delegation_call_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await?;
    Ok(conv.map(conv_to_summary))
}

pub async fn list_by_folder(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    status: Option<String>,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let mut query = conversation::Entity::find()
        .filter(conversation::Column::FolderId.eq(folder_id))
        .filter(conversation::Column::DeletedAt.is_null());

    // Filter by agent_type
    if let Some(ref at) = agent_type {
        let at_str = serde_json::to_value(at)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        query = query.filter(conversation::Column::AgentType.eq(at_str));
    }

    // Search by title
    if let Some(ref s) = search {
        if !s.is_empty() {
            query = query.filter(conversation::Column::Title.contains(s));
        }
    }

    // Filter by status
    if let Some(ref st) = status {
        if let Ok(status_enum) = serde_json::from_value::<conversation::ConversationStatus>(
            serde_json::Value::String(st.clone()),
        ) {
            query = query.filter(conversation::Column::Status.eq(status_enum));
        }
    }

    // Activity order (matches list_all / sidebar): most recently updated first.
    query = match sort_by.as_deref() {
        Some("oldest") => query.order_by_asc(conversation::Column::UpdatedAt),
        _ => query.order_by_desc(conversation::Column::UpdatedAt),
    };

    let rows = query.all(conn).await?;

    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_child_counts(conn, &mut summaries).await?;

    Ok(summaries)
}

/// List conversations across folders. When `folder_ids` is `None`, queries all
/// When `folder_ids` is provided, results are scoped to that set. Otherwise
/// returns conversations across every non-deleted folder (open or not).
///
/// `include_children` controls visibility of delegation sub-sessions. When
/// `false` (the default for the top-level list), rows whose `parent_id` is
/// non-null are filtered out — they belong to their parent's tool-call view,
/// not the workspace conversation list. Rows with `kind = 'loop'` are always
/// excluded — they belong to the loops workbench.
pub async fn list_all(
    conn: &DatabaseConnection,
    folder_ids: Option<Vec<i32>>,
    agent_type: Option<AgentType>,
    search: Option<String>,
    sort_by: Option<String>,
    status: Option<String>,
    include_children: bool,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let mut query = conversation::Entity::find().filter(conversation::Column::DeletedAt.is_null());

    // Loop-engineering runs never surface in the workspace conversation list —
    // their entry point is the loops workbench.
    query = query.filter(conversation::Column::Kind.ne(ConversationKind::Loop));

    if !include_children {
        query = query.filter(conversation::Column::ParentId.is_null());
    }

    match folder_ids {
        Some(ids) if !ids.is_empty() => {
            query = query.filter(conversation::Column::FolderId.is_in(ids));
        }
        _ => {
            // Exclude conversations whose folder was soft-deleted.
            let active_folder_ids: Vec<i32> = folder::Entity::find()
                .filter(folder::Column::DeletedAt.is_null())
                .all(conn)
                .await?
                .into_iter()
                .map(|m| m.id)
                .collect();
            if active_folder_ids.is_empty() {
                return Ok(Vec::new());
            }
            query = query.filter(conversation::Column::FolderId.is_in(active_folder_ids));
        }
    }

    if let Some(ref at) = agent_type {
        let at_str = serde_json::to_value(at)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default();
        query = query.filter(conversation::Column::AgentType.eq(at_str));
    }

    if let Some(ref s) = search {
        if !s.is_empty() {
            query = query.filter(conversation::Column::Title.contains(s));
        }
    }

    if let Some(ref st) = status {
        if let Ok(status_enum) = serde_json::from_value::<conversation::ConversationStatus>(
            serde_json::Value::String(st.clone()),
        ) {
            query = query.filter(conversation::Column::Status.eq(status_enum));
        }
    }

    query = match sort_by.as_deref() {
        Some("oldest") => query.order_by_asc(conversation::Column::UpdatedAt),
        _ => query.order_by_desc(conversation::Column::UpdatedAt),
    };

    let rows = query.all(conn).await?;
    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_child_counts(conn, &mut summaries).await?;
    Ok(summaries)
}

/// List delegation children of a single parent conversation, most recently
/// active first (`updated_at` DESC), matching the sidebar's activity ordering
/// so a sub-agent that just replied surfaces under its parent. The only other
/// consumer (`inject_delegation_meta`) keys these by `parent_tool_use_id` and
/// is order-agnostic. Returns rows where `parent_id == parent_conversation_id`.
/// Soft-deleted children are filtered out so a removed sub-session stays hidden
/// in the parent's tool-call view too.
pub async fn list_children(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Vec<DbConversationSummary>, DbError> {
    let rows = conversation::Entity::find()
        .filter(conversation::Column::ParentId.eq(parent_conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .order_by_desc(conversation::Column::UpdatedAt)
        // Explicit id tie-break so same-timestamp siblings are deterministic and
        // match the frontend re-sort (which tie-breaks id DESC): the raw fetch
        // snapshot and a live-inserted child then land in the same order.
        .order_by_desc(conversation::Column::Id)
        .all(conn)
        .await?;
    let mut summaries: Vec<DbConversationSummary> = rows.into_iter().map(conv_to_summary).collect();
    fill_child_counts(conn, &mut summaries).await?;
    fill_open_delegation_attention(conn, &mut summaries).await?;
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};

    /// Build a parent + a delegation child for filter assertions.
    async fn seed_parent_with_child(conn: &DatabaseConnection, folder_id: i32) -> (i32, i32) {
        let parent = create(
            conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("P".into()),
            None,
        )
        .await
        .expect("parent");
        let link = DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-1".into(),
            delegation_call_id: "call-1".into(),
        };
        let child = create_with_delegation(
            conn,
            folder_id,
            AgentType::Codex,
            Some("C".into()),
            None,
            Some(link),
        )
        .await
        .expect("child");
        (parent.id, child.id)
    }

    #[tokio::test]
    async fn list_all_excludes_children_by_default() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-default").await;
        let (parent, _child) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert!(ids.contains(&parent), "parent must remain visible: {ids:?}");
        assert_eq!(
            rows.len(),
            1,
            "expected only the parent, got {} rows: {ids:?}",
            rows.len()
        );
    }

    #[tokio::test]
    async fn list_all_includes_children_when_requested() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-on").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_all(&db.conn, None, None, None, None, None, true)
            .await
            .expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert!(
            ids.contains(&parent) && ids.contains(&child),
            "both parent + child must appear when include_children=true, got: {ids:?}",
        );
    }

    #[tokio::test]
    async fn list_children_returns_only_matching_parent() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-only").await;
        let (parent_a, child_a) = seed_parent_with_child(&db.conn, folder).await;
        let (_parent_b, _child_b) = seed_parent_with_child(&db.conn, folder).await;

        let rows = list_children(&db.conn, parent_a).await.expect("list");
        assert_eq!(
            rows.len(),
            1,
            "expected 1 child of parent_a, got {}",
            rows.len()
        );
        assert_eq!(rows[0].id, child_a);
        assert_eq!(rows[0].parent_id, Some(parent_a));
    }

    #[tokio::test]
    async fn fill_open_delegation_attention_attaches_at_most_one_open_per_task() {
        use crate::db::entities::delegation_attention_request;
        use sea_orm::{ActiveModelTrait, Set};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-fill-open-attention").await;
        let (parent_a, child_a) = seed_parent_with_child(&db.conn, folder).await;
        // Second parent/child uses the same seed helper → call-1 again would
        // collide; create an extra sibling with a distinct task id.
        let parent_b = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("P2".into()),
            None,
        )
        .await
        .expect("parent_b");
        let child_b = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("C2".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_b.id,
                parent_tool_use_id: "tu-2".into(),
                delegation_call_id: "call-2".into(),
            }),
        )
        .await
        .expect("child_b");
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-07-17T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // One open row per task and attention kind.
        for (req_id, task_id, p, c) in [
            ("req-a", "call-1", parent_a, child_a),
            ("req-b", "call-2", parent_b.id, child_b.id),
        ] {
            let row = delegation_attention_request::ActiveModel {
                request_id: Set(req_id.into()),
                task_id: Set(task_id.into()),
                parent_conversation_id: Set(p),
                child_conversation_id: Set(Some(c)),
                child_tool_call_id: Set(Some(format!("tc-{req_id}"))),
                status: Set("open".into()),
                message: Set(format!("msg-{req_id}")),
                reply: Set(None),
                resolution_code: Set(None),
                created_at: Set(t0),
                resolved_at: Set(None),
                kind: Set(delegation_attention_request::AttentionKind::ChildQuestion),
                latest_run_id: Set(None),
                node_id: Set(None),
                payload_json: Set(None),
                resolution_json: Set(None),
                captured_scope_digest: Set(None),
            };
            row.insert(&db.conn).await.expect("insert attention");
        }
        // Bulk fill over two unrelated parent lists proves one query path per
        // list_children call (not N+1 per child).
        let rows_a = list_children(&db.conn, parent_a).await.expect("list a");
        let rows_b = list_children(&db.conn, parent_b.id).await.expect("list b");
        assert_eq!(
            rows_a[0]
                .delegation_attention_request
                .as_ref()
                .unwrap()
                .request_id,
            "req-a"
        );
        assert_eq!(
            rows_b[0]
                .delegation_attention_request
                .as_ref()
                .unwrap()
                .message,
            "msg-req-b"
        );

        // Explicit fill over a mixed slice still attaches by task id once.
        let mut mixed = vec![rows_a[0].clone(), rows_b[0].clone()];
        // Clear then refill.
        mixed[0].delegation_attention_request = None;
        mixed[1].delegation_attention_request = None;
        fill_open_delegation_attention(&db.conn, &mut mixed)
            .await
            .expect("bulk fill");
        assert_eq!(
            mixed[0]
                .delegation_attention_request
                .as_ref()
                .unwrap()
                .request_id,
            "req-a"
        );
        assert_eq!(
            mixed[1]
                .delegation_attention_request
                .as_ref()
                .unwrap()
                .request_id,
            "req-b"
        );
    }

    #[tokio::test]
    async fn list_children_orders_newest_first() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-order").await;
        let parent = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("P".into()),
            None,
        )
        .await
        .expect("parent");
        // Two children created oldest → newest under the same parent.
        let first = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("first".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("first child");
        let second = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("second".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-2".into(),
                delegation_call_id: "call-2".into(),
            }),
        )
        .await
        .expect("second child");

        // The sidebar shows sub-sessions newest-first so a freshly-spawned
        // sub-agent surfaces right under its parent.
        let rows = list_children(&db.conn, parent.id).await.expect("list");
        let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![second.id, first.id],
            "newest child must come first"
        );
    }

    #[tokio::test]
    async fn child_count_reflects_direct_children() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-direct").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        // The root listing carries the parent's direct-child count so the
        // sidebar knows to show a chevron; the leaf child carries 0.
        let roots = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let parent_row = roots.iter().find(|r| r.id == parent).expect("parent row");
        assert_eq!(parent_row.child_count, 1, "parent has one delegation child");

        let children = list_children(&db.conn, parent).await.expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child);
        assert_eq!(children[0].child_count, 0, "leaf child has no children");
    }

    #[tokio::test]
    async fn child_count_counts_grandchildren_for_nested_chevron() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-nested").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        // Delegate a grandchild from the child so the child itself becomes
        // expandable one level down.
        let link = DelegationLink {
            parent_conversation_id: child,
            parent_tool_use_id: "tu-2".into(),
            delegation_call_id: "call-2".into(),
        };
        create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("G".into()),
            None,
            Some(link),
        )
        .await
        .expect("grandchild");

        // list_children(parent) must report the child's OWN child_count (1) so
        // the recursive chevron appears on the nested row.
        let children = list_children(&db.conn, parent).await.expect("children");
        let child_row = children.iter().find(|r| r.id == child).expect("child row");
        assert_eq!(child_row.child_count, 1, "child has one grandchild");
    }

    #[tokio::test]
    async fn child_count_excludes_soft_deleted_children() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-child-count-deleted").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        soft_delete(&db.conn, child)
            .await
            .expect("soft delete child");

        // A removed sub-session must not keep the parent's chevron alive: the
        // aggregate filters deleted_at IS NULL, matching list_children.
        let roots = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        let parent_row = roots.iter().find(|r| r.id == parent).expect("parent row");
        assert_eq!(
            parent_row.child_count, 0,
            "soft-deleted child must not be counted"
        );
    }

    #[tokio::test]
    async fn update_pin_sets_and_clears_without_bumping_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-update-pin").await;
        let conv = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("c".into()),
            None,
        )
        .await
        .expect("create");

        // Freshly created rows are unpinned, and the summary projection carries
        // the field through (conv_to_summary mapping).
        let before = get_by_id(&db.conn, conv.id).await.expect("get before");
        assert!(
            before.pinned_at.is_none(),
            "new conversation must be unpinned"
        );
        let updated_at_before = before.updated_at;

        // Pin → pinned_at populated; updated_at must NOT move (pin is a view
        // preference, not activity).
        update_pin(&db.conn, conv.id, true).await.expect("pin");
        let pinned = get_by_id(&db.conn, conv.id).await.expect("get pinned");
        assert!(
            pinned.pinned_at.is_some(),
            "pinned_at must be set after pin"
        );
        assert_eq!(
            pinned.updated_at, updated_at_before,
            "pinning must not bump updated_at"
        );

        // Unpin → pinned_at cleared back to NULL; updated_at still unchanged.
        update_pin(&db.conn, conv.id, false).await.expect("unpin");
        let unpinned = get_by_id(&db.conn, conv.id).await.expect("get unpinned");
        assert!(
            unpinned.pinned_at.is_none(),
            "pinned_at must clear after unpin"
        );
        assert_eq!(
            unpinned.updated_at, updated_at_before,
            "unpinning must not bump updated_at"
        );
    }

    #[tokio::test]
    async fn list_children_excludes_soft_deleted() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-list-children-soft-del").await;
        let (parent, child) = seed_parent_with_child(&db.conn, folder).await;

        soft_delete(&db.conn, child).await.expect("soft delete");

        let rows = list_children(&db.conn, parent).await.expect("list");
        assert!(
            rows.is_empty(),
            "soft-deleted child must not appear: {rows:?}"
        );
    }

    #[tokio::test]
    async fn create_leaves_title_unlocked() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-unlocked").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("hi".into()),
            None,
        )
        .await
        .expect("create");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert!(
            !summary.title_locked,
            "new conversation must start unlocked"
        );
    }

    #[tokio::test]
    async fn update_title_locks_the_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-lock").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "My name".into())
            .await
            .expect("rename");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("My name"));
        assert!(summary.title_locked, "manual rename must lock the title");
    }

    /// Read a row straight from the table — `get_by_id` returns a summary and
    /// filters deleted rows, but these tests assert on raw persisted columns.
    async fn raw_row(conn: &DatabaseConnection, id: i32) -> conversation::Model {
        conversation::Entity::find_by_id(id)
            .one(conn)
            .await
            .expect("query")
            .expect("row")
    }

    /// The single live row (if any) holding `external_id`, whatever its id.
    async fn rows_holding(
        conn: &DatabaseConnection,
        external_id: &str,
    ) -> Vec<conversation::Model> {
        conversation::Entity::find()
            .filter(conversation::Column::ExternalId.eq(external_id))
            .filter(conversation::Column::DeletedAt.is_null())
            .all(conn)
            .await
            .expect("query by external_id")
    }

    #[tokio::test]
    async fn bind_external_id_preserves_the_outgoing_session_on_a_new_row() {
        // codeg#500 in miniature: a row bound to S1 is handed S2 (a fresh
        // session that knows nothing about it). S1 must survive on a row of its
        // own, wearing the identity the user recognises, or it disappears from
        // a sidebar that is built purely from DB rows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-preserve").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("nouveauté de cette version".into()),
            Some("main".into()),
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");
        let before = raw_row(&db.conn, row.id).await;

        let preserved_id = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("rebind")
            .expect("a session change with nothing holding S1 must preserve it");

        let current = raw_row(&db.conn, row.id).await;
        assert_eq!(
            current.external_id.as_deref(),
            Some("S2"),
            "the live row advances to the new session"
        );

        let preserved = raw_row(&db.conn, preserved_id).await;
        assert_eq!(preserved.external_id.as_deref(), Some("S1"));
        assert_eq!(
            preserved.title.as_deref(),
            Some("nouveauté de cette version"),
            "the preserved row must keep the name the user recognises"
        );
        assert_eq!(preserved.folder_id, before.folder_id);
        assert_eq!(preserved.agent_type, before.agent_type);
        assert_eq!(preserved.git_branch.as_deref(), Some("main"));
        assert_eq!(
            preserved.created_at, before.created_at,
            "created_at is carried, not stamped now — the preserved row IS the \
             old conversation and must not read as freshly created"
        );
        assert_eq!(
            preserved.updated_at, before.updated_at,
            "updated_at is carried so the sidebar's activity ordering keeps the \
             conversation in place instead of floating it to the top. This also \
             pins the self-assigning claim write: bumping updated_at to claim \
             the writer lock would make the SELECT read the clobbered value."
        );
        assert!(preserved.deleted_at.is_none());
        assert!(
            preserved.pinned_at.is_none(),
            "pinning belongs to the row the user pinned, not to the history"
        );
    }

    #[tokio::test]
    async fn bind_external_id_advances_in_place_when_the_session_is_carried_forward() {
        // A custom agent that keeps sessions in memory forgets them on every
        // restart. codeg's answer is to open a fresh session and link the
        // transcripts (`continues_from`), and both the reader and the generic
        // parser then render the chain as ONE conversation — the parser even
        // hides the superseded ids, precisely so the conversation isn't listed
        // twice.
        //
        // So a bind whose incoming session carries the outgoing one forward
        // must NOT split. Splitting would clone the conversation in the sidebar
        // on every restart, with each copy rendering a longer prefix of the
        // same history (S1, S1+S2, S1+S2+S3...), and double-count its tokens.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-continues").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Custom("goose"),
            Some("Long-running chat".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        // Restart 1: S2 continues S1.
        let preserved = bind_external_id(&db.conn, row.id, "S2", &["S1".to_string()])
            .await
            .expect("continuation bind");
        assert_eq!(
            preserved, None,
            "a carried-forward session is the same conversation, not a new one"
        );

        // Restart 2: S3 continues S2 (and transitively S1).
        let preserved = bind_external_id(
            &db.conn,
            row.id,
            "S3",
            &["S2".to_string(), "S1".to_string()],
        )
        .await
        .expect("second continuation bind");
        assert_eq!(preserved, None);

        let rows = conversation::Entity::find()
            .filter(conversation::Column::DeletedAt.is_null())
            .all(&db.conn)
            .await
            .expect("list");
        assert_eq!(
            rows.len(),
            1,
            "two restarts must leave ONE conversation, got {rows:?}"
        );
        assert_eq!(rows[0].external_id.as_deref(), Some("S3"));

        // The guard is not over-broad: an UNRELATED session on the same row is
        // still split off, which is the bug this whole primitive exists for.
        let preserved = bind_external_id(&db.conn, row.id, "UNRELATED", &[])
            .await
            .expect("unrelated bind")
            .expect("an unrelated session must still preserve the history");
        assert_eq!(
            raw_row(&db.conn, preserved).await.external_id.as_deref(),
            Some("S3")
        );
    }

    #[tokio::test]
    async fn bind_external_id_survives_the_unique_index() {
        // `idx_conversation_external_agent` is UNIQUE over
        // `(external_id, agent_type)`, so the preserving row can only be
        // inserted after the current row has released S1. Insert-first would
        // fail the constraint every single time — this test exists so a
        // refactor that "tidies" the two writes into the intuitive order gets
        // caught here rather than in production.
        //
        // The migrations run in `fresh_in_memory_db`, so the index is real.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-unique").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("the split must not trip the unique index")
            .expect("preserved");

        assert_eq!(
            rows_holding(&db.conn, "S1").await.len(),
            1,
            "exactly one row holds the old session"
        );
        assert_eq!(
            rows_holding(&db.conn, "S2").await.len(),
            1,
            "exactly one row holds the new session"
        );
    }

    #[tokio::test]
    async fn bind_external_id_refuses_a_session_another_row_already_holds() {
        // The incoming id is exposed to the unique index in BOTH branches, and
        // neither used to check for a holder, so each of these used to raise a
        // raw `UNIQUE constraint failed`.
        //
        // Two things are asserted together because they only make sense
        // together: the DB is left completely untouched, AND the caller is told
        // so. `Conflict` rather than `Ok(None)` is the load-bearing half —
        // `send_prompt_linked` reads any `Ok` as "the row is bound now" and
        // goes on to dispatch the prompt, which would then land in the HOLDER
        // row's transcript while every event named this row.
        for repointing in [false, true] {
            let db = fresh_in_memory_db().await;
            let folder = seed_folder(&db, "/tmp/codeg-bind-conflict").await;
            let holder = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("holder");
            bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
                .await
                .expect("seed holder");

            let row = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("create");
            // `false` exercises the plain-bind branch (a fresh row whose
            // connection resumed a session codeg already indexed); `true` the
            // split branch, which would otherwise fail on the release write
            // before ever reaching its INSERT.
            if repointing {
                bind_external_id(&db.conn, row.id, "S_OWN", &[])
                    .await
                    .expect("seed own session");
            }

            let err = bind_external_id(&db.conn, row.id, "S_SHARED", &[])
                .await
                .expect_err("a taken session id must be refused, not reported bound");

            assert!(
                matches!(err, DbError::Conflict(_)),
                "must be distinguishable from transient contention, got {err:?}"
            );
            assert!(
                err.to_string().contains(&holder.id.to_string()),
                "the message must name the row that actually holds the session, got {err}"
            );
            assert_eq!(
                raw_row(&db.conn, row.id).await.external_id.as_deref(),
                repointing.then_some("S_OWN"),
                "the refused row keeps exactly the session it had"
            );
            let holders = rows_holding(&db.conn, "S_SHARED").await;
            assert_eq!(holders.len(), 1, "the original holder is untouched");
            assert_eq!(holders[0].id, holder.id);
        }
    }

    #[tokio::test]
    async fn bind_external_id_refuses_a_session_a_soft_deleted_row_holds() {
        // The unique index carries no `deleted_at` predicate, so a soft-deleted
        // row still occupies its id — a lookup that filtered on `deleted_at IS
        // NULL` would miss it and walk straight into the constraint. Refused on
        // the same terms as a live holder rather than having its id taken,
        // matching `import_one` / `refresh_existing`, which skip such a row
        // instead of rewriting it.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-conflict-deleted").await;
        let holder = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("holder");
        bind_external_id(&db.conn, holder.id, "S_SHARED", &[])
            .await
            .expect("seed holder");
        soft_delete(&db.conn, holder.id).await.expect("soft delete");

        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S_OWN", &[])
            .await
            .expect("seed own session");

        let err = bind_external_id(&db.conn, row.id, "S_SHARED", &[])
            .await
            .expect_err("a deleted row's id is still taken; refuse it");

        assert!(matches!(err, DbError::Conflict(_)), "got {err:?}");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("S_OWN")
        );
        let holder_after = raw_row(&db.conn, holder.id).await;
        assert_eq!(holder_after.external_id.as_deref(), Some("S_SHARED"));
        assert!(
            holder_after.deleted_at.is_some(),
            "a refused bind must not resurrect the deleted holder"
        );
    }

    #[tokio::test]
    async fn bind_external_id_is_a_plain_write_on_first_bind() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-first").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");

        let preserved = bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("first bind");

        assert_eq!(
            preserved, None,
            "a row with no session yet has nothing to preserve"
        );
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("S1")
        );
    }

    #[tokio::test]
    async fn bind_external_id_is_idempotent_for_the_same_session() {
        // A duplicate SessionStarted (replay, agent re-init, or simply both
        // subscribers handling the same event) must not fork the history.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-idem").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("bind");

        let preserved = bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("rebind to the same id");

        assert_eq!(preserved, None, "rebinding the same id preserves nothing");
        assert_eq!(rows_holding(&db.conn, "S1").await.len(), 1);
    }

    #[tokio::test]
    async fn bind_external_id_adopts_a_row_that_already_holds_the_old_session() {
        // What fork establishes: a sibling already carries S1, so the invariant
        // holds before we start and re-preserving would both duplicate the row
        // and (given the unique index) fail outright. Either race order must
        // land on exactly two rows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-adopt").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("bind");
        // Stand in for fork's sibling insert.
        let sibling = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("sibling");
        bind_external_id(&db.conn, sibling.id, "S1_HOLDER", &[])
            .await
            .expect("seed");
        let mut active: conversation::ActiveModel = raw_row(&db.conn, sibling.id).await.into();
        active.external_id = Set(Some("S1".into()));
        // Release S1 on the original first, exactly as the real fork does.
        let mut original: conversation::ActiveModel = raw_row(&db.conn, row.id).await.into();
        original.external_id = Set(Some("S2".into()));
        original.update(&db.conn).await.expect("release");
        active
            .update(&db.conn)
            .await
            .expect("hand S1 to the sibling");

        // Now the late SessionStarted{S2} arrives for the original row.
        let preserved = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("late rebind");

        assert_eq!(
            preserved, None,
            "S1 already has a home; preserving again would duplicate it"
        );
        assert_eq!(rows_holding(&db.conn, "S1").await.len(), 1);
    }

    #[tokio::test]
    async fn bind_external_id_maps_in_progress_to_pending_review() {
        // The preserved row has no agent attached any more, so carrying
        // `InProgress` over would leave it spinning in the sidebar forever.
        // Settled states are carried as-is — they are user-visible and still
        // true of the history being preserved.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-status").await;

        for (original, expected) in [
            (
                conversation::ConversationStatus::InProgress,
                conversation::ConversationStatus::PendingReview,
            ),
            (
                conversation::ConversationStatus::Completed,
                conversation::ConversationStatus::Completed,
            ),
            (
                conversation::ConversationStatus::Cancelled,
                conversation::ConversationStatus::Cancelled,
            ),
        ] {
            let row = create(&db.conn, folder, AgentType::Codex, None, None)
                .await
                .expect("create");
            let seed = format!("S1-{original:?}");
            bind_external_id(&db.conn, row.id, &seed, &[])
                .await
                .expect("bind");
            update_status(&db.conn, row.id, original.clone())
                .await
                .expect("status");

            let preserved_id = bind_external_id(&db.conn, row.id, &format!("S2-{original:?}"), &[])
                .await
                .expect("rebind")
                .expect("preserved");

            assert_eq!(
                raw_row(&db.conn, preserved_id).await.status,
                expected,
                "{original:?} must be preserved as {expected:?}"
            );
        }
    }

    #[tokio::test]
    async fn bind_external_id_keeps_a_delegation_child_inside_its_parent() {
        // `kind == Delegate ⟺ parent_id IS NOT NULL` is a documented invariant,
        // and a preserved child that lost its parent would surface as a stray
        // root in the sidebar. The two near-unique delegation identity fields
        // are deliberately NOT carried — duplicating them would point the
        // parent's tool-call view at two children for one call.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-delegate").await;
        let (parent_id, child_id) = seed_parent_with_child(&db.conn, folder).await;
        bind_external_id(&db.conn, child_id, "S1", &[])
            .await
            .expect("bind child");

        let preserved_id = bind_external_id(&db.conn, child_id, "S2", &[])
            .await
            .expect("rebind child")
            .expect("preserved");

        let preserved = raw_row(&db.conn, preserved_id).await;
        assert_eq!(preserved.parent_id, Some(parent_id));
        assert_eq!(preserved.kind, ConversationKind::Delegate);
        assert_eq!(preserved.parent_tool_use_id, None);
        assert_eq!(preserved.delegation_call_id, None);
    }

    #[tokio::test]
    async fn bind_external_id_ignores_the_same_session_under_another_agent() {
        // The uniqueness the index enforces is per-AGENT, and so is the
        // "already preserved" lookup. A row belonging to a different agent that
        // happens to share the id string must not be mistaken for a home for
        // this agent's outgoing session.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-bind-agent-scope").await;
        let other_agent = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("other agent row");
        bind_external_id(&db.conn, other_agent.id, "S1", &[])
            .await
            .expect("bind other agent");
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "S1", &[])
            .await
            .expect("same id, different agent is allowed by the index");

        let preserved = bind_external_id(&db.conn, row.id, "S2", &[])
            .await
            .expect("rebind")
            .expect("the other agent's row is not a home for OUR session");

        let preserved = raw_row(&db.conn, preserved).await;
        assert_eq!(preserved.agent_type, row.agent_type);
    }

    #[tokio::test]
    async fn renormalize_external_id_alias_is_a_compare_and_swap() {
        // The alias path rewrites one spelling of a session into another and
        // must NOT split history. Its expected-old guard exists so a
        // `SessionStarted` that rebound the row while the parse was running
        // cannot be clobbered by the stale write that follows.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-alias").await;
        let row = create(&db.conn, folder, AgentType::Gemini, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "acp-uuid", &[])
            .await
            .expect("bind");

        renormalize_external_id_alias(&db.conn, row.id, Some("acp-uuid"), "branch-id".into())
            .await
            .expect("normalize");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("branch-id"),
            "the alias is normalized in place, with no second row"
        );
        assert_eq!(
            rows_holding(&db.conn, "acp-uuid").await.len(),
            0,
            "an alias rewrite must not leave a phantom conversation behind"
        );

        // Stale write: the row has since moved on. It must match nothing.
        renormalize_external_id_alias(&db.conn, row.id, Some("acp-uuid"), "stale".into())
            .await
            .expect("a no-match is a silent no-op, not an error");
        assert_eq!(
            raw_row(&db.conn, row.id).await.external_id.as_deref(),
            Some("branch-id"),
            "a stale alias write must not clobber a newer binding"
        );
    }

    #[tokio::test]
    async fn bind_external_id_skips_soft_deleted_row() {
        // A late/stale `SessionStarted` write — e.g. a fork's SessionStarted{S2}
        // landing after the user deleted the conversation — must NOT mutate a
        // soft-deleted row. `bind_external_id` is guarded on `deleted_at IS
        // NULL`, so it is a silent no-op: the deleted row keeps its old
        // external_id and is never half-resurrected.
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-extid-deleted").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Doomed".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-S1", &[])
            .await
            .expect("seed external_id");
        soft_delete(&db.conn, row.id).await.expect("soft delete");

        // The guarded write must no-op (Ok) without touching the deleted row.
        // Critically it must ALSO not preserve anything: a deleted row's
        // session is not being orphaned, so manufacturing a preserving row
        // here would resurrect a conversation the user just deleted.
        let preserved = bind_external_id(&db.conn, row.id, "session-S2", &[])
            .await
            .expect("a stale SessionStarted write must be a no-op, not an error");
        assert_eq!(
            preserved, None,
            "a soft-deleted row must not spawn a preserving row"
        );

        // Inspect the raw row directly — `get_by_id` filters deleted rows out.
        let raw = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row still exists (soft-deleted)");
        assert!(raw.deleted_at.is_some(), "row must remain soft-deleted");
        assert_eq!(
            raw.external_id.as_deref(),
            Some("session-S1"),
            "a stale external_id write must not re-point a soft-deleted row"
        );

        // Sanity: the guard is not over-broad — a LIVE row still updates.
        let live = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Live".into()),
            None,
        )
        .await
        .expect("create live");
        bind_external_id(&db.conn, live.id, "session-S9", &[])
            .await
            .expect("live update");
        let live_raw = conversation::Entity::find_by_id(live.id)
            .one(&db.conn)
            .await
            .expect("query live")
            .expect("live row");
        assert_eq!(
            live_raw.external_id.as_deref(),
            Some("session-S9"),
            "a live row must still receive its external_id"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_writes_when_unlocked_and_changed() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");

        let wrote = refresh_auto_title(&db.conn, row.id, "fresh".into())
            .await
            .expect("auto");
        assert!(wrote, "an unlocked, changed title must be written");

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("fresh"));
        assert!(
            !summary.title_locked,
            "auto refresh must NOT lock — the title stays eligible for future refreshes"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_skips_when_unchanged_or_empty() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto-skip").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("same".into()),
            None,
        )
        .await
        .expect("create");

        assert!(
            !refresh_auto_title(&db.conn, row.id, "same".into())
                .await
                .expect("auto-same"),
            "identical title must be a no-op"
        );
        assert!(
            !refresh_auto_title(&db.conn, row.id, String::new())
                .await
                .expect("auto-empty"),
            "empty title must be a no-op"
        );
    }

    #[tokio::test]
    async fn refresh_auto_title_never_clobbers_a_locked_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-auto-locked").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        let wrote = refresh_auto_title(&db.conn, row.id, "parser title".into())
            .await
            .expect("auto");
        assert!(!wrote, "a locked title must never be auto-overwritten");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
        assert!(summary.title_locked);
    }

    #[tokio::test]
    async fn refresh_auto_title_does_not_bump_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-no-bump").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;

        let wrote = refresh_auto_title(&db.conn, row.id, "fresh".into())
            .await
            .expect("auto");
        assert!(wrote);

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("fresh"));
        assert_eq!(
            summary.updated_at, before,
            "auto-title backfill is metadata, not activity — it must not bump updated_at"
        );
    }

    #[tokio::test]
    async fn seed_auto_title_if_empty_writes_only_when_untitled() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-seed").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let before = row.updated_at;

        assert!(
            seed_auto_title_if_empty(&db.conn, row.id, "  First prompt  ".into())
                .await
                .expect("seed"),
            "an empty unlocked title must be seeded"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("First prompt"));
        assert!(!summary.title_locked);
        assert_eq!(summary.updated_at, before, "seed must not bump updated_at");

        assert!(
            !seed_auto_title_if_empty(&db.conn, row.id, "Second prompt".into())
                .await
                .expect("seed-2"),
            "a later prompt must not replace the first-prompt seed"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get-2");
        assert_eq!(summary.title.as_deref(), Some("First prompt"));
    }

    #[tokio::test]
    async fn seed_auto_title_if_empty_skips_locked_and_empty() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-seed-skip").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        assert!(
            !seed_auto_title_if_empty(&db.conn, row.id, "First prompt".into())
                .await
                .expect("seed-locked"),
            "a locked title must not be seeded over"
        );
        assert!(!seed_auto_title_if_empty(&db.conn, row.id, String::new())
            .await
            .expect("seed-empty"));
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
    }

    /// Neither auto-title primitive may write a soft-deleted row. Both are now
    /// driven from the live ACP path (a title can land while the user is
    /// deleting the conversation), and a late write to a deleted row is a
    /// resurrection the sidebar can never show — `emit_conversation_upsert`
    /// filters it out, so the row would silently diverge from every client.
    #[tokio::test]
    async fn auto_title_writes_skip_soft_deleted_rows() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-deleted").await;

        // Untitled + deleted: the first-prompt seed must not name it.
        let seeded = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        soft_delete(&db.conn, seeded.id).await.expect("soft delete");
        assert!(
            !seed_auto_title_if_empty(&db.conn, seeded.id, "First prompt".into())
                .await
                .expect("seed"),
            "a soft-deleted row must not be seeded"
        );

        // Titled + deleted: a live ACP title must not replace it either.
        let refreshed = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Old name".into()),
            None,
        )
        .await
        .expect("create");
        soft_delete(&db.conn, refreshed.id)
            .await
            .expect("soft delete");
        assert!(
            !refresh_auto_title(&db.conn, refreshed.id, "Agent title".into())
                .await
                .expect("refresh"),
            "a soft-deleted row must not be auto-retitled"
        );

        for (id, expected) in [(seeded.id, None), (refreshed.id, Some("Old name"))] {
            let row = conversation::Entity::find_by_id(id)
                .one(&db.conn)
                .await
                .expect("query")
                .expect("row still present");
            assert_eq!(row.title.as_deref(), expected);
        }
    }

    /// The work-task / automation launch path: the seed IS the name, so locking
    /// must keep the title byte-identical, keep the row where it is in a
    /// recency-sorted sidebar, and make the next auto-title a no-op.
    #[tokio::test]
    async fn lock_title_freezes_the_seed_without_rewriting_or_bumping() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-title-lock-seed").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("Fix the login flow".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;
        assert!(!row.title_locked, "a fresh row starts unlocked");

        lock_title(&db.conn, row.id).await.expect("lock");

        let locked = get_by_id(&db.conn, row.id).await.expect("get");
        assert!(locked.title_locked, "lock_title must set the flag");
        assert_eq!(
            locked.title.as_deref(),
            Some("Fix the login flow"),
            "lock_title must not rewrite the title"
        );
        assert_eq!(
            locked.updated_at, before,
            "locking is metadata, not activity — it must not bump updated_at"
        );

        // The whole point: the parsed session title can no longer take over.
        assert!(
            !refresh_auto_title(&db.conn, row.id, "项目：/Users/me/app".into())
                .await
                .expect("auto"),
            "a seeded-and-locked title must survive the per-turn backfill"
        );
        let after = get_by_id(&db.conn, row.id).await.expect("get again");
        assert_eq!(after.title.as_deref(), Some("Fix the login flow"));
    }

    /// A missing row must not be an error — the caller locks on a best-effort
    /// path where a failure would be worse than the drift it prevents.
    #[tokio::test]
    async fn lock_title_on_a_missing_row_is_a_no_op() {
        let db = fresh_in_memory_db().await;
        lock_title(&db.conn, 999_999).await.expect("no-op lock");
    }

    #[tokio::test]
    async fn retitle_if_unchanged_follows_the_owner_rename() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-follow").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        let before = row.updated_at;
        lock_title(&db.conn, row.id).await.expect("lock");

        let wrote = retitle_if_unchanged(&db.conn, row.id, "old", "new")
            .await
            .expect("retitle");
        assert!(wrote, "a title still in sync with its owner must follow it");

        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("new"));
        assert!(summary.title_locked, "the row must stay locked");
        assert_eq!(
            summary.updated_at, before,
            "following an owner rename is metadata, not activity"
        );
    }

    /// The guard that makes the sync safe: once the user names the conversation
    /// themselves, the owner's rename must not reach it. `title_locked` is true
    /// in BOTH cases, so only the expected-value comparison can tell them apart.
    #[tokio::test]
    async fn retitle_if_unchanged_refuses_after_a_manual_rename() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-refuse").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("old".into()),
            None,
        )
        .await
        .expect("create");
        update_title(&db.conn, row.id, "User pick".into())
            .await
            .expect("rename");

        let wrote = retitle_if_unchanged(&db.conn, row.id, "old", "new")
            .await
            .expect("retitle");
        assert!(!wrote, "a hand-picked title must not follow the owner");
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("User pick"));
    }

    #[tokio::test]
    async fn retitle_if_unchanged_skips_empty_and_identical() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-retitle-skip").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("same".into()),
            None,
        )
        .await
        .expect("create");

        assert!(
            !retitle_if_unchanged(&db.conn, row.id, "same", "same")
                .await
                .expect("identical"),
            "an unchanged owner name must be a no-op"
        );
        assert!(
            !retitle_if_unchanged(&db.conn, row.id, "same", "   ")
                .await
                .expect("blank"),
            "a blank new title must never erase the name"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.title.as_deref(), Some("same"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_converges_without_bumping_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-title").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("Makefile 文件的作用".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "codex-session-1", &[])
            .await
            .expect("set external id");
        let before = get_by_id(&db.conn, row.id).await.expect("get before");
        let titles = HashMap::from([(
            "codex-session-1".to_string(),
            "解释 Makefile 文件作用".to_string(),
        )]);

        assert_eq!(
            refresh_codex_auto_titles(&db.conn, &titles).await,
            vec![row.id]
        );
        let refreshed = get_by_id(&db.conn, row.id).await.expect("get refreshed");
        assert_eq!(refreshed.title.as_deref(), Some("解释 Makefile 文件作用"));
        assert_eq!(
            refreshed.updated_at, before.updated_at,
            "index title refresh must not count as conversation activity"
        );

        assert_eq!(
            refresh_codex_auto_titles(&db.conn, &titles).await,
            Vec::<i32>::new(),
            "a converged title map must issue no UPDATE"
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_keeps_partial_successes_after_row_failure() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-partial-failure").await;
        let successful = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old success".into()),
            None,
        )
        .await
        .expect("create successful candidate");
        bind_external_id(&db.conn, successful.id, "session-success", &[])
            .await
            .expect("set successful external id");
        let failing = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old failure".into()),
            None,
        )
        .await
        .expect("create failing candidate");
        bind_external_id(&db.conn, failing.id, "session-failure", &[])
            .await
            .expect("set failing external id");
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    r#"CREATE TRIGGER fail_second_codex_title_sync
                       BEFORE UPDATE OF title ON conversation
                       WHEN OLD.id = {}
                       BEGIN
                         SELECT RAISE(FAIL, 'injected partial title sync failure');
                       END"#,
                    failing.id
                ),
            ))
            .await
            .expect("install title failure trigger");
        let titles = HashMap::from([
            ("session-success".to_string(), "new success".to_string()),
            ("session-failure".to_string(), "new failure".to_string()),
        ]);

        let refreshed = refresh_codex_auto_titles(&db.conn, &titles).await;

        assert_eq!(refreshed, vec![successful.id]);
        assert_eq!(
            get_by_id(&db.conn, successful.id)
                .await
                .expect("get successful row")
                .title
                .as_deref(),
            Some("new success")
        );
        assert_eq!(
            get_by_id(&db.conn, failing.id)
                .await
                .expect("get failing row")
                .title
                .as_deref(),
            Some("old failure")
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_external_id_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-before", &[])
            .await
            .expect("set initial external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        // In-place re-point: this test is about the CAS seeing a changed
        // `external_id`, not about session preservation, so it uses the alias
        // rewrite rather than `bind_external_id` (which would additionally
        // split `session-before` onto a preserving row and put a second
        // candidate-shaped row in front of the assertions below).
        renormalize_external_id_alias(
            &db.conn,
            row.id,
            Some("session-before"),
            "session-after".into(),
        )
        .await
        .expect("re-point conversation");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "title for session-before",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not update a re-pointed row");
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.external_id.as_deref(), Some("session-after"));
        assert_eq!(current.title.as_deref(), Some("old title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_original_title_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-title-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("candidate title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-title-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        refresh_auto_title(&db.conn, row.id, "newer automatic title".into())
            .await
            .expect("apply concurrent automatic title");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not overwrite a newer title");
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.title.as_deref(), Some("newer automatic title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_deleted_at_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-delete-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("candidate title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-delete-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        soft_delete(&db.conn, row.id)
            .await
            .expect("concurrently delete candidate");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(!wrote, "a stale candidate must not update a deleted row");
        let current = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("read current row")
            .expect("soft-deleted row remains persisted");
        assert!(current.deleted_at.is_some());
        assert_eq!(current.title.as_deref(), Some("candidate title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_title_lock_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-lock-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("same manual title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-lock-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        update_title(&db.conn, row.id, "same manual title".into())
            .await
            .expect("lock title without changing its value");
        let wrote = refresh_codex_auto_title_candidate(
            &db.conn,
            &stale_candidate,
            "stale session index title",
        )
        .await
        .expect("conditional refresh");

        assert!(
            !wrote,
            "a stale candidate must not overwrite a locked title"
        );
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert!(current.title_locked);
        assert_eq!(current.title.as_deref(), Some("same manual title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_rechecks_folder_deletion_at_write_time() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-folder-race").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("old title".into()),
            None,
        )
        .await
        .expect("create");
        bind_external_id(&db.conn, row.id, "session-folder-race", &[])
            .await
            .expect("set external id");
        let stale_candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");

        crate::db::service::folder_service::soft_delete_folder(&db.conn, folder)
            .await
            .expect("soft delete folder");
        let wrote =
            refresh_codex_auto_title_candidate(&db.conn, &stale_candidate, "index title").await;

        assert!(
            !wrote.expect("conditional refresh"),
            "a row whose folder was deleted mid-refresh must not be rewritten (and so must not be broadcast)"
        );
        let current = get_by_id(&db.conn, row.id).await.expect("read current row");
        assert_eq!(current.title.as_deref(), Some("old title"));
    }

    #[tokio::test]
    async fn refresh_codex_auto_title_candidate_adopts_a_title_over_null() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-codex-index-null-title").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create");
        bind_external_id(&db.conn, row.id, "session-null-title", &[])
            .await
            .expect("set external id");
        let candidate = conversation::Entity::find_by_id(row.id)
            .one(&db.conn)
            .await
            .expect("query candidate")
            .expect("candidate exists");
        assert!(candidate.title.is_none(), "fixture must start titleless");

        assert!(
            refresh_codex_auto_title_candidate(&db.conn, &candidate, "first Codex title")
                .await
                .expect("conditional refresh"),
            "the IS NULL branch of the observed-title CAS must still write"
        );
        assert_eq!(
            get_by_id(&db.conn, row.id)
                .await
                .expect("read current row")
                .title
                .as_deref(),
            Some("first Codex title")
        );

        // ...and once a title exists, the same stale (title = NULL) candidate
        // must no longer match.
        assert!(
            !refresh_codex_auto_title_candidate(&db.conn, &candidate, "second Codex title")
                .await
                .expect("conditional refresh"),
            "a stale titleless candidate must not clobber the title it just wrote"
        );
    }

    #[tokio::test]
    async fn refresh_codex_auto_titles_skips_rows_the_sidebar_list_hides() {
        let db = fresh_in_memory_db().await;
        let live_folder = seed_folder(&db, "/tmp/codeg-codex-index-visible").await;
        let dead_folder = seed_folder(&db, "/tmp/codeg-codex-index-hidden").await;

        let visible = create(
            &db.conn,
            live_folder,
            AgentType::Codex,
            Some("old visible".into()),
            None,
        )
        .await
        .expect("create visible");
        bind_external_id(&db.conn, visible.id, "session-visible", &[])
            .await
            .expect("set visible external id");

        let in_dead_folder = create(
            &db.conn,
            dead_folder,
            AgentType::Codex,
            Some("old hidden".into()),
            None,
        )
        .await
        .expect("create hidden");
        bind_external_id(&db.conn, in_dead_folder.id, "session-hidden", &[])
            .await
            .expect("set hidden external id");
        crate::db::service::folder_service::soft_delete_folder(&db.conn, dead_folder)
            .await
            .expect("soft delete folder");

        let loop_row = create(
            &db.conn,
            live_folder,
            AgentType::Codex,
            Some("old loop".into()),
            None,
        )
        .await
        .expect("create loop row");
        bind_external_id(&db.conn, loop_row.id, "session-loop", &[])
            .await
            .expect("set loop external id");
        // No public write path mints kind='loop' yet, so flip it directly.
        let mut active: conversation::ActiveModel = conversation::Entity::find_by_id(loop_row.id)
            .one(&db.conn)
            .await
            .expect("query loop row")
            .expect("loop row exists")
            .into();
        active.kind = Set(ConversationKind::Loop);
        active.update(&db.conn).await.expect("flip kind");

        let titles = HashMap::from([
            ("session-visible".to_string(), "new visible".to_string()),
            ("session-hidden".to_string(), "new hidden".to_string()),
            ("session-loop".to_string(), "new loop".to_string()),
        ]);

        let refreshed = refresh_codex_auto_titles(&db.conn, &titles).await;

        assert_eq!(
            refreshed,
            vec![visible.id],
            "only rows `list_all` would return may be refreshed — every refreshed id is broadcast as a sidebar upsert"
        );
        assert_eq!(
            get_by_id(&db.conn, in_dead_folder.id)
                .await
                .expect("read hidden row")
                .title
                .as_deref(),
            Some("old hidden")
        );
        assert_eq!(
            get_by_id(&db.conn, loop_row.id)
                .await
                .expect("read loop row")
                .title
                .as_deref(),
            Some("old loop")
        );
    }

    #[tokio::test]
    async fn refresh_external_activity_moves_forward_only() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-activity").await;
        let row = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("kept".into()),
            None,
        )
        .await
        .expect("create");
        let created_at = row.created_at;
        let before = row.updated_at;

        // The session kept running in the agent's own CLI after import.
        let later = before + chrono::Duration::hours(2);
        assert!(
            refresh_external_activity(&db.conn, row.id, later, 7)
                .await
                .expect("newer"),
            "newer transcript activity must be adopted"
        );
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.updated_at, later);
        assert_eq!(summary.message_count, 7);
        assert_eq!(
            summary.created_at, created_at,
            "created_at is the import/creation time and must not move"
        );
        assert_eq!(summary.title.as_deref(), Some("kept"));

        // Re-scanning the same (or an older) transcript must not move the row
        // back or re-order a sidebar sorted by recency.
        for (at, label) in [(later, "identical"), (before, "older")] {
            assert!(
                !refresh_external_activity(&db.conn, row.id, at, 1)
                    .await
                    .expect("no-op"),
                "{label} activity must be a no-op"
            );
        }
        let summary = get_by_id(&db.conn, row.id).await.expect("get");
        assert_eq!(summary.updated_at, later);
        assert_eq!(summary.message_count, 7, "a no-op must not resync counts");
    }

    #[tokio::test]
    async fn refresh_external_activity_skips_deleted_and_child_rows() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-activity-guards").await;
        let parent = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("parent");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-activity".into(),
                delegation_call_id: "call-activity".into(),
            }),
        )
        .await
        .expect("child");
        let deleted = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("deleted");
        soft_delete(&db.conn, deleted.id)
            .await
            .expect("soft delete");

        let later = Utc::now() + chrono::Duration::hours(1);
        assert!(
            !refresh_external_activity(&db.conn, child.id, later, 9)
                .await
                .expect("child"),
            "a delegation child is not a sidebar row"
        );
        assert!(
            !refresh_external_activity(&db.conn, deleted.id, later, 9)
                .await
                .expect("deleted"),
            "a soft-deleted conversation must never be half-resurrected"
        );

        for id in [child.id, deleted.id] {
            let raw = conversation::Entity::find_by_id(id)
                .one(&db.conn)
                .await
                .expect("query")
                .expect("row present");
            assert_eq!(raw.message_count, 0, "row {id} untouched");
        }
    }

    #[tokio::test]
    async fn create_paths_write_expected_kinds() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/kinds").await;

        let regular = create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("regular");
        assert_eq!(regular.kind, ConversationKind::Regular);

        let chat = create_chat(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("chat");
        assert_eq!(chat.kind, ConversationKind::Chat);

        let child = create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: regular.id,
                parent_tool_use_id: "tu-kind".into(),
                delegation_call_id: "call-kind".into(),
            }),
        )
        .await
        .expect("delegate");
        assert_eq!(child.kind, ConversationKind::Delegate);
        assert_eq!(child.parent_id, Some(regular.id));
    }

    #[tokio::test]
    async fn create_with_delegation_initializes_runtime_rollup() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/rollup-init").await;

        let regular = create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("regular");
        let delegated = create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: regular.id,
                parent_tool_use_id: "tu-rollup".into(),
                delegation_call_id: "call-rollup".into(),
            }),
        )
        .await
        .expect("delegate");

        assert!(regular.delegation_tool_call_count.is_none());
        assert_eq!(delegated.delegation_tool_call_count, Some(0));
        assert_eq!(delegated.delegation_edit_tool_call_count, Some(0));
        assert_eq!(
            delegated.delegation_touched_files_json.as_deref(),
            Some("[]")
        );
        assert_eq!(delegated.delegation_touched_files_truncated, Some(false));
        assert_eq!(delegated.delegation_line_counts_complete, Some(false));
    }

    #[tokio::test]
    async fn create_create_chat_and_delegation_enroll_exactly_one_job_when_enabled() {
        use crate::auto_title::{enable_title_api_for_test, title_key};
        use crate::db::entities::auto_title_job;

        let db = fresh_in_memory_db().await;
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;
        let folder_id = seed_folder(&db, "/tmp/create-enroll").await;

        let regular = create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("regular");
        let chat = create_chat(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("chat");
        let child = create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
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
            assert_eq!(
                auto_title_job::Entity::find_by_id(id)
                    .all(&db.conn)
                    .await
                    .expect("jobs")
                    .len(),
                1,
                "enabled create path must enroll exactly one job for {id}"
            );
        }
    }

    #[tokio::test]
    async fn list_all_excludes_loop_kind_rows() {
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, "/tmp/loop-filter").await;
        let keep = create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("keep".into()),
            None,
        )
        .await
        .expect("keep");
        let hide = create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("hide".into()),
            None,
        )
        .await
        .expect("hide");
        // No public write path mints kind='loop' yet (reserved for the loop
        // engine), so flip the row directly to exercise the filter.
        let mut active: conversation::ActiveModel = hide.into();
        active.kind = Set(ConversationKind::Loop);
        active.update(&db.conn).await.expect("flip kind");

        let rows = list_all(&db.conn, None, None, None, None, None, false)
            .await
            .expect("list");
        assert!(rows.iter().any(|r| r.id == keep.id), "regular row stays");
        assert!(
            !rows.iter().any(|r| r.title.as_deref() == Some("hide")),
            "loop row must be excluded"
        );
    }

    #[tokio::test]
    async fn awaiting_reply_eligible_root_end_turn_sets_one_generation_atomically() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-root").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("create root");

        let patch = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .expect("finish")
            .expect("CAS changed row");
        assert_eq!(patch.status, "pending_review");
        assert!(patch.awaiting_reply_token.is_some());

        let duplicate = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .expect("duplicate finish");
        assert!(duplicate.is_none(), "duplicate end_turn must lose the CAS");
    }

    #[tokio::test]
    async fn awaiting_reply_background_root_and_child_never_get_a_generation() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-ineligible").await;
        let root = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("root");
        let root_patch = finish_end_turn_if_in_progress(&db.conn, root.id, false)
            .await
            .expect("background finish")
            .expect("root transition");
        assert!(root_patch.awaiting_reply_token.is_none());

        let (parent_id, child_id) = seed_parent_with_child(&db.conn, folder).await;
        let child_patch = finish_end_turn_if_in_progress(&db.conn, child_id, true)
            .await
            .expect("child finish")
            .expect("child transition");
        assert_eq!(child_patch.status, "pending_review");
        assert!(child_patch.awaiting_reply_token.is_none());
        assert_ne!(parent_id, child_id);
    }

    #[tokio::test]
    async fn awaiting_reply_terminal_status_wins_over_delayed_end_turn() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-terminal-race").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("root");
        update_status(
            &db.conn,
            row.id,
            conversation::ConversationStatus::Completed,
        )
        .await
        .expect("complete first");

        assert!(finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .expect("delayed finish")
            .is_none());
        let current = get_by_id(&db.conn, row.id).await.expect("current row");
        assert_eq!(current.status, "completed");
        assert!(current.awaiting_reply_token.is_none());
    }

    #[tokio::test]
    async fn awaiting_reply_stale_clear_cannot_remove_a_newer_generation() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-stale-clear").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("root");
        let first = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .unwrap()
            .unwrap();
        let token_a = first.awaiting_reply_token.expect("token A");

        update_status(
            &db.conn,
            row.id,
            conversation::ConversationStatus::InProgress,
        )
        .await
        .expect("next prompt");
        let second = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .unwrap()
            .unwrap();
        let token_b = second.awaiting_reply_token.clone().expect("token B");
        assert_ne!(token_a, token_b);

        let stale = clear_awaiting_reply(&db.conn, row.id, &token_a)
            .await
            .expect("stale clear");
        assert!(!stale.changed);
        assert_eq!(
            stale.patch.awaiting_reply_token.as_deref(),
            Some(token_b.as_str())
        );
    }

    #[tokio::test]
    async fn awaiting_reply_matching_clear_preserves_status_and_updated_at() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-clear").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("root");
        let before = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .unwrap()
            .unwrap();
        let token = before.awaiting_reply_token.clone().unwrap();

        let cleared = clear_awaiting_reply(&db.conn, row.id, &token)
            .await
            .expect("clear");
        assert!(cleared.changed);
        assert_eq!(cleared.patch.status, "pending_review");
        assert!(cleared.patch.awaiting_reply_token.is_none());
        assert_eq!(cleared.patch.updated_at, before.updated_at);
    }

    #[tokio::test]
    async fn awaiting_reply_metadata_preserves_token_but_manual_status_clears_it() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/await-metadata").await;
        let row = create(&db.conn, folder, AgentType::Codex, None, None)
            .await
            .expect("root");
        let marked = finish_end_turn_if_in_progress(&db.conn, row.id, true)
            .await
            .unwrap()
            .unwrap();
        let token = marked.awaiting_reply_token.clone().unwrap();

        update_title(&db.conn, row.id, "renamed".into())
            .await
            .unwrap();
        update_pin(&db.conn, row.id, true).await.unwrap();
        update_external_id(&db.conn, row.id, "external-1".into())
            .await
            .unwrap();
        assert_eq!(
            get_by_id(&db.conn, row.id)
                .await
                .unwrap()
                .awaiting_reply_token
                .as_deref(),
            Some(token.as_str())
        );

        update_status(
            &db.conn,
            row.id,
            conversation::ConversationStatus::PendingReview,
        )
        .await
        .expect("manual review status");
        assert!(get_by_id(&db.conn, row.id)
            .await
            .unwrap()
            .awaiting_reply_token
            .is_none());
    }

    async fn seed_provisional_child(
        conn: &DatabaseConnection,
        folder_id: i32,
        call_id: &str,
    ) -> (i32, i32) {
        let parent = create(
            conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("parent-prov".into()),
            None,
        )
        .await
        .expect("parent");
        let child = create_with_delegation(
            conn,
            folder_id,
            AgentType::Codex,
            Some("child-prov".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: format!("tu-{call_id}"),
                delegation_call_id: call_id.into(),
            }),
        )
        .await
        .expect("child");
        (parent.id, child.id)
    }

    async fn insert_minimal_run(
        conn: &DatabaseConnection,
        task_id: &str,
        parent_id: i32,
        child_id: i32,
    ) {
        use crate::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
        use sea_orm::{ActiveModelTrait, Set};

        let now = Utc::now();
        let row = delegation_task_run::ActiveModel {
            task_id: Set(task_id.into()),
            root_task_id: Set(task_id.into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent_id),
            parent_tool_use_id: Set(Some(format!("tu-{task_id}"))),
            child_conversation_id: Set(child_id),
            agent_type: Set("codex".into()),
            profile_id: Set(None),
            workspace_path: Set(None),
            route_fingerprint: Set(None),
            launch_snapshot_version: Set(None),
            mode_id: Set(None),
            config_values_json: Set(None),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            reached_running_at: Set(None),
            lineage_root_task_id: Set(task_id.into()),
            work_unit_key: Set(None),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Reserving),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            tool_call_count: Set(None),
            edit_tool_call_count: Set(None),
            touched_files_json: Set(None),
            touched_files_truncated: Set(None),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(None),
            card_summary_json: Set(None),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        row.insert(conn).await.expect("insert run");
    }

    #[tokio::test]
    async fn terminalize_provisional_child_sets_failed_admission_rejected() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-term-ok").await;
        let (parent_id, child_id) = seed_provisional_child(&db.conn, folder, "call-term-ok").await;

        let outcome = terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("terminalize");
        assert_eq!(outcome, ProvisionalTerminalizeOutcome::Terminalized);

        let raw = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        assert_eq!(
            raw.delegation_task_status,
            Some(conversation::DelegationTaskStatus::Failed)
        );
        assert_eq!(
            raw.delegation_error_code.as_deref(),
            Some(PROVISIONAL_ADMISSION_REJECTED)
        );
        assert!(raw.delegation_finished_at.is_some());
        assert!(raw.deleted_at.is_none(), "Step 1 must not soft-delete");
        assert_eq!(
            list_children(&db.conn, parent_id)
                .await
                .expect("list")
                .len(),
            1,
            "failed provisional remains visible until Step 2"
        );
    }

    #[tokio::test]
    async fn terminalize_provisional_child_already_terminal_is_idempotent() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-term-idem").await;
        let (_parent_id, child_id) =
            seed_provisional_child(&db.conn, folder, "call-term-idem").await;

        terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("first");
        let second = terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("second");
        assert_eq!(second, ProvisionalTerminalizeOutcome::AlreadyTerminal);
    }

    /// Parent-end durable settle may project a pure provisional shell to
    /// canceled before abandon reclaims the run. Step 1 must rewrite that
    /// shell to `provisional_admission_rejected` so Step 2 can hide it.
    #[tokio::test]
    async fn terminalize_provisional_child_rewrites_parent_end_canceled_shell() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-term-parent-end").await;
        let (_parent_id, child_id) =
            seed_provisional_child(&db.conn, folder, "call-term-parent-end").await;

        let row = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        let mut active = row.into_active_model();
        active.delegation_task_status = Set(Some(conversation::DelegationTaskStatus::Canceled));
        active.delegation_error_code = Set(Some("parent_canceled".into()));
        active.delegation_finished_at = Set(Some(Utc::now()));
        active.status = Set(conversation::ConversationStatus::Cancelled);
        active.update(&db.conn).await.expect("project canceled");

        let outcome = terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("rewrite");
        assert_eq!(outcome, ProvisionalTerminalizeOutcome::Terminalized);

        let raw = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        assert_eq!(
            raw.delegation_task_status,
            Some(conversation::DelegationTaskStatus::Failed)
        );
        assert_eq!(
            raw.delegation_error_code.as_deref(),
            Some(PROVISIONAL_ADMISSION_REJECTED)
        );
        assert!(raw.deleted_at.is_none());
    }

    #[tokio::test]
    async fn terminalize_provisional_child_with_run_is_invariant_error() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-term-run").await;
        let (parent_id, child_id) = seed_provisional_child(&db.conn, folder, "call-term-run").await;
        insert_minimal_run(&db.conn, "task-term-run", parent_id, child_id).await;

        let err = terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect_err("must not terminalize admitted child");
        match err {
            DbError::Validation(msg) => assert!(msg.contains("acquired run"), "{msg}"),
            other => panic!("expected Validation, got {other:?}"),
        }
        let raw = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        assert_eq!(
            raw.delegation_task_status,
            Some(conversation::DelegationTaskStatus::Running),
            "must remain running"
        );
        assert!(raw.deleted_at.is_none());
    }

    #[tokio::test]
    async fn soft_delete_provisional_child_requires_failed_admission_rejected() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-sd-guard").await;
        let (parent_id, child_id) = seed_provisional_child(&db.conn, folder, "call-sd-guard").await;

        // Generic soft-delete would hide a still-running row; guarded Step 2 must not.
        let err = soft_delete_provisional_child(&db.conn, child_id)
            .await
            .expect_err("still-running must not soft-delete");
        match err {
            DbError::Validation(_) => {}
            other => panic!("expected Validation, got {other:?}"),
        }
        assert_eq!(
            list_children(&db.conn, parent_id)
                .await
                .expect("list")
                .len(),
            1
        );

        terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("term");
        let outcome = soft_delete_provisional_child(&db.conn, child_id)
            .await
            .expect("soft-delete");
        assert_eq!(outcome, ProvisionalSoftDeleteOutcome::SoftDeleted);
        assert!(list_children(&db.conn, parent_id)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test]
    async fn soft_delete_provisional_child_never_hides_child_with_run() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-sd-run").await;
        let (parent_id, child_id) = seed_provisional_child(&db.conn, folder, "call-sd-run").await;
        // Terminalize first (no run yet), then interleave admission, then Step 2.
        terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("term");
        insert_minimal_run(&db.conn, "task-sd-run", parent_id, child_id).await;

        let err = soft_delete_provisional_child(&db.conn, child_id)
            .await
            .expect_err("must not hide admitted child");
        match err {
            DbError::Validation(msg) => assert!(msg.contains("acquired run"), "{msg}"),
            other => panic!("expected Validation, got {other:?}"),
        }
        assert_eq!(
            list_children(&db.conn, parent_id)
                .await
                .expect("list")
                .len(),
            1,
            "interleaved admission must leave child visible"
        );
        let raw = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row");
        assert!(raw.deleted_at.is_none());
    }

    #[tokio::test]
    async fn soft_delete_provisional_child_already_deleted_is_idempotent() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/prov-sd-idem").await;
        let (_parent_id, child_id) = seed_provisional_child(&db.conn, folder, "call-sd-idem").await;
        terminalize_provisional_child(&db.conn, child_id)
            .await
            .expect("term");
        soft_delete_provisional_child(&db.conn, child_id)
            .await
            .expect("first");
        let second = soft_delete_provisional_child(&db.conn, child_id)
            .await
            .expect("second");
        assert_eq!(second, ProvisionalSoftDeleteOutcome::AlreadySoftDeleted);
    }
}
