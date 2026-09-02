"use client"

/**
 * Shared read-only live-transcript surface: the same `MessageListView` used by
 * the main conversation panel, without the input bar, send signal, or
 * reload/new-session handlers — plus the child connection's blocking prompts
 * that resolve WITHOUT driving a new turn (permission request, codeg-mcp
 * `ask_user_question`, Grok plan approval), answered through the viewed
 * connection id.
 *
 * Extracted from the delegation sub-agent dialog so other embeds (the work-task
 * transcript viewer) reuse the exact same streaming pipeline. The host owns the
 * chrome (Dialog/Drawer + header) and, when the viewed connection is not a
 * delegation child already attached by its parent, the host also owns the
 * `attachDelegationChild`/`detachDelegationChild` lifecycle.
 *
 * Streaming: while mounted, a runtime-bound provider sink mirrors accepted
 * canonical messages into `conversationId`, and the provider promotes admitted
 * completions in the same commit. Persistence also comes from the broker's DB
 * writes via `useConversationDetail`. On unmount the runtime session is dropped,
 * so the next mount starts from a fresh persisted fetch.
 */

import { useCallback, useEffect, useRef, useSyncExternalStore } from "react"

import {
  MessageListView,
  type ResolvedMessageGroup,
} from "@/components/message/message-list-view"
import { useConversationDetail } from "@/hooks/use-conversation-detail"
import {
  useConversationRuntimeActions,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import {
  useAcpActions,
  useConnectionStore,
  type ConnectionState,
} from "@/contexts/acp-connections-context"
import { PermissionDialog } from "@/components/chat/permission-dialog"
import { AskQuestionCard } from "@/components/chat/ask-question-card"
import { PlanApprovalCard } from "@/components/chat/plan-approval-card"
import {
  type AgentType,
  type PlanApprovalAnswer,
  type QuestionAnswer,
} from "@/lib/types"

export function useConnectionStateById(
  connectionId: string | null
): ConnectionState | undefined {
  const store = useConnectionStore()
  const subscribe = useCallback(
    (cb: () => void) => {
      if (!connectionId) return () => {}
      return store.subscribeKey(connectionId, cb)
    },
    [store, connectionId]
  )
  const getSnapshot = useCallback(
    () => (connectionId ? store.getConnection(connectionId) : undefined),
    [store, connectionId]
  )
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

/**
 * Bridge the provider's accepted canonical `liveMessage` into the runtime
 * session for `conversationId`, so the read-only `MessageListView` sees
 * streaming turns and provider-admitted completions while the viewer is open.
 * Binding the runtime id prevents another alias's terminal from projecting into
 * this viewer.
 *
 * **Close-mid-stream / reopen-after-complete.** The viewer owns this runtime
 * session, so its full unmount drops the session and forces the next open to
 * fetch persisted detail from scratch.
 *
 * The detail-fetch no longer races the streaming bridge: the viewer's mount
 * fetch uses `preserveLive: true`, so `FETCH_DETAIL_SUCCESS` keeps the bridged
 * `liveMessage` instead of wiping it — no re-bridge effect is needed.
 *
 * **Reopen-after-completion.** If the provider admitted the completion while no
 * viewer sink was mounted, its accepted message marker allows the retained
 * final `liveMessage` to be adopted while persisted detail catches up. Raw or
 * status-only events cannot authorize this path.
 */
export function useLiveTranscriptBridge(
  conversationId: number,
  connState: ConnectionState | undefined
) {
  const { setLiveMessage, syncTurnMetadata, removeConversation } =
    useConversationRuntimeActions()
  const { registerLiveSinks } = useAcpActions()

  const connectionId = connState?.connectionId ?? null
  const acceptedCompletionMessageId = (
    connState?.acceptedCompletionRuntimeConversationIds ?? []
  ).includes(conversationId)
    ? (connState?.acceptedCompletionMessageId ?? null)
    : null

  // Backfill token usage / duration / model into the promoted reply once the
  // session's persisted transcript catches up. `completeTurn` lands the
  // streamed reply WITHOUT those fields — `buildStreamingTurnsFromLiveMessage`
  // carries no usage data; it comes from the DB parser — so without this the
  // post-stream stats row stays blank. Mirrors `conversation-detail-panel.tsx`:
  // a delayed, self-retrying DB roundtrip that PATCHes metadata onto the
  // existing `localTurns` (it never replaces them, so the kept live reply is
  // not blanked, unlike a `refetchDetail`). Cancel the previous sync before
  // starting a new one, and on viewer close, via the ref.
  const syncCancelRef = useRef<(() => void) | null>(null)
  const startMetadataSync = useCallback(() => {
    if (conversationId <= 0) return
    syncCancelRef.current?.()
    syncCancelRef.current = syncTurnMetadata(conversationId)
  }, [conversationId, syncTurnMetadata])

  // Provider commit owns both canonical mirroring and turn completion. Binding
  // the runtime id lets terminal projection exclude aliases on another turn.
  useEffect(() => {
    if (!connectionId) return
    return registerLiveSinks(connectionId, {
      runtimeConversationId: conversationId,
      canonical: (message, isLive, deliveryIds) => {
        setLiveMessage(conversationId, message, isLive, deliveryIds)
        return (
          useConversationRuntimeStore
            .getState()
            .byConversationId.get(conversationId)?.liveMessage === message
        )
      },
    })
  }, [connectionId, conversationId, registerLiveSinks, setLiveMessage])

  const syncedCompletionRef = useRef<string | null>(null)
  useEffect(() => {
    if (
      acceptedCompletionMessageId == null ||
      syncedCompletionRef.current === acceptedCompletionMessageId
    ) {
      return
    }
    syncedCompletionRef.current = acceptedCompletionMessageId
    startMetadataSync()
  }, [acceptedCompletionMessageId, startMetadataSync])

  // Full teardown on viewer close: cancel any in-flight metadata sync, then
  // drop the runtime session so the next open starts from a fresh
  // `fetchDetail` instead of stale bridged state.
  useEffect(() => {
    return () => {
      syncCancelRef.current?.()
      syncCancelRef.current = null
      removeConversation(conversationId)
    }
  }, [conversationId, removeConversation])
}

interface LiveTranscriptViewProps {
  conversationId: number
  /** Live connection to mirror from; null renders the persisted transcript
   *  only. The host attaches/detaches non-delegation connections itself. */
  connectionId: string | null
  agentType: AgentType | null
  /**
   * The kickoff prompt text, known synchronously by the host. Surfaced so the
   * kickoff user turn can be shown immediately while the persisted transcript
   * still lags the live stream (agent CLIs write their JSONL asynchronously).
   */
  kickoffText?: string | null
  /** Pure per-user-turn phase label (see `MessageListView.userTurnHeader`). */
  userTurnHeader?: ((group: ResolvedMessageGroup) => string | null) | null
}

export function LiveTranscriptView({
  conversationId,
  connectionId,
  agentType,
  kickoffText,
  userTurnHeader = null,
}: LiveTranscriptViewProps) {
  const conn = useConnectionStateById(connectionId)
  const connStatus = conn?.status ?? null
  const isStreaming = connStatus === "prompting"

  const { refetchDetail, setLiveOwnsActiveTurn } =
    useConversationRuntimeActions()

  // Enter viewer mode: mark the session live-owned and record the known
  // kickoff text. `getTimelineTurns` then (a) synthesizes the kickoff user
  // turn from this text while the persisted transcript still lags the live
  // stream, so the user message shows immediately, and (b) strips the
  // persisted copy of the reply while the live/local reply is present, so it
  // never duplicates the stream. Re-applies if `kickoffText` resolves late
  // (harmless).
  useEffect(() => {
    setLiveOwnsActiveTurn(conversationId, true, kickoffText ?? null)
  }, [conversationId, kickoffText, setLiveOwnsActiveTurn])

  // Single persisted-detail fetch on mount, always `preserveLive: true` so the
  // bridged/promoted reply is never wiped — the render-time projection above
  // handles dedup against the persisted copy. No settle-time refetch: when the
  // session finishes, `completeTurn` promotes its (complete) live reply into
  // localTurns, which the projection keeps showing; replacing it from the DB
  // would race the still-lagging transcript and could blank the reply.
  useEffect(() => {
    refetchDetail(conversationId, { preserveLive: true })
  }, [conversationId, refetchDetail])

  // Reader only — its built-in auto-fetch is disabled; the effect above is
  // the sole fetch path.
  const { loading, error, acpLoadError } = useConversationDetail(
    conversationId,
    { enabled: false }
  )

  // While streaming, mask loading as false: the live bridge owns the reply and
  // the synthesized kickoff covers the user turn, so we don't want a skeleton
  // over the live stream. Passed to MessageListView only.
  const detailLoading = isStreaming ? false : loading

  useLiveTranscriptBridge(conversationId, conn)

  // The session runs with the user's configured permission level, so it may
  // raise a permission request; it may also call the codeg-mcp
  // `ask_user_question` tool or (Grok) `exit_plan_mode`. All three are
  // blocking prompts resolved WITHOUT driving a new turn — route the answers
  // through the viewed connection id. The legacy free-text `pendingQuestion`
  // path is intentionally NOT hosted here: it is answered by sending a prompt,
  // which a read-only viewer deliberately cannot do.
  const { respondPermission, answerQuestion, answerPlanApproval } =
    useAcpActions()
  const pendingPermission = conn?.pendingPermission ?? null
  const onRespondPermission = useCallback(
    (requestId: string, optionId: string) => {
      if (!connectionId) return
      void respondPermission(connectionId, requestId, optionId)
    },
    [connectionId, respondPermission]
  )

  const pendingAskQuestion = conn?.pendingAskQuestion ?? null
  const onAnswerAskQuestion = useCallback(
    (questionId: string, answer: QuestionAnswer) => {
      if (!connectionId) return
      return answerQuestion(connectionId, questionId, answer)
    },
    [connectionId, answerQuestion]
  )

  const pendingPlanApproval = conn?.pendingPlanApproval ?? null
  const onAnswerPlanApproval = useCallback(
    (approvalId: string, answer: PlanApprovalAnswer) => {
      if (!connectionId) return
      return answerPlanApproval(connectionId, approvalId, answer)
    },
    [connectionId, answerPlanApproval]
  )

  return (
    // Sized as a flex child: fills the host column's remaining height under
    // its header (h-full here would overflow by the header's height).
    <div className="flex min-h-0 flex-1 flex-col">
      {pendingPermission && (
        <div className="border-b border-border px-4 py-3">
          <PermissionDialog
            permission={pendingPermission}
            onRespond={onRespondPermission}
          />
        </div>
      )}
      {connectionId &&
        pendingAskQuestion &&
        pendingAskQuestion.questions.length > 0 && (
          <div className="border-b border-border px-4 py-3">
            <AskQuestionCard
              question={pendingAskQuestion}
              onAnswer={onAnswerAskQuestion}
            />
          </div>
        )}
      {connectionId && pendingPlanApproval && (
        <div className="border-b border-border px-4 py-3">
          <PlanApprovalCard
            key={pendingPlanApproval.approval_id}
            approval={pendingPlanApproval}
            onAnswer={onAnswerPlanApproval}
          />
        </div>
      )}
      {/* No padding of its own. `MessageListView` insets its own content —
          every virtualized row is wrapped in `mx-auto max-w-3xl px-4` and the
          virtualizer adds 16px above the first row and below the last — so a
          padded wrapper here doubled it, and in a panel this narrow the two
          layers cost the transcript a visible chunk of its width. This is
          exactly how the main conversation panel mounts the same component. */}
      <div className="min-h-0 flex-1">
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
          userTurnHeader={userTurnHeader}
        />
      </div>
    </div>
  )
}
