import { createHash } from "node:crypto"
import { execFileSync } from "node:child_process"
import {
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { dirname, isAbsolute, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)))
const cargoRoot = join(repositoryRoot, "src-tauri")
const outputPath = join(cargoRoot, "resources", "THIRD_PARTY_LICENSES.txt")
const licenseFilenamePattern = /^(license|licence|copying|notice)(\..+)?$/i

export const CARGO_TARGETS = [
  "x86_64-pc-windows-msvc",
  "aarch64-pc-windows-msvc",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
]

const compareStrings = (left, right) =>
  left < right ? -1 : left > right ? 1 : 0

const compareRecords = (left, right) =>
  compareStrings(left.ecosystem, right.ecosystem) ||
  compareStrings(left.name, right.name) ||
  compareStrings(left.version, right.version)

const normalizeText = (text) =>
  text
    .replace(/\r\n?/g, "\n")
    .replace(/[ \t]+$/gm, "")
    .trimEnd()

const normalizeHomepage = (homepage) => {
  const value = typeof homepage === "string" ? homepage.trim() : ""
  if (
    !value ||
    value.startsWith("file:") ||
    isAbsolute(value) ||
    /^[A-Za-z]:[\\/]/.test(value) ||
    value.startsWith("\\\\")
  ) {
    return ""
  }
  return value
}

const packageIdentifier = ({ ecosystem, name, version }) =>
  `${ecosystem}:${name}@${version}`

const assertLicensed = (record) => {
  if (!record.declaredLicense && record.licenseTexts.length === 0) {
    throw new Error(
      `${packageIdentifier(record)} has neither a license declaration nor license text`
    )
  }
  return record
}

export const findLicenseFiles = (packageDirectory) =>
  readdirSync(packageDirectory, { withFileTypes: true })
    .filter(
      (entry) =>
        licenseFilenamePattern.test(entry.name) &&
        statSync(join(packageDirectory, entry.name)).isFile()
    )
    .sort((left, right) => compareStrings(left.name, right.name))
    .map((entry) => ({
      name: entry.name,
      text: normalizeText(
        readFileSync(join(packageDirectory, entry.name), "utf8")
      ),
    }))
    .filter(({ text }) => text.length > 0)

export const collectNpmPackages = (pnpmReport) => {
  const records = []

  for (const entries of Object.values(pnpmReport)) {
    for (const entry of entries) {
      if (entry.versions.length !== entry.paths.length) {
        throw new Error(
          `pnpm returned mismatched versions and paths for ${entry.name}`
        )
      }

      for (let index = 0; index < entry.versions.length; index += 1) {
        const declaredLicense =
          entry.license === "Unknown" ? "" : String(entry.license ?? "").trim()
        records.push(
          assertLicensed({
            ecosystem: "npm",
            name: String(entry.name),
            version: String(entry.versions[index]),
            declaredLicense,
            homepage: normalizeHomepage(entry.homepage),
            licenseTexts: findLicenseFiles(entry.paths[index]),
          })
        )
      }
    }
  }

  return records.sort(compareRecords)
}

