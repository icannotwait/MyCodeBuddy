/**
 * Per-conversation workflow graph snapshot store.
 *
 * - Manifest mode: discard stale applies by `graph_revision` (higher wins).
 * - Observed-only / compatibility_nudge: local request generation so an older
 *   in-flight fetch cannot overwrite a newer nudge result.
 * - Cold detail installs via `applyFromDetail`; live clock via graph-changed
 *   + snapshot refetch; compatibility_nudge triggers the same refetch path.
 * - Authority refresh: active workflow state and discovered active-surface
 *   overlay interest use 15 seconds. Expanded-only and undiscovered interest
 *   use a 10-minute fallback.
 */

import { create } from "zustand"
import {
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
  subscribeWorkflowCompatibilityNudge,
  subscribeWorkflowGraphChanged,
} from "@/lib/api"
import type { CompletionDecisionResolvedEventPayload } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import {
  getWorkspaceStateStore,
  type WorkspaceStateStore,
  type WorkspaceWatchToken,
} from "@/hooks/use-workspace-state-store"
import { joinRootRel, normalizeAbsPath } from "@/lib/file-open-target"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"
import type {
  DelegationRuntimeStats,
  WorkflowGateSnapshot,
  WorkflowGraphSnapshot,
  WorkflowNodeSnapshot,
  WorkflowPhaseSnapshot,
} from "@/lib/types"

export type WorkflowGraphChangedPayload = {
  parent_conversation_id: number
  workflow_id: string
  graph_revision: number
}

export type WorkflowCompatibilityNudgePayload = {
  parent_conversation_id: number
}

export type WorkflowSegment = "workflow" | "sessions"

export type PhaseRailKind = "design" | "plan" | "tasks" | "final"

export type PhaseRailStatus =
  | "completed"
  | "current"
  | "blocked"
  | "pending"
  | "estimated"

export type PhaseRailItem = {
  kind: PhaseRailKind
  id: string | null
  title: string | null
  status: PhaseRailStatus
  /** B11: required reviewers only (from gate snapshot). */
  gate: {
    returned: number
    required: number
    running: number
    blocked: number
  } | null
  taskProgress: { current: number; total: number } | null
  nodeRows: WorkflowNodeRow[]
}

export type WorkflowNodeRow = {
  id: string
  taskIndex: number | null
  nodes: WorkflowNodeSnapshot[]
  reviewerProgress: { returned: number; required: number } | null
}

type ConversationGraphEntry = {
  snapshot: WorkflowGraphSnapshot | null
  /** Last applied durable graph_revision; null for observed-only / unknown. */
  appliedGraphRevision: number | null
  /**
   * Local request generation for observed-only / nudge races and for
   * invalidating in-flight fetches when a newer signal arrives.
   */
  requestGeneration: number
  inFlightGeneration: number | null
  loading: boolean
  error: string | null
}

type InterestMode = "overlay" | "expanded"

type ActiveConversationRecord = {
  overlayCount: number
  expandedCount: number
  epoch: number
  readinessSettled: boolean
  fallbackTimer: ReturnType<typeof setTimeout> | null
}

type RefreshApplyOutcome =
  | "applied"
  | "newer_revision"
  | "soft_absent"
  | "failed"
  | "stale_generation"

type WorkflowGraphState = {
  byConversationId: Map<number, ConversationGraphEntry>
  /**
   * Install / update from conversation detail. `undefined` means the field was
   * absent (treat as no graph). Always authoritative for cold load relative to
   * an empty entry; still revision-gated against a newer live snapshot.
   */
  applyFromDetail: (
    conversationId: number,
    graph: WorkflowGraphSnapshot | null | undefined
  ) => void
  /** Project latest delegation runtime fields onto matching cached nodes. */
  applyRuntimeStats: (taskId: string, stats: DelegationRuntimeStats) => void
  /** Apply a fetched snapshot with optional request-generation gate. */
  applyFetchedSnapshot: (
    conversationId: number,
    snapshot: WorkflowGraphSnapshot | null,
    requestGeneration: number
  ) => RefreshApplyOutcome
  handleGraphChanged: (payload: WorkflowGraphChangedPayload) => void
  handleCompletionDecisionResolved: (
    payload: CompletionDecisionResolvedEventPayload
  ) => void
  handleCompatibilityNudge: (payload: WorkflowCompatibilityNudgePayload) => void
  /** Acquire open-overlay interest and return an idempotent cleanup lease. */
  activateOverlayInterest: (conversationId: number) => () => void
  /** Acquire live workflow interest and return an idempotent cleanup lease. */
  activateConversation: (conversationId: number) => () => void
  /** Watch the exact Simple Plan/progress files through a paths-only lease. */
  activateSimpleFileInterest: (
    conversationId: number,
    workspaceRootPath: string,
    planRelPath: string,
    progressRelPath: string
  ) => () => void
  refresh: (conversationId: number) => Promise<void>
  getEntry: (conversationId: number) => ConversationGraphEntry | undefined
  getSnapshot: (conversationId: number) => WorkflowGraphSnapshot | null
  clearConversation: (conversationId: number) => void
  /** Test / backend-reset helper. */
  reset: () => void
}

const FIXED_PHASES: PhaseRailKind[] = ["design", "plan", "tasks", "final"]
const EVENT_READINESS_TIMEOUT_MS = 5_000
const ACTIVE_AUTHORITY_REFRESH_MS = 15_000
const FALLBACK_REFRESH_MS = 10 * 60 * 1_000
const SIMPLE_FILE_REFRESH_DEBOUNCE_MS = 150
const SOFT_ABSENCE_ERROR = "Workflow graph snapshot unavailable"

