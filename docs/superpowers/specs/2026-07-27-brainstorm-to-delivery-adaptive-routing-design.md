# Brainstorm-to-Delivery Adaptive Routing Design

Date: 2026-07-27

Status: Approved by the user on 2026-07-27.

## Summary

Optimize `brainstorm-to-delivery` around two independent decisions:

1. Plan work is always authored and revised by a dedicated Codex Plan Author,
   while Plan re-review narrows to reviewers who still own open high-severity
   findings.
2. Every implementation Task is classified before execution. Normal-risk Tasks
   retain Grok implementation plus independent Codex review. High-risk Tasks
   use Codex implementation plus independent Codex and Grok review, with both
   reviewers required to approve the same latest artifact.

The parent remains an orchestrator. It validates evidence, consolidates
findings, settles gates, and handles recovery, but does not author the Plan or
implement Task code.

The platform contract moves directly to manifest schema v2. There is no v1
parser, compatibility mode, dual-write period, or legacy fallback. This is
acceptable because the v1 workflow has not entered use.

## Context And Evidence

The session
`B2D会话writing-plans多轮并行审核耗时与Token优化分析` (`codeg://session/2070`)
showed two separate cost multipliers:

- Plan cost grows approximately with review rounds multiplied by the number of
  reviewers. The Taskbar Plan reached round 8, and the Viewer-Only Plan used
  roughly five rounds across three reviewers.
- Delivery time grows mainly in Task implementation, review, fix, and re-review
  loops. Adding stronger reviewers everywhere does not shorten weak authoring
  or implementation loops.

The current workflow contract reinforces those costs:

- `brainstorm-to-delivery` lets the parent invoke `writing-plans` and then sends
  every Plan revision back to the same complete review group.
- The manifest validator requires exactly one implementer and one reviewer for
  every Task.
- Task execution gates accept only one reviewer result.
- Recovery and Graph projection expose `pair_frozen`, which encodes the current
  two-node assumption.

The optimization therefore strengthens the producer at the point where work is
created, narrows repeated Plan review only when evidence permits it, and spends
dual-review cost only on Tasks classified as high risk before execution.

## Goals

- Make a dedicated Codex child the author and reviser of every B2D Plan.
- Classify every Task before its first implementation dispatch.
- Preserve an inexpensive route for normal Tasks.
- Give high-risk Tasks a stronger Codex implementer and two independent,
  cross-model reviewers.
- Reduce repeated Plan-review fan-out without weakening the initial review.
- Detect review stagnation deterministically and stop unbounded loops.
- Persist high-risk reasons so the policy can be audited and tuned later.
- Make manifest validation, admission, execution gates, recovery, and Graph
  projection agree on the same route.
- Reuse existing conversation usage, delegation runtime statistics, workflow
  revisions, and gate cycles to measure the result.

## Non-Goals

- No manifest v1 compatibility, conversion, fallback, or historical projection.
- No model selection below the Codeg agent type or configured profile.
- No dynamic Task promotion based only on how many review failures occurred.
- No voting or majority rule for document or code review.
- No parallel Task implementation; B2D Task execution remains sequential.
- No change to the existing final whole-branch review and Final fixer policy.
- No cost dashboard, new cost API, or cost-specific database feature.
- No changes to the generic `writing-plans` or
  `subagent-driven-development` Skills. B2D owns the orchestration policy.

## Selected Approach

Three approaches were considered:

1. Keep the full review group on every round. This preserves the current safety
   shape but retains the dominant Plan token multiplier.
2. Route by risk at Plan time and use finding-owned Plan re-review. This is the
   selected approach because it makes cost proportional to unresolved risk
   while preserving a full initial review and strict high-risk Task gates.
3. Start every Task on the normal route and promote it after failures. This
   reacts too late: a weak implementation has already consumed a review/fix
   cycle, and dangerous work initially receives the weaker route.

## Terms

**Plan Author** is the independent Codex child that invokes and follows
`writing-plans`, writes the Plan artifact, and performs all Plan revisions.

**Complete Plan review group** is the configured set of independent Plan
reviewer work units captured for the current Plan lineage.

