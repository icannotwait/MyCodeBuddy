/**
 * Shared parsing + state-resolution helpers for `delegate_to_agent`
 * delegation cards.
 *
 * Extracted from `DelegatedSubThread` so the inline message-stream card AND
 * the top-right sub-agent overlay resolve the same agent type / task / status /
 * child ids from the exact same logic — one source of truth, no drift.
 *
 * Everything here is pure (no React). The React-specific binding/permission
 * lookups live in `useDelegationCardModel`.
 */

import { extractEmbeddedJsonObject } from "@/lib/embedded-json"
import { formatConversationTitle } from "@/lib/conversation-title"
import { peelMcpResultEnvelope } from "@/lib/mcp-result-envelope"
import {
  ALL_AGENT_TYPES,
  isCustomAgentType,
  type AgentType,
  type AttentionRequestSummary,
  type DelegationRuntimeStats,
  type DelegationTouchedFile,
} from "@/lib/types"
import {
  type DelegationBinding,
  type DelegationStatus,
} from "@/contexts/delegation-context"
import type { ToolCallState } from "@/lib/adapters/ai-elements-adapter"

/**
 * The full status a delegation card can render. Extends the wire-level
 * `DelegationStatus` ("running" | "ok" | "err") with UI-only "starting"
 * (binding not yet arrived), "waiting" (child blocked on a permission
 * decision), and soft-watchdog observations for a still-running binding
 * (never terminal / never destructive).
 */
export type DelegationCardStatus =
  | "starting"
  | "running"
  | "active"
  | "waiting"
  | "waiting_input"
  | "stalled"
  | "ok"
  | "err"

export type ParsedInput = {
  agentType: AgentType | null
  profileLabel: string | null
  task: string | null
  workingDir: string | null
  workUnitKey: string | null
  targetTaskId: string | null
  replacesTaskId: string | null
}

export interface DelegationRunIdentityInput {
  parentConversationId: number
  parentToolUseId: string
  input?: string | null
  output?: string | null
  errorText?: string | null
  meta?: Record<string, unknown> | null
}

export interface DelegationRunIdentity {
  parentConversationId: number
  parentToolUseId: string
  workUnitKey: string | null
  taskId: string | null
  childConversationId: number | null
  linkedTaskIds: string[]
}

// Derived from the canonical `ALL_AGENT_TYPES` so a newly added agent is
// recognized here automatically. A hand-maintained duplicate previously drifted
// (it omitted `grok` and `cursor`), so their delegation cards resolved
// `agentType: null` — rendering the blank "unknown sub-agent" avatar/label
// instead of the agent's icon. Keep this sourced from one place.
const KNOWN_AGENT_TYPES: ReadonlySet<AgentType> = new Set<AgentType>(
  ALL_AGENT_TYPES
)

/**
 * Narrow an untrusted wire string to an `AgentType`, or `null`.
 *
 * Accepts the built-ins plus any `custom:<id>` slug — a user-registered ACP
 * agent is as delegatable as a built-in, and `AgentIcon` / `getAgentLabel`
 * already render one.
 */
export function coerceAgentType(value: unknown): AgentType | null {
  if (typeof value !== "string") return null
  const trimmed = value.trim()
  if (!trimmed) return null
  if (KNOWN_AGENT_TYPES.has(trimmed)) return trimmed as AgentType
  return isCustomAgentType(trimmed) ? (trimmed as AgentType) : null
}

export type ParsedMeta = {
  status: DelegationStatus
  /** Durable run agent used when continuation input omits agent_type. */
  agentType: AgentType | null
  /** Bounded broker task preview for identity-less parent tool calls. */
  task: string | null
  taskId: string | null
  childConnectionId: string | null
  childConversationId: number | null
  errorCode: string | null
  startedAt: string | null
  finishedAt: string | null
  /** Wire snake_case object when shape is valid; never throws on bad input. */
  runtimeStats: DelegationRuntimeStats | null
  attentionRequest: AttentionRequestSummary | null
  textPreview: string | null
  /** Durable run generation injected while reconstructing cold history. */
  generation: number | null
  /** Historical metadata is correlation seed, not fresher than a run DTO. */
  syntheticHistorical: boolean
}

function readOptionalString(value: unknown): string | null {
  return typeof value === "string" ? value : null
}

function readNonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null
}

/** Optional string|null field. Missing → omit; wrong type → invalid. */
function readOptionalNullableString(
  obj: Record<string, unknown>,
  key: string
): { ok: true; value?: string | null } | { ok: false } {
  if (!(key in obj)) return { ok: true }
  const value = obj[key]
  if (value === null) return { ok: true, value: null }
  if (typeof value === "string") return { ok: true, value }
  return { ok: false }
}

function readOptionalNullableCount(
  obj: Record<string, unknown>,
  key: string
): { ok: true; value?: number | null } | { ok: false } {
  if (!(key in obj)) return { ok: true }
  const value = obj[key]
  if (value === null) return { ok: true, value: null }
  if (typeof value === "number" && Number.isFinite(value)) {
    return { ok: true, value }
  }
  return { ok: false }
}

/**
 * Validate a single touched-file entry. Invalid entries fail the whole
 * runtime_stats object (never partially accept malformed rollups).
 */
function parseTouchedFile(value: unknown): DelegationTouchedFile | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  const obj = value as Record<string, unknown>
  if (typeof obj.path !== "string") return null
  if (typeof obj.outside_workspace !== "boolean") return null
  const additions = readOptionalNullableCount(obj, "additions")
  if (!additions.ok) return null
  const deletions = readOptionalNullableCount(obj, "deletions")
  if (!deletions.ok) return null
  const file: DelegationTouchedFile = {
    path: obj.path,
    outside_workspace: obj.outside_workspace,
  }
  if (additions.value !== undefined) file.additions = additions.value
  if (deletions.value !== undefined) file.deletions = deletions.value
  return file
}

