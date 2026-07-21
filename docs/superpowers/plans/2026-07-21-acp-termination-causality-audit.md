# ACP Termination Causality Audit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attribute every ACP cancel/disconnect to an exact typed producer, preserve the initiating cause through cleanup, write a secret-free structured causality chain, and expose the latest durable termination summary for `codeg://session/<id>` diagnosis.

**Architecture:** A new `acp::termination_audit` module owns the cause model, request/root correlation, bounded intent registry, classifications, and structured log helpers. `ConnectionManager` admits every destructive request before removal or control send; `connection.rs` records control receipt and unrequested exits; the lifecycle worker joins terminal observation, broker settlement, and a conditional conversation projection without making teardown depend on audit persistence.

**Tech Stack:** Rust 2021, Tokio, tracing JSON logs, SeaORM, SQLite, serde, UUID v4, Tauri 2, Axum, React 19, TypeScript strict, Vitest.

## Global Constraints

- The approved design is `docs/superpowers/specs/2026-07-21-acp-termination-causality-audit-design.md`.
- Audit schema version is exactly `1`.
- Structured events use target `codeg_lib::acp::termination` and message `acp_termination`.
- Existing INFO JSON logging and `CODEG_LOG_MAX_FILES` retention remain unchanged; the default remains 30 files.
- The first destructive root wins. Cleanup requests get new request ids but preserve `root_id` and `parent_request_id`.
- Registry size is capped at 4096 entries and incomplete entries expire after 24 hours.
- Candidate summaries are ordered by `connection_started_at`, then `ownership_generation`, then `observed_at`.
- Teardown never waits for file-log I/O and never fails because summary persistence failed.
- Missing reasons are accepted only on the old web boundary as `legacy_unspecified` and emit WARN. Unknown strings are rejected.
- Audit fields are limited to fixed enums, booleans, counters, timestamps, and opaque Codeg ids.
- Never log or persist prompts, model output, task text/previews, tool payloads, paths, environment values, credentials, command lines, provider messages, stack traces, or free-form errors.
- Keep the existing raw broker terminal-error behavior separate from the audit record; the new audit path adds only fixed source/reason/root fields.
- Do not add an audit UI and do not change ordinary conversation rendering.
- Do not implement interrupted-run retry/recovery policy in this work.
- Before editing, run the exact task-scoped preflight command printed
  in that task. Preserve concurrent Grok-retry/chat-channel work and stage only
  the literal paths printed in that task's commit step.

---

## File Map

- `src-tauri/src/acp/termination_audit.rs`: typed causes, request/root correlation, state snapshots, bounded registry, summary classification, and tracing helpers.
- `src-tauri/src/acp/session_state.rs`: backend-only connection start and ownership metadata used by snapshots.
- `src-tauri/src/acp/manager.rs`: admission before teardown, state capture, registry ownership, and typed manager/spawner APIs.
- `src-tauri/src/acp/connection.rs`: typed control payloads, control receipt markers, cancel-notification markers, and proven unrequested exit causes.
- `src-tauri/src/acp/lifecycle.rs`: TurnComplete/terminal observation, broker handoff, summary persistence, and idempotent finalization.
- `src-tauri/src/acp/delegation/{spawner.rs,broker.rs}`: typed teardown calls and stable provenance in child-cancel settlement.
- `src-tauri/src/db/migration/m20260721_000001_acp_termination_audit.rs`: nullable conversation projection column.
- `src-tauri/src/db/{entities/conversation.rs,service/conversation_service.rs,service/import_service.rs}`: entity mapping, conditional write, decoding, and insert defaults.
- `src-tauri/src/models/conversation.rs`, `src-tauri/src/commands/conversations.rs`, and `src-tauri/src/acp/session_info.rs`: typed diagnostic responses and fixture initialization.
- `src-tauri/src/{automation/engine.rs,auto_title/runner.rs,document_translate/runner.rs}`: exact non-frontend producer causes.
- `src-tauri/src/{commands/acp.rs,web/handlers/acp.rs,lib.rs,bin/codeg_server.rs}`: transport boundaries, window/app shutdown, and standalone-server shutdown.
- `src/lib/{types.ts,api.ts,tauri.ts}` and `src/contexts/acp-connections-context.tsx`: fixed frontend reason union and exact call-site attribution.

---

### Task 1: Typed Audit Model, Classification, And Bounded Registry

**Files:**
- Create: `src-tauri/src/acp/termination_audit.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/Cargo.toml`
- Test: inline tests in `src-tauri/src/acp/termination_audit.rs`

**Preflight:**

```powershell
git diff -- src-tauri/Cargo.toml src-tauri/src/acp/mod.rs src-tauri/src/acp/termination_audit.rs
```

**Interfaces:**
- Produces: `AcpTerminationCause` and all fixed nested reason enums.
- Produces: `AcpTerminationCorrelation::{root, child_of}`.
- Produces: `TerminationIntentRegistry::{admit, admit_existing, register_unrequested_exit, current, current_summary, mark_control_sent, mark_control_send_failed, mark_control_received, mark_cancel_notification_sent, observe_turn_complete, observe_terminal, mark_persisted, finish}`.
- Produces: `AcpTerminationSummaryV1` and `AcpTerminationClassification`.
- Produces: structured event helpers for all 12 approved event names.

- [ ] **Step 1: Write failing model and conversation-832 classification tests**

Create the module with tests first. Use one fixed snapshot helper so no test
silently omits a privacy-sensitive field:

```rust
fn prompting_snapshot(started_at: DateTime<Utc>) -> AcpTerminationStateSnapshot {
    AcpTerminationStateSnapshot {
        connection_status: ConnectionStatus::Prompting,
        conversation_id: Some(832),
        agent_type: AgentType::Codex,
        connection_started_at: started_at,
        event_seq: 980,
        active_prompt: true,
        pending_permission: false,
        active_tool_call_count: 1,
        background_outstanding: 0,
        last_activity_age_ms: 10,
        last_agent_activity_age_ms: 15,
        owner_window_label: "main".into(),
        owner_operation_id: None,
        ownership_generation: 0,
    }
}

#[test]
fn conversation_832_provider_unmount_is_disconnect_before_turn_complete() {
    let registry = TerminationIntentRegistry::for_test(16, Duration::hours(1));
    let now = DateTime::parse_from_rfc3339("2026-07-21T07:41:43Z")
        .unwrap()
        .with_timezone(&Utc);
    registry.admit(
        "child-832",
        AcpTerminationAction::Disconnect,
        AcpTerminationCause::Frontend(FrontendTerminationReason::ProviderUnmount),
        AcpTerminationCorrelation::root(None),
        prompting_snapshot(now - Duration::minutes(10)),
        now,
    );

    let observed = registry
        .observe_terminal(
            "child-832",
            Some(981),
            None,
            AcpTerminationCause::LegacyUnspecified,
            None,
            now + Duration::milliseconds(2),
        )
        .expect("registered intent has a snapshot");

    assert_eq!(
        observed.summary.classification,
        AcpTerminationClassification::DisconnectBeforeTurnComplete
    );
    assert_eq!(observed.summary.source, AcpTerminationSource::Frontend);
    assert_eq!(
        observed.summary.reason,
        AcpTerminationReason::ProviderUnmount
    );
}

#[test]
fn conversation_832_backend_idle_is_not_provider_unmount() {
    let registry = TerminationIntentRegistry::for_test(16, Duration::hours(1));
    let now = Utc::now();
    registry.admit(
        "child-832-idle",
        AcpTerminationAction::Disconnect,
        AcpTerminationCause::BackendIdleSweep {
            idle_age_ms: 181_000,
            timeout_ms: 180_000,
        },
        AcpTerminationCorrelation::root(None),
        prompting_snapshot(now - Duration::minutes(10)),
        now,
    );
    let observed = registry
        .observe_terminal(
            "child-832-idle",
            Some(981),
            None,
            AcpTerminationCause::LegacyUnspecified,
            None,
            now,
        )
        .unwrap();
    assert_eq!(observed.summary.source, AcpTerminationSource::BackendIdleSweep);
    assert_eq!(
        observed.summary.reason,
        AcpTerminationReason::BackendIdleSweep
    );
}
```

Also add tests for:

- stable snake-case serialization of every source/reason variant;
- child requests preserving the first root cause;
- duplicate requests not replacing the root;
- `TurnComplete` after request classifying `turn_complete_before_disconnect`;
- no-active-prompt teardown classifying `disconnect_without_active_prompt`;
- absent intent becoming `unrequested_terminal`;
- stale/max-capacity eviction;
- a newer connection start beating an older late observation;
- summary JSON containing no key outside the approved allowlist.

- [ ] **Step 2: Run the focused test and verify RED**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils acp::termination_audit::tests -- --nocapture
```

Expected: compilation fails because the module and types do not exist.

- [ ] **Step 3: Implement the fixed enums and public data shapes**

Enable UUID serde:

```toml
uuid = { version = "1", features = ["v4", "serde"] }
```

Expose the following exact public surface:

```rust
pub const TERMINATION_LOG_TARGET: &str = "codeg_lib::acp::termination";
pub const TERMINATION_AUDIT_VERSION: u8 = 1;
pub const MAX_TERMINATION_INTENTS: usize = 4096;
pub const TERMINATION_INTENT_TTL_HOURS: i64 = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationAction {
    Cancel,
    Disconnect,
}

