# Codex Archived Turn Effort Status Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Backfill Codex's archived per-turn reasoning effort onto the completed local assistant turn so the existing status bar shows `<model> · high` only after the archive records `high`.

**Architecture:** Extend the existing post-turn metadata patch contract and its history-aware alignment helper to carry `MessageTurn.reasoning_effort` from the same parsed assistant turn used for model and timing metadata. Apply that patch in the existing `PATCH_TURN_METADATA` reducer with model-style first-write-wins semantics; the current turns-first status resolver and history-only display resolver then update without any status-bar component change.

**Tech Stack:** TypeScript 5.8 (strict), Zustand 5, React 19, Vitest 2, pnpm 11.

**Design:** `docs/superpowers/specs/2026-07-28-codex-archived-turn-effort-status-design.md` (`sha256:1da4693dbdaedced7f2ad32c53eb0ca9e89e0e0ea89e3bb1bdf936cb53785ac6`)

## Global Constraints

- Display only effort persisted on a real archived assistant turn.
- Never infer or fall back to the live ACP `reasoning_effort` selector.
- Update the status bar after the existing post-turn archive reparse observes the completed Codex turn.
- Preserve the history boundary: older archived turn effort is never assigned to a newly completed reply.
- Preserve first-write-wins: archive fills missing local effort and never overwrites populated local effort.
- Continue showing only model when the archived turn has no effort.
- Extend `TurnMetadataPatch` with optional `reasoning_effort?: string | null` and mirror that field in the internal `PATCH_TURN_METADATA` action payload.
- `computeTurnMetadataPatches` must copy effort from the same matched parsed assistant turn used for model, usage, duration, and completion time.
- In the merged-sub-turn case, prefer the latest aligned parsed turn's effort; when it is missing, use exactly the existing folded-turn fallback policy used by model metadata. Do not consult session configuration.
- `PATCH_TURN_METADATA` must include effort in change detection and the updated local turn, using `turn.reasoning_effort ?? patch.reasoning_effort` when the patch field is defined.
- Do not modify `resolveActiveSessionDetails`, `resolveSessionModelDisplay`, or `StatusBarSessionModel`; their current local-turn preference and refusal to use live-config effort are the intended display path.
- Non-goals: showing selected effort before archive persistence; session-level effort on conversation summaries; Codex rollout parser changes; ACP selector changes; status-bar visual, layout, or i18n changes.
- Keep the change frontend-only. Do not modify Rust, parser, database, transport, dependency, or localization files.
- Preserve all unrelated worktree changes. At plan validation time,
  `docs/superpowers/specs/2026-07-28-grok-acp-mcp-bridge-design.md` is modified
  independently of this work and must remain untouched.
- Acceptance: archived effort `high` after a successful post-turn reparse produces `<model> · high`; a parsed turn with no archived effort produces only `<model>`.

---

## File Map

| File | Planned responsibility |
|---|---|
| `src/stores/conversation-runtime-store.ts` | Add the optional patch/action field, propagate aligned/folded archived effort, and apply it to `ConversationRuntimeSession.localTurns` with first-write-wins semantics. |
| `src/stores/turn-metadata-patches.test.ts` | Add focused alignment, history-boundary, merged-sub-turn, reducer, and end-to-end status-data regressions through the existing `syncTurnMetadata` path. |
| `src/components/conversations/active-session-details.ts` | Verification only: confirm `localTurns` remains preferred over cold `detail.turns`; no edit planned. |
| `src/components/conversations/active-session-details.test.ts` | Verification only: retain latest-local-turn effort coverage; no edit planned. |
| `src/lib/status-bar-session-model.ts` | Verification only: retain archive-only effort resolution; no edit planned. |
| `src/lib/status-bar-session-model.test.ts` | Verification only: retain the regression proving live ACP config is not an effort fallback; no edit planned. |
| `src/components/layout/status-bar-session-model.tsx` | Verification only: existing rendering already emits `model`, separator, and effort; no edit planned. |

## Inspected Baseline

