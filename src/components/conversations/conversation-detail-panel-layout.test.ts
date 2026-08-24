import { readFileSync } from "node:fs"
import { resolve } from "node:path"
import * as ts from "typescript"

function hasComponentReturnBeforeCall(
  sourceText: string,
  componentName: string,
  callName: string
): boolean {
  const file = ts.createSourceFile(
    "component.tsx",
    sourceText,
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX
  )
  let component: ts.FunctionDeclaration | ts.FunctionExpression | undefined
  const findComponent = (node: ts.Node) => {
    if (
      (ts.isFunctionDeclaration(node) || ts.isFunctionExpression(node)) &&
      node.name?.text === componentName
    ) {
      component = node
      return
    }
    ts.forEachChild(node, findComponent)
  }
  findComponent(file)
  if (!component?.body) throw new Error(`Missing component: ${componentName}`)
  const componentBody = component.body

  let callStart: number | undefined
  const findCall = (node: ts.Node) => {
    if (
      callStart == null &&
      ts.isCallExpression(node) &&
      ts.isIdentifier(node.expression) &&
      node.expression.text === callName
    ) {
      callStart = node.getStart(file)
      return
    }
    ts.forEachChild(node, findCall)
  }
  findCall(componentBody)
  if (callStart == null) throw new Error(`Missing call: ${callName}`)

  let found = false
  const scan = (node: ts.Node) => {
    if (found || node.getStart(file) >= callStart!) return
    if (node !== component!.body && ts.isFunctionLike(node)) return
    if (ts.isReturnStatement(node)) {
      found = true
      return
    }
    ts.forEachChild(node, scan)
  }
  scan(componentBody)
  return found
}

/** Session UI lives in ConversationSessionSurface (extracted from TabView). */
const source = readFileSync(
  resolve(
    process.cwd(),
    "src/components/conversations/conversation-session-surface.tsx"
  ),
  "utf8"
)
/** Multi-tab shell still in conversation-detail-panel. */
const panelSource = readFileSync(
  resolve(
    process.cwd(),
    "src/components/conversations/conversation-detail-panel.tsx"
  ),
  "utf8"
)
const welcomeHeroSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/welcome-hero.tsx"),
  "utf8"
)
const chatInputSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/chat-input.tsx"),
  "utf8"
)
const messageInputSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/message-input.tsx"),
  "utf8"
)
const conversationShellSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/conversation-shell.tsx"),
  "utf8"
)
const globalsCssSource = readFileSync(
  resolve(process.cwd(), "src/app/globals.css"),
  "utf8"
)
const workspaceLayoutSource = readFileSync(
  resolve(process.cwd(), "src/app/workspace/layout.tsx"),
  "utf8"
)
const tabBarSource = readFileSync(
  resolve(process.cwd(), "src/components/tabs/tab-bar.tsx"),
  "utf8"
)
const messageListViewSource = readFileSync(
  resolve(process.cwd(), "src/components/message/message-list-view.tsx"),
  "utf8"
)

describe("ConversationDetailPanel inactive panel paint isolation", () => {
  it("uses native hidden outside tile mode instead of visibility-only hiding", () => {
    expect(panelSource).toContain("hidden={!visible}")
    expect(panelSource).not.toContain(
      '"absolute inset-0 invisible pointer-events-none"'
    )
  })
})

describe("ConversationDetailPanel draft route override create wiring", () => {
  it("passes draft delegationRouteOverride as the last arg to first createConversation", () => {
    // Exact production call site (not just a nearby comment).
    expect(source).toMatch(
      /createConversation\(\s*folderId,\s*selectedAgent,\s*title,\s*sendOwnTab\?\.delegationRouteOverride \?\? null\s*\)/
    )
  })

  it("passes draft delegationRouteOverride as the last arg to first createChatConversation", () => {
    expect(source).toMatch(
      /createChatConversation\(\s*selectedAgent,\s*title,\s*chatExistingDir,\s*sendOwnTab\?\.delegationRouteOverride \?\? null\s*\)/
    )
  })

  it("threads the same override into connect lifecycle (conversationId + route)", () => {
    expect(source).toContain(
      "delegationRouteOverride: ownTab?.delegationRouteOverride ?? undefined"
    )
    expect(source).toContain("conversationId: dbConversationId ?? undefined")
  })
})

