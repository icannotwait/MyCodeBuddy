# MyCodeBuddy Windows Fork Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `icannotwait/MyCodeBuddy` a license-compliant, Windows-only GitHub Release fork with its own updater channel and signing key, without breaking local macOS builds.

**Architecture:** Keep runtime and local desktop support cross-platform, but replace the tag-triggered release workflow with Windows desktop x64/ARM64 and Windows server x64 jobs. Centralize fork identity and release-policy checks in Node scripts, generate a deterministic bundled third-party license report, and document a two-remote merge-based upstream sync process.

**Tech Stack:** Tauri 2.10, Rust/Cargo, Next.js 16, pnpm 11, Node.js 24 built-in test runner, GitHub Actions, NSIS, Tauri updater/minisign.

## Global Constraints

- Fork repository is exactly `icannotwait/MyCodeBuddy`.
- GitHub Release automation publishes Windows artifacts only.
- Local `pnpm tauri dev` and secret-free `pnpm tauri build --bundles app` on macOS must remain supported.
- Windows installers are not Authenticode-signed; Tauri updater artifacts must be signed.
- Updater artifacts are disabled in the default Tauri config and enabled only through `src-tauri/tauri.release.conf.json`.
- The private updater key and password must never enter the repository or command output.
- Versions must use `MAJOR.MINOR.PATCH-mycodebuddy.COUNTER`, starting with `0.18.8-mycodebuddy.1`.
- Package, Cargo, Tauri, and tag versions must match exactly.
- Completed `-mycodebuddy.` builds are published as non-prerelease GitHub Releases so `/releases/latest` works.
- Existing unrelated working-tree changes must not be staged, reverted, reformatted, or committed.

---

### Task 1: Release Policy Library

**Files:**
- Create: `scripts/release-policy.mjs`
- Create: `scripts/release-policy.test.mjs`
- Create: `scripts/check-release-config.mjs`
- Modify: `package.json`

**Interfaces:**
- Produces: `readCargoVersion(text) -> string`
- Produces: `assertMatchingVersions({ packageVersion, cargoVersion, tauriVersion, tag? })`
- Produces: `assertForkVersion(version)`
- Produces: `findForbiddenRuntimeUrls(files) -> string[]`
- Produces: `assertWindowsReleaseWorkflow(workflowText)`
- Produces: `assertComplianceResources(tauriConfig)`
- Consumes: Node.js built-ins only.

- [ ] **Step 1: Write failing unit tests**

Create `scripts/release-policy.test.mjs` with Node's built-in test runner. Cover:

```js
import assert from "node:assert/strict"
import test from "node:test"
import {
  assertComplianceResources,
  assertForkVersion,
  assertMatchingVersions,
  assertWindowsReleaseWorkflow,
  findForbiddenRuntimeUrls,
  readCargoVersion,
} from "./release-policy.mjs"

test("reads the package version from Cargo.toml", () => {
  assert.equal(
    readCargoVersion('[package]\nname = "codeg"\nversion = "0.18.8-mycodebuddy.1"\n'),
    "0.18.8-mycodebuddy.1"
  )
})

test("requires the MyCodeBuddy version suffix", () => {
  assert.doesNotThrow(() => assertForkVersion("0.18.8-mycodebuddy.1"))
  assert.throws(() => assertForkVersion("0.18.8"), /mycodebuddy/)
})

test("requires package Cargo Tauri and tag versions to match", () => {
  const version = "0.18.8-mycodebuddy.1"
  assert.doesNotThrow(() =>
    assertMatchingVersions({
      packageVersion: version,
      cargoVersion: version,
      tauriVersion: version,
      tag: `v${version}`,
    })
  )
  assert.throws(
    () =>
      assertMatchingVersions({
        packageVersion: version,
        cargoVersion: "0.18.8",
        tauriVersion: version,
      }),
    /version mismatch/
  )
})

test("finds upstream URLs in runtime-owned files", () => {
  assert.deepEqual(
    findForbiddenRuntimeUrls({
      "tauri.conf.json":
        "https://github.com/xintaofei/codeg/releases/latest/download/latest.json",
      "NOTICE": "Based on https://github.com/xintaofei/codeg",
    }),
    ["tauri.conf.json"]
  )
})

test("requires Windows desktop targets and rejects non-Windows release jobs", () => {
  const good = `
    target: x86_64-pc-windows-msvc
    target: aarch64-pc-windows-msvc
    runner: windows-2022
  `
  assert.doesNotThrow(() => assertWindowsReleaseWorkflow(good))
  assert.throws(
    () => assertWindowsReleaseWorkflow(`${good}\ntarget: x86_64-apple-darwin`),
    /non-Windows/
  )
})

test("requires bundled compliance resources", () => {
  assert.doesNotThrow(() =>
    assertComplianceResources({
      bundle: {
        license: "Apache-2.0",
        licenseFile: "../LICENSE",
        resources: {
          "../LICENSE": "licenses/LICENSE",
          "../NOTICE": "licenses/NOTICE",
          "resources/THIRD_PARTY_LICENSES.txt":
            "licenses/THIRD_PARTY_LICENSES.txt",
        },
      },
    })
  )
})
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
node --test scripts/release-policy.test.mjs
```