/**
 * Shape-guard `runtime_stats` from meta/event JSON. Missing or malformed
 * objects become `null` (never throw; never invent zero counts).
 */
export function parseRuntimeStats(
  value: unknown
): DelegationRuntimeStats | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null
  }
  const obj = value as Record<string, unknown>
  if (typeof obj.started_at !== "string") return null
  if (
    typeof obj.tool_call_count !== "number" ||
    !Number.isFinite(obj.tool_call_count)
  ) {
    return null
  }
  if (
    typeof obj.edit_tool_call_count !== "number" ||
    !Number.isFinite(obj.edit_tool_call_count)
  ) {
    return null
  }
  if (!Array.isArray(obj.touched_files)) return null
  if (typeof obj.touched_files_truncated !== "boolean") return null
  if (typeof obj.line_counts_complete !== "boolean") return null

  const touchedFiles: DelegationTouchedFile[] = []
  for (const entry of obj.touched_files) {
    const file = parseTouchedFile(entry)
    if (!file) return null
    touchedFiles.push(file)
  }

  const finishedAt = readOptionalNullableString(obj, "finished_at")
  if (!finishedAt.ok) return null
  const additions = readOptionalNullableCount(obj, "additions")
  if (!additions.ok) return null
  const deletions = readOptionalNullableCount(obj, "deletions")
  if (!deletions.ok) return null

  const stats: DelegationRuntimeStats = {
    started_at: obj.started_at,
    tool_call_count: obj.tool_call_count,
    edit_tool_call_count: obj.edit_tool_call_count,
    touched_files: touchedFiles,
    touched_files_truncated: obj.touched_files_truncated,
    line_counts_complete: obj.line_counts_complete,
  }
  if (finishedAt.value !== undefined) stats.finished_at = finishedAt.value
  if (additions.value !== undefined) stats.additions = additions.value
  if (deletions.value !== undefined) stats.deletions = deletions.value
  return stats
}

/**
 * Shape-guard `attention_request` from meta/event JSON. Invalid → null.
 */
export function parseAttentionRequest(
  value: unknown
): AttentionRequestSummary | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null
  const obj = value as Record<string, unknown>
  if (typeof obj.request_id !== "string" || !obj.request_id) return null
  if (typeof obj.task_id !== "string" || !obj.task_id) return null
  if (typeof obj.message !== "string") return null
  if (typeof obj.created_at !== "string") return null
  return {
    request_id: obj.request_id,
    task_id: obj.task_id,
    message: obj.message,
    created_at: obj.created_at,
  }
}

/**
 * Extract delegation state from a `ToolCallState.meta` value. Returns
 * `null` when the meta doesn't carry the `codeg.delegation` sub-object —
 * caller falls back to the live binding / `parseInput` chain.
 *
 * The shape mirrors what the broker writes via `DelegationMetaWriter`
 * (`DelegationMetaSnapshot`): status, task/task ids, child ids, error_code,
 * text_preview, timestamps, optional runtime_stats / attention_request.
 * Invalid nested objects become null fields and never throw.
 */
export function parseDelegationMeta(
  meta: Record<string, unknown> | null | undefined
): ParsedMeta | null {
  if (!meta || typeof meta !== "object") return null
  const inner = meta["codeg.delegation"]
  if (!inner || typeof inner !== "object" || Array.isArray(inner)) return null
  const obj = inner as Record<string, unknown>
  const rawStatus = obj["status"]
  let status: DelegationStatus
  switch (rawStatus) {
    case "running":
    case "pending":
      status = "running"
      break
    case "completed":
    case "ok":
      status = "ok"
      break
    case "failed":
    case "err":
      status = "err"
      break
    default:
      return null
  }
  const child_connection_id = obj["child_connection_id"]
  const child_conversation_id = obj["child_conversation_id"]
  const agent_type = obj["agent_type"]
  const error_code = obj["error_code"]
  const task_preview = obj["task_preview"]
  const task_id = obj["task_id"]
  const generation = obj["generation"]
  return {
    status,
    agentType: coerceAgentType(agent_type),
    task:
      typeof task_preview === "string" && task_preview ? task_preview : null,
    taskId: readNonEmptyString(task_id),
    childConnectionId:
      typeof child_connection_id === "string" ? child_connection_id : null,
    childConversationId:
      typeof child_conversation_id === "number" ? child_conversation_id : null,
    errorCode: typeof error_code === "string" ? error_code : null,
    startedAt: readOptionalString(obj["started_at"]),
    finishedAt: readOptionalString(obj["finished_at"]),
    runtimeStats: parseRuntimeStats(obj["runtime_stats"]),
    attentionRequest: parseAttentionRequest(obj["attention_request"]),
    textPreview: readOptionalString(obj["text_preview"]),
    generation:
      typeof generation === "number" &&
      Number.isInteger(generation) &&
      generation > 0
        ? generation
        : null,
    syntheticHistorical: obj["synthetic_historical"] === true,
  }
}

const EMPTY_PARSED_INPUT: ParsedInput = {
  agentType: null,
  profileLabel: null,
  task: null,
  workingDir: null,
  workUnitKey: null,
  targetTaskId: null,
  replacesTaskId: null,
}

// Wrapper keys that hosts use to nest the actual tool arguments. JSON-RPC
// servers and various MCP relays will pack the call as `{name, arguments}`
// or `{params: {...}}`; some agents stash the args under a generic
// `input`/`payload` key alongside metadata; Cursor's MCP calls surface as
// `{providerIdentifier, toolName, args: {...}}`. Walked recursively (small
// depth cap) so any single layer of wrapping peels off without false
// positives on legitimate shallow fields. Mirrors `ARGS_WRAPPER_KEYS` in
// `acp/lifecycle.rs` — the two walkers must peel the same shapes.
const ARGS_WRAPPER_KEYS = [
  "arguments",
  "input",
  "params",
  "payload",
  "_meta",
  "args",
] as const

