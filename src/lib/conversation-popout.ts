import {
  abortConversationPopoutOperation,
  closeConversationWindow,
  completeConversationPopoutOperation,
  focusConversationWindow,
  getConversationPopoutOperation,
  openConversationWindow,
  type OpenConversationResult,
  type PopoutOpStatus,
} from "@/lib/api"
import {
  clearTransferringOut,
  getTransferFence,
  markTransferringOut,
  reclaimAfterAbort,
  releaseConnectionWithoutDisconnect,
} from "@/lib/conversation-popout-acp-bridge"
import { isLocalDesktop, subscribe } from "@/lib/platform"
import type { AgentType } from "@/lib/types"
import { useTabStore, type DetachRestoreToken } from "@/stores/tab-store"

export type PopOutEnablement =
  | { enabled: true }
  | { enabled: false; reason: "not_local_desktop" | "draft" | "last_tab" }

const detachedCache = new Set<number>()
const inFlight = new Map<number, Promise<void>>()

type ReadyPayload = {
  conversationId: number
  operationId: string
  connectionId?: string | null
  ownershipGeneration?: number | null
}

type ClosedPayload = {
  conversationId: number
  operationId: string
  abortOutcome?: unknown
}

let closedUnsub: (() => void) | null = null

function ensureClosedListener() {
  if (closedUnsub || typeof window === "undefined") return
  void subscribe<ClosedPayload>("conversation-window://closed", (payload) => {
    if (payload?.conversationId != null) {
      detachedCache.delete(payload.conversationId)
    }
  }).then((unsub) => {
    closedUnsub = unsub
  })
}

export function canPopOutConversation(args: {
  conversationId: number | null | undefined
  isOpenMainTab: boolean
  mainTabCount: number
}): PopOutEnablement {
  if (!isLocalDesktop()) {
    return { enabled: false, reason: "not_local_desktop" }
  }
  if (args.conversationId == null || args.conversationId <= 0) {
    return { enabled: false, reason: "draft" }
  }
  if (args.isOpenMainTab && args.mainTabCount < 2) {
    return { enabled: false, reason: "last_tab" }
  }
  return { enabled: true }
}

export function isConversationDetachedCache(conversationId: number): boolean {
  return detachedCache.has(conversationId)
}

/** True while popOutConversation single-flight holds this conversation. */
export function isPopOutInFlight(conversationId: number): boolean {
  return conversationId > 0 && inFlight.has(conversationId)
}

export async function focusDetachedConversation(
  conversationId: number
): Promise<boolean> {
  if (!isLocalDesktop()) return false
  ensureClosedListener()
  try {
    const focused = await focusConversationWindow(conversationId)
    if (focused) {
      detachedCache.add(conversationId)
      return true
    }
    detachedCache.delete(conversationId)
    return false
  } catch {
    detachedCache.delete(conversationId)
    return false
  }
}

type ArmedWait = {
  ready: Promise<ReadyPayload>
  /** Rejects if the window closes before handoff completes. */
  closed: Promise<never>
  isClosed: () => boolean
  cancel: () => void
  armed: Promise<void>
}

/**
 * Register ready/closed listeners **before** open returns so a fast ready
 * emission cannot race past an incomplete subscribe().
 * Closed stays observed until cancel(); callers must re-check isClosed() after
 * each await (Promise.race alone does not cancel later stages).
 */
function armHandoffWait(
  operationId: string,
  conversationId: number,
  timeoutMs: number
): ArmedWait {
  let settleReady!: (v: ReadyPayload) => void
  let settleClosed!: (e: Error) => void
  let readySettled = false
  let closedFlag = false
  let cancelled = false
  let unsubReady: (() => void) | null = null
  let unsubClosed: (() => void) | null = null
  let timer: ReturnType<typeof setTimeout> | null = null

  const ready = new Promise<ReadyPayload>((resolve) => {
    settleReady = resolve
  })

  const closed = new Promise<never>((_, reject) => {
    settleClosed = reject
  })

  const cancel = () => {
    if (cancelled) return
    cancelled = true
    if (timer) clearTimeout(timer)
    unsubReady?.()
    unsubClosed?.()
  }

  const armed = (async () => {
    unsubReady = await subscribe<ReadyPayload>(
      "conversation-window://ready",
      (payload) => {
        if (
          !readySettled &&
          payload?.operationId === operationId &&
          payload.conversationId === conversationId
        ) {
          readySettled = true
          unsubReady?.()
          unsubReady = null
          settleReady(payload)
        }
      }
    )
    unsubClosed = await subscribe<ClosedPayload>(
      "conversation-window://closed",
      (payload) => {
        if (payload?.operationId === operationId) {
          closedFlag = true
          settleClosed(
            new Error("pop-out window closed before handoff completed")
          )
        }
      }
    )
    timer = setTimeout(() => {
      if (!readySettled) {
        closedFlag = true
        settleClosed(new Error("pop-out handoff timed out waiting for ready"))
      }
    }, timeoutMs)
  })()

  return {
    ready,
    closed,
    isClosed: () => closedFlag,
    cancel,
    armed,
  }
}

