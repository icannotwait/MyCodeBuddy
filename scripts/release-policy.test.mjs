import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import test from "node:test"
import {
  assertComplianceResources,
  assertForkVersion,
  assertMatchingVersions,
  assertNoAuthenticodeConfig,
  assertServerInstallerCompliance,
  assertUpdaterArtifactPolicy,
  assertWindowsReleaseWorkflow,
  findForbiddenRuntimeUrls,
  readCargoVersion,
} from "./release-policy.mjs"

const readRepositoryFile = (path) =>
  readFileSync(new URL(`../${path}`, import.meta.url), "utf8").replace(
    /\r\n/g,
    "\n"
  )

const validWindowsWorkflow = `
name: Release MyCodeBuddy

env:
  RELEASE_REPOSITORY: icannotwait/MyCodeBuddy

jobs:
  verify:
    runs-on: ubuntu-22.04
    steps:
      - name: Verify fork repository
        shell: bash
        run: |
          test "$GITHUB_REPOSITORY" = "icannotwait/MyCodeBuddy"

  build-desktop:
    strategy:
      matrix:
        include:
          - name: Windows x64
            runner: windows-2022
            target: x86_64-pc-windows-msvc
    runs-on: \${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - name: Stage desktop sidecars
        run: pnpm tauri:prepare-sidecars --target \${{ matrix.target }}
      - name: Verify sidecars
        run: Test-Path src-tauri/binaries/codeg-mcp-x86_64-pc-windows-msvc.exe
      - uses: tauri-apps/tauri-action@v0.6.1
        with:
          prerelease: false
          args: --target \${{ matrix.target }} --bundles nsis
        env:
          TAURI_SIGNING_PRIVATE_KEY: \${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: \${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
`

test("reads the package version from Cargo.toml", () => {
  assert.equal(
    readCargoVersion(
      '[package]\nname = "codeg"\nversion = "0.18.8-mycodebuddy.1"\n'
    ),
    "0.18.8-mycodebuddy.1"
  )
})

test("requires the MyCodeBuddy version suffix", () => {
  assert.doesNotThrow(() => assertForkVersion("0.18.8-mycodebuddy.1"))
  assert.throws(() => assertForkVersion("0.18.8"), /mycodebuddy/)
})

test("requires a positive MyCodeBuddy version counter", () => {
  assert.throws(() => assertForkVersion("0.18.8-mycodebuddy.0"), /positive/)
})

test("requires package Cargo Tauri and tag versions to match", () => {
  const version = "0.20.2-mycodebuddy.8"
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
      NOTICE: "Based on https://github.com/xintaofei/codeg",
    }),
    ["tauri.conf.json"]
  )
})

test("repository identity matches the MyCodeBuddy release policy", () => {
  const version = "0.22.2-mycodebuddy.1"
  const packageJson = JSON.parse(readRepositoryFile("package.json"))
  const cargoToml = readRepositoryFile("src-tauri/Cargo.toml")
  const tauriConfig = JSON.parse(
    readRepositoryFile("src-tauri/tauri.conf.json")
  )
  const runtimeFiles = Object.fromEntries(
    [
      "src-tauri/tauri.conf.json",
      "src-tauri/src/update/version.rs",
      "src/components/settings/system-network-settings.tsx",
      "install.ps1",
    ].map((path) => [path, readRepositoryFile(path)])
  )

  assertMatchingVersions({
    packageVersion: packageJson.version,
    cargoVersion: readCargoVersion(cargoToml),
    tauriVersion: tauriConfig.version,
  })
  assert.equal(packageJson.version, version)
  assert.match(readRepositoryFile("install.ps1"), new RegExp(`v${version}`))
  const syncGuide = readRepositoryFile("docs/UPSTREAM_SYNC.md")
  assert.match(syncGuide, /sync\/codeg-0\.22\.2/)
  assert.match(syncGuide, new RegExp(version.replaceAll(".", String.raw`\.`)))
  assertComplianceResources(tauriConfig)
  for (const path of [
    "LICENSE",
    "NOTICE",
    "src-tauri/resources/THIRD_PARTY_LICENSES.txt",
  ]) {
    assert.ok(readRepositoryFile(path).trim().length > 0, `${path} is empty`)
  }
  assert.deepEqual(findForbiddenRuntimeUrls(runtimeFiles), [])
})

