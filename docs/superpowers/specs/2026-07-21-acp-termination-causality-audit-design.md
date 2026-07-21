# ACP Termination Causality Audit Design

**Date:** 2026-07-21

**Status:** Approved

**Scope:** All ACP cancel and disconnect paths in desktop and server modes,
including frontend lifecycle cleanup, user cancellation, frontend and backend
idle sweeps, broker teardown, parent cascades, automation, internal runners,
agent probes, application shutdown, transport closure, and process exit.

## Problem

ACP teardown currently converges on APIs that carry only a connection id:

```rust
ConnectionManager::disconnect(connection_id)
ConnectionControl::Disconnect
```

The resulting log line identifies the victim but not the initiator, reason, or
state that made the teardown eligible. A delegation child that disappears
before `TurnComplete` is consequently persisted as a generic cancellation:

```text
canceled: child session ended without TurnComplete
```

Conversation 832 demonstrates the cost. The final-review child was actively
working and had announced that it was about to write its report. Its Codex
turn then received `turn_aborted(reason=interrupted)`, the child connection
ended before Codeg observed `TurnComplete`, and the broker recorded `canceled`.
The parent did not call `cancel_delegation`. Existing durable state cannot
distinguish a frontend unmount, an idle sweep, a transport teardown, or another
disconnect producer, so the expensive review had to be started again.

This is an observability gap. The audit must identify the exact teardown path
without logging user content or turning normal teardown into a blocking
dependency.

## Goals

- Attribute every ACP cancel and disconnect request to a typed source and
  reason.
- Correlate the first destructive trigger, subsequent cleanup actions, the
  connection terminal event, broker settlement, and durable projection.
- Capture the connection state at the destructive boundary, especially
  whether a prompt was active and whether `TurnComplete` was observed.
- Persist a compact latest termination summary on the conversation so a
  `codeg://session/<id>` investigation does not depend on retained log files.
- Write the full causality chain to the existing daily JSON logs at INFO or
  higher, retained for 30 files by default.
- Classify an unrequested terminal event as a transport/process-side exit
  instead of silently inventing a caller.
- Supply the structured termination provenance required by reusable
  delegation runs and interrupted-run recovery.
- Keep teardown best-effort: audit logging or summary persistence failure must
  never prevent cancellation or disconnection.

## Non-goals

- A general-purpose event-sourcing system for ACP state.
- Retaining every teardown forever in the database.
- A new audit-log UI or changes to normal conversation rendering.
- Logging prompts, model responses, tool arguments/results, workspace paths,
  environment variables, credentials, provider errors, or process command
  lines.
- Automatically retrying or continuing interrupted delegation runs. This
  design produces the provenance consumed by the separate reusable-session
  design; it does not implement that recovery policy.
- Changing which paths are currently allowed to cancel or disconnect. Any
  behavioral defect exposed by the new evidence is fixed separately with its
  own regression test.

## Alternatives

### 1. Add free-form logs at each call site

Rejected. Call sites would use inconsistent fields, new paths could omit the
log, and a final `disconnect connection=<id>` still could not be joined
reliably to broker settlement. Free-form reasons also risk capturing user or
provider text.

### 2. Propagate a typed termination context and persist a compact projection

Selected. The type system requires every in-process caller to name its source
and reason. A correlation id follows the request through the connection and
lifecycle layers. Detailed events remain in the existing structured file log,
while one bounded summary remains on the conversation.

### 3. Store an append-only database audit journal

Deferred. It preserves complete history after file rotation but adds an
unbounded table, retention jobs, query APIs, and privacy surface. The current
need is satisfied by a latest summary plus 30-day detailed logs. The stable
record schema can become a journal row later without changing producers.

## Core Model

### Typed cause

Add a dedicated module, `acp::termination_audit`, containing secret-free types.
Callers cannot provide arbitrary metadata or reason text.

```rust
pub enum AcpTerminationAction {
    Cancel,
    Disconnect,
}

pub enum AcpTerminationCause {
    Frontend(FrontendTerminationReason),
    BackendIdleSweep { idle_age_ms: u64, timeout_ms: u64 },
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
```

The nested reason enums use stable `snake_case` serialization. Required
frontend reasons are:

