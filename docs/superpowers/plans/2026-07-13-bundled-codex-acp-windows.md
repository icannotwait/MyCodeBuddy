# Bundled Codex ACP for Windows x64 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the customized codex-acp fork inside the single MyCodeBuddy Windows x64 NSIS installer and always prefer it over a global official adapter.

**Architecture:** Keep codex-acp in a public fork pinned into MyCodeBuddy as a Git submodule. Build it into a Bun Windows x64 executable during release, bundle it as a Tauri sidecar, and represent it in the ACP registry with a dedicated `Bundled` distribution whose resolver checks an explicit override, the installed sibling, then PATH.

**Tech Stack:** TypeScript, npm, Bun compiled executables, Rust 2021, Tauri 2 sidecars, React 19/Vitest, Cargo tests, GitHub Actions/PowerShell.

## Global Constraints

- The only bundled adapter target is `x86_64-pc-windows-msvc`.
- Users receive one NSIS installer; codex-acp is not a separate release asset.
- The fork repository is public at `https://github.com/icannotwait/codex-acp.git`.
- The submodule path is `src-tauri/vendor/codex-acp`.
- The first bundled adapter version is `1.1.0-mycodebuddy.1`.
- Windows defaults are `CODEX_ACP_USE_CLI=1` and `CODEX_ACP_CLI_MODEL=gpt-5.5`; saved per-agent environment values override them.
- Packaged Windows builds never install, update, or uninstall global npm codex-acp.
- macOS development retains the existing npm/PATH Codex behavior.
- Preserve all unrelated dirty-worktree changes in both repositories.

---

### Task 1: Publish And Pin The Customized Fork

**Files:**
- Modify in codex-acp fork: `package.json`
- Modify in codex-acp fork: `package-lock.json`
- Create in Codeg: `.gitmodules`
- Create in Codeg: `src-tauri/vendor/codex-acp` (Git submodule gitlink)

**Interfaces:**
- Consumes: the existing local `/Users/pengchao/Documents/Codeg_Fork/codex-acp` branch and its uncommitted customization.
- Produces: public fork commit `1.1.0-mycodebuddy.1` and a Codeg gitlink pinned to that exact commit.

- [ ] **Step 1: Create the public fork before changing remotes**

Create `icannotwait/codex-acp` as a public GitHub fork of `agentclientprotocol/codex-acp`. Verify read access:

```bash
git ls-remote https://github.com/icannotwait/codex-acp.git HEAD
```

Expected: exit 0 and one line containing a 40-character commit SHA followed by
`HEAD`. Do not continue while it returns `Repository not found`.

- [ ] **Step 2: Preserve and review the local fork work**

```bash
cd /Users/pengchao/Documents/Codeg_Fork/codex-acp
git status --short
git diff --check
npm run typecheck
npm test
```

Expected: `git diff --check`, typecheck, and Vitest all exit 0. Review every modified file shown by `git status`; do not discard any current customization.

- [ ] **Step 3: Assign the fork version**

Change both package manifests from `1.1.0` to the exact value:

```json
"version": "1.1.0-mycodebuddy.1"
```

Run:

```bash
npm install --package-lock-only --ignore-scripts
npm run build
node dist/index.js --version
```

Expected final output:

```text
@agentclientprotocol/codex-acp 1.1.0-mycodebuddy.1
```

- [ ] **Step 4: Commit the complete fork customization**

```bash
git add package.json package-lock.json src
git commit -m "feat: add MyCodeBuddy Codex CLI runtime"
```

Expected: the commit contains the current CLI runtime changes plus the fork version, while ignored `dist/` and `node_modules/` remain untracked from Git.

- [ ] **Step 5: Configure fork remotes and publish**

```bash
git remote rename origin upstream
git remote add origin https://github.com/icannotwait/codex-acp.git
git push -u origin codex/codex-acp-cli-runtime
```

Expected:

```bash
git remote get-url origin
# https://github.com/icannotwait/codex-acp.git
git remote get-url upstream
# https://github.com/agentclientprotocol/codex-acp.git
```

- [ ] **Step 6: Add the public submodule to Codeg**

```bash
cd /Users/pengchao/Documents/Codeg_Fork/codeg
git submodule add -b codex/codex-acp-cli-runtime \
  https://github.com/icannotwait/codex-acp.git \
  src-tauri/vendor/codex-acp
git submodule status src-tauri/vendor/codex-acp
```

