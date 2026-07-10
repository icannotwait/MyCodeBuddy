import assert from "node:assert/strict"
import test from "node:test"
import {
  assertComplianceResources,
  assertForkVersion,
  assertMatchingVersions,
  assertNoAuthenticodeConfig,
  assertWindowsReleaseWorkflow,
  findForbiddenRuntimeUrls,
  readCargoVersion,
} from "./release-policy.mjs"

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
      NOTICE: "Based on https://github.com/xintaofei/codeg",
    }),
    ["tauri.conf.json"]
  )
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
