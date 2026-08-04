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

### Cycle-1 Review Resolutions

The user selected the reviewer-recommended defaults for every Critical and
Important finding. Cheap, unambiguous Minor findings are also closed here.

| Closed findings | Adopted default |
| --- | --- |
| `D-CODEX-C1` | Keep `git_head_v1`, but require both a resolvable platform `HEAD` and a completely clean tracked/untracked worktree for a passing producer outcome; otherwise open artifact recovery. |
| `D-CODEX-C2`, `D-GROK-M1` | Separate stable material `gate_lineage` from platform-assigned `review_round`; an expected round is concurrency CAS only. |
| `D-CODEX-C3`, `D-GROK-M2` | Empty Design `self_review` requires an authenticated typed user decision bound to the current Design scope and is never vacuous approval. |
| `D-GROK-C1` | Treat legacy fingerprints as v1 audit/superset keys and define an independent v2 material input set whose scope survives non-material fingerprint changes. |
| `D-CODEX-I1` | Append a server-owned, versioned canonical instruction block to every admitted protocol-v2 workflow child and bind its digest into scope. |
| `D-CODEX-I2` | Canonically source and persist every scope identity, including requirements, task specification, and Final findings identities, with golden JSON vectors. |
| `D-CODEX-I3` | Bind the child connection before MCP injection and expose `complete_work` only through a workflow-v2 token feature plus Broker authorization. |
| `D-CODEX-I4` | Fence continue/replace before authorization or budget use while a current completion decision or artifact recovery is open. |
| `D-CODEX-I5` | Rebuild settlement and attention tables transactionally, preserving the complete v1 schema and data before adding v2 fields. |
| `D-GROK-I1`, `D-GROK-I2` | Use transaction-only, kind-specific terminal attention open APIs and explicit per-kind payload, resolution, CAS, and caller contracts. |
| `D-GROK-I3`, `D-GROK-I4`, `D-GROK-I5` | Emit durable adjudication-resolved events, require complete resolved evidence before reduction, and branch recovery onto v2 evidence rather than Cards/counts. |
| `D-CODEX-M1`, `D-CODEX-M2`, `D-GROK-M3` | Finish parser edge rules, rename the projection flag to `evidence_validated`, and leave `card_summary_json` null and unused on new v2 runs. |

### Cycle-2 Residual Resolutions

The residual Codex findings use the cycle-2 brief's normative defaults. These
defaults refine the corresponding Cycle-1 rows and are authoritative for
implementation.

| Closed finding | Adopted default |
| --- | --- |
| `D-CODEX-C1` | Retain `git_head_v1`, require a clean resolvable admission baseline, require workflow-owned producer commits before pass except an explicitly authorized no-op, and revalidate `HEAD` plus cleanliness at code-Reviewer admission and terminal materialization. |
| `D-CODEX-I2` | Define bounded `PlanMaterialSchemaV1` parsing, normalization, material keys/selectors, identities, fail-closed lineage behavior, construction order, and parser fixtures. |
| `D-CODEX-I3` | Generate stable `child_tool_call_id` before Broker dispatch from MCP tool-use identity or connection-incarnation plus JSON-RPC request identity; accepted ordinals are audit/display only. |
| `D-CODEX-I6` | Bind ordered, immutable remediation-context snapshots into `FinalFindingsPackageV1`, Final Fixer identity, and the server-owned Fixer instruction block; absent material context opens typed user decision. |

### Cycle-3 Residual Resolution

| Closed finding | Adopted default |
| --- | --- |
| `D-CODEX-C1` (post-approval Final drift) | History tidy/aggregation that changes `HEAD` must finish before Final Reviewer admission. Passing Final freezes the delivered commit id. Post-settlement `HEAD` drift blocks delivery with `final_artifact_drift` and reopens Final review. Skill finalization cannot commit after Final pass. |

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
- Invalidate evidence when its artifact, producer, node identity, material gate
  lineage, or actual review scope changes; require a new round only from
  Reviewers selected for that localized corrective round.
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
- Existing `card_summary_json` values remain untouched and readable only as
  legacy display data for v1 sessions. New v2 runs leave the column null,
  never parse or validate it, and project display only from
  `CompletionCardV2`.
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

### Server-Owned Prompt Binding

Protocol-v2 workflow child admission constructs and appends a canonical
instruction block owned by the server. Parent-authored `task` prose is
supplemental context and is never evidence that the child received the policy
represented by its scope. The block contains:

- `template_id` and `template_version`;
- the role-compatible conclusion suffix drawn from the versioned parser table;
- a bounded node-binding summary: role, phase, task index, gate, and selected
  review round when applicable; and
- a bounded material-scope summary containing platform identities rather than
  child- or Parent-authored digests.

The canonical UTF-8 bytes and their domain-separated SHA-256 digest are
persisted with the run binding. That instruction-block digest is an input to
`review_scope_digest` and therefore to `evidence_scope_digest`. Admission
fails closed if the platform cannot build, persist, or append the exact block.
No caller may replace or shadow it with free-form prose.

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
digest, producer ID, gate ID, gate lineage, review round, revision, counts,
commits, or tests.

Admission binds the child connection to its durable run before companion MCP
configuration is injected. Only a workflow-bound protocol-v2
`DelegationChild` token receives the `completion_v2` feature bit. Root,
standalone, v1, and not-yet-bound connections never receive that bit. The
companion catalog includes `complete_work` only when the bit is present, and
the Broker independently rechecks the live connection, run, workflow version,
terminal state, and role before accepting the call. Catalog filtering is not
authorization. The role-specific outcome set is checked against the durable
binding.

Before the first Broker dispatch, the companion generates
`child_tool_call_id`. It prefers MCP `_meta.tool_use_id`; when that field is
absent it uses
`rpc:{connection_incarnation_id}:{json_rpc_request_id}`, derived from the MCP
connection incarnation and inbound JSON-RPC request identity. The companion
sends that ID with the Broker request, and every transport redelivery of the
same request must carry the same value.

The Broker makes delivery idempotent by `(task_id, child_tool_call_id)` plus
the canonical request digest. The same ID with the same digest returns the
prior acceptance; the same ID with a different digest is a conflict and is
rejected. Each distinct valid ID is a new accepted call and receives a durable
per-task `accepted_ordinal`; the highest ordinal supersedes earlier calls,
including a changed outcome. `platform:{task_id}:{accepted_ordinal}` is a
display/audit label only and is never a transport redelivery key. Every
accepted call remains in the audit log. Invalid requests do not supersede the
last valid call, so a child may correct one with a later valid call.

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
separators are `:`, `：`, and `-`. The optional terminal punctuation is exactly
one of `.`, `!`, `。`, or `！`. Wrapper removal is single-pass and enumerated:

- an optional column-zero heading prefix of `# ` through `###### `, or one
  column-zero list prefix of `- `, `* `, `+ `, or one to three ASCII digits
  followed by `. ` or `) `;
- then an optional whole-line bold pair `**...**` or `__...__`.

A line cannot have both heading and list prefixes; wrappers cannot be nested or
removed recursively. An unindented list item is top-level after its one allowed
prefix is removed. Any indented/nested list item, partial bold span, unmatched
wrapper, or other punctuation remains ineligible.

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

Report hints on an already resolved intent are audit/display metadata only. A
selected `complete_work` intent uses its validated `report_file`; otherwise the
text channel uses the first workspace-relative `.md` path in a plain,
top-level line immediately before or after its authoritative conclusion. Hint
extraction never scans code, quotes, tables, or nested lists and never causes a
lower report outcome to compete.

The versioned grammar ships normative accepted/rejected fixtures used by the
server prompt generator, terminal parser, and report parser. The fixtures cover
every wrapper token, punctuation mark, adjacency rule, and top-level rejection.

### 3. Report Top-Level Conclusion

When neither of the first two channels resolves intent, Codeg examines bounded
Markdown report candidates in this order:

1. Markdown links in the final assistant response; and
2. platform-observed touched `.md` or `.markdown` files.

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
    "gate_lineage": "sha256:platform-minted-lineage",
    "review_round": 2,
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
  gate_lineage                       TEXT NULL
  review_round                       INTEGER NULL
  instruction_block_digest           TEXT NULL
  material_selector_digest           TEXT NULL
  subject_material_digest            TEXT NULL
  requirements_identity              TEXT NULL
  task_specification_identity         TEXT NULL
  final_findings_identity             TEXT NULL
  producer_baseline_head             TEXT NULL

delegation_workflow_gate_states
  workflow_id                        TEXT NOT NULL
  gate_id                            TEXT NOT NULL
  gate_lineage                       TEXT NOT NULL
  current_review_round               INTEGER NOT NULL
  selected_node_ids_json             TEXT NOT NULL
  PRIMARY KEY(workflow_id, gate_id)

delegation_workflow_design_root_bindings
  workflow_id                        TEXT NOT NULL
  gate_id                            TEXT NOT NULL
  gate_lineage                       TEXT NOT NULL
  node_id                            TEXT NOT NULL
  task_id                            TEXT NOT NULL UNIQUE
  latest_run_id                      TEXT NOT NULL UNIQUE
  design_identity                    TEXT NOT NULL
  evidence_scope_digest              TEXT NOT NULL
  graph_revision                     INTEGER NOT NULL
  PRIMARY KEY(workflow_id, gate_id, gate_lineage)

