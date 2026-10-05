//! Roundtable isolation boundary.
//!
//! The persistent execution gate defaults to off. This module does not migrate
//! ordinary session rows and does not claim a live OS sandbox on hosts that
//! cannot prove one.

mod acceptance;
pub(crate) mod capabilities;
mod clock;
mod companion;
mod feature_gate;
mod mcp;
mod gateway;
pub(crate) mod ingress;
mod qualification;
mod qualification_harness;
mod registry;
mod relay;
mod request_accounting;
mod runtime;
mod objects;
mod sandbox;
mod schema;
mod snapshot;
mod store;
mod tool_core;

pub use companion::{
    advertised_tools, bind_service_process, callable_tools, legacy_companion_context,
    parse_companion_args, plan_service_launch, service_channel_owner,
    service_tools_ignoring_host_flags, tool_callable, CompanionMode, CompanionParse,
    FakeBrokerTransport, LegacyParentArgs, ServiceLaunchInput, ServiceLaunchPlan, ServiceProcess,
    ServiceWatchState, ServiceWatchdog, ATTEMPT_TOKEN_ENV,
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
pub use capabilities::{
    classify_service_failure, drain_recorded_failures, host_broker_call,
    host_tools_agent_manifest, policy_hash_for, record_service_response, record_service_update,
    sealed_service_manifest, service_client_capabilities_value, service_manifest_for_session,
    verify_service_manifest, FailureObservation, FailureSource, LaunchCapabilityManifestV1,
    ManifestExtras, OrdinarySessionFlags, ServiceTurnDecision,
};
pub use qualification::{
    evaluate_certificate, qualify_service, CertifiedBinary, OsIdentity, QualificationKey,
    QualificationReport,
};
pub use qualification_harness::QualificationHarness;
pub use registry::{
    downgrade_is_silent_compatible, hidden_from_ordinary_discovery, DiscoveryLease, ExternalId,
    InternalBindingRecord, ObserverWindow, RegisteredBinding, RegistryStore, RootLease,
    RoundtableSessionRegistry, StoredBinding,
};
pub use schema::{
    apply_roundtable_schema, drop_roundtable_schema, roundtable_table_names, DurabilityProfile,
    LOGICAL_MODEL, RECORDED_DURABILITY,
};
pub use objects::{
    ObjectFault, ObjectRef, ObjectStore, ReservationLedger, StorageLease,
};
pub use snapshot::{
    build_delivery, canonical_path_bytes, capture_snapshot, commit_captured_manifest,
    confirmation_echo, ensure_within_root, freeze_phase, freeze_preflight, fresh_binding_context,
    lexical_within, line_start_offsets, offer_text_tool, path_within_root, validate_relative_path,
    ConfirmationEcho, PhaseInput, PreflightLog, ResolvedRecipients, RoomDraft, SelectedFile,
    SnapshotEncoding, SnapshotLimits, SourceClass, SourceEntryV1, SourceManifestV1,
    SourceSelection, FRESH_CONTEXT_STATE, MAX_SNAPSHOT_READS,
};
pub use acceptance::{AcceptInput, CloseInput, ClosingSetRef, PublishInput};
pub use clock::{deadline_reached, AcceptStep, FakeClock, LockGate, MonoClock, SystemMono};
pub use store::{
    durability_from_report, migrate_roundtable, open_roundtable_store, promises_power_loss,
    verify_connection_profile, NewAttempt, NewBinding, NewClaim, NewCommand, NewEvent, NewEvidence,
    NewManifest, NewMessage, NewPhase, NewRoom, NewSpeaker, NewSubmission, NewTurn, RoundtableStore,
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
pub use mcp::{
    invoke_scoped_tool, note_unverified_reference, persist_candidate, read_evidence,
    register_input_evidence, search_evidence, DurableToolStore, EvidenceSearchHit,
    EvidenceSearchPage, EvidenceSlice,
    EvidenceUsage, GateToolAuthority, InputEvidence, PersistedEvidence, ReadEvidenceArgs,
    SearchEvidenceArgs, SubmissionAudit, ToolAuthority, ToolSession,
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
