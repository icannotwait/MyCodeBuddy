use std::collections::BTreeMap;
use std::sync::Arc;

use sea_orm::{
    ActiveModelTrait, ActiveValue::NotSet, ColumnTrait, DatabaseConnection, EntityTrait,
    QueryFilter, Set,
};

use crate::auto_title::InternalAgentSessionRegistry;
use crate::commands::conversations::filter_internal_summaries;
use crate::db::entities::conversation;
use crate::db::error::DbError;
use crate::db::service::conversation_service;
use crate::models::{AgentType, ConversationSummary, ImportResult};
use crate::parsers::{build_agent_parser_with_runtime_env, path_eq_for_matching, AgentParser};

/// Every locally-parsable agent, in the canonical parser order.
const ALL_PARSER_AGENTS: [AgentType; 14] = [
    AgentType::ClaudeCode,
    AgentType::Codex,
    AgentType::OpenCode,
    AgentType::Gemini,
    AgentType::Cline,
    AgentType::Hermes,
    AgentType::CodeBuddy,
    AgentType::KimiCode,
    AgentType::Pi,
    AgentType::Grok,
    AgentType::Cursor,
    AgentType::DeepSeek,
    AgentType::Qoder,
    AgentType::Antigravity,
];

fn build_parser(
    agent_type: AgentType,
    deepseek_env: &BTreeMap<String, String>,
) -> Option<Box<dyn AgentParser>> {
    build_agent_parser_with_runtime_env(agent_type, deepseek_env)
}

/// List every local agent's sessions — one `spawn_blocking` per parser so the
/// filesystem walks run concurrently (each closure captures only the Copy
/// `AgentType` and constructs its parser inside, since `dyn AgentParser` is
/// not `Send`). `on_agent_done(agent, done, total, session_count)` fires once
/// per parser (in fixed parser order) so callers can surface scan progress. A
/// parser error is logged and contributes zero sessions; the scan still
/// completes.
///
/// Delegation children (`parent_id.is_some()`) are filtered out here: they are
/// captured live by the delegation flow with their parent linkage, and
/// `import_one` inserts new rows with `parent_id: None` — importing one from a
/// parser listing would surface a sub-session as a root conversation.
/// Duplicates are dropped by `(agent_type, id)`, matching
/// `list_conversations_sync`.
pub(crate) async fn collect_local_summaries<F>(
    deepseek_env: BTreeMap<String, String>,
    mut on_agent_done: F,
) -> Vec<(AgentType, ConversationSummary)>
where
    F: FnMut(AgentType, u32, u32, u32),
{
    let total = ALL_PARSER_AGENTS.len() as u32;
    let deepseek_env = Arc::new(deepseek_env);

    let tasks: Vec<(AgentType, tokio::task::JoinHandle<Vec<ConversationSummary>>)> =
        ALL_PARSER_AGENTS
            .into_iter()
            .map(|at| {
                let deepseek_env = Arc::clone(&deepseek_env);
                (
                    at,
                    tokio::task::spawn_blocking(move || {
                        let Some(parser) = build_parser(at, &deepseek_env) else {
                            return Vec::new();
                        };
                        match parser.list_conversations() {
                            Ok(convs) => convs,
                            Err(e) => {
                                tracing::error!("Error listing {} conversations: {}", at, e);
                                Vec::new()
                            }
                        }
                    }),
                )
            })
            .collect();

    let mut all: Vec<(AgentType, ConversationSummary)> = Vec::new();
    let mut seen: std::collections::HashSet<(AgentType, String)> = std::collections::HashSet::new();
    let mut done = 0u32;

    // Awaiting in parser order only affects callback ordering — all parser
    // walks already run concurrently on the blocking pool.
    for (at, task) in tasks {
        let mut count = 0u32;
        match task.await {
            Ok(convs) => {
                for c in convs {
                    if c.parent_id.is_some() {
                        continue;
                    }
                    if seen.insert((at, c.id.clone())) {
                        all.push((at, c));
                        count += 1;
                    }
                }
            }
            Err(e) => {
                tracing::error!("Session listing task for {} panicked: {}", at, e);
            }
        }
        done += 1;
        on_agent_done(at, done, total, count);
    }

    all
}

