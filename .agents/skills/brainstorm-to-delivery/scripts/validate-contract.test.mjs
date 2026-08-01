/**
 * Focused mutation/fixture tests for B2D skill contract validator.
 * Break each test is named to catch: wrong route rows, single high reviewer,
 * contradictory parent permission, and literal forbidden tokens (incl. negated).
 */
import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { describe, it } from "node:test"
import { fileURLToPath } from "node:url"
import {
  extractTaskRouteSection,
  findParentPermissionViolations,
  parseExactAgentIdentity,
  parseMarkdownTablesByHeading,
  validateParentOwnership,
  validateRouteTables,
  validateSkillMarkdown,
} from "./validate-contract.lib.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const realSkill = readFileSync(join(__dirname, "..", "SKILL.md"), "utf8")

const RULE_ID_RE = /^\[([A-Z0-9-]+)\]\s/

function failureRuleIds(failures) {
  return failures.map((failure) => {
    const match = failure.match(RULE_ID_RE)
    assert.ok(match, `failure lacks stable rule id: ${failure}`)
    return match[1]
  })
}

function assertHasRuleId(failures, expected) {
  assert.ok(
    failureRuleIds(failures).includes(expected),
    `expected ${expected}; got: ${failures.join("; ")}`
  )
}

const RECOVERY_PARAGRAPH = `Recovery is index-first. Treat recovery_sources and
actionable_task_routes as authoritative. Read each workspace report_file before
settlement, use get_session_info for bounded child transcripts, and use
get_delegation_status for selected run outcomes. Never depend on
inline finding summaries.

Recovery is status-first and resume-first. Cancellation-family evidence never
maps to unresumable, and tool_stalled_timeout is not a replacement source.
For tool_stalled_timeout, use a confirmed same-key continue; only genuine
unexpected transport loss may continue without confirmation when central policy permits.

Delegation recovery follows this exact ordered recipe: make the projected call;
receive typed recovery_confirmation_required; call
request_recovery_authorization; then replay the exact rejected continue or
replacement call with recovery_authorization_id and the same key, profile, and
action. Never persist recovery_authorization_id in status, ledger, report, or card.

Workflow recovery follows this exact ordered recipe: get_workflow_state; call
request_recovery_authorization; then call receipt-required recover_workflow.
An enabled catalog missing recover_workflow hard-blocks. recover_workflow never
generates a challenge. user_decision_required requires exact reset_plan_lineage
authorization tied to the displayed reason hash; its receipt is the durable
requirements-change reason and begins a new authorized stagnation baseline.

First admission freezes the key, role, agent, profile, and inherited continue
and replacement counters. Pre-admission profile or route correction is a
material Plan revision. Recovery never changes key/profile or resets inherited
consumption. Exhausted continue uses same-key budget_exhausted_continue replacement
only while replacement budget remains; after replacement consumption, block.

A platform-harvested and validated card settles. Failed or unavailable harvest
degrades the child and requires same-child continue to re-emit the card; prose
never settles. Normal Task review independently recomputes b2d_task_risk_v1;
migration, security/authorization, concurrency, persistence/state-machine,
externally visible compatibility, and ambiguity each trigger external Design review.

Before every delegation or continue, write ledger intent with intended key,
role, agent, profile, and action. Fill latest_task_id after admission and
reconcile from platform state after recovery.
`

/** Minimal skill that satisfies all contracts (hand-maintained fixture). */
function baseValidSkill(overrides = {}) {
  const route =
    overrides.route ??
    `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |

- High gate is strict AND. Both reviewers re-review the latest artifact after every fix.
- Every review covers reviewed_task_id and artifact_digest on the latest producer.
- One consolidated fix to the implementer.
`

  const body = `---
name: brainstorm-to-delivery
description: Use when a Codeg conversation provides a completed Brainstorm file and asks for a high-quality locally deliverable implementation.
---

# Brainstorm to Delivery

## Codeg roles and tools

| Route | Role | Agent |
| --- | --- | --- |
| Normal | Implementer / fixer | Grok |
| Normal | Independent reviewer | Codex |
| High | Implementer / fixer | Codex |
| High | Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| High | Independent reviewer 2 | Grok (independent child) |

A Codex Plan Author owns every Plan. Parent must not implement Task code.
Parent must not write or rewrite the Plan. Author owns the Plan file and all
revisions. Invoke subagent-driven-development and writing-plans by name.

Plan production uses reviewer_cohort_node_ids, cohort_frozen, holistic rewrite,
user-approved requirements change, b2d_task_risk_v1.

${RECOVERY_PARAGRAPH}

Scoped re-review: owners of open Critical and Important findings only.
Full-group reset for material changes. Two non-improving rounds trigger stagnation handling.
Pre-admission risk correction uses material Plan revision. Post-admission uses cohort_frozen.

${route}

## Quick reference under pressure

- Design Gate approved -> dispatch Plan Author automatically.
- Plan Gate approved -> run Workspace gate, then dispatch the first eligible Task automatically.
- Task Gate passed -> dispatch the next eligible Task or Final review automatically.
- Final review approved -> verify, commit, and report automatically.
- Only pause for a hard block, user_decision_required, or an unresolved choice that changes requirements, scope, architecture, or user data handling.
- If state is stale, call get_workflow_state and continue without extra user approval.

${overrides.extra ?? ""}
`
  return body
}

