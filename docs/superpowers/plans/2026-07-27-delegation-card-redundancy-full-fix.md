# Delegation Card Redundancy Full-Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate false continuation wait timeouts and render one continuous delegation card per work unit across dispatch, continuation, status polling, checkpoint re-entry, and historical reload.

**Architecture:** Keep durable events unchanged. The Rust listener treats an explicit cancel cause as cancellation and a cause-less watch close as a non-blocking Broker snapshot; the frontend builds a pure work-unit projection from adapted transcript parts, then folds all run observations into sticky runtime chrome. Live redundant companion tools use the same identity index, while exact interruption markers are filtered only for delegated child rendering.

**Tech Stack:** Rust 2021, Tokio, TypeScript strict, React 19, Next.js 16 static export, Zustand, Vitest, Testing Library, next-intl.

## Global Constraints

- Design: `docs/superpowers/specs/2026-07-27-delegation-card-redundancy-full-fix-design.md`.
- Preserve raw transcript events and `delegation_task_runs`; no schema migration or data rewrite.
- Do not change Broker Join ownership, continuation worker lifetime, or the 600-second checkpoint.
- Explicit `AutoTimeout` remains `tool_stalled_timeout`; explicit `UserStop` remains `user_cancelled`.
- Closed watch sender plus no `CancelCause` is release, not cancellation.
- Sticky orphan timeout is exactly `900_000` ms.
- Never invent a zero tool count when runtime stats are absent.
- Unmapped status rows remain visible; uncertain identities stay separate.
- The redacted workflow graph DTO must not expose `work_unit_key`.
- Reuse existing translations; add no locale keys unless implementation proves necessary.
- Prettier: no semicolons, trailing commas, 2-space indent, 80-column target.
- Stage exact task-owned paths only; never use `git add -A`.

## File Map

| Path | Responsibility |
|------|----------------|
| `src-tauri/src/acp/delegation/listener.rs` | Resolve post-suspension wait release from explicit cause or Broker snapshot |
| `src/lib/delegation-card.ts` | Parse work-unit, target, replacement, task, and continuation identities from structured tool input/output |
| `src/lib/delegation-work-unit.ts` | Generic union/grouping of dispatch runs and stable identity index |
| `src/lib/delegation-work-unit.test.ts` | Identity priority, continuation/replacement linkage, parallel isolation |
| `src/lib/delegation-transcript-projection.ts` | Two-pass session projection and live companion-tool fold predicate |
| `src/lib/delegation-transcript-projection.test.ts` | 2075-like history, mixed/unmapped polls, structural sharing |
| `src/lib/adapters/ai-elements-adapter.ts` | Adapted `delegation-work-unit` part and residual status-row allowlist types |
| `src/lib/delegation-status.ts` | Filter grouped status rows by residual task ids without rewriting JSON |
| `src/components/message/content-parts-renderer.tsx` | Render canonical work-unit parts with stable React keys |
| `src/components/message/message-list-view.tsx` | Apply history projection and pass live identity/filter context |
| `src/components/message/live-transcript-row.tsx` | Suppress live companion tools already represented by a canonical unit |
| `src/lib/delegation-work-unit-runtime.ts` | Pure elapsed/tool/edit/touched-file fold and sticky phase/orphan rules |
| `src/lib/delegation-work-unit-runtime.test.ts` | Runtime aggregation and terminal/recoverable phase tests |
| `src/hooks/use-delegation-card-model.ts` | Merge work-unit runtime into the current authoritative card model |
| `src/hooks/use-delegated-sub-session.ts` | Resolve the newest live binding by shared child identity |
| `src/contexts/delegation-context.tsx` | Deterministic newest binding lookup for one child conversation |
| `src/components/message/delegated-sub-thread.tsx` | Accept canonical key and all correlated run sources |
| `src/components/message/delegation-card-chrome.tsx` | Prepend localized generating segment |
| `src/components/chat/sub-agent-overlay.tsx` | Group overlay rows by the shared work-unit identity rules |
| `src/lib/delegation-conversation-interrupted.ts` | Exact interruption-marker detector/filter |
| `src/components/conversations/conversation-session-surface.tsx` | Pass delegated-child identity into transcript rendering |

---

### Task 1: Cause-Less Continuation Release Returns a Broker Snapshot

**Files:**
- Modify: `src-tauri/src/acp/delegation/listener.rs:1172`
- Test: `src-tauri/src/acp/delegation/listener.rs` module tests near the continuation wait tests

**Interfaces:**
- Consumes: `DelegationBroker::get_tasks_status`, `StatusWait::Snapshot`, `CancelCause`
- Produces: private async `continuation_release_batch(...) -> DelegationStatusBatch`

- [ ] **Step 1: Write failing listener tests for cause-less release**

Add tests using `running_task_fixture()` / `make_listener_with_wait_cancel()` and the private helper:

```rust
#[tokio::test]
async fn continuation_causeless_release_returns_running_snapshot() {
    let (broker, tokens, task_id) = running_task_fixture().await;
    let listener = make_listener_with_wait_cancel(
        broker,
        tokens,
        Some(1),
        WaitCancelRegistry::new_shared(),
    );

    let batch = listener
        .continuation_release_batch(
            "parent-conn",
            1,
            std::slice::from_ref(&task_id),
            None,
        )
        .await;

    assert_eq!(batch.tasks[0].status, TaskStatus::Running);
    assert_eq!(batch.tasks[0].error_code, None);
    assert_eq!(batch.wake_reason, Some(DelegationWakeReason::Unavailable));
}

#[tokio::test]
async fn continuation_causeless_release_returns_completed_snapshot() {
    let (broker, tokens, task_id) = running_task_fixture().await;
    complete_running_task(&broker, &task_id).await;
    let listener = make_listener_with_wait_cancel(
        broker,
        tokens,
        Some(1),
        WaitCancelRegistry::new_shared(),
    );

    let batch = listener
        .continuation_release_batch(
            "parent-conn",
            1,
            std::slice::from_ref(&task_id),
            None,
        )
        .await;

    assert_eq!(batch.tasks[0].status, TaskStatus::Completed);
    assert_ne!(
        batch.tasks[0].error_code.as_deref(),
        Some("tool_stalled_timeout")
    );
}
```

Add explicit cause regression assertions in the same test block:

```rust
let timeout = listener
    .continuation_release_batch(
        "parent-conn",
        1,
        std::slice::from_ref(&task_id),
        Some(CancelCause::AutoTimeout),
    )
    .await;
assert_eq!(
    timeout.tasks[0].error_code.as_deref(),
    Some("tool_stalled_timeout")
);

let stopped = listener
    .continuation_release_batch(
        "parent-conn",
        1,
        std::slice::from_ref(&task_id),
        Some(CancelCause::UserStop),
    )
    .await;
assert_eq!(stopped.tasks[0].error_code.as_deref(), Some("user_cancelled"));
```

- [ ] **Step 2: Run the new Rust tests and verify failure**

Run from `src-tauri/`:

```powershell
cargo test --features test-utils continuation_causeless_release -- --nocapture
```

Expected: compilation fails because `continuation_release_batch` does not exist.

- [ ] **Step 3: Implement the release helper and use it after suspension**

Add a private listener method with this contract:

```rust
async fn continuation_release_batch(
    &self,
    parent_connection_id: &str,
    parent_conversation_id: i32,
    canonical_task_ids: &[String],
    cause: Option<crate::acp::tool_watchdog::CancelCause>,
) -> DelegationStatusBatch {
    if let Some(cause) = cause {
        return DelegationStatusBatch::joined(
            canonical_task_ids
                .iter()
                .map(|id| wait_cancel_report(id, cause))
                .collect(),
            DelegationWakeReason::Unavailable,
            Vec::new(),
        );
    }

    let tasks = self
        .broker
        .get_tasks_status(
            parent_connection_id,
            Some(parent_conversation_id),
            canonical_task_ids,
            StatusWait::Snapshot,
        )
        .await;
    DelegationStatusBatch::joined(
        tasks,
        DelegationWakeReason::Unavailable,
        Vec::new(),
    )
}
```

At the post-suspension channel loop, read `cancel_cause_of(&cancel_rx)` without
`unwrap_or`, deregister idempotently, and return this helper's batch. Keep the
pre-suspension explicit-cancel branches unchanged.

- [ ] **Step 4: Run focused continuation/wait tests**

```powershell
cargo test --features test-utils continuation_causeless_release -- --nocapture
cargo test --features test-utils continuation_wait_cancel_after_suspend_control_preserves_waiting -- --nocapture
cargo test --features test-utils legacy_indefinite_registers_canonical_task_ids_and_tool_id -- --nocapture
```

Expected: all pass; explicit timeout assertions remain unchanged.

- [ ] **Step 5: Commit the backend contract fix**

```powershell
git add src-tauri/src/acp/delegation/listener.rs
git commit -m "fix(delegation): preserve snapshot on continuation release"
```

---

### Task 2: Structured Work-Unit Identity and Generic Run Grouping

**Files:**
- Modify: `src/lib/delegation-card.ts`
- Modify: `src/lib/delegation-card.test.ts`
- Create: `src/lib/delegation-work-unit.ts`
- Create: `src/lib/delegation-work-unit.test.ts`

**Interfaces:**
- Produces: extended `ParsedInput`, `parseDelegateRunIdentity`, `groupDelegationRuns`, `DelegationIdentityIndex`
- Consumes: existing envelope parsers (`peelMcpResultEnvelope`, `parseDelegateTaskId`, `parseToolOutput`)

- [ ] **Step 1: Add failing structured parser tests**

Extend `ParsedInput` expectations with these fields:

```ts
expect(
  parseInput(
    JSON.stringify({
      task: "continue",
      task_id: "run-1",
      work_unit_key: "task|1|implementer|grok|none",
      replaces_task_id: "run-0",
    })
  )
).toMatchObject({
  targetTaskId: "run-1",
  workUnitKey: "task|1|implementer|grok|none",
  replacesTaskId: "run-0",
})
```

Add an output-envelope case:

