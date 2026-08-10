# WebView2 Conversation Pop-out Version Gate Design

**Date:** 2026-08-10
**Status:** Approved in conversation; pending written-spec review

## Problem

On Windows, the shared Evergreen WebView2 Runtime can update while DrawCode is
still running. The existing DrawCode process and browser process continue using
the startup Runtime, but a later conversation pop-out can attempt to create a
new WebView against the newly installed Runtime. On the affected machine this
produced mixed WebView2 versions in one long-lived process and failed window
creation with `RPC_E_DISCONNECTED` (`0x80010108`).

The failure is isolated to creating a new WebView after Runtime drift. Existing
agent tasks do not need to be interrupted, and DrawCode does not need to restart
until the user asks to use conversation pop-out again.

## Decision

Gate only the creation of a new desktop conversation pop-out:

1. Keep the WebView2 version captured at DrawCode process startup.
2. Immediately before creating a new conversation WebView, query the available
   Runtime version again.
3. If the versions differ, do not create the window. Return a typed,
   localizable error and show a persistent restart-required toast.
4. Let the user restart DrawCode explicitly. Do not automatically restart or
   wait for active tasks.

Focusing an already existing conversation pop-out remains allowed because that
path does not create another WebView.

## Goals

1. Prevent the known mixed-Runtime conversation pop-out failure before native
   window creation begins.
2. Leave existing agent turns, goals, delegations, terminals, and windows
   untouched until the user explicitly restarts.
3. Preserve the existing focus-existing behavior for an already open pop-out.
4. Provide a one-click real application relaunch; closing the main window is
   not sufficient on Windows because it normally hides DrawCode to the tray.
5. Keep web/server pop-out behavior and non-Windows desktop behavior unchanged.
6. Reuse the existing Runtime snapshot, structured command errors, pop-out
   compensation, localization, and Tauri relaunch facilities.

## Non-goals

- Listening continuously for WebView2 updates.
- Automatically restarting DrawCode when work becomes idle.
- Blocking new agent tasks, waiting for long-running goals, or inspecting task
  lifecycle state.
- Pre-creating or pooling renderer/WebView slots.
- Changing the system WebView2 update policy.
- Bundling a Fixed Version Runtime.
- Protecting every auxiliary window type in this iteration. The gate applies
  only to desktop conversation pop-outs.
- Providing an atomic lock against WebView2 updating in the small interval
  between the version check and native WebView creation.

## Existing Architecture

### Startup Runtime snapshot

`src-tauri/src/window_diagnostics.rs::initialize` already calls
`tauri::webview_version()` before Tauri window construction and stores the
result in the process-wide `PROCESS_STATE` `OnceLock`. This is the startup
baseline and requires no new registry reader or dependency.

### Conversation pop-out command

`open_conversation_window` currently performs these relevant steps:

1. Validate input and caller.
2. Focus an existing pop-out, if present.
3. Resolve title and URL.
4. Insert the pop-out operation record.
5. Build the new `WebviewWindow`.

The version gate belongs between steps 2 and 3. Therefore a drift rejection is
guaranteed to happen before the backend inserts an operation or creates a
window.

### Frontend handoff

The desktop frontend establishes an in-memory transfer fence before invoking
`open_conversation_window`. Generic open failures enter the full compensation
path because older or asynchronous paths might have created backend state.

The Runtime-drift error has a stronger contract: the backend returns it before
creating any operation. The frontend can therefore cancel its ready wait and
clear the transfer fence directly. It must not run the generic abort/reverse
poll for this error.

### Relaunch

`src/lib/updater.ts::relaunchApp` already calls
`@tauri-apps/plugin-process.relaunch`. Runtime drift uses this plain relaunch,
not `restart_app`, because `restart_app` is reserved for a staged DrawCode
self-update.

## Detailed Design

### 1. Runtime drift projection

Add a small Windows-aware projection in `window_diagnostics` that compares:

- `startup`: `current_process_state().snapshot.webview_version`
- `available_now`: a fresh `tauri::webview_version()` result

The projection has three outcomes:

| Outcome | Meaning | Pop-out behavior |
| --- | --- | --- |
| `Unchanged` | Both versions are present and equal | Continue |
| `Changed` | Both versions are present and differ | Reject new WebView |
| `Unknown` | Either query is unavailable | Fail open and log |

Comparison uses trimmed exact version strings. WebView2 versions contain four
numeric components, so they must not be parsed with the three-component SemVer
model. The user-facing error does not need to expose either version; structured
diagnostics may log both because version numbers are not sensitive.

On non-Windows platforms the projection always returns `Unchanged` without a
Runtime query.

Failing open on `Unknown` avoids disabling pop-out for an unrelated transient
Runtime query error. Existing window-creation diagnostics remain the fallback
for a real creation failure.

### 2. Backend creation gate

In `open_conversation_window`:

1. Keep validation and `try_focus_existing_conversation_window` unchanged.
2. If an existing window was focused, return `FocusedExisting` without checking
   drift.
3. Otherwise evaluate the Runtime drift projection.
4. On `Changed`, emit a `webview_runtime_drift_blocked_popout` diagnostic and
   return `AppCommandError::window(...)` with a stable i18n key.
