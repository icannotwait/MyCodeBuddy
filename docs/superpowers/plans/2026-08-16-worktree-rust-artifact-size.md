# Worktree Rust Artifact Size Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make ordinary Rust dev and test builds generate bounded per-worktree artifacts by disabling debug information and incremental compilation by default.

**Architecture:** Keep each Git worktree's Cargo target isolated, and change the repository-level Cargo defaults that every ordinary invocation inherits. Add a fast Node policy test for the exact Cargo and agent-command contracts, then validate Cargo parsing and a narrow Rust test against a fresh target.

**Tech Stack:** Cargo configuration, Rust 2021, Node.js `node:test`, pnpm

## Global Constraints

- Keep a separate `src-tauri/target` directory in every Git worktree.
- Set `debug = 0` for Cargo dev and test profiles.
- Set `incremental = false` for ordinary Cargo builds.
- Do not set default Cargo build jobs to one; normal parallel compilation remains enabled.
- Preserve the low-memory overlay's `jobs = 1`, `RUST_TEST_THREADS = 1`, and `RUST_MIN_STACK = 33554432` behavior.
- Do not change release profile behavior.
- Daily Rust verification uses library-only or explicitly selected integration-test targets; the full suite remains available for final regression.
- Do not clean a target while Cargo or rustc is using it.

---

### Task 1: Enforce Disk-Saving Cargo Defaults and Verification Policy

**Files:**
- Create: `scripts/rust-build-policy.test.mjs`
- Modify: `.cargo/config.toml`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: Cargo's repository-local configuration discovery and the existing `pnpm test:release` glob for `scripts/*.test.mjs`.
- Produces: ordinary dev/test builds with no debug information or incremental cache, plus a repository policy test that protects those settings and the narrow-test guidance.

- [ ] **Step 1: Write the failing policy test**

Create `scripts/rust-build-policy.test.mjs`:

```javascript
import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { dirname, join } from "node:path"
import { spawnSync } from "node:child_process"
import test from "node:test"
import { fileURLToPath } from "node:url"

const repositoryRoot = dirname(fileURLToPath(import.meta.url)) + "/.."

test("repository Cargo config omits debug and incremental artifacts", () => {
  const fixtureRoot = mkdtempSync(join(tmpdir(), "codeg-cargo-policy-"))
  const manifestPath = join(fixtureRoot, "Cargo.toml")
  const targetPath = join(fixtureRoot, "target")

  try {
    mkdirSync(join(fixtureRoot, "src"))
    writeFileSync(
      manifestPath,
      '[package]\nname = "cargo-policy-fixture"\nversion = "0.1.0"\nedition = "2021"\n'
    )
    writeFileSync(join(fixtureRoot, "src", "lib.rs"), "pub fn value() -> u8 { 1 }\n")

    const result = spawnSync(
      "cargo",
      [
        "--config",
        join(repositoryRoot, ".cargo", "config.toml"),
        "test",
        "--no-run",
        "-v",
        "--manifest-path",
        manifestPath,
        "--target-dir",
        targetPath,
      ],
      {
        encoding: "utf8",
        env: { ...process.env, CARGO_TERM_COLOR: "never" },
        maxBuffer: 10 * 1024 * 1024,
      }
    )
    const output = `${result.stdout}\n${result.stderr}`

    assert.equal(result.status, 0, output)
    assert.match(output, /rustc.*cargo_policy_fixture/)
    assert.doesNotMatch(output, /-C debuginfo=[1-9]/)
    assert.doesNotMatch(output, /-C incremental=/)
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true })
  }
})
```

- [ ] **Step 2: Run the policy test and verify it fails**

Run:

```bash
node --test scripts/rust-build-policy.test.mjs
```

Expected: FAIL because Cargo's verbose rustc commands contain positive
`-C debuginfo` and `-C incremental` arguments under the current defaults.

- [ ] **Step 3: Add the default Cargo profile settings**

Prepend this content to `.cargo/config.toml`, leaving the target and environment
sections unchanged:

```toml
[build]
incremental = false

[profile.dev]
debug = 0

[profile.test]
debug = 0
```

- [ ] **Step 4: Document narrow verification and cleanup commands**

Extend the Rust test guidance in `AGENTS.md` with these exact commands and
rules:

```bash
# Daily library tests: avoids linking every integration-test executable
cargo test --lib --features test-utils

# One integration target; replace the file stem only when another target is relevant
cargo test --test delegation_session_reuse_integration --features test-utils

# Explicit cleanup after Cargo/rustc exits or before removing a worktree
cargo clean --manifest-path src-tauri/Cargo.toml --target-dir src-tauri/target
```

State that `cargo test some_test_name` filters executed test functions but
still compiles all selected test targets, so it is not a narrow-build command.
Reserve `cargo test --features test-utils` for final regression, CI, or an
explicit request.

- [ ] **Step 5: Run the policy test and repository release-policy suite**

Run:

```bash
node --test scripts/rust-build-policy.test.mjs
pnpm test:release
```

Expected: both commands PASS with zero failed tests.

- [ ] **Step 6: Verify both Cargo configuration layers parse**

Run:

```bash
cd src-tauri
cargo metadata --no-deps --format-version 1
cargo --config ../.cargo/low-memory.toml metadata --no-deps --format-version 1
```

Expected: both commands exit zero and emit Cargo metadata JSON. No release
profile configuration is introduced.

- [ ] **Step 7: Commit the policy implementation**

```bash
git add .cargo/config.toml AGENTS.md scripts/rust-build-policy.test.mjs
git commit -m "build: reduce Rust worktree artifacts"
```

### Task 2: Clean Legacy Artifacts and Validate a Fresh Narrow Build

**Files:**
- Modify: none; this task validates generated output only.

**Interfaces:**
- Consumes: the Cargo defaults and narrow commands from Task 1.
- Produces: fresh evidence that ordinary Cargo commands omit debug information and incremental output, plus reclaimed disk space from the legacy worktree target.

- [ ] **Step 1: Confirm no process uses the legacy target**

Run:

```bash
ps aux | rg '/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/shared-acp-session-broker/.+(cargo|rustc)|(cargo|rustc).+/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/shared-acp-session-broker' || true
```

Expected: no matching Cargo or rustc process. Stop before cleanup if a process
is present.

- [ ] **Step 2: Remove the legacy full-debug target**

Run:

```bash
cargo clean \
  --manifest-path /Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/shared-acp-session-broker/src-tauri/Cargo.toml \
  --target-dir /Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/shared-acp-session-broker/src-tauri/target
```

Expected: Cargo reports removed artifacts and the target no longer consumes
the prior 91.7 GiB.

- [ ] **Step 3: Run a narrow library test with verbose compiler evidence**

Run from the main worktree:

```bash
cd src-tauri
cargo test -v --lib --features test-utils \
  acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

Expected: the exact test passes. Compiler commands generated after Task 1 use
`-C strip=debuginfo` or omit positive `-C debuginfo` settings, and do not use
`-C incremental`.

- [ ] **Step 4: Verify the resulting artifact layout and disk state**

Run from the repository root:

```bash
du -x -sh src-tauri/target
du -x -sh .worktrees/*/src-tauri/target 2>/dev/null || true
find src-tauri/target/debug -type d -name incremental -prune -exec du -sh {} \;
df -h .
git status --short
```

Expected: the new target is materially below the previous 91.7 GiB sample,
incremental directories are absent or empty, disk free space reflects legacy
cleanup, and Git status contains only the user's pre-existing icon changes.

- [ ] **Step 5: Run final policy verification and inspect the implementation commit**

Run:

```bash
node --test scripts/rust-build-policy.test.mjs
git show --check --stat --oneline HEAD
```

Expected: the policy tests pass, the implementation commit contains only
`.cargo/config.toml`, `AGENTS.md`, and
`scripts/rust-build-policy.test.mjs`, and `git show --check` reports no
whitespace errors.
