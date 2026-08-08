# Task 2 Report: Active Workflow Authority Refresh Scheduling

## Status

**IMPLEMENTATION COMPLETE; AUTOMATED VERIFICATION DEFERRED**

Task 2 now keeps visible active workflow graphs converging from the durable
authority every 15 seconds. Active eligibility covers
`overall_state === "in_progress"` plus nodes in `reserving`, `running`,
`waiting_review`, or `waiting_adjudication`.

The existing `fallbackTimer` remains the sole refresh timer owner for each
active conversation. Its delay is selected after a current fetch completes:

- 15 seconds for active workflow state;
- 10 minutes for expanded interest or an undiscovered overlay; and
- no timer for a settled, discovered, overlay-only graph.

The existing `graph_revision`, request-generation, and activation-epoch gates
remain in place ahead of timer re-arming.

## Commit

| SHA | Subject |
| --- | --- |
| `e3654d13` | `fix: refresh active workflow graphs from authority` |

The producer commit contains exactly:

- `src/lib/workflow-graph-store.ts`;
- `src/lib/workflow-graph-store.test.ts`.

## Regression Coverage

Tests were written before the production scheduler change. Added or clarified
coverage records the intended behavior for:

- active numbered overlays refreshing at 15 seconds and stopping after a
  newer settled revision is applied;
- each active node status and an `in_progress` graph without active rows;
- settled numbered overlays owning no refresh timer;
- expanded and undiscovered graphs retaining the 10-minute fallback;
- failures, stale request generations, and old activation epochs retaining
  their existing scheduler gates; and
- lease release preventing late completions from re-arming timers.

## Verification

**Tests, ESLint, and builds were not run.** Task 2 constraints defer all
automated red/green and broader verification to Task 5.

Deferred red/green command:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected Task 5 result: active graphs refetch at 15 seconds, settle without
rearming, and the explicit 10-minute cases retain their prior behavior.

Fresh static evidence:

- approved design LF-normalized SHA-256 matched
  `2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf`;
- Prettier reported both producer files use the expected code style;
- `git diff --check` and `git diff --cached --check` reported no whitespace
  errors; and
- producer commit inspection confirmed no additional timer field or file.

## Review Package

- Base: `c7ed02c74ae4df45e18960f492b275f7c9a1a3e5`
- Producer: `e3654d13`
- Risk: `high`
- Trigger: `concurrency_lifecycle`
- Reviewers: `codex + grok`
- Policy: `b2d_task_risk_v1`

The package is ready for the scheduled Task 2 reviewers. No human acceptance
gate was added.

## Concerns

No implementation concern identified. Runtime correctness evidence remains
intentionally deferred to Task 5.
