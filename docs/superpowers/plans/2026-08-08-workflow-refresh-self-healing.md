# Workflow Refresh Self-Healing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make delegation cards and visible workflow graphs converge from durable snapshots after missed frontend events, while required workflow event subscriptions recover automatically.

**Architecture:** Keep events as the low-latency path and add bounded durable reconciliation in the two existing frontend owners. The delegation model first selects identity-safe effective binding, meta, and run-snapshot sources; the workflow store selects one per-conversation refresh delay from authoritative graph activity and owns generation-scoped required-listener retry state. No backend contract, schema, event payload, or persistence code changes.

**Tech Stack:** Next.js 16, React 19, strict TypeScript, Zustand, Vitest fake timers, pnpm, PowerShell 7.

## Global Constraints

- Approved baseline: `docs/superpowers/specs/2026-08-08-workflow-refresh-self-healing-design.md`, LF-normalized SHA-256 `2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf`. Do not modify the design during delivery.
- Product scope is frontend-only. Do not change Rust, database schemas, persistence, HTTP/Tauri APIs, transport event names, event payloads, or backend behavior.
- Modify only `src/hooks/use-delegation-card-model.ts`, `src/hooks/use-delegation-card-model.test.ts`, `src/lib/workflow-graph-store.ts`, and `src/lib/workflow-graph-store.test.ts` unless a final verification failure proves an additional frontend file is required. Any scope expansion requires the parent to reroute risk before editing.
- Preserve `scopeDelegationBindingForCard` exactly as the card-to-run isolation gate. Preserve the existing 15-second `useDelegationRunSnapshot` cache refresh behavior.
- Preserve `graph_revision`, request-generation, and activation-epoch gates. Durable refresh may change when a fetch is requested, never which snapshot is allowed to win.
- Preserve the existing 10-minute fallback for expanded interest and undiscovered overlay interest. A discovered, settled, overlay-only graph owns no refresh timer.
- Required listener recovery covers only `workflow_graph://changed` and `workflow_graph://compatibility_nudge`. `completion_decision_resolved` remains optional and never keeps the retry loop alive.
- Use one required-listener retry timer per install generation, one fallback/authority timer per active conversation, and dispose late listener results from prior generations immediately.
- Implementers write regression tests before production changes, but MUST NOT execute `pnpm test`, Vitest, `cargo test`, ESLint, or builds during Tasks 1-4. All test, lint, and build execution is deferred to Task 5, unless the parent explicitly declares pipeline end-of-life earlier.
- A deferred red/green command is documentation of the intended TDD checkpoint, not authorization to run it. Producer commits are allowed before those commands execute.
- Do not add mid-plan human acceptance, manual click-through, or user sign-off gates. Producer review packages flow directly to the next task; human acceptance is post-delivery only.
- Use PowerShell syntax from `D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing`. Stage only task-owned files. Create local commits, but do not push, merge, rebase, or open a pull request.
- Keep Prettier conventions: no semicolons, 2-space indentation, 80-column formatting, strict types, and no unused symbols.
- Add no dependencies and do not regenerate lockfiles.

### Risk Policy

Policy version: `b2d_task_risk_v1`.

- Hard triggers always produce `high`: `concurrency_lifecycle`, `security_trust_boundary`, `migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`, `update_rollback`.
- Soft signals sum once each: `cross_runtime_or_process=2`; `broad_production_surface=1`; `multiple_ownership_modules=1`; `shared_interface=1`; `dependency_or_build=1`; `multi_layer_without_test_seam=1`.
- Soft total `>=3` produces `high`; totals `0-2` produce `normal` when no hard trigger applies.
- Route `normal` tasks to implementer `grok` with reviewer `codex`.
- Route `high` tasks to implementer `codex` with reviewers `codex + grok`.

## Task Routing Matrix

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Reconcile delegation cards from exact terminal run snapshots | `use-delegation-card-model` source and tests | `concurrency_lifecycle`: asynchronously arriving live and durable lifecycle sources; `security_trust_boundary`: task identity decides whether one run may replace another run's fields | `broad_production_surface=1`, `shared_interface=1`; total `2` | `high`: hard triggers apply to lifecycle precedence and fail-closed identity | `codex` | `codex + grok` | `b2d_task_risk_v1` |
| 2 | Add active workflow authority refresh scheduling | `workflow-graph-store` scheduler and timer tests | `concurrency_lifecycle`: timer ownership, fetch completion, request generation, and activation epochs race across lease changes | `broad_production_surface=1`, `shared_interface=1`; total `2` | `high`: timer and async lifecycle hard trigger | `codex` | `codex + grok` | `b2d_task_risk_v1` |
| 3 | Recover required workflow event subscriptions | `workflow-graph-store` listener install/disposal and tests | `concurrency_lifecycle`: pending subscription promises, retry timers, and install-generation disposal | `cross_runtime_or_process=2`, `broad_production_surface=1`, `shared_interface=1`; total `4` | `high`: hard trigger applies; transport-spanning soft total also exceeds threshold | `codex` | `codex + grok` | `b2d_task_risk_v1` |
| 4 | Aggregate the pre-final delivery and scope audit | committed frontend diff across both ownership modules | none | `multiple_ownership_modules=1`; total `1` | `normal`: read-only aggregation has one soft signal and no hard trigger | `grok` | `codex` | `b2d_task_risk_v1` |
| 5 | Run final automated verification, review, and delivery | both targeted suites, full frontend suite, lint, build, final diff | none | `broad_production_surface=1`, `multiple_ownership_modules=1`, `dependency_or_build=1`; total `3` | `high`: aggregate verification crosses the soft threshold | `codex` | `codex + grok` | `b2d_task_risk_v1` |

## File Structure

| File | Responsibility in this change |
| --- | --- |
| `src/hooks/use-delegation-card-model.ts` | Select identity-compatible effective binding/meta/snapshot sources once, then use them for every run-scoped card field while leaving snapshot polling and binding scoping intact. |
| `src/hooks/use-delegation-card-model.test.ts` | Prove terminal snapshot convergence, exact identity isolation, coherent terminal fields, and live-terminal precedence. |
| `src/lib/workflow-graph-store.ts` | Select 15-second, 10-minute, or no per-conversation refresh; track required channel subscription state; warn, retry, and dispose by install generation. |
| `src/lib/workflow-graph-store.test.ts` | Prove fast authority convergence, settled/discovery/expanded timer behavior, required-listener retry without duplication, warning latches, and final lease cleanup. |

## Design Traceability

| Design requirement | Producer task | Final evidence |
| --- | --- | --- |
| Exact-run terminal snapshot can close stale running binding/meta | Task 1 | Targeted delegation suite in Task 5 |
| Live terminal binding, then live terminal meta, remain highest precedence | Task 1 | Explicit binding and meta precedence regressions |
| Mismatch or missing live task identity fails closed | Task 1 | Mismatch and missing-ID regressions |
| Stale source is omitted for every run-scoped field | Task 1 | One coherent completed-card assertion set and one failed-meta assertion set |
| Preserve card binding scope and existing snapshot polling | Task 1 | Source review plus unchanged hook interval/scoping paths |
| Active numbered overlay refreshes every 15 seconds | Task 2 | Fake-timer convergence regression |
| Four node states and `overall_state=in_progress` qualify | Task 2 | Table-driven fake-timer regression |
| Settled overlay stops fast refresh; expanded/discovery keep 10 minutes | Task 2 | Settled, expanded, and discovery regressions |
| Required channel failures warn once and retry after 5 seconds | Task 3 | Warning/retry regression |
| Successful sibling listener is not duplicated | Task 3 | Mixed-success retry regression |
| Final lease release clears timers, latches, pending state, and late results | Task 3 | Timer-count, reactivation, and existing generation regressions |
| No backend or protocol change | Tasks 4-5 | Exact changed-file allowlist and final diff review |

