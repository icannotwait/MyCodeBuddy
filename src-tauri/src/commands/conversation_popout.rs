//! Conversation pop-out window lifecycle (local desktop only).
//!
//! Ownership is **incarnation-scoped** by `operation_id`, not window label alone.
//! Labels reuse `conversation-{id}` across reopen; delayed cleanup for op A must
//! not kill op B.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::app_error::AppCommandError;
use crate::commands::windows::{apply_platform_window_style, post_window_setup};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;

#[cfg(feature = "tauri-runtime")]
use crate::acp::manager::ConnectionManager;
#[cfg(feature = "tauri-runtime")]
use crate::terminal::manager::TerminalManager;

/// Window label for a detached conversation.
pub fn conversation_window_label(conversation_id: i32) -> String {
    format!("conversation-{conversation_id}")
}

pub fn parse_conversation_id_from_label(label: &str) -> Option<i32> {
    label
        .strip_prefix("conversation-")?
        .parse::<i32>()
        .ok()
        .filter(|&id| id > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PopoutPhase {
    Opening,
    ReadyPending,
    HandoffComplete,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AbortOutcome {
    NeverRebound,
    AlreadyMain,
    Reversed { generation: u64 },
    Superseded {
        current_generation: u64,
        current_owner: String,
    },
    AlreadyComplete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PopoutOpStatus {
    pub phase: PopoutPhase,
    pub conversation_id: i32,
    pub operation_id: String,
    pub ownership_generation: Option<u64>,
    pub abort_outcome: Option<AbortOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OpenConversationResult {
    Opened,
    FocusedExisting,
}

pub use crate::acp::owner_rebind::RebindResult;

#[derive(Debug, Clone)]
pub struct OpRecord {
    conversation_id: i32,
    operation_id: String,
    phase: PopoutPhase,
    /// Generation written by forward rebind, if any.
    ownership_generation: Option<u64>,
    /// True between admit_forward_rebind and record_rebind (or reverse-on-fail).
    /// Abort must not treat this as NeverRebound — ownership may already have moved.
    rebind_in_flight: bool,
    /// True after decide_abort returns NeedReverse until abort outcome is committed.
    /// Blocks concurrent forward rebind admission during reverse.
    abort_reserved: bool,
    #[allow(dead_code)]
    from_owner: String,
    #[allow(dead_code)]
    to_owner: String,
    abort_outcome: Option<AbortOutcome>,
    /// In-flight registration refcount for tombstone retention.
    inflight_registrations: u32,
}

#[derive(Default)]
pub struct ConversationPopoutState {
    by_operation: Mutex<HashMap<String, OpRecord>>,
    /// conversation_id -> current open operation_id (if any live window).
    current_by_conversation: Mutex<HashMap<i32, String>>,
    /// Closed incarnations still fencing late registration.
    tombstones: Mutex<HashMap<(String, String), u32>>, // (label, op) -> inflight
    /// Ops whose close cleanup already ran (CloseRequested + Destroyed dedupe).
    close_cleanup_done: Mutex<HashMap<String, ()>>,
}

impl ConversationPopoutState {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert_opened(
        &self,
        conversation_id: i32,
        operation_id: String,
        to_owner: String,
    ) -> Result<(), AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        if by_op.contains_key(&operation_id) {
            return Err(AppCommandError::invalid_input(format!(
                "duplicate operation_id {operation_id}"
            )));
        }
        by_op.insert(
            operation_id.clone(),
            OpRecord {
                conversation_id,
                operation_id: operation_id.clone(),
                phase: PopoutPhase::Opening,
                ownership_generation: None,
                rebind_in_flight: false,
                abort_reserved: false,
                from_owner: "main".to_string(),
                to_owner: to_owner.clone(),
                abort_outcome: None,
                inflight_registrations: 0,
            },
        );
        drop(by_op);
        let mut current = self
            .current_by_conversation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout current lock poisoned"))?;
        current.insert(conversation_id, operation_id);
        Ok(())
    }

    pub fn get_status(&self, operation_id: &str) -> Result<PopoutOpStatus, AppCommandError> {
        let by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        Ok(PopoutOpStatus {
            phase: rec.phase,
            conversation_id: rec.conversation_id,
            operation_id: rec.operation_id.clone(),
            ownership_generation: rec.ownership_generation,
            abort_outcome: rec.abort_outcome.clone(),
        })
    }

    pub fn mark_ready_pending(&self, operation_id: &str) -> Result<(), AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if matches!(rec.phase, PopoutPhase::Aborted | PopoutPhase::HandoffComplete) {
            return Ok(());
        }
        rec.phase = PopoutPhase::ReadyPending;
        Ok(())
    }

    /// Pre-admit a forward rebind: reject if op is missing or already terminal.
    /// Marks `rebind_in_flight` so concurrent abort cannot claim NeverRebound
    /// while connection ownership is mid-flight.
    pub fn admit_forward_rebind(&self, operation_id: &str) -> Result<(), AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if matches!(rec.phase, PopoutPhase::Aborted | PopoutPhase::HandoffComplete) {
            return Err(AppCommandError::task_execution_failed(format!(
                "cannot rebind terminal operation {operation_id}"
            )));
        }
        if rec.abort_reserved {
            return Err(AppCommandError::task_execution_failed(format!(
                "cannot rebind while abort is reserved for {operation_id}"
            )));
        }
        rec.rebind_in_flight = true;
        Ok(())
    }

    /// Clear abort reservation (e.g. reverse failed with unknown error).
    pub fn clear_abort_reserved(&self, operation_id: &str) {
        if let Ok(mut by_op) = self.by_operation.lock() {
            if let Some(rec) = by_op.get_mut(operation_id) {
                rec.abort_reserved = false;
            }
        }
    }

    /// Clear in-flight flag without recording generation (rebind itself failed).
    pub fn clear_rebind_in_flight(&self, operation_id: &str) {
        if let Ok(mut by_op) = self.by_operation.lock() {
            if let Some(rec) = by_op.get_mut(operation_id) {
                rec.rebind_in_flight = false;
            }
        }
    }

    /// Record forward rebind generation atomically with phase check.
    /// Returns Err if the op became terminal between rebind and record (caller must reverse).
    pub fn record_rebind(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<(), AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if matches!(rec.phase, PopoutPhase::Aborted | PopoutPhase::HandoffComplete) {
            rec.rebind_in_flight = false;
            return Err(AppCommandError::task_execution_failed(format!(
                "cannot rebind terminal operation {operation_id}"
            )));
        }
        rec.ownership_generation = Some(generation);
        rec.rebind_in_flight = false;
        rec.phase = PopoutPhase::ReadyPending;
        Ok(())
    }

    /// Reserve close cleanup for a known operation_id (per-window handler path).
    /// Returns false if cleanup was already reserved for this op.
    pub fn reserve_close_operation(&self, operation_id: &str) -> bool {
        let Ok(mut done) = self.close_cleanup_done.lock() else {
            return false;
        };
        if done.contains_key(operation_id) {
            return false;
        }
        done.insert(operation_id.to_string(), ());
        true
    }

    /// Fallback when only the label is known (should be rare). Prefer
    /// per-window handlers that close over the immutable operation_id.
    pub fn capture_close_operation(&self, conversation_id: i32) -> Option<String> {
        let op = self.operation_for_conversation(conversation_id)?;
        if self.reserve_close_operation(&op) {
            Some(op)
        } else {
            None
        }
    }

    /// True if close cleanup was reserved/completed for this op.
    pub fn is_close_cleanup_reserved(&self, operation_id: &str) -> bool {
        self.close_cleanup_done
            .lock()
            .ok()
            .map(|m| m.contains_key(operation_id))
            .unwrap_or(false)
    }

    pub fn complete(&self, operation_id: &str) -> Result<PopoutOpStatus, AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        match rec.phase {
            PopoutPhase::HandoffComplete => {}
            PopoutPhase::Aborted => {
                return Ok(PopoutOpStatus {
                    phase: rec.phase,
                    conversation_id: rec.conversation_id,
                    operation_id: rec.operation_id.clone(),
                    ownership_generation: rec.ownership_generation,
                    abort_outcome: rec.abort_outcome.clone(),
                });
            }
            PopoutPhase::Opening | PopoutPhase::ReadyPending => {
                rec.phase = PopoutPhase::HandoffComplete;
            }
        }
        Ok(PopoutOpStatus {
            phase: rec.phase,
            conversation_id: rec.conversation_id,
            operation_id: rec.operation_id.clone(),
            ownership_generation: rec.ownership_generation,
            abort_outcome: rec.abort_outcome.clone(),
        })
    }

    /// Idempotent abort. Returns stored outcome if already aborted.
    pub fn abort(
        &self,
        operation_id: &str,
        compute: impl FnOnce(&OpRecord) -> AbortOutcome,
    ) -> Result<AbortOutcome, AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if let Some(existing) = &rec.abort_outcome {
            return Ok(existing.clone());
        }
        if rec.phase == PopoutPhase::HandoffComplete {
            let outcome = AbortOutcome::AlreadyComplete;
            rec.abort_outcome = Some(outcome.clone());
            rec.abort_reserved = false;
            // Keep phase HandoffComplete for close path
            return Ok(outcome);
        }
        if rec.rebind_in_flight {
            return Err(AppCommandError::task_execution_failed(
                "cannot abort while forward rebind is in flight",
            ));
        }
        let outcome = compute(rec);
        rec.phase = PopoutPhase::Aborted;
        rec.abort_outcome = Some(outcome.clone());
        rec.abort_reserved = false;
        rec.rebind_in_flight = false;
        Ok(outcome)
    }

    /// Atomic decision for abort/close: single lock for in-flight check +
    /// generation snapshot + terminal commits that need no reverse.
    ///
    /// - `AlreadyComplete` / existing abort outcome: committed under lock
    /// - `NeverRebound`: committed under lock only when gen is None and not in-flight
    /// - `NeedReverse(gen)`: does **not** mutate phase; caller must reverse then `abort`
    /// - Err if rebind_in_flight
    pub fn decide_abort(
        &self,
        operation_id: &str,
    ) -> Result<AbortDecision, AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if let Some(existing) = &rec.abort_outcome {
            return Ok(AbortDecision::Done {
                outcome: existing.clone(),
                conversation_id: rec.conversation_id,
            });
        }
        if rec.phase == PopoutPhase::HandoffComplete {
            let outcome = AbortOutcome::AlreadyComplete;
            rec.abort_outcome = Some(outcome.clone());
            return Ok(AbortDecision::Done {
                outcome,
                conversation_id: rec.conversation_id,
            });
        }
        if rec.rebind_in_flight {
            return Err(AppCommandError::task_execution_failed(
                "cannot abort while forward rebind is in flight",
            ));
        }
        match rec.ownership_generation {
            None => {
                let outcome = AbortOutcome::NeverRebound;
                rec.phase = PopoutPhase::Aborted;
                rec.abort_outcome = Some(outcome.clone());
                Ok(AbortDecision::Done {
                    outcome,
                    conversation_id: rec.conversation_id,
                })
            }
            Some(generation) => {
                // Reserve abort so concurrent forward rebind cannot re-admit
                // before reverse + commit finish.
                rec.abort_reserved = true;
                Ok(AbortDecision::NeedReverse {
                    conversation_id: rec.conversation_id,
                    generation,
                })
            }
        }
    }

    pub fn operation_for_conversation(&self, conversation_id: i32) -> Option<String> {
        self.current_by_conversation
            .lock()
            .ok()?
            .get(&conversation_id)
            .cloned()
    }

    pub fn is_registration_accepted(&self, conversation_id: i32, operation_id: &str) -> bool {
        let current = self
            .current_by_conversation
            .lock()
            .ok()
            .and_then(|m| m.get(&conversation_id).cloned());
        match current {
            Some(cur) => cur == operation_id,
            None => false,
        }
    }

    pub fn begin_registration(
        &self,
        label: &str,
        operation_id: &str,
    ) -> Result<(), AppCommandError> {
        // Reject if tombstoned
        if let Ok(tombs) = self.tombstones.lock() {
            if tombs.contains_key(&(label.to_string(), operation_id.to_string())) {
                return Err(AppCommandError::task_execution_failed(
                    "conversation window incarnation is closed",
                ));
            }
        }
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::task_execution_failed("unknown conversation popout operation for registration")
        })?;
        if matches!(rec.phase, PopoutPhase::Aborted) {
            return Err(AppCommandError::task_execution_failed(
                "conversation popout operation aborted",
            ));
        }
        rec.inflight_registrations = rec.inflight_registrations.saturating_add(1);
        Ok(())
    }

    pub fn end_registration(&self, operation_id: &str) {
        if let Ok(mut by_op) = self.by_operation.lock() {
            if let Some(rec) = by_op.get_mut(operation_id) {
                rec.inflight_registrations = rec.inflight_registrations.saturating_sub(1);
            }
        }
        // If tombstoned and inflight 0, tombstone can stay until drop
        if let Ok(mut tombs) = self.tombstones.lock() {
            let keys: Vec<_> = tombs
                .keys()
                .filter(|(_, op)| op == operation_id)
                .cloned()
                .collect();
            for k in keys {
                if let Some(n) = tombs.get_mut(&k) {
                    *n = n.saturating_sub(1);
                }
            }
        }
    }

    pub fn tombstone_on_close(&self, label: &str, operation_id: &str) {
        let inflight = self
            .by_operation
            .lock()
            .ok()
            .and_then(|m| m.get(operation_id).map(|r| r.inflight_registrations))
            .unwrap_or(0);
        if let Ok(mut tombs) = self.tombstones.lock() {
            tombs.insert((label.to_string(), operation_id.to_string()), inflight);
        }
        if let Ok(mut current) = self.current_by_conversation.lock() {
            current.retain(|_, op| op != operation_id);
        }
    }

    pub fn matches_expected_operation(
        &self,
        conversation_id: i32,
        expected_operation_id: &str,
    ) -> bool {
        self.operation_for_conversation(conversation_id)
            .as_deref()
            == Some(expected_operation_id)
    }
}

