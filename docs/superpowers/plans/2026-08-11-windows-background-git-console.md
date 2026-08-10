# Windows Background Git Console Fix Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent workflow Git probes from opening command-line windows on Windows while preserving their current Git behavior and error handling.

**Architecture:** Keep both workflow call sites intact except for how they construct `git`. Each module gets a production-used, module-private command builder; regression tests inspect the builder's explicit UTF-8 locale configuration, and the final implementation delegates that builder to the existing centralized process helper that also owns `CREATE_NO_WINDOW` on Windows.

**Tech Stack:** Rust 2021, Tokio process API, standard-library process API, Cargo test harness

## Global Constraints

- `artifact_resolver::run_git` must use `crate::process::tokio_command("git")` through its production command builder.
- `admission::workspace_head_commit` must use `crate::process::std_command("git")` through its production command builder.
- Preserve all Git arguments, current directories, Tokio `kill_on_drop(true)`, output capture, exit-status handling, and error mapping.
- Do not add a new Windows-only branch or duplicate `CREATE_NO_WINDOW`; `src-tauri/src/process.rs` remains the single owner.
- Regression tests must inspect constructed command behavior and must not grep source files.
- Do not modify or remove unrelated `.codex-tmp-*` files.

---

### Task 1: Route artifact resolver Git through the Tokio helper

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/artifact_resolver.rs:210`
- Test: `src-tauri/src/acp/delegation/workflow/artifact_resolver.rs:311`

**Interfaces:**
- Consumes: `crate::process::tokio_command<S>(program: S) -> tokio::process::Command`
- Produces: `fn git_command() -> tokio::process::Command`, used by `run_git`

- [ ] **Step 1: Confirm the existing real-Git behavior test is green**

Run:

```powershell
cargo test --lib --features test-utils acp::delegation::workflow::artifact_resolver::tests::git_resolver_requires_head_and_completely_empty_porcelain -- --exact
```

Expected: one test passes and no tests fail.

- [ ] **Step 2: Extract the current raw construction without changing behavior**

Add this immediately before `run_git`:

```rust
fn git_command() -> tokio::process::Command {
    tokio::process::Command::new("git")
}
```

Change only the construction expression in `run_git`:

```rust
let output = git_command()
    .args(args)
    .current_dir(workspace)
    .kill_on_drop(true)
    .output()
    .await
    .map_err(|_| ArtifactError::Unavailable(ArtifactFailure::GitCommandFailed))?;
