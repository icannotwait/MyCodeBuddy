# Tool Execution Watchdog Design

Date: 2026-07-22

Status: Approved in conversation on 2026-07-22

## Summary

Add a host-owned execution lease for every foreground ACP tool call. The lease
tracks semantic progress rather than transport liveness, warns after 10 minutes
without progress, grants a full 10-minute user-visible grace period, and then
cancels the narrowest resource the host can identify. Cancellation escalates
from a specific terminal, delegation, or MCP request to the current ACP turn,
and disconnects the ACP connection only as a final convergence fallback.

Keep the existing 300-second delegation soft watchdog as an earlier,
observe-only child-health signal. It remains incapable of canceling, settling,
disconnecting, or changing routes.

## Incident Evidence

Conversations 1018 and 1026 both remained `in_progress` while Grok waited for
foreground terminal commands that had launched the Rust `parent_cancel` test
filter. The corresponding test processes remained alive for hours with no CPU
progress. Both session JSONL files were valid and ended at `tool_call`; neither
had produced a final output to truncate.

The incident exposed two independent unbounded waits:

- `TerminalRuntime::wait_for_terminal_exit` has no natural execution deadline.
- Grok `use_tool` calls had previously waited 100 minutes before the provider's
  own timeout returned control.

The existing delegation soft watchdog did not recover either root session. It
only marked running Broker child tasks as `stalled`, and deliberately had no
destructive capability.

## Goals

- Cover foreground ACP tool calls for every supported agent, not only Grok.
- Detect lack of semantic progress without treating keepalive or polling as
  progress.
- Warn the user before any automatic destructive action.
- Allow unlimited explicit 10-minute extensions.
- Cancel the narrowest owned resource and preserve a reusable ACP connection
  whenever possible.
- Guarantee that a timed-out turn does not remain durably `in_progress`.
- Keep desktop Tauri and server/Web behavior equivalent through shared backend
  cores and the existing transport abstraction.
- Preserve the existing delegation soft-watchdog behavior and settings.
- Avoid leaking terminal commands, environment variables, or MCP arguments in
  events, logs, notifications, or metrics.

## Non-Goals

- Inferring progress from CPU use, process existence, or transport heartbeat.
- Automatically retrying timed-out tools.
- Persisting live execution leases across application restarts.
- Replacing the ACP idle sweep, delegation lifecycle, or Broker terminal-state
  model.
- Adding provider-specific Grok timeout behavior.
- Guaranteeing request-level cancellation for third-party MCP servers that do
  not expose a cancellable request handle.
- Treating output truncation as a timeout. Truncation and progress tracking are
  separate concerns.

## Confirmed Product Decisions

| Area | Decision |
| --- | --- |
| Scope | All agents' foreground ACP tool calls |
| Primary architecture | Manager-level `ToolExecutionLeaseRegistry` |
| Progress clock | Semantic tool progress only |
| Delegation early signal | Keep current observe-only 300-second soft watchdog |
| Warning threshold | 600 seconds without semantic progress |
| Grace period | 600 seconds after warning publication |
| User extension | Reset grace to 600 seconds; unlimited repetitions |
| Automatic action | Cancel narrowest resource, then escalate if convergence fails |
| Automatic retry | Never |
| Untracked-turn fallback | Warn at 1,800 seconds, cancel turn after another 600 seconds |
| Persistence | In-memory leases only; terminal outcome still converges through existing lifecycle |
| Settings storage | Existing `app_metadata`; no schema migration |

## Relationship to Existing Watchdogs

Codeg has multiple unrelated timeout mechanisms. They must remain distinct.

### Delegation soft watchdog

`delegation.stalled_after_seconds` remains 300 seconds by default and remains
configurable from 60 through 3,600 seconds. It observes only logically running
Broker delegation tasks. Every 15 seconds, or on a coalesced wake, it derives:

- `active` from recent child agent activity;
- `waiting_input` when the child needs user input; or
- `stalled` after the configured silence threshold.

