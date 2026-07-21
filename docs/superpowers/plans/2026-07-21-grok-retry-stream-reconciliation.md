# Grok Retry Stream Reconciliation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reconcile Grok provider retries before generic ACP rendering so failed-attempt output is rolled back or dropped across chat, automatic titles, translation, delegation, and chat-channel consumers.

**Architecture:** A per-prompt `GrokRetryReconciler` consumes raw xAI metadata before typed ACP conversion. It emits an invisible `TurnAttemptRollback` barrier for already-streamed speculative output and drops causally older late updates from the failed stream; downstream consumers truncate only content after the latest accepted tool-call boundary.

**Tech Stack:** Rust 2021, Tokio, sacp/ACP JSON-RPC, serde_json, React 19, TypeScript strict, Vitest.

## Global Constraints

- Reconciliation applies only to Grok updates during an active prompt.
- Only `_x.ai/session/update` with `sessionUpdate=retry_state` and `type=retrying` starts rollback.
- Ambiguous or malformed metadata fails open; never infer retries from generated text.
- Preserve accepted tool calls and all content before the latest tool-call boundary.
- Keep the existing exact-AA title collapse as defense in depth; add no fuzzy matching.
- No database migration and no automatic rewrite of existing malformed titles.
- Do not alter non-Grok or xAI compact-notification behavior.

---

### Task 1: Pure Grok Retry Reconciler

**Files:**
- Create: `src-tauri/src/acp/grok_retry.rs`
- Modify: `src-tauri/src/acp/mod.rs`

**Interfaces:**
- Consumes: raw `sacp::UntypedMessage` notifications.
- Produces: `GrokRetryReconciler::observe(&UntypedMessage) -> GrokRetryAction`.
- Produces: `GrokRetryAction::{Pass, Consume, Rollback { attempt }, DropStale { update_kind }}`.

- [ ] **Step 1: Write failing unit tests for the conversation-800 ordering**

Add tests that construct raw notifications with the real metadata shape:

```rust
let mut reconciler = GrokRetryReconciler::default();
assert_eq!(
    reconciler.observe(&standard("agent_thought_chunk", 21, 1_000, "p", 100)),
    GrokRetryAction::Pass
);
assert_eq!(
    reconciler.observe(&retry(32, 1_100, 1)),
    GrokRetryAction::Rollback { attempt: 1 }
);
assert!(matches!(
    reconciler.observe(&standard("agent_message_chunk", 31, 1_100, "p", 100)),
    GrokRetryAction::DropStale { update_kind: "agent_message_chunk" }
));
assert_eq!(
    reconciler.observe(&standard("agent_message_chunk", 61, 2_000, "p", 200)),
    GrokRetryAction::Pass
);
```

Also cover consecutive retries, missing metadata fail-open, a different
`promptId`, `failed`/`exhausted` retry states, `ToolCallUpdate`, and the bounded
failed-window capacity.

- [ ] **Step 2: Run the focused Rust test and confirm failure**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils acp::grok_retry::tests -- --nocapture
```

Expected: FAIL because `acp::grok_retry` and its public state-machine types do
not exist.

- [ ] **Step 3: Implement the minimal pure state machine**

Implement the following public surface and keep parsing helpers private:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokRetryAction {
    Pass,
    Consume,
    Rollback { attempt: u32 },
    DropStale { update_kind: &'static str },
}

#[derive(Debug, Default)]
pub struct GrokRetryReconciler {
    active_stream: Option<StreamIdentity>,
    failed: std::collections::VecDeque<FailedWindow>,
    speculative_output: bool,
}

impl GrokRetryReconciler {
    pub fn observe(&mut self, notification: &sacp::UntypedMessage) -> GrokRetryAction {
        if let Some(marker) = parse_retry_marker(notification) {
            return self.observe_retry(marker);
        }
        let Some(update) = parse_standard_update(notification) else {
            return GrokRetryAction::Pass;
        };
        self.observe_standard(update)
    }
}
```

