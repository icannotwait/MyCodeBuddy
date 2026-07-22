# Task 7 Report - Frontend + Snapshot DTO

## Status

DONE_WITH_CONCERNS

## Commits

- `861e65b17527548d6211de17ef4912cbfa876c79` - `feat(ui): per-run cards, snapshot DTO, continue tool chrome`
- `0b7564ff6bf8124ee1cb9d6c96fbc7c793ca6ef5` - `fix(ui): continue agent-like chrome and fail-closed projection gate`

## Summary

Implemented immutable, per-run delegation cards and the parent-scoped
`get_delegation_run_snapshot(task_id)` DTO for both Tauri and Axum. Historical
cards resolve durable run data by task id, validate summary data defensively,
and cannot be overwritten by a later run sharing the same child conversation.

Both `delegate_to_agent` and `continue_delegation` now use delegation card
chrome. The overlay groups reusable runs by child conversation, displays run
count/latest state, keeps replacements separate, and uses cold snapshots when
tool metadata has no child id. Structured review and implementation summaries
render across all ten locales with responsive and RTL-safe layout. A selected
run's optional child turn anchor is passed through the session dialog and
scrolls to the matching persisted child turn when available.

## Verification

| Command | Result |
| --- | --- |
| `pnpm exec vitest run src/components/chat/sub-agent-overlay.test.tsx src/components/message/content-parts-renderer.test.tsx src/components/message/delegated-sub-thread.test.tsx src/components/message/sub-agent-session-dialog.test.tsx src/components/message/message-list-view.test.tsx src/hooks/use-delegation-card-model.test.ts src/lib/delegation-run-snapshot.test.ts src/lib/delegation-binding-reduce.test.ts src/lib/tool-call-normalization.test.ts` | PASS - 9 files, 233 tests |
| `pnpm exec eslint <all touched TypeScript/TSX files>` | PASS |
| `pnpm build` | PASS - static export and TypeScript completed |
| `cargo test --features test-utils --test delegation_run_snapshot` | PASS - 1 test |
| `cargo check` | PASS - desktop/Tauri mode |
| `cargo check --no-default-features --bin codeg-server` | PASS - web/server mode |
| `cargo clippy --lib --features test-utils -- -D warnings` | PASS |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | PASS |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | FAIL - six pre-existing Task 6 test/helper lints in `broker.rs` and `run_store.rs`; no Task 7 file is reported |

The focused Vitest output contains the existing intentional malformed-input
diagnostic from overlay fallback tests. It has no failed assertions.

## Files

Backend:

- `src-tauri/src/commands/delegation.rs`
- `src-tauri/src/web/handlers/delegation.rs`
- `src-tauri/src/web/router.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/tests/delegation_run_snapshot.rs`

Frontend:

- `src/lib/delegation-run-snapshot.ts` and test
- `src/lib/types.ts`, `src/lib/api.ts`
- `src/lib/delegation-binding-reduce.ts` and test
- `src/lib/delegation-card.ts`, `src/lib/tool-call-normalization.ts` and tests
- `src/hooks/use-delegation-card-model.ts` and test
- `src/components/message/delegation-run-summary.tsx`
- delegation card, renderer, message-list, session-dialog, and overlay sources/tests
- all ten `src/i18n/messages/*.json` files

## Brief Self-Review

| Brief requirement | Result |
| --- | --- |
| Parent-authorized immutable snapshot DTO | Pass. Query filters `(parent_conversation_id, task_id)` and returns not-found for another parent. |
| Desktop and web surfaces | Pass. Tauri command registration plus Axum handler/route share one core function. |
| Optional `DelegationCompleted.card_summary` | Pass. Rust/TS event types exist; the binding reducer validates and stores it only on the matching card. |
| Separate cards for initial and continued runs | Pass. Tool normalization and renderer recognize both tool names. |
| Same-child immutability | Pass. Task-scoped snapshots/cache freeze terminal data, and stale child projections are suppressed for a different task id. |
| Invalid summary fallback | Pass. Rust omits malformed persisted JSON; client normalization produces a status-only card. |
| Overlay grouping/replacements | Pass. Snapshot-aware grouping shows run count/latest source; replacements remain marked separate rows. |
| Responsive and RTL smoke | Pass. Summary layout uses constrained responsive flex/break classes; Arabic test asserts `dir="auto"` and safe sizing. |
| Turn focus | Pass. Inline and overlay cards pass the selected anchor to the dialog; message-list test verifies exact-turn scrolling. |
| TDD | Current-session regressions for stale running snapshots, cold grouping, and anchor handoff/focus were observed failing before their fixes. Initial inherited WIP red-phase history was not reconstructible. |