It publishes observation snapshots and UI status only. It has no cancel,
disconnect, settle, or route-change capability.

### ACP idle sweep

The connection idle sweep continues to reclaim abandoned `Connected`
connections. It continues to skip `Prompting` connections, pending permission,
and known background work. Frontend keepalive may refresh the idle-sweep clock,
but never the execution-lease progress clock.

### New execution watchdog

The new watchdog owns user warning and cancellation for foreground execution.
With product defaults, the three levels for a delegated child are:

1. 300 seconds: child observation becomes `stalled`.
2. 600 seconds: the relevant foreground lease warns the user.
3. 1,200 seconds: the lease cancels unless progress or a user extension occurs.

## Architecture

### Ownership

`ConnectionManager` owns a manager-level `ToolExecutionLeaseRegistry`.
`AppState` starts one `ToolExecutionWatchdog` supervisor and provides the
event-emission and cancellation capabilities it needs.

The registry owns only identity, timestamps, state, settings snapshots, and an
opaque typed cancellation capability. It does not own child processes, MCP
clients, or Broker run state.

Each executor registers a cancellation capability with the lease:

- `Terminal`: a control channel to the connection loop that owns
  `TerminalRuntime` and its terminal id;
- `Delegation`: a Broker task id and parent-wait association;
- `McpRequest`: a request-scoped cancellation handle when supported;
- `Turn`: the ACP connection/session/turn generation fallback.

The registry never guesses a cancellation target. When concurrent tool calls
make terminal association ambiguous, the lease receives only a turn-level
fallback capability.

### Lease identity

The logical key is:

```text
connection_id + connection_incarnation + turn_generation + tool_call_id
```

Each registration also creates an opaque `lease_id`. Every state transition
increments a `version`. UI actions must include both `lease_id` and `version`,
so stale windows, replayed ACP updates, old connection incarnations, and reused
tool-call ids cannot affect a newer execution.

### Data flow

```text
ACP updates / terminal deltas / MCP progress / delegation child activity
    -> semantic progress normalizer
    -> ToolExecutionLeaseRegistry
    -> ToolExecutionWatchdog
    -> warning projection or typed cancellation control
    -> executor-owned cancellation and lifecycle convergence
```

The existing delegation supervisor remains specialized. Child semantic
activity renews both the child-health observation clock and any foreground
parent lease waiting for that child.

Progress is attributed to exactly one mapped lease or one verified dependency
edge. Activity from one parallel tool must not renew any sibling lease. Generic
agent transcript activity renews only the untracked-turn fallback clock unless
the provider supplies a reliable tool-call association.

## Lease Model

A lease records:

```text
lease_id
version
connection_id
connection_incarnation
session_id
turn_generation
tool_call_id
tool_kind
display_title
state
last_progress_at
warning_emitted_at
grace_deadline
pause_reason
cancellation_scope
```

`display_title` is a safe normalized label such as `run_terminal_command`. Raw
commands and arguments are never stored in watchdog projections.

### States

```text
Running <-> Paused
Running -> Warning -> Grace -> Cancelling -> TimedOut
Running/Paused/Warning/Grace -> Completed
```

`Warning` is the transition that publishes the actionable warning. Once the
warning is published, the lease enters `Grace` with a full grace deadline.
The live lease is removed after a terminal transition. Bounded diagnostics
remain in the existing session event history rather than a second registry
ledger.

### Timing rules

- `Running` enters `Warning` after 600 seconds without semantic progress.
- Warning publication starts a new 600-second `Grace`; elapsed historical time
  is never deducted from this grace period.
- "Wait 10 minutes" sets a new grace deadline at `now + captured_grace` and
  increments the version. It does not update `last_progress_at`.
- User extensions are unlimited.
- Semantic progress during `Warning` or `Grace` returns the lease to `Running`,
  clears warning state, and starts a new 600-second progress window.
- Completion removes the lease from every non-terminal state.
- A setting reduction or application resume may produce an immediate warning,
  but never warning and cancellation in the same supervisor pass.
