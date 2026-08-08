# Task 5 Report: Final Automated Verification, Review, and Delivery

## Status

**DONE_WITH_CONCERNS**

All required frontend verification commands pass after two focused Task 5
repairs. The branch is ready for post-delivery human acceptance. No push,
merge, rebase, pull request, or manual click-through was performed.

The remaining concerns are non-blocking:

1. Repository-wide ESLint exits zero but reports 25 pre-existing warnings.
2. Parent adjudication authorized a formatting-only scope exception in two
   frontend helper files so the required full lint command could pass.

## Delivery Anchors

| Field | Value |
| --- | --- |
| Branch | `feat/workflow-refresh-self-healing` |
| Delivery base | `f80ea84fb32cceaf4a0580658764e31965112439` |
| Verified product/style HEAD | `e3940f41c6bd7200442192d644b627d23945549f` |
| Approved design LF SHA-256 | `2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf` |
| Risk | `high` under `b2d_task_risk_v1` |
| Implementer | Codex |
| Review lanes | Codex final audit + independent Grok review |

The design digest was recomputed at delivery and matched the approved digest.

## Failure Recovery

### Full-suite import compatibility

The first full `pnpm test` run collected nine suites with zero tests because
their explicit `@/lib/api` mocks did not export
`WORKFLOW_GRAPH_CHANGED_EVENT`. Task 3 had begun reading that binding while
constructing module-level listener slots.

The focused repair keeps the production event constants but defers reading the
warning label until a required subscription actually fails. It does not alter
subscription ownership, retry timing, refresh timing, revision gates,
request-generation gates, or activation epochs.

```text
752b06a7 fix: defer workflow event channel lookup
```

A focused previously failing suite then passed 29/29, the graph suite passed
64/64, and the complete Task 5 sequence restarted from Step 1.

### Pre-existing Prettier errors

The first `pnpm eslint .` run found six Prettier errors in files unchanged
from `main`. Parent adjudication explicitly authorized Prettier-only fixes to
the three named files. Prettier changed two; the third was already unchanged.

```text
e3940f41 style: fix pre-existing prettier lint in chat helpers
```

The commit changes only wrapping and parentheses in:

```text
src/components/chat/sub-agent-overlay.tsx
src/lib/delegation-activity.ts
```

`src/lib/delegation-conversation-interrupted.ts` was authorized but remained
unchanged. No protocol, API, payload, transport, backend, schema, persistence,
dependency, lockfile, locale, or generated-file surface changed.

## Final Command Summary

| Command | Exit | Result |
| --- | ---: | --- |
| `pnpm test -- src/hooks/use-delegation-card-model.test.ts` | 0 | 1 file, 51 tests passed |
| `pnpm test -- src/lib/workflow-graph-store.test.ts` | 0 | 1 file, 64 tests passed |
| `pnpm test` | 0 | 346 files, 5,082 tests passed |
| `pnpm eslint .` | 0 | 0 errors, 25 warnings |
| `pnpm build` | 0 | Next.js 16.1.6 compiled; TypeScript passed; 33/33 static pages generated |

The targeted tests are included in the 5,082-test full-suite count and are not
double-counted as unique tests.

## Full Command Outputs

### `pnpm test -- src/hooks/use-delegation-card-model.test.ts`

Exit code: `0`

~~~text
$ vitest run "src/hooks/use-delegation-card-model.test.ts"
The CJS build of Vite's Node API is deprecated. See https://vite.dev/guide/troubleshooting.html#vite-cjs-node-api-deprecated for more details.

 RUN  v2.1.9 D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing

 ✓ src/hooks/use-delegation-card-model.test.ts (51 tests) 8ms

 Test Files  1 passed (1)
      Tests  51 passed (51)
   Start at  17:26:02
   Duration  1.91s (transform 357ms, setup 76ms, collect 1.07s, tests 8ms, environment 364ms, prepare 64ms)
~~~

### `pnpm test -- src/lib/workflow-graph-store.test.ts`

Exit code: `0`

The warning output below is intentional recovery-path coverage.

~~~text
$ vitest run "src/lib/workflow-graph-store.test.ts"
The CJS build of Vite's Node API is deprecated. See https://vite.dev/guide/troubleshooting.html#vite-cjs-node-api-deprecated for more details.

 RUN  v2.1.9 D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing

stderr | src/lib/workflow-graph-store.test.ts > workflow activation lifecycle > keeps a successful listener owned when its sibling rejects
[workflow-graph-store] required event subscription failed { channel: 'workflow_graph://changed', error: 'changed unavailable' }

stderr | src/lib/workflow-graph-store.test.ts > workflow activation lifecycle > retries only the missing required listener and retains its sibling
[workflow-graph-store] required event subscription failed { channel: 'workflow_graph://changed', error: 'changed unavailable' }

stderr | src/lib/workflow-graph-store.test.ts > active workflow refresh scheduling > subscription failures still allow initial and periodic refresh
[workflow-graph-store] required event subscription failed {
  channel: 'workflow_graph://changed',
  error: 'graph events unavailable'
}
[workflow-graph-store] required event subscription failed {
  channel: 'workflow_graph://compatibility_nudge',
  error: 'nudge events unavailable'
}

 ✓ src/lib/workflow-graph-store.test.ts (64 tests) 144ms

 Test Files  1 passed (1)
      Tests  64 passed (64)
   Start at  17:26:14
   Duration  1.07s (transform 91ms, setup 76ms, collect 105ms, tests 144ms, environment 365ms, prepare 65ms)
~~~

### `pnpm test`

Exit code: `0`

~~~text
$ vitest run
The CJS build of Vite's Node API is deprecated. See https://vite.dev/guide/troubleshooting.html#vite-cjs-node-api-deprecated for more details.

 RUN  v2.1.9 D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing

stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > does not close detached when reclaim throws / no bridge
[ConversationPopout] reclaimAfterAbort failed; keeping transfer fence Error: ACP reclaim bridge is not registered
    at Module.reclaimAfterAbort (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout-acp-bridge.ts:188:11)
    at reclaimForMatchingFence (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:198:11)
    at recoverPopoutAbortTerminal (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:704:11)
    at compensate (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:881:9)
    at processTicksAndRejections (node:internal/process/task_queues:104:5)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1060:7
    at Module.popOutConversation (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1071:5)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.test.ts:432:5
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)

 ✓ src/components/conversations/sidebar-conversation-grouping.test.ts (105 tests) 23ms
stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > retries restore flush up to 3 times and does not close when still rejected
[ConversationPopout] restore opened_tabs CAS rejected after 3 retries (version=9) { lastAccepted: false }
[ConversationPopout] restore+flush failed; leaving detached open Error: restore opened_tabs CAS rejected after 3 retries (version=9)
    at restoreTabWithFlushRetry (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:508:15)
    at processTicksAndRejections (node:internal/process/task_queues:104:5)
    at recoverPopoutAbortTerminal (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:714:7)
    at compensate (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:881:3)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1060:7
    at Module.popOutConversation (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1071:5)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.test.ts:471:5
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)

stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > re-resolves current tab id before detach after concurrent openTab
[ConversationPopout] emit commit-ack failed TypeError: Cannot read properties of undefined (reading 'invoke')
    at invoke (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@tauri-apps+api@2.11.1/node_modules/@tauri-apps/api/core.js:202:39)
    at emit (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@tauri-apps+api@2.11.1/node_modules/@tauri-apps/api/event.js:131:11)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1050:15
    at processTicksAndRejections (node:internal/process/task_queues:104:5)
    at Module.popOutConversation (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1071:5)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.test.ts:515:5
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)

stderr | src/lib/delegation-card.test.ts > parseInput wrapper peeling > returns empty fields for a non-delegation payload
[delegation-card] could not extract delegation args (no known wrapper matched). shape=object{ command: string }

 ✓ src/lib/delegation-card.test.ts (72 tests) 13ms
stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > does not clear transfer fence when abort stays non-terminal after wait
[ConversationPopout] abort still pending after terminal wait; keeping transfer fence and scheduling recovery {
  conversationId: 1,
  operationId: 'c8cf371d-158e-4763-8cdf-0b793962aae3',
  phase: 'ready_pending'
}

stderr | src/lib/workflow-graph-store.test.ts > workflow activation lifecycle > keeps a successful listener owned when its sibling rejects
[workflow-graph-store] required event subscription failed { channel: 'workflow_graph://changed', error: 'changed unavailable' }