```ts
expect(
  parseDelegateRunIdentity({
    parentConversationId: 2075,
    parentToolUseId: "tool-2",
    input: JSON.stringify({ task: "continue", task_id: "run-1" }),
    output: JSON.stringify({
      structuredContent: {
        task_id: "run-2",
        continued_from_task_id: "run-1",
        child_conversation_id: 3001,
        status: "running",
      },
    }),
    errorText: null,
    meta: null,
  })
).toMatchObject({
  taskId: "run-2",
  childConversationId: 3001,
  linkedTaskIds: ["run-1"],
})
```

- [ ] **Step 2: Add failing union/group tests**

Create `delegation-work-unit.test.ts` with:

```ts
it("unions initial and continued runs by work key and task link", () => {
  const grouped = groupDelegationRuns([
    run("tool-1", "run-1", "unit-a", null, []),
    run("tool-2", "run-2", "unit-a", 3001, ["run-1"]),
  ])

  expect(grouped.units).toHaveLength(1)
  expect(grouped.units[0].runs.map((entry) => entry.value)).toEqual([
    "tool-1",
    "tool-2",
  ])
  expect(grouped.index.taskToUnitKey.get("run-2")).toBe(grouped.units[0].key)
})

it("keeps equal task ids from different parents isolated", () => {
  const grouped = groupDelegationRuns([
    run("a", "same", null, 10, [], 1),
    run("b", "same", null, 10, [], 2),
  ])
  expect(grouped.units).toHaveLength(2)
})
```

- [ ] **Step 3: Run parser/group tests and verify failure**

```powershell
pnpm exec vitest run src/lib/delegation-card.test.ts src/lib/delegation-work-unit.test.ts
```

Expected: new fields/functions are missing.

- [ ] **Step 4: Implement identity parsing and grouping**

Use these public contracts:

```ts
export type ParsedInput = {
  agentType: AgentType | null
  profileLabel: string | null
  task: string | null
  workingDir: string | null
  workUnitKey: string | null
  targetTaskId: string | null
  replacesTaskId: string | null
}

export interface DelegationRunIdentityInput {
  parentConversationId: number
  parentToolUseId: string
  input?: string | null
  output?: string | null
  errorText?: string | null
  meta?: Record<string, unknown> | null
}

export interface DelegationRunIdentity {
  parentConversationId: number
  parentToolUseId: string
  workUnitKey: string | null
  taskId: string | null
  childConversationId: number | null
  linkedTaskIds: string[]
}

export function parseDelegateRunIdentity(
  input: DelegationRunIdentityInput
): DelegationRunIdentity
```

`parseInput` must continue peeling existing wrapper keys and double-encoded JSON.
Read `work_unit_key`, `task_id`, and `replaces_task_id` only from structured
objects; do not scan arbitrary prompt text.

In `delegation-work-unit.ts`, implement union-find over identity tokens scoped by
parent conversation:

```ts
export interface DelegationRunRecord<T> {
  value: T
  identity: DelegationRunIdentity
}

export interface DelegationIdentityIndex {
  taskToUnitKey: ReadonlyMap<string, string>
  workUnitToUnitKey: ReadonlyMap<string, string>
  knownTaskIds: ReadonlySet<string>
  knownWorkUnitKeys: ReadonlySet<string>
}

export function groupDelegationRuns<T>(
  records: readonly DelegationRunRecord<T>[]
): {
  units: Array<{ key: string; runs: DelegationRunRecord<T>[] }>
  index: DelegationIdentityIndex
}
```

Union records sharing explicit work key, parent+child token, own task token, or
linked task token. Choose the display key by explicit work key, then
parent+child, then task id, then parent tool-use id. Preserve first-appearance
unit and run order.

- [ ] **Step 5: Run tests and commit**

```powershell
pnpm exec vitest run src/lib/delegation-card.test.ts src/lib/delegation-work-unit.test.ts
git add src/lib/delegation-card.ts src/lib/delegation-card.test.ts src/lib/delegation-work-unit.ts src/lib/delegation-work-unit.test.ts
git commit -m "feat(ui): resolve delegation work-unit identities"
```

---

### Task 3: Pure Session Transcript Projection

**Files:**
- Modify: `src/lib/adapters/ai-elements-adapter.ts`
- Modify: `src/lib/adapters/ai-elements-adapter.test.ts`
- Modify: `src/lib/delegation-status.ts`
- Modify: `src/lib/delegation-status.test.ts`
- Create: `src/lib/delegation-transcript-projection.ts`
- Create: `src/lib/delegation-transcript-projection.test.ts`

**Interfaces:**
- Consumes: `groupDelegationRuns`, `parseTaskIds`, `parseStatusReports`, normalized tool names
- Produces: `AdaptedDelegationWorkUnitPart`, `projectDelegationTranscript`, `shouldFoldLiveDelegationTool`

- [ ] **Step 1: Add adapted part and residual row failing tests**

Define the intended adapter shapes in tests:

```ts
const unit: AdaptedContentPart = {
  type: "delegation-work-unit",
  key: "wu:unit-a",
  sources: [delegatePart("tool-1", "run-1")],
  explicitUserCancel: false,
}

const residual: AdaptedContentPart = {
  type: "delegation-status-group",
  polls: [batchPoll(["run-1", "orphan"])],
  visibleTaskIds: ["orphan"],
}
```

Extend `buildDelegationTaskRows` tests:

```ts
expect(
  buildDelegationTaskRows([batchPoll(["run-1", "orphan"])], new Set(["orphan"]))
    .map((row) => row.taskId)
).toEqual(["orphan"])
```

- [ ] **Step 2: Write the 2075-like projection failure test**

Create messages containing:

```ts
const messages = [
  assistant(delegate("tool-1", "run-1", "unit-a")),
  assistant(status("poll-1", ["run-1"], "running")),
  assistant(text("checkpoint explanation")),
  assistant(continuation("tool-2", "run-1", "run-2", "unit-a")),
  assistant(status("poll-2", ["run-2"], "running")),
  assistant(text("still working")),
  assistant(status("poll-3", ["run-2", "unknown-run"], "completed")),
]

const projected = projectDelegationTranscript(messages, 2075)
const parts = projected.messages.flatMap((message) => message.content)

expect(parts.filter((part) => part.type === "delegation-work-unit")).toHaveLength(1)
expect(
  parts
    .filter((part) => part.type === "text")
    .map((part) => part.text)
).toEqual(["checkpoint explanation", "still working"])
expect(statusTaskIds(parts)).toEqual(["unknown-run"])
expect(messages.flatMap((message) => message.content)).not.toContainEqual(
  expect.objectContaining({ type: "delegation-work-unit" })
)
```

Add parallel units, out-of-order linkage, all-unmapped, and mixed mapped/unmapped
batch cases. Assert unchanged messages retain reference equality.

- [ ] **Step 3: Run projection tests and verify failure**

```powershell
pnpm exec vitest run src/lib/delegation-status.test.ts src/lib/delegation-transcript-projection.test.ts
```

Expected: part type, row filter, and projector are absent.

- [ ] **Step 4: Implement the two-pass projection**

Add adapter types:

```ts
export type AdaptedDelegationWorkUnitPart = {
  type: "delegation-work-unit"
  key: string
  sources: AdaptedToolCallPart[]
  explicitUserCancel: boolean
}

// On delegation-status-group:
visibleTaskIds?: string[]
```

Use this projector contract:

```ts
export function projectDelegationTranscript(
  messages: readonly AdaptedMessage[],
  parentConversationId: number
): {
  messages: AdaptedMessage[]
  identityIndex: DelegationIdentityIndex
}
```

First flatten dispatch/continue parts with stable locations and group them. Then
rewrite only affected message content:

- replace each unit's first dispatch part with `delegation-work-unit`;
- remove later dispatch parts in that unit;
- remove fully mapped status groups;
- set `visibleTaskIds` on a mixed residual group;
- retain wholly unmapped groups and unattributed rows;
- observe mapped `cancel_delegation` calls without removing their audit card,
  and set `explicitUserCancel` on the affected canonical unit when the cancel
  call returned successfully or its report is `canceled`;
- recurse through `goal-run` items defensively; agent-like calls should remain
  top-level under the normal adapter path.

Add live predicate:

```ts
export function shouldFoldLiveDelegationTool(
  part: AdaptedToolCallPart,
  index: DelegationIdentityIndex,
  parentConversationId: number
): boolean
```

Return true for a status call only when every non-empty requested/report task id
is known. Return true for continuation/replacement dispatch only when its work
key or linked target task is known. Never fold an identity-free initial
`delegate_to_agent`.

- [ ] **Step 5: Run focused tests and commit**

```powershell
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts src/lib/delegation-status.test.ts src/lib/delegation-transcript-projection.test.ts
git add src/lib/adapters/ai-elements-adapter.ts src/lib/adapters/ai-elements-adapter.test.ts src/lib/delegation-status.ts src/lib/delegation-status.test.ts src/lib/delegation-transcript-projection.ts src/lib/delegation-transcript-projection.test.ts
git commit -m "feat(ui): project delegation transcript by work unit"
```

---

### Task 4: Wire Canonical History Cards and Suppress Redundant Live Tools