delegation_workflow_gate_settlements
  evidence_scope_digest              TEXT NULL
  gate_lineage                       TEXT NULL
  review_round                       INTEGER NULL
  required_node_set_json             TEXT NULL
  required_evidence_task_ids_json    TEXT NULL
  evidence_scope_digests_json        TEXT NULL
  localized_change_digest            TEXT NULL
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

delegation_final_findings_packages
  package_id                         TEXT PRIMARY KEY
  workflow_id                        TEXT NOT NULL
  gate_id                            TEXT NOT NULL
  gate_lineage                       TEXT NOT NULL
  source_evaluation_key              TEXT NOT NULL
  source_evidence_task_ids_json      TEXT NOT NULL
  items_json                         TEXT NOT NULL
  remediation_contexts_json          TEXT NOT NULL
  package_digest                     TEXT NOT NULL
  status                             TEXT NOT NULL CHECK(status IN
    ('active', 'superseded', 'resolved'))
  created_graph_revision             INTEGER NOT NULL
  resolved_graph_revision            INTEGER NULL
  UNIQUE(workflow_id, gate_id, gate_lineage) WHERE status = 'active'

delegation_attention_requests
  kind                               TEXT NOT NULL DEFAULT 'child_question'
  child_conversation_id              INTEGER NULL
  child_tool_call_id                 TEXT NULL
  latest_run_id                      TEXT NULL
  node_id                            TEXT NULL
  payload_json                       TEXT NULL
  resolution_json                    TEXT NULL
  captured_scope_digest              TEXT NULL
  CHECK(kind != 'child_question' OR
    (child_conversation_id IS NOT NULL AND child_tool_call_id IS NOT NULL))
  CHECK(kind != 'design_self_review_decision' OR
    child_conversation_id IS NULL)
  UNIQUE(task_id, kind) WHERE status = 'open'
  UNIQUE(task_id, child_tool_call_id)
    WHERE child_tool_call_id IS NOT NULL

delegation_workflow_outbox_events
  event_id                           TEXT PRIMARY KEY
  workflow_id                        TEXT NOT NULL
  graph_revision                     INTEGER NOT NULL
  event_kind                         TEXT NOT NULL
  subject_key                        TEXT NOT NULL
  payload_json                       TEXT NOT NULL
  dispatch_attempts                  INTEGER NOT NULL DEFAULT 0
  created_at                         TEXT NOT NULL
  delivered_at                       TEXT NULL
  UNIQUE(workflow_id, graph_revision, event_kind, subject_key)
```

`delegation_completion_tool_intents` intentionally stores only tool-channel
intents, so its tool-call ID remains non-null. Text, report, and user intents
are resolved directly into evidence or attention state. Existing attention
rows become `child_question`.

Settlement finding-count columns remain v1 audit fields and are null for v2.
They are made nullable as part of the settlement migration; null never means
zero findings and no v2 reducer reads these columns.

Persistence is split and registered in `src-tauri/src/db/migration/mod.rs` in
this order:

1. `m20260804_000001_completion_protocol_and_run_evidence`;
2. `m20260804_000002_completion_scope_and_gate_settlement`;
3. `m20260804_000003_completion_tool_intents_and_restart_link`; and
4. `m20260804_000004_typed_completion_attention`.

Migration `m20260804_000002_completion_scope_and_gate_settlement` creates the
gate-state, Design-root-binding, and Final-findings-package tables, adds the
run-binding scope columns, and must rebuild
`delegation_workflow_gate_settlements`; SQLite cannot remove the existing
finding-count `NOT NULL` constraints in place. In one transaction it creates
the replacement table with every historical column, check, foreign key, and
index plus the new nullable counts and v2 columns; copies every v1 row
byte-for-byte; swaps tables; recreates every index; runs
`PRAGMA foreign_key_check`; and commits only if every step succeeds. Any copy,
schema, index, or FK validation failure rolls back the entire migration. Null
counts mean “not represented by this protocol,” never zero findings. Upgrade
tests cover every supported historical settlement schema.

Migration `m20260804_000004_typed_completion_attention` follows the same
explicit rebuild checklist: preserve every historical column, check, foreign
key, index, and v1 row; set existing rows to `child_question`; change
`child_conversation_id` and `child_tool_call_id` nullability; add the
kind-specific checks, typed fields, and partial unique indexes; validate
foreign keys; and roll back atomically on failure. It must remove the
incompatible old one-open-row constraint rather than layering a new index over
it. The migration also creates the workflow outbox table.

All v2 completion columns are written in one terminal-settlement transaction:

1. verify the current durable run and workflow binding;
2. select the latest accepted tool intent or apply the precomputed lower
   channel candidate;
3. re-resolve platform identity and artifact inputs;
4. for Design and Plan documents, re-read the bytes and recompute their digest
   inside the settlement transaction's critical section;
5. compute the evidence scope;
6. write either evidence or one open completion attention request through the
   kind-specific terminal API;
7. update run-binding projections and graph revision; and
8. commit before emitting completion, attention, or workflow events.

The terminal attention open API is callable only inside terminal settlement or
artifact-recovery transactions. Unlike live `child_question.open_or_recover`,
it authorizes a completed workflow-bound run and its node binding rather than
requiring `status = running`, and completion kinds may have a null
`child_tool_call_id`. The status transition, evidence-or-attention write, and
graph revision occur in the same SQLite transaction; the live-question open
path is never reused.

Report reads and the first bounded Git/file resolution may occur before the
transaction. Database identity alone is not sufficient revalidation: required
Design and Plan bytes are hashed a second time in the transaction, and Git
`HEAD` plus complete worktree cleanliness are re-resolved immediately before
the evidence write. A filesystem change after that read cannot be made atomic
with SQLite, but the next admission or settlement scope recomputation detects
it and invalidates the evidence. A retry for the same terminal run is
idempotent and cannot create two evidence records or two open completion
decisions.

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

This slice retains resolver kind `git_head_v1`; it does not identify
uncommitted worktree bytes. Task Implementer and Final Fixer admission
therefore has a producer commit contract. Before producer work starts, the
platform must resolve `git rev-parse HEAD`, observe `git status --porcelain`
as exactly empty, and persist that commit as `producer_baseline_head` on the
run binding. Missing `HEAD`, a failed status command, or any tracked, staged,
or untracked entry fails admission with `completion_artifact_unavailable` and
a workspace-not-ready diagnostic. The producer is not dispatched on an
unclean or unresolvable baseline.

A passing producer terminal requires all of the following at terminal
materialization:

1. `git rev-parse HEAD` succeeds;
2. `git status --porcelain` is exactly empty; and
3. current `HEAD` differs from `producer_baseline_head`, or current `HEAD`
   equals the baseline and the producing Task's durable policy explicitly sets
   `allow_noop_verification = true`.

`allow_noop_verification` defaults to false. It is only for a Task whose
declared semantic outcome is verification that legitimately requires no code
change; it is not inferred from the child's prose. The default Implementer and
Final Fixer path for work that modifies code requires a new committed `HEAD`.
A model-authored commit list or Card value is never a fallback.

Completion-protocol-v2 B2D and Skill workflows must require Implementers and
Final Fixers to create workflow-owned commits for their Task changes before
concluding with a passing outcome. Mid-task producer commits are the identity
for Task Reviewers; they are not deferred until Final.

**Final delivery HEAD freeze (closes residual D-CODEX-C1):**

1. Any whole-branch history aggregation, squash, rebase, or tidy commit that
   changes `HEAD` must complete **before Final Reviewer admission**. The Final
   Reviewer binds and approves the **delivered** `HEAD`, not a pre-tidy tip.
2. After a **passing** Final settlement for the current Final `gate_lineage`,
   the platform forbids additional commits on the workflow delivery branch as
   part of workflow completion: post-settlement `HEAD` drift (different commit
   id than Final evidence artifact) atomically blocks delivery/report success,
   marks the delivery state `final_artifact_drift`, and reopens Final review
   under a new `gate_lineage` (or new Final review_round with full required
   Final Reviewer set) until the new tip is re-reviewed.
3. Parent/Skill finalization therefore orders as: finish history tidy → admit
   Final Reviewer → pass Final → deliver/report the **same** `HEAD` as Final
   evidence. There is no post-approval commit step that may rewrite delivery.
4. Tree-preserving history rewrites still change `git_head_v1` identity and are
   treated as drift; equality is by commit id, not tree oid alone.

There is no allowlist for unrelated dirty user work in this resolver slice: any
porcelain output at producer admission or pass materialization makes the
artifact unavailable. Such user work must be isolated outside the workflow
worktree before admission; the workflow never stashes, commits, overwrites, or
discards it.

If terminal `HEAD` is unavailable, the status command fails, the worktree is
dirty, or the required commit/no-op condition is not satisfied, the artifact
is unavailable. A non-passing semantic outcome remains valid without an
artifact because it cannot unlock a dependency. A passing producer outcome
instead opens `completion_artifact_recovery` attention and blocks the
dependency with `completion_artifact_unavailable`; it never asks the child to
re-emit metadata. A typed retry reruns both commands and the baseline rule and
materializes evidence only after the same producer scope satisfies the full
producer commit contract.

The terminal transaction persists this kind's versioned payload so recovery
does not depend on replaying child output:

```text
ArtifactRecoveryPayloadV1
  normalized_intent { outcome, summary, report_hint, source }
  source_audit_ref
  producer_scope_digest
  producer_baseline_head
  expected_resolver_kind
  producer_task_id
  producer_generation
