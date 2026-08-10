# Task 3 Report: Classify Runtime Drift and Clear the Frontend Fence

## Status

**done_with_concerns - implementation complete; independent Codex and Grok review pending**

- Work unit: `task|3|implementer|codex|none`
- Scope: TypeScript wire key, runtime-restart classifier, and drift-only
  frontend fence cleanup
- Baseline HEAD: `087d0acb994efb2bfb321ad9724e280e357dd53e`
- Task 2 producer: `4f5a7136ad87761f801564b09fdf13fb977f9926`
- Producer commit: `bb24727a2ef6308ee811025fc76657c3b1044699`
- Task 4 notification and relaunch UX: not started, as required

## Implementation

- Exported
  `CONVERSATION_POPOUT_RUNTIME_RESTART_REQUIRED_I18N_KEY` beside the
  conversation-window API with the exact Rust wire value and lockstep comment.
- Added `PopOutRuntimeRestartRequiredError` and
  `isPopOutRuntimeRestartRequiredError` for downstream UI classification.
- Classified backend command errors through `extractAppCommandError` and the
  exported constant, without duplicating the stable literal in production.
- Changed only the desktop `openConversationWindow` rejection path: Runtime
  drift cancels both listeners, compare-and-clears the transfer fence, and
  throws the recognizable error.
- Kept generic open failures on the existing compensation path and left the
  pure-web branch unchanged.
- Added regression coverage proving Runtime drift performs no abort, status
  poll, close, or detached-tab restore while generic failure still aborts.

## TDD Evidence

The API lockstep test and the two orchestration regressions were added before
production changes. The focused RED command reported 2 expected failures and
51 passing tests: the TypeScript constant was `undefined`, and the runtime
restart classifier was absent. The generic compensation sibling already
passed, establishing the behavior that the branch split had to preserve.

After the minimal production implementation, the same focused command passed
all 53 tests. The broader plan command then passed all 63 API, orchestration,
and ACP bridge tests.

## Verification

- `pnpm test -- src/lib/api.test.ts src/lib/conversation-popout.test.ts`
  - Pass: 2 files, 53 tests, 0 failures.
- `pnpm test -- src/lib/api.test.ts src/lib/conversation-popout.test.ts src/lib/conversation-popout-acp-bridge.test.ts`
  - Pass: 3 files, 63 tests, 0 failures.
- `pnpm eslint src/lib/api.ts src/lib/api.test.ts src/lib/conversation-popout.ts src/lib/conversation-popout.test.ts`
  - Pass: exit 0, 0 errors; 2 pre-existing unused-parameter warnings in an
    unchanged test callback.
- Stable-key occurrence review and `git diff --check`
  - Pass: the literal is confined to the TypeScript declaration and explicit
    tests; the classifier consumes the constant; the producer commit changes
    only the four Task 3 files.

## Commits

- `bb24727a2ef6308ee811025fc76657c3b1044699` -
  `fix: bypass pop-out compensation for runtime drift`

## Concerns

- Independent Codex and Grok review is pending before Task 4 admission.
- Scoped ESLint retains two pre-existing `_cid` / `_op` unused-parameter
  warnings in `conversation-popout.test.ts`; there are no lint errors.
- Vitest emits the existing Vite CJS deprecation warning and expected stderr
  from compensation-path diagnostic tests; all selected tests pass.

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the TypeScript runtime-restart wire contract and recognizable error, clearing only the matching frontend transfer fence for WebView2 drift while preserving generic compensation and pure-web behavior.","commits":[{"sha":"bb24727a2ef6308ee811025fc76657c3b1044699","subject":"fix: bypass pop-out compensation for runtime drift"}],"tests":{"status":"passed","passed":63,"failed":0,"summary":"The required 53-test focused command and the broader 63-test API/orchestration/ACP bridge command passed; scoped ESLint exited 0 with no errors and two pre-existing warnings; stable-key and diff checks passed."},"concerns":["Independent Codex and Grok review is pending before Task 4.","Scoped ESLint retains two pre-existing unused-parameter warnings in an unchanged test callback.","Vitest retains existing Vite CJS and expected diagnostic stderr output."],"report_file":".superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-3-report.md"}
-->
