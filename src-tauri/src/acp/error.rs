use std::collections::BTreeMap;

use serde::Serialize;

use crate::acp::delegation::continuation::types::ContinuationState;
use crate::acp::delegation::route::RouteDegradedReason;
use crate::acp::delegation::workflow::{
    CompletionProtocolConfigurationRemoved, WorkflowStoreError,
};
use crate::app_error::{AppCommandError, AppErrorCode};
use crate::terminal::shell::ShellResolveError;

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("agent process failed to spawn: {0}")]
    SpawnFailed(String),
    #[error("connection not found: {0}")]
    ConnectionNotFound(String),
    #[error("ACP protocol error: {0}")]
    Protocol(String),
    #[error("selected terminal shell is unavailable: {display_name} ({executable})")]
    TerminalShellUnavailable {
        display_name: String,
        executable: String,
    },
    #[error("selected terminal shell is unsupported: {display_name} ({executable})")]
    TerminalShellUnsupported {
        display_name: String,
        executable: String,
    },
    /// Managed Codeg route could not be established (child never falls back).
    #[error("delegation route unavailable: {reason:?}")]
    RouteUnavailable { reason: RouteDegradedReason },
    /// Session-id reuse found an existing connection with an incompatible route.
    #[error("session route conflict with connection {existing_connection_id}")]
    SessionRouteConflict { existing_connection_id: String },
    #[error("agent process exited unexpectedly")]
    ProcessExited,
    /// A prompt arrived while this connection already had a turn in flight.
    /// The connection loop processes one turn at a time; a second concurrent
    /// prompt (e.g. two co-controlling clients sending near-simultaneously)
    /// is rejected here rather than silently dropped after a false success.
    /// The frontend recognizes this (via the stable Display text, carried as
    /// the error message on both transports) and re-queues the draft in the
    /// message queue above the input box instead of surfacing an error.
    #[error("turn already in progress for this connection")]
    TurnInProgress,
    #[error("conversation {conversation_id} is waiting for subagents ({})", state.as_str())]
    ContinuationInProgress {
        conversation_id: i32,
        state: ContinuationState,
    },
    /// Live feedback was submitted while no turn was in flight. Feedback only
    /// makes sense while the agent is working (it is pulled mid-turn via the
    /// `check_user_feedback` MCP tool); with no active turn there is nothing to
    /// steer. The frontend recognizes this (stable Display text) and falls back
    /// to sending the text as an ordinary prompt instead.
    #[error("no active turn to send feedback to")]
    NoActiveTurn,
    /// Live feedback was submitted while the feature is disabled. The settings
    /// toggle gates both MCP tool injection and the UI affordance; this is the
    /// backend's defense-in-depth for a direct/stale call.
    #[error("live feedback is disabled")]
    FeedbackDisabled,
    /// The submitted feedback note is empty or exceeds the per-note size bound.
    /// The full text rides in the broadcast event + snapshot + MCP response, so
    /// a sanity bound keeps a single pathological note from bloating them.
    #[error("invalid feedback: {0}")]
    InvalidFeedback(String),
    #[error("binary download failed: {0}")]
    DownloadFailed(String),
    #[error("platform not supported: {0}")]
    PlatformNotSupported(String),
    #[error("{0}")]
    SdkNotInstalled(String),
    #[error("Agent did not respond to Initialize within 60 seconds. The cached binary may be outdated or incompatible. Try upgrading it from Agent Settings.")]
    InitializeTimeout,
    #[error("Agent did not publish its configurable options within 60 seconds. The probe was aborted; the agent may be slow, idle, or not ACP-compliant — try again or check the agent binary.")]
    ProbeTimedOut,
    /// A user-entry mutation targeted a delegated child that is currently
    /// viewer-only (task running, parent turn active, or fail-closed unknown).
    #[error("delegated conversation is viewer-only: {reason:?}")]
    DelegateViewerOnly {
        reason: crate::models::DelegateAccessReason,
    },
    #[error("legacy completion protocol restart required; successor conversation {successor_conversation_id}")]
    LegacyCompletionProtocolRestart { successor_conversation_id: i32 },
    #[error("legacy completion protocol is read-only")]
    LegacyCompletionProtocolReadOnly,
    #[error("unsupported completion protocol: {0}")]
    UnsupportedCompletionProtocol(String),
    #[error("completion instruction binding failed: {0}")]
    CompletionInstructionBindingFailed(String),
    #[error("completion protocol configuration was removed: {variable}")]
    CompletionProtocolConfigurationRemoved { variable: &'static str },
}