stderr | src/lib/workflow-graph-store.test.ts > workflow activation lifecycle > retries only the missing required listener and retains its sibling
[workflow-graph-store] required event subscription failed { channel: 'workflow_graph://changed', error: 'changed unavailable' }

stderr | src/lib/workflow-graph-store.test.ts > active workflow refresh scheduling > subscription failures still allow initial and periodic refresh
[workflow-graph-store] required event subscription failed {
  channel: 'workflow_graph://changed',
  error: 'graph events unavailable'
}
[workflow-graph-store] required event subscription failed {
  channel: 'workflow_graph://compatibility_nudge',
  error: 'nudge events unavailable'
}

 ✓ src/lib/workflow-graph-store.test.ts (64 tests) 148ms
stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > background recovery reclaims late Reversed after terminal wait timeout
[ConversationPopout] abort still pending after terminal wait; keeping transfer fence and scheduling recovery {
  conversationId: 1,
  operationId: '4eb23379-0948-4569-bda0-075bc8a2a7a9',
  phase: 'ready_pending'
}

 ✓ src/components/conversations/use-sidebar-reorder-animation.test.tsx (19 tests) 84ms
stderr | src/lib/conversation-popout.test.ts > popOutConversation compensation > refuses second pop-out while fence/recovery active; late O1 Reversed still reclaims
[ConversationPopout] abort still pending after terminal wait; keeping transfer fence and scheduling recovery {
  conversationId: 1,
  operationId: 'ad43e7ff-31c5-4f88-a920-60c4628051a7',
  phase: 'ready_pending'
}

 ✓ src/lib/adapters/ai-elements-adapter.test.ts (84 tests) 23ms
stderr | src/components/providers/update-provider.test.tsx > UpdateProvider — availability > records a failed check without persisting it as a completed one
[Update] check failed: Error: error sending request for url
    at checkResult (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:382:13)
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:43:47)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.checkAppUpdateInfo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:242:27)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:411:30
    at Object.checkNow (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:461:17)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:388:18
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\@testing-library+react@16.3_3a20851e9a4423bc10fc5626bc37c041\node_modules\@testing-library\react\dist\act-compat.js:47:24
    at process.env.NODE_ENV.exports.act (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react@19.2.4\node_modules\react\cjs\react.development.js:814:22)

stderr | src/lib/conversation-popout.test.ts > isPopOutInFlight / transfer fence for openTab > bumps transfer epoch at pop-out start and end
[ConversationPopout] emit commit-ack failed TypeError: Cannot read properties of undefined (reading 'invoke')
    at invoke (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@tauri-apps+api@2.11.1/node_modules/@tauri-apps/api/core.js:202:39)
    at emit (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@tauri-apps+api@2.11.1/node_modules/@tauri-apps/api/event.js:131:11)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1050:15
    at Module.popOutConversation (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.ts:1071:5)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.test.ts:1257:5
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)

 ✓ src/lib/conversation-popout.test.ts (26 tests) 604ms
stderr | src/components/providers/update-provider.test.tsx > UpdateProvider — stale cache after an upgrade > falls back to /health when the status route 404s (older server)
[Update] status route unavailable: Error: not implemented
    at callImpl (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:589:51)
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:39:24)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.getServerUpdateStatus (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:285:25)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:526:32
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:555:15
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:577:45
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:599:10
    at Object.react_stack_bottom_frame (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:25989:20)

 ✓ src/components/chat/composer/reference-search-controller.test.ts (28 tests) 56ms
stderr | src/components/providers/update-provider.test.tsx > UpdateProvider — sibling window survives a backend restart > stops advertising a release the backend is now running, without reloading
[Update] check failed: Error: manifest unreachable
    at callImpl (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:701:15)
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.test.tsx:39:24)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.checkAppUpdateInfo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:242:27)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:411:30
    at Object.current (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:461:17)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:509:22
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:551:5

 ✓ src/components/providers/update-provider.test.tsx (33 tests) 461ms
stderr | src/contexts/workspace-context.test.tsx > openFilePreview cache semantics > retries after a cold open failure by creating a fresh tab
[file-open] file:%2Frepo%2Fa.ts boom

 ✓ src/lib/delegation-status.test.ts (62 tests) 11ms
 ✓ src/stores/viewer-detail-sync.test.ts (29 tests) 16ms
 ✓ src/stores/cancel-reconcile.test.ts (70 tests) 42ms
 ✓ src/contexts/conversation-runtime-context.test.tsx (63 tests) 121ms
 ✓ src/stores/app-workspace-store.test.ts (31 tests) 10ms
 ✓ src/components/conversations/conversation-session-surface.test.ts (59 tests) 295ms
stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > cold open failure removes the tab and does not leave saveState error
[file-open] file:%2Frepo%2Fmissing.ts ENOENT

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > warm reload failure keeps prior content
[file-open] file:%2Frepo%2Fa.ts locked

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > failed cold open with pre-existing other tab does not steal maximize incorrectly
[file-open] file:%2Frepo%2Fb.ts ENOENT

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > reloadOpenFileBackground warm fail keeps content and toasts
[file-open] file:%2Frepo%2Fa.ts io fail

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > concurrent openFilePreview shares failure settle (not premature ok)
[file-open] file:%2Frepo%2Fmissing.ts ENOENT

stderr | src/contexts/app-workspace-context.test.tsx > AppWorkspaceProvider folder://changed sync > auto_empty post-refetch guard restores membership after first reopen fails
[AppWorkspace] silent re-open after auto_empty close failed: Error: first pre-refetch reopen fails
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\app-workspace-context.test.tsx:1004:30
    at runNextTicks (node:internal/process/task_queues:65:5)
    at processTimers (node:internal/timers:538:9)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > warm fail after rejectFileTab retry keeps non-empty last-good content
[file-open] file:%2Frepo%2Fa.ts retry-fail

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > rich-diff both-side read failures cold-close tab and toast
[file-open] diff:working:1:a.ts git-show-fail

stderr | src/contexts/workspace-context.test.tsx > openFilePreview failure matrix and maximize-on-success > image open sets hasLoadedSuccessfully; warm fail does not cold-close
[file-open] file:%2Frepo%2Fphoto.png image-io-fail

 ✓ src/contexts/workspace-context.test.tsx (82 tests) 924ms
 ✓ src/contexts/app-workspace-context.test.tsx (35 tests) 325ms
 ✓ src/components/conversations/conversation-detail-panel-layout.test.ts (31 tests) 79ms
 ✓ src/lib/tool-call-normalization.test.ts (87 tests) 10ms
stdout | src/contexts/acp-connections-context.test.tsx > out-of-turn wire guard + background activity > drops streaming deltas while the connection is not prompting (Bug-A guard)
[acp] dropping out-of-turn streaming deltas (transcript overlay renders them) { contextKey: 'conv-1-claude_code-42', type: 'CONTENT_DELTA' }

stderr | src/contexts/acp-connections-context.test.tsx > root_conversation_activity_at_acp_dispatch_boundaries > rolls back the exact answer-question token when acpAnswerQuestion rejects
[AcpConnections] answerQuestion failed: Error: answer failed
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\acp-connections-context.test.tsx:4013:49
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

stdout | src/contexts/acp-connections-context.test.tsx > tool_watchdog_changed reduction and desktop notification > conversation_linked updates null conversationId so later watchdog notify has target
[acp-context] conversation_linked {
  contextKey: 'conv-1-claude_code-42',
  connectionId: 'spawned-conn',
  conversationId: 99,
  folderId: 1
}

 ✓ src/hooks/use-delegation-card-model.test.ts (51 tests) 13ms
stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider canonical observer aliases > sequence-gap rejected-snapshot recovery does not acpConnect for discovery errors
[acp-context] sequence gap recovery failed broker-child Error: malformed discovery payload
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\acp-connections-context.test.tsx:6040:7
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11
    at withEnv (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:90:5)
    at run (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:112:3)

stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider observe_existing intent > stops observe discovery immediately on non-retryable auth error
[acp-context] observer discovery failed { status: 401, message: 'Unauthorized' }

stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider observe_existing intent > retries observe discovery on retryable timeout errors
[acp-context] observer discovery failed Error: Request timed out
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\acp-connections-context.test.tsx:6555:30
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider observe_existing intent > handoff auth error re-attaches observer and owner connects after broker removed
[acp-context] handoff discovery failed { status: 401, message: 'Unauthorized' }

stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider observe_existing intent > observe_existing fails closed on malformed discovery payload
[acp-context] observer discovery returned malformed payload { connection_id: '', event_seq: 0 }

