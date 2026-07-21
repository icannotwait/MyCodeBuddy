#!/usr/bin/env node
//
// Local-only accelerated Tauri desktop package build.
//
// Default `pnpm tauri build` always runs a full beforeBuild pipeline
// (licenses → Next → sidecar). This helper skips steps whose inputs and
// artifacts are unchanged, which cuts local iteration when only Rust (or only
// frontend) changed.
//
// Does NOT replace the default build path used by CI/release.
//
// Usage:
//   pnpm tauri:build:fast
//   pnpm tauri:build:fast -- --force
//   pnpm tauri:build:fast -- --force-frontend --force-sidecar
//   pnpm tauri:build:fast -- --skip-sidecar
//   pnpm tauri:build:fast -- --bundles app
//   pnpm tauri:before-build:fast          # only the smart beforeBuild hook
//
// Flags (before `--` passthrough to `tauri build`):
//   --force              Rebuild licenses + frontend + sidecar
//   --force-licenses     Force license generation
//   --force-frontend     Force `pnpm build`
//   --force-sidecar      Force codeg-mcp rebuild
//   --skip-sidecar       Same as CODEG_SKIP_SIDECAR=1
//   --before-build-only  Run the smart beforeBuild steps and exit
//
// Cache stamps live in `.codeg-build-cache/` (gitignored).

import { createHash } from "node:crypto"
import { execFileSync, spawnSync } from "node:child_process"
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs"
import { dirname, join, relative, resolve, sep } from "node:path"
import { fileURLToPath, pathToFileURL } from "node:url"
import process from "node:process"

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url))
export const REPO_ROOT = resolve(SCRIPT_DIR, "..")
export const CACHE_DIR_NAME = ".codeg-build-cache"
export const STAMPS_FILE_NAME = "stamps.json"
export const FAST_CONFIG_FILE_NAME = "tauri.fast.conf.json"

const SKIP_DIR_NAMES = new Set([
  ".git",
  ".next",
  ".codeg-build-cache",
  "node_modules",
  "target",
  "coverage",
  "out",
  "binaries",
  "gen",
  ".worktrees",
  "public/vs",
])

function log(msg) {
  console.log(`[tauri-build-fast] ${msg}`)
}

function die(msg) {
  console.error(`[tauri-build-fast][ERROR] ${msg}`)
  process.exit(1)
}

export function cacheDir(repoRoot = REPO_ROOT) {
  return join(repoRoot, CACHE_DIR_NAME)
}

export function stampsPath(repoRoot = REPO_ROOT) {
  return join(cacheDir(repoRoot), STAMPS_FILE_NAME)
}

export function fastConfigPath(repoRoot = REPO_ROOT) {
  return join(cacheDir(repoRoot), FAST_CONFIG_FILE_NAME)
}

export function loadStamps(repoRoot = REPO_ROOT) {
  const path = stampsPath(repoRoot)
  if (!existsSync(path)) return {}
  try {
    return JSON.parse(readFileSync(path, "utf8"))
  } catch {
    return {}
  }
}

export function saveStamps(stamps, repoRoot = REPO_ROOT) {
  const dir = cacheDir(repoRoot)
  mkdirSync(dir, { recursive: true })
  writeFileSync(stampsPath(repoRoot), `${JSON.stringify(stamps, null, 2)}\n`)
}

function shouldSkipDir(name, relativeDir) {
  if (SKIP_DIR_NAMES.has(name)) return true
  // Monaco copy is large and already covered via package lock for license/frontend.
  if (relativeDir.replace(/\\/g, "/").endsWith("public/vs")) return true
  if (name === "vs" && relativeDir.replace(/\\/g, "/").endsWith("public")) {
    return true
  }
  return false
}

/**
 * Fingerprint a set of roots by relative path + size + mtime.
 * Fast enough for local use; content hashing the whole tree would be slower
 * than a skip decision is worth.
 */
