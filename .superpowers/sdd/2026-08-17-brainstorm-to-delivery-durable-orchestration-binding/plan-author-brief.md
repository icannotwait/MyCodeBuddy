# Plan Author Brief — Durable Orchestration Binding Increment

You are the independent Codex Plan Author for brainstorm-to-delivery.
You own creating and later revising this Implementation Plan. The parent
orchestrator coordinates only and will not edit Plan content.

## Required first action

Read and follow `/Users/pengchao/.grok/skills/writing-plans/SKILL.md`
exactly. If that path is unavailable, follow the `writing-plans` skill from
your local Superpowers/skills tree. Announce that you are using writing-plans.

Do not implement production code, Skill prose, validator/tests, migrations,
or Task implementation. Write only the Plan file named below and the report
named below.

## Paths

- Working directory (absolute):
  `/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`
- Requirements baseline (approved Design):
  `docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md`
- Write the Plan to exactly:
  `docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md`
- Write your report to exactly:
  `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-author-report.md`
- Do not stage or commit `.superpowers/sdd/**`.

## Current repository truth

Inspect disk yourself. Do not trust this brief as a substitute for reading
the Design and current sources.

- Branch: `codex/b2d-generic-task-agent-routing`
- HEAD at dispatch: `f13c0c79`
- Worktree is isolated and should be clean except files you create.
- Preserve unrelated changes. You are not alone in the worktree historically;
  do not revert earlier commits.

Already landed on this branch (do **not** re-plan or re-implement):

- Original Plan:
  `docs/superpowers/plans/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing.md`
- Task 1 `6973793f`: Design Fixer + slotted Task reviewer key identity
- Task 2 `ab23f562`: bounded Plan routing + additive progress metadata parse
- Task 3 `ee2dfd62`: normal/high Simple route derivation and warning-only
  reconciliation
- Task 4 `145651ef`: route-aware Simple producer/reviewer graph nodes
- Task 5 `caaae2fe` plus later Skill/validator hardening through `21401f42`
  and `816dbe92`: Skill contract v2 and deterministic route validation
- Design increment: `08224d24`, hardened `f13c0c79`

The 2026-08-16 increment's final review was BLOCKED because a retained
admitted run could rewrite mutable `task_agent_generation` together with
Plan/progress. The user approved durable generic-run binding. Independent
Design review of `f13c0c79` is APPROVED (0 Critical, 0 Important).

This new Plan is the remaining increment that makes admitted Task routes
immutable by binding them to durable generic-run identity.

## Task Agent for this delivery

Invocation omitted an Agent. Generation 1 is:

- `agent_type`: `grok`
- `profile_id`: `null`

Use that generation as `effective_from_task_index: 1` in the Plan routing
block. Do not invent another Task Agent.

## What the Plan must implement

Cover the approved Design completely, including but not limited to:

1. Optional `orchestration_binding` v1 on `delegate_to_agent` and
   `continue_delegation` (all-or-none, unknown fields rejected).
2. Four new nullable columns on `delegation_task_runs`, no backfill,
   all-or-none insert trigger `trg_dtr_orchestration_binding_shape`,
   immutability trigger `trg_dtr_orchestration_binding_immutable`, and
   index `(parent_conversation_id, orchestration_namespace, created_at, task_id)`.
3. Persist the binding in the same reserving transaction. Never include the
   four columns in lifecycle/status update models.
4. Continuation/replacement inherit the source binding. Explicit mismatch or
   bound/unbound conversion returns
   `orchestration_binding_lineage_mismatch` before child or recovery side
   effects. Malformed input returns `orchestration_binding_invalid`.
5. Bound request fingerprints use the Design's v2 string-array shape. Unbound
   requests keep the current seven-string fingerprint.
6. Read-only MCP tool `get_delegation_orchestration_bindings` with the exact
   page/snapshot/cursor/conflict-set contract. Parent identity comes from the
   MCP token only. Return actual durable `agent_type`/`profile_id`, lineage,
   status, and binding. Never return prompt, preview, output, result,
   termination details, card summaries, or profile config.
7. Node validator modes:
   - Skill-only
   - `--derive-plan-routing --output-json` (no progress; never authorizes)
   - combined static Plan/progress (`admission_authorized: false`)
   - `--document-admission --durable-evidence FILE --output-json`
   - `--admission --durable-evidence FILE --output-json`