**Finding owner** is a reviewer node responsible for re-checking a canonical
Plan finding that it raised. Deduplicated findings may have multiple owners.

**Task risk policy** is the versioned hard-trigger and soft-score policy used to
classify a Task before execution.

**Task cohort** is the complete set of implementation and review work units for
one Task. It contains two nodes for a normal Task and three nodes for a high-risk
Task.

**Latest artifact identity** is the pair `(producer task_id, artifact_digest)`.
Both values must match the newest terminal producer run covered by a review.

## End-To-End Workflow

```text
approved Design
  -> independent Codex Plan Author using writing-plans
  -> Plan with Task Routing Matrix
  -> full configured Plan review group
  -> parent adjudication and finding ledger
  -> scoped owner re-review, or full re-review after material change
  -> optional one-time holistic rewrite on stagnation
  -> approved manifest v2
  -> sequential Task cohorts selected by recorded risk
  -> existing final whole-branch review
  -> verification and reporting
```

No Task implementation may be admitted until its risk assessment, route, and
required reviewer set are present in the approved v2 manifest.

## Codex Plan Author Contract

Every B2D implementation Plan, regardless of size, is produced by a fresh
Codex Plan Author child.

The v2 skeleton predeclares the workspace-relative target Plan path and the
Codex Author node before Author dispatch. The Plan document reference and
digest are absent at that point. Author admission is therefore plan-driven and
durable rather than attached to the Graph after the file is written. The first
estimated revision after Author completion must use the same target path and add
the resulting Plan digest, Task policies, routes, and Plan review gate.

- The child must invoke and completely follow `writing-plans`.
- The parent supplies the approved Design, repository constraints, configured
  Plan reviewer group, risk policy version, and required routing-matrix format.
- The Author owns the Plan file and all revisions. The parent may reject invalid
  output or adjudicate review evidence, but it must not directly rewrite the
  Plan.
- Revisions continue the same Plan Author work unit and child conversation when
  recoverable. A legal same-role replacement follows the existing recovery
  budget and retains the work-unit identity.
- Plan reviewers use separate child conversations from the Author and from one
  another.
- Every Author completion records the Author run `task_id`, Plan digest, and
  workspace-relative report path. A Plan review is fresh only when it covers
  that exact `task_id` and digest.

Manifest node role `author` is added for this work unit. Its canonical key is:

```text
plan|{normalized-relative-plan-path}|author|codex|{profile-or-none}
```

Plan reviewer keys retain the corresponding `reviewer` role. The Author node
must exist before Plan reviewer admission and is visible in workflow recovery
and Graph projection.

## Adaptive Plan Review

### Initial round

The first review of a newly authored Plan always dispatches the complete
configured Plan review group. Reviewers inspect the same Author `task_id` and
Plan digest independently. The parent waits for every required reviewer,
deduplicates overlapping findings, verifies repository evidence, and records a
single adjudicated finding ledger.

### Stable findings

Every canonical finding contains:

- a stable `finding_id`;
- severity: `critical`, `important`, or `minor`;
- status: `open`, `resolved`, `new`, or `reopened`;
- one or more owner reviewer node IDs;
- first-seen and last-seen gate cycles;
- a bounded summary and evidence reference;
- the Plan digest on which the status was last evaluated.

Reviewers must reuse their finding IDs across continuations. The parent maps
duplicate reviewer findings to one canonical ID and retains all contributing
reviewers as owners. `new` and `reopened` findings count as open.

After the parent accepts a Critical or Important finding as valid, it may move
to `resolved` only when every current owner has returned fresh coverage of the
latest Author artifact and no owner keeps or reopens it. The parent may reject
an invalid finding during adjudication with recorded evidence, but it may not
silently close an accepted finding to reduce the count.

### Scoped re-review

After a localized Plan revision, the next required reviewer set is the union of
owners of all open Critical and Important findings. Reviewers without an open
high-severity finding are not resumed. A scoped reviewer may raise a genuinely
new finding; it receives a new stable ID and enters the ledger normally.

Minor-only findings do not keep the Plan gate open. They are fixed or retained
with an explicit rationale under the existing B2D policy.

### Full-group reset

The complete Plan review group is restored when a revision materially changes
any of the following:

