# Windows Background Git Console Suppression Design

## Status

Direction approved on 2026-08-11.

## Problem

DrawCode runs Git commands in the background while admitting and settling
multi-agent workflow tasks. On Windows, console applications create a visible
console host unless the parent sets `CREATE_NO_WINDOW`.

The repository already centralizes this platform behavior in
`crate::process::tokio_command` and `crate::process::std_command`. Both helpers
also preserve the existing program normalization and UTF-8 environment.
However, two production workflow paths bypass those helpers:

1. `artifact_resolver::run_git` directly creates a Tokio Git command. This path
   repeatedly runs `git rev-parse HEAD` and
   `git status --porcelain=v1 --untracked-files=all`.
2. `admission::workspace_head_commit` directly creates a standard-library Git
   command when recording a terminal workflow artifact.

Process inspection on the affected machine showed the first path being
created by `DrawCode.exe` every few seconds. Windows Terminal then created an
`OpenConsole.exe` and `conhost.exe` instance for each invocation. Hundreds of
console hosts accumulated even though each Git process was short-lived.

## Decision

Route both production workflow Git entry points through the existing process
helpers:

- `artifact_resolver::run_git` uses `crate::process::tokio_command("git")`.
- `admission::workspace_head_commit` uses
  `crate::process::std_command("git")`.

No new process abstraction or Windows-only branch is introduced. The shared
helpers remain the single owner of `CREATE_NO_WINDOW` and program resolution.

The change preserves each call site's Git arguments, current directory,
`kill_on_drop` behavior, output capture, exit-status handling, and error
mapping. It does not alter workflow admission rules, artifact cleanliness
requirements, retry cadence, or task state.

## Alternatives Considered

### Fix only `artifact_resolver::run_git`

This is the smallest change and fixes the high-frequency process pair observed
in the incident. It leaves `workspace_head_commit` able to open a console when
terminal workflow state is recorded, so the same defect remains in the same
subsystem. Rejected in favor of covering both production workflow paths.

### Add `CREATE_NO_WINDOW` directly at each call site

This would hide the windows but duplicate Windows-specific behavior and omit
the central helper's program normalization and UTF-8 environment. It also
makes future audits harder. Rejected because the repository already has the
correct abstraction.

### Replace Git subprocesses with a Git library

This would avoid console processes entirely, but changes command semantics and
error behavior far beyond the incident. Rejected as unnecessary scope.

## Testing

Implementation follows test-driven development.

Two module-local policy regressions will inspect only the production portion of
their source modules and reject raw `Command::new("git")` construction while
requiring the corresponding central helper. This is intentionally a structural
test: the defect is bypassing a mandatory process-construction policy, and the
Windows creation flags are not exposed by Rust's public `Command` inspection
API. Existing artifact resolver tests continue to prove real Git behavior,
including HEAD normalization, dirty-worktree rejection, and error
classification.

Targeted Rust verification from `src-tauri/`:

```text
cargo test --lib --features test-utils acp::delegation::workflow::artifact_resolver::tests::production_git_spawn_uses_central_process_helper -- --exact
cargo test --lib --features test-utils acp::delegation::workflow::admission::tests::production_git_spawn_uses_central_process_helper -- --exact
cargo check
```

A Windows smoke check will compare `conhost.exe` and `OpenConsole.exe` creation
before and after repeatedly exercising the two Git paths. The smoke check must
not terminate unrelated console processes.

## Success Criteria

- Repeated workflow artifact checks create no visible command-line windows on
  Windows.
- Terminal workflow HEAD capture also creates no visible command-line window.
- Git arguments, output, failure classification, and workflow behavior are
  unchanged.
- Both call sites use the centralized process helpers, preventing a local
  Windows flag from drifting again.
- Targeted Rust tests and desktop `cargo check` pass.
