use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::app_error::{AppCommandError, AppErrorCode};

fn status_for_app_error_code(code: AppErrorCode) -> StatusCode {
    match code {
        AppErrorCode::InvalidInput
        | AppErrorCode::CompletionProtocolConfigurationRemoved
        | AppErrorCode::TerminalShellUnavailable
        | AppErrorCode::TerminalShellUnsupported
        | AppErrorCode::RouteUnavailable
        | AppErrorCode::InvalidSharedSessionField
        | AppErrorCode::InvalidPattern
        | AppErrorCode::InvalidRequest => StatusCode::BAD_REQUEST,
        AppErrorCode::ServerShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
        AppErrorCode::NotFound => StatusCode::NOT_FOUND,
        AppErrorCode::AlreadyExists
        | AppErrorCode::TurnInProgress
        | AppErrorCode::ConversationWaitingForSubagents
        | AppErrorCode::DelegateViewerOnly
        | AppErrorCode::LegacyCompletionProtocolReadOnly
        | AppErrorCode::WorkflowV2Retired
        | AppErrorCode::WorkflowIdentityCorrupt
        | AppErrorCode::SimpleSuccessorCreationRetired
        | AppErrorCode::UnsupportedCompletionProtocol
        | AppErrorCode::CompletionInstructionBindingFailed
        | AppErrorCode::SessionRouteConflict
        | AppErrorCode::SharedSessionConfigConflict
        | AppErrorCode::SharedSessionProtocolRequired
        | AppErrorCode::SharedSessionGenerationStale
        | AppErrorCode::SharedSessionClosing
        | AppErrorCode::SharedSessionCleanupInProgress
        | AppErrorCode::ClientLeaseMissing
        | AppErrorCode::IdempotencyKeyConflict
        | AppErrorCode::QueueItemNotFound
        | AppErrorCode::QueueItemAlreadyDispatching
        | AppErrorCode::InteractionAlreadyResolved
        | AppErrorCode::StaleTurn
        | AppErrorCode::SessionUnavailable
        | AppErrorCode::CompanionInitializationFailed
        | AppErrorCode::SharedSessionConversationKeyConflict
        | AppErrorCode::Cancelled
        | AppErrorCode::StaleStart
        | AppErrorCode::StalePage
        | AppErrorCode::LimitEpochChanged
        | AppErrorCode::SourceEpochChanged => StatusCode::CONFLICT,
        AppErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
        AppErrorCode::ConfigurationMissing
        | AppErrorCode::ConfigurationInvalid
        | AppErrorCode::DependencyMissing
        | AppErrorCode::NotAGitRepository
        | AppErrorCode::AuthenticationFailed => StatusCode::UNPROCESSABLE_ENTITY,
        AppErrorCode::JobExpired | AppErrorCode::ClientLeaseExpired => StatusCode::GONE,
        AppErrorCode::SourceTimeout => StatusCode::REQUEST_TIMEOUT,
        AppErrorCode::RegistryOverloaded
        | AppErrorCode::ClientLeaseCapacityExceeded
        | AppErrorCode::ConnectIdempotencyCapacityExceeded
        | AppErrorCode::PromptIdempotencyCapacityExceeded
        | AppErrorCode::PromptQueueFull => StatusCode::TOO_MANY_REQUESTS,
        AppErrorCode::NetworkError
        | AppErrorCode::DatabaseError
        | AppErrorCode::IoError
        | AppErrorCode::ExternalCommandFailed
        | AppErrorCode::WindowOperationFailed
        | AppErrorCode::TaskExecutionFailed
        | AppErrorCode::SourceFailed => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl IntoResponse for AppCommandError {
    fn into_response(self) -> Response {
        let status = status_for_app_error_code(self.code);
        (status, Json(self)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_authentication_failures_are_not_web_session_unauthorized() {
        assert_eq!(
            status_for_app_error_code(AppErrorCode::AuthenticationFailed),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn acp_error_serialization_maps_shell_codes_to_http_400() {
        assert_eq!(
            status_for_app_error_code(AppErrorCode::TerminalShellUnavailable),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_app_error_code(AppErrorCode::TerminalShellUnsupported),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn continuation_gate_waiting_error_maps_to_http_409() {
        assert_eq!(
            status_for_app_error_code(AppErrorCode::ConversationWaitingForSubagents),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn stable_completion_protocol_errors_map_to_expected_http_status() {
        for code in [
            AppErrorCode::LegacyCompletionProtocolReadOnly,
            AppErrorCode::WorkflowV2Retired,
            AppErrorCode::WorkflowIdentityCorrupt,
            AppErrorCode::SimpleSuccessorCreationRetired,
            AppErrorCode::UnsupportedCompletionProtocol,
            AppErrorCode::CompletionInstructionBindingFailed,
        ] {
            assert_eq!(status_for_app_error_code(code), StatusCode::CONFLICT);
        }
        assert_eq!(
            status_for_app_error_code(AppErrorCode::CompletionProtocolConfigurationRemoved),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn shared_session_errors_map_to_stable_http_statuses() {
        assert_eq!(
            status_for_app_error_code(AppErrorCode::InvalidSharedSessionField),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status_for_app_error_code(AppErrorCode::ClientLeaseMissing),
            StatusCode::CONFLICT
        );
        assert_eq!(
            status_for_app_error_code(AppErrorCode::ClientLeaseExpired),
            StatusCode::GONE
        );
        for code in [
            AppErrorCode::ClientLeaseCapacityExceeded,
            AppErrorCode::ConnectIdempotencyCapacityExceeded,
            AppErrorCode::PromptIdempotencyCapacityExceeded,
            AppErrorCode::PromptQueueFull,
        ] {
            assert_eq!(
                status_for_app_error_code(code),
                StatusCode::TOO_MANY_REQUESTS
            );
        }
        for code in [
            AppErrorCode::SharedSessionConfigConflict,
            AppErrorCode::SharedSessionProtocolRequired,
            AppErrorCode::SharedSessionGenerationStale,
            AppErrorCode::SharedSessionClosing,
            AppErrorCode::SharedSessionCleanupInProgress,
            AppErrorCode::IdempotencyKeyConflict,
            AppErrorCode::QueueItemNotFound,
            AppErrorCode::QueueItemAlreadyDispatching,
            AppErrorCode::InteractionAlreadyResolved,
            AppErrorCode::StaleTurn,
            AppErrorCode::SessionUnavailable,
            AppErrorCode::CompanionInitializationFailed,
            AppErrorCode::SharedSessionConversationKeyConflict,
        ] {
            assert_eq!(status_for_app_error_code(code), StatusCode::CONFLICT);
        }
    }
}
