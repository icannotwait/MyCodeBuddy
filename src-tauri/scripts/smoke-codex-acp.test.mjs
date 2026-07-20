import assert from "node:assert/strict"
import { join } from "node:path"
import test from "node:test"

import {
  assertDynamicModelList,
  classifySessionNewResponse,
  isAuthenticationRequiredError,
  isUsableHostCodexPath,
  preferHostCodexCandidate,
  resolveHostCodexPath,
} from "./smoke-codex-acp.mjs"

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

test("session/new smoke requires a non-empty dynamic model list", () => {
  assert.equal(
    assertDynamicModelList({
      models: {
        availableModels: [{ modelId: "gpt-5.5" }, { modelId: "gpt-5.5-codex" }],
      },
    }),
    2
  )

  assert.throws(
    () => assertDynamicModelList({ models: { availableModels: [] } }),
    /did not return any models/
  )
})

test("Windows host Codex path rejects PowerShell shims and prefers .cmd", () => {
  assert.equal(isUsableHostCodexPath("C:\\npm\\codex.ps1", "win32"), false)
  assert.equal(isUsableHostCodexPath("C:\\npm\\codex.cmd", "win32"), true)

  const existing = new Set([
    "C:\\npm\\codex.ps1",
    "C:\\npm\\codex",
    "C:\\npm\\codex.cmd",
  ])
  assert.equal(
    preferHostCodexCandidate(
      ["C:\\npm\\codex.ps1", "C:\\npm\\codex", "C:\\npm\\codex.cmd"],
      {
        platform: "win32",
        exists: (candidate) => existing.has(candidate),
      }
    ),
    "C:\\npm\\codex.cmd"
  )

  // Ambient CODEX_PATH pointing at .ps1 must not win over APPDATA .cmd.
  const appData = join("fixtures", "appdata")
  const cmd = join(appData, "npm", "codex.cmd")
  assert.equal(
    resolveHostCodexPath({
      env: {
        CODEX_PATH: join(appData, "npm", "codex.ps1"),
        APPDATA: appData,
      },
      platform: "win32",
      exists: (candidate) =>
        candidate === cmd || candidate.endsWith("codex.ps1"),
      findOnPath: () => null,
    }),
    cmd
  )
})

test("Authentication required is recognized for clean CI packaging smoke", () => {
  assert.equal(
    isAuthenticationRequiredError({
      code: -32000,
      message: "Authentication required",
    }),
    true
  )
  assert.equal(isAuthenticationRequiredError("Authentication required"), true)
  assert.equal(
    isAuthenticationRequiredError({ code: -32000, message: "other" }),
    false
  )
})

test("session/new classifies models or auth-required packaging paths", () => {
  assert.deepEqual(
    classifySessionNewResponse({
      result: {
        models: {
          availableModels: [{ modelId: "gpt-5.5" }],
        },
      },
    }),
    { kind: "models", modelCount: 1 }
  )

  assert.deepEqual(
    classifySessionNewResponse({
      error: { code: -32000, message: "Authentication required" },
    }),
    { kind: "auth_required" }
  )

  assert.throws(
    () =>
      classifySessionNewResponse({
        error: { code: -32603, message: "internal boom" },
      }),
    /session\/new failed/
  )
})
