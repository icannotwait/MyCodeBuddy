# Low-Memory Rust Development Design

## Status

Approved in conversation on 2026-08-06.

Implementation was authorized in conversation on 2026-08-06.

## Problem

The Rust package is large enough that normal desktop test builds can exceed
the practical limits of a 4 GiB development machine. Evidence gathered on
Windows showed a single `rustc --test` process reaching a working set above
12 GiB under the normal profile, and a concurrent full test build failed in
LLVM with an out-of-memory error.

Removing `staticlib` and `cdylib` avoids a separate 4 GiB build artifact, but
does not make the large test harness itself fit in 4 GiB. A supported
low-memory workflow must also prevent concurrent rustc processes, reduce debug
information, avoid incremental compiler state, and keep routine work away from
the Tauri feature graph when desktop-only code is not being changed.

The repository's normal Cargo and CI behavior must remain unchanged for
developers and release jobs with adequate resources.

## Goals

- Provide an explicit, cross-platform low-memory mode using Cargo's native
  configuration system.
- Make shared-core checks and targeted shared-core library tests the normal
  workflow on a 4 GiB machine.
- Provide opt-in desktop, server, and MCP checks under the same constraints.
- Keep the normal Cargo configuration, release builds, and CI matrix
  unchanged.
- Avoid a custom process wrapper or new runtime dependency.

## Non-Goals

- Guaranteeing the complete 4,000-plus Rust test suite on a 4 GiB machine.
- Guaranteeing `pnpm tauri dev` or desktop release builds on a 4 GiB machine.
- Changing production binaries, Cargo features, dependency versions, or CI.
- Adding a separate Cargo target directory or deleting existing build cache.
- Enforcing a hardware check based on reported physical memory.
- Optimizing frontend memory consumption.

## Selected Approach

Add an alternate Cargo configuration at `.cargo/low-memory.toml`. It is never
auto-loaded because Cargo only auto-loads `.cargo/config.toml`; every
low-memory command must opt in with `--config ../.cargo/low-memory.toml` while
running from `src-tauri`.

The configuration is:

```toml
[build]
jobs = 1
incremental = false

[profile.dev]
debug = 0

[profile.test]
debug = 0

[env]
RUST_TEST_THREADS = { value = "1", force = true }
```

The controls address distinct memory sources:

- `jobs = 1` prevents multiple Cargo compilation units from running at once.
- `incremental = false` avoids retaining incremental compiler state.
- `debug = 0` prevents large debug/PDB data in dev and test artifacts.
- `RUST_TEST_THREADS = 1` serializes libtest execution after compilation.

Cargo 1.97 in the current toolchain accepts all four settings through native
`--config` overrides. No JavaScript wrapper is required.

## Commands

Add package scripts that run from `src-tauri` and preserve Cargo's exit code:

```json
{
  "rust:check:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --lib",
  "rust:test:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml test --locked --no-default-features --features test-utils --lib",
  "rust:check:desktop:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --lib",
  "rust:check:server:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --features server --lib --bin codeg-server",
  "rust:check:mcp:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --bin codeg-mcp"
}
```

The first two commands intentionally disable default features. They cover the
shared Rust core without activating Tauri. A targeted test is passed through
pnpm, for example:

```bash
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

The desktop command exists for changes under `#[cfg(feature =
"tauri-runtime")]`, but it is expected to consume more memory than shared-core
checks. The server and MCP commands select their existing production feature
surfaces without enabling Tauri.

## Configuration Precedence

The existing `.cargo/config.toml` remains authoritative for target-specific
linkers. The alternate file is supplied as an additional CLI configuration,
so Cargo merges it with normal hierarchical configuration for the working
directory.

Low-memory settings deliberately win for the invoked process. They do not
mutate the caller's shell environment and do not affect later plain `cargo`
commands. Normal and low-memory profile variants may coexist in
`src-tauri/target`; the first low-memory invocation can therefore be a cold
build.

## Errors And User Feedback

No custom error translation is added. pnpm invokes Cargo directly, streams
Cargo diagnostics unchanged, and returns Cargo's exit status.

Documentation must state that:

- the first low-memory build can still be slow;
- the mode reduces peak pressure but cannot prove success on every 4 GiB
  operating-system configuration;
- targeted tests are preferred over an unfiltered library suite; and
- full regression remains a CI or higher-memory-machine responsibility.

## Documentation

Update the root `README.md` development section with the command table and a
short explanation of the support boundary. Update `AGENTS.md` so coding agents
use the same commands when operating under an explicit low-memory constraint.

Localized README copies are not changed in this work. The new workflow is a
developer-only repository command rather than a product feature, and updating
ten translations would add disproportionate maintenance.

## Verification

1. Parse the alternate file with Cargo metadata and `--locked`.
2. Run `pnpm rust:check:low-memory`.
3. Run one exact targeted test through `pnpm rust:test:low-memory`.
4. Run the desktop, server, and MCP low-memory checks.
5. Run the existing release-script tests to prove package-script edits did not
   disturb Node tooling.
6. Run `git diff --check` and confirm CI, Cargo features, and lockfiles are
   unchanged.

The current machine cannot emulate a 4 GiB memory ceiling reliably, so
verification proves command behavior and configuration isolation, not a hard
hardware guarantee.

## Alternatives Considered

### Cross-Platform Node Wrapper

A Node wrapper could force environment variables and validate modes. It would
introduce custom process-management code and tests for behavior Cargo already
supports. The user requested a lower-maintenance option, so native Cargo
configuration is preferred.

### Global Cargo Defaults

Adding the limits to `.cargo/config.toml` would require no special command,
but it would serialize and de-optimize builds for every developer and CI job.
The low-memory behavior must remain opt-in.

### PowerShell And Shell Wrappers

Platform scripts can set the same environment variables, but duplicate logic
between Windows and Unix and need separate maintenance. Package scripts plus
Cargo's native configuration are cross-platform and smaller.
