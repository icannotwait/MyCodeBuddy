import assert from "node:assert/strict"
import { mkdtempSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

import {
  codexBundleScript,
  readCodexAcpVersion,
  sidecarDestination,
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

test("requires an initialized locked codex submodule", () => {
  const dir = mkdtempSync(join(tmpdir(), "codeg-codex-sidecar-"))
  writeFileSync(join(dir, "package.json"), '{"version":"1.1.2-mycodebuddy.1"}')
  assert.throws(() => readCodexAcpVersion(dir), /not initialized/)
  writeFileSync(join(dir, "package-lock.json"), "{}")
  assert.equal(readCodexAcpVersion(dir), "1.1.2-mycodebuddy.1")
})
