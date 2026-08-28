# Delegation Suspension Transcript Checkpoint Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve every delivered delegation continuation stage by checkpointing its live assistant message when a retained active connection moves from `prompting` to `connected` without `turn_complete`.

**Architecture:** Extend the existing prepared-frame pipeline with checkpoint-specific owner admission, an intermediate frame boundary, and exact-live-ID promotion immediately after canonical mirroring. Keep terminal completion unchanged, reuse the current runtime and transcript sinks, and pin the existing Rust `Connected` event as a tested contract. Desktop, Web live delivery, and trusted replay share this path; snapshot-only reconstruction remains outside scope because the snapshot has no message payload.

**Tech Stack:** React 19, TypeScript strict mode, Vitest, Zustand conversation runtime state, Next.js 16 static export, Rust 2021, Tokio, and the existing ACP event/session test harness.

**Spec:** `docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md`

## Global Constraints

- Add no dependency, database migration, wire event, transport field, parser behavior, or production Rust behavior.
- Keep `admitTurnComplete()` behavior unchanged. Add a separate `admitSuspensionCheckpoint()` and share only stable connection/session identity reads.
- Recognize a checkpoint only for an accepted typed `status_changed(connected)` whose pre-event snapshot is `prompting` with a non-null `liveMessage`.
- Do not inspect provider text, continuation markers, `turn_aborted`, timing, or `ContinuationWaitingChanged`.
- Keep `checkpointRuntimeConversationIds` separate from `completionRuntimeConversationIds`.
- Mirror only to admitted checkpoint owners, honor canonical `false` rejections, and promote only when `runtime.liveMessage?.id === finalLiveMessage.id` after mirroring.
- Never use the completion fallback based on `awaiting_persist` or in-flight optimistic turns for checkpoint ownership.
- Do not set accepted-completion markers or dispatch `TURN_COMPLETION_ACCEPTED` for a checkpoint.
- Do not trigger notifications, awaiting-reply state, session-failure settlement, user-stop coordination, broker/lifecycle settlement, or `PendingReview` from a checkpoint.
- Preserve existing leave-prompting cleanup and the existing out-of-turn streaming guard.
- Preserve the current virtual-step identity rule. Do not move message UUID allocation into `FrameAction`.
- Set checkpoint `liveMessageIsLive=true` for desktop, Web live, and trusted replay based on the pre-boundary prompting state.
- Do not claim recovery from a Web snapshot containing `status=connected, live_message=null`; that path still relies on cold transcript parsing and existing history coverage.
- Follow RED-GREEN for frontend behavior changes. The Rust edit is a characterization assertion for behavior that already exists and is expected to remain green.
- A filtered test command that executes zero tests is a failure, even when it exits successfully.
- Preserve unrelated dirty-worktree changes and stage only the exact files listed in each commit step.

---

## File Map

- `docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md`: approved behavior and scope contract.
- `docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md`: this executable task plan.
- `src/contexts/acp-connections-context.tsx`: checkpoint admission, prepared-step metadata, boundary mirroring, and exact-owner promotion.
- `src/contexts/acp-connections-context.test.tsx`: desktop, coalesced identity, canonical rejection, Web replay, alias ownership, and terminal-isolation regressions.
- `src-tauri/src/acp/connection.rs`: test-only assertion that successful suspension records exactly one `StatusChanged(Connected)` and no `TurnComplete`.

No new production file is needed. The frame, mirror, and completion machinery already lives together in `acp-connections-context.tsx`; a new module would add an interface without reducing ownership complexity.

---

### Task 1: Preserve The Design And Execution Plan

**Files:**
- Add: `docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md`
- Add: `docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md`

**Interfaces:**
- Consumes: the reviewed design and this implementation plan.
- Produces: committed planning artifacts that an isolated execution worktree can read at the exact paths in the plan header.

- [ ] **Step 1: Record the dirty-worktree baseline and locate both artifacts**

Run:

```powershell
git status --short
git status --short --ignored -- docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
```

Expected: record all pre-existing unrelated paths for the final scope comparison. The scoped command shows the design as untracked or modified and the plan as ignored (`!!`) until it is force-added.

- [ ] **Step 2: Stage and inspect the planning artifacts**

Run:

```powershell
git add docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md
git add -f docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
git diff --cached --check -- docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
git diff --cached --name-only -- docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
```

Expected: whitespace check exits 0 and the staged-name output contains exactly the two paths above.

- [ ] **Step 3: Commit the planning baseline**

Run:

```powershell
git commit --only -m "docs: plan delegation suspension transcript checkpoints" -- docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
```

Expected: one documentation commit containing the design and plan only.

---

### Task 2: Pin The Backend Suspension Boundary Contract

**Files:**
- Modify: `src-tauri/src/acp/connection.rs:17506-17554`
- Test: `src-tauri/src/acp/connection.rs:17506-17554`

