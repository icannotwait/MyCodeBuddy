# Task 6 Report: Props-driven ConversationSessionSurface + detached page

**Status:** DONE_WITH_CONCERNS  
**Branch:** `feat/conversation-popout-window`  
**Commit:** `1de859be` — `feat(ui): detached conversation page with session surface`

## Implemented

### 1. ConversationSessionSurface (extract)
- Created `src/components/conversations/conversation-session-surface.tsx` by extracting the former `ConversationTabView` body.
- Props-driven identity: `folderId` is an explicit prop (not only tab-store lookup). Falls back to tab row when prop is 0.
- `conversation-detail-panel.tsx` keeps multi-tab shell + thin `ConversationTabView` wrapper that supplies `folderId` from the tab row.

### 2. Detached page bootstrap (claim-before-activate)
- Rewrote `src/app/conversation/page.tsx` with required order:
  1. Parse/validate query (`conversationId`, `folderId`, `agentType`, `operationId`)
  2. Load conversation + folder metadata
  3. Seed memory-only tab + folder (no `TabProvider` hydrate/save)
  4. Live: discover → rebind → `claimConnectionOwnership` → ready  
     Cold: ready without generation; **isActive=false until commit-ack**
  5. Emit `conversation-window://ready`
  6. Render surface; connect gated via `resolveDetachedConnectGate`
- Commit-ack listener + poll fallback on `getConversationPopoutOperation`
- `suppressFrontendDisconnect` for transfer lifetime until ack

### 3. Providers (`detached-shell.tsx`)
- `AlertProvider` → `GitCredentialProvider` → `TaskProvider` → `AcpConnectionsProvider` → `SessionStatsProvider` → `ConversationRuntimeProvider` → `DelegationProvider` → `WorkspaceProvider`
- **No** workspace `TabProvider` (no opened-tabs hydrate/CAS save)
- Synthetic tab seed via `useTabStore.setState` + `tabsHydrated: true`
- `DetachedOpenTabKeysRegistrar` keeps idle sweep from reaping the detached context key

### 4. ACP bridge / provider
- `setSuppressFrontendDisconnect` / `isFrontendDisconnectSuppressed`
- `claimConnectionOwnership` on bridge + implementation in `AcpConnectionsProvider` (owner attach, no second spawn; cold no-op)
- Idle sweep / unmount / disconnect skip `acpDisconnect` when suppressed (viewer-style detach)

### 5. Main commit-ack
- After `completeConversationPopoutOperation` returns HandoffComplete, main emits `conversation-window://commit-ack` `{ operationId }`

### 6. Pure bootstrap helpers
- `src/lib/conversation-popout-detached-bootstrap.ts` — parse query, connect gate, ready payload, phase helpers

## Tests + TDD evidence

| Suite | Result |
| --- | --- |
| `conversation-popout-detached-bootstrap.test.ts` | 11 pass (claim-before-activate / cold gate) |
| `detached-bootstrap-flow.test.ts` | 4 pass |
| `conversation-popout-acp-bridge.test.ts` | 5 pass (suppress + claim null-safe) |
| `conversation-popout.test.ts` | 7 pass |
| `conversation-session-surface.test.ts` | 2 pass (folderId prop preference) |
| `conversation-detail-panel-layout.test.ts` | 23 pass (updated for surface extract) |

**Total targeted: 52 passed**

```bash
pnpm exec vitest run \
  src/lib/conversation-popout-detached-bootstrap.test.ts \
  src/lib/conversation-popout-acp-bridge.test.ts \
  src/lib/conversation-popout.test.ts \
  src/components/conversations/conversation-session-surface.test.ts \
  src/app/conversation/_components/detached-bootstrap-flow.test.ts \
  src/components/conversations/conversation-detail-panel-layout.test.ts
```

TDD: pure gate helpers written with red/green coverage for live vs cold connect gates; bridge suppress/claim tests extended before wiring; layout source-scan tests updated after extract.

## Files changed

**Created**
- `src/components/conversations/conversation-session-surface.tsx`
- `src/components/conversations/conversation-session-surface.test.ts`
- `src/app/conversation/_components/detached-shell.tsx`
- `src/app/conversation/_components/detached-bootstrap-flow.test.ts`
- `src/lib/conversation-popout-detached-bootstrap.ts`
- `src/lib/conversation-popout-detached-bootstrap.test.ts`

**Modified**
- `src/app/conversation/page.tsx`
- `src/components/conversations/conversation-detail-panel.tsx`
- `src/components/conversations/conversation-detail-panel-layout.test.ts`
- `src/contexts/acp-connections-context.tsx`
- `src/lib/conversation-popout.ts`
- `src/lib/conversation-popout-acp-bridge.ts`
- `src/lib/conversation-popout-acp-bridge.test.ts`

## Self-review

### Spec compliance
- ✅ Props-driven surface with explicit `folderId`
- ✅ Bootstrap order: validate → metadata → claim/rebind live vs cold gate → ready → surface
- ✅ No TabProvider hydrate/save; synthetic tab persist-off (memory only)
- ✅ suppressFrontendDisconnect until commit-ack
- ✅ Main emits commit-ack after HandoffComplete
- ✅ Providers listed in brief mounted (WorkspaceProvider for artifact hooks)

