/**
 * Per-conversation workflow graph snapshot store.
 *
 * - Manifest mode: discard stale applies by `graph_revision` (higher wins).
 * - Observed-only / compatibility_nudge: local request generation so an older
 *   in-flight fetch cannot overwrite a newer nudge result.
 * - Cold detail installs via `applyFromDetail`; live clock via graph-changed
 *   + snapshot refetch; compatibility_nudge triggers the same refetch path.
 */

import { create } from "zustand"
import {
  getWorkflowGraphSnapshot,
  subscribeWorkflowCompatibilityNudge,
  subscribeWorkflowGraphChanged,
} from "@/lib/api"
import { registerBackendScopedStoreReset } from "@/stores/backend-scoped-store-reset"
import type {
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
  /** Apply a fetched snapshot with optional request-generation gate. */
  applyFetchedSnapshot: (
    conversationId: number,
    snapshot: WorkflowGraphSnapshot | null,
    requestGeneration: number
  ) => void
  handleGraphChanged: (payload: WorkflowGraphChangedPayload) => void
  handleCompatibilityNudge: (
    payload: WorkflowCompatibilityNudgePayload
  ) => void
  /** Ensure global event listeners + interest in this conversation. */
  mountConversation: (conversationId: number) => () => void
  refresh: (conversationId: number) => Promise<void>
  getEntry: (conversationId: number) => ConversationGraphEntry | undefined
  getSnapshot: (conversationId: number) => WorkflowGraphSnapshot | null
  clearConversation: (conversationId: number) => void
  /** Test / backend-reset helper. */
  reset: () => void
}

const FIXED_PHASES: PhaseRailKind[] = ["design", "plan", "tasks", "final"]

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
 * only when applying a numbered snapshot; a null snapshot clears only if the
 * caller is authoritative (detail clear / successful empty fetch).
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
 * callbacks overwrite `graphChangedUnsub` / `nudgeUnsub` with the wrong
 * dispose handle. Each install captures its generation; dispose bumps the
 * counter so stale `.then` handlers always dispose-and-drop instead of
 * assigning into the live slots.
 */
let eventInstallGeneration = 0
/** Non-zero while an install is active (matches that install's generation). */
let activeEventInstallGeneration = 0
let graphChangedUnsub: (() => void) | null = null
let nudgeUnsub: (() => void) | null = null
const mountedConversations = new Set<number>()

function installEventListeners(get: () => WorkflowGraphState): void {
  if (activeEventInstallGeneration !== 0) return
  const generation = ++eventInstallGeneration
  activeEventInstallGeneration = generation

  void subscribeWorkflowGraphChanged((payload) => {
    get().handleGraphChanged(payload)
  })
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      graphChangedUnsub = dispose
    })
    .catch(() => {
      // Transport without subscribe — refresh-only path.
    })

  void subscribeWorkflowCompatibilityNudge((payload) => {
    get().handleCompatibilityNudge(payload)
  })
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      nudgeUnsub = dispose
    })
    .catch(() => {
      // Transport without subscribe — refresh-only path.
    })
}

function disposeEventListeners(): void {
  // Clear only the active-generation reference. `eventInstallGeneration` is
  // monotonic forever — never decremented or zeroed — so in-flight `.then`
  // callbacks from a disposed install still see `active !== their generation`.
  activeEventInstallGeneration = 0
  graphChangedUnsub?.()
  nudgeUnsub?.()
  graphChangedUnsub = null
  nudgeUnsub = null
}

