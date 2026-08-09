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
