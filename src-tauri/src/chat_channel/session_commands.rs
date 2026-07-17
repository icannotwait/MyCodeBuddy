use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use tokio::sync::Mutex;

use super::i18n::{self, Lang};
use super::session_bridge::{ActiveSession, SessionBridge};
use super::types::{MessageLevel, RichMessage};
use crate::acp::error::AcpError;
use crate::acp::manager::ConnectionManager;
use crate::acp::registry::all_acp_agents;
use crate::acp::types::PromptInputBlock;
use crate::auto_title::{
    user_launch_context_from_db, ConnectionLaunchContext, ConnectionPurpose, PromptCaptureContext,
};
use crate::commands::delegation::DelegationRuntimeSettings;
use crate::db::entities::conversation;
use crate::db::service::{
    app_metadata_service, conversation_service, folder_service, sender_context_service,
};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;
use crate::models::system::AppLocale;
use crate::web::event_bridge::EventEmitter;

const MESSAGE_LANGUAGE_KEY: &str = "chat_message_language";

/// Resolve the channel's `AppLocale`: configured chat message language when
/// valid, otherwise persisted system language (then English via settings load).
pub(crate) async fn resolve_channel_app_locale(db: &DatabaseConnection) -> AppLocale {
    if let Ok(Some(raw)) = app_metadata_service::get_value(db, MESSAGE_LANGUAGE_KEY).await {
        if let Some(lang) = Lang::parse_strict(&raw) {
            return i18n::lang_to_app_locale(lang);
        }
    }
    user_launch_context_from_db(db)
        .await
        .inherited_locale
        .unwrap_or(AppLocale::En)
}

/// Chat root/resume launch context: User purpose + resolved channel locale.
pub(crate) async fn channel_launch_context_from_db(
    db: &DatabaseConnection,
) -> ConnectionLaunchContext {
    ConnectionLaunchContext {
        purpose: ConnectionPurpose::User,
        inherited_locale: Some(resolve_channel_app_locale(db).await),
    }
}

/// Database-aware linked send for chat producers: authoritative conversation
/// folder, exact visible text, and resolved channel locale.
pub(crate) async fn send_prompt_linked_for_chat(
    db: &DatabaseConnection,
    conn_mgr: &ConnectionManager,
    connection_id: &str,
    conversation_id: i32,
    text: &str,
) -> Result<(), AcpError> {
    let conv = conversation_service::get_by_id(db, conversation_id)
        .await
        .map_err(|e| AcpError::protocol(e.to_string()))?;
    let locale = resolve_channel_app_locale(db).await;
    let app_db = AppDatabase { conn: db.clone() };
    let blocks = vec![PromptInputBlock::Text {
        text: text.to_string(),
    }];
    conn_mgr
        .send_prompt_linked(
            &app_db,
            connection_id,
            blocks,
            Some(conv.folder_id),
            Some(conversation_id),
            None,
            Some(PromptCaptureContext::new(
                Some(text.to_string()),
                Some(locale),
            )),
        )
        .await
        .map(|_| ())
}

pub struct FollowupRequest<'a> {
    pub db: &'a DatabaseConnection,
    pub text: &'a str,
    pub channel_id: i32,
    pub sender_id: &'a str,
    pub conn_mgr: &'a ConnectionManager,
    pub bridge: &'a Arc<Mutex<SessionBridge>>,
    pub lang: Lang,
    pub prefix: &'a str,
}

// ── /folder ──

pub async fn handle_folder(
    db: &DatabaseConnection,
    args: &str,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    if args.is_empty() {
        return list_folders(db, channel_id, sender_id, lang, prefix).await;
    }

    // Try parse as index (1-based)
    if let Ok(idx) = args.parse::<usize>() {
        return select_folder_by_index(db, idx, channel_id, sender_id, lang, prefix).await;
    }

    // Treat as path
    select_folder_by_path(db, args, channel_id, sender_id, lang).await
}

async fn list_folders(
    db: &DatabaseConnection,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    let folders = match folder_service::list_folders(db).await {
        Ok(f) => f,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_list_folders_label(lang)));
        }
    };

    if folders.is_empty() {
        return RichMessage::info(i18n::no_folders_found(lang))
            .with_title(i18n::folder_title(lang));
    }

    let ctx = sender_context_service::get_or_create(db, channel_id, sender_id)
        .await
        .ok();

    let mut body = String::new();
    for (i, f) in folders.iter().take(10).enumerate() {
        let current = ctx
            .as_ref()
            .and_then(|c| c.current_folder_id)
            .map(|id| id == f.id)
            .unwrap_or(false);
        let marker = if current { " [*]" } else { "" };
        body.push_str(&format!("{}. {}{} ({})\n", i + 1, f.name, marker, f.path));
    }

    body.push_str(&format!("\n{}", i18n::folder_select_hint(lang, prefix)));

    RichMessage::info(body.trim_end()).with_title(i18n::folder_title(lang))
}

