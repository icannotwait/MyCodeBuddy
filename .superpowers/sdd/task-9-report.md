# Task 9 Report — Persistent Warning Banner, Countdown, and User Actions

**Status:** DONE  
**Branch:** `feat/tool-execution-watchdog`  
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
