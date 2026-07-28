#!/usr/bin/env node
/**
 * Deterministic contract checks for brainstorm-to-delivery SKILL.md (v2 adaptive routing).
 * Exit 0 on PASS, 1 on FAIL.
 */
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import { validateSkillMarkdown } from "./validate-contract.lib.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const skillPath = join(__dirname, "..", "SKILL.md")
const skill = readFileSync(skillPath, "utf8")
const { failures, notes } = validateSkillMarkdown(skill)

if (failures.length) {
  console.error("FAIL: brainstorm-to-delivery skill contract")
  for (const f of failures) {
    console.error(`  - ${f}`)
  }
  if (notes.length) {
    console.error("\nPartial matches:")
    for (const n of notes) console.error(`  ${n}`)
  }
  console.error(
    `\n${failures.length} failure(s), ${notes.length} check(s) passed`
  )
  process.exit(1)
}

console.log("PASS: brainstorm-to-delivery skill contract")
for (const n of notes) console.log(`  ${n}`)
console.log(`\n0 failures, ${notes.length} checks passed`)
process.exit(0)
