use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sea_orm::DatabaseConnection;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use super::i18n::Lang;
use super::session_bridge::{PendingPermission, SessionBridge};
use super::types::{MessageLevel, RichMessage};
use crate::acp::internal_bus::InternalEventBus;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::{AcpEvent, ConnectionStatus, EventEnvelope, PromptInputBlock};

use crate::db::service::{app_metadata_service, conversation_service, sender_context_service};

use super::manager::ChatChannelManager;

const FLUSH_INTERVAL_SECS: u64 = 10;
const BUFFER_FLUSH_THRESHOLD: usize = 500;
const MAX_MESSAGE_LEN: usize = 2000;
const MESSAGE_LANGUAGE_KEY: &str = "chat_message_language";
const COMMAND_PREFIX_KEY: &str = "chat_command_prefix";
const DEFAULT_COMMAND_PREFIX: &str = "/";

pub fn spawn_session_event_subscriber(
    bus: Arc<InternalEventBus>,
    bridge: Arc<Mutex<SessionBridge>>,
    manager: ChatChannelManager,
    conn_mgr: ConnectionManager,
    db_conn: DatabaseConnection,
) -> JoinHandle<()> {
    let mut rx = bus.subscribe();
    let metrics = Arc::clone(bus.metrics());

    tokio::spawn(async move {
        let mut last_heartbeat = Instant::now();

        loop {
            tokio::select! {
                result = rx.recv() => {
                    let envelope_arc = match result {
                        Ok(e) => e,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[SessionEventSub] lagged {n} events");
                            metrics.lagged_count.fetch_add(n, Ordering::Relaxed);
                            continue;
                        }
                        Err(_) => break,
                    };

                    handle_acp_envelope(
                        envelope_arc.as_ref(),
                        &bridge,
                        &manager,
                        &conn_mgr,
                        &db_conn,
                    )
                    .await;
                }
                _ = tokio::time::sleep(Duration::from_secs(FLUSH_INTERVAL_SECS)) => {
                    if last_heartbeat.elapsed() >= Duration::from_secs(FLUSH_INTERVAL_SECS) {
                        flush_progress(&bridge, &manager, &db_conn).await;
                        last_heartbeat = Instant::now();
                    }
                }
            }
        }
    })
}

async fn get_lang(db: &DatabaseConnection) -> Lang {
    app_metadata_service::get_value(db, MESSAGE_LANGUAGE_KEY)
        .await
        .ok()
        .flatten()
        .map(|v| Lang::from_str_lossy(&v))
        .unwrap_or_default()
}

async fn get_prefix(db: &DatabaseConnection) -> String {
    app_metadata_service::get_value(db, COMMAND_PREFIX_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_COMMAND_PREFIX.to_string())
}