- user-visible or system scope;
- a shared or public interface;
- Task decomposition or dependency ordering;
- any Task risk classification or implementation/reviewer route;
- data ownership, persistence, migration, security, concurrency, or lifecycle
  behavior; or
- configured Plan reviewer membership or profile identity.

The Plan Author labels the revision as localized or material and explains why.
The parent checks the diff and evidence; an ambiguous classification fails
closed to a full-group review.

The complete group is a durable `reviewer_cohort_node_ids` list on the Plan
gate. Each cycle's `required_reviewer_node_ids` is either that complete list or
the evidence-derived owner subset. This prevents recovery from confusing a
scoped cycle with the configured full group.

### Stagnation

For each completed Plan review round, let `C` and `I` be the counts of open
Critical and Important findings after canonical deduplication. A round has net
improvement only when:

1. `C` does not increase; and
2. `C + I` is lower than in the preceding completed round.

This prevents replacing several Important findings with a new Critical finding
from being treated as progress. New and reopened findings participate in the
same counts.

The first completed full review establishes the baseline and does not increment
the stagnation counter. Reviewer infrastructure failures and incomplete rounds
also do not increment it; they follow normal recovery or block the workflow.
After the one-time holistic rewrite, the counter returns to zero but the first
post-rewrite round is compared with the last completed pre-rewrite round, so a
rewrite cannot erase evidence of no progress. Only an explicit user-approved
requirements change starts a new Plan lineage and a new baseline; that reset and
its reason are persisted.

Two consecutive rounds without net improvement trigger exactly one holistic
Plan rewrite by the Plan Author. The rewrite must reconsider the Plan as a
whole rather than patch individual findings, and its next review uses the
complete group. If two consecutive non-improving rounds occur again after that
rewrite, the workflow blocks and asks the user to decide. It does not start a
second automatic rewrite.

The finding ledger, prior count pair, stagnation counter, and `rewrite_used`
flag are durable recovery state. Compaction cannot reset them.

## Task Risk Policy

Classification occurs in the Plan Author phase, before any Task implementation
is dispatched. The policy version for this design is
`b2d_task_risk_v1`.

### Hard triggers

Any hard trigger makes the Task `high`, regardless of its soft score:

| Signal | Trigger |
| --- | --- |
| `concurrency_lifecycle` | Threading, async coordination, cancellation, ordering, ownership lifetime, or process lifecycle behavior changes. |
| `security_trust_boundary` | Authentication, authorization, secrets, sandboxing, validation across a trust boundary, or privilege changes. |
| `migration_destructive_persistence` | Schema/data migration, deletion, irreversible persistence, or destructive state transition. |
| `public_compatibility` | Public API/protocol/schema compatibility, serialized formats, or externally consumed behavior changes. |
| `unsafe_ffi` | Rust `unsafe`, native FFI, ABI, memory ownership, or equivalent low-level boundary changes. |
| `update_rollback` | Installer, updater, rollback, recovery, or version-transition behavior changes. |

### Soft signals

When no hard trigger is present, sum each distinct active soft signal once:

| Signal | Score | Trigger |
| --- | ---: | --- |
| `cross_runtime_or_process` | 2 | Changes code or a contract across runtime or process boundaries. |
| `broad_production_surface` | 1 | Touches five or more production files; tests, docs, snapshots, and generated output do not count. |
| `multiple_ownership_modules` | 1 | Touches two or more independently owned modules or subsystems. |
| `shared_interface` | 1 | Changes an interface or contract consumed outside the Task's owning module. |
| `dependency_or_build` | 1 | Changes dependencies, lockfiles, build configuration, packaging, or deployment behavior. |
| `multi_layer_without_test_seam` | 1 | Spans two or more architectural layers and lacks an isolated test seam covering the boundary. |

A soft score of 3 or greater is `high`; 0 through 2 is `normal`.

The Author records exact file/module/interface evidence instead of only setting
booleans. The backend validates known signal names, weights, unique use, score
arithmetic, hard-trigger precedence, threshold selection, and non-empty
evidence. It cannot prove repository semantics, so the initial full Plan review
also reviews the classification evidence.

