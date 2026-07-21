"use client"

import { Suspense, useEffect, useMemo, useState } from "react"
import { useSearchParams } from "next/navigation"
import { useTranslations } from "next-intl"
import { Loader2 } from "lucide-react"
import { AppTitleBar } from "@/components/layout/app-title-bar"
import { AppToaster } from "@/components/ui/app-toaster"
import { ConversationSessionSurface } from "@/components/conversations/conversation-session-surface"
import {
  acpFindConnectionForConversation,
  getFolder,
  getFolderConversation,
  getConversationPopoutOperation,
  rebindConnectionOwnerWindow,
} from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import {
  buildReadyPayload,
  classifyDiscoveryResult,
  conversationWindowLabel,
  CONVERSATION_WINDOW_COMMIT_ACK_EVENT,
  CONVERSATION_WINDOW_READY_EVENT,
  decideLiveHandoffResult,
  isAbortedPhase,
  isHandoffCompletePhase,
  parseConversationPopoutQuery,
  resolveDetachedConnectGate,
  shouldClearSuppressOnDetachedUnmount,
  shouldMountDetachedSurface,
  shouldReverseRebindAfterLiveFailure,
} from "@/lib/conversation-popout-detached-bootstrap"
import {
  claimConnectionOwnership,
  setSuppressFrontendDisconnect,
} from "@/lib/conversation-popout-acp-bridge"
import { isLocalDesktop, subscribe } from "@/lib/platform"
import type { AgentType, DbConversationDetail, FolderDetail } from "@/lib/types"
import { RemoteConnectionGate } from "@/contexts/remote-connection-context"
import {
  DetachedOpenTabKeysRegistrar,
  DetachedShellProviders,
  seedDetachedFolder,
  seedDetachedSessionTab,
} from "./_components/detached-shell"

const TOAST_DURATION_MS = 6000
const COMMIT_ACK_POLL_MS = 750
const COMMIT_ACK_POLL_MAX_MS = 30_000