function findDelegationArgs(
  value: unknown,
  depth = 0
): Record<string, unknown> | null {
  if (depth > 4) return null
  if (value === null || value === undefined) return null
  // Some hosts double-encode the raw input (JSON-of-JSON). Recurse once
  // on the parsed inner value before giving up.
  if (typeof value === "string") {
    try {
      return findDelegationArgs(JSON.parse(value), depth + 1)
    } catch {
      return null
    }
  }
  if (typeof value !== "object" || Array.isArray(value)) return null
  const obj = value as Record<string, unknown>
  // Direct hit: this object has at least one of the delegation fields
  // declared on its top level.
  if (
    typeof obj.task === "string" ||
    typeof obj.agent_type === "string" ||
    typeof obj.profile_label === "string" ||
    typeof obj.working_dir === "string" ||
    typeof obj.work_unit_key === "string" ||
    typeof obj.task_id === "string" ||
    typeof obj.replaces_task_id === "string"
  ) {
    return obj
  }
  for (const key of ARGS_WRAPPER_KEYS) {
    const child = obj[key]
    if (child === undefined) continue
    const found = findDelegationArgs(child, depth + 1)
    if (found) return found
  }
  return null
}

/**
 * A content-free structural descriptor of a value: object keys (recursively,
 * depth- and width-capped), array lengths, and primitive *types* — never the
 * values themselves. This is exactly what diagnoses an unrecognized wire shape
 * (which keys did the host nest the args under?) without exposing any content.
 */
function describeShape(value: unknown, depth = 0): string {
  if (value === null) return "null"
  if (Array.isArray(value)) return `array(${value.length})`
  if (typeof value !== "object") return typeof value
  if (depth >= 3) return "object{…}"
  const obj = value as Record<string, unknown>
  const keys = Object.keys(obj)
  if (keys.length === 0) return "object{}"
  const shown = keys
    .slice(0, 20)
    .map((k) => `${k}: ${describeShape(obj[k], depth + 1)}`)
    .join(", ")
  return `object{ ${shown}${keys.length > 20 ? ", …" : ""} }`
}

// One-line debug breadcrumb. The walker covers the wrappers we know about
// (`arguments`, `input`, `params`, `payload`, `_meta`); if a non-empty raw
// input still doesn't yield delegation args, the host is using a shape we
// haven't accounted for. We log the unrecognized *shape* (keys + types, never
// values) so the next "task didn't show up" report is self-debugging — the
// wire shape lands in the user's devtools — without dumping the raw `task`
// text, `working_dir` path, or anything a user pasted into a prompt into the
// console.
function warnDelegationInputUnparseable(shape: string, reason: string): void {
  console.warn(
    `[delegation-card] could not extract delegation args (${reason}). shape=${shape}`
  )
}

export function parseInput(raw: string | null | undefined): ParsedInput {
  if (!raw || typeof raw !== "string") return EMPTY_PARSED_INPUT
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    warnDelegationInputUnparseable(
      `non-JSON(len=${raw.length})`,
      "JSON.parse threw"
    )
    return EMPTY_PARSED_INPUT
  }
  const obj = findDelegationArgs(parsed)
  if (!obj) {
    // An empty object is the EXPECTED shape on identity-less hosts (Cursor
    // announces every MCP call with raw_input "{}") — nothing to diagnose,
    // so don't spam the console on every render of those cards.
    const isEmptyObject =
      parsed !== null &&
      typeof parsed === "object" &&
      !Array.isArray(parsed) &&
      Object.keys(parsed as Record<string, unknown>).length === 0
    if (!isEmptyObject) {
      warnDelegationInputUnparseable(
        describeShape(parsed),
        "no known wrapper matched"
      )
    }
    return EMPTY_PARSED_INPUT
  }
  return {
    agentType: coerceAgentType(obj.agent_type),
    profileLabel:
      typeof obj.profile_label === "string" ? obj.profile_label : null,
    task: typeof obj.task === "string" ? obj.task : null,
    workingDir: typeof obj.working_dir === "string" ? obj.working_dir : null,
    workUnitKey: readNonEmptyString(obj.work_unit_key),
    targetTaskId: readNonEmptyString(obj.task_id),
    replacesTaskId: readNonEmptyString(obj.replaces_task_id),
  }
}

export function parseCancelDelegationReason(
  raw: string | null | undefined
): string | null {
  if (!raw || typeof raw !== "string") return null
  try {
    const obj = findDelegationArgs(JSON.parse(raw))
    return obj ? readNonEmptyString(obj.reason) : null
  } catch {
    return null
  }
}

/**
 * Parsed form of the parent `delegate_to_agent` tool output.
 *
 * Under ASYNC delegation the tool output is a *running ack* — the result
 * arrives later via the `delegation_completed` event / meta, NOT on the tool
 * output. So we must distinguish:
 *   - `ack`     — a running (or otherwise non-terminal) task: there is NO
 *                 result to render on the card yet.
 *   - `outcome` — a terminal result to render (a fast-complete ack where the
 *                 child finished during setup, or a legacy pre-async
 *                 synchronous result).
 * Returning `ack` — rather than letting the raw ack JSON fall through as an
 * "outcome" — is what stops the card from painting the ack as the result and
 * from prematurely flipping the status badge to "ok".
 *
 * `durationMs` is retained from non-negative wire `duration_ms` on terminal
 * reports so cold cards can fall back when `finishedAt - startedAt` is absent;
 * structured non-terminal reports carry `null`.
 */