function emptyEntry(): ConversationGraphEntry {
  return {
    snapshot: null,
    appliedGraphRevision: null,
    requestGeneration: 0,
    inFlightGeneration: null,
    loading: false,
    error: null,
  }
}

function revisionOf(snapshot: WorkflowGraphSnapshot | null): number | null {
  if (!snapshot) return null
  if (snapshot.graph_revision == null) return null
  return Number(snapshot.graph_revision)
}

/**
 * Whether `incoming` should replace `current` under the graph_revision clock.
 * Observed-only (null revision) always replaces when the apply is generation-
 * gated by the caller; here null vs number: a numbered revision wins over null
 * only when applying a numbered snapshot. Null transport responses are
 * classified separately so an existing graph can be retained as soft-absent.
 */
function isStaleByRevision(
  currentRev: number | null,
  incomingRev: number | null
): boolean {
  if (currentRev == null) return false
  if (incomingRev == null) return true
  return incomingRev < currentRev
}

function mapSetEntry(
  map: Map<number, ConversationGraphEntry>,
  conversationId: number,
  entry: ConversationGraphEntry
): Map<number, ConversationGraphEntry> {
  const next = new Map(map)
  next.set(conversationId, entry)
  return next
}

/**
 * Per-install generation token for event subscriptions.
 *
 * React Strict Mode (mount → unmount → remount) can leave in-flight
 * `subscribe().then(dispose => …)` callbacks from the first install. A shared
 * boolean `eventDisposed` that remount flips back to false lets those stale
 * callbacks overwrite the active listener slots with the wrong dispose
 * handles. Each install captures its generation; stale `.then` handlers
 * always dispose-and-drop instead of assigning into the live slots.
 */
let eventInstallGeneration = 0
/** Non-zero while an install is active (matches that install's generation). */
let activeEventInstallGeneration = 0
const REQUIRED_LISTENER_RETRY_MS = 5_000
const REQUIRED_LISTENER_KEYS = ["graphChanged", "compatibilityNudge"] as const

type RequiredListenerKey = (typeof REQUIRED_LISTENER_KEYS)[number]

type RequiredListenerSlot = {
  subscribed: boolean
  subscribing: boolean
  warningEmitted: boolean
  unsubscribe: (() => void) | null
}

const requiredListenerSlots: Record<RequiredListenerKey, RequiredListenerSlot> =
  {
    graphChanged: {
      subscribed: false,
      subscribing: false,
      warningEmitted: false,
      unsubscribe: null,
    },
    compatibilityNudge: {
      subscribed: false,
      subscribing: false,
      warningEmitted: false,
      unsubscribe: null,
    },
  }

let completionUnsub: (() => void) | null = null
let eventReadinessPromise: Promise<void> | null = null
let eventReadinessDeadline: ReturnType<typeof setTimeout> | null = null
let requiredListenerRetryTimer: ReturnType<typeof setTimeout> | null = null
const activeConversations = new Map<number, ActiveConversationRecord>()
const seenCompletionEvents = new Set<string>()
let activationEpochCounter = 0

type SimpleFileInterestRecord = {
  count: number
  workspaceStore: WorkspaceStateStore
  watchToken: WorkspaceWatchToken
  unsubscribe: () => void
  debounceTimer: ReturnType<typeof setTimeout> | null
}

const simpleFileInterests = new Map<string, SimpleFileInterestRecord>()

function normalizeWorkspaceFileForComparison(
  workspaceRootPath: string,
  relPath: string
): string {
  const root = normalizeAbsPath(workspaceRootPath)
  const absolute = joinRootRel(root, relPath)
  const caseInsensitive = /^[a-zA-Z]:\//.test(root) || root.startsWith("//")
  return caseInsensitive ? absolute.toLowerCase() : absolute
}

function activateSimpleFileInterest(
  get: () => WorkflowGraphState,
  conversationId: number,
  workspaceRootPath: string,
  planRelPath: string,
  progressRelPath: string
): () => void {
  if (
    !Number.isSafeInteger(conversationId) ||
    conversationId <= 0 ||
    !workspaceRootPath.trim() ||
    !planRelPath.trim() ||
    !progressRelPath.trim()
  ) {
    return () => {}
  }

  const targets = new Set([
    normalizeWorkspaceFileForComparison(workspaceRootPath, planRelPath),
    normalizeWorkspaceFileForComparison(workspaceRootPath, progressRelPath),
  ])
  const key = JSON.stringify([
    conversationId,
    normalizeWorkspaceFileForComparison(workspaceRootPath, ""),
    ...Array.from(targets).sort(),
  ])
  const existing = simpleFileInterests.get(key)
  if (existing) {
    existing.count += 1
    let released = false
    return () => {
      if (released) return
      released = true
      if (simpleFileInterests.get(key) !== existing) return
      existing.count -= 1
      if (existing.count > 0) return
      if (existing.debounceTimer != null) {
        clearTimeout(existing.debounceTimer)
      }
      existing.unsubscribe()
      existing.workspaceStore.release(existing.watchToken)
      simpleFileInterests.delete(key)
    }
  }

  const workspaceStore = getWorkspaceStateStore(workspaceRootPath)
  const watchToken = workspaceStore.acquire("paths")
  const record: SimpleFileInterestRecord = {
    count: 1,
    workspaceStore,
    watchToken,
    unsubscribe: () => {},
    debounceTimer: null,
  }
  record.unsubscribe = workspaceStore.subscribeEnvelopes(
    ({ changed_paths: changedPaths }) => {
      const relevant =
        changedPaths.length === 0 ||
        changedPaths.some((changedPath) =>
          targets.has(
            normalizeWorkspaceFileForComparison(workspaceRootPath, changedPath)
          )
        )
      if (!relevant) return
      if (record.debounceTimer != null) clearTimeout(record.debounceTimer)
      record.debounceTimer = setTimeout(() => {
        record.debounceTimer = null
        void get().refresh(conversationId)
      }, SIMPLE_FILE_REFRESH_DEBOUNCE_MS)
    }
  )
  simpleFileInterests.set(key, record)

  let released = false
  return () => {
    if (released) return
    released = true
    const current = simpleFileInterests.get(key)
    if (current !== record) return
    record.count -= 1
    if (record.count > 0) return
    if (record.debounceTimer != null) clearTimeout(record.debounceTimer)
    record.unsubscribe()
    workspaceStore.release(watchToken)
    simpleFileInterests.delete(key)
  }
}

