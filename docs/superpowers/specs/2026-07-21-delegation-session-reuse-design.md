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
- Recover an unexpectedly interrupted run in the same child session without
  reopening or rewriting the canceled run.
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
- Do not require the platform to parse Skill artifact semantics (Design vs Plan
  vs Task). Platform recovery rails use opaque thread lineage and optional
  opaque work-unit keys supplied by the Skill, not document-path interpretation.

## Terminology And Invariants

**Delegation thread** means one durable child conversation plus its external
agent session. It owns the reusable context.

**Delegation run** means one parent MCP tool invocation and one child turn. It
owns a task id, lifecycle, runtime statistics, result summary, and parent tool
card.

The following invariants are binding:

1. A child conversation has one `(external_id, agent_type)` identity for its
   entire lifetime. Resume/load that would attach a different external id is a
   typed resumability failure, never a silent rewrite.
2. Every run has a unique `task_id` and exactly one `parent_tool_use_id`.
3. Multiple runs may reference the same `child_conversation_id`.
4. At most one run for a child conversation may be non-terminal (`reserving` or
   `running`) at a time.
5. A terminal run and its card summary are immutable after the settlement
   transaction commits (including any terminal runtime-stats write that is part
   of that same transaction).
6. A continuation uses the same agent type, external session, workspace, and
   non-secret launch snapshot as its thread.
7. A replacement starts a new child conversation but keeps an explicit link to
   the failed thread in the SDD ledger and run metadata.
8. The run table is authoritative for MCP status, cancel, and per-card history.
   Conversation columns remain a latest-run projection only.

## Document Precedence

This design amends and partially supersedes earlier one-shot assumptions:

| Prior document | Relationship |
| --- | --- |
| `2026-07-16-delegation-route-reliability-design.md` | Lifecycle and route reliability still apply; per-run identity and settlement keys move from root `delegation_call_id` to `delegation_task_runs.task_id`. |
| `2026-07-17-event-driven-delegation-join-design.md` | Join/Broker coordination still applies; task keys and completion fences use run `task_id` + generation + connection incarnation. |
| `2026-07-19-delegation-continuation-design.md` | Parent-turn suspension remains orthogonal. This design does **not** use parent continuation arms for child session reuse. V1 ownership transfer that stops at a live parent connection remains for *parent* continuation; child reuse uses `resume_existing_only` after disconnect-at-settlement. |
| `2026-07-21-delegation-task-id-prefix-recovery-design.md` | Prefix recovery remains, but scans parent-scoped **run** rows, not only conversation root call ids. |
| `2026-07-21-acp-termination-causality-audit-design.md` | Termination audit remains the provenance source for unexpected-cancel continuability. |

