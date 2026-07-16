# Codeg Delegation Route Reliability Design

## Status

Approved in conversation on 2026-07-16. This specification turns the findings
in
`docs/superpowers/plans/2026-07-16-codex-harness-toolcall-subagent-latency.md`
into a Codeg product-layer design for Codex, Grok, CodeBuddy, and Claude Code.

The implementation plan has not been written yet. This document defines the
behavioral contract that the plan must preserve.

## Problem

The measured Codex sessions do not show a general ACP tool-call performance
regression. Ordinary `exec` calls are comparable to native Codex, and both
harnesses pay roughly the same multi-agent startup cost. The visible failure is
instead a lifecycle and control-plane problem:

- a parent can see both its platform's native sub-agent tools and Codeg's
  `codeg-mcp` delegation tools;
- short bounded waits are easy for the model to interpret as task failure even
  though the child is still running;
- an optional child can remain `running` without a sufficiently useful progress
  signal, encouraging the parent to cancel and replace it;
- a process restart can leave a persisted delegation child looking
  `in_progress` after no live process can possibly complete it; and
- changing routing settings while a connection is live can otherwise produce
  a process whose launch arguments and exposed MCP tools disagree.

The product therefore needs a single route decision per connection, a hard
mutual-exclusion contract between native and Codeg creation tools, and a
non-destructive way to identify suspiciously quiet children.

## Goals

- Expose no more than one sub-agent creation control plane in a supported
  session, and expose the selected one whenever its underlying capability is
  available.
- Default supported root sessions to Codeg routing when Codeg delegation is
  enabled.
- Preserve main-task availability when a root session cannot establish Codeg
  routing.
- Preserve Codeg ownership for Codeg-created children.
- Make an accepted Codeg task queryable through a durable terminal state.
- Distinguish a quiet child from a failed child without automatically canceling
  it.
- Keep route selection configurable globally and overridable per root session.
- Apply route changes only at a connection boundary and surface stale running
  sessions clearly.
- Keep the current nesting depth semantics unchanged.

## Non-Goals

- Modifying the upstream Codex, Grok, CodeBuddy, or Claude Code products.
- Making native sub-agents run through `DelegationBroker`.
- Providing Codeg-owned cancel, resume, or terminal guarantees for native
  sub-agents.
- Redesigning delegation profiles, agent defaults, completed-result cache
  limits, or the child conversation tree.
- Adding a new nesting quota or changing the meaning of `depth_limit`.
- Claiming hard route exclusion for Agent types whose native creation surface
  has not been verified.
- Automatically switching route, reconnecting, or canceling work after a
  connection has started.

## Supported Scope

The managed route contract applies to these four Agent types:

- Codex
- Grok
- CodeBuddy
- Claude Code

Other Agent types keep their current delegation behavior in this change. Their
sessions do not show the managed route selector and are not included in the
hard mutual-exclusion acceptance claim. A Codeg delegation targeting one of
those Agent types also keeps the existing launch behavior; adding an adapter
for it is a separate, evidence-driven extension.

## Alternatives Considered

### Product route plus lifecycle contract

Selected. Codeg resolves one immutable route plan, adapts each platform at its
launch boundary, gates the companion's delegation feature from that plan, and
owns the durable lifecycle only for Codeg tasks.

This is the only approach that can enforce mutual exclusion without depending
on model compliance.

### Prompt-only guidance

Rejected. Telling the model to prefer one tool family still leaves both
creation surfaces callable. Prompt behavior can drift by model, version, and
context pressure, so it is not a control-plane guarantee.

### Always use platform-native sub-agents

Rejected. It avoids duplicate tools but gives up Codeg's cross-agent routing,
shared child-session tree, profiles, durable task identity, and common status
protocol.

### Fail every connection when Codeg routing is unavailable

Rejected for root sessions because the primary objective is task-completion
reliability. It remains the correct behavior for a managed Codeg-created child,
where falling back would create an unowned native subtree.

## Core Invariants

For every managed connection, at most one creation entry is exposed. The normal
route states are:

```text
Codeg route:  native creation suppressed AND codeg delegation exposed
Native route: native creation unchanged  AND codeg delegation hidden
```