- `MessageTurn` already exposes `reasoning_effort?: string | null` in `src/lib/types.ts:271`.
- `TurnMetadataPatch` at `src/stores/conversation-runtime-store.ts:1389` currently carries usage, duration, model, completion time, and tool metadata, but not effort.
- The internal `Action` variant at `src/stores/conversation-runtime-store.ts:515` duplicates the scalar patch shape and must stay type-consistent with `TurnMetadataPatch`.
- `computeTurnMetadataPatches` at `src/stores/conversation-runtime-store.ts:1434` slices `persistedAssistantCount`, tail-aligns complete parses, head-aligns lagging parses, and folds only unmatched current-session sub-turns. Its model fallback is `if (!modelToApply && extra.model) modelToApply = extra.model`.
- `syncTurnMetadata` at `src/stores/conversation-runtime-store.ts:4740` already performs the delayed `getFolderConversation` reparse and dispatches the computed patches. Its retry scheduling and usage-based retry criterion remain unchanged.
- The `PATCH_TURN_METADATA` reducer at `src/stores/conversation-runtime-store.ts:2402` implements first-write-wins for model as `turn.model ?? patch.model`, performs identity-based change detection, and updates `localTurns`.
- `resolveActiveSessionDetails` at `src/components/conversations/active-session-details.ts:82` scans `localTurns` before `detail.turns` for both model and effort.
- `resolveSessionModelDisplay` at `src/lib/status-bar-session-model.ts:45` permits live-config fallback for model only and derives `thinkingLevel` exclusively from `conversationEffort`.
- `StatusBarSessionModel` at `src/components/layout/status-bar-session-model.tsx:108` renders the resolved values as `model · thinkingLevel`; no display-layer code is missing.
- Baseline focused verification on 2026-07-30: `pnpm exec vitest run src/stores/turn-metadata-patches.test.ts src/components/conversations/active-session-details.test.ts src/lib/status-bar-session-model.test.ts` passes 35 tests across 3 files.

## Risk Policy

Policy version: `b2d_task_risk_v1`

Hard triggers always route high: `concurrency_lifecycle`,
`security_trust_boundary`, `migration_destructive_persistence`,
`public_compatibility`, `unsafe_ffi`, `update_rollback`.

When no hard trigger is active, sum distinct evidence-backed soft signals:
`cross_runtime_or_process=2`; `broad_production_surface=1`;
`multiple_ownership_modules=1`; `shared_interface=1`;
`dependency_or_build=1`; `multi_layer_without_test_seam=1`. A total of 3 or
more routes high; 0-2 routes normal.

Plan reviewer cohort for the material revision's later plan-review gate (the
author does not self-review):

`plan|docs/superpowers/plans/2026-07-28-codex-archived-turn-effort-status.md|reviewer|codex|none`

## Task Routing Matrix

| Task | Planned production surface / files-modules | Hard triggers (evidence) | Soft signals (score parts) | Soft total | Final level + reason | Implementer | Reviewer set | Policy version |
|---|---|---|---|---:|---|---|---|---|
| 1 | One frontend module: `src/stores/conversation-runtime-store.ts` patch type + pure alignment helper; one colocated store test file | None: no scheduling, cancellation, retry lifecycle, trust boundary, persistence, FFI, updater, or rollback behavior changes; history slicing and timer control flow remain unchanged. `public_compatibility` is not active because `TurnMetadataPatch` is an in-repository helper/test export, the new field is optional and additive, and no package, transport, parser, database, or serialized contract changes. | `shared_interface=1`: adds one optional field to exported `TurnMetadataPatch` and its internal action mirror. `cross_runtime_or_process=0`: parsed data is already in frontend memory. `broad_production_surface=0`: one helper branch. `multiple_ownership_modules=0`: one production module. `dependency_or_build=0`: no package/config change. `multi_layer_without_test_seam=0`: `computeTurnMetadataPatches` is directly unit tested. | 1 | normal: no hard trigger and soft total 1 | Grok | Independent Codex reviewer (not the author) | `b2d_task_risk_v1` |
| 2 | One frontend module: `src/stores/conversation-runtime-store.ts` reducer branch; integration assertions in `src/stores/turn-metadata-patches.test.ts`; existing status resolvers are read-only consumers | None: the existing post-turn timer/retry lifecycle is invoked by tests but not modified; no security, persistence, compatibility, FFI, update, or rollback surface changes. | `shared_interface=1`: the reducer consumes Task 1's optional patch field and updates the shared `ConversationRuntimeSession.localTurns` turn shape. `cross_runtime_or_process=0`: mocked archive results enter through the existing API. `broad_production_surface=0`: only `PATCH_TURN_METADATA`. `multiple_ownership_modules=0`: no status production module edit. `dependency_or_build=0`: no dependency/config change. `multi_layer_without_test_seam=0`: the public `syncTurnMetadata` action plus pure status resolvers form an existing test seam. | 1 | normal: no hard trigger and soft total 1 | Grok | Independent Codex reviewer (not the author) | `b2d_task_risk_v1` |