test("release workflow publishes only Windows MyCodeBuddy desktop artifacts", () => {
  const workflowText = readRepositoryFile(".github/workflows/release.yml")
  const desktopJob = workflowText.match(
    /^  build-desktop:\n([\s\S]*?)(?=^  [A-Za-z0-9_-]+:)/m
  )?.[1]

  assertWindowsReleaseWorkflow(workflowText)
  assert.match(workflowText, /MyCodeBuddy \$\{tag\}/)
  assert.match(workflowText, /prerelease:\s*false/)
  assert.doesNotMatch(workflowText, /^  build-server:/m)
  assert.doesNotMatch(workflowText, /codeg-server-windows-x64/)
  assert.doesNotMatch(workflowText, /includeUpdaterJson:\s*false/)
  assert.ok(desktopJob, "build-desktop job is missing")
  assert.doesNotMatch(desktopJob, /Windows ARM64/)
  assert.doesNotMatch(desktopJob, /aarch64-pc-windows-msvc/)
  assert.doesNotMatch(desktopJob, /^      max-parallel:/m)
  assert.match(desktopJob, /^          includeUpdaterJson:\s*true\s*$/m)
})

test("uses updater artifacts only through the release Tauri config", () => {
  const defaultConfig = JSON.parse(
    readRepositoryFile("src-tauri/tauri.conf.json")
  )
  const releaseConfig = JSON.parse(
    readRepositoryFile("src-tauri/tauri.release.conf.json")
  )
  const workflowText = readRepositoryFile(".github/workflows/release.yml")
  const desktopJob = workflowText.match(
    /^  build-desktop:\n([\s\S]*?)(?=^  [A-Za-z0-9_-]+:)/m
  )?.[1]

  assert.equal(defaultConfig.bundle.createUpdaterArtifacts, false)
  assert.equal(releaseConfig.bundle.createUpdaterArtifacts, true)
  assert.ok(desktopJob, "build-desktop job is missing")
  assert.match(
    desktopJob,
    /args:\s*.*--config\s+src-tauri\/tauri\.release\.conf\.json.*--target\s+\$\{\{\s*matrix\.target\s*\}\}.*--bundles\s+nsis/
  )
  assert.match(desktopJob, /^          includeUpdaterJson:\s*true\s*$/m)
  assert.doesNotThrow(() =>
    assertUpdaterArtifactPolicy({
      defaultConfig,
      releaseConfig,
      workflowText,
    })
  )
})

