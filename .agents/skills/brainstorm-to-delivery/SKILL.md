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

| Work unit | Agent | First dispatch | Continue |
| --- | --- | --- | --- |
| Plan Author | Codex (`agent_type: "codex"`) | Fresh Author work unit | Same Author for revisions / one holistic rewrite |
| Plan reviewer (each) | Configured doc group (always includes Codex) | Independent child | Same reviewer for re-review |
| Task implementer/fixer (normal) | Grok | New per Task | Same implementer for fix rounds |
| Task implementer/fixer (high) | Codex | New per Task | Same implementer for fix rounds |
| Task reviewer (normal) | Independent Codex | New per Task | Same reviewer |
| Task reviewers (high) | Independent Codex **and** independent Grok | New per Task; neither reuses Author or implementer | Both re-review after every fix |
| Final whole-branch reviewer | Codex | Always new | That Final thread only |
| Final fixer | Grok | Only after non-pass Final | Never reuse a Task implementer |

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

| Unit | Materials |
| --- | --- |
| Design reviewer | `design\|{rel}\|reviewer\|{agent}\|{profile\|none}` |
| Plan Author | `plan\|{rel}\|author\|codex\|{profile\|none}` |
| Plan reviewer | `plan\|{rel}\|reviewer\|{agent}\|{profile\|none}` |
| Task implementer | `task\|{n}\|implementer\|{agent}\|{profile\|none}` |
| Task reviewer | `task\|{n}\|reviewer\|{agent}\|{profile\|none}` |
| Final reviewer | `final_review\|reviewer\|{agent}\|{profile\|none}` |
| Final fixer | `final_review\|fixer\|{agent}\|{profile\|none}` |

Prefer `continue_delegation` when the ledger shows a recoverable thread. Same
key cold `delegate_to_agent` without `replaces_task_id` is invalid once lineage
exists. Replacement only for `unresumable` | `budget_exhausted_continue` |
`not_supported`, same key/role/profile, at most one replacement and two
unexpected continues per work unit. Recovery is a **new turn**: re-inspect
disk, treat prior reasoning as provisional, rebuild missing reports, re-run
covering tests before claiming done.

## Workflow capability (v2 only)

Before first Design dispatch, call `get_workflow_capabilities`. Require the
full v2 tool set (`get_workflow_capabilities`, `get_workflow_state`,
`publish_workflow_manifest`, `settle_workflow_gate`). Missing or inconsistent
tools → hard block. **Do not use legacy capability mode or any pre-v2 manifest
schema.**

Publish with schema v2 only (`workflow_kind=brainstorm_to_delivery`), required
`plan_target_rel_path`, `risk_policy_version: "b2d_task_risk_v1"`, Task
policies/routes, Plan `reviewer_cohort_node_ids`, and Author node. Use a ledger
`publication_token` (UUID) for skeleton create; same digest retries reuse it;
digest mismatch → hard stop + `get_workflow_state`. CAS updates carry
`workflow_id` + `expected_manifest_revision`.

Lifecycle: skeleton (target path + Author work unit, no Plan digest yet) →
estimated (digest, matrix, policies, routes, full initial required set) →
parent `settle_workflow_gate` → approved. Material Plan change demotes to
estimated and reopens Plan review. On recovery, read `get_workflow_state` +
ledger; never invent a second active workflow from memory.

Every Design/Plan/Task/Final child must emit one validated terminal
`<!-- codeg-card-summary-v1 ... -->` block. Parent advances only on platform-
validated summaries. Review pass set: `approve` | `approve_with_minors`.
Implementation pass set: `done` | `done_with_concerns`.

Respect platform bounds (Tasks≤100, nodes≤400, edges≤800, gates≤50, adj≤4KiB,
JSON≤512KiB). Details of wire fields live in tools/validation—do not restate
full schemas here.

## 1. Conditional Design review

