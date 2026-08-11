/**
 * Deterministic contract checks for the Simple brainstorm-to-delivery Skill
 * and its Plan/progress documents.
 */

export const MAX_PLAN_DOCUMENT_BYTES = 2 * 1024 * 1024
export const MAX_PROGRESS_DOCUMENT_BYTES = 512 * 1024
export const MAX_PROGRESS_BLOCK_BYTES = 64 * 1024

const PROGRESS_MARKER = "<!-- codeg-simple-progress-v1"
const COMMENT_END = "-->"
const TASK_STATUSES = new Set([
  "pending",
  "in_progress",
  "completed",
  "blocked",
])
const RUN_STATES = new Set([
  "reserving",
  "running",
  "completed",
  "failed",
  "canceled",
  "cancelled",
  "stalled",
  "unknown",
])
const REPLACEMENT_REASONS = new Set([
  "unresumable",
  "budget_exhausted_continue",
  "not_supported",
  "admission_failed",
  "admission_unknown",
])
const BUILTIN_AGENT_TYPES = new Set([
  "claude_code",
  "codex",
  "open_code",
  "gemini",
  "cline",
  "hermes",
  "code_buddy",
  "kimi_code",
  "pi",
  "grok",
  "cursor",
])
const RESERVED_CUSTOM_AGENT_IDS = new Set([
  ...BUILTIN_AGENT_TYPES,
  "claude-acp",
  "codex-acp",
  "opencode",
  "codebuddy-code",
  "kimi-code",
  "pi-acp",
  "grok-build",
  "kimi",
])
const V2_SKILL_IDENTIFIERS = [
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
]
const FORBIDDEN_PROGRESS_FIELDS = new Set([
  "workflow_id",
  "workflow_kind",
  "workflow_state",
  "publication_token",
  "manifest_revision",
  "expected_manifest_revision",
  "graph_revision",
  "expected_graph_revision",
  "gate",
  "gates",
  "gate_id",
  "node_id",
  "nodes",
  "artifact_digest",
  "reviewed_task_id",
  "completion",
  "completion_card",
  "card",
  "cards",
  "recovery_authorization_id",
  "risk_policy_version",
  "task_policies",
  "reviewer_cohort_node_ids",
])

function byteLength(value) {
  return Buffer.byteLength(String(value ?? ""), "utf8")
}

function fail(failures, ruleId, message) {
  failures.push(`[${ruleId}] ${message}`)
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
}

function nonEmptyString(value) {
  return typeof value === "string" && value.trim().length > 0
}

function optionalString(value) {
  return value === undefined || value === null || typeof value === "string"
}

function positiveInteger(value) {
  return Number.isInteger(value) && value > 0
}

function hasControl(value) {
  return /\p{Cc}/u.test(value)
}

function normalizeRelPath(value) {
  if (typeof value !== "string" || value.length === 0) return null
  if (value.includes("|") || hasControl(value)) return null

  const nfc = value.normalize("NFC")
  if (
    nfc.startsWith("/") ||
    nfc.startsWith("\\\\") ||
    /^[A-Za-z]:/.test(nfc)
  ) {
    return null
  }

  let normalized = nfc.replace(/[\\/]+/g, "/")
  while (normalized.startsWith("./")) normalized = normalized.slice(2)
  if (normalized.endsWith("/") && normalized.length > 1) {
    normalized = normalized.slice(0, -1)
  }
  if (
    normalized.length === 0 ||
    normalized === "." ||
    normalized.startsWith("/")
  ) {
    return null
  }
  if (
    normalized
      .split("/")
      .some((component) => ["", ".", ".."].includes(component))
  ) {
    return null
  }
  return process.platform === "win32" ? normalized.toLowerCase() : normalized
}

function validAgentType(value) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.includes("|") ||
    hasControl(value)
  ) {
    return false
  }
  if (BUILTIN_AGENT_TYPES.has(value)) return true
  if (!value.startsWith("custom:")) return false
  const id = value.slice("custom:".length)
  return (
    Buffer.byteLength(id, "utf8") <= 64 &&
    /^[a-z0-9_-][a-z0-9._-]*$/.test(id) &&
    !RESERVED_CUSTOM_AGENT_IDS.has(id)
  )
}

function parseProfileToken(value) {
  if (!value || value.includes("|") || hasControl(value)) return undefined
  return value === "none" ? null : value
}

