# Task 7 Report - Frontend + Snapshot DTO

## Status

DONE_WITH_CONCERNS

## Commits

- `861e65b17527548d6211de17ef4912cbfa876c79` - `feat(ui): per-run cards, snapshot DTO, continue tool chrome`
- `8bc32307a3505fbe053e68022ec1881c1da1b601` - `fix(ui): continue agent-like chrome and fail-closed projection gate`
- `2aea1525c0111757052c73401734cbe2f4b90742` - `fix(ui): prioritize selected delegation turn focus`

## Summary

Implemented parent-authorized `get_delegation_run_snapshot(task_id)` for Tauri
and Axum, backed exclusively by durable `delegation_task_runs` rows. The
frontend caches immutable run snapshots by parent/task id, validates summaries
defensively, and prevents a later run's child projection from reopening or
rewriting an older card.

Initial and continued delegation tools now render independent per-run cards.
The overlay groups reusable runs by child conversation, exposes run count and
the latest state, and keeps replacements visible as separate marked rows.
Validated review/implementation summaries render in all ten locales, with
responsive and RTL layout smoke coverage. A selected run's turn anchor opens
the shared child session at the matching persisted turn and suppresses the
competing initial scroll-to-bottom path.

The review follow-up also keeps historical `continue_delegation` calls out of
generic tool groups, makes projection fallback fail closed when task identity is
missing or mismatched, and applies settlement-level summary bounds to snapshot
loading.

## Verification

| Command | Result |
| --- | --- |
| `pnpm exec vitest run` across the 11 Task 7 adapter/card/overlay/dialog/snapshot suites | PASS - 11 files, 332 tests |
| `pnpm exec eslint` across all touched TypeScript and TSX files | PASS |
| `pnpm build` | PASS - TypeScript and static export completed |
| `cargo test --features test-utils --test delegation_run_snapshot` | PASS - 1 test |
| `cargo check` | PASS - desktop/Tauri mode |
| `cargo check --no-default-features --bin codeg-server` | PASS - web/server mode |
| `cargo clippy --lib --features test-utils -- -D warnings` | PASS |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | PASS |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | FAIL - six pre-existing Task 6 test/helper lints in `broker.rs` and `run_store.rs`; no Task 7 file is reported |

The focused Vitest output includes the existing intentional malformed-input
diagnostic from overlay fallback tests. It has no failed assertions.

## Files

Backend:

- `src-tauri/src/commands/delegation.rs`
- `src-tauri/src/web/handlers/delegation.rs`
- `src-tauri/src/web/router.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/acp/delegation/card_summary.rs`
- `src-tauri/tests/delegation_run_snapshot.rs`

Frontend:

- `src/lib/delegation-run-snapshot.ts` and test
- `src/lib/types.ts`, `src/lib/api.ts`
- `src/lib/delegation-binding-reduce.ts` and test
- `src/lib/delegation-card.ts`, `src/lib/tool-call-normalization.ts` and tests
- `src/lib/adapters/tool-kind-classifier.ts` and adapter tests
- `src/hooks/use-delegation-card-model.ts` and test
- message card, summary, renderer, overlay, message-list, and dialog sources/tests
- all ten `src/i18n/messages/*.json` files

## Brief Self-Review

| Requirement | Result |
| --- | --- |
| Parent-authorized immutable snapshot DTO | Pass. Query filters `(parent_conversation_id, task_id)` and foreign parents receive `not_found`. |
| Desktop and web surfaces | Pass. Tauri command and Axum route share one core query. |
| Optional `DelegationCompleted.card_summary` | Pass. Rust/TS wire fields exist; the reducer validates and stores the summary only for its exact card. |
| Initial and continued cards | Pass. Normalization, classifier, adapter, and renderer recognize `delegate_to_agent` and `continue_delegation`. |
| Same-child immutability | Pass. Terminal snapshots freeze, and null/mismatched child projection task ids cannot supply run-scoped status or stats. |
| Invalid summary fallback | Pass. Server and client use bounded validation; malformed summaries yield a status-only card. |
| Overlay grouping and replacements | Pass. Cold snapshot child ids coalesce groups; replacement rows remain separately marked. |
| Responsive/mobile and RTL smoke | Pass. Constrained responsive summary layout plus Arabic `dir="auto"` smoke test. |
| Session focus | Pass. Inline/overlay anchors flow through the dialog; anchor priority prevents the initial bottom scroll from overriding focus. |
| TDD | Current-session regressions for stale snapshots, cold grouping, anchor handoff, and anchor-scroll ordering were observed failing before fixes. The initial inherited WIP red phase was not reconstructible. |

## Concerns

1. Full all-target Rust clippy remains red only on six existing Task 6
   `broker.rs`/`run_store.rs` test/helper lints. The desktop and server clippy
   modes covering Task 7 pass.
2. Responsive and RTL checks are component layout smoke tests rather than
   browser screenshot tests. Live multi-run browser e2e belongs to Task 9.
3. `child_turn_anchor` is currently commonly null at persistence time. The
   optional focus behavior is implemented and covered when a producer supplies
   an anchor.

## Out Of Scope

- Task 8 skill markdown and routing work.
- Task 9 live-agent end-to-end fixtures and browser visual e2e.
- Reopening Task 6 broker races or repairing its existing all-target clippy findings.
