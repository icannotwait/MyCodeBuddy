# Task 4 Report — Migration no-bump, unbound id, dual-path envelope/surface

**Status:** `DONE`  
**Date:** 2026-07-27  
**Branch:** `feature/b2d-user-stop-transcript-reconciliation`  
**Implementer:** Grok (Task 4 only — Tasks 5–7 not implemented)

## Commits

| SHA | Message |
| --- | --- |
| `35a44b9a02ff03be27e3ba3d38c3ec9c9e6f99d8` | `feat(runtime): migration no-bump, unbound id, dual-path ownerPreserve` |

## Scope

**Files changed (task-owned):**

| File | Change |
| --- | --- |
| `src/stores/conversation-runtime-store.ts` | `MIGRATE_CONVERSATION` migrates pending/soft/owner; action no-bump gen; migrate ownership + `recordedTurnOutcomeKeys`; re-arm soft fence + restart coordinator under post-migration id |
| `src/stores/cancel-reconcile.test.ts` | Invert HEAD migrate-stale tests; Task 4 suite (migrate, identity replace, unbound store gate, late envelope, suppress) |
| `src/contexts/acp-connections-context.tsx` | `acceptUserStopTurnComplete`: missing provider / unbound detail id → `enterOwnerPreserve`; positive id + provider → coordinator |
| `src/contexts/user-stop-dual-path.test.ts` | Unbound, ownerPreserve on missing provider, late envelope after age-out, migrate-then-envelope, panel audit retained |

**Spot-check:** `conversation-detail-panel.tsx` — still promote-only (no `startCancelReconcile` / `recordTurnOutcome` / `acceptUserStopTurnComplete`). Covered by dual-path wiring audit.

**Consumes Tasks 2–3:** soft fence / `ownerPreserve` / Branch A/B already in store.

**Not touched:** AC3 vendor (5*), presentation (6), full verification (7).

## Design contract implemented

### Runtime-key migration (no-bump)

| Item | Behavior |
| --- | --- |
| `cancelGeneration` | **Moved** to destination (no `+1`) |
| `pendingCancel` | Migrated; `conversationId` rewritten to `to` |
| `softFence` / `ownerPreserve` | Migrated (OR of from/to) |
| Ownership snapshot | Copied to `to`; `from` tombstone retained |
| `recordedTurnOutcomeKeys` | Migrated to **both** ids (no second footer) |
| Soft-fence timer | Re-armed on `to` when soft fence still active |
| Coordinator timers | Stopped then **restarted** under post-migration id (same completion / gen) |
| Late envelope after migrate | **Not** stale (`isStaleUserStopEnvelope(to) === false`) |

### Identity replacement (still bump + clear)

| Path | Behavior |
| --- | --- |
| `setExternalId` rebind (external id change) | Bump gen; clear pending/soft/owner; stop timers |
| `setDbConversationId` replace (existing ≠ new) | Same |

### Unbound detail id + missing provider (envelope)

| Case | Outcome | Coordinator | Suppress |
| --- | --- | --- | --- |
| `user_stop` + empty/null `provider_turn_id` | Recorded | No | `ownerPreserve` |
| `user_stop` + provider + persisted id ≤ 0 | Recorded | No | `ownerPreserve` |
| `user_stop` + provider + positive persisted id | Recorded | Yes (`startCancelReconcile`) | `pendingCancel` |

### Late envelope after soft-fence age-out

- Age-out → `ownerPreserve` still suppresses.
- If Stop ownership gen is **still current**, accepted envelope **may** start coordinator.
- If gen advanced (next prompt), envelope is stale → no-op.

### Dual-path / surface / panel

- Envelope path remains sole starter for outcome + coordinator.
- Status-edge / surface / panel remain promotion-only (source audits).
- Destructive under suppress still no-ops (store + dual-path coverage).

## HEAD invert

Previously `MIGRATE_CONVERSATION` nulled `pendingCancel`, cleared soft/owner, and bumped gens so late envelopes were stale.

**Inverted tests:**

- `migrates userStop ownership without bumping cancelGeneration`
- `migrate after appendOptimisticTurn keeps cancelGeneration and ownership current`

## Tests run

```powershell
pnpm exec vitest run src/stores/cancel-reconcile.test.ts src/contexts/user-stop-dual-path.test.ts
# 80 passed (66 + 14)

pnpm exec vitest run src/stores/ src/contexts/user-stop-dual-path.test.ts
# 280 passed (19 files)

pnpm exec eslint src/stores/conversation-runtime-store.ts src/stores/cancel-reconcile.test.ts src/contexts/acp-connections-context.tsx src/contexts/user-stop-dual-path.test.ts
# 0 errors (pre-existing react-hooks warnings only in acp-connections-context)
```

## Self-review

- Migration does **not** bump `cancelGeneration`; identity rebind still does.
- In-flight coordinator is restarted after migrate (clear + `startCancelReconcile`) so deferred reconcile uses post-migration id without gen bump.
- Soft-fence age-out helper extracted (`scheduleSoftFenceAgeOut`) and shared with Stop ownership + migrate re-arm.
- Envelope path calls `enterOwnerPreserve` for both missing-provider and unbound-id paths.
- Panel/surface dual-path sole-starter audits unchanged and green.

## Concerns

1. **Coordinator restart after migrate** resets the retry delay schedule to attempt 0 (does not resume mid-delay index). Design allows in-flight acceptance under post-migration identity; residual delay budget is not preserved.
2. **Soft-fence re-arm** restarts a full 30s age-out rather than remaining time.
3. **`cancelGeneration` on FROM** is left at the pre-migrate value (not deleted); session-map absence already makes `isStaleUserStopEnvelope(FROM)` true.
4. **Prettier/CRLF** on Windows: working tree may rewrite LF→CRLF on touch; no functional impact.
5. **AC3 / Tasks 5–7** still pending per plan DAG.

## Out of scope (confirmed not done)

- Task 5a/5b/5c AC3 vendor pin / managed install
- Task 6 presentation RETAIN audit
- Task 7 full AGENTS verification sweep
- Push / PR