describe("extractTaskRouteSection", () => {
  it("isolates numbered Task route heading and does not fall back to whole skill", () => {
    const skill = `${baseValidSkill()}\n\n## elsewhere\n\nstrict AND only here\n`
    const section = extractTaskRouteSection(skill)
    assert.ok(section, "must find ## 4. Task route")
    assert.match(section, /## 4\. Task route/)
    assert.doesNotMatch(section, /elsewhere/)
  })

  it("returns null when Task route section is missing", () => {
    const skill = baseValidSkill({
      route: `## 4. Something else\n\nno routes\n`,
    })
    // Force missing by stripping after build
    const stripped = skill.replace(/## 4\. Task route[\s\S]*?(?=## |$)/, "")
    assert.equal(extractTaskRouteSection(stripped), null)
  })
})

describe("validateRouteTables", () => {
  it("fails when normal implementer row maps to Codex instead of Grok", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex |
| Independent reviewer 2 | Grok |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("fails when high route has only one reviewer row", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer | Codex |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("fails when high has two reviewer rows both Codex (not distinct Grok)", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex |
| Independent reviewer 2 | Codex |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("does not accept strict AND outside Task route as high dual-review proof", () => {
    // Wrong high table (one reviewer) + strict AND elsewhere in skill
    const skill = baseValidSkill({
      route: `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer | Codex |

`,
      extra: `## Notes\n\nRemember: high gate is strict AND somewhere else.\n`,
    })
    const { failures } = validateSkillMarkdown(skill)
    assertHasRuleId(failures, "B2D-007")
  })

  it("parses role/agent rows from real skill Task route tables", () => {
    const section = extractTaskRouteSection(realSkill)
    assert.ok(section)
    const tables = parseMarkdownTablesByHeading(section)
    assert.ok([...tables.keys()].some((k) => /normal route/i.test(k)))
    assert.ok([...tables.keys()].some((k) => /high route/i.test(k)))
    const failures = validateRouteTables(section)
    assert.deepEqual(failures, [])
  })

  it("fails when normal implementer cell mixes alternative identities", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok or Codex |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("fails when high implementer or reviewer cells mix identities", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex or Grok |
| Independent reviewer 1 | Codex or Grok |
| Independent reviewer 2 | Grok |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("fails when normal table has extra implementer or reviewer mapping", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |
| Extra implementer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("fails when high table is not exactly Codex implementer plus Codex and Grok reviewers", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok |
| Independent reviewer 1 | Codex |
| Independent reviewer 2 | Grok |
| Independent reviewer 3 | Codex |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })

  it("accepts parenthetical annotations but still requires one exact agent identity", () => {
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | **Grok** |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |
`
    assert.deepEqual(validateRouteTables(section), [])
  })

  it("rejects parentheticals that smuggle alternative agent identities", () => {
    // Reviewer example 1: Grok (or Codex)
    const mixedOr = parseExactAgentIdentity("Grok (or Codex)")
    assert.equal(mixedOr.ok, false, "Grok (or Codex) must fail closed")

    // Reviewer example 2: Codex (Grok fallback)
    const fallback = parseExactAgentIdentity("Codex (Grok fallback)")
    assert.equal(fallback.ok, false, "Codex (Grok fallback) must fail closed")

    // Harmless annotation control: still allowed
    const harmless = parseExactAgentIdentity("Codex (≠ implementer, ≠ Author)")
    assert.deepEqual(harmless, { ok: true, agent: "codex" })

    const child = parseExactAgentIdentity("Grok (independent child)")
    assert.deepEqual(child, { ok: true, agent: "grok" })

    // Table-level: smuggled identity must fail route validation
    const section = `## 4. Task route

### Normal route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Grok (or Codex) |
| Independent reviewer | Codex |

### High route

| Role | Agent |
| --- | --- |
| Implementer / fixer | Codex (Grok fallback) |
| Independent reviewer 1 | Codex (≠ implementer, ≠ Author) |
| Independent reviewer 2 | Grok (independent child) |
`
    assertHasRuleId(
      validateSkillMarkdown(baseValidSkill({ route: section })).failures,
      "B2D-007"
    )
  })
})

describe("validateParentOwnership", () => {
  const ownershipGrammarCases = [
    // Protected affirmative actions.
    ["protected affirmative", "Parent writes the Plan", "Parent writes Plan"],
    [
      "protected affirmative",
      "Parent authors Task code",
      "Parent writes Task code",
    ],
    [
      "protected affirmative",
      "Parent edits the Task code",
      "Parent writes Task code",
    ],
    [
      "protected affirmative",
      "Parent implements the Task",
      "Parent implements Task",
    ],
    [
      "protected affirmative",
      "Parent may write the Plan document",
      "Parent writes Plan",
    ],
    [
      "protected affirmative",
      "Parent invokes writing-plans",
      "parent invokes writing-plans (Author must)",
    ],
    [
      "protected affirmative",
      "Parent is writing the Plan",
      "Parent writes Plan",
    ],
    [
      "protected affirmative",
      "Parent is now writing the Plan",
      "Parent writes Plan",
    ],
    [
      "protected affirmative",
      "Parent writes the Plan itself",
      "Parent writes Plan",
    ],
    [
      "protected affirmative",
      "Parent writes the Plan (directly)",
      "Parent writes Plan",
    ],

    // A prohibition governs its action and a coordinated action-object series.
    ["protected prohibition", "Parent must not write the Plan", null],
    ["protected prohibition", "Parent does not implement the Task", null],
    ["protected prohibition", "Parent never authors Task code", null],
    ["protected prohibition", "Parent must not invoke writing-plans", null],
    [
      "shared prohibition",
      "Parent must not write the Plan or implement the Task",
      null,
    ],
    [
      "shared prohibition",
      "Parent must not invoke writing-plans or write Task code",
      null,
    ],
    [
      "shared prohibition",
      "Parent must not write the Plan and edit Task code",
      null,
    ],
    [
      "shared prohibition",
      "Parent must not write the Plan, implement the Task, or edit Task code",
      null,
    ],

    // Contrast and affirmative modals end the shared prohibition scope.
    [
      "contrast boundary",
      "Parent must not write the Plan but implements the Task",
      "Parent implements Task",
    ],
    [
      "contrast boundary",
      "Parent must not write the Plan; however, authors Task code",
      "Parent writes Task code",
    ],
    [
      "contrast boundary",
      "Parent must not invoke writing-plans yet writes the Plan",
      "Parent writes Plan",
    ],
    [
      "contrast boundary",
      "Parent must not write Task code, then implements the Task",
      "Parent implements Task",
    ],
    [
      "affirmative boundary",
      "Parent must not write the Plan and may implement the Task",
      "Parent implements Task",
    ],
    [
      "affirmative boundary",
      "Parent must not write the Plan and does implement the Task",
      "Parent implements Task",
    ],
    [
      "affirmative boundary",
      "Parent must not write the Plan and is allowed to implement the Task",
      "Parent implements Task",
    ],
    [
      "affirmative boundary",
      "Parent must not write the Plan and is authorized to invoke writing-plans",
      "parent invokes writing-plans (Author must)",
    ],
    [
      "affirmative boundary",
      "Parent does not write the Plan, reviews findings, and implements the Task",
      "Parent implements Task",
    ],
    [
      "unrelated prohibition",
      "Parent must not wait and writes Task code",
      "Parent writes Task code",
    ],
    [
      "unrelated prohibition",
      "Parent must not review findings and implements the Task",
      "Parent implements Task",
    ],
    [
      "unrelated prohibition",
      "Parent works without delay and invokes writing-plans",
      "parent invokes writing-plans (Author must)",
    ],
    [
      "unrelated prohibition",
      "Parent writes Task brief without delay and implements Task",
      "Parent implements Task",
    ],
    [
      "unrelated prohibition",
      "Parent must not wait - writes the Plan",
      "Parent writes Plan",
    ],
    [
      "unrelated prohibition",
      "Parent must not wait, writes Task code",
      "Parent writes Task code",
    ],

    // Coordination artifacts are not the protected Plan or Task code objects.
    [
      "legitimate artifact",
      "Parent writes Task brief for the implementer",
      null,
    ],
    ["legitimate artifact", "Parent writes the Task review report", null],
    [
      "legitimate artifact",
      "Parent authors Task acceptance criteria for the brief",
      null,
    ],
    [
      "legitimate artifact",
      "Parent edits Plan review findings without editing the Plan file",
      null,
    ],
    [
      "legitimate artifact",
      "Parent edits Plan quarterly review findings",
      null,
    ],
    ["legitimate artifact", "Parent edits Plan (review findings)", null],
    [
      "foreign subject",
      "Parent must not write the Plan; Author writes the Plan",
      null,
    ],
    [
      "foreign subject",
      "Parent writes Task brief, but Implementer writes Task code",
      null,
    ],

    // Explicit delegation assigns the infinitive action to the named child.
    ["delegated child", "Parent dispatches Author to write the Plan", null],
    ["delegated child", "Parent asks the Author to edit the Plan file", null],
    [
      "delegated child",
      "Parent instructs Implementer to write Task code",
      null,
    ],
    ["delegated child", "Parent requires Author to invoke writing-plans", null],
    ["delegated child", "Parent tells Reviewer to edit the Plan file", null],
    [
      "delegated child",
      "Parent tells the Reviewer to write the Task code",
      null,
    ],
    [
      "delegated child",
      "Parent dispatches the Reviewer to invoke writing-plans",
      null,
    ],
    [
      "modal delegated child",
      "Parent must dispatch Author to write the Plan",
      null,
    ],
    [
      "modal delegated child",
      "Parent may ask the Reviewer to edit the Plan file",
      null,
    ],
    [
      "modal delegated child",
      "Parent should instruct Implementer to write Task code",
      null,
    ],
    [
      "modal delegated child",
      "Parent can require Author to invoke writing-plans",
      null,
    ],
    [
      "modal delegated child",
      "Parent will tell Reviewer to edit the Plan file",
      null,
    ],

    // Delegation must not weaken direct or later explicit Parent ownership.
    ["direct Parent control", "Parent edits Plan file", "Parent writes Plan"],
    [
      "direct Parent control",
      "Parent writes Task code",
      "Parent writes Task code",
    ],
    [
      "direct Parent control",
      "Parent implements Task",
      "Parent implements Task",
    ],
    [
      "direct modal Parent control",
      "Parent may write the Plan",
      "Parent writes Plan",
    ],
    [
      "direct modal Parent control",
      "Parent must edit Plan file",
      "Parent writes Plan",
    ],
    [
      "direct modal Parent control",
      "Parent should write Task code",
      "Parent writes Task code",
    ],
    [
      "direct modal Parent control",
      "Parent can implement the Task",
      "Parent implements Task",
    ],
    [
      "direct modal Parent control",
      "Parent will invoke writing-plans",
      "parent invokes writing-plans (Author must)",
    ],
    [
      "delegation contrast",
      "Parent dispatches Author to write the Plan, but Parent writes Task code",
      "Parent writes Task code",
    ],
    [
      "delegation contrast",
      "Parent requires Reviewer to invoke writing-plans; however, Parent edits Plan file",
      "Parent writes Plan",
    ],
  ]

  for (const [category, sentence, expectedLabel] of ownershipGrammarCases) {
    it(`${category}: ${sentence}`, () => {
      const violations = findParentPermissionViolations(sentence)
      const expected = expectedLabel
        ? [`parent authorship permission present: ${expectedLabel}`]
        : []
      assert.deepEqual(
        violations,
        expected,
        `${JSON.stringify(sentence)} expected ${JSON.stringify(expected)}; got: ${violations.join("; ")}`
      )
    })
  }

  it("rejects contradictory parent Plan authoring even when Author owns is stated", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent writes the Plan when urgency requires it.
`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-006")
  })

  it("rejects parent instructed to invoke writing-plans itself", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
使用 \`writing-plans\` 编写任何实施计划
`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-006")
  })

  it("rejects Parent writes Task code with urgency clause despite Author-owns", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent writes Task code when urgency requires it.
`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-006")
  })

  it("rejects Parent implements Task and Parent writes Plan without modal verbs", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent implements Task.
Parent writes Plan.
`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-006")
  })

  it("preserves legitimate prohibition text without treating it as permission", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code.
Parent must not write or rewrite the Plan.
Parent never writes the Plan.
Parent does not implement Task code.
`
    const { failures } = validateParentOwnership(skill)
    assert.deepEqual(failures, [])
  })

  it("does not let an unrelated prohibition mask an affirmative parent permission clause", () => {
    // Reviewer example: unrelated negative clause + affirmative Task-code write
    const mixed =
      "Parent must not wait; Parent writes Task code when urgency requires it"
    const violations = findParentPermissionViolations(mixed)
    assert.ok(
      violations.some((v) => /Task code/i.test(v)),
      `mixed prohibition+permission must fail; got: ${violations.join("; ")}`
    )

    // Direct relevant prohibition control: still allowed (no violation)
    const banOnly = "Parent must not write Task code"
    assert.deepEqual(findParentPermissionViolations(banOnly), [])

    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent must not wait; Parent writes Task code when urgency requires it.
`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-006")
  })

  it("rejects the exact comma-but Task-writing mutation", () => {
    const violations = findParentPermissionViolations(
      "Parent must not wait, but Parent writes Task code when urgency requires it"
    )
    assert.ok(
      violations.some((v) => /Task code/i.test(v)),
      `comma-but permission must fail; got: ${violations.join("; ")}`
    )
  })

  it("rejects the exact and-joined Task-writing mutation", () => {
    const violations = findParentPermissionViolations(
      "Parent must not wait and Parent writes Task code when urgency requires it"
    )
    assert.ok(
      violations.some((v) => /Task code/i.test(v)),
      `and-joined permission must fail; got: ${violations.join("; ")}`
    )
  })

  it("rejects the exact optional-article Task-writing mutation", () => {
    const violations = findParentPermissionViolations(
      "Parent writes the Task code when urgency requires it"
    )
    assert.ok(
      violations.some((v) => /Task code/i.test(v)),
      `optional-article Task write must fail; got: ${violations.join("; ")}`
    )
  })

  it("rejects the exact optional-article Task-implementation mutation", () => {
    const violations = findParentPermissionViolations(
      "Parent implements the Task when urgency requires it"
    )
    assert.ok(
      violations.some((v) => /implements Task/i.test(v)),
      `optional-article Task implementation must fail; got: ${violations.join("; ")}`
    )
  })

  it("does not use punctuation boundaries to scope unrelated negatives", () => {
    const mutations = [
      "Parent must not wait, Parent writes Task code",
      "Parent must not wait: Parent writes Task code",
      "Parent must not wait - Parent writes Task code",
      "Parent must not wait / Parent writes Task code",
    ]

    for (const mutation of mutations) {
      const violations = findParentPermissionViolations(mutation)
      assert.ok(
        violations.some((v) => /Task code/i.test(v)),
        `unrelated negative must not mask ${JSON.stringify(mutation)}; got: ${violations.join("; ")}`
      )
    }
  })

  it("allows negation that governs the matched ownership action", () => {
    const prohibitions = [
      "Parent must not write the Task code",
      "Parent does not implement the Task",
      "Parent never writes the Plan",
      "Parent must not invoke `writing-plans`",
    ]

    for (const prohibition of prohibitions) {
      assert.deepEqual(
        findParentPermissionViolations(prohibition),
        [],
        `relevant prohibition must pass: ${prohibition}`
      )
    }
  })

  it("detects every ownership action after an unrelated negative", () => {
    const mutations = [
      ["Parent must not wait: Parent writes the Plan", /writes Plan/i],
      ["Parent must not wait, but Parent authors the Plan", /authors/i],
      ["Parent must not wait - Parent implements the Task", /implements Task/i],
      [
        "Parent must not wait and Parent invokes `writing-plans`",
        /writing-plans/i,
      ],
    ]

    for (const [mutation, expected] of mutations) {
      const violations = findParentPermissionViolations(mutation)
      assert.ok(
        violations.some((v) => expected.test(v)),
        `expected ${expected} for ${JSON.stringify(mutation)}; got: ${violations.join("; ")}`
      )
    }
  })

  it("accepts whitespace and Markdown emphasis without losing action scope", () => {
    const violations = findParentPermissionViolations(
      "**Parent**   writes   **the**   Task code"
    )
    assert.ok(
      violations.some((v) => /Task code/i.test(v)),
      `emphasized affirmative permission must fail; got: ${violations.join("; ")}`
    )

    assert.deepEqual(
      findParentPermissionViolations(
        "**Parent** must **not** write **the** Task code"
      ),
      []
    )
  })

  it("passes clean Author-owned skill without parent write permission", () => {
    const skill = baseValidSkill()
    const { failures } = validateParentOwnership(skill)
    assert.deepEqual(failures, [])
  })
})

describe("index-first recovery contract", () => {
  it("accepts the complete recovery fixture", () => {
    const { failures } = validateSkillMarkdown(baseValidSkill())
    assert.deepEqual(failures, [])
  })

  it("reports every recovery requirement when the paragraph is removed", () => {
    const mutated = baseValidSkill().replace(RECOVERY_PARAGRAPH, "")
    const { failures } = validateSkillMarkdown(mutated)
    const ids = failureRuleIds(failures)
    assert.equal(ids.filter((id) => id === "B2D-003").length, 6)
  })
})

describe("forbidden literals", () => {
  it("rejects workflow_manifest_v1 even on a negated ban line", () => {
    const skill = baseValidSkill({
      extra: "Do not use workflow_manifest_v1 under any circumstances.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assertHasRuleId(failures, "B2D-001")
  })

  it("rejects schema_version = 1 on a never-use line", () => {
    const skill = baseValidSkill({
      extra: "Never set schema_version = 1 for manifests.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assertHasRuleId(failures, "B2D-001")
  })

  it("rejects pair_frozen even when saying avoid pair_frozen", () => {
    const skill = baseValidSkill({
      extra: "Avoid pair_frozen; use cohort_frozen instead.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assertHasRuleId(failures, "B2D-001")
  })

  it("rejects mode=legacy on a ban line", () => {
    const skill = baseValidSkill({
      extra: "mode=legacy is forbidden.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assertHasRuleId(failures, "B2D-001")
  })
})

describe("real SKILL.md", () => {
  it("passes the production skill contract", () => {
    const { failures } = validateSkillMarkdown(realSkill)
    assert.deepEqual(
      failures,
      [],
      `production skill failures: ${failures.join("; ")}`
    )
  })
})

describe("phase-transition pressure reference", () => {
  it("requires automatic continuation after approved gates and names the only pause conditions", () => {
    const quickReference = realSkill.match(
      /^## Quick reference under pressure\s*$([\s\S]*?)(?=^## |(?![\s\S]))/im
    )?.[1]

    assert.ok(
      quickReference,
      "must include a Quick reference under pressure section"
    )
    assert.match(quickReference, /Design Gate approved[\s\S]*?Plan Author/i)
    assert.match(
      quickReference,
      /Plan Gate approved[\s\S]*?Workspace gate[\s\S]*?Task/i
    )
    assert.match(
      quickReference,
      /Task Gate passed[\s\S]*?(?:next eligible Task|Final)/i
    )
    assert.match(
      quickReference,
      /Final review approved[\s\S]*?verify[\s\S]*?commit[\s\S]*?report/i
    )
    assert.match(
      quickReference,
      /Only pause[\s\S]*?hard block[\s\S]*?user_decision_required[\s\S]*?requirements, scope, architecture, or user data handling/i
    )
    assert.match(
      quickReference,
      /stale[\s\S]*?get_workflow_state[\s\S]*?continue/i
    )
    assert.match(
      quickReference,
      /do not request[\s\S]*?(?:extra )?user approval/i
    )
  })
})

describe("stable validator rule ids", () => {
  const mutationCases = [
    ["B2D-001", (skill) => `${skill}\nworkflow_manifest_v1\n`],
    [
      "B2D-002",
      (skill) => skill.replace(/reviewer_cohort_node_ids/g, "reviewer cohort"),
    ],
    ["B2D-003", (skill) => skill.replace(/recovery_sources/g, "sources")],
    [
      "B2D-004",
      (skill) =>
        skill.replace(
          /description:.*$/m,
          "description: Use when running the high route and stagnation workflow."
        ),
    ],
    ["B2D-005", (skill) => `${skill}${"padding\n".repeat(501)}`],
    ["B2D-006", (skill) => `${skill}\nParent drafts the Plan.\n`],
    [
      "B2D-007",
      (skill) =>
        skill.replace(
          "| Implementer / fixer | Grok |",
          "| Implementer / fixer | Codex |"
        ),
    ],
    [
      "B2D-008",
      (skill) => `${skill}\nHigh route may pass with one reviewer.\n`,
    ],
    ["B2D-009", (skill) => skill.replace(/reviewed_task_id/g, "task id")],
    [
      "B2D-010",
      (skill) =>
        skill.replace(
          /Two non-improving rounds trigger stagnation handling\./,
          ""
        ),
    ],
    [
      "B2D-011",
      (skill) =>
        skill.replace(/subagent-driven-development/g, "delegated development"),
    ],
  ]

  for (const [ruleId, mutate] of mutationCases) {
    it(`uses ${ruleId} for its existing fixture family`, () => {
      const { failures } = validateSkillMarkdown(mutate(baseValidSkill()))
      assertHasRuleId(failures, ruleId)
    })
  }

  it("positive fixture has no failure ids", () => {
    const { failures } = validateSkillMarkdown(baseValidSkill())
    assert.deepEqual(failureRuleIds(failures), [])
  })
})

describe("B2D-012 automatic phase transition contract", () => {
  const requiredLines = [
    "Design Gate approved -> dispatch Plan Author automatically.",
    "Plan Gate approved -> run Workspace gate, then dispatch the first eligible Task automatically.",
    "Task Gate passed -> dispatch the next eligible Task or Final review automatically.",
    "Final review approved -> verify, commit, and report automatically.",
  ]

  for (const requiredLine of requiredLines) {
    it(`rejects removal of ${requiredLine}`, () => {
      const { failures } = validateSkillMarkdown(
        baseValidSkill().replace(requiredLine, "")
      )
      assertHasRuleId(failures, "B2D-012")
    })
  }

  it("rejects an extra user-approval pause", () => {
    const { failures } = validateSkillMarkdown(
      baseValidSkill({
        extra: "Pause for user approval before starting every next phase.\n",
      })
    )
    assertHasRuleId(failures, "B2D-012")
  })

  for (const condition of [
    "hard block",
    "user_decision_required",
    "requirements, scope, architecture, or user data handling",
  ]) {
    it(`rejects omission of hard pause condition ${condition}`, () => {
      const { failures } = validateSkillMarkdown(
        baseValidSkill().replace(condition, "ordinary work")
      )
      assertHasRuleId(failures, "B2D-012")
    })
  }
})

describe("authoritative route surface parity", () => {
  it("uses B2D-013 when the top normal row is mutated", () => {
    const skill = baseValidSkill().replace(
      "| Normal | Implementer / fixer | Grok |",
      "| Normal | Implementer / fixer | Codex |"
    )
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-013")
  })

  it("uses B2D-013 when the top high row is removed", () => {
    const skill = baseValidSkill().replace(
      "| High | Independent reviewer 2 | Grok (independent child) |\n",
      ""
    )
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-013")
  })

  it("uses B2D-014 when only the numbered route differs", () => {
    const firstNumbered = baseValidSkill().lastIndexOf(
      "| Implementer / fixer | Grok |"
    )
    const skill = `${baseValidSkill().slice(0, firstNumbered)}| Implementer / fixer | Codex |${baseValidSkill().slice(firstNumbered + "| Implementer / fixer | Grok |".length)}`
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-014")
  })
})

describe("authorized recovery validator contract", () => {
  for (const token of [
    "request_recovery_authorization",
    "recovery_authorization_id",
    "recovery_confirmation_required",
    "recover_workflow",
  ]) {
    it(`uses B2D-R001 when ${token} is absent`, () => {
      const { failures } = validateSkillMarkdown(
        baseValidSkill().replaceAll(token, `missing_${token}`)
      )
      assertHasRuleId(failures, "B2D-R001")
    })

    it(`uses B2D-R001 when ${token} appears only in a negated sentence`, () => {
      const stripped = baseValidSkill().replaceAll(token, `missing_${token}`)
      const { failures } = validateSkillMarkdown(
        `${stripped}\nNever call ${token}.\n`
      )
      assertHasRuleId(failures, "B2D-R001")
    })

    it(`uses B2D-R001 when ${token} has a negated call despite the positive recipe`, () => {
      const extra =
        token === "recovery_authorization_id"
          ? `Do not use ${token} for the replay.\n`
          : `Do not call ${token} during recovery.\n`
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra })).failures,
        "B2D-R001"
      )
    })
  }

  const sequenceMutations = [
    "Call request_recovery_authorization before recovery_confirmation_required.",
    "After authorization, construct a similar replacement call instead of replaying the exact rejected call.",
    "After rejection, change the key and profile before replaying the action.",
  ]
  for (const mutation of sequenceMutations) {
    it(`uses B2D-R002 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R002"
      )
    })
  }

  for (const cause of [
    "parent_canceled",
    "parent_turn_failed",
    "join_abandoned",
    "user_cancelled",
    "tool_stalled_timeout",
  ]) {
    it(`uses B2D-R003 when ${cause} maps affirmatively to unresumable`, () => {
      const skill = baseValidSkill({
        extra: `${cause} maps directly to replacement_reason=unresumable.\n`,
      })
      assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-R003")
    })
  }

  it("allows a direct cancellation prohibition", () => {
    const failures = validateSkillMarkdown(
      baseValidSkill({ extra: "Never map cancellation to unresumable.\n" })
    ).failures
    assert.ok(!failureRuleIds(failures).includes("B2D-R003"))
  })

  it("allows a direct stall prohibition", () => {
    const failures = validateSkillMarkdown(
      baseValidSkill({
        extra: "tool_stalled_timeout is not a replacement source.\n",
      })
    ).failures
    assert.ok(!failureRuleIds(failures).includes("B2D-R003"))
  })

  it("allows an all-safe multi-cause cancellation clause", () => {
    const failures = validateSkillMarkdown(
      baseValidSkill({
        extra:
          "parent_canceled is not a replacement source and parent_turn_failed never maps to unresumable and join_abandoned is not a replacement source and user_cancelled never maps to replacement and tool_stalled_timeout is not a replacement source.\n",
      })
    ).failures
    assert.ok(!failureRuleIds(failures).includes("B2D-R003"))
  })

  it("does not let unrelated negation mask an affirmative mapping", () => {
    for (const mutation of [
      "Never discard work; parent_canceled maps to replacement_reason=unresumable.",
      "Never discard work, parent_canceled maps to replacement_reason=unresumable.",
      "Never lose history, tool_stalled_timeout maps to replacement_reason=unresumable.",
      "parent_canceled is not a replacement source but tool_stalled_timeout maps to replacement_reason=unresumable.",
    ]) {
      const skill = baseValidSkill({ extra: `${mutation}\n` })
      assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-R003")
    }
  })

  for (const mutation of [
    "tool_stalled_timeout continues without confirmation.",
    "tool_stalled_timeout continues automatically.",
    "tool_stalled_timeout uses replacement before continue.",
  ]) {
    it(`uses B2D-R004 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R004"
      )
    })
  }

  for (const mutation of [
    "Call recover_workflow before request_recovery_authorization.",
    "Workflow recovery may skip get_workflow_state when the id is known.",
    "An enabled catalog missing recover_workflow may proceed with a fallback.",
  ]) {
    it(`uses B2D-R005 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R005"
      )
    })
  }

  it("uses B2D-R006 for an unreceipted lineage reset", () => {
    const skill = baseValidSkill({
      extra:
        "user_decision_required may reset the Plan lineage without reset_plan_lineage authorization, displayed reason hash, or a new baseline.\n",
    })
    assertHasRuleId(validateSkillMarkdown(skill).failures, "B2D-R006")
  })

  for (const mutation of [
    "Recovery may change the admitted key or profile.",
    "Recovery resets inherited continue and replacement consumption.",
  ]) {
    it(`uses B2D-R007 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R007"
      )
    })
  }

  for (const mutation of [
    "Reject a platform-harvested and validated card.",
    "Prose approval settles the recovery card.",
    "A degraded child may finish without same-child card re-emission.",
  ]) {
    it(`uses B2D-R008 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R008"
      )
    })
  }

  const designTriggers = [
    "Migration",
    "Security/authorization",
    "Concurrency",
    "Persistence/state-machine",
    "Externally visible compatibility",
    "Ambiguity",
  ]
  for (const mutation of [
    "Normal Task review copies b2d_task_risk_v1 from the manifest.",
    ...designTriggers.flatMap((trigger) => [
      `${trigger} does not trigger external Design review.`,
      `${trigger} never triggers external Design review.`,
      `${trigger} must never trigger external Design review.`,
      `${trigger} should not trigger external Design review.`,
      `${trigger} never triggers an external Design review.`,
    ]),
  ]) {
    it(`uses B2D-R009 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R009"
      )
    })
  }

  for (const control of designTriggers.flatMap((trigger) => [
    `${trigger} must trigger an external Design review.`,
    `${trigger} may trigger an external Design review.`,
  ])) {
    it(`allows positive Design-trigger modal control: ${control}`, () => {
      const failures = validateSkillMarkdown(
        baseValidSkill({ extra: `${control}\n` })
      ).failures
      assert.ok(!failureRuleIds(failures).includes("B2D-R009"))
    })
  }

  for (const mutation of [
    "At continue exhaustion, mint a new key and profile.",
    "At continue exhaustion, replace with replacement_reason=unresumable.",
    "After replacement consumption, replace again.",
  ]) {
    it(`uses B2D-R010 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R010"
      )
    })
  }

  for (const mutation of [
    "Write ledger intent after the delegation mutation.",
    "Ledger intent may omit intended action and identity.",
    "Skip platform-state reconciliation after recovery.",
  ]) {
    it(`uses B2D-R011 for ${mutation}`, () => {
      assertHasRuleId(
        validateSkillMarkdown(baseValidSkill({ extra: `${mutation}\n` }))
          .failures,
        "B2D-R011"
      )
    })
  }
})

describe("expanded Parent ownership grammar", () => {
  for (const verb of [
    "draft",
    "drafts",
    "drafting",
    "drafted",
    "compose",
    "composes",
    "composing",
    "composed",
    "generate",
    "generates",
    "generating",
    "generated",
  ]) {
    it(`uses B2D-006 for Parent ${verb} Plan`, () => {
      assertHasRuleId(
        validateSkillMarkdown(
          baseValidSkill({ extra: `Parent ${verb} the Plan.\n` })
        ).failures,
        "B2D-006"
      )
    })

    it(`uses B2D-006 for Parent ${verb} Task code`, () => {
      assertHasRuleId(
        validateSkillMarkdown(
          baseValidSkill({ extra: `Parent ${verb} Task code.\n` })
        ).failures,
        "B2D-006"
      )
    })
  }

  for (const verb of [
    "起草",
    "拟写",
    "编写",
    "撰写",
    "创作",
    "生成",
    "改写",
    "重写",
    "编辑",
    "修改",
  ]) {
    it(`uses B2D-006 for 父会话${verb} Plan`, () => {
      assertHasRuleId(
        validateSkillMarkdown(
          baseValidSkill({ extra: `父会话${verb} Plan。\n` })
        ).failures,
        "B2D-006"
      )
    })

    it(`uses B2D-006 for 父会话${verb} Task code`, () => {
      assertHasRuleId(
        validateSkillMarkdown(
          baseValidSkill({ extra: `父会话${verb} Task code。\n` })
        ).failures,
        "B2D-006"
      )
    })
  }

  it("allows action-scoped negative controls and coordination artifacts", () => {
    const skill = baseValidSkill({
      extra: `Parent must not draft or compose the Plan.
父会话不得编写 Plan。
Parent drafts the Task brief and composes review findings.
`,
    })
    assert.ok(
      !failureRuleIds(validateSkillMarkdown(skill).failures).includes("B2D-006")
    )
  })
})
