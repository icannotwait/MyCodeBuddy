import assert from "node:assert/strict"
import { join } from "node:path"
import test from "node:test"

import { resolveHostCodexPath } from "./smoke-codex-acp.mjs"

test("PATH hit wins over APPDATA fallback", () => {
  const appData = join("fixtures", "appdata")
  const appDataCmd = join(appData, "npm", "codex.cmd")
  const pathHit = join("fixtures", "path", "codex")
  const existing = new Set([appDataCmd, pathHit])

  const selected = resolveHostCodexPath({
    env: { APPDATA: appData },
    exists: (candidate) => existing.has(candidate),
    findOnPath: () => pathHit,
  })

  assert.equal(selected, pathHit)
})

test("APPDATA cmd wins over JavaScript fallback", () => {
  const appData = join("fixtures", "appdata")
  const cmd = join(appData, "npm", "codex.cmd")
  const js = join(
    appData,
    "npm",
    "node_modules",
    "@openai",
    "codex",
    "bin",
    "codex.js"
  )
  const existing = new Set([cmd, js])

  const selected = resolveHostCodexPath({
    env: { APPDATA: appData },
    exists: (candidate) => existing.has(candidate),
    findOnPath: () => null,
  })

  assert.equal(selected, cmd)
})
