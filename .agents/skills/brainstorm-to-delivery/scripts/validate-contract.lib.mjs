/**
 * Pure contract checks for brainstorm-to-delivery SKILL.md (v2 adaptive routing).
 * Structural/machine-enforceable rules live here; judgment rules stay in the Skill body.
 */

const FORBIDDEN = [
  [/workflow_manifest_v1/, "forbidden workflow_manifest_v1"],
  [/schema_version\s*[=:]\s*1/, "forbidden schema_version = 1"],
  [/pair_frozen/, "forbidden pair_frozen (use cohort_frozen)"],
  [/mode\s*=\s*legacy/i, "forbidden mode=legacy"],
]

const REQUIRED = [
  [/Codex Plan Author/, "Codex Plan Author"],
  [/writing-plans/, "writing-plans"],
  [/b2d_task_risk_v1/, "b2d_task_risk_v1"],
  [/reviewer_cohort_node_ids/, "reviewer_cohort_node_ids"],
  [/cohort_frozen/, "cohort_frozen"],
  [/holistic rewrite/i, "holistic rewrite"],
  [/user-approved requirements change/i, "user-approved requirements change"],
  [/reviewed_task_id/, "reviewed_task_id"],
  [/artifact_digest/, "artifact_digest"],
]

/**
 * Isolate the numbered Task route section (`## 4. Task route` or `## Task route`).
 * Returns null when the section is missing.
 */