export type ParsedToolOutput =
  | {
      kind: "ack"
      childConversationId: number | null
      durationMs?: number | null
      agentType?: AgentType | null
      errorCode?: string | null
    }
  | {
      kind: "outcome"
      text: string
      isError: boolean
      childConversationId: number | null
      durationMs: number | null
      agentType?: AgentType | null
      /**
       * Stable wire `error_code` / legacy `code` when present.
       * Never a call `correlation_id` — that token is transport-only.
       */
      errorCode?: string | null
    }

/**
 * A failed continuation can still echo the previous child conversation even
 * when admission failed before minting a new run. Without a current task id,
 * that child identity cannot safely scope lifecycle or grouping data.
 */
export function isUncorrelatedDelegationFailure(
  output: ParsedToolOutput | null,
  currentTaskId: string | null | undefined,
  options?: { syntheticHistorical?: boolean }
): boolean {
  if (options?.syntheticHistorical) return false
  return !currentTaskId && output?.kind === "outcome" && output.isError === true
}

/**
 * `DelegationTaskReport.error_code` for a refused `resume_delegation`
 * (`broker.rs::NOT_RESUMABLE_CODE`). Mirror of the Rust constant.
 */
export const NOT_RESUMABLE_CODE = "not_resumable"

/**
 * Opening words of the two `resume_delegation` messages that must stay
 * readable when the structured report is gone. `companion.rs::render_task_report`
 * puts the whole report — `error_code`, `child_conversation_id`, everything —
 * in `structuredContent` and renders only `message` as content text, and some
 * hosts keep only the text: OpenCode "drops the MCP `structuredContent`
 * entirely, so the human-readable lines ARE the whole record"
 * (`acp/connection.rs`, verified against opencode 1.18.23).
 *
 * The backend writes these prefixes deliberately for that case — see
 * `broker.rs::not_resumable_report` ("a message that opens with 'Not resumed'
 * — unambiguous against the ack even on hosts that only surface the content
 * text") and `resume_ack`. Both sides must keep spelling them the same way.
 */
const REFUSED_RESUME_TEXT = "Not resumed:"
const RESUMED_ACK_TEXT = "Delegation resumed"

/**
 * Whether a real `DelegationTaskReport` was recovered, as opposed to opaque
 * result text. `interpretReport` sets `agentType` on every branch — to `null`
 * when the report carried no agent — while generic text fallbacks omit it.
 */
function isStructuredReport(parsed: ParsedToolOutput | null): boolean {
  return (
    parsed != null && Object.prototype.hasOwnProperty.call(parsed, "agentType")
  )
}

/**
 * Does this result OPEN with one of the backend's markers?
 *
 * Anchored, never a substring search, because the text channel is not always
 * the backend's own message: `render_task_report` renders `text` in preference
 * to `message` for a `completed` report, and a resume whose child finished
 * during setup (`broker.rs`'s `Disposition::ChildTerminal`) reports exactly
 * that child's LLM-written output. A sub-agent that merely discusses
 * delegation would otherwise be read as a verdict about its own card.
 *
 * Checks the parsed outcome text as well as the raw string so a host envelope
 * around the message doesn't hide the marker.
 */
function opensWith(
  parsed: ParsedToolOutput | null,
  raw: string | null | undefined,
  marker: string
): boolean {
  const parsedText = parsed?.kind === "outcome" ? parsed.text : null
  return [parsedText, raw].some(
    (candidate) => candidate?.trimStart().startsWith(marker) ?? false
  )
}

/**
 * Whether a `resume_delegation` result is the broker REFUSING to resume.
 *
 * This cannot be read off `status`: a refusal deliberately reports the task's
 * ACTUAL state (`broker.rs::not_resumable_report`), so "already completed"
 * arrives as `status: "completed"` and "still running" as `status: "running"`
 * — indistinguishable from a real resume by status alone. Only `error_code`
 * separates them — or, where no structure survived, the message prefix.
 *
 * It matters because a refusal still carries `agent_type` and
 * `child_conversation_id`, which is otherwise exactly the evidence a resumed
 * sub-agent card runs on: without this check the card paints a live-looking
 * (or done-looking) sub-agent for a resume that never happened, and buries the
 * one thing the user needs — the "Not resumed: …" explanation.
 */
export function isRefusedResume(
  output?: string | null,
  errorText?: string | null
): boolean {
  const parsed = parseResumeResult(output, errorText)
  // Structure survived ⇒ it is the whole answer. Falling through to the text
  // would be reading a child's own output for a verdict about the call.
  if (isStructuredReport(parsed)) {
    return parsed?.errorCode === NOT_RESUMABLE_CODE
  }
  return (
    opensWith(parsed, output, REFUSED_RESUME_TEXT) ||
    opensWith(parsed, errorText, REFUSED_RESUME_TEXT)
  )
}

/**
 * Whether a `resume_delegation` result is the broker CONFIRMING the resume —
 * as opposed to refusing it, or reporting an unknown task
 * (`broker.rs::unknown_report`, which a foreign task id lands on).
 *
 * Used to corroborate a task-id binding lookup when the report itself named no
 * child conversation: on a host that drops `structuredContent` the ack's
 * `child_conversation_id` is gone, so the confirmation text is the only thing
 * left that distinguishes "this call really did revive that task" from "the
 * model named somebody else's task id".
 */
export function isAffirmedResume(
  output?: string | null,
  errorText?: string | null
): boolean {
  if (isRefusedResume(output, errorText)) return false
  const parsed = parseResumeResult(output, errorText)
  // With the report intact, naming a child IS the confirmation — and its
  // absence is what marks `unknown_report`. No need to read prose for it.
  if (isStructuredReport(parsed)) {
    return parsed?.childConversationId != null
  }
  return (
    opensWith(parsed, output, RESUMED_ACK_TEXT) ||
    opensWith(parsed, errorText, RESUMED_ACK_TEXT)
  )
}

