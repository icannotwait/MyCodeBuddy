# Delegation Session Reuse Design

## Context

Codeg delegation is currently one-shot. Every `delegate_to_agent` call creates
a new child conversation and a new external agent session, then disconnects it
after one turn. The `brainstorm-to-delivery` Skill reinforces that behavior by
requiring a new child for every document re-review and Task re-review.

Conversation 800 demonstrates the cost. Three plan reviewers (two optional
CodeBuddy profiles plus the required Codex reviewer) reviewed the same plan
across four rounds. Codeg created twelve child conversations even though each
reviewer was continuing the same artifact-specific review. The repeated cold
starts discarded useful context and expanded the parent timeline and overlay.

Codeg already supports resuming external sessions through ACP
`session/resume`, with `session/load` as the compatibility fallback. What is
missing is a durable delegation-run model and an MCP operation that can append
a new delegated turn to an existing child conversation without overwriting the
history of earlier parent tool calls.

## Goals

- Continue the same child agent session for revisions of the same Design,
  implementation plan, or SDD Task role.
- Keep one immutable parent card and one durable task id per delegation round.
- Preserve the complete child transcript and external context in one child
  conversation.
- Preserve independent roles: a Grok implementer thread never becomes a Codex
  reviewer thread, and the final whole-branch review always starts fresh.
- Fall back to a fresh child of the same role and profile when an existing
  external session cannot be resumed.
- Render a compact, structured, immutable result summary on each round card.
- Recover task status, card bindings, and continuation chains after restart.

## Non-goals

- Do not merge different artifacts or different Tasks into one child session.
- Do not reuse an implementer as an independent reviewer.
- Do not reuse a Task reviewer as the final whole-branch reviewer.
- Do not allow callers to change agent type, profile, or workspace while
  continuing an existing delegation thread.
- Do not persist a second copy of the complete child transcript in SQLite.
- Do not automatically substitute an unavailable required agent type.

## Terminology And Invariants

**Delegation thread** means one durable child conversation plus its external
agent session. It owns the reusable context.

**Delegation run** means one parent MCP tool invocation and one child turn. It
owns a task id, lifecycle, runtime statistics, result summary, and parent tool
card.

The following invariants are binding:

1. A child conversation has one `(external_id, agent_type)` identity for its
   entire lifetime.
2. Every run has a unique `task_id` and exactly one `parent_tool_use_id`.
3. Multiple runs may reference the same `child_conversation_id`.
4. At most one run for a child conversation may be active at a time.
5. A terminal run and its card summary are immutable.
6. A continuation uses the same agent type and external session as its thread.
7. A replacement starts a new child conversation but keeps an explicit link to
   the failed thread in the SDD ledger and run metadata.

## Architecture

### Conversation Versus Run

The existing `conversation` row remains the identity and transcript owner for
the child session. A new `delegation_task_runs` table becomes the canonical
store for individual delegation lifecycles.

Initial delegation creates both records:

```text
parent tool call A
  -> run task-A (generation 1)
  -> child conversation 803
  -> external Codex session X
```

Continuation creates only a new run and resumes the existing child:

```text
parent tool call B
  -> run task-B (generation 2, previous task-A)
  -> child conversation 803
  -> external Codex session X
```

The existing unique index on `(external_id, agent_type)` stays unchanged. No
duplicate conversation row is created for a continued turn.

### Durable Run Model

`delegation_task_runs` contains at least:

| Column | Purpose |
| --- | --- |
| `task_id` | Primary key for status, cancellation, and card identity. |
| `root_task_id` | First run in the reusable thread. |
| `previous_task_id` | Immediately preceding run in the same thread. |
| `generation` | One-based run number within the child conversation. |
| `parent_conversation_id` | Owning parent; used for authorization. |
| `parent_tool_use_id` | Exact parent card binding for this run. |
| `child_conversation_id` | Shared child session identity. |
| `agent_type` | Immutable run-time routing snapshot. |
| `profile_id` | Optional immutable profile identity used for audit/fallback. |
| `task_preview` | Bounded display/debug preview, not the full prompt. |
| `status` / `error_code` | Durable run lifecycle. |
| `started_at` / `finished_at` | Per-run timing. |
| runtime-stat columns | Per-run tool, file, and line-change projection. |
| `card_summary_json` | Validated optional Review or Implementation summary. |
| replacement columns | Optional replaced task id and reason code. |

Required indexes include:

- unique `(child_conversation_id, generation)`;
- unique `(parent_conversation_id, parent_tool_use_id)`;
- lookup indexes for parent, child, root, and previous task ids;
- a partial unique index that permits only one active run per child.

The conversation's original `parent_id`, `parent_tool_use_id`, and
`delegation_call_id` remain the immutable creation/root linkage for backward
compatibility. Its task-status, timing, error, and runtime-stat columns remain
as a latest-run projection for existing sidebar and overlay consumers during
the transition. The run table is authoritative for per-card history and MCP
status operations.

