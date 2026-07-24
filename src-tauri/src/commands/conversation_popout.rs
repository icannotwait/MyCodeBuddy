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
    /// Reverse rebind found no live connection for this conversation.
    /// Not reclaimable — do not invent a main owner entry for a dead agent.
    ConnectionGone,
    Reversed { generation: u64 },
    Superseded {
        current_generation: u64,
        current_owner: String,
    },
    AlreadyComplete,
    /// Close-path honest failure: reverse did not clearly succeed.
    /// Non-reclaimable — never fabricate `Reversed` for FE main-owner lease.
    ReverseUncertain,
}

/// True when reverse rebind failed because the connection no longer exists.
fn reverse_error_is_connection_gone(msg: &str) -> bool {
    let m = msg.to_lowercase();
    (m.contains("connection") && m.contains("not found"))
        || m.contains("no connection for conversation")
}

/// True when reverse failed due to ownership CAS (generation / label / operation).
fn reverse_err_is_cas_superseded(msg: &str) -> bool {
    msg.contains("generation CAS")
        || msg.contains("owner label CAS")
        || msg.contains("owner operation CAS")
        || msg.contains("operation CAS")
}

/// Classify a reverse manager error for the close / close-reserved path.
fn classify_close_reverse_error(msg: &str, generation_hint: u64) -> AbortOutcome {
    if reverse_error_is_connection_gone(msg) {
        AbortOutcome::ConnectionGone
    } else if reverse_err_is_cas_superseded(msg) {
        AbortOutcome::Superseded {
            current_generation: generation_hint,
            current_owner: "unknown".into(),
        }
    } else {
        AbortOutcome::ReverseUncertain
    }
}

/// Abort outcome after forced reverse when `record_rebind` lost to close.
/// Connection disappearance / CAS / unknown must not fabricate reclaimable
/// `Reversed` from the forward generation alone.
fn abort_outcome_for_close_reserved_forced_reverse(
    reverse_generation: Option<u64>,
    reverse_err: Option<&str>,
    forward_generation: u64,
) -> AbortOutcome {
    if let Some(generation) = reverse_generation {
        return AbortOutcome::Reversed { generation };
    }
    if let Some(msg) = reverse_err {
        return classify_close_reverse_error(msg, forward_generation);
    }
    AbortOutcome::ReverseUncertain
}

/// Late `record_rebind` close-reserved: prefer residual stamped rebind gen over
/// a forced-primary `ConnectionGone` / Uncertain / Superseded so we never
/// commit a non-reclaimable outcome while residual already moved ownership.
fn close_reserved_outcome_after_residual(
    forced_outcome: AbortOutcome,
    residual_max_gen: Option<u64>,
) -> AbortOutcome {
    if let Some(generation) = residual_max_gen {
        AbortOutcome::Reversed { generation }
    } else {
        forced_outcome
    }
}

/// True for close-path terminal ownership outcomes (no second reverse).
fn is_close_terminal_ownership_outcome(outcome: &AbortOutcome) -> bool {
    matches!(
        outcome,
        AbortOutcome::Reversed { .. }
            | AbortOutcome::ConnectionGone
            | AbortOutcome::Superseded { .. }
            | AbortOutcome::ReverseUncertain
    )
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OpenConversationResult {
    Opened,
    FocusedExisting,
}

/// Side effects for open/focus-existing decisions.
///
/// Production uses a Tauri-backed adapter for get/unminimize/focus.
/// Unit tests use a recording fake that also implements insert/create so the
/// FocusedExisting path can prove those ops are skipped when a label exists.
pub(crate) trait ConversationWindowOps {
    fn get_by_label(&self, label: &str) -> bool;
    fn unminimize(&self, label: &str);
    fn set_focus(&self, label: &str) -> Result<(), String>;
    /// Used by [`decide_open_or_focus_existing`] (behavioral tests).
    #[cfg_attr(not(test), allow(dead_code))]
    fn insert_op(
        &self,
        conversation_id: i32,
        operation_id: &str,
        label: &str,
    ) -> Result<(), String>;
    /// Used by [`decide_open_or_focus_existing`] (behavioral tests).
    #[cfg_attr(not(test), allow(dead_code))]
    fn create_window(&self, label: &str) -> Result<(), String>;
}

/// If a window with `label` exists: unminimize + focus and return
/// `FocusedExisting` **without** insert_op / create_window.
/// Otherwise: insert_op + create_window and return `Opened`.
///
/// Production open uses [`try_focus_existing_conversation_window`] for the
/// early return, then its own create path; this helper models the full
/// branch so tests can assert create/insert are skipped on focus.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decide_open_or_focus_existing(
    ops: &impl ConversationWindowOps,
    conversation_id: i32,
    operation_id: &str,
    label: &str,
) -> Result<OpenConversationResult, String> {
    if let Some(focused) = try_focus_existing_conversation_window(ops, label)? {
        return Ok(focused);
    }
    ops.insert_op(conversation_id, operation_id, label)?;
    ops.create_window(label)?;
    Ok(OpenConversationResult::Opened)
}

/// Focus-existing early return used by the Tauri open command.
/// Returns `Some(FocusedExisting)` after unminimize+focus when the label
/// already has a window; `None` means the caller should proceed to create
/// (and must not have called insert_op / create_window yet).
pub(crate) fn try_focus_existing_conversation_window(
    ops: &impl ConversationWindowOps,
    label: &str,
) -> Result<Option<OpenConversationResult>, String> {
    if !ops.get_by_label(label) {
        return Ok(None);
    }
    ops.unminimize(label);
    ops.set_focus(label)?;
    Ok(Some(OpenConversationResult::FocusedExisting))
}

/// Tauri-backed window ops for the open/focus path (desktop only).
/// Only get/unminimize/focus are used on the production early-return path;
/// insert/create stay in `open_conversation_window` after `None`.
#[cfg(feature = "tauri-runtime")]
struct TauriConversationWindowOps<'a> {
    app: &'a AppHandle,
}

