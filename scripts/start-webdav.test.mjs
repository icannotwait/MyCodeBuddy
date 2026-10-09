import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

const scriptPath = new URL(
  "../deploy/codeg-boot/scripts/start-webdav.sh",
  import.meta.url
).pathname

// Everything runs in a temp root: fake python/venv/pip/start.sh, a port that
// nothing listens on, and no real WsgiDAV, network, or /workspace paths.
function setup({ cfg = true, pipExit = 0 } = {}) {
  const root = mkdtempSync(join(tmpdir(), "start-webdav-test-"))
  const dav = join(root, "webdav")
  const hb = join(root, "hb")
  mkdirSync(dav, { recursive: true })
  if (cfg) writeFileSync(join(dav, "wsgidav.yaml"), "port: 1\n")
  writeFileSync(
    join(dav, "start.sh"),
    `#!/bin/bash\necho started >>"${root}/starts"\n`
  )
  chmodSync(join(dav, "start.sh"), 0o755)
  const fakePython = join(root, "python3")
  writeFileSync(
    fakePython,
    `#!/bin/bash
echo "$*" >>"${root}/python-calls"
[ "$1" = -m ] && [ "$2" = venv ] || exit 2
mkdir -p "$3/bin"
printf '#!/bin/bash\\nexit 0\\n' >"$3/bin/python"
printf '#!/bin/bash\\necho "$*" >>"${root}/pip-calls"\\n[ "${pipExit}" = 0 ] && touch "$(dirname "$0")/wsgidav" && chmod +x "$(dirname "$0")/wsgidav"\\nexit ${pipExit}\\n' >"$3/bin/pip"
chmod +x "$3/bin/python" "$3/bin/pip"
`
  )
  chmodSync(fakePython, 0o755)
  const run = (extraEnv = {}) => {
    const result = spawnSync("bash", [scriptPath], {
      env: {
        ...process.env,
        CODEG_WEBDAV_ROOT: dav,
        CODEG_HB: hb,
        CODEG_WEBDAV_PORT: "1",
        CODEG_WEBDAV_PYTHON: fakePython,
        ...extraEnv,
      },
      encoding: "utf8",
      timeout: 15000,
    })
    assert.ifError(result.error)
    return result
  }
  const waitFor = (name) => {
    const deadline = Date.now() + 3000
    while (!existsSync(join(root, name)) && Date.now() < deadline) {
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20)
    }
  }
  const read = (name) =>
    existsSync(join(root, name)) ? readFileSync(join(root, name), "utf8") : ""
  return {
    root,
    dav,
    hb,
    run,
    read,
    waitFor,
    cleanup: () => rmSync(root, { recursive: true, force: true }),
  }
}

test("a missing venv is rebuilt from the pinned spec, then WsgiDAV starts", () => {
  const t = setup()
  try {
    const result = t.run()
    assert.equal(result.status, 0, result.stderr)
    assert.match(t.read("python-calls"), /^-m venv .*\/webdav\/venv\n$/)
    assert.match(
      t.read("pip-calls"),
      /install .*WsgiDAV==4\.3\.5 cheroot==11\.1\.2/
    )
    t.waitFor("starts")
    assert.equal(t.read("starts"), "started\n")
    assert.match(
      readFileSync(join(t.dav, "webdav.log"), "utf8"),
      /venv rebuilt/
    )
    // Healthy venv afterwards: the next call (port still down) only restarts.
    t.run()
    const deadline = Date.now() + 3000
    while (t.read("starts") !== "started\nstarted\n" && Date.now() < deadline) {
      Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20)
    }
    assert.equal(t.read("starts"), "started\nstarted\n")
    assert.equal(t.read("python-calls").split("\n").filter(Boolean).length, 1)
  } finally {
    t.cleanup()
  }
})

test("a failed rebuild is logged, cleaned up, and rate-limited", () => {
  const t = setup({ pipExit: 1 })
  try {
    const first = t.run()
    assert.equal(first.status, 1)
    assert.equal(existsSync(join(t.dav, "venv")), false)
    assert.equal(t.read("starts"), "")
    const second = t.run()
    assert.equal(second.status, 1)
    assert.equal(t.read("python-calls").split("\n").filter(Boolean).length, 1)
    const log = readFileSync(join(t.dav, "webdav.log"), "utf8")
    assert.match(log, /venv rebuild failed/)
    assert.match(log, /retry after 3600s/)
    const third = t.run({ CODEG_WEBDAV_BOOTSTRAP_INTERVAL: "0" })
    assert.equal(third.status, 1)
    assert.equal(t.read("python-calls").split("\n").filter(Boolean).length, 2)
  } finally {
    t.cleanup()
  }
})

test("a missing config fails visibly without touching the venv", () => {
  const t = setup({ cfg: false })
  try {
    const result = t.run()
    assert.equal(result.status, 1)
    assert.match(result.stderr, /missing .*wsgidav\.yaml/)
    assert.match(
      readFileSync(join(t.dav, "webdav.log"), "utf8"),
      /missing .*wsgidav\.yaml/
    )
    assert.equal(t.read("python-calls"), "")
  } finally {
    t.cleanup()
  }
})
