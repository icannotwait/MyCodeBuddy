# Conversation Pop-out Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users pop a conversation into an independent local-desktop `WebviewWindow` with safe ACP ownership handoff (move, not mirror), MRU tab focus, and no restart restore.

**Architecture:** Independent static route `conversation` + Tauri `WebviewWindow` label `conversation-{id}` (no parent). Main orchestrates a transfer state machine: register ready waiter → open window → detached claims ownership + rebinds root tree → main release-without-disconnect → `detachTab` with awaited CAS. Frontend uses a props-driven session surface; registry cache is non-authoritative.

**Tech Stack:** Tauri 2 (`tauri-runtime`), Rust ACP `ConnectionManager`, Next.js static export, React 19, Zustand tab store, next-intl, Vitest.

## Global Constraints

- Local desktop only: `isLocalDesktop()` + Rust reject remote caller windows; no `remote_connection_id` in v1.
- Static export: fixed path `src/app/conversation/page.tsx` + query params only.
- Capabilities: add `conversation-*` to `default.json` and `desktop.json`.
- No mirror: main tab removed only after ready; CAS failure reverses rebind before closing detached.
- No restart restore of detached windows.
- Do not leave orphan agent processes; cascade rebind root descendant tree only.
- Workspace-global `opened_tabs` CAS removal is intentional (same as closing a tab).
- i18n: all 10 locales under existing next-intl layout.
- Prettier/ESLint/TS strict and Rust clippy as project norms.
- Spec baseline: `docs/superpowers/specs/2026-07-20-conversation-popout-window-design.md`.

---

## File map

| Path | Responsibility |
| --- | --- |
| `src-tauri/capabilities/default.json`, `desktop.json` | Allow `conversation-*` windows |
| `src-tauri/src/commands/windows.rs` | `open_conversation_window`, `focus_conversation_window` |
| `src-tauri/src/acp/manager.rs` | `rebind_connection_owner_window` + generation/descendant revalidation |
| `src-tauri/src/lib.rs` | Register commands; close cleanup for `conversation-*` |
| `src/lib/conversation-popout.ts` | Enablement, single-flight, popOut orchestration, cache |
| `src/lib/api.ts` / `src/lib/tauri.ts` | Desktop invoke wrappers |
| `src/stores/tab-store.ts` | `lastActivatedAt` / MRU, `detachTab`, `flushOpenedTabsSave` |
| `src/contexts/acp-connections-context.tsx` | `releaseWithoutDisconnect`, `claimOwnership`, transferredOut skip in idle/unmount |
| `src/hooks/use-connection-lifecycle.ts` | Honor detaching / transferredOut suppress |
| `src/components/conversations/conversation-session-surface.tsx` | Props-driven session UI extracted from tab view |
| `src/components/conversations/conversation-detail-panel.tsx` | Thin wrapper over surface from tab row |
| `src/app/conversation/page.tsx` | Detached window page + minimal providers |
| `src/components/tabs/tab-item.tsx` | Pop-out menu item |
| `src/components/conversations/sidebar-conversation-card.tsx` | Pop-out menu item |
| `src/components/conversations/sidebar-conversation-list.tsx` (or click path) | Focus detached before openTab |
| `src/i18n/messages/*.json` | Keys for pop-out |
| Tests: `*.test.ts(x)` beside modules; Rust unit tests in manager |

---

### Task 1: Capabilities + operation lifecycle + open/focus + close

**Files:**
- Modify: `src-tauri/capabilities/default.json`, `desktop.json`
- Modify: `src-tauri/src/commands/windows.rs`, `src-tauri/src/lib.rs`
- Create/modify: small op-state helper used by windows close + abort/complete (so Task 1 `cargo check` stands alone)