Expected: output begins with a commit SHA and ends with `(heads/codex/codex-acp-cli-runtime)`; it must not begin with `-`.

- [ ] **Step 7: Commit the pinned source dependency**

```bash
git add .gitmodules src-tauri/vendor/codex-acp
git commit -m "build: pin customized codex-acp fork"
```

---

### Task 2: Add A Bundled-Agent Registry And Executable Resolver

**Files:**
- Create: `src-tauri/src/acp/bundled_agent.rs`
- Modify: `src-tauri/src/acp/mod.rs`
- Modify: `src-tauri/src/acp/registry.rs`
- Test: `src-tauri/src/acp/bundled_agent.rs`
- Test: `src-tauri/src/acp/registry.rs`

**Interfaces:**
- Consumes: installed sibling name `codex-acp.exe` and optional `CODEG_CODEX_ACP_BIN`.
- Produces: `AgentDistribution::Bundled`, `locate_bundled_executable(cmd, override_env_key) -> Result<Option<PathBuf>, AcpError>`, and registry metadata used by connection/status/preflight.

- [ ] **Step 1: Write failing resolver tests**

Add tests around a pure candidate selector in `bundled_agent.rs`:

```rust
#[test]
fn explicit_override_wins_over_sibling_and_path() {
    let temp = tempfile::tempdir().unwrap();
    let explicit = executable_fixture(temp.path(), "explicit.exe");
    let sibling = executable_fixture(temp.path(), "sibling.exe");
    let path = executable_fixture(temp.path(), "path.exe");
    assert_eq!(
        select_bundled_executable(Some(&explicit), Some(sibling), Some(path)).unwrap(),
        Some(explicit)
    );
}

#[test]
fn invalid_explicit_override_is_an_error() {
    let missing = PathBuf::from("Z:/missing/codex-acp.exe");
    let error = select_bundled_executable(Some(&missing), None, None).unwrap_err();
    assert!(error.to_string().contains("CODEG_CODEX_ACP_BIN"));
}
```

Also cover sibling-before-PATH and `Ok(None)` when no candidates exist.

- [ ] **Step 2: Run the focused Rust test and confirm failure**

```bash
cd src-tauri
cargo test --features test-utils bundled_agent --lib
```

Expected: FAIL because `bundled_agent` and its selector do not exist.

- [ ] **Step 3: Implement the focused resolver module**

Implement these public surfaces:

```rust
pub const CODEX_ACP_OVERRIDE_ENV: &str = "CODEG_CODEX_ACP_BIN";

pub fn locate_bundled_executable(
    cmd: &str,
    override_env_key: &str,
) -> Result<Option<PathBuf>, AcpError> {
    let explicit = std::env::var_os(override_env_key).map(PathBuf::from);
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(platform_filename(cmd))));
    let on_path = which::which(platform_filename(cmd)).ok();
    select_bundled_executable(explicit.as_deref(), sibling, on_path)
}
```

`select_bundled_executable` must return an error when an explicit path was supplied but is not an executable file. Without an explicit path it returns the executable sibling, then executable PATH result, then `None`. On Unix, executable means a regular file with at least one execute bit; on Windows, it means a regular file.

- [ ] **Step 4: Add the bundled distribution and platform-aware Codex recipe**

Add the variant:

```rust
Bundled {
    version: &'static str,
    cmd: &'static str,
    args: &'static [&'static str],
    env: &'static [(&'static str, &'static str)],
    override_env: &'static str,
    platforms: &'static [&'static str],
},
```

Include `Bundled` in `registry_version()`. Extract Codex recipe construction into `codex_distribution_for(platform: &str)`. For `windows-x86_64`, return:

```rust
AgentDistribution::Bundled {
    version: "1.1.0-mycodebuddy.1",
    cmd: "codex-acp",
    args: &[],
    env: &[
        ("CODEX_ACP_USE_CLI", "1"),
        ("CODEX_ACP_CLI_MODEL", "gpt-5.5"),
    ],
    override_env: "CODEG_CODEX_ACP_BIN",
    platforms: &["windows-x86_64"],
}
```

For every other platform, return the existing official `Npx` recipe unchanged. `get_agent_meta` calls this helper with `current_platform()`.

- [ ] **Step 5: Add cross-platform registry tests**