function parseRecognizedWorkUnitKey(value) {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    [...value].length > 200 ||
    hasControl(value)
  ) {
    return null
  }

  const parts = value.split("|")
  if (parts[0] === "task" && parts.length === 5) {
    const [, indexToken, role, agentType, profileToken] = parts
    if (
      !/^[1-9][0-9]*$/.test(indexToken) ||
      !["implementer", "reviewer"].includes(role) ||
      !validAgentType(agentType)
    ) {
      return null
    }
    const taskIndex = Number(indexToken)
    const profileId = parseProfileToken(profileToken)
    if (
      !Number.isInteger(taskIndex) ||
      taskIndex > 0xffffffff ||
      profileId === undefined
    ) {
      return null
    }
    return { kind: "task", taskIndex, role, agentType, profileId }
  }

  if (["design", "plan"].includes(parts[0]) && parts.length === 5) {
    const [kind, path, role, agentType, profileToken] = parts
    const normalizedPath = normalizeRelPath(path)
    const allowedRole =
      (kind === "design" && role === "reviewer") ||
      (kind === "plan" && ["author", "reviewer"].includes(role))
    const profileId = parseProfileToken(profileToken)
    if (
      normalizedPath !== path ||
      !allowedRole ||
      !validAgentType(agentType) ||
      profileId === undefined
    ) {
      return null
    }
    return { kind, path, role, agentType, profileId }
  }

  if (parts[0] === "final_review" && parts.length === 4) {
    const [, role, agentType, profileToken] = parts
    const profileId = parseProfileToken(profileToken)
    if (
      !["reviewer", "fixer"].includes(role) ||
      !validAgentType(agentType) ||
      profileId === undefined
    ) {
      return null
    }
    return { kind: "final_review", role, agentType, profileId }
  }
  return null
}