describe("ConversationDetailPanel new conversation layout", () => {
  it("keeps the new-conversation input in the welcome panel with the original scroll layout", () => {
    expect(source).toContain(
      "hideInput={isWelcomeMode || Boolean(acpLoadError)}"
    )

    const welcomeBranchStart = source.indexOf("{isWelcomeMode ? (")
    const nextBranchStart = source.indexOf(
      ") : showDraftHeader ?",
      welcomeBranchStart
    )

    expect(welcomeBranchStart).toBeGreaterThan(-1)
    expect(nextBranchStart).toBeGreaterThan(welcomeBranchStart)

    const welcomeBranch = source.slice(welcomeBranchStart, nextBranchStart)
    expect(welcomeBranch).toContain("<ChatInput")
    // The welcome page scrolls with the app's shared overlay scrollbar (the
    // sidebar's os-theme-codeg bar), not the platform's native one. `min-h-full`
    // on the inner column preserves the spacer layout the old
    // `overflow-y-auto` flex column had.
    expect(welcomeBranch).toContain("<ScrollArea")
    expect(welcomeBranch).toContain('className="flex min-h-full flex-col"')
    expect(welcomeBranch).not.toContain("overflow-y-auto")
    expect(welcomeBranch).not.toContain("WelcomeBackdrop")
    // The welcome input is flushed: the welcome column already supplies px-4, so
    // the input must not double-pad (would make it narrower than the cards).
    expect(welcomeBranch).toContain("flush")
    // The welcome composer is taller (min-h-30) than the compact default kept by
    // active/historical conversations.
    expect(welcomeBranch).toContain("tall")
  })

  it("centers the welcome agent pills via AgentSelector align, not a parent justify-center", () => {
    // AgentSelector is `@container flex-1` so it can measure overflow. That
    // makes a wrapping `flex justify-center` a no-op — the row is already
    // full width and the pills sit at the start. The welcome page has to
    // pass the intent down as `align="center"`.
    const welcomeBranchStart = source.indexOf("{isWelcomeMode ? (")
    const nextBranchStart = source.indexOf(
      ") : showDraftHeader ?",
      welcomeBranchStart
    )
    expect(welcomeBranchStart).toBeGreaterThan(-1)
    expect(nextBranchStart).toBeGreaterThan(welcomeBranchStart)

    const welcomeBranch = source.slice(welcomeBranchStart, nextBranchStart)
    expect(welcomeBranch).toMatch(/<AgentSelector[\s\S]*?align="center"/)
  })

  it("snaps the hidden keep-alive tab so `transition-all` descendants don't ghost", () => {
    // Inactive tabs stay mounted and hide with `visibility: hidden` (`invisible`).
    // In Tailwind v4 `transition-all` transitions `visibility` too, so welcome
    // controls (agent pills, quick-action tabs, composer buttons) would linger
    // 150–300ms as ghosts over the newly-active conversation. The wrapper must
    // carry `conversation-tab-hidden` next to `invisible`, and globals.css must
    // drop transitions for that subtree so visibility snaps. Both halves are
    // required — assert they stay coupled.
    expect(panelSource).toContain(
      '"conversation-tab-hidden absolute inset-0 invisible pointer-events-none"'
    )
    expect(globalsCssSource).toContain(".conversation-tab-hidden *")
    const rule = globalsCssSource.slice(
      globalsCssSource.indexOf(".conversation-tab-hidden,"),
      globalsCssSource.indexOf(".conversation-tab-hidden,") + 200
    )
    expect(rule).toContain("transition-property: none !important")
  })

  // Regression: with a workspace background image on, every covering surface is
  // TRANSPARENT rather than opaque, so a hidden-but-mounted subtree that still
  // paints is visible straight through it. `visibility` inherits, but a
  // descendant can opt back in — Monaco's DiffEditorWidget writes an inline
  // `visibility: visible` on its two panes — so an open git-diff file tab showed
  // through the full-page routes (task board / automations / token usage) and
  // through the conversation overlay in conversation-only mode, while a plain
  // file tab (no inline visibility) hid correctly.
  it("re-hides Monaco's diff panes inside a hidden keep-alive subtree", () => {
    const selector = ".conversation-tab-hidden .monaco-diff-editor > .editor"
    expect(globalsCssSource).toContain(selector)
    const rule = globalsCssSource.slice(
      globalsCssSource.indexOf(selector),
      globalsCssSource.indexOf(selector) + 120
    )
    // Only `!important` outranks Monaco's inline declaration.
    expect(rule).toContain("visibility: hidden !important")
  })

  it("marks every hidden keep-alive subtree with the hardening class", () => {
    // Under a full-page workbench route (desktop + mobile shells).
    expect(workspaceLayoutSource).toContain(
      '!isConversations && "conversation-tab-hidden invisible"'
    )
    // The FILE column under the conversation overlay — this is the one that
    // hosts git-diff tabs.
    expect(workspaceLayoutSource).toContain(
      'mode === "conversation" && "conversation-tab-hidden invisible"'
    )
    // The conversation column under the files-maximized overlay.
    expect(workspaceLayoutSource).toContain(
      'filesMaximized && "conversation-tab-hidden invisible"'
    )
  })

  it("does not render a decorative welcome backdrop", () => {
    expect(welcomeHeroSource).not.toContain("export function WelcomeBackdrop")
    expect(welcomeHeroSource).not.toContain("bg-gradient-to-r")
  })

  it("uses the shared attached folder branch picker treatment for all chat inputs", () => {
    expect(source).not.toContain("attachFolderBranchPickerToInput")
    expect(conversationShellSource).not.toContain(
      "attachFolderBranchPickerToInput"
    )
    expect(messageInputSource).not.toContain("attachFolderBranchPickerToInput")
    expect(messageInputSource).toContain(
      "const folderBranchPickerAttached = hasFolderBranchPicker"
    )
    expect(messageInputSource).not.toContain("rounded-b-none")

    const pickerStart = messageInputSource.indexOf(
      "{hasFolderBranchPicker && ("
    )
    // The picker row is the last thing inside the composer wrapper; the
    // server-file dialog that follows it sits outside, so it anchors the slice.
    const pickerEnd = messageInputSource.indexOf(
      "{!attach.showNativePaperclip && (",
      pickerStart
    )
    expect(pickerStart).toBeGreaterThan(-1)
    expect(pickerEnd).toBeGreaterThan(pickerStart)

    const pickerWrapper = messageInputSource.slice(pickerStart, pickerEnd)
    expect(messageInputSource).toContain(
      '"overflow-hidden rounded-xl transition-colors"'
    )
    expect(messageInputSource).not.toContain("bg-muted/60")
    expect(messageInputSource).toContain(': "contents"')
    // The rounded border lives in the always-on base (so the active-session flow
    // gradient can overlay a real 1px border without a layout shift); the
    // attached folder-branch-picker treatment still adds a solid surface
    // (`bg-background`, which goes transparent to reveal a workspace-bg image via
    // `ws-transparent-bg` instead of frosting) + the inset focus ring on top.
    // The resting border is `border-foreground/20` (a touch darker than the
    // near-invisible default `border-input`, and legible over a background image).
    expect(messageInputSource).toContain(
      "rounded-xl border border-foreground/20 bg-transparent transition-colors"
    )
    expect(messageInputSource).toContain(
      '"bg-background ws-transparent-bg focus-within:border-ring focus-within:ring-[3px] focus-within:ring-inset focus-within:ring-ring/50"'
    )
    expect(pickerWrapper).not.toContain("border-t border-input")
    expect(pickerWrapper).not.toContain("bg-muted/30")
    expect(pickerWrapper).toContain("pt-1")
    expect(pickerWrapper).not.toContain("py-1")
    expect(pickerWrapper).toContain("rounded-b-xl")
    // The row only renders while attached below the composer, so the detached
    // `mt-1.5` else-branch is gone; it always takes the rounded-bottom box.
    expect(pickerWrapper).not.toContain("mt-1.5")
    // `px-2` keeps the left gutter aligned with the composer above while also
    // padding the trailing edge where the status indicators sit.
    expect(pickerWrapper).toContain("px-2")
    expect(pickerWrapper).not.toContain("pl-[")
    expect(pickerWrapper).not.toContain("pl-1.5")
    expect(pickerWrapper).not.toMatch(/\bborder-b\b/)
    expect(pickerWrapper).not.toMatch(/\bborder-x\b/)
    // The context-usage circle + agent connection status moved here from the
    // bottom status bar: they right-align at the trailing edge (justify-between)
    // while the folder/branch pickers stay on the left.
    expect(pickerWrapper).toContain("justify-between")
    expect(pickerWrapper).toContain("<ComposerContextUsage")
    expect(pickerWrapper).toContain("<ComposerConnectionStatus")
  })

  it("keeps ordinary chat input constrained to the message column width", () => {
    expect(conversationShellSource).toContain(
      'className="mx-auto w-full max-w-3xl"'
    )
    // Ordinary (active/historical) chat input keeps its own px-4 gutter to align
    // with the sibling cards in conversation-shell AND a tight bottom gap (pb-1)
    // matching the attached folder/branch row's `pt-1` top gap; only the welcome
    // input drops the gutter via `flush` (the welcome column already provides
    // px-4) and uses the same pb-1.
    expect(chatInputSource).toContain(
      'cn("pt-0", flush ? "pb-1" : "px-4 pb-1")'
    )
    expect(chatInputSource).toContain(
      'cn(tall ? "min-h-30" : "min-h-24", "max-h-60")'
    )
    expect(chatInputSource).not.toContain("containerClassName")
    expect(source).not.toContain("containerClassName")
    expect(conversationShellSource).not.toContain("containerClassName")
    expect(source).toContain("mx-auto flex w-full max-w-3xl")
  })
})