function disposeSimpleFileInterests(): void {
  for (const record of simpleFileInterests.values()) {
    if (record.debounceTimer != null) clearTimeout(record.debounceTimer)
    record.unsubscribe()
    record.workspaceStore.release(record.watchToken)
  }
  simpleFileInterests.clear()
}

function totalInterest(active: ActiveConversationRecord): number {
  return active.overlayCount + active.expandedCount
}

function isActiveEpoch(conversationId: number, epoch: number): boolean {
  const active = activeConversations.get(conversationId)
  return active != null && totalInterest(active) > 0 && active.epoch === epoch
}

function hasExpandedInterestEpoch(
  conversationId: number,
  epoch: number
): boolean {
  const active = activeConversations.get(conversationId)
  return active != null && active.epoch === epoch && active.expandedCount > 0
}

function hasOverlayInterestEpoch(
  conversationId: number,
  epoch: number
): boolean {
  const active = activeConversations.get(conversationId)
  return active != null && active.epoch === epoch && active.overlayCount > 0
}

function clearFallbackTimer(active: ActiveConversationRecord): void {
  if (active.fallbackTimer != null) {
    clearTimeout(active.fallbackTimer)
    active.fallbackTimer = null
  }
}

function subscribeRequiredListener(
  key: RequiredListenerKey,
  get: () => WorkflowGraphState
): Promise<() => void> {
  if (key === "graphChanged") {
    return subscribeWorkflowGraphChanged((payload) => {
      get().handleGraphChanged(payload)
    })
  }
  return subscribeWorkflowCompatibilityNudge((payload) => {
    get().handleCompatibilityNudge(payload)
  })
}

function requiredListenerChannel(key: RequiredListenerKey): string {
  return key === "graphChanged"
    ? WORKFLOW_GRAPH_CHANGED_EVENT
    : WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT
}

function scheduleRequiredListenerRetry(
  get: () => WorkflowGraphState,
  generation: number
): void {
  if (
    requiredListenerRetryTimer != null ||
    activeEventInstallGeneration !== generation ||
    activeConversations.size === 0 ||
    !REQUIRED_LISTENER_KEYS.some((key) => {
      const slot = requiredListenerSlots[key]
      return !slot.subscribed && !slot.subscribing
    })
  ) {
    return
  }

  requiredListenerRetryTimer = setTimeout(() => {
    requiredListenerRetryTimer = null
    if (
      activeEventInstallGeneration !== generation ||
      activeConversations.size === 0
    ) {
      return
    }
    for (const attempt of attemptMissingRequiredListeners(get, generation)) {
      void attempt
    }
  }, REQUIRED_LISTENER_RETRY_MS)
}

function attemptRequiredListener(
  key: RequiredListenerKey,
  get: () => WorkflowGraphState,
  generation: number
): Promise<void> {
  const slot = requiredListenerSlots[key]
  if (slot.subscribed || slot.subscribing) return Promise.resolve()
  slot.subscribing = true

  return subscribeRequiredListener(key, get)
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      slot.subscribing = false
      if (slot.subscribed || slot.unsubscribe) {
        dispose()
        return
      }
      slot.subscribed = true
      slot.warningEmitted = false
      slot.unsubscribe = dispose
    })
    .catch((error: unknown) => {
      if (activeEventInstallGeneration !== generation) return
      slot.subscribing = false
      if (!slot.warningEmitted) {
        console.warn(
          "[workflow-graph-store] required event subscription failed",
          {
            channel: requiredListenerChannel(key),
            error: toErrorMessage(error),
          }
        )
        slot.warningEmitted = true
      }
      scheduleRequiredListenerRetry(get, generation)
    })
}

function attemptMissingRequiredListeners(
  get: () => WorkflowGraphState,
  generation: number
): Promise<void>[] {
  return REQUIRED_LISTENER_KEYS.filter((key) => {
    const slot = requiredListenerSlots[key]
    return !slot.subscribed && !slot.subscribing
  }).map((key) => attemptRequiredListener(key, get, generation))
}