async fn select_folder_by_index(
    db: &DatabaseConnection,
    idx: usize,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    if idx == 0 {
        return RichMessage::info(i18n::index_starts_from_one(lang));
    }

    let folders = match folder_service::list_folders(db).await {
        Ok(f) => f,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_list_folders_label(lang)));
        }
    };

    let Some(folder) = folders.get(idx - 1) else {
        return RichMessage::info(i18n::folder_index_out_of_range(lang, prefix));
    };

    let _ = sender_context_service::update_folder(db, channel_id, sender_id, Some(folder.id)).await;

    RichMessage::info(format!("{} ({})", folder.name, folder.path))
        .with_title(i18n::folder_selected_title(lang))
}

async fn select_folder_by_path(
    db: &DatabaseConnection,
    path: &str,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
) -> RichMessage {
    let entry = match folder_service::add_folder(db, path).await {
        Ok(e) => e,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_add_folder_label(lang)));
        }
    };

    let _ = sender_context_service::update_folder(db, channel_id, sender_id, Some(entry.id)).await;

    RichMessage::info(format!("{} ({})", entry.name, entry.path))
        .with_title(i18n::folder_selected_title(lang))
}

// ── /agent ──

pub async fn handle_agent(
    db: &DatabaseConnection,
    args: &str,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    if args.is_empty() {
        return list_agents(db, channel_id, sender_id, lang, prefix).await;
    }

    // Try parse as index
    if let Ok(idx) = args.parse::<usize>() {
        return select_agent_by_index(db, idx, channel_id, sender_id, lang, prefix).await;
    }

    // Try parse as agent type name
    select_agent_by_name(db, args, channel_id, sender_id, lang).await
}

async fn list_agents(
    db: &DatabaseConnection,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    let agents = all_acp_agents();
    let ctx = sender_context_service::get_or_create(db, channel_id, sender_id)
        .await
        .ok();

    let mut body = String::new();
    for (i, at) in agents.iter().enumerate() {
        let at_str = agent_type_to_string(*at);
        let current = ctx
            .as_ref()
            .and_then(|c| c.current_agent_type.as_deref())
            .map(|s| s == at_str)
            .unwrap_or(false);
        let marker = if current { " [*]" } else { "" };
        body.push_str(&format!("{}. {}{}\n", i + 1, at, marker));
    }

    body.push_str(&format!("\n{}", i18n::agent_select_hint(lang, prefix)));

    RichMessage::info(body.trim_end()).with_title(i18n::agent_title(lang))
}

async fn select_agent_by_index(
    db: &DatabaseConnection,
    idx: usize,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    let agents = all_acp_agents();
    if idx == 0 || idx > agents.len() {
        return RichMessage::info(i18n::agent_index_out_of_range(lang, prefix));
    }

    let at = agents[idx - 1];
    let at_str = agent_type_to_string(at);
    let _ = sender_context_service::update_agent(db, channel_id, sender_id, Some(at_str)).await;

    RichMessage::info(at.to_string()).with_title(i18n::agent_selected_title(lang))
}

async fn select_agent_by_name(
    db: &DatabaseConnection,
    name: &str,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
) -> RichMessage {
    let at = match parse_agent_type(name) {
        Some(a) => a,
        None => {
            return RichMessage::info(format!("{}{}", i18n::unknown_agent_label(lang), name));
        }
    };

    let at_str = agent_type_to_string(at);
    let _ = sender_context_service::update_agent(db, channel_id, sender_id, Some(at_str)).await;

    RichMessage::info(at.to_string()).with_title(i18n::agent_selected_title(lang))
}

// ── /task ──

