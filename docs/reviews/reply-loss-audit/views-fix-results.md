# Task 3: F7 / F8 / F9

## Progress

- Read the audit, implementation constraints, AGENTS.md, systematic-debugging, TDD and ponytail instructions. No provider/store edits, commits or subagents.
- Baseline readonly bridge suite: 2 passed; first test 2,202 ms, second about 2 ms. The first test dynamically imported the full view/UI module graph inside its 5-second test budget. Moved that import to module initialization; no timeout increase. Afterward the five bridge tests together ran in 29 ms (including expected red assertions).
- Red: viewer unmount deleted a sibling's real runtime state for both streaming and completed replies; standalone readonly frame delivery left the actual transcript projection empty.
- Red: a real runtime reply rendered before binding/remount, but the incoming bound-draft surface rendered no reply after a cross-group remount. The existing test harness now supports the real store for this regression; assertions inspect rendered text and runtime identity, not source code.
- Production changes applied: retain shared runtime on readonly viewer unmount, register the existing transcript frame sink, and initialize the surface runtime key from the retained tab runtime ID.
- Validation complete for the owned component scope.

## Changes

- F7: `useLiveTranscriptBridge` no longer calls `removeConversation` on unmount. Its provider sink disposer and metadata-sync cancellation still run. Regression cases retain a sibling consumer's exact real-store session for both streaming and completed replies; dialog coverage also checks released sink/store subscriptions and cancelled metadata sync.
- F8: `ConversationSessionSurface` initializes its stable runtime key using `ownTab.runtimeConversationId`, then the DB conversation ID, then the existing virtual-ID helper. The regression creates and completes a reply in a negative runtime, binds it to DB ID 42, unmounts the outgoing group, and verifies the incoming surface still renders that reply from the same runtime.
- F9: readonly registration includes `createLiveTranscriptFrameSink(conversationId, connectionId)`, matching the existing main-surface integration. The regression drives the registered canonical/projection sinks and checks the real subscribed projection after snapshot rebuild and incremental append, without a main surface. Canonical content remains intact too.
- Test setup: the readonly view import is static, so module transformation/loading completes before individual test execution. This removes dependency-loading latency from the default 5-second per-test budget; the production module graph itself is unchanged.

## Verification

All commands run from `D:/MyCodeBuddy` in PowerShell; no timeout override.

```powershell
pnpm exec vitest run src/components/message/live-transcript-view.test.tsx src/components/message/sub-agent-session-dialog.test.tsx src/components/conversations/conversation-session-surface.test.ts
pnpm exec eslint src/components/message/live-transcript-view.tsx src/components/message/live-transcript-view.test.tsx src/components/message/sub-agent-session-dialog.test.tsx src/components/conversations/conversation-session-surface.tsx src/components/conversations/conversation-session-surface.test.ts
pnpm exec tsc --noEmit --incremental false
```

- Component suites: **119 passed**, 3 files, exit 0. Final timing: readonly bridge 5 tests / 21 ms; dialog 23 / 289 ms; surface 91 / 829 ms; total command test-run duration 4.06 s.
- Narrow regression/cleanup selection: **9 passed**, 110 skipped; no React act warnings in this selection.
- ESLint on all five owned source/test files: exit 0. Prettier applied and scoped `git diff --check` passed.
- TypeScript: exit 1 with **36 diagnostics outside the owned files**, zero diagnostics in the five changed files. Examples include missing `hasLoadedSuccessfully` in browser test fixtures, duplicate `loadMessageInputDraftV2` imports, missing runtime fixture fields, and provider test fixture fields. These were not edited; concurrent workers/integration own their resolution.

## Remaining boundaries

- The complete component suite emits 39 React `act(...)` warnings from other existing surface cases, plus Vite's CJS deprecation warning. The new focused regression is warning-free; passing tests do not mean warning-free full-suite output.
- The readonly regression verifies actual projection state, not browser layout/virtualized pixel rendering. Provider frame admission and multiple-consumer registration remain the separately assigned provider work; no provider/store production files were changed here.
- Shared viewer data is intentionally retained until the owning session/tab lifecycle or store reset releases it. Viewer close is not a cache-eviction boundary.
- Full frontend build/suite and browser/end-to-end integration were not run by this worker; they remain controller-owned integration checks. No commits, resets, subagents, app restarts or deployment.
