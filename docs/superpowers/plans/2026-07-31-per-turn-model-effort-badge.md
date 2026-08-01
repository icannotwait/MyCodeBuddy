# Per-turn model and reasoning effort badges Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move model and archived reasoning-effort metadata from the session status bar into each assistant turn's existing metadata footer.

**Architecture:** Extend the existing `MessageTurn` to `AdaptedMessage` to `ResolvedMessageGroup` pipeline with `reasoning_effort`, including the stateful adapter cache fingerprint. Aggregate distinct model and effort values when consecutive assistant sub-turns become one visual response, render them in `TurnStats`, and remove the status-bar-only session chip without changing backend contracts.

**Tech Stack:** Next.js 16, React 19, strict TypeScript, next-intl, Vitest, Testing Library, Tailwind CSS v4, lucide-react.

## Global Constraints

- Do not change Rust parsers, ACP frames, database schemas, backend APIs, or the conversation details panel.
- The footer reads archived turn metadata only; it never synthesizes effort from live session configuration.
- Null, undefined, and blank effort values render no effort control.
- Distinct model and effort values in a merged assistant run remain in first-seen order.
- Preserve all existing footer actions and all unrelated status-bar controls.
- Add the localized `messageList.reasoningEffort` label to all ten locale files.
- Every production behavior change follows a red-green-refactor cycle: write a focused failing test, run it, implement minimally, then rerun it.
- Use `pnpm` commands from `D:\MyCodeBuddy`; do not modify generated `out`, `.next`, `node_modules`, or Rust files.

---

### Task 1: Propagate reasoning effort through the adapter and invalidate its cache

**Files:**
- Modify: `src/lib/adapters/ai-elements-adapter.ts` (`AdaptedMessage`, `adaptMessageTurn`, `TurnCacheEntry`, `createMessageTurnAdapter`)
- Test: `src/lib/adapters/ai-elements-adapter.test.ts`

**Interfaces:**
- Consumes: `MessageTurn.reasoning_effort` from `src/lib/types.ts`.
- Produces: `AdaptedMessage.reasoning_effort?: string | null`, copied from the source turn; `createMessageTurnAdapter()` must return a new adapted object when only that field changes.

- [ ] **Step 1: Write the failing pass-through test**

Add a focused test beside the existing `adaptMessageTurn` tests:

```ts
it("copies reasoning effort onto the adapted message", () => {
  const adapted = adaptMessageTurn(
    {
      id: "a-effort",
      role: "assistant",
      timestamp: "2026-07-31T12:00:00.000Z",
      blocks: [{ type: "text", text: "reply" }],
      reasoning_effort: "high",
    },
    {
      attachedResources: "Attached resources",
      toolCallFailed: "Tool failed",
    }
  )

  expect(adapted.reasoning_effort).toBe("high")
})
```

- [ ] **Step 2: Run the adapter test to verify the pass-through test fails**

Run:

```text
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts
```

Expected: the new assertion fails because `adaptMessageTurn` does not yet expose `reasoning_effort`.

- [ ] **Step 3: Write the failing cache invalidation test**

Add a separate test to the adapter cache section:

```ts
it("misses the turn cache when only reasoning effort changes", () => {
  const adapter = createMessageTurnAdapter()
  const blocks = [{ type: "text" as const, text: "reply" }]
  const baseTurn = {
    id: "a-effort-cache",
    role: "assistant" as const,
    timestamp: "2026-07-31T12:00:00.000Z",
    blocks,
  }

  const [first] = adapter.adapt(
    [{ ...baseTurn, reasoning_effort: "low" }],
    {
      attachedResources: "Attached resources",
      toolCallFailed: "Tool failed",
    }
  )
  const [second] = adapter.adapt(
    [{ ...baseTurn, reasoning_effort: "high" }],
    {
      attachedResources: "Attached resources",
      toolCallFailed: "Tool failed",
    }
  )

  expect(second).not.toBe(first)
  expect(first.reasoning_effort).toBe("low")
  expect(second.reasoning_effort).toBe("high")
})
```

- [ ] **Step 4: Run the cache test to verify it fails for the missing fingerprint**

Run:

```text
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts
```