8. RFC 8785 canonical route fingerprint. Parent and Plan Author must not
   independently hash. Publish the Design's exact high-Task vector
   `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
9. Lost-acknowledgement adoption with operation-specific first/continue/
   replacement lineage. Deleted mirrors without an unresolved intent stay
   blocking.
10. Skill operational updates for binding query, exact validator-copied
    bindings, dispatch intents, fail-closed recovery, and parent-does-not-
    author constraints. Keep `SKILL.md` under 500 lines and imperative.
11. Warning-only Rust Simple projection codes
    `simple_orchestration_binding_missing`,
    `simple_orchestration_binding_mismatch`,
    `simple_orchestration_binding_orphan`. No Gate, manifest, completion
    Card, or platform-owned completion decision.
12. The complete Testing, Compatibility, and Success Criteria sections of
    the Design.

## Retained Design Minor that the Plan must carry

Keep the Grok `tools/list` 7680-byte JSONL budget as an explicit
schema-growth regression. Current test:
`src-tauri/src/acp/delegation/companion.rs` around the
`Grok tools/list JSONL bytes` assertion. Do not weaken the 7680 literal.
Budget the new tool plus nested binding fields so that test remains green
and is named in a Task's verification.

## Hard constraints

- Simple remains the only writable brainstorm-to-delivery mode.
- Do not restore workflow manifests, platform Gates, gate settlement,
  completion Cards, artifact digests, or platform-owned completion decisions.
- Do not reuse `delegation_task_runs.route_fingerprint` for orchestration
  identity.
- Do not backfill historical bindings.
- Do not change standalone/ad-hoc delegation when the binding is omitted.
- Requirement, scope, architecture, and user-data decisions stay user-owned.
- Follow RED-GREEN-REFACTOR. Every production behavior change starts with a
  focused test observed failing for the intended reason.
- Every filtered test command must execute at least one test.
- Every Rust compile/test/lint command in this Plan MUST disable default
  features and enable exactly `server,test-utils`. Example:

```bash
cd src-tauri
cargo test --no-default-features --features server,test-utils durable_binding
```

No verification command may enable the default `tauri-runtime`.
- Keep the Plan ≤ 2 MiB and the routing block ≤ 256 KiB.
- Include exactly one unfenced `codeg-b2d-routing-v1` JSON comment.
- Task headings must be contiguous from Task 1 and match the routing block.
- Classify every Task with `b2d_task_risk_v1`. Hard triggers make the Task
  high. Soft score ≥ 3 is high. Every signal needs non-empty evidence.
- High Tasks: Codex implementer, Codex primary reviewer, Grok auxiliary
  reviewer. Normal Tasks: Grok implementer, Codex primary reviewer.
- Express dependencies through prior Task outputs so execution stays serial.
- Give every Task exact file ownership, interfaces, verification commands,
  report location, and one focused commit boundary.
- Report path convention:
  `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/task-N-report.md`
- Do not stage those reports.
- No placeholders: no TBD, TODO, "implement later", "add validation", or
  "similar to Task N".
- Do not author Skill/validator/test/Rust production code in this turn.

## Current surfaces the Plan must name accurately

Read these before decomposing files:

- `src-tauri/src/acp/delegation/types.rs` — `DelegationRequest`,
  `ContinueDelegationRequest`, `DelegationError`
- `src-tauri/src/acp/delegation/tool_schema.json`
- `src-tauri/src/acp/delegation/listener.rs` — `parse_work_unit_key` and
  delegate/continue parsing
- `src-tauri/src/acp/delegation/run_store.rs` — `request_fingerprint`,
  `ReservingRunInsert`
- `src-tauri/src/acp/delegation/broker.rs`
- `src-tauri/src/acp/delegation/companion.rs` — Grok 7680-byte tools/list
- `src-tauri/src/db/entities/delegation_task_run.rs`
- `src-tauri/src/db/migration/mod.rs` — last migration is
  `m20260811_000001_simple_workflows`
- Design-required new migration:
  `src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs`
- `.agents/skills/brainstorm-to-delivery/SKILL.md`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`
- `src-tauri/src/acp/delegation/workflow/project.rs`
- `src-tauri/src/acp/delegation/workflow/key.rs`
- `src-tauri/tests/delegation_session_reuse_integration.rs`

Live MCP currently has no `orchestration_binding` field and no
`get_delegation_orchestration_bindings` tool. That is expected; this Plan
must add them.

Current validator CLI only accepts Skill-only or
`--plan --progress --plan-rel-path` together. It has no
`--derive-plan-routing`, `--document-admission`, `--admission`,
`--durable-evidence`, or `--output-json`.

## Plan self-check before you finish

1. Every Design Testing/Success Criteria item maps to a Task.
2. No placeholder language.
3. Later-task types and names match earlier-task interfaces.
4. Routing JSON Task indices match headings.
5. Risk arithmetic and derived routes are valid.
6. You ran Plan-only derivation if the current validator already supports
   `--derive-plan-routing --output-json`. If it does not (current HEAD does
   not), record that the parent cannot derive fingerprints until the
   validator Task lands, and still emit a complete valid routing block.

## Report contract

Write `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-author-report.md`
with:

- status: `DONE` or `BLOCKED`
- Plan path
- Task count and high/normal split
- self-review notes
- any concerns

Return only status, Plan path, Task count, and concerns. Do not paste the
Plan into the chat.