Implementation admission rule: classify each task from this matrix before work
begins. A task reviewer must be independent of both the Plan Author and that
task's implementer.

---

### Task 1: Carry Archived Effort Through Turn Alignment

**Files:**

- Modify: `src/stores/conversation-runtime-store.ts:515-527`
- Modify: `src/stores/conversation-runtime-store.ts:1388-1529`
- Test: `src/stores/turn-metadata-patches.test.ts:41-229`

**Interfaces:**

- Consumes: `MessageTurn.reasoning_effort?: string | null`; `computeTurnMetadataPatches(params: { localAssistantIndices: number[]; parsedAssistantTurns: MessageTurn[]; persistedAssistantCount: number }): TurnMetadataPatch[]`.
- Produces: `TurnMetadataPatch.reasoning_effort?: string | null`, mirrored by `Action`'s `PATCH_TURN_METADATA.turnPatches[]`; patches may be emitted when archived effort is the only available scalar metadata.
- Alignment contract: the matched `sessionParsedTurns[parsedIdx]` wins; only for local index 0 with `offset > 0`, a missing matched effort follows the same first-truthy folded-extra policy as model.

**Requirement coverage:** This task sources effort only from parsed archived assistant turns, never reads ACP config, preserves `persistedAssistantCount`, keeps a lagging parse from assigning older metadata forward, implements the specified merged-sub-turn policy, and leaves absent archived effort absent.

- [ ] **Step 1: Add failing alignment tests**

Add these cases inside the existing `describe("computeTurnMetadataPatches", ...)` block in `src/stores/turn-metadata-patches.test.ts`:

```ts
  it("copies reasoning effort from the matched archived assistant turn", () => {
    const patches = computeTurnMetadataPatches({
      localAssistantIndices: [1],
      parsedAssistantTurns: [asst({ id: "new", reasoning_effort: "high" })],
      persistedAssistantCount: 0,
    })

    expect(patches).toEqual([
      {
        index: 1,
        usage: undefined,
        duration_ms: undefined,
        model: undefined,
        completed_at: undefined,
        reasoning_effort: "high",
      },
    ])
  })

  it("does not cross the history boundary for reasoning effort", () => {
    const patches = computeTurnMetadataPatches({
      localAssistantIndices: [1],
      parsedAssistantTurns: [
        asst({ id: "history", reasoning_effort: "high" }),
        asst({ id: "new", model: "gpt-5.6-sol" }),
      ],
      persistedAssistantCount: 1,
    })

    expect(patches).toHaveLength(1)
    expect(patches[0]).toMatchObject({ index: 1, model: "gpt-5.6-sol" })
    expect(patches[0]?.reasoning_effort).toBeUndefined()
  })

  it("uses the latest aligned sub-turn effort when parser sub-turns are folded", () => {
    const patches = computeTurnMetadataPatches({
      localAssistantIndices: [0],
      parsedAssistantTurns: [
        asst({ id: "s0", reasoning_effort: "low" }),
        asst({ id: "s1", reasoning_effort: "medium" }),
        asst({ id: "s2", reasoning_effort: "high" }),
      ],
      persistedAssistantCount: 0,
    })

    expect(patches).toHaveLength(1)
    expect(patches[0]?.reasoning_effort).toBe("high")
  })

  it("uses model-style folded fallback when the aligned sub-turn has no effort", () => {
    const patches = computeTurnMetadataPatches({
      localAssistantIndices: [0],
      parsedAssistantTurns: [
        asst({ id: "s0", reasoning_effort: "low" }),
        asst({ id: "s1", reasoning_effort: "medium" }),
        asst({ id: "s2", model: "gpt-5.6-sol" }),
      ],
      persistedAssistantCount: 0,
    })

    expect(patches).toHaveLength(1)
    expect(patches[0]).toMatchObject({
      index: 0,
      model: "gpt-5.6-sol",
      reasoning_effort: "low",
    })
  })
```

