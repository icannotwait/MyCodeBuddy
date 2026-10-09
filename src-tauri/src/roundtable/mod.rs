//! Roundtable isolation boundary.
//!
//! The persistent execution gate defaults to off. This module does not migrate
//! ordinary session rows and does not claim a live OS sandbox on hosts that
//! cannot prove one.

mod acceptance;
mod actor;
mod api;
mod attempt_trace;
mod authorization;
mod budget_ledger;
pub(crate) mod capabilities;
mod clock;
mod conclusion;
#[cfg(any(test, feature = "test-utils"))]
mod command_processor;
mod companion;
mod control;
mod diagnostics;
mod e2e;
mod events;
mod feature_gate;
mod gateway;
mod host_model_auth;
pub(crate) mod ingress;
mod installed_runtime;
mod live_gateway;
mod live_runtime;
pub(crate) mod live_stream;
mod maintenance;
mod mcp;
mod objects;
mod owned_runtime;
mod ownership;
mod paging;
mod product;
mod qualification;
mod qualification_experiment;
mod qualification_harness;
mod qualification_linux;
mod qualification_probe;
mod qualification_profiles;
mod recovery;
mod registry;
mod relay;
mod request_accounting;
mod resources;
mod rollout;
mod runtime;
mod sandbox;
mod schema;
mod service;
mod snapshot;
mod store;
mod tool_core;
mod usage;

