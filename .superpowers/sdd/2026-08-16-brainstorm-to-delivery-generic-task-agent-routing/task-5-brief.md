### Task 5: Ship Skill contract v2 and deterministic route validation

**Dependencies:** Tasks 1-4 recognize and project every key and document shape the revised Skill emits. This Task is the emitter switch and must be last.

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: `src-tauri/tests/delegation_session_reuse_integration.rs`
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-5-report.md` (do not commit)

**Interfaces:**

- Consumes: Tasks 1-4 canonical keys/projection behavior, live generic delegation Agent identities, `writing-plans`, `subagent-driven-development`, and existing registration/recovery tools.
- Produces: exactly one `codeg-b2d-skill-contract-v2`, authoritative `codeg-b2d-routing-v1` validation, additive progress route validation, and complete operational instructions for independent document/Task roles.

Before editing the Skill, read and follow `/Users/pengchao/.codex/skills/.system/skill-creator/SKILL.md` and `/Users/pengchao/.codex/plugins/cache/gf-team/superpowers/6.2.0/skills/writing-skills/SKILL.md`. Keep `SKILL.md` below 500 lines and imperative.

Replace the v1 contract with this exact positive contract (the JavaScript constant and Markdown JSON must deep-equal after canonical key ordering):

```json
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
```

In `validate-contract.lib.mjs`, export and use:

```js
export const MAX_ROUTING_BLOCK_BYTES = 256 * 1024
const SKILL_CONTRACT_MARKER = "<!-- codeg-b2d-skill-contract-v2"
const ROUTING_MARKER = "<!-- codeg-b2d-routing-v1"
const RISK_POLICY_VERSION = "b2d_task_risk_v1"
const SOFT_SIGNAL_SCORES = new Map([
  ["cross_runtime_or_process", 2],
  ["broad_production_surface", 1],
  ["multiple_ownership_modules", 1],
  ["shared_interface", 1],
  ["dependency_or_build", 1],
  ["multi_layer_without_test_seam", 1],
])
const HARD_TRIGGER_KINDS = new Set([
  "concurrency_lifecycle",
  "security_trust_boundary",
  "migration_destructive_persistence",
  "public_compatibility",
  "unsafe_ffi",
  "update_rollback",
])
```

Add and export these pure interfaces:

```js
export function parseSimpleRouting(planMarkdown) {
  // returns { snapshot, failures }
}

export function validateRoutingSnapshot(snapshot, plan, failures) {
  // returns normalized generations/tasks for progress comparison
}

export function deriveExpectedRoute(task, generation, failures) {
  // returns exact implementer/primary/optional auxiliary identities and keys
}