function installEventListeners(get: () => WorkflowGraphState): Promise<void> {
  if (activeEventInstallGeneration !== 0 && eventReadinessPromise != null) {
    return eventReadinessPromise
  }
  const generation = ++eventInstallGeneration
  activeEventInstallGeneration = generation

  const requiredAttempts = attemptMissingRequiredListeners(get, generation)
  const completionAttempt = subscribeCompletionDecisionResolved((payload) => {
    get().handleCompletionDecisionResolved(payload)
  })
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      completionUnsub = dispose
    })
    .catch(() => {
      // Older transports converge through graph snapshot refresh.
    })

  const attemptsSettled = Promise.allSettled([
    ...requiredAttempts,
    completionAttempt,
  ]).then(() => undefined)
  const deadline = new Promise<void>((resolve) => {
    eventReadinessDeadline = setTimeout(resolve, EVENT_READINESS_TIMEOUT_MS)
  })
  eventReadinessPromise = Promise.race([attemptsSettled, deadline]).finally(
    () => {
      if (activeEventInstallGeneration !== generation) return
      if (eventReadinessDeadline != null) {
        clearTimeout(eventReadinessDeadline)
        eventReadinessDeadline = null
      }
    }
  )
  return eventReadinessPromise
}

function disposeEventListeners(): void {
  // Clear only the active-generation reference. `eventInstallGeneration` is
  // monotonic forever — never decremented or zeroed — so in-flight `.then`
  // callbacks from a disposed install still see `active !== their generation`.
  activeEventInstallGeneration = 0
  if (eventReadinessDeadline != null) {
    clearTimeout(eventReadinessDeadline)
    eventReadinessDeadline = null
  }
  if (requiredListenerRetryTimer != null) {
    clearTimeout(requiredListenerRetryTimer)
    requiredListenerRetryTimer = null
  }
  eventReadinessPromise = null

  for (const key of REQUIRED_LISTENER_KEYS) {
    const slot = requiredListenerSlots[key]
    slot.unsubscribe?.()
    slot.unsubscribe = null
    slot.subscribed = false
    slot.subscribing = false
    slot.warningEmitted = false
  }

  completionUnsub?.()
  completionUnsub = null
}

function releaseConversation(
  conversationId: number,
  epoch: number,
  mode: InterestMode
): void {
  const active = activeConversations.get(conversationId)
  if (!active || active.epoch !== epoch) return
  if (mode === "overlay") {
    if (active.overlayCount <= 0) return
    active.overlayCount -= 1
  } else {
    if (active.expandedCount <= 0) return
    active.expandedCount -= 1
    // Do not clear the fallback solely because expanded interest dropped:
    // overlay-only discovery still needs the 10-minute safety net while
    // `appliedGraphRevision` is null. The timer callback re-checks need.
  }
  if (totalInterest(active) > 0) return
  clearFallbackTimer(active)
  activationEpochCounter += 1
  activeConversations.delete(conversationId)
  if (activeConversations.size === 0) disposeEventListeners()
}

function activateInterest(
  get: () => WorkflowGraphState,
  conversationId: number,
  mode: InterestMode
): () => void {
  if (!Number.isSafeInteger(conversationId) || conversationId <= 0) {
    return () => {}
  }

  let active = activeConversations.get(conversationId)
  const becameTotalActive = active == null
  const wasExpanded = (active?.expandedCount ?? 0) > 0
  if (!active) {
    active = {
      overlayCount: 0,
      expandedCount: 0,
      epoch: ++activationEpochCounter,
      readinessSettled: false,
      fallbackTimer: null,
    }
    activeConversations.set(conversationId, active)
  }
  if (mode === "overlay") active.overlayCount += 1
  else active.expandedCount += 1
  const epoch = active.epoch

  if (!get().getEntry(conversationId)) {
    useWorkflowGraphStore.setState((state) => ({
      byConversationId: mapSetEntry(
        state.byConversationId,
        conversationId,
        emptyEntry()
      ),
    }))
  }

  if (becameTotalActive) {
    const readiness = installEventListeners(get)
    void readiness.then(() => {
      const currentActive = activeConversations.get(conversationId)
      if (!currentActive || currentActive.epoch !== epoch) return
      currentActive.readinessSettled = true
      void get().refresh(conversationId)
    })
  } else if (mode === "expanded" && !wasExpanded && active.readinessSettled) {
    void get().refresh(conversationId)
  }

  let released = false
  return () => {
    if (released) return
    released = true
    releaseConversation(conversationId, epoch, mode)
  }
}

async function fetchAndApply(
  get: () => WorkflowGraphState,
  conversationId: number,
  requestGeneration: number
): Promise<RefreshApplyOutcome> {
  try {
    const snapshot = await getWorkflowGraphSnapshot(conversationId)
    return get().applyFetchedSnapshot(
      conversationId,
      snapshot,
      requestGeneration
    )
  } catch (err: unknown) {
    const entry = get().getEntry(conversationId) ?? emptyEntry()
    if (entry.requestGeneration !== requestGeneration) {
      return "stale_generation"
    }
    const message =
      err instanceof Error ? err.message : "Failed to load workflow graph"
    useWorkflowGraphStore.setState((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...entry,
        inFlightGeneration: null,
        loading: false,
        error: message,
      }),
    }))
    return "failed"
  }
}

