import {
  abortConversationPopoutOperation,
  closeConversationWindow,
  completeConversationPopoutOperation,
  focusConversationWindow,
  openConversationWindow,
  type OpenConversationResult,
} from "@/lib/api"
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

function waitForReady(
  operationId: string,
  conversationId: number,
  timeoutMs: number
): Promise<ReadyPayload> {
  return new Promise((resolve, reject) => {
    let settled = false
    let unsubReady: (() => void) | null = null
    let unsubClosed: (() => void) | null = null
    const timer = setTimeout(() => {
      finish(() =>
        reject(new Error("pop-out handoff timed out waiting for ready"))
      )
    }, timeoutMs)

    const finish = (fn: () => void) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      unsubReady?.()
      unsubClosed?.()
      fn()
    }

    void subscribe<ReadyPayload>("conversation-window://ready", (payload) => {
      if (
        payload?.operationId === operationId &&
        payload.conversationId === conversationId
      ) {
        finish(() => resolve(payload))
      }
    }).then((u) => {
      unsubReady = u
    })

    void subscribe<ClosedPayload>("conversation-window://closed", (payload) => {
      if (payload?.operationId === operationId) {
        finish(() =>
          reject(new Error("pop-out window closed before handoff completed"))
        )
      }
    }).then((u) => {
      unsubClosed = u
    })
  })
}

async function detachIfNeeded(
  tabId: string | undefined
): Promise<{ restoreToken: DetachRestoreToken } | null> {
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
  return { restoreToken: result.restoreToken }
}

/**
 * Orchestrate pop-out: open window → wait ready → detachTab + CAS → complete.
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

    const readyPromise = waitForReady(operationId, args.conversationId, 15_000)

    let openResult: OpenConversationResult
    try {
      openResult = await openConversationWindow({
        conversationId: args.conversationId,
        folderId: args.folderId,
        agentType: args.agentType,
        operationId,
      })
    } catch (e) {
      throw e
    }

    if (openResult === "focusedExisting") {
      detachedCache.add(args.conversationId)
      return
    }

    try {
      await readyPromise
    } catch (e) {
      try {
        await abortConversationPopoutOperation(operationId)
      } catch {
        /* ignore */
      }
      try {
        await closeConversationWindow(args.conversationId, operationId)
      } catch {
        /* ignore */
      }
      throw e
    }

    const detachResult = await detachIfNeeded(tab?.id)

    try {
      await completeConversationPopoutOperation(operationId)
      detachedCache.add(args.conversationId)
    } catch (e) {
      if (detachResult?.restoreToken) {
        useTabStore.getState().restoreDetachedTab(detachResult.restoreToken)
        await useTabStore.getState().flushOpenedTabsSave().catch(() => null)
      }
      try {
        await abortConversationPopoutOperation(operationId)
      } catch {
        /* ignore */
      }
      try {
        await closeConversationWindow(args.conversationId, operationId)
      } catch {
        /* ignore */
      }
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
