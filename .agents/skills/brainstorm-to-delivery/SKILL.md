---
name: brainstorm-to-delivery
description: Use when a Codeg conversation has an approved or completed Brainstorm artifact and needs the work carried through to a high-quality local delivery.
---

# Brainstorm to Delivery

Coordinate delivery through Simple Markdown documents and generic delegation.
Keep requirement, scope, architecture, and user-data decisions with the user.
Keep the parent focused on coordination, adjudication, progress, and delivery.

<!-- codeg-b2d-skill-contract-v2
{
  "schema_version": 2,
  "phase_order": [
    "establish-current-truth",
    "resolve-task-agent",
    "review-and-revise-design",
    "author-and-review-plan",
    "maintain-progress",
    "apply-workspace-gate",
    "execute-tasks-serially",
    "recover-generic-runs",
    "complete-final-review"
  ],
  "interfaces": {
    "plan_authoring": "writing-plans",
    "task_execution": "subagent-driven-development",
    "registration": "register_simple_workflow",
    "first_run": "delegate_to_agent",
    "later_run": "continue_delegation",
    "join": "get_delegation_status",
    "recovery_authorization": "request_recovery_authorization"
  },
  "plan_setup_order": [
    "create-progress",
    "dispatch-plan-author",
    "confirm-plan-on-disk",
    "validate-routing",
    "review-plan",
    "register-simple-workflow",
    "sync-plan-tasks"
  ],
  "document_work": {
    "parent_edits": false,
    "design_review": "conditional",
    "design_reviewer": "independent_codex",
    "design_fixer": "independent_codex",
    "plan_author": "independent_codex",
    "plan_reviewer": "independent_codex",
    "producer_reviewer_independence": true,
    "plan_rereview": "full_latest_plan",
    "user_named_reviewers": "design_and_plan_only"
  },
  "conversation_identity": {
    "distinct_work_units": "distinct_child_conversations",
    "continuation": "same_work_unit_only"
  },
  "task_agent": {
    "default_agent_type": "grok",
    "selection_source": "invocation",
    "explicit_substitution": "forbidden",
    "change_boundary": "completed_tasks_after_plan_revision_and_full_rereview"
  },
  "routing": {
    "marker": "codeg-b2d-routing-v1",
    "risk_policy_version": "b2d_task_risk_v1",
    "normal": {
      "implementer": "task_agent",
      "reviewers": ["codex_primary"]
    },
    "high": {
      "implementer": "codex",
      "reviewers": ["codex_primary", "task_agent_auxiliary"]
    },
    "reviewer_slots": ["primary", "auxiliary"],
    "task_order": "serial",
    "high_review_fan_out": "parallel_after_implementation"
  },
  "progress": {
    "marker": "codeg-simple-progress-v1",
    "mutation_order": [
      "record-reserving-intent",
      "delegate",
      "record-admission",
      "record-observed-state"
    ],
    "route_metadata": "additive"
  },
  "workspace_policy": "preserve-user-changes",
  "recovery": {
    "unexpected_continuations": 2,
    "logical_replacements": 1,
    "replacement_retry": "pre-admission-only"
  },
  "final_review": {
    "required": true,
    "independent": true,
    "reviewer": "codex",
    "fix_owner": "task_producer"
  }
}
-->

## 1. Establish current truth

Read the invocation, Brainstorm, repository instructions, current Plan and
progress when present, Task reports, reviews, commits, and worktree state.
Inspect live Agent discovery and the schemas for register_simple_workflow,
delegate_to_agent, continue_delegation, get_delegation_status, and
request_recovery_authorization. Refresh discovery after compaction or stale
tool errors.

Treat simulated Agent responses only as explicitly labeled workflow test
doubles. Use live generic delegation for real work. Preserve user files and
decisions. Assign all Design, Plan, implementation, fix, and review writing to
child work units; keep the parent in the coordinator role.

## 2. Resolve the Task Agent

