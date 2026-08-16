import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { describe, it } from "node:test"
import { fileURLToPath } from "node:url"
import {
  MAX_ROUTING_BLOCK_BYTES,
  deriveExpectedRoute,
  parseSimplePlan,
  parseSimpleRouting,
  validateProgressRouting,
  validateRoutingSnapshot,
  validateSimpleDocuments,
  validateSkillMarkdown,
} from "./validate-contract.lib.mjs"

const here = dirname(fileURLToPath(import.meta.url))
const realSkill = readFileSync(join(here, "..", "SKILL.md"), "utf8")
const planRelPath = "docs/superpowers/plans/example.md"
const copy = structuredClone

const SKILL_CONTRACT = {
  schema_version: 2,
  phase_order: [
    "establish-current-truth",
    "resolve-task-agent",
    "review-and-revise-design",
    "author-and-review-plan",
    "maintain-progress",
    "apply-workspace-gate",
    "execute-tasks-serially",
    "recover-generic-runs",
    "complete-final-review",
  ],
  interfaces: {
    plan_authoring: "writing-plans",
    task_execution: "subagent-driven-development",
    registration: "register_simple_workflow",
    first_run: "delegate_to_agent",
    later_run: "continue_delegation",
    join: "get_delegation_status",
    recovery_authorization: "request_recovery_authorization",
  },
  plan_setup_order: [
    "create-progress",
    "dispatch-plan-author",
    "confirm-plan-on-disk",
    "validate-routing",
    "review-plan",
    "register-simple-workflow",
    "sync-plan-tasks",
  ],
  document_work: {
    parent_edits: false,
    design_review: "conditional",
    design_reviewer: "independent_codex",
    design_fixer: "independent_codex",
    plan_author: "independent_codex",
    plan_reviewer: "independent_codex",
    producer_reviewer_independence: true,
    plan_rereview: "full_latest_plan",
    user_named_reviewers: "design_and_plan_only",
  },
  conversation_identity: {
    distinct_work_units: "distinct_child_conversations",
    continuation: "same_work_unit_only",
  },
  task_agent: {
    default_agent_type: "grok",
    selection_source: "invocation",
    explicit_substitution: "forbidden",
    change_boundary: "completed_tasks_after_plan_revision_and_full_rereview",
  },
  routing: {
    marker: "codeg-b2d-routing-v1",
    risk_policy_version: "b2d_task_risk_v1",
    normal: { implementer: "task_agent", reviewers: ["codex_primary"] },
    high: {
      implementer: "codex",
      reviewers: ["codex_primary", "task_agent_auxiliary"],
    },
    reviewer_slots: ["primary", "auxiliary"],
    task_order: "serial",
    high_review_fan_out: "parallel_after_implementation",
  },
  progress: {
    marker: "codeg-simple-progress-v1",
    mutation_order: [
      "record-reserving-intent",
      "delegate",
      "record-admission",
      "record-observed-state",
    ],
    route_metadata: "additive",
  },
  workspace_policy: "preserve-user-changes",
  recovery: {
    unexpected_continuations: 2,
    logical_replacements: 1,
    replacement_retry: "pre-admission-only",
  },
  final_review: {
    required: true,
    independent: true,
    reviewer: "codex",
    fix_owner: "task_producer",
  },
}

function block(marker, value) {
  return `<!-- ${marker}\n${JSON.stringify(value, null, 2)}\n-->`
}

const skill = `---
name: brainstorm-to-delivery
description: Use when a completed Brainstorm must become a local delivery.
---

# Brainstorm to Delivery

${block("codeg-b2d-skill-contract-v2", SKILL_CONTRACT)}

## 1. Establish current truth
Inspect current files and live delegation schemas. Preserve user decisions.

## 2. Resolve the Task Agent
Inspect discovery and choose the invocation selection. Record an omitted selection as Grok and block invalid identities.

## 3. Review and revise Design
Dispatch a conditional independent Codex Design Reviewer. Continue a separate Codex Design Fixer for every revision.

## 4. Author and review Plan
Create progress first. Dispatch an independent Codex Plan Author with writing-plans, validate routing, and use a separate Codex Plan Reviewer for full latest Plan review before registration.

## 5. Maintain progress
Record reserving intent, delegation, admission, and observed state in order. Keep route metadata.

## 6. Apply the workspace gate
Inspect status and diffs. Preserve user changes and request user-owned decisions.

## 7. Execute Tasks serially
Use subagent-driven-development. Run normal Tasks through the Task Agent and Codex primary reviewer. Run high Tasks through a Codex implementer, then fan out Codex primary and Task Agent auxiliary reviews. Return fixes to the owning producer and rerun every required review.

## 8. Recover generic runs
Continue only the same work unit and preserve its identity. Permit two unexpected continuations and request one logical replacement.

## 9. Complete final review
Dispatch an independent Codex final reviewer. Route findings to Task producers, run checks, and continue Task and final reviews.
`