---

### Task 1: Reconcile Delegation Cards from Exact Terminal Run Snapshots

**Files:**

- Modify: `src/hooks/use-delegation-card-model.ts` (`effectiveDelegationMeta`, `buildDelegationCardModel`, `useDelegationCardModel`)
- Test: `src/hooks/use-delegation-card-model.test.ts` (`buildDelegationCardModel — merge precedence`)

**Interfaces:**

- Consumes: `DelegationBinding.status/taskId`, `ParsedMeta.status/taskId/syntheticHistorical`, and `DelegationRunSnapshot.status/task_id`.
- Produces: private `EffectiveDelegationSources` with `binding`, `parsedMeta`, and `runSnapshot`; a mismatched snapshot becomes `null`, a matching terminal snapshot removes stale running sources, a terminal binding remains first, and a terminal meta removes a running binding.
- Preserves: exported `buildDelegationCardModel(...)`, `scopeDelegationBindingForCard(...)`, `DelegationCardModel`, and the `useDelegationRunSnapshot` interval contract.

**Task Routing Matrix:**

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Reconcile delegation cards from exact terminal run snapshots | hook source and focused test | `concurrency_lifecycle`: live/durable arrival order; `security_trust_boundary`: exact task identity gates cross-run replacement | `broad_production_surface=1`, `shared_interface=1`; total `2` | `high`: two hard triggers | `codex` | `codex + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Add a complete run-snapshot fixture**

Add `DelegationRunSnapshot` to the existing type import from `@/lib/types`, then add this helper after `projection(...)`:

```ts
function runSnapshot(
  overrides: Partial<DelegationRunSnapshot> = {}
): DelegationRunSnapshot {
  return {
    task_id: "task-1",
    root_task_id: "task-1",
    previous_task_id: null,
    generation: 1,
    parent_tool_use_id: "pt-1",
    child_conversation_id: 123,
    agent_type: "grok",
    profile_id: null,
    task_preview: "durable task",
    status: "running",
    error_code: null,
    started_at: STARTED_AT,
    finished_at: null,
    runtime_stats: RUNNING_SUMMARY_STATS,
    card_summary: null,
    child_turn_anchor: null,
    replaced_task_id: null,
    replacement_reason: null,
    ...overrides,
  }
}
```

- [ ] **Step 2: Write the delegation precedence regressions before production edits**

Add these tests inside `describe("buildDelegationCardModel — merge precedence", ...)` at the existing block beginning near line 178:

```ts
it("matching completed snapshot replaces every stale running binding field", () => {
  const terminalSummary: CardSummary = {
    kind: "implementation",
    phase: "implementation",
    status: "done",
    summary: "Durable run completed.",
  }
  const model = build({
    parsedInput: parseInput(null),
    binding: binding({
      taskId: "task-1",
      status: "running",
      agentType: "codex",
      task: "stale live task",
      childConnectionId: "stale-connection",
      childConversationId: 99,
      runtimeStats: RUNNING_SUMMARY_STATS,
      attentionRequest: ATTENTION,
      errorCode: "stale-running-error",
      completedDurationMs: 45_000,
      cardSummary: null,
    }),
    runSnapshot: runSnapshot({
      task_id: "task-1",
      generation: 4,
      status: "completed",
      agent_type: "grok",
      task_preview: "durable task",
      child_conversation_id: 123,
      runtime_stats: LIVE_STATS,
      finished_at: FINISHED_AT,
      card_summary: terminalSummary,
    }),
  })

  expect(model).toMatchObject({
    lifecycleStatus: "ok",
    status: "ok",
    brokerTaskId: "task-1",
    generation: 4,
    agentType: "grok",
    task: "durable task",
    childConversationId: 123,
    childConnectionId: null,
    runtimeStats: LIVE_STATS,
    finishedAt: FINISHED_AT,
    attentionRequest: null,
    completedDurationMs: null,
    errorCode: undefined,
    cardSummary: terminalSummary,
  })
  expect(model.toolCallCount).toBe(12)
  expect(isTickerEligible(model)).toBe(false)
})

it("matching failed snapshot replaces stale running live meta as an error", () => {
  const model = build({
    parsedInput: parseInput(null),
    parsedMeta: meta({
      taskId: "task-1",
      status: "running",
      task: "stale meta task",
      childConnectionId: "stale-meta-connection",
      childConversationId: 42,
      runtimeStats: RUNNING_SUMMARY_STATS,
      attentionRequest: ATTENTION,
      errorCode: "stale-meta-error",
    }),
    runSnapshot: runSnapshot({
      task_id: "task-1",
      generation: 2,
      status: "failed",
      error_code: "durable_child_failed",
      task_preview: "durable failed task",
      child_conversation_id: 123,
      runtime_stats: LIVE_STATS,
      finished_at: FINISHED_AT,
    }),
  })

  expect(model).toMatchObject({
    lifecycleStatus: "err",
    status: "err",
    brokerTaskId: "task-1",
    generation: 2,
    task: "durable failed task",
    childConversationId: 123,
    childConnectionId: null,
    runtimeStats: LIVE_STATS,
    finishedAt: FINISHED_AT,
    attentionRequest: null,
    errorCode: "durable_child_failed",
  })
  expect(isTickerEligible(model)).toBe(false)
})

it("mismatched terminal snapshot cannot change a running binding", () => {
  const model = build({
    parsedInput: parseInput(null),
    binding: binding({
      taskId: "task-1",
      status: "running",
      task: "live task",
      runtimeStats: RUNNING_SUMMARY_STATS,
    }),
    runSnapshot: runSnapshot({
      task_id: "task-other",
      generation: 9,
      status: "completed",
      runtime_stats: LIVE_STATS,
      finished_at: FINISHED_AT,
      card_summary: {
        kind: "implementation",
        phase: "implementation",
        status: "done",
        summary: "Wrong run.",
      },
    }),
  })

  expect(model.lifecycleStatus).toBe("running")
  expect(model.status).toBe("active")
  expect(model.brokerTaskId).toBe("task-1")
  expect(model.generation).toBeNull()
  expect(model.task).toBe("live task")
  expect(model.agentType).toBe("codex")
  expect(model.childConnectionId).toBe("c1")
  expect(model.runtimeStats).toEqual(RUNNING_SUMMARY_STATS)
  expect(model.finishedAt).toBeNull()
  expect(model.cardSummary).toBeNull()
})