pub use acceptance::{AcceptInput, CloseInput, ClosingSetRef, PublishInput};
pub use actor::{RoomActor, RoomGate};
pub use api::http_status;
#[cfg(any(test, feature = "test-utils"))]
pub use api::{execute, RoundtableRequestV1, RoundtableResponseV1};
pub use authorization::{authorize_room, RoomDirectory, RoomGrant};
pub use budget_ledger::ActiveBudgetLease;
#[cfg(any(test, feature = "test-utils"))]
pub use budget_ledger::{
    checkpoint_active, recompute_room_budget, reserve_budget, AdmissionWindow, BillingSplit,
    RoomClockLedger,
};
pub use capabilities::{
    classify_service_failure, drain_recorded_failures, host_broker_call, host_tools_agent_manifest,
    policy_hash_for, record_service_response, record_service_update, sealed_service_manifest,
    service_client_capabilities_value, service_manifest_for_session, verify_service_manifest,
    FailureObservation, FailureSource, LaunchCapabilityManifestV1, ManifestExtras,
    OrdinarySessionFlags, ServiceTurnDecision,
};
pub use clock::{
    deadline_reached, AcceptStep, FakeClock, LockGate, MonoClock, MonotonicClock, SystemMono,
};
#[cfg(any(test, feature = "test-utils"))]
pub use command_processor::{next_phase, pause_again, retry_synthesis};
pub use companion::{
    advertised_tools, bind_service_process, callable_tools, legacy_companion_context,
    parse_companion_args, plan_service_launch, service_channel_owner,
    service_tools_ignoring_host_flags, tool_callable, CompanionMode, CompanionParse,
    FakeBrokerTransport, LegacyParentArgs, ServiceBroker, ServiceConnection, ServiceLaunchInput,
    ServiceLaunchPlan, ServiceProcess, ServiceWatchState, ServiceWatchdog, ATTEMPT_TOKEN_ENV,
};
pub use control::ControlRequest;
#[cfg(any(test, feature = "test-utils"))]
pub use control::{
    advance_control, apply_command, ControlBook, MatrixOutcome, MatrixRow, MutationCommandV1,
};
pub use diagnostics::{seal_diagnostic, DiagnosticCapture, DiagnosticInput, DiagnosticRef};
pub use e2e::{exercise_room, Trajectory};
pub use events::{
    advance_private_watermark, apply_projection, moderator_preview_allowed, projection_hash,
    SubscriptionHub,
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
#[cfg(any(test, feature = "test-utils"))]
pub use installed_runtime::verify_runtime_contract_fixture;
pub use installed_runtime::ProviderBinding;
#[cfg(any(test, feature = "test-utils"))]
pub use live_gateway::{
    exercise_gateway_http_failure_fixture, exercise_gateway_shutdown_fixture,
    exercise_live_gateway_fixture, exercise_queued_gateway_fixture, GatewayFixtureObservation,
};
#[cfg(any(test, feature = "test-utils"))]
pub use live_runtime::{
    acp_frame_fixture, diagnostic_finish_reason_fixture, drive_permission_repair_frames_fixture,
    drive_prompt_frames_fixture, exercise_live_acp_rpc, exercise_schema_seat_rpc,
    permission_reply_fixture, persist_runtime_diagnostic_fixture, prepared_live_cleanup_fixture,
    redact_untrusted_excerpt_fixture, rejected_live_executor_fixture, retire_attempt_files_fixture,
    retired_live_executor_fixture, run_dir_retention_fixture, session_params_fixture,
    verify_confirmed_option_fixture, LiveAcpRpcObservation, SchemaSeatObservation,
};
pub use maintenance::{
    backup_roundtable, gc_unreferenced, restore_roundtable, BackupManifest, GcReport,
};
pub use mcp::{
    invoke_scoped_tool, note_unverified_reference, persist_candidate, read_evidence,
    register_input_evidence, search_evidence, DurableToolStore, EvidenceSearchHit,
    EvidenceSearchPage, EvidenceSlice, EvidenceUsage, GateToolAuthority, InputEvidence,
    PersistedEvidence, ReadEvidenceArgs, SearchEvidenceArgs, SubmissionAudit, ToolAuthority,
    ToolSession,
};
pub use objects::{ObjectFault, ObjectRef, ObjectStore, ReservationLedger, StorageLease};
pub use owned_runtime::{
    OwnedParticipantRuntime, RoundtableTurnExecutor, RoundtableTurnOutcome, RoundtableTurnRequest,
    RuntimeCapability,
};
pub use ownership::{
    lock_path_for, matches_instance, CoordinatorLock, OwnedProcess, RoundtableReadService,
};
pub use paging::{read_manifest_page, CursorScope, ManifestPage, ScopedCursors};
pub use qualification::{
    evaluate_certificate, qualify_service, CertifiedBinary, OsIdentity, QualificationKey,
    QualificationReport,
};
pub use qualification_harness::QualificationHarness;
#[cfg(any(test, feature = "test-utils"))]
pub use qualification_linux::{
    advertised_probe_models, isolation_probe_script, probe_acp_exit_cleans_container,
    probe_model_binding_error, probe_reap_classification, probe_recorded_crun_version,
    probe_scratch_home, remove_probe_scratch_home,
};
#[cfg(any(test, feature = "test-utils"))]
pub use qualification_probe::{assemble_probe_report_for_test, verify_installed_report_for_test};
pub use qualification_probe::{
    qualify_adapter_on_host, roundtable_qualify_requested, run_roundtable_qualify, ProbeCheck,
    ProbeFacts, ProbeOutcome, ProbeRequest,
};
pub use qualification_profiles::{adapter_profiles, os_accepted, profile_by_id, profile_for_agent};
pub use recovery::{recover_service, RecoveryReport};
#[cfg(any(test, feature = "test-utils"))]
pub use recovery::{recovery_action, RecoveryAction, RecoveryState};
pub use registry::{
    downgrade_is_silent_compatible, hidden_from_ordinary_discovery, DiscoveryLease, ExternalId,
    InternalBindingRecord, ObserverWindow, RegisteredBinding, RegistryStore, RootLease,
    RoundtableSessionRegistry, StoredBinding,
};
pub use relay::ServiceModelRelay;
pub use relay::{
    forward_to_caller_target, probe_instance_socket, relay_from_helper, InstanceSocket,
    LoopbackRelay, SocketProbe, SANDBOX_ENDPOINT,
};
pub use request_accounting::{
    AccountingSnapshot, EncodedModelRequest, RequestAccounting, RequestPermit,
};
pub use resources::{ExecutionLease, PermitBundle, ResourceAllocator, PREPAID_SLICE_MS};
pub use rollout::{disable_and_drain, may_start, Rollout};
pub use runtime::{
    prepare_roundtable_connection, try_enqueue, AdmittedPrompt, ConnectionOwner,
    InteractivePermission, PreparedPrompt, PreparedRoundtableConnection, PrivateRuntimeSink,
    QueueReject, RoundtableLaunch, RoundtableLaunchPolicy, TurnGeneration,
    ROUNDTABLE_SERVICE_LABEL,
};
pub use sandbox::{
    attempt_live_escapes, build_qualified_sandbox_plan, build_sandbox_plan,
    qualified_oci_profile_hash, qualified_rootfs_digest, qualified_rootfs_digest_detail,
    verify_qualified_oci_profile, DbIdentity, EscapeReport, IsolationProvider,
    JournalLaunchIntentStore, LaunchIntent, LaunchIntentStore, LinuxOciIsolator, PreparedSandbox,
    QualifiedOciProfile, SandboxInput, SandboxInstance, SandboxPlan,
};
#[cfg(any(test, feature = "test-utils"))]
pub use sandbox::{
    cgroup_delegation_failure, live_slirp_document, slirp_hook_phase, slirp_hook_script,
    stage_attempt_auth, syscall_allowlist,
};
pub use schema::{
    apply_roundtable_schema, drop_roundtable_schema, roundtable_table_names, DurabilityProfile,
    LOGICAL_MODEL, RECORDED_DURABILITY,
};
pub use service::{
    build_production_service, shutdown_order, OpenedRoundtable, ParticipantRuntime,
    QuarantineLease, RoomMessage, RoomMessageKind, RoomReply, RoundtableService, RoundtableSlot,
    RuntimeIdentity, ServiceConfig, ServiceReadiness, ShutdownReport,
};
pub use snapshot::{
    build_delivery, canonical_path_bytes, capture_snapshot, commit_captured_manifest,
    confirmation_echo, ensure_within_root, freeze_phase, freeze_preflight, fresh_binding_context,
    lexical_within, line_start_offsets, offer_text_tool, path_within_root, rehome_manifest,
    validate_relative_path, ConfirmationEcho, PhaseInput, PreflightLog, ResolvedRecipients,
    RoomDraft, SelectedFile, SnapshotEncoding, SnapshotLimits, SourceClass, SourceEntryV1,
    SourceManifestV1, SourceSelection, FRESH_CONTEXT_STATE, MAX_SNAPSHOT_READS,
};
pub use store::{
    durability_from_report, migrate_roundtable, open_roundtable_store, promises_power_loss,
    verify_connection_profile, NewAttempt, NewBinding, NewClaim, NewCommand, NewEvent, NewEvidence,
    NewManifest, NewMessage, NewPhase, NewRoom, NewSpeaker, NewSubmission, NewTurn,
    RoundtableStore,
};
pub use tool_core::{
    dispatch_tool, service_result_schema, service_tool_names, service_tool_schema,
    AdmittedToolScope, AttemptToken, InMemoryToolStore, RoundtableToolCall, RoundtableToolResponse,
    TokenBinding, TokenRegistry, ToolStore, SERVICE_RESULT_SCHEMA_ID, SERVICE_TOOL_VERSION,
};
pub use usage::{archive_late_measurement, RoomMeter};

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
