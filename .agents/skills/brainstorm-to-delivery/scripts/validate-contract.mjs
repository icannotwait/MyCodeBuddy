#!/usr/bin/env node
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import {
  validateSimpleDocuments,
  validateSkillMarkdown,
} from "./validate-contract.lib.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const skillPath = join(__dirname, "..", "SKILL.md")

function parseArguments(args) {
  const values = new Map()
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index]
    const value = args[index + 1]
    if (
      !["--plan", "--progress", "--plan-rel-path"].includes(flag) ||
      value === undefined
    ) {
      throw new Error(
        "usage: validate-contract.mjs " +
          "[--plan FILE --progress FILE --plan-rel-path REL_PATH]"
      )
    }
    values.set(flag, value)
  }
  if (values.size !== 0 && values.size !== 3) {
    throw new Error(
      "--plan, --progress, and --plan-rel-path must be provided together"
    )
  }
  return values
}

let result
try {
  const options = parseArguments(process.argv.slice(2))
  const skillMarkdown = readFileSync(skillPath, "utf8")
  result =
    options.size === 0
      ? validateSkillMarkdown(skillMarkdown)
      : validateSimpleDocuments({
          skillMarkdown,
          planMarkdown: readFileSync(options.get("--plan"), "utf8"),
          progressMarkdown: readFileSync(options.get("--progress"), "utf8"),
          planRelPath: options.get("--plan-rel-path"),
        })
} catch (error) {
  const message = error instanceof Error ? error.message : String(error)
  console.error(`FAIL: ${message}`)
  process.exit(1)
}

if (result.failures.length > 0) {
  console.error("FAIL: brainstorm-to-delivery Simple contract")
  for (const failure of result.failures) console.error(`  - ${failure}`)
  if (result.notes.length > 0) {
    console.error("\nPartial checks:")
    for (const note of result.notes) console.error(`  ${note}`)
  }
  console.error(
    `\n${result.failures.length} failure(s), ` +
      `${result.notes.length} check(s) completed`
  )
  process.exit(1)
}

console.log("PASS: brainstorm-to-delivery Simple contract")
for (const note of result.notes) console.log(`  ${note}`)
console.log(`\n0 failures, ${result.notes.length} checks completed`)