Expected: FAIL with `ERR_MODULE_NOT_FOUND` for `scripts/release-policy.mjs`.

- [ ] **Step 3: Implement the release-policy module**

Create `scripts/release-policy.mjs`. Use:

```js
const FORK_VERSION_RE = /^\d+\.\d+\.\d+-mycodebuddy\.\d+$/
const UPSTREAM_REPO_RE =
  /https:\/\/(?:github\.com|raw\.githubusercontent\.com)\/xintaofei\/codeg/gi
const ALLOWED_UPSTREAM_FILES = new Set(["NOTICE", "docs/UPSTREAM_SYNC.md"])

export function readCargoVersion(text) {
  const packageBlock = text.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1]
  const version = packageBlock?.match(/^\s*version\s*=\s*"([^"]+)"/m)?.[1]
  if (!version) throw new Error("Cargo package version not found")
  return version
}

export function assertForkVersion(version) {
  if (!FORK_VERSION_RE.test(version)) {
    throw new Error(
      `version must match MAJOR.MINOR.PATCH-mycodebuddy.COUNTER: ${version}`
    )
  }
}

export function assertMatchingVersions({
  packageVersion,
  cargoVersion,
  tauriVersion,
  tag,
}) {
  const versions = new Set([packageVersion, cargoVersion, tauriVersion])
  if (versions.size !== 1) {
    throw new Error(
      `version mismatch: package=${packageVersion}, cargo=${cargoVersion}, tauri=${tauriVersion}`
    )
  }
  assertForkVersion(packageVersion)
  if (tag && tag !== `v${packageVersion}`) {
    throw new Error(`tag ${tag} does not match v${packageVersion}`)
  }
}

export function findForbiddenRuntimeUrls(files) {
  return Object.entries(files)
    .filter(([name, text]) => {
      UPSTREAM_REPO_RE.lastIndex = 0
      return !ALLOWED_UPSTREAM_FILES.has(name) && UPSTREAM_REPO_RE.test(text)
    })
    .map(([name]) => name)
    .sort()
}

export function assertWindowsReleaseWorkflow(workflowText) {
  for (const target of [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
  ]) {
    if (!workflowText.includes(target)) {
      throw new Error(`missing Windows target ${target}`)
    }
  }
  for (const forbidden of [
    "apple-darwin",
    "unknown-linux",
    "APPLE_CERTIFICATE",
    "DOCKERHUB_",
    "build-docker",
  ]) {
    if (workflowText.includes(forbidden)) {
      throw new Error(`release workflow contains non-Windows entry ${forbidden}`)
    }
  }
}

export function assertComplianceResources(tauriConfig) {
  const bundle = tauriConfig.bundle ?? {}
  const resources = bundle.resources ?? {}
  const expected = {
    "../LICENSE": "licenses/LICENSE",
    "../NOTICE": "licenses/NOTICE",
    "resources/THIRD_PARTY_LICENSES.txt":
      "licenses/THIRD_PARTY_LICENSES.txt",
  }
  if (bundle.license !== "Apache-2.0") {
    throw new Error("bundle.license must be Apache-2.0")
  }
  if (bundle.licenseFile !== "../LICENSE") {
    throw new Error("bundle.licenseFile must be ../LICENSE")
  }
  for (const [source, target] of Object.entries(expected)) {
    if (resources[source] !== target) {
      throw new Error(`missing compliance resource ${source} -> ${target}`)
    }
  }
}
```