impl AcpError {
    pub fn protocol(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let sanitized = sanitize_protocol_message(&raw);

        if is_executable_format_error(&sanitized) {
            return Self::Protocol(
                "Agent executable appears incompatible or corrupted. Please retry to re-download it."
                    .into(),
            );
        }

        Self::Protocol(sanitized)
    }

    /// Stable machine-readable identifier for this error kind.
    ///
    /// Returned to the frontend alongside the human-readable message so
    /// the UI can render a localized message based on the code instead
    /// of parsing English text. `None` means "no stable code — show the
    /// raw message as a fallback".
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::SdkNotInstalled(_) => Some("sdk_not_installed"),
            Self::PlatformNotSupported(_) => Some("platform_not_supported"),
            Self::InitializeTimeout => Some("initialize_timeout"),
            Self::ProbeTimedOut => Some("probe_timed_out"),
            Self::ProcessExited => Some("process_exited"),
            Self::TurnInProgress => Some("turn_in_progress"),
            Self::ContinuationInProgress { .. } => Some("conversation_waiting_for_subagents"),
            Self::NoActiveTurn => Some("no_active_turn"),
            Self::FeedbackDisabled => Some("feedback_disabled"),
            Self::InvalidFeedback(_) => Some("invalid_feedback"),
            Self::SpawnFailed(_) => Some("spawn_failed"),
            Self::DownloadFailed(_) => Some("download_failed"),
            Self::ConnectionNotFound(_) => Some("connection_not_found"),
            Self::TerminalShellUnavailable { .. } => Some("terminal_shell_unavailable"),
            Self::TerminalShellUnsupported { .. } => Some("terminal_shell_unsupported"),
            Self::RouteUnavailable { .. } => Some("route_unavailable"),
            Self::SessionRouteConflict { .. } => Some("session_route_conflict"),
            Self::DelegateViewerOnly { .. } => Some("delegate_viewer_only"),
            Self::LegacyCompletionProtocolRestart { .. } => {
                Some("legacy_completion_protocol_restart_required")
            }
            Self::LegacyCompletionProtocolReadOnly => Some("legacy_completion_protocol_read_only"),
            Self::UnsupportedCompletionProtocol(_) => Some("unsupported_completion_protocol"),
            Self::CompletionInstructionBindingFailed(_) => {
                Some("completion_instruction_binding_failed")
            }
            Self::CompletionProtocolConfigurationRemoved { .. } => {
                Some("completion_protocol_configuration_removed")
            }
            Self::Protocol(_) => None,
        }
    }

    /// Structured wire payload for command boundary failures.
    ///
    /// Other variants return `None` so Tauri serialization stays a bare
    /// legacy string (preserving SdkNotInstalled substring matching, etc.).
    pub(crate) fn app_command_error(&self) -> Option<AppCommandError> {
        match self {
            AcpError::TurnInProgress => Some(AppCommandError::new(
                AppErrorCode::TurnInProgress,
                "turn already in progress for this connection",
            )),
            AcpError::TerminalShellUnavailable {
                display_name,
                executable,
            } => Some(
                AppCommandError::new(
                    AppErrorCode::TerminalShellUnavailable,
                    "Selected terminal shell is unavailable",
                )
                .with_detail(executable.clone())
                .with_i18n(
                    "backendErrors.terminalShellUnavailable",
                    BTreeMap::from([("shell".into(), display_name.clone())]),
                ),
            ),
            AcpError::TerminalShellUnsupported {
                display_name,
                executable,
            } => Some(
                AppCommandError::new(
                    AppErrorCode::TerminalShellUnsupported,
                    "Selected terminal shell is unsupported",
                )
                .with_detail(executable.clone())
                .with_i18n(
                    "backendErrors.terminalShellUnsupported",
                    BTreeMap::from([("shell".into(), display_name.clone())]),
                ),
            ),
            AcpError::RouteUnavailable { reason } => Some(
                AppCommandError::new(
                    AppErrorCode::RouteUnavailable,
                    "Delegation route unavailable",
                )
                .with_detail(format!("{reason:?}")),
            ),
            AcpError::SessionRouteConflict {
                existing_connection_id,
            } => Some(
                AppCommandError::new(
                    AppErrorCode::SessionRouteConflict,
                    "Session route conflict with an existing connection",
                )
                .with_detail(existing_connection_id.clone()),
            ),
            AcpError::ContinuationInProgress {
                conversation_id,
                state,
            } => Some(
                AppCommandError::new(
                    AppErrorCode::ConversationWaitingForSubagents,
                    "Conversation is waiting for subagents",
                )
                .with_i18n(
                    "backendErrors.conversationWaitingForSubagents",
                    BTreeMap::from([
                        ("conversationId".into(), conversation_id.to_string()),
                        ("state".into(), state.as_str().to_string()),
                    ]),
                ),
            ),
            AcpError::DelegateViewerOnly { reason } => Some(
                AppCommandError::new(
                    AppErrorCode::DelegateViewerOnly,
                    "Delegated conversation is read-only",
                )
                .with_detail(reason.as_str())
                .with_i18n(
                    "backendErrors.delegateViewerOnly",
                    BTreeMap::from([("reason".into(), reason.as_str().into())]),
                ),
            ),
            AcpError::LegacyCompletionProtocolRestart {
                successor_conversation_id,
            } => Some(
                AppCommandError::new(
                    AppErrorCode::LegacyCompletionProtocolRestartRequired,
                    "Legacy workflow restarted in a fresh conversation",
                )
                .with_detail(successor_conversation_id.to_string()),
            ),
            AcpError::LegacyCompletionProtocolReadOnly => Some(AppCommandError::new(
                AppErrorCode::LegacyCompletionProtocolReadOnly,
                "Legacy completion protocol is read-only",
            )),
            AcpError::UnsupportedCompletionProtocol(detail) => Some(
                AppCommandError::new(
                    AppErrorCode::UnsupportedCompletionProtocol,
                    "Unsupported completion protocol",
                )
                .with_detail(detail.clone()),
            ),
            AcpError::CompletionInstructionBindingFailed(detail) => Some(
                AppCommandError::new(
                    AppErrorCode::CompletionInstructionBindingFailed,
                    "Completion instruction binding failed",
                )
                .with_detail(detail.clone()),
            ),
            AcpError::CompletionProtocolConfigurationRemoved { variable } => Some(
                AppCommandError::new(
                    AppErrorCode::CompletionProtocolConfigurationRemoved,
                    "Completion protocol configuration was removed",
                )
                .with_detail(*variable),
            ),
            _ => None,
        }
    }
}