async function fetchAndApply(
  get: () => WorkflowGraphState,
  conversationId: number,
  requestGeneration: number
): Promise<void> {
  try {
    const snapshot = await getWorkflowGraphSnapshot(conversationId)
    get().applyFetchedSnapshot(conversationId, snapshot, requestGeneration)
  } catch (err: unknown) {
    const entry = get().getEntry(conversationId) ?? emptyEntry()
    if (entry.requestGeneration !== requestGeneration) return
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
  }
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
    // graph. Explicit empty fetches go through applyFetchedSnapshot instead.
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

  applyFetchedSnapshot: (conversationId, snapshot, requestGeneration) => {
    const current = get().getEntry(conversationId) ?? emptyEntry()
    // Discard if a newer request superseded this one (nudge / graph-changed).
    if (requestGeneration !== current.requestGeneration) return

    const incomingRev = revisionOf(snapshot)
    if (isStaleByRevision(current.appliedGraphRevision, incomingRev)) {
      set((state) => ({
        byConversationId: mapSetEntry(state.byConversationId, conversationId, {
          ...current,
          inFlightGeneration: null,
          loading: false,
        }),
      }))
      return
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
  },

  handleGraphChanged: (payload) => {
    const conversationId = payload.parent_conversation_id
    if (!mountedConversations.has(conversationId)) {
      // Still accept if we already have an entry (detail-installed, not yet
      // mounted overlay) so live clock stays warm for open conversations.
      if (!get().byConversationId.has(conversationId)) return
    }
    const current = get().getEntry(conversationId) ?? emptyEntry()
    const eventRev = Number(payload.graph_revision)
    if (
      current.appliedGraphRevision != null &&
      eventRev <= current.appliedGraphRevision
    ) {
      return
    }
    const nextGen = current.requestGeneration + 1
    set((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...current,
        requestGeneration: nextGen,
        inFlightGeneration: nextGen,
        loading: true,
        error: null,
      }),
    }))
    void fetchAndApply(get, conversationId, nextGen)
  },

  handleCompatibilityNudge: (payload) => {
    const conversationId = payload.parent_conversation_id
    if (
      !mountedConversations.has(conversationId) &&
      !get().byConversationId.has(conversationId)
    ) {
      return
    }
    const current = get().getEntry(conversationId) ?? emptyEntry()
    const nextGen = current.requestGeneration + 1
    set((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...current,
        requestGeneration: nextGen,
        inFlightGeneration: nextGen,
        loading: true,
        error: null,
      }),
    }))
    void fetchAndApply(get, conversationId, nextGen)
  },

  mountConversation: (conversationId) => {
    installEventListeners(get)
    mountedConversations.add(conversationId)
    const current = get().getEntry(conversationId)
    if (!current) {
      set((state) => ({
        byConversationId: mapSetEntry(
          state.byConversationId,
          conversationId,
          emptyEntry()
        ),
      }))
    }
    return () => {
      mountedConversations.delete(conversationId)
      if (mountedConversations.size === 0) {
        // Keep entries (detail may re-open); only tear down listeners when idle.
        disposeEventListeners()
      }
    }
  },

  refresh: async (conversationId) => {
    const current = get().getEntry(conversationId) ?? emptyEntry()
    const nextGen = current.requestGeneration + 1
    set((state) => ({
      byConversationId: mapSetEntry(state.byConversationId, conversationId, {
        ...current,
        requestGeneration: nextGen,
        inFlightGeneration: nextGen,
        loading: true,
        error: null,
      }),
    }))
    await fetchAndApply(get, conversationId, nextGen)
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
    mountedConversations.clear()
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
  return node.status === "estimated" || (!node.is_observed && !node.retained_observed)
}

/** Observed nodes with a child conversation are openable via child-tab path. */
export function canOpenWorkflowNode(node: WorkflowNodeSnapshot): boolean {
  if (isEstimatedNode(node)) return false
  const childId = node.latest_child_conversation_id
  return childId != null && childId > 0 && (node.is_observed || node.retained_observed)
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
          FIXED_PHASES.indexOf(
            (currentPhase as PhaseRailKind) ?? "final"
          )
      ) {
        status = "completed"
      } else {
        status = "pending"
      }
    }

    let taskProgress: PhaseRailItem["taskProgress"] = null
    if (kind === "tasks") {
      taskProgress = computeTaskPhaseProgress(
        nodes,
        snapshot.current_node_ids
      )
    }

    return {
      kind,
      id: phaseMeta?.id ?? null,
      title: phaseMeta?.title ?? null,
      status,
      gate: compactRequiredGateCounts(snapshot, kind),
      taskProgress,
    }
  })
}

function isTaskWorkTerminal(status: WorkflowNodeSnapshot["status"]): boolean {
  return (
    status === "completed" ||
    status === "superseded" ||
    status === "canceled"
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
    taskIndices.size > 0
      ? Array.from(taskIndices).sort((a, b) => a - b)
      : null

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
