# Task 1 Implementer Report

## Identity

- Work unit: `task|1|implementer|codex|none`
- Requested baseline: `bb24a884`
- Dispatch ledger HEAD at start: `190c1e141de84460c01130b4da42713a14362759`
- Producer commit: `017954713566ddcbfd274f099055ddce022e2d01`
- Scope: Task 1 only; Task 2 was not started.

## Implementation

- Added the fixed writable protocol identity:
  `CURRENT_COMPLETION_PROTOCOL_VERSION = 2` and
  `current_completion_protocol_mode() = V2Enforce`.
- Added `require_v2_mutation(version, mode)` with exact pair semantics:
  `(2, v2_enforce)` succeeds, every version-1 pair is read-only, and every
  other pair is unsupported. Both rejection classes are non-retryable.
- Added the typed removed-configuration error for the Task 2 startup preflight.
- Added stable `AppErrorCode` and `AcpError` variants for read-only legacy,
  unsupported protocol, instruction binding failure, and removed
  configuration, with structural conversions and snake_case serialization.
- Mapped read-only, unsupported, and instruction-binding errors to HTTP 409;
  removed configuration maps to HTTP 400.
- Extended the listener's exhaustive `WorkflowStoreError` mapping so the new
  errors retain their stable codes at the MCP boundary. Rollout and legacy
  restart definitions remain available for later planned tasks.

## TDD Evidence

RED was observed with the new tests present before production implementation.
The focused command failed on missing `require_v2_mutation` and missing stable
`AppErrorCode` variants. An initial infrastructure-only build-script failure
for absent ignored `out/` was corrected, then the intended compile failure was
captured before GREEN implementation.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils require_v2_mutation`
  - Pass: 1 selected, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils stable_protocol_error_codes`
  - Pass: 1 selected, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils stable_completion_protocol`
  - Pass: 2 selected, 0 failed.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass.
- `git diff --cached --check`
  - Pass before commit.

Cargo emitted the existing local packaging warning that the ignored
`codeg-mcp` sidecar is a zero-byte placeholder. It did not affect compilation
or tests and is not part of the producer diff.

## Scope Notes

The plan's Step 6 staging list did not name `listener.rs`, but adding variants
to `WorkflowStoreError` makes its exhaustive MCP mapping a required compile
fan-out. The listener change is limited to the two new stable codes.

## Conclusion

done

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Defined the fixed v2 completion identity, exact non-retryable mutation guard, and stable typed ACP/App/HTTP error mappings with negative tests.","commits":[{"sha":"017954713566ddcbfd274f099055ddce022e2d01","subject":"feat: define v2-only completion protocol guard"}],"tests":{"status":"pass","passed":4,"failed":0,"summary":"Guard matrix, stable public-code integration, App/HTTP unit mappings, and desktop cargo check passed."},"concerns":[],"report_file":".superpowers/sdd/task-1-report.md"}
-->