/// Result of [`ConversationPopoutState::decide_abort`].
#[derive(Debug, Clone)]
pub enum AbortDecision {
    Done {
        outcome: AbortOutcome,
        conversation_id: i32,
    },
    NeedReverse {
        conversation_id: i32,
        generation: u64,
    },
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn open_conversation_window(
    app: AppHandle,
    window: tauri::WebviewWindow,
    db: State<'_, AppDatabase>,
    popout: State<'_, ConversationPopoutState>,
    conversation_id: i32,
    folder_id: i32,
    agent_type: AgentType,
    locale: Option<crate::models::system::AppLocale>,
    operation_id: String,
) -> Result<OpenConversationResult, AppCommandError> {
    if conversation_id <= 0 {
        return Err(AppCommandError::invalid_input(
            "conversation_id must be positive",
        ));
    }
    if operation_id.is_empty() {
        return Err(AppCommandError::invalid_input(
            "operation_id is required",
        ));
    }
    let caller = window.label().to_string();
    if caller.starts_with("remote-workspace-") {
        return Err(AppCommandError::invalid_input(
            "conversation pop-out is local desktop only",
        ));
    }

    let label = conversation_window_label(conversation_id);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.unminimize();
        existing.set_focus().map_err(|e| {
            AppCommandError::window("Failed to focus conversation window", e.to_string())
        })?;
        return Ok(OpenConversationResult::FocusedExisting);
    }

