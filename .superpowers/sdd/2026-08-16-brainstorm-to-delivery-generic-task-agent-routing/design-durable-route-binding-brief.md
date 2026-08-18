# Design Revision Brief: Durable Orchestration Route Binding

## Role and Scope

You are the independent Codex Design Fixer. Revise only:

`docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md`

Do not edit the implementation Plan, Skill, validator, tests, Rust production
code, migrations, or progress ledger. The parent is coordination-only.

Starting implementation head: `816dbe92e62e5cd6d4cd8f7026fe4af4b18791de`.

## Root Cause the Revision Must Resolve

The current Design requires a Task Agent generation and admitted Task route to
be immutable, but the authoritative JavaScript check sees only mutable Plan and
progress snapshots. A coordinated rewrite of Plan Task metadata, progress Task
metadata, and a retained high-Task Codex implementer run's mirrored generation
passes because the high implementer key is generation-invariant. Pure mutable
documents cannot prove historical immutability, and durable generic runs do not
currently carry the workflow generation/fingerprint needed as an external
reference.

The user selected and approved durable generic-run binding (option A). This is
an architecture correction, not a relaxation of the active-Task switch rule.

## Approved Architecture

1. Add an optional generic `orchestration_binding` to delegation inputs and
   durable run identity. Its conceptual v1 shape is:

   ```json
   {
     "namespace": "brainstorm-to-delivery",
     "generation": 2,
     "route_fingerprint": "sha256:<64 lowercase hex>"
   }
   ```

2. Do not reuse the existing `delegation_task_runs.route_fingerprint`; that
   field identifies ACP launch/config route state and has different semantics.
   Use clearly separate durable fields or an equivalently typed separate
   representation.
3. For brainstorm-to-delivery, `generation` is the Plan Task's
   `task_agent_generation`. The Node validator derives the binding; the parent
   must use its exact output rather than independently recomputing it.
4. Canonical route fingerprint input includes schema/namespace, Task index,
   Task Agent generation, risk level, implementer Agent/profile, ordered
   reviewer slots with Agent/profile, and all expected canonical work-unit
   keys. Specify deterministic canonical JSON and SHA-256 encoding.
5. `delegate_to_agent` persists the binding in the same reserving transaction
   that creates the durable run, before child spawn/resume side effects.
6. A binding is immutable after insert. No lifecycle/status update path may
   alter it.
7. `continue_delegation` inherits the source binding. An explicitly supplied
   mismatch is rejected before side effects or recovery-budget consumption.
8. A replacement must inherit the replaced lineage's binding and rejects a
   mismatch before side effects or budget consumption.
9. Binding participates in request fingerprint/idempotency so different route
   bindings cannot alias one tool call.
10. Add a parent-scoped, read-only, bounded/paginated durable binding query
    (design an exact tool/API name and contract). It returns only run identity,
    status, and binding, never prompt/output. It must support complete discovery
    of bound runs so progress cannot hide a durable run by deleting its mirror.
11. Before every dispatch, continuation, recovery, resume-after-compaction,
    and Task Agent selection change, the parent obtains the complete durable
    binding set and supplies it to deterministic validation alongside Skill,
    Plan, and progress.
12. Validation cross-checks task ID, work-unit key, child conversation,
    generation, fingerprint, current status, and absence/presence in both
    directions. Any coordinated Plan/progress rewrite must fail against the
    durable row.
13. A legal boundary switch appends a generation and changes only never-
    admitted pending Tasks. Historical bindings remain unchanged.
14. Rust Simple projection may expose bounded reconciliation warnings but does
    not own the execution decision and does not create a manifest, Gate, gate
    settlement, completion Card/evidence, or platform completion decision.

## Approved Error and Compatibility Rules

- Binding fields are all-or-none and bounded. Version/namespace must be
  explicit; generation is positive and bounded; fingerprint is exact lowercase
  `sha256:` plus 64 hex characters.
- Stable typed transport/admission errors distinguish malformed binding from
  source-lineage mismatch.
- Binding-query unavailable, incomplete/truncated pagination, stale snapshot,
  DB failure, missing expected row, unexpected extra bound row, or any identity
  mismatch fails closed in the Skill. Never fall back to Plan/progress alone.
- Migration columns are nullable with no guessed backfill.
- Existing standalone/ad-hoc delegation and old clients may omit a binding.
- A legacy Simple workflow without the new routing block keeps legacy behavior.
  A workflow with the new routing block and an admitted unbound run cannot
  retroactively invent evidence and must block.
- Missing binding is not itself a Rust Simple platform Gate. The revised Skill
  and deterministic validator require it for newly routed work.