export const collectCargoPackages = (cargoMetadata) => {
  if (!cargoMetadata.resolve || !Array.isArray(cargoMetadata.resolve.nodes)) {
    throw new Error("Cargo metadata resolve graph is missing")
  }
  const rootId = cargoMetadata.resolve.root
  if (typeof rootId !== "string" || rootId.length === 0) {
    throw new Error("Cargo metadata resolve root is missing")
  }
  if (!Array.isArray(cargoMetadata.packages)) {
    throw new Error("Cargo metadata packages list is missing")
  }

  const packagesById = new Map(
    cargoMetadata.packages.map((cargoPackage) => [
      cargoPackage.id,
      cargoPackage,
    ])
  )
  if (!packagesById.has(rootId)) {
    throw new Error(
      `Cargo metadata resolve root references unknown package ID: ${rootId}`
    )
  }

  const nodesById = new Map()
  for (const node of cargoMetadata.resolve.nodes) {
    if (!packagesById.has(node.id)) {
      throw new Error(
        `Cargo metadata resolve node references unknown package ID: ${node.id}`
      )
    }
    nodesById.set(node.id, node)
    for (const dependency of node.deps ?? []) {
      if (!packagesById.has(dependency.pkg)) {
        throw new Error(
          `Cargo metadata resolve graph references unknown package ID: ${dependency.pkg}`
        )
      }
    }
  }
  if (!nodesById.has(rootId)) {
    throw new Error(
      `Cargo metadata resolve graph is missing the root node: ${rootId}`
    )
  }

  const reachablePackageIds = new Set([rootId])
  const pendingPackageIds = [rootId]
  while (pendingPackageIds.length > 0) {
    const packageId = pendingPackageIds.pop()
    const node = nodesById.get(packageId)
    if (!node) {
      throw new Error(
        `Cargo metadata resolve graph is missing node for package ID: ${packageId}`
      )
    }
    for (const dependency of node.deps ?? []) {
      const isProductionEdge = (dependency.dep_kinds ?? []).some(
        ({ kind }) => kind == null || kind === "normal" || kind === "build"
      )
      if (isProductionEdge && !reachablePackageIds.has(dependency.pkg)) {
        reachablePackageIds.add(dependency.pkg)
        pendingPackageIds.push(dependency.pkg)
      }
    }
  }

  return [...reachablePackageIds]
    .filter((packageId) => packageId !== rootId)
    .map((packageId) => packagesById.get(packageId))
    .map((cargoPackage) =>
      assertLicensed({
        ecosystem: "cargo",
        name: String(cargoPackage.name),
        version: String(cargoPackage.version),
        declaredLicense: String(cargoPackage.license ?? "").trim(),
        homepage: normalizeHomepage(
          cargoPackage.homepage ?? cargoPackage.repository
        ),
        licenseTexts: findLicenseFiles(dirname(cargoPackage.manifest_path)),
      })
    )
    .sort(compareRecords)
}

const mergeEquivalentValue = (identifier, field, left, right) => {
  if (!left) return right
  if (!right) return left
  if (left !== right) {
    throw new Error(
      `${identifier} has conflicting ${field} metadata: ${left} !== ${right}`
    )
  }
  return left
}

const mergeEquivalentLicenseTexts = (identifier, left, right) => {
  if (left.length === 0) return right
  if (right.length === 0) return left

  const leftTexts = [...new Set(left.map(({ text }) => text))].sort(
    compareStrings
  )
  const rightTexts = [...new Set(right.map(({ text }) => text))].sort(
    compareStrings
  )
  if (
    leftTexts.length !== rightTexts.length ||
    leftTexts.some((text, index) => text !== rightTexts[index])
  ) {
    throw new Error(`${identifier} has conflicting license text metadata`)
  }

  const namesByText = new Map()
  for (const { name, text } of [...left, ...right]) {
    const currentName = namesByText.get(text)
    if (!currentName || compareStrings(name, currentName) < 0) {
      namesByText.set(text, name)
    }
  }
  return [...namesByText]
    .map(([text, name]) => ({ name, text }))
    .sort(
      (leftText, rightText) =>
        compareStrings(leftText.name, rightText.name) ||
        compareStrings(leftText.text, rightText.text)
    )
}

export const collectCargoPackageUnion = (cargoMetadataRecords) => {
  const packagesByIdentifier = new Map()

  for (const cargoMetadata of cargoMetadataRecords) {
    for (const record of collectCargoPackages(cargoMetadata)) {
      const identifier = packageIdentifier(record)
      const existing = packagesByIdentifier.get(identifier)
      if (!existing) {
        packagesByIdentifier.set(identifier, record)
        continue
      }

      packagesByIdentifier.set(identifier, {
        ...existing,
        declaredLicense: mergeEquivalentValue(
          identifier,
          "license",
          existing.declaredLicense,
          record.declaredLicense
        ),
        homepage: mergeEquivalentValue(
          identifier,
          "homepage",
          existing.homepage,
          record.homepage
        ),
        licenseTexts: mergeEquivalentLicenseTexts(
          identifier,
          existing.licenseTexts,
          record.licenseTexts
        ),
      })
    }
  }

  return [...packagesByIdentifier.values()].sort(compareRecords)
}