Missing, contradictory, or unversioned risk data makes the manifest invalid; it
does not silently default to `normal` or invent a route.

Before a Task cohort is admitted, new evidence may change its classification
only through a material Plan revision and full-group review. After any cohort
node is admitted, evidence that invalidates the recorded risk assessment or
route blocks that Task and escalates to the user. The workflow preserves the
frozen cohort and partial work; it does not dynamically swap the implementer,
append an unreviewed route, or continue under a known-wrong classification.

## Task Routing Matrix

The Plan contains one required row per Task with:

- Task index and title;
- planned production files and ownership modules;
- hard triggers and their evidence;
- soft signals, individual scores, and evidence;
- total soft score;
- final risk level and concise reason;
- implementer agent type;
- complete reviewer agent set;
- risk policy version.

The matrix is the human-reviewable source. The manifest carries the same data
as the machine-enforced source. A mismatch blocks Plan approval.

The backend does not parse Plan Markdown to discover or compare routes. The
Plan Author supplies the structured manifest data, and the initial reviewers
plus parent adjudication compare it with the visible matrix. Backend validation
is limited to the structured manifest's completeness and internal consistency.

High-risk reasons are never reduced to a bare score. The policy version, signal
breakdown, score, evidence, and reason are retained in the Plan, every immutable
manifest revision containing the Task, and agent-facing recovery state. This
supports later threshold and signal tuning from actual classifications.

## Task Routes And Gates

### Normal route

| Role | Required agent |
| --- | --- |
| Implementer and fixer | Grok |
| Independent reviewer | Codex |

The Task cohort contains one Grok implementer and one Codex reviewer. The gate
passes when the latest implementer result passes and the reviewer approves the
same latest artifact.

### High-risk route

| Role | Required agent |
| --- | --- |
| Implementer and fixer | Codex |
| Independent reviewer 1 | Codex |
| Independent reviewer 2 | Grok |

The two reviewers are independent of the implementer and of each other. The
Codex reviewer is a different child conversation from the Codex implementer.
Neither reviewer may reuse the Plan Author conversation.

Both reviewers start from the same review package and may run concurrently.
The parent waits for both, deduplicates their valid findings, and sends one
consolidated fix dispatch to the Codex implementer. Every new implementation or
fix artifact invalidates both earlier review results, including an earlier
approval. Both reviewers perform scoped re-review of the new artifact.

The execution gate is strict AND. It passes only when every required reviewer:

- has a validated terminal review summary;
- returns `approve` or `approve_with_minors`;
- records the latest implementer/fixer `task_id` as `reviewed_task_id`; and
- records the same non-empty artifact digest as the latest implementer/fixer.

A missing, failed, stale, or unavailable reviewer blocks the gate. The route is
never downgraded to save time or tokens.

### SDD specialization

B2D still invokes and follows `subagent-driven-development` for workspace,
brief, report, review-package, fix-loop, ledger, and final-review behavior. B2D
is the stricter agent-routing layer around that generic process:

- the risk route replaces generic model-selection discretion for Task agent
  types;
- a normal Task runs the generic review step once;
- a high-risk Task fans the same task brief, report, review package, and rubric
  out to both required reviewers, then joins them at one gate; and
- fix-round capability escalation may choose only a profile that remains legal
  under the frozen route. It may not change the required agent type or bypass a
  reviewer.

This specialization does not require editing the generic SDD Skill.

## Manifest Schema v2

Schema v2 extends the current graph document with explicit Plan authoring and
Task policy records. The conceptual v2 shape below omits unchanged fields:

```json
{
  "schema_version": 2,
  "plan_target_rel_path": "docs/superpowers/plans/example.md",
  "risk_policy_version": "b2d_task_risk_v1",
  "gates": [
    {
      "id": "plan-gate",
      "gate_kind": "plan",
      "reviewer_cohort_node_ids": ["plan-reviewer-codex"],
      "required_reviewer_node_ids": ["plan-reviewer-codex"]
    }
  ],
  "task_policies": [
    {
      "task_index": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": ["src/...", "src-tauri/..."]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": ["transport contract"]
          }
        ],
        "score": 3,
        "reason": "Changes a shared desktop/web transport boundary."
      },
      "route": {
        "implementer_node_id": "task-1-implementer",
        "reviewer_node_ids": [
          "task-1-reviewer-codex",
          "task-1-reviewer-grok"
        ]
      }
    }
  ]
}
```

