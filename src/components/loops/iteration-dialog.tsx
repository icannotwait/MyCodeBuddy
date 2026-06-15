"use client"

/**
 * Viewer for a single loop iteration's agent session. Opened from the iteration
 * list (any iteration) or a `question` inbox card (one awaiting an answer), it
 * shows the iteration's transcript and — while the iteration is still running —
 * its live stream, plus the live `AskQuestionCard` so a person can answer the
 * iteration's `ask_user_question`.
 *
 * Live attach is self-discovered: on open the dialog asks the backend whether a
 * LIVE engine-owned connection is bound to this `conversationId` (the engine
 * binds it when it sends the iteration's briefing prompt, so a running iteration
 * is discoverable). If one exists it attaches read-only via `connectAsViewer`
 * (torn down with `disconnect`, which only detaches a viewer — it never kills the
 * engine's agent) and bridges the live stream in with the shared
 * `child-session-hooks`. If none exists (a settled iteration, or one opened
 * before its connection bound) it falls back to the persisted transcript. The
 * answer flows through `answerQuestion` on the discovered connection; the
 * backend's `QuestionResolved` then clears the inbox card.
 *
 * The agent type is the conversation's own (authoritative, from its summary);
 * the optional `agentType` prop is a fast-path hint shown while the summary
 * loads — e.g. a question card carries it in its payload.
 */

import { useCallback, useEffect, useState } from "react"
import { useTranslations } from "next-intl"

import { AgentIcon } from "@/components/agent-icon"
import { Button } from "@/components/ui/button"
import { useLoopNav } from "@/hooks/use-loop-nav"
import type { IterationIssueContext } from "@/components/loops/loop-overlays-context"
import { MessageListView } from "@/components/message/message-list-view"
import {
  useChildConnectionState,
  useChildLiveBridge,
} from "@/components/message/child-session-hooks"
import { AskQuestionCard } from "@/components/chat/ask-question-card"
import { PermissionDialog } from "@/components/chat/permission-dialog"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog"
import { useConversationDetail } from "@/hooks/use-conversation-detail"
import { useConversationRuntime } from "@/contexts/conversation-runtime-context"
import { useAcpActions } from "@/contexts/acp-connections-context"
import { acpFindConnectionForConversation } from "@/lib/api"
import { AGENT_LABELS, type AgentType, type QuestionAnswer } from "@/lib/types"

interface Props {
  open: boolean
  onOpenChange: (open: boolean) => void
  conversationId: number
  /**
   * Optional agent-type hint (e.g. from a question card payload) shown while the
   * conversation summary — the authoritative source — loads. Omit it and the
   * dialog derives the agent type from the conversation itself.
   */
  agentType?: AgentType | null
  /** Issue identity from the opener; when present the header shows it and offers
   *  an "open issue" back-link. */
  issueContext?: IterationIssueContext | null
}

export function IterationDialog({
  open,
  onOpenChange,
  conversationId,
  agentType,
  issueContext,
}: Props) {
  const t = useTranslations("Loops.iteration")

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        closeButtonClassName="top-2 right-2"
        className="flex h-[85vh] w-full max-w-3xl flex-col gap-0 overflow-hidden rounded-2xl p-0 lg:max-w-4xl"
      >
        <DialogTitle className="sr-only">{t("title")}</DialogTitle>
        <DialogDescription className="sr-only">
          {t("description")}
        </DialogDescription>
        {open && conversationId > 0 ? (
          <IterationSessionBody
            conversationId={conversationId}
            agentTypeHint={agentType ?? null}
            issueContext={issueContext ?? null}
            onClose={() => onOpenChange(false)}
          />
        ) : null}
      </DialogContent>
    </Dialog>
  )
}