```rust
#[test]
fn codex_is_bundled_only_on_windows_x64() {
    assert!(matches!(
        codex_distribution_for("windows-x86_64"),
        AgentDistribution::Bundled {
            version: "1.1.0-mycodebuddy.1",
            override_env: "CODEG_CODEX_ACP_BIN",
            ..
        }
    ));
    assert!(matches!(
        codex_distribution_for("darwin-aarch64"),
        AgentDistribution::Npx {
            package: "@agentclientprotocol/codex-acp@1.1.2",
            ..
        }
    ));
}
```

Update the existing registry pin test so Codex is tested through this helper rather than `assert_npx_version` on the host platform.

- [ ] **Step 6: Run tests and commit**

```bash
cargo test --features test-utils bundled_agent --lib
cargo test --features test-utils registry --lib
git add src-tauri/src/acp/bundled_agent.rs src-tauri/src/acp/mod.rs src-tauri/src/acp/registry.rs
git commit -m "feat(acp): define bundled Codex adapter"
```

Expected: both focused test commands pass.

---

### Task 3: Launch And Report The Bundled Adapter Without npm

**Files:**
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/preflight.rs`
- Modify: `src-tauri/src/commands/acp.rs`
- Test: `src-tauri/src/acp/connection.rs`
- Test: `src-tauri/src/acp/preflight.rs`
- Test: `src-tauri/src/commands/acp.rs`

**Interfaces:**
- Consumes: `AgentDistribution::Bundled` and `locate_bundled_executable` from Task 2.
- Produces: bundled spawn behavior, `distribution_type = "bundled"`, installed fork version, and no npm mutation path.

The connection helpers introduced by this task have these exact signatures:

```rust
fn ensure_current_platform(
    platforms: &[&str],
    agent_name: &str,
) -> Result<(), AcpError>;

fn build_stdio_agent(
    agent_name: &str,
    executable: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    runtime_env: &BTreeMap<String, String>,
) -> Result<AcpAgent, AcpError>;

async fn check_bundled_environment(
    cmd: &str,
    override_env: &str,
    platforms: &[&str],
) -> Vec<CheckItem>;
```

- [ ] **Step 1: Add failing backend behavior tests**

Add tests named `bundled_codex_status_does_not_require_npm`,
`bundled_codex_preflight_uses_executable`, and
`bundled_codex_rejects_agent_management`, asserting:

```rust
assert_eq!(codex_info.distribution_type, "bundled");
assert_eq!(codex_info.registry_version.as_deref(), Some("1.1.0-mycodebuddy.1"));
assert_eq!(codex_info.installed_version.as_deref(), Some("1.1.0-mycodebuddy.1"));
```

Use a temporary executable through `CODEG_CODEX_ACP_BIN` under the repository's existing serialized environment-test guard. Exercise a factored status-projection helper with `codex_distribution_for("windows-x86_64")`, so the test remains valid when run on a macOS host. Add a preflight test asserting its checks contain `bundled_executable` and contain neither `node_available` nor `npm_available`. Add command tests asserting prepare, download, and uninstall reject `Bundled` with `"bundled agents are updated with MyCodeBuddy"`.

- [ ] **Step 2: Run focused tests and confirm failure**

```bash
cd src-tauri
cargo test --features test-utils bundled_codex --lib
```

Expected: FAIL because current exhaustive matches do not handle `Bundled` and Codex still reports `npx`.

- [ ] **Step 3: Add the bundled spawn branch**

In `build_agent`, resolve the executable and construct `McpServerStdio` with the same argument/environment application used by existing distributions:

```rust
AgentDistribution::Bundled {
    cmd,
    args,
    env,
    override_env,
    platforms,
    ..
} => {
    ensure_current_platform(platforms, meta.name)?;
    let path = bundled_agent::locate_bundled_executable(cmd, override_env)?
        .ok_or_else(|| AcpError::SdkNotInstalled(format!(
            "Bundled {} executable is missing; reinstall or update MyCodeBuddy.",
            meta.name
        )))?;
    build_stdio_agent(meta.name, &path, args, env, runtime_env)?
}
```

Extract only the duplicated stdio construction needed to share argument/env behavior; do not refactor unrelated Npx/Binary/Uvx logic. Existing `merge_agent_env` ensures runtime environment values override bundled defaults.

- [ ] **Step 4: Add bundled preflight and status behavior**

`run_preflight` calls `check_bundled_environment(cmd, override_env, platforms)`. It returns:

- `platform_supported` pass/fail;
- `bundled_executable` pass when resolution succeeds;
- `bundled_executable` fail with the resolver error for an invalid explicit override;
- `bundled_executable` fail with reinstall/update wording when no executable exists.

In status/list/detection paths, resolve the executable without invoking npm. When present, set `available = true`, distribution type `bundled`, and installed version to the registry version. When absent, set `available = false` and installed version `None`. Do not write a bundled version into the binary cache.

- [ ] **Step 5: Close every management match arm explicitly**

Add `Bundled` cases to download, prepare, uninstall, custom-version, and version-detection matches. Each mutation command returns:

```rust
Err(AcpError::protocol(
    "bundled agents are updated with MyCodeBuddy",
))
```

Read-only listing and preflight paths must never call Node/npm for `Bundled`.

- [ ] **Step 6: Run backend regression tests and commit**

```bash
cargo test --features test-utils bundled_codex --lib
cargo test --features test-utils acp::registry --lib
cargo test --features test-utils acp::preflight --lib
cargo test --features test-utils commands::acp --lib
git add src-tauri/src/acp/connection.rs src-tauri/src/acp/preflight.rs src-tauri/src/commands/acp.rs
git commit -m "feat(acp): launch bundled Codex adapter"
```

Expected: all four commands pass and compiler exhaustiveness confirms every distribution match is handled.

---

### Task 4: Render Bundled Codex As Read-Only In Settings

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/components/settings/acp-agent-settings.tsx`
- Modify: `src/components/settings/acp-agent-settings.test.tsx`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: the other locale JSON files under `src/i18n/messages/`