- `user_stop`
- `context_disconnect`
- `provider_unmount`
- `frontend_idle_timeout`
- `connect_abandoned`
- `connect_superseded`
- `connection_replaced`
- `disconnect_all`

Required broker reasons are:

- `terminal_cleanup`
- `setup_failure_cleanup`
- `terminal_persistence_failure_cleanup`
- `explicit_task_cancel`
- `external_handle_cancel`

Required parent reasons are `parent_cancel`, `parent_disconnect`, and
`parent_turn_ended`. Automation and internal-runner reasons distinguish normal
completion, explicit cancellation, admission failure, and failure cleanup.

`LegacyUnspecified` is accepted only at backward-compatible transport
boundaries. Internal Rust APIs and current TypeScript wrappers require a typed
cause. Receiving the legacy value emits a WARN so omissions remain visible.

### Request and root correlation

Every destructive request has its own `request_id` and belongs to one
`root_id` causality chain:

```rust
pub struct AcpTerminationRequest {
    pub version: u8, // 1
    pub request_id: Uuid,
    pub root_id: Uuid,
    pub parent_request_id: Option<Uuid>,
    pub action: AcpTerminationAction,
    pub cause: AcpTerminationCause,
    pub requested_at: DateTime<Utc>,
    pub task_id: Option<String>,
}
```

The first cancel/disconnect request uses its own id as `root_id`. Cleanup caused
by that request creates a child request with a new `request_id`, preserves the
same `root_id`, and points `parent_request_id` at the preceding request. For
example, `frontend/user_stop` is the root cause and a later
`broker/terminal_cleanup` disconnect is a cleanup stage. The cleanup must not
replace the root cause in durable state.

An unexpected connection terminal event with no registered request creates a
synthetic root request whose typed cause is `TransportClosed`,
`ProcessExited`, or `ControlChannelClosed` when the runtime can prove one of
those signals. Otherwise it uses `LegacyUnspecified`; it never guesses a
frontend or policy source.

### State snapshot

At request admission, before removing the connection from the manager, capture:

```rust
pub struct AcpTerminationStateSnapshot {
    pub connection_status: ConnectionStatus,
    pub conversation_id: Option<i32>,
    pub agent_type: AgentType,
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
```

The snapshot contains no transcript, path, tool payload, or free-form error.
`owner_window_label` is a Codeg-controlled identifier. The snapshot is logged
in full but the database projection retains only fields needed to classify the
termination.

## Lifecycle Architecture

### Intent registry

`ConnectionManager` owns a bounded `TerminationIntentRegistry` keyed by
connection id. It survives removal of the live connection long enough for the
lifecycle subscriber to observe the terminal event.

The registry stores the causality chain, state snapshot, and event markers:

- first destructive request;
- latest cleanup request;
- control send/receive state;
- cancel notification state;
- `TurnComplete` event sequence and stop reason, when observed;
- terminal `Disconnected`/`Error` event sequence and stable error code;
- persistence state.

The first destructive root wins. A duplicate request logs a duplicate event and
may append a cleanup child, but it cannot overwrite root provenance. Entries
are removed after terminal persistence. Incomplete entries are time-bounded and
evicted on later registry operations so a missing terminal event cannot leak
memory.

### Control propagation

`ConnectionControl::Cancel` and `ConnectionControl::Disconnect` carry the
typed request rather than being unit variants. Manager APIs require the cause:

```rust
cancel(db, connection_id, cause, correlation) -> Result<Uuid, AcpError>
disconnect(connection_id, cause, correlation) -> Result<Uuid, AcpError>
```

Broker, automation, runners, probes, idle sweeps, and commands all use named
constructors. The broker's `ConnectionSpawner` cancel/disconnect methods also
accept correlation context so broker teardown remains in the same root chain.

The frontend wrappers require a `FrontendTerminationReason`. Desktop invoke
and web JSON handlers deserialize the fixed enum. Missing reasons from an old
web client map to `LegacyUnspecified` for compatibility; unknown strings are
rejected.

### Terminal observation

The connection loop records receipt of the control before sending the ACP
`CancelNotification`. It does not claim that the agent acknowledged the cancel
unless the protocol produces a corresponding event.

The lifecycle subscriber marks `TurnComplete` with its internal event sequence.
When `Disconnected` or a terminal `Error` arrives, it consumes the registered
intent and records the terminal sequence. This yields an evidence-based
classification:

- `turn_complete_before_disconnect`: expected teardown after a terminal turn;
- `disconnect_before_turn_complete`: destructive teardown interrupted a turn;
- `disconnect_without_active_prompt`: connection cleanup outside a turn;
- `unrequested_terminal`: transport/process/control-channel exit;
- `ordering_unknown`: legacy or missing event metadata.

The broker receives this typed provenance when `cancel_by_child_connection`
settles a delegation. Its parent-facing message may include only stable fields:

```text
canceled: child session ended without TurnComplete
(source=frontend, reason=provider_unmount, root_id=<uuid>)
```

No arbitrary error detail is added by the new audit path.

## Structured Log Contract

Use the dedicated tracing target `codeg_lib::acp::termination`. Every event has
the stable message `acp_termination` plus structured fields. Event names are:

1. `termination.requested`
2. `termination.duplicate`
3. `termination.control_sent`
4. `termination.control_send_failed`
5. `termination.control_received`
6. `termination.cancel_notification_sent`
7. `termination.turn_complete_observed`
8. `termination.connection_terminal_observed`
9. `termination.broker_settled`
10. `termination.summary_persisted`
11. `termination.summary_persist_failed`
12. `termination.intent_evicted`

All records include `connection_id`, `request_id`, `root_id`, `action`,
`source`, and `reason` when available. Conversation id, task id, agent type,
event sequences, state snapshot values, and cleanup parent id are typed fields.

Severity rules:

- INFO: requested actions, expected terminal cleanup, successful settlement,
  and persistence.
- WARN: a destructive request while prompting, disconnect before
  `TurnComplete`, unrequested terminal, legacy unspecified cause, duplicate
  destructive request, or stale intent eviction.
- ERROR: control-send failure, terminal-summary persistence failure, or an
  invariant violation in correlation.

Desktop and server binaries already write INFO JSON logs daily and retain 30
files by default. `codeg-mcp` remains stderr-only; the authoritative lifecycle
events are emitted by the host process, not the companion.

## Durable Summary

### Current conversation projection

Add nullable `conversation.last_termination_audit_json`. The JSON is a typed,
versioned `AcpTerminationSummaryV1`:

```json
{
  "version": 1,
  "root_id": "uuid",
  "final_request_id": "uuid",
  "connection_id": "opaque-id",
  "action": "disconnect",
  "source": "frontend",
  "reason": "provider_unmount",
  "classification": "disconnect_before_turn_complete",
  "task_id": "optional-task-id",
  "connection_status_at_request": "prompting",
  "active_prompt": true,
  "turn_complete_event_seq": null,
  "terminal_event_seq": 981,
  "requested_at": "2026-07-21T07:41:43Z",
  "observed_at": "2026-07-21T07:41:43Z"
}
```

The summary is the latest termination episode, not a history. A conditional
write compares connection ownership generation and observation time so a late
event from an old connection cannot overwrite a newer incarnation. Cleanup
requests in the same `root_id` update the episode but retain the root source and
reason.

Conversation detail/session-reference responses expose this optional typed
summary for diagnostics. Existing UI does not render it.

### Reusable delegation runs

The approved reusable-session design adds authoritative per-run termination
audit fields to `delegation_task_runs`. When that table exists, delegation
settlement writes the same `AcpTerminationSummaryV1` to the run and projects the
latest run onto `conversation.last_termination_audit_json` in the same
transaction. Until then, the conversation projection is authoritative.

This stable shape lets interrupted-run recovery allow only typed unexpected
transport/process/session interruptions. Explicit user or parent cancellation,
policy rejection, and `LegacyUnspecified` remain ineligible for automatic
recovery.

## Error Handling

- Teardown never waits on log-file I/O; the existing non-blocking appender owns
  flushing.
- Registry insertion occurs before connection removal and control send.
- A failed control send is logged and returned through the existing error path;
  the registered intent remains available to explain a later terminal event.
- Summary persistence runs in the lifecycle worker. Failure emits ERROR but
  does not undo broker settlement or keep the child process alive.
- Serialization failure is an invariant error because only typed fields are
  serializable. It produces ERROR and leaves the prior summary intact.
- A missing connection records `termination.requested` plus
  `outcome=connection_not_found`; it cannot fabricate a state snapshot.
