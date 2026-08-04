# Platform-Generated Completion Evidence Design

## Status and Relationship to Existing Designs

Approved in the 2026-08-04 design discussion, then revised after a
code-grounded review of the gate, Plan-round, freshness, and attention
subsystems. This version incorporates the approved review decisions.

This design is authoritative wherever completion Card handling or completion
evidence freshness conflicts with these earlier designs:

- `2026-07-27-delegation-card-redundancy-full-fix-design.md`;
- `2026-07-28-workflow-reviewer-amendment-design.md`; and
- `2026-07-30-brainstorm-to-delivery-recovery-contract-hardening-design.md`.

Their work-unit projection, recovery authorization, reviewer amendment, and
workflow safety rules remain in force. The following old requirements are
superseded:

- a child must emit `<!-- codeg-card-summary-v1 ... -->`;
- a missing or malformed Card requires a same-child continuation;
- a Plan Author's model-authored `plan_digest` is trusted as document
  identity; and
- current `manifest_revision` equality is required for otherwise unchanged
  completion evidence.

This is a deliberately incompatible completion protocol. Existing workflows
remain readable, but their completion evidence is never upgraded or reused.

## Executive Decision

A model reports only a semantic result. Codeg derives all trusted identity,
artifact, lineage, and freshness fields from durable platform state.

The minimum supported child capability is plain natural language. A child may
optionally use a small `complete_work` tool, but successful workflow completion
must not depend on JSON, HTML comments, hidden metadata, digest calculation, or
tool-call support.

Completion has four ordered input channels:

1. a valid `complete_work` call;
2. an explicit terminal conclusion line in the final assistant response;
3. an explicit top-level conclusion in a bounded report file; and
4. a direct user adjudication when the earlier channels are absent or
   semantically ambiguous.

The selected semantic intent is combined with platform-generated evidence and
persisted as `completion_evidence_json`. Workflow admission and gate settlement
read that evidence. A Card is generated from the evidence for display only.

Document gates are reduced from the platform-bound Reviewer outcomes. Finding
counts and structured finding ledgers may be shown as untrusted report content,
but they are never gate inputs in protocol v2.

Malformed Card-like text never triggers `continue_delegation`. A completed run
whose meaning cannot be resolved becomes `needs_decision`, opens durable
attention, and waits for the user without creating another child run.

## Problem

The current protocol asks models with very different formatting ability to
produce a role-specific JSON object inside an exact HTML comment. It also mixes
three different concerns in that object:

- the child's semantic judgment, such as `approve` or `done`;
- display content, such as summary, counts, tests, and report path; and
- trusted workflow evidence, such as role shape and Plan digest.

That coupling created the failure seen in `codeg://session/2889`. The Parent
repeatedly instructed a Plan Author to emit an implementation-shaped Card with
`kind: implementation`, `verdict`, and `artifact_digest`. The platform actually
required an Author-shaped Card with `kind: author`, `status`, `plan_digest`, and
`report_file`. The instructed combination does not exist in any v1 Card kind:
`verdict` belongs to the `review` wire kind, while `artifact_digest` is a run
binding column and is not a Card field at all. This was an invalid Parent
instruction, not an alternative platform schema.

The failure then amplified through this deterministic path:

```text
child completes useful work
  -> Card is missing, malformed, or has the wrong role shape
  -> card_summary parser silently returns None
  -> workflow run binding receives summary_validated = false
  -> next reviewer admission reports a generic stale/missing evidence error
  -> B2D guidance requires same-child continue to re-emit the Card
  -> every continue creates a new run and a short child turn
  -> transcript accumulates "CARD RE-EMIT ONLY" sessions
```

Those sessions are not additional review or implementation work. They are
format-repair retries caused by treating model-authored UI serialization as a
workflow precondition.

The current freshness check compounds the problem. Evidence is often bound to
a whole manifest revision even when the reviewed artifact and reviewer remain
unchanged. Removing one unavailable sibling reviewer can therefore invalidate
unrelated completed reviews and cause more work to rerun.

## Goals

- Let a child complete reliably with one explicit natural-language conclusion.
- Keep `complete_work` as an optional fast path for tool-capable models.
- Make Codeg the sole producer of task, role, workflow, gate, lineage, digest,
  and evidence-scope fields.
- Compute Design, Plan, and code artifact identity on the platform; ignore any
  model claim about those values.
- Make parsing syntactically tolerant but semantically fail closed.
- Convert missing, conflicting, or role-incompatible meaning into durable user
  attention without spawning another child run.
- Make Cards deterministic UI projections that never participate in admission
  or settlement.
- Derive Design and Plan gate decisions from platform-bound Reviewer outcomes,
  without trusting finding counts or ledgers from a child or Parent.
- Preserve valid sibling review evidence across unrelated manifest changes and
  reviewer-only amendments.
- Invalidate evidence when its artifact, producer, node identity, gate cycle,
  or actual review scope changes.
- Keep old sessions readable while requiring a new linked workflow and a full
  rerun from the Design gate when an old workflow is resumed.
- Provide typed diagnostics that identify intent, artifact, scope, or legacy
  restart failures directly.

## Non-Goals

- General free-form natural-language classification.
- Using another model to judge a child's completion prose.
- Trusting commit SHAs, counts, digests, task IDs, roles, or gate data emitted
  by a child or Parent.
- Preserving structured Plan findings as a protocol-v2 state-machine input.
- Reusing any v1 Card or settlement as v2 evidence.
- Automatically approving a user-adjudicated gate without its normal
  settlement operation.
- Replacing continuation for genuine transport, cancellation, stall, or
  infrastructure recovery.
- Changing raw transcript retention or the existing work-unit card folding
  rules.
- Redesigning standalone, non-workflow delegation in this slice.

## Design Principles

### Semantic Minimum

The child owns only its conclusion, a short summary, and an optional report
reference. Everything else is platform state.

### Separate Infrastructure From Meaning

`DelegationRunStatus::Completed` means the child turn ended successfully. It
does not mean the work passed its workflow gate. Semantic completion is a
separate resolution with either trusted evidence or `needs_decision`.

### Syntax Tolerant, Semantics Fail Closed

Whitespace, case, Markdown wrappers, and common punctuation may vary. The
platform never infers a verdict from keywords embedded in ordinary prose.
Within an ordered source, the last valid explicit statement supersedes earlier
statements and all statements remain auditable. Conflicting conclusions from
different report files have no reliable ordering and require user adjudication.

### No Format-Recovery Delegation

Completion metadata is resolved locally from existing output or by the user.
`continue_delegation` is reserved for real work or infrastructure recovery and
is never legal solely to repair completion formatting.

### Narrow Freshness

Evidence validity depends only on the exact subject and identity that the
evidence covers. A global revision remains useful for audit and concurrency,
but it is not a semantic freshness key.

## Protocol Version Boundary

Add `completion_protocol_version` to the durable workflow header.