Self-check the Brainstorm. External Design review when: cross-module/large
surface, migration, concurrency, security, real ambiguity, or high-risk design
without independent evidence. Otherwise v2 still uses Design gate
`resolution_mode=self_review` with empty `required_reviewer_node_ids` plus
design rel_path/digest, then explicit settle.

With external Design reviewers: `parent_adjudication` and full required set.
Clear Critical/Important via owner re-review. Pause for user if fixes change
requirements, scope, architecture, or user data handling.

## 2. Plan production

1. Publish a v2 skeleton with the target Plan path and a **fresh Codex Plan
   Author** work unit (`plan|…|author|codex|…`).
2. Dispatch that Author. Require complete `writing-plans` behavior. Parent
   supplies Design, constraints, configured Plan reviewer group, policy version
   `b2d_task_risk_v1`, and Task Routing Matrix format. **Author owns the Plan
   file and all revisions. Parent must not write or rewrite the Plan**; parent
   may only reject invalid output and adjudicate evidence.
3. Publish estimated: Plan digest, Task Routing Matrix, task policies/routes,
   complete Plan `reviewer_cohort_node_ids`, and full initial
   `required_reviewer_node_ids`. Author records `task_id`, digest, and report
   path. Plan reviewers are separate children from the Author and each other.
4. **Initial Plan review** always uses the complete configured cohort.
5. **Scoped re-review (localized revision):** next required set = union of
   owners of open Critical and Important findings only. Reviewers without an
   open high-severity finding are not resumed. Minors do not keep the gate open
   (fix or retain with rationale).
6. **Full-group reset** when a revision materially changes scope, public/
   shared interface, Task decomposition/order, **any Task risk or route**,
   data/persistence/migration/security/concurrency/lifecycle, or Plan reviewer
   membership/profile. Ambiguous classification → full group. Author labels
   localized vs material; parent verifies the diff.
7. **Stagnation:** after each completed round, net improvement requires
   non-increasing Critical count and lower Critical+Important vs prior
   completed round. Baseline first full round does not increment. Two
   non-improving rounds → exactly one **holistic rewrite** by the Author
   (whole Plan, then full-group review). After that rewrite, if two non-
   improving rounds occur again → **block and ask the user** (no second
   automatic rewrite). Only a **user-approved requirements change** with a
   **persisted reason** resets Plan lineage and baseline.
8. Persist finding ledger, prior counts, stagnation counter, and
   `rewrite_used` in recovery state. Compaction must not erase them.

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
  and the same non-empty `artifact_digest`.
- Missing, failed, stale, or unavailable reviewer → **block**. Never
  downgrade high→normal to save time/tokens. High never ships on a single
  approval.
- Serial Task implementation only. Match A1 keys to approved manifest nodes.
- Canceling a frozen cohort: finish its gate or publish blocked/canceled
  **retaining bindings**—never silent drop; never “stop talking” as cancel.

## 5. Workspace gate (before Task execution)

When the approved Plan is about to execute (again after material re-approval):
inspect `git status` and full unstaged/staged diffs. Few clear non-overlapping
edits may continue with evidence. Many, overlapping, or unknown → pause for
user. Never stash, commit, overwrite, or discard user work unasked.

## 6. Execute with SDD specialization

Run full `subagent-driven-development`. B2D only overrides agent routing:

- Risk route replaces generic model discretion for Task agent types.
- Normal: one Grok implementer + one Codex reviewer.
- High: Codex implementer + Codex and Grok reviewers; join at one gate.
- Fix-round profile escalation may only pick a profile still legal under the
  frozen route; it may not change agent type or drop a reviewer.
- Final whole-branch review remains a new Codex child after all active Task
  gates pass; Final fixer is Grok on non-pass Final only.

## 7. Verify, commit, report

Per-Task targeted checks; final scope-appropriate test/lint/build. Re-run after
fixes. Stage only owned changes; local commits only—no merge/push/PR. Final
report: results, diffs, commands, reviews, retained Minors/risks, commits,
worktree, blockers.

