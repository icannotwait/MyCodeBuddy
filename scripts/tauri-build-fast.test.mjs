import assert from "node:assert/strict"
import {
  mkdirSync,
  mkdtempSync,
  writeFileSync,
  rmSync,
  utimesSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"
import {
  canSkipStep,
  computeStepFingerprint,
  fingerprintRoots,
  FINGERPRINT_ROOTS,
  parseArgs,
  sidecarArtifactPath,
} from "./tauri-build-fast.mjs"

function makeTempRepo() {
  const root = mkdtempSync(join(tmpdir(), "codeg-build-fast-"))
  const write = (rel, content = "x") => {
    const abs = join(root, rel)
    mkdirSync(join(abs, ".."), { recursive: true })
    writeFileSync(abs, content)
    return abs
  }
  write("package.json", "{}")
  write("pnpm-lock.yaml", "lock")
  write("scripts/third-party-licenses.mjs", "// licenses")
  write("src-tauri/Cargo.toml", "[package]\nname='codeg'")
  write("src-tauri/Cargo.lock", "# lock")
  write("src-tauri/build.rs", "fn main() {}")
  write("src-tauri/src/lib.rs", "pub fn x() {}")
  write("src-tauri/scripts/prepare-sidecars.mjs", "// sidecar")
  write("src-tauri/vendor/sacp-tokio/Cargo.toml", "[package]")
  write("next.config.ts", "export default {}")
  write("postcss.config.mjs", "export default {}")
  write("tsconfig.json", "{}")
  write("components.json", "{}")
  write("src/app/page.tsx", "export default function Page() {}")
  write("public/icon.svg", "<svg />")
  write("src/i18n/messages/en.json", "{}")
  return root
}

test("fingerprintRoots is stable for identical trees", () => {
  const root = makeTempRepo()
  try {
    const a = fingerprintRoots(root, FINGERPRINT_ROOTS.frontend)
    const b = fingerprintRoots(root, FINGERPRINT_ROOTS.frontend)
    assert.equal(a, b)
    assert.match(a, /^[a-f0-9]{64}$/)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("fingerprintRoots changes when a tracked file changes", () => {
  const root = makeTempRepo()
  try {
    const before = computeStepFingerprint(root, "frontend")
    const page = join(root, "src/app/page.tsx")
    // Bump mtime and content so both size and mtime differ.
    writeFileSync(page, "export default function Page() { return 1 }")
    const now = new Date()
    utimesSync(page, now, now)
    const after = computeStepFingerprint(root, "frontend")
    assert.notEqual(before, after)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("canSkipStep requires matching stamp and ready artifact", () => {
  assert.equal(
    canSkipStep({
      stamps: { frontend: "abc" },
      step: "frontend",
      fingerprint: "abc",
      artifactReady: true,
      force: false,
    }),
    true
  )
  assert.equal(
    canSkipStep({
      stamps: { frontend: "abc" },
      step: "frontend",
      fingerprint: "abc",
      artifactReady: false,
      force: false,
    }),
    false
  )
  assert.equal(
    canSkipStep({
      stamps: { frontend: "abc" },
      step: "frontend",
      fingerprint: "def",
      artifactReady: true,
      force: false,
    }),
    false
  )
  assert.equal(
    canSkipStep({
      stamps: { frontend: "abc" },
      step: "frontend",
      fingerprint: "abc",
      artifactReady: true,
      force: true,
    }),
    false
  )
})

test("sidecar fingerprint includes host triple extra", () => {
  const root = makeTempRepo()
  try {
    const a = computeStepFingerprint(root, "sidecar", "x86_64-pc-windows-msvc")
    const b = computeStepFingerprint(root, "sidecar", "aarch64-apple-darwin")
    assert.notEqual(a, b)
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test("sidecarArtifactPath uses .exe on windows triples", () => {
  const win = sidecarArtifactPath("/repo", "x86_64-pc-windows-msvc")
  assert.ok(win.endsWith("codeg-mcp-x86_64-pc-windows-msvc.exe"))
  const unix = sidecarArtifactPath("/repo", "x86_64-apple-darwin")
  assert.ok(unix.endsWith("codeg-mcp-x86_64-apple-darwin"))
  assert.ok(!unix.endsWith(".exe"))
})

test("parseArgs separates force flags from tauri passthrough", () => {
  const opts = parseArgs([
    "--force-frontend",
    "--skip-sidecar",
    "--bundles",
    "app",
  ])
  assert.equal(opts.forceFrontend, true)
  assert.equal(opts.skipSidecar, true)
  assert.deepEqual(opts.tauriArgs, ["--bundles", "app"])
})

test("parseArgs supports -- separator", () => {
  const opts = parseArgs(["--force", "--", "--bundles", "nsis"])
  assert.equal(opts.force, true)
  assert.deepEqual(opts.tauriArgs, ["--bundles", "nsis"])
})
