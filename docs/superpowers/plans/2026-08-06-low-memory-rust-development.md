# Low-Memory Rust Development Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in, check-oriented Rust workflow for 4 GiB machines plus
a lower-memory test command for larger machines, without changing normal Cargo
or CI behavior.

**Architecture:** A CLI-supplied Cargo configuration serializes compilation,
disables incremental state and debug information, and serializes libtest.
Root pnpm scripts expose shared-core, desktop, server, and MCP entry points;
documentation makes shared-core checking the supported 4 GiB workflow and
keeps test-harness compilation outside that guarantee.

**Tech Stack:** Cargo 1.97, Rust 2021, pnpm 11, PowerShell, JSON, TOML,
Markdown

## Global Constraints

- Keep low-memory behavior opt-in through `.cargo/low-memory.toml`; do not
  change `.cargo/config.toml` or normal Cargo defaults.
- Set `build.jobs = 1`, `build.incremental = false`, `profile.dev.debug = 0`,
  `profile.test.debug = 0`, and forced `RUST_TEST_THREADS = "1"` exactly.
- Shared-core commands must use `--no-default-features --lib` so routine work
  does not activate Tauri.
- Do not add a Node, PowerShell, or shell process wrapper or a new dependency.
- Do not change Cargo features, dependency versions, CI, `Cargo.lock`, or
  `pnpm-lock.yaml`.
- Do not add a separate Cargo target directory or delete existing build
  artifacts.
- Do not claim that full Rust regression, Tauri development, or release builds
  are guaranteed to fit on every 4 GiB machine.
- Do not claim that an exact unit-test filter makes compilation fit 4 GiB. The
  measured library test program contains 4,028 tests and its single `rustc`
  process peaked at approximately 7.55 GiB under this configuration.
- Keep localized README files unchanged.

## Validation Finding

Task 1 validation confirmed that Cargo compiled the exact filtered test into
the same monolithic library test program as the other 4,027 unit tests. The
filter is applied when that program runs, not while Cargo compiles it. The
implementation therefore keeps `rust:test:low-memory` as a materially smaller
option for machines with enough memory or page file, but Task 2 documents
`rust:check:low-memory` as the only recommended 4 GiB Rust path.

---

### Task 1: Add The Native Low-Memory Command Surface

**Files:**
- Create: `.cargo/low-memory.toml`
- Modify: `package.json:13-24`
- Test: Cargo configuration parsing, five package-script entry points, and one
  exact shared-core library test

**Interfaces:**
- Consumes: Cargo's `--config` global option and the existing `test-utils`,
  `server`, and `tauri-runtime` feature surfaces.
- Produces: pnpm scripts named `rust:check:low-memory`,
  `rust:test:low-memory`, `rust:check:desktop:low-memory`,
  `rust:check:server:low-memory`, and `rust:check:mcp:low-memory`.

- [ ] **Step 1: Verify the command surface is absent**

Run from the repository root:

```powershell
$package = Get-Content -Raw package.json | ConvertFrom-Json
$names = @(
  'rust:check:low-memory'
  'rust:test:low-memory'
  'rust:check:desktop:low-memory'
  'rust:check:server:low-memory'
  'rust:check:mcp:low-memory'
)
if (Test-Path .cargo/low-memory.toml) {
  throw '.cargo/low-memory.toml unexpectedly exists'
}
foreach ($name in $names) {
  if ($package.scripts.PSObject.Properties.Name -contains $name) {
    throw "$name unexpectedly exists"
  }
}
throw 'expected baseline failure: low-memory workflow is absent'
```

Expected: exit code 1 with `expected baseline failure: low-memory workflow is
absent`.

- [ ] **Step 2: Add the alternate Cargo configuration**

Create `.cargo/low-memory.toml` with exactly:

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

- [ ] **Step 3: Add the five package scripts**

Add these entries after `test:release` in `package.json`:

```json
"rust:check:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --lib",
"rust:test:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml test --locked --no-default-features --features test-utils --lib",
"rust:check:desktop:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --lib",
"rust:check:server:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --features server --lib --bin codeg-server",
"rust:check:mcp:low-memory": "cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --bin codeg-mcp",
```

- [ ] **Step 4: Assert the exact configuration and scripts**

Run from the repository root:

```powershell
$expectedConfig = @'
[build]
jobs = 1
incremental = false

[profile.dev]
debug = 0

[profile.test]
debug = 0

[env]
RUST_TEST_THREADS = { value = "1", force = true }
'@ -replace "`r`n", "`n"
$actualConfig = (Get-Content -Raw .cargo/low-memory.toml) -replace "`r`n", "`n"
if ($actualConfig.TrimEnd() -ne $expectedConfig.TrimEnd()) {
  throw 'unexpected low-memory Cargo configuration'
}

$package = Get-Content -Raw package.json | ConvertFrom-Json
$expectedScripts = [ordered]@{
  'rust:check:low-memory' = 'cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --lib'
  'rust:test:low-memory' = 'cd src-tauri && cargo --config ../.cargo/low-memory.toml test --locked --no-default-features --features test-utils --lib'
  'rust:check:desktop:low-memory' = 'cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --lib'
  'rust:check:server:low-memory' = 'cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --features server --lib --bin codeg-server'
  'rust:check:mcp:low-memory' = 'cd src-tauri && cargo --config ../.cargo/low-memory.toml check --locked --no-default-features --bin codeg-mcp'
}
foreach ($entry in $expectedScripts.GetEnumerator()) {
  if ($package.scripts.($entry.Key) -ne $entry.Value) {
    throw "unexpected script: $($entry.Key)"
  }
}
```

Expected: exit code 0 and no output.

- [ ] **Step 5: Parse the alternate configuration with Cargo**

Run from `src-tauri`:

```powershell
cargo --config ../.cargo/low-memory.toml metadata --locked --no-deps --format-version 1 | Out-Null
```

Expected: exit code 0 with no TOML or Cargo configuration error.

- [ ] **Step 6: Check the shared core**

Run from the repository root:

```powershell
pnpm rust:check:low-memory
```

Expected: exit code 0 after checking `codeg` without default features.

- [ ] **Step 7: Run one exact shared-core test**

Run from the repository root:

```powershell
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

Expected: exit code 0 with exactly one selected test passing.

- [ ] **Step 8: Check the opt-in runtime surfaces**

Run each command from the repository root:

```powershell
pnpm rust:check:desktop:low-memory
pnpm rust:check:server:low-memory
pnpm rust:check:mcp:low-memory
```

Expected: all three commands exit 0. The desktop check may still use more than
4 GiB and is not part of the guaranteed daily shared-core workflow.

- [ ] **Step 9: Review and commit the command surface**

Run:

```powershell
git diff --check
git diff -- .cargo/low-memory.toml package.json
git status --short
```

Expected: no whitespace errors and only the two Task 1 files are modified.
Then commit:

```powershell
git add -- .cargo/low-memory.toml package.json
git commit -m "build: add low-memory Rust commands"
```

---

### Task 2: Document The 4 GiB Workflow And Support Boundary

**Files:**
- Modify: `README.md:237-282`
- Modify: `AGENTS.md:31-54`
- Test: documentation assertions, existing release-script suite, and protected
  file diff checks

**Interfaces:**
- Consumes: the five package scripts produced by Task 1.
- Produces: contributor and coding-agent guidance that uses shared-core checks
  and exact tests by default under an explicit low-memory constraint.

- [ ] **Step 1: Confirm low-memory guidance is absent**

Run from the repository root:

```powershell
if (Select-String -Quiet -LiteralPath README.md -Pattern 'rust:check:low-memory') {
  throw 'README low-memory guidance unexpectedly exists'
}
if (Select-String -Quiet -LiteralPath AGENTS.md -Pattern 'rust:check:low-memory') {
  throw 'AGENTS low-memory guidance unexpectedly exists'
}
throw 'expected baseline failure: low-memory guidance is absent'
```

Expected: exit code 1 with `expected baseline failure: low-memory guidance is
absent`.

- [ ] **Step 2: Add the README workflow**

After the existing Rust test commands in `README.md`, add a `Low-memory Rust
development` subsection containing:

````markdown
#### Low-memory Rust development

Run these opt-in commands from the repository root:

| Command                                                                                                        | Scope                                                                                             |
| -------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------- |
| `pnpm rust:check:low-memory`                                                                                   | Shared Rust library without Tauri; the recommended 4 GiB daily check                              |
| `pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact`            | One exact shared-core test at runtime; compilation still builds the complete library test harness |
| `pnpm rust:check:desktop:low-memory`                                                                           | Desktop library, including Tauri                                                                  |
| `pnpm rust:check:server:low-memory`                                                                            | Server library and binary                                                                         |
| `pnpm rust:check:mcp:low-memory`                                                                               | MCP companion binary                                                                              |

