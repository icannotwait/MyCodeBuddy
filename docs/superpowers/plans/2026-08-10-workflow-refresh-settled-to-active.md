# Workflow Refresh Settled-to-Active Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the active conversation's workflow graph converge automatically after a missed settled-to-active event, even while its workflow chip is collapsed.

**Architecture:** Propagate the existing conversation activity bit to the workflow overlay so only the active surface owns an overlay-interest lease. Keep that lease independent of chip expansion, then let the graph store poll a discovered numbered graph every 15 seconds while overlay interest exists; events remain the immediate path and inactive tabs release all timers.

**Tech Stack:** Next.js 16, React 19, strict TypeScript, Zustand, Vitest, Testing Library, pnpm, PowerShell 7.

## Global Constraints

- Approved design: `docs/superpowers/specs/2026-08-10-workflow-refresh-settled-to-active-design.md`.
- Frontend-only change. Do not modify Rust, backend APIs, schemas, event names, event payloads, or persistence behavior.
- Production files are limited to `src/components/chat/sub-agent-overlay.tsx`, `src/components/message/message-list-view.tsx`, and `src/lib/workflow-graph-store.ts`.
- Test files are limited to `src/components/chat/workflow-overlay.test.tsx`, `src/components/message/message-list-view.test.tsx`, and `src/lib/workflow-graph-store.test.ts`.
- Preserve immediate event-driven refresh, request-generation gates, `graph_revision` ordering, activation epochs, and one timer per conversation.
- Use 15 seconds only for active workflow state or a discovered numbered graph with overlay interest.
- Preserve the 10-minute fallback for an undiscovered graph and expanded-only interest.
- Inactive or unmounted conversations must own no workflow interest or timer.
- Add no dependencies and do not regenerate lockfiles.
- Follow TDD: write each regression, observe its expected failure, make the minimum production change, then rerun it.

## File Structure

| File | Responsibility |
| --- | --- |
| `src/components/chat/sub-agent-overlay.tsx` | Translate active-surface, chip, segment, and full-graph UI state into overlay and expanded store leases. |
| `src/components/chat/workflow-overlay.test.tsx` | Prove collapsed active chips retain overlay interest and inactive surfaces release all interest. |
| `src/components/message/message-list-view.tsx` | Carry the existing `isActive` prop through both incremental and legacy overlay render paths. |
| `src/components/message/message-list-view.test.tsx` | Prove both render paths forward the real activity bit. |
| `src/lib/workflow-graph-store.ts` | Choose the next 15-second, 10-minute, or absent refresh timer from graph and lease state. |
| `src/lib/workflow-graph-store.test.ts` | Prove settled-to-active convergence, continued active-surface reconciliation, and timer disposal. |

---

### Task 1: Keep Workflow Interest for the Active Surface

**Files:**

- Modify: `src/components/chat/sub-agent-overlay.tsx:117-146,359-423`
- Test: `src/components/chat/workflow-overlay.test.tsx:1540-1630`
- Modify: `src/components/message/message-list-view.tsx:1037-1108,1111-1131,1913-1936`
- Test: `src/components/message/message-list-view.test.tsx:234-248,523-532,699-724,1494-1588`

**Interfaces:**

- Consumes: `MessageListViewProps.isActive?: boolean`, defaulting to `true`.
- Produces: `SubAgentOverlayProps.isActive?: boolean`, defaulting to `true`.
- Lease rule: overlay interest is `valid conversationId && isActive`; expanded interest additionally requires an expanded chip, workflow segment, and expanded full graph.

- [ ] **Step 1: Replace the collapsed-chip regression and add inactive release coverage**

Replace `chip-collapsed overlays acquire no interest` in `workflow-overlay.test.tsx` with:

```tsx
it("chip-collapsed active overlays retain overlay interest", () => {
  const releaseOverlay = vi.fn()
  const activateOverlay = vi
    .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
    .mockReturnValue(releaseOverlay)
  const activateExpanded = vi.spyOn(
    useWorkflowGraphStore.getState(),
    "activateConversation"
  )

  const { unmount } = renderWithIntl(
    <SubAgentOverlay
      delegations={[]}
      activities={[]}
      conversationId={42}
      workflowGraph={skeletonGraph()}
      defaultExpanded={false}
    />
  )

  expect(activateOverlay).toHaveBeenCalledOnce()
  expect(activateOverlay).toHaveBeenCalledWith(42)
  expect(activateExpanded).not.toHaveBeenCalled()

  unmount()
  expect(releaseOverlay).toHaveBeenCalledOnce()
})
```