Expected: the test fails because the cached object is reused or does not carry the new field.

- [ ] **Step 5: Implement the minimal adapter changes**

Make these exact changes:

1. Add `reasoning_effort?: string | null` after `model` in `AdaptedMessage`.
2. Add the same nullable field to `TurnCacheEntry`.
3. Include `reasoning_effort: turn.reasoning_effort` in the object returned by `adaptMessageTurn`.
4. Add `cached.reasoning_effort === turn.reasoning_effort` to the cache reuse predicate.
5. Store `reasoning_effort: turn.reasoning_effort` in every cache entry.
6. Update the cache invalidation comment so its listed metadata fields include `reasoning_effort` after `model`.

- [ ] **Step 6: Run the adapter tests to verify green**

Run:

```text
pnpm exec vitest run src/lib/adapters/ai-elements-adapter.test.ts
```

Expected: all tests in the file pass, including both new tests.

- [ ] **Step 7: Commit the adapter task**

```text
git add src/lib/adapters/ai-elements-adapter.ts src/lib/adapters/ai-elements-adapter.test.ts
git commit -m "feat: carry reasoning effort through message adapter"
```

### Task 2: Preserve effort when assistant sub-turns are merged

**Files:**
- Modify: `src/components/message/message-list-view.tsx` (`ResolvedMessageGroup`, group creation, `mergeConsecutiveAssistantTurns`)
- Test: `src/components/message/message-list-view.test.tsx`

**Interfaces:**
- Consumes: `AdaptedMessage.reasoning_effort` from Task 1.
- Produces: `ResolvedMessageGroup.reasoning_effort?: string | null` and `reasoning_efforts?: string[]`, with the same singular/plural convention already used for model values.

- [ ] **Step 1: Write failing merge tests**

Add these tests to the existing `mergeConsecutiveAssistantTurns completion metadata` describe block:

```ts
it("preserves one reasoning effort on a merged assistant response", () => {
  const merged = mergeConsecutiveAssistantTurns([
    assistantItem("a", { reasoning_effort: "high" }),
    assistantItem("b"),
  ])

  const item = merged[0] as TurnItem
  expect(item.group.reasoning_effort).toBe("high")
  expect(item.group.reasoning_efforts).toBeUndefined()
})

it("deduplicates merged reasoning efforts in encounter order and ignores blanks", () => {
  const merged = mergeConsecutiveAssistantTurns([
    assistantItem("a", { reasoning_effort: " low " }),
    assistantItem("b", { reasoning_effort: "high" }),
    assistantItem("c", { reasoning_effort: "low" }),
    assistantItem("d", { reasoning_effort: "   " }),
  ])

  const item = merged[0] as TurnItem
  expect(item.group.reasoning_effort).toBe("low")
  expect(item.group.reasoning_efforts).toEqual(["low", "high"])
})
```

- [ ] **Step 2: Run the merge tests to verify they fail**

Run:

```text
pnpm exec vitest run src/components/message/message-list-view.test.tsx
```

Expected: the new assertions fail because `ResolvedMessageGroup` does not yet carry or aggregate effort.

- [ ] **Step 3: Implement the group fields and source mapping**

Make these exact changes in `message-list-view.tsx`:

1. Add optional `reasoning_effort?: string | null` and `reasoning_efforts?: string[]` after the existing `model`/`models` fields in `ResolvedMessageGroup`.
2. Add `reasoning_effort: msg.reasoning_effort` to the `group` object created from each adapted message.
3. In the merge accumulator, create `seenReasoningEfforts` and `mergedReasoningEfforts` next to the existing model sets.
4. For each group, trim `it.group.reasoning_effort`; if non-empty and unseen, append it.
5. Set `reasoning_effort` to the first merged effort and `reasoning_efforts` to the full array only when its length is greater than one.
6. Keep the existing model, duration, usage, completion, and outcome aggregation unchanged.

Use this normalization block inside the existing `for (const it of buffer)` loop:

```ts
const effort = it.group.reasoning_effort?.trim()
if (effort && !seenReasoningEfforts.has(effort)) {
  seenReasoningEfforts.add(effort)
  mergedReasoningEfforts.push(effort)
}
```