- The server owns a creation mode: `v1`, `v2_shadow`, or `v2_enforce`.
- `v1` and `v2_shadow` create version-1 workflows; shadow mode additionally
  runs the v2 resolver for metrics only and cannot write evidence or affect a
  gate.
- `v2_enforce` creates version-2 workflows. The selected protocol is frozen at
  creation and an existing v2 workflow is never downgraded.
- Existing rows are classified as version `1`; migration does not inspect or
  transform their Cards and records their creation mode as `v1`.
- Every run bound to a workflow inherits the workflow version at admission.
- A v2 run never invokes the `codeg-card-summary-v1` parser.
- `card_summary_json` remains untouched and readable only as legacy display
  data for v1 sessions.
- `summary_validated`, model-authored `plan_digest`, and v1 Card kind checks are
  not consulted by any v2 admission or settlement path.
- `manifest_revision` and `content_fingerprint` remain v1 audit fields; neither
  is a v2 semantic eligibility key.

The version is server-owned. It is not a manifest field that a model may choose
or echo.

## Architecture

```text
complete_work --------+
terminal conclusion --+--> completion_intent resolver -- resolved --+
report conclusion ----+             |                               |
                                    +--> needs_decision --> attention
                                                            |
                                                   user adjudication
                                                            |
resolved intent <-------------------------------------------+
       |
       v
artifact_resolver --> evidence_scope --> completion_evidence_json
                                               |
                              +----------------+----------------+
                              v                                 v
                    workflow safety                 completion_projection
                    admission/gates                 UI + parent status
```

The units are intentionally separate:

| Component | Responsibility | Does not decide |
| --- | --- | --- |
| `completion_intent` | Resolve one role-compatible semantic outcome from ordered sources | Artifact identity or gate freshness |
| `completion_evidence` | Combine intent with trusted platform context and persist it atomically | How prose is rendered |
| `artifact_resolver` | Resolve canonical document or code identity from the workspace | Semantic outcome |
| `evidence_scope` | Canonicalize the exact validity inputs and hash them | Reviewer policy or UI state |
| `completion_projection` | Derive Cards and parent-facing completion state | Admission or settlement |
| `attention` | Persist and resolve terminal semantic ambiguity | Child execution or continuation |
| `workflow_restart` | Create one linked v2 successor for a resumed v1 workflow | Legacy evidence conversion |

## Semantic Outcome Contract

The canonical outcome enum is deliberately small.

| Bound role | Accepted outcomes | Passing outcomes |
| --- | --- | --- |
| Design, Plan, Task, or Final Reviewer | `approve`, `approve_with_minors`, `request_changes`, `block` | `approve`, `approve_with_minors` |
| Plan Author | `done`, `done_with_concerns`, `blocked` | `done`, `done_with_concerns` |
| Task Implementer or Final Fixer | `done`, `done_with_concerns`, `blocked` | `done`, `done_with_concerns` |

Role compatibility comes from the durable node binding. A model never submits
`kind`, `role`, or `phase`.

The workflow node role is named `reviewer`. The legacy Card wire discriminator
is `kind: review`, not `kind: reviewer`; protocol v2 requires neither value from
the model.

`approve_with_minors` and `done_with_concerns` pass but retain their concern
signal in UI and settlement audit. `request_changes`, `block`, and `blocked`
are valid terminal meanings, not parse failures.

## Completion Input Channels

### 1. `complete_work` Fast Path

Expose a child-only MCP tool with this semantic-only request:

```json
{
  "outcome": "approve",
  "summary": "The Plan is implementable; two naming nits remain.",
  "report_file": "reports/plan-review.md"
}
```

Only `outcome` is required. `summary` and `report_file` are bounded optional
display inputs. The request has no task ID, workflow ID, role, kind, phase,
digest, producer ID, gate ID, gate cycle, revision, counts, commits, or tests.

The Broker resolves the caller through its live delegated-child connection and
stores the intent against that run. The tool is exposed only when the caller is
a `DelegationChild`, is bound to a workflow run, and that workflow uses
completion protocol v2. Calls from a root, a standalone delegated child, an
unrelated child, or a terminal run are rejected. The role-specific outcome set
is checked against the durable binding.

Repeated delivery of one tool call is idempotent by tool-call ID and canonical
request digest. Across distinct valid calls for the same run, the last accepted
call supersedes earlier calls, including a changed outcome. Every accepted call
remains in the audit log. Invalid requests are rejected immediately and do not
supersede the last valid call, so a child may correct one with a later valid
call.

Calling `complete_work` records intent but does not itself terminate the child
or settle a gate. Evidence is materialized only when the run reaches its normal
terminal boundary.

### 2. Explicit Terminal Conclusion

Every workflow child prompt ends with one short role-specific instruction.

Reviewer example:

```text
Finish with one plain-language conclusion line:
Conclusion: approve | approve with minor issues | request changes | blocked
```

Author, Implementer, or Fixer example:

```text
Finish with one plain-language conclusion line:
Conclusion: done | done with concerns | blocked
```

No Card template, JSON example, digest, or hidden marker is included.

The parser examines only top-level lines in the final assistant text. It
ignores fenced code, HTML comments, block quotes, tables, and quoted examples.
After Unicode compatibility normalization, case folding, whitespace collapse,
and removal of optional Markdown heading/list/bold wrappers, an eligible line
must match the whole-line form:

```text
<label> <separator> <outcome> <optional terminal punctuation>
```

Supported labels are `Conclusion`, `Final conclusion`, `Verdict`, `结论`,
`最终结论`, and `审核结论`. Broad labels such as `Status` and `状态` are not
eligible because they commonly introduce per-item status lists. Supported
separators are `:`, `：`, and `-`.

The initial bounded outcome lexicon is:

| Canonical outcome | Accepted whole-value aliases |
| --- | --- |
| `approve` | `approve`, `approved`, `pass`, `通过`, `认可` |
| `approve_with_minors` | `approve with minors`, `approve with minor issues`, `pass with minor issues`, `有小问题通过`, `有轻微问题通过` |
| `request_changes` | `request changes`, `changes requested`, `needs changes`, `需修改`, `需要修改` |
| `block` | `block`, `blocked`, `阻塞`, `无法通过` |
| `done` | `done`, `complete`, `completed`, `完成`, `已完成` |
| `done_with_concerns` | `done with concerns`, `completed with concerns`, `有顾虑完成`, `完成但有顾虑` |
| `blocked` | `blocked`, `无法完成`, `阻塞` |

The role disambiguates `blocked`/`阻塞` into reviewer `block` or worker
`blocked`. Aliases live in one versioned parser table and the platform-injected
prompt uses only aliases from that table.

The last eligible top-level conclusion line is authoritative within the final
assistant response; earlier eligible lines remain diagnostic audit material.
If that final explicit outcome is incompatible with the durable role, the run
enters `needs_decision`. Words such as "approved" or "done" inside ordinary
prose never count.

If this channel resolves one intent, report conclusions are not interpreted as
competing claims. The resolver is a fallback chain, not a vote across sources.

### 3. Report Top-Level Conclusion

When neither of the first two channels resolves intent, Codeg examines bounded
Markdown report candidates in this order:

1. a normalized report hint already present in child output;
2. Markdown links in the final assistant response; and
3. platform-observed touched `.md` or `.markdown` files.

The existing limits remain: at most eight candidates and at most 512 KiB per
file. A candidate must resolve inside the workspace after canonicalization;
absolute paths, parent traversal, non-files, symlink escapes, and non-Markdown
files are rejected.

A Markdown parser, rather than raw substring search, identifies either:

- a top-level conclusion label line using the terminal-line grammar; or
- a level-one or level-two `Conclusion`/`Verdict`/`结论` section whose first
  plain paragraph is exactly one accepted outcome alias.

Content inside code, quotes, tables, nested lists, or examples is ignored. The
last eligible conclusion within one report is authoritative. All eligible
report candidates are then evaluated. Equivalent outcomes coalesce; different
outcomes across files produce `needs_decision` because file ordering is not
treated as authorial supersession. Report IO failure is recorded as a
diagnostic and cannot turn prose into a pass.

The report path is a convenience and audit link. Report content never supplies
task identity, role, artifact digest, producer identity, gate data, or revision
data.

### 4. Direct User Adjudication

If the selected automated channel is missing, conflicting, or
role-incompatible, terminal settlement creates a durable attention request of
kind `completion_decision`.

The task remains infrastructure-completed while its workflow completion state
is `needs_decision`. The existing attention event and card plumbing is reused,
but this new kind differs from a live child question:

- it is allowed to remain open after the task becomes terminal;
- terminal cleanup does not resolve it as `task_terminal`;
- it presents the role-specific outcome set and bounded source excerpts;
- it is resolved through an authenticated desktop/Web UI operation with a
  typed outcome, not through a free-form Parent model reply; and
- it never wakes or resumes the completed child.

Resolution uses a compare-and-set token over the attention record, latest run,
node binding, and captured evidence scope. If the run was superseded or the
scope changed, the request becomes `superseded` and cannot mint stale evidence.
Otherwise the user outcome becomes an intent with source `user_adjudication`
and enters normal evidence materialization.

The user may choose any outcome legal for the bound role, including an outcome
that was not one of the ambiguous candidates. This is adjudication, not merely
candidate selection.

## Resolver Precedence and States

The resolver processes channels strictly in order.

```text
valid complete_work candidates?
  yes                -> latest valid call resolves
  no                 -> inspect terminal conclusion

valid terminal conclusion candidates?
  yes                -> last eligible line resolves or role-mismatches
  no                 -> inspect reports

valid report conclusion candidates?
  yes, one outcome   -> resolved
  yes, cross-file conflict -> needs_decision
  no                 -> needs_decision
```

The result type is explicit:

```text
Resolved(CompletionIntent)
NeedsDecision { reason_code, bounded_candidates, diagnostics }
```

There is no `best_effort` or keyword-derived result. A missing summary does not
invalidate an otherwise explicit outcome. The projection uses, in order, a
tool summary, a bounded plain paragraph adjacent to the conclusion, a report
summary, or the normalized outcome label itself.

## Trusted Completion Evidence

`CompletionIntent` is a semantic claim. `CompletionEvidenceV2` is the trusted
workflow fact produced by Codeg.

Illustrative persisted shape:

```json
{
  "version": 2,
  "intent": {
    "outcome": "approve",
    "summary": "The Plan is implementable.",
    "report_file": "reports/plan-review.md",
    "source": "assistant_conclusion"
  },
  "binding": {
    "workflow_id": "platform-generated",
    "task_id": "platform-generated",
    "node_id": "plan-review-codex",
    "role": "reviewer",
    "phase_id": "plan",
    "task_index": null,
    "gate_id": "plan-gate",
    "gate_cycle": 1,
    "reviewed_task_id": "platform-selected-author-run",
    "reviewed_generation": 1,
    "manifest_revision_observed": 4
  },
  "artifact": {
    "kind": "document_sha256",
    "rel_path": "docs/superpowers/plans/example.md",
    "digest": "sha256:platform-computed"
  },
  "review_scope_digest": "sha256:platform-computed",
  "evidence_scope_digest": "sha256:platform-computed",
  "captured_at": "2026-08-04T00:00:00Z"
}
```

The model influences only `intent.outcome`, `intent.summary`, and the optional
report hint. The platform derives or normalizes every other field.
`manifest_revision_observed` is retained for audit only and is not compared for
semantic freshness.

The authoritative JSON is stored on the durable task run as
`completion_evidence_json`. Query-friendly outcome and scope columns are
transactional projections of that JSON, not independent facts.

### Persistence Changes

Add or extend these durable fields:

```text
delegation_workflows
  completion_protocol_version        INTEGER NOT NULL
  completion_protocol_mode           TEXT NOT NULL
  legacy_source_workflow_id          TEXT NULL
  UNIQUE(legacy_source_workflow_id) WHERE legacy_source_workflow_id IS NOT NULL

delegation_task_runs
  completion_state                   TEXT NULL
  completion_outcome                 TEXT NULL
  completion_evidence_json           TEXT NULL

delegation_workflow_run_bindings
  evidence_scope_digest              TEXT NULL

delegation_workflow_gate_settlements
  evidence_scope_digest              TEXT NULL
  required_evidence_task_ids_json    TEXT NULL
  plan_round_state_v2_json           TEXT NULL
  critical_count                     INTEGER NULL
  important_count                    INTEGER NULL
  minor_count                        INTEGER NULL

delegation_completion_tool_intents
  intent_id                          TEXT PRIMARY KEY
  task_id                            TEXT NOT NULL
  child_tool_call_id                 TEXT NOT NULL
  accepted_ordinal                   INTEGER NOT NULL
  outcome                            TEXT NOT NULL
  summary                            TEXT NULL
  report_hint                        TEXT NULL
  request_digest                     TEXT NOT NULL
  created_at                         TEXT NOT NULL
  UNIQUE(task_id, child_tool_call_id)
  UNIQUE(task_id, accepted_ordinal)

delegation_attention_requests
  kind                               TEXT NOT NULL DEFAULT 'child_question'
  child_tool_call_id                 TEXT NULL
  payload_json                       TEXT NULL
  resolution_json                    TEXT NULL
  captured_scope_digest              TEXT NULL
  UNIQUE(task_id, kind) WHERE status = 'open'
  UNIQUE(task_id, child_tool_call_id)
    WHERE child_tool_call_id IS NOT NULL
```

`delegation_completion_tool_intents` intentionally stores only tool-channel
intents, so its tool-call ID remains non-null. Text, report, and user intents
are resolved directly into evidence or attention state. Existing attention
rows become `child_question`; SQLite migration rebuilds the table to change
nullability and replaces the old open-row index rather than layering a second
incompatible constraint on top of it.

Settlement finding-count columns remain v1 audit fields and are null for v2.
They are made nullable as part of the settlement migration; null never means
zero findings and no v2 reducer reads these columns.

Persistence is split and registered in `src-tauri/src/db/migration/mod.rs` in
this order:

1. `m20260804_000001_completion_protocol_and_run_evidence`;
2. `m20260804_000002_completion_scope_and_gate_settlement`;
3. `m20260804_000003_completion_tool_intents_and_restart_link`; and
4. `m20260804_000004_typed_completion_attention`.

All v2 completion columns are written in one terminal-settlement transaction:

1. verify the current durable run and workflow binding;
2. select the latest accepted tool intent or apply the precomputed lower
   channel candidate;
3. re-resolve platform identity and artifact inputs;
4. for Design and Plan documents, re-read the bytes and recompute their digest
   inside the settlement transaction's critical section;
5. compute the evidence scope;
6. write either evidence or one open completion attention request;
7. update run-binding projections and graph revision; and
8. commit before emitting completion, attention, or workflow events.

Report reads and the first bounded Git/file resolution may occur before the
transaction. Database identity alone is not sufficient revalidation: required
Design and Plan bytes are hashed a second time in the transaction, and Git
`HEAD` is re-resolved immediately before the evidence write. A filesystem
change after that read cannot be made atomic with SQLite, but the next
admission or settlement scope recomputation detects it and invalidates the
evidence. A retry for the same terminal run is idempotent and cannot create two
evidence records or two open completion decisions.

## Artifact Resolution

The artifact resolver has role-specific platform rules.

### Design and Plan Documents

Document identity is the workspace-relative normalized path plus SHA-256 of
the exact file bytes. The platform canonicalizes the path, rejects workspace
escape, reads the file with a bounded size, and emits the canonical
`sha256:<lowercase hex>` form.

For completion protocol v2, document digests supplied in workflow publication
requests are non-authoritative. The request may omit them. If present during a
transition period, they are ignored and replaced by the resolver result before
the immutable manifest revision is stored.

This produces the required sequence for Plan:

1. the skeleton declares the server-validated Plan target path;
2. the Author writes the Plan and completes;
3. Codeg hashes that path and records Author evidence;
4. estimated-manifest publication resolves the same path again; and
5. Plan Reviewer admission compares platform-produced artifact identity.

A child-provided `plan_digest`, `artifact_digest`, or report digest is never
read.

### Code-Producing Roles

Task Implementers and Final Fixers already use the platform-read Git `HEAD` in
`workflow/admission.rs::on_terminal_settle_txn`; when `HEAD` is unavailable the
existing code leaves `workspace_head_commit` empty and does not fall back to a
Card commit. Protocol v2 reuses that behavior, stores an explicit resolver kind
such as `git_head_v1`, and makes the unavailable case typed. This is not a new
replacement for model-authored code identity; the model-authored digest defect
applies primarily to the Plan Author's `plan_digest`.

If the workspace is not a Git worktree or `HEAD` cannot be resolved, the
artifact is unavailable. A non-passing semantic outcome remains valid without
an artifact because it cannot unlock a dependency. A passing producer outcome
requires the artifact; Codeg opens `completion_artifact_recovery` attention and
blocks the dependency with `completion_artifact_unavailable`. It never asks the
child to re-emit metadata.

Dirty-worktree snapshot identity and per-file task ownership are outside this
slice. The explicit resolver-kind field allows a later `git_snapshot_v2`
without changing evidence semantics.

### Reviewers

A Reviewer does not supply or recompute the producer identity. Admission
selects the latest eligible producer run from durable workflow state and
captures its platform-resolved artifact:

- Plan Reviewer: active Plan Author run plus the current Plan document digest;
- Task Reviewer: latest eligible Implementer run and generation plus its code
  artifact digest;
- Final Reviewer: current branch-tip/fixer artifact selected by Final admission;
- Design Reviewer: current Design document digest, with no child producer.

The same captured producer and artifact must still be current when evidence is
materialized.

## Evidence Scope

`evidence_scope_digest` replaces whole-manifest revision equality for v2
completion validity. It is SHA-256 over canonical JSON with sorted keys,
explicit nulls, versioned field names, and normalized path strings.

For Design and Plan phases, the canonical scope includes the complete material
input set currently represented by the workflow header's
`design_fingerprint` or `plan_fingerprint`, respectively. The implementation
may reuse a domain-separated digest of those canonical inputs, but it must not
silently reuse or reinterpret the legacy `content_fingerprint` column. Tests
must prove that every material input which changes the current fingerprint also
changes the v2 scope digest.

The canonical input contains only:

- completion protocol and scope-schema versions;
- workflow ID, preventing cross-workflow reuse;
- stable node identity: node ID, role, phase, task index, agent, profile, and
  canonical work-unit identity;
- gate ID and gate cycle when the node belongs to a gate;
- platform-resolved artifact kind, path where applicable, and digest;
- reviewed producer task ID and generation where applicable; and
- a role-specific `review_scope_digest`.

`review_scope_digest` captures material instructions beyond the artifact:

| Role | Review-scope material |
| --- | --- |
| Design Reviewer | workflow kind, approved requirements identity, Design target path, and effective Design-review policy |
| Plan Author | Plan target path and current Design identity |
| Plan Reviewer | Design identity, Plan target, risk-policy version, effective review policy, task policies, task routes, and material Plan inputs |
| Task Implementer | task index, task specification/dependencies, and admitted Plan identity |
| Task Reviewer | implementer task specification, risk classification, review requirements, and admitted Plan identity |
| Final Fixer | active Final findings identity and current branch-tip input |
| Final Reviewer | active Plan identity, active task-output identities, and current Final review requirements |

The following values are deliberately excluded:

- active or observed `manifest_revision`;
- `graph_revision`;
- reviewer cohort and required-reviewer sets;
- sibling reviewer node identities or outcomes;
- roster-only revision counters whose effective policy content is unchanged;
- display titles, summaries, counts, and report paths; and
- unrelated future task or UI metadata.

Eligibility always also requires that the node is currently active and
required where applicable. Therefore removing a reviewer makes that review
ineligible without changing sibling evidence. Replacing a reviewer creates a
new node identity and requires new evidence only from the replacement.

### Freshness Consequences

| Change | Existing evidence |
| --- | --- |
| Remove an unavailable sibling reviewer | Preserved for unchanged required siblings |
| Replace an unavailable sibling reviewer | Preserved for siblings; replacement starts empty |
| Change only a title or graph layout | Preserved |
| Publish an unrelated manifest revision | Preserved |
| Change reviewed document or code artifact | Invalidated |
| Select a new producer run or generation | Invalidated |
| Change node role, task index, agent/profile, or work-unit identity | Invalidated |
| Change material task/risk/review scope | Invalidated |
| Open a new gate cycle | Invalidated |
| Attempt reuse in another workflow | Invalidated |

Gate settlement stores the exact required-node set, evidence task IDs, and
scope digests used for the decision so historical adjudication remains
reproducible.

Legacy run-binding and settlement `content_fingerprint` values retain their
existing v1 meaning and remain available for historical display. A v2
settlement writes and validates its independent `evidence_scope_digest`;
`project.rs` display filtering and `admission.rs` Plan-gate reopening branch by
protocol rather than overloading the old column. Evidence used for a new round
must also be newer than the prior settlement it replaces.