class DetachCasError extends Error {
  readonly restoreToken: DetachRestoreToken
  readonly tabRemoved = true as const
  constructor(restoreToken: DetachRestoreToken) {
    super("opened_tabs CAS rejected")
    this.name = "DetachCasError"
    this.restoreToken = restoreToken
  }
}

function isDetachCasError(e: unknown): e is DetachCasError {
  return (
    e instanceof DetachCasError ||
    (typeof e === "object" &&
      e != null &&
      (e as { name?: string }).name === "DetachCasError" &&
      "restoreToken" in e)
  )
}

async function detachIfNeeded(
  tabId: string | undefined
): Promise<{ restoreToken: DetachRestoreToken; tabRemoved: boolean } | null> {
  if (!tabId) return null
  const result = useTabStore.getState().detachTab(tabId)
  if (!result.ok) {
    throw new Error(result.reason)
  }
  const flush = await useTabStore.getState().flushOpenedTabsSave()
  if (!flush.accepted) {
    // Do not restore here — compensation does reverse first, then restore.
    throw new DetachCasError(result.restoreToken)
  }
  return { restoreToken: result.restoreToken, tabRemoved: true }
}

function isHandoffComplete(status: PopoutOpStatus | null | undefined): boolean {
  return status?.phase === "handoff_complete"
}

function isAbortedPhase(status: PopoutOpStatus | null | undefined): boolean {
  return status?.phase === "aborted"
}

/** Parse abort outcome for safe compensation branching. */
function classifyAbortOutcome(outcome: unknown): {
  kind:
    | "already_complete"
    | "superseded"
    | "connection_gone"
    | "reclaimable"
    | "unknown"
} {
  if (outcome == null) return { kind: "unknown" }
  if (typeof outcome === "string") {
    const s = outcome.toLowerCase()
    if (s.includes("already_complete") || s.includes("alreadycomplete")) {
      return { kind: "already_complete" }
    }
    if (s.includes("superseded")) return { kind: "superseded" }
    if (s.includes("connection_gone") || s.includes("connectiongone")) {
      return { kind: "connection_gone" }
    }
    if (
      s.includes("never_rebound") ||
      s.includes("already_main") ||
      s.includes("reversed")
    ) {
      return { kind: "reclaimable" }
    }
    return { kind: "unknown" }
  }
  if (typeof outcome === "object") {
    const o = outcome as Record<string, unknown>
    // serde internally tagged: { kind: "connection_gone" } or snake keys
    const keys = Object.keys(o).map((k) => k.toLowerCase())
    if (
      keys.some(
        (k) => k.includes("already_complete") || k === "alreadycomplete"
      )
    ) {
      return { kind: "already_complete" }
    }
    if (keys.some((k) => k.includes("superseded"))) {
      return { kind: "superseded" }
    }
    if (
      keys.some(
        (k) => k.includes("connection_gone") || k === "connectiongone"
      )
    ) {
      return { kind: "connection_gone" }
    }
    if (
      keys.some(
        (k) =>
          k.includes("never_rebound") ||
          k.includes("already_main") ||
          k.includes("reversed")
      )
    ) {
      return { kind: "reclaimable" }
    }
    if (typeof o.kind === "string") {
      return classifyAbortOutcome(o.kind)
    }
  }
  return { kind: "unknown" }
}

/**
 * Extract post-reverse ownership generation from abort outcomes.
 * Accepts both internally tagged `{ kind: "reversed", generation }` and
 * externally tagged `{ reversed: { generation } }` wire shapes.
 */
function extractReversedGeneration(outcome: unknown): number | null {
  if (outcome == null || typeof outcome !== "object") return null
  const o = outcome as Record<string, unknown>
  const kind =
    typeof o.kind === "string" ? o.kind.toLowerCase() : null
  if (
    kind === "reversed" &&
    typeof o.generation === "number" &&
    Number.isFinite(o.generation)
  ) {
    return o.generation
  }
  for (const key of Object.keys(o)) {
    if (!key.toLowerCase().includes("reversed")) continue
    const nested = o[key]
    if (nested != null && typeof nested === "object") {
      const g = (nested as { generation?: unknown }).generation
      if (typeof g === "number" && Number.isFinite(g)) return g
    }
    if (typeof nested === "number" && Number.isFinite(nested)) {
      return nested
    }
  }
  return null
}

/**
 * Restore detached tab then CAS-flush, re-merging the token and retrying when
 * the server snapshot rejects (plan: ≤3 attempts). Fails observably so callers
 * do not close the detached window after a lost restore.
 */
