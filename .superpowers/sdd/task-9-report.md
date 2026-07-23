# Task 9 Report — Persistent Warning Banner, Countdown, and User Actions

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
**Commit:** `844105c1df0e94f1d91b761c249a96b9c018b651` — `feat(ui): warn and control stalled foreground tools`  
**Base:** `83ee6b883ef3cadd4eb27537fb4147c898bc7be7`  
**Date:** 2026-07-23  
**Worktree:** `D:\MyCodeBuddy\.worktrees\tool-execution-watchdog`

## Summary

Task 9 delivers the in-session tool-watchdog warning surface, multi-window
version reduction, extend/cancel APIs, and desktop-only hidden-app system
notifications with optional `notification-navigate` targets.

## What landed

### Banner UI (`ToolWatchdogBanner`)

- Persistent session surface (session `topBanner`, not toast / not tool card)
- Shows allowlisted tool title, last progress, grace countdown, **Stop now**,
  **Wait 10 minutes**
- Actions send `lease_id` + current `version` only
- Controls disable after first click for that version until the next
  authoritative projection (or stale-error refresh)

### Event reduction (all windows)

- `ConnectionState.toolWatchdogProjections` hydrated from snapshot + live
  `tool_watchdog_changed`
- Pure reducer `reduceToolWatchdogProjection` — higher version wins; older
  ignored; `cleared` / `timed_out` remove keys
- No window invents a local terminal tool outcome

### APIs

```ts
extendToolWatchdogLease(leaseId, version)
cancelToolWatchdogLease(leaseId, version)
```

Transport calls: `acp_tool_watchdog_extend` / `acp_tool_watchdog_cancel`.

### Notifications

- `sendSystemNotification(title, body, target?)` with
  `NotificationTarget = { kind: "conversation", conversationId }`
- Watchdog: desktop-only, once per `(lease_id, version)`, hidden document only,
  no tool input; includes conversation target when known
- Server/Web: banner + shared events only — never browser `Notification` for
  watchdog (target path short-circuits; non-desktop notify gate)
- Host `send_notification` accepts optional `action_id` + `conversation_id`,
  registers short-lived map (15m TTL), emits `notification-navigate` via
  `fire_notification_navigate`
- Frontend listens for `notification-navigate` and `openTab`s the conversation

## Files

| Path | Change |
| --- | --- |
| `src/components/conversations/tool-watchdog-banner.tsx` | **Create** banner UI |
| `src/components/conversations/tool-watchdog-banner.test.tsx` | **Create** tests |
| `src/lib/tool-watchdog-projection.ts` | **Create** pure reduce/countdown helpers |
| `src/components/conversations/conversation-session-surface.tsx` | Wire banner into `topBanner` |
| `src/contexts/acp-connections-context.tsx` | State, reduce, notify, navigate listen |
| `src/contexts/acp-connections-context.test.tsx` | Multi-window / notify / navigate tests |
| `src/hooks/use-connection.ts` | Expose `toolWatchdogProjections` |
| `src/lib/api.ts` | Extend/cancel lease APIs |
| `src/lib/notification.ts` | Optional navigation target |
| `src/lib/types.ts` | Prettier-only on title union |
| `src-tauri/src/commands/notification.rs` | action_id map + navigate emit |
| `src-tauri/src/lib.rs` | Optional args on existing notify call site |

## Verification

```powershell
pnpm test -- src/components/conversations/tool-watchdog-banner.test.tsx src/contexts/acp-connections-context.test.tsx
# 118 passed (13 banner + 105 context)

pnpm eslint src/components/conversations/tool-watchdog-banner.tsx src/contexts/acp-connections-context.tsx src/lib/api.ts src/lib/types.ts src/lib/notification.ts
# 0 errors; 1 pre-existing exhaustive-deps warning in acp-connections-context

cd src-tauri
cargo test --lib commands::notification --features test-utils
# 3 passed
```

## Coverage mapping (brief Step 1)

