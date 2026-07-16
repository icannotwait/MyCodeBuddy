/**
 * Read-only projection of Codeg and platform-native sub-agent activity.
 *
 * Lifecycle boundary:
 * - Native views are informational only (`origin: "native"`, `authoritative: false`).
 * - Native activities NEVER enter DelegationBroker / task store, never claim
 *   startup reconciliation, and never expose Broker cancel actions/callbacks.
 * - Original tool-call blocks remain rendered as-is; activity is derived alongside.
 * - Codeg views are authoritative and may retain existing Codeg actions.
 *
 * Conservative parsing only: known four-platform tools, documented id fields,
 * and explicit terminal status when the platform emits it.
 */

import type {
  AgentType,
  DelegationActivityOperation,
  DelegationActivityView,
  DelegationObservedStatus,
} from "@/lib/types"
import { normalizeToolName } from "@/lib/tool-call-normalization"

export type {
  DelegationActivityOperation,
  DelegationActivityView,
  DelegationObservedStatus,
} from "@/lib/types"

/** Managed platforms with a native sub-agent surface for this projection. */
export type ManagedNativePlatform =
  | "codex"
  | "grok"
  | "code_buddy"
  | "claude_code"

export type NativeToolCallSignal = {
  /** Default tool-call path (omitted kind treated as tool_call). */
  kind?: "tool_call"
  platform: ManagedNativePlatform
  toolName: string
  toolCallId: string
  input: string | null
  output: string | null
  at: string
  /**
   * ACP tool-call status. A tool-call error is NOT treated as child-task
   * failure — observed_status stays unknown unless the platform body says so.
   */
  toolCallStatus?: "pending" | "in_progress" | "completed" | "failed"
}

/**
 * CodeBuddy background task notifications enter as an explicit signal variant
 * rather than a fabricated tool call.
 */
export type CodeBuddyBackgroundSignal = {
  kind: "codebuddy_background"
  platform: "code_buddy"
  taskId?: string
  status?: string
  at: string
  operation?: DelegationActivityOperation
}

/**
 * Claude raw SDK task messages enter as an explicit signal variant rather than
 * a fabricated tool call.
 */
export type ClaudeSdkTaskSignal = {
  kind: "claude_sdk_task"
  platform: "claude_code"
  taskId?: string
  status?: string
  at: string
  operation?: DelegationActivityOperation
}

export type NativeDelegationSignal =
  | NativeToolCallSignal
  | CodeBuddyBackgroundSignal
  | ClaudeSdkTaskSignal

export type CodegDelegationActivityEvent =
  | {
      type: "delegation_started"
      agent_type: AgentType
      task_id?: string
      parent_tool_use_id?: string
      at?: string
    }
  | {
      type: "delegation_completed"
      agent_type: AgentType
      task_id?: string
      parent_tool_use_id?: string
      /** Terminal status from the broker result / binding. */
      status?: DelegationObservedStatus | "ok" | "err" | string
      at?: string
    }

/** Exact four-platform known native tools → operation. */
const NATIVE_TOOL_OPERATIONS: Readonly<
  Record<
    ManagedNativePlatform,
    Readonly<Record<string, DelegationActivityOperation>>
  >
> = {
  codex: {
    spawn_agent: "spawn",
    wait_agent: "wait",
    list_agents: "status",
    interrupt_agent: "cancel",
  },
  grok: {
    spawn_subagent: "spawn",
    get: "status",
    wait: "wait",
    kill: "cancel",
  },
  code_buddy: {
    agent: "spawn",
    task: "spawn",
  },
  claude_code: {
    agent: "spawn",
    // Design: Agent/Task creation surface; TaskOutput/TaskStop for lifecycle.
    task: "spawn",
    taskoutput: "wait",
    task_output: "wait",
    taskstop: "cancel",
    task_stop: "cancel",
  },
}

function isManagedNativePlatform(
  platform: AgentType
): platform is ManagedNativePlatform {
  return (
    platform === "codex" ||
    platform === "grok" ||
    platform === "code_buddy" ||
    platform === "claude_code"
  )
}

/** Guarded JSON object parse — non-objects and parse failures yield null. */
function parseObject(
  raw: string | null | undefined
): Record<string, unknown> | null {
  if (raw == null) return null
  const trimmed = raw.trim()
  if (!trimmed) return null
  try {
    const v = JSON.parse(trimmed) as unknown
    if (v && typeof v === "object" && !Array.isArray(v)) {
      return v as Record<string, unknown>
    }
    return null
  } catch {
    return null
  }
}

