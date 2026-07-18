import assert from "node:assert/strict"
import test from "node:test"

import { sidecarDestination } from "./prepare-sidecars.mjs"

test("uses Tauri target-qualified sidecar names", () => {
  assert.equal(
    sidecarDestination("codeg-mcp", "x86_64-pc-windows-msvc"),
    "codeg-mcp-x86_64-pc-windows-msvc.exe"
  )
  assert.equal(
    sidecarDestination("codeg-mcp", "aarch64-apple-darwin"),
    "codeg-mcp-aarch64-apple-darwin"
  )
})