- [ ] **Step 4: Add the repository checker CLI**

Create `scripts/check-release-config.mjs`. It must:

1. read `package.json`, `src-tauri/Cargo.toml`,
   `src-tauri/tauri.conf.json`, `src-tauri/tauri.release.conf.json`,
   `.github/workflows/release.yml`,
   `src-tauri/src/update/version.rs`,
   `src/components/settings/system-network-settings.tsx`, and `install.ps1`;
2. accept optional `--tag v0.18.8-mycodebuddy.1`;
3. invoke all exported assertions;
4. print `Release configuration is valid.` on success.

Use `fileURLToPath(import.meta.url)` to resolve the repository root so the
script works from any current directory.

- [ ] **Step 5: Add package scripts**

Add:

```json
"test:release": "node --test scripts/*.test.mjs",
"release:check": "node scripts/check-release-config.mjs"
```

- [ ] **Step 6: Run tests and commit**

Run:

```bash
pnpm test:release
```

Expected: unit tests PASS. Do not run `release:check` yet because the repository
still contains the old release configuration.

Commit only:

```bash
git add package.json scripts/release-policy.mjs scripts/release-policy.test.mjs scripts/check-release-config.mjs
git commit -m "test(release): add fork policy checks"
```

---

### Task 2: Fork Identity, Version, And Documentation

**Files:**
- Modify: `package.json`
- Modify: `src-tauri/Cargo.toml`
- Modify: `src-tauri/Cargo.lock`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `src-tauri/src/update/version.rs`
- Modify: `src/components/settings/system-network-settings.tsx`
- Modify: `install.ps1`
- Delete: `install.sh`
- Modify: `README.md`
- Modify: `docs/readme/README.*.md`
- Modify: `docs/CLIENT-PRIVACY.md`
- Modify: `docker-compose.yml`
- Create: `docs/UPSTREAM_SYNC.md`
- Modify: `scripts/release-policy.test.mjs`

**Interfaces:**
- Consumes: release-policy assertions from Task 1.
- Produces: runtime update and download URLs rooted at
  `https://github.com/icannotwait/MyCodeBuddy`.
- Produces: application version `0.18.8-mycodebuddy.1`.

- [ ] **Step 1: Add a failing repository integration test**

Extend `scripts/release-policy.test.mjs` with a test that reads the real
package, Cargo, Tauri, updater, settings, and installer files. Assert:

```js
const version = "0.18.8-mycodebuddy.1"
assertMatchingVersions({
  packageVersion: packageJson.version,
  cargoVersion: readCargoVersion(cargoToml),
  tauriVersion: tauriConfig.version,
})
assert.equal(packageJson.version, version)
assert.deepEqual(findForbiddenRuntimeUrls(runtimeFiles), [])
```

- [ ] **Step 2: Run the test and verify RED**

Run:

```bash
pnpm test:release
```

Expected: FAIL because the current version is `0.18.8` and runtime URLs still
point to `xintaofei/codeg`.

- [ ] **Step 3: Update package identity and version**

Set the exact version in:

```text
package.json                 0.18.8-mycodebuddy.1
src-tauri/Cargo.toml         0.18.8-mycodebuddy.1
src-tauri/tauri.conf.json    0.18.8-mycodebuddy.1
```

Also add:

```json
"license": "Apache-2.0",
"repository": {
  "type": "git",
  "url": "https://github.com/icannotwait/MyCodeBuddy.git"
},
"homepage": "https://github.com/icannotwait/MyCodeBuddy"
```

to `package.json`, and add:

```toml
license = "Apache-2.0"
repository = "https://github.com/icannotwait/MyCodeBuddy"
homepage = "https://github.com/icannotwait/MyCodeBuddy"
```

to the Cargo `[package]` table.

