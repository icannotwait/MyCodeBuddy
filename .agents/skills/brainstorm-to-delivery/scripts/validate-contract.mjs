#!/usr/bin/env node
import { closeSync, fstatSync, openSync, readSync, realpathSync } from "node:fs"
import { dirname, join, resolve } from "node:path"
import { TextDecoder } from "node:util"
import { fileURLToPath } from "node:url"
import {
  MAX_DURABLE_EVIDENCE_BYTES,
  MAX_PLAN_DOCUMENT_BYTES,
  MAX_PROGRESS_DOCUMENT_BYTES,
  runValidation,
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

export function canonicalPathsEqual(left, right, platform = process.platform) {
  const normalize =
    platform === "win32" ? (value) => value.toLowerCase() : (value) => value
  return normalize(left) === normalize(right)
}

export function isDirectInvocation(entryPath, moduleUrl) {
  if (!entryPath) return false
  try {
    return canonicalPathsEqual(
      realpathSync.native(resolve(entryPath)),
      realpathSync.native(fileURLToPath(moduleUrl))
    )
  } catch {
    return false
  }
}

const VALUE_FLAGS = new Set([
  "--plan",
  "--progress",
  "--plan-rel-path",
  "--durable-evidence",
])
const BOOLEAN_FLAGS = new Set([
  "--derive-plan-routing",
  "--output-json",
  "--document-admission",
  "--admission",
])

export function parseArguments(args) {
  const options = {
    plan: null,
    progress: null,
    planRelPath: null,
    durableEvidence: null,
    derivePlanRouting: false,
    outputJson: false,
    documentAdmission: false,
    admission: false,
  }
  const seen = new Set()
  for (let index = 0; index < args.length; index += 1) {
    const flag = args[index]
    if (!VALUE_FLAGS.has(flag) && !BOOLEAN_FLAGS.has(flag)) {
      throw new Error(`unknown flag: ${flag}`)
    }
    if (seen.has(flag)) throw new Error(`duplicate flag: ${flag}`)
    seen.add(flag)
    if (BOOLEAN_FLAGS.has(flag)) {
      if (flag === "--derive-plan-routing") options.derivePlanRouting = true
      if (flag === "--output-json") options.outputJson = true
      if (flag === "--document-admission") options.documentAdmission = true
      if (flag === "--admission") options.admission = true
      continue
    }
    const value = args[index + 1]
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`${flag} requires a value`)
    }
    index += 1
    if (flag === "--plan") options.plan = value
    if (flag === "--progress") options.progress = value
    if (flag === "--plan-rel-path") options.planRelPath = value
    if (flag === "--durable-evidence") options.durableEvidence = value
  }

  const hasFlags = seen.size > 0
  if (!hasFlags) return { ...options, mode: "skill" }
  if (options.documentAdmission) {
    if (
      options.plan !== null ||
      options.progress !== null ||
      options.planRelPath !== null ||
      options.derivePlanRouting ||
      options.admission
    ) {
      throw new Error(
        "--document-admission cannot be combined with Plan, progress, or derive flags"
      )
    }
    if (options.durableEvidence === null || !options.outputJson) {
      throw new Error(
        "--document-admission requires --durable-evidence and --output-json"
      )
    }
    return { ...options, mode: "document" }
  }
  if (options.plan === null || options.planRelPath === null) {
    throw new Error("--plan and --plan-rel-path must be provided together")
  }
  if (options.derivePlanRouting) {
    if (options.progress !== null || options.durableEvidence !== null) {
      throw new Error(
        "--derive-plan-routing cannot be combined with --progress or --durable-evidence"
      )
    }
    if (options.admission) {
      throw new Error(
        "--derive-plan-routing cannot be combined with --admission"
      )
    }
    if (!options.outputJson) {
      throw new Error("--derive-plan-routing requires --output-json")
    }
    return { ...options, mode: "plan" }
  }
  if (options.admission) {
    if (options.progress === null || options.durableEvidence === null) {
      throw new Error("--admission requires --progress and --durable-evidence")
    }
    if (!options.outputJson) {
      throw new Error("--admission requires --output-json")
    }
    return { ...options, mode: "admission" }
  }
  if (options.durableEvidence !== null) {
    throw new Error(
      "--durable-evidence requires --admission or --document-admission"
    )
  }
  if (options.progress === null) {
    throw new Error("static Plan validation requires --progress")
  }
  return { ...options, mode: "static" }
}