Resolve one Task Agent identity from the invocation before document work.
Record an omitted selection as generation 1 with agent_type grok and a null
profile. Validate an explicit built-in or custom Agent and profile against live
discovery. Block an invalid, reserved, ambiguous, or unavailable selection and
request an explicit user choice before recording a different identity.

Keep generations contiguous from 1 and set effective_from_task_index to the
first referenced pending Task. Apply a change only after the completed Task
prefix, while every remaining Task is pending with an empty runs list, after
Plan Author revision and full Plan re-review. Defer an active-Task change and
request a user decision while preserving its admitted route.

## 3. Review and revise Design

Trigger Design review when the Brainstorm spans modules, migration,
concurrency, security, persistence, externally visible compatibility, or
material ambiguity. Always dispatch an independent Codex Design Reviewer when
any trigger is present. Dispatch every user-named Design Reviewer as an
additional separate document-only work unit. Never let a user-named reviewer
replace the Codex Design Reviewer. Use
design|DESIGN_PATH|reviewer|AGENT|PROFILE for each reviewer. Dispatch an
independent Codex Design Fixer on design|DESIGN_PATH|fixer|codex|none.

Adjudicate findings against current artifacts. Continue the same Design Fixer
for revisions and continue each separate reviewer for re-review. Request a
user decision for requirement, scope, architecture, or user-data changes.
Require covering Design reviews to approve the same latest Design. Keep
user-named Design and Plan reviewers within document review roles.

### Operational policy JSON

Apply this exact inline policy. Treat each hard or soft array entry as an
evidence object, count each distinct active soft signal once, and reject every
condition named by `invalid`.

Emit an active hard trigger as `{"kind":"...","evidence":["..."]}`. Emit an
active soft signal as
`{"kind":"...","score":N,"evidence":["..."]}` with its fixed score. Use an
empty signal array when that class is inactive; reject bare names and empty
evidence arrays.

```json
{
  "design_review_triggers": [
    "spans_modules",
    "migration",
    "concurrency",
    "security",
    "persistence",
    "externally_visible_compatibility",
    "material_ambiguity"
  ],
  "risk_policy": {
    "version": "b2d_task_risk_v1",
    "hard_triggers": [
      { "kind": "concurrency_lifecycle", "trigger": "Threading, async coordination, cancellation, ordering, ownership lifetime, or process lifecycle behavior changes" },
      { "kind": "security_trust_boundary", "trigger": "Authentication, authorization, secrets, sandboxing, trust-boundary validation, or privilege changes" },
      { "kind": "migration_destructive_persistence", "trigger": "Schema/data migration, deletion, irreversible persistence, or destructive state transitions" },
      { "kind": "public_compatibility", "trigger": "Public API, protocol, schema, serialized format, or externally consumed behavior changes" },
      { "kind": "unsafe_ffi", "trigger": "Rust unsafe, native FFI, ABI, memory ownership, or equivalent low-level boundaries" },
      { "kind": "update_rollback", "trigger": "Installer, updater, rollback, recovery, or version-transition behavior changes" }
    ],
    "soft_signals": [
      { "kind": "cross_runtime_or_process", "score": 2, "trigger": "Changes code or a contract across runtime or process boundaries" },
      { "kind": "broad_production_surface", "score": 1, "trigger": "Touches at least five production files, excluding tests, docs, snapshots, and generated output" },
      { "kind": "multiple_ownership_modules", "score": 1, "trigger": "Touches at least two independently owned modules or subsystems" },
      { "kind": "shared_interface", "score": 1, "trigger": "Changes an interface or contract consumed outside the owning module" },
      { "kind": "dependency_or_build", "score": 1, "trigger": "Changes dependencies, lockfiles, build configuration, packaging, or deployment" },
      { "kind": "multi_layer_without_test_seam", "score": 1, "trigger": "Spans at least two architectural layers without an isolated boundary test seam" }
    ],
    "evidence_fields": {
      "hard_trigger": ["kind", "evidence"],
      "soft_signal": ["kind", "score", "evidence"],
      "evidence": "non-empty file, module, or interface facts"
    },
    "arithmetic": {
      "distinct_active_signal_count": 1,
      "any_hard_trigger_level": "high",
      "normal_soft_score_range": [0, 2],
      "high_soft_score_minimum": 3,
      "invalid": "unknown, duplicate, contradictory, incorrect, or evidence-free"
    }
  },
  "byte_limits": {
    "plan_document": 2097152,
    "routing_block": 262144,
    "progress_document": 524288,
    "progress_block": 65536
  }
}
```