| Case | Where |
| --- | --- |
| Warning content | banner test |
| Countdown boundary | `remainingGraceSeconds` / formatCountdown |
| Unlimited extensions | pure reduce loop versions 1..5 |
| Progress clear | reduce cleared + banner empty map |
| Stale-action error refresh | banner stale clear pending |
| Double-click dedup | banner stop called once |
| Two-window winner/loser | reduce + reducer tests |
| Hidden-only notification | context notify tests |
| Notification click navigate | `notification-navigate` openTab |
| Timed-out failed tool entry | map removes timed_out; no local terminal status |
| Restored composer input | timed_out does not force connection status; turn_complete path unchanged |

## Concerns

1. **OS click → navigate is best-effort on desktop.** Registration + TTL +
   `fire_notification_navigate` are implemented; macOS / Windows plugin paths
   do not always expose a reliable OS click callback. Banner remains
   authoritative. Future work can wire platform-specific activation args.
2. **Banner copy is English inline** for Task 9; Task 10 owns full locale keys
   under General settings / i18n.
3. **Pre-existing dirty files left unstaged:**
   `.superpowers/sdd/task-6-report.md`,
   `docs/superpowers/specs/2026-07-22-tool-execution-watchdog-design.md`.
4. **Pre-existing eslint warning** in `acp-connections-context.tsx`
   (`connect` / `setActiveKey` exhaustive-deps) — unrelated to this task.

## Out of scope (Task 10)

- General settings UI for watchdog
- Session details diagnostics
- All-ten-locale message parity
- End-to-end Rust lifecycle fixtures

---

## Review fixes (P1 / P2)

**Date:** 2026-07-23  
**Review:** `.superpowers/sdd/task-9-review.md`  
**Status:** FIXED (P1-1, P1-2, P2 host dedupe)

### P1-1 — Desktop Stop / Wait Tauri arg naming

- **Root cause:** extend/cancel sent `{ lease_id, version }` while Tauri default
  command arg renaming expects camelCase (`leaseId`), matching other ACP calls.
  Web Axum accepted snake_case only — desktop+web disagreed.
- **Fix:** TS `api.ts` sends `{ leaseId, version }`. Web body
  `ToolWatchdogLeaseAction` uses `#[serde(rename_all = "camelCase")]`.
- **Contract tests:** `src/lib/api.test.ts` (extend + cancel key shape);
  Rust lease-action wire tests updated.

### P1-2 — Notification click → navigate producer

- **Root cause (a):** TS sent `action_id` / `conversation_id`; Tauri expects
  `actionId` / `conversationId` — registration never ran.
- **Root cause (b):** neither macOS nor non-macOS show paths called
  `fire_notification_navigate`.
- **Fix:**
  - TS `notification.ts` uses camelCase wire keys.
  - Register navigation targets **only** when
    `NOTIFICATION_CLICK_NAVIGATION_SUPPORTED` (macOS + Linux/XDG).
  - **macOS:** `wait_for_click` on a worker thread → `fire_notification_navigate`
    on Click / ActionButton.
  - **Linux:** `notify-rust` `.action("default")` + `wait_for_action` → fire.
  - **Windows:** omit target registration cleanly (no click callback in
    notify-rust / plugin); banner remains authoritative.
- **Tests:** `src/lib/notification.test.ts` payload keys; Rust
  `maybe_register_click_target` / fire / omit-on-unsupported.

### P2 — Multi-window notification dedupe (simple host gate)

- Renderer still has a fast local Set; host `send_notification` accepts optional
  `dedupeKey` and claims once per key process-wide.
- Watchdog path passes `leaseId:version` as `dedupeKey`.
- Rust unit test: `host_dedupe_claims_once_per_key`.

### Verification (review-fix)

