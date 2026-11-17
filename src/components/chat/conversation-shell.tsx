import type { ConversationFolderPickerOverride } from "@/components/chat/conversation-context-bar"
import type { ReactNode } from "react"
import type {
  AgentType,
  ConnectionStatus,
  ContinuationWaitingProjection,
  PendingPlanApprovalState,
  PendingQuestionState,
  PlanApprovalAnswer,
  PromptCapabilitiesInfo,
  PromptDraft,
  PromptInputBlock,
  QuestionAnswer,
  SessionConfigOptionInfo,
  AsyncTaskRecord,
  SessionFailureRecord,
  SnapshotTruncation,
  SessionModeInfo,
  AvailableCommandInfo,
  RemoteControlInfo,
} from "@/lib/types"
import { ComposerStatusStrips } from "@/components/chat/composer-status-strips"
import { RemoteControlStrip } from "@/components/chat/remote-control-strip"
import type {
  PendingPermission,
  PendingQuestion,
  ClaudeApiRetryState,
} from "@/contexts/acp-connections-context"
import type { QueuedMessage } from "@/hooks/use-message-queue"
import type { SharedQueuedPrompt } from "@/lib/snapshot-denormalize"
import { useTranslations } from "next-intl"
import { ChatInput } from "@/components/chat/chat-input"
import { ChatWidthHandles } from "@/components/chat/chat-width-handles"
import { chatGutterStyle } from "@/lib/chat-content-width"
import { cn } from "@/lib/utils"
import type {
  ComposerInjectContent,
  PromptDraftRestore,
} from "@/components/chat/message-input"
import { PermissionDialog } from "@/components/chat/permission-dialog"
import { QuestionDialog } from "@/components/chat/question-dialog"
import { AskQuestionCard } from "@/components/chat/ask-question-card"
import { PlanApprovalCard } from "@/components/chat/plan-approval-card"
import { MarkdownImageProvider } from "@/components/ai-elements/markdown-local-image"

