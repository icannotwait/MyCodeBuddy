import assert from "node:assert/strict"
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
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
import { readUtf8FileBounded } from "./validate-contract.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const realSkill = readFileSync(join(__dirname, "..", "SKILL.md"), "utf8")
const planRelPath = "docs/superpowers/plans/example.md"

const METADATA_ONLY_SKILL = `---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm to Delivery

Use Simple documents and generic delegation to deliver the approved work.
`

const CONTRACT_SKILL = `---
name: brainstorm-to-delivery
description: Use when a completed Brainstorm must become a local delivery.
---

# Brainstorm to Delivery

## 1. Discover current truth

After compaction, inspect live schemas for `register_simple_workflow`,
`delegate_to_agent`, `continue_delegation`, `get_delegation_status`, and
`request_recovery_authorization`.

## 2. Plan then register

Before any reviewer dispatch, create progress. Use `writing-plans` to write
the Plan. After the Plan exists, call `register_simple_workflow`, refresh every
Task as pending, then request Plan review.

## 3. Mutate progress around delegation

Use one `codeg-simple-progress-v1` block. Before delegation, write reserving
intent. After every observed state change, refresh the block.

## 4. Protect the workspace

Inspect `git status`, staged diff, and unstaged diff. Preserve user changes.

## 5. Execute Tasks serially

Execute Tasks serially. Use `delegate_to_agent` for a first run and
`continue_delegation` for later work with one stable `work_unit_key`.
Use Grok to implement, Codex to review, and join `task_ids`.

## 6. Recover generic runs

For `recovery_confirmation_required`, request authorization and replay with
`recovery_authorization_id`. Handle `fresh_dispatch`, `unresumable`,
`budget_exhausted_continue`, `not_supported`, `admission_failed`, and
`admission_unknown` without changing identity.

| Recovery rail | Limit |
| --- | --- |
| Unexpected continuations | 2 |
| Logical replacement | 1 |

## 7. Review and deliver

Set `final_review_status` to in_progress, request an independent Codex final
review, then set it to completed and commit only owned changes locally.
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
            task_id: "task-2-a",
            child_conversation_id: 50,
            state: "failed",
            work_unit_key: "task|2|implementer|codex|release",
            recovery_count: 1,
            replaced_task_id: null,
            replacement_reason: null,
          },
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
    skillMarkdown: CONTRACT_SKILL,
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

function assertHasText(failures, text) {
  assert.ok(
    failures.some((failure) => failure.includes(text)),
    `expected ${JSON.stringify(text)}; got ${failures.join("; ")}`
  )
}

function removeNumberedSection(skill, index) {
  const start = skill.indexOf(`## ${index}.`)
  const end =
    index === 7 ? skill.length : skill.indexOf(`## ${index + 1}.`, start)
  assert.ok(start >= 0 && end > start)
  return skill.slice(0, start) + skill.slice(end)
}

function swapPlanAndProgressSections(skill) {
  const second = skill.indexOf("## 2.")
  const third = skill.indexOf("## 3.")
  const fourth = skill.indexOf("## 4.")
  assert.ok(second >= 0 && third > second && fourth > third)
  return (
    skill.slice(0, second) +
    skill.slice(third, fourth) +
    skill.slice(second, third) +
    skill.slice(fourth)
  )
}