async function restoreTabWithFlushRetry(
  restoreToken: DetachRestoreToken
): Promise<void> {
  const maxAttempts = 3
  let lastAccepted = false
  let lastVersion: number | undefined
  for (let attempt = 0; attempt < maxAttempts; attempt++) {
    // Re-merge on every attempt: a rejected flush applies the remote snapshot
    // which can drop the restored tab again.
    useTabStore.getState().restoreDetachedTab(restoreToken)
    try {
      const flush = await useTabStore.getState().flushOpenedTabsSave()
      lastAccepted = flush.accepted
      lastVersion = flush.version
      if (flush.accepted) return
    } catch (e) {
      console.error(
        "[ConversationPopout] restore flush attempt failed",
        attempt + 1,
        e
      )
    }
  }
  const err = new Error(
    `restore opened_tabs CAS rejected after ${maxAttempts} retries` +
      (lastVersion != null ? ` (version=${lastVersion})` : "")
  )
  console.error("[ConversationPopout]", err.message, { lastAccepted })
  throw err
}

/**
 * Compensation: reverse (abort) first, then main reclaim, then restore + flush
 * retry, then close detached. already_complete / superseded / unknown are
 * non-destructive (do not kill a live transferred session).
 */
async function compensate(args: {
  conversationId: number
  operationId: string
  restoreToken?: DetachRestoreToken | null
  tabRemoved?: boolean
}): Promise<void> {
  let abortOutcome: unknown = null
  try {
    abortOutcome = await abortConversationPopoutOperation(args.operationId)
  } catch {
    /* best-effort reverse */
  }

  let status: PopoutOpStatus | null = null
  try {
    status = await getConversationPopoutOperation(args.operationId)
  } catch {
    /* ignore */
  }

  if (isHandoffComplete(status)) {
    detachedCache.add(args.conversationId)
    clearTransferringOut(args.conversationId, args.operationId)
    return
  }

  const classified = classifyAbortOutcome(
    abortOutcome ?? status?.abortOutcome ?? null
  )
  if (classified.kind === "already_complete") {
    detachedCache.add(args.conversationId)
    clearTransferringOut(args.conversationId, args.operationId)
    return
  }
  if (
    classified.kind === "superseded" ||
    classified.kind === "connection_gone" ||
    classified.kind === "unknown"
  ) {
    // Non-destructive / non-reclaimable:
    // - superseded/unknown: never restore/close against a newer owner
    // - connection_gone: agent died between forward and abort — do not invent
    //   CONNECTION_CREATED for a dead connection
    clearTransferringOut(args.conversationId, args.operationId)
    // connection_gone still restores the main tab UI (no live agent to reclaim)
    if (classified.kind === "connection_gone") {
      // fall through to restore + close detached without reclaim
    } else {
      return
    }
  }

  // reclaimable: never_rebound | already_main | reversed
  // Order: reverse (above) → main lease refresh/reclaim → tab restore → close.
  // After reverse, adopt the post-reverse lease (current op + new generation)
  // rather than the pre-transfer snapshot taken at release.
  // Pre-ready claim failure: main may still hold the connection (not released);
  // still refresh the in-place lease when reverse returned a generation.
  const fence = getTransferFence(args.conversationId)
  if (
    classified.kind === "reclaimable" &&
    fence?.operationId === args.operationId
  ) {
    const reversedGen = extractReversedGeneration(
      abortOutcome ?? status?.abortOutcome ?? null
    )
    const reclaimLease =
      reversedGen != null
        ? {
            ownershipGeneration: reversedGen,
            ownerWindowLabel: "main" as const,
          }
        : undefined
    // Full reclaim when main released; in-place lease refresh when reverse
    // advanced the generation while main still holds the connection.
    if (fence.mainReleased || reclaimLease != null) {
      try {
        await reclaimAfterAbort(
          args.conversationId,
          args.operationId,
          reclaimLease
        )
      } catch (e) {
        // Fail closed: do not restore/close detached without a restored main
        // owner — that would strand the agent or leave no UI owner.
        console.error(
          "[ConversationPopout] reclaimAfterAbort failed; leaving detached open",
          e
        )
        clearTransferringOut(args.conversationId, args.operationId)
        throw e
      }
    }
  }

  if (args.tabRemoved && args.restoreToken) {
    try {
      await restoreTabWithFlushRetry(args.restoreToken)
    } catch (e) {
      // Fail observably before closing detached so we do not drop both UIs.
      console.error(
        "[ConversationPopout] restore+flush failed; leaving detached open",
        e
      )
      clearTransferringOut(args.conversationId, args.operationId)
      throw e
    }
  }

  try {
    await closeConversationWindow(args.conversationId, args.operationId)
  } catch {
    /* ignore */
  }
  clearTransferringOut(args.conversationId, args.operationId)
}

