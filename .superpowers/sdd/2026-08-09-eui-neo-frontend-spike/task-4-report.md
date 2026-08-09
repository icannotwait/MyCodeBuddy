# Task 4 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 4 adds the narrow Grok/Codex settings facade and routes the existing EUI
settings/probe ABI operations through asynchronous Tokio workers. Settings
writes remain owned by the existing ACP persistence helpers, and accepted
requests use the Task 3 completion ledger for one terminal completion.

## Implementation

- Added public backend-aligned `EuiAgentSettings`, request-only
  `EuiAgentSettingsPatch`, `EuiAgentProbe`, and `EuiFacadeError` types.
- Restricted the wire vocabulary to exact `"codex"` and `"grok"` values.
  Typed and wire-level unsupported agents are rejected before DB or native
  config access.
- Projected settings from `AcpAgentInfo`, omitting fields owned by the other
  supported agent.
- Validated cross-agent patch fields before reading current settings.
- Delegated environment/provider writes only to
  `acp_update_agent_env_and_refresh` and native config writes only to
  `acp_update_agent_config_and_refresh`; the facade performs no direct file
  writes and adds no persistence schema.
- Delegated probes to `acp_preflight_core(agent, Some(true), db)` and returned
  launchability, installed version, and non-passing diagnostic messages.
- Shared `AppState` with workers through `Arc<AppState>` and added an injectable
  `CoreOps` boundary. Get, set, and probe work runs in spawned Tokio tasks and
  serializes result DTOs into completion JSON.
- Added pre-acceptance bounded JSON parsing with outer
  `deny_unknown_fields` for `codeg_eui_set_agent_settings`.
- Added a deterministic slow-probe worker test and an isolated child-process
  settings contract covering malformed patch rejection, polled completion,
  Codex/Grok DTO projection, and native `CODEX_HOME`/`GROK_HOME` files.
- Documented the asynchronous completion contract beside the public C
  declarations.

`commands.rs` and `model.rs` required no Task 4 edit: Task 3 already defined
the frozen operation discriminants, settings payload shape, completion
ledger, and JSON result storage used by this implementation.

## TDD Evidence

### RED

Two reversible mutations were run against the final focused probes:

- Removing the typed-agent pre-access guard made
  `unsupported_typed_agent_is_rejected_by_the_pre_access_guard` fail with the
  expected assertion (`0 passed, 1 failed`).
- Replacing the probe worker route with an error made
  `slow_probe_does_not_block_frame_build_and_completes_once` fail on the
  expected completion status (`0 passed, 1 failed`). The same mutation was
  also rejected by the `-D warnings` compile because `probe_agent` became dead
  code.

The malformed-patch contract also fixes the acceptance boundary: unknown
outer fields return `CODEG_EUI_ERR_INVALID_STATE` and leave the caller's
request ID unchanged.

### GREEN

After restoring the production paths:

- Actual `eui_facade.rs` compiled with `rustc -D warnings` against a
  shape-compatible ACP/AppState boundary; **4/4 facade unit tests passed**.
- Actual Task 4 `abi.rs` and `runtime.rs` compiled with `rustc -D warnings`
  against a shape-compatible facade/AppState boundary; **7/7 focused
  ABI/model/runtime tests passed**, including the slow-probe completion test.
- Contracts-only CMake/CTest: **3/3 passed** (harness, ABI layout, UI
  snapshot).

The shape-compatible probes validate the actual changed modules but do not
replace compiling or running them against the full shared `codeg` crate.

## Verification

Passed:

- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `cargo metadata --manifest-path src-tauri/codeg-eui-core/Cargo.toml --no-deps`
- Direct actual-source Rust probes with `-D warnings`: **11/11 passed**.
- Contracts-only CMake build and CTest: **3/3 passed**.
- Task 4 raw/nested JSON fixture parsing probe.
- `git diff --check`.
- Approved design SHA-256 matched
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- No standalone `src-tauri/codeg-eui-core/Cargo.lock` remains.

Per the parent instruction, **all full Cargo tests were skipped**. No full
package/workspace test, `cargo test --lib --features test-utils`, or broad
shared-codeg suite was run.

