# Task 9 Independent Codex Review

## Findings

### Minor: T9-CODEX-M1 - Manual-resume plumbing remains as a dead production interface

`src/components/chat/workflow-graph-panel.tsx:61-63` still declares the now-unused
`conversationId` and `onResumeRoot` props. `onResumeRoot` remains threaded through
`src/components/chat/sub-agent-overlay.tsx:144,366,859`,
`src/components/message/message-list-view.tsx:153,1044,1058,1105,1129,1921,1933`,
and `src/components/conversations/conversation-session-surface.tsx:2201-2206,2252`.
The conversation surface also retains the manual-resume prompt constant at line
104, and its test still proves that the dead callback can send that prompt.

The graph exposes no button that invokes this callback, so this is not a current
v1 mutation affordance. It is nevertheless obsolete manual-resume production
surface left behind by Task 9 Step 4 and conflicts with the report's broad
no-obsolete-symbol claim. Remove the graph-only `conversationId` prop, the full
`onResumeRoot` prop chain, the prompt callback/constant, and the now-obsolete
callback test coverage.

### Minor: T9-CODEX-M2 - The v1 UI regression fixture uses a retired restart error code

`src/components/chat/workflow-overlay.test.tsx:451` sets `read_only_reason` to
`legacy_completion_protocol_restart_required`. That restart-family code is
retired by the approved design; the historical projection contract is
`legacy_completion_protocol_read_only`. Because the component checks only
truthiness, the test passes while carrying obsolete protocol vocabulary. Update
the fixture to the post-change stable code so the regression represents the
actual backend projection.

Critical: 0. Important: 0. Minor: 2.

## Verdict

`approve_with_minors`

## Review Identity

- `reviewed_task_id`: `291c8192-3f96-43f6-b031-2b5511f7f8ee`
- Producer: `bd011e81`
- Producer commit: `bd011e818cec86c543744abf07df7f0e8c3ff6f5`
- Reviewed HEAD/report commit: `180218a488d3fb95a61f9cc649fc2a7885b37932`
- Scope: Plan Task 9, independent HIGH Codex review only; no product changes

## Contract Review

- A version-1 snapshot suppresses root and node completion decision/recovery
  cards. Restart/manual-resume buttons, pending/error state, restart API calls,
  settings API/types, rollout display, and restart capability replay are gone.
- Historical read-only copy and both persisted relationship links still render
  and invoke root-conversation navigation. The required projection fields,
  including `creation_mode` and `automatic_root_wake`, remain typed.
- A version-2 snapshot still renders and submits a typed completion decision;
  automatic-root-wake copy remains visible and the capability token still
  replays by attention id.
- Ordinary conversation deletion remains in its independent API and header/
  sidebar owners. The existing header interaction regression also passes.
- All ten locale JSON files parse, have parity with English, retain historical
  link/read-only and v2 automatic-wake keys, and preserve link placeholders.
- Plan-exact removed frontend API/UI/translation symbol search is clean. The two
  broader obsolete remnants are reported above.

## Verification Evidence

Fresh verification at reviewed HEAD:

- Task 9 focused suite: 5 files, 88 passed, 0 failed.
- Conversation detail header deletion suite: 1 file, 5 passed, 0 failed.
- Plan-targeted ESLint: passed with no errors.
- Locale JSON parse, key parity, and historical-link placeholder checks: passed
  for all 10 locales.
- Producer `git diff --check`: clean.
- Producer scope: exactly the 20 expected Task 9 files; report tip changes only
  `.superpowers/sdd/task-9-report.md`.

Vitest emitted the existing Vite CJS API deprecation warning. Expected error-path
logging appeared in the conversation-header tests; both negative cases passed.

Conclusion: approve_with_minors

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"291c8192-3f96-43f6-b031-2b5511f7f8ee","producer_commit":"bd011e818cec86c543744abf07df7f0e8c3ff6f5","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"Task 9 removes legacy restart/settings UI and keeps v1 history read-only, links, v2 decisions/wake, deletion, and locale parity. Two minors remain: dead manual-resume plumbing and one retired error-code fixture.","report_file":".superpowers/sdd/task-9-review-codex-report.md"}
-->