async function runRefresh(
  get: () => WorkflowGraphState,
  conversationId: number,
  activationEpoch: number
): Promise<void> {
  if (!isActiveEpoch(conversationId, activationEpoch)) return
  const active = activeConversations.get(conversationId)
  if (!active) return

  clearFallbackTimer(active)
  const current = get().getEntry(conversationId) ?? emptyEntry()
  const requestGeneration = current.requestGeneration + 1
  useWorkflowGraphStore.setState((state) => ({
    byConversationId: mapSetEntry(state.byConversationId, conversationId, {
      ...current,
      requestGeneration,
      inFlightGeneration: requestGeneration,
      loading: true,
      error: null,
    }),
  }))

  const outcome = await fetchAndApply(get, conversationId, requestGeneration)
  if (outcome === "stale_generation") return

  const completed = get().getEntry(conversationId)
  if (
    completed?.requestGeneration !== requestGeneration ||
    !isActiveEpoch(conversationId, activationEpoch)
  ) {
    return
  }

  const currentActive = activeConversations.get(conversationId)
  if (!currentActive || !isActiveEpoch(conversationId, activationEpoch)) {
    return
  }
  // Active state and discovered active-surface overlay interest use 15 seconds.
  // Expanded-only and undiscovered interest use the 10-minute fallback.
  const delay = nextAuthorityRefreshDelay(conversationId, activationEpoch, get)
  if (delay == null) return

  currentActive.fallbackTimer = setTimeout(() => {
    if (
      nextAuthorityRefreshDelay(conversationId, activationEpoch, get) == null
    ) {
      return
    }
    void get().refresh(conversationId)
  }, delay)
}

function hasActiveWorkflowState(
  snapshot: WorkflowGraphSnapshot | null
): boolean {
  if (!snapshot) return false
  if (snapshot.overall_state === "in_progress") return true
  return snapshot.nodes.some(
    (node) =>
      node.status === "reserving" ||
      node.status === "running" ||
      node.status === "waiting_review" ||
      node.status === "waiting_adjudication"
  )
}

function nextAuthorityRefreshDelay(
  conversationId: number,
  epoch: number,
  get: () => WorkflowGraphState
): number | null {
  if (!isActiveEpoch(conversationId, epoch)) return null
  const entry = get().getEntry(conversationId)
  if (hasActiveWorkflowState(entry?.snapshot ?? null)) {
    return ACTIVE_AUTHORITY_REFRESH_MS
  }
  if (
    entry?.appliedGraphRevision != null &&
    hasOverlayInterestEpoch(conversationId, epoch)
  ) {
    return ACTIVE_AUTHORITY_REFRESH_MS
  }
  if (hasExpandedInterestEpoch(conversationId, epoch)) {
    return FALLBACK_REFRESH_MS
  }
  return entry?.appliedGraphRevision == null ? FALLBACK_REFRESH_MS : null
}

export const useWorkflowGraphStore = create<WorkflowGraphState>((set, get) => ({
  byConversationId: new Map(),

  applyFromDetail: (conversationId, graph) => {
    const snapshot = graph ?? null
    const incomingRev = revisionOf(snapshot)
    const current = get().getEntry(conversationId) ?? emptyEntry()

    // Drop stale detail if live clock is already ahead.
    if (
      snapshot != null &&
      isStaleByRevision(current.appliedGraphRevision, incomingRev)
    ) {
      return
    }
    // Soft-fail / projector omission on detail must not wipe a live numbered
    // graph. Transport absence is classified by applyFetchedSnapshot instead.
    if (snapshot == null && current.snapshot != null) {
      return
    }

    set((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...current,
        snapshot,
        appliedGraphRevision: incomingRev,
        loading: false,
        error: null,
      }),
    }))
  },

  applyRuntimeStats: (taskId, stats) => {
    if (!taskId) return
    set((state) => {
      let changed = false
      const byConversationId = new Map(state.byConversationId)

      for (const [conversationId, entry] of state.byConversationId) {
        const snapshot = entry.snapshot
        if (!snapshot) continue

        let nodesChanged = false
        const nodes = snapshot.nodes.map((item) => {
          if (item.latest_task_id !== taskId) return item
          nodesChanged = true
          return {
            ...item,
            tool_call_count: stats.tool_call_count,
            edit_tool_call_count: stats.edit_tool_call_count,
            touched_file_count: stats.touched_files.length,
            touched_files_truncated: stats.touched_files_truncated,
            additions: stats.additions ?? null,
            deletions: stats.deletions ?? null,
            line_counts_complete: stats.line_counts_complete,
          }
        })

        if (!nodesChanged) continue
        changed = true
        byConversationId.set(conversationId, {
          ...entry,
          snapshot: { ...snapshot, nodes },
        })
      }

      return changed ? { byConversationId } : state
    })
  },

  handleCompletionDecisionResolved: (payload) => {
    const eventKey = `${payload.event_id}:${payload.graph_revision}`
    if (seenCompletionEvents.has(eventKey)) return

    for (const [conversationId, entry] of get().byConversationId) {
      const snapshot = entry.snapshot
      if (
        !snapshot ||
        !activeConversations.has(conversationId) ||
        snapshot.workflow_id !== payload.workflow_id ||
        (entry.appliedGraphRevision ?? -1) >= payload.graph_revision
      ) {
        continue
      }

      const node = snapshot.nodes.find(
        (candidate) => candidate.latest_task_id === payload.task_id
      )
      const attention =
        node?.completion?.card.attention ?? snapshot.completion?.card.attention
      if (
        !attention ||
        attention.task_id !== payload.task_id ||
        attention.kind !== payload.kind ||
        attention.captured_scope_digest !== payload.evidence_scope_digest
      ) {
        continue
      }

      seenCompletionEvents.add(eventKey)
      void get().refresh(conversationId)
    }
  },

  applyFetchedSnapshot: (conversationId, snapshot, requestGeneration) => {
    const current = get().getEntry(conversationId) ?? emptyEntry()
    // Discard if a newer request superseded this one (nudge / graph-changed).
    if (requestGeneration !== current.requestGeneration) {
      return "stale_generation"
    }

    if (snapshot == null && current.snapshot != null) {
      set((state) => ({
        byConversationId: mapSetEntry(state.byConversationId, conversationId, {
          ...current,
          inFlightGeneration: null,
          loading: false,
          error: SOFT_ABSENCE_ERROR,
        }),
      }))
      return "soft_absent"
    }

    const incomingRev = revisionOf(snapshot)
    if (isStaleByRevision(current.appliedGraphRevision, incomingRev)) {
      set((state) => ({
        byConversationId: mapSetEntry(state.byConversationId, conversationId, {
          ...current,
          inFlightGeneration: null,
          loading: false,
          error: null,
        }),
      }))
      return "newer_revision"
    }

    set((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...current,
        snapshot,
        appliedGraphRevision: incomingRev,
        inFlightGeneration: null,
        loading: false,
        error: null,
      }),
    }))
    return "applied"
  },

  handleGraphChanged: (payload) => {
    const conversationId = payload.parent_conversation_id
    if (!activeConversations.has(conversationId)) return
    const current = get().getEntry(conversationId) ?? emptyEntry()
    const eventRev = Number(payload.graph_revision)
    if (
      current.appliedGraphRevision != null &&
      eventRev <= current.appliedGraphRevision
    ) {
      return
    }
    void get().refresh(conversationId)
  },

  handleCompatibilityNudge: (payload) => {
    const conversationId = payload.parent_conversation_id
    if (!activeConversations.has(conversationId)) return
    void get().refresh(conversationId)
  },

  activateOverlayInterest: (conversationId) =>
    activateInterest(get, conversationId, "overlay"),

  activateConversation: (conversationId) =>
    activateInterest(get, conversationId, "expanded"),

  activateSimpleFileInterest: (
    conversationId,
    workspaceRootPath,
    planRelPath,
    progressRelPath
  ) =>
    activateSimpleFileInterest(
      get,
      conversationId,
      workspaceRootPath,
      planRelPath,
      progressRelPath
    ),

  refresh: async (conversationId) => {
    const active = activeConversations.get(conversationId)
    if (!active || totalInterest(active) <= 0) return
    await runRefresh(get, conversationId, active.epoch)
  },

  getEntry: (conversationId) => get().byConversationId.get(conversationId),

  getSnapshot: (conversationId) =>
    get().byConversationId.get(conversationId)?.snapshot ?? null,

  clearConversation: (conversationId) => {
    set((state) => {
      if (!state.byConversationId.has(conversationId)) return state
      const next = new Map(state.byConversationId)
      next.delete(conversationId)
      return { byConversationId: next }
    })
  },

  reset: () => {
    for (const active of activeConversations.values()) {
      clearFallbackTimer(active)
    }
    activeConversations.clear()
    disposeSimpleFileInterests()
    seenCompletionEvents.clear()
    activationEpochCounter += 1
    disposeEventListeners()
    // Do not reset `eventInstallGeneration` — monotonic for process lifetime.
    // disposeEventListeners already clears the active reference only.
    set({ byConversationId: new Map() })
  },
}))