5. Only after `Unchanged` or `Unknown` proceed to database lookup,
   `popout.insert_opened`, and `WebviewWindowBuilder`.

Stable wire key:

```text
ConversationPopout.runtimeRestartRequired
```

The frontend treats this key as the semantic discriminator. No new broad
`AppErrorCode` variant is required; the existing `window_operation_failed`
category remains accurate.

### 3. Frontend error handling

Add a classifier for the stable Runtime-drift i18n key. When
`openConversationWindow` rejects with that error, `popOutConversation` must:

1. Cancel the armed ready/closed wait.
2. Call `clearTransferringOut(conversationId, operationId)` directly.
3. Skip `compensate`, because the backend contract guarantees that no pop-out
   operation exists.
4. Rethrow a recognizable Runtime-restart-required error to the UI caller.

All other open failures retain the existing compensation behavior.

Both desktop pop-out entry points, the tab bar and sidebar conversation card,
handle the recognizable error before their generic failure toast.

### 4. Restart-required UX

Show a persistent Sonner toast with a fixed ID so repeated pop-out attempts
replace or reuse one notification rather than stacking notifications.

Required localized content:

- Message: WebView2 was updated. Restart DrawCode before using conversation
  pop-out. Restarting interrupts currently running tasks.
- Action: Restart DrawCode.

Behavior:

- The toast does not restart automatically.
- The user may dismiss it and keep working in the main window.
- Clicking the action calls `relaunchApp()`.
- If relaunch rejects, log the failure and show a localized restart-failed
  message; do not claim that restart succeeded.
- A later pop-out attempt while drift remains shows the same fixed-ID toast
  again.

All ten locale catalogs receive the new message, action label, and restart
failure text.

## Data Flow

```text
User clicks Pop out
  -> frontend establishes transfer fence
  -> Rust focuses existing pop-out, if any
       -> focused: return success; no version gate
  -> Rust compares startup Runtime with available Runtime
       -> equal/unknown: continue existing open + handoff flow
       -> changed: return typed restart-required error before operation insert
  -> frontend cancels wait and clears transfer fence
  -> UI shows one persistent restart-required toast
       -> Later: user continues working
       -> Restart: plain Tauri relaunch, new startup baseline is captured
```

## Failure and Race Handling

- **Startup version unavailable:** fail open and retain diagnostics.
- **Current version query fails:** fail open and retain diagnostics.
- **Runtime changes after the preflight check:** this narrow time-of-check/
  time-of-use race is not eliminated. The existing 15-second ready timeout,
  compensation, and window diagnostics remain active. A post-failure drift
  reclassification can be added later if field evidence shows this race.
- **User dismisses the toast:** no state is persisted; the next pop-out attempt
  checks again and re-prompts.
- **Relaunch fails:** current process and tasks remain alive; report failure.
- **Agent task is active:** no automatic action occurs. Only the user-triggered
  restart can interrupt it, and the prompt states that consequence.

## Testing

### Rust unit tests

1. Equal startup/current versions project `Unchanged`.
2. Different startup/current versions project `Changed`.
3. Missing startup version projects `Unknown` and fails open.
4. Failed current query projects `Unknown` and fails open.
5. Four-component versions are compared as trimmed strings.
6. The drift error carries the exact stable i18n key.
7. The gate is evaluated only on the create-new path, after focus-existing and
   before operation insertion.

### Frontend unit tests

1. Runtime-drift command error cancels the wait and clears the transfer fence.
2. Runtime-drift command error does not invoke generic abort compensation.
3. Generic open errors still invoke existing compensation.
4. Tab-bar and sidebar entry points show the restart-required toast instead of
   `popOutHandoffFailed`.
5. Repeated errors use one fixed toast ID.
6. Restart action invokes `relaunchApp`.
7. Relaunch rejection shows the restart-failed message.

### Regression checks

- Existing desktop pop-out handoff tests remain green.
- Existing-window focus remains available after simulated Runtime drift.
- Pure web pop-out behavior is unchanged.
- Non-Windows builds do not acquire Windows-only dependencies or behavior.

## Acceptance Criteria

1. With startup Runtime `.59` and currently available Runtime `.72`, requesting
   a new conversation pop-out creates no backend operation and no new window.
2. The main conversation stays usable and ownership remains with the main
   window after the rejection.
3. A single persistent localized prompt offers a real DrawCode relaunch.
4. Dismissing the prompt never interrupts active work.
5. After relaunch, the new baseline matches the installed Runtime and pop-out
   follows its normal handoff flow.
6. An already existing pop-out can still be focused during Runtime drift.

## Alternatives Considered

### Automatically restart after all work drains

Rejected for this scope. It requires authoritative aggregation of active turns,
open goals, permissions, delegation continuations, automations, and terminal
work, and can still wait indefinitely.

### Pre-create a renderer pool

Rejected. It adds persistent memory use and does not guarantee that existing
controllers remain healthy after a Runtime update.

### Fixed Version Runtime or managed update window

Rejected for this scope. Both shift Runtime distribution and security-update
responsibility to DrawCode or an administrator. The pop-out gate addresses the
observed failure without changing system servicing.
