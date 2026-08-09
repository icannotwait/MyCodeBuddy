# Task 2 High-Risk Review (Codex)

## Verdict

**`request_changes`**

The initial root pin, ABI root precedence, bootstrap ordering, and dormant EUI
`AppState` assembly match the Task 2 contract on source inspection. One
Important lifecycle defect remains: legal same-root re-initialization mutates
the process environment after EUI's process-retained logging worker has
started. This violates the explicit startup-only pinning boundary and must be
fixed before approval.

## Findings

### Critical

None.

### Important

#### I1. Same-root re-init repeats environment mutation after a worker thread survives shutdown

`pin_eui_data_root` verifies the immutable path and then unconditionally calls
`remove_var("CODEG_HOME")` and `set_var("CODEG_DATA_DIR", ...)` on every
successful call (`src-tauri/codeg-eui-core/src/data_root.rs:66-77`). That is
correct for the first pin, but the same function is called again by every
legal re-init (`src-tauri/codeg-eui-core/src/bootstrap.rs:109-115`).

The first initialization calls `init_eui` after pinning
(`src-tauri/codeg-eui-core/src/bootstrap.rs:61-65`). `init_eui` stores an
`Arc<WorkerGuard>` in the process-global `EUI_LOG_GUARD`
(`src-tauri/src/logging/init.rs:43, 251-254`). `tracing_appender::non_blocking`
creates a dedicated logging thread, and `WorkerGuard` is what signals and joins
that thread on drop. EUI shutdown drops only the Tokio runtime
(`src-tauri/codeg-eui-core/src/bootstrap.rs:84-88`); the static guard keeps the
logging worker alive.

The resulting required lifecycle is therefore:

`first pin -> logging worker starts -> ABI shutdown -> same-root init -> env mutation`

This contradicts both the global requirement to resolve/pin before worker
threads and the function's own startup-only safety comment at
`data_root.rs:73-74`. On Unix, process-environment writes are the class of
operations made unsafe in Rust 2024 because they require excluding concurrent
environment access by other threads. The crate's Rust 2021 edition does not
make the calls syntactically unsafe, but it does not remove that platform
constraint.

Required change: make an equal already-pinned root return success without
touching process environment; perform `CODEG_HOME` removal and `CODEG_DATA_DIR`
overwrite only as part of the first successful pin, before logging/Tokio
workers exist. Keep the divergent-root rejection and add a regression seam
that proves same-root ABI restart does not execute a second environment-write
phase. The existing restart assertion at
`src-tauri/codeg-eui-core/tests/data_root_isolation.rs:166` proves only the
return status and cannot detect this violation.

### Minor

#### M1. Dependency-complete Cargo verification remains host-limited

All four brief-mandated Cargo test/check commands were killed by signal 9 while
compiling the existing shared `codeg` crate on the 3.8 GiB/no-swap host. The
producer accurately discloses that no Rust diagnostic or assertion failure was
emitted, and the focused stub-boundary probes provide useful evidence, but
they do not prove the real shared-core type/dependency boundary. Rerun the four
exact commands on a higher-memory host after I1 is fixed.

#### M2. The process-global EUI log guard cannot flush through normal ABI shutdown

Because `EUI_LOG_GUARD` permanently owns an `Arc<WorkerGuard>`, dropping the
bootstrap's clone during a successful shutdown never drops the real guard.
Rust statics are not destructed when `main` returns, so the appender's shutdown
and flush handshake is skipped on clean EUI exit as well as abrupt exit. This
is broader than the producer report's abrupt-exit concern. It is not a Task 2
data-root violation, but the residual log-tail loss should be corrected or
explicitly accepted with an actual process-finalization mechanism.

#### M3. Dormancy flags are assertions about defaults, not observations of starts

