import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import {
  collectCargoPackages,
  collectNpmPackages,
  findLicenseFiles,
  renderLicenseReport,
} from "./third-party-licenses.mjs"

const temporaryDirectories = []

const makeTemporaryDirectory = () => {
  const directory = mkdtempSync(join(tmpdir(), "codeg-licenses-"))
  temporaryDirectories.push(directory)
  return directory
}

const makePackageDirectory = (name, files = {}) => {
  const directory = join(makeTemporaryDirectory(), name)
  mkdirSync(directory, { recursive: true })
  for (const [filename, text] of Object.entries(files)) {
    writeFileSync(join(directory, filename), text)
  }
  return directory
}

test.afterEach(() => {
  for (const directory of temporaryDirectories.splice(0)) {
    rmSync(directory, { recursive: true, force: true })
  }
})

test("finds supported license filenames case-insensitively", () => {
  for (const [index, filename, source, expected] of [
    [0, "LICENSE", "license text\r\n\r\n", "license text"],
    [1, "license", "lowercase text\n", "lowercase text"],
    [2, "COPYING", "copying text\n", "copying text"],
    [
      3,
      "NOTICE.md",
      "notice text  \r\ncontinuation\t \r\n",
      "notice text\ncontinuation",
    ],
  ]) {
    const packageDirectory = makePackageDirectory(`fixture-${index}`, {
      [filename]: source,
      README: "not a license",
    })
    assert.deepEqual(findLicenseFiles(packageDirectory), [
      { name: filename, text: expected },
    ])
  }
})

test("collects npm versions and accepts khroma's lowercase license file", () => {
  const alphaV2 = makePackageDirectory("alpha-v2", { LICENSE: "MIT text" })
  const alphaV1 = makePackageDirectory("alpha-v1", { COPYING: "MIT text" })
  const khroma = makePackageDirectory("khroma", { license: "Khroma text" })
  const report = {
    Unknown: [
      {
        name: "khroma",
        versions: ["2.1.0"],
        paths: [khroma],
        license: "Unknown",
        homepage: "https://example.test/khroma",
      },
    ],
    MIT: [
      {
        name: "alpha",
        versions: ["2.0.0", "1.0.0"],
        paths: [alphaV2, alphaV1],
        license: "MIT",
        homepage: "https://example.test/alpha",
      },
    ],
  }

  const records = collectNpmPackages(report)

  assert.deepEqual(
    records.map(({ name, version, declaredLicense }) => ({
      name,
      version,
      declaredLicense,
    })),
    [
      { name: "alpha", version: "1.0.0", declaredLicense: "MIT" },
      { name: "alpha", version: "2.0.0", declaredLicense: "MIT" },
      { name: "khroma", version: "2.1.0", declaredLicense: "" },
    ]
  )
  assert.deepEqual(records[2].licenseTexts, [
    { name: "license", text: "Khroma text" },
  ])
})

test("collects Cargo Windows metadata and excludes the workspace root", () => {
  const beta = makePackageDirectory("beta", { LICENSE: "Beta terms" })
  const alpha = makePackageDirectory("alpha", { NOTICE: "Alpha terms" })
  const workspaceRoot = makePackageDirectory("codeg")
  const cargoMetadata = {
    workspace_members: ["path+file:///checkout/src-tauri#codeg@1.0.0"],
    packages: [
      {
        id: "registry+https://example.test#beta@2.0.0",
        name: "beta",
        version: "2.0.0",
        license: "MIT",
        manifest_path: join(beta, "Cargo.toml"),
        homepage: null,
        repository: "https://example.test/beta",
      },
      {
        id: "path+file:///checkout/src-tauri#codeg@1.0.0",
        name: "codeg",
        version: "1.0.0",
        license: "Apache-2.0",
        manifest_path: join(workspaceRoot, "Cargo.toml"),
      },
      {
        id: "registry+https://example.test#alpha@1.0.0",
        name: "alpha",
        version: "1.0.0",
        license: "Apache-2.0",
        manifest_path: join(alpha, "Cargo.toml"),
        homepage: "https://example.test/alpha",
      },
    ],
  }

  const records = collectCargoPackages(cargoMetadata)

  assert.deepEqual(
    records.map(({ ecosystem, name, version, homepage }) => ({
      ecosystem,
      name,
      version,
      homepage,
    })),
    [
      {
        ecosystem: "cargo",
        name: "alpha",
        version: "1.0.0",
        homepage: "https://example.test/alpha",
      },
      {
        ecosystem: "cargo",
        name: "beta",
        version: "2.0.0",
        homepage: "https://example.test/beta",
      },
    ]
  )
})

test("sorts packages, omits paths, and deduplicates license text by hash", () => {
  const absolutePath = join(makeTemporaryDirectory(), "must-not-appear")
  const sharedText = "Shared license terms"
  const records = [
    {
      ecosystem: "npm",
      name: "zeta",
      version: "2.0.0",
      declaredLicense: "MIT",
      homepage: "https://example.test/zeta",
      licenseTexts: [{ name: "LICENSE", text: sharedText }],
    },
    {
      ecosystem: "cargo",
      name: "alpha",
      version: "1.0.0",
      declaredLicense: "MIT",
      homepage: "",
      licenseTexts: [{ name: "COPYING", text: sharedText }],
      manifestPath: absolutePath,
    },
    {
      ecosystem: "npm",
      name: "alpha",
      version: "1.0.0",
      declaredLicense: "Apache-2.0",
      homepage: "",
      licenseTexts: [],
    },
  ]

  const output = renderLicenseReport(records)

  assert.ok(
    output.indexOf("cargo:alpha@1.0.0") < output.indexOf("npm:alpha@1.0.0")
  )
  assert.ok(
    output.indexOf("npm:alpha@1.0.0") < output.indexOf("npm:zeta@2.0.0")
  )
  assert.equal(output.split(sharedText).length - 1, 1)
  assert.match(output, /cargo:alpha@1\.0\.0/)
  assert.match(output, /npm:zeta@2\.0\.0/)
  assert.doesNotMatch(output, new RegExp(absolutePath.replaceAll("/", "\\/")))
})

test("rejects packages missing both a declaration and license text", () => {
  assert.throws(
    () =>
      renderLicenseReport([
        {
          ecosystem: "npm",
          name: "unlicensed",
          version: "1.0.0",
          declaredLicense: "",
          homepage: "",
          licenseTexts: [],
        },
      ]),
    /unlicensed@1\.0\.0.*declaration.*text/i
  )
  assert.throws(
    () =>
      renderLicenseReport([
        {
          ecosystem: "cargo",
          name: "empty-file",
          version: "1.0.0",
          declaredLicense: "",
          homepage: "",
          licenseTexts: [{ name: "LICENSE", text: " \r\n" }],
        },
      ]),
    /empty-file@1\.0\.0.*declaration.*text/i
  )
})

test("renders byte-identical output on repeated runs", () => {
  const records = [
    {
      ecosystem: "npm",
      name: "stable",
      version: "1.0.0",
      declaredLicense: "MIT",
      homepage: "",
      licenseTexts: [{ name: "LICENSE", text: "Stable terms" }],
    },
  ]

  assert.deepEqual(
    Buffer.from(renderLicenseReport(records)),
    Buffer.from(renderLicenseReport(records))
  )
})