describe("ConversationDetailPanel split-group render model", () => {
  // The split feature's one structural invariant: group shells are FLAT
  // SIBLINGS keyed by their stable group id, positioned purely by computed
  // percentage rects. Nesting shells per layout-tree depth would reparent (and
  // remount) every live conversation view on split/unsplit/orientation
  // changes.
  it("renders group shells as flat keyed siblings from computed rects", () => {
    expect(panelSource).toContain(
      "{orderedGroupIds.map((groupId) => renderGroupShell(groupId))}"
    )
    expect(panelSource).toContain("const renderGroupShell = (groupId: string)")
    // A component defined inside render would change type identity every
    // render and remount its subtree — keep these plain function calls.
    expect(panelSource).not.toContain("<RenderGroupShell")
    expect(panelSource).not.toContain("<RenderTabWrapper")
    expect(panelSource).toContain("key={groupId}")
    expect(panelSource).toContain("computeRects(groupLayout)")
  })

  it("marks the active session whenever several are visible (split or tiled)", () => {
    expect(panelSource).toContain(
      "showActiveFlow={(isSplit || canTileGroup) && active}"
    )
  })

  it("gives each split group its own strip and divider overlays only while split", () => {
    expect(panelSource).toContain("<TabBar groupId={groupId} />")
    const handlesIdx = panelSource.indexOf("groupHandles.map((handle) => (")
    expect(handlesIdx).toBeGreaterThan(-1)
    expect(panelSource.slice(handlesIdx - 80, handlesIdx)).toContain(
      "{isSplit &&"
    )
  })

  // Each split group keeps the unsplit layout's "tabs + conversation title
  // bar" pairing: its own header (driven by the GROUP's selected tab) sits
  // under its strip, and the global single header steps aside while split.
  it("pairs every split group with its own title bar and gates the global one", () => {
    const shellStart = panelSource.indexOf("const renderGroupShell = (groupId")
    const shellBody = panelSource.slice(shellStart, shellStart + 6000)
    expect(shellBody).toContain("{isSplit && selectedTab && (")
    expect(shellBody).toContain("<ConversationDetailHeader")
    expect(shellBody).toContain("tabId={selectedTab.id}")
    expect(panelSource).toContain("{!isSplit && activeTab && (")
  })

  // While split the workspace layout drops its title-bar strip row ENTIRELY —
  // no blank drag row above the shells. The window-drag surface moves into the
  // group strips instead: every strip's tail spacer is a drag region, and the
  // TOP-edge strips re-create the corner reserves (traffic lights / caption
  // buttons / chrome clusters) the unsplit row normally provides.
  it("replaces the unsplit title-bar row with in-strip drag surfaces while split", () => {
    // Layout: the whole h-10 conversation top bar is gated on !isConvSplit;
    // the old always-rendered row with a split drag-region branch is gone.
    expect(workspaceLayoutSource).toContain("{!isConvSplit && (")
    expect(workspaceLayoutSource).not.toContain("hasConvTabs && !isConvSplit")

    // Panel: TOP-edge group strips carry the corner reserves themselves.
    const shellStart = panelSource.indexOf("const renderGroupShell = (groupId")
    const shellBody = panelSource.slice(shellStart, shellStart + 6000)
    expect(shellBody).toContain(
      '{touchesLeft && <SplitStripCornerReserve side="left" />}'
    )
    expect(shellBody).toContain(
      '{touchesRight && <SplitStripCornerReserve side="right" />}'
    )

    // Tab bar: the tail spacer is a window-drag region on EVERY strip (group
    // strips are the window's top edge while split), not just the unsplit one.
    expect(tabBarSource).toContain(
      '<div data-tauri-drag-region className="h-full min-w-10 flex-1" />'
    )
    expect(tabBarSource).not.toContain("data-tauri-drag-region={groupId")
  })
})