## Workflow Safety Rules

Infrastructure success and semantic pass are checked independently.

For a v2 dependency or gate to pass, the latest eligible run must:

1. be durably `completed`;
2. have `completion_state = resolved`;
3. contain a valid `CompletionEvidenceV2` for its durable role;
4. have a current `evidence_scope_digest`; and
5. carry a passing outcome from the role matrix.

Consequences:

- A completed Reviewer with `request_changes` or `block` has valid evidence
  but cannot approve a gate.
- A completed Author/Implementer/Fixer with `blocked` has valid evidence but
  cannot unlock its dependent Reviewer.
- A completed run in `needs_decision` never counts as pass or non-pass
  adjudication until the user resolves it.
- A failed or canceled run has no completion evidence; existing typed recovery
  and reviewer-amendment policy applies.
- `approve_with_minors` and `done_with_concerns` pass without erasing their
  concern status.

For v2, `settle_workflow_gate` derives the current evidence set from the store.
The caller may state an expected settlement outcome and summary, but it does
not submit Reviewer task IDs, counts, findings, covered Author task/digest,
required Reviewer IDs, gate cycle, or manifest binding as evidence. The
platform derives those values from the active workflow and rejects an expected
outcome that disagrees with the reducer. Existing revision CAS may remain for
concurrency, but a caller-echoed revision is never proof of freshness.

The v2 request surface therefore contains only workflow/gate identity,
`expected_graph_revision`, an optional expected outcome, a bounded display
summary, and an existing recovery-authorization ID where that operation
requires one. It has no `SettleGateEvidence`, finding counts/updates, covered
task/digest, required-node set, or model-produced scope fields.

An `approved` settlement succeeds only when every current required Reviewer
has passing evidence for the current scope. A `changes_requested` or `blocked`
settlement must be the canonical result of current non-passing evidence or the
existing explicit self-review/user-adjudication rules.

All v2 consumers call the same bounded evidence validator. In particular:

- `workflow/admission.rs` uses it for producer and Reviewer admission;
- `workflow/store.rs` uses it for document settlement and Plan rounds;
- `workflow/gates.rs` uses it instead of `summary_validated`, Card
  `work_status`, and Card `review_verdict` for Task/Final pass;
- `workflow/project.rs` uses it for node state, missing-evidence display,
  settlement filtering, and gate reopening; and
- `workflow/recovery_policy.rs` uses it for recovery eligibility.

`broker.rs` materializes and projects the evidence but is not an independent
gate authority. A protocol-v2 code path in any of these modules must not infer
semantic state from `card_summary_json` or `summary_validated`.

## Outcome-Based Document Gate Reduction

### Design Gate

The platform reads the latest fresh evidence for every required Design
Reviewer. All `approve`/`approve_with_minors` outcomes derive `Approved`; any
`request_changes` derives `ChangesRequested`; otherwise a current `block`
derives `Blocked`. Legacy Critical/Important/Minor count fields are null for v2
and `ApprovalWithOpenFindings` is a v1-only rule.

### Plan Review Rounds

Protocol v2 replaces `PlanReviewRoundSubmission.finding_updates` with a
platform-derived `PlanReviewRoundStateV2`. For each current Reviewer it stores
the Reviewer node ID, canonical outcome, evidence task ID, and evidence scope
digest. The platform also derives the covered Author task/generation, Plan
artifact digest, and required Reviewer set; the Parent cannot submit or amend
them.

Review scope, revision kind, and requirements-lineage reset come from the
existing trusted workflow transition and recovery-authorization state. They
are not inferred from completion prose and are not evidence fields supplied by
the Parent settlement request.

Reviewer outcomes have these ranks:

| Outcome | Rank | Effect |
| --- | ---: | --- |
| `approve`, `approve_with_minors` | 0 | Pass; no routine re-review required |
| `request_changes` | 1 | Blocking for this round |
| `block` | 2 | Stronger blocking result |

A round approves only when every currently required Reviewer is rank 0. The
next localized corrective round requires exactly the current non-passing
Reviewer IDs. Initial, materially revised, and holistic-rewrite Plans require
the full current cohort. A roster-only amendment evaluates only newly added
Reviewers, preserves fresh sibling evidence, and does not increment
stagnation. Removing a Reviewer removes it from the required set without
rewriting sibling state.

For two completed corrective rounds in one requirements lineage, the current
round is a strict improvement only when all three conditions hold:

1. every current non-passing Reviewer was also non-passing previously;
2. no surviving Reviewer's rank increased; and
3. at least one prior blocker became passing or decreased rank.

An initial/lineage-reset round establishes the baseline. Strict improvement
resets `stagnation_count`; any completed non-improving corrective round
increments it. Infrastructure failures and roster-only amendments do not
advance it. Two stagnant rounds derive `HolisticRewriteRequired`. The holistic
rewrite is allowed once per lineage and resets the counter; two further
stagnant rounds derive `UserDecisionRequired`. The corresponding settlement is
`Approved` for an all-pass state, `ChangesRequested` for continued review or a
required rewrite, and `Blocked` while awaiting the required user decision.

Reviewer reports may still display findings, severity labels, and counts, but
the platform neither merges them into a trusted ledger nor reads them for
eligibility, improvement, next action, or settlement. Existing v1
`finding_ledger_json` and count columns remain historical audit data;
`plan_round_state_v2_json` is the only v2 Plan reducer state.

## Completion Projection

Cards become pure projections over durable v2 evidence and attention state.
They are never parsed back into workflow logic.

The projection exposes a bounded DTO similar to:

```text
CompletionCardV2
  state: resolved | needs_decision | blocked
  role: platform-derived display role
  outcome: optional semantic outcome
  summary: bounded display text
  report_file: optional validated workspace-relative path
  source: complete_work | assistant_conclusion | report | user_adjudication
  platform_verified: boolean
  attention: optional typed completion-decision summary
```

Counts, commits, and tests may continue to appear from runtime telemetry or
report projection, but they are not completion evidence. The UI must not show
model-authored IDs or digests as verified facts.

Parent-facing `delegate_to_agent`, `continue_delegation`, and status results add
a platform-generated completion projection:

```json
{
  "completion": {
    "protocol_version": 2,
    "state": "resolved",
    "outcome": "approve",
    "source": "assistant_conclusion"
  }
}
```

When state is `needs_decision`, the result carries the typed attention summary
and instructs the Parent to stop and wait for user resolution. It never asks
the Parent to parse or repair a Card.

## Attention and Continuation Policy

The B2D Skill and its validator are updated to enforce these rules:

- never emit or request `codeg-card-summary-v1` for a v2 workflow;
- never use the phrase or operation `CARD RE-EMIT ONLY`;
- never call `continue_delegation` because completion intent, summary, report
  path, or display projection is missing or malformed;