describe("Skill metadata contract", () => {
  it("rejects metadata-only guidance without the ordered contract", () => {
    assertHasRule(
      validateSkillMarkdown(METADATA_ONLY_SKILL).failures,
      "B2D-SKILL-004"
    )
  })

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
        validateSkillMarkdown(`${CONTRACT_SKILL}\n${identifier}\n`).failures,
        "B2D-SKILL-003"
      )
    }
  })

  it("requires only name and a trigger-only Use when description", () => {
    assertHasRule(
      validateSkillMarkdown(
        CONTRACT_SKILL.replace("name:", "title:")
      ).failures,
      "B2D-SKILL-001"
    )
    assertHasRule(
      validateSkillMarkdown(
        CONTRACT_SKILL.replace(
          "description: Use when",
          "description: This runs"
        )
      ).failures,
      "B2D-SKILL-001"
    )
    assertHasRule(
      validateSkillMarkdown(
        CONTRACT_SKILL.replace(
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

  it("requires all seven ordered workflow phases", () => {
    for (let index = 1; index <= 7; index += 1) {
      assertHasRule(
        validateSkillMarkdown(removeNumberedSection(CONTRACT_SKILL, index))
          .failures,
        "B2D-SKILL-004"
      )
    }
    assertHasRule(
      validateSkillMarkdown(swapPlanAndProgressSections(CONTRACT_SKILL))
        .failures,
      "B2D-SKILL-004"
    )
  })

  it("requires phase-specific ordering and safety rails", () => {
    const mutations = [
      CONTRACT_SKILL.replace("`writing-plans`", "planning"),
      CONTRACT_SKILL.replace(
        "Use `writing-plans` to write\nthe Plan. After the Plan exists, " +
          "call `register_simple_workflow`",
        "Call `register_simple_workflow`, then use `writing-plans` to " +
          "write the Plan. The Plan exists afterward"
      ),
      CONTRACT_SKILL.replace(
        "Before delegation, write reserving\nintent. After every observed " +
          "state change",
        "After every observed state change, write reserving intent before " +
          "delegation"
      ),
      CONTRACT_SKILL.replace("Execute Tasks serially.", "Execute Tasks."),
      CONTRACT_SKILL.replace("| Unexpected continuations | 2 |", ""),
      CONTRACT_SKILL.replace("`final_review_status`", "final status"),
    ]
    for (const mutation of mutations) {
      assertHasRule(
        validateSkillMarkdown(mutation).failures,
        "B2D-SKILL-004"
      )
    }
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

  it("requires progress Tasks to exactly match ordered Plan Tasks", () => {
    for (const tasks of [
      [],
      [validSnapshot().tasks[1]],
      [validSnapshot().tasks[0]],
      [...validSnapshot().tasks].reverse(),
    ]) {
      assertHasRule(
        validate({
          progressMarkdown: progressBlock(
            validSnapshot({ active_task_index: null, tasks })
          ),
        }).failures,
        "B2D-PROGRESS-005"
      )
    }
  })

  it("validates generic run identity and replacement metadata", () => {
    const missingPair = validSnapshot()
    missingPair.tasks[1].runs[1].replacement_reason = null
    assertHasRule(
      validate({ progressMarkdown: progressBlock(missingPair) }).failures,
      "B2D-PROGRESS-006"
    )

    const wrongReason = validSnapshot()
    wrongReason.tasks[1].runs[1].replacement_reason = "workflow_recovery"
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
    for (const run of scalarSnapshot.tasks[1].runs) {
      run.profile_id = scalarProfile
      run.work_unit_key = scalarPrefix + scalarProfile
    }
    assert.deepEqual(
      validate({ progressMarkdown: progressBlock(scalarSnapshot) }).failures,
      []
    )
  })

  it("keeps one stable identity for every Task-role lineage", () => {
    const changedAgent = validSnapshot()
    changedAgent.tasks[1].runs[1].agent_type = "grok"
    changedAgent.tasks[1].runs[1].work_unit_key =
      "task|2|implementer|grok|release"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(changedAgent) }).failures,
      "B2D-PROGRESS-006"
    )

    const changedProfile = validSnapshot()
    changedProfile.tasks[1].runs[1].profile_id = "other"
    changedProfile.tasks[1].runs[1].work_unit_key =
      "task|2|implementer|codex|other"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(changedProfile) }).failures,
      "B2D-PROGRESS-006"
    )

    const literalNone = validSnapshot()
    literalNone.tasks[0].runs[0].profile_id = "none"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(literalNone) }).failures,
      "B2D-PROGRESS-006"
    )
  })

  it("enforces unique task IDs and same-lineage replacement sources", () => {
    const duplicateTaskId = validSnapshot()
    duplicateTaskId.tasks[0].runs[1].task_id = "task-1-impl"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(duplicateTaskId) }).failures,
      "B2D-PROGRESS-006"
    )

    const missingSource = validSnapshot()
    missingSource.tasks[1].runs[1].replaced_task_id = "not-in-lineage"
    assertHasRule(
      validate({ progressMarkdown: progressBlock(missingSource) }).failures,
      "B2D-PROGRESS-006"
    )
  })

  it("allows one logical replacement and identical admission retries", () => {
    const secondReplacement = validSnapshot()
    secondReplacement.tasks[1].runs.push({
      ...secondReplacement.tasks[1].runs[1],
      task_id: "task-2-c",
      child_conversation_id: 52,
      replaced_task_id: "task-2-b",
    })
    assertHasRule(
      validate({ progressMarkdown: progressBlock(secondReplacement) }).failures,
      "B2D-PROGRESS-006"
    )

    const admittedDuplicate = validSnapshot()
    admittedDuplicate.tasks[1].runs.push({
      ...admittedDuplicate.tasks[1].runs[1],
      task_id: "task-2-c",
      child_conversation_id: 52,
    })
    assertHasText(
      validate({ progressMarkdown: progressBlock(admittedDuplicate) }).failures,
      "prior attempt was admitted"
    )

    const admissionRetry = validSnapshot()
    admissionRetry.tasks[1].runs[1].child_conversation_id = null
    admissionRetry.tasks[1].runs[1].state = "failed"
    admissionRetry.tasks[1].runs.push({
      ...admissionRetry.tasks[1].runs[1],
      task_id: "task-2-c",
      child_conversation_id: null,
      state: "reserving",
    })
    assert.deepEqual(
      validate({ progressMarkdown: progressBlock(admissionRetry) }).failures,
      []
    )
  })

  it("enforces recovery rails and backend integer ranges", () => {
    const exactBoundaries = validSnapshot()
    exactBoundaries.tasks[0].runs[0].child_conversation_id = 0x7fffffff
    exactBoundaries.tasks[1].runs[1].recovery_count = 2
    assert.deepEqual(
      validate({ progressMarkdown: progressBlock(exactBoundaries) }).failures,
      []
    )

    const childOverflow = validSnapshot()
    childOverflow.tasks[0].runs[0].child_conversation_id = 0x80000000
    assertHasText(
      validate({ progressMarkdown: progressBlock(childOverflow) }).failures,
      "signed 32-bit"
    )

    const railOverflow = validSnapshot()
    railOverflow.tasks[1].runs[1].recovery_count = 3
    assertHasText(
      validate({ progressMarkdown: progressBlock(railOverflow) }).failures,
      "at most 2"
    )

    const u32Boundary = validSnapshot()
    u32Boundary.tasks[1].runs[1].recovery_count = 0xffffffff
    const u32BoundaryFailures = validate({
      progressMarkdown: progressBlock(u32Boundary),
    }).failures
    assertHasText(u32BoundaryFailures, "at most 2")
    assert.ok(
      !u32BoundaryFailures.some((failure) =>
        failure.includes("unsigned 32-bit")
      ),
      "expected the u32 boundary to remain in range; got " +
        u32BoundaryFailures.join("; ")
    )

    const u32Overflow = validSnapshot()
    u32Overflow.tasks[1].runs[1].recovery_count = 0x100000000
    assertHasText(
      validate({ progressMarkdown: progressBlock(u32Overflow) }).failures,
      "unsigned 32-bit"
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

describe("bounded CLI file reads", () => {
  it("accepts an exact UTF-8 byte boundary and rejects overflow", () => {
    const directory = mkdtempSync(join(tmpdir(), "b2d-validator-"))
    const fixture = join(directory, "fixture.md")
    try {
      writeFileSync(fixture, "\u00e9")
      assert.equal(readUtf8FileBounded(fixture, 2, "fixture"), "\u00e9")
      assert.throws(
        () => readUtf8FileBounded(fixture, 1, "fixture"),
        /fixture exceeds 1 bytes/
      )

      writeFileSync(fixture, Buffer.from([0xc3]))
      assert.throws(
        () => readUtf8FileBounded(fixture, 1, "fixture"),
        /fixture is not valid UTF-8/
      )
    } finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })
})