stderr | src/contexts/acp-connections-context.test.tsx > AcpConnectionsProvider observe_existing intent > handoff auth error does not reattach after disconnect abandons the key
[acp-context] handoff discovery failed { status: 401, message: 'Unauthorized' }

 ✓ src/contexts/acp-connections-context.test.tsx (173 tests) 607ms
 ✓ src/components/message/delegated-sub-thread.test.tsx (39 tests) 531ms
 ✓ src/components/settings/acp-agent-settings.test.tsx (61 tests) 14ms
 ✓ src/stores/conversation-runtime-store.test.ts (23 tests) 29ms
 ✓ src/stores/tab-store-tab-limit.test.ts (22 tests) 17ms
stderr | src/components/chat/sub-agent-overlay.test.tsx > SubAgentOverlay > renders a graceful fallback row for a delegation with unparseable input
[delegation-card] could not extract delegation args (JSON.parse threw). shape=non-JSON(len=8)

stderr | src/components/chat/sub-agent-overlay.test.tsx > SubAgentOverlay > renders fallback rows even when every delegation is unresolvable
[delegation-card] could not extract delegation args (JSON.parse threw). shape=non-JSON(len=8)
[delegation-card] could not extract delegation args (JSON.parse threw). shape=non-JSON(len=8)

 ✓ src/components/chat/sub-agent-overlay.test.tsx (27 tests) 370ms
stderr | src/components/message/message-list-view.test.tsx > MessageListView waiting-for-subagents bottom banner > latches pre-suspend live tools into the waiting banner
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTurnStatsBanner2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveAgentPlanOverlay2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveAwareSubAgentOverlay2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to MessageListView inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptSegmentView2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptSegmentView2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveToolCard2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTurnStatsBanner2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
 ✓ src/components/message/message-list-view.test.tsx (42 tests) 359ms
An update to MessageListView inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/lib/opencode-connect.test.ts (38 tests) 14ms
 ✓ src/components/conversations/sidebar-conversation-list.test.tsx (34 tests) 1707ms
 ✓ src/lib/reference-search-cache.test.ts (16 tests) 17ms
 ✓ src/stores/background-overlay.test.ts (22 tests) 14ms
stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > sends the parsed context window once the form is valid
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > drops the 'key works' verdict once the credentials it measured change
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/components/chat/composer/suggestion/suggestion-popup.test.tsx (29 tests) 633ms
stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > warns when the chosen model is absent from the key's model list
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/lib/transport/web-transport.test.ts (20 tests) 16ms
 ✓ src/components/message/virtualized-message-thread.test.tsx (30 tests) 552ms
   ✓ VirtualizedMessageThread footer slot > 500 footer height changes do not change Virtua items or keys 417ms
stderr | src/contexts/user-stop-dual-path.test.ts > FE11 dual-path completion orderings > typed envelope then status-edge: one outcome, one coordinator, content kept
[conversation-runtime] COMPLETE_TURN dispatched on an already-drained session; ignoring { conversationId: 42 }

 ✓ src/contexts/user-stop-dual-path.test.ts (15 tests) 18ms
 ✓ src/lib/delegation-transcript-projection.test.ts (23 tests) 9ms
 ✓ src/stores/turn-metadata-patches.test.ts (22 tests) 9ms
stderr | src/stores/tab-store-empty-folder.test.ts > draft leave → conditional close > transport error refetches and retries once when still open + leave holds
[maybeCloseEmptyFolder] closeFolderIfEmpty failed: Error: network
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\stores\tab-store-empty-folder.test.ts:392:30
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

 ✓ src/components/chat/workflow-overlay.test.tsx (34 tests) 2178ms
   ✓ SubAgentOverlay A13 workflow mount > renders legacy backlinks and resumes only through durable root controls 559ms
 ✓ src/stores/tab-store-empty-folder.test.ts (11 tests) 317ms
 ✓ src/components/message/delegation-status-group-card.test.tsx (20 tests) 261ms
stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > keeps reasoning out of the payload until it is switched on
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > sends the chosen levels once reasoning is switched on
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > drops a default level when its chip is switched back off
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/settings/kimi-code-config-panel.test.tsx > KimiCodeConfigPanel > surfaces the backend's own message when a save is rejected
[KimiCode] save config failed Error: kimi native config requires a model id
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\kimi-code-config-panel.test.tsx:787:7
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11
An update to KimiCodeConfigPanel inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItemText inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to SelectItem inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/hooks/use-connection-lifecycle.test.ts > handle_send_forwards_display_text_and_effective_locale > does not reach prompt dispatch when mode change fails
[ConnLifecycle] sendPrompt: Error: mode failed
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\hooks\use-connection-lifecycle.test.ts:500:37
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runNextTicks (node:internal/process/task_queues:65:5)
    at processTimers (node:internal/timers:538:9)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)

 ✓ src/components/settings/kimi-code-config-panel.test.tsx (52 tests) 1924ms
 ✓ src/lib/html-preview-inline.test.ts (44 tests) 329ms
 ✓ src/contexts/delegation-context.test.tsx (17 tests) 340ms
 ✓ src/hooks/use-connection-lifecycle.test.ts (22 tests) 443ms
 ✓ src/lib/background-task.test.ts (32 tests) 8ms
stderr | src/components/conversations/sidebar-conversation-card.test.tsx > mutation failure feedback > keeps the rename dialog open and toasts when rename fails
[SidebarConversationCard] rename: Error: db locked
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\conversations\sidebar-conversation-card.test.tsx:573:36
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at processTicksAndRejections (node:internal/process/task_queues:104:5)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > does not mount a thinking segment when visibility is off
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > keeps tools visible when a thinking segment is hidden
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > keeps a mapped continuation visible as its own card alongside shell tools
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > keeps an identity-free initial delegation visible
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > shows a typing indicator when the live snapshot has no segments yet
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > renders text segments via narrow subscriptions
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > hides the exact live interrupt marker on parent and child sessions
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > updates text without remounting the row when chunks append
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > updates one tool card without rendering siblings
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to Presence inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/message/live-transcript-row.test.tsx > LiveTranscriptRow > collapses multi-tool groups to a summary until expanded
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to LiveTranscriptRow2 inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/components/message/live-transcript-row.test.tsx (15 tests) 205ms
stderr | src/components/conversations/sidebar-conversation-card.test.tsx > mutation failure feedback > keeps the delete dialog open and toasts when delete fails
[SidebarConversationCard] delete: Error: db locked
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\conversations\sidebar-conversation-card.test.tsx:585:36
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

 ✓ src/components/conversations/sidebar-conversation-card.test.tsx (29 tests) 1184ms
 ✓ src/lib/ask-question.test.ts (34 tests) 6ms
 ✓ src/lib/acp/event-ingestor.test.ts (18 tests) 9ms
 ✓ src/lib/delegation-activity.test.ts (34 tests) 8ms
 ✓ src/lib/perf/streaming-perf-recorder.test.ts (9 tests) 10ms
 ✓ src/components/chat/composer/rich-composer.test.tsx (30 tests) 707ms
 ✓ src/components/message/initial-history-scroll-controller.test.tsx (23 tests) 53ms
stderr | src/stores/live-transcript-store.test.ts > live-transcript-store > rebuilds from canonical state without advancing a false cursor
[live-transcript-store] projector failed; rebuilding from canonical Error: projector boom
    at Object.applyLiveTranscriptEvents (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\stores\live-transcript-store.test.ts:95:15)
    at Object.publish (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\stores\live-transcript-store.ts:565:30)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\stores\live-transcript-store.test.ts:100:11
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at processTicksAndRejections (node:internal/process/task_queues:104:5)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)

 ✓ src/stores/live-transcript-store.test.ts (13 tests) 26ms
 ✓ src/lib/model-config-groups.test.ts (27 tests) 6ms
