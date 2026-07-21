# Task 8 Report: Integration verification + final polish

**Status:** DONE  
**Branch:** `feat/conversation-popout-window`  
**Worktree:** `D:\MyCodeBuddy\.worktrees\conversation-popout-window`  
**Date:** 2026-07-21

## Summary

Full verification suite for conversation pop-out was run on this worktree. One Clippy failure introduced by pop-out Tauri commands was fixed (`too_many_arguments` on `open_conversation_window` and `rebind_connection_owner_window`). Required test inventory from Tasks 2–7 is present with documented gaps for pure-Tauri window E2E cases. Manual multi-monitor smoke was not run in this environment.

Design/plan docs are already on the branch (`docs/superpowers/specs/2026-07-20-conversation-popout-window-design.md`, `docs/superpowers/plans/2026-07-21-conversation-popout-window.md`).

## Commits (this task)

| SHA | Message |
| --- | --- |
| _(pending at report write; see git log)_ | `fix(clippy): allow too_many_arguments on popout Tauri commands` |

## Verification matrix

### Frontend

| Command | Result | Notes |
| --- | --- | --- |
| `pnpm test` | **PASS** | 273 files / **3487** tests passed (~37s) |
| `pnpm eslint .` | **FAIL (env)** | ~240k `prettier/prettier` `Delete ␍` errors — whole checkout is CRLF on Windows vs LF prettier norm. Not introduced by Tasks 6–7. |
| Popout-path eslint with `--rule "prettier/prettier: off"` | **PASS** | 0 errors; 8 unused-arg warnings in test mocks only |
| `pnpm build` | **PASS** | Next.js 16 static export; `/conversation` route present |

**ESLint mitigation used:**

```powershell
pnpm eslint <popout-related paths> --rule "prettier/prettier: off"
```

Do **not** mass-run `eslint --fix` on the whole tree on Windows CRLF checkouts (would thrash line endings). Prefer path-scoped lint or LF-normalized blobs if CI uses LF.

### Rust desktop (`src-tauri/`)

| Command | Result | Notes |
| --- | --- | --- |
| `cargo test --features test-utils` | **PASS** | lib 2439 passed / 1 ignored; integration suites all green |
| `cargo check` | **PASS** | sidecar placeholder warning only |
| `cargo clippy --all-targets --features test-utils -- -D warnings` | **PASS** after fix | Initially failed on 2× `clippy::too_many_arguments` in `conversation_popout.rs` |

### Server mode

| Command | Result | Notes |
| --- | --- | --- |
| `cargo check --no-default-features --bin codeg-server` | **PASS** | |
| `cargo test --no-default-features --bin codeg-server --lib` | **PASS** | 2387 passed / 1 ignored |
| `cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings` | **PASS** | |

### codeg-mcp companion

| Command | Result | Notes |
| --- | --- | --- |
| `cargo check --no-default-features --bin codeg-mcp` | **PASS** | |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | **PASS** | |

### Targeted re-runs after clippy fix

| Suite | Result |
| --- | --- |
| Popout FE vitest (10 files) | 90 passed |
| `commands::conversation_popout` unit tests | 13 passed |
| Ownership/CAS manager tests (listed below) | each 1 passed |

## Fix applied (Task 8)

**File:** `src-tauri/src/commands/conversation_popout.rs`

```rust
#[allow(clippy::too_many_arguments)]
// on open_conversation_window and rebind_connection_owner_window
```

Matches existing Tauri-command pattern in `commands/acp.rs` and elsewhere. Argument counts come from injected `State`/`AppHandle` plus domain params; no behavior change.

## Required test inventory (Tasks 2–7)

