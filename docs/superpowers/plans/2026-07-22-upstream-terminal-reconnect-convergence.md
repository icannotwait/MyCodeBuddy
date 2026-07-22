# Upstream Terminal Reconnect Convergence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep a root conversation visibly cancelled after an upstream terminal disconnect, require an explicit reconnect, and never replay queued work without an explicit queue-resume decision.

**Architecture:** Expose the backend's existing ACP `Error.terminal` classification on the frontend wire. `ConversationSessionSurface` combines that immediate signal with its authoritative workspace-summary row to gate automatic connect/focus retry. The lifecycle hook offers a separate explicit reconnect, while a per-tab terminal queue-pause latch blocks automatic FIFO flushing but allows an intentional new prompt.

**Tech Stack:** Rust 2021, Tokio, ACP events, React 19, TypeScript strict mode, Zustand, Vitest, next-intl, Lucide.

## Global Constraints

- No database migration or durable cancellation-source field.
- `summary.status === "cancelled"` is the only status-derived automatic-connect block. `pending_review` and `completed` keep existing behavior.
- A missing persisted summary is fail-closed for automatic connection.
- Wire `terminal: boolean` on all ACP error events. Only `terminal: true` is a terminal error.
- Arm terminal latches only while the bound root summary is `in_progress`, capturing that row's `updated_at` baseline. Treat bare `status_changed: "disconnected"` with the same rule.
- Keep reconnect suppression through the stale baseline row and a later `cancelled` row. Clear it only for a non-`cancelled` authoritative row whose `updated_at` is newer than the captured baseline. Clear queue pause only through Resume Queue.
- Preserve active/idle user-Cancel behavior, existing normal FIFO, delegate terminal projections, and `send_prompt_linked` status writes.
- Reconnect never changes row status or flushes queue. A direct prompt bypasses a terminal-paused queue; Resume Queue restores FIFO.
- Localize all user-visible copy in all ten `src/i18n/messages/*.json` files. Use native accessible buttons with Lucide icons.
- Work only in `D:\MyCodeBuddy\.worktrees\upstream-terminal-reconnect-convergence` on `feat/upstream-terminal-reconnect-convergence`.

---

## File Structure

- `src-tauri/src/acp/types.rs`: serializes terminal error classification.
- `src/lib/types.ts`: mirrors the error-wire contract.
- `src-tauri/src/acp/lifecycle.rs` and `src-tauri/src/acp/delegation/broker.rs`: prove root terminal CAS precedence and delegate `parent_disconnected` causality.
- `src/hooks/use-connection-lifecycle.ts` and test: separates automatic/focus permission from explicit reconnect while retaining a compatibility default until the surface explicitly supplies its durable policy.
- `src/lib/terminal-reconnect.ts` and test: pure terminal-latch predicates.
- `src/lib/queue-flush.ts` and test: paused-queue direct-send exception.
- `src/components/conversations/conversation-session-surface.tsx` and test: durable summary selection, latches, lifecycle and queue integration.
- `src/components/chat/conversation-shell.tsx`, `chat-input.tsx`, `message-queue-display.tsx`, plus tests: explicit controls.
- All locale JSON files: Reconnect and Resume Queue labels.

### Task 1: Expose Terminal ACP Errors on the Wire

**Files:**

- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/contexts/acp-connections-context.test.tsx`
- Test: `src-tauri/src/acp/types.rs`

**Interfaces:**

- Consumes: existing `AcpEvent::Error { message, agent_type, code, terminal }` emitters in `connection.rs`.
- Produces: `terminal: boolean` in web/desktop JSON and in the TypeScript `AcpEvent` error variant.
- Preserves: existing error codes/messages and nonterminal classification of recoverable errors.

- [ ] **Step 1: Write the failing serialization test**

Add an in-module test in `src-tauri/src/acp/types.rs`:

```rust
#[test]
fn error_events_serialize_terminal_classification() {
    let terminal = AcpEvent::Error {
        message: "agent exited".into(), agent_type: "codex".into(),
        code: Some("process_exited".into()), terminal: true,
    };
    let recoverable = AcpEvent::Error {
        message: "mode rejected".into(), agent_type: "codex".into(),
        code: None, terminal: false,
    };
    assert_eq!(serde_json::to_value(terminal).unwrap()["terminal"], true);
    assert_eq!(serde_json::to_value(recoverable).unwrap()["terminal"], false);
}
```

- [ ] **Step 2: Confirm the regression is red**

```powershell
Set-Location src-tauri
cargo test --features test-utils error_events_serialize_terminal_classification --lib
```

Expected: FAIL because `terminal` is currently `#[serde(skip, default)]`.