test("server installer validates and copies compliance files before install writes", () => {
  const installScript = readRepositoryFile("install.ps1")

  assert.doesNotThrow(() => assertServerInstallerCompliance(installScript))
  assert.match(installScript, /\$item\.Length -gt 0/)
  assert.match(
    installScript,
    /\$RequiredWebFiles\s*=\s*@\("web\\index\.html"\)/
  )
  assert.match(
    installScript,
    /\$RequiredInstalledFiles\s*=\s*@\("codeg-server\.exe",\s*"codeg-mcp\.exe",\s*"LICENSE",\s*"NOTICE",\s*"THIRD_PARTY_LICENSES\.txt"\)/
  )
  assert.match(
    installScript,
    /\$RequiredWebFiles\s*=\s*@\("web\\index\.html"\)/
  )
  assert.match(
    installScript,
    /-and \(Test-InstalledFilesComplete -Directory \$InstallDir\)/
  )
  for (const requiredEntry of [
    "codeg-server.exe",
    "codeg-mcp.exe",
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_LICENSES.txt",
  ]) {
    const requiredListName = "$RequiredInstalledFiles"
    const requiredListLine = installScript
      .split("\n")
      .find((line) => line.startsWith(requiredListName))
    assert.ok(requiredListLine, `${requiredListName} is missing`)
    const withoutRequiredEntry = installScript.replace(
      requiredListLine,
      requiredListLine
        .replace(`"${requiredEntry}", `, "")
        .replace(`, "${requiredEntry}"`, "")
        .replace(`"${requiredEntry}"`, "")
    )
    assert.notEqual(
      withoutRequiredEntry,
      installScript,
      `fixture failed to remove ${requiredEntry}`
    )
    assert.throws(
      () => assertServerInstallerCompliance(withoutRequiredEntry),
      /required installed/i,
      `policy accepted a shortcut without ${requiredEntry}`
    )
  }
  assert.throws(
    () =>
      assertServerInstallerCompliance(
        installScript.replace(
          '$ComplianceFiles = @("LICENSE", "NOTICE", "THIRD_PARTY_LICENSES.txt")',
          '$ComplianceFiles = @("LICENSE", "NOTICE")'
        )
      ),
    /LICENSE.*NOTICE.*THIRD_PARTY_LICENSES/
  )
  assert.throws(
    () =>
      assertServerInstallerCompliance(
        installScript.replace(
          "# ── Install ──",
          [
            "# ── Install ──",
            "New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null",
          ].join("\n")
        )
      ),
    /before writing InstallDir/
  )
  const withoutZeroSizeCheck = installScript.replace(
    "$item.Length -gt 0",
    "$item.Length -ge 0"
  )
  assert.notEqual(
    withoutZeroSizeCheck,
    installScript,
    "fixture failed to remove the zero-size check"
  )
  assert.throws(
    () => assertServerInstallerCompliance(withoutZeroSizeCheck),
    /nonempty regular file/,
    "policy accepted an installer that permits zero-byte files"
  )
  const withoutWebEntry = installScript.replace(
    '$RequiredWebFiles = @("web\\index.html")',
    "$RequiredWebFiles = @()"
  )
  assert.notEqual(
    withoutWebEntry,
    installScript,
    "fixture failed to remove web/index.html"
  )
  assert.throws(
    () => assertServerInstallerCompliance(withoutWebEntry),
    /web\/index\.html/,
    "policy accepted an installer without the static web entry"
  )
  const archiveWebValidation = "foreach ($relativePath in $RequiredWebFiles) {"
  const archiveWebValidationIndex = installScript.indexOf(
    archiveWebValidation,
    installScript.indexOf("# ── Install ──")
  )
  assert.notEqual(
    archiveWebValidationIndex,
    -1,
    "fixture failed to find archive web validation"
  )
  const withoutArchiveWebValidation =
    installScript.slice(0, archiveWebValidationIndex) +
    "foreach ($relativePath in @()) {" +
    installScript.slice(archiveWebValidationIndex + archiveWebValidation.length)
  assert.notEqual(
    withoutArchiveWebValidation,
    installScript,
    "fixture failed to remove archive web validation"
  )
  assert.throws(
    () => assertServerInstallerCompliance(withoutArchiveWebValidation),
    /web\/index\.html.*before writing InstallDir/,
    "policy accepted an archive validation that skips web/index.html"
  )
})