/**
 * Canonical key for known-tool matching. Uses normalizeToolName only as a
 * secondary probe — Codex aliases would collapse `spawn_agent`→`agent`, so the
 * primary key preserves the bare tool identity.
 */
function toolMatchKeys(toolName: string): string[] {
  const raw = toolName.trim()
  if (!raw) return []
  const lower = raw.toLowerCase()
  const underscored = lower
    .replace(/[().]/g, "_")
    .replace(/[\s-]+/g, "_")
    .replace(/_+/g, "_")
  const keys = new Set<string>([lower, underscored])
  // Secondary: normalized form (useful for host-prefixed MCP-style names).
  try {
    const normalized = normalizeToolName(raw).toLowerCase()
    if (normalized && normalized !== "tool") keys.add(normalized)
  } catch {
    // normalizeToolName is pure and should not throw; ignore defensively.
  }
  return [...keys]
}

function resolveNativeOperation(
  platform: ManagedNativePlatform,
  toolName: string
): DelegationActivityOperation | null {
  const table = NATIVE_TOOL_OPERATIONS[platform]
  for (const key of toolMatchKeys(toolName)) {
    const op = table[key]
    if (op) return op
  }
  return null
}

/**
 * Extract ids only from documented fields: agent_id, task_id, agentId, taskId.
 * Never invent from other keys.
 */
function extractDocumentedId(
  ...sources: Array<Record<string, unknown> | null>
): string | undefined {
  for (const obj of sources) {
    if (!obj) continue
    for (const key of ["agent_id", "task_id", "agentId", "taskId"] as const) {
      const v = obj[key]
      if (typeof v === "string") {
        const trimmed = v.trim()
        if (trimmed.length > 0) return trimmed
      }
    }
  }
  return undefined
}

function isWaitTimeout(
  input: Record<string, unknown> | null,
  output: Record<string, unknown> | null
): boolean {
  for (const obj of [output, input]) {
    if (!obj) continue
    if (obj.timed_out === true || obj.timedOut === true) return true
    if (obj.timeout === true) return true
    const status =
      typeof obj.status === "string" ? obj.status.toLowerCase() : ""
    if (
      status === "timeout" ||
      status === "timed_out" ||
      status === "timedout"
    ) {
      return true
    }
    const retrieval =
      typeof obj.retrieval_status === "string"
        ? obj.retrieval_status.toLowerCase()
        : typeof obj.retrievalStatus === "string"
          ? obj.retrievalStatus.toLowerCase()
          : ""
    if (retrieval === "timeout") return true
  }
  return false
}

/**
 * Map an explicit platform status string. Returns null when the value is
 * absent or unrecognized (caller keeps unknown).
 */
function mapExplicitStatus(
  raw: string | null | undefined
): DelegationObservedStatus | null {
  if (raw == null) return null
  const s = raw.trim().toLowerCase()
  if (!s) return null
  switch (s) {
    case "running":
    case "in_progress":
    case "inprogress":
    case "pending":
    case "pendinginit":
    case "active":
    case "started":
      return "running"
    case "completed":
    case "complete":
    case "success":
    case "ok":
    case "succeeded":
    case "done":
      return "completed"
    case "failed":
    case "fail":
    case "error":
    case "errored":
    case "err":
      return "failed"
    case "canceled":
    case "cancelled":
    case "interrupted":
    case "stopped":
    case "shutdown":
    case "killed":
      return "canceled"
    case "timeout":
    case "timed_out":
    case "timedout":
    case "unknown":
      return "unknown"
    default:
      return null
  }
}

function extractExplicitStatus(
  ...sources: Array<Record<string, unknown> | null>
): DelegationObservedStatus | null {
  for (const obj of sources) {
    if (!obj) continue
    if (typeof obj.status === "string") {
      const mapped = mapExplicitStatus(obj.status)
      if (mapped) return mapped
    }
  }
  return null
}

function isTerminal(status: DelegationObservedStatus): boolean {
  return status === "completed" || status === "failed" || status === "canceled"
}

