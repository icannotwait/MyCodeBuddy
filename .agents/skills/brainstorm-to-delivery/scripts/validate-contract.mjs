#!/usr/bin/env node
import { closeSync, fstatSync, openSync, readSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { TextDecoder } from "node:util"
import { fileURLToPath } from "node:url"
import {
  MAX_PLAN_DOCUMENT_BYTES,
  MAX_PROGRESS_DOCUMENT_BYTES,
  validateSimpleDocuments,
  validateSkillMarkdown,
} from "./validate-contract.lib.mjs"

const __dirname = dirname(fileURLToPath(import.meta.url))
const skillPath = join(__dirname, "..", "SKILL.md")
const MAX_SKILL_DOCUMENT_BYTES = 512 * 1024
const READ_CHUNK_BYTES = 64 * 1024

export function readUtf8FileBounded(path, maxBytes, label) {
  const handle = openSync(path, "r")
  try {
    if (fstatSync(handle).size > maxBytes) {
      throw new Error(`${label} exceeds ${maxBytes} bytes`)
    }
    const chunks = []
    let total = 0
    while (total <= maxBytes) {
      const capacity = Math.min(READ_CHUNK_BYTES, maxBytes + 1 - total)
      const chunk = Buffer.allocUnsafe(capacity)
      const bytesRead = readSync(handle, chunk, 0, capacity, null)
      if (bytesRead === 0) break
      total += bytesRead
      if (total > maxBytes) {
        throw new Error(`${label} exceeds ${maxBytes} bytes`)
      }
      chunks.push(chunk.subarray(0, bytesRead))
    }

    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(
        Buffer.concat(chunks, total)
      )
    } catch (error) {
      if (error instanceof TypeError) {
        throw new Error(`${label} is not valid UTF-8`)
      }
      throw error
    }
  } finally {
    closeSync(handle)
  }
}

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

function run(args) {
  let result
  try {
    const options = parseArguments(args)
    const skillMarkdown = readUtf8FileBounded(
      skillPath,
      MAX_SKILL_DOCUMENT_BYTES,
      "SKILL.md"
    )
    result =
      options.size === 0
        ? validateSkillMarkdown(skillMarkdown)
        : validateSimpleDocuments({
            skillMarkdown,
            planMarkdown: readUtf8FileBounded(
              options.get("--plan"),
              MAX_PLAN_DOCUMENT_BYTES,
              "Plan"
            ),
            progressMarkdown: readUtf8FileBounded(
              options.get("--progress"),
              MAX_PROGRESS_DOCUMENT_BYTES,
              "progress document"
            ),
            planRelPath: options.get("--plan-rel-path"),
          })
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    console.error(`FAIL: ${message}`)
    return 1
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
    return 1
  }

  console.log("PASS: brainstorm-to-delivery Simple contract")
  for (const note of result.notes) console.log(`  ${note}`)
  console.log(`\n0 failures, ${result.notes.length} checks completed`)
  return 0
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  process.exitCode = run(process.argv.slice(2))
}
