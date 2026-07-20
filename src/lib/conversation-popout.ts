import {
  abortConversationPopoutOperation,
  closeConversationWindow,
  completeConversationPopoutOperation,
  focusConversationWindow,
  openConversationWindow,
  type OpenConversationResult,
} from "@/lib/api"
import {
  clearTransferringOut,
  markMainReleased,
  markTransferringOut,
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

/**
 * Register ready/closed listeners **before** open returns so a fast ready
 * emission cannot race past an incomplete subscribe().
 */
function armReadyWait(
  operationId: string,
  conversationId: number,
  timeoutMs: number
): {
  ready: Promise<ReadyPayload>
  armed: Promise<void>
} {
  let settleReady!: (v: ReadyPayload) => void
  let settleReject!: (e: Error) => void
  let settled = false
  let unsubReady: (() => void) | null = null
  let unsubClosed: (() => void) | null = null
  let timer: ReturnType<typeof setTimeout> | null = null

  const ready = new Promise<ReadyPayload>((resolve, reject) => {
    settleReady = resolve
    settleReject = reject
  })

  const finish = (fn: () => void) => {
    if (settled) return
    settled = true
    if (timer) clearTimeout(timer)
    unsubReady?.()
    unsubClosed?.()
    fn()
  }

  const armed = (async () => {
    unsubReady = await subscribe<ReadyPayload>(
      "conversation-window://ready",
      (payload) => {
        if (
          payload?.operationId === operationId &&
          payload.conversationId === conversationId
        ) {
          finish(() => settleReady(payload))
        }
      }
    )
    unsubClosed = await subscribe<ClosedPayload>(
      "conversation-window://closed",
      (payload) => {
        if (payload?.operationId === operationId) {
          finish(() =>
            settleReject(
              new Error("pop-out window closed before handoff completed")
            )
          )
        }
      }
    )
    timer = setTimeout(() => {
      finish(() =>
        settleReject(new Error("pop-out handoff timed out waiting for ready"))
      )
    }, timeoutMs)
  })()

  return { ready, armed }
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
    useTabStore.getState().restoreDetachedTab(result.restoreToken)
    await useTabStore.getState().flushOpenedTabsSave().catch(() => null)
    throw new Error("opened_tabs CAS rejected")
  }
  return { restoreToken: result.restoreToken, tabRemoved: true }
}

async function compensateAbortClose(args: {
  conversationId: number
  operationId: string
  restoreToken?: DetachRestoreToken | null
}): Promise<void> {
  // reverse (via abort) → restore tab → close window
  try {
    await abortConversationPopoutOperation(args.operationId)
  } catch {
    /* best-effort reverse */
  }
  if (args.restoreToken) {
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
 * Orchestrate pop-out: open window → wait ready → release + detachTab + CAS → complete.
 *
 * After ready, any detach/CAS failure MUST reverse/abort/close (no escape path
 * that leaves ownership on the detached window while main tab is restored).
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

    // Fence main disconnect before open so unmount/idle cannot kill session.
    markTransferringOut(args.conversationId, operationId)

    const { ready: readyPromise, armed } = armReadyWait(
      operationId,
      args.conversationId,
      15_000
    )
    // Listeners must be registered before open can emit ready.
    await armed

    let openResult: OpenConversationResult
    try {
      openResult = await openConversationWindow({
        conversationId: args.conversationId,
        folderId: args.folderId,
        agentType: args.agentType,
        operationId,
      })
    } catch (e) {
      clearTransferringOut(args.conversationId, operationId)
      throw e
    }

    if (openResult === "focusedExisting") {
      clearTransferringOut(args.conversationId, operationId)
      detachedCache.add(args.conversationId)
      return
    }

    try {
      await readyPromise
    } catch (e) {
      await compensateAbortClose({
        conversationId: args.conversationId,
        operationId,
      })
      throw e
    }

    // Live path: mark main released (drop local UI ownership without disconnect).
    // Full bridge release is consulted by ACP idle/unmount via isTransferringOut.
    markMainReleased(args.conversationId, operationId)

    let restoreToken: DetachRestoreToken | null = null
    try {
      const detachResult = await detachIfNeeded(tab?.id)
      restoreToken = detachResult?.restoreToken ?? null

      await completeConversationPopoutOperation(operationId)
      detachedCache.add(args.conversationId)
      clearTransferringOut(args.conversationId, operationId)
    } catch (e) {
      // Critical: reverse → restore → close. Never leave ownership on detached
      // after detach/CAS/complete failure without compensation.
      await compensateAbortClose({
        conversationId: args.conversationId,
        operationId,
        restoreToken,
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
