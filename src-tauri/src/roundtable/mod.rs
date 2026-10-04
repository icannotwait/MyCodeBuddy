//! Roundtable isolation boundary.
//!
//! The persistent execution gate defaults to off. This module does not migrate
//! ordinary session rows and does not claim a live OS sandbox on hosts that
//! cannot prove one.

mod companion;
mod feature_gate;
mod gateway;
pub(crate) mod ingress;
mod qualification;
mod qualification_harness;
mod registry;
mod relay;
mod request_accounting;
mod runtime;
mod sandbox;
mod tool_core;

pub use companion::{
    advertised_tools, bind_service_process, callable_tools, legacy_companion_context,
    parse_companion_args, plan_service_launch, service_tools_ignoring_host_flags, tool_callable,
    CompanionMode, CompanionParse, FakeBrokerTransport, LegacyParentArgs, ServiceLaunchInput,
    ServiceLaunchPlan, ServiceProcess, ServiceWatchState, ServiceWatchdog, ATTEMPT_TOKEN_ENV,
};
pub use feature_gate::{
    scopes_convert, AdmissionFacts, ExecutionGate, ExecutionPolicy, ExecutionScope, GatePermit,
};
pub use gateway::{
    ApprovedOrigin, ClientPolicy, GatewayHandle, GatewayLease, HoldRelease, HostCredential,
    HostModelGateway, ModelRequest, ModelResponse, ObservedForward,
};
pub use ingress::{
    bind_private_ingress, BarrierFact, CompletionCoordinator, CompletionMarker, IngressKind,
    RuntimeIngress,
};
pub use qualification::{
    evaluate_certificate, CertifiedBinary, OsIdentity, QualificationKey, QualificationReport,
};
pub use qualification_harness::QualificationHarness;
pub use registry::{
    downgrade_is_silent_compatible, hidden_from_ordinary_discovery, DiscoveryLease, ExternalId,
    InternalBindingRecord, ObserverWindow, RegisteredBinding, RootLease, RoundtableSessionRegistry,
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
pub use tool_core::{
    dispatch_tool, service_result_schema, service_tool_names, service_tool_schema,
    AdmittedToolScope, AttemptToken, InMemoryToolStore, RoundtableToolCall, RoundtableToolResponse,
    TokenBinding, TokenRegistry, ToolStore, SERVICE_RESULT_SCHEMA_ID, SERVICE_TOOL_VERSION,
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
