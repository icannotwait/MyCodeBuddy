# Delegation Continuation (opt-in)

## Behavior change

Without the capability, canonical Join (`return_when=all_terminal_or_attention`,
`wait_ms=0`) keeps the existing event-driven parked status wait: the parent turn
remains open and the model can still loop on status tools.

With **delegation continuation v1** enabled on a connection, a running Join:

1. inserts exactly one durable continuation row for the parent conversation;
2. dispatches `SuspendForDelegation` so the parent turn can end without
   cascading child cancellation;
3. waits without periodic parent model requests until all-terminal, parent
   attention, unavailable reclassification, or the 240-second checkpoint;
4. admits exactly one **server-authored hidden prompt** into the same agent
   session with a typed snapshot and durable marker;
5. projects a conversation-scoped waiting lock so external user prompts are
   rejected (not queued) while the continuation is active.

Join socket close is waiter-only: it does not cancel the arm worker, the
continuation row, or live children.

## Capability and agent matrix

| Condition | Result |
| --- | --- |
| `CODEG_DELEGATION_CONTINUATION_V1` unset / `0` / other | **Default off** — no arming |
| Env `1` or `true` (case-insensitive) | Opt-in allowed |
| Agent type Codex + Codeg route exposure | Capability can arm |
| Non-Codex agents | Never arms (Codex-only) |
| Native / non-Codeg route | Never arms |
| `CompanionFeatures::coordination_v1` | Unchanged; independent of continuation |

Connection-bound: the flag is evaluated at companion injection / launch and
stored on the token as `delegation_continuation_v1`.

## Opt-in command examples

```powershell
# Desktop (process env before launch)
$env:CODEG_DELEGATION_CONTINUATION_V1 = "1"
# then start the Codeg desktop binary as usual
```

```powershell
# Server
$env:CODEG_DELEGATION_CONTINUATION_V1 = "1"
# then start codeg-server with your usual CODEG_* host/port/token vars
```

```bash
# Unix shell
export CODEG_DELEGATION_CONTINUATION_V1=1
```

Only the literal values `1` and `true` (any case) enable the capability.
Values such as `yes`, `on`, or `0` leave it disabled.

**This document does not claim default-on rollout.** Default remains off until
exit criteria below are met and a separate release decision flips the default.

## Observability

### Low-cardinality metric names

Process-local counters on `DelegationMetrics` (stable labels only):

| Metric | Labels / notes |
| --- | --- |
| `continuation_armed` | count |
| `continuation_suspended` | with suspend duration totals |
| `continuation_wake_claimed` | wake reason: `all_terminal`, `attention_required`, `unavailable`, `checkpoint` |
| `continuation_prompt_admitted` | count |
| `continuation_cancelled` | prior phase: `arming`, `waiting`, `wake_pending`, `resuming` |
| `continuation_failed` | phase × failure code |
| `continuation_reconciled` | prior active phase on startup |
| `continuation_duplicate_claim_suppressed` | count |
| `continuation_prompt_delivery_retry` | count |
| `continuation_wait_duration_ms_*` | by wake reason |
| `continuation_suspend_duration_ms_*` | totals |

Failure codes (wire-stable): `arm_failed`, `suspend_dispatch_failed`,
`suspend_drain_timeout`, `parent_connection_lost`, `prompt_delivery_failed`,
`state_conflict`.

### Log correlation fields

Prefer structured fields, never secret prompt bodies or markers:

- `parent_connection_id`
- `conversation_id`
- `continuation_id`
- `generation`
- `state` / prior phase
- `wake_reason`
- `failure_code` / `code`
- `parent_turn_generation` (when fencing suspension)

Do not log full hidden prompt text, internal markers, or foreign task payloads
in user-facing channels.

## Rollback order

1. **Disable new arming** — unset `CODEG_DELEGATION_CONTINUATION_V1` (or set to
   a non-enabling value) and restart processes so new launches inject
   capability-off tokens. Existing event-driven Join behavior returns for new
   waits.
2. **Let understood active rows finish** — allow in-flight continuations to
   complete, cancel via user Stop, fail on disconnect, or be reconciled on
   process restart. Deploy builds that still understand the
   `delegation_continuations` schema while any active rows may exist.
3. **Only then deploy older behavior** that may lack arming/wake logic — still
   keep schema-compatible cleanup if possible.
4. **Never** `DROP TABLE delegation_continuations` or delete active rows to
   “force unlock”; that can strand parent locks and orphan child ownership.

The migration is additive; empty tables may remain after rollback.

## Failure semantics

| Path | Effect |
| --- | --- |
| **User Stop** | Cancel live workers; drain Broker children for the parent turn; CAS active row to `cancelled` (unless prompt already durably admitted — then ordinary cancel owns the new turn). Waiting projection cleared. No duplicate hidden prompt. |
| **Parent disconnect** | Cancel workers; `cancel_by_parent`; CAS fail with `parent_connection_lost`; publish failure; clear waiting. Lock released only after cleanup ownership is established. |
| **Suspend drain timeout** | Typed failure `suspend_drain_timeout`; fail-closed for that arm; children follow the disconnect/timeout cleanup path. |
| **Process restart** | `reconcile_on_startup` fails non-terminal rows with `parent_connection_lost`, records prior-phase metrics, publishes failure once; second reconcile is a no-op. |
| **Prompt delivery failure** | Bounded retries then fail with `prompt_delivery_failed`; children drained before terminal CAS. |

## Non-goals

- **Cross-connection handoff** — a continuation is bound to the parent
  connection that armed it; ownership is not transferred to another live
  connection.
- Default-on for all agents or non-Codeg routes.
- UI configuration of the 240s checkpoint (fixed backend constant in v1).
- Periodic parent model polling while children are merely running.

## Default-on exit criteria

Do **not** flip the default until a telemetry window shows:

- no orphan tasks after Stop, disconnect, or restart;
- no duplicate admissions / dual hidden prompts for one generation;
- no hidden-message leaks into live `UserMessage` or cold user transcripts;
- no stuck conversation locks (`waiting_for_subagents`) after terminal rows;

and the Codex-shaped e2e matrix (`delegation_continuation_e2e_*`) plus desktop,
server, companion, and frontend verification remain green on a release build.