- The grace duration is captured when the warning is published. Later setting
  changes do not mutate an active countdown. Extensions use that captured
  duration.
- Disabling the watchdog clears active generic warning/grace states without
  fabricating progress. Re-enabling can warn an overdue lease, but still grants
  a complete new grace period.

The supervisor uses a coalescing wake plus a bounded periodic scan. Deadlines
are derived from recorded timestamps, not scan time, so scan jitter cannot
accumulate.

### Paused execution

The following states pause the lease and suppress warning/cancellation:

- pending user permission;
- a structured agent question awaiting the user's answer;
- delegation `waiting_input`;
- another verified user-input wait represented by a normalized ACP state.

Leaving `Paused` starts a fresh progress window at the current time. An
acknowledged background task is removed from foreground watchdog ownership as
soon as its foreground tool call returns the background handle.

Application backgrounding does not pause a lease. A warning produced while the
application is hidden also emits a system notification.

## Semantic Progress

Only a new semantic fact renews a lease.

### Counts as progress

- A positive terminal output-offset advance, including output retained behind
  a truncation window.
- A terminal exit-status transition.
- A new agent message, thought chunk, or changed plan payload for the
  untracked-turn fallback clock. It renews a tool lease only when provider
  metadata reliably associates that update with the same tool call.
- A tool-call status transition or changed tool content.
- An MCP progress notification or final result.
- Real child-agent activity for a foreground wait associated with a Broker
  delegation task.

### Does not count as progress

- ACP or frontend keepalive.
- Terminal polling with an unchanged output offset.
- Repeated cumulative output snapshots with identical normalized content.
- Usage, session-info, available-command, or config metadata.
- Process existence, CPU growth, or child-process churn by itself.
- A user grace extension.
- Repeated status queries that return the same downstream state.

Progress normalizers retain bounded monotonic offsets, status enums, or content
fingerprints. They do not retain an unbounded second copy of tool output.

## Warning UX

The 600-second warning is a persistent in-session surface, not only a toast. It
shows:

- the safe tool display title;
- the last progress time;
- the remaining grace countdown;
- "Stop now"; and
- "Wait 10 minutes".

When the application is hidden, Codeg sends a system notification that opens
the affected session. The system notification excludes command text and tool
arguments.

All open windows receive the same lease projection. An action is accepted only
when its `lease_id` and `version` match the current lease. Losing windows apply
the winner's next event and do not display a contradictory local outcome.

After timeout, the tool remains visible as a failed transcript entry and the
composer becomes usable again. Codeg does not offer an automatic retry because
the original tool may already have produced side effects.

## Cancellation and Convergence

When grace expires, the watchdog atomically claims the current lease version
and enters `Cancelling`. Completion or progress that acquired the claim first
wins. Output arriving after the cancellation claim increments a secret-safe
`late_activity` diagnostic counter, but cannot revive the lease or overwrite
its outcome.

Cancellation uses the narrowest available capability.

### Terminal

Kill the complete process tree through the connection loop and allow
`waitForExit` to return a killed exit status. The owning agent can then produce
its normal tool error and continue the turn.

### Delegation

Call the existing Broker cancellation path with
`cancel_delegation(reason=timeout)`. Publish the terminal task report and wake
all status or terminal-only waiters, including `wait_ms=0`.

### Cancellable MCP request

Cancel the specific request and wait for its error/result to settle the tool
call.

### Turn fallback

If request-level cancellation is unavailable or a precise association was not
safe, send ACP `session/cancel` for the stamped current turn. This ends the turn
without deliberately disconnecting the reusable ACP connection.

### Escalation

After initiating specific cancellation, wait up to 10 seconds for a terminal
tool update or turn convergence. If it does not converge, cancel the current
turn. If turn cancellation also fails to converge within 10 seconds,
disconnect only the offending ACP connection and run normal connection-loss
cleanup.

Every path must leave the conversation out of `in_progress` through an
idempotent, generation-guarded lifecycle transition. It must not overwrite a
completion that committed before timeout ownership was claimed.