Keep the Plan document at or below 2 MiB and its routing block at or below 256
KiB. Keep the progress document at or below 512 KiB and its structured block
at or below 64 KiB.

## 4. Author and review Plan

Create progress first with one bounded codeg-simple-progress-v1 block.
Dispatch an independent Codex Plan Author with writing-plans on
plan|PLAN_PATH|author|codex|none. Require ordered Task headings and exactly one
bounded unfenced codeg-b2d-routing-v1 JSON block.

Validate schema version 1, b2d_task_risk_v1, Agent generations, every Task
risk, every exact route, and heading alignment. Confirm the Plan on disk.
Dispatch an independent Codex Plan Reviewer on
plan|PLAN_PATH|reviewer|codex|none plus any user-named Plan reviewers. Review
the complete latest Plan rather than a diff.

Route accepted findings to the same Plan Author, validate the rewritten Plan,
and continue every separate Plan reviewer for full re-review. After approval,
call register_simple_workflow and sync ordered Plan Tasks into progress.

Preserve an archived legacy Simple run on its recorded route. Before the next
pending Task adopts adaptive routing, route a complete routing block through
Plan Author revision, deterministic validation, and full Plan re-review.

### Plan routing JSON

Emit this complete JSON shape inside the single routing marker. Repeat the
generation and Task entries as needed, and keep Task order identical to Plan
headings.

```json
{
  "schema_version": 1,
  "risk_policy_version": "b2d_task_risk_v1",
  "task_agent_generations": [
    {
      "generation": 1,
      "agent_type": "grok",
      "profile_id": null,
      "effective_from_task_index": 1
    }
  ],
  "tasks": [
    {
      "index": 1,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [],
        "soft_signals": [
          { "kind": "cross_runtime_or_process", "score": 2, "evidence": ["src/lib/transport", "src-tauri/src/web"] },
          { "kind": "shared_interface", "score": 1, "evidence": ["transport request contract"] }
        ],
        "score": 3,
        "reason": "Changes a shared desktop/server transport boundary."
      },
      "route": {
        "implementer": { "agent_type": "codex", "profile_id": null },
        "reviewers": [
          { "slot": "primary", "agent_type": "codex", "profile_id": null },
          { "slot": "auxiliary", "agent_type": "grok", "profile_id": null }
        ]
      }
    }
  ]
}
```

## 5. Maintain progress

Keep Plan Task indices, risk level, Task Agent generation, expected work-unit
keys, status, commit, and runs synchronized in one progress block. Derive
normal implementer keys as task|N|implementer|TASK_AGENT|PROFILE and high
implementer keys as task|N|implementer|codex|none. Derive primary reviewer keys
as task|N|reviewer|primary|codex|none and high auxiliary reviewer keys as
task|N|reviewer|auxiliary|TASK_AGENT|PROFILE. Use
final_review|reviewer|codex|none for final review.

Use the key token none only for a null profile. Emit explicit six-part primary
and auxiliary reviewer keys for routed Tasks. Read a legacy five-part Task
reviewer key only as a legacy primary lineage.

Before each call, record reserving intent with the exact Agent, profile, role,
and key. Call generic delegation. After admission, record task and child
conversation IDs. After each observation, record the latest state. Keep
task_id globally unique and attach one non-null child conversation to only one
complete work-unit key.

### Progress JSON

Maintain this complete JSON shape inside the single progress marker. Repeat
Task and run entries without dropping route or lineage fields.