it("terminal snapshot fails closed when live meta has no task id", () => {
  const model = build({
    parsedInput: parseInput(null),
    parsedMeta: meta({
      taskId: null,
      status: "running",
      task: "identity-less meta task",
      runtimeStats: RUNNING_SUMMARY_STATS,
    }),
    runSnapshot: runSnapshot({
      task_id: "task-1",
      status: "completed",
      runtime_stats: LIVE_STATS,
      finished_at: FINISHED_AT,
    }),
  })

  expect(model.lifecycleStatus).toBe("running")
  expect(model.brokerTaskId).toBeNull()
  expect(model.task).toBe("identity-less meta task")
  expect(model.childConnectionId).toBe("c-meta")
  expect(model.runtimeStats).toEqual(RUNNING_SUMMARY_STATS)
  expect(model.finishedAt).toBeNull()
})

it("terminal live meta outranks a running binding and running snapshot", () => {
  const model = build({
    parsedInput: parseInput(null),
    binding: binding({
      taskId: "task-1",
      status: "running",
      task: "stale binding task",
      runtimeStats: RUNNING_SUMMARY_STATS,
    }),
    parsedMeta: meta({
      taskId: "task-1",
      status: "err",
      task: "terminal meta task",
      errorCode: "live_meta_failed",
      finishedAt: FINISHED_AT,
      runtimeStats: LIVE_STATS,
      attentionRequest: null,
    }),
    runSnapshot: runSnapshot({
      task_id: "task-1",
      status: "running",
      runtime_stats: RUNNING_SUMMARY_STATS,
    }),
  })

  expect(model.lifecycleStatus).toBe("err")
  expect(model.status).toBe("err")
  expect(model.task).toBe("terminal meta task")
  expect(model.errorCode).toBe("live_meta_failed")
  expect(model.childConnectionId).toBe("c-meta")
  expect(model.runtimeStats).toEqual(LIVE_STATS)
  expect(model.finishedAt).toBe(FINISHED_AT)
})

it("terminal live binding outranks a lower running snapshot", () => {
  const liveSummary: CardSummary = {
    kind: "review",
    verdict: "approve",
    critical: 0,
    important: 0,
    minor: 0,
    summary: "Live completion wins.",
  }
  const model = build({
    parsedInput: parseInput(null),
    binding: binding({
      taskId: "task-1",
      status: "ok",
      runtimeStats: LIVE_STATS,
      finishedAt: FINISHED_AT,
      attentionRequest: null,
      cardSummary: liveSummary,
    }),
    runSnapshot: runSnapshot({
      task_id: "task-1",
      status: "running",
      error_code: null,
      runtime_stats: RUNNING_SUMMARY_STATS,
      card_summary: {
        kind: "review",
        verdict: "block",
        critical: 1,
        important: 0,
        minor: 0,
        summary: "Lower running data.",
      },
    }),
  })

  expect(model.lifecycleStatus).toBe("ok")
  expect(model.status).toBe("ok")
  expect(model.errorCode).toBeUndefined()
  expect(model.runtimeStats).toEqual(LIVE_STATS)
  expect(model.cardSummary).toEqual(liveSummary)
})
```

- [ ] **Step 3: Record the deferred red checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/hooks/use-delegation-card-model.test.ts
```

Expected red result before production edits: the completed and failed snapshot tests remain `running`, terminal meta loses to the running binding, or mismatched snapshot fields leak into the card.

- [ ] **Step 4: Add one identity-safe effective-source selector**

Replace `effectiveDelegationMeta(...)` and add the selector immediately after it:

```ts
function effectiveDelegationMeta(
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null
): ParsedMeta | null {
  return parsedMeta?.syntheticHistorical && runSnapshot ? null : parsedMeta
}

type EffectiveDelegationSources = {
  binding: DelegationBinding | undefined
  parsedMeta: ParsedMeta | null
  runSnapshot: DelegationRunSnapshot | null
}

function isTerminalRunSnapshot(snapshot: DelegationRunSnapshot): boolean {
  return (
    snapshot.status === "completed" ||
    snapshot.status === "failed" ||
    snapshot.status === "canceled"
  )
}

function effectiveDelegationSources(
  binding: DelegationBinding | undefined,
  parsedMeta: ParsedMeta | null,
  runSnapshot: DelegationRunSnapshot | null
): EffectiveDelegationSources {
  const availableTaskIds = [
    ...(binding ? [binding.taskId] : []),
    ...(parsedMeta ? [parsedMeta.taskId] : []),
  ]
  const snapshotMatches =
    runSnapshot == null ||
    availableTaskIds.every(
      (taskId) => taskId != null && taskId === runSnapshot.task_id
    )
  const effectiveRunSnapshot = snapshotMatches ? runSnapshot : null
  const effectiveMeta = effectiveDelegationMeta(
    parsedMeta,
    effectiveRunSnapshot
  )

  if (binding && binding.status !== "running") {
    return {
      binding,
      parsedMeta: effectiveMeta,
      runSnapshot: effectiveRunSnapshot,
    }
  }

  if (effectiveMeta && effectiveMeta.status !== "running") {
    return {
      binding: undefined,
      parsedMeta: effectiveMeta,
      runSnapshot: effectiveRunSnapshot,
    }
  }

  if (effectiveRunSnapshot && isTerminalRunSnapshot(effectiveRunSnapshot)) {
    return {
      binding: undefined,
      parsedMeta: null,
      runSnapshot: effectiveRunSnapshot,
    }
  }

  return {
    binding,
    parsedMeta: effectiveMeta,
    runSnapshot: effectiveRunSnapshot,
  }
}
```

The empty `availableTaskIds` case intentionally accepts a standalone durable snapshot. A present binding or meta contributes an identity and must contribute a non-null exact match.

- [ ] **Step 5: Feed only effective sources into every run-scoped merge**

At the start of `buildDelegationCardModel`, derive the three effective inputs before computing `knownTaskId`:

```ts
const {
  binding: effectiveBinding,
  parsedMeta: effectiveMeta,
  runSnapshot: effectiveRunSnapshot,
} = effectiveDelegationSources(binding, parsedMeta, runSnapshot)

const knownTaskId =
  effectiveBinding?.taskId ??
  effectiveMeta?.taskId ??
  effectiveRunSnapshot?.task_id ??
  displayTaskId ??
  null
```

Use these exact inputs for lifecycle, badge, and field selectors:

```ts
const lifecycleStatus = resolveLifecycleStatus({
  binding: effectiveBinding,
  parsedMeta: effectiveMeta,
  runSnapshot: effectiveRunSnapshot,
  childProjection: runScopedProjection,
  toolOutput,
  state,
  errorText,
})

const status =
  !effectiveBinding && !effectiveMeta && effectiveRunSnapshot
    ? cardStatusFromLifecycle(lifecycleStatus)
    : resolveDelegationStatus({
        binding: effectiveBinding,
        parsedMeta: effectiveMeta,
        toolOutput,
        state,
        errorText,
        childAwaitingPermission:
          !uncorrelatedFailure && childAwaitingPermission,
        childTaskStatus: runScopedProjection?.taskStatus ?? null,
      })

const runtimeStats = pickRuntimeStats(
  effectiveBinding,
  effectiveMeta,
  effectiveRunSnapshot,
  runScopedProjection
)
const attentionRequest = pickAttentionRequest(
  effectiveBinding,
  effectiveMeta,
  effectiveRunSnapshot,
  runScopedProjection
)
const startedAt = pickStartedAt(
  effectiveBinding,
  effectiveMeta,
  effectiveRunSnapshot,
  runScopedProjection,
  runtimeStats
)
const finishedAt = pickFinishedAt(
  effectiveBinding,
  effectiveMeta,
  effectiveRunSnapshot,
  runScopedProjection,
  runtimeStats,
  lifecycleStatus
)
const completedDurationMs = pickCompletedDurationMs(
  effectiveBinding,
  toolOutput
)
```