/// Scan all parsers while holding the registry's shared discovery lease, then
/// remove internal sessions before any caller can import or display them.
pub(crate) async fn collect_visible_local_summaries<F>(
    registry: &InternalAgentSessionRegistry,
    on_agent_done: F,
) -> Result<Vec<(AgentType, ConversationSummary)>, DbError>
where
    F: FnMut(AgentType, u32, u32, u32),
{
    let deepseek_env = crate::commands::acp::build_agent_effective_config_env(
        registry.database_connection(),
        AgentType::DeepSeek,
    )
    .await
    .map_err(|error| DbError::Database(sea_orm::DbErr::Custom(error.to_string())))?;
    let (guard, filter) = registry.shared_filter().await?;
    let summaries = collect_local_summaries(deepseek_env, on_agent_done).await;
    let visible = filter_internal_summaries(summaries, &filter);
    drop(guard);
    Ok(visible)
}

/// Reconcile a batch of parsed summaries into `folder_id` via [`import_one`].
/// Returns the tally plus the ids of already-imported conversations whose title
/// was refreshed, so the caller can broadcast a sidebar upsert without
/// re-querying. Strict: the first row error aborts and propagates — this is the
/// legacy per-folder import's contract, so its public command still surfaces DB
/// failures rather than silently returning `0/0/0`. The batch importer uses the
/// resilient [`import_summaries_resilient`] instead.
pub(crate) async fn import_summaries(
    conn: &DatabaseConnection,
    folder_id: i32,
    items: &[(AgentType, ConversationSummary)],
) -> Result<(ImportResult, Vec<i32>), DbError> {
    let mut imported = 0u32;
    let mut updated = 0u32;
    let mut skipped = 0u32;
    let mut updated_ids: Vec<i32> = Vec::new();

    for (agent_type, summary) in items {
        match import_one(conn, folder_id, agent_type, summary).await? {
            ImportOutcome::Imported => imported += 1,
            ImportOutcome::Updated(id) => {
                updated += 1;
                updated_ids.push(id);
            }
            ImportOutcome::Skipped => skipped += 1,
        }
    }

    Ok((
        ImportResult {
            imported,
            updated,
            skipped,
        },
        updated_ids,
    ))
}

/// Like [`import_summaries`] but resilient — a single row's DB error is logged
/// and counted in the returned `failed` count rather than aborting the group.
/// The batch importer wants this because each `import_one` insert autocommits:
/// a mid-loop `?`-abort would strand already-committed rows uncounted and leave
/// their (already-created) folder unbroadcast, and imports are idempotent so a
/// re-run finishes the rest — rolling good rows back would be worse. Callers
/// surface the `failed` count in their own structured result.
pub(crate) async fn import_summaries_resilient(
    conn: &DatabaseConnection,
    folder_id: i32,
    items: &[(AgentType, ConversationSummary)],
) -> (ImportResult, Vec<i32>, u32) {
    let mut imported = 0u32;
    let mut updated = 0u32;
    let mut skipped = 0u32;
    let mut failed = 0u32;
    let mut updated_ids: Vec<i32> = Vec::new();

    for (agent_type, summary) in items {
        match import_one(conn, folder_id, agent_type, summary).await {
            Ok(ImportOutcome::Imported) => imported += 1,
            Ok(ImportOutcome::Updated(id)) => {
                updated += 1;
                updated_ids.push(id);
            }
            Ok(ImportOutcome::Skipped) => skipped += 1,
            Err(e) => {
                failed += 1;
                tracing::error!(
                    "Failed to import session {} ({}): {}",
                    summary.id,
                    agent_type,
                    e
                );
            }
        }
    }

    (
        ImportResult {
            imported,
            updated,
            skipped,
        },
        updated_ids,
        failed,
    )
}

/// Import (and refresh the titles of) the local agent sessions under
/// `folder_path`. Strict: a DB error surfaces to the caller (the legacy
/// command's back-compat contract).
pub async fn import_local_conversations(
    conn: &DatabaseConnection,
    registry: &InternalAgentSessionRegistry,
    folder_id: i32,
    folder_path: &str,
) -> Result<(ImportResult, Vec<i32>), DbError> {
    let summaries = collect_visible_local_summaries(registry, |_, _, _, _| {}).await?;
    let matched: Vec<(AgentType, ConversationSummary)> = summaries
        .into_iter()
        .filter(|(_, c)| {
            c.folder_path
                .as_deref()
                .map(|p| path_eq_for_matching(p, folder_path))
                .unwrap_or(false)
        })
        .collect();

    import_summaries(conn, folder_id, &matched).await
}

