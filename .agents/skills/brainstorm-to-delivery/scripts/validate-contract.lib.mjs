/**
 * Deterministic contract checks for the Simple brainstorm-to-delivery Skill
 * and its Plan/progress documents.
 */

export const MAX_PLAN_DOCUMENT_BYTES = 2 * 1024 * 1024
export const MAX_PROGRESS_DOCUMENT_BYTES = 512 * 1024
export const MAX_PROGRESS_BLOCK_BYTES = 64 * 1024
export const MAX_ROUTING_BLOCK_BYTES = 256 * 1024

const MAX_I32 = 0x7fffffff
const MAX_U32 = 0xffffffff
const MAX_UNEXPECTED_CONTINUATIONS = 2

const SKILL_CONTRACT_MARKER = "<!-- codeg-b2d-skill-contract-v2"
const ROUTING_MARKER = "<!-- codeg-b2d-routing-v1"
const PROGRESS_MARKER = "<!-- codeg-simple-progress-v1"
const RISK_POLICY_VERSION = "b2d_task_risk_v1"
const COMMENT_END = "-->"
const SOFT_SIGNAL_SCORES = new Map([
  ["cross_runtime_or_process", 2],
  ["broad_production_surface", 1],
  ["multiple_ownership_modules", 1],
  ["shared_interface", 1],
  ["dependency_or_build", 1],
  ["multi_layer_without_test_seam", 1],
])
const HARD_TRIGGER_KINDS = new Set([
  "concurrency_lifecycle",
  "security_trust_boundary",
  "migration_destructive_persistence",
  "public_compatibility",
  "unsafe_ffi",
  "update_rollback",
])
const REQUIRED_SKILL_CONTRACT = {
  schema_version: 2,
  phase_order: [
    "establish-current-truth",
    "resolve-task-agent",
    "review-and-revise-design",
    "author-and-review-plan",
    "maintain-progress",
    "apply-workspace-gate",
    "execute-tasks-serially",
    "recover-generic-runs",
    "complete-final-review",
  ],
  interfaces: {
    plan_authoring: "writing-plans",
    task_execution: "subagent-driven-development",
    registration: "register_simple_workflow",
    first_run: "delegate_to_agent",
    later_run: "continue_delegation",
    join: "get_delegation_status",
    recovery_authorization: "request_recovery_authorization",
  },
  plan_setup_order: [
    "create-progress",
    "dispatch-plan-author",
    "confirm-plan-on-disk",
    "validate-routing",
    "review-plan",
    "register-simple-workflow",
    "sync-plan-tasks",
  ],
  document_work: {
    parent_edits: false,
    design_review: "conditional",
    design_reviewer: "independent_codex",
    design_fixer: "independent_codex",
    plan_author: "independent_codex",
    plan_reviewer: "independent_codex",
    producer_reviewer_independence: true,
    plan_rereview: "full_latest_plan",
    user_named_reviewers: "design_and_plan_only",
  },
  conversation_identity: {
    distinct_work_units: "distinct_child_conversations",
    continuation: "same_work_unit_only",
  },
  task_agent: {
    default_agent_type: "grok",
    selection_source: "invocation",
    explicit_substitution: "forbidden",
    change_boundary: "completed_tasks_after_plan_revision_and_full_rereview",
  },
  routing: {
    marker: "codeg-b2d-routing-v1",
    risk_policy_version: "b2d_task_risk_v1",
    normal: {
      implementer: "task_agent",
      reviewers: ["codex_primary"],
    },
    high: {
      implementer: "codex",
      reviewers: ["codex_primary", "task_agent_auxiliary"],
    },
    reviewer_slots: ["primary", "auxiliary"],
    task_order: "serial",
    high_review_fan_out: "parallel_after_implementation",
  },
  progress: {
    marker: "codeg-simple-progress-v1",
    mutation_order: [
      "record-reserving-intent",
      "delegate",
      "record-admission",
      "record-observed-state",
    ],
    route_metadata: "additive",
  },
  workspace_policy: "preserve-user-changes",
  recovery: {
    unexpected_continuations: 2,
    logical_replacements: 1,
    replacement_retry: "pre-admission-only",
  },
  final_review: {
    required: true,
    independent: true,
    reviewer: "codex",
    fix_owner: "task_producer",
  },
}
const CONTRACT_ACTIONS = [
  "writing-plans",
  "register_simple_workflow",
  "codeg-simple-progress-v1",
  "reserving",
  "delegate_to_agent",
  "continue_delegation",
  "get_delegation_status",
  "request_recovery_authorization",
  "recovery_confirmation_required",
  "recovery_authorization_id",
  "fresh_dispatch",
  "serial(?:ly)?",
  "unexpected continuations?",
  "logical replacements?",
  "final_review_status",
  "final\\s+review",
  "independent\\s+codex\\s+final\\s+review",
].join("|")
const NEGATIVE_CONTRACT_DIRECTIVE = new RegExp(
  `\\b(?:never|(?:do|does|shall)\\s+not|don't|must\\s+not|` +
    `should\\s+not|skip|omit|avoid|forbid(?:den)?|prohibit(?:ed)?|` +
    `decline\\s+to|refuse\\s+to|refrain\\s+from)\\b` +
    `[^.!?\\n]{0,200}` +
    `(?:${CONTRACT_ACTIONS})`,
  "i"
)
const NEGATED_CONTRACT_ACTION = new RegExp(
  `(?:${CONTRACT_ACTIONS})[^.!?\\n]{0,120}` +
    `\\b(?:must\\s+not|should\\s+not|shall\\s+not|may\\s+not|` +
    `(?:is|are)\\s+(?:forbidden|prohibited))\\b`,
  "i"
)
const POSITIVE_SKILL_DIRECTIVES = new Set([
  "adjudicate",
  "call",
  "choose",
  "commit",
  "complete",
  "continue",
  "create",
  "dispatch",
  "execute",
  "handle",
  "inspect",
  "join",
  "keep",
  "mark",
  "prefer",
  "preserve",
  "read",
  "re-read",
  "record",
  "refresh",
  "replace",
  "replay",
  "report",
  "request",
  "route",
  "run",
  "set",
  "supply",
  "treat",
  "update",
  "use",
  "write",
])
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
const RETIRED_WORKFLOW_IDENTIFIERS = [
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
const DIRECTIVE_WINDOW_TOKENS = 64
const DIRECTIVE_WINDOW_OVERLAP = 16
// Match bounded directive structures only; the embedded positive contract
// remains the authoritative workflow definition.
const PRODUCTION_ACTIONS = new Set([
  "author",
  "authored",
  "authors",
  "authoring",
  "change",
  "changed",
  "changes",
  "changing",
  "create",
  "created",
  "creates",
  "creating",
  "edit",
  "edited",
  "edits",
  "editing",
  "fix",
  "fixed",
  "fixes",
  "fixing",
  "implement",
  "implemented",
  "implementation",
  "implementer",
  "implements",
  "implementing",
  "modify",
  "modified",
  "modifies",
  "modifying",
  "own",
  "owned",
  "owns",
  "patch",
  "patched",
  "patches",
  "patching",
  "produce",
  "produced",
  "produces",
  "producing",
  "revise",
  "revised",
  "revises",
  "revising",
  "update",
  "updated",
  "updates",
  "updating",
  "write",
  "writes",
  "writing",
  "written",
  "wrote",
])
const TASK_ROUTE_ACTIONS = new Set([
  ...PRODUCTION_ACTIONS,
  "delegate",
  "delegated",
  "delegates",
  "delegating",
  "route",
  "routed",
  "routes",
  "routing",
])
const DOCUMENT_OR_CODE_TARGETS = new Set([
  "code",
  "design",
  "designs",
  "implementation",
  "plan",
  "plans",
  "task",
  "tasks",
])
const NEGATION_TERMS = new Set([
  "forbid",
  "forbidden",
  "forbids",
  "never",
  "no",
  "not",
  "prevent",
  "prevented",
  "preventing",
  "prevents",
  "prohibit",
  "prohibited",
  "prohibits",
  "without",
])
const ACTOR_LINKS = new Set(["by", "to"])
const HIGH_TASK_SCOPES = new Set(["high", "high-risk"])
const UNIVERSAL_TASK_SCOPES = new Set([
  "all",
  "always",
  "each",
  "every",
  "unconditionally",
])
const TASK_ACTIVITY_TERMS = new Set([
  "active",
  "current",
  "in-progress",
  "running",
])
const TASK_COMPLETION_TERMS = new Set([
  "complete",
  "completed",
  "completes",
  "completion",
  "finish",
  "finished",
  "finishes",
])
const REVIEW_BYPASS_ACTIONS = new Set([
  "instead",
  "omit",
  "omits",
  "omitted",
  "omitting",
  "optional",
  "optionally",
  "replace",
  "replaced",
  "replaces",
  "replacing",
  "skip",
  "skipped",
  "skips",
  "skipping",
  "substitute",
  "substituted",
  "substitutes",
  "substituting",
])
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
  if (nfc.startsWith("/") || nfc.startsWith("\\\\") || /^[A-Za-z]:/.test(nfc)) {
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
    return {
      kind: "task",
      taskIndex,
      role,
      slot: role === "reviewer" ? "primary" : null,
      agentType,
      profileId,
      legacy: role === "reviewer",
    }
  }

  if (parts[0] === "task" && parts.length === 6) {
    const [, indexToken, role, slot, agentType, profileToken] = parts
    if (
      !/^[1-9][0-9]*$/.test(indexToken) ||
      role !== "reviewer" ||
      !["primary", "auxiliary"].includes(slot) ||
      !validAgentType(agentType)
    ) {
      return null
    }
    const taskIndex = Number(indexToken)
    const profileId = parseProfileToken(profileToken)
    if (
      !Number.isInteger(taskIndex) ||
      taskIndex > MAX_U32 ||
      profileId === undefined
    ) {
      return null
    }
    return {
      kind: "task",
      taskIndex,
      role,
      slot,
      agentType,
      profileId,
      legacy: false,
    }
  }

  if (["design", "plan"].includes(parts[0]) && parts.length === 5) {
    const [kind, path, role, agentType, profileToken] = parts
    const normalizedPath = normalizeRelPath(path)
    const allowedRole =
      (kind === "design" && ["reviewer", "fixer"].includes(role)) ||
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
    return { kind, path, role, agentType, profileId, legacy: false }
  }

  if (parts[0] === "final_review" && parts.length === 4) {
    const [, role, agentType, profileToken] = parts
    const profileId = parseProfileToken(profileToken)
    if (
      role !== "reviewer" ||
      !validAgentType(agentType) ||
      profileId === undefined
    ) {
      return null
    }
    return {
      kind: "final_review",
      role,
      agentType,
      profileId,
      legacy: false,
    }
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

function numberedSkillSections(skill) {
  const sections = []
  let current = null
  let fence = null
  for (const [lineIndex, line] of skill.split(/\r?\n/).entries()) {
    if (fence) {
      if (fenceEnd(line, fence)) fence = null
      continue
    }
    fence = fenceStart(line)
    if (fence) continue
    const heading = line.match(/^##\s+([1-9][0-9]*)\.\s+\S/)
    if (heading) {
      current = {
        index: Number(heading[1]),
        line: lineIndex + 1,
        lines: [],
      }
      sections.push(current)
    } else if (current) {
      current.lines.push(line)
    }
  }
  return sections.map((section) => ({
    index: section.index,
    line: section.line,
    body: section.lines.join("\n"),
  }))
}

function embeddedSkillContracts(skill) {
  const extracted = extractUnfencedComment(
    skill,
    SKILL_CONTRACT_MARKER,
    512 * 1024
  )
  return {
    contracts:
      extracted.body === null
        ? []
        : [{ line: extracted.line, source: extracted.body }],
    markerCount: extracted.markerCount,
    unterminated: extracted.problem === "truncated",
  }
}

function canonicalJson(value) {
  if (Array.isArray(value)) return value.map(canonicalJson)
  if (!isObject(value)) return value
  return Object.fromEntries(
    Object.keys(value)
      .sort()
      .map((key) => [key, canonicalJson(value[key])])
  )
}

function skillContractsEqual(actual, expected) {
  return (
    JSON.stringify(canonicalJson(actual)) ===
    JSON.stringify(canonicalJson(expected))
  )
}

function visibleSkillProse(body) {
  return body.replace(/<!--[\s\S]*?-->/g, " ")
}

function unfencedVisibleSkillProse(skill) {
  const lines = []
  let fence = null
  for (const line of skill.split(/\r?\n/)) {
    if (fence) {
      if (fenceEnd(line, fence)) fence = null
      continue
    }
    fence = fenceStart(line)
    if (!fence) lines.push(line)
  }
  return visibleSkillProse(lines.join("\n"))
}

function directiveWindows(prose) {
  const windows = []
  const step = DIRECTIVE_WINDOW_TOKENS - DIRECTIVE_WINDOW_OVERLAP
  const withoutKeys = prose.replace(
    /\b(?:design|final_review|plan|task)\|[^\s.,;:]+/gi,
    " "
  )
  for (const clause of withoutKeys
    .normalize("NFKC")
    .toLowerCase()
    .split(/[.!?;]+|\n\s*\n+/)) {
    const tokens = clause.match(/[a-z0-9]+(?:-[a-z0-9]+)*/g) ?? []
    for (let start = 0; start < tokens.length; start += step) {
      windows.push(tokens.slice(start, start + DIRECTIVE_WINDOW_TOKENS))
    }
  }
  return windows
}

function tokenIndex(tokens, candidates, start = 0, end = tokens.length) {
  const limit = Math.min(tokens.length, end)
  for (let index = Math.max(0, start); index < limit; index += 1) {
    if (candidates.has(tokens[index])) return index
  }
  return -1
}

function tokenIndexes(tokens, candidates, start = 0, end = tokens.length) {
  const indexes = []
  const limit = Math.min(tokens.length, end)
  for (let index = Math.max(0, start); index < limit; index += 1) {
    if (candidates.has(tokens[index])) indexes.push(index)
  }
  return indexes
}

function phraseIndex(tokens, phrase, start = 0, end = tokens.length) {
  const limit = Math.min(tokens.length, end) - phrase.length
  for (let index = Math.max(0, start); index <= limit; index += 1) {
    if (phrase.every((token, offset) => tokens[index + offset] === token)) {
      return index
    }
  }
  return -1
}

function actionIsNegated(tokens, actionIndex) {
  const prefix = tokens.slice(Math.max(0, actionIndex - 6), actionIndex)
  return (
    prefix.some((token) => NEGATION_TERMS.has(token)) ||
    prefix.at(-1) === "from"
  )
}

function taskAgentActorIndexes(tokens) {
  const indexes = []
  for (const [index, token] of tokens.entries()) {
    if (
      token === "grok" ||
      (token === "task" && tokens[index + 1] === "agent")
    ) {
      indexes.push(index)
    }
  }
  return indexes
}

function hasScopedTask(tokens, start = 0, end = tokens.length) {
  for (const taskIndex of tokenIndexes(
    tokens,
    new Set(["task", "tasks"]),
    start,
    end
  )) {
    const scopeStart = Math.max(start, taskIndex - 3)
    const scopeEnd = Math.min(end, taskIndex + 2)
    if (
      tokenIndex(tokens, HIGH_TASK_SCOPES, scopeStart, scopeEnd) >= 0 ||
      tokenIndex(tokens, UNIVERSAL_TASK_SCOPES, scopeStart, scopeEnd) >= 0
    ) {
      return true
    }
  }
  return false
}

function hasTaskActivity(tokens) {
  if (
    tokens.some((token) =>
      ["active-task", "current-task", "running-task"].includes(token)
    )
  ) {
    return true
  }
  return tokenIndexes(tokens, new Set(["task", "tasks"])).some(
    (taskIndex) =>
      tokenIndex(
        tokens,
        TASK_ACTIVITY_TERMS,
        Math.max(0, taskIndex - 2),
        taskIndex + 4
      ) >= 0
  )
}

function hasDocumentProducerBetween(tokens, start, end) {
  return (
    phraseIndex(tokens, ["plan", "author"], start, end) >= 0 ||
    phraseIndex(tokens, ["design", "fixer"], start, end) >= 0
  )
}

function actionHasCodexPassiveActor(tokens, action) {
  const by = tokenIndex(tokens, new Set(["by"]), action + 1, action + 5)
  return by >= 0 && tokenIndex(tokens, new Set(["codex"]), by + 1, by + 4) >= 0
}

function actorLinkIsNegated(tokens, link, actor) {
  return tokenIndex(tokens, NEGATION_TERMS, Math.max(0, link - 3), actor) >= 0
}

function hasCompletedTaskBoundary(tokens, change) {
  const after = tokenIndex(tokens, new Set(["after"]), change + 1)
  if (after < 0) return false
  const task = tokenIndex(tokens, new Set(["task", "tasks"]), after + 1)
  if (task < 0) return false
  return tokenIndexes(tokens, TASK_COMPLETION_TERMS, task + 1, task + 5).some(
    (completion) => !actionIsNegated(tokens, completion)
  )
}

function hasReviewRoleNear(tokens, bypass, roles) {
  return tokenIndexes(tokens, roles, Math.max(0, bypass - 8), bypass + 9).some(
    (role) =>
      tokenIndex(
        tokens,
        new Set(["review", "reviewer", "reviewers"]),
        Math.max(0, role - 1),
        role + 3
      ) >= 0
  )
}

function isOptionalDocumentReviewer(tokens, bypass) {
  if (!["optional", "optionally"].includes(tokens[bypass])) return false
  const start = Math.max(0, bypass - 2)
  const end = Math.min(tokens.length, bypass + 8)
  const hasUserNamed =
    tokenIndex(tokens, new Set(["user-named"]), start, end) >= 0 ||
    phraseIndex(tokens, ["user", "named"], start, end) >= 0
  return (
    hasUserNamed &&
    tokenIndex(tokens, new Set(["design", "plan"]), start, end) >= 0 &&
    tokenIndex(
      tokens,
      new Set(["review", "reviewer", "reviewers"]),
      start,
      end
    ) >= 0
  )
}

function hasCodexDesignReviewNear(tokens, bypass) {
  return tokenIndexes(
    tokens,
    new Set(["codex"]),
    Math.max(0, bypass - 8),
    bypass + 9
  ).some(
    (codex) =>
      tokenIndex(tokens, new Set(["design"]), codex, codex + 3) >= 0 &&
      tokenIndex(
        tokens,
        new Set(["review", "reviewer", "reviewers"]),
        codex,
        codex + 4
      ) >= 0
  )
}

function conflictsWithParentOwnership(tokens) {
  const parent = tokenIndex(tokens, new Set(["parent"]))
  if (parent < 0) return false
  const activeConflict = tokenIndexes(
    tokens,
    PRODUCTION_ACTIONS,
    parent + 1,
    parent + 12
  ).some(
    (action) =>
      !actionIsNegated(tokens, action) &&
      !hasDocumentProducerBetween(tokens, parent + 1, action + 1) &&
      tokenIndex(tokens, DOCUMENT_OR_CODE_TARGETS, action + 1, action + 10) >= 0
  )
  if (activeConflict) return true

  const by = tokens.lastIndexOf("by", parent - 1)
  if (by < Math.max(0, parent - 3)) return false
  return tokenIndexes(
    tokens,
    PRODUCTION_ACTIONS,
    Math.max(0, by - 12),
    by
  ).some(
    (action) =>
      !actionIsNegated(tokens, action) &&
      tokenIndex(
        tokens,
        DOCUMENT_OR_CODE_TARGETS,
        Math.max(0, action - 10),
        action
      ) >= 0
  )
}

function conflictsWithTaskAgentRoute(tokens) {
  for (const actor of taskAgentActorIndexes(tokens)) {
    for (const action of tokenIndexes(
      tokens,
      TASK_ROUTE_ACTIONS,
      actor + 1,
      actor + 9
    )) {
      if (tokens[action - 1] === "codex" || actionIsNegated(tokens, action)) {
        continue
      }
      if (actionHasCodexPassiveActor(tokens, action)) continue
      const alwaysImplementer =
        tokens[action] === "implementer" &&
        tokenIndex(
          tokens,
          UNIVERSAL_TASK_SCOPES,
          Math.max(0, actor - 4),
          action + 1
        ) >= 0
      if (alwaysImplementer || hasScopedTask(tokens, action + 1, action + 12)) {
        return true
      }
    }

    const link = tokenIndexes(
      tokens,
      ACTOR_LINKS,
      Math.max(0, actor - 3),
      actor
    ).at(-1)
    if (link === undefined || actorLinkIsNegated(tokens, link, actor)) continue
    for (const action of tokenIndexes(
      tokens,
      TASK_ROUTE_ACTIONS,
      Math.max(0, link - 12),
      link
    )) {
      if (
        !actionIsNegated(tokens, action) &&
        tokenIndex(tokens, ACTOR_LINKS, action + 1, link) < 0 &&
        hasScopedTask(tokens, Math.max(0, action - 5), link)
      ) {
        return true
      }
    }
  }
  return false
}

function conflictsWithConversationIdentity(tokens) {
  const reuse = tokenIndex(
    tokens,
    new Set(["reuse", "reuses", "reusing", "share", "shares", "sharing"])
  )
  if (reuse < 0 || actionIsNegated(tokens, reuse)) return false
  return (
    tokenIndex(tokens, new Set(["conversation", "conversations"])) >= 0 &&
    tokenIndex(tokens, new Set(["implementation", "implementer"])) >= 0 &&
    tokenIndex(tokens, new Set(["review", "reviewer", "reviewers"])) >= 0
  )
}

function conflictsWithActiveTaskSwitch(tokens) {
  const change = tokenIndex(
    tokens,
    new Set([
      "change",
      "changes",
      "changing",
      "replace",
      "replaces",
      "replacing",
      "switch",
      "switches",
      "switching",
    ])
  )
  if (change < 0 || actionIsNegated(tokens, change)) return false
  const hasAgent =
    tokenIndex(tokens, new Set(["agent", "agents"])) >= 0 ||
    phraseIndex(tokens, ["task", "agent"]) >= 0
  return (
    hasAgent &&
    hasTaskActivity(tokens) &&
    !hasCompletedTaskBoundary(tokens, change)
  )
}

function conflictsWithRequiredReview(tokens) {
  return tokenIndexes(tokens, REVIEW_BYPASS_ACTIONS).some((bypass) => {
    if (
      actionIsNegated(tokens, bypass) ||
      isOptionalDocumentReviewer(tokens, bypass)
    ) {
      return false
    }
    return (
      hasReviewRoleNear(tokens, bypass, new Set(["auxiliary", "primary"])) ||
      hasCodexDesignReviewNear(tokens, bypass)
    )
  })
}

function hasConflictingSkillDirective(prose) {
  return directiveWindows(prose).some(
    (tokens) =>
      conflictsWithParentOwnership(tokens) ||
      conflictsWithTaskAgentRoute(tokens) ||
      conflictsWithConversationIdentity(tokens) ||
      conflictsWithActiveTaskSwitch(tokens) ||
      conflictsWithRequiredReview(tokens)
  )
}

function hasSubstantiveSkillProse(body) {
  const prose = visibleSkillProse(body)
    .replace(/`[^`\r\n]*`/g, " ")
    .replace(/^\s*\|.*$/gm, " ")
  const words = prose.match(/[A-Za-z][A-Za-z-]*/g) ?? []
  const directives = words.filter((word) =>
    POSITIVE_SKILL_DIRECTIVES.has(word.toLowerCase())
  )
  return (
    words.length >= 6 && directives.length >= 2 && /[.!:](?:\s|$)/m.test(prose)
  )
}

function validateEmbeddedSkillContract(skill, firstSectionLine, failures) {
  const { contracts, markerCount, unterminated } = embeddedSkillContracts(skill)
  if (unterminated || markerCount !== 1 || contracts.length !== 1) {
    fail(
      failures,
      "B2D-SKILL-004",
      "Skill requires exactly one complete unfenced structured contract"
    )
    return
  }

  const embedded = contracts[0]
  if (embedded.line >= firstSectionLine) {
    fail(
      failures,
      "B2D-SKILL-004",
      "Structured contract must precede the numbered workflow"
    )
  }

  let contract
  try {
    contract = JSON.parse(embedded.source)
  } catch {
    fail(
      failures,
      "B2D-SKILL-004",
      "Structured contract must contain valid JSON"
    )
    return
  }

  if (!skillContractsEqual(contract, REQUIRED_SKILL_CONTRACT)) {
    fail(
      failures,
      "B2D-SKILL-004",
      "Structured contract must match the required positive Simple semantics"
    )
  }
}

function validateOrderedSkillContract(skill, failures) {
  const sections = numberedSkillSections(skill)
  if (
    sections.map((section) => section.index).join(",") !== "1,2,3,4,5,6,7,8,9"
  ) {
    fail(
      failures,
      "B2D-SKILL-004",
      "Skill must contain exactly nine numbered workflow sections in order"
    )
    return
  }

  validateEmbeddedSkillContract(skill, sections[0].line, failures)
  for (const section of sections) {
    if (!hasSubstantiveSkillProse(section.body)) {
      fail(
        failures,
        "B2D-SKILL-004",
        `Skill section ${section.index} requires substantive unfenced guidance`
      )
    }
  }

  const prose = unfencedVisibleSkillProse(skill)
  if (
    NEGATIVE_CONTRACT_DIRECTIVE.test(prose) ||
    NEGATED_CONTRACT_ACTION.test(prose)
  ) {
    fail(
      failures,
      "B2D-SKILL-004",
      "Skill prose negates a required contract action"
    )
  }
}

/**
 * Validate metadata and the embedded positive contract without matching exact
 * natural-language workflow prose.
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
      fail(failures, "B2D-SKILL-001", 'description must start with "Use when"')
    }
    if (
      /\b(?:Plan|progress|registration|register|serial|delegate|review|workflow tool)\b/i.test(
        description
      )
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
  for (const identifier of RETIRED_WORKFLOW_IDENTIFIERS) {
    if (lower.includes(identifier.toLowerCase())) {
      fail(
        failures,
        "B2D-SKILL-003",
        `retired workflow identifier remains in Skill: ${identifier}`
      )
    }
  }

  validateOrderedSkillContract(skill, failures)
  const prose = unfencedVisibleSkillProse(skill)
  if (hasConflictingSkillDirective(prose)) {
    fail(
      failures,
      "B2D-SKILL-005",
      "Skill prose contradicts required v2 ownership or routing"
    )
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

function extractUnfencedComment(source, marker, maxBlockBytes) {
  const lines = String(source ?? "").split(/\r?\n/)
  let fence = null
  let active = null
  let markerCount = 0
  let body = null
  let firstLine = 0
  let problem = null

  for (const [lineIndex, line] of lines.entries()) {
    if (active) {
      const end = line.indexOf(COMMENT_END)
      active.push(end < 0 ? line : line.slice(0, end))
      if (byteLength(active.join("\n")) > maxBlockBytes && !problem) {
        problem = "too_large"
      }
      if (end >= 0) {
        if (body === null && problem !== "too_large") body = active.join("\n")
        active = null
      }
      continue
    }
    if (fence) {
      if (fenceEnd(line, fence)) fence = null
      continue
    }
    fence = fenceStart(line)
    if (fence) continue
    if (line.trim() === marker) {
      markerCount += 1
      if (firstLine === 0) firstLine = lineIndex + 1
      if (body === null) active = []
    }
  }
  if (active && !problem) problem = "truncated"
  return { body, markerCount, problem, line: firstLine }
}

/** Parse the bounded authoritative routing block from a Plan. */
export function parseSimpleRouting(planMarkdown) {
  const failures = []
  const extracted = extractUnfencedComment(
    planMarkdown,
    ROUTING_MARKER,
    MAX_ROUTING_BLOCK_BYTES
  )
  if (extracted.markerCount !== 1 || extracted.problem === "truncated") {
    fail(
      failures,
      "B2D-ROUTING-001",
      "Plan must contain exactly one complete unfenced routing block"
    )
    return { snapshot: null, failures }
  }
  if (extracted.problem === "too_large") {
    fail(failures, "B2D-ROUTING-002", "routing block exceeds 256 KiB")
    return { snapshot: null, failures }
  }
  let snapshot
  try {
    snapshot = JSON.parse(extracted.body.trim())
  } catch {
    fail(failures, "B2D-ROUTING-003", "routing block is not valid JSON")
    return { snapshot: null, failures }
  }
  if (!isObject(snapshot)) {
    fail(failures, "B2D-ROUTING-003", "routing snapshot must be an object")
    return { snapshot: null, failures }
  }
  return { snapshot, failures }
}

function validProfileId(value) {
  return (
    value === null ||
    (nonEmptyString(value) &&
      [...value].length <= 128 &&
      value !== "none" &&
      !value.includes("|") &&
      !hasControl(value))
  )
}

function validAgentSelection(value) {
  return (
    isObject(value) &&
    validAgentType(value.agent_type) &&
    validProfileId(value.profile_id)
  )
}

function validateEvidenceList(evidence, label, failures) {
  if (
    !Array.isArray(evidence) ||
    evidence.length === 0 ||
    evidence.some((entry) => !nonEmptyString(entry)) ||
    new Set(evidence).size !== evidence.length
  ) {
    fail(
      failures,
      "B2D-RISK-002",
      `${label} requires unique non-empty evidence strings`
    )
  }
}

function validateTaskRisk(value, taskIndex, failures) {
  const label = `Task ${taskIndex} risk`
  if (!isObject(value)) {
    fail(failures, "B2D-RISK-001", `${label} must be an object`)
    return null
  }
  if (
    !Array.isArray(value.hard_triggers) ||
    !Array.isArray(value.soft_signals)
  ) {
    fail(failures, "B2D-RISK-001", `${label} requires evidence arrays`)
    return null
  }
  const hardKinds = new Set()
  for (const trigger of value.hard_triggers) {
    if (
      !isObject(trigger) ||
      !HARD_TRIGGER_KINDS.has(trigger.kind) ||
      hardKinds.has(trigger.kind)
    ) {
      fail(
        failures,
        "B2D-RISK-001",
        `${label} has unknown or duplicate hard trigger`
      )
      continue
    }
    hardKinds.add(trigger.kind)
    validateEvidenceList(trigger.evidence, `${label} ${trigger.kind}`, failures)
  }
  const softKinds = new Set()
  const softEvidence = new Set()
  let softTotal = 0
  for (const signal of value.soft_signals) {
    if (
      !isObject(signal) ||
      !SOFT_SIGNAL_SCORES.has(signal.kind) ||
      softKinds.has(signal.kind)
    ) {
      fail(
        failures,
        "B2D-RISK-001",
        `${label} has unknown or duplicate soft signal`
      )
      continue
    }
    softKinds.add(signal.kind)
    const expectedScore = SOFT_SIGNAL_SCORES.get(signal.kind)
    if (signal.score !== expectedScore) {
      fail(failures, "B2D-RISK-003", `${label} has a wrong soft-signal score`)
    }
    softTotal += expectedScore
    validateEvidenceList(signal.evidence, `${label} ${signal.kind}`, failures)
    if (Array.isArray(signal.evidence)) {
      for (const evidence of signal.evidence) {
        if (softEvidence.has(evidence)) {
          fail(
            failures,
            "B2D-RISK-001",
            `${label} counts one evidence string in multiple soft signals`
          )
        }
        softEvidence.add(evidence)
      }
    }
  }
  if (value.score !== softTotal) {
    fail(
      failures,
      "B2D-RISK-003",
      `${label} score must equal the soft-signal total`
    )
  }
  const expectedLevel = hardKinds.size > 0 || softTotal >= 3 ? "high" : "normal"
  if (value.level !== expectedLevel) {
    fail(failures, "B2D-RISK-004", `${label} level contradicts the risk policy`)
  }
  if (!nonEmptyString(value.reason)) {
    fail(failures, "B2D-RISK-005", `${label} requires a non-empty reason`)
  }
  return expectedLevel
}

/** Derive the only accepted route and canonical work-unit keys. */
export function deriveExpectedRoute(task, generation, failures) {
  const taskAgent = {
    agent_type: generation?.agent_type,
    profile_id: generation?.profile_id,
  }
  if (!validAgentSelection(taskAgent)) {
    fail(
      failures,
      "B2D-ROUTING-005",
      `Task ${task?.index} has no valid Task Agent`
    )
    return null
  }
  const high = task?.risk?.level === "high"
  const profile = taskAgent.profile_id ?? "none"
  const expectedRoute = high
    ? {
        implementer: { agent_type: "codex", profile_id: null },
        reviewers: [
          { slot: "primary", agent_type: "codex", profile_id: null },
          { slot: "auxiliary", ...taskAgent },
        ],
      }
    : {
        implementer: taskAgent,
        reviewers: [{ slot: "primary", agent_type: "codex", profile_id: null }],
      }
  const expectedWorkUnitKeys = {
    implementer: high
      ? `task|${task.index}|implementer|codex|none`
      : `task|${task.index}|implementer|${taskAgent.agent_type}|${profile}`,
    reviewers: {
      primary: `task|${task.index}|reviewer|primary|codex|none`,
      auxiliary: high
        ? `task|${task.index}|reviewer|auxiliary|${taskAgent.agent_type}|${profile}`
        : null,
    },
  }
  const derivedKeys = [
    expectedWorkUnitKeys.implementer,
    expectedWorkUnitKeys.reviewers.primary,
    expectedWorkUnitKeys.reviewers.auxiliary,
  ].filter(nonEmptyString)
  if (derivedKeys.some((key) => !parseRecognizedWorkUnitKey(key))) {
    fail(
      failures,
      "B2D-ROUTING-009",
      `Task ${task.index} derives a non-canonical work-unit key`
    )
    return null
  }
  return {
    route: expectedRoute,
    expected_work_unit_keys: expectedWorkUnitKeys,
  }
}

/** Validate routing semantics and return normalized generations and Tasks. */
export function validateRoutingSnapshot(snapshot, plan, failures) {
  const normalized = { generations: [], tasks: [] }
  if (!isObject(snapshot)) {
    fail(failures, "B2D-ROUTING-003", "routing snapshot must be an object")
    return normalized
  }
  if (
    snapshot.schema_version !== 1 ||
    snapshot.risk_policy_version !== RISK_POLICY_VERSION
  ) {
    fail(
      failures,
      "B2D-ROUTING-003",
      "routing schema or risk policy is unsupported"
    )
  }

  const rawGenerations = snapshot.task_agent_generations
  if (!Array.isArray(rawGenerations) || rawGenerations.length === 0) {
    fail(
      failures,
      "B2D-ROUTING-006",
      "routing must serialize a non-empty Task Agent generation array"
    )
  } else {
    for (const [offset, generation] of rawGenerations.entries()) {
      if (!isObject(generation) || !validAgentSelection(generation)) {
        fail(
          failures,
          "B2D-ROUTING-005",
          `generation ${offset + 1} has an invalid Agent/profile`
        )
        continue
      }
      if (
        generation.generation !== offset + 1 ||
        !positiveInteger(generation.effective_from_task_index) ||
        (offset === 0 && generation.effective_from_task_index !== 1) ||
        (offset > 0 &&
          (!isObject(rawGenerations[offset - 1]) ||
            generation.effective_from_task_index <=
              rawGenerations[offset - 1].effective_from_task_index))
      ) {
        fail(
          failures,
          "B2D-ROUTING-006",
          "generations must be contiguous with increasing boundaries"
        )
      }
      normalized.generations.push({
        generation: generation.generation,
        agent_type: generation.agent_type,
        profile_id: generation.profile_id,
        effective_from_task_index: generation.effective_from_task_index,
      })
    }
  }

  if (!Array.isArray(snapshot.tasks)) {
    fail(failures, "B2D-ROUTING-004", "routing tasks must be an array")
    return normalized
  }
  const planIndexes = plan.tasks.map((task) => task.index)
  if (
    snapshot.tasks.map((task) => task?.index).join(",") !==
    planIndexes.join(",")
  ) {
    fail(
      failures,
      "B2D-ROUTING-004",
      "routing Task indices must exactly match Plan headings"
    )
  }
  for (const routeTask of snapshot.tasks) {
    if (!isObject(routeTask) || !positiveInteger(routeTask.index)) continue
    const generation = normalized.generations.find(
      (candidate) => candidate.generation === routeTask.task_agent_generation
    )
    if (!generation) {
      fail(
        failures,
        "B2D-ROUTING-006",
        `Task ${routeTask.index} references an unknown generation`
      )
      continue
    }
    const applicableGeneration = normalized.generations
      .filter(
        (candidate) => candidate.effective_from_task_index <= routeTask.index
      )
      .at(-1)
    if (applicableGeneration?.generation !== routeTask.task_agent_generation) {
      fail(
        failures,
        "B2D-ROUTING-006",
        `Task ${routeTask.index} does not use the generation active at its boundary`
      )
    }
    const expectedLevel = validateTaskRisk(
      routeTask.risk,
      routeTask.index,
      failures
    )
    const derived = deriveExpectedRoute(routeTask, generation, failures)
    if (
      !derived ||
      !isObject(routeTask.route) ||
      !skillContractsEqual(routeTask.route, derived.route)
    ) {
      fail(
        failures,
        "B2D-ROUTING-009",
        `Task ${routeTask.index} route is not the exact deterministic route`
      )
    }
    normalized.tasks.push({
      ...routeTask,
      risk: {
        ...routeTask.risk,
        level: expectedLevel ?? routeTask.risk?.level,
      },
      expected_work_unit_keys: derived?.expected_work_unit_keys ?? null,
    })
  }
  for (const generation of normalized.generations) {
    const first = normalized.tasks.find(
      (routeTask) => routeTask.task_agent_generation === generation.generation
    )
    if (!first || first.index !== generation.effective_from_task_index) {
      fail(
        failures,
        "B2D-ROUTING-006",
        `generation ${generation.generation} boundary must equal its first Task`
      )
    }
  }
  return normalized
}

/** Parse the Plan Task headings used by the backend Simple projector. */
export function parseSimplePlan(planMarkdown) {
  const source = String(planMarkdown ?? "")
  const failures = []
  const tasks = []

  if (byteLength(source) > MAX_PLAN_DOCUMENT_BYTES) {
    fail(failures, "B2D-PLAN-001", "Plan exceeds the 2 MiB limit")
    return { tasks, routing: null, failures }
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

  const extracted = extractUnfencedComment(
    source,
    ROUTING_MARKER,
    MAX_ROUTING_BLOCK_BYTES
  )
  let routing = null
  if (extracted.markerCount > 0) {
    const parsedRouting = parseSimpleRouting(source)
    failures.push(...parsedRouting.failures)
    routing = parsedRouting.snapshot
  }
  return { tasks, routing, failures }
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
      fail(failures, "B2D-PROGRESS-006", `${label} requires non-empty ${field}`)
    }
  }
  const parsedKey = parseRecognizedWorkUnitKey(run.work_unit_key)
  if (run.profile_id === "none") {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} profile_id must use null rather than the key token "none"`
    )
  }
  const runProfile =
    run.profile_id === undefined || run.profile_id === null
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
    (!positiveInteger(run.child_conversation_id) ||
      run.child_conversation_id > MAX_I32)
  ) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} child_conversation_id must be a positive signed 32-bit integer`
    )
  }
  if (
    run.recovery_count !== undefined &&
    run.recovery_count !== null &&
    (!Number.isInteger(run.recovery_count) ||
      run.recovery_count < 0 ||
      run.recovery_count > MAX_U32)
  ) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} recovery_count must be an unsigned 32-bit integer`
    )
  } else if (run.recovery_count > MAX_UNEXPECTED_CONTINUATIONS) {
    fail(
      failures,
      "B2D-PROGRESS-006",
      `${label} recovery_count permits at most 2 unexpected continuations`
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

function runProfileIdentity(run) {
  return run.profile_id === undefined || run.profile_id === null
    ? null
    : run.profile_id
}

function validateTaskRunLineages(task, failures, taskIds, childOwners) {
  const groups = new Map()
  for (const [runIndex, run] of task.runs.entries()) {
    if (!isObject(run)) continue
    if (nonEmptyString(run.task_id)) {
      if (taskIds.has(run.task_id)) {
        fail(
          failures,
          "B2D-PROGRESS-006",
          `Task ${task.index} run ${runIndex + 1} repeats task_id ` +
            run.task_id
        )
      } else {
        taskIds.add(run.task_id)
      }
    }
    if (
      positiveInteger(run.child_conversation_id) &&
      nonEmptyString(run.work_unit_key)
    ) {
      const owner = childOwners.get(run.child_conversation_id)
      if (owner && owner !== run.work_unit_key) {
        fail(
          failures,
          "B2D-PROGRESS-006",
          `child conversation ${run.child_conversation_id} is shared by distinct work-unit keys`
        )
      } else {
        childOwners.set(run.child_conversation_id, run.work_unit_key)
      }
    }
    if (!nonEmptyString(run.work_unit_key)) continue
    const group = groups.get(run.work_unit_key) ?? []
    group.push({ run, runIndex })
    groups.set(run.work_unit_key, group)
  }

  for (const [workUnitKey, entries] of groups) {
    const first = entries[0].run
    const firstProfile = runProfileIdentity(first)
    const priorTaskIds = new Set()
    const replacementAttempts = new Map()
    for (const { run, runIndex } of entries) {
      const label = `Task ${task.index} ${workUnitKey} run ${runIndex + 1}`
      if (
        run.work_unit_key !== first.work_unit_key ||
        run.agent_type !== first.agent_type ||
        runProfileIdentity(run) !== firstProfile
      ) {
        fail(
          failures,
          "B2D-PROGRESS-006",
          `${label} changes key, agent, or profile within one lineage`
        )
      }

      const replaced = nonEmptyString(run.replaced_task_id)
      const reason = nonEmptyString(run.replacement_reason)
      if (replaced && reason) {
        if (!priorTaskIds.has(run.replaced_task_id)) {
          fail(
            failures,
            "B2D-PROGRESS-006",
            `${label} replaced_task_id must name a prior same-lineage run`
          )
        }
        const replacement = JSON.stringify([
          run.replaced_task_id,
          run.replacement_reason,
        ])
        const priorAttempt = replacementAttempts.get(replacement)
        if (
          priorAttempt &&
          (priorAttempt.state !== "failed" ||
            (priorAttempt.child_conversation_id !== undefined &&
              priorAttempt.child_conversation_id !== null))
        ) {
          fail(
            failures,
            "B2D-PROGRESS-006",
            `${label} repeats a replacement after its prior attempt was ` +
              "admitted"
          )
        }
        replacementAttempts.set(replacement, run)
      }
      if (nonEmptyString(run.task_id)) priorTaskIds.add(run.task_id)
    }
    if (replacementAttempts.size > 1) {
      fail(
        failures,
        "B2D-PROGRESS-006",
        `Task ${task.index} ${workUnitKey} exceeds one logical replacement`
      )
    }
  }
}

function validateProgressTasks(snapshot, plan, failures) {
  if (!Array.isArray(snapshot.tasks)) {
    fail(failures, "B2D-PROGRESS-005", "progress tasks must be an array")
    return []
  }
  const orderedPlanIndexes = plan.tasks.map((task) => task.index)
  const planIndexes = new Set(orderedPlanIndexes)
  const seen = new Set()
  const tasks = []
  const taskIds = new Set()
  const childOwners = new Map()

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
      validateTaskRunLineages(task, failures, taskIds, childOwners)
    }
    tasks.push(task)
  }
  const progressIndexes = tasks.map((task) => task.index)
  if (progressIndexes.join(",") !== orderedPlanIndexes.join(",")) {
    fail(
      failures,
      "B2D-PROGRESS-005",
      "progress Task indices must exactly match the ordered Plan Task indices"
    )
  }
  return tasks
}