**Files:**
- Modify: `src/components/message/content-parts-renderer.tsx`
- Modify: `src/components/message/content-parts-renderer.test.tsx`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/message/delegation-status-group-card.tsx`
- Modify: `src/components/message/delegation-status-group-card.test.tsx`

**Interfaces:**
- Consumes: `projectDelegationTranscript`, `shouldFoldLiveDelegationTool`, `AdaptedDelegationWorkUnitPart`
- Produces: one stable `DelegatedSubThread` render per unit and identity-aware live footer filtering

- [ ] **Step 1: Write renderer and message-list failing tests**

Add a renderer test asserting a work-unit part produces one card and passes the
latest source plus every source:

```tsx
render(
  <ContentPartsRenderer
    role="assistant"
    parentConversationId={2075}
    parts={[
      {
        type: "delegation-work-unit",
        key: "wu:unit-a",
        sources: [delegatePart("tool-1"), delegatePart("tool-2")],
        explicitUserCancel: false,
      },
    ]}
  />
)
expect(screen.getAllByTestId("delegated-sub-thread")).toHaveLength(1)
expect(screen.getByTestId("delegated-sub-thread")).toHaveAttribute(
  "data-work-unit-key",
  "wu:unit-a"
)
```

In `message-list-view.test.tsx`, install persisted turns equivalent to the Task 3
sequence and assert one delegated card, preserved interleaved text, and one
residual unknown status row.

- [ ] **Step 2: Write live-footer failing tests**

Pass a historical identity index containing `run-1`, publish a live
`continue_delegation` targeting `run-1` plus an unrelated shell tool, and assert:

```tsx
expect(screen.queryByText(/continue_delegation/i)).not.toBeInTheDocument()
expect(screen.getByText(/shell/i)).toBeInTheDocument()
```

Add an initial identity-free delegation case and assert it remains visible.

- [ ] **Step 3: Run component tests and verify failure**

```powershell
pnpm exec vitest run src/components/message/content-parts-renderer.test.tsx src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/delegation-status-group-card.test.tsx
```

- [ ] **Step 4: Wire projection and stable canonical rendering**

In `MessageListView`'s existing adaptation memo, call
`projectDelegationTranscript(allAdapted, conversationId)` before creating raw
thread items. Preserve raw non-streaming adapted messages for plan extraction.
Return the projection's identity index alongside `threadItems`.

In `ContentPartsRenderer`, render the new part with:

```tsx
if (part.type === "delegation-work-unit") {
  const latest = part.sources[part.sources.length - 1]
  return (
    <DelegatedSubThread
      key={`dwu-${part.key}`}
      parentToolUseId={latest.toolCallId}
      parentConversationId={parentConversationId}
      input={latest.input ?? null}
      output={latest.output ?? null}
      errorText={latest.errorText ?? null}
      state={latest.state}
      meta={latest.meta ?? null}
      workUnitKey={part.key}
      explicitUserCancel={part.explicitUserCancel}
      workUnitSources={part.sources.map((source) => ({
        parentToolUseId: source.toolCallId,
        parentConversationId,
        input: source.input ?? null,
        output: source.output ?? null,
        errorText: source.errorText ?? null,
        state: source.state,
        meta: source.meta ?? null,
      }))}
    />
  )
}
```

Update status-group rendering to pass `visibleTaskIds` into
`buildDelegationTaskRows`.

- [ ] **Step 5: Filter redundant incremental-live companion tools**

Add `delegationIdentityIndex?: DelegationIdentityIndex | null` to
`LiveTranscriptRowProps`. Pass the historical index from `MessageListView`.
While building footer items, adapt a tool and skip its segment only when
`shouldFoldLiveDelegationTool` returns true. A mixed status batch or unknown id
stays visible until it settles into history.

- [ ] **Step 6: Run tests and commit**

```powershell
pnpm exec vitest run src/components/message/content-parts-renderer.test.tsx src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/delegation-status-group-card.test.tsx
git add src/components/message/content-parts-renderer.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/message/delegation-status-group-card.tsx src/components/message/delegation-status-group-card.test.tsx
git commit -m "feat(ui): render one delegation card per work unit"
```

---

### Task 5: Pure Sticky Runtime Aggregation

**Files:**
- Create: `src/lib/delegation-work-unit-runtime.ts`
- Create: `src/lib/delegation-work-unit-runtime.test.ts`

**Interfaces:**
- Consumes: normalized per-run observations and `DelegationRuntimeStats`
- Produces: `buildDelegationWorkUnitRuntime`, `STICKY_ORPHAN_TIMEOUT_MS`, combined runtime stats

- [ ] **Step 1: Write failing aggregation and phase tests**

Use the locked contracts:

```ts
export type WorkUnitRunObservation = {
  identity: string
  taskId: string | null
  lifecycleStatus: "running" | "ok" | "err"
  errorCode: string | null
  startedAt: string | null
  finishedAt: string | null
  lastAgentActivityAt: string | null
  runtimeStats: DelegationRuntimeStats | null
  current: boolean
}