Parse the numeric suffix with `eventId.rsplit_once('-')`, require the primary
`promptId + streamStartMs` proof when available, use the timestamp fallback only
when no failed stream was observable, cap `failed` at 16 entries, and mark an
accepted initial `tool_call` as a commit boundary (`speculative_output=false`).

- [ ] **Step 4: Run reconciler tests**

```powershell
cargo test --features test-utils acp::grok_retry::tests -- --nocapture
```

Expected: all `grok_retry` tests PASS.

- [ ] **Step 5: Commit the state machine**

```powershell
git add src-tauri/src/acp/grok_retry.rs src-tauri/src/acp/mod.rs
git commit -m "fix(acp/grok): classify retried stream updates"
```

---

### Task 2: Rollback Event And Backend State

**Files:**
- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src-tauri/src/acp/session_state.rs`
- Modify: `src-tauri/src/acp/desktop_event_batcher.rs`
- Test: existing inline test modules in those files.

**Interfaces:**
- Consumes: `AcpEvent::TurnAttemptRollback { attempt: u32 }`.
- Produces: `SessionState::has_live_agent_output(&self) -> bool` for the connection loop after rollback.

- [ ] **Step 1: Add failing state and batching tests**

Build a live message containing text, an accepted tool ref, then trailing
thinking/text/plan. Apply rollback and assert only the prefix through the tool
remains:

```rust
state.apply_event(&AcpEvent::TurnAttemptRollback { attempt: 1 });
assert!(matches!(state.live_message.as_ref().unwrap().content.last(),
    Some(LiveContentBlock::ToolCallRef { tool_call_id }) if tool_call_id == "tc-1"));
assert!(state.has_live_agent_output());
```

Add a no-tool case asserting content becomes empty and
`has_live_agent_output()` becomes false. Add a desktop batcher assertion that
rollback is flush-sensitive.

- [ ] **Step 2: Run focused backend tests and confirm failure**

```powershell
cargo test --features test-utils session_state::tests -- --nocapture
cargo test --features test-utils desktop_event_batcher::tests -- --nocapture
```

Expected: FAIL because the event variant and state helper are absent.

- [ ] **Step 3: Implement backend rollback semantics**

Add the event beside `Thinking`:

```rust
TurnAttemptRollback { attempt: u32 },
```

In `SessionState::apply_event`, truncate `live_message.content` to the position
immediately after the final `ToolCallRef`, or to zero when none exists. Preserve
`active_tool_calls`, pending interaction state, usage, and delegation state.
Implement:

```rust
pub(crate) fn has_live_agent_output(&self) -> bool {
    self.live_message
        .as_ref()
        .is_some_and(|live| !live.content.is_empty())
        || !self.active_tool_calls.is_empty()
}
```

Add `TurnAttemptRollback` to `desktop_event_batcher::is_flush_sensitive`.
No lifecycle-critical-lane entry is needed.

- [ ] **Step 4: Run focused backend tests**

Run the two commands from Step 2. Expected: PASS.

- [ ] **Step 5: Commit backend event support**

```powershell
git add src-tauri/src/acp/types.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/desktop_event_batcher.rs
git commit -m "fix(acp): roll back speculative retry output"
```

---

### Task 3: Connect Raw Retry Reconciliation To Every Rust Consumer

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/auto_title/runner.rs`
- Modify: `src-tauri/src/document_translate/runner.rs`
- Modify: `src-tauri/src/chat_channel/session_bridge.rs`
- Modify: `src-tauri/src/chat_channel/session_event_subscriber.rs`
- Modify: `src-tauri/src/chat_channel/event_subscriber.rs`
- Modify: `src-tauri/src/chat_channel/session_commands.rs`
- Test: inline modules in the files above.

**Interfaces:**
- Consumes: `GrokRetryReconciler`, `GrokRetryAction`, and `AcpEvent::TurnAttemptRollback`.
- Produces: one shared `reconcile_grok_retry_dispatch(...) -> bool` path used by the main prompt loop and pre-finalization drain.