A connection may deliberately have zero creation entries when the user has
disabled native tools, the Codeg master switch is off for a forced child, or a
Codeg companion becomes unavailable after launch. Those states are explicit
and never cause the other route to be enabled in place.

The forbidden state is:

```text
native creation exposed AND codeg delegation exposed
```

The route is immutable from process launch through connection teardown. A
configuration save may make the connection stale, but it cannot mutate the
running route.

`native` means "Codeg permits only the platform-native route." It does not mean
"Codeg forcibly enables native sub-agents." A user or administrator's existing
native deny setting remains authoritative.

## Route Model

The backend introduces the following logical model. Exact Rust module layout
may be adjusted by the implementation plan, but these fields and meanings are
normative.

```rust
#[serde(rename_all = "snake_case")]
enum DelegationRoutePolicy {
    Codeg,
    Native,
}

#[serde(rename_all = "snake_case")]
enum DelegationRouteSource {
    ForcedChild,
    SessionOverride,
    GlobalDefault,
    FeatureDisabled,
    SafeFallback,
}

struct DelegationRoutePlan {
    requested: DelegationRoutePolicy,
    effective: DelegationRoutePolicy,
    source: DelegationRouteSource,
    native_suppression: NativeSuppressionPlan,
    expose_codeg_delegation: bool,
    degraded_reason: Option<RouteDegradedReason>,
    fingerprint: String,
}
```

The plan is computed once before `build_agent` and stored on both
`AgentConnection` and the client-visible session snapshot. Process arguments,
session metadata, companion features, connection reuse, logs, and stale
calculation consume the same plan. None independently re-resolves the policy.

### Resolution order

For a managed Agent type:

1. A Codeg-created child is forced to `codeg`.
2. A root session's explicit override wins when present.
3. Otherwise the global policy applies; its default is `codeg`.

The existing `delegation.enabled` setting remains the master Codeg delegation
switch:

- a root connection resolves to `native` with source `feature_disabled` while
  the switch is off; `requested` still records the session/global selection,
  while `effective=native` records what can actually launch;
- an already-created Codeg child never changes to native merely because the
  switch was later turned off; after reconnect it still suppresses native
  creation but does not expose a new Codeg delegation entry; and
- the Broker cannot create a new child while the switch is off, matching the
  current kill-switch behavior.

Turning the switch off does not cancel an already accepted task. It prevents
new Broker creation and marks an incompatible running root route stale.

The global and session route settings apply only to managed Agent types.

## Platform Adapter Contract

Only creation entry points are suppressed. Result/status tools that also serve
ordinary background commands remain available.

| Platform | Native creation surface | `codeg` suppression | `native` behavior |
| --- | --- | --- | --- |
| Codex | `spawn_agent`, `wait_agent`, `interrupt_agent` | `features.multi_agent=false` | Do not force `true`; preserve user config |
| Grok | `spawn_subagent`, `get`, `wait`, `kill` | root flag `--no-subagents` | Omit the flag |
| CodeBuddy | `Agent`, `Task`, background Agent/Teams creation | `--disallowedTools Agent Task` | Add no Codeg deny entries |
| Claude Code | `Agent`/`Task` creation | merge `Agent`, `Task` into ACP `disallowedTools` | Add no Codeg deny entries |

### Codex

The Rust connection layer sets the connection-scoped adapter contract
`CODEX_ACP_MULTI_AGENT=0` only for a Codeg route. The vendored adapter consumes
that contract in both runtimes:

- CLI runtime adds `-c features.multi_agent=false` to every new and resumed
  `codex exec` launch; and
- App Server runtime deep-merges `features.multi_agent=false` into the
  `thread/start.config` object without replacing unrelated `features` keys.

The environment variable is absent on a native route. Codeg never injects
`features.multi_agent=true`, because that would override a user safety choice.

The adapter change lives in the vendored Codeg copy of `codex-acp`; it does not
require an upstream Codex CLI change.

### Grok

The route adapter inserts `--no-subagents` with Grok's other root flags and
before the ACP subcommand. The resulting ordering is:

```text
grok --no-auto-update [--always-approve] --no-subagents agent stdio
```