- Durable mismatch has stable `B2D-DURABLE-*` validator rule IDs. Projection
  warning codes remain non-blocking.
- Define pagination/snapshot consistency and fail-closed recovery clearly.

## Existing Decisions That Must Remain Intact

- Grok is the default Task Agent.
- Normal Task: selected Task Agent producer/fixer plus independent Codex primary
  reviewer.
- High Task: independent Codex producer/fixer plus independent Codex primary
  reviewer and independent selected Task Agent auxiliary reviewer.
- The Task Agent is auxiliary at the workflow level and cannot switch during an
  active/admitted Task.
- Design Fixer, Plan Author, document reviewers, Task work units, and final
  reviewer are independent child conversations. The parent does not write
  Design, Plan, Skill prose, validator/tests, or Task implementation.
- Initial Plan authoring is by the independent Codex Plan Author using
  `writing-plans`.
- Simple remains manifest-free and platform-gate-free; platform reconciliation
  remains warning-only.
- Canonical Task work-unit key grammar remains unchanged, including explicit
  primary/auxiliary slots and legacy five-part primary readability.
- Generic continuation/replacement budgets and stable lineage semantics remain
  authoritative.

## Concrete Repository Context to Reflect Accurately

- `delegation_task_runs` already has an ACP/config `route_fingerprint`; do not
  overload it.
- Generic request types are in
  `src-tauri/src/acp/delegation/types.rs`.
- Durable insert/view and request fingerprinting are in
  `src-tauri/src/acp/delegation/run_store.rs` and broker admission paths.
- The MCP schema is
  `src-tauri/src/acp/delegation/tool_schema.json`; listener parsing is in
  `src-tauri/src/acp/delegation/listener.rs`.
- DB entity and migrations are under `src-tauri/src/db/entities/` and
  `src-tauri/src/db/migration/`.
- `get_delegation_status` currently returns task reports but no work-unit or
  orchestration binding identity, so the Design must name the durable evidence
  read surface needed for recovery.
- The current validator CLI accepts Skill/Plan/progress only. The Design must
  define how a bounded durable-evidence input/snapshot is supplied and how
  static contract-only validation remains usable.

## Required Test Design

The revised Testing section must include at least:

- Cross-language/boundary format vectors for valid and malformed bindings.
- Node canonical fingerprint vectors and determinism/order controls.
- Strict negative test for the exact coordinated rewrite: Plan, progress Task,
  and mirrored run generation/fingerprint rewritten while durable evidence
  remains original.
- Missing/deleted progress mirror, fabricated task ID, wrong child ID, wrong
  work-unit key, wrong generation/fingerprint, extra durable run, unbound routed
  run, and legal boundary-generation controls.
- Migration/entity round-trip and null legacy behavior.
- Listener/MCP schema validation and parent isolation.
- Reserving-transaction persistence, no post-insert mutation path, request-
  fingerprint separation, idempotent replay, continue inheritance/mismatch,
  replacement inheritance/mismatch, and pre-side-effect/pre-budget rejection.
- Durable binding query pagination, stable snapshot, truncation/failure,
  namespace filter, and no prompt/output leakage.
- End-to-end Skill scenario proving an admitted high Task cannot switch from
  Grok auxiliary route to another Task Agent after Codex producer admission.
- Existing normal/high routing, independent reviewer, legacy key, recovery,
  warning-only Simple projection, and no-manifest/no-Gate regressions.
- Rust commands must always use
  `--no-default-features --features server,test-utils`; never enable default
  `tauri-runtime` during this work.

## Required Design Quality

- Integrate the correction coherently rather than appending an isolated note.
- Update Goals, Non-Goals, terminology, invariants, structured contracts,
  execution/recovery flow, backend/Skill/validator changes, testing,
  compatibility, and success criteria where affected.
- Remove or revise now-false statements such as “No new database table...” so
  they accurately allow a nullable migration while still forbidding a new
  workflow state machine.
- Explicitly distinguish durable identity preservation from a platform-owned
  Simple admission Gate.
- No placeholders, TBDs, contradictory requirements, or underspecified hash
  inputs.
- Keep scope to this route-binding correction and the already-approved generic
  Task Agent design.

## Deliverables

1. Revise the Design document.
2. Perform at least three named self-review passes: completeness, internal
   consistency/threat model, and implementation/testability. Fix every issue
   found.
3. Run a placeholder/contradiction scan and `git diff --check`.
4. Commit only the Design document with a focused commit message.
5. Write a full report to:

   `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/design-durable-route-binding-report.md`

Return only status, commit SHA, one-line self-review summary, and concerns.
