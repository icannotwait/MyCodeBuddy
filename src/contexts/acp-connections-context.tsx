"use client"

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  type ReactNode,
} from "react"
import { useTranslations } from "next-intl"
import { getEventStream } from "@/lib/platform"
import { getTransport, isRemoteDesktopMode } from "@/lib/transport"
import { subscribeDesktopAcpEvents } from "@/lib/transport/desktop-acp-events"
import { EventIngestor } from "@/lib/acp/event-ingestor"
import {
  getStreamingPerformanceConfig,
  initializeStreamingPerformanceConfig,
  __resetStreamingPerformanceConfigForTests,
} from "@/lib/acp/streaming-performance-config"
import type { LiveTranscriptFrameSink } from "@/stores/live-transcript-store"
import type {
  AttachErrorCode,
  AttachHandlers,
  EventStreamSubscription,
} from "@/lib/transport/types"
import { randomUUID } from "@/lib/utils"
import { inferLiveToolName } from "@/lib/tool-call-normalization"
import {
  accumulateRequestUsage,
  EMPTY_REQUEST_USAGE,
  resolveRequestUsageSample,
} from "@/lib/request-usage-speed"
import {
  createRequestTokenEstimator,
  discardEstimatedRequest,
  hasUnsettledEstimatedRequest,
  observeEstimatedDelta,
  observeEstimatedSnapshot,
  replaceEstimatorFromHydration,
  settleEstimatedRequest,
  type EstimatorHydrationSeed,
  type EstimatorObservation,
  type RequestTokenEstimatorState,
} from "@/lib/request-token-estimator"
import {
  aliasRequestUsageIds,
  publishRequestUsage,
} from "@/lib/request-usage-live"
import {
  continuationFailureI18nKey,
  isContinuationFailureCode,
} from "@/lib/continuation-waiting"
import {
  acpConnect,
  acpConnectOrAttach,
  acpGetAgentStatus,
  acpPrompt,
  acpSetMode,
  acpSetConfigOption,
  acpGoalControl,
  acpCancel,
  acpCancelQueuedPrompt,
  acpRespondPermission,
  acpAnswerQuestion,
  acpAnswerPlanApproval,
  acpDisconnect,
  acpReleaseLease,
  acpTerminateSharedSession,
  acpTouchConnection,
  acpGetSessionSnapshot,
  acpFindConnectionForConversation,
  acpGetDesktopDeliveryCapabilities,
  acpGetEventMetrics,
  acpReplayStreamingPerfFixture,
  getSystemRenderingSettings,
  type AcpDisconnectLease,
  type AcpDisconnectOrigin,
} from "@/lib/api"
import {
  getSharedClientIdentity,
  newSharedRequestId,
} from "@/lib/acp/shared-session-client"
import {
  getTransferFence,
  isFrontendDisconnectSuppressed,
  isTransferringOut,
  leaseArgsForDisconnect,
  markMainReleased,
  registerPopoutAcpBridge,
} from "@/lib/conversation-popout-acp-bridge"
import {
  streamingPerfRecorder,
  type PerfRateProfile,
} from "@/lib/perf/streaming-perf-recorder"
import {
  downloadStreamingPerfReport,
  extractWebviewVersion,
  GROK_RICH_V1_EXPECTED_EVENTS,
  GROK_RICH_V1_EXPECTED_TEXT_SHA256,
  legacyStreamingPerformanceFlags,
  type StreamingPerfReport,
} from "@/lib/perf/streaming-perf-report"
import { denormalizeSnapshot } from "@/lib/snapshot-denormalize"
import {
  filterSessionConfigOptions,
  isHiddenSessionConfigOptionId,
} from "@/lib/session-config-filter"
import { buildDelegationSeedEnvelopes } from "@/lib/delegation-seed"
import { reduceToolWatchdogProjection as reduceToolWatchdogProjectionMap } from "@/lib/tool-watchdog-projection"
import { isNewerDiagnostic as isNewerDiagnosticProjection } from "@/lib/tool-watchdog-diagnostic"
import {
  extractAppCommandError,
  toLocalizedErrorMessage,
} from "@/lib/app-error"
import {
  CONNECTION_NOT_FOUND_CODE,
  isConnectionBusy,
  isConnectionGoneError,
} from "@/lib/connection-teardown"
import {
  completeLiveTranscriptTurn,
  enterOwnerPreserve,
  getConversationIdByExternalIdFromStore,
  getUserStopFenceToken,
  isStaleUserStopEnvelope,
  isUserStopNoCoordinatorCompletion,
  markUserStopNoCoordinatorCompletion,
  noteUserStopTurnOwnership,
  resolveRuntimeConversationIdForOwnership,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import type {
  AgentType,
  AcpAgentStatus,
  AcpEvent,
  ActiveDelegationState,
  AvailableCommandInfo,
  ConfigStaleKind,
  ConnectionStatus,
  ConversationConnectionInfo,
  DelegationRoutePolicy,
  DelegationRouteSnapshot,
  EventEnvelope,
  PlanEntryInfo,
  PermissionOptionInfo,
  PendingQuestionState,
  QuestionAnswer,
  PendingPlanApprovalState,
  PlanApprovalAnswer,
  SessionConfigOptionInfo,
  SessionFailureRecord,
  SessionModeStateInfo,
  SessionUsageUpdateInfo,
  PromptCapabilitiesInfo,
  AcceptedConnectionFrame,
  AcceptedEventFrame,
  DesktopAcpEventBatch,
  DesktopDeliveryCapabilities,
  DesktopDeliveryFailure,
  SequenceGap,
  AcpPromptContext,
  PromptInputBlock,
  ToolCallImageWire,
  TurnOutcome,
  UserMessageBlock,
  PromptEnqueueResult,
  AcpConnectOrAttachResponse,
  SharedSessionPhase,
} from "@/lib/types"
import {
  dismissSessionFailures,
  hasSettleableRetryIncident,
  mergeSessionFailures,
  settleSessionFailures,
  upsertSessionFailure,
  type SessionFailureSettleScope,
} from "@/lib/session-failures"
import type {
  SharedActiveTurn,
  SharedQueuedPrompt,
  SharedSessionPhaseView,
} from "@/lib/snapshot-denormalize"
import { getAgentLabel } from "@/lib/custom-agents"
import {
  CONNECTION_IDLE_TIMEOUT_MS,
  CONNECTION_KEEPALIVE_INTERVAL_MS,
  IDLE_SWEEP_INTERVAL_MS,
} from "@/lib/constants"
import { sendSystemNotification } from "@/lib/notification"
import { primeNotificationSoundOutput } from "@/lib/notification-sound"
import {
  getSavedPrefsForConnect,
  saveModePreference,
  saveConfigPreference,
} from "@/lib/selector-prefs-storage"
import { useAlertContext, type AlertAction } from "@/contexts/alert-context"
import { useActiveFolder } from "@/contexts/active-folder-context"

// ── Shared types (re-exported for consumers) ──

/** ACP extensibility metadata attached to tool calls. */
export type ToolCallMeta = Record<string, unknown> | null

/**
 * An image attached to a tool call (e.g. codex-acp v0.14+ image generation).
 * Re-exports the wire-level `ToolCallImageWire` from `@/lib/types` so that
 * snapshot, live `tool_call(_update)` events, and `ToolCallInfo` share one
 * shape. `data` is base64 (potentially multi-MB), `mime_type` defaults to
 * `image/png` when the agent omits it, `uri` is the on-disk path when the
 * agent persisted the asset (e.g. codex's `~/.codex/generated_images/...`).
 */
export type ToolCallImage = ToolCallImageWire

export interface ToolCallInfo {
  tool_call_id: string
  title: string
  kind: string
  status: string
  content: string | null
  raw_input: string | null
  raw_output_chunks: string[]
  raw_output_total_bytes: number
  locations: unknown
  meta: ToolCallMeta
  /**
   * Replace-on-update: a fresh ToolCallUpdate carrying images replaces this
   * vec; an absent images field preserves the prior value. Empty array
   * means "no images on this tool call". Persisted via snapshot so a
   * frontend reconnecting mid-turn or after refresh sees the same image.
   */
  images: ToolCallImage[]
}

export interface PendingPermission {
  request_id: string
  tool_call: unknown
  options: PermissionOptionInfo[]
  /** Requests queued behind this card (only one shows at a time). */
  queued?: number
}

/** In-flight user prompt carried on a connection (from a `user_message` event
 *  or a snapshot's `pending_user_message`). Mirrored into the runtime as a
 *  synthesized user turn for cross-client VIEWERS so they see the sender's
 *  message (the sender renders its own optimistic turn and ignores it). */
export interface PendingUserMessage {
  messageId: string
  blocks: UserMessageBlock[]
}

export interface PendingQuestion {
  tool_call_id: string
  question: string
}

export interface ClaudeApiRetryState {
  sessionId: string
  attempt: number | null
  maxRetries: number | null
  error: string | null
  errorStatus: number | null
  retryDelayMs: number | null
}

export type LiveContentBlock =
  | { type: "text"; text: string; parentToolUseId?: string }
  | { type: "thinking"; text: string; parentToolUseId?: string }
  | { type: "plan"; entries: PlanEntryInfo[] }
  | { type: "tool_call"; info: ToolCallInfo }

export interface LiveMessage {
  id: string
  role: "assistant" | "tool"
  content: LiveContentBlock[]
  startedAt: number
}

// ── Per-connection state ──

export interface ConnectionState {
  connectionId: string
  contextKey: string
  agentType: AgentType
  workingDir: string | null
  status: ConnectionStatus
  promptCapabilities: PromptCapabilitiesInfo
  supportsFork: boolean
  selectorsReady: boolean
  sessionId: string | null
  modes: SessionModeStateInfo | null
  configOptions: SessionConfigOptionInfo[] | null
  availableCommands: AvailableCommandInfo[] | null
  usage: SessionUsageUpdateInfo | null
  requestUsage?: import("@/lib/request-usage-speed").RequestUsageSnapshot
  requestEstimator?: import("@/lib/request-token-estimator").RequestTokenEstimatorState
  generationClockStartedAt?: number | null
  liveMessage: LiveMessage | null
  pendingPermission: PendingPermission | null
  /** In-flight user prompt for the current turn — set from a `user_message`
   *  event or a snapshot's `pending_user_message`. A VIEWER mirrors this into
   *  the runtime as a synthesized user turn; `null` outside an active turn. */
  pendingUserMessage: PendingUserMessage | null
  pendingQuestion: PendingQuestion | null
  /** Awaiting-answer multiple-choice `ask_user_question` (the codeg-mcp blocking
   *  tool). Set from a `question_request` event or a snapshot's
   *  `pending_question`; cleared on `question_resolved` or turn end. Distinct
   *  from the free-text `pendingQuestion` above. */
  pendingAskQuestion: PendingQuestionState | null
  /** Awaiting-decision Grok `exit_plan_mode` approval (the plan the agent is
   *  blocked on). Set from a `plan_approval_request` event or a snapshot's
   *  `pending_plan_approval`; cleared on `plan_approval_resolved` or turn end. */
  pendingPlanApproval: PendingPlanApprovalState | null
  claudeApiRetry: ClaudeApiRetryState | null
  /** AIR typed session failure table (see `lib/session-failures.ts` for the
   *  merge/settle contract). Retained resolved — entries double as per-id
   *  revision watermarks; the banner splits active from resolved itself. */
  sessionFailures: SessionFailureRecord[]
  error: string | null
  /**
   * Recoverable attach-protocol error. The agent process is still alive.
   * Cleared on a successful snapshot hydrate or when the user/WS retries.
   */
  attachError?: { code: AttachErrorCode; retryable: boolean } | null
  /**
   * Set when the agent rejected `session/load` non-recoverably because a
   * historical session cannot be resumed.
   * Distinct from `error` because the UI surfaces it inline in the message
   * list with reload / new-conversation actions, instead of as a toast.
   * Cleared on the next CONNECTION_CREATED for the same key, or by
   * CLEAR_ACP_LOAD_ERROR (Reload button).
   */
  loadError: string | null
  /** Stable backend code for `loadError`, used to choose valid recovery actions. */
  loadErrorCode: string | null
  /**
   * Highest envelope.seq applied to this connection. Used to dedup the
   * live `acp://event` stream against the snapshot endpoint: a
   * HYDRATE_FROM_SNAPSHOT sets this to snapshot.event_seq, and incoming
   * envelopes with seq <= lastAppliedSeq are dropped as duplicates.
   * Phase 3b initialises to 0 on CONNECTION_CREATED.
   */
  lastAppliedSeq: number
  /**
   * True when this entry was synthesized for a backend connection that
   * was spawned by the delegation broker (not via a user-driven
   * `connect()`). Such entries piggy-back on the same reducer pipeline
   * as real connections so the child's live message, tool calls, and
   * permission requests reach the UI, but they MUST be hidden from any
   * user-facing connection list / picker, and they MUST NOT be reaped
   * by the idle sweep — their lifetime is governed by the parent's
   * delegation_started / delegation_completed events.
   */
  isDelegationChild: boolean
  /**
   * For delegation-child entries: the parent's `tool_use_id` that owns
   * this child. The DelegatedSubThread component uses this to resolve
   * the child connection state from its parent-side identifier. Null
   * for non-delegation connections.
   */
  parentToolUseId: string | null
  /**
   * For delegation-child entries: the parent connection that spawned
   * this child. Carried for diagnostic / cascade-cancel purposes; not
   * required for the rendering path. Null for non-delegation
   * connections.
   */
  parentConnectionId: string | null
  /**
   * True when this client did NOT spawn the backend connection but attached to
   * one another client already owns (cross-client live streaming, discovered
   * via `acp_find_connection_for_conversation`). A viewer is a NON-OWNING,
   * co-controlling client: it streams the same turn and MAY drive the shared
   * agent (sendPrompt/cancel target the owner's connection, serialized
   * server-side by its prompt_lock; turn-level concurrency rejection is a
   * tracked follow-up). The one hard invariant: on teardown a viewer MUST
   * detach (drop its attach subscription / reverse-map entry) and MUST NOT
   * `acpDisconnect` — that would kill the agent for the owner. Like
   * `isDelegationChild`, viewers are skipped by the idle sweep's disconnect
   * path. Distinct from `isDelegationChild` (broker-owned child bookkeeping);
   * a plain viewer is the lighter cousin with no delegation state.
   */
  isViewer: boolean
  /**
   * True when the agent's effective settings changed after this session was
   * spawned, so the running process is still on its launch-time config (env
   * vars / model provider / native config). Set from a `session_config_stale`
   * event or a hydrated snapshot; cleared when the user reverts the setting or
   * restarts the session via `reapplyConfig`. Drives the per-conversation
   * "restart to apply" banner.
   */
  configStale: boolean
  /** Which settings surface drifted, for the banner's wording. `null` when not stale. */
  configStaleKind: ConfigStaleKind | null
  /**
   * Launched-but-unresolved background tasks (async sub-agents / background
   * shells) on this connection, mirrored from `background_activity` events
   * (authoritative accounting lives in the backend transcript watcher).
   * Non-zero exempts the connection from the frontend idle sweep — killing
   * the connection kills the agent CLI and the background work with it —
   * and drives the "background tasks running" chip.
   */
  backgroundOutstanding: number
  /**
   * Epoch ms of the most recent `background_activity` event that settled a
   * task, cleared when the follow-up overlay turns start arriving. Bridges
   * the otherwise-blank gap between "N tasks running" disappearing and the
   * agent's reaction to the results surfacing (model time-to-first-block +
   * the transcript's record granularity — typically 3–15s): the chip shows a
   * "syncing results" state while this is set. Client-local UI state (not in
   * the snapshot); the chip additionally expires it from display after a
   * fixed window so a killed CLI can't strand the indicator.
   */
  backgroundSettleSyncingSince: number | null
  /**
   * Tool-call context observed OUT-OF-TURN (status !== "prompting"), kept
   * ONLY so a background permission request can still render its command/
   * diff details. Out-of-turn wire tool events are barred from `liveMessage`
   * (the transcript overlay renders that content), but the permission dialog
   * enriches from the live tool registry — this small bounded map
   * (`OUT_OF_TURN_TOOL_CALL_CAP` newest entries) is that registry's
   * out-of-turn stand-in. Cleared when the next prompting turn starts.
   * `null` when empty (the common case allocates nothing).
   */
  outOfTurnToolCalls: ReadonlyMap<string, ToolCallInfo> | null
  /**
   * Client-local: the user dismissed (X) the stale banner for the CURRENT
   * drift. Hides the banner without touching the underlying `configStale`
   * state. Reset to `false` whenever a fresh `session_config_stale` arrives (a
   * new change re-shows the banner) and on a new connection. Never sourced from
   * the snapshot — dismissal is per-client UI state.
   */
  configStaleDismissed: boolean
  /**
   * Authoritative route snapshot from the backend (HYDRATE / availability
   * events). Optional so older direct test fixtures remain valid; production
   * constructors set it explicitly (`null` until snapshot arrives).
   */
  delegationRoute?: DelegationRouteSnapshot | null
  /**
   * Conversation id used for the winning connect request (viewer discovery +
   * reapply). Optional for older fixtures.
   */
  conversationId?: number | null
  /**
   * Route override supplied at connect time (reused by reapplyConfig). Optional
   * for older fixtures.
   */
  delegationRouteOverride?: DelegationRoutePolicy | null
  /**
   * Durable continuation waiting projection for this connection's conversation.
   * Independent of `status` and turn_in_flight. `null` when not waiting.
   * Optional on older fixtures; production constructors always set it.
   */
  waitingForSubagents?:
    | import("@/lib/types").ContinuationWaitingProjection
    | null
  /**
   * Currently actionable tool-watchdog projections keyed by `lease_id`.
   * Reduced from snapshot hydrate + live `tool_watchdog_changed` events.
   * Optional on older fixtures; production constructors always set `{}`.
   */
  toolWatchdogProjections?: Record<
    string,
    import("@/lib/types").ToolWatchdogProjection
  >
  /**
   * Per-lease max projection version seen (including after terminal remove).
   * Tombstones block late lower-version Cancelling from resurrecting banners.
   * Optional on older fixtures; production constructors always set `{}`.
   */
  toolWatchdogMaxVersions?: Record<string, number>
  /**
   * Most recent tool-watchdog projection observed on this connection (includes
   * terminal timed_out/cleared). Session details reads secret-safe fields only.
   * Optional on older fixtures.
   */
  lastToolWatchdogDiagnostic?:
    | import("@/lib/types").ToolWatchdogProjection
    | null
  /**
   * Pop-out ownership lease (detached claim). Used so disconnect paths can
   * prefer incarnation-aware teardown and avoid killing a reclaimed main
   * session after reverse rebind. Optional for older fixtures / main-window
   * connects that never rebound.
   */
  ownershipGeneration?: number | null
  ownerOperationId?: string | null
  ownerWindowLabel?: string | null
  /** Server-broker lease and authoritative queue projection. */
  sharedSession: SharedConnectionState | null
}

export interface SharedConnectionState {
  generation: number
  leaseId: string
  leaseExpiresAt: string
  /** Client-local idempotency key; never serialized into snapshots or events. */
  connectRequestId: string
  phase: SharedSessionPhaseView
  queue: SharedQueuedPrompt[]
  activeTurn: SharedActiveTurn | null
}

/** Owner spawn path vs attach-only observation of an existing broker ACP. */
export type ConnectionIntent = "own_or_observe" | "observe_existing"

type ConnectRequest = {
  agentType: AgentType
  workingDir?: string
  sessionId?: string
  // Persisted conversation id (when known) — drives the cross-client viewer
  // discovery gate in connect() and is sent to acpConnect.
  conversationId?: number
  // Draft/session route override for managed agents (null = inherit global).
  delegationRouteOverride?: DelegationRoutePolicy | null
  /** Detached cold-connect incarnation (pop-out operation id). */
  ownerOperationId?: string | null
  /** Stable for an attach/reconnect incarnation; broker idempotency key. */
  sharedRequestId?: string
  /** Explicit retry fence for a cleanup-complete failed shared generation. */
  retryFailedGeneration?: number
  /** Cursor carried from a terminal shared detach into broker reattachment. */
  sharedReconnect?: { generation: number; sinceSeq: number }
  intent: ConnectionIntent
  /** When true, poll discovery on the full delay schedule (task_running). */
  retryObserverDiscovery: boolean
}

function sameConnectRequest(a: ConnectRequest, b: ConnectRequest) {
  return (
    a.agentType === b.agentType &&
    (a.workingDir ?? null) === (b.workingDir ?? null) &&
    (a.sessionId ?? null) === (b.sessionId ?? null) &&
    (a.conversationId ?? null) === (b.conversationId ?? null) &&
    (a.delegationRouteOverride ?? null) ===
      (b.delegationRouteOverride ?? null) &&
    (a.ownerOperationId ?? null) === (b.ownerOperationId ?? null) &&
    (a.sharedRequestId ?? null) === (b.sharedRequestId ?? null) &&
    (a.retryFailedGeneration ?? null) === (b.retryFailedGeneration ?? null) &&
    (a.sharedReconnect?.generation ?? null) ===
      (b.sharedReconnect?.generation ?? null) &&
    (a.sharedReconnect?.sinceSeq ?? null) ===
      (b.sharedReconnect?.sinceSeq ?? null) &&
    a.intent === b.intent &&
    a.retryObserverDiscovery === b.retryObserverDiscovery
  )
}

/**
 * Classify observer/handoff discovery failures (Amendment 21).
 * Retryable: transport timeout, network reset, HTTP 5xx, temporary "not ready".
 * Non-retryable: auth/401/403, permanent not-found, malformed payload,
 * explicit protocol permanent errors. Auth never spins.
 */
export function isRetryableObserverDiscoveryError(error: unknown): boolean {
  const status = readErrorHttpStatus(error)
  if (status != null) {
    if (status === 401 || status === 403) return false
    if (status === 404) return false
    if (status >= 500 && status <= 599) return true
    if (status >= 400 && status < 500) return false
  }

  const appError = extractAppCommandError(error)
  if (appError) {
    const code = appError.code.toLowerCase()
    if (
      code === "unauthorized" ||
      code === "forbidden" ||
      code === "auth_required" ||
      code === "authentication_failed" ||
      code === "http_401" ||
      code === "http_403" ||
      code.includes("auth")
    ) {
      return false
    }
    if (
      code === "not_found" ||
      code === "conversation_not_found" ||
      code === "permanent_not_found" ||
      code === "http_404"
    ) {
      return false
    }
    if (
      code === "malformed" ||
      code === "invalid_payload" ||
      code === "malformed_payload" ||
      code === "protocol_error" ||
      code === "permanent_error"
    ) {
      return false
    }
    if (
      code === "timeout" ||
      code === "request_timeout" ||
      code === "network_error" ||
      code === "network_reset" ||
      code === "not_ready" ||
      code === "temporarily_unavailable" ||
      code === "service_unavailable" ||
      code === "internal_error" ||
      /^http_5\d\d$/.test(code)
    ) {
      return true
    }
  }

  const message = normalizeErrorMessage(error).toLowerCase()
  if (
    /\b401\b|\b403\b|unauthorized|forbidden|authentication|not authorized/.test(
      message
    )
  ) {
    return false
  }
  if (
    /malformed|invalid payload|protocol (permanent |)error|permanent not.?found/.test(
      message
    )
  ) {
    return false
  }
  // Permanent conversation not-found — but not "not ready".
  if (
    /not found|conversation.*missing|no such conversation/.test(message) &&
    !/not ready/.test(message)
  ) {
    return false
  }
  if (
    /timeout|timed out|network|econnreset|econnrefused|socket hang up|5\d\d|not ready|temporarily|service unavailable/.test(
      message
    )
  ) {
    return true
  }

  // Unknown errors: fail closed (do not spin discovery).
  return false
}

function readErrorHttpStatus(error: unknown): number | null {
  if (!error || typeof error !== "object") return null
  const obj = error as Record<string, unknown>
  if (typeof obj.status === "number" && Number.isFinite(obj.status)) {
    return obj.status
  }
  if (typeof obj.statusCode === "number" && Number.isFinite(obj.statusCode)) {
    return obj.statusCode
  }
  if (typeof obj.httpStatus === "number" && Number.isFinite(obj.httpStatus)) {
    return obj.httpStatus
  }
  // Nested transport shapes: `{ response: { status } }`, `{ cause: { status } }`.
  for (const nestedKey of ["response", "cause", "error"] as const) {
    const nested = obj[nestedKey]
    if (!nested || typeof nested !== "object") continue
    const n = nested as Record<string, unknown>
    if (typeof n.status === "number" && Number.isFinite(n.status)) {
      return n.status
    }
    if (typeof n.statusCode === "number" && Number.isFinite(n.statusCode)) {
      return n.statusCode
    }
  }
  if (typeof obj.code === "string") {
    const fromCode = obj.code.match(/^http[_-]?(\d{3})$/i)
    if (fromCode) return Number(fromCode[1])
  }
  return null
}

/**
 * Fail-closed validation for observer/handoff discovery payloads.
 * Missing/empty connection_id or non-finite event_seq is treated as malformed
 * (non-retryable) so we never attach/spawn on garbage identity.
 */
export function isValidConversationConnectionInfo(
  value: unknown
): value is ConversationConnectionInfo {
  if (!value || typeof value !== "object") return false
  const obj = value as Record<string, unknown>
  return (
    typeof obj.connection_id === "string" &&
    obj.connection_id.length > 0 &&
    typeof obj.event_seq === "number" &&
    Number.isFinite(obj.event_seq)
  )
}

/** Fixed observer discovery delays (ms). Never falls through to acpConnect. */
const OBSERVER_DISCOVERY_DELAYS_MS = [0, 300, 700, 1500, 2500] as const

// ── Reducer actions ──

type EstimatorActionContext = {
  receivedAt: number
}

type ToolEstimatorActionContext = EstimatorActionContext & {
  raw_input_is_model_authored?: boolean
}

type Action =
  | {
      type: "CONNECTION_CREATED"
      contextKey: string
      connectionId: string
      agentType: AgentType
      workingDir: string | null
      // Set when attaching to a connection another client owns (viewer).
      // Defaults to false (owner) when omitted.
      isViewer?: boolean
      conversationId?: number | null
      delegationRouteOverride?: DelegationRoutePolicy | null
      /** Pop-out claim lease fields (detached owner). */
      ownershipGeneration?: number | null
      ownerOperationId?: string | null
      ownerWindowLabel?: string | null
      sharedSession?: SharedConnectionState | null
    }
  | {
      /** In-place lease refresh after reverse while main still holds the conn. */
      type: "OWNERSHIP_LEASE_UPDATED"
      contextKey: string
      ownershipGeneration?: number | null
      ownerOperationId?: string | null
      ownerWindowLabel?: string | null
    }
  | {
      type: "DELEGATION_ROUTE_AVAILABILITY"
      contextKey: string
      available: boolean
    }
  | {
      type: "CONTINUATION_WAITING_CHANGED"
      contextKey: string
      waiting: import("@/lib/types").ContinuationWaitingProjection | null
    }
  | {
      type: "TOOL_WATCHDOG_CHANGED"
      contextKey: string
      projection: import("@/lib/types").ToolWatchdogProjection
    }
  | {
      type: "HYDRATE_FROM_SNAPSHOT"
      contextKey: string
      patch: import("@/lib/snapshot-denormalize").SnapshotPatch
    }
  | {
      type: "SHARED_SESSION_UPDATED"
      contextKey: string
      generation: number
      update: Partial<
        Pick<
          SharedConnectionState,
          "phase" | "queue" | "activeTurn" | "leaseExpiresAt"
        >
      >
    }
  | {
      type: "SHARED_SESSION_EVENT"
      contextKey: string
      event: Extract<
        AcpEvent,
        | { type: "shared_session_phase_changed" }
        | { type: "prompt_queued" }
        | { type: "prompt_queue_item_cancelled" }
        | { type: "prompt_dispatch_started" }
        | { type: "prompt_queue_item_failed" }
        | { type: "prompt_queue_depth_changed" }
        | { type: "shared_turn_settled" }
      > & { seq: number }
    }
  | {
      type: "DISMISS_FAILED_SHARED_PROMPT"
      contextKey: string
      queueItemId: string
    }
  | { type: "CONNECTION_REMOVED"; contextKey: string }
  | { type: "REMOVE_ALL" }
  | { type: "REKEY_CONNECTION"; fromKey: string; toKey: string }
  | {
      type: "STATUS_CHANGED"
      contextKey: string
      status: ConnectionStatus
    }
  | {
      // One AIR typed session-failure upsert (`session_failure` event).
      // Merged monotonically by id+revision; see `lib/session-failures.ts`.
      type: "SESSION_FAILURE"
      contextKey: string
      record: SessionFailureRecord
    }
  | {
      // Lifecycle settle for the AIR failure table (mirrors
      // `SessionState::apply_event`). `retry_incidents` rides turn PROGRESS —
      // fresh output proves the adapter reconnected. `warnings` is dispatched
      // from the `turn_complete` handler on a CLEAN (`end_turn`) end only: a
      // cancelled/failed exit ended a turn that did NOT recover, so its
      // warnings must stay active.
      type: "SETTLE_SESSION_FAILURES"
      contextKey: string
      scope: SessionFailureSettleScope
    }
  | {
      // The user closed a strip. Client-local, like `DISMISS_CONFIG_STALE`.
      // Takes every id that strip stood for: the collapsed warning bar closes
      // its hidden siblings with it.
      type: "DISMISS_SESSION_FAILURES"
      contextKey: string
      ids: string[]
    }
  | {
      // Mirror of a `background_activity` event onto the connection: the
      // `outstanding` count (the backend transcript watcher's authoritative
      // accounting) plus whether this event settled tasks / carried overlay
      // turns, which drive the settle-syncing bridge state. No-op when
      // nothing it mirrors changed, so repeat events don't re-render
      // connection consumers. `outOfTurnSettleCount` counts only settles whose
      // reply arrives OUT OF TURN (a separate overlay turn) — i.e. NOT
      // wire-visible; those are the ones the "syncing results" hint bridges.
      type: "SET_BACKGROUND_OUTSTANDING"
      contextKey: string
      outstanding: number
      outOfTurnSettleCount: number
      turnsCount: number
    }
  | StreamingAction
  | { type: "STREAM_BATCH"; actions: StreamingAction[] }
  | ({
      type: "TOOL_CALL"
      contextKey: string
      tool_call_id: string
      title: string
      kind: string
      status: string
      content: string | null
      raw_input: string | null
      raw_output: string | null
      locations: unknown
      meta: ToolCallMeta
      /** `null` when the wire event omitted the field (no images). */
      images: ToolCallImage[] | null
    } & ToolEstimatorActionContext)
  | ({
      type: "TOOL_CALL_UPDATE"
      contextKey: string
      tool_call_id: string
      title: string | null
      fallback_title: string
      fallback_kind: string
      status: string | null
      content: string | null
      raw_input: string | null
      raw_output: string | null
      raw_output_append?: boolean
      locations: unknown
      meta: ToolCallMeta
      /**
       * `null` when the wire event omitted the field — preserve prior images.
       * `[]` (empty array) when the agent explicitly cleared images.
       * `[a, b]` to replace.
       */
      images: ToolCallImage[] | null
    } & ToolEstimatorActionContext)
  | {
      type: "BATCH_TOOL_CALL_UPDATES"
      actions: Array<
        {
          contextKey: string
          tool_call_id: string
          title: string | null
          fallback_title: string
          fallback_kind: string
          status: string | null
          content: string | null
          raw_input: string | null
          raw_output: string | null
          raw_output_append?: boolean
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          locations: any | null
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          meta: any | null
          images: ToolCallImage[] | null
        } & ToolEstimatorActionContext
      >
    }
  | {
      type: "PERMISSION_REQUEST"
      contextKey: string
      request_id: string
      tool_call: unknown
      fallback_title: string
      fallback_kind: string
      options: PermissionOptionInfo[]
      queued?: number
    }
  | {
      type: "PERMISSION_QUEUE_DEPTH"
      contextKey: string
      depth: number
    }
  | {
      type: "PERMISSION_CLEARED"
      contextKey: string
      /**
       * When present, only clear if the current pendingPermission's request_id
       * matches. Guards against a late `permission_resolved` event wiping out a
       * fresh permission that was raised between resolve and dispatch.
       * Omit for unconditional clears (e.g. cancel paths).
       */
      requestId?: string
    }
  | {
      type: "SET_PENDING_QUESTION"
      contextKey: string
      pendingQuestion: PendingQuestion
    }
  | { type: "CLEAR_PENDING_QUESTION"; contextKey: string }
  | {
      type: "SET_ASK_QUESTION"
      contextKey: string
      pendingAskQuestion: PendingQuestionState
    }
  | {
      type: "CLEAR_ASK_QUESTION"
      contextKey: string
      /** When present, only clear if the current question_id matches (guards a
       *  late `question_resolved` from wiping a freshly-raised question). */
      questionId?: string
    }
  | {
      type: "SET_PLAN_APPROVAL"
      contextKey: string
      pendingPlanApproval: PendingPlanApprovalState
    }
  | {
      type: "CLEAR_PLAN_APPROVAL"
      contextKey: string
      /** When present, only clear if the current approval_id matches (guards a
       *  late `plan_approval_resolved` from wiping a freshly-raised approval). */
      approvalId?: string
    }
  | { type: "SESSION_STARTED"; contextKey: string; sessionId: string }
  | {
      /** Backend bound a draft/live connection to a persisted conversation row. */
      type: "CONVERSATION_LINKED"
      contextKey: string
      conversationId: number
    }
  | {
      type: "SESSION_MODES"
      contextKey: string
      modes: SessionModeStateInfo
    }
  | {
      type: "SESSION_CONFIG_OPTIONS"
      contextKey: string
      configOptions: SessionConfigOptionInfo[]
    }
  | {
      type: "CONFIG_STALE_CHANGED"
      contextKey: string
      stale: boolean
      kind: ConfigStaleKind
    }
  | {
      type: "DISMISS_CONFIG_STALE"
      contextKey: string
    }
  | {
      type: "SELECTORS_READY"
      contextKey: string
    }
  | {
      type: "PROMPT_CAPABILITIES"
      contextKey: string
      promptCapabilities: PromptCapabilitiesInfo
    }
  | {
      type: "FORK_SUPPORTED"
      contextKey: string
      supported: boolean
    }
  | { type: "MODE_CHANGED"; contextKey: string; modeId: string }
  | {
      type: "CONFIG_OPTION_CHANGED"
      contextKey: string
      configId: string
      valueId: string
    }
  | ({
      type: "PLAN_UPDATE"
      contextKey: string
      entries: PlanEntryInfo[]
    } & EstimatorActionContext)
  | { type: "TURN_ATTEMPT_ROLLBACK"; contextKey: string }
  | {
      type: "CLAUDE_API_RETRY"
      contextKey: string
      retry: ClaudeApiRetryState | null
    }
  | { type: "ERROR"; contextKey: string; message: string }
  | {
      type: "ATTACH_ERROR"
      contextKey: string
      code: AttachErrorCode
      retryable: boolean
    }
  | { type: "CLEAR_ATTACH_ERROR"; contextKey: string }
  | {
      type: "ACP_LOAD_ERROR"
      contextKey: string
      message: string
      code: string
    }
  | { type: "CLEAR_ACP_LOAD_ERROR"; contextKey: string }
  | {
      type: "AVAILABLE_COMMANDS"
      contextKey: string
      commands: AvailableCommandInfo[]
    }
  | {
      type: "USAGE_UPDATE"
      contextKey: string
      usage: SessionUsageUpdateInfo
      boundaryAt: number
    }
  | {
      type: "REQUEST_USAGE"
      contextKey: string
      outputTokens: number
      durationMs: number | null
      endedAt: number
    }
  | {
      type: "GENERATION_CLOCK_START"
      contextKey: string
      at: number
    }
  | {
      type: "EVENT_APPLIED"
      contextKey: string
      seq: number
    }
  | {
      /**
       * Synthesize a ConnectionState for a delegation-spawned child so
       * its acp://event stream lands in the reducer the same way a
       * user-driven connect() does. contextKey == connectionId for these
       * entries — the child has no user-facing tab to anchor a separate
       * key against.
       */
      type: "DELEGATION_CHILD_ATTACH"
      contextKey: string
      connectionId: string
      agentType: AgentType
      parentConnectionId: string
      parentToolUseId: string
    }
  | {
      /**
       * Remove the synthetic child entry once the delegation has wound
       * down (delegation_completed) and any grace window has elapsed.
       * When `retainObserver` is true, clear only the three delegation
       * fields and keep the canonical viewer entry (an open tab alias
       * still needs the attach).
       * No-op when the entry is already gone.
       */
      type: "DELEGATION_CHILD_DETACH"
      contextKey: string
      retainObserver?: boolean
    }
  | {
      /**
       * Merge non-destructive observer metadata into an existing
       * canonical connection (e.g. a second tab aliases the same
       * backend connectionId). Preserves live stream fields.
       */
      type: "OBSERVER_METADATA_MERGED"
      contextKey: string
      conversationId: number | null
      workingDir: string | null
    }
  | {
      type: "APPLY_EVENT_FRAME"
      frames: readonly PreparedConnectionFrame[]
    }

type StreamingAction =
  | ({
      type: "CONTENT_DELTA"
      contextKey: string
      text: string
      parentToolUseId?: string
    } & EstimatorActionContext)
  | ({
      type: "THINKING"
      contextKey: string
      text: string
      parentToolUseId?: string
    } & EstimatorActionContext)

type MapLevelAction = Extract<
  Action,
  {
    type:
      | "CONNECTION_CREATED"
      | "CONNECTION_REMOVED"
      | "REMOVE_ALL"
      | "REKEY_CONNECTION"
      | "DELEGATION_CHILD_ATTACH"
      | "DELEGATION_CHILD_DETACH"
  }
>

type FrameAction = Exclude<
  Action,
  MapLevelAction | { type: "APPLY_EVENT_FRAME" }
>

interface PreparedConnectionFrame {
  contextKey: string
  deliveryIds: readonly number[]
  actions: readonly FrameAction[]
  highestSeq: number
}

type ConnectionsMap = Map<string, ConnectionState>
type ReducerEffect = () => void

/** Test-only: counts outer-map clones from writableConnections. */
let writableConnectionsCloneCount = 0

function writableConnections(
  state: ConnectionsMap,
  mutateUnpublished: boolean
): ConnectionsMap {
  if (mutateUnpublished) return state
  if (process.env.NODE_ENV === "test") {
    writableConnectionsCloneCount += 1
  }
  return new Map(state)
}

/** @internal */
export function __getWritableConnectionsCloneCount(): number {
  return writableConnectionsCloneCount
}

/** @internal */
export function __resetWritableConnectionsCloneCount(): void {
  if (process.env.NODE_ENV !== "test") return
  writableConnectionsCloneCount = 0
}

/** @internal */
/** @internal Clear durable desktop delivery failure (tests / HMR). */
export function __resetDesktopDeliveryFailedForTests(): void {
  if (process.env.NODE_ENV !== "test") return
  writeDesktopDeliveryFailed(false)
}

export function __resetStreamingConfigForProviderTests(): void {
  __resetStreamingPerformanceConfigForTests()
}
const MAX_LIVE_TOOL_RAW_OUTPUT_CHARS = 200_000
const MAX_BUFFERED_UNMAPPED_EVENTS_PER_CONNECTION = 64
const MAX_BUFFERED_UNMAPPED_CONNECTIONS = 128
/**
 * How many times a user-driven `reconnect` will wait for an in-flight
 * `connect()` on the same key before giving up and rebuilding anyway. Small on
 * purpose: this only absorbs a connect that was already running (or the one
 * connect() itself re-dispatches for a superseded request), and a key that
 * keeps reconnecting on its own must not hang the button forever.
 */
const MAX_RECONNECT_SETTLE_WAITS = 3
/**
 * How long each of those waits will hold. A connect settles by resolving its
 * IPC, so one that never answers would otherwise park the reconnect FOREVER —
 * and a wedged connect is the very state this button is clicked from. Generous
 * enough to cover a real agent spawn (the wait exists to let that finish);
 * expiring just returns the button to the user to try again.
 */
const CONNECT_SETTLE_WAIT_TIMEOUT_MS = 15_000

// Per-agentType cache for selectors (modes / configOptions).
// Populated when real data arrives from the backend.
// Used as UI-layer fallback when the connection hasn't received real data yet.
const selectorsCache = new Map<
  string,
  {
    modes: SessionModeStateInfo | null
    configOptions: SessionConfigOptionInfo[] | null
  }
>()

export function getCachedSelectors(agentType: string) {
  return selectorsCache.get(agentType) ?? null
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

const PERMISSION_TOOL_INPUT_KEYS = [
  "rawInput",
  "raw_input",
  "input",
  "arguments",
  "params",
  "payload",
] as const

function asFiniteNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value
  }
  if (typeof value === "string" && value.trim().length > 0) {
    const parsed = Number(value)
    return Number.isFinite(parsed) ? parsed : null
  }
  return null
}

function parseClaudeApiRetryEvent(
  event: Extract<AcpEvent, { type: "claude_sdk_message" }>
): ClaudeApiRetryState | null {
  const message = asRecord(event.message)
  if (!message) return null
  if (message.type !== "system" || message.subtype !== "api_retry") return null

  return {
    sessionId:
      typeof message.session_id === "string"
        ? message.session_id
        : event.session_id,
    attempt: asFiniteNumber(message.attempt),
    maxRetries: asFiniteNumber(message.max_retries),
    error: typeof message.error === "string" ? message.error : null,
    errorStatus: asFiniteNumber(message.error_status),
    retryDelayMs: asFiniteNumber(message.retry_delay_ms),
  }
}

function extractPermissionToolCallId(toolCall: unknown): string | null {
  const record = asRecord(toolCall)
  if (!record) return null
  const candidates = [
    record.call_id,
    record.callId,
    record.tool_call_id,
    record.toolCallId,
    record.id,
  ]
  for (const candidate of candidates) {
    if (typeof candidate === "string" && candidate.trim().length > 0) {
      return candidate
    }
  }
  return null
}

function pickPermissionToolInput(record: Record<string, unknown>): unknown {
  for (const key of PERMISSION_TOOL_INPUT_KEYS) {
    const value = record[key]
    if (value === undefined || value === null) continue
    if (typeof value === "string" && value.trim().length === 0) continue
    return value
  }
  return null
}

function serializePermissionInput(value: unknown): string | null {
  if (value === undefined || value === null) return null
  if (typeof value === "string") {
    return value.trim().length > 0 ? value : null
  }
  try {
    return JSON.stringify(value)
  } catch {
    return null
  }
}

function serializePermissionToolCall(toolCall: unknown): string | null {
  const record = asRecord(toolCall)
  if (!record) return null
  try {
    // Extract the actual tool input rather than serializing the entire
    // permission wrapper (which includes internal fields like kind/status/id).
    const nestedInput = pickPermissionToolInput(record)
    const serializedNestedInput = serializePermissionInput(nestedInput)
    if (serializedNestedInput) return serializedNestedInput

    // Fallback: strip wrapper-only fields to avoid rendering internal
    // permission structure as raw text.
    const wrapperKeys = new Set([
      "content",
      "kind",
      "status",
      "title",
      "toolCallId",
      "tool_call_id",
      "callId",
      "call_id",
      ...PERMISSION_TOOL_INPUT_KEYS,
    ])
    const rest: Record<string, unknown> = {}
    for (const [k, v] of Object.entries(record)) {
      if (!wrapperKeys.has(k)) rest[k] = v
    }
    return Object.keys(rest).length > 0 ? JSON.stringify(rest) : null
  } catch {
    return null
  }
}

function findLiveToolCallInfo(
  content: LiveContentBlock[],
  toolCallId: string | null
): ToolCallInfo | null {
  if (!toolCallId) return null
  const block = content.find(
    (item) => item.type === "tool_call" && item.info.tool_call_id === toolCallId
  )
  return block?.type === "tool_call" ? block.info : null
}

function mergePermissionToolCallWithLiveInfo(
  toolCall: unknown,
  liveInfo: ToolCallInfo | null
): unknown {
  if (!liveInfo) return toolCall

  const rawInput = serializePermissionInput(liveInfo.raw_input)
  const record = asRecord(toolCall)
  if (!record) {
    if (!rawInput) return toolCall
    return {
      toolCallId: liveInfo.tool_call_id,
      title: liveInfo.title,
      kind: liveInfo.kind,
      rawInput,
    }
  }

  const next = { ...record }
  let changed = false
  const existingInput = serializePermissionInput(pickPermissionToolInput(next))
  if (!existingInput && rawInput) {
    next.rawInput = rawInput
    changed = true
  }
  if (typeof next.title !== "string" || next.title.trim().length === 0) {
    next.title = liveInfo.title
    changed = true
  }
  if (typeof next.kind !== "string" || next.kind.trim().length === 0) {
    next.kind = liveInfo.kind
    changed = true
  }
  if (!extractPermissionToolCallId(next)) {
    next.toolCallId = liveInfo.tool_call_id
    changed = true
  }
  return changed ? next : toolCall
}

function mergePendingPermissionWithLiveInfo(
  pendingPermission: PendingPermission | null,
  liveInfo: ToolCallInfo | null
): PendingPermission | null {
  if (!pendingPermission || !liveInfo) return pendingPermission
  const permissionCallId = extractPermissionToolCallId(
    pendingPermission.tool_call
  )
  if (permissionCallId !== liveInfo.tool_call_id) return pendingPermission

  const toolCall = mergePermissionToolCallWithLiveInfo(
    pendingPermission.tool_call,
    liveInfo
  )
  if (toolCall === pendingPermission.tool_call) return pendingPermission
  return {
    ...pendingPermission,
    tool_call: toolCall,
  }
}

function mergePendingPermissionWithLiveMessage(
  pendingPermission: PendingPermission | null,
  liveMessage: LiveMessage | null
): PendingPermission | null {
  const permissionCallId = extractPermissionToolCallId(
    pendingPermission?.tool_call
  )
  const liveInfo = liveMessage
    ? findLiveToolCallInfo(liveMessage.content, permissionCallId)
    : null
  return mergePendingPermissionWithLiveInfo(pendingPermission, liveInfo)
}

function extractPermissionToolTitle(toolCall: unknown): string | null {
  const record = asRecord(toolCall)
  if (!record) return null
  const candidates = [record.title, record.tool_name, record.name, record.type]
  for (const candidate of candidates) {
    if (typeof candidate === "string" && candidate.trim().length > 0) {
      return candidate
    }
  }
  return null
}

function extractPermissionToolKind(toolCall: unknown): string | null {
  const record = asRecord(toolCall)
  if (!record) return null
  const candidates = [record.kind, record.tool_name, record.name, record.type]
  for (const candidate of candidates) {
    if (typeof candidate === "string" && candidate.trim().length > 0) {
      return candidate
    }
  }
  return null
}

/**
 * Extract the free-text question for the LEGACY `QuestionDialog` from a tool
 * call's raw input — gated on a singular `question` STRING field. Exported so a
 * regression test can prove the new multiple-choice `ask_user_question` tool
 * (whose input is `{ questions: [...] }`, plural array) never trips this legacy
 * path even though tool-name normalization classifies it as "question".
 */
export function extractQuestionText(rawInput: string | null): string | null {
  if (!rawInput) return null
  try {
    const parsed = JSON.parse(rawInput)
    if (
      parsed &&
      typeof parsed === "object" &&
      typeof parsed.question === "string"
    ) {
      return parsed.question
    }
  } catch {
    // not JSON, try using rawInput as-is if it looks like a question
  }
  return null
}

function sameModes(
  a: SessionModeStateInfo | null,
  b: SessionModeStateInfo
): boolean {
  if (a === b) return true
  if (!a) return false
  if (a.current_mode_id !== b.current_mode_id) return false
  if (a.available_modes.length !== b.available_modes.length) return false
  for (let i = 0; i < a.available_modes.length; i += 1) {
    const left = a.available_modes[i]
    const right = b.available_modes[i]
    if (
      left.id !== right.id ||
      left.name !== right.name ||
      left.description !== right.description
    ) {
      return false
    }
  }
  return true
}

function samePromptCapabilities(
  a: PromptCapabilitiesInfo,
  b: PromptCapabilitiesInfo
): boolean {
  return (
    a.image === b.image &&
    a.audio === b.audio &&
    a.embedded_context === b.embedded_context
  )
}

function samePlanEntries(a: PlanEntryInfo[], b: PlanEntryInfo[]): boolean {
  if (a === b) return true
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i += 1) {
    if (
      a[i].content !== b[i].content ||
      a[i].priority !== b[i].priority ||
      a[i].status !== b[i].status
    ) {
      return false
    }
  }
  return true
}

function sameConfigOptions(
  a: SessionConfigOptionInfo[] | null,
  b: SessionConfigOptionInfo[]
): boolean {
  if (a === b) return true
  if (!a) return false
  if (a.length !== b.length) return false

  for (let i = 0; i < a.length; i += 1) {
    const left = a[i]
    const right = b[i]
    if (
      left.id !== right.id ||
      left.name !== right.name ||
      left.description !== right.description ||
      left.category !== right.category
    ) {
      return false
    }

    const leftKind = left.kind
    const rightKind = right.kind
    if (leftKind.type !== rightKind.type) return false

    if (leftKind.type === "select") {
      if (leftKind.current_value !== rightKind.current_value) return false
      if (leftKind.options.length !== rightKind.options.length) return false
      if (leftKind.groups.length !== rightKind.groups.length) return false

      for (let j = 0; j < leftKind.options.length; j += 1) {
        const lo = leftKind.options[j]
        const ro = rightKind.options[j]
        if (
          lo.value !== ro.value ||
          lo.name !== ro.name ||
          lo.description !== ro.description
        ) {
          return false
        }
      }

      for (let j = 0; j < leftKind.groups.length; j += 1) {
        const lg = leftKind.groups[j]
        const rg = rightKind.groups[j]
        if (lg.group !== rg.group || lg.name !== rg.name) return false
        if (lg.options.length !== rg.options.length) return false
        for (let k = 0; k < lg.options.length; k += 1) {
          const lgo = lg.options[k]
          const rgo = rg.options[k]
          if (
            lgo.value !== rgo.value ||
            lgo.name !== rgo.name ||
            lgo.description !== rgo.description
          ) {
            return false
          }
        }
      }
    }
  }
  return true
}

function sameCommands(
  a: AvailableCommandInfo[] | null,
  b: AvailableCommandInfo[]
): boolean {
  if (a === b) return true
  if (!a) return false
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i += 1) {
    if (
      a[i].name !== b[i].name ||
      a[i].description !== b[i].description ||
      a[i].input_hint !== b[i].input_hint
    ) {
      return false
    }
  }
  return true
}

function dedupeCommandsByName(
  commands: AvailableCommandInfo[]
): AvailableCommandInfo[] {
  const seen = new Set<string>()
  let deduped: AvailableCommandInfo[] | null = null

  for (let i = 0; i < commands.length; i += 1) {
    const command = commands[i]
    if (seen.has(command.name)) {
      deduped ??= commands.slice(0, i)
      continue
    }

    seen.add(command.name)
    deduped?.push(command)
  }

  return deduped ?? commands
}

/**
 * Lazy-create a `LiveMessage` shell mirroring the backend's
 * `ensure_live_message` semantic. Required because the backend only
 * initializes `session_state.live_message` when the first `ContentDelta` /
 * `Thinking` / `ToolCall` / `PlanUpdate` arrives — there's a window between
 * `StatusChanged(Prompting)` and the first content event in which the
 * snapshot reports `live_message: null`. After a browser refresh inside
 * that window, the live `STATUS_CHANGED(prompting)` event won't re-fire
 * (status is already prompting in the snapshot), so without this fallback
 * the reducer would drop every subsequent delta / tool call / plan update.
 */
function ensureLiveMessage(prev: LiveMessage | null): LiveMessage {
  if (prev) return prev
  return {
    id: randomUUID(),
    role: "assistant",
    content: [],
    startedAt: Date.now(),
  }
}

function rollbackLiveMessageAttempt(prev: LiveMessage): LiveMessage {
  let boundary = -1
  for (let index = prev.content.length - 1; index >= 0; index--) {
    if (prev.content[index].type === "tool_call") {
      boundary = index
      break
    }
  }
  const retainedLength = boundary + 1
  if (retainedLength === prev.content.length) return prev
  return { ...prev, content: prev.content.slice(0, retainedLength) }
}

/** Last time an out-of-turn drop was logged — module-level sampling clock. */
let lastOutOfTurnDropLogAt = 0

function estimatorFor(conn: ConnectionState): RequestTokenEstimatorState {
  return conn.requestEstimator ?? createRequestTokenEstimator()
}

function estimatorHydrationSeed(
  liveMessage: LiveMessage | null
): EstimatorHydrationSeed {
  const planEntries: string[] = []
  const toolInputs: Array<readonly [string, string]> = []
  for (const block of liveMessage?.content ?? []) {
    if (block.type === "plan") {
      for (const entry of block.entries) planEntries.push(entry.content)
    }
    if (block.type === "tool_call" && block.info.raw_input != null) {
      toolInputs.push([`tool:${block.info.tool_call_id}`, block.info.raw_input])
    }
  }
  return { planText: planEntries.join("\n"), toolInputs }
}

function estimatorObservation(
  conn: ConnectionState,
  receivedAt: number
): EstimatorObservation {
  return {
    agentType: conn.agentType === "grok" ? "grok" : "codex",
    configOptions: conn.configOptions,
    receivedAt,
  }
}

function requiredReceivedAt(event: EventEnvelope): number {
  if (event.received_at == null) {
    throw new Error("accepted ACP event is missing received_at")
  }
  return event.received_at
}

function applyStreamingAction(
  conn: ConnectionState,
  action: StreamingAction
): ConnectionState | null {
  // OUT-OF-TURN guard: the backend's idle loop forwards session/updates that
  // arrive BETWEEN turns (background sub-agent completions, the agent's
  // continued autonomous work). Appending those here would graft them onto
  // the previous turn's completed liveMessage — the historical "background
  // results render garbled/incomplete" bug. The transcript watcher's
  // `background_activity` overlay is the single render path for out-of-turn
  // content, so wire deltas outside a prompting turn are dropped. Ordering is
  // safe: the backend emits StatusChanged(prompting) before any turn content,
  // and turn_complete flushes queued deltas before flipping status back.
  if (conn.status !== "prompting") {
    // Sampled: an autonomous (cron//loop) turn streams the ENTIRE wire
    // out-of-turn — logging every dropped delta would spam the console and
    // allocate per token for minutes at a time.
    const now = Date.now()
    if (now - lastOutOfTurnDropLogAt > 5_000) {
      lastOutOfTurnDropLogAt = now
      console.debug(
        "[acp] dropping out-of-turn streaming deltas (provider policy owns out-of-turn rendering)",
        { contextKey: conn.contextKey, type: action.type }
      )
    }
    return null
  }
  // CONTENT_DELTA with empty text is a true no-op. THINKING with empty text
  // is allowed to create the initial placeholder block so the UI can show
  // a "Thinking..." indicator immediately (and for newer Claude models that
  // redact thinking text entirely, keeping the empty block as the signal).
  if (action.type === "CONTENT_DELTA" && action.text.length === 0) return null

  if (action.parentToolUseId) {
    const parentPresent = conn.liveMessage?.content.some(
      (block) =>
        block.type === "tool_call" &&
        block.info.tool_call_id === action.parentToolUseId
    )
    if (!parentPresent) return null
  }

  const prev = ensureLiveMessage(conn.liveMessage)
  const lastBlock = prev.content[prev.content.length - 1]
  let newContent: LiveContentBlock[] | null = null

  if (action.type === "CONTENT_DELTA") {
    if (
      lastBlock?.type === "text" &&
      lastBlock.parentToolUseId === action.parentToolUseId
    ) {
      newContent = [
        ...prev.content.slice(0, -1),
        {
          type: "text",
          text: lastBlock.text + action.text,
          parentToolUseId: action.parentToolUseId,
        },
      ]
    } else {
      newContent = [
        ...prev.content,
        {
          type: "text",
          text: action.text,
          parentToolUseId: action.parentToolUseId,
        },
      ]
    }
  } else {
    if (
      action.text.length === 0 &&
      lastBlock?.type === "thinking" &&
      lastBlock.parentToolUseId === action.parentToolUseId
    ) {
      // Already have a thinking block; an empty follow-up event is a no-op.
      return null
    }
    if (
      lastBlock?.type === "thinking" &&
      lastBlock.parentToolUseId === action.parentToolUseId
    ) {
      newContent = [
        ...prev.content.slice(0, -1),
        {
          type: "thinking",
          text: lastBlock.text + action.text,
          parentToolUseId: action.parentToolUseId,
        },
      ]
    } else {
      newContent = [
        ...prev.content,
        {
          type: "thinking",
          text: action.text,
          parentToolUseId: action.parentToolUseId,
        },
      ]
    }
  }

  if (!newContent) return null
  const sessionFailures = hasSettleableRetryIncident(conn.sessionFailures)
    ? settleSessionFailures(conn.sessionFailures, "retry_incidents")
    : conn.sessionFailures
  const requestEstimator =
    conn.agentType === "codex" && !action.parentToolUseId
      ? observeEstimatedDelta(
          estimatorFor(conn),
          action.text,
          estimatorObservation(conn, action.receivedAt)
        )
      : conn.requestEstimator
  return {
    ...conn,
    liveMessage: { ...prev, content: newContent },
    sessionFailures,
    requestEstimator,
    // Streaming content implies the SDK has recovered from any in-flight
    // Claude API retry, so hide the retry banner immediately instead of
    // waiting for the prompt cycle to end.
    claudeApiRetry: null,
  }
}

/** Newest out-of-turn tool-call contexts kept per connection (see
 *  `ConnectionState.outOfTurnToolCalls`). Permission enrichment only ever
 *  needs the last few. */
const OUT_OF_TURN_TOOL_CALL_CAP = 8

/**
 * Overlay-fold refetch: once a conversation's background overlay exceeds this
 * many turns, fold them into persisted turns via a detail refetch (the
 * watermark rule retires covered entries). Keeps a day-long cron//loop
 * session's overlay — which never settles, so nothing else refetches —
 * bounded. Sized so the fold runs every few dozen autonomous turns, not per
 * turn.
 */
const OVERLAY_FOLD_THRESHOLD = 60
/** Floor between overlay-fold refetches per conversation, so a failing
 *  backend (refetch errors, overlay keeps growing) can't escalate into a
 *  refetch per background event. */
const OVERLAY_FOLD_MIN_INTERVAL_MS = 30_000
/** conversationId → epoch ms of the last overlay-fold refetch. Module-level:
 *  survives provider re-renders; a few entries only (conversations with
 *  active background overlay). */
const overlayFoldRefetchAt = new Map<number, number>()

/**
 * Desktop-only: emit one hidden-app system notification per
 * `(lease_id, version)`. Server/Web never reaches this path for watchdog
 * (caller gates on desktop runtime). Never includes tool input.
 */
const notifiedToolWatchdogKeys = new Set<string>()

function maybeNotifyToolWatchdog(
  leaseId: string,
  version: number,
  title: string,
  body: string,
  conversationId: number | null
): void {
  if (!getTransport().isDesktop()) return
  if (typeof document !== "undefined" && !document.hidden) return
  const key = `${leaseId}:${version}`
  // Renderer-local fast path (single window). Host `dedupeKey` is the
  // multi-window authority so two Tauri webviews cannot each notify.
  if (notifiedToolWatchdogKeys.has(key)) return
  notifiedToolWatchdogKeys.add(key)
  // Bound memory: drop oldest when the set grows large (many leases / versions).
  if (notifiedToolWatchdogKeys.size > 256) {
    const first = notifiedToolWatchdogKeys.values().next().value
    if (first !== undefined) notifiedToolWatchdogKeys.delete(first)
  }
  const target =
    conversationId != null && Number.isFinite(conversationId)
      ? { kind: "conversation" as const, conversationId }
      : undefined
  sendSystemNotification(title, body, target, { dedupeKey: key }).catch(
    () => {}
  )
}

/** @internal test helper */
export function __resetToolWatchdogNotifyDedupeForTests(): void {
  notifiedToolWatchdogKeys.clear()
}

/** Upsert one out-of-turn tool-call info into the bounded registry,
 *  evicting the oldest entry past the cap. Returns a fresh map. */
function recordOutOfTurnToolCall(
  existing: ReadonlyMap<string, ToolCallInfo> | null,
  info: ToolCallInfo
): ReadonlyMap<string, ToolCallInfo> {
  const next = new Map(existing ?? [])
  next.delete(info.tool_call_id)
  next.set(info.tool_call_id, info)
  while (next.size > OUT_OF_TURN_TOOL_CALL_CAP) {
    const oldest = next.keys().next().value
    if (oldest === undefined) break
    next.delete(oldest)
  }
  return next
}

function reduceSingleAction(
  state: ConnectionsMap,
  action: Exclude<Action, { type: "APPLY_EVENT_FRAME" }>,
  mutateUnpublished = false,
  effects?: ReducerEffect[]
): ConnectionsMap {
  switch (action.type) {
    case "CONNECTION_CREATED": {
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        connectionId: action.connectionId,
        contextKey: action.contextKey,
        agentType: action.agentType,
        workingDir: action.workingDir,
        status: "connecting",
        promptCapabilities: {
          image: false,
          audio: false,
          embedded_context: false,
        },
        supportsFork: false,
        selectorsReady: false,
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        requestUsage: EMPTY_REQUEST_USAGE,
        requestEstimator: createRequestTokenEstimator(),
        generationClockStartedAt: null,
        liveMessage: null,
        pendingPermission: null,
        pendingUserMessage: null,
        pendingQuestion: null,
        pendingAskQuestion: null,
        pendingPlanApproval: null,
        claudeApiRetry: null,
        sessionFailures: [],
        error: null,
        attachError: null,
        loadError: null,
        loadErrorCode: null,
        lastAppliedSeq: 0,
        isDelegationChild: false,
        parentToolUseId: null,
        parentConnectionId: null,
        isViewer: action.isViewer ?? false,
        configStale: false,
        configStaleKind: null,
        configStaleDismissed: false,
        backgroundOutstanding: 0,
        backgroundSettleSyncingSince: null,
        outOfTurnToolCalls: null,
        delegationRoute: null,
        conversationId: action.conversationId ?? null,
        delegationRouteOverride: action.delegationRouteOverride ?? null,
        waitingForSubagents: null,
        toolWatchdogProjections: {},
        toolWatchdogMaxVersions: {},
        lastToolWatchdogDiagnostic: null,
        ownershipGeneration: action.ownershipGeneration ?? null,
        ownerOperationId: action.ownerOperationId ?? null,
        ownerWindowLabel: action.ownerWindowLabel ?? null,
        sharedSession: action.sharedSession ?? null,
      })
      return next
    }

    case "OWNERSHIP_LEASE_UPDATED": {
      const current = state.get(action.contextKey)
      if (!current) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        ownershipGeneration:
          action.ownershipGeneration !== undefined
            ? action.ownershipGeneration
            : current.ownershipGeneration,
        ownerOperationId:
          action.ownerOperationId !== undefined
            ? action.ownerOperationId
            : current.ownerOperationId,
        ownerWindowLabel:
          action.ownerWindowLabel !== undefined
            ? action.ownerWindowLabel
            : current.ownerWindowLabel,
      })
      return next
    }

    case "SHARED_SESSION_UPDATED": {
      const current = state.get(action.contextKey)
      const shared = current?.sharedSession
      if (!current || !shared || shared.generation !== action.generation) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        sharedSession: { ...shared, ...action.update },
      })
      return next
    }

    case "SHARED_SESSION_EVENT": {
      const current = state.get(action.contextKey)
      const shared = current?.sharedSession
      if (
        !current ||
        !shared ||
        shared.generation !== action.event.generation
      ) {
        return state
      }
      let nextShared = shared
      switch (action.event.type) {
        case "shared_session_phase_changed":
          nextShared = {
            ...shared,
            phase: sharedPhaseFromWire(action.event.phase),
          }
          break
        case "prompt_queued": {
          const item = sharedQueueItemFromWire(action.event.item)
          const without = shared.queue.filter(
            (queued) => queued.queueItemId !== item.queueItemId
          )
          nextShared = { ...shared, queue: [...without, item] }
          break
        }
        case "prompt_queue_item_cancelled": {
          const event = action.event as Extract<
            AcpEvent,
            { type: "prompt_queue_item_cancelled" }
          >
          nextShared = {
            ...shared,
            queue: shared.queue.filter(
              (item) => item.queueItemId !== event.queue_item_id
            ),
            activeTurn:
              shared.activeTurn?.queueItemId === event.queue_item_id
                ? { ...shared.activeTurn, promptSummary: null }
                : shared.activeTurn,
          }
          break
        }
        case "prompt_queue_item_failed": {
          const event = action.event as Extract<
            AcpEvent,
            { type: "prompt_queue_item_failed" }
          > & { seq: number }
          const failedItem =
            shared.queue.find(
              (item) => item.queueItemId === event.queue_item_id
            ) ??
            (shared.activeTurn?.queueItemId === event.queue_item_id
              ? shared.activeTurn.promptSummary
              : null)
          if (!failedItem) return state
          const failedRow: SharedQueuedPrompt = {
            ...failedItem,
            state: "failed",
            errorCode: event.error_code,
            failureEventSeq: event.seq,
          }
          nextShared = {
            ...shared,
            queue: [
              ...shared.queue.filter(
                (item) => item.queueItemId !== event.queue_item_id
              ),
              failedRow,
            ],
            activeTurn:
              shared.activeTurn?.queueItemId === event.queue_item_id
                ? { ...shared.activeTurn, promptSummary: null }
                : shared.activeTurn,
          }
          break
        }
        case "prompt_dispatch_started": {
          const turn = action.event.turn
          const promptSummary =
            shared.queue.find(
              (item) => item.queueItemId === turn.queue_item_id
            ) ?? null
          nextShared = {
            ...shared,
            queue: shared.queue.filter(
              (item) => item.queueItemId !== turn.queue_item_id
            ),
            activeTurn: {
              ...sharedTurnFromWire(turn),
              promptSummary,
            },
          }
          break
        }
        case "shared_turn_settled": {
          if (shared.activeTurn?.turnId !== action.event.turn_id) return state
          const settledQueueItemId = shared.activeTurn.queueItemId
          const existingFailedRow = shared.queue.find(
            (item) =>
              item.queueItemId === settledQueueItemId && item.state === "failed"
          )
          const synthesizedFailedRow =
            action.event.outcome === "failed" &&
            !existingFailedRow &&
            shared.activeTurn.promptSummary
              ? {
                  ...shared.activeTurn.promptSummary,
                  state: "failed" as const,
                  errorCode: "shared_turn_failed",
                  failureEventSeq: action.event.seq,
                }
              : null
          nextShared = {
            ...shared,
            queue:
              action.event.outcome === "failed"
                ? existingFailedRow
                  ? shared.queue.map((item) =>
                      item === existingFailedRow
                        ? {
                            ...item,
                            failureEventSeq: Math.max(
                              item.failureEventSeq ?? 0,
                              action.event.seq
                            ),
                          }
                        : item
                    )
                  : synthesizedFailedRow
                    ? [
                        ...shared.queue.filter(
                          (item) => item.queueItemId !== settledQueueItemId
                        ),
                        synthesizedFailedRow,
                      ]
                    : shared.queue
                : shared.queue.filter(
                    (item) => item.queueItemId !== settledQueueItemId
                  ),
            activeTurn: null,
          }
          break
        }
        case "prompt_queue_depth_changed":
          return state
      }
      if (nextShared === shared) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, { ...current, sharedSession: nextShared })
      return next
    }

    case "DISMISS_FAILED_SHARED_PROMPT": {
      const current = state.get(action.contextKey)
      const shared = current?.sharedSession
      if (!current || !shared) return state
      const queue = shared.queue.filter(
        (item) =>
          item.queueItemId !== action.queueItemId || item.state !== "failed"
      )
      if (queue.length === shared.queue.length) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        sharedSession: { ...shared, queue },
      })
      return next
    }

    case "DELEGATION_CHILD_ATTACH": {
      // Idempotent merge: if an entry already exists for this key with the
      // same connectionId (canonical viewer or prior attach), enrich
      // delegation metadata without blowing away the live stream. If the
      // connectionId differs we replace, since a new spawn won the race.
      const existing = state.get(action.contextKey)
      if (existing && existing.connectionId === action.connectionId) {
        if (
          existing.isDelegationChild &&
          existing.parentConnectionId === action.parentConnectionId &&
          existing.parentToolUseId === action.parentToolUseId
        ) {
          return state
        }
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, {
          ...existing,
          isDelegationChild: true,
          parentConnectionId: action.parentConnectionId,
          parentToolUseId: action.parentToolUseId,
        })
        return next
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        connectionId: action.connectionId,
        contextKey: action.contextKey,
        agentType: action.agentType,
        workingDir: null,
        // The child is already alive in the backend by the time
        // delegation_started fires; treat it as connected so any UI
        // surface that gates on status reflects reality.
        status: "connected",
        promptCapabilities: {
          image: false,
          audio: false,
          embedded_context: false,
        },
        supportsFork: false,
        selectorsReady: true,
        sessionId: null,
        modes: null,
        configOptions: null,
        availableCommands: null,
        usage: null,
        requestUsage: EMPTY_REQUEST_USAGE,
        requestEstimator: createRequestTokenEstimator(),
        generationClockStartedAt: null,
        liveMessage: null,
        pendingPermission: null,
        pendingUserMessage: null,
        pendingQuestion: null,
        pendingAskQuestion: null,
        pendingPlanApproval: null,
        claudeApiRetry: null,
        sessionFailures: [],
        error: null,
        attachError: null,
        loadError: null,
        loadErrorCode: null,
        lastAppliedSeq: 0,
        isDelegationChild: true,
        parentToolUseId: action.parentToolUseId,
        parentConnectionId: action.parentConnectionId,
        isViewer: false,
        configStale: false,
        configStaleKind: null,
        configStaleDismissed: false,
        backgroundOutstanding: 0,
        backgroundSettleSyncingSince: null,
        outOfTurnToolCalls: null,
        delegationRoute: null,
        conversationId: null,
        delegationRouteOverride: null,
        waitingForSubagents: null,
        toolWatchdogProjections: {},
        toolWatchdogMaxVersions: {},
        lastToolWatchdogDiagnostic: null,
        sharedSession: null,
      })
      return next
    }

    case "DELEGATION_CHILD_DETACH": {
      const existing = state.get(action.contextKey)
      if (!existing || !existing.isDelegationChild) return state
      const next = writableConnections(state, mutateUnpublished)
      if (action.retainObserver) {
        next.set(action.contextKey, {
          ...existing,
          isDelegationChild: false,
          parentConnectionId: null,
          parentToolUseId: null,
        })
        return next
      }
      next.delete(action.contextKey)
      return next
    }

    case "OBSERVER_METADATA_MERGED": {
      const current = state.get(action.contextKey)
      if (!current) return state
      const nextConversationId =
        action.conversationId != null
          ? action.conversationId
          : current.conversationId
      const nextWorkingDir = current.workingDir ?? action.workingDir
      if (
        current.isViewer &&
        current.conversationId === nextConversationId &&
        current.workingDir === nextWorkingDir
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        isViewer: true,
        conversationId: nextConversationId,
        workingDir: nextWorkingDir,
      })
      return next
    }

    case "HYDRATE_FROM_SNAPSHOT": {
      const current = state.get(action.contextKey)
      if (!current) return state
      // Identity guard: the connection at this contextKey may have been
      // disconnected and replaced between the snapshot fetch firing and
      // its async response. eventSeq alone is not enough — a stale snapshot
      // from connection A (high seq) would otherwise overwrite a fresh
      // connection B (lastAppliedSeq=0) at the same contextKey.
      if (current.connectionId !== action.patch.connectionId) return state

      // Latched-once / fill-null fields are always safe to merge, even when
      // the snapshot is stale by event_seq. Their producing events
      // (`selectors_ready`, `fork_supported`, `session_modes`,
      // `session_config_options`, `available_commands`, `prompt_capabilities`)
      // typically fire only once during the initial handshake, so the
      // snapshot is the only recovery path after a refresh that missed the
      // original live event. Without this, a mid-stream browser refresh
      // races the snapshot fetch against new content_delta events: the
      // deltas advance lastAppliedSeq past the snapshot's event_seq, the
      // outer guard rejects the patch, and `selectorsReady` never recovers
      // — leaving the bottom status bar stuck on "正在初始化 xxx 会话".
      const mergedSelectorsReady =
        action.patch.selectorsReady || current.selectorsReady
      const mergedSupportsFork =
        action.patch.supportsFork || current.supportsFork
      const mergedModes = current.modes ?? action.patch.modes
      const mergedConfigOptions =
        current.configOptions ?? action.patch.configOptions
      const mergedAvailableCommands =
        current.availableCommands ?? action.patch.availableCommands
      const mergedPromptCapabilities =
        action.patch.promptCapabilities ?? current.promptCapabilities
      // Fill-null: recover conversation binding when connect started without a
      // row (draft tab). Never clear an already-bound id with a null patch.
      const mergedConversationId =
        current.conversationId ?? action.patch.conversationId ?? null
      // Shared queue/phase state is authoritative at an equal sequence (the
      // snapshot can carry a projection not represented by a later event), but
      // an older snapshot must never overwrite a newer live projection.
      const mayMergeSharedSession =
        action.patch.eventSeq >= current.lastAppliedSeq
      const mergedSharedSession =
        mayMergeSharedSession &&
        current.sharedSession &&
        action.patch.sharedSession &&
        current.sharedSession.generation ===
          action.patch.sharedSession.generation
          ? {
              ...current.sharedSession,
              phase: action.patch.sharedSession.phase,
              queue: mergeSharedQueueWithRetainedFailures(
                current.sharedSession.queue,
                action.patch.sharedSession.queue,
                action.patch.eventSeq
              ),
              activeTurn: mergeSharedActiveTurnPromptSummary(
                current.sharedSession.activeTurn,
                action.patch.sharedSession.activeTurn
              ),
              leaseExpiresAt:
                action.patch.sharedSession.leaseExpiresAt ??
                current.sharedSession.leaseExpiresAt,
            }
          : current.sharedSession

      // Race guard: the snapshot may have been generated BEFORE events
      // that have since arrived and been applied to in-memory state.
      // Mutable fields (status, sessionId, liveMessage, pendingPermission,
      // usage, error) are fresher in memory than in the snapshot and must NOT
      // be overwritten — but the latched/fill-null fields above are still
      // applied so the once-per-lifetime bits can recover. `error` in
      // particular is cleared on a new prompt (STATUS_CHANGED → prompting), so
      // folding a stale snapshot's `lastError` back in here would resurrect an
      // error the current turn already cleared; it is recovered on the fresh
      // path below instead.
      // AIR failure records merge on BOTH branches: the per-id monotonic rule
      // is idempotent and can only add or upgrade entries, never clobber a
      // fresher live one — so even a stale-by-eventSeq snapshot may safely
      // contribute records this client attached too late to see live.
      const mergedSessionFailures = mergeSessionFailures(
        current.sessionFailures,
        action.patch.sessionFailures
      )

      if (action.patch.eventSeq <= current.lastAppliedSeq) {
        if (
          mergedSelectorsReady === current.selectorsReady &&
          mergedSupportsFork === current.supportsFork &&
          mergedModes === current.modes &&
          mergedConfigOptions === current.configOptions &&
          mergedAvailableCommands === current.availableCommands &&
          mergedPromptCapabilities === current.promptCapabilities &&
          mergedConversationId === current.conversationId &&
          mergedSessionFailures === current.sessionFailures &&
          mergedSharedSession === current.sharedSession
        ) {
          return state
        }
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, {
          ...current,
          modes: mergedModes,
          configOptions: mergedConfigOptions,
          availableCommands: mergedAvailableCommands,
          promptCapabilities: mergedPromptCapabilities,
          selectorsReady: mergedSelectorsReady,
          supportsFork: mergedSupportsFork,
          conversationId: mergedConversationId,
          sessionFailures: mergedSessionFailures,
          sharedSession: mergedSharedSession,
        })
        return next
      }

      const hydratedLiveMessage = action.patch.liveMessage
      const hydratedPendingPermission = mergePendingPermissionWithLiveMessage(
        action.patch.pendingPermission,
        hydratedLiveMessage ?? current.liveMessage
      )
      const hydratedRequestEstimator = replaceEstimatorFromHydration(
        estimatorFor(current),
        estimatorHydrationSeed(hydratedLiveMessage)
      )
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        status: action.patch.status,
        sessionId: action.patch.sessionId,
        modes: action.patch.modes,
        configOptions: action.patch.configOptions,
        availableCommands: action.patch.availableCommands,
        usage: action.patch.usage,
        requestEstimator: hydratedRequestEstimator,
        requestUsage: EMPTY_REQUEST_USAGE,
        generationClockStartedAt: null,
        liveMessage: hydratedLiveMessage,
        pendingPermission: hydratedPendingPermission,
        pendingAskQuestion: action.patch.pendingAskQuestion,
        pendingPlanApproval: action.patch.pendingPlanApproval,
        pendingUserMessage: action.patch.pendingUserMessage,
        promptCapabilities: mergedPromptCapabilities,
        selectorsReady: mergedSelectorsReady,
        supportsFork: mergedSupportsFork,
        // Staleness is a current-state field (like status): apply the snapshot's
        // value on the fresh path. `configStaleDismissed` is client-local and
        // preserved via `...current`.
        configStale: action.patch.configStale,
        configStaleKind: action.patch.configStaleKind,
        // Current-state field like `status`: a client attaching mid-episode
        // recovers the pending-background count the one-shot events won't
        // replay for it (sweep exemption + chip).
        backgroundOutstanding: action.patch.backgroundOutstanding,
        // Authoritative route snapshot — never derived from live settings.
        delegationRoute: action.patch.delegationRoute,
        // Waiting projection is independent of status / turn_in_flight.
        waitingForSubagents: action.patch.waitingForSubagents,
        // Attach-replayable watchdog map (Task 8/9). Always replace with the
        // authoritative snapshot map on a fresh hydrate path.
        toolWatchdogProjections: action.patch.toolWatchdogProjections ?? {},
        // Seed complete per-lease terminal floors from the snapshot field
        // (multi-lease tombstones), then merge live map + last diagnostic so
        // older servers without tool_watchdog_max_versions still work.
        toolWatchdogMaxVersions: (() => {
          const maxVersions: Record<string, number> = {
            ...(action.patch.toolWatchdogMaxVersions ?? {}),
          }
          const map = action.patch.toolWatchdogProjections ?? {}
          for (const [id, p] of Object.entries(map)) {
            maxVersions[id] = Math.max(maxVersions[id] ?? 0, p.version)
          }
          const diag = action.patch.lastToolWatchdogDiagnostic
          if (diag) {
            maxVersions[diag.lease_id] = Math.max(
              maxVersions[diag.lease_id] ?? 0,
              diag.version
            )
          }
          return maxVersions
        })(),
        // Prefer server-retained last diagnostic (survives timed_out). Fall
        // back to the latest live projection by transition wall time — never
        // by per-lease version alone (versions restart at 1 per lease).
        lastToolWatchdogDiagnostic: (() => {
          const retained = action.patch.lastToolWatchdogDiagnostic ?? null
          const map = action.patch.toolWatchdogProjections ?? {}
          let best: import("@/lib/types").ToolWatchdogProjection | null =
            retained
          for (const p of Object.values(map)) {
            if (!best || isNewerDiagnosticProjection(p, best)) {
              best = p
            }
          }
          return best
        })(),
        // Fill-null conversation binding (draft tab → linked row).
        conversationId: mergedConversationId,
        sessionFailures: mergedSessionFailures,
        // Recover the latest runtime error only from a fresh snapshot. The
        // stale path above deliberately preserves the current cleared value.
        error: action.patch.lastError,
        attachError: null,
        lastAppliedSeq: action.patch.eventSeq,
        sharedSession: mergedSharedSession,
      })
      effects?.push(() =>
        publishRequestUsage(mergedConversationId, EMPTY_REQUEST_USAGE)
      )
      return next
    }

    case "CONTINUATION_WAITING_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (conn.waitingForSubagents === action.waiting) return state
      // Structural equality: avoid re-render when projection is identical.
      if (
        conn.waitingForSubagents &&
        action.waiting &&
        conn.waitingForSubagents.conversation_id ===
          action.waiting.conversation_id &&
        conn.waitingForSubagents.state === action.waiting.state &&
        conn.waitingForSubagents.generation === action.waiting.generation &&
        conn.waitingForSubagents.armed_at === action.waiting.armed_at &&
        conn.waitingForSubagents.wake_at === action.waiting.wake_at
      ) {
        return state
      }
      if (conn.waitingForSubagents == null && action.waiting == null) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        waitingForSubagents: action.waiting,
      })
      return next
    }

    case "TOOL_WATCHDOG_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const current = conn.toolWatchdogProjections ?? {}
      const { map: nextMap, maxVersionByLease: nextMax } =
        reduceToolWatchdogProjectionMap(
          current,
          action.projection,
          conn.toolWatchdogMaxVersions ?? {}
        )
      // Retain the latest transition for session-details (by transition_at),
      // including terminal timed_out/cleared that leave the live map.
      const prevDiag = conn.lastToolWatchdogDiagnostic ?? null
      const nextDiag =
        !prevDiag || isNewerDiagnosticProjection(action.projection, prevDiag)
          ? action.projection
          : prevDiag
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        toolWatchdogProjections: nextMap,
        toolWatchdogMaxVersions: nextMax,
        lastToolWatchdogDiagnostic: nextDiag,
      })
      return next
    }

    case "DELEGATION_ROUTE_AVAILABILITY": {
      const conn = state.get(action.contextKey)
      if (!conn?.delegationRoute) return state
      if (conn.delegationRoute.delegation_available === action.available) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        delegationRoute: {
          ...conn.delegationRoute,
          delegation_available: action.available,
        },
      })
      return next
    }

    case "EVENT_APPLIED": {
      const current = state.get(action.contextKey)
      if (!current) return state
      // Idempotent: only advances if the new seq is strictly higher.
      if (action.seq <= current.lastAppliedSeq) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...current,
        lastAppliedSeq: action.seq,
      })
      return next
    }

    case "CONNECTION_REMOVED": {
      const next = writableConnections(state, mutateUnpublished)
      next.delete(action.contextKey)
      return next
    }

    case "REMOVE_ALL":
      return new Map()

    case "REKEY_CONNECTION": {
      const conn = state.get(action.fromKey)
      if (!conn) return state
      // Defensive: if toKey already has an entry, do not clobber it.
      if (state.has(action.toKey)) return state
      const next = writableConnections(state, mutateUnpublished)
      next.delete(action.fromKey)
      next.set(action.toKey, { ...conn, contextKey: action.toKey })
      return next
    }

    case "STATUS_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      const updated = { ...conn, status: action.status }
      if (action.status === "prompting") {
        updated.liveMessage = {
          id: randomUUID(),
          role: "assistant",
          content: [],
          startedAt: Date.now(),
        }
        updated.requestEstimator = createRequestTokenEstimator()
        updated.requestUsage = EMPTY_REQUEST_USAGE
        updated.generationClockStartedAt = null
        effects?.push(() =>
          publishRequestUsage(conn.conversationId, EMPTY_REQUEST_USAGE)
        )
        updated.pendingQuestion = null
        updated.claudeApiRetry = null
        updated.error = null
        // Starting a prompt past an active AIR failure acknowledges it —
        // settle EVERYTHING (watermarks retained). A failure that is still
        // real re-arms via a higher revision on the same id.
        updated.sessionFailures = settleSessionFailures(
          conn.sessionFailures,
          "all"
        )
        // The out-of-turn window ended: its tool-call contexts (kept only for
        // background permission enrichment) are stale for the new turn.
        updated.outOfTurnToolCalls = null
      } else if (conn.status === "prompting") {
        updated.requestEstimator = discardEstimatedRequest(estimatorFor(conn))
        updated.generationClockStartedAt = null
        // Prompt cycle ended: clear in-flight Claude API retry banner.
        updated.claudeApiRetry = null
        // AIR failures deliberately NOT settled here: leaving `prompting`
        // covers error/cancel exits too, where the incident did not recover —
        // settling on any exit painted a still-dead connection as a recovered
        // warning. The `turn_complete` handler settles warnings on a clean
        // `end_turn` instead (SETTLE_SESSION_FAILURES), after the response's
        // terminal error escalation (if any) has already landed.
        // A blocked ask_user_question can't outlive its turn. The normal path
        // clears it via `question_resolved`; this is the safety net for a turn
        // that ended without one (agent error / abandoned block).
        updated.pendingAskQuestion = null
        // Likewise a blocked exit_plan_mode approval — cleared via
        // `plan_approval_resolved` normally; this is the turn-end safety net.
        updated.pendingPlanApproval = null
      }
      next.set(action.contextKey, updated)
      return next
    }

    case "SET_BACKGROUND_OUTSTANDING": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      // Settle-syncing bridge: a settlement whose reply arrives OUT OF TURN
      // (as a separate overlay turn) means that reply is being generated — arm
      // the "syncing results" hint to fill the gap until it surfaces. The
      // backend classifies this per settle via `wire_visible` (folded into
      // `outOfTurnSettleCount` by the handler): under claude-agent-acp #870 the
      // launching turn is held OPEN and the reply streams LIVE as its tail —
      // already on screen, no gap to bridge — so those are excluded. Arming for
      // a held settle would STRAND the hint: its reply never arrives as an
      // overlay `turns` event, so nothing disarms it and it sits until the 30s
      // cap (the "结果都出来了还显示 Syncing" bug). Using the backend flag rather
      // than the connection's current status is deliberate — it's correct even
      // when the watcher reads the settlement after the turn already fell back
      // to `connected`. The first genuinely out-of-turn `turns` event disarms.
      const syncingSince =
        action.outOfTurnSettleCount > 0
          ? Date.now()
          : action.turnsCount > 0
            ? null
            : conn.backgroundSettleSyncingSince
      if (
        conn.backgroundOutstanding === action.outstanding &&
        conn.backgroundSettleSyncingSince === syncingSince
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        backgroundOutstanding: action.outstanding,
        backgroundSettleSyncingSince: syncingSince,
      })
      return next
    }

    case "CONTENT_DELTA":
    case "THINKING": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const updated = applyStreamingAction(conn, action)
      if (!updated) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, updated)
      return next
    }

    case "TURN_ATTEMPT_ROLLBACK": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.status !== "prompting" || !conn.liveMessage) {
        return state
      }
      const currentEstimator = estimatorFor(conn)
      const requestEstimator = discardEstimatedRequest(currentEstimator)
      const liveMessage = rollbackLiveMessageAttempt(conn.liveMessage)
      if (
        liveMessage === conn.liveMessage &&
        requestEstimator === currentEstimator
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        liveMessage,
        requestEstimator,
        generationClockStartedAt: null,
      })
      return next
    }

    case "STREAM_BATCH": {
      if (action.actions.length === 0) return state
      const grouped = new Map<string, StreamingAction[]>()
      for (const streamAction of action.actions) {
        const list = grouped.get(streamAction.contextKey)
        if (list) {
          list.push(streamAction)
        } else {
          grouped.set(streamAction.contextKey, [streamAction])
        }
      }

      let next: ConnectionsMap | null = null

      for (const [contextKey, streamActions] of grouped) {
        const source = next ?? state
        const conn = source.get(contextKey)
        if (!conn) continue

        let updatedConn = conn
        let hasChange = false
        for (const streamAction of streamActions) {
          const updated = applyStreamingAction(updatedConn, streamAction)
          if (!updated) continue
          updatedConn = updated
          hasChange = true
        }
        if (!hasChange) continue

        if (!next) {
          next = writableConnections(state, mutateUnpublished)
        }
        next.set(contextKey, updatedConn)
      }

      return next ?? state
    }

    case "TOOL_CALL": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      // Out-of-turn wire tool activity stays OUT of `liveMessage` (the
      // transcript overlay renders that content — grafting it here recreated
      // the garbled-timeline bug), but its context is still recorded so a
      // background permission request can render command/diff details.
      if (conn.status !== "prompting") {
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, {
          ...conn,
          outOfTurnToolCalls: recordOutOfTurnToolCall(conn.outOfTurnToolCalls, {
            tool_call_id: action.tool_call_id,
            title: action.title,
            kind: action.kind,
            status: action.status,
            content: action.content,
            raw_input: action.raw_input,
            raw_output_chunks:
              action.raw_output !== null ? [action.raw_output] : [],
            raw_output_total_bytes: action.raw_output?.length ?? 0,
            locations: action.locations,
            meta: action.meta,
            images: action.images ?? [],
          }),
        })
        return next
      }
      const requestEstimator =
        conn.agentType === "codex" &&
        action.raw_input_is_model_authored === true &&
        action.raw_input != null &&
        action.raw_input.length > 0
          ? observeEstimatedSnapshot(
              estimatorFor(conn),
              `tool:${action.tool_call_id}`,
              action.raw_input,
              estimatorObservation(conn, action.receivedAt)
            )
          : estimatorFor(conn)
      const prev = ensureLiveMessage(conn.liveMessage)
      const existingIndex = prev.content.findIndex(
        (b) =>
          b.type === "tool_call" && b.info.tool_call_id === action.tool_call_id
      )
      let newContent: LiveContentBlock[]
      if (existingIndex !== -1) {
        const block = prev.content[existingIndex]
        if (block.type === "tool_call") {
          newContent = [
            ...prev.content.slice(0, existingIndex),
            {
              type: "tool_call",
              info: {
                ...block.info,
                title: action.title ?? block.info.title,
                kind: action.kind ?? block.info.kind,
                status: action.status ?? block.info.status,
                content: action.content ?? block.info.content,
                raw_input: action.raw_input ?? block.info.raw_input,
                raw_output_chunks:
                  action.raw_output !== null
                    ? [action.raw_output]
                    : block.info.raw_output_chunks,
                raw_output_total_bytes:
                  action.raw_output !== null
                    ? action.raw_output.length
                    : block.info.raw_output_total_bytes,
                images:
                  action.images !== null ? action.images : block.info.images,
              },
            },
            ...prev.content.slice(existingIndex + 1),
          ]
        } else {
          newContent = prev.content
        }
      } else {
        newContent = [
          ...prev.content,
          {
            type: "tool_call",
            info: {
              tool_call_id: action.tool_call_id,
              title: action.title,
              kind: action.kind,
              status: action.status,
              content: action.content,
              raw_input: action.raw_input,
              raw_output_chunks:
                action.raw_output !== null ? [action.raw_output] : [],
              raw_output_total_bytes: action.raw_output?.length ?? 0,
              locations: action.locations ?? null,
              meta: action.meta ?? null,
              images: action.images ?? [],
            },
          },
        ]
      }
      const nextLiveMessage = { ...prev, content: newContent }
      const nextInfo = findLiveToolCallInfo(newContent, action.tool_call_id)
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        liveMessage: nextLiveMessage,
        pendingPermission: mergePendingPermissionWithLiveInfo(
          conn.pendingPermission,
          nextInfo
        ),
        claudeApiRetry: null,
        requestEstimator,
      })
      return next
    }

    case "TOOL_CALL_UPDATE": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      // Out-of-turn: stay out of `liveMessage` (see TOOL_CALL), but merge the
      // registry entry and backfill an open permission dialog waiting for
      // this tool's input — a background permission must still show its
      // command/diff details. In-turn ordering is safe: the panel flushes
      // pending tool-call updates at turn_complete BEFORE status flips back.
      if (conn.status !== "prompting") {
        const existing = conn.outOfTurnToolCalls?.get(action.tool_call_id)
        const merged: ToolCallInfo = existing
          ? {
              ...existing,
              title: action.title ?? existing.title,
              status: action.status ?? existing.status,
              content: action.content ?? existing.content,
              raw_input: action.raw_input ?? existing.raw_input,
              locations: action.locations ?? existing.locations,
              meta: action.meta ?? existing.meta,
              images: action.images !== null ? action.images : existing.images,
            }
          : {
              tool_call_id: action.tool_call_id,
              title: action.title ?? action.fallback_title,
              kind: action.fallback_kind,
              status: action.status ?? "pending",
              content: action.content,
              raw_input: action.raw_input,
              raw_output_chunks: [],
              raw_output_total_bytes: 0,
              locations: action.locations,
              meta: action.meta,
              images: action.images ?? [],
            }
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, {
          ...conn,
          outOfTurnToolCalls: recordOutOfTurnToolCall(
            conn.outOfTurnToolCalls,
            merged
          ),
          pendingPermission: mergePendingPermissionWithLiveInfo(
            conn.pendingPermission,
            merged
          ),
        })
        return next
      }
      const requestEstimator =
        conn.agentType === "codex" &&
        action.raw_input_is_model_authored === true &&
        action.raw_input != null &&
        action.raw_input.length > 0
          ? observeEstimatedSnapshot(
              estimatorFor(conn),
              `tool:${action.tool_call_id}`,
              action.raw_input,
              estimatorObservation(conn, action.receivedAt)
            )
          : estimatorFor(conn)
      const prev = ensureLiveMessage(conn.liveMessage)
      const existingIndex = prev.content.findIndex(
        (b) =>
          b.type === "tool_call" && b.info.tool_call_id === action.tool_call_id
      )
      let newContent: LiveContentBlock[]

      if (existingIndex === -1) {
        const initialChunks =
          action.raw_output !== null ? [action.raw_output] : []
        const initialBytes = action.raw_output?.length ?? 0
        newContent = [
          ...prev.content,
          {
            type: "tool_call",
            info: {
              tool_call_id: action.tool_call_id,
              title: action.title ?? action.fallback_title,
              kind: action.fallback_kind,
              status:
                action.status ??
                (initialChunks.length > 0 ? "in_progress" : "pending"),
              content: action.content,
              raw_input: action.raw_input,
              raw_output_chunks: initialChunks,
              raw_output_total_bytes: initialBytes,
              locations: action.locations ?? null,
              meta: action.meta ?? null,
              images: action.images ?? [],
            },
          },
        ]
      } else {
        const block = prev.content[existingIndex]
        if (block.type !== "tool_call") return state

        let newChunks: string[]
        let newTotalBytes: number

        if (action.raw_output === null) {
          newChunks = block.info.raw_output_chunks
          newTotalBytes = block.info.raw_output_total_bytes
        } else if (action.raw_output_append) {
          newChunks = [...block.info.raw_output_chunks, action.raw_output]
          newTotalBytes =
            block.info.raw_output_total_bytes + action.raw_output.length

          // 超限时从头部批量移除 chunks（单次 slice 替代循环 shift）
          if (
            newTotalBytes > MAX_LIVE_TOOL_RAW_OUTPUT_CHARS &&
            newChunks.length > 1
          ) {
            let evictCount = 0
            let evictedBytes = 0
            while (
              evictCount < newChunks.length - 1 &&
              newTotalBytes - evictedBytes > MAX_LIVE_TOOL_RAW_OUTPUT_CHARS
            ) {
              evictedBytes += newChunks[evictCount].length
              evictCount++
            }
            if (evictCount > 0) {
              newChunks = newChunks.slice(evictCount)
              newTotalBytes -= evictedBytes
            }
          }
        } else {
          // 非 append 模式（替换）
          newChunks = [action.raw_output]
          newTotalBytes = action.raw_output.length
        }

        newContent = [
          ...prev.content.slice(0, existingIndex),
          {
            type: "tool_call" as const,
            info: {
              ...block.info,
              title: action.title ?? block.info.title,
              status: action.status ?? block.info.status,
              content: action.content ?? block.info.content,
              raw_input: action.raw_input ?? block.info.raw_input,
              raw_output_chunks: newChunks,
              locations: action.locations ?? block.info.locations,
              meta: action.meta ?? block.info.meta,
              raw_output_total_bytes: newTotalBytes,
              images:
                action.images !== null ? action.images : block.info.images,
            },
          },
          ...prev.content.slice(existingIndex + 1),
        ]
      }

      const nextLiveMessage = { ...prev, content: newContent }
      const nextInfo = findLiveToolCallInfo(newContent, action.tool_call_id)
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        liveMessage: nextLiveMessage,
        pendingPermission: mergePendingPermissionWithLiveInfo(
          conn.pendingPermission,
          nextInfo
        ),
        claudeApiRetry: null,
        requestEstimator,
      })
      return next
    }

    case "BATCH_TOOL_CALL_UPDATES": {
      let current = state
      for (const sub of action.actions) {
        current = reduceSingleAction(
          current,
          {
            type: "TOOL_CALL_UPDATE",
            ...sub,
          },
          mutateUnpublished,
          effects
        )
      }
      return current
    }

    case "PERMISSION_REQUEST": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      let updatedLiveMessage = conn.liveMessage
      const permissionCallId = extractPermissionToolCallId(action.tool_call)
      // Live tool context first; for an OUT-OF-TURN permission (background
      // sub-agent work — liveMessage intentionally untouched) fall back to
      // the out-of-turn registry so the dialog still shows command/diff.
      const existingInfo =
        (updatedLiveMessage
          ? findLiveToolCallInfo(updatedLiveMessage.content, permissionCallId)
          : null) ??
        (permissionCallId
          ? (conn.outOfTurnToolCalls?.get(permissionCallId) ?? null)
          : null)
      const permissionToolCall = mergePermissionToolCallWithLiveInfo(
        action.tool_call,
        existingInfo
      )
      const permissionToolInput =
        serializePermissionToolCall(permissionToolCall)
      if (
        updatedLiveMessage &&
        permissionCallId &&
        typeof permissionToolInput === "string"
      ) {
        const existingIndex = updatedLiveMessage.content.findIndex(
          (block) =>
            block.type === "tool_call" &&
            block.info.tool_call_id === permissionCallId
        )
        if (existingIndex !== -1) {
          const block = updatedLiveMessage.content[existingIndex]
          if (block.type === "tool_call") {
            const nextContent: LiveContentBlock[] = [
              ...updatedLiveMessage.content.slice(0, existingIndex),
              {
                type: "tool_call",
                info: {
                  ...block.info,
                  raw_input:
                    block.info.raw_input && block.info.raw_input.length > 0
                      ? block.info.raw_input
                      : permissionToolInput,
                },
              },
              ...updatedLiveMessage.content.slice(existingIndex + 1),
            ]
            updatedLiveMessage = {
              ...updatedLiveMessage,
              content: nextContent,
            }
          }
        } else {
          updatedLiveMessage = {
            ...updatedLiveMessage,
            content: [
              ...updatedLiveMessage.content,
              {
                type: "tool_call",
                info: {
                  tool_call_id: permissionCallId,
                  title:
                    extractPermissionToolTitle(action.tool_call) ??
                    action.fallback_title,
                  kind:
                    extractPermissionToolKind(action.tool_call) ??
                    action.fallback_kind,
                  status: "pending",
                  content: null,
                  raw_input: permissionToolInput,
                  raw_output_chunks: [],
                  raw_output_total_bytes: 0,
                  locations: null,
                  meta: null,
                  images: [],
                },
              },
            ],
          }
        }
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        liveMessage: updatedLiveMessage,
        pendingPermission: {
          request_id: action.request_id,
          tool_call: permissionToolCall,
          options: action.options,
          queued: action.queued,
        },
      })
      return next
    }

    case "PERMISSION_QUEUE_DEPTH": {
      // Depth-only: a request queued up behind the visible card, which emits no
      // PERMISSION_REQUEST of its own. No card up → nothing to annotate (a late
      // depth event after a drain must not resurrect one).
      const conn = state.get(action.contextKey)
      if (!conn?.pendingPermission) return state
      if (conn.pendingPermission.queued === action.depth) return state
      const next = new Map(state)
      next.set(action.contextKey, {
        ...conn,
        pendingPermission: {
          ...conn.pendingPermission,
          queued: action.depth,
        },
      })
      return next
    }

    case "PERMISSION_CLEARED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (
        action.requestId !== undefined &&
        conn.pendingPermission?.request_id !== action.requestId
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingPermission: null,
      })
      return next
    }

    case "SET_PENDING_QUESTION": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingQuestion: action.pendingQuestion,
      })
      return next
    }

    case "CLEAR_PENDING_QUESTION": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingQuestion: null,
      })
      return next
    }

    case "SET_ASK_QUESTION": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingAskQuestion: action.pendingAskQuestion,
      })
      return next
    }

    case "CLEAR_ASK_QUESTION": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (
        action.questionId !== undefined &&
        conn.pendingAskQuestion?.question_id !== action.questionId
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingAskQuestion: null,
      })
      return next
    }

    case "SET_PLAN_APPROVAL": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingPlanApproval: action.pendingPlanApproval,
      })
      return next
    }

    case "CLEAR_PLAN_APPROVAL": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (
        action.approvalId !== undefined &&
        conn.pendingPlanApproval?.approval_id !== action.approvalId
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        pendingPlanApproval: null,
      })
      return next
    }

    case "SESSION_STARTED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        sessionId: action.sessionId,
      })
      return next
    }

    case "CONVERSATION_LINKED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (conn.conversationId === action.conversationId) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        conversationId: action.conversationId,
      })
      const previousConversationId = conn.conversationId
      if (previousConversationId != null && previousConversationId !== 0) {
        effects?.push(() =>
          aliasRequestUsageIds(previousConversationId, action.conversationId)
        )
      }
      const runtimeId = resolveRuntimeConversationIdForOwnership(
        action.conversationId
      )
      if (runtimeId != null && runtimeId !== action.conversationId) {
        effects?.push(() =>
          aliasRequestUsageIds(runtimeId, action.conversationId)
        )
      }
      effects?.push(() =>
        publishRequestUsage(
          action.conversationId,
          conn.requestUsage ?? EMPTY_REQUEST_USAGE
        )
      )
      return next
    }

    case "SESSION_MODES": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (sameModes(conn.modes, action.modes)) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        modes: action.modes,
      })
      return next
    }

    case "SESSION_CONFIG_OPTIONS": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (sameConfigOptions(conn.configOptions, action.configOptions)) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        configOptions: action.configOptions,
      })
      return next
    }

    case "CONFIG_STALE_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const kind = action.stale ? action.kind : null
      // A fresh stale=true is a NEW drift → un-dismiss so the banner reappears
      // even if the user had dismissed a previous one. stale=false clears it.
      const dismissed = action.stale ? false : conn.configStaleDismissed
      if (
        conn.configStale === action.stale &&
        conn.configStaleKind === kind &&
        conn.configStaleDismissed === dismissed
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        configStale: action.stale,
        configStaleKind: kind,
        configStaleDismissed: dismissed,
      })
      return next
    }

    case "DISMISS_CONFIG_STALE": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.configStaleDismissed) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        configStaleDismissed: true,
      })
      return next
    }

    case "SELECTORS_READY": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.selectorsReady) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        selectorsReady: true,
      })
      return next
    }

    case "PROMPT_CAPABILITIES": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (
        samePromptCapabilities(
          conn.promptCapabilities,
          action.promptCapabilities
        )
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        promptCapabilities: action.promptCapabilities,
      })
      return next
    }

    case "FORK_SUPPORTED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      if (conn.supportsFork === action.supported) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        supportsFork: action.supported,
      })
      return next
    }

    case "MODE_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn?.modes) return state
      if (conn.modes.current_mode_id === action.modeId) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        modes: {
          ...conn.modes,
          current_mode_id: action.modeId,
        },
      })
      return next
    }

    case "CONFIG_OPTION_CHANGED": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const options =
        conn.configOptions ??
        selectorsCache.get(conn.agentType)?.configOptions ??
        null
      if (!options) return state
      const idx = options.findIndex((o) => o.id === action.configId)
      if (idx === -1) return state
      const opt = options[idx]
      if (
        opt.kind.type !== "select" ||
        opt.kind.current_value === action.valueId
      ) {
        return state
      }
      const updated = [...options]
      updated[idx] = {
        ...opt,
        kind: { ...opt.kind, current_value: action.valueId },
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, { ...conn, configOptions: updated })
      return next
    }

    case "PLAN_UPDATE": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      // Same out-of-turn guard as TOOL_CALL / streaming deltas.
      if (conn.status !== "prompting") return state
      const planText = action.entries.map((entry) => entry.content).join("\n")
      const currentEstimator = estimatorFor(conn)
      const requestEstimator =
        conn.agentType === "codex"
          ? observeEstimatedSnapshot(
              currentEstimator,
              "plan",
              planText,
              estimatorObservation(conn, action.receivedAt)
            )
          : currentEstimator
      const prev = ensureLiveMessage(conn.liveMessage)
      const nonPlanContent = prev.content.filter(
        (block) => block.type !== "plan"
      )
      const currentPlan = [...prev.content]
        .reverse()
        .find((block): block is { type: "plan"; entries: PlanEntryInfo[] } => {
          return block.type === "plan"
        })

      if (
        action.entries.length === 0 &&
        currentPlan === undefined &&
        nonPlanContent.length === prev.content.length
      ) {
        if (requestEstimator === currentEstimator) return state
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, { ...conn, requestEstimator })
        return next
      }

      const isAlreadyCanonicalPlan =
        currentPlan !== undefined &&
        samePlanEntries(currentPlan.entries, action.entries) &&
        prev.content.length === nonPlanContent.length + 1 &&
        prev.content[prev.content.length - 1]?.type === "plan"

      if (isAlreadyCanonicalPlan) {
        if (requestEstimator === currentEstimator) return state
        const next = writableConnections(state, mutateUnpublished)
        next.set(action.contextKey, { ...conn, requestEstimator })
        return next
      }

      const newContent =
        action.entries.length === 0
          ? nonPlanContent
          : [
              ...nonPlanContent,
              { type: "plan" as const, entries: action.entries },
            ]

      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        liveMessage: { ...prev, content: newContent },
        claudeApiRetry: null,
        requestEstimator,
      })
      return next
    }

    case "CLAUDE_API_RETRY": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        claudeApiRetry: action.retry,
      })
      return next
    }

    case "SESSION_FAILURE": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const merged = upsertSessionFailure(conn.sessionFailures, action.record)
      // Stale/replayed upserts are rejected by reference — no re-render.
      if (merged === conn.sessionFailures) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, { ...conn, sessionFailures: merged })
      return next
    }

    case "SETTLE_SESSION_FAILURES": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const settled = settleSessionFailures(conn.sessionFailures, action.scope)
      // Nothing needed settling — same reference, no re-render.
      if (settled === conn.sessionFailures) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, { ...conn, sessionFailures: settled })
      return next
    }

    case "DISMISS_SESSION_FAILURES": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const dismissed = dismissSessionFailures(conn.sessionFailures, action.ids)
      // Unknown ids / already resolved — same reference, no re-render.
      if (dismissed === conn.sessionFailures) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, { ...conn, sessionFailures: dismissed })
      return next
    }

    case "ERROR": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        claudeApiRetry: null,
        error: action.message,
      })
      return next
    }

    case "ATTACH_ERROR": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        attachError: { code: action.code, retryable: action.retryable },
      })
      return next
    }

    case "CLEAR_ATTACH_ERROR": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.attachError == null) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        attachError: null,
      })
      return next
    }

    case "ACP_LOAD_ERROR": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        loadError: action.message,
        loadErrorCode: action.code,
      })
      return next
    }

    case "CLEAR_ACP_LOAD_ERROR": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.loadError === null) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        loadError: null,
        loadErrorCode: null,
      })
      return next
    }

    case "AVAILABLE_COMMANDS": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const commands = dedupeCommandsByName(action.commands)
      if (sameCommands(conn.availableCommands, commands)) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        availableCommands: commands,
      })
      return next
    }

    case "GENERATION_CLOCK_START": {
      const conn = state.get(action.contextKey)
      if (!conn || conn.generationClockStartedAt != null) return state
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        generationClockStartedAt: action.at,
      })
      return next
    }

    case "REQUEST_USAGE": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const estimator = estimatorFor(conn)
      if (
        conn.agentType === "codex" &&
        !hasUnsettledEstimatedRequest(estimator)
      ) {
        return state
      }
      const measuredStart =
        conn.agentType === "codex"
          ? estimator.startedAt
          : conn.generationClockStartedAt
      const measuredDuration =
        measuredStart != null ? action.endedAt - measuredStart : 0
      const sample = resolveRequestUsageSample(
        { outputTokens: action.outputTokens, durationMs: action.durationMs },
        measuredDuration
      )
      const currentRequestUsage = conn.requestUsage ?? EMPTY_REQUEST_USAGE
      const requestUsage = accumulateRequestUsage(currentRequestUsage, sample)
      const requestEstimator =
        conn.agentType === "codex"
          ? discardEstimatedRequest(estimator)
          : estimator
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        requestUsage,
        requestEstimator,
        generationClockStartedAt: null,
      })
      if (requestUsage !== currentRequestUsage) {
        effects?.push(() =>
          publishRequestUsage(conn.conversationId, requestUsage)
        )
      }
      return next
    }

    case "USAGE_UPDATE": {
      const conn = state.get(action.contextKey)
      if (!conn) return state
      const currentEstimator = estimatorFor(conn)
      const settlement =
        conn.agentType === "codex"
          ? settleEstimatedRequest(currentEstimator, action.boundaryAt)
          : { state: currentEstimator, sample: null }
      const currentRequestUsage = conn.requestUsage ?? EMPTY_REQUEST_USAGE
      const requestUsage = settlement.sample
        ? accumulateRequestUsage(currentRequestUsage, settlement.sample)
        : currentRequestUsage

      let usage = conn.usage
      if (
        action.usage.size > 0 &&
        !(action.usage.used === 0 && usage && usage.used > 0) &&
        (usage?.used !== action.usage.used || usage?.size !== action.usage.size)
      ) {
        usage = action.usage
      }

      if (
        settlement.state === currentEstimator &&
        requestUsage === currentRequestUsage &&
        usage === conn.usage
      ) {
        return state
      }
      const next = writableConnections(state, mutateUnpublished)
      next.set(action.contextKey, {
        ...conn,
        requestEstimator: settlement.state,
        requestUsage,
        usage,
      })
      if (requestUsage !== currentRequestUsage) {
        effects?.push(() =>
          publishRequestUsage(conn.conversationId, requestUsage)
        )
      }
      return next
    }

    default:
      return state
  }
}

function connectionsReducer(
  state: ConnectionsMap,
  action: Action,
  effects?: ReducerEffect[]
): ConnectionsMap {
  if (action.type === "APPLY_EVENT_FRAME") {
    const next = writableConnections(state, false)
    let changed = false
    for (const frame of action.frames) {
      const beforeConn = next.get(frame.contextKey)
      if (!beforeConn) continue
      for (const item of frame.actions) {
        reduceSingleAction(next, item, true, effects)
      }
      const reduced = next.get(frame.contextKey)
      if (reduced && frame.highestSeq > reduced.lastAppliedSeq) {
        next.set(frame.contextKey, {
          ...reduced,
          lastAppliedSeq: frame.highestSeq,
        })
      }
      if (next.get(frame.contextKey) !== beforeConn) changed = true
    }
    return changed ? next : state
  }
  return reduceSingleAction(state, action, false, effects)
}

/** @internal — for frame-action parity tests. */
export function __connectionsReducerForTests(
  state: ConnectionsMap,
  action: Action
): ConnectionsMap {
  return connectionsReducer(state, action)
}

/** @internal */
export type __FrameActionForTests = FrameAction

// --- prepareMappedEnvelope + frame helpers (inserted before provider) ---

/**
 * Dual-path completion (design): status-edge / COMPLETE_TURN only promotes
 * live buffers. Accepted `turn_complete` with `termination_source ===
 * "user_stop"` is the **sole** starter for `RECORD_TURN_OUTCOME` +
 * `START_CANCEL_RECONCILE` (completion_seq = EventEnvelope.seq).
 *
 * Promotes only while the cancelled completion still owns the session.
 * A late envelope after a next prompt (cancelGeneration advanced past the
 * Stop snapshot — even if the next turn already completed and cleared
 * activeTurnToken) is rejected. When still current, promotes before outcome
 * attach so reverse envelope→status-edge order attaches to the cancelled
 * assistant.
 */
export function acceptUserStopTurnComplete(params: {
  sessionId: string
  connectionId: string
  completionSeq: number
  stopReason: string
  terminationSource?: "user_stop" | null
  providerTurnId?: string | null
  snapshotConversationId?: number | null
}): void {
  if (params.terminationSource !== "user_stop") return

  const conversationId =
    getConversationIdByExternalIdFromStore(params.sessionId) ??
    params.snapshotConversationId ??
    useAppWorkspaceStore
      .getState()
      .conversations.find((c) => c.external_id === params.sessionId)?.id ??
    null
  if (conversationId == null) return

  // Ownership fence: Cancel snapshotted cancelGeneration (+ token). If a
  // newer prompt (or other lifecycle bump) advanced generation, this envelope
  // is stale — do not promote/attach/start under the newer transcript.
  if (isStaleUserStopEnvelope(conversationId)) {
    return
  }

  const prePromote = useConversationRuntimeStore
    .getState()
    .byConversationId.get(conversationId)
  if (!prePromote) return

  // Prefer cancel-time ownership token; fall back to pre-promote session token.
  const fenceToken = getUserStopFenceToken(conversationId) ?? null

  // Promote only while undrained cancel buffers remain (avoids COMPLETE_TURN
  // "already-drained" warnings when status-edge already promoted).
  const needsPromote =
    prePromote.liveMessage != null || prePromote.optimisticTurns.length > 0
  if (needsPromote) {
    completeLiveTranscriptTurn(conversationId)
  }

  const providerTurnId =
    typeof params.providerTurnId === "string" &&
    params.providerTurnId.length > 0
      ? params.providerTurnId
      : null

  const outcome: TurnOutcome = {
    status: "interrupted",
    stop_reason: "cancelled",
    source: "user_stop",
    provider_turn_id: providerTurnId,
  }

  const runtimeActions = useConversationRuntimeStore.getState().actions
  const outcomeStatus = runtimeActions.recordTurnOutcome({
    conversationId,
    connectionId: params.connectionId,
    completionSeq: params.completionSeq,
    outcome,
  })

  // First unbound/missing-provider acceptance is terminal for coordinator
  // start. Track completion identity so redelivery after migrate/bind never
  // starts cancel reconcile (outcome footer remains idempotent separately).
  if (
    isUserStopNoCoordinatorCompletion(params.connectionId, params.completionSeq)
  ) {
    enterOwnerPreserve(conversationId)
    return
  }

  // Duplicate completion: decision already made on first accept — do not
  // re-run coordinator transition logic.
  if (outcomeStatus === "duplicate") {
    return
  }

  // Coordinator start gates: non-empty provider id + positive persisted
  // conversation id. Missing provider id or unbound detail id (≤0) still
  // record the outcome above, enter durable owner_preserve, and skip the
  // coordinator (design Round 4e).
  if (params.stopReason === "cancelled" && providerTurnId) {
    const sessionAfter = useConversationRuntimeStore
      .getState()
      .byConversationId.get(conversationId)
    const persistedId = sessionAfter?.dbConversationId ?? conversationId
    if (persistedId > 0) {
      runtimeActions.startCancelReconcile({
        conversationId,
        connectionId: params.connectionId,
        completionSeq: params.completionSeq,
        providerTurnId,
        activeTurnToken: fenceToken,
      })
    } else {
      markUserStopNoCoordinatorCompletion(
        params.connectionId,
        params.completionSeq
      )
      enterOwnerPreserve(conversationId)
    }
  } else if (params.stopReason === "cancelled") {
    // user_stop accepted without provider_turn_id.
    markUserStopNoCoordinatorCompletion(
      params.connectionId,
      params.completionSeq
    )
    enterOwnerPreserve(conversationId)
  }
}

interface PreparedEnvelope {
  actions: FrameAction[]
  afterCommit: Array<() => void>
}

interface PrepareEnv {
  t: (key: string, params?: Record<string, string | number>) => string
  tChat: (key: string, params?: Record<string, string | number>) => string
  folderName: string | undefined
  pushAlert: (
    kind: "error" | "warning",
    title: string,
    message: string,
    actions?: AlertAction[]
  ) => void
}

function sharedPhaseFromWire(
  phase: SharedSessionPhase
): SharedSessionPhaseView {
  switch (phase.phase) {
    case "reserved":
      return { phase: "reserved" }
    case "bootstrapping":
      return { phase: "bootstrapping" }
    case "ready":
      return { phase: "ready" }
    case "failed":
      return {
        phase: "failed",
        errorCode: phase.error_code,
        cleanupComplete: phase.cleanup_complete,
      }
    case "closing":
      return { phase: "closing" }
  }
}

function sharedPhaseFromResponse(
  response: AcpConnectOrAttachResponse
): SharedSessionPhaseView {
  if (response.phase === "failed") {
    return {
      phase: "failed",
      errorCode: response.error?.code ?? "shared_session_failed",
      cleanupComplete: response.error?.cleanupComplete ?? false,
    }
  }
  return { phase: response.phase }
}

function isSharedInteractionConvergenceError(error: unknown): boolean {
  const code = extractAppCommandError(error)?.code
  return code === "interaction_already_resolved" || code === "stale_turn"
}

function sharedQueueItemFromWire(
  item: import("@/lib/types").SharedQueuedPromptSummary
): SharedQueuedPrompt {
  return {
    queueItemId: item.queue_item_id,
    enqueueSeq: item.enqueue_seq,
    clientMessageId: item.client_message_id,
    visibleText: item.visible_text,
    visibleTextTruncated: item.visible_text_truncated,
    attachmentCount: item.attachment_count,
    submittedAt: item.submitted_at,
    state: item.state,
  }
}

function mergeSharedQueueWithRetainedFailures(
  current: SharedQueuedPrompt[],
  incoming: SharedQueuedPrompt[],
  incomingEventSeq: number
): SharedQueuedPrompt[] {
  const incomingIds = new Set(incoming.map((item) => item.queueItemId))
  const retainedFailures = current.filter(
    (item) =>
      item.state === "failed" &&
      !incomingIds.has(item.queueItemId) &&
      item.failureEventSeq != null &&
      item.failureEventSeq >= incomingEventSeq
  )
  return retainedFailures.length > 0
    ? [...incoming, ...retainedFailures]
    : incoming
}

function mergeSharedActiveTurnPromptSummary(
  current: SharedActiveTurn | null,
  incoming: SharedActiveTurn | null
): SharedActiveTurn | null {
  if (
    !current ||
    !incoming ||
    current.turnId !== incoming.turnId ||
    current.queueItemId !== incoming.queueItemId ||
    current.promptSummary === undefined
  ) {
    return incoming
  }
  return { ...incoming, promptSummary: current.promptSummary }
}

function sharedTurnFromWire(
  turn: import("@/lib/types").SharedActiveTurnProjection
): SharedActiveTurn {
  return {
    turnId: turn.turn_id,
    queueItemId: turn.queue_item_id,
    enqueueSeq: turn.enqueue_seq,
    clientMessageId: turn.client_message_id,
    stopRequested: turn.stop_requested,
  }
}

function prepareMappedEnvelope(
  contextKey: string,
  envelope: EventEnvelope,
  snapshot: ConnectionState,
  env: PrepareEnv
): PreparedEnvelope {
  const actions: FrameAction[] = []
  const afterCommit: Array<() => void> = []
  const e = envelope

  switch (e.type) {
    case "shared_session_phase_changed":
    case "prompt_queued":
    case "prompt_queue_item_cancelled":
    case "prompt_dispatch_started":
    case "prompt_queue_item_failed":
    case "prompt_queue_depth_changed":
    case "shared_turn_settled":
      actions.push({ type: "SHARED_SESSION_EVENT", contextKey, event: e })
      break
    case "status_changed":
      actions.push({ type: "STATUS_CHANGED", contextKey, status: e.status })
      break
    case "content_delta":
      if (!e.parent_tool_use_id && snapshot.generationClockStartedAt == null) {
        actions.push({
          type: "GENERATION_CLOCK_START",
          contextKey,
          at: requiredReceivedAt(e),
        })
      }
      actions.push({
        type: "CONTENT_DELTA",
        contextKey,
        text: e.text,
        parentToolUseId: e.parent_tool_use_id ?? undefined,
        receivedAt: requiredReceivedAt(e),
      })
      if (hasSettleableRetryIncident(snapshot.sessionFailures)) {
        actions.push({
          type: "SETTLE_SESSION_FAILURES",
          contextKey,
          scope: "retry_incidents",
        })
      }
      break
    case "thinking":
      if (!e.parent_tool_use_id && snapshot.generationClockStartedAt == null) {
        actions.push({
          type: "GENERATION_CLOCK_START",
          contextKey,
          at: requiredReceivedAt(e),
        })
      }
      actions.push({
        type: "THINKING",
        contextKey,
        text: e.text,
        parentToolUseId: e.parent_tool_use_id ?? undefined,
        receivedAt: requiredReceivedAt(e),
      })
      if (hasSettleableRetryIncident(snapshot.sessionFailures)) {
        actions.push({
          type: "SETTLE_SESSION_FAILURES",
          contextKey,
          scope: "retry_incidents",
        })
      }
      break
    case "turn_attempt_rollback":
      actions.push({ type: "TURN_ATTEMPT_ROLLBACK", contextKey })
      break
    case "claude_sdk_message":
      actions.push({
        type: "CLAUDE_API_RETRY",
        contextKey,
        retry: parseClaudeApiRetryEvent(e),
      })
      break
    case "tool_call":
      if (snapshot.generationClockStartedAt == null) {
        actions.push({
          type: "GENERATION_CLOCK_START",
          contextKey,
          at: e.received_at ?? performance.now(),
        })
      }
      actions.push({
        type: "TOOL_CALL",
        contextKey,
        tool_call_id: e.tool_call_id,
        title: e.title,
        kind: e.kind,
        status: e.status,
        content: e.content,
        raw_input: e.raw_input,
        raw_input_is_model_authored: e.raw_input_is_model_authored,
        raw_output: e.raw_output,
        locations: e.locations ?? null,
        meta: (e.meta as ToolCallMeta) ?? null,
        images: e.images ?? null,
        receivedAt: requiredReceivedAt(e),
      })
      // A new tool call is model output — the same recovery evidence as a
      // content delta. Status-only `tool_call_update` is not.
      if (hasSettleableRetryIncident(snapshot.sessionFailures)) {
        actions.push({
          type: "SETTLE_SESSION_FAILURES",
          contextKey,
          scope: "retry_incidents",
        })
      }
      break
    case "tool_call_update":
      actions.push({
        type: "TOOL_CALL_UPDATE",
        contextKey,
        tool_call_id: e.tool_call_id,
        title: e.title,
        fallback_title: env.t("toolFallbackTitle"),
        fallback_kind: "tool",
        status: e.status,
        content: e.content,
        raw_input: e.raw_input,
        raw_input_is_model_authored: e.raw_input_is_model_authored,
        raw_output: e.raw_output,
        raw_output_append: e.raw_output_append,
        locations: e.locations ?? null,
        meta: (e.meta as ToolCallMeta) ?? null,
        images: e.images ?? null,
        receivedAt: requiredReceivedAt(e),
      })
      break
    case "permission_resolved":
      actions.push({
        type: "PERMISSION_CLEARED",
        contextKey,
        requestId: e.request_id,
      })
      break
    case "question_request":
      actions.push({
        type: "SET_ASK_QUESTION",
        contextKey,
        pendingAskQuestion: {
          question_id: e.question_id,
          questions: e.questions,
          created_at: new Date().toISOString(),
        },
      })
      break
    case "question_resolved":
      actions.push({
        type: "CLEAR_ASK_QUESTION",
        contextKey,
        questionId: e.question_id,
      })
      break
    case "plan_approval_request":
      actions.push({
        type: "SET_PLAN_APPROVAL",
        contextKey,
        pendingPlanApproval: {
          approval_id: e.approval_id,
          tool_call_id: e.tool_call_id,
          plan_markdown: e.plan_markdown,
          created_at: new Date().toISOString(),
        },
      })
      break
    case "plan_approval_resolved":
      actions.push({
        type: "CLEAR_PLAN_APPROVAL",
        contextKey,
        approvalId: e.approval_id,
      })
      break
    case "background_activity": {
      actions.push({
        type: "SET_BACKGROUND_OUTSTANDING",
        contextKey,
        outstanding: e.outstanding,
        outOfTurnSettleCount:
          e.settled?.filter((settlement) => !settlement.wire_visible).length ??
          0,
        turnsCount: e.turns?.length ?? 0,
      })
      const sessionId = e.session_id
      const turns = e.turns
      const watermark = e.watermark
      const settled = e.settled
      const detailRefetch = e.detail_refetch === true
      const transcriptReset = e.transcript_reset === true
      const agentType = snapshot.agentType
      afterCommit.push(() => {
        const conversationId = getConversationIdByExternalIdFromStore(sessionId)
        if ((turns && turns.length > 0) || transcriptReset) {
          if (conversationId != null) {
            const runtime = useConversationRuntimeStore.getState()
            const overlayEvicted = runtime.actions.applyBackgroundActivity(
              conversationId,
              turns ?? [],
              watermark,
              transcriptReset
            )
            const session = useConversationRuntimeStore
              .getState()
              .byConversationId.get(conversationId)
            const now = Date.now()
            const lastAt = overlayFoldRefetchAt.get(conversationId) ?? 0
            if (
              session &&
              session.backgroundTurns.length > OVERLAY_FOLD_THRESHOLD &&
              !detailRefetch &&
              (overlayEvicted ||
                (!session.detailLoading &&
                  now - lastAt > OVERLAY_FOLD_MIN_INTERVAL_MS))
            ) {
              overlayFoldRefetchAt.set(conversationId, now)
              runtime.actions.refetchDetail(conversationId, {
                preserveLive: true,
              })
            }
          }
        }
        if (detailRefetch && conversationId != null) {
          useConversationRuntimeStore
            .getState()
            .actions.refetchDetail(conversationId, {
              preserveLive: true,
            })
        }
        if (settled && settled.length > 0) {
          const agentLabel = getAgentLabel(agentType)
          const fn = env.folderName
          const title = fn ? `${fn} - DrawCode` : "DrawCode"
          for (const item of settled) {
            const body =
              item.summary ??
              env.tChat("backgroundTasks.settledFallback", {
                status: item.status,
              })
            sendSystemNotification(title, `${agentLabel}: ${body}`).catch(
              () => {}
            )
          }
          if (conversationId != null) {
            const runtimeActions =
              useConversationRuntimeStore.getState().actions
            for (const item of settled) {
              if (!item.tool_use_id) continue
              runtimeActions.resolveBackgroundTask(conversationId, {
                toolUseId: item.tool_use_id,
                taskId: item.task_id,
                status: item.status,
                summary: item.summary ?? null,
                result: item.result ?? null,
              })
            }
          }
        }
      })
      break
    }
    case "permission_request": {
      actions.push({
        type: "PERMISSION_REQUEST",
        contextKey,
        request_id: e.request_id,
        tool_call: e.tool_call,
        fallback_title: env.t("toolFallbackTitle"),
        fallback_kind: "tool",
        options: e.options,
      })
      const agentLabel = getAgentLabel(snapshot.agentType)
      const fn = env.folderName
      afterCommit.push(() => {
        const title = fn ? `${fn} - DrawCode` : "DrawCode"
        sendSystemNotification(
          title,
          `${agentLabel}: ${env.tChat("permissionDialog.subtitle")}`
        ).catch(() => {})
      })
      break
    }
    case "session_started":
      actions.push({
        type: "SESSION_STARTED",
        contextKey,
        sessionId: e.session_id,
      })
      break
    case "conversation_linked": {
      actions.push({
        type: "CONVERSATION_LINKED",
        contextKey,
        conversationId: e.conversation_id,
      })
      const payload = {
        contextKey,
        connectionId: e.connection_id,
        conversationId: e.conversation_id,
        folderId: e.folder_id,
      }
      afterCommit.push(() => {
        console.log("[acp-context] conversation_linked", payload)
      })
      break
    }
    case "session_modes": {
      actions.push({
        type: "SESSION_MODES",
        contextKey,
        modes: e.modes,
      })
      const agentType = snapshot.agentType
      const modes = e.modes
      afterCommit.push(() => {
        const entry = selectorsCache.get(agentType) ?? {
          modes: null,
          configOptions: null,
        }
        entry.modes = modes
        selectorsCache.set(agentType, entry)
      })
      break
    }
    case "session_config_options": {
      const configOptions = filterSessionConfigOptions(e.config_options) ?? []
      actions.push({
        type: "SESSION_CONFIG_OPTIONS",
        contextKey,
        configOptions,
      })
      const agentType = snapshot.agentType
      afterCommit.push(() => {
        const entry = selectorsCache.get(agentType) ?? {
          modes: null,
          configOptions: null,
        }
        entry.configOptions = configOptions
        selectorsCache.set(agentType, entry)
      })
      break
    }
    case "session_config_stale":
      actions.push({
        type: "CONFIG_STALE_CHANGED",
        contextKey,
        stale: e.stale,
        kind: e.kind,
      })
      break
    case "delegation_availability_changed":
      actions.push({
        type: "DELEGATION_ROUTE_AVAILABILITY",
        contextKey,
        available: e.available,
      })
      break
    case "continuation_waiting_changed":
      actions.push({
        type: "CONTINUATION_WAITING_CHANGED",
        contextKey,
        waiting: e.waiting,
      })
      break
    case "tool_watchdog_changed": {
      const projection = e.projection
      actions.push({
        type: "TOOL_WATCHDOG_CHANGED",
        contextKey,
        projection,
      })
      // Desktop-only hidden-app system notification once per (lease_id, version).
      // Server/Web never dispatches a browser Notification for watchdog warnings.
      // No tool input / command / prompt in title or body.
      if (
        (projection.phase === "warning" || projection.phase === "grace") &&
        getTransport().isDesktop()
      ) {
        const conversationId = snapshot.conversationId ?? null
        const agentLabel = getAgentLabel(snapshot.agentType)
        const fn = env.folderName
        const leaseId = projection.lease_id
        const version = projection.version
        afterCommit.push(() => {
          maybeNotifyToolWatchdog(
            leaseId,
            version,
            fn ? `${fn} - DrawCode` : "DrawCode",
            `${agentLabel}: a foreground tool appears stalled`,
            conversationId
          )
        })
      }
      break
    }
    case "selectors_ready": {
      actions.push({ type: "SELECTORS_READY", contextKey })
      const agentType = snapshot.agentType
      const modes = snapshot.modes
      const configOptions = snapshot.configOptions
      afterCommit.push(() => {
        if (!selectorsCache.has(agentType)) {
          selectorsCache.set(agentType, { modes, configOptions })
        }
      })
      break
    }
    case "prompt_capabilities":
      actions.push({
        type: "PROMPT_CAPABILITIES",
        contextKey,
        promptCapabilities: e.prompt_capabilities,
      })
      break
    case "fork_supported":
      actions.push({
        type: "FORK_SUPPORTED",
        contextKey,
        supported: e.supported,
      })
      break
    case "mode_changed":
      actions.push({
        type: "MODE_CHANGED",
        contextKey,
        modeId: e.mode_id,
      })
      break
    case "plan_update":
      actions.push({
        type: "PLAN_UPDATE",
        contextKey,
        entries: e.entries,
        receivedAt: requiredReceivedAt(e),
      })
      break
    case "session_failure":
      // JetBrains AIR typed session failure upsert — merged monotonically
      // by id+revision (stale/replayed records are dropped in the reducer).
      actions.push({
        type: "SESSION_FAILURE",
        contextKey,
        record: e.record,
      })
      break
    case "turn_retrying":
      actions.push({
        type: "CLAUDE_API_RETRY",
        contextKey,
        retry: {
          sessionId: snapshot.sessionId ?? "",
          attempt: null,
          maxRetries: null,
          error: e.message,
          errorStatus: e.error_status ?? null,
          retryDelayMs: null,
        },
      })
      break
    case "turn_complete": {
      // AIR retry warnings settle only at a CLEAN turn end, mirroring the
      // backend's `apply_event`. A failed turn's terminal failure rides the
      // prompt response and was emitted as a `session_failure` event just
      // before this one, so settling here can no longer paint an unrecovered
      // incident as recovered.
      if (e.stop_reason === "end_turn") {
        actions.push({
          type: "SETTLE_SESSION_FAILURES",
          contextKey,
          scope: "warnings",
        })
      }
      actions.push({ type: "PERMISSION_CLEARED", contextKey })
      actions.push({
        type: "STATUS_CHANGED",
        contextKey,
        status: "connected",
      })
      if (snapshot.liveMessage) {
        const blocks = snapshot.liveMessage.content
        for (let i = blocks.length - 1; i >= 0; i--) {
          const block = blocks[i]
          if (block.type !== "tool_call") continue
          const normalized = inferLiveToolName({
            title: block.info.title,
            kind: block.info.kind,
            rawInput: block.info.raw_input,
            meta: block.info.meta,
          })
          if (normalized === "question") {
            const questionText = extractQuestionText(block.info.raw_input)
            if (questionText) {
              actions.push({
                type: "SET_PENDING_QUESTION",
                contextKey,
                pendingQuestion: {
                  tool_call_id: block.info.tool_call_id,
                  question: questionText,
                },
              })
            }
            break
          }
        }
      }
      // Dual-path: only typed user_stop may record outcome + start coordinator.
      // completion_seq = EventEnvelope.seq. Status-edge remains promotion-only.
      if (e.termination_source === "user_stop") {
        const sessionId = e.session_id
        const connectionId = e.connection_id
        const completionSeq = e.seq
        const stopReason = e.stop_reason
        const providerTurnId = e.provider_turn_id ?? null
        const snapshotConversationId = snapshot.conversationId ?? null
        afterCommit.push(() => {
          acceptUserStopTurnComplete({
            sessionId,
            connectionId,
            completionSeq,
            stopReason,
            terminationSource: "user_stop",
            providerTurnId,
            snapshotConversationId,
          })
        })
      }
      const agentLabel = getAgentLabel(snapshot.agentType)
      const fn = env.folderName
      afterCommit.push(() => {
        const title = fn ? `${fn} - DrawCode` : "DrawCode"
        sendSystemNotification(
          title,
          env.t("notificationTurnComplete", { agent: agentLabel })
        ).catch(() => {})
      })
      break
    }
    case "error": {
      const agentLabel = getAgentLabel(snapshot.agentType)
      const localizedMessage = (() => {
        // Shared mapping with cold detail projection — keep in sync via
        // continuationFailureI18nKey so live events cannot drift.
        if (isContinuationFailureCode(e.code)) {
          return env.t(continuationFailureI18nKey(e.code))
        }
        switch (e.code) {
          case "initialize_timeout":
            return env.t("backendErrors.initializeTimeout", {
              agent: agentLabel,
            })
          case "sdk_not_installed":
            return env.t("blocked.sdkMissing", { agent: agentLabel })
          case "platform_not_supported":
            return env.t("blocked.unavailable", { agent: agentLabel })
          case "process_exited":
            return env.t("backendErrors.processExited", { agent: agentLabel })
          case "spawn_failed":
            return env.t("backendErrors.spawnFailed", {
              agent: agentLabel,
              message: e.message,
            })
          case "download_failed":
            return env.t("backendErrors.downloadFailed", {
              agent: agentLabel,
              message: e.message,
            })
          case "turn_failed_refusal":
            return env.t("backendErrors.turnFailedRefusal", {
              agent: agentLabel,
            })
          case "turn_failed_max_tokens":
            return env.t("backendErrors.turnFailedMaxTokens", {
              agent: agentLabel,
            })
          case "turn_failed_max_turn_requests":
            return env.t("backendErrors.turnFailedMaxTurnRequests", {
              agent: agentLabel,
            })
          case "turn_failed_unknown":
            return env.t("backendErrors.turnFailedUnknown", {
              agent: agentLabel,
            })
          case "turn_failed_empty":
            return env.t("backendErrors.turnFailedEmpty", { agent: agentLabel })
          case "grok_model_switch_incompatible_agent":
            return env.t("backendErrors.grokModelSwitchIncompatibleAgent", {
              agent: agentLabel,
            })
          default:
            return e.message
        }
      })()
      actions.push({ type: "ERROR", contextKey, message: localizedMessage })
      afterCommit.push(() => {
        env.pushAlert("error", env.t("eventErrorTitle"), localizedMessage)
        const fn = env.folderName
        const title = fn ? `${fn} - DrawCode` : "DrawCode"
        sendSystemNotification(
          title,
          env.t("notificationError", {
            agent: agentLabel,
            message: localizedMessage,
          })
        ).catch(() => {})
      })
      break
    }
    case "session_load_failed": {
      const agentLabel = getAgentLabel(snapshot.agentType)
      const localizedMessage = (() => {
        switch (e.code) {
          case "resource_not_found":
            return env.t("backendErrors.sessionLoadResourceNotFound", {
              agent: agentLabel,
            })
          case "session_unavailable":
            return env.t("backendErrors.sessionLoadUnavailable", {
              agent: agentLabel,
            })
          case "legacy_cli_session":
            return env.t("backendErrors.sessionLoadLegacyCliSession", {
              agent: agentLabel,
            })
          default:
            return e.message
        }
      })()
      actions.push({
        type: "ACP_LOAD_ERROR",
        contextKey,
        message: localizedMessage,
        code: e.code,
      })
      break
    }
    case "available_commands":
      actions.push({
        type: "AVAILABLE_COMMANDS",
        contextKey,
        commands: e.commands,
      })
      break
    case "usage_update":
      actions.push({
        type: "USAGE_UPDATE",
        contextKey,
        usage: { used: e.used, size: e.size },
        boundaryAt: requiredReceivedAt(e),
      })
      break
    case "request_usage": {
      const endedAt = requiredReceivedAt(e)
      actions.push({
        type: "REQUEST_USAGE",
        contextKey,
        outputTokens: e.output_tokens,
        durationMs: e.duration_ms && e.duration_ms > 0 ? e.duration_ms : null,
        endedAt,
      })
      break
    }
    case "user_message":
    case "user_prompt_sent":
    case "conversation_status_changed":
    case "delegation_started":
    case "delegation_completed":
    case "delegation_observation_changed":
    case "delegation_runtime_stats_changed":
    case "delegation_attention_changed":
    case "feedback_submitted":
    case "feedback_consumed":
      // No store mutations; raw subscribers (e.g. DelegationProvider) still run.
      // Do NOT add delegation_availability_changed here — it mutates route
      // availability via DELEGATION_ROUTE_AVAILABILITY above.
      break
    default: {
      const unknown = envelope as EventEnvelope & { type: string }
      console.warn("[acp-context] unknown ACP event type", {
        type: unknown.type,
      })
      break
    }
  }

  return { actions, afterCommit }
}

interface PreparedEventFrame {
  connections: PreparedConnectionFrame[]
  afterCommit: Array<() => void>
  changedConnections: Array<{
    contextKey: string
    deliveryIds: readonly number[]
  }>
  renderChangedConnections: Array<{ contextKey: string }>
}

function resolveContextKeyForConnection(
  connectionId: string,
  fallbackKey: string,
  reverseMap: Map<string, string>,
  connections: ConnectionsMap
): string {
  const fromReverse = reverseMap.get(connectionId)
  if (fromReverse) return fromReverse
  for (const [key, conn] of connections) {
    if (conn.connectionId === connectionId) return key
  }
  return fallbackKey
}

function onlyCursorChanged(
  before: ConnectionState | undefined,
  after: ConnectionState | undefined
): boolean {
  if (!before || !after) return false
  if (before === after) return true
  if (before.lastAppliedSeq === after.lastAppliedSeq) return false
  return (
    before.connectionId === after.connectionId &&
    before.status === after.status &&
    before.liveMessage === after.liveMessage &&
    before.pendingPermission === after.pendingPermission &&
    before.pendingQuestion === after.pendingQuestion &&
    before.pendingAskQuestion === after.pendingAskQuestion &&
    before.pendingUserMessage === after.pendingUserMessage &&
    before.sessionId === after.sessionId &&
    before.modes === after.modes &&
    before.configOptions === after.configOptions &&
    before.availableCommands === after.availableCommands &&
    before.usage === after.usage &&
    before.error === after.error &&
    before.loadError === after.loadError &&
    before.loadErrorCode === after.loadErrorCode &&
    before.selectorsReady === after.selectorsReady &&
    before.supportsFork === after.supportsFork &&
    before.promptCapabilities === after.promptCapabilities &&
    before.claudeApiRetry === after.claudeApiRetry &&
    before.configStale === after.configStale &&
    before.configStaleKind === after.configStaleKind &&
    before.configStaleDismissed === after.configStaleDismissed &&
    before.backgroundOutstanding === after.backgroundOutstanding &&
    before.backgroundSettleSyncingSince ===
      after.backgroundSettleSyncingSince &&
    before.sharedSession === after.sharedSession &&
    before.sessionFailures === after.sessionFailures &&
    before.outOfTurnToolCalls === after.outOfTurnToolCalls &&
    before.isViewer === after.isViewer &&
    before.isDelegationChild === after.isDelegationChild &&
    before.delegationRoute === after.delegationRoute &&
    before.conversationId === after.conversationId &&
    before.delegationRouteOverride === after.delegationRouteOverride &&
    before.waitingForSubagents === after.waitingForSubagents &&
    before.toolWatchdogProjections === after.toolWatchdogProjections
  )
}

function prepareEventFrame(
  frame: AcceptedEventFrame,
  connections: ConnectionsMap,
  reverseMap: Map<string, string>,
  env: PrepareEnv
): PreparedEventFrame {
  // Progressive draft so later envelopes in the same frame see prior effects.
  let draft = new Map(connections)
  const preparedConnections: PreparedConnectionFrame[] = []
  const afterCommit: Array<() => void> = []
  const changedConnections: PreparedEventFrame["changedConnections"] = []
  const renderChangedConnections: PreparedEventFrame["renderChangedConnections"] =
    []

  for (const connFrame of frame.connections) {
    const contextKey = resolveContextKeyForConnection(
      connFrame.connectionId,
      connFrame.contextKey,
      reverseMap,
      draft
    )
    let snapshot = draft.get(contextKey)
    if (!snapshot) continue

    const before = connections.get(contextKey) ?? snapshot
    const actions: FrameAction[] = []
    for (const event of connFrame.applyEvents) {
      const prepared = prepareMappedEnvelope(contextKey, event, snapshot, env)
      actions.push(...prepared.actions)
      afterCommit.push(...prepared.afterCommit)
      for (const item of prepared.actions) {
        draft = reduceSingleAction(draft, item, false)
      }
      const nextSnap = draft.get(contextKey)
      if (nextSnap) snapshot = nextSnap
    }

    // Cursor advance (mirrors APPLY_EVENT_FRAME)
    if (snapshot && connFrame.highestSeq > snapshot.lastAppliedSeq) {
      const advanced: ConnectionState = {
        ...snapshot,
        lastAppliedSeq: connFrame.highestSeq,
      }
      draft.set(contextKey, advanced)
      snapshot = advanced
    }

    preparedConnections.push({
      contextKey,
      deliveryIds: connFrame.deliveryIds,
      actions,
      highestSeq: connFrame.highestSeq,
    })

    const after = draft.get(contextKey)
    if (after && after !== before) {
      changedConnections.push({
        contextKey,
        deliveryIds: connFrame.deliveryIds,
      })
      if (!onlyCursorChanged(before, after)) {
        renderChangedConnections.push({ contextKey })
      }
    }
  }

  return {
    connections: preparedConnections,
    afterCommit,
    changedConnections,
    renderChangedConnections,
  }
}

/** Test-only publication counter for outer connection-map swaps. */
let publishedConnectionMapsCount = 0

/** @internal */
export function __getPublishedConnectionMapsCount(): number {
  return publishedConnectionMapsCount
}

/** @internal */
export function __resetPublishedConnectionMapsCount(): void {
  if (process.env.NODE_ENV !== "test") return
  publishedConnectionMapsCount = 0
}

const LEGACY_DISABLED_CAPABILITIES: DesktopDeliveryCapabilities = {
  mode: "legacy",
  flags: {
    desktop_acp_event_batching: false,
    incremental_live_transcript: false,
    deferred_streaming_rich_content: false,
  },
  perf_replay_available: false,
  failure_event: "acp://delivery-failed",
}

/** sessionStorage key — survives Provider remount / soft WebView reload. */
const DESKTOP_DELIVERY_FAILED_STORAGE_KEY = "codeg.desktopAcpDeliveryFailed"

/** Begin optimistic root activity for real ACP dispatches (prompt / root
 *  structured question). Excludes delegation children and unknown ids. */
function beginRootConversationActivity(
  connection: ConnectionState,
  explicitConversationId?: number | null
): { id: number; token: string } | null {
  if (connection.isDelegationChild) return null
  const id = explicitConversationId ?? connection.conversationId ?? null
  if (id == null) return null
  const token = useAppWorkspaceStore.getState().beginConversationActivity(id)
  return token ? { id, token } : null
}

function rollbackRootConversationActivity(
  activity: { id: number; token: string } | null
) {
  if (!activity) return
  useAppWorkspaceStore
    .getState()
    .rollbackConversationActivity(activity.id, activity.token)
}

function readDesktopDeliveryFailed(): boolean {
  if (typeof sessionStorage === "undefined") return false
  try {
    return sessionStorage.getItem(DESKTOP_DELIVERY_FAILED_STORAGE_KEY) === "1"
  } catch {
    return false
  }
}

function writeDesktopDeliveryFailed(failed: boolean): void {
  if (typeof sessionStorage === "undefined") return
  try {
    if (failed) {
      sessionStorage.setItem(DESKTOP_DELIVERY_FAILED_STORAGE_KEY, "1")
    } else {
      sessionStorage.removeItem(DESKTOP_DELIVERY_FAILED_STORAGE_KEY)
    }
  } catch {
    // Private mode / quota — in-memory ref still holds process-local state.
  }
}

/** Join assistant text blocks from a live message for integrity hashing. */
function extractLiveAssistantText(
  liveMessage: LiveMessage | null | undefined
): string {
  if (!liveMessage) return ""
  let text = ""
  for (const block of liveMessage.content) {
    if (block.type === "text") text += block.text
  }
  return text
}

/**
 * Project only events the canonical reducer would accept into the live
 * transcript. Walks applyEvents with the same prompting-window rules as
 * `applyStreamingAction` / turn_complete status flip — never whole-frame.
 */
export function selectTranscriptApplyEvents(
  events: readonly EventEnvelope[],
  initialStatus: ConnectionState["status"]
): EventEnvelope[] {
  let status = initialStatus
  const out: EventEnvelope[] = []
  for (const event of events) {
    if (event.type === "status_changed") {
      out.push(event)
      status = event.status
      continue
    }
    if (event.type === "turn_complete") {
      // Always project so the sink can mark completing; status leaves prompting
      // after this event (matches prepareMappedEnvelope STATUS_CHANGED).
      out.push(event)
      status = "connected"
      continue
    }
    // Content that the out-of-turn guard drops when not prompting.
    if (
      event.type === "content_delta" ||
      event.type === "thinking" ||
      event.type === "tool_call" ||
      event.type === "tool_call_update" ||
      event.type === "plan_update" ||
      event.type === "turn_attempt_rollback"
    ) {
      if (status === "prompting") out.push(event)
      continue
    }
    // Permissions / config / other wire events are not projected by the
    // live-transcript projector path; skip.
  }
  return out
}

// ── Ref-based store (replaces useReducer + Context) ──

interface InternalStore {
  connections: ConnectionsMap
  activeKey: string | null
  keyListeners: Map<string, Set<() => void>>
  activeKeyListeners: Set<() => void>
}

// ── Store API for consumers ──

export interface ConnectionStoreApi {
  getConnection(key: string): ConnectionState | undefined
  getActiveKey(): string | null
  subscribeKey(key: string, cb: () => void): () => void
  subscribeActiveKey(cb: () => void): () => void
}

const ConnectionStoreContext = createContext<ConnectionStoreApi | null>(null)

export function useConnectionStore(): ConnectionStoreApi {
  const ctx = useContext(ConnectionStoreContext)
  if (!ctx) {
    throw new Error(
      "useConnectionStore must be used within AcpConnectionsProvider"
    )
  }
  return ctx
}

// ── Actions context (unchanged interface) ──

/**
 * Sink that mirrors a connection's `liveMessage` into the conversation-runtime
 * store OUTSIDE React. Registered per `contextKey` by the conversation panel and
 * invoked synchronously from `dispatch` whenever that connection's `liveMessage`
 * reference changes (streaming deltas, tool updates, the prompt-start reset).
 * Moving this write out of a React effect lets the keep-alive panel stop
 * re-rendering on every streaming token — only the runtime-store subscriber (the
 * message list) re-renders. `isLive` is `status === "prompting"`, which the
 * runtime reducer uses to bypass its stale-reconnect-replay guard.
 */
export type LiveMessageSink = (
  liveMessage: LiveMessage,
  isLive: boolean,
  /** Optional ACP delivery IDs for streaming-perf live-publication marks. */
  deliveryIds?: readonly number[]
) => void

/**
 * Per-connection live sinks: canonical runtime mirror + optional UI transcript
 * projection. Frame commit calls `canonical` once, then `transcript.publish`
 * once with the accepted connection frame. Registration and snapshot hydrate
 * call `transcript.rebuild` with the current canonical message.
 */
export interface ConnectionLiveSinks {
  canonical(
    liveMessage: LiveMessage,
    isLive: boolean,
    deliveryIds?: readonly number[]
  ): void
  transcript?: LiveTranscriptFrameSink
}

export interface AcpActionsValue {
  connect(
    contextKey: string,
    agentType: AgentType,
    workingDir?: string,
    sessionId?: string,
    conversationId?: number,
    delegationRouteOverride?: DelegationRoutePolicy | null,
    ownerOperationId?: string | null,
    intent?: ConnectionIntent,
    retryObserverDiscovery?: boolean
  ): Promise<void>
  disconnect(contextKey: string, origin?: AcpDisconnectOrigin): Promise<boolean>
  disconnectIfIdle(contextKey: string): Promise<void>
  disconnectAll(): Promise<void>
  sendPrompt(
    contextKey: string,
    blocks: PromptInputBlock[],
    opts?: {
      folderId?: number | null
      conversationId?: number | null
      clientMessageId?: string | null
      promptContext?: AcpPromptContext
    }
  ): Promise<PromptEnqueueResult | null>
  setMode(contextKey: string, modeId: string): Promise<void>
  setConfigOption(
    contextKey: string,
    configId: string,
    valueId: string
  ): Promise<void>
  cancel(contextKey: string): Promise<void>
  cancelQueuedPrompt(contextKey: string, queueItemId: string): Promise<void>
  dismissFailedSharedPrompt(contextKey: string, queueItemId: string): void
  respondPermission(
    contextKey: string,
    requestId: string,
    optionId: string
  ): Promise<void>
  answerQuestion(
    contextKey: string,
    questionId: string,
    answer: QuestionAnswer
  ): Promise<void>
  answerPlanApproval(
    contextKey: string,
    approvalId: string,
    answer: PlanApprovalAnswer
  ): Promise<void>
  /** Pause or clear the session's active Codex goal (codex-acp #293). */
  goalControl(contextKey: string, action: "pause" | "clear"): Promise<void>
  setActiveKey(key: string | null): void
  touchActivity(contextKey: string): void
  registerOpenTabKeys(keys: Set<string>): void
  /**
   * Register live sinks for this contextKey (canonical runtime mirror + optional
   * transcript projection). Returns an unregister fn (idempotent — only removes
   * the entry if it still points at these sinks). Immediately replays the
   * current canonical message through `canonical` and `transcript.rebuild`.
   */
  registerLiveSinks(contextKey: string, sinks: ConnectionLiveSinks): () => void
  /**
   * Backward-compatible wrapper around `registerLiveSinks` with only a
   * canonical sink. Prefer `registerLiveSinks` for new call sites.
   */
  registerLiveMessageSink(contextKey: string, sink: LiveMessageSink): () => void
  /**
   * Clear `loadError` set by a `session/load` failure so the next auto-connect
   * attempt isn't gated by stale failure state. Wired to the Reload button in
   * the conversation detail panel.
   */
  clearAcpLoadError(contextKey: string): void
  /**
   * Register a delegation-spawned child connection so its acp://event
   * stream lands in the reducer (live message, tool calls, permission
   * requests). The child connection is already alive on the backend —
   * this is a frontend-only attach. Idempotent on connectionId.
   *
   * Routing:
   *   * Tauri: registers the connectionId in the global event router
   *     and drains any envelopes that arrived before registration.
   *   * Web/remote: opens a per-connection WS attach so the snapshot +
   *     replay + live events arrive on a dedicated stream.
   */
  attachDelegationChild(args: {
    connectionId: string
    parentConnectionId: string
    parentToolUseId: string
    agentType: AgentType
    /**
     * Backfill the in-flight turn from a session snapshot before routing
     * live events. Required when attaching MID-TURN, which the desktop
     * firehose cannot serve on its own: `acp://event` only carries FUTURE
     * events, so without a snapshot the viewer misses everything the turn
     * already produced and its status stays `connected` instead of
     * `prompting` (no streaming affordance, empty live message). Real
     * delegation children attach at `delegation_started` — before the
     * child's first event — and leave this off. No effect on web/remote:
     * the attach protocol always opens with a snapshot.
     */
    hydrate?: boolean
  }): void
  /**
   * Tear down a previously-attached delegation child. Releases the
   * synthetic ConnectionState and any per-connection WS attach. Does
   * NOT call acpDisconnect — the broker owns the child's backend
   * lifecycle. No-op when the child isn't attached.
   */
  detachDelegationChild(connectionId: string): void
  /**
   * Restart the session at `contextKey` so it picks up the latest agent/model
   * settings: disconnect the running process, then reconnect with the same
   * `sessionId` (the agent resumes the conversation — history is preserved).
   * The freshly spawned process reads current config, so its recomputed
   * fingerprint matches and `configStale` clears. Wired to the "restart to
   * apply" banner button. Returns `true` if it actually restarted, `false` if
   * it was a no-op (no connection, or a viewer / delegation child that doesn't
   * own the backend process) — callers gate their "applied" confirmation on it.
   */
  reapplyConfig(contextKey: string): Promise<boolean>
  /**
   * User-driven reconnect for the composer's connection-status popover, usable
   * in ANY state — unlike `reapplyConfig`, which only restarts a live owner.
   *
   *   * live owner  → disconnect + connect (a restart; a prompting turn dies
   *     with the agent CLI, which is why the popover warns before offering it)
   *   * viewer      → detach + re-run discovery, so it re-attaches (or spawns
   *     its own agent if the previous owner is gone). Never `acpDisconnect`s.
   *   * no entry    → connect with the params `connect()` last recorded for
   *     this key, which is what makes the button work from `disconnected` /
   *     `error`, where the store holds nothing at all.
   *
   * Returns `false` on a no-op: a delegation child (broker-owned) or a key we
   * have no params for (never connected in this session).
   */
  reconnect(contextKey: string): Promise<boolean>
  /**
   * Re-issue the attach subscription after a recoverable attach error
   * (`snapshot_budget_exceeded` / `oversized_frame`). Does not disconnect
   * the live agent.
   */
  retryAttach(contextKey: string): void
  /**
   * The params `reconnect(contextKey)` would use, or `null` when it would be a
   * no-op. Lets the status popover name the agent and enable its button while
   * NO connection exists. Non-reactive by design — the values only change when
   * `connect()` runs, which also notifies the store.
   */
  getReconnectInfo(contextKey: string): {
    agentType: AgentType
    workingDir: string | null
    sessionId: string | null
  } | null
  /**
   * Dismiss the "restart to apply" banner for the current drift WITHOUT
   * restarting (client-local; the underlying `configStale` is untouched). A
   * subsequent settings change re-shows it. Wired to the banner's X button.
   */
  dismissConfigStale(contextKey: string): void
  /**
   * Close AIR failure strips (client-local, like `dismissConfigStale`) — one
   * call per strip, carrying every record that strip stood for. The records
   * stay in the table as their revision watermarks, so this silences only what
   * was on screen: a failure that is still real re-arms via a higher revision.
   * Unlike the recovery actions this is NOT gated on owning the session — a
   * viewer dismissing a strip only edits its own projection.
   */
  dismissSessionFailures(contextKey: string, ids: string[]): void
}

function disconnectLeaseWithOrigin(
  lease: AcpDisconnectLease | null | undefined,
  origin: AcpDisconnectOrigin
): AcpDisconnectLease {
  return { ...(lease ?? {}), origin }
}

const AcpActionsContext = createContext<AcpActionsValue | null>(null)

export function useAcpActions(): AcpActionsValue {
  const ctx = useContext(AcpActionsContext)
  if (!ctx) {
    throw new Error("useAcpActions must be used within AcpConnectionsProvider")
  }
  return ctx
}

// ── Event subscriber context ──
//
// JS-level fanout of `acp://event` envelopes. The provider owns the single
// physical Tauri/WebSocket subscription; consumers register callbacks here
// instead of opening a second listener. See `useAcpEvent` below.

type EventSubscriberHandler = (envelope: EventEnvelope) => void
type EventSubscriberRef = { current: EventSubscriberHandler }

interface AcpEventSubscriberApi {
  subscribers: Set<EventSubscriberRef>
}

const AcpEventSubscriberContext = createContext<AcpEventSubscriberApi | null>(
  null
)

/**
 * Subscribe to `acp://event` envelopes via the provider's primary listener.
 *
 * The handler is invoked AFTER the context's reducer has dispatched its own
 * actions for that envelope (state is consistent at fire time). It also
 * inherits the provider's `seq` dedup — duplicates the primary listener
 * would skip are skipped here too. Unmapped events (no `contextKey`) do
 * NOT fan out.
 *
 * Stability: the latest `handler` is stored in a ref each render, so callers
 * may pass an inline function. There is no need for caller-side refs to keep
 * the subscription stable across renders.
 *
 * Errors thrown by `handler` are caught and logged so a single buggy
 * subscriber cannot break the central listener.
 */
export function useAcpEvent(handler: EventSubscriberHandler): void {
  const ctx = useContext(AcpEventSubscriberContext)
  if (!ctx) {
    throw new Error("useAcpEvent must be used within AcpConnectionsProvider")
  }
  const handlerRef = useRef(handler)
  // Re-sync each render so the latest closure is used at fire time.
  useEffect(() => {
    handlerRef.current = handler
  })
  // Register / unregister exactly once. Set-of-refs (not Set-of-functions)
  // so unmount cleanup matches the original entry even though `handler`
  // identity may change between renders.
  useEffect(() => {
    const ref = handlerRef
    ctx.subscribers.add(ref)
    return () => {
      ctx.subscribers.delete(ref)
    }
  }, [ctx])
}

// ── Helper: extract affected key from action ──

function getAffectedKey(action: Action): string | null {
  if (action.type === "REMOVE_ALL") return null // special: all keys
  if (action.type === "STREAM_BATCH") return null
  if (action.type === "APPLY_EVENT_FRAME") return null
  if (action.type === "BATCH_TOOL_CALL_UPDATES") return null
  if ("contextKey" in action) return action.contextKey
  return null
}

function normalizeErrorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

type AlertedError = Error & { alerted: true }

function createAlertedError(message: string): AlertedError {
  const error = new Error(message) as AlertedError
  error.alerted = true
  return error
}

function isAlertedError(error: unknown): error is AlertedError {
  if (!error || typeof error !== "object") return false
  return (error as { alerted?: unknown }).alerted === true
}

function isSharedSessionConfigConflict(error: unknown): boolean {
  return (
    extractAppCommandError(error)?.code === "shared_session_config_conflict"
  )
}

// ── Provider ──

export function AcpConnectionsProvider({ children }: { children: ReactNode }) {
  const t = useTranslations("Folder.chat.acpConnections")
  const tChat = useTranslations("Folder.chat")
  const { pushAlert } = useAlertContext()
  const { activeFolder: folder } = useActiveFolder()
  const folderNameRef = useRef(folder?.name)
  useEffect(() => {
    folderNameRef.current = folder?.name
  }, [folder?.name])
  const pushAlertRef = useRef(pushAlert)
  useEffect(() => {
    pushAlertRef.current = pushAlert
  }, [pushAlert])

  // Desktop notification click → select/focus the affected conversation.
  // Event name: `notification-navigate`
  // Payload: { kind: "conversation", conversationId: number }
  useEffect(() => {
    if (!getTransport().isDesktop()) return
    let dispose: (() => void) | null = null
    let cancelled = false
    void (async () => {
      try {
        const off = await getTransport().subscribe<{
          kind?: string
          conversationId?: number
          conversation_id?: number
        }>("notification-navigate", (payload) => {
          const conversationId = Number(
            payload?.conversationId ?? payload?.conversation_id
          )
          if (!Number.isFinite(conversationId) || conversationId <= 0) return
          void (async () => {
            try {
              const { useTabStore } = await import("@/stores/tab-store")
              const workspace = useAppWorkspaceStore.getState()
              const conv = workspace.conversations.find(
                (c) => c.id === conversationId
              )
              if (!conv) {
                // Best-effort: focus detached window only when we lack local
                // summary metadata for openTab.
                const { focusConversationWindow } = await import("@/lib/api")
                await focusConversationWindow(conversationId).catch(() => false)
                return
              }
              if (!workspace.folders.some((f) => f.id === conv.folder_id)) {
                await workspace
                  .addFolderToWorkspaceById(conv.folder_id)
                  .catch(() => {})
              }
              await useTabStore
                .getState()
                .openTab(
                  conv.folder_id,
                  conv.id,
                  conv.agent_type,
                  true,
                  conv.title ?? undefined
                )
            } catch (err) {
              console.warn(
                "[acp-context] notification-navigate open failed:",
                err
              )
            }
          })()
        })
        if (cancelled) off()
        else dispose = off
      } catch (err) {
        console.warn(
          "[acp-context] notification-navigate subscription failed:",
          err
        )
      }
    })()
    return () => {
      cancelled = true
      if (dispose) dispose()
    }
  }, [])

  // Notification sounds: browsers only open audio output from inside a user
  // gesture, so start watching for one now. The user's ordinary first click in
  // the workspace unlocks it, well before an agent event needs it — otherwise
  // the session's first cue is lost (the Settings preview cannot stand in for
  // it: that is a different window with its own audio context). No-op while
  // sounds are disabled.
  useEffect(() => primeNotificationSoundOutput(), [])
  // Ref-based store — mutations don't trigger React state updates
  const storeRef = useRef<InternalStore>({
    connections: new Map(),
    activeKey: null,
    keyListeners: new Map(),
    activeKeyListeners: new Set(),
  })

  // connectionId → contextKey reverse mapping. Used by the legacy global
  // `acp://event` listener path. Attach-protocol connections (web mode)
  // bypass this entirely — their events are routed by the per-subscription
  // handlers registered in `attachSubscriptionsRef`.
  const reverseMapRef = useRef(new Map<string, string>())

  // contextKey → diagnostic evidence already surfaced as an alert. The same
  // error reaches us twice: live on the wire, and again in `last_error` on
  // every re-attach snapshot. Without this a browser refresh would re-raise
  // the alert each time.
  const alertedErrorDetailsRef = useRef(new Map<string, string>())

  // contextKey → active EventStream subscription handle. Populated only for
  // connections established via the Subscribe-with-Snapshot attach
  // protocol (web + remote-desktop). Used to (a) detach on disconnect /
  // tab close, and (b) re-attach with the current cursor when a connection
  // is rekeyed (orphan rescue) so handlers reference the new contextKey.
  const attachSubscriptionsRef = useRef(
    new Map<string, EventStreamSubscription>()
  )
  type AttachRetryRecord = {
    connectionId: string
    sinceSeq: number | undefined
    reconnectMode: "resume" | "cold"
    shared: { generation: number; leaseId: string } | undefined
    autoRetryUsed: boolean
  }
  const attachRetryRef = useRef(new Map<string, AttachRetryRecord>())
  const retryAttachRef = useRef<(contextKey: string, isAuto: boolean) => void>(
    () => {}
  )

  // Open tab keys — updated by child TabProvider via registerOpenTabKeys
  const openTabKeysRef = useRef(new Set<string>())

  // Guard against concurrent connect() calls
  const connectingKeysRef = useRef(new Set<string>())
  const pendingConnectRequestsRef = useRef(new Map<string, ConnectRequest>())
  // Last params `connect()` was called with, per contextKey — kept AFTER the
  // connection is gone (teardown removes the store entry entirely, so a
  // `disconnected` / `error` composer has nothing left to reconnect from).
  // Recorded even for attempts that fail, which is exactly the `error` case the
  // status popover's Reconnect button has to serve.
  //
  // Backend-RESOLVED identity is folded back in as it arrives (see
  // `rememberResolvedIdentity`): a new conversation connects with no sessionId
  // at all, so the request as issued would reconnect into a FRESH session and
  // silently abandon the conversation's history.
  const lastConnectParamsRef = useRef(new Map<string, ConnectRequest>())
  // Keys whose disconnect was requested while connect was still in flight
  const abandonedKeysRef = useRef(new Set<string>())
  // Resolvers waiting for an in-flight connect() on a key to settle. Only a
  // user-driven `reconnect` uses this: connect() parks a same-parameter request
  // in `pendingConnectRequestsRef` and its `finally` then DROPS it as a
  // duplicate, so a reconnect landing mid-connect would vanish silently.
  const connectSettledWaitersRef = useRef(new Map<string, Array<() => void>>())
  const connectRef = useRef<AcpActionsValue["connect"] | null>(null)

  // Cancelable observer/handoff discovery delays (contextKey → cancel).
  const observerDelayCancelsRef = useRef(new Map<string, () => void>())

  // In-flight connect request per contextKey (for identical-vs-superseding races).
  const inflightConnectRequestsRef = useRef(new Map<string, ConnectRequest>())

  // One-shot handoff re-entry: after re-attach during own_or_observe, watch the
  // broker canonical id for removal and re-invoke connect(own_or_observe).
  type HandoffWatcher = {
    brokerConnectionId: string
    request: ConnectRequest
  }
  const handoffWatchersRef = useRef(new Map<string, HandoffWatcher>())
  // Queued re-entry microtasks must be cancellable: close/relock between
  // queueMicrotask and run must not start owner ACP (Task 5 r3 Important 1).
  type HandoffReentryToken = { cancelled: boolean }
  const handoffReentryTokensRef = useRef(new Map<string, HandoffReentryToken>())

  const cancelObserverDelay = (contextKey: string) => {
    const cancel = observerDelayCancelsRef.current.get(contextKey)
    if (!cancel) return
    cancel()
  }

  const cancelAllObserverDelays = () => {
    for (const key of [...observerDelayCancelsRef.current.keys()]) {
      cancelObserverDelay(key)
    }
  }

  const cancelHandoffReentry = (contextKey: string) => {
    const token = handoffReentryTokensRef.current.get(contextKey)
    if (!token) return
    token.cancelled = true
    handoffReentryTokensRef.current.delete(contextKey)
  }

  const cancelAllHandoffReentries = () => {
    for (const token of handoffReentryTokensRef.current.values()) {
      token.cancelled = true
    }
    handoffReentryTokensRef.current.clear()
  }

  const clearAllHandoffWatchers = () => {
    handoffWatchersRef.current.clear()
    cancelAllHandoffReentries()
  }

  const waitObserverDelay = (key: string, delayMs: number) =>
    new Promise<boolean>((resolve) => {
      if (delayMs === 0) return resolve(true)
      const timer = setTimeout(() => {
        observerDelayCancelsRef.current.delete(key)
        resolve(true)
      }, delayMs)
      observerDelayCancelsRef.current.set(key, () => {
        clearTimeout(timer)
        observerDelayCancelsRef.current.delete(key)
        resolve(false)
      })
    })

  const clearHandoffWatcher = (contextKey: string) => {
    handoffWatchersRef.current.delete(contextKey)
    // Fresh connect / disconnect / intent change also drops any already-queued
    // re-entry microtask for this tab.
    cancelHandoffReentry(contextKey)
  }

  /**
   * Queue a one-shot own_or_observe re-entry after broker removal. Cancelable
   * via clearHandoffWatcher / disconnect / disconnectAll / intent supersession.
   * Microtask re-checks abandonedKeys and still-wanted ownership before connect.
   */
  const queueOwnOrObserveReentry = (
    contextKey: string,
    request: ConnectRequest
  ) => {
    cancelHandoffReentry(contextKey)
    const token: HandoffReentryToken = { cancelled: false }
    handoffReentryTokensRef.current.set(contextKey, token)
    queueMicrotask(() => {
      if (handoffReentryTokensRef.current.get(contextKey) === token) {
        handoffReentryTokensRef.current.delete(contextKey)
      }
      if (token.cancelled) return
      if (abandonedKeysRef.current.has(contextKey)) return
      // Still want ownership: a queued relock (observe_existing) or other
      // non-own intent supersedes this re-entry.
      const pending = pendingConnectRequestsRef.current.get(contextKey)
      if (pending != null && pending.intent !== "own_or_observe") return
      const inflight = inflightConnectRequestsRef.current.get(contextKey)
      if (inflight != null && inflight.intent !== "own_or_observe") return
      if (request.intent !== "own_or_observe") return
      connectRef
        .current?.(
          contextKey,
          request.agentType,
          request.workingDir,
          request.sessionId,
          request.conversationId,
          request.delegationRouteOverride,
          request.ownerOperationId,
          "own_or_observe",
          request.retryObserverDiscovery
        )
        .catch(() => {})
    })
  }

  const fireHandoffWatchersForRemoved = (connectionId: string) => {
    const fired: Array<{ contextKey: string; request: ConnectRequest }> = []
    for (const [contextKey, watcher] of handoffWatchersRef.current) {
      if (watcher.brokerConnectionId !== connectionId) continue
      handoffWatchersRef.current.delete(contextKey)
      fired.push({ contextKey, request: watcher.request })
    }
    for (const { contextKey, request } of fired) {
      queueOwnOrObserveReentry(contextKey, request)
    }
  }

  /**
   * Drop a store entry for a connection that is no longer usable locally.
   * Does **not** fire handoff re-entry — use when death is unconfirmed
   * (snapshot throw / discovery error) so we never claim ownership via
   * acpConnect while the broker may still be live (Task 5 r5).
   */
  const removeDeadCanonicalOnly = (contextKey: string) => {
    dispatch({ type: "CONNECTION_REMOVED", contextKey })
  }

  /**
   * Confirmed-dead broker removal: drop the store entry and fire handoff
   * re-entry so own_or_observe can retry. Only for verified-null snapshot
   * or connection_gone — not snapshot throw paths (Task 5 r5).
   * Intentional disconnect / idle teardown must not use this (they cancel
   * watchers instead).
   */
  const removeDeadCanonicalAndFireHandoff = (
    contextKey: string,
    connectionId: string
  ) => {
    removeDeadCanonicalOnly(contextKey)
    fireHandoffWatchersForRemoved(connectionId)
  }

  const scheduleOwnOrObserveOnBrokerRemoved = (
    contextKey: string,
    brokerConnectionId: string,
    request: ConnectRequest
  ) => {
    // clearHandoffWatcher also cancels any prior queued re-entry for this key.
    clearHandoffWatcher(contextKey)
    // Immediate post-registration check: broker already vanished.
    if (!storeRef.current.connections.get(brokerConnectionId)) {
      queueOwnOrObserveReentry(contextKey, request)
      return
    }
    handoffWatchersRef.current.set(contextKey, {
      brokerConnectionId,
      request,
    })
  }

  /**
   * Fold backend-resolved identity into the remembered connect params.
   *
   * `connect()` records what the CALLER asked for, and for a new conversation
   * that request carries no `sessionId` / `conversationId` — the backend mints
   * them later and they only ever land on the store entry. But the entry is
   * exactly what disappears when a connection is removed WITHOUT a user
   * teardown (backend GC via `connection_gone`, the idle sweep, the unmount
   * cleanup), which is the main way a composer ends up needing Reconnect. With
   * only the original request left, that button would start a fresh ACP session
   * instead of resuming the conversation.
   *
   * No-op when nothing was remembered for the key: `agentType` alone makes a
   * request reconnectable, and it can only come from `connect()`.
   */
  const rememberResolvedIdentity = useCallback(
    (
      contextKey: string,
      patch: { sessionId?: string; conversationId?: number }
    ) => {
      const remembered = lastConnectParamsRef.current.get(contextKey)
      if (!remembered) return
      lastConnectParamsRef.current.set(contextKey, { ...remembered, ...patch })
    },
    []
  )

  /**
   * Snapshot the live entry's resolved identity into the remembered params
   * immediately BEFORE that entry goes away.
   *
   * Identity reaches the entry by several routes — the `session_started` event,
   * a snapshot hydrate on a cold attach (where the event was already consumed
   * before this client attached, so it is never replayed), a replayed event —
   * but it leaves by exactly one: the entry being removed. Capturing at the
   * single exit covers every route in, including ones added later.
   */
  const captureIdentityBeforeRemoval = useCallback(
    (contextKey: string) => {
      const sessionId = storeRef.current.connections.get(contextKey)?.sessionId
      if (!sessionId) return
      rememberResolvedIdentity(contextKey, { sessionId })
    },
    [rememberResolvedIdentity]
  )

  type ConnectBlockState =
    | { kind: "none"; reason: "" }
    | {
        kind: "missing_config" | "disabled" | "unavailable" | "sdk_missing"
        reason: string
      }

  const buildOpenAgentsSettingsAction = useCallback(
    (agentType?: AgentType): AlertAction => {
      const payload =
        typeof agentType === "string"
          ? JSON.stringify({
              section: "agents",
              agentType,
            })
          : "agents"
      return {
        label: t("actions.openAgentsSettings"),
        kind: "open_agents_settings",
        payload,
      }
    },
    [t]
  )

  const resolveConnectBlockState = useCallback(
    (agent: AcpAgentStatus | null): ConnectBlockState => {
      if (!agent) {
        return { kind: "missing_config", reason: t("blocked.missingConfig") }
      }

      const agentLabel = getAgentLabel(agent.agent_type)
      if (!agent.enabled) {
        return {
          kind: "disabled",
          reason: t("blocked.disabled", { agent: agentLabel }),
        }
      }

      if (!agent.available) {
        return {
          kind: "unavailable",
          reason: t("blocked.unavailable", { agent: agentLabel }),
        }
      }

      if (agent.installed_version) {
        return { kind: "none", reason: "" }
      }

      return {
        kind: "sdk_missing",
        // Claude Code / Codex install a separate ACP adapter package, not the
        // vendor CLI — saying "{agent} is not installed" to someone who has
        // `claude` on their PATH reads as a bug in codeg. Name what's actually
        // missing instead.
        reason: agent.is_acp_adapter
          ? t("blocked.adapterMissing", { agent: agentLabel })
          : t("blocked.sdkMissing", { agent: agentLabel }),
      }
    },
    [t]
  )

  // Per-contextKey live sinks (canonical runtime mirror + optional transcript).
  // Fired synchronously from frame commit / dispatch when liveMessage changes.
  // A ref → no re-renders.
  const liveSinksRef = useRef(new Map<string, ConnectionLiveSinks>())

  // Activity tracking (no re-renders)
  const lastActivityRef = useRef(new Map<string, number>())
  const pendingUnmappedEventsRef = useRef(new Map<string, EventEnvelope[]>())
  /**
   * Desktop/attach listener lifecycle.
   * - idle/starting: connect() may wait
   * - ready: only phase that resolves waiters
   * - failed: rejects waiters (capability/subscribe/runtime)
   * - cancelled: effect cleanup; rejects waiters so connect never hangs
   */
  type ListenerPhase = "idle" | "starting" | "ready" | "failed" | "cancelled"
  const listenerPhaseRef = useRef<ListenerPhase>("idle")
  const listenerInitErrorRef = useRef<Error | null>(null)
  const listenerReadyWaitersRef = useRef<
    Array<{ resolve: () => void; reject: (err: Error) => void }>
  >([])
  const eventIngestorRef = useRef<EventIngestor | null>(null)
  // Process + session durable: survives Provider remount / WebView soft reload
  // until the app process is fully restarted (batcher is not rebuilt).
  const desktopDeliveryFailedRef = useRef(readDesktopDeliveryFailed())
  // Set of refs (not callbacks) so unmount cleanup matches the original
  // registration even when caller-side handler identity changes per render.
  // Populated by the `useAcpEvent` hook; read by the primary event path
  // and the buffered-events replay loop.
  const eventSubscribersRef = useRef<Set<EventSubscriberRef>>(new Set())

  // Observer tabs are names only. Canonical ACP state, cursor, subscription and
  // reverse routing stay under the backend connection id.
  const observerAliasesRef = useRef(new Map<string, string>())

  // ── Notify helpers ──

  const notifyKeyListeners = useCallback((key: string) => {
    const listeners = storeRef.current.keyListeners.get(key)
    if (listeners) {
      for (const cb of listeners) cb()
    }
  }, [])

  const aliasKeysFor = useCallback((canonical: string): string[] => {
    const aliases: string[] = []
    for (const [alias, target] of observerAliasesRef.current) {
      if (target === canonical) aliases.push(alias)
    }
    return aliases
  }, [])

  // When a canonical connection is terminal-removed (connection_gone, idle
  // disconnect, etc.), drop every tab alias that still points at it. Leaving
  // those aliases would make getConnection(tabId) resolve to the dead key
  // and block owner reconnect under the tab id.
  const clearAliasesPointingTo = useCallback(
    (canonical: string) => {
      for (const [alias, target] of [...observerAliasesRef.current]) {
        if (target !== canonical) continue
        observerAliasesRef.current.delete(alias)
        liveSinksRef.current.delete(alias)
        notifyKeyListeners(alias)
      }
    },
    [notifyKeyListeners]
  )

  const canonicalKey = useCallback((key: string): string => {
    return observerAliasesRef.current.get(key) ?? key
  }, [])

  const notifyConnectionKeys = useCallback(
    (canonical: string) => {
      notifyKeyListeners(canonical)
      for (const alias of aliasKeysFor(canonical)) notifyKeyListeners(alias)
    },
    [aliasKeysFor, notifyKeyListeners]
  )

  const notifyAllKeyListeners = useCallback(() => {
    for (const [, listeners] of storeRef.current.keyListeners) {
      for (const cb of listeners) cb()
    }
  }, [])

  const notifyActiveKeyListeners = useCallback(() => {
    for (const cb of storeRef.current.activeKeyListeners) cb()
  }, [])

  const notifyRawSubscribers = useCallback((envelope: EventEnvelope) => {
    for (const ref of eventSubscribersRef.current) {
      try {
        ref.current(envelope)
      } catch (err) {
        console.error("[acp-context] subscriber threw:", err)
      }
    }
  }, [])

  const getPrepareEnv = useCallback((): PrepareEnv => {
    return {
      t: (key, params) =>
        // next-intl keys are typed; prepare path uses dynamic keys.
        (t as (k: string, p?: Record<string, string | number>) => string)(
          key,
          params
        ),
      tChat: (key, params) =>
        (tChat as (k: string, p?: Record<string, string | number>) => string)(
          key,
          params
        ),
      folderName: folderNameRef.current,
      pushAlert: (kind, title, message, actions) => {
        pushAlertRef.current(kind, title, message, actions)
      },
    }
  }, [t, tChat])

  const mirrorLiveMessageOnce = useCallback(
    (
      key: string,
      previous: ConnectionsMap,
      next: ConnectionsMap,
      deliveryIds: readonly number[],
      connectionFrame?: AcceptedConnectionFrame
    ) => {
      const sinks = liveSinksRef.current.get(key)
      if (!sinks) return
      // Sink is registered under the caller's key (tab alias or canonical);
      // state always lives under the canonical connection id.
      const stateKey = observerAliasesRef.current.get(key) ?? key
      const nextConn = next.get(stateKey)
      if (!nextConn || nextConn.liveMessage == null) return
      const liveChanged =
        nextConn.liveMessage !== previous.get(stateKey)?.liveMessage
      if (!liveChanged && !connectionFrame) return

      if (liveChanged) {
        streamingPerfRecorder.setCurrentDeliveryIds(deliveryIds)
        try {
          sinks.canonical(
            nextConn.liveMessage,
            nextConn.status === "prompting",
            deliveryIds
          )
          streamingPerfRecorder.flushQueuedLivePublication()
        } finally {
          streamingPerfRecorder.setCurrentDeliveryIds(null)
        }
      }

      if (!sinks.transcript) return
      if (connectionFrame) {
        // Per-event filter matching canonical out-of-turn transitions — not
        // whole-frame before/after status (mixed turn_complete+delta frames).
        const previousStatus = previous.get(stateKey)?.status ?? nextConn.status
        const projectedEvents = selectTranscriptApplyEvents(
          connectionFrame.applyEvents,
          previousStatus
        )
        if (projectedEvents.length > 0 || liveChanged) {
          sinks.transcript.publish(
            projectedEvents.length === connectionFrame.applyEvents.length
              ? connectionFrame
              : { ...connectionFrame, applyEvents: projectedEvents },
            nextConn.liveMessage
          )
        }
      } else if (liveChanged) {
        // Snapshot hydrate / non-frame dispatches: full rebuild at cursor.
        sinks.transcript.rebuild(nextConn.liveMessage, nextConn.lastAppliedSeq)
      }
    },
    []
  )

  const mirrorLiveMessageForCanonical = useCallback(
    (
      canonical: string,
      previous: ConnectionsMap,
      next: ConnectionsMap,
      deliveryIds: readonly number[],
      connectionFrame?: AcceptedConnectionFrame
    ) => {
      // At most one sink invocation per registered key: canonical first,
      // then each open tab alias (two open aliases mirror into two sessions).
      mirrorLiveMessageOnce(
        canonical,
        previous,
        next,
        deliveryIds,
        connectionFrame
      )
      for (const alias of aliasKeysFor(canonical)) {
        mirrorLiveMessageOnce(
          alias,
          previous,
          next,
          deliveryIds,
          connectionFrame
        )
      }
    },
    [aliasKeysFor, mirrorLiveMessageOnce]
  )

  const commitEventFrame = useCallback(
    (frame: AcceptedEventFrame): void => {
      const prepared = prepareEventFrame(
        frame,
        storeRef.current.connections,
        reverseMapRef.current,
        getPrepareEnv()
      )
      const previous = storeRef.current.connections
      const reducerEffects: ReducerEffect[] = []
      const next = connectionsReducer(
        previous,
        {
          type: "APPLY_EVENT_FRAME",
          frames: prepared.connections,
        },
        reducerEffects
      )
      if (next !== previous) {
        storeRef.current.connections = next
        if (process.env.NODE_ENV === "test") {
          publishedConnectionMapsCount += 1
        }
        for (const effect of reducerEffects) effect()
      }
      streamingPerfRecorder.markConnectionFrameCommitted(
        frame.deliveryIds,
        prepared.changedConnections.length,
        next !== previous
      )
      if (streamingPerfRecorder.isActive()) {
        // Only the fixture target connection — never all connections.
        // Count seq-accepted rawEvents (pre-compaction); text hash is taken
        // from final canonical liveMessage after quiet, not raw deltas here.
        const targetId = streamingPerfRecorder.getTargetConnectionId()
        let accepted = 0
        let firstSeq = 0
        let lastSeq = 0
        for (const connFrame of frame.connections) {
          if (targetId && connFrame.connectionId !== targetId) continue
          accepted += connFrame.rawEvents.length
          for (const event of connFrame.rawEvents) {
            if (firstSeq === 0 || event.seq < firstSeq) firstSeq = event.seq
            if (event.seq > lastSeq) lastSeq = event.seq
          }
        }
        if (accepted > 0) {
          streamingPerfRecorder.markFrontendEventsAccepted(accepted)
          if (lastSeq > 0) {
            streamingPerfRecorder.noteFrontendSeqRange(firstSeq, lastSeq)
          }
        }
      }

      // Map contextKey → accepted connection frame for transcript.publish.
      const framesByKey = new Map<string, AcceptedConnectionFrame>()
      for (const connFrame of frame.connections) {
        const key =
          reverseMapRef.current.get(connFrame.connectionId) ??
          connFrame.contextKey
        framesByKey.set(key, connFrame)
      }

      for (const connection of prepared.changedConnections) {
        mirrorLiveMessageForCanonical(
          connection.contextKey,
          previous,
          next,
          connection.deliveryIds,
          framesByKey.get(connection.contextKey)
        )
      }
      for (const effect of prepared.afterCommit) effect()
      for (const connection of prepared.renderChangedConnections) {
        notifyConnectionKeys(connection.contextKey)
      }
      for (const event of frame.rawEventsInDeliveryOrder) {
        notifyRawSubscribers(event)
      }
      // Desktop path has no EventStream onDetached: broker exit arrives as
      // status_changed(disconnected). Fire the same one-shot handoff re-entry
      // used by attach-stream connection_gone (idempotent if both fire).
      for (const event of frame.rawEventsInDeliveryOrder) {
        if (
          event.type === "status_changed" &&
          event.status === "disconnected"
        ) {
          fireHandoffWatchersForRemoved(event.connection_id)
        }
      }
    },
    [
      getPrepareEnv,
      mirrorLiveMessageForCanonical,
      notifyConnectionKeys,
      notifyRawSubscribers,
    ]
  )

  // ── Dispatch (replaces useReducer dispatch) ──

  const dispatch = useCallback(
    (action: Action) => {
      if (action.type === "APPLY_EVENT_FRAME") {
        // Frame commits go through commitEventFrame (publish order).
        return
      }
      const prev = storeRef.current.connections
      const reducerEffects: ReducerEffect[] = []
      const next = connectionsReducer(prev, action, reducerEffects)
      if (next === prev) return // no change

      storeRef.current.connections = next
      if (process.env.NODE_ENV === "test") {
        publishedConnectionMapsCount += 1
      }
      for (const effect of reducerEffects) effect()

      const mirrorLiveMessage = (key: string) => {
        mirrorLiveMessageForCanonical(key, prev, next, [])
      }

      if (action.type === "REMOVE_ALL") {
        notifyAllKeyListeners()
      } else if (action.type === "STREAM_BATCH") {
        const keys = new Set(action.actions.map((item) => item.contextKey))
        for (const key of keys) {
          mirrorLiveMessage(key)
          notifyConnectionKeys(key)
        }
      } else if (action.type === "BATCH_TOOL_CALL_UPDATES") {
        const keys = new Set(action.actions.map((item) => item.contextKey))
        for (const key of keys) {
          mirrorLiveMessage(key)
          notifyConnectionKeys(key)
        }
      } else if (action.type === "REKEY_CONNECTION") {
        // Move sink registration with the context key; projection IDs unchanged.
        const sinks = liveSinksRef.current.get(action.fromKey)
        if (sinks) {
          liveSinksRef.current.delete(action.fromKey)
          liveSinksRef.current.set(action.toKey, sinks)
        }
        mirrorLiveMessage(action.toKey)
        notifyKeyListeners(action.fromKey)
        notifyConnectionKeys(action.toKey)
      } else {
        const key = getAffectedKey(action)
        if (key) {
          mirrorLiveMessage(key)
          notifyConnectionKeys(key)
        }
      }
    },
    [
      mirrorLiveMessageForCanonical,
      notifyConnectionKeys,
      notifyKeyListeners,
      notifyAllKeyListeners,
    ]
  )

  // ── setActiveKey ──

  const setActiveKey = useCallback(
    (key: string | null) => {
      if (storeRef.current.activeKey === key) return
      storeRef.current.activeKey = key
      notifyActiveKeyListeners()
    },
    [notifyActiveKeyListeners]
  )

  // ── Store API (stable object — never recreated) ──

  const storeApi = useMemo<ConnectionStoreApi>(() => {
    return {
      getConnection(key: string) {
        const canonical = observerAliasesRef.current.get(key) ?? key
        return storeRef.current.connections.get(canonical)
      },
      getActiveKey() {
        return storeRef.current.activeKey
      },
      subscribeKey(key: string, cb: () => void) {
        const { keyListeners } = storeRef.current
        let set = keyListeners.get(key)
        if (!set) {
          set = new Set()
          keyListeners.set(key, set)
        }
        set.add(cb)
        return () => {
          set!.delete(cb)
          if (set!.size === 0) keyListeners.delete(key)
        }
      },
      subscribeActiveKey(cb: () => void) {
        storeRef.current.activeKeyListeners.add(cb)
        return () => {
          storeRef.current.activeKeyListeners.delete(cb)
        }
      },
    }
  }, [])

  const touchActivity = useCallback((contextKey: string) => {
    // Alias focus is a UI event, not an ACP keepalive/health event.
    if (observerAliasesRef.current.has(contextKey)) return
    lastActivityRef.current.set(contextKey, Date.now())
  }, [])

  const registerOpenTabKeys = useCallback((keys: Set<string>) => {
    openTabKeysRef.current = keys
  }, [])

  const registerLiveSinks = useCallback(
    (contextKey: string, sinks: ConnectionLiveSinks) => {
      liveSinksRef.current.set(contextKey, sinks)
      const stateKey = observerAliasesRef.current.get(contextKey) ?? contextKey
      const conn = storeRef.current.connections.get(stateKey)
      if (conn?.liveMessage != null) {
        sinks.canonical(conn.liveMessage, conn.status === "prompting")
        sinks.transcript?.rebuild(conn.liveMessage, conn.lastAppliedSeq)
      }
      return () => {
        if (liveSinksRef.current.get(contextKey) === sinks) {
          liveSinksRef.current.delete(contextKey)
        }
      }
    },
    []
  )

  const registerLiveMessageSink = useCallback(
    (contextKey: string, sink: LiveMessageSink) => {
      return registerLiveSinks(contextKey, { canonical: sink })
    },
    [registerLiveSinks]
  )

  const clearAcpLoadError = useCallback(
    (contextKey: string) => {
      dispatch({
        type: "CLEAR_ACP_LOAD_ERROR",
        contextKey: canonicalKey(contextKey),
      })
    },
    [canonicalKey, dispatch]
  )

  const settleListenerWaiters = useCallback(
    (outcome: "ready" | "failed" | "cancelled", error?: Error) => {
      if (outcome === "ready") {
        listenerPhaseRef.current = "ready"
        listenerInitErrorRef.current = null
      } else if (outcome === "failed") {
        listenerPhaseRef.current = "failed"
        listenerInitErrorRef.current =
          error ?? new Error("Desktop ACP event listener failed")
      } else {
        listenerPhaseRef.current = "cancelled"
        listenerInitErrorRef.current =
          error ?? new Error("Desktop ACP event listener was cancelled")
      }
      const waiters = listenerReadyWaitersRef.current
      listenerReadyWaitersRef.current = []
      if (outcome === "ready") {
        for (const w of waiters) w.resolve()
      } else {
        const err =
          listenerInitErrorRef.current ??
          new Error("Desktop ACP event listener unavailable")
        for (const w of waiters) w.reject(err)
      }
    },
    []
  )

  const waitForListenerReady = useCallback(async () => {
    const phase = listenerPhaseRef.current
    if (phase === "ready") return
    if (phase === "failed") {
      throw (
        listenerInitErrorRef.current ??
        new Error("Desktop ACP event listener failed")
      )
    }
    if (phase === "cancelled") {
      throw (
        listenerInitErrorRef.current ??
        new Error("Desktop ACP event listener was cancelled")
      )
    }
    // idle | starting — wait for settle (ready resolves; failed/cancelled reject)
    await new Promise<void>((resolve, reject) => {
      listenerReadyWaitersRef.current.push({ resolve, reject })
    })
  }, [])

  const bufferUnmappedEvent = useCallback((event: EventEnvelope) => {
    const connectionId = event.connection_id
    const buffered = pendingUnmappedEventsRef.current.get(connectionId) ?? []
    if (buffered.length >= MAX_BUFFERED_UNMAPPED_EVENTS_PER_CONNECTION) {
      buffered.shift()
    }
    buffered.push(event)
    pendingUnmappedEventsRef.current.set(connectionId, buffered)

    if (
      pendingUnmappedEventsRef.current.size > MAX_BUFFERED_UNMAPPED_CONNECTIONS
    ) {
      const oldest = pendingUnmappedEventsRef.current.keys().next().value
      if (oldest) {
        pendingUnmappedEventsRef.current.delete(oldest)
      }
    }
  }, [])

  const consumeBufferedEvents = useCallback(
    (connectionId: string): EventEnvelope[] => {
      const buffered = pendingUnmappedEventsRef.current.get(connectionId)
      if (!buffered || buffered.length === 0) return []
      pendingUnmappedEventsRef.current.delete(connectionId)
      return buffered
    },
    []
  )

  const seedDelegationsFromSnapshotRef = useRef<
    (
      connectionId: string,
      activeDelegations: ActiveDelegationState[],
      eventSeq: number
    ) => void
  >(() => {})

  const handleSequenceGap = useCallback(
    (gap: SequenceGap) => {
      if (
        streamingPerfRecorder.isActive() &&
        streamingPerfRecorder.matchesTargetConnection(gap.connectionId)
      ) {
        streamingPerfRecorder.markFrontendSequenceGap()
      }
      const ingestor = eventIngestorRef.current
      ingestor?.pauseConnection(gap.connectionId)
      void (async () => {
        // After any await, orphan rescue may have rekeyed the store entry.
        // Always re-resolve the live context key before hydrate/remove so we
        // never clear the stale pre-await key while leaving the new canonical
        // entry stranded.
        const resolveGapContextKey = () =>
          resolveContextKeyForConnection(
            gap.connectionId,
            gap.contextKey,
            reverseMapRef.current,
            storeRef.current.connections
          )
        // Only mutate/remove when the resolved entry is still the original
        // connection A. If the tab now holds replacement B, leave it alone.
        const entryStillOriginal = (contextKey: string) => {
          const entry = storeRef.current.connections.get(contextKey)
          return entry?.connectionId === gap.connectionId
        }
        const dropOriginalBookkeepingOnly = () => {
          reverseMapRef.current.delete(gap.connectionId)
          pendingUnmappedEventsRef.current.delete(gap.connectionId)
          clearAliasesPointingTo(gap.connectionId)
          ingestor?.resumeConnection(gap.connectionId, Number.MAX_SAFE_INTEGER)
        }
        try {
          const snapshot = await acpGetSessionSnapshot(gap.connectionId)
          const contextKey = resolveGapContextKey()
          if (!entryStillOriginal(contextKey)) {
            dropOriginalBookkeepingOnly()
            return
          }
          if (!snapshot) {
            reverseMapRef.current.delete(gap.connectionId)
            // Drop tab aliases that still target this dead canonical so a
            // later owner/viewer reconnect under the tab id can resolve.
            clearAliasesPointingTo(contextKey)
            if (gap.connectionId !== contextKey) {
              clearAliasesPointingTo(gap.connectionId)
            }
            removeDeadCanonicalAndFireHandoff(contextKey, gap.connectionId)
            ingestor?.resumeConnection(
              gap.connectionId,
              Number.MAX_SAFE_INTEGER
            )
            return
          }
          const patch = denormalizeSnapshot(snapshot)
          dispatch({ type: "HYDRATE_FROM_SNAPSHOT", contextKey, patch })
          seedDelegationsFromSnapshotRef.current(
            patch.connectionId,
            patch.activeDelegations,
            patch.eventSeq
          )
          const applied = storeRef.current.connections.get(contextKey)
          const resumeSeq =
            applied?.connectionId === gap.connectionId
              ? applied.lastAppliedSeq
              : patch.eventSeq
          ingestor?.resumeConnection(gap.connectionId, resumeSeq)
        } catch (err) {
          console.warn(
            "[acp-context] sequence gap recovery failed",
            gap.connectionId,
            err
          )
          const contextKey = resolveGapContextKey()
          if (!entryStillOriginal(contextKey)) {
            dropOriginalBookkeepingOnly()
            return
          }
          reverseMapRef.current.delete(gap.connectionId)
          clearAliasesPointingTo(contextKey)
          if (gap.connectionId !== contextKey) {
            clearAliasesPointingTo(gap.connectionId)
          }
          // Throw ≠ confirmed dead: clean up local entry only. Do not fire
          // handoff re-entry (would acpConnect-spawn while broker may live).
          removeDeadCanonicalOnly(contextKey)
          ingestor?.resumeConnection(gap.connectionId, Number.MAX_SAFE_INTEGER)
        }
      })()
    },
    [clearAliasesPointingTo, dispatch]
  )

  const handleDesktopDeliveryFailure = useCallback(
    (failure: DesktopDeliveryFailure) => {
      // Terminal for this process: batcher is not rebuilt. Persist so Provider
      // remount / soft WebView reload cannot pretend delivery is live again.
      desktopDeliveryFailedRef.current = true
      writeDesktopDeliveryFailed(true)
      const failErr = new Error(
        "Desktop ACP event delivery failed; restart the application"
      )
      settleListenerWaiters("failed", failErr)
      const ingestor = eventIngestorRef.current
      // Commit any contiguous pending work before tearing the ingestor down so
      // a successfully emitted but unflushed batch is not silently discarded.
      try {
        ingestor?.flushNow()
      } catch (err) {
        console.warn(
          "[acp-context] flush before delivery-failure dispose failed",
          err
        )
      }
      ingestor?.dispose()
      eventIngestorRef.current = null

      const failMessage = t("desktopDeliveryFailedRestart")
      for (const range of failure.affected) {
        const fallbackContextKey = reverseMapRef.current.get(
          range.connection_id
        )
        if (!fallbackContextKey) continue
        void (async () => {
          // Orphan rescue may rekey during the snapshot await. Re-resolve the
          // current store key before every hydrate/error/removal branch.
          const resolveFailureContextKey = () =>
            resolveContextKeyForConnection(
              range.connection_id,
              fallbackContextKey,
              reverseMapRef.current,
              storeRef.current.connections
            )
          // Only mutate/remove when the resolved entry is still the original
          // connection A. If the tab now holds replacement B, leave it alone.
          const entryStillOriginal = (contextKey: string) => {
            const entry = storeRef.current.connections.get(contextKey)
            return entry?.connectionId === range.connection_id
          }
          const dropOriginalBookkeepingOnly = () => {
            reverseMapRef.current.delete(range.connection_id)
            pendingUnmappedEventsRef.current.delete(range.connection_id)
            clearAliasesPointingTo(range.connection_id)
          }
          try {
            const snapshot = await acpGetSessionSnapshot(range.connection_id)
            const contextKey = resolveFailureContextKey()
            if (!entryStillOriginal(contextKey)) {
              dropOriginalBookkeepingOnly()
              return
            }
            if (!snapshot) {
              reverseMapRef.current.delete(range.connection_id)
              clearAliasesPointingTo(contextKey)
              if (range.connection_id !== contextKey) {
                clearAliasesPointingTo(range.connection_id)
              }
              removeDeadCanonicalAndFireHandoff(contextKey, range.connection_id)
              return
            }
            const patch = denormalizeSnapshot(snapshot)
            dispatch({ type: "HYDRATE_FROM_SNAPSHOT", contextKey, patch })
            seedDelegationsFromSnapshotRef.current(
              patch.connectionId,
              patch.activeDelegations,
              patch.eventSeq
            )
            // Snapshot is a best-effort final view — the producer may still
            // advance SessionState after this; UI must not accept new prompts.
            dispatch({
              type: "ERROR",
              contextKey,
              message: failMessage,
            })
          } catch (err) {
            console.warn(
              "[acp-context] delivery-failure snapshot recovery failed",
              range.connection_id,
              err
            )
            const contextKey = resolveFailureContextKey()
            if (!entryStillOriginal(contextKey)) {
              dropOriginalBookkeepingOnly()
              return
            }
            reverseMapRef.current.delete(range.connection_id)
            clearAliasesPointingTo(contextKey)
            if (range.connection_id !== contextKey) {
              clearAliasesPointingTo(range.connection_id)
            }
            // Throw ≠ confirmed dead: clean up only; no handoff re-entry spawn.
            removeDeadCanonicalOnly(contextKey)
          }
        })()
      }

      pushAlertRef.current("error", t("eventErrorTitle"), failMessage)
    },
    [clearAliasesPointingTo, dispatch, settleListenerWaiters, t]
  )

  // Push envelopes into the frame ingestor. Optional flush for connect-time
  // buffered drains that must apply before the next tick.
  const pushMappedEvents = useCallback(
    (contextKey: string, events: readonly EventEnvelope[], flush = false) => {
      if (events.length === 0) return
      lastActivityRef.current.set(contextKey, Date.now())
      const ingestor = eventIngestorRef.current
      if (!ingestor) {
        console.warn(
          "[acp-context] event ingestor not ready; dropping mapped events",
          { contextKey, count: events.length }
        )
        return
      }
      ingestor.pushMapped(contextKey, events)
      if (flush) ingestor.flushNow()
    },
    []
  )

  // Apply a single envelope through the ingestor (attach / buffered drain).
  const applyMappedEnvelope = useCallback(
    (contextKey: string, envelope: EventEnvelope, flush = true) => {
      const conn = storeRef.current.connections.get(contextKey)
      if (conn && envelope.seq <= conn.lastAppliedSeq) return
      pushMappedEvents(contextKey, [envelope], flush)
    },
    [pushMappedEvents]
  )

  // Re-seed `DelegationProvider` bindings from a snapshot's active_delegations.
  // `delegation_started` / `delegation_completed` are transient — they mutate
  // no SessionState field, so they are NOT in `to_snapshot()` and (on the
  // snapshot attach path) are never replayed. Without this, a web/server client
  // that cold-attaches, re-attaches after a broadcast lag, or refreshes
  // mid-delegation never establishes the live binding: the card shows a
  // premature "completed" and no "查看会话" until the child finally finishes.
  // We synthesize the same envelopes the broker emits live and fan them ONLY to
  // the JS event subscribers (DelegationProvider), bypassing applyMappedEnvelope
  // so we neither run the store reducer (which has no case for these) nor touch
  // `lastAppliedSeq` / trip the seq-dedup. Idempotent with any live/replayed
  // event for the same `parent_tool_use_id` (DelegationProvider overwrites the
  // binding and `attachDelegationChild` early-returns when already attached).
  const seedDelegationsFromSnapshot = useCallback(
    (
      connectionId: string,
      activeDelegations: ActiveDelegationState[],
      eventSeq: number
    ) => {
      const envelopes = buildDelegationSeedEnvelopes(
        connectionId,
        activeDelegations,
        eventSeq
      )
      for (const envelope of envelopes) {
        notifyRawSubscribers(envelope)
      }
    },
    [notifyRawSubscribers]
  )
  seedDelegationsFromSnapshotRef.current = seedDelegationsFromSnapshot

  // Surface diagnostic evidence carried by a snapshot's `last_error`.
  //
  // Alerts are live-only, so a client that attached AFTER the error fired
  // (browser refresh, second tab, cold attach mid-session) would otherwise
  // never learn why the turn came back empty — the snapshot is its only
  // channel. Scoped to errors that actually carry evidence, i.e. the inferred
  // `turn_failed_empty*` family, so attaching to a connection with any older
  // error doesn't start raising alerts it never used to.
  //
  // Same routing rules as the live path: evidence goes to the alert only,
  // never to `conn.error` (composer tooltip) and never to an OS notification.
  // Held in a ref, following `pushAlertRef` above: the attach/hydrate
  // callbacks below are deliberately identity-stable (an empty dep array keeps
  // a re-render from tearing down and re-establishing live subscriptions), so
  // they must not close over a `t`-dependent callback directly.
  const surfaceSnapshotErrorDetails = useCallback(
    (
      contextKey: string,
      patch: import("@/lib/snapshot-denormalize").SnapshotPatch
    ) => {
      const evidence = patch.lastErrorDetails?.trim()
      if (!evidence) return
      if (alertedErrorDetailsRef.current.get(contextKey) === evidence) return
      alertedErrorDetailsRef.current.set(contextKey, evidence)
      pushAlertRef.current(
        "error",
        t("eventErrorTitle"),
        patch.lastError ?? undefined,
        undefined,
        evidence
      )
    },
    [t]
  )
  const surfaceSnapshotErrorDetailsRef = useRef(surfaceSnapshotErrorDetails)
  useEffect(() => {
    surfaceSnapshotErrorDetailsRef.current = surfaceSnapshotErrorDetails
  }, [surfaceSnapshotErrorDetails])

  // Open a Subscribe-with-Snapshot stream for `connectionId` and route its
  // frames into the store under `contextKey`. Returns the subscription
  // handle for cleanup, or `null` when the active transport doesn't
  // implement the attach protocol (caller falls back to the legacy
  // snapshot-fetch + global-listener flow).
  //
  // The subscription survives WS reconnects automatically — see
  // `WebEventStream.reattachAll`. Detach reasons are handled here:
  //   - lagged / server_shutdown: re-attach with current cursor so the
  //     consumer doesn't have to think about transient disconnects
  //   - connection_gone: terminal; clean up store entry and let the next
  //     user interaction surface the failure
  const setupAttachSubscription = useCallback(
    (
      contextKey: string,
      connectionId: string,
      sinceSeq: number | undefined,
      reconnectMode: "resume" | "cold" = "resume",
      shared?: { generation: number; leaseId: string }
    ): EventStreamSubscription | null => {
      const stream = getEventStream()
      if (!stream) return null

      let activeSub: EventStreamSubscription | null = null
      let lastBackgroundDetailRevision = 0
      let lastBackgroundTranscriptGeneration = 0
      const handlers: AttachHandlers = {
        onSnapshot: (snapshot) => {
          const record = attachRetryRef.current.get(contextKey)
          if (record) record.autoRetryUsed = false
          const patch = denormalizeSnapshot(snapshot)
          const detailRevision = patch.backgroundDetailRevision ?? 0
          const transcriptGeneration = patch.backgroundTranscriptGeneration ?? 0
          const recoverBackgroundDetail =
            detailRevision > lastBackgroundDetailRevision
          const resetBackgroundTranscript =
            transcriptGeneration > lastBackgroundTranscriptGeneration
          lastBackgroundDetailRevision = Math.max(
            lastBackgroundDetailRevision,
            detailRevision
          )
          lastBackgroundTranscriptGeneration = Math.max(
            lastBackgroundTranscriptGeneration,
            transcriptGeneration
          )
          dispatch({ type: "HYDRATE_FROM_SNAPSHOT", contextKey, patch })
          surfaceSnapshotErrorDetailsRef.current(contextKey, patch)
          lastActivityRef.current.set(contextKey, Date.now())
          seedDelegationsFromSnapshot(
            patch.connectionId,
            patch.activeDelegations,
            patch.eventSeq
          )
          // Feed cursor back so a queued batch drops old events.
          const applied = storeRef.current.connections.get(contextKey)
          const resumeSeq =
            applied?.connectionId === connectionId
              ? applied.lastAppliedSeq
              : patch.eventSeq
          eventIngestorRef.current?.resumeConnection(connectionId, resumeSeq)
          if (recoverBackgroundDetail || resetBackgroundTranscript) {
            const runtimeConversationId =
              (patch.sessionId
                ? getConversationIdByExternalIdFromStore(patch.sessionId)
                : null) ?? patch.conversationId
            if (runtimeConversationId != null) {
              const runtimeActions =
                useConversationRuntimeStore.getState().actions
              if (resetBackgroundTranscript) {
                runtimeActions.applyBackgroundActivity(
                  runtimeConversationId,
                  [],
                  0,
                  true
                )
              }
              runtimeActions.refetchDetail(runtimeConversationId, {
                preserveLive: true,
              })
            }
          }
        },
        onReplay: (events) => {
          pushMappedEvents(contextKey, events, true)
        },
        onEvent: (envelope) => {
          applyMappedEnvelope(contextKey, envelope, true)
        },
        onAttachError: (code, retryable) => {
          // Agent is still alive; only this attach frame failed. Keep
          // canonical state and wait for a new WS ready or an explicit retry.
          attachSubscriptionsRef.current.delete(contextKey)
          dispatch({
            type: "ATTACH_ERROR",
            contextKey,
            code,
            retryable,
          })
        },
        onDetached: (reason) => {
          if (reason === "lagged" || reason === "server_shutdown") {
            const conn = storeRef.current.connections.get(contextKey)
            const newSinceSeq = conn?.lastAppliedSeq
            const cursorOption =
              newSinceSeq !== undefined ? { sinceSeq: newSinceSeq } : {}
            const newSub = stream.attach(
              connectionId,
              reconnectMode === "cold"
                ? {
                    ...cursorOption,
                    reconnectMode: "cold",
                    ...(shared ? { shared } : {}),
                  }
                : {
                    ...cursorOption,
                    ...(shared ? { shared } : {}),
                  },
              handlers
            )
            activeSub = newSub
            attachSubscriptionsRef.current.set(contextKey, newSub)
            return
          }
          if (
            shared &&
            (reason === "lease_expired" ||
              reason === "generation_stale" ||
              reason === "session_replaced")
          ) {
            const request = lastConnectParamsRef.current.get(contextKey)
            const cursor =
              storeRef.current.connections.get(contextKey)?.lastAppliedSeq
            if (request && cursor != null) {
              lastConnectParamsRef.current.set(contextKey, {
                ...request,
                sharedReconnect: {
                  generation: shared.generation,
                  sinceSeq: cursor,
                },
              })
            }
            attachSubscriptionsRef.current.delete(contextKey)
            attachRetryRef.current.delete(contextKey)
            acpReleaseLease(
              connectionId,
              shared.generation,
              shared.leaseId
            ).catch(() => {})
            dispatch({ type: "CONNECTION_REMOVED", contextKey })
            if (request) {
              queueMicrotask(() => {
                connectRef
                  .current?.(
                    contextKey,
                    request.agentType,
                    request.workingDir,
                    request.sessionId,
                    request.conversationId,
                    request.delegationRouteOverride,
                    request.ownerOperationId,
                    request.intent,
                    request.retryObserverDiscovery
                  )
                  .catch(() => {})
              })
            }
            return
          }
          // Terminal detach (connection_gone): remove canonical state and every
          // tab alias that pointed at it so a later owner reconnect under the
          // tab key can resolve/getConnection correctly.
          attachSubscriptionsRef.current.delete(contextKey)
          attachRetryRef.current.delete(contextKey)
          clearAliasesPointingTo(contextKey)
          reverseMapRef.current.delete(connectionId)
          pendingUnmappedEventsRef.current.delete(connectionId)
          lastActivityRef.current.delete(contextKey)
          removeDeadCanonicalAndFireHandoff(contextKey, connectionId)
        },
      }

      // Only pass reconnectMode when cold so ordinary resume subscriptions keep
      // the historical attach options shape (`{ sinceSeq }` only).
      const cursorOption = sinceSeq !== undefined ? { sinceSeq } : {}
      activeSub = stream.attach(
        connectionId,
        reconnectMode === "cold"
          ? {
              ...cursorOption,
              reconnectMode: "cold",
              ...(shared ? { shared } : {}),
            }
          : { ...cursorOption, ...(shared ? { shared } : {}) },
        handlers
      )
      attachSubscriptionsRef.current.set(contextKey, activeSub)
      const prior = attachRetryRef.current.get(contextKey)
      attachRetryRef.current.set(contextKey, {
        connectionId,
        sinceSeq,
        reconnectMode,
        shared,
        autoRetryUsed: prior?.autoRetryUsed ?? false,
      })
      return activeSub
    },
    [
      applyMappedEnvelope,
      clearAliasesPointingTo,
      dispatch,
      pushMappedEvents,
      seedDelegationsFromSnapshot,
    ]
  )

  // Tear down an attach subscription: detach the WS subscription so the
  // server-side forwarder task exits, and clear the local handle.
  // Idempotent — safe to call from disconnect, idle sweep, REKEY, and
  // REMOVE_ALL paths without checking whether a sub exists. No-op for
  // legacy (Tauri) connections that never went through
  // `setupAttachSubscription`.
  const teardownAttachSubscription = useCallback((contextKey: string) => {
    const sub = attachSubscriptionsRef.current.get(contextKey)
    attachRetryRef.current.delete(contextKey)
    if (!sub) return
    attachSubscriptionsRef.current.delete(contextKey)
    try {
      sub.detach()
    } catch (err) {
      console.warn("[acp-context] attach detach threw:", err)
    }
  }, [])

  const retryAttachNow = useCallback(
    (contextKey: string, isAuto = false) => {
      const canonical = observerAliasesRef.current.get(contextKey) ?? contextKey
      const record = attachRetryRef.current.get(canonical)
      if (!record) return
      const conn = storeRef.current.connections.get(canonical)
      if (!conn?.attachError?.retryable) return
      if (isAuto && record.autoRetryUsed) return
      if (isAuto) record.autoRetryUsed = true
      const errorCode = conn.attachError.code
      const sinceSeq =
        errorCode === "snapshot_budget_exceeded"
          ? undefined
          : conn.lastAppliedSeq > 0
            ? conn.lastAppliedSeq
            : record.sinceSeq
      const reconnectMode =
        errorCode === "snapshot_budget_exceeded" ? "cold" : record.reconnectMode
      dispatch({ type: "CLEAR_ATTACH_ERROR", contextKey: canonical })
      setupAttachSubscription(
        canonical,
        record.connectionId,
        sinceSeq,
        reconnectMode,
        record.shared
      )
    },
    [dispatch, setupAttachSubscription]
  )
  retryAttachRef.current = retryAttachNow

  const retryAttach = useCallback(
    (contextKey: string) => {
      retryAttachNow(contextKey, false)
    },
    [retryAttachNow]
  )

  useEffect(() => {
    const unsub = getTransport().onReconnect?.(() => {
      for (const key of [...attachRetryRef.current.keys()]) {
        retryAttachRef.current(key, true)
      }
    })
    return () => {
      unsub?.()
    }
  }, [])

  // The desktop subscription is process-global for this provider mount. Keep
  // callbacks current without rebuilding the async listener when translations
  // or lifecycle closures change during a render.
  const desktopListenerCallbacksRef = useRef({
    commitEventFrame,
    handleSequenceGap,
    handleDesktopDeliveryFailure,
    t,
  })
  useEffect(() => {
    desktopListenerCallbacksRef.current = {
      commitEventFrame,
      handleSequenceGap,
      handleDesktopDeliveryFailure,
      t,
    }
  })

  // One EventIngestor for desktop + attach. Desktop also subscribes via
  // subscribeDesktopAcpEvents (never hot-switches after failure).
  useEffect(() => {
    let cancelled = false
    let unsubDesktop: (() => void) | null = null

    const buildIngestor = () =>
      new EventIngestor({
        resolveContextKey: (connectionId) =>
          reverseMapRef.current.get(connectionId) ?? null,
        readCursor: (contextKey) =>
          storeRef.current.connections.get(contextKey)?.lastAppliedSeq ?? 0,
        commit: (frame) => {
          desktopListenerCallbacksRef.current.commitEventFrame(frame)
        },
        onGap: (gap) => {
          desktopListenerCallbacksRef.current.handleSequenceGap(gap)
        },
        onDuplicate: (info) => {
          if (
            streamingPerfRecorder.isActive() &&
            streamingPerfRecorder.matchesTargetConnection(info.connectionId)
          ) {
            streamingPerfRecorder.markFrontendDuplicate()
          }
        },
        onUnmapped: (event) => {
          bufferUnmappedEvent(event)
        },
        scheduleFrame: (cb) => requestAnimationFrame(cb),
        cancelFrame: (handle) => cancelAnimationFrame(handle),
      })

    const start = async () => {
      listenerPhaseRef.current = "starting"
      listenerInitErrorRef.current = null

      // Re-read durable flag (Provider remount after soft reload).
      if (readDesktopDeliveryFailed()) {
        desktopDeliveryFailedRef.current = true
      }

      if (desktopDeliveryFailedRef.current) {
        // Terminal failure: do not re-subscribe (no hot-switch / no legacy).
        settleListenerWaiters(
          "failed",
          new Error("Desktop ACP delivery previously failed; restart the app")
        )
        return
      }

      const hasAttachStream = getEventStream() !== null

      if (hasAttachStream) {
        // Web / remote: no Tauri capability command; explicit legacy snapshot.
        try {
          initializeStreamingPerformanceConfig(LEGACY_DISABLED_CAPABILITIES)
        } catch {
          // Already initialized in this process (HMR / remount).
        }
        if (cancelled) {
          settleListenerWaiters("cancelled")
          return
        }
        const ingestor = buildIngestor()
        eventIngestorRef.current = ingestor
        settleListenerWaiters("ready")
        return
      }

      // Desktop firehose path.
      // Never invent a delivery mode: if capability query fails we fail closed
      // rather than assume legacy while the backend may be emitting batches.
      let capabilities: DesktopDeliveryCapabilities | null = null
      if (getTransport().isDesktop()) {
        const maxAttempts = 3
        for (let attempt = 1; attempt <= maxAttempts; attempt++) {
          try {
            capabilities = await acpGetDesktopDeliveryCapabilities()
            break
          } catch (err) {
            console.warn(
              `[acp-context] desktop delivery capabilities failed (attempt ${attempt}/${maxAttempts})`,
              err
            )
            if (attempt < maxAttempts) {
              await new Promise((r) => setTimeout(r, 50 * attempt))
            }
          }
        }
        if (!capabilities) {
          console.error(
            "[acp-context] desktop delivery capabilities unavailable; listener failed"
          )
          const err = new Error(
            "Could not negotiate desktop event delivery. Restart the app."
          )
          pushAlertRef.current(
            "error",
            desktopListenerCallbacksRef.current.t("eventErrorTitle"),
            desktopListenerCallbacksRef.current.t(
              "desktopDeliveryNegotiateFailed"
            )
          )
          if (!cancelled) settleListenerWaiters("failed", err)
          return
        }
      } else {
        capabilities = LEGACY_DISABLED_CAPABILITIES
      }
      if (cancelled) {
        settleListenerWaiters("cancelled")
        return
      }
      try {
        capabilities = initializeStreamingPerformanceConfig(capabilities)
      } catch {
        // Already initialized with the same snapshot.
        capabilities =
          getStreamingPerformanceConfig() ?? LEGACY_DISABLED_CAPABILITIES
      }
      if (cancelled) {
        settleListenerWaiters("cancelled")
        return
      }

      const ingestor = buildIngestor()
      eventIngestorRef.current = ingestor

      try {
        unsubDesktop = await subscribeDesktopAcpEvents(capabilities, {
          onBatch: (batch: DesktopAcpEventBatch) => {
            if (streamingPerfRecorder.isActive()) {
              streamingPerfRecorder.markBatchReceived(
                batch.batch_id,
                batch.events.length
              )
            }
            for (const event of batch.events) {
              const key = reverseMapRef.current.get(event.connection_id)
              if (key) lastActivityRef.current.set(key, Date.now())
            }
            eventIngestorRef.current?.pushBatch(batch)
          },
          onFailure: (failure) => {
            desktopListenerCallbacksRef.current.handleDesktopDeliveryFailure(
              failure
            )
          },
        })
      } catch (err) {
        console.error("[acp-context] desktop ACP subscribe failed", err)
        eventIngestorRef.current?.dispose()
        eventIngestorRef.current = null
        pushAlertRef.current(
          "error",
          desktopListenerCallbacksRef.current.t("eventErrorTitle"),
          desktopListenerCallbacksRef.current.t(
            "desktopDeliverySubscribeFailed"
          )
        )
        if (!cancelled) {
          settleListenerWaiters(
            "failed",
            err instanceof Error
              ? err
              : new Error("Failed to subscribe to desktop ACP events")
          )
        }
        return
      }

      if (cancelled) {
        unsubDesktop?.()
        ingestor.dispose()
        eventIngestorRef.current = null
        settleListenerWaiters("cancelled")
        return
      }
      settleListenerWaiters("ready")
    }

    void start()

    return () => {
      cancelled = true
      // Reject waiters — never resolve when the listener is already torn down.
      if (listenerPhaseRef.current !== "ready") {
        settleListenerWaiters("cancelled")
      } else {
        listenerPhaseRef.current = "cancelled"
      }
      unsubDesktop?.()
      eventIngestorRef.current?.dispose()
      eventIngestorRef.current = null
    }
  }, [bufferUnmappedEvent, settleListenerWaiters])

  // ── Backend keepalive timer ──
  // Frontend is the only side that knows which conversation tabs the
  // user has open. Without this, the backend's idle sweep
  // (CODEG_ACP_IDLE_TIMEOUT_SECS, default 180s) would reap connections
  // backing visible tabs whenever the user was just reading without
  // sending — forcing them to re-spawn the agent on next message.
  // Touching only bumps last_activity_at; it does not emit any event.
  useEffect(() => {
    const timer = setInterval(() => {
      const currentActiveKey = storeRef.current.activeKey
      const currentOpenTabKeys = openTabKeysRef.current
      const seen = new Set<string>()
      const toTouch: string[] = []
      const consider = (contextKey: string) => {
        if (seen.has(contextKey)) return
        seen.add(contextKey)
        const conn = storeRef.current.connections.get(contextKey)
        if (!conn) return
        // Prompting is already sweep-protected on the backend; touching
        // is harmless but redundant. Connecting hasn't reached the
        // sweep-eligible state yet. Only Connected matters.
        if (conn.status !== "connected") return
        // Shared roots are kept alive by the EventStream's 30s lease ping.
        if (conn.sharedSession) return
        toTouch.push(conn.connectionId)
      }
      if (currentActiveKey) consider(currentActiveKey)
      for (const contextKey of currentOpenTabKeys) consider(contextKey)
      for (const connectionId of toTouch) {
        acpTouchConnection(connectionId).catch(() => {})
      }
    }, CONNECTION_KEEPALIVE_INTERVAL_MS)

    return () => clearInterval(timer)
  }, [])

  // Pop-out: register release / claim / reclaim so orchestration and detached
  // bootstrap can transfer ownership without acpDisconnect. Cleared on unmount.
  // Released owner snapshots are kept per (conversationId, operationId) so
  // abort/compensation can reclaim as owner without a second acpConnect.
  const releasedForReclaimRef = useRef(
    new Map<
      string,
      Array<{
        contextKey: string
        connectionId: string
        agentType: AgentType
        workingDir: string | null
        conversationId: number
        ownershipGeneration: number | null
        ownerOperationId: string | null
        ownerWindowLabel: string | null
      }>
    >()
  )

  useEffect(() => {
    const releaseKey = (conversationId: number, operationId: string) =>
      `${conversationId}:${operationId}`

    registerPopoutAcpBridge({
      releaseConnectionWithoutDisconnect: (conversationId, operationId) => {
        const store = storeRef.current
        const snapshot: Array<{
          contextKey: string
          connectionId: string
          agentType: AgentType
          workingDir: string | null
          conversationId: number
          ownershipGeneration: number | null
          ownerOperationId: string | null
          ownerWindowLabel: string | null
        }> = []
        for (const [contextKey, conn] of store.connections) {
          if (conn.conversationId !== conversationId) continue
          if (conn.isDelegationChild || conn.isViewer) continue
          snapshot.push({
            contextKey,
            connectionId: conn.connectionId,
            agentType: conn.agentType,
            workingDir: conn.workingDir,
            conversationId,
            ownershipGeneration: conn.ownershipGeneration ?? null,
            ownerOperationId: conn.ownerOperationId ?? operationId,
            ownerWindowLabel: conn.ownerWindowLabel ?? "main",
          })
          // Viewer-style teardown: drop local tracking only.
          teardownAttachSubscription(contextKey)
          reverseMapRef.current.delete(conn.connectionId)
          pendingUnmappedEventsRef.current.delete(conn.connectionId)
          lastActivityRef.current.delete(contextKey)
          clearAliasesPointingTo(contextKey)
          if (conn.connectionId !== contextKey) {
            clearAliasesPointingTo(conn.connectionId)
          }
          dispatch({ type: "CONNECTION_REMOVED", contextKey })
        }
        if (snapshot.length > 0) {
          releasedForReclaimRef.current.set(
            releaseKey(conversationId, operationId),
            snapshot
          )
        }
      },
      hasReleasedForReclaim: (conversationId, operationId) => {
        const snap = releasedForReclaimRef.current.get(
          releaseKey(conversationId, operationId)
        )
        return Array.isArray(snap) && snap.length > 0
      },
      reclaimAfterAbort: async (conversationId, operationId, lease) => {
        const key = releaseKey(conversationId, operationId)
        const snapshot = releasedForReclaimRef.current.get(key)
        releasedForReclaimRef.current.delete(key)

        // Prefer an explicit post-reverse lease (operationId + new generation
        // stamped by reverse rebind) over the pre-transfer release snapshot.
        const adoptFromLease = (fallback: {
          ownershipGeneration: number | null
          ownerOperationId: string | null
          ownerWindowLabel: string | null
        }) => ({
          ownershipGeneration:
            lease?.ownershipGeneration !== undefined
              ? lease.ownershipGeneration
              : fallback.ownershipGeneration,
          ownerOperationId:
            lease?.ownershipGeneration !== undefined
              ? operationId
              : (fallback.ownerOperationId ?? operationId),
          ownerWindowLabel:
            lease?.ownerWindowLabel !== undefined &&
            lease.ownerWindowLabel !== null
              ? lease.ownerWindowLabel
              : (fallback.ownerWindowLabel ?? "main"),
        })

        if (!snapshot?.length) {
          // Pre-ready abort: main never released. Refresh lease in place on
          // existing owner entries — never invent CONNECTION_CREATED.
          let refreshed = 0
          if (
            lease?.ownershipGeneration != null &&
            Number.isFinite(lease.ownershipGeneration)
          ) {
            for (const [contextKey, conn] of storeRef.current.connections) {
              if (conn.conversationId !== conversationId) continue
              if (conn.isDelegationChild || conn.isViewer) continue
              dispatch({
                type: "OWNERSHIP_LEASE_UPDATED",
                contextKey,
                ...adoptFromLease({
                  ownershipGeneration: conn.ownershipGeneration ?? null,
                  ownerOperationId: conn.ownerOperationId ?? null,
                  ownerWindowLabel: conn.ownerWindowLabel ?? null,
                }),
              })
              refreshed += 1
            }
          }
          // Map empty + no releasedForReclaim snapshot: cannot adopt a reverse
          // lease. Fail so compensate keeps the transfer fence (do not report
          // success with zero UI owners).
          if (
            refreshed === 0 &&
            lease?.ownershipGeneration != null &&
            Number.isFinite(lease.ownershipGeneration)
          ) {
            throw new Error(
              "reclaim_failed: no main owner entry or releasedForReclaim snapshot"
            )
          }
          return
        }

        for (const entry of snapshot) {
          const {
            contextKey,
            connectionId,
            agentType,
            workingDir,
            ownershipGeneration,
            ownerOperationId,
            ownerWindowLabel,
          } = entry
          const existing = storeRef.current.connections.get(contextKey)
          if (
            existing &&
            existing.connectionId === connectionId &&
            !existing.isViewer
          ) {
            // Already owning this connection — refresh lease, keep live.
            dispatch({
              type: "OWNERSHIP_LEASE_UPDATED",
              contextKey,
              ...adoptFromLease({
                ownershipGeneration: ownershipGeneration ?? null,
                ownerOperationId: ownerOperationId ?? null,
                ownerWindowLabel: ownerWindowLabel ?? null,
              }),
            })
            continue
          }
          if (existing) {
            teardownAttachSubscription(contextKey)
            reverseMapRef.current.delete(existing.connectionId)
            pendingUnmappedEventsRef.current.delete(existing.connectionId)
            lastActivityRef.current.delete(contextKey)
            clearAliasesPointingTo(contextKey)
            if (existing.connectionId !== contextKey) {
              clearAliasesPointingTo(existing.connectionId)
            }
            dispatch({ type: "CONNECTION_REMOVED", contextKey })
          }
          // Prove the backend connection is still live BEFORE publishing a
          // local owner. Ok(None)/failure → ConnectionGone (no dead owner).
          let snapshotPayload: Awaited<
            ReturnType<typeof acpGetSessionSnapshot>
          > = null
          try {
            snapshotPayload = await acpGetSessionSnapshot(connectionId)
          } catch (e) {
            console.warn(
              "[acp-context] reclaimAfterAbort snapshot failed for",
              connectionId,
              e
            )
            throw new Error("connection_gone")
          }
          if (!snapshotPayload) {
            throw new Error("connection_gone")
          }

          // Re-attach as owner for the still-live backend connection.
          // Without the reverse-lease override, a second failed handoff would
          // restore a stale operationId and later disconnect_if_owner no-ops.
          const adoptedLease = adoptFromLease({
            ownershipGeneration: ownershipGeneration ?? null,
            ownerOperationId: ownerOperationId ?? null,
            ownerWindowLabel: ownerWindowLabel ?? null,
          })
          dispatch({
            type: "CONNECTION_CREATED",
            contextKey,
            connectionId,
            agentType,
            workingDir: workingDir ?? null,
            isViewer: false,
            conversationId,
            ...adoptedLease,
          })
          lastActivityRef.current.set(contextKey, Date.now())

          const stream = getEventStream()
          if (stream) {
            setupAttachSubscription(contextKey, connectionId, undefined)
            reverseMapRef.current.set(connectionId, contextKey)
            continue
          }

          const patch = denormalizeSnapshot(snapshotPayload)
          if (
            storeRef.current.connections.get(contextKey)?.connectionId !==
            connectionId
          ) {
            continue
          }
          dispatch({ type: "HYDRATE_FROM_SNAPSHOT", contextKey, patch })
          seedDelegationsFromSnapshot(
            patch.connectionId,
            patch.activeDelegations,
            patch.eventSeq
          )
          reverseMapRef.current.set(connectionId, contextKey)
          for (const env of consumeBufferedEvents(connectionId)) {
            applyMappedEnvelope(contextKey, env)
          }
        }
      },
      claimConnectionOwnership: async (args) => {
        // Cold: no live connection to claim — surface will connect later.
        if (!args.connectionId) {
          return {}
        }
        // Live takeover: attach as OWNER UI (not permanent viewer) under the
        // detached context key. Rebind is performed by the page before claim
        // (or generation is returned after). Must not spawn a second agent.
        const {
          connectionId,
          contextKey,
          agentType,
          workingDir,
          conversationId,
          operationId,
          ownershipGeneration,
          ownerWindowLabel,
        } = args
        const lease = {
          ownershipGeneration: ownershipGeneration ?? null,
          ownerOperationId: operationId,
          ownerWindowLabel:
            ownerWindowLabel ?? `conversation-${conversationId}`,
        }
        const existing = storeRef.current.connections.get(contextKey)
        if (
          existing &&
          existing.connectionId === connectionId &&
          !existing.isViewer
        ) {
          // Already claimed as owner under this key — keep live state.
          return {
            connectionId,
            ownershipGeneration:
              ownershipGeneration ?? existing.ownershipGeneration ?? undefined,
          }
        }
        // Drop any stale entry at this key first (viewer-style only).
        if (existing) {
          teardownAttachSubscription(contextKey)
          reverseMapRef.current.delete(existing.connectionId)
          pendingUnmappedEventsRef.current.delete(existing.connectionId)
          lastActivityRef.current.delete(contextKey)
          clearAliasesPointingTo(contextKey)
          if (existing.connectionId !== contextKey) {
            clearAliasesPointingTo(existing.connectionId)
          }
          dispatch({ type: "CONNECTION_REMOVED", contextKey })
        }
        dispatch({
          type: "CONNECTION_CREATED",
          contextKey,
          connectionId,
          agentType,
          workingDir: workingDir ?? null,
          isViewer: false,
          conversationId,
          ...lease,
        })
        lastActivityRef.current.set(contextKey, Date.now())

        const stream = getEventStream()
        if (stream) {
          setupAttachSubscription(contextKey, connectionId, undefined)
          return {
            connectionId,
            ownershipGeneration: ownershipGeneration ?? undefined,
          }
        }

        // Desktop firehose: snapshot + reverse-map (same as viewer path, but
        // owner so unmount disconnect uses incarnation CAS / can acpDisconnect).
        let patch: import("@/lib/snapshot-denormalize").SnapshotPatch | null =
          null
        try {
          const snapshot = await acpGetSessionSnapshot(connectionId)
          if (snapshot) patch = denormalizeSnapshot(snapshot)
        } catch (e) {
          console.warn(
            "[acp-context] claim ownership snapshot failed for",
            connectionId,
            e
          )
        }
        if (
          storeRef.current.connections.get(contextKey)?.connectionId !==
          connectionId
        ) {
          return {
            connectionId,
            ownershipGeneration: ownershipGeneration ?? undefined,
          }
        }
        if (patch) {
          dispatch({ type: "HYDRATE_FROM_SNAPSHOT", contextKey, patch })
          seedDelegationsFromSnapshot(
            patch.connectionId,
            patch.activeDelegations,
            patch.eventSeq
          )
        }
        reverseMapRef.current.set(connectionId, contextKey)
        for (const env of consumeBufferedEvents(connectionId)) {
          applyMappedEnvelope(contextKey, env)
        }
        return {
          connectionId,
          ownershipGeneration: ownershipGeneration ?? undefined,
        }
      },
    })
    return () => {
      registerPopoutAcpBridge(null)
    }
  }, [
    applyMappedEnvelope,
    clearAliasesPointingTo,
    consumeBufferedEvents,
    dispatch,
    seedDelegationsFromSnapshot,
    setupAttachSubscription,
    teardownAttachSubscription,
  ])

  // ── Idle sweep timer ──
  // Complements the backend keepalive: this sweep targets connections
  // that are NOT in `openTabKeys ∪ {activeKey}` — i.e. connections the
  // frontend opened but is no longer surfacing to the user (panel
  // dismissed, navigated away). The backend's own idle sweep would
  // reap them on its 60s cadence regardless; doing it here too keeps
  // the React store free of stale entries and triggers an explicit
  // disconnect rather than waiting for the backend's own timeout.
  // Connections backing currently-open tabs are never reaped here —
  // those are kept alive by the keepalive loop above.
  useEffect(() => {
    const timer = setInterval(() => {
      const now = Date.now()
      const currentActiveKey = storeRef.current.activeKey

      const currentOpenTabKeys = openTabKeysRef.current
      const toDisconnect: { contextKey: string; connectionId: string }[] = []
      for (const [contextKey, conn] of storeRef.current.connections) {
        if (contextKey === currentActiveKey) continue
        if (currentOpenTabKeys.has(contextKey)) continue
        if (conn.status === "prompting" || conn.status === "connecting") {
          continue
        }
        if (conn.status !== "connected") continue
        // Delegation children are owned by the broker — the
        // delegation_completed event is the only signal that should
        // tear them down (via detachDelegationChild). The idle sweep
        // would otherwise call acpDisconnect on a backend connection
        // still mid-prompt for the parent's tool_use.
        if (conn.isDelegationChild) continue
        // Viewers don't own their backend connection — acpDisconnect here
        // would kill another client's agent. The viewer is torn down when its
        // tab unmounts (disconnect's isViewer branch detaches it).
        if (conn.isViewer) continue
        // Launched-but-unresolved background work (async sub-agent /
        // background shell): disconnecting would kill the agent CLI and the
        // background task with it. The backend watcher settles or max-age
        // expires the accounting and emits `outstanding: 0`, which re-arms
        // this sweep for the connection.
        if (conn.backgroundOutstanding > 0) continue
        // Pop-out transfer: main released ownership without disconnect;
        // reaping here would kill the session the detached window just claimed.
        if (
          conn.conversationId != null &&
          (isTransferringOut(conn.conversationId) ||
            isFrontendDisconnectSuppressed(conn.conversationId))
        ) {
          continue
        }
        const lastActive = lastActivityRef.current.get(contextKey) ?? 0
        if (now - lastActive > CONNECTION_IDLE_TIMEOUT_MS) {
          toDisconnect.push({
            contextKey,
            connectionId: conn.connectionId,
          })
        }
      }

      for (const { contextKey, connectionId } of toDisconnect) {
        const leaseConn = storeRef.current.connections.get(contextKey)
        if (leaseConn?.sharedSession) {
          acpReleaseLease(
            connectionId,
            leaseConn.sharedSession.generation,
            leaseConn.sharedSession.leaseId
          ).catch(() => {})
        } else {
          acpDisconnect(
            connectionId,
            disconnectLeaseWithOrigin(
              leaseConn ? leaseArgsForDisconnect(leaseConn) : null,
              "idle_timeout"
            )
          ).catch(() => {})
        }
        reverseMapRef.current.delete(connectionId)
        teardownAttachSubscription(contextKey)
        lastActivityRef.current.delete(contextKey)
        pendingUnmappedEventsRef.current.delete(connectionId)
        clearAliasesPointingTo(contextKey)
        if (connectionId !== contextKey) {
          clearAliasesPointingTo(connectionId)
        }
        // Reclaimed for idleness, not closed: the tab is still open and its
        // Reconnect button must resume this session, not start a new one.
        captureIdentityBeforeRemoval(contextKey)
        dispatch({ type: "CONNECTION_REMOVED", contextKey })
      }
    }, IDLE_SWEEP_INTERVAL_MS)

    return () => clearInterval(timer)
  }, [clearAliasesPointingTo, dispatch, teardownAttachSubscription])

  // Disconnect all on unmount
  useEffect(() => {
    const attachSubs = attachSubscriptionsRef.current
    // Capture the store ref at effect-setup time so the cleanup
    // function doesn't read a moving target (`storeRef.current` is the
    // same object across renders by design, but the lint rule
    // `react-hooks/exhaustive-deps` flags reading it inside cleanup
    // because in the general case a ref's `.current` can be replaced).
    const store = storeRef.current
    return () => {
      for (const conn of store.connections.values()) {
        // Delegation-child entries are not real user-facing
        // connections — the broker owns their backend lifecycle and
        // will tear them down when the parent's delegation resolves.
        // Calling acpDisconnect on them here would race the broker's
        // own one-shot teardown and emit a benign-but-noisy "unknown
        // connection" error from the backend.
        if (conn.isDelegationChild) continue
        // Viewers attach to a connection another client owns — never
        // acpDisconnect it on our unmount. The attach-sub detach loop below
        // releases our read-only subscription cleanly.
        if (conn.isViewer) continue
        // Pop-out handoff: ownership moved (or is moving) to a detached window.
        if (
          conn.conversationId != null &&
          (isTransferringOut(conn.conversationId) ||
            isFrontendDisconnectSuppressed(conn.conversationId))
        ) {
          continue
        }
        if (conn.sharedSession) {
          acpReleaseLease(
            conn.connectionId,
            conn.sharedSession.generation,
            conn.sharedSession.leaseId
          ).catch(() => {})
        } else {
          acpDisconnect(
            conn.connectionId,
            disconnectLeaseWithOrigin(
              leaseArgsForDisconnect(conn),
              "provider_unmount"
            )
          ).catch(() => {})
        }
      }
      for (const [, sub] of attachSubs) {
        try {
          sub.detach()
        } catch {
          // best-effort during teardown
        }
      }
    }
  }, [])

  // Attach this client to a backend connection ANOTHER client owns
  // (cross-client live streaming). The viewer is a NON-OWNING, co-controlling
  // client: it streams the same turn and may also drive the shared agent
  // (sendPrompt/cancel go to the owner's connection, serialized server-side by
  // its prompt_lock; turn-level concurrency rejection is tracked as a
  // follow-up). The one hard invariant: a viewer's teardown DETACHES, never
  // `acpDisconnect`s — that would kill the owner's agent. Generalizes
  // `attachDelegationChild`: Subscribe-with-Snapshot attach on web, snapshot-
  // hydrate + firehose reverse-map on desktop.
  //
  // ALWAYS a COLD attach (no `sinceSeq`): the viewer has applied no prior
  // events, so it must receive a full snapshot of the in-flight turn — passing
  // the discovered `event_seq` as a cursor could yield only a post-cursor
  // replay and miss all earlier live state. Reconnects re-attach with the
  // running `lastAppliedSeq` (see `setupAttachSubscription.onDetached`).
  const releaseObserverAlias = useCallback(
    (alias: string): string | null => {
      const canonical = observerAliasesRef.current.get(alias)
      if (!canonical) return null
      observerAliasesRef.current.delete(alias)
      liveSinksRef.current.delete(alias)
      notifyKeyListeners(alias)

      const hasOtherAlias = aliasKeysFor(canonical).length > 0
      const conn = storeRef.current.connections.get(canonical)
      if (!hasOtherAlias && conn?.isViewer && !conn.isDelegationChild) {
        teardownAttachSubscription(canonical)
        reverseMapRef.current.delete(conn.connectionId)
        pendingUnmappedEventsRef.current.delete(conn.connectionId)
        lastActivityRef.current.delete(canonical)
        dispatch({ type: "CONNECTION_REMOVED", contextKey: canonical })
      }
      return canonical
    },
    [aliasKeysFor, dispatch, notifyKeyListeners, teardownAttachSubscription]
  )

  const bindObserverAlias = useCallback(
    (
      alias: string,
      connectionId: string,
      agentType: AgentType,
      workingDir: string | null,
      conversationId: number | null
    ) => {
      const previous = observerAliasesRef.current.get(alias)
      if (previous && previous !== connectionId) releaseObserverAlias(alias)
      observerAliasesRef.current.set(alias, connectionId)

      const existing = storeRef.current.connections.get(connectionId)
      if (existing) {
        dispatch({
          type: "OBSERVER_METADATA_MERGED",
          contextKey: connectionId,
          conversationId,
          workingDir,
        })
      } else {
        dispatch({
          type: "CONNECTION_CREATED",
          contextKey: connectionId,
          connectionId,
          agentType,
          workingDir,
          isViewer: true,
          conversationId,
        })
      }
      notifyKeyListeners(alias)
    },
    [dispatch, notifyKeyListeners, releaseObserverAlias]
  )

  const connectAsViewer = useCallback(
    async (
      contextKey: string,
      connectionId: string,
      agentType: AgentType,
      workingDir: string | null,
      conversationId: number | null,
      reconnectMode: "resume" | "cold" = "resume"
    ) => {
      bindObserverAlias(
        contextKey,
        connectionId,
        agentType,
        workingDir,
        conversationId
      )
      lastActivityRef.current.set(connectionId, Date.now())

      const stream = getEventStream()
      if (stream) {
        // Web / remote: the per-connection WS attach delivers snapshot +
        // replay + live events atomically over the same socket. One
        // subscription per backend connectionId. Cold observers replace any
        // prior resume subscription (e.g. parent DELEGATION_CHILD_ATTACH)
        // so reconnect always requests a full snapshot.
        if (
          reconnectMode === "cold" &&
          attachSubscriptionsRef.current.has(connectionId)
        ) {
          teardownAttachSubscription(connectionId)
        }
        if (!attachSubscriptionsRef.current.has(connectionId)) {
          setupAttachSubscription(
            connectionId,
            connectionId,
            undefined,
            reconnectMode
          )
        }
        return
      }

      // Desktop firehose: the global `acp://event` stream only carries FUTURE
      // events, so fetch a snapshot to backfill the in-flight turn, then route
      // this connection's events via the reverse-map and drain anything that
      // arrived while the snapshot was in flight. Mirrors the legacy owner
      // path in `connect()`.
      let patch: import("@/lib/snapshot-denormalize").SnapshotPatch | null =
        null
      try {
        const snapshot = await acpGetSessionSnapshot(connectionId)
        if (snapshot) patch = denormalizeSnapshot(snapshot)
      } catch (e) {
        console.warn(
          "[acp-context] viewer snapshot fetch failed for",
          connectionId,
          e
        )
      }
      // Detach race: the tab may have disconnected (disconnect() removed the
      // entry) while the snapshot fetch was in flight. Re-check the store still
      // holds THIS viewer connection BEFORE applying the snapshot, seeding
      // delegations, or installing firehose routing — otherwise we'd hydrate /
      // seed child streams / route for a viewer no one is watching anymore.
      if (
        storeRef.current.connections.get(connectionId)?.connectionId !==
        connectionId
      ) {
        return
      }
      if (patch) {
        dispatch({
          type: "HYDRATE_FROM_SNAPSHOT",
          contextKey: connectionId,
          patch,
        })
        seedDelegationsFromSnapshot(
          patch.connectionId,
          patch.activeDelegations,
          patch.eventSeq
        )
      }
      reverseMapRef.current.set(connectionId, connectionId)
      for (const env of consumeBufferedEvents(connectionId)) {
        applyMappedEnvelope(connectionId, env)
      }
    },
    [
      applyMappedEnvelope,
      bindObserverAlias,
      consumeBufferedEvents,
      dispatch,
      seedDelegationsFromSnapshot,
      setupAttachSubscription,
      teardownAttachSubscription,
    ]
  )

  const isConnectionOwnedLocally = useCallback((connectionId: string) => {
    if (reverseMapRef.current.has(connectionId)) return true
    for (const conn of storeRef.current.connections.values()) {
      if (conn.connectionId === connectionId) return true
    }
    return false
  }, [])

  const connect = useCallback(
    async (
      contextKey: string,
      agentType: AgentType,
      workingDir?: string,
      sessionId?: string,
      conversationId?: number,
      delegationRouteOverride?: DelegationRoutePolicy | null,
      ownerOperationId?: string | null,
      intent: ConnectionIntent = "own_or_observe",
      retryObserverDiscovery = false
    ) => {
      const remembered = lastConnectParamsRef.current.get(contextKey)
      const launchIdentityChanged =
        remembered != null &&
        (remembered.agentType !== agentType ||
          (remembered.workingDir ?? null) !== (workingDir ?? null))
      const request: ConnectRequest = {
        agentType,
        workingDir,
        sessionId,
        conversationId,
        delegationRouteOverride,
        ownerOperationId: ownerOperationId ?? null,
        intent,
        retryObserverDiscovery,
        sharedRequestId: launchIdentityChanged
          ? newSharedRequestId()
          : (remembered?.sharedRequestId ?? newSharedRequestId()),
        retryFailedGeneration: launchIdentityChanged
          ? undefined
          : remembered?.retryFailedGeneration,
        sharedReconnect: launchIdentityChanged
          ? undefined
          : remembered?.sharedReconnect,
      }
      // Remember BEFORE the in-flight early return and before the preflight can
      // throw: a connect that never produced a store entry is precisely when
      // `reconnect()` has nothing else to go on.
      lastConnectParamsRef.current.set(contextKey, request)
      if (connectingKeysRef.current.has(contextKey)) {
        pendingConnectRequestsRef.current.set(contextKey, request)
        // Only cancel in-flight observer/handoff delays when the queued request
        // supersedes the current attempt. An identical reconnect must not abort
        // mid-settle or the tab can strand with neither observer nor owner.
        const inflight = inflightConnectRequestsRef.current.get(contextKey)
        if (!inflight || !sameConnectRequest(inflight, request)) {
          cancelObserverDelay(contextKey)
          // Intent/param change also drops any already-queued re-entry microtask.
          cancelHandoffReentry(contextKey)
        }
        return
      }
      connectingKeysRef.current.add(contextKey)
      inflightConnectRequestsRef.current.set(contextKey, request)
      // A fresh connect supersedes any prior handoff re-entry watcher / queued
      // re-entry microtask for this tab — the current attempt owns scheduling.
      clearHandoffWatcher(contextKey)

      const isConnectAbandonedOrSuperseded = (
        key: string,
        activeRequest: ConnectRequest
      ): boolean => {
        if (abandonedKeysRef.current.has(key)) return true
        const queued = pendingConnectRequestsRef.current.get(key)
        return queued != null && !sameConnectRequest(queued, activeRequest)
      }

      const reattachHandoffObserver = async (
        handoffKey: string,
        brokerConnectionId: string,
        handoffAgentType: AgentType,
        handoffWorkingDir: string | null,
        handoffConversationId: number,
        handoffRequest: ConnectRequest
      ): Promise<void> => {
        // Re-check after any prior await (discovery failure path) so disconnect
        // / intent supersession cannot reattach a dead or replaced request.
        if (isConnectAbandonedOrSuperseded(handoffKey, handoffRequest)) return
        await connectAsViewer(
          handoffKey,
          brokerConnectionId,
          handoffAgentType,
          handoffWorkingDir,
          handoffConversationId,
          "resume"
        )
        if (isConnectAbandonedOrSuperseded(handoffKey, handoffRequest)) return
        scheduleOwnOrObserveOnBrokerRemoved(
          handoffKey,
          brokerConnectionId,
          handoffRequest
        )
      }

      /**
       * After handoff discovery confirms the broker is gone (`null`), drop any
       * leftover connectionId-keyed entry that `releaseObserverAlias` retained
       * because `isDelegationChild` was still set (e.g. 2s post-detach grace).
       * Without this, orphan rescue re-binds the dead broker as a viewer and
       * the terminal child never proceeds to owner `acpConnect`.
       */
      const dropReleasedHandoffBrokerEntry = (
        brokerConnectionId: string
      ): void => {
        const conn = storeRef.current.connections.get(brokerConnectionId)
        if (!conn) return
        // Only non-owning leftovers; never tear down a true local owner.
        if (!conn.isViewer && !conn.isDelegationChild) return
        // Other tabs may still observe the same canonical connection.
        if (aliasKeysFor(brokerConnectionId).length > 0) return
        teardownAttachSubscription(brokerConnectionId)
        reverseMapRef.current.delete(conn.connectionId)
        pendingUnmappedEventsRef.current.delete(conn.connectionId)
        lastActivityRef.current.delete(brokerConnectionId)
        if (conn.connectionId !== brokerConnectionId) {
          lastActivityRef.current.delete(conn.connectionId)
        }
        clearAliasesPointingTo(brokerConnectionId)
        if (conn.connectionId !== brokerConnectionId) {
          clearAliasesPointingTo(conn.connectionId)
        }
        dispatch({
          type: "CONNECTION_REMOVED",
          contextKey: brokerConnectionId,
        })
      }

      let configuredAgent: AcpAgentStatus | null = null
      try {
        // Web and remote-desktop roots are broker-owned. A non-null event
        // stream is the transport capability boundary; pure Tauri continues
        // through the legacy owner/viewer path below.
        if (
          intent === "own_or_observe" &&
          getEventStream() !== null &&
          (!getTransport().isDesktop() || isRemoteDesktopMode())
        ) {
          const nextWorkingDir = workingDir ?? null
          const existing = storeRef.current.connections.get(contextKey)
          if (
            existing &&
            existing.sharedSession &&
            existing.agentType === agentType &&
            existing.workingDir === nextWorkingDir &&
            existing.status !== "disconnected" &&
            existing.status !== "error"
          ) {
            return
          }
          if (existing) {
            teardownAttachSubscription(contextKey)
            if (existing.sharedSession) {
              acpReleaseLease(
                existing.connectionId,
                existing.sharedSession.generation,
                existing.sharedSession.leaseId
              ).catch(() => {})
            }
            dispatch({ type: "CONNECTION_REMOVED", contextKey })
          }
          const prefs = getSavedPrefsForConnect(agentType)
          const identity = getSharedClientIdentity()
          const attachWith = (connectRequest: ConnectRequest) =>
            acpConnectOrAttach({
              conversationId: conversationId ?? null,
              agentType,
              workingDir: nextWorkingDir,
              externalSessionId: sessionId ?? null,
              delegationRouteOverride: delegationRouteOverride ?? null,
              preferredModeId: prefs.modeId,
              preferredConfigValues: prefs.configValues,
              deviceId: identity.deviceId,
              clientInstanceId: identity.clientInstanceId,
              requestId: connectRequest.sharedRequestId ?? newSharedRequestId(),
              retryFailedGeneration:
                connectRequest.retryFailedGeneration ?? null,
            })
          let activeRequest = request
          let response
          try {
            response = await attachWith(activeRequest)
          } catch (error) {
            if (!isSharedSessionConfigConflict(error)) throw error
            activeRequest = {
              ...activeRequest,
              sharedRequestId: newSharedRequestId(),
            }
            lastConnectParamsRef.current.set(contextKey, activeRequest)
            response = await attachWith(activeRequest)
          }
          if (isConnectAbandonedOrSuperseded(contextKey, activeRequest)) {
            acpReleaseLease(
              response.connectionId,
              response.generation,
              response.leaseId
            ).catch(() => {})
            return
          }
          lastActivityRef.current.set(contextKey, Date.now())
          dispatch({
            type: "CONNECTION_CREATED",
            contextKey,
            connectionId: response.connectionId,
            agentType,
            workingDir: nextWorkingDir,
            conversationId: conversationId ?? null,
            delegationRouteOverride: delegationRouteOverride ?? null,
            isViewer: false,
            sharedSession: {
              generation: response.generation,
              leaseId: response.leaseId,
              leaseExpiresAt: response.leaseExpiresAt,
              connectRequestId:
                activeRequest.sharedRequestId ?? newSharedRequestId(),
              phase: sharedPhaseFromResponse(response),
              queue: [],
              activeTurn: null,
            },
          })
          const sameGeneration =
            response.generation === activeRequest.sharedReconnect?.generation
          const attachSinceSeq = sameGeneration
            ? activeRequest.sharedReconnect?.sinceSeq
            : undefined
          setupAttachSubscription(
            contextKey,
            response.connectionId,
            attachSinceSeq,
            sameGeneration ? "resume" : "cold",
            { generation: response.generation, leaseId: response.leaseId }
          )
          if (
            activeRequest.retryFailedGeneration != null ||
            activeRequest.sharedReconnect != null
          ) {
            lastConnectParamsRef.current.set(contextKey, {
              ...activeRequest,
              retryFailedGeneration: undefined,
              sharedReconnect: undefined,
            })
          }
          return
        }

        // ── observe_existing: bounded discovery only; never preflight/spawn ──
        if (intent === "observe_existing") {
          const direct = storeRef.current.connections.get(contextKey)
          if (direct && !observerAliasesRef.current.has(contextKey)) {
            // Re-locking a locally owned connection changes access, not ownership.
            return
          }
          if (conversationId == null || conversationId <= 0) return

          const delays = retryObserverDiscovery
            ? OBSERVER_DISCOVERY_DELAYS_MS
            : OBSERVER_DISCOVERY_DELAYS_MS.slice(0, 1)
          for (const delay of delays) {
            if (!(await waitObserverDelay(contextKey, delay))) return
            if (abandonedKeysRef.current.has(contextKey)) return
            const queued = pendingConnectRequestsRef.current.get(contextKey)
            if (queued && !sameConnectRequest(queued, request)) return

            let discovered: ConversationConnectionInfo | null = null
            try {
              discovered = await acpFindConnectionForConversation(
                conversationId,
                sessionId,
                agentType
              )
            } catch (error) {
              console.warn("[acp-context] observer discovery failed", error)
              // Classify: transport/timeout/5xx → retryable (continue).
              // Auth, not-found permanent, malformed, explicit unrecoverable → stop.
              // Never fall through to acpConnect from this branch.
              if (!isRetryableObserverDiscoveryError(error)) {
                return
              }
              continue
            }
            if (isConnectAbandonedOrSuperseded(contextKey, request)) return
            // null = not found yet (retryable when more delays remain).
            if (discovered == null) continue
            // Malformed payload: fail closed (never attach on garbage identity).
            if (!isValidConversationConnectionInfo(discovered)) {
              console.warn(
                "[acp-context] observer discovery returned malformed payload",
                discovered
              )
              return
            }
            await connectAsViewer(
              contextKey,
              discovered.connection_id,
              agentType,
              workingDir ?? null,
              conversationId,
              "cold"
            )
            return
          }
          return
        }

        // ── own_or_observe: release alias, wait for broker settle, then spawn ──
        const releasedObserverId =
          observerAliasesRef.current.get(contextKey) ?? null
        if (releasedObserverId) releaseObserverAlias(contextKey)
        // When handoff confirms broker null, never orphan-rescue reattach this
        // tab to that known-dead broker — even if another observer alias keeps
        // the store entry alive (dropReleasedHandoffBrokerEntry early-returns).
        let skipOrphanReattachTo: string | null = null

        if (
          releasedObserverId &&
          conversationId != null &&
          conversationId > 0
        ) {
          let oldStillAlive = true
          for (const delay of OBSERVER_DISCOVERY_DELAYS_MS) {
            if (!(await waitObserverDelay(contextKey, delay))) return
            if (abandonedKeysRef.current.has(contextKey)) return
            const queuedBeforeLookup =
              pendingConnectRequestsRef.current.get(contextKey)
            if (
              queuedBeforeLookup &&
              !sameConnectRequest(queuedBeforeLookup, request)
            ) {
              return
            }
            let found: ConversationConnectionInfo | null = null
            try {
              found = await acpFindConnectionForConversation(
                conversationId,
                sessionId,
                agentType
              )
            } catch (error) {
              // Same classification as observe_existing: do NOT treat errors as
              // confirmed disappearance (that would spawn a second ACP while the
              // broker may still be alive).
              console.warn("[acp-context] handoff discovery failed", error)
              if (!isRetryableObserverDiscoveryError(error)) {
                // reattachHandoffObserver re-checks abandoned/supersession.
                await reattachHandoffObserver(
                  contextKey,
                  releasedObserverId,
                  agentType,
                  workingDir ?? null,
                  conversationId,
                  request
                )
                return
              }
              continue
            }
            if (isConnectAbandonedOrSuperseded(contextKey, request)) return
            if (found == null) {
              oldStillAlive = false
              break
            }
            // Malformed discovery: fail closed — reattach observer, never spawn.
            if (!isValidConversationConnectionInfo(found)) {
              console.warn(
                "[acp-context] handoff discovery returned malformed payload",
                found
              )
              await reattachHandoffObserver(
                contextKey,
                releasedObserverId,
                agentType,
                workingDir ?? null,
                conversationId,
                request
              )
              return
            }
            if (found.connection_id !== releasedObserverId) {
              // Different live owner appeared: attach to it, still register
              // watcher so a subsequent disappearance retries own_or_observe.
              await reattachHandoffObserver(
                contextKey,
                found.connection_id,
                agentType,
                workingDir ?? null,
                conversationId,
                request
              )
              return
            }
          }
          if (oldStillAlive) {
            await reattachHandoffObserver(
              contextKey,
              releasedObserverId,
              agentType,
              workingDir ?? null,
              conversationId,
              request
            )
            return
          }
          // Confirmed-null: broker ACP is gone. Clear any stale
          // isDelegationChild/viewer entry retained through grace so the
          // own_or_observe path can spawn instead of orphan-rescue reattach.
          // When another tab still aliases the same id, drop is a no-op — mark
          // skipOrphanReattachTo so orphan rescue / post-null discovery cannot
          // re-bind this releasing tab to the dead broker without a watcher.
          dropReleasedHandoffBrokerEntry(releasedObserverId)
          skipOrphanReattachTo = releasedObserverId
        }

        // Preflight: read agent status and block if the SDK / binary is
        // not installed. The session page must never trigger a download
        // or install — if the agent is not ready, prompt the user to
        // install it from Agent Settings instead.
        try {
          configuredAgent = await acpGetAgentStatus(agentType)
        } catch (error) {
          const reason = t("unableReadAgentConfig", {
            message: normalizeErrorMessage(error),
          })
          const failedTitle = t("connectFailedTitle", {
            agent: getAgentLabel(agentType),
          })
          pushAlertRef.current(
            "error",
            failedTitle,
            `${reason}\n${t("agentsSetupHint")}`,
            [buildOpenAgentsSettingsAction(agentType)]
          )
          throw createAlertedError(reason)
        }

        const blocked = resolveConnectBlockState(configuredAgent)
        if (blocked.kind !== "none") {
          const failedTitle = t("connectFailedTitle", {
            agent: getAgentLabel(agentType),
          })
          const detail =
            blocked.kind === "sdk_missing"
              ? t("withSetupHint", {
                  message: blocked.reason,
                  hint: t("agentsSetupHint"),
                })
              : `${blocked.reason}\n${t("agentsSetupHint")}`
          pushAlertRef.current(
            "error",
            blocked.kind === "sdk_missing" ? blocked.reason : failedTitle,
            detail,
            [buildOpenAgentsSettingsAction(agentType)]
          )
          throw createAlertedError(blocked.reason)
        }

        const nextWorkingDir = workingDir ?? null
        const existingKey = canonicalKey(contextKey)
        const existing = storeRef.current.connections.get(existingKey)
        if (existing) {
          if (
            existing.agentType === agentType &&
            existing.workingDir === nextWorkingDir &&
            existing.status !== "disconnected" &&
            existing.status !== "error"
          ) {
            // Ensure tab alias still points at the live canonical entry
            // (e.g. remount with the same viewer params).
            if (
              existing.isViewer &&
              observerAliasesRef.current.get(contextKey) !==
                existing.connectionId
            ) {
              observerAliasesRef.current.set(contextKey, existing.connectionId)
            }
            return
          }
          if (
            existing.status !== "disconnected" &&
            existing.status !== "error"
          ) {
            // A viewer doesn't own the backend connection — detach only, never
            // acpDisconnect (that would kill the owner's agent). Owners are
            // disconnected normally before re-spawning under new params.
            // Pop-out / detached suppress: also detach-only (keep agent alive).
            if (observerAliasesRef.current.has(contextKey)) {
              releaseObserverAlias(contextKey)
            } else if (!existing.isViewer) {
              const suppressBare =
                existing.conversationId != null &&
                (isTransferringOut(existing.conversationId) ||
                  isFrontendDisconnectSuppressed(existing.conversationId))
              if (!suppressBare) {
                await acpDisconnect(
                  existing.connectionId,
                  disconnectLeaseWithOrigin(
                    leaseArgsForDisconnect(existing),
                    "connection_superseded"
                  )
                ).catch(() => {})
              }
              reverseMapRef.current.delete(existing.connectionId)
              teardownAttachSubscription(existingKey)
              lastActivityRef.current.delete(existingKey)
              pendingUnmappedEventsRef.current.delete(existing.connectionId)
            } else {
              reverseMapRef.current.delete(existing.connectionId)
              teardownAttachSubscription(existingKey)
              lastActivityRef.current.delete(existingKey)
              pendingUnmappedEventsRef.current.delete(existing.connectionId)
            }
          }
        }

        // Orphan rescue: when no entry exists at this contextKey but an
        // alive connection with the same sessionId exists at another
        // contextKey, rekey instead of creating a fresh backend connection.
        // This handles tab close+reopen for newly-created conversations:
        // the original tab's contextKey (e.g. "new-XXXX") differs from
        // the canonical sidebar-reopen contextKey (e.g. "conv-{folderId}-
        // {agent}-{convId}"), and the orphaned connection holds the
        // in-flight live state (live_message, pending_permission, etc.)
        // that we want to preserve across the remount.
        //
        // Invariant: live ACP state for a backend connectionId must remain
        // addressable by that connectionId. Never rekey viewer / delegation /
        // connectionId-keyed rows onto a tab key — alias the tab instead so
        // a later connectAsViewer reuses the same state + subscription.
        if (!existing && sessionId) {
          let orphanKey: string | null = null
          let orphanConn: ConnectionState | null = null
          for (const [key, conn] of storeRef.current.connections) {
            if (key === contextKey) continue
            if (
              conn.sessionId === sessionId &&
              conn.agentType === agentType &&
              conn.workingDir === nextWorkingDir &&
              conn.status !== "disconnected" &&
              conn.status !== "error"
            ) {
              orphanKey = key
              orphanConn = conn
              break
            }
          }
          if (orphanKey && orphanConn) {
            const staysOnConnectionId =
              orphanConn.isViewer ||
              orphanConn.isDelegationChild ||
              orphanKey === orphanConn.connectionId
            // Confirmed-null handoff: skip rebind to the known-dead broker when
            // another observer alias kept the store entry alive. Fall through
            // to owner spawn instead of attach-without-watcher.
            const skipDeadHandoffBroker =
              skipOrphanReattachTo != null &&
              orphanConn.connectionId === skipOrphanReattachTo
            if (staysOnConnectionId && !skipDeadHandoffBroker) {
              await connectAsViewer(
                contextKey,
                orphanConn.connectionId,
                agentType,
                nextWorkingDir,
                conversationId ?? null
              )
              return
            }
            if (!staysOnConnectionId) {
              reverseMapRef.current.set(orphanConn.connectionId, contextKey)
              const lastActivity = lastActivityRef.current.get(orphanKey)
              lastActivityRef.current.delete(orphanKey)
              lastActivityRef.current.set(
                contextKey,
                lastActivity ?? Date.now()
              )
              if (storeRef.current.activeKey === orphanKey) {
                setActiveKey(contextKey)
              }
              // Migrate any active attach subscription from the orphan key to
              // the new key. The handlers' contextKey was captured by closure
              // at attach time, so a simple Map rename would leave events
              // dispatching to the (now-removed) orphan key. Detach + re-attach
              // with the current cursor is correct: the attach response is
              // either a (possibly empty) replay or a fresh snapshot, both
              // converge on the same state.
              const orphanCursor = orphanConn.lastAppliedSeq
              teardownAttachSubscription(orphanKey)
              // Rekey removes the old store key. Clear any tab aliases that
              // still pointed at the pre-rekey owner key.
              clearAliasesPointingTo(orphanKey)
              if (orphanConn.connectionId !== orphanKey) {
                clearAliasesPointingTo(orphanConn.connectionId)
              }
              dispatch({
                type: "REKEY_CONNECTION",
                fromKey: orphanKey,
                toKey: contextKey,
              })
              setupAttachSubscription(
                contextKey,
                orphanConn.connectionId,
                orphanCursor
              )
              return
            }
            // staysOnConnectionId && skipDeadHandoffBroker: fall through.
          }
        }

        // Cross-client viewer attach. Before spawning a NEW backend agent, ask
        // whether another client already holds a LIVE connection for this
        // persisted conversation; if so, attach to it as a (co-controlling)
        // both clients stream the same in-flight turn (fixes desktop→browser
        // streaming). Only for real persisted conversations (id > 0) — a
        // brand-new conversation has no live owner yet, so we spawn + own.
        // Best-effort: a discovery failure falls through to the owner spawn.
        if (conversationId != null && conversationId > 0) {
          let discovered: ConversationConnectionInfo | null = null
          try {
            // Pass sessionId so discovery can fall back to external_id when the
            // live owner hasn't bound its conversation_id yet (pre-first-prompt
            // window) — without it a second client would reuse the owner's
            // connection as a mis-tagged owner and kill it on tab close. The
            // external_id fallback is matched WITH agentType (external_id is
            // unique only per agent).
            discovered = await acpFindConnectionForConversation(
              conversationId,
              sessionId,
              agentType
            )
          } catch (e) {
            console.warn(
              "[acp-context] connection discovery failed for conversation",
              conversationId,
              e
            )
          }
          // Discovery awaited: re-check the abandon/supersede guards in case a
          // disconnect() or a newer connect() for this key landed meanwhile
          // (mirrors the post-acpConnect guards below). The finally block
          // clears connectingKeys/abandoned, so a bare return is safe.
          if (abandonedKeysRef.current.has(contextKey)) {
            return
          }
          const pendingAfterDiscovery =
            pendingConnectRequestsRef.current.get(contextKey)
          if (
            pendingAfterDiscovery &&
            !sameConnectRequest(pendingAfterDiscovery, request)
          ) {
            return
          }
          if (discovered != null) {
            // Malformed discovery payload: fail closed — fall through to spawn
            // only when identity is well-formed (owner path may create new ACP).
            if (!isValidConversationConnectionInfo(discovered)) {
              console.warn(
                "[acp-context] connection discovery returned malformed payload",
                discovered
              )
            } else if (
              // Confirmed-null handoff already proved this broker gone — do not
              // re-observe it from a stale store/discovery race; spawn/own.
              skipOrphanReattachTo == null ||
              discovered.connection_id !== skipOrphanReattachTo
            ) {
              // Own as interactive owner only when this client holds a non-viewer
              // entry for the connection. A prior observer alias must re-bind as
              // viewer (never demote an owner; never spawn a second agent for a
              // connection we already observe).
              let ownedAsOwner = false
              for (const conn of storeRef.current.connections.values()) {
                if (
                  conn.connectionId === discovered.connection_id &&
                  !conn.isViewer &&
                  !conn.isDelegationChild
                ) {
                  ownedAsOwner = true
                  break
                }
              }
              if (!ownedAsOwner) {
                await connectAsViewer(
                  contextKey,
                  discovered.connection_id,
                  agentType,
                  nextWorkingDir,
                  conversationId ?? null
                )
                return
              }
            }
          }
        }

        // Wait for the legacy global listener to register so Tauri's drain
        // path picks up any events emitted between acpConnect returning
        // and reverseMap.set below. Web/remote use attach which doesn't
        // need this gate, but the wait is a fast no-op once the initial
        // subscribe resolves.
        await waitForListenerReady()
        // Ship the user's saved selector preferences (mode + per-config
        // values, persisted per agentType in localStorage) up to the backend
        // at connect time. The backend applies them on the freshly-attached
        // session before emitting `session_modes` / `session_config_options`,
        // so by the time the frontend sees those events (or a snapshot frame
        // on the Subscribe-with-Snapshot attach), `current_mode_id` and
        // `current_value` already reflect the user's preferences. This
        // eliminates the prior "intercept event → overwrite locally → sync
        // back to agent" path, which fixed new-conversation flow but quietly
        // regressed when the snapshot path replaced the event path on tab
        // re-open (the snapshot frame doesn't carry a `session_modes` event,
        // so the apply-on-event hook never fired).
        const savedPrefs = getSavedPrefsForConnect(agentType)
        const coldOwnerOperationId =
          ownerOperationId && ownerOperationId.trim() !== ""
            ? ownerOperationId.trim()
            : null
        const coldOwnerWindowLabel =
          coldOwnerOperationId && conversationId != null
            ? `conversation-${conversationId}`
            : null
        const coldLease = coldOwnerOperationId
          ? {
              expectedOperationId: coldOwnerOperationId,
              expectedOwnerWindow: coldOwnerWindowLabel,
              expectedOwnershipGeneration: 0 as number | null,
            }
          : null
        let connectionId: string
        try {
          connectionId = await acpConnect(
            agentType,
            workingDir,
            sessionId,
            savedPrefs.modeId,
            savedPrefs.configValues,
            conversationId,
            delegationRouteOverride,
            coldOwnerOperationId
          )
        } catch (spawnErr) {
          // Session route conflict: another live connection already owns this
          // conversation under a different route. Attach as viewer/current —
          // keep its snapshot route; never disconnect except explicit reapply.
          const appErr = extractAppCommandError(spawnErr)
          if (
            appErr?.code === "session_route_conflict" &&
            typeof appErr.detail === "string" &&
            appErr.detail.length > 0
          ) {
            await connectAsViewer(
              contextKey,
              appErr.detail,
              agentType,
              nextWorkingDir,
              conversationId ?? null
            )
            return
          }
          throw spawnErr
        }

        // If disconnect was requested while connect was in flight,
        // tear down immediately instead of registering the connection.
        // Detached suppress / transfer fence: never bare-acpDisconnect.
        const suppressBareSpawn =
          conversationId != null &&
          (isTransferringOut(conversationId) ||
            isFrontendDisconnectSuppressed(conversationId))
        if (abandonedKeysRef.current.delete(contextKey)) {
          // The backend dedups by (agent, cwd, session), so `acpConnect` may
          // have handed back a connection this client already holds under
          // another contextKey. Killing that one would end a turn nobody
          // asked to stop.
          if (!suppressBareSpawn && !isConnectionOwnedLocally(connectionId)) {
            acpDisconnect(
              connectionId,
              disconnectLeaseWithOrigin(coldLease, "abandoned_connect")
            ).catch(() => {})
          }
          return
        }
        const pendingRequest = pendingConnectRequestsRef.current.get(contextKey)
        if (pendingRequest && !sameConnectRequest(pendingRequest, request)) {
          if (!suppressBareSpawn && !isConnectionOwnedLocally(connectionId)) {
            acpDisconnect(
              connectionId,
              disconnectLeaseWithOrigin(coldLease, "abandoned_connect")
            ).catch(() => {})
          }
          return
        }

        lastActivityRef.current.set(contextKey, Date.now())
        dispatch({
          type: "CONNECTION_CREATED",
          contextKey,
          connectionId,
          agentType,
          workingDir: nextWorkingDir,
          conversationId: conversationId ?? null,
          delegationRouteOverride: delegationRouteOverride ?? null,
          ownerOperationId: coldOwnerOperationId,
          ownerWindowLabel: coldOwnerWindowLabel,
          ownershipGeneration: coldOwnerOperationId != null ? 0 : null,
        })

        // Subscribe-with-Snapshot path. When the active transport supports
        // the attach protocol (currently web mode), the per-connection WS
        // stream delivers snapshot + replay + live events atomically — no
        // separate snapshot HTTP fetch, no reverse-map, no unmapped buffer.
        // Returns null on transports without attach support; we fall
        // through to the legacy snapshot+global-listener path below.
        const attachSub = setupAttachSubscription(
          contextKey,
          connectionId,
          undefined
        )
        if (attachSub) {
          // Done — the EventStream handles snapshot, replay, live events,
          // and reconnect entirely in-band over the same WS.
        } else {
          // Legacy path (Tauri desktop, RemoteDesktop): same flow as
          // before Phase 3. Awaits snapshot HTTP first, then registers
          // reverseMap, then drains any envelopes that arrived on the
          // global listener while the snapshot was in flight.
          let snapshotPatch:
            | import("@/lib/snapshot-denormalize").SnapshotPatch
            | null = null
          try {
            const snapshot = await acpGetSessionSnapshot(connectionId)
            if (snapshot) {
              snapshotPatch = denormalizeSnapshot(snapshot)
            }
          } catch (e: unknown) {
            console.warn(
              "[acp-context] snapshot fetch failed for",
              connectionId,
              e
            )
          }

          if (snapshotPatch) {
            dispatch({
              type: "HYDRATE_FROM_SNAPSHOT",
              contextKey,
              patch: snapshotPatch,
            })
            surfaceSnapshotErrorDetailsRef.current(contextKey, snapshotPatch)
            // Recover delegation bindings from the snapshot here too. On
            // Tauri the firehose also delivers the events (so this is an
            // idempotent no-op), but it keeps RemoteDesktop and the legacy
            // path symmetric with the attach path above.
            seedDelegationsFromSnapshot(
              snapshotPatch.connectionId,
              snapshotPatch.activeDelegations,
              snapshotPatch.eventSeq
            )
          }

          reverseMapRef.current.set(connectionId, contextKey)

          const buffered = consumeBufferedEvents(connectionId)
          if (buffered.length > 0) {
            for (const event of buffered) {
              applyMappedEnvelope(contextKey, event)
            }
          }
        }
      } catch (err) {
        const pendingRequest = pendingConnectRequestsRef.current.get(contextKey)
        const superseded =
          pendingRequest != null && !sameConnectRequest(pendingRequest, request)
        if (!superseded && !isAlertedError(err)) {
          // Prefer structured AppCommandError payloads (shell preflight
          // i18n_key) while keeping the legacy SdkNotInstalled string
          // path. Only shell AcpError variants serialize as objects;
          // all other ACP errors remain bare strings.
          const appError = extractAppCommandError(err)
          const message = toLocalizedErrorMessage(
            err,
            t as unknown as (
              key: string,
              params?: Record<string, string | number>
            ) => string
          )
          const agentLabel = getAgentLabel(agentType)
          // Backend safety net: if the agent turned out to be not
          // installed (e.g. the binary was removed between preflight
          // and spawn), surface the same install prompt with a direct
          // "Open Agent Settings" action. Title is localized via the
          // same i18n key the preflight path uses.
          //
          // INVARIANT: `AcpError::SdkNotInstalled` still serializes as a
          // bare string whose payload contains "is not installed". Shell
          // failures serialize as AppCommandError objects with a stable
          // `code` / `i18n_key` instead.
          const sdkMissing =
            appError?.code === "sdk_not_installed" ||
            message.includes("is not installed")
          if (sdkMissing) {
            pushAlertRef.current(
              "error",
              configuredAgent?.is_acp_adapter
                ? t("blocked.adapterMissing", { agent: agentLabel })
                : t("blocked.sdkMissing", { agent: agentLabel }),
              t("agentsSetupHint"),
              [buildOpenAgentsSettingsAction(agentType)]
            )
          } else {
            pushAlertRef.current(
              "error",
              t("connectFailedTitle", { agent: agentLabel }),
              message
            )
          }
        }
        if (!superseded) {
          throw err
        }
      } finally {
        connectingKeysRef.current.delete(contextKey)
        inflightConnectRequestsRef.current.delete(contextKey)
        abandonedKeysRef.current.delete(contextKey)
        const settledWaiters = connectSettledWaitersRef.current.get(contextKey)
        if (settledWaiters) {
          connectSettledWaitersRef.current.delete(contextKey)
          for (const resolveWaiter of settledWaiters) resolveWaiter()
        }
        const pendingRequest = pendingConnectRequestsRef.current.get(contextKey)
        if (pendingRequest) {
          pendingConnectRequestsRef.current.delete(contextKey)
          if (!sameConnectRequest(pendingRequest, request)) {
            queueMicrotask(() => {
              connectRef
                .current?.(
                  contextKey,
                  pendingRequest.agentType,
                  pendingRequest.workingDir,
                  pendingRequest.sessionId,
                  pendingRequest.conversationId,
                  pendingRequest.delegationRouteOverride,
                  pendingRequest.ownerOperationId,
                  pendingRequest.intent,
                  pendingRequest.retryObserverDiscovery
                )
                .catch(() => {})
            })
          }
        }
      }
    },
    [
      aliasKeysFor,
      applyMappedEnvelope,
      buildOpenAgentsSettingsAction,
      canonicalKey,
      clearAliasesPointingTo,
      connectAsViewer,
      consumeBufferedEvents,
      dispatch,
      isConnectionOwnedLocally,
      releaseObserverAlias,
      resolveConnectBlockState,
      seedDelegationsFromSnapshot,
      setActiveKey,
      setupAttachSubscription,
      t,
      teardownAttachSubscription,
      waitForListenerReady,
    ]
  )
  connectRef.current = connect

  const disconnect = useCallback(
    async (
      contextKey: string,
      origin: AcpDisconnectOrigin = "explicit_user"
    ) => {
      pendingConnectRequestsRef.current.delete(contextKey)
      // Cancel in-flight observer discovery delays and handoff re-entry.
      cancelObserverDelay(contextKey)
      clearHandoffWatcher(contextKey)
      // Always mark abandoned when connect is in flight for this key — even if
      // we only release an observer alias below. Mid-lookup discovery can still
      // complete after the delay is cancelled; without abandonedKeys the
      // post-lookup path would re-bind the broker after the tab closed.
      if (connectingKeysRef.current.has(contextKey)) {
        abandonedKeysRef.current.add(contextKey)
      }
      if (observerAliasesRef.current.has(contextKey)) {
        releaseObserverAlias(contextKey)
        return false
      }
      const conn = storeRef.current.connections.get(contextKey)
      if (!conn) {
        // connect() is still in flight with no store entry yet — abandoned
        // already set above when connectingKeys has the key.
        return false
      }
      // Before either branch drops the entry: an explicit teardown is also how
      // a `reconnect` starts, and the session it resumes may only ever have
      // been known to the entry (cold attach hydrates it from the snapshot,
      // never from a replayed `session_started`).
      captureIdentityBeforeRemoval(contextKey)
      if (conn.sharedSession) {
        teardownAttachSubscription(contextKey)
        reverseMapRef.current.delete(conn.connectionId)
        pendingUnmappedEventsRef.current.delete(conn.connectionId)
        lastActivityRef.current.delete(contextKey)
        clearAliasesPointingTo(contextKey)
        if (conn.connectionId !== contextKey) {
          clearAliasesPointingTo(conn.connectionId)
        }
        await acpReleaseLease(
          conn.connectionId,
          conn.sharedSession.generation,
          conn.sharedSession.leaseId
        ).catch(() => {})
        if (origin === "explicit_user") {
          await acpTerminateSharedSession(
            conn.connectionId,
            conn.sharedSession.generation
          )
        }
        dispatch({ type: "CONNECTION_REMOVED", contextKey })
        return true
      }
      if (conn.isViewer) {
        // Viewer teardown: drop our read-only attachment WITHOUT
        // `acpDisconnect` — the backend connection belongs to another client,
        // and disconnecting it would kill the owner's agent mid-turn. Mirrors
        // detachDelegationChild. The owner's own disconnect / the idle sweep
        // governs the connection's real lifetime.
        teardownAttachSubscription(contextKey)
        reverseMapRef.current.delete(conn.connectionId)
        pendingUnmappedEventsRef.current.delete(conn.connectionId)
        lastActivityRef.current.delete(contextKey)
        // Drop any tab aliases that pointed at this canonical entry.
        clearAliasesPointingTo(contextKey)
        dispatch({ type: "CONNECTION_REMOVED", contextKey })
        return true
      }
      // Pop-out transfer / detached suppress: release local UI ownership without
      // killing the agent (transfer fence or pre-commit-ack suppress).
      if (
        conn.conversationId != null &&
        (isTransferringOut(conn.conversationId) ||
          isFrontendDisconnectSuppressed(conn.conversationId))
      ) {
        // Fenced source-tab teardown (e.g. user closes main tab while reverse
        // is still pending): snapshot into releasedForReclaim so late Reversed
        // can full-reclaim a main owner. Without this, reclaim only updates
        // in-place and returns success with zero owners → agent orphan.
        if (
          isTransferringOut(conn.conversationId) &&
          !conn.isDelegationChild &&
          !conn.isViewer
        ) {
          const fence = getTransferFence(conn.conversationId)
          if (fence) {
            const reclaimKey = `${conn.conversationId}:${fence.operationId}`
            const prev = releasedForReclaimRef.current.get(reclaimKey) ?? []
            if (!prev.some((e) => e.contextKey === contextKey)) {
              prev.push({
                contextKey,
                connectionId: conn.connectionId,
                agentType: conn.agentType,
                workingDir: conn.workingDir,
                conversationId: conn.conversationId,
                ownershipGeneration: conn.ownershipGeneration ?? null,
                ownerOperationId: conn.ownerOperationId ?? fence.operationId,
                ownerWindowLabel: conn.ownerWindowLabel ?? "main",
              })
              releasedForReclaimRef.current.set(reclaimKey, prev)
            }
            markMainReleased(conn.conversationId, fence.operationId)
          }
        }
        teardownAttachSubscription(contextKey)
        reverseMapRef.current.delete(conn.connectionId)
        pendingUnmappedEventsRef.current.delete(conn.connectionId)
        lastActivityRef.current.delete(contextKey)
        clearAliasesPointingTo(contextKey)
        if (conn.connectionId !== contextKey) {
          clearAliasesPointingTo(conn.connectionId)
        }
        dispatch({ type: "CONNECTION_REMOVED", contextKey })
        return true
      }
      // A failed backend teardown must not strand the local entry: propagating
      // would leak the attach subscription and leave an entry that makes the
      // next `connect()` take its "already connected" fast path. Release
      // locally either way, and report whether the backend is actually gone.
      let tornDown = true
      try {
        await acpDisconnect(
          conn.connectionId,
          disconnectLeaseWithOrigin(leaseArgsForDisconnect(conn), origin)
        )
      } catch (error: unknown) {
        if (!isConnectionGoneError(error)) {
          console.warn(
            "[Acp] backend teardown failed, releasing locally:",
            error
          )
          tornDown = false
        }
      }
      reverseMapRef.current.delete(conn.connectionId)
      teardownAttachSubscription(contextKey)
      lastActivityRef.current.delete(contextKey)
      pendingUnmappedEventsRef.current.delete(conn.connectionId)
      // Owner teardown under the canonical key may still have tab aliases
      // from a prior observer bind that re-bound ownership; clear them.
      clearAliasesPointingTo(contextKey)
      // Also clear aliases that pointed at the backend connectionId when the
      // owner was stored under a different context key.
      if (conn.connectionId !== contextKey) {
        clearAliasesPointingTo(conn.connectionId)
      }
      dispatch({ type: "CONNECTION_REMOVED", contextKey })
      return tornDown
    },
    [
      clearAliasesPointingTo,
      dispatch,
      releaseObserverAlias,
      teardownAttachSubscription,
    ]
  )

  const reapplyConfig = useCallback(
    async (contextKey: string): Promise<boolean> => {
      const conn = storeRef.current.connections.get(contextKey)
      // Viewers / delegation children don't own the process, and shared roots
      // need a coordinated broker recreate transition rather than reattach.
      // The banner hides restart for all three, but guard here too so callers
      // cannot report a false "applied" confirmation for a no-op.
      if (
        !conn ||
        conn.isViewer ||
        conn.isDelegationChild ||
        conn.sharedSession
      )
        return false
      // Capture identity BEFORE teardown. `sessionId` is what makes the new
      // process resume this conversation (session/load) rather than start fresh.
      // conversationId + route override are also reused so reconnect keeps the
      // same route plan inputs the user last chose.
      const {
        agentType,
        workingDir,
        sessionId,
        conversationId: boundConversationId,
        delegationRouteOverride: boundRouteOverride,
        ownerOperationId: boundOwnerOperationId,
      } = conn
      const tornDown = await disconnect(contextKey, "config_reapply")
      await connect(
        contextKey,
        agentType,
        workingDir ?? undefined,
        sessionId ?? undefined,
        boundConversationId ?? undefined,
        boundRouteOverride ?? undefined,
        boundOwnerOperationId
      )
      // Reconnect regardless — the user is left with a working connection
      // either way — but an unconfirmed teardown means the old process may
      // still be alive and holding the OLD config, and `connect()` can land
      // right back on it. Returning false keeps the caller from showing an
      // "applied" confirmation it can't stand behind.
      return tornDown
    },
    [connect, disconnect]
  )

  // Params a reconnect would use: the LIVE connection wins (it carries what the
  // backend actually resolved — notably a sessionId minted after connect), with
  // the remembered request filling in what the store doesn't hold
  // (conversationId, and everything at all once the entry is gone).
  const resolveReconnectRequest = useCallback(
    (contextKey: string): ConnectRequest | null => {
      const conn = storeRef.current.connections.get(canonicalKey(contextKey))
      // Broker-owned: its lifetime is the parent's delegation_started /
      // _completed pair, and disconnecting would kill a child the user never
      // spawned. Bail before falling back to any remembered params.
      if (conn?.isDelegationChild) return null
      const remembered = lastConnectParamsRef.current.get(contextKey)
      const agentType = conn?.agentType ?? remembered?.agentType
      if (!agentType) return null
      return {
        agentType,
        workingDir: conn?.workingDir ?? remembered?.workingDir ?? undefined,
        sessionId: conn?.sessionId ?? remembered?.sessionId ?? undefined,
        conversationId: conn?.conversationId ?? remembered?.conversationId,
        delegationRouteOverride: remembered?.delegationRouteOverride,
        ownerOperationId: remembered?.ownerOperationId,
        sharedRequestId: remembered?.sharedRequestId,
        retryFailedGeneration: remembered?.retryFailedGeneration,
        sharedReconnect: remembered?.sharedReconnect,
        intent: remembered?.intent ?? "own_or_observe",
        retryObserverDiscovery: remembered?.retryObserverDiscovery ?? false,
      }
    },
    [canonicalKey]
  )

  const getReconnectInfo = useCallback(
    (contextKey: string) => {
      const request = resolveReconnectRequest(contextKey)
      if (!request) return null
      return {
        agentType: request.agentType,
        workingDir: request.workingDir ?? null,
        sessionId: request.sessionId ?? null,
      }
    },
    [resolveReconnectRequest]
  )

  // Settle-point for an in-flight connect() on a key. Resolves `true` once that
  // connect finishes (immediately when nothing is connecting), `false` if it
  // has not answered within the timeout — a connect whose IPC never settles
  // must not hold the caller forever.
  const waitForConnectSettled = useCallback(
    (contextKey: string): Promise<boolean> => {
      if (!connectingKeysRef.current.has(contextKey))
        return Promise.resolve(true)
      return new Promise<boolean>((resolve) => {
        const onSettled = () => {
          clearTimeout(timer)
          resolve(true)
        }
        const timer = setTimeout(() => {
          // Drop our resolver so the abandoned wait can't accumulate on a key
          // that keeps failing to settle — including the key itself once the
          // last waiter gives up, since a connect that never answers would
          // otherwise leave the empty list behind for good.
          const waiters = connectSettledWaitersRef.current.get(contextKey)
          const at = waiters?.indexOf(onSettled) ?? -1
          if (waiters && at >= 0) waiters.splice(at, 1)
          if (waiters?.length === 0) {
            connectSettledWaitersRef.current.delete(contextKey)
          }
          resolve(false)
        }, CONNECT_SETTLE_WAIT_TIMEOUT_MS)
        const waiters = connectSettledWaitersRef.current.get(contextKey)
        if (waiters) waiters.push(onSettled)
        else connectSettledWaitersRef.current.set(contextKey, [onSettled])
      })
    },
    []
  )

  const reconnect = useCallback(
    async (contextKey: string): Promise<boolean> => {
      // A connect() already in flight would SWALLOW this one: connect() parks a
      // same-parameter request as pending and its `finally` discards it as a
      // duplicate, so the button would spin once and change nothing — with no
      // store entry yet, the teardown below wouldn't run either. Wait for the
      // attempt to settle and then rebuild: the user asked for a new
      // connection, not to join whatever is already running (they typically
      // click precisely BECAUSE the connecting state is stuck).
      //
      // Bounded rather than looped-to-clear: a key that keeps reconnecting on
      // its own must not hang the button forever, and one more contending
      // connect is what connectingKeysRef already exists to serialise.
      //
      // Each wait is also time-bounded, because the connect this one is stuck
      // behind may never answer at all. Rebuilding anyway would be worse than
      // useless — connect() would park it as a duplicate and drop it — so give
      // up and hand the button back instead of spinning on a wedged IPC.
      for (let i = 0; i < MAX_RECONNECT_SETTLE_WAITS; i++) {
        if (!connectingKeysRef.current.has(contextKey)) break
        if (!(await waitForConnectSettled(contextKey))) return false
      }
      // Resolved AFTER the wait: the connect we just waited on may have minted
      // the sessionId that makes this a resume rather than a fresh session.
      let request = resolveReconnectRequest(contextKey)
      if (!request) return false
      const shared = storeRef.current.connections.get(contextKey)?.sharedSession
      if (shared?.phase.phase === "failed") {
        if (!shared.phase.cleanupComplete) return false
        request = {
          ...request,
          sharedRequestId: newSharedRequestId(),
          retryFailedGeneration: shared.generation,
        }
        lastConnectParamsRef.current.set(contextKey, request)
      }
      // Tear down first even though connect() would: its "same params, still
      // alive → no-op" fast path would otherwise swallow the whole thing, and
      // this button exists precisely to rebuild a connection whose params did
      // NOT change. `disconnect` detaches viewers without killing the owner's
      // agent, so this stays safe for them too.
      //
      // An unconfirmed teardown is deliberately NOT fatal here: the local entry
      // is released either way, and refusing to reconnect would strand the user
      // on the dead connection this button exists to replace.
      // Viewers live under the backend connectionId with the tab as an
      // alias, so `connections.has(tabKey)` is false. Still detach first:
      // otherwise `connect()` treats the leftover alias as a broker handoff
      // and waits out the discovery delay ladder for the owner to die.
      if (
        storeRef.current.connections.has(contextKey) ||
        observerAliasesRef.current.has(contextKey)
      ) {
        await disconnect(contextKey, "connection_superseded")
      }
      await connect(
        contextKey,
        request.agentType,
        request.workingDir,
        request.sessionId,
        request.conversationId
      )
      return true
    },
    [connect, disconnect, resolveReconnectRequest, waitForConnectSettled]
  )

  const dismissConfigStale = useCallback(
    (contextKey: string) => {
      dispatch({ type: "DISMISS_CONFIG_STALE", contextKey })
    },
    [dispatch]
  )

  const dismissSessionFailuresAction = useCallback(
    (contextKey: string, ids: string[]) => {
      dispatch({
        type: "DISMISS_SESSION_FAILURES",
        contextKey: canonicalKey(contextKey),
        ids,
      })
    },
    [canonicalKey, dispatch]
  )

  const disconnectAll = useCallback(async () => {
    const promises: Promise<void>[] = []
    pendingConnectRequestsRef.current.clear()
    // Abort any in-flight observer/handoff settle so disconnectAll cannot leave
    // delayed reconnects firing after the store is cleared.
    cancelAllObserverDelays()
    clearAllHandoffWatchers()
    inflightConnectRequestsRef.current.clear()
    for (const contextKey of connectingKeysRef.current) {
      abandonedKeysRef.current.add(contextKey)
    }
    for (const [contextKey, conn] of storeRef.current.connections) {
      // Viewers attach to a connection another client owns — detach our
      // read-only subscription but never acpDisconnect (that would kill the
      // owner's agent). Owners are torn down normally unless pop-out suppress
      // / transfer fence is active (detached keepalive). Delegation children
      // also attach without owning the process — detach only.
      if (conn.sharedSession) {
        promises.push(
          acpReleaseLease(
            conn.connectionId,
            conn.sharedSession.generation,
            conn.sharedSession.leaseId
          ).catch(() => {})
        )
      } else if (!conn.isViewer && !conn.isDelegationChild) {
        const suppressBare =
          conn.conversationId != null &&
          (isTransferringOut(conn.conversationId) ||
            isFrontendDisconnectSuppressed(conn.conversationId))
        if (!suppressBare) {
          promises.push(
            acpDisconnect(
              conn.connectionId,
              disconnectLeaseWithOrigin(
                leaseArgsForDisconnect(conn),
                "disconnect_all"
              )
            ).catch(() => {})
          )
        }
      }
      reverseMapRef.current.delete(conn.connectionId)
      teardownAttachSubscription(contextKey)
      pendingUnmappedEventsRef.current.delete(conn.connectionId)
    }
    observerAliasesRef.current.clear()
    lastActivityRef.current.clear()
    // Context keys are reused across backends, so a surviving entry here would
    // suppress the first snapshot alert of an unrelated session.
    alertedErrorDetailsRef.current.clear()
    // Same reuse hazard: remembered connect params must not let a reconnect
    // resurrect the previous backend's session under a recycled key.
    lastConnectParamsRef.current.clear()
    await Promise.all(promises)
    dispatch({ type: "REMOVE_ALL" })
  }, [dispatch, teardownAttachSubscription])

  const sendPrompt = useCallback(
    async (
      contextKey: string,
      blocks: PromptInputBlock[],
      opts?: {
        folderId?: number | null
        conversationId?: number | null
        clientMessageId?: string | null
        promptContext?: AcpPromptContext
      }
    ) => {
      if (desktopDeliveryFailedRef.current || readDesktopDeliveryFailed()) {
        throw new Error(
          "Desktop ACP delivery failed; restart the application before sending"
        )
      }
      if (
        getTransport().isDesktop() &&
        getEventStream() === null &&
        listenerPhaseRef.current !== "ready"
      ) {
        throw new Error(
          "Desktop ACP event listener is not ready; cannot send prompt"
        )
      }
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) {
        throw Object.assign(new Error(`connection not found: ${contextKey}`), {
          code: CONNECTION_NOT_FOUND_CODE,
        })
      }
      lastActivityRef.current.set(key, Date.now())
      const promptContext = opts?.promptContext ?? {
        visibleText: null,
        locale: null,
      }
      // Begin optimistic sidebar activity only once a real connection exists
      // and immediately before the ACP wire call. Explicit prompt conversationId
      // wins over the connection-bound id; rollback the exact token on reject.
      const activity = beginRootConversationActivity(conn, opts?.conversationId)
      try {
        const shared = conn.sharedSession
        const identity = shared ? getSharedClientIdentity() : null
        const result = shared
          ? await acpPrompt(
              conn.connectionId,
              blocks,
              opts?.folderId ?? null,
              opts?.conversationId ?? null,
              opts?.clientMessageId ?? null,
              promptContext,
              {
                generation: shared.generation,
                leaseId: shared.leaseId,
                clientInstanceId: identity!.clientInstanceId,
                // Prompt admission has an idempotency domain separate from
                // connect-or-attach. The optimistic message id is stable for
                // a retry of one logical prompt, while a new message gets a
                // distinct request id.
                clientRequestId: opts?.clientMessageId ?? newSharedRequestId(),
              }
            )
          : await acpPrompt(
              conn.connectionId,
              blocks,
              opts?.folderId ?? null,
              opts?.conversationId ?? null,
              opts?.clientMessageId ?? null,
              promptContext
            )
        return result
      } catch (e) {
        rollbackRootConversationActivity(activity)
        throw e
      }
    },
    [canonicalKey]
  )

  const setMode = useCallback(
    async (contextKey: string, modeId: string) => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) return
      // Persist user's mode selection to localStorage
      const modes =
        conn.modes ?? selectorsCache.get(conn.agentType)?.modes ?? null
      if (modes) {
        saveModePreference(conn.agentType, {
          ...modes,
          current_mode_id: modeId,
        })
      }
      lastActivityRef.current.set(key, Date.now())
      const shared = conn.sharedSession
      if (shared) {
        await acpSetMode(conn.connectionId, modeId, {
          generation: shared.generation,
          leaseId: shared.leaseId,
        })
      } else {
        await acpSetMode(conn.connectionId, modeId)
      }
    },
    [canonicalKey]
  )

  const setConfigOption = useCallback(
    async (contextKey: string, configId: string, valueId: string) => {
      // Host-hidden options (e.g. Codex fast-mode) must not be toggled or
      // persisted even if an older agent package still advertises them.
      if (isHiddenSessionConfigOptionId(configId)) return
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) return
      dispatch({
        type: "CONFIG_OPTION_CHANGED",
        contextKey: key,
        configId,
        valueId,
      })
      // Persist user selection to localStorage so the next `acp_connect`
      // can ship it back to the backend as a preferred config value.
      saveConfigPreference(conn.agentType, configId, valueId)
      lastActivityRef.current.set(key, Date.now())
      const shared = conn.sharedSession
      if (shared) {
        await acpSetConfigOption(conn.connectionId, configId, valueId, {
          generation: shared.generation,
          leaseId: shared.leaseId,
        })
      } else {
        await acpSetConfigOption(conn.connectionId, configId, valueId)
      }
    },
    [canonicalKey, dispatch]
  )

  const convergeSharedMutation = useCallback(
    (contextKey: string, conn: ConnectionState, error: unknown): boolean => {
      const shared = conn.sharedSession
      if (!shared || !isSharedInteractionConvergenceError(error)) return false
      const current = storeRef.current.connections.get(contextKey)
      if (
        current?.connectionId !== conn.connectionId ||
        current.sharedSession?.generation !== shared.generation
      ) {
        return true
      }
      teardownAttachSubscription(contextKey)
      setupAttachSubscription(
        contextKey,
        conn.connectionId,
        undefined,
        "cold",
        { generation: shared.generation, leaseId: shared.leaseId }
      )
      return true
    },
    [setupAttachSubscription, teardownAttachSubscription]
  )

  const cancel = useCallback(
    async (contextKey: string) => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) return
      // Snapshot cancelled-turn ownership before turn_complete may arrive late.
      // Prefer runtime session key via external session id (draft virtual id);
      // conn.conversationId is often the positive DB id and is only a fallback.
      const conversationId =
        (conn.sessionId
          ? getConversationIdByExternalIdFromStore(conn.sessionId)
          : null) ??
        conn.conversationId ??
        null
      if (conversationId != null) {
        noteUserStopTurnOwnership(conversationId)
      }
      const shared = conn.sharedSession
      if (shared) {
        const turnId = shared.activeTurn?.turnId
        if (!turnId) return
        try {
          await acpCancel(conn.connectionId, {
            generation: shared.generation,
            leaseId: shared.leaseId,
            turnId,
          })
        } catch (error) {
          if (!convergeSharedMutation(key, conn, error)) throw error
        }
        return
      }
      await acpCancel(conn.connectionId)
    },
    [canonicalKey, convergeSharedMutation]
  )

  const cancelQueuedPrompt = useCallback(
    async (contextKey: string, queueItemId: string) => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      const shared = conn?.sharedSession
      if (!conn || !shared) return
      try {
        await acpCancelQueuedPrompt(conn.connectionId, queueItemId, {
          generation: shared.generation,
          leaseId: shared.leaseId,
        })
      } catch (error) {
        if (!convergeSharedMutation(key, conn, error)) throw error
      }
    },
    [canonicalKey, convergeSharedMutation]
  )

  const dismissFailedSharedPrompt = useCallback(
    (contextKey: string, queueItemId: string) => {
      dispatch({
        type: "DISMISS_FAILED_SHARED_PROMPT",
        contextKey: canonicalKey(contextKey),
        queueItemId,
      })
    },
    [canonicalKey, dispatch]
  )

  const goalControl = useCallback(
    async (contextKey: string, action: "pause" | "clear") => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) return
      // Fire-and-forget: there is no in-flight card UI to settle (unlike
      // answerQuestion). The resulting goal snapshot arrives as a normal
      // session_info_update, and a wire failure is surfaced by the backend's
      // recoverable Error event — so log here and don't rethrow.
      try {
        lastActivityRef.current.set(key, Date.now())
        const shared = conn.sharedSession
        if (shared) {
          await acpGoalControl(conn.connectionId, action, {
            generation: shared.generation,
            leaseId: shared.leaseId,
          })
        } else {
          await acpGoalControl(conn.connectionId, action)
        }
      } catch (e) {
        console.error("[AcpConnections] goalControl failed:", e)
      }
    },
    [canonicalKey]
  )

  const respondPermission = useCallback(
    async (contextKey: string, requestId: string, optionId: string) => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) {
        console.error(
          "[AcpConnections] respondPermission: no connection for",
          contextKey
        )
        return
      }
      try {
        lastActivityRef.current.set(key, Date.now())
        const shared = conn.sharedSession
        if (shared) {
          await acpRespondPermission(conn.connectionId, requestId, optionId, {
            generation: shared.generation,
            leaseId: shared.leaseId,
          })
        } else {
          await acpRespondPermission(conn.connectionId, requestId, optionId)
        }
        dispatch({ type: "PERMISSION_CLEARED", contextKey: key, requestId })
      } catch (e) {
        if (convergeSharedMutation(key, conn, e)) return
        console.error("[AcpConnections] respondPermission failed:", e)
        throw e
      }
    },
    [canonicalKey, convergeSharedMutation, dispatch]
  )

  const answerQuestion = useCallback(
    async (contextKey: string, questionId: string, answer: QuestionAnswer) => {
      const key = canonicalKey(contextKey)
      const conn = storeRef.current.connections.get(key)
      if (!conn) {
        // Throw, don't silently return: AskQuestionCard awaits this and holds a
        // disabled in-flight state (spinner) until it resolves, only re-enabling
        // on rejection. A silent resolve here would leave the card stuck. The
        // throw routes to the card's retryable inline error instead.
        throw new Error(
          `[AcpConnections] answerQuestion: no connection for ${contextKey}`
        )
      }
      // Root structured answers promote sidebar activity; delegation children
      // stay excluded (no conversation id / isDelegationChild guard).
      const activity = beginRootConversationActivity(conn)
      try {
        lastActivityRef.current.set(key, Date.now())
        const shared = conn.sharedSession
        if (shared) {
          await acpAnswerQuestion(conn.connectionId, questionId, answer, {
            generation: shared.generation,
            leaseId: shared.leaseId,
          })
        } else {
          await acpAnswerQuestion(conn.connectionId, questionId, answer)
        }
        // Optimistically clear; the backend also broadcasts question_resolved
        // (idempotent on the matched id).
        dispatch({ type: "CLEAR_ASK_QUESTION", contextKey: key, questionId })
      } catch (e) {
        if (convergeSharedMutation(key, conn, e)) return
        rollbackRootConversationActivity(activity)
        console.error("[AcpConnections] answerQuestion failed:", e)
        throw e
      }
    },
    [canonicalKey, convergeSharedMutation, dispatch]
  )

  const answerPlanApproval = useCallback(
    async (
      contextKey: string,
      approvalId: string,
      answer: PlanApprovalAnswer
    ) => {
      const conn = storeRef.current.connections.get(contextKey)
      if (!conn) {
        // Throw, don't silently return: PlanApprovalCard awaits this and holds a
        // disabled in-flight state until it resolves. A silent resolve would
        // leave the card stuck; the throw routes to its retryable inline error.
        throw new Error(
          `[AcpConnections] answerPlanApproval: no connection for ${contextKey}`
        )
      }
      try {
        lastActivityRef.current.set(contextKey, Date.now())
        const shared = conn.sharedSession
        if (shared) {
          await acpAnswerPlanApproval(conn.connectionId, approvalId, answer, {
            generation: shared.generation,
            leaseId: shared.leaseId,
          })
        } else {
          await acpAnswerPlanApproval(conn.connectionId, approvalId, answer)
        }
        // Optimistically clear; the backend also broadcasts
        // plan_approval_resolved (idempotent on the matched id).
        dispatch({ type: "CLEAR_PLAN_APPROVAL", contextKey, approvalId })
      } catch (e) {
        if (convergeSharedMutation(contextKey, conn, e)) return
        console.error("[AcpConnections] answerPlanApproval failed:", e)
        throw e
      }
    },
    [convergeSharedMutation, dispatch]
  )

  const attachDelegationChild = useCallback(
    (args: {
      connectionId: string
      parentConnectionId: string
      parentToolUseId: string
      agentType: AgentType
      hydrate?: boolean
    }) => {
      const {
        connectionId,
        parentConnectionId,
        parentToolUseId,
        agentType,
        hydrate,
      } = args
      const existing = storeRef.current.connections.get(connectionId)
      if (
        existing &&
        existing.isDelegationChild &&
        existing.connectionId === connectionId &&
        existing.parentConnectionId === parentConnectionId &&
        existing.parentToolUseId === parentToolUseId
      ) {
        // Already attached with the same metadata; just refresh activity so
        // the idle sweep doesn't trip on a duplicate delegation_started.
        lastActivityRef.current.set(connectionId, Date.now())
        return
      }
      dispatch({
        type: "DELEGATION_CHILD_ATTACH",
        contextKey: connectionId,
        connectionId,
        agentType,
        parentConnectionId,
        parentToolUseId,
      })
      lastActivityRef.current.set(connectionId, Date.now())

      const stream = getEventStream()
      if (stream) {
        // One attach subscription per backend connectionId. A viewer
        // discovery that already opened the stream is reused when a later
        // delegation_started event enriches the same canonical entry.
        if (!attachSubscriptionsRef.current.has(connectionId)) {
          setupAttachSubscription(connectionId, connectionId, undefined)
        }
        return
      }

      // Tauri desktop: the global acp://event listener routes by
      // reverseMap. Register the identity mapping and drain any
      // envelopes that arrived between the child's spawn and now.
      const route = () => {
        reverseMapRef.current.set(connectionId, connectionId)
        for (const env of consumeBufferedEvents(connectionId)) {
          applyMappedEnvelope(connectionId, env)
        }
      }
      if (!hydrate) {
        route()
        return
      }
      // Mid-turn attach: the firehose carries only future events, so backfill
      // the turn already in flight from a snapshot FIRST, then route (same
      // order as `connectAsViewer` — anything that lands while the fetch is in
      // flight stays in the unmapped buffer and is deduped by seq on drain).
      void (async () => {
        let patch: import("@/lib/snapshot-denormalize").SnapshotPatch | null =
          null
        try {
          const snapshot = await acpGetSessionSnapshot(connectionId)
          if (snapshot) patch = denormalizeSnapshot(snapshot)
        } catch (e) {
          console.warn(
            "[acp-context] child snapshot fetch failed for",
            connectionId,
            e
          )
        }
        // The viewer may have closed while the snapshot was in flight —
        // never hydrate or install routing for a detached child.
        const still = storeRef.current.connections.get(connectionId)
        if (!still?.isDelegationChild || still.connectionId !== connectionId) {
          return
        }
        if (patch) {
          dispatch({
            type: "HYDRATE_FROM_SNAPSHOT",
            contextKey: connectionId,
            patch,
          })
          // Same recovery the other three snapshot consumers do
          // (`setupAttachSubscription.onSnapshot`, `connectAsViewer`,
          // `connect()`'s legacy branch): `delegation_started` is transient and
          // never replayed, so a viewer opening onto a turn that ALREADY
          // delegated (the work-task transcript dialog is the case) would
          // otherwise establish no binding — no agent icon/label, no child
          // sub-stream, no "待批准" badge on the sub-agent card. Idempotent
          // against any live event for the same `parent_tool_use_id`.
          seedDelegationsFromSnapshot(
            patch.connectionId,
            patch.activeDelegations,
            patch.eventSeq
          )
        }
        route()
      })()
    },
    [
      applyMappedEnvelope,
      consumeBufferedEvents,
      dispatch,
      seedDelegationsFromSnapshot,
      setupAttachSubscription,
    ]
  )

  const detachDelegationChild = useCallback(
    (connectionId: string) => {
      const existing = storeRef.current.connections.get(connectionId)
      if (!existing || !existing.isDelegationChild) return
      const retainObserver = aliasKeysFor(connectionId).length > 0
      if (retainObserver) {
        // Open tab aliases still need the canonical attach; only clear
        // delegation parent fields in place.
        dispatch({
          type: "DELEGATION_CHILD_DETACH",
          contextKey: connectionId,
          retainObserver: true,
        })
        return
      }
      teardownAttachSubscription(connectionId)
      reverseMapRef.current.delete(connectionId)
      pendingUnmappedEventsRef.current.delete(connectionId)
      lastActivityRef.current.delete(connectionId)
      dispatch({ type: "DELEGATION_CHILD_DETACH", contextKey: connectionId })
    },
    [aliasKeysFor, dispatch, teardownAttachSubscription]
  )

  const disconnectIfIdle = useCallback(
    async (contextKey: string) => {
      const conn = storeRef.current.connections.get(contextKey)
      if (
        conn &&
        !conn.isViewer &&
        !conn.sharedSession &&
        isConnectionBusy(conn)
      )
        return
      await disconnect(
        contextKey,
        conn?.sharedSession ? "idle_timeout" : "explicit_user"
      )
    },
    [disconnect]
  )

  const actions = useMemo<AcpActionsValue>(
    () => ({
      connect,
      disconnect,
      disconnectIfIdle,
      disconnectAll,
      sendPrompt,
      setMode,
      setConfigOption,
      cancel,
      cancelQueuedPrompt,
      dismissFailedSharedPrompt,
      goalControl,
      respondPermission,
      answerQuestion,
      answerPlanApproval,
      setActiveKey,
      touchActivity,
      registerOpenTabKeys,
      registerLiveSinks,
      registerLiveMessageSink,
      clearAcpLoadError,
      attachDelegationChild,
      detachDelegationChild,
      reapplyConfig,
      reconnect,
      retryAttach,
      getReconnectInfo,
      dismissConfigStale,
      dismissSessionFailures: dismissSessionFailuresAction,
    }),
    [
      connect,
      disconnect,
      disconnectIfIdle,
      disconnectAll,
      sendPrompt,
      setMode,
      setConfigOption,
      cancel,
      cancelQueuedPrompt,
      dismissFailedSharedPrompt,
      goalControl,
      respondPermission,
      answerQuestion,
      answerPlanApproval,
      setActiveKey,
      touchActivity,
      registerOpenTabKeys,
      registerLiveSinks,
      registerLiveMessageSink,
      clearAcpLoadError,
      attachDelegationChild,
      detachDelegationChild,
      reapplyConfig,
      reconnect,
      retryAttach,
      getReconnectInfo,
      dismissConfigStale,
      dismissSessionFailuresAction,
    ]
  )

  const eventSubscriberApi = useMemo<AcpEventSubscriberApi>(
    () => ({ subscribers: eventSubscribersRef.current }),
    []
  )

  // Install `window.__codegStreamingPerf` only when the test-utils replay
  // command is available. Removed on provider unmount; never persists content.
  useEffect(() => {
    let cancelled = false

    const waitTwoRafs = () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => {
          requestAnimationFrame(() => resolve())
        })
      })

    const buildEnvironment = async (): Promise<
      StreamingPerfReport["environment"]
    > => {
      let hardwareAcceleration: "enabled" | "disabled" | "unknown" = "unknown"
      try {
        const settings = await getSystemRenderingSettings()
        hardwareAcceleration = settings.disable_hardware_acceleration
          ? "disabled"
          : "enabled"
      } catch {
        hardwareAcceleration = "unknown"
      }
      const userAgent =
        typeof navigator !== "undefined" ? navigator.userAgent : "unknown"
      const caps = getStreamingPerformanceConfig()
      const deliveryMode: "legacy" | "batched" =
        caps?.mode === "batched" ? "batched" : "legacy"
      const flags = caps?.flags ?? legacyStreamingPerformanceFlags()
      return {
        platform:
          typeof navigator !== "undefined" ? navigator.platform : "unknown",
        userAgent,
        webviewVersion: extractWebviewVersion(userAgent),
        buildMode:
          process.env.NODE_ENV === "production" ? "production" : "development",
        hardwareAcceleration,
        deliveryMode,
        flags: {
          desktop_acp_event_batching: flags.desktop_acp_event_batching,
          incremental_live_transcript: flags.incremental_live_transcript,
          deferred_streaming_rich_content:
            flags.deferred_streaming_rich_content,
        },
      }
    }

    const isReplayCommandMissing = (error: unknown): boolean => {
      const msg = error instanceof Error ? error.message : String(error)
      // Domain errors from the registered command (e.g. empty connectionId →
      // "connection not found") must NOT be treated as a missing command; the
      // probe intentionally uses an empty id so a domain error means "present".
      if (/connection not found/i.test(msg)) {
        return false
      }
      return /not found|unknown command|does not exist|not available|no such command|Command.*not found/i.test(
        msg
      )
    }

    const installHarness = () => {
      if (cancelled || typeof window === "undefined") return

      window.__codegStreamingPerf = {
        debugState() {
          return {
            activeKey: storeRef.current.activeKey,
            connections: Array.from(storeRef.current.connections.entries()).map(
              ([key, value]) => ({
                key,
                connectionId: value.connectionId,
                status: value.status,
                agentType: value.agentType,
              })
            ),
          }
        },
        async ensureConnected(options?: {
          agentType?: string
          workingDir?: string
          conversationId?: number
          contextKey?: string
        }) {
          const agentType = (options?.agentType ??
            "grok") as import("@/lib/types").AgentType
          const workingDir = options?.workingDir ?? "D:\\MyCodeBuddy"
          const contextKey =
            options?.contextKey ??
            `perf-${agentType}-${options?.conversationId ?? "chat"}`
          const existing = storeRef.current.connections.get(contextKey)
          if (
            existing?.connectionId &&
            existing.status !== "error" &&
            existing.status !== "disconnected"
          ) {
            setActiveKey(contextKey)
            return {
              contextKey,
              connectionId: existing.connectionId,
              status: existing.status,
            }
          }
          await connect(
            contextKey,
            agentType,
            workingDir,
            undefined,
            options?.conversationId
          )
          setActiveKey(contextKey)
          const conn = storeRef.current.connections.get(contextKey)
          if (!conn?.connectionId) {
            throw new Error(
              `streaming perf: ensureConnected failed for ${contextKey}`
            )
          }
          return {
            contextKey,
            connectionId: conn.connectionId,
            status: conn.status,
          }
        },
        async run(options) {
          const rateProfile = options.rateProfile as PerfRateProfile
          const seed = options.seed ?? 0xc0de
          const activeKey = storeRef.current.activeKey
          if (!activeKey) {
            throw new Error(
              "streaming perf: no active connection (setActiveKey first)"
            )
          }
          const conn = storeRef.current.connections.get(activeKey)
          if (!conn?.connectionId) {
            throw new Error(
              "streaming perf: active connection has no connectionId"
            )
          }

          const metricsBefore = await acpGetEventMetrics()
          const environment = await buildEnvironment()

          streamingPerfRecorder.start({
            seed,
            rateProfile,
            expectedEvents: GROK_RICH_V1_EXPECTED_EVENTS,
            expectedTextSha256: GROK_RICH_V1_EXPECTED_TEXT_SHA256,
            targetConnectionId: conn.connectionId,
          })
          streamingPerfRecorder.setEnvironment(environment)

          try {
            const result = await acpReplayStreamingPerfFixture(
              conn.connectionId,
              {
                fixture_id: "grok_rich_v1",
                seed,
                rate_profile: rateProfile,
              }
            )

            await streamingPerfRecorder.waitForQuiet()
            await waitTwoRafs()

            const metricsAfter = await acpGetEventMetrics()
            // Frontend-committed counts for the target connection only.
            // Text digest from final canonical liveMessage (not raw deltas).
            const appliedEvents =
              streamingPerfRecorder.getFrontendAcceptedEvents()
            const finalConn = storeRef.current.connections.get(activeKey)
            const finalText = extractLiveAssistantText(finalConn?.liveMessage)
            streamingPerfRecorder.setFrontendText(finalText)
            const finalTextSha256 =
              await streamingPerfRecorder.computeFrontendTextSha256()
            const integrityOk =
              appliedEvents === GROK_RICH_V1_EXPECTED_EVENTS &&
              finalTextSha256 === GROK_RICH_V1_EXPECTED_TEXT_SHA256 &&
              result.event_count === GROK_RICH_V1_EXPECTED_EVENTS

            streamingPerfRecorder.setDeliverySnapshot(metricsAfter)
            streamingPerfRecorder.setIntegrity({
              expectedEvents: GROK_RICH_V1_EXPECTED_EVENTS,
              appliedEvents,
              firstSeq: appliedEvents > 0 ? 1 : 0,
              lastSeq: result.final_event_seq,
              // gap/duplicate come from recorder marks during the run
              finalTextSha256,
              ok: integrityOk,
            })

            // Silence unused metricsBefore (kept for delivery delta diagnostics).
            void metricsBefore

            const report = streamingPerfRecorder.buildReport({
              delivery: metricsAfter,
              environment,
            })

            if (options.download) {
              downloadStreamingPerfReport(report)
            }
            return report
          } finally {
            streamingPerfRecorder.stop()
          }
        },
      }
    }

    void (async () => {
      try {
        // Probe: empty connectionId. If the command is registered we get a
        // domain error; if it is absent we get a missing-command error.
        await acpReplayStreamingPerfFixture("", {
          fixture_id: "grok_rich_v1",
          seed: 0,
          rate_profile: "eps_100",
        })
        if (!cancelled) installHarness()
      } catch (error) {
        if (cancelled) return
        if (!isReplayCommandMissing(error)) {
          installHarness()
        }
      }
    })()

    return () => {
      cancelled = true
      if (typeof window !== "undefined") {
        delete window.__codegStreamingPerf
      }
    }
  }, [])

  return (
    <AcpActionsContext.Provider value={actions}>
      <ConnectionStoreContext.Provider value={storeApi}>
        <AcpEventSubscriberContext.Provider value={eventSubscriberApi}>
          {children}
        </AcpEventSubscriberContext.Provider>
      </ConnectionStoreContext.Provider>
    </AcpActionsContext.Provider>
  )
}