Run:

```bash
cd src-tauri && cargo check --no-default-features --bin codeg-server
```

Expected: Cargo updates the root package entry in `Cargo.lock` to
`0.18.8-mycodebuddy.1`.

- [ ] **Step 4: Replace runtime-owned repository URLs**

Use exactly:

```text
https://github.com/icannotwait/MyCodeBuddy/releases/latest/download/latest.json
https://github.com/icannotwait/MyCodeBuddy/releases/latest/download
https://github.com/icannotwait/MyCodeBuddy/releases/latest
https://github.com/icannotwait/MyCodeBuddy
```

in Tauri updater config, Rust server updater constants, settings links, and
`install.ps1`. Set `$Repo = "icannotwait/MyCodeBuddy"` in the PowerShell
installer. Before creating or writing `InstallDir`, validate
`codeg-server.exe`, `codeg-mcp.exe`, `LICENSE`, `NOTICE`, and
`THIRD_PARTY_LICENSES.txt`; then copy all five files while preserving the web
asset copy behavior.

Delete `install.sh` because the fork no longer publishes Unix server artifacts.

- [ ] **Step 5: Update user-facing documentation**

Update the root README and every localized README so:

- release badges and release links point to `icannotwait/MyCodeBuddy`;
- the one-line Unix server installer section is removed;
- the prebuilt server table contains only
  `Windows x64 | codeg-server-windows-x64.zip`;
- the Windows PowerShell example points to the fork;
- Windows prebuilt upgrades require rerunning `install.ps1` or replacing files
  from the next Windows ZIP;
- Linux/macOS in-place updates are described only as local source-built
  behavior because this fork publishes no prebuilt Linux/macOS server assets;
- Docker instructions describe local `docker compose up -d` builds only and
  contain no `ghcr.io/xintaofei/codeg` image;
- the license section links to `LICENSE`;
- an attribution sentence links to the original Codeg repository.

Update `docs/CLIENT-PRIVACY.md` to use the MyCodeBuddy name and
`https://github.com/icannotwait/MyCodeBuddy/issues`.

Remove the stale prebuilt-image comment from `docker-compose.yml`.

- [ ] **Step 6: Add upstream synchronization documentation**

Create `docs/UPSTREAM_SYNC.md` containing the exact remote setup:

```bash
git remote rename origin upstream
git remote add origin https://github.com/icannotwait/MyCodeBuddy.git
git fetch --all --prune
git config rerere.enabled true
```

Document the sync flow:

```bash
git fetch upstream
git switch main
git pull --ff-only origin main
git switch -c sync/codeg-0.18.9
git merge --no-ff upstream/main
```

Require a PR into MyCodeBuddy `main`, full verification before merge, no rebase
of published history, and version reset to `0.18.9-mycodebuddy.1` for the
example Codeg 0.18.9 sync.
Include conflict priorities for branding/updater files, deleted OpenClaw code,
release workflow, and functional fork changes.

- [ ] **Step 7: Verify and commit**

Run:

```bash
pnpm test:release
rg -n "xintaofei/codeg|ghcr.io/xintaofei" \
  README.md docs/readme docs/CLIENT-PRIVACY.md install.ps1 \
  src-tauri/tauri.conf.json src-tauri/src/update/version.rs \
  src/components/settings/system-network-settings.tsx docker-compose.yml
```

Expected: tests PASS. `rg` may match only explicit upstream attribution links
in README files; it must not match runtime update, download, install, support,
or image URLs.

Commit only the files listed in this task:

```bash
git commit -m "chore: establish MyCodeBuddy release identity"
```

---

### Task 3: Deterministic Third-Party License Report

**Files:**
- Create: `scripts/third-party-licenses.mjs`
- Create: `scripts/third-party-licenses.test.mjs`
- Modify: `package.json`
- Create: `src-tauri/resources/THIRD_PARTY_LICENSES.txt`