/// Outcome of reconciling a single parsed session against the DB.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImportOutcome {
    /// A new conversation row was inserted.
    Imported,
    /// An already-imported conversation had its auto-title refreshed; carries
    /// the row id so the caller can broadcast a sidebar upsert.
    Updated(i32),
    /// Already imported, title left unchanged (locked, identical, or the parse
    /// produced no title).
    Skipped,
}

/// Test-only aliases so conversation filter fixtures can exercise the same loop.
#[cfg(test)]
pub(crate) type ImportOutcomeForTest = ImportOutcome;

/// The `conversation.agent_type` column's string form.
fn agent_type_db_str(agent_type: &AgentType) -> String {
    serde_json::to_value(agent_type)
        .ok()
        .and_then(|value| value.as_str().map(String::from))
        .unwrap_or_default()
}

/// Reconcile ONE already-imported conversation against a fresh parse of its
/// agent-side session file. Returns `true` when a row was written, so the
/// caller can count it and broadcast a sidebar upsert. Never inserts, never
/// moves the conversation between folders.
///
/// Two independent refreshes, because a re-scan can find either kind of drift
/// (or both) — a session titled after the fact, and a session the user kept
/// working on in the agent's own CLI:
///
/// * [`conversation_service::refresh_auto_title`] adopts a title that did not
///   exist at first import. A missing/empty parsed title leaves the existing
///   one intact rather than nulling it, a locked title is never clobbered, and
///   it deliberately does not bump `updated_at` (a title is metadata, not
///   activity).
/// * [`conversation_service::refresh_external_activity`] adopts the transcript's
///   own last-activity time into `updated_at` (plus the fresh `message_count`)
///   when it is strictly newer, so a conversation continued outside codeg sorts
///   and reads correctly in a recency-ordered sidebar.
///
/// Both are single conditional UPDATEs whose guards are re-evaluated by the
/// database at write time, so a concurrent manual rename or a live turn wins.
/// The `if` conditions here only mirror those guards in Rust to skip the
/// round-trip when nothing drifted — a converged conversation (the common case,
/// and every row on a re-scan) costs ZERO statements, which is what keeps a
/// whole-machine scan over thousands of imported sessions cheap.
async fn refresh_existing(
    conn: &DatabaseConnection,
    existing: &conversation::Model,
    summary: &ConversationSummary,
) -> Result<bool, DbError> {
    // Rows the sidebar never shows are left completely alone: a soft-deleted
    // conversation must stay deleted (never resurrected or rewritten), and a
    // delegation child is not a sidebar row (the upsert broadcast suppresses it
    // too, which would also desync the `updated` count).
    if existing.parent_id.is_some() || existing.deleted_at.is_some() {
        return Ok(false);
    }

    let mut wrote = false;

    if !existing.title_locked {
        if let Some(title) = summary
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty() && existing.title.as_deref() != Some(*t))
        {
            wrote |= conversation_service::refresh_auto_title(conn, existing.id, title.to_string())
                .await?;
        }
    }

    if let Some(activity_at) = summary.ended_at.filter(|at| *at > existing.updated_at) {
        wrote |= conversation_service::refresh_external_activity(
            conn,
            existing.id,
            activity_at,
            summary.message_count,
        )
        .await?;
    }

    Ok(wrote)
}

/// Refresh every already-imported session in `items` in place — see
/// [`refresh_existing`]. Returns the ids actually written so the caller can
/// broadcast sidebar upserts.
///
/// Unlike [`import_one`] this NEVER inserts: a session the user has not
/// imported yet stays untouched (and unimported) until they pick it. It rides
/// along with the import-picker scan, which already has to load `rows` — every
/// conversation carrying an `external_id` — to mark which sessions exist, so
/// the sync costs one index build plus one UPDATE per row that genuinely
/// drifted, with no per-session SELECT.
///
/// `rows` is matched, not `items`, so every row carrying an `external_id` is
/// visited directly rather than looked up per session. (An earlier version of
/// this comment claimed the pair had no unique index and that duplicate rows
/// therefore had to converge — both are wrong: the init migration creates
/// `idx_conversation_external_agent` UNIQUE over `(external_id, agent_type)`,
/// so duplicates cannot exist. The iteration shape is unchanged; only the
/// stated reason was.) A row error is logged and skipped: a best-effort refresh
/// must never fail the scan it rides along with.
pub(crate) async fn sync_imported_sessions(
    conn: &DatabaseConnection,
    rows: &[conversation::Model],
    items: &[(AgentType, ConversationSummary)],
) -> Vec<i32> {
    let parsed: std::collections::HashMap<(String, &str), &ConversationSummary> = items
        .iter()
        .map(|(at, s)| ((agent_type_db_str(at), s.id.as_str()), s))
        .collect();

    let mut refreshed = Vec::new();
    for row in rows {
        if row.parent_id.is_some() || row.deleted_at.is_some() {
            continue;
        }
        let Some(external_id) = row.external_id.as_deref() else {
            continue;
        };
        let Some(summary) = parsed.get(&(row.agent_type.clone(), external_id)) else {
            continue;
        };
        match refresh_existing(conn, row, summary).await {
            Ok(true) => refreshed.push(row.id),
            Ok(false) => {}
            Err(e) => tracing::error!(
                "Failed to refresh imported session {} ({}): {}",
                external_id,
                row.agent_type,
                e
            ),
        }
    }
    refreshed
}