#[cfg(feature = "tauri-runtime")]
impl ConversationWindowOps for TauriConversationWindowOps<'_> {
    fn get_by_label(&self, label: &str) -> bool {
        self.app.get_webview_window(label).is_some()
    }

    fn unminimize(&self, label: &str) {
        if let Some(existing) = self.app.get_webview_window(label) {
            let _ = existing.unminimize();
        }
    }

    fn set_focus(&self, label: &str) -> Result<(), String> {
        let existing = self
            .app
            .get_webview_window(label)
            .ok_or_else(|| format!("window {label} disappeared before focus"))?;
        existing.set_focus().map_err(|e| e.to_string())
    }

    fn insert_op(
        &self,
        _conversation_id: i32,
        _operation_id: &str,
        _label: &str,
    ) -> Result<(), String> {
        Err("Tauri adapter: insert_op owned by open_conversation_window".into())
    }

    fn create_window(&self, _label: &str) -> Result<(), String> {
        Err("Tauri adapter: create_window owned by open_conversation_window".into())
    }
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
        // Close fence first (separate lock): never admit into a closing op.
        if self.is_close_cleanup_reserved(operation_id) {
            return Err(AppCommandError::task_execution_failed(format!(
                "cannot rebind closed popout operation {operation_id}"
            )));
        }
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

    /// True while admit_forward_rebind has not yet recorded or cleared.
    pub fn is_rebind_in_flight(&self, operation_id: &str) -> bool {
        self.by_operation
            .lock()
            .ok()
            .and_then(|m| m.get(operation_id).map(|r| r.rebind_in_flight))
            .unwrap_or(false)
    }

    /// Reserve abort/close while a forward rebind may still be in flight.
    /// Unlike `decide_abort`, this does **not** require rebind_in_flight == false.
    /// A finishing `record_rebind` observes this reservation and rejects so the
    /// rebind path reverses (or reaps) instead of stranding ownership on a
    /// closed child window.
    pub fn reserve_abort_for_close(&self, operation_id: &str) -> Result<(), AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;
        if matches!(rec.phase, PopoutPhase::Aborted | PopoutPhase::HandoffComplete) {
            // Already terminal — reservation is a no-op for finishers.
            return Ok(());
        }
        if rec.abort_outcome.is_some() {
            return Ok(());
        }
        rec.abort_reserved = true;
        Ok(())
    }

    /// Record forward rebind generation atomically with phase check.
    /// Returns Err if the op became terminal, close-reserved, or abort-reserved
    /// between rebind and record (caller must reverse and/or reap).
    ///
    /// When the close/abort fence wins after a successful forward rebind, the
    /// forward generation is **kept** and `rebind_in_flight` stays true until
    /// the forced reverse commits via [`Self::abort_after_forced_reverse`].
    /// Clearing both would let `decide_abort` commit `NeverRebound` while the
    /// backend already owns the child (or later main after reverse) with a
    /// newer generation — orphaning the agent against a stale main FE lease.
    pub fn record_rebind(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<(), AppCommandError> {
        // Close fence before by_op so a finishing forward rebind never becomes
        // visible as ReadyPending after close cleanup was reserved.
        let close_reserved = self.is_close_cleanup_reserved(operation_id);
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
        if close_reserved || rec.abort_reserved {
            // Ownership already moved on the connection. Hold forward gen +
            // in-flight fence so concurrent close waits / NeedReverse instead
            // of NeverRebound while forced reverse runs.
            rec.ownership_generation = Some(generation);
            // Keep rebind_in_flight = true (admit set it; do not clear).
            let reason = if close_reserved {
                format!("cannot rebind closed popout operation {operation_id}")
            } else {
                format!("cannot rebind while abort is reserved for {operation_id}")
            };
            return Err(AppCommandError::task_execution_failed(reason));
        }
        rec.ownership_generation = Some(generation);
        rec.rebind_in_flight = false;
        rec.phase = PopoutPhase::ReadyPending;
        Ok(())
    }

    /// Record post-reverse ownership generation so a later abort CAS uses the
    /// live lease (not the pre-reverse forward generation). Safe for
    /// Opening/ReadyPending; no-ops when already terminal.
    pub fn record_reverse_generation(
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
        if matches!(
            rec.phase,
            PopoutPhase::Aborted | PopoutPhase::HandoffComplete
        ) {
            return Ok(());
        }
        rec.ownership_generation = Some(generation);
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

    /// Mark handoff complete. Mutually exclusive with close/abort:
    /// - already `HandoffComplete` → idempotent success
    /// - already `Aborted` → return aborted status (caller must not treat as success)
    /// - close fence reserved or `abort_reserved` → reject (do not race reverse/cleanup)
    pub fn complete(&self, operation_id: &str) -> Result<PopoutOpStatus, AppCommandError> {
        // Close fence first (separate lock): never complete into a closing op.
        // Exception handled below for already-terminal phases under by_op.
        let close_reserved = self.is_close_cleanup_reserved(operation_id);

        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;

        match rec.phase {
            PopoutPhase::HandoffComplete => {
                // Idempotent success even if close started after handoff.
            }
            PopoutPhase::Aborted => {
                // Not success: main must compensate / reclaim if reverse ran.
                return Ok(PopoutOpStatus {
                    phase: rec.phase,
                    conversation_id: rec.conversation_id,
                    operation_id: rec.operation_id.clone(),
                    ownership_generation: rec.ownership_generation,
                    abort_outcome: rec.abort_outcome.clone(),
                });
            }
            PopoutPhase::Opening | PopoutPhase::ReadyPending => {
                if close_reserved {
                    return Err(AppCommandError::task_execution_failed(format!(
                        "cannot complete popout operation {operation_id} while close is reserved"
                    )));
                }
                if rec.abort_reserved {
                    return Err(AppCommandError::task_execution_failed(format!(
                        "cannot complete popout operation {operation_id} while abort is reserved"
                    )));
                }
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
        self.abort_inner(operation_id, compute, /*allow_rebind_in_flight=*/ false)
    }

    /// Commit abort after a forced reverse when `record_rebind` lost to the
    /// close/abort fence. Unlike [`Self::abort`], this is allowed while
    /// `rebind_in_flight` is still true (the reverse completed ownership
    /// movement; we must publish `Reversed` / `ConnectionGone` before close
    /// can decide `NeverRebound`).
    ///
    /// For `Reversed { generation }`, also stamps `ownership_generation` so
    /// status and later CAS paths see the post-reverse lease.
    pub fn abort_after_forced_reverse(
        &self,
        operation_id: &str,
        outcome: AbortOutcome,
    ) -> Result<AbortOutcome, AppCommandError> {
        self.abort_inner(operation_id, |_| outcome, /*allow_rebind_in_flight=*/ true)
    }

    fn abort_inner(
        &self,
        operation_id: &str,
        compute: impl FnOnce(&OpRecord) -> AbortOutcome,
        allow_rebind_in_flight: bool,
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
        if rec.rebind_in_flight && !allow_rebind_in_flight {
            return Err(AppCommandError::task_execution_failed(
                "cannot abort while forward rebind is in flight",
            ));
        }
        let outcome = compute(rec);
        if let AbortOutcome::Reversed { generation } = &outcome {
            rec.ownership_generation = Some(*generation);
        }
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

    /// Window-close decision (reverse-first). Distinct from [`Self::decide_abort`]:
    /// - `HandoffComplete` still needs reverse (API abort stays `AlreadyComplete`)
    /// - API skip outcomes (`AlreadyComplete` / `NeverRebound`) do not skip reverse
    /// - ownership terminal outcomes return `Done` (no second reverse)
    pub fn decide_close(&self, operation_id: &str) -> Result<CloseDecision, AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;

        // 1) Close-path (or ownership-recovery) terminal outcomes → Done.
        if let Some(existing) = &rec.abort_outcome {
            if is_close_terminal_ownership_outcome(existing) {
                return Ok(CloseDecision::Done {
                    outcome: existing.clone(),
                    conversation_id: rec.conversation_id,
                });
            }
            // 3) API-only AlreadyComplete / NeverRebound / AlreadyMain: ignore
            // for reverse skip — fall through to generation rows.
        }

        // 2) rebind_in_flight → Err (caller polls / timeout falls through).
        if rec.rebind_in_flight {
            return Err(AppCommandError::task_execution_failed(
                "cannot abort while forward rebind is in flight",
            ));
        }

        // 5–6) Generation rows (including HandoffComplete).
        rec.abort_reserved = true;
        match rec.ownership_generation {
            Some(generation) => Ok(CloseDecision::NeedReverse {
                conversation_id: rec.conversation_id,
                generation,
            }),
            None => Ok(CloseDecision::NeedReverseBestEffort {
                conversation_id: rec.conversation_id,
            }),
        }
    }

    /// Commit close reverse outcome. Bypasses `abort_inner`'s HandoffComplete →
    /// AlreadyComplete short-circuit. API skip outcomes may be overwritten;
    /// ownership terminal outcomes are generally first-writer wins, except
    /// `ReverseUncertain` / `Superseded` / `ConnectionGone` may be upgraded to
    /// `Reversed { gen }` when a late reverse succeeds after timeout, CAS race,
    /// or residual stamped rebind moves cold-stamped leftovers to main.
    /// `Reversed` always stamps `ownership_generation`.
    pub fn commit_close_reverse(
        &self,
        operation_id: &str,
        outcome: AbortOutcome,
    ) -> Result<AbortOutcome, AppCommandError> {
        let mut by_op = self
            .by_operation
            .lock()
            .map_err(|_| AppCommandError::task_execution_failed("popout op lock poisoned"))?;
        let rec = by_op.get_mut(operation_id).ok_or_else(|| {
            AppCommandError::not_found(format!("popout operation {operation_id} not found"))
        })?;

        if let Some(existing) = rec.abort_outcome.clone() {
            // Late reverse after timeout / CAS race / residual stamped rebind:
            // upgrade non-reclaimable placeholders to Reversed{gen} when a real
            // reverse later succeeds (including primary ConnectionGone when
            // residual rebound_count > 0 moved ownership to main).
            let upgrade_to_reversed = matches!(
                existing,
                AbortOutcome::ReverseUncertain
                    | AbortOutcome::Superseded { .. }
                    | AbortOutcome::ConnectionGone
            ) && matches!(&outcome, AbortOutcome::Reversed { .. });

            if is_close_terminal_ownership_outcome(&existing) && !upgrade_to_reversed {
                // First-writer: keep existing ownership terminal outcome.
                // If already Reversed, ensure gen is stamped (idempotent).
                if let AbortOutcome::Reversed { generation } = &existing {
                    rec.ownership_generation = Some(*generation);
                }
                rec.abort_reserved = false;
                rec.rebind_in_flight = false;
                return Ok(existing);
            }
            // AlreadyComplete / NeverRebound / AlreadyMain are non-terminal for
            // close and may be replaced by ownership recovery outcomes.
            // ReverseUncertain/Superseded/ConnectionGone + Reversed upgrades.
        }

        // Reversed always stamps ownership_generation (including upgrade path).
        if let AbortOutcome::Reversed { generation } = &outcome {
            rec.ownership_generation = Some(*generation);
        }
        rec.phase = PopoutPhase::Aborted;
        rec.abort_outcome = Some(outcome.clone());
        rec.abort_reserved = false;
        rec.rebind_in_flight = false;
        Ok(outcome)
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
        // Close fence: reject once close is reserved (before/without tombstone)
        // so cold registration cannot start after cleanup begins.
        if self.is_close_cleanup_reserved(operation_id) {
            return Err(AppCommandError::task_execution_failed(
                "conversation window incarnation is closed",
            ));
        }
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

    /// In-flight cold-registration count for this operation (0 if unknown).
    pub fn inflight_registrations(&self, operation_id: &str) -> u32 {
        self.by_operation
            .lock()
            .ok()
            .and_then(|m| m.get(operation_id).map(|r| r.inflight_registrations))
            .unwrap_or(0)
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

    /// True after [`Self::tombstone_on_close`] for this (label, operation).
    pub fn is_tombstoned(&self, label: &str, operation_id: &str) -> bool {
        self.tombstones
            .lock()
            .ok()
            .map(|t| t.contains_key(&(label.to_string(), operation_id.to_string())))
            .unwrap_or(false)
    }

    /// True when the operation is unknown or already aborted.
    pub fn is_operation_aborted(&self, operation_id: &str) -> bool {
        match self.by_operation.lock() {
            Ok(by_op) => match by_op.get(operation_id) {
                Some(rec) => matches!(rec.phase, PopoutPhase::Aborted),
                // Unknown op after close cleanup — treat as not open for
                // late-connect teardown decisions (tombstone is the primary
                // close-during-connect signal).
                None => false,
            },
            Err(_) => true,
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

/// Result of [`ConversationPopoutState::decide_close`] (window close only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseDecision {
    Done {
        outcome: AbortOutcome,
        conversation_id: i32,
    },
    NeedReverse {
        conversation_id: i32,
        generation: u64,
    },
    NeedReverseBestEffort {
        conversation_id: i32,
    },
}

#[cfg(feature = "tauri-runtime")]
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
#[allow(clippy::too_many_arguments)]
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
    // FocusExisting path: unminimize+focus; no insert_opened / no second window.
    // Behavioral unit coverage: `decide_open_or_focus_existing` with a fake ops.
    let focus_ops = TauriConversationWindowOps { app: &app };
    if let Some(focused) = try_focus_existing_conversation_window(&focus_ops, &label).map_err(
        |e| AppCommandError::window("Failed to focus conversation window", e),
    )? {
        return Ok(focused);
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
#[allow(clippy::too_many_arguments)]
pub async fn rebind_connection_owner_window(
    cm: State<'_, ConnectionManager>,
    popout: State<'_, ConversationPopoutState>,
    tm: State<'_, TerminalManager>,
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

    // Stamp forward rebind onto the op record. If the op was aborted / close-
    // reserved between rebind and record, reverse ownership immediately so we
    // never leave ownership on a closed child window. When close cleanup is
    // already reserved, also reap residual resources for this (label, op) so a
    // reverse failure cannot strand the agent.
    if is_forward {
        if let Err(e) = popout.record_rebind(&operation_id, result.ownership_generation) {
            // record_rebind held forward gen + rebind_in_flight on close/abort
            // fence loss — do not clear the fence until reverse outcome is
            // committed (or non-close reverse path finishes).
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
            let close_reserved = popout.is_close_cleanup_reserved(&operation_id);
            if let Ok(ref rev) = reverse {
                // Keep op gen aligned with live ownership after forced reverse
                // (still Opening/ReadyPending until abort commits).
                let _ = popout.record_reverse_generation(
                    &operation_id,
                    rev.ownership_generation,
                );
            }
            if let Err(ref rev_err) = reverse {
                tracing::error!(
                    "[popout] record_rebind failed and reverse also failed op={} gen={} record_err={} reverse_err={}",
                    operation_id,
                    result.ownership_generation,
                    e,
                    rev_err
                );
            }
            if close_reserved {
                // Residual BEFORE commit_close_reverse: keep rebind_in_flight set
                // while stamped reverse runs. Committing ConnectionGone first
                // clears the fence, letting close observe Done(ConnectionGone)
                // and emit non-reclaimable closed while residual later upgrades
                // to Reversed (FE treats ConnectionGone as terminal).
                let reverse_err_msg = reverse.as_ref().err().map(|e| e.to_string());
                let forced_outcome = abort_outcome_for_close_reserved_forced_reverse(
                    reverse
                        .as_ref()
                        .ok()
                        .map(|rev| rev.ownership_generation),
                    reverse_err_msg.as_deref(),
                    result.ownership_generation,
                );
                // Busy-safe residual: stamped reverse + idle disconnect + terminal rebind.
                let residual_gen = residual_reconcile_after_close(
                    cm.inner(),
                    Some(tm.inner()),
                    &to_owner_window,
                    &operation_id,
                )
                .await;
                // Prefer residual Reversed{max_gen} when rebound_count > 0.
                let outcome = close_reserved_outcome_after_residual(
                    forced_outcome,
                    residual_gen,
                );
                let _ = popout.commit_close_reverse(&operation_id, outcome);
            } else {
                // Non-close reject (e.g. terminal race): drop the in-flight fence
                // so a later abort can proceed with the stamped gen.
                popout.clear_rebind_in_flight(&operation_id);
            }
            return Err(e);
        }
    } else {
        // Reverse (including detached pre-ready claim failure): stamp the
        // post-reverse generation so abort does not CAS with a stale forward gen.
        let _ = popout.record_reverse_generation(&operation_id, result.ownership_generation);
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
                // Stamp the post-reverse generation (not the pre-reverse CAS
                // expected value) so main reclaim can adopt the live lease.
                Ok(rev) => (
                    popout.abort(&operation_id, |_| AbortOutcome::Reversed {
                        generation: rev.ownership_generation,
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
                    if reverse_error_is_connection_gone(&msg) {
                        (
                            popout.abort(&operation_id, |_| AbortOutcome::ConnectionGone)?,
                            conversation_id,
                        )
                    } else if msg.contains("generation CAS") || msg.contains("owner label CAS") {
                        (
                            popout.abort(&operation_id, |_| AbortOutcome::Superseded {
                                current_generation: generation,
                                current_owner: "unknown".into(),
                            })?,
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

/// Shared close residual: best-effort reverse every still-stamped `(label, op)`
/// connection to `main`, then idle-only disconnect, then terminal rebind
/// (never kill on close residual).
///
/// Returns the max post-rebind ownership generation when any connection was
/// reverse-rebound (`rebound_count > 0`), so callers can upgrade a premature
/// `Superseded` / `ReverseUncertain` / `ConnectionGone` commit to reclaimable
/// `Reversed { gen }` (cold-stamped leftovers primary reverse misses).
///
/// Close-reachable sites (audit):
/// 1. `handle_conversation_window_closed` primary residual
/// 2. `handle_conversation_window_closed` final-reap after inflight wait
/// 3. Late `record_rebind` close-reserved path
/// 4. Close-fence late `acp_connect` (spawn finished after fence)
#[cfg(feature = "tauri-runtime")]
pub(crate) async fn residual_reconcile_after_close(
    cm: &ConnectionManager,
    tm: Option<&TerminalManager>,
    label: &str,
    operation_id: &str,
) -> Option<u64> {
    let (rebound, max_gen) = cm
        .rebind_stamped_connections_owner_window(label, operation_id, "main")
        .await;
    if rebound > 0 {
        tracing::info!(
            "[ACP] close residual stamped rebind label={} op={} count={} max_gen={:?}",
            label,
            operation_id,
            rebound,
            max_gen
        );
    }
    let n = cm
        .disconnect_idle_by_owner_window_and_operation(label, operation_id)
        .await;
    tracing::info!(
        "[ACP] conversation window close idle residual label={} op={} count={}",
        label,
        operation_id,
        n
    );
    // Terminals: rebind to main (keep PTY alive). Never kill on close residual.
    if let Some(tm) = tm {
        let n = tm.rebind_owner_window_by_operation(label, operation_id, "main");
        if n > 0 {
            tracing::info!(
                "[TERM] close residual rebound label={} op={} count={}",
                label,
                operation_id,
                n
            );
        }
    }
    max_gen
}

/// Close-fence late connect path (Route A): reverse-to-main + idle residual.
/// Never hard-kills a busy agent via `disconnect_if_owner`.
#[cfg(feature = "tauri-runtime")]
pub(crate) async fn close_fence_late_connect_reconcile(
    cm: &ConnectionManager,
    tm: Option<&TerminalManager>,
    label: &str,
    operation_id: &str,
) -> Option<u64> {
    residual_reconcile_after_close(cm, tm, label, operation_id).await
}

/// Run primary reverse for a close decision and commit an honest outcome.
#[cfg(feature = "tauri-runtime")]
async fn close_reverse_and_commit(
    popout: &ConversationPopoutState,
    cm: Option<&ConnectionManager>,
    conversation_id: i32,
    label: &str,
    operation_id: &str,
    expected_generation: Option<u64>,
) -> AbortOutcome {
    let generation_hint = expected_generation.unwrap_or(0);
    let Some(cm) = cm else {
        return popout
            .commit_close_reverse(operation_id, AbortOutcome::ReverseUncertain)
            .unwrap_or(AbortOutcome::ReverseUncertain);
    };
    match cm
        .rebind_connection_owner_window(
            conversation_id,
            None,
            label,
            "main",
            operation_id,
            expected_generation,
        )
        .await
    {
        Ok(rev) => popout
            .commit_close_reverse(
                operation_id,
                AbortOutcome::Reversed {
                    generation: rev.ownership_generation,
                },
            )
            .unwrap_or(AbortOutcome::Reversed {
                generation: rev.ownership_generation,
            }),
        Err(e) => {
            let msg = e.to_string();
            tracing::warn!(
                "[popout] reverse rebind on close failed label={} op={} gen={:?}: {}",
                label,
                operation_id,
                expected_generation,
                e
            );
            let classified = classify_close_reverse_error(&msg, generation_hint);
            popout
                .commit_close_reverse(operation_id, classified.clone())
                .unwrap_or(classified)
        }
    }
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

    // Condition-based close vs rebind: leave an abort reservation immediately
    // so a finishing forward rebind observes close and reverse/reaps instead of
    // stranding ownership on this closed child. Poll until rebind_in_flight
    // clears (or a hard upper bound), then decide_close.
    //
    // Close cleanup is already reserved via capture_close_operation; that fence
    // alone makes record_rebind reject. We also set abort_reserved for symmetry
    // with decide_close's NeedReverse path.
    if let Err(e) = popout.reserve_abort_for_close(&operation_id) {
        tracing::warn!(
            "[popout] close reserve_abort_for_close failed label={} op={}: {}",
            label,
            operation_id,
            e
        );
    }

    // ~5s upper bound at 25ms; finishing rebind should clear long before this.
    // After the bound: fall through to best-effort reverse + residual (never
    // early-return with null abortOutcome only).
    const CLOSE_REBIND_WAIT_ITERS: u32 = 200;
    let mut decision = None;
    for _ in 0..CLOSE_REBIND_WAIT_ITERS {
        match popout.decide_close(&operation_id) {
            Ok(d) => {
                decision = Some(d);
                break;
            }
            Err(e) if e.to_string().contains("rebind is in flight") => {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(e) => {
                tracing::warn!(
                    "[popout] close decide_close failed label={} op={}: {}",
                    label,
                    operation_id,
                    e
                );
                // Non-destructive: tombstone only; do not disconnect.
                // Abort/close reservations remain so late record_rebind reverses.
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
                "[popout] close waiting on rebind timed out; best-effort reverse + residual label={} op={} rebind_in_flight={}",
                label,
                operation_id,
                popout.is_rebind_in_flight(&operation_id)
            );
            // Timeout fall-through: NeedReverseBestEffort + residual + honest
            // terminal outcome (never emit closed with null-only cleanup).
            CloseDecision::NeedReverseBestEffort { conversation_id }
        }
    };

    let cm = app.try_state::<ConnectionManager>();
    let cm_ref = cm.as_ref().map(|s| s.inner());

    let outcome = match decision {
        CloseDecision::Done { outcome, .. } => outcome,
        CloseDecision::NeedReverse {
            conversation_id: cid,
            generation,
        } => {
            close_reverse_and_commit(
                popout.inner(),
                cm_ref,
                cid,
                label,
                &operation_id,
                Some(generation),
            )
            .await
        }
        CloseDecision::NeedReverseBestEffort {
            conversation_id: cid,
        } => {
            close_reverse_and_commit(
                popout.inner(),
                cm_ref,
                cid,
                label,
                &operation_id,
                None,
            )
            .await
        }
    };

    // Publish close fence (tombstone) BEFORE the residual scan so a concurrent
    // acp_connect that finishes after the scan still sees the fence and tears
    // down, and so begin_registration rejects new work for this incarnation.
    // (reserve_close_operation already ran in the window event handler.)
    popout.tombstone_on_close(label, &operation_id);

    // Residual always runs for close (including Done / ReverseUncertain):
    // best-effort reverse leftovers + idle-only disconnect. Never full
    // disconnect_by_owner_window_and_operation on close paths.
    //
    // Collect residual reverse generations so a premature Superseded /
    // ReverseUncertain from the primary reverse can be upgraded to Reversed
    // before we publish closed (rebind-timeout / late residual path).
    let mut residual_reverse_gen: Option<u64> = None;
    let tm = app.try_state::<TerminalManager>();
    if let Some(cm) = cm_ref {
        if let Some(gen) = residual_reconcile_after_close(
            cm,
            tm.as_ref().map(|s| s.inner()),
            label,
            &operation_id,
        )
        .await
        {
            residual_reverse_gen = Some(
                residual_reverse_gen.map_or(gen, |m| m.max(gen)),
            );
        }

        // Wait for in-flight registrations that began before the fence, then
        // final residual pass (same helper).
        const INFLIGHT_WAIT_MS: u64 = 25;
        const INFLIGHT_WAIT_ITERS: u32 = 80; // ~2s
        for _ in 0..INFLIGHT_WAIT_ITERS {
            if popout.inflight_registrations(&operation_id) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(INFLIGHT_WAIT_MS)).await;
        }
        if popout.inflight_registrations(&operation_id) > 0 {
            tracing::warn!(
                "[popout] close final residual with inflight still >0 label={} op={} inflight={}",
                label,
                operation_id,
                popout.inflight_registrations(&operation_id)
            );
        }
        if let Some(gen) = residual_reconcile_after_close(
            cm,
            tm.as_ref().map(|s| s.inner()),
            label,
            &operation_id,
        )
        .await
        {
            residual_reverse_gen = Some(
                residual_reverse_gen.map_or(gen, |m| m.max(gen)),
            );
        }
    }

    // Prefer a late residual reverse generation over Superseded/Uncertain/
    // ConnectionGone so FE receives reclaimable Reversed when residual
    // rebound_count > 0 actually moved ownership to main (including cold-
    // stamped connections primary reverse missed).
    let outcome = if let Some(gen) = residual_reverse_gen {
        popout
            .commit_close_reverse(
                &operation_id,
                AbortOutcome::Reversed { generation: gen },
            )
            .unwrap_or(outcome)
    } else {
        outcome
    };

    // Harden: about to publish ConnectionGone — re-check live status and any
    // residual already left on main with this op. A racing late record_rebind
    // residual may have moved ownership and/or upgraded the stored outcome
    // after we snapshotted Done(ConnectionGone).
    let outcome = upgrade_connection_gone_before_emit(
        popout.inner(),
        cm_ref,
        &operation_id,
        outcome,
    )
    .await;

    let _ = app.emit(
        "conversation-window://closed",
        serde_json::json!({
            "conversationId": conversation_id,
            "operationId": operation_id,
            "abortOutcome": outcome,
        }),
    );
}

/// If `outcome` is `ConnectionGone`, prefer a live upgraded status or residual
/// already on `main` with matching op (commit allows ConnectionGone→Reversed).
#[cfg(feature = "tauri-runtime")]
async fn upgrade_connection_gone_before_emit(
    popout: &ConversationPopoutState,
    cm: Option<&ConnectionManager>,
    operation_id: &str,
    outcome: AbortOutcome,
) -> AbortOutcome {
    if !matches!(outcome, AbortOutcome::ConnectionGone) {
        return outcome;
    }
    // Live status may already be Reversed from a racing residual commit.
    if let Ok(status) = popout.get_status(operation_id) {
        if let Some(AbortOutcome::Reversed { generation }) = status.abort_outcome {
            return AbortOutcome::Reversed { generation };
        }
    }
    let Some(cm) = cm else {
        return outcome;
    };
    let Some(gen) = cm
        .max_ownership_generation_for_owner_operation("main", operation_id)
        .await
    else {
        return outcome;
    };
    popout
        .commit_close_reverse(
            operation_id,
            AbortOutcome::Reversed { generation: gen },
        )
        .unwrap_or(outcome)
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

    /// Recording fake for open/focus idempotency behavioral tests.
    #[derive(Default)]
    struct FakeConversationWindowOps {
        /// Labels that already have a window.
        existing: std::sync::Mutex<std::collections::HashSet<String>>,
        unminimize_calls: std::sync::atomic::AtomicUsize,
        focus_calls: std::sync::atomic::AtomicUsize,
        insert_op_calls: std::sync::atomic::AtomicUsize,
        create_window_calls: std::sync::atomic::AtomicUsize,
        last_focused_label: std::sync::Mutex<Option<String>>,
    }

    impl FakeConversationWindowOps {
        fn with_existing(label: impl Into<String>) -> Self {
            let fake = Self::default();
            fake.existing.lock().unwrap().insert(label.into());
            fake
        }

        fn count(atom: &std::sync::atomic::AtomicUsize) -> usize {
            atom.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl ConversationWindowOps for FakeConversationWindowOps {
        fn get_by_label(&self, label: &str) -> bool {
            self.existing.lock().unwrap().contains(label)
        }

        fn unminimize(&self, _label: &str) {
            self.unminimize_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn set_focus(&self, label: &str) -> Result<(), String> {
            self.focus_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.last_focused_label.lock().unwrap() = Some(label.to_string());
            Ok(())
        }

        fn insert_op(
            &self,
            _conversation_id: i32,
            _operation_id: &str,
            _label: &str,
        ) -> Result<(), String> {
            self.insert_op_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }

        fn create_window(&self, _label: &str) -> Result<(), String> {
            self.create_window_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn open_conversation_focuses_existing_when_label_present() {
        let label = conversation_window_label(42);
        let fake = FakeConversationWindowOps::with_existing(label.clone());

        let result =
            decide_open_or_focus_existing(&fake, 42, "op-focus-1", &label).expect("focus path");

        assert_eq!(result, OpenConversationResult::FocusedExisting);
        assert_eq!(FakeConversationWindowOps::count(&fake.focus_calls), 1);
        assert_eq!(FakeConversationWindowOps::count(&fake.unminimize_calls), 1);
        assert_eq!(FakeConversationWindowOps::count(&fake.insert_op_calls), 0);
        assert_eq!(
            FakeConversationWindowOps::count(&fake.create_window_calls),
            0
        );
        assert_eq!(
            fake.last_focused_label.lock().unwrap().as_deref(),
            Some(label.as_str())
        );

        // Production early-return helper agrees with the same fake.
        let again = try_focus_existing_conversation_window(&fake, &label)
            .expect("try focus")
            .expect("existing maps to Some");
        assert_eq!(again, OpenConversationResult::FocusedExisting);
        assert_eq!(FakeConversationWindowOps::count(&fake.focus_calls), 2);
        assert_eq!(FakeConversationWindowOps::count(&fake.insert_op_calls), 0);
        assert_eq!(
            FakeConversationWindowOps::count(&fake.create_window_calls),
            0
        );

        let json = serde_json::to_value(OpenConversationResult::FocusedExisting).unwrap();
        assert_eq!(json, serde_json::json!("focusedExisting"));
        let back: OpenConversationResult = serde_json::from_value(json).unwrap();
        assert_eq!(back, OpenConversationResult::FocusedExisting);
    }

    #[test]
    fn open_conversation_creates_when_label_absent() {
        let label = conversation_window_label(7);
        let fake = FakeConversationWindowOps::default();

        let result =
            decide_open_or_focus_existing(&fake, 7, "op-open-1", &label).expect("open path");

        assert_eq!(result, OpenConversationResult::Opened);
        assert_eq!(FakeConversationWindowOps::count(&fake.focus_calls), 0);
        assert_eq!(FakeConversationWindowOps::count(&fake.unminimize_calls), 0);
        assert_eq!(FakeConversationWindowOps::count(&fake.insert_op_calls), 1);
        assert_eq!(
            FakeConversationWindowOps::count(&fake.create_window_calls),
            1
        );

        assert_eq!(
            try_focus_existing_conversation_window(&fake, &label).unwrap(),
            None
        );
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

    /// Barrier: close reserves (+ abort reserved / NeedReverse in flight) then
    /// concurrent complete must not mark HandoffComplete — otherwise reverse
    /// commits AlreadyComplete and main never reclaims the main lease.
    #[test]
    fn complete_rejects_when_close_or_abort_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-race".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-race").unwrap();
        state.record_rebind("op-race", 5).unwrap();
        assert_eq!(
            state.get_status("op-race").unwrap().phase,
            PopoutPhase::ReadyPending
        );

        // Close path: fence + decide_abort → NeedReverse (abort_reserved).
        assert!(state.reserve_close_operation("op-race"));
        match state.decide_abort("op-race").unwrap() {
            AbortDecision::NeedReverse { generation, .. } => assert_eq!(generation, 5),
            other => panic!("expected NeedReverse, got {other:?}"),
        }

        // Main complete must fail while reverse/cleanup is in flight.
        let err = state.complete("op-race").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("close is reserved") || msg.contains("abort is reserved"),
            "expected close/abort rejection, got {msg}"
        );
        assert_eq!(
            state.get_status("op-race").unwrap().phase,
            PopoutPhase::ReadyPending,
            "must not mark HandoffComplete during NeedReverse"
        );

        // After reverse, abort commits Reversed (not AlreadyComplete).
        let outcome = state
            .abort("op-race", |_| AbortOutcome::Reversed { generation: 6 })
            .unwrap();
        assert!(matches!(
            outcome,
            AbortOutcome::Reversed { generation: 6 }
        ));
        assert_eq!(
            state.get_status("op-race").unwrap().phase,
            PopoutPhase::Aborted
        );

        // Complete after abort surfaces aborted (not success).
        let status = state.complete("op-race").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
    }

    #[test]
    fn complete_rejects_when_only_close_fence_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(2, "op-close-only".into(), "conversation-2".into())
            .unwrap();
        assert!(state.reserve_close_operation("op-close-only"));
        let err = state.complete("op-close-only").unwrap_err();
        assert!(
            err.to_string().contains("close is reserved"),
            "got {}",
            err
        );
        assert_ne!(
            state.get_status("op-close-only").unwrap().phase,
            PopoutPhase::HandoffComplete
        );
    }

    #[test]
    fn complete_rejects_when_only_abort_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(3, "op-abort-only".into(), "conversation-3".into())
            .unwrap();
        state.admit_forward_rebind("op-abort-only").unwrap();
        state.record_rebind("op-abort-only", 1).unwrap();
        match state.decide_abort("op-abort-only").unwrap() {
            AbortDecision::NeedReverse { .. } => {}
            other => panic!("expected NeedReverse, got {other:?}"),
        }
        // No close fence — abort_reserved alone must still block complete.
        let err = state.complete("op-abort-only").unwrap_err();
        assert!(
            err.to_string().contains("abort is reserved"),
            "got {}",
            err
        );
    }

    #[test]
    fn complete_idempotent_after_handoff_even_if_close_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(4, "op-done".into(), "conversation-4".into())
            .unwrap();
        state.complete("op-done").unwrap();
        assert!(state.reserve_close_operation("op-done"));
        let s = state.complete("op-done").unwrap();
        assert_eq!(s.phase, PopoutPhase::HandoffComplete);
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
    fn begin_registration_rejects_tombstoned_and_tracks_inflight() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        state
            .begin_registration("conversation-1", "op1")
            .expect("open incarnation accepts registration");
        {
            let by_op = state.by_operation.lock().unwrap();
            assert_eq!(by_op.get("op1").unwrap().inflight_registrations, 1);
        }
        state.tombstone_on_close("conversation-1", "op1");
        assert!(state.is_tombstoned("conversation-1", "op1"));
        // Late begin during/after close must fail so cold connect aborts.
        assert!(state
            .begin_registration("conversation-1", "op1")
            .is_err());
        state.end_registration("op1");
        {
            let by_op = state.by_operation.lock().unwrap();
            assert_eq!(by_op.get("op1").unwrap().inflight_registrations, 0);
        }
    }

    #[test]
    fn begin_registration_rejects_close_reserved_before_tombstone() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op1".into(), "conversation-1".into())
            .unwrap();
        assert!(state.reserve_close_operation("op1"));
        // Fence is published with reserve; registration must not start.
        assert!(state
            .begin_registration("conversation-1", "op1")
            .is_err());
        assert_eq!(state.inflight_registrations("op1"), 0);
    }

    #[test]
    fn close_fence_with_inflight_registration_then_final_reap_window() {
        // Barrier-style interleaving of registration vs close fence:
        // 1) registration begins (inflight=1)
        // 2) close reserves + tombstones (new begin rejected)
        // 3) registration ends (inflight=0) — close's final reap can run
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-race".into(), "conversation-1".into())
            .unwrap();
        state
            .begin_registration("conversation-1", "op-race")
            .expect("pre-close registration accepted");
        assert_eq!(state.inflight_registrations("op-race"), 1);

        assert!(state.reserve_close_operation("op-race"));
        state.tombstone_on_close("conversation-1", "op-race");
        assert!(state.is_tombstoned("conversation-1", "op-race"));
        assert!(state
            .begin_registration("conversation-1", "op-race")
            .is_err());

        // In-flight registration still holds the count until it finishes.
        assert_eq!(state.inflight_registrations("op-race"), 1);
        state.end_registration("op-race");
        assert_eq!(state.inflight_registrations("op-race"), 0);
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

    /// Barrier: close reserves abort while rebind is in flight; a finishing
    /// forward rebind that would complete after the old 500ms close timeout
    /// must not become visible — `record_rebind` rejects so the rebind path
    /// reverses / reaps. Gen + in-flight fence stay until forced reverse
    /// commits (see close_reserved_forced_reverse_pending_not_never_rebound).
    #[test]
    fn late_record_rebind_rejects_after_close_abort_reservation() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-late".into(), "conversation-1".into())
            .unwrap();
        // Forward rebind admitted (ownership mid-flight).
        state.admit_forward_rebind("op-late").unwrap();
        assert!(state.is_rebind_in_flight("op-late"));
        // Close starts: cleanup fence + abort reservation (even while in flight).
        assert!(state.reserve_close_operation("op-late"));
        state.reserve_abort_for_close("op-late").unwrap();
        // decide_abort still blocked while rebind_in_flight.
        assert!(state.decide_abort("op-late").is_err());
        // Simulate wall-clock beyond the old 20×25ms poll: rebind finishes late.
        // record_rebind must reject (close + abort reservation) but keep fence.
        let err = state.record_rebind("op-late", 9).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("closed") || msg.contains("abort is reserved"),
            "expected close/abort rejection, got {msg}"
        );
        assert!(
            state.is_rebind_in_flight("op-late"),
            "must hold in-flight until forced reverse commits"
        );
        assert_eq!(
            state.get_status("op-late").unwrap().ownership_generation,
            Some(9)
        );
        // After reverse, forced-reverse abort commits (regular abort still
        // blocked while fence holds).
        assert!(state
            .abort("op-late", |_| AbortOutcome::Reversed { generation: 9 })
            .is_err());
        let outcome = state
            .abort_after_forced_reverse(
                "op-late",
                AbortOutcome::Reversed { generation: 9 },
            )
            .unwrap();
        assert!(matches!(
            outcome,
            AbortOutcome::Reversed { generation: 9 }
        ));
        assert!(!state.is_rebind_in_flight("op-late"));
        // New forward admits stay blocked (terminal + close reserved).
        assert!(state.admit_forward_rebind("op-late").is_err());
    }

    #[test]
    fn record_rebind_rejects_when_only_close_cleanup_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(2, "op-close".into(), "conversation-2".into())
            .unwrap();
        state.admit_forward_rebind("op-close").unwrap();
        assert!(state.reserve_close_operation("op-close"));
        // No abort_reserved yet — close fence alone must still reject.
        assert!(state.record_rebind("op-close", 1).is_err());
        // Fence + gen held until forced reverse commits.
        assert!(state.is_rebind_in_flight("op-close"));
        assert_eq!(
            state.get_status("op-close").unwrap().ownership_generation,
            Some(1)
        );
    }

    #[test]
    fn admit_forward_rebind_rejects_close_reserved() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(3, "op-adm".into(), "conversation-3".into())
            .unwrap();
        assert!(state.reserve_close_operation("op-adm"));
        assert!(state.admit_forward_rebind("op-adm").is_err());
    }

    #[test]
    fn record_reverse_generation_updates_forward_gen_for_abort_cas() {
        // Prior rollback left gen-2; second forward stamps gen-3; pre-ready
        // reverse must advance the op record to gen-4 so abort is not Superseded.
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-B".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-B").unwrap();
        state.record_rebind("op-B", 3).unwrap();
        {
            let by_op = state.by_operation.lock().unwrap();
            assert_eq!(by_op.get("op-B").unwrap().ownership_generation, Some(3));
        }
        state.record_reverse_generation("op-B", 4).unwrap();
        {
            let by_op = state.by_operation.lock().unwrap();
            assert_eq!(by_op.get("op-B").unwrap().ownership_generation, Some(4));
        }
        match state.decide_abort("op-B").unwrap() {
            AbortDecision::NeedReverse { generation, .. } => assert_eq!(generation, 4),
            other => panic!("expected NeedReverse(4), got {other:?}"),
        }
    }

    #[test]
    fn reverse_error_is_connection_gone_matches_manager_messages() {
        assert!(reverse_error_is_connection_gone(
            "connection abc not found"
        ));
        assert!(reverse_error_is_connection_gone(
            "no connection for conversation 12"
        ));
        assert!(!reverse_error_is_connection_gone(
            "generation CAS failed: expected 3, have 4"
        ));
        assert!(!reverse_error_is_connection_gone(
            "owner label CAS failed: expected conversation-1, have main"
        ));
    }

    #[test]
    fn abort_outcome_connection_gone_serializes() {
        let json = serde_json::to_value(AbortOutcome::ConnectionGone).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "connection_gone" })
        );
        let back: AbortOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, AbortOutcome::ConnectionGone);
    }

    #[test]
    fn close_reserved_forced_reverse_not_found_is_connection_gone() {
        assert_eq!(
            abort_outcome_for_close_reserved_forced_reverse(
                None,
                Some("connection abc not found"),
                9
            ),
            AbortOutcome::ConnectionGone
        );
        assert_eq!(
            abort_outcome_for_close_reserved_forced_reverse(Some(11), None, 9),
            AbortOutcome::Reversed { generation: 11 }
        );
        // CAS failure maps to Superseded (not fabricated Reversed).
        assert_eq!(
            abort_outcome_for_close_reserved_forced_reverse(
                None,
                Some("generation CAS failed: expected 3, have 4"),
                9
            ),
            AbortOutcome::Superseded {
                current_generation: 9,
                current_owner: "unknown".into(),
            }
        );
    }

    #[test]
    fn decide_close_handoff_complete_needs_reverse() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-hc".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-hc").unwrap();
        state.record_rebind("op-hc", 5).unwrap();
        state.complete("op-hc").unwrap();

        match state.decide_close("op-hc").unwrap() {
            CloseDecision::NeedReverse {
                conversation_id,
                generation,
            } => {
                assert_eq!(conversation_id, 1);
                assert_eq!(generation, 5);
            }
            other => panic!("expected NeedReverse, got {other:?}"),
        }
        // API abort path still AlreadyComplete on HandoffComplete.
        match state.decide_abort("op-hc").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::AlreadyComplete,
                ..
            } => {}
            other => panic!("decide_abort must stay AlreadyComplete, got {other:?}"),
        }
    }

    #[test]
    fn commit_close_reverse_from_handoff_complete_sets_aborted_reversed() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-cc".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-cc").unwrap();
        state.record_rebind("op-cc", 3).unwrap();
        state.complete("op-cc").unwrap();

        let outcome = state
            .commit_close_reverse("op-cc", AbortOutcome::Reversed { generation: 4 })
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 4 });
        let status = state.get_status("op-cc").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::Reversed { generation: 4 })
        );
        assert_eq!(status.ownership_generation, Some(4));
        assert!(!state.is_rebind_in_flight("op-cc"));

        // API decide_abort on a fresh HandoffComplete still short-circuits;
        // after commit_close_reverse the stored outcome is Reversed.
        match state.decide_abort("op-cc").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::Reversed { generation: 4 },
                ..
            } => {}
            other => panic!("expected Done(Reversed), got {other:?}"),
        }
    }

    #[test]
    fn decide_close_after_api_already_complete_still_needs_reverse() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-ac".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-ac").unwrap();
        state.record_rebind("op-ac", 7).unwrap();
        state.complete("op-ac").unwrap();
        // API abort commits AlreadyComplete under HandoffComplete.
        match state.decide_abort("op-ac").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::AlreadyComplete,
                ..
            } => {}
            other => panic!("expected AlreadyComplete, got {other:?}"),
        }
        // Close must still reverse-first despite stored AlreadyComplete.
        match state.decide_close("op-ac").unwrap() {
            CloseDecision::NeedReverse { generation, .. } => assert_eq!(generation, 7),
            other => panic!("expected NeedReverse, got {other:?}"),
        }
        let outcome = state
            .commit_close_reverse("op-ac", AbortOutcome::Reversed { generation: 8 })
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 8 });
        assert_eq!(
            state.get_status("op-ac").unwrap().abort_outcome,
            Some(AbortOutcome::Reversed { generation: 8 })
        );
    }

    #[test]
    fn decide_close_after_api_reversed_is_done_no_second_reverse() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-rev".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-rev").unwrap();
        state.record_rebind("op-rev", 2).unwrap();
        state
            .abort("op-rev", |_| AbortOutcome::Reversed { generation: 3 })
            .unwrap();
        match state.decide_close("op-rev").unwrap() {
            CloseDecision::Done {
                outcome: AbortOutcome::Reversed { generation: 3 },
                conversation_id: 1,
            } => {}
            other => panic!("expected Done(Reversed), got {other:?}"),
        }
    }

    #[test]
    fn decide_close_after_api_connection_gone_is_done() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(2, "op-gone2".into(), "conversation-2".into())
            .unwrap();
        state.admit_forward_rebind("op-gone2").unwrap();
        state.record_rebind("op-gone2", 1).unwrap();
        state
            .abort("op-gone2", |_| AbortOutcome::ConnectionGone)
            .unwrap();
        match state.decide_close("op-gone2").unwrap() {
            CloseDecision::Done {
                outcome: AbortOutcome::ConnectionGone,
                conversation_id: 2,
            } => {}
            other => panic!("expected Done(ConnectionGone), got {other:?}"),
        }
    }

    #[test]
    fn decide_close_no_gen_is_need_reverse_best_effort() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(3, "op-be".into(), "conversation-3".into())
            .unwrap();
        match state.decide_close("op-be").unwrap() {
            CloseDecision::NeedReverseBestEffort {
                conversation_id: 3,
            } => {}
            other => panic!("expected NeedReverseBestEffort, got {other:?}"),
        }
        // API abort still commits NeverRebound when no gen.
        match state.decide_abort("op-be").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::NeverRebound,
                ..
            } => {}
            other => panic!("API path expected NeverRebound, got {other:?}"),
        }
    }

    #[test]
    fn abort_outcome_unknown_reverse_is_uncertain_not_reversed() {
        let o = abort_outcome_for_close_reserved_forced_reverse(
            None,
            Some("weird error"),
            9,
        );
        assert_eq!(o, AbortOutcome::ReverseUncertain);
    }

    #[test]
    fn abort_outcome_operation_cas_is_superseded_not_uncertain() {
        let o = abort_outcome_for_close_reserved_forced_reverse(
            None,
            Some("owner operation CAS failed: expected op-A, have op-B"),
            12,
        );
        assert_eq!(
            o,
            AbortOutcome::Superseded {
                current_generation: 12,
                current_owner: "unknown".into(),
            }
        );
        assert!(reverse_err_is_cas_superseded(
            "owner operation CAS failed: expected op-A, have op-B"
        ));
    }

    #[test]
    fn reverse_uncertain_serializes_as_kind_snake_case() {
        let json = serde_json::to_value(AbortOutcome::ReverseUncertain).unwrap();
        assert_eq!(json["kind"], "reverse_uncertain");
        let back: AbortOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, AbortOutcome::ReverseUncertain);
    }

    #[test]
    fn rebind_in_flight_timeout_falls_through_to_best_effort_residual() {
        // Close poll times out while rebind_in_flight: treat as
        // NeedReverseBestEffort, commit terminal outcome, clear fences.
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-to".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-to").unwrap();
        assert!(state.reserve_close_operation("op-to"));
        state.reserve_abort_for_close("op-to").unwrap();
        assert!(state.is_rebind_in_flight("op-to"));

        let err = state.decide_close("op-to").unwrap_err();
        assert!(
            err.to_string().contains("rebind is in flight"),
            "got {err}"
        );

        // Timeout branch constructs NeedReverseBestEffort from known conversation.
        let status = state.get_status("op-to").unwrap();
        let decision = CloseDecision::NeedReverseBestEffort {
            conversation_id: status.conversation_id,
        };
        assert_eq!(
            decision,
            CloseDecision::NeedReverseBestEffort {
                conversation_id: 1
            }
        );

        // Without a clear reverse success, commit ReverseUncertain and clear
        // abort_reserved + rebind_in_flight (residual still runs in handler).
        let outcome = state
            .commit_close_reverse("op-to", AbortOutcome::ReverseUncertain)
            .unwrap();
        assert_eq!(outcome, AbortOutcome::ReverseUncertain);
        assert!(!state.is_rebind_in_flight("op-to"));
        let status = state.get_status("op-to").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::ReverseUncertain)
        );
        assert!(
            !status
                .abort_outcome
                .as_ref()
                .is_some_and(|o| matches!(o, AbortOutcome::Reversed { .. })),
            "must not fabricate Reversed on timeout"
        );
    }

    /// Timeout may commit ReverseUncertain first; a late successful reverse
    /// must upgrade to Reversed{gen} and stamp ownership_generation.
    #[test]
    fn commit_close_reverse_upgrades_reverse_uncertain_to_reversed_with_gen() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-upg".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-upg").unwrap();
        state.record_rebind("op-upg", 5).unwrap();

        // Simulate timeout path committing ReverseUncertain first.
        let uncertain = state
            .commit_close_reverse("op-upg", AbortOutcome::ReverseUncertain)
            .unwrap();
        assert_eq!(uncertain, AbortOutcome::ReverseUncertain);
        let status = state.get_status("op-upg").unwrap();
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::ReverseUncertain)
        );
        // Forward gen still present; no reverse gen stamped yet.
        assert_eq!(status.ownership_generation, Some(5));

        // Late reverse succeeds with a real post-reverse generation.
        let outcome = state
            .commit_close_reverse("op-upg", AbortOutcome::Reversed { generation: 6 })
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 6 });
        let status = state.get_status("op-upg").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::Reversed { generation: 6 })
        );
        assert_eq!(
            status.ownership_generation,
            Some(6),
            "upgrade must stamp reverse ownership_generation"
        );
        assert!(!state.is_rebind_in_flight("op-upg"));
    }

    /// Rebind-timeout / CAS race may commit Superseded first; a late successful
    /// reverse must upgrade to Reversed{gen} so FE can reclaim (same as
    /// ReverseUncertain upgrade).
    #[test]
    fn commit_close_reverse_upgrades_superseded_to_reversed_with_gen() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(1, "op-ss".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-ss").unwrap();
        state.record_rebind("op-ss", 4).unwrap();

        let superseded = state
            .commit_close_reverse(
                "op-ss",
                AbortOutcome::Superseded {
                    current_generation: 4,
                    current_owner: "unknown".into(),
                },
            )
            .unwrap();
        assert_eq!(
            superseded,
            AbortOutcome::Superseded {
                current_generation: 4,
                current_owner: "unknown".into(),
            }
        );
        assert_eq!(
            state.get_status("op-ss").unwrap().abort_outcome,
            Some(AbortOutcome::Superseded {
                current_generation: 4,
                current_owner: "unknown".into(),
            })
        );

        let outcome = state
            .commit_close_reverse("op-ss", AbortOutcome::Reversed { generation: 7 })
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 7 });
        let status = state.get_status("op-ss").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::Reversed { generation: 7 })
        );
        assert_eq!(
            status.ownership_generation,
            Some(7),
            "Superseded→Reversed upgrade must stamp reverse ownership_generation"
        );
        assert!(!state.is_rebind_in_flight("op-ss"));
    }

    /// Residual stamped reverse that lands after a premature Superseded commit
    /// must upgrade the stored outcome so closed emit can publish Reversed.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn residual_stamped_rebind_upgrades_superseded_outcome() {
        use crate::acp::manager::ConnectionManager;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let state = ConversationPopoutState::new();
        state
            .insert_opened(9, "op-res".into(), "conversation-9".into())
            .unwrap();
        state.admit_forward_rebind("op-res").unwrap();
        let _ = state.record_rebind("op-res", 2);

        // Primary reverse CAS path published Superseded first.
        state
            .commit_close_reverse(
                "op-res",
                AbortOutcome::Superseded {
                    current_generation: 2,
                    current_owner: "unknown".into(),
                },
            )
            .unwrap();

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "still-on-popout",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("still-on-popout").unwrap();
            conn.owner_window_label = "conversation-9".into();
            conn.owner_operation_id = Some("op-res".into());
            conn.ownership_generation = 2;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-9".into();
            st.conversation_id = Some(9);
            st.status = crate::acp::types::ConnectionStatus::Prompting;
        }

        let max_gen =
            residual_reconcile_after_close(&cm, None, "conversation-9", "op-res").await;
        assert!(
            max_gen.is_some(),
            "busy leftover must reverse to main with a new generation"
        );
        let gen = max_gen.unwrap();
        let upgraded = state
            .commit_close_reverse("op-res", AbortOutcome::Reversed { generation: gen })
            .unwrap();
        assert_eq!(upgraded, AbortOutcome::Reversed { generation: gen });
        assert_eq!(
            state.get_status("op-res").unwrap().abort_outcome,
            Some(AbortOutcome::Reversed { generation: gen })
        );
        {
            let map = cm.connections.lock().await;
            let conn = map.get("still-on-popout").expect("busy survives");
            assert_eq!(conn.owner_window_label, "main");
            assert_eq!(conn.ownership_generation, gen);
        }
    }

    /// Primary reverse by conversation_id can miss a cold-stamped connection
    /// (no conversation_id on state) and commit ConnectionGone; residual
    /// stamped rebind still moves ownership to main and must upgrade to
    /// reclaimable Reversed{gen}.
    #[test]
    fn commit_close_reverse_upgrades_connection_gone_to_reversed_with_gen() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(11, "op-cg".into(), "conversation-11".into())
            .unwrap();
        state.admit_forward_rebind("op-cg").unwrap();
        state.record_rebind("op-cg", 3).unwrap();

        let gone = state
            .commit_close_reverse("op-cg", AbortOutcome::ConnectionGone)
            .unwrap();
        assert_eq!(gone, AbortOutcome::ConnectionGone);
        assert_eq!(
            state.get_status("op-cg").unwrap().abort_outcome,
            Some(AbortOutcome::ConnectionGone)
        );

        let outcome = state
            .commit_close_reverse("op-cg", AbortOutcome::Reversed { generation: 4 })
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 4 });
        let status = state.get_status("op-cg").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::Reversed { generation: 4 })
        );
        assert_eq!(
            status.ownership_generation,
            Some(4),
            "ConnectionGone→Reversed upgrade must stamp reverse ownership_generation"
        );
        assert!(!state.is_rebind_in_flight("op-cg"));
    }

    /// Cold-stamped residual: primary reverse misses (no conversation_id →
    /// ConnectionGone), residual stamped rebind moves to main, close path
    /// must publish Reversed when rebound_count > 0.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn residual_cold_stamped_rebind_upgrades_connection_gone_outcome() {
        use crate::acp::manager::ConnectionManager;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let state = ConversationPopoutState::new();
        state
            .insert_opened(12, "op-cold".into(), "conversation-12".into())
            .unwrap();
        state.admit_forward_rebind("op-cold").unwrap();
        let _ = state.record_rebind("op-cold", 1);

        // Primary reverse by conversation_id found nothing → ConnectionGone.
        state
            .commit_close_reverse("op-cold", AbortOutcome::ConnectionGone)
            .unwrap();

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "cold-stamped",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("cold-stamped").unwrap();
            conn.owner_window_label = "conversation-12".into();
            conn.owner_operation_id = Some("op-cold".into());
            conn.ownership_generation = 1;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-12".into();
            // Cold stamp: op-owned on popout, but no conversation_id — primary
            // reverse by conversation_id misses this connection.
            st.conversation_id = None;
            st.status = crate::acp::types::ConnectionStatus::Prompting;
        }

        // Primary reverse would miss; residual stamped rebind must move it.
        let primary = cm
            .rebind_connection_owner_window(
                12,
                None,
                "conversation-12",
                "main",
                "op-cold",
                Some(1),
            )
            .await;
        assert!(
            primary.is_err(),
            "primary reverse must miss cold-stamped connection without conversation_id"
        );

        let max_gen =
            residual_reconcile_after_close(&cm, None, "conversation-12", "op-cold").await;
        assert!(
            max_gen.is_some(),
            "residual rebound_count>0 must return max_gen for close upgrade"
        );
        let gen = max_gen.unwrap();

        // Close path: residual reverse after primary ConnectionGone → Reversed.
        let upgraded = state
            .commit_close_reverse("op-cold", AbortOutcome::Reversed { generation: gen })
            .unwrap();
        assert_eq!(
            upgraded,
            AbortOutcome::Reversed { generation: gen },
            "ConnectionGone must upgrade to Reversed when residual stamped rebind succeeded"
        );
        assert_eq!(
            state.get_status("op-cold").unwrap().abort_outcome,
            Some(AbortOutcome::Reversed { generation: gen })
        );
        {
            let map = cm.connections.lock().await;
            let conn = map.get("cold-stamped").expect("cold-stamped survives");
            assert_eq!(conn.owner_window_label, "main");
            assert_eq!(conn.ownership_generation, gen);
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-cold"));
        }
    }

    /// Pure order rule: residual max_gen wins over forced ConnectionGone so we
    /// never commit non-reclaimable ConnectionGone when residual rebound_count>0.
    #[test]
    fn close_reserved_outcome_prefers_residual_reversed_over_connection_gone() {
        assert_eq!(
            close_reserved_outcome_after_residual(
                AbortOutcome::ConnectionGone,
                Some(42),
            ),
            AbortOutcome::Reversed { generation: 42 }
        );
        assert_eq!(
            close_reserved_outcome_after_residual(AbortOutcome::ConnectionGone, None),
            AbortOutcome::ConnectionGone
        );
        assert_eq!(
            close_reserved_outcome_after_residual(
                AbortOutcome::ReverseUncertain,
                Some(7),
            ),
            AbortOutcome::Reversed { generation: 7 }
        );
        assert_eq!(
            close_reserved_outcome_after_residual(
                AbortOutcome::Reversed { generation: 3 },
                Some(9),
            ),
            AbortOutcome::Reversed { generation: 9 },
            "residual gen preferred even when forced reverse already Reversed"
        );
    }

    /// Wave2 race regression: residual stamped rebind MUST complete while
    /// `rebind_in_flight` is still true, then a single commit publishes
    /// `Reversed{max_gen}` — never commit ConnectionGone first (that clears
    /// the fence and lets close emit stale non-reclaimable ConnectionGone).
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn late_record_rebind_close_reserved_residual_before_commit_order() {
        use crate::acp::manager::ConnectionManager;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let state = ConversationPopoutState::new();
        state
            .insert_opened(13, "op-order".into(), "conversation-13".into())
            .unwrap();
        state.admit_forward_rebind("op-order").unwrap();
        assert!(state.reserve_close_operation("op-order"));
        // record_rebind loses to close fence — keeps rebind_in_flight.
        assert!(state.record_rebind("op-order", 5).is_err());
        assert!(
            state.is_rebind_in_flight("op-order"),
            "fence must stay set until residual + commit finish"
        );

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "order-stamped",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("order-stamped").unwrap();
            // After forced reverse miss (no conversation_id), ownership still
            // on popout with op stamp — residual stamped rebind recovers it.
            conn.owner_window_label = "conversation-13".into();
            conn.owner_operation_id = Some("op-order".into());
            conn.ownership_generation = 5;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-13".into();
            st.conversation_id = None;
            st.status = crate::acp::types::ConnectionStatus::Prompting;
        }

        // Forced primary reverse (by conversation_id) would miss → ConnectionGone.
        let forced = abort_outcome_for_close_reserved_forced_reverse(
            None,
            Some("no connection for conversation"),
            5,
        );
        assert_eq!(forced, AbortOutcome::ConnectionGone);

        // Production order: residual WHILE rebind_in_flight still true.
        assert!(state.is_rebind_in_flight("op-order"));
        let residual_gen =
            residual_reconcile_after_close(&cm, None, "conversation-13", "op-order").await;
        assert!(
            residual_gen.is_some(),
            "residual rebound_count>0 must return max_gen"
        );
        assert!(
            state.is_rebind_in_flight("op-order"),
            "rebind_in_flight must still be true after residual (before commit) \
             so close cannot observe Done(ConnectionGone) mid-race"
        );

        let outcome =
            close_reserved_outcome_after_residual(forced, residual_gen);
        assert_eq!(
            outcome,
            AbortOutcome::Reversed {
                generation: residual_gen.unwrap()
            }
        );
        let committed = state
            .commit_close_reverse("op-order", outcome)
            .unwrap();
        assert_eq!(
            committed,
            AbortOutcome::Reversed {
                generation: residual_gen.unwrap()
            }
        );
        assert!(!state.is_rebind_in_flight("op-order"));

        // Close now sees reclaimable Reversed — never stale ConnectionGone.
        match state.decide_close("op-order").unwrap() {
            CloseDecision::Done {
                outcome: AbortOutcome::Reversed { generation },
                ..
            } => assert_eq!(generation, residual_gen.unwrap()),
            other => panic!("expected Done(Reversed), got {other:?}"),
        }
        {
            let map = cm.connections.lock().await;
            let conn = map.get("order-stamped").expect("survives");
            assert_eq!(conn.owner_window_label, "main");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-order"));
        }
    }

    /// Close emit harden: snapshot ConnectionGone but residual already left
    /// ownership on main with matching op → upgrade before publish.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn upgrade_connection_gone_before_emit_uses_main_residual() {
        use crate::acp::manager::ConnectionManager;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let state = ConversationPopoutState::new();
        state
            .insert_opened(14, "op-harden".into(), "conversation-14".into())
            .unwrap();
        state.admit_forward_rebind("op-harden").unwrap();
        state.record_rebind("op-harden", 2).unwrap();
        state
            .commit_close_reverse("op-harden", AbortOutcome::ConnectionGone)
            .unwrap();

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "already-on-main",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("already-on-main").unwrap();
            conn.owner_window_label = "main".into();
            conn.owner_operation_id = Some("op-harden".into());
            conn.ownership_generation = 8;
        }

        let upgraded = upgrade_connection_gone_before_emit(
            &state,
            Some(&cm),
            "op-harden",
            AbortOutcome::ConnectionGone,
        )
        .await;
        assert_eq!(
            upgraded,
            AbortOutcome::Reversed { generation: 8 },
            "must upgrade ConnectionGone when residual already on main"
        );
        assert_eq!(
            state.get_status("op-harden").unwrap().abort_outcome,
            Some(AbortOutcome::Reversed { generation: 8 })
        );
    }

    /// Close emit harden: live status already Reversed wins over stale snapshot.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn upgrade_connection_gone_before_emit_prefers_live_reversed_status() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(15, "op-live".into(), "conversation-15".into())
            .unwrap();
        state.admit_forward_rebind("op-live").unwrap();
        state.record_rebind("op-live", 1).unwrap();
        state
            .commit_close_reverse("op-live", AbortOutcome::ConnectionGone)
            .unwrap();
        // Racing residual path upgraded after close snapshotted ConnectionGone.
        state
            .commit_close_reverse("op-live", AbortOutcome::Reversed { generation: 11 })
            .unwrap();

        let upgraded = upgrade_connection_gone_before_emit(
            &state,
            None,
            "op-live",
            AbortOutcome::ConnectionGone,
        )
        .await;
        assert_eq!(
            upgraded,
            AbortOutcome::Reversed { generation: 11 },
            "stale ConnectionGone snapshot must yield to live Reversed status"
        );
    }

    /// Close-fence late connect (spawn finished after fence / inflight wait)
    /// must use reverse-to-main + idle residual — never hard-kill busy.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn close_fence_late_connect_reconcile_keeps_busy_reverses_to_main() {
        use crate::acp::manager::ConnectionManager;
        use crate::acp::types::ConnectionStatus;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "late-spawn-busy",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("late-spawn-busy").unwrap();
            conn.owner_window_label = "conversation-3".into();
            conn.owner_operation_id = Some("op-late".into());
            conn.ownership_generation = 1;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-3".into();
            st.status = ConnectionStatus::Prompting;
        }

        // Same helper acp_connect uses after close fence instead of disconnect_if_owner.
        close_fence_late_connect_reconcile(&cm, None, "conversation-3", "op-late").await;

        let map = cm.connections.lock().await;
        let conn = map
            .get("late-spawn-busy")
            .expect("busy late connect must not be hard-killed");
        assert_eq!(conn.owner_window_label, "main");
        assert_eq!(conn.owner_operation_id.as_deref(), Some("op-late"));
        let st = conn.state.try_read().unwrap();
        assert_eq!(st.status, ConnectionStatus::Prompting);
    }

    /// Close-fence late connect for idle: reverse-to-main first (Route A).
    /// After reverse, connection is main-owned (idle sweep / later residual);
    /// must not remain stamped on the closed pop-out label.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn close_fence_late_connect_reconcile_moves_idle_off_closed_label() {
        use crate::acp::manager::ConnectionManager;
        use crate::models::agent::AgentType;
        use crate::web::event_bridge::EventEmitter;

        let cm = ConnectionManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "late-spawn-idle",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("late-spawn-idle").unwrap();
            conn.owner_window_label = "conversation-3".into();
            conn.owner_operation_id = Some("op-idle".into());
            conn.ownership_generation = 1;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-3".into();
            st.status = crate::acp::types::ConnectionStatus::Connected;
        }

        close_fence_late_connect_reconcile(&cm, None, "conversation-3", "op-idle").await;

        let map = cm.connections.lock().await;
        match map.get("late-spawn-idle") {
            None => {
                // Idle residual reaped a leftover that reverse could not move
                // (unusual after stamped rebind, but valid).
            }
            Some(conn) => {
                assert_eq!(
                    conn.owner_window_label, "main",
                    "idle late connect must leave the closed label via reverse"
                );
                assert_ne!(
                    conn.owner_window_label, "conversation-3",
                    "must not remain owned by closed pop-out"
                );
            }
        }
    }

    /// Barrier (R5 Critical): forward rebind already moved ownership, but
    /// `record_rebind` loses to the close fence. While the forced reverse is
    /// still pending, close's `decide_abort` must not commit `NeverRebound`.
    /// After reverse succeeds, abort must atomically publish
    /// `Reversed { post_reverse_gen }` so main can refresh its lease even
    /// without ready/release (prior lease still held on main).
    #[test]
    fn close_reserved_forced_reverse_pending_not_never_rebound() {
        let state = ConversationPopoutState::new();
        // Recovered prior handoff context: main still holds a live lease while
        // op-B forward-rebinds (gen-10). Close races before record_rebind.
        state
            .insert_opened(1, "op-B".into(), "conversation-1".into())
            .unwrap();
        state.admit_forward_rebind("op-B").unwrap();
        assert!(state.is_rebind_in_flight("op-B"));

        // Close fence + abort reservation while ownership mid-flight.
        assert!(state.reserve_close_operation("op-B"));
        state.reserve_abort_for_close("op-B").unwrap();

        // Forward rebind succeeded on the connection; record loses to close.
        let err = state.record_rebind("op-B", 10).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("closed") || msg.contains("abort is reserved"),
            "expected close/abort rejection, got {msg}"
        );

        // Must keep forward gen and/or reverse-in-flight fencing so close
        // cannot decide NeverRebound while forced reverse is pending.
        let status = state.get_status("op-B").unwrap();
        assert_eq!(
            status.ownership_generation,
            Some(10),
            "forward gen must remain visible until reverse commits"
        );
        match state.decide_abort("op-B") {
            Err(e) => {
                // Waiting on reverse/rebind fence — acceptable.
                assert!(
                    e.to_string().contains("rebind is in flight")
                        || e.to_string().contains("reverse"),
                    "unexpected decide_abort err: {e}"
                );
            }
            Ok(AbortDecision::NeedReverse { generation, .. }) => {
                assert_eq!(generation, 10, "NeedReverse must use forward gen");
            }
            Ok(AbortDecision::Done {
                outcome: AbortOutcome::NeverRebound,
                ..
            }) => panic!(
                "close must not commit NeverRebound while forced reverse is pending \
                 (would orphan agent: backend main/new-gen, FE stale lease)"
            ),
            Ok(other) => panic!("unexpected decide_abort decision: {other:?}"),
        }

        // Forced reverse succeeds → post-reverse gen 11 on main. Commit
        // Reversed atomically (even while reverse/rebind fence still set).
        let outcome = state
            .abort_after_forced_reverse(
                "op-B",
                AbortOutcome::Reversed { generation: 11 },
            )
            .unwrap();
        assert_eq!(outcome, AbortOutcome::Reversed { generation: 11 });

        let status = state.get_status("op-B").unwrap();
        assert_eq!(status.phase, PopoutPhase::Aborted);
        assert_eq!(
            status.ownership_generation,
            Some(11),
            "post-reverse gen must be stamped before close can re-decide"
        );
        assert_eq!(
            status.abort_outcome,
            Some(AbortOutcome::Reversed { generation: 11 })
        );
        assert!(
            !state.is_rebind_in_flight("op-B"),
            "in-flight fence must clear on forced-reverse commit"
        );

        // Later close decide / abort must surface Reversed, never NeverRebound.
        match state.decide_abort("op-B").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::Reversed { generation: 11 },
                ..
            } => {}
            other => panic!("expected Done(Reversed{{11}}), got {other:?}"),
        }
        // Idempotent: cannot overwrite Reversed with NeverRebound.
        let again = state
            .abort("op-B", |_| AbortOutcome::NeverRebound)
            .unwrap();
        assert_eq!(again, AbortOutcome::Reversed { generation: 11 });
    }

    /// Reverse not-found on the close-reserved forced-reverse path commits
    /// ConnectionGone (not reclaimable NeverRebound / Reversed).
    #[test]
    fn close_reserved_forced_reverse_commit_connection_gone() {
        let state = ConversationPopoutState::new();
        state
            .insert_opened(2, "op-gone".into(), "conversation-2".into())
            .unwrap();
        state.admit_forward_rebind("op-gone").unwrap();
        assert!(state.reserve_close_operation("op-gone"));
        state.record_rebind("op-gone", 7).unwrap_err();
        let outcome = state
            .abort_after_forced_reverse("op-gone", AbortOutcome::ConnectionGone)
            .unwrap();
        assert_eq!(outcome, AbortOutcome::ConnectionGone);
        match state.decide_abort("op-gone").unwrap() {
            AbortDecision::Done {
                outcome: AbortOutcome::ConnectionGone,
                ..
            } => {}
            other => panic!("expected ConnectionGone, got {other:?}"),
        }
    }

    // --- Task 3: terminal rebind on residual (no kill) ---

    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn residual_reconcile_does_not_call_kill() {
        use crate::acp::manager::ConnectionManager;
        use crate::terminal::manager::TerminalManager;

        let cm = ConnectionManager::new();
        let tm = TerminalManager::new();
        tm.insert_test_terminal("term-residual", "conversation-1", Some("op-1"));

        residual_reconcile_after_close(&cm, Some(&tm), "conversation-1", "op-1").await;

        assert!(
            tm.contains_for_test("term-residual"),
            "close residual must rebind terminals, never kill them"
        );
        assert_eq!(
            tm.owner_window_label_for_test("term-residual").as_deref(),
            Some("main"),
            "matching terminal must rebind to main"
        );
    }

    /// Late close-reserved residual (same shared helper) rebinds stamped
    /// terminals to main without kill_by_owner_window_and_operation.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn close_reserved_late_rebind_rebinds_stamped_terminal_to_main() {
        use crate::acp::manager::ConnectionManager;
        use crate::terminal::manager::TerminalManager;

        let cm = ConnectionManager::new();
        let tm = TerminalManager::new();
        // Terminal still owned by the closed conversation window + incarnation.
        tm.insert_test_terminal("term-late", "conversation-3", Some("op-late"));
        // Mismatch must stay on the child label.
        tm.insert_test_terminal("term-other", "conversation-3", Some("op-other"));

        // Shared residual used by late record_rebind close-reserved path.
        residual_reconcile_after_close(&cm, Some(&tm), "conversation-3", "op-late").await;

        assert!(tm.contains_for_test("term-late"), "must not kill");
        assert_eq!(
            tm.owner_window_label_for_test("term-late").as_deref(),
            Some("main")
        );
        assert_eq!(
            tm.owner_window_label_for_test("term-other").as_deref(),
            Some("conversation-3"),
            "other op must not rebind"
        );
        assert!(tm.contains_for_test("term-other"));
    }

    /// Spec test 12: busy leftover still stamped (label, op) survives idle residual.
    #[cfg(feature = "tauri-runtime")]
    #[tokio::test]
    async fn late_record_rebind_busy_connection_survives_idle_residual() {
        use crate::acp::manager::ConnectionManager;
        use crate::acp::types::ConnectionStatus;
        use crate::models::agent::AgentType;
        use crate::terminal::manager::TerminalManager;
        use crate::web::event_bridge::EventEmitter;

        let cm = ConnectionManager::new();
        let tm = TerminalManager::new();
        let _rx = cm
            .insert_test_connection_live(
                "busy-leftover",
                AgentType::ClaudeCode,
                None,
                EventEmitter::Noop,
            )
            .await;
        {
            let mut map = cm.connections.lock().await;
            let conn = map.get_mut("busy-leftover").unwrap();
            conn.owner_window_label = "conversation-1".into();
            conn.owner_operation_id = Some("op-busy".into());
            conn.ownership_generation = 3;
            let mut st = conn.state.try_write().unwrap();
            st.owner_window_label = "conversation-1".into();
            st.status = ConnectionStatus::Prompting;
        }

        tm.insert_test_terminal("term-busy-path", "conversation-1", Some("op-busy"));

        residual_reconcile_after_close(&cm, Some(&tm), "conversation-1", "op-busy").await;

        // Busy connection lives: stamped rebind moves ownership to main; idle
        // disconnect never removes a Prompting process.
        {
            let map = cm.connections.lock().await;
            let conn = map
                .get("busy-leftover")
                .expect("busy connection must survive idle residual");
            assert_eq!(conn.owner_window_label, "main");
            assert_eq!(conn.owner_operation_id.as_deref(), Some("op-busy"));
            let st = conn.state.try_read().unwrap();
            assert_eq!(st.status, ConnectionStatus::Prompting);
        }
        assert!(
            tm.contains_for_test("term-busy-path"),
            "terminal rebind path must not kill"
        );
        assert_eq!(
            tm.owner_window_label_for_test("term-busy-path").as_deref(),
            Some("main")
        );
    }
}