When specific tool cancellation returns control to the agent, the normal agent
turn lifecycle remains authoritative. If escalation reaches ACP turn cancel,
the manager uses the existing CAS transition from `InProgress` to `Cancelled`
and emits the watchdog error alongside the status change. Connection fallback
uses the existing connection-loss reconciliation and must reach the same
non-`InProgress` invariant.

### Error contract

Automatic expiry emits a structured error with stable code
`tool_stalled_timeout` and safe metadata:

```text
lease_id
tool_call_id
last_progress_at
terminated_at
cancellation_scope
```

User "Stop now" uses `user_cancelled`. Automatic timeout and user cancellation
must never share a code. Cancellation transport failures retain
`tool_stalled_timeout` as the initiating cause and add a separate structured
escalation/failure field.

## Untracked Prompting Fallback

Some providers may emit a foreground tool call without enough information to
associate a specific cancellation target. A separate connection-turn fallback
applies only when all of the following are true:

- the connection remains `Prompting`;
- no trackable foreground lease exists for the current turn;
- no pending permission or user answer exists;
- no verified background work accounts for the turn; and
- there has been no semantic agent activity for 1,800 seconds.

Codeg then warns and grants 600 seconds. Expiry cancels only the current turn.
It does not attempt to identify or kill a guessed tool. The ACP idle sweep
remains independent.

## Settings and APIs

Keep the existing key unchanged:

```text
delegation.stalled_after_seconds = 300
```

Add:

```text
tool_watchdog.enabled = true
tool_watchdog.warning_after_seconds = 600
tool_watchdog.grace_seconds = 600
```

The two durations are clamped to 60 through 3,600 seconds. The settings use
existing `app_metadata` storage and additive settings API fields, so no schema
migration is required.

The exact settings transport operations are:

```text
acp_get_tool_watchdog_settings()
acp_set_tool_watchdog_settings(enabled, warning_after_seconds, grace_seconds)
```

Their Rust cores use the same names with a `_core` suffix; Axum exposes
`POST /acp_get_tool_watchdog_settings` and
`POST /acp_set_tool_watchdog_settings`. The settings UI places a separate
"Tool execution watchdog" section under Settings > General. It does not place
global execution policy inside the existing Delegation section; that section
continues to own only `delegation.stalled_after_seconds` and other delegation
policy.

Shared backend cores and their exact transport operation names are:

```text
acp_tool_watchdog_extend(lease_id, version)
acp_tool_watchdog_cancel(lease_id, version)
```

The Rust cores are named `acp_tool_watchdog_extend_core` and
`acp_tool_watchdog_cancel_core`. Tauri commands use the operation names above;
Axum exposes `POST /acp_tool_watchdog_extend` and
`POST /acp_tool_watchdog_cancel`. Both request bodies contain only `lease_id`
and `version`. A stale action returns stable code
`stale_tool_watchdog_lease` without mutation.

Tauri commands and Axum handlers call the same cores. The frontend uses the
existing transport abstraction.

The connection event stream adds exactly one variant:

```text
AcpEvent::ToolWatchdogChanged {
    projection: ToolWatchdogProjection
}
```

`ToolWatchdogProjection` contains `lease_id`, `version`, optional
`tool_call_id`, safe `tool_title`, `phase`, `last_progress_at`, optional
`grace_deadline`, optional `cancellation_scope`, and optional `error_code`.
`phase` is one of `warning`, `grace`, `cancelling`, `timed_out`, or `cleared`.
The live session snapshot carries the optional currently actionable projection
so attach/replay cannot lose an open warning. Desktop batching treats warning,
cancelling, timed-out, and cleared transitions as flush-sensitive.

## Restart and Cleanup

Live leases are in-memory because their ACP requests and terminal processes do
not survive a clean Codeg restart. On disconnect, turn replacement, session
replacement, or connection incarnation change, the manager removes the old
leases and invokes existing resource cleanup.

