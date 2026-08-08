# Task 3 Report: Required Workflow Event Subscription Recovery

## Status

**IMPLEMENTATION COMPLETE; AUTOMATED VERIFICATION DEFERRED**

The workflow graph store now recovers the two required event subscriptions for
`workflow_graph://changed` and `workflow_graph://compatibility_nudge`. Each
channel owns a keyed listener slot, while both channels share one five-second
retry timer for the active install generation.

Retries target only missing, non-pending slots. A successful sibling remains
installed, each failed channel warns once per install generation, and a later
success resets its warning latch. Final global lease release clears the shared
retry timer, refresh timers, slot state, warning latches, and stored disposers.
Late subscription results from older install generations are disposed without
overwriting current-generation state.

The optional `completion_decision_resolved` listener retains its prior
best-effort behavior and never keeps the required-listener retry loop alive.

## Commit

| SHA | Subject |
| --- | --- |
| `d42869b183930ea69cf34dc94110355c349ad940` | `fix: retry workflow event subscriptions` |

The producer commit contains exactly:

- `src/lib/workflow-graph-store.ts`;
- `src/lib/workflow-graph-store.test.ts`.

## Regression Coverage

Tests were written before production changes. Added or updated coverage records
the intended behavior for:

- production event names passing through the explicit API mock;
- one warning per failed required channel and one shared five-second timer;
- retrying only a missing listener while retaining its successful sibling;
- final lease release clearing retry, refresh, and warning-generation state;
- durable ten-minute polling continuing while listener retries remain pending;
- pending-listener readiness and late-success disposal across generations; and
- monotonic install generations and stale lease isolation.

## Verification

**Tests, ESLint, TypeScript compilation, and builds were not run.** Task 3
constraints defer all automated red/green and broader verification to Task 5.

Deferred red/green command:

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected Task 5 result: failed required channels retry every five seconds,
successful siblings remain single, warnings latch per generation, durable
refresh continues, and final release leaves zero timers.

Fresh static evidence:

- approved design LF-normalized SHA-256 matched
  `2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf`;
- Prettier reported both producer files use the expected code style;
- `git diff --check` and `git diff --cached --check` reported no whitespace
  errors; and
- producer commit inspection confirmed exactly the two allowlisted files.

## Review Package

- Base: `84b916b55de19799321b180aa9b30a94a0240bd7`
- Producer: `d42869b183930ea69cf34dc94110355c349ad940`
- Risk: `high`
- Trigger: `concurrency_lifecycle`
- Reviewers: `codex + grok`
- Policy: `b2d_task_risk_v1`

The package is ready for the scheduled Task 3 reviewers. No human acceptance
gate was added.

## Concerns

No implementation concern identified. Runtime correctness evidence remains
intentionally deferred to Task 5.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Recovered required workflow event subscriptions with one shared five-second retry loop, generation-latched warnings, and generation-safe disposal.","commits":[{"sha":"d42869b183930ea69cf34dc94110355c349ad940","subject":"fix: retry workflow event subscriptions"}],"tests":{"status":"not_run","passed":0,"failed":0,"summary":"Deferred to Task 5 by workflow constraint; static checks passed."},"concerns":[],"report_file":".superpowers/sdd/2026-08-08-workflow-refresh-self-healing/task-3-report.md"}
-->
