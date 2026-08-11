import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { describe, it } from "node:test"
import { fileURLToPath } from "node:url"
import {
  MAX_PLAN_DOCUMENT_BYTES,
  MAX_PROGRESS_BLOCK_BYTES,
  MAX_PROGRESS_DOCUMENT_BYTES,
  parseSimplePlan,
  validateSimpleDocuments,
  validateSkillMarkdown,
} from "./validate-contract.lib.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const realSkill = readFileSync(join(__dirname, "..", "SKILL.md"), "utf8")
const planRelPath = "docs/superpowers/plans/example.md"

const SIMPLE_SKILL = `---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm to Delivery

Use Simple documents and generic delegation to deliver the approved work.
`

const PLAN = `# Example Implementation Plan

### Task 1: Parse documents

**Files:**
- Modify: \`src/parser.ts\`

Run: \`node --test parser.test.mjs\`

## Task 2: Project progress

**Files:**
- Modify: \`src/projector.ts\`

Run: \`node --test projector.test.mjs\`
`

function progressBlock(snapshot, notes = "") {
  return `# Delivery progress

<!-- codeg-simple-progress-v1
${JSON.stringify(snapshot, null, 2)}
-->

${notes}`
}

function validSnapshot(overrides = {}) {
  return {
    schema_version: 1,
    plan_rel_path: planRelPath,
    active_task_index: 2,
    tasks: [
      {
        index: 1,
        status: "completed",
        commit: "0123456789abcdef",
        runs: [
          {
            role: "implementer",
            agent_type: "grok",
            profile_id: null,
            task_id: "task-1-impl",
            child_conversation_id: 41,
            state: "completed",
            work_unit_key: "task|1|implementer|grok|none",
            recovery_count: 0,
            replaced_task_id: null,
            replacement_reason: null,
          },
          {
            role: "reviewer",
            agent_type: "codex",
            task_id: "task-1-review",
            child_conversation_id: 42,
            state: "completed",
            work_unit_key: "task|1|reviewer|codex|none",
            recovery_count: 0,
          },
        ],
      },
      {
        index: 2,
        status: "in_progress",
        runs: [
          {
            role: "implementer",
            agent_type: "codex",
            profile_id: "release",
            task_id: "task-2-b",
            child_conversation_id: 51,
            state: "running",
            work_unit_key: "task|2|implementer|codex|release",
            recovery_count: 1,
            replaced_task_id: "task-2-a",
            replacement_reason: "unresumable",
          },
        ],
      },
    ],
    final_review_status: "pending",
    updated_at: "2026-08-11T00:00:00Z",
    ...overrides,
  }
}

function validate(overrides = {}) {
  return validateSimpleDocuments({
    skillMarkdown: SIMPLE_SKILL,
    planMarkdown: PLAN,
    progressMarkdown: progressBlock(validSnapshot()),
    planRelPath,
    ...overrides,
  })
}

function assertHasRule(failures, ruleId) {
  assert.ok(
    failures.some((failure) => failure.startsWith(`[${ruleId}]`)),
    `expected ${ruleId}; got ${failures.join("; ")}`
  )
}