describe("ConversationDetailPanel chat-mode send path", () => {
  // Regression guard for the "first chat message gets stuck in the queue and is
  // never sent" bug: the chat first-send must NOT enqueue-and-return, it must
  // take the same inline create+bind+lifecycleSend path as a normal new
  // conversation. The old failure mode relied on the flush-on-connect engine,
  // which went dormant once the eager connection was already `connected`.
  it("does not special-case the chat first send into an enqueue-and-return branch", () => {
    // The old chat-draft early branch and its single-flight guard are gone.
    expect(source).not.toContain(
      "sendOwnTab?.isChat === true && dbConvIdRef.current == null"
    )
    expect(source).not.toContain("createChatPendingRef")
  })

  it("creates the chat row inline in the shared new-tab path and sends via lifecycleSend", () => {
    // Chat send is selected synchronously, then the SAME async block that
    // handles normal new conversations creates the row and delivers inline.
    expect(source).toContain("const chatSend = sendOwnTab?.isChat === true")
    expect(source).toContain("createChatConversation(")

    const sendStart = source.indexOf("const chatSend = sendOwnTab?.isChat")
    const sendEnd = source.indexOf(
      "createConversationPendingRef.current = false"
    )
    expect(sendStart).toBeGreaterThan(-1)
    expect(sendEnd).toBeGreaterThan(sendStart)
    const block = source.slice(sendStart, sendEnd)
    // Inline delivery (the fix) — not an mqEnqueue that defers to the queue.
    expect(block).toContain("lifecycleSend(draft, selectedModeIdArg, {")
    expect(block).not.toContain("mqEnqueue")
  })

  it("gates the chat-draft composer on a live connection (no offline compose)", () => {
    // allowOfflineCompose let the user send before connecting, which is what
    // parked the first prompt in the never-flushed queue. The composer now
    // waits for `connected` like a normal conversation.
    expect(source).not.toContain("allowOfflineCompose")
  })

  it("surfaces a non-silent error when the eager scratch-dir prepare fails", () => {
    // Without offline compose, a failed mkdir would silently disable the
    // composer forever; the eager effect must surface it instead.
    expect(source).toContain(
      'setAgentConnectError(tWelcome("prepareSessionFailed"))'
    )
  })
})