In `switching segments and collapsing the overlay releases the active lease`, rename the test to `switching segments and collapsing releases only expanded interest` and replace its final overlay assertion with:

```ts
expect(releaseExpanded1).toHaveBeenCalledTimes(1)
expect(releaseExpanded2).toHaveBeenCalledTimes(1)
expect(releaseOverlay).not.toHaveBeenCalled()
```

Add this test immediately afterward:

```tsx
it("becoming inactive releases overlay and expanded interest", () => {
  const releaseOverlay = vi.fn()
  const releaseExpanded = vi.fn()
  const activateOverlay = vi
    .spyOn(useWorkflowGraphStore.getState(), "activateOverlayInterest")
    .mockReturnValue(releaseOverlay)
  const activateExpanded = vi
    .spyOn(useWorkflowGraphStore.getState(), "activateConversation")
    .mockReturnValue(releaseExpanded)

  const { rerender } = renderWithIntl(
    <SubAgentOverlay
      delegations={[]}
      activities={[]}
      conversationId={42}
      workflowGraph={skeletonGraph()}
      defaultExpanded
      isActive
    />
  )
  fireEvent.click(screen.getByTestId("workflow-expand-toggle"))
  expect(activateOverlay).toHaveBeenCalledWith(42)
  expect(activateExpanded).toHaveBeenCalledWith(42)

  rerender(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <SubAgentOverlay
        delegations={[]}
        activities={[]}
        conversationId={42}
        workflowGraph={skeletonGraph()}
        defaultExpanded
        isActive={false}
      />
    </NextIntlClientProvider>
  )

  expect(releaseOverlay).toHaveBeenCalledOnce()
  expect(releaseExpanded).toHaveBeenCalledOnce()
})
```

- [ ] **Step 2: Add activity forwarding regressions for both message-list paths**

Extend both captured overlay prop types in `message-list-view.test.tsx` with:

```ts
isActive?: boolean
```

Change the two helper option types and the rendered prop to:

```tsx
function messageListUi(options?: {
  waitingForSubagentsArmedAtMs?: number | null
  connStatus?: "connected" | "prompting" | "connecting" | "disconnected"
  isActive?: boolean
}) {
  return (
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageListView
        conversationId={CID}
        agentType="codex"
        connStatus={options?.connStatus ?? "prompting"}
        isActive={options?.isActive ?? true}
        showMessageNav={false}
        waitingForSubagentsArmedAtMs={
          options?.waitingForSubagentsArmedAtMs ?? null
        }
      />
    </NextIntlClientProvider>
  )
}

function renderMessageList(options?: {
  waitingForSubagentsArmedAtMs?: number | null
  connStatus?: "connected" | "prompting" | "connecting" | "disconnected"
  isActive?: boolean
}) {
  return render(messageListUi(options))
}
```

Add these tests to `MessageListView sub-agent overlay composition`:

```tsx
it("forwards inactive state through the incremental overlay path", () => {
  renderMessageList({ isActive: false })
  expect(lastOverlayProps().isActive).toBe(false)
})

it("forwards inactive state through the legacy overlay path", () => {
  __resetStreamingPerformanceConfigForTests()
  renderMessageList({ isActive: false })
  expect(lastOverlayProps().isActive).toBe(false)
})
```

- [ ] **Step 3: Run the focused tests and verify RED**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/message/message-list-view.test.tsx
```

Expected: failures show that a collapsed chip does not acquire overlay interest, `isActive={false}` does not release it, and `MessageListView` does not forward `isActive`.

- [ ] **Step 4: Implement active-surface lease ownership in `SubAgentOverlay`**

Replace the `conversationId` documentation and add the activity prop with:

```ts
/**
 * Parent conversation id. It seeds the graph store and scopes both interest
 * leases. The active surface keeps overlay interest while collapsed; only the
 * open full graph adds expanded interest.
 */