```

- [ ] **Step 3: Verify the extraction preserves behavior**

Run the Step 1 command again.

Expected: one test passes and no tests fail.

- [ ] **Step 4: Write the failing command-configuration regression**

Add `git_command` to the test module's `use super::{...}` list, then add:

```rust
#[test]
fn git_command_sets_explicit_utf8_locale() {
    let command = git_command();
    let envs: std::collections::HashMap<_, _> = command
        .as_std()
        .get_envs()
        .filter_map(|(key, value)| {
            Some((
                key.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();

    assert_eq!(envs.get("LANG").map(String::as_str), Some("C.UTF-8"));
    assert_eq!(
        envs.get("LC_ALL").map(String::as_str),
        Some("C.UTF-8")
    );
}
```

- [ ] **Step 5: Run the regression and verify RED**

Run:

```powershell
cargo test --lib --features test-utils acp::delegation::workflow::artifact_resolver::tests::git_command_sets_explicit_utf8_locale -- --exact
```

Expected: the assertion fails because raw Tokio `Command::new("git")` has no explicit `LANG` value.

- [ ] **Step 6: Switch the builder to the centralized Tokio helper**

Replace the builder body with:

```rust
fn git_command() -> tokio::process::Command {
    crate::process::tokio_command("git")
}
```

- [ ] **Step 7: Verify GREEN and the existing Git behavior**

Run the Step 5 command, then the Step 1 command.

Expected: both commands pass with no failures.

- [ ] **Step 8: Commit the artifact resolver fix**

```powershell
git add src-tauri/src/acp/delegation/workflow/artifact_resolver.rs
git commit -m "fix: hide workflow artifact git processes"
```

### Task 2: Route admission HEAD capture through the standard helper

**Files:**
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs:1354`
- Test: `src-tauri/src/acp/delegation/workflow/admission.rs:2936`

**Interfaces:**
- Consumes: `crate::process::std_command<S>(program: S) -> std::process::Command`
- Produces: `fn git_command() -> std::process::Command`, used by `workspace_head_commit`

- [ ] **Step 1: Confirm the existing branch-tip behavior test is green**

Run:

```powershell
cargo test --lib --features test-utils acp::delegation::workflow::admission::tests::final_first_pass_stamps_branch_tip_digest -- --exact
```

Expected: one test passes and no tests fail.

- [ ] **Step 2: Extract the current raw construction without changing behavior**

Add this immediately before `workspace_head_commit`:

```rust
fn git_command() -> std::process::Command {
    std::process::Command::new("git")
}
```

Change only the construction expression in `workspace_head_commit`:

```rust
let output = git_command()
    .args(["rev-parse", "HEAD"])
    .current_dir(path)
    .output()
    .ok()?;
```

- [ ] **Step 3: Verify the extraction preserves behavior**

Run the Step 1 command again.

Expected: one test passes and no tests fail.

- [ ] **Step 4: Write the failing command-configuration regression**

Add this test to the module-local test module:

```rust
#[test]
fn git_command_sets_explicit_utf8_locale() {
    let command = git_command();
    let envs: std::collections::HashMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| {
            Some((
                key.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();

    assert_eq!(envs.get("LANG").map(String::as_str), Some("C.UTF-8"));
    assert_eq!(
        envs.get("LC_ALL").map(String::as_str),
        Some("C.UTF-8")
    );
}
```

- [ ] **Step 5: Run the regression and verify RED**

Run:

```powershell
cargo test --lib --features test-utils acp::delegation::workflow::admission::tests::git_command_sets_explicit_utf8_locale -- --exact
```

Expected: the assertion fails because raw standard `Command::new("git")` has no explicit `LANG` value.

- [ ] **Step 6: Switch the builder to the centralized standard helper**

Replace the builder body with:

```rust
fn git_command() -> std::process::Command {
    crate::process::std_command("git")
}
```

- [ ] **Step 7: Verify GREEN and the existing branch-tip behavior**

Run the Step 5 command, then the Step 1 command.

Expected: both commands pass with no failures.

- [ ] **Step 8: Commit the admission fix**

```powershell
git add src-tauri/src/acp/delegation/workflow/admission.rs
git commit -m "fix: hide workflow admission git process"
```

### Task 3: Verify the complete desktop fix

**Files:**
- Modify: `docs/superpowers/specs/2026-08-11-windows-background-git-console-design.md`
- Create: `docs/superpowers/plans/2026-08-11-windows-background-git-console.md`

**Interfaces:**
- Consumes: the two production command builders completed in Tasks 1 and 2
- Produces: verified desktop compilation and documented regression strategy

- [ ] **Step 1: Run both new regressions together**

Run:

```powershell
cargo test --lib --features test-utils git_command_sets_explicit_utf8_locale
```

Expected: two tests pass and no tests fail.

- [ ] **Step 2: Run desktop compilation verification**

Run:

```powershell
cargo check
```

Expected: Cargo exits successfully with no compiler errors.

- [ ] **Step 3: Inspect scope and whitespace**

Run:

```powershell
git diff --check
git diff --stat
git status --short
```

Expected: no whitespace errors; only the two workflow modules and the two Superpowers documents are part of this fix, while pre-existing untracked `.codex-tmp-*` files remain untouched.

- [ ] **Step 4: Commit the documentation update**

```powershell
git add docs/superpowers/specs/2026-08-11-windows-background-git-console-design.md
git add -f docs/superpowers/plans/2026-08-11-windows-background-git-console.md
git commit -m "docs: plan Windows background Git console fix"
```

- [ ] **Step 5: Review final history and requirements**

Run:

```powershell
git log -4 --oneline
git status --short --branch
```

Expected: implementation and documentation commits are present; no tracked changes remain; unrelated untracked files are unchanged.
