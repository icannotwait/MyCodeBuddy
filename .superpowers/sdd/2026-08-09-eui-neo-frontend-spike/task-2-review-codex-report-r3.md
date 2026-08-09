# Task 2 Fix Re-review (Codex R3)

## Verdict

**`approve`**

N1 is addressed. Root publication and the first environment-write phase now
share the `OnceLock` initialization boundary, so equal or divergent callers
cannot observe or return from the pin until the trust-boundary transition is
complete. No new defect was found in the scoped one-file fix delta.

## Finding Disposition

| Finding | Disposition | Result |
| --- | --- | --- |
| N1: root published before first environment-write phase completes | **ADDRESSED** | Environment writes execute inside `get_or_init`; publication and competing-caller release occur only after the closure returns. |

## N1 Analysis

`pin_eui_data_root` now places `CODEG_HOME` removal, `CODEG_DATA_DIR`
assignment, the test write counter, and the pinned path value inside
`PINNED_EUI_DATA_ROOT.get_or_init`
(`src-tauri/codeg-eui-core/src/data_root.rs:77-98`). `OnceLock` does not publish
the value until its initialization closure completes. A concurrent caller to
the same `get_or_init` waits for that completion and only then reaches
`roots_match`.

This closes both branches identified by N1:

- An equal caller cannot return `Ok(())` while ambient root variables remain
  effective.
- A divergent caller cannot evaluate `AlreadyPinned` against a partially
  initialized pin; it waits, then compares against the completed first root.

Later serial equal re-pins do not rerun the closure, preserving the Round 1 I1
fix. Divergent roots still return `DataRootError::AlreadyPinned`, and embedded
NUL is still rejected before attempting initialization.

## Regression Audit

The updated unit regression pauses the winning initializer inside the
`OnceLock` closure before the environment writes
(`src-tauri/codeg-eui-core/src/data_root.rs:162-216`). While paused, it checks
that `PINNED_EUI_DATA_ROOT.get()` is `None` and launches an equal pin caller.
After releasing the initializer it verifies:

- the root was not published early;
- the equal caller did not return early;
- both callers returned `Ok(())`;
- exactly one environment-write phase occurred.

The same test then performs two full ABI init/shutdown cycles at the pinned
root and confirms the write count remains one
(`src-tauri/codeg-eui-core/src/data_root.rs:218-237`). This preserves direct
coverage of the original I1 behavior while adding the N1 concurrency case.
The reported RED failure against Round 1 and GREEN `1/1` result are coherent
with the implementation change.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 Reviewer 1 (Codex), scoped N1 re-review |
| Work unit | `task\|2\|reviewer\|codex\|none` |
| Reviewed task ID | `dc04d65a-a464-4e31-9c57-497a4792a0e6` |
| Fix base | `1e92ed75da0702bc628b5f42e0af7fe5d48c7814` |
| Producer artifact / commit | `be8b41cf8545470694e2d0b490ec5b6f6cb1a227` |
| Scope | Prior open N1 plus breakage introduced by the fix diff |

The producer commit exists at `HEAD`, its sole parent is the stated fix base,
and the package accurately limits the change to
`src-tauri/codeg-eui-core/src/data_root.rs`. The worktree was clean before the
review artifact was written.

## Independent Verification

Passed locally:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- `git diff --check 1e92ed75..be8b41cf`
- Commit object, parent, subject, one-file scope, and clean-worktree checks
- Source-level trace of first-pin, equal-pin, divergent-pin, and serial ABI
  restart behavior

Not rerun:

- Dependency-complete Cargo tests/checks. The producer's focused Cargo attempt
  was again SIGKILLed while compiling the existing shared `codeg` crate on the
  3.8 GiB/no-swap host. Producer evidence reports the actual Task 2 modules
  compiled with `rustc -D warnings` and the focused atomic-pin, isolation, and
  ABI probes passing 9/9. This scoped approval does not claim completion of the
  host-limited Cargo gate.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 2 N1 is addressed: OnceLock publishes the root only after first-pin environment writes complete, and no new breakage was found in the scoped fix delta.","reviewed_task_id":"dc04d65a-a464-4e31-9c57-497a4792a0e6","artifact_digest":"be8b41cf8545470694e2d0b490ec5b6f6cb1a227","concerns":["Dependency-complete Cargo verification remains host-limited and is not claimed by this scoped approval."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-codex-report-r3.md"}
-->