registerBackendScopedStoreReset(() => {
  useWorkflowGraphStore.getState().reset()
})

// ---------------------------------------------------------------------------
// Pure helpers (B11 compact counts, phase rail, openability)
// ---------------------------------------------------------------------------

export function isEstimatedNode(node: WorkflowNodeSnapshot): boolean {
  return (
    node.status === "estimated" ||
    (!node.is_observed && !node.retained_observed)
  )
}

/** Observed nodes with a child conversation are openable via child-tab path. */
export function canOpenWorkflowNode(node: WorkflowNodeSnapshot): boolean {
  if (isEstimatedNode(node)) return false
  const childId = node.latest_child_conversation_id
  return (
    childId != null &&
    childId > 0 &&
    (node.is_observed || node.retained_observed)
  )
}

/**
 * B11: compact gate progress uses gate snapshot fields (required reviewers only).
 * Falls back to counting required nodes when no gate row exists.
 */
export function compactRequiredGateCounts(
  snapshot: WorkflowGraphSnapshot,
  phaseKind: PhaseRailKind
): {
  returned: number
  required: number
  running: number
  blocked: number
} | null {
  const gate = snapshot.gates.find((g) => g.gate_kind === phaseKind)
  if (gate) {
    return {
      returned: gate.returned_count,
      required: gate.required_count,
      running: gate.running_count,
      blocked: gate.blocked_count,
    }
  }

  // Tasks / Final: no document gate — do not invent required denominators from
  // optional-inclusive node lists. Return null so the rail omits gate chrome.
  if (phaseKind === "tasks" || phaseKind === "final") return null

  const phaseNodes = snapshot.nodes.filter(
    (n) =>
      n.phase_id === phaseKind ||
      n.phase_id?.endsWith(phaseKind) ||
      phaseMatches(n.phase_id, snapshot.phases, phaseKind)
  )
  const requiredNodes = phaseNodes.filter((n) => n.required)
  if (requiredNodes.length === 0) return null
  let returned = 0
  let running = 0
  let blocked = 0
  for (const n of requiredNodes) {
    if (n.status === "completed") returned += 1
    else if (n.status === "running" || n.status === "reserving") running += 1
    else if (
      n.status === "blocked" ||
      n.status === "failed" ||
      n.status === "missing_summary"
    ) {
      blocked += 1
    }
  }
  return {
    returned,
    required: requiredNodes.length,
    running,
    blocked,
  }
}

