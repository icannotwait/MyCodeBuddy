---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm to Delivery

Treat the completed Brainstorm named in this conversation as the requirements
baseline. Do not re-brainstorm. Advance to a locally deliverable result except
at explicit hard gates.

**Core principle:** The parent orchestrates. A Codex Plan Author owns every Plan
file and revision. Risk-routed Task cohorts implement and review. The parent
must not implement Task code and must not write or rewrite the Plan.

**REQUIRED SUB-SKILL:** Invoke and fully follow `subagent-driven-development`
for workspace gates, Task briefs, reports, review packages, fix loops, ledgers,
and final whole-branch review. **REQUIRED SUB-SKILL for Plans:** the Codex Plan
Author must invoke and fully follow `writing-plans`. Do not paste those Skills'
full procedures here.

**Violating the letter of these routing rules is violating their spirit.**

## Codeg roles and tools

| Route | Role | Agent |
| --- | --- | --- |
| Normal | Implementer / fixer | Grok |
| Normal | Independent reviewer | Codex |
| High | Implementer / fixer | Codex |
| High | Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| High | Independent reviewer 2 | Grok (independent child) |

Cross-agent routing uses Codeg `delegate_to_agent` / `continue_delegation`
(`agent_type: "grok"` | `"codex"`). Discover delayed MCP tools before reporting
agent unavailability. `spawn_agent` cannot select Grok and is not the Codeg
route. Missing required agents or tools → hard block; never substitute agent
types or implement in the parent.

### Thread ledger and A1 `work_unit_key`

Maintain a durable thread ledger in `.superpowers/sdd/progress.md` (or
equivalent): `work_unit_key`, role, `agent_type`, profile, child id,
`latest_task_id`, state, recovery_count, replacement metadata, plus workflow
id / publication_token / revisions / gate settlements / plan path+digest.

Keys are workspace-relative, NFC/B1-normalized, `|`-separated, ≤200 scalars:

| Unit             | Materials                                           |
| ---------------- | --------------------------------------------------- |
| Design reviewer  | `design\|{rel}\|reviewer\|{agent}\|{profile\|none}` |
| Plan Author      | `plan\|{rel}\|author\|codex\|{profile\|none}`       |
| Plan reviewer    | `plan\|{rel}\|reviewer\|{agent}\|{profile\|none}`   |
| Task implementer | `task\|{n}\|implementer\|{agent}\|{profile\|none}`  |
| Task reviewer    | `task\|{n}\|reviewer\|{agent}\|{profile\|none}`     |
| Final reviewer   | `final_review\|reviewer\|{agent}\|{profile\|none}`  |
| Final fixer      | `final_review\|fixer\|{agent}\|{profile\|none}`     |

Prefer `continue_delegation` when the ledger shows a recoverable thread.
Same-key cold `delegate_to_agent` without `replaces_task_id` is invalid once
lineage exists. Recovery is a **new turn**: re-inspect disk, treat prior
reasoning as provisional, rebuild missing reports, and re-run covering tests.

## Workflow capability (v2 only)

Before first Design dispatch, call `get_workflow_capabilities`. Require the
full v2 tool set (`get_workflow_capabilities`, `get_workflow_state`,
`publish_workflow_manifest`, `settle_workflow_gate`, `recover_workflow`).
Missing or inconsistent tools → hard block. **Do not use legacy capability
mode or any pre-v2 manifest schema.**

Publish with schema v2 only (`workflow_kind=brainstorm_to_delivery`), required
`plan_target_rel_path`, `risk_policy_version: "b2d_task_risk_v1"`, Task
policies/routes, Plan `reviewer_cohort_node_ids`, and Author node. Use a ledger
`publication_token` (UUID) for skeleton create; same digest retries reuse it;
digest mismatch → hard stop + `get_workflow_state`. CAS updates carry
`workflow_id` + `expected_manifest_revision`.

Lifecycle: skeleton (target path + Author work unit, no Plan digest yet) →
estimated (digest, matrix, policies, routes, full initial required set) →
parent `settle_workflow_gate` → approved. Material Plan change demotes to
estimated and reopens Plan review.

Recovery is index-first. Treat recovery_sources and actionable_task_routes as
authoritative. Read each workspace report_file before settlement, use
get_session_info for bounded child transcripts, and use get_delegation_status
for selected run outcomes. Never depend on inline finding summaries.