**Interfaces:**
```rust
enum OpenConversationResult { Opened, FocusedExisting }
// FocusedExisting: no new op record; caller must not wait for ready

open_conversation_window(..., operation_id) -> Result<OpenConversationResult>
focus_conversation_window(conversation_id) -> Result<bool>
abort_conversation_popout_operation(operation_id) -> Result<AbortOutcome> // idempotent terminal
complete_conversation_popout_operation(operation_id) -> Result<PopoutOpStatus>
// PopoutOpStatus: { phase: Opening|ReadyPending|HandoffComplete|Aborted, abort_outcome? }
// Idempotent: if already HandoffComplete, returns that phase (not error)

get_conversation_popout_operation(operation_id) -> Result<PopoutOpStatus>
// status query for lost-ack recovery; record retained until ack TTL after complete
```

- Label still `conversation-{id}` for window identity, but **ownership incarnation is `operationId`** (not label alone)
- Connections/terminals store `owner_window_label` **and** `owner_operation_id` (or equivalent incarnation token set at open/rebind/connect)
- Op record **only on Opened**; phases include terminal `Aborted(outcome)` (tombstone retained until ack/TTL)
- `close_conversation_window(conversation_id, expected_operation_id) -> Result<bool>` — CAS close only if stored op matches
- Close (dedupe per operationId):
  - Disconnect / kill only resources matching **this operationId incarnation**
  - HandoffComplete / AlreadyComplete → disconnect incarnation
  - NeverRebound / Aborted before complete → disconnect incarnation-owned resources (cold/half-open)
  - Reversed → still **kill residual terminals/connections still tagged with this operationId** after reverse (leak prevention); do not touch reclaimed main-owned resources without this operationId
  - Superseded → no frontend reclaim/tab restore, but **still tombstone + reap residual resources** matching the superseded `(label, operationId)`
- Emit closed with operationId + abortOutcome
- **Closing tombstone is (label, operationId)** — late `acp_connect` with stale operationId rejected; new open uses new operationId

- [ ] **Step 1: Capabilities + op APIs + open/focus + close**
- [ ] **Step 2: Tests Opened/FocusedExisting; abort idempotent; close table**
- [ ] **Step 3: cargo check + tests**
- [ ] **Step 4: Commit** `feat(windows): conversation pop-out window lifecycle`

---

### Task 2: Rust root-tree rebind + generation CAS + child fence

**Files:**
- Modify: `src-tauri/src/acp/manager.rs`, `session_state.rs`, `connection.rs`, `commands/acp.rs`
- Modify: `src-tauri/src/terminal/manager.rs`, `commands/terminal.rs` (owner_operation_id on spawn; kill by label+operationId)
- Modify: frontend terminal spawn wrappers if they pass owner label
- Test: manager rebind races; terminal old-incarnation close vs new spawn; delayed Destroyed A after B reopen

**Interfaces:**
```rust
pub struct RebindResult {
  pub rebound_count: usize,
  pub ownership_generation: u64,
  pub operation_id: String,
}

rebind_connection_owner_window(
  conversation_id: i32,
  connection_id: Option<String>,
  from_owner_window: String,
  to_owner_window: String,
  operation_id: String,
  expected_generation: Option<u64>, // required on reverse; None means "accept any current gen on first forward"
) -> Result<RebindResult, AppCommandError>
```

Rules:
- Locate **root only** by connection_id and/or conversation_id; rebind root + **descendant tree only**
- Label CAS: must be `from`, or already `to` **and** `owner_operation_id == operation_id` (idempotent same operation only)
- **Single critical section:** forward rebind + op-record generation + `owner_operation_id` stamp + phase transition are one serialized transaction. Terminal `Aborted` rejects late rebind.
- **Generation CAS on reverse:** require `expected_generation`
- Stamp generation + **operation_id incarnation** on root/descendants under same lock
- **Child spawn fence (concrete):** track in-flight spawns per parent; rebind either waits or marks them; at registration child performs parent-generation CAS and adopts parent’s current `(label, generation, operationId)` before becoming visible. Barrier test required.
- **Incarnation on registration:** `acp_connect` / `terminal_spawn` take `owner_operation_id`. Registration stores it. Pre/post tombstone check.  
  **Two layers:**  
  1) **Safety tombstone** `(label, operationId)` retained until in-flight registration refcount for that operationId hits 0 (independent of status-record TTL / coordinator ack).  
  2) **Admission:** accept registration only if `operationId` is the **current registration-accepting incarnation** for that conversation window (the Opened/HandoffComplete op not yet closed). Any other operationId is rejected even if no newer open exists yet.