```

`normalized_intent` is copied for every channel, including terminal text and
report fallback; a tool source additionally references its durable intent row.
`producer_scope_digest` is the canonical evidence input excluding only the
currently unavailable artifact and includes the immutable admitted
`producer_baseline_head`. For `completion_artifact_recovery`, the CAS field
`captured_scope_digest` stores that producer-scope digest. Artifact availability
or identity is therefore the expected recovery input, not a CAS mismatch; any
unrelated node, producer, instruction, policy, requirements, Final-package,
lineage, or task-scope change resolves the row as `superseded`.

Typed retry runs in one transaction: validate the full attention CAS and
producer scope, reload the persisted intent, resolve the artifact, compute the
final evidence scope, write evidence/binding projections, resolve attention as
`artifact_resolved`, bump graph revision, and enqueue events. A retry cannot
change the semantic outcome and never wakes the child.

`git_snapshot_v2`, a bounded identity over actual worktree bytes, is the
planned next resolver kind and remains outside this slice. Per-file task
ownership is also out of scope. The explicit resolver kind prevents later
snapshot evidence from being confused with `git_head_v1`.

### Reviewers

A Reviewer does not supply or recompute the producer identity. Admission
selects the latest eligible producer run from durable workflow state and
captures its platform-resolved artifact:

- Plan Reviewer: active Plan Author run plus the current Plan document digest;
- Task Reviewer: latest eligible Implementer run and generation plus the
  producer's platform-captured `git_head_v1` `HEAD`;
- Final Reviewer: current branch-tip/fixer artifact selected by Final admission;
- Design Reviewer: current Design document digest, with no child producer.

The same captured producer and artifact must still be current when evidence is
materialized. Task Reviewer and Final Reviewer admission re-run
`git rev-parse HEAD` and `git status --porcelain`, require empty porcelain, and
require current `HEAD` to equal the selected producer evidence artifact digest.
They repeat those checks at terminal evidence materialization. A new or
different commit is `completion_scope_changed`; a dirty worktree, missing
`HEAD`, or command failure is `completion_artifact_unavailable`. Either result
blocks Reviewer admission or evidence materialization and invalidates the
prior producer evidence for pass. A prior clean check is not durable proof of
later worktree bytes. Reviewers still do not hash dirty bytes;
`git_snapshot_v2` remains future work.

After passing Final settlement, delivery and Parent finalization re-read
`HEAD` and require exact equality with the Final evidence artifact before
emitting success or publishing delivery artifacts. Inequality yields
`final_artifact_drift` and reopens Final review; it never silently ships a
newer commit.

## Evidence Scope

`evidence_scope_digest` replaces whole-manifest revision equality for v2
completion validity. It is SHA-256 over canonical JSON with sorted keys,
explicit nulls, versioned field names, and normalized path strings.

Legacy `design_fingerprint`, `plan_fingerprint`, and `content_fingerprint` are
v1 audit/superset keys. They include fields such as roster, titles, layout, or
revision counters that do not change what one v2 node reviewed, so they may
change without invalidating v2 evidence. They are never reused, reinterpreted,
or required to equal a v2 scope digest. The v2 material input set below is the
only semantic freshness contract.

Every role scope starts with these common fields:

- completion protocol and scope-schema versions;
- workflow ID, preventing cross-workflow reuse;
- stable node identity: node ID, role, phase, task index, agent, profile, and
  canonical work-unit identity;
- gate ID and material `gate_lineage` when the node belongs to a gate;
- `review_round` only for a Reviewer selected for that round;
- platform-resolved artifact kind and role-scoped subject identity, while the
  evidence record separately retains the full resolved artifact for audit;
- reviewed producer task ID and generation where applicable; and
- the server-owned instruction-block digest and a role-specific
  `review_scope_digest`.

Canonical field sources are normative:

| Scope field | Durable source | Normalization |
| --- | --- | --- |
| Protocol/scope schema | `delegation_workflows.completion_protocol_version` plus server scope-schema constant | JSON integers; unsupported versions reject validation |
| Workflow identity | `delegation_workflows.workflow_id` | Exact stored UTF-8 identifier |
| Node identity | `delegation_workflow_node_bindings.{node_id,role,phase_id,task_index,agent_type,profile_id,work_unit_key}` | Enum wire values; explicit nulls; strings NFC-normalized; task index as integer |
| Gate identity | `delegation_workflow_run_bindings.gate_id` plus `delegation_workflow_gate_states.gate_lineage` | Exact gate ID plus lowercase `sha256:` lineage token; absent values are explicit nulls |
| Review round | `delegation_workflow_gate_states.{current_review_round,selected_node_ids_json}` copied to `delegation_workflow_run_bindings.review_round` at admission | Positive integer on a Reviewer run admitted in that round's selected set; explicit null for producers. A sibling unselected by a later round retains its stored earlier round rather than being re-canonicalized |
| Instruction binding | `delegation_workflow_run_bindings.instruction_block_digest` | SHA-256 of the exact server-owned canonical UTF-8 block with its template ID/version domain separator |
| Artifact/subject identity | Full platform `artifact_resolver` output in `delegation_task_runs.completion_evidence_json` and `delegation_workflow_run_bindings.artifact_digest`; role-scoped selector/material digests in the binding | Producers, Design Reviewers, and code Reviewers use the full artifact identity. Plan Reviewers use the Plan path, lineage, selector digest, and selected-material digest defined below; full Plan digest remains audit data |
| Producer identity | `delegation_workflow_run_bindings.{reviewed_task_id,reviewed_implementer_generation}` selected at admission | Exact task ID plus non-negative generation integer; explicit null where no producer exists |
| Role-specific material | fields/artifacts in the table below, with missing identities copied to the named `delegation_workflow_run_bindings` columns | Versioned canonical JSON, sorted keys, explicit nulls, UTF-8 NFC strings, slash-normalized paths |

`review_scope_digest` captures material instructions beyond the artifact. Its
role-specific fields and sources are:

References to active manifest `document_json` below mean field selection from
that immutable row, never hashing the raw manifest object. The canonicalizer
includes document path/digest, policy enum/version/value fields, subject task
index/specification, dependency `from`/`to` pairs, and routes that affect the
subject. It excludes display titles, edge IDs/layout metadata, reviewer cohort
and required sets, sibling-only nodes/routes, publication/revision fields, and
all other values listed in the exclusion table below.

| Role | Material fields | Durable source and normalization |
| --- | --- | --- |
| Design Root (`self_review`) | workflow kind, current Design identity, effective self-review policy, and gate lineage | `delegation_workflow_design_root_bindings` created by the gate-readiness transaction from workflow header, active manifest, gate state, and platform Design resolver |
| Design Reviewer | workflow kind, current Design identity, effective Design-review policy | `delegation_workflows.workflow_kind`; active `delegation_workflow_manifest_revisions.document_json.design` path re-resolved to exact bytes; normalized Design gate/node policy from the same immutable manifest row |
| Plan Author | Plan target and `requirements_identity` | Active manifest `document_json.plan_target_rel_path`; `delegation_workflow_run_bindings.requirements_identity` captured at Author admission |
| Plan Reviewer | `requirements_identity`, reviewer-specific Plan subject identity, risk-policy version, effective review policy, task policies/routes, and material Plan inputs | Binding requirements/selector/material digests; active manifest `document_json.{plan,risk_policy_version,gates,task_policies,nodes,edges}`; full Plan path/digest retained for audit |
| Task Implementer | task index, `task_specification_identity`, dependency identities, routes, and admitted Plan identity | Node binding task index; binding task-spec identity; active manifest `document_json.{nodes,edges,task_policies}`; latest Approved Plan settlement scope/artifact |
| Task Reviewer | `task_specification_identity`, risk classification, review requirements, admitted Plan identity, and selected Implementer identity | Reviewer binding task-spec/producer fields; active manifest task policy/node route; latest Approved Plan settlement scope/artifact |
| Final Fixer | `final_findings_identity` and current branch-tip input | Binding Final-findings identity computed from the platform-owned active Final package; admitted `git_head_v1` artifact identity |
| Final Reviewer | active Plan identity, ordered active task-output identities, and current Final-review requirements | Latest Approved Plan settlement; active Task/Final run bindings ordered by task index then node ID; active manifest Final nodes/edges/policies |

The missing cross-phase identities are persisted on the admitted run binding:

- `requirements_identity` is the domain-separated hash of canonical JSON
  containing the normalized Design path, platform Design digest, and the
  approved Design settlement scope when one exists. It is captured at Plan
  Author admission and reused by downstream Plan/Task bindings; it is never a
  child summary or transcript position.
- `final_findings_identity` is the domain-separated hash of the platform-owned
  active `FinalFindingsPackageV1` defined below. Finding semantics and
  severity/count summaries remain untrusted, while the exact selected
  remediation-context bytes are identity inputs.
- `task_specification_identity` is the SHA-256 of the
  `PlanMaterialSchemaV1` canonical JSON identity defined below.

### `PlanMaterialSchemaV1`

The platform parses the bounded Plan with a CommonMark-compatible Markdown AST
from the same parser-library family used by the report conclusion parser. The
requirement is shared CommonMark AST behavior, not a particular crate. AST
nodes select source spans; identity bytes come from the corresponding literal
source text rather than an implementation-specific AST rendering.

Task sections follow this grammar:

- a Task heading is an ATX heading at level 2 or 3 whose plain heading text
  matches `^Task\s+(\d+)\b`; `Task` is case-sensitive and the index contains
  ASCII digits only;
- text after the captured index is an optional title;
- for an index that occurs once, its first matching heading supplies the
  section; a later `Task N` heading is a duplicate and makes the entire parse
  fail rather than replacing or merging the first body; and
- the section body starts after the heading line and ends immediately before
  the next same-or-higher-level Task heading, or at EOF. The Task heading line
  itself is excluded. Lower-level headings and non-Task headings within that
  span remain part of the body.

The parser produces a unique key-to-body map. Every task index referenced by
active `task_policies` must have exactly one `task.N` body. A duplicate Task
heading, a referenced missing Task, or any other ambiguous parse prevents
material lineage construction and blocks Plan Author pass with the typed
`completion_plan_material_invalid` diagnostic. Republishing after such a
failure mints a new Plan `gate_lineage`; no partial map or previous body is
reused.

The map always contains these shared keys in addition to `task.N` keys:

- `plan.front_matter`: YAML front matter selected by the parser's compatible
  front-matter extension, or the normalized empty body when absent;
- `plan.global_preamble`: content before the first Task heading, or the
  normalized empty body when there is none;
- `plan.global_constraints`: the first section under a heading whose plain
  text matches `/^Global Constraints\b/i`, starting after that heading line
  and ending at the next heading of the same or higher level, or the normalized
  empty body when absent; and
- `plan.policies_fingerprint`: the platform hash of canonical active
  `task_policies` plus routes from the immutable manifest revision.

Fenced code, tables, and lists are included as literal selected source text.
Every text body removes a UTF-8 BOM, converts CRLF/CR to LF, normalizes Unicode
to NFC, strips trailing whitespace from each line, and ends in exactly one LF.
The empty body therefore normalizes to one LF. Hashes use those normalized
UTF-8 bytes.

For Task index `N`, let `body_sha256` be the lowercase `sha256:<hex>` digest of
the normalized `task.N` body. `task_specification_identity` is the lowercase
`sha256:<hex>` digest of canonical JSON:

```json
{
  "schema": "PlanMaterialSchemaV1",
  "task_index": 1,
  "body_sha256": "sha256:<lowercase hex>"
}
```

Plan Reviewer material selectors use only the key universe
`{task.N, plan.global_*, plan.front_matter, plan.policies_fingerprint}`;
`plan.global_*` expands to `plan.global_preamble` and
`plan.global_constraints`. The platform derives the Task indices from durable
review policy/routes and always includes the shared keys required by that
policy. A selector or parsed-map key-set difference, or a normalized body-hash
change, is material. When a Plan Author republishes an estimated Plan with any
such changed key, the platform mints a new Plan `gate_lineage`. Later
post-review corrective edits may retain a lineage only through the separately
authorized `PlanLocalizedChangeV2` rules below.

The hard bounds are a 2 MiB Plan file, at most 100 unique Task sections, and at
most 512 KiB of normalized bytes for any one section. Exceeding a bound fails
closed with `completion_plan_material_invalid`; truncation is forbidden.
Required golden fixtures map complete parser inputs to normalized key/body
hashes, selectors, task identities, and expected failures.

Construction order is normative and acyclic: parse
`PlanMaterialSchemaV1`, compute material keys/selectors and identities, build
the server-owned instruction block containing the selected key list and
digests, then compute `review_scope_digest` and `evidence_scope_digest`. No
instruction or scope digest is an input to Plan parsing or material identity.

Plan Reviewer scope uses `PlanSubjectIdentityV2`, not unconditional equality
to the full Plan-file digest:

```text
PlanSubjectIdentityV2
  plan_rel_path
  gate_lineage
  material_selector_digest
  selected_material_digest
