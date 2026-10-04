import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"

const tauriDir = join(dirname(fileURLToPath(import.meta.url)), "..")

test("default Tauri bundle ships the MCP and computer helper sidecars", () => {
  const config = JSON.parse(
    readFileSync(join(tauriDir, "tauri.conf.json"), "utf8")
  )

  assert.deepEqual(
    config.bundle.externalBin,
    ["binaries/codeg-mcp", "binaries/codeg-computer-helper"],
    "Ship both opt-in companions; never bundle the server or Codex ACP"
  )
})

test("release Tauri bundle ships the MCP and computer helper sidecars", () => {
  const config = JSON.parse(
    readFileSync(join(tauriDir, "tauri.release.conf.json"), "utf8")
  )

  assert.deepEqual(
    config.bundle.externalBin,
    ["binaries/codeg-mcp", "binaries/codeg-computer-helper"],
    "Ship both opt-in companions; never bundle the server or Codex ACP"
  )
})
