import assert from "node:assert/strict"
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import {
  codexBundleScript,
  codexBundleEnv,
  npmCommandInvocation,
  readCodexAcpVersion,
  sidecarDestination,
  stageCodexCompileRuntime,
} from "./prepare-sidecars.mjs"

test("maps only the Windows x64 target to the codex bundle", () => {
  assert.equal(codexBundleScript("x86_64-pc-windows-msvc"), "bundle:win-x64")
  assert.equal(codexBundleScript("aarch64-pc-windows-msvc"), null)
  assert.equal(codexBundleScript("aarch64-apple-darwin"), null)
})

test("uses Tauri target-qualified sidecar names", () => {
  assert.equal(
    sidecarDestination("codex-acp", "x86_64-pc-windows-msvc"),
    "codex-acp-x86_64-pc-windows-msvc.exe"
  )
})

test("runs npm directly on POSIX", () => {
  assert.deepEqual(npmCommandInvocation(["ci"], "darwin"), {
    command: "npm",
    args: ["ci"],
  })
})

test("runs npm.cmd through cmd.exe on Windows", () => {
  assert.deepEqual(
    npmCommandInvocation(["ci"], "win32", "C:\\Windows\\System32\\cmd.exe"),
    {
      command: "C:\\Windows\\System32\\cmd.exe",
      args: ["/d", "/s", "/c", "npm.cmd", "ci"],
    }
  )
})

test("stages the locked Bun Windows runtime for offline compilation", () => {
  const dir = mkdtempSync(join(tmpdir(), "codeg-bun-runtime-"))
  const runtime = join(
    dir,
    "node_modules",
    "@oven",
    "bun-windows-x64-baseline",
    "bin",
    "bun.exe"
  )
  mkdirSync(join(runtime, ".."), { recursive: true })
  writeFileSync(runtime, "bun-runtime")

  const staged = stageCodexCompileRuntime(dir, "x86_64-pc-windows-msvc")

  assert.equal(staged, join(dir, "bun-windows-x64-baseline-v1.3.14"))
  assert.equal(readFileSync(staged, "utf8"), "bun-runtime")
})

test("adds the locked Bun runtime directory to PATH for the Windows bundle", () => {
  const dir = "C:\\repo\\codex-acp"
  const env = codexBundleEnv(
    dir,
    "x86_64-pc-windows-msvc",
    { Path: "C:\\Windows\\System32" },
    ";"
  )

  assert.equal(
    env.Path,
    `${join(
      dir,
      "node_modules",
      "@oven",
      "bun-windows-x64-baseline",
      "bin"
    )};C:\\Windows\\System32`
  )
})

test("requires the locked Bun Windows runtime before compilation", () => {
  const dir = mkdtempSync(join(tmpdir(), "codeg-bun-runtime-missing-"))

  assert.throws(
    () => stageCodexCompileRuntime(dir, "x86_64-pc-windows-msvc"),
    /locked Bun compile runtime is missing/
  )
})

test("requires an initialized locked codex submodule", () => {
  const dir = mkdtempSync(join(tmpdir(), "codeg-codex-sidecar-"))
  writeFileSync(join(dir, "package.json"), '{"version":"1.1.2-mycodebuddy.2"}')
  assert.throws(() => readCodexAcpVersion(dir), /not initialized/)
  writeFileSync(join(dir, "package-lock.json"), "{}")
  assert.equal(readCodexAcpVersion(dir), "1.1.2-mycodebuddy.2")
})