```

The server derives the immutable material selector from the Reviewer's durable
review policy/routes before it builds the canonical instruction block. A
selector uses the `PlanMaterialSchemaV1` keys above; `all` expands to every
parsed key and intersects every Plan change. The Plan parser hashes only the
selected material into `selected_material_digest`. The full exact-byte Plan
digest remains in the evidence artifact and settlement for audit and producer
identity, but it is not the freshness key for an unselected Plan Reviewer
whose selected material is proved unchanged.

Every localized Plan transition also persists a `PlanLocalizedChangeV2`
artifact and its digest. The platform parses the prior/current Plan through
`PlanMaterialSchemaV1`, compares key sets and normalized body hashes, and
records the changed-key set, prior/current full Plan digests, classifier
version, and authorization. A same-lineage localized round is legal only when
parsing is complete and every changed key is covered by the selected Reviewer
set.
Unparseable edits, shared/global material without complete selector coverage,
risk/policy/route changes, or any ambiguous key are material changes and mint a
new lineage for the full cohort.

`FinalFindingsPackageV1` gives Final Fixer scope a canonical durable origin.
After a complete current Final Reviewer set reduces to non-pass, the Final gate
evaluation transaction creates the package from platform evidence only:

```text
FinalFindingsPackageV1
  workflow_id
  gate_id
  gate_lineage
  source_evaluation_key
  items[] {
    finding_id
    reviewer_node_id
    evidence_task_id
    evidence_scope_digest
    outcome
    target_work_unit_keys[]
    remediation_route_ids[]
  }
  remediation_contexts[] {
    source_evidence_task_id
    source_kind                 // report_file | terminal_snapshot
    rel_path?                   // validated workspace-relative report path
    content_sha256
    byte_len
    availability                // available | missing
  }