| Requirement | Status | Where covered |
| --- | --- | --- |
| **Prompting + idle handoff (release without disconnect)** | **Present** | FE: `use-connection-lifecycle.test.ts` (`shouldDisconnectOnUnmount` keeps prompting owner; disconnects idle); `conversation-popout-acp-bridge.test.ts` (`releaseConnectionWithoutDisconnect`, suppress disconnect). Rust: `acp::manager::tests::sweep_idle_skips_prompting_connection`. |
| **Owner promotion (not permanent viewer)** | **Present (partial unit / impl-proven)** | Bridge: `claimConnectionOwnership` delegates with generation/label. Bootstrap: `decideLiveHandoffResult` requires rebind+claim match; reverse on claim failure. Impl in `acp-connections-context.tsx` sets `isViewer: false` on claim. **Gap:** no full provider mount test asserting claim → `isViewer:false` (would need heavier AcpConnectionsProvider harness). |
| **Detached close during initialization** | **Present** | Rust: `begin_registration_rejects_tombstoned_and_tracks_inflight`, `begin_registration_rejects_close_reserved_before_tombstone`, `close_fence_with_inflight_registration_then_final_reap_window`. FE: suppress remains until ack; live rebind/claim failure blocks ready. |
| **Open/focus idempotency** | **Present (logic + FE)** | FE: `openTab` awaits focus; returns false without adding tab when detached focuses; cold-cache race. Rust: open path returns `FocusedExisting` when label exists (window API). **Gap:** no pure unit test for `OpenConversationResult::FocusedExisting` (needs Tauri window mock / E2E). |
| **Close cleanup isolation (main vs conversation-\*)** | **Present** | Rust: `reserve_close_is_per_operation_not_conversation`, `capture_close_operation_is_idempotent_and_survives_reopen`, `disconnect_by_owner_window_and_operation_reaps_stamped_cold_conn`, `disconnect_if_owner_cas_skips_reused_main_connection`. |
| **Concurrent child spawn rebind** | **Present (related)** | Rust: `spawn_agent_cold_dedup_rejects_main_owned_and_reuses_same_incarnation`; rebind admit/record vs abort atomicity (`decide_abort_never_rebound_is_atomic_with_in_flight`, `admit_forward_rebind_rejects_terminal`); lease CAS on disconnect after rebind. **Gap:** no multi-task stress test of two concurrent `rebind_connection_owner_window` calls racing the same connection (would need async harness). |
| **CAS reject restore token** | **Present** | FE: `conversation-popout.test.ts` — detach CAS fail after ready aborts then `restoreDetachedTab`; already-complete handoff skips restore/close. `tab-store-popout.test.ts` — detach returns restore token + restore round-trip. |
| **Sidebar single + double click focus-before-open** | **Present** | `tab-store-popout.test.ts` openTab focus-before-open; `search-command-dialog.focus.test.tsx` selection short-circuit; sidebar card menu pop-out tests; list routes clicks through async `openTab` (Task 7). |

### Key test file index

```
src/lib/conversation-popout.test.ts
src/lib/conversation-popout-acp-bridge.test.ts
src/lib/conversation-popout-detached-bootstrap.test.ts
src/app/conversation/_components/detached-bootstrap-flow.test.ts
src/stores/tab-store-popout.test.ts
src/hooks/use-connection-lifecycle.test.ts
src/components/conversations/search-command-dialog.focus.test.tsx
src/components/conversations/sidebar-conversation-card.test.tsx
src/components/workspace/deep-link-bootstrap.test.tsx
src-tauri/src/commands/conversation_popout.rs  (mod tests)
src-tauri/src/acp/manager.rs                 (ownership / idle / cold-dedup tests)
```

## Manual Windows smoke

| Scenario | Status |
| --- | --- |
| Two monitors / snap | **Skipped** — no interactive desktop run in this agent session |
| Last tab pop-out disabled | Covered by unit tests (`canPopOutConversation` / `detachTab` last_tab); UI smoke skipped |
| Hide-to-tray | **Skipped** |
| Remote menu hidden | Unit: `canPopOutConversation` not_local_desktop + card menu tests; full remote workspace smoke skipped |

Recommend a short human pass on local desktop before merge if not already done in Tasks 6–7.

## Critical / Important findings

| Severity | Finding | Action |
| --- | --- | --- |
| **Important (build gate)** | Clippy `-D warnings` failed on pop-out Tauri commands | Fixed with `#[allow(clippy::too_many_arguments)]` |
| **Env** | Whole-repo eslint unusable on Windows CRLF | Documented; path-scoped lint OK |
| **Minor gap** | No provider-level claim→owner unit test | Acceptable; pure helpers + bridge cover contract |
| **Minor gap** | No multi-thread concurrent rebind stress | Acceptable; atomic admit/record + cold-dedup cover intended races |

No Critical product logic failures found in full test suites.

## Design success criteria (1–9) — verification posture

| # | Criterion | Evidence |
| --- | --- | --- |
| 1 | Local desktop pop-out | FE gates + Rust open command; build includes `/conversation` |
| 2 | Snap / multi-monitor | Manual only — skipped |
| 3 | Overlays in detached | Surface extract + mount gate tests |
| 4 | Last-tab guard | Unit tests |
| 5 | MRU after detach | `tab-store-popout` |
| 6 | No re-dock | Design + CAS restore only on failure |
| 7 | Focus existing | openTab + FocusedExisting path |
| 8 | No restore / web-remote hidden | canPopOut + local desktop gate |
| 9 | Handoff safe | rebind/claim/suppress/close fence tests |

## Concerns / follow-ups

1. Normalize line endings (LF) for Windows contributors or configure prettier `endOfLine: "auto"` if project agrees — otherwise `pnpm eslint .` stays red locally.
2. Optional: AcpConnectionsProvider claim ownership unit test (`isViewer: false`, no second spawn).
3. Optional: Tauri-level open/focus idempotency integration test.
4. Human multi-monitor + tray smoke before release.

## Self-review

1. **Spec coverage:** verification suite + inventory complete for Tasks 1–8 handoff/rebind/detach/focus/menus/i18n/capabilities.
2. **Placeholders:** none intentional.
3. **Types:** no type changes this task; clippy allow only.