interface ConversationShellProps {
  status: ConnectionStatus | null
  promptCapabilities: PromptCapabilitiesInfo
  defaultPath?: string
  /** Authoritative folder scope for mention search (`ownFolderId`, not `0`). */
  folderId?: number | null
  agentName?: string
  /** The in-flight retry line under the composer (see
   *  `ComposerStatusStrips`). The session's errors are notifications, raised by
   *  the connections provider — none is drawn here. */
  claudeApiRetry: ClaudeApiRetryState | null
  /** AIR typed session failures for this connection (active + resolved); the
   *  dock draws only the in-flight retry incidents. Omit/empty renders
   *  nothing. */
  sessionFailures?: SessionFailureRecord[]
  /** Background tasks and the owner stop handler. The running row is
   *  MessageListView's live activity; the session and canvas surfaces also
   *  pass these into the shell. */
  asyncTasks?: AsyncTaskRecord[]
  onStopAsyncTask?: (taskId: string) => Promise<boolean>
  /** Bounded-snapshot omission counts. Persistent status copy above the
   *  transcript — not a toast and not a refetch. `null` when the snapshot
   *  reported no omission. */
  snapshotTruncation?: SnapshotTruncation | null
  /** Closes an incident strip, taking every record it stands for. Passed for
   *  every surface with a live store — dismissing is client-local, so viewers
   *  get it too. */
  onSessionFailureDismiss?: (ids: string[]) => void
  /** Claude Code's Remote Control while it is on; its strip shows the
   *  session's link and QR code. Omit/`null` renders nothing. */
  remoteControl?: RemoteControlInfo | null
  /** Turns Remote Control off (sends `/remote-control off`). Omitted for
   *  surfaces without a connection of their own — the button is then hidden. */
  onRemoteControlOff?: () => void
  pendingPermission: PendingPermission | null
  pendingQuestion: PendingQuestion | null
  /** Awaiting-answer multiple-choice `ask_user_question`. */
  pendingAskQuestion: PendingQuestionState | null
  /** Awaiting-decision Grok `exit_plan_mode` approval. */
  pendingPlanApproval: PendingPlanApprovalState | null
  onFocus: () => void
  onSend: (
    draft: PromptDraft,
    modeId?: string | null
  ) => void | Promise<unknown>
  sendClearMode?: "immediate" | "after-admission"
  onCancel: () => void
  onRespondPermission: (requestId: string, optionId: string) => void
  onAnswerQuestion: (answer: string) => void
  onAnswerAskQuestion: (
    questionId: string,
    answer: QuestionAnswer
  ) => void | Promise<void>
  onAnswerPlanApproval: (
    approvalId: string,
    answer: PlanApprovalAnswer
  ) => void | Promise<void>
  children: ReactNode
  modes?: SessionModeInfo[]
  configOptions?: SessionConfigOptionInfo[]
  modeLoading?: boolean
  configOptionsLoading?: boolean
  selectorsLoading?: boolean
  selectedModeId?: string | null
  onModeChange?: (modeId: string) => void
  onConfigOptionChange?: (configId: string, valueId: string) => void
  agentType?: AgentType | null
  availableCommands?: AvailableCommandInfo[] | null
  /** The prompt the agent predicts the user will send next; the empty
   *  composer offers it (see `MessageInput.promptSuggestion`). */
  promptSuggestion?: string | null
  attachmentTabId?: string | null
  /** Pass-through: see `MessageInput`. */
  folderPickerOverride?: ConversationFolderPickerOverride
  draftStorageKey?: string | null
  /** Pass-through: see `MessageInput.getSentHistory`. */
  getSentHistory?: () => string[]
  hideInput?: boolean
  /** Optional banner rendered in the composer dock, where the input sits.
   *  Used with `hideInput` to explain WHY the composer is unavailable (e.g.
   *  the agent failed to load this session) without hijacking the message
   *  area above. Renders nothing when omitted. */
  composerBanner?: ReactNode
  /** Optional read-only live-feedback notes list rendered just above the
   *  composer (see `FeedbackNotesDisplay`). Renders nothing when there are no
   *  notes for the current turn. */
  feedbackList?: ReactNode
  /** Open the live-feedback dialog from the composer "+" menu (hidden when
   *  omitted / feature off). */
  onAddFeedback?: () => void
  /** Grey out the live-feedback "+" entry when a note can't be sent right now. */
  feedbackAddDisabled?: boolean
  isActive?: boolean
  /** Show the composer's flowing active-session border (tiled multi-session
   *  active tab only). Threaded straight through to the composer. */
  showActiveFlow?: boolean
  queue?: QueuedMessage[]
  sharedQueue?: SharedQueuedPrompt[]
  onSharedQueueCancel?: (queueItemId: string) => Promise<void>
  onSharedQueueFailedDismiss?: (queueItemId: string) => void
  onEnqueue?: (draft: PromptDraft, modeId: string | null) => void
  onQueueReorder?: (items: QueuedMessage[]) => void
  onQueueEdit?: (id: string) => void
  onQueueDelete?: (id: string) => void
  /** Insert one queued item into the RUNNING turn over the session's
   *  live-feedback channel; threaded straight through to the composer's
   *  queue list. See `ChatInputProps.onQueueSteer`. */
  onQueueSteer?: (id: string) => Promise<void> | void
  editingItemId?: string | null
  editingDraftText?: string | null
  editingDraftBlocks?: PromptInputBlock[] | null
  isEditingQueueItem?: boolean
  onSaveQueueEdit?: (draft: PromptDraft) => void
  onCancelQueueEdit?: () => void
  onForkSend?: (draft: PromptDraft, modeId?: string | null) => void
  /** Durable continuation waiting projection for this conversation. */
  waitingForSubagents?: ContinuationWaitingProjection | null
  /** Restore a rejected direct-send draft into the composer. */
  draftRestore?: PromptDraftRestore | null
  /** Send the draft into the RUNNING turn over the session's live-feedback
   *  channel. Present only for sessions with a working delivery channel;
   *  threaded straight through to the composer. `blocks` carries the full
   *  draft when it holds more than plain text (image attachments, file
   *  badges); `text` stays the recorded/display form. Must stay in sync with
   *  `MessageInputProps.onSteer` — the optional second parameter makes a
   *  stale one-arg declaration here assignable, so tsc would NOT catch a
   *  wrapper that silently drops the blocks. */
  onSteer?: (text: string, blocks?: PromptInputBlock[]) => Promise<void>
  /** Which channel `onSteer` rides (picks the composer's honest copy);
   *  threaded straight through. See `MessageInput`. */
  steerChannel?: "native" | "pull"
  /** Optional banner pinned to the top of the panel, above the message area
   *  (e.g. the "restart to apply" config-stale banner). Renders nothing when
   *  omitted. */
  topBanner?: ReactNode
  /** Optional secondary notice under the top banner (e.g. route degraded). */
  routeNotice?: ReactNode
  /** Terminal-disconnect reconnect affordance (Task 5 wires the latch). */
  showReconnect?: boolean
  onReconnect?: () => void
  /** Queue paused by terminal disconnect — threaded to the queue display. */
  queuePaused?: boolean
  onResumeQueue?: () => void
  /**
   * Independent access capability lock for delegated children in viewer-only
   * mode. Propagated to composer + question answers; deliberately NOT to
   * PermissionDialog (approve/reject stay enabled).
   */
  interactionLocked?: boolean
  /** Content pushed into the docked composer from outside it — currently a
   *  quoted transcript selection. Cleared by the host via `onInjectConsumed`
   *  once the composer has taken it. */
  injectContent?: ComposerInjectContent | null
  onInjectConsumed?: () => void
  /** Give the chat column drag handles on its edges (`ChatWidthHandles`) and
   *  the window-edge gutter they need. Only for the conversation tab: a canvas
   *  card renders this shell inside the board's zoom transform, where the
   *  handles' layout maths would be off by the zoom factor, and a card has no
   *  window edge to keep clear. */
  resizableWidth?: boolean
  /**
   * Shared sessions have no local queue. Enter while the turn is prompting
   * still calls `onSend` so the server can queue the follow-up.
   */
  deliverWhilePrompting?: boolean
}

