import assert from "node:assert/strict"
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import {
  collectCargoPackageUnion,
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
    resolve: {
      root: "path+file:///checkout/src-tauri#codeg@1.0.0",
      nodes: [
        {
          id: "path+file:///checkout/src-tauri#codeg@1.0.0",
          deps: [
            {
              name: "alpha",
              pkg: "registry+https://example.test#alpha@1.0.0",
              dep_kinds: [{ kind: null, target: null }],
            },
            {
              name: "beta",
              pkg: "registry+https://example.test#beta@2.0.0",
              dep_kinds: [{ kind: null, target: null }],
            },
          ],
        },
        {
          id: "registry+https://example.test#alpha@1.0.0",
          deps: [],
        },
        {
          id: "registry+https://example.test#beta@2.0.0",
          deps: [],
        },
      ],
    },
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

test("collects only production-reachable Cargo packages from the resolve graph", () => {
  const packageRecord = (name) => {
    const directory = makePackageDirectory(name, {
      LICENSE: `${name} terms`,
    })
    return {
      id: `registry+https://example.test#${name}@1.0.0`,
      name,
      version: "1.0.0",
      license: "MIT",
      manifest_path: join(directory, "Cargo.toml"),
      homepage: `https://example.test/${name}`,
    }
  }
  const root = {
    id: "path+file:///checkout/src-tauri#codeg@1.0.0",
    name: "codeg",
    version: "1.0.0",
    license: "Apache-2.0",
    manifest_path: join(makePackageDirectory("codeg"), "Cargo.toml"),
  }
  const packages = [
    root,
    ...[
      "normal-direct",
      "normal-transitive",
      "build-direct",
      "build-transitive",
      "mixed-normal-dev",
      "axum-test",
      "insta",
      "temp-env",
    ].map(packageRecord),
  ]
  const id = (name) =>
    packages.find((cargoPackage) => cargoPackage.name === name).id
  const dependency = (name, kinds) => ({
    name,
    pkg: id(name),
    dep_kinds: kinds.map((kind) => ({ kind, target: null })),
  })
  const metadata = {
    packages,
    workspace_members: [root.id],
    resolve: {
      root: root.id,
      nodes: [
        {
          id: root.id,
          deps: [
            dependency("normal-direct", [null]),
            dependency("build-direct", ["build"]),
            dependency("mixed-normal-dev", [null, "dev"]),
            dependency("axum-test", ["dev"]),
            dependency("insta", ["dev"]),
            dependency("temp-env", ["dev"]),
          ],
        },
        {
          id: id("normal-direct"),
          deps: [dependency("normal-transitive", [null])],
        },
        {
          id: id("normal-transitive"),
          deps: [],
        },
        {
          id: id("build-direct"),
          deps: [dependency("build-transitive", ["build"])],
        },
        {
          id: id("build-transitive"),
          deps: [],
        },
        {
          id: id("mixed-normal-dev"),
          deps: [],
        },
        {
          id: id("axum-test"),
          deps: [],
        },
        {
          id: id("insta"),
          deps: [],
        },
        {
          id: id("temp-env"),
          deps: [],
        },
      ],
    },
  }

  const records = collectCargoPackages(metadata)

  assert.deepEqual(
    records.map(({ name }) => name),
    [
      "build-direct",
      "build-transitive",
      "mixed-normal-dev",
      "normal-direct",
      "normal-transitive",
    ]
  )
})

test("rejects missing or invalid Cargo resolve graphs", () => {
  const root = {
    id: "path+file:///checkout/src-tauri#codeg@1.0.0",
    name: "codeg",
    version: "1.0.0",
    license: "Apache-2.0",
    manifest_path: join(makePackageDirectory("codeg"), "Cargo.toml"),
  }
  const dependency = {
    id: "registry+https://example.test#dependency@1.0.0",
    name: "dependency",
    version: "1.0.0",
    license: "MIT",
    manifest_path: join(
      makePackageDirectory("dependency", { LICENSE: "Dependency terms" }),
      "Cargo.toml"
    ),
  }

  assert.throws(
    () => collectCargoPackages({ packages: [root, dependency] }),
    /Cargo metadata resolve graph is missing/
  )
  assert.throws(
    () =>
      collectCargoPackages({
        packages: [root, dependency],
        resolve: { root: null, nodes: [] },
      }),
    /Cargo metadata resolve root is missing/
  )
  assert.throws(
    () =>
      collectCargoPackages({
        packages: [root, dependency],
        resolve: {
          root: root.id,
          nodes: [
            {
              id: root.id,
              deps: [
                {
                  name: "missing",
                  pkg: "registry+https://example.test#missing@1.0.0",
                  dep_kinds: [{ kind: null, target: null }],
                },
              ],
            },
          ],
        },
      }),
    /unknown package ID.*missing@1\.0\.0/
  )
  assert.throws(
    () =>
      collectCargoPackageUnion([
        {
          packages: [root, dependency],
          resolve: {
            root: root.id,
            nodes: [
              { id: root.id, deps: [] },
              { id: "unknown", deps: [] },
            ],
          },
        },
      ]),
    /resolve node references unknown package ID.*unknown/
  )
})

test("unions the four bundled Cargo targets and deduplicates shared packages", async () => {
  const { CARGO_TARGETS } = await import("./third-party-licenses.mjs")
  const shared = makePackageDirectory("shared", { LICENSE: "Shared terms" })
  const windowsArmOnly = makePackageDirectory("windows-arm-only", {
    LICENSE: "Windows ARM terms",
  })
  const macOnly = makePackageDirectory("mac-only", {
    LICENSE: "macOS terms",
  })
  const cargoPackage = (directory, name, version = "1.0.0") => ({
    id: `registry+https://example.test#${name}@${version}`,
    name,
    version,
    license: "MIT",
    manifest_path: join(directory, "Cargo.toml"),
    homepage: `https://example.test/${name}`,
  })
  const metadata = (packages) => {
    const root = {
      id: "path+file:///checkout/src-tauri#codeg@1.0.0",
      name: "codeg",
      version: "1.0.0",
      license: "Apache-2.0",
      manifest_path: join(makePackageDirectory("codeg"), "Cargo.toml"),
    }
    return {
      workspace_members: [root.id],
      packages: [root, ...packages],
      resolve: {
        root: root.id,
        nodes: [
          {
            id: root.id,
            deps: packages.map((cargoPackage) => ({
              name: cargoPackage.name,
              pkg: cargoPackage.id,
              dep_kinds: [{ kind: null, target: null }],
            })),
          },
          ...packages.map((cargoPackage) => ({
            id: cargoPackage.id,
            deps: [],
          })),
        ],
      },
    }
  }

  assert.deepEqual(CARGO_TARGETS, [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
  ])

  const records = collectCargoPackageUnion([
    metadata([cargoPackage(shared, "shared")]),
    metadata([
      cargoPackage(shared, "shared"),
      cargoPackage(windowsArmOnly, "windows-arm-only"),
    ]),
    metadata([
      cargoPackage(shared, "shared"),
      cargoPackage(macOnly, "mac-only"),
    ]),
    metadata([
      cargoPackage(shared, "shared"),
      cargoPackage(macOnly, "mac-only"),
    ]),
  ])

  assert.deepEqual(
    records.map(({ name, version }) => ({ name, version })),
    [
      { name: "mac-only", version: "1.0.0" },
      { name: "shared", version: "1.0.0" },
      { name: "windows-arm-only", version: "1.0.0" },
    ]
  )
})

test("rejects conflicting Cargo metadata while merging equivalent records", async () => {
  const first = makePackageDirectory("conflict-first", {
    LICENSE: "First terms",
  })
  const second = makePackageDirectory("conflict-second", {
    LICENSE: "Second terms",
  })
  const cargoPackage = (directory, overrides = {}) => ({
    id: "registry+https://example.test#conflict@1.0.0",
    name: "conflict",
    version: "1.0.0",
    license: "MIT",
    manifest_path: join(directory, "Cargo.toml"),
    homepage: "https://example.test/conflict",
    ...overrides,
  })
  const metadata = (cargoPackageRecord) => {
    const root = {
      id: "path+file:///checkout/src-tauri#codeg@1.0.0",
      name: "codeg",
      version: "1.0.0",
      license: "Apache-2.0",
      manifest_path: join(makePackageDirectory("codeg"), "Cargo.toml"),
    }
    return {
      workspace_members: [root.id],
      packages: [root, cargoPackageRecord],
      resolve: {
        root: root.id,
        nodes: [
          {
            id: root.id,
            deps: [
              {
                name: cargoPackageRecord.name,
                pkg: cargoPackageRecord.id,
                dep_kinds: [{ kind: null, target: null }],
              },
            ],
          },
          { id: cargoPackageRecord.id, deps: [] },
        ],
      },
    }
  }

  assert.throws(
    () =>
      collectCargoPackageUnion([
        metadata(cargoPackage(first)),
        metadata(cargoPackage(second, { license: "Apache-2.0" })),
      ]),
    /conflicting.*license/i
  )
  assert.throws(
    () =>
      collectCargoPackageUnion([
        metadata(cargoPackage(first)),
        metadata(
          cargoPackage(second, {
            homepage: "https://example.test/different",
          })
        ),
      ]),
    /conflicting.*homepage/i
  )
  assert.throws(
    () =>
      collectCargoPackageUnion([
        metadata(cargoPackage(first)),
        metadata(cargoPackage(second)),
      ]),
    /conflicting.*license text/i
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
  assert.match(output, /x86_64-pc-windows-msvc/)
  assert.match(output, /aarch64-pc-windows-msvc/)
  assert.match(output, /x86_64-apple-darwin/)
  assert.match(output, /aarch64-apple-darwin/)
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