```

`source_evaluation_key` is the canonical hash of the active Final requirements,
ordered required Reviewer evidence task/scope/outcome tuples, and graph
revision consumed by the evaluator. The platform assigns `finding_id` from the
lineage, Reviewer node, and evidence task; target work units/routes come from
durable manifest routing. Neither a child nor Parent can create/update items,
IDs, outcomes, targets, or status. Finding prose remains untrusted for gate
reduction, but the bytes supplied as remediation context are bound.

At package mint, the platform builds `remediation_contexts` in required Final
Reviewer order, then source-discovery order. A `report_file` path is accepted
only after workspace-relative normalization, containment/symlink validation,
and the existing bounded report read. A `terminal_snapshot` is the exact
bounded terminal-output bytes selected for that evidence task. Each available
entry records the digest and length of the exact bytes used. A missing source
records `availability = missing`, `byte_len = 0`, and the SHA-256 of empty
bytes; it has no snapshot content and cannot satisfy the material-context
requirement.

Available context bytes are stored immutably at package mint in
`remediation_contexts_json` as base64 alongside their ordered metadata. The
package never re-reads a mutable report path for Fixer authority. Final Fixer
admission and terminal settlement decode the stored snapshot, enforce the
bound, and verify its stored length and digest. A mismatch is
`completion_evidence_corrupt`. This immutable-snapshot default removes the
report-path TOCTOU window while retaining `rel_path` as provenance.

`final_findings_identity` and `package_digest` include the ordered context
metadata and every ordered content digest in addition to canonical items.
Canonical items sort by `finding_id`; target and route arrays sort and
deduplicate before hashing, while remediation contexts retain their defined
order. The server-owned Final Fixer instruction block lists each source task,
kind, validated path when present, content digest, and byte length and supplies
the exact stored available bytes as bounded remediation context. Its digest is
then computed normally and included in Fixer scope.

At least one available, non-empty remediation context must be captured for a
non-pass Final evaluation before a package is minted or a Fixer can be
admitted. If none is available, package mint aborts and the evaluator opens a
typed user decision with
`completion_remediation_context_required` instead of synthesizing findings or
dispatching a Fixer.

There is at most one active package per Final gate lineage. A newer complete
non-pass Final evaluation atomically supersedes the prior package and creates a
new one; a new lineage also supersedes it. A subsequent passing Final
evaluation or explicit workflow terminal/delete marks it resolved. Final Fixer
admission requires the current active package and copies its `package_digest`
to `delegation_workflow_run_bindings.final_findings_identity`. This lifecycle,
including its immutable remediation snapshots rather than interpreted child
findings prose, is the authorized producer of that identity.

The canonicalizer ships golden canonical-JSON byte vectors and expected
lowercase SHA-256 outputs for every role. Admission, terminal materialization,
settlement, projection, and recovery validators must pass the same vectors;
they may not implement local field selection or normalization variants.

The following values are deliberately excluded:

- active or observed `manifest_revision`;
- `graph_revision`;
- reviewer cohort and required-reviewer sets;
- sibling reviewer node identities or outcomes;
- roster-only revision counters whose effective policy content is unchanged;
- display titles, summaries, counts, and report paths not selected as
  `FinalFindingsPackageV1` remediation-context provenance; and
- unrelated future task or UI metadata.

Eligibility always also requires that the node is currently active and
required where applicable. Therefore removing a reviewer makes that review
ineligible without changing sibling evidence. Replacing a reviewer creates a
new node identity and requires new evidence only from the replacement.

`gate_lineage` and `review_round` are different identities:

- `gate_lineage` is a server-minted material-lineage token. It remains stable
  across localized corrective and roster-only re-review rounds.
- `review_round` is a platform-assigned positive ordinal for each document-gate
  settlement attempt or corrective round. A caller may provide
  `expected_review_round` (or the legacy request name `expected_gate_cycle`)
  only as concurrency CAS; the platform assigns the actual round and it is not
  caller evidence.
- A material Plan revision or holistic rewrite mints a new Plan-gate lineage.
  A Design document change mints a new Design-gate lineage and, through the
  changed `requirements_identity`, a new Plan-gate lineage. A requirements
  reset does the same for every affected gate. Each new lineage invalidates
  every prior Reviewer evidence item for that gate.
- A localized corrective round retains the lineage, increments the round, and
  selects every current non-passing Reviewer plus any passing Reviewer whose
  material selector intersects `PlanLocalizedChangeV2`. It requires new
  evidence only from those selected nodes. Unselected required siblings keep
  prior evidence when their subject-material, producer, node, instruction,
  policy, and all other scope dimensions remain current.
- A roster-only addition selects only the new node in a new round under the
  current lineage; removal only changes eligibility. Neither action rewrites
  sibling scope or advances stagnation.

If a legacy storage column named `gate_cycle` must be retained during the
migration, it maps only to `review_round`. It must never store or derive
`gate_lineage`, and one counter must not serve both meanings.

### Freshness Consequences

| Change | Existing evidence |
| --- | --- |
| Remove an unavailable sibling reviewer | Preserved for unchanged required siblings |
| Replace an unavailable sibling reviewer | Preserved for siblings; replacement starts empty |
| Change only a title or graph layout | Preserved |
| Publish an unrelated manifest revision | Preserved |
| Change Design, producer document, code artifact, or a Reviewer's selected Plan material | Invalidated |
| Localized Plan edit outside an unselected Reviewer's material selector | Preserved under the same lineage with a valid localized-change artifact |
| Select a new producer run or generation | Invalidated |
| Change node role, task index, agent/profile, or work-unit identity | Invalidated |
| Change material task/risk/review scope | Invalidated |
| Open a localized corrective/roster round under the same lineage | Preserved for unselected current siblings; selected nodes require that round's evidence |
| Material revision, holistic rewrite, Design change, or requirements reset | New lineage; all prior Reviewer evidence for that gate invalidated |
| Change only a legacy fingerprint because roster/title/layout/counter changed | Preserved when every v2 material dimension is unchanged |
| Attempt reuse in another workflow | Invalidated |

Gate settlement stores `{gate_lineage, review_round, required_node_set,
evidence task IDs, evidence_scope digests}`, the current full Plan artifact,
and any `localized_change_digest` so historical adjudication remains
reproducible. Readiness requires evidence from the current round only for its
selected nodes; unselected required nodes may contribute older evidence from
the same lineage only when the localized-change artifact proves their selected
material and every other scope dimension remain current.

Legacy run-binding and settlement `content_fingerprint` values retain their
existing v1 meaning and remain available for historical display. A v2
settlement writes and validates its independent `evidence_scope_digest`;
`project.rs` display filtering and `admission.rs` Plan-gate reopening branch by
protocol rather than overloading the old column. There is no rule that every
legacy-fingerprint change invalidates v2 evidence, and there is no blanket
“new round invalidates all evidence” timestamp rule.

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
required Reviewer IDs, gate lineage/round, or manifest binding as evidence.
The platform derives those values from the active workflow and rejects an
expected outcome that disagrees with the reducer. `expected_graph_revision`
and optional `expected_review_round`/legacy `expected_gate_cycle` remain
concurrency CAS only; a caller-echoed revision or round is never proof of
freshness and cannot select the actual platform-assigned round.

The v2 request surface therefore contains only workflow/gate identity,
`expected_graph_revision`, an optional expected-review-round CAS, an optional
expected outcome, a bounded display summary, and an existing
recovery-authorization ID where that operation requires one. It has no
`SettleGateEvidence`, finding counts/updates, covered task/digest,
required-node set, or model-produced scope fields.

Before any outcome reduction, settlement readiness requires every currently
required Reviewer to have resolved, valid, fresh evidence under the current
lineage and selected-round rules. A required node in `needs_decision`, artifact
recovery, or any other unresolved state makes the gate not ready even when a
different Reviewer has `request_changes` or `block`; unresolved evidence is
neither pass nor non-pass. The only empty-required-set exception is the typed
Design `self_review` user-decision path defined below.

After readiness succeeds, an `approved` settlement requires every current
required Reviewer to have a passing outcome. A `changes_requested` or
`blocked` settlement must be the canonical reduction of the complete resolved
set or the explicit Design self-review decision. The Parent's expected outcome
never fills a missing node.

All continue and replacement Broker entry points enforce a durable
server-side fence before authorization checks or budget consumption. If the
latest run for the node has `completion_state = needs_decision`, or current
scope has an open `completion_artifact_recovery`, the request is rejected with
`completion_decision_required` or `completion_artifact_unavailable`. The state
may be superseded only by a real scope, artifact, or producer change, or by an
explicit workflow terminal/delete operation. Formatting, missing summary,
missing report path, malformed Card text, or unchanged retry prose can never
authorize continue or replacement.

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

Recovery policy branches explicitly by completion protocol. The v2
`exact_current_plan_approval` predicate is true only when:

1. the workflow uses completion protocol v2;
2. the latest Plan settlement outcome is `Approved`;
3. `plan_round_state_v2` contains every currently required Reviewer at rank 0;
4. the current Plan `evidence_scope_digest` and platform artifact identity
   match the settlement; and
5. no required Plan node has an open `completion_decision`,
   `design_self_review_decision`, or artifact-recovery attention.

The v2 branch never reads Critical/Important/Minor counts,
`summary_validated`, Cards, or a legacy fingerprint. The v1 branch retains its
existing counts/fingerprint behavior unchanged.

## Outcome-Based Document Gate Reduction

### Design Gate

For an external-review Design gate, the platform first requires resolved fresh
evidence for every required Design Reviewer. It then reduces the complete set:
all `approve`/`approve_with_minors` outcomes derive `Approved`; any
`request_changes` derives `ChangesRequested`; otherwise a current `block`
derives `Blocked`. Legacy Critical/Important/Minor count fields are null for v2
and `ApprovalWithOpenFindings` is a v1-only rule.

An empty-required-reviewer Design `self_review` is not reduction over an empty
set and never vacuously approves from the Parent's expected outcome. Under
completion protocol v2 it requires an authenticated typed user Design
adjudication of kind `design_self_review_decision`, bound by the same CAS class
to the current Design-root subject and gate lineage. Its role-bounded semantic
choices are
`approve`/`approve_with_minors`, `request_changes`, and `block`. The recorded
choice is the only semantic authority for the self-review settlement.

The self-review readiness transaction resolves the current Design bytes and
creates or reuses one `delegation_workflow_design_root_bindings` row for the
current `(workflow_id, gate_id, gate_lineage)`. The platform assigns a stable
`node_id` with role `design_root`, a platform-only `task_id`, and a
`latest_run_id` representing that Design-root subject revision; these are CAS
identities, not a delegated task, child conversation, budget, or executable
run. The row persists the Design identity, canonical Design-root
`evidence_scope_digest`, and graph revision. Material Design/policy change
mints a new lineage and row and supersedes the prior decision. Non-material
graph/display changes do not rotate the subject.

`design_self_review_decision` opens through a kind-specific gate-readiness API
in the same transaction that validates/creates this binding. Its attention row
uses the binding's platform task/run/node IDs, so the normal six-field CAS is
fully populated even though no child exists. Delegation, continuation, join,
and child cleanup APIs must reject those platform-only IDs.

The Parent may call `settle_workflow_gate` only after that user decision is
durably recorded. Parent prose, an expected settlement outcome, and an empty
required set cannot mint or replace the decision. The external Design Reviewer
path remains the normal outcome reduction above.

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

A round is reducible only after every current required Reviewer satisfies the
resolved freshness precondition. It approves only when every current required
Reviewer is rank 0. The next localized corrective round keeps the current
`gate_lineage`, increments `review_round`, and selects every current
non-passing Reviewer plus any passing Reviewer whose material selector
intersects the trusted localized change. Only those selected nodes need newer
evidence for that round. An unclassifiable/shared change is material rather
than localized. Initial and materially revised Plans and holistic rewrites
mint a new lineage and require the full current cohort. A roster-only amendment
opens a round under the same lineage for only newly added Reviewers, preserves
fresh sibling evidence, and does not increment stagnation. Removing a Reviewer
removes it from the required set without rewriting sibling state.

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
  evidence_validated: boolean
  attention: optional typed completion-decision summary
```

Counts, commits, and tests may continue to appear from runtime telemetry or
report projection, but they are not completion evidence. The UI must not show
model-authored IDs or digests as verified facts. `evidence_validated` means the
platform validated binding, artifact, scope, and outcome legality; it does not
claim that Codeg independently made the semantic judgment, whose source stays
visible in `source`.

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
the Parent to parse or repair a Card. After adjudication, the same projection
changes to `state = resolved` with the committed outcome and
`source = user_adjudication`.

## Attention and Continuation Policy

The B2D Skill and its validator are updated to enforce these rules:

- never emit or request `codeg-card-summary-v1` for a v2 workflow;
- never use the phrase or operation `CARD RE-EMIT ONLY`;
- never call `continue_delegation` because completion intent, summary, report
  path, or display projection is missing or malformed;
- advance only from platform completion state and workflow admission results;
- on `needs_decision`, surface the durable attention and wait;
- after a resolved decision event or a user “continue orchestration” turn,
  re-enter gate settlement/admission from durable state without calling
  `continue_delegation` on the completed child; and
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
`child_question`, `completion_decision`, `completion_artifact_recovery`, and
`design_self_review_decision` have separate payload validators, resolution
operations, and caller authorization.

Every completion-family resolver carries this full CAS envelope:

```text
attention_id
task_id
kind
captured_scope_digest
latest_run_id
node_id
```