**Interfaces:**
- Produces: `findLicenseFiles(packageDir) -> Array<{ name, text }>`
- Produces: `collectNpmPackages(pnpmReport) -> PackageRecord[]`
- Produces: `collectCargoPackages(cargoMetadata) -> PackageRecord[]`
- Produces: `collectCargoPackageUnion(cargoMetadataRecords) -> PackageRecord[]`
- Produces: `renderLicenseReport(records) -> string`
- Produces: CLI output at
  `src-tauri/resources/THIRD_PARTY_LICENSES.txt`.

- [ ] **Step 1: Write failing generator tests**

Use temporary fixture directories and assert:

- license filenames are found case-insensitively for `LICENSE`, `license`,
  `COPYING`, and `NOTICE.md`;
- npm and Cargo records are sorted by ecosystem, name, and version;
- absolute filesystem paths never appear in output;
- identical license texts are emitted once and reference all packages;
- a package with neither declaration nor license file throws;
- running the renderer twice returns byte-identical output.
- Cargo records present only on Windows ARM64 or macOS remain in the union;
- duplicate Cargo ecosystem/name/version records merge only equivalent
  declarations, homepages, and license texts, and conflicts throw.

Run:

```bash
node --test scripts/third-party-licenses.test.mjs
```

Expected: FAIL because the module does not exist.

- [ ] **Step 2: Implement package collection**

Create `scripts/third-party-licenses.mjs` using only Node built-ins.

`PackageRecord` is:

```js
{
  ecosystem: "npm" | "cargo",
  name: string,
  version: string,
  declaredLicense: string,
  homepage: string,
  licenseTexts: Array<{ name: string, text: string }>
}
```

Implementation requirements:

- run `pnpm licenses list --prod --json` from the repository root;
- run Cargo metadata from `src-tauri` for exactly
  `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`,
  `x86_64-apple-darwin`, and `aarch64-apple-darwin`;
- collect npm production metadata once and form a deterministic Cargo union;
- merge equivalent duplicate metadata and reject conflicting metadata;
- exclude the workspace root `codeg` package from third-party Cargo records;
- inspect each package directory for files matching
  `/^(license|licence|copying|notice)(\..+)?$/i`;
- normalize CRLF to LF and trim trailing whitespace;
- treat npm's `Unknown` value as an empty declaration but accept `khroma`
  because its lowercase `license` file is present;
- fail if both declaration and license text are absent;
- group identical text by SHA-256 and print each text once;
- omit generation timestamps and absolute paths.

- [ ] **Step 3: Add generation script**

Add:

```json
"licenses:generate": "node scripts/third-party-licenses.mjs"
```

Update `tauri:before-build` to:

```json
"tauri:before-build": "pnpm licenses:generate && pnpm build && pnpm tauri:prepare-sidecars"
```

- [ ] **Step 4: Verify RED-to-GREEN and generate the tracked report**

Run:

```bash
pnpm test:release
pnpm licenses:generate
cp src-tauri/resources/THIRD_PARTY_LICENSES.txt /tmp/licenses-first.txt
pnpm licenses:generate
cmp /tmp/licenses-first.txt src-tauri/resources/THIRD_PARTY_LICENSES.txt
```

Expected: tests PASS and `cmp` exits zero.

- [ ] **Step 5: Commit**

```bash
git add package.json scripts/third-party-licenses.mjs \
  scripts/third-party-licenses.test.mjs \
  src-tauri/resources/THIRD_PARTY_LICENSES.txt
git commit -m "build: generate bundled third-party licenses"
```

---

### Task 4: Attribution And Bundle Compliance Resources

**Files:**
- Create: `NOTICE`
- Modify: `src-tauri/tauri.conf.json`
- Modify: `scripts/release-policy.test.mjs`
- Modify: `README.md`

**Interfaces:**
- Consumes: generated license report from Task 3.
- Produces: Tauri resources under `licenses/`.

- [ ] **Step 1: Add a failing integration assertion**

Extend the real-repository release-policy test:

```js
assertComplianceResources(tauriConfig)
```

Also assert that `LICENSE`, `NOTICE`, and
`src-tauri/resources/THIRD_PARTY_LICENSES.txt` exist and are non-empty.

- [ ] **Step 2: Run and verify RED**

Run:

```bash
pnpm test:release
```

Expected: FAIL because `NOTICE`, `bundle.license`, `bundle.licenseFile`, and
the compliance resource mappings are missing.