function mergeWithPrevious(
  view: DelegationActivityView,
  previous?: DelegationActivityView | null
): DelegationActivityView {
  if (!previous || previous.origin !== view.origin) return view
  const task_id = view.task_id ?? previous.task_id
  const started_at = previous.started_at ?? view.started_at
  const updated_at = view.updated_at ?? previous.updated_at
  let observed_status = view.observed_status
  // Prefer explicit new status; if new is unknown and previous was known, keep
  // previous unless the operation is wait-timeout (already unknown on view).
  if (
    view.observed_status === "unknown" &&
    previous.observed_status !== "unknown" &&
    view.operation !== "wait"
  ) {
    observed_status = previous.observed_status
  }
  const finished_at =
    view.finished_at ??
    (isTerminal(observed_status)
      ? (previous.finished_at ?? view.updated_at)
      : previous.finished_at)
  return {
    ...view,
    task_id,
    observed_status,
    started_at,
    updated_at,
    finished_at,
  }
}

function projectToolCallSignal(
  signal: NativeToolCallSignal,
  previous?: DelegationActivityView | null
): DelegationActivityView | null {
  const operation = resolveNativeOperation(signal.platform, signal.toolName)
  if (!operation) return null

  const inputObj = parseObject(signal.input)
  const outputObj = parseObject(signal.output)
  const task_id = extractDocumentedId(outputObj, inputObj)

  let observed_status: DelegationObservedStatus = "unknown"

  if (isWaitTimeout(inputObj, outputObj)) {
    // Wait timeout is unknown, never failed.
    observed_status = "unknown"
  } else {
    const explicit = extractExplicitStatus(outputObj, inputObj)
    if (explicit) {
      observed_status = explicit
    } else if (operation === "spawn" && !signal.output) {
      // Spawn in flight with no body yet — still running observation only when
      // the tool call itself is active; otherwise leave unknown.
      if (
        signal.toolCallStatus === "pending" ||
        signal.toolCallStatus === "in_progress" ||
        signal.toolCallStatus == null
      ) {
        observed_status =
          signal.toolCallStatus === "pending" ||
          signal.toolCallStatus === "in_progress"
            ? "running"
            : "unknown"
      }
    } else if (operation === "cancel") {
      // Cancel tool without explicit body status: unknown (not invented canceled).
      observed_status = "unknown"
    }
  }

  // Tool-call error is NOT child failure — never force failed from toolCallStatus.
  // (explicit platform body status may still be failed above.)

  const at = signal.at
  const view: DelegationActivityView = {
    origin: "native",
    authoritative: false,
    platform: signal.platform,
    ...(task_id ? { task_id } : {}),
    operation,
    observed_status,
    started_at: at,
    updated_at: at,
    ...(isTerminal(observed_status) ? { finished_at: at } : {}),
  }

  return mergeWithPrevious(view, previous)
}

function projectExplicitVariant(
  signal: CodeBuddyBackgroundSignal | ClaudeSdkTaskSignal,
  previous?: DelegationActivityView | null
): DelegationActivityView {
  const task_id =
    typeof signal.taskId === "string" && signal.taskId.trim().length > 0
      ? signal.taskId.trim()
      : undefined
  const observed_status =
    mapExplicitStatus(signal.status) ?? ("unknown" as const)
  const operation: DelegationActivityOperation =
    signal.operation ??
    (observed_status === "canceled"
      ? "cancel"
      : observed_status === "running"
        ? "status"
        : "status")
  const at = signal.at
  const view: DelegationActivityView = {
    origin: "native",
    authoritative: false,
    platform: signal.platform,
    ...(task_id ? { task_id } : {}),
    operation,
    observed_status,
    started_at: at,
    updated_at: at,
    ...(isTerminal(observed_status) ? { finished_at: at } : {}),
  }
  return mergeWithPrevious(view, previous)
}

/**
 * Project a native platform signal into a read-only activity view.
 * Returns null when the tool/shape is not a known native sub-agent signal
 * (callers keep rendering the original tool call unchanged).
 */
export function projectNativeDelegationActivity(
  signal: NativeDelegationSignal,
  previous?: DelegationActivityView | null
): DelegationActivityView | null {
  if (signal.kind === "codebuddy_background") {
    return projectExplicitVariant(signal, previous)
  }
  if (signal.kind === "claude_sdk_task") {
    return projectExplicitVariant(signal, previous)
  }
  // tool_call (default)
  return projectToolCallSignal(signal, previous)
}