export function fingerprintRoots(repoRoot, roots) {
  const hash = createHash("sha256")
  const entries = []

  const visit = (absPath) => {
    let st
    try {
      st = statSync(absPath)
    } catch {
      return
    }
    const rel = relative(repoRoot, absPath).split(sep).join("/")
    if (st.isDirectory()) {
      const name = absPath.split(/[/\\]/).pop()
      const parentRel = relative(repoRoot, dirname(absPath)).split(sep).join("/")
      if (shouldSkipDir(name, parentRel)) return
      let children
      try {
        children = readdirSync(absPath)
      } catch {
        return
      }
      children.sort()
      for (const child of children) {
        visit(join(absPath, child))
      }
      return
    }
    if (!st.isFile()) return
    entries.push(`${rel}\0${st.size}\0${Math.trunc(st.mtimeMs)}`)
  }

  for (const root of roots) {
    const abs = resolve(repoRoot, root)
    if (!existsSync(abs)) {
      entries.push(`missing:${root.replace(/\\/g, "/")}`)
      continue
    }
    visit(abs)
  }

  entries.sort()
  for (const e of entries) hash.update(e)
  hash.update("\n")
  return hash.digest("hex")
}

export function resolveHostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  const line = out.split(/\r?\n/).find((l) => l.startsWith("host:"))
  if (!line) throw new Error("rustc -vV missing host: line")
  return line.replace(/^host:\s*/, "").trim()
}

export function sidecarArtifactPath(repoRoot, triple) {
  const ext = triple.includes("windows") ? ".exe" : ""
  return join(
    repoRoot,
    "src-tauri",
    "binaries",
    `codeg-mcp-${triple}${ext}`
  )
}

export function frontendArtifactReady(repoRoot) {
  return existsSync(join(repoRoot, "out", "index.html"))
}

export function licensesArtifactReady(repoRoot) {
  return existsSync(
    join(repoRoot, "src-tauri", "resources", "THIRD_PARTY_LICENSES.txt")
  )
}

export const FINGERPRINT_ROOTS = {
  licenses: [
    "package.json",
    "pnpm-lock.yaml",
    "scripts/third-party-licenses.mjs",
    "src-tauri/Cargo.toml",
    "src-tauri/Cargo.lock",
  ],
  frontend: [
    "package.json",
    "pnpm-lock.yaml",
    "next.config.ts",
    "postcss.config.mjs",
    "tsconfig.json",
    "components.json",
    "src",
    "public",
  ],
  sidecar: [
    "src-tauri/Cargo.toml",
    "src-tauri/Cargo.lock",
    "src-tauri/build.rs",
    "src-tauri/src",
    "src-tauri/vendor/sacp-tokio",
    "src-tauri/scripts/prepare-sidecars.mjs",
  ],
}

export function computeStepFingerprint(repoRoot, step, extra = "") {
  const roots = FINGERPRINT_ROOTS[step]
  if (!roots) throw new Error(`unknown fingerprint step: ${step}`)
  const base = fingerprintRoots(repoRoot, roots)
  if (!extra) return base
  return createHash("sha256").update(base).update("\0").update(extra).digest("hex")
}

export function canSkipStep({
  stamps,
  step,
  fingerprint,
  artifactReady,
  force,
}) {
  if (force) return false
  if (!artifactReady) return false
  return stamps[step] === fingerprint
}

function pnpmCommand() {
  return process.platform === "win32" ? "pnpm.cmd" : "pnpm"
}

function runPnpm(args, repoRoot) {
  // Prefer execFile-style spawn without shell so argv is not concatenated.
  // On Windows pnpm is pnpm.cmd; Node can exec it directly via PATHEXT.
  const result = spawnSync(pnpmCommand(), args, {
    cwd: repoRoot,
    stdio: "inherit",
    env: process.env,
    windowsHide: true,
  })
  if (result.error) {
    die(`failed to spawn pnpm: ${result.error.message}`)
  }
  if (result.status !== 0) {
    die(`command failed: pnpm ${args.join(" ")} (exit ${result.status})`)
  }
}