On application startup, the registry starts empty. Existing boot/disconnect
reconciliation owns any conversation rows left `in_progress`; the watchdog
must not invent timeout outcomes for resources it never observed.

Registry cleanup is mandatory on completion, cancellation, disconnect, and
turn-generation replacement. Bounded terminal diagnostics use the existing
session/event history rather than an unbounded watchdog ledger.

## Observability and Security

Emit structured, secret-safe transitions:

```text
lease_started
lease_warning
lease_extended
lease_progress
lease_paused
lease_resumed
lease_cancelling
lease_terminated
lease_completed
```

Metrics count warning episodes, extensions, automatic timeouts, user stops,
specific-cancel success, turn-level fallback, disconnect fallback, and
cancellation failure by agent type and coarse tool category.

Logs, metrics, events, system notifications, and stored watchdog diagnostics
must not contain raw command text, environment variables, file contents,
prompts, or MCP arguments. Backend actions validate lease identity, version,
connection incarnation, and turn generation before mutation.

The session-details surface shows the most recent watchdog transition,
timestamp, safe tool title, and stable reason code.

## Testing

### State-machine unit tests

Use a controlled clock to cover:

- the 600-second warning boundary and complete 600-second grace;
- unlimited extension without changing `last_progress_at`;
- progress returning `Warning` or `Grace` to `Running`;
- duplicate snapshots, keepalive, polling, usage, and CPU activity not renewing;
- pause/resume for permission, question, and `waiting_input`;
- system-resume and setting-reduction behavior warning without same-pass kill;
- live disable/re-enable behavior;
- completion, progress, manual stop, and timeout racing for one winner;
- rejection of stale lease versions, incarnations, and turn generations; and
- cleanup on completion, replacement, and disconnect.

### Executor integration tests

- Start a controlled silent terminal helper, advance through warning/grace,
  and verify the full process tree exits and `waitForExit` converges.
- Stream output beyond truncation caps and verify monotonic output offsets renew
  the lease without unbounded watchdog memory.
- Exercise an MCP server that honors request cancellation.
- Exercise an MCP server that ignores request cancellation and verify turn then
  connection escalation.
- Verify Broker child activity renews a parent wait lease.
- Verify Broker timeout cancellation wakes terminal-only `wait_ms=0` status
  waits and writes one terminal task result.
- Verify an ambiguous parallel terminal association never kills a guessed
  terminal and uses only the turn fallback.

### Lifecycle and persistence tests

- Automatic timeout, user stop, agent completion, and disconnect each leave the
  conversation out of `in_progress` exactly once.
- Completion committed before timeout remains authoritative.
- Timeout ownership committed first rejects late completion overwrite.
- Session resume keeps the external session id and can accept a new prompt.
- Desktop and server paths emit equivalent projections and outcomes.

### Frontend tests

- Warning content, countdown, extend, and stop actions.
- Progress clears the warning and resets the visible deadline.
- Multiple windows deduplicate actions by lease version.
- Hidden-app system notification is emitted once and contains no raw input.
- Timeout leaves a failed tool entry and restores composer input.
- Settings default, clamp, save, reload, and disable behavior.
- All ten locale files contain the additive watchdog messages.

## Acceptance Criteria

1. Every safely observable foreground ACP tool receives a generation-stamped
   execution lease regardless of agent provider.
2. Only semantic progress renews a lease.
3. Delegation retains its 300-second non-destructive stalled observation.
4. Generic warning occurs after 600 seconds and always precedes a complete
   600-second grace period.
5. User extensions are version-checked, grant a full new grace period, and are
   unlimited.
6. Timeout cancels the narrowest resource and escalates only when convergence
   fails.
7. No automatic timeout leaves a conversation durably `in_progress`.
8. No automatic retry occurs.
9. Ambiguous tool association never causes a guessed process or request to be
   canceled.
10. Logs, events, notifications, and metrics expose no raw tool input.
11. Desktop and server transports provide the same warning and control
    behavior.
12. The original silent-terminal reproduction converges without output
    truncation being treated as the cause.
