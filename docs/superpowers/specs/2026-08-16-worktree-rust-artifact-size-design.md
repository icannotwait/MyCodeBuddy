# Worktree Rust Artifact Size Design

## Status

Approved in brainstorming on 2026-08-16.

## Executive Decision

Keep a separate Cargo `target` directory in every Git worktree so concurrent
agents can build without sharing locks or allowing one worktree's cleanup to
invalidate another. Reduce each directory at the source by making Rust dev and
test builds omit debug information and disable incremental compilation by
default.

The repository-level Cargo configuration will apply these settings to normal
commands in newly created worktrees:

```toml
[build]
incremental = false

[profile.dev]
debug = 0

[profile.test]
debug = 0
```

The low-memory configuration will continue to add single-job compilation and
single-threaded test execution. It will no longer be the only path that gets
the disk-saving profile settings.

## Evidence

The `shared-acp-session-broker` worktree produced a 91.7 GiB
`src-tauri/target` directory from ordinary debug and test builds. Its contents
included:

- 83.6 GiB allocated under `target/debug/deps`;
- 7.2 GiB allocated under `target/debug/incremental`;
- 30,342 macOS split-debug `.o` entries with a combined logical size of
  105.6 GiB.

The builds used Cargo's macOS dev/test defaults: full debug information,
unpacked split debug information, and incremental compilation. These settings
produce large retained object files for this dependency graph and multiply the
cost across worktrees. Cargo also retains artifacts for previously compiled
feature and test-target combinations until explicitly cleaned.

The repository already has an opt-in `.cargo/low-memory.toml` configuration
with `debug = 0` and `incremental = false`. It does not prevent growth when a
developer or agent runs ordinary `cargo` commands, which is the common path.

## Goals

- Make ordinary Rust development and test commands disk-efficient without
  relying on developers or agents to remember a special command.
- Prevent unpacked macOS debug object files and incremental caches from
  dominating every worktree.
- Preserve independent worktree build directories and parallel agent
  development.
- Preserve debug assertions, overflow checks, test behavior, and release
  profile behavior.
- Prefer narrow daily verification commands so integration-test executables
  are linked only when relevant.
- Provide an explicit cleanup path for accumulated or retired worktree
  artifacts.

## Non-Goals

- Preserving LLDB local-variable inspection or source-line debug information
  in ordinary dev/test builds.
- Changing release optimization, release symbols, signing, packaging, or CI
  release outputs.
- Sharing one Cargo target directory between worktrees.
- Installing or configuring `sccache`. It may reduce later rebuild time but
  does not remove each worktree's final artifacts and introduces another cache
  that requires a size policy.
- Automatically deleting a live worktree's build output.
- Restricting normal parallel compilation through a global `jobs = 1` setting.

## Cargo Configuration

Add the disk-saving settings to `.cargo/config.toml`, which Cargo loads for
ordinary commands launched from the repository or `src-tauri` directory.
New worktrees created from a revision containing the change inherit the same
defaults.

`debug = 0` applies only to the dev and test profiles. It removes the debug
information that causes rustc to retain the large unpacked object set on
macOS. It does not disable `debug-assertions` or `overflow-checks`.

`incremental = false` prevents new `target/debug/incremental` caches. This
trades some edit-and-rebuild speed for bounded per-worktree storage. Cargo can
still reuse unchanged dependency artifacts within a worktree.

Do not set `split-debuginfo`: with debug information disabled there is no
split debug payload to configure. Do not set `jobs` in the default config;
parallel compilation remains the normal behavior.

Keep `.cargo/low-memory.toml` as an overlay for constrained machines. Its
effective behavior remains:

- one Cargo build job;
- no incremental compilation;
- no dev/test debug information;
- one Rust test thread;
- the existing enlarged test stack reservation.

The duplicated disk-profile values may remain in the overlay so invoking it
is self-describing and cannot accidentally weaken the low-memory contract.

## Verification Commands

Document and reinforce three Rust verification levels:

1. Daily shared-core feedback:
   `cargo test --lib --features test-utils`.
2. Relevant integration coverage:
   `cargo test --test <integration-test-name> --features test-utils` with the
   feature set required by that test.
3. Full regression:
   `cargo test --features test-utils`, reserved for branch completion, CI, or
   an explicit full-suite request.

A positional test-name filter such as `cargo test some_test_name` is not a
substitute for `--lib` or `--test`: Cargo still compiles all selected test
targets before filtering which test functions execute.

Update `AGENTS.md` so agents use the narrowest command that proves their
change. Full regression remains available and is not weakened; it is moved out
of the default inner development loop.

## Cleanup Lifecycle

The profile change affects newly generated artifacts only. Existing large
targets must be cleaned once before their worktree benefits fully.

Use an explicit manifest and target path for cleanup:

```bash
cargo clean \
  --manifest-path <worktree>/src-tauri/Cargo.toml \
  --target-dir <worktree>/src-tauri/target
```

Cleanup must not run while a Cargo or rustc process is using the target. It is
appropriate after the relevant command finishes, when retiring a worktree, or
when stale feature/profile combinations have accumulated. Removing a Git
worktree through `git worktree remove` also removes its contained target after
the worktree is confirmed clean.

No automatic cross-worktree cleanup service is introduced. Its ownership and
age rules would be more complex than the storage problem requires, and an
automatic cleaner could delete artifacts from an active agent.

## Alternatives Rejected

### Keep the opt-in low-memory commands only

This already exists and did not prevent the observed growth because ordinary
`cargo test` and `cargo check` commands bypass the overlay. Disk-saving
defaults must apply to the common path.

### Share one target directory across all worktrees

This can reuse more dependency artifacts, but it couples independent agents
through Cargo target locks and shared fingerprints. Alternating branches and
feature sets can invalidate each other's artifacts, and one `cargo clean`
would affect every worktree. The repository values parallel isolation more
than this additional reuse.

### Retain reduced debug information

Using line-table-only debug information with split debug disabled would be a
reasonable compromise for frequent LLDB use. The selected workflow rarely
needs LLDB, so `debug = 0` provides a simpler configuration and the largest
reduction.

## Validation

Implementation validation will cover:

- Cargo accepts the merged default and low-memory configurations.
- Cargo reports `debug = 0` and incremental compilation disabled for dev/test
  invocations, using verbose command output or a bounded fresh target.
- A narrow library test still runs successfully.
- Existing release profile behavior remains unchanged.
- Git ignore rules still exclude per-worktree target directories.
- A before/after size sample records the resulting target size without
  claiming an exact universal limit; size varies with selected features and
  test targets.

## Expected Trade-offs

Fresh and source-changing dev/test builds may take longer because incremental
compilation is disabled. LLDB will not have ordinary Rust source-line and
local-variable debug information. Developers needing a one-off debug session
can override the profile from the command line or a temporary Cargo config
without changing the repository default.

These costs are accepted in exchange for preventing tens of GiB of retained
artifacts per worktree.