**Interfaces:**
- Consumes: `finalize_turn_terminal(TurnTerminalSource::Upstream("cancelled"), ...)` and `SessionState::recent_events_after(0)`.
- Produces: a characterization assertion that successful suspension records exactly one `AcpEvent::StatusChanged { status: ConnectionStatus::Connected }` and zero `AcpEvent::TurnComplete` events.

- [ ] **Step 1: Strengthen the existing Rust test without changing production code**

Replace the current event assertion inside `delegation_suspend_cancelled_response_clears_turn_without_tree_cancel` with:

```rust
        let events = state.recent_events_after(0).expect("contiguous events");
        let connected_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Connected
                    }
                )
            })
            .count();
        assert_eq!(connected_count, 1, "suspension restores connected once");
        assert!(events
            .iter()
            .all(|event| !matches!(event.payload, AcpEvent::TurnComplete { .. })));
```

Do not change `finalize_turn_terminal()`, `emit_with_state()`, or suspension control flow.
Keep `EventEmitter::Noop`: `emit_with_state()` still records the event in
`SessionState`, so the existing test can observe the contract without a new
emitter fixture.

- [ ] **Step 2: Run the exact Rust characterization test**

Run from the repository root:

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils acp::connection::tests::delegation_suspend_cancelled_response_clears_turn_without_tree_cancel -- --exact
```

Expected: exactly one test executes and passes. A failure means the frontend design's event contract is not currently true and implementation stops for design review.

- [ ] **Step 3: Commit the contract test**

Run:

```powershell
git add src-tauri/src/acp/connection.rs
git diff --cached --check -- src-tauri/src/acp/connection.rs
git commit --only -m "test(acp): pin delegation suspension status boundary" -- src-tauri/src/acp/connection.rs
```

Expected: one test-only Rust commit.

### Task 3: Add The Core Desktop Transcript Checkpoint

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx:4953-5196`
- Modify: `src/contexts/acp-connections-context.tsx:6715-6910`
- Test: `src/contexts/acp-connections-context.test.tsx:6900-9400`

**Interfaces:**
- Consumes: `ConnectionState`, `resolveKnownConnectionSessionId()`, `useConversationRuntimeStore`, `PreparedEventFrame.connectionSteps`, `mirrorLiveMessageForCanonical()`, and `completeLiveTranscriptTurn(conversationId, liveMessage)`.
- Produces: test helper `status(connectionId, seq, value): EventEnvelope`; `admitSuspensionCheckpoint(snapshot): number[]`; `PreparedEventFrame.connectionSteps[].checkpointRuntimeConversationIds`; one step split after an admitted checkpoint; boundary-neutral canonical filtering/rejection; and exact-live-ID promotion before the non-completion return.

- [ ] **Step 1: Import the test types and reuse the existing event-fixture pattern**

Add `type LiveMessage` to the existing context import:

```typescript
import {
  AcpConnectionsProvider,
  useAcpActions,
  useConnectionStore,
  isRetryableObserverDiscoveryError,
  isValidConversationConnectionInfo,
  __getPublishedConnectionMapsCount,
  __resetPublishedConnectionMapsCount,
  __resetStreamingConfigForProviderTests,
  __connectionsReducerForTests,
  __resetWritableConnectionsCloneCount,
  __getWritableConnectionsCloneCount,
  type LiveMessage,
} from "@/contexts/acp-connections-context"
```

Add `ConnectionStatus` to the first existing `@/lib/types` type import:

```typescript
import type {
  ConnectionStatus,
  DbConversationDetail,
  DbConversationSummary,
  DesktopAcpEventBatch,
  DesktopDeliveryFailure,
} from "@/lib/types"
```

Then place this helper beside `content()` and `thinking()`:

```typescript
function status(
  connectionId: string,
  seq: number,
  value: ConnectionStatus
): EventEnvelope {
  return {
    connection_id: connectionId,
    seq,
    type: "status_changed",
    status: value,
  }
}
```

- [ ] **Step 2: Add failing desktop, coalesced-frame, and rejection regressions**

Inside `describe("AcpConnectionsProvider frame transactions (raw order)", ...)`, add this nested block. It uses `batch()`, `content()`, `status()`, and the existing `mountDesktopOwner()` fixture.