describe("ConversationDetailPanel send-path hardening", () => {
  // Guards for the production-readiness fixes from the Codex review of the
  // chat-mode work. The behavioral cores (readiness predicate, duplicate-create
  // rejection) are unit-tested in src/lib/queue-flush.test.ts; these assert they
  // are actually wired into the send path here.
  it("gates direct send on cwd-matched legacy or shared prompt admission", () => {
    // A chat draft mid-reconnect can read a stale "connected" for the previous
    // cwd; sending then would hit the wrong workspace. handleSend must gate on
    // the readiness predicate (connected AND cwd matches), like the flush effect.
    expect(source).toContain("isConnectionReady(")
    expect(source).toContain("const promptAdmissionReady =")
    expect(source).toContain("connectionReady ||")
    expect(source).toContain("if (!promptAdmissionReady) return")
  })

  it("disables the welcome composer while connected-but-not-ready", () => {
    // The composer reads a downgraded status so its send affordance is disabled
    // during the transient mismatch window instead of inviting a rejected send.
    expect(source).toContain("composerConnStatus")
    expect(source).toContain("status={composerConnStatus}")
  })

  it("single-flights the unbound create before any optimistic mutation", () => {
    // A double-submit during the create window must be rejected BEFORE the
    // optimistic turn is appended, or it orphans a turn it can never deliver.
    expect(source).toContain("shouldRejectDuplicateCreate(")
    const guardIdx = source.indexOf("shouldRejectDuplicateCreate(")
    // The CALL site (assignment), not the function definition earlier in the file.
    const optimisticIdx = source.indexOf(
      "const builtOptimistic = buildOptimisticUserTurnFromDraft("
    )
    expect(guardIdx).toBeGreaterThan(-1)
    expect(optimisticIdx).toBeGreaterThan(guardIdx)
  })

  it("fully restores pre-send state when the create fails", () => {
    // A failed create must not strand the user behind a blank panel: drop the
    // optimistic turn, return to welcome mode, re-seed the draft, surface error.
    const catchIdx = source.indexOf(
      "[ConversationSessionSurface] create conversation:"
    )
    expect(catchIdx).toBeGreaterThan(-1)
    const catchBlock = source.slice(catchIdx, catchIdx + 1500)
    expect(catchBlock).toContain("removeOptimisticTurn(")
    expect(catchBlock).toContain("setHasSentMessage(false)")
    expect(catchBlock).toContain("saveMessageInputDraft(")
    expect(catchBlock).toContain(
      'setAgentConnectError(tWelcome("createConversationFailed"))'
    )
  })
})