- advance only from platform completion state and workflow admission results;
- on `needs_decision`, surface the durable attention and wait; and
- retain normal continuation/replacement policy for genuine incomplete work,
  transport loss, stall, cancellation, or other infrastructure causes.

This separates two superficially similar cases:

```text
child did not finish the work       -> normal recovery may continue or replace
child finished but output is unclear -> terminal attention; never continue
```

The validator contains negative fixtures for Card templates, digest requests,
and format-only continuation guidance so the old loop cannot re-enter through
prompt drift.

### Durable Attention Lifecycle

Attention lookup and resolution APIs take both `task_id` and `kind`; a generic
`resolve_task(task_id, ...)` is not allowed to select an arbitrary open row.
`child_question`, `completion_decision`, and
`completion_artifact_recovery` have separate payload validators and resolution
operations. Only authenticated typed UI adjudication can resolve a
`completion_decision`; a Parent free-form reply remains valid only for
`child_question`.

An open `completion_decision` may close only because:

- the user commits a role-valid outcome through its CAS operation;
- the captured run, node, or evidence scope is superseded;
- the user explicitly terminates/abandons the workflow, or a competing CAS
  completes it through another valid path; or
- the workflow or owning conversation is explicitly deleted.

The recoverable workflow state `Blocked` used for
`UserDecisionRequired` is not such a terminal command and does not close the
decision it is waiting for.

Task terminal cleanup, host restart, Parent turn failure/cancellation, Parent
disconnect, and join teardown or `join_abandoned` are not workflow-terminal
events and must not close it. Broker tree teardown therefore filters by
attention kind before applying `parent_canceled`, `parent_disconnected`, or
`join_abandoned` cleanup.

Startup `reconcile_open()` retains its current recovery behavior only for
`child_question`. For `completion_decision`, it reloads the workflow, latest
run, node binding, and captured scope: a current request remains open; a stale
request is resolved as `superseded`, and the latest unresolved run may open one
replacement through the normal idempotent path. Application restart alone
never resolves it as `host_restarted` or `task_terminal`.

`completion_artifact_recovery` follows the same restart and Parent-teardown
durability. It resolves only when a typed retry materializes current evidence,
its captured scope is superseded, or the workflow is explicitly terminated or
deleted.

An active workflow's completion decision has no automatic timeout. Cleanup is
limited to the explicit terminal/deletion cases above. Operations expose the
open count and oldest unresolved age so abandoned decisions are observable
without silently discarding them.

## Legacy Workflow Restart

No v1 evidence is converted, backfilled, or reused.

Migration marks existing workflows as `completion_protocol_version = 1` and
leaves all transcripts, runs, Cards, reports, manifests, and settlements
unchanged. They remain available for read-only inspection.

An in-place forward upgrade is intentionally rejected. A settled v1 Design or
Plan gate has no v2 evidence scope, no platform-derived Reviewer outcome state,
and may have been approved from Card/count semantics that v2 explicitly does
not trust. Letting only new runs use v2 while retaining those settlements would
create a workflow whose gates are proven by incompatible rules. Reconstructing
them would be evidence conversion, which is outside the trust boundary.

Under the normal `v2_enforce` mode, when a user attempts to resume or send a
new turn to a v1 workflow session, Codeg performs an idempotent restart
operation:

1. keep the source conversation and workflow read-only;
2. find an existing restart successor and open it, if one already exists;
3. otherwise create one new root conversation linked to the source;
4. copy only the original user request, workspace/folder selection, and launch
   configuration needed to restate the job;
5. create a new workflow header with completion protocol version 2;
6. start at an open Design gate with no imported run, evidence, settlement, or
   gate-cycle state; and
7. rerun Design review, Plan authoring/review, Tasks, and Final review through
   new task IDs.

Existing Design or Plan files may be read as workspace inputs, but their
digests are recomputed and every gate runs again. Existing implementation may
lead a new Implementer to verify or make no changes, but the run and its
evidence are new.

Persist a unique source-to-successor restart record so repeated clicks, process
restarts, or transport retries cannot create multiple successor sessions. The
new session displays a backlink to the legacy source; the old session displays
the successor link and the reason it is read-only.

If restart creation fails, the old session remains unchanged and the user gets
a typed retryable error. Codeg never falls back to mutating or upgrading the
legacy workflow.

The normal post-rollout mode is `v2_enforce`, so a legacy resume follows this
restart path by default. The explicit operational `v1` and `v2_shadow` modes
are temporary rollout exceptions: workflows created or resumed while those
modes are selected use the existing v1 authority. Once the server returns to
`v2_enforce`, any later resume of such a workflow follows the same full-restart
rule; no v1 evidence is imported.

## Rollout and Rollback

`completion_protocol_mode` is selected from server configuration, may be
overridden by an agent/profile rollout allowlist, is recorded on the workflow
header, and has three values:

| Mode | New workflow authority | v2 side effects |
| --- | --- | --- |
| `v1` | Existing v1 completion path | None |
| `v2_shadow` | Existing v1 completion path | Parser/reducer metrics only |
| `v2_enforce` | v2 evidence and gates | Full |

Changing the setting affects only workflows created afterward. Existing v2
workflows continue with v2 even if the operator rolls future creation back to
v1; no workflow changes protocol in place. Shadow computation uses bounded
copies of the same terminal inputs but cannot persist v2 evidence, create v2
attention, change a gate, or trigger continuation.

Rollout expands by agent/profile only after at least 100 terminal samples for
that profile. Expansion stops when `completion_outcome_role_mismatch` exceeds
1% or the total `needs_decision` rate exceeds 5% in the evaluation window. The
operator may then return future creation to `v2_shadow` or `v1` while active v2
workflows remain intact. Semantic strictness is not configurable: no rollout
mode may turn an ambiguous result into a pass.

## Error Contract

New stable reason codes distinguish the recovery path:

| Code | Meaning | Workflow response |
| --- | --- | --- |
| `completion_intent_missing` | No eligible tool, terminal, or report conclusion | Open completion decision |
| `completion_intent_conflict` | Different report files contain incompatible top-level outcomes | Open completion decision |
| `completion_outcome_role_mismatch` | Explicit outcome is illegal for the durable role | Open completion decision |
| `completion_report_unavailable` | A report candidate could not be safely read | Inspect remaining local candidates; if none resolve, open completion decision |
| `completion_artifact_unavailable` | Platform cannot resolve identity required by a passing producer outcome | Open artifact-recovery attention and block the dependency; never format retry |
| `completion_scope_changed` | Identity/artifact/scope changed before evidence commit or user resolution | Supersede decision and require current work evaluation |
| `completion_decision_superseded` | An open decision no longer covers the latest run/node/scope | Close stale attention and re-evaluate the current run |
| `completion_evidence_corrupt` | Persisted v2 evidence fails bounded schema validation | Fail closed and surface repair diagnostic |
| `completion_decision_required` | Terminal run awaits direct user adjudication | Stop Parent orchestration |
| `legacy_completion_protocol_restart_required` | A v1 workflow received a mutation/resume attempt under `v2_enforce` | Create/open linked v2 successor |