export function ConversationShell({
  status,
  promptCapabilities,
  defaultPath,
  folderId = null,
  agentName,
  claudeApiRetry,
  sessionFailures,
  snapshotTruncation = null,
  onSessionFailureDismiss,
  remoteControl,
  onRemoteControlOff,
  pendingPermission,
  pendingQuestion,
  pendingAskQuestion,
  pendingPlanApproval,
  onFocus,
  onSend,
  sendClearMode = "immediate",
  onCancel,
  onRespondPermission,
  onAnswerQuestion,
  onAnswerAskQuestion,
  onAnswerPlanApproval,
  children,
  modes,
  configOptions,
  modeLoading = false,
  configOptionsLoading = false,
  selectorsLoading = false,
  selectedModeId,
  onModeChange,
  onConfigOptionChange,
  agentType,
  availableCommands,
  promptSuggestion,
  attachmentTabId,
  folderPickerOverride,
  draftStorageKey,
  getSentHistory,
  hideInput = false,
  composerBanner,
  feedbackList,
  onAddFeedback,
  feedbackAddDisabled,
  isActive,
  showActiveFlow,
  queue,
  sharedQueue,
  onSharedQueueCancel,
  onSharedQueueFailedDismiss,
  onEnqueue,
  onQueueReorder,
  onQueueEdit,
  onQueueDelete,
  onQueueSteer,
  editingItemId,
  editingDraftText,
  editingDraftBlocks,
  isEditingQueueItem,
  onSaveQueueEdit,
  onCancelQueueEdit,
  onForkSend,
  waitingForSubagents = null,
  draftRestore = null,
  onSteer,
  steerChannel,
  topBanner,
  routeNotice,
  showReconnect = false,
  onReconnect,
  queuePaused = false,
  onResumeQueue,
  interactionLocked = false,
  injectContent,
  onInjectConsumed,
  resizableWidth = false,
  deliverWhilePrompting = false,
}: ConversationShellProps) {
  return (
    // The gutter keeps the chat column (transcript and composer alike) off
    // the window edge, so its width handles stay grabbable at any width.
    <div
      className="relative flex h-full min-h-0 flex-col"
      style={resizableWidth ? chatGutterStyle : undefined}
    >
      {topBanner}
      {routeNotice}

      {/* Above the transcript, not down in the composer dock: Remote Control
          is the session's standing state, and pinning it here keeps it still
          while the messages scroll under it. The dock below is for things that
          come and go with the turn or the connection
          (`ComposerStatusStrips`). Work running past a turn — background
          tasks, sub-agents, delegations — is the transcript's live activity
          row's "N running" (`MessageListView`'s `liveRuns`). */}
      {remoteControl && (
        <RemoteControlStrip
          remoteControl={remoteControl}
          onTurnOff={onRemoteControlOff}
          canTurnOff={status === "connected"}
        />
      )}
      {snapshotTruncation ? (
        <SnapshotLimitedNotice truncation={snapshotTruncation} />
      ) : null}
      <div className={cn("flex-1 min-h-0", resizableWidth && "relative")}>
        {children}
        {resizableWidth && <ChatWidthHandles />}
      </div>

      {/* Below the transcript, so outside its image scope: a request or a
          plan names local images the way a reply does, against the same
          folder. */}
      <MarkdownImageProvider rootPath={defaultPath ?? null}>
        <PermissionDialog
          permission={pendingPermission}
          onRespond={onRespondPermission}
          agentType={agentType}
        />
      </MarkdownImageProvider>

      <QuestionDialog
        question={pendingQuestion}
        onAnswer={onAnswerQuestion}
        interactionLocked={interactionLocked}
      />

      {/* Composer dock. The ask-question card sits in normal flow just above the
          feedback list and input — like the permission/question dialogs — so it
          shrinks the message list instead of covering it, while staying aligned
          to the input width. */}
      <div>
        {pendingAskQuestion && pendingAskQuestion.questions.length > 0 && (
          <div className="mx-auto w-full chat-content-w px-4">
            <AskQuestionCard
              question={pendingAskQuestion}
              onAnswer={onAnswerAskQuestion}
              interactionLocked={interactionLocked}
            />
          </div>
        )}
        {pendingPlanApproval && (
          <div className="mx-auto w-full chat-content-w px-4">
            {/* key on approval_id so the card always remounts (fresh in-flight /
                feedback state) if the slot is ever reused for a new approval. */}
            <MarkdownImageProvider rootPath={defaultPath ?? null}>
              <PlanApprovalCard
                key={pendingPlanApproval.approval_id}
                approval={pendingPlanApproval}
                onAnswer={onAnswerPlanApproval}
              />
            </MarkdownImageProvider>
          </div>
        )}

        {composerBanner && (
          <div className="mx-auto w-full chat-content-w px-4 pb-2">
            {composerBanner}
          </div>
        )}

        {!hideInput && feedbackList && (
          <div className="mx-auto w-full chat-content-w px-4">
            {feedbackList}
          </div>
        )}

        {!hideInput && (
          <div className="mx-auto w-full chat-content-w">
            <ChatInput
              status={status}
              promptCapabilities={promptCapabilities}
              defaultPath={defaultPath}
              folderId={folderId}
              agentName={agentName}
              onFocus={onFocus}
              onSend={onSend}
              sendClearMode={sendClearMode}
              onCancel={onCancel}
              waitingForSubagents={waitingForSubagents}
              draftRestore={draftRestore}
              interactionLocked={interactionLocked}
              modes={modes}
              configOptions={configOptions}
              modeLoading={modeLoading}
              configOptionsLoading={configOptionsLoading}
              selectorsLoading={selectorsLoading}
              selectedModeId={selectedModeId}
              onModeChange={onModeChange}
              onConfigOptionChange={onConfigOptionChange}
              agentType={agentType}
              availableCommands={availableCommands}
              promptSuggestion={promptSuggestion}
              attachmentTabId={attachmentTabId}
              folderPickerOverride={folderPickerOverride}
              draftStorageKey={draftStorageKey}
              getSentHistory={getSentHistory}
              isActive={isActive}
              showActiveFlow={showActiveFlow}
              queue={queue}
              sharedQueue={sharedQueue}
              onSharedQueueCancel={onSharedQueueCancel}
              onSharedQueueFailedDismiss={onSharedQueueFailedDismiss}
              onEnqueue={onEnqueue}
              onQueueReorder={onQueueReorder}
              onQueueEdit={onQueueEdit}
              onQueueDelete={onQueueDelete}
              onQueueSteer={onQueueSteer}
              editingItemId={editingItemId}
              editingDraftText={editingDraftText}
              editingDraftBlocks={editingDraftBlocks}
              isEditingQueueItem={isEditingQueueItem}
              onSaveQueueEdit={onSaveQueueEdit}
              onCancelQueueEdit={onCancelQueueEdit}
              onForkSend={onForkSend}
              onSteer={onSteer}
              steerChannel={steerChannel}
              onAddFeedback={onAddFeedback}
              feedbackAddDisabled={feedbackAddDisabled}
              showReconnect={showReconnect}
              onReconnect={onReconnect}
              queuePaused={queuePaused}
              onResumeQueue={onResumeQueue}
              injectContent={injectContent}
              onInjectConsumed={onInjectConsumed}
              deliverWhilePrompting={deliverWhilePrompting}
            />
          </div>
        )}
      </div>

      <ComposerStatusStrips
        status={status}
        claudeApiRetry={claudeApiRetry}
        sessionFailures={sessionFailures}
        onSessionFailureDismiss={onSessionFailureDismiss}
      />
    </div>
  )
}

/** Persistent omission copy. Mounted only when a snapshot carried truncation,
 *  so shells rendered without next-intl never call `useTranslations`. */
function SnapshotLimitedNotice({
  truncation,
}: {
  truncation: SnapshotTruncation
}) {
  const t = useTranslations("Folder.chat")
  const tools = truncation.omitted_tool_calls
  const images = truncation.omitted_images
  return (
    <p
      className="mx-auto w-full max-w-3xl shrink-0 px-4 py-1.5 text-center text-xs text-muted-foreground"
      role="status"
    >
      {tools === 0 && images === 0
        ? t("snapshotDetailsLimited")
        : t("snapshotLimited", { tools, images })}
    </p>
  )
}