- **Destructive disconnect CAS (all paths that can race with rebind):**  
  - Frontend: `acp_disconnect(connection_id, expected_owner_window, expected_operation_id)` required; lease tokens stored on local connection state at own/rebind/reclaim time (not re-queried at disconnect).  
  - **Backend idle sweep / any delayed disconnect:** when removing a connection, re-validate idle predicate **and** owner+operationId under the same lock before remove; if rebind changed incarnation, skip.  
  - App quit may still force disconnect_all.  
  Tests: sweep-vs-rebind; disconnect-before-rebind; rebind-then-stale-disconnect rejected

Also expose `owner_window_label` on discovery DTO.

**Durable operation record:**
```rust
// Created only on Opened. Terminal Aborted(outcome) retained as tombstone until
// coordinator ack OR bounded TTL (e.g. 2 min) — not wiped solely by window destroy
// until abort outcome was delivered at least once (emit closed carries it).

enum AbortOutcome { NeverRebound, AlreadyMain, Reversed{generation}, Superseded{..}, AlreadyComplete }
```

**Frontend compensation (flag-based, not outcome-only):**

| Condition | Action |
| --- | --- |
| `mainReleased` && outcome ∈ {NeverRebound, AlreadyMain, Reversed} | reclaim ACP |
| `tabRemoved` && outcome ∈ {NeverRebound, AlreadyMain, Reversed} | restoreDetachedTab + flush retry |
| Superseded / AlreadyComplete | clear this op fences only; no reclaim/restore against newer owner; **still reap residual resources for this op's (label, operationId)** |
| NeverRebound on Rust close | disconnect connections still tagged with **this operationId** (tombstone) |

- [ ] **Step 1: Failing tests** — rebind tree; concurrent child; close-before-ready; complete-then-close; supersede reaps A residuals without closing B; cold open; A-close-after-B-reopen

- [ ] **Step 2: Implement**

- [ ] **Step 3: `cargo test --features test-utils rebind_connection_owner`**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(acp): generation-CAS rebind for conversation pop-out"
```

---

### Task 3: Tab store detachTab + MRU + awaited CAS flush + restore token

**Files:**
- Modify: `src/stores/tab-store.ts`
- Test: `src/stores/tab-store-popout.test.ts` (new)

**Interfaces:**
```ts
type DetachTabOk = {
  ok: true
  nextActiveId: string
  restoreToken: {
    tab: TabItemInternal // full removed row (pin, folder, agent, workingDir, title, activationSeq)
    index: number
    previousActiveTabId: string | null
  }
}
type DetachTabErr = { ok: false; reason: "not_found" | "last_tab" }

detachTab(tabId: string): DetachTabOk | DetachTabErr
restoreDetachedTab(token: DetachTabOk["restoreToken"]): void
// MERGE into **current** rawTabs (never wholesale replace with a stale full snapshot):
// 1) If a tab for the same conversation already exists → activate **that** tab (do not duplicate).
// 2) Else insert `token.tab` at min(index, length), then activate the restored tab.
// 3) `previousActiveTabId` is used only when deciding post-insert focus if (1) did not apply
//    and previousActive still exists **and** caller requests preserve-focus mode; default for
//    handoff compensation is activate the restored conversation tab.