/**
 * Project a Codeg Broker lifecycle event into an authoritative activity view.
 * Does not invent Broker cancel callbacks — the view is data only.
 */
export function projectCodegDelegationActivity(
  event: CodegDelegationActivityEvent
): DelegationActivityView {
  const at = event.at ?? new Date().toISOString()
  if (event.type === "delegation_started") {
    return {
      origin: "codeg",
      authoritative: true,
      platform: event.agent_type,
      ...(event.task_id ? { task_id: event.task_id } : {}),
      operation: "spawn",
      observed_status: "running",
      started_at: at,
      updated_at: at,
    }
  }

  const mapped = mapExplicitStatus(event.status)
  const observed_status: DelegationObservedStatus = mapped ?? "unknown"
  return {
    origin: "codeg",
    authoritative: true,
    platform: event.agent_type,
    ...(event.task_id ? { task_id: event.task_id } : {}),
    operation: "spawn",
    observed_status,
    started_at: at,
    updated_at: at,
    ...(isTerminal(observed_status) ? { finished_at: at } : {}),
  }
}

/**
 * Infer managed native platform from a known tool name when the session agent
 * type is unavailable. Returns null for ambiguous names (Agent/Task) or
 * unmapped tools.
 */
export function inferNativePlatformFromToolName(
  toolName: string
): ManagedNativePlatform | null {
  const keys = toolMatchKeys(toolName)
  for (const platform of [
    "codex",
    "grok",
    "code_buddy",
    "claude_code",
  ] as const) {
    const table = NATIVE_TOOL_OPERATIONS[platform]
    for (const key of keys) {
      if (table[key]) {
        // Agent/Task are shared by code_buddy and claude_code — ambiguous.
        if (key === "agent" || key === "task") return null
        // Grok get/wait/kill are short names; only claim when exclusive.
        if (
          platform === "grok" &&
          (key === "get" || key === "wait" || key === "kill")
        ) {
          return "grok"
        }
        if (platform === "codex") return "codex"
        if (platform === "grok") return "grok"
        if (platform === "claude_code") return "claude_code"
      }
    }
  }
  return null
}

/**
 * Derive native activity views from live/persisted tool-call fields without
 * mutating the source blocks. Pass the session agent type when known so
 * Agent/Task tools resolve to the correct platform.
 */
export function deriveNativeActivitiesFromToolCalls(
  tools: ReadonlyArray<{
    toolCallId: string
    toolName: string
    input?: string | null
    output?: string | null
    status?: string | null
    at?: string
  }>,
  platformHint?: AgentType | null
): DelegationActivityView[] {
  const activities: DelegationActivityView[] = []
  const indexByTaskId = new Map<string, number>()

  for (const tool of tools) {
    let platform: ManagedNativePlatform | null = null
    if (platformHint && isManagedNativePlatform(platformHint)) {
      platform = platformHint
    } else {
      platform = inferNativePlatformFromToolName(tool.toolName)
    }
    if (!platform) continue
    if (!resolveNativeOperation(platform, tool.toolName)) continue

    let toolCallStatus: NativeToolCallSignal["toolCallStatus"]
    switch (tool.status) {
      case "pending":
      case "in_progress":
      case "completed":
      case "failed":
        toolCallStatus = tool.status
        break
      default:
        toolCallStatus = undefined
    }

    const prior =
      // Merge against earlier view with the same toolCallId is not tracked here;
      // task_id merge covers multi-tool lifecycle for one native agent.
      undefined as DelegationActivityView | undefined

    const signal: NativeToolCallSignal = {
      platform,
      toolName: tool.toolName,
      toolCallId: tool.toolCallId,
      input: tool.input ?? null,
      output: tool.output ?? null,
      at: tool.at ?? new Date(0).toISOString(),
      toolCallStatus,
    }

    // If we already have a view for this task id, pass it as previous.
    const provisional = projectNativeDelegationActivity(signal, prior)
    if (!provisional) continue

    if (provisional.task_id && indexByTaskId.has(provisional.task_id)) {
      const idx = indexByTaskId.get(provisional.task_id)!
      const merged =
        projectNativeDelegationActivity(signal, activities[idx]) ?? provisional
      activities[idx] = merged
    } else {
      if (provisional.task_id) {
        indexByTaskId.set(provisional.task_id, activities.length)
      }
      activities.push(provisional)
    }
  }

  return activities
}

export { isManagedNativePlatform }