The last assertion deliberately expects `low`: the current model fallback scans folded extras from index 0 and accepts the first truthy value. Effort must follow that exact existing policy rather than inventing a different session-level or latest-extra rule.

- [ ] **Step 2: Run the focused tests and verify RED**

Run from `D:\MyCodeBuddy`:

```powershell
pnpm exec vitest run src/stores/turn-metadata-patches.test.ts -t "reasoning effort|sub-turn effort|folded fallback"
```

Expected: FAIL. The effort-only case currently returns `[]`, and merged cases receive `undefined` because `TurnMetadataPatch` and `computeTurnMetadataPatches` do not carry `reasoning_effort`.

- [ ] **Step 3: Extend the patch contracts**

Add the optional field to both exact patch shapes in `src/stores/conversation-runtime-store.ts`:

```ts
export interface TurnMetadataPatch {
  index: number
  usage?: TurnUsage | null
  duration_ms?: number | null
  model?: string | null
  reasoning_effort?: string | null
  completed_at?: string | null
  tool_meta?: Array<{
    tool_use_id: string
    meta: Record<string, unknown> | null
  }>
}
```

```ts
  | {
      type: "PATCH_TURN_METADATA"
      conversationId: number
      turnPatches: Array<{
        index: number
        usage?: TurnUsage | null
        duration_ms?: number | null
        model?: string | null
        reasoning_effort?: string | null
        completed_at?: string | null
        tool_meta?: Array<{
          tool_use_id: string
          meta: Record<string, unknown> | null
        }>
      }>
      sessionStats?: SessionStats | null
    }
```

Also change the metadata helper doc phrase from `(usage / duration / model / completed_at)` to `(usage / duration / model / reasoning effort / completed_at)` so its declared contract matches its output.

- [ ] **Step 4: Implement matched and folded effort propagation**

In `computeTurnMetadataPatches`, add the effort variable beside `modelToApply`, source it from the matched parsed turn, mirror the model fallback inside the existing folded-extra loop, include it in the empty-patch guard, and emit it:

```ts
    let usageToApply: TurnUsage | null | undefined
    let durationToApply: number | null | undefined
    let modelToApply: string | null | undefined
    let reasoningEffortToApply: string | null | undefined
    // For the merged-sub-turn case (offset > 0), the latest completion is
    // sessionParsedTurns[parsedIdx] (the sub-turn we matched); earlier
    // rolled-in parsed turns precede it in time, so we don't aggregate
    // completion timestamps.
    let completedAtToApply: string | null | undefined

    if (parsedIdx >= 0 && parsedIdx < sessionParsedTurns.length) {
      const pt = sessionParsedTurns[parsedIdx]
      usageToApply = pt.usage
      durationToApply = pt.duration_ms
      modelToApply = pt.model
      reasoningEffortToApply = pt.reasoning_effort
      completedAtToApply = pt.completed_at
    }
```

Add this immediately after the existing model fallback in the folded-extra loop:

```ts
        if (!modelToApply && extra.model) {
          modelToApply = extra.model
        }
        if (!reasoningEffortToApply && extra.reasoning_effort) {
          reasoningEffortToApply = extra.reasoning_effort
        }
```

Replace the patch guard and push with:

```ts
    if (
      !usageToApply &&
      !durationToApply &&
      !modelToApply &&
      !reasoningEffortToApply &&
      !completedAtToApply
    )
      continue
    patches.push({
      index: localAssistantIndices[i],
      usage: usageToApply,
      duration_ms: durationToApply,
      model: modelToApply,
      reasoning_effort: reasoningEffortToApply,
      completed_at: completedAtToApply,
    })
```

Do not read `configOptions`, `SESSION_REASONING_CONFIG_ID`, connection state, conversation summary, or parser/session configuration anywhere in this helper.

- [ ] **Step 5: Run the focused and complete helper tests and verify GREEN**

