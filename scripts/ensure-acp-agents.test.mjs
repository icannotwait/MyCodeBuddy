import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { readFileSync } from "node:fs"
import test from "node:test"

const scriptPath = new URL(
  "../deploy/codeg-boot/scripts/ensure-acp-agents.sh",
  import.meta.url
)

// Run only the real registry_plan() function on a fixture acp_list_agents
// payload. The rest of the script (hardcoded /workspace paths, live server,
// downloads) is never executed.
function plan(agents, env = {}) {
  const source = readFileSync(scriptPath, "utf8")
  const fn = source.match(/^registry_plan\(\) \{\n[\s\S]*?^\}/m)
  assert.ok(fn, "registry_plan() not found")
  const cleanEnv = { ...process.env }
  for (const key of [
    "CODEG_ANTIGRAVITY_VER",
    "CODEG_CURSOR_VER",
    "CODEG_GROK_VER",
  ]) {
    delete cleanEnv[key]
  }
  const result = spawnSync(
    "bash",
    ["-c", `set -uo pipefail\n${fn[0]}\nregistry_plan`],
    {
      input: JSON.stringify(agents),
      env: { ...cleanEnv, ...env },
      encoding: "utf8",
      timeout: 5000,
    }
  )
  assert.ifError(result.error)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stderr, "")
  return result.stdout.trim().split("\n").filter(Boolean)
}

const agent = (
  agent_type,
  installed_version,
  registry_version,
  extra = {}
) => ({
  agent_type,
  distribution_type: agent_type === "grok" ? "npx" : "binary",
  enabled: true,
  installed_version,
  registry_version,
  ...extra,
})

test("an early upgrade above the registry pin is kept, never refreshed down", () => {
  assert.deepEqual(plan([agent("antigravity", "1.3.0", "1.2.1")]), [
    "keep binary antigravity 1.3.0 1.2.1 newer",
  ])
})

test("matching versions are kept; older or missing installs move to the registry minimum", () => {
  assert.deepEqual(
    plan([
      agent("antigravity", "1.2.1", "1.2.1"),
      agent("cursor", "2026.09.18-9a7762b", "2026.09.28-64d2043"),
      agent("grok", null, "1.0.46"),
    ]),
    [
      "keep binary antigravity 1.2.1 1.2.1 equal",
      "plan binary cursor 2026.09.28-64d2043 2026.09.18-9a7762b",
      "plan npx grok 1.0.46 -",
    ]
  )
})

test("comparison is numeric SemVer precedence, not string order", () => {
  const rows = (installed, registry) =>
    plan([agent("antigravity", installed, registry)])[0].split(" ")[0]
  assert.equal(rows("1.10.0", "1.9.9"), "keep")
  assert.equal(rows("1.9.9", "1.10.0"), "plan")
  assert.equal(rows("2026.09.28-64d2043", "2026.09.18-9a7762b"), "keep")
  assert.equal(rows("v1.3.0", "1.2.1"), "keep")
  assert.equal(rows("1.3", "1.3.0"), "keep")
  assert.equal(rows("1.3.0+build.7", "1.3.0"), "keep")
  // Prerelease sorts below its release; numeric identifiers compare numerically.
  assert.equal(rows("1.3.0-rc.1", "1.3.0"), "plan")
  assert.equal(rows("1.3.0", "1.3.0-rc.1"), "keep")
  assert.equal(rows("1.3.0-rc.10", "1.3.0-rc.2"), "keep")
  assert.equal(rows("1.3.0-alpha", "1.3.0-alpha.1"), "plan")
  assert.equal(rows("1.3.0-1", "1.3.0-alpha"), "plan")
})

test("an unparseable installed version is still repaired to the registry version", () => {
  assert.deepEqual(plan([agent("antigravity", "unknown-build", "1.2.1")]), [
    "plan binary antigravity 1.2.1 unknown-build",
  ])
})

test("explicit CODEG_<AGENT>_VER is an exact pin and may downgrade", () => {
  const agents = [
    agent("antigravity", "1.3.0", "1.2.1"),
    agent("grok", "1.0.46", "1.0.46"),
  ]
  assert.deepEqual(plan(agents, { CODEG_ANTIGRAVITY_VER: "1.2.1" }), [
    "plan binary antigravity 1.2.1 1.3.0",
    "keep npx grok 1.0.46 1.0.46 equal",
  ])
  assert.deepEqual(plan(agents, { CODEG_ANTIGRAVITY_VER: "1.3.0" }), [
    "keep npx grok 1.0.46 1.0.46 equal",
  ])
})

test("disabled, unmanaged, uvx, and registry-less agents are ignored", () => {
  assert.deepEqual(
    plan([
      agent("antigravity", null, "1.2.1", { enabled: false }),
      agent("claude_code", null, "2.1.1"),
      agent("cursor", null, "2026.09.28-64d2043", { distribution_type: "uvx" }),
      agent("grok", null, null),
    ]),
    []
  )
})
