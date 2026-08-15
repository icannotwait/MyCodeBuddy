# Brainstorm-to-Delivery Generic Task Agent and Adaptive Routing Design

## Status

Approved in brainstorming on 2026-08-16.

This design extends the writable Simple brainstorm-to-delivery workflow from
`2026-08-11-simple-workflow-v2-retirement-design.md`. It selectively restores
the independent document-authoring and adaptive Task-routing behavior from
`2026-07-27-brainstorm-to-delivery-adaptive-routing-design.md` without
restoring workflow manifests, platform gates, gate settlements, completion
Cards, or platform-owned completion evidence.

## Executive Decision

Simple remains the only writable brainstorm-to-delivery mode. Its durable
coordination surfaces remain the Brainstorm/Design, Implementation Plan,
progress ledger, Git repository, and generic delegation runs.

The workflow gains four behaviors:

1. The Grok-specific implementation role becomes a user-selectable **Task
   Agent** role. Grok remains the default.
2. A dedicated Codex Plan Author writes and revises the Implementation Plan.
3. Dedicated Codex document producers are separate from Codex document
   reviewers. The parent orchestrator no longer edits Design or Plan content.
4. `b2d_task_risk_v1` selects a normal or high Task route. High Tasks force
   Codex implementation and use Codex plus Task Agent review.

```text
completed Brainstorm
  -> conditional Codex Design review
  -> independent Codex Design Fixer when changes are required
  -> independent Codex Plan Author using writing-plans
  -> independent Codex Plan review
  -> serial Tasks
       normal: Task Agent implementer -> Codex primary reviewer
       high:   Codex implementer -----> Codex primary reviewer
                                 \----> Task Agent auxiliary reviewer
  -> independent Codex final review
  -> owning Task producer fixes findings
  -> local delivery
```

The parent orchestrator selects and dispatches roles, reconciles files and
generic runs, adjudicates findings against repository evidence, updates the
progress ledger, and controls delivery. It does not write Design, Plan, or Task
implementation content.

## Goals

- Keep Grok as the default Task Agent while allowing a user to select another
  built-in or custom Agent in the Skill invocation message.
- Use one Task Agent selection at a time and permit an explicit selection
  change only between Tasks.
- Keep Task-Agent identity stable within every admitted work-unit lineage.
- Restore a dedicated Codex Plan Author for initial Plan creation and revision.
- Route Design fixes and Plan revisions through independent Codex producer
  conversations rather than the parent orchestrator.
- Preserve producer/reviewer conversation independence even when all selected
  Agent types are Codex.
- Restore the deterministic `b2d_task_risk_v1` policy and its normal/high
  routes.
- Keep Simple's non-blocking platform projection and generic delegation
  recovery model.
- Make routing, recovery, and Task-Agent changes machine-checkable through
  bounded Plan and progress contracts.

## Non-Goals

- Restoring workflow manifest publication or any workflow-v2 mutation tool.
- Restoring platform gate settlement, completion Cards, artifact digests, or
  platform-owned completion decisions.
- Adding an Agent picker to the Codeg frontend.
- Adding project-level Task-Agent configuration.
- Automatically selecting an Agent from task content or availability.
- Silently substituting another Agent when the selected Task Agent is
  unavailable.
- Supporting an Agent handoff inside an active Task.
- Restoring Plan finding-owner subsets, scoped reviewer cohorts, stagnation
  counters, or automatic holistic Plan rewrites.
- Changing standalone delegation outside brainstorm-to-delivery.

## Terminology

**Task Agent** is the workflow-selected auxiliary Agent type and optional
profile. It implements and fixes normal Tasks and acts as the auxiliary
reviewer for high Tasks. Grok is the default, not a special role.

**Task Agent generation** is a monotonically increasing selection record. Each
Task references exactly one generation. Changing the selection appends a new
generation for pending Tasks and never rewrites prior Task identity.

**Primary reviewer** is the independent Codex Task reviewer required for both
normal and high Tasks.

**Auxiliary reviewer** is the independent Task Agent reviewer additionally
required for high Tasks.