```powershell
pnpm exec vitest run src/stores/turn-metadata-patches.test.ts -t "computeTurnMetadataPatches"
```

Expected: PASS for all alignment tests, including the existing usage/duration/model/history regressions and the four new effort cases.

- [ ] **Step 6: Commit Task 1**

```powershell
git add src/stores/conversation-runtime-store.ts src/stores/turn-metadata-patches.test.ts
git commit -m "feat(runtime): carry archived turn reasoning effort"
```

---

### Task 2: Apply Effort First-Write-Wins and Verify Status Output

**Files:**

- Modify: `src/stores/conversation-runtime-store.ts:2402-2462`
- Test: `src/stores/turn-metadata-patches.test.ts:1-7`
- Test: `src/stores/turn-metadata-patches.test.ts:237-303` (add helpers and a new describe block before the existing baseline-capture suite)
- Verify only: `src/components/conversations/active-session-details.test.ts`
- Verify only: `src/lib/status-bar-session-model.test.ts`

**Interfaces:**

- Consumes: Task 1's `TurnMetadataPatch.reasoning_effort?: string | null`; existing `RuntimeActions.syncTurnMetadata(dbConversationId: number, runtimeConversationId?: number): () => void`; existing `resolveActiveSessionDetails(...)` and `resolveSessionModelDisplay(...)`.
- Produces: a local assistant `MessageTurn.reasoning_effort` that is filled only when missing; existing values, including any non-null string, remain authoritative.
- State rule: when the patch omits effort, retain `turn.reasoning_effort`; when the patch supplies effort, compute `turn.reasoning_effort ?? patch.reasoning_effort`.

**Requirement coverage:** This task exercises the real post-turn reparse action, confirms the completed local turn changes after archive observation, proves first-write-wins, proves an archive without effort stays model-only, and runs the existing regression that forbids live ACP selector fallback. It does not modify status UI, parser, summary, selector, layout, or i18n code.

Task-level acceptance requires all six locked behaviors together: display only effort persisted on a real archived assistant turn; never infer or fall back to the live ACP `reasoning_effort` selector; update after the existing post-turn archive reparse observes the completed Codex turn; never assign an older archived turn's effort across the history boundary; fill only missing local effort without overwriting a populated value; and continue showing only model when archived effort is absent.

- [ ] **Step 1: Add the mocked archive and status-data test imports**

Replace the Vitest import and add the resolver imports/mocked API at the top of `src/stores/turn-metadata-patches.test.ts`:

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest"
import {
  computeTurnMetadataPatches,
  resetConversationRuntimeStore,
  useConversationRuntimeStore,
} from "@/stores/conversation-runtime-store"
import { resolveActiveSessionDetails } from "@/components/conversations/active-session-details"
import { resolveSessionModelDisplay } from "@/lib/status-bar-session-model"
import type { DbConversationDetail, MessageTurn, TurnUsage } from "@/lib/types"

vi.mock("@/lib/api", () => ({
  getFolderConversation: vi.fn(),
}))

const { getFolderConversation } = await import("@/lib/api")
const mockGetFolderConversation = vi.mocked(getFolderConversation)
```

- [ ] **Step 2: Add failing store/reducer integration tests**

Insert these helpers and tests after the existing `baseline()` helper and before `describe("historyAssistantBaseline capture", ...)`:

```ts
function seedMetadataSyncSession(
  localAssistant: MessageTurn,
  persistedTurns: MessageTurn[] = []
) {
  seedDetail(persistedTurns)
  useConversationRuntimeStore.setState((state) => {
    const byId = new Map(state.byConversationId)
    const current = byId.get(CID)
    if (!current) throw new Error("missing metadata sync session")
    byId.set(CID, {
      ...current,
      localTurns: [localAssistant],
      historyAssistantBaseline: persistedTurns.filter(
        (turn) => turn.role === "assistant"
      ).length,
    })
    return { byConversationId: byId }
  })
}

function currentMetadataSyncSession() {
  const session = useConversationRuntimeStore
    .getState()
    .byConversationId.get(CID)
  if (!session) throw new Error("missing metadata sync session")
  return session
}

async function flushMetadataSync() {
  await vi.advanceTimersByTimeAsync(1500)
  await Promise.resolve()
  await Promise.resolve()
}