function parseResumeResult(
  output?: string | null,
  errorText?: string | null
): ParsedToolOutput | null {
  return (
    (errorText ? parseToolOutput(errorText, true) : null) ??
    parseToolOutput(output)
  )
}

function readChildConversationId(obj: Record<string, unknown>): number | null {
  return typeof obj.child_conversation_id === "number"
    ? obj.child_conversation_id
    : null
}

/** Non-negative finite `duration_ms` only; invalid / negative → null. */
function readDurationMs(obj: Record<string, unknown>): number | null {
  const value = obj.duration_ms
  if (typeof value === "number" && Number.isFinite(value) && value >= 0) {
    return value
  }
  return null
}

/** Prefer `error_code` (task report); fall back to legacy outcome `code`. */
function readWireErrorCode(obj: Record<string, unknown>): string | null {
  if (typeof obj.error_code === "string" && obj.error_code.length > 0) {
    return obj.error_code
  }
  if (typeof obj.code === "string" && obj.code.length > 0) {
    return obj.code
  }
  return null
}

function outcomeResult(fields: {
  text: string
  isError: boolean
  childConversationId: number | null
  durationMs: number | null
  agentType?: AgentType | null
  errorCode?: string | null
}): ParsedToolOutput {
  return {
    kind: "outcome",
    text: fields.text,
    isError: fields.isError,
    childConversationId: fields.childConversationId,
    durationMs: fields.durationMs,
    ...(Object.prototype.hasOwnProperty.call(fields, "agentType")
      ? { agentType: fields.agentType ?? null }
      : {}),
    errorCode: fields.errorCode ?? null,
  }
}

/**
 * Interpret the broker's inner shape — the async `DelegationTaskReport`
 * (discriminated by `status`) or the legacy synchronous `DelegationOutcome`
 * (discriminated by `kind`). Returns null when neither discriminator is present
 * so the caller can fall through to other unwrapping strategies.
 */
function interpretReport(
  obj: Record<string, unknown>
): ParsedToolOutput | null {
  const childConversationId = readChildConversationId(obj)
  const durationMs = readDurationMs(obj)
  // `DelegationTaskReport.agent_type` (types.rs). For `delegate_to_agent` this
  // merely echoes the `agent_type` argument the card already parsed; it earns
  // its keep on `resume_delegation`, whose arguments are only
  // `{task_id, reason}` — the report is the ONLY place a reloaded resume card
  // can learn which agent it revived.
  const agentType = coerceAgentType(obj.agent_type)
  // Carried on EVERY variant, not just the failed ones: a refused resume pairs
  // `not_resumable` with the task's real status, so the code is the only thing
  // that distinguishes it from the report of a genuine resume. See
  // `isRefusedResume`.
  const errorCode = typeof obj.error_code === "string" ? obj.error_code : null
  const status = typeof obj.status === "string" ? obj.status : null
  if (status) {
    switch (status) {
      case "running":
      case "unknown":
        // No terminal result to show on the card — it's an ack.
        return {
          kind: "ack",
          childConversationId,
          durationMs: null,
          agentType,
          errorCode,
        }
      case "completed":
        return outcomeResult({
          text: typeof obj.text === "string" ? obj.text : "",
          isError: false,
          childConversationId,
          durationMs,
          agentType,
          errorCode,
        })
      case "failed":
      case "canceled": {
        const message = typeof obj.message === "string" ? obj.message : ""
        const code = readWireErrorCode(obj) ?? ""
        return outcomeResult({
          text: message || code || "Delegation failed.",
          isError: true,
          childConversationId,
          durationMs,
          agentType,
          errorCode: code || null,
        })
      }
      default:
        return {
          kind: "ack",
          childConversationId,
          durationMs: null,
          agentType,
          errorCode,
        }
    }
  }
  // Legacy synchronous outcome shape. These branches must set `errorCode` too
  // — `null` where there is none — because its ABSENCE is what
  // `isStructuredReport` reads as "no report survived, fall back to the text".
  // Leaving it off here would send a perfectly well-formed legacy result down
  // the text path, where a result that merely opens with "Not resumed:" would
  // be taken for a refusal.
  const kind = typeof obj.kind === "string" ? obj.kind : null
  if (kind === "ok") {
    return outcomeResult({
      text: typeof obj.text === "string" ? obj.text : "",
      isError: false,
      childConversationId,
      durationMs,
      agentType,
      errorCode: null,
    })
  }
  if (kind === "err") {
    const message = typeof obj.message === "string" ? obj.message : ""
    const code = readWireErrorCode(obj) ?? ""
    return outcomeResult({
      text: message || code || "Delegation failed.",
      isError: true,
      childConversationId,
      durationMs,
      agentType,
      errorCode: code || null,
    })
  }
  return null
}

/**
 * When an MCP `CallToolResult` lacks a usable `structuredContent`, the broker's
 * `DelegationTaskReport` may still be inlined in `content[0]` — either as a
 * structured `.json` object, or (Codex-style) as a JSON string in `.text`
 * (optionally wrapped, e.g. `"Wall time: N seconds\nOutput:\n<json>_"`).
 * Recognize it so a running ack yields `kind:"ack"` (not a premature "ok") and
 * its `child_conversation_id` is preserved for the "查看会话" affordance. Returns
 * null when no report can be recovered from the content array.
 */
function interpretMcpContentArray(
  obj: Record<string, unknown>
): ParsedToolOutput | null {
  if (!Array.isArray(obj.content)) return null
  const first = (obj.content as unknown[])[0]
  if (!first || typeof first !== "object" || Array.isArray(first)) return null
  const firstObj = first as Record<string, unknown>
  // Some hosts attach a structured `json` field on the content item.
  if (
    firstObj.json &&
    typeof firstObj.json === "object" &&
    !Array.isArray(firstObj.json)
  ) {
    const interpreted = interpretReport(
      firstObj.json as Record<string, unknown>
    )
    if (interpreted) return interpreted
  }
  // Codex-style: `content[0].text` is itself the serialized report.
  if (typeof firstObj.text === "string") {
    const embedded = extractEmbeddedJsonObject(firstObj.text)
    if (embedded) {
      const interpreted = interpretReport(embedded)
      if (interpreted) return interpreted
    }
  }
  return null
}

