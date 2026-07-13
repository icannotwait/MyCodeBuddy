#!/usr/bin/env node

/**
 * Smoke-test the bundled codex-acp sidecar under CLI mode.
 *
 * Exit 0 only if:
 *  1. `--version` equals `@agentclientprotocol/codex-acp 1.1.2-mycodebuddy.1`
 *  2. ACP `initialize` succeeds with CODEX_ACP_USE_CLI=1 and CODEX_PATH set
 *  3. stdout/stderr do not contain `Cannot find module '@openai/codex/bin/codex.js'`
 *
 * Usage: node smoke-codex-acp.mjs <codex-acp.exe>
 */

import { spawn, spawnSync } from "node:child_process"
import { existsSync } from "node:fs"
import { join, resolve } from "node:path"
import { pathToFileURL } from "node:url"

const EXPECTED_VERSION = "@agentclientprotocol/codex-acp 1.1.2-mycodebuddy.1"
const MISSING_MODULE = "Cannot find module '@openai/codex/bin/codex.js'"
const INIT_TIMEOUT_MS = 15_000

export function findCodexOnPath({
  platform = process.platform,
  run = spawnSync,
  exists = existsSync,
} = {}) {
  const locator = platform === "win32" ? "where.exe" : "which"
  const result = run(locator, ["codex"], {
    encoding: "utf8",
    windowsHide: true,
  })
  if (result.status !== 0) {
    return null
  }

  return (
    (result.stdout ?? "")
      .split(/\r?\n/)
      .map((candidate) => candidate.trim())
      .find((candidate) => candidate && exists(candidate)) ?? null
  )
}

export function resolveHostCodexPath({
  env = process.env,
  exists = existsSync,
  findOnPath,
} = {}) {
  if (env.CODEX_PATH && exists(env.CODEX_PATH)) {
    return env.CODEX_PATH
  }

  const pathHit = (findOnPath ?? (() => findCodexOnPath({ exists })))()
  if (pathHit && exists(pathHit)) {
    return pathHit
  }

  const appData = env.APPDATA
  if (appData) {
    const cmd = join(appData, "npm", "codex.cmd")
    if (exists(cmd)) {
      return cmd
    }

    const js = join(
      appData,
      "npm",
      "node_modules",
      "@openai",
      "codex",
      "bin",
      "codex.js"
    )
    if (exists(js)) {
      return js
    }
  }

  return null
}

function assertNoMissingModule(label, text) {
  if (text.includes(MISSING_MODULE)) {
    throw new Error(
      `${label} contains missing @openai/codex module error:\n${text}`
    )
  }
}

function checkVersion(binary, env) {
  const adapter = spawnSync(binary, ["--version"], {
    encoding: "utf8",
    timeout: 20_000,
    env,
  })

  const out = `${adapter.stdout ?? ""}${adapter.stderr ?? ""}`
  assertNoMissingModule("--version", out)

  if (
    adapter.status !== 0 ||
    (adapter.stdout ?? "").trim() !== EXPECTED_VERSION
  ) {
    throw new Error(
      `adapter version smoke test failed (status=${adapter.status}): ${out}`
    )
  }

  return (adapter.stdout ?? "").trim()
}

function initializeOnce(binary, env) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(binary, [], {
      env,
      stdio: ["pipe", "pipe", "pipe"],
    })

    let stdout = ""
    let stderr = ""
    let settled = false

    const finish = (err, value) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      try {
        child.kill()
      } catch {
        // ignore kill races after exit
      }
      if (err) reject(err)
      else resolvePromise(value)
    }

    const timer = setTimeout(() => {
      finish(
        new Error(
          `ACP initialize timed out after ${INIT_TIMEOUT_MS}ms\n` +
            `stdout:\n${stdout}\nstderr:\n${stderr}`
        )
      )
    }, INIT_TIMEOUT_MS)

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString()
      try {
        assertNoMissingModule("stdout", stdout)
      } catch (err) {
        finish(err)
        return
      }

      // Success: a JSON-RPC response line with id 1 and a result.
      if (stdout.includes('"id":1') && stdout.includes("result")) {
        finish(null, { stdout, stderr })
      }
    })

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString()
      try {
        assertNoMissingModule("stderr", stderr)
      } catch (err) {
        finish(err)
      }
    })

    child.on("error", (err) => {
      finish(new Error(`failed to spawn codex-acp: ${err.message}`))
    })

    child.on("exit", (code, signal) => {
      if (settled) return
      finish(
        new Error(
          `codex-acp exited before initialize response ` +
            `(code=${code}, signal=${signal})\n` +
            `stdout:\n${stdout}\nstderr:\n${stderr}`
        )
      )
    })

    const init = {
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: {
        protocolVersion: 1,
        clientCapabilities: {
          fs: { readTextFile: true, writeTextFile: true },
          terminal: true,
        },
        clientInfo: { name: "smoke-codex-acp", version: "0.0.0" },
      },
    }

    child.stdin.write(`${JSON.stringify(init)}\n`)
  })
}

async function main() {
  const arg = process.argv[2]
  if (!arg) {
    throw new Error("usage: smoke-codex-acp.mjs <codex-acp.exe>")
  }

  const binary = resolve(arg)
  if (!existsSync(binary)) {
    throw new Error(`codex-acp binary not found: ${binary}`)
  }

  const codexPath = resolveHostCodexPath()
  if (!codexPath) {
    throw new Error(
      "Host Codex CLI not found. Install with " +
        "`npm install -g @openai/codex@0.144.1`, or set CODEX_PATH " +
        "to codex.cmd / codex.js. Searched: process.env.CODEX_PATH, " +
        "codex on PATH, " +
        "%APPDATA%\\npm\\codex.cmd, " +
        "%APPDATA%\\npm\\node_modules\\@openai\\codex\\bin\\codex.js"
    )
  }

  const env = {
    ...process.env,
    CODEX_ACP_USE_CLI: "1",
    CODEX_ACP_CLI_MODEL: "gpt-5.5",
    CODEX_PATH: codexPath,
  }

  const version = checkVersion(binary, env)
  const { stdout, stderr } = await initializeOnce(binary, env)

  assertNoMissingModule("initialize stdout", stdout)
  assertNoMissingModule("initialize stderr", stderr)

  process.stdout.write(`${version}\n`)
  process.stdout.write(`initialize ok via CODEX_PATH=${codexPath}\n`)
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  main().catch((err) => {
    console.error(err instanceof Error ? err.message : err)
    process.exit(1)
  })
}