/// Phase 5: typed-envelope dispatcher. Replaces the prior JSON
/// `payload.get("type").as_str()` switch — every accessor we used to need
/// (type / connection_id / event-specific fields) is now a structural
/// match on `AcpEvent`, with no `unwrap_or("")` defensive fallbacks.
async fn handle_acp_envelope(
    envelope: &EventEnvelope,
    bridge: &Arc<Mutex<SessionBridge>>,
    manager: &ChatChannelManager,
    conn_mgr: &ConnectionManager,
    db: &DatabaseConnection,
) {
    let connection_id = envelope.connection_id.as_str();

    match &envelope.payload {
        AcpEvent::SessionStarted { session_id } => {
            let mut guard = bridge.lock().await;
            if let Some(session) = guard.get_mut(connection_id) {
                let _ = conversation_service::update_external_id(
                    db,
                    session.conversation_id,
                    session_id.clone(),
                )
                .await;

                if let Some(prompt_text) = session.pending_prompt.take() {
                    let blocks = vec![PromptInputBlock::Text { text: prompt_text }];
                    if let Err(e) = conn_mgr.send_prompt(connection_id, blocks).await {
                        eprintln!("[SessionEventSub] failed to send pending prompt: {e}");
                        let channel_id = session.channel_id;
                        let msg = RichMessage::error(format!("Failed to send task: {e}"));
                        let _ = manager.send_to_channel(channel_id, &msg).await;
                    }
                }
            }
        }

        AcpEvent::ContentDelta { text } => {
            // Collect flush info under the lock, then release before any IO.
            let flush_info: Option<(i32, String, Option<String>)> = {
                let mut guard = bridge.lock().await;
                match guard.get_mut(connection_id) {
                    Some(session) => {
                        session.content_buffer.push_str(text);
                        if session.content_buffer.len() >= BUFFER_FLUSH_THRESHOLD
                            && session.last_flushed.elapsed() >= Duration::from_secs(2)
                        {
                            session.last_flushed = Instant::now();
                            Some((
                                session.channel_id,
                                session.agent_type.to_string(),
                                session.tool_calls.last().cloned(),
                            ))
                        } else {
                            None
                        }
                    }
                    None => None,
                }
            };

            if let Some((channel_id, agent_label, last_tool)) = flush_info {
                let lang = get_lang(db).await;
                let mut status = super::i18n::agent_responding(lang, &agent_label);
                if let Some(tool) = last_tool {
                    status.push_str(&format!(" | {tool}"));
                }
                let msg = RichMessage::info(status);
                let _ = manager.send_to_channel(channel_id, &msg).await;
            }
        }

        AcpEvent::ToolCall {
            tool_call_id,
            title,
            raw_input,
            ..
        } => {
            // Emit a "delegation started" placeholder to the channel so
            // remote users see something happen as soon as the parent agent
            // fires `delegate_to_agent`, not only when the child wraps up.
            let delegation_announce = if is_delegation_title(title) {
                raw_input
                    .as_deref()
                    .and_then(extract_agent_type)
                    .map(|agent| format!("🤖 Delegating to {agent}…"))
            } else {
                None
            };

            let mut guard = bridge.lock().await;
            if let Some(session) = guard.get_mut(connection_id) {
                // Store title for progress indicator; store raw_input for later
                session.tool_calls.push(title.clone());
                if let Some(input) = raw_input.as_deref() {
                    session
                        .tool_call_inputs
                        .insert(tool_call_id.clone(), input.to_string());
                }
                if let Some(text) = delegation_announce {
                    let channel_id = session.channel_id;
                    drop(guard);
                    let msg = RichMessage::info(text);
                    let _ = manager.send_to_channel(channel_id, &msg).await;
                }
            }
        }

        AcpEvent::ToolCallUpdate {
            tool_call_id,
            title,
            status,
            raw_input,
            raw_output,
            ..
        } => {
            let mut guard = bridge.lock().await;
            if let Some(session) = guard.get_mut(connection_id) {
                // Accumulate raw_input if newly available
                if let Some(input) = raw_input.as_deref() {
                    session
                        .tool_call_inputs
                        .insert(tool_call_id.clone(), input.to_string());
                }

                if status.as_deref() == Some("completed") {
                    let stored_input = session.tool_call_inputs.remove(tool_call_id);
                    let effective_title = title.as_deref().unwrap_or("tool");
                    let input_ref = stored_input.as_deref().or(raw_input.as_deref());
                    let channel_id = session.channel_id;

                    let body = if is_delegation_title(effective_title)
                        || input_ref
                            .map(|s| extract_agent_type(s).is_some())
                            .unwrap_or(false)
                    {
                        format_delegation_outcome(input_ref, raw_output.as_deref())
                    } else {
                        format!(">> {}", format_tool_call_detail(effective_title, input_ref))
                    };
                    drop(guard);

                    let msg = RichMessage::info(body);
                    let _ = manager.send_to_channel(channel_id, &msg).await;
                }
            }
        }

        AcpEvent::PermissionRequest {
            request_id,
            tool_call,
            options,
        } => {
            let mut guard = bridge.lock().await;
            if let Some(session) = guard.get_mut(connection_id) {
                let channel_id = session.channel_id;
                let sender_id = session.sender_id.clone();

                let auto_approve =
                    sender_context_service::get_or_create(db, channel_id, &sender_id)
                        .await
                        .map(|ctx| ctx.auto_approve)
                        .unwrap_or(false);

                if auto_approve {
                    let option_id = options
                        .iter()
                        .find(|o| o.kind == "allow" || o.kind == "allowForSession")
                        .or_else(|| options.first())
                        .map(|o| o.option_id.clone());

                    drop(guard);

                    if let Some(oid) = option_id {
                        let _ = conn_mgr
                            .respond_permission(connection_id, request_id, &oid)
                            .await;
                    }
                    return;
                }

                let tool_title = tool_call
                    .get("title")
                    .and_then(|v| v.as_str())
                    .or_else(|| tool_call.get("tool_name").and_then(|v| v.as_str()))
                    .unwrap_or("Unknown tool");

                // Extract detail from rawInput / raw_input in the tool_call object
                let raw_input_str = tool_call
                    .get("rawInput")
                    .or_else(|| tool_call.get("raw_input"))
                    .and_then(|v| match v {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Null => None,
                        other => Some(other.to_string()),
                    });
                let tool_desc = format_tool_call_detail(tool_title, raw_input_str.as_deref());

                session.permission_pending = Some(PendingPermission {
                    request_id: request_id.clone(),
                    tool_description: tool_desc.clone(),
                    options: options.clone(),
                    sent_message_id: None,
                });

                drop(guard);

                let lang = get_lang(db).await;
                let prefix = get_prefix(db).await;
                let body = match lang {
                    Lang::ZhCn | Lang::ZhTw => {
                        format!("Agent 请求权限: {tool_desc}\n\n{prefix}approve 批准 | {prefix}deny 拒绝 | {prefix}approve always 自动批准")
                    }
                    _ => {
                        format!("Agent requests permission: {tool_desc}\n\n{prefix}approve | {prefix}deny | {prefix}approve always")
                    }
                };

                let msg = RichMessage {
                    title: Some(match lang {
                        Lang::ZhCn | Lang::ZhTw => "权限请求".to_string(),
                        _ => "Permission Request".to_string(),
                    }),
                    body,
                    fields: Vec::new(),
                    level: MessageLevel::Warning,
                };
                let _ = manager.send_to_channel(channel_id, &msg).await;
            }
        }

        AcpEvent::TurnComplete {
            stop_reason,
            agent_type,
            ..
        } => {
            let mut guard = bridge.lock().await;
            if let Some(session) = guard.get_mut(connection_id) {
                let channel_id = session.channel_id;
                let conv_id = session.conversation_id;
                let content = std::mem::take(&mut session.content_buffer);
                let tool_count = session.tool_calls.len();
                session.tool_calls.clear();
                session.last_flushed = Instant::now();
                drop(guard);

                let lang = get_lang(db).await;
                let body = format_completion(&content, tool_count, lang);

                let msg = RichMessage::info(body)
                    .with_title(match lang {
                        Lang::ZhCn | Lang::ZhTw => "任务完成",
                        _ => "Turn Complete",
                    })
                    .with_field("Agent", agent_type)
                    .with_field(
                        match lang {
                            Lang::ZhCn | Lang::ZhTw => "结束原因",
                            _ => "Stop Reason",
                        },
                        localize_stop_reason(stop_reason, lang),
                    );

                let _ = manager.send_to_channel(channel_id, &msg).await;

                if stop_reason == "end_turn" {
                    let _ = conversation_service::update_status(
                        db,
                        conv_id,
                        crate::db::entities::conversation::ConversationStatus::Completed,
                    )
                    .await;
                }
            }
        }

        AcpEvent::Error {
            message,
            agent_type,
            terminal,
            ..
        } => {
            // Non-terminal Errors (`turn_failure_error_event`,
            // `session/load` fallback, empty-prompt rejection, SetMode /
            // SetConfigOption failures) leave the ACP connection alive —
            // the next prompt on the same session will still work. Posting
            // the error to the channel is useful, but tearing down the
            // bridge session and flipping the conversation row to
            // Cancelled would break remote chat-channel users (their next
            // message would spawn a brand-new session, losing context).
            // The lifecycle worker mirrors this gating; see F2 in the
            // v0.14.3 sub-agent delegation post-mortem.
            let lang = get_lang(db).await;
            let msg = RichMessage {
                title: Some(match lang {
                    Lang::ZhCn | Lang::ZhTw => "Agent 错误".to_string(),
                    _ => "Agent Error".to_string(),
                }),
                body: format!("[{agent_type}] {message}"),
                fields: Vec::new(),
                level: MessageLevel::Error,
            };

            if !*terminal {
                let channel_id = {
                    let guard = bridge.lock().await;
                    guard.get(connection_id).map(|s| s.channel_id)
                };
                if let Some(channel_id) = channel_id {
                    let _ = manager.send_to_channel(channel_id, &msg).await;
                }
                return;
            }

            let mut guard = bridge.lock().await;
            if let Some(session) = guard.remove(connection_id) {
                let channel_id = session.channel_id;
                let sender_id = session.sender_id.clone();
                let conv_id = session.conversation_id;
                drop(guard);

                let _ = manager.send_to_channel(channel_id, &msg).await;

                let _ = conversation_service::update_status(
                    db,
                    conv_id,
                    crate::db::entities::conversation::ConversationStatus::Cancelled,
                )
                .await;
                let _ = sender_context_service::clear_session(db, channel_id, &sender_id).await;
            }
        }

        AcpEvent::StatusChanged { status } => {
            if matches!(
                status,
                ConnectionStatus::Disconnected | ConnectionStatus::Error
            ) {
                let mut guard = bridge.lock().await;
                if let Some(session) = guard.remove(connection_id) {
                    let channel_id = session.channel_id;
                    let sender_id = session.sender_id.clone();
                    drop(guard);

                    let _ = sender_context_service::clear_session(db, channel_id, &sender_id).await;
                }
            }
        }

        _ => {}
    }
}