- [ ] **Step 3: Implement the wire contract**

Change the error field to `#[serde(default)]` and update its doc comment to state it reaches frontend consumers for reconnect suppression. Add this required property to the `type: "error"` union branch in `src/lib/types.ts`:

```ts
code: string | null
terminal: boolean
```

Do not change `connection.rs` terminal/nonterminal emitter sites.

Update every typed `EventEnvelope` error fixture, including
`src/contexts/acp-connections-context.test.tsx`, with `terminal: false` unless
the test specifically exercises terminal delivery. Add one raw subscriber test
there that emits `terminal: true` and asserts the `useAcpEvent` callback
receives that value unchanged.

- [ ] **Step 4: Confirm green and commit**

```powershell
cargo test --features test-utils error_events_serialize_terminal_classification --lib
Set-Location ..
pnpm eslint src/lib/types.ts
pnpm test -- src/contexts/acp-connections-context.test.tsx
git add src-tauri/src/acp/types.rs src/lib/types.ts src/contexts/acp-connections-context.test.tsx
git commit -m "feat(acp): expose terminal error classification"
```

Expected: both terminal values serialize and static checking succeeds.

### Task 2: Prove Terminal Root Convergence and Delegate Causality

**Files:**

- Modify: `src-tauri/src/acp/lifecycle.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Test: `src-tauri/src/acp/lifecycle.rs`, `src-tauri/src/acp/delegation/broker.rs`

**Interfaces:**

- Consumes: `handle_terminal_event`, a linked root `CachedConn`, a delayed
  `AcpEvent::TurnComplete { stop_reason: "end_turn" }`, and
  `DelegationBroker::cancel_by_parent`.
- Produces: a durable root `Cancelled` winner with exactly one
  `conversation://changed` State patch; child settlement remains broker-owned
  with error code `parent_disconnected`.
- Preserves: lifecycle CAS semantics, one global patch per winner, and existing
  delegation first-terminal-wins behavior.

- [ ] **Step 1: Add failing root terminal-order regression**

In `src-tauri/src/acp/lifecycle.rs`, add a test beside
`terminal_disconnect_emits_exactly_one_state_event_on_cas_win` that creates an
in-progress root, registers a fake live connection with a web-only broadcaster,
and seeds the linked cache. Call `handle_terminal_event`, then call
`handle_event` with the same connection and this delayed envelope:

```rust
AcpEvent::TurnComplete {
    session_id: "ext-1".into(),
    stop_reason: "end_turn".into(),
    agent_type: "claude_code".into(),
    mark_awaiting_reply: false,
}
```

Assert the persisted row is still `ConversationStatus::Cancelled` and draining
the broadcaster yields exactly one `CONVERSATION_CHANGED_EVENT` State patch
whose status is `"cancelled"`.

Extend `connection_teardown_cascades_parent_disconnected` in
`src-tauri/src/acp/delegation/broker.rs` with an assertion that its terminal
state emission remains owned by the broker (one settled child record with
`error_code == Some("parent_disconnected")`) after the root-terminal scenario.

- [ ] **Step 2: Run the new regression against existing lifecycle code**

```powershell
Set-Location src-tauri
cargo test --features test-utils terminal_disconnect_wins_over_delayed_end_turn --lib
```

Expected: PASS without production changes if the existing CAS implementation is
correct. This task is regression coverage for an existing durable guarantee;
do not weaken the delayed `TurnComplete` assertion merely to manufacture a red
test.

- [ ] **Step 3: Complete only required fixture wiring**

Use the existing `fake_connection_with_state`, `seed_cache`,
`read_row_status`, and `WebEventBroadcaster` helpers. Do not change lifecycle
production logic unless the regression demonstrates a real CAS defect. Keep
the child test on `DelegationBroker`; do not move its settlement ownership into
the lifecycle worker.

- [ ] **Step 4: Confirm green and commit**

```powershell
cargo test --features test-utils terminal_disconnect_wins_over_delayed_end_turn --lib
cargo test --features test-utils connection_teardown_cascades_parent_disconnected --lib
git add src/acp/lifecycle.rs src/acp/delegation/broker.rs
git commit -m "test(acp): cover terminal reconnect convergence"
Set-Location ..
```

Expected: terminal root CAS remains the winner after delayed completion and
delegate evidence still reports `parent_disconnected` through the broker.