#[allow(clippy::too_many_arguments)]
pub async fn handle_task(
    db: &DatabaseConnection,
    task_description: &str,
    channel_id: i32,
    sender_id: &str,
    conn_mgr: &ConnectionManager,
    emitter: &EventEmitter,
    bridge: &Arc<Mutex<SessionBridge>>,
    lang: Lang,
    prefix: &str,
    runtime: &DelegationRuntimeSettings,
    data_dir: &Path,
) -> RichMessage {
    if task_description.is_empty() {
        return RichMessage::info(i18n::task_usage(lang, prefix));
    }

    // 1. Load sender context
    let ctx = match sender_context_service::get_or_create(db, channel_id, sender_id).await {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_load_context_label(lang)));
        }
    };

    let folder_id = match ctx.current_folder_id {
        Some(id) => id,
        None => {
            return RichMessage::info(i18n::no_folder_selected(lang, prefix));
        }
    };

    // 2. Get folder info
    let folder = match folder_service::get_folder_by_id(db, folder_id).await {
        Ok(Some(f)) => f,
        _ => {
            return RichMessage::info(i18n::folder_not_found_with_hint(lang, prefix));
        }
    };

    // 3. Resolve agent type
    let agent_type = match resolve_agent_type(&ctx.current_agent_type, &folder.default_agent_type) {
        Some(at) => at,
        None => {
            return RichMessage::info(i18n::no_agent_selected(lang, prefix));
        }
    };

    // 4. Create conversation record
    let conv = match conversation_service::create(
        db,
        folder_id,
        agent_type,
        Some(truncate_title(task_description)),
        folder.git_branch.clone(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!(
                "{}{e}",
                i18n::failed_to_create_conversation_label(lang)
            ));
        }
    };

    // 5. Spawn ACP agent with a real one-shot route resolution against the
    // live runtime snapshot and the just-created conversation row.
    let app_db = AppDatabase { conn: db.clone() };
    let runtime_snap = runtime.snapshot();
    let launch_inputs = match crate::acp::terminal_context::build_acp_launch_inputs(
        &app_db,
        agent_type,
        None,
        data_dir,
        crate::acp::terminal_context::AcpRouteRequest::root(Some(conv.id), None),
        &runtime_snap,
    )
    .await
    {
        Ok(inputs) => inputs,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_start_agent_label(lang)));
        }
    };
    let owner_label = format!("chat_channel:{}:{}", channel_id, sender_id);
    let launch_context = channel_launch_context_from_db(db).await;
    let connection_id = match conn_mgr
        .spawn_agent(
            agent_type,
            Some(folder.path.clone()),
            None,
            launch_inputs,
            owner_label,
            emitter.clone(),
            None,
            BTreeMap::new(),
            launch_context,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            // Clean up the conversation record and broadcast the exact patch.
            if let Ok(patch) = conversation_service::update_status_with_patch(
                db,
                conv.id,
                conversation::ConversationStatus::Cancelled,
            )
            .await
            {
                crate::commands::conversations::emit_conversation_state(emitter, patch);
            }
            return RichMessage::error(format!("{}{e}", i18n::failed_to_start_agent_label(lang)));
        }
    };

    // 6. Register in bridge (prompt will be sent after SessionStarted event)
    {
        let session = ActiveSession {
            channel_id,
            sender_id: sender_id.to_string(),
            conversation_id: conv.id,
            connection_id: connection_id.clone(),
            agent_type,
            content_buffer: String::new(),
            tool_calls: Vec::new(),
            tool_call_inputs: std::collections::HashMap::new(),
            delegation_rendered: std::collections::HashSet::new(),
            last_flushed: Instant::now(),
            pending_prompt: Some(task_description.to_string()),
            permission_pending: None,
        };
        bridge.lock().await.register(connection_id.clone(), session);
    }

    // 7. Update sender context
    let _ = sender_context_service::update_session(
        db,
        channel_id,
        sender_id,
        Some(conv.id),
        Some(connection_id),
    )
    .await;

    RichMessage::info(format!("[{}] #{} @ {}", agent_type, conv.id, folder.name,))
        .with_title(i18n::task_started_title(lang))
}

// ── /sessions ──