function IterationSessionBody({
  conversationId,
  agentTypeHint,
  issueContext,
  onClose,
}: {
  conversationId: number
  agentTypeHint: AgentType | null
  issueContext: IterationIssueContext | null
  onClose: () => void
}) {
  const t = useTranslations("Loops.iteration")
  const tStage = useTranslations("Loops.stage")
  const { gotoIssue } = useLoopNav()
  const { connectAsViewer, disconnect, answerQuestion, respondPermission } =
    useAcpActions()
  const { refetchDetail, setLiveOwnsActiveTurn } = useConversationRuntime()

  // Single persisted-detail fetch on mount, `preserveLive: true` so a bridged
  // reply is never wiped (the projection dedups against the persisted copy).
  useEffect(() => {
    refetchDetail(conversationId, { preserveLive: true })
  }, [conversationId, refetchDetail])

  const { loading, error, acpLoadError, detail } = useConversationDetail(
    conversationId,
    { enabled: false }
  )

  // Authoritative agent type is the conversation's own; the caller's hint covers
  // the brief window before the summary loads.
  const agentType: AgentType | null =
    detail?.summary.agent_type ?? agentTypeHint

  // Discover the engine-owned LIVE connection bound to this conversation. The
  // engine binds conversation_id when it sends the iteration's briefing prompt,
  // so a running iteration is discoverable here; a settled one resolves to null
  // and we read the persisted transcript instead. The primary lookup is by
  // conversation_id — agentType only seeds the unused session_id fallback — so we
  // run discovery immediately without waiting for the summary.
  const [liveConnId, setLiveConnId] = useState<string | null>(null)
  useEffect(() => {
    let cancelled = false
    void acpFindConnectionForConversation(
      conversationId,
      undefined,
      agentTypeHint ?? "claude_code"
    )
      .then((info) => {
        if (!cancelled) setLiveConnId(info?.connection_id ?? null)
      })
      .catch(() => {
        if (!cancelled) setLiveConnId(null)
      })
    return () => {
      cancelled = true
    }
    // Discovery is keyed on the conversation only; agentTypeHint is not part of
    // the primary lookup and changing it must not re-run discovery.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [conversationId])

  // Attach read-only once we have BOTH a live connection and a resolved agent
  // type, so the viewer parses/renders events with the right agent (avoids a
  // wrong-agent attach + re-attach flicker). Detach — never disconnect the
  // engine's agent — on close.
  useEffect(() => {
    if (!liveConnId || !agentType) return
    void connectAsViewer(liveConnId, liveConnId, agentType, null)
    return () => {
      void disconnect(liveConnId)
    }
  }, [liveConnId, agentType, connectAsViewer, disconnect])

  const conn = useChildConnectionState(liveConnId)
  const connStatus = conn?.status ?? null
  const isStreaming = connStatus === "prompting"

  // Viewer mode for this conversation: while a live connection is attached, strip
  // the persisted copy of the active reply so the stream never duplicates. With
  // no live connection (a settled iteration), keep the persisted transcript
  // whole. No kickoff text either: the iteration's user turn (its briefing) is
  // persisted.
  useEffect(() => {
    setLiveOwnsActiveTurn(conversationId, liveConnId != null, null)
  }, [conversationId, liveConnId, setLiveOwnsActiveTurn])

  const detailLoading = isStreaming ? false : loading

  useChildLiveBridge(conversationId, conn)

  const pendingPermission = conn?.pendingPermission ?? null
  const pendingAsk = conn?.pendingAskQuestion ?? null

  const onRespondPermission = useCallback(
    (requestId: string, optionId: string) => {
      if (!liveConnId) return
      void respondPermission(liveConnId, requestId, optionId)
    },
    [liveConnId, respondPermission]
  )

  const onAnswerAsk = useCallback(
    (questionId: string, answer: QuestionAnswer) => {
      if (!liveConnId) return
      return answerQuestion(liveConnId, questionId, answer)
    },
    [liveConnId, answerQuestion]
  )

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-3 border-b border-border px-5 py-2.5 pr-12">
        <span className="inline-flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border bg-background text-foreground">
          {agentType ? (
            <AgentIcon agentType={agentType} className="h-4 w-4" />
          ) : (
            <span className="h-2 w-2 rounded-sm bg-muted-foreground/60" />
          )}
        </span>
        <div className="min-w-0 flex-1">
          <div className="truncate text-sm font-semibold text-foreground">
            {agentType ? AGENT_LABELS[agentType] : t("title")}
          </div>
          {issueContext && (
            <div className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground">
              <span className="font-mono">#{issueContext.issueSeq}</span>
              {issueContext.stage && (
                <span className="rounded bg-muted px-1.5 py-0.5">
                  {tStage(issueContext.stage)}
                </span>
              )}
            </div>
          )}
        </div>
        {issueContext && (
          <Button
            size="sm"
            variant="outline"
            className="h-7 shrink-0"
            onClick={() => {
              gotoIssue(issueContext.spaceId, issueContext.issueId)
              onClose()
            }}
          >
            {t("openIssue")}
          </Button>
        )}
      </div>
      <div className="min-h-0 flex-1 px-4 py-3">
        <MessageListView
          conversationId={conversationId}
          agentType={agentType ?? "claude_code"}
          connStatus={connStatus}
          isActive={false}
          detailLoading={detailLoading}
          detailError={error}
          acpLoadError={acpLoadError}
          hideEmptyState={false}
          showMessageNav={false}
        />
      </div>
      {(pendingPermission ||
        (pendingAsk && pendingAsk.questions.length > 0)) && (
        <div className="max-h-[60%] shrink-0 overflow-y-auto border-t border-border px-4 py-3">
          {pendingPermission && (
            <PermissionDialog
              permission={pendingPermission}
              onRespond={onRespondPermission}
            />
          )}
          {pendingAsk && pendingAsk.questions.length > 0 && liveConnId && (
            <AskQuestionCard question={pendingAsk} onAnswer={onAnswerAsk} />
          )}
        </div>
      )}
    </div>
  )
}