Parser diagnostics must say which channel and semantic reason failed without
logging full child output, report contents, user text, absolute paths, or
profile configuration.

## Concurrency and Idempotency

- `complete_work` uses `(task_id, child_tool_call_id)` as its idempotency key
  and stores a canonical request digest. Distinct valid calls receive a durable
  per-task ordinal; the highest ordinal is authoritative and earlier calls
  remain auditable.
- Terminal evidence materialization is unique per task. An identical retry
  returns the existing evidence or attention state.
- A terminal writer and user-decision writer compare the latest run, node,
  scope, and graph state; at most one current evidence record commits.
- Completion attention has at most one open row per task and kind.
- User-decision retries with the same typed outcome are idempotent; a different
  outcome after resolution is a conflict.
- Reviewer amendment and user decision race through the same workflow
  transaction/CAS boundary. Removal or replacement can supersede attention but
  cannot transfer evidence to another node.
- Startup reconciliation and Broker teardown apply kind-specific closure
  rules; generic task/Parent cleanup cannot win a CAS against a current
  completion decision.
- Events are emitted only after commit and carry the committed graph revision.

## Security and Bounds

- Tool caller identity comes from the Broker connection, never request fields;
  `complete_work` additionally requires a workflow-bound v2
  `DelegationChild` run.
- Completion text, summary, and report hints retain existing payload limits;
  summaries are bounded to 4 KiB in evidence and more tightly in Card display.
- Report candidates remain limited to eight files and 512 KiB each.
- Document hashing uses workspace-contained canonical paths and bounded files.
- Symlink escape, absolute paths, parent traversal, alternate URI schemes, and
  non-files are rejected.
- Canonical JSON hashing uses a fixed schema version and domain separator so a
  digest from one purpose cannot be reused as another.
- User adjudication requires authenticated ownership of the root conversation.
- Parent agents cannot resolve `completion_decision` through free-form reply.
- Logs contain IDs, enum codes, sizes, and digest prefixes only; no raw report
  or completion prose.

## Expected Implementation Boundary

Backend modules:

- add focused `completion_intent`, `completion_evidence`,
  `artifact_resolver`, `evidence_scope`, `completion_projection`, and
  `workflow_restart` modules under `src-tauri/src/acp/delegation/` or its
  workflow package;
- replace the v2 branch of `prepare_terminal_with_card_summary` in
  `broker.rs` with completion resolution/materialization;
- update `workflow/admission.rs`, `workflow/store.rs`, `workflow/gates.rs`,
  `workflow/project.rs`, and `workflow/recovery_policy.rs` to consume the same
  v2 evidence validator and scoped freshness;
- replace the v2 branch of `workflow/plan_review.rs` with the outcome-based
  round reducer while retaining its v1 ledger decoder for historical rows;
- add the child-only `complete_work` operation to companion schema/dispatch;
- extend typed attention with kind-specific query, resolution, startup
  reconciliation, and Broker teardown behavior;
- add SeaORM entities/migrations for protocol version, intents, evidence,
  settlement scope/Plan state, attention kind, and restart linkage using the
  four ordered `m20260804_...` migrations above; and
- expose completion projection consistently through Tauri, Axum, WebSocket,
  ACP history, and `codeg-mcp` status results.

Frontend and Skill surfaces:

- mirror `CompletionCardV2` and typed completion attention in TypeScript;
- render resolved, needs-decision, blocked, and legacy-restart states;
- add a direct role-bounded adjudication control with desktop/server parity;
- expose the server-owned creation mode and shadow/enforce telemetry to the
  existing operational configuration surface;
- update all locale files for new states and actions;
- replace v1 Card instructions in
  `.agents/skills/brainstorm-to-delivery/SKILL.md`; and
- update the Skill validator and behavior fixtures to reject format-repair
  continuation guidance.

Unrelated recovery authorization, work-unit grouping, transcript persistence,
and workflow overlay layout are outside this implementation boundary.

## Test Strategy

### Intent Parser

- Accept each canonical Reviewer and worker outcome through `complete_work`.
- Reject tool outcomes incompatible with the durable role.
- Coalesce delivery retries by tool-call ID and make the latest distinct valid
  call authoritative, including outcome changes.
- Accept casing, spacing, punctuation, Markdown wrapper, English, and Chinese
  terminal-line variants.
- Reject `Status` and `状态` as conclusion labels.
- Ignore outcome words in prose, code fences, HTML comments, quotes, tables,
  and examples.
- Make the last eligible terminal line authoritative and retain earlier lines
  only for diagnostics.
- Verify strict source precedence: a resolved higher channel prevents lower
  report interpretation.
- Parse only top-level report conclusions through a Markdown AST.
- Make the last conclusion in one report authoritative and route incompatible
  conclusions across report files to `needs_decision`.
- Enforce candidate count, file size, workspace containment, and symlink rules.

### Evidence and Artifact Resolution

- Ignore false child/Parent task IDs, roles, kinds, revisions, and digests.
- Compute Plan and Design digests from exact file bytes.
- Change a Design/Plan file between the pre-read and settlement write and prove
  the in-transaction second hash prevents stale evidence from committing.
- Compute code artifact identity from platform Git `HEAD`; never use a
  model-authored commit list.
- Reuse existing `workspace_head_commit` behavior and persist a typed resolver
  kind/unavailable state.
- Persist the selected producer task and generation from admission state.
- Atomically write terminal status, evidence/binding projection, graph
  revision, and events.
- Keep a resolved semantic intent non-passing when required artifact identity
  is unavailable.

### Scope Freshness

- Preserve completed sibling reviews after removing an unavailable Reviewer.
- Preserve siblings and require only the new node after replacement.
- Preserve evidence across title-only, graph-only, unrelated-task, and
  roster/revision-counter-only manifest changes whose effective review policy
  is unchanged.
- Invalidate on document/code digest, producer task/generation, node identity,
  material review scope, gate cycle, or workflow change.
- Prove required-reviewer sets and `manifest_revision` are absent from scope
  canonicalization.
- Prove every material input represented by legacy Design/Plan fingerprints is
  represented by the v2 scope, while `content_fingerprint` keeps its v1
  storage meaning.
- Reject a digest built with another domain/schema version.

### Gate Safety

- Permit Reviewer pass only for `approve` and `approve_with_minors`.
- Permit Author/Implementer/Fixer pass only for `done` and
  `done_with_concerns`.
- Treat `request_changes`, `block`, and `blocked` as valid non-pass evidence.
- Never count infrastructure completion alone.
- Never count unresolved attention as terminal gate evidence.
- Derive gate evidence from the store rather than caller-submitted IDs or
  digests.
- Exercise `admission.rs`, `store.rs`, `gates.rs`, `project.rs`, and
  `recovery_policy.rs` with v2 evidence and prove none requires
  `summary_validated` or a Card status/verdict.
- Derive Design approval/changes/blocked solely from required Reviewer
  outcomes; counts remain null/non-authoritative.
- Keep failed/canceled recovery and reviewer-amendment behavior intact.