Recovery is status-first and resume-first. Cancellation-family evidence never
maps to unresumable, and tool_stalled_timeout is not a replacement source.
For tool_stalled_timeout, use a confirmed same-key continue; only genuine
unexpected transport loss may continue without confirmation when central
policy permits.

Delegation recovery follows this exact ordered recipe: make the projected call;
receive typed recovery_confirmation_required; call
request_recovery_authorization; then replay the exact rejected continue or
replacement call with recovery_authorization_id and the same key, profile, and
action. Never persist recovery_authorization_id in status, ledger, report, or
completion projection.

Workflow recovery follows this exact ordered recipe: get_workflow_state; call
request_recovery_authorization; then call receipt-required recover_workflow.
An enabled catalog missing recover_workflow hard-blocks. recover_workflow never
generates a challenge. user_decision_required requires exact reset_plan_lineage
authorization tied to the displayed reason hash; its receipt is the durable
requirements-change reason and begins a new authorized stagnation baseline.

First admission freezes the key, role, agent, profile, and inherited continue
and replacement counters. Pre-admission profile or route correction is a
material Plan revision. Recovery never changes key/profile or resets inherited
consumption. Exhausted continue uses same-key budget_exhausted_continue replacement
only while replacement budget remains; after replacement consumption, block.

Before every delegation or continue, write ledger intent with intended key,
role, agent, profile, and action. Fill latest_task_id after admission and
reconcile from platform state after recovery.

### Protocol-v2 completion and durable re-entry

For a protocol-v2 workflow, workers call `complete_work` when exposed or emit
one explicit terminal or report conclusion otherwise. The Parent advances only
from platform `completion.state` and workflow admission state. It never asks a
child to supply semantic IDs, digests, a Card, or completion-format repair.

When `completion.state` is `needs_decision` or `artifact_recovery`, surface the
durable typed attention and wait. After resolution or a user continuation turn,
reload workflow state at the root and re-enter gate settlement or admission.
Never continue, replace, or reopen the semantically terminal child. Genuine
incomplete work, stall, cancellation, and transport or process loss stay on the
existing typed recovery path.

Design, Plan, Task, and Final gates advance from platform outcomes and validated
scope. Review pass outcomes are `approve` and `approve_with_minors`; producer
pass outcomes are `done` and `done_with_concerns`. For a non-pass Final, consume
only the platform Final-findings package and context before dispatching the
Final Fixer.

### Frozen v1 historical branch

A workflow whose frozen completion protocol remains v1 retains its historical
Card/count settlement behavior. This branch is only for legacy history. A v2
successor starts from durable root restart state; no v1 evidence or settlement
may cross into it.

Normal Task review independently recomputes b2d_task_risk_v1. Migration,
security/authorization, concurrency, persistence/state-machine,
externally visible compatibility, and ambiguity each trigger external Design
review.

Respect platform bounds (Tasks≤100, nodes≤400, edges≤800, gates≤50, adj≤4KiB,
JSON≤512KiB). Details of wire fields live in tools/validation—do not restate
full schemas here.

## 1. Conditional Design review

Self-check the Brainstorm. External Design review when: cross-module/large
surface, migration, concurrency, security, real ambiguity, or high-risk design
without independent evidence. Otherwise v2 still uses Design gate
`resolution_mode=self_review` with empty `required_reviewer_node_ids` plus
design rel_path/digest, then explicit settle.

With external Design reviewers: `parent_adjudication` and the full required
set. After a Design revision, reload workflow state and follow the
platform-selected reviewer nodes and lineage. Pause for the user only if fixes
change requirements, scope, architecture, or user data handling.

## 2. Plan production

1. Publish a v2 skeleton with the target Plan path and a **fresh Codex Plan
   Author** work unit (`plan|…|author|codex|…`).
2. Dispatch that Author. Require complete `writing-plans` behavior. Parent
   supplies Design, constraints, configured Plan reviewer group, policy version
   `b2d_task_risk_v1`, and Task Routing Matrix format. **Author owns the Plan
   file and all revisions. Parent must not write or rewrite the Plan**; parent
   may only reject invalid output and adjudicate evidence.
3. Publish estimated with the platform-resolved Plan digest, Task Routing
   Matrix, task policies/routes, complete Plan `reviewer_cohort_node_ids`, and
   full initial `required_reviewer_node_ids`. Plan reviewers are separate
   children from the Author and each other. Never ask the Author or reviewers
   to produce IDs or digests used as semantic authority.