A focused dependency-complete standalone-crate check was attempted with the
repository's one-job/no-debug low-memory configuration. It reached the shared
`codeg` crate with no emitted Rust diagnostic, then the kernel killed `rustc`
with `SIGKILL` on the 3.8 GiB/no-swap host. The focused
`settings_contract` Cargo binary therefore could not be dependency-completely
compiled or run on this host. The generated standalone lockfile was removed.

## Files Changed

- `src-tauri/src/commands/eui_facade.rs`
- `src-tauri/src/commands/mod.rs`
- `src-tauri/codeg-eui-core/src/abi.rs`
- `src-tauri/codeg-eui-core/src/bootstrap.rs`
- `src-tauri/codeg-eui-core/src/runtime.rs`
- `src-tauri/codeg-eui-core/tests/settings_contract.rs`
- `codeg-eui/app/bridge/codeg_eui_bridge.h`

## Self-Review

- Unsupported wire agents are parsed before an `AppState` reference is used;
  typed facade callers pass the same guard before DB/config access.
- Cross-agent fields are rejected before the facade reads current settings.
- The facade contains no filesystem write calls and widens none of the
  existing ACP helper visibilities.
- Settings patch deserialization is request-only and rejects unknown outer
  fields before request acceptance; output DTOs are serialization-only.
- Get/set/probe work is spawned off the UI thread. Worker success, error, and
  panic paths all terminalize through the existing exactly-once ledger.
- Completion payloads are JSON bytes owned by the retained frame. Contract
  helpers copy ABI slices before another successful poll can invalidate them.
- No auth or environment value is added to tracing/logging. Errors contain
  operation diagnostics, not patch contents.
- Generated Cargo/CMake outputs and temporary shape probes are excluded from
  the implementation package.

## Concern

Dependency-complete Rust verification, including the isolated native-file
round-trip integration test, must be rerun on a host with more memory or usable
swap. This is a verification limitation, not a known Task 4 behavior defect.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the Grok/Codex settings facade and asynchronous get/set/probe bridge workers over existing ACP persistence and preflight helpers.","commits":[{"subject":"feat(eui): expose Grok and Codex settings facade"}],"tests":{"status":"partial","passed":14,"failed":0,"summary":"11 actual-source shape-compatible Rust probes and 3 contracts-only CTest cases pass; full Cargo tests were skipped by parent instruction and dependency-complete codeg checking was host-SIGKILLed."},"concerns":["Dependency-complete settings_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md"}
-->

## High-Gate Fix Round 1/5

Status: DONE_WITH_CONCERNS

The two independent reviewers found the same Important defect: the focused
`settings_contract` called `codeg_eui_shutdown` without importing it, so the
integration target could not resolve that name. The import is now present.

The cheap review minors were also covered in the same contract target:

- A settings body of `CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1` is rejected with
  `CODEG_EUI_ERR_TOO_LARGE` before acceptance and leaves the request ID
  unchanged.
- The public get-settings ABI accepts an unsupported wire request
  asynchronously, then returns exactly one error completion with no settings
  payload.
- The public probe ABI returns a JSON completion containing `launchable`,
  `installedVersion`, and `message` through the worker/facade route.

TDD and verification evidence:

- RED: direct `rustc --test` compilation of the committed contract failed with
  `E0425: cannot find function codeg_eui_shutdown in this scope` at
  `complete_shutdown`.
- GREEN: the complete `settings_contract.rs` compiled with `-D warnings`
  against the established shape-compatible Task 4 boundary.
- Five selected real-ABI contract cases passed: malformed, oversized,
  unsupported agent, probe completion, and get completion.
- Contracts-only CTest remained **3/3 passed**.
- Both Cargo format checks, `git diff --check`, approved-design digest, and
  standalone-lockfile absence passed.

Per parent instruction, **all full Cargo tests remain skipped**. The isolated
native-file round-trip still requires dependency-complete execution on a host
with more memory or usable swap; this fix round does not claim otherwise.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"Task 4 high-gate I1 fixed: settings_contract now imports codeg_eui_shutdown and covers oversized JSON, unsupported agents, and the public probe ABI.","commits":[{"subject":"fix(eui): compile settings bridge contract"}],"tests":{"status":"partial","passed":8,"failed":0,"summary":"5 selected settings ABI contract cases and 3 contracts-only CTest cases pass; the full Cargo suite remains skipped by parent instruction."},"concerns":["Dependency-complete native-file round-trip and shared-codeg verification still require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md"}
-->