function identity(agent_type = "grok", profile_id = null) {
  return { agent_type, profile_id }
}

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

function risk(level = "normal") {
  const value = {
    level,
    hard_triggers: [],
    soft_signals: [],
    score: 0,
    reason: "Concrete deterministic risk evidence.",
  }
  if (level === "high") {
    value.hard_triggers.push({
      kind: "public_compatibility",
      evidence: ["public contract"],
    })
  }
  return value
}

function task(index, level, taskAgent = identity(), generation = 1) {
  return {
    index,
    task_agent_generation: generation,
    risk: risk(level),
    route:
      level === "normal"
        ? {
            implementer: copy(taskAgent),
            reviewers: [
              { slot: "primary", agent_type: "codex", profile_id: null },
            ],
          }
        : {
            implementer: identity("codex"),
            reviewers: [
              { slot: "primary", agent_type: "codex", profile_id: null },
              { slot: "auxiliary", ...copy(taskAgent) },
            ],
          },
  }
}

function routing() {
  return {
    schema_version: 1,
    risk_policy_version: "b2d_task_risk_v1",
    task_agent_generations: [
      {
        generation: 1,
        agent_type: "grok",
        profile_id: null,
        effective_from_task_index: 1,
      },
    ],
    tasks: [task(1, "normal"), task(2, "high")],
  }
}

function plan(snapshot = routing()) {
  return `# Plan

${block("codeg-b2d-routing-v1", snapshot)}

## Task 1: Parse documents

### Task 2: Project progress
`
}

function run(key, state, task_id, child_conversation_id) {
  const parts = key.split("|")
  const slotted = parts.length === 6
  const profile = parts[slotted ? 5 : 4]
  return {
    role: parts[2],
    agent_type: parts[slotted ? 4 : 3],
    profile_id: profile === "none" ? null : profile,
    task_id,
    child_conversation_id,
    state,
    work_unit_key: key,
    recovery_count: 0,
    replaced_task_id: null,
    replacement_reason: null,
  }
}

function progressTask(routeTask, status, child = 10) {
  const taskAgent =
    routeTask.risk.level === "normal"
      ? (routeTask.route?.implementer ?? identity())
      : (routeTask.route?.reviewers?.[1] ?? identity())
  const keys = expectedWorkUnitKeys(
    routeTask.index,
    routeTask.risk.level,
    taskAgent
  )
  const runs = []
  if (status === "completed") {
    runs.push(
      run(keys.implementer, "completed", `t${routeTask.index}-i`, child)
    )
    runs.push(
      run(
        keys.reviewers.primary,
        "completed",
        `t${routeTask.index}-p`,
        child + 1
      )
    )
    if (keys.reviewers.auxiliary) {
      runs.push(
        run(
          keys.reviewers.auxiliary,
          "completed",
          `t${routeTask.index}-a`,
          child + 2
        )
      )
    }
  }
  return {
    index: routeTask.index,
    status,
    commit: status === "completed" ? `commit-${routeTask.index}` : null,
    risk_level: routeTask.risk.level,
    task_agent_generation: routeTask.task_agent_generation,
    expected_work_unit_keys: keys,
    runs,
  }
}

function progress(snapshot = routing()) {
  return {
    schema_version: 1,
    plan_rel_path: planRelPath,
    active_task_index: null,
    tasks: [
      progressTask(snapshot.tasks[0], "completed"),
      progressTask(snapshot.tasks[1], "pending", 20),
    ],
    final_review_status: "pending",
    updated_at: "2026-08-16T00:00:00Z",
  }
}

function validate(
  snapshot = routing(),
  state = progress(snapshot),
  planMarkdown = plan(snapshot)
) {
  return validateSimpleDocuments({
    skillMarkdown: skill,
    planMarkdown,
    progressMarkdown: `# Progress\n\n${block(
      "codeg-simple-progress-v1",
      state
    )}\n`,
    planRelPath,
  }).failures
}