    let _ = locale;
    let conv_title = crate::db::service::conversation_service::get_by_id(&db.conn, conversation_id)
        .await
        .ok()
        .and_then(|c| c.title)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| format!("Conversation {conversation_id}"));
    let window_title = format!("{conv_title} · {agent_type}");

    let agent_qs = serde_json::to_value(agent_type)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "claude_code".to_string());
    let url_str = format!(
        "conversation?conversationId={conversation_id}&folderId={folder_id}&agentType={agent_qs}&operationId={operation_id}"
    );
    let url = WebviewUrl::App(url_str.into());

    popout.insert_opened(conversation_id, operation_id.clone(), label.clone())?;

    let builder = WebviewWindowBuilder::new(&app, &label, url)
        .title(window_title)
        .inner_size(960.0, 720.0)
        .min_inner_size(480.0, 400.0)
        .center();
    // Intentionally no .parent — independent top-level (like settings).
    let conv_window = apply_platform_window_style(builder)
        .build()
        .map_err(|e| {
            // Roll back op record on build failure
            popout.tombstone_on_close(&label, &operation_id);
            AppCommandError::window("Failed to open conversation window", e.to_string())
        })?;
    post_window_setup(&conv_window);
    // Per-window close: close over THIS incarnation's operation_id so a delayed
    // Destroyed after label reuse cannot resolve to a newer open's op.
    register_conversation_window_close_handler(&conv_window, &operation_id);
    // Focus is best-effort. The window + close handler already exist; failing
    // focus must not return Err (which would leave main without compensating
    // while the detached page continues handoff).
    if let Err(e) = conv_window.set_focus() {
        tracing::warn!(
            "[popout] focus after open failed label={} op={}: {}",
            label,
            operation_id,
            e
        );
    }

    Ok(OpenConversationResult::Opened)
}