Where this design and a prior one-shot lifecycle rule conflict on child reuse,
this design wins for reusable runs. Parent-continuation and route-policy
invariants outside that scope remain in force.

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
duplicate conversation row is created for a continued turn. Replacement creates
a new conversation with a new external id (it must not reuse the failed
thread's external id, which would collide with the unique index).

### Durable Run Model

`delegation_task_runs` contains these required fields (runtime-stat fields may
remain flattened as they are on `conversation` today):

| Column | Purpose |
| --- | --- |
| `task_id` | Primary key for status, cancellation, and card identity. |
| `root_task_id` | First run in the reusable thread. |
| `previous_task_id` | Immediately preceding run in the same thread. |
| `generation` | One-based run number within the child conversation. |
| `parent_conversation_id` | Owning parent; used for authorization. |
| `parent_tool_use_id` | Exact parent card binding for this run. |
| `child_conversation_id` | Shared child session identity. |
| `agent_type` | Immutable run-time routing snapshot; must equal the child conversation's `agent_type` at creation. |
| `profile_id` | Optional immutable profile identity used for audit/fallback. |
| `workspace_path` | Normalized absolute workspace recorded at root creation. |
| `route_fingerprint` | Stable non-secret hash of agent type + profile id + workspace + launch-snapshot version. |
| `launch_snapshot_version` | Version tag for the snapshot schema. |
| `mode_id` / `config_values_json` | Resolved non-secret launch-option snapshot for exact continuation (allowlisted keys only). |
| `task_preview` | Server-derived display preview only (see Task preview rules). Never store the full prompt. |
| `request_fingerprint` | Non-reversible hash used for exact duplicate detection; never logged as the prompt. **Both** `delegate_to_agent` and `continue_delegation` use the same canonicalization: fixed field order over (tool name, full task text NFC-normalized, `work_unit_key` or empty, `replaces_task_id` or empty, `replacement_reason` or empty, target `task_id` or empty, stored `route_fingerprint` hex). Absent optional fields are empty strings, not omitted. |
| `admission_class` | Set at `reserving` insert: `normal_revision`, `unexpected_continue`, or `replacement`. Survives restart. Authoritative for which counter (if any) is charged at prompt-admission success. |
| `reached_running_at` | Nullable timestamp set only when the run first successfully admits a prompt and transitions to `running`. Durable signal for \"ever reached running\" / lineage established. |
| `lineage_root_task_id` | Root of replacement-aware lineage: equals `root_task_id` for the original thread; for a replacement run, equals the replaced thread's `lineage_root_task_id`. Used for shared recovery rails. |
| `work_unit_key` | Opaque Skill-supplied key (≤ 200 chars) when the caller participates in budgeted orchestration; joins platform budget rows under `(parent_conversation_id, work_unit_key)`. Nullable only for ad-hoc non-orchestrated one-shots. |
| `legacy_parent_tool_use_id` | Non-unique copy of a historical parent tool id when the unique `parent_tool_use_id` must be NULL after migration collision handling. |
| `history_only` | Boolean; true for migration losers / non-continuable backfills. Continuability requires `history_only = false`. |
| `status` / `error_code` | Durable run lifecycle (see status enum below). |
| termination audit fields | Structured source, reason, and request/correlation id for cancellation or disconnect. |
| `started_at` / `finished_at` | Per-run timing. |
| runtime-stat columns | Per-run tool, file, and line-change projection. |
| `card_summary_json` | Validated optional Review or Implementation summary (frontend-only display data). |
| `child_turn_anchor` | Optional child message/turn anchor for open-session focus; null when unavailable. |
| `child_connection_id` | Live connection incarnation while non-terminal; used for settlement fencing. |
| replacement columns | Optional `replaced_task_id` / reason code when this run is a recorded same-role replacement. |

#### Run status enum

Closed set:

| Status | Terminal? | Notes |
| --- | --- | --- |
| `reserving` | no | Durable claim inserted before ACP spawn / resume. |
| `running` | no | Prompt successfully enqueued to the agent; live connection registered; counters already charged in the same transaction. |
| `completed` | yes | Successful child turn settlement. |
| `failed` | yes | Business/runtime failure after reservation. |
| `canceled` | yes | Cancellation or unexpected interruption settlement. |

There is no separate long-lived intermediate beyond `reserving` → `running`.
If resume or prompt admission fails after reservation, the run is settled to
`failed` or `canceled` with a typed error in the same transaction that clears
the active-run claim. Never strand a non-terminal run across process restart
without startup reconciliation.

#### Indexes

Required indexes include:

- unique `(child_conversation_id, generation)`;
- unique `(parent_conversation_id, parent_tool_use_id)` where `parent_tool_use_id`
  is non-null;
- lookup indexes for parent, child, root, previous task ids, and
  `lineage_root_task_id`;
- lookup index `(parent_conversation_id, work_unit_key)` where `work_unit_key`
  is non-null (orchestrated same-key detection);
- partial unique index: one non-terminal run per child, e.g.
  `UNIQUE(child_conversation_id) WHERE status IN ('reserving', 'running')`;
- partial unique index: at most one non-terminal gen-1 run per orchestrated
  key, e.g. `UNIQUE(parent_conversation_id, work_unit_key)
  WHERE status IN ('reserving', 'running') AND generation = 1
  AND work_unit_key IS NOT NULL`.

First-dispatch serialization for the same orchestrated key: when
`work_unit_key` is present and no prior run for that key has
`reached_running_at IS NOT NULL`, use `INSERT … ON CONFLICT DO NOTHING` for the
work-unit budget row
(reuse an existing zero-lineage row from a dead pre-admission attempt) and
insert the generation-1 run in the same transaction. Concurrent dual first-
dispatches that both pass the never-running check are fenced by a partial
unique index: at most one non-terminal gen-1 run per
`(parent_conversation_id, work_unit_key)` while status is
`reserving`/`running`. Losers return `busy_thread` /
`invalid_replacement` rather than two independent roots. The Skill still
serializes same-key dispatch as best-effort UX.

The partial unique index is the **authoritative** concurrency fence for dual
`continue_delegation` (and dual initial-spawn races that share a child). The
application-level "no active run" check is best-effort UX only. Insert-loses:
exactly one insert of generation N+1 succeeds; the loser returns typed
`busy_thread` (the winner may still be `reserving` — callers treat that as
busy, not a separate error code).

#### Task preview rules

`task_preview` is **never** taken verbatim from the client as durable storage of
the full prompt. The server derives it at run creation **in memory only**:

1. Require UTF-8 task text; otherwise store empty preview.
2. Apply fail-closed redaction to the **full** admitted task text first:
   replace substrings matching a fixed secret pattern set (API key prefixes,
   `Bearer ` tokens, `sk-`/`ghp_`/`github_pat_`/`glpat-`/`xox`/`AKIA` class
   tokens, `-----BEGIN` PEM blocks) with `[redacted]`.
3. Then take the first 200 Unicode scalars of the redacted text (bound always
   applies after redaction, so `[redacted]` expansion cannot exceed 200).
4. No second copy of the full prompt is written to SQLite. Migration backfill
   sets `task_preview` NULL when the original prompt is not recoverable.

#### Launch snapshot and secrets

The resolved mode and config snapshot contains only **allowlisted non-secret**
ACP configuration options already accepted by delegation profiles (ids, mode
names, non-secret option values). Credentials and provider secrets are never
copied into a run. At continuation time:

1. Non-secret launch options come from the **root run's** immutable snapshot
   (not a re-resolved, possibly edited profile).
2. Secrets are re-resolved live at launch: if `profile_id` is present, load
   current credentials for that profile; if the profile is deleted or cannot
   be launched with the recorded non-secret snapshot, return typed
   `unresumable` / configuration-blocked (Skill may replace or block). Secret
   rotation that still launches the same profile is allowed and does not
   change the non-secret snapshot.
3. Profile-less / ad-hoc delegations re-resolve credentials from the same
   agent-default secret source used by the original launch path; they remain
   continuable only when a full launch snapshot (including workspace and
   allowlisted config) was recorded at generation 1.

#### Projection fence

Conversation columns remain a latest-run projection for sidebar/overlay during
the transition. Add `delegation_run_generation` (integer, nullable for
pre-migration rows) on `conversation`.

Run creation, terminal settlement, and runtime projection update the canonical
run plus the conversation's latest-run projection in **one transaction**. The
conversation update is **monotonic**:

```text
UPDATE conversation
SET <latest-run projection fields>,
    delegation_run_generation = :incoming_generation
WHERE id = :child_id
  AND (
    delegation_run_generation IS NULL
    OR delegation_run_generation <= :incoming_generation
  );
```

A delayed older write therefore cannot replace a newer projection. Terminal
runtime-stats are written inside the settlement transaction; after commit, no
further writes are allowed for that run (true freeze).

#### Store re-keying

`DelegationTaskStore` and Broker paths that today key on
`conversation.delegation_call_id` move their authoritative key to
`delegation_task_runs.task_id` for:

- load / settle / cancel / status;
- `reconcile_running` (run rows + conditional conversation projection);
- task-id prefix recovery (`resolve_unique_owned_prefix`), which must scan
  parent-scoped **run** rows;
- Broker completion and cancel-by-connection paths that resolve a run from
  `(child_connection_id, task_id, generation)`.

Conversation `delegation_call_id` remains the immutable **root** linkage for
backward compatibility and equals generation-1 `task_id`.

### Migration

The migration creates one generation-1 run for every non-deleted delegate
conversation with a non-null `delegation_call_id`.

**Backfill identity rules:**

- `task_id = conversation.delegation_call_id` (mandatory — preserves in-flight
  parent MCP task ids across upgrade).
- `root_task_id = task_id`, `previous_task_id = NULL`, `generation = 1`.
- `parent_conversation_id` / `parent_tool_use_id` / `child_conversation_id` /
  `agent_type` copied from the conversation.
- Launch snapshot fields (`workspace_path`, `route_fingerprint`,
  `mode_id` / `config_values_json`, `launch_snapshot_version`): populated only
  when reconstructible from durable records. If not reconstructible, leave
  null and mark the run **non-continuable** (readable history only).
- Existing task status wins. For older rows with a null task status, map
  conservatively from conversation status:

| Conversation status | Backfilled run status |
| --- | --- |
| `in_progress` | `running` (then immediately subject to startup reconcile) |
| `pending_review` / `completed` | `completed` |
| `cancelled` | `canceled` |

**Preflight / collision handling:**

- Duplicate `delegation_call_id` values: keep the newest non-deleted child;
  other rows remain conversations but do not receive a continuable run
  (log + skip; do not invent ids).
- Duplicate non-null `(parent_conversation_id, parent_tool_use_id)` among
  candidates (ordered after call-id dedupe):
  1. Winner = newest non-deleted child by `(created_at, id)`.
  2. Winner keeps `parent_tool_use_id` and `history_only = false` only if a
     launch snapshot is also reconstructible.
  3. Losers: copy original tool id into `legacy_parent_tool_use_id`, set
     `parent_tool_use_id = NULL`, set `history_only = true` (never continuable).
  4. Migration never aborts on historical duplicates.
- Null or empty `parent_tool_use_id`: normalize empty string to NULL before
  insert; create a generation-1 run with `history_only = true` for status
  recovery; open-card binding is legacy fallback-only; not continuable.
- Missing `external_id`: history-only, non-continuable.
- Deleted parents: still backfill child runs for local history; continuation
  ownership checks fail closed.

Legacy rows without a delegation call id remain readable conversations but are
not continuable. Their existing cards keep the current fallback behavior.

### Startup reconciliation

Before the listener accepts requests, extend `reconcile_running` to:

1. Settle every non-terminal **run** with `host_restarted` in the same
   transaction that updates the conversation latest-run projection (monotonic
   generation guard), using the status split:
   - prior `reserving` → `failed` / `host_restarted`
   - prior `running` → `canceled` / `host_restarted`
2. Preserve structured termination audit so the Skill can decide whether the
   work unit is eligible for unexpected-interruption recovery.
3. Never leave a non-terminal run after the gate returns.
4. **Budget impact of restart:**
   - A `reserving` run that never reached `running` did not consume unexpected-
     continue or replacement counters (increment happens only at admission).
   - A `running` run force-settled as `canceled`/`host_restarted` **keeps** any
     counter it already consumed at admission; do not invent a refund. The
     Skill may attempt another unexpected-cancel continue only while budget
     remains.

`host_restarted` on a formerly-`running` run is treated as unexpected process
exit for continuability eligibility when the external session still exists.

## MCP Contract

### Continue Tool

Add a coordination-aware MCP tool:

```text
continue_delegation(task_id, task)
```

Input deliberately omits agent type, profile, and working directory. The
server resolves those values from the owned thread so a caller cannot switch
identity or resume an unrelated session.

Parent authentication comes from the MCP companion's bound parent conversation
(the same binding `delegate_to_agent` uses). The run's `parent_conversation_id`
must match that binding; `task_id` alone is never sufficient.

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

`reused_session: true` means the external session id was verified unchanged
after `session/resume` or `session/load`. It is false only if the platform had
to refuse (error path); success path never attaches a new session.

The existing `get_delegation_status` and `cancel_delegation` operate on the new
run id. Parent-facing MCP reports keep the existing `DelegationTaskReport.text`
shape. **Structured `card_summary_json` is frontend display data only and is
never echoed into parent-facing MCP tool results** (prevents prompt-injection
from untrusted child-authored summary text). The complete child result remains
available in the child conversation.

### Continuability and typed errors

A referenced run is continuable when all of the following hold:

1. Direct-parent ownership verified.
2. `history_only = false`.
3. It is the latest run for its child.
4. It is terminal.
5. No non-terminal run exists for the child.
6. This **child conversation** has not been superseded: no run exists with
   `replaced_task_id` pointing at any run belonging to this child (else
   `not_continuable` — continue the replacement child instead). The
   replacement child itself remains continuable for later same-unit rounds;
   only the replaced source child is locked out.
7. A complete launch snapshot exists (workspace + allowlisted config + external
   id + agent type).
8. Either:
   - the prior run is `completed`, or `failed` with a **revision-eligible**
     terminal reason — including (a) ordinary child/runtime failure after a
     prompt was admitted, and (b) only `error_code = host_restarted` from a
     pre-admission `reserving` run whose `admission_class` is
     `normal_revision` or `unexpected_continue` and that still has a complete
     launch snapshot + external session id; the **next** continue re-creates a
     `reserving` run that **inherits** that `admission_class` so the correct
     counter is still charged at the first successful `running` promotion; or
   - the prior run is `canceled` and its structured termination audit
     identifies unexpected transport disconnect, process exit /
     `host_restarted` (only when the run had reached `running`), session
     loss, or non-user/non-parent `interrupted` turn (interruption recovery);
     the next continue uses `admission_class = unexpected_continue`.

**Not continuable via `continue_delegation`:** explicit user/parent
cancellation; policy/route-policy/authorization failure; `budget_exhausted`;
`not_supported`; any pre-admission `reserving` failure other than
`host_restarted` (including `unresumable` and config-blocked); pre-admission
`host_restarted` when `admission_class = replacement` (use replacement retry
via `delegate_to_agent` instead — never inherit `replacement` onto the continue
path); generation-1 without external session (fresh first-dispatch); and any
other rail/policy `error_code` that is not revision-eligible above.

**Pre-admission gen-1 re-dispatch (orchestrated path):** when a gen-1 run dies
in `reserving` before external session creation, lineage is not established.
The Skill may call `delegate_to_agent` again with the **same** `work_unit_key`
and **without** `replaces_task_id`. The platform existence check ignores prior
runs with `reached_running_at IS NULL` (and never consumed a counter) for that key.
Work-unit budget rows are reused via `ON CONFLICT DO NOTHING` (counters remain
0). This is a fresh first-dispatch, not a replacement, and does not consume
the replacement rail.

**Pre-admission replacement retry:** when a replacement gen-1 dies in
`reserving` before reaching `running`, the replacement counter was never
charged. The Skill retries via `delegate_to_agent` with the same
`replaces_task_id`, `replacement_reason`, and `work_unit_key`. The new run
sets `admission_class = replacement` fresh (not via continue inheritance).

**Startup `host_restarted` status split (mandatory):**

| Prior non-terminal status | Settled status | Continuability |
| --- | --- | --- |
| `reserving` + snapshot + external id, class ≠ `replacement` | `failed` / `host_restarted` | Continue with **inherited** class (charge at next `running`) |
| `reserving` + class = `replacement` | `failed` / `host_restarted` | `not_continuable` on continue; replacement retry via `delegate_to_agent` |
| `reserving` gen-1 without external id | `failed` / `host_restarted` | Fresh first-dispatch (budget-row reuse; never-running priors ignored) |
| `running` | `canceled` / `host_restarted` | Next continue uses `admission_class = unexpected_continue` |

Typed error envelope (stable, redacted — no child details on cross-parent).
Codes used by `continue_delegation` and, where noted, `delegate_to_agent`:

| Code | Tool(s) | When |
| --- | --- | --- |
| `not_found` | both | Unknown task id (or cross-parent without revealing existence). |
| `stale_task_id` | continue | Task exists on the child but is not the latest terminal run. |
| `busy_thread` | continue | A non-terminal run exists for the child, or concurrent insert lost. |
| `not_continuable` | continue | Latest terminal run fails eligibility (explicit cancel, history-only, replaced lineage, missing snapshot, unknown-origin cancel). |
| `unresumable` | continue | Resume/load failed, external id mismatch, or launch config/profile unavailable. |
| `not_supported` | continue | Agent type is not capability-enabled for child session reuse (no resume attempted). |
| `budget_exhausted` | both | Unexpected-continue rail, generation ceiling, or replacement rail refused. |
| `duplicate_parent_tool` | both | Same `parent_tool_use_id` already bound under this parent. If durable `request_fingerprint` matches the new request → idempotent return of that run (even if terminal or non-terminal). If fingerprint differs or is missing on a legacy row → reject without overwrite. Never guess from `task_preview`. |
| `invalid_replacement` | delegate | `replaces_task_id` present but fails server eligibility (wrong reason vs durable state, not latest eligible source, etc.). |

Error precedence (first match wins):

`not_found` → parent-tool exact-duplicate handling (**before** lifecycle
errors): matching `request_fingerprint` → idempotent success return of existing
run; mismatched or legacy-missing fingerprint → `duplicate_parent_tool` reject
→ `not_supported` (only after ownership resolved) → `busy_thread` →
`stale_task_id` → `not_continuable` → `budget_exhausted` → `unresumable` →
`invalid_replacement`.

Run `error_code` values such as `host_restarted` are an internal settlement
namespace and are not the same as the caller-facing MCP codes above.

### Continuation Flow

1. Claim and normalize the parent `continue_delegation` tool call; resolve
   parent from MCP binding.
2. Load the referenced target `task_id` run for ownership and route material
   only (`not_found` on failure). Do not yet evaluate busy/stale.
3. Compute `request_fingerprint` using tool name, full task text, work_unit_key,
   target `task_id`, and the target thread's durable `route_fingerprint`
   (deterministic canonical serialization: fixed field order, NFC text,
   fixed integer encodings; include `route_fingerprint` as its stored value).
4. Look up any existing run with the same
   `(parent_conversation_id, parent_tool_use_id)`. Matching fingerprint →
   idempotent return (**before** busy/stale/continuability). Mismatch or
   legacy-missing fingerprint → `duplicate_parent_tool` reject.
5. If agent type is not capability-enabled for reuse, return `not_supported`.
6. Verify continuability (history_only, latest, terminal, not superseded child,
   eligible terminal class, complete snapshot) and best-effort "no active run".
7. Load the child conversation, agent type, external session id, workspace, and
   immutable root launch snapshot. Validate `run.agent_type == conversation.agent_type`.
8. Derive `admission_class` for the new run:
   - if the referenced run is `canceled` with unexpected provenance →
     `unexpected_continue`;
   - else if the referenced run is `failed`/`host_restarted` from `reserving`
     with class ∈ {`normal_revision`, `unexpected_continue`} → **inherit**
     that class;
   - else if class would be `replacement` → reject `not_continuable` (use
     replacement retry on `delegate_to_agent`);
   - else → `normal_revision`.
9. In **one transaction**:
   - If generation would exceed 100, abort with `budget_exhausted`.
   - If `admission_class = unexpected_continue`: create budget rows lazily and
     preflight `unexpected_continue_count < 2` on each applicable row. If
     either is already at limit → `budget_exhausted` without insert.
   - Insert the new run in `reserving` with the next generation,
     `admission_class`, and `request_fingerprint`.
   - Partial unique index + unique `(child, generation)` ensure exactly one
     winner under concurrent doubles; loser → `busy_thread`.
10. Open an ACP connection in **`resume_existing_only` mode**:
    - Prefer `session/resume`, then `session/load` if resume is unsupported or
      fails non-terminally.
    - **Never** fall through to `session/new` on this path.
    - After resume/load, verify the returned external session id equals the
      recorded conversation `external_id`. Mismatch or resume failure → settle
      run `failed` with `unresumable` **without** incrementing any counter, and
      do not rewrite identity.
    - Do not deduplicate against a still-retiring prior connection: wait for
      prior disconnect completion or use a new connection incarnation id.
11. Enqueue the prompt on the already-linked child conversation (delegation
    existing-conversation path). If enqueue fails, settle `failed` without
    charging counters and disconnect.
12. In **one transaction** after successful prompt enqueue:
    - Charge counters according to durable `admission_class` on this run:
      - `unexpected_continue` → conditional +1 on unexpected-continue rails;
        zero `rows_affected` → cancel the enqueued prompt if the transport
        allows, settle `failed`/`budget_exhausted`, disconnect;
      - `normal_revision` → no counter charge;
      - `replacement` never appears on this path.
    - Set status `running`, set `reached_running_at`, register
      `child_connection_id` incarnation, emit `DelegationStarted` against the
      new parent tool id and new task id.
13. On terminal settlement, write run + monotonic conversation projection +
    optional validated card summary in one transaction; disconnect the child
    connection. Counters are never refunded after a successful step 12.

If the process crashes after prompt enqueue but before step 12 commits, startup
reconcile settles the still-`reserving` run as `failed`/`host_restarted` without
charging counters. The Skill may continue with inherited `admission_class`.
Operators must accept that a rare orphaned agent turn may exist externally;
the platform does not invent a counter charge without a durable `running` row.

Lifecycle completion is fenced by task id, generation, and child connection
incarnation. The live lifecycle path resolves the run from the Broker's
registered child connection identity and verifies all three values before
settlement. A cold fallback queries the single non-terminal run for the child;
it never reads the conversation's immutable root `delegation_call_id` to settle
a continued run. A late event from an earlier connection therefore cannot
settle a newer run.

### Capability gate / rollout

Child session reuse is enabled only for agent types that advertise reliable
same-id `session/resume` or `session/load` after disconnect. Initial enablement
is capability-gated per agent type (mirroring the continuation design's
per-agent rollout). Disabled agents return `not_supported` from
`continue_delegation` (no resume is attempted). The Skill may create a
same-role replacement or block; it must not treat `not_supported` as a
transient resume glitch.

### Platform recovery rails (enforceable)

The platform enforces hard safety rails on durable lineage. These are
independent of Skill document semantics.

**Lineage root.** Every run carries `lineage_root_task_id`. For the first
thread it equals `root_task_id`. For a replacement it equals the replaced
thread's `lineage_root_task_id`, so counters share across original and
replacement children. A replacement therefore inherits whatever unexpected-
continue budget remains (may be zero if two recovers were already spent).

**Budget table** `delegation_lineage_budgets`:

- PK `lineage_root_task_id`
- `unexpected_continue_count` INTEGER NOT NULL DEFAULT 0
- `replacement_count` INTEGER NOT NULL DEFAULT 0

Optional parallel table `delegation_work_unit_budgets` PK
`(parent_conversation_id, work_unit_key)` with the same counters.

| Rail | Limit | What increments it |
| --- | --- | --- |
| Unexpected-cancel continues | 2 | Conditional +1 only when a run with `admission_class = unexpected_continue` transitions `reserving` → `running` |
| Recorded replacements | 1 | Conditional +1 only when a run with `admission_class = replacement` transitions `reserving` → `running` |
| Generation per child | 100 hard | Reject continue that would create generation > 100 with `budget_exhausted` |

**Authoritative concurrency for counters:** always
`UPDATE … SET count = count + 1 WHERE key = ? AND count < limit` and require
`rows_affected = 1`, executed in the **same transaction** as the
`reserving` → `running` status transition. Application pre-checks are
best-effort UX only. There is no unique index on `replaced_task_id` alone;
the conditional counter update is the fence for dual replacement admissions.

**No refund path.** Counters never increment until durable admission success
(`running`). Failed resume/spawn while still `reserving`, and
`host_restarted` settlement of `reserving` rows, leave counters unchanged.
This avoids crash-window ambiguity between "never admitted" and "admitted but
not yet marked running."

Rules:

1. The initial generation-1 run does not consume either counter.
2. Normal continues after `completed` or `failed` do **not** consume the
   unexpected-cancel counter. Multi-round Design/Plan re-review is therefore
   not capped at 2; the generation ≤ 100 hard ceiling still applies.
3. Unexpected-cancel continue with `rows_affected = 0` → `budget_exhausted`,
   no run created.
4. Second replacement attempt for the same lineage → `budget_exhausted`.
5. If both lineage and work-unit budget rows apply, **both** conditional
   updates must succeed in the same transaction (stricter wins).

Skill still owns routing (which thread to continue) and may stop earlier than
the platform rails; it cannot exceed them.

**Operational note:** because counters are never refunded after a successful
`running` admission, repeated host instability during recovery alone can
exhaust the unexpected-continue rail and the one-replacement rail and force
user escalation. That escalation is expected platform behavior, not a Skill
bug.

### Replacement

The platform never silently creates a replacement from
`continue_delegation`. A resumability failure returns typed `unresumable`
(or `not_supported` when capability-disabled).

#### When `work_unit_key` is present (orchestrated Skill path)

`delegate_to_agent` **must** include verified replacement linkage whenever the
same `(parent_conversation_id, work_unit_key)` already has any prior run with
`reached_running_at IS NOT NULL` (lineage established):

- Prior runs that never set `reached_running_at` do **not** establish lineage;
  a same-key re-dispatch without `replaces_task_id` is a fresh first-dispatch.
- If a lineage exists and is still resumable via `continue_delegation`, the
  Skill must continue — a new `delegate_to_agent` with the same key and
  without `replaces_task_id` returns `invalid_replacement` / policy reject
  (`duplicate_work_unit` may be aliased to `invalid_replacement` in v1).
- If the Skill intends a same-role recovery child, it **must** supply:
  - `replaces_task_id` (latest terminal run of the prior child in that unit)
  - `replacement_reason` ∈ {`unresumable`, `budget_exhausted_continue`,
    `not_supported`} matching durable server state for that run
  - the same `work_unit_key`
  - `admission_class = replacement` on the new generation-1 run

Omitting `replaces_task_id` while reusing a key with an established lineage is
a hard reject — this closes the bypass that would otherwise mint an independent
thread after `budget_exhausted`.

#### When `work_unit_key` is absent (ad-hoc path)

A new child without linkage is allowed (independent thread, no shared budget).
This path is for non-orchestrated one-shots, not brainstorm-to-delivery.

#### Server verification when `replaces_task_id` is present

All checks run in one transaction with the conditional replacement counter:

1. Direct-parent ownership of the replaced run.
2. Same agent type and same profile id (when either side has a profile).
3. Same normalized workspace as the replaced thread's launch snapshot.
4. Replaced run is terminal and is the latest run on its child.
5. `replacement_reason` matches durable state, e.g.:
   - `unresumable` only if the latest continue/initial admission failed with
     `unresumable` or the thread is missing a resume-capable external session;
   - `budget_exhausted_continue` only if unexpected-continue budget is already
     at 2;
   - `not_supported` only if the agent type is capability-disabled.
6. Lazily ensure budget rows exist and **preflight** `replacement_count < 1`
   on lineage and (when present) work-unit rows. If either is already at
   limit → `budget_exhausted` without insert.
7. Insert the new generation-1 run in `reserving` with `replaced_task_id`,
   inherited `lineage_root_task_id`, and the supplied `work_unit_key`.
8. Spawn the new child and enqueue the first prompt. On spawn or prompt-enqueue
   failure: settle `failed` with typed error; **no counter increment**.
9. On durable prompt-admission success, in **one transaction**:
   - Conditional
     `UPDATE … SET replacement_count = replacement_count + 1
      WHERE … AND replacement_count < 1` on lineage and (when present)
     work-unit rows; any zero `rows_affected` → best-effort cancel the
     enqueued prompt, settle `failed` with `budget_exhausted` without
     promoting to `running`.
   - Else set status `running`, set `reached_running_at`, and emit
     `DelegationStarted`.

Once a replacement has `reached_running_at` set, a second replacement for the
same lineage is rejected with `budget_exhausted` even if that running
replacement later fails or is canceled — the Skill escalates to the user.

Business errors (`not_found`, `stale_task_id`, `busy_thread`, authorization,
route-policy) never permit replacement fallback and must not send
`replaces_task_id`.

### Recovery budget authority (Skill vs platform)

**Platform** enforces the hard rails in "Platform recovery rails" above
(unexpected-cancel continue ≤ 2 per lineage, replacement ≤ 1 per lineage,
generation soft ceiling, optional work-unit key dual keying).

**Skill / SDD ledger** owns routing tables and may apply stricter policy. It
records human-readable work-unit keys aligned with:

| Work unit | Ledger / work_unit_key material |
| --- | --- |
| Design review | `design\|{absolute_doc_path}\|{role}\|{profile_id\|none}` |
| Plan review | `plan\|{absolute_plan_path}\|{role}\|{profile_id\|none}` |
| Task implementer | `task\|{task_index}\|implementer\|{profile_id\|none}` |
| Task reviewer | `task\|{task_index}\|reviewer\|{profile_id\|none}` |
| Final whole-branch review | `final_review\|{branch_ref}\|reviewer\|{profile_id\|none}` |

Skill policy (mirrors platform rails; may stop earlier):

1. The initial run does not consume the budget.
2. At most two unexpected-interruption continues per work unit.
3. Budget is shared across original and replacement via the same
   `work_unit_key` and platform `lineage_root_task_id`.
4. At most one fresh same-role/profile replacement per work unit.
5. After rails are exhausted, stop and ask the user.

Compaction recovery uses the ledger plus durable run and budget rows and never
re-dispatches a completed sequence from memory alone.

Recovery starts a new child turn, not the interrupted process instruction. The
prompt must tell the child to inspect the current repository/artifacts, treat
partial prior reasoning as provisional, and recreate any final report that was
not durably written. Read-only reviewers can reuse their accumulated analysis;
implementers must first audit partial filesystem changes and rerun covering
tests before reporting completion.

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

### Extraction and validation

- Terminal settlement has access to the raw final assistant message (before
  Markdown rendering). The Broker extracts the **last** well-formed
  `<!-- codeg-card-summary-v1 ... -->` block in that raw text. Earlier echoed
  examples are ignored.
- Parser uses `serde_json`, validates enums, bounds, collection lengths, and
  string lengths. Persist only validated data into `card_summary_json`.
- Concrete bounds (v1):
  - `summary` ≤ 240 chars;
  - `commits` ≤ 20 items; each `sha` ≤ 64, `subject` ≤ 200;
  - `concerns` ≤ 20 items; each ≤ 240 chars;
  - counts `critical`/`important`/`minor`/`passed`/`failed` ∈ [0, 1_000_000];
  - `report_file` ≤ 512 chars, workspace-relative, no `..` segments.
- Verdict and counts are independent display fields (no cross-field reject for
  `approve` with non-zero critical); the UI may still show both honestly.
- Invalid or missing summaries do not fail the delegation: the card falls back
  to its status-only form.
- Completion events carry the validated summary to the frontend for the
  specific `task_id` / `parent_tool_use_id`. Parent MCP tool results do **not**
  include the structured summary.

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
projection to overwrite an older run.

**Cold projection transport:** desktop/web expose an authorized query/snapshot
DTO keyed by `task_id` (or exact parent tool id) that returns that run's
immutable fields and summary. Historical cards load via this path. Existing
child projection cache remains useful for the latest overlay state and legacy
cards only; it must not backfill older run cards.

### Shared Session Dialog And Overlay

The open-session action on every run card opens the same child conversation.
It receives the selected task id and, when `child_turn_anchor` is non-null,
attempts to focus that turn; otherwise it opens the full session without
falsely highlighting a turn. V1 may leave anchors null (full session only)
without blocking the rest of the feature.

The top-right Sub-agent Overlay groups by child conversation rather than parent
tool id. It renders one row per reusable session with total run count and the
latest active or terminal state. Replacement sessions remain separate rows and
carry a replacement marker.

## Skill Routing Contract

`brainstorm-to-delivery` keeps a durable thread table in the SDD progress
ledger. The key is the work unit plus role and immutable profile identity
(see ledger key table above).

| Work unit | First dispatch | Subsequent work on same unit |
| --- | --- | --- |
| Design + reviewer/profile | New reviewer | Continue that reviewer for Design revisions. |
| Plan + reviewer/profile | New reviewer | Continue that reviewer for Plan revisions. |
| Task N + Grok implementer | New Grok | Continue it for questions and fixes on Task N. |
| Task N + Codex reviewer | New independent Codex | Continue it for Task N re-reviews. |
| Task N+1 | New Grok and new Codex | Never reuse Task N threads. |
| Final whole-branch review | New Codex | Continue only this exact final reviewer after a typed unexpected interruption; never continue a Task reviewer. |

Design and plan are separate work units even when they use the same reviewer
profile. Optional document reviewers remain optional; Codex remains mandatory
in the document review group. Optional document reviewers cannot become code
reviewers.

The ledger records each thread's child conversation id, latest task id, agent
type, profile id, state, work-unit recovery count, and any replacement
relationship.

`continue_delegation` runs on a child conversation without interacting with
`DelegationContinuationCoordinator` (parent-turn suspension). A continued
child run remains a valid member of an active parent continuation's task set.

## Failure And Security Rules

- Continue only the latest terminal run owned by the current direct parent.
- Reject cross-parent ids without revealing child details.
- Reject continuation when the child is busy (`busy_thread`).
- Do not allow tool input to override agent, profile, workspace, or external
  session identity.
- Persist a failed terminal run if resume or prompt admission fails after run
  reservation; never strand `reserving`/`running`.
- Platform eligibility for canceled-run recovery requires structured unexpected
  termination provenance **and** remaining unexpected-continue budget on the
  lineage (and work-unit key when supplied).
- Never auto-continue an explicit user/parent cancellation or an unknown legacy
  cancellation whose origin cannot be established.
- Cancel only the selected active run. Never delete prior cards or the child
  transcript.
- Permit a same-role/profile fresh replacement only when server-validated
  `replacement_reason` is one of: `unresumable` (missing historical session,
  unsupported resume/load, corrupt session, failed resume handshake, external-id
  mismatch, launch config unavailable), `budget_exhausted_continue` (unexpected-
  continue rail already at 2 — the documented two-continues-then-replacement
  path), or `not_supported` (capability-disabled agent). `replaces_task_id` plus
  reason are mandatory for orchestrated (`work_unit_key`) replacements and are
  verified under server checks.
- Do not fall back for authorization, stale-id, route-policy, or concurrency
  errors.
- Never substitute agent type. Required Grok or Codex unavailability remains a
  hard delivery blocker.
- Treat card summaries as bounded frontend display data only. Never execute,
  interpret as commands, or echo them into parent MCP results.
- Config snapshots use an allowlist; raw secrets never persist on runs.
- `resume_existing_only` never falls through to `session/new`.

## Validation

### Backend

- Migration and legacy backfill tests, including:
  - `task_id = delegation_call_id` for generation-1 backfill;
  - non-continuable legacy without launch snapshot;
  - duplicate call-id / null parent-tool preflight skip rules.
- Tool-list, schema, dispatch, acknowledgement, status, and cancel tests for
  `continue_delegation`.
- Ownership, stale-id, missing-id, deleted-child, busy-thread, not-continuable,
  unresumable, and duplicate-parent-tool tests.
- Same child conversation plus new task id across multiple runs.
- Concurrent double-continue → exactly one new run; loser gets `busy_thread`.
- `resume_existing_only`: `session/resume`, `session/load` fallback, no
  `session/new`, external-id mismatch → `unresumable`.
- Prompt-admission and terminal-persistence failure tests.
- Late old-connection completion cannot settle the new generation.
- Monotonic conversation projection CAS by `delegation_run_generation`.
- Restart/cold-load: non-terminal runs fail with `host_restarted` before accept.
- Store load/settle/prefix-recovery keyed by run `task_id`.
- Summary parser acceptance, multi-comment last-match, bounds, invalid-input
  fallback, immutability, and non-exposure in parent MCP reports.
- Unexpected interruption recovery eligibility, explicit-cancel suppression,
  unknown-origin suppression, and third unexpected-continue → `budget_exhausted`.
- Replacement lineage via mandatory server checks on `replaces_task_id`; second
  replacement for same lineage → `budget_exhausted`; business errors do not
  create replacements.
- Capability-gated agent types refuse continue with `not_supported`.
- Migration: duplicate non-null parent-tool keys do not abort; losers get NULL
  unique key + legacy column.
- Task preview: length bound, redaction patterns, fail-closed empty preview.

### Frontend

- Several cards can share one child conversation and retain independent run
  status and summaries.
- A running later run cannot reopen or mutate a terminal earlier card.
- Historical cards load via task_id snapshot DTO, never latest child projection.
- Review and Implementation summary rendering and invalid-summary fallback.
- Overlay grouping by child conversation with run count and latest state.
- All run cards open the same session and request the correct run focus when
  anchor present.
- Replacement sessions remain visibly separate.
- Responsive screenshot checks at desktop and mobile widths; RTL/locale smoke
  for summary layout on supported locales.

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
- A final reviewer interrupted before producing a verdict continues its own
  fresh final-review session; it never switches to a Task reviewer.
- Business errors and unavailable required agents do not trigger substitution.
- Skill budget: at most two automatic continues + one replacement per work unit.

Use conversation 832 as the interruption recovery regression fixture: its
terminal row is canceled, its external Codex session remains present, and its
turn ended with `interrupted` before `TurnComplete`. The expected recovery is a
new run and card on the same child conversation, not mutation of the canceled
run.

Conversation 835 replacement path assertions:

1. Original thread's latest run is terminal-canceled / unresumable.
2. Replacement run exists on a **different** child conversation with
   `replaced_task_id` pointing at the failed thread's run.
3. `continue_delegation` on the original (replaced) child's task id returns
   `not_continuable` because a run with `replaced_task_id` points at that
   child; the replacement child remains continuable for later rounds.

Run the affected Rust tests for desktop, server, and `codeg-mcp` surfaces, plus
frontend tests, lint, build, and targeted visual verification.

## Acceptance Criteria

- A same-unit continuation creates a new run and card but reuses the exact child
  conversation and external agent session (`resume_existing_only`, no
  `session/new`).
- Historical run cards remain immutable through later runs and process restart.
- Each run remains independently queryable and cancelable by task id.
- Conversation 800's four-by-three review pattern requires only three child
  conversations.
- Review and Implementation cards show validated structured summaries when
  supplied and degrade without inventing data when not supplied; summaries are
  not injected into parent MCP results.
- New Tasks and the final global Codex review preserve fresh-session isolation.
- Resume failures can fall back to a recorded same-role/profile replacement;
  authorization and role failures cannot.
- Unexpected interruption recovery creates a new run in the same child, never
  resumes an exact process instruction; platform and Skill each cap unexpected
  continues at two per lineage/work unit before the one-replacement path.
- Platform enforces lineage recovery rails; Skill ledger routes threads and may
  stop earlier but cannot exceed platform rails.
- Legacy backfill preserves `task_id = delegation_call_id` for generation-1
  runs; rows without reconstructible launch snapshots are non-continuable.
- The Skill retains optional document review models and the mandatory Codex
  review role.
- Prior one-shot lifecycle docs are amended per the Document Precedence table.

## Review adjudication log (2026-07-21)

Document review group: CodeBuddy Opus 4.8, CodeBuddy GLM 5.2, Codex CLI.

| Finding (source) | Severity | Resolution |
| --- | --- | --- |
| Platform vs Skill recovery budget (Opus C1, Codex C3/r2) | Critical | Platform lineage rails (≤2 unexpected continues, ≤1 replacement) + optional `work_unit_key`; Skill routes and may be stricter |
| Silent session/new fallthrough (Codex C1, Opus I1) | Critical | `resume_existing_only`; external-id verify; capability gate → `not_supported` |
| Missing immutable launch identity (Codex C2) | Critical | workspace, route fingerprint, snapshot version; legacy without snapshot non-continuable |
| Active-run race / connection retirement (Codex C4, Opus I4, GLM I2–3) | Critical | `reserving`→`running`→terminal; partial unique index; incarnation fence |
| Projection fence wording (Opus I3, GLM I1) | Important | monotonic `delegation_run_generation` |
| Store re-key to run task_id (Opus I2) | Important | enumerated touchpoints + prefix recovery |
| Backfill task_id = call id (GLM I4) | Important | mandatory |
| Startup reconcile runs (all) | Important | fail non-terminal runs with `host_restarted` |
| Summary not in parent MCP (GLM I6) | Important | frontend-only |
| Continuable set vs latest terminal (GLM I7) | Important | explicit eligibility + `not_continuable` |
| Secrets re-resolve (Opus I5, Codex I4) | Important | allowlist snapshot + live secret re-resolve |
| Cold projection DTO (Codex I1) | Important | task_id snapshot path |
| Migration collisions (Codex I2/r2, Opus M4) | Important | duplicate call-id and parent-tool preflight; empty→NULL |
| Document precedence (Codex I5) | Important | supersedes matrix |
| task_preview redaction (Codex I r2) | Important | server-derived ≤200 chars, pattern redaction, fail-closed |
| Card extraction / bounds (all Minor) | Minor | last match + concrete bounds |