- [ ] **Step 1: Add failing collector and connection-path tests**

For auto-title and translation, send old text, rollback, new text, and terminal:

```rust
tx.send(event(1, AcpEvent::ContentDelta { text: "old".into() })).unwrap();
tx.send(event(2, AcpEvent::TurnAttemptRollback { attempt: 1 })).unwrap();
tx.send(event(3, AcpEvent::ContentDelta { text: "accepted".into() })).unwrap();
tx.send(event(4, end_turn())).unwrap();
assert_eq!(collect_result.await.unwrap(), "accepted");
```

Add a connection test queue containing the conversation-800 raw order and
assert the private stream receives rollback, accepted content, and one terminal
without the stale content. Add parity coverage for the pre-finalization drain.

- [ ] **Step 2: Run focused tests and confirm failure**

```powershell
cargo test --features test-utils collect_title_output -- --nocapture
cargo test --features test-utils collect_translate_output -- --nocapture
cargo test --features test-utils grok_retry -- --nocapture
```

Expected: collector assertions fail because rollback is ignored; connection
tests fail because raw retry notifications are not reconciled.

- [ ] **Step 3: Integrate one reconciler per active prompt**

Initialize `GrokRetryReconciler::default()` beside `turn_had_agent_output`.
Before `parse_extension_turn_completed` and `MatchDispatch`, inspect only
`Dispatch::Notification` for Grok:

```rust
match reconciler.observe(notification) {
    GrokRetryAction::Pass => false,
    GrokRetryAction::Consume => true,
    GrokRetryAction::DropStale { update_kind } => {
        tracing::debug!(update_kind, "dropping stale Grok retry update");
        true
    }
    GrokRetryAction::Rollback { attempt } => {
        emit_with_state(state, emitter, AcpEvent::TurnAttemptRollback { attempt }).await;
        *turn_had_agent_output = state.read().await.has_live_agent_output();
        true
    }
}
```

Factor this into one helper and pass the same mutable reconciler into
`drain_ready_in_prompt_updates`. Do not call it from idle or load-replay paths.

- [ ] **Step 4: Update Rust consumers**

Add `TurnAttemptRollback` match arms:

```rust
// auto-title / translation
AcpEvent::TurnAttemptRollback { .. } => buf.clear(),
```

Add `content_checkpoint_len: usize` to `ActiveSession`, initialize it to zero,
set it to `content_buffer.len()` on accepted `ToolCall`, truncate to the bounded
checkpoint on rollback, and reset it on terminal.

- [ ] **Step 5: Run focused Rust tests**

Run the three commands from Step 2. Expected: PASS.

- [ ] **Step 6: Commit Rust integration**

```powershell
git add src-tauri/src/acp/connection.rs src-tauri/src/auto_title/runner.rs src-tauri/src/document_translate/runner.rs src-tauri/src/chat_channel
git commit -m "fix(acp/grok): reconcile retry attempts across consumers"
```

---