test("documents desktop-only releases and opt-in server self-host", () => {
  const paths = [
    "README.md",
    "docs/readme/README.ar.md",
    "docs/readme/README.de.md",
    "docs/readme/README.es.md",
    "docs/readme/README.fr.md",
    "docs/readme/README.ja.md",
    "docs/readme/README.ko.md",
    "docs/readme/README.pt.md",
    "docs/readme/README.zh-CN.md",
    "docs/readme/README.zh-TW.md",
  ]

  for (const path of paths) {
    const text = readRepositoryFile(path)
    assert.doesNotMatch(text, /v0\.5\.2/, `${path} has the old version`)
    assert.match(
      text,
      /uninstall-server\.ps1/,
      `${path} must document removing leftover codeg-server installs`
    )
    assert.match(
      text,
      /git pull/,
      `${path} must tell source-built deployments to pull source`
    )
    assert.match(
      text,
      /cargo build --release --bin codeg-server --no-default-features --features server/,
      `${path} must tell source-built deployments to rebuild with --features server`
    )
    assert.match(
      text,
      /Linux\/macOS/,
      `${path} must describe Linux/macOS source-built upgrades`
    )
    assert.match(text, /GitHub\s+Releases/)
    // Must not present prebuilt server zips as a current consumer download.
    assert.doesNotMatch(
      text,
      /\| Windows x64 \| `codeg-server-windows-x64\.zip` \|/,
      `${path} must not list codeg-server-windows-x64.zip as a release artifact`
    )
    assert.doesNotMatch(
      text,
      /--supervise/,
      `${path} must not advertise supervisor-driven auto-update`
    )
  }
})

test("accepts the complete Windows release policy", () => {
  assert.doesNotThrow(() => assertWindowsReleaseWorkflow(validWindowsWorkflow))
})

test("rejects duplicate checkout configuration in the desktop release", () => {
  const duplicateWith = validWindowsWorkflow.replace(
    "        with:\n          submodules: recursive",
    "        with:\n          submodules: recursive\n        with:\n          fetch-depth: 0"
  )

  assert.throws(
    () => assertWindowsReleaseWorkflow(duplicateWith),
    /checkout step must contain exactly one with block/
  )
})

test("requires recursive checkout in the desktop job itself", () => {
  const misplacedSubmodules = validWindowsWorkflow
    .replace("        with:\n          submodules: recursive\n", "")
    .replace(
      "      - name: Verify fork repository",
      "      - uses: actions/checkout@v4\n        with:\n          submodules: recursive\n      - name: Verify fork repository"
    )

  assert.throws(
    () => assertWindowsReleaseWorkflow(misplacedSubmodules),
    /desktop release must checkout submodules recursively/
  )
})

test("rejects publishing the standalone codeg-server binary from release", () => {
  const withServerJob = `${validWindowsWorkflow}
  build-server:
    runs-on: windows-2022
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - run: cargo build --release --bin codeg-server --no-default-features --features server
`

  assert.throws(
    () => assertWindowsReleaseWorkflow(withServerJob),
    /must not include a build-server job/
  )
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        `${validWindowsWorkflow}\nrun: echo codeg-server-windows-x64.zip\n`
      ),
    /must not publish codeg-server-windows-x64/
  )
})

test("rejects every release target except Windows x64 MSVC", () => {
  for (const target of [
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "i686-pc-windows-msvc",
  ]) {
    assert.throws(
      () =>
        assertWindowsReleaseWorkflow(
          `${validWindowsWorkflow}\n  target: ${target}\n`
        ),
      /unsupported release target/
    )
  }
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        `${validWindowsWorkflow}
run: rustup target add wasm32-unknown-unknown
`
      ),
    /unsupported release target/
  )
})

test("requires both Tauri updater signing secret references", () => {
  for (const secret of [
    "TAURI_SIGNING_PRIVATE_KEY",
    "TAURI_SIGNING_PRIVATE_KEY_PASSWORD",
  ]) {
    const withoutSecret = validWindowsWorkflow.replace(
      `\${{ secrets.${secret} }}`,
      "missing"
    )
    assert.throws(
      () =>
        assertWindowsReleaseWorkflow(
          `${withoutSecret}\n# \${{ secrets.${secret} }}\n`
        ),
      new RegExp(secret)
    )
  }
})

