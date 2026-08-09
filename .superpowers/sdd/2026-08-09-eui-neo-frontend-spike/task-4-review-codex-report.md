# Task 4 High-Risk Review (Codex)

## Verdict

**`request_changes`**

The production facade and async worker routing match the approved Task 4
shape: only Codex/Grok reach the ACP cores, settings writes stay behind the
existing helpers, and accepted operations use the Task 3 completion ledger.
The mandatory dependency-complete settings contract cannot compile, however,
because it calls the public shutdown function without importing it.

## Findings

### Critical

None.

### Important

#### I1. The mandatory settings contract does not compile

`src-tauri/codeg-eui-core/tests/settings_contract.rs:5-9` explicitly imports
the EUI functions used by the test but omits `codeg_eui_shutdown`. The helper
at `settings_contract.rs:242-247` then calls `codeg_eui_shutdown()` as an
unqualified name. There is no local definition or prelude export that can
resolve that call; the public function exists only as
`codeg_eui_core::codeg_eui_shutdown` (`src/abi.rs:278`). Rust will therefore
reject this integration-test target with `E0425` once dependency compilation
reaches the test crate.

This is the Task 4 brief's required dependency-complete native-file
round-trip target, and the producer explicitly reports that it was never
compiled because the host killed the shared `codeg` build first. The
shape-compatible probes cannot catch a name-resolution error in a separate
integration-test crate. As committed, the documented Task 4 verification
command cannot pass on any host.

Required change: import `codeg_eui_shutdown` in `settings_contract.rs`, then
compile and run the focused `settings_contract` target on a capable host.

### Minor

#### N1. The focused contract omits three required boundary checks

The Task 4 verification criteria require oversized settings JSON rejection,
unsupported-agent fail-closed behavior, and the real probe path. The committed
tests cover malformed outer fields, get/set polling, native Codex/Grok files,
and an injected slow probe, but no test submits
`CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1`, calls a public facade/ABI operation
with an unsupported agent, or invokes `codeg_eui_probe_agent` through the real
facade. The existing unit tests only exercise the private guard in isolation,
and the slow-probe test uses a `CoreOps` stub.

Add focused boundary cases to `settings_contract.rs` (or an equally narrow
contract target) so these acceptance claims remain protected independently of
source inspection.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 4 high-risk Reviewer 1 (Codex) |
| Work unit | `task\|4\|reviewer\|codex\|none` |
| Reviewed task ID | `48d79f89-e4ef-4240-8092-f98bc9306cf2` |
| Base | `66f7cff1ee5b02773f19f938482c3a112792ecb0` |
| Producer artifact / commit | `89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf` |
| Policy | `b2d_task_risk_v1` (`high`: security trust boundary and public compatibility) |

The producer commit is `HEAD`, its sole parent is the stated base, and its
eight-path diff matches the review package. The worktree was clean before this
review report was written. The approved design independently recomputes to
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Specification Audit

- Exact wire parsing accepts only `"codex"` and `"grok"`; typed public facade
  calls run the same support guard before DB/config access.
- Cross-agent fields are rejected before the current settings row is read.
- Reads project the narrow DTO from `AcpAgentInfo`; writes call only
  `acp_update_agent_env_and_refresh` and
  `acp_update_agent_config_and_refresh`; probes call
  `acp_preflight_core(agent, Some(true), db)`.
- ABI settings input is UTF-8/bounds checked, outer JSON is deserialized with
  `deny_unknown_fields` before acceptance, and the worker serializes successful
  facade DTOs into completion payloads.
- Get/set/probe work is spawned through the existing worker `JoinSet`; normal
  result, error, panic, stale, and shutdown paths retain the Task 3 exactly-once
  completion behavior.
- No new direct persistence implementation or secret-bearing trace statement
  appears in the scoped production delta.
- The integration target defect and missing acceptance cases remain as I1/N1.

## Independent Verification

Passed locally:

- Commit parent, package scope, clean baseline, and approved-design digest
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `git diff --check` for the producer range
- C11 and C++17 public-header syntax with
  `-Wall -Wextra -Wpedantic -Werror`
- Fresh contracts-only CMake configure/build and CTest: **3/3 passed**

The focused Cargo integration target was not rerun: the parent instruction
forbids full Cargo tests, and the producer already demonstrated that compiling
the shared dependency graph exceeds this host's memory. I1 is independent of
that host limit and follows directly from Rust name resolution in the committed
test crate.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","critical":0,"important":1,"minor":1,"summary":"Task 4 production facade and async routing match the approved shape, but the mandatory settings_contract target cannot compile because codeg_eui_shutdown is called without being imported.","reviewed_task_id":"48d79f89-e4ef-4240-8092-f98bc9306cf2","artifact_digest":"89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf","concerns":["Import codeg_eui_shutdown and compile/run the focused settings_contract target on a capable host.","Add focused coverage for oversized settings JSON, unsupported public-agent calls, and the real probe facade/ABI path."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-codex-report.md"}
-->