`--always-approve` remains controlled solely by the existing permission
setting. A native route omits only `--no-subagents`.

### CodeBuddy

The Codeg route builds:

```text
codebuddy --disallowedTools Agent Task --acp
```

If Codeg later gains another source of disallowed CodeBuddy tools, the adapter
must form a stable, de-duplicated union. A native route contributes no deny
entries. `TaskOutput` is not disabled because it also observes ordinary
background commands.

### Claude Code

For `session/new`, `session/load`, and `session/resume`, the Codeg route merges:

```json
{
  "claudeCode": {
    "options": {
      "disallowedTools": ["Agent", "Task"]
    }
  }
}
```

into the existing ACP `_meta`. The merge preserves
`claudeCode.emitRawSDKMessages`, terminal metadata, adapter metadata, and any
existing user deny list. `TaskOutput` and `TaskStop` remain available. A native
route leaves `disallowedTools` unchanged.

## Atomic Plan Application

Route setup follows this order:

```text
load global setting and session override
  -> determine managed root/child policy
  -> verify platform suppression capability
  -> verify codeg-mcp binary when Codeg delegation is required
  -> produce one immutable DelegationRoutePlan
  -> build Agent process arguments/environment
  -> initialize ACP session metadata
  -> inject codeg-mcp with plan-derived features
```

`inject_codeg_mcp` must no longer decide delegation exposure only from a fresh
Broker config snapshot. It receives the plan's `expose_codeg_delegation` bit.
The existing hot settings for `feedback`, `ask`, and `sessions` are snapshotted
at launch as today and combined independently.

This ordering ensures a Codeg root can fall back before a process with native
creation disabled is launched. It also prevents a suppression error from
leaving Codeg delegation exposed.

## Capability and Version Handling

The pinned versions verified for this design are:

- Codex CLI `0.144.1`
- Grok `0.2.98`
- CodeBuddy `2.118.2`
- Claude Code `2.1.205`
- Claude ACP wrapper `0.58.1`

Adapters advertise suppression support only for protocol/version shapes covered
by contract tests. The normal pinned distribution path must not execute an
extra `--help` probe on every connection. A custom executable or incompatible
major version that cannot satisfy the adapter contract is reported as
unsupported and enters the root/child failure policy below.

Updating a pinned Agent or wrapper version requires updating and passing its
route-adapter contract tests.

## Failure and Degradation

### Root connection

When a root requested `codeg` but Codeg cannot prove the launch contract before
process start, Codeg discards the partial plan and builds a fresh native plan:

- no native suppression is applied;
- the companion omits `delegation`;
- `requested=codeg`, `effective=native`, and `source=safe_fallback` are exposed;
- a stable degraded reason is recorded; and
- fallback happens at most once.

Pre-launch fallback reasons include:

- `native_suppression_unsupported`
- `native_suppression_invalid`
- `companion_binary_unavailable`
- `agent_mcp_unsupported`

This is a visible safe fallback, not a silent success.

If a route-specific unsupported-option/config error or companion initialization
error occurs after process spawn but before the ACP session is exposed as
connected, Codeg tears that attempt down and may perform the same one-time
native fallback. The error must be positively classified as route-specific.
Authentication failures, provider failures, missing SDKs, and generic process
exits retain their original error and never trigger a native retry.

### Managed Codeg child

A Codeg-created child targeting one of the four managed Agent types cannot
fall back to native. If the Codeg route cannot be established, Broker creation
returns `route_unavailable` before an accepted task is registered. If failure
occurs after registration began, the task reaches `failed` rather than staying
`running`.

This prevents a topology such as Codeg root -> Codeg child -> unobserved native
grandchild from being created as an implicit fallback.

### Failure after launch

The route never changes after launch. If `codeg-mcp` fails to initialize or
exits later:

- native creation remains suppressed;
- the session becomes `delegation_unavailable`;
- ordinary Agent work continues when the platform permits it; and
- the user may explicitly reconnect using native routing.

Codeg does not re-enable native tools, reconnect automatically, or cancel an
active task in response.

## Global and Session Configuration

`DelegationSettings` gains:

```text
route_policy: codeg | native            default: codeg
stalled_after_seconds: integer          default: 300
```

`stalled_after_seconds` is clamped to `60..=3600`. The existing fields retain
their current defaults, including `enabled=false`, `depth_limit=1`, and
`completed_cache_max_mb=512`.

Persistence adds these `app_metadata` keys:

```text
delegation.route_policy
delegation.stalled_after_seconds
```

Malformed values fall back to product defaults, matching the current settings
loading contract.

The `conversation` table gains:

```text
delegation_route_override  TEXT NULL  -- null | codeg | native
```

The migration uses a database check constraint where SQLite migration support
permits it, and backend validation remains mandatory on every write.

Only root conversations may set the override. Child conversations display the
forced route as read-only, and backend mutation rejects a child override even
if a stale client attempts it.

For a fresh conversation, the connect request carries the selected override so
the route is available before an Agent process exists. The first conversation
row creation persists it in the same transaction as the rest of the row. For a
persisted conversation, the backend-loaded row is authoritative; the frontend
payload cannot silently replace it.

## Connection Reuse and Staleness

`DelegationRoutePlan.fingerprint` becomes an explicit component of the
connection's immutable spawn configuration. It is not inferred indirectly from
Agent environment variables because Grok, CodeBuddy, and Claude apply route at
different boundaries.

The fingerprint includes the managed Agent type, requested and effective
policies, suppression plan, delegation exposure, adapter contract version, and
degraded reason. It excludes the preference source, so changing from inherited
`codeg` to an explicit `codeg` override does not make an otherwise identical
connection stale.

Connection behavior is:

- attaching by an already-known connection id keeps the existing route and
  does not resolve settings again;
- a session-id dedup lookup reuses an existing connection only when Agent,
  working directory, external session id, and effective route are compatible;
- a route mismatch returns `session_route_conflict` with the existing
  connection id instead of launching a second process for the same external
  session; and
- an explicit reconnect disconnects the existing connection and then resolves
  a new plan.

The frontend handles `session_route_conflict` during refresh by attaching the
supplied existing connection and showing its stale route. Only an explicit
reconnect action tears that connection down and retries the launch.

`ConfigStaleKind` gains `DelegationRoute`. Global route saves recompute the
effective route per live root connection because session overrides differ.
Session override saves recompute only the associated live connection. Forced
children are unaffected by global or root override changes.

Changing `delegation.enabled` also recomputes route staleness for managed root
connections. Changing only `stalled_after_seconds` applies live and does not
mark a route stale.

A change emits `SessionConfigStale` and updates the snapshot-recoverable stale
state. It never mutates process arguments, ACP metadata, companion features, or
the route plan. Reverting to the launch-time effective route clears route
staleness.

When more than one config surface is stale, the existing single visible stale
kind uses this priority:

```text
terminal shell > delegation route > agent/model config
```

The route fingerprint remains independently stale even when another kind has
display priority, so reverting one surface reveals any remaining stale kind.

## Nesting Contract

The existing nesting behavior is retained:

- default `depth_limit=1`;
- allowed range `1..=8`;
- value `2` permits root -> child -> grandchild;
- a Codeg child receives delegation injection again when enabled; and
- Broker calculates the persisted parent chain and rejects an over-depth
  creation before spawning another connection.

No new per-depth route, quota, timeout, or automatic cancel is introduced.

## Durable Codeg Task Lifecycle

The authoritative task states remain:

```text
running -> completed | failed | canceled
```

`unknown` is a scoped query result, not a lifecycle state. `stalled` is not a
task state.

### Persistence fields

Delegate conversation rows gain:

```text
delegation_task_status   TEXT NULL  -- running | completed | failed | canceled
delegation_error_code    TEXT NULL
delegation_started_at    DATETIME NULL
delegation_finished_at   DATETIME NULL
```

`ConversationStatus` remains the conversation/sidebar status and is not used as
the sole delegation truth after this migration. This avoids mapping every
non-canceled historical row to `completed` and losing a failed task's error
code after the in-memory result cache is evicted.

Legacy delegate rows are backfilled as follows:

| Conversation status | Delegation task status |
| --- | --- |
| `in_progress` | `running` |
| `pending_review` or `completed` | `completed` |
| `cancelled` | `canceled` |

Non-delegate rows keep all four fields null.

### Accepted boundary

Broker returns a `running` accepted acknowledgement only after:

1. native/Codeg route validation succeeded;
2. the child connection was created;
3. the child conversation row and delegation link were persisted; and
4. `delegation_task_status=running` plus `delegation_started_at` were persisted;
   and
5. the child's first prompt was successfully enqueued.

Failure before this boundary returns either a setup failure or an immediate
terminal report, never a `running` acknowledgement. A row that was already
created during setup is settled or removed by that failure path. Failure after
the boundary must settle the registered task.

### Terminal transition

Terminal persistence uses a conditional `running -> terminal` database update.
Completion, child failure, parent teardown, and explicit cancellation may race,
but only the first terminal transition wins. Repeated callbacks are idempotent
and return the already-persisted terminal report.

On the normal writable-database path, the terminal write records
`delegation_finished_at` and any stable error code before publishing the
terminal notification. The in-memory completed cache, `result_notify`, parent
tool metadata, frontend event, and child teardown are then driven from that
winning result.

A transient database write failure receives bounded retry with backoff. If the
write still cannot be persisted, Broker must not leave the parent waiting: it
publishes an in-memory `failed/persistence_error` terminal report, tears the
child down, and retains a background retry record that attempts to persist that
failure while the process remains alive. If the process exits before recovery,
startup reconciliation converts the still-running row to
`failed/host_restarted`. Durable terminal-state guarantees therefore assume the
SQLite database is eventually writable; permanent storage failure is surfaced
as an explicit failure rather than an indefinitely running task.

If completed text has left the bounded in-memory cache, status still comes from
the task fields and points the caller to the child conversation for full output.

### Restart reconciliation

During application startup, after migrations and before delegation requests are
accepted, every delegate row still marked `running` is settled as:

```text
delegation_task_status = failed
delegation_error_code = host_restarted
delegation_finished_at = startup time
conversation.status = cancelled
```

ACP child processes are owned by the previous Codeg process and cannot safely
be assumed resumable. This reconciliation prevents a pre-restart task from
remaining permanently running. Cold-load metadata injection and DB status
fallback must read the new task fields.

## Soft Watchdog

Task status and health observation are separate:

```text
TaskStatus:       running | completed | failed | canceled
TaskObservation: active | stalled | waiting_input
```

Only a running task has an observation.

### Activity source

`SessionState` gains a dedicated `last_agent_activity_at`. The inbound Agent
event boundary refreshes it for every ACP `session/update` that advances the
child turn, including text/thinking deltas, tool starts/updates, and plan
updates. Adapter-specific raw progress messages count only when they advance
the same live transcript or tool state. This is an explicit inbound-event
mark, not a side effect of applying every Codeg `AcpEvent`.

The timestamp is initialized when the child's first prompt is successfully
enqueued, so a newly accepted but initially silent child receives the full
watchdog interval.

It is not refreshed by:

- frontend tab keepalive;
- a status query from the parent;
- watchdog scans;
- route/stale UI events; or
- repeated rendering of the same latest reply.

The existing general `last_activity_at` remains unchanged for connection idle
sweeping.

### Observation calculation

For a running task, precedence is:

1. a pending permission or Codeg question produces `waiting_input`;
2. otherwise, silence at least `stalled_after_seconds` produces `stalled`;
3. otherwise the task is `active`.

A centralized supervisor examines running Broker tasks and their child session
snapshots every 15 seconds and also wakes immediately on task/activity/config
notifications. It records the last emitted observation per task and emits only
on a transition. A new Agent activity event returns a stalled task to `active`.
`stalled_since` is the logical threshold instant
`last_agent_activity_at + stalled_after_seconds`, not the later scan time.

Changing the watchdog threshold applies live because it changes observation
only. It does not mark the connection stale and cannot alter routing or task
status.

### Non-destructive guarantee

The watchdog never calls:

- `cancel_delegation`;
- a platform-native interrupt/kill tool;
- child disconnect;
- route fallback;
- connection reconnect; or
- a terminal database transition.