flushOpenedTabsSave(): Promise<{ accepted: boolean; version: number }>
// - Cancels debounce; serializes with in-flight save
// - Returns accepted to caller (orchestrator owns compensation)
```

- Monotonic `activationSeq` (module counter++)
- `detachTab` never replacement draft; requires `rawTabs.length >= 2`
- MRU via max `activationSeq`

- [ ] **Step 1: Tests** — last tab; MRU; restore merges under concurrent tab edits; flush accepted false

- [ ] **Step 2: Implement**

- [ ] **Step 3: `pnpm exec vitest run src/stores/tab-store-popout.test.ts`**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(tabs): detachTab with restore token and serialized flush"
```

---

### Task 4: ACP frontend transfer helpers + coordinator bridge

**Files:**
- Modify: `src/contexts/acp-connections-context.tsx`, `use-connection-lifecycle.ts`
- Create: `src/lib/conversation-popout-acp-bridge.ts` (or export store-level helpers)
- Extend discovery DTO types in `src/lib/types.ts` if Rust adds `owner_window_label`
- Test: transfer / idle-sweep / claim validation tests

**Interfaces:**
Because each webview has its own React ACP context, pure `popOutConversation` **cannot** call context methods directly. Use an explicit bridge:

```ts
// Registered by AcpConnectionsProvider on mount (main + detached)
export type PopoutAcpBridge = {
  findLocalOwnerContextKey(conversationId: number): string | null
  getLocalConnectionId(conversationId: number): string | null
  markTransferringOut(conversationId: number, operationId: string): void
  clearTransferringOut(conversationId: number, operationId: string): void
  // clear is compare-and-clear: only clears if fence.operationId matches
  releaseConnectionWithoutDisconnect(conversationId: number, operationId: string): void
  reclaimAfterAbort(conversationId: number, operationId: string): Promise<void>
  // delayed A cleanup must not clear B's fences (test immediate reopen)
  claimConnectionOwnership(args: {
    conversationId: number
    connectionId?: string | null // null/undefined = cold path
    agentType: AgentType
    workingDir: string
    operationId: string
    expectedOwnerWindowLabel?: string // live: must match discovery; refuse "web"
  }): Promise<{ ownershipGeneration?: number; connectionId?: string } | void>
  // Live: may call rebind wrapper and return generation, OR page calls
  // rebindConnectionOwnerWindow separately then returns generation in ready.
  // Cold: returns {} / void with no generation.
}

export function registerPopoutAcpBridge(bridge: PopoutAcpBridge | null): void
export function getPopoutAcpBridge(): PopoutAcpBridge | null
```

Semantics:
- `releaseConnectionWithoutDisconnect`: drop local owner UI; set transferredOut; **never** `acpDisconnect`; idle sweep + unmount skip
- `claimConnectionOwnership`: optional `connectionId` — **live** path validates `owner_window_label` (refuse `"web"`) and takes over; **cold** path (`connectionId == null`) skips takeover/rebind and only boots empty owner UI (normal connect on first use)
- Prefer atomic backend path when feasible for live claim+rebind

- [ ] **Step 1: Tests** — sweep skip; unmount skip; claim refuses web owner; bridge null safe

- [ ] **Step 2: Implement bridge + helpers**