```typescript
  describe("delegation suspension transcript checkpoints", () => {
    it("checkpoints a suspended stage before the next desktop continuation", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
        const published: Array<{
          message: LiveMessage
          isLive: boolean
        }> = []
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical: (message, isLive) => {
            published.push({ message, isLive })
            runtimeActions.setLiveMessage(42, message, isLive)
            return (
              useConversationRuntimeStore.getState().byConversationId.get(42)
                ?.liveMessage === message
            )
          },
        })
        h.sendSystemNotification.mockClear()

        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              {
                connection_id: "owner-conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("owner-conn", 2, "prompting"),
              content("owner-conn", 3, "stage A"),
              status("owner-conn", 4, "connected"),
            ])
          )
          h.runAnimationFrame()
        })

        let runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        const stageATurn = runtime?.localTurns[0]
        expect(runtime?.localTurns).toEqual([
          {
            id: expect.stringMatching(/^live-42-/),
            role: "assistant",
            blocks: [{ type: "text", text: "stage A" }],
            timestamp: expect.any(String),
          },
        ])
        expect(runtime?.optimisticTurns).toEqual([])
        expect(runtime?.liveMessage).toBeNull()
        expect(runtime?.syncState).toBe("idle")
        expect(h.store!.getConnection(TAB)).toMatchObject({
          status: "connected",
          acceptedCompletionMessageId: null,
          acceptedCompletionRuntimeConversationIds: null,
        })
        expect(h.sendSystemNotification).not.toHaveBeenCalled()

        published.length = 0
        act(() => {
          h.emitDesktopBatch(
            batch(2, [
              content("owner-conn", 5, "late out-of-turn text"),
              status("owner-conn", 6, "prompting"),
              content("owner-conn", 7, "stage B"),
            ])
          )
          h.runAnimationFrame()
        })

        runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.localTurns).toHaveLength(1)
        expect(runtime?.localTurns[0]).toBe(stageATurn)
        expect(runtime?.liveMessage?.content).toEqual([
          { type: "text", text: "stage B" },
        ])
        expect(
          published.map(({ message }) =>
            message.content
              .map((block) => (block.type === "text" ? block.text : ""))
              .join("")
          )
        ).toEqual(["stage B"])
      } finally {
        resetConversationRuntimeStore()
      }
    })

    it("preserves checkpoint identity in a coalesced continuation frame", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
        const published: Array<{
          message: LiveMessage
          isLive: boolean
        }> = []
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical: (message, isLive) => {
            published.push({ message, isLive })
            runtimeActions.setLiveMessage(42, message, isLive)
            return (
              useConversationRuntimeStore.getState().byConversationId.get(42)
                ?.liveMessage === message
            )
          },
        })

        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              {
                connection_id: "owner-conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("owner-conn", 2, "prompting"),
              content("owner-conn", 3, "stage A"),
              status("owner-conn", 4, "connected"),
              status("owner-conn", 5, "prompting"),
              content("owner-conn", 6, "stage B"),
            ])
          )
          h.runAnimationFrame()
        })

        expect(published).toHaveLength(2)
        const checkpointMessage = published[0]!.message
        const currentMessage = published[1]!.message
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.localTurns).toEqual([
          {
            id: `live-42-${checkpointMessage.id}`,
            role: "assistant",
            blocks: [{ type: "text", text: "stage A" }],
            timestamp: expect.any(String),
          },
        ])
        expect(runtime?.liveMessage).toBe(currentMessage)
        expect(h.store!.getConnection(TAB)?.liveMessage).toBe(currentMessage)
        expect(checkpointMessage.id).not.toBe(currentMessage.id)
        expect(published.map(({ isLive }) => isLive)).toEqual([true, true])
      } finally {
        resetConversationRuntimeStore()
      }
    })

    it("honors canonical rejection at the checkpoint boundary", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")
      let rejectBoundary = false

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
        const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
          if (rejectBoundary) return false
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        })
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical,
        })

        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              {
                connection_id: "owner-conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("owner-conn", 2, "prompting"),
              content("owner-conn", 3, "stage A"),
            ])
          )
          h.runAnimationFrame()
        })
        const stageA = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)?.liveMessage
        expect(stageA).not.toBeNull()

        rejectBoundary = true
        canonical.mockClear()
        act(() => {
          h.emitDesktopBatch(
            batch(2, [
              status("owner-conn", 4, "connected"),
            ])
          )
          h.runAnimationFrame()
        })

        expect(canonical).toHaveBeenCalledTimes(1)
        expect(canonical.mock.calls[0]![0]).toBe(stageA)
        expect(canonical.mock.calls[0]![1]).toBe(true)
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.liveMessage).toBe(stageA)
        expect(runtime?.localTurns).toEqual([])
      } finally {
        resetConversationRuntimeStore()
      }
    })
  })
```