- [ ] **Step 4: Run the message-list tests to verify green**

Run:

```text
pnpm exec vitest run src/components/message/message-list-view.test.tsx
```

Expected: all message-list tests pass and the two new merge tests pass.

- [ ] **Step 5: Commit the merge task**

```text
git add src/components/message/message-list-view.tsx src/components/message/message-list-view.test.tsx
git commit -m "feat: preserve effort across merged assistant turns"
```

### Task 3: Render model and reasoning effort in each turn footer

**Files:**
- Modify: `src/components/message/turn-stats.tsx`
- Modify: `src/components/message/message-list-view.tsx` (`HistoricalMessageGroup` props)
- Modify: `src/components/message/turn-stats.test.tsx`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Consumes: `ResolvedMessageGroup.model`, `models`, `reasoning_effort`, and `reasoning_efforts`.
- Produces: `TurnStats` props `reasoningEffort?: string | null` and `reasoningEfforts?: string[]`, with accessible labels and localized tooltips.

- [ ] **Step 1: Write failing footer tests**

Extend `src/components/message/turn-stats.test.tsx` with a test that first verifies both controls and then verifies the effort control disappears when the prop is removed:

```tsx
it("shows model and archived reasoning effort in the footer", () => {
  const view = renderStats(
    <TurnStats
      copyText="reply"
      model="gpt-5.6-sol"
      reasoningEffort="high"
    />
  )

  expect(screen.getByLabelText("Model")).toBeInTheDocument()
  expect(screen.getByLabelText("Reasoning effort")).toBeInTheDocument()

  view.rerender(
    <NextIntlClientProvider locale="en" messages={enMessages}>
      <MessageScrollProvider value={{ scrollToIndex: vi.fn() }}>
        <TurnStats copyText="reply" model="gpt-5.6-sol" />
      </MessageScrollProvider>
    </NextIntlClientProvider>
  )
  expect(screen.queryByLabelText("Reasoning effort")).not.toBeInTheDocument()
})

it("renders metadata even when a turn has no copyable body", () => {
  renderStats(
    <TurnStats model="gpt-5.6-sol" reasoningEffort="medium" copyText="" />
  )

  expect(screen.getByLabelText("Model")).toBeInTheDocument()
  expect(screen.getByLabelText("Reasoning effort")).toBeInTheDocument()
})
```

- [ ] **Step 2: Run the footer tests to verify they fail**

Run:

```text
pnpm exec vitest run src/components/message/turn-stats.test.tsx
```

Expected: the new effort assertions fail because `TurnStats` has no effort prop/control and the metadata-only case returns `null`.

- [ ] **Step 3: Add the localized message key**

Add `reasoningEffort` immediately after `model` inside `Folder.chat.messageList` in all ten locale files with these values:

```text
ar: جهد الاستدلال
de: Denkaufwand
en: Reasoning effort
es: Esfuerzo de razonamiento
fr: Effort de raisonnement
ja: 推論レベル
ko: 추론 강도
pt: Esforço de raciocínio
zh-CN: 推理级别
zh-TW: 推理級別
```

- [ ] **Step 4: Implement the footer controls and pass the group metadata**

In `turn-stats.tsx`:

1. Import `Gauge` from `lucide-react`.
2. Add `reasoningEffort` and `reasoningEfforts` to `TurnStatsProps` and the component parameters.
3. Build `displayEfforts` from the plural values when present, otherwise the singular trimmed value.
4. Add `hasModel` and `hasEffort` booleans to the render guard so metadata-only turns do not return `null`.
5. Render a read-only `Gauge` tooltip control after the model control, using `t("reasoningEffort")` for its accessible label and joining display values with `, ` in the tooltip.
6. Keep the existing model tooltip and all other controls unchanged.

Use this shape for the effort value normalization:

```ts
const displayEfforts = (reasoningEfforts?.length
  ? reasoningEfforts
  : reasoningEffort?.trim()
    ? [reasoningEffort]
    : []
).map((value) => value.trim()).filter(Boolean)
const hasModel = displayModels.length > 0
const hasEffort = displayEfforts.length > 0
```