`stalled` therefore always remains `status=running` until a real lifecycle
event occurs.

## Status and Wait Protocol

The existing `wait_ms` wire shape remains compatible and is documented as
three modes:

| Mode | Wire form | Return condition |
| --- | --- | --- |
| `snapshot` | omit `wait_ms` | return current reports immediately |
| `supervised` | positive `wait_ms` | terminal, observation transition, or bounded deadline |
| `terminal` | `wait_ms=0` | terminal only; no timeout |

Positive waits retain the current 60-second per-call ceiling. They return a
running snapshot at the deadline; this is a successful status result, not task
failure.

A supervised call returns immediately when any requested task is already
terminal, stalled, or waiting for input. It parks only while every requested
task is currently `running/active`. After receiving an actionable non-active
observation, the tool guidance tells the parent to surface or handle it, or use
terminal wait when the result is still required, rather than tight-looping the
same supervised call.

Batch waits preserve input order and return when any requested task reaches the
mode's condition. A completed item is never held behind a running sibling. The
caller narrows subsequent requests to unfinished task ids.

`DelegationTaskReport` adds optional running-only fields:

```text
observation
last_agent_activity_at
stalled_since
```

`cancel_delegation(reason=timeout)` remains non-canceling and returns stable
guidance to continue waiting. Canceling an MCP status request closes only that
wait; it never changes the underlying task. Explicit `taskfail`, `usercancel`,
or `others` reasons retain their current cancel behavior.

## Event and Snapshot Contract

The live connection snapshot adds:

```text
delegation_route:
  requested
  effective
  source
  managed
  degraded_reason?
  delegation_available
```

The route snapshot is immutable except for `delegation_available` changing to
false when a post-launch companion failure is observed. The effective route
itself does not change.

Active delegation snapshots and events add the observation fields. A new
observation-change event updates the existing card without synthesizing a
terminal `DelegationCompleted` event. Snapshot reattachment recovers the same
state if the live transition event was missed.

Terminal events continue to use the existing started/completed fanout, but the
persisted task fields become the cold-load source of truth.

## Native Route Observation

Codeg does not make native tasks Broker tasks. It builds a read-only
`DelegationActivityView` projection for route-aware UI grouping and local
latency metrics:

```text
origin                 codeg | native
authoritative          boolean
platform               AgentType
task_id                string?
operation              spawn | status | wait | cancel | unknown
observed_status         running | completed | failed | canceled | unknown
started_at              timestamp?
updated_at              timestamp?
finished_at             timestamp?
```

Codeg Broker events populate a complete `authoritative=true` view. Native
normalizers populate `authoritative=false` only from signals actually emitted
by the platform:

| Platform | Native signals recognized for the projection |
| --- | --- |
| Codex | `spawn_agent`, `wait_agent`, `list_agents`, `interrupt_agent` |
| Grok | `spawn_subagent`, `get`, `wait`, `kill` |
| CodeBuddy | `Agent`/`Task` calls and background task notifications |
| Claude Code | `Agent`/`Task`, `TaskOutput`, `TaskStop`, and matching raw SDK task messages |

When no stable task id or terminal signal exists, the projection keeps the
field unknown instead of inventing lifecycle state. The original tool call
continues to render normally.

The native platform remains authoritative for creation, waiting, completion,
and cancellation. Codeg does not expose a second cancel button that calls the
Broker, does not promise startup reconciliation for native work, and does not
convert a native wait timeout into a Codeg task failure.

This gives the UI a common origin label and timing vocabulary without creating
a second control plane.

## User Interface

### Global settings

The existing Multi-agent General tab adds:

- a `Codeg` / `Native` segmented control for the managed-Agent default; and
- a numeric soft-watchdog threshold in seconds.

The route control is disabled while `delegation.enabled` is off, with the
effective behavior shown as native. The existing depth and cache controls keep
their semantics.

### Session override

A managed root conversation's session menu provides:

```text
Inherit global
Codeg
Native
```

Saving an override persists immediately. For a running connection it shows the
existing restart-to-apply banner with stale kind `delegation_route`; it does not
switch tools in place. A child conversation shows `Codeg (inherited)` as
read-only.