- Duplicate and cleanup requests are idempotent and preserve the root cause.
- A missing reason from an old client maps to `LegacyUnspecified`; unknown or
  malformed values are rejected at the transport boundary.

## Privacy And Security

The audit allowlist contains only fixed enums, booleans, counters, timestamps,
and opaque Codeg ids. Specifically forbidden:

- prompt or response text;
- task text or unbounded task previews;
- tool arguments or results;
- working directories and file paths;
- environment variables, command lines, and credentials;
- provider error messages and stack traces;
- arbitrary frontend reason strings.

Stable internal error codes may be recorded. Existing unrelated logs are not
expanded by this design. Typed constructors and private record fields prevent
callers from attaching ad hoc metadata.

## Validation

### Pure model tests

- Every source/reason variant has stable snake-case serialization.
- Invalid source/reason combinations cannot be constructed through public
  APIs.
- Summary serialization contains no free-form field.
- Root/child request chains preserve the first cause.
- A duplicate request cannot replace the root cause.

### Manager and connection tests

- Cancel and disconnect register intent before removing/sending control.
- Control variants carry the same request and root ids.
- User stop followed by broker cleanup remains rooted at user stop.
- Frontend lifecycle, frontend idle, backend idle, replacement, automation,
  runner, probe, and shutdown paths supply their exact typed cause.
- Backend idle records measured age and configured timeout.
- A prompting-state disconnect logs WARN with the request snapshot.
- A missing connection and a failed control send retain diagnosable outcomes.

### Lifecycle and broker tests

- `TurnComplete` before disconnect classifies expected cleanup.
- Disconnect before `TurnComplete` classifies interruption.
- A terminal event with no intent becomes an unrequested transport/process
  termination and never a user cancellation.
- Lifecycle summary persistence is non-blocking with respect to broker
  settlement failure handling.
- Broker cancellation output includes only stable source/reason/root id.
- Late terminal events from an old ownership generation cannot overwrite the
  current conversation summary.
- Desktop critical-lane/broadcast duplication produces one idempotent summary.

### Frontend and transport tests

- The TypeScript cancel/disconnect wrappers require a typed reason.
- Every current `acpDisconnect` call site passes the expected reason.
- Desktop invoke and web handlers preserve the reason unchanged.
- Old web requests without a reason become `legacy_unspecified` and WARN.
- Unknown reason strings are rejected.

### Regression fixture

Model conversation 832 with a synthetic active Codex child:

1. child is `Prompting` and has recent agent activity;
2. a selected teardown producer disconnects it;
3. no `TurnComplete` is delivered;
4. broker settles canceled;
5. the summary names the exact source/reason and
   `disconnect_before_turn_complete`;
6. all detailed records share the same root id;
7. changing the producer to transport exit yields `unrequested_terminal`, not
   the selected teardown reason.

The test must cover at least frontend provider unmount and backend idle sweep so
the two leading hypotheses for conversation 832 are mechanically
distinguishable.

## Rollout

- Existing rows keep a null summary and are treated as unknown legacy
  termination provenance.
- No backfill guesses causes from `conversation.status` or
  `delegation_error_code`.
- New backend events use version 1. Readers ignore unknown future fields and
  reject unsupported future versions for recovery decisions.
- INFO remains the default level, so the audit is captured without asking the
  user to enable DEBUG.
- The existing `CODEG_LOG_MAX_FILES` override continues to control detailed
  file retention.

## Acceptance Criteria

- Every current in-process cancel/disconnect path supplies a typed source and
  reason; no bare internal teardown API remains.
- A `codeg://session/<id>` lookup can return the latest termination summary.
- Searching JSON logs by `root_id` reconstructs request through durable
  settlement without inspecting model transcripts.
- Unexpected termination is distinguishable from user stop, parent cascade,
  idle sweep, and normal broker cleanup.
- A cleanup stage never overwrites its initiating root cause.
- A late old-incarnation event cannot overwrite a newer summary.
- Audit failure never blocks or reverses teardown.
- No newly emitted or persisted audit field contains user content, paths,
  environment data, credentials, command lines, or free-form errors.
- Conversation 832's regression fixture identifies the selected producer
  exactly and proves that transport exit remains a separate classification.
