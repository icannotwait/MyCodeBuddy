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
  readFileSync(new URL(`../${path}`, import.meta.url), "utf8")

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
          - name: Windows ARM64
            runner: windows-latest
            target: aarch64-pc-windows-msvc
    runs-on: \${{ matrix.runner }}
    steps:
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
  const version = "0.20.1-mycodebuddy.1"
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
  const version = "0.20.1-mycodebuddy.1"
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
  assert.match(syncGuide, /sync\/codeg-0\.20\.1/)
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

test("release workflow publishes only Windows MyCodeBuddy artifacts", () => {
  const workflowText = readRepositoryFile(".github/workflows/release.yml")
  const desktopJob = workflowText.match(
    /^  build-desktop:\n([\s\S]*?)(?=^  build-server:)/m
  )?.[1]

  assertWindowsReleaseWorkflow(workflowText)
  assert.match(workflowText, /MyCodeBuddy \$\{tag\}/)
  assert.match(workflowText, /prerelease:\s*false/)
  assert.match(workflowText, /codeg-server-windows-x64/)
  assert.doesNotMatch(workflowText, /includeUpdaterJson:\s*false/)
  assert.ok(desktopJob, "build-desktop job is missing")
  assert.match(desktopJob, /^      max-parallel:\s*1\s*$/m)
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
    /^  build-desktop:\n([\s\S]*?)(?=^  build-server:)/m
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

test("server READMEs require manual Windows upgrades and current examples", () => {
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
    const releaseSectionStart = text.indexOf("codeg-server-windows-x64.zip")

    assert.notEqual(
      releaseSectionStart,
      -1,
      `${path} lacks the Windows server artifact`
    )
    assert.doesNotMatch(text, /v0\.5\.2/, `${path} has the old version`)
    assert.match(
      text,
      /\.\\install\.ps1 -Version v0\.20\.1-mycodebuddy\.1/,
      `${path} lacks the current installer example`
    )
    assert.ok(
      text.split("install.ps1").length - 1 >= 3,
      `${path} must tell Windows users to rerun install.ps1`
    )
    assert.ok(
      text.split("codeg-server-windows-x64.zip").length - 1 >= 2,
      `${path} must describe replacement from the next Windows ZIP`
    )
    assert.match(
      text,
      /git pull/,
      `${path} must tell source-built deployments to pull source`
    )
    assert.match(
      text,
      /cargo build --release --bin codeg-server --no-default-features/,
      `${path} must tell source-built deployments to rebuild`
    )
    assert.match(
      text,
      /Linux\/macOS/,
      `${path} must describe Linux/macOS source-built upgrades`
    )
    assert.match(text, /GitHub\s+Releases/)
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

test("rejects every release target except the two Windows MSVC targets", () => {
  for (const target of [
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