/** Whether `obj` is already one of the shapes [`parseToolOutput`] reads — a
 *  report (`status`), a legacy outcome (`kind`), or an MCP `CallToolResult`.
 *  Stops the host-envelope peel at a result that itself happens to carry a
 *  `result` key. (A child's arbitrary payload is guarded on the other side too:
 *  `peelMcpResultEnvelope` only ever peels TO a real `CallToolResult`.) */
function isResolvableDelegateResult(obj: Record<string, unknown>): boolean {
  return (
    typeof obj.status === "string" ||
    typeof obj.kind === "string" ||
    Array.isArray(obj.content) ||
    (typeof obj.structuredContent === "object" &&
      obj.structuredContent !== null &&
      !Array.isArray(obj.structuredContent))
  )
}

/**
 * Best-effort parse of the `delegate_to_agent` tool output into a
 * `ParsedToolOutput`. Mirrors the old unwrapping chain (direct JSON →
 * embedded-object scan → MCP `CallToolResult` envelope from
 * `companion.rs::render_task_report`) but yields the ack/outcome tagged union
 * so a running ack is never rendered as a result. `forceError` is set when
 * parsing the tool's `errorText` channel.
 *
 * A host envelope around the MCP result — Codex's live wire sends
 * `{result: <CallToolResult>, error: null}` — is peeled first, so the chain
 * below only ever faces the result itself.
 */
export function parseToolOutput(
  raw: string | null | undefined,
  forceError = false
): ParsedToolOutput | null {
  if (!raw || typeof raw !== "string") return null
  const trimmed = raw.trim()
  if (!trimmed) return null

  let obj: Record<string, unknown> | null = null
  try {
    const v = JSON.parse(trimmed) as unknown
    if (v && typeof v === "object" && !Array.isArray(v)) {
      obj = v as Record<string, unknown>
    } else {
      // Top-level primitive (string/number/bool): render directly.
      return outcomeResult({
        text: String(v),
        isError: forceError,
        childConversationId: null,
        durationMs: null,
      })
    }
  } catch {
    obj = extractEmbeddedJsonObject(trimmed)
  }

  if (!obj) {
    return outcomeResult({
      text: trimmed,
      isError: forceError,
      childConversationId: null,
      durationMs: null,
    })
  }

  const peel = peelMcpResultEnvelope(obj, isResolvableDelegateResult)
  obj = peel.obj

  // MCP `CallToolResult` envelope: `{ content: [...], structuredContent?, isError? }`.
  if (Array.isArray(obj.content)) {
    const inner =
      obj.structuredContent &&
      typeof obj.structuredContent === "object" &&
      !Array.isArray(obj.structuredContent)
        ? (obj.structuredContent as Record<string, unknown>)
        : null
    // 1. Prefer the full structured report.
    if (inner) {
      const interpreted = interpretReport(inner)
      if (interpreted) {
        // Honor an outer `isError: true` the host already decided.
        if (interpreted.kind === "outcome" && obj.isError === true) {
          return { ...interpreted, isError: true }
        }
        return interpreted
      }
    }
    // 2. No usable `structuredContent` (e.g. a host that surfaces only the
    //    content array): the report may be inlined in `content[0]`. Recognize a
    //    running ack here so it isn't mis-rendered as a terminal "ok" and its
    //    child id survives.
    const fromContent = interpretMcpContentArray(obj)
    if (fromContent) {
      if (fromContent.kind === "outcome" && obj.isError === true) {
        return { ...fromContent, isError: true }
      }
      return fromContent
    }
    // 3. Last resort: render `content[0].text` as opaque outcome text, carrying
    //    any child id from `structuredContent` if it was present but
    //    uninterpretable. Prefer a structured error_code when the envelope had
    //    one even if the report shape was not fully interpretable.
    const first = (obj.content as unknown[])[0]
    if (first && typeof first === "object" && !Array.isArray(first)) {
      const text = (first as Record<string, unknown>).text
      if (typeof text === "string") {
        return outcomeResult({
          text,
          isError: obj.isError === true || forceError,
          childConversationId: inner ? readChildConversationId(inner) : null,
          durationMs: null,
          errorCode: inner ? readWireErrorCode(inner) : null,
        })
      }
    }
  }

  const interpreted = interpretReport(obj)
  if (interpreted) {
    if (interpreted.kind === "outcome" && forceError) {
      return { ...interpreted, isError: true }
    }
    return interpreted
  }

  // A host envelope that failed outright carries no result to render — its own
  // error string is the whole story, and beats dumping the envelope JSON.
  if (peel.hostError) {
    return outcomeResult({
      text: peel.hostError,
      isError: true,
      childConversationId: null,
      durationMs: null,
    })
  }

  // Unrecognized JSON — pretty-print so we don't surface raw braces.
  // Do not promote unknown keys (including correlation_id) into errorCode.
  return outcomeResult({
    text: "```json\n" + JSON.stringify(obj, null, 2) + "\n```",
    isError: forceError,
    childConversationId: null,
    durationMs: null,
  })
}

/**
 * Surface the broker-minted `task_id` from the `delegate_to_agent` ack so the
 * user can correlate this delegation with the later `get_delegation_status` /
 * `cancel_delegation` cards. It is carried two ways: as
 * `structuredContent.task_id` (persisted / snapshot rows) and embedded in the
 * running-ack message text as `task_id=<id>` (the live wire forwards only the
 * `CallToolResult.content` text, not `structuredContent`). Returns null when no
 * id can be recovered. The structured form is tried first; the text scan is a
 * fallback so a stray `"task_id":...` inside JSON never beats the real field.
 */
