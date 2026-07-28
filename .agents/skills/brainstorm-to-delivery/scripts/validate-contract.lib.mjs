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
  return cells[1] || ""
}

/**
 * Strip harmless Markdown annotations/punctuation, then require exactly one
 * canonical agent identity (`grok` | `codex`). No substring membership.
 * @returns {{ ok: true, agent: "grok"|"codex" } | { ok: false, reason: string }}
 */
export function parseExactAgentIdentity(raw) {
  let s = String(raw ?? "")
  // Drop parenthetical annotations: (≠ implementer, ≠ Author), (independent child)
  s = s.replace(/\([^)]*\)/g, " ")
  // Drop Markdown emphasis
  s = s.replace(/[*_`~]+/g, " ")
  // Normalize separators that signal alternatives / lists
  s = s.replace(/[/|,;&]+/g, " ")
  s = s.replace(/\b(?:or|and|\/)\b/gi, " ")
  s = s.replace(/\s+/g, " ").trim().toLowerCase()

  if (!s) {
    return { ok: false, reason: "empty agent cell" }
  }

  // Tokenize on whitespace; only exact tokens "grok" and "codex" count.
  const tokens = s.split(" ").filter(Boolean)
  const agents = []
  const unknown = []
  for (const t of tokens) {
    // Allow benign role words that sometimes appear after stripping notes
    if (
      /^(?:agent|type|independent|child|implementer|fixer|reviewer|author|profile|none)$/.test(
        t
      )
    ) {
      continue
    }
    if (t === "grok" || t === "codex") {
      agents.push(t)
      continue
    }
    unknown.push(t)
  }

  if (unknown.length) {
    return {
      ok: false,
      reason: `non-canonical agent token(s): ${unknown.join(" ")}`,
    }
  }
  const unique = [...new Set(agents)]
  if (unique.length === 0) {
    return { ok: false, reason: "no canonical agent identity" }
  }
  if (unique.length > 1) {
    return {
      ok: false,
      reason: `mixed agent identities: ${unique.join(" / ")}`,
    }
  }
  return { ok: true, agent: unique[0] }
}

function classifyRole(roleRaw) {
  const role = String(roleRaw || "")
    .toLowerCase()
    .replace(/[*_`~]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
  if (!role || role === "role") return null
  if (/implementer/.test(role)) return "implementer"
  if (/reviewer/.test(role)) return "reviewer"
  return "other"
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
    const rows = tables.get(normalKey).filter((r) => classifyRole(roleCell(r.cells)))
    const impls = []
    const revs = []
    const extras = []
    for (const r of rows) {
      const kind = classifyRole(roleCell(r.cells))
      const parsed = parseExactAgentIdentity(agentCell(r.cells))
      if (kind === "implementer") {
        if (!parsed.ok) {
          failures.push(
            `normal route implementer cell is not exact: ${parsed.reason}`
          )
        } else {
          impls.push(parsed.agent)
        }
      } else if (kind === "reviewer") {
        if (!parsed.ok) {
          failures.push(
            `normal route reviewer cell is not exact: ${parsed.reason}`
          )
        } else {
          revs.push(parsed.agent)
        }
      } else {
        extras.push(roleCell(r.cells))
      }
    }

    if (impls.length !== 1 || revs.length !== 1 || extras.length > 0) {
      failures.push(
        `normal route must have exactly one implementer row and one reviewer row (got implementer=${impls.length}, reviewer=${revs.length}, extra=${extras.length})`
      )
    }
    if (impls.length === 1 && impls[0] !== "grok") {
      failures.push(
        "normal route table must map Implementer/fixer exactly to Grok"
      )
    }
    if (revs.length === 1 && revs[0] !== "codex") {
      failures.push(
        "normal route table must map Independent reviewer exactly to Codex"
      )
    }
    // Keep legacy-style message for pure wrong-agent single-token failures
    if (impls.length === 1 && impls[0] === "codex") {
      failures.push("normal route implementer must be Grok, not Codex")
    }
  }

  if (!highKey) {
    failures.push("Task route missing `### High route` table")
  } else {
    const rows = tables.get(highKey).filter((r) => classifyRole(roleCell(r.cells)))
    const impls = []
    const revs = []
    const extras = []
    for (const r of rows) {
      const kind = classifyRole(roleCell(r.cells))
      const parsed = parseExactAgentIdentity(agentCell(r.cells))
      if (kind === "implementer") {
        if (!parsed.ok) {
          failures.push(
            `high route implementer cell is not exact: ${parsed.reason}`
          )
        } else {
          impls.push(parsed.agent)
        }
      } else if (kind === "reviewer") {
        if (!parsed.ok) {
          failures.push(
            `high route reviewer cell is not exact: ${parsed.reason}`
          )
        } else {
          revs.push(parsed.agent)
        }
      } else {
        extras.push(roleCell(r.cells))
      }
    }

    if (impls.length !== 1) {
      failures.push(
        `high route must have exactly one implementer row (got ${impls.length})`
      )
    } else if (impls[0] !== "codex") {
      failures.push(
        "high route table must map Implementer/fixer exactly to Codex"
      )
    }

    if (revs.length !== 2) {
      failures.push(
        `high route table must list exactly two distinct Independent reviewer rows (got ${revs.length})`
      )
    } else {
      const set = new Set(revs)
      if (!(set.has("codex") && set.has("grok"))) {
        failures.push(
          "high route table must assign exactly independent Codex AND Grok reviewers"
        )
      }
      if (revs[0] === revs[1]) {
        failures.push(
          "high route must use two distinct reviewer agents (not the same agent twice)"
        )
      }
    }

    if (extras.length > 0) {
      failures.push(
        `high route has unexpected extra role row(s): ${extras.join(", ")}`
      )
    }
  }

  return failures
}