export function validateProgressRouting(snapshot, routing, failures) {
  // enforces Plan/progress agreement and boundary-only generation changes
}
```

- [ ] **Step 1: Replace test fixtures with v2 Skill, routed Plan, and routed progress**

Build test helpers for a normal Grok Task and a high Task. The progress helper must derive these exact fields rather than hand-wave them:

```js
function expectedWorkUnitKeys(index, level, taskAgent) {
  const profile = taskAgent.profile_id ?? "none"
  return {
    implementer:
      level === "normal"
        ? `task|${index}|implementer|${taskAgent.agent_type}|${profile}`
        : `task|${index}|implementer|codex|none`,
    reviewers: {
      primary: `task|${index}|reviewer|primary|codex|none`,
      auxiliary:
        level === "high"
          ? `task|${index}|reviewer|auxiliary|${taskAgent.agent_type}|${profile}`
          : null,
    },
  }
}
```

Update `parseRecognizedWorkUnitKey` so Design Fixer and explicit reviewer slots parse, while legacy five-part Task reviewer keys parse with `slot: "primary", legacy: true`.

- [ ] **Step 2: Write failing Skill v2 ownership tests**

Test exactly one unfenced v2 block, all nine ordered phases, independent Design Fixer/Plan Author/reviewers, Grok default plus invocation selection, no parent Design/Plan/Task writing, conditional Design review, full Plan re-review, serial Tasks, high-review fan-out, owning-producer final fixes, recovery rails, and the ban on every retired v2 workflow mutation identifier.

Add negative prose fixtures for:

```text
The parent revises the Plan directly.
Always use Grok as the implementer.
Use the Task Agent to implement high Tasks.
Reuse one Codex conversation for implementation and review.
Switch Agent immediately inside the active Task.
Skip the auxiliary review after a high-Task fix.
```

- [ ] **Step 3: Write failing risk, generation, and route tests**

Cover all of these deterministic cases with explicit JSON mutations and rule IDs:

- omitted initial override resolves to generation 1 Grok; each built-in and valid `custom:*` identity/profile validates;
- invalid/reserved custom ID, ambiguous/unavailable placeholder, literal profile `"none"`, or malformed Agent never falls back;
- generations start at 1, remain contiguous/strictly increasing, start at Task 1, and each later `effective_from_task_index` equals the first pending Task that references it;
- any Task with a non-empty `runs` list freezes its generation/route; a generation change with an active/blocked/admitted Task or before the completed prefix fails, and its effective Task must still be pending with `runs: []`;
- all six hard triggers force high and require non-empty evidence;
- soft totals 0, 1, 2 are normal and 3+ are high;
- unknown, duplicate, evidence-free, wrong-score, wrong-total, contradictory level, and empty reason fail;
- normal has exactly selected implementer plus Codex primary; high has exactly Codex implementer plus Codex primary and selected auxiliary;
- wrong profile, order, slot, duplicate reviewer, missing reviewer, surplus reviewer, or free-form route fails;
- routing Task indices exactly match ordered Plan headings.

- [ ] **Step 4: Write failing progress agreement and lineage tests**

Require every routed progress Task to match `risk_level`, `task_agent_generation`, and all expected keys. A completed routed Task must contain a terminal completed lineage for every expected key. Runs outside the expected set fail.

Change lineage grouping from `run.role` to `run.work_unit_key`:

```js
const group = groups.get(run.work_unit_key) ?? []
group.push({ run, runIndex })
groups.set(run.work_unit_key, group)
```

Then test:

- primary and auxiliary reviewers both use generic role `reviewer` without merging lineages;
- key/Agent/profile remain stable within each complete-key group;
- replacement source and one-replacement budget are checked per complete key;
- `task_id` remains globally unique;
- two distinct work-unit keys cannot share one non-null child conversation ID;
- a legacy five-part reviewer remains readable only as primary in a legacy Plan/progress fixture; new routed progress requires the explicit six-part primary key;
- Plan/progress level, generation, implementer, primary, or auxiliary mismatch fails deterministically.

- [ ] **Step 5: Run the validator suite and verify RED**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: FAIL because the production validator and Skill still implement contract v1 and role-grouped lineages.

- [ ] **Step 6: Implement the v2 validator and rewrite the Skill**

Use a shared bounded unfenced-comment extractor for Skill/routing/progress markers. `parseSimplePlan` returns both ordered headings and routing. `validateSimpleDocuments` validates Skill, Plan routing, progress, and their agreement in that order.

Rewrite the seven current operational sections into the nine contract phases. The Skill must explicitly:

- inspect live delegation schemas/Agent discovery and resolve the invocation selection before document work;
- dispatch conditional Design Reviewer and independent Codex Design Fixer keys;
- create progress first, then dispatch independent Codex Plan Author with `writing-plans`; register Simple only after Plan validation/review approval;
- continue the same Design Fixer/Plan Author work unit for revisions and the same separate reviewer units for full re-review;
- prevent parent document/code edits and user-named document reviewers from entering Task/final roles;
- validate `b2d_task_risk_v1` and Plan/progress before every Task dispatch;
- return pre-admission risk-evidence changes through Plan Author revision and full Plan re-review; after admission, block and request a user decision instead of swapping the active route;
- execute the exact normal/high route, re-run all required reviewers after every fix, and defer/block active-Task Agent changes;
- route final findings back to the owning producer and reopen Task/final reviews;
- permit an archived/legacy Simple run to remain on its recorded route, but require a Plan Author revision with a complete routing block before the next pending Task adopts adaptive routing;
- preserve Simple registration, workspace gate, generic continuation/replacement behavior, and local-only delivery.

Do not mention or call retired workflow-v2 tools. Do not restore a Final Fixer work unit; `final_review|reviewer|codex|...` remains the only Final key.

- [ ] **Step 7: Rewrite the Rust Skill-forward contract scenarios**

Update `delegation_session_reuse_integration.rs` to read the v2 marker and assert the exact contract above. Replace the old Grok-hard-coded nine-scenario matrix with the approved eleven scenarios:

1. default normal: Grok implementer plus Codex primary;
2. selected non-Grok normal route;
3. high: Codex implementer plus Codex primary and Task Agent auxiliary;
4. Task Agent Codex still uses three distinct keys/children;
5. high fix continues Codex implementer and both reviewers re-review;
6. conditional Design Reviewer and Design Fixer are separate;
7. initial/revised Plan stays with Plan Author and separate Plan Reviewer;
8. boundary Agent change affects pending Tasks only;
9. active-Task change defers/blocks without handoff;
10. unavailable/recovery/replacement keeps Agent/profile/key and budgets;
11. final findings continue the owning normal/high producer and reopen reviews.

Use the canonical keys from the approved design and assert no two distinct route keys share a child conversation in the reuse integration setup.

- [ ] **Step 8: Run Task 5 GREEN and production checks**

From the repository root:

```bash
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs
```

Expected: both commands PASS; the second validates the production Skill rather than only fixtures.

From `src-tauri/`:

```bash
cargo test --test delegation_session_reuse_integration skill_forward_ -- --nocapture
cargo test --lib --features test-utils workflow::key::tests -- --nocapture
cargo test --lib --features test-utils simple_parse -- --nocapture
cargo test --lib --features test-utils simple_projection_ -- --nocapture
cargo check --lib --features test-utils
```

Expected: every filter executes at least one test and passes; Rust shared library compiles.

- [ ] **Step 9: Run formatting/lint for changed surfaces**

From the repository root:

```bash
pnpm exec prettier --check .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