- [ ] **Step 3: Run the new tests and verify the failures**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "delegation suspension transcript checkpoints"
```

Expected: all three tests execute and fail. Stage A is absent from `localTurns`, and the unchanged checkpoint object is not re-acknowledged by canonical, so rejection cannot fence promotion yet.

- [ ] **Step 4: Add checkpoint-specific admission**

Immediately after `admitTurnComplete()`, add the dedicated helper below. Return the owner IDs directly instead of adding a second one-use admission interface. Leave `admitTurnComplete()` unchanged.

```typescript
function admitSuspensionCheckpoint(snapshot: ConnectionState): number[] {
  if (snapshot.status !== "prompting" || snapshot.liveMessage == null) {
    return []
  }

  const knownSessionId = resolveKnownConnectionSessionId(snapshot)
  const owners: number[] = []
  for (const [
    runtimeConversationId,
    runtime,
  ] of useConversationRuntimeStore.getState().byConversationId) {
    const runtimeLiveMatches =
      runtime.liveMessage?.id === snapshot.liveMessage.id
    const mappedToConnection =
      (knownSessionId != null && runtime.externalId === knownSessionId) ||
      (snapshot.conversationId != null &&
        (runtime.conversationId === snapshot.conversationId ||
          runtime.dbConversationId === snapshot.conversationId))
    if (!mappedToConnection && !runtimeLiveMatches) continue
    if (
      knownSessionId != null &&
      runtime.externalId != null &&
      runtime.externalId !== knownSessionId
    ) {
      continue
    }
    if (runtime.liveMessage != null && !runtimeLiveMatches) continue
    owners.push(runtimeConversationId)
  }

  return owners
}
```

- [ ] **Step 5: Carry checkpoint owners and split the prepared frame**

Add the new field to `PreparedEventFrame.connectionSteps`:

```typescript
    checkpointRuntimeConversationIds: readonly number[] | undefined
    completionRuntimeConversationIds: readonly number[] | undefined
```

Add checkpoint state beside completion state in `prepareEventFrame()`:

```typescript
    let stepCheckpointRuntimeConversationIds: readonly number[] | undefined
    let stepCompletionRuntimeConversationIds: readonly number[] | undefined
```

Include and reset it in `pushConnectionStep()`:

```typescript
        checkpointRuntimeConversationIds:
          stepCheckpointRuntimeConversationIds,
        completionRuntimeConversationIds: stepCompletionRuntimeConversationIds,
      })
      stepBefore = stepNext
      stepEvents = []
      stepRawFloor = highestSeq
      stepLiveMessageIsLive = undefined
      stepCheckpointRuntimeConversationIds = undefined
      stepCompletionRuntimeConversationIds = undefined
```

At the start of each event-loop iteration, evaluate the pre-event checkpoint. Do not skip an unadmitted status event because status must still update:

```typescript
      const event = connFrame.applyEvents[eventIndex]!
      const checkpointRuntimeConversationIds =
        event.type === "status_changed" &&
        event.status === "connected" &&
        snapshot.status === "prompting" &&
        snapshot.liveMessage != null
          ? admitSuspensionCheckpoint(snapshot)
          : []
      if (checkpointRuntimeConversationIds.length > 0) {
        stepCheckpointRuntimeConversationIds =
          checkpointRuntimeConversationIds
        stepLiveMessageIsLive = true
      }
      if (event.type === "turn_complete") {
        const admission = admitTurnComplete(
          snapshot,
          event,
          before.liveMessage,
          hasAuthoritativeTerminalDelivery(connFrame, event.seq)
        )
        if (!admission.accepted) continue
        stepCompletionRuntimeConversationIds = admission.runtimeConversationIds
      }
```

After reducer preview updates `snapshot`, split either accepted boundary:

```typescript
      if (
        event.type === "turn_complete" ||
        checkpointRuntimeConversationIds.length > 0
      ) {
        pushConnectionStep(event.seq, snapshot)
      }
```

- [ ] **Step 6: Reuse canonical filtering and promote exact owners**

Rename the final two parameters of `mirrorLiveMessageOnce()` and
`mirrorLiveMessageForCanonical()` without changing their existing completion
behavior:

```text
completionRuntimeConversationIds -> boundaryRuntimeConversationIds
rejectedCompletionRuntimeConversationIds -> rejectedBoundaryRuntimeConversationIds
```

Use the renamed owner filter and rejection collector:

```typescript
      if (
        boundaryRuntimeConversationIds &&
        sinks.runtimeConversationId != null &&
        !boundaryRuntimeConversationIds.has(sinks.runtimeConversationId)
      ) {
        return
      }
```

Replace the existing `hasTurnComplete`/canonical acknowledgement block with:

```typescript
      const hasTurnComplete =
        connectionFrame?.applyEvents.some(
          (event) => event.type === "turn_complete"
        ) ?? false
      const hasTranscriptBoundary =
        hasTurnComplete || boundaryRuntimeConversationIds !== undefined
      let canonicalAccepted = true

      if (liveChanged || hasTranscriptBoundary) {
        streamingPerfRecorder.setCurrentDeliveryIds(deliveryIds)
        try {
          canonicalAccepted =
            sinks.canonical(
              nextConn.liveMessage,
              liveMessageIsLive ?? nextConn.status === "prompting",
              deliveryIds
            ) !== false
          streamingPerfRecorder.flushQueuedLivePublication()
        } finally {
          streamingPerfRecorder.setCurrentDeliveryIds(null)
        }
      }
```

Replace the existing rejection collector with:

```typescript
      if (!canonicalAccepted && sinks.runtimeConversationId != null) {
        rejectedBoundaryRuntimeConversationIds?.add(
          sinks.runtimeConversationId
        )
      }