### Task 3: Split Automatic and Explicit Connection Policy

**Files:**

- Modify: `src/hooks/use-connection-lifecycle.ts`
- Modify: `src/hooks/use-connection-lifecycle.test.ts`

**Interfaces:**

- Consumes: `isActive` and `autoConnectAllowed?: boolean`, which defaults to `true` only for pre-Task-5 callers.
- Produces: `handleReconnect(): Promise<void>` that calls existing `connect` with the stored agent, cwd, external session, conversation id, route and owner operation id.
- Preserves: active-key updates, unmount cleanup, existing successful auto connection and all connection arguments.

- [ ] **Step 1: Write failing hook tests**

Update existing render options with `autoConnectAllowed: true`, then add the
following regressions. Also retain one existing-behavior test that omits the
new option and observes normal automatic connection through the compatibility
default:

```ts
it("does not automatically connect or focus-retry when autoConnectAllowed is false", async () => {
  const { result } = renderHook(() => useConnectionLifecycle({
    contextKey: "terminal-tab", agentType: "codex", isActive: true,
    autoConnectAllowed: false, workingDir: "/tmp/project", sessionId: "s1",
    conversationId: 42,
  }))
  await act(async () => {})
  expect(h.connect).not.toHaveBeenCalled()
  h.touchActivity.mockClear()
  act(() => result.current.handleFocus())
  expect(h.connect).not.toHaveBeenCalled()
  expect(h.touchActivity).toHaveBeenCalledTimes(1)
  expect(h.touchActivity).toHaveBeenCalledWith("terminal-tab")
})

it("explicit reconnect preserves the stored session identity", async () => {
  const { result } = renderHook(() => useConnectionLifecycle({
    contextKey: "terminal-tab", agentType: "codex", isActive: true,
    autoConnectAllowed: false, workingDir: "/tmp/project", sessionId: "s1",
    conversationId: 42, ownerOperationId: "op-1",
  }))
  await result.current.handleReconnect()
  expect(h.connect).toHaveBeenCalledTimes(1)
  expect(h.connect).toHaveBeenCalledWith("codex", "/tmp/project", "s1", 42, undefined, "op-1")
})
```

- [ ] **Step 2: Confirm red**

```powershell
pnpm test -- src/hooks/use-connection-lifecycle.test.ts
```

Expected: new option/callback are absent and automatic connection still ignores policy.

- [ ] **Step 3: Implement the separated policies**

Expose `touchActivity` from the hoisted ACP-actions mock so the disabled-focus
test can verify its existing activity behavior. Remove the warning-producing
unused mock callback parameters rather than suppressing ESLint; a zero-argument
implementation remains assignable to `ConnectFn` while Vitest still records the
real call arguments. Destructure with `autoConnectAllowed = true`, then gate both auto effect and
`handleFocus` with `isActive && autoConnectAllowed`. Task 5 must still pass an
explicit durable policy; the default exists only so this task leaves the branch
type-correct. Add and return this callback; it must not call the queue or
mutate conversation status:

```ts
const handleReconnect = useCallback(async () => {
  setLastAutoConnectError(null)
  await connConnect(agentType, workingDir, sessionId, conversationId,
    delegationRouteOverride, ownerOperationIdRef.current)
}, [agentType, workingDir, sessionId, conversationId,
  delegationRouteOverride, connConnect])
```

- [ ] **Step 4: Confirm green and commit**

```powershell
pnpm test -- src/hooks/use-connection-lifecycle.test.ts
pnpm eslint src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts
git add src/hooks/use-connection-lifecycle.ts src/hooks/use-connection-lifecycle.test.ts
git commit -m "feat(acp): separate explicit reconnect from auto connect"
```

Expected: omitted policy retains legacy auto-connect, disabled auto/focus never
invokes `connect`, explicit reconnect invokes it once, and ESLint is warning-free.

### Task 4: Implement Terminal Queue Policy and Controls

**Files:**

- Create: `src/lib/terminal-reconnect.ts`, `src/lib/terminal-reconnect.test.ts`
- Modify: `src/lib/queue-flush.ts`, `src/lib/queue-flush.test.ts`
- Modify: `src/components/chat/conversation-shell.tsx`, `src/components/chat/chat-input.tsx`, `src/components/chat/message-queue-display.tsx`
- Modify: `src/components/chat/chat-input.test.tsx`
- Create: `src/components/chat/message-queue-display.test.tsx`
- Modify: `src/i18n/messages/ar.json`, `de.json`, `en.json`, `es.json`, `fr.json`, `ja.json`, `ko.json`, `pt.json`, `zh-CN.json`, `zh-TW.json`