impl AcpTerminationAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cancel => "cancel",
            Self::Disconnect => "disconnect",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrontendTerminationReason {
    UserStop,
    ContextDisconnect,
    ProviderUnmount,
    FrontendIdleTimeout,
    ConnectAbandoned,
    ConnectSuperseded,
    ConnectionReplaced,
    DisconnectAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSetupTerminationReason {
    RouteFallbackCleanup,
    BootstrapFailureCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrokerTerminationReason {
    TerminalCleanup,
    SetupFailureCleanup,
    TerminalPersistenceFailureCleanup,
    ExplicitTaskCancel,
    ExternalHandleCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentTerminationReason {
    ParentCancel,
    ParentDisconnect,
    ParentTurnEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationTerminationReason {
    NormalCompletion,
    ExplicitCancellation,
    AdmissionFailure,
    FailureCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalRunnerTerminationReason {
    NormalCompletion,
    ExplicitCancellation,
    AdmissionFailure,
    FailureCleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpTerminationCause {
    Frontend(FrontendTerminationReason),
    BackendIdleSweep { idle_age_ms: u64, timeout_ms: u64 },
    ConnectionSetup(ConnectionSetupTerminationReason),
    Broker(BrokerTerminationReason),
    Parent(ParentTerminationReason),
    Automation(AutomationTerminationReason),
    InternalRunner(InternalRunnerTerminationReason),
    AgentProbe,
    ApplicationShutdown,
    TransportClosed,
    ProcessExited { stable_code: Option<StableExitCode> },
    ControlChannelClosed,
    LegacyUnspecified,
}

impl From<FrontendTerminationReason> for AcpTerminationReason {
    fn from(value: FrontendTerminationReason) -> Self {
        match value {
            FrontendTerminationReason::UserStop => Self::UserStop,
            FrontendTerminationReason::ContextDisconnect => {
                Self::ContextDisconnect
            }
            FrontendTerminationReason::ProviderUnmount => Self::ProviderUnmount,
            FrontendTerminationReason::FrontendIdleTimeout => {
                Self::FrontendIdleTimeout
            }
            FrontendTerminationReason::ConnectAbandoned => Self::ConnectAbandoned,
            FrontendTerminationReason::ConnectSuperseded => {
                Self::ConnectSuperseded
            }
            FrontendTerminationReason::ConnectionReplaced => {
                Self::ConnectionReplaced
            }
            FrontendTerminationReason::DisconnectAll => Self::DisconnectAll,
        }
    }
}

impl From<ConnectionSetupTerminationReason> for AcpTerminationReason {
    fn from(value: ConnectionSetupTerminationReason) -> Self {
        match value {
            ConnectionSetupTerminationReason::RouteFallbackCleanup => {
                Self::RouteFallbackCleanup
            }
            ConnectionSetupTerminationReason::BootstrapFailureCleanup => {
                Self::BootstrapFailureCleanup
            }
        }
    }
}

impl From<BrokerTerminationReason> for AcpTerminationReason {
    fn from(value: BrokerTerminationReason) -> Self {
        match value {
            BrokerTerminationReason::TerminalCleanup => Self::TerminalCleanup,
            BrokerTerminationReason::SetupFailureCleanup => {
                Self::SetupFailureCleanup
            }
            BrokerTerminationReason::TerminalPersistenceFailureCleanup => {
                Self::TerminalPersistenceFailureCleanup
            }
            BrokerTerminationReason::ExplicitTaskCancel => {
                Self::ExplicitTaskCancel
            }
            BrokerTerminationReason::ExternalHandleCancel => {
                Self::ExternalHandleCancel
            }
        }
    }
}

impl From<ParentTerminationReason> for AcpTerminationReason {
    fn from(value: ParentTerminationReason) -> Self {
        match value {
            ParentTerminationReason::ParentCancel => Self::ParentCancel,
            ParentTerminationReason::ParentDisconnect => Self::ParentDisconnect,
            ParentTerminationReason::ParentTurnEnded => Self::ParentTurnEnded,
        }
    }
}

impl From<AutomationTerminationReason> for AcpTerminationReason {
    fn from(value: AutomationTerminationReason) -> Self {
        match value {
            AutomationTerminationReason::NormalCompletion => {
                Self::NormalCompletion
            }
            AutomationTerminationReason::ExplicitCancellation => {
                Self::ExplicitCancellation
            }
            AutomationTerminationReason::AdmissionFailure => {
                Self::AdmissionFailure
            }
            AutomationTerminationReason::FailureCleanup => Self::FailureCleanup,
        }
    }
}

impl From<InternalRunnerTerminationReason> for AcpTerminationReason {
    fn from(value: InternalRunnerTerminationReason) -> Self {
        match value {
            InternalRunnerTerminationReason::NormalCompletion => {
                Self::NormalCompletion
            }
            InternalRunnerTerminationReason::ExplicitCancellation => {
                Self::ExplicitCancellation
            }
            InternalRunnerTerminationReason::AdmissionFailure => {
                Self::AdmissionFailure
            }
            InternalRunnerTerminationReason::FailureCleanup => {
                Self::FailureCleanup
            }
        }
    }
}

impl AcpTerminationCause {
    pub const fn source(&self) -> AcpTerminationSource {
        match self {
            Self::Frontend(_) => AcpTerminationSource::Frontend,
            Self::BackendIdleSweep { .. } => {
                AcpTerminationSource::BackendIdleSweep
            }
            Self::ConnectionSetup(_) => AcpTerminationSource::ConnectionSetup,
            Self::Broker(_) => AcpTerminationSource::Broker,
            Self::Parent(_) => AcpTerminationSource::Parent,
            Self::Automation(_) => AcpTerminationSource::Automation,
            Self::InternalRunner(_) => AcpTerminationSource::InternalRunner,
            Self::AgentProbe => AcpTerminationSource::AgentProbe,
            Self::ApplicationShutdown => AcpTerminationSource::Application,
            Self::TransportClosed => AcpTerminationSource::Transport,
            Self::ProcessExited { .. } => AcpTerminationSource::Process,
            Self::ControlChannelClosed => AcpTerminationSource::ControlChannel,
            Self::LegacyUnspecified => AcpTerminationSource::Legacy,
        }
    }

    pub fn reason(&self) -> AcpTerminationReason {
        match self {
            Self::Frontend(reason) => (*reason).into(),
            Self::BackendIdleSweep { .. } => {
                AcpTerminationReason::BackendIdleSweep
            }
            Self::ConnectionSetup(reason) => (*reason).into(),
            Self::Broker(reason) => (*reason).into(),
            Self::Parent(reason) => (*reason).into(),
            Self::Automation(reason) => (*reason).into(),
            Self::InternalRunner(reason) => (*reason).into(),
            Self::AgentProbe => AcpTerminationReason::AgentProbe,
            Self::ApplicationShutdown => AcpTerminationReason::ApplicationShutdown,
            Self::TransportClosed => AcpTerminationReason::TransportClosed,
            Self::ProcessExited { .. } => AcpTerminationReason::ProcessExited,
            Self::ControlChannelClosed => {
                AcpTerminationReason::ControlChannelClosed
            }
            Self::LegacyUnspecified => AcpTerminationReason::LegacyUnspecified,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpTerminationCorrelation {
    pub root_id: Option<Uuid>,
    pub parent_request_id: Option<Uuid>,
    pub task_id: Option<String>,
}

impl AcpTerminationCorrelation {
    pub fn root(task_id: Option<String>) -> Self {
        Self {
            root_id: None,
            parent_request_id: None,
            task_id,
        }
    }

    pub fn child_of(
        root_id: Uuid,
        parent_request_id: Uuid,
        task_id: Option<String>,
    ) -> Self {
        Self {
            root_id: Some(root_id),
            parent_request_id: Some(parent_request_id),
            task_id,
        }
    }

    pub fn from_request(request: &AcpTerminationRequest) -> Self {
        Self::child_of(
            request.root_id,
            request.request_id,
            request.task_id.clone(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpTerminationRequest {
    pub version: u8,
    pub request_id: Uuid,
    pub root_id: Uuid,
    pub parent_request_id: Option<Uuid>,
    pub action: AcpTerminationAction,
    pub cause: AcpTerminationCause,
    pub requested_at: DateTime<Utc>,
    pub task_id: Option<String>,
}
```

Add every downstream type in this task; later tasks must not invent parallel
wire shapes:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationSource {
    Frontend,
    BackendIdleSweep,
    ConnectionSetup,
    Broker,
    Parent,
    Automation,
    InternalRunner,
    AgentProbe,
    Application,
    Transport,
    Process,
    ControlChannel,
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationReason {
    UserStop,
    ContextDisconnect,
    ProviderUnmount,
    FrontendIdleTimeout,
    ConnectAbandoned,
    ConnectSuperseded,
    ConnectionReplaced,
    DisconnectAll,
    BackendIdleSweep,
    RouteFallbackCleanup,
    BootstrapFailureCleanup,
    TerminalCleanup,
    SetupFailureCleanup,
    TerminalPersistenceFailureCleanup,
    ExplicitTaskCancel,
    ExternalHandleCancel,
    ParentCancel,
    ParentDisconnect,
    ParentTurnEnded,
    NormalCompletion,
    ExplicitCancellation,
    AdmissionFailure,
    FailureCleanup,
    AgentProbe,
    ApplicationShutdown,
    TransportClosed,
    ProcessExited,
    ControlChannelClosed,
    LegacyUnspecified,
}

impl AcpTerminationSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::BackendIdleSweep => "backend_idle_sweep",
            Self::ConnectionSetup => "connection_setup",
            Self::Broker => "broker",
            Self::Parent => "parent",
            Self::Automation => "automation",
            Self::InternalRunner => "internal_runner",
            Self::AgentProbe => "agent_probe",
            Self::Application => "application",
            Self::Transport => "transport",
            Self::Process => "process",
            Self::ControlChannel => "control_channel",
            Self::Legacy => "legacy",
        }
    }
}

impl AcpTerminationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserStop => "user_stop",
            Self::ContextDisconnect => "context_disconnect",
            Self::ProviderUnmount => "provider_unmount",
            Self::FrontendIdleTimeout => "frontend_idle_timeout",
            Self::ConnectAbandoned => "connect_abandoned",
            Self::ConnectSuperseded => "connect_superseded",
            Self::ConnectionReplaced => "connection_replaced",
            Self::DisconnectAll => "disconnect_all",
            Self::BackendIdleSweep => "backend_idle_sweep",
            Self::RouteFallbackCleanup => "route_fallback_cleanup",
            Self::BootstrapFailureCleanup => "bootstrap_failure_cleanup",
            Self::TerminalCleanup => "terminal_cleanup",
            Self::SetupFailureCleanup => "setup_failure_cleanup",
            Self::TerminalPersistenceFailureCleanup => {
                "terminal_persistence_failure_cleanup"
            }
            Self::ExplicitTaskCancel => "explicit_task_cancel",
            Self::ExternalHandleCancel => "external_handle_cancel",
            Self::ParentCancel => "parent_cancel",
            Self::ParentDisconnect => "parent_disconnect",
            Self::ParentTurnEnded => "parent_turn_ended",
            Self::NormalCompletion => "normal_completion",
            Self::ExplicitCancellation => "explicit_cancellation",
            Self::AdmissionFailure => "admission_failure",
            Self::FailureCleanup => "failure_cleanup",
            Self::AgentProbe => "agent_probe",
            Self::ApplicationShutdown => "application_shutdown",
            Self::TransportClosed => "transport_closed",
            Self::ProcessExited => "process_exited",
            Self::ControlChannelClosed => "control_channel_closed",
            Self::LegacyUnspecified => "legacy_unspecified",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StableExitCode {
    ProcessExited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StableStopReason {
    EndTurn,
    Cancelled,
    Refusal,
    MaxTokens,
    MaxTurnRequests,
    Empty,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpTerminationSummaryWriteLogOutcome {
    Persisted,
    SkippedNewer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpTerminationSummaryFailureCode {
    DatabaseWriteFailed,
}

impl StableStopReason {
    pub fn from_wire(value: &str) -> Self {
        match value {
            "end_turn" => Self::EndTurn,
            "cancelled" => Self::Cancelled,
            "refusal" => Self::Refusal,
            "max_tokens" => Self::MaxTokens,
            "max_turn_requests" => Self::MaxTurnRequests,
            "empty" => Self::Empty,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpTerminationClassification {
    TurnCompleteBeforeDisconnect,
    DisconnectBeforeTurnComplete,
    DisconnectWithoutActivePrompt,
    UnrequestedTerminal,
    OrderingUnknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpTerminationStateSnapshot {
    pub connection_status: ConnectionStatus,
    pub conversation_id: Option<i32>,
    pub agent_type: AgentType,
    pub connection_started_at: DateTime<Utc>,
    pub event_seq: u64,
    pub active_prompt: bool,
    pub pending_permission: bool,
    pub active_tool_call_count: u32,
    pub background_outstanding: u32,
    pub last_activity_age_ms: u64,
    pub last_agent_activity_age_ms: u64,
    pub owner_window_label: String,
    pub owner_operation_id: Option<String>,
    pub ownership_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcpTerminationSummaryV1 {
    pub version: u8,
    pub root_id: Uuid,
    pub final_request_id: Uuid,
    pub connection_id: String,
    pub action: AcpTerminationAction,
    pub source: AcpTerminationSource,
    pub reason: AcpTerminationReason,
    pub classification: AcpTerminationClassification,
    pub task_id: Option<String>,
    pub connection_status_at_request: ConnectionStatus,
    pub active_prompt: bool,
    pub connection_started_at: DateTime<Utc>,
    pub ownership_generation: u64,
    pub turn_complete_event_seq: Option<u64>,
    pub terminal_event_seq: Option<u64>,
    pub requested_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpTerminationObservation {
    pub summary: AcpTerminationSummaryV1,
    pub snapshot: AcpTerminationStateSnapshot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcpTerminationIntentView {
    pub root_request: AcpTerminationRequest,
    pub latest_request: AcpTerminationRequest,
    pub snapshot: AcpTerminationStateSnapshot,
    pub synthetic_unrequested: bool,
    pub control_sent_at: Option<DateTime<Utc>>,
    pub control_send_failed_at: Option<DateTime<Utc>>,
    pub control_received_at: Option<DateTime<Utc>>,
    pub cancel_notification_sent_at: Option<DateTime<Utc>>,
    pub turn_complete_event_seq: Option<u64>,
    pub stop_reason: Option<StableStopReason>,
    pub terminal_event_seq: Option<u64>,
    pub stable_error_code: Option<StableExitCode>,
    pub persisted_at: Option<DateTime<Utc>>,
}
```

`AcpTerminationCause::source()` and `reason()` return the two fixed summary
enums exhaustively. `StableExitCode::ProcessExited` is emitted only for the
typed `AcpError::ProcessExited` path; arbitrary protocol/provider text maps to
`None`, never to a new string value.

- [ ] **Step 4: Implement snapshots, summary ordering, and the registry**

Use a short synchronous critical section:

```rust
#[derive(Clone)]
pub struct TerminationIntentRegistry {
    inner: Arc<std::sync::Mutex<RegistryState>>,
    max_entries: usize,
    ttl: Duration,
}

impl TerminationIntentRegistry {
    pub fn admit(
        &self,
        connection_id: &str,
        action: AcpTerminationAction,
        cause: AcpTerminationCause,
        correlation: AcpTerminationCorrelation,
        snapshot: AcpTerminationStateSnapshot,
        requested_at: DateTime<Utc>,
    ) -> AcpTerminationRequest;

    pub fn admit_existing(
        &self,
        connection_id: &str,
        action: AcpTerminationAction,
        cause: AcpTerminationCause,
        correlation: AcpTerminationCorrelation,
        requested_at: DateTime<Utc>,
    ) -> Option<AcpTerminationRequest>;

    pub fn register_unrequested_exit(
        &self,
        connection_id: &str,
        cause: AcpTerminationCause,
        snapshot: AcpTerminationStateSnapshot,
        observed_at: DateTime<Utc>,
    ) -> AcpTerminationRequest;

    pub fn current(
        &self,
        connection_id: &str,
    ) -> Option<AcpTerminationIntentView>;

    pub fn current_summary(
        &self,
        connection_id: &str,
        root_id: Uuid,
    ) -> Option<AcpTerminationSummaryV1>;

    pub fn mark_control_sent(
        &self,
        connection_id: &str,
        request_id: Uuid,
        observed_at: DateTime<Utc>,
    ) -> bool;

    pub fn mark_control_send_failed(
        &self,
        connection_id: &str,
        request_id: Uuid,
        stable_error_code: StableExitCode,
        observed_at: DateTime<Utc>,
    ) -> bool;

    pub fn mark_control_received(
        &self,
        connection_id: &str,
        request_id: Uuid,
        observed_at: DateTime<Utc>,
    ) -> bool;

    pub fn mark_cancel_notification_sent(
        &self,
        connection_id: &str,
        request_id: Uuid,
        observed_at: DateTime<Utc>,
    ) -> bool;

    pub fn observe_turn_complete(
        &self,
        connection_id: &str,
        event_seq: u64,
        stop_reason: StableStopReason,
        observed_at: DateTime<Utc>,
    );

    pub fn observe_terminal(
        &self,
        connection_id: &str,
        terminal_event_seq: Option<u64>,
        stable_error_code: Option<StableExitCode>,
        fallback_cause: AcpTerminationCause,
        fallback_snapshot: Option<AcpTerminationStateSnapshot>,
        observed_at: DateTime<Utc>,
    ) -> Option<AcpTerminationObservation>;

    pub fn mark_persisted(
        &self,
        connection_id: &str,
        root_id: Uuid,
        observed_at: DateTime<Utc>,
    ) -> bool;

    pub fn finish(&self, connection_id: &str, root_id: Uuid) -> bool;
}
```

Store a marker-only entry when `TurnComplete` arrives before a cleanup request.
When a root is already present, `admit` creates a child request from the latest
request even if the caller supplied root correlation. `admit_existing` does the
same without a new snapshot; it exists for idempotent broker cleanup after the
live connection has already left the manager map. It returns `None` when no
registry entry exists, so a genuinely unknown connection cannot acquire
fabricated state. `register_unrequested_exit` accepts only `TransportClosed`,
`ProcessExited`, `ControlChannelClosed`, or `LegacyUnspecified`; it creates a
synthetic root marked `synthetic_unrequested=true` only when no request already
exists. It never overwrites an admitted destructive request. `observe_terminal`
uses that marker to classify `unrequested_terminal` even if the snapshot says a
prompt was active. `current_summary` rebuilds `final_request_id` from the latest
cleanup request after terminal observation. Evict expired entries before each
mutation, then evict the oldest updated entry until the cap is met. Recover
poisoned mutexes with `into_inner()` and emit an invariant ERROR.

Summary replacement must use:

```rust
pub fn supersedes(&self, current: &Self) -> bool {
    (
        self.connection_started_at,
        self.ownership_generation,
        self.observed_at,
    ) >= (
        current.connection_started_at,
        current.ownership_generation,
        current.observed_at,
    )
}
```

- [ ] **Step 5: Implement the structured log helpers**

Keep each helper's fields fixed. The request helper chooses WARN for an active
prompt or `legacy_unspecified` and INFO otherwise. Control-send failure,
summary persistence failure, and invariant violations use ERROR.

```rust
tracing::warn!(
    target: TERMINATION_LOG_TARGET,
    event = "termination.requested",
    connection_id = %connection_id,
    request_id = %request.request_id,
    root_id = %request.root_id,
    action = request.action.as_str(),
    source = request.cause.source().as_str(),
    reason = request.cause.reason().as_str(),
    conversation_id = ?snapshot.conversation_id,
    event_seq = snapshot.event_seq,
    active_prompt = snapshot.active_prompt,
    pending_permission = snapshot.pending_permission,
    active_tool_call_count = snapshot.active_tool_call_count,
    background_outstanding = snapshot.background_outstanding,
    ownership_generation = snapshot.ownership_generation,
    "acp_termination"
);
```

The lifecycle-facing persistence helpers have fixed enum inputs:

```rust
pub fn log_summary_persisted(
    summary: &AcpTerminationSummaryV1,
    outcome: AcpTerminationSummaryWriteLogOutcome,
);

pub fn log_summary_persist_failed(
    summary: &AcpTerminationSummaryV1,
    code: AcpTerminationSummaryFailureCode,
);
```

Implement all approved event names:
`termination.requested`, `termination.duplicate`,
`termination.control_sent`, `termination.control_send_failed`,
`termination.control_received`, `termination.cancel_notification_sent`,
`termination.turn_complete_observed`,
`termination.connection_terminal_observed`,
`termination.broker_settled`, `termination.summary_persisted`,
`termination.summary_persist_failed`, and `termination.intent_evicted`.

- [ ] **Step 6: Run model tests and commit**

```powershell
rustfmt --edition 2021 src/acp/termination_audit.rs src/acp/mod.rs
cargo test --features test-utils acp::termination_audit::tests -- --nocapture
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/acp/mod.rs src-tauri/src/acp/termination_audit.rs
git commit -m "feat(acp): add termination causality model"
```

Expected: focused tests pass and the commit contains only the four listed
files.

---

### Task 2: Durable Conversation Projection And Conditional Ordering

**Files:**
- Create: `src-tauri/src/db/migration/m20260721_000001_acp_termination_audit.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Modify: `src-tauri/src/db/entities/conversation.rs`
- Modify: `src-tauri/src/db/service/conversation_service.rs`
- Modify: `src-tauri/src/db/service/import_service.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/models/conversation.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Test: inline migration and conversation-service tests.

**Preflight:**

```powershell
git diff -- src-tauri/src/db/migration/m20260721_000001_acp_termination_audit.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/conversation.rs src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/models/conversation.rs src-tauri/src/commands/conversations.rs
```

**Interfaces:**
- Produces: nullable `conversation.last_termination_audit_json TEXT`.
- Produces:
  `persist_last_termination_audit<C: ConnectionTrait>(conn: &C, conversation_id: i32, candidate: &AcpTerminationSummaryV1) -> Result<TerminationSummaryWrite, DbError>`.
- Produces: optional `DbConversationSummary.last_termination_audit`.

- [ ] **Step 1: Write the failing migration and conditional-write tests**

The migration test must create a legacy conversation row, run `up`, and prove
the new column is null. Service tests must cover null-to-value, same-connection
cleanup update, older-incarnation rejection, malformed-existing-value
replacement, and a CAS retry.

```rust
fn summary(
    connection_id: &str,
    connection_started_at: &str,
    ownership_generation: u64,
) -> AcpTerminationSummaryV1 {
    let started_at = DateTime::parse_from_rfc3339(connection_started_at)
        .unwrap()
        .with_timezone(&Utc);
    AcpTerminationSummaryV1 {
        version: TERMINATION_AUDIT_VERSION,
        root_id: Uuid::parse_str("11111111-1111-4111-8111-111111111111")
            .unwrap(),
        final_request_id: Uuid::parse_str(
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap(),
        connection_id: connection_id.to_string(),
        action: AcpTerminationAction::Disconnect,
        source: AcpTerminationSource::Frontend,
        reason: AcpTerminationReason::ProviderUnmount,
        classification:
            AcpTerminationClassification::DisconnectBeforeTurnComplete,
        task_id: None,
        connection_status_at_request: ConnectionStatus::Prompting,
        active_prompt: true,
        connection_started_at: started_at,
        ownership_generation,
        turn_complete_event_seq: None,
        terminal_event_seq: Some(981),
        requested_at: started_at + Duration::minutes(1),
        observed_at: started_at + Duration::minutes(1),
    }
}

#[tokio::test]
async fn late_old_connection_cannot_replace_newer_termination_summary() {
    let db = test_helpers::fresh_in_memory_db().await;
    let folder = test_helpers::seed_folder(&db, "/tmp/termination-order").await;
    let row = create(&db.conn, folder, AgentType::Codex, None, None)
        .await
        .unwrap();

    let newer = summary("new-connection", "2026-07-21T08:00:00Z", 0);
    let older = summary("old-connection", "2026-07-21T07:00:00Z", 99);
    assert_eq!(
        persist_last_termination_audit(&db.conn, row.id, &newer)
            .await
            .unwrap(),
        TerminationSummaryWrite::Persisted
    );
    assert_eq!(
        persist_last_termination_audit(&db.conn, row.id, &older)
            .await
            .unwrap(),
        TerminationSummaryWrite::SkippedNewer
    );
    assert_eq!(
        get_by_id(&db.conn, row.id)
            .await
            .unwrap()
            .last_termination_audit,
        Some(newer)
    );
}
```

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cargo test --features test-utils m20260721_000001_acp_termination_audit -- --nocapture
cargo test --features test-utils last_termination_audit -- --nocapture
```

Expected: compilation fails because the migration, entity field, response
field, and persistence function do not exist.

- [ ] **Step 3: Add the migration and entity field**

Use the existing nullable-column pattern:

```rust
manager
    .alter_table(
        Table::alter()
            .table(Conversation::Table)
            .add_column(ColumnDef::new(Conversation::LastTerminationAuditJson).text())
            .to_owned(),
    )
    .await
```

Register the migration last in `Migrator::migrations()`. Add:

```rust
pub last_termination_audit_json: Option<String>,
```

to the SeaORM model. Set `last_termination_audit_json: Set(None)` in all four
explicit conversation ActiveModel constructors:

- `conversation_service::create`;
- `import_service::import_one`;
- `import_service::reimport_skips_a_delegation_child`;
- `ConnectionManager::fork_session`.

Set `last_termination_audit: None` in the `summary_child` Rust fixture in
`src-tauri/src/commands/conversations.rs`; it is the only direct
`DbConversationSummary` literal outside `conv_to_summary`.

Implement `down` with the exact inverse and extend the migration test to prove
the column is absent again afterward:

```rust
manager
    .alter_table(
        Table::alter()
            .table(Conversation::Table)
            .drop_column(Conversation::LastTerminationAuditJson)
            .to_owned(),
    )
    .await
```

- [ ] **Step 4: Implement decode and compare-and-swap persistence**

Add the typed response field:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub last_termination_audit:
    Option<crate::acp::termination_audit::AcpTerminationSummaryV1>,
```

Decode in `conv_to_summary`. Reject `version != 1` and malformed JSON with a
WARN containing only `conversation_id` and a stable code.

Implement a maximum-three-attempt CAS loop:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationSummaryWrite {
    Persisted,
    SkippedNewer,
}

pub async fn persist_last_termination_audit<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    candidate: &AcpTerminationSummaryV1,
) -> Result<TerminationSummaryWrite, DbError>;
```

Each attempt reads only `LastTerminationAuditJson`, decodes it, returns
`SkippedNewer` when `candidate.supersedes(current)` is false, serializes the
candidate, and updates with both `Id == conversation_id` and the exact prior
column value (or `IS NULL`). Do not bump `updated_at`. A zero-row update retries;
three consecutive conflicts return a fixed database error without embedding
the JSON.

- [ ] **Step 5: Run database tests and commit**

```powershell
rustfmt --edition 2021 src/db/migration/m20260721_000001_acp_termination_audit.rs src/db/migration/mod.rs src/db/entities/conversation.rs src/db/service/conversation_service.rs src/db/service/import_service.rs src/models/conversation.rs src/acp/manager.rs src/commands/conversations.rs
cargo test --features test-utils m20260721_000001_acp_termination_audit -- --nocapture
cargo test --features test-utils last_termination_audit -- --nocapture
git add src-tauri/src/db/migration/m20260721_000001_acp_termination_audit.rs src-tauri/src/db/migration/mod.rs src-tauri/src/db/entities/conversation.rs src-tauri/src/db/service/conversation_service.rs src-tauri/src/db/service/import_service.rs src-tauri/src/acp/manager.rs src-tauri/src/models/conversation.rs src-tauri/src/commands/conversations.rs
git commit -m "feat(db): persist latest ACP termination audit"
```

Expected: both focused test groups pass; existing rows decode with a null
summary.

---

### Task 3: Manager Admission And Typed Connection Controls

**Files:**
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Test: inline tests in those three files.

**Preflight:**

```powershell
git diff -- src-tauri/src/acp/session_state.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs
```

**Interfaces:**
- Produces: `ConnectionControl::Cancel(AcpTerminationRequest)` and `Disconnect(AcpTerminationRequest)`.
- Produces: `ConnectionManager::{cancel_with_audit, disconnect_with_audit}` during migration.
- Produces: `ConnectionManager::termination_intents() -> &TerminationIntentRegistry`.
- Consumes: `AcpTerminationCause` and `AcpTerminationCorrelation` from Task 1.

- [ ] **Step 1: Write failing admission-order and payload tests**

Add manager tests that hold the returned control receiver and assert:

```rust
let request_id = manager
    .disconnect_with_audit(
        "prompting-child",
        AcpTerminationCause::Frontend(FrontendTerminationReason::ProviderUnmount),
        AcpTerminationCorrelation::root(None),
    )
    .await
    .unwrap();
match control_rx.recv().await.unwrap() {
    ConnectionControl::Disconnect(request) => {
        assert_eq!(request.request_id, request_id);
        assert_eq!(request.root_id, request_id);
    }
    _ => panic!("expected typed disconnect"),
}
let intent = manager
    .termination_intents()
    .current("prompting-child")
    .expect("intent registered before control delivery");
assert!(intent.snapshot.active_prompt);
```

Cover cancel, missing connection, failed control send, duplicate cleanup,
state-snapshot counters, ownership metadata, and registry sharing through
`clone_ref`. Add a rebind test proving the backend-only state ownership fields
change with the `AgentConnection` fields.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cargo test --features test-utils termination_control -- --nocapture
cargo test --features test-utils termination_snapshot -- --nocapture
```

Expected: compilation fails because manager admission APIs and typed control
payloads do not exist.

- [ ] **Step 3: Add backend-only connection metadata to SessionState**

Add fields initialized by `SessionState::new`:

```rust
pub connection_started_at: DateTime<Utc>,
pub owner_operation_id: Option<String>,
pub ownership_generation: u64,
```

Use one captured `now` for `connection_started_at`, `last_activity_at`, and
`last_agent_activity_at`. In `rebind_connection_owner_window`, update
`owner_window_label`, `owner_operation_id`, and `ownership_generation` in both
`AgentConnection` and `SessionState`.

- [ ] **Step 4: Make ConnectionManager own and share the registry**

Add:

```rust
termination_intents: TerminationIntentRegistry,
```

to `ConnectionManager`. Initialize it in `new` and test constructors and clone
it in `clone_ref`. Add a read-only accessor for lifecycle/broker integration.

Implement one private snapshot builder that runs while the `SessionState` read
guard is held:

```rust
fn termination_snapshot(
    state: &SessionState,
    now: DateTime<Utc>,
) -> AcpTerminationStateSnapshot;
```

Use `state.turn_in_flight` for `active_prompt` and saturating non-negative
millisecond ages.

- [ ] **Step 5: Admit requests before removal or send**

Introduce typed methods while retaining temporary legacy shims until Task 6:

```rust
pub async fn cancel_with_audit(
    &self,
    db: &DatabaseConnection,
    conn_id: &str,
    cause: AcpTerminationCause,
    correlation: AcpTerminationCorrelation,
) -> Result<Uuid, AcpError>;

pub async fn disconnect_with_audit(
    &self,
    conn_id: &str,
    cause: AcpTerminationCause,
    correlation: AcpTerminationCorrelation,
) -> Result<Uuid, AcpError>;
```

For disconnect, hold the connection-map lock, acquire the state read lock,
capture/register the intent, and only then remove the map entry. Holding the
state read guard prevents the connection task from emitting its terminal state
before registration. Release all guards before awaiting the control send.

If the live map has no connection, call `admit_existing` before reporting
`ConnectionNotFound`. A matching registry entry means this is idempotent
cleanup after terminal observation: append the cleanup request, emit
`termination.duplicate`, and return its request id without sending control. If
both the live map and registry are absent, log a standalone
`outcome=connection_not_found` request without a snapshot and return the
existing error.

For cancel, register before continuation cleanup and before the control send.
On success record `termination.control_sent`. On send failure record
`termination.control_send_failed` and return `AcpError::ProcessExited` while
leaving the intent available.

The temporary existing `cancel`/`disconnect` methods delegate with
`LegacyUnspecified`; Task 6 removes them after all Rust producers are migrated.

- [ ] **Step 6: Carry requests through ConnectionControl**

Change:

```rust
pub enum ConnectionControl {
    SuspendForDelegation {
        continuation_id: String,
        parent_turn_generation: u64,
        reply: oneshot::Sender<Result<SuspensionAck, AcpError>>,
    },
    Cancel(AcpTerminationRequest),
    Disconnect(AcpTerminationRequest),
}
```

Update all production matches and direct test sends. Add one
`test_termination_request(action)` helper under `cfg(test)` so connection-loop
tests do not hand-build inconsistent ids.

At every receive site, record `termination.control_received` with the carried
request. After a successful `CancelNotification` send, record
`termination.cancel_notification_sent`. Never log the ACP response body.

- [ ] **Step 7: Run manager/connection tests and commit**

```powershell
rustfmt --edition 2021 src/acp/session_state.rs src/acp/connection.rs src/acp/manager.rs
cargo test --features test-utils termination_control -- --nocapture
cargo test --features test-utils termination_snapshot -- --nocapture
cargo test --features test-utils acp::manager::tests -- --nocapture
git add src-tauri/src/acp/session_state.rs src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs
git commit -m "feat(acp): propagate typed termination controls"
```

Expected: focused and manager module tests pass; concurrent Grok retry changes
remain present and are not reverted.

---

### Task 4: Proven Unrequested Exits And Lifecycle Persistence

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Test: inline tests in `connection.rs` and `lifecycle.rs`.

**Preflight:**

```powershell
git diff -- src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/broker.rs
```

**Interfaces:**
- Consumes: the shared `TerminationIntentRegistry` from Task 3.
- Produces:
  `DelegationBroker::cancel_by_child_connection_with_audit(&self, child_connection_id: &str, terminal_error: Option<&str>, termination: Option<&AcpTerminationSummaryV1>) -> ()`.
- Produces: terminal summary persistence without duplicate critical-lane writes.

- [ ] **Step 1: Write failing exit-proof and lifecycle tests**

Add connection tests for:

- typed disconnect received while prompting does not synthesize another root;
- both command/control lanes closing creates `control_channel_closed`;
- a residual transport failure creates `transport_closed`;
- a proven `AcpError::ProcessExited` records `process_exited` with only a stable
  code;
- a partial-spawn task abort records a terminal observation even though it
  cannot emit `Disconnected`.

Add lifecycle tests that send terminal Error followed by Disconnected through
the critical lane and assert one persistence call and one registry finalization.
Add a summary-persistence failure fixture proving broker settlement still runs.

```rust
#[tokio::test]
async fn dispatcher_terminal_pair_persists_one_audit_episode() {
    let db = test_helpers::fresh_in_memory_db().await;
    let folder_id = test_helpers::seed_folder(&db, "/tmp/terminal-audit").await;
    let conversation = conversation_service::create(
        &db.conn,
        folder_id,
        AgentType::Codex,
        None,
        None,
    )
    .await
    .unwrap();
    let manager = ConnectionManager::new();
    let requested_at = Utc::now();
    manager
        .termination_intents()
        .admit(
            "child-audit",
            AcpTerminationAction::Disconnect,
            AcpTerminationCause::Frontend(
                FrontendTerminationReason::ProviderUnmount,
            ),
            AcpTerminationCorrelation::root(None),
            AcpTerminationStateSnapshot {
                connection_status: ConnectionStatus::Prompting,
                conversation_id: Some(conversation.id),
                agent_type: AgentType::Codex,
                connection_started_at: requested_at
                    - chrono::Duration::minutes(10),
                event_seq: 980,
                active_prompt: true,
                pending_permission: false,
                active_tool_call_count: 1,
                background_outstanding: 0,
                last_activity_age_ms: 10,
                last_agent_activity_age_ms: 15,
                owner_window_label: "main".into(),
                owner_operation_id: None,
                ownership_generation: 0,
            },
            requested_at,
        );

    let (broker, driver) =
        stage_pending_delegation("child-audit", conversation.id).await;
    let bus = Arc::new(InternalEventBus::new(Arc::new(
        EventBusMetrics::default(),
    )));
    let dispatcher = tokio::spawn(lifecycle_subscriber_task(
        db.conn.clone(),
        manager.clone_ref(),
        bus.clone(),
        Some(broker),
    ));
    bus.send(Arc::new(EventEnvelope {
        seq: 981,
        connection_id: "child-audit".into(),
        payload: AcpEvent::Error {
            message: "transport closed".into(),
            agent_type: "codex".into(),
            code: Some("process_exited".into()),
            terminal: true,
        },
    }));
    bus.send(Arc::new(EventEnvelope {
        seq: 982,
        connection_id: "child-audit".into(),
        payload: AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected,
        },
    }));
    drop(bus);
    dispatcher.await.unwrap();
    driver.await.unwrap();

    let summary = conversation_service::get_by_id(&db.conn, conversation.id)
        .await
        .unwrap()
        .last_termination_audit
        .unwrap();
    assert_eq!(
        summary.classification,
        AcpTerminationClassification::DisconnectBeforeTurnComplete
    );
    assert_eq!(summary.reason, AcpTerminationReason::ProviderUnmount);
    assert!(manager
        .termination_intents()
        .current("child-audit")
        .is_none());
}
```

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cargo test --features test-utils unrequested_termination -- --nocapture
cargo test --features test-utils terminal_audit_episode -- --nocapture
```

Expected: assertions fail because clean/error exits are not connected to the
registry and lifecycle does not persist audit summaries.

- [ ] **Step 3: Pass the shared registry into the connection task**

Add `TerminationIntentRegistry` to `spawn_agent_connection` and thread it into
`run_connection` / `run_conversation_loop` without creating a second registry.
Register terminal intent exactly once per outer connection-task exit, after
`run_connection` resolves and before the first terminal Error/Disconnected (and
before parent/delegation cleanup). The trailing Disconnected must reuse that
episode rather than registering a second synthetic root. At that boundary:

- preserve an existing admitted request;
- call `register_unrequested_exit` with `ProcessExited` only when a stable
  process-exit signal exists;
- use `TransportClosed` for a proven ACP transport close;
- register `ControlChannelClosed` at each `ConversationInput::ChannelsClosed`
  boundary;
- use `LegacyUnspecified` only when none of those signals is provable.

Build the fallback snapshot from `SessionState` while holding its read lock.
Do not pass or log `AcpError::to_string()` through the audit module.
Do not call `observe_terminal` on these ordinary event-producing paths; the
lifecycle worker owns that transition after it receives the terminal event.
For a partial-spawn abort that cannot emit a lifecycle event, the manager is
the sole exception: it observes, logs, and finalizes the already-admitted
`ConnectionSetup` intent directly because no conversation is linked.

- [ ] **Step 4: Mark TurnComplete before broker handling**

At the beginning of `handle_internal_event`'s TurnComplete arm, before any
database or broker await:

```rust
manager.termination_intents().observe_turn_complete(
    &internal.connection_id,
    internal.seq,
    StableStopReason::from_wire(stop_reason),
    Utc::now(),
);
```

`StableStopReason::from_wire` maps only `end_turn`, `cancelled`, `refusal`,
`max_tokens`, `max_turn_requests`, `empty`, and `unknown`; every other string
becomes `Unknown` without retaining the raw value.

- [ ] **Step 5: Observe once, settle broker, then persist the refreshed summary**

Refactor the terminal branches in `connection_worker_loop` into one helper:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminationPersistenceOutcome {
    NotLinked,
    Persisted,
    SkippedNewer,
    Failed,
}

async fn persist_termination_summary_best_effort(
    db: &DatabaseConnection,
    conversation_id: Option<i32>,
    summary: &AcpTerminationSummaryV1,
) -> TerminationPersistenceOutcome {
    let Some(conversation_id) = conversation_id else {
        return TerminationPersistenceOutcome::NotLinked;
    };
    match persist_last_termination_audit(db, conversation_id, summary).await {
        Ok(TerminationSummaryWrite::Persisted) => {
            log_summary_persisted(
                summary,
                AcpTerminationSummaryWriteLogOutcome::Persisted,
            );
            TerminationPersistenceOutcome::Persisted
        }
        Ok(TerminationSummaryWrite::SkippedNewer) => {
            log_summary_persisted(
                summary,
                AcpTerminationSummaryWriteLogOutcome::SkippedNewer,
            );
            TerminationPersistenceOutcome::SkippedNewer
        }
        Err(_) => {
            log_summary_persist_failed(
                summary,
                AcpTerminationSummaryFailureCode::DatabaseWriteFailed,
            );
            TerminationPersistenceOutcome::Failed
        }
    }
}

async fn handle_terminal_audit(
    db: &DatabaseConnection,
    manager: &ConnectionManager,
    broker: Option<&Arc<DelegationBroker>>,
    connection_id: &str,
    event_seq: u64,
    stable_error_code: Option<StableExitCode>,
    terminal_error: Option<&str>,
) {
    let observation = manager.termination_intents().observe_terminal(
        connection_id,
        Some(event_seq),
        stable_error_code,
        AcpTerminationCause::LegacyUnspecified,
        None,
        Utc::now(),
    );
    let Some(observation) = observation else {
        return;
    };
    let initial_summary = observation.summary;
    let conversation_id = observation.snapshot.conversation_id;

    if let Some(broker) = broker {
        broker
            .cancel_by_child_connection_with_audit(
                connection_id,
                terminal_error,
                Some(&initial_summary),
            )
            .await;
    }

    let summary = manager
        .termination_intents()
        .current_summary(connection_id, initial_summary.root_id)
        .unwrap_or(initial_summary);
    let persistence = persist_termination_summary_best_effort(
        db,
        conversation_id,
        &summary,
    )
    .await;
    if persistence == TerminationPersistenceOutcome::Persisted {
        manager
            .termination_intents()
            .mark_persisted(connection_id, summary.root_id, Utc::now());
    }
    if persistence != TerminationPersistenceOutcome::Failed {
        manager
            .termination_intents()
            .finish(connection_id, summary.root_id);
    }
}
```

Keep the record through broker settlement so its idempotent cleanup can append
a child request even though the live connection has already left the manager
map. Refresh the summary only after that append, ensuring `final_request_id`
names the real final cleanup request. Persistence occurs after broker
settlement, so DB latency/failure cannot delay or reverse the parent result.
For unlinked connections, skip the DB write and finalize after broker work. For
persistence failure, emit ERROR and retain the intent for TTL eviction rather
than reporting success.

- [ ] **Step 6: Keep terminal Error/Disconnected idempotent**

Preserve `terminal_dispatched` in the per-connection worker and make
`observe_terminal` idempotent by terminal sequence/root. Critical-lane and
broadcast duplication must produce one summary, one broker settlement, and one
`termination.summary_persisted` record.

- [ ] **Step 7: Run lifecycle tests and commit**

```powershell
rustfmt --edition 2021 src/acp/connection.rs src/acp/manager.rs src/acp/lifecycle.rs src/acp/delegation/broker.rs
cargo test --features test-utils unrequested_termination -- --nocapture
cargo test --features test-utils terminal_audit_episode -- --nocapture
cargo test --features test-utils acp::lifecycle::tests -- --nocapture
git add src-tauri/src/acp/connection.rs src-tauri/src/acp/manager.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/broker.rs
git commit -m "feat(acp): persist terminal causality"
```

Expected: focused and lifecycle tests pass; the existing terminal-error detail
tests retain their pre-audit text when no provenance is supplied.

---

### Task 5: Broker Root Preservation And Stable Parent-Facing Provenance

**Files:**
- Modify: `src-tauri/src/acp/delegation/spawner.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Test: inline broker/spawner/manager tests.

**Preflight:**

```powershell
git diff -- src-tauri/src/acp/delegation/spawner.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/manager.rs
```

**Interfaces:**
- Produces: typed `ConnectionSpawner::cancel` and `disconnect` methods returning request ids.
- Produces: `SettleContext` teardown cause/correlation.
- Consumes: terminal summary from Task 4 for child-disconnect settlement.

- [ ] **Step 1: Write failing broker attribution tests**

Extend existing tests rather than replacing their connection-id assertions:

```rust
async fn accepted_running_task_with_handle_fixture(
    external_handle: &str,
) -> AcceptedFixture {
    let spawner = Arc::new(MockSpawner::new());
    spawner.queue_spawn(Ok("child-conn".into())).await;
    spawner.queue_send(Ok(accepted(42, Utc::now()))).await;
    let store = Arc::new(MockTaskStore::accept_any_running(42));
    let broker = broker_with_store(spawner.clone(), store);
    enable_delegation(&broker).await;
    let ack = broker
        .start_delegation(request_with_handle(
            1,
            "pt-accepted",
            external_handle,
        ))
        .await;
    assert_eq!(ack.status, TaskStatus::Running);
    AcceptedFixture {
        broker,
        spawner,
        task_id: ack.task_id.expect("accepted task id"),
        parent_id: "parent-conn".into(),
    }
}

#[tokio::test]
async fn external_handle_cancel_keeps_one_root_through_disconnect() {
    let fixture =
        accepted_running_task_with_handle_fixture("external-1").await;
    fixture
        .broker
        .cancel_by_external_handle("external-1", "host cancelled".into())
        .await;

    let cancels = fixture.spawner.cancel_audits.lock().await;
    let disconnects = fixture.spawner.disconnect_audits.lock().await;
    assert_eq!(
        cancels[0].cause,
        AcpTerminationCause::Broker(
            BrokerTerminationReason::ExternalHandleCancel,
        )
    );
    assert_eq!(
        disconnects[0].correlation.parent_request_id,
        Some(cancels[0].request_id)
    );
    assert_eq!(
        cancels[0].correlation.task_id.as_deref(),
        Some(fixture.task_id.as_str())
    );
}
```

Add parallel tests for:

- `cancel_task_by_id` -> `explicit_task_cancel`;
- parent connection teardown -> `parent_disconnect`;
- parent user stop -> `parent_cancel`;
- parent failed/abandoned turn -> `parent_turn_ended`;
- normal child completion -> `terminal_cleanup` without cancel;
- setup send failure -> `setup_failure_cleanup`;
- terminal persistence failure -> `terminal_persistence_failure_cleanup`;
- child terminal provenance remaining the root when broker issues duplicate
  cleanup.

- [ ] **Step 2: Run focused broker tests and verify RED**

```powershell
cargo test --features test-utils broker_termination_cause -- --nocapture
cargo test --features test-utils external_handle_cancel_keeps_one_root -- --nocapture
```

Expected: compilation fails because `ConnectionSpawner` does not carry audit
context and mocks do not record it.

- [ ] **Step 3: Make ConnectionSpawner teardown typed**

Change the trait to:

```rust
async fn cancel(
    &self,
    conn_id: &str,
    cause: AcpTerminationCause,
    correlation: AcpTerminationCorrelation,
) -> Result<Uuid, SpawnerError>;

async fn disconnect(
    &self,
    conn_id: &str,
    cause: AcpTerminationCause,
    correlation: AcpTerminationCorrelation,
) -> Result<Uuid, SpawnerError>;
```

The production implementation calls `cancel_with_audit` /
`disconnect_with_audit`. Keep the mock's existing `cancels` and `disconnects`
`Vec<String>` fields so current assertions remain valid, and add parallel
`cancel_audits` / `disconnect_audits` records containing connection id, cause,
correlation, and generated request id.

Use this exact mock record for both vectors:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationCallAudit {
    pub connection_id: String,
    pub cause: AcpTerminationCause,
    pub correlation: AcpTerminationCorrelation,
    pub request_id: Uuid,
}
```

- [ ] **Step 4: Put teardown provenance in SettleContext**

Add:

```rust
termination_cause: AcpTerminationCause,
termination_correlation: AcpTerminationCorrelation,
```

to `SettleContext`. Require every constructor/call site to choose a fixed cause.
Do not derive it from `message`, `error_code`, or task text.

When `cancel_turn` is true:

1. send cancel with the context cause;
2. capture the returned cancel request id;
3. compute the root as
   `ctx.termination_correlation.root_id.unwrap_or(cancel_request_id)`;
4. send disconnect with `Broker(TerminalCleanup)` and
   `AcpTerminationCorrelation::child_of(root_id, cancel_request_id, task_id)`.

When the child already terminated, send only the idempotent cleanup disconnect
as a child of `summary.final_request_id`. On persistence failure, use
`TerminalPersistenceFailureCleanup` as the cleanup cause.

- [ ] **Step 5: Append only stable provenance to the canceled message**

Keep `terminal_error` behavior unchanged, then append:

```rust
fn stable_termination_suffix(summary: Option<&AcpTerminationSummaryV1>) -> String {
    summary.map_or_else(String::new, |summary| {
        format!(
            " (source={}, reason={}, root_id={})",
            summary.source.as_str(),
            summary.reason.as_str(),
            summary.root_id
        )
    })
}
```

No task text, provider message, state snapshot, or terminal error enters this
suffix. Emit `termination.broker_settled` after the durable winner is known,
with fixed outcome/status fields and the same root id.

- [ ] **Step 6: Run broker/spawner tests and commit**

```powershell
rustfmt --edition 2021 src/acp/delegation/spawner.rs src/acp/delegation/broker.rs src/acp/manager.rs
cargo test --features test-utils broker_termination_cause -- --nocapture
cargo test --features test-utils acp::delegation::spawner::mock::tests -- --nocapture
cargo test --features test-utils cancel_by_child_connection -- --nocapture
git add src-tauri/src/acp/delegation/spawner.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/acp/manager.rs
git commit -m "feat(delegation): retain termination provenance"
```

Expected: focused tests pass, existing broker id/count assertions still pass,
and audit assertions prove the root is not overwritten by cleanup.

---

### Task 6: Classify Every Rust Producer And Remove Bare Manager APIs

**Files:**
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/automation/engine.rs`
- Modify: `src-tauri/src/auto_title/runner.rs`
- Modify: `src-tauri/src/document_translate/runner.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Modify: `src-tauri/src/web/handlers/acp.rs`
- Modify: `src-tauri/src/commands/conversation_popout.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/bin/codeg_server.rs`
- Modify: `src-tauri/Cargo.toml`
- Test: inline tests in the modified Rust modules.

**Preflight:**

```powershell
git diff -- src-tauri/Cargo.toml src-tauri/src/acp/manager.rs src-tauri/src/acp/connection.rs src-tauri/src/automation/engine.rs src-tauri/src/auto_title/runner.rs src-tauri/src/document_translate/runner.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/commands/conversation_popout.rs src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs
```

**Interfaces:**
- Replaces temporary `cancel_with_audit` / `disconnect_with_audit` with required typed `cancel` / `disconnect`.
- Removes every bare in-process cancel/disconnect call.
- Preserves old-web compatibility through an optional reason only at the Axum boundary.

- [ ] **Step 1: Add failing producer-mapping tests**

Add exact assertions for the following table:

| Producer | Action | Cause |
|---|---|---|
| backend idle sweep | disconnect | `BackendIdleSweep { idle_age_ms, timeout_ms }` |
| route fallback partial spawn | disconnect/abort | `ConnectionSetup(RouteFallbackCleanup)` |
| fatal bootstrap partial spawn | disconnect/abort | `ConnectionSetup(BootstrapFailureCleanup)` |
| agent-options probe | disconnect | `AgentProbe` |
| main/pop-out window teardown | disconnect | `Frontend(ProviderUnmount)` unless app is quitting |
| desktop/server quit | disconnect | `ApplicationShutdown` |
| automation completed turn | disconnect | `Automation(NormalCompletion)` |
| automation cancel | cancel | `Automation(ExplicitCancellation)` |
| automation pre-prompt/send failure | disconnect | `Automation(AdmissionFailure)` |
| automation post-admission failure | disconnect | `Automation(FailureCleanup)` |
| hidden title/translate success | disconnect | `InternalRunner(NormalCompletion)` |
| hidden title explicit cancel | disconnect | `InternalRunner(ExplicitCancellation)` |
| hidden runner identity/register/send failure | disconnect | `InternalRunner(AdmissionFailure)` |
| hidden runner stream/output failure | disconnect | `InternalRunner(FailureCleanup)` |

For backend idle, assert the measured age is at least the threshold and the
configured timeout is exact. For hidden runners, extend fake drivers to record
the cause without changing their existing disconnect-count assertions.

- [ ] **Step 2: Run producer tests and verify RED**

```powershell
cargo test --features test-utils termination_producer -- --nocapture
cargo test --features test-utils runner_cleanup_reason -- --nocapture
```

Expected: tests fail because the current producers call bare manager/driver
methods.

- [ ] **Step 3: Finalize typed manager API and internal manager paths**

Rename `cancel_with_audit` to `cancel` and `disconnect_with_audit` to
`disconnect`, then delete the temporary legacy shims. Update internal manager
paths:

- `sweep_idle` supplies measured age and timeout;
- `probe_agent_options` supplies `AgentProbe`;
- `teardown_unexposed_attempt` receives a connection-setup cause from each
  caller before aborting;
- `disconnect_by_owner_window` and
  `disconnect_by_owner_window_and_operation` accept an explicit cause;
- `disconnect_all` uses `ApplicationShutdown`;
- bulk operations call the same admission helper as single disconnect and
  preserve ownership CAS.

For a missing connection, create/log a standalone request with
`outcome=connection_not_found` only after `admit_existing` confirms there is no
terminal intent awaiting idempotent cleanup. Do not fabricate a snapshot or
registry entry for a genuinely unknown id.

- [ ] **Step 4: Classify automation and hidden runners**

Change runner driver methods to:

```rust
async fn disconnect(
    &self,
    conn_id: &str,
    reason: InternalRunnerTerminationReason,
) -> Result<(), AcpError>;
```

Compute the reason from the typed outcome before cleanup:

```rust
fn title_cleanup_reason(
    outcome: &Result<String, AutoTitleRunError>,
) -> InternalRunnerTerminationReason {
    match outcome {
        Ok(_) => InternalRunnerTerminationReason::NormalCompletion,
        Err(AutoTitleRunError::Cancelled) => {
            InternalRunnerTerminationReason::ExplicitCancellation
        }
        Err(
            AutoTitleRunError::Identity(_)
            | AutoTitleRunError::Registry(_)
            | AutoTitleRunError::Spawn(_),
        ) => InternalRunnerTerminationReason::AdmissionFailure,
        Err(_) => InternalRunnerTerminationReason::FailureCleanup,
    }
}
```

Add the equivalent exhaustive mapping for `DocumentTranslateError`. Preserve
the current cleanup timeout and directory-removal behavior.

Automation cancel must use the returned cancel request id as the parent
correlation for its following disconnect.

- [ ] **Step 5: Add typed desktop and web command boundaries**

Desktop commands require the enum:

```rust
pub async fn acp_disconnect(
    connection_id: String,
    reason: FrontendTerminationReason,
    manager: State<'_, ConnectionManager>,
) -> Result<(), AcpError> {
    manager
        .disconnect(
            &connection_id,
            AcpTerminationCause::Frontend(reason),
            AcpTerminationCorrelation::root(None),
        )
        .await
        .map(|_| ())
}
```

Use the same required reason for desktop cancel. Axum request structs use:

```rust
#[serde(default)]
pub reason: Option<FrontendTerminationReason>,
```

Map `None` to `LegacyUnspecified` and emit the registry's WARN. Serde rejects
unknown strings before the handler runs. Add deserialization tests for a
missing field, every known value, and an unknown value.

- [ ] **Step 6: Cover desktop and standalone-server shutdown**

Main-window teardown uses `ApplicationShutdown` when `APP_QUITTING` is set and
`Frontend(ProviderUnmount)` otherwise. Pop-out close uses
`Frontend(ProviderUnmount)` with its ownership CAS.

Enable Tokio signal support:

```toml
tokio = { version = "1", features = ["process", "io-util", "sync", "macros", "rt", "net", "rt-multi-thread", "time", "signal"] }
```

Run standalone Axum with a graceful shutdown future that listens for Ctrl-C
and Unix SIGTERM. After serving stops, call `disconnect_all` before returning
from `init_server`. Do not print or log the bearer token in the new path.

- [ ] **Step 7: Prove no bare Rust teardown remains**

Run from the repository root and inspect every result:

```powershell
rg -n "manager\.(cancel|disconnect)\(" src-tauri/src
rg -n "ConnectionControl::(Cancel|Disconnect)([^\(]|$)" src-tauri/src
rg -n "disconnect_by_owner_window|disconnect_all|sweep_idle" src-tauri/src
```

Expected: each manager call includes cause + correlation, each control variant
carries a request, and every bulk path has a fixed cause. `LegacyUnspecified`
appears only in `termination_audit.rs`, Axum missing-reason compatibility,
terminal fallback, and tests.

- [ ] **Step 8: Run Rust producer tests and commit**

```powershell
rustfmt --edition 2021 src/acp/manager.rs src/acp/connection.rs src/automation/engine.rs src/auto_title/runner.rs src/document_translate/runner.rs src/commands/acp.rs src/web/handlers/acp.rs src/commands/conversation_popout.rs src/lib.rs src/bin/codeg_server.rs
cargo test --features test-utils termination_producer -- --nocapture
cargo test --features test-utils runner_cleanup_reason -- --nocapture
cargo test --features test-utils web::handlers::acp::tests -- --nocapture
git add src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/src/acp/manager.rs src-tauri/src/acp/connection.rs src-tauri/src/automation/engine.rs src-tauri/src/auto_title/runner.rs src-tauri/src/document_translate/runner.rs src-tauri/src/commands/acp.rs src-tauri/src/web/handlers/acp.rs src-tauri/src/commands/conversation_popout.rs src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs
git commit -m "feat(acp): classify backend teardown producers"
```

Expected: all focused tests pass and the search in Step 7 has no unexplained
bare producer.

---

### Task 7: Frontend Reason Contract And Exact Lifecycle Attribution

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/contexts/acp-connections-context.test.tsx`

**Preflight:**

```powershell
git diff -- src/lib/types.ts src/lib/api.ts src/lib/tauri.ts src/lib/api.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
```

**Interfaces:**
- Produces: `FrontendTerminationReason` TypeScript union matching Rust.
- Produces: required `acpCancel(connectionId, reason)` and
  `acpDisconnect(connectionId, reason)` wrappers.
- Preserves: `AcpActions.disconnect(contextKey)` compatibility through a
  default `context_disconnect` reason.

- [ ] **Step 1: Write failing raw-wrapper payload tests**

Add both imports and assertions to `src/lib/api.test.ts`:

```typescript
it("sends typed ACP termination reasons unchanged", async () => {
  await acpCancel("connection", "user_stop")
  expect(mockTransport.call).toHaveBeenLastCalledWith("acp_cancel", {
    connectionId: "connection",
    reason: "user_stop",
  })

  await acpDisconnect("connection", "provider_unmount")
  expect(mockTransport.call).toHaveBeenLastCalledWith("acp_disconnect", {
    connectionId: "connection",
    reason: "provider_unmount",
  })
})
```

Add provider tests for owner teardown, reapply, replacement, connect-abandoned,
connect-superseded, provider unmount, frontend idle, disconnect-all, and cancel.
Use fake timers for the idle sweep and unmount the rendered provider for the
provider-unmount case.

- [ ] **Step 2: Run focused frontend tests and verify RED**

```powershell
pnpm test -- src/lib/api.test.ts src/contexts/acp-connections-context.test.tsx
```

Expected: assertions fail because the wrappers and context pass only a
connection id.

- [ ] **Step 3: Add the exact TypeScript reason union**

Add beside the ACP connection types:

```typescript
export type FrontendTerminationReason =
  | "user_stop"
  | "context_disconnect"
  | "provider_unmount"
  | "frontend_idle_timeout"
  | "connect_abandoned"
  | "connect_superseded"
  | "connection_replaced"
  | "disconnect_all"

export type AcpTerminationSource =
  | "frontend"
  | "backend_idle_sweep"
  | "connection_setup"
  | "broker"
  | "parent"
  | "automation"
  | "internal_runner"
  | "agent_probe"
  | "application"
  | "transport"
  | "process"
  | "control_channel"
  | "legacy"

export type AcpTerminationReason =
  | FrontendTerminationReason
  | "backend_idle_sweep"
  | "route_fallback_cleanup"
  | "bootstrap_failure_cleanup"
  | "terminal_cleanup"
  | "setup_failure_cleanup"
  | "terminal_persistence_failure_cleanup"
  | "explicit_task_cancel"
  | "external_handle_cancel"
  | "parent_cancel"
  | "parent_disconnect"
  | "parent_turn_ended"
  | "normal_completion"
  | "explicit_cancellation"
  | "admission_failure"
  | "failure_cleanup"
  | "agent_probe"
  | "application_shutdown"
  | "transport_closed"
  | "process_exited"
  | "control_channel_closed"
  | "legacy_unspecified"
```

Also mirror the diagnostic summary:

```typescript
export interface AcpTerminationSummaryV1 {
  version: 1
  root_id: string
  final_request_id: string
  connection_id: string
  action: "cancel" | "disconnect"
  source: AcpTerminationSource
  reason: AcpTerminationReason
  classification:
    | "turn_complete_before_disconnect"
    | "disconnect_before_turn_complete"
    | "disconnect_without_active_prompt"
    | "unrequested_terminal"
    | "ordering_unknown"
  task_id?: string | null
  connection_status_at_request: ConnectionStatus
  active_prompt: boolean
  connection_started_at: string
  ownership_generation: number
  turn_complete_event_seq?: number | null
  terminal_event_seq?: number | null
  requested_at: string
  observed_at: string
}
```

Add `last_termination_audit?: AcpTerminationSummaryV1 | null` to
`DbConversationSummary`. Do not render it.

- [ ] **Step 4: Require reasons in both raw wrappers**

Use the same signature in `api.ts` and `tauri.ts`:

```typescript
export async function acpCancel(
  connectionId: string,
  reason: FrontendTerminationReason
): Promise<void> {
  return getTransport().call("acp_cancel", { connectionId, reason })
}

export async function acpDisconnect(
  connectionId: string,
  reason: FrontendTerminationReason
): Promise<void> {
  return getTransport().call("acp_disconnect", { connectionId, reason })
}
```

The direct Tauri wrapper uses `invoke` with the same payload.

- [ ] **Step 5: Attribute every AcpConnectionsProvider call site**

Use exactly:

| Context call site | Reason |
|---|---|
| frontend idle timer | `frontend_idle_timeout` |
| provider cleanup effect | `provider_unmount` |
| replacing an owned connection with new parameters | `connection_replaced` |
| connection returned after explicit abandon | `connect_abandoned` |
| connection returned after a newer request superseded it | `connect_superseded` |
| public `disconnect(contextKey)` default | `context_disconnect` |
| `reapplyConfig` restart | `connection_replaced` |
| `disconnectAll` | `disconnect_all` |
| public `cancel(contextKey)` | `user_stop` |

Implement the context action as:

```typescript
const disconnect = useCallback(
  async (
    contextKey: string,
    reason: FrontendTerminationReason = "context_disconnect"
  ) => {
    pendingConnectRequestsRef.current.delete(contextKey)
    const conn = storeRef.current.connections.get(contextKey)
    if (!conn) {
      if (connectingKeysRef.current.has(contextKey)) {
        abandonedKeysRef.current.add(contextKey)
      }
      return
    }
    if (conn.isViewer) {
      teardownAttachSubscription(contextKey)
      reverseMapRef.current.delete(conn.connectionId)
      pendingUnmappedEventsRef.current.delete(conn.connectionId)
      lastActivityRef.current.delete(contextKey)
      dispatch({ type: "CONNECTION_REMOVED", contextKey })
      return
    }
    if (
      conn.conversationId != null &&
      isTransferringOut(conn.conversationId)
    ) {
      teardownAttachSubscription(contextKey)
      reverseMapRef.current.delete(conn.connectionId)
      pendingUnmappedEventsRef.current.delete(conn.connectionId)
      lastActivityRef.current.delete(contextKey)
      dispatch({ type: "CONNECTION_REMOVED", contextKey })
      return
    }
    await acpDisconnect(conn.connectionId, reason)
    reverseMapRef.current.delete(conn.connectionId)
    teardownAttachSubscription(contextKey)
    lastActivityRef.current.delete(contextKey)
    pendingUnmappedEventsRef.current.delete(conn.connectionId)
    dispatch({ type: "CONNECTION_REMOVED", contextKey })
  },
  [dispatch, teardownAttachSubscription]
)
```

Keep `AcpActionsValue.disconnect(contextKey)` as a one-argument public
interface; the implementation's optional second argument is internal. Change
`reapplyConfig` to call
`disconnect(contextKey, "connection_replaced")`. Keep the existing inline
viewer/transfer detach operations and their safety behavior unchanged.

- [ ] **Step 6: Update exact frontend expectations**

Representative assertions:

```typescript
expect(h.acpDisconnect).toHaveBeenCalledWith(
  "spawned-conn",
  "context_disconnect"
)
expect(h.acpDisconnect).toHaveBeenCalledWith(
  "spawned-conn",
  "connection_replaced"
)
expect(h.acpCancel).toHaveBeenCalledWith("spawned-conn", "user_stop")
```

Retain all tests proving viewers never call `acpDisconnect`.

- [ ] **Step 7: Search for missing raw-wrapper reasons**

```powershell
rg -n "acpDisconnect\(" src --glob '*.ts' --glob '*.tsx'
rg -n "acpCancel\(" src --glob '*.ts' --glob '*.tsx'
```

Expected: every raw wrapper call has a second argument. Calls to the context
action `disconnect(contextKey)` may rely on its intentional default.

- [ ] **Step 8: Run frontend tests and commit**

```powershell
pnpm exec prettier --write src/lib/types.ts src/lib/api.ts src/lib/tauri.ts src/lib/api.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
pnpm test -- src/lib/api.test.ts src/contexts/acp-connections-context.test.tsx
git add src/lib/types.ts src/lib/api.ts src/lib/tauri.ts src/lib/api.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit -m "feat(acp): classify frontend teardown requests"
```

Expected: focused tests pass and viewer teardown remains disconnect-free.

---

### Task 8: Session Diagnostics And Full Conversation-832 Regression

**Files:**
- Modify: `src-tauri/src/acp/session_info.rs`
- Modify: `src-tauri/src/commands/session_info.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Test: inline tests in those files.

**Preflight:**

```powershell
git diff -- src-tauri/src/acp/session_info.rs src-tauri/src/commands/session_info.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/broker.rs
```

**Interfaces:**
- Produces: optional `SessionInfo.last_termination_audit`.
- Produces: one stable human-readable termination line in
  `get_session_info` output.
- Produces: end-to-end regression proving provider unmount, backend idle, and
  transport exit remain distinct.

- [ ] **Step 1: Write failing session-info exposure tests**

Seed a conversation with a version-1 summary and resolve it through
`DbSessionInfoLookup`:

```rust
let info = lookup.resolve(conversation_id, 0).await;
let audit = info
    .last_termination_audit
    .expect("session diagnostics expose termination summary");
assert_eq!(audit.reason, AcpTerminationReason::ProviderUnmount);
assert_eq!(
    audit.classification,
    AcpTerminationClassification::DisconnectBeforeTurnComplete
);
```

Add a companion render test asserting:

```text
Last termination: frontend/provider_unmount
classification: disconnect_before_turn_complete
root_id: 11111111-1111-4111-8111-111111111111
```

The text must not contain owner label, task text, provider error, or paths.

- [ ] **Step 2: Write the full conversation-832 regression fixture**

Use the real `InternalEventBus`, lifecycle dispatcher, in-memory DB, manager
registry, and broker mock:

1. create parent and delegate conversation rows;
2. stage a running broker task for the child connection;
3. set child state to `Prompting` with recent agent activity;
4. admit `Frontend(ProviderUnmount)`;
5. emit terminal Disconnected without `TurnComplete`;
6. await broker canceled settlement and summary persistence;
7. assert every captured structured record uses the same root id;
8. repeat with `BackendIdleSweep`;
9. repeat without an admitted request but with `TransportClosed`.

Core assertions:

```rust
assert_eq!(provider.reason, AcpTerminationReason::ProviderUnmount);
assert_eq!(idle.reason, AcpTerminationReason::BackendIdleSweep);
assert_eq!(transport.classification, AcpTerminationClassification::UnrequestedTerminal);
assert_eq!(transport.source, AcpTerminationSource::Transport);
assert_ne!(provider.root_id, idle.root_id);
assert!(broker_message.contains("source=frontend"));
assert!(broker_message.contains("reason=provider_unmount"));
assert!(!broker_message.contains("task prompt text"));
```

- [ ] **Step 3: Run diagnostic/regression tests and verify RED**

```powershell
cargo test --features test-utils session_info_exposes_termination -- --nocapture
cargo test --features test-utils conversation_832_termination_causality -- --nocapture
```

Expected: session info lacks the field and the end-to-end fixture cannot yet
observe all three outcomes.

- [ ] **Step 4: Expose the typed summary in SessionInfo**

Add:

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub last_termination_audit: Option<AcpTerminationSummaryV1>,
```

Initialize it from `DbConversationSummary.last_termination_audit` in
`DbSessionInfoLookup`. Set it to `None` in not-found/default and listener test
fixtures.

In `render_session_summary_text`, render only source, reason, classification,
and root id. `structuredContent` already carries the complete typed object; no
tool schema change is required.

- [ ] **Step 5: Complete the regression and privacy assertions**

Add a tracing capture layer scoped to the regression test. Parse captured JSON
as structured values and assert:

- all approved event names use message `acp_termination`;
- request/terminal/broker/persistence events share `root_id`;
- provider unmount request is WARN because `active_prompt=true`;
- ordinary terminal cleanup is INFO;
- there are no keys named `prompt`, `response`, `task_text`, `tool_args`,
  `working_dir`, `environment`, `command_line`, `error_message`, or
  `stack_trace`.

- [ ] **Step 6: Run focused tests and commit**

```powershell
rustfmt --edition 2021 src/acp/session_info.rs src/commands/session_info.rs src/acp/delegation/companion.rs src/acp/delegation/listener.rs src/acp/lifecycle.rs src/acp/delegation/broker.rs
cargo test --features test-utils session_info_exposes_termination -- --nocapture
cargo test --features test-utils conversation_832_termination_causality -- --nocapture
cargo test --features test-utils render_session_result -- --nocapture
git add src-tauri/src/acp/session_info.rs src-tauri/src/commands/session_info.rs src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/lifecycle.rs src-tauri/src/acp/delegation/broker.rs
git commit -m "test(acp): cover termination causality diagnostics"
```

Expected: the fixture identifies the selected producer exactly, transport exit
never becomes user cancellation, and session references expose the durable
summary.

---

### Task 9: Repository-Wide Verification And Audit-Surface Review

**Files:**
- No planned modifications. A verification failure returns to the task that
  owns the affected literal path and uses that task's preflight, tests, and
  exact `git add` list.

**Interfaces:**
- Verifies desktop, server, MCP, frontend, migration, privacy, and static
  call-site contracts.

- [ ] **Step 1: Re-run static attribution and privacy searches**

```powershell
rg -n "LegacyUnspecified" src-tauri/src
rg -n "ConnectionControl::(Cancel|Disconnect)([^\(]|$)" src-tauri/src
rg -n "manager\.(cancel|disconnect)\(" src-tauri/src
rg -n "acpDisconnect\(" src --glob '*.ts' --glob '*.tsx'
rg -n "acpCancel\(" src --glob '*.ts' --glob '*.tsx'
rg -n "codeg_lib::acp::termination|termination\.(requested|duplicate|control_sent|control_send_failed|control_received|cancel_notification_sent|turn_complete_observed|connection_terminal_observed|broker_settled|summary_persisted|summary_persist_failed|intent_evicted)" src-tauri/src
```

Expected:

- legacy appears only at compatibility/fallback boundaries and tests;
- no unit control variant remains;
- every manager/raw frontend call is typed;
- all 12 log event names are present;
- no audit log helper accepts `String` metadata beyond approved opaque ids.

- [ ] **Step 2: Run formatting and frontend checks**

From repository root:

```powershell
pnpm eslint .
pnpm test
pnpm build
```

From `src-tauri/`:

```powershell
cargo fmt --all -- --check
```

Expected: every command exits zero.

- [ ] **Step 3: Run desktop Rust checks**

From `src-tauri/`:

```powershell
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Expected: every command exits zero.

- [ ] **Step 4: Run server and MCP Rust checks**

```powershell
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command exits zero. The MCP companion remains stderr-only and
does not become an authoritative termination logger.

- [ ] **Step 5: Inspect the final diff and commit verification fixes**

```powershell
git status --short
git diff --check
git diff --stat
git log -9 --oneline
```

Confirm no concurrent Grok-retry/chat-channel work was reverted or accidentally
staged. Do not make or stage fixes from this step: return to the owning task,
apply the fix there, rerun that task's focused test and exact commit step, then
restart Task 9. If no fixes are required, do not create an empty commit.

---

## Self-Review

- Spec coverage: typed causes, all cancel/disconnect producers, request/root
  chaining, pre-teardown snapshots, all 12 log events, severity, bounded
  eviction, unrequested exits, broker settlement, conditional DB projection,
  session-reference diagnostics, privacy, compatibility, and conversation 832
  each map to an explicit task and test.
- File boundaries: the new module owns only the pure audit model/registry;
  database ordering stays in conversation service; lifecycle owns persistence;
  producer modules only choose fixed causes.
- Type consistency: manager, control variants, spawner, broker, lifecycle, DB,
  Rust responses, and TypeScript use the same action/source/reason/summary
  names. Request ids are `Uuid` throughout and wire summaries serialize them as
  strings.
- Failure behavior: control-send errors remain visible, logging is
  non-blocking, persistence failure never reverses teardown or broker
  settlement, and stale events cannot replace a newer connection summary.
- Placeholder scan: every implementation step names concrete files, interfaces,
  commands, expected outcomes, and fixed error behavior.