## Concerns

1. The full all-target Rust clippy command remains red only on six existing
   Task 6 `broker.rs`/`run_store.rs` test/helper lints. The narrowed desktop and
   server clippy commands covering Task 7 pass.
2. Responsive and RTL checks are component layout smoke tests, not browser
   screenshot tests. Live multi-run browser e2e is reserved for Task 9.
3. `child_turn_anchor` is currently usually null at persistence time; its
   optional focus path is implemented and tested for when a producer supplies
   one.

## Out Of Scope

- Task 8 skill markdown/routing work.
- Task 9 live-agent end-to-end fixtures and visual browser e2e.
- Reopening Task 6 broker race work or its existing all-target clippy findings.

---

## Codex review follow-up (Important fixes)

**Status:** DONE  
**Commit:** `0b7564ff6bf8124ee1cb9d6c96fbc7c793ca6ef5`  
**Message:** `fix(ui): continue agent-like chrome and fail-closed projection gate`

### Important 1 — `continue_delegation` must show per-run card

**Root cause:** `isAgentLikeToolName` listed `delegate_to_agent` but omitted
`continue_delegation`, so `groupConsecutiveToolCalls` folded historical continues
into a generic collapsible tool-group; the card only mounted after expansion.

**Fix:** treat bare + host-prefixed `continue_delegation` as agent-like in
`tool-kind-classifier.ts` (same break-run path as `delegate_to_agent`). Adapter-path
tests in `ai-elements-adapter.test.ts` assert continues stay standalone
`tool-call` parts, not `tool-group` items.

### Important 2 — projection gate fails open when `projection.taskId` is null

**Root cause:** `runScopedChildProjection` only rejected non-null mismatched task
IDs. With a known card `task_id` and a null projection `taskId`, lifecycle/stats
still applied (fail open).

**Fix:** when `knownTaskId` is set, require exact `projection.taskId === knownTaskId`;
null/undefined/mismatch ignores run-scoped projection fields (session title still
allowed). Regression: terminal meta `run-1` + later null-taskId projection must not
mutate status/stats.

### Minor — snapshot summary bounds

- Server snapshot path uses public `parse_and_validate_summary_json` (settlement
  bounds) instead of shape-only serde.
- Client `normalizeCardSummary` rejects invalid `report_file` (length / absolute /
  `..`) matching server `validate_report_file`.
- Rust + vitest coverage for absolute `report_file` omission.

### Verification (review fix)

| Command | Result |
| --- | --- |
| `pnpm test --` adapters / use-delegation-card-model / delegation-run-snapshot / tool-call-normalization / message / sub-agent-overlay | **PASS** — 553 tests |
| `npx eslint` (touched frontend sources) | **PASS** |
| `cargo test --test delegation_run_snapshot --features test-utils` | **PASS** — 1 test |
| `cargo check --lib --features test-utils` | **PASS** |

### Files in review-fix commit

- `src/lib/adapters/tool-kind-classifier.ts` (+ test)
- `src/lib/adapters/ai-elements-adapter.test.ts`
- `src/hooks/use-delegation-card-model.ts` (+ test)
- `src/lib/delegation-run-snapshot.ts` (+ test)
- `src-tauri/src/acp/delegation/card_summary.rs`
- `src-tauri/src/commands/delegation.rs`
- `src-tauri/tests/delegation_run_snapshot.rs`

**Not staged:** `.superpowers/sdd/task-5-report.md`, `target-task6-debug/**`,
unrelated turn-anchor scroll WIP in `message-list-view*` / `sub-agent-session-dialog*`