**Interfaces:**
- Consumes: backend `distribution_type = "bundled"` and installed/registry version `1.1.0-mycodebuddy.1`.
- Produces: a pass/fail version check with no npm management actions and localized built-in labeling.

- [ ] **Step 1: Write the failing UI unit tests**

Add:

```ts
it("shows bundled Codex as built in without management actions", () => {
  const check = buildVersionCheck(
    makeAgent({
      agent_type: "codex" as AgentType,
      distribution_type: "bundled",
      available: true,
      registry_version: "1.1.0-mycodebuddy.1",
      installed_version: "1.1.0-mycodebuddy.1",
    })
  )
  expect(check?.status).toBe("pass")
  expect(check?.fixes).toEqual([])
  expect(check?.message).toContain("built in")
})

it("reports a missing bundled Codex without install actions", () => {
  const check = buildVersionCheck(
    makeAgent({ distribution_type: "bundled", available: false, installed_version: null })
  )
  expect(check?.status).toBe("fail")
  expect(check?.fixes).toEqual([])
})
```

- [ ] **Step 2: Run the test and confirm failure**

```bash
pnpm test src/components/settings/acp-agent-settings.test.tsx
```

Expected: FAIL because `buildVersionCheck` currently returns `null` for `bundled`.

- [ ] **Step 3: Implement bundled version-check rendering**

Document `"bundled"` as a backend-produced `AcpAgentInfo.distribution_type`
value without narrowing the field from `string` (existing tests also use
synthetic `npm` and `system` values). Handle it before install-action
construction:

```ts
if (agent.distribution_type === "bundled") {
  return {
    check_id: "version_status",
    label: acpText("version.statusLabel", "Version Status"),
    status: agent.available ? "pass" : "fail",
    message: agent.available
      ? acpText("version.bundled", "Built in: {version}", { version: localVersion })
      : acpText("version.bundledMissing", "Built-in adapter is missing. Reinstall or update MyCodeBuddy."),
    fixes: [],
  }
}
```

Ensure action dispatch guards reject `bundled` if an obsolete UI event reaches them. Display `Built in` rather than the raw distribution token in the agent detail panel.

- [ ] **Step 4: Add all locale keys**

Add `version.bundled` and `version.bundledMissing` under the existing ACP settings namespace in every locale file. Exact English copy is shown above; exact simplified Chinese copy is:

```json
"bundled": "内置版本：{version}",
"bundledMissing": "内置适配器缺失，请重新安装或更新 MyCodeBuddy。"
```

Use accurate translations for the remaining existing locales and preserve valid JSON.

- [ ] **Step 5: Verify and commit**

```bash
pnpm test src/components/settings/acp-agent-settings.test.tsx
pnpm eslint src/components/settings/acp-agent-settings.tsx src/components/settings/acp-agent-settings.test.tsx src/lib/types.ts
git add src/lib/types.ts src/components/settings/acp-agent-settings.tsx src/components/settings/acp-agent-settings.test.tsx src/i18n/messages
git commit -m "feat(settings): show bundled Codex adapter"
```