`EuiBootstrap::new` always installs `StartedServices::default()`
(`src-tauri/codeg-eui-core/src/bootstrap.rs:91-97`), and
`bootstrap_profile.rs:26-43` only asserts those booleans remain false. An
accidental future call to an excluded start function would not update the
flags, so most of the test would still pass. The current `new_eui` source was
audited and contains none of the excluded start calls; this is therefore a
coverage weakness rather than a present service-start defect. Prefer an
observable start hook/counter or otherwise bind these flags to actual startup
operations.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 Reviewer 1 (Codex) |
| Work unit | `task\|2\|reviewer\|codex\|none` |
| Reviewed task ID | `eb250a5f-e61e-441f-af46-f5130a615ed8` |
| Base | `6fcfd6999d69d16d829b0410c1e828069aec0628` |
| Producer commit / artifact digest | `8bac8d78bcdf7f189304fa714d068e2d73ddb541` |
| Policy | `b2d_task_risk_v1` (`high`: security trust boundary and concurrency lifecycle) |

The producer commit exists at `HEAD`, its sole parent is the stated base, and
its ten changed paths match the supplied review package. The worktree was
clean before review. The approved design digest was independently recomputed
as `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Specification Audit

- **Initial pin and isolation:** resolver precedence ignores ambient
  `CODEG_DATA_DIR`, filters empty EUI/XDG/home candidates, lexical-normalizes
  relative roots against the captured working directory, rejects embedded
  NUL before committing the pin, removes `CODEG_HOME`, and overwrites
  `CODEG_DATA_DIR` before logging or Tokio on the first init.
- **ABI authority:** a non-empty pointer/length argument is UTF-8 checked,
  NUL checked, bounded at 32,768 bytes, and made authoritative over
  `CODEG_EUI_DATA_DIR`. Invalid arguments roll `starting` back to `stopped`;
  same-root restart and divergent-root rejection are covered in a child
  process.
- **Filesystem isolation:** child-process tests place SQLite and logs under the
  EUI/argument root and assert no database/log writes under ambient main-app
  roots.
- **Bootstrap order:** production startup performs resolve/pin, directory
  creation, logging, runtime creation, database initialization, persisted log
  level application, then `AppState::new_eui`.
- **EUI AppState:** `EventEmitter::WebOnly`, an empty `ConnectionManager`,
  persisted internal-session loading, disabled document translation, and the
  complete shared field map are present. No web server, pet mapper, updater,
  chat, automation, auto-title, translation, reference sweeper, delegation
  listener/supervisor, or completion outbox dispatcher start call appears in
  the EUI profile.
- **Known constructor side effect:** the brief-required shared delegation
  constructor starts recovery-authorization pruning. That service is not in
  the design's explicit excluded list, but it should remain visible as a
  conscious EUI background task.
- **Build isolation:** the standalone manifest retains `staticlib`/`rlib`,
  depends on `codeg` with default features disabled, and its normal dependency
  graph contains no Tauri package.

## Independent Verification

Passed locally:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- `cargo metadata --manifest-path src-tauri/codeg-eui-core/Cargo.toml --no-deps`
- Normal dependency-tree audit; inverse `tauri` query reports no matching
  package
- `git diff --check 6fcfd699..8bac8d78`
- Commit parent, changed-path, approved-design digest, and EUI submodule pin
  checks

Not rerun by this reviewer:

- The four dependency-complete Cargo test/check commands. This host has
  3.8 GiB RAM, no swap, and no completed `libcodeg` metadata in the 8.7 GiB
  partial EUI target cache; another run would repeat the producer's documented
  shared-crate SIGKILL condition rather than add evidence.

The standalone `Cargo.lock` regenerated by metadata/tree inspection was
removed; no generated artifact remains in the worktree.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","summary":"Task 2 matches the initial isolation/profile brief, but legal same-root re-init repeats process-environment mutation after the retained logging worker has started.","reviewed_task_id":"eb250a5f-e61e-441f-af46-f5130a615ed8","artifact_digest":"8bac8d78bcdf7f189304fa714d068e2d73ddb541","concerns":["Same-root ABI re-init must not repeat CODEG_HOME/CODEG_DATA_DIR mutation after the process-global logging worker exists.","The four real shared-core Cargo gates remain unverified on the 3.8 GiB/no-swap host.","The process-retained log guard cannot flush through ordinary ABI shutdown or clean main return."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-codex-report.md"}
-->

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":3,"summary":"Task 2 requires a same-root re-init fix: do not mutate process env after the retained logging worker starts.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-codex-report.md"}
-->
