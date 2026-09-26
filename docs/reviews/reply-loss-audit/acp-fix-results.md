# ACP reply preservation fixes

Scope: Task 2, F5/F6/F10/F12. Provider and provider tests only; public
`registerLiveSinks(contextKey, sinks): () => void` stays unchanged.

## Progress

- Read the audit, implementation plan, and systematic-debugging/TDD guidance.
- Traced frame admission, canonical/transcript publication, snapshot hydration,
  attach streaming queues, and event-ingestor pause/resume.
- Added permanent copies of the three audit variants without changing their
  original fixtures, preserving the standalone audit configuration.
- Red run: seven targeted cases failed for the intended behaviors (three audit
  variants, both sink disposal orders, transient recovery, and alias preservation).
- Green run: all seven targeted cases passed.

## Concrete changes

- F5: frame preparation records runtime IDs whose earlier terminal event was
  admitted. Later completions see those owners as completed instead of consulting
  the previous frame's stale live message. Existing session and user-stop guards
  remain in force.
- F6: gap snapshot hydration flushes accepted streaming text first. Existing idle
  settlement then transfers it into local history for a completed snapshot.
  A permanent test checks both canonical delivery exactly once and real runtime
  history, beyond the original audit's publication-only check.
- F10: each context key holds independent sink registrations. Publication,
  snapshot replay, completion checks, transcript clearing, and rekeying handle all
  registrations. Disposers remove only their own registration, including after
  rekeying; registering another sink does not replay existing consumers.
- F12: failed snapshot reads preserve the connection, aliases, accepted text,
  cursor, and queued envelopes. One recovery per connection retries after one
  second, resumes at the applied snapshot cursor, and delivers buffered events.
  Streaming gap envelopes now enter the existing ingestor instead of being lost.
  Retry timers are cleared on unmount; late responses check ingestor identity.
  Confirmed-null snapshots retain existing removal/handoff behavior.

## Final validation

- Provider + event-ingestor suites: **448 passed** (425 provider, 23 ingestor).
- Original standalone ACP audit configuration: **3 passed**; fixtures unchanged.
- Changed-file ESLint: exit 0, no errors, 11 pre-existing hook dependency warnings
  (the HEAD provider also reports 11 hook dependency warnings).
- Prettier check and scoped `git diff --check`: passed.
- Full `tsc --noEmit --incremental false` is blocked by existing fixture/type
  errors across the repository. Final filtered diagnostics contain no provider
  production errors or errors in the added regressions; existing provider test
  fixtures still lack error-envelope fields and `steeredMessageIds`. Other
  repository failures include missing `hasLoadedSuccessfully`, duplicate
  `loadMessageInputDraftV2`, and incomplete runtime fixtures.
- Final repeat after formatting/test typing correction: **448 passed** plus
  **3 standalone audit cases passed**; Prettier and scoped diff checks pass.

Reproduce:

```powershell
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx src/lib/acp/event-ingestor.test.ts
pnpm exec vitest run --config docs/reviews/reply-loss-audit/acp-variants.config.ts
pnpm exec eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
pnpm exec prettier --check src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx
pnpm exec tsc --noEmit --incremental false
```

## Scope and limits

- No event-ingestor source changes, runtime-store edits, component edits, commits,
  subagents, app restarts, or deployment.
- Browser/desktop end-to-end validation and full integration remain controller
  work. Persistent snapshot errors retain buffered work and keep retrying while
  the provider is mounted; recovery still requires a successful snapshot read.

## Review follow-up: R1 / R2

Read the full `event-view-review.md` and reproduced both findings with real
runtime/projection state before changing production code.

### R1: stale external ID with two terminals in one frame

- Permanent paired regression uses virtual runtime `-1049581191`, DB binding 7,
  stale external ID `stale-persisted-sess`, active connection session `sess-1`,
  and a second DB runtime carrying the active external ID.
- Red: A's text arrived in frame 1; delivering A completion plus all of B in
  frame 2 dropped B from virtual history. Moving only B's completion into frame 3
  passed. Assertions check both completed replies in real local history.
- Fix: frame preparation now records the session ID for each admitted completed
  runtime. Later boundaries may reuse that proven ownership only for that same
  session, with the connection's known session also matching. Wrong-session,
  connection mapping, and stale user-stop checks still apply.
- Green: both same-frame and split-frame controls pass. The full provider suite,
  including wrong-session and stale-stop regressions, passes.

### R2: independent sinks sharing one projection

- Red: two real `createLiveTranscriptFrameSink(42, connectionId)` instances
  produced `answer Aanswer A` in the singleton projection. Four cases cover a
  shared context key versus canonical/alias keys and disposal of either sink.
- Final provider regressions use desktop batches and real canonical/runtime and
  projection stores. They verify initial publication, another delta while both
  subscriptions exist, idempotent disposal, and another delta arriving once in
  the surviving subscription's shared projection.
- Fix: `liveTranscriptStore.publish` ignores already-applied/older cursors for
  the same connection and message. A changed connection/message rebuilds from
  canonical. A replay overlapping the current cursor also rebuilds from canonical
  because compacted deltas can include both previously applied and new text.
  Explicit snapshot rebuilds remain available, including equal-cursor recovery.
- Added projection regressions for duplicate/older publication, completion,
  overlapping compacted replay, resumed connections with lower cursors,
  equal-cursor rebuild, and changed message identity. Three failed before the
  production fix; the explicit rebuild/message-change control already passed.

### Follow-up validation

- Focused red: provider **5 failed / 1 passed**; projection **3 failed / 1 passed**.
- Focused green: **10 passed**.
- Full requested suites: **487 passed** (431 provider, 23 ingestor,
  18 projection store, 15 projector).
- Original ACP audit variants: **3 passed**; original fixtures/config unchanged.
- Changed-file ESLint: **0 errors**, same **11 existing provider hook warnings**.
- Formatting and scoped diff checks pass. No additional dependencies, provider
  API changes, commits, subagents, history-store changes, or backend changes.
- This follow-up does not claim full app build, browser validation, or a clean
  repository-wide TypeScript check; the previously recorded fixture errors remain
  outside this change. Controller re-review remains pending.

```powershell
pnpm exec vitest run src/contexts/acp-connections-context.test.tsx src/lib/acp/event-ingestor.test.ts src/stores/live-transcript-store.test.ts src/lib/acp/live-transcript-projector.test.ts
pnpm exec vitest run --config docs/reviews/reply-loss-audit/acp-variants.config.ts
pnpm exec eslint src/contexts/acp-connections-context.tsx src/contexts/acp-connections-context.test.tsx src/stores/live-transcript-store.ts src/stores/live-transcript-store.test.ts
```