**Document producer** is either the Design Fixer or Plan Author. A document
producer may continue its own work unit for revisions but cannot review or
approve its own artifact.

**Parent orchestrator** is the root conversation running the Skill. It owns
coordination and adjudication, not document or Task production.

## System Invariants

1. Simple remains manifest-free and has no platform-owned execution gate.
2. The invocation resolves exactly one initial Task Agent generation. An
   omitted selection resolves to Grok.
3. An explicit Task Agent is never silently replaced or downgraded.
4. A Task Agent generation is immutable once any Task referencing it is
   admitted.
5. A Task's risk, producer route, and reviewer slots are fixed at admission.
6. A Task Agent selection change can affect only pending Tasks after a Task
   boundary and a newly reviewed Plan revision.
7. Distinct work-unit keys cannot share a child conversation ID. A continuation
   reuses only its own work unit's child conversation.
8. Consequently, the Design Fixer, Plan Author, Task producers, document
   reviewers, Task reviewers, and final reviewer remain separate. This applies
   even when their Agent types and profiles match.
9. Task order is serial. The two high-Task reviews may run concurrently after
   the producer finishes.
10. Any implementation or fix invalidates all prior Task review conclusions.
11. A high Task completes only after both reviewers cover and approve the
    latest producer result.
12. Generic delegation continuation and replacement budgets remain
    authoritative for each stable work-unit lineage.
13. Plan/progress/run disagreement creates a platform reconciliation warning,
    not a platform admission gate. The Skill still fails closed before an
    invalid route is dispatched.
14. Requirement, scope, architecture, or user-data decisions remain owned by
    the user. Agents may not infer a material change from a review finding.

## Role Contract

### Document roles

| Work | Producer | Reviewer | Trigger |
| --- | --- | --- | --- |
| Design revision | Codex Design Fixer | Mandatory Codex Design Reviewer plus user-named document reviewers | Conditional Design review finds a valid issue |
| Plan initial draft | Codex Plan Author | Mandatory Codex Plan Reviewer plus user-named document reviewers | Every workflow |
| Plan revision | Same Codex Plan Author work unit | Same document reviewer work units | Valid Plan finding or pending-Task route change |

Design review keeps the current conditional trigger. It is required when the
Brainstorm spans modules, migration, concurrency, security, persistence,
externally visible compatibility, or material ambiguity. Otherwise the
completed Brainstorm remains the requirements baseline without a Design review
round.

Document Reviewers are read-only. Codex is mandatory; reviewers explicitly
named by the user remain optional and may participate only in Design and Plan
review. When the parent adjudicates a Design finding as valid and it does not
require a user decision, the parent sends a consolidated revision brief to the
Design Fixer. Later Design fixes continue the same Fixer work unit, and
re-review continues the same Reviewer work units.

The Plan Author is the only role that creates or edits the Plan. Its first
prompt requires it to invoke and follow `writing-plans`. A review-driven Plan
revision continues the same Author work unit. The parent may update progress
and adjudication notes but cannot patch Plan content.

Plan re-review remains deliberately Simple: the same independent document
Reviewer work units review the complete latest Plan after every revision until
no Critical or Important finding remains. There is no finding-owner subset,
stagnation state, or automatic rewrite rail.

### Task routes

| Risk | Implementer and fixer | Required reviewers |
| --- | --- | --- |
| `normal` | Task Agent | Independent Codex primary reviewer |
| `high` | Independent Codex implementer | Independent Codex primary reviewer and independent Task Agent auxiliary reviewer |

The Task Agent is auxiliary at the workflow level rather than an unconditional
implementer. High risk always forces the producer to Codex. The selected Task
Agent then supplies the second review perspective.

When the Task Agent is also Codex, high risk intentionally creates three
different Codex child conversations: implementer, primary reviewer, and
auxiliary reviewer. Conversation independence remains mandatory even though
cross-model diversity is absent.