### Quality
- Extraction preserves session UI path; main panel is a thin wrapper
- Connect gate logic is pure and unit-tested
- Prettier/eslint clean on touched files (one pre-existing hooks warning in acp context)

## Concerns

1. **Discovery DTO lacks `owner_window_label`** — claim cannot yet refuse `"web"` / unexpected owners at discovery time (Rust `ConversationConnectionInfo` still only has `connection_id` + `event_seq`). Live path relies on rebind CAS from `"main"`.

2. **Full WorkspaceProvider on detached** — heavier than a stub; ensures `useWorkspaceActions` does not throw. File open from artifacts will operate without a files pane UI (acceptable for v1; may want stubs later).

3. **Manual E2E not run** — live handoff + cold connect after ack need a desktop smoke test (Task 8 territory).

4. **Incarnation tombstone** for late registers during close-during-connect is still primarily Rust-side; frontend suppress clears on unmount.

5. **Surface still reads tab store** for `isChat` / `isPinned` / `delegationRouteOverride` — fine with synthetic seed; pure props-only would be a follow-up refactor.

---

## Review fix pass (Critical + Important)

**Status:** DONE  
**Commit:** `1a40d309` — `fix(ui): safe detached claim, suppress lifetime, local gate`

### Fixes

| Finding | Fix |
| --- | --- |
| **C1** Live discovery/rebind/claim → false cold ready | `classifyDiscoveryResult` / `decideLiveHandoffResult`: discovery transport error and rebind/claim failure set `error`, **do not emit ready**, do not set bootstrapReady. Claim failure after rebind reverse-CASes ownership back to `main`. True cold remains discovery `none`. |
| **C2** Suppress cleared before descendant unmount | Removed effective clear-on-unmount (`shouldClearSuppressOnDetachedUnmount() === false`). Suppress clears only on commit-ack while tree is mounted. Claim stores `ownershipGeneration` / `ownerOperationId` / `ownerWindowLabel` lease on connection entry. |
| **I1** Cold overlays before active | `shouldMountDetachedSurface` requires `isActive` (commit-ack for cold, claimed for live). Loading placeholder until then — MessageListView/overlays not mounted. |
| **I2** Local desktop boundary | Early `isLocalDesktop()` reject before metadata/ACP; i18n `localDesktopOnly`. |

### Tests re-run

| Suite | Result |
| --- | --- |
| `conversation-popout-detached-bootstrap.test.ts` | 19 pass |
| `detached-bootstrap-flow.test.ts` | 8 pass |
| `conversation-popout-acp-bridge.test.ts` | 6 pass |
| `conversation-popout.test.ts` | 7 pass |
| `conversation-session-surface.test.ts` | 2 pass |
| `conversation-detail-panel-layout.test.ts` | 23 pass |

**Total targeted: 65 passed**

ESLint on touched pure/page/test files: clean (pre-existing hooks warning only in acp-connections-context).


### Remaining concerns (pre-fix2; superseded where noted)

1. ~~acp_disconnect connection-id only~~ — **fixed in fix pass 2** via disconnect_if_owner + lease wire args.
2. Full page mount integration (Next/Tauri) still not exercised in vitest — failure-to-ready covered via pure helpers used by page.tsx.

---

## Review fix pass 2 (r2 Critical + Important)

**Status:** DONE
**Commit:** baafdca8 — fix(acp): incarnation-stamped connect and lease disconnect

### Fixes

| Finding | Fix |
| --- | --- |
| **C1** Cold connect never joins pop-out incarnation | acp_connect accepts optional owner_operation_id; desktop path begin_registration/end_registration (RAII), stamps AgentConnection.owner_operation_id at spawn insert, tears down if tombstoned/aborted mid-connect. FE: page passes operationId to surface to lifecycle to acpConnect. Window-close already uses disconnect_by_owner_window_and_operation. |
| **C2** Leased disconnect bypassed | Backend disconnect_if_owner CAS (window/op/generation); bare disconnect when lease empty. FE all teardown paths pass leaseArgsForDisconnect when lease present (idle, unmount, replace, abandoned, disconnect, disconnectAll). |
| **I1** Claim silent-success without bridge | claimConnectionOwnership throws if bridge missing; page validates claim returns matching connectionId. |
| **I2** Focused lifecycle tests | Rust: CAS stale after rebind no-op; operation-scoped reap of stamped cold conn; registration tombstone. FE: claim fails without bridge; acpDisconnect/acpConnect lease/op wire payloads. |

### Tests re-run

**Vitest (50 passed):**
- conversation-popout-acp-bridge.test.ts (7)
- delegation-route-api.test.ts (7)
- conversation-popout-detached-bootstrap.test.ts (19)
- detached-bootstrap-flow.test.ts (8)
- conversation-popout.test.ts (7)
- conversation-session-surface.test.ts (2)

**Cargo (--features test-utils):**
- disconnect_if_owner_stamps_and_cas_skips_stale_after_rebind ok
- disconnect_by_owner_window_and_operation_reaps_stamped_cold_conn ok
- begin_registration_rejects_tombstoned_and_tracks_inflight ok

**cargo check (desktop):** ok

### Notes
- Optional params only — main/non-leased callers unchanged.
- Cold ownership generation stamped 0 on FE until rebind bumps generation.