Replace the remaining run-scoped expressions with these exact forms:

```ts
const brokerTaskId =
  effectiveBinding?.taskId ??
  effectiveMeta?.taskId ??
  effectiveRunSnapshot?.task_id ??
  runScopedProjection?.taskId ??
  null

const childConnectionId = uncorrelatedFailure
  ? null
  : (effectiveBinding?.childConnectionId ??
    effectiveMeta?.childConnectionId ??
    null)
const childConversationId = uncorrelatedFailure
  ? null
  : (effectiveBinding?.childConversationId ??
    effectiveMeta?.childConversationId ??
    effectiveRunSnapshot?.child_conversation_id ??
    toolOutput?.childConversationId ??
    scopedChildProjection?.childConversationId ??
    null)

const agentType: AgentType | null =
  effectiveBinding?.agentType ??
  parsedInput.agentType ??
  agentTypeFromRunSnapshot(effectiveRunSnapshot)

const toolErrorCode =
  toolOutput?.kind === "outcome" ? toolOutput.errorCode : null
const errorCode =
  effectiveBinding?.errorCode ??
  effectiveMeta?.errorCode ??
  effectiveRunSnapshot?.error_code ??
  runScopedProjection?.errorCode ??
  toolErrorCode ??
  undefined

const task =
  parsedInput.task ??
  effectiveBinding?.task ??
  effectiveMeta?.task ??
  effectiveRunSnapshot?.task_preview ??
  null
```

Keep original `binding` and `parsedMeta` only in the `hasModel` visibility check. Update returned run-scoped fields exactly:

```ts
generation:
  effectiveRunSnapshot?.generation ?? effectiveMeta?.generation ?? null,
cardSummary:
  effectiveBinding?.cardSummary ??
  effectiveRunSnapshot?.card_summary ??
  null,
isReplacement: Boolean(effectiveRunSnapshot?.replaced_task_id),
childTurnAnchor: effectiveRunSnapshot?.child_turn_anchor ?? null,
```

- [ ] **Step 6: Make work-unit live-binding bookkeeping use the same selection**

In `useDelegationCardModel`, after `runSnapshot` is read, add:

```ts
const hasEffectiveLiveBinding = useMemo(
  () =>
    effectiveDelegationSources(binding, parsedMeta, runSnapshot).binding !=
    null,
  [binding, parsedMeta, runSnapshot]
)
```

Pass `hasEffectiveLiveBinding` to both `mergeDelegationWorkUnitModel(...)` calls instead of `binding != null`, and add it to the final `useMemo` dependency list. Keep the child connection/projection lookup and `scopeDelegationBindingForCard` call unchanged.

- [ ] **Step 7: Record the deferred green checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/hooks/use-delegation-card-model.test.ts
```

Expected green result at final verification: all existing and new tests in the file pass.

- [ ] **Step 8: Commit the producer task and prepare its review package**

```powershell
git add src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts
git commit -m "fix: reconcile delegation cards from run snapshots"
git show --stat --oneline HEAD
git diff HEAD^ -- src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts
```

Expected package: one commit containing only the hook and its focused test. Queue it to `codex + grok` reviewers with the Task 1 risk row, then continue to Task 2 without a human gate.

### Task 2: Add Active Workflow Authority Refresh Scheduling

**Files:**

- Modify: `src/lib/workflow-graph-store.ts` (refresh constants, `runRefresh`, fallback eligibility helper)
- Test: `src/lib/workflow-graph-store.test.ts` (snapshot helpers and `active workflow refresh scheduling`)

**Interfaces:**

- Consumes: `WorkflowGraphSnapshot.overall_state`, `WorkflowNodeSnapshot.status`, `ActiveConversationRecord` counts/epoch, and the existing entry revision.
- Produces: private `hasActiveWorkflowState(snapshot): boolean` and `nextAuthorityRefreshDelay(conversationId, epoch, get): number | null`.
- Delay contract: `15_000` for active state; otherwise `600_000` for expanded or undiscovered interest; otherwise `null`.
- Preserves: one `fallbackTimer` field per active conversation and all request-generation/revision/activation-epoch guards.

**Task Routing Matrix:**

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2 | Add active workflow authority refresh scheduling | graph store scheduler and fake-timer tests | `concurrency_lifecycle`: timer callbacks and async fetch completions cross lease epochs | `broad_production_surface=1`, `shared_interface=1`; total `2` | `high`: hard trigger | `codex` | `codex + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Add explicit active and settled graph fixtures**

Add these helpers after `baseSnapshot(...)` and `node(...)` are defined:

```ts
function activeSnapshot(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  return baseSnapshot({
    overall_state: "in_progress",
    current_phase_id: "tasks",
    current_node_ids: ["n-task-active"],
    nodes: [
      node({
        node_id: "n-task-active",
        phase_id: "tasks",
        role: "implementer",
        status: "running",
        is_observed: true,
        latest_child_conversation_id: 42,
      }),
    ],
    gates: [],
    ...overrides,
  })
}

function settledSnapshot(
  overrides: Partial<WorkflowGraphSnapshot> = {}
): WorkflowGraphSnapshot {
  return baseSnapshot({
    overall_state: "completed",
    current_phase_id: "final",
    current_node_ids: [],
    nodes: [
      node({
        node_id: "n-final-complete",
        phase_id: "final",
        role: "reviewer",
        status: "completed",
        is_observed: true,
        latest_child_conversation_id: 42,
      }),
    ],
    gates: [],
    ...overrides,
  })
}
```

- [ ] **Step 2: Write the fast convergence and stop regressions**

Add these tests at the start of `describe("active workflow refresh scheduling", ...)`:

```ts
it("active numbered overlay converges after 15 seconds and stops when settled", async () => {
  const active = activeSnapshot({ graph_revision: 2 })
  const settled = settledSnapshot({ graph_revision: 3 })
  useWorkflowGraphStore.getState().applyFromDetail(201, active)
  getWorkflowGraphSnapshot
    .mockResolvedValueOnce(active)
    .mockResolvedValueOnce(settled)

  const release = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(201)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(14_999)
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  await flushMicrotasks()

  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(201)?.graph_revision
  ).toBe(3)
  expect(
    useWorkflowGraphStore.getState().getSnapshot(201)?.overall_state
  ).toBe("completed")

  await vi.advanceTimersByTimeAsync(20 * 60 * 1_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  release()
})

it.each([
  [
    "reserving node",
    settledSnapshot({
      graph_revision: 4,
      overall_state: "approved",
      nodes: [node({ node_id: "active", status: "reserving" })],
    }),
  ],
  [
    "running node",
    settledSnapshot({
      graph_revision: 4,
      overall_state: "approved",
      nodes: [node({ node_id: "active", status: "running" })],
    }),
  ],
  [
    "waiting_review node",
    settledSnapshot({
      graph_revision: 4,
      overall_state: "approved",
      nodes: [node({ node_id: "active", status: "waiting_review" })],
    }),
  ],
  [
    "waiting_adjudication node",
    settledSnapshot({
      graph_revision: 4,
      overall_state: "approved",
      nodes: [node({ node_id: "active", status: "waiting_adjudication" })],
    }),
  ],
  [
    "in_progress graph without active rows",
    settledSnapshot({
      graph_revision: 4,
      overall_state: "in_progress",
      nodes: [],
    }),
  ],
])("uses the 15-second authority timer for %s", async (_label, snapshot) => {
  getWorkflowGraphSnapshot.mockResolvedValue(snapshot)

  const release = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(202)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(14_999)
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  release()
})
```