export function parseDelegateTaskId(
  output: string | null | undefined,
  errorText: string | null | undefined
): string | null {
  for (const raw of [output, errorText]) {
    if (!raw || typeof raw !== "string") continue
    const trimmed = raw.trim()
    if (!trimmed) continue
    let obj: Record<string, unknown> | null = null
    try {
      const v = JSON.parse(trimmed) as unknown
      if (v && typeof v === "object" && !Array.isArray(v)) {
        obj = v as Record<string, unknown>
      }
    } catch {
      obj = extractEmbeddedJsonObject(trimmed)
    }
    if (obj) {
      // Peel Codex's live `{result, error}` wrapper so `structuredContent` is
      // reachable; a bare `task_id` at this level already ends the walk.
      const { obj: result } = peelMcpResultEnvelope(
        obj,
        (o) => typeof o.task_id === "string" || isResolvableDelegateResult(o)
      )
      const sc = result.structuredContent
      if (sc && typeof sc === "object" && !Array.isArray(sc)) {
        const id = (sc as Record<string, unknown>).task_id
        if (typeof id === "string" && id) return id
      }
      if (typeof result.task_id === "string" && result.task_id)
        return result.task_id
    }
    // Live wire: the ack message text embeds `task_id=<id>`.
    const m = trimmed.match(/task_id[=:]\s*"?([A-Za-z0-9][\w-]*)"?/)
    if (m) return m[1]
  }
  return null
}

function parseStructuredDelegateReport(
  raw: string | null | undefined
): Record<string, unknown> | null {
  if (!raw || typeof raw !== "string") return null
  const trimmed = raw.trim()
  if (!trimmed) return null

  let obj: Record<string, unknown> | null = null
  try {
    const value = JSON.parse(trimmed) as unknown
    if (value && typeof value === "object" && !Array.isArray(value)) {
      obj = value as Record<string, unknown>
    }
  } catch {
    obj = extractEmbeddedJsonObject(trimmed)
  }
  if (!obj) return null

  const { obj: result } = peelMcpResultEnvelope(obj, isResolvableDelegateResult)
  const structured = result.structuredContent
  if (
    structured &&
    typeof structured === "object" &&
    !Array.isArray(structured)
  ) {
    return structured as Record<string, unknown>
  }
  if (typeof result.status === "string" || typeof result.kind === "string") {
    return result
  }
  if (!Array.isArray(result.content)) return null
  const first = result.content[0]
  if (!first || typeof first !== "object" || Array.isArray(first)) return null
  const json = (first as Record<string, unknown>).json
  if (!json || typeof json !== "object" || Array.isArray(json)) return null
  return json as Record<string, unknown>
}

export function parseDelegateRunIdentity(
  input: DelegationRunIdentityInput
): DelegationRunIdentity {
  const parsedInput = parseInput(input.input)
  const parsedMeta = parseDelegationMeta(input.meta)
  const taskId =
    parseDelegateTaskId(input.output, input.errorText) ??
    parsedMeta?.taskId ??
    null

  const outputCandidates = [
    parseToolOutput(input.output),
    parseToolOutput(input.errorText, true),
  ]
  const structuredReports = [input.output, input.errorText]
    .map(parseStructuredDelegateReport)
    .filter((report): report is Record<string, unknown> => report !== null)
  const uncorrelatedFailure = outputCandidates.some((candidate) =>
    isUncorrelatedDelegationFailure(candidate, taskId, {
      syntheticHistorical: parsedMeta?.syntheticHistorical === true,
    })
  )
  const childConversationId = uncorrelatedFailure
    ? null
    : (outputCandidates.find(
        (candidate) => candidate?.childConversationId != null
      )?.childConversationId ??
      structuredReports
        .map((report) => readChildConversationId(report))
        .find((id) => id != null) ??
      parsedMeta?.childConversationId ??
      null)

  const linkedTaskIds: string[] = []
  const linked = new Set<string>()
  const addLinked = (value: unknown) => {
    const id = readNonEmptyString(value)
    if (!id || id === taskId || linked.has(id)) return
    linked.add(id)
    linkedTaskIds.push(id)
  }
  addLinked(parsedInput.targetTaskId)
  addLinked(parsedInput.replacesTaskId)
  for (const report of structuredReports) {
    addLinked(report.continued_from_task_id)
    addLinked(report.replaces_task_id)
  }

  return {
    parentConversationId: input.parentConversationId,
    parentToolUseId: input.parentToolUseId,
    workUnitKey: parsedInput.workUnitKey,
    taskId,
    childConversationId,
    linkedTaskIds,
  }
}

/**
 * Whether a (already normalized or raw) tool name denotes the multi-agent
 * delegation-dispatch companion tool. Matches initial and continued runs in
 * bare or host-prefixed forms.
 */
export function isDelegateToAgentToolName(name: string): boolean {
  const lower = name.toLowerCase()
  return (
    lower === "delegate_to_agent" ||
    lower === "continue_delegation" ||
    /[^a-z0-9](delegate_to_agent|continue_delegation)$/.test(lower)
  )
}

/**
 * Map durable child-conversation `delegation_task_status` into a card badge
 * terminal/running signal. Null/unknown → null (no contribution).
 */
export function badgeFromChildTaskStatus(
  taskStatus: "running" | "completed" | "failed" | "canceled" | null | undefined
): "running" | "ok" | "err" | null {
  switch (taskStatus) {
    case "completed":
      return "ok"
    case "failed":
    case "canceled":
      return "err"
    case "running":
      return "running"
    default:
      return null
  }
}