describe("syncTurnMetadata archived reasoning effort", () => {
  beforeEach(() => {
    resetConversationRuntimeStore()
    mockGetFolderConversation.mockReset()
    vi.useFakeTimers()
  })

  afterEach(() => {
    resetConversationRuntimeStore()
    vi.useRealTimers()
  })

  it("fills missing local effort and resolves model plus high for the status bar", async () => {
    seedMetadataSyncSession(
      asst({ id: "live", model: "gpt-5.6-sol", reasoning_effort: null })
    )
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWith([
        asst({
          id: "archived",
          model: "gpt-5.6-sol",
          reasoning_effort: "high",
          usage: usage(10, 2),
        }),
      ])
    )

    const cancel = useConversationRuntimeStore
      .getState()
      .actions.syncTurnMetadata(CID)
    await flushMetadataSync()
    cancel()

    const runtime = currentMetadataSyncSession()
    expect(runtime.localTurns[0]?.reasoning_effort).toBe("high")
    const details = resolveActiveSessionDetails(
      { conversationId: CID },
      (id) => (id === CID ? runtime : null),
      []
    )
    const display = resolveSessionModelDisplay({
      configOptions: null,
      conversationModel: details.model,
      conversationEffort: details.reasoningEffort,
    })
    expect([display.model, display.thinkingLevel].filter(Boolean).join(" · ")).toBe(
      "gpt-5.6-sol · high"
    )
  })

  it("does not overwrite effort already present on the local turn", async () => {
    seedMetadataSyncSession(
      asst({
        id: "live",
        model: "live-model",
        reasoning_effort: "medium",
      })
    )
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWith([
        asst({
          id: "archived",
          model: "archive-model",
          reasoning_effort: "high",
          usage: usage(10, 2),
        }),
      ])
    )

    const cancel = useConversationRuntimeStore
      .getState()
      .actions.syncTurnMetadata(CID)
    await flushMetadataSync()
    cancel()

    expect(currentMetadataSyncSession().localTurns[0]).toMatchObject({
      model: "live-model",
      reasoning_effort: "medium",
    })
  })

  it("keeps the status output model-only when the archive has no effort", async () => {
    seedMetadataSyncSession(asst({ id: "live", model: "gpt-5.6-sol" }))
    mockGetFolderConversation.mockResolvedValueOnce(
      detailWith([
        asst({
          id: "archived",
          model: "gpt-5.6-sol",
          usage: usage(10, 2),
        }),
      ])
    )

    const cancel = useConversationRuntimeStore
      .getState()
      .actions.syncTurnMetadata(CID)
    await flushMetadataSync()
    cancel()

    const runtime = currentMetadataSyncSession()
    expect(runtime.localTurns[0]?.reasoning_effort).toBeUndefined()
    const details = resolveActiveSessionDetails(
      { conversationId: CID },
      (id) => (id === CID ? runtime : null),
      []
    )
    const display = resolveSessionModelDisplay({
      configOptions: null,
      conversationModel: details.model,
      conversationEffort: details.reasoningEffort,
    })
    expect([display.model, display.thinkingLevel].filter(Boolean).join(" · ")).toBe(
      "gpt-5.6-sol"
    )
  })
})
```

`usage(10, 2)` prevents the existing usage-based retry from scheduling a second fetch, so these tests isolate the first successful archive observation. The tests invoke the existing 1500 ms post-turn delay and do not bypass the production reducer.

- [ ] **Step 3: Run the reducer/status integration tests and verify RED**

```powershell
pnpm exec vitest run src/stores/turn-metadata-patches.test.ts -t "syncTurnMetadata archived reasoning effort"
```

Expected: FAIL in `fills missing local effort...`; Task 1 emits `reasoning_effort: "high"`, but `PATCH_TURN_METADATA` does not yet copy it to `localTurns`, so the rendered status string is model-only.

- [ ] **Step 4: Apply reducer first-write-wins semantics**

In `case "PATCH_TURN_METADATA"`, add `newReasoningEffort` immediately after `newModel`:

```ts
        const newModel =
          patch.model === undefined ? turn.model : (turn.model ?? patch.model)
        const newReasoningEffort =
          patch.reasoning_effort === undefined
            ? turn.reasoning_effort
            : (turn.reasoning_effort ?? patch.reasoning_effort)
        const newCompletedAt =
          patch.completed_at === undefined
            ? turn.completed_at
            : (turn.completed_at ?? patch.completed_at)