pub async fn handle_sessions(
    db: &DatabaseConnection,
    channel_id: i32,
    sender_id: &str,
    lang: Lang,
    prefix: &str,
) -> RichMessage {
    let ctx = match sender_context_service::get_or_create(db, channel_id, sender_id).await {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_load_context_label(lang)));
        }
    };

    let folder_id = match ctx.current_folder_id {
        Some(id) => id,
        None => {
            return RichMessage::info(i18n::no_folder_selected(lang, prefix));
        }
    };

    let folder = match folder_service::get_folder_by_id(db, folder_id).await {
        Ok(Some(f)) => f,
        _ => {
            return RichMessage::info(i18n::folder_not_found(lang));
        }
    };

    let convs = match conversation_service::list_by_folder(
        db,
        folder_id,
        None,
        None,
        None,
        Some("in_progress".to_string()),
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_list_sessions_label(lang)));
        }
    };

    if convs.is_empty() {
        return RichMessage::info(i18n::no_active_sessions_in_folder(lang)).with_title(format!(
            "{} - {}",
            i18n::sessions_title(lang),
            folder.name
        ));
    }

    let mut body = String::new();
    for (i, c) in convs.iter().take(10).enumerate() {
        let title = c.title.as_deref().unwrap_or("(untitled)");
        let current = ctx
            .current_conversation_id
            .map(|id| id == c.id)
            .unwrap_or(false);
        let marker = if current { " [*]" } else { "" };
        body.push_str(&format!(
            "{}. [{}] {} (#{}){}  \n",
            i + 1,
            c.agent_type,
            title,
            c.id,
            marker,
        ));
    }

    body.push_str(&format!("\n{}", i18n::sessions_resume_hint(lang, prefix)));

    RichMessage::info(body.trim_end()).with_title(format!(
        "{} - {}",
        i18n::sessions_title(lang),
        folder.name
    ))
}

// ── /resume ──

#[allow(clippy::too_many_arguments)]
pub async fn handle_resume(
    db: &DatabaseConnection,
    args: &str,
    channel_id: i32,
    sender_id: &str,
    conn_mgr: &ConnectionManager,
    emitter: &EventEmitter,
    bridge: &Arc<Mutex<SessionBridge>>,
    lang: Lang,
    prefix: &str,
    runtime: &DelegationRuntimeSettings,
    data_dir: &Path,
) -> RichMessage {
    if args.is_empty() {
        return list_recent_sessions(db, lang, prefix).await;
    }

    let conversation_id: i32 = match args.parse() {
        Ok(id) => id,
        Err(_) => {
            return list_recent_sessions(db, lang, prefix).await;
        }
    };

    let conv = match conversation_service::get_by_id(db, conversation_id).await {
        Ok(c) => c,
        Err(_) => {
            return RichMessage::info(i18n::conversation_not_found(lang));
        }
    };

    let folder = match folder_service::get_folder_by_id(db, conv.folder_id).await {
        Ok(Some(f)) => f,
        _ => {
            return RichMessage::info(i18n::folder_not_found(lang));
        }
    };

    // Spawn agent with session_id for resume; resolve route once against the
    // live runtime and the persisted conversation row (agent-type validated).
    let app_db = AppDatabase { conn: db.clone() };
    let runtime_snap = runtime.snapshot();
    let launch_inputs = match crate::acp::terminal_context::build_acp_launch_inputs(
        &app_db,
        conv.agent_type,
        conv.external_id.as_deref(),
        data_dir,
        crate::acp::terminal_context::AcpRouteRequest::root(Some(conv.id), None),
        &runtime_snap,
    )
    .await
    {
        Ok(inputs) => inputs,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_start_agent_label(lang)));
        }
    };
    let owner_label = format!("chat_channel:{}:{}", channel_id, sender_id);
    let launch_context = channel_launch_context_from_db(db).await;
    let connection_id = match conn_mgr
        .spawn_agent(
            conv.agent_type,
            Some(folder.path.clone()),
            conv.external_id.clone(),
            launch_inputs,
            owner_label,
            emitter.clone(),
            None,
            BTreeMap::new(),
            launch_context,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_start_agent_label(lang)));
        }
    };

    // Register in bridge (no pending prompt for resume)
    {
        let session = ActiveSession {
            channel_id,
            sender_id: sender_id.to_string(),
            conversation_id: conv.id,
            connection_id: connection_id.clone(),
            agent_type: conv.agent_type,
            content_buffer: String::new(),
            tool_calls: Vec::new(),
            tool_call_inputs: std::collections::HashMap::new(),
            delegation_rendered: std::collections::HashSet::new(),
            last_flushed: Instant::now(),
            pending_prompt: None,
            permission_pending: None,
        };
        bridge.lock().await.register(connection_id.clone(), session);
    }

    // Update sender context
    let _ = sender_context_service::update_session(
        db,
        channel_id,
        sender_id,
        Some(conv.id),
        Some(connection_id),
    )
    .await;
    let _ = sender_context_service::update_folder(db, channel_id, sender_id, Some(conv.folder_id))
        .await;

    let title = conv.title.as_deref().unwrap_or("(untitled)");
    RichMessage::info(format!(
        "[{}] #{} {} @ {}",
        conv.agent_type, conv.id, title, folder.name,
    ))
    .with_title(i18n::session_resumed_title(lang))
}

