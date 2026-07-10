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