### Migration

The migration creates one generation-1 run for every non-deleted delegate
conversation with a `delegation_call_id`. Existing task status wins. For older
rows with a null task status, map conservatively from conversation status:

| Conversation status | Backfilled run status |
| --- | --- |
| `in_progress` | `running` |
| `pending_review` / `completed` | `completed` |
| `cancelled` | `canceled` |

Legacy rows without a delegation call id remain readable conversations but are
not continuable. Their existing cards keep the current fallback behavior.

## MCP Contract

### Continue Tool

Add a coordination-aware MCP tool:

```text
continue_delegation(task_id, task)
```

Input deliberately omits agent type, profile, and working directory. The
server resolves those values from the owned thread so a caller cannot switch
identity or resume an unrelated session.

A successful asynchronous acknowledgement includes:

```json
{
  "task_id": "new-run-id",
  "continued_from_task_id": "previous-run-id",
  "child_conversation_id": 803,
  "agent_type": "codex",
  "reused_session": true,
  "status": "running"
}
```

The existing `get_delegation_status` and `cancel_delegation` operate on the new
run id. A stale run id that is no longer the latest terminal run returns a
typed `stale_task_id` response rather than branching a thread.

### Continuation Flow

1. Claim and normalize the parent `continue_delegation` tool call.
2. Load the referenced run and verify direct-parent ownership.
3. Verify it is the latest terminal run and no run for the child is active.
4. Load the child conversation, agent type, external session id, folder, and
   profile audit identity.
5. Mint and durably reserve a new run before prompt admission.
6. Spawn or deduplicate an ACP connection using the existing external session
   id. Prefer `session/resume`; use the existing `session/load` fallback.
7. Send the prompt to the already-linked child conversation through a
   delegation-specific existing-conversation path.
8. Emit `DelegationStarted` and subsequent runtime/completion events against
   the new parent tool id and new task id.
9. Disconnect the child connection at terminal settlement, as today. The next
   run resumes the same external session again.

Lifecycle completion must be fenced by both task id and child connection (or
an equivalent run-generation token). A late event from an earlier connection
cannot settle a newer run.

### Replacement

The platform never silently creates a replacement from
`continue_delegation`. A resumability failure returns a typed error. The Skill
then calls `delegate_to_agent` with the same role and profile and records the
replacement relationship and reason. Business errors such as wrong ownership,
a stale task id, or a busy thread do not permit fallback.

## Card Summary Contract

The child LLM keeps its normal human-readable final result and appends one
hidden, versioned HTML comment. The comment is part of the raw final result but
is invisible in rendered Markdown.

Review example:

```html
<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,
 "important":0,"minor":2,"summary":"Two Minor findings remain."}
-->
```

Implementation example:

```html
<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done",
 "summary":"Implemented the cleaning component and automation tests.",
 "commits":[{"sha":"a1b2c3d","subject":"feat: add cleaning component"}],
 "tests":{"status":"passed","passed":14,"failed":0,
 "summary":"14/14 passing, output pristine"},"concerns":[],
 "report_file":".superpowers/sdd/task-3-report.md"}
-->
```

Review verdicts and implementation statuses are closed enums. Test counts are
optional when they cannot be measured reliably; agents must not invent them.
The parser uses `serde_json`, validates enums, numeric bounds, collection
lengths, and string lengths, and persists only validated data. Invalid or
missing summaries do not fail the delegation: the card falls back to its
status-only form and the complete result remains available in the child
conversation and parent tool output.

Supported implementation phases are `implementation` and `fix`. Supported
work statuses are `done`, `done_with_concerns`, `blocked`, and
`needs_context`. Work status is distinct from Broker lifecycle: an agent can
finish its turn successfully while reporting `blocked` work.

## UI Behavior

### Parent Timeline

Every initial or continued run renders as a separate card at its own parent
tool call. Different cards may carry the same `childConversationId`.

Always-visible content is:

- agent/profile identity, short task id, and lifecycle/work badge;
- artifact/Task and round label;
- a continuation indicator that names the shared child conversation;
- elapsed time and available runtime statistics;
- the existing open-session action.

Terminal cards also show the validated structured summary:

- Review: verdict, Critical/Important/Minor counts, and one-line conclusion.
- Implementation/fix: SDD work status, commit short ids, test summary, one-line
  delivery/blocker summary, and a concern indicator when applicable.

The full response is not duplicated inline. It remains in the child session
dialog and report file.

### Immutability

While a run is active, its card updates from that run's live binding. Once the
run becomes terminal, its lifecycle, runtime statistics, work result, and card
summary are frozen. Starting a later run must not change an earlier card.

The frontend therefore resolves historical card data by `task_id` or exact
`parent_tool_use_id`. It must not use the child conversation's latest task
projection to overwrite an older run. Existing child projection remains useful
for the latest overlay state and legacy cards only.