test("requires direct updater signing secret env mappings", () => {
  for (const [secret, alias] of [
    ["TAURI_SIGNING_PRIVATE_KEY", "UPDATER_PRIVATE_KEY"],
    ["TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "UPDATER_KEY_PASSWORD"],
  ]) {
    const directMapping = `${secret}: ` + `\${{ secrets.${secret} }}`
    const aliasMapping = `${alias}: ` + `\${{ secrets.${secret} }}`

    assert.throws(
      () =>
        assertWindowsReleaseWorkflow(
          validWindowsWorkflow.replace(directMapping, aliasMapping)
        ),
      /direct env mapping/
    )
    assert.throws(
      () =>
        assertWindowsReleaseWorkflow(
          `${validWindowsWorkflow}\n${aliasMapping}\n`
        ),
      /env alias/
    )
  }
})

test("rejects macOS runners but allows Ubuntu release management", () => {
  assert.doesNotThrow(() => assertWindowsReleaseWorkflow(validWindowsWorkflow))
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        `${validWindowsWorkflow}
  build-macos:
    runs-on: macos-14
`
      ),
    /macOS runner/
  )
})

test("requires an allowed Windows target on every Tauri release build", () => {
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        validWindowsWorkflow.replace(
          "args: --target ${{ matrix.target }} --bundles nsis",
          "args: --bundles nsis"
        )
      ),
    /Windows matrix target/
  )
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        `${validWindowsWorkflow}
      - name: Untargeted CLI build
        run: pnpm tauri build --bundles nsis
`
      ),
    /Windows matrix target/
  )
  assert.doesNotThrow(() =>
    assertWindowsReleaseWorkflow(
      `${validWindowsWorkflow}
      - name: Targeted CLI build
        run: pnpm tauri build --target \${{ matrix.target }} --bundles nsis
`
    )
  )
})

test("rejects Authenticode certificate and signing configuration", () => {
  for (const configuration of [
    "certificateThumbprint: ABCDEF",
    "run: signtool sign MyCodeBuddy.exe",
    "TAURI_BUNDLER_WINDOWS_DIGEST_ALGORITHM: sha256",
  ]) {
    assert.throws(
      () =>
        assertWindowsReleaseWorkflow(
          `${validWindowsWorkflow}\n${configuration}\n`
        ),
      /Authenticode/
    )
  }
  assert.doesNotThrow(() =>
    assertWindowsReleaseWorkflow(
      `${validWindowsWorkflow}
- name: Build without Authenticode
# Authenticode certificate signing is intentionally disabled.
# target: x86_64-apple-darwin
`
    )
  )
})

test("requires GitHub releases to set prerelease false", () => {
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        validWindowsWorkflow.replace("prerelease: false", "prerelease: true")
      ),
    /prerelease: false/
  )
})

test("requires the MyCodeBuddy fork repository identity", () => {
  assert.throws(
    () =>
      assertWindowsReleaseWorkflow(
        `${validWindowsWorkflow.replaceAll(
          "icannotwait/MyCodeBuddy",
          "someone/other-repository"
        )}
# GITHUB_REPOSITORY must be icannotwait/MyCodeBuddy
`
      ),
    /icannotwait\/MyCodeBuddy/
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

test("rejects Tauri Authenticode configuration", () => {
  for (const key of [
    "certificateThumbprint",
    "digestAlgorithm",
    "timestampUrl",
    "signCommand",
    "certificatePath",
  ]) {
    assert.throws(
      () =>
        assertNoAuthenticodeConfig({
          bundle: { windows: { [key]: "configured" } },
        }),
      /Authenticode/
    )
  }
  assert.doesNotThrow(() =>
    assertNoAuthenticodeConfig({
      bundle: {
        windows: { nsis: { installerHooks: "./windows/installer-hooks.nsh" } },
      },
      plugins: { updater: { pubkey: "public-key" } },
    })
  )
})
