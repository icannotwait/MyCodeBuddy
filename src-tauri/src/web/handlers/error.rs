use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use crate::app_error::{AppCommandError, AppErrorCode};

fn status_for_app_error_code(code: AppErrorCode) -> StatusCode {
    match code {
        AppErrorCode::InvalidInput
        | AppErrorCode::TerminalShellUnavailable
        | AppErrorCode::TerminalShellUnsupported => StatusCode::BAD_REQUEST,
        AppErrorCode::NotFound => StatusCode::NOT_FOUND,
        AppErrorCode::AlreadyExists | AppErrorCode::TurnInProgress => StatusCode::CONFLICT,
        AppErrorCode::PermissionDenied => StatusCode::FORBIDDEN,
        AppErrorCode::ConfigurationMissing
        | AppErrorCode::ConfigurationInvalid
        | AppErrorCode::DependencyMissing
        | AppErrorCode::NotAGitRepository
        | AppErrorCode::AuthenticationFailed => StatusCode::UNPROCESSABLE_ENTITY,
        AppErrorCode::NetworkError
        | AppErrorCode::DatabaseError
        | AppErrorCode::IoError
        | AppErrorCode::ExternalCommandFailed
        | AppErrorCode::WindowOperationFailed
        | AppErrorCode::TaskExecutionFailed => StatusCode::INTERNAL_SERVER_ERROR,
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
}