/**
 * Orchestrate pop-out: open → ready → release without disconnect → detach +
 * CAS → complete. Re-check closed after every await stage.
 */
export async function popOutConversation(args: {
  conversationId: number
  folderId: number
  agentType: AgentType
}): Promise<void> {
  ensureClosedListener()

  const existing = inFlight.get(args.conversationId)
  if (existing) {
    await existing
    return
  }

  // Register single-flight immediately so concurrent openTab sees the fence
  // before any await inside the run body.
  let settleInFlight!: () => void
  const inFlightDone = new Promise<void>((resolve) => {
    settleInFlight = resolve
  })
  inFlight.set(args.conversationId, inFlightDone)

  const run = (async () => {
    const tabs = useTabStore.getState().rawTabs
    const tab = tabs.find(
      (t) =>
        t.conversationId === args.conversationId &&
        t.folderId === args.folderId &&
        t.agentType === args.agentType
    )
    const enablement = canPopOutConversation({
      conversationId: args.conversationId,
      isOpenMainTab: !!tab,
      mainTabCount: tabs.length,
    })
    if (!enablement.enabled) {
      throw new Error(enablement.reason)
    }

    if (await focusDetachedConversation(args.conversationId)) {
      return
    }

    const operationId =
      typeof crypto !== "undefined" && crypto.randomUUID
        ? crypto.randomUUID()
        : `op-${Date.now()}-${Math.random().toString(16).slice(2)}`

    // Fence before open/work so concurrent openTab focus-or-skips.
    markTransferringOut(args.conversationId, operationId)

    const wait = armHandoffWait(operationId, args.conversationId, 15_000)
    const assertNotClosed = () => {
      if (wait.isClosed()) {
        throw new Error("pop-out window closed before handoff completed")
      }
    }

    try {
      await wait.armed
    } catch (e) {
      wait.cancel()
      clearTransferringOut(args.conversationId, operationId)
      throw e
    }

    let openResult: OpenConversationResult
    try {
      openResult = await openConversationWindow({
        conversationId: args.conversationId,
        folderId: args.folderId,
        agentType: args.agentType,
        operationId,
      })
    } catch (e) {
      wait.cancel()
      // Window may have been created despite error in older code paths;
      // still attempt non-destructive abort + close CAS.
      await compensate({
        conversationId: args.conversationId,
        operationId,
      })
      throw e
    }

    if (openResult === "focusedExisting") {
      wait.cancel()
      clearTransferringOut(args.conversationId, operationId)
      detachedCache.add(args.conversationId)
      return
    }

    let restoreToken: DetachRestoreToken | null = null
    let tabRemoved = false

    try {
      await Promise.race([wait.ready, wait.closed])
      assertNotClosed()

      await releaseConnectionWithoutDisconnect(args.conversationId, operationId)
      assertNotClosed()

      // Re-resolve the current main tab immediately before detach: a concurrent
      // openTab may have created a tab after our initial snapshot (sidebar /
      // deep-link race). Prefer detaching that live tab over a stale id.
      const currentTab = useTabStore.getState().rawTabs.find(
        (t) =>
          t.conversationId === args.conversationId &&
          t.folderId === args.folderId &&
          t.agentType === args.agentType
      )
      const tabIdToDetach = currentTab?.id ?? tab?.id

      try {
        const detachResult = await detachIfNeeded(tabIdToDetach)
        restoreToken = detachResult?.restoreToken ?? null
        tabRemoved = !!detachResult?.tabRemoved
      } catch (detachErr) {
        if (isDetachCasError(detachErr)) {
          restoreToken = detachErr.restoreToken
          tabRemoved = true
        }
        throw detachErr
      }
      assertNotClosed()

      const status = await completeConversationPopoutOperation(operationId)
      if (isAbortedPhase(status) || !isHandoffComplete(status)) {
        throw new Error(
          `pop-out complete returned non-success phase: ${status?.phase ?? "unknown"}`
        )
      }

      // Commit-ack: detached cold path stays connect-gated until this arrives
      // (or poll sees HandoffComplete). Emit even on idempotent already-complete.
      try {
        const { emit } = await import("@tauri-apps/api/event")
        await emit("conversation-window://commit-ack", { operationId })
      } catch (e) {
        console.error("[ConversationPopout] emit commit-ack failed", e)
      }

      wait.cancel()
      detachedCache.add(args.conversationId)
      clearTransferringOut(args.conversationId, operationId)
    } catch (e) {
      wait.cancel()
      await compensate({
        conversationId: args.conversationId,
        operationId,
        restoreToken,
        tabRemoved,
      })
      throw e
    }
  })()

  try {
    await run
  } finally {
    settleInFlight()
    inFlight.delete(args.conversationId)
  }
}