function printReadableResult(result) {
  if (result.failures.length > 0) {
    console.error("FAIL: brainstorm-to-delivery Simple contract")
    for (const failure of result.failures) {
      console.error(`  - [${failure.rule_id}] ${failure.message}`)
    }
    console.error(`\n${result.failures.length} failure(s)`)
    return 1
  }
  console.log("PASS: brainstorm-to-delivery Simple contract")
  console.log(
    `\n0 failures, ${result.task_bindings.length} Task binding(s) derived`
  )
  return 0
}

function printSkillResult(skillResult) {
  if (skillResult.failures.length > 0) {
    console.error("FAIL: brainstorm-to-delivery Simple contract")
    for (const failure of skillResult.failures) console.error(`  - ${failure}`)
    console.error(
      `\n${skillResult.failures.length} failure(s), ` +
        `${skillResult.notes.length} check(s) completed`
    )
    return 1
  }
  console.log("PASS: brainstorm-to-delivery Simple contract")
  for (const note of skillResult.notes) console.log(`  ${note}`)
  console.log(`\n0 failures, ${skillResult.notes.length} checks completed`)
  return 0
}

function cliFailureEnvelope(message, ruleId = "B2D-CLI-002") {
  return {
    schema_version: 1,
    admission_authorized: false,
    durable_snapshot: null,
    task_bindings: [],
    reconciliation_actions: [],
    failures: [{ rule_id: ruleId, message }],
  }
}

function neutralizeInheritedColorConflict() {
  // Node emits a process warning on first console use when FORCE_COLOR and
  // NO_COLOR are both set. Drop both so CLI stderr stays contract-empty.
  delete process.env.FORCE_COLOR
  delete process.env.NO_COLOR
}

function run(args) {
  neutralizeInheritedColorConflict()
  let options
  try {
    options = parseArguments(args)
    const skillMarkdown = readUtf8FileBounded(
      skillPath,
      MAX_SKILL_DOCUMENT_BYTES,
      "SKILL.md"
    )
    if (options.mode === "skill") {
      return printSkillResult(validateSkillMarkdown(skillMarkdown))
    }
    const durableEvidence =
      options.durableEvidence === null
        ? null
        : readUtf8FileBounded(
            options.durableEvidence,
            MAX_DURABLE_EVIDENCE_BYTES,
            "durable evidence"
          )
    const result = runValidation({
      skillMarkdown,
      planMarkdown:
        options.plan === null
          ? null
          : readUtf8FileBounded(options.plan, MAX_PLAN_DOCUMENT_BYTES, "Plan"),
      progressMarkdown:
        options.progress === null
          ? null
          : readUtf8FileBounded(
              options.progress,
              MAX_PROGRESS_DOCUMENT_BYTES,
              "progress document"
            ),
      planRelPath: options.planRelPath,
      derivePlanRouting: options.derivePlanRouting,
      outputJson: options.outputJson,
      documentAdmission: options.documentAdmission,
      admission: options.admission,
      durableEvidence,
    })
    if (options.outputJson) console.log(JSON.stringify(result, null, 2))
    else return printReadableResult(result)
    return result.failures.length === 0 ? 0 : 1
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error)
    if (args.includes("--output-json")) {
      console.log(JSON.stringify(cliFailureEnvelope(message), null, 2))
      return 1
    }
    console.error(`FAIL: ${message}`)
    return 1
  }
}

if (isDirectInvocation(process.argv[1], import.meta.url)) {
  process.exitCode = run(process.argv.slice(2))
}
