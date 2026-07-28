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

A Codex Plan Author owns every Plan. Parent must not implement Task code.
Parent must not write or rewrite the Plan. Author owns the Plan file and all
revisions. Invoke subagent-driven-development and writing-plans by name.

Plan production uses reviewer_cohort_node_ids, cohort_frozen, holistic rewrite,
user-approved requirements change, b2d_task_risk_v1.

Scoped re-review: owners of open Critical and Important findings only.
Full-group reset for material changes. Two non-improving rounds trigger stagnation handling.
Pre-admission risk correction uses material Plan revision. Post-admission uses cohort_frozen.

${route}

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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) => /normal route table must map Implementer/i.test(f)),
      `expected normal implementer failure, got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) => /two distinct Independent reviewer/i.test(f)),
      `expected dual-reviewer failure, got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some(
        (f) =>
          /Codex AND Grok/i.test(f) || /two distinct reviewer agents/i.test(f)
      ),
      `expected distinct high reviewers failure, got: ${failures.join("; ")}`
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
    assert.ok(
      failures.some((f) => /high route/i.test(f) && /reviewer/i.test(f)),
      `strict AND outside tables must not mask one-reviewer high table; got: ${failures.join("; ")}`
    )
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) => /normal|exact|mixed|identity|implementer/i.test(f)),
      `mixed normal implementer must fail; got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) =>
        /high|exact|mixed|identity|implementer|reviewer/i.test(f)
      ),
      `mixed high cells must fail; got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) => /extra|exact|unexpected|row|mapping/i.test(f)),
      `extra normal row must fail; got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) => /high/i.test(f)),
      `non-exact high mapping must fail; got: ${failures.join("; ")}`
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
    const failures = validateRouteTables(section)
    assert.ok(
      failures.some((f) =>
        /parenthetical|mixed|exact|identity|fallback/i.test(f)
      ),
      `parenthetical identity smuggling must fail; got: ${failures.join("; ")}`
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
    const { failures } = validateParentOwnership(skill)
    assert.ok(
      failures.some((f) => /parent|Plan|authorship|permission/i.test(f)),
      `expected contradictory ownership failure, got: ${failures.join("; ")}`
    )
  })

  it("rejects parent instructed to invoke writing-plans itself", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
使用 \`writing-plans\` 编写任何实施计划
`
    const { failures } = validateParentOwnership(skill)
    assert.ok(
      failures.some((f) => /writing-plans|parent authorship/i.test(f)),
      `expected parent writing-plans failure, got: ${failures.join("; ")}`
    )
  })

  it("rejects Parent writes Task code with urgency clause despite Author-owns", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent writes Task code when urgency requires it.
`
    const { failures } = validateParentOwnership(skill)
    assert.ok(
      failures.some((f) => /Task code|parent authorship|permission/i.test(f)),
      `expected Task code permission failure, got: ${failures.join("; ")}`
    )
  })

  it("rejects Parent implements Task and Parent writes Plan without modal verbs", () => {
    const skill = `Codex Plan Author owns every Plan. Author owns the Plan.
Parent must not implement Task code. Parent must not write or rewrite the Plan.
Parent implements Task.
Parent writes Plan.
`
    const { failures } = validateParentOwnership(skill)
    assert.ok(
      failures.some((f) => /parent/i.test(f)),
      `expected direct parent permission failures, got: ${failures.join("; ")}`
    )
    assert.ok(
      failures.length >= 1,
      `expected at least one ownership failure, got: ${failures.join("; ")}`
    )
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
    const { failures } = validateParentOwnership(skill)
    assert.ok(
      failures.some((f) => /Task code|parent authorship|permission/i.test(f)),
      `line-level prohibition must not mask permission; got: ${failures.join("; ")}`
    )
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

describe("forbidden literals", () => {
  it("rejects workflow_manifest_v1 even on a negated ban line", () => {
    const skill = baseValidSkill({
      extra: "Do not use workflow_manifest_v1 under any circumstances.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assert.ok(
      failures.some((f) => /workflow_manifest_v1/i.test(f)),
      `negated forbidden token must still fail; got: ${failures.join("; ")}`
    )
  })

  it("rejects schema_version = 1 on a never-use line", () => {
    const skill = baseValidSkill({
      extra: "Never set schema_version = 1 for manifests.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assert.ok(
      failures.some((f) => /schema_version/i.test(f)),
      `got: ${failures.join("; ")}`
    )
  })

  it("rejects pair_frozen even when saying avoid pair_frozen", () => {
    const skill = baseValidSkill({
      extra: "Avoid pair_frozen; use cohort_frozen instead.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assert.ok(
      failures.some((f) => /pair_frozen/i.test(f)),
      `got: ${failures.join("; ")}`
    )
  })

  it("rejects mode=legacy on a ban line", () => {
    const skill = baseValidSkill({
      extra: "mode=legacy is forbidden.\n",
    })
    const { failures } = validateSkillMarkdown(skill)
    assert.ok(
      failures.some((f) => /mode=legacy|mode\s*=\s*legacy/i.test(f)),
      `got: ${failures.join("; ")}`
    )
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