The complete v2 contract also makes these changes:

- Add `author` to `ManifestNodeRole` and to key parsing/building.
- Require the normalized target Plan path and exactly one Codex Plan Author
  node before Author admission.
- Require the eventual Plan document path to equal the predeclared target.
- Preserve the complete Plan reviewer cohort separately from each cycle's
  required full or scoped reviewer set.
- Replace the exactly-one-Task-reviewer invariant with route-dependent cohort
  validation.
- Require one Task policy for every contiguous Task index in estimated and
  approved manifests.
- Keep skeleton manifests Task-policy-free until the Plan exists.
- Preserve immutable manifest revisions and CAS publication.
- Freeze the policy, implementer, and complete reviewer cohort once any node in
  that Task cohort is admitted.
- Rename `pair_frozen` to `cohort_frozen` in storage, agent recovery, internal
  models, errors, tests, and DTOs. The database migration preserves the boolean
  value but no dual field name remains at runtime.

Final-review nodes and routing keep their current contract.

## Validation And Admission

The validator enforces all structural rules before publication:

- schema version is exactly 2 and capability is `workflow_manifest_v2`;
- Plan target path, Author role, agent, key, eventual Plan path, and digest are
  consistent with the manifest lifecycle state;
- the Plan review cohort is complete and each cycle's required set is a valid
  full-group or finding-owner subset;
- Task indices, policies, nodes, routes, edges, and reviewer sets agree;
- hard triggers and soft-score arithmetic produce the declared risk level;
- normal Tasks have Grok implementation and one Codex reviewer;
- high Tasks have Codex implementation and exactly Codex plus Grok reviewers;
- work-unit keys and configured profiles are canonical and unique;
- no reviewer child conversation is also the Task producer, another required
  reviewer, or the Plan Author child conversation;
- a frozen cohort cannot drop or replace any route member or risk policy.

Admission repeats the relevant checks against the latest approved manifest and
actual child-conversation bindings. Manifest validity alone cannot establish
conversation independence because conversation IDs do not exist before
dispatch.

On first admission, every workflow work unit must bind a child conversation not
already bound to another Plan Author, Plan reviewer, Task implementer, Task
reviewer, Final fixer, or Final reviewer work unit in that workflow. Continue
runs reuse their own work unit; a legal replacement may change that work unit's
child conversation but still cannot reuse another work unit's conversation.

Typed errors distinguish actionable failures, including:

- `plan_author_mismatch`;
- `risk_assessment_invalid`;
- `task_route_mismatch`;
- `reviewer_set_mismatch`;
- `reviewer_not_independent`;
- `reviewed_task_stale`;
- `artifact_digest_mismatch`;
- `cohort_frozen`.

All failures are fail-closed. An absent or inconsistent v2 capability blocks
the structured B2D workflow; it does not enter v1 or legacy mode.

## Plan Review Gate Evidence

Plan gate cycles record more than aggregate finding counts. Each cycle stores:

- full or scoped review mode and the reason;
- required reviewer node IDs for that cycle;
- Author `task_id` and Plan digest covered by every reviewer;
- canonical finding updates and owner node IDs;
- open Critical, Important, and Minor counts;
- whether the round achieved net improvement;
- the consecutive-stagnation count and `rewrite_used` flag;
- parent adjudication summary and report paths.

Approval remains impossible while Critical or Important findings are open.
Scoped review cannot settle until all owner reviewers required for that cycle
have returned fresh evidence. A material revision cannot settle from a scoped
reviewer subset.

Task and Final execution gates remain projected rather than manually settled.
Task projection changes from one reviewer evidence item to a required reviewer
map and evaluates every item against the same producer identity.

## Structured Evidence Wire Contract

The existing root workflow tool names remain, but their schemas move as one
atomic capability set to `workflow_manifest_v2`:

- `get_workflow_capabilities` advertises only the complete v2 set;
- `publish_workflow_manifest` accepts only schema v2 documents;
- `settle_workflow_gate` accepts the Plan review scope, required reviewer set,
  covered Author identity, canonical finding updates, owner sets, and bounded
  report paths; and