stderr | src/contexts/tab-context.test.tsx > TabProvider tab groups > keeps the saved layout and composer drafts when the tab fetch fails
[TabStore] listOpenedTabs failed: Error: backend down
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\tab-context.test.tsx:2282:46
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11
    at withEnv (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:90:5)
    at run (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:112:3)

 ✓ src/lib/acp/live-transcript-projector.test.ts (11 tests) 17ms
 ✓ src/components/ai-elements/link-safety.test.tsx (25 tests) 1780ms
 ✓ src/components/files/file-workspace-tab-bar.test.tsx (11 tests) 111ms
 ✓ src/components/message/delegation-status-card.test.tsx (24 tests) 272ms
 ✓ src/lib/delegation-binding-reduce.test.ts (17 tests) 6ms
 ✓ src/components/settings/logs-settings.test.tsx (16 tests) 1563ms
 ✓ src/components/settings/conversation-experience-settings.test.tsx (14 tests) 966ms
 ✓ src/components/chat/ask-question-card.test.tsx (29 tests) 5070ms
   ✓ AskQuestionCard > localizes a recovery card from codes and submits raw approve or decline 3888ms
 ✓ src/lib/delegation-work-unit-runtime.test.ts (18 tests) 6ms
 ✓ src/components/chat/message-input.test.tsx (17 tests) 2315ms
   ✓ MessageInput collapsed selectors popover > selects a config option from the cog Popover and closes it 338ms
   ✓ MessageInput collapsed selectors popover > uses a searchable virtualized list for a long model list 431ms
 ✓ src/stores/tab-store-popout.test.ts (14 tests) 9ms
 ✓ src/components/layout/aux-panel-file-tree-tab-source.test.ts (23 tests) 5ms
stderr | src/components/chat/composer/rich-composer-mention.test.tsx > RichComposer @ mention integration > does not submit on Enter while the panel is open
An update to ForwardRef(SuggestionPopup2) inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to Portals inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to ForwardRef(RichComposer2) inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to ForwardRef(RichComposer2) inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/lib/delegation-child-projection-cache.test.ts (18 tests) 980ms
stderr | src/contexts/tab-context.test.tsx > TabProvider tab groups > adopts the recovery refetch even when its version does not advance
[TabStore] listOpenedTabs failed: Error: backend down
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\tab-context.test.tsx:2329:46
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:5
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:11)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11
    at withEnv (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:90:5)
    at run (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:112:3)

 ✓ src/components/shared/directory-browser-dialog.test.tsx (14 tests) 1225ms
 ✓ src/components/chat/composer/rich-composer-mention.test.tsx (9 tests) 774ms
 ✓ src/lib/snapshot-denormalize.test.ts (21 tests) 5ms
 ✓ src/lib/markdown/local-path-links.test.ts (79 tests) 22ms
 ✓ src/components/message/ask-question-result-card.test.tsx (11 tests) 379ms
 ✓ src/lib/updater.test.ts (24 tests) 79ms
 ✓ src/lib/reference-link.test.ts (36 tests) 12ms
 ✓ src/components/chat/composer/to-prompt-blocks.test.ts (18 tests) 195ms
 ✓ src/hooks/use-session-feedback.test.ts (12 tests) 703ms
 ✓ src/components/message/delegation-card-chrome.test.tsx (14 tests) 96ms
stderr | src/components/settings/system-network-settings.test.tsx > SystemNetworkSettings — update source outage > loads proxy settings and exposes rollback when the manifest is unreachable
[Update] check failed: Error: manifest unreachable
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\system-network-settings.test.tsx:137:15)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.checkAppUpdateInfo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:242:27)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:411:30
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:461:17
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\system-network-settings.tsx:245:7
    at Object.react_stack_bottom_frame (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:25989:20)
    at runWithFiberInDEV (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:874:13)
    at commitHookEffectListMount (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:13249:29)
[Settings] updater unknown error: manifest unreachable
[Settings] updater unknown error: manifest unreachable

 ✓ src/lib/tab-group-layout.test.ts (26 tests) 6ms
 ✓ src/lib/session-files.test.ts (13 tests) 8ms
stderr | src/components/settings/system-network-settings.test.tsx > SystemNetworkSettings — update source outage > loads proxy settings even when the status route is also unavailable (older server)
[Update] check failed: Error: manifest unreachable
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\system-network-settings.test.tsx:179:15)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.checkAppUpdateInfo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:242:27)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:411:30
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:461:17
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\system-network-settings.tsx:245:7
    at Object.react_stack_bottom_frame (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:25989:20)
    at runWithFiberInDEV (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:874:13)
    at commitHookEffectListMount (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:13249:29)
[Update] status route unavailable: Error: not implemented
    at Object.<anonymous> (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\system-network-settings.test.tsx:182:15)
    at Object.mockCall (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+spy@2.1.9/node_modules/@vitest/spy/dist/index.js:61:17)
    at Object.spy [as call] (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/tinyspy@3.0.2/node_modules/tinyspy/dist/index.js:45:80)
    at Module.getServerUpdateStatus (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\updater.ts:285:25)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:526:32
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:555:15
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:577:45
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\providers\update-provider.tsx:599:10
    at Object.react_stack_bottom_frame (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:25989:20)
    at runWithFiberInDEV (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:874:13)
[Settings] updater unknown error: manifest unreachable
[Settings] updater unknown error: manifest unreachable

 ✓ src/hooks/use-ignored-file-tree.test.ts (8 tests) 491ms
 ✓ src/components/conversations/conversation-awaiting-reply-clearer.test.tsx (11 tests) 182ms
 ✓ src/components/chat/chat-input.test.tsx (11 tests) 585ms
 ✓ src/hooks/use-delegate-access.test.ts (10 tests) 762ms
 ✓ src/components/chat/composer/composer-commands.test.ts (25 tests) 214ms
 ✓ src/components/ai-elements/streamdown-plugins.test.ts (19 tests) 811ms
 ✓ src/lib/branch-selector-rows.test.ts (17 tests) 10ms
 ✓ src/components/settings/system-network-settings.test.tsx (6 tests) 728ms
 ✓ src/components/message/content-parts-renderer.test.tsx (13 tests) 386ms
 ✓ src/components/conversations/tool-watchdog-banner.test.tsx (15 tests) 237ms
 ✓ src/lib/api.test.ts (13 tests) 6ms
 ✓ src/lib/branch-tree.test.ts (25 tests) 11ms
 ✓ src/components/chat/composer/from-prompt-blocks.test.ts (17 tests) 70ms
 ✓ src/contexts/tab-context.test.tsx (89 tests) 8914ms
   ✓ TabProvider cross-client sync > saves the focused tab so focus syncs across clients 580ms
   ✓ TabProvider cross-client sync > cancels a pending local save when a remote snapshot supersedes it 620ms
   ✓ TabProvider cross-client sync > cancels an armed save when the set reverts to the last-saved state 606ms
   ✓ TabProvider cross-client sync > re-saves to reconcile when the set reverts while a save is in flight 1163ms
   ✓ TabProvider cross-client sync > does not regress the version when an accepted save resolves after a newer remote 589ms
   ✓ TabProvider cross-client sync > ignores a rejected save's stale snapshot when a newer remote already applied 590ms
   ✓ TabProvider cross-client sync > adopts the server snapshot when a save is rejected with no newer remote already applied 535ms
   ✓ TabProvider tab groups > group actions on the already-active tab never dirty the synced payload 715ms
   ✓ TabProvider tab groups > keeps the saved layout and composer drafts when the tab fetch fails 782ms
   ✓ TabProvider tab groups > restores drafts without dirtying the synced payload 714ms
   ✓ TabProvider tab groups > moveTabToGroup with index (drag drops) > keeps the menu move (no index) free of rawTabs reorder and saves 708ms
   ✓ TabProvider tab groups > moveTabToGroup with index (drag drops) > syncs a bound-tab drag insert like any user reorder 715ms
 ✓ src/components/settings/channel-events-tab.test.tsx (15 tests) 920ms
 ✓ src/lib/unified-diff-generator.test.ts (9 tests) 19ms
 ✓ src/lib/collab-tool.test.ts (26 tests) 5ms
 ✓ src/lib/monaco-themes.test.ts (12 tests) 4ms
 ✓ src/lib/conversation-popout-detached-bootstrap.test.ts (23 tests) 6ms
stderr | src/hooks/use-acp-agents.test.ts > useAgentThinkingVisibility > selects thinking visibility without flashing the loaded value
An update to TestComponent inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/components/chat/composer/plain-text-content.test.ts (18 tests) 6ms
 ✓ src/stores/tab-store-close-mru.test.ts (9 tests) 7ms
stderr | src/hooks/use-acp-agents.test.ts > useAgentThinkingVisibility > refreshes thinking visibility after the shared agent event
An update to TestComponent inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/hooks/use-acp-agents.test.ts (6 tests) 445ms
 ✓ src/lib/export-conversation.test.ts (6 tests) 18ms
 ✓ src/components/conversations/session-details-dialog.test.tsx (14 tests) 515ms
 ✓ src/components/settings/delegation-settings.test.tsx (9 tests) 482ms
 ✓ src/lib/adapters/background-task-grouping.test.ts (7 tests) 6ms
 ✓ src/components/settings/cursor-config-panel.test.tsx (14 tests) 930ms
 ✓ src/lib/message-input-draft.test.ts (17 tests) 9ms
 ✓ src/hooks/use-workspace-state-store.test.ts (7 tests) 8ms
 ✓ src/lib/collab-collapse.test.ts (7 tests) 3ms
 ✓ src/lib/file-open-target.test.ts (32 tests) 7ms
 ✓ src/lib/tool-watchdog-diagnostic.test.ts (13 tests) 3ms
 ✓ src/components/import-sessions/import-sessions-window.test.tsx (10 tests) 1492ms