### Task 4: Frontend Rollback Projection

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/contexts/acp-connections-context.tsx`
- Modify: `src/lib/acp/live-transcript-projector.ts`
- Test: `src/contexts/acp-connections-context.test.tsx`
- Test: `src/lib/acp/live-transcript-projector.test.ts`
- Test: `src/lib/acp/event-ingestor.test.ts`

**Interfaces:**
- Consumes: `{ type: "turn_attempt_rollback"; attempt: number }`.
- Produces: identical canonical reducer and incremental-projector truncation.

- [ ] **Step 1: Add failing frontend tests**

Add a typed envelope helper and assert rollback is a compaction barrier:

```ts
const events = [
  envelope(1, "c1", { type: "content_delta", text: "old" }),
  envelope(2, "c1", { type: "turn_attempt_rollback", attempt: 1 }),
  envelope(3, "c1", { type: "content_delta", text: "accepted" }),
]
expect(compactAdjacentDeltas(events).map((event) => event.type)).toEqual([
  "content_delta",
  "turn_attempt_rollback",
  "content_delta",
])
```

For both the connection reducer and projector, assert no-tool rollback clears
the speculative tail, while a tool-boundary fixture retains the tool and prior
text but removes later thinking/text/plan.

- [ ] **Step 2: Run targeted Vitest files and confirm failure**

```powershell
pnpm test -- src/lib/acp/event-ingestor.test.ts src/lib/acp/live-transcript-projector.test.ts src/contexts/acp-connections-context.test.tsx
```

Expected: FAIL because the event is absent from `AcpEvent` and reducers.

- [ ] **Step 3: Implement typed event and canonical reducer**

Add to `AcpEvent`:

```ts
| { type: "turn_attempt_rollback"; attempt: number }
```

Map it to a `TURN_ATTEMPT_ROLLBACK` frame action. Implement a helper that finds
the final `tool_call` block and slices through it, or returns an empty content
array. Keep the same `LiveMessage` shell and status.

- [ ] **Step 4: Implement incremental projector rollback**

Add `rollbackAttempt(snapshot)` that finds the final `tool` or
`generated-image` segment, retains segment IDs through it, deletes later
segments from a cloned map, and preserves the tools map. Apply it in both
`applyLiveTranscriptEvents` and `applyEventsToCanonicalLiveMessage`.

No special ingestor implementation is required: the new non-delta event is a
natural barrier; retain the explicit regression test.

- [ ] **Step 5: Run targeted frontend tests**

Run the command from Step 2. Expected: PASS.

- [ ] **Step 6: Commit frontend projection support**

```powershell
git add src/lib/types.ts src/contexts/acp-connections-context.tsx src/lib/acp
git commit -m "fix(chat): roll back retried Grok live output"
```

---

### Task 5: Full Verification And Cleanup

**Files:**
- Modify only files needed to resolve failures caused by the new exhaustive event variant; do not perform unrelated refactors.

**Interfaces:**
- Consumes: all preceding task outputs.
- Produces: a repository-clean, fully tested implementation matching the design spec.

- [ ] **Step 1: Format and run focused suites again**

```powershell
cd src-tauri
cargo fmt -- --check
cargo test --features test-utils grok_retry -- --nocapture
cargo test --features test-utils collect_title_output -- --nocapture
cargo test --features test-utils collect_translate_output -- --nocapture
cd ..
pnpm test -- src/lib/acp/event-ingestor.test.ts src/lib/acp/live-transcript-projector.test.ts src/contexts/acp-connections-context.test.tsx
```

Expected: all PASS and formatting clean.

- [ ] **Step 2: Run repository-required backend checks**

From `src-tauri/`:

```powershell
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo check --no-default-features --bin codeg-mcp
```

Expected: all commands exit 0.

- [ ] **Step 3: Run repository-required frontend checks**

From the repository root:

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: all commands exit 0.

- [ ] **Step 4: Verify scope and commit final adjustments**

```powershell
git diff --check
git status --short
git diff --stat
```

Expected: only planned implementation/test files are changed. Commit any
verification-driven adjustment with:

```powershell
git add src-tauri/src/acp/grok_retry.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/types.rs src-tauri/src/acp/session_state.rs src-tauri/src/acp/desktop_event_batcher.rs src-tauri/src/acp/connection.rs src-tauri/src/auto_title/runner.rs src-tauri/src/document_translate/runner.rs src-tauri/src/chat_channel/session_bridge.rs src-tauri/src/chat_channel/session_event_subscriber.rs src-tauri/src/chat_channel/event_subscriber.rs src-tauri/src/chat_channel/session_commands.rs src/lib/types.ts src/lib/acp/event-ingestor.test.ts src/lib/acp/live-transcript-projector.ts src/lib/acp/live-transcript-projector.test.ts src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
git commit -m "test(acp/grok): cover retry stream reconciliation"
```