## Quick reference under pressure

| Pressure | Required action |
| --- | --- |
| “Finish Plan review fast” after localized Important fix | Dispatch **owners of open Critical/Important only**, not full group |
| “Migration is mechanical—use cheap Grok” | Hard triggers → **high**: Codex implementer + Codex **and** Grok reviewers |
| High Task: one reviewer approved, other unavailable | **Block**; never pass or downgrade |
| Two stagnant Plan rounds | One holistic Author rewrite + full group; second stagnant pair → user |
| “Parent will tweak the Plan / Task code” | Forbidden. Author owns Plan; routed implementers own code |
| Pre-admission risk looks wrong | Material Plan revision + full-group review |
| Post-admission risk looks wrong | Block + escalate; do not mutate `cohort_frozen` |
| Urgency / small Task | Still Author + Plan review + risk route + SDD |
| Agent unavailable | Hard block; no agent substitution |

## Rationalizations

| Excuse | Reality |
| --- | --- |
| “Full group every Plan revision is safer.” | Initial + material resets use full cohort; localized open-finding re-review is owner-scoped by design. |
| “One Codex approval is enough on high.” | High is strict AND on both reviewers covering the same latest artifact. |
| “Grok is fine for migrations/updaters.” | Those hard triggers force high + Codex implementer. |
| “Skip the missing reviewer to ship.” | Unavailable required reviewer blocks the gate. |
| “I’ll rewrite the Plan in the parent.” | Parent must not write the Plan; continue the Author. |
| “Change the route after admission.” | `cohort_frozen`; post-admission invalidation blocks, never mutates. |
| “Second holistic rewrite will unstick us.” | Only one automatic rewrite; then user decision. |
| “Reset stagnation after compaction.” | Ledger + platform state are durable; only user-approved requirements change with persisted reason resets lineage. |
| “Direct parent implement is faster.” | Parent only orchestrates. |
| “`spawn_agent` can’t pick Grok.” | Use Codeg `delegate_to_agent`. |

## Red flags — stop

- Parent writing Plan or Task code
- Pre-v2 manifest schema, legacy capability mode, or old pair-freeze wording
- Full Plan group forced for purely localized open-finding re-review
- High Task shipping on a single approval or a downgraded route
- Grok implementer on hard-trigger Tasks
- Gate pass without matching `reviewed_task_id` + non-empty `artifact_digest`
- Silent cohort drop or conversation-stop as cancel

## End-to-end example

User: `Deliver from docs/brainstorm/payment.md. Parallel reviewers: @Code Buddy`

1. Capability probe → v2 tools only → UUID `publication_token` → skeleton with
   `plan_target_rel_path=docs/superpowers/plans/payment.md` and Author node
   `plan|docs/superpowers/plans/payment.md|author|codex|none`.
2. Conditional Design review (A1 Design keys; card summaries; settle).
3. Dispatch Codex Plan Author with Design + `writing-plans` + matrix format +
   `b2d_task_risk_v1`. Author writes Plan with per-Task risk rows.
4. Estimated publish: digest, policies, routes, `reviewer_cohort_node_ids`,
   full initial required set. Full cohort reviews same Author `task_id`+digest.
5. One Important remains after localized fix → continue **only** that finding’s
   owners. Material risk change → full cohort again.
6. Two non-improving rounds → one holistic Author rewrite; full cohort;
   second stagnant pair would block for the user.
7. Workspace gate → sequential Tasks: normal Tasks `grok`+`codex`; a migration
   Task high → `codex` implementer + `codex` and `grok` reviewers; both must
   approve same `reviewed_task_id`/`artifact_digest`; fixes re-open both.
8. All Task gates pass → new Final Codex; on request_changes → Final fixer
   Grok then continue Final. Local commits + final report. Recovery always
   `get_workflow_state` + ledger first.