function has(failures, rule) {
  assert.ok(
    failures.some((failure) => failure.startsWith(`[${rule}]`)),
    `expected ${rule}; got ${failures.join("; ")}`
  )
}

describe("Skill contract v2", () => {
  it("accepts the exact nine-phase ownership contract and production Skill", () => {
    assert.deepEqual(validateSkillMarkdown(skill).failures, [])
    assert.deepEqual(validateSkillMarkdown(realSkill).failures, [])
  })

  it("requires exactly one unfenced v2 contract and nine phases", () => {
    has(
      validateSkillMarkdown(skill.replace("v2", "v1")).failures,
      "B2D-SKILL-004"
    )
    has(
      validateSkillMarkdown(
        `${skill}\n${block("codeg-b2d-skill-contract-v2", SKILL_CONTRACT)}`
      ).failures,
      "B2D-SKILL-004"
    )
    assert.deepEqual(
      validateSkillMarkdown(
        skill.replace(
          "# Brainstorm to Delivery",
          `# Brainstorm to Delivery\n\n\`\`\`md\n${block("codeg-b2d-skill-contract-v2", SKILL_CONTRACT)}\n\`\`\``
        )
      ).failures,
      []
    )
    has(
      validateSkillMarkdown(skill.replace("## 5.", "## 10.")).failures,
      "B2D-SKILL-004"
    )
  })

  it("bans retired mutation identifiers", () => {
    for (const name of [
      "get_workflow_capabilities",
      "get_workflow_state",
      "publish_workflow_manifest",
      "settle_workflow_gate",
      "recover_workflow",
      "complete_work",
      "publication_token",
      "manifest_revision",
      "graph_revision",
      "gate_id",
      "artifact_digest",
      "reviewed_task_id",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${name}`).failures, "B2D-SKILL-003")
    }
  })

  it("rejects contradictory ownership and route prose", () => {
    for (const prose of [
      "The parent revises the Plan directly.",
      "Always use Grok as the implementer.",
      "Use the Task Agent to implement high Tasks.",
      "Reuse one Codex conversation for implementation and review.",
      "Switch Agent immediately inside the active Task.",
      "Skip the auxiliary review after a high-Task fix.",
      "When the user names a Design Reviewer, use that reviewer instead of the Codex Design Reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })
})

describe("bounded routing extraction", () => {
  it("parses one exact unfenced block with ordered headings", () => {
    const parsed = parseSimplePlan(plan())
    assert.deepEqual(parsed.failures, [])
    assert.deepEqual(
      parsed.tasks.map(({ index, title }) => ({ index, title })),
      [
        { index: 1, title: "Parse documents" },
        { index: 2, title: "Project progress" },
      ]
    )
    assert.equal(parsed.routing.schema_version, 1)
  })

  it("rejects missing, duplicate, truncated, oversized, invalid, and fenced-only blocks", () => {
    has(parseSimpleRouting("# none").failures, "B2D-ROUTING-001")
    has(parseSimpleRouting(`${plan()}\n${plan()}`).failures, "B2D-ROUTING-001")
    has(
      parseSimpleRouting("<!-- codeg-b2d-routing-v1\n{").failures,
      "B2D-ROUTING-001"
    )
    has(
      parseSimpleRouting(
        `<!-- codeg-b2d-routing-v1\n${"x".repeat(
          MAX_ROUTING_BLOCK_BYTES + 1
        )}\n-->`
      ).failures,
      "B2D-ROUTING-002"
    )
    has(
      parseSimpleRouting("<!-- codeg-b2d-routing-v1\nnope\n-->").failures,
      "B2D-ROUTING-003"
    )
    has(
      parseSimpleRouting("~~~md\n<!-- codeg-b2d-routing-v1\n{}\n-->\n~~~")
        .failures,
      "B2D-ROUTING-001"
    )
  })
})

describe("risk, generation, and exact routes", () => {
  it("requires a non-empty serialized Task Agent generation array", () => {
    const snapshot = routing()
    delete snapshot.task_agent_generations
    has(validate(snapshot), "B2D-ROUTING-006")

    const empty = routing()
    empty.task_agent_generations = []
    has(validate(empty), "B2D-ROUTING-006")
  })

  it("accepts every built-in and valid custom identity/profile", () => {
    for (const selected of [
      identity("claude_code"),
      identity("codex"),
      identity("open_code"),
      identity("gemini"),
      identity("cline"),
      identity("hermes"),
      identity("code_buddy"),
      identity("kimi_code"),
      identity("pi"),
      identity("grok"),
      identity("cursor"),
      identity("custom:reviewer-x", "deep"),
    ]) {
      const snapshot = routing()
      snapshot.task_agent_generations[0] = {
        generation: 1,
        ...selected,
        effective_from_task_index: 1,
      }
      snapshot.tasks = [task(1, "normal", selected), task(2, "high", selected)]
      assert.deepEqual(validate(snapshot), [], selected.agent_type)
    }
  })

  it("rejects malformed, reserved, unavailable, ambiguous, and literal-none identities", () => {
    for (const selected of [
      identity("custom:codex"),
      identity("custom:Bad"),
      identity("auto"),
      identity("unavailable"),
      identity("ambiguous"),
      identity("grok", "none"),
      identity("grok", ""),
      identity("grok|bad"),
      identity("grok", "x".repeat(201)),
    ]) {
      const snapshot = routing()
      snapshot.task_agent_generations[0] = {
        generation: 1,
        ...selected,
        effective_from_task_index: 1,
      }
      has(validate(snapshot), "B2D-ROUTING-005")
    }
  })

  it("reports multiple malformed generation entries without throwing", () => {
    const snapshot = routing()
    snapshot.task_agent_generations = [
      null,
      {
        generation: 2,
        ...identity("gemini"),
        effective_from_task_index: 2,
      },
    ]
    assert.doesNotThrow(() => validate(snapshot))
    has(validate(snapshot), "B2D-ROUTING-005")
  })

  it("requires contiguous generations and exact effective boundaries", () => {
    for (const mutate of [
      (s) => {
        s.task_agent_generations[0].generation = 2
      },
      (s) => {
        s.task_agent_generations.push({
          generation: 3,
          ...identity("gemini"),
          effective_from_task_index: 2,
        })
      },
      (s) => {
        s.task_agent_generations.push({
          generation: 2,
          ...identity("gemini"),
          effective_from_task_index: 1,
        })
      },
      (s) => {
        s.task_agent_generations.push({
          generation: 2,
          ...identity("gemini"),
          effective_from_task_index: 2,
        })
        s.tasks[1].task_agent_generation = 1
      },
    ]) {
      const snapshot = routing()
      mutate(snapshot)
      has(validate(snapshot), "B2D-ROUTING-006")
    }
  })

  it("rejects a Task that routes back to an earlier generation", () => {
    const snapshot = routing()
    snapshot.task_agent_generations.push({
      generation: 2,
      ...identity("gemini"),
      effective_from_task_index: 2,
    })
    snapshot.tasks[1] = task(2, "normal", identity("gemini"), 2)
    snapshot.tasks.push(task(3, "normal", identity(), 1))
    const routedPlan = `${plan(snapshot)}\n## Task 3: Revert generation\n`
    const parsed = parseSimplePlan(routedPlan)
    const failures = []
    validateRoutingSnapshot(parsed.routing, parsed, failures)
    has(failures, "B2D-ROUTING-006")
  })

  it("forces high for all six hard triggers with evidence", () => {
    for (const kind of [
      "concurrency_lifecycle",
      "security_trust_boundary",
      "migration_destructive_persistence",
      "public_compatibility",
      "unsafe_ffi",
      "update_rollback",
    ]) {
      const snapshot = routing()
      snapshot.tasks[0].risk.hard_triggers = [{ kind, evidence: ["specific"] }]
      has(validate(snapshot), "B2D-RISK-004")
      snapshot.tasks[0] = task(1, "high")
      snapshot.tasks[0].risk.hard_triggers = [{ kind, evidence: ["specific"] }]
      assert.deepEqual(validate(snapshot), [], kind)
      snapshot.tasks[0].risk.hard_triggers[0].evidence = []
      has(validate(snapshot), "B2D-RISK-002")
    }
  })

  it("classifies soft totals 0..2 normal and 3+ high", () => {
    for (const [kinds, level] of [
      [[], "normal"],
      [["shared_interface"], "normal"],
      [["cross_runtime_or_process"], "normal"],
      [["cross_runtime_or_process", "shared_interface"], "high"],
    ]) {
      const snapshot = routing()
      snapshot.tasks[0] = task(1, level)
      snapshot.tasks[0].risk.hard_triggers = []
      snapshot.tasks[0].risk.soft_signals = kinds.map((kind) => ({
        kind,
        score: kind === "cross_runtime_or_process" ? 2 : 1,
        evidence: [kind],
      }))
      snapshot.tasks[0].risk.score = snapshot.tasks[0].risk.soft_signals.reduce(
        (sum, signal) => sum + signal.score,
        0
      )
      assert.deepEqual(
        validate(snapshot),
        [],
        String(snapshot.tasks[0].risk.score)
      )
    }
  })

  it("rejects unknown, duplicate, evidence-free, wrong-score, wrong-total, contradictory, and empty risk", () => {
    for (const [rule, mutate] of [
      [
        "B2D-RISK-001",
        (r) =>
          (r.soft_signals = [{ kind: "unknown", score: 1, evidence: ["x"] }]),
      ],
      [
        "B2D-RISK-001",
        (r) =>
          (r.soft_signals = [
            { kind: "shared_interface", score: 1, evidence: ["x"] },
            { kind: "shared_interface", score: 1, evidence: ["y"] },
          ]),
      ],
      [
        "B2D-RISK-002",
        (r) =>
          (r.soft_signals = [
            { kind: "shared_interface", score: 1, evidence: [] },
          ]),
      ],
      [
        "B2D-RISK-003",
        (r) => {
          r.soft_signals = [
            { kind: "shared_interface", score: 2, evidence: ["x"] },
          ]
          r.score = 2
        },
      ],
      [
        "B2D-RISK-003",
        (r) => {
          r.soft_signals = [
            { kind: "shared_interface", score: 1, evidence: ["x"] },
          ]
          r.score = 2
        },
      ],
      ["B2D-RISK-004", (r) => (r.level = "high")],
      ["B2D-RISK-005", (r) => (r.reason = "")],
    ]) {
      const snapshot = routing()
      mutate(snapshot.tasks[0].risk)
      has(validate(snapshot), rule)
    }
  })

  it("rejects one evidence string counted by multiple soft signals", () => {
    const snapshot = routing()
    snapshot.tasks[0].risk.soft_signals = [
      { kind: "shared_interface", score: 1, evidence: ["same fact"] },
      { kind: "dependency_or_build", score: 1, evidence: ["same fact"] },
    ]
    snapshot.tasks[0].risk.score = 2
    has(validate(snapshot), "B2D-RISK-001")
  })

  it("requires exact profile, order, slots, count, identities, and structured route", () => {
    for (const mutate of [
      (t) => (t.route.implementer.profile_id = "wrong"),
      (t) => (t.route.reviewers[0].slot = "auxiliary"),
      (t) => t.route.reviewers.push(copy(t.route.reviewers[0])),
      (t) => (t.route.reviewers = []),
      (t) => (t.route = "free-form"),
      (t) => t.route.reviewers.reverse(),
    ]) {
      const snapshot = routing()
      mutate(snapshot.tasks[1])
      has(validate(snapshot), "B2D-ROUTING-009")
    }
  })

  it("matches routing indices to headings and derives explicit keys", () => {
    const bad = routing()
    bad.tasks[1].index = 3
    has(validate(bad), "B2D-ROUTING-004")
    const failures = []
    const expected = deriveExpectedRoute(
      routing().tasks[1],
      routing().task_agent_generations[0],
      failures
    )
    assert.deepEqual(failures, [])
    assert.deepEqual(
      expected.expected_work_unit_keys,
      expectedWorkUnitKeys(2, "high", identity())
    )
  })

  it("rejects maximum Agent/profile selections whose implementer key exceeds the canonical limit", () => {
    const selected = identity(`custom:${"a".repeat(64)}`, "p".repeat(128))
    const failures = []
    const derived = deriveExpectedRoute(
      task(0xffffffff, "normal", selected),
      { generation: 1, ...selected, effective_from_task_index: 1 },
      failures
    )
    assert.equal(derived, null)
    has(failures, "B2D-ROUTING-009")
  })

  it("rejects maximum Agent/profile selections whose slotted reviewer key exceeds the canonical limit", () => {
    const selected = identity(`custom:${"a".repeat(64)}`, "p".repeat(128))
    const failures = []
    const derived = deriveExpectedRoute(
      task(0xffffffff, "high", selected),
      { generation: 1, ...selected, effective_from_task_index: 1 },
      failures
    )
    assert.equal(derived, null)
    has(failures, "B2D-ROUTING-009")
  })
})

describe("progress agreement and per-key lineage", () => {
  it("validates the complete routed fixture", () => {
    assert.deepEqual(validate(), [])
  })

  it("keeps generic primary and auxiliary reviewer lineages separate", () => {
    const route = routing()
    const state = progress(route)
    state.tasks[1] = progressTask(route.tasks[1], "completed", 20)
    assert.ok(
      state.tasks[1].runs
        .filter((entry) => entry.role === "reviewer")
        .every((entry) => entry.role === "reviewer")
    )
    assert.deepEqual(validate(route, state), [])
  })

  it("rejects risk, generation, implementer, primary, and auxiliary mismatches", () => {
    for (const mutate of [
      (p) => (p.tasks[0].risk_level = "high"),
      (p) => (p.tasks[0].task_agent_generation = 2),
      (p) =>
        (p.tasks[0].expected_work_unit_keys.implementer =
          "task|1|implementer|codex|none"),
      (p) =>
        (p.tasks[0].expected_work_unit_keys.reviewers.primary =
          "task|1|reviewer|codex|none"),
      (p) => (p.tasks[1].expected_work_unit_keys.reviewers.auxiliary = null),
    ]) {
      const state = progress()
      mutate(state)
      has(validate(routing(), state), "B2D-PROGRESS-009")
    }
  })

  it("requires every expected completed key and rejects outside keys", () => {
    const missing = progress()
    missing.tasks[0].runs.pop()
    has(validate(routing(), missing), "B2D-PROGRESS-010")
    const extra = progress()
    extra.tasks[0].runs.push(
      run("task|1|reviewer|auxiliary|grok|none", "completed", "extra", 99)
    )
    has(validate(routing(), extra), "B2D-PROGRESS-009")
  })

  it("requires admission identity on every terminal completed lineage", () => {
    for (const mutate of [
      (entry) => (entry.task_id = ""),
      (entry) => (entry.child_conversation_id = null),
      (entry) => (entry.child_conversation_id = 0),
    ]) {
      const state = progress()
      mutate(state.tasks[0].runs[0])
      has(validate(routing(), state), "B2D-PROGRESS-010")
    }
  })

  it("enforces stable identity and one replacement per exact key", () => {
    const state = progress()
    const key = state.tasks[0].expected_work_unit_keys.implementer
    const source = state.tasks[0].runs[0]
    source.state = "failed"
    state.tasks[0].runs.push({
      ...run(key, "failed", "replacement-1", null),
      replaced_task_id: source.task_id,
      replacement_reason: "unresumable",
    })
    state.tasks[0].runs.push({
      ...run(key, "completed", "replacement-2", 90),
      replaced_task_id: "replacement-1",
      replacement_reason: "unresumable",
    })
    has(validate(routing(), state), "B2D-PROGRESS-006")
    state.tasks[0].runs[1].agent_type = "codex"
    has(validate(routing(), state), "B2D-PROGRESS-006")
  })

  it("requires globally unique task IDs and child IDs across distinct keys", () => {
    const taskIds = progress()
    taskIds.tasks[0].runs[1].task_id = taskIds.tasks[0].runs[0].task_id
    has(validate(routing(), taskIds), "B2D-PROGRESS-006")
    const children = progress()
    children.tasks[0].runs[1].child_conversation_id =
      children.tasks[0].runs[0].child_conversation_id
    has(validate(routing(), children), "B2D-PROGRESS-006")
  })

  it("reads legacy reviewer as primary but requires explicit routed primary", () => {
    const legacyProgress = {
      schema_version: 1,
      plan_rel_path: planRelPath,
      active_task_index: null,
      tasks: [
        {
          index: 1,
          status: "completed",
          commit: "c",
          runs: [
            run("task|1|implementer|grok|none", "completed", "li", 1),
            run("task|1|reviewer|codex|none", "completed", "lr", 2),
          ],
        },
      ],
      final_review_status: "pending",
      updated_at: null,
    }
    assert.deepEqual(
      validateSimpleDocuments({
        skillMarkdown: skill,
        planMarkdown: "# Plan\n\n## Task 1: Legacy\n",
        progressMarkdown: `# Progress\n${block(
          "codeg-simple-progress-v1",
          legacyProgress
        )}`,
        planRelPath,
      }).failures,
      []
    )
    const routed = progress()
    routed.tasks[0].runs[1].work_unit_key = "task|1|reviewer|codex|none"
    has(validate(routing(), routed), "B2D-PROGRESS-009")
  })

  it("accepts an adopted generation throughout its Task lifecycle and the following Task", () => {
    const route = routing()
    const selected = identity("gemini", "careful")
    route.task_agent_generations.push({
      generation: 2,
      ...selected,
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "normal", selected, 2)
    route.tasks.push(task(3, "high", selected, 2))
    const routedPlan = `${plan(route)}\n## Task 3: Continue generation\n`
    const state = progress(route)
    state.tasks.push(progressTask(route.tasks[2], "pending", 30))

    assert.deepEqual(validate(route, state, routedPlan), [], "pre-admission")

    const implementer = state.tasks[1].expected_work_unit_keys.implementer
    state.tasks[1].status = "in_progress"
    state.active_task_index = 2
    state.tasks[1].runs.push(run(implementer, "running", "t2-i", 20))
    assert.deepEqual(validate(route, state, routedPlan), [], "admitted/active")

    state.tasks[1].runs[0].state = "completed"
    state.tasks[1].runs.push(
      run(
        state.tasks[1].expected_work_unit_keys.reviewers.primary,
        "running",
        "t2-p",
        21
      )
    )
    assert.deepEqual(
      validate(route, state, routedPlan),
      [],
      "reviewer dispatch"
    )

    state.tasks[1].runs[1].state = "completed"
    state.tasks[1].status = "completed"
    state.tasks[1].commit = "commit-2"
    state.active_task_index = null
    assert.deepEqual(validate(route, state, routedPlan), [], "completed")

    state.tasks[2].status = "in_progress"
    state.active_task_index = 3
    state.tasks[2].runs.push(
      run(
        state.tasks[2].expected_work_unit_keys.implementer,
        "running",
        "t3-i",
        30
      )
    )
    assert.deepEqual(validate(route, state, routedPlan), [], "following Task")
  })

  it("rejects historical generation adoption by admitted reviewer-only runs", () => {
    const route = routing()
    const selected = identity("gemini", "careful")
    route.task_agent_generations.push({
      generation: 2,
      ...selected,
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "high", selected, 2)

    for (const reviewerSlots of [
      ["primary"],
      ["auxiliary"],
      ["primary", "auxiliary"],
    ]) {
      const state = progress(route)
      state.tasks[1].status = "in_progress"
      state.active_task_index = 2
      state.tasks[1].runs = reviewerSlots.map((slot, index) =>
        run(
          state.tasks[1].expected_work_unit_keys.reviewers[slot],
          "running",
          `t2-${slot}`,
          20 + index
        )
      )

      has(validate(route, state), "B2D-ROUTING-007")
    }
  })

  it("adopts a later generation only at an empty pending boundary", () => {
    const route = routing()
    const selected = identity("gemini", "careful")
    route.task_agent_generations.push({
      generation: 2,
      ...selected,
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "normal", selected, 2)
    for (const mutate of [
      (p) => {
        p.tasks[1].status = "in_progress"
        p.active_task_index = 2
      },
      (p) => {
        p.tasks[1].status = "blocked"
        p.active_task_index = 2
      },
      (p) => {
        p.tasks[1].runs = [
          run(
            p.tasks[1].expected_work_unit_keys.implementer,
            "reserving",
            "reserved",
            null
          ),
        ]
      },
      (p) => {
        p.tasks[1].runs = [
          run(
            p.tasks[1].expected_work_unit_keys.implementer,
            "running",
            "admitted",
            20
          ),
        ]
      },
      (p) => {
        p.tasks[0].status = "pending"
        p.tasks[0].runs = []
      },
    ]) {
      const state = progress(route)
      mutate(state)
      has(validate(route, state), "B2D-ROUTING-007")
    }
  })

  it("exposes pure routing/progress agreement functions", () => {
    const parsed = parseSimplePlan(plan())
    const failures = []
    const normalized = validateRoutingSnapshot(parsed.routing, parsed, failures)
    validateProgressRouting(progress(), normalized, failures)
    assert.deepEqual(failures, [])
  })
})