```

In each `commitEventFrame()` step, derive checkpoint owners before the current
`hasCompletion` declaration. Keep `hasCompletion` so ordinary completion
rejection collection is unchanged:

```typescript
        const checkpointRuntimeConversationIds =
          step.checkpointRuntimeConversationIds == null
            ? undefined
            : new Set(step.checkpointRuntimeConversationIds)
        const hasCompletion = connectionFrame.applyEvents.some(
          (event) => event.type === "turn_complete"
        )
        const rejectedBoundaryRuntimeConversationIds =
          hasCompletion || checkpointRuntimeConversationIds != null
            ? new Set<number>()
            : undefined
```

Delete the old `rejectedCompletionRuntimeConversationIds` declaration. After
the existing `completionRuntimeConversationIds` calculation, select the one
owner filter active for this already-split step:

```typescript
        const boundaryRuntimeConversationIds =
          completionRuntimeConversationIds ?? checkpointRuntimeConversationIds
```

Pass the boundary sets to the mirror, then promote the checkpoint before the
existing non-completion return:

```typescript
        mirrorLiveMessageForCanonical(
          contextKey,
          previousConnection,
          nextConnection,
          connectionFrame.deliveryIds,
          connectionFrame,
          step.liveMessageIsLive,
          boundaryRuntimeConversationIds,
          rejectedBoundaryRuntimeConversationIds
        )

        if (checkpointRuntimeConversationIds && finalLiveMessage) {
          const runtimeState = useConversationRuntimeStore.getState()
          for (const runtimeConversationId of checkpointRuntimeConversationIds) {
            if (
              rejectedBoundaryRuntimeConversationIds?.has(
                runtimeConversationId
              )
            ) {
              continue
            }
            const runtime = runtimeState.byConversationId.get(
              runtimeConversationId
            )
            if (runtime?.liveMessage?.id !== finalLiveMessage.id) continue
            completeLiveTranscriptTurn(runtimeConversationId, finalLiveMessage)
          }
        }

        if (!completion) continue
```

Rename the later completion rejection-filter reference to
`rejectedBoundaryRuntimeConversationIds`. Do not change completion ownership
fallbacks, accepted-completion markers, pending cleanup, notifications, or
user-stop handling. The checkpoint block adds none of those terminal effects.

- [ ] **Step 7: Run the checkpoint tests and complete context test file**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "delegation suspension transcript checkpoints"
pnpm test -- src/contexts/acp-connections-context.test.tsx
```

Expected: the focused tests pass, including exact checkpoint identity and canonical rejection; the complete context test file also passes.

- [ ] **Step 8: Commit the core checkpoint**

Run:

```powershell
pnpm exec prettier --write src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git diff --cached --check -- src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit --only -m "fix(acp): checkpoint suspended continuation transcripts" -- src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
```

Expected: one frontend commit containing complete desktop checkpoint behavior, canonical fencing, and the three regressions.

---

### Task 4: Preserve Web Replay And Live Checkpoint Semantics

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx:5180-5195`
- Test: `src/contexts/acp-connections-context.test.tsx:6900-9400`

**Interfaces:**
- Consumes: Task 3's `status()` test helper, complete desktop checkpoint, boundary filtering, and exact-owner promotion.
- Produces: transport-neutral checkpoint `liveMessageIsLive=true` for Web resume replay and live delivery without changing ordinary mapped-frame fallback behavior.

- [ ] **Step 1: Add a failing Web replay and live-delivery regression**

Add this test to the same checkpoint block:

```typescript
    it("checkpoints hidden stages during Web replay and live delivery", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")

      try {
        h.isDesktop = false
        await mountProvider()
        await act(async () => {
          await h.actions!.connect(
            TAB,
            "claude_code",
            "/tmp/x",
            "sess-1",
            42
          )
        })
        const publications: Array<{
          message: LiveMessage
          isLive: boolean
        }> = []
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical: (message, isLive) => {
            publications.push({ message, isLive })
            runtimeActions.setLiveMessage(42, message, isLive)
            return (
              useConversationRuntimeStore.getState().byConversationId.get(42)
                ?.liveMessage === message
            )
          },
        })
        const handlers = latestAttachHandlers()

        act(() => {
          handlers.onReplay(
            [
              {
                connection_id: "conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("conn", 2, "prompting"),
              content("conn", 3, "stage A"),
              status("conn", 4, "connected"),
              status("conn", 5, "prompting"),
              content("conn", 6, "stage B"),
              status("conn", 7, "connected"),
            ],
            7,
            0
          )
        })
        act(() => {
          for (const envelope of [
            status("conn", 8, "prompting"),
            content("conn", 9, "stage C"),
            status("conn", 10, "connected"),
            status("conn", 11, "prompting"),
            content("conn", 12, "stage D"),
            {
              connection_id: "conn",
              seq: 13,
              type: "usage_update" as const,
              used: 1,
              size: 100,
            },
          ]) {
            handlers.onEvent(envelope)
          }
        })

        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(
          runtime?.localTurns.map((turn) => ({
            role: turn.role,
            blocks: turn.blocks,
          }))
        ).toEqual([
          {
            role: "assistant",
            blocks: [{ type: "text", text: "stage A" }],
          },
          {
            role: "assistant",
            blocks: [{ type: "text", text: "stage B" }],
          },
          {
            role: "assistant",
            blocks: [{ type: "text", text: "stage C" }],
          },
        ])
        expect(runtime?.liveMessage?.content).toEqual([
          { type: "text", text: "stage D" },
        ])
        expect(publications.length).toBeGreaterThanOrEqual(4)
        expect(publications.every(({ isLive }) => isLive)).toBe(true)
      } finally {
        resetConversationRuntimeStore()
      }
    })