export function extractTaskRouteSection(skill) {
  const start = skill.search(/^##\s*(?:\d+\.\s*)?Task route\b/im)
  if (start < 0) return null
  const from = skill.slice(start)
  // Next ## heading after this one (not the current line).
  const nextRel = from.slice(1).search(/^##\s/m)
  if (nextRel < 0) return from.trimEnd() + (from.endsWith("\n") ? "\n" : "")
  return from.slice(0, nextRel + 1)
}

/**
 * Parse Markdown tables under a subsection heading inside `section`.
 * Returns Map of heading-lower -> array of {cells: string[]}
 */
export function parseMarkdownTablesByHeading(section) {
  const result = new Map()
  if (!section) return result

  const lines = section.split(/\r?\n/)
  let currentHeading = null
  let tableRows = null

  const flush = () => {
    if (currentHeading && tableRows && tableRows.length) {
      result.set(currentHeading, tableRows)
    }
    tableRows = null
  }

  for (const raw of lines) {
    const line = raw.trimEnd()
    const heading = line.match(/^#{2,4}\s+(.+?)\s*$/)
    if (heading) {
      flush()
      currentHeading = heading[1].trim().toLowerCase()
      continue
    }
    if (!/^\|/.test(line.trim())) {
      if (tableRows) flush()
      continue
    }
    const cells = line
      .trim()
      .replace(/^\|/, "")
      .replace(/\|$/, "")
      .split("|")
      .map((c) => c.trim())
    // separator row
    if (cells.every((c) => /^:?-{3,}:?$/.test(c))) {
      if (!tableRows) tableRows = []
      continue
    }
    if (!tableRows) tableRows = []
    tableRows.push({ cells })
  }
  flush()
  return result
}

function roleCell(cells) {
  return (cells[0] || "").toLowerCase()
}

function agentCell(cells) {
  return (cells[1] || "").toLowerCase()
}

/**
 * Assert normal/high route tables map roles to exact agents.
 * @returns {string[]} failure messages
 */
export function validateRouteTables(taskRouteSection) {
  const failures = []
  if (!taskRouteSection) {
    failures.push(
      "missing Task route section (expected `## Task route` or `## N. Task route`)"
    )
    return failures
  }

  const tables = parseMarkdownTablesByHeading(taskRouteSection)
  const normalKey = [...tables.keys()].find((k) => /^normal route\b/i.test(k))
  const highKey = [...tables.keys()].find((k) =>
    /^high(?:-risk)? route\b/i.test(k)
  )

  if (!normalKey) {
    failures.push("Task route missing `### Normal route` table")
  } else {
    const rows = tables.get(normalKey).filter((r) => {
      const role = roleCell(r.cells)
      return role && role !== "role"
    })
    const impl = rows.find((r) => /implementer/.test(roleCell(r.cells)))
    const rev = rows.find((r) => /reviewer/.test(roleCell(r.cells)))
    if (!impl || !/\bgrok\b/i.test(agentCell(impl.cells))) {
      failures.push(
        "normal route table must map Implementer/fixer to Grok"
      )
    }
    if (!rev || !/\bcodex\b/i.test(agentCell(rev.cells))) {
      failures.push(
        "normal route table must map Independent reviewer to Codex"
      )
    }
    // Reject wrong agents even if labels exist
    if (impl && /\bcodex\b/i.test(agentCell(impl.cells)) && !/\bgrok\b/i.test(agentCell(impl.cells))) {
      failures.push("normal route implementer must be Grok, not Codex")
    }
  }

  if (!highKey) {
    failures.push("Task route missing `### High route` table")
  } else {
    const rows = tables.get(highKey).filter((r) => {
      const role = roleCell(r.cells)
      return role && role !== "role"
    })
    const impl = rows.find((r) => /implementer/.test(roleCell(r.cells)))
    const reviewers = rows.filter((r) => /reviewer/.test(roleCell(r.cells)))
    if (!impl || !/\bcodex\b/i.test(agentCell(impl.cells))) {
      failures.push(
        "high route table must map Implementer/fixer to Codex"
      )
    }
    if (reviewers.length < 2) {
      failures.push(
        "high route table must list two distinct Independent reviewer rows"
      )
    } else {
      const agents = reviewers.map((r) => agentCell(r.cells))
      const hasCodex = agents.some((a) => /\bcodex\b/i.test(a))
      const hasGrok = agents.some((a) => /\bgrok\b/i.test(a))
      if (!hasCodex || !hasGrok) {
        failures.push(
          "high route table must assign independent Codex AND Grok reviewers"
        )
      }
      // Distinct agents across the two reviewer rows (not both same single agent)
      const primary = agents.map((a) => {
        if (/\bcodex\b/i.test(a)) return "codex"
        if (/\bgrok\b/i.test(a)) return "grok"
        return a
      })
      if (primary[0] && primary[0] === primary[1]) {
        failures.push(
          "high route must use two distinct reviewer agents (not the same agent twice)"
        )
      }
    }
  }

  return failures
}

/**
 * Reject any parent Plan/Task authorship permission; require explicit bans.
 * @returns {{ failures: string[], notes: string[] }}
 */
export function validateParentOwnership(skill) {
  const failures = []
  const notes = []

  const parentPermissionPatterns = [
    [
      /parent\s+(?:may|can|should|must)\s+(?:write|author|rewrite|implement|edit)\s+(?:the\s+)?(?:Plan|Task)/i,
      "parent permission to write/implement Plan or Task",
    ],
    [
      /父会话(?:可以|可|应当|应|必须).*(?:编写|撰写|改写|实现|修复).*(?:计划|Plan|Task|代码)/i,
      "parent Chinese permission to author Plan/Task",
    ],
    [
      /使用\s*`writing-plans`\s*编写任何实施计划/,
      "parent instructed to invoke writing-plans itself",
    ],
    [
      /parent.*invoke(?:s|ing)?\s+`?writing-plans`?/i,
      "parent invokes writing-plans (Author must)",
    ],
    [
      /父会话.*`writing-plans`/,
      "parent Chinese writing-plans ownership",
    ],
  ]

  for (const [re, label] of parentPermissionPatterns) {
    // Ignore lines that only ban the behavior
    const lines = skill.split(/\r?\n/)
    for (const line of lines) {
      if (!re.test(line)) continue
      const isBan =
        /\b(must not|may not|cannot|can not|never|forbid|forbidden|do not|don't|不得|禁止|不要)\b/i.test(
          line
        )
      if (!isBan) {
        failures.push(`parent authorship permission present: ${label}`)
        break
      }
    }
  }

  const parentNoTaskCode =
    /parent must not implement Task code/i.test(skill) ||
    /父会话不得亲自\s*实现|不得由父会话直接实现/i.test(skill) ||
    /parent.*must not.*(?:implement|write).*(?:Task code|Plan)/i.test(skill)

  if (!parentNoTaskCode) {
    failures.push("missing ban: parent must not implement Task code")
  } else {
    notes.push("parent must not implement Task code")
  }

  const parentNoPlanWrite =
    /parent must not write or rewrite the Plan/i.test(skill) ||
    /Parent must not write or rewrite the Plan/i.test(skill) ||
    /父会话不得(?:直接)?(?:改写|编写|撰写|重写)(?:实施)?计划/i.test(skill)

  const authorOwns =
    /Author owns the Plan/i.test(skill) ||
    /Codex Plan Author owns every Plan/i.test(skill)

  if (!parentNoPlanWrite) {
    failures.push(
      "missing ban: parent must not write/rewrite the Plan (Author owns it)"
    )
  } else {
    notes.push("Plan authorship owned by Author, not parent")
  }

  if (!authorOwns) {
    failures.push("missing statement that Codex Plan Author owns the Plan")
  }

  // Author-owns alone must not mask contradictory parent authoring
  const contradictoryParentAuthoring =
    (/parent\s+(?:writes|authors|rewrites)\s+(?:the\s+)?plan/i.test(skill) ||
      /使用\s*`writing-plans`\s*编写任何实施计划/.test(skill)) &&
    authorOwns
  if (contradictoryParentAuthoring) {
    failures.push(
      "contradictory parent Plan authorship: Author-owns cannot mask parent write permission"
    )
  }

  return { failures, notes }
}

/**
 * Validate full skill markdown. Returns { failures, notes }.
 */
export function validateSkillMarkdown(skill) {
  const failures = []
  const notes = []
  const lines = skill.split(/\r?\n/)

  const fail = (msg) => failures.push(msg)
  const pass = (msg) => notes.push(`OK: ${msg}`)

  // --- Forbidden: reject every literal occurrence (brief raw patterns) ---
  for (const [re, label] of FORBIDDEN) {
    if (re.test(skill)) {
      fail(label)
    } else {
      pass(label.replace(/^forbidden /, "absent "))
    }
  }

  for (const [re, label] of REQUIRED) {
    if (!re.test(skill)) {
      fail(`missing required term: ${label}`)
    } else {
      pass(`has ${label}`)
    }
  }

  const fm = skill.match(/^---\r?\n([\s\S]*?)\r?\n---/)
  if (!fm) {
    fail("missing YAML frontmatter")
  } else {
    const descMatch = fm[1].match(/^description:\s*(.+)$/m)
    if (!descMatch) {
      fail("frontmatter missing description")
    } else {
      const desc = descMatch[1].trim()
      if (
        /\b(Codex Plan Author|b2d_task_risk|normal route|high route|stagnation)\b/i.test(
          desc
        )
      ) {
        fail(
          "frontmatter description must be trigger-only (leaks workflow terms)"
        )
      } else {
        pass("frontmatter description present")
      }
      if (!/^Use when\b/i.test(desc)) {
        fail('frontmatter description should start with "Use when"')
      }
    }
  }

  if (lines.length >= 500) {
    fail(`SKILL.md has ${lines.length} lines (must be < 500)`)
  } else {
    pass(`line count ${lines.length} < 500`)
  }

  const ownership = validateParentOwnership(skill)
  for (const f of ownership.failures) fail(f)
  for (const n of ownership.notes) pass(n)

  const taskRouteSection = extractTaskRouteSection(skill)
  const routeFailures = validateRouteTables(taskRouteSection)
  if (routeFailures.length === 0) {
    pass("normal route table Grok implementer + Codex reviewer")
    pass("high route table Codex implementer + two distinct reviewers")
  } else {
    for (const f of routeFailures) fail(f)
  }

  // High must not allow single-reviewer pass (permissive language in whole skill)
  const highAllowsOneReviewer =
    /high[\s\S]{0,200}?\b(may|can|should|enough to)\b[\s\S]{0,80}?\b(pass|ship|approve)\b[\s\S]{0,80}?\b(one|single)\b[\s\S]{0,40}?reviewer/i.test(
      skill
    ) ||
    /\b(one|single)\b\s+reviewer\s+(is\s+)?enough\b/i.test(skill) ||
    /pass high with (only\s+)?one reviewer/i.test(skill)

  if (highAllowsOneReviewer) {
    fail("high route must not allow passing with a single reviewer")
  }

  const coverage =
    /reviewed_task_id/.test(skill) &&
    /artifact_digest/.test(skill) &&
    /latest/i.test(skill)
  if (!coverage) {
    fail(
      "missing exact latest producer coverage (reviewed_task_id + artifact_digest)"
    )
  } else {
    pass("latest producer coverage terms present")
  }

  const planContracts = [
    [
      /owners of open Critical|open Critical and Important|owners of all open/i,
      "scoped owner re-review",
    ],
    [
      /full[- ]group|complete cohort|complete Plan review group|restore.*complete/i,
      "full-group reset",
    ],
    [/two (consecutive )?non-improving|stagnat/i, "stagnation"],
    [
      /pre-admission|before.*cohort.*admitted/i,
      "pre-admission risk correction path",
    ],
    [
      /post-admission|after any cohort.*admitted|cohort_frozen/i,
      "post-admission freeze",
    ],
    [/consolidated fix|one consolidated/i, "consolidated fix request"],
    [
      /both\s+reviewers[\s\*]*re-review|Both reviewers perform|both\s+prior\s+reviews|both reviewers re-review the latest|全部审核者[\s\S]{0,20}复审/i,
      "both-reviewers-after-fix",
    ],
  ]

  for (const [re, label] of planContracts) {
    if (!re.test(skill)) {
      fail(`missing contract: ${label}`)
    } else {
      pass(label)
    }
  }

  if (!/subagent-driven-development/.test(skill)) {
    fail("must invoke subagent-driven-development by name")
  } else {
    pass("invokes subagent-driven-development")
  }

  return { failures, notes }
}

export { FORBIDDEN, REQUIRED }