async fn flush_progress(
    bridge: &Arc<Mutex<SessionBridge>>,
    manager: &ChatChannelManager,
    db: &DatabaseConnection,
) {
    let lang = get_lang(db).await;
    let updates: Vec<(i32, String)> = {
        let mut guard = bridge.lock().await;
        let mut out = Vec::new();
        for session in guard.all_sessions_mut() {
            if !session.content_buffer.is_empty()
                && session.last_flushed.elapsed() >= Duration::from_secs(FLUSH_INTERVAL_SECS)
            {
                session.last_flushed = Instant::now();
                let last_tool = session.tool_calls.last().cloned();
                let agent_label = session.agent_type.to_string();
                let mut status = super::i18n::agent_responding(lang, &agent_label);
                if let Some(tool) = last_tool {
                    status.push_str(&format!(" | {tool}"));
                }
                out.push((session.channel_id, status));
            }
        }
        out
    };

    for (channel_id, text) in updates {
        let msg = RichMessage::info(text);
        let _ = manager.send_to_channel(channel_id, &msg).await;
    }
}

fn format_completion(content: &str, tool_count: usize, lang: Lang) -> String {
    if content.is_empty() {
        return match lang {
            Lang::ZhCn | Lang::ZhTw => format!("(无文本输出, {tool_count} 次工具调用)"),
            _ => format!("(No text output, {tool_count} tool calls)"),
        };
    }

    if content.len() <= MAX_MESSAGE_LEN {
        let mut body = content.to_string();
        if tool_count > 0 {
            body.push_str(&format!(
                "\n\n[{} {}]",
                tool_count,
                match lang {
                    Lang::ZhCn | Lang::ZhTw => "次工具调用",
                    _ => "tool calls",
                }
            ));
        }
        return body;
    }

    // Truncate long content (use char boundaries to avoid panic on multi-byte)
    let head_end = content
        .char_indices()
        .nth(500)
        .map(|(i, _)| i)
        .unwrap_or(content.len());
    let head = &content[..head_end];
    let tail_start = content
        .char_indices()
        .rev()
        .nth(499)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let tail = &content[tail_start..];

    match lang {
        Lang::ZhCn | Lang::ZhTw => {
            format!(
                "{head}\n\n...\n\n{tail}\n\n[完整回复: {} 字符, {tool_count} 次工具调用]",
                content.len()
            )
        }
        _ => {
            format!(
                "{head}\n\n...\n\n{tail}\n\n[Full response: {} chars, {tool_count} tool calls]",
                content.len()
            )
        }
    }
}