/// Attach CloseRequested/Destroyed cleanup with an immutable operation_id
/// captured at window creation (not looked up later by conversation id).
#[cfg(feature = "tauri-runtime")]
fn register_conversation_window_close_handler(
    window: &tauri::WebviewWindow,
    operation_id: &str,
) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let app = window.app_handle().clone();
    let label = window.label().to_string();
    let operation_id = operation_id.to_string();
    let scheduled = Arc::new(AtomicBool::new(false));
    window.on_window_event(move |event| {
        if !matches!(
            event,
            tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
        ) {
            return;
        }
        // Dedupe CloseRequested + Destroyed for this window instance.
        if scheduled.swap(true, Ordering::SeqCst) {
            return;
        }
        let app = app.clone();
        let label = label.clone();
        let operation_id = operation_id.clone();
        // Reserve close cleanup for this op (also fences global fallback).
        if let Some(popout) = app.try_state::<ConversationPopoutState>() {
            let _ = popout.reserve_close_operation(&operation_id);
        }
        tauri::async_runtime::spawn(async move {
            handle_conversation_window_closed(&app, &label, operation_id).await;
        });
    });
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn focus_conversation_window(
    app: AppHandle,
    conversation_id: i32,
) -> Result<bool, AppCommandError> {
    let label = conversation_window_label(conversation_id);
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.unminimize();
        existing.set_focus().map_err(|e| {
            AppCommandError::window("Failed to focus conversation window", e.to_string())
        })?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn close_conversation_window(
    app: AppHandle,
    popout: State<'_, ConversationPopoutState>,
    conversation_id: i32,
    expected_operation_id: String,
) -> Result<bool, AppCommandError> {
    if !popout.matches_expected_operation(conversation_id, &expected_operation_id) {
        return Ok(false);
    }
    let label = conversation_window_label(conversation_id);
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
        return Ok(true);
    }
    Ok(false)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn complete_conversation_popout_operation(
    popout: State<'_, ConversationPopoutState>,
    operation_id: String,
) -> Result<PopoutOpStatus, AppCommandError> {
    popout.complete(&operation_id)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_conversation_popout_operation(
    popout: State<'_, ConversationPopoutState>,
    operation_id: String,
) -> Result<PopoutOpStatus, AppCommandError> {
    popout.get_status(&operation_id)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn rebind_connection_owner_window(
    cm: State<'_, ConnectionManager>,
    popout: State<'_, ConversationPopoutState>,
    conversation_id: i32,
    connection_id: Option<String>,
    from_owner_window: String,
    to_owner_window: String,
    operation_id: String,
    expected_generation: Option<u64>,
) -> Result<RebindResult, AppCommandError> {
    // Reject if op already terminal
    let status = popout.get_status(&operation_id)?;
    if matches!(
        status.phase,
        PopoutPhase::Aborted | PopoutPhase::HandoffComplete
    ) && expected_generation.is_none()
    {
        // Forward rebind into terminal op not allowed; reverse may still use expected_generation
        if status.phase == PopoutPhase::Aborted {
            return Err(AppCommandError::task_execution_failed(
                "cannot rebind aborted popout operation",
            ));
        }
    }

    let is_forward = to_owner_window.starts_with("conversation-");
    if is_forward {
        // Reject terminal ops and mark rebind_in_flight before mutating ownership.
        popout.admit_forward_rebind(&operation_id)?;
    }

    let result = match cm
        .rebind_connection_owner_window(
            conversation_id,
            connection_id.as_deref(),
            &from_owner_window,
            &to_owner_window,
            &operation_id,
            expected_generation,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            if is_forward {
                popout.clear_rebind_in_flight(&operation_id);
            }
            return Err(e);
        }
    };

    // Stamp forward rebind onto the op record. If the op was aborted between
    // rebind and record, reverse ownership immediately so we never leave a
    // generation-less forward rebind hanging.
    if is_forward {
        if let Err(e) = popout.record_rebind(&operation_id, result.ownership_generation) {
            let reverse = cm
                .rebind_connection_owner_window(
                    conversation_id,
                    connection_id.as_deref(),
                    &to_owner_window,
                    &from_owner_window,
                    &operation_id,
                    Some(result.ownership_generation),
                )
                .await;
            popout.clear_rebind_in_flight(&operation_id);
            if let Err(rev_err) = reverse {
                tracing::error!(
                    "[popout] record_rebind failed and reverse also failed op={} gen={} record_err={} reverse_err={}",
                    operation_id,
                    result.ownership_generation,
                    e,
                    rev_err
                );
            }
            return Err(e);
        }
    }
    Ok(result)
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn abort_conversation_popout_operation(
    app: AppHandle,
    popout: State<'_, ConversationPopoutState>,
    cm: State<'_, ConnectionManager>,
    operation_id: String,
) -> Result<AbortOutcome, AppCommandError> {
    // Atomic: in-flight fence + gen snapshot + NeverRebound commit under one lock.
    let decision = popout.decide_abort(&operation_id)?;
    let (outcome, conversation_id) = match decision {
        AbortDecision::Done {
            outcome,
            conversation_id,
        } => (outcome, conversation_id),
        AbortDecision::NeedReverse {
            conversation_id,
            generation,
        } => {
            let to_label = conversation_window_label(conversation_id);
            match cm
                .rebind_connection_owner_window(
                    conversation_id,
                    None,
                    &to_label,
                    "main",
                    &operation_id,
                    Some(generation),
                )
                .await
            {
                Ok(_) => (
                    popout.abort(&operation_id, |_| AbortOutcome::Reversed {
                        generation,
                    })?,
                    conversation_id,
                ),
                Err(e) => {
                    let msg = e.to_string();
                    tracing::warn!(
                        "[popout] reverse rebind on abort failed op={} gen={}: {}",
                        operation_id,
                        generation,
                        e
                    );
                    if msg.contains("generation CAS") || msg.contains("owner label CAS") {
                        (
                            popout.abort(&operation_id, |_| AbortOutcome::Superseded {
                                current_generation: generation,
                                current_owner: "unknown".into(),
                            })?,
                            conversation_id,
                        )
                    } else if msg.contains("not found") {
                        (
                            popout.abort(&operation_id, |_| AbortOutcome::AlreadyMain)?,
                            conversation_id,
                        )
                    } else {
                        popout.clear_abort_reserved(&operation_id);
                        return Err(AppCommandError::task_execution_failed(format!(
                            "reverse rebind failed for op {operation_id}: {e}"
                        )));
                    }
                }
            }
        }
    };

    let _ = app.emit(
        "conversation-window://abort",
        serde_json::json!({
            "conversationId": conversation_id,
            "operationId": operation_id,
            "abortOutcome": outcome,
        }),
    );

    Ok(outcome)
}

/// Handle window close/destroy for conversation-* labels.
///
/// `operation_id` **must** be captured synchronously on the window-event
/// thread via [`ConversationPopoutState::capture_close_operation`] before this
/// async task is spawned. Looking up the current op here would allow a delayed
/// Destroyed(A) to clean up a reopened incarnation B (label-reuse ABA).
#[cfg(feature = "tauri-runtime")]
pub async fn handle_conversation_window_closed(
    app: &AppHandle,
    label: &str,
    operation_id: String,
) {
    let Some(conversation_id) = parse_conversation_id_from_label(label) else {
        return;
    };
    if operation_id.is_empty() {
        return;
    }
    let Some(popout) = app.try_state::<ConversationPopoutState>() else {
        return;
    };

    // Wait out a brief rebind_in_flight window so we never treat mid-rebind
    // as NeverRebound and kill a just-transferred connection.
    let mut decision = None;
    for _ in 0..20 {
        match popout.decide_abort(&operation_id) {
            Ok(d) => {
                decision = Some(d);
                break;
            }
            Err(e) if e.to_string().contains("rebind is in flight") => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(e) => {
                tracing::warn!(
                    "[popout] close decide_abort failed label={} op={}: {}",
                    label,
                    operation_id,
                    e
                );
                // Non-destructive: tombstone only; do not disconnect.
                popout.tombstone_on_close(label, &operation_id);
                let _ = app.emit(
                    "conversation-window://closed",
                    serde_json::json!({
                        "conversationId": conversation_id,
                        "operationId": operation_id,
                        "abortOutcome": null,
                    }),
                );
                return;
            }
        }
    }
    let decision = match decision {
        Some(d) => d,
        None => {
            tracing::warn!(
                "[popout] close deferred: rebind still in flight label={} op={}",
                label,
                operation_id
            );
            popout.tombstone_on_close(label, &operation_id);
            let _ = app.emit(
                "conversation-window://closed",
                serde_json::json!({
                    "conversationId": conversation_id,
                    "operationId": operation_id,
                    "abortOutcome": null,
                }),
            );
            return;
        }
    };

    let outcome = match decision {
        AbortDecision::Done { outcome, .. } => outcome,
        AbortDecision::NeedReverse {
            conversation_id: cid,
            generation,
        } => {
            if let Some(cm) = app.try_state::<ConnectionManager>() {
                match cm
                    .rebind_connection_owner_window(
                        cid,
                        None,
                        label,
                        "main",
                        &operation_id,
                        Some(generation),
                    )
                    .await
                {
                    Ok(_) => popout
                        .abort(&operation_id, |_| AbortOutcome::Reversed { generation })
                        .unwrap_or(AbortOutcome::Reversed { generation }),
                    Err(e) => {
                        let msg = e.to_string();
                        tracing::warn!(
                            "[popout] reverse rebind on close failed label={} op={} gen={}: {}",
                            label,
                            operation_id,
                            generation,
                            e
                        );
                        if msg.contains("generation CAS") || msg.contains("owner label CAS") {
                            popout
                                .abort(&operation_id, |_| AbortOutcome::Superseded {
                                    current_generation: generation,
                                    current_owner: "unknown".into(),
                                })
                                .unwrap_or(AbortOutcome::Superseded {
                                    current_generation: generation,
                                    current_owner: "unknown".into(),
                                })
                        } else if msg.contains("not found") {
                            popout
                                .abort(&operation_id, |_| AbortOutcome::AlreadyMain)
                                .unwrap_or(AbortOutcome::AlreadyMain)
                        } else {
                            // Unknown reverse: reap residual for this op only after
                            // best-effort; do not invent NeverRebound.
                            popout
                                .abort(&operation_id, |_| AbortOutcome::Reversed { generation })
                                .unwrap_or(AbortOutcome::Reversed { generation })
                        }
                    }
                }
            } else {
                popout
                    .abort(&operation_id, |_| AbortOutcome::Reversed { generation })
                    .unwrap_or(AbortOutcome::Reversed { generation })
            }
        }
    };

    // Disconnect / kill only resources still matching this incarnation
    // (label + op). After a successful reverse, owner is main so they are skipped.
    // Superseded residual with this op on this label is reaped intentionally.
    let should_disconnect = matches!(
        outcome,
        AbortOutcome::AlreadyComplete
            | AbortOutcome::NeverRebound
            | AbortOutcome::Reversed { .. }
            | AbortOutcome::Superseded { .. }
            | AbortOutcome::AlreadyMain
    );

    if should_disconnect {
        if let Some(cm) = app.try_state::<ConnectionManager>() {
            let n = cm
                .disconnect_by_owner_window_and_operation(label, &operation_id)
                .await;
            tracing::info!(
                "[ACP] conversation window close disconnected label={} op={} count={}",
                label,
                operation_id,
                n
            );
        }
        if let Some(tm) = app.try_state::<TerminalManager>() {
            let n = tm.kill_by_owner_window_and_operation(label, Some(&operation_id));
            tracing::info!(
                "[TERM] conversation window close killed label={} op={} count={}",
                label,
                operation_id,
                n
            );
        }
    }

    popout.tombstone_on_close(label, &operation_id);

    let _ = app.emit(
        "conversation-window://closed",
        serde_json::json!({
            "conversationId": conversation_id,
            "operationId": operation_id,
            "abortOutcome": outcome,
        }),
    );
}

// ---- stubs used when rebind APIs not yet fully wired in non-test builds ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_roundtrip() {
        assert_eq!(conversation_window_label(42), "conversation-42");
        assert_eq!(parse_conversation_id_from_label("conversation-42"), Some(42));
        assert_eq!(parse_conversation_id_from_label("main"), None);
    }

    #[test]
    fn abort_is_idempotent() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        let a = state
            .abort("op1", |_| AbortOutcome::NeverRebound)
            .unwrap();
        let b = state
            .abort("op1", |_| AbortOutcome::Reversed { generation: 9 })
            .unwrap();
        assert_eq!(a, AbortOutcome::NeverRebound);
        assert_eq!(b, AbortOutcome::NeverRebound);
    }

    #[test]
    fn complete_is_idempotent() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        let s1 = state.complete("op1").unwrap();
        assert_eq!(s1.phase, PopoutPhase::HandoffComplete);
        let s2 = state.complete("op1").unwrap();
        assert_eq!(s2.phase, PopoutPhase::HandoffComplete);
    }

    #[test]
    fn registration_requires_current_op() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        assert!(state.is_registration_accepted(1, "op1"));
        assert!(!state.is_registration_accepted(1, "op2"));
    }

    #[test]
    fn capture_close_operation_is_idempotent_and_survives_reopen() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "opA".into(), "conversation-1".into())
            .unwrap();
        let first = state.capture_close_operation(1);
        assert_eq!(first.as_deref(), Some("opA"));
        // Second capture (Destroyed after CloseRequested) is a no-op
        assert_eq!(state.capture_close_operation(1), None);

        // Simulated reopen: insert B (overwrites current). Delayed close for A
        // already reserved so it cannot re-capture; B gets its own capture.
        state
            .insert_opened(1, "opB".into(), "conversation-1".into())
            .unwrap();
        state.tombstone_on_close("conversation-1", "opA");
        // current still B (tombstone only drops matching op value)
        assert_eq!(
            state.operation_for_conversation(1).as_deref(),
            Some("opB")
        );
        let second = state.capture_close_operation(1);
        assert_eq!(second.as_deref(), Some("opB"));
        assert_ne!(second.as_deref(), Some("opA"));
    }

    #[test]
    fn reserve_close_is_per_operation_not_conversation() {
        let state = ConversationPopoutState::new();
        assert!(state.reserve_close_operation("opA"));
        assert!(!state.reserve_close_operation("opA"));
        // B is independent — delayed A close must not block B reservation.
        assert!(state.reserve_close_operation("opB"));
    }

    #[test]
    fn decide_abort_never_rebound_is_atomic_with_in_flight() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op1").unwrap();
        assert!(state.decide_abort("op1").is_err());
        // After record, NeedReverse rather than NeverRebound
        state.record_rebind("op1", 3).unwrap();
        match state.decide_abort("op1").unwrap() {
            AbortDecision::NeedReverse { generation, .. } => assert_eq!(generation, 3),
            other => panic!("expected NeedReverse, got {other:?}"),
        }
        // Forward rebind blocked while abort reserved
        assert!(state.admit_forward_rebind("op1").is_err());
    }

    #[test]
    fn decide_abort_commits_never_rebound_only_when_no_gen() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        match state.decide_abort("op1").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::NeverRebound,
                ..
            } => {}
            other => panic!("expected NeverRebound, got {other:?}"),
        }
        // Idempotent
        match state.decide_abort("op1").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::NeverRebound,
                ..
            } => {}
            other => panic!("expected NeverRebound again, got {other:?}"),
        }
    }

    #[test]
    fn admit_forward_rebind_rejects_terminal() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op1").unwrap();
        // in-flight until record
        {
            let by_op = state.by_operation.lock().unwrap();
            assert!(by_op.get("op1").unwrap().rebind_in_flight);
        }
        state.record_rebind("op1", 1).unwrap();
        {
            let by_op = state.by_operation.lock().unwrap();
            assert!(!by_op.get("op1").unwrap().rebind_in_flight);
            assert_eq!(by_op.get("op1").unwrap().ownership_generation, Some(1));
        }
        state.complete("op1").unwrap();
        assert!(state.admit_forward_rebind("op1").is_err());
    }

    #[test]
    fn record_rebind_rejects_after_abort() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        state
            .abort("op1", |_| AbortOutcome::NeverRebound)
            .unwrap();
        assert!(state.record_rebind("op1", 1).is_err());
    }
}