### Route and task feedback

The normal route does not add a persistent warning badge. UI feedback appears
when action or explanation is useful:

- Codeg-to-native fallback shows the reason and effective route;
- a missing post-launch companion shows delegation unavailable;
- a stale route shows reconnect-to-apply;
- a running card may show active, waiting for input, or stalled; and
- stalled text never uses failed/canceled visual treatment.

All new strings are added to the ten existing locale files.

## Error Taxonomy

Stable codes introduced or formalized by this design are:

| Code | Meaning |
| --- | --- |
| `route_unavailable` | a managed Codeg child could not establish its required route |
| `route_degraded` | a root requested Codeg and safely started native instead |
| `session_route_conflict` | a live external session exists with an incompatible route |
| `native_suppression_unsupported` | the adapter/version cannot enforce native suppression |
| `native_suppression_invalid` | the adapter produced or received an invalid suppression configuration |
| `agent_mcp_unsupported` | the managed Agent cannot receive the required companion MCP entry |
| `companion_binary_unavailable` | `codeg-mcp` was absent during preflight |
| `delegation_unavailable` | the Codeg route exists but delegation became unavailable after launch |
| `host_restarted` | startup reconciliation settled an orphaned running task |
| `persistence_error` | task settlement could not be durably written after bounded retry |

Human-readable messages may be localized in the UI. Wire codes and structured
log values remain stable English identifiers.

## Observability

Structured logs include, without prompt text or credentials:

- `connection_id`, `conversation_id`, `agent_type`;
- requested/effective route, source, managed flag, and degraded reason;
- native suppression adapter and application result;
- companion delegation exposure;
- task id, child conversation id, lifecycle status, and observation;
- wait mode, requested wait, wall time, and return reason; and
- explicit cancel reason and terminal winner.

Recommended local metrics are:

- route selections and safe fallbacks by Agent type;
- suppression failures;
- accepted-to-terminal rate;
- task completion/failure/cancel counts;
- time to terminal;
- stalled episodes and stalled-to-active recoveries;
- snapshot/supervised/terminal wait counts and durations; and
- explicit cancel count, separated from MCP status-request cancellation.

An invariant counter for
`native_creation_exposed && codeg_delegation_exposed` must remain zero for the
four managed platforms.

## Security and Safety

- Platform controls are built as structured argv, environment, config, or ACP
  metadata; no shell command string is constructed from route values.
- Claude and CodeBuddy denies are additive and de-duplicated.
- Native routing never overrides a user's deny configuration to force tools on.
- Companion feature gating is enforced in both `tools/list` and `tools/call`.
- Route and observation payloads contain no API keys, token values, or prompt
  bodies.
- Per-launch companion authentication and token revocation remain unchanged.
- Unknown or malformed persisted policy values cannot produce mixed routing;
  they fall back to the product default before plan construction.

## Implementation Boundaries

The implementation plan should keep these responsibilities separate:

### Route resolver

A focused delegation route module owns policy resolution, source selection,
fallback construction, adapter capability, and fingerprinting. It does not
spawn processes or inject MCP servers.

### Platform launch adapters

The ACP connection layer applies the already-resolved suppression plan to
process argv/environment or session `_meta`. Codex's two runtime translations
remain inside the vendored adapter.

### Companion feature selection

`inject_codeg_mcp` combines the immutable route decision with the independent
feedback/ask/session settings. It does not choose a route.

### Lifecycle store and supervisor

Broker lifecycle persistence owns accepted/terminal transitions. The soft
supervisor reads child activity and emits observations but has no cancel or
terminal capability.

### Settings and session UI

Global settings own the default and watchdog threshold. Conversation commands
own root overrides. The connection snapshot is the authority for the currently
effective route.

## Testing Strategy

### Route resolver tests

Cover every managed Agent type across:

- root global `codeg` and `native`;
- session override precedence;
- `delegation.enabled=false`;
- forced Codeg child inheritance;
- supported and unsupported suppression capability;
- companion present and absent;
- root safe fallback; and
- child `route_unavailable` rejection.