**Interfaces:**

- `shouldLatchTerminalDisconnect(event, connectionId, summary): boolean` returns true only for the same connection while the root summary is `in_progress`, and either `error.terminal` or bare disconnected.
- `shouldClearTerminalDisconnectLatch(latch, summary): boolean` returns true only for a non-`cancelled` summary whose `updated_at` is newer than the latch's captured baseline.
- `shouldQueueDirectSend(fromQueueFlush, queueLength, queuePausedByTerminalDisconnect): boolean` preserves FIFO except during terminal pause.
- UI receives optional `showReconnect`/`onReconnect` and `queuePaused`/`onResumeQueue` through shell, input and queue display.

- [ ] **Step 1: Write failing pure policy and component tests**

Create terminal predicate tests for terminal versus recoverable error, matching versus nonmatching connection, and both error/bare-disconnect paths with an `in_progress` versus cancelled/pending summary. Test the full ordering sequence: terminal event captures baseline `2026-07-22T01:00:00.000Z`; the unchanged stale `in_progress` row does not clear; a newer `cancelled` row does not clear; and newer `in_progress`, `pending_review`, and `completed` rows do clear.

Extend queue tests with:

```ts
expect(shouldQueueDirectSend(false, 2, true)).toBe(false)
expect(shouldQueueDirectSend(false, 2, false)).toBe(true)
```

Render ChatInput/MessageQueueDisplay with test callbacks. Assert Reconnect and Resume Queue are absent without their predicates, appear as native buttons with localized labels when active, and call each callback once on click.

- [ ] **Step 2: Confirm red**

```powershell
pnpm test -- src/lib/terminal-reconnect.test.ts src/lib/queue-flush.test.ts src/components/chat/chat-input.test.tsx src/components/chat/message-queue-display.test.tsx
```

Expected: predicate imports, third queue argument, controls, and test file are absent.

- [ ] **Step 3: Implement pure terminal and queue predicates**

Create `src/lib/terminal-reconnect.ts` with this exact behavior:

```ts
export interface TerminalDisconnectLatch { baselineUpdatedAt: string }

export function shouldLatchTerminalDisconnect(event: EventEnvelope, connectionId: string | null, summary: Pick<DbConversationSummary, "status" | "updated_at"> | null): boolean {
  if (connectionId == null || event.connection_id !== connectionId || summary?.status !== "in_progress") return false
  return event.type === "error" ? event.terminal : event.type === "status_changed" && event.status === "disconnected"
}
export function shouldClearTerminalDisconnectLatch(latch: TerminalDisconnectLatch | null, summary: Pick<DbConversationSummary, "status" | "updated_at"> | null): boolean {
  return latch != null && summary != null && summary.status !== "cancelled" &&
    Date.parse(summary.updated_at) > Date.parse(latch.baselineUpdatedAt)
}
```

Change `shouldQueueDirectSend` to:

```ts
return !fromQueueFlush && !queuePausedByTerminalDisconnect && queueLength > 0
```

- [ ] **Step 4: Implement controls and all translations**

Thread the control props through ConversationShell and ChatInput. ChatInput renders a compact `type="button"` Reconnect command directly above the composer using Lucide `RotateCcw`, visible text, and `title={t("reconnect")}`. MessageQueueDisplay accepts `paused` and `onResumeQueue`; when its queue is nonempty and paused, render its pause text and a `type="button"` Lucide `Play` command with visible Resume Queue text. Preserve reorder/edit/delete behavior.

Add `chatInput.reconnect`, `messageQueue.paused`, and `messageQueue.resumeQueue` adjacent to existing keys in every locale. The `en.json` values are exactly `Reconnect`, `Queue paused`, and `Resume queue`.

- [ ] **Step 5: Confirm green and commit**

```powershell
pnpm test -- src/lib/terminal-reconnect.test.ts src/lib/queue-flush.test.ts src/components/chat/chat-input.test.tsx src/components/chat/message-queue-display.test.tsx
pnpm eslint src/lib/terminal-reconnect.ts src/lib/queue-flush.ts src/components/chat/conversation-shell.tsx src/components/chat/chat-input.tsx src/components/chat/message-queue-display.tsx
git add src/lib/terminal-reconnect.ts src/lib/terminal-reconnect.test.ts src/lib/queue-flush.ts src/lib/queue-flush.test.ts src/components/chat/conversation-shell.tsx src/components/chat/chat-input.tsx src/components/chat/message-queue-display.tsx src/components/chat/chat-input.test.tsx src/components/chat/message-queue-display.test.tsx src/i18n/messages
git commit -m "feat(conversations): pause queue after terminal disconnect"
```