From `src-tauri/`:

```bash
cargo fmt --all -- --check
cargo clippy --lib --features test-utils -- -D warnings
```

Expected: all checks PASS with no formatting diff and no warnings.

- [ ] **Step 10: Commit Task 5**

```bash
git add -- .agents/skills/brainstorm-to-delivery/SKILL.md .agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs src-tauri/tests/delegation_session_reuse_integration.rs
git commit -m "feat(skill): route brainstorm delivery by task risk"
```

- [ ] **Step 11: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-5-report.md` with the v2 contract result, scenario coverage, exact commands/outcomes, commit hash, and retained Minors. Do not stage it.

---

## Final Verification and Review

After Task 5's independent primary and auxiliary reviews approve its latest producer result:

- [ ] Re-read the approved Design, this Plan/routing block, all five Task reports, commits, and the complete branch diff.
- [ ] Run the Task 5 Step 8 and Step 9 commands again against final HEAD; record exact test counts and outcomes.
- [ ] Run `git status --short --branch` and verify only the ignored `.superpowers/sdd/**` reports remain outside committed Task changes.
- [ ] Dispatch a fresh independent Codex final reviewer on the complete branch. It must inspect spec coverage, Skill/validator contradiction resistance, legacy key/progress compatibility, non-blocking projection warnings, and producer/reviewer independence.
- [ ] Return each Critical/Important final finding to its owning Task producer work unit: Tasks 1-5 to their Codex implementer. After a fix, rerun every reviewer required by that Task's high route and then continue the same final-review work unit.
- [ ] Retain a Minor only with a concrete reason in the final-review ledger. Complete delivery only when covering checks and final review approve the same repository state.

## Recovery and Rollback Boundaries

- If Task 1 cannot preserve legacy five-part parsing, stop before the Skill emitter switch; do not migrate archived keys.
- If routing/progress parsing is malformed or oversized, keep safe Plan tasks/progress partial state and warnings; never convert it into admission authority.
- If routed projection cannot prove a valid expected route, fall back to the legacy aggregate display for that Task with `simple_plan_routing_invalid`; do not invent Agent identity.
- If the selected Task Agent/profile becomes unavailable, keep its generation and route recorded and block/defer according to the Skill. Do not rewrite it to Grok.
- If Task 5 validator/Skill integration fails, revert only the uncommitted Task 5 emitter changes; Tasks 1-4 are backward-compatible readers/projectors and may remain safely on the branch.
- No database rollback, manifest conversion, frontend migration, or data rewrite is required by this plan.
