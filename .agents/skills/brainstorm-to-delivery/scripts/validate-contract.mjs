#!/usr/bin/env node
/**
 * Deterministic contract checks for brainstorm-to-delivery SKILL.md (v2 adaptive routing).
 * Structural/machine-enforceable rules live here; judgment rules stay in the Skill body.
 *
 * Exit 0 on PASS, 1 on FAIL.
 */
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const __dirname = dirname(fileURLToPath(import.meta.url))
const skillPath = join(__dirname, "..", "SKILL.md")
const skill = readFileSync(skillPath, "utf8")
const lines = skill.split(/\r?\n/)
const body = skill.replace(/^---[\s\S]*?---\s*/, "")
const bodyLines = body.split(/\r?\n/)

const failures = []
const notes = []

function fail(msg) {
  failures.push(msg)
}

function pass(msg) {
  notes.push(`OK: ${msg}`)
}

// --- Forbidden v1 / legacy vocabulary (as active contracts, not ban lists) ---
// Match endorsing/using the term; allow explicit "do not use X" / "no X" bans.
function endorsesForbidden(re) {
  const lines = skill.split(/\r?\n/)
  for (const line of lines) {
    if (!re.test(line)) continue
    if (
      /\b(no|not|never|forbid|forbidden|ban|without|avoid|reject|do not|must not|不得|禁止|没有|无)\b/i.test(
        line
      )
    ) {
      continue
    }
    return true
  }
  return false
}

const forbidden = [
  [/workflow_manifest_v1/, "forbidden workflow_manifest_v1"],
  [/schema_version\s*[=:]\s*1/, "forbidden schema_version = 1"],
  [/pair_frozen/, "forbidden pair_frozen (use cohort_frozen)"],
  [/mode\s*=\s*legacy/i, "forbidden mode=legacy"],
]

for (const [re, label] of forbidden) {
  if (endorsesForbidden(re)) {
    fail(label)
  } else if (re.test(skill)) {
    pass(`${label.replace(/^forbidden /, "")} only in ban context`)
  } else {
    pass(label.replace(/^forbidden /, "absent "))
  }
}