// ── /cancel ──

pub async fn handle_cancel(
    db: &DatabaseConnection,
    channel_id: i32,
    sender_id: &str,
    conn_mgr: &ConnectionManager,
    bridge: &Arc<Mutex<SessionBridge>>,
    lang: Lang,
) -> RichMessage {
    let ctx = match sender_context_service::get_or_create(db, channel_id, sender_id).await {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_load_context_label(lang)));
        }
    };

    let connection_id = match &ctx.current_connection_id {
        Some(id) => id.clone(),
        None => {
            return RichMessage::info(i18n::no_active_session_to_cancel(lang));
        }
    };

    // Cancel the ACP connection (CAS-updates InProgress → Cancelled, emits
    // per-connection ConversationStatusChanged + global State patch when the
    // CAS wins). Do not write status again here — a second unconditional write
    // would emit a duplicate/out-of-order patch for the same user cancel.
    let _ = conn_mgr.cancel(db, &connection_id).await;

    // Remove from bridge
    bridge.lock().await.remove(&connection_id);

    // Clear session from context
    let _ = sender_context_service::clear_session(db, channel_id, sender_id).await;

    RichMessage::info(i18n::task_cancelled_body(lang)).with_title(i18n::task_cancelled_title(lang))
}

// ── /approve, /deny ──

#[allow(clippy::too_many_arguments)]
pub async fn handle_permission_response(
    approve: bool,
    always: bool,
    db: &DatabaseConnection,
    channel_id: i32,
    sender_id: &str,
    conn_mgr: &ConnectionManager,
    bridge: &Arc<Mutex<SessionBridge>>,
    lang: Lang,
) -> RichMessage {
    let ctx = match sender_context_service::get_or_create(db, channel_id, sender_id).await {
        Ok(c) => c,
        Err(e) => {
            return RichMessage::error(format!("{}{e}", i18n::failed_to_load_context_label(lang)));
        }
    };

    let connection_id = match &ctx.current_connection_id {
        Some(id) => id.clone(),
        None => {
            return RichMessage::info(i18n::no_active_session(lang));
        }
    };

    let pending = {
        let mut bridge_guard = bridge.lock().await;
        let session = match bridge_guard.get_mut(&connection_id) {
            Some(s) => s,
            None => {
                return RichMessage::info(i18n::no_active_session_found(lang));
            }
        };
        session.permission_pending.take()
    };

    let pending = match pending {
        Some(p) => p,
        None => {
            return RichMessage::info(i18n::no_pending_permission(lang));
        }
    };

    // Find the appropriate option_id
    let option_id = if approve {
        pending
            .options
            .iter()
            .find(|o| o.kind == "allow" || o.kind == "allowForSession")
            .or_else(|| pending.options.first())
            .map(|o| o.option_id.clone())
    } else {
        pending
            .options
            .iter()
            .find(|o| o.kind == "deny")
            .or_else(|| pending.options.last())
            .map(|o| o.option_id.clone())
    };

    let Some(option_id) = option_id else {
        return RichMessage::info(i18n::no_valid_permission_option(lang));
    };

    if let Err(e) = conn_mgr
        .respond_permission(&connection_id, &pending.request_id, &option_id)
        .await
    {
        return RichMessage::error(format!(
            "{}{e}",
            i18n::failed_permission_response_label(lang)
        ));
    }

    // Update auto_approve if requested
    if always && approve {
        let _ = sender_context_service::update_auto_approve(db, channel_id, sender_id, true).await;
    }

    let action = if approve {
        i18n::approved_label(lang)
    } else {
        i18n::denied_label(lang)
    };

    let mut msg = RichMessage::info(format!("{}: {}", action, pending.tool_description));
    if always && approve {
        msg = msg.with_field("", i18n::auto_approve_enabled(lang));
    }
    msg.with_title(i18n::permission_response_title(lang))
}

// ── follow-up (non-command text) ──