fn localize_stop_reason(reason: &str, lang: Lang) -> String {
    match lang {
        Lang::ZhCn => match reason {
            "end_turn" => "正常结束",
            "cancelled" => "已取消",
            "max_tokens" => "达到最大长度",
            "stop_sequence" => "遇到停止序列",
            "error" => "错误",
            "timeout" => "超时",
            other => other,
        },
        Lang::ZhTw => match reason {
            "end_turn" => "正常結束",
            "cancelled" => "已取消",
            "max_tokens" => "達到最大長度",
            "stop_sequence" => "遇到停止序列",
            "error" => "錯誤",
            "timeout" => "逾時",
            other => other,
        },
        Lang::Ja => match reason {
            "end_turn" => "正常終了",
            "cancelled" => "キャンセル",
            "max_tokens" => "最大トークン数到達",
            "stop_sequence" => "停止シーケンス",
            "error" => "エラー",
            "timeout" => "タイムアウト",
            other => other,
        },
        Lang::Ko => match reason {
            "end_turn" => "정상 종료",
            "cancelled" => "취소됨",
            "max_tokens" => "최대 길이 도달",
            "stop_sequence" => "정지 시퀀스",
            "error" => "오류",
            "timeout" => "시간 초과",
            other => other,
        },
        Lang::Es => match reason {
            "end_turn" => "Finalizado",
            "cancelled" => "Cancelado",
            "max_tokens" => "Longitud máxima alcanzada",
            "error" => "Error",
            "timeout" => "Tiempo agotado",
            other => other,
        },
        Lang::De => match reason {
            "end_turn" => "Abgeschlossen",
            "cancelled" => "Abgebrochen",
            "max_tokens" => "Maximale Länge erreicht",
            "error" => "Fehler",
            "timeout" => "Zeitüberschreitung",
            other => other,
        },
        Lang::Fr => match reason {
            "end_turn" => "Terminé",
            "cancelled" => "Annulé",
            "max_tokens" => "Longueur maximale atteinte",
            "error" => "Erreur",
            "timeout" => "Délai dépassé",
            other => other,
        },
        Lang::Pt => match reason {
            "end_turn" => "Concluído",
            "cancelled" => "Cancelado",
            "max_tokens" => "Comprimento máximo atingido",
            "error" => "Erro",
            "timeout" => "Tempo esgotado",
            other => other,
        },
        Lang::Ar => match reason {
            "end_turn" => "اكتمل",
            "cancelled" => "ملغى",
            "max_tokens" => "تم بلوغ الحد الأقصى",
            "error" => "خطأ",
            "timeout" => "انتهت المهلة",
            other => other,
        },
        Lang::En => match reason {
            "end_turn" => "Completed",
            "cancelled" => "Cancelled",
            "max_tokens" => "Max length reached",
            "stop_sequence" => "Stop sequence",
            "error" => "Error",
            "timeout" => "Timeout",
            other => other,
        },
    }
    .to_string()
}