conversationId?: number | null
/** Whether this conversation surface is the active tab in its window. */
isActive?: boolean
```

Destructure it with the existing defaults:

```ts
conversationId = null,
workflowGraph = null,
isActive = true,
```

Replace the interest predicates with:

```ts
const overlayInterestActive =
  conversationId != null && conversationId > 0 && isActive
const expandedGraphInterestActive =
  overlayInterestActive &&
  isExpanded &&
  activeSegment === "workflow" &&
  graphExpanded
```

- [ ] **Step 5: Forward `isActive` through both `MessageListView` paths**

Add `isActive: boolean` to `LiveAwareSubAgentOverlay`'s parameter type and destructured arguments, then pass it to its `SubAgentOverlay`:

```tsx
<SubAgentOverlay
  key={historicalKey}
  delegations={delegations}
  activities={activities}
  overlayKey={historicalKey}
  defaultExpanded
  conversationId={conversationId}
  workflowGraph={workflowGraph}
  isActive={isActive}
  onResumeRoot={onResumeRoot}
  onOpenRootConversation={onOpenRootConversation}
/>
```

Rename `isActive: _isActive = true` in `MessageListView` to:

```ts
isActive = true,
```

Pass `isActive={isActive}` to `LiveAwareSubAgentOverlay` and to the directly rendered `SubAgentOverlay`.

- [ ] **Step 6: Run focused tests and verify GREEN**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/message/message-list-view.test.tsx
```

Expected: exit code `0`; both suites pass, including collapsed, inactive, incremental, and legacy cases.

- [ ] **Step 7: Commit the UI lifecycle change**

```powershell
git add src/components/chat/sub-agent-overlay.tsx src/components/chat/workflow-overlay.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx
git commit -m "fix: retain active workflow refresh interest"
```

Expected: one commit containing only the four listed component and test files.

---

### Task 2: Poll Settled Active-Surface Graphs from Authority

**Files:**

- Modify: `src/lib/workflow-graph-store.ts:1-12,243-258,585-628`
- Test: `src/lib/workflow-graph-store.test.ts:1108-1375`

**Interfaces:**

- Consumes: `ActiveConversationRecord.overlayCount`, activation epoch, and `ConversationGraphEntry.appliedGraphRevision`.
- Produces: private `hasOverlayInterestEpoch(conversationId, epoch): boolean` and updated `nextAuthorityRefreshDelay(...)` behavior.
- Preserves: public store API, one timer per conversation, event listener ownership, request generations, and revision ordering.

- [ ] **Step 1: Rewrite timer expectations to the approved behavior**

Replace `active numbered overlay converges after 15 seconds and stops when settled` with:

```ts
it("active overlay keeps polling after authority settles until release", async () => {
  const active = activeSnapshot({ graph_revision: 2 })
  const settled = settledSnapshot({ graph_revision: 3 })
  useWorkflowGraphStore.getState().applyFromDetail(201, active)
  getWorkflowGraphSnapshot
    .mockResolvedValueOnce(active)
    .mockResolvedValue(settled)

  const release = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(201)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(201)?.overall_state
  ).toBe("completed")

  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

  release()
  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
})
```

Replace `settled numbered overlay handles newer events without a timer` with the missed-event regression:

```ts
it("settled numbered overlay discovers a newer active revision without an event", async () => {
  useWorkflowGraphStore
    .getState()
    .applyFromDetail(92, settledSnapshot({ graph_revision: 2 }))
  getWorkflowGraphSnapshot
    .mockResolvedValueOnce(settledSnapshot({ graph_revision: 2 }))
    .mockResolvedValue(activeSnapshot({ graph_revision: 3 }))

  const release = useWorkflowGraphStore.getState().activateOverlayInterest(92)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(14_999)
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  await flushMicrotasks()

  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(92)?.graph_revision
  ).toBe(3)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(92)?.overall_state
  ).toBe("in_progress")
  release()
})
```

Replace `overlay-only discovery re-arms the 10-minute fallback until a graph appears` with:

```ts
it("overlay discovery uses ten minutes until numbered, then fifteen seconds", async () => {
  const discovered = settledSnapshot({ graph_revision: 1 })
  getWorkflowGraphSnapshot
    .mockResolvedValueOnce(null)
    .mockResolvedValueOnce(null)
    .mockResolvedValue(discovered)

  const release = useWorkflowGraphStore.getState().activateOverlayInterest(94)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  expect(useWorkflowGraphStore.getState().getSnapshot(94)).toBeNull()

  await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(useWorkflowGraphStore.getState().getSnapshot(94)).toBeNull()

  await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(94)?.graph_revision
  ).toBe(1)

  await vi.advanceTimersByTimeAsync(14_999)
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
  await vi.advanceTimersByTimeAsync(1)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(4)

  release()
  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(4)
})
```

Replace `seeds the first publish event after an overlay mount resolves null` with:

```ts
it("first publish event switches overlay to fifteen-second authority", async () => {
  const changedDispose = vi.fn()
  const nudgeDispose = vi.fn()
  const discovered = settledSnapshot({ graph_revision: 1 })
  subscribeWorkflowGraphChanged.mockResolvedValue(changedDispose)
  subscribeWorkflowCompatibilityNudge.mockResolvedValue(nudgeDispose)
  getWorkflowGraphSnapshot
    .mockResolvedValueOnce(null)
    .mockResolvedValue(discovered)

  const release = useWorkflowGraphStore.getState().activateOverlayInterest(98)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  expect(useWorkflowGraphStore.getState().getSnapshot(98)).toBeNull()

  useWorkflowGraphStore.getState().handleGraphChanged({
    parent_conversation_id: 98,
    workflow_id: "wf-1",
    graph_revision: 1,
  })
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(98)?.graph_revision
  ).toBe(1)

  await vi.advanceTimersByTimeAsync(14_999)
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(1)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

  release()
  expect(changedDispose).toHaveBeenCalledOnce()
  expect(nudgeDispose).toHaveBeenCalledOnce()
  useWorkflowGraphStore.getState().handleGraphChanged({
    parent_conversation_id: 98,
    workflow_id: "wf-1",
    graph_revision: 2,
  })
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
})
```

Replace `releasing expanded interest keeps overlay events but stops fallback` with:

```ts
it("releasing expanded interest keeps overlay authority polling", async () => {
  useWorkflowGraphStore
    .getState()
    .applyFromDetail(93, settledSnapshot({ graph_revision: 5 }))
  getWorkflowGraphSnapshot.mockResolvedValue(
    settledSnapshot({ graph_revision: 6 })
  )

  const releaseOverlay = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(93)
  await flushMicrotasks()
  const releaseExpanded = useWorkflowGraphStore
    .getState()
    .activateConversation(93)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)

  releaseExpanded()
  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

  releaseOverlay()
  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
})
```

Replace `late expanded completion updates cache but cannot arm an overlay timer` with:

```ts
it("late expanded completion keeps the overlay authority timer", async () => {
  useWorkflowGraphStore
    .getState()
    .applyFromDetail(97, settledSnapshot({ graph_revision: 1 }))
  const pending = deferred<WorkflowGraphSnapshot | null>()
  getWorkflowGraphSnapshot
    .mockReturnValueOnce(pending.promise)
    .mockReturnValueOnce(pending.promise)
    .mockResolvedValue(settledSnapshot({ graph_revision: 2 }))

  const releaseOverlay = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(97)
  await flushMicrotasks()
  const releaseExpanded = useWorkflowGraphStore
    .getState()
    .activateConversation(97)
  await flushMicrotasks()
  releaseExpanded()

  pending.resolve(settledSnapshot({ graph_revision: 2 }))
  await flushMicrotasks()
  expect(
    useWorkflowGraphStore.getState().getSnapshot(97)?.graph_revision
  ).toBe(2)

  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)

  releaseOverlay()
  await vi.advanceTimersByTimeAsync(15_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(3)
})
```

- [ ] **Step 2: Run the graph-store suite and verify RED**

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected: the settled numbered overlay has no 15-second timer, so the new missed-event and continued-polling assertions fail.

- [ ] **Step 3: Add an epoch-safe overlay-interest predicate**

Add this next to `hasExpandedInterestEpoch`:

```ts
function hasOverlayInterestEpoch(
  conversationId: number,
  epoch: number
): boolean {
  const active = activeConversations.get(conversationId)
  return active != null && active.epoch === epoch && active.overlayCount > 0
}
```

- [ ] **Step 4: Select 15 seconds for discovered overlay interest**

