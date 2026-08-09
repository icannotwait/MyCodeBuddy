# Task 2 Fix Re-review (Codex R2)

## Verdict

**`request_changes`**

The original sequential same-root restart defect I1 is addressed. The fix
introduces a new initialization-publication race in the process pin: the root
becomes observable as pinned before the winning caller completes the
environment-write phase. That new trust-boundary breakage must be corrected
before Task 2 can be approved.

## Finding Dispositions

| Finding | Disposition | Result |
| --- | --- | --- |
| I1: same-root re-init repeats environment writes after logging starts | **ADDRESSED** | Equal re-pins now return without `remove_var`/`set_var`; the full ABI restart regression observes one write phase. |
| N1: fix publishes the pin before the first environment-write phase completes | **NOT ADDRESSED** | A concurrent equal caller can return success while `CODEG_HOME`/`CODEG_DATA_DIR` are still ambient. |

## Prior Finding

### I1 - ADDRESSED

`pin_eui_data_root` now returns immediately when
`verify_or_set_process_pin` reports an equal existing root
(`src-tauri/codeg-eui-core/src/data_root.rs:69-84`). Consequently, the legal
serial lifecycle

`first init -> logging worker -> shutdown -> same-root init`

does not execute a second environment-write phase. Divergent roots still flow
through `roots_match` and return `DataRootError::AlreadyPinned`.

The new unit regression performs two complete ABI init/shutdown cycles and
checks that `ENVIRONMENT_WRITE_PHASES` remains at one after the second init
(`src-tauri/codeg-eui-core/src/data_root.rs:163-187`). This directly covers the
test seam requested by I1. The reported RED (`left: 2, right: 1`) and GREEN
(`1/1`) evidence are coherent with the source delta.

## New Breakage

### N1 - NOT ADDRESSED (Important): pin publication precedes environment pin completion

`verify_or_set_process_pin` calls `PINNED_EUI_DATA_ROOT.set(...)` and returns
`true` to the winner (`src-tauri/codeg-eui-core/src/data_root.rs:107-124`).
Only after that function returns does the winner remove `CODEG_HOME` and set
`CODEG_DATA_DIR` (`data_root.rs:78-83`). `OnceLock::set` publishes the value
before those environment operations occur.

An equal concurrent caller can therefore execute this sequence:

1. Caller A publishes the root at line 113 and is descheduled before line 80.
2. Caller B observes the published equal root at lines 108-110.
3. Caller B returns `Ok(())` from `pin_eui_data_root` at lines 74-75.
4. Caller B starts an environment-reading helper or bootstrap logging while
   ambient main-app `CODEG_HOME`/`CODEG_DATA_DIR` are still active.

That violates the process-once pin contract and the invariant that the one EUI
root is effective before logging, credentials, or workers. The public C ABI is
UI-thread-only, but `pin_eui_data_root` is itself a separately exported safe
Rust interface and has no enforced single-caller precondition. The use of a
thread-safe `OnceLock` must not expose a partially completed trust-boundary
transition.

Required change: make path publication and the first environment-write phase
one initialization operation. For example, perform the writes inside the
`OnceLock` initialization closure so equal callers wait until initialization
is complete before observing the pinned value, or use equivalent
synchronization. Preserve the serial I1 behavior: later equal re-pins must
remain read-only and divergent roots must remain errors. Add a deterministic
concurrency regression that pauses the first initializer before completion and
proves an equal caller cannot return early.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 Reviewer 1 (Codex), scoped fix re-review |
| Work unit | `task\|2\|reviewer\|codex\|none` |
| Reviewed task ID | `315c9c36-091c-4146-95de-0f071d43b7cf` |
| Fix base | `8bac8d78bcdf7f189304fa714d068e2d73ddb541` |
| Producer artifact / commit | `1e92ed75da0702bc628b5f42e0af7fe5d48c7814` |
| Scope | Prior I1 only, plus breakage introduced by the fix diff |

The producer commit exists at `HEAD`, its sole parent is the stated fix base,
and the fix changes only
`src-tauri/codeg-eui-core/src/data_root.rs` as declared by the package. Prior
minor findings M1-M3 were not reopened or re-verdictized in this scoped pass.

## Independent Verification

Passed locally:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- `git diff --check 8bac8d78..1e92ed75`
- Commit object, parent, subject, one-file scope, and clean-worktree checks
- Source-level trace of the ABI restart and first-pin/equal-pin branches

Not rerun:

- Dependency-complete Cargo tests/checks. The producer's focused Cargo rerun
  again reached the existing shared `codeg` crate and was SIGKILLed on this
  3.8 GiB/no-swap host before the EUI unit test ran. The R2 verdict is based on
  the fix source and its reported direct `rustc -D warnings` RED/GREEN probe;
  it does not claim real shared-core Cargo completion.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"Task 2 I1 is addressed for serial same-root restart, but the fix publishes the OnceLock root before completing first-pin environment writes, allowing an equal concurrent caller to return early.","reviewed_task_id":"315c9c36-091c-4146-95de-0f071d43b7cf","artifact_digest":"1e92ed75da0702bc628b5f42e0af7fe5d48c7814","concerns":["Make root publication and the first CODEG_HOME/CODEG_DATA_DIR write phase atomic with respect to equal pin callers."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-codex-report-r2.md"}
-->
