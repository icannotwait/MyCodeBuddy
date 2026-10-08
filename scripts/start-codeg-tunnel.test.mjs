import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

const scriptPath = new URL("../deploy/codeg-boot/scripts/start-codeg-tunnel.sh", import.meta.url)
  .pathname

// Only --print-config is exercised: it resolves the config and exits before
// the lock, any ps/kill, or launching cloudflared. No real tunnel is touched.
function printConfig(env, args = []) {
  const clean = { ...process.env }
  delete clean.CF_CONFIG
  delete clean.FORCE_RESTART
  const result = spawnSync("bash", [scriptPath, "--print-config", ...args], {
    env: { ...clean, ...env },
    encoding: "utf8",
    timeout: 5000,
  })
  assert.ifError(result.error)
  return result
}

function withHomes(fn) {
  const root = mkdtempSync(join(tmpdir(), "start-tunnel-test-"))
  const box = join(root, "box")
  const sandbox = join(root, "agent-reach-home")
  mkdirSync(join(box, ".cloudflared"), { recursive: true })
  mkdirSync(sandbox, { recursive: true })
  writeFileSync(join(box, ".cloudflared", "config.yml"), "tunnel: test\n")
  try {
    fn({ root, box, sandbox })
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

test("a sandbox HOME without a config still resolves the box owner's config", () => {
  withHomes(({ box, sandbox }) => {
    const result = printConfig({ HOME: sandbox, CODEG_CF_HOME: box }, ["--force"])
    assert.equal(result.status, 0, result.stderr)
    assert.equal(
      result.stdout,
      `config=${box}/.cloudflared/config.yml state=found run_home=${box} force=1\n`
    )
  })
})

test("the built-in box home is /home/box, independent of HOME", () => {
  const result = printConfig({ HOME: "/tmp/no-such-home", CODEG_CF_HOME: "" })
  assert.match(result.stdout, /^config=\/home\/box\/\.cloudflared\/config\.yml /)
  assert.match(result.stdout, / run_home=\/home\/box /)
})

test("CF_CONFIG overrides every default", () => {
  withHomes(({ root, box, sandbox }) => {
    const custom = join(root, "custom.yml")
    writeFileSync(custom, "tunnel: other\n")
    const result = printConfig({ HOME: sandbox, CODEG_CF_HOME: box, CF_CONFIG: custom })
    assert.equal(result.status, 0, result.stderr)
    assert.equal(result.stdout, `config=${custom} state=found run_home=${box} force=0\n`)
  })
})

test("falls back to $HOME only when the box config is absent; missing is reported", () => {
  withHomes(({ root, sandbox }) => {
    const emptyBox = join(root, "empty-box")
    mkdirSync(emptyBox)
    const missing = printConfig({ HOME: sandbox, CODEG_CF_HOME: emptyBox })
    assert.equal(missing.status, 1)
    assert.match(missing.stdout, /config=.*empty-box\/\.cloudflared\/config\.yml state=missing/)

    mkdirSync(join(sandbox, ".cloudflared"))
    writeFileSync(join(sandbox, ".cloudflared", "config.yml"), "tunnel: x\n")
    const fallback = printConfig({ HOME: sandbox, CODEG_CF_HOME: emptyBox })
    assert.equal(fallback.status, 0, fallback.stderr)
    assert.equal(
      fallback.stdout,
      `config=${sandbox}/.cloudflared/config.yml state=found run_home=${sandbox} force=0\n`
    )
  })
})

test("launchers pin HOME/USER to the box owner before starting services", () => {
  for (const name of ["reload-watchdog-once.sh", "codeg-supervisor.sh", "codeg-watchdog.sh"]) {
    const source = readFileSync(
      new URL(`../deploy/codeg-boot/scripts/${name}`, import.meta.url),
      "utf8"
    )
    const pin = source.indexOf("export HOME=/home/box USER=box LOGNAME=box\n")
    assert.ok(pin > 0, `${name} must pin HOME`)
    const firstLaunch = source.search(/nohup |\n\s*"\$BOOT\/|\nexec 9>/)
    assert.ok(firstLaunch === -1 || pin < firstLaunch, `${name} pins HOME before launching`)
  }
})