function runNode(scriptRel, args, repoRoot) {
  const result = spawnSync(
    process.execPath,
    [join(repoRoot, scriptRel), ...args],
    {
      cwd: repoRoot,
      stdio: "inherit",
      env: process.env,
    }
  )
  if (result.status !== 0) {
    die(`command failed: node ${scriptRel} ${args.join(" ")} (exit ${result.status})`)
  }
}

/**
 * Smart beforeBuild: skip licenses / frontend / sidecar when fingerprints match.
 */
export function runBeforeBuildFast(options = {}) {
  const repoRoot = options.repoRoot || REPO_ROOT
  const force = Boolean(options.force)
  const forceLicenses = force || Boolean(options.forceLicenses)
  const forceFrontend = force || Boolean(options.forceFrontend)
  const forceSidecar = force || Boolean(options.forceSidecar)
  const skipSidecar =
    Boolean(options.skipSidecar) || process.env.CODEG_SKIP_SIDECAR === "1"

  const stamps = loadStamps(repoRoot)
  let triple = ""
  try {
    triple = resolveHostTriple()
  } catch (e) {
    die(`cannot resolve host triple: ${e.message}`)
  }

  // --- licenses ---
  const licensesFp = computeStepFingerprint(repoRoot, "licenses")
  if (
    canSkipStep({
      stamps,
      step: "licenses",
      fingerprint: licensesFp,
      artifactReady: licensesArtifactReady(repoRoot),
      force: forceLicenses,
    })
  ) {
    log("licenses: skip (unchanged)")
  } else {
    log("licenses: generate")
    runPnpm(["licenses:generate"], repoRoot)
    stamps.licenses = licensesFp
    saveStamps(stamps, repoRoot)
  }

  // --- frontend ---
  const frontendFp = computeStepFingerprint(repoRoot, "frontend")
  if (
    canSkipStep({
      stamps,
      step: "frontend",
      fingerprint: frontendFp,
      artifactReady: frontendArtifactReady(repoRoot),
      force: forceFrontend,
    })
  ) {
    log("frontend: skip (unchanged, out/ present)")
  } else {
    log("frontend: pnpm build")
    runPnpm(["build"], repoRoot)
    if (!frontendArtifactReady(repoRoot)) {
      die("frontend build finished but out/index.html is missing")
    }
    stamps.frontend = frontendFp
    saveStamps(stamps, repoRoot)
  }

  // --- sidecar ---
  if (skipSidecar) {
    log("sidecar: skip (CODEG_SKIP_SIDECAR / --skip-sidecar)")
  } else {
    const sidecarFp = computeStepFingerprint(repoRoot, "sidecar", triple)
    const artifact = sidecarArtifactPath(repoRoot, triple)
    if (
      canSkipStep({
        stamps,
        step: "sidecar",
        fingerprint: sidecarFp,
        artifactReady: existsSync(artifact),
        force: forceSidecar,
      })
    ) {
      log(`sidecar: skip (unchanged, ${relative(repoRoot, artifact)})`)
    } else {
      log("sidecar: prepare-sidecars")
      runNode("src-tauri/scripts/prepare-sidecars.mjs", [], repoRoot)
      if (!existsSync(artifact)) {
        die(`sidecar expected at ${artifact} after prepare-sidecars`)
      }
      stamps.sidecar = sidecarFp
      stamps.sidecarTriple = triple
      saveStamps(stamps, repoRoot)
    }
  }

  log("beforeBuild complete")
  return { stamps, triple }
}

export function writeFastTauriConfig(repoRoot = REPO_ROOT) {
  const dir = cacheDir(repoRoot)
  mkdirSync(dir, { recursive: true })
  // beforeBuildCommand runs with package manager cwd = repo root.
  const config = {
    build: {
      beforeBuildCommand: "node scripts/tauri-build-fast.mjs --before-build-only",
    },
  }
  const path = fastConfigPath(repoRoot)
  writeFileSync(path, `${JSON.stringify(config, null, 2)}\n`)
  return path
}