Task review findings are consolidated by the parent and returned to the owning
producer work unit. A normal fix returns to the Task Agent implementer. A high
fix returns to the Codex implementer. After any fix, the normal primary review
or both high reviews must re-run against the latest result.

### Final review

After every Task is complete and covering verification passes, a fresh Codex
final reviewer inspects the complete delivery. Final findings are mapped to the
owning Task producer:

- normal Task findings return to that Task's Task Agent implementer;
- high Task findings return to that Task's Codex implementer.

Fixes reopen covering Task review and final review. There is no separate Final
Fixer that bypasses Task ownership.

## Task Agent Selection

### Invocation resolution

The Skill reads the invocation message before Plan authoring:

- no Agent named: `agent_type: "grok"`, `profile_id: null` unless a Grok
  profile is explicitly selected;
- Agent named: resolve the canonical built-in or `custom:*` wire identity from
  live delegation schemas and available Agent discovery;
- profile named: bind that profile to the generation;
- Agent or profile ambiguous: ask one focused clarification before dispatch;
- Agent unavailable: record a typed blocker and ask the user to choose; do not
  fall back to Grok.

The initial result is generation 1 and is supplied to the Plan Author.

### Boundary changes

A Task Agent change is legal only when no Task is active and every prior Task
is completed. The change procedure is:

1. Record the requested Agent/profile and next generation in the progress
   ledger as a pending route-change intent.
2. Confirm the Agent and profile are available.
3. Continue the Plan Author with a brief that appends the generation and
   rewrites only pending Task routes.
4. Run the deterministic Plan/routing validator.
5. Continue the Plan Reviewer for a full latest-Plan review.
6. After approval, update pending progress entries and admit the next Task.

Completed and previously admitted Tasks retain their original generation,
route, work-unit keys, run history, and recovery consumption. An unresolved
blocked Task stops serial execution and is not a boundary for changing its own
Agent.

A change requested during an active Task is deferred until that Task reaches a
boundary. If the user requires an immediate switch, the workflow blocks; this
design does not create a same-Task cross-Agent handoff lineage.

## Task Risk Policy

Plan classification uses the existing policy identifier
`b2d_task_risk_v1`.

### Hard triggers

Any hard trigger makes a Task high regardless of soft score:

| Signal | Trigger |
| --- | --- |
| `concurrency_lifecycle` | Threading, async coordination, cancellation, ordering, ownership lifetime, or process lifecycle behavior changes |
| `security_trust_boundary` | Authentication, authorization, secrets, sandboxing, trust-boundary validation, or privilege changes |
| `migration_destructive_persistence` | Schema/data migration, deletion, irreversible persistence, or destructive state transitions |
| `public_compatibility` | Public API, protocol, schema, serialized format, or externally consumed behavior changes |
| `unsafe_ffi` | Rust `unsafe`, native FFI, ABI, memory ownership, or equivalent low-level boundaries |
| `update_rollback` | Installer, updater, rollback, recovery, or version-transition behavior changes |

### Soft signals

When no hard trigger is present, each distinct active soft signal contributes
once:

| Signal | Score | Trigger |
| --- | ---: | --- |
| `cross_runtime_or_process` | 2 | Changes code or a contract across runtime or process boundaries |
| `broad_production_surface` | 1 | Touches at least five production files, excluding tests, docs, snapshots, and generated output |
| `multiple_ownership_modules` | 1 | Touches at least two independently owned modules or subsystems |
| `shared_interface` | 1 | Changes an interface or contract consumed outside the owning module |
| `dependency_or_build` | 1 | Changes dependencies, lockfiles, build configuration, packaging, or deployment |
| `multi_layer_without_test_seam` | 1 | Spans at least two architectural layers without an isolated boundary test seam |

A score of 3 or greater is high; 0 through 2 is normal. Every active signal
requires non-empty file, module, or interface evidence. Unknown signals,
duplicates, contradictory levels, incorrect arithmetic, or evidence-free
signals invalidate the Plan route.