- [ ] **Step 3: Make legacy timer tests explicit about settled state**

Keep `baseSnapshot()` unchanged because phase-rail tests rely on its running reviewer. In the scheduling block, use `settledSnapshot(...)` for every mocked or seeded numbered graph whose test expects a 10-minute interval or no overlay timer.

Apply the replacement in these exact existing tests:

```text
overlay-only interest handles newer events without a fallback timer
overlay-only discovery re-arms the 10-minute fallback until a graph appears
seeds the first publish event after an overlay mount resolves null
releasing expanded interest keeps overlay events but stops fallback
late expanded completion updates cache but cannot arm an overlay timer
refreshes every ten minutes and resets the clock after event convergence
equal and lower graph revisions neither fetch nor reset the timer
a compatibility nudge fetches only while active and resets fallback from completion
one of two leases keeps event and fallback eligibility
pending-readiness duplicate leases share one fallback after the creator releases
a current response behind a newer cached revision still rearms fallback
old activation epoch completion cannot arm a reactivated epoch timer
null preserves an existing graph but remains empty without a cache
failed refresh retains the graph and retries only at the next interval
failed and stale compatibility completions follow the common scheduler
subscription failures still allow initial and periodic refresh
```

For the null/observed-only test, use this exact settled seed so active rows do not accidentally select 15 seconds:

```ts
settledSnapshot({
  graph_revision: null,
  compatibility: "observed_only",
  overall_state: "observed_only",
  workflow_id: null,
})
```

Rename the first scheduling test and use settled fixtures throughout it:

```ts
it("settled numbered overlay handles newer events without a timer", async () => {
  useWorkflowGraphStore
    .getState()
    .applyFromDetail(92, settledSnapshot({ graph_revision: 2 }))
  getWorkflowGraphSnapshot.mockResolvedValue(
    settledSnapshot({ graph_revision: 3 })
  )
  const release = useWorkflowGraphStore.getState().activateOverlayInterest(92)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  useWorkflowGraphStore.getState().handleGraphChanged({
    parent_conversation_id: 92,
    workflow_id: "wf-1",
    graph_revision: 3,
  })
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(20 * 60 * 1_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
  release()
})
```

- [ ] **Step 4: Record the deferred red checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected red result before production edits: the active overlay has no 15-second timer and the active-state table does not refetch.

- [ ] **Step 5: Implement delay selection and reuse the existing timer owner**

Update the constants and replace `needsFallbackRefresh(...)` with:

```ts
const ACTIVE_AUTHORITY_REFRESH_MS = 15_000
const FALLBACK_REFRESH_MS = 10 * 60 * 1_000

function hasActiveWorkflowState(
  snapshot: WorkflowGraphSnapshot | null
): boolean {
  if (!snapshot) return false
  if (snapshot.overall_state === "in_progress") return true
  return snapshot.nodes.some(
    (node) =>
      node.status === "reserving" ||
      node.status === "running" ||
      node.status === "waiting_review" ||
      node.status === "waiting_adjudication"
  )
}

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
  if (hasExpandedInterestEpoch(conversationId, epoch)) {
    return FALLBACK_REFRESH_MS
  }
  return entry?.appliedGraphRevision == null ? FALLBACK_REFRESH_MS : null
}
```

After a current fetch completes in `runRefresh(...)`, replace the fixed fallback scheduling block with:

```ts
const delay = nextAuthorityRefreshDelay(
  conversationId,
  activationEpoch,
  get
)
if (delay == null) return

currentActive.fallbackTimer = setTimeout(() => {
  if (
    nextAuthorityRefreshDelay(conversationId, activationEpoch, get) == null
  ) {
    return
  }
  void get().refresh(conversationId)
}, delay)
```

Update the file header and scheduler comments to state that active authority uses 15 seconds, expanded/undiscovered fallback uses 10 minutes, and settled discovered overlay uses no timer. Do not add a second timer field.

- [ ] **Step 6: Record the deferred green checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected green result at final verification: active graphs refetch at 15 seconds, settle without rearming, and all explicit 10-minute cases retain their prior behavior.

- [ ] **Step 7: Commit the producer task and prepare its review package**

```powershell
git add src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
git commit -m "fix: refresh active workflow graphs from authority"
git show --stat --oneline HEAD
git diff HEAD^ -- src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
```

Expected package: one commit containing only the graph store and tests, with no new timer owner. Queue it to `codex + grok` reviewers with the Task 2 risk row, then continue to Task 3 without a human gate.

### Task 3: Recover Required Workflow Event Subscriptions

**Files:**

- Modify: `src/lib/workflow-graph-store.ts` (API imports, required listener slots, install, retry, disposal)
- Test: `src/lib/workflow-graph-store.test.ts` (`workflow activation lifecycle`, scheduling failure case)

**Interfaces:**

- Consumes: existing async subscription functions and active-interest map.
- Produces: two `RequiredListenerSlot` records keyed by `graphChanged` and `compatibilityNudge`; one `requiredListenerRetryTimer`; 5-second retry of missing non-pending slots.
- Warning contract: one `console.warn` per failed channel per install generation with `{ channel, error: toErrorMessage(error) }`; retry failures stay latched; success and final disposal reset the latch.
- Preserves: optional completion listener behavior, readiness deadline, install-generation monotonicity, initial authoritative refresh, and exactly one stored disposer per successful channel.
- Consumes from Task 2 tests: `activeSnapshot(...)` and `settledSnapshot(...)`.

**Task Routing Matrix:**

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 3 | Recover required workflow event subscriptions | listener lifecycle and fake-timer tests | `concurrency_lifecycle`: retry/pending/install-generation races | `cross_runtime_or_process=2`, `broad_production_surface=1`, `shared_interface=1`; total `4` | `high`: hard trigger and soft threshold | `codex` | `codex + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Export production event names from the explicit API mock**

Replace the hoisted mock setup and `vi.mock("@/lib/api", ...)` factory at the top of `src/lib/workflow-graph-store.test.ts` with this complete block before changing the store imports. The store and warning assertions then consume the same hoisted production strings:

```ts
const {
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
  subscribeWorkflowGraphChanged,
  subscribeWorkflowCompatibilityNudge,
} = vi.hoisted(() => ({
  WORKFLOW_GRAPH_CHANGED_EVENT: "workflow_graph://changed",
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT:
    "workflow_graph://compatibility_nudge",
  getWorkflowGraphSnapshot: vi.fn(),
  subscribeCompletionDecisionResolved: vi.fn(async () => () => {}),
  subscribeWorkflowGraphChanged: vi.fn(async () => () => {}),
  subscribeWorkflowCompatibilityNudge: vi.fn(async () => () => {}),
}))