function validateSerialState(snapshot, tasks, failures) {
  const ordered = tasks
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

function expectedKeySet(expected) {
  return new Set(
    [
      expected?.implementer,
      expected?.reviewers?.primary,
      expected?.reviewers?.auxiliary,
    ].filter(nonEmptyString)
  )
}

/** Enforce routed Plan/progress agreement and Task Agent change boundaries. */
export function validateProgressRouting(snapshot, routing, failures) {
  if (!isObject(snapshot) || !routing || routing.tasks.length === 0) return
  if (!Array.isArray(snapshot.tasks)) {
    fail(failures, "B2D-PROGRESS-009", "routed progress requires Tasks")
    return
  }
  const progressByIndex = new Map(
    snapshot.tasks
      .filter((task) => isObject(task) && positiveInteger(task.index))
      .map((task) => [task.index, task])
  )
  for (const routeTask of routing.tasks) {
    const task = progressByIndex.get(routeTask.index)
    const expected = routeTask.expected_work_unit_keys
    if (!task) {
      fail(
        failures,
        "B2D-PROGRESS-009",
        `Task ${routeTask.index} is missing from routed progress`
      )
      continue
    }
    if (
      task.risk_level !== routeTask.risk.level ||
      task.task_agent_generation !== routeTask.task_agent_generation ||
      !skillContractsEqual(task.expected_work_unit_keys, expected)
    ) {
      fail(
        failures,
        "B2D-PROGRESS-009",
        `Task ${routeTask.index} route metadata disagrees with the Plan`
      )
    }
    const allowed = expectedKeySet(expected)
    const groups = new Map()
    if (Array.isArray(task.runs)) {
      for (const run of task.runs) {
        if (!isObject(run) || !nonEmptyString(run.work_unit_key)) continue
        if (!allowed.has(run.work_unit_key)) {
          fail(
            failures,
            "B2D-PROGRESS-009",
            `Task ${routeTask.index} run is outside its expected route`
          )
        }
        const entries = groups.get(run.work_unit_key) ?? []
        entries.push(run)
        groups.set(run.work_unit_key, entries)
      }
    }
    if (task.status === "completed") {
      for (const key of allowed) {
        const lineage = groups.get(key) ?? []
        const latest = lineage.at(-1)
        if (
          !latest ||
          latest.state !== "completed" ||
          !nonEmptyString(latest.task_id) ||
          !positiveInteger(latest.child_conversation_id) ||
          latest.child_conversation_id > MAX_I32
        ) {
          fail(
            failures,
            "B2D-PROGRESS-010",
            `completed Task ${routeTask.index} lacks an admitted completed lineage ${key}`
          )
        }
      }
    }
  }

  for (const generation of routing.generations.slice(1)) {
    const boundary = progressByIndex.get(generation.effective_from_task_index)
    const boundaryRoute = routing.tasks.find(
      (task) => task.index === generation.effective_from_task_index
    )
    const prior = routing.tasks
      .filter((task) => task.index < generation.effective_from_task_index)
      .map((task) => progressByIndex.get(task.index))
    const suffix = routing.tasks
      .filter((task) => task.index >= generation.effective_from_task_index)
      .map((task) => progressByIndex.get(task.index))
    const pendingSuffixIsClean = suffix.every(
      (task) =>
        task?.status === "pending" &&
        Array.isArray(task.runs) &&
        task.runs.length === 0
    )
    const priorCompleted = prior.every((task) => task?.status === "completed")
    const boundaryRuns = Array.isArray(boundary?.runs) ? boundary.runs : []
    const emptyPendingBoundary =
      boundary?.status === "pending" &&
      Array.isArray(boundary.runs) &&
      boundaryRuns.length === 0 &&
      pendingSuffixIsClean &&
      snapshot.active_task_index === null
    const frozenRouteMatches =
      boundary?.risk_level === boundaryRoute?.risk?.level &&
      boundary?.task_agent_generation ===
        boundaryRoute?.task_agent_generation &&
      skillContractsEqual(
        boundary?.expected_work_unit_keys,
        boundaryRoute?.expected_work_unit_keys
      )
    const implementerKey = boundaryRoute?.expected_work_unit_keys?.implementer
    const hasAdmittedImplementerRun = boundaryRuns.some(
      (run) =>
        isObject(run) &&
        run.work_unit_key === implementerKey &&
        nonEmptyString(run.task_id) &&
        positiveInteger(run.child_conversation_id) &&
        run.child_conversation_id <= MAX_I32
    )
    const historicalAdoptedBoundary =
      boundary?.status !== "pending" &&
      frozenRouteMatches &&
      hasAdmittedImplementerRun
    if (
      !boundary ||
      !priorCompleted ||
      (!emptyPendingBoundary && !historicalAdoptedBoundary)
    ) {
      fail(
        failures,
        "B2D-ROUTING-007",
        `generation ${generation.generation} is neither awaiting adoption across a clean pending suffix nor frozen on its admitted route`
      )
    }
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

  const extracted = extractUnfencedComment(
    source,
    PROGRESS_MARKER,
    MAX_PROGRESS_BLOCK_BYTES
  )
  if (extracted.markerCount !== 1 || extracted.problem === "truncated") {
    fail(
      failures,
      "B2D-PROGRESS-001",
      "progress document must contain exactly one marker; " +
        `found ${extracted.markerCount}`
    )
    return { ...progress, failures }
  }
  if (extracted.problem === "too_large") {
    fail(
      failures,
      "B2D-PROGRESS-002",
      "structured progress block exceeds the 64 KiB limit"
    )
    return { ...progress, failures }
  }
  const json = extracted.body.trim()

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
    fail(failures, "B2D-PROGRESS-003", "progress schema_version must equal 1")
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
    fail(failures, "B2D-PROGRESS-003", "updated_at must be a string or null")
  }

  const tasks = validateProgressTasks(snapshot, plan, failures)
  validateSerialState(snapshot, tasks, failures)
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
  const routingFailures = []
  if (
    !plan.routing &&
    !plan.failures.some((failure) => failure.startsWith("[B2D-ROUTING-"))
  ) {
    fail(
      routingFailures,
      "B2D-ROUTING-001",
      "authoritative document validation requires exactly one routing block"
    )
  }
  const routing = plan.routing
    ? validateRoutingSnapshot(plan.routing, plan, routingFailures)
    : null
  const progress = parseSimpleProgress(progressMarkdown, planRelPath, plan)
  const agreementFailures = []
  if (routing && progress.snapshot) {
    validateProgressRouting(progress.snapshot, routing, agreementFailures)
  }
  return {
    failures: [
      ...skill.failures,
      ...plan.failures,
      ...routingFailures,
      ...progress.failures,
      ...agreementFailures,
    ],
    notes: [
      ...skill.notes,
      `Plan Tasks parsed: ${plan.tasks.length}`,
      `Progress Tasks parsed: ${progress.snapshot?.tasks?.length ?? 0}`,
    ],
    plan,
    routing,
    progress,
  }
}