4. **Initial Plan review** follows the complete platform-selected reviewer set
   and its current gate lineage.
5. After each Plan revision, reload workflow state and dispatch exactly the
   platform-selected Plan nodes for that lineage. Do not reconstruct a review
   round from model findings, severity counts, expected rounds, or a parent
   ledger.
6. Material Plan changes still republish through manifest CAS. Scope, public or
   shared interfaces, Task decomposition/order, any Task risk or route,
   persistence, migration, security, concurrency, lifecycle, or reviewer
   membership/profile changes let the platform open the required full-group
   lineage. Do not locally override its selected set.
7. Platform `user_decision_required` or another typed hard block is the only
   pause for Plan stagnation or a requirements change. A platform-selected
   holistic rewrite remains Author-owned. Only a
   user-approved requirements change with its durable receipt begins a new
   lineage. Use the exact
   authorization/recovery path above, then reload that lineage from the root.
8. On Plan recovery, use the index-first procedure, verify referenced reports,
   and follow platform-selected nodes and lineage. Reports are operational
   context, not semantic settlement evidence.

## 3. Task risk policy (`b2d_task_risk_v1`)

Classify every Task **before** first implementation admission. Missing,
contradictory, or unversioned risk data invalidates the manifest—never silent
`normal`.

**Hard triggers → always `high`:**
`concurrency_lifecycle`, `security_trust_boundary`,
`migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`,
`update_rollback`.

**Soft signals (no hard trigger):** sum distinct active scores —
`cross_runtime_or_process`=2; each of `broad_production_surface`,
`multiple_ownership_modules`, `shared_interface`, `dependency_or_build`,
`multi_layer_without_test_seam` =1. Soft ≥3 → `high`; 0–2 → `normal`.

Author records evidence, not bare booleans. Matrix row per Task: index, title,
files/modules, hard/soft evidence, soft total, final level+reason, implementer,
reviewer set, policy version. Matrix and manifest must agree or approval blocks.

**Pre-admission** risk correction requires a **material Plan revision** and
**full-group** Plan review. **Post-admission** invalidation **blocks** the Task
and escalates; preserve frozen cohort and partial work. Do **not** mutate
`cohort_frozen`, swap implementer, append an unreviewed route, or continue under
a known-wrong classification.

## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |

- First admission of any cohort member freezes policy + all route identities
  (`cohort_frozen`).
- High gate is **strict AND**. Both reviewers start from the same package
  (may run concurrently). Parent waits for both, deduplicates, sends **one
  consolidated fix** to the implementer. Every new implement/fix artifact
  invalidates both prior reviews (including an earlier approve); both
  reviewers re-review the latest artifact after every fix.
- Every review must cover the latest producer `task_id` as `reviewed_task_id`
  and the same non-empty platform-resolved `artifact_digest`. Code Reviewers
  re-resolve a clean `HEAD` at admission and completion and require it to equal
  the producer commit recorded by platform evidence.
- Missing, failed, stale, or unavailable reviewer → **block**. Never
  downgrade high→normal to save time/tokens. High never ships on a single
  approval.
- Serial Task implementation only. Match A1 keys to approved manifest nodes.
- Canceling a frozen cohort: finish its gate or publish blocked/canceled
  **retaining bindings**—never silent drop; never “stop talking” as cancel.

## 5. Workspace gate (before Task execution)

When the approved Plan is about to execute (again after material re-approval):
inspect `git status` and full unstaged/staged diffs. Before every protocol-v2
Implementer or Final Fixer admission, the platform must resolve `HEAD`, require
`git status --porcelain` to be exactly empty, and persist
`producer_baseline_head`. An unresolvable or dirty baseline blocks dispatch;
there is no unrelated-dirt allowance. Never stash, commit, overwrite, or
discard user work unasked.

A passing Implementer or Final Fixer completion requires a clean
workflow-owned producer commit different from that baseline. Only durable Task
policy `allow_noop_verification = true` may authorize a verified no-op. Task and
Final code Reviewers validate clean `HEAD` against that producer commit at both
admission and completion.

## 6. Execute with SDD specialization

Run full `subagent-driven-development`. B2D only overrides agent routing:

- Risk route replaces generic model discretion for Task agent types.
- Normal: one Grok implementer + one Codex reviewer.
- High: Codex implementer + Codex and Grok reviewers; join at one gate.
- Admitted key, role, agent, and profile remain frozen through every fix round.
- Finish all Final history aggregation, tidy commits, and branch-tracked report
  changes before Final Reviewer admission. Final whole-branch review remains a
  new Codex child after all active Task gates pass; Final Fixer is Grok on a
  platform-projected non-pass Final only.
- A passing Final freezes the reviewed delivery `HEAD` through delivery and
  reporting. Post-settlement drift is `final_artifact_drift`; reopen Final on
  the new platform lineage instead of delivering or adding a post-pass commit.

## 7. Verify, commit, report

Per-Task targeted checks; final scope-appropriate test/lint/build. Re-run after
fixes. Each passing producer commits only workflow-owned changes before review;
local commits only—no merge/push/PR. Prepare branch-tracked aggregation and
reports before Final Reviewer admission. After a passing Final, deliver and
report the same frozen commit without another branch mutation. Final reporting
includes results, diffs, commands, reviews, retained Minors/risks, commits,
worktree, and blockers.

## Quick reference under pressure

| Pressure | Required action |
| --- | --- |
| “Finish Plan review fast” after an Author revision | Reload state and dispatch exactly the platform-selected Plan nodes for the current lineage. |
| “Migration is mechanical—use cheap Grok” | Hard triggers → **high**: Codex implementer + Codex **and** Grok reviewers |
| High Task: one reviewer approved, other unavailable | **Block**; never pass or downgrade |
| Plan stagnation appears unresolved | Follow the platform-selected holistic rewrite or typed user-decision route; never rebuild it from counts. |
| “Parent will tweak the Plan / Task code” | Forbidden. Author owns Plan; routed implementers own code |
| Pre-admission risk looks wrong | Material Plan revision + platform-selected full-group lineage |
| Post-admission risk looks wrong | Block + escalate; do not mutate `cohort_frozen` |
| Urgency / small Task | Still Author + Plan review + risk route + SDD |
| Agent unavailable | Hard block; no agent substitution |
| Design Gate approved | Design Gate approved -> dispatch Plan Author automatically. |
| Plan Gate approved | Plan Gate approved -> run Workspace gate, then dispatch the first eligible Task automatically. |
| Task Gate passed | Task Gate passed -> dispatch the next eligible Task or Final review automatically. |
| Final review approved | Final review approved -> deliver and report the frozen commit automatically. |
| Final has a platform non-pass outcome | Consume only the platform Final-findings package/context, then dispatch Final Fixer (Grok). Do not request extra user approval. |
| `needs_decision` or artifact recovery | Surface durable attention and wait. After resolution, reload root workflow state and re-enter settlement/admission; never continue or replace the terminal child. |
| Protocol-v1 history | Keep it on the frozen v1 historical branch; restart a v2 successor at the root and never import v1 evidence or settlement. |
| Cancellation-family / `tool_stalled_timeout` | Never map cancellation to `unresumable`; stall uses confirmed same-key continue before replacement. |
| Typed delegation recovery challenge | Projected call -> `recovery_confirmation_required` -> `request_recovery_authorization` -> exact rejected call replay with the ID. |
| Workflow recovery | `get_workflow_state` -> `request_recovery_authorization` -> receipt-required `recover_workflow`; a missing enabled tool hard-blocks. |
| Continue budget exhausted | Same-key `budget_exhausted_continue` replacement if its budget remains; otherwise block. |
| Any phase could continue | Only pause for a hard block, `user_decision_required`, or an unresolved choice that changes requirements, scope, architecture, or user data handling. |
| Ledger or document status is stale/conflicting | Run `get_workflow_state`, reconcile durable state, then continue. Stale text is not a gate. |
| Context compacted / recovery resumed | One index read for gates, selected nodes, lineages, and current/next routes; read referenced reports and bounded secondary detail before settle |

## Rationalizations