#[cfg(test)]
pub(crate) async fn import_one_for_test(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: &AgentType,
    summary: &ConversationSummary,
) -> Result<ImportOutcome, DbError> {
    import_one(conn, folder_id, agent_type, summary).await
}

/// Insert a brand-new conversation, or — when it already exists — refresh its
/// title from the freshly parsed session file so an AI-generated title that did
/// not exist at first import is adopted. `refresh_auto_title` is a single
/// conditional UPDATE that skips locked or unchanged rows and never bumps
/// `updated_at`, so a re-import neither clobbers a manual rename nor reorders a
/// recency-sorted sidebar. A missing/empty parsed title leaves the existing
/// title intact rather than nulling it.
pub(crate) async fn import_one(
    conn: &DatabaseConnection,
    folder_id: i32,
    agent_type: &AgentType,
    summary: &ConversationSummary,
) -> Result<ImportOutcome, DbError> {
    let at_str = agent_type_db_str(agent_type);

    let exists = conversation::Entity::find()
        .filter(conversation::Column::ExternalId.eq(&summary.id))
        .filter(conversation::Column::AgentType.eq(&at_str))
        .one(conn)
        .await?;

    if let Some(existing) = exists {
        // Preserve the original skip for rows the sidebar never shows.
        if existing.parent_id.is_some() || existing.deleted_at.is_some() {
            return Ok(ImportOutcome::Skipped);
        }
        if let Err(error) =
            crate::db::service::token_usage_service::mark_stale_for_reparse(conn, existing.id).await
        {
            tracing::warn!(
                conversation_id = existing.id,
                error = %error,
                "import: failed to invalidate the token-usage stamp"
            );
        }
        return Ok(if refresh_existing(conn, &existing, summary).await? {
            ImportOutcome::Updated(existing.id)
        } else {
            ImportOutcome::Skipped
        });
    }

    let created_at = summary.started_at;
    let updated_at = summary.ended_at.unwrap_or(created_at);
    let conv = conversation::ActiveModel {
        id: NotSet,
        folder_id: Set(folder_id),
        title: Set(summary.title.clone()),
        title_locked: Set(false),
        auto_title_finalized: Set(false),
        agent_type: Set(at_str),
        // Imported sessions land as `PendingReview` ("待审查"), not `Completed`:
        // they are surfaced for the user to review and stay visible even when
        // the sidebar's "show completed" filter is off (now its default).
        status: Set(conversation::ConversationStatus::PendingReview),
        // Imports scan regular folders' session files; chat scratch dirs and
        // loop runs are never import targets, so every imported row is regular.
        kind: Set(conversation::ConversationKind::Regular),
        model: Set(summary.model.clone()),
        git_branch: Set(summary.git_branch.clone()),
        external_id: Set(Some(summary.id.clone())),
        parent_id: Set(None),
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
        message_count: Set(summary.message_count as i32),
        created_at: Set(created_at),
        updated_at: Set(updated_at),
        deleted_at: Set(None),
        pinned_at: Set(None),
        awaiting_reply_token: Set(None),
        delegation_run_generation: Set(None),
        last_termination_audit_json: Set(None),

        origin_cwd: Set(None),
    };
    conv.insert(conn).await?;
    Ok(ImportOutcome::Imported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use chrono::Utc;

    #[test]
    fn import_parser_omits_deepseek_when_child_home_is_unresolved() {
        #[cfg(windows)]
        let home_key = "USERPROFILE";
        #[cfg(not(windows))]
        let home_key = "HOME";
        let runtime_env = BTreeMap::from([
            ("DEEPSEEK_ACP_SESSIONS_ROOT".to_string(), String::new()),
            ("DSH_HOME".to_string(), String::new()),
            (home_key.to_string(), String::new()),
        ]);

        assert!(build_parser(AgentType::DeepSeek, &runtime_env).is_none());
        assert!(build_parser(AgentType::ClaudeCode, &runtime_env).is_some());
    }

    fn summary(id: &str, title: Option<&str>) -> ConversationSummary {
        ConversationSummary {
            id: id.to_string(),
            agent_type: AgentType::ClaudeCode,
            folder_path: Some("/tmp/codeg-import".to_string()),
            folder_name: None,
            title: title.map(|t| t.to_string()),
            started_at: Utc::now(),
            ended_at: None,
            message_count: 3,
            model: None,
            git_branch: None,
            parent_id: None,
            parent_tool_use_id: None,
            delegation_call_id: None,
        }
    }

    async fn find_id(conn: &DatabaseConnection, ext: &str) -> i32 {
        conversation::Entity::find()
            .filter(conversation::Column::ExternalId.eq(ext))
            .one(conn)
            .await
            .expect("query")
            .expect("row exists")
            .id
    }

    #[tokio::test]
    async fn imported_raw_session_never_enrolls_auto_title() {
        use crate::auto_title::{enable_title_api_for_test, title_key};
        use crate::db::entities::auto_title_job;

        let db = fresh_in_memory_db().await;
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;
        let folder = seed_folder(&db, "/tmp/codeg-import-no-enroll").await;
        let at = AgentType::ClaudeCode;

        let outcome = import_one(
            &db.conn,
            folder,
            &at,
            &summary("raw-ext-1", Some("historical")),
        )
        .await
        .expect("import");
        assert_eq!(outcome, ImportOutcome::Imported);

        let id = find_id(&db.conn, "raw-ext-1").await;
        assert!(
            auto_title_job::Entity::find_by_id(id)
                .one(&db.conn)
                .await
                .expect("query job")
                .is_none(),
            "raw import must stay historical and never enroll an auto-title job"
        );
    }

    #[tokio::test]
    async fn reimport_refreshes_a_changed_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import").await;
        let at = AgentType::ClaudeCode;

        let first = import_one(
            &db.conn,
            folder,
            &at,
            &summary("ext-1", Some("first prompt")),
        )
        .await
        .expect("import");
        assert_eq!(first, ImportOutcome::Imported);

        let id = find_id(&db.conn, "ext-1").await;
        // The agent generated an AI title only after the first import; a
        // re-import must adopt it.
        let again = import_one(&db.conn, folder, &at, &summary("ext-1", Some("AI Summary")))
            .await
            .expect("re-import");
        assert_eq!(again, ImportOutcome::Updated(id));

        let got = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("get");
        assert_eq!(got.title.as_deref(), Some("AI Summary"));
        assert!(!got.title_locked, "auto refresh must not lock the title");
    }

    #[tokio::test]
    async fn import_inserts_a_pending_review_conversation() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-status").await;
        let at = AgentType::ClaudeCode;

        assert_eq!(
            import_one(
                &db.conn,
                folder,
                &at,
                &summary("ext-1", Some("first prompt"))
            )
            .await
            .expect("import"),
            ImportOutcome::Imported
        );

        let id = find_id(&db.conn, "ext-1").await;
        let got = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("get");
        // Imported rows are surfaced for review, not marked done — so they stay
        // visible even when the sidebar hides completed conversations.
        // `DbConversationSummary.status` is the serde-serialized string
        // (`rename_all = "snake_case"`), not the enum.
        assert_eq!(got.status, "pending_review");
    }

    #[tokio::test]
    async fn reimport_skips_an_unchanged_title() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-same").await;
        let at = AgentType::ClaudeCode;
        let s = summary("ext-1", Some("same title"));

        assert_eq!(
            import_one(&db.conn, folder, &at, &s).await.expect("import"),
            ImportOutcome::Imported
        );
        assert_eq!(
            import_one(&db.conn, folder, &at, &s)
                .await
                .expect("re-import"),
            ImportOutcome::Skipped
        );
    }

    #[tokio::test]
    async fn reimport_never_clobbers_a_manual_rename() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-lock").await;
        let at = AgentType::ClaudeCode;

        import_one(
            &db.conn,
            folder,
            &at,
            &summary("ext-1", Some("first prompt")),
        )
        .await
        .expect("import");
        let id = find_id(&db.conn, "ext-1").await;
        conversation_service::update_title(&db.conn, id, "User Pick".into())
            .await
            .expect("rename");

        let outcome = import_one(&db.conn, folder, &at, &summary("ext-1", Some("AI Summary")))
            .await
            .expect("re-import");
        assert_eq!(
            outcome,
            ImportOutcome::Skipped,
            "a locked title must not be touched by import"
        );

        let got = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("get");
        assert_eq!(got.title.as_deref(), Some("User Pick"));
    }

    #[tokio::test]
    async fn reimport_with_no_title_keeps_the_existing_one() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-empty").await;
        let at = AgentType::ClaudeCode;

        import_one(&db.conn, folder, &at, &summary("ext-1", Some("kept title")))
            .await
            .expect("import");
        let id = find_id(&db.conn, "ext-1").await;

        // A parse that yields no title (or only whitespace) must not null the
        // existing title.
        assert_eq!(
            import_one(&db.conn, folder, &at, &summary("ext-1", None))
                .await
                .expect("none"),
            ImportOutcome::Skipped
        );
        assert_eq!(
            import_one(&db.conn, folder, &at, &summary("ext-1", Some("   ")))
                .await
                .expect("blank"),
            ImportOutcome::Skipped
        );
        let got = conversation_service::get_by_id(&db.conn, id)
            .await
            .expect("get");
        assert_eq!(got.title.as_deref(), Some("kept title"));
    }

    #[tokio::test]
    async fn reimport_skips_a_soft_deleted_conversation() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-deleted").await;
        let at = AgentType::ClaudeCode;

        import_one(&db.conn, folder, &at, &summary("ext-1", Some("original")))
            .await
            .expect("import");
        let id = find_id(&db.conn, "ext-1").await;
        conversation_service::soft_delete(&db.conn, id)
            .await
            .expect("soft delete");

        // A re-import must neither resurrect nor rewrite a deleted conversation.
        let outcome = import_one(&db.conn, folder, &at, &summary("ext-1", Some("AI Summary")))
            .await
            .expect("re-import");
        assert_eq!(outcome, ImportOutcome::Skipped);

        let row = conversation::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("row still present");
        assert_eq!(row.title.as_deref(), Some("original"), "title untouched");
        assert!(row.deleted_at.is_some(), "must stay soft-deleted");
    }

    #[tokio::test]
    async fn reimport_skips_a_delegation_child() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/codeg-import-child").await;
        let at = AgentType::ClaudeCode;
        let at_str = serde_json::to_value(at)
            .expect("ser")
            .as_str()
            .expect("str")
            .to_string();

        // A root conversation to parent the child.
        import_one(
            &db.conn,
            folder,
            &at,
            &summary("parent-ext", Some("parent")),
        )
        .await
        .expect("import parent");
        let parent_id = find_id(&db.conn, "parent-ext").await;

        // A delegation child carrying its own external_id, as a parser would
        // surface that child's session file on disk.
        let now = Utc::now();
        conversation::ActiveModel {
            id: NotSet,
            folder_id: Set(folder),
            title: Set(Some("child original".to_string())),
            title_locked: Set(false),
            auto_title_finalized: Set(false),
            agent_type: Set(at_str),
            status: Set(conversation::ConversationStatus::Completed),
            kind: Set(conversation::ConversationKind::Delegate),
            model: Set(None),
            git_branch: Set(None),
            external_id: Set(Some("child-ext".to_string())),
            parent_id: Set(Some(parent_id)),
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
            message_count: Set(1),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            pinned_at: Set(None),
            awaiting_reply_token: Set(None),
            delegation_run_generation: Set(None),
            last_termination_audit_json: Set(None),

            origin_cwd: Set(None),
        }
        .insert(&db.conn)
        .await
        .expect("insert child");

        let outcome = import_one(
            &db.conn,
            folder,
            &at,
            &summary("child-ext", Some("AI Summary")),
        )
        .await
        .expect("re-import child");
        assert_eq!(
            outcome,
            ImportOutcome::Skipped,
            "a delegation child is never a sidebar row"
        );

        let child_id = find_id(&db.conn, "child-ext").await;
        let row = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("child present");
        assert_eq!(
            row.title.as_deref(),
            Some("child original"),
            "child title untouched"
        );
    }
}