// Pass hoisted mocks through directly — do not re-wrap with `...args: unknown[]`
// spreads (TS2556: spread of unknown[] is not a rest tuple).
vi.mock("@/lib/api", () => ({
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
  subscribeWorkflowGraphChanged,
  subscribeWorkflowCompatibilityNudge,
}))
```

- [ ] **Step 2: Write warning, retry, sibling, and final-release regressions**

Add these tests inside `describe("workflow activation lifecycle", ...)`:

```ts
it("warns once per required channel and shares one five-second retry timer", async () => {
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
  subscribeWorkflowGraphChanged.mockRejectedValue(
    new Error("changed unavailable")
  )
  subscribeWorkflowCompatibilityNudge.mockRejectedValue(
    new Error("nudge unavailable")
  )
  getWorkflowGraphSnapshot.mockResolvedValue(
    settledSnapshot({ graph_revision: 2 })
  )

  const release = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(301)
  try {
    await flushMicrotasks()
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
    expect(warn).toHaveBeenCalledWith(
      "[workflow-graph-store] required event subscription failed",
      {
        channel: WORKFLOW_GRAPH_CHANGED_EVENT,
        error: "changed unavailable",
      }
    )
    expect(warn).toHaveBeenCalledWith(
      "[workflow-graph-store] required event subscription failed",
      {
        channel: WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
        error: "nudge unavailable",
      }
    )
    expect(warn).toHaveBeenCalledTimes(2)
    expect(vi.getTimerCount()).toBe(1)

    await vi.advanceTimersByTimeAsync(4_999)
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    await flushMicrotasks()

    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
    expect(warn).toHaveBeenCalledTimes(2)
    expect(vi.getTimerCount()).toBe(1)
  } finally {
    release()
    warn.mockRestore()
  }
})

it("retries only the missing required listener and retains its sibling", async () => {
  const changedDispose = vi.fn()
  const nudgeDispose = vi.fn()
  subscribeWorkflowGraphChanged
    .mockRejectedValueOnce(new Error("changed unavailable"))
    .mockResolvedValueOnce(changedDispose)
  subscribeWorkflowCompatibilityNudge.mockResolvedValue(nudgeDispose)
  getWorkflowGraphSnapshot.mockResolvedValue(
    settledSnapshot({ graph_revision: 2 })
  )

  const release = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(302)
  await flushMicrotasks()
  expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
  expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(5_000)
  await flushMicrotasks()
  expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
  expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)
  expect(changedDispose).not.toHaveBeenCalled()
  expect(nudgeDispose).not.toHaveBeenCalled()

  release()
  expect(changedDispose).toHaveBeenCalledTimes(1)
  expect(nudgeDispose).toHaveBeenCalledTimes(1)
})

it("final lease release clears retry, refresh, and warning generation state", async () => {
  const warn = vi.spyOn(console, "warn").mockImplementation(() => {})
  subscribeWorkflowGraphChanged.mockRejectedValue(
    new Error("changed unavailable")
  )
  subscribeWorkflowCompatibilityNudge.mockRejectedValue(
    new Error("nudge unavailable")
  )
  getWorkflowGraphSnapshot.mockResolvedValue(
    activeSnapshot({ graph_revision: 2 })
  )

  const firstRelease = useWorkflowGraphStore
    .getState()
    .activateOverlayInterest(303)
  try {
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(vi.getTimerCount()).toBe(2)

    firstRelease()
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(15_000)
    await flushMicrotasks()
    expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(1)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(1)

    const secondRelease = useWorkflowGraphStore
      .getState()
      .activateOverlayInterest(303)
    await flushMicrotasks()
    expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
    expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
    expect(warn).toHaveBeenCalledTimes(4)
    secondRelease()
    expect(vi.getTimerCount()).toBe(0)
  } finally {
    warn.mockRestore()
  }
})
```

Keep the existing pending-listener, late-success, stale-lease, and install-generation tests. They continue to prove that a late successful subscription from an old generation calls its returned disposer instead of overwriting the active generation.

- [ ] **Step 3: Bound the existing durable-polling failure regression**

Replace the existing `subscription failures still allow initial and periodic refresh` test with this version. The first retry becomes pending so advancing ten minutes does not manufacture 120 immediate retry failures:

```ts
it("subscription failures still allow initial and periodic refresh", async () => {
  const pendingChanged = deferred<() => void>()
  const pendingNudge = deferred<() => void>()
  subscribeWorkflowGraphChanged
    .mockRejectedValueOnce(new Error("graph events unavailable"))
    .mockReturnValueOnce(pendingChanged.promise)
  subscribeWorkflowCompatibilityNudge
    .mockRejectedValueOnce(new Error("nudge events unavailable"))
    .mockReturnValueOnce(pendingNudge.promise)
  getWorkflowGraphSnapshot.mockResolvedValue(
    settledSnapshot({ graph_revision: 2 })
  )

  const release = useWorkflowGraphStore.getState().activateConversation(85)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(1)

  await vi.advanceTimersByTimeAsync(10 * 60 * 1_000)
  await flushMicrotasks()
  expect(getWorkflowGraphSnapshot).toHaveBeenCalledTimes(2)
  expect(subscribeWorkflowGraphChanged).toHaveBeenCalledTimes(2)
  expect(subscribeWorkflowCompatibilityNudge).toHaveBeenCalledTimes(2)
  release()
})
```

- [ ] **Step 4: Record the deferred red checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected red result before production edits: failed subscriptions never retry, warnings are absent, and retry timer assertions fail.

- [ ] **Step 5: Import channel names and normalized error formatting**

Replace the API import with:

```ts
import {
  WORKFLOW_GRAPH_CHANGED_EVENT,
  WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
  getWorkflowGraphSnapshot,
  subscribeCompletionDecisionResolved,
  subscribeWorkflowCompatibilityNudge,
  subscribeWorkflowGraphChanged,
} from "@/lib/api"
import type { CompletionDecisionResolvedEventPayload } from "@/lib/api"
import { toErrorMessage } from "@/lib/app-error"
```

- [ ] **Step 6: Add required listener slot and retry state**

Replace `graphChangedUnsub` and `nudgeUnsub` with this state next to the existing install-generation globals:

```ts
const REQUIRED_LISTENER_RETRY_MS = 5_000
const REQUIRED_LISTENER_KEYS = [
  "graphChanged",
  "compatibilityNudge",
] as const

type RequiredListenerKey = (typeof REQUIRED_LISTENER_KEYS)[number]

type RequiredListenerSlot = {
  channel: string
  subscribed: boolean
  subscribing: boolean
  warningEmitted: boolean
  unsubscribe: (() => void) | null
}

const requiredListenerSlots: Record<
  RequiredListenerKey,
  RequiredListenerSlot
> = {
  graphChanged: {
    channel: WORKFLOW_GRAPH_CHANGED_EVENT,
    subscribed: false,
    subscribing: false,
    warningEmitted: false,
    unsubscribe: null,
  },
  compatibilityNudge: {
    channel: WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
    subscribed: false,
    subscribing: false,
    warningEmitted: false,
    unsubscribe: null,
  },
}