| Excuse | Reality |
| --- | --- |
| “Full group every Plan revision is safer.” | Reload the platform-selected nodes and lineage; material changes select the required full group. |
| “One Codex approval is enough on high.” | High is strict AND on both reviewers covering the same latest artifact. |
| “Grok is fine for migrations/updaters.” | Those hard triggers force high + Codex implementer. |
| “Skip the missing reviewer to ship.” | Unavailable required reviewer blocks the gate. |
| “I’ll rewrite the Plan in the parent.” | Parent must not write the Plan; continue the Author. |
| “Change the route after admission.” | `cohort_frozen`; post-admission invalidation blocks, never mutates. |
| “I can infer the next holistic rewrite from finding counts.” | Follow the platform-selected Author/reviewer route; counts are not v2 authority. |
| “Reset Plan lineage after compaction.” | Platform state is durable; only a user-approved requirements-change receipt opens a new lineage. |
| “Direct parent implement is faster.” | Parent only orchestrates. |
| “`spawn_agent` can’t pick Grok.” | Use Codeg `delegate_to_agent`. |
| “The Gate is approved, but I should ask the user to confirm the next phase.” | An approved Gate is not a user gate. Dispatch the next admissible work unit in the same parent turn. |
| “A ledger or document still says pending, so I should stop.” | Reconcile with `get_workflow_state`; only its hard block or an explicit pause condition may stop advancement. |
| “Cancellation means the child is unresumable, so replacement is faster.” | Cancellation-family evidence never proves `unresumable`; preserve the admitted key/profile and follow the confirmed authorization sequence. |
| “A stalled tool can be replaced immediately because the deadline is tight.” | `tool_stalled_timeout` is resume-first and requires a confirmed same-key continue. |
| “The old four-tool catalog is close enough.” | Workflow recovery requires `recover_workflow`; its absence from an enabled catalog hard-blocks. |
| “The user approved in prose, so the workflow can reset.” | Prose never settles. `user_decision_required` needs the exact reason-hash-bound `reset_plan_lineage` receipt. |
| “Final said request_changes in prose / report; start fixer.” | Reload platform completion state. Only its non-pass outcome plus Final-findings package authorizes the Final Fixer. |
| “The child concluded, but completion is unclear; continue it for a cleaner answer.” | A semantically terminal child is never continued or replaced. Surface typed attention, wait, then re-enter from root state. |
| “Final passed; add one tidy/report commit.” | Final freezes the reviewed commit. Aggregate before admission; any later drift reopens Final. |

## Red flags — stop

- Parent writing Plan or Task code
- Pre-v2 manifest schema, legacy capability mode, or old pair-freeze wording
- Full Plan group forced for purely localized open-finding re-review
- High Task shipping on a single approval or a downgraded route
- Grok implementer on hard-trigger Tasks
- Gate pass without matching `reviewed_task_id` + non-empty `artifact_digest`
- Silent cohort drop or conversation-stop as cancel
- Cancellation or stall treated as proof of `unresumable`
- Recovery changes key/profile, resets consumption, or skips the typed challenge
- Prose approval, missing recovery receipt, or an incomplete enabled tool catalog

## End-to-end example

User: `Deliver from docs/brainstorm/payment.md. Parallel reviewers: @Code Buddy`

1. Capability probe → v2 tools only → UUID `publication_token` → skeleton with
   `plan_target_rel_path=docs/superpowers/plans/payment.md` and Author node
   `plan|docs/superpowers/plans/payment.md|author|codex|none`.
2. Conditional Design review (A1 Design keys; platform outcomes; settle).
3. Dispatch Codex Plan Author with Design + `writing-plans` + matrix format +
   `b2d_task_risk_v1`. Author writes Plan with per-Task risk rows.
4. Estimated publish: platform digest, policies, routes,
   `reviewer_cohort_node_ids`, and full initial required set. Reviewers bind to
   the same platform Author evidence.
5. After an Author revision, reload state and dispatch only platform-selected
   Plan nodes on the current lineage. Material risk change opens the required
   full-group lineage.
6. Typed stagnation or requirements attention waits for durable resolution;
   then root re-entry loads the platform-selected Author/reviewer route.
7. Clean workspace gate → sequential Tasks: normal Tasks `grok`+`codex`; a
   migration Task high → `codex` implementer + `codex` and `grok` reviewers.
   Producer commits precede review; both reviewers cover the same platform
   `reviewed_task_id`/`artifact_digest`; fixes reopen both.
8. After all Task gates pass, finish aggregation and admit new Final Codex. A
   platform non-pass package routes to Final Fixer; a pass freezes the reviewed
   commit through local delivery/reporting. Recovery always starts with the
   compact index, referenced workspace reports, and bounded child/run lookups.
