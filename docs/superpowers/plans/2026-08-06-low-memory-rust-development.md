# Low-Memory Rust Development Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an opt-in Rust development workflow that lowers compiler and
test-runner memory pressure for 4 GiB machines without changing normal Cargo
or CI behavior.

**Architecture:** A CLI-supplied Cargo configuration serializes compilation,
disables incremental state and debug information, and serializes libtest.
Root pnpm scripts expose shared-core, desktop, server, and MCP entry points;
documentation makes targeted shared-core work the supported 4 GiB workflow.

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
- Keep localized README files unchanged.

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
development (4 GiB machines)` subsection containing:

````markdown
#### Low-memory Rust development (4 GiB machines)

Run these opt-in commands from the repository root:

| Command | Scope |
| --- | --- |
| `pnpm rust:check:low-memory` | Shared Rust library without Tauri (recommended daily check) |
| `pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact` | One exact shared-core library test |
| `pnpm rust:check:desktop:low-memory` | Desktop library, including Tauri |
| `pnpm rust:check:server:low-memory` | Server library and binary |
| `pnpm rust:check:mcp:low-memory` | MCP companion binary |

The alternate Cargo configuration limits compilation and test execution to
one job/thread and disables incremental state and debug information. The first
invocation can still be slow because it may need a cold build. These settings
reduce peak memory pressure but cannot guarantee every desktop build will fit
within 4 GiB; prefer the shared-core check and exact tests for daily work. Run
the complete Rust regression suite in CI or on a higher-memory machine.

For example:

```bash
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
```
````

- [ ] **Step 3: Add coding-agent guidance**

After the normal backend Rust command block in `AGENTS.md`, add:

````markdown
### 低内存 Rust 开发（在仓库根目录执行）

仅在明确受 4 GiB 内存约束时使用以下 opt-in 命令：

```bash
# 日常共享核心检查（不启用 Tauri）
pnpm rust:check:low-memory
# 优先运行单个精确测试；将路径替换为本次改动对应的测试
pnpm rust:test:low-memory -- acp::codex_goal::tests::clear_with_no_open_goal_is_a_noop -- --exact
# 仅在改动对应运行面时执行
pnpm rust:check:desktop:low-memory
pnpm rust:check:server:low-memory
pnpm rust:check:mcp:low-memory
```

低内存配置将编译与测试线程限制为 1，并关闭增量编译和调试信息。首次冷编译
仍可能较慢，也不能保证完整桌面构建适配所有 4 GiB 环境。日常应优先共享核心
检查和定向测试；完整 Rust 回归交由 CI 或更高内存机器执行。
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
if (-not $readme.Contains('cannot guarantee every desktop build will fit within 4 GiB')) {
  throw 'README missing 4 GiB support boundary'
}
if (-not $agents.Contains('不能保证完整桌面构建适配所有 4 GiB 环境')) {
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
`.cargo/low-memory.toml`, `package.json`, `README.md`, and `AGENTS.md`.