/**
 * True when a line is a legitimate prohibition (not a permission grant).
 * Same-line negation only — do not treat unrelated Author-owns text as a ban.
 */
function lineIsProhibition(line) {
  return /\b(must not|may not|cannot|can not|never|forbid|forbidden|do not|don't|does not|doesn't|不得|禁止|不要|不得亲自)\b/i.test(
    line
  )
}

/**
 * Detect direct Plan/Task authoring or implementation permission statements.
 * Independent of modal verbs. Urgency clauses do not exempt.
 */
export function findParentPermissionViolations(skill) {
  const found = []
  const lines = skill.split(/\r?\n/)

  // Direct affirmative permission / action patterns (no modal required).
  const direct = [
    [
      /\bparent\s+writes\s+(?:the\s+)?plan\b/i,
      "Parent writes Plan",
    ],
    [
      /\bparent\s+writes\s+task\s+code\b/i,
      "Parent writes Task code",
    ],
    [
      /\bparent\s+implements\s+tasks?\b/i,
      "Parent implements Task",
    ],
    [
      /\bparent\s+(?:authors|rewrites|edits)\s+(?:the\s+)?plan\b/i,
      "Parent authors/rewrites/edits Plan",
    ],
    [
      /\bparent\s+(?:may|can|should|must)\s+(?:write|author|rewrite|implement|edit)\s+(?:the\s+)?(?:plan|task)\b/i,
      "parent modal permission to write/implement Plan or Task",
    ],
    [
      /使用\s*`writing-plans`\s*编写任何实施计划/,
      "parent instructed to invoke writing-plans itself",
    ],
    [
      /\bparent\b[^\n]{0,40}\binvoke(?:s|ing)?\s+`?writing-plans`?/i,
      "parent invokes writing-plans (Author must)",
    ],
    [
      /父会话(?:可以|可|应当|应|必须).*(?:编写|撰写|改写|实现|修复).*(?:计划|Plan|Task|代码)/i,
      "parent Chinese permission to author Plan/Task",
    ],
    [
      /父会话.*`writing-plans`/,
      "parent Chinese writing-plans ownership",
    ],
  ]

  for (const line of lines) {
    if (!line.trim()) continue
    if (lineIsProhibition(line)) continue
    for (const [re, label] of direct) {
      if (re.test(line)) {
        found.push(`parent authorship permission present: ${label}`)
      }
    }
  }

  return [...new Set(found)]
}

/**
 * Reject any parent Plan/Task authorship permission; require explicit bans.
 * @returns {{ failures: string[], notes: string[] }}
 */
export function validateParentOwnership(skill) {
  const failures = []
  const notes = []

  for (const msg of findParentPermissionViolations(skill)) {
    failures.push(msg)
  }

  // Author-owns alone must never mask a permission violation already collected.
  const authorOwns =
    /Author owns the Plan/i.test(skill) ||
    /Codex Plan Author owns every Plan/i.test(skill)

  if (
    authorOwns &&
    failures.some((f) => /parent authorship permission present/i.test(f))
  ) {
    failures.push(
      "contradictory parent Plan/Task authorship: Author-owns cannot mask parent write/implement permission"
    )
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