```

- [ ] **Step 2: Run the Web regression and verify the failure**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "Web replay and live delivery"
```

Expected: the test executes and fails because replayed B is rejected after A creates `localTurns`, and checkpoint publications carry `isLive=false`.

- [ ] **Step 3: Preserve the pre-boundary live bit for every delivery source**

In `pushConnectionStep()`, replace the `liveMessageIsLive` assignment with:

```typescript
        liveMessageIsLive:
          stepCheckpointRuntimeConversationIds != null
            ? true
            : connFrame.deliverySource === "desktop"
              ? stepLiveMessageIsLive
              : undefined,
```

Only checkpoint steps bypass the existing desktop-only rule. Ordinary mapped and replay frames retain current fallback behavior.

- [ ] **Step 4: Run focused and complete context tests**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "delegation suspension transcript checkpoints"
pnpm test -- src/contexts/acp-connections-context.test.tsx
```

Expected: canonical rejection leaves stage A live and unpromoted; Web replay/live delivery preserves A, B, and C in order with D live; all existing context tests pass.

- [ ] **Step 5: Commit Web checkpoint semantics**

Run:

```powershell
pnpm exec prettier --write src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git add src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git diff --cached --check -- src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit --only -m "fix(acp): preserve Web suspension checkpoints" -- src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
```

Expected: one frontend commit containing only transport-neutral checkpoint liveness and its Web regression.

---

### Task 5: Lock Owner, Alias, And Terminal Isolation

**Files:**
- Test: `src/contexts/acp-connections-context.test.tsx:6900-9400`
- Test: `src/contexts/acp-connections-context.test.tsx:12400-12700`

**Interfaces:**
- Consumes: Task 3's `status()` helper, the complete checkpoint implementation from Tasks 3 and 4, and the existing canonical observer-alias harness.
- Produces: coverage for exact post-mirror ownership, ownerless no-op behavior, conflicting-live rejection, multi-alias promotion, and ordinary completion exactly once. Task 3 already covers late out-of-turn deltas.

- [ ] **Step 1: Add the exact post-mirror owner regression**

Add this test to the checkpoint describe block:

```typescript
    it("requires exact post-mirror live ownership", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")
      runtimeActions.appendOptimisticTurn(
        42,
        {
          id: "user-in-flight",
          role: "user",
          blocks: [{ type: "text", text: "keep this pending" }],
          timestamp: "2026-08-25T07:31:49.000Z",
        },
        "turn-in-flight"
      )

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
        const canonical = vi.fn(
          (_message: LiveMessage, _isLive: boolean) => true
        )
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical,
        })
        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              {
                connection_id: "owner-conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("owner-conn", 2, "prompting"),
              content("owner-conn", 3, "stage A"),
            ])
          )
          h.runAnimationFrame()
        })
        canonical.mockClear()

        act(() => {
          h.emitDesktopBatch(
            batch(2, [
              status("owner-conn", 4, "connected"),
            ])
          )
          h.runAnimationFrame()
        })

        expect(canonical).toHaveBeenCalledTimes(1)
        expect(canonical.mock.calls[0]![1]).toBe(true)
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.liveMessage).toBeNull()
        expect(runtime?.localTurns).toEqual([])
        expect(runtime?.optimisticTurns.map((turn) => turn.id)).toEqual([
          "user-in-flight",
        ])
      } finally {
        resetConversationRuntimeStore()
      }
    })
