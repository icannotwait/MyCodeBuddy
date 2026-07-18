# Coordination V1 Status-Wait Contract Design

## Status

Approved in conversation on 2026-07-18.

This specification tightens the MCP input contract defined by
`docs/superpowers/specs/2026-07-17-event-driven-delegation-join-design.md`.
That design remains authoritative for Broker-owned Join, attention, task
lifecycle, observation, and Delegation Card behavior. This document changes
only how a coordination-aware `codeg-mcp` companion advertises and validates
legacy positive status waits.

No implementation plan has been approved yet. The implementation plan must
preserve the contracts in this document.

## Problem

`coordination_v1` exposes the canonical event-driven Join:

```text
get_delegation_status({
  task_ids,
  wait_ms: 0,
  return_when: "all_terminal_or_attention"
})
```

The same coordination-aware tool schema still advertises positive `wait_ms`
values as valid. When `return_when` is omitted, the companion forwards that
request through the legacy supervised-wait path. A supervised wait can return
when an active child's `last_agent_activity_at` changes. If the parent model
then repeats the same status call, ordinary child activity can recreate the
high-frequency polling loop that Join was designed to avoid.

Rejecting positive waits only in the runtime is insufficient. A schema that
advertises the input as valid while the companion rejects it gives models a
contradictory contract and increases the chance of repeated invalid tool
calls. The advertised schema, tool guidance, parameter guidance, and runtime
validation must agree.

## Goals

- Make positive `wait_ms` invalid on `coordination_v1` connections.
- Keep immediate snapshots available by omitting `wait_ms`.
- Keep the canonical blocking path as `wait_ms: 0` plus
  `return_when: "all_terminal_or_attention"`.
- Reject a positive wait synchronously in the companion before any Broker
  round trip.
- Return a stable, actionable error that gives the model the exact replacement
  arguments.
- Make MCP tool and parameter descriptions accurately explain the contract.
- Preserve the complete pre-coordination schema and behavior for legacy
  connections.
- Preserve the existing serialized `tools/list` size budget for Grok stdio.

## Non-Goals

- Removing legacy supervised waits from legacy connections.
- Changing `DelegationBroker::get_tasks_status` or `StatusWait::Supervised`.
- Silently remapping a bounded legacy wait into an unbounded Join.
- Rejecting an omitted `wait_ms` snapshot request.
- Rejecting `wait_ms: 0` without `return_when`; this existing terminal-only
  legacy form remains accepted, although the coordination guidance promotes
  the canonical Join instead.
- Adding frontend hover tooltips or changing ToolCall card rendering.
- Adding a separate Join MCP tool.
- Automatically retrying an invalid tool call.

## Selected Approach

Use capability-specific schema projection plus defense-in-depth validation.

The embedded `tool_schema.json` remains the coordination-aware source schema.
For `get_delegation_status`, its `wait_ms` property carries `maximum: 0` and
coordination-specific descriptions. When `coordination_v1` is absent,
`tools/list` restores the legacy tool description, removes `return_when`,
removes `maximum` from `wait_ms`, and restores the legacy `wait_ms`
description.

The companion independently validates coordination-aware calls. If
`coordination_v1` is enabled, `return_when` is absent, and `wait_ms` is
positive, the call returns JSON-RPC `-32602` synchronously. It does not open a
Broker socket request, mutate task state, or start an automatic retry.

This keeps old clients wire-compatible while making the new capability's
contract strict and self-consistent.

## Alternatives Considered

### Guidance-only deprecation

Rejected. Better descriptions reduce invalid calls but do not prevent a model
from selecting the still-valid positive-wait schema branch.

### Silent positive-wait remapping

Rejected. A legacy positive wait is bounded and returns when any requested
task becomes actionable. Canonical Join is unbounded and waits for all
terminal tasks or attention. Silently converting one into the other changes
observable timing and batch semantics.

### Dedicated Join tool

Deferred. A separate tool would make the model-facing API clearer, but it
would duplicate task-id collection concepts and require a broader API and
adapter migration. The existing additive `return_when` field is sufficient
for V1.

## Capability-Specific Contract

### Coordination-aware connection

| Input | Result |
| --- | --- |
| `task_ids` only | Immediate snapshot |
| `task_ids`, `wait_ms: 0` | Existing terminal-only wait |
| `task_ids`, `wait_ms: 0`, `return_when: "all_terminal_or_attention"` | Canonical Join |
| `task_ids`, positive `wait_ms`, no `return_when` | Synchronous `-32602` |
| `return_when` with missing or nonzero `wait_ms` | Existing synchronous `-32602` |

The positive-wait rejection message is stable and corrective:

