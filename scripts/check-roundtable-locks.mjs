import { readFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

const root = join(dirname(fileURLToPath(import.meta.url)), "..")
const mainLock = join(root, "src-tauri/Cargo.lock")
const protocolLock = join(root, "src-tauri/roundtable-protocol/Cargo.lock")

function field(body, key) {
  const match = body.match(new RegExp(`^${key} = "([^"]*)"`, "m"))
  return match ? match[1] : ""
}

function packages(text) {
  const blocks = text.split(/\r?\n\[\[package\]\]\r?\n/).slice(1)
  const byName = new Map()
  for (const block of blocks) {
    const body = block.split(/\r?\n\[\[/)[0]
    const name = field(body, "name")
    if (!name) {
      continue
    }
    const entry = {
      name,
      version: field(body, "version"),
      source: field(body, "source"),
      checksum: field(body, "checksum"),
    }
    const list = byName.get(name) ?? []
    list.push(entry)
    byName.set(name, list)
  }
  return byName
}

function byVersion(groups) {
  const out = new Map()
  for (const entries of groups.values()) {
    for (const entry of entries) {
      const key = `${entry.name}@${entry.version}`
      const prior = out.get(key)
      if (
        prior &&
        (prior.source !== entry.source || prior.checksum !== entry.checksum)
      ) {
        throw new Error(`duplicate ${key} rows disagree inside one lock`)
      }
      out.set(key, entry)
    }
  }
  return out
}

const [mainText, protocolText] = await Promise.all([
  readFile(mainLock, "utf8"),
  readFile(protocolLock, "utf8"),
])
const main = byVersion(packages(mainText))
const protocol = byVersion(packages(protocolText))
const shared = [...protocol.keys()].filter((key) => main.has(key)).sort()
const mismatches = []

for (const key of shared) {
  const left = main.get(key)
  const right = protocol.get(key)
  if (left.source !== right.source || left.checksum !== right.checksum) {
    mismatches.push(
      [
        key,
        `  src-tauri: ${left.source} ${left.checksum}`,
        `  roundtable-protocol: ${right.source} ${right.checksum}`,
      ].join("\n")
    )
  }
}

if (mismatches.length > 0) {
  console.error(
    `shared Cargo.lock packages differ (${mismatches.length}):\n${mismatches.join("\n\n")}`
  )
  process.exit(1)
}

console.log(`shared lock packages match (${shared.length})`)