```powershell
pnpm test -- src/components/conversations/tool-watchdog-banner.test.tsx `
  src/contexts/acp-connections-context.test.tsx `
  src/lib/api.test.ts src/lib/notification.test.ts
# 128 passed

pnpm eslint src/components/conversations/tool-watchdog-banner.tsx `
  src/contexts/acp-connections-context.tsx src/lib/api.ts src/lib/types.ts `
  src/lib/notification.ts src/lib/api.test.ts src/lib/notification.test.ts
# 0 errors; 1 pre-existing exhaustive-deps warning

cd src-tauri
cargo test --lib commands::notification --features test-utils
# 7 passed
cargo test --lib commands::tool_watchdog --features test-utils
# 7 passed
cargo test --lib web::handlers::tool_watchdog --no-default-features
# 3 passed
```

### Files touched in review fix

| Path | Change |
| --- | --- |
| `src/lib/api.ts` | camelCase leaseId wire |
| `src/lib/api.test.ts` | desktop+web contract tests |
| `src/lib/notification.ts` | camelCase actionId/conversationId/dedupeKey |
| `src/lib/notification.test.ts` | **Create** payload tests |
| `src/contexts/acp-connections-context.tsx` | pass host dedupeKey |
| `src/contexts/acp-connections-context.test.tsx` | assert dedupeKey |

---

## Review fix r2 (I1)

**Date:** 2026-07-23  
**Review:** `.superpowers/sdd/task-9-review-r2.md`  
**Status:** FIXED (I1)

### I1 — Fresh conversations lose the notification navigation target

- **Root cause:** New tabs auto-connect with `ConnectionState.conversationId:
  null`. The backend later emits `conversation_linked`, but the frontend only
  logged the payload and never updated connection state. Snapshot recovery also
  dropped `wire.conversation_id` in `denormalizeSnapshot`. A subsequent
  `tool_watchdog_changed` therefore called `maybeNotifyToolWatchdog` with a
  null id and omitted `NotificationTarget` — OS click could not navigate.
- **Fix:**
  1. New reducer action `CONVERSATION_LINKED` sets `conversationId` on the live
     connection.
  2. `prepareMappedEnvelope` dispatches that action for `conversation_linked`
     (log afterCommit retained).
  3. `SnapshotPatch.conversationId` preserved from `wire.conversation_id`;
     `HYDRATE_FROM_SNAPSHOT` fill-null merges it (never clears a bound id).
- **Regression test:** connect with null id → `conversation_linked(99)` →
  connection id is 99 → `tool_watchdog_changed` notify includes
  `{ kind: "conversation", conversationId: 99 }`.
- **Also:** denormalize unit tests for `conversationId` mapping.

### Verification (r2 I1)

```powershell
pnpm test -- src/components/conversations/tool-watchdog-banner.test.tsx `
  src/contexts/acp-connections-context.test.tsx `
  src/lib/api.test.ts src/lib/notification.test.ts `
  src/lib/snapshot-denormalize.test.ts
# 145 passed

pnpm eslint src/contexts/acp-connections-context.tsx `
  src/contexts/acp-connections-context.test.tsx `
  src/lib/snapshot-denormalize.ts src/lib/snapshot-denormalize.test.ts
# 0 errors; 1 pre-existing exhaustive-deps warning
```

### Files touched in r2 I1 fix

| Path | Change |
| --- | --- |
| `src/contexts/acp-connections-context.tsx` | `CONVERSATION_LINKED` action + reduce + event map + hydrate fill-null |
| `src/contexts/acp-connections-context.test.tsx` | null → linked → notify target regression |
| `src/lib/snapshot-denormalize.ts` | preserve `conversationId` on patch |
| `src/lib/snapshot-denormalize.test.ts` | conversation_id mapping tests |
| `src-tauri/src/commands/notification.rs` | click wire + omit + dedupe |
| `src-tauri/src/commands/tool_watchdog.rs` | camelCase lease action body |
| `src-tauri/src/web/handlers/tool_watchdog.rs` | camelCase request body |
| `src-tauri/src/lib.rs` | send_notification new arg |
| `src-tauri/Cargo.toml` / `Cargo.lock` | notify-rust (Linux click) |
