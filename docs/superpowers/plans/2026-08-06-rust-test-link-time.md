# Rust Test Link-Time Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove mobile-only Rust library outputs from desktop/server builds
and document a fast local unit-test command without reducing CI coverage.

**Architecture:** The Cargo library target will emit only `rlib`, which is the
format consumed by this repository's Rust binaries and tests. Contributor
guidance will distinguish fast library-only feedback from the unchanged full
desktop regression command; CI remains untouched.

**Tech Stack:** Cargo, Rust 2021, Tauri 2, PowerShell, Markdown

## Global Constraints

- The supported build matrix is desktop plus standalone server; Android and
  iOS builds are intentionally unsupported after this change.
- Do not remove, merge, ignore, or feature-gate any existing test.
- Do not change `.github/workflows/test.yml` or reduce CI coverage.
- Do not split `test-utils`, `tauri/test`, or `tauri/devtools` in this work.
- Do not clean or delete existing files under `src-tauri/target`.
- Keep tracked implementation changes limited to `src-tauri/Cargo.toml` and
  `AGENTS.md`, in addition to the approved design and this plan.

---

### Task 1: Remove The Measured Link Bottleneck

**Files:**
- Modify: `src-tauri/Cargo.toml:15-20`
- Modify: `AGENTS.md:32-38`
- Test: Cargo target metadata and existing Rust test suites

**Interfaces:**
- Consumes: Cargo's `[lib].crate-type` target definition and the existing
  `test-utils` feature.
- Produces: one `rlib` library target plus documented fast and full test
  commands.

- [ ] **Step 1: Confirm the failing baseline**

Use the diagnostic already captured for this task:

```text
cargo test --locked --features test-utils --no-run
wall_seconds=480.15
codeg_lib.lib approximately 4.0 GiB
```

Run this metadata assertion before editing:

```powershell
$metadata = cargo metadata --format-version 1 --locked |
  ConvertFrom-Json
$package = $metadata.packages |
  Where-Object name -eq 'codeg' |
  Select-Object -First 1
($package.targets |
  Where-Object name -eq 'codeg_lib').crate_types -join ','
```

Expected before the change:

```text
staticlib,cdylib,rlib
```

- [ ] **Step 2: Restrict the library target to `rlib`**

Change `src-tauri/Cargo.toml` to:

```toml
[lib]
# The library is consumed only by Rust desktop/server binaries and tests.
# Tauri mobile builds would additionally require staticlib/cdylib.
name = "codeg_lib"
crate-type = ["rlib"]
```

- [ ] **Step 3: Document fast and full desktop test levels**

Replace the desktop test block in `AGENTS.md` with:

```bash
# Desktop mode (default feature)
cargo check
# Fast local feedback: library unit tests only
cargo test --lib --features test-utils
# Complete regression: unit, binary, and integration tests
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
```

Keep every server, MCP, snapshot, and wait-strategy command unchanged.

- [ ] **Step 4: Verify the Cargo target changed**

Run:

```powershell
$metadata = cargo metadata --format-version 1 --locked |
  ConvertFrom-Json
$package = $metadata.packages |
  Where-Object name -eq 'codeg' |
  Select-Object -First 1
$crateTypes = @(($package.targets |
  Where-Object name -eq 'codeg_lib').crate_types)
if ($crateTypes.Count -ne 1 -or $crateTypes[0] -ne 'rlib') {
  throw "unexpected crate types: $($crateTypes -join ',')"
}
```

Expected: exit code 0 and no output.

- [ ] **Step 5: Run fast local tests**

Run from `src-tauri` and record wall time:

```powershell
$timer = [System.Diagnostics.Stopwatch]::StartNew()
cargo test --locked --lib --features test-utils
$exitCode = $LASTEXITCODE
$timer.Stop()
"wall_seconds=$([math]::Round($timer.Elapsed.TotalSeconds, 2))"
exit $exitCode
```

Expected: exit code 0 with all library unit tests passing.

- [ ] **Step 6: Compile the complete desktop regression suite**

Run from `src-tauri` and record wall time:

```powershell
$timer = [System.Diagnostics.Stopwatch]::StartNew()
cargo test --locked --features test-utils --no-run
$exitCode = $LASTEXITCODE
$timer.Stop()
"wall_seconds=$([math]::Round($timer.Elapsed.TotalSeconds, 2))"
exit $exitCode
```

Expected: exit code 0; Cargo lists the unit, binary, and 20 integration-test
executables without rebuilding a `staticlib` or `cdylib` target.

- [ ] **Step 7: Check desktop and server configurations**

Run from `src-tauri`:

```powershell
cargo check --locked
cargo check --locked --no-default-features --features server --bin codeg-server
```

Expected: both commands exit 0.

- [ ] **Step 8: Review and commit the implementation**

Run:

```powershell
git diff --check
git diff -- src-tauri/Cargo.toml AGENTS.md
git status --short
```

Expected: no whitespace errors and no unrelated tracked changes. Then commit:

```powershell
git add -- src-tauri/Cargo.toml AGENTS.md
git commit -m "build: avoid mobile library outputs in Rust tests"
```