```

The canonical sink deliberately returns `true` without installing the message. The runtime remains eligible for completion's optimistic fallback, but checkpoint promotion must reject it.

- [ ] **Step 2: Add ownerless and conflicting-live regressions**

Add these tests to the checkpoint block:

```typescript
    it("does not materialize a runtime for an ownerless checkpoint", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1")
        const runtimeCount =
          useConversationRuntimeStore.getState().byConversationId.size
        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              status("owner-conn", 1, "prompting"),
              content("owner-conn", 2, "unowned stage"),
              status("owner-conn", 3, "connected"),
            ])
          )
          h.runAnimationFrame()
        })

        expect(
          useConversationRuntimeStore.getState().byConversationId.size
        ).toBe(runtimeCount)
        expect(h.store!.getConnection(TAB)?.status).toBe("connected")
      } finally {
        resetConversationRuntimeStore()
      }
    })

    it("does not replace a mapped runtime that owns another live message", async () => {
      const { useConversationRuntimeStore, resetConversationRuntimeStore } =
        await import("@/stores/conversation-runtime-store")
      resetConversationRuntimeStore()
      const runtimeActions = useConversationRuntimeStore.getState().actions
      runtimeActions.setExternalId(42, "sess-1")

      try {
        await mountDesktopOwner("owner-conn", TAB, "sess-1", 42)
        act(() => {
          h.emitDesktopBatch(
            batch(1, [
              {
                connection_id: "owner-conn",
                seq: 1,
                type: "session_started",
                session_id: "sess-1",
              },
              status("owner-conn", 2, "prompting"),
              content("owner-conn", 3, "connection stage"),
            ])
          )
          h.runAnimationFrame()
        })

        const otherLive: LiveMessage = {
          id: "other-live",
          role: "assistant",
          content: [{ type: "text", text: "other turn" }],
          startedAt: 1_700_000_000_000,
        }
        runtimeActions.setLiveMessage(42, otherLive, true)
        const canonical = vi.fn((message: LiveMessage, isLive: boolean) => {
          runtimeActions.setLiveMessage(42, message, isLive)
          return (
            useConversationRuntimeStore.getState().byConversationId.get(42)
              ?.liveMessage === message
          )
        })
        h.actions!.registerLiveSinks(TAB, {
          runtimeConversationId: 42,
          canonical,
        })
        canonical.mockClear()

        act(() => {
          h.emitDesktopBatch(
            batch(2, [
              status("owner-conn", 4, "connected"),
            ])
          )
          h.runAnimationFrame()
        })

        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(42)
        expect(runtime?.liveMessage).toBe(otherLive)
        expect(runtime?.localTurns).toEqual([])
        expect(canonical).not.toHaveBeenCalled()
      } finally {
        resetConversationRuntimeStore()
      }
    })
```

- [ ] **Step 3: Add a canonical multi-alias checkpoint regression**

Place this test beside `promotes a coalesced completion in every canonical observer alias`:

```typescript
  it("checkpoints a suspended stage in every canonical observer alias", async () => {
    const TAB2 = "conv-2-claude_code-99"
    const { useConversationRuntimeStore, resetConversationRuntimeStore } =
      await import("@/stores/conversation-runtime-store")
    resetConversationRuntimeStore()
    const runtimeActions = useConversationRuntimeStore.getState().actions
    runtimeActions.setExternalId(42, "sess-shared")
    runtimeActions.setExternalId(99, "sess-shared")

    try {
      h.acpFindConnectionForConversation.mockResolvedValue({
        connection_id: "broker-child",
        event_seq: 0,
      })
      await mountProvider()
      await act(async () => {
        await h.actions!.connect(
          TAB,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          42
        )
      })
      const handlers = latestAttachHandlers()
      emitAcpEvent(handlers, {
        seq: 1,
        connection_id: "broker-child",
        type: "session_started",
        session_id: "sess-shared",
      })
      h.acpFindConnectionForConversation.mockResolvedValue(null)
      await act(async () => {
        await h.actions!.connect(
          TAB2,
          "claude_code",
          "/tmp/x",
          "sess-shared",
          99
        )
      })

      for (const [contextKey, conversationId] of [
        [TAB, 42],
        [TAB2, 99],
      ] as const) {
        h.actions!.registerLiveSinks(contextKey, {
          runtimeConversationId: conversationId,
          canonical: (message, isLive) => {
            runtimeActions.setLiveMessage(conversationId, message, isLive)
            return (
              useConversationRuntimeStore
                .getState()
                .byConversationId.get(conversationId)?.liveMessage === message
            )
          },
        })
      }

      act(() => {
        handlers.onReplay(
          [
            status("broker-child", 2, "prompting"),
            content("broker-child", 3, "shared stage A"),
            status("broker-child", 4, "connected"),
            status("broker-child", 5, "prompting"),
            content("broker-child", 6, "shared stage B"),
          ],
          6,
          1
        )
      })

      for (const conversationId of [42, 99]) {
        const runtime = useConversationRuntimeStore
          .getState()
          .byConversationId.get(conversationId)
        expect(runtime?.localTurns).toEqual([
          {
            id: expect.stringMatching(
              new RegExp(`^live-${conversationId}-`)
            ),
            role: "assistant",
            blocks: [{ type: "text", text: "shared stage A" }],
            timestamp: expect.any(String),
          },
        ])
        expect(runtime?.liveMessage?.content).toEqual([
          { type: "text", text: "shared stage B" },
        ])
      }
    } finally {
      resetConversationRuntimeStore()
    }
  })