### Plan Outcome Reducer

- Approve only when all current required Reviewers are rank 0.
- Re-review only current non-pass Reviewers after a localized correction.
- Require the full cohort for initial, material, and holistic revisions.
- Preserve siblings and avoid stagnation advancement for roster-only
  add/remove amendments.
- Treat blocker-set shrink or rank decrease as improvement only when no new
  blocker appears and no surviving blocker worsens.
- Trigger holistic rewrite after two stagnant rounds and user decision after
  two post-rewrite stagnant rounds.
- Prove findings, severity counts, Parent-supplied coverage, and v1
  `finding_ledger_json` cannot change a v2 next action.

### Attention and No-Re-Emit Behavior

- A completed child with no explicit conclusion opens one completion decision.
- Incompatible report files or a role-incompatible authoritative conclusion
  open one decision with bounded candidate excerpts.
- Terminal cleanup leaves completion decisions open while still closing live
  child-question attention.
- Host restart, Parent turn failure/cancellation/disconnect, and
  `join_abandoned` leave current completion decisions open.
- Startup reconcile retains current decisions, supersedes stale scopes, and
  opens at most one current replacement.
- Active completion decisions do not expire; explicit workflow termination or
  deletion closes them deterministically.
- Artifact-recovery attention also survives restart and Parent teardown, then
  resolves on a successful typed retry, scope supersession, explicit workflow
  termination, or deletion.
- A direct user choice creates evidence without waking, continuing, or
  replacing the child.
- Concurrent amendment, superseding run, or scope change prevents stale user
  resolution.
- No malformed JSON, HTML comment, missing Card, or report-only issue invokes
  `continue_delegation`.
- A session-2889 fixture produces zero `CARD RE-EMIT ONLY` runs.

### Legacy Restart

- Migration labels all existing workflows v1 without changing their data.
- v2 logic never reads legacy `card_summary_json` as evidence.
- Under `v2_enforce`, a v1 resume creates exactly one linked v2 root and opens
  its Design gate.
- Repeated resume requests return the same successor.
- No old run, evidence, settlement, gate cycle, or task ID appears in the new
  workflow.
- Old and new sessions expose reciprocal links and the old session stays
  read-only.
- An old settled gate cannot be carried into a workflow containing v2 runs.

### Rollout Modes

- `v1` and `v2_shadow` keep v1 authoritative; shadow writes metrics only.
- `v2_enforce` writes v2 evidence and never calls the v1 Card parser.
- A mode change affects new workflows only and cannot downgrade an active v2
  workflow.
- Rollout thresholds stop expansion at greater than 1% role mismatch or 5%
  `needs_decision` after the 100-sample minimum.

### End-to-End Capability Matrix

- A tool-capable model completes through `complete_work`.
- A model that can only emit plain text completes through one conclusion line.
- A model that writes its conclusion only in a report completes through report
  fallback.
- A model that emits useful work but no clear conclusion reaches user
  adjudication without another child session.
- A model that emits an obsolete or wrong-shaped v1 Card plus a valid natural
  conclusion succeeds through the natural-language channel; the Card is
  ignored.
- Desktop and server modes produce identical evidence, attention, projection,
  and restart behavior.

### Repository Verification

Implementation will require targeted unit/integration tests plus the repository
checks in `AGENTS.md`:

```powershell
pnpm eslint .
pnpm test
pnpm build

Set-Location src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

## Observability

Add bounded counters and structured events for:

- completion resolution by source and role;
- accepted and superseded `complete_work` calls;
- missing, conflicting, and role-mismatched intent;
- completion decisions opened, resolved, and superseded;
- open completion decisions and oldest unresolved age;
- artifact-resolution failure by resolver kind;
- evidence invalidation by changed scope dimension;
- Plan reducer outcome, strict improvement, stagnation, rewrite, and next
  action;
- v1-to-v2 restart creation/reuse/failure;
- protocol creation mode and shadow/enforce result differences; and
- continuation reason, with a dedicated invariant counter proving that
  format-only completion repair remains zero.

The primary rollout indicators are:

- percentage resolved by natural-language fallback;
- completion-decision rate by agent/profile;
- median time from terminal run to user adjudication;
- oldest unresolved completion-decision age;
- role-mismatch rate per agent/profile after the 100-sample minimum;
- number of new child runs whose prompt contains `CARD RE-EMIT ONLY`; and
- number of sibling reviews rerun after reviewer-only amendment.

Rollout expansion stops above the approved 1% role-mismatch or 5%
completion-decision thresholds. The last two metrics must remain zero under
protocol v2.

## Acceptance Criteria

1. A workflow child can complete using only one explicit natural-language
   conclusion and no JSON, HTML comment, digest, or tool call.
2. Tool-capable children may use `complete_work`, whose request contains only
   semantic outcome, summary, and optional report path.
3. Codeg derives role, phase, task, workflow, gate, cycle, producer, revision
   provenance, artifact identity, and evidence scope from durable state.
4. Model-authored Plan/code digests and task/role identities are ignored.
5. v2 admission and settlement use `completion_evidence_json`, never
   `card_summary_json` or `summary_validated`.
6. Cards are deterministic projections and cannot affect workflow validity.
7. Missing, conflicting, or role-incompatible meaning opens one durable user
   decision and creates no continuation or replacement run.
8. Reviewer pass is limited to `approve`/`approve_with_minors`; producer pass
   is limited to `done`/`done_with_concerns`.
9. Design and Plan gates are derived from platform-bound Reviewer outcomes;
   child/Parent findings and counts cannot approve, block, or advance a round.
10. Plan corrective rounds use the approved strict-improvement relation,
    selective re-review, two-round rewrite threshold, and post-rewrite user
    decision threshold.
11. Removing or replacing an unavailable Reviewer preserves unchanged sibling
   evidence, while artifact, producer, node, review-scope, gate-cycle, and
   workflow changes invalidate it.
12. `evidence_scope_digest` replaces both `manifest_revision` and
    `content_fingerprint` for v2 eligibility without changing either legacy
    field's stored meaning.
13. A current `completion_decision` survives task terminal cleanup, process
    restart, Parent teardown, disconnect, and join abandonment; only its typed
    lifecycle closure rules may resolve it.
14. Session-2889-style wrong Card templates cannot block Plan Reviewer
    admission when a valid semantic conclusion and platform-resolved Plan
    artifact exist.
15. New v2 sessions never contain format-only `CARD RE-EMIT ONLY` child runs.
16. Existing v1 sessions remain readable but immutable under normal
    `v2_enforce`; resume creates or reopens exactly one linked v2 session and
    reruns the workflow from Design through Final with no reused evidence.
17. Rollout mode is frozen per workflow; shadow has no semantic side effects,
    and rollback affects future creation without downgrading active v2 work.
18. `admission.rs`, `store.rs`, `gates.rs`, `project.rs`, and
    `recovery_policy.rs` all consume the same v2 validator.
19. Desktop, server, and `codeg-mcp` paths expose the same completion truth and
    pass the required verification suites.