/// Extract a concise detail string from a tool call's `raw_input` JSON.
///
/// Returns a formatted string like `"Read: src/main.rs"` or `"Bash: npm test"`.
/// Falls back to the original title if no detail can be extracted.
fn format_tool_call_detail(title: &str, raw_input: Option<&str>) -> String {
    let parsed = raw_input.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());

    let normalized_title = title.to_lowercase().replace([' ', '-'], "_");

    if let Some(ref obj) = parsed {
        // File operations: read, edit, write, delete
        if let Some(path) = obj
            .get("file_path")
            .or_else(|| obj.get("path"))
            .or_else(|| obj.get("notebook_path"))
            .and_then(|v| v.as_str())
        {
            let short = short_path(path);
            let label = match normalized_title.as_str() {
                s if s.contains("write") => "Write",
                s if s.contains("edit") || s.contains("change") || s.contains("update") => "Edit",
                s if s.contains("delete") => "Delete",
                _ => "Read",
            };
            return format!("{label}: {short}");
        }

        // Bash / shell commands
        if let Some(cmd) = obj
            .get("command")
            .or_else(|| obj.get("cmd"))
            .and_then(|v| v.as_str())
        {
            let short = truncate_str(cmd.lines().next().unwrap_or(cmd), 80);
            return format!("Bash: {short}");
        }

        // Grep / search
        if let Some(pattern) = obj.get("pattern").and_then(|v| v.as_str()) {
            let path = obj.get("path").and_then(|v| v.as_str());
            return if let Some(p) = path {
                format!(
                    "Grep: \"{}\" in {}",
                    truncate_str(pattern, 40),
                    short_path(p)
                )
            } else {
                format!("Grep: \"{}\"", truncate_str(pattern, 60))
            };
        }

        // Glob
        if let Some(pat) = obj.get("glob").and_then(|v| v.as_str()) {
            return format!("Glob: {pat}");
        }

        // Agent / task
        if obj.get("subagent_type").is_some()
            || obj.get("task_id").is_some()
            || obj.get("subject").is_some()
        {
            let desc = obj
                .get("description")
                .or_else(|| obj.get("subject"))
                .or_else(|| obj.get("prompt"))
                .and_then(|v| v.as_str());
            if let Some(d) = desc {
                return format!("Agent: {}", truncate_str(d, 60));
            }
        }

        // Web fetch
        if let Some(url) = obj.get("url").and_then(|v| v.as_str()) {
            return format!("Fetch: {}", truncate_str(url, 80));
        }

        // Web search
        if let Some(query) = obj.get("query").and_then(|v| v.as_str()) {
            return format!("Search: {}", truncate_str(query, 60));
        }

        // TodoWrite
        if obj.get("todos").is_some() {
            return "TodoWrite".to_string();
        }
    }

    // Fallback: if raw_input is a plain string (e.g. a bare command), use it directly
    if let Some(raw) = raw_input {
        if !raw.starts_with('{') && !raw.starts_with('[') {
            let short = truncate_str(raw.lines().next().unwrap_or(raw), 80);
            if normalized_title.contains("bash")
                || normalized_title.contains("shell")
                || normalized_title.contains("exec")
            {
                return format!("Bash: {short}");
            }
        }
    }

    title.to_string()
}