let completionUnsub: (() => void) | null = null
let eventReadinessPromise: Promise<void> | null = null
let eventReadinessDeadline: ReturnType<typeof setTimeout> | null = null
let requiredListenerRetryTimer: ReturnType<typeof setTimeout> | null = null
```

- [ ] **Step 7: Implement missing-only attempts and one shared retry loop**

Add these functions before `installEventListeners(...)`:

```ts
function subscribeRequiredListener(
  key: RequiredListenerKey,
  get: () => WorkflowGraphState
): Promise<() => void> {
  if (key === "graphChanged") {
    return subscribeWorkflowGraphChanged((payload) => {
      get().handleGraphChanged(payload)
    })
  }
  return subscribeWorkflowCompatibilityNudge((payload) => {
    get().handleCompatibilityNudge(payload)
  })
}

function scheduleRequiredListenerRetry(
  get: () => WorkflowGraphState,
  generation: number
): void {
  if (
    requiredListenerRetryTimer != null ||
    activeEventInstallGeneration !== generation ||
    activeConversations.size === 0 ||
    !REQUIRED_LISTENER_KEYS.some((key) => {
      const slot = requiredListenerSlots[key]
      return !slot.subscribed && !slot.subscribing
    })
  ) {
    return
  }

  requiredListenerRetryTimer = setTimeout(() => {
    requiredListenerRetryTimer = null
    if (
      activeEventInstallGeneration !== generation ||
      activeConversations.size === 0
    ) {
      return
    }
    for (const attempt of attemptMissingRequiredListeners(get, generation)) {
      void attempt
    }
  }, REQUIRED_LISTENER_RETRY_MS)
}

function attemptRequiredListener(
  key: RequiredListenerKey,
  get: () => WorkflowGraphState,
  generation: number
): Promise<void> {
  const slot = requiredListenerSlots[key]
  if (slot.subscribed || slot.subscribing) return Promise.resolve()
  slot.subscribing = true

  return subscribeRequiredListener(key, get)
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      slot.subscribing = false
      if (slot.subscribed || slot.unsubscribe) {
        dispose()
        return
      }
      slot.subscribed = true
      slot.warningEmitted = false
      slot.unsubscribe = dispose
    })
    .catch((error: unknown) => {
      if (activeEventInstallGeneration !== generation) return
      slot.subscribing = false
      if (!slot.warningEmitted) {
        console.warn(
          "[workflow-graph-store] required event subscription failed",
          {
            channel: slot.channel,
            error: toErrorMessage(error),
          }
        )
        slot.warningEmitted = true
      }
      scheduleRequiredListenerRetry(get, generation)
    })
}

