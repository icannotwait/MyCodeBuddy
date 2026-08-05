# Grok Delegation Continuation Design

## Status

Approved in conversation on 2026-08-05.

This specification extends the existing delegation continuation design in
`2026-07-19-delegation-continuation-design.md` to Grok. It does not replace the
existing coordinator, persistence, suspension, or Codex contracts. Where this
document is silent, the existing continuation design and release guide remain
authoritative.

No implementation plan has been approved yet.

## Problem

Canonical Join uses:

```json
{
  "return_when": "all_terminal_or_attention",
  "wait_ms": 0
}
```

Semantically, `wait_ms: 0` means that the Join has no task-duration deadline.
In a Grok parent session today, however, continuation is disabled, so the Join
is implemented as one parked MCP request. Grok still applies a finite host
deadline to that request. The repository currently configures
`get_delegation_status` to 5,400,000 ms, while Grok's unconfigured default is
6,000 seconds. Either value is finite and can be exceeded by a valid complex
task.

The failure is therefore at the request-transport layer, not the child-task
layer. Field evidence from session 2957 showed that:

- Grok terminated long Join requests at its host deadline;
- the delegated child remained running after each timeout;
- later Join calls could still observe and complete other tasks;
- after repeated timeout-shaped tool failures, the parent changed strategy to
  `Start-Sleep` and immediate snapshots; and
- ending the parent turn while a child remained live then produced the
  intentional `join_abandoned` cleanup.

Increasing the host deadline cannot make an unbounded semantic wait correct.
It only moves the same failure farther into the future.

## Goals

- Make normal long-running Grok Join waits independent of any single MCP tool
  request deadline.
- Preserve the existing Codex continuation behavior.
- Reuse the existing durable continuation coordinator and state machine.
- Suspend and later resume the same Grok ACP session automatically.
- Keep child tasks running while the parent turn is suspended.
- Treat the 600-second continuation checkpoint as a successful wake event, not
  a task failure or tool timeout.
- Keep user Stop, parent disconnect, suspension failure, and task failure
  distinct from a normal wait checkpoint.
- Allow Grok continuation to be disabled without disabling Codex
  continuation.
- Prove real Grok cancel-then-prompt compatibility before release.

## Non-Goals

- Removing all finite timeouts from Grok or MCP.
- Changing child execution limits or task terminal-state semantics.
- Changing the canonical Join request shape.
- Enabling continuation for agents other than Codex and Grok.
- Enabling continuation on native or otherwise non-Codeg delegation routes.
- Transferring a continuation to a replacement ACP connection after the
  original connection is lost.
- Adding a user-facing checkpoint-duration setting.
- Making suspension protocol failures look successful.
- Changing the 600-second checkpoint interval in this work.
- Redesigning the continuation database schema, UI waiting projection, or
  hidden-turn filtering.

## Selected Approach

Extend the existing connection-bound `delegation_continuation_v1` eligibility
from Codex-only to Codex-or-Grok for Codeg delegation routes.

No Grok-specific coordinator is introduced. A Grok Join follows the same
durable sequence already used by Codex:

```text
canonical Join
  -> evaluate current Broker snapshot
  -> ready: return the ordinary Join result
  -> still running:
       insert durable continuation row
       transfer wait ownership to the continuation coordinator
       dispatch SuspendForDelegation for the exact parent turn generation
       receive suspension acknowledgement
       release the foreground MCP response
       wait server-side for Broker events or checkpoint
  -> atomically claim one wake reason
  -> admit one hidden prompt into the same Grok session
  -> continue in a new parent turn
```

The state machine remains:

```text
Arming -> Waiting -> WakePending -> Resuming -> Completed
             |                         |
             +------ cancel/fail ------+
```

The wake predicate remains the first of:

- every requested task is terminal;
- parent attention is required;
- a required task or producer becomes unavailable; or
- `armed_at + 600 seconds` is reached.

At a checkpoint, the hidden prompt contains the existing typed continuation
envelope with `wake_reason: "checkpoint"` and a fresh task snapshot. The
existing coordination tool contract tells the model to re-Join only required
tasks that are still running. A new Join creates the next continuation
generation. Repeating this cycle gives the overall wait no duration limit even
though each foreground MCP call remains short.

## Eligibility and Rollback

`continuation_enabled_for_launch` remains a launch-time, connection-bound
decision. The capability can arm only when all of the following are true:

- the immutable route plan exposes Codeg delegation;
- the agent is Codex or Grok; and
- the relevant environment kill switches are not disabled.

Environment behavior is:

| Setting | Codex | Grok |
| --- | --- | --- |
| Both variables unset | enabled | enabled |
| `CODEG_DELEGATION_CONTINUATION_V1=0/false` | disabled | disabled |
| `CODEG_DELEGATION_CONTINUATION_GROK_V1=0/false` | unchanged | disabled |
| Grok-specific variable set to another value | unchanged | enabled |

Comparison with `false` is case-insensitive. As with the existing global
switch, only literal `0` or `false` disables the feature; unknown values do not
silently disable it. The global switch is the master switch and wins over a
Grok-specific enabled value.

Codex's default-on behavior and existing global rollback contract are
unchanged. The Grok-specific switch permits an operational rollback without
removing continuation from Codex. Other agents remain in compatibility Join.

## Turn Suspension and Resume

The existing `SuspendForDelegation` control remains the only operation that may
end a parent turn without draining its child tree. Enabling the capability for
Grok must not route suspension through ordinary user cancellation or natural
turn completion.

For Grok, the connection loop sends the standard ACP `session/cancel`
notification only after installing a `SuspensionLease` fenced by:

- continuation ID;
- parent connection ID;
- parent session ID; and
- parent turn generation.

The connection acknowledges suspension only after the exact old turn is no
longer in flight and the connection is safe to accept another prompt. A Grok
terminal notification that matches the lease finalizes as
`DelegationSuspended`; it must not become `ParentCanceled`, `JoinAbandoned`, or
`ParentTurnFailed`, and it must not call the Broker parent-tree drain.

After acknowledgement, the listener returns the existing normal continuation
release batch and opens the foreground MCP release fence. The coordinator does
not admit the hidden prompt until that fence confirms that the old response
frame has been flushed or the foreground waiter has otherwise been released.
This preserves ordering between the old MCP call and the resumed turn.

Before prompt admission, the coordinator revalidates the connection,
conversation, session, continuation generation, and suspended turn generation.
The hidden prompt is admitted into the same Grok session as a new turn. Late
events or tool output from the suspended generation cannot satisfy or mutate
the resumed generation.

## Timeout Semantics

The implementation must distinguish four independent concepts:

| Event | Classification | Child-task effect |
| --- | --- | --- |
| Child runs longer than any MCP host deadline | normal waiting | none |
| 600-second continuation checkpoint | successful wake | none |
| Foreground Join socket closes | phase-fenced waiter release | none; pre-transfer arm is aborted, post-transfer continuation survives |
| Suspension drain exceeds 30 seconds | infrastructure failure | existing fail-closed cleanup |

In particular:

- `wait_ms: 0` never becomes a child-task timeout.
- `ContinuationWakeReason::Checkpoint` is recorded as a wake metric, not a
  continuation failure or task failure.
- An MCP cancellation before wait-ownership transfer aborts that arm attempt
  and releases its registration. It does not mark the child failed.
- An MCP cancellation after transfer is waiter-only. The durable continuation
  worker and child execution remain live.
- A failure to persist, dispatch, drain, fence, or admit a prompt remains a
  real infrastructure failure. Once `session/cancel` has been sent, the old
  turn cannot safely receive a fabricated successful checkpoint; doing so
  could create two resume owners.
- User Stop retains its existing explicit cancellation meaning and may cancel
  both the continuation and its child tree.

No correctness claim depends on the configured 5,400,000 ms Grok status-tool
timeout. That timeout remains defense in depth for compatibility Join when
continuation is disabled.

## Races and Ownership

The existing linearization points remain authoritative and must also be tested
with Grok-shaped turn events:

- **Task completes before durable insert:** ordinary Join returns immediately.
- **Task completes during arming:** one wake claim is retained, suspension
  completes, and the parent is resumed once.
- **Waiter closes before transfer:** listener cleanup owns the registration;
  no continuation survives.
- **Waiter closes after transfer:** coordinator cleanup owns the registration;
  the continuation survives.
- **Checkpoint races task completion:** the store CAS admits exactly one wake
  reason and one hidden prompt.
- **Late old-turn completion:** the suspension lease and turn-generation fence
  prevent it from completing the resumed turn.
- **User Stop races wake:** existing stop ownership wins according to the
  durable prompt-admission fence; no duplicate prompt is admitted.
- **Parent connection exits:** workers are cancelled and the continuation is
  failed using the existing parent-connection-lost semantics.
- **Process restarts:** non-terminal rows are reconciled using the existing
  fail-closed startup behavior; no cross-connection adoption is attempted.

## Observability

The existing low-cardinality continuation metrics and failure codes are
reused. No agent name is added as an unbounded label. Logs for the Grok path
must contain the existing correlation fields:

- parent connection ID;
- conversation ID;
- continuation ID and generation;
- parent turn generation;
- state or prior phase;
- wake reason; and
- stable failure code when applicable.

A healthy long Grok wait should show:

```text
continuation_armed
continuation_suspended
continuation_wake_claimed(checkpoint|all_terminal|attention_required|unavailable)
continuation_prompt_admitted
```

It must not emit an MCP timeout error, `join_abandoned`, or child cancellation
for a normal checkpoint cycle. The release guide's capability matrix and
kill-switch examples must be updated for Grok.

## Testing

### Eligibility Unit Tests

Cover the complete launch matrix:

- Codex + Codeg remains default-on.
- Grok + Codeg becomes default-on.
- the global kill switch disables both Codex and Grok;
- the Grok-specific kill switch disables only Grok;
- explicit enabled and unknown values follow the existing semantics;
- native routes remain disabled for Codex and Grok; and
- Claude Code and every other non-eligible agent remain disabled.

### Deterministic Continuation Tests

Use paused Tokio time and existing test ports to prove:

- a running Grok-eligible Join suspends rather than parking to the host
  deadline;
- no prompt is admitted before 600 seconds when no task event occurs;
- exactly one checkpoint prompt is admitted at 600 seconds;
- a checkpoint followed by re-Join creates the next generation;
- completing the task in the later generation resumes with the terminal
  result;
- child state never changes to failed or cancelled because of checkpoint or
  foreground waiter closure;
- task completion during arming is delivered once;
- pre-transfer and post-transfer socket closure have their distinct ownership
  behavior;
- duplicate wake claims are suppressed;
- late old-turn events are fenced; and
- Stop, disconnect, restart, and prompt-delivery failure retain their existing
  failure semantics.

### Grok ACP Integration Tests

Feed the connection loop Grok's actual standard and extension turn-completion
shapes. Assert that:

- `session/cancel` drains the exact active turn within the suspension lease;
- the turn finalizes as `DelegationSuspended`;
- no Broker parent-tree cancellation occurs;
- the same session accepts the hidden continuation prompt;
- the new turn generation is greater than the suspended generation; and
- delayed old tool output cannot mutate the new turn.

### Real Grok Release Probe

Default-on release is gated on a live Grok CLI probe against the Codeg route:

1. Start a controlled slow delegation and issue canonical Join.
2. Verify the parent suspends promptly while the child remains running.
3. Complete the child and verify the same Grok session resumes and consumes
   the result without user input.
4. Run a task through at least one checkpoint cycle and verify that Grok
   reissues canonical Join for still-running required task IDs instead of using
   `Start-Sleep` or immediate-snapshot polling.
5. Confirm the expected continuation metric sequence and absence of MCP
   timeout, `join_abandoned`, and child cancellation.

The automated deadline acceptance test sets a simulated host deadline shorter
than the child duration. The full delegation chain must still finish without a
Join timeout error because no foreground Join remains open until that deadline.

## Alternatives Considered

### Return a Successful Checkpoint From Every Bounded Join

This avoids tool errors but requires the model to call Join repeatedly. It
still spends inference turns merely waiting and allows the model to drift into
sleep or snapshot polling. This design does not add that protocol path. The
selected continuation already provides a successful server-owned checkpoint
without keeping one MCP request open, while genuine pre-suspension failures
remain visible and a post-cancel turn is never given a fabricated result.

### Increase the Grok Tool Timeout

Rejected as a correctness mechanism. A 90-minute or 100-minute timeout is
still shorter than some valid tasks. It also leaves one tool call, one model
turn, and one transport connection responsible for the entire duration.

### Add a Separate Grok Continuation Coordinator

Rejected. The existing state machine already isolates provider-specific turn
handling behind the parent continuation port. Duplicating persistence and wake
ownership would increase race surface and allow Codex and Grok semantics to
diverge.

### Enable Continuation for Every Agent

Rejected for this work. Cancel-then-prompt behavior is an ACP-host compatibility
property and must be demonstrated per agent. This design adds only Grok, as
requested, and leaves all other eligibility unchanged.

## Implementation Scope

Expected code and documentation changes are deliberately narrow:

- extend the pure launch eligibility function and its tests;
- add the Grok-specific environment kill switch at companion injection;
- add Grok-shaped suspension/resume regression coverage around the existing
  connection loop and continuation tests;
- update the delegation continuation release guide and capability matrix; and
- add a repeatable live-probe checklist or harness for the Grok release gate.

No database migration, public API change, frontend feature, or child-task state
change is required.