```

Add effort to the existing change-detection condition:

```ts
        if (
          newUsage !== turn.usage ||
          newDuration !== turn.duration_ms ||
          newModel !== turn.model ||
          newReasoningEffort !== turn.reasoning_effort ||
          newCompletedAt !== turn.completed_at ||
          nextBlocks !== turn.blocks
        ) {
```

Add it to the updated local turn:

```ts
          patchedTurns[patch.index] = {
            ...turn,
            blocks: nextBlocks,
            usage: newUsage,
            duration_ms: newDuration,
            model: newModel,
            reasoning_effort: newReasoningEffort,
            completed_at: newCompletedAt,
          }
```

Do not change `syncTurnMetadata` delays, retry count, usage-based retry condition, history baseline capture, status resolver precedence, or UI rendering.

- [ ] **Step 5: Run the new integration tests and verify GREEN**

```powershell
pnpm exec vitest run src/stores/turn-metadata-patches.test.ts -t "syncTurnMetadata archived reasoning effort"
```

Expected: PASS. The first case resolves `gpt-5.6-sol · high`, the second retains local `medium`, and the third resolves only `gpt-5.6-sol`.

- [ ] **Step 6: Run focused status and boundary regressions**

```powershell
pnpm exec vitest run src/stores/turn-metadata-patches.test.ts src/components/conversations/active-session-details.test.ts src/lib/status-bar-session-model.test.ts
```

Expected: PASS. In particular, `does not show effort from live config when history has none` must remain green, proving no ACP selector fallback was introduced.

- [ ] **Step 7: Run the required frontend verification**

```powershell
pnpm eslint .
pnpm test
pnpm build
```

Expected: all commands exit 0; Vitest has no failed tests; Next.js completes its static export build. Do not run Rust checks because the planned production and test changes are frontend-only.

- [ ] **Step 8: Inspect the final diff for scope and commit Task 2**

```powershell
git diff --check
git diff -- src/stores/conversation-runtime-store.ts src/stores/turn-metadata-patches.test.ts
git status --short
git add src/stores/conversation-runtime-store.ts src/stores/turn-metadata-patches.test.ts
git commit -m "feat(runtime): backfill archived turn reasoning effort"
```

Expected task diff scope: only the two planned frontend files. Leave every
unrelated modification unstaged and unchanged, including the pre-existing
Grok bridge design edit recorded in Global Constraints.

---

## Author Self-Review Against Design

- [x] Spec coverage: every locked requirement maps to Task 1 alignment tests/implementation or Task 2 reducer/status integration tests; every non-goal is an explicit no-edit boundary.
- [x] Archive provenance: all new values originate at `MessageTurn.reasoning_effort` in `parsedAssistantTurns`; no session config or selector API appears in production snippets.
- [x] History safety: the existing `persistedAssistantCount` slice is unchanged and receives a dedicated effort regression.
- [x] Merged-sub-turn consistency: matched effort wins; missing matched effort copies the current model fallback's first-truthy folded-extra behavior.
- [x] First-write-wins: reducer formula, change detection, updated turn, fill test, and non-overwrite test use the same `string | null | undefined` contract.
- [x] Missing-effort behavior: an archive with no effort remains model-only, and the existing live-config non-fallback test is part of required verification.
- [x] Placeholder scan: every code-changing step contains concrete TypeScript, exact paths, commands, expected outcomes, and commit commands.
- [x] Type consistency: `reasoning_effort?: string | null` matches `MessageTurn`, `TurnMetadataPatch`, the internal action payload, reducer input, and local turn output.
- [x] Scope consistency: no parser, ACP selector, summary, status component, visual, layout, localization, Rust, dependency, or build configuration edit is planned.
- [x] Routing consistency: both tasks have no hard trigger, soft total 1, Grok implementer, and independent Codex reviewer under `b2d_task_risk_v1`.

This plan ends at the implementation boundary. Execute it using the
user-selected workflow from the handoff: subagent-driven task execution or
inline execution with checkpoints.