```

- [ ] **Step 4: Add backend trailing connected to the ordinary-completion regression**

In `promotes an earlier completion without pairing it with a later turn`, replace the events beginning at its completion with:

```typescript
            {
              connection_id: "owner-conn",
              seq: 3,
              type: "turn_complete",
              session_id: "sess-1",
              stop_reason: "end_turn",
              mark_awaiting_reply: false,
            },
            status("owner-conn", 4, "connected"),
            status("owner-conn", 5, "prompting"),
            content("owner-conn", 6, "answer B in progress"),
```

Keep its existing assertion that only answer A is in `localTurns` and answer B is live. This proves trailing connected does not promote twice.

- [ ] **Step 5: Run the focused safety matrix**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "requires exact post-mirror|ownerless checkpoint|owns another live message|every canonical observer alias|promotes an earlier completion"
```

Expected: every selected test executes and passes. Exact ownership rejects the completion fallback; both aliases checkpoint once; ordinary completion remains exactly once.

- [ ] **Step 6: Run the complete context test file**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx
```

Expected: all tests in the file pass, including existing initial-connected, out-of-turn streaming, notification, user-stop, retained-completion, and alias coverage.

- [ ] **Step 7: Commit the safety coverage**

Run:

```powershell
pnpm exec prettier --write src/contexts/acp-connections-context.test.tsx
git add src/contexts/acp-connections-context.test.tsx
git diff --cached --check -- src/contexts/acp-connections-context.test.tsx
git commit --only -m "test(acp): cover suspension checkpoint ownership" -- src/contexts/acp-connections-context.test.tsx
```

Expected: one test-only frontend commit.

---

### Task 6: Run Final Verification And Review The Scope

**Files:**
- Verify: `src/contexts/acp-connections-context.tsx`
- Verify: `src/contexts/acp-connections-context.test.tsx`
- Verify: `src-tauri/src/acp/connection.rs`
- Verify: `docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md`

**Interfaces:**
- Consumes: all prior task commits.
- Produces: evidence that focused regression, full frontend suite, lint, static export build, Rust contract, formatting, and scope checks pass together.

- [ ] **Step 1: Verify formatting without creating a cleanup commit**

Run:

```powershell
pnpm exec prettier --check src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx docs/designs/2026-08-28-delegation-suspension-transcript-checkpoint-design.md docs/superpowers/plans/2026-08-28-delegation-suspension-transcript-checkpoint.md
```

Expected: Prettier exits 0. Tasks 3-5 formatted their own files before committing, so final verification must not need a broad style-only commit.

- [ ] **Step 2: Run focused frontend and Rust evidence**

Run:

```powershell
pnpm test -- src/contexts/acp-connections-context.test.tsx -t "delegation suspension transcript checkpoints"
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils acp::connection::tests::delegation_suspend_cancelled_response_clears_turn_without_tree_cancel -- --exact
```

Expected: every checkpoint regression passes, Rust formatting is clean, and exactly one Rust test passes.

- [ ] **Step 3: Run lint, the complete frontend suite, and the static build**

Run:

```powershell
pnpm eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
pnpm test
pnpm build
```

Expected: ESLint exits 0, full Vitest reports zero failures, and the Next.js static export succeeds.

- [ ] **Step 4: Prove scope and contract preservation from the commits**

Run:

```powershell
git status --short
git log --oneline --name-only -5
```

Inspect the five planned commits by subject and path instead of assuming a
fixed `HEAD~N` diff base. Expected: the worktree contains no new unexpected
changes, and the commits touch only the two planning documents, the context
source/test pair, and the existing Rust test file.

Expected:

- Production TypeScript changes are limited to checkpoint admission, prepared-step metadata, mirror parameter naming and acknowledgement, transport-neutral checkpoint `isLive`, and exact-owner promotion.
- `admitTurnComplete()` has no semantic change.
- UUID allocation remains in the reducer; `FrameAction` has no new message-ID field.
- Rust production code is unchanged; only the existing test assertion changed.
- No snapshot, transport, database, parser, dependency, notification, awaiting-reply, or lifecycle code changed.

---

## Completion Criteria

- Desktop delivery promotes stage A before stage B starts.
- One coalesced frame promotes A using the exact canonical A object while final connection and runtime share B's object.
- Canonical `false` prevents promotion even when runtime still owns exact A.
- Web resume replay preserves at least two completed hidden stages and leaves the newest stage live, with every checkpoint publication marked live.
- A runtime without exact post-mirror live ID is not promoted even when completion's optimistic fallback would accept it.
- Ownerless and conflicting-live runtimes are not materialized, overwritten, or promoted.
- Every exact canonical observer alias receives one completed checkpoint turn.
- Late out-of-turn content does not mutate or republish a completed checkpoint.
- Ordinary `turn_complete` followed by backend `connected` remains exactly once with existing terminal behavior.
- Successful Rust suspension records exactly one `StatusChanged(Connected)` and no `TurnComplete`.
- Focused tests, complete context test, scoped ESLint, full Vitest, static export build, Rust exact test, formatting, and scope checks all pass.