function phaseMatches(
  phaseId: string | null | undefined,
  phases: WorkflowPhaseSnapshot[],
  kind: PhaseRailKind
): boolean {
  if (!phaseId) return false
  if (phaseId === kind) return true
  const phase = phases.find((p) => p.id === phaseId)
  if (!phase) return false
  return phase.kind === kind || phase.id === kind
}

function phaseHasBlocked(nodes: WorkflowNodeSnapshot[]): boolean {
  return nodes.some(
    (n) =>
      n.status === "blocked" ||
      n.status === "failed" ||
      n.status === "missing_summary"
  )
}

function phaseAllTerminalDone(nodes: WorkflowNodeSnapshot[]): boolean {
  if (nodes.length === 0) return false
  return nodes.every(
    (n) =>
      n.status === "completed" ||
      n.status === "superseded" ||
      n.status === "canceled"
  )
}

function phaseHasActive(nodes: WorkflowNodeSnapshot[]): boolean {
  return nodes.some(
    (n) =>
      n.status === "running" ||
      n.status === "reserving" ||
      n.status === "waiting_review" ||
      n.status === "waiting_adjudication"
  )
}

function reviewerProgressForNodes(
  nodes: WorkflowNodeSnapshot[]
): WorkflowNodeRow["reviewerProgress"] {
  const source = nodes.find(
    (node) =>
      node.required_reviewer_count != null &&
      node.returned_reviewer_count != null
  )
  if (!source) return null

  const required = source.required_reviewer_count as number
  const returned = source.returned_reviewer_count as number
  if (
    !Number.isSafeInteger(required) ||
    !Number.isSafeInteger(returned) ||
    required < 0 ||
    returned < 0
  ) {
    return null
  }

  return {
    returned: Math.min(returned, required),
    required,
  }
}

function orderedCohort(
  nodes: WorkflowNodeSnapshot[],
  primaryRole: "author" | "implementer"
): WorkflowNodeSnapshot[] {
  const primary = nodes.filter((node) => node.role === primaryRole)
  const reviewers = nodes.filter((node) => node.role === "reviewer")
  const remaining = nodes.filter(
    (node) => node.role !== primaryRole && node.role !== "reviewer"
  )
  return [...primary, ...reviewers, ...remaining]
}

/** Stable graph rows; source order is the authoritative reviewer policy order. */
export function buildWorkflowNodeRows(
  nodes: WorkflowNodeSnapshot[],
  phaseKind: PhaseRailKind
): WorkflowNodeRow[] {
  if (phaseKind === "tasks") {
    const indexed = new Map<number, WorkflowNodeSnapshot[]>()
    const unindexed: WorkflowNodeSnapshot[] = []

    for (const node of nodes) {
      if (node.task_index == null) {
        unindexed.push(node)
        continue
      }
      const cohort = indexed.get(node.task_index) ?? []
      cohort.push(node)
      indexed.set(node.task_index, cohort)
    }

    const rows = Array.from(indexed.entries())
      .sort(([left], [right]) => left - right)
      .map(([taskIndex, cohort]) => {
        const ordered = orderedCohort(cohort, "implementer")
        return {
          id: `tasks-${taskIndex}`,
          taskIndex,
          nodes: ordered,
          reviewerProgress: reviewerProgressForNodes(ordered),
        }
      })

    return [
      ...rows,
      ...unindexed.map((node) => ({
        id: `tasks-${node.node_id}`,
        taskIndex: null,
        nodes: [node],
        reviewerProgress: null,
      })),
    ]
  }

  if (phaseKind === "plan") {
    const cohort = nodes.filter(
      (node) => node.role === "author" || node.role === "reviewer"
    )
    const remaining = nodes.filter(
      (node) => node.role !== "author" && node.role !== "reviewer"
    )
    const rows: WorkflowNodeRow[] = []
    if (cohort.length > 0) {
      rows.push({
        id: "plan",
        taskIndex: null,
        nodes: orderedCohort(cohort, "author"),
        reviewerProgress: null,
      })
    }
    rows.push(
      ...remaining.map((node) => ({
        id: `plan-${node.node_id}`,
        taskIndex: null,
        nodes: [node],
        reviewerProgress: null,
      }))
    )
    return rows
  }

  return nodes.map((node) => ({
    id: `${phaseKind}-${node.node_id}`,
    taskIndex: null,
    nodes: [node],
    reviewerProgress: null,
  }))
}

