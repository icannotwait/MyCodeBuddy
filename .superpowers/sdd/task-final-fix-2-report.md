# Final-review fix 2 — report

## Status

**DONE**

## Summary

Fixed the Important #1 ownership-fence gaps from final-review r2:

1. **`appendViewerUserTurn` always invalidates** on a real co-controller/viewer prompt — `stopCancelReconcileTimers` + `bumpCancelGeneration` — even when `pendingCancel` is still null (Stop before typed `user_stop` envelope). Exact-id sender-echo still keeps the fence.
2. **`migrateConversation` carries `userStopOwnershipById`** from → to (and leaves the from-id entry as a tombstone). Combined with existing gen bumps, late envelopes for either id are stale; absence is no longer treated as “current” on the new id.
3. **Regressions** cover pre-envelope viewer prompt and migrate-before-envelope.

## Changes

### `src/stores/conversation-runtime-store.ts`

- Extracted `isExactIdViewerUserEcho` (shared by reducer + action).
- `appendViewerUserTurn`: invalidate on `!exactIdEcho` instead of only when `pendingCancel` transitions non-null → null.
- `migrateConversation`: copy ownership entry `from → to` before gen bumps; keep from-id record so missing session + owned record ⇒ stale.
- Added `__getCancelGenerationForTests` for regressions.
- Comments updated to match the always-bump-on-real-prompt rule.

### `src/stores/cancel-reconcile.test.ts`

- Pre-envelope co-controller prompt: `noteUserStopTurnOwnership` with null `pendingCancel` → `appendViewerUserTurn` advances gen → `isStaleUserStopEnvelope` true.
- Migration: ownership present on from → migrate → ownership on to (+ tombstone on from) → both ids stale.
- Exact-id sender-echo: also asserts `cancelGeneration` is unchanged.

## Tests

```text
pnpm exec vitest run \
  src/stores/cancel-reconcile.test.ts \
  src/stores/conversation-runtime-store.test.ts \
  src/stores/turn-metadata-patches.test.ts \
  src/contexts/user-stop-dual-path.test.ts
```

Result: **4 files, 83 tests, all passed**.

## Commit

- **Hash:** `959f3c80dea727cc871fb9c0723cb00adb379d9b` (`959f3c80`)
- **Message:** `fix(runtime): invalidate user_stop ownership on viewer prompt and migrate`

## Out of scope (preserved)

- codex-acp Minor packaging items (absolute path, seed identity, smoke prettier)
- Uncommitted edits to `.superpowers/sdd/task-4-report.md` and `task-5-report.md`