impl From<WorkflowStoreError> for AcpError {
    fn from(error: WorkflowStoreError) -> Self {
        let message = error.to_string();
        match error {
            WorkflowStoreError::LegacyCompletionProtocolReadOnly => {
                Self::LegacyCompletionProtocolReadOnly
            }
            WorkflowStoreError::UnsupportedCompletionProtocol { .. }
            | WorkflowStoreError::UnsupportedCompletionProtocolHeader(_) => {
                Self::UnsupportedCompletionProtocol(message)
            }
            _ => Self::Protocol(message),
        }
    }
}

impl From<CompletionProtocolConfigurationRemoved> for AcpError {
    fn from(error: CompletionProtocolConfigurationRemoved) -> Self {
        Self::CompletionProtocolConfigurationRemoved {
            variable: error.variable,
        }
    }
}

impl From<ShellResolveError> for AcpError {
    fn from(err: ShellResolveError) -> Self {
        match err {
            ShellResolveError::Unavailable {
                display_name,
                executable,
            } => Self::TerminalShellUnavailable {
                display_name,
                executable,
            },
            ShellResolveError::Unsupported {
                display_name,
                executable,
            } => Self::TerminalShellUnsupported {
                display_name,
                executable,
            },
        }
    }
}

impl Serialize for AcpError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if let Some(error) = self.app_command_error() {
            return error.serialize(serializer);
        }
        serializer.serialize_str(&self.to_string())
    }
}