Update the early return to include `!hasModel && !hasEffort`, and pass `reasoningEffort={group.reasoning_effort}` and `reasoningEfforts={group.reasoning_efforts}` from `HistoricalMessageGroup`.

- [ ] **Step 5: Run the footer tests to verify green**

Run:

```text
pnpm exec vitest run src/components/message/turn-stats.test.tsx
```

Expected: all footer tests pass, including the existing jump gating tests and both new metadata tests.

- [ ] **Step 6: Verify locale key parity**

Run:

```text
pnpm exec vitest run src/i18n/messages.test.ts
```

Expected: the message schema/key tests pass for all ten locales.

- [ ] **Step 7: Commit the footer task**

```text
git add src/components/message/turn-stats.tsx src/components/message/turn-stats.test.tsx src/components/message/message-list-view.tsx src/i18n/messages
git commit -m "feat: show model and effort in turn footer"
```

### Task 4: Remove the session-level status-bar chip

**Files:**
- Modify: `src/components/layout/status-bar.tsx`
- Create: `src/components/layout/status-bar.test.tsx`
- Modify: `src/stores/turn-metadata-patches.test.ts`
- Delete: `src/components/layout/status-bar-session-model.tsx`
- Delete: `src/lib/status-bar-session-model.ts`
- Delete: `src/lib/status-bar-session-model.test.ts`

**Interfaces:**
- Consumes: the existing `StatusBar` children and `active-session-details` helpers used by detail surfaces.
- Produces: a status bar that retains stats/tasks/update/command/alerts but contains no `StatusBarSessionModel` in either responsive branch.

- [ ] **Step 1: Write a failing status-bar integration test**

Create `src/components/layout/status-bar.test.tsx` with lightweight child mocks. The session-chip mock intentionally renders a test id so the current implementation fails:

```tsx
import { render, screen } from "@testing-library/react"
import { describe, expect, it, vi } from "vitest"
import { StatusBar } from "./status-bar"

const mobileState = vi.hoisted(() => ({ value: false }))

vi.mock("@/hooks/use-mobile", () => ({
  useIsMobile: () => mobileState.value,
}))
vi.mock("./status-bar-stats", () => ({
  StatusBarStats: () => <span data-testid="status-bar-stats" />,
}))
vi.mock(
  "./status-bar-session-model",
  () => ({
    StatusBarSessionModel: () => (
      <span data-testid="status-bar-session-model" />
    ),
  }),
  { virtual: true }
)
vi.mock("./status-bar-tasks", () => ({
  StatusBarTasks: () => <span data-testid="status-bar-tasks" />,
}))
vi.mock("./status-bar-alerts", () => ({
  StatusBarAlerts: () => <span data-testid="status-bar-alerts" />,
}))
vi.mock("./status-bar-update", () => ({
  StatusBarUpdate: () => <span data-testid="status-bar-update" />,
}))
vi.mock("./command-dropdown", () => ({
  CommandDropdown: () => <span data-testid="command-dropdown" />,
}))

describe("StatusBar", () => {
  it.each([false, true])("does not render session model chip on mobile=%s", (mobile) => {
    mobileState.value = mobile
    render(<StatusBar />)

    expect(screen.getByTestId("status-bar-stats")).toBeInTheDocument()
    expect(screen.queryByTestId("status-bar-session-model")).not.toBeInTheDocument()
  })
})
```

- [ ] **Step 2: Run the status-bar test to verify it fails**

Run:

```text
pnpm exec vitest run src/components/layout/status-bar.test.tsx
```

Expected: both cases fail because the current `StatusBar` renders `StatusBarSessionModel`.

- [ ] **Step 3: Remove the status-bar-only implementation**

1. Delete the `StatusBarSessionModel` import and both JSX usages from `status-bar.tsx`.
2. Update the mobile comment to describe workspace stats on the left without session model/thinking.
3. Update the desktop comment to describe only the remaining left-side workspace stats.
4. Delete the three status-bar-only source/test files listed above.
5. In `turn-metadata-patches.test.ts`, remove the `resolveSessionModelDisplay` import and replace the two status-output assertions with direct assertions on `resolveActiveSessionDetails`: the archive-effort case must produce `model === "gpt-5.6-sol"` and `reasoningEffort === "high"`; the no-effort case must produce `model === "gpt-5.6-sol"` and `reasoningEffort === null`.