Expected: direct sends only bypass queue during terminal pause, controls are accessible, and user-Cancel has no new pause path.

### Task 5: Integrate Durable Root State, Latches, and Queue Actions

**Files:**

- Modify: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-session-surface.test.ts`

**Interfaces:**

- Consumes: Task 3 `autoConnectAllowed`/`handleReconnect`; Task 4 predicates and controls.
- Produces: fail-closed persisted reconnect policy, terminal latches, paused auto-flush, direct prompt bypass, and FIFO restoration after Resume Queue.
- Preserves: workspace state patch ownership, child delegate detail, user-Cancel, normal FIFO, and prompt status transitions.

- [ ] **Step 1: Write failing surface regressions**

Export a narrow pure surface policy seam if necessary; do not mock the entire session view. Test that missing and cancelled persisted summaries deny automatic connection, `pending_review`/`completed` allow it, and a terminal latch denies it. In addition, add a focused `ConversationSessionSurface` harness that mocks and captures `useConnectionLifecycle` options: assert `autoConnectAllowed === false` reaches the hook for a missing persisted summary, a cancelled summary, and an armed terminal latch, while a non-cancelled resolved summary passes `true`. This captured-options assertion is required even though the compatibility default permits omission. Add an event-to-latch harness proving a terminal error and bare disconnect block focus before the global patch, the unchanged stale `in_progress` summary does not clear the latch, a newer `cancelled` patch remains latched, and only a later newer `in_progress`/`pending_review`/`completed` patch clears it. Also prove direct send leaves a terminal-paused head untouched and Resume Queue dequeues that head FIFO.

- [ ] **Step 2: Confirm red**

```powershell
pnpm test -- src/components/conversations/conversation-session-surface.test.ts
```

Expected: missing policy seam and integration wiring fail the new cases.

- [ ] **Step 3: Wire the session surface**

Select the root summary with `useAppWorkspaceStore((s) => s.conversations.find((row) => row.id === dbConversationId) ?? null)`. Keep `terminalDisconnectLatch` as `TerminalDisconnectLatch | null` and `queuePausedByTerminalDisconnect` as a boolean, initially null/false. A `useAcpEvent` callback uses Task 4's predicate to set both and captures `persistedSummary.updated_at` only on the first latch. An effect invokes Task 4's clear predicate with the latch and current summary, so the stale baseline row and a newer cancelled row remain blocked; queue pause changes only via `onResumeQueue`.

Pass real tab activity as `isActive` and separately derive `autoConnectAllowed`: false for a persisted missing summary, cancelled summary, or reconnect latch. Pass that value explicitly to `useConnectionLifecycle` even though its temporary compatibility default is true. Show explicit Reconnect only for a cancelled/latch-root whose ACP state is `null`, `disconnected`, or `error`. Add queue pause to the auto-flush effect and its timer recheck, pass it to `shouldQueueDirectSend`, and thread controls to ConversationShell. Reconnect must call only `handleReconnect`.

- [ ] **Step 4: Confirm focused green**

```powershell
pnpm test -- src/components/conversations/conversation-session-surface.test.ts src/hooks/use-connection-lifecycle.test.ts src/lib/terminal-reconnect.test.ts src/lib/queue-flush.test.ts src/components/chat/chat-input.test.tsx src/components/chat/message-queue-display.test.tsx src/contexts/app-workspace-context.test.tsx
pnpm eslint src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts
```

Expected: state patches remain authoritative, pre-patch focus cannot reconnect, explicit reconnect does not mutate status or queue, direct send bypasses only paused historical work, and Resume Queue restores FIFO.

- [ ] **Step 5: Run full verification and commit**

```powershell
pnpm test
pnpm eslint .
pnpm build
Set-Location src-tauri
cargo test --features test-utils terminal_disconnect_emits_exactly_one_state_event_on_cas_win --lib
cargo test --features test-utils terminal_disconnect_wins_over_delayed_end_turn --lib
cargo test --features test-utils connection_teardown_cascades_parent_disconnected --lib
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
Set-Location ..
git add src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts
git commit -m "fix(conversations): gate terminal reconnect and queue replay"
```

Expected: all commands exit 0. Do not merge, push, or create a pull request.