- [ ] **Step 3: vitest**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(acp): pop-out ownership bridge and release-without-disconnect"
```

---

### Task 5: Pop-out orchestration module + API wrappers

**Files:**
- Create: `src/lib/conversation-popout.ts`, `src/lib/conversation-popout.test.ts`
- Modify: `src/lib/api.ts` wrappers via `getShellTransport`:
  - `openConversationWindow`
  - `focusConversationWindow`
  - `rebindConnectionOwnerWindow`
  - `abortConversationPopoutOperation`
  - `completeConversationPopoutOperation` → PopoutOpStatus
  - `getConversationPopoutOperation` → PopoutOpStatus
  - `closeConversationWindow(conversationId, expectedOperationId)` → boolean CAS

**Interfaces:** enablement + popOut/focus/cache; uses `getPopoutAcpBridge()` + tab store + api.

**Phase machine** per `operationId` (main):

`Idle → Preparing → AwaitingReady → Releasing → Done | Aborting`

**Local op flags:** `mainReleased`, `tabRemoved`, `restoreToken?`

**Branches after ready:**
- **Live + main tab:** release (`mainReleased`) → detach (`tabRemoved`) → flush → complete → cache + commit-ack
- **Live + not main tab:** release if owner (`mainReleased`) → complete → cache + commit-ack
- **Cold + main tab:** detach → flush → complete → cache + commit-ack (detached stays connect-gated until ack)
- **Cold + not main tab:** complete → cache + commit-ack

Rules:
1. Preparing: markTransferringOut if live
2. If open returns `FocusedExisting` → **clear transferring fence**, unregister ready/closed waiters, return focus success (no ready wait)
3. Ready wait only for `Opened`
4. **Commit-ack protocol (concrete):**  
   - Main registers detached listener **before** ready wait (same as ready/closed).  
   - On `complete_` returning `phase=HandoffComplete` (including idempotent already-complete): emit `conversation-window://commit-ack` `{ operationId }` to the conversation window.  
   - Detached on ack: enable connect + clear suppressFrontendDisconnect.  
   - If detached does not receive ack within T while still open: poll `get_conversation_popout_operation(operationId)`:  
     - `HandoffComplete` → same as commit-ack (enable connect, clear suppress)  
     - `Aborted` → stay gated; enter close/compensation UI (do **not** enable connect)  
     - Opening/ReadyPending → keep polling until terminal or close  
   - Tests: lost complete; delayed ack; Aborted poll; listener registered before ready.
5. **Compensation uses flags:** mainReleased → reclaim; tabRemoved → restore + ≤3 CAS retries; Superseded: fence clear + reap A residuals only (do not close B window)
6. Timeout: abort; `closeConversationWindow(id, expectedOperationId)` only — never bare label close
7. Closed mid-handoff: never detach after closed; flag-based reclaim/restore
8. Tests: FocusedExisting fence clear; abort vs rebind; close-during-cold-connect; **A timeout/close after B reopen must not close B**

- [ ] **Step 1: Unit tests**
- [ ] **Step 2: Implement orchestration + api wrappers**
- [ ] **Step 3: vitest**
- [ ] **Step 4: Commit** `feat(popout): orchestrate conversation window handoff`

---

### Task 6: Props-driven ConversationSessionSurface + detached page

**Files:**
- Create: `src/components/conversations/conversation-session-surface.tsx`
- Modify: `src/components/conversations/conversation-detail-panel.tsx`
- Create: `src/app/conversation/page.tsx` + optional `_components/detached-shell.tsx`
- Providers as required: `AcpConnectionsProvider`, `ConversationRuntimeProvider`, `SessionStatsProvider`, `DelegationProvider`, `TaskProvider`, `AlertProvider`, `AppToaster`, `RemoteConnectionGate` (local), Git credentials if composer needs them

**Bootstrap ordering (required):**
1. Parse query (`conversationId`, `folderId`, `agentType`, `operationId`); validate
2. Load conversation/folder metadata
3. Register popout ACP bridge for this webview
4. **Bootstrap before surface auto-connect:**
   - **Live:** claim + rebind (viewer-safe until rebind succeeds — no owner disconnect of main during claim); gate `isActive` until claimed; suppressFrontendDisconnect on detached for transfer lifetime
   - **Cold:** no rebind; emit ready without generation; **keep isActive=false / connect disabled** until commit-ack; after ack enable connect. Incarnation tombstone `(label, operationId)` still kills late registers if closed during connect.
- Detached `suppressFrontendDisconnect` until commit-ack; after HandoffComplete, normal disconnect still uses incarnation CAS so main cannot kill detached session
5. Emit ready after live claim/rebind or cold metadata load
6. Render surface; overlays only after active allowed