Every result asserts the hard invariant, source, degradation, companion feature
bit, and stable fingerprint behavior.

### Platform adapter tests

- Codex CLI new/resume arguments contain exactly one
  `features.multi_agent=false` override on Codeg and none on native.
- Codex App Server deep-merges the same setting without dropping other config.
- Grok root flags precede `agent stdio` in every permission mode.
- CodeBuddy emits a stable de-duplicated `Agent Task` deny and preserves
  `--acp` parsing.
- Claude new/load/resume requests preserve existing `_meta` while merging the
  deny list.
- Native plans never inject a Codeg-created enable or deny override.

### Companion and mutual-exclusion tests

For all four Agent types:

- Codeg plan means suppression present and delegation feature present;
- native plan means suppression absent and delegation feature absent;
- root fallback means suppression absent and delegation feature absent; and
- a disabled delegation tool is absent from `tools/list` and rejected by
  `tools/call`.

### Connection tests

Cover matching-route reuse, incompatible-route conflict, browser reattachment,
explicit reconnect, global stale refresh, per-session stale refresh, revert
clearing, and forced-child immunity to root/global route changes.

### Database and lifecycle tests

Cover migration/backfill, root override validation, accepted-boundary rollback,
every terminal source, terminal races, idempotent replay, cache eviction DB
fallback, failed error-code recovery, and startup orphan reconciliation.

### Watchdog and wait tests

Use a controllable clock to cover active, threshold crossing, waiting input,
recovery, one event per transition, live threshold changes, and the guarantee
that no watchdog path reaches cancel/disconnect/terminal methods.

Cover immediate snapshot, bounded supervised waits, terminal waits, observation
wakeups, batch-any semantics, peer-close, and non-canceling timeout guidance.

### Frontend tests

Cover global controls, session override states, child read-only behavior, route
fallback, delegation unavailable, stale banner, snapshot recovery, and distinct
active/stalled/waiting-input card rendering in all relevant adapters.

### Verification commands

After focused tests, run:

```bash
pnpm eslint .
pnpm test
pnpm build

cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Vendored `codex-acp` tests covering both runtimes must also run through its
existing package test command identified during implementation planning.

## Acceptance Criteria

1. Every managed connection exposes at most one sub-agent creation route; an
   explicit zero-entry unavailable/disabled state never activates the other
   route in place.
2. The forbidden mixed-route invariant remains zero in unit, integration, and
   pinned-platform smoke tests.
3. Supported roots default to Codeg when delegation is enabled and can select
   native globally or per session.
4. A Codeg routing preflight failure keeps a root usable through visible native
   fallback and never silently falls back for a managed Codeg child.
5. Route changes never mutate a live connection; they produce stale state and
   apply only after explicit reconnect.
6. `depth_limit` retains its current `1..=8` semantics and Codeg nesting works
   when the configured depth allows it.
7. With an eventually writable SQLite database, every accepted Codeg task can
   be recovered as running or a persisted terminal state, including after
   cache eviction and host restart; storage failure returns an explicit
   `persistence_error` instead of hanging.
8. A quiet task becomes `running/stalled`, not failed or canceled, and activity
   can restore it to `running/active`.
9. Bounded wait expiry and MCP status-request cancellation never cancel the
   underlying task.
10. Native tasks remain platform-owned; Codeg observation cannot accidentally
    invoke Broker lifecycle or cancellation.
11. Focused tests and the full frontend, desktop, server, and companion checks
    pass.

## Rollout and Rollback

The existing `delegation.enabled=false` default means installations that have
never enabled Codeg delegation do not change their effective root behavior.
For installations where delegation is already enabled, the four managed Agent
types adopt the new default Codeg route; this behavior change must be called out
in release notes.

Database additions are nullable or backfilled and remain readable by the new
code when routing is disabled. Existing `wait_ms` calls remain wire-compatible.
Other Agent types remain on the previous path.

Operational rollback is available without schema rollback:

- set the global route to `native` to stop exposing Codeg delegation on new
  managed connections; or
- disable delegation entirely.

Running connections retain their launch route until explicitly reconnected,
including during rollback. Code removal may leave the additive database columns
in place safely; destructive migration rollback is not required.