- `get_workflow_state` returns the resulting v2 recovery state.

The server derives open counts, net improvement, consecutive stagnation, and
rewrite eligibility from the prior immutable cycle plus the submitted finding
updates. It does not trust parent-supplied aggregate booleans or counters.

Validated terminal card summaries gain the minimum role-specific evidence
needed by v2. A Plan Author summary includes status, Plan digest, and a
workspace-relative report path. A Plan or Task review summary includes verdict,
bounded counts, and a workspace-relative report path. Exact
`reviewed_task_id` and `artifact_digest` coverage remains in durable run
bindings populated against the admitted workflow node. Full finding detail
lives in the bounded report and the canonical gate-settlement payload rather
than in compact card text.

A missing or invalid required summary prevents the run from satisfying its
gate. All workspace-relative paths use the existing normalization and traversal
checks.

## Graph Projection

The Graph keeps one Task row but supports route-dependent fan-out:

```text
normal: implementer -> Codex reviewer --------> Task gate

high:   implementer -> Codex reviewer ----\
                   -> Grok reviewer -------+-> Task gate
```

Plan now displays the Author node before its review fan-out. Compact progress
uses returned/required reviewer counts. A high-risk Task exposes its level and
reason codes in node detail, while workspace paths and free-form evidence stay
out of the redacted frontend DTO.

No force-directed layout or new cost visualization is introduced. Existing
deterministic phase lanes, Task rows, session actions, and responsive behavior
remain in force.

## Persistence And Recovery

Immutable manifest revisions remain the authority for planned structure and
routing. Durable run bindings and gate-cycle evidence remain the authority for
actual work.

The v2 manifest JSON stores Task policies and routes. Run bindings store exact
producer/reviewer coverage and child-conversation association. Immutable Plan
gate-settlement rows are extended with bounded structured review scope, Author
coverage, finding-ledger, owner, report-path, and stagnation evidence. The node
binding column is migrated losslessly from `pair_frozen` to `cohort_frozen`.
These are workflow correctness records, not a cost-tracking schema.

`get_workflow_state` returns enough bounded agent-facing state to resume without
memory-based reconstruction:

- Plan Author node, latest task ID, Plan digest, and report path;
- target Plan path and complete Plan reviewer cohort;
- every Task policy, risk reason, signal evidence, score, and route;
- `cohort_frozen` state for every Task node;
- latest implementer/fixer identity and artifact digest;
- per-reviewer task ID, child-conversation identity, verdict, reviewed task ID,
  reviewed digest, and report path;
- Plan finding ledger, owner sets, review scope, stagnation counter, and rewrite
  state;
- manifest and graph revisions plus gate cycles.

Evidence remains bounded. If older completed evidence must be truncated under
the existing size class, current open findings, active Task cohort evidence,
latest producer/reviewer identities, and stagnation state are never dropped.

Compaction recovery always reads `get_workflow_state` plus the local B2D ledger
before dispatching. It must not recreate a Plan Author, reset findings, alter a
Task route, or treat one high-risk reviewer approval as gate completion.

## Skill Changes

`brainstorm-to-delivery/SKILL.md` will be revised to:

- dispatch Codex Plan Author before any Plan is written;
- require the Plan routing matrix and policy evidence;
- replace full-group Plan re-review loops with finding-owned scoped re-review;
- define material-change reset and stagnation escalation;
- dispatch normal and high-risk Task cohorts according to manifest v2;
- consolidate high-risk findings into one fix request and resume both reviewers
  after every new artifact;
- require exact producer task ID and digest evidence;
- recover all counters and evidence before continuing;
- remove v1, legacy, one-reviewer, and `pair_frozen` language.

The Skill continues to invoke `writing-plans` and
`subagent-driven-development`; it does not duplicate their generic procedural
instructions.

## Testing

### Unit and table tests

- Every hard trigger forces `high`.
- Soft scores 0 through 2 produce `normal`; score 3 and above produces `high`.
- Unknown, duplicated, contradictory, or evidence-free signals fail.
- Route generation and validation match each risk level.
- Plan Author is Codex and has a unique conversation.
- The Plan Author is admitted from a v2 skeleton before a Plan digest exists,
  and the completed Plan must retain the predeclared target path.