Expected: Vitest and ESLint exit 0.

---

### Task 5: Build And Bundle The Windows x64 Sidecar

**Files:**
- Modify: `src-tauri/scripts/prepare-sidecars.mjs`
- Create: `src-tauri/scripts/prepare-sidecars.test.mjs`
- Create: `src-tauri/scripts/smoke-codex-acp.mjs`
- Modify: `src-tauri/tauri.release.conf.json`
- Modify: `src-tauri/build.rs`
- Modify: `.github/workflows/release.yml`
- Modify: `scripts/release-policy.mjs`
- Modify: `scripts/release-policy.test.mjs`
- Modify: `scripts/third-party-licenses.mjs`
- Modify: `scripts/third-party-licenses.test.mjs`

**Interfaces:**
- Consumes: initialized submodule at `src-tauri/vendor/codex-acp` and its `bundle:win-x64` script.
- Produces: `src-tauri/binaries/codex-acp-x86_64-pc-windows-msvc.exe` and a release installer containing sibling `codex-acp.exe`.

- [ ] **Step 1: Write failing sidecar script tests**

Export pure helpers from `prepare-sidecars.mjs` and test:

```js
assert.equal(codexBundleScript("x86_64-pc-windows-msvc"), "bundle:win-x64")
assert.equal(codexBundleScript("aarch64-pc-windows-msvc"), null)
assert.equal(
  sidecarDestination("codex-acp", "x86_64-pc-windows-msvc"),
  "codex-acp-x86_64-pc-windows-msvc.exe"
)
```

Also test that submodule validation rejects a missing `package-lock.json` and that expected version parsing returns `1.1.0-mycodebuddy.1`.

Guard the script entry point so importing helpers does not execute a build:

```js
if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main()
}
```

- [ ] **Step 2: Run script tests and confirm failure**

```bash
node --test src-tauri/scripts/prepare-sidecars.test.mjs
```

Expected: FAIL because the exported codex helpers do not exist.

- [ ] **Step 3: Extend sidecar preparation**

Keep the existing codeg-mcp build. For `x86_64-pc-windows-msvc`, additionally run in `src-tauri/vendor/codex-acp`:

```text
npm ci
npm run typecheck
npm test
npm run bundle:win-x64
```

Copy `dist/bin/codex-acp-x64-windows.exe` to `src-tauri/binaries/codex-acp-x86_64-pc-windows-msvc.exe`. Delete any existing destination before building so stale output cannot pass. Validate non-zero size and execute `--version`; require exact stdout `@agentclientprotocol/codex-acp 1.1.0-mycodebuddy.1`.

Add `CODEG_SKIP_CODEX_ACP_SIDECAR=1` only for local diagnostic builds. The release policy must reject this variable in release workflow steps.

- [ ] **Step 4: Add the release-only Tauri sidecar**

Set the release config bundle array explicitly so config-array replacement retains both binaries:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "bundle": {
    "createUpdaterArtifacts": true,
    "externalBin": ["binaries/codeg-mcp", "binaries/codex-acp"]
  }
}
```

Extend `build.rs` placeholder handling to create/check both target-qualified sidecars when the merged release configuration requests codex-acp. The production workflow must overwrite placeholders, and its explicit verification rejects zero-byte files.

- [ ] **Step 5: Make the release workflow x64-only and submodule-aware**

Configure checkout:

```yaml
- uses: actions/checkout@v4
  with:
    submodules: recursive
```

Remove the Windows ARM64 matrix entry. Pin Bun with:

```yaml
- uses: oven-sh/setup-bun@v2
  with:
    bun-version: 1.3.14