pub async fn handle_followup(req: FollowupRequest<'_>) -> RichMessage {
    let ctx =
        match sender_context_service::get_or_create(req.db, req.channel_id, req.sender_id).await {
            Ok(c) => c,
            Err(e) => {
                return RichMessage::error(format!(
                    "{}{e}",
                    i18n::failed_to_load_context_label(req.lang)
                ));
            }
        };

    let connection_id = match &ctx.current_connection_id {
        Some(id) => id.clone(),
        None => {
            return RichMessage::info(i18n::no_active_session_use_task(req.lang, req.prefix));
        }
    };

    // Check connection exists in bridge; take the pre-created conversation id
    // so folder lookup is authoritative (not the sender's current folder).
    let conversation_id = {
        let bridge_guard = req.bridge.lock().await;
        match bridge_guard.get(&connection_id) {
            Some(session) => session.conversation_id,
            None => {
                // Connection lost, clear context
                drop(bridge_guard);
                let _ =
                    sender_context_service::clear_session(req.db, req.channel_id, req.sender_id)
                        .await;
                return RichMessage::info(i18n::session_connection_lost(req.lang, req.prefix));
            }
        }
    };

    if let Err(e) = send_prompt_linked_for_chat(
        req.db,
        req.conn_mgr,
        &connection_id,
        conversation_id,
        req.text,
    )
    .await
    {
        // A turn is already in flight on this (shared) connection — another
        // client, or a previous prompt still running. This is transient: the
        // connection is alive, so do NOT tear down the bridge/session. Tell the
        // user to retry once the current turn finishes.
        if matches!(e, crate::acp::error::AcpError::TurnInProgress) {
            return RichMessage::info(i18n::agent_busy_retry(req.lang).to_string());
        }
        // Otherwise the connection may have died — clean up.
        req.bridge.lock().await.remove(&connection_id);
        let _ = sender_context_service::clear_session(req.db, req.channel_id, req.sender_id).await;
        return RichMessage::error(format!(
            "{}{e}",
            i18n::failed_to_send_message_label(req.lang)
        ));
    }

    RichMessage::info(i18n::message_sent(req.lang))
}

// ── /resume (list recent) ──

async fn list_recent_sessions(db: &DatabaseConnection, lang: Lang, prefix: &str) -> RichMessage {
    let recent = match conversation::Entity::find()
        .filter(conversation::Column::DeletedAt.is_null())
        .order_by_desc(conversation::Column::CreatedAt)
        .limit(10)
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return RichMessage {
                title: Some(i18n::query_failed_title(lang).to_string()),
                body: e.to_string(),
                fields: Vec::new(),
                level: MessageLevel::Error,
            };
        }
    };

    if recent.is_empty() {
        return RichMessage::info(i18n::no_conversations_found(lang))
            .with_title(i18n::recent_conversations_title(lang));
    }

    let mut body = String::new();
    for conv in &recent {
        let title = conv.title.as_deref().unwrap_or(i18n::untitled(lang));
        let agent = &conv.agent_type;
        let time = conv.created_at.format("%m-%d %H:%M");
        body.push_str(&format!("#{} [{}] {} ({})\n", conv.id, agent, title, time,));
    }

    body.push_str(&format!("\n{}", i18n::recent_resume_hint(lang, prefix)));

    RichMessage::info(body.trim_end()).with_title(i18n::recent_conversations_title(lang))
}

// ── Helpers ──

