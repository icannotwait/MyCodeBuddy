# Task 1 Report: Exact-Run Delegation Reconciliation

## Status

**DONE**

Task 1 now reconciles delegation cards from an exact matching terminal run
snapshot. A terminal binding remains authoritative, terminal live meta removes
a running binding, and a matching terminal snapshot removes all stale running
binding/meta fields before the card model is merged. Missing or mismatched live
task identities fail closed.

`scopeDelegationBindingForCard` is unchanged. The existing 15-second
`useDelegationRunSnapshot` refresh interval and terminal-stop behavior are also
unchanged.

## Commits

| SHA | Subject |
| --- | --- |
| `15a9eaca` | `fix: reconcile delegation cards from run snapshots` |

## Tests

**NOT RUN (deferred).** Test execution is deferred to Task 5 per the user and
Task 1 brief constraints.

Regression coverage was added for:

- matching completed snapshots replacing every stale running binding field;
- matching failed snapshots replacing stale running live meta;
- mismatched terminal snapshots leaving running bindings unchanged;
- terminal snapshots failing closed when live meta has no task ID;
- terminal live meta outranking a running binding and snapshot; and
- terminal live bindings outranking lower running snapshots.

Deferred command:

```powershell
pnpm test -- src/hooks/use-delegation-card-model.test.ts
```

Read-only/static checks completed:

- approved design LF-normalized SHA-256 matched
  `2ad2ed367c50ea9cb7c01675dbf5dcf8bbcefb43c2960d278f2d26454fdb84cf`;
- `git diff --check` reported no whitespace errors before the producer commit;
- the producer commit contains exactly
  `src/hooks/use-delegation-card-model.ts` and
  `src/hooks/use-delegation-card-model.test.ts`; and
- Prettier reported both Task 1 files unchanged.

## Concerns

None. Automated correctness evidence remains intentionally deferred to Task 5.

## Fix Round 1: Work-Unit Reconciliation

**Review:** Codex requested one important change; Grok approved.

The final work-unit merge reparsed the raw current source after
`buildDelegationCardModel` had selected a matching terminal snapshot. Because
the runtime projection keeps per-run peaks, stale running meta counters and
file rollups could replace the coherent terminal snapshot fields for the same
task.

Same-task source observations now take lifecycle, error, timestamps, and
runtime stats from the already-reconciled current model before aggregation.
The raw activity anchor remains available for sticky orphan handling, and
observations with distinct task IDs continue to aggregate across continuation
runs.

A final-merge regression supplies stale running meta with 99 tool calls and
larger file totals, then asserts that the rendered work-unit model retains the
terminal snapshot's 12 tool calls, runtime stats, and edit rollup.

| SHA | Subject |
| --- | --- |
| `526c73b7` | `fix: keep terminal delegation stats through work-unit merge` |

**Tests: NOT RUN (deferred).** The focused suite remains deferred to Task 5 per
the user constraint. Prettier reported both modified files unchanged, and
`git diff --cached --check` reported no whitespace errors before the fix
commit.