### Shared Session Dialog And Overlay

The open-session action on every run card opens the same child conversation.
It receives the selected task id and attempts to focus the corresponding
generation in the transcript; if a reliable turn anchor is unavailable, it
opens the full session without falsely highlighting a turn.

The top-right Sub-agent Overlay groups by child conversation rather than parent
tool id. It renders one row per reusable session with total run count and the
latest active or terminal state. Replacement sessions remain separate rows and
carry a replacement marker.

## Skill Routing Contract

`brainstorm-to-delivery` keeps a durable thread table in the SDD progress
ledger. The key is the work unit plus role and immutable profile identity.

| Work unit | First dispatch | Subsequent work on same unit |
| --- | --- | --- |
| Design + reviewer/profile | New reviewer | Continue that reviewer for Design revisions. |
| Plan + reviewer/profile | New reviewer | Continue that reviewer for Plan revisions. |
| Task N + Grok implementer | New Grok | Continue it for questions and fixes on Task N. |
| Task N + Codex reviewer | New independent Codex | Continue it for Task N re-reviews. |
| Task N+1 | New Grok and new Codex | Never reuse Task N threads. |
| Final whole-branch review | New Codex | Never continue a Task reviewer. |

Design and plan are separate work units even when they use the same reviewer
profile. Optional document reviewers remain optional; Codex remains mandatory
in the document review group. Optional document reviewers cannot become code
reviewers.

The ledger records each thread's child conversation id, latest task id, agent
type, profile id, state, and any replacement relationship. Compaction recovery
uses this ledger plus durable run records and never re-dispatches a completed
sequence from memory alone.

## Failure And Security Rules

- Continue only the latest terminal run owned by the current direct parent.
- Reject cross-parent ids without revealing child details.
- Reject continuation when the child is busy.
- Do not allow tool input to override agent, profile, workspace, or external
  session identity.
- Persist a failed terminal run if resume or prompt admission fails after run
  reservation; never strand `running`.
- Cancel only the selected active run. Never delete prior cards or the child
  transcript.
- Permit a same-role/profile fresh replacement only for typed resumability
  failures: missing historical session, unsupported resume/load, corrupt
  session, or failed resume handshake.
- Do not fall back for authorization, stale-id, route-policy, or concurrency
  errors.
- Never substitute agent type. Required Grok or Codex unavailability remains a
  hard delivery blocker.
- Treat card summaries as bounded display data only. Never execute or interpret
  summary text as commands.

## Validation

### Backend

- Migration and legacy backfill tests.
- Tool-list, schema, dispatch, acknowledgement, status, and cancel tests for
  `continue_delegation`.
- Ownership, stale-id, missing-id, deleted-child, and busy-thread tests.
- Same child conversation plus new task id across multiple runs.
- `session/resume`, `session/load` fallback, and typed resumability failures.
- Prompt-admission and terminal-persistence failure tests.
- Late old-connection completion cannot settle the new generation.
- Restart/cold-load status and card-binding recovery.
- Summary parser acceptance, bounds, invalid-input fallback, and immutability.

### Frontend

- Several cards can share one child conversation and retain independent run
  status and summaries.
- A running later run cannot reopen or mutate a terminal earlier card.
- Review and Implementation summary rendering and invalid-summary fallback.
- Overlay grouping by child conversation with run count and latest state.
- All run cards open the same session and request the correct run focus.
- Replacement sessions remain visibly separate.
- Responsive screenshot checks at desktop and mobile widths.

### Skill Forward Tests

Use conversation 800's shape as the RED scenario. Four review rounds across
three reviewers must produce three child conversations and twelve durable runs,
not twelve child conversations.

Additional scenarios verify:

- Design and plan re-review continue the matching reviewer/profile thread.
- A Task fix continues the Task's Grok implementer.
- A Task re-review continues the Task's independent Codex reviewer.
- The next Task starts fresh Grok and Codex threads.
- Final whole-branch review always starts a fresh Codex thread.
- A resumability failure creates a recorded same-role/profile replacement.
- Business errors and unavailable required agents do not trigger substitution.

Run the affected Rust tests for desktop, server, and `codeg-mcp` surfaces, plus
frontend tests, lint, build, and targeted visual verification.

## Acceptance Criteria

- A same-unit continuation creates a new run and card but reuses the exact child
  conversation and external agent session.
- Historical run cards remain immutable through later runs and process restart.
- Each run remains independently queryable and cancelable by task id.
- Conversation 800's four-by-three review pattern requires only three child
  conversations.
- Review and Implementation cards show validated structured summaries when
  supplied and degrade without inventing data when not supplied.
- New Tasks and the final global Codex review preserve fresh-session isolation.
- Resume failures can fall back to a recorded same-role/profile replacement;
  authorization and role failures cannot.
- The Skill retains optional document review models and the mandatory Codex
  review role.
