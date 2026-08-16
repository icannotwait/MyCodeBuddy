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
const NAMED_AGENT_TERMS = new Set([
  "claude",
  "cline",
  "code-buddy",
  "codebuddy",
  "codex",
  "cursor",
  "gemini",
  "grok",
  "hermes",
  "kimi",
  "open-code",
  "opencode",
  "pi",
])
const TASK_AGENT_NAME_TERMS = new Set(
  [...NAMED_AGENT_TERMS].filter((name) => name !== "codex")
)
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
const BARE_PRODUCTION_ACTIONS = new Set([
  "author",
  "change",
  "create",
  "edit",
  "fix",
  "implement",
  "modify",
  "own",
  "patch",
  "produce",
  "revise",
  "update",
  "write",
])
const DIRECT_ROUTE_ACTIONS = new Set([
  "delegate",
  "delegated",
  "delegates",
  "delegating",
  "route",
  "routed",
  "routes",
  "routing",
])
const REVIEW_TERMS = new Set([
  "review",
  "reviewed",
  "reviewer",
  "reviewers",
  "reviewing",
  "reviews",
])
const ORCHESTRATION_ACTIONS = new Set([
  "ask",
  "asked",
  "asks",
  "asking",
  "direct",
  "directed",
  "directing",
  "directs",
  "dispatch",
  "dispatched",
  "dispatches",
  "dispatching",
  "instruct",
  "instructed",
  "instructing",
  "instructs",
  "send",
  "sending",
  "sends",
  "sent",
  "tell",
  "telling",
  "tells",
  "told",
])
const PRODUCER_DELEGATION_ACTIONS = new Set([
  ...ORCHESTRATION_ACTIONS,
  ...DIRECT_ROUTE_ACTIONS,
])
const FINITE_PARENT_PREDICATE_MARKERS = new Set([
  "can",
  "could",
  "did",
  "does",
  "itself",
  "may",
  "might",
  "must",
  "shall",
  "should",
  "will",
  "would",
])
const DOCUMENT_OR_CODE_TARGETS = new Set([
  "artifact",
  "artifacts",
  "code",
  "design",
  "designs",
  "document",
  "documents",
  "implementation",
  "plan",
  "plans",
  "task",
  "tasks",
])
const PLURAL_DOCUMENT_TARGETS = new Set([
  "artifacts",
  "designs",
  "documents",
  "plans",
  "tasks",
])
const PEOPLE_ANTECEDENT_TERMS = new Set([
  "authors",
  "developer",
  "developers",
  "fixers",
  "people",
  "producer",
  "producers",
  "reviewer",
  "reviewers",
])
const PLURAL_PEOPLE_ANTECEDENT_TERMS = new Set([
  "authors",
  "developers",
  "fixers",
  "people",
  "producers",
  "reviewers",
])
const DOCUMENT_PERSON_ROLE_TERMS = new Set([
  "author",
  "authors",
  "fixer",
  "fixers",
  "producer",
  "producers",
  "reviewer",
  "reviewers",
])
const ANTECEDENT_PREDICATES = new Set([
  ...PRODUCTION_ACTIONS,
  ...ORCHESTRATION_ACTIONS,
  "discuss",
  "discussed",
  "discusses",
  "discussing",
  "list",
  "listed",
  "listing",
  "lists",
])
const NEGATION_TERMS = new Set([
  "avoid",
  "avoided",
  "avoiding",
  "avoids",
  "cannot",
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
  "refuse",
  "refused",
  "refuses",
  "refusing",
  "without",
])
const REVIEW_ABSENCE_TERMS = new Set([
  "absent",
  "lack",
  "lacking",
  "lacks",
  "missing",
  "no",
  "omit",
  "omits",
  "omitted",
  "omitting",
  "without",
])
const POSTPOSED_REVIEW_ABSENCE_TERMS = new Set([
  "absent",
  "lacking",
  "missing",
  "omitted",
])
const ACTOR_LINKS = new Set(["by", "to"])
const NESTED_ACTOR_PREFIX_LINKS = new Set([
  "after",
  "before",
  "for",
  "from",
  "of",
  "through",
  "with",
])
const ACTOR_RELATION_BOUNDARIES = new Set([
  "after",
  "before",
  "during",
  "if",
  "once",
  "unless",
  "upon",
  "when",
  "while",
])
const CLAUSE_COORDINATORS = new Set(["and", "but", "while", "yet"])
const PREDICATE_COORDINATORS = new Set(["and", "but"])
const ALTERNATIVE_RESET_COORDINATORS = new Set(["but", "yet"])
const TASK_RELATION_BOUNDARIES = new Set([...CLAUSE_COORDINATORS, "or"])
const HIGH_TASK_SCOPES = new Set(["high", "high-risk"])
const NORMAL_TASK_SCOPES = new Set(["normal"])
const UNIVERSAL_TASK_SCOPES = new Set([
  "all",
  "always",
  "each",
  "every",
  "unconditionally",
])
const EXPLICIT_TASK_ACTIVITY_TERMS = new Set([
  "active",
  "in-progress",
  "running",
])
const TASK_COMPLETION_TERMS = new Set([
  "complete",
  "completed",
  "completes",
  "completion",
  "done",
  "finish",
  "finished",
  "finishes",
])
const TASK_COMPLETION_BRIDGE_TERMS = new Set([
  "already",
  "be",
  "been",
  "being",
  "can",
  "could",
  "had",
  "has",
  "have",
  "is",
  "may",
  "might",
  "must",
  "now",
  "was",
  "were",
  "will",
  "would",
])
const ACTIVE_TIMING_MARKERS = new Set(["during", "inside", "while"])
const BOUNDARY_TIMING_MARKERS = new Set([
  "after",
  "following",
  "on",
  "once",
  "upon",
  "when",
])
const PRE_COMPLETION_TIMING_MARKERS = new Set(["before", "prior"])
const TASK_REFERENCE_BOUNDARIES = new Set([
  ...CLAUSE_COORDINATORS,
  ...ACTIVE_TIMING_MARKERS,
  ...BOUNDARY_TIMING_MARKERS,
  ...PRE_COMPLETION_TIMING_MARKERS,
  "for",
  "from",
  "of",
  "through",
  "to",
  "until",
  "with",
])
const REVIEW_SUBJECT_BOUNDARIES = new Set([
  ...CLAUSE_COORDINATORS,
  ...BOUNDARY_TIMING_MARKERS,
  ...PRE_COMPLETION_TIMING_MARKERS,
  "if",
  "unless",
])
const REVIEW_BYPASS_ACTIONS = new Set([
  "instead",
  "omit",
  "omits",
  "omitted",
  "omitting",
  "optional",
  "optionally",
  "place",
  "replace",
  "replaced",
  "replaces",
  "replacing",
  "skip",
  "skipped",
  "skips",
  "skipping",
  "stand",
  "standing",
  "stands",
  "stood",
  "substitute",
  "substituted",
  "substitutes",
  "substituting",
])
const REVIEW_REPLACEMENT_ACTIONS = new Set([
  "place",
  "replace",
  "replaced",
  "replaces",
  "replacing",
  "stand",
  "standing",
  "stands",
  "stood",
  "substitute",
  "substituted",
  "substitutes",
  "substituting",
])
const REVIEW_TAKE_ACTIONS = new Set(["take", "takes", "taking", "took"])
const REVIEW_REQUIREMENT_ACTIONS = new Set(["mandatory", "required"])
const REVIEW_EXHAUSTIVE_QUANTIFIERS = new Set([
  "alone",
  "exclusively",
  "only",
  "solely",
])
const REVIEW_ACTOR_TAIL_TERMS = new Set([
  "a",
  "agent",
  "an",
  "auxiliary",
  "primary",
  "review",
  "reviewer",
  "reviewers",
  "role",
  "slot",
  "the",
])
const QUANTIFIER_COMPLEMENT_LINKS = new Set(["about", "for", "on", "regarding"])
const REVIEW_SUBJECT_LINKS = new Set([
  "although",
  "am",
  "are",
  "be",
  "been",
  "being",
  "is",
  "remain",
  "remains",
  "though",
  "was",
  "were",
])
const REFLEXIVE_TARGETS = new Set(["itself", "themselves"])
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
  for (const paragraph of withoutKeys
    .normalize("NFKC")
    .toLowerCase()
    .split(/\n\s*\n+/)) {
    let priorTokens = []
    for (const source of paragraph.split(/[.!?;]+/)) {
      const tokens = source.match(/[a-z0-9]+(?:-[a-z0-9]+)*/g) ?? []
      for (let start = 0; start < tokens.length; start += step) {
        windows.push({
          tokens: tokens.slice(start, start + DIRECTIVE_WINDOW_TOKENS),
          priorReviewers: directiveReviewers(priorTokens),
          priorTasks: directiveTaskAntecedents(priorTokens),
          priorDocumentTargets: directiveDocumentTargets(priorTokens),
          priorPronounAntecedent: directivePronounAntecedent(priorTokens),
        })
      }
      if (tokens.length > 0) {
        priorTokens = tokens.slice(-DIRECTIVE_WINDOW_TOKENS)
      }
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
  const coordinator = tokenIndexes(
    tokens,
    CLAUSE_COORDINATORS,
    Math.max(0, actionIndex - 6),
    actionIndex
  ).at(-1)
  const prefix = tokens.slice(
    Math.max(0, actionIndex - 6, (coordinator ?? -1) + 1),
    actionIndex
  )
  return (
    prefix.some((token) => NEGATION_TERMS.has(token)) ||
    prefix.at(-1) === "from"
  )
}

function agentActorEnd(tokens, baseEnd) {
  if (tokens[baseEnd] === "task" && tokens[baseEnd + 1] === "agent") {
    return baseEnd + 2
  }
  return tokens[baseEnd] === "agent" ? baseEnd + 1 : baseEnd
}

function directiveActors(tokens) {
  const actors = []
  for (const [index, token] of tokens.entries()) {
    if (actors.some((actor) => actor.start <= index && index < actor.end)) {
      continue
    }
    if (token === "parent") {
      actors.push({ role: "parent", start: index, end: index + 1 })
    } else if (token === "codex") {
      const isTaskAgent =
        tokens[index + 1] === "task" && tokens[index + 2] === "agent"
      const end = isTaskAgent
        ? index + 3
        : tokens[index + 1] === "agent"
          ? index + 2
          : index + 1
      actors.push({
        role: isTaskAgent ? "task_agent" : "codex",
        start: index,
        end,
      })
    } else if (token === "task" && tokens[index + 1] === "agent") {
      actors.push({ role: "task_agent", start: index, end: index + 2 })
    } else if (token === "plan" && tokens[index + 1] === "author") {
      actors.push({ role: "plan_author", start: index, end: index + 2 })
    } else if (token === "design" && tokens[index + 1] === "fixer") {
      actors.push({ role: "design_fixer", start: index, end: index + 2 })
    } else if (token === "custom") {
      const hasNamedId =
        tokens[index + 1] !== undefined && tokens[index + 1] !== "agent"
      const baseEnd = index + (hasNamedId ? 2 : 1)
      actors.push({
        role: "task_agent",
        start: index,
        end: agentActorEnd(tokens, baseEnd),
      })
    } else if (
      TASK_AGENT_NAME_TERMS.has(token) &&
      tokens[index - 1] !== "custom"
    ) {
      const baseEnd =
        ["claude", "kimi"].includes(token) && tokens[index + 1] === "code"
          ? index + 2
          : index + 1
      actors.push({
        role: "task_agent",
        start: index,
        end: agentActorEnd(tokens, baseEnd),
      })
    } else if (
      (token === "code" && tokens[index + 1] === "buddy") ||
      (token === "open" && tokens[index + 1] === "code")
    ) {
      actors.push({
        role: "task_agent",
        start: index,
        end: agentActorEnd(tokens, index + 2),
      })
    }
  }
  return actors
}

function directiveActions(tokens, actors) {
  const roleTokens = new Set()
  for (const actor of actors) {
    if (actor.role !== "plan_author") continue
    for (let index = actor.start; index < actor.end; index += 1) {
      roleTokens.add(index)
    }
  }
  const actions = []
  for (const [index, token] of tokens.entries()) {
    if (REVIEW_TERMS.has(token)) {
      actions.push({ index, kind: "review", token })
    } else if (DIRECT_ROUTE_ACTIONS.has(token)) {
      actions.push({ index, kind: "route", token })
    } else if (PRODUCTION_ACTIONS.has(token) && !roleTokens.has(index)) {
      actions.push({ index, kind: "production", token })
    }
  }
  return actions
}

function taskScopes(tokens, taskIndex) {
  const start = Math.max(0, taskIndex - 3)
  const end = Math.min(tokens.length, taskIndex + 2)
  const scopes = new Set()
  if (tokenIndex(tokens, HIGH_TASK_SCOPES, start, end) >= 0) scopes.add("high")
  if (tokenIndex(tokens, NORMAL_TASK_SCOPES, start, end) >= 0) {
    scopes.add("normal")
  }
  if (
    scopes.size === 0 &&
    tokenIndex(tokens, UNIVERSAL_TASK_SCOPES, start, end) >= 0
  ) {
    scopes.add("universal")
  }
  if (scopes.size === 0) scopes.add("unspecified")
  return scopes
}

function directiveTasks(tokens) {
  const tasks = []
  for (const [index, token] of tokens.entries()) {
    if (
      !["task", "tasks"].includes(token) ||
      (token === "task" && tokens[index + 1] === "agent")
    ) {
      continue
    }
    tasks.push({ index, scopes: taskScopes(tokens, index) })
  }
  return tasks
}

function directiveTaskAntecedents(tokens) {
  const clause = { tokens }
  return directiveTasks(tokens).map((task) => ({
    ...task,
    active: taskHasAffirmativeActivity(clause, task),
    completed: taskHasCompletedState(clause, task),
  }))
}

function directiveDocumentTargets(tokens) {
  const actors = directiveActors(tokens)
  return tokens.flatMap((token, index) =>
    DOCUMENT_OR_CODE_TARGETS.has(token) &&
    !actors.some((actor) => actor.start <= index && index < actor.end) &&
    !DOCUMENT_PERSON_ROLE_TERMS.has(tokens[index + 1])
      ? [{ index, token }]
      : []
  )
}

function directivePeopleAntecedents(tokens) {
  const actors = directiveActors(tokens).map((actor) => ({
    start: actor.start,
    end: actor.end,
    plural: false,
  }))
  const people = tokens.flatMap((token, index) =>
    PEOPLE_ANTECEDENT_TERMS.has(token) &&
    !actors.some((actor) => actor.start <= index && index < actor.end)
      ? [
          {
            start: index,
            end: index + 1,
            plural: PLURAL_PEOPLE_ANTECEDENT_TERMS.has(token),
          },
        ]
      : []
  )
  return [...actors, ...people].sort((left, right) => left.start - right.start)
}

function mentionsPluralGroup(tokens, mentions) {
  if (mentions.some((mention) => mention.plural)) return true
  if (mentions.length < 2) return false
  return (
    tokenIndex(
      tokens,
      new Set(["and", "or"]),
      mentions[0].end,
      mentions.at(-1).start
    ) >= 0
  )
}

function directivePronounAntecedent(tokens) {
  const documents = directiveDocumentTargets(tokens)
  const people = directivePeopleAntecedents(tokens)
  const actors = directiveActors(tokens)
  const predicate = tokenIndexes(tokens, ANTECEDENT_PREDICATES)
    .filter(
      (index) =>
        !actors.some((actor) => actor.start <= index && index < actor.end)
    )
    .at(-1)
  const documentMentions = documents.map((target) => ({
    start: target.index,
    end: target.index + 1,
    plural: PLURAL_DOCUMENT_TARGETS.has(target.token),
  }))
  const documentObjects =
    predicate === undefined
      ? documentMentions
      : documentMentions.filter((target) => target.start > predicate)
  const peopleSubjects =
    predicate === undefined
      ? people
      : people.filter((person) => person.end <= predicate)
  const peopleRecipients =
    predicate === undefined
      ? []
      : people.filter(
          (person) =>
            person.start > predicate &&
            tokenIndex(
              tokens,
              new Set(["by", "to"]),
              predicate + 1,
              person.start
            ) >= 0
        )

  const pluralDocuments = mentionsPluralGroup(tokens, documentObjects)
  const pluralPeople =
    mentionsPluralGroup(tokens, peopleSubjects) ||
    mentionsPluralGroup(tokens, peopleRecipients)
  if (pluralPeople) return "people"
  if (pluralDocuments) return "document"
  return null
}

function directiveReviewers(tokens) {
  const reviewers = []
  for (const index of tokenIndexes(tokens, REVIEW_TERMS)) {
    const start = Math.max(0, index - 4)
    const prefix = tokens.slice(start, index + 1)
    const userNamed =
      prefix.includes("user-named") ||
      phraseIndex(tokens, ["user", "named"], start, index + 1) >= 0
    reviewers.push({
      index,
      primary: prefix.includes("primary"),
      auxiliary: prefix.includes("auxiliary"),
      codex: prefix.includes("codex"),
      required: prefix.includes("mandatory") || prefix.includes("required"),
      optionalDocument:
        userNamed && (prefix.includes("design") || prefix.includes("plan")),
    })
  }
  return reviewers
}

function parseDirectiveClause(window) {
  const {
    tokens,
    priorReviewers = [],
    priorTasks = [],
    priorDocumentTargets = [],
    priorPronounAntecedent = null,
  } = window
  const actors = directiveActors(tokens)
  return {
    tokens,
    actors,
    actions: directiveActions(tokens, actors),
    tasks: directiveTasks(tokens),
    reviewers: directiveReviewers(tokens),
    priorReviewers,
    priorTasks,
    priorDocumentTargets,
    priorPronounAntecedent,
  }
}

function actionSegment(clause, actionIndex) {
  const coordinators = tokenIndexes(clause.tokens, CLAUSE_COORDINATORS)
  const before = coordinators.filter((index) => index < actionIndex).at(-1)
  const after = coordinators.find((index) => index > actionIndex)
  return {
    start: (before ?? -1) + 1,
    end: after ?? clause.tokens.length,
  }
}

function actionsSharePredicateRelations(clause, left, right) {
  return (
    tokenIndex(
      clause.tokens,
      PREDICATE_COORDINATORS,
      left.index + 1,
      right.index
    ) >= 0 &&
    !clause.actors.some(
      (actor) => actor.start > left.index && actor.end <= right.index
    )
  )
}

function actionsShareTaskScope(clause, left, right) {
  return (
    tokenIndex(
      clause.tokens,
      PREDICATE_COORDINATORS,
      left.index + 1,
      right.index
    ) >= 0
  )
}

function relatedActionGroup(clause, action, sharesRelations) {
  const position = clause.actions.findIndex(
    (candidate) => candidate.index === action.index
  )
  if (position < 0) return [action]
  let start = position
  let end = position
  while (
    start > 0 &&
    sharesRelations(clause, clause.actions[start - 1], clause.actions[start])
  ) {
    start -= 1
  }
  while (
    end + 1 < clause.actions.length &&
    sharesRelations(clause, clause.actions[end], clause.actions[end + 1])
  ) {
    end += 1
  }
  return clause.actions.slice(start, end + 1)
}

function predicateActionGroup(clause, action) {
  return relatedActionGroup(clause, action, actionsSharePredicateRelations)
}

function taskScopeActionGroup(clause, action) {
  return relatedActionGroup(clause, action, actionsShareTaskScope)
}

function localTasksForAction(clause, action) {
  const position = clause.actions.findIndex(
    (candidate) => candidate.index === action.index
  )
  const previous = position > 0 ? clause.actions[position - 1] : null
  const next =
    position + 1 < clause.actions.length ? clause.actions[position + 1] : null
  const before = tokenIndexes(
    clause.tokens,
    TASK_RELATION_BOUNDARIES,
    previous?.index ?? 0,
    action.index
  )
    .filter(
      (boundary) =>
        Boolean(previous && previous.index < boundary) ||
        clause.actors.some(
          (actor) => actor.start > boundary && actor.end <= action.index
        )
    )
    .at(-1)
  const after = next
    ? tokenIndex(
        clause.tokens,
        TASK_RELATION_BOUNDARIES,
        action.index + 1,
        next.index
      )
    : -1
  const tasks = clause.tasks.filter(
    (task) =>
      task.index >= (before ?? -1) + 1 &&
      task.index < (after >= 0 ? after : clause.tokens.length)
  )
  const followingPrevious = previous
    ? tasks.filter((task) => task.index > previous.index)
    : []
  return followingPrevious.length > 0 ? followingPrevious : tasks
}

function scopesForTasks(tasks) {
  const scopes = new Set(tasks.flatMap((task) => [...task.scopes]))
  if (scopes.size === 0) scopes.add("unspecified")
  return scopes
}

function actionTaskScopes(clause, action) {
  const segment = actionSegment(clause, action.index)
  let tasks = localTasksForAction(clause, action)
  if (
    tasks.length === 0 &&
    clause.tokens.slice(action.index + 1, segment.end).includes("them")
  ) {
    const previous = clause.tasks
      .filter((task) => task.index < segment.start)
      .at(-1)
    if (previous) tasks = [previous]
  }
  if (
    tasks.length === 0 &&
    clause.priorTasks.length > 0 &&
    (action.token === "implementation" ||
      tokenIndex(
        clause.tokens,
        new Set(["they", "them"]),
        segment.start,
        segment.end
      ) >= 0)
  ) {
    tasks = clause.priorTasks
  }
  if (tasks.length === 0) {
    tasks = taskScopeActionGroup(clause, action).flatMap((candidate) =>
      localTasksForAction(clause, candidate)
    )
  }
  const scopes = scopesForTasks(tasks)
  if (!scopes.has("unspecified")) return scopes
  if (
    action.token === "implementer" &&
    tokenIndex(clause.tokens, UNIVERSAL_TASK_SCOPES) >= 0
  ) {
    return new Set(["universal"])
  }
  return scopes
}

function nearestActionBefore(clause, index) {
  return clause.actions.filter((action) => action.index < index).at(-1)
}

function actorRelationPrefixIsValid(prefix) {
  return (
    prefix.length <= 6 &&
    !prefix.some(
      (token) =>
        CLAUSE_COORDINATORS.has(token) ||
        ACTOR_LINKS.has(token) ||
        PRODUCTION_ACTIONS.has(token) ||
        DIRECT_ROUTE_ACTIONS.has(token) ||
        REVIEW_TERMS.has(token) ||
        NESTED_ACTOR_PREFIX_LINKS.has(token) ||
        ["task", "tasks"].includes(token)
    )
  )
}

function actorsAfterLink(clause, link, end = clause.tokens.length) {
  const nextLink = tokenIndex(clause.tokens, ACTOR_LINKS, link + 1, end)
  let relationEnd = nextLink >= 0 ? nextLink : end
  const boundary = tokenIndex(
    clause.tokens,
    ACTOR_RELATION_BOUNDARIES,
    link + 1,
    relationEnd
  )
  if (boundary >= 0) relationEnd = boundary
  const actors = clause.actors.filter(
    (actor) => actor.start > link && actor.end <= relationEnd
  )
  if (actors.length === 0) return []
  const prefix = clause.tokens.slice(link + 1, actors[0].start)
  if (!actorRelationPrefixIsValid(prefix)) return []
  return actors
}

function alternativeExclusionStart(clause, action, end) {
  const starts = [
    phraseIndex(clause.tokens, ["rather", "than"], action.index + 1, end),
    phraseIndex(clause.tokens, ["instead", "of"], action.index + 1, end),
  ].filter((index) => index >= 0)
  return starts.length > 0 ? Math.min(...starts) : -1
}

function positionIsExcludedAlternative(clause, alternativeStart, position) {
  if (alternativeStart < 0 || position <= alternativeStart) return false
  return (
    tokenIndex(
      clause.tokens,
      ALTERNATIVE_RESET_COORDINATORS,
      alternativeStart + 2,
      position
    ) < 0
  )
}

function actionIsExcludedAlternative(clause, action) {
  const previous = clause.actions
    .filter((candidate) => candidate.index < action.index)
    .at(-1)
  const start = (previous?.index ?? -1) + 1
  const starts = [
    phraseIndex(clause.tokens, ["rather", "than"], start, action.index + 1),
    phraseIndex(clause.tokens, ["instead", "of"], start, action.index + 1),
  ].filter((index) => index >= 0)
  if (starts.length === 0) return false
  return positionIsExcludedAlternative(
    clause,
    Math.max(...starts),
    action.index
  )
}

function actorBindingsAfterLink(clause, action, link, end) {
  const actors = actorsAfterLink(clause, link, end)
  const relationNegated = relationTargetIsNegated(clause, action, link)
  const alternativeStart = alternativeExclusionStart(clause, action, end)
  return actors.map((actor, index) => {
    const localStart = index === 0 ? link + 1 : actors[index - 1].end
    const coordinator = tokenIndexes(
      clause.tokens,
      CLAUSE_COORDINATORS,
      localStart,
      actor.start
    ).at(-1)
    const negationStart = (coordinator ?? localStart - 1) + 1
    const explicitlyNegated =
      tokenIndex(clause.tokens, NEGATION_TERMS, negationStart, actor.start) >= 0
    const excludedAlternative = positionIsExcludedAlternative(
      clause,
      alternativeStart,
      actor.start
    )
    return {
      actor,
      negated:
        explicitlyNegated ||
        excludedAlternative ||
        (coordinator !== undefined && clause.tokens[coordinator] === "but"
          ? false
          : relationNegated),
    }
  })
}

function directPassiveActorsForAction(clause, action) {
  const nextAction = clause.actions.find(
    (candidate) => candidate.index > action.index
  )
  const actors = []
  const links = tokenIndexes(
    clause.tokens,
    new Set(["by"]),
    action.index + 1,
    nextAction?.index ?? clause.tokens.length
  )
  for (const [linkIndex, link] of links.entries()) {
    if (linkIndex > 0) {
      const previousLink = links[linkIndex - 1]
      const previousActor = clause.actors
        .filter((actor) => actor.start > previousLink && actor.end <= link)
        .at(-1)
      const relationStart = previousActor?.end ?? previousLink + 1
      const continuesRelation =
        tokenIndex(
          clause.tokens,
          new Set(["and", "but", "or", "yet"]),
          relationStart,
          link
        ) >= 0 ||
        phraseIndex(clause.tokens, ["rather", "than"], relationStart, link) >=
          0 ||
        phraseIndex(clause.tokens, ["instead", "of"], relationStart, link) >= 0
      if (!continuesRelation) continue
    }
    actors.push(
      ...actorBindingsAfterLink(
        clause,
        action,
        link,
        nextAction?.index ?? clause.tokens.length
      )
        .filter((binding) => !binding.negated)
        .map((binding) => binding.actor)
    )
  }
  return actors
}

function uniqueActors(actors) {
  return actors.filter(
    (actor, index) =>
      actors.findIndex(
        (candidate) =>
          candidate.role === actor.role && candidate.start === actor.start
      ) === index
  )
}

function passiveActorsForAction(clause, action) {
  const direct = directPassiveActorsForAction(clause, action)
  if (direct.length > 0) return direct
  if (actionIsNegated(clause.tokens, action.index)) return []
  const group = predicateActionGroup(clause, action)
  if (group.length === 1) return []
  const last = group.at(-1)
  const nextAction = clause.actions.find(
    (candidate) => candidate.index > last.index
  )
  return uniqueActors(
    tokenIndexes(
      clause.tokens,
      new Set(["by"]),
      last.index + 1,
      nextAction?.index ?? clause.tokens.length
    ).flatMap((link) =>
      actorsAfterLink(clause, link, nextAction?.index ?? clause.tokens.length)
    )
  )
}

function subjectActorsForAction(clause, action) {
  const passive = passiveActorsForAction(clause, action)
  if (passive.length > 0) return passive

  const preceding = clause.actors.filter((actor) => actor.end <= action.index)
  if (preceding.length === 0) return []
  const subjects = [preceding.at(-1)]
  for (let index = preceding.length - 2; index >= 0; index -= 1) {
    const previous = preceding[index]
    const current = subjects[0]
    const coordinated =
      tokenIndex(
        clause.tokens,
        new Set(["and"]),
        previous.end,
        current.start
      ) >= 0
    const linked =
      tokenIndex(clause.tokens, ACTOR_LINKS, previous.end, current.start) >= 0
    const interveningAction = clause.actions.some(
      (candidate) =>
        candidate.index >= previous.end && candidate.index < current.start
    )
    if (!coordinated || linked || interveningAction) break
    subjects.unshift(previous)
  }
  return subjects
}

function linkBeforeActor(clause, actor) {
  return tokenIndexes(
    clause.tokens,
    ACTOR_LINKS,
    Math.max(0, actor.start - 4),
    actor.start
  ).at(-1)
}

function relationTargetIsNegated(clause, action, link) {
  const coordinators = tokenIndexes(
    clause.tokens,
    CLAUSE_COORDINATORS,
    action.index + 1,
    link
  )
  const coordinator = coordinators.at(-1)
  const localStart = (coordinator ?? action.index) + 1
  if (
    tokenIndex(
      clause.tokens,
      NEGATION_TERMS,
      Math.max(localStart, link - 4),
      link
    ) >= 0
  ) {
    return true
  }
  if (coordinator !== undefined && clause.tokens[coordinator] === "but") {
    return false
  }
  return actionIsNegated(clause.tokens, action.index)
}

function actionHasDocumentTarget(clause, action) {
  const segment = actionSegment(clause, action.index)
  const documentTargets = directiveDocumentTargets(clause.tokens)
  if (
    documentTargets.some(
      (target) => target.index >= segment.start && target.index < segment.end
    )
  ) {
    return true
  }
  const hasDocumentAntecedent =
    clause.priorDocumentTargets.length > 0 ||
    documentTargets.some((target) => target.index < segment.start)
  if (!hasDocumentAntecedent) return false
  if (
    tokenIndex(clause.tokens, new Set(["it"]), segment.start, segment.end) >=
      0 ||
    phraseIndex(
      clause.tokens,
      ["that", "document"],
      segment.start,
      segment.end
    ) >= 0 ||
    phraseIndex(
      clause.tokens,
      ["that", "artifact"],
      segment.start,
      segment.end
    ) >= 0 ||
    phraseIndex(
      clause.tokens,
      ["the", "document"],
      segment.start,
      segment.end
    ) >= 0 ||
    phraseIndex(
      clause.tokens,
      ["the", "artifact"],
      segment.start,
      segment.end
    ) >= 0
  ) {
    return true
  }
  if (
    clause.priorPronounAntecedent === "document" &&
    tokenIndex(clause.tokens, new Set(["them"]), segment.start, segment.end) >=
      0
  ) {
    return true
  }

  const trailing = clause.tokens.slice(action.index + 1, segment.end)
  return trailing.every((token) =>
    new Set(["again", "afterward", "directly", "too"]).has(token)
  )
}

function reviewPurposeInRange(clause, start, end) {
  if (tokenIndex(clause.tokens, REVIEW_TERMS, start, end) < 0) {
    return null
  }
  if (tokenIndex(clause.tokens, new Set(["primary"]), start, end) >= 0) {
    return "primary"
  }
  if (tokenIndex(clause.tokens, new Set(["auxiliary"]), start, end) >= 0) {
    return "auxiliary"
  }
  return "review"
}

function routeReviewPurpose(clause, action) {
  const segment = actionSegment(clause, action.index)
  return reviewPurposeInRange(clause, segment.start, segment.end)
}

function singleReviewSlot(clause, start, end) {
  const slots = ["primary", "auxiliary"].filter(
    (slot) => tokenIndex(clause.tokens, new Set([slot]), start, end) >= 0
  )
  return slots.length === 1 ? slots[0] : null
}

function relationBindingsAfterLink(clause, action, link, end) {
  const nextLink = tokenIndex(clause.tokens, ACTOR_LINKS, link + 1, end)
  const relationEnd = nextLink >= 0 ? nextLink : end
  const allActors = actorsAfterLink(clause, link, end)
  const actors = actorBindingsAfterLink(clause, action, link, end)
    .filter((binding) => !binding.negated)
    .map((binding) => binding.actor)
  const coordinator = tokenIndexes(
    clause.tokens,
    CLAUSE_COORDINATORS,
    action.index + 1,
    link
  ).at(-1)
  const prefixSlot = singleReviewSlot(
    clause,
    (coordinator ?? action.index) + 1,
    link
  )
  const relationPurpose = reviewPurposeInRange(
    clause,
    (coordinator ?? action.index) + 1,
    relationEnd
  )
  return actors.map((actor) => {
    const actorIndex = allActors.findIndex(
      (candidate) => candidate.start === actor.start
    )
    const previousActor = allActors[actorIndex - 1]
    const nextActor = allActors[actorIndex + 1]
    const actorCoordinator = previousActor
      ? tokenIndexes(
          clause.tokens,
          CLAUSE_COORDINATORS,
          previousActor.end,
          actor.start
        ).at(-1)
      : undefined
    const nextActorCoordinator = nextActor
      ? tokenIndexes(
          clause.tokens,
          CLAUSE_COORDINATORS,
          actor.end,
          nextActor.start
        ).at(-1)
      : undefined
    let preStart =
      actorIndex === 0
        ? link + 1
        : (actorCoordinator ?? previousActor.end - 1) + 1
    if (previousActor && actorCoordinator === undefined) {
      const previousRoleDescriptor = tokenIndex(
        clause.tokens,
        new Set([
          "implementation",
          "implementer",
          "review",
          "reviewer",
          "reviewers",
        ]),
        previousActor.end,
        actor.start
      )
      if (previousRoleDescriptor >= 0) preStart = previousRoleDescriptor + 1
    }
    const postEnd = nextActorCoordinator ?? nextActor?.start ?? relationEnd
    const slot =
      prefixSlot ??
      singleReviewSlot(clause, preStart, actor.start) ??
      singleReviewSlot(clause, actor.end, postEnd)
    const localPurpose =
      reviewPurposeInRange(clause, preStart, actor.start) ??
      reviewPurposeInRange(clause, actor.end, postEnd)
    const explicitImplementation =
      tokenIndex(
        clause.tokens,
        new Set(["implementation", "implementer"]),
        preStart,
        actor.start
      ) >= 0 ||
      tokenIndex(
        clause.tokens,
        new Set(["implementation", "implementer"]),
        actor.end,
        postEnd
      ) >= 0
    return {
      actor,
      implementationExplicit: explicitImplementation,
      purpose: explicitImplementation
        ? null
        : (slot ?? localPurpose ?? relationPurpose),
      slot: explicitImplementation ? null : slot,
    }
  })
}

function routeTargetBindingsForAction(clause, action) {
  const nextRoute = clause.actions.find(
    (candidate) => candidate.kind === "route" && candidate.index > action.index
  )
  const end = nextRoute?.index ?? clause.tokens.length
  const bindings = tokenIndexes(
    clause.tokens,
    new Set(["to"]),
    action.index + 1,
    end
  ).flatMap((link) => relationBindingsAfterLink(clause, action, link, end))
  const reflexive = tokenIndexes(
    clause.tokens,
    REFLEXIVE_TARGETS,
    action.index + 1,
    end
  )
  if (reflexive.length > 0 && !actionIsNegated(clause.tokens, action.index)) {
    bindings.push(
      ...subjectActorsForAction(clause, action).map((actor) => ({
        actor,
        implementationExplicit: false,
        purpose: routeReviewPurpose(clause, action),
        slot: null,
      }))
    )
  }
  return bindings.filter(
    (binding, index) =>
      bindings.findIndex(
        (candidate) =>
          candidate.actor.role === binding.actor.role &&
          candidate.actor.start === binding.actor.start
      ) === index
  )
}

function actionDelegatesToProducer(clause, parent, action) {
  return clause.actors.some((producer) => {
    if (
      !["design_fixer", "plan_author"].includes(producer.role) ||
      producer.start <= parent.end ||
      producer.end > action.index
    ) {
      return false
    }
    const infinitive = tokenIndex(
      clause.tokens,
      new Set(["to"]),
      producer.end,
      action.index + 1
    )
    if (infinitive < 0) return false
    if (
      tokenIndex(
        clause.tokens,
        PRODUCER_DELEGATION_ACTIONS,
        parent.end,
        producer.start
      ) < 0
    ) {
      return false
    }

    const delegatedActions = clause.actions.filter(
      (candidate) =>
        candidate.kind === "production" &&
        candidate.index > infinitive &&
        candidate.index <= action.index
    )
    if (delegatedActions.length === 0) return false
    if (delegatedActions[0].index === action.index) return true

    return delegatedActions.slice(1).every((candidate, index) => {
      const previous = delegatedActions[index]
      const relationTokens = clause.tokens.slice(
        previous.index + 1,
        candidate.index
      )
      const hasCoordination =
        relationTokens.length === 0 ||
        relationTokens.some((token) => ["and", "or"].includes(token))
      return (
        BARE_PRODUCTION_ACTIONS.has(candidate.token) &&
        hasCoordination &&
        !relationTokens.some((token) =>
          FINITE_PARENT_PREDICATE_MARKERS.has(token)
        ) &&
        !relationTokens.some((token) => ["but", "then"].includes(token)) &&
        !clause.actors.some(
          (actor) =>
            actor.start > previous.index && actor.end <= candidate.index
        )
      )
    })
  })
}

function actionIsPassivelyDelegatedToProducer(clause, parent, action) {
  return clause.actors.some((producer) => {
    if (
      !["design_fixer", "plan_author"].includes(producer.role) ||
      producer.end > parent.start ||
      parent.end > action.index
    ) {
      return false
    }
    const passiveLink = tokenIndex(
      clause.tokens,
      new Set(["by"]),
      producer.end,
      parent.start
    )
    const orchestration = tokenIndex(
      clause.tokens,
      ORCHESTRATION_ACTIONS,
      producer.end,
      parent.start
    )
    const infinitive = tokenIndex(
      clause.tokens,
      new Set(["to"]),
      parent.end,
      action.index
    )
    const repeatedParent = clause.actors.some(
      (actor) =>
        actor.role === "parent" &&
        actor.start > parent.end &&
        actor.end <= action.index
    )
    return (
      passiveLink >= 0 &&
      orchestration >= 0 &&
      infinitive >= 0 &&
      !repeatedParent
    )
  })
}

function repeatedProducerSubjectForAction(clause, action) {
  const previous = clause.actions
    .filter(
      (candidate) =>
        candidate.kind === "production" && candidate.index < action.index
    )
    .at(-1)
  if (!previous) return null
  return clause.actors.find(
    (actor) =>
      ["design_fixer", "plan_author"].includes(actor.role) &&
      actor.start > previous.index &&
      actor.end <= action.index &&
      tokenIndex(
        clause.tokens,
        ACTOR_LINKS,
        Math.max(previous.index + 1, actor.start - 2),
        actor.start
      ) < 0
  )
}

function reviewActorBindingsForAction(clause, action) {
  if (["reviewer", "reviewers"].includes(action.token)) {
    const describedActor = clause.actors
      .filter((actor) => actor.end === action.index)
      .at(-1)
    if (describedActor) {
      return [
        {
          actor: describedActor,
          slot: singleReviewSlot(
            clause,
            Math.max(0, describedActor.start - 4),
            action.index
          ),
        },
      ]
    }
  }
  const nextAction = clause.actions.find(
    (candidate) =>
      candidate.index > action.index &&
      !(
        candidate.kind === "review" &&
        ["reviewer", "reviewers"].includes(candidate.token)
      )
  )
  const end = nextAction?.index ?? clause.tokens.length
  const passive = tokenIndexes(
    clause.tokens,
    new Set(["by"]),
    action.index + 1,
    end
  ).flatMap((link) => relationBindingsAfterLink(clause, action, link, end))
  if (passive.length > 0) return passive

  const actors = subjectActorsForAction(clause, action)
  return actors.map((actor, index) => ({
    actor,
    slot: singleReviewSlot(
      clause,
      actor.end,
      actors[index + 1]?.start ?? action.index
    ),
  }))
}

function statementIsNegatedBeforeAction(clause, action) {
  const segment = actionSegment(clause, action.index)
  return (
    tokenIndex(clause.tokens, NEGATION_TERMS, segment.start, action.index) >= 0
  )
}

function reviewSlotForAction(clause, action) {
  const segment = actionSegment(clause, action.index)
  return singleReviewSlot(clause, segment.start, segment.end)
}

function relationIsExplicitlyAbsent(clause, start, segmentStart) {
  const absence = tokenIndexes(
    clause.tokens,
    REVIEW_ABSENCE_TERMS,
    Math.max(segmentStart, start - 5),
    start
  ).at(-1)
  if (
    absence === undefined ||
    tokenIndex(clause.tokens, new Set(["other"]), absence + 1, start) >= 0
  ) {
    return false
  }
  const coordinator = tokenIndexes(
    clause.tokens,
    CLAUSE_COORDINATORS,
    segmentStart,
    absence
  ).at(-1)
  return (
    tokenIndex(
      clause.tokens,
      NEGATION_TERMS,
      (coordinator ?? segmentStart - 1) + 1,
      absence
    ) < 0
  )
}

function reviewerSlotIsExplicitlyAbsent(clause, action, slotName) {
  const segment = actionSegment(clause, action.index)
  return tokenIndexes(
    clause.tokens,
    new Set([slotName]),
    segment.start,
    segment.end
  ).some((slot) => relationIsExplicitlyAbsent(clause, slot, segment.start))
}

function reviewerRoleIsExplicitlyAbsent(clause, action, role) {
  const segment = actionSegment(clause, action.index)
  const nextAction = clause.actions.find(
    (candidate) => candidate.index > action.index
  )
  const relationEnd = nextAction?.index ?? clause.tokens.length
  return clause.actors
    .filter(
      (actor) =>
        actor.role === role &&
        actor.start >= segment.start &&
        actor.end <= relationEnd
    )
    .some((actor) => {
      if (relationIsExplicitlyAbsent(clause, actor.start, segment.start)) {
        return true
      }
      const negation = tokenIndexes(
        clause.tokens,
        NEGATION_TERMS,
        Math.max(segment.start, actor.start - 5),
        actor.start
      ).at(-1)
      if (
        negation !== undefined &&
        tokenIndex(
          clause.tokens,
          new Set(["alone", "only"]),
          negation + 1,
          relationEnd
        ) < 0 &&
        tokenIndex(
          clause.tokens,
          REVIEW_ABSENCE_TERMS,
          negation + 1,
          actor.start
        ) < 0
      ) {
        return true
      }
      return tokenIndexes(
        clause.tokens,
        POSTPOSED_REVIEW_ABSENCE_TERMS,
        actor.end,
        Math.min(relationEnd, actor.end + 6)
      ).some((absence) => {
        if (actionIsNegated(clause.tokens, absence)) return false
        const subjectLink = tokenIndex(
          clause.tokens,
          REVIEW_SUBJECT_LINKS,
          actor.end,
          absence
        )
        if (subjectLink < 0) return false
        if (
          tokenIndex(
            clause.tokens,
            REVIEW_SUBJECT_BOUNDARIES,
            actor.end,
            subjectLink
          ) >= 0
        ) {
          return false
        }
        if (
          tokenIndex(clause.tokens, CLAUSE_COORDINATORS, actor.end, absence) >=
          0
        ) {
          return false
        }
        return !clause.actors.some(
          (candidate) =>
            candidate.start >= actor.end && candidate.end <= absence
        )
      })
    })
}

function reviewerSetIsExplicitlyEmpty(clause, action) {
  const segment = actionSegment(clause, action.index)
  const absence = tokenIndexes(
    clause.tokens,
    REVIEW_ABSENCE_TERMS,
    Math.max(segment.start, action.index - 5),
    action.index
  ).at(-1)
  if (absence === undefined) return false
  const qualified =
    tokenIndex(
      clause.tokens,
      new Set([
        "agent",
        "another",
        "auxiliary",
        "codex",
        "more",
        "other",
        "primary",
        "task",
      ]),
      absence + 1,
      action.index
    ) >= 0
  return (
    !qualified &&
    relationIsExplicitlyAbsent(clause, action.index, segment.start)
  )
}

function explicitReviewerCardinality(clause, action) {
  const counts = new Map([
    ["single", 1],
    ["sole", 1],
    ["one", 1],
    ["1", 1],
    ["first", 1],
    ["two", 2],
    ["2", 2],
    ["second", 2],
    ["three", 3],
    ["3", 3],
    ["third", 3],
  ])
  const start = Math.max(
    actionSegment(clause, action.index).start,
    action.index - 6
  )
  const extraMarkers = tokenIndexes(
    clause.tokens,
    new Set(["additional", "another", "extra", "further", "surplus"]),
    start,
    action.index
  )
  const markerIntroducesComplementarySlot = (marker) => {
    const slots = ["primary", "auxiliary"].filter(
      (slot) =>
        tokenIndex(clause.tokens, new Set([slot]), marker + 1, action.index) >=
        0
    )
    if (
      slots.length !== 1 ||
      !clause.actors.some(
        (actor) => actor.start > marker && actor.end <= action.index
      )
    ) {
      return false
    }
    const slot = slots[0]
    return clause.reviewers.some(
      (reviewer) =>
        reviewer.index !== action.index &&
        ((slot === "primary" && reviewer.auxiliary) ||
          (slot === "auxiliary" && reviewer.primary))
    )
  }
  const countBeforeMore = tokenIndexes(
    clause.tokens,
    new Set([...counts.keys()]),
    start,
    action.index
  ).some(
    (count) =>
      tokenIndex(
        clause.tokens,
        new Set(["more"]),
        count + 1,
        Math.min(action.index, count + 3)
      ) >= 0
  )
  if (
    extraMarkers.some((marker) => !markerIntroducesComplementarySlot(marker)) ||
    countBeforeMore
  ) {
    return { extra: true, count: null, qualifiers: new Set() }
  }
  for (let index = action.index - 1; index >= start; index -= 1) {
    const count = counts.get(clause.tokens[index])
    if (count === undefined) continue
    const qualifiers = new Set()
    if (
      tokenIndex(
        clause.tokens,
        new Set(["primary"]),
        index + 1,
        action.index
      ) >= 0
    ) {
      qualifiers.add("primary")
    }
    if (
      tokenIndex(
        clause.tokens,
        new Set(["auxiliary"]),
        index + 1,
        action.index
      ) >= 0
    ) {
      qualifiers.add("auxiliary")
    }
    if (
      tokenIndex(clause.tokens, new Set(["codex"]), index + 1, action.index) >=
      0
    ) {
      qualifiers.add("codex")
    }
    if (
      phraseIndex(clause.tokens, ["task", "agent"], index + 1, action.index) >=
      0
    ) {
      qualifiers.add("task_agent")
    }
    return { extra: false, count, qualifiers }
  }
  return null
}

function reviewerCardinalityContradictsRoute(cardinality, normal, high) {
  if (!cardinality) return false
  if (cardinality.extra) return normal || high
  const expected = new Map([
    ["primary", 1],
    ["auxiliary", high ? 1 : 0],
    ["codex", 1],
    ["task_agent", high ? 1 : 0],
  ])
  if (cardinality.qualifiers.size === 0) {
    return (
      (normal && cardinality.count !== 1) || (high && cardinality.count !== 2)
    )
  }
  return [...cardinality.qualifiers].some(
    (qualifier) => cardinality.count !== expected.get(qualifier)
  )
}

function reviewStatementIsExhaustive(clause, action) {
  const segment = actionSegment(clause, action.index)
  for (const quantifier of tokenIndexes(
    clause.tokens,
    REVIEW_EXHAUSTIVE_QUANTIFIERS,
    segment.start,
    segment.end
  )) {
    if (
      quantifier < action.index &&
      clause.tokens
        .slice(quantifier + 1, action.index)
        .every((token) => REVIEW_ACTOR_TAIL_TERMS.has(token))
    ) {
      return true
    }
    if (ACTOR_LINKS.has(clause.tokens[quantifier + 1])) return true
    const link = tokenIndexes(
      clause.tokens,
      ACTOR_LINKS,
      action.index + 1,
      quantifier + 1
    ).at(-1)
    if (link === undefined) continue
    const actors = actorsAfterLink(clause, link, segment.end)
    if (
      actors.some(
        (actor) => actor.start > quantifier && actor.end <= segment.end
      )
    ) {
      return true
    }
    if (QUANTIFIER_COMPLEMENT_LINKS.has(clause.tokens[quantifier + 1])) {
      continue
    }
    if (
      actors.some(
        (actor) =>
          actor.end <= quantifier &&
          clause.tokens
            .slice(actor.end, quantifier)
            .every((token) => REVIEW_ACTOR_TAIL_TERMS.has(token))
      )
    ) {
      return true
    }
  }
  return (
    ["reviewer", "reviewers"].some(
      (reviewer) =>
        phraseIndex(clause.tokens, ["no", "other", reviewer], action.index) >= 0
    ) || phraseIndex(clause.tokens, ["no", "other", "agent"], action.index) >= 0
  )
}

function actorsMatchReviewerSet(actors, expected) {
  if (actors.length !== expected.length) return false
  const actualRoles = actors.map((actor) => actor.role).sort()
  const expectedRoles = [...expected].sort()
  return actualRoles.every((role, index) => role === expectedRoles[index])
}

function conflictsWithReviewRoute(clause, action, scopes) {
  const bindings = reviewActorBindingsForAction(clause, action)
  const actors = bindings.map((binding) => binding.actor)
  const normal = ["normal", "universal"].some((scope) => scopes.has(scope))
  const high = ["high", "universal"].some((scope) => scopes.has(scope))
  const negated = statementIsNegatedBeforeAction(clause, action)

  if ((normal || high) && reviewerSetIsExplicitlyEmpty(clause, action)) {
    return true
  }
  if (
    (normal || high) &&
    (reviewerSlotIsExplicitlyAbsent(clause, action, "primary") ||
      reviewerRoleIsExplicitlyAbsent(clause, action, "codex"))
  ) {
    return true
  }
  if (
    high &&
    (reviewerSlotIsExplicitlyAbsent(clause, action, "auxiliary") ||
      reviewerRoleIsExplicitlyAbsent(clause, action, "task_agent"))
  ) {
    return true
  }
  if (negated) return false

  const cardinality = explicitReviewerCardinality(clause, action)
  if (reviewerCardinalityContradictsRoute(cardinality, normal, high)) {
    return true
  }

  const actionSlot = reviewSlotForAction(clause, action)
  if (
    normal &&
    (actionSlot === "auxiliary" ||
      bindings.some((binding) => binding.slot === "auxiliary") ||
      actors.length > 1)
  ) {
    return true
  }
  if (high && actors.length > 2) return true
  if (
    high &&
    actors.length > 1 &&
    actors.some(
      (actor, index) =>
        actors.findIndex((candidate) => candidate.role === actor.role) !== index
    )
  ) {
    return true
  }
  if (
    high &&
    reviewStatementIsExhaustive(clause, action) &&
    actors.length === 0 &&
    actionSlot !== null
  ) {
    return true
  }

  for (const { actor, slot } of bindings) {
    if (
      high &&
      ((slot === "primary" && actor.role === "task_agent") ||
        (slot === "auxiliary" && actor.role === "codex"))
    ) {
      return true
    }
  }
  if (
    high &&
    actors.length === 2 &&
    bindings.some((binding) => binding.slot !== null) &&
    (!bindings.some(
      ({ actor, slot }) => actor.role === "codex" && slot === "primary"
    ) ||
      !bindings.some(
        ({ actor, slot }) => actor.role === "task_agent" && slot === "auxiliary"
      ))
  ) {
    return true
  }

  if (!reviewStatementIsExhaustive(clause, action) || actors.length === 0) {
    return false
  }
  if (normal && !actorsMatchReviewerSet(actors, ["codex"])) return true
  if (high && !actorsMatchReviewerSet(actors, ["codex", "task_agent"])) {
    return true
  }
  return false
}

function conflictsWithDirectRoute(clause, action, scopes) {
  const bindings = routeTargetBindingsForAction(clause, action)
  const normal = ["normal", "universal"].some((scope) => scopes.has(scope))
  const high = ["high", "universal"].some((scope) => scopes.has(scope))
  const reviewBindings = bindings.filter((binding) => binding.purpose)
  const implementationBindings = bindings.filter((binding) => !binding.purpose)

  if (
    reviewBindings.length > 0 &&
    implementationBindings.some((binding) => !binding.implementationExplicit)
  ) {
    return true
  }

  if (reviewBindings.length > 0) {
    const targets = reviewBindings.map((binding) => binding.actor)
    if (
      normal &&
      (targets.length > 1 ||
        targets.some((target) => target.role !== "codex") ||
        reviewBindings.some((binding) => binding.slot === "auxiliary"))
    ) {
      return true
    }
    if (high && targets.length > 2) return true
    if (
      high &&
      targets.some((target) => !["codex", "task_agent"].includes(target.role))
    ) {
      return true
    }
    if (
      high &&
      reviewBindings.some(
        ({ actor, slot }) =>
          (slot === "primary" && actor.role === "task_agent") ||
          (slot === "auxiliary" && actor.role === "codex")
      )
    ) {
      return true
    }
    if (
      high &&
      targets.length === 2 &&
      reviewBindings.some((binding) => binding.slot !== null) &&
      (!reviewBindings.some(
        ({ actor, slot }) => actor.role === "codex" && slot === "primary"
      ) ||
        !reviewBindings.some(
          ({ actor, slot }) =>
            actor.role === "task_agent" && slot === "auxiliary"
        ))
    ) {
      return true
    }
    if (
      high &&
      targets.some(
        (target, index) =>
          targets.findIndex((candidate) => candidate.role === target.role) !==
          index
      )
    ) {
      return true
    }
    if (reviewStatementIsExhaustive(clause, action)) {
      if (normal && !actorsMatchReviewerSet(targets, ["codex"])) return true
      if (high && !actorsMatchReviewerSet(targets, ["codex", "task_agent"])) {
        return true
      }
    }
  }

  const implementers = implementationBindings.map((binding) => binding.actor)
  if (implementers.length > 1) return true
  return implementers.some(
    (target) =>
      (high && target.role === "task_agent") ||
      (normal && target.role === "codex")
  )
}

function reviewActionIsRoutePurpose(clause, action) {
  const route = clause.actions
    .filter(
      (candidate) =>
        candidate.kind === "route" && candidate.index < action.index
    )
    .at(-1)
  return Boolean(route && routeReviewPurpose(clause, route))
}

function conflictsWithParentOwnership(clause) {
  const parents = clause.actors.filter((actor) => actor.role === "parent")
  for (const parent of parents) {
    const link = linkBeforeActor(clause, parent)
    if (link !== undefined && clause.tokens[link] === "by") {
      const action = nearestActionBefore(clause, link)
      if (
        action?.kind === "production" &&
        (actionHasDocumentTarget(clause, action) ||
          predicateActionGroup(clause, action).some((candidate) =>
            actionHasDocumentTarget(clause, candidate)
          )) &&
        !relationTargetIsNegated(clause, action, link)
      ) {
        return true
      }
    }

    for (const action of clause.actions) {
      if (
        action.kind !== "production" ||
        action.index <= parent.start ||
        actionIsNegated(clause.tokens, action.index) ||
        !actionHasDocumentTarget(clause, action)
      ) {
        continue
      }
      const passiveActors = passiveActorsForAction(clause, action)
      if (
        passiveActors.length > 0 &&
        passiveActors.every((actor) => actor.role !== "parent")
      ) {
        continue
      }
      if (
        actionDelegatesToProducer(clause, parent, action) ||
        actionIsPassivelyDelegatedToProducer(clause, parent, action)
      ) {
        continue
      }
      if (repeatedProducerSubjectForAction(clause, action)) continue
      return true
    }
  }
  return false
}

function conflictsWithTaskAgentRoute(clause) {
  for (const action of clause.actions) {
    if (actionIsExcludedAlternative(clause, action)) continue
    const passiveSubjects = passiveActorsForAction(clause, action)
    const scopes = actionTaskScopes(clause, action)
    if (
      action.kind === "review" &&
      !reviewActionIsRoutePurpose(clause, action) &&
      conflictsWithReviewRoute(clause, action, scopes)
    ) {
      return true
    }
    if (
      action.kind !== "route" &&
      actionIsNegated(clause.tokens, action.index) &&
      passiveSubjects.length === 0
    ) {
      continue
    }
    const subjects =
      passiveSubjects.length > 0
        ? passiveSubjects
        : subjectActorsForAction(clause, action)
    if (
      action.kind === "production" &&
      !scopes.has("unspecified") &&
      subjects.length > 1
    ) {
      return true
    }
    if (
      action.kind === "production" &&
      ["high", "universal"].some((scope) => scopes.has(scope)) &&
      subjects.some((actor) => actor.role === "task_agent")
    ) {
      return true
    }
    if (
      action.kind === "production" &&
      ["normal", "universal"].some((scope) => scopes.has(scope)) &&
      subjects.some((actor) => actor.role === "codex")
    ) {
      return true
    }
    if (
      action.kind === "review" &&
      ["normal", "universal"].some((scope) => scopes.has(scope)) &&
      subjects.some((actor) => actor.role === "task_agent")
    ) {
      return true
    }
    if (action.kind !== "route") {
      continue
    }
    if (subjects.some((actor) => actor.role === "task_agent")) return true
    if (conflictsWithDirectRoute(clause, action, scopes)) return true
  }
  return false
}

function conflictsWithConversationIdentity(clause) {
  const { tokens } = clause
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

function taskStateIndexes(clause, task, states) {
  return tokenIndexes(
    clause.tokens,
    states,
    Math.max(0, task.index - 2),
    task.index + 5
  )
}

function completionBelongsToTask(clause, task, completion) {
  if (completion >= task.index && completion - task.index <= 4) {
    return clause.tokens
      .slice(task.index + 1, completion)
      .every(
        (token) =>
          TASK_COMPLETION_BRIDGE_TERMS.has(token) || token.endsWith("ly")
      )
  }
  if (
    completion < task.index &&
    task.index - completion <= 2 &&
    clause.tokens[completion] !== "completion"
  ) {
    return true
  }
  if (clause.tokens[completion] !== "completion") return false
  const objectLink = tokenIndex(
    clause.tokens,
    new Set(["of"]),
    completion + 1,
    task.index
  )
  return (
    objectLink >= 0 &&
    tokenIndex(
      clause.tokens,
      TASK_REFERENCE_BOUNDARIES,
      objectLink + 1,
      task.index
    ) < 0 &&
    tokenIndex(
      clause.tokens,
      new Set([
        ...PRODUCTION_ACTIONS,
        ...DIRECT_ROUTE_ACTIONS,
        ...REVIEW_TERMS,
      ]),
      objectLink + 1,
      task.index
    ) < 0
  )
}

function taskHasCompletedState(clause, task) {
  return tokenIndexes(clause.tokens, TASK_COMPLETION_TERMS).some(
    (state) =>
      completionBelongsToTask(clause, task, state) &&
      !actionIsNegated(clause.tokens, state)
  )
}

function taskHasAffirmativeActivity(clause, task) {
  const explicit = taskStateIndexes(clause, task, EXPLICIT_TASK_ACTIVITY_TERMS)
  if (explicit.some((state) => !actionIsNegated(clause.tokens, state))) {
    return true
  }
  return (
    taskStateIndexes(clause, task, new Set(["current"])).length > 0 &&
    !taskHasCompletedState(clause, task) &&
    explicit.length === 0
  )
}

function taskHasNegatedActivity(clause, task) {
  return taskStateIndexes(clause, task, EXPLICIT_TASK_ACTIVITY_TERMS).some(
    (state) => actionIsNegated(clause.tokens, state)
  )
}

function hasActiveTaskTiming(clause) {
  return tokenIndexes(clause.tokens, ACTIVE_TIMING_MARKERS).some((marker) =>
    clause.tasks.some(
      (task) =>
        Math.abs(task.index - marker) <= 8 &&
        taskHasAffirmativeActivity(clause, task)
    )
  )
}

function hasNegatedTaskActivityTiming(clause) {
  return tokenIndexes(clause.tokens, ACTIVE_TIMING_MARKERS).some((marker) =>
    clause.tasks.some(
      (task) =>
        Math.abs(task.index - marker) <= 8 &&
        taskHasNegatedActivity(clause, task)
    )
  )
}

function timingSegmentEnd(clause, marker) {
  return (
    tokenIndexes(
      clause.tokens,
      new Set([...BOUNDARY_TIMING_MARKERS, ...PRE_COMPLETION_TIMING_MARKERS]),
      marker + 1
    )[0] ?? clause.tokens.length
  )
}

function timingReferencesCompletion(clause, marker) {
  const end = timingSegmentEnd(clause, marker)
  return tokenIndexes(clause.tokens, TASK_COMPLETION_TERMS, marker + 1, end)
    .filter((completion) => !actionIsNegated(clause.tokens, completion))
    .some((completion) => {
      if (
        clause.tasks.some(
          (task) =>
            task.index > marker &&
            task.index < end &&
            completionBelongsToTask(clause, task, completion)
        )
      ) {
        return true
      }
      const hasTaskAnaphor =
        tokenIndex(
          clause.tokens,
          new Set(["it", "its", "that", "this"]),
          marker + 1,
          Math.min(end, completion + 2)
        ) >= 0
      const hasImplicitTaskSubject = completion === marker + 1
      return (
        clause.priorTasks.length > 0 &&
        (hasTaskAnaphor || hasImplicitTaskSubject)
      )
    })
}

function hasCompletedTaskTiming(clause) {
  return tokenIndexes(clause.tokens, BOUNDARY_TIMING_MARKERS).some((marker) =>
    timingReferencesCompletion(clause, marker)
  )
}

function hasPreCompletionTaskTiming(clause) {
  return tokenIndexes(clause.tokens, PRE_COMPLETION_TIMING_MARKERS).some(
    (marker) => timingReferencesCompletion(clause, marker)
  )
}

function conflictsWithActiveTaskSwitch(clause) {
  const { tokens } = clause
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
    clause.actors.some((actor) => ["codex", "task_agent"].includes(actor.role))
  if (!hasAgent) return false
  if (hasActiveTaskTiming(clause)) return true
  if (hasPreCompletionTaskTiming(clause)) return true
  if (hasCompletedTaskTiming(clause)) {
    const laterActiveState = clause.tasks.some(
      (task) =>
        taskStateIndexes(clause, task, EXPLICIT_TASK_ACTIVITY_TERMS).some(
          (state) => !actionIsNegated(clause.tokens, state)
        ) && !taskHasCompletedState(clause, task)
    )
    return laterActiveState
  }
  if (
    clause.tasks.some(
      (task) =>
        taskHasAffirmativeActivity(clause, task) &&
        taskStateIndexes(clause, task, EXPLICIT_TASK_ACTIVITY_TERMS).length > 0
    )
  ) {
    return true
  }
  if (clause.tasks.some((task) => taskHasCompletedState(clause, task))) {
    return false
  }
  if (clause.priorTasks.some((task) => task.active)) return true
  if (clause.priorTasks.some((task) => task.completed)) return false
  if (hasNegatedTaskActivityTiming(clause)) return false
  return clause.tasks.some((task) => taskHasAffirmativeActivity(clause, task))
}

function reviewerIsRequiredByContract(reviewer) {
  return (
    (reviewer.primary ||
      reviewer.auxiliary ||
      reviewer.codex ||
      reviewer.required) &&
    !reviewer.optionalDocument
  )
}

function reviewAntecedentBefore(clause, index) {
  const current = clause.reviewers.filter(
    (reviewer) =>
      reviewer.index < index && reviewerIsRequiredByContract(reviewer)
  )
  if (current.length > 0) return current.at(-1)
  const prior = clause.priorReviewers.filter(reviewerIsRequiredByContract)
  return prior.at(-1) ?? null
}

function takeRoleObjectLink(clause, index) {
  let cursor = index + 1
  if (clause.tokens[cursor] === "on") cursor += 1
  if (new Set(["a", "an", "the"]).has(clause.tokens[cursor])) cursor += 1
  if (clause.tokens[cursor] !== "role") return -1
  return tokenIndex(
    clause.tokens,
    new Set(["of"]),
    cursor + 1,
    Math.min(clause.tokens.length, cursor + 3)
  )
}

function reviewActionIsReplacement(clause, index) {
  if (REVIEW_REPLACEMENT_ACTIONS.has(clause.tokens[index])) return true
  if (!REVIEW_TAKE_ACTIONS.has(clause.tokens[index])) return false
  return (
    clause.tokens[index + 1] === "over" ||
    takeRoleObjectLink(clause, index) >= 0
  )
}

function reviewBypassActionIndexes(
  clause,
  start = 0,
  end = clause.tokens.length
) {
  const direct = tokenIndexes(clause.tokens, REVIEW_BYPASS_ACTIONS, start, end)
  const takeOvers = tokenIndexes(
    clause.tokens,
    REVIEW_TAKE_ACTIONS,
    start,
    end
  ).filter((index) => reviewActionIsReplacement(clause, index))
  return [...new Set([...direct, ...takeOvers])].sort(
    (left, right) => left - right
  )
}

function substitutionReviewTarget(clause, bypass) {
  const token = clause.tokens[bypass]
  let objectLink = -1
  if (token === "instead") {
    objectLink = tokenIndex(
      clause.tokens,
      new Set(["of"]),
      bypass + 1,
      bypass + 3
    )
  } else if (token === "place") {
    const substitutionPrefix = tokenIndex(
      clause.tokens,
      new Set(["in", "take", "takes", "taking", "took"]),
      Math.max(0, bypass - 3),
      bypass
    )
    if (substitutionPrefix >= 0) {
      objectLink = tokenIndex(
        clause.tokens,
        new Set(["of"]),
        bypass + 1,
        bypass + 3
      )
      if (
        objectLink < 0 &&
        ["its", "their"].includes(clause.tokens[bypass - 1])
      ) {
        const replacementSubject = clause.reviewers
          .filter((reviewer) => reviewer.index < bypass)
          .at(-1)
        return reviewAntecedentBefore(
          clause,
          replacementSubject?.index ?? bypass
        )
      }
    }
  } else if (REVIEW_TAKE_ACTIONS.has(token)) {
    const roleObjectLink = takeRoleObjectLink(clause, bypass)
    objectLink =
      roleObjectLink >= 0
        ? roleObjectLink
        : tokenIndex(clause.tokens, new Set(["for"]), bypass + 1, bypass + 4)
  } else if (reviewActionIsReplacement(clause, bypass)) {
    objectLink = tokenIndex(
      clause.tokens,
      new Set(["for"]),
      bypass + 1,
      bypass + 4
    )
  }
  if (objectLink < 0) return null
  return (
    clause.reviewers.find(
      (reviewer) =>
        reviewer.index > objectLink && reviewer.index <= objectLink + 5
    ) ?? null
  )
}

function reviewTargetForBypass(clause, bypass) {
  const segment = actionSegment(clause, bypass)
  const reviewers = clause.reviewers.filter(
    (reviewer) =>
      reviewer.index >= segment.start && reviewer.index < segment.end
  )
  const before = reviewers.filter((reviewer) => reviewer.index < bypass).at(-1)
  const after = reviewers.find((reviewer) => reviewer.index > bypass)
  const replacement = reviewActionIsReplacement(clause, bypass)
  const substitution = substitutionReviewTarget(clause, bypass)
  if (substitution) return substitution
  const roleReference =
    replacement &&
    !after &&
    tokenIndexes(
      clause.tokens,
      new Set(["reviewer", "role"]),
      bypass + 1,
      Math.min(segment.end, bypass + 6)
    ).some((target) => {
      const prefix = clause.tokens.slice(
        Math.max(bypass + 1, target - 2),
        target
      )
      if (clause.tokens[target] === "role") {
        return new Set(["same", "that", "the", "their", "this", "its"]).has(
          clause.tokens[target - 1]
        )
      }
      return prefix.some((token) =>
        new Set(["same", "that", "the", "their", "this", "its"]).has(token)
      )
    })
  const pronounTarget =
    replacement &&
    (tokenIndex(
      clause.tokens,
      new Set(["former", "it", "them"]),
      bypass + 1,
      Math.min(segment.end, bypass + 6)
    ) >= 0 ||
      phraseIndex(
        clause.tokens,
        ["that", "reviewer"],
        bypass + 1,
        Math.min(segment.end, bypass + 6)
      ) >= 0 ||
      phraseIndex(
        clause.tokens,
        ["this", "reviewer"],
        bypass + 1,
        Math.min(segment.end, bypass + 6)
      ) >= 0 ||
      phraseIndex(
        clause.tokens,
        ["this", "role"],
        bypass + 1,
        Math.min(segment.end, bypass + 6)
      ) >= 0 ||
      phraseIndex(
        clause.tokens,
        ["that", "role"],
        bypass + 1,
        Math.min(segment.end, bypass + 6)
      ) >= 0 ||
      phraseIndex(
        clause.tokens,
        ["its", "role"],
        bypass + 1,
        Math.min(segment.end, bypass + 6)
      ) >= 0)
  const passiveTarget =
    replacement &&
    tokenIndex(clause.tokens, new Set(["by"]), bypass + 1, segment.end) >= 0
  if (roleReference || pronounTarget || passiveTarget) {
    const replacementSubject = before
    return reviewAntecedentBefore(clause, replacementSubject?.index ?? bypass)
  }
  if (
    before &&
    tokenIndex(clause.tokens, REVIEW_SUBJECT_LINKS, before.index + 1, bypass) >=
      0
  ) {
    return before
  }
  if (after) return after
  if (before) return before
  return reviewAntecedentBefore(clause, bypass)
}

function reviewBypassIsNegated(clause, bypass) {
  return (
    actionIsNegated(clause.tokens, bypass) ||
    (clause.tokens[bypass - 2] === "rather" &&
      clause.tokens[bypass - 1] === "than")
  )
}

function conflictsWithRequiredReview(clause) {
  for (const requirement of tokenIndexes(
    clause.tokens,
    REVIEW_REQUIREMENT_ACTIONS
  )) {
    if (!actionIsNegated(clause.tokens, requirement)) continue
    const segment = actionSegment(clause, requirement)
    const negatedBypass = reviewBypassActionIndexes(
      clause,
      segment.start,
      requirement
    )
      .reverse()
      .find((bypass) => reviewBypassIsNegated(clause, bypass))
    if (negatedBypass !== undefined) continue
    const target = reviewTargetForBypass(clause, requirement)
    if (target && reviewerIsRequiredByContract(target)) {
      return true
    }
  }
  return reviewBypassActionIndexes(clause).some((bypass) => {
    if (
      clause.tokens[bypass] === "place" &&
      !substitutionReviewTarget(clause, bypass)
    ) {
      return false
    }
    if (reviewBypassIsNegated(clause, bypass)) return false
    const target = reviewTargetForBypass(clause, bypass)
    return Boolean(target && reviewerIsRequiredByContract(target))
  })
}

function hasConflictingSkillDirective(prose) {
  return directiveWindows(prose).some((window) => {
    const clause = parseDirectiveClause(window)
    return (
      conflictsWithParentOwnership(clause) ||
      conflictsWithTaskAgentRoute(clause) ||
      conflictsWithConversationIdentity(clause) ||
      conflictsWithActiveTaskSwitch(clause) ||
      conflictsWithRequiredReview(clause)
    )
  })
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
    const pendingTasksRemainClean = suffix.every(
      (task) =>
        isObject(task) &&
        (task.status !== "pending" ||
          (Array.isArray(task.runs) && task.runs.length === 0))
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
      hasAdmittedImplementerRun &&
      pendingTasksRemainClean
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