/**
 * Resolve the card status from the live binding / persisted meta / parsed tool
 * output, in priority order. Pure mirror of the resolution that used to live
 * inline in `DelegatedSubThread`.
 *
 *   waiting (child blocked on permission) > live binding > snapshot meta
 *   > terminal child projection > error channel / terminal tool outcome
 *   > running ack / running projection > output-available > starting
 *
 * Terminal child projection must win over a still-present parent **ack** so
 * cold recovery does not show a spinning badge against a finished summary.
 * Non-terminal projection must NOT block a terminal tool outcome.
 */
export function resolveDelegationStatus({
  binding,
  parsedMeta,
  toolOutput,
  state,
  errorText,
  childAwaitingPermission,
  childTaskStatus = null,
}: {
  binding: DelegationBinding | undefined
  parsedMeta: ParsedMeta | null
  toolOutput: ParsedToolOutput | null
  state: ToolCallState | undefined
  errorText: string | null | undefined
  childAwaitingPermission: boolean
  /**
   * Durable child summary status when binding/meta are absent
   * (`delegation_task_status` from the projection cache).
   */
  childTaskStatus?: "running" | "completed" | "failed" | "canceled" | null
}): DelegationCardStatus {
  // A child awaiting a permission decision is blocked until the user acts;
  // surface it over the plain running state so the card cues opening "查看会话".
  if (childAwaitingPermission) return "waiting"
  if (binding) {
    // Lifecycle status stays running/ok/err. Soft-watchdog observation only
    // refines a still-running card — never terminal, never invents a binding.
    if (binding.status === "running") {
      if (binding.observation === "waiting_input") return "waiting_input"
      if (binding.observation === "stalled") return "stalled"
      if (binding.observation === "active") return "active"
    }
    return binding.status
  }
  if (parsedMeta) return parsedMeta.status

  const fromProj = badgeFromChildTaskStatus(childTaskStatus)
  // Terminal projection outranks ack / non-terminal tool state.
  if (fromProj === "ok" || fromProj === "err") return fromProj

  if (state === "output-error" || errorText) return "err"
  // Terminal tool outcome outranks a still-running summary projection.
  if (toolOutput?.kind === "outcome") return toolOutput.isError ? "err" : "ok"
  // Async: the parent output is a running ack while the child runs — keep
  // "running" rather than letting output-available flip the badge to "ok".
  if (toolOutput?.kind === "ack") return "running"
  if (fromProj === "running") return "running"
  if (state === "output-available") return "ok"
  // No binding, no meta, parent tool call not yet terminal: the sub-agent
  // connection is still being set up. Flips the instant a binding, meta, or
  // terminal output arrives.
  return "starting"
}

/**
 * Title-first secondary line for delegation cards.
 * `formatConversationTitle(title).trim()` then fall through to task; empty → null.
 */
export function formatDelegationDisplaySecondary(
  title: string | null | undefined,
  task: string | null | undefined
): string | null {
  const formatted = formatConversationTitle(title).trim()
  if (formatted) return formatted
  if (typeof task === "string" && task.length > 0) return task
  return null
}

function parseTimestampMs(value: string | null): number | null {
  if (value == null || value === "") return null
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? ms : null
}

/**
 * Elapsed milliseconds for the operational line. Uses **lifecycle** status
 * (`running` | `ok` | `err`), not badge refinements (active/stalled/…).
 *
 * - running → `nowMs - startedAt` when started is valid
 * - terminal → `finishedAt - startedAt` when both valid; else `completedDurationMs`
 * - invalid / negative spans → null (never NaN)
 */
export function computeDelegationElapsedMs(args: {
  lifecycleStatus: "running" | "ok" | "err"
  startedAt: string | null
  finishedAt: string | null
  completedDurationMs: number | null
  nowMs: number
}): number | null {
  const { lifecycleStatus, startedAt, finishedAt, completedDurationMs, nowMs } =
    args

  if (lifecycleStatus === "running") {
    const startedMs = parseTimestampMs(startedAt)
    if (startedMs == null) return null
    const elapsed = nowMs - startedMs
    return elapsed >= 0 ? elapsed : null
  }

  // Terminal (ok | err): prefer finished - started, else broker duration.
  const startedMs = parseTimestampMs(startedAt)
  const finishedMs = parseTimestampMs(finishedAt)
  if (startedMs != null && finishedMs != null) {
    const elapsed = finishedMs - startedMs
    if (elapsed >= 0) return elapsed
    // Negative span is invalid — fall through to completedDurationMs.
  }
  if (
    typeof completedDurationMs === "number" &&
    Number.isFinite(completedDurationMs) &&
    completedDurationMs >= 0
  ) {
    return completedDurationMs
  }
  return null
}

/**
 * Compact edit-rollup view model for the operational line.
 * Paths win over edit-call counts; null stats → omit (tool count omitted separately).
 */
export type EditRollupViewModel =
  | {
      mode: "files"
      fileCount: number
      fileCountTruncated: boolean
      additions: number | null
      deletions: number | null
      showLineTotals: boolean
    }
  | { mode: "editCalls"; editCallCount: number }
  | { mode: "omit" }

export function buildEditRollupViewModel(
  stats: DelegationRuntimeStats | null
): EditRollupViewModel {
  if (!stats) return { mode: "omit" }

  if (stats.touched_files.length > 0) {
    const additions =
      stats.additions === undefined || stats.additions === null
        ? null
        : stats.additions
    const deletions =
      stats.deletions === undefined || stats.deletions === null
        ? null
        : stats.deletions
    const showLineTotals =
      stats.line_counts_complete && additions != null && deletions != null
    return {
      mode: "files",
      fileCount: stats.touched_files.length,
      fileCountTruncated: stats.touched_files_truncated,
      additions,
      deletions,
      showLineTotals,
    }
  }

  if (stats.edit_tool_call_count > 0) {
    return {
      mode: "editCalls",
      editCallCount: stats.edit_tool_call_count,
    }
  }

  return { mode: "omit" }
}
