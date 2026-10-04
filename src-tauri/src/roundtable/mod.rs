//! Roundtable isolation boundary.
//!
//! The persistent execution gate defaults to off. This module does not migrate
//! ordinary session rows and does not claim a live OS sandbox on hosts that
//! cannot prove one.

mod feature_gate;
mod gateway;
mod qualification;
mod relay;
mod request_accounting;
mod runtime;
mod sandbox;

pub use feature_gate::{
    scopes_convert, AdmissionFacts, ExecutionGate, ExecutionPolicy, ExecutionScope, GatePermit,
};
pub use gateway::{
    ApprovedOrigin, ClientPolicy, GatewayHandle, GatewayLease, HoldRelease, HostCredential,
    HostModelGateway, ModelRequest, ModelResponse, ObservedForward,
};
pub use qualification::{
    evaluate_certificate, CertifiedBinary, OsIdentity, QualificationKey, QualificationReport,
};
pub use relay::{
    forward_to_caller_target, probe_instance_socket, relay_from_helper, InstanceSocket,
    LoopbackRelay, SocketProbe, SANDBOX_ENDPOINT,
};
pub use request_accounting::{
    AccountingSnapshot, EncodedModelRequest, RequestAccounting, RequestPermit,
};
pub use runtime::{
    prepare_roundtable_connection, try_enqueue, AdmittedPrompt, ConnectionOwner,
    InteractivePermission, PreparedPrompt, PreparedRoundtableConnection, PrivateRuntimeSink,
    QueueReject, RoundtableLaunch, RoundtableLaunchPolicy, TurnGeneration,
    ROUNDTABLE_SERVICE_LABEL,
};
pub use sandbox::{
    attempt_live_escapes, build_sandbox_plan, DbIdentity, EscapeReport, IsolationProvider,
    JournalLaunchIntentStore, LaunchIntent, LaunchIntentStore, LinuxOciIsolator, PreparedSandbox,
    SandboxInput, SandboxInstance, SandboxPlan,
};

use roundtable_protocol::{ErrorCode, ErrorDetails, RtError};

pub(crate) fn rt_error(code: ErrorCode, reason: &'static str) -> RtError {
    let message = match code {
        ErrorCode::CapabilityUnqualified => "The capability is not qualified.",
        ErrorCode::PolicyUnenforceable => "The policy cannot be enforced.",
        ErrorCode::InvalidArgument => "The request is invalid.",
        ErrorCode::Unauthenticated => "The request is not authenticated.",
        ErrorCode::Forbidden => "The request is forbidden.",
        ErrorCode::CommandInProgress => "The command is already in progress.",
        ErrorCode::ContextTooLarge => "The context is too large.",
        ErrorCode::CapacityUnknown => "The context capacity is unknown.",
        ErrorCode::StorageUnavailable => "Storage is unavailable.",
        ErrorCode::InvalidState => "The room cannot accept this command.",
        ErrorCode::RuntimeUnavailable => "The runtime is unavailable.",
        _ => "The request is invalid.",
    };
    RtError {
        code,
        message: message.to_string(),
        retryable: code.retryable(),
        current_revision: None,
        details: ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}