// --- Required v2 terms ---
const required = [
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

for (const [re, label] of required) {
  if (!re.test(skill)) {
    fail(`missing required term: ${label}`)
  } else {
    pass(`has ${label}`)
  }
}

// --- Frontmatter description must stay trigger-only (no workflow summary) ---
const fm = skill.match(/^---\r?\n([\s\S]*?)\r?\n---/)
if (!fm) {
  fail("missing YAML frontmatter")
} else {
  const descMatch = fm[1].match(/^description:\s*(.+)$/m)
  if (!descMatch) {
    fail("frontmatter missing description")
  } else {
    const desc = descMatch[1].trim()
    const workflowLeak =
      /author|risk matrix|cohort|holistic|implementer|reviewer fan|writing-plans then|dispatch/i.test(
        desc
      ) && /plan|task|route|review/i.test(desc)
    // Allow "Use when..." triggers; reject process summaries of adaptive routing.
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
    // Soft check: description should start with Use when
    if (!/^Use when\b/i.test(desc)) {
      fail('frontmatter description should start with "Use when"')
    }
    void workflowLeak
  }
}

// --- Line budget ---
if (lines.length >= 500) {
  fail(`SKILL.md has ${lines.length} lines (must be < 500)`)
} else {
  pass(`line count ${lines.length} < 500`)
}

// --- Parent must not write Plan or Task code ---
const parentWritesPlan =
  /父会话.*(?:写|编写|撰写).*(?:计划|Plan)/i.test(skill) ||
  /parent\s+(?:writes|authors|rewrites)\s+(?:the\s+)?plan/i.test(skill) ||
  /使用\s*`writing-plans`\s*编写任何实施计划/.test(skill)
// The old skill tells the parent to invoke writing-plans itself; v2 must forbid that.
const parentInvokesWritingPlans =
  /父会话.*`writing-plans`|parent.*invoke.*writing-plans/i.test(skill) &&
  !/禁止父会话.*writing-plans|parent must not.*writing-plans|不得由父会话.*writing-plans|Plan Author.*writing-plans/i.test(
    skill
  )

const authorOwns =
  /Codex Plan Author/.test(skill) &&
  /(?:Author|作者).*(?:owns|拥有|owns the Plan|撰写并修订)|(?:计划|Plan).*(?:仅|only).*(?:Author|作者)/i.test(
    skill
  )

if (parentWritesPlan && !authorOwns) {
  fail(
    "parent appears allowed to write the Plan; Codex Plan Author must own Plan authorship"
  )
}

// Explicit parent-implementation bans should exist for Task code
const parentNoTaskCode =
  /父会话不得亲自\s*实现|parent must not.*implement|不得由父会话直接实现|parent.*不得.*实现或修复|parent never writes the Plan or Task code|never writes the Plan or Task code|Parent only orchestrates|parent.*must not.*Task code|Forbidden\.\s*Author owns Plan;\s*routed implementers own code/i.test(
    skill
  )
if (!parentNoTaskCode) {
  fail("missing ban: parent must not implement Task code")
} else {
  pass("parent must not implement Task code")
}

// Plan authorship: require explicit ban on parent rewriting the Plan
const parentNoPlanWrite =
  /parent\s+must\s+not\s+(?:directly\s+)?(?:rewrite|write|author)\s+the\s+Plan/i.test(
    skill
  ) ||
  /父会话不得(?:直接)?(?:改写|编写|撰写|重写)(?:实施)?计划/i.test(skill) ||
  /不得由父会话.*(?:编写|撰写|改写).*Plan/i.test(skill) ||
  /Author owns the Plan|Author.*owns.*Plan file|Plan Author.*owns/i.test(skill)
if (!parentNoPlanWrite) {
  fail("missing ban: parent must not write/rewrite the Plan (Author owns it)")
} else {
  pass("Plan authorship owned by Author, not parent")
}

// --- Route tables: normal uses Grok implementer + Codex reviewer ---
function extractTableBlock(headingPattern) {
  const idx = skill.search(headingPattern)
  if (idx < 0) return null
  const slice = skill.slice(idx, idx + 1200)
  return slice
}

const normalBlock =
  extractTableBlock(/###?\s*Normal route/i) ||
  extractTableBlock(/normal[:\s].*Grok|Grok implementer.*Codex reviewer/i)
const highBlock =
  extractTableBlock(/###?\s*High(?:-risk)? route/i) ||
  extractTableBlock(/high[:\s].*Codex implementer/i)

// Prefer explicit Task route section content
const taskRouteSection = (() => {
  const m = skill.match(
    /##\s*Task route[\s\S]*?(?=##\s|$)/i
  )
  return m ? m[0] : skill
})()

const normalOk =
  /normal/i.test(taskRouteSection) &&
  /Grok/i.test(taskRouteSection) &&
  /Codex/i.test(taskRouteSection) &&
  (
    /normal[^\n]*Grok[^\n]*Codex/i.test(taskRouteSection) ||
    /normal[\s\S]{0,400}?Implementer[\s\S]{0,80}?Grok/i.test(taskRouteSection) ||
    /normal:.*Grok implementer.*Codex reviewer/i.test(taskRouteSection)
  )

if (!normalOk) {
  fail(
    "normal route must use Grok implementer/fixer and independent Codex reviewer"
  )
} else {
  pass("normal route Grok + Codex")
}

// High must require two reviewers (Codex AND Grok), not pass with one
const highRequiresBoth =
  /high[^\n]*Codex[^\n]*Grok|high[^\n]*Codex AND Grok|high[\s\S]{0,500}?both reviewers|strict AND|两个审核者|Codex\s+AND\s+Grok/i.test(
    taskRouteSection
  ) ||
  (/high/i.test(taskRouteSection) &&
    /independent Codex/i.test(taskRouteSection) &&
    /Grok reviewer/i.test(taskRouteSection) &&
    /both|AND|两个|全部/i.test(taskRouteSection))

// Fail if high is allowed to pass with one reviewer (permissive language only)
const highAllowsOneReviewer =
  /high[\s\S]{0,200}?\b(may|can|should|enough to)\b[\s\S]{0,80}?\b(pass|ship|approve)\b[\s\S]{0,80}?\b(one|single)\b[\s\S]{0,40}?reviewer/i.test(
    skill
  ) ||
  /\b(one|single)\b\s+reviewer\s+(is\s+)?enough\b/i.test(skill) ||
  /pass high with (only\s+)?one reviewer/i.test(skill) ||
  /high[\s\S]{0,120}?downgrade to normal to pass/i.test(skill)

if (highAllowsOneReviewer) {
  fail("high route must not allow passing with a single reviewer")
}

if (!highRequiresBoth) {
  fail(
    "high route must require independent Codex AND Grok reviewers (dual review)"
  )
} else {
  pass("high route dual reviewers")
}

// High implementer must be Codex
const highImplCodex =
  /high[^\n]*Codex implementer|high[\s\S]{0,300}?Implementer[\s\S]{0,80}?Codex|high:.*Codex implementer/i.test(
    taskRouteSection
  ) || /high[\s\S]{0,400}?Codex implementer\/fixer/i.test(taskRouteSection)

if (!highImplCodex) {
  fail("high route must use Codex implementer/fixer")
} else {
  pass("high implementer Codex")
}

// Coverage of latest producer
const coverage =
  /reviewed_task_id/.test(skill) &&
  /artifact_digest/.test(skill) &&
  /latest/i.test(skill)
if (!coverage) {
  fail("missing exact latest producer coverage (reviewed_task_id + artifact_digest)")
} else {
  pass("latest producer coverage terms present")
}

// Scoped owner re-review + full-group reset + stagnation + pre/post admission
const planContracts = [
  [/owners of open Critical|open Critical and Important|owners of all open/i, "scoped owner re-review"],
  [/full[- ]group|complete cohort|complete Plan review group|restore.*complete/i, "full-group reset"],
  [/two (consecutive )?non-improving|stagnat/i, "stagnation"],
  [/pre-admission|before.*cohort.*admitted/i, "pre-admission risk correction path"],
  [/post-admission|after any cohort.*admitted|cohort_frozen/i, "post-admission freeze"],
  [/consolidated fix|one consolidated/i, "consolidated fix request"],
  [/both\s+reviewers[\s\*]*re-review|Both reviewers perform|both\s+prior\s+reviews|both reviewers re-review the latest|全部审核者[\s\S]{0,20}复审/i, "both-reviewers-after-fix"],
]

for (const [re, label] of planContracts) {
  if (!re.test(skill)) {
    fail(`missing contract: ${label}`)
  } else {
    pass(label)
  }
}

// Invoke generic skills by name (must mention subagent-driven-development)
if (!/subagent-driven-development/.test(skill)) {
  fail("must invoke subagent-driven-development by name")
} else {
  pass("invokes subagent-driven-development")
}

// Output
if (failures.length) {
  console.error("FAIL: brainstorm-to-delivery skill contract")
  for (const f of failures) {
    console.error(`  - ${f}`)
  }
  if (notes.length) {
    console.error("\nPartial matches:")
    for (const n of notes) console.error(`  ${n}`)
  }
  console.error(`\n${failures.length} failure(s), ${notes.length} check(s) passed`)
  process.exit(1)
}

console.log("PASS: brainstorm-to-delivery skill contract")
for (const n of notes) console.log(`  ${n}`)
console.log(`\n0 failures, ${notes.length} checks passed`)
process.exit(0)