stderr | src/components/automations/automations-page.test.tsx > AutomationsPage (master-detail) > can open the gallery from New and cancel back to the detail
An update to RunHistory inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to RunHistory inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

stderr | src/components/automations/automations-page.test.tsx > AutomationsPage (master-detail) > keeps the header switch and surfaces Run now + Edit beneath the prompt
An update to RunHistory inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act
An update to RunHistory inside a test was not wrapped in act(...).

When testing, code that causes React state updates should be wrapped into act(...):

act(() => {
  /* fire events that update state */
});
/* assert on the output */

This ensures that you're testing the behavior the user would see in the browser. Learn more at https://react.dev/link/wrap-tests-with-act

 ✓ src/components/message/agent-tool-call.test.tsx (17 tests) 344ms
 ✓ src/components/conversations/sidebar-reorder-animation.test.ts (17 tests) 4ms
 ✓ src/hooks/use-scrollbar-safe-dismiss.test.ts (13 tests) 27ms
 ✓ src/lib/plan-parse.test.ts (21 tests) 4ms
 ✓ src/components/ai-elements/markdown-link.test.tsx (21 tests) 315ms
 ✓ src/components/layout/status-bar-update.test.tsx (15 tests) 424ms
 ✓ src/components/settings/skill-agent-matrix.test.tsx (13 tests) 714ms
 ✓ src/components/ai-elements/message-local-path-autolink.test.tsx (20 tests) 1393ms
 ✓ src/components/message/collab-agent-card.test.tsx (14 tests) 258ms
 ✓ src/components/message/user-message-segments.test.ts (23 tests) 8ms
 ✓ src/hooks/use-subsession-sync.test.ts (11 tests) 21ms
 ✓ src/components/chat/composer/suggestion/mention-suggestion.test.ts (25 tests) 147ms
 ✓ src/components/conversations/active-session-details.test.ts (17 tests) 5ms
 ✓ src/components/conversations/delegation-route-menu.test.tsx (12 tests) 1603ms
 ✓ src/components/message/file-reference-actions.test.tsx (18 tests) 462ms
 ✓ src/components/automations/automations-page.test.tsx (11 tests) 1386ms
   ✓ AutomationsPage (master-detail) > runs an automation from the list ⋯ menu 305ms
 ✓ src/hooks/use-connection.test.tsx (10 tests) 14ms
 ✓ src/components/chat/composer/reference-text.test.ts (26 tests) 4ms
 ✓ src/lib/adapters/tool-kind-classifier.test.ts (36 tests) 5ms
 ✓ src/components/chat/session-config-selector.test.tsx (7 tests) 761ms
 ✓ src/hooks/use-ime-safe-editor-value.test.tsx (8 tests) 21ms
 ✓ src/lib/file-tree-keyboard.test.ts (21 tests) 4ms
 ✓ src/lib/composer-copy-text.test.ts (27 tests) 87ms
 ✓ src/components/ai-elements/message-windows-file-link.test.tsx (8 tests) 661ms
 ✓ src/components/chat/completion-decision-card.test.tsx (8 tests) 265ms
 ✓ src/lib/delegation-run-snapshot.test.ts (11 tests) 64ms
 ✓ src/hooks/use-workspace-file-search.test.ts (5 tests) 30ms
 ✓ src/components/message/codex-search-tool-card.test.tsx (14 tests) 300ms
 ✓ src/components/ui/instant-collapsible.test.tsx (8 tests) 125ms
 ✓ src/lib/document-translate.test.ts (18 tests) 5ms
 ✓ src/lib/language-detect.test.ts (79 tests) 6ms
 ✓ src/lib/composer-draft-sanitize.test.ts (7 tests) 78ms
 ✓ src/components/message/goal-tool-call.test.tsx (8 tests) 143ms
 ✓ src/components/chat/agent-selector.test.tsx (6 tests) 169ms
 ✓ src/lib/conversation-title.test.ts (18 tests) 10ms
 ✓ src/components/layout/sidebar.test.tsx (12 tests) 480ms
 ✓ src/components/ai-elements/remark-autolink-local-paths.test.ts (10 tests) 4ms
 ✓ src/hooks/use-enabled-skill-ids.test.ts (4 tests) 349ms
 ✓ src/components/layout/aux-panel-git-changes-tab.test.tsx (14 tests) 3903ms
   ✓ GitChangesTab render cap > caps a large change set and offers to reveal the rest 969ms
   ✓ GitChangesTab render cap > reveals all rows once expanded 1054ms
   ✓ GitChangesTab render cap > re-caps when a fresh, larger change set replaces the revealed one 1725ms
stderr | src/components/conversations/conversation-detail-header.test.tsx > ConversationDetailHeader dialog target snapshot > toasts and keeps the rename dialog open when rename fails
[ConversationDetailHeader] rename: Error: db locked
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\conversations\conversation-detail-header.test.tsx:143:53
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

 ✓ src/components/chat/composer/reference-uri.test.ts (16 tests) 4ms
 ✓ src/stores/runtime-timeline-prefix-cache.test.ts (5 tests) 5ms
 ✓ src/hooks/use-connection-lifecycle.send-failure.test.ts (5 tests) 22ms
stderr | src/components/conversations/conversation-detail-header.test.tsx > ConversationDetailHeader dialog target snapshot > toasts and keeps the delete dialog open when delete fails
[ConversationDetailHeader] delete: Error: db locked
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\conversations\conversation-detail-header.test.tsx:162:48
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:146:14
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:533:11
    at runWithTimeout (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:39:7)
    at runTest (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1056:17)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runSuite (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1205:15)
    at runFiles (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1262:5)
    at startTests (file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/@vitest+runner@2.1.9/node_modules/@vitest/runner/dist/index.js:1271:3)
    at file:///D:/MyCodeBuddy/.worktrees/workflow-refresh-self-healing/node_modules/.pnpm/vitest@2.1.9_@types+node@25_a00748de01525b92f420576dcf1e92c3/node_modules/vitest/dist/chunks/runBaseTests.3qpJUEJM.js:126:11

 ✓ src/lib/custom-agents.test.ts (8 tests) 5ms
 ✓ src/stores/delegation-profile-store.test.ts (4 tests) 3ms
 ✓ src/components/chat/agent-plan-overlay.test.tsx (9 tests) 92ms
 ✓ src/components/conversations/conversation-detail-header.test.tsx (5 tests) 1819ms
   ✓ ConversationDetailHeader dialog target snapshot > deletes the conversation the dialog was opened for, even after the active tab switches 355ms
   ✓ ConversationDetailHeader dialog target snapshot > renames the conversation the dialog was opened for, even after the active tab switches 473ms
   ✓ ConversationDetailHeader dialog target snapshot > toasts and keeps the rename dialog open when rename fails 545ms
 ✓ src/components/message/conversation-message-nav.test.tsx (7 tests) 163ms
 ✓ src/components/conversations/group-shell-reconciliation.test.tsx (3 tests) 32ms
 ✓ src/lib/delegation-running-ticker.test.ts (10 tests) 6ms
 ✓ src/components/chat/composer/badges/reference-badge.test.tsx (8 tests) 43ms
 ✓ src/components/chat/composer/nodes/reference-node.test.tsx (11 tests) 264ms
 ✓ src/lib/search-output.test.ts (17 tests) 5ms
 ✓ src/lib/terminal/write-queue.test.ts (9 tests) 5ms
 ✓ src/stores/conversation-experience-store.test.ts (7 tests) 119ms
 ✓ src/components/ai-elements/remark-cjk-autolink-tail.test.ts (26 tests) 4ms
 ✓ src/lib/conversation-popout-acp-bridge.test.ts (10 tests) 7ms
stderr | src/app/pet-panel/_components/PetPanel.test.tsx > PetPanel > renders the session list with a header count when sessions exist
Received `true` for a non-boolean attribute `layout`.

