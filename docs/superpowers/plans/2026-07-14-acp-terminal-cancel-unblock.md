# ACP Terminal Cancel Unblock Implementation Plan

**Goal:** Unblock cancel/release when an agent is blocked on `waitForExit`, emit `TurnComplete { cancelled }` before terminal cleanup, and bound kill wait so connection recovery cannot hang.

**Architecture:** Per-`TerminalInstance` `CancellationToken` + exit `Notify`. Waiters select on process exit vs cancel and drop the child mutex on cancel. Kill cancels first, then kills the process tree and publishes status. Session release uses a per-terminal timeout with background continuation.

**Tech Stack:** Rust 2021, Tokio, `tokio_util::sync::CancellationToken`, `kill_tree`, existing ACP terminal runtime tests.

## File Map

| File | Change |
|---|---|
| `src-tauri/src/acp/terminal_runtime.rs` | Token/notify, rewrite wait/kill/release, regression test |
| `src-tauri/src/acp/connection.rs` | Mid-prompt Cancel: TurnComplete before `release_all_for_session` |
| `docs/superpowers/specs/2026-07-14-acp-terminal-cancel-unblock-design.md` | Design (already written) |

## Tasks

### Task 1: TerminalInstance cancel + wait/kill redesign

- [x] Add `cancel` + `exit_notify` fields
- [x] `wait_for_exit`: select; on cancel drop lock and await published status
- [x] `kill_command`: cancel token first, then kill_tree + wait + publish + notify
- [x] Helper to await published exit status without busy-spinning forever

### Task 2: Bounded session release

- [x] Constant for kill bound (e.g. 3s)
- [x] `release_all_for_session` timeout + background continue

### Task 3: Connection cancel ordering

- [x] Mid-prompt Cancel: emit TurnComplete before terminal release

### Task 4: Regression test + verify

- [x] Concurrent long-running wait + release completes within bound
- [x] `cargo test` for terminal_runtime module
- [x] `cargo check` / clippy as needed