All six fields must match the open row and current workflow projection. Missing
or mismatched fields fail closed; same-resolution replay is idempotent and a
different resolution after commit conflicts.

| Kind | Validated open payload | Resolution codes | Authorized resolver |
| --- | --- | --- | --- |
| `child_question` | Existing bounded question/options and live-child binding | Existing `parent_reply`, `task_terminal`, `parent_canceled`, `parent_turn_failed`, `join_abandoned`, `parent_disconnected`, `host_restarted` | Existing Parent-reply operation and Broker lifecycle paths only |
| `completion_decision` | Reason code, legal role outcomes, bounded candidates/diagnostics, and CAS binding | `user_outcome_committed`, `superseded`, `workflow_terminated`, `workflow_deleted` | Authenticated desktop/Web typed-outcome operation for user commit; Broker workflow lifecycle for the other codes |
| `completion_artifact_recovery` | `ArtifactRecoveryPayloadV1`: normalized selected intent/source reference, resolver failure, artifact-independent producer-scope digest, producer identity, and CAS binding | `artifact_resolved`, `superseded`, `workflow_terminated`, `workflow_deleted` | Typed artifact-retry operation for resolution; Broker workflow lifecycle for the other codes |
| `design_self_review_decision` | Current Design identity, legal Design outcomes, gate lineage, and CAS binding | `user_outcome_committed`, `superseded`, `workflow_terminated`, `workflow_deleted` | Authenticated desktop/Web Design-adjudication operation for user commit; Broker workflow lifecycle for the other codes |

`resolution_json` is kind-versioned. A user outcome resolution stores its code,
legal canonical outcome, authenticated actor identity, and committed scope
digest. Artifact resolution stores its code, resolver kind, and resolved
artifact identity. Lifecycle resolutions store the code and workflow graph
revision. Raw prose is not accepted by any completion-family resolver. A
Parent free-form reply remains valid only for `child_question`; neither Parent
prose nor `settle_workflow_gate` may resolve the other kinds.

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

`design_self_review_decision` follows the completion-decision CAS and lifecycle
rules. The Design self-review readiness path opens it for the current
Design-root binding when no valid decision exists. It does not require or wake
a child, and a material Design/scope change resolves the old row as
`superseded` before a current replacement may open.

An active workflow's completion decision has no automatic timeout. Cleanup is
limited to the explicit terminal/deletion cases above. Operations expose the
open count and oldest unresolved age so abandoned decisions are observable
without silently discarding them.

### Parent Resume After Adjudication

When authenticated user adjudication mints completion evidence or a typed
Design self-review decision, the same transaction bumps the workflow graph
revision and inserts a unique `completion_decision_resolved` row into
`delegation_workflow_outbox_events`. The payload names the workflow,
task/node, kind, outcome, evidence scope digest, and committed graph revision
without carrying raw prose. Parent status and completion projection
simultaneously expose `state = resolved` and the outcome.

After commit, an at-least-once dispatcher publishes pending outbox rows and
marks an attempt delivered only after handing it to the host's durable root
wake queue when that capability exists. Startup and periodic reconciliation
retry every undelivered row; the unique event key makes retries idempotent.
Event/history clients may replay from graph revision, and the current
projection is level-triggered, so a disconnected UI that misses a toast still
shows the resolved badge on reload. A crash after decision commit but before
publish therefore delays delivery rather than losing it.

Desktop and server runtimes publish the same durable event and display the same
toast/badge. If the host supports automatic root wake, it should schedule one
Parent orchestration turn from that event. Otherwise the UI tells the user that
one Parent “continue orchestration” turn is required. In both cases the B2D
Skill reloads current workflow state and re-enters `settle_workflow_gate` or
the next admission operation. It must not call `continue_delegation`, replace,
or reopen the completed child. Replayed wake events are harmless because
settlement and admission retain their normal graph/round CAS.

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
6. start at an open Design gate with no imported run, evidence, settlement,
   legacy gate-cycle state, gate lineage, or review-round state; and
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
| `completion_plan_material_invalid` | Plan material parsing is ambiguous, incomplete, or over bounds | Block Author pass/material lineage construction; corrected publication mints a new lineage |
| `completion_remediation_context_required` | A non-pass Final evaluation has no captured non-empty remediation bytes | Open typed user decision; do not synthesize findings or dispatch a Fixer |
| `completion_scope_changed` | Identity/artifact/scope changed before evidence commit or user resolution | Supersede decision and require current work evaluation |
| `completion_decision_superseded` | An open decision no longer covers the latest run/node/scope | Close stale attention and re-evaluate the current run |
| `completion_evidence_corrupt` | Persisted v2 evidence fails bounded schema validation | Fail closed and surface repair diagnostic |
| `completion_decision_required` | Terminal run awaits direct user adjudication | Stop Parent orchestration |
| `legacy_completion_protocol_restart_required` | A v1 workflow received a mutation/resume attempt under `v2_enforce` | Create/open linked v2 successor |

Parser diagnostics must say which channel and semantic reason failed without
logging full child output, report contents, user text, absolute paths, or
profile configuration.

## Concurrency and Idempotency

- `complete_work` uses `(task_id, child_tool_call_id)` plus the canonical
  request digest for idempotency/conflict detection. Before first Broker
  dispatch, the companion sets the tool-call ID from MCP `_meta.tool_use_id`
  or `rpc:{connection_incarnation_id}:{json_rpc_request_id}`. Redelivery reuses
  it; the same ID with another digest conflicts. Distinct valid IDs receive a
  durable per-task ordinal, whose highest value is authoritative.
  `platform:{task_id}:{accepted_ordinal}` is an audit/display label only.
- Terminal evidence materialization is unique per task. An identical retry
  returns the existing evidence or attention state.
- A terminal writer and user-decision writer compare the attention ID, task,
  kind, latest run, node, captured scope, and graph state; at most one current
  evidence record commits.
- Completion attention has at most one open row per task and kind.
- User-decision retries with the same typed outcome are idempotent; a different
  outcome after resolution is a conflict.
- Reviewer amendment and user decision race through the same workflow
  transaction/CAS boundary. Removal or replacement can supersede attention but
  cannot transfer evidence to another node.
- Startup reconciliation and Broker teardown apply kind-specific closure
  rules; generic task/Parent cleanup cannot win a CAS against a current
  completion decision.
- Durable outbox rows are written with the state transaction; external events
  are dispatched only after commit, at least once, and carry the committed
  graph revision.

## Security and Bounds

- Tool caller identity comes from the Broker connection, never request fields;
  `complete_work` additionally requires a workflow-bound v2
  `DelegationChild` run.
- Completion text, summary, and report hints retain existing payload limits;
  summaries are bounded to 4 KiB in evidence and more tightly in Card display.
- Report candidates remain limited to eight files and 512 KiB each.
- `PlanMaterialSchemaV1` rejects Plan files over 2 MiB, more than 100 Task
  sections, or any normalized section over 512 KiB.
- Final remediation snapshots retain the existing per-source read bounds and
  candidate-count bound; immutable storage is rejected rather than truncated
  when any bound is exceeded.
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
- bind workflow children to runs before MCP injection, append the canonical
  instruction block, stamp the `completion_v2` feature, and add the child-only
  `complete_work` operation to companion schema/dispatch with its stable
  pre-dispatch connection/request identity;
- add durable gate-lineage/round state and the shared canonical scope/identity
  source implementation with golden vectors, including the bounded
  `PlanMaterialSchemaV1` parser, material selectors, input-to-key fixtures, and
  conservative localized-change classifier;
- add the platform-only Design-root CAS binding and platform-generated Final
  findings-package lifecycle with immutable remediation-context snapshots;
- fence every continuation/replacement recovery entry point on current
  completion-decision and artifact-recovery state before authorization/budget;
- extend typed attention with kind-specific query, resolution, startup
  reconciliation, artifact-recovery intent payloads, transactional outbox
  events, and Broker teardown behavior;
- add SeaORM entities/migrations for protocol version, intents, evidence,
  settlement scope/Plan state, attention kind, and restart linkage using the
  four ordered `m20260804_...` migrations above; and
- expose completion projection consistently through Tauri, Axum, WebSocket,
  ACP history, and `codeg-mcp` status results.

Frontend and Skill surfaces:

- mirror `CompletionCardV2` and typed completion attention in TypeScript;
- render resolved, needs-decision, blocked, and legacy-restart states;
- add a direct role-bounded adjudication control with desktop/server parity;
- surface `completion_decision_resolved`, automatic-root-wake capability, and
  the fallback Parent-resume action without reopening a completed child;
- expose the server-owned creation mode and shadow/enforce telemetry to the
  existing operational configuration surface;
- update all locale files for new states and actions;
- replace v1 Card instructions in
  `.agents/skills/brainstorm-to-delivery/SKILL.md`, require a clean producer
  admission baseline and workflow-owned Implementer/Final-Fixer commits before
  pass, and remove any unrelated-dirt allowance for protocol-v2 worktrees; and
- update the Skill validator and behavior fixtures to reject format-repair
  continuation guidance.

Unrelated recovery authorization, work-unit grouping, transcript persistence,
and workflow overlay layout are outside this implementation boundary.

## Test Strategy

### Intent Parser

- Accept each canonical Reviewer and worker outcome through `complete_work`.
- Reject tool outcomes incompatible with the durable role.
- Expose `complete_work` only after run binding and only with the
  workflow-v2 `completion_v2` token feature; prove the Broker rejects every
  non-eligible caller even if it forges a catalog request.
