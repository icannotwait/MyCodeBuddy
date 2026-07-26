#!/usr/bin/env node
//
// RETIRED: desktop/server/Docker no longer ship codex-acp-seed.
// Codex ACP launches from official npm `@agentclientprotocol/codex-acp@1.1.7`
// (Agent Settings / `npm install -g`). Kept only for local debugging of the
// vendored fork tree — not wired into package.json, release.yml, or Dockerfile.
//
// Historical behavior (do not re-enable in release packaging):
//   1. Hard-fails if `src-tauri/vendor/codex-acp` is missing/empty (submodule).
//   2. Runs `npm ci && npm run build` inside the vendor package.
//   3. Copies the installable seed tree into `src-tauri/resources/codex-acp-seed/`.
//
// Intentionally Node-only: identical on macOS, Linux, and Windows runners.

import { execFileSync } from "node:child_process"
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import process from "node:process"

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url))
const SRC_TAURI = resolve(SCRIPT_DIR, "..")
const REPO_ROOT = resolve(SRC_TAURI, "..")
const VENDOR_DIR = join(SRC_TAURI, "vendor", "codex-acp")
const SEED_DIR = join(SRC_TAURI, "resources", "codex-acp-seed")

function log(msg) {
  console.log(`[stage-codex-acp] ${msg}`)
}

function die(msg) {
  console.error(`[stage-codex-acp][ERROR] ${msg}`)
  process.exit(1)
}

function assertVendorPresent() {
  const pkg = join(VENDOR_DIR, "package.json")
  if (!existsSync(pkg)) {
    die(
      `vendor submodule empty or missing: ${pkg}\n` +
        `Run: git submodule update --init --recursive src-tauri/vendor/codex-acp`
    )
  }
  try {
    const raw = readFileSync(pkg, "utf8")
    const json = JSON.parse(raw)
    if (!json.version) {
      die(`vendor package.json missing version field`)
    }
    return json.version
  } catch (e) {
    die(`failed to read vendor package.json: ${e.message}`)
  }
}

function runNpm(args, cwd) {
  log(`$ npm ${args.join(" ")} (cwd=${cwd})`)
  try {
    execFileSync("npm", args, {
      cwd,
      stdio: "inherit",
      env: process.env,
      shell: process.platform === "win32",
    })
  } catch (e) {
    die(`npm ${args.join(" ")} failed: ${e.message}`)
  }
}

function copySeed(version) {
  const distEntry = join(VENDOR_DIR, "dist", "index.js")
  if (!existsSync(distEntry)) {
    die(`vendor build did not produce ${distEntry}`)
  }

  if (existsSync(SEED_DIR)) {
    rmSync(SEED_DIR, { recursive: true, force: true })
  }
  mkdirSync(SEED_DIR, { recursive: true })

  // Installable local package tree: package metadata + lock + built bin entry.
  for (const name of ["package.json", "package-lock.json", "README.md", "LICENSE"]) {
    const src = join(VENDOR_DIR, name)
    if (existsSync(src)) {
      cpSync(src, join(SEED_DIR, name))
    }
  }
  cpSync(join(VENDOR_DIR, "dist"), join(SEED_DIR, "dist"), { recursive: true })

  // Stamp so runtime integrity can cross-check the locked pin without reading
  // a nested node_modules path before install.
  writeFileSync(
    join(SEED_DIR, ".codeg-seed-version"),
    `${version}\n`,
    "utf8"
  )

  if (!existsSync(join(SEED_DIR, "package.json"))) {
    die("seed package.json missing after copy")
  }
  if (!existsSync(join(SEED_DIR, "dist", "index.js"))) {
    die("seed dist/index.js missing after copy")
  }
  log(`seed ready at ${SEED_DIR} (pin=${version})`)
}

function main() {
  if (process.env.CODEG_SKIP_CODEX_ACP_STAGE === "1") {
    log("CODEG_SKIP_CODEX_ACP_STAGE=1 — skipping codex-acp seed stage")
    return
  }

  log(`repo=${REPO_ROOT}`)
  const version = assertVendorPresent()
  log(`vendor pin=${version}`)
  runNpm(["ci"], VENDOR_DIR)
  runNpm(["run", "build"], VENDOR_DIR)
  copySeed(version)
}

main()