Do **not** mount workspace `TabProvider` hydrate/save. Synthetic tab seed must set persist-off.

Audit workspace-only hooks (`message-list-view` navigation/artifacts): provide stubs or include providers that those hooks require so detached does not throw.

- [ ] **Step 1: Extract surface; main tests still pass**

- [ ] **Step 2: Detached page with claim-before-activate**

- [ ] **Step 3: Targeted vitest + typecheck**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(ui): detached conversation page with session surface"
```

---

### Task 7: Menus + centralize focus-before-open + i18n

**Files:**
- Modify: `tab-item.tsx`, `tab-bar.tsx`, `sidebar-conversation-card.tsx`, `sidebar-conversation-list.tsx`
- Modify: **`src/stores/tab-store.ts` `openTab`** (or tab actions facade) to **asynchronously** `focusDetachedConversation` first when `conversationId > 0` and local desktop — if focused, **return without adding main tab**. This covers search, automations, deep links, sidebar single/double click without hunting every caller.
- Still update sidebar click paths to prefer conversation pane without forcing open when focus succeeds (if openTab short-circuits)
- All 10 locale files

**Keys:** `popOutWindow`, `cannotPopOutLastTab`, `cannotPopOutDraft`, `popOutHandoffFailed`

- [ ] **Step 1: i18n**
- [ ] **Step 2: menus + central openTab focus gate + smoke callers (search/deep-link tests)**
- [ ] **Step 3: tests**
- [ ] **Step 4: Commit** `feat(ui): pop-out menus and focus-before-open in openTab`

---

### Task 8: Integration verification + final polish

**Files:** fixups from review

Required verification (match design risk):

```bash
# Frontend (full project suite + build)
pnpm test
pnpm eslint .
pnpm build

# Rust desktop
cd src-tauri
cargo test --features test-utils
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings

# Server mode
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings

# codeg-mcp companion
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Tests that must exist by end of Tasks 2–7:
- Prompting + idle handoff (release without disconnect)
- Owner promotion (not permanent viewer)
- Detached close during initialization
- Open/focus idempotency
- Close cleanup isolation (main vs conversation-*)
- Concurrent child spawn rebind
- CAS reject restore token
- Sidebar single + double click focus-before-open

Manual Windows smoke (document in final report if environment allows): two monitors, last tab disabled, hide-to-tray, remote menu hidden.

- [ ] **Step 1: Run full verification suite above**

- [ ] **Step 2: Codex code review of full branch diff; fix Critical/Important to zero**

- [ ] **Step 3: Commit docs + any fixes**

```bash
git add docs/superpowers/specs/2026-07-20-conversation-popout-window-design.md docs/superpowers/plans/2026-07-21-conversation-popout-window.md
git commit -m "docs: conversation pop-out design and implementation plan"
```

---

## Risk checklist (from design)

| Risk | Plan response |
| --- | --- |
| Second webview viewer + main unmount kills agent | Task 4–5 transfer machine |
| Child spawn during rebind | Task 2 generation revalidation |
| Debounced CAS hides failure | Task 3 `flushOpenedTabsSave` |
| Capability missing | Task 1 |
| Detached provider incomplete | Task 6 checklist vs ConversationTabView deps |
| Remote workspace | Gate FE+Rust Task 1/5 |

## Completion criteria

Matches design success criteria 1–9 (local desktop pop-out, snap, overlays, last-tab guard, MRU, no re-dock, focus existing, no restore, web/remote hidden, handoff safe).

## Self-review

1. **Spec coverage:** handoff, rebind, detachTab, page, menus, focus, i18n, capabilities, tests → Tasks 1–8.
2. **Placeholders:** none intentional; plan-time choices listed only where design left them open.
3. **Types:** `operationId`, `RebindResult`, `detachTab` result used consistently across tasks.