fn sanitize_protocol_message(raw: &str) -> String {
    let without_spawned_at = regex::Regex::new(r#"\s*,?\s*"spawned_at"\s*:\s*"[^"]*"\s*,?"#)
        .ok()
        .map(|re| re.replace_all(raw, "").into_owned())
        .unwrap_or_else(|| raw.to_string());

    let without_dangling_comma = regex::Regex::new(r#",\s*([}\]])"#)
        .ok()
        .map(|re| re.replace_all(&without_spawned_at, "$1").into_owned())
        .unwrap_or(without_spawned_at);

    regex::Regex::new(r#"/(?:Users|home)/[^"\s]+"#)
        .ok()
        .map(|re| {
            re.replace_all(&without_dangling_comma, "<local-path>")
                .into_owned()
        })
        .unwrap_or(without_dangling_comma)
}

fn is_executable_format_error(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("malformed mach-o file")
        || lowered.contains("exec format error")
        || lowered.contains("bad cpu type in executable")
        || lowered.contains("not a valid win32 application")
        || lowered.contains("is not a valid application for this os platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acp_error_serialization_structures_shell_failure() {
        let err = AcpError::TerminalShellUnavailable {
            display_name: "PowerShell 7".into(),
            executable: r"C:\missing\pwsh.exe".into(),
        };
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(
            value,
            json!({
                "code": "terminal_shell_unavailable",
                "message": "Selected terminal shell is unavailable",
                "detail": r"C:\missing\pwsh.exe",
                "i18n_key": "backendErrors.terminalShellUnavailable",
                "i18n_params": { "shell": "PowerShell 7" },
            })
        );

        let unsupported = AcpError::TerminalShellUnsupported {
            display_name: "mystery.exe".into(),
            executable: r"C:\tools\mystery.exe".into(),
        };
        let value = serde_json::to_value(&unsupported).expect("serialize");
        assert_eq!(value["code"], "terminal_shell_unsupported");
        assert_eq!(value["i18n_key"], "backendErrors.terminalShellUnsupported");
    }

    #[test]
    fn acp_error_serialization_preserves_sdk_string() {
        let err = AcpError::SdkNotInstalled("agent is not installed".into());
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(value, json!("agent is not installed"));
    }

    #[test]
    fn continuation_gate_error_serializes_stable_waiting_fields() {
        let err = AcpError::ContinuationInProgress {
            conversation_id: 42,
            state: crate::acp::delegation::continuation::types::ContinuationState::Arming,
        };
        let value = serde_json::to_value(&err).expect("serialize");
        assert_eq!(
            value,
            json!({
                "code": "conversation_waiting_for_subagents",
                "message": "Conversation is waiting for subagents",
                "i18n_key": "backendErrors.conversationWaitingForSubagents",
                "i18n_params": { "conversationId": "42", "state": "arming" },
            })
        );
    }

    #[test]
    fn acp_error_serialization_preserves_turn_in_progress_http_contract() {
        let value = serde_json::to_value(&AcpError::TurnInProgress).expect("serialize");
        assert_eq!(
            value,
            json!({
                "code": "turn_in_progress",
                "message": "turn already in progress for this connection",
            })
        );
    }

    #[test]
    fn delegate_viewer_only_serializes_reason_and_stable_code() {
        let value = serde_json::to_value(&AcpError::DelegateViewerOnly {
            reason: crate::models::DelegateAccessReason::ParentTurnActive,
        })
        .unwrap();
        assert_eq!(
            value,
            json!({
                "code": "delegate_viewer_only",
                "message": "Delegated conversation is read-only",
                "detail": "parent_turn_active",
                "i18n_key": "backendErrors.delegateViewerOnly",
                "i18n_params": { "reason": "parent_turn_active" },
            })
        );
    }
}
