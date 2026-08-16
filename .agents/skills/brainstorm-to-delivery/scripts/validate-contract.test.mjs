import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { describe, it } from "node:test"
import { fileURLToPath } from "node:url"
import {
  MAX_ROUTING_BLOCK_BYTES,
  deriveExpectedRoute,
  parseSimplePlan,
  parseSimpleProgress,
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

function assertSkillClassifications(cases) {
  const mismatches = []
  for (const { prose, reject } of cases) {
    const failures = validateSkillMarkdown(`${skill}\n${prose}`).failures
    const rejected = failures.some((failure) =>
      failure.startsWith("[B2D-SKILL-005]")
    )
    if (rejected !== reject) {
      mismatches.push(
        `${reject ? "expected rejection" : "expected acceptance"}: ${prose}`
      )
    }
  }
  assert.deepEqual(mismatches, [])
}

function fencedJsonAfterHeading(markdown, heading) {
  const headingIndex = markdown.indexOf(heading)
  assert.notEqual(headingIndex, -1, `missing Skill heading: ${heading}`)
  const match = markdown
    .slice(headingIndex + heading.length)
    .match(/\n```json\n([\s\S]*?)\n```/)
  assert.ok(match, `missing JSON contract after: ${heading}`)
  return JSON.parse(match[1])
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

  it("rejects bounded ownership and route directive paraphrases", () => {
    for (const prose of [
      "The parent writes and revises every Plan.",
      "Grok implements every Task, including high Tasks.",
      "The parent authors all Design documents and Plans.",
      "The Task Agent owns implementation for each high-risk Task.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    assert.deepEqual(
      validateSkillMarkdown(
        `${skill}\nKeep the parent from writing the Plan. Route a selected Grok Agent only to normal Tasks.`
      ).failures,
      []
    )
  })

  for (const [name, prose] of [
    [
      "passive parent ownership",
      "The Plan is written and updated by the parent.",
    ],
    [
      "passive Task Agent route",
      "High Tasks are implemented by the Task Agent.",
    ],
    ["running Task switch", "Switch the Task Agent while a Task is running."],
    [
      "optional auxiliary review",
      "After fixing a high Task, auxiliary review is optional.",
    ],
    ["normal primary bypass", "Skip primary review on normal Tasks."],
    [
      "omitted auxiliary reviewer",
      "The auxiliary reviewer may be omitted after a high Task fix.",
    ],
    ["direct high route", "Route high Tasks to the Task Agent."],
    ["direct high delegation", "Delegate all high-risk Tasks to Grok."],
    ["current Task switch", "Switch the Task Agent during the current Task."],
  ]) {
    it(`rereview rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "passive parent ownership",
      "The Plan must not be written or updated by the parent.",
    ],
    ["high route", "Never route high Tasks to the Task Agent."],
    ["high delegation", "Do not delegate any high-risk Task to Grok."],
    [
      "running switch",
      "Do not switch the Task Agent while the current Task is running.",
    ],
    [
      "auxiliary review",
      "Auxiliary review is not optional and may not be omitted after a high Task fix.",
    ],
    ["normal primary review", "Never skip primary review on normal Tasks."],
  ]) {
    it(`rereview permits explicit prohibition of ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "parent orchestration",
      "The parent directs the Plan Author to update the Plan.",
    ],
    [
      "Codex implementation before Task Agent review",
      "High Tasks are implemented by Codex and reviewed by the Task Agent.",
    ],
    [
      "Task Agent review of Codex implementation",
      "The Task Agent reviews work implemented by Codex for high Tasks.",
    ],
    [
      "explicit Task Agent route exclusion",
      "Route high Tasks to Codex, not to the Task Agent.",
    ],
    [
      "completed Task boundary switch",
      "Switch the Task Agent after the current Task completes.",
    ],
    [
      "optional user reviewer cannot replace Codex",
      "Optional user-named Design reviewers do not replace the Codex Design Reviewer.",
    ],
    [
      "required primary review with optional document reviewers",
      "Primary review remains required; optional user-named Design reviewers are document-only.",
    ],
  ]) {
    it(`round-2 accepts compliant ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "normal Task Agent implementation",
      "The Task Agent implements every normal Task.",
    ],
    [
      "high auxiliary review route",
      "Route every high Task auxiliary review to the Task Agent.",
    ],
    [
      "once-complete boundary",
      "Switch the Task Agent once the current Task completes.",
    ],
    [
      "following-completion boundary",
      "Switch the Task Agent following completion of the current Task.",
    ],
    [
      "mandatory primary review",
      "Primary review is mandatory rather than optional.",
    ],
    [
      "required non-omittable primary review",
      "Primary review is required and cannot be omitted.",
    ],
    [
      "semicolon-separated optional document review",
      "Primary review is required; user-named Design reviewers are optional.",
    ],
    [
      "completion-before-current-Task boundary",
      "Switch the Task Agent after completion of the current Task.",
    ],
    [
      "once-finished boundary",
      "Switch the Task Agent once the current Task finishes.",
    ],
    [
      "normal implementation and high auxiliary review",
      "The Task Agent implements normal Tasks and reviews high Tasks implemented by Codex.",
    ],
  ]) {
    it(`round-3 accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "parent and Plan Author co-ownership",
      "The parent and the Plan Author revise the Plan.",
    ],
    [
      "parent revision after delegation",
      "The parent directs the Plan Author and then revises the Plan.",
    ],
    [
      "Task Agent route after Codex exclusion",
      "Route high Tasks not to Codex but to the Task Agent.",
    ],
    [
      "Task Agent implementation after Codex exclusion",
      "High Tasks are not implemented by Codex but by the Task Agent.",
    ],
    [
      "active switch with later completion timing",
      "Switch the Task Agent during the current Task and after the current Task completes.",
    ],
    [
      "optional document reviewer replacement",
      "Optional user-named Design reviewers replace the Codex reviewer.",
    ],
    [
      "semicolon-separated auxiliary bypass",
      "Never skip primary review; skip auxiliary review after a high Task fix.",
    ],
    [
      "high implementation despite Codex review",
      "The Task Agent implements work reviewed by Codex for high Tasks.",
    ],
    ["joint high route", "Route high Tasks to Codex and to the Task Agent."],
    [
      "high review and implementation co-ownership",
      "The Task Agent reviews high Tasks and implements them.",
    ],
  ]) {
    it(`round-3 rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "optional user-named reviewers with irreplaceable Codex reviewer",
      "User-named Design reviewers are optional and do not replace the Codex reviewer.",
    ],
    [
      "optional user-named reviewers with required Codex reviewer",
      "User-named Design reviewers are optional and the Codex reviewer is required.",
    ],
  ]) {
    it(`round-3 reviewer attachment accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "optional Codex reviewer with irreplaceable user-named reviewers",
      "The Codex reviewer is optional and user-named Design reviewers cannot replace it.",
    ],
    [
      "optional primary Codex reviewer with optional user-named reviewers",
      "The primary Codex reviewer is optional and user-named Design reviewers remain optional.",
    ],
  ]) {
    it(`round-3 reviewer attachment rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "coordinated producer predicates",
      "The parent directs the Plan Author to revise and update the Plan.",
    ],
    [
      "high auxiliary-review route purpose",
      "Route high Tasks to the Task Agent for auxiliary review.",
    ],
    [
      "high review route purpose",
      "Route high Tasks to the Task Agent for review.",
    ],
    [
      "completed-state Task boundary",
      "Change the Task Agent when the current Task is complete.",
    ],
    [
      "negated active-state Task boundary",
      "Change the Task Agent only while no Task is running.",
    ],
    [
      "parenthetical optional document reviewers",
      "User-named Design reviewers, although optional, cannot replace the Codex reviewer.",
    ],
    [
      "parent delegation to Plan Author",
      "The parent delegates the Plan Author to write the Plan.",
    ],
    [
      "parent routing to Design Fixer",
      "The parent routes the Design Fixer to fix the Design.",
    ],
    ["avoid primary-review bypass", "Avoid skipping primary review."],
  ]) {
    it(`round-4 accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "Task Agent coordinated high implementation",
      "The Task Agent implements and reviews high Tasks.",
    ],
    [
      "Task Agent shared passive actor after implementation",
      "High Tasks are implemented and reviewed by the Task Agent.",
    ],
    [
      "Task Agent shared passive actor before implementation",
      "High Tasks are reviewed and implemented by the Task Agent.",
    ],
    [
      "Codex reviewer pronoun replacement after contrast",
      "The Codex reviewer remains required, but user-named Design reviewers may replace it.",
    ],
    [
      "Codex reviewer pronoun replacement after coordination",
      "The Codex reviewer is required and user-named Design reviewers replace it.",
    ],
    [
      "passive primary Codex reviewer replacement",
      "The primary Codex reviewer is required but can be replaced by user-named Design reviewers.",
    ],
    [
      "Task Agent self-route for high Tasks",
      "The Task Agent routes high Tasks to itself.",
    ],
    [
      "Task Agent primary-review route for high Tasks",
      "Route a high Task to Grok for primary review.",
    ],
    [
      "coordinated Task Agent high implementation subject",
      "The Task Agent and Codex implement high Tasks.",
    ],
    ["Codex normal implementation", "Codex implements normal Tasks."],
    ["Task Agent normal review", "The Task Agent reviews normal Tasks."],
    ["optional primary review polarity", "Primary review is not required."],
    ["absent primary review polarity", "No primary review is required."],
    ["optional Codex reviewer polarity", "The Codex reviewer is not required."],
    [
      "Codex reviewer in-place substitution",
      "Use optional user-named Design reviewers in place of the Codex reviewer.",
    ],
    [
      "completed switch with active-state override",
      "Switch the Task Agent after the current Task completes, but the current Task is active.",
    ],
  ]) {
    it(`round-4 rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "explicit repeated Plan Author subject",
      "The parent delegates the Plan Author to write the Plan and the Plan Author updates the Plan.",
    ],
    [
      "done Task boundary",
      "Switch the Task Agent after the current Task is done.",
    ],
    [
      "on-completion Task boundary",
      "Switch the Task Agent on completion of the current Task.",
    ],
    [
      "upon-completion Task boundary",
      "Switch the Task Agent upon completion of the current Task.",
    ],
    [
      "though-optional reviewer prohibition",
      "User-named Design reviewers, though optional, cannot replace the Codex reviewer.",
    ],
  ]) {
    it(`round-6 accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "finite parent revision after delegated infinitive",
      "The parent asks the Plan Author to revise the Plan and revises it too.",
    ],
    [
      "finite parent update after delegated infinitive",
      "The parent directs the Plan Author to revise the Plan, then updates the Design.",
    ],
    [
      "finite parent edit after and-then delegated infinitive",
      "The parent tells the Plan Author to update the Plan and then edits the Design itself.",
    ],
    [
      "finite parent edit after delegated Plan write",
      "The parent delegates the Plan Author to write the Plan and then edits the Plan.",
    ],
    [
      "Task Agent high implementation before contrast",
      "The Task Agent implements but does not review high Tasks.",
    ],
    [
      "Task Agent shared passive high implementation before contrast",
      "High Tasks are implemented but not reviewed by the Task Agent.",
    ],
    [
      "Codex mixed normal and high implementation",
      "Codex implements normal and high Tasks.",
    ],
    [
      "Task Agent mixed normal and high review",
      "The Task Agent reviews normal and high Tasks.",
    ],
    [
      "Task Agent primary-slot high review route",
      "Route a high Task to Grok for review in the primary slot.",
    ],
    [
      "Task Agent primary-reviewer high review route",
      "Route a high Task to Grok for review as the primary reviewer.",
    ],
    [
      "Task Agent orchestration of high Task",
      "The Task Agent routes high Tasks to Codex.",
    ],
    [
      "Codex high auxiliary-review route",
      "Route high Tasks to Codex for auxiliary review.",
    ],
    [
      "Task Agent normal auxiliary-review route",
      "Route every normal Task auxiliary review to the Task Agent.",
    ],
    [
      "Codex leading high auxiliary-review route",
      "Route auxiliary review of high Tasks to Codex.",
    ],
    [
      "preposed running Task switch",
      "While the current Task is running, change the Task Agent.",
    ],
    [
      "preposed active Task switch",
      "During an active Task, switch the Task Agent.",
    ],
    [
      "preposed current active Task switch",
      "The current Task is active when you switch the Task Agent.",
    ],
    [
      "switch before Task completion",
      "Switch the Task Agent before the current Task completes.",
    ],
    [
      "switch immediately before Task finish",
      "Switch the Task Agent immediately before the current Task finishes.",
    ],
    [
      "Codex reviewer replacement after yet",
      "The Codex reviewer is required, yet user-named Design reviewers may replace it.",
    ],
    [
      "plural Codex reviewer replacement",
      "The primary Codex reviewers are required, but user-named Design reviewers may replace them.",
    ],
    [
      "demonstrative Codex reviewer replacement",
      "The Codex reviewer is required, but user-named Design reviewers may replace that reviewer.",
    ],
    [
      "Codex reviewer replacement in the place of",
      "Use optional user-named Design reviewers in the place of the Codex reviewer.",
    ],
    [
      "Codex reviewer replacement after while",
      "The Codex reviewer is required, while user-named Design reviewers may replace it.",
    ],
    [
      "Codex reviewer replacement after semicolon",
      "The Codex reviewer remains required; user-named Design reviewers may replace it.",
    ],
    [
      "Codex reviewer used-instead-of replacement",
      "Optional user-named Design reviewers are used instead of the Codex reviewer.",
    ],
  ]) {
    it(`round-6 rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  it("rejects high Task Agent implementation with explicit passive actors", () => {
    has(
      validateSkillMarkdown(
        `${skill}\nHigh Tasks are reviewed by Codex but implemented by Grok.`
      ).failures,
      "B2D-SKILL-005"
    )
  })

  for (const [name, prose] of [
    [
      "high Task Agent implementation after conjunction",
      "High Tasks are reviewed by Codex and implemented by Grok.",
    ],
    [
      "normal Codex implementation after contrast",
      "Normal Tasks are reviewed by Grok but implemented by Codex.",
    ],
  ]) {
    it(`passive actor relation pressure rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "high Codex implementation after Task Agent review",
      "High Tasks are reviewed by the Task Agent but implemented by Codex.",
    ],
    [
      "high Codex implementation after Codex review",
      "High Tasks are reviewed by Codex and implemented by Codex.",
    ],
    [
      "normal Task Agent implementation after Codex review",
      "Normal Tasks are reviewed by Codex but implemented by Grok.",
    ],
  ]) {
    it(`passive actor relation pressure accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  it("nearby relation pressure accepts a coordinated auxiliary-review dispatch", () => {
    assert.deepEqual(
      validateSkillMarkdown(
        `${skill}\nRoute high Tasks to Codex and delegate their auxiliary review to the Task Agent.`
      ).failures,
      []
    )
  })

  it("nearby relation pressure rejects a parent edit after producer advice", () => {
    has(
      validateSkillMarkdown(
        `${skill}\nThe parent revises the Plan, asks the Plan Author for advice, and then updates the Design.`
      ).failures,
      "B2D-SKILL-005"
    )
  })

  for (const [name, prose] of [
    [
      "normal review with a second Grok actor",
      "Normal Tasks are reviewed by Codex and Grok.",
    ],
    [
      "normal review with a second Task Agent actor",
      "Normal Tasks are reviewed by Codex and the Task Agent.",
    ],
    [
      "normal implementation with a second Codex actor",
      "Normal Tasks are implemented by Grok and Codex.",
    ],
    [
      "high implementation with a second Grok actor",
      "High Tasks are implemented by Codex and Grok.",
    ],
    [
      "high implementation with a second Task Agent actor",
      "High Tasks are implemented by Codex and the Task Agent.",
    ],
    [
      "high route with a second Grok target",
      "Route high Tasks to Codex and Grok.",
    ],
    [
      "high route with a second Task Agent target",
      "Route high Tasks to Codex and the Task Agent.",
    ],
    [
      "high delegation with a second Grok target",
      "Delegate high Tasks to Codex and Grok.",
    ],
    [
      "normal delegation with a second Codex target",
      "Delegate normal Tasks to Grok and Codex.",
    ],
    [
      "comma-coordinated normal review actors",
      "Normal Tasks are reviewed by Codex, Grok, and the Task Agent.",
    ],
    [
      "comma-coordinated high route targets",
      "Route high Tasks to Codex, Grok, and the Task Agent.",
    ],
  ]) {
    it(`round-8 coordinated actor binding rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "Codex then Grok high review actors",
      "High Tasks are reviewed by Codex and Grok.",
    ],
    [
      "Task Agent then Codex high review actors",
      "High Tasks are reviewed by the Task Agent and Codex.",
    ],
    [
      "Codex then Task Agent high review actors",
      "High Tasks are reviewed by Codex and the Task Agent.",
    ],
    ["single Codex high implementer", "High Tasks are implemented by Codex."],
    ["single Grok normal implementer", "Normal Tasks are implemented by Grok."],
    ["single Codex high target", "Route high Tasks to Codex."],
    ["single Grok normal target", "Delegate normal Tasks to Grok."],
    [
      "separate high implementation and auxiliary routes",
      "Route high Tasks to Codex and delegate their auxiliary review to the Task Agent.",
    ],
  ]) {
    it(`round-8 coordinated actor binding accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "normal Codex auxiliary route",
      "Route normal Tasks to Codex for auxiliary review.",
    ],
    [
      "normal passive Codex auxiliary slot",
      "Normal Tasks are reviewed by Codex in the auxiliary slot.",
    ],
    [
      "normal preposed auxiliary route",
      "Normal Tasks route auxiliary review to Codex.",
    ],
    ["normal auxiliary reviewer", "Normal Tasks have an auxiliary reviewer."],
    [
      "normal primary and auxiliary reviewers",
      "Normal Tasks have primary and auxiliary reviewers.",
    ],
    [
      "high Codex-only passive review",
      "High Tasks are reviewed only by Codex.",
    ],
    [
      "high Codex review with no other reviewer",
      "High Tasks are reviewed by Codex and no other reviewer.",
    ],
    [
      "high missing auxiliary reviewer",
      "High Tasks have no auxiliary reviewer.",
    ],
    [
      "high Codex primary with no auxiliary reviewer",
      "High Tasks have Codex primary and no auxiliary reviewer.",
    ],
    ["high sole Codex reviewer", "High Tasks use Codex as the only reviewer."],
  ]) {
    it(`round-8 reviewer slot and cardinality rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    [
      "normal Codex primary route",
      "Route normal Tasks to Codex for primary review.",
    ],
    [
      "normal passive Codex primary slot",
      "Normal Tasks are reviewed by Codex in the primary slot.",
    ],
    [
      "normal sole Codex reviewer",
      "Normal Tasks use Codex as the only reviewer.",
    ],
    [
      "high primary then auxiliary passive roles",
      "High Tasks are reviewed by Codex as primary and Grok as auxiliary reviewers.",
    ],
    [
      "high auxiliary then primary passive roles",
      "High Tasks are reviewed by the Task Agent as auxiliary and Codex as primary reviewers.",
    ],
    [
      "high primary then auxiliary direct routes",
      "Route high Tasks to Codex for primary review and to Grok for auxiliary review.",
    ],
    [
      "high auxiliary then primary direct routes",
      "Route high Tasks auxiliary review to the Task Agent and primary review to Codex.",
    ],
    [
      "high exact reviewer set",
      "High Tasks use Codex and the Task Agent as the only reviewers.",
    ],
  ]) {
    it(`round-8 reviewer slot and cardinality accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    [
      "normal auxiliary reviewer prohibition",
      "Normal Tasks must not have an auxiliary reviewer.",
    ],
    [
      "normal auxiliary route prohibition",
      "Never route normal Tasks to Codex for auxiliary review.",
    ],
    [
      "high auxiliary skip prohibition",
      "Never skip the auxiliary reviewer for high Tasks.",
    ],
    [
      "high auxiliary omission prohibition",
      "Do not omit Task Agent auxiliary review from high Tasks.",
    ],
    [
      "high Codex-only prohibition",
      "High Tasks must not use Codex as the only reviewer.",
    ],
  ]) {
    it(`round-8 reviewer prohibition accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  for (const [name, prose] of [
    ["high missing primary reviewer", "High Tasks have no primary reviewer."],
    [
      "normal missing primary reviewer",
      "Normal Tasks have no primary reviewer.",
    ],
    ["normal zero reviewers", "Normal Tasks have no reviewer."],
    ["high zero reviewers", "High Tasks have no reviewers."],
    ["high one-reviewer cardinality", "High Tasks have only one reviewer."],
    ["normal two-reviewer cardinality", "Normal Tasks have two reviewers."],
    ["high three-reviewer cardinality", "High Tasks have three reviewers."],
    ["high missing Codex reviewer", "High Tasks have no Codex reviewer."],
    ["normal missing Codex reviewer", "Normal Tasks have no Codex reviewer."],
    [
      "high missing Task Agent reviewer",
      "High Tasks have no Task Agent reviewer.",
    ],
  ]) {
    it(`round-8 exact reviewer set rejects ${name}`, () => {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    })
  }

  for (const [name, prose] of [
    ["normal one-reviewer cardinality", "Normal Tasks have only one reviewer."],
    ["high two-reviewer cardinality", "High Tasks have exactly two reviewers."],
    [
      "normal Task Agent reviewer exclusion",
      "Normal Tasks have no Task Agent reviewer.",
    ],
    [
      "required high reviewer omission prohibition",
      "Do not omit the primary or auxiliary reviewer from high Tasks.",
    ],
  ]) {
    it(`round-8 exact reviewer set accepts ${name}`, () => {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    })
  }

  it("round-9 keeps coordinated producer infinitives separate from finite parent actions", () => {
    for (const prose of [
      "The parent directs the Plan Author to revise, update, and edit the Plan.",
      "The parent asks the Plan Author to revise or update the Plan.",
      "The parent asks the Plan Author to revise, update, or edit the Plan.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The parent asks the Plan Author to revise the Plan and afterward will edit the Design.",
      "The parent asks the Plan Author to revise the Plan and will itself edit the Design.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-9 accepts the post-current pre-next Task boundary", () => {
    for (const prose of [
      "Switch the Task Agent after the current Task completes but before the next Task starts.",
      "After the current Task completes, switch the Task Agent before the next Task begins.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "Switch the Task Agent before the current Task completes.",
      "Switch the Task Agent while the current Task is running.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-9 resolves reviewer demonstrative, role, and possessive antecedents", () => {
    for (const prose of [
      "The Codex reviewer remains required; optional user-named Design reviewers may replace the former.",
      "The Codex reviewer remains required; optional user-named Design reviewers may replace this reviewer.",
      "The Codex reviewer is required. User-named Design reviewers can take its place.",
      "The Codex reviewer is mandatory. User-named Design reviewers replace this role.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "The Codex reviewer remains required; optional user-named Design reviewers may not replace the former.",
      "The Codex reviewer is required. User-named Design reviewers cannot take its place.",
      "The Codex reviewer is mandatory. User-named Design reviewers must not replace this role.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-9 binds actor-local negation, omitted links, and qualified actors", () => {
    for (const prose of [
      "High Tasks are implemented by Codex, not Grok.",
      "Normal Tasks are reviewed by Codex, not the Task Agent.",
      "Route high Tasks to Codex, not Grok.",
      "High Tasks are implemented by the independent Codex implementer, not Grok.",
      "High Tasks are reviewed by the independent primary Codex reviewer and the selected auxiliary Task Agent reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "High Tasks are implemented not by Codex but Grok.",
      "Route high Tasks not to Codex but Grok.",
      "Route high Tasks to the selected auxiliary Task Agent and Codex.",
      "High Tasks are reviewed by the independent primary Codex reviewer and Grok.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-9 accepts a complete direct high route with role-distinct Codex bindings", () => {
    assert.deepEqual(
      validateSkillMarkdown(
        `${skill}\nRoute high Tasks to Codex for implementation and to Codex for primary review and to Grok for auxiliary review.`
      ).failures,
      []
    )

    for (const prose of [
      "Route high Tasks to Codex for implementation and to Grok for primary review and to Codex for auxiliary review.",
      "Route high Tasks to Codex for implementation and only to Codex for primary review.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-9 enforces qualified, synonym, ordinal, missing, and extra reviewer cardinality", () => {
    for (const prose of [
      "Route high Tasks only to Codex for review.",
      "Route high Tasks to Codex and no other Agent for review.",
      "High Tasks have two primary reviewers.",
      "High Tasks have two Codex reviewers.",
      "High Tasks have two auxiliary reviewers.",
      "Normal Tasks have two primary reviewers.",
      "High Tasks have a single reviewer.",
      "High Tasks have only a primary reviewer.",
      "High Tasks are missing an auxiliary reviewer.",
      "Normal Tasks have a second reviewer.",
      "Normal Tasks have an extra reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "Normal Tasks have a single primary Codex reviewer.",
      "High Tasks have one primary Codex reviewer and one auxiliary Task Agent reviewer.",
      "High Tasks are not missing an auxiliary reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-9 preserves prohibitions against incomplete review", () => {
    for (const prose of [
      "Normal Tasks cannot complete without a reviewer.",
      "High Tasks cannot complete without both reviewers.",
      "High Tasks must not proceed with no auxiliary reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "Normal Tasks complete without a reviewer.",
      "High Tasks proceed with no auxiliary reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-9 carries Task, document, and active-state antecedents across clauses", () => {
    for (const prose of [
      "High Tasks are reviewed by Codex; they are implemented by Grok.",
      "High Tasks are reviewed by Codex. Implementation is by Grok.",
      "The Plan Author writes the Plan; the parent revises it.",
      "The current Task is running. Change the Task Agent now.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "High Tasks are reviewed by Codex; they are implemented by Codex.",
      "High Tasks are reviewed by Codex. Implementation is by Codex.",
      "The Plan Author writes the Plan; the Plan Author revises it.",
      "The current Task is completed. Change the Task Agent now.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-9 recognizes named-Agent active switches", () => {
    for (const prose of [
      "Switch from Grok to Gemini during the current Task.",
      "Replace Grok while the current Task is active.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "Switch from Grok to Gemini after the current Task completes.",
      "Replace Grok once the current Task is done.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 rejects concrete and qualified Task Agents on invalid routes", () => {
    for (const prose of [
      "High Tasks are implemented by Gemini.",
      "High Tasks are implemented by Cline.",
      "High Tasks are implemented by Claude.",
      "High Tasks are implemented by custom Acme Agent.",
      "High Tasks are implemented by Hermes, Cursor, OpenCode, Kimi, or Pi.",
      "Always use Gemini as the implementer.",
      "Use Gemini as the implementer for all Tasks.",
      "Route high Tasks to the currently selected Task Agent and Codex.",
      "High Tasks are implemented by the user-selected Task Agent and Codex.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "Normal Tasks are implemented by Gemini.",
      "High Tasks are implemented by Codex and reviewed by Gemini.",
      "Route high Tasks to Codex for implementation and to Gemini for auxiliary review.",
      "Normal Tasks are implemented by the currently selected Task Agent.",
      "Normal Tasks are implemented by the Claude Code Task Agent.",
      "Normal Tasks are implemented by the Gemini Task Agent.",
      "The parent updates the Claude Code Task Agent with adjudicated findings.",
      "The parent updates the Kimi Code Task Agent with adjudicated findings.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 resolves carried active Tasks against pronoun completion", () => {
    for (const prose of [
      "The current Task is running. Change the Task Agent after it completes.",
      "The current Task is active. Switch the Task Agent once it is finished.",
      "Before the next Task starts, after the current Task completes, switch the Task Agent.",
      "The current Task is completed. Switch the Task Agent before the next Task starts.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The current Task is running. Change the Task Agent now.",
      "The current Task is running. Change the Task Agent before it completes.",
      "The current Task is running; switch the Task Agent before the next Task starts.",
      "The current Task is active. Switch the Task Agent now, before the next Task begins.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 resolves that-role reviewer replacement antecedents", () => {
    for (const prose of [
      "The Codex reviewer is mandatory. User-named Design reviewers replace that role.",
      "The primary reviewer remains required; optional Plan reviewers substitute for that role.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "The Codex reviewer is mandatory. User-named Design reviewers must not replace that role.",
      "The primary reviewer remains required; optional Plan reviewers do not substitute for that role.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 treats another and one-more reviewers as surplus", () => {
    for (const prose of [
      "Normal Tasks have another reviewer.",
      "Normal Tasks have one more reviewer.",
      "High Tasks have another reviewer.",
      "High Tasks have one more reviewer.",
      "Every normal Task gets yet another reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "Normal Tasks have no other reviewer.",
      "Normal Tasks must not have another reviewer.",
      "High Tasks have one primary reviewer and one auxiliary reviewer.",
      "High Tasks must not add one more reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 binds rather-than and instead-of actor polarity", () => {
    for (const prose of [
      "High Tasks are implemented by Codex rather than Grok.",
      "Route high Tasks to Codex instead of Grok.",
      "Normal Tasks are implemented by the Task Agent rather than Codex.",
      "High Tasks are implemented by Codex instead of Gemini.",
      "Route normal Tasks to Gemini rather than Codex.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "High Tasks are implemented by Gemini rather than Codex.",
      "Route high Tasks to Grok instead of Codex.",
      "Normal Tasks are implemented by Codex instead of Gemini.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 preserves delegated producer ownership across afterward", () => {
    for (const prose of [
      "The parent asks the Plan Author to revise the Plan and afterward update the Design.",
      "The parent asks the Plan Author to revise the Plan and afterward update it.",
      "The parent directs the Design Fixer to fix and afterward update the Design.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The parent asks the Plan Author to revise the Plan and afterward the parent updates the Design.",
      "The parent asks the Plan Author to revise the Plan and afterward will update the Design.",
      "The parent directs the Design Fixer to fix the Design and afterward itself updates the Plan.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 distinguishes document recipients and people pronouns from artifacts", () => {
    for (const prose of [
      "The parent updates the Plan Author with review findings.",
      "The Plan Author and Codex reviewer discuss the Plan. The parent updates them with review findings.",
      "The parent updates the Design Fixer with adjudicated findings.",
      "The Design Fixer and Plan Author discuss the Design. The parent updates them with findings.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The parent updates the Plan directly with review findings.",
      "The Plan Author discusses the Plan. The parent updates it with review findings.",
      "The parent updates that Plan with review findings.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 rejects incomplete and duplicate passive reviewer sets", () => {
    for (const prose of [
      "High Tasks are reviewed by Codex, not the Task Agent.",
      "High Tasks are reviewed by Codex and Codex.",
      "High Tasks are reviewed by Codex and the Task Agent is omitted.",
      "High Tasks are reviewed by the Task Agent, not Codex.",
      "Normal Tasks have another reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "High Tasks are reviewed by Codex and the Task Agent.",
      "High Tasks are reviewed by the primary Codex reviewer and the auxiliary Codex Task Agent reviewer.",
      "Normal Tasks are reviewed by Codex, not the Task Agent.",
      "High Tasks must not omit the Task Agent auxiliary reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 resolves possessive-role and takeover reviewer replacements", () => {
    for (const prose of [
      "The Codex reviewer remains required; optional user-named Design reviewers may replace its role.",
      "The Codex reviewer remains required; optional user-named Design reviewers may take over for it.",
      "The Codex reviewer remains required; optional user-named Design reviewers may replace the mandatory reviewer.",
      "The primary reviewer remains required; optional Plan reviewers take over its role.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "The Codex reviewer remains required; optional user-named Design reviewers may not replace its role.",
      "The Codex reviewer remains required; optional user-named Design reviewers must not take over for it.",
      "The Codex reviewer remains required; optional user-named Design reviewers do not replace the mandatory reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 keeps active-switch timing directional across clauses", () => {
    for (const prose of [
      "Before the next Task starts, after the current Task completes, switch the Task Agent.",
      "The current Task is running. Change the Task Agent after it completes.",
      "The current Task is completed; switch the Task Agent before the next Task starts.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The current Task is running; switch the Task Agent before the next Task starts.",
      "The current Task is active. Before the next Task begins, change the Task Agent now.",
      "Before the current Task completes, switch the Task Agent after the next Task is planned.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 carries typed document antecedents into parent edit predicates", () => {
    for (const prose of [
      "The Plan Author writes the Plan; the parent revises that document.",
      "The Plan Author writes the Plan. The parent revises.",
      "The Design Fixer edits the Design. The parent updates that artifact.",
      "The Plan Author authors the Plan; afterward the parent modifies it.",
      "The parent revises that document.",
      "The parent updates the artifact directly.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "The Plan Author writes the Plan. The parent discusses review findings.",
      "The Plan Author writes the Plan. The parent updates the Plan Author.",
      "The Design Fixer edits the Design. The parent revises the finding summary.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 recognizes custom identities in active Task switches", () => {
    for (const prose of [
      "Switch from custom:foo to custom:bar while the current Task is active.",
      "Replace custom Acme Agent during the current Task.",
      "Change from custom alpha to Gemini while a Task is running.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }

    for (const prose of [
      "Switch from custom:foo to custom:bar after the current Task completes.",
      "Replace custom Acme Agent once the current Task is done.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }
  })

  it("round-10 binds implementation and reviewer purpose within one route relation", () => {
    for (const prose of [
      "Route high Tasks to Codex for implementation and the primary Codex reviewer and the auxiliary Grok reviewer.",
      "Route high Tasks to the Codex implementer, the primary Codex reviewer, and the auxiliary Gemini reviewer.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "Route high Tasks to Grok for implementation and the primary Codex reviewer and the auxiliary Grok reviewer.",
      "Route high Tasks to Codex for implementation and the primary Grok reviewer and the auxiliary Codex reviewer.",
      "Route normal Tasks to the Codex implementer and the primary Codex reviewer.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-10 recognizes passive parent delegation to document producers", () => {
    for (const prose of [
      "The Plan Author is asked by the parent to revise the Plan.",
      "The Design Fixer is directed by the parent to fix the Design.",
      "The Plan Author is instructed by the parent to update the Plan.",
    ]) {
      assert.deepEqual(validateSkillMarkdown(`${skill}\n${prose}`).failures, [])
    }

    for (const prose of [
      "The Plan is revised by the parent.",
      "The Design is fixed by the parent.",
      "The Plan Author is asked by the parent to revise the Plan, and afterward the parent updates it.",
    ]) {
      has(validateSkillMarkdown(`${skill}\n${prose}`).failures, "B2D-SKILL-005")
    }
  })

  it("round-11 binds qualified generic Task Agent actors", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are implemented by the chosen Task Agent.",
        reject: true,
      },
      {
        prose: "High Tasks are implemented by the resolved Task Agent.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by the invocation-selected Task Agent.",
        reject: true,
      },
      {
        prose: "Normal Tasks are implemented by the chosen Task Agent.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and the auxiliary chosen Task Agent reviewer.",
        reject: false,
      },
    ])
  })

  it("round-11 carries typed completion timing across clauses", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. Change the Task Agent after this completes.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. When complete, change the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. Change the Task Agent before this completes.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. While it remains active, change the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-11 resolves noun-phrase reviewer replacement antecedents", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers replace the role.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers replace the same role.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers must not replace the role.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers do not replace the same role.",
        reject: false,
      },
    ])
  })

  it("round-11 distinguishes surplus reviewer cardinality", () => {
    assertSkillClassifications([
      { prose: "High Tasks have two more reviewers.", reject: true },
      { prose: "High Tasks have a further reviewer.", reject: true },
      {
        prose: "High Tasks must not have two more reviewers.",
        reject: false,
      },
      {
        prose: "High Tasks must not have a further reviewer.",
        reject: false,
      },
      { prose: "High Tasks have two additional reviewers.", reject: true },
      { prose: "High Tasks have two reviewers.", reject: false },
    ])
  })

  it("round-11 propagates actor alternatives across lists and repeated links", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are implemented by Codex rather than Grok or Gemini.",
        reject: false,
      },
      {
        prose: "High Tasks are implemented by Codex rather than by Grok.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok or by Gemini.",
        reject: false,
      },
      {
        prose: "High Tasks are implemented by Grok rather than by Codex.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Gemini rather than by Codex or by Grok.",
        reject: true,
      },
    ])
  })

  it("round-11 keeps typed document and people antecedents distinct", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers discuss the Plan. The parent updates them with review findings.",
        reject: false,
      },
      {
        prose:
          "The Plan Author lists the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose: "The Plan Author writes the Plans. The parent revises them.",
        reject: true,
      },
      {
        prose:
          "The parent updates the document reviewer with adjudicated findings.",
        reject: false,
      },
      {
        prose:
          "The parent updates the document producer with adjudicated findings.",
        reject: false,
      },
      {
        prose:
          "The document reviewers discuss the Plan. The parent updates them with findings.",
        reject: false,
      },
      {
        prose: "The parent updates the documents with review findings.",
        reject: true,
      },
    ])
  })

  it("round-11 applies explicit active-completion order", () => {
    assertSkillClassifications([
      {
        prose: "After the active Task completes, switch the Task Agent.",
        reject: false,
      },
      {
        prose: "Switch the Task Agent after the running Task completes.",
        reject: false,
      },
      {
        prose: "While the active Task runs, switch the Task Agent.",
        reject: true,
      },
      {
        prose: "Switch the Task Agent before the running Task completes.",
        reject: true,
      },
    ])
  })

  it("round-11 recognizes exhaustive high-review assertions", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are reviewed by Codex alone.",
        reject: true,
      },
      {
        prose: "High Tasks are reviewed only by Codex.",
        reject: true,
      },
      {
        prose: "High Tasks are reviewed by Codex and the Task Agent alone.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and the auxiliary Task Agent reviewer.",
        reject: false,
      },
    ])
  })

  it("round-11 distinguishes plain Codex Agent from Codex Task Agent", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are implemented by the Codex Agent.",
        reject: false,
      },
      {
        prose: "High Tasks are implemented by the Codex agent.",
        reject: false,
      },
      {
        prose: "Normal Tasks are reviewed by the Codex Agent.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex Agent and the auxiliary Grok Agent.",
        reject: false,
      },
      {
        prose: "High Tasks are implemented by the Codex Task Agent.",
        reject: true,
      },
      {
        prose: "Normal Tasks are reviewed by the Codex Task Agent.",
        reject: true,
      },
      {
        prose: "Normal Tasks are implemented by the Codex Task Agent.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex Agent and the auxiliary Codex Task Agent reviewer.",
        reject: false,
      },
    ])
  })

  it("round-11 binds take-over replacements without treating take as replacement", () => {
    assertSkillClassifications([
      {
        prose: "The primary Codex reviewer takes notes.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. It takes notes for the Plan Author.",
        reject: false,
      },
      {
        prose: "The required Codex reviewer takes notes.",
        reject: false,
      },
      {
        prose:
          "Optional Design reviewers take over for the required Codex reviewer.",
        reject: true,
      },
      {
        prose:
          "Optional Design reviewers take the place of the required Codex reviewer.",
        reject: true,
      },
      {
        prose:
          "Optional Design reviewers must not take over for the required Codex reviewer.",
        reject: false,
      },
    ])
  })

  it("round-11 binds another to the reviewer it qualifies", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and another auxiliary Task Agent reviewer.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and yet another auxiliary Task Agent reviewer.",
        reject: false,
      },
      { prose: "High Tasks have another reviewer.", reject: true },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer, the auxiliary Task Agent reviewer, and another reviewer.",
        reject: true,
      },
    ])
  })

  it("round-11 binds postposed absence to its missing subject", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent when evidence is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent when evidence is absent.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent is missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the auxiliary review is missing.",
        reject: true,
      },
    ])
  })

  it("round-12 binds passive implementers only to their governing relation", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are implemented by Codex after approval by the independently selected Task Agent.",
        reject: false,
      },
      {
        prose:
          "Normal Tasks are implemented by following instructions from Codex.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented by the independently selected Task Agent.",
        reject: true,
      },
      {
        prose: "Normal Tasks are implemented by the resolved Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-12 binds completion timing to the Task that completes", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. After review completes, change the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. When testing completes, change the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. Once validation finishes, change the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After review completion, switch the Task Agent.",
        reject: true,
      },
      {
        prose: "After the active Task review completes, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "When the running Task testing completes, change the Task Agent.",
        reject: true,
      },
      {
        prose: "After completion of the active Task, switch the Task Agent.",
        reject: false,
      },
      {
        prose: "After completion of the running Task, switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the currently active Task, switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. When complete, change the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-12 binds replacement constructions to their reviewer object", () => {
    assertSkillClassifications([
      {
        prose:
          "Optional Design reviewers take the role of the required Codex reviewer.",
        reject: true,
      },
      {
        prose:
          "Optional Design reviewers take on the role of the required Codex reviewer.",
        reject: true,
      },
      {
        prose: "The required Codex reviewer takes notes over time.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers replace the advisory role.",
        reject: false,
      },
      {
        prose:
          "Optional Design reviewers take over for the required Codex reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. User-named Design reviewers replace the role.",
        reject: true,
      },
    ])
  })

  it("round-12 treats qualified same-slot reviewers as surplus", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks have another primary Codex reviewer.",
        reject: true,
      },
      {
        prose: "Normal Tasks have an additional primary Codex reviewer.",
        reject: true,
      },
      {
        prose: "Normal Tasks are reviewed by another primary Codex reviewer.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and another auxiliary Task Agent reviewer.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by the primary Codex reviewer and yet another auxiliary Task Agent reviewer.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by another auxiliary Task Agent reviewer and the primary Codex reviewer.",
        reject: false,
      },
    ])
  })

  it("round-12 bounds alternative polarity to its contrast branch", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, but by Gemini when Codex is unavailable.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok but also by Gemini.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than being implemented by Grok.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented by Grok rather than being implemented by Codex.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok or by Gemini.",
        reject: false,
      },
    ])
  })

  it("round-12 resolves plural pronouns by subject and recipient roles", () => {
    assertSkillClassifications([
      {
        prose:
          "The Plan Author sends the Plan and Design to the developers. The parent updates them with review findings.",
        reject: false,
      },
      {
        prose:
          "The reviewers discuss the Plans. The parent updates them with assignments.",
        reject: false,
      },
      {
        prose:
          "The Plan Author lists the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The Plan Author sends the Plan and Design to the developers. The parent updates the documents.",
        reject: true,
      },
    ])
  })

  it("round-12 binds review exclusivity to the reviewer relation", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are reviewed exclusively by Codex.",
        reject: true,
      },
      {
        prose: "High Tasks are reviewed solely by Codex.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex for findings solely about correctness. The Task Agent provides the auxiliary review.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex only for correctness. The Task Agent provides the auxiliary review.",
        reject: false,
      },
      {
        prose: "High Tasks are reviewed by Codex and the Task Agent alone.",
        reject: false,
      },
    ])
  })

  it("round-12 binds postposed reviewer absence through its predicate", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is definitely omitted.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is unexpectedly missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer for the high route is unexpectedly missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent when evidence is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is definitely not omitted.",
        reject: false,
      },
    ])
  })

  it("round-13 primary 1 binds sequential passive implementers", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are implemented by Codex, then by Grok.",
        reject: true,
      },
      {
        prose: "High Tasks are implemented by Codex, subsequently by Grok.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented first by Codex, then reviewed by the Task Agent.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented first by Codex, then approved by Gemini.",
        reject: false,
      },
    ])
  })

  it("round-13 primary 2 carries a Task subject across completion modifiers", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. When finally complete, change the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. Once already complete, change the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. When review is finally complete, change the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. Once testing is already complete, change the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-13 primary 3 keeps unrelated take-role objects local", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of observers.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take the role of note takers.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take the role of this required reviewer.",
        reject: true,
      },
    ])
  })

  it("round-13 primary 4 resets alternatives at positive subordinate clauses", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, while Gemini also implements them.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, although Gemini also implements them.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, while Gemini does not implement them.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, although Gemini remains available.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, whereas Gemini also implements them.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex rather than by Grok, whereas Gemini remains available.",
        reject: false,
      },
    ])
  })

  it("round-13 primary 5 prefers a predicate's plural document object", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers list the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The reviewers list the Plan and Design. The parent revises them.",
        reject: true,
      },
      {
        prose:
          "The developers list the Plan and Design. The parent updates the developers.",
        reject: false,
      },
      {
        prose:
          "The reviewers list the Plan and Design. The parent revises the documents.",
        reject: true,
      },
    ])
  })

  it("round-13 primary 6 binds exclusivity to its temporal complement", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex exclusively after implementation. The Task Agent provides the auxiliary review.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex exclusively after testing. The Task Agent provides the auxiliary review.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed exclusively by Codex after implementation. The Task Agent provides the auxiliary review.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex only after testing. The Task Agent provides the auxiliary review.",
        reject: false,
      },
    ])
  })

  it("round-13 primary 7 keeps embedded missing evidence off the reviewer", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is aware input is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is told evidence is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is still missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is definitely not missing.",
        reject: false,
      },
    ])
  })

  it("round-13 auxiliary 1 follows sequential passive actor ellipsis", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are implemented first by Codex, then by Gemini.",
        reject: true,
      },
      {
        prose:
          "Normal Tasks are implemented first by Gemini, then reviewed by Codex.",
        reject: false,
      },
      {
        prose:
          "High Tasks are implemented first by Codex, then checked by Gemini.",
        reject: false,
      },
    ])
  })

  it("round-13 auxiliary 2 rejects completion of a Task review", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. After completion of the active Task review, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, review it and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the review for the active Task, switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-13 auxiliary 3 resolves mixed plural subject and object roles", () => {
    assertSkillClassifications([
      {
        prose:
          "The reviewers list the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The reviewers list the Plan and Design. The parent updates the reviewers.",
        reject: false,
      },
      {
        prose:
          "The reviewers list the Plan and Design. The parent updates the documents.",
        reject: true,
      },
    ])
  })

  it("round-13 review preserves take-role pronoun antecedents", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the former.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of observers.",
        reject: false,
      },
    ])
  })

  it("round-13 review distinguishes Task components from following actions", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. After completion of the active Task, review the report and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task review, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. When supply is complete, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. When finally complete, switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-13 review accepts ordinary reviewer-absence modifiers", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is now missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is again missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is aware evidence is now missing.",
        reject: false,
      },
    ])
  })

  it("round-13 review carries sequential passive relation fillers", () => {
    assertSkillClassifications([
      {
        prose: "High Tasks are implemented by Codex, then also by Gemini.",
        reject: true,
      },
      {
        prose:
          "High Tasks are implemented by Codex, then also approved by Gemini.",
        reject: false,
      },
    ])
  })

  it("round-13 review resolves transitive plural document objects", () => {
    assertSkillClassifications([
      {
        prose:
          "The reviewers revise the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The developers edit the Plan and Design. The parent revises them.",
        reject: true,
      },
      {
        prose:
          "The reviewers discuss the Plan and Design. The parent updates them with assignments.",
        reject: false,
      },
    ])
  })

  it("round-13 review carries modifiers into temporal exclusivity complements", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex exclusively immediately after implementation. The Task Agent provides the auxiliary review.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed exclusively immediately by Codex after implementation. The Task Agent provides the auxiliary review.",
        reject: true,
      },
    ])
  })

  it("round-14 uses Task punctuation before bare review and test objects", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. After completion of the active Task, review findings and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, test results and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, review evidence and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, test outputs and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review findings and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: test results and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, review findings. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task review, record findings. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is completed and running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is complete and active. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed while active. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task review, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task testing, switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-14 resolves qualified take-role targets in both directions", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of their advisory reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of their optional advisory reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Another reviewer takes on the role of that optional Design reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of their Design reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take the role of this required reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary reviewer.",
        reject: true,
      },
    ])
  })

  it("round-14 prefers people beneficiaries and participants over document objects", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers revise the Plan and Design for the reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design on behalf of the reviewers. The parent updates them.",
        reject: false,
      },
      {
        prose:
          "The developers edit the Plan and Design with the reviewers. The parent updates them.",
        reject: false,
      },
      {
        prose:
          "The developers edit the Plan and Design together with the reviewers. The parent updates them.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity while the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design with annotations while the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity as the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design with annotations because the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity though the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The reviewers revise the Plan and Design. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity. The parent updates them.",
        reject: true,
      },
      {
        prose:
          "The developers edit the Plan and Design with annotations. The parent updates them.",
        reject: true,
      },
    ])
  })

  it("round-14 binds ordinary multiword complements to reviewer absence", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is for now missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is often found missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is now missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is again missing.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing evidence.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is for now missing input.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is often found missing results.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing feedback.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing approval.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing documentation.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing the context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing from the route.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again missing today.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is once again aware input is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is for now told evidence is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is often found where evidence is missing.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is often found not missing.",
        reject: false,
      },
    ])
  })

  it("round-15 distinguishes Task component states from separated actions", () => {
    assertSkillClassifications([
      {
        prose:
          "The current Task is running. After completion of the active Task: review remains incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, testing remains unfinished. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: validation is still ongoing. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: the review remains incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: its review remains incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: its final independent security review remains incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: the final review remains incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review has not finished. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review has yet to finish. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review remains to be finished. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (review pending), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (validation underway), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (review not complete), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (review pending final approval), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review pending findings, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (review not yet complete), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review and testing remain incomplete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is not pending. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is not incomplete. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: carefully review pending issues, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review remains anything but complete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is scarcely complete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is almost complete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is barely complete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review remains nowhere near complete. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is fully complete. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is no longer incomplete. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is still pending. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review is complete. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review still continues. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task - review the report and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task -- test the results and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task \u2014 validate the results and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task (review the report) and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task / review the report and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task, review findings and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: test results and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: test whether the server is still running, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: review open issues, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: test running services, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task: review carefully, then continue by switching the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The current Task is running. After completion of the active Task review, switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-15 resolves qualified anaphoric and explicit take-role targets", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. Another reviewer takes on the role of that very reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that former reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of this original reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the former reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previous reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the aforementioned reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously designated reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary note taker.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary meeting note taker.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary note-taker.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary for this Task.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary for the producer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary during final review.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary temporarily.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary whenever needed.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary after the implementer leaves.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary once the implementer leaves.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary if the implementer leaves.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary until the implementer returns.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary because the implementer left.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary unless the implementer returns.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary upon the implementer's departure.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary note taker once the implementer leaves.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary in case the producer leaves.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary as soon as the implementer leaves.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary meeting facilitator.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary contact person.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required contact person.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary now.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary selected for this Task.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary slot.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required auxiliary slot.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary today.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required individual.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required party.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary assigned to this Task.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary designated by the Plan.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the same one.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the mandatory one.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary meeting note taker temporarily.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of their advisory reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Another reviewer takes on the role of that optional Design reviewer.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary reviewer.",
        reject: true,
      },
    ])
  })

  it("round-15 binds people antecedents to their actual relations", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers revise the Plan and Design for the reviewers' archive. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for the archive of the reviewers. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design with the reviewers' annotations. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design on behalf of the reviewers' organization. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by consulting both reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design after consulting both reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by the senior reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by the external reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by 3 reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by the currently available reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design by consulting both senior and external reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design by consulting senior and external reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design by consulting all of the currently available external reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The Plan Author revises the Plan and Design by carefully consulting all of the currently available external security reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The Plan Author revises the Plan and Design using feedback from the reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The Plan Author revises the Plan and Design alongside the reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity: reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity so that three reviewers can respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by locally and internationally recognized reviewers yesterday. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The Plan Author revises the Plan and Design for the reviewers who requested changes. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by senior and external reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers send the Plan and Design to the assigned reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity while the reviewers observe. The parent edits them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity after the reviewers leave. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity since the reviewers left. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity and both reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by working carefully and the reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by working independently, then the reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by working independently, and reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by working independently, and senior reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design by working independently and reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by senior, external, and security reviewers. The parent updates them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design by working hard, and senior reviewers observe. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design reviewed by locally and internationally recognized reviewers. The parent updates them on progress.",
        reject: false,
      },
    ])
  })

  it("round-15 distinguishes reviewer absence from transitive predicates", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing right now.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing this morning.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing altogether.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing at present.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing as usual.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing once more.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing every Friday.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing twice today.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing because their assignment ended.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing this afternoon.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing tomorrow.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing due to illness.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing all day.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing over the weekend.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing without explanation.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing throughout the day.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing with no explanation.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing all weekend.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing across the weekend.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the entire day.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing every other day.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing all quarter.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing three days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the next three days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing these three days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing several days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing a few days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing most of the week.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the coming week.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing half the week.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing a couple of days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing every single day.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing one and a half days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the first three days.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing a couple of findings.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the first three deadlines.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing a day and a half.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the past week.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing a dozen findings.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the next three deadlines.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing yesterday morning.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing - critical context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing (critical) context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing over half the required findings.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing at least one deadline.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing evidence.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing input.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing the deadline.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking in experience.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking in critical context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking technically.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking entirely.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking completely.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing today because their assignment ended.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking all week due to illness.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing today and tomorrow because their assignment ended.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking today and tomorrow.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing all morning and afternoon.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing all context and evidence.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing Friday's deadline.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is missing from the route.",
        reject: true,
      },
    ])
  })

  it("round-15 preserves carried Task activity and completion order", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is partially completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is only partly completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is provisionally completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent switches immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is partially completed, but is now fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The currently active Task is completed but is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed and still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but after the final checks is still actively running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, according to telemetry, it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but according to telemetry, it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but according to the integration service itself, it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but in fact it remains active. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is fully completed while the integration service is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but its validation is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but the review is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose: "The Task continues running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but after final checks the integration service is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but after release the server is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but after checks service is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but the service is restarted and it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed, but only provisionally. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed, but only partly. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed, but only partially. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed in part. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed, though only partially. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed, yet only partly. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed only in part. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed merely in part. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed to some extent. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed to the full extent required. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is nearly completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed in large part. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is fully completed but only partially celebrated. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running and then completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running and then it completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running, but it has now completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running and then it completed validation. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is running and then it finished its review. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is running and then it completed, then review findings and switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running. It completed validation. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The separate build server reports that it completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose: "The Task is running. It completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running. After final checks, it was fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is running. Then it completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running and completed reviews are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed code reviews are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed integration tests are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed reviews of the implementation are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed final independent external security reviews are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed reviews and tests are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running and completed reviews and validation are archived. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed and then running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed and then it starts running again. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but afterward it resumes running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but it resumes. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but it starts again. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but it restarts. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but it begins again. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but it becomes active again. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed, but the server restarts. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed, but the server becomes active again. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed, but it appears the server is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. Do not switch the build server, but switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. Switch the build server, but do not switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. Switch the build server, not the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. Switch the build server rather than the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. Switch the Task Agent rather than the build server.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. Do not change the build server. Switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed. Do not change the build server. Switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. Do not change the build server before switching the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. Do not switch the Task Agent while it runs; switch the Task Agent after it completes.",
        reject: false,
      },
      {
        prose:
          "The active Task is fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed and is no longer running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is mostly completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed for the most part. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is largely completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is substantially completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is almost completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is barely completed. Then switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-16 binds imperative objects and possessive Task components", () => {
    assertSkillClassifications([
      {
        prose:
          "After completion of the active Task: test running software, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: please review pending work, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: please test running code, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: please review pending issue, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: review pending issue, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but the server's review is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: please review the pending issue, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but the Task's review is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but its review is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "After completion of the active Task (review pending final approval), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "After completion of the active Task (test running software), switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-16 preserves explicit role heads after reviewer modifiers", () => {
    assertSkillClassifications([
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer contact person.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer note taker.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of the required primary reviewer.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer from before.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer who served earlier.",
        reject: true,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer that was assigned earlier.",
        reject: true,
      },
    ])
  })

  it("round-16 bounds people relations at document heads and purpose clauses", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers revise the Plan and Design after consulting both reviewer and producer Plans. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design after consulting both reviewer and producer. The parent updates both of them.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity in order that three reviewers can respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity in order for three reviewers to respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity so that three reviewers can respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for clarity, allowing three reviewers to respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design for three reviewers to use. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan and Design with both reviewers to advise. The parent edits both of them.",
        reject: false,
      },
    ])
  })

  it("round-16 keeps modified lacking-in predicates transitive", () => {
    assertSkillClassifications([
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking completely in experience.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking completely in critical context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking entirely in experience.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking entirely in critical context.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking severely in experience.",
        reject: false,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking completely.",
        reject: true,
      },
      {
        prose:
          "High Tasks are reviewed by Codex and the Task Agent reviewer is lacking in experience.",
        reject: false,
      },
    ])
  })

  it("round-16 recognizes subject-first Agent change bridges", () => {
    assertSkillClassifications([
      ...["will", "must", "should", "can", "may"].map((modal) => ({
        prose: `The active Task is running. The Task Agent ${modal} switch immediately.`,
        reject: true,
      })),
      {
        prose:
          "The active Task is running. The Task Agent then switches immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent itself switches immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent later switches immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent will not switch immediately.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed. The Task Agent will switch immediately.",
        reject: false,
      },
      {
        prose:
          "The active Task is running. The build server will switch immediately.",
        reject: false,
      },
    ])
  })

  it("round-16 lets later full completion supersede partial completion", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is partially completed and later fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is partially completed and afterward fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is partially completed and later only partly completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed and afterward resumes running. Then switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-16 keeps explicit non-Task subjects from claiming Task state", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is completed but, according to telemetry, a separate server says that it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed but the server restarts and it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but in fact the integration service monitoring it remains active. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but according to telemetry the server tracking it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but in fact the integration service monitoring only it remains active. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but according to telemetry it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but in fact it remains active. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed but it restarts and it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed but the server restarts and the Task is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed but the server for the Task restarts and it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed but the server monitoring the Task restarts and it is still running. Then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-17 preserves explicit unfinished Task status around imperatives", () => {
    assertSkillClassifications([
      {
        prose:
          "After completion of the active Task (please note its review pending), switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "After completion of the active Task: the Task's test running overnight, switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "After completion of the active Task: please review pending work, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: test running software, then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "After completion of the active Task: please note the server's review pending, then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-17 binds possessive Task component ownership locally", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is completed but the Task's primary reviewer's mandatory review is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, despite the server's warning, the review is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, following the server's report, the validation is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but the server's review is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but the Task's primary reviewer's server is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but the Task's report says the server's review is still running. Then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-17 distinguishes purpose clauses from people objects", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers revise the Plan, Design, and code to enable both reviewers. The parent updates both of them.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan, Design, and code by allowing both reviewers to participate. The parent updates both of them.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Plan, Design, and code in order that three reviewers can respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan, Design, and code in order for three reviewers to respond. The parent edits both of them.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Plan, Design, and code, allowing three reviewers to respond. The parent edits both of them.",
        reject: true,
      },
    ])
  })

  it("round-17 keeps reviewer postmodifiers on anaphoric role heads", () => {
    assertSkillClassifications([
      ...[
        "on duty",
        "with long tenure",
        "assigned earlier",
        "still responsible",
      ].map((postmodifier) => ({
        prose: `The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer ${postmodifier}.`,
        reject: true,
      })),
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer contact person.",
        reject: false,
      },
      {
        prose:
          "The Codex reviewer is mandatory. Optional Design reviewers take on the role of that previously assigned reviewer note taker.",
        reject: false,
      },
    ])
  })

  it("round-17 bounds role-document attachment at punctuation", () => {
    assertSkillClassifications([
      {
        prose:
          "The developers revise the Design after consulting both reviewer and producer (Plan work begins later). The parent updates both of them on progress.",
        reject: false,
      },
      {
        prose:
          "The developers revise the Design after consulting both reviewer and producer Plans. The parent updates both of them on progress.",
        reject: true,
      },
      {
        prose:
          "The developers revise the Design after consulting both reviewer and producer: Plan work begins later. The parent updates both of them on progress.",
        reject: false,
      },
    ])
  })

  it("round-17 requires an Agent replacement object for subject-first changes", () => {
    assertSkillClassifications([
      ...[
        "will switch branches immediately",
        "can change directories",
        "should replace a file",
        "may switch the logging mode",
      ].map((action) => ({
        prose: `The active Task is running. The Task Agent ${action}.`,
        reject: false,
      })),
      {
        prose:
          "The active Task is running. The Task Agent will switch immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent itself switches immediately.",
        reject: true,
      },
      {
        prose:
          "The active Task is running. The Task Agent later switches immediately.",
        reject: true,
      },
    ])
  })

  it("round-17 rejects transitive completion as full Task completion", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is partially complete and later completed documentation. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is partially complete and afterward completed the migration. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is partially complete and later completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is partially complete and afterward fully completed. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is partially complete and later only partly completed. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed and afterward resumes running. Then switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-17 retains Task anaphors through reporting and participial adjuncts", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is completed but, according to its own telemetry, it is reported that it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, according to its own telemetry, after restarting, it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but according to ongoing monitoring it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, according to telemetry, a separate server says that it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but according to telemetry the server tracking it is still running. Then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-17 honors an explicit Task reporting subject", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is completed but, according to telemetry, the Task says that it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, according to telemetry, the server says that it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but, according to telemetry, it says that it is still running. Then switch the Task Agent.",
        reject: true,
      },
    ])
  })

  it("round-17 binds transitive restart objects to the Task", () => {
    assertSkillClassifications([
      {
        prose:
          "The Task is completed but the server restarts it and it is still running. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The Task is completed but the server restarts and it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed but the server restarts itself and it is still running. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The Task is completed but the server restarts after it receives a signal and it is still running. Then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("round-17 binds explicit Task restarts after preposed gerund adjuncts", () => {
    assertSkillClassifications([
      {
        prose:
          "The active Task is completed but, with monitoring complete, the Task restarts. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, with testing complete, the Task restarts. Then switch the Task Agent.",
        reject: true,
      },
      {
        prose:
          "The active Task is completed but, with monitoring complete, the server restarts. Then switch the Task Agent.",
        reject: false,
      },
      {
        prose:
          "The active Task is completed but, with testing complete, the server restarts and it is still running. Then switch the Task Agent.",
        reject: false,
      },
    ])
  })

  it("contains the complete operational policy and document JSON shapes", () => {
    const policy = fencedJsonAfterHeading(
      realSkill,
      "### Operational policy JSON"
    )
    assert.deepEqual(policy.design_review_triggers, [
      "spans_modules",
      "migration",
      "concurrency",
      "security",
      "persistence",
      "externally_visible_compatibility",
      "material_ambiguity",
    ])
    assert.deepEqual(
      policy.risk_policy.hard_triggers.map(({ kind }) => kind),
      [
        "concurrency_lifecycle",
        "security_trust_boundary",
        "migration_destructive_persistence",
        "public_compatibility",
        "unsafe_ffi",
        "update_rollback",
      ]
    )
    assert.deepEqual(
      policy.risk_policy.soft_signals.map(({ kind, score }) => [kind, score]),
      [
        ["cross_runtime_or_process", 2],
        ["broad_production_surface", 1],
        ["multiple_ownership_modules", 1],
        ["shared_interface", 1],
        ["dependency_or_build", 1],
        ["multi_layer_without_test_seam", 1],
      ]
    )
    assert.deepEqual(policy.risk_policy.evidence_fields, {
      hard_trigger: ["kind", "evidence"],
      soft_signal: ["kind", "score", "evidence"],
      evidence: "non-empty file, module, or interface facts",
    })
    assert.deepEqual(policy.risk_policy.arithmetic, {
      distinct_active_signal_count: 1,
      any_hard_trigger_level: "high",
      normal_soft_score_range: [0, 2],
      high_soft_score_minimum: 3,
      invalid: "unknown, duplicate, contradictory, incorrect, or evidence-free",
    })
    assert.deepEqual(policy.byte_limits, {
      plan_document: 2 * 1024 * 1024,
      routing_block: 256 * 1024,
      progress_document: 512 * 1024,
      progress_block: 64 * 1024,
    })

    const planShape = fencedJsonAfterHeading(realSkill, "### Plan routing JSON")
    assert.deepEqual(Object.keys(planShape), [
      "schema_version",
      "risk_policy_version",
      "task_agent_generations",
      "tasks",
    ])
    assert.deepEqual(Object.keys(planShape.task_agent_generations[0]), [
      "generation",
      "agent_type",
      "profile_id",
      "effective_from_task_index",
    ])
    assert.deepEqual(Object.keys(planShape.tasks[0]), [
      "index",
      "task_agent_generation",
      "risk",
      "route",
    ])
    assert.deepEqual(Object.keys(planShape.tasks[0].risk), [
      "level",
      "hard_triggers",
      "soft_signals",
      "score",
      "reason",
    ])
    assert.deepEqual(Object.keys(planShape.tasks[0].route), [
      "implementer",
      "reviewers",
    ])

    const progressShape = fencedJsonAfterHeading(realSkill, "### Progress JSON")
    assert.deepEqual(Object.keys(progressShape), [
      "schema_version",
      "plan_rel_path",
      "active_task_index",
      "tasks",
      "final_review_status",
      "updated_at",
    ])
    assert.deepEqual(Object.keys(progressShape.tasks[0]), [
      "index",
      "status",
      "commit",
      "risk_level",
      "task_agent_generation",
      "expected_work_unit_keys",
      "runs",
    ])
    assert.deepEqual(Object.keys(progressShape.tasks[0].runs[0]), [
      "role",
      "agent_type",
      "profile_id",
      "task_id",
      "child_conversation_id",
      "state",
      "work_unit_key",
      "recovery_count",
      "replaced_task_id",
      "replacement_reason",
    ])
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

  it("requires routing authoritatively while preserving markerless parser compatibility", () => {
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
    const legacyPlanMarkdown = "# Plan\n\n## Task 1: Legacy\n"
    const legacyProgressMarkdown = `# Progress\n${block(
      "codeg-simple-progress-v1",
      legacyProgress
    )}`
    const parsedPlan = parseSimplePlan(legacyPlanMarkdown)
    assert.deepEqual(parsedPlan.failures, [])
    assert.equal(parsedPlan.routing, null)
    assert.deepEqual(
      parseSimpleProgress(legacyProgressMarkdown, planRelPath, parsedPlan)
        .failures,
      []
    )

    has(
      validateSimpleDocuments({
        skillMarkdown: skill,
        planMarkdown: legacyPlanMarkdown,
        progressMarkdown: legacyProgressMarkdown,
        planRelPath,
      }).failures,
      "B2D-ROUTING-001"
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

  it("requires the entire pending suffix to have empty runs at a new generation boundary", () => {
    const route = routing()
    const selected = identity("gemini", "careful")
    route.task_agent_generations.push({
      generation: 2,
      ...selected,
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "normal", selected, 2)
    route.tasks.push(task(3, "normal", selected, 2))
    const routedPlan = `${plan(route)}\n## Task 3: Dirty later Task\n`
    const state = progress(route)
    state.tasks.push(progressTask(route.tasks[2], "pending", 30))
    state.tasks[2].runs.push(
      run(
        state.tasks[2].expected_work_unit_keys.implementer,
        "reserving",
        "reserved-later",
        null
      )
    )

    has(validate(route, state, routedPlan), "B2D-ROUTING-007")
  })

  it("round-9 keeps the future pending suffix clean after generation admission", () => {
    const route = routing()
    const selected = identity("gemini", "careful")
    route.task_agent_generations.push({
      generation: 2,
      ...selected,
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "normal", selected, 2)
    route.tasks.push(task(3, "normal", selected, 2))
    const routedPlan = `${plan(route)}\n## Task 3: Preserve serial suffix\n`
    const state = progress(route)
    state.tasks.push(progressTask(route.tasks[2], "pending", 30))
    state.tasks[1].status = "in_progress"
    state.active_task_index = 2
    state.tasks[1].runs.push(
      run(
        state.tasks[1].expected_work_unit_keys.implementer,
        "running",
        "t2-i",
        20
      )
    )
    state.tasks[2].runs.push(
      run(
        state.tasks[2].expected_work_unit_keys.implementer,
        "reserving",
        "reserved-later",
        null
      )
    )

    has(validate(route, state, routedPlan), "B2D-ROUTING-007")
    state.tasks[2].runs = []
    assert.deepEqual(validate(route, state, routedPlan), [])
  })

  it("returns deterministic failures for null Tasks during generation validation", () => {
    const route = routing()
    route.task_agent_generations.push({
      generation: 2,
      ...identity("gemini"),
      effective_from_task_index: 2,
    })
    route.tasks[1] = task(2, "normal", identity("gemini"), 2)
    const state = progress(route)
    state.tasks = [null]

    assert.doesNotThrow(() => validate(route, state))
    has(validate(route, state), "B2D-PROGRESS-005")
  })

  it("exposes pure routing/progress agreement functions", () => {
    const parsed = parseSimplePlan(plan())
    const failures = []
    const normalized = validateRoutingSnapshot(parsed.routing, parsed, failures)
    validateProgressRouting(progress(), normalized, failures)
    assert.deepEqual(failures, [])
  })
})