- [ ] **Step 4: Run the status-bar and metadata tests to verify green**

Run:

```text
pnpm exec vitest run src/components/layout/status-bar.test.tsx src/stores/turn-metadata-patches.test.ts
```

Expected: the new desktop/mobile status-bar assertions and existing metadata-sync tests pass, with no import of the deleted resolver remaining.

- [ ] **Step 5: Commit the status-bar removal**

```text
git add src/components/layout/status-bar.tsx src/components/layout/status-bar.test.tsx src/stores/turn-metadata-patches.test.ts
git rm src/components/layout/status-bar-session-model.tsx src/lib/status-bar-session-model.ts src/lib/status-bar-session-model.test.ts
git commit -m "refactor: move session model metadata into turn footers"
```

### Task 5: Run full verification and review the implementation

**Files:**
- Verify: all changed files from Tasks 1-4
- Include: `docs/superpowers/plans/2026-07-31-per-turn-model-effort-badge.md` in the final documentation commit if the ignored plan file is not already tracked.

**Interfaces:**
- Consumes: all implementation changes and focused test evidence from Tasks 1-4.
- Produces: a clean, reviewed commit on `main` with no untracked implementation files.

- [ ] **Step 1: Inspect the complete diff and status**

Run:

```text
git status --short
git diff 646746e7..HEAD --stat
git diff 646746e7..HEAD --check
rg -n -S --glob 'src/**/*.{ts,tsx}' "StatusBarSessionModel|resolveSessionModelDisplay|reasoning_effort" src/components src/lib src/stores
```

Confirm that only the intended UI, adapter, tests, locale files, and plan/documentation files changed; no deleted resolver import remains; and the effort field appears in the adapter/group/footer path.

- [ ] **Step 2: Run the full Vitest suite**

Run:

```text
pnpm test
```

Expected: exit code 0 with zero failed tests.

- [ ] **Step 3: Run ESLint**

Run:

```text
pnpm eslint .
```

Expected: exit code 0 with zero errors.

- [ ] **Step 4: Run the static export build**

Run:

```text
pnpm build
```

Expected: exit code 0 and a successful Next.js static export.

- [ ] **Step 5: Request code review against the pre-feature commit**

Use the requesting-code-review workflow with:

- Description: Move archived per-turn model and reasoning effort metadata into `TurnStats`; remove the session-level status-bar chip; preserve merged assistant metadata and existing footer/status-bar behavior.
- Requirements: `docs/superpowers/specs/2026-07-31-per-turn-model-effort-badge-design.md` and its acceptance criteria.
- Base SHA: `646746e7` (the approved design commit before implementation).
- Head SHA: the implementation commit at this point.

Fix every Critical or Important review finding, rerun the relevant focused test and full verification command, and do not finalize while an Important finding remains unresolved.

- [ ] **Step 6: Commit the plan if needed and record the final implementation**

If the plan remains ignored and untracked, force-add only this plan file before the final implementation commit:

```text
git add -f docs/superpowers/plans/2026-07-31-per-turn-model-effort-badge.md
git commit -m "docs: add per-turn model effort implementation plan"
```

Otherwise, leave already tracked documentation unchanged. Confirm the final implementation commit with `git show --stat --oneline HEAD` and `git status --short`.

## Plan Self-review

- Spec coverage: Tasks 1 and 2 cover the complete metadata pipeline and merged-turn semantics; Task 3 covers compact UI, accessibility, and all ten locales; Task 4 removes both responsive status-bar usages without touching detail surfaces; Task 5 covers focused tests, full tests, lint, build, review, and final VCS state.
- Placeholder scan: no unfinished placeholder or unspecified implementation step is used; every test and command names a concrete file and expected outcome.
- Type consistency: `reasoning_effort` is singular and nullable at `MessageTurn`, `AdaptedMessage`, and `ResolvedMessageGroup`; plural `reasoning_efforts` is introduced only for merged display groups; `TurnStats` accepts both forms and normalizes them before rendering.
- Scope: no backend or schema task is present because the approved design explicitly reuses the existing archived field.
