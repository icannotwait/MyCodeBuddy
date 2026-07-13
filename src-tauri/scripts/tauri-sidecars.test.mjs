import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"

const tauriDir = join(dirname(fileURLToPath(import.meta.url)), "..")

test("default Tauri bundle includes the Codex ACP adapter", () => {
  const config = JSON.parse(
    readFileSync(join(tauriDir, "tauri.conf.json"), "utf8")
  )

  assert.ok(
    config.bundle.externalBin.includes("binaries/codex-acp"),
    "default local bundles must include codex-acp"
  )
})
