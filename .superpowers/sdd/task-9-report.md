# Task 9 Report: Frontend Restart, Rollout, And Settings Surface Removal

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX AND GROK REVIEW PENDING**

- Work unit: `task|9|implementer|codex|none`
- Scope: Completion Protocol V2-Only plan Task 9 only
- Baseline HEAD: `218d23e5e18949873fd78426686b15c71a9d49ba`
- Producer commit: `bd011e818cec86c543744abf07df7f0e8c3ff6f5`
- Task 10 and later work: not started

## Implementation

- Deleted the frontend legacy restart API, response projection, settings
  snapshot/API, and restart-specific web capability mapping.
- Removed completion rollout status loading, rendering, and formatting from
  delegation settings; the panel now loads only delegation settings and the
  profile catalog.
- Made v1 workflow graphs strictly read-only by suppressing root and node
  completion decision/recovery cards and deleting restart/manual-resume
  controls, pending state, refresh logic, and mutation errors.
- Retained the required `CompletionProtocolWorkflowProjection` fields,
  historical source/successor links, read-only notice, and automatic wake.
- Retained valid v2 completion decisions and automatic-wake rendering.
- Retained ordinary conversation deletion through its existing API/sidebar
  ownership, outside workflow graph mutation controls.
- Deleted rollout, restart, and manual-resume translations from all 10
  locales while retaining historical read-only/link and valid v2 keys.
- Updated locale parity coverage to require the retained historical and v2
  keys. This necessary file was discovered by the plan-exact absence gate.

## TDD Evidence

Before production edits, the updated focused suite failed because the legacy
graph still rendered its restart button, delegation settings still requested
the removed settings API, and web transport still replayed a root capability
for `restart_legacy_workflow`.

After the implementation, the same suite passes with a v1 snapshot containing
both historical links plus root/node mutation projections: links and read-only
copy render, all mutation cards/buttons stay absent, and the supplied resume
callback remains untouched. A v2 snapshot still submits its typed completion
decision and renders automatic wake. The API test also retains the ordinary
`delete_conversation` transport contract.

## Verification

- Focused frontend and locale suite:
  - Pass: 5 files, 88 tests, 0 failures.
- Task-plan targeted ESLint command:
  - Pass: no errors.
- Plan-exact plus broader obsolete-symbol search:
  - Pass: no removed restart, settings, rollout, shadow, creation-count,
    override, sample/minimum, or manual-resume frontend symbols remain.
- Projection and locale retention gates:
  - Pass: required projection fields remain and all four retained historical
    or automatic-wake keys exist in all 10 locales.
- Locale JSON parse and key-parity checks:
  - Pass: all 10 locale files parse and have the same key set as English.
- `git diff --check` and producer allowlist review:
  - Pass: clean; producer commit contains exactly the 20 Task 9 files.

Vitest continues to print the existing Vite CJS API deprecation warning. It is
not caused by this diff. Task 9 used the plan-specified focused suite and
targeted ESLint rather than the full frontend build/test matrix.

## Producer Commit

- `bd011e818cec86c543744abf07df7f0e8c3ff6f5` -
  `refactor: remove legacy completion controls`

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Removed frontend legacy restart/rollout/settings surfaces; v1 graphs are read-only with history links, v2 decisions and automatic wake remain, ordinary deletion remains, and all 10 locale keysets stay valid.","commits":[{"sha":"bd011e818cec86c543744abf07df7f0e8c3ff6f5","subject":"refactor: remove legacy completion controls"}],"tests":{"status":"passed","passed":88,"failed":0,"summary":"Five focused frontend and locale test files passed; targeted ESLint, absence/retention gates, locale parsing, parity, and diff checks were clean."},"concerns":["The existing Vite CJS API deprecation warning remains outside this diff.","Independent Codex and Grok review is pending before Task 10."],"report_file":".superpowers/sdd/task-9-report.md"}
-->