- Normal and high reviewer-set cardinality and agent types are exact.
- Every reviewer must cover the latest producer `task_id` and digest.
- Strict AND rejects missing, failed, stale, or mismatched high-risk reviews.
- `cohort_frozen` protects all two or three route nodes.
- Net-improvement and two-round stagnation boundaries are deterministic.
- Graph projection renders route-dependent reviewer fan-out and correct
  returned/required counts for both cohort sizes.

### Workflow scenarios

1. A normal Task runs Grok implementation plus Codex review and passes.
2. A hard-trigger Task runs Codex implementation plus Codex/Grok dual review
   and persists its reason.
3. A score-3 Task selects the same high-risk route without a hard trigger.
4. A localized Plan fix resumes only owners of open Critical/Important
   findings.
5. A scope, interface, decomposition, or route change restores the complete
   Plan review group.
6. Two non-improving rounds cause one holistic rewrite; a repeated pair after
   rewrite blocks for user input.
7. One high-risk reviewer approves and one requests changes; one consolidated
   fix is dispatched and both must review the new artifact.
8. Recovery preserves routes, reasons, reviewer evidence, finding owners,
   stagnation state, and report paths.
9. Any v1 document or partial capability set is rejected without fallback.
10. A pre-admission risk change forces full Plan review; a post-admission
    classification invalidation blocks without mutating the frozen cohort.

### Static Skill contract

Run the Skill validator and scan examples, quick-reference tables, prompts, and
recovery instructions. The test fails if any language allows the parent to
write the Plan, omits high-risk reasons, restores fixed full-group re-review,
permits one-reviewer high-risk completion, or refers to manifest v1.

## Measurement

Measurement uses existing records rather than a new product feature:

- parent and child conversation usage totals for tokens;
- delegation `started_at` and `finished_at` for child elapsed time;
- workflow timestamps for end-to-end elapsed time;
- node/run counts for Author, reviewer, implementation, fix, and rewrite calls;
- manifest revisions and gate cycles for Plan rounds;
- finding owners and required reviewer sets for scoped re-review fan-out;
- Task policies for normal/high distribution and recorded reasons.

Report these per workflow and grouped by role:

- total tokens and elapsed time;
- Plan review rounds and reviewer invocations;
- holistic rewrite count;
- Task implementation/fix rounds;
- Task reviewer invocations and high-risk dual-review overhead;
- scoped-review fan-out versus complete-group size;
- high-risk rate and signal distribution.

Session 2070 is the historical baseline. Evaluation also uses representative
normal, hard-trigger high, and score-trigger high scenarios, with at least three
runs per scenario and median comparisons to reduce model variance. Performance
measurements are diagnostic, not runtime safety gates.

The deterministic structural success condition is that a localized revision
with one open finding owner dispatches one reviewer rather than the whole group.
Normal Tasks retain one reviewer; high-risk Tasks deliberately retain two.
Safety and route assertions must pass before token or time reductions are
considered successful.

## Acceptance Criteria

- Every B2D Plan and revision comes from an independent Codex Plan Author using
  `writing-plans`.
- Initial Plan review uses the complete configured group.
- Localized re-review uses only owners of open Critical/Important findings.
- Material changes restore the complete group.
- Stable finding IDs and durable stagnation state prevent false convergence or
  counter resets.
- Every Task has a valid versioned risk assessment before execution.
- Every high-risk reason is present in the Plan, manifest, and recovery state.
- Normal and high Task routes exactly match the approved role matrix.
- High-risk completion requires independent Codex and Grok approvals of the
  same latest Codex implementation artifact.
- Validator, admission, execution gate, Graph, and recovery agree on the same
  reviewer cohort.
- Manifest schema is v2 only, with no v1 or legacy fallback.
- Existing final review behavior remains unchanged.
- Tests cover rule boundaries, workflow scenarios, recovery, and prohibited
  Skill language.
- Existing statistics can quantify Plan fan-out, Task review overhead, tokens,
  and elapsed time without a new cost feature.