```

Extend the staging verification PowerShell block to check both:

```powershell
$sidecars = @(
  "src-tauri/binaries/codeg-mcp-x86_64-pc-windows-msvc.exe",
  "src-tauri/binaries/codex-acp-x86_64-pc-windows-msvc.exe"
)
foreach ($sidecar in $sidecars) {
  if (-not (Test-Path -LiteralPath $sidecar -PathType Leaf)) { throw "Missing staged sidecar: $sidecar" }
  if ((Get-Item -LiteralPath $sidecar).Length -le 0) { throw "Staged sidecar is empty: $sidecar" }
}
```

Invoke `smoke-codex-acp.mjs` after staging. It must first run
`codex-acp.exe cli --version` and require a successful Codex CLI version output;
this specifically proves the platform-specific `@openai/codex` executable was
embedded. It then starts `codex-acp.exe`, sends ACP initialize and session/new
requests through stdin, receives successful JSON-RPC responses, and closes
stdin. Supply `CODEX_ACP_USE_CLI=1`, `CODEX_ACP_CLI_MODEL=gpt-5.5`, and an
isolated temporary `CODEX_HOME`; do not send a model prompt or require
credentials. Kill the child and fail with its captured stderr after 20 seconds.

- [ ] **Step 6: Extend release and license policy tests**

Require all of the following in `assertWindowsReleaseWorkflow` and its tests:

- exactly `x86_64-pc-windows-msvc` for desktop release;
- recursive submodule checkout;
- exact Bun `1.3.14` setup;
- codex sidecar staging and non-zero verification;
- no `CODEG_SKIP_CODEX_ACP_SIDECAR` in release steps;
- release config lists both external binaries.

Extend license generation to read the fork `LICENSE`, `NOTICE.md`, package manifest, and locked production dependency graph. Add a fixture test proving the generated report contains `@agentclientprotocol/codex-acp 1.1.0-mycodebuddy.1`, its Apache-2.0 text reference, and embedded production dependencies.

- [ ] **Step 7: Run build-policy tests and commit**

```bash
node --test src-tauri/scripts/prepare-sidecars.test.mjs
pnpm test:release
pnpm licenses:generate
git diff --check
git add src-tauri/scripts src-tauri/tauri.release.conf.json src-tauri/build.rs src-tauri/resources/THIRD_PARTY_LICENSES.txt .github/workflows/release.yml scripts
git commit -m "build: bundle customized codex-acp on Windows"
```

Expected: all Node tests pass and the generated third-party report includes the fork.

---

### Task 6: Full Verification And Release Acceptance

**Files:**
- Modify if results require corrections: files already listed in Tasks 2-5
- Create: `docs/releasing/bundled-codex-acp.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: verified x64 NSIS installer and documented upstream/update procedure.

- [ ] **Step 1: Document adapter maintenance**

Add exact commands for:

```bash
cd src-tauri/vendor/codex-acp
git fetch upstream
git merge upstream/main
npm ci
npm run typecheck
npm test
git push origin codex/codex-acp-cli-runtime
cd ../../..
git add src-tauri/vendor/codex-acp
git commit -m "chore: update bundled codex-acp"
```

Document that each adapter change increments `1.1.0-mycodebuddy.N` and that MyCodeBuddy updates, not Agent Settings, distribute it.

- [ ] **Step 2: Run complete repository verification**

```bash
pnpm test
pnpm eslint .
pnpm build
pnpm test:release
cd src-tauri
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
```

Expected: every command exits 0. Record any unrelated pre-existing failure separately; do not weaken checks to hide it.

- [ ] **Step 3: Build the Windows x64 NSIS installer**

On the Windows release runner:

```powershell
pnpm tauri:prepare-sidecars --target x86_64-pc-windows-msvc
pnpm tauri build --config src-tauri/tauri.release.conf.json --target x86_64-pc-windows-msvc --bundles nsis
```

Expected: one NSIS setup executable under `src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/`; no separate codex-acp release asset.

- [ ] **Step 4: Verify on a clean Windows x64 VM**

Before install, confirm:

```powershell
Get-Command node -ErrorAction SilentlyContinue
Get-Command codex-acp -ErrorAction SilentlyContinue
```

Expected: both return no command. Install MyCodeBuddy, confirm `codex-acp.exe` is beside `MyCodeBuddy.exe`, start a Codex session, and inspect logs for the absolute sibling path and version `1.1.0-mycodebuddy.1`.

- [ ] **Step 5: Verify bundled precedence and Defender behavior**

Install official global codex-acp, restart MyCodeBuddy, and start another session:

```powershell
npm install -g @agentclientprotocol/codex-acp@1.1.2
Get-Command codex-acp
```

Expected: Codeg logs still show the sibling bundled executable, not the npm path. Run a Microsoft Defender custom scan over the install directory and record whether the Bun executable is quarantined. Treat quarantine as a release blocker requiring signing or packaging adjustment.

- [ ] **Step 6: Commit release documentation and final corrections**

```bash
git add docs/releasing src-tauri src scripts .github/workflows/release.yml
git commit -m "docs: document bundled Codex release flow"
git status --short
```

Expected: only unrelated user-owned changes remain in `git status`.