- Coalesce delivery retries by tool-call ID and make the latest distinct valid
  call authoritative, including outcome changes.
- Before first Broker dispatch, prefer MCP `_meta.tool_use_id` or derive
  `rpc:{connection_incarnation_id}:{json_rpc_request_id}`; prove transport
  redelivery reuses it, same-ID/different-digest conflicts, and a distinct ID
  receives a new superseding ordinal. Prove the platform ordinal label is not
  accepted as a redelivery key.
- Generate the server-owned instruction block from durable scope, append it
  after supplemental Parent prose, bind its template/version digest into
  scope, and reject admission when construction/persistence fails.
- Accept casing, spacing, the exact optional `.!。！` punctuation set, eligible
  unindented list wrappers, English, and Chinese terminal-line variants.
- Cover every enumerated heading/list/bold wrapper in shared normative
  fixtures; reject combined heading+list, recursive/unmatched/partial wrappers,
  indented/nested list conclusions, and punctuation outside the exact set.
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
- Select the first workspace-relative `.md` path on a
  conclusion-adjacent plain line as the text report hint; prefer a selected
  tool intent's validated `report_file` for its audit/display hint, and prove
  neither hint participates in fallback outcome discovery.
- Discover report fallback only through final-response Markdown links and
  platform-observed touched Markdown files.
- Enforce candidate count, file size, workspace containment, and symlink rules.

### Evidence and Artifact Resolution

- Ignore false child/Parent task IDs, roles, kinds, revisions, and digests.
- Compute Plan and Design digests from exact file bytes.
- Change a Design/Plan file between the pre-read and settlement write and prove
  the in-transaction second hash prevents stale evidence from committing.
- For `git_head_v1`, admit an Implementer/Final Fixer only after resolvable
  platform Git `HEAD` and exactly empty `git status --porcelain`; persist that
  `producer_baseline_head` and prove unready workspaces never dispatch.
- Require a clean terminal `HEAD` different from the baseline for default pass;
  allow equal `HEAD` only under durable `allow_noop_verification = true`, and
  prove child prose cannot authorize the no-op.
- Make tracked, staged, untracked, and nominally unrelated user dirtiness,
  status-command failure, non-Git workspaces, unavailable `HEAD`, and missing
  required producer commit fail closed; never use a model-authored commit list.
- Allow a non-passing producer outcome to resolve without an artifact and
  prove typed retry rechecks baseline, current `HEAD`, cleanliness, and
  commit/no-op authorization for the same scope.
- For passing tool, terminal-text, and report intents, persist identical
  `ArtifactRecoveryPayloadV1` semantics, accept expected artifact recovery
  without failing producer-scope CAS, and supersede unrelated scope changes.
- At Task/Final Reviewer admission and terminal materialization, re-run `HEAD`
  plus porcelain, require clean `HEAD == producer artifact digest`, classify
  commit drift as `completion_scope_changed`, and classify dirt/command failure
  as `completion_artifact_unavailable`. Keep `git_snapshot_v2` outside this
  slice.
- Require history tidy/aggregation that changes `HEAD` to complete before Final
  Reviewer admission; Final binds the delivered tip.
- After passing Final settlement, re-read `HEAD` at delivery/finalization and
  reject any commit id different from Final evidence as `final_artifact_drift`,
  reopening Final review; never ship a post-approval commit.
- Persist the selected producer task and generation from admission state.
- Atomically write terminal status, evidence/binding projection, graph
  revision, and events.
- Keep a resolved semantic intent non-passing when required artifact identity
  is unavailable.

### Persistence and Migration

- Upgrade each supported historical schema through
  `m20260804_000002_completion_scope_and_gate_settlement`, compare every v1
  settlement column byte-for-byte, and verify all prior/new checks, foreign
  keys, indexes, gate state, Design-root binding, and Final-package tables
  after the rebuild.
- Prove v2 settlement counts are null and are never interpreted as zero.
- Upgrade each supported attention schema through
  `m20260804_000004_typed_completion_attention`, preserve every v1 row as
  `child_question`, and verify kind-specific child-conversation/tool-call
  nullability and checks plus both partial unique indexes.
- Inject copy, schema, index, and foreign-key-check failures into each rebuild
  and prove the transaction leaves the original table/data intact.
- Verify Design-root platform IDs are durable CAS subjects but cannot be used
  with delegation/continue/join APIs.
- Create, supersede, resolve, and hash `FinalFindingsPackageV1` solely from
  complete platform Final evidence/routes plus ordered captured remediation
  bytes. Prove changing a captured snapshot changes identity while interpreted
  prose, severity/count meaning, and Parent input cannot drive the gate.
- Store available report/terminal contexts immutably at package mint; verify
  base64 bytes, length, and digest at Fixer admission and settlement without
  re-reading mutable paths. Corrupt snapshots fail closed, and a non-pass Final
  with no non-empty available context opens typed decision instead of a Fixer.
- Prove new v2 runs leave `card_summary_json` null and no v2 validator parses
  or validates that column.

### Scope Freshness

- Parse Plan fixtures with the shared CommonMark-compatible AST and exact
  `PlanMaterialSchemaV1` heading/boundary rules; cover H2/H3 Tasks, optional
  titles, nested content, literal fences/tables/lists, global keys, and absent
  front matter/global sections.
- Fail duplicate or referenced-missing Tasks, ambiguous material, more than
  100 Tasks, Plan files over 2 MiB, and sections over 512 KiB with
  `completion_plan_material_invalid`; truncation and partial lineage are
  forbidden.
- Verify NFC/LF/trailing-whitespace/single-final-LF normalization, canonical
  task identity JSON, selector key expansion, policies fingerprint, key-set and
  body-hash change classification, and published input-to-key golden fixtures.
- Republish an estimated Plan with a selector/map key-set or body-hash change
  and prove it mints a new lineage; retain a lineage after review only through
  an authorized, completely covered `PlanLocalizedChangeV2`.
- Prove the construction order is parser and identities, instruction block,
  review scope, then evidence scope, with no circular digest dependency.

- Preserve completed sibling reviews after removing an unavailable Reviewer.
- Preserve siblings and require only the new node after replacement.
- Preserve evidence across title-only, graph-only, unrelated-task, and
  roster/revision-counter-only manifest changes whose effective review policy
  is unchanged.
- Invalidate on document/code digest, producer task/generation, node identity,
  material review scope, new gate lineage, or workflow change.
- Keep `gate_lineage` stable across localized corrective rounds, increment the
  platform-assigned `review_round`, classify normalized changed material,
  select non-pass plus intersecting Reviewers, and preserve only siblings whose
  subject material is proved unchanged.
- Edit material covered only by a selected Reviewer and prove an unselected
  sibling remains eligible despite a changed full Plan digest; make ambiguous,
  shared, policy, and uncovered edits mint a new lineage/full-cohort round.
- Mint a new lineage and invalidate all gate Reviewer evidence for a material
  Plan revision, holistic rewrite, Design change, or requirements reset.
- Prove required-reviewer sets and `manifest_revision` are absent from scope
  canonicalization.
- Prove every v2 material input change invalidates scope.
- Independently prove roster/title/layout/revision-counter-only changes can
  change legacy Design/Plan/content fingerprints without changing v2 scope.
- Run the same golden canonical-JSON vectors through every validator consumer,
  including requirements, task-specification, Final-findings, and instruction
  block identities.
- Change the order or content digest of a Final remediation context and prove
  `final_findings_identity`, the Fixer instruction block, and Fixer scope all
  change together.
- Reject a digest built with another domain/schema version.

### Gate Safety

- Permit Reviewer pass only for `approve` and `approve_with_minors`.
- Permit Author/Implementer/Fixer pass only for `done` and
  `done_with_concerns`.
- Treat `request_changes`, `block`, and `blocked` as valid non-pass evidence.
- Never count infrastructure completion alone.
- Never count unresolved attention as terminal gate evidence.
- Reject settlement until every current required Reviewer has resolved fresh
  evidence; specifically reject mixed `needs_decision + request_changes` and
  `needs_decision + block` sets without reducing either outcome.
- Derive gate evidence from the store rather than caller-submitted IDs or
  digests.
- Exercise `admission.rs`, `store.rs`, `gates.rs`, `project.rs`, and
  `recovery_policy.rs` with v2 evidence and prove none requires
  `summary_validated` or a Card status/verdict.
- Derive Design approval/changes/blocked solely from required Reviewer
  outcomes; counts remain null/non-authoritative.
- For empty Design `self_review`, require a current authenticated typed
  `design_self_review_decision` against a persisted Design-root binding;
  reject stale lineage/Design identity, Parent expected-outcome prose, vacuous
  empty-set approval, and attempts to treat platform IDs as child runs.
- Keep failed/canceled recovery and reviewer-amendment behavior intact.

### Recovery Policy

- Keep the v1 `exact_current_plan_approval` counts/fingerprint branch
  unchanged.
- For v2, require protocol v2, latest Plan settlement `Approved`, every
  required `plan_round_state_v2` rank equal to 0, matching current Plan scope
  and artifact, and no open completion decision on required Plan nodes.
- Prove v2 recovery eligibility is unchanged by counts,
  `summary_validated`, Card content, or legacy fingerprints.
- Reject continue and replacement before authorization/budget consumption for
  current `needs_decision` or artifact-recovery state at every direct Broker
  entry point; allow supersession only for material scope/artifact/producer
  change or explicit workflow terminal/delete.

### Plan Outcome Reducer