describe("Skill metadata contract", () => {
  it(
    "accepts trigger-only metadata without requiring exact workflow prose",
    () => {
      assert.deepEqual(validateSkillMarkdown(SIMPLE_SKILL).failures, [])
    }
  )

  it("rejects v2-only tool and output identifiers wherever they appear", () => {
    for (const identifier of [
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
      assertHasRule(
        validateSkillMarkdown(`${SIMPLE_SKILL}\n${identifier}\n`).failures,
        "B2D-SKILL-003"
      )
    }
  })

  it("requires only name and a trigger-only Use when description", () => {
    assertHasRule(
      validateSkillMarkdown(SIMPLE_SKILL.replace("name:", "title:")).failures,
      "B2D-SKILL-001"
    )
    assertHasRule(
      validateSkillMarkdown(
        SIMPLE_SKILL.replace("description: Use when", "description: This runs")
      ).failures,
      "B2D-SKILL-001"
    )
    assertHasRule(
      validateSkillMarkdown(
        SIMPLE_SKILL.replace(
          /description:.*$/m,
          "description: Use when work needs a Plan, registration, and serial delegation."
        )
      ).failures,
      "B2D-SKILL-001"
    )
  })

  it("accepts the rewritten production Skill", () => {
    assert.deepEqual(validateSkillMarkdown(realSkill).failures, [])
  })
})

describe("Simple Plan parsing", () => {
  it("parses level-2 and level-3 Task headings in display order", () => {
    const parsed = parseSimplePlan(PLAN)
    assert.deepEqual(
      parsed.tasks.map(({ index, title }) => ({ index, title })),
      [
        { index: 1, title: "Parse documents" },
        { index: 2, title: "Project progress" },
      ]
    )
    assert.deepEqual(parsed.failures, [])
  })

  it("ignores Task-looking headings inside fenced code", () => {
    const parsed = parseSimplePlan(`## Task 1: Real

\`\`\`markdown
### Task 2: Example only
\`\`\`

### Task 2: Real second
`)
    assert.deepEqual(
      parsed.tasks.map((task) => task.title),
      ["Real", "Real second"]
    )
    assert.deepEqual(parsed.failures, [])
  })

  it("rejects duplicate, non-contiguous, and malformed Task headings", () => {
    const parsed = parseSimplePlan(`## Task 1: One
## Task 3: Three
### Task 3: Duplicate
### Task x: Malformed
`)
    assertHasRule(parsed.failures, "B2D-PLAN-002")
    assertHasRule(parsed.failures, "B2D-PLAN-003")
  })

  it("bounds the Plan document", () => {
    assertHasRule(
      parseSimplePlan("x".repeat(MAX_PLAN_DOCUMENT_BYTES + 1)).failures,
      "B2D-PLAN-001"
    )
  })
})

describe("Simple progress parsing and validation", () => {
  it("accepts the exact bounded block plus human-readable notes", () => {
    const result = validate({
      progressMarkdown: progressBlock(
        validSnapshot(),
        "Task 1 evidence and recovery notes remain ordinary Markdown."
      ),
    })
    assert.deepEqual(result.failures, [])
    assert.equal(result.plan.tasks.length, 2)
    assert.equal(result.progress.snapshot.tasks.length, 2)
  })

  it("requires exactly one complete progress marker", () => {
    assertHasRule(
      validate({ progressMarkdown: "# no structured block" }).failures,
      "B2D-PROGRESS-001"
    )
    const block = progressBlock(validSnapshot())
    assertHasRule(
      validate({ progressMarkdown: `${block}\n${block}` }).failures,
      "B2D-PROGRESS-001"
    )
    assertHasRule(
      validate({
        progressMarkdown:
          "<!-- codeg-simple-progress-v1 {\"schema_version\":1}",
      }).failures,
      "B2D-PROGRESS-001"
    )
  })

  it("bounds the full ledger and structured block", () => {
    assertHasRule(
      validate({
        progressMarkdown: "x".repeat(MAX_PROGRESS_DOCUMENT_BYTES + 1),
      }).failures,
      "B2D-PROGRESS-002"
    )
    assertHasRule(
      validate({
        progressMarkdown: `<!-- codeg-simple-progress-v1\n${"x".repeat(
          MAX_PROGRESS_BLOCK_BYTES + 1
        )}\n-->`,
      }).failures,
      "B2D-PROGRESS-002"
    )
  })

  it("requires schema version 1 and the registered Plan path", () => {
    assertHasRule(
      validate({
        progressMarkdown:
          "<!-- codeg-simple-progress-v1\n{not-json}\n-->",
      }).failures,
      "B2D-PROGRESS-003"
    )
    assertHasRule(
      validate({
        progressMarkdown: progressBlock(validSnapshot({ schema_version: 2 })),
      }).failures,
      "B2D-PROGRESS-003"
    )
    assertHasRule(
      validate({
        progressMarkdown: progressBlock(
          validSnapshot({ plan_rel_path: "docs/other.md" })
        ),
      }).failures,
      "B2D-PROGRESS-004"
    )
  })

  it("matches backend relative-path normalization", () => {
    const nfcSnapshot = validSnapshot({
      plan_rel_path: "docs/superpowers/plans/caf\u00e9.md",
    })
    const normalized = validate({
      planRelPath: "docs/superpowers/plans/cafe\u0301.md",
      progressMarkdown: progressBlock(nfcSnapshot),
    })
    assert.ok(
      !normalized.failures.some((failure) =>
        failure.startsWith("[B2D-PROGRESS-004]")
      )
    )

    if (process.platform === "win32") {
      const windowsSnapshot = validSnapshot({
        plan_rel_path: "docs/superpowers/plans/example.md",
      })
      const windowsCase = validate({
        planRelPath: "DOCS\\SUPERPOWERS\\PLANS\\EXAMPLE.MD",
        progressMarkdown: progressBlock(windowsSnapshot),
      })
      assert.ok(
        !windowsCase.failures.some((failure) =>
          failure.startsWith("[B2D-PROGRESS-004]")
        )
      )
    }

    for (const invalidPath of [
      "C:relative.md",
      "docs/./plans/example.md",
      "docs/plans/a|b.md",
      "docs/plans/a\u0001.md",
    ]) {
      assertHasRule(
        validate({
          planRelPath: invalidPath,
          progressMarkdown: progressBlock(
            validSnapshot({ plan_rel_path: invalidPath })
          ),
        }).failures,
        "B2D-PROGRESS-004"
      )
    }
  })

  it("requires unique Plan-backed Task indices and known statuses", () => {
    const badTasks = [
      { index: 1, status: "completed", runs: [] },
      { index: 1, status: "pending", runs: [] },
      { index: 9, status: "mystery", runs: [] },
    ]
    assertHasRule(
      validate({
        progressMarkdown: progressBlock(validSnapshot({ tasks: badTasks })),
      }).failures,
      "B2D-PROGRESS-005"
    )
  })

  it("validates generic run identity and replacement metadata", () => {
    const missingPair = validSnapshot()
    missingPair.tasks[1].runs[0].replacement_reason = null
    assertHasRule(
      validate({ progressMarkdown: progressBlock(missingPair) }).failures,
      "B2D-PROGRESS-006"
    )

    const wrongReason = validSnapshot()
    wrongReason.tasks[1].runs[0].replacement_reason = "workflow_recovery"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(wrongReason) }).failures,
      "B2D-PROGRESS-006"
    )

    const unstableKey = validSnapshot()
    unstableKey.tasks[1].runs[0].work_unit_key = ""
    assertHasRule(
      validate({ progressMarkdown: progressBlock(unstableKey) }).failures,
      "B2D-PROGRESS-006"
    )

    for (const mutate of [
      (run) => {
        run.work_unit_key = "task|1|implementer|codex|release"
      },
      (run) => {
        run.work_unit_key = "task|2|reviewer|codex|release"
      },
      (run) => {
        run.work_unit_key = "task|2|implementer|grok|release"
      },
      (run) => {
        run.work_unit_key = "task|2|implementer|codex|other"
      },
      (run) => {
        run.work_unit_key = "task|02|implementer|codex|release"
      },
      (run) => {
        run.agent_type = "not_an_agent"
        run.work_unit_key = "task|2|implementer|not_an_agent|release"
      },
    ]) {
      const mismatchedIdentity = validSnapshot()
      mutate(mismatchedIdentity.tasks[1].runs[0])
      assertHasRule(
        validate({
          progressMarkdown: progressBlock(mismatchedIdentity),
        }).failures,
        "B2D-PROGRESS-006"
      )
    }

    const scalarSnapshot = validSnapshot()
    const scalarPrefix = "task|2|implementer|codex|"
    const scalarProfile = "\ud83d\ude00".repeat(200 - [...scalarPrefix].length)
    scalarSnapshot.tasks[1].runs[0].profile_id = scalarProfile
    scalarSnapshot.tasks[1].runs[0].work_unit_key =
      scalarPrefix + scalarProfile
    assert.deepEqual(
      validate({ progressMarkdown: progressBlock(scalarSnapshot) }).failures,
      []
    )
  })

  it("rejects v2 and transport-only output fields recursively", () => {
    for (const [location, field] of [
      ["root", "workflow_id"],
      ["root", "manifest_revision"],
      ["root", "gate_id"],
      ["task", "artifact_digest"],
      ["run", "reviewed_task_id"],
      ["run", "recovery_authorization_id"],
      ["run", "completion_card"],
    ]) {
      const snapshot = validSnapshot()
      const target =
        location === "root"
          ? snapshot
          : location === "task"
            ? snapshot.tasks[0]
            : snapshot.tasks[0].runs[0]
      target[field] = "stale-v2-value"
      assertHasRule(
        validate({ progressMarkdown: progressBlock(snapshot) }).failures,
        "B2D-PROGRESS-007"
      )
    }
  })

  it("enforces serial Task and final-review state", () => {
    const twoActive = validSnapshot()
    twoActive.tasks[0].status = "in_progress"
    delete twoActive.tasks[0].commit
    assertHasRule(
      validate({ progressMarkdown: progressBlock(twoActive) }).failures,
      "B2D-PROGRESS-008"
    )

    const prematureFinal = validSnapshot({
      active_task_index: null,
      final_review_status: "completed",
    })
    prematureFinal.tasks[1].status = "pending"
    prematureFinal.tasks[1].runs = []
    assertHasRule(
      validate({ progressMarkdown: progressBlock(prematureFinal) }).failures,
      "B2D-PROGRESS-008"
    )

    const wrongActive = validSnapshot({ active_task_index: 1 })
    assertHasRule(
      validate({ progressMarkdown: progressBlock(wrongActive) }).failures,
      "B2D-PROGRESS-008"
    )

    const skippedEarlier = validSnapshot()
    skippedEarlier.tasks[0].status = "pending"
    skippedEarlier.tasks[0].runs = []
    delete skippedEarlier.tasks[0].commit
    assertHasRule(
      validate({ progressMarkdown: progressBlock(skippedEarlier) }).failures,
      "B2D-PROGRESS-008"
    )

    const skippedBlocked = validSnapshot({ active_task_index: 1 })
    skippedBlocked.tasks[0].status = "blocked"
    delete skippedBlocked.tasks[0].commit
    assertHasRule(
      validate({ progressMarkdown: progressBlock(skippedBlocked) }).failures,
      "B2D-PROGRESS-008"
    )

    const twoBlocked = validSnapshot({ active_task_index: 1 })
    twoBlocked.tasks[0].status = "blocked"
    twoBlocked.tasks[1].status = "blocked"
    delete twoBlocked.tasks[0].commit
    assertHasRule(
      validate({ progressMarkdown: progressBlock(twoBlocked) }).failures,
      "B2D-PROGRESS-008"
    )

    const untrackedBlocked = validSnapshot({ active_task_index: null })
    untrackedBlocked.tasks[1].status = "blocked"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(untrackedBlocked) }).failures,
      "B2D-PROGRESS-008"
    )
  })
})