Replace `nextAuthorityRefreshDelay` with:

```ts
function nextAuthorityRefreshDelay(
  conversationId: number,
  epoch: number,
  get: () => WorkflowGraphState
): number | null {
  if (!isActiveEpoch(conversationId, epoch)) return null
  const entry = get().getEntry(conversationId)
  if (hasActiveWorkflowState(entry?.snapshot ?? null)) {
    return ACTIVE_AUTHORITY_REFRESH_MS
  }
  if (
    entry?.appliedGraphRevision != null &&
    hasOverlayInterestEpoch(conversationId, epoch)
  ) {
    return ACTIVE_AUTHORITY_REFRESH_MS
  }
  if (hasExpandedInterestEpoch(conversationId, epoch)) {
    return FALLBACK_REFRESH_MS
  }
  return entry?.appliedGraphRevision == null ? FALLBACK_REFRESH_MS : null
}
```

Replace the module scheduling comment with:

```ts
 * - Authority refresh: active workflow state and discovered active-surface
 *   overlay interest use 15 seconds. Expanded-only and undiscovered interest
 *   use a 10-minute fallback.
```

Replace the scheduling comment in `runRefresh` with:

```ts
// Active state and discovered active-surface overlay interest use 15 seconds.
// Expanded-only and undiscovered interest use the 10-minute fallback.
```

- [ ] **Step 5: Run the graph-store suite and verify GREEN**

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected: exit code `0`; settled-to-active, active-to-settled continuation, discovery, expanded-only, release, generation, and event tests all pass.

- [ ] **Step 6: Commit the store scheduling change**

```powershell
git add src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
git commit -m "fix: reconcile settled workflow graphs"
```

Expected: one commit containing only the store and its test file.

---

### Task 3: Verify the Complete Frontend Change

**Files:**

- Verify: the six files listed in Tasks 1 and 2
- Modify: none unless a command exposes a regression; any repair stays in the owning task's files and receives a focused regression first

**Interfaces:**

- Consumes: both green focused changes.
- Produces: fresh focused, full-suite, lint, build, and diff evidence.

- [ ] **Step 1: Run all three focused suites together**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/message/message-list-view.test.tsx src/lib/workflow-graph-store.test.ts
```

Expected: exit code `0`; all focused tests pass with no leaked timers or unhandled errors.

- [ ] **Step 2: Run the complete frontend test suite**

```powershell
pnpm test
```

Expected: exit code `0`; all Vitest suites pass.

- [ ] **Step 3: Run frontend lint**

```powershell
pnpm eslint .
```

Expected: exit code `0`; no ESLint or TypeScript lint errors.

- [ ] **Step 4: Run the static export build**

```powershell
pnpm build
```

Expected: exit code `0`; Next.js static export completes successfully.

- [ ] **Step 5: Audit scope and whitespace**

```powershell
$implementationBase = git rev-list -n 1 --grep="^docs: plan settled-to-active workflow refresh$" HEAD
if (-not $implementationBase) {
  throw "Implementation plan commit was not found."
}
git diff --check "$implementationBase..HEAD"
git diff --name-only "$implementationBase..HEAD" -- src
git status --short
```

Expected: no whitespace errors; source changes are limited to the six approved frontend files. Pre-existing untracked user artifacts may remain and must not be staged or removed.

- [ ] **Step 6: Record any verification-only repair**

When Step 1-4 reveals a UI lifecycle regression, add the smallest failing test, observe RED, apply the minimum repair, rerun Steps 1-4 from the beginning, and commit only the UI lifecycle files:

```powershell
git add src/components/chat/sub-agent-overlay.tsx src/components/chat/workflow-overlay.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx
git commit -m "fix: repair workflow interest lifecycle"
```

When the failure belongs to store scheduling, use only the store pair:

```powershell
git add src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
git commit -m "fix: repair workflow authority scheduling"
```

Expected: no repair commit is created when all commands are green. No Rust command is required because the changed-file audit contains no Rust or backend contract file.

## Execution Handoff

Plan execution can use either a fresh subagent per task with review between tasks, or inline execution in this session with checkpoints. Do not parallelize Tasks 1 and 2 because both behavior layers must be verified against the same lease semantics before broad validation.