function ConversationPageInner() {
  const t = useTranslations("ConversationPopout")
  const searchParams = useSearchParams()
  const localDesktop = isLocalDesktop()

  const parsed = useMemo(
    () =>
      parseConversationPopoutQuery({
        conversationId: searchParams.get("conversationId"),
        folderId: searchParams.get("folderId"),
        agentType: searchParams.get("agentType"),
        operationId: searchParams.get("operationId"),
      }),
    [searchParams]
  )

  const [conversation, setConversation] = useState<DbConversationDetail | null>(
    null
  )
  const [folder, setFolder] = useState<FolderDetail | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [bootstrapReady, setBootstrapReady] = useState(false)
  const [isLivePath, setIsLivePath] = useState(false)
  const [commitAcked, setCommitAcked] = useState(false)
  const [tabId, setTabId] = useState<string | null>(null)
  const [readyEmitted, setReadyEmitted] = useState(false)

  const valid = parsed != null
  const conversationId = parsed?.conversationId ?? 0
  const folderId = parsed?.folderId ?? 0
  const agentType: AgentType | null = parsed?.agentType ?? null

  // Local-desktop boundary: reject browser / remote workspace before any
  // metadata or ACP work. Menu gates are not enough for a static route.
  useEffect(() => {
    if (!localDesktop) {
      setError(t("localDesktopOnly"))
    }
  }, [localDesktop, t])

  // 1–2: Load conversation + folder metadata
  useEffect(() => {
    if (!parsed || !localDesktop) return
    let cancelled = false
    ;(async () => {
      try {
        const [c, f] = await Promise.all([
          getFolderConversation(parsed.conversationId),
          getFolder(parsed.folderId),
        ])
        if (cancelled) return
        setConversation(c)
        setFolder(f)
        setError(null)
        seedDetachedFolder(f)
        const seededTabId = seedDetachedSessionTab({
          folderId: parsed.folderId,
          conversationId: parsed.conversationId,
          agentType: parsed.agentType,
          workingDir: f.path,
          title: c.summary.title ?? undefined,
        })
        setTabId(seededTabId)
      } catch (err) {
        if (!cancelled) {
          setConversation(null)
          setFolder(null)
          setError(toErrorMessage(err))
        }
      }
    })()
    return () => {
      cancelled = true
    }
  }, [parsed, localDesktop])

  // 3–5: Claim/rebind (live) or cold gate; emit ready only on success; suppress until ack
  useEffect(() => {
    if (
      !parsed ||
      !localDesktop ||
      !conversation ||
      !folder ||
      !tabId ||
      readyEmitted
    ) {
      return
    }
    let cancelled = false

    ;(async () => {
      setSuppressFrontendDisconnect(parsed.conversationId, true)

      const externalId = conversation.summary.external_id ?? undefined
      let discoveredRaw: { connection_id?: string | null } | null = null
      let discoveryError: unknown = null
      try {
        discoveredRaw = await acpFindConnectionForConversation(
          parsed.conversationId,
          externalId,
          parsed.agentType
        )
      } catch (e) {
        discoveryError = e
      }

      if (cancelled) return

      const discovery = classifyDiscoveryResult({
        discovered: discoveredRaw,
        error: discoveryError,
        errorMessage:
          discoveryError != null ? toErrorMessage(discoveryError) : undefined,
      })

      // Discovery transport/API failure: do NOT emit cold ready — main still owns.
      if (discovery.kind === "error") {
        console.error(
          "[ConversationPopout] live discovery failed",
          discoveryError
        )
        if (!cancelled) {
          setError(discovery.message || t("liveHandoffFailed"))
          setBootstrapReady(false)
          setIsLivePath(false)
        }
        return
      }

      let ownershipGeneration: number | undefined
      let live = false
      let connectionIdForReady: string | null = null

      if (discovery.kind === "live") {
        const discoveredConnectionId = discovery.connectionId
        const label = conversationWindowLabel(parsed.conversationId)
        let rebindError: unknown = null
        let rebindGen: number | null = null
        let claimError: unknown = null

        try {
          const rebind = await rebindConnectionOwnerWindow({
            conversationId: parsed.conversationId,
            connectionId: discoveredConnectionId,
            fromOwnerWindow: "main",
            toOwnerWindow: label,
            operationId: parsed.operationId,
          })
          rebindGen = rebind.ownershipGeneration
        } catch (e) {
          rebindError = e
        }

        if (!rebindError && rebindGen != null) {
          try {
            const claimResult = await claimConnectionOwnership({
              conversationId: parsed.conversationId,
              connectionId: discoveredConnectionId,
              agentType: parsed.agentType,
              workingDir: folder.path,
              operationId: parsed.operationId,
              contextKey: tabId,
              expectedOwnerWindowLabel: "main",
              ownershipGeneration: rebindGen,
              ownerWindowLabel: label,
            })
            // Live claim must return the same connection; empty/mismatched
            // results mean the bridge no-oped or attached the wrong owner.
            if (
              !claimResult?.connectionId ||
              claimResult.connectionId !== discoveredConnectionId
            ) {
              claimError = new Error(
                "claimConnectionOwnership did not confirm the rebinding connection"
              )
            }
          } catch (e) {
            claimError = e
          }
        }

        if (cancelled) return

        const decision = decideLiveHandoffResult({
          connectionId: discoveredConnectionId,
          rebindError,
          rebindErrorMessage:
            rebindError != null ? toErrorMessage(rebindError) : undefined,
          ownershipGeneration: rebindGen,
          claimError,
          claimErrorMessage:
            claimError != null ? toErrorMessage(claimError) : undefined,
        })

        if (decision.kind === "failed") {
          console.error(
            "[ConversationPopout] live rebind/claim failed",
            decision.message,
            { rebindError, claimError }
          )
          // Reverse forward rebind when claim failed after successful CAS.
          if (
            shouldReverseRebindAfterLiveFailure({
              rebindSucceeded: decision.rebindSucceeded,
              ownershipGeneration: decision.ownershipGeneration,
            }) &&
            decision.connectionId &&
            decision.ownershipGeneration != null
          ) {
            try {
              await rebindConnectionOwnerWindow({
                conversationId: parsed.conversationId,
                connectionId: decision.connectionId,
                fromOwnerWindow: label,
                toOwnerWindow: "main",
                operationId: parsed.operationId,
                expectedGeneration: decision.ownershipGeneration,
              })
            } catch (revErr) {
              console.error(
                "[ConversationPopout] reverse rebind after claim failure failed",
                revErr
              )
            }
          }
          if (!cancelled) {
            setError(decision.message || t("liveHandoffFailed"))
            setBootstrapReady(false)
            setIsLivePath(false)
          }
          // Keep suppress=true so any partial claim teardown is viewer-style.
          return
        }

        live = true
        ownershipGeneration = decision.ownershipGeneration
        connectionIdForReady = decision.connectionId
      }

      // discovery.kind === "none" → true cold path
      if (cancelled) return

      setIsLivePath(live)
      setBootstrapReady(true)

      const payload = buildReadyPayload({
        conversationId: parsed.conversationId,
        operationId: parsed.operationId,
        ownershipGeneration: ownershipGeneration ?? null,
        connectionId: connectionIdForReady,
      })

      try {
        const { emit } = await import("@tauri-apps/api/event")
        await emit(CONVERSATION_WINDOW_READY_EVENT, payload)
      } catch (e) {
        console.error("[ConversationPopout] emit ready failed", e)
      }
      if (!cancelled) setReadyEmitted(true)
    })()

    return () => {
      cancelled = true
    }
  }, [parsed, conversation, folder, tabId, readyEmitted, localDesktop, t])

  // Commit-ack listener + poll fallback
  useEffect(() => {
    if (!parsed || !bootstrapReady || commitAcked) return
    let cancelled = false
    let unsub: (() => void) | null = null
    let pollTimer: ReturnType<typeof setInterval> | null = null
    const startedAt = Date.now()

    const applyAck = () => {
      if (cancelled) return
      setCommitAcked(true)
      // Clear suppress only after handoff commits while the tree is still
      // mounted — never from a parent unmount effect (React 19 parent-first
      // cleanup would race descendant useConnectionLifecycle disconnect).
      setSuppressFrontendDisconnect(parsed.conversationId, false)
    }

    void (async () => {
      unsub = await subscribe<{ operationId?: string }>(
        CONVERSATION_WINDOW_COMMIT_ACK_EVENT,
        (payload) => {
          if (payload?.operationId === parsed.operationId) {
            applyAck()
          }
        }
      )

      pollTimer = setInterval(() => {
        if (cancelled) return
        if (Date.now() - startedAt > COMMIT_ACK_POLL_MAX_MS) {
          if (pollTimer) clearInterval(pollTimer)
          return
        }
        void getConversationPopoutOperation(parsed.operationId)
          .then((status) => {
            if (cancelled) return
            if (isHandoffCompletePhase(status?.phase)) {
              applyAck()
            } else if (isAbortedPhase(status?.phase)) {
              // Stay gated + suppressed; main remains / reclaims ownership.
              if (pollTimer) clearInterval(pollTimer)
            }
          })
          .catch(() => {})
      }, COMMIT_ACK_POLL_MS)
    })()

    return () => {
      cancelled = true
      unsub?.()
      if (pollTimer) clearInterval(pollTimer)
    }
  }, [parsed, bootstrapReady, commitAcked])

  // Intentionally do NOT clear suppress on unmount. Parent cleanup runs before
  // descendants; clearing would let useConnectionLifecycle bare-acpDisconnect.
  useEffect(() => {
    if (!parsed) return
    return () => {
      if (shouldClearSuppressOnDetachedUnmount()) {
        setSuppressFrontendDisconnect(parsed.conversationId, false)
      }
    }
  }, [parsed])

  const gate = resolveDetachedConnectGate({
    bootstrapReady,
    isLivePath,
    commitAcked,
  })

  const title = useMemo(() => {
    if (!conversation) return t("title")
    return conversation.summary.title?.trim() || t("untitled")
  }, [conversation, t])

  useEffect(() => {
    document.title = `${title} - codeg`
  }, [title])

  const workingDir = folder?.path
  const showSurface = shouldMountDetachedSurface({
    valid: valid && localDesktop,
    hasError: Boolean(error),
    bootstrapReady,
    readyEmitted,
    isActive: gate.isActive,
  })

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <AppTitleBar
        center={
          <div className="text-sm font-semibold tracking-tight">{title}</div>
        }
      />
      <main className="flex min-h-0 flex-1 flex-col">
        {!localDesktop ? (
          <div className="m-3 rounded-lg border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {t("localDesktopOnly")}
          </div>
        ) : !valid ? (
          <div className="m-3 rounded-lg border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {t("invalidParams")}
          </div>
        ) : error ? (
          <div className="m-3 rounded-lg border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {error}
          </div>
        ) : !showSurface ? (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            {t("loading")}
          </div>
        ) : (
          <>
            <DetachedOpenTabKeysRegistrar contextKey={tabId!} />
            <div className="min-h-0 flex-1">
              <ConversationSessionSurface
                tabId={tabId!}
                conversationId={conversationId}
                folderId={folderId}
                agentType={agentType!}
                workingDir={workingDir}
                isActive={gate.isActive}
                showActiveFlow={false}
                reloadSignal={0}
                ownerOperationId={parsed?.operationId ?? null}
              />
            </div>
          </>
        )}
      </main>
      <AppToaster duration={TOAST_DURATION_MS} />
    </div>
  )
}

export default function ConversationPage() {
  return (
    <RemoteConnectionGate>
      <DetachedShellProviders>
        <Suspense
          fallback={
            <div className="flex h-screen items-center justify-center text-sm text-muted-foreground">
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            </div>
          }
        >
          <ConversationPageInner />
        </Suspense>
      </DetachedShellProviders>
    </RemoteConnectionGate>
  )
}