export type WorkUnitRuntimeProjection = {
  activeSticky: boolean
  startedAt: string | null
  finishedAt: string | null
  elapsedMs: number | null
  runtimeStats: DelegationRuntimeStats | null
  toolCallCount: number | null
  lifecycleOverride: "running" | null
  statusOverride: "running" | null
  suppressErrorCode: boolean
}
```

Test per-run peak folding:

```ts
const result = buildDelegationWorkUnitRuntime({
  runs: [
    observed("run-1", 5, "2026-07-27T00:00:00Z", "err", "parent_turn_failed"),
    observed("run-1", 3, "2026-07-27T00:00:00Z", "err", "parent_turn_failed"),
    observed("run-2", 2, "2026-07-27T00:05:00Z", "running", null, true),
  ],
  nowMs: Date.parse("2026-07-27T00:06:00Z"),
  hasLiveBinding: true,
  explicitUserCancel: false,
})
expect(result.toolCallCount).toBe(7)
expect(result.startedAt).toBe("2026-07-27T00:00:00Z")
expect(result.activeSticky).toBe(true)
expect(result.elapsedMs).toBe(360_000)
```

Add tests for:

- recoverable `parent_turn_failed` remains active before 15 minutes;
- the same record becomes terminal at `900_000` ms without live/recovery evidence;
- missing persisted orphan time never starts a fresh historical sticky window;
- completed, business failed, and `user_cancelled` are terminal;
- no observed stats yields `toolCallCount: null`, not zero;
- touched files union by path and edit/addition/deletion totals fold by run peak.

- [ ] **Step 2: Run the runtime tests and verify failure**

```powershell
pnpm exec vitest run src/lib/delegation-work-unit-runtime.test.ts
```

- [ ] **Step 3: Implement the pure runtime projector**

Define:

```ts
export const STICKY_ORPHAN_TIMEOUT_MS = 900_000

export function buildDelegationWorkUnitRuntime(input: {
  runs: readonly WorkUnitRunObservation[]
  nowMs: number
  hasLiveBinding: boolean
  explicitUserCancel: boolean
}): WorkUnitRuntimeProjection
```

Deduplicate by `taskId ?? identity`. For each identity keep the observation with
the highest `tool_call_count` and highest edit count, while lifecycle/current
fields come from the newest current observation. Sum peaks across distinct run
identities. Use earliest valid start as anchor. Use the latest valid persisted
`finishedAt`, runtime `finished_at`, `lastAgentActivityAt`, or start as the orphan
reference. Invalid dates contribute no elapsed/orphan evidence.

Recoverable codes are a read-only set containing `parent_turn_failed`,
`join_abandoned`, and `parent_disconnected`; accept `parent_canceled` only when
`explicitUserCancel` is false. Never recover `user_cancelled` or a non-listed
business error.

- [ ] **Step 4: Run tests and commit**

```powershell
pnpm exec vitest run src/lib/delegation-work-unit-runtime.test.ts
git add src/lib/delegation-work-unit-runtime.ts src/lib/delegation-work-unit-runtime.test.ts
git commit -m "feat(ui): aggregate sticky delegation runtime"
```

---

### Task 6: Merge Sticky Runtime Into Inline and Overlay Cards

**Files:**
- Modify: `src/hooks/use-delegation-card-model.ts`
- Modify: `src/hooks/use-delegation-card-model.test.ts`
- Modify: `src/hooks/use-delegation-card-model-hook.test.tsx`
- Modify: `src/hooks/use-delegated-sub-session.ts`
- Modify: `src/contexts/delegation-context.tsx`
- Modify: `src/components/message/delegated-sub-thread.tsx`
- Modify: `src/components/message/delegated-sub-thread.test.tsx`
- Modify: `src/components/message/delegation-card-chrome.tsx`
- Modify: `src/components/message/delegation-card-chrome.test.tsx`
- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/chat/sub-agent-overlay.test.tsx`

**Interfaces:**
- Consumes: work-unit sources and `buildDelegationWorkUnitRuntime`
- Produces: `DelegationCardModel.showGeneratingSegment`, stable `stickyKey`, aggregated runtime model

- [ ] **Step 1: Add failing model tests**

Extend `DelegationCardModel` with:

```ts
showGeneratingSegment: boolean
stickyKey: string | null
```

Add pure merge tests that pass two sources through a new exported helper:

```ts
const merged = mergeDelegationWorkUnitModel({
  model: completedOrRunningBaseModel,
  sources: [sourceWithStats("run-1", 5), sourceWithStats("run-2", 2)],
  stickyKey: "wu:unit-a",
  nowMs: Date.parse("2026-07-27T00:06:00Z"),
  hasLiveBinding: true,
  explicitUserCancel: false,
})
expect(merged.toolCallCount).toBe(7)
expect(merged.showGeneratingSegment).toBe(true)
expect(merged.stickyKey).toBe("wu:unit-a")
```

Add a recoverable-error case expecting lifecycle/status `running` and cleared
display error code, plus completed/user-canceled cases expecting no generating
segment.

- [ ] **Step 2: Add newest-child-binding failing test**

Build a Delegation context map containing an older completed binding and a newer
running binding for the same `childConversationId`. Assert
`findByChildConversationId` returns the newer running/later-started binding.
In the hook test, call `useDelegatedSubSession` with an old tool id plus fallback
child id and assert the returned binding is the new run.

- [ ] **Step 3: Add chrome and card failing tests**

Render `DelegationCardChrome` with `showGeneratingSegment`:

```tsx
expect(screen.getByTestId("delegation-operational")).toHaveTextContent(
  /streaming|generating|生成中/i
)
expect(screen.getByTestId("delegation-operational").textContent).toMatch(/\|/)
```

Render `DelegatedSubThread` with two `workUnitSources` and assert stable
`data-work-unit-key`, aggregated tool count, and one card.

- [ ] **Step 4: Run focused tests and verify failure**