describe("ConversationDetailPanel continuation waiting / draft-safe wiring", () => {
  it("guards queue flush both before scheduling and inside the timer callback", () => {
    // Pre-schedule guard + dependency.
    expect(source).toMatch(/if \(conn\.waitingForSubagents\) return/)
    expect(source).toContain("conn.waitingForSubagents")
    // Timer callback must re-check (Connected-before-waiting race).
    const flushStart = source.indexOf(
      "Flush queued messages whenever the agent is idle"
    )
    const flushEnd = source.indexOf(
      "Mirror the connection's liveMessage into the runtime session",
      flushStart
    )
    expect(flushStart).toBeGreaterThan(-1)
    expect(flushEnd).toBeGreaterThan(flushStart)
    const flushBlock = source.slice(flushStart, flushEnd)
    expect(flushBlock).toContain("setTimeout")
    // Dependency array includes waiting projection.
    expect(flushBlock).toMatch(/conn\.waitingForSubagents/)

    // Pin the *in-timer* runtime guard specifically. Pre-schedule alone must not
    // satisfy this: removing only `if (waitingForSubagentsRef.current) return`
    // inside setTimeout must fail the test.
    const beforeTimer = flushBlock.slice(0, flushBlock.indexOf("setTimeout"))
    expect(beforeTimer).toMatch(/if \(conn\.waitingForSubagents\) return/)
    const timerMatch = flushBlock.match(
      /setTimeout\(\s*\(\)\s*=>\s*\{([\s\S]*?)\n\s*\}, wait\)/
    )
    expect(timerMatch).not.toBeNull()
    const timerBody = timerMatch![1]
    expect(timerBody).toMatch(
      /if\s*\(\s*waitingForSubagentsRef\.current\s*\)\s*return/
    )
    // Guard must run before dequeue/auto-send of the queue head.
    const refGuard = timerBody.search(
      /if\s*\(\s*waitingForSubagentsRef\.current\s*\)\s*return/
    )
    const autoSend = timerBody.indexOf("autoSendQueueRef")
    expect(refGuard).toBeGreaterThan(-1)
    expect(autoSend).toBeGreaterThan(refGuard)
  })

  it("handleSend returns early when snapshot already says waiting", () => {
    const sendStart = source.indexOf("const handleSend = useCallback(")
    const sendEnd = source.indexOf(
      "// Sync handleSend ref for auto-send effect",
      sendStart
    )
    expect(sendStart).toBeGreaterThan(-1)
    const sendBlock = source.slice(sendStart, sendEnd)
    // Guard before optimistic mutation.
    const waitingGuard = sendBlock.indexOf("waitingForSubagents")
    const optimistic = sendBlock.indexOf("buildOptimisticUserTurnFromDraft")
    expect(waitingGuard).toBeGreaterThan(-1)
    expect(optimistic).toBeGreaterThan(waitingGuard)
  })

  it("pins direct-restore vs queue-head-requeue on continuation waiting rejection", () => {
    expect(source).toContain("onContinuationWaiting")
    const start = source.indexOf("onContinuationWaiting")
    const block = source.slice(start, start + 1200)
    // Direct path restores PromptDraft; queue-flush requeues front.
    expect(block).toContain("fromQueueFlush")
    expect(block).toContain("mqRequeueFront")
    expect(block).toMatch(
      /PromptDraftRestore|draftRestore|setDraftRestore|setPromptDraftRestore/
    )
    // Direct restore must not enqueue.
    expect(block).not.toMatch(/onContinuationWaiting[\s\S]{0,400}mqEnqueue\(/)
  })

  it("cold continuation_failure effect keys dedup by code and finished_at", () => {
    expect(source).toContain("continuation_failure")
    expect(source).toContain("continuationFailureI18nKey")
    // Identity is (code, finished_at) — both must appear in the dedup key.
    expect(source).toMatch(
      /continuation_failure[\s\S]{0,800}(code|finished_at)[\s\S]{0,200}(finished_at|code)/
    )
    // Toast must use the shared failure-code mapping (not raw DB text / free t()).
    expect(source).toMatch(
      /toast\.error\(\s*tAcpConnections\(\s*continuationFailureI18nKey\(\s*failure\.code\s*\)\s*\)\s*\)/
    )
  })

  it("threads waitingForSubagents through ConversationShell and welcome ChatInput", () => {
    expect(conversationShellSource).toContain("waitingForSubagents")
    expect(chatInputSource).toContain("waitingForSubagents")
    expect(chatInputSource).toContain("showCancel")
    expect(messageInputSource).toContain("showCancel")
    expect(messageInputSource).toContain("draftRestore")
    expect(messageInputSource).toContain("PromptDraftRestore")
    // Welcome + shell both receive waiting.
    expect(source).toMatch(/waitingForSubagents=\{/)
  })
})

describe("ConversationTabView initial history eligibility", () => {
  it("distinguishes component early returns from lazy callback returns", () => {
    expect(
      hasComponentReturnBeforeCall(
        "function Sample() { if (!ready) return null; targetHook() }",
        "Sample",
        "targetHook"
      )
    ).toBe(true)
    expect(
      hasComponentReturnBeforeCall(
        "function Sample() { useState(() => { return null }); targetHook() }",
        "Sample",
        "targetHook"
      )
    ).toBe(false)
  })

  it("captures persisted eligibility at mount and passes successful load state", () => {
    expect(source).toMatch(
      /useInitialHistoryScrollEligibility\(\s*conversationId\s*\)/
    )
    expect(source).toContain(
      "initialHistoryScrollEligible={initialHistoryScrollEligible}"
    )
    expect(source).toContain("historyLoadComplete={detail != null}")
  })

  // Identity audit: draft first-send bind must not remount the session surface
  // or the lazy eligibility latch would re-sample a non-null conversationId.
  it("keeps session surface identity on draft bind (tab.id key, not conversationId)", () => {
    // Parent maps keep-alive wrappers by stable tab id, not conversation id.
    expect(panelSource).toContain("key={tab.id}")
    expect(panelSource).not.toMatch(/key=\{tab\.conversationId\}/)
    // bindConversationTab updates conversationId on the same tab row.
    expect(source).toContain("bindConversationTab(")
    // Hook call is unconditional near the start of ConversationSessionSurface
    // (before any early return) and freezes via useState — prop changes do not remount.
    const tabViewStart = source.indexOf("function ConversationSessionSurface({")
    const hookIdx = source.indexOf(
      "useInitialHistoryScrollEligibility(conversationId)",
      tabViewStart
    )
    expect(tabViewStart).toBeGreaterThan(-1)
    expect(hookIdx).toBeGreaterThan(tabViewStart)
    // No component-level early return before the hook. Nested lazy callbacks
    // may return without making the hook conditional.
    expect(
      hasComponentReturnBeforeCall(
        source,
        "ConversationSessionSurface",
        "useInitialHistoryScrollEligibility"
      )
    ).toBe(false)
  })

  it("does not remount the tab view on manual reload (reloadSignal only refetches)", () => {
    // Manual reload bumps reloadSignal / calls reloadDetail (cancel-fence
    // override); it does not change the React key or recreate ConversationSessionSurface.
    expect(source).toContain(
      'reloadDetail(effectiveConversationId, { reason: "manual_reload" })'
    )
    expect(panelSource).toContain("reloadSignal={reloadByTabId[tab.id] ?? 0}")
    // historyLoadComplete tracks detail presence, so a failed load stays false
    // until a successful fetch retains detail on the session.
    expect(source).toContain("historyLoadComplete={detail != null}")
  })
})

describe("ConversationDetailPanel session-load failure surface", () => {
  // When session/load fails on a conversation whose transcript already
  // rendered (e.g. its folder was deleted), the history must STAY readable;
  // the failure surfaces as a banner docked at the composer, not as a
  // full-page error over the message area.
  it("escalates the ACP load error to full-page only when nothing is renderable", () => {
    expect(messageListViewSource).toContain(
      "const blockingLoadError = hasRenderableContent ? null : (acpLoadError ?? null)"
    )
  })

  it("docks the load error at the composer with the recovery actions", () => {
    // The composer input stays hidden (a send can't reach the dead session)…
    expect(source).toContain(
      "hideInput={isWelcomeMode || Boolean(acpLoadError)}"
    )
    // …and the banner takes its place, explaining why and offering recovery.
    expect(source).toContain("composerBanner={acpLoadErrorBanner}")
    const bannerStart = source.indexOf("const acpLoadErrorBanner")
    expect(bannerStart).toBeGreaterThan(-1)
    const bannerEnd = source.indexOf("const goalControlValue", bannerStart)
    expect(bannerEnd).toBeGreaterThan(bannerStart)
    const banner = source.slice(bannerStart, bannerEnd)
    expect(banner).toContain("hasPersistedConversation && acpLoadError")
    expect(banner).toContain("handleReloadDetail")
    expect(banner).toContain("handleOpenNewSession")
    // The shell renders the banner inside the composer dock, constrained to
    // the same message-column width as the input it replaces.
    const dockIdx = conversationShellSource.indexOf("{composerBanner && (")
    expect(dockIdx).toBeGreaterThan(-1)
    const dock = conversationShellSource.slice(dockIdx, dockIdx + 200)
    expect(dock).toContain("mx-auto w-full max-w-3xl")
  })
})