function attemptMissingRequiredListeners(
  get: () => WorkflowGraphState,
  generation: number
): Promise<void>[] {
  return REQUIRED_LISTENER_KEYS.filter((key) => {
    const slot = requiredListenerSlots[key]
    return !slot.subscribed && !slot.subscribing
  }).map((key) => attemptRequiredListener(key, get, generation))
}
```

- [ ] **Step 8: Install required attempts with readiness, leaving completion optional**

Replace `installEventListeners(...)` with:

```ts
function installEventListeners(get: () => WorkflowGraphState): Promise<void> {
  if (activeEventInstallGeneration !== 0 && eventReadinessPromise != null) {
    return eventReadinessPromise
  }
  const generation = ++eventInstallGeneration
  activeEventInstallGeneration = generation

  const requiredAttempts = attemptMissingRequiredListeners(get, generation)
  const completionAttempt = subscribeCompletionDecisionResolved((payload) => {
    get().handleCompletionDecisionResolved(payload)
  })
    .then((dispose) => {
      if (activeEventInstallGeneration !== generation) {
        dispose()
        return
      }
      completionUnsub = dispose
    })
    .catch(() => {
      // Older transports converge through graph snapshot refresh.
    })

  const attemptsSettled = Promise.allSettled([
    ...requiredAttempts,
    completionAttempt,
  ]).then(() => undefined)
  const deadline = new Promise<void>((resolve) => {
    eventReadinessDeadline = setTimeout(resolve, EVENT_READINESS_TIMEOUT_MS)
  })
  eventReadinessPromise = Promise.race([attemptsSettled, deadline]).finally(
    () => {
      if (activeEventInstallGeneration !== generation) return
      if (eventReadinessDeadline != null) {
        clearTimeout(eventReadinessDeadline)
        eventReadinessDeadline = null
      }
    }
  )
  return eventReadinessPromise
}
```

- [ ] **Step 9: Dispose retry state, latches, pending flags, and listeners together**

Replace `disposeEventListeners()` with:

```ts
function disposeEventListeners(): void {
  activeEventInstallGeneration = 0
  if (eventReadinessDeadline != null) {
    clearTimeout(eventReadinessDeadline)
    eventReadinessDeadline = null
  }
  if (requiredListenerRetryTimer != null) {
    clearTimeout(requiredListenerRetryTimer)
    requiredListenerRetryTimer = null
  }
  eventReadinessPromise = null

  for (const key of REQUIRED_LISTENER_KEYS) {
    const slot = requiredListenerSlots[key]
    slot.unsubscribe?.()
    slot.unsubscribe = null
    slot.subscribed = false
    slot.subscribing = false
    slot.warningEmitted = false
  }

  completionUnsub?.()
  completionUnsub = null
}
```

Do not reset monotonic `eventInstallGeneration`. Keep final lease release and store reset calling this disposer exactly once when global interest reaches zero.

- [ ] **Step 10: Record the deferred green checkpoint without executing it**

Deferred until Task 5:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected green result at final verification: failed channels retry every 5 seconds, successful siblings stay single, warnings latch per generation, durable refresh continues, and final release leaves zero timers.

- [ ] **Step 11: Commit the producer task and prepare its review package**

```powershell
git add src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
git commit -m "fix: retry workflow event subscriptions"
git show --stat --oneline HEAD
git diff HEAD^ -- src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
```

Expected package: one commit containing only the graph store and tests. Queue it to `codex + grok` reviewers with the Task 3 risk row, then continue directly to Task 4.

### Task 4: Aggregate the Pre-Final Delivery and Scope Audit

**Files:**

- Read: Task 1-3 commits and the four allowed frontend files
- Modify: none
- Test execution: none

**Interfaces:**

- Consumes: committed producer outputs and their review packages.
- Produces: exact changed-file allowlist evidence, commit series, whitespace check, and one consolidated review diff for Task 5.

**Task Routing Matrix:**

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 4 | Aggregate the pre-final delivery and scope audit | committed diff across hook and graph store modules | none | `multiple_ownership_modules=1`; total `1` | `normal`: read-only audit with one soft signal | `grok` | `codex` | `b2d_task_risk_v1` |

- [ ] **Step 1: Resolve the plan commit as the delivery base**

```powershell
$deliveryBase = git rev-list -n 1 --grep="^docs: plan workflow refresh self-healing$" HEAD
if (-not $deliveryBase) {
  throw "Plan commit was not found in the current branch history."
}
git log --reverse --format="%h %s" "$deliveryBase..HEAD"
```

Expected: the three producer commits appear in Task order. Additional repair commits are allowed only when their diff remains inside the owning task's files.

- [ ] **Step 2: Enforce the exact frontend changed-file allowlist**

```powershell
$changedFiles = @(git diff --name-only "$deliveryBase..HEAD")
$allowedFiles = @(
  "src/hooks/use-delegation-card-model.ts"
  "src/hooks/use-delegation-card-model.test.ts"
  "src/lib/workflow-graph-store.ts"
  "src/lib/workflow-graph-store.test.ts"
)
$unexpectedFiles = @(
  $changedFiles | Where-Object { $_ -notin $allowedFiles }
)
$missingFiles = @(
  $allowedFiles | Where-Object { $_ -notin $changedFiles }
)
if ($unexpectedFiles.Count -gt 0) {
  throw "Unexpected delivery files: $($unexpectedFiles -join ', ')"
}
if ($missingFiles.Count -gt 0) {
  throw "Missing delivery files: $($missingFiles -join ', ')"
}
$changedFiles
```

Expected: exactly the four allowed paths. Any Rust, API, transport, schema, event, persistence, lockfile, generated, or locale path is a hard scope failure.

- [ ] **Step 3: Check the aggregated diff without executing tests**

```powershell
git diff --check "$deliveryBase..HEAD"
git diff --stat "$deliveryBase..HEAD"
git diff "$deliveryBase..HEAD" -- src/hooks/use-delegation-card-model.ts src/hooks/use-delegation-card-model.test.ts src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
```

Expected: `git diff --check` prints nothing. The diff shows only the approved frontend convergence behavior and regressions.

- [ ] **Step 4: Produce the pre-final review checklist**

Attach these exact assertions to the consolidated diff:

```text
1. Delegation: terminal binding > terminal meta > matching terminal snapshot > running live sources > non-terminal snapshot.
2. Delegation: any present binding/meta identity is non-null and equals snapshot.task_id before snapshot fields may participate.
3. Delegation: effective sources drive lifecycle, badge, stats, timestamps, error, attention, task, agent, connection, generation, and card summary.
4. Workflow: active nodes or overall in_progress select 15 seconds; settled expanded/undiscovered select 10 minutes; settled discovered overlay selects no timer.
5. Workflow: request generation, graph revision, and activation epoch still reject stale completion.
6. Listeners: required channels warn once, retry missing non-pending slots after 5 seconds, keep successful siblings, and share one retry timer.
7. Disposal: final interest release clears per-conversation refresh timers and install-generation listener retry state; late successes dispose themselves.
8. Scope: no backend, API, payload, persistence, dependency, or generated-file change.
```

Queue this package to the Task 4 `codex` reviewer and proceed to Task 5. Do not wait for user acceptance.

### Task 5: Run Final Automated Verification, Review, and Delivery

**Files:**

- Verify: `src/hooks/use-delegation-card-model.test.ts`
- Verify: `src/lib/workflow-graph-store.test.ts`
- Verify: complete frontend test/lint/build surface
- Modify: none when green; any failure returns to the owning producer task for a focused committed repair

**Interfaces:**

- Consumes: clean committed Task 1-3 output and Task 4 aggregate package.
- Produces: targeted and broad command evidence, final two-reviewer findings, clean worktree evidence, and delivery commit list.

**Task Routing Matrix:**

| task_index | title | files/modules | hard_triggers evidence | soft_signals evidence + soft total | final risk level + reason | implementer agent | reviewer set | policy version |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 5 | Run final automated verification, review, and delivery | targeted suites, full suite, lint, build, final diff | none | `broad_production_surface=1`, `multiple_ownership_modules=1`, `dependency_or_build=1`; total `3` | `high`: soft threshold reached | `codex` | `codex + grok` | `b2d_task_risk_v1` |

- [ ] **Step 1: Run the delegation regression suite**

```powershell
pnpm test -- src/hooks/use-delegation-card-model.test.ts
```

Expected: exit code `0`; all existing and new delegation-card tests pass.

- [ ] **Step 2: Run the workflow graph store regression suite**

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected: exit code `0`; all revision, activation, 15-second, 10-minute, retry, warning, lease, and generation tests pass with no leaked fake timers.

- [ ] **Step 3: Run the full frontend test suite**

```powershell
pnpm test
```

Expected: exit code `0`; the complete Vitest suite passes.

- [ ] **Step 4: Run frontend lint**

```powershell
pnpm eslint .
```

Expected: exit code `0`; no ESLint or TypeScript lint errors.

- [ ] **Step 5: Run the static export build**

```powershell
pnpm build
```

Expected: exit code `0`; Next.js static export completes successfully.

No Rust command is required because Task 4 proves that no Rust or backend contract file changed.

- [ ] **Step 6: Apply the automated failure protocol when any command is red**

If a command fails, stop the verification sequence, assign the failure to the owning high-risk producer task, add or tighten a focused regression when the failure lacks one, make the minimal frontend repair, and commit it with an exact subject describing the repair. Then rerun Task 5 from Step 1; partial command success is not final evidence.

- [ ] **Step 7: Run final diff and worktree checks**

```powershell
$deliveryBase = git rev-list -n 1 --grep="^docs: plan workflow refresh self-healing$" HEAD
if (-not $deliveryBase) {
  throw "Plan commit was not found in the current branch history."
}
git diff --check "$deliveryBase..HEAD"
git status --short
git log --reverse --format="%h %s" "$deliveryBase..HEAD"
git diff --stat "$deliveryBase..HEAD"
```

Expected: no whitespace errors, a clean worktree, and only producer/repair commits for the four allowed frontend files.

- [ ] **Step 8: Complete final independent review**

Send the Task 4 aggregate diff, Task 5 command outputs, and the Task 5 risk row to both reviewers:

```text
codex review focus:
- exact identity compatibility and complete stale-source omission
- lifecycle precedence and terminal non-reopening
- refresh delay selection after success, soft absence, and failure
- activation epoch and request-generation preservation
- required-listener pending/success/failure/disposal transitions

grok review focus:
- independent b2d_task_risk_v1 recomputation
- design-to-test coverage for every listed regression
- duplicate timer/listener and warning-latch edge cases
- frontend-only scope and absence of protocol drift
```

All critical and important findings must return to the owning producer for a committed repair followed by the complete Task 5 command sequence. Minor findings are either repaired before delivery or recorded explicitly in the delivery package. This is an automated review gate, not a user sign-off gate.

- [ ] **Step 9: Deliver the completed branch evidence**

The delivery package contains:

```text
- branch: feat/workflow-refresh-self-healing
- producer and repair commit list from the plan commit through HEAD
- exact four-file change list
- targeted delegation test result
- targeted workflow graph test result
- full pnpm test result
- pnpm eslint . result
- pnpm build result
- codex and grok final review outcomes
- remaining minor concerns, or an explicit statement that none remain
```

Do not push. Human acceptance and any manual click-through occur only after this delivery package is published.

## Execution Handoff

Use `superpowers:subagent-driven-development` for serial Task 1-5 execution and route each task exactly as its matrix row specifies. The parent orchestrator owns reviewer scheduling and final publication; no workflow identifiers or artifact digests beyond the approved design digest are introduced by this plan.

Plan gate cohort for the parent to wire:

```text
plan-reviewer-codex (codex)
plan-reviewer-grok (grok)
```
