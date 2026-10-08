import assert from "node:assert/strict"
import { existsSync, readFileSync } from "node:fs"
import { dirname, resolve } from "node:path"
import test from "node:test"
import { fileURLToPath } from "node:url"
import { parse } from "yaml"

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")

test("CI Cargo config files exist relative to each step's working directory", () => {
  const workflow = parse(
    readFileSync(resolve(repositoryRoot, ".github/workflows/test.yml"), "utf8")
  )
  const missingConfigs = []

  for (const [jobId, job] of Object.entries(workflow.jobs)) {
    for (const step of job.steps ?? []) {
      const workingDirectory =
        step["working-directory"] ??
        job.defaults?.run?.["working-directory"] ??
        workflow.defaults?.run?.["working-directory"] ??
        "."

      for (const line of (step.run ?? "").split("\n")) {
        if (!/\bcargo\b/.test(line)) continue

        for (const match of line.matchAll(
          /--config(?:\s+|=)(?:"([^"]+)"|'([^']+)'|([^\s]+))/g
        )) {
          const config = match[1] ?? match[2] ?? match[3]
          // Cargo also accepts inline TOML key=value overrides.
          if (config.includes("=")) continue

          if (!existsSync(resolve(repositoryRoot, workingDirectory, config))) {
            missingConfigs.push(`${jobId} / ${step.name}: ${config}`)
          }
        }
      }
    }
  }

  assert.deepEqual(
    missingConfigs,
    [],
    "CI references missing Cargo config files"
  )
})