```text
positive wait_ms is unavailable with coordination_v1; retry with
return_when="all_terminal_or_attention" and wait_ms=0
```

The message must identify the rejected field, the capability that changes its
meaning, and the complete replacement pair. It must not suggest cancellation
or imply that the child task failed.

### Legacy connection

Legacy `tools/list` and runtime behavior remain unchanged:

- `return_when` is not advertised and is rejected if called directly.
- Omitted `wait_ms` returns an immediate snapshot.
- `wait_ms: 0` performs the existing terminal-only wait.
- Positive `wait_ms` performs the existing bounded supervised wait.

## MCP Guidance

The coordination-aware tool description must lead with the supported choices:

1. Omit `wait_ms` for an immediate snapshot.
2. For blocking collection, use the canonical Join with
   `return_when=all_terminal_or_attention` and `wait_ms=0`.
3. Positive `wait_ms` values are rejected on `coordination_v1`.
4. Answer returned attention requests and re-Join only still-running required
   task ids.

The `wait_ms` parameter description must state that omission means snapshot,
zero is used by canonical Join, and positive values are rejected. The
`return_when` parameter description must state the exact enum literal, require
explicit `wait_ms: 0`, and summarize the three Join wake outcomes: all
terminal, attention required, or unavailable.

The legacy tool and `wait_ms` descriptions continue to explain snapshot,
terminal-only, and positive supervised waits. Coordination-only terminology
must not leak into legacy `tools/list` output.

Descriptions should be concise enough to keep the all-feature serialized
`tools/list` response within `GROK_STDIO_SAFE_TOOLS_LIST_BYTES` (7,680 bytes).

## Validation Flow

For `get_delegation_status`, the companion processes input in this order:

1. Normalize and validate the non-empty `task_ids` array.
2. Read `wait_ms` and validate the optional `return_when` value.
3. Apply the capability-specific positive-wait rule.
4. Return synchronous `-32602` for a contract violation.
5. Only valid requests construct `BrokerStatusRequest` and open the Broker
   round trip.

Validation belongs in the companion because it owns the MCP schema and knows
the launch-bound `coordination_v1` capability. The listener retains its
authorization and defense-in-depth checks; the Broker remains unaware of MCP
schema policy.

## Error and Retry Behavior

The companion does not automatically retry tool calls. A host may expose the
`-32602` result to the model, which may choose to correct or repeat the call.
Schema enforcement and exact corrective guidance reduce that risk but cannot
guarantee model behavior.

No server can both strictly reject every repeated invalid request and
guarantee that a misbehaving client never receives repeated errors. The V1
contract therefore prevents invalid calls at schema-aware clients and makes a
single correction straightforward for clients that bypass schema validation.

## Testing

### Companion validation

- A coordination-aware positive `wait_ms` without `return_when` returns
  `-32602` with the exact corrective message.
- The rejected call does not create a spawned Broker round trip.
- Omitted `wait_ms` remains a valid snapshot request.
- `wait_ms: 0` without `return_when` remains valid.
- Canonical Join remains valid.
- A legacy positive `wait_ms` remains valid.

### Tool schema projection

- Coordination-aware `tools/list` exposes `return_when`.
- Coordination-aware `wait_ms` has `minimum: 0` and `maximum: 0`.
- Coordination-aware tool and parameter descriptions contain the snapshot,
  canonical Join, and positive-rejection guidance.
- Legacy `tools/list` removes `return_when` and the `maximum` constraint.
- Legacy tool and parameter descriptions continue to advertise positive
  bounded waits and contain no coordination-only Join guidance.

### Regression gates

- Existing Join input validation tests continue to pass.
- Existing Join and supervised-wait Broker tests remain unchanged.
- The all-feature `tools/list` response remains at or below 7,680 bytes.
- `cargo test --features test-utils` and the `codeg-mcp` check/clippy gates
  remain green, subject to the repository's standard verification scope.

## Rollout and Compatibility

The strict contract applies only to newly launched companions whose immutable
feature list contains `coordination_v1`. Existing processes and sessions keep
the feature set and schema with which they were launched. A rebuilt application
must be restarted, and affected agent connections must be relaunched, before
the new contract is observable.

No database migration, persisted-data change, frontend migration, or Broker
protocol version bump is required.

## Success Criteria

- A coordination-aware model cannot discover positive `wait_ms` as a valid
  schema value.
- A client that bypasses schema validation receives an immediate actionable
  `-32602` and causes no Broker status wait.
- Canonical Join and immediate snapshot behavior are unchanged.
- Legacy connections retain positive supervised waits without schema or
  runtime regressions.
- MCP tool tips accurately describe the capability-specific contract.