Both `hard_triggers` and `soft_signals` contain objects with a canonical
`kind` and non-empty `evidence` array. Soft-signal objects additionally contain
their fixed policy score. An empty array means that signal class is inactive;
a bare signal name without evidence is invalid.

Before Task admission, new evidence may change classification only through a
Plan Author revision and full Plan re-review. After admission, evidence that
invalidates the frozen classification or route blocks the Task and escalates
to the user. The Skill does not dynamically swap the active producer.

## Structured Contracts

### Skill contract v2

The Skill replaces `codeg-b2d-skill-contract-v1` with exactly one
`codeg-b2d-skill-contract-v2` JSON comment. Its positive contract records:

- existing phase order and generic delegation interfaces;
- Codex document roles and independence;
- Task Agent default, invocation source, and boundary-only changes;
- `b2d_task_risk_v1`;
- normal and high producer/reviewer routes;
- reviewer slots;
- serial Task execution and parallel high review fan-out;
- existing generic recovery limits; and
- independent Codex final review.

The repository validator treats this block as authoritative and rejects prose
that negates or contradicts it.

### Plan routing block

Every Plan contains exactly one bounded JSON comment marked
`codeg-b2d-routing-v1`. It is the machine-readable source for Agent selection,
risk classification, and Task routes. Its conceptual shape is:

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
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": ["src/lib/transport", "src-tauri/src/web"]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": ["transport request contract"]
          }
        ],
        "score": 3,
        "reason": "Changes a shared desktop/server transport boundary."
      },
      "route": {
        "implementer": {
          "agent_type": "codex",
          "profile_id": null
        },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    }
  ]
}
```

Task indices match the Plan headings exactly. Generations are contiguous,
strictly increasing, and cover Task ranges without rewriting prior ranges.
Normal and high routes are derived rather than freely chosen.

Plan Task bodies retain concise human-readable risk reasoning and evidence for
review. There is no second Markdown routing matrix; avoiding duplicate
machine-readable sources prevents drift.

### Progress contract

The existing `codeg-simple-progress-v1` marker and schema remain compatible
with current Simple projection. Each Task gains additive routing context:

- `risk_level`;
- `task_agent_generation`;
- the expected implementer and reviewer-slot work-unit keys; and
- the existing actual `runs` list.

The Plan block is authoritative for planned classification and routes. Progress
mirrors the effective route so the parent can reconcile intent with actual
runs after compaction. The validator rejects Plan/progress route disagreement.
The platform parser may project disagreement as warning state; it never turns
the routing metadata into a workflow gate.

The existing Plan, progress-block, and progress-document size limits remain in
force. A bounded routing-block limit is added and must fit within the Plan's
existing 2 MiB limit.

## Canonical Work-Unit Keys

New work uses these keys:

```text
design|{design_rel_path}|fixer|codex|{profile_or_none}
design|{design_rel_path}|reviewer|{agent}|{profile_or_none}
plan|{plan_rel_path}|author|codex|{profile_or_none}
plan|{plan_rel_path}|reviewer|{agent}|{profile_or_none}
task|{index}|implementer|{agent}|{profile_or_none}
task|{index}|reviewer|primary|{agent}|{profile_or_none}
task|{index}|reviewer|auxiliary|{agent}|{profile_or_none}
final_review|reviewer|codex|{profile_or_none}
```

The reviewer slot is part of identity. It allows primary and auxiliary
reviewers to use the same Agent type and profile without key collision.

Existing five-part Task reviewer keys remain recognized as legacy primary
reviewer keys so historical Simple runs and archived projections remain
readable. New Skill runs always emit the explicit six-part reviewer key.

Lineage validation groups runs by complete work-unit key, not by the generic
`reviewer` role. The role remains `reviewer` in generic run metadata; the key
provides the stable slot identity.

## Execution Flow

### Establish current truth

The parent reads repository instructions, Brainstorm, relevant code and tests,
recent commits, user changes, live delegation schemas, and available Agents.
It resolves the initial Task Agent before document work starts.

### Review and revise Design

When a Design trigger is present, the parent records review intent and
dispatches the independent Design Reviewer. Valid non-material findings are
consolidated into the Design Fixer brief. Material requirement, scope,
architecture, or user-data changes pause for user decision before the Fixer is
continued. The Design Reviewer re-reviews the latest file.

### Author and review Plan

The parent creates the initial progress document and records Plan Author
intent. The independent Codex Plan Author invokes `writing-plans`, writes the
Plan and routing block, validates them, and reports the result. The independent
Plan Reviewer reviews task decomposition, risk evidence, routing, verification,
and repository fit. Valid findings return to the same Plan Author work unit.

After Plan approval, the parent registers the Simple descriptor and syncs all
Task entries into progress. Registration remains locator metadata rather than
an execution gate.

### Execute Tasks serially

Immediately before each Task, the parent applies the existing workspace gate
and validates the latest Plan/progress pair.

For a normal Task, it dispatches or continues the Task Agent implementer,
checks the report and repository state, then dispatches the Codex primary
reviewer.

For a high Task, it dispatches or continues the Codex implementer, checks the
report and repository state, records both review intents, dispatches primary
and auxiliary reviewers as separate work units, and joins both. Their review
runs may execute concurrently; the next Task cannot start until both settle.

Critical and Important findings return in one adjudicated producer brief.
After a fix, every reviewer required by that Task route re-reviews the latest
result. Retained Minor findings require a recorded reason.

### Verify and deliver

After all Tasks pass their routes, the parent runs scope-appropriate test,
lint, build, and project checks. It then dispatches a fresh Codex final
reviewer. Final fixes return to owning Task producer work units and reopen the
affected Task review plus final review. Delivery is complete only from current
repository evidence, covering verification, and approved final review.

## Recovery and Error Handling

Every first run uses `delegate_to_agent`; later work on the same unit uses
`continue_delegation`. The existing limits remain two unexpected
continuations and one logical replacement per established lineage, with
pre-admission retries retaining current semantics.

After compaction, interruption, or resume, the parent re-reads the Design,
Plan, routing block, progress, reports, Git state, and live generic run state.
It treats remembered routing as provisional until those sources agree.

The workflow blocks without substitution when:

- the selected Task Agent or profile is unavailable;
- the routing block is absent, malformed, oversized, or inconsistent;
- risk signals, evidence, score, level, or derived route are invalid;
- a producer and reviewer share a child conversation;
- a high Task lacks either reviewer slot;
- a Task Agent change touches an active, completed, or previously admitted
  Task;
- a review covers stale producer output;
- a requested active-Task Agent handoff cannot be represented safely; or
- generic continuation or replacement rails are exhausted.

Plan/progress/durable-run mismatches that affect display still produce Simple
projection warnings. They do not authorize the Skill to bypass an invalid
route, generic identity rule, or recovery budget.

## Backend and Projection Changes

This design requires bounded backend support for the expanded Simple key
vocabulary, not a new workflow state machine:

- recognize Design Fixer keys;
- recognize explicit primary and auxiliary Task reviewer keys;
- preserve legacy five-part Task reviewer parsing;
- include reviewer slot in parsed identity and synthetic Simple node identity;
- project simultaneous primary and auxiliary reviewer runs as separate nodes;
- parse additive progress route metadata for reconciliation warnings where
  useful; and
- keep all routing discrepancies non-blocking at the platform layer.

No new database table, workflow header, manifest revision, gate cycle, or
completion evidence row is introduced.

## Skill and Validator Changes

`.agents/skills/brainstorm-to-delivery/SKILL.md` will be revised to:

- resolve the Task Agent from the invocation with Grok default;
- dispatch independent Design Fixer, Plan Author, and document reviewers;
- prohibit parent Design and Plan edits;
- require `b2d_task_risk_v1` evidence and derived routes;
- execute normal and high routes exactly;
- support boundary-only Task Agent changes;
- make both high reviewers stale after every producer mutation;
- preserve generic recovery and workspace safety; and
- avoid every workflow-v2 mutation interface.

The JavaScript validator will add deterministic parsers and checks for:

- Skill contract v2;
- the Plan routing block and size bound;
- Task Agent generations;
- hard and soft risk arithmetic;
- normal/high route derivation;
- primary/auxiliary reviewer keys;
- Plan/progress agreement;
- per-key lineage stability;
- producer/reviewer conversation independence when IDs are known; and
- boundary-only pending-Task route changes.

## Testing

### Skill and document contract tests

- Missing Task Agent selection resolves to Grok.
- Every supported built-in and valid `custom:*` identity can be selected.
- Invalid, ambiguous, or unavailable selections never silently fall back.
- Skill prose cannot restore a Grok-only implementer contract.
- Parent Design/Plan writing language is rejected.
- Plan Author, Design Fixer, and document reviewers use the required keys.
- User-named document reviewers remain optional and cannot enter Task or final
  review roles.
- Every hard trigger forces high.
- Soft scores 0 through 2 are normal; 3 and above are high.
- Unknown, duplicate, contradictory, or evidence-free signals fail.
- Normal and high routes contain exactly the required roles.
- A Task-Agent generation change rewrites pending routes only.
- Plan/progress mismatch fails deterministic document validation.

### Rust unit and projection tests

- New Design Fixer and slotted reviewer keys round-trip canonically.
- Legacy Task reviewer keys parse as primary.
- Primary and auxiliary Codex reviewers remain distinct with identical
  profiles.
- Invalid slot, path, Agent, profile, index, or control characters fail.
- Simple observation recognizes new keys without creating a workflow header.
- Simple projection renders high producer and both reviewers independently.
- Routing disagreement remains a warning rather than an admission failure.

### Workflow scenarios

1. No override runs a normal Task with Grok implementation and Codex review.
2. A user-selected non-Grok Task Agent runs the complete normal route.
3. A high Task forces Codex implementation and Codex plus Task Agent review.
4. Task Agent Codex still produces three independent high-Task conversations.
5. One high reviewer requests changes; the Codex implementer fixes and both
   reviewers re-review.
6. Conditional Design review uses separate Codex Reviewer and Fixer sessions.
7. Plan initial authoring and every revision remain in the Plan Author session,
   separate from Plan review.
8. A boundary Agent change affects pending Tasks and preserves all prior
   lineages.
9. An active-Task switch request blocks or defers without a handoff dispatch.
10. Agent unavailability, compaction, continuation, replacement, and exhausted
    recovery rails preserve the recorded route.
11. Final findings return to the correct normal or high producer and reopen
    required reviews.

### Verification commands

Implementation verification will include the focused Skill validator suite,
the validator's production-file check, Rust key/parser/projection unit tests,
and the brainstorm-to-delivery integration contract tests. Scope-appropriate
formatting, lint, and Cargo checks will run for every changed runtime surface.

## Compatibility and Rollout

Existing Simple workflows continue to parse their progress-v1 files and
five-part reviewer keys. They are not retroactively assigned risk blocks or
Task Agent generations. A resumed legacy workflow may continue under its
recorded legacy route; adopting this design's adaptive routing requires a Plan
Author revision that creates a complete routing block before the next pending
Task.

Archived manifest workflows remain read-only and retain their current
projection. This design does not revive or convert them in place.

The change ships atomically across the repository Skill, validators, canonical
key parser, Simple projection, and integration contracts so no new Skill emits
keys or routing data that the current runtime cannot recognize.

## Success Criteria

- Grok is a default Task Agent selection, not a hard-coded implementation
  role.
- A user can select another Task Agent from the invocation message.
- A Task Agent can change between Tasks without altering earlier lineages.
- Design and Plan producers never review their own artifacts.
- The parent orchestrator does not edit Design, Plan, or Task code.
- Every Task has a validated `b2d_task_risk_v1` classification and derived
  route before admission.
- High risk always forces Codex implementation and two independent reviewers.
- Same-Agent reviewer combinations cannot collide in work-unit identity.
- Simple remains manifest-free, platform-gate-free, and recoverable through
  Plan, progress, Git, reports, and generic delegation state.
