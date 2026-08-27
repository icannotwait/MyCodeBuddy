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
    "recovery_authorization": "request_recovery_authorization",
    "binding_query": "get_delegation_orchestration_bindings"
  },
  "plan_setup_order": [
    "create-progress-shell",
    "dispatch-plan-author",
    "derive-plan-routing",
    "initialize-progress-from-validator",
    "validate-static-documents",
    "validate-durable-admission",
    "review-plan",
    "register-simple-workflow"
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
    "user_named_reviewers": "design_and_plan_only",
    "admission_cadence": "fresh_applicable_mode_before_every_dispatch_or_continuation"
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
    "high_review_fan_out": "parallel_after_implementation",
    "binding_schema_version": 1,
    "binding_namespace": "brainstorm-to-delivery",
    "binding_source": "validator_output"
  },
  "progress": {
    "marker": "codeg-simple-progress-v1",
    "mutation_order": [
      "record-reserving-intent",
      "delegate",
      "record-admission",
      "record-observed-state"
    ],
    "route_metadata": "additive",
    "dispatch_intent": "operation_specific_before_call",
    "pending_route_change": "record_before_plan_revision_clear_after_approval"
  },
  "workspace_policy": "preserve-user-changes",
  "recovery": {
    "unexpected_continuations": 2,
    "logical_replacements": 1,
    "replacement_retry": "pre-admission-only",
    "durable_reconciliation": "fresh_complete_parent_scoped_snapshot",
    "lost_acknowledgement": "one_exact_unresolved_intent",
    "status_refresh": "state_only_then_fresh_full_admission"
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
Read the invocation, Brainstorm, repository instructions, current Plan and progress when present, Task reports, reviews, commits, and worktree state.
Inspect live Agent discovery and the schemas for get_delegation_orchestration_bindings, register_simple_workflow,
delegate_to_agent, continue_delegation, get_delegation_status, and request_recovery_authorization. Require the query tool.
For every complete snapshot, run this procedure before the applicable admission mode:

1. Inspect the live binding-query schema.
2. When ticket-v1 is advertised, the complete-snapshot procedure follows section 5's write-ahead UUID and bounded fingerprint steps through the exact `admission_intent` artifact request. For legacy artifact mode, when it advertises artifact delivery, request `delivery: "artifact"` once. Pass `artifact_path` directly to `--durable-evidence` and `artifact_sha256` directly to `--durable-evidence-sha256` in the validator invocation.
3. Never open, read, print, copy, summarize, embed, or delegate inspection of the artifact. Delete the exact artifact after the validator exits.
4. Treat artifact request, descriptor, digest, stale, validator, and cleanup errors as blocking and never fall back to pages.
5. Use legacy page pagination only when the live schema does not advertise artifact delivery. In legacy mode, collect one complete parent-scoped snapshot in an OS-temporary evidence file, restart stale, mixed, truncated, or oversized pagination from page one, invoke the validator without a digest, and remove the file.

Refresh discovery after compaction. Treat simulated Agent responses only as labeled test doubles. Preserve user files.
Assign Design, Plan, implementation, fix, and review writing to child work units; keep the parent in the coordinator role.
Query unavailability, DB failure, incomplete evidence, a missing query tool, wrong namespace, an unbound routed row,
a deleted mirror, ambiguous adoption, an unavailable Agent or profile, or an exhausted rail blocks.
Copy the validator's exact binding. Independent parent or Plan Author hashing is forbidden.
get_delegation_status remains the join tool after admission.

## 2. Resolve the Task Agent
Resolve one Task Agent identity from the invocation before document work.
Record an omitted selection as generation 1 with agent_type grok and a null
profile. Validate an explicit built-in or custom Agent and profile against live
discovery. Block an invalid, reserved, ambiguous, or unavailable selection and
request an explicit user choice before recording a different identity.

Keep generations contiguous from 1. For a boundary change, run this exact
ordered mutation and recovery sequence:

1. Run fresh full admission against the unchanged Plan/progress with the complete-snapshot procedure.
2. Prove every affected Task is pending with an empty run list and no selected durable row in any status.
3. Persist `pending_route_change` with the requested Agent/profile, next generation, first affected index, and complete affected suffix.
4. Confirm that exact Agent/profile is currently available; keep the intent and block on unavailability rather than substituting.
5. Continue the same Plan Author to append the generation and rewrite only that suffix.
6. Run Author then parent Plan-only derivation, resynchronize only those entries from the parent's exact output, pass combined static validation and fresh full admission with the procedure, then continue every Plan Reviewer for complete re-review.
7. After approval, run the procedure for full admission, clear `pending_route_change` to null, persist, and run it again before the next Task.
8. At any interruption, retain the intent, re-read Plan/progress/reviews, run the procedure, and identify the checkpoint. Resume synchronized states with full admission; resume the one Plan-ahead/progress-old state only through Plan-only derivation and the non-authorizing combined-static transition check, then replace the complete suffix and rerun the procedure. Never infer, partially patch, or erase a half-applied change.

Block Task dispatch while pending_route_change is non-null. A missing or
altered intent, a partial resync, or any affected durable row blocks. Defer
an active-Task change and request a user decision while preserving its
admitted route.

## 3. Review and revise Design
Trigger Design review when the Brainstorm spans modules, migration,
concurrency, security, persistence, externally visible compatibility, or
material ambiguity. Always dispatch an independent Codex Design Reviewer when
any trigger is present. Dispatch every user-named Design Reviewer as an
additional separate document-only work unit. Use
design|DESIGN_PATH|reviewer|AGENT|PROFILE for each reviewer. Dispatch an
independent Codex Design Fixer on design|DESIGN_PATH|fixer|codex|none. Record
an unbound intent and run the complete-snapshot procedure in the applicable
admission mode before every reviewer or Fixer dispatch or continuation. Use document
admission only before a routed Plan and synchronized progress exist. Use
full admission for every later Design decision. Any intervening delegation
action invalidates the prior snapshot. These document runs stay unbound.
Adjudicate findings against current artifacts. Continue the same Design Fixer
for revisions and continue each separate reviewer for re-review. Request a
user decision for requirement, scope, architecture, or user-data changes.
Require covering Design reviews to approve the same latest Design.

### Operational policy JSON

Apply this exact inline policy. Treat each hard or soft array entry as an
evidence object, count each distinct active soft signal once, and reject every
condition named by `invalid`.

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
Create only a bounded codeg-simple-progress-v1 shell. Pass document admission.
Dispatch an independent Codex Plan Author with writing-plans on
plan|PLAN_PATH|author|codex|none. Require ordered Task headings and exactly one
bounded unfenced codeg-b2d-routing-v1 JSON block.

Run Plan-only derivation twice: Author, then parent. Initialize route fields
only from the parent's exact rerun. Run combined static validation, then the
complete-snapshot procedure for full admission. Dispatch an independent
Codex Plan Reviewer on plan|PLAN_PATH|reviewer|codex|none plus any user-named
Plan reviewers. Review the complete latest Plan rather than a diff.

Before every later Author or reviewer continuation, run the procedure for
fresh full admission against synchronized Plan and progress. Route
accepted findings to the same Plan Author. After approval, call
register_simple_workflow. These document runs stay unbound.

Preserve an archived legacy Simple run on its recorded route. Adding routing
over admitted unbound history blocks.

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

Before each logical call, append a reserving run with the exact validator-copied binding, null durable identity, and operation fields; write a canonical lowercase UUID `intent_id` to progress before constructing the pending call. Run `validate-contract.mjs --ticket-v1-fingerprint --output-json` with the exact
pending call on bounded stdin; continue has null cwd. Copy only
`request_fingerprint` and `normalized_working_dir`, and retain the pending call
and digest. Request the artifact with the exact `admission_intent`, then validate
its returned path and digest through the validator.

On `prepared`, call delegate or continue with the same intent ID, returned admission ticket, same pending-call values, and a fresh physical correlation ID;
delegate uses the non-empty normalized cwd and continue has no cwd. On acknowledgement, fill returned durable identity without deleting intent history. On unknown
acknowledgement retain the intent, pending call, and digest. Process
`already_admitted` only through the validator's one adoption action. Stale,
consumed, or authorization failure discards the artifact and ticket and requires
a fresh artifact, validation, and ticket. Never expose artifact paths or
admission tickets in cards or prose. A definitive pre-reservation failure may
close the intent with null durable identity; otherwise preserve intent history.
Keep task_id globally unique and one non-null child per complete work-unit key.

### Progress JSON

Maintain this complete JSON shape inside the single progress marker. Repeat
Task and run entries without dropping route, lineage, dispatch_intent, or
binding fields. Keep top-level pending_route_change null until a boundary
change records the object below.

```json
{
  "schema_version": 1,
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "active_task_index": 1,
  "pending_route_change": null,
  "tasks": [
    {
      "index": 1,
      "status": "in_progress",
      "commit": null,
      "risk_level": "high",
      "task_agent_generation": 1,
      "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a",
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
          "task_agent_generation": 1,
          "root_task_id": null,
          "previous_task_id": null,
          "lineage_root_task_id": null,
          "generic_generation": null,
          "recovery_count": 0,
          "replaced_task_id": null,
          "replacement_reason": null,
          "dispatch_intent": {
            "intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
            "kind": "first",
            "continuation_target_task_id": null,
            "replacement_target_task_id": null,
            "replacement_reason": null,
            "expected_root_task_id": null,
            "expected_lineage_root_task_id": null,
            "expected_generic_generation": 1,
            "expected_child_conversation_id": null,
            "adopted_after_lost_acknowledgement": false
          },
          "orchestration_binding": {
            "schema_version": 1,
            "namespace": "brainstorm-to-delivery",
            "generation": 1,
            "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
          }
        }
      ]
    }
  ],
  "final_review_status": "pending",
  "updated_at": "2026-08-16T00:00:00Z"
}
```

### Pending route-change object

Record this exact non-null object only after step 2 of the section 2 sequence
proves a never-admitted pending suffix. Clear it only after approval and
another full admission.

```json
{
  "pending_route_change": {
    "requested_agent_type": "gemini",
    "requested_profile_id": null,
    "next_generation": 2,
    "effective_from_task_index": 5,
    "affected_task_indices": [5, 6, 7]
  }
}
```

## 6. Apply the workspace gate
Inspect git status, staged and unstaged diffs, recent commits, ignored
delivery reports, and repository instructions before each producer dispatch.
Record ownership and expected files. Preserve unrelated user changes, build
outputs, generated files, and concurrent edits. Require every producer to
inspect disk state before editing, stay within assigned files, use
test-first development, and report exact tests and diffs. Pause on ambiguous
ownership, destructive operations, secrets, external side effects, or
user-owned decisions. Request direction and resume from refreshed truth.

## 7. Execute Tasks serially
Use subagent-driven-development and execute one Plan Task at a time. Before
every first, continue, or replacement action, follow section 5; its artifact
validation is the complete-snapshot procedure for fresh full admission. Pass
the exact emitted binding. Route changed
pre-admission risk evidence through the same Plan Author, rerun validation,
and continue every Plan reviewer for full re-review. Block changed
post-admission evidence, including a coordinated Plan and progress generation
rewrite against an admitted durable row.

For a normal Task, dispatch the selected Task Agent as implementer and fixer;
after it settles, dispatch an independent Codex primary reviewer. For a high
Task, dispatch an independent Codex implementer and fixer; after it settles,
admit the Codex primary intent, call, and acknowledgement first, refresh and
validate, then admit the Task Agent auxiliary intent, call, and
acknowledgement, after which those children may run concurrently. Keep high
reviewers on distinct keys and child conversations. All routed Task work
units share that Task's one binding.

Join every required Task run. Continue the owning producer for each accepted
fix, invalidate all prior review conclusions for that Task, and rerun every
required reviewer on the latest producer result. Complete a Task only after
all expected key lineages end completed, checks pass, reviewers approve, and
the owned commit and report are current.

## 8. Recover generic runs
Continue a run with continue_delegation only on its stable key and child
conversation. Join observed tasks through get_delegation_status. For recovery
confirmation, call request_recovery_authorization and replay the authorized
generic call.

After compaction or a lost response, query first. Apply only one exact
validator adoption action to still-matching progress, mark
adopted_after_lost_acknowledgement true, persist, requery, and revalidate.
First, continue, and replacement lost acknowledgements adopt only an exact
unresolved intent. Deleting the intent or mirror remains blocking.

Only when every failure is B2D-DURABLE-005 with exact prefix
`status-only refresh required:` and the validator reports no identity or
binding failure, update each named progress run's state from its matched
durable status, persist, discard the snapshot, and rerun the complete-snapshot
procedure for full admission. Treat a validator-confirmed status-only
lifecycle advance as that state-only refresh loop, not a permanent identity failure.
Never rewrite Task ID, child, lineage, Agent/profile, key, generation, or
binding, and never idempotently replay an unresolved call before
reconciliation.

If pending_route_change exists, start with the complete-snapshot procedure
and resume the section 2 checkpoint sequence. Preserve at most two unexpected
continuations and one logical replacement per complete key lineage. Surface
the typed blocker and retain the recorded route.

Projection may emit simple_orchestration_binding_missing,
simple_orchestration_binding_mismatch, and
simple_orchestration_binding_orphan. Those warnings stay warning-only. They
never create a Gate, Card, or completion decision.

## 9. Complete final review
After all Tasks complete, re-read the Brainstorm, Design, Plan, routing,
progress, reports, reviews, commits, full branch diff, and worktree state.
Run covering tests, lint, build, and project checks. Run the complete-snapshot
procedure for fresh full admission before dispatching a fresh independent Codex
final reviewer on final_review|reviewer|codex|none. That work unit stays
unbound. After each owning producer fix, run the procedure for fresh full
admission before continuing the same final reviewer.
Bound final fixes return to the owning Task producer and reopen Task and
final review. Complete local delivery only when covering checks and
independent final review approve the same latest state. Commit only owned
changes locally. Leave merge, push, PR creation, and deployment to a
separate explicit request. Report commits, exact commands, outcomes, review
conclusions, retained Minors, worktree state, and blockers.