```powershell
pnpm exec vitest run src/hooks/use-delegation-card-model.test.ts src/hooks/use-delegation-card-model-hook.test.tsx src/components/message/delegated-sub-thread.test.tsx src/components/message/delegation-card-chrome.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

- [ ] **Step 5: Implement model aggregation and ticker eligibility**

Add:

```ts
export function mergeDelegationWorkUnitModel(input: {
  model: DelegationCardModel
  sources: readonly DelegationCardSource[]
  stickyKey: string | null
  nowMs: number
  hasLiveBinding: boolean
  explicitUserCancel: boolean
}): DelegationCardModel
```

Map every source's parsed meta/run identity into `WorkUnitRunObservation`, add
the current authoritative model observation, then merge the pure runtime output.
Rebuild `editRollup` from combined runtime stats. When sticky-active, set
`lifecycleStatus: "running"`, convert an error badge to `running`, clear the
recoverable display error code, and set `showGeneratingSegment: true`.

`useDelegationCardModel` accepts optional options:

```ts
export function useDelegationCardModel(
  source: DelegationCardSource,
  options?: {
    workUnitSources?: readonly DelegationCardSource[]
    stickyKey?: string | null
    explicitUserCancel?: boolean
  }
): DelegationCardModel
```

Build a preview merged model before ticker subscription so a recoverable sticky
state continues the existing one-second ticker. Rebuild inside the final memo
when `tickerVersion` changes.

- [ ] **Step 6: Implement newest live child binding and card wiring**

Make `findByChildConversationId` choose a running binding over terminal ones,
then the greatest valid `startedAt`; use parent tool-use id as deterministic
tie-breaker. In `useDelegatedSubSession`, fall back to this lookup when the direct
tool-id binding is absent or older than the same-child candidate.

Add optional `workUnitKey` / `workUnitSources` props to `DelegatedSubThread` and
pass them to the model hook. Pass the canonical part's
`explicitUserCancel` flag as well. Add `data-work-unit-key` on its root. For
overlay-only rows, derive the same flag from a current `user_cancelled` error;
`parent_canceled` remains recoverable only when that explicit flag is false.

In `DelegationCardChrome`, prepend `tLive("streaming")` to operational segments
when `showGeneratingSegment` is true. Pass the flag from inline and overlay
cards.

Update overlay grouping to call `parseDelegateRunIdentity` /
`groupDelegationRuns` so explicit work key wins over child-only grouping. Feed
all unit sources to the latest row's model hook and keep the existing run-count
label.

- [ ] **Step 7: Run tests and commit**

```powershell
pnpm exec vitest run src/hooks/use-delegation-card-model.test.ts src/hooks/use-delegation-card-model-hook.test.tsx src/components/message/delegated-sub-thread.test.tsx src/components/message/delegation-card-chrome.test.tsx src/components/chat/sub-agent-overlay.test.tsx
git add src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts src/hooks/use-delegation-card-model-hook.test.tsx src/hooks/use-delegated-sub-session.ts src/contexts/delegation-context.tsx src/components/message/delegated-sub-thread.tsx src/components/message/delegated-sub-thread.test.tsx src/components/message/delegation-card-chrome.tsx src/components/message/delegation-card-chrome.test.tsx src/components/chat/sub-agent-overlay.tsx src/components/chat/sub-agent-overlay.test.tsx
git commit -m "feat(ui): keep delegation runtime sticky across runs"
```

---

### Task 7: Hide Exact Interruption Markers Only in Delegated Children

**Files:**
- Create: `src/lib/delegation-conversation-interrupted.ts`
- Create: `src/lib/delegation-conversation-interrupted.test.ts`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/components/message/message-list-view.test.tsx`
- Modify: `src/components/message/live-transcript-row.tsx`
- Modify: `src/components/message/live-transcript-row.test.tsx`
- Modify: `src/components/conversations/conversation-session-surface.tsx`

**Interfaces:**
- Produces: `isConversationInterruptedAgentText`, `filterDelegatedInterruptParts`
- Consumes: `isDelegatedChild` from `DbConversationDetail.summary.parent_id`

- [ ] **Step 1: Write exact matcher failure tests**

```ts
expect(isConversationInterruptedAgentText("*Conversation interrupted*")).toBe(true)
expect(isConversationInterruptedAgentText(" **Conversation interrupted** \n")).toBe(true)
expect(isConversationInterruptedAgentText("Conversation interrupted")).toBe(true)
expect(
  isConversationInterruptedAgentText("*Conversation interrupted*\nMore detail")
).toBe(false)
expect(isConversationInterruptedAgentText("Conversation was interrupted")).toBe(false)
```

Test that the filter removes only assistant text parts when delegated and returns
the original array for standalone conversations.

- [ ] **Step 2: Add historical and live rendering failure tests**

In `message-list-view.test.tsx`, render the exact marker with
`isDelegatedChild={true}` and assert it is absent; rerender with false and assert
it remains. In `live-transcript-row.test.tsx`, publish an exact live text segment
and repeat the delegated/standalone assertions.

- [ ] **Step 3: Run tests and verify failure**

