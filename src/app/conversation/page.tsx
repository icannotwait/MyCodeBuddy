"use client"

import { Suspense, useCallback, useEffect, useMemo, useState } from "react"
import { useSearchParams } from "next/navigation"
import { useTranslations } from "next-intl"
import { Loader2 } from "lucide-react"
import { AppTitleBar } from "@/components/layout/app-title-bar"
import { AppToaster } from "@/components/ui/app-toaster"
import { getConversation, rebindConnectionOwnerWindow } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
import type { AgentType, ConversationDetail } from "@/lib/types"
import { RemoteConnectionGate } from "@/contexts/remote-connection-context"

const TOAST_DURATION_MS = 6000

function parseAgentType(raw: string | null): AgentType | null {
  if (!raw) return null
  const allowed: AgentType[] = [
    "claude_code",
    "codex",
    "open_code",
    "gemini",
    "cline",
    "hermes",
    "code_buddy",
    "kimi_code",
    "pi",
    "grok",
    "cursor",
  ]
  return (allowed as string[]).includes(raw) ? (raw as AgentType) : null
}

function ConversationPageInner() {
  const t = useTranslations("ConversationPopout")
  const searchParams = useSearchParams()
  const conversationId = Number(searchParams.get("conversationId") ?? "0")
  const folderId = Number(searchParams.get("folderId") ?? "0")
  const operationId = searchParams.get("operationId") ?? ""
  const agentType = parseAgentType(searchParams.get("agentType"))

  const [conversation, setConversation] = useState<ConversationDetail | null>(
    null
  )
  const [error, setError] = useState<string | null>(null)
  const [ready, setReady] = useState(false)

  const valid =
    Number.isFinite(conversationId) &&
    conversationId > 0 &&
    Number.isFinite(folderId) &&
    folderId > 0 &&
    !!agentType &&
    operationId.length > 0

  useEffect(() => {
    if (!valid || !agentType) return
    let cancelled = false
    getConversation(agentType, String(conversationId))
      .then((c) => {
        if (!cancelled) {
          setConversation(c)
          setError(null)
        }
      })
      .catch((err) => {
        if (!cancelled) {
          setConversation(null)
          setError(toErrorMessage(err))
        }
      })
    return () => {
      cancelled = true
    }
  }, [valid, conversationId, agentType])

  // Handoff: try rebind if live connection, then emit ready
  useEffect(() => {
    if (!valid || !conversation || ready) return
    let cancelled = false
    ;(async () => {
      let ownershipGeneration: number | undefined
      try {
        const result = await rebindConnectionOwnerWindow({
          conversationId,
          fromOwnerWindow: "main",
          toOwnerWindow: `conversation-${conversationId}`,
          operationId,
        })
        ownershipGeneration = result.ownershipGeneration
      } catch {
        // Cold session: no live connection to rebind — still ready
      }
      if (cancelled) return
      try {
        const { emit } = await import("@tauri-apps/api/event")
        await emit("conversation-window://ready", {
          conversationId,
          operationId,
          ownershipGeneration: ownershipGeneration ?? null,
        })
      } catch (e) {
        console.error("[ConversationPopout] emit ready failed", e)
      }
      if (!cancelled) setReady(true)
    })()
    return () => {
      cancelled = true
    }
  }, [valid, conversation, ready, conversationId, operationId])

  const title = useMemo(() => {
    if (!conversation) return t("title")
    return conversation.title?.trim() || t("untitled")
  }, [conversation, t])

  useEffect(() => {
    document.title = `${title} - codeg`
  }, [title])

  const closeWindow = useCallback(async () => {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window")
      await getCurrentWindow().close()
    } catch (err) {
      console.error("[ConversationPopout] close failed", err)
    }
  }, [])

  return (
    <div className="flex h-screen flex-col overflow-hidden bg-background text-foreground">
      <AppTitleBar
        center={
          <div className="text-sm font-semibold tracking-tight">{title}</div>
        }
      />
      <main className="flex min-h-0 flex-1 flex-col p-3">
        {!valid ? (
          <div className="rounded-lg border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {t("invalidParams")}
          </div>
        ) : error ? (
          <div className="rounded-lg border border-destructive/50 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            {error}
          </div>
        ) : !conversation || !ready ? (
          <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
            {t("loading")}
          </div>
        ) : (
          <div className="flex min-h-0 flex-1 flex-col gap-2">
            <p className="text-sm text-muted-foreground">
              {t("detachedHint", {
                agent: agentType ?? "",
                id: String(conversationId),
              })}
            </p>
            <p className="text-xs text-muted-foreground">
              {t("fullSurfaceFollowUp")}
            </p>
            <button
              type="button"
              className="self-start rounded-md border px-3 py-1.5 text-sm"
              onClick={() => void closeWindow()}
            >
              {t("close")}
            </button>
          </div>
        )}
      </main>
      <AppToaster duration={TOAST_DURATION_MS} />
    </div>
  )
}

export default function ConversationPage() {
  return (
    <RemoteConnectionGate>
      <Suspense
        fallback={
          <div className="flex h-screen items-center justify-center text-sm text-muted-foreground">
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          </div>
        }
      >
        <ConversationPageInner />
      </Suspense>
    </RemoteConnectionGate>
  )
}
