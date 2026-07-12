#!/usr/bin/env node

import { spawnSync } from "node:child_process"
import { resolve } from "node:path"

const binary = resolve(process.argv[2] ?? "")
if (!process.argv[2]) {
  throw new Error("usage: smoke-codex-acp.mjs <codex-acp.exe>")
}

const run = (args) =>
  spawnSync(binary, args, {
    encoding: "utf8",
    timeout: 20_000,
    env: {
      ...process.env,
      CODEX_ACP_USE_CLI: "1",
      CODEX_ACP_CLI_MODEL: "gpt-5.5",
    },
  })

const adapter = run(["--version"])
if (
  adapter.status !== 0 ||
  adapter.stdout.trim() !==
    "@agentclientprotocol/codex-acp 1.1.2-mycodebuddy.1"
) {
  throw new Error(`adapter version smoke test failed: ${adapter.stderr}`)
}

const codex = run(["cli", "--version"])
if (codex.status !== 0 || !/codex/i.test(codex.stdout + codex.stderr)) {
  throw new Error(
    `embedded Codex CLI smoke test failed (status=${codex.status}): ${codex.stderr}`
  )
}

process.stdout.write(`${adapter.stdout.trim()}\n${codex.stdout.trim()}\n`)