If you want to write it to the DOM, pass a string instead: layout="true" or layout={value.toString()}.

 ✓ src/app/pet-panel/_components/PetPanel.test.tsx (7 tests) 60ms
 ✓ src/components/settings/agent-thinking-visibility-switch.test.tsx (5 tests) 198ms
 ✓ src/lib/open-delegated-child-session.test.ts (9 tests) 5ms
 ✓ src/hooks/use-message-queue.test.ts (7 tests) 24ms
 ✓ src/lib/clipboard-images.test.ts (12 tests) 5ms
 ✓ src/components/chat/permission-dialog.test.tsx (7 tests) 162ms
 ✓ src/components/settings/tool-watchdog-settings.test.tsx (6 tests) 239ms
 ✓ src/app/pet/_hooks/usePetSessions.test.ts (5 tests) 79ms
 ✓ src/app/conversation/_components/detached-bootstrap-flow.test.ts (9 tests) 2ms
stderr | src/components/ai-elements/code-block.test.tsx > CodeBlockContent > keeps raw code visible when Shiki rejects
Failed to highlight code: Error: shiki unavailable
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.test.tsx:155:22
    at getHighlighter (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.tsx:191:30)
    at startHighlight (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.tsx:219:19)
    at highlightCode (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.tsx:302:10)
    at D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.tsx:518:11
    at mountMemo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:8777:23)
    at Object.useMemo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:26216:18)
    at Proxy.process.env.NODE_ENV.exports.useMemo (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react@19.2.4\node_modules\react\cjs\react.development.js:1251:34)
    at CodeBlockContent (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\ai-elements\code-block.tsx:517:25)
    at Object.react_stack_bottom_frame (D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\node_modules\.pnpm\react-dom@19.2.4_react@19.2.4\node_modules\react-dom\cjs\react-dom-client.development.js:25904:20)

 ✓ src/components/ai-elements/code-block.test.tsx (5 tests) 135ms
 ✓ src/components/message/search-results-output.test.tsx (8 tests) 151ms
 ✓ src/components/chat/composer/submit-key.test.ts (26 tests) 3ms
 ✓ src/stores/runtime-live-message-slice-decoupling.test.ts (2 tests) 3ms
 ✓ src/components/chat/session-selectors.test.tsx (6 tests) 169ms
 ✓ src/components/files/office-preview.test.tsx (7 tests) 134ms
 ✓ src/lib/font-presets.test.ts (18 tests) 5ms
 ✓ src/components/ai-elements/message.test.tsx (6 tests) 58ms
 ✓ src/lib/terminal-reconnect.test.ts (11 tests) 3ms
 ✓ src/components/chat/conversation-shell.test.tsx (3 tests) 106ms
 ✓ src/components/diff/diff-viewer.test.tsx (2 tests) 41ms
 ✓ src/lib/history-window.test.ts (9 tests) 3ms
 ✓ src/components/chat/model-option-list.test.tsx (7 tests) 563ms
 ✓ src/lib/file-path-display.test.ts (16 tests) 3ms
 ✓ src/lib/file-tree-dnd.test.ts (14 tests) 13ms
 ✓ src/components/message/live-turn-stats.test.tsx (7 tests) 64ms
 ✓ src/lib/acp/streaming-performance-config.test.ts (5 tests) 4ms
 ✓ src/lib/pet/use-proxied-marketplace-asset.test.ts (5 tests) 330ms
 ✓ src/lib/open-folder-with-draft.test.ts (5 tests) 5ms
 ✓ src/lib/delegation-seed.test.ts (9 tests) 3ms
 ✓ src/hooks/use-delegation-card-model-hook.test.tsx (2 tests) 17ms
 ✓ src/lib/queue-flush.test.ts (17 tests) 4ms
 ✓ src/components/settings/codebuddy-config.test.ts (16 tests) 5ms
 ✓ src/components/ai-elements/heavy-plugins-warmup.test.tsx (7 tests) 24ms
 ✓ src/components/layout/aux-panel-session-details-tab.test.tsx (2 tests) 71ms
 ✓ src/lib/agent-plan.test.ts (5 tests) 2ms
 ✓ src/lib/fuzzy-text-match.test.ts (12 tests) 3ms
 ✓ src/lib/delegation-work-unit.test.ts (9 tests) 4ms
 ✓ src/components/ai-elements/rehype-allow-codeg.test.ts (17 tests) 6ms
 ✓ src/components/settings/add-custom-agent-dialog.test.ts (10 tests) 3ms
 ✓ src/components/conversations/sidebar-section-header.test.tsx (6 tests) 68ms
 ✓ src/components/message/streaming-markdown-document.test.tsx (7 tests) 47ms
 ✓ src/app/conversation/_components/detached-shell.test.tsx (4 tests) 18ms
 ✓ src/lib/codex-command-action.test.ts (15 tests) 5ms
 ✓ src/components/settings/skill-packs-settings.test.tsx (6 tests) 334ms
 ✓ src/components/connection/web-connection-guard.test.tsx (7 tests) 180ms
 ✓ src/components/message/reply-artifacts.test.tsx (4 tests) 175ms
 ✓ src/components/chat/question-dialog.test.tsx (6 tests) 131ms
 ✓ src/components/settings/general-settings.test.tsx (2 tests) 321ms
 ✓ src/hooks/use-feedback-enabled.test.ts (5 tests) 226ms
 ✓ src/lib/branch-switch.test.ts (12 tests) 3ms
 ✓ src/components/chat/conversation-context-bar.test.tsx (3 tests) 163ms
 ✓ src/lib/delegation-route-api.test.ts (7 tests) 5ms
 ✓ src/components/message/plain-text-with-badges.test.tsx (7 tests) 58ms
 ✓ src/components/message/background-task-card.test.tsx (5 tests) 147ms
 ✓ src/lib/file-tab-id.test.ts (19 tests) 6ms
 ✓ src/stores/tab-store-delegation-route.test.ts (5 tests) 6ms
 ✓ src/components/chat/composer/use-reference-search.test.ts (2 tests) 15ms
 ✓ src/lib/file-search-match.test.ts (9 tests) 3ms
 ✓ src/stores/tab-store-dispose-draft.test.ts (3 tests) 4ms
 ✓ src/components/message/composer-to-bubble-roundtrip.test.ts (3 tests) 89ms
 ✓ src/lib/transport/desktop-acp-events.test.ts (4 tests) 5ms
 ✓ src/lib/feedback-check.test.ts (10 tests) 5ms
 ✓ src/components/chat/delegate-access-status.test.tsx (9 tests) 70ms
 ✓ src/hooks/use-conversation-detail.test.tsx (2 tests) 18ms
 ✓ src/lib/codex-provider-model.test.ts (8 tests) 3ms
 ✓ src/lib/continuation-waiting.test.ts (5 tests) 24ms
 ✓ src/components/ai-elements/remark-file-uri-links.test.ts (11 tests) 4ms
 ✓ src/components/settings/delegation-agent-defaults.test.tsx (2 tests) 141ms
 ✓ src/components/layout/git-log-commit-message.test.tsx (3 tests) 47ms
 ✓ src/lib/file-tree-display-prefs.test.ts (5 tests) 58ms
 ✓ src/components/chat/composer/suggestion/adapters.test.ts (4 tests) 2ms
 ✓ src/lib/folder-display.test.ts (14 tests) 3ms
 ✓ src/components/chat/composer/suggestion/popup-position.test.ts (10 tests) 2ms
 ✓ src/components/message/collapsible-user-message.test.tsx (3 tests) 52ms
 ✓ src/lib/delegation-conversation-interrupted.test.ts (13 tests) 5ms
 ✓ src/lib/markdown/incremental-stream-blocks.test.ts (12 tests) 22ms
 ✓ src/components/chat/composer/editor-config.test.ts (4 tests) 90ms
 ✓ src/stores/command-terminal-link-store.test.ts (10 tests) 3ms
 ✓ src/contexts/workspace-dirty-close.test.ts (10 tests) 3ms
 ✓ src/components/workspace/pet-focus-bridge.test.tsx (3 tests) 263ms
 ✓ src/lib/cron-humanize.test.ts (10 tests) 3ms
 ✓ src/components/conversations/tile-scroll-container.test.tsx (4 tests) 24ms
 ✓ src/components/message/agent-capsule.test.tsx (7 tests) 130ms
 ✓ src/i18n/messages.test.ts (11 tests) 43ms
 ✓ src/lib/resource-kind.test.ts (43 tests) 6ms
 ✓ src/components/diff/unified-diff-preview.test.tsx (4 tests) 645ms
 ✓ src/components/chat/plan-approval-card.test.tsx (5 tests) 188ms
 ✓ src/components/layout/workspace-chrome-controller-source.test.ts (2 tests) 2ms
 ✓ src/components/chat/delegation-route-notice.test.tsx (4 tests) 59ms
 ✓ src/components/message/user-resource-links.test.tsx (4 tests) 100ms
 ✓ src/lib/turn-busy.test.ts (8 tests) 2ms
 ✓ src/components/workspace/deep-link-bootstrap.test.tsx (2 tests) 104ms
 ✓ src/components/project-boot/shadcn/shadcn-preview.test.tsx (4 tests) 80ms
 ✓ src/lib/workspace-file-api.test.ts (3 tests) 2ms
 ✓ src/components/message/read-output.test.ts (7 tests) 3ms
 ✓ src/components/settings/codebuddy-config-panel.test.tsx (2 tests) 132ms
 ✓ src/components/merge/conflict-parser.test.ts (8 tests) 3ms
 ✓ src/lib/path-utils.test.ts (16 tests) 3ms
 ✓ src/components/settings/agent-diagnostics-dialog.test.tsx (2 tests) 234ms
 ✓ src/stores/folder-derivation-decoupling.test.ts (3 tests) 1ms
 ✓ src/components/chat/file-mention-menu.test.tsx (5 tests) 88ms
 ✓ src/components/tabs/tab-strip-wiring.test.ts (3 tests) 2ms
 ✓ src/components/ai-elements/message-codeg-badge.test.tsx (4 tests) 102ms
 ✓ src/components/chat/message-input-attachments.test.ts (5 tests) 2ms
 ✓ src/components/chat/composer/invocation-reference.test.ts (6 tests) 4ms
 ✓ src/lib/perf/streaming-perf-report.test.ts (6 tests) 3ms
 ✓ src/lib/overlay-size-storage.test.ts (6 tests) 4ms
 ✓ src/components/layout/aux-panel-git-log-tab.test.ts (11 tests) 3ms
 ✓ src/components/message/plan-mode-card.test.tsx (5 tests) 95ms
 ✓ src/contexts/remote-connection-context.test.tsx (5 tests) 16ms
 ✓ src/lib/research-actions.test.ts (4 tests) 3ms
 ✓ src/lib/prompt-upload-strip.test.ts (7 tests) 2ms
 ✓ src/components/chat/session-config-stale-banner.test.tsx (5 tests) 108ms
 ✓ src/lib/notification.test.ts (4 tests) 4ms
 ✓ src/lib/claude-provider-model.test.ts (4 tests) 3ms
 ✓ src/lib/hermes-providers.test.ts (1 test) 4ms
 ✓ src/lib/pet/animation.test.ts (10 tests) 2ms
 ✓ src/lib/transport/web-event-stream.test.ts (2 tests) 3ms
 ✓ src/lib/agent-install-error.test.ts (14 tests) 2ms
 ✓ src/components/message/turn-stats.test.tsx (4 tests) 90ms
 ✓ src/lib/pet/session-display.test.ts (8 tests) 2ms
 ✓ src/lib/resolve-default-agent.test.ts (7 tests) 2ms
 ✓ src/hooks/use-file-tree.test.ts (7 tests) 2ms
 ✓ src/lib/window-chrome.test.ts (3 tests) 1ms
 ✓ src/components/chat/workflow-status-icon.test.tsx (21 tests) 59ms
 ✓ src/components/chat/message-queue-display.test.tsx (3 tests) 98ms
 ✓ src/lib/prompt-draft.test.ts (5 tests) 3ms
 ✓ src/components/layout/branch-selector-placement.test.ts (3 tests) 2ms
 ✓ src/lib/monaco-model-path.test.ts (8 tests) 3ms
 ✓ src/lib/cache/weighted-lru.test.ts (4 tests) 2ms
 ✓ src/components/conversations/search-command-dialog.focus.test.tsx (2 tests) 873ms
   ✓ SearchCommandDialog focus-before-open via openTab > routes selection through openTab and skips openConversations when focus short-circuits 493ms
   ✓ SearchCommandDialog focus-before-open via openTab > opens conversation pane when openTab opens/activates a main tab 379ms
 ✓ src/components/settings/delegation-profiles.test.tsx (2 tests) 177ms
 ✓ src/components/layout/aux-panel.test.tsx (8 tests) 3ms
 ✓ src/lib/tool-call-lifecycle.test.ts (6 tests) 2ms
 ✓ src/components/message/plan-card.test.tsx (5 tests) 69ms
 ✓ src/components/settings/session-feedback-settings.test.tsx (2 tests) 117ms
 ✓ src/components/automations/template-gallery.test.tsx (4 tests) 164ms
 ✓ src/lib/file-tab-memory.test.ts (5 tests) 2ms
 ✓ src/lib/background-agent.test.ts (4 tests) 3ms
 ✓ src/components/ai-elements/file-tree.test.tsx (3 tests) 114ms
 ✓ src/contexts/select-transcript-apply-events.test.ts (4 tests) 3ms
 ✓ src/lib/drag-selection-guard.test.ts (4 tests) 4ms
 ✓ src/components/chat/composer/clipboard-actions.test.ts (3 tests) 4ms
 ✓ src/components/chat/composer/inactive-selection.test.ts (5 tests) 109ms
 ✓ src/components/conversations/conversation-mutation-feedback-source.test.ts (4 tests) 2ms
 ✓ src/lib/keyboard-shortcuts.test.ts (3 tests) 2ms
 ✓ src/components/files/file-workspace-panel-ime-source.test.ts (4 tests) 2ms
 ✓ src/lib/skill-frontmatter.test.ts (4 tests) 3ms
 ✓ src/lib/delegated-child-tab-intent.test.ts (4 tests) 2ms
 ✓ src/components/terminal/terminal-close-confirm-dialog.test.tsx (4 tests) 188ms
 ✓ src/lib/context-window.test.ts (7 tests) 2ms
 ✓ src/components/settings/log-buffer.test.ts (6 tests) 2ms
 ✓ src/components/conversations/folder-alias-label.test.tsx (4 tests) 25ms
 ✓ src/lib/transport/index.test.ts (3 tests) 2ms
 ✓ src/lib/tab-drag-drop.test.ts (6 tests) 3ms
 ✓ src/lib/format-elapsed.test.ts (5 tests) 2ms
 ✓ src/contexts/workbench-route-context.test.tsx (2 tests) 34ms
 ✓ src/lib/scheduling/idle-work.test.ts (3 tests) 5ms
 ✓ src/lib/conversation-activity.test.ts (3 tests) 2ms
 ✓ src/components/layout/status-bar.test.tsx (2 tests) 23ms
 ✓ src/contexts/workspace-context-source.test.ts (3 tests) 2ms
 ✓ src/stores/backend-scoped-store-reset.test.ts (3 tests) 2ms
 ✓ src/components/chat/collapsed-overlay-chip.test.tsx (2 tests) 63ms
 ✓ src/lib/session-config-filter.test.ts (4 tests) 2ms
 ✓ src/contexts/ask-question-no-legacy-dialog.test.ts (3 tests) 2ms
 ✓ src/contexts/terminal-close-guard.test.ts (5 tests) 2ms
 ✓ src/lib/utils.test.ts (3 tests) 2ms
 ✓ src/lib/transport/detect.test.ts (2 tests) 1ms
 ✓ src/lib/session-config-display.test.ts (4 tests) 1ms
 ✓ src/lib/embedded-json.test.ts (5 tests) 2ms
 ✓ src/app/pet-panel/_components/PanelPermissionCard.test.tsx (2 tests) 83ms
 ✓ src/test-setup.test.ts (2 tests) 3ms
 ✓ src/components/ai-elements/message-file-uri-pipeline.test.tsx (2 tests) 61ms
 ✓ src/lib/agent-types.test.ts (1 test) 2ms
 ✓ src/components/layout/git-log-timeline.test.ts (2 tests) 1ms
 ✓ src/components/message/delegation-run-summary.test.tsx (1 test) 26ms
 ✓ src/components/tabs/tab-drag-ghost.test.tsx (2 tests) 16ms
 ✓ src/lib/eslint-config.test.ts (1 test) 1389ms
   ✓ ESLint workspace boundaries > ignores linked worktrees without ignoring the active checkout 1388ms

 Test Files  346 passed (346)
      Tests  5082 passed (5082)
   Start at  17:26:24
   Duration  31.11s (transform 20.16s, setup 39.56s, collect 189.34s, tests 79.89s, environment 233.95s, prepare 35.74s)