function frontmatter(skillMarkdown) {
  const match = String(skillMarkdown ?? "").match(
    /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/
  )
  if (!match) return null
  const entries = new Map()
  for (const line of match[1].split(/\r?\n/)) {
    const field = line.match(/^([A-Za-z0-9_-]+):\s*(.*)$/)
    if (!field) return null
    entries.set(field[1], field[2].trim().replace(/^(["'])(.*)\1$/, "$2"))
  }
  return entries
}

/**
 * Validate metadata and retirement identifiers without matching workflow
 * prose.
 */
export function validateSkillMarkdown(skillMarkdown) {
  const skill = String(skillMarkdown ?? "")
  const failures = []
  const notes = []
  const metadata = frontmatter(skill)

  if (!metadata) {
    fail(failures, "B2D-SKILL-001", "missing or malformed YAML frontmatter")
  } else {
    const keys = [...metadata.keys()].sort()
    if (keys.join(",") !== "description,name") {
      fail(
        failures,
        "B2D-SKILL-001",
        "frontmatter must contain only name and description"
      )
    }
    if (metadata.get("name") !== "brainstorm-to-delivery") {
      fail(
        failures,
        "B2D-SKILL-001",
        "frontmatter name must be brainstorm-to-delivery"
      )
    }
    const description = metadata.get("description") ?? ""
    if (!/^Use when\b/.test(description)) {
      fail(
        failures,
        "B2D-SKILL-001",
        'description must start with "Use when"'
      )
    }
    if (
      /\b(?:Plan|progress|registration|register|serial|delegate|review|workflow tool)\b/i
        .test(description)
    ) {
      fail(
        failures,
        "B2D-SKILL-001",
        "description must contain triggers only, not workflow steps"
      )
    }
  }

  const lineCount = skill.split(/\r?\n/).length
  if (lineCount >= 500) {
    fail(
      failures,
      "B2D-SKILL-002",
      `SKILL.md has ${lineCount} lines; expected fewer than 500`
    )
  } else {
    notes.push(`SKILL.md line count: ${lineCount}`)
  }

  const lower = skill.toLowerCase()
  for (const identifier of V2_SKILL_IDENTIFIERS) {
    if (lower.includes(identifier.toLowerCase())) {
      fail(
        failures,
        "B2D-SKILL-003",
        `v2-only identifier remains in Skill: ${identifier}`
      )
    }
  }

  return { failures, notes }
}

function fenceStart(line) {
  const match = line.match(/^\s{0,3}(`{3,}|~{3,})/)
  return match ? { character: match[1][0], length: match[1].length } : null
}

function fenceEnd(line, fence) {
  if (!fence) return false
  const escaped = fence.character === "`" ? "`" : "~"
  return new RegExp(`^\\s{0,3}${escaped}{${fence.length},}\\s*$`).test(line)
}

/** Parse the Plan Task headings used by the backend Simple projector. */
export function parseSimplePlan(planMarkdown) {
  const source = String(planMarkdown ?? "")
  const failures = []
  const tasks = []

  if (byteLength(source) > MAX_PLAN_DOCUMENT_BYTES) {
    fail(failures, "B2D-PLAN-001", "Plan exceeds the 2 MiB limit")
    return { tasks, failures }
  }

  let fence = null
  for (const [lineNumber, line] of source.split(/\r?\n/).entries()) {
    if (fence) {
      if (fenceEnd(line, fence)) fence = null
      continue
    }
    fence = fenceStart(line)
    if (fence) continue

    const heading = line.match(/^\s{0,3}#{2,3}\s+(.+?)\s*#*\s*$/)
    if (!heading) continue
    const text = heading[1].trim()
    if (!text.startsWith("Task ")) continue
    const task = text.match(/^Task ([1-9][0-9]*):\s*(\S(?:.*\S)?)$/)
    if (!task) {
      fail(
        failures,
        "B2D-PLAN-002",
        `malformed Task heading at line ${lineNumber + 1}`
      )
      continue
    }
    const index = Number(task[1])
    if (tasks.some((candidate) => candidate.index === index)) {
      fail(failures, "B2D-PLAN-002", `duplicate Task index: ${index}`)
      continue
    }
    tasks.push({ index, title: task[2], line: lineNumber + 1 })
  }

  if (tasks.length === 0) {
    fail(failures, "B2D-PLAN-001", "Plan contains no Task headings")
  }
  if (tasks.some((task, offset) => task.index !== offset + 1)) {
    fail(
      failures,
      "B2D-PLAN-003",
      "Plan Task indices must be contiguous and ordered from 1"
    )
  }

  return { tasks, failures }
}

function markerOffsets(source) {
  const offsets = []
  let offset = 0
  while (offset < source.length) {
    const found = source.indexOf(PROGRESS_MARKER, offset)
    if (found < 0) break
    offsets.push(found)
    offset = found + PROGRESS_MARKER.length
  }
  return offsets
}

function findForbiddenProgressFields(value, path = "$", found = []) {
  if (Array.isArray(value)) {
    value.forEach((entry, index) =>
      findForbiddenProgressFields(entry, `${path}[${index}]`, found)
    )
    return found
  }
  if (!isObject(value)) return found
  for (const [key, entry] of Object.entries(value)) {
    const childPath = `${path}.${key}`
    if (FORBIDDEN_PROGRESS_FIELDS.has(key.toLowerCase())) found.push(childPath)
    findForbiddenProgressFields(entry, childPath, found)
  }
  return found
}

function validateRun(run, taskIndex, runIndex, failures) {
  const label = `Task ${taskIndex} run ${runIndex + 1}`
  if (!isObject(run)) {
    fail(failures, "B2D-PROGRESS-006", `${label} must be an object`)
    return
  }
  for (const field of ["role", "agent_type", "state", "work_unit_key"]) {
    if (!nonEmptyString(run[field])) {
      fail(
        failures,
        "B2D-PROGRESS-006",
        `${label} requires non-empty ${field}`
      )
    }
  }
  const parsedKey = parseRecognizedWorkUnitKey(run.work_unit_key)
  const runProfile =
    run.profile_id === undefined ||
    run.profile_id === null ||
    run.profile_id === "none"
      ? null
      : run.profile_id
  if (
    !parsedKey ||
    parsedKey.kind !== "task" ||
    parsedKey.taskIndex !== taskIndex ||
    parsedKey.role !== run.role ||
    parsedKey.agentType !== run.agent_type ||
    parsedKey.profileId !== runProfile
  ) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} work_unit_key must be a canonical A1 Task key matching ` +
        "its Task, role, agent, and profile"
    )
  }
  if (!RUN_STATES.has(run.state)) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} has unknown state: ${String(run.state)}`
    )
  }
  for (const field of [
    "profile_id",
    "task_id",
    "replaced_task_id",
    "replacement_reason",
  ]) {
    if (!optionalString(run[field])) {
      fail(
        failures,
        "B2D-PROGRESS-006",
        `${label} ${field} must be a string or null`
      )
    }
  }
  if (
    run.child_conversation_id !== undefined &&
    run.child_conversation_id !== null &&
    !positiveInteger(run.child_conversation_id)
  ) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} child_conversation_id must be a positive integer`
    )
  }
  if (
    run.recovery_count !== undefined &&
    run.recovery_count !== null &&
    (!Number.isInteger(run.recovery_count) || run.recovery_count < 0)
  ) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} recovery_count must be a non-negative integer`
    )
  }

  const replaced = nonEmptyString(run.replaced_task_id)
  const reason = nonEmptyString(run.replacement_reason)
  if (replaced !== reason) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} replacement linkage must include both replaced_task_id and ` +
        "replacement_reason"
    )
  }
  if (reason && !REPLACEMENT_REASONS.has(run.replacement_reason)) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} has unsupported replacement_reason: ${run.replacement_reason}`
    )
  }
}

function validateProgressTasks(snapshot, plan, failures) {
  if (!Array.isArray(snapshot.tasks)) {
    fail(failures, "B2D-PROGRESS-005", "progress tasks must be an array")
    return []
  }
  const planIndexes = new Set(plan.tasks.map((task) => task.index))
  const seen = new Set()
  const tasks = []

  for (const task of snapshot.tasks) {
    if (!isObject(task) || !positiveInteger(task.index)) {
      fail(
        failures,
        "B2D-PROGRESS-005",
        "each progress Task requires a positive integer index"
      )
      continue
    }
    if (seen.has(task.index)) {
      fail(
        failures,
        "B2D-PROGRESS-005",
        `duplicate progress Task index: ${task.index}`
      )
      continue
    }
    seen.add(task.index)
    if (!planIndexes.has(task.index)) {
      fail(
        failures,
        "B2D-PROGRESS-005",
        `progress Task ${task.index} is absent from the Plan`
      )
    }
    if (!TASK_STATUSES.has(task.status)) {
      fail(
        failures,
        "B2D-PROGRESS-005",
        `progress Task ${task.index} has unknown status: ${String(task.status)}`
      )
    }
    if (!optionalString(task.commit)) {
      fail(
        failures,
        "B2D-PROGRESS-005",
        `progress Task ${task.index} commit must be a string or null`
      )
    }
    if (!Array.isArray(task.runs)) {
      fail(
        failures,
        "B2D-PROGRESS-006",
        `progress Task ${task.index} runs must be an array`
      )
    } else {
      task.runs.forEach((run, index) =>
        validateRun(run, task.index, index, failures)
      )
    }
    tasks.push(task)
  }
  return tasks
}

function validateSerialState(snapshot, plan, tasks, failures) {
  const byIndex = new Map(tasks.map((task) => [task.index, task]))
  const ordered = plan.tasks.map(
    (task) =>
      byIndex.get(task.index) ?? { index: task.index, status: "pending" }
  )
  const frontiers = ordered.filter((task) =>
    ["in_progress", "blocked"].includes(task.status)
  )
  if (frontiers.length > 1) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      "serial execution permits at most one in_progress or blocked Task"
    )
  }

  const activeIndex = snapshot.active_task_index
  if (
    activeIndex !== undefined &&
    activeIndex !== null &&
    !positiveInteger(activeIndex)
  ) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      "active_task_index must be a positive integer or null"
    )
  } else if (frontiers.length === 1 && activeIndex !== frontiers[0].index) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      "active_task_index must match the in_progress or blocked Task"
    )
  } else if (
    frontiers.length === 0 &&
    activeIndex !== undefined &&
    activeIndex !== null
  ) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      "active_task_index must be null when there is no Task frontier"
    )
  }

  let phase = "completed"
  for (const task of ordered) {
    if (!TASK_STATUSES.has(task.status)) continue
    if (task.status === "completed" && phase === "completed") continue
    if (
      ["in_progress", "blocked"].includes(task.status) &&
      phase === "completed"
    ) {
      phase = "pending"
      continue
    }
    if (task.status === "pending") {
      phase = "pending"
      continue
    }
    fail(
      failures,
      "B2D-PROGRESS-008",
      `Task ${task.index} violates completed-prefix, single-frontier, ` +
        "pending-suffix order"
    )
  }

  if (!TASK_STATUSES.has(snapshot.final_review_status)) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      `unknown final_review_status: ${String(snapshot.final_review_status)}`
    )
  } else if (
    snapshot.final_review_status !== "pending" &&
    ordered.some((task) => task.status !== "completed")
  ) {
    fail(
      failures,
      "B2D-PROGRESS-008",
      "final review cannot start before every Plan Task is completed"
    )
  }
}

/** Parse and validate the exact Simple progress block. */
export function parseSimpleProgress(
  progressMarkdown,
  expectedPlanRelPath,
  plan
) {
  const source = String(progressMarkdown ?? "")
  const failures = []
  const progress = { snapshot: null }

  if (byteLength(source) > MAX_PROGRESS_DOCUMENT_BYTES) {
    fail(
      failures,
      "B2D-PROGRESS-002",
      "progress document exceeds the 512 KiB limit"
    )
    return { ...progress, failures }
  }

  const starts = markerOffsets(source)
  if (starts.length !== 1) {
    fail(
      failures,
      "B2D-PROGRESS-001",
      "progress document must contain exactly one marker; " +
        `found ${starts.length}`
    )
    if (starts.length === 0) return { ...progress, failures }
  }
  const jsonStart = starts[0] + PROGRESS_MARKER.length
  const relativeEnd = source.slice(jsonStart).indexOf(COMMENT_END)
  if (relativeEnd < 0) {
    fail(
      failures,
      "B2D-PROGRESS-001",
      "progress block is missing its closing comment marker"
    )
    return { ...progress, failures }
  }
  const json = source.slice(jsonStart, jsonStart + relativeEnd).trim()
  if (byteLength(json) > MAX_PROGRESS_BLOCK_BYTES) {
    fail(
      failures,
      "B2D-PROGRESS-002",
      "structured progress block exceeds the 64 KiB limit"
    )
    return { ...progress, failures }
  }

  let snapshot
  try {
    snapshot = JSON.parse(json)
  } catch {
    fail(failures, "B2D-PROGRESS-003", "progress block is not valid JSON")
    return { ...progress, failures }
  }
  if (!isObject(snapshot)) {
    fail(failures, "B2D-PROGRESS-003", "progress snapshot must be an object")
    return { ...progress, failures }
  }
  progress.snapshot = snapshot

  const forbiddenFields = findForbiddenProgressFields(snapshot)
  if (forbiddenFields.length > 0) {
    const locations = forbiddenFields.join(", ")
    fail(
      failures,
      "B2D-PROGRESS-007",
      "v2 or transport-only fields are not part of Simple progress: " +
        locations
    )
  }
  if (snapshot.schema_version !== 1) {
    fail(
      failures,
      "B2D-PROGRESS-003",
      "progress schema_version must equal 1"
    )
  }

  const expected = normalizeRelPath(expectedPlanRelPath)
  const actual = normalizeRelPath(snapshot.plan_rel_path)
  if (!expected || !actual || actual !== expected) {
    fail(
      failures,
      "B2D-PROGRESS-004",
      "progress plan_rel_path must match the normalized registered Plan path"
    )
  }
  if (!optionalString(snapshot.updated_at)) {
    fail(
      failures,
      "B2D-PROGRESS-003",
      "updated_at must be a string or null"
    )
  }

  const tasks = validateProgressTasks(snapshot, plan, failures)
  validateSerialState(snapshot, plan, tasks, failures)
  return { ...progress, failures }
}

/** Validate a Skill plus controlled Plan/progress fixtures as one contract. */
export function validateSimpleDocuments({
  skillMarkdown,
  planMarkdown,
  progressMarkdown,
  planRelPath,
}) {
  const skill = validateSkillMarkdown(skillMarkdown)
  const plan = parseSimplePlan(planMarkdown)
  const progress = parseSimpleProgress(progressMarkdown, planRelPath, plan)
  return {
    failures: [...skill.failures, ...plan.failures, ...progress.failures],
    notes: [
      ...skill.notes,
      `Plan Tasks parsed: ${plan.tasks.length}`,
      `Progress Tasks parsed: ${progress.snapshot?.tasks?.length ?? 0}`,
    ],
    plan,
    progress,
  }
}
