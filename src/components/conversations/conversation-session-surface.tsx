"use client"

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslations } from "next-intl"
import { toast } from "sonner"
import { AlertCircle, Loader2, Plus, RefreshCw } from "lucide-react"
import {
  getCachedSelectors,
  useAcpActions,
  useAcpEvent,
  useConnectionStore,
} from "@/contexts/acp-connections-context"
import { useAcpAgents } from "@/hooks/use-acp-agents"
import { useAppWorkspaceStore } from "@/stores/app-workspace-store"
import { useTabActions, useTabStore } from "@/contexts/tab-context"
import { isReparentUnmount } from "@/stores/tab-store"
import { randomUUID } from "@/lib/utils"
import { useConnectionLifecycle } from "@/hooks/use-connection-lifecycle"
import { useMessageQueue, type QueuedMessage } from "@/hooks/use-message-queue"
import { MessageListView } from "@/components/message/message-list-view"
import {
  GoalControlProvider,
  type GoalControlValue,
} from "@/components/message/goal-control-context"
import { useInitialHistoryScrollEligibility } from "@/components/message/initial-history-scroll-controller"
import { ConversationShell } from "@/components/chat/conversation-shell"
import { DelegateAccessStatus } from "@/components/chat/delegate-access-status"
import { SessionConfigStaleBanner } from "@/components/chat/session-config-stale-banner"
import { ToolWatchdogBanner } from "@/components/conversations/tool-watchdog-banner"
import { DelegationRouteNotice } from "@/components/chat/delegation-route-notice"
import { BackgroundTasksChip } from "@/components/chat/background-tasks-chip"
import { FeedbackNotesDisplay } from "@/components/chat/feedback-notes-display"
import { FeedbackDialog } from "@/components/chat/feedback-dialog"
import { AgentDiagnosticsDialog } from "@/components/settings/agent-diagnostics-dialog"
import { useFeedbackEnabled } from "@/hooks/use-feedback-enabled"
import { useSessionFeedback } from "@/hooks/use-session-feedback"
import { AgentSelector } from "@/components/chat/agent-selector"
import { ChatInput } from "@/components/chat/chat-input"
import { WelcomeHero, WelcomeTip } from "@/components/chat/welcome-hero"
import { QuickActions } from "@/components/chat/quick-actions"
import { ScrollArea } from "@/components/ui/scroll-area"
import type { ComposerInjectContent } from "@/components/chat/message-input"
import {
  acpFork,
  createChatConversation,
  createChatDir,
  createConversation,
  openSettingsWindow,
} from "@/lib/api"
import {
  flushRetryDelayMs,
  forkSendBlockedByQueue,
  isConnectionReady,
  shouldQueueDirectSend,
  shouldRejectDuplicateCreate,
} from "@/lib/queue-flush"
import {
  shouldClearTerminalDisconnectLatch,
  shouldLatchTerminalDisconnect,
  type TerminalDisconnectLatch,
} from "@/lib/terminal-reconnect"
import { TurnBusyError } from "@/lib/turn-busy"
import { continuationFailureI18nKey } from "@/lib/continuation-waiting"
import { consumeDelegatedChildTabIntent } from "@/lib/delegated-child-tab-intent"
import {
  completeLiveTranscriptTurn,
  useConversationRuntimeActions,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import { createLiveTranscriptFrameSink } from "@/stores/live-transcript-store"
import { useShallow } from "zustand/react/shallow"
import { useConversationDetail } from "@/hooks/use-conversation-detail"
import {
  extractUserImagesFromDraft,
  getPromptDraftDisplayText,
} from "@/lib/prompt-draft"
import {
  type AgentType,
  type ContentBlock,
  type DbConversationSummary,
  type DelegateAccessState,
  type EventEnvelope,
  type MessageTurn,
  type PlanApprovalAnswer,
  type PromptDraft,
  type QuestionAnswer,
  type UserMessageBlock,
} from "@/lib/types"
import { getAgentLabel } from "@/lib/custom-agents"
import type { ConnectionIntent } from "@/contexts/acp-connections-context"
import { useDelegateAccess } from "@/hooks/use-delegate-access"
import { isDelegateViewerOnlyRejection } from "@/lib/delegate-access"
import {
  getSavedModeId,
  saveModePreference,
} from "@/lib/selector-prefs-storage"
import {
  buildConversationDraftStorageKey,
  buildNewConversationDraftStorageKey,
  clearMessageInputDraft,
  saveMessageInputDraft,
} from "@/lib/message-input-draft"
import type { PromptDraftRestore } from "@/components/chat/message-input"

const ROOT_ORCHESTRATION_RESUME_PROMPT =
  "Continue root orchestration from the durable workflow state."

/**
 * Durable auto-connect policy for a session surface root.
 *
 * Fail-closed for a persisted conversation whose workspace summary is missing
 * or `cancelled`, and whenever a terminal reconnect latch is armed. Drafts
 * (no db id) stay open unless latched. Explicit reconnect is separate.
 */
export function resolveSessionAutoConnectAllowed(args: {
  hasPersistedConversation: boolean
  persistedSummary: Pick<DbConversationSummary, "status"> | null
  terminalDisconnectLatch: TerminalDisconnectLatch | null
}): boolean {
  if (args.terminalDisconnectLatch != null) return false
  if (!args.hasPersistedConversation) return true
  if (args.persistedSummary == null) return false
  if (args.persistedSummary.status === "cancelled") return false
  return true
}

/**
 * Prefer the workspace root summary; fall back to detail summary so delegated
 * children excluded from the root list still resolve durable policy.
 */
export function resolveSurfacePersistedSummary(
  root: DbConversationSummary | null,
  detail: DbConversationSummary | null
): DbConversationSummary | null {
  return root ?? detail
}

/**
 * Map fail-closed delegate access onto lifecycle connection intent + the
 * independent main-tab interaction lock.
 */
export function resolveDelegateConnectionPolicy(args: {
  isDelegate: boolean
  access: DelegateAccessState
}): {
  interactionLocked: boolean
  intent: ConnectionIntent
  retryObserverDiscovery: boolean
} {
  const interactionLocked =
    args.isDelegate && args.access.mode === "viewer_only"
  return {
    interactionLocked,
    intent: interactionLocked ? "observe_existing" : "own_or_observe",
    retryObserverDiscovery:
      interactionLocked && args.access.reason === "task_running",
  }
}

/**
 * Surface event → latch/pause transition.
 *
 * First arm captures `summary.updated_at`. If a prior latch is already armed
 * but the delivery-time summary would clear it (newer non-cancelled root),
 * re-arm immediately with that newer baseline so a terminal event for Y cannot
 * be lost to the passive clear effect. Stale/same-baseline and newer cancelled
 * summaries preserve the existing baseline.
 */
export function applyTerminalDisconnectEvent(
  state: {
    latch: TerminalDisconnectLatch | null
    queuePaused: boolean
  },
  event: EventEnvelope,
  connectionId: string | null,
  summary: Pick<DbConversationSummary, "status" | "updated_at"> | null
): { latch: TerminalDisconnectLatch | null; queuePaused: boolean } {
  if (!shouldLatchTerminalDisconnect(event, connectionId, summary)) {
    return state
  }
  const nextBaseline = {
    baselineUpdatedAt: summary!.updated_at,
  } satisfies TerminalDisconnectLatch
  if (state.latch == null) {
    return { latch: nextBaseline, queuePaused: true }
  }
  // Prior latch eligible to clear against delivery summary → treat as new arm.
  if (shouldClearTerminalDisconnectLatch(state.latch, summary)) {
    return { latch: nextBaseline, queuePaused: true }
  }
  // Stale / same-baseline / cancelled — keep the original first-arm baseline.
  return { latch: state.latch, queuePaused: true }
}

/** Clear reconnect latch from an authoritative workspace summary (not queue pause). */
export function applyPersistedSummaryToTerminalLatch(
  latch: TerminalDisconnectLatch | null,
  summary: Pick<DbConversationSummary, "status" | "updated_at"> | null
): TerminalDisconnectLatch | null {
  if (shouldClearTerminalDisconnectLatch(latch, summary)) return null
  return latch
}

/**
 * Explicit Reconnect affordance: cancelled or latched root, and ACP is not live.
 * When `sessionIdentityReady` is false, hide the control so a persisted
 * non-cline historical tab cannot invite reconnect before external identity
 * resolves (session/new would orphan prior context).
 */
export function shouldShowTerminalReconnect(args: {
  rootCancelled: boolean
  terminalDisconnectLatch: TerminalDisconnectLatch | null
  connStatus: string | null
  /**
   * False while a persisted non-cline root still lacks a resumable external
   * session id. Drafts and cline leave this true. Defaults to true so pure
   * unit callers that omit identity stay focused on cancelled/latch/status.
   */
  sessionIdentityReady?: boolean
}): boolean {
  if (args.sessionIdentityReady === false) return false
  const cancelledOrLatched =
    args.rootCancelled || args.terminalDisconnectLatch != null
  if (!cancelledOrLatched) return false
  return (
    args.connStatus == null ||
    args.connStatus === "disconnected" ||
    args.connStatus === "error"
  )
}

/**
 * Whether explicit reconnect may invoke connect for this root.
 *
 * Persisted non-cline sessions require a resolved resumable external id
 * (detail.summary.external_id or runtime externalId). Without it, connect
 * falls through to session/new and orphans historical context. Drafts and
 * cline do not need a resumable identity (cline cannot resume).
 */
export function canExplicitReconnectWithSessionIdentity(args: {
  hasPersistedConversation: boolean
  isCline: boolean
  externalSessionId: string | null | undefined
}): boolean {
  if (!args.hasPersistedConversation) return true
  if (args.isCline) return true
  return (
    typeof args.externalSessionId === "string" &&
    args.externalSessionId.length > 0
  )
}

export interface ConversationSessionSurfaceProps {
  tabId: string
  conversationId: number | null
  /** Props-driven folder id (not resolved only via tab store). */
  folderId: number
  agentType: AgentType
  workingDir?: string
  isActive: boolean
  /** Drive the composer's flowing active-session border. True only for the
   *  active tab while tiled across multiple sessions — the one place the flow
   *  serves as the "which tile is active" cue. Distinct from `isActive`, which
   *  also governs auto-focus/connect and is true even for a lone session. */
  showActiveFlow: boolean
  reloadSignal: number
  /** Split-group owner used to fence transient reparent unmounts. */
  groupId?: string | null
  /**
   * Detached pop-out operation id. When set (cold path after commit-ack),
   * ACP connect stamps the incarnation so window-close can reap it.
   */
  ownerOperationId?: string | null
}

function buildOptimisticUserTurnFromDraft(
  draft: PromptDraft,
  attachedResourcesFallback: string
): MessageTurn {
  // `draft.displayText` is the composer's full Markdown, which already renders
  // every inline file/resource badge as a `[label](uri)` link (see
  // `referenceToMarkdown`). Re-appending the resource blocks here would duplicate
  // each attached file in the optimistic bubble, so the display text is used
  // as-is — images are the only out-of-band content left to add as blocks.
  const text = getPromptDraftDisplayText(draft, attachedResourcesFallback)

  const blocks: ContentBlock[] = []
  for (const image of extractUserImagesFromDraft(draft)) {
    blocks.push({
      type: "image",
      data: image.data,
      mime_type: image.mime_type,
      uri: image.uri ?? null,
    })
  }
  blocks.push({ type: "text", text })

  return {
    id: `optimistic-${randomUUID()}`,
    role: "user",
    blocks,
    timestamp: new Date().toISOString(),
  }
}

/** Build a user `MessageTurn` from a broadcast `user_message` (event or
 *  snapshot `pending_user_message`). Used by cross-client VIEWERS to render the
 *  sender's prompt. The turn `id` is the broadcast `message_id` so the runtime
 *  reducer can dedup it idempotently. */
function buildUserTurnFromMessageBlocks(
  messageId: string,
  blocks: UserMessageBlock[]
): MessageTurn {
  const contentBlocks: ContentBlock[] = blocks.map((b) =>
    b.type === "image"
      ? { type: "image", data: b.data, mime_type: b.mime_type, uri: null }
      : { type: "text", text: b.text }
  )
  return {
    id: messageId,
    role: "user",
    blocks: contentBlocks,
    timestamp: new Date().toISOString(),
  }
}

function buildVirtualConversationId(seed: string): number {
  let hash = 0
  for (let i = 0; i < seed.length; i += 1) {
    hash = (hash * 31 + seed.charCodeAt(i)) | 0
  }
  const normalized = Math.abs(hash) + 1
  return -normalized
}

export const ConversationSessionSurface = memo(
  function ConversationSessionSurface({
    tabId,
    conversationId,
    folderId: folderIdProp,
    agentType,
    workingDir,
    isActive,
    showActiveFlow,
    reloadSignal,
    groupId = null,
    ownerOperationId = null,
  }: ConversationSessionSurfaceProps) {
    // One-shot intent from openDelegatedChildSession (live ownership + turn focus).
    // Consumed on first surface mount for this conversation id only — not on
    // tab-store rehydrate/remote sync.
    const [delegatedOpenIntent] = useState(() => {
      const cid = conversationId ?? null
      if (cid == null || cid <= 0) return null
      return consumeDelegatedChildTabIntent(cid)
    })
    const [focusTurnAnchor] = useState<string | null>(
      () => delegatedOpenIntent?.focusTurnAnchor ?? null
    )
    // Freeze mount-time eligibility for uncached history scroll. Lazy useState
    // inside the hook keeps a draft (null) ineligible after first-send bind, and
    // keep-alive tab identity (key=tab.id) means active/inactive CSS and reloads
    // do not recreate the latch.
    const baseInitialHistoryScrollEligible =
      useInitialHistoryScrollEligibility(conversationId)
    // Selected-run open: suppress initial scroll-to-bottom so focusTurnAnchor wins.
    const initialHistoryScrollEligible =
      focusTurnAnchor != null ? false : baseInitialHistoryScrollEligible
    const t = useTranslations("Folder.conversation")
    const tWelcome = useTranslations("Folder.chat.welcomeInputPanel")
    const tDiag = useTranslations("DiagnosticsSettings")
    const sharedT = useTranslations("Folder.chat.shared")
    const tMessageList = useTranslations("Folder.chat.messageList")
    const refreshConversations = useAppWorkspaceStore(
      (s) => s.refreshConversations
    )
    const upsertFolder = useAppWorkspaceStore((s) => s.upsertFolder)
    // Subscribe to ONLY this tab's own row (identified by `tabId`), not the whole
    // `tabs` array — so a sibling tab changing, or a tab-switch (isActive rides in
    // as a prop), never re-renders this keep-alive panel. `find` returns the same
    // object reference across derives until this tab itself changes.
    const ownTab = useTabStore(
      (s) => s.tabs.find((tab) => tab.id === tabId) ?? null
    )
    // Prefer explicit props.folderId so detached pages do not depend on tab-store
    // row lookup for session identity. Fall back to the tab row when present.
    const ownFolderId =
      folderIdProp > 0 ? folderIdProp : (ownTab?.folderId ?? null)
    const folder = useAppWorkspaceStore((s) =>
      ownFolderId != null
        ? (s.allFolders.find((f) => f.id === ownFolderId) ?? null)
        : null
    )
    const folderId = ownFolderId ?? 0
    const {
      openTab,
      bindConversationTab,
      setChatDraftWorkingDir,
      setTabRuntimeConversationId,
      pinTab,
      openNewConversationTab,
      closeTab,
      confirmDraftAgent,
      setDraftAgentFromFallback,
    } = useTabActions()
    const {
      appendOptimisticTurn,
      removeOptimisticTurn,
      appendViewerUserTurn,
      reloadDetail,
      syncTurnMetadata,
      syncDelegateTerminalDetail,
      removeConversation,
      setAcpLoadError,
      setDbConversationId,
      setExternalId,
      setLiveMessage,
      setLiveOwnsActiveTurn,
      setPendingCleanup,
      setSyncState,
    } = useConversationRuntimeActions()
    const acpActions = useAcpActions()
    // Stable store API (ref-backed getConnection). Used at ACP event delivery so
    // same-bound-connection checks see the live map, not a stale render closure.
    // Provider updates the connection map before notifyRawSubscribers.
    const connectionStore = useConnectionStore()

    // Stable runtime session key — set once at mount, never changes.
    // For new conversations this is a virtual (negative) ID; for existing
    // conversations opened from the sidebar it equals the real DB ID.
    const [effectiveConversationId] = useState(
      () => conversationId ?? buildVirtualConversationId(`draft-${tabId}`)
    )
    const [createdConversationId, setCreatedConversationId] = useState<
      number | null
    >(null)
    const dbConversationId = conversationId ?? createdConversationId
    const [draftAgentType, setDraftAgentType] = useState<AgentType>(agentType)
    const selectedAgent = conversationId != null ? agentType : draftAgentType
    // Seed from localStorage so the React state reflects the user's saved
    // mode for this agent immediately on mount. Without this seed, a reuse-
    // path connect (idle window after a refresh, before the agent is GC'd)
    // would silently fall back to whatever `current_mode_id` the backend
    // happens to be on: `handleModeChange` updates only React state and
    // localStorage, not the agent — the agent gets synced inside
    // `handleSend` by diffing `modeId` against `modes.current_mode_id`.
    // A null seed here means that diff is "agent default vs null", which
    // resolves the displayed mode through `conn.modes.current_mode_id`
    // and never triggers the catch-up `setMode`.
    const [modeId, setModeId] = useState<string | null>(() =>
      getSavedModeId(agentType)
    )
    const [sendSignal, setSendSignal] = useState(0)
    const [agentsLoaded, setAgentsLoaded] = useState(false)
    const [usableAgentCount, setUsableAgentCount] = useState(0)
    const [composerDiagnosticsOpen, setComposerDiagnosticsOpen] =
      useState(false)
    const [agentConnectError, setAgentConnectError] = useState<string | null>(
      null
    )
    // Direct-send rejection restore: MessageInput applies strictly newer revisions.
    const [promptDraftRestore, setPromptDraftRestore] =
      useState<PromptDraftRestore | null>(null)
    const promptDraftRestoreRevisionRef = useRef(0)
    // Cold continuation_failure toast: dedupe by (code, finished_at) per mount.
    const lastContinuationFailureKeyRef = useRef<string | null>(null)
    const tAcpConnections = useTranslations("Folder.chat.acpConnections")
    const [hasSentMessage, setHasSentMessage] = useState(false)
    const [
      retainWelcomeComposerForAdmission,
      setRetainWelcomeComposerForAdmission,
    ] = useState(false)
    const [quickActionInject, setQuickActionInject] =
      useState<ComposerInjectContent | null>(null)

    const hasPersistedConversation = dbConversationId != null

    // Authoritative root summary from the workspace store (not detail fetch).
    // Missing row is fail-closed for auto-connect until reconciliation lands.
    const persistedSummary = useAppWorkspaceStore((s) =>
      dbConversationId != null
        ? (s.conversations.find((row) => row.id === dbConversationId) ?? null)
        : null
    )
    const [terminalDisconnectLatch, setTerminalDisconnectLatch] =
      useState<TerminalDisconnectLatch | null>(null)
    const [
      queuePausedByTerminalDisconnect,
      setQueuePausedByTerminalDisconnect,
    ] = useState(false)
    // Authoritative pause flag for zero-delay auto-flush timer rechecks.
    // Updated SYNCHRONOUSLY with setState only in the two write paths
    // (terminal arm → true, Resume Queue → false). Do not mirror state→ref
    // in a passive effect: after Resume Queue commits false, a fresh terminal
    // arm can set the ref true before the stale false effect runs and
    // clobber it back to false, letting a scheduled flush dequeue history.
    const queuePausedByTerminalDisconnectRef = useRef(false)

    // A folderless chat draft before its first send (chat tab, not yet persisted).
    // Used to trigger the eager scratch-dir prepare below, which gives the draft a
    // real workingDir so the ACP connection can spawn BEFORE the first send — the
    // composer is gated on `connected` like any normal conversation (no offline
    // compose). Once bound it has a persisted row + workingDir and this is false.
    const isChatDraft = useMemo(
      () => ownTab?.isChat === true && !hasPersistedConversation,
      [ownTab, hasPersistedConversation]
    )

    // Expose the runtime session key to the tab so the aux panel (Diff sidebar)
    // can look up live turns even before the DB conversation is created.
    useEffect(() => {
      if (effectiveConversationId !== conversationId) {
        setTabRuntimeConversationId(tabId, effectiveConversationId)
      }
    }, [
      tabId,
      effectiveConversationId,
      conversationId,
      setTabRuntimeConversationId,
    ])

    // Clear pendingCleanup when tab is (re)opened
    useEffect(() => {
      setPendingCleanup(effectiveConversationId, false)
    }, [effectiveConversationId, setPendingCleanup])

    const latestReloadSignal = useRef(reloadSignal)
    const pendingReloadState = useRef<{
      signal: number
      sawLoading: boolean
    } | null>(null)
    const dbConvIdRef = useRef<number | null>(conversationId)
    const mountedRef = useRef(true)
    const selectedAgentRef = useRef(selectedAgent)
    const createConversationPendingRef = useRef(false)
    // Single-flight guard for the eager scratch-dir prepare (on chat-mode select).
    const prepareChatDirPendingRef = useRef(false)
    const sessionIdRef = useRef<string | null>(null)
    const syncCancelRef = useRef<(() => void) | null>(null)

    useEffect(() => {
      dbConvIdRef.current = dbConversationId
      // Bind the DB row id onto the runtime session when the two ids diverge
      // (draft-started tab: virtual runtime key, row created on first send).
      // `refetchDetail` on the runtime key fetches with this binding — without
      // it, a settle-driven refetch (background task finished) asks the backend
      // for the virtual id and silently fails, leaving stale live turns on
      // screen forever.
      if (
        dbConversationId != null &&
        dbConversationId !== effectiveConversationId
      ) {
        setDbConversationId(effectiveConversationId, dbConversationId)
      }
    }, [dbConversationId, effectiveConversationId, setDbConversationId])

    useEffect(() => {
      selectedAgentRef.current = selectedAgent
    }, [selectedAgent])

    // Eagerly create the chat-mode scratch dir the moment this becomes an unbound
    // chat draft, so the ACP connection can spawn at a real cwd BEFORE the first
    // send — picking "no-folder mode" no longer leaves the agent unconnected.
    // Filesystem-only (writes no DB rows), so the lazy-conversation invariant
    // holds; the first send reuses this dir via createChatConversation(existingDir),
    // keeping the connection's cwd put across the bind. Single-flight and
    // self-disarming: once workingDir lands the guard flips false. openChatModeTab
    // clears workingDir on re-entry, so a fresh dir is prepared each time.
    useEffect(() => {
      if (!isActive || !isChatDraft || workingDir) return
      if (prepareChatDirPendingRef.current) return
      prepareChatDirPendingRef.current = true
      void (async () => {
        try {
          const res = await createChatDir()
          if (mountedRef.current) {
            setChatDraftWorkingDir(tabId, res.path)
          }
        } catch (e) {
          // The composer is gated on a live connection (no offline compose), and
          // the connection needs this scratch dir. If the mkdir fails the draft
          // would otherwise sit with a permanently disabled composer and no
          // explanation — surface it on the welcome screen's error banner so the
          // user can re-enter chat mode to retry.
          console.error("[ConversationSessionSurface] prepare chat dir:", e)
          if (mountedRef.current) {
            setAgentConnectError(tWelcome("prepareSessionFailed"))
          }
        } finally {
          prepareChatDirPendingRef.current = false
        }
      })()
    }, [
      isActive,
      isChatDraft,
      workingDir,
      tabId,
      setChatDraftWorkingDir,
      tWelcome,
    ])

    // Sync the agentType prop into draftAgentType for draft tabs. The prop
    // changes when openNewConversationTab re-points an existing draft at a
    // different folder's default agent (or when any other external mutation
    // updates tab.agentType). Without this mirror, the local draftAgentType
    // would stay frozen at its mount value and the UI/connection would not
    // follow. Persisted conversations read agentType directly from the prop
    // via selectedAgent, so they are unaffected.
    useEffect(() => {
      if (conversationId != null) return
      if (agentType === selectedAgentRef.current) return
      setDraftAgentType(agentType)
      setModeId(getSavedModeId(agentType))
      setAgentConnectError(null)
    }, [agentType, conversationId])

    const {
      detail,
      loading: detailLoading,
      error: detailError,
      acpLoadError,
    } = useConversationDetail(effectiveConversationId)

    // Cold detail: surface a redacted continuation failure exactly once per
    // (code, finished_at) identity while this panel is mounted. Uses the same
    // i18n mapping as live ACP error events so copy cannot drift.
    useEffect(() => {
      const failure = detail?.continuation_failure
      if (!failure) return
      const identity = `${failure.code}|${failure.finished_at}`
      if (lastContinuationFailureKeyRef.current === identity) return
      lastContinuationFailureKeyRef.current = identity
      toast.error(tAcpConnections(continuationFailureI18nKey(failure.code)))
    }, [detail?.continuation_failure, tAcpConnections])

    // Subscribe to only the fields this panel actually reads from its runtime
    // session — NOT the whole session object. The live-message sink rewrites the
    // session object on every streaming batch (~60/s, via SET_LIVE_MESSAGE); a
    // whole-object selector here would re-render this keep-alive panel (and the
    // composer subtree it wraps) on every streaming token, even though none of
    // these three fields change mid-stream. `useShallow` keeps the returned slice
    // reference-stable across batches, so the panel re-renders only when one of
    // them actually changes. (message-list-view subscribes to the session's
    // liveMessage separately to render the live stream.)
    const {
      externalId: runtimeExternalId,
      syncState: runtimeSyncState,
      delegateSyncError,
    } = useConversationRuntimeStore(
      useShallow((s) => {
        const session = s.byConversationId.get(effectiveConversationId)
        return {
          externalId: session?.externalId ?? null,
          syncState: session?.syncState ?? "idle",
          delegateSyncError: session?.delegateSyncError ?? null,
        }
      })
    )

    // Two-source resolution for the session id passed to acp_connect:
    //   1. detail.summary.external_id — DB value, available for tabs opened
    //      from the sidebar (effectiveConversationId equals the real cid).
    //   2. runtimeExternalId — populated by the connSessionId effect
    //      below when SessionStarted fires. This is the ONLY source for tabs
    //      that started as a new conversation: their effectiveConversationId
    //      is locked to a virtual negative id (line 186 useState initializer
    //      runs once), useConversationDetail skips fetching for virtual ids,
    //      and detail stays null forever. Without this fallback, every
    //      reconnect on a new-conversation tab passes sessionId=undefined →
    //      backend takes session/new → DB.external_id is overwritten on the
    //      next prompt → original sid orphaned, agent loses prior context.
    const externalId =
      detail?.summary.external_id ?? runtimeExternalId ?? undefined
    // For persisted conversations opened from the sidebar, wait until the
    // session's external_id has been resolved before auto-connecting.
    // Otherwise the auto-connect effect fires with sessionId=undefined and
    // the backend falls back to session/new, orphaning the historical
    // context. Historical Cline rows also wait for detail when we do not yet
    // know they are a delegate open (fail-closed identity; see plan Minor 13)
    // — only a known delegated-open intent keeps the old immediate-connect
    // shortcut.
    const awaitingHistoricalSessionId =
      hasPersistedConversation &&
      detailLoading &&
      (selectedAgent !== "cline" || delegatedOpenIntent == null)
    // Install status of the currently selected agent. An agent can be enabled and
    // platform-available yet have no CLI/SDK installed; selecting one can never
    // connect. Rather than firing a doomed (and racy) auto-connect whose only
    // outcome is a transient "not installed" toast, we skip the connect and
    // surface a persistent install prompt instead (see composerBlockedMessage).
    const { agents: acpAgents } = useAcpAgents()
    const selectedAgentNotInstalled = useMemo(() => {
      const info = acpAgents.find((a) => a.agent_type === selectedAgent)
      return (
        info != null &&
        info.enabled &&
        info.available &&
        !info.installed_version
      )
    }, [acpAgents, selectedAgent])
    const canAutoConnect =
      (hasPersistedConversation || (agentsLoaded && usableAgentCount > 0)) &&
      !awaitingHistoricalSessionId &&
      // Skip the doomed auto-connect for a not-installed agent ONLY in the draft
      // surfaces, where the persistent install banner explains it instead. A
      // persisted conversation keeps its existing connect-and-surface-the-error
      // behavior (its agent can't be swapped from the picker anyway).
      !(selectedAgentNotInstalled && !hasPersistedConversation) &&
      !(hasPersistedConversation && detailError) &&
      !(hasPersistedConversation && acpLoadError)
    const draftStorageKey = useMemo(() => {
      if (dbConversationId != null) {
        return buildConversationDraftStorageKey(dbConversationId)
      }
      return buildNewConversationDraftStorageKey(tabId)
    }, [dbConversationId, tabId])
    // Use the per-tab workingDir (derived from the tab's own folderId by the
    // parent) rather than the active folder's path — otherwise switching tabs
    // briefly exposes the previous folder's path to the ACP auto-connect
    // effect, and the connection sticks with the wrong cwd.
    const workingDirForConnection = workingDir ?? folder?.path

    // Delegate identity + access: open intent OR durable detail kind.
    // Fail-closed access while loading is owned by useDelegateAccess.
    const isDelegateConversation =
      delegatedOpenIntent != null || detail?.summary.kind === "delegate"
    const {
      access: delegateAccess,
      loading: delegateAccessLoading,
      refresh: refreshDelegateAccess,
    } = useDelegateAccess({
      conversationId: dbConversationId,
      enabled: isDelegateConversation,
    })
    const delegatePolicy = resolveDelegateConnectionPolicy({
      isDelegate: isDelegateConversation,
      access: delegateAccess,
    })
    const interactionLocked = delegatePolicy.interactionLocked
    const interactionLockedRef = useRef(interactionLocked)
    interactionLockedRef.current = interactionLocked
    // Root workspace list excludes child rows — fall back to detail summary
    // so auto-connect / latch policy still sees a durable status.
    const summaryForSessionPolicy = resolveSurfacePersistedSummary(
      persistedSummary,
      detail?.summary ?? null
    )

    // Durable reconnect policy (explicit boolean — do not rely on hook default).
    // Existing readiness (`canAutoConnect`) stays in autoConnectAllowed so real
    // tab activity can still drive bookkeeping via isActive.
    const durableAutoConnectAllowed = resolveSessionAutoConnectAllowed({
      hasPersistedConversation,
      persistedSummary: summaryForSessionPolicy,
      terminalDisconnectLatch,
    })
    const autoConnectAllowed = canAutoConnect && durableAutoConnectAllowed

    // Ref bridge: lifecycle is constructed before the queue helpers below, and
    // send opts need the full draft-restore path. Always call through the ref.
    type DelegateViewerOnlyOpts = {
      optimisticTurnId?: string
      fromQueueFlush?: boolean
      draft?: PromptDraft
      selectedModeIdArg?: string | null
    }
    const handleDelegateViewerOnlyRejectionRef = useRef<
      (options?: DelegateViewerOnlyOpts) => void
    >(() => {
      void refreshDelegateAccess()
    })
    const isTransientUnmount = useCallback(
      () =>
        groupId != null &&
        isReparentUnmount(useTabStore.getState(), tabId, groupId),
      [tabId, groupId]
    )

    const {
      conn,
      modeLoading,
      configOptionsLoading,
      selectorsLoading,
      autoConnectError,
      handleFocus,
      handleReconnect,
      handleSend: lifecycleSend,
      handleSetConfigOption: lifecycleSetConfigOption,
      handleCancel: lifecycleCancel,
      handleRespondPermission,
    } = useConnectionLifecycle({
      contextKey: tabId,
      agentType: selectedAgent,
      // Real tab activity — not folded with durable/cancelled policy.
      isActive,
      autoConnectAllowed,
      workingDir: workingDirForConnection,
      sessionId:
        dbConversationId != null && selectedAgent !== "cline"
          ? externalId
          : undefined,
      // Drives cross-client viewer discovery: when another client is already
      // live on this conversation, attach to its connection instead of spawning.
      conversationId: dbConversationId ?? undefined,
      // Memory-only draft override (and reapply after bind uses conversation row).
      delegationRouteOverride: ownTab?.delegationRouteOverride ?? undefined,
      // Detached cold connect: stamp pop-out incarnation on the agent process.
      ownerOperationId: ownerOperationId ?? undefined,
      connectionIntent: delegatePolicy.intent,
      retryObserverDiscovery: delegatePolicy.retryObserverDiscovery,
      isTransientUnmount,
      onDelegateViewerOnly: () =>
        handleDelegateViewerOnlyRejectionRef.current(),
    })
    const { status: connStatus, sessionId: connSessionId } = conn
    const messageQueue = useMessageQueue()
    const {
      queue: msgQueue,
      enqueue: mqEnqueue,
      requeueFront: mqRequeueFront,
      getQueueLength: mqGetQueueLength,
      dequeue: mqDequeue,
      remove: mqRemove,
      reorder: mqReorder,
      updateItem: mqUpdateItem,
      editingItemId: mqEditingItemId,
      startEditing: mqStartEditing,
      cancelEditing: mqCancelEditing,
    } = messageQueue

    // Centralized typed rejection → draft restore / access refresh (Amendment 7).
    // Non-prompt paths omit draft/optimisticTurnId and only refresh access.
    const handleDelegateViewerOnlyRejection = useCallback(
      (options?: {
        optimisticTurnId?: string
        fromQueueFlush?: boolean
        draft?: PromptDraft
        selectedModeIdArg?: string | null
      }) => {
        if (options?.optimisticTurnId) {
          removeOptimisticTurn(
            effectiveConversationId,
            options.optimisticTurnId
          )
        }
        setSyncState(effectiveConversationId, "idle")
        if (options?.fromQueueFlush && options.draft != null) {
          mqRequeueFront(options.draft, options.selectedModeIdArg ?? null)
        } else if (options?.draft != null) {
          promptDraftRestoreRevisionRef.current += 1
          setPromptDraftRestore({
            revision: promptDraftRestoreRevisionRef.current,
            draft: options.draft,
          })
        }
        void refreshDelegateAccess()
      },
      [
        effectiveConversationId,
        removeOptimisticTurn,
        setSyncState,
        mqRequeueFront,
        refreshDelegateAccess,
      ]
    )
    handleDelegateViewerOnlyRejectionRef.current =
      handleDelegateViewerOnlyRejection
    const connStatusRef = useRef(connStatus)
    useEffect(() => {
      connStatusRef.current = connStatus
    }, [connStatus])
    const isViewerRef = useRef(conn.isViewer)
    useEffect(() => {
      isViewerRef.current = conn.isViewer
    }, [conn.isViewer])
    const isConnecting = connStatus === "connecting"
    // The tab's connection is keyed by a stable tabId, but agent switching is
    // async — and for a not-installed target, connect()'s preflight throws BEFORE
    // it tears down the old connection. So `conn` can still describe the PREVIOUS
    // agent while `selectedAgent` has already advanced. When that's the case we
    // must NOT surface the previous agent's selectors / ready-state as the
    // selected one's: doing so showed the old agent's model + config list and
    // (worse) let a send reach the wrong agent. Reconcile everything the composer
    // reads against `selectedAgent`, falling back to that agent's own cached
    // selectors (empty until it connects).
    const connIsForOtherAgent =
      conn.agentType != null && conn.agentType !== selectedAgent
    const effectiveModes = connIsForOtherAgent
      ? (getCachedSelectors(selectedAgent)?.modes ?? null)
      : conn.modes
    const effectiveConfigOptions = connIsForOtherAgent
      ? (getCachedSelectors(selectedAgent)?.configOptions ?? null)
      : conn.configOptions
    // The live connection is ready for THIS tab only when it's connected AND its
    // cwd matches the tab's intended working dir. A just-retargeted chat draft (or
    // any mid-reconnect) can briefly read a stale "connected" for the PREVIOUS cwd;
    // sending then would deliver the prompt to the wrong agent/workspace. Every
    // direct send gates on this (handleSend), mirroring the flush effect's guard.
    // No-op for normal conversations, whose connected cwd always equals intended.
    // A connection still bound to a different agent is never "ready" for the
    // selected one — it would otherwise let a send reach the previous agent.
    const connectionReady =
      !connIsForOtherAgent &&
      isConnectionReady(
        connStatus,
        conn.connectedWorkingDir,
        workingDirForConnection
      )
    const promptAdmissionReady =
      connectionReady ||
      (conn.sharedSession != null &&
        !connIsForOtherAgent &&
        connStatus === "prompting" &&
        (conn.connectedWorkingDir ?? null) ===
          (workingDirForConnection ?? null))
    // Present "connecting" to the composer while connected-but-not-ready, so it
    // disables its send affordance instead of inviting a submit handleSend rejects.
    // While the live connection still belongs to a different agent, present the
    // selected agent's real state: "disconnected" when it isn't installed (the
    // install banner explains why), otherwise "connecting" (the switch is in
    // flight). Only ever differs from connStatus during those transient windows.
    const composerConnStatus = connIsForOtherAgent
      ? selectedAgentNotInstalled
        ? "disconnected"
        : "connecting"
      : connStatus === "connected" && !connectionReady
        ? "connecting"
        : connStatus
    const connectionModes = useMemo(
      () => effectiveModes?.available_modes ?? [],
      [effectiveModes]
    )
    const connectionConfigOptions = useMemo(
      () => effectiveConfigOptions ?? [],
      [effectiveConfigOptions]
    )
    const connectionCommands = useMemo(
      () => (connIsForOtherAgent ? [] : (conn.availableCommands ?? [])),
      [connIsForOtherAgent, conn.availableCommands]
    )
    const selectedModeId = useMemo(() => {
      if (connectionModes.length === 0) return null
      if (modeId && connectionModes.some((mode) => mode.id === modeId)) {
        return modeId
      }
      return effectiveModes?.current_mode_id ?? connectionModes[0]?.id ?? null
    }, [effectiveModes, connectionModes, modeId])

    // The single blocking message shown in the composer's inline banner (clicking
    // it opens Agent Settings). The not-installed prompt takes priority: it's the
    // actionable one and, unlike the connect-time toast, it's deterministic — it
    // appears the moment a not-installed agent is selected, independent of whether
    // a (deduped/superseded) connect attempt ever reached the preflight.
    const composerBlockedMessage = selectedAgentNotInstalled
      ? tWelcome("agentNotInstalled", { agent: getAgentLabel(selectedAgent) })
      : (autoConnectError ?? agentConnectError)

    useEffect(() => {
      if (connSessionId) {
        sessionIdRef.current = connSessionId
      }
    }, [connSessionId])

    // Mirror the connection's load failure (set on `session_load_failed` from
    // the agent) onto the per-conversation runtime session so the detail UI
    // can surface it next to detail-load errors. Cleared automatically when
    // the connection's loadError clears (e.g. via Reload).
    const connLoadError = conn.loadError
    const connLoadErrorCode = conn.loadErrorCode
    useEffect(() => {
      setAcpLoadError(effectiveConversationId, connLoadError ?? null)
    }, [connLoadError, effectiveConversationId, setAcpLoadError])

    // Promote the completed turn on the prompting→idle edge. (There is no longer
    // an ordering constraint against a setLiveMessage cleanup: the liveMessage
    // sink writes the runtime store from the connection dispatch, not a React
    // effect — see registerLiveMessageSink.)
    const prevConnStatusRef = useRef(connStatus)
    useEffect(() => {
      const wasPrompting = prevConnStatusRef.current === "prompting"
      prevConnStatusRef.current = connStatus
      if (!wasPrompting || connStatus === "prompting") return

      // Turn completed — promote liveMessage + optimisticTurns to localTurns,
      // and drop the live transcript projection in the same call stack (no
      // blank/duplicate assistant frame). Don't pass conn.liveMessage: this
      // panel no longer subscribes to it (see useConnection); COMPLETE_TURN
      // falls back to session.liveMessage written by the sink before status change.
      completeLiveTranscriptTurn(effectiveConversationId)
      if (isDelegateConversation) {
        syncDelegateTerminalDetail(effectiveConversationId)
      }

      // Cancel previous metadata sync (handles rapid consecutive turns)
      syncCancelRef.current?.()
      syncCancelRef.current = null

      const persistedId = dbConvIdRef.current
      if (persistedId && persistedId > 0) {
        syncCancelRef.current = syncTurnMetadata(
          persistedId,
          effectiveConversationId
        )
      }
    }, [
      connStatus,
      effectiveConversationId,
      isDelegateConversation,
      syncDelegateTerminalDetail,
      syncTurnMetadata,
    ])

    // Access leaving task_running (except state_unknown outages) recovers a
    // missed TurnComplete when the resolver already shows a terminal task.
    const previousDelegateReasonRef = useRef(delegateAccess.reason)
    useEffect(() => {
      const previous = previousDelegateReasonRef.current
      previousDelegateReasonRef.current = delegateAccess.reason
      if (
        isDelegateConversation &&
        previous === "task_running" &&
        delegateAccess.reason !== "task_running" &&
        delegateAccess.reason !== "state_unknown"
      ) {
        syncDelegateTerminalDetail(effectiveConversationId)
      }
    }, [
      delegateAccess.reason,
      effectiveConversationId,
      isDelegateConversation,
      syncDelegateTerminalDetail,
    ])

    // Auto-send queued messages when agent finishes responding.
    // Refs are synced via useEffect; the auto-send effect is declared
    // AFTER completeTurn so React runs it second.
    const autoSendQueueRef = useRef<() => QueuedMessage | undefined>(mqDequeue)
    useEffect(() => {
      autoSendQueueRef.current = mqDequeue
    }, [mqDequeue])
    const handleSendRef = useRef<
      (
        draft: PromptDraft,
        modeId?: string | null,
        opts?: { fromQueueFlush?: boolean }
      ) => void
    >(() => {})
    // Timestamp of the last send that bounced with TurnBusyError. The flush below
    // backs off after a bounce so repeated busy rejections (backend still running
    // another turn while this client believes it is idle) don't spin one failed
    // send per round-trip.
    const lastFlushBounceAtRef = useRef(0)

    // Flush queued messages whenever the agent is idle. This is the queue's send
    // engine, covering BOTH:
    //   - the normal case: a message queued while the agent was prompting, sent
    //     once the turn completes (prompting→connected drives syncState→idle); and
    //   - a draft re-queued by a bounced concurrent send that landed AFTER the
    //     prompting→connected transition already passed — which an edge-triggered
    //     flush would strand until the next turn.
    // Gated on syncState !== "awaiting_persist" so exactly one item flushes at a
    // time: dequeuing + sending appends an optimistic turn → awaiting_persist,
    // which blocks re-entry until that send settles (the turn completes, or it
    // bounces and rolls back to idle to retry the next item). A bounce backoff
    // rate-limits retries against a still-busy backend.
    const waitingForSubagentsRef = useRef(conn.waitingForSubagents)
    useEffect(() => {
      waitingForSubagentsRef.current = conn.waitingForSubagents
    }, [conn.waitingForSubagents])

    useEffect(() => {
      if (conn.sharedSession) return
      if (connStatus !== "connected") return
      // Do not dequeue / auto-flush while a durable continuation owns this
      // conversation — waiting is independent of status/turn_in_flight.
      if (conn.waitingForSubagents) return
      // Viewer-only lock: queue stays visible but never auto-flushes.
      if (interactionLocked) return
      // Terminal disconnect pause: never auto-drain historical queue items until
      // the user explicitly resumes. Reconnect alone keeps the pause.
      if (queuePausedByTerminalDisconnect) return
      // Don't flush onto a connection whose cwd doesn't match the tab's intended
      // working dir. This matters for a just-bound chat conversation: bind switches
      // the tab's workingDir from the draft's previous folder to the scratch dir,
      // and for one render `connStatus` can still read the stale "connected" of the
      // old-folder session before the reconnect lands. Flushing then would deliver
      // the queued prompt to the wrong folder's agent. (No-op for normal
      // conversations, whose connection cwd always equals the intended one.)
      if (
        (conn.connectedWorkingDir ?? null) !== (workingDirForConnection ?? null)
      ) {
        return
      }
      if (runtimeSyncState === "awaiting_persist") return
      if (msgQueue.length === 0) return
      // setTimeout (not microtask) so a COMPLETE_TURN commit settles first AND so
      // a just-bounced retry waits out the backoff window before re-sending.
      const wait = flushRetryDelayMs(Date.now(), lastFlushBounceAtRef.current)
      const timer = setTimeout(() => {
        if (connStatusRef.current !== "connected") return
        // Re-check waiting inside the timer: Connected-before-waiting-event race.
        if (waitingForSubagentsRef.current) return
        // Re-check access lock inside the timer (relock can land after schedule).
        if (interactionLockedRef.current) return
        // Re-check terminal pause inside the timer (pause can arm after schedule).
        if (queuePausedByTerminalDisconnectRef.current) return
        const next = autoSendQueueRef.current()
        if (next) {
          // Mark this as the queue auto-flush: it sends the dequeued head now and,
          // on a bounce, returns it to the FRONT (vs a direct send → tail).
          handleSendRef.current(next.draft, next.modeId, {
            fromQueueFlush: true,
          })
        }
      }, wait)
      return () => clearTimeout(timer)
    }, [
      connStatus,
      runtimeSyncState,
      msgQueue.length,
      conn.connectedWorkingDir,
      workingDirForConnection,
      conn.waitingForSubagents,
      interactionLocked,
      queuePausedByTerminalDisconnect,
      conn.sharedSession,
    ])

    // Mirror the connection's liveMessage into the runtime session OUTSIDE React,
    // and publish incremental live-transcript projection for the UI footer (Task 11).
    // The connection dispatch invokes these sinks synchronously whenever liveMessage
    // changes (streaming deltas, tool updates, the prompt-start reset), so the
    // streaming content flows straight to the runtime / transcript stores WITHOUT
    // this keep-alive panel re-rendering per token. Canonical writes non-null values
    // with isLive = (status === "prompting"), which tells the runtime reducer to
    // bypass its stale-reconnect-replay guard. Turn-end clearing is owned by
    // COMPLETE_TURN; unmount clearing by removeConversation. `tabId` is the
    // connection contextKey.
    const connectionIdForSink = conn.connectionId
    useEffect(() => {
      const conversationId = effectiveConversationId
      return acpActions.registerLiveSinks(tabId, {
        canonical: (liveMessage, isLive, deliveryIds) => {
          if (deliveryIds && deliveryIds.length > 0) {
            setLiveMessage(conversationId, liveMessage, isLive, deliveryIds)
          } else {
            setLiveMessage(conversationId, liveMessage, isLive)
          }
        },
        transcript: createLiveTranscriptFrameSink(
          conversationId,
          connectionIdForSink || "pending"
        ),
      })
    }, [
      acpActions,
      tabId,
      effectiveConversationId,
      connectionIdForSink,
      setLiveMessage,
    ])

    const expectedSharedUserMessageIdRef = useRef<string | null>(
      conn.sharedSession?.activeTurn?.clientMessageId ?? null
    )
    useEffect(() => {
      const activeMessageId =
        conn.sharedSession?.activeTurn?.clientMessageId ?? null
      if (activeMessageId)
        expectedSharedUserMessageIdRef.current = activeMessageId
    }, [conn.sharedSession?.activeTurn?.clientMessageId])

    // Cross-client VIEWER (Bug 2): mirror the connection's in-flight user prompt
    // (from a snapshot's `pending_user_message`, captured when we attach
    // mid-turn) into the runtime as a synthesized user turn. The reducer
    // sender-guards + dedups by id, so this is a no-op on the sender and
    // idempotent against the live `user_message` event below. This branch covers
    // the prompt that was sent BEFORE we attached; the live handler covers
    // prompts sent AFTER.
    useEffect(() => {
      const pending = conn.pendingUserMessage
      if (!pending) return
      if (
        conn.sharedSession &&
        pending.messageId !== expectedSharedUserMessageIdRef.current
      ) {
        return
      }
      appendViewerUserTurn(
        effectiveConversationId,
        buildUserTurnFromMessageBlocks(pending.messageId, pending.blocks)
      )
    }, [
      conn.pendingUserMessage,
      conn.sharedSession,
      effectiveConversationId,
      appendViewerUserTurn,
    ])

    // Cross-client VIEWER (Bug 2): a `user_message` event for THIS connection
    // that arrives while we're attached. The owner added its user turn
    // optimistically; a viewer only receives the assistant stream, so without
    // this the reply would render with no user message above it. Sender-guarded +
    // idempotent in the reducer (the sender's own echo is a no-op).
    useAcpEvent(
      useCallback(
        (envelope: EventEnvelope) => {
          if (
            envelope.type === "prompt_dispatch_started" &&
            envelope.connection_id === conn.connectionId &&
            conn.sharedSession &&
            envelope.generation === conn.sharedSession.generation
          ) {
            expectedSharedUserMessageIdRef.current =
              envelope.turn.client_message_id
            return
          }
          if (envelope.type !== "user_message") return
          if (envelope.connection_id !== conn.connectionId) return
          if (
            conn.sharedSession &&
            envelope.message_id !== expectedSharedUserMessageIdRef.current
          ) {
            return
          }
          appendViewerUserTurn(
            effectiveConversationId,
            buildUserTurnFromMessageBlocks(envelope.message_id, envelope.blocks)
          )
        },
        [
          conn.connectionId,
          conn.sharedSession,
          effectiveConversationId,
          appendViewerUserTurn,
        ]
      )
    )

    // Terminal disconnect latch: arm before the global cancelled patch can race
    // with focus. Same signal also pauses the queue (Resume Queue is the only
    // clear for that pause). Baseline updated_at is captured only on first arm.
    // Queue-pause ref is set synchronously with setState so any already-
    // scheduled zero-delay auto-flush timer sees the pause in the same turn
    // (no passive state→ref mirror — that can clobber a later arm; see ref).
    //
    // Root summary is read from the workspace store at event delivery time
    // (not from this render's `persistedSummary` closure). `useAcpEvent` only
    // installs the latest handler via a passive effect, so a store patch that
    // lands before that effect (or before re-render) would otherwise arm from
    // a stale in_progress / old updated_at and immediately clear on the newer
    // baseline.
    //
    // The persisted root *id* is also resolved at delivery time from
    // `dbConvIdRef` (sync-written on first-send bind), not from the render
    // closure's `dbConversationId`. A draft's first send assigns the ref
    // before `setCreatedConversationId` can re-render / reinstall the ACP
    // handler; a stale handler that still closed over null would otherwise
    // derive a null delivery summary and reject a valid same-connection
    // terminal event (no latch, no queue pause).
    //
    // Same-bound-connection is resolved at delivery time from the connection
    // store (`getConnection(tabId)?.connectionId`), not from the render
    // closure's `conn.connectionId`. During A→B transitions the passive
    // useAcpEvent callback refresh lags; a stale handler that closed over A
    // would accept late A terminal events after B is current, or reject B
    // terminal events before the handler reinstalls. The provider updates its
    // connection map before notifyRawSubscribers, so delivery-time lookup is
    // authoritative. Render `conn` remains for UI elsewhere.
    useAcpEvent(
      useCallback(
        (envelope: EventEnvelope) => {
          const deliveryConversationId = dbConvIdRef.current
          const deliverySummary =
            deliveryConversationId != null
              ? (useAppWorkspaceStore
                  .getState()
                  .conversations.find(
                    (row) => row.id === deliveryConversationId
                  ) ?? null)
              : null
          const deliveryConnectionId =
            connectionStore.getConnection(tabId)?.connectionId ?? null
          if (
            !shouldLatchTerminalDisconnect(
              envelope,
              deliveryConnectionId,
              deliverySummary
            )
          ) {
            return
          }
          // Reconcile any existing latch against the delivery-time summary in
          // the same updater. If Y is a newer non-cancelled root that would
          // clear baseline X, re-arm immediately with Y so a later clear
          // against Y cannot drop the latch after this terminal event. Stale /
          // same-baseline / cancelled keep the prior baseline (first-arm).
          setTerminalDisconnectLatch((prev) => {
            const nextBaseline = {
              baselineUpdatedAt: deliverySummary!.updated_at,
            } satisfies TerminalDisconnectLatch
            if (prev == null) return nextBaseline
            if (shouldClearTerminalDisconnectLatch(prev, deliverySummary)) {
              return nextBaseline
            }
            return prev
          })
          queuePausedByTerminalDisconnectRef.current = true
          setQueuePausedByTerminalDisconnect(true)
        },
        [connectionStore, tabId]
      )
    )

    // Clear reconnect latch when a newer non-cancelled workspace summary is
    // observed. Adjust during render (React-recommended store→state sync) so
    // a summary advancement is applied in the same commit that reads it —
    // not only in a later passive effect. Queue pause is intentionally not
    // cleared here.
    const [latchSummaryEpoch, setLatchSummaryEpoch] = useState<{
      status: string | undefined
      updatedAt: string | undefined
    }>(() => ({
      status: persistedSummary?.status,
      updatedAt: persistedSummary?.updated_at,
    }))
    if (
      latchSummaryEpoch.status !== persistedSummary?.status ||
      latchSummaryEpoch.updatedAt !== persistedSummary?.updated_at
    ) {
      setLatchSummaryEpoch({
        status: persistedSummary?.status,
        updatedAt: persistedSummary?.updated_at,
      })
      setTerminalDisconnectLatch((prev) =>
        applyPersistedSummaryToTerminalLatch(prev, persistedSummary)
      )
    }

    useEffect(() => {
      if (effectiveConversationId <= 0) return
      setExternalId(
        effectiveConversationId,
        detail?.summary.external_id ?? null
      )
    }, [effectiveConversationId, detail?.summary.external_id, setExternalId])

    useEffect(() => {
      if (!connSessionId) return
      setExternalId(effectiveConversationId, connSessionId)
    }, [connSessionId, effectiveConversationId, setExternalId])

    useEffect(() => {
      if (dbConversationId == null) return
      if (reloadSignal === latestReloadSignal.current) return
      latestReloadSignal.current = reloadSignal
      pendingReloadState.current = {
        signal: reloadSignal,
        sawLoading: false,
      }
      // Manual Reload override: clear cancel fence then authoritative load.
      reloadDetail(effectiveConversationId, { reason: "manual_reload" })
    }, [dbConversationId, effectiveConversationId, reloadDetail, reloadSignal])

    useEffect(() => {
      const pending = pendingReloadState.current
      if (!pending) return

      if (detailLoading) {
        pending.sawLoading = true
        return
      }

      if (!pending.sawLoading) return

      pendingReloadState.current = null

      if (detailError) {
        toast.error(t("reloadFailed", { message: detailError }))
        return
      }

      toast.success(t("reloaded"))
    }, [detailLoading, detailError, t])

    // Cleanup runtime data on unmount (tab close)
    useEffect(() => {
      mountedRef.current = true
      return () => {
        mountedRef.current = false
        syncCancelRef.current?.()
        if (connStatusRef.current === "prompting" && !isViewerRef.current) {
          // Owner, agent still responding — keep the session for deferred cleanup
          // (the background turn_complete handler removes it once done).
          setPendingCleanup(effectiveConversationId, true)
        } else {
          // Idle owner, or a VIEWER (any status): remove immediately. A viewer's
          // unmount detaches its attach subscription, so no turn_complete will
          // arrive to resolve a deferred cleanup — deferring would leak the
          // runtime session (especially in web mode, which has no event firehose
          // after detach).
          removeConversation(effectiveConversationId)
        }
      }
    }, [effectiveConversationId, removeConversation, setPendingCleanup])

    const handleSend = useCallback(
      (
        draft: PromptDraft,
        selectedModeIdArg?: string | null,
        // `fromQueueFlush` marks the auto-flush draining the queue head — that
        // path always sends and, on a bounce, re-queues at the FRONT. A direct
        // input send (no flag) must NOT jump ahead of already-queued items: when
        // a queue exists it tail-enqueues instead of sending, and on a bounce it
        // re-queues at the TAIL.
        opts?: { fromQueueFlush?: boolean }
      ): void | Promise<unknown> => {
        // Access lock first — do not queue, clear draft, or build optimistic turns.
        if (interactionLocked) return

        // Capture the tab's chat-draft state + eager scratch dir synchronously,
        // before any await. A folderless chat draft is NOT special-cased here:
        // its first send takes the exact same gated, inline path as a normal new
        // conversation (the new-tab branch below just creates the row via
        // createChatConversation, reusing this eager dir). The composer is gated
        // on `connected` for chat drafts too, so by the time we get here the agent
        // is live and the prompt is delivered inline — never parked in the queue.
        const sendOwnTab = ownTab

        if (!hasPersistedConversation && !canAutoConnect) {
          setAgentConnectError(tWelcome("enableAgentFirstPlaceholder"))
          return
        }
        // Connected AND the connection's cwd matches this tab's working dir. Bare
        // `connStatus === "connected"` is not enough: a chat draft mid-reconnect can
        // read a stale "connected" for the old cwd, and an inline send then would
        // deliver to the wrong workspace. Same predicate the flush effect uses.
        if (!promptAdmissionReady) return
        // Advisory UI lock can race the backend gate — still refuse optimistic
        // mutation when the current snapshot already says waiting.
        if (conn.waitingForSubagents) return

        const fromQueueFlush = opts?.fromQueueFlush ?? false
        // Preserve FIFO: a direct send issued while the queue is non-empty joins
        // the tail rather than racing ahead of the queued items. Read the
        // queue length synchronously (it reflects a same-tick bounce requeue).
        // During terminal pause, direct sends bypass historical head so the user
        // can continue without draining stale queued drafts first.
        if (
          !conn.sharedSession &&
          shouldQueueDirectSend(
            fromQueueFlush,
            mqGetQueueLength(),
            queuePausedByTerminalDisconnect
          )
        ) {
          mqEnqueue(draft, selectedModeIdArg ?? null)
          return
        }

        // Single-flight the unbound new-tab create. A second direct submit fired
        // before the first create resolves (a double Enter / double click) would
        // otherwise append an optimistic turn it can never deliver: the
        // createConversationPendingRef guard further down returns AFTER the
        // optimistic append. Reject the duplicate here, before any optimistic
        // mutation. Only the unbound path (no persisted id yet) is single-flighted,
        // so persisted sends keep their concurrent queued-send behavior. Applies
        // equally to chat and normal new conversations.
        if (
          shouldRejectDuplicateCreate(
            dbConvIdRef.current != null,
            createConversationPendingRef.current
          )
        ) {
          return
        }

        const optimisticTurn = buildOptimisticUserTurnFromDraft(
          draft,
          sharedT("attachedResources")
        )
        appendOptimisticTurn(
          effectiveConversationId,
          optimisticTurn,
          optimisticTurn.id
        )
        setSendSignal((prev) => prev + 1)
        setSyncState(effectiveConversationId, "awaiting_persist")
        const preservesSharedWelcomeComposer =
          conn.sharedSession != null &&
          (!hasPersistedConversation || retainWelcomeComposerForAdmission)
        if (preservesSharedWelcomeComposer) {
          setRetainWelcomeComposerForAdmission(true)
        } else {
          setHasSentMessage(true)
        }

        const completeSharedWelcomeAdmission = <T,>(result: T): T => {
          if (preservesSharedWelcomeComposer) {
            setHasSentMessage(true)
            setRetainWelcomeComposerForAdmission(false)
          }
          return result
        }

        // Backend rejected the send because a turn was already in flight (another
        // co-controlling client, or a "prompting" status this client hadn't
        // observed yet). Roll back the optimistic user turn and drop the draft
        // into the queue above the input box — it auto-sends when the current
        // turn completes, identical to enqueuing while already prompting. Stamp
        // the bounce so the flush backs off instead of immediately retrying.
        const onTurnInProgress = () => {
          lastFlushBounceAtRef.current = Date.now()
          removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
          // FIFO: the auto-flush draft WAS the queue head → return it to the
          // front; a direct send (queue was empty when it left) → tail.
          if (fromQueueFlush) {
            mqRequeueFront(draft, selectedModeIdArg ?? null)
          } else {
            mqEnqueue(draft, selectedModeIdArg ?? null)
          }
        }

        // Continuation waiting rejection: restore runtime to idle, drop the
        // optimistic turn. Direct composer send → PromptDraftRestore (never
        // queue). Queue-flush rejection → requeue the same head (never overwrite
        // the editor). fromQueueFlush remains visible to this closure.
        const onContinuationWaiting = () => {
          removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
          setSyncState(effectiveConversationId, "idle")
          if (fromQueueFlush) {
            mqRequeueFront(draft, selectedModeIdArg ?? null)
          } else {
            promptDraftRestoreRevisionRef.current += 1
            setPromptDraftRestore({
              revision: promptDraftRestoreRevisionRef.current,
              draft,
            })
          }
        }

        const onSendFailed = () => {
          removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
          if (conn.sharedSession) {
            setSyncState(effectiveConversationId, "idle")
          }
        }

        const onPromptAdmitted = (
          result: import("@/lib/types").PromptEnqueueResult | null
        ) => {
          if (conn.sharedSession && result?.state === "queued") {
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
            setSyncState(effectiveConversationId, "idle")
          }
        }

        // Pin the tab if it was a temporary preview (single-click opened)
        if (ownTab && !ownTab.isPinned) {
          pinTab(tabId)
        }

        const persistedId = dbConvIdRef.current
        if (persistedId) {
          // Existing-tab path: row already exists, send immediately with the
          // conversation_id pinned so the backend reuses our row instead of
          // creating a duplicate.
          const sendResult = lifecycleSend(draft, selectedModeIdArg, {
            folderId,
            conversationId: persistedId,
            // The backend echoes this as the broadcast UserMessage's message_id,
            // so viewers' synthesized user turn dedups against our own optimistic
            // turn by exact id (and never suppresses a different sender's prompt).
            clientMessageId: optimisticTurn.id,
            onTurnInProgress: conn.sharedSession ? undefined : onTurnInProgress,
            onContinuationWaiting: conn.sharedSession
              ? undefined
              : onContinuationWaiting,
            onSendFailed,
            onPromptAdmitted,
            onDelegateViewerOnly: () =>
              conn.sharedSession
                ? onSendFailed()
                : handleDelegateViewerOnlyRejection({
                    optimisticTurnId: optimisticTurn.id,
                    fromQueueFlush,
                    draft,
                    selectedModeIdArg,
                  }),
          })
          return preservesSharedWelcomeComposer
            ? Promise.resolve(sendResult).then(completeSharedWelcomeAdmission)
            : sendResult
        }

        // New-tab path: create the DB row first, then send with the new id
        // pinned. This prevents the backend's send_prompt_linked from racing
        // us to create its own conversation row. A folderless chat draft creates
        // via createChatConversation (reusing the eager scratch dir) and binds to
        // its hidden chat folder; every other step — the optimistic turn
        // appended above, the inline lifecycleSend, the rollback — is identical to
        // a normal new conversation. This is the whole point of the fix: after the
        // scratch dir exists, chat mode shares the normal send path and never
        // depends on the flush-on-connect queue to deliver its first prompt.
        if (createConversationPendingRef.current) return
        createConversationPendingRef.current = true
        const title = getPromptDraftDisplayText(
          draft,
          sharedT("attachedResources")
        ).slice(0, 80)
        const chatSend = sendOwnTab?.isChat === true
        const chatExistingDir = sendOwnTab?.workingDir

        const createAndSend = async () => {
          try {
            let newConversationId: number
            // The send's folderId defaults to the active folder; a chat send
            // overrides it with the backend-created hidden chat folder.
            let sendFolderId = folderId
            if (chatSend) {
              const res = await createChatConversation(
                selectedAgent,
                title,
                chatExistingDir,
                sendOwnTab?.delegationRouteOverride ?? null
              )
              newConversationId = res.conversationId
              sendFolderId = res.folderId
              dbConvIdRef.current = newConversationId
              setExternalId(
                effectiveConversationId,
                sessionIdRef.current ?? null
              )
              // Bind the DB id BEFORE the prompt goes out. The mirror effect
              // below also binds, but only after a re-render — this closes that
              // window and covers the unmounted-early return just under it.
              setDbConversationId(effectiveConversationId, newConversationId)
              if (!mountedRef.current) {
                setPendingCleanup(effectiveConversationId, true)
                refreshConversations()
                return
              }
              // Seed allFolders with the hidden chat folder so the tab's new
              // folderId resolves (cwd / active-folder) on the next render. bind
              // reuses the eager scratch dir as workingDir, so the connection's
              // cwd does not move and no reconnect is triggered.
              upsertFolder(res.folder)
              setCreatedConversationId(newConversationId)
              bindConversationTab(
                tabId,
                newConversationId,
                selectedAgent,
                title,
                effectiveConversationId,
                res.folderId,
                res.folder.path
              )
            } else {
              newConversationId = await createConversation(
                folderId,
                selectedAgent,
                title,
                sendOwnTab?.delegationRouteOverride ?? null
              )
              dbConvIdRef.current = newConversationId
              // Set external ID on the stable virtual session (no migration needed —
              // effectiveConversationId never changes, so the session stays in place).
              // DB persistence of external_id is now backend-driven from
              // send_prompt_linked once the row is linked, so no explicit DB write here.
              setExternalId(
                effectiveConversationId,
                sessionIdRef.current ?? null
              )
              // Bind the DB id BEFORE the prompt goes out (see the chat branch).
              setDbConversationId(effectiveConversationId, newConversationId)
              if (!mountedRef.current) {
                // Component unmounted while creating — mark for deferred cleanup
                // so the background turn_complete handler can clean up later.
                setPendingCleanup(effectiveConversationId, true)
                refreshConversations()
                return
              }
              setCreatedConversationId(newConversationId)
              bindConversationTab(
                tabId,
                newConversationId,
                selectedAgent,
                title,
                effectiveConversationId
              )
            }
            if (!conn.sharedSession) clearMessageInputDraft(draftStorageKey)
            refreshConversations()

            // Now that the row exists, kick off the actual prompt with the
            // conversation_id pinned so the backend adopts our row instead of
            // creating a duplicate one.
            await lifecycleSend(draft, selectedModeIdArg, {
              folderId: sendFolderId,
              conversationId: newConversationId,
              clientMessageId: optimisticTurn.id,
              onTurnInProgress: conn.sharedSession
                ? undefined
                : onTurnInProgress,
              onContinuationWaiting: conn.sharedSession
                ? undefined
                : onContinuationWaiting,
              onSendFailed,
              onPromptAdmitted,
              onDelegateViewerOnly: () =>
                conn.sharedSession
                  ? onSendFailed()
                  : handleDelegateViewerOnlyRejection({
                      optimisticTurnId: optimisticTurn.id,
                      fromQueueFlush,
                      draft,
                      selectedModeIdArg,
                    }),
            })
          } catch (e) {
            if (conn.sharedSession && dbConvIdRef.current != null) {
              throw e
            }
            console.error(
              "[ConversationSessionSurface] create conversation:",
              e
            )
            // A failed create (chat OR normal) must fully restore the pre-send
            // state, not strand the user behind a blank panel:
            //   1. drop the optimistic turn (no ghost stuck in awaiting_persist),
            //   2. return syncState to idle,
            //   3. setHasSentMessage(false) → re-enters welcome mode (otherwise the
            //      welcome screen never returns and the list is empty),
            //   4. re-seed the draft text — message-input clears it synchronously on
            //      send, so without this the user's prompt is lost on failure,
            //   5. surface the error on the welcome banner so it isn't silent.
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
            setSyncState(effectiveConversationId, "idle")
            setHasSentMessage(false)
            const draftText = draft.displayText.trim()
            if (!conn.sharedSession && draftText) {
              saveMessageInputDraft(draftStorageKey, draftText)
            }
            if (mountedRef.current) {
              setAgentConnectError(tWelcome("createConversationFailed"))
            }
            if (conn.sharedSession) throw e
          } finally {
            createConversationPendingRef.current = false
          }
        }
        const pendingCreate = createAndSend()
        if (conn.sharedSession) {
          return preservesSharedWelcomeComposer
            ? pendingCreate.then(completeSharedWelcomeAdmission)
            : pendingCreate
        }
        void pendingCreate
      },
      [
        appendOptimisticTurn,
        removeOptimisticTurn,
        mqEnqueue,
        mqRequeueFront,
        mqGetQueueLength,
        bindConversationTab,
        canAutoConnect,
        promptAdmissionReady,
        conn.sharedSession,
        retainWelcomeComposerForAdmission,
        conn.waitingForSubagents,
        draftStorageKey,
        effectiveConversationId,
        folderId,
        handleDelegateViewerOnlyRejection,
        hasPersistedConversation,
        interactionLocked,
        lifecycleSend,
        pinTab,
        queuePausedByTerminalDisconnect,
        refreshConversations,
        selectedAgent,
        setDbConversationId,
        setExternalId,
        setPendingCleanup,
        setSyncState,
        sharedT,
        ownTab,
        tWelcome,
        tabId,
        upsertFolder,
      ]
    )

    // Explicit reconnect must not fire session/new for a historical non-cline
    // root while external identity is still unknown (detail loading with no
    // runtime id). Same identity sources as the sessionId passed to lifecycle.
    const sessionIdentityReady = canExplicitReconnectWithSessionIdentity({
      hasPersistedConversation,
      isCline: selectedAgent === "cline",
      externalSessionId: externalId,
    })
    const showReconnect = shouldShowTerminalReconnect({
      rootCancelled: persistedSummary?.status === "cancelled",
      terminalDisconnectLatch,
      connStatus,
      sessionIdentityReady,
    })
    const onReconnect = useCallback(() => {
      // Defense in depth: refuse stale UI callbacks that still fire without a
      // resumable identity. Presentation already gates showReconnect; this
      // blocks connect even if a prior onReconnect reference is invoked.
      if (
        !canExplicitReconnectWithSessionIdentity({
          hasPersistedConversation,
          isCline: selectedAgent === "cline",
          externalSessionId: externalId,
        })
      ) {
        console.warn(
          "[ConversationSessionSurface] explicit reconnect blocked: historical session identity unresolved"
        )
        return
      }
      // Explicit reconnect only — does not mutate status or queue pause.
      // Rejection is consumed inside handleReconnect (no unhandled rejection).
      void handleReconnect()
    }, [hasPersistedConversation, selectedAgent, externalId, handleReconnect])
    const onResumeQueue = useCallback(() => {
      // Sync ref with state in the same turn so a same-tick flush timer recheck
      // observes the resumed decision. Only write path that clears the pause
      // (no passive state→ref mirror — see ref declaration).
      queuePausedByTerminalDisconnectRef.current = false
      setQueuePausedByTerminalDisconnect(false)
    }, [])

    // Sync handleSend ref for auto-send effect (declared before handleSend)
    useEffect(() => {
      handleSendRef.current = handleSend
    }, [handleSend])

    const handleForkSend = useCallback(
      // Fire-and-forget: the input clears the draft synchronously on click (like a
      // normal send), so there is no in-flight editable window. If the fork can't
      // run right now — disconnected, or the queue is non-empty (a fork is an
      // immediate session side effect and must not jump ahead of queued items) —
      // the draft is NOT lost: it is queued as a normal send (it flushes after any
      // queued items). The same on a fork failure.
      async (draft: PromptDraft, selectedModeIdArg?: string | null) => {
        if (interactionLocked) return
        const connectionId = conn.connectionId
        if (
          !connectionId ||
          connStatus !== "connected" ||
          // Read the queue length SYNCHRONOUSLY so a draft re-queued by a same-
          // tick bounce is seen even before React commits. The UI also hides the
          // fork affordance while the queue is non-empty; this is the guard.
          forkSendBlockedByQueue(mqGetQueueLength())
        ) {
          mqEnqueue(draft, selectedModeIdArg ?? null)
          return
        }
        try {
          // Backend performs all DB writes in one transaction-shaped call:
          // - current row: external_id=S2, title="[Fork] ..."
          // - sibling row: created with external_id=S1, status=pending_review
          // Pass (conversationId, folderId) so a conversation opened from history
          // — whose connection resumed via session_id but isn't row-linked until
          // its first prompt — is adopted by the backend before forking (a
          // fork-send forks BEFORE that prompt). No-op once already linked. Use
          // the real persisted DB id (`dbConvIdRef`, same as the send path below),
          // NOT the runtime key `effectiveConversationId` which can be virtual.
          const { forkedSessionId } = await acpFork(
            connectionId,
            dbConvIdRef.current,
            folderId
          )
          // Update runtime session id to S2 (frontend in-memory state only)
          sessionIdRef.current = forkedSessionId
          setExternalId(effectiveConversationId, forkedSessionId)

          // NOTE: a fork is a transcript discontinuity — the row's session flips
          // S1→S2, and S2 is a COPY of S1's transcript plus the turns to come.
          // The pre-fork history is NOT re-surfaced here: the backend background
          // watcher correctly excludes the fork-copied prefix from the out-of-turn
          // overlay (see `baseline_offset_since`), so `detail.turns` (S1 parse) +
          // the new local turns render each exchange exactly once. No detail
          // refetch is needed or wanted — an early one races the forked turn and
          // can drop the just-sent message.
          refreshConversations()
          // Send the message on the forked session (S2)
          handleSend(draft, selectedModeIdArg)
        } catch (err) {
          if (isDelegateViewerOnlyRejection(err)) {
            // Capture draft before clear already happened in MessageInput; restore
            // via the shared rejection handler so the composer is not emptied.
            handleDelegateViewerOnlyRejection({
              draft,
              selectedModeIdArg,
            })
            return
          }
          // Busy (a turn is in flight, e.g. another co-controlling client started
          // one): NOT a fork failure — silently re-queue, like a normal bounce.
          // It sends after the current turn.
          if (err instanceof TurnBusyError) {
            mqEnqueue(draft, selectedModeIdArg ?? null)
            return
          }
          // Real fork failure: surface it. EXPLICIT product decision — fork-send
          // is best-effort, so the draft is never lost; it is re-queued and sent
          // on the current (un-forked) session.
          toast.error(
            t("forkSessionFailed", {
              error:
                err instanceof Error
                  ? err.message
                  : typeof err === "object" && err !== null
                    ? JSON.stringify(err)
                    : String(err),
            })
          )
          mqEnqueue(draft, selectedModeIdArg ?? null)
        }
      },
      [
        conn.connectionId,
        connStatus,
        mqGetQueueLength,
        mqEnqueue,
        effectiveConversationId,
        folderId,
        handleDelegateViewerOnlyRejection,
        handleSend,
        interactionLocked,
        refreshConversations,
        setExternalId,
        t,
      ]
    )

    const handleOpenAgentsSettings = useCallback(() => {
      openSettingsWindow("agents", { agentType: selectedAgent }).catch(
        (err) => {
          console.error(
            "[ConversationSessionSurface] failed to open settings window:",
            err
          )
        }
      )
    }, [selectedAgent])

    // Manual agent switch only updates local draft state. The single source of
    // truth for (dis)connecting is `useConnectionLifecycle`'s auto-connect
    // effect: when `selectedAgent` changes, the hook re-fires `connect()`,
    // which internally disconnects the old agent's connection at the same
    // contextKey before creating the new one (acp-connections-context.tsx).
    // Doing the disconnect+reconnect here too would race the lifecycle path:
    // a late-returning disconnect would dispatch CONNECTION_REMOVED by
    // contextKey and wipe the new connection's frontend state, leaving a
    // backend orphan.
    const handleAgentSelect = useCallback(
      (nextAgentType: AgentType) => {
        if (nextAgentType === selectedAgentRef.current) return
        if (dbConvIdRef.current) return

        setDraftAgentType(nextAgentType)
        setModeId(getSavedModeId(nextAgentType))
        setAgentConnectError(null)
        // Real user click — clear the provisional flag so TabProvider's
        // correction effect leaves this tab alone.
        confirmDraftAgent(tabId, nextAgentType)
      },
      [confirmDraftAgent, tabId]
    )

    // AgentSelector auto-fallback: the requested default agent was missing
    // or unavailable, so it picked a substitute on its own. Sync local UI
    // state (so the connection points at the right agent immediately) but
    // mark the tab as still provisional — TabProvider's correction effect
    // will re-resolve against the folder's saved default once all three
    // hydration gates are open, and overwrite this substitute if needed.
    const handleAgentFallback = useCallback(
      (nextAgentType: AgentType) => {
        if (nextAgentType === selectedAgentRef.current) return
        if (dbConvIdRef.current) return

        setDraftAgentType(nextAgentType)
        setModeId(getSavedModeId(nextAgentType))
        setAgentConnectError(null)
        setDraftAgentFromFallback(tabId, nextAgentType)
      },
      [setDraftAgentFromFallback, tabId]
    )

    const handleModeChange = useCallback(
      (newModeId: string) => {
        if (interactionLocked) return
        setModeId(newModeId)
        // Persist mode selection to localStorage immediately. Use effectiveModes
        // (reconciled to selectedAgent) rather than the raw connection modes, so a
        // mode change made during a cross-agent switch window can't save the
        // previous agent's mode shape under the selected agent.
        if (effectiveModes) {
          saveModePreference(selectedAgent, {
            ...effectiveModes,
            current_mode_id: newModeId,
          })
        }
      },
      [effectiveModes, interactionLocked, selectedAgent]
    )

    const handleCancel = useCallback(() => {
      if (interactionLocked) return
      lifecycleCancel()
    }, [interactionLocked, lifecycleCancel])

    const handleSharedQueueCancel = useCallback(
      (queueItemId: string) =>
        acpActions.cancelQueuedPrompt(tabId, queueItemId),
      [acpActions, tabId]
    )

    const handleSetConfigOption = useCallback(
      (configId: string, valueId: string) => {
        if (interactionLocked) return
        lifecycleSetConfigOption(configId, valueId)
      },
      [interactionLocked, lifecycleSetConfigOption]
    )

    const handleAnswerQuestion = useCallback(
      (answer: string) => {
        if (interactionLocked) return
        if (connStatus !== "connected") return
        const optimisticTurn: MessageTurn = {
          id: `optimistic-${randomUUID()}`,
          role: "user",
          blocks: [{ type: "text", text: answer }],
          timestamp: new Date().toISOString(),
        }
        const draft: PromptDraft = {
          blocks: [{ type: "text", text: answer }],
          displayText: answer,
        }
        appendOptimisticTurn(
          effectiveConversationId,
          optimisticTurn,
          optimisticTurn.id
        )
        setSendSignal((prev) => prev + 1)
        setSyncState(effectiveConversationId, "awaiting_persist")
        lifecycleSend(draft, null, {
          clientMessageId: optimisticTurn.id,
          // Rejected because a turn was already in flight — roll back the
          // optimistic turn and re-queue so it isn't stranded or lost.
          onTurnInProgress: () => {
            lastFlushBounceAtRef.current = Date.now()
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
            // A direct answer (never dequeued from the queue) re-queues at the
            // TAIL — it was sent after any already-queued items, so FIFO keeps it
            // behind them. (Only the auto-flush path, whose draft WAS the head,
            // re-queues at the front.)
            mqEnqueue(draft, null)
          },
          onSendFailed: () => {
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
          },
          onDelegateViewerOnly: () =>
            handleDelegateViewerOnlyRejection({
              optimisticTurnId: optimisticTurn.id,
              draft,
            }),
        })
      },
      [
        appendOptimisticTurn,
        removeOptimisticTurn,
        mqEnqueue,
        connStatus,
        effectiveConversationId,
        handleDelegateViewerOnlyRejection,
        interactionLocked,
        lifecycleSend,
        setSyncState,
      ]
    )

    // Answer a blocking multiple-choice `ask_user_question`. Routes straight to
    // the dedicated answer endpoint (NOT a prompt) so it resolves the parked tool
    // call; the backend broadcasts `question_resolved` to clear the card on every
    // client.
    const handleAnswerAskQuestion = useCallback(
      async (questionId: string, answer: QuestionAnswer) => {
        if (interactionLocked) {
          // Reject so AskQuestionCard clears submitting rather than treating a
          // silent return as success (which latches the spinner permanently).
          throw new Error("delegate viewer-only: question answer blocked")
        }
        try {
          await acpActions.answerQuestion(tabId, questionId, answer)
        } catch (err) {
          if (isDelegateViewerOnlyRejection(err)) {
            handleDelegateViewerOnlyRejection()
            // Re-throw so the card's catch path clears submitting / shows retry.
            throw err
          }
          throw err
        }
      },
      [acpActions, handleDelegateViewerOnlyRejection, interactionLocked, tabId]
    )

    // Grok `exit_plan_mode` approval resolves the blocked request. Revision
    // notes must also be sent as a normal follow-up prompt because Grok's
    // keep-planning response does not consume the approval feedback itself.
    const handleAnswerPlanApproval = useCallback(
      async (approvalId: string, answer: PlanApprovalAnswer) => {
        if (interactionLocked) {
          throw new Error("delegate viewer-only: plan approval blocked")
        }
        try {
          await acpActions.answerPlanApproval(tabId, approvalId, answer)
        } catch (err) {
          if (isDelegateViewerOnlyRejection(err)) {
            handleDelegateViewerOnlyRejection()
          }
          throw err
        }

        const notes = answer.feedback?.trim()
        if (answer.decision !== "request_changes" || !notes) return
        if (connStatus !== "connected") return

        const optimisticTurn: MessageTurn = {
          id: `optimistic-${randomUUID()}`,
          role: "user",
          blocks: [{ type: "text", text: notes }],
          timestamp: new Date().toISOString(),
        }
        const draft: PromptDraft = {
          blocks: [{ type: "text", text: notes }],
          displayText: notes,
        }
        appendOptimisticTurn(
          effectiveConversationId,
          optimisticTurn,
          optimisticTurn.id
        )
        setSendSignal((prev) => prev + 1)
        setSyncState(effectiveConversationId, "awaiting_persist")
        lifecycleSend(draft, null, {
          clientMessageId: optimisticTurn.id,
          onTurnInProgress: () => {
            lastFlushBounceAtRef.current = Date.now()
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
            mqEnqueue(draft, null)
          },
          onSendFailed: () => {
            removeOptimisticTurn(effectiveConversationId, optimisticTurn.id)
          },
          onDelegateViewerOnly: () =>
            handleDelegateViewerOnlyRejection({
              optimisticTurnId: optimisticTurn.id,
              draft,
            }),
        })
      },
      [
        acpActions,
        appendOptimisticTurn,
        connStatus,
        effectiveConversationId,
        handleDelegateViewerOnlyRejection,
        interactionLocked,
        lifecycleSend,
        mqEnqueue,
        removeOptimisticTurn,
        setSyncState,
        tabId,
      ]
    )

    // Queue edit flow: derive editing draft text from queue state
    const editingQueueDraftText = useMemo(() => {
      if (!mqEditingItemId) return null
      const item = msgQueue.find((m) => m.id === mqEditingItemId)
      return item?.draft.displayText ?? null
    }, [mqEditingItemId, msgQueue])

    // The editing item's full blocks, so the composer can restore inline badges +
    // attachments (not just the display text) when re-opening a queued message.
    const editingQueueDraftBlocks = useMemo(() => {
      if (!mqEditingItemId) return null
      const item = msgQueue.find((m) => m.id === mqEditingItemId)
      return item?.draft.blocks ?? null
    }, [mqEditingItemId, msgQueue])

    const handleQueueEdit = useCallback(
      (id: string) => {
        mqStartEditing(id)
      },
      [mqStartEditing]
    )

    const handleQueueCancelEdit = useCallback(() => {
      mqCancelEditing()
    }, [mqCancelEditing])

    const handleSaveQueueEdit = useCallback(
      (draft: PromptDraft) => {
        if (mqEditingItemId) {
          mqUpdateItem(mqEditingItemId, draft)
        }
      },
      [mqEditingItemId, mqUpdateItem]
    )

    const showDraftHeader = !hasPersistedConversation && !hasSentMessage
    const isWelcomeMode = showDraftHeader || retainWelcomeComposerForAdmission

    const handleQuickAction = useCallback((payload: ComposerInjectContent) => {
      setQuickActionInject(payload)
    }, [])

    const handleQuickActionConsumed = useCallback(() => {
      setQuickActionInject(null)
    }, [])

    const canShowDetailErrorActions =
      hasPersistedConversation && dbConversationId != null && !!folder
    const handleReloadDetail = useCallback(() => {
      if (dbConversationId == null) return
      // Clear the ACP load failure so canAutoConnect re-enables and the next
      // auto-connect attempt is allowed to retry session/load. The mirror
      // effect above syncs this back into the runtime session as null.
      if (acpLoadError) {
        acpActions.clearAcpLoadError(tabId)
      }
      // Manual Reload override: clear cancel fence then authoritative load.
      reloadDetail(effectiveConversationId, { reason: "manual_reload" })
    }, [
      acpActions,
      acpLoadError,
      dbConversationId,
      effectiveConversationId,
      reloadDetail,
      tabId,
    ])
    // Open (or re-activate) the singleton draft tab BEFORE closing the failing
    // tab. closeTab auto-creates a replacement draft when it removes the last
    // tab, and `openNewConversationTab` reads `rawTabsRef.current` which
    // wouldn't yet reflect either pending update if we closed first — the
    // singleton check would miss the replacement and we'd end up with two
    // drafts. Doing it in this order means the second `setTabs` (closeTab)
    // runs against the result of the first.
    const handleOpenNewSession = useCallback(() => {
      if (!folder) return
      // Retry-from-error: user wants a fresh draft in the same conversation
      // context, so inherit the active tab's agent when the folder has no
      // pinned default.
      openNewConversationTab(
        folder.id,
        workingDirForConnection ?? folder.path,
        {
          inheritFromActive: true,
        }
      )
      closeTab(tabId)
    }, [
      closeTab,
      folder,
      openNewConversationTab,
      tabId,
      workingDirForConnection,
    ])

    // Delegation child tab: adopt live ownership + kickoff before detail races.
    useEffect(() => {
      if (!delegatedOpenIntent?.liveOwnsActiveTurn) return
      if (effectiveConversationId <= 0) return
      setLiveOwnsActiveTurn(
        effectiveConversationId,
        true,
        delegatedOpenIntent.kickoffTask
      )
    }, [delegatedOpenIntent, effectiveConversationId, setLiveOwnsActiveTurn])

    const acpLoadErrorBanner =
      hasPersistedConversation && acpLoadError ? (
        <div
          role="alert"
          className="flex w-full items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive"
        >
          <AlertCircle aria-hidden="true" className="h-4 w-4 shrink-0" />
          <span
            className="min-w-0 flex-1 overflow-hidden text-ellipsis whitespace-nowrap"
            title={acpLoadError}
          >
            {acpLoadError}
          </span>
          {canShowDetailErrorActions && (
            <>
              <button
                type="button"
                onClick={handleReloadDetail}
                disabled={detailLoading}
                aria-busy={detailLoading}
                className="flex shrink-0 items-center gap-1 rounded border border-destructive/40 px-2 py-0.5 font-medium transition-colors hover:bg-destructive/10 disabled:pointer-events-none disabled:opacity-50"
              >
                {detailLoading ? (
                  <Loader2
                    aria-hidden="true"
                    className="h-3 w-3 animate-spin"
                  />
                ) : (
                  <RefreshCw aria-hidden="true" className="h-3 w-3" />
                )}
                {tMessageList("errorActionReload")}
              </button>
              <button
                type="button"
                onClick={handleOpenNewSession}
                className="flex shrink-0 items-center gap-1 rounded border border-destructive/40 px-2 py-0.5 font-medium transition-colors hover:bg-destructive/10"
              >
                <Plus aria-hidden="true" className="h-3 w-3" />
                {tMessageList("errorActionNewSession")}
              </button>
            </>
          )}
        </div>
      ) : null

    const goalControlValue = useMemo<GoalControlValue>(() => {
      const live =
        conn.connectionId !== null &&
        (connStatus === "connected" || connStatus === "prompting") &&
        !conn.isViewer &&
        !interactionLocked
      return {
        onGoalControl: live
          ? (action) => {
              void acpActions.goalControl(tabId, action)
            }
          : null,
      }
    }, [
      acpActions,
      conn.connectionId,
      conn.isViewer,
      connStatus,
      interactionLocked,
      tabId,
    ])

    const waitingForSubagentsArmedAtMs = (() => {
      const waiting = conn.waitingForSubagents
      if (!waiting) return null
      const armedAtMs = Date.parse(waiting.armed_at)
      return Number.isFinite(armedAtMs) ? armedAtMs : Date.now()
    })()

    const handleResumeRoot = useCallback(() => {
      handleSend({
        blocks: [{ type: "text", text: ROOT_ORCHESTRATION_RESUME_PROMPT }],
        displayText: ROOT_ORCHESTRATION_RESUME_PROMPT,
      })
    }, [handleSend])

    const handleOpenRootConversation = useCallback(
      async (rootConversationId: number) => {
        let summary = useAppWorkspaceStore
          .getState()
          .conversations.find((row) => row.id === rootConversationId)
        if (!summary) {
          await refreshConversations()
          summary = useAppWorkspaceStore
            .getState()
            .conversations.find((row) => row.id === rootConversationId)
        }
        if (!summary || summary.folder_id <= 0) return
        await openTab(
          summary.folder_id,
          summary.id,
          summary.agent_type,
          true,
          summary.title ?? undefined
        )
      },
      [openTab, refreshConversations]
    )

    const messageListNode = (
      <GoalControlProvider value={goalControlValue}>
        <MessageListView
          conversationId={effectiveConversationId}
          agentType={selectedAgent}
          workspaceRootPath={folder?.path ?? null}
          connStatus={connStatus}
          isActive={isActive}
          sendSignal={sendSignal}
          detailLoading={detailLoading}
          detailError={detailError}
          acpLoadError={acpLoadError}
          acpLoadErrorCode={connLoadErrorCode}
          hideEmptyState={!hasPersistedConversation || hasSentMessage}
          onReload={canShowDetailErrorActions ? handleReloadDetail : undefined}
          onNewSession={
            canShowDetailErrorActions ? handleOpenNewSession : undefined
          }
          initialHistoryScrollEligible={initialHistoryScrollEligible}
          historyLoadComplete={detail != null}
          focusTurnAnchor={focusTurnAnchor}
          waitingForSubagentsArmedAtMs={waitingForSubagentsArmedAtMs}
          onResumeRoot={handleResumeRoot}
          onOpenRootConversation={handleOpenRootConversation}
        />
      </GoalControlProvider>
    )

    // Live-feedback bar gating + the "agent never read your note" resend fallback.
    // Enqueue rather than `handleSend`: this fallback fires on a turn-end race
    // where the backend already reports no active turn but the frontend may still
    // read `connStatus === "prompting"`, and `handleSend` no-ops unless
    // "connected" — which would silently drop the note. The message queue holds it
    // (visible above the composer) and auto-flushes when the turn completes, so
    // the user's note is never lost.
    const feedbackEnabled = useFeedbackEnabled()
    const resendFeedbackAsPrompt = useCallback(
      (text: string) => {
        if (interactionLocked) return
        mqEnqueue(
          { blocks: [{ type: "text", text }], displayText: text },
          selectedModeId
        )
      },
      [interactionLocked, mqEnqueue, selectedModeId]
    )
    const feedback = useSessionFeedback({
      connectionId: conn.connectionId,
      connStatus,
      enabled: feedbackEnabled,
      interactionLocked,
      onResendAsPrompt: resendFeedbackAsPrompt,
      onDelegateViewerOnly: handleDelegateViewerOnlyRejection,
    })

    // Locked delegates without a canonical connection should show the waiting
    // row rather than a stale generic ACP owner disconnect error.
    const shellConnectionError =
      isDelegateConversation && interactionLocked && conn.connectionId == null
        ? null
        : conn.error

    return (
      <ConversationShell
        topBanner={
          <>
            <SessionConfigStaleBanner contextKey={tabId} />
            <ToolWatchdogBanner contextKey={tabId} />
            <BackgroundTasksChip contextKey={tabId} />
            {isDelegateConversation ? (
              <DelegateAccessStatus
                access={delegateAccess}
                loading={delegateAccessLoading}
                connectionId={conn.connectionId ?? null}
                syncError={delegateSyncError}
              />
            ) : null}
          </>
        }
        routeNotice={<DelegationRouteNotice contextKey={tabId} />}
        status={connStatus}
        promptCapabilities={conn.promptCapabilities}
        defaultPath={workingDirForConnection}
        folderId={ownFolderId}
        agentName={getAgentLabel(selectedAgent)}
        error={shellConnectionError}
        claudeApiRetry={conn.claudeApiRetry}
        pendingPermission={conn.pendingPermission}
        pendingQuestion={conn.pendingQuestion}
        pendingAskQuestion={conn.pendingAskQuestion}
        pendingPlanApproval={conn.pendingPlanApproval}
        onFocus={handleFocus}
        onSend={handleSend}
        sendClearMode={conn.sharedSession ? "after-admission" : "immediate"}
        onCancel={handleCancel}
        onRespondPermission={handleRespondPermission}
        onAnswerQuestion={handleAnswerQuestion}
        onAnswerAskQuestion={handleAnswerAskQuestion}
        onAnswerPlanApproval={handleAnswerPlanApproval}
        modes={connectionModes}
        configOptions={connectionConfigOptions}
        modeLoading={modeLoading}
        configOptionsLoading={configOptionsLoading}
        selectorsLoading={selectorsLoading}
        selectedModeId={selectedModeId}
        onModeChange={handleModeChange}
        onConfigOptionChange={handleSetConfigOption}
        agentType={selectedAgent}
        availableCommands={connectionCommands}
        attachmentTabId={tabId}
        draftStorageKey={draftStorageKey}
        hideInput={isWelcomeMode || Boolean(acpLoadError)}
        composerBanner={acpLoadErrorBanner}
        feedbackList={
          feedback.showList ? (
            <FeedbackNotesDisplay notes={feedback.notes} />
          ) : null
        }
        onAddFeedback={
          feedback.featureEnabled ? feedback.openDialog : undefined
        }
        feedbackAddDisabled={!feedback.canSubmit}
        isActive={isActive}
        showActiveFlow={showActiveFlow}
        queue={conn.sharedSession ? undefined : msgQueue}
        sharedQueue={conn.sharedSession?.queue}
        onSharedQueueCancel={
          conn.sharedSession ? handleSharedQueueCancel : undefined
        }
        onEnqueue={conn.sharedSession ? undefined : mqEnqueue}
        onQueueReorder={conn.sharedSession ? undefined : mqReorder}
        onQueueEdit={conn.sharedSession ? undefined : handleQueueEdit}
        onQueueDelete={conn.sharedSession ? undefined : mqRemove}
        editingItemId={mqEditingItemId}
        editingDraftText={editingQueueDraftText}
        editingDraftBlocks={editingQueueDraftBlocks}
        isEditingQueueItem={mqEditingItemId != null}
        onSaveQueueEdit={handleSaveQueueEdit}
        onCancelQueueEdit={handleQueueCancelEdit}
        onForkSend={
          !interactionLocked &&
          !conn.sharedSession &&
          connStatus === "connected" &&
          hasPersistedConversation &&
          conn.supportsFork &&
          !conn.waitingForSubagents &&
          !forkSendBlockedByQueue(msgQueue.length)
            ? handleForkSend
            : undefined
        }
        waitingForSubagents={conn.waitingForSubagents}
        draftRestore={promptDraftRestore}
        interactionLocked={interactionLocked}
        showReconnect={showReconnect}
        onReconnect={showReconnect ? onReconnect : undefined}
        queuePaused={queuePausedByTerminalDisconnect}
        onResumeQueue={
          queuePausedByTerminalDisconnect ? onResumeQueue : undefined
        }
      >
        {isWelcomeMode ? (
          <ScrollArea
            className="relative isolate h-full min-h-0"
            x="hidden"
            y="scroll"
          >
            <div className="flex min-h-full flex-col">
              <div className="flex-1" />
              <div className="mx-auto flex w-full max-w-3xl shrink-0 flex-col gap-6 px-4 py-4">
                <WelcomeHero />
                <QuickActions
                  onSelect={handleQuickAction}
                  agentType={selectedAgent}
                />
                <div className="flex justify-center">
                  <AgentSelector
                    defaultAgentType={selectedAgent}
                    onSelect={handleAgentSelect}
                    onFallback={handleAgentFallback}
                    onAgentsLoaded={(agents) => {
                      setAgentsLoaded(true)
                      setUsableAgentCount(
                        agents.filter(
                          (agent) => agent.enabled && agent.available
                        ).length
                      )
                    }}
                    onOpenAgentsSettings={handleOpenAgentsSettings}
                    disabled={isConnecting || dbConversationId != null}
                  />
                </div>
                {composerBlockedMessage ? (
                  <div className="flex w-full items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
                    <button
                      type="button"
                      onClick={handleOpenAgentsSettings}
                      title={composerBlockedMessage}
                      className="min-w-0 flex-1 cursor-pointer overflow-hidden text-ellipsis whitespace-nowrap text-left transition-colors hover:text-destructive/80"
                    >
                      {composerBlockedMessage}
                    </button>
                    {selectedAgentNotInstalled ? (
                      <button
                        type="button"
                        onClick={() => setComposerDiagnosticsOpen(true)}
                        className="shrink-0 rounded border border-destructive/40 px-2 py-0.5 font-medium transition-colors hover:bg-destructive/10"
                      >
                        {tDiag("button")}
                      </button>
                    ) : null}
                  </div>
                ) : null}
                <ChatInput
                  // composerConnStatus (not connStatus): a chat draft mid-reconnect
                  // reads "connecting" until the connection's cwd matches, so the
                  // send affordance stays disabled until handleSend would accept it.
                  status={composerConnStatus}
                  promptCapabilities={conn.promptCapabilities}
                  defaultPath={workingDirForConnection}
                  folderId={ownFolderId}
                  agentName={getAgentLabel(selectedAgent)}
                  onFocus={handleFocus}
                  onSend={handleSend}
                  sendClearMode={
                    conn.sharedSession ? "after-admission" : "immediate"
                  }
                  onCancel={handleCancel}
                  waitingForSubagents={conn.waitingForSubagents}
                  draftRestore={promptDraftRestore}
                  interactionLocked={interactionLocked}
                  modes={connectionModes}
                  configOptions={connectionConfigOptions}
                  modeLoading={modeLoading}
                  configOptionsLoading={configOptionsLoading}
                  selectorsLoading={selectorsLoading}
                  selectedModeId={selectedModeId}
                  onModeChange={handleModeChange}
                  onConfigOptionChange={handleSetConfigOption}
                  agentType={selectedAgent}
                  availableCommands={connectionCommands}
                  attachmentTabId={tabId}
                  draftStorageKey={draftStorageKey}
                  isActive={isActive}
                  showActiveFlow={showActiveFlow}
                  onAddFeedback={
                    feedback.featureEnabled ? feedback.openDialog : undefined
                  }
                  feedbackAddDisabled={!feedback.canSubmit}
                  injectContent={quickActionInject}
                  onInjectConsumed={handleQuickActionConsumed}
                  flush
                  tall
                />
              </div>
              <div className="flex-1" />
              <div className="mx-auto w-full max-w-3xl shrink-0 px-4 pb-6">
                <WelcomeTip />
              </div>
            </div>
          </ScrollArea>
        ) : showDraftHeader ? (
          <div className="flex h-full min-h-0 flex-col">
            <div className="px-4 pt-3 pb-2">
              <AgentSelector
                defaultAgentType={selectedAgent}
                onSelect={handleAgentSelect}
                onFallback={handleAgentFallback}
                onAgentsLoaded={(agents) => {
                  setAgentsLoaded(true)
                  setUsableAgentCount(
                    agents.filter((agent) => agent.enabled && agent.available)
                      .length
                  )
                }}
                onOpenAgentsSettings={handleOpenAgentsSettings}
                disabled={isConnecting || dbConversationId != null}
              />
              {composerBlockedMessage ? (
                <div className="mt-2 flex w-full items-center gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
                  <button
                    type="button"
                    onClick={handleOpenAgentsSettings}
                    title={composerBlockedMessage}
                    className="min-w-0 flex-1 cursor-pointer overflow-hidden text-ellipsis whitespace-nowrap text-left transition-colors hover:text-destructive/80"
                  >
                    {composerBlockedMessage}
                  </button>
                  {selectedAgentNotInstalled ? (
                    <button
                      type="button"
                      onClick={() => setComposerDiagnosticsOpen(true)}
                      className="shrink-0 rounded border border-destructive/40 px-2 py-0.5 font-medium transition-colors hover:bg-destructive/10"
                    >
                      {tDiag("button")}
                    </button>
                  ) : null}
                </div>
              ) : null}
            </div>
            <div className="min-h-0 flex-1">{messageListNode}</div>
          </div>
        ) : (
          messageListNode
        )}
        <FeedbackDialog
          open={feedback.dialogOpen}
          onOpenChange={(open) => {
            if (open) feedback.openDialog()
            else feedback.closeDialog()
          }}
          onSubmit={feedback.submit}
          submitting={feedback.submitting}
          agentName={getAgentLabel(selectedAgent)}
        />
        <AgentDiagnosticsDialog
          open={composerDiagnosticsOpen}
          onOpenChange={setComposerDiagnosticsOpen}
          agentType={selectedAgent}
        />
      </ConversationShell>
    )
  }
)