- [ ] **Step 3: Add the NOTICE file**

Create root `NOTICE` with:

```text
MyCodeBuddy
Copyright 2026 MyCodeBuddy contributors

MyCodeBuddy is a modified distribution based on Codeg:
https://github.com/xintaofei/codeg

The original work and this distribution are provided under the Apache
License, Version 2.0. MyCodeBuddy contains modifications made by MyCodeBuddy
contributors. The Codeg name and other third-party names may be trademarks of
their respective owners. No endorsement by the original project is implied.

Third-party software notices are provided in THIRD_PARTY_LICENSES.txt.
```

- [ ] **Step 4: Configure Tauri bundle metadata and resources**

In `src-tauri/tauri.conf.json`, add:

```json
"publisher": "MyCodeBuddy contributors",
"homepage": "https://github.com/icannotwait/MyCodeBuddy",
"license": "Apache-2.0",
"licenseFile": "../LICENSE"
```

Merge these mappings into the existing `bundle.resources` object:

```json
"../LICENSE": "licenses/LICENSE",
"../NOTICE": "licenses/NOTICE",
"resources/THIRD_PARTY_LICENSES.txt": "licenses/THIRD_PARTY_LICENSES.txt"
```

Keep the existing `../out -> web/` mapping and all macOS icons/settings.

- [ ] **Step 5: Document distributed notices**

Add a short README paragraph stating that installed desktop bundles include
Apache-2.0, modification attribution, and generated third-party notices under
their resources directory.

- [ ] **Step 6: Verify and commit**

Run:

```bash
pnpm test:release
pnpm release:check
pnpm tauri info
```

Expected: compliance tests PASS. `release:check` may still fail only on the
old all-platform release workflow, which is handled in Task 5.

Commit:

```bash
git add NOTICE README.md src-tauri/tauri.conf.json \
  scripts/release-policy.test.mjs
git commit -m "docs: bundle license and attribution notices"
```

---

### Task 5: Windows-Only GitHub Release Workflow

**Files:**
- Replace: `.github/workflows/release.yml`
- Modify: `scripts/release-policy.test.mjs`
- Modify: `src-tauri/tauri.conf.json`
- Create: `src-tauri/tauri.release.conf.json`

**Interfaces:**
- Consumes: `pnpm release:check`, `pnpm licenses:generate`,
  `pnpm tauri:prepare-sidecars`.
- Produces: Windows x64/ARM64 NSIS and updater assets.
- Produces: `codeg-server-windows-x64.zip`, signature, and checksum.

- [ ] **Step 1: Add a failing workflow integration test**

Read `.github/workflows/release.yml` and invoke:

```js
assertWindowsReleaseWorkflow(workflowText)
```

Also assert:

```js
assert.match(workflowText, /MyCodeBuddy \$\{tag\}/)
assert.match(workflowText, /prerelease:\s*false/)
assert.match(workflowText, /codeg-server-windows-x64/)
assert.doesNotMatch(workflowText, /includeUpdaterJson:\s*false/)
```

- [ ] **Step 2: Run and verify RED**

Run:

```bash
pnpm test:release
```

Expected: FAIL because the workflow still contains Apple, Linux, and Docker
targets.

- [ ] **Step 3: Replace the workflow**

Keep the tag trigger and draft-release reuse behavior, but reduce jobs to:

```text
create-draft-release
build-desktop (matrix: Windows x64, Windows ARM64)
build-server (Windows x64)
publish-release
```

Required details:

- trigger tags with `v*.*.*-mycodebuddy.*`;
- run `node scripts/check-release-config.mjs --tag "$GITHUB_REF_NAME"` in the
  draft job;
- release name is `MyCodeBuddy ${tag}`;
- GitHub `prerelease` is always `false`;
- desktop matrix uses `windows-2022` for x64 and `windows-latest` for ARM64;
- install pnpm, Node 24, Rust stable, the matrix Rust target, and Rust cache;
- run `pnpm install --frozen-lockfile`;
- run `pnpm licenses:generate`;
- stage and verify both
  `codeg-mcp-x86_64-pc-windows-msvc.exe` and
  `codeg-mcp-aarch64-pc-windows-msvc.exe` in their matrix jobs;