```json
{
  "schema_version": 1,
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "active_task_index": 1,
  "tasks": [
    {
      "index": 1,
      "status": "in_progress",
      "commit": null,
      "risk_level": "high",
      "task_agent_generation": 1,
      "expected_work_unit_keys": {
        "implementer": "task|1|implementer|codex|none",
        "reviewers": {
          "primary": "task|1|reviewer|primary|codex|none",
          "auxiliary": "task|1|reviewer|auxiliary|grok|none"
        }
      },
      "runs": [
        {
          "role": "implementer",
          "agent_type": "codex",
          "profile_id": null,
          "task_id": null,
          "child_conversation_id": null,
          "state": "reserving",
          "work_unit_key": "task|1|implementer|codex|none",
          "recovery_count": 0,
          "replaced_task_id": null,
          "replacement_reason": null
        }
      ]
    }
  ],
  "final_review_status": "pending",
  "updated_at": "2026-08-16T00:00:00Z"
}
```

## 6. Apply the workspace gate

Inspect git status, staged diff, unstaged diff, recent commits, ignored delivery
reports, and repository instructions before each producer dispatch. Record
ownership and expected files. Preserve unrelated user changes, build outputs,
generated files, and concurrent edits.

Require every producer to inspect disk state before editing, stay within
assigned files, use test-first development, and report exact tests and diffs.
Pause on ambiguous ownership, destructive operations, secrets, external side
effects, or user-owned decisions. Request direction and resume from refreshed
repository truth.

## 7. Execute Tasks serially

Use subagent-driven-development and execute one Plan Task at a time. Before
every dispatch, validate the Skill, b2d_task_risk_v1, Plan routing, progress,
and agreement. Route changed pre-admission risk evidence through the same Plan
Author, rerun validation, and continue every Plan reviewer for full re-review.
Block changed post-admission evidence and request a user decision while
preserving the active route.

For a normal Task, dispatch the selected Task Agent as implementer and fixer;
after it settles, dispatch an independent Codex primary reviewer. For a high
Task, dispatch an independent Codex implementer and fixer; after it settles,
fan out an independent Codex primary reviewer and the selected Task Agent
auxiliary reviewer. Keep high reviewers on distinct keys and child
conversations even when all route identities are Codex.

Join every required Task run. Continue the owning producer for each accepted
fix, invalidate all prior review conclusions for that Task, and rerun every
required reviewer on the latest producer result. Complete a Task only after
all expected key lineages end completed, checks pass, reviewers approve, and
the owned commit and report are current. Start the next Task after settlement.

## 8. Recover generic runs

Continue a run with continue_delegation only on its stable key and child
conversation. Join observed tasks through get_delegation_status. For recovery
confirmation, call request_recovery_authorization and replay the authorized
generic call.

Handle pre-admission retry within its existing rail. For a supported terminal
recovery reason, record replacement intent and use a fresh delegate_to_agent
with the original Agent, profile, key, replaces_task_id, and exact reason:
unresumable, budget_exhausted_continue, not_supported, admission_failed, or
admission_unknown.

Preserve at most two unexpected continuations and one logical replacement per
complete key lineage. Block exhausted recovery, unresolved admission, and
unavailable required identities. Surface the typed blocker and retain the
recorded route; request a user decision for any later route change.

## 9. Complete final review

After all Tasks complete, re-read the Brainstorm, Design, Plan, routing,
progress, reports, reviews, commits, full branch diff, and worktree state. Run
all covering tests, lint, build, and project checks.

Dispatch a fresh independent Codex final reviewer on
final_review|reviewer|codex|none. Adjudicate findings against current truth.
Route each accepted finding to the Task producer that owns the affected code:
the selected Task Agent for a normal Task or the Codex implementer for a high
Task. Continue that producer, rerun every reviewer required by its Task route,
rerun covering checks, and continue the same final reviewer on the new state.

Complete local delivery only when the latest repository state passes covering
checks and independent final review. Commit only owned changes locally. Leave
merge, push, PR creation, and deployment to a separate explicit request.
Report commits, exact commands and outcomes, review conclusions, retained
Minors, worktree state, and blockers.
