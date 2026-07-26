# Task 5 Report: Frontend cancel reconciliation coordinator

## Status

**DONE**

## Commits

| Hash | Message |
| --- | --- |
| *(see `git log` tip after land)* | `feat(runtime): abort-fenced cancel reconciliation coordinator` |

**Base:** `761eecfc` (Task 4 tip)  
**Branch:** `feature/user-stop-transcript-reconciliation`  
**Local only** (no push).

## Files changed

| File | Change |
| --- | --- |
| `src/stores/conversation-runtime-store.ts` | `CancelCompletionKey`, `cancelGeneration`, raw detail fetch, actions `RECORD_TURN_OUTCOME` / `START_CANCEL_RECONCILE` / `RECONCILE_CANCELLED_TURN` / `CLEAR_CANCEL_RECONCILE`, coordinator (100/300/1000), exclusive destructive path, `reloadDetail`, lifecycle clears |
| `src/stores/cancel-reconcile.test.ts` | **New** — design FE cases 1–10, 12–15, 16–18 + start gates + outcome attach |
| `src/stores/conversation-runtime-store.test.ts` | Session seed `pendingCancel` / missing fields |
| `src/stores/viewer-detail-sync.test.ts` | Session seed `pendingCancel` + `delegationActivities` |
| `src/stores/runtime-live-message-slice-decoupling.test.ts` | Session seed fields for new session shape |
| `src/stores/runtime-timeline-prefix-cache.test.ts` | Session seed fields |
| `src/stores/turn-metadata-patches.test.ts` | Session seed fields |
| `src/hooks/use-conversation-detail.test.tsx` | Session seed fields |
| `src/components/message/message-list-view.test.tsx` | Session seed fields |

## Implementation summary

1. **`CancelCompletionKey`** on `ConversationRuntimeSession.pendingCancel` with dedicated `cancelGenerationById` map (not `fetchGeneration`).
2. **Actions (fixed names):**
   - `recordTurnOutcome` → `RECORD_TURN_OUTCOME` (idempotent by `connectionId`+`completionSeq`; trailing assistant attach or outcome-only append).
   - `startCancelReconcile` → `START_CANCEL_RECONCILE` + sequential raw reads at **100 / 300 / 1000** ms.
   - Success applies **only** `RECONCILE_CANCELLED_TURN` (never `FETCH_DETAIL_SUCCESS`).
   - `clearCancelReconcile` / lifecycle paths clear key + timers + bump generation.
   - `reloadDetail(id, { reason: "manual_reload" })` clears fence then authoritative load; resolves negative runtime ids via `dbConversationId`.
3. **`RECONCILE_CANCELLED_TURN` merge:** replace `detail`, clear `localTurns`/`optimisticTurns`/`liveMessage`, retain background settlements/bindings/ACP errors/cleanup, stamp `source: "user_stop"` on matched fence, retire Claude overlay by watermark.
4. **Exclusive path while pending:** `refetchDetail` / `fetchDetail` / `syncViewerDetail` / `syncDelegateTerminalDetail` no-op destructive commits.
5. **Lifecycle clears:** success, retry exhaustion, Manual Reload, new prompt (`appendOptimisticTurn`), remove, external rebind, DB identity replace, migrate, store reset.
6. **Start gates:** non-empty `providerTurnId`; positive persisted id (`dbConversationId ?? conversationId > 0`).

## Design FE cases (Task 5 ownership)

| Case | Covered |
| --- | --- |
| 1–10 | yes (`cancel-reconcile.test.ts`) |
| 12–15 | yes |
| 16 (`syncTurnMetadata`) | yes |
| 17 (competing generations) | yes |
| 18 (cleanup resumes sync) | yes |
| **11** (envelope ordering) | **not Task 5** → Task 6 |
| **19** (adapter cache) | **not Task 5** → Task 7 |

## TDD

### RED
- Added `cancel-reconcile.test.ts` against missing APIs / behavior.

### GREEN
- Implemented reducers + coordinator + exclusive path + `reloadDetail`.
- Fixed FE14 assertion: next prompt lands in `optimisticTurns`.

## Tests run

```powershell
pnpm test src/stores/cancel-reconcile.test.ts src/stores/conversation-runtime-store.test.ts src/stores/viewer-detail-sync.test.ts
```

| Suite | Result |
| --- | --- |
| `cancel-reconcile.test.ts` | **27 passed** |
| `conversation-runtime-store.test.ts` | **22 passed** |
| `viewer-detail-sync.test.ts` | **29 passed** |
| **Total** | **78 passed** |

Also verified: `background-overlay` + `turn-metadata-patches` (110 total with those suites).

## Self-review

- Scope held: store coordinator only; **no** envelope wiring (Task 6), **no** presentation/i18n (Task 7).
- Uses Task 1 `TurnOutcome` (`status: "interrupted"`, `stop_reason: "cancelled"`, optional `source` / `provider_turn_id`).
- Coordinator never applies via `FETCH_DETAIL_SUCCESS`.
- No auto-resume of cancelled prompt.

## Concerns

1. **Outcome idempotency map is module-level** (`recordedTurnOutcomeKeys`) — cleared on remove/migrate/reset; not serialized. Fine for runtime lifetime.
2. **`syncTurnMetadata` still issues raw-ish detail reads** for patches; tests prove it neither clears pendingCancel nor replaces local content. Task 6 should not start a second coordinator from metadata.
3. **Start does not re-validate `stop_reason`/`termination_source`** — Task 6 envelope acceptance is the gate; store trusts callers that already accepted user_stop.
4. **Pre-existing `tsc` noise** in unrelated composer tests remains; store-related missing-seed fields for `pendingCancel` were fixed.
5. **No push** — local commit only.

## Out of scope (confirmed not done)

- Task 6: envelope `START_CANCEL_RECONCILE` / status-edge promotion-only / double-start audit
- Task 7: footer, adapter cache fingerprint, i18n `responseInterrupted`
- E2E Task 8
