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
  markTransferringOut,
  releaseConnectionWithoutDisconnect,
} from "@/lib/conversation-popout-acp-bridge"
import { isLocalDesktop, subscribe } from "@/lib/platform"
import type { AgentType } from "@/lib/types"
import {
  useTabStore,
  type DetachRestoreToken,
} from "@/stores/tab-store"

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
  cancel: () => void
  armed: Promise<void>
}

/**
 * Register ready/closed listeners **before** open returns so a fast ready
 * emission cannot race past an incomplete subscribe().
 * Closed stays observed until cancel() — including after ready.
 */
function armHandoffWait(
  operationId: string,
  conversationId: number,
  timeoutMs: number
): ArmedWait {
  let settleReady!: (v: ReadyPayload) => void
  let settleClosed!: (e: Error) => void
  let readySettled = false
  let cancelled = false
  let unsubReady: (() => void) | null = null
  let unsubClosed: (() => void) | null = null
  let timer: ReturnType<typeof setTimeout> | null = null

  const ready = new Promise<ReadyPayload>((resolve, reject) => {
    settleReady = resolve
    // ready rejection shared with timeout path
    void reject
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
          // Keep closed armed until cancel after complete.
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
          settleClosed(
            new Error("pop-out window closed before handoff completed")
          )
        }
      }
    )
    timer = setTimeout(() => {
      if (!readySettled) {
        settleClosed(new Error("pop-out handoff timed out waiting for ready"))
      }
    }, timeoutMs)
  })()

  return { ready, closed, cancel, armed }
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
    throw Object.assign(new Error("opened_tabs CAS rejected"), {
      restoreToken: result.restoreToken,
      tabRemoved: true,
    })
  }
  return { restoreToken: result.restoreToken, tabRemoved: true }
}

function isTerminalHandoffComplete(status: PopoutOpStatus | null | undefined): boolean {
  const phase = status?.phase
  return phase === "handoff_complete" || phase === "HandoffComplete"
}

function isTerminalAborted(status: PopoutOpStatus | null | undefined): boolean {
  const phase = status?.phase
  return phase === "aborted" || phase === "Aborted"
}

/**
 * Compensation: reverse (abort) first, then restore tab only for reclaimable
 * outcomes. Never restore/close on AlreadyComplete (lost ack after success).
 */
async function compensate(args: {
  conversationId: number
  operationId: string
  restoreToken?: DetachRestoreToken | null
  /** When true, tab was already detached and may need restore. */
  tabRemoved?: boolean
}): Promise<void> {
  let abortOutcome: unknown = null
  try {
    abortOutcome = await abortConversationPopoutOperation(args.operationId)
  } catch {
    /* best-effort reverse */
  }

  // Authoritative status if abort payload is sparse
  let status: PopoutOpStatus | null = null
  try {
    status = await getConversationPopoutOperation(args.operationId)
  } catch {
    /* ignore */
  }

  if (isTerminalHandoffComplete(status)) {
    // Lost-response after success: do NOT restore main tab or close detached.
    detachedCache.add(args.conversationId)
    clearTransferringOut(args.conversationId, args.operationId)
    return
  }

  const outcomeStr = JSON.stringify(abortOutcome ?? status?.abortOutcome ?? "")
  const superseded =
    outcomeStr.includes("Superseded") || outcomeStr.includes("superseded")
  if (superseded) {
    // Newer owner: clear our fences only; do not restore/close B.
    clearTransferringOut(args.conversationId, args.operationId)
    return
  }

  if (args.tabRemoved && args.restoreToken) {
    useTabStore.getState().restoreDetachedTab(args.restoreToken)
    await useTabStore.getState().flushOpenedTabsSave().catch(() => null)
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
 * CAS → complete. Closed observation stays armed until terminal success.
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

    markTransferringOut(args.conversationId, operationId)

    const wait = armHandoffWait(operationId, args.conversationId, 15_000)
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
      clearTransferringOut(args.conversationId, operationId)
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
      // Race ready against closed — closed stays armed after ready too.
      await Promise.race([wait.ready, wait.closed])

      // Live path: drop main owner UI without killing the agent.
      await releaseConnectionWithoutDisconnect(
        args.conversationId,
        operationId
      )

      try {
        const detachResult = await detachIfNeeded(tab?.id)
        restoreToken = detachResult?.restoreToken ?? null
        tabRemoved = !!detachResult?.tabRemoved
      } catch (detachErr) {
        const token = (
          detachErr as { restoreToken?: DetachRestoreToken }
        )?.restoreToken
        if (token) {
          restoreToken = token
          tabRemoved = true
        }
        throw detachErr
      }

      const status = await completeConversationPopoutOperation(operationId)
      if (isTerminalAborted(status) || !isTerminalHandoffComplete(status)) {
        throw new Error(
          `pop-out complete returned non-success phase: ${status?.phase ?? "unknown"}`
        )
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

  inFlight.set(args.conversationId, run)
  try {
    await run
  } finally {
    inFlight.delete(args.conversationId)
  }
}