~~~

### `pnpm eslint .`

Exit code: `0`

~~~text

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\chat\composer\reference-search-controller.ts
  1002:5  warning  '_pageIndex' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\chat\message-input.test.tsx
  256:23  warning  '_data' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\conversations\sidebar-conversation-card.test.tsx
  19:12  warning  '_args' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\message\message-list-view.tsx
  1115:13  warning  '_isActive' is assigned a value but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\components\settings\acp-agent-settings.tsx
  411:3  warning  '_platform' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\acp-connections-context.tsx
  4954:5  warning  React Hook useCallback has a missing dependency: 'fireHandoffWatchersForRemoved'. Either include it or remove the dependency array                                      react-hooks/exhaustive-deps
  5285:5  warning  React Hook useCallback has missing dependencies: 'removeDeadCanonicalAndFireHandoff' and 'removeDeadCanonicalOnly'. Either include them or remove the dependency array  react-hooks/exhaustive-deps
  5393:5  warning  React Hook useCallback has missing dependencies: 'removeDeadCanonicalAndFireHandoff' and 'removeDeadCanonicalOnly'. Either include them or remove the dependency array  react-hooks/exhaustive-deps
  5542:5  warning  React Hook useCallback has a missing dependency: 'removeDeadCanonicalAndFireHandoff'. Either include it or remove the dependency array                                  react-hooks/exhaustive-deps
  7292:5  warning  React Hook useCallback has missing dependencies: 'clearHandoffWatcher' and 'scheduleOwnOrObserveOnBrokerRemoved'. Either include them or remove the dependency array    react-hooks/exhaustive-deps
  7419:5  warning  React Hook useCallback has a missing dependency: 'clearHandoffWatcher'. Either include it or remove the dependency array                                                react-hooks/exhaustive-deps
  7511:6  warning  React Hook useCallback has missing dependencies: 'cancelAllObserverDelays' and 'clearAllHandoffWatchers'. Either include them or remove the dependency array            react-hooks/exhaustive-deps
  8106:6  warning  React Hook useEffect has missing dependencies: 'connect' and 'setActiveKey'. Either include them or remove the dependency array                                         react-hooks/exhaustive-deps

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\contexts\workspace-context.test.tsx
  4306:9  warning  'switchFileTab' is assigned a value but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\hooks\use-connection.ts
  265:9  warning  The 'toolWatchdogProjections' logical expression could make the dependencies of useMemo Hook (at line 391) change on every render. Move it inside the useMemo callback. Alternatively, wrap the initialization of 'toolWatchdogProjections' in its own useMemo() Hook  react-hooks/exhaustive-deps

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\conversation-popout.test.ts
  1038:34  warning  '_cid' is defined but never used  @typescript-eslint/no-unused-vars
  1038:48  warning  '_op' is defined but never used   @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\delegation-conversation-interrupted.ts
  53:3  warning  '_isDelegatedChild' is assigned a value but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\delegation-transcript-projection.ts
  252:3  warning  '_parentConversationId' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\lib\perf\streaming-perf-recorder.test.ts
  284:45  warning  '_delay' is defined but never used  @typescript-eslint/no-unused-vars