export function parseArgs(argv) {
  const options = {
    force: false,
    forceLicenses: false,
    forceFrontend: false,
    forceSidecar: false,
    skipSidecar: false,
    beforeBuildOnly: false,
    tauriArgs: [],
  }

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === "--") {
      options.tauriArgs.push(...argv.slice(i + 1))
      break
    }
    switch (a) {
      case "--force":
        options.force = true
        break
      case "--force-licenses":
        options.forceLicenses = true
        break
      case "--force-frontend":
        options.forceFrontend = true
        break
      case "--force-sidecar":
        options.forceSidecar = true
        break
      case "--skip-sidecar":
        options.skipSidecar = true
        break
      case "--before-build-only":
        options.beforeBuildOnly = true
        break
      case "--help":
      case "-h":
        options.help = true
        break
      default:
        // Unknown flags go to tauri (e.g. --bundles app)
        options.tauriArgs.push(a)
        break
    }
  }
  return options
}

function printHelp() {
  console.log(`Local accelerated Tauri build

Usage:
  pnpm tauri:build:fast [options] [-- tauri-build-args...]
  node scripts/tauri-build-fast.mjs [options]

Options:
  --force              Force licenses + frontend + sidecar
  --force-licenses     Force license generation only
  --force-frontend     Force Next.js production build only
  --force-sidecar      Force codeg-mcp sidecar rebuild only
  --skip-sidecar       Skip sidecar (delegation disabled in that build)
  --before-build-only  Only run the smart beforeBuild steps
  -h, --help           Show this help

Examples:
  pnpm tauri:build:fast
  pnpm tauri:build:fast -- --force
  pnpm tauri:build:fast -- --skip-sidecar --bundles app
`)
}

export function runTauriBuildFast(options = {}) {
  const repoRoot = options.repoRoot || REPO_ROOT
  if (options.skipSidecar) {
    process.env.CODEG_SKIP_SIDECAR = "1"
  }

  // Propagate force flags to the nested beforeBuild invocation via env,
  // because Tauri spawns beforeBuildCommand as a fresh process.
  process.env.CODEG_BUILD_FAST_FORCE = options.force ? "1" : "0"
  process.env.CODEG_BUILD_FAST_FORCE_LICENSES = options.forceLicenses
    ? "1"
    : "0"
  process.env.CODEG_BUILD_FAST_FORCE_FRONTEND = options.forceFrontend
    ? "1"
    : "0"
  process.env.CODEG_BUILD_FAST_FORCE_SIDECAR = options.forceSidecar
    ? "1"
    : "0"
  if (options.skipSidecar) {
    process.env.CODEG_SKIP_SIDECAR = "1"
  }

  const configPath = writeFastTauriConfig(repoRoot)
  const args = [
    "tauri",
    "build",
    "--config",
    configPath,
    ...(options.tauriArgs || []),
  ]
  log(`running: pnpm ${args.join(" ")}`)
  const result = spawnSync(pnpmCommand(), args, {
    cwd: repoRoot,
    stdio: "inherit",
    env: process.env,
    windowsHide: true,
  })
  if (result.error) {
    die(`failed to spawn pnpm: ${result.error.message}`)
  }
  if (result.status !== 0) {
    die(`tauri build failed (exit ${result.status})`)
  }
  log("done")
}

function optionsFromEnv(base) {
  return {
    ...base,
    force: base.force || process.env.CODEG_BUILD_FAST_FORCE === "1",
    forceLicenses:
      base.forceLicenses ||
      process.env.CODEG_BUILD_FAST_FORCE_LICENSES === "1",
    forceFrontend:
      base.forceFrontend ||
      process.env.CODEG_BUILD_FAST_FORCE_FRONTEND === "1",
    forceSidecar:
      base.forceSidecar ||
      process.env.CODEG_BUILD_FAST_FORCE_SIDECAR === "1",
    skipSidecar:
      base.skipSidecar || process.env.CODEG_SKIP_SIDECAR === "1",
  }
}

function main() {
  const options = optionsFromEnv(parseArgs(process.argv.slice(2)))
  if (options.help) {
    printHelp()
    return
  }
  if (options.beforeBuildOnly) {
    runBeforeBuildFast(options)
    return
  }
  runTauriBuildFast(options)
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main()
}