- Approve only when all current required Reviewers are rank 0.
- Re-review current non-pass Reviewers plus any passing Reviewer whose material
  selector intersects a localized correction.
- Require the full cohort for initial, material, and holistic revisions.
- Persist `gate_lineage`, platform-assigned `review_round`, required node set,
  evidence task IDs, and scope digests for every settlement; treat an expected
  round as CAS only.
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
- Open completion kinds through the transaction-only terminal API after the
  workflow-bound run completes; prove the live running-only
  `child_question.open_or_recover` path cannot be reused.
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
- Validate the exact payload and resolution-code matrix for
  `child_question`, `completion_decision`, `completion_artifact_recovery`, and
  `design_self_review_decision`, including all six CAS fields and authorized
  Broker/UI callers.
- A direct user choice creates evidence without waking, continuing, or
  replacing the child.
- Emit `completion_decision_resolved` only after commit with a bumped graph
  revision through the transactional outbox; crash after state commit and
  before publish, restart, and prove at-least-once desktop/server delivery plus
  level-triggered badge recovery.
- Exercise automatic root wake and user-triggered Parent resume. Both must
  reload durable state and re-enter settle/admission without continuing or
  reopening the completed child.
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
- No old run, evidence, settlement, legacy gate cycle, gate lineage,
  review-round state, or task ID appears in the new workflow.
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
- artifact-resolution failure by resolver kind, producer admission baseline,
  commit/no-op condition, and Reviewer revalidation phase;
- evidence invalidation by changed scope dimension;
- localized Plan classification, selected-material intersections, and
  fail-closed material-lineage resets, including Plan parser diagnostics and
  bound failures;
- Final findings packages created, superseded, and resolved, plus available,
  missing, and corrupt immutable remediation contexts;
- completion outbox pending age, retry count, and delivery latency;
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
   semantic outcome, summary, and optional report path. Its catalog exposure
   requires the workflow-v2 `completion_v2` token bit after run binding, and
   the Broker rejects every ineligible caller. Before dispatch, the companion
   supplies a stable MCP tool-use ID or connection-incarnation/JSON-RPC request
   ID; redelivery reuses it, same-ID/different-digest conflicts, and accepted
   ordinals are supersession/audit values rather than transport identity.
3. Every admitted protocol-v2 workflow child receives a server-owned canonical
   instruction block whose template/version, role conclusion suffix, binding
   summary, and digest are platform-derived and included in scope; Parent task
   prose is supplemental only.
4. Codeg derives role, phase, task, workflow, gate lineage, selected review
   round, producer, revision provenance, artifact identity, and evidence scope
   from durable state. Expected round/revision values are concurrency CAS only.
5. Model-authored Plan/code digests and task/role identities are ignored.
6. Implementer/Final-Fixer admission records a resolvable clean
   `producer_baseline_head` or fails before dispatch. Passing `git_head_v1`
   completion requires a clean current `HEAD` and a workflow-owned producer
   commit relative to that baseline unless durable Task policy explicitly
   authorizes no-op verification. There is no unrelated-dirt allowlist; Final
   aggregation does not substitute for the producer commit and must finish
   before Final Reviewer admission. Non-pass may resolve without an artifact,
   while recovery persists intent and rechecks the complete baseline/commit
   contract.
7. Task and Final code Reviewers re-run `HEAD` plus porcelain at admission and
   terminal materialization and require clean `HEAD` equal to the producer
   evidence digest. Commit drift is `completion_scope_changed`; dirt or command
   failure is `completion_artifact_unavailable`. After passing Final, delivery
   requires the same Final evidence commit id; post-settlement drift is
   `final_artifact_drift` and reopens Final. Worktree-byte resolver
   `git_snapshot_v2` remains future work.
8. v2 admission and settlement use `completion_evidence_json`, never
   `card_summary_json` or `summary_validated`; new v2 runs leave
   `card_summary_json` null.
9. Cards are deterministic projections and cannot affect workflow validity.
   `evidence_validated` describes platform binding/artifact/scope validation,
   not independent semantic judgment.
10. Missing, conflicting, or role-incompatible meaning opens one durable user
    decision and creates no continuation or replacement run.
11. Reviewer pass is limited to `approve`/`approve_with_minors`; producer pass
    is limited to `done`/`done_with_concerns`.
12. Settlement is illegal until every current required Reviewer has resolved
    fresh evidence. `needs_decision` mixed with any non-pass outcome is not
    reducible.
13. External Design and Plan gates are derived from platform-bound Reviewer
    outcomes; child/Parent findings and counts cannot approve, block, or
    advance a round.
14. Empty Design `self_review` requires an authenticated typed
    `design_self_review_decision` bound to a persisted platform-only
    Design-root subject for current Design scope. Its task/run/node IDs satisfy
    CAS but are never valid delegation targets; Parent prose or expected
    outcome cannot mint approval.
15. `gate_lineage` remains stable across localized corrective rounds, whose
    trusted Plan-change artifact selects all non-pass and material-intersecting
    Reviewers. Unselected siblings remain valid only when their selected
    material is unchanged; ambiguous/shared/uncovered edits, a material Plan
    revision, holistic rewrite, Design change, or requirements reset mint a new
    lineage and invalidate prior gate Reviewer evidence. Any key-set or
    normalized body-hash change when the Author republishes estimated also
    mints a new lineage.
16. Plan corrective rounds use the approved strict-improvement relation,
    selective re-review, two-round rewrite threshold, and post-rewrite user
    decision threshold. Each settlement records lineage, round, required node
    set, evidence task IDs, scope digests, current full Plan artifact, and any
    localized-change digest.
17. Removing or replacing an unavailable Reviewer preserves unchanged sibling
    evidence. Subject artifact/material, producer, node, instruction, material
    review scope, lineage, or workflow changes invalidate the affected
    evidence; a full Plan digest change alone does not stale an unselected
    Reviewer whose selected material is proved unchanged.
18. Legacy `design_fingerprint`, `plan_fingerprint`, and
    `content_fingerprint` retain v1 audit/superset meaning. Every v2 material
    change invalidates v2 scope, while roster/title/layout/counter-only legacy
    fingerprint changes do not.
19. `PlanMaterialSchemaV1` uses the shared CommonMark-compatible AST, exact
    H2/H3 Task grammar and boundaries, unique referenced indices, required
    global keys, NFC/LF/trailing-whitespace normalization, bounded literal
    section bytes, canonical Task identity JSON, and manifest policy/route
    selectors. Duplicate/missing/ambiguous or over-bound input fails closed;
    parser input-to-key fixtures and construction order are normative.
20. `FinalFindingsPackageV1` binds ordered report/terminal context metadata and
    exact immutable snapshot digests into `final_findings_identity` and the
    Final Fixer instruction/scope while leaving finding semantics untrusted.
    Admission and settlement verify stored bytes, and a non-pass Final with no
    captured non-empty context opens typed user decision instead of a Fixer.
21. Every role scope field has the specified durable source and normalization;
    requirements, Plan subject/change, platform Final-findings package, task
    specification, and instruction binding identities pass shared golden
    canonical-JSON vectors. Final package state comes only from complete
    platform Final evidence/routes and captured context, never Parent findings
    or interpreted child prose.
22. Migration 2 transactionally rebuilds gate settlements, and migration 4
    transactionally rebuilds attention, preserving all v1 data/schema objects,
    validating foreign keys, creating the gate/Design-root/Final-package/outbox
    tables, and rolling back fully on failure. Null counts never mean zero.
23. Terminal completion/artifact attention opens only through the
    kind-specific terminal/artifact transaction against a completed
    workflow-bound run and never reuses the running-only `child_question` open
    path. Design self-review opens only through gate readiness against the
    durable Design-root subject.
24. Each attention kind enforces its defined payload, resolution codes,
    six-field CAS, and Broker/UI caller authorization. A current completion
    decision survives task terminal cleanup, restart, Parent teardown,
    disconnect, and join abandonment.
25. User adjudication transactionally enqueues durable
    `completion_decision_resolved` with a bumped graph revision. At-least-once
    outbox dispatch and startup replay close the commit/publish crash window;
    automatic or user-triggered Parent resume re-enters settlement/admission
    without continuing or reopening the child, with desktop/server parity.
26. Every continue/replace entry point rejects current `needs_decision` or
    artifact-recovery state before authorization or budget use. Only material
    scope/artifact/producer supersession or explicit workflow terminal/delete
    can clear the fence; format repair never can.
27. v2 `exact_current_plan_approval` depends only on protocol, latest Approved
    settlement, all-rank-0 `plan_round_state_v2`, matching current Plan
    scope/artifact, and no open decisions on required Plan nodes. The v1
    counts/fingerprint branch remains unchanged.
28. Session-2889-style wrong Card templates cannot block Plan Reviewer
    admission when a valid semantic conclusion and platform-resolved Plan
    artifact exist, and new v2 sessions contain no format-only
    `CARD RE-EMIT ONLY` child runs.
29. Existing v1 sessions remain readable but immutable under normal
    `v2_enforce`; resume creates or reopens exactly one linked v2 session and
    reruns the workflow from Design through Final with no reused evidence.
30. Rollout mode is frozen per workflow; shadow has no semantic side effects,
    and rollback affects future creation without downgrading active v2 work.
31. `admission.rs`, `store.rs`, `gates.rs`, `project.rs`, and
    `recovery_policy.rs` all consume the same v2 validator. Desktop, server,
    and `codeg-mcp` expose the same completion truth and pass the required
    verification suites.