D:\MyCodeBuddy\.worktrees\workflow-refresh-self-healing\src\stores\tab-store-popout.test.ts
    3:48  warning  '_id' is defined but never used  @typescript-eslint/no-unused-vars
    4:33  warning  '_id' is defined but never used  @typescript-eslint/no-unused-vars
    5:34  warning  '_id' is defined but never used  @typescript-eslint/no-unused-vars
    6:44  warning  '_id' is defined but never used  @typescript-eslint/no-unused-vars
  192:10  warning  '_id' is defined but never used  @typescript-eslint/no-unused-vars

✖ 25 problems (0 errors, 25 warnings)
~~~

### `pnpm build`

Exit code: `0`

~~~text
$ next build
▲ Next.js 16.1.6 (Turbopack)

  Creating an optimized production build ...
✓ Compiled successfully in 6.1s
  Running TypeScript ...
  Collecting page data using 23 workers ...
  Generating static pages using 23 workers (0/33) ...
  Generating static pages using 23 workers (8/33)
  Generating static pages using 23 workers (16/33)
  Generating static pages using 23 workers (24/33)
✓ Generating static pages using 23 workers (33/33) in 597.5ms
  Finalizing page optimization ...

Route (app)
┌ ○ /
├ ○ /_not-found
├ ○ /commit
├ ○ /conversation
├ ○ /import-sessions
├ ○ /login
├ ○ /merge
├ ○ /pet
├ ○ /pet-panel
├ ○ /project-boot
├ ○ /push
├ ○ /settings
├ ○ /settings/agents
├ ○ /settings/appearance
├ ○ /settings/chat-channels
├ ○ /settings/experts
├ ○ /settings/general
├ ○ /settings/logs
├ ○ /settings/mcp
├ ○ /settings/model-providers
├ ○ /settings/office-tools
├ ○ /settings/quick-messages
├ ○ /settings/science
├ ○ /settings/shortcuts
├ ○ /settings/skill-packs
├ ○ /settings/skills
├ ○ /settings/system
├ ○ /settings/version-control
├ ○ /settings/web-service
├ ○ /stash
└ ○ /workspace


○  (Static)  prerendered as static content
~~~

## Delivery Checks

~~~text
DELIVERY_BASE=f80ea84fb32cceaf4a0580658764e31965112439
HEAD=e3940f41c6bd7200442192d644b627d23945549f
BRANCH=feat/workflow-refresh-self-healing

git diff --check "$deliveryBase..HEAD"
# empty output; exit 0

git status --short
# empty output before report/card creation

design LF SHA-256
2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf
~~~

### Commit List

~~~text
4ca70896 docs: revise plan for mock exports and describe title
15a9eaca fix: reconcile delegation cards from run snapshots
6626d209 docs: report task 1 delegation reconciliation
526c73b7 fix: keep terminal delegation stats through work-unit merge
c7ed02c7 docs: record task 1 work-unit reconciliation fix
e3654d13 fix: refresh active workflow graphs from authority
84b916b5 docs: report task 2 authority refresh scheduling
d42869b1 fix: retry workflow event subscriptions
b8bd0693 docs: report task 3 event subscription recovery
4ad0d7b5 docs: report task 4 pre-final scope audit
752b06a7 fix: defer workflow event channel lookup
e3940f41 style: fix pre-existing prettier lint in chat helpers
~~~

### Product Change List

The workflow implementation remains exactly the four planned frontend files:

~~~text
src/hooks/use-delegation-card-model.test.ts
src/hooks/use-delegation-card-model.ts
src/lib/workflow-graph-store.test.ts
src/lib/workflow-graph-store.ts
~~~

Parent-authorized verification-only formatting additions:

~~~text
src/components/chat/sub-agent-overlay.tsx
src/lib/delegation-activity.ts
~~~

Full-range ancillary files are SDD reports/cards and the approved plan
revision. No Rust or backend contract file changed.

## Final Review

### Codex final audit

**APPROVE: 0 critical, 0 important.**

The final audit checked exact identity compatibility, complete stale-source
omission, lifecycle precedence, terminal non-reopening, authority delay
selection, revision/generation/epoch gates, and required-listener
pending/success/failure/disposal transitions. The focused channel-lookup repair
preserves the production constants and moves only their evaluation time.

### Independent Grok review

**APPROVE: 0 critical, 0 important, 2 minor process notes.**

Grok independently recomputed the Task 5 risk as `high`:
`broad_production_surface=1`,
`multiple_ownership_modules=1`, and `dependency_or_build=1`, total `3`,
with no hard triggers. It mapped every listed design regression to tests,
reviewed duplicate timer/listener and warning-latch edges, confirmed the
formatting commit is wrap/parenthesis-only, and found no protocol drift.

Minor notes:

1. The formatting-only allowlist expansion must remain recorded as explicit
   parent adjudication.
2. The 25 pre-existing ESLint warnings are not material to this delivery but
   remain repository cleanup debt.

No critical or important finding requires producer return.

## Delivery

- Automated tests: **passed**
- ESLint: **passed with 25 warnings**
- Static export build: **passed**
- Whitespace: **clean**
- Design digest: **matched**
- Remaining critical findings: **none**
- Remaining important findings: **none**
- Push/merge/PR: **not performed**
- Human acceptance: **pending after delivery**

Implementation card:
`.superpowers/sdd/2026-08-08-workflow-refresh-self-healing/task-5-implementation-card.html`