export function buildPhaseRail(
  snapshot: WorkflowGraphSnapshot
): PhaseRailItem[] {
  const currentPhase = snapshot.current_phase_id ?? null

  return FIXED_PHASES.map((kind) => {
    const phaseMeta =
      snapshot.phases.find((p) => p.kind === kind || p.id === kind) ?? null
    const phaseId = phaseMeta?.id ?? kind
    const nodes = snapshot.nodes.filter((n) =>
      phaseMatches(n.phase_id, snapshot.phases, kind)
    )

    let status: PhaseRailStatus = "pending"
    const isCurrent =
      currentPhase != null &&
      (currentPhase === phaseId ||
        currentPhase === kind ||
        phaseMatches(currentPhase, snapshot.phases, kind) ||
        snapshot.current_node_ids.some((id) =>
          nodes.some((n) => n.node_id === id)
        ))

    if (phaseHasBlocked(nodes)) status = "blocked"
    else if (isCurrent || phaseHasActive(nodes)) status = "current"
    else if (phaseAllTerminalDone(nodes)) status = "completed"
    else if (nodes.length > 0 && nodes.every((n) => n.status === "estimated")) {
      status = "estimated"
    } else if (nodes.length === 0) {
      // Skeleton: mark current overall phase only.
      if (
        snapshot.overall_state === "skeleton" &&
        (currentPhase === kind || currentPhase === phaseId)
      ) {
        status = "current"
      } else if (
        snapshot.overall_state === "completed" &&
        FIXED_PHASES.indexOf(kind) <=
          FIXED_PHASES.indexOf((currentPhase as PhaseRailKind) ?? "final")
      ) {
        status = "completed"
      } else {
        status = "pending"
      }
    }

    let taskProgress: PhaseRailItem["taskProgress"] = null
    if (kind === "tasks") {
      taskProgress = computeTaskPhaseProgress(nodes, snapshot.current_node_ids)
    }

    return {
      kind,
      id: phaseMeta?.id ?? null,
      title: phaseMeta?.title ?? null,
      status,
      gate: compactRequiredGateCounts(snapshot, kind),
      taskProgress,
      nodeRows: buildWorkflowNodeRows(nodes, kind),
    }
  })
}

function isTaskWorkTerminal(status: WorkflowNodeSnapshot["status"]): boolean {
  return (
    status === "completed" || status === "superseded" || status === "canceled"
  )
}

/**
 * Compact Task position (`Task current / total`).
 *
 * - **total**: implementer work units only (distinct `task_index` when present).
 * - **current**: never jumps ahead while an earlier task's reviewer is still
 *   active. Prefer `min(task_index)` among `current_node_ids` (includes active
 *   reviewers), also considering the earliest incomplete implementer/reviewer
 *   pair. When everything is done, `current === total`.
 */
export function computeTaskPhaseProgress(
  nodes: WorkflowNodeSnapshot[],
  currentNodeIds: readonly string[] = []
): { current: number; total: number } | null {
  const implementers = nodes.filter((n) => n.role === "implementer")
  if (implementers.length === 0) return null

  const indexed = implementers.filter((n) => n.task_index != null)
  const total =
    indexed.length > 0
      ? new Set(indexed.map((n) => n.task_index as number)).size
      : implementers.length
  if (total <= 0) return null

  const byId = new Map(nodes.map((n) => [n.node_id, n]))
  const candidates: number[] = []

  // 1) min task_index from current_node_ids (implementers AND reviewers).
  for (const id of currentNodeIds) {
    const n = byId.get(id)
    if (n?.task_index != null) candidates.push(n.task_index)
  }

  // 2) Earliest incomplete implementer/reviewer pair (and any incomplete
  //    implementer without a reviewer). Keeps position on task N while its
  //    reviewer is still active even if a later implementer is also current.
  const taskIndices = new Set<number>()
  for (const n of implementers) {
    if (n.task_index != null) taskIndices.add(n.task_index)
  }
  const sortedIndices =
    taskIndices.size > 0 ? Array.from(taskIndices).sort((a, b) => a - b) : null

  if (sortedIndices) {
    for (const taskIndex of sortedIndices) {
      if (!isImplementerReviewerPairComplete(nodes, taskIndex)) {
        candidates.push(taskIndex)
      }
    }
  } else {
    // Unindexed implementers: first non-terminal is the current slot (1-based).
    const firstOpen = implementers.findIndex(
      (n) => !isTaskWorkTerminal(n.status)
    )
    if (firstOpen >= 0) candidates.push(firstOpen + 1)
  }

  if (candidates.length === 0) {
    // All pairs complete (or all implementers terminal).
    return { current: total, total }
  }

  const current = Math.min(...candidates)
  // Clamp into [1, total] for display safety.
  return {
    current: Math.min(total, Math.max(1, current)),
    total,
  }
}

/** Task pair is complete only when implementer is terminal and reviewer (if any) is terminal. */
function isImplementerReviewerPairComplete(
  nodes: WorkflowNodeSnapshot[],
  taskIndex: number
): boolean {
  const impl = nodes.find(
    (n) => n.role === "implementer" && n.task_index === taskIndex
  )
  if (!impl || !isTaskWorkTerminal(impl.status)) return false
  const reviewers = nodes.filter(
    (n) => n.role === "reviewer" && n.task_index === taskIndex
  )
  if (reviewers.length === 0) return true
  return reviewers.every((r) => isTaskWorkTerminal(r.status))
}

export function selectCurrentNodes(
  snapshot: WorkflowGraphSnapshot
): WorkflowNodeSnapshot[] {
  if (snapshot.current_node_ids.length === 0) return []
  const set = new Set(snapshot.current_node_ids)
  return snapshot.nodes.filter((n) => set.has(n.node_id))
}

export function findGateForPhase(
  snapshot: WorkflowGraphSnapshot,
  phaseKind: PhaseRailKind
): WorkflowGateSnapshot | undefined {
  return snapshot.gates.find((g) => g.gate_kind === phaseKind)
}

/** Test-only: reset module listener bookkeeping (store.reset covers state). */
export function __resetWorkflowGraphStoreForTests(): void {
  useWorkflowGraphStore.getState().reset()
}

/** Test-only: inspect active event-install generation (0 = disposed). */
export function __getWorkflowGraphEventInstallGenerationForTests(): number {
  return activeEventInstallGeneration
}

/** Test-only: monotonic install counter (never reset). */
export function __getWorkflowGraphEventInstallCounterForTests(): number {
  return eventInstallGeneration
}