```powershell
pnpm exec vitest run src/lib/delegation-conversation-interrupted.test.ts src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.test.tsx
```

- [ ] **Step 4: Implement display-only filtering**

Use trim plus symmetric emphasis removal:

```ts
export function isConversationInterruptedAgentText(text: string): boolean {
  const trimmed = text.trim()
  if (trimmed === "Conversation interrupted") return true
  for (const marker of ["**", "__", "*", "_"] as const) {
    if (
      trimmed.startsWith(marker) &&
      trimmed.endsWith(marker) &&
      trimmed.length > marker.length * 2
    ) {
      return (
        trimmed.slice(marker.length, -marker.length) ===
        "Conversation interrupted"
      )
    }
  }
  return false
}
```

`filterDelegatedInterruptParts` recursively filters assistant top-level /
goal-run text but leaves reasoning and tool output untouched.

Add `isDelegatedChild?: boolean` to `MessageListViewProps` and
`LiveTranscriptRowProps`. Apply the filter before history grouping and return
`null` for an exact live text segment. Pass
`detail?.summary.parent_id != null` from `conversation-session-surface.tsx`.

- [ ] **Step 5: Run tests and commit**

```powershell
pnpm exec vitest run src/lib/delegation-conversation-interrupted.test.ts src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.test.tsx
git add src/lib/delegation-conversation-interrupted.ts src/lib/delegation-conversation-interrupted.test.ts src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.tsx src/components/message/live-transcript-row.test.tsx src/components/conversations/conversation-session-surface.tsx
git commit -m "fix(ui): hide interruption marker in delegated children"
```

---

### Task 8: Integration Regression and Verification

**Files:**
- Modify: `src/components/message/message-list-view.test.tsx`
- Test: frontend suites from Tasks 2-7 and Rust listener tests from Task 1

**Interfaces:**
- Consumes: completed backend and frontend contracts
- Produces: verified full fix with clean worktree

- [ ] **Step 1: Add one end-to-end frontend regression fixture**

In `message-list-view.test.tsx`, create a persisted session-shaped fixture with:

- two parallel explicit work keys;
- 10 non-adjacent status polls for the first unit;
- three continuations for that unit;
- interleaved assistant text;
- one orphan status id;
- final completion metadata.

Assert exactly two delegated cards, one residual orphan status row, all
interleaved text present, completed chrome terminal for the first unit, and no
mutation of the original turn fixture.

- [ ] **Step 2: Run frontend typecheck and focused tests**

```powershell
pnpm exec tsc --noEmit --incremental false
pnpm exec vitest run src/lib/delegation-card.test.ts src/lib/delegation-work-unit.test.ts src/lib/delegation-transcript-projection.test.ts src/lib/delegation-work-unit-runtime.test.ts src/lib/delegation-conversation-interrupted.test.ts src/hooks/use-delegation-card-model.test.ts src/hooks/use-delegation-card-model-hook.test.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/message-list-view.test.tsx src/components/message/live-transcript-row.test.tsx src/components/message/delegation-card-chrome.test.tsx src/components/message/delegated-sub-thread.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

Expected: no TypeScript errors and all focused tests pass.

- [ ] **Step 3: Run full frontend gates**

```powershell
pnpm test
pnpm eslint .
pnpm build
```

Expected: all commands exit 0.

- [ ] **Step 4: Run Rust delegation and project gates**

From `src-tauri/`:

```powershell
cargo test --features test-utils acp::delegation::listener -- --nocapture
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

Expected: all commands exit 0. If a full existing suite failure is unrelated,
record the exact test name and reproduce it on the branch base before deciding
whether it blocks this change.

- [ ] **Step 5: Inspect final diff and repository state**

```powershell
git diff --check
git status --short
git log --oneline --decorate -8
```

Confirm no database, generated asset, lockfile, or unrelated metadata changes.

- [ ] **Step 6: Commit only verification-driven fixes**

Stage the integration fixture and commit it:

```powershell
git add src/components/message/message-list-view.test.tsx
git commit -m "test(delegation): cover work-unit card regression"
```

Any verification failure outside this fixture returns to the owning task's
test/implementation cycle before this commit; do not bundle unrelated files.

## Spec Coverage

| Requirement | Task |
|-------------|------|
| Cause-less release returns real Broker state | 1 |
| Explicit timeout and user stop preserved | 1 |
| Work key / parent-child / continuation / task fallback | 2 |
| One canonical historical card per work unit | 3, 4 |
| Interleaved text preserved | 3, 8 |
| Unmapped and mixed status rows preserved | 3, 4 |
| Redundant incremental-live tools suppressed | 4 |
| Sticky elapsed/tool/edit continuity | 5, 6 |
| Orphan timeout and true terminals | 5, 6 |
| Inline and overlay identity agreement | 6 |
| Delegated-only interruption marker suppression | 7 |
| Historical session improvement without rewrite | 3, 4, 8 |
| Full frontend/Rust verification | 8 |

## Execution

Execute inline in this worktree with `superpowers:executing-plans`. The user has
already requested direct completion, and no sub-agent delegation is authorized
for this run.