export const renderLicenseReport = (records) => {
  const sortedRecords = records
    .map((record) =>
      assertLicensed({
        ecosystem: record.ecosystem,
        name: String(record.name),
        version: String(record.version),
        declaredLicense: String(record.declaredLicense ?? "").trim(),
        homepage: normalizeHomepage(record.homepage),
        licenseTexts: (record.licenseTexts ?? [])
          .map(({ name, text }) => ({
            name: String(name),
            text: normalizeText(String(text)),
          }))
          .filter(({ text }) => text.length > 0),
      })
    )
    .sort(compareRecords)
  const textGroups = new Map()
  const hashesByPackage = new Map()

  for (const record of sortedRecords) {
    const identifier = packageIdentifier(record)
    const packageHashes = new Set()
    for (const licenseText of record.licenseTexts) {
      const hash = createHash("sha256")
        .update(licenseText.text, "utf8")
        .digest("hex")
      packageHashes.add(hash)
      const group = textGroups.get(hash) ?? {
        hash,
        text: licenseText.text,
        packages: new Set(),
      }
      group.packages.add(identifier)
      textGroups.set(hash, group)
    }
    hashesByPackage.set(identifier, [...packageHashes].sort(compareStrings))
  }

  const lines = [
    "THIRD-PARTY SOFTWARE LICENSES",
    "",
    "This report covers locked npm production dependencies and Cargo",
    "normal/build dependency graphs for the deterministic union of these targets:",
    ...CARGO_TARGETS.map((target) => `- ${target}`),
    "",
    "PACKAGE INVENTORY",
    "=================",
  ]

  for (const record of sortedRecords) {
    const identifier = packageIdentifier(record)
    lines.push("", identifier)
    lines.push(`Declared license: ${record.declaredLicense || "not declared"}`)
    if (record.homepage) {
      lines.push(`Homepage: ${record.homepage}`)
    }
    const hashes = hashesByPackage.get(identifier)
    lines.push(
      `License text SHA-256: ${hashes.length ? hashes.join(", ") : "none"}`
    )
  }

  lines.push("", "LICENSE TEXTS", "=============")
  for (const group of [...textGroups.values()].sort((left, right) =>
    compareStrings(left.hash, right.hash)
  )) {
    lines.push("", `SHA-256: ${group.hash}`, "Packages:")
    for (const identifier of [...group.packages].sort(compareStrings)) {
      lines.push(`- ${identifier}`)
    }
    lines.push("", group.text)
  }

  return `${lines.join("\n")}\n`
}

export const buildCommandInvocation = ({ command, args, platform, comSpec }) =>
  platform === "win32" && command.toLowerCase().endsWith(".cmd")
    ? {
        command: comSpec || "cmd.exe",
        args: ["/d", "/s", "/c", command, ...args],
      }
    : { command, args }

const readJsonCommand = (command, args, cwd) => {
  const invocation = buildCommandInvocation({
    command,
    args,
    platform: process.platform,
    comSpec: process.env.ComSpec,
  })
  return JSON.parse(
    execFileSync(invocation.command, invocation.args, {
      cwd,
      encoding: "utf8",
      maxBuffer: 100 * 1024 * 1024,
    })
  )
}

export const generateLicenseReport = () => {
  const pnpmCommand = process.platform === "win32" ? "pnpm.cmd" : "pnpm"
  const pnpmReport = readJsonCommand(
    pnpmCommand,
    ["licenses", "list", "--prod", "--json"],
    repositoryRoot
  )
  const cargoMetadataRecords = CARGO_TARGETS.map((target) =>
    readJsonCommand(
      "cargo",
      [
        "metadata",
        "--format-version",
        "1",
        "--locked",
        "--filter-platform",
        target,
      ],
      cargoRoot
    )
  )
  const records = [
    ...collectNpmPackages(pnpmReport),
    ...collectCargoPackageUnion(cargoMetadataRecords),
  ]
  const report = renderLicenseReport(records)

  mkdirSync(dirname(outputPath), { recursive: true })
  writeFileSync(outputPath, report, "utf8")
  return { outputPath, packageCount: records.length }
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const { packageCount } = generateLicenseReport()
  process.stdout.write(
    `Wrote src-tauri/resources/THIRD_PARTY_LICENSES.txt for ${packageCount} packages.\n`
  )
}