fn short_path(path: &str) -> &str {
    // Show last 2 path components at most, or the full path if short enough
    if path.len() <= 60 {
        return path;
    }
    let parts: Vec<&str> = path.rsplitn(3, '/').collect();
    if parts.len() >= 2 {
        // e.g. "src/main.rs" from "/very/long/path/src/main.rs"
        let tail = &path[path.len() - parts[0].len() - parts[1].len() - 1..];
        if tail.len() < path.len() {
            return tail;
        }
    }
    path
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Title-side match for `delegate_to_agent`. Title is free-form text the
/// host agent composes; some hosts copy the bare MCP method, some prefix
/// it with `mcp__<server>__`, some rephrase it. Match by substring so any
/// of those forms get the delegation-announcement path. The completion-
/// side callsite already pairs this with a raw_input shape check, so a
/// rare false-positive here just sends one announce message that gets
/// overwritten by the completion's actual outcome.
fn is_delegation_title(title: &str) -> bool {
    let normalized = title.to_lowercase().replace([' ', '-'], "_");
    normalized.contains("delegate_to_agent")
}

/// Pull `agent_type` out of the raw_input JSON (e.g. `{"agent_type":"codex",
/// "task":"..."}`). Returns the canonical string the agent supplied so the
/// announce message matches what the user wrote, not a re-mapped label.
fn extract_agent_type(raw_input: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(raw_input).ok()?;
    parsed
        .get("agent_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Build the chat-channel summary for a finished `delegate_to_agent` call.
/// Receives the broker's wire payload (already a JSON-serialized
/// `DelegationOutcome`) and renders a compact ✅/❌ line plus the short
/// preview text the user can act on.
fn format_delegation_outcome(raw_input: Option<&str>, raw_output: Option<&str>) -> String {
    let agent = raw_input
        .and_then(extract_agent_type)
        .unwrap_or_else(|| "agent".to_string());

    // Try to parse the MCP-style structured output Phase 5 emits:
    //   `{ "kind": "ok", "text": "…", … }` or `{ "kind": "err", "code": "…" }`.
    // Fall back to the plain text body if the agent already collapsed it.
    if let Some(out) = raw_output {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(out) {
            let kind = value.get("kind").and_then(|v| v.as_str());
            match kind {
                Some("ok") => {
                    let text = value
                        .get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim();
                    if text.is_empty() {
                        return format!("✅ {agent} done");
                    }
                    let preview = truncate_str(text, 200);
                    return format!("✅ {agent}: {preview}");
                }
                Some("err") => {
                    let code = value.get("code").and_then(|v| v.as_str()).unwrap_or("err");
                    return format!("❌ {agent} failed ({code})");
                }
                _ => {}
            }
        }
        let preview = truncate_str(out.trim(), 200);
        if !preview.is_empty() {
            return format!("✅ {agent}: {preview}");
        }
    }
    format!("✅ {agent} done")
}

#[cfg(test)]
mod delegation_relay_tests {
    use super::*;

    #[test]
    fn is_delegation_title_matches_variants() {
        assert!(is_delegation_title("delegate_to_agent"));
        assert!(is_delegation_title("Delegate To Agent"));
        assert!(is_delegation_title("delegate-to-agent"));
        assert!(is_delegation_title(
            "mcp__codeg-delegate__delegate_to_agent"
        ));
        assert!(is_delegation_title("Run mcp__codeg__delegate_to_agent"));
        assert!(!is_delegation_title("agent"));
        assert!(!is_delegation_title("write"));
    }

    #[test]
    fn extract_agent_type_pulls_canonical_string() {
        assert_eq!(
            extract_agent_type(r#"{"agent_type":"codex","task":"x"}"#),
            Some("codex".into())
        );
        assert_eq!(extract_agent_type(r#"{"task":"x"}"#), None);
        assert_eq!(extract_agent_type("not json"), None);
    }

    #[test]
    fn format_delegation_outcome_renders_ok_with_preview() {
        let out = r#"{"kind":"ok","text":"  hello world  "}"#;
        let body = format_delegation_outcome(Some(r#"{"agent_type":"codex"}"#), Some(out));
        assert_eq!(body, "✅ codex: hello world");
    }

    #[test]
    fn format_delegation_outcome_renders_err_with_code() {
        let out = r#"{"kind":"err","code":"timeout"}"#;
        let body = format_delegation_outcome(Some(r#"{"agent_type":"gemini"}"#), Some(out));
        assert_eq!(body, "❌ gemini failed (timeout)");
    }

    #[test]
    fn format_delegation_outcome_falls_back_to_plain_text() {
        let body =
            format_delegation_outcome(Some(r#"{"agent_type":"cline"}"#), Some("plain reply body"));
        assert_eq!(body, "✅ cline: plain reply body");
    }

    #[test]
    fn format_delegation_outcome_empty_output_marks_done() {
        let body = format_delegation_outcome(Some(r#"{"agent_type":"open_code"}"#), None);
        assert_eq!(body, "✅ open_code done");
    }

    #[test]
    fn format_delegation_outcome_truncates_long_ok_text() {
        let long_text = "x".repeat(400);
        let out = format!(r#"{{"kind":"ok","text":"{long_text}"}}"#);
        let body = format_delegation_outcome(Some(r#"{"agent_type":"codex"}"#), Some(&out));
        // 200-char cap + "..."
        assert!(body.len() < 300);
        assert!(body.starts_with("✅ codex: "));
        assert!(body.ends_with("..."));
    }
}

#[cfg(test)]
mod error_terminal_gate_tests {
    //! Regression coverage for the F2-aligned `AcpEvent::Error` gating —
    //! non-terminal Errors must leave the chat-channel session and the
    //! conversation row untouched, so a recoverable failure (turn refusal,
    //! `session/load` fallback, idle SetMode failure) doesn't kill the
    //! remote user's bridge session. Terminal Errors continue to tear the
    //! session down as before. (P2 follow-up to the v0.14.3 sub-agent
    //! delegation post-mortem.)
    use super::*;
    use crate::acp::manager::ConnectionManager;
    use crate::acp::types::{AcpEvent, EventEnvelope};
    use crate::chat_channel::manager::ChatChannelManager;
    use crate::chat_channel::session_bridge::{ActiveSession, SessionBridge};
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::test_helpers;
    use crate::models::agent::AgentType;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::Mutex;

    async fn read_row_status(db: &crate::db::AppDatabase, id: i32) -> ConversationStatus {
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        conversation::Entity::find_by_id(id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists")
            .status
    }

    async fn seed_session(
        db: &crate::db::AppDatabase,
        connection_id: &str,
    ) -> (Arc<Mutex<SessionBridge>>, i32) {
        let folder_id = test_helpers::seed_folder(db, "/tmp/chat-error-gate").await;
        let conv_id = test_helpers::seed_conversation(db, folder_id, AgentType::ClaudeCode).await;
        let bridge = Arc::new(Mutex::new(SessionBridge::new()));
        bridge.lock().await.register(
            connection_id.to_string(),
            ActiveSession {
                channel_id: 7,
                sender_id: "u1".into(),
                conversation_id: conv_id,
                connection_id: connection_id.to_string(),
                agent_type: AgentType::ClaudeCode,
                content_buffer: String::new(),
                tool_calls: Vec::new(),
                tool_call_inputs: std::collections::HashMap::new(),
                last_flushed: Instant::now(),
                pending_prompt: None,
                permission_pending: None,
            },
        );
        (bridge, conv_id)
    }

    #[tokio::test]
    async fn non_terminal_error_keeps_session_and_conversation_intact() {
        let db = test_helpers::fresh_in_memory_db().await;
        let (bridge, conv_id) = seed_session(&db, "c-nonterm").await;
        let chat_mgr = ChatChannelManager::new();
        let conn_mgr = ConnectionManager::new();

        let envelope = EventEnvelope {
            seq: 1,
            connection_id: "c-nonterm".to_string(),
            payload: AcpEvent::Error {
                message: "Failed to set mode: bad id".into(),
                agent_type: "claude_code".into(),
                code: None,
                terminal: false,
            },
        };
        handle_acp_envelope(&envelope, &bridge, &chat_mgr, &conn_mgr, &db.conn).await;

        // Session bridge entry is preserved — the next user message on the
        // same connection can still flow through it.
        assert!(
            bridge.lock().await.get("c-nonterm").is_some(),
            "non-terminal Error must leave the bridge session in place"
        );
        assert_eq!(
            read_row_status(&db, conv_id).await,
            ConversationStatus::InProgress,
            "non-terminal Error must not flip the conversation to Cancelled"
        );
    }

    #[tokio::test]
    async fn terminal_error_tears_down_session_and_writes_cancelled() {
        let db = test_helpers::fresh_in_memory_db().await;
        let (bridge, conv_id) = seed_session(&db, "c-term").await;
        let chat_mgr = ChatChannelManager::new();
        let conn_mgr = ConnectionManager::new();

        let envelope = EventEnvelope {
            seq: 1,
            connection_id: "c-term".to_string(),
            payload: AcpEvent::Error {
                message: "transport closed".into(),
                agent_type: "claude_code".into(),
                code: None,
                terminal: true,
            },
        };
        handle_acp_envelope(&envelope, &bridge, &chat_mgr, &conn_mgr, &db.conn).await;

        assert!(
            bridge.lock().await.get("c-term").is_none(),
            "terminal Error must remove the bridge session so the next message starts fresh"
        );
        assert_eq!(
            read_row_status(&db, conv_id).await,
            ConversationStatus::Cancelled
        );
    }
}