fn agent_type_to_string(at: AgentType) -> String {
    serde_json::to_value(at)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn parse_agent_type(name: &str) -> Option<AgentType> {
    let normalized = name.to_lowercase().replace([' ', '-'], "_");
    serde_json::from_value(serde_json::Value::String(normalized)).ok()
}

fn resolve_agent_type(
    sender_agent: &Option<String>,
    folder_default: &Option<AgentType>,
) -> Option<AgentType> {
    if let Some(ref at_str) = sender_agent {
        if let Some(at) = parse_agent_type(at_str) {
            return Some(at);
        }
    }
    folder_default.as_ref().copied()
}

fn truncate_title(s: &str) -> String {
    if s.chars().count() <= 80 {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(77).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::auto_title::{ConnectionPurpose, PromptCaptureContext};
    use crate::commands::conversation_experience::KEY_AUTO_TITLE_AGENT;
    use crate::commands::system_settings::SYSTEM_LANGUAGE_SETTINGS_KEY;
    use crate::db::entities::auto_title_job;
    use crate::db::service::{app_metadata_service, conversation_service, sender_context_service};
    use crate::db::test_helpers;
    use crate::models::system::{AppLocale, LanguageMode, SystemLanguageSettings};
    use crate::web::event_bridge::EventEmitter;
    use sea_orm::EntityTrait;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::Mutex;

    #[test]
    fn lang_to_app_locale_maps_all_ten_languages() {
        let cases = [
            (Lang::En, AppLocale::En),
            (Lang::ZhCn, AppLocale::ZhCn),
            (Lang::ZhTw, AppLocale::ZhTw),
            (Lang::Ja, AppLocale::Ja),
            (Lang::Ko, AppLocale::Ko),
            (Lang::Es, AppLocale::Es),
            (Lang::De, AppLocale::De),
            (Lang::Fr, AppLocale::Fr),
            (Lang::Pt, AppLocale::Pt),
            (Lang::Ar, AppLocale::Ar),
        ];
        for (lang, expected) in cases {
            assert_eq!(
                i18n::lang_to_app_locale(lang),
                expected,
                "lang {lang:?} → AppLocale"
            );
        }
    }

    #[tokio::test]
    async fn channel_locale_prefers_configured_message_language() {
        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            SYSTEM_LANGUAGE_SETTINGS_KEY,
            &serde_json::to_string(&SystemLanguageSettings {
                mode: LanguageMode::Manual,
                language: AppLocale::Ko,
            })
            .expect("serialize"),
        )
        .await
        .expect("system language");
        app_metadata_service::upsert_value(&db.conn, "chat_message_language", "zh-cn")
            .await
            .expect("channel language");

        let locale = resolve_channel_app_locale(&db.conn).await;
        assert_eq!(locale, AppLocale::ZhCn);

        let launch = channel_launch_context_from_db(&db.conn).await;
        assert_eq!(launch.purpose, ConnectionPurpose::User);
        assert_eq!(launch.inherited_locale, Some(AppLocale::ZhCn));
    }

    #[tokio::test]
    async fn channel_locale_falls_back_to_system_when_missing_or_invalid() {
        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            SYSTEM_LANGUAGE_SETTINGS_KEY,
            &serde_json::to_string(&SystemLanguageSettings {
                mode: LanguageMode::Manual,
                language: AppLocale::Fr,
            })
            .expect("serialize"),
        )
        .await
        .expect("system language");

        // Missing channel language → system Fr.
        assert_eq!(
            resolve_channel_app_locale(&db.conn).await,
            AppLocale::Fr,
            "missing channel language falls back to system language"
        );

        // Invalid channel language → system Fr (not forced English).
        app_metadata_service::upsert_value(&db.conn, "chat_message_language", "klingon")
            .await
            .expect("invalid channel language");
        assert_eq!(
            resolve_channel_app_locale(&db.conn).await,
            AppLocale::Fr,
            "invalid channel language falls back to system language"
        );
    }

    /// Resume launch policy loads channel locale and does not enroll a new job.
    #[tokio::test]
    async fn resume_launch_gets_channel_locale_without_enrolling_title_job() {
        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).expect("serialize"),
        )
        .await
        .expect("enable auto title");
        app_metadata_service::upsert_value(
            &db.conn,
            SYSTEM_LANGUAGE_SETTINGS_KEY,
            &serde_json::to_string(&SystemLanguageSettings {
                mode: LanguageMode::Manual,
                language: AppLocale::En,
            })
            .expect("serialize"),
        )
        .await
        .expect("system language");
        app_metadata_service::upsert_value(&db.conn, "chat_message_language", "ko")
            .await
            .expect("channel language");

        let folder_id = test_helpers::seed_folder(&db, "/tmp/chat-resume-locale").await;
        let conv = conversation_service::create(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("existing".into()),
            None,
        )
        .await
        .expect("existing conversation");
        let jobs_before = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("jobs");
        assert_eq!(jobs_before.len(), 1, "create enrolls exactly one job");

        // Production resume/task launch helper — no create, no enroll.
        let launch = channel_launch_context_from_db(&db.conn).await;
        assert_eq!(launch.purpose, ConnectionPurpose::User);
        assert_eq!(
            launch.inherited_locale,
            Some(AppLocale::Ko),
            "resume/root launch must inherit channel locale"
        );

        let jobs_after = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("jobs");
        assert_eq!(
            jobs_after.len(),
            1,
            "launch context resolution must not enroll a new title job"
        );
        assert_eq!(jobs_after[0].conversation_id, conv.id);
    }

    /// Follow-up uses the conversation row's folder, not the sender's current folder.
    #[tokio::test]
    async fn followup_uses_conversation_folder_not_sender_current_folder() {
        use crate::db::entities::chat_channel;
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, ActiveValue::NotSet, Set};

        let db = test_helpers::fresh_in_memory_db().await;
        app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).expect("serialize"),
        )
        .await
        .expect("enable auto title");
        app_metadata_service::upsert_value(&db.conn, "chat_message_language", "de")
            .await
            .expect("channel language");

        let auth_folder = test_helpers::seed_folder(&db, "/tmp/chat-auth-folder").await;
        let other_folder = test_helpers::seed_folder(&db, "/tmp/chat-other-folder").await;
        let conv = conversation_service::create(
            &db.conn,
            auth_folder,
            AgentType::ClaudeCode,
            Some("linked".into()),
            None,
        )
        .await
        .expect("conversation in auth folder");

        let now = Utc::now();
        let channel = chat_channel::ActiveModel {
            id: NotSet,
            name: Set("test-channel".into()),
            channel_type: Set("telegram".into()),
            enabled: Set(true),
            config_json: Set("{}".into()),
            event_filter_json: Set(None),
            daily_report_enabled: Set(false),
            daily_report_time: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("seed chat channel");
        let channel_id = channel.id;
        let sender_id = "sender-1";
        // Sender switched to a different folder after the conversation was created.
        sender_context_service::update_folder(&db.conn, channel_id, sender_id, Some(other_folder))
            .await
            .expect("sender folder");
        sender_context_service::update_session(
            &db.conn,
            channel_id,
            sender_id,
            Some(conv.id),
            Some("follow-conn".into()),
        )
        .await
        .expect("sender session");

        let bridge = Arc::new(Mutex::new(SessionBridge::new()));
        bridge.lock().await.register(
            "follow-conn".into(),
            ActiveSession {
                channel_id,
                sender_id: sender_id.into(),
                conversation_id: conv.id,
                connection_id: "follow-conn".into(),
                agent_type: AgentType::ClaudeCode,
                content_buffer: String::new(),
                tool_calls: Vec::new(),
                tool_call_inputs: HashMap::new(),
                delegation_rendered: HashSet::new(),
                last_flushed: Instant::now(),
                pending_prompt: None,
                permission_pending: None,
            },
        );

        let mgr = ConnectionManager::new();
        let mut cmd_rx = mgr
            .insert_test_connection_live(
                "follow-conn",
                AgentType::ClaudeCode,
                Some(PathBuf::from("/tmp/chat-auth-folder")),
                EventEmitter::Noop,
            )
            .await;
        {
            let state = mgr.get_state("follow-conn").await.unwrap();
            let mut s = state.write().await;
            // Unlinked until first linked send (resume-style).
            s.conversation_id = None;
            s.folder_id = None;
            s.purpose = ConnectionPurpose::User;
            s.effective_locale = AppLocale::En;
        }

        let follow_text = "continue with the plan";
        let msg = handle_followup(FollowupRequest {
            db: &db.conn,
            text: follow_text,
            channel_id,
            sender_id,
            conn_mgr: &mgr,
            bridge: &bridge,
            lang: Lang::De,
            prefix: "/",
        })
        .await;
        assert_eq!(
            msg.level,
            MessageLevel::Info,
            "follow-up should succeed, got {msg:?}"
        );

        {
            let state = mgr.get_state("follow-conn").await.unwrap();
            let s = state.read().await;
            assert_eq!(s.conversation_id, Some(conv.id));
            assert_eq!(
                s.folder_id,
                Some(auth_folder),
                "conversation row folder wins over sender current folder {other_folder}"
            );
            assert_ne!(s.folder_id, Some(other_folder));
        }

        let job = auto_title_job::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .expect("query")
            .expect("job");
        assert_eq!(job.first_user_text.as_deref(), Some(follow_text));
        assert_eq!(job.locale.as_deref(), Some("de"));

        // Drain: ensure a prompt was enqueued (send reached manager).
        let mut saw_prompt = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            if matches!(
                cmd,
                crate::acp::connection::ConnectionCommand::Prompt { .. }
            ) {
                saw_prompt = true;
            }
        }
        assert!(saw_prompt, "follow-up must enqueue a Prompt command");

        // No conversation should have been created in the sender's other folder.
        let in_other =
            conversation_service::list_by_folder(&db.conn, other_folder, None, None, None, None)
                .await
                .expect("list other folder");
        assert!(
            in_other.is_empty(),
            "sender current folder must not receive a redirected conversation"
        );

        // Capture constructor shape required by chat producers.
        let capture = PromptCaptureContext::new(Some(follow_text.into()), Some(AppLocale::De));
        assert_eq!(capture.visible_text.as_deref(), Some(follow_text));
        assert_eq!(capture.locale, Some(AppLocale::De));
    }
}