- invoke `tauri-apps/tauri-action@v0.6.1` with
  `--config src-tauri/tauri.release.conf.json --target ${{ matrix.target }} --bundles nsis`;
- set default `bundle.createUpdaterArtifacts` to `false` and the release
  override to `true`;
- set `includeUpdaterJson: true`;
- pass only `GITHUB_TOKEN`, `TAURI_SIGNING_PRIVATE_KEY`, and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
- server job builds frontend, `codeg-server.exe`, and `codeg-mcp.exe`;
- server ZIP contains `web/`, `LICENSE`, `NOTICE`, and
  `THIRD_PARTY_LICENSES.txt`;
- server job runs `codeg-mcp.exe --help`, signs the ZIP, writes SHA-256, and
  uploads all three files;
- publish job depends on both build jobs and publishes only when both succeed.

Do not retain Apple, Linux, Docker, QEMU, cross-compiler, Docker Hub, or GHCR
steps.

- [ ] **Step 4: Verify**

Run:

```bash
pnpm test:release
pnpm release:check
```

Expected: PASS.

If `actionlint` is available, also run:

```bash
actionlint .github/workflows/release.yml
```

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml scripts/release-policy.test.mjs
git commit -m "ci: publish Windows-only MyCodeBuddy releases"
```

---

### Task 6: Generate And Install The Fork Updater Key

**Files:**
- Create: `scripts/prepare-updater-signing-key.mjs`
- Create: `scripts/prepare-updater-signing-key.test.mjs`
- Modify: `.gitignore`
- Modify: `src-tauri/tauri.conf.json` (generated public key only)
- Create: `docs/RELEASING_WINDOWS.md`

**Interfaces:**
- Produces private files under
  `~/.config/mycodebuddy/signing/`.
- Produces committed public key in Tauri updater config.
- Never prints private key or password.

- [ ] **Step 1: Write failing helper tests**

Test pure helpers:

```js
updatePublicKey(config, "public-key")
```

must update only `plugins.updater.pubkey`, and:

```js
buildSigningPaths("/home/test")
```

must return paths under `/home/test/.config/mycodebuddy/signing`.

Run:

```bash
node --test scripts/prepare-updater-signing-key.test.mjs
```

Expected: FAIL because the module does not exist.

- [ ] **Step 2: Implement the key helper**

The CLI must:

1. create `~/.config/mycodebuddy/signing` with mode `0700`;
2. refuse to overwrite an existing key;
3. create a 32-byte base64url password with `crypto.randomBytes`;
4. execute this Node call without logging `password`:

   ```js
   execFileSync(
     process.platform === "win32" ? "pnpm.cmd" : "pnpm",
     [
       "tauri",
       "signer",
       "generate",
       "--ci",
       "-p",
       password,
       "-w",
       paths.privateKey,
     ],
     { cwd: repoRoot, stdio: ["ignore", "ignore", "inherit"] }
   )
   ```

5. chmod private key and password file to `0600`;
6. update only the public key in `src-tauri/tauri.conf.json`;
7. write `local-build.env` containing the resolved
   `TAURI_SIGNING_PRIVATE_KEY_PATH` value and
   `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
8. write `GITHUB_SECRETS.md` outside the repository with GitHub UI field names
   and paths, but never duplicate the private key content into repository files;
9. print only the generated file paths and next-step names.

Add ignore rules for `updater-signing.key`, `updater-signing.key.pub`,
`local-build.env`, and `GITHUB_SECRETS.md` as defense in depth.

- [ ] **Step 3: Run tests and generate the real fork key**

Run:

```bash
pnpm test:release
node scripts/prepare-updater-signing-key.mjs
```

Expected:

- tests PASS;
- the Tauri public key differs from the upstream key;
- private material exists only under
  `~/.config/mycodebuddy/signing`;
- `git status --short` shows only the public config/script/doc changes.

- [ ] **Step 4: Add Windows release documentation**

Create `docs/RELEASING_WINDOWS.md` documenting:

- required GitHub secrets:
  `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
- where the local private key and password were generated;
- how to enter them through the GitHub repository Settings UI;
- that the installer is not Authenticode-signed and may trigger SmartScreen;
- release sequence:

  ```bash
  pnpm release:check
  git tag v0.18.8-mycodebuddy.1
  git push origin v0.18.8-mycodebuddy.1
  ```

- key backup and loss consequences;
- no release tag should be pushed until the secrets are configured.

- [ ] **Step 5: Verify secrets are absent and commit**

Run:

```bash
git grep -n "TAURI_SIGNING_PRIVATE_KEY=" -- . ':!docs/RELEASING_WINDOWS.md'
git status --short
pnpm test:release
```

Expected: no private key assignment or password appears in tracked files.

Commit:

```bash
git add .gitignore scripts/prepare-updater-signing-key.mjs \
  scripts/prepare-updater-signing-key.test.mjs \
  src-tauri/tauri.conf.json docs/RELEASING_WINDOWS.md
git commit -m "build: configure MyCodeBuddy updater signing"
```

---

### Task 7: Configure Git Remotes Safely

**Files:**
- No tracked file changes.
- Uses: `docs/UPSTREAM_SYNC.md`

**Interfaces:**
- Produces local `origin` and `upstream` remote configuration.

- [ ] **Step 1: Record the current remote and branch state**

Run:

```bash
git remote -v
git branch -vv
git status --short --branch
```

Confirm that no command in this task stages or modifies working-tree files.

- [ ] **Step 2: Configure remotes**

Run only if `origin` still points to `xintaofei/codeg`:

```bash
git remote rename origin upstream
git remote add origin https://github.com/icannotwait/MyCodeBuddy.git
git fetch --all --prune
git config rerere.enabled true
```

- [ ] **Step 3: Verify divergence without merging or pushing**

Run:

```bash
git remote -v
git log --oneline --left-right --graph origin/main...HEAD --max-count=40
git log --oneline --left-right --graph upstream/main...HEAD --max-count=40
```

Do not push `main`, create tags, or change branch tracking in this task. The
current implementation branch is pushed only after the user chooses direct
push versus PR.

---

### Task 8: Full Verification And Local macOS Bundle

**Files:**
- Modify only files required to fix failures introduced by Tasks 1-7.

- [ ] **Step 1: Run release and frontend verification**

```bash
pnpm licenses:generate
git diff --exit-code -- src-tauri/resources/THIRD_PARTY_LICENSES.txt
pnpm test:release
pnpm release:check
pnpm eslint .
pnpm test
pnpm build
```

Expected: every command exits zero.

- [ ] **Step 2: Run Rust verification**

From `src-tauri/`:

```bash
cargo test --features test-utils
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --all-targets --features test-utils -- -D warnings
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
```

Expected: all tests and clippy checks PASS.

- [ ] **Step 3: Build a local macOS app bundle**

Ensure all Tauri signing variables are unset. Do not source
`local-build.env`, then run the exact local command:

```bash
pnpm tauri build --bundles app
```

Expected: a MyCodeBuddy `.app` bundle is created under the macOS target release
bundle directory without updater signing secrets or updater artifacts. Verify
it contains the license resources:

```bash
find src-tauri/target -path '*MyCodeBuddy.app*' \
  \( -name LICENSE -o -name NOTICE -o -name THIRD_PARTY_LICENSES.txt \) \
  -print
```

Expected: all three compliance files are present.

- [ ] **Step 4: Inspect the final diff**

Run:

```bash
git status --short --branch
git diff --check
git log --oneline --decorate -12
```

Confirm unrelated pre-existing modified files remain untouched and unstaged.

- [ ] **Step 5: Request final code review**

Use the `requesting-code-review` skill against the complete implementation.
Resolve all correctness, release-safety, secret-handling, and macOS-regression
findings before reporting completion.

- [ ] **Step 6: Present integration choices**

Report:

- exact verification results;
- updater secret paths and the fact that GitHub Secrets still require user
  entry;
- current branch and remote state;
- Windows release tag to use after merge;
- choice of pushing the implementation branch for a PR or explicitly pushing
  to `main`.

Do not push or tag until the user chooses.
