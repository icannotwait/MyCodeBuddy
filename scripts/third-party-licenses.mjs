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

const compareStrings = (left, right) =>
  left < right ? -1 : left > right ? 1 : 0

const compareRecords = (left, right) =>
  compareStrings(left.ecosystem, right.ecosystem) ||
  compareStrings(left.name, right.name) ||
  compareStrings(left.version, right.version)

const normalizeText = (text) =>
  text.replace(/\r\n?/g, "\n").replace(/[ \t]+$/gm, "").trimEnd()

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
  const workspaceMembers = new Set(cargoMetadata.workspace_members ?? [])

  return cargoMetadata.packages
    .filter(
      (cargoPackage) =>
        !(
          cargoPackage.name === "codeg" && workspaceMembers.has(cargoPackage.id)
        )
    )
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
    "dependencies resolved for x86_64-pc-windows-msvc.",
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

const readJsonCommand = (command, args, cwd) =>
  JSON.parse(
    execFileSync(command, args, {
      cwd,
      encoding: "utf8",
      maxBuffer: 100 * 1024 * 1024,
    })
  )

export const generateLicenseReport = () => {
  const pnpmCommand = process.platform === "win32" ? "pnpm.cmd" : "pnpm"
  const pnpmReport = readJsonCommand(
    pnpmCommand,
    ["licenses", "list", "--prod", "--json"],
    repositoryRoot
  )
  const cargoMetadata = readJsonCommand(
    "cargo",
    [
      "metadata",
      "--format-version",
      "1",
      "--locked",
      "--filter-platform",
      "x86_64-pc-windows-msvc",
    ],
    cargoRoot
  )
  const records = [
    ...collectNpmPackages(pnpmReport),
    ...collectCargoPackages(cargoMetadata),
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