The alternate Cargo configuration limits compilation and test execution to
one job/thread and disables incremental state and debug information. It is
opt-in, so normal Cargo commands and CI are unchanged. The first invocation
can still be slow because it may need a cold build.

On the current Windows codebase, even one filtered unit test first compiles a
single harness containing all 4,028 library tests. The low-memory profile
reduced its observed `rustc` peak from roughly 12.2 GiB to 7.55 GiB, but the
test-name filter only changes execution after compilation. A 4 GiB machine
should therefore use `rust:check:low-memory` for daily Rust feedback and leave
Rust tests to CI or a higher-memory machine; a large system page file may help
but is not guaranteed. The desktop check and Tauri development can also exceed
4 GiB.

When enough memory is available, an exact test can be run with:

```bash
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```
````

- [ ] **Step 3: Add coding-agent guidance**

After the normal backend Rust command block in `AGENTS.md`, add:

````markdown
### 低内存 Rust 开发（在仓库根目录执行）

仅在明确受低内存约束时使用以下 opt-in 命令：

```bash
# 4 GiB 机器的日常 Rust 反馈：只检查共享核心，不启用 Tauri
pnpm rust:check:low-memory
# 仅在改动对应运行面时执行；桌面检查仍可能超过 4 GiB
pnpm rust:check:desktop:low-memory
pnpm rust:check:server:low-memory
pnpm rust:check:mcp:low-memory
# 有更高可用内存或足够页文件时，才运行精确单测
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```

低内存配置将 Cargo 编译任务和测试线程限制为 1，并关闭增量编译和调试信息，
但首次冷编译仍可能较慢。`--exact` 只过滤测试运行，不会缩小编译目标；当前
Windows 整库测试程序包含 4,028 个测试，单个 `rustc` 在低内存配置下实测峰值
仍约 7.55 GiB。因此 4 GiB 机器默认只运行共享核心 check，Rust 单测与完整回归
交由 CI 或更高内存机器执行；足够大的系统页文件可能有帮助，但不作成功保证。
````

- [ ] **Step 4: Assert the documented commands and caveats**

Run from the repository root:

```powershell
$readme = Get-Content -Raw README.md
$agents = Get-Content -Raw AGENTS.md
$requiredCommands = @(
  'rust:check:low-memory'
  'rust:test:low-memory'
  'rust:check:desktop:low-memory'
  'rust:check:server:low-memory'
  'rust:check:mcp:low-memory'
)
foreach ($command in $requiredCommands) {
  if (-not $readme.Contains($command)) { throw "README missing $command" }
  if (-not $agents.Contains($command)) { throw "AGENTS missing $command" }
}
if (-not $readme.Contains('test-name filter only changes execution after compilation')) {
  throw 'README missing 4 GiB support boundary'
}
if (-not $agents.Contains('4 GiB 机器默认只运行共享核心 check')) {
  throw 'AGENTS missing 4 GiB support boundary'
}
```

Expected: exit code 0 and no output.

- [ ] **Step 5: Run existing package-script regression tests**

Run from the repository root:

```powershell
pnpm test:release
```

Expected: exit code 0 with all Node test cases passing.

- [ ] **Step 6: Verify protected files and normal configuration are unchanged**

Run from the repository root:

```powershell
$protected = @(
  '.cargo/config.toml'
  '.github/workflows/test.yml'
  '.github/workflows/release.yml'
  'src-tauri/Cargo.toml'
  'src-tauri/Cargo.lock'
  'pnpm-lock.yaml'
)
$changed = @(git diff HEAD~1 --name-only -- $protected)
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($changed.Count -ne 0) {
  throw "protected files changed: $($changed -join ', ')"
}
git diff --check
```

Expected: exit code 0, no protected file names, and no whitespace errors.

- [ ] **Step 7: Review and commit the documentation**

Run:

```powershell
git diff -- README.md AGENTS.md
git status --short
```

Expected: only the documented low-memory sections remain uncommitted. Then
commit:

```powershell
git add -- README.md AGENTS.md
git commit -m "docs: explain low-memory Rust development"
```

- [ ] **Step 8: Perform final repository verification**

Run from the repository root:

```powershell
git diff --check HEAD~2..HEAD
git status --short --branch
git diff HEAD~2..HEAD --name-only
```

Expected: a clean worktree; implementation changes are limited to
`.cargo/low-memory.toml`, `package.json`, `README.md`, `AGENTS.md`, and the
validation corrections in this plan and its design document.
