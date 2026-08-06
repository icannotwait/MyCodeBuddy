# Rust Test Link-Time Design

## Status

Approved in conversation on 2026-08-06.

No implementation plan has been approved yet.

## Problem

The standard desktop command is currently:

```bash
cargo test --features test-utils
```

On Windows, a diagnostic `--no-run` build took 480.15 seconds before running
any test. During that build, Cargo invoked `rustc` for `codeg_lib` with all
three configured crate types:

```toml
crate-type = ["staticlib", "cdylib", "rlib"]
```

The resulting `codeg_lib.lib` was approximately 4.0 GiB. Relevant cached test
artifacts occupied approximately 6.74 GiB. The full command also prepares 20
independent integration-test binaries, compared with 10 on current upstream.

The local-only `tauri/test` and `tauri/devtools` feature differences do not add
packages to the resolved graph: both configurations resolve 843 packages.
They can create additional feature-specific cache variants, but they are not
the cause of the observed eight-minute link.

## Goals

- Stop producing mobile-oriented `staticlib` and `cdylib` outputs for this
  desktop/server-only repository.
- Provide a fast local unit-test command that does not prepare integration-test
  targets.
- Preserve the existing complete Rust regression suite in CI and as an
  explicit local command.
- Keep the change limited to the Cargo library output and contributor guidance.

## Non-Goals

- Supporting Android or iOS builds after this change.
- Removing, merging, or weakening any existing test.
- Changing the CI test matrix or its coverage.
- Splitting `test-utils`, `tauri/test`, or `tauri/devtools` into new features.
- Automatically deleting existing files under `src-tauri/target`.
- Optimizing test runtime after compilation and linking complete.

## Selected Approach

Change the library target to emit only a Rust library:

```toml
[lib]
name = "codeg_lib"
crate-type = ["rlib"]
```

`rlib` is sufficient for the desktop `codeg` binary, the standalone server,
`codeg-mcp`, unit tests, and integration tests. The removed output types are
needed by Tauri mobile consumers, which are explicitly outside this
repository's supported build matrix.

Document two desktop test levels in `AGENTS.md`:

```bash
# Fast local feedback: library unit tests only
cargo test --lib --features test-utils

# Complete desktop regression: unit, binary, and integration tests
cargo test --features test-utils
```

The existing CI command remains unchanged and continues to execute or compile
the complete suite on its current operating-system matrix. Contributors can
still run the full command before high-risk changes.

## Expected Effect

The manifest change prevents future test builds from linking the approximately
4.0 GiB Windows static library. The fast local command additionally skips the
20 integration-test targets and the normal library build they require.

Existing static-library artifacts may remain in `target` until an ordinary
Cargo cleanup. They are ignored build output and do not affect the new target
selection.

No exact speedup is guaranteed because link time depends on cache state,
hardware, antivirus scanning, and source size. Verification will report fresh
observed timings rather than extrapolating from the baseline.

## Compatibility And Risks

- Desktop Tauri builds continue to consume `codeg_lib` as an `rlib`.
- Server and MCP builds continue to consume the same Rust library target.
- Android and iOS library packaging is intentionally no longer supported.
- The fast command does not exercise integration tests. This is visible in its
  name and documentation; the full command and CI remain authoritative.
- Keeping the existing `test-utils` feature composition avoids changing the
  desktop IPC regression and streaming-performance harness in this work.

## Verification

After implementation:

1. Inspect `cargo metadata` and confirm the library target exposes only
   `rlib`.
2. Run `cargo test --locked --lib --features test-utils` and report its result
   and wall time.
3. Run `cargo test --locked --features test-utils --no-run` and report its
   result and wall time, confirming that the full target set still compiles.
4. Run `cargo check --locked` for the default desktop configuration.
5. Run `cargo check --locked --no-default-features --features server --bin
   codeg-server` for the standalone server configuration.
6. Confirm the tracked diff is limited to `src-tauri/Cargo.toml`, `AGENTS.md`,
   and the approved design/plan documents.

Because this is a manifest and documentation change with no new program
behavior, verification uses Cargo target metadata and real builds rather than
adding an application unit test.

## Alternatives Considered

### Split Tauri Test Features

Separating `tauri/test` and `tauri/devtools` could reduce feature-specific
cache churn. It does not reduce the resolved package count and would require
source `cfg` changes plus dedicated CI commands. It is not justified before
removing the measured static-library bottleneck.

### Gate Integration-Test Targets

Setting `autotests = false` and registering each integration test behind an
opt-in feature would make plain `cargo test` faster. It adds ongoing manifest
maintenance and risks silently omitting newly added test files. An explicit
`--lib` local command provides the same intentional fast path without changing
test discovery.

### Keep All Crate Types

This preserves hypothetical mobile support but retains the measured 4.0 GiB
static-library output. The user confirmed that Android and iOS builds are not
required, so this tradeoff is unnecessary.
