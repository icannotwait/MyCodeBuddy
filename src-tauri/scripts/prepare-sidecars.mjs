#!/usr/bin/env node
//
// Prepare Tauri sidecars before `tauri build` / `tauri dev` consume them.
//
// What it does:
//   1. Resolves the target triple — `--target <triple>` arg, or
//      `TAURI_TARGET_TRIPLE` env, or the host's `rustc -vV` host triple.
//   2. Runs `cargo build --release --bin codeg-mcp --no-default-features`
//      for that triple from `src-tauri/`.
//   3. Copies the produced binary to
//      `src-tauri/binaries/codeg-mcp-<triple>{.exe}` so Tauri's externalBin
//      bundler picks it up under the bare name `codeg-mcp` at install time.
//
// Why a separate script (not inline in beforeBuildCommand / GitHub Actions):
//   - Cross-compile in release.yml passes `--target <triple>` so we honour
//     the matrix triple rather than rebuilding for the host.
//   - Local `pnpm tauri dev` / `pnpm tauri build` invoke it without args and
//     get a host-triple build, so the externalBin lookup still finds a file.
//   - Skippable: set `CODEG_SKIP_SIDECAR=1` when iterating on the frontend
//     and you don't care about delegation.
//
// Intentionally Node-only (no shell): runs identically on macOS, Linux,
// Windows GitHub runners.

import { execFileSync } from "node:child_process"
import {
  existsSync,
  copyFileSync,
  mkdirSync,
  chmodSync,
  readFileSync,
  rmSync,
  statSync,
} from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import process from "node:process"

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url))
const SRC_TAURI = resolve(SCRIPT_DIR, "..")
const BINARIES_DIR = join(SRC_TAURI, "binaries")
const BIN_NAME = "codeg-mcp"
const CODEX_ACP_VERSION = "1.1.2-mycodebuddy.1"
const CODEX_ACP_DIR = join(SRC_TAURI, "vendor", "codex-acp")

export function codexBundleScript(target) {
  return target === "x86_64-pc-windows-msvc" ? "bundle:win-x64" : null
}

export function sidecarDestination(name, target) {
  const ext = target.includes("windows") ? ".exe" : ""
  return `${name}-${target}${ext}`
}

export function readCodexAcpVersion(sourceDir) {
  const manifest = join(sourceDir, "package.json")
  const lockfile = join(sourceDir, "package-lock.json")
  if (!existsSync(manifest) || !existsSync(lockfile)) {
    throw new Error(`codex-acp submodule is not initialized at ${sourceDir}`)
  }
  return JSON.parse(readFileSync(manifest, "utf8")).version
}

function log(msg) {
  console.log(`[prepare-sidecars] ${msg}`)
}

function die(msg) {
  console.error(`[prepare-sidecars][ERROR] ${msg}`)
  process.exit(1)
}

function parseArgs(argv) {
  const args = { target: null }
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === "--target" && argv[i + 1]) {
      args.target = argv[++i]
    } else if (a.startsWith("--target=")) {
      args.target = a.slice("--target=".length)
    }
  }
  return args
}

function resolveHostTriple() {
  try {
    const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
    const line = out.split(/\r?\n/).find((l) => l.startsWith("host:"))
    if (!line) throw new Error("rustc -vV missing host: line")
    return line.replace(/^host:\s*/, "").trim()
  } catch (e) {
    die(`cannot determine host triple via rustc -vV: ${e.message}`)
  }
}

function main() {
  if (process.env.CODEG_SKIP_SIDECAR === "1") {
    log("CODEG_SKIP_SIDECAR=1 — skipping sidecar preparation")
    return
  }

  const { target: cliTarget } = parseArgs(process.argv.slice(2))
  const target =
    cliTarget || process.env.TAURI_TARGET_TRIPLE || resolveHostTriple()
  const isWindows = target.includes("windows")
  const ext = isWindows ? ".exe" : ""

  log(`target triple: ${target}`)
  log(`building ${BIN_NAME} (--release --no-default-features)`)

  // cargo build needs to run from src-tauri so it resolves the local manifest
  // and shares the swatinem/rust-cache key with other cargo invocations.
  // `--no-default-features` keeps codeg-mcp free of the Tauri runtime deps —
  // the bin's required-features is empty, so this just enables cross-compile
  // without dragging in macOS-private-api / Linux WebKit / Windows WebView2.
  execFileSync(
    "cargo",
    [
      "build",
      "--release",
      "--bin",
      BIN_NAME,
      "--no-default-features",
      "--target",
      target,
    ],
    { stdio: "inherit", cwd: SRC_TAURI }
  )

  const built = join(
    SRC_TAURI,
    "target",
    target,
    "release",
    `${BIN_NAME}${ext}`
  )
  if (!existsSync(built)) {
    die(`expected ${built} after cargo build, but it does not exist`)
  }

  mkdirSync(BINARIES_DIR, { recursive: true })
  const dest = join(BINARIES_DIR, sidecarDestination(BIN_NAME, target))
  copyFileSync(built, dest)
  if (!isWindows) {
    // copyFileSync preserves modes on POSIX, but be explicit for tarball
    // sources that may strip the +x bit.
    chmodSync(dest, 0o755)
  }
  log(`sidecar staged at ${dest}`)

  const codexScript = codexBundleScript(target)
  if (!codexScript || process.env.CODEG_SKIP_CODEX_ACP_SIDECAR === "1") {
    return
  }
  const version = readCodexAcpVersion(CODEX_ACP_DIR)
  if (version !== CODEX_ACP_VERSION) {
    die(`expected codex-acp ${CODEX_ACP_VERSION}, found ${version}`)
  }
  const npm = process.platform === "win32" ? "npm.cmd" : "npm"
  for (const args of [
    ["ci"],
    ["run", "typecheck"],
    ["test"],
    ["run", codexScript],
  ]) {
    execFileSync(npm, args, { stdio: "inherit", cwd: CODEX_ACP_DIR })
  }
  const codexBuilt = join(
    CODEX_ACP_DIR,
    "dist",
    "bin",
    "codex-acp-x64-windows.exe"
  )
  const codexDest = join(
    BINARIES_DIR,
    sidecarDestination("codex-acp", target)
  )
  rmSync(codexDest, { force: true })
  copyFileSync(codexBuilt, codexDest)
  if (statSync(codexDest).size <= 0) {
    die(`staged codex-acp is empty: ${codexDest}`)
  }
  const reported = execFileSync(codexDest, ["--version"], {
    encoding: "utf8",
  }).trim()
  const expected = `@agentclientprotocol/codex-acp ${CODEX_ACP_VERSION}`
  if (reported !== expected) {
    die(`codex-acp version mismatch: expected ${expected}, got ${reported}`)
  }
  log(`sidecar staged at ${codexDest}`)
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main()
}
