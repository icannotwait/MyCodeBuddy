# Task 3 Independent Review (Grok) — Lifecycle, Async Requests, Completions, Immutable Frames

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 3 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|3\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id | `e53d2f15-9667-4dc8-94d0-ff366f390e36` |
| Commit (HEAD) | `b55f20ddb97706ebd78126e5ffd5ef4cb249ab57` |
| BASE | `be8b41cf8545470694e2d0b490ec5b6f6cb1a227` |
| Producer status | `DONE_WITH_CONCERNS` (full Cargo skipped by parent; shared-core Cargo SIGKILL on 4 GiB host) |
| Brief | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-brief.md` |
| Report | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md` |
| Review package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-package.md` |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`unsafe_ffi` + `concurrency_lifecycle`; soft total 4); policy `b2d_task_risk_v1` |
| Parent rule | **SKIP all full cargo test** — package/source/CTest/ABI evidence only |

This review is independent of the implementer and of any Codex reviewer thread.
Findings are from commit contents, brief/spec cross-check, static source audit of
ABI/runtime/model/commands, and independent host re-runs of contracts CTest,
header syntax, layout probe, and symbol checks.

## Overall Assessment

Task 3 delivers ABI v1 lifecycle and request surfaces: stable errors `0..9`,
full 160-byte `CodegEuiFrame`, UI-thread affinity, panic containment, bounded
256 admission + completion reservation, exactly-once terminalization with
stale/cancel marking, immutable `OwnedFrame` retention with atomic completion
drain on successful poll, two-phase shutdown (`begin_shutdown` → stopping polls
→ observable `shutdown_ready` → final join/free), opt-in C test hook, and a C++
value-copy boundary.

Hard-trigger surfaces (FFI pointer lifetimes, concurrent admission vs worker
exit, shutdown drain observability) check out under independent review. No
Critical or Important source defect that blocks Task 4 was found.

**Verdict: `approve_with_minors`**

Minors are residual host Cargo evidence debt, a weaker C++ `UiSnapshot` live-ABI
coverage shape than the brief sketch, and a non-blocking join-handle style note.
None require a code change before continuing the task sequence.

## Spec Compliance Matrix

| Requirement | Result | Notes |
| --- | --- | --- |
| Stable errors `OK…NOT_READY` = `0..9` | Pass | Rust + C header + layout CTest |
| Lifecycle enum + order; re-init after stopped | Pass | `BridgeSlot` + contract tests; isolation helpers drain-poll first |
| UI-thread-only (except `api_version`) | Pass | `ensure_ui_thread`; wrong-thread poll returns `WRONG_THREAD` |
| Panic never unwinds across ABI | Pass | `ffi_guard` → `CODEG_EUI_ERR_PANIC` + diagnostic strip |
| Complete frame layout 160 B; bools as `u8`; no Rust `bool` in ABI | Pass | Host C++ probe: size 160, `shutdown_ready` @ 98 |
| Slice / session / completion sizes & offsets match C | Pass | Rust `offset_of` tests + C `static_assert` + layout CTest |
| Input bounds path/message/settings JSON | Pass | Constants + reject null/UTF-8/too-large before accept |
| Path-like reject embedded NUL; messages allow embedded NUL | Pass | `enqueue_path` vs `enqueue_utf8`; path_nul contract |
| Queue 256 + completion capacity 256; reject before accept | Pass | permit then reserve; 257th → `QUEUE_FULL` |
| Monotonic non-zero request IDs | Pass | process-global `AtomicU64`; survives re-init |
| Exactly one terminal completion per accepted ID | Pass | ledger `assert!` on double terminalize; cancel_all path |
| Stale when selection_epoch differs (not for cancelled) | Pass | `terminalize` marks Stale; cancel preserves Cancelled |
| Successful poll drains ready completions into retained frame | Pass | `commit_ready` after `OwnedFrame::new`; failed poll leaves prior |
| Frame pointers valid until next successful poll or final free | Pass | `last_frame` owns bytes; enqueue does not touch it |
| Two-phase shutdown; `shutdown_ready` only after successful out copy | Pass | latch after `out.write`; final needs Stopping + observed |
| Final shutdown rejects with `NOT_READY` until ready frame seen | Pass | C++ drain + Rust shutdown contracts |
| Cancelled in-flight work observed before free | Pass | Independent CTest `codeg_eui_shutdown_drain` Pass |
| `ffi-test-hooks` opt-in; not default; gated C declaration | Pass | Cargo features; header `#if CODEG_EUI_TEST_HOOKS` |
| C++ deep-copy; reject `(null,len>0)`; accept `(null,0)` | Pass | `ui_snapshot.h` + CTest |
| Product isolation (no root feature on `codeg`) | Pass | `default-features = false`; not in `src-tauri/Cargo.toml` |
| Design SHA-256 + EUI-NEO gitlink unchanged | Pass | `b3446ec3…d2bdef`; gitlink `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| Commit stages sources only (no build/archive) | Pass | 19 paths; no `target/` / `build-*` / `.a` |

### Justified deviations from brief snippets (not defects)

1. **Worker error/panic/stale/duplicate cases live as unit tests** in
   `model.rs` / `runtime.rs` rather than only `bridge_contract` — still
   automated and focused.
2. **C++ `UiSnapshot` uses fabricated frames** instead of the brief’s
   `frameWithAssistant` + live `advanceToFrameBAndShutdown` story; deep-copy
   ownership is still proven, and live ABI frame lifetime is covered in Rust.
3. **Operations enqueue with stub worker results** (“not implemented in Task 3”)
   — correct M0/M1 bridge scope; real ACP/session work is later tasks.
4. **`enqueue_blocked_for_test` always available on the Rust rlib** for
   `shutdown_contract`; only the C `codeg_eui_test_*` export is feature-gated.
5. **Data-root isolation / smoke helpers** updated to drain-until-ready so
   Task 2 contracts stay valid under async shutdown.

## Independent Verification (this host)

Host: Linux, `MemTotal` ≈ 3.8 GiB, no swap.
`HEAD == b55f20dd…`; tree matches the review package.

| Check | Result |
| --- | --- |
| Commit message / ancestry vs package | Match (`feat(eui): implement async bridge lifecycle`; BASE `be8b41cf`) |
| Design SHA-256 | `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| EUI-NEO gitlink | `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| C11 header syntax `-Wall -Wextra -Wpedantic -Werror` | **Pass** |
| C++ `ui_snapshot.h` compile | **Pass** |
| Host layout probe (sizes/offsets) | **Pass** (frame 160; `shutdown_ready` 98) |
| `assert_ctest_registered` (4 names) on `build-contract` | **Pass** |
| `ctest` full contracts dir | **4/4 Pass** (harness, abi_layout, ui_snapshot, shutdown_drain) |
| `ctest` `build-abi-link` shutdown_drain | **Pass** |
| Drain binary exports public ABI + `codeg_eui_test_enqueue_blocked` | **Pass** |
| Layout test binary has no `codeg_eui_test_*` | **Pass** |
| Static audit: admission mutex vs worker-exit cancel | **Pass** |
| Static audit: completion reserve/terminalize/commit invariants | **Pass** |
| Static audit: poll latches `shutdown_ready_observed` after `*out` write | **Pass** |
| Full / package Cargo tests | **Skipped** (parent rule); not re-attempted beyond authorized skip |

Producer-claimed focused Rust green counts (bridge/shutdown/smoke/unit) were not
re-executed here because they require linking the shared `codeg` crate via
Cargo; residual host memory risk is unchanged from Tasks 1–2.

## High-Risk Focus

### Unsafe FFI / ABI

- Repr(C) structs mirror the C header field-for-field; empty slices/arrays use
  null + zero length; non-empty slices reject null on the C++ copy boundary.
- `OwnedFrame` builds C views only after owning `Vec`s exist; raw pointers
  reference that frame’s heap only. `unsafe impl Send` is required for
  `Mutex<BridgeSlot>` and is justified by exclusive ownership of those heaps.
- Successful poll copies only the top-level `CodegEuiFrame` into `*out` after
  installing `last_frame`; failed polls (null out, wrong thread, bad lifecycle)
  never call `build_frame` / never commit completions.
- `ffi_guard` contains panics on every mutating export; `api_version` remains
  the unrestricted call.

### Concurrency / lifecycle

```
uninitialized → starting → running → stopping → stopped
```

- Admission: `try_reserve` → allocate ID → `ledger.reserve` → `permit.send`.
  Capacity failure rejects before acceptance; dropped permits restore channel
  slots.
- Completion accounting: `reserved` covers accepted + ready until poll
  `commit_ready`; the 256 bound cannot overflow after acceptance.
- Worker terminalizes on join (ok/error/panic→error). Shutdown aborts tasks,
  discards intermediate join payloads, then `cancel_all` so every still-accepted
  ID becomes exactly one `Cancelled` completion (including blocked hooks).
- `WorkerExitGuard` takes the admission mutex before `cancel_all` + `quiesced`,
  so an unexpected worker exit cannot race a concurrent reserve/send.
- `begin_shutdown` is UI-thread + Running only; closes command admission and
  signals the worker. Enqueue is Running-only → rejects in Stopping.
- `shutdown_ready` requires Stopping ∧ worker quiesced ∧ `accepted` empty; the
  same successful frame that first satisfies this also transfers remaining ready
  completions. Final `shutdown` requires `shutdown_ready_observed` after a
  successful poll copy.

### Immutable frames / completion transfer

- Model mutations and enqueue never write `last_frame`.
- `build_frame` snapshots ready completions, constructs `OwnedFrame`, then
  commits the ready queue — transfer is atomic w.r.t. the model lock.
- Frame contract tests keep prior completion bytes readable across later
  enqueue and failed wrong-thread poll; drained IDs are absent from the next
  frame.

## Findings

### Critical

None.

### Important

None.

### Minor

1. **Residual dependency-complete Cargo verification on ≤4 GiB hosts**  
   Parent rule forbids full Cargo suite. Producer already reports shared-core
   `rustc` SIGKILL under low-memory config. Independent review therefore trusts
   CTest/ABI/source evidence for this gate. Re-run focused
   `bridge_contract`, `shutdown_contract`, `abi_smoke`, and crate check on a
   higher-memory host before treating the Cargo-orchestrated path as CI-green.
   **No source change required for Task 3 acceptance.**

2. **C++ `UiSnapshot` contract is pure deep-copy, not live ABI lifetime**  
   Brief Step 16 sketched copy of frame A, advance to frame B, shutdown, then
   read copied strings. Shipped `ui_snapshot_test.cpp` fabricates a frame,
   mutates/clears local backing strings, and asserts `copy_frame` ownership —
   correct for the C++ boundary, weaker for end-to-end poll lifetime. Live ABI
   retention/transfer is covered by Rust `frame_bytes_survive_…` and
   shutdown contracts. Optional follow-up: one ABI-linked C++ test that copies
   after a real `codeg_eui_poll`. Not blocking.

3. **`RuntimeOwner::join` drops the worker `JoinHandle` without awaiting**  
   Final free does `drop(worker)` then `bootstrap.shutdown()` (runtime drop).
   This is safe only because final shutdown is gated on `quiesced`, which the
   worker exit guard stores after the drain body (including `disconnect_all`)
   finishes. Prefer an explicit finished-handle wait for clarity; no observable
   contract break found under the current latch.

## Non-Findings / Notes

- Shutdown reclassifying in-flight (even already-finished-but-unjoined) work as
  `Cancelled` is consistent with drain semantics and the blocked-request tests.
- Operation handlers intentionally return “not implemented in Task 3” errors;
  enqueue/completion machinery is what this task owns.
- `data_root_isolation` / unit `complete_shutdown` helpers correctly wait for
  async `shutdown_ready` rather than assuming the first stopping poll is ready.
- Generated Cargo/CMake outputs remain unstaged; package file list matches
  `git show --stat` on `b55f20dd`.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 3
reviewed_commit: b55f20ddb97706ebd78126e5ffd5ef4cb249ab57
reviewed_task_id: e53d2f15-9667-4dc8-94d0-ff366f390e36
continue_sequence: yes
code_changes_required: no
residual: re-run focused Cargo bridge/shutdown/smoke on higher-memory host; optional ABI-linked C++ frame lifetime test; optional explicit worker JoinHandle wait in final join
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":3,"summary":"Task 3 OK: ABI v1 lifecycle, 256 admission/completions, immutable frames, two-phase shutdown drain, C++ deep-copy. Independent 4/4 CTest + ABI probe pass. Minors: host Cargo residual; UiSnapshot pure-copy scope; join drops JoinHandle.","reviewed_task_id":"e53d2f15-9667-4dc8-94d0-ff366f390e36","artifact_digest":"b55f20ddb97706ebd78126e5ffd5ef4cb249ab57","concerns":["Dependency-complete Cargo bridge/shutdown/smoke not re-run on this 4GiB host (parent SKIP + historical SIGKILL).","C++ UiSnapshot proves deep-copy only; live poll lifetime is Rust-covered.","RuntimeOwner::join drops JoinHandle without await; gated by quiesced latch."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-grok-report.md"}
-->
