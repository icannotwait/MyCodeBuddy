import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import {
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
  "../deploy/codeg-boot/scripts/start-codeg-tunnel.sh",
  import.meta.url
).pathname

// --print-config resolves the config and exits before the lock, any ps/kill,
// or launching cloudflared. resolve_tunnel_edge_args is sourced with fake
// getent/curl so DoH --edge can be tested without a real tunnel.
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
    const result = printConfig({ HOME: sandbox, CODEG_CF_HOME: box }, [
      "--force",
    ])
    assert.equal(result.status, 0, result.stderr)
    assert.equal(
      result.stdout,
      `config=${box}/.cloudflared/config.yml state=found run_home=${box} force=1\n`
    )
  })
})

test("the built-in box home is /home/box, independent of HOME", () => {
  const result = printConfig({ HOME: "/tmp/no-such-home", CODEG_CF_HOME: "" })
  assert.match(
    result.stdout,
    /^config=\/home\/box\/\.cloudflared\/config\.yml /
  )
  assert.match(result.stdout, / run_home=\/home\/box /)
})

test("CF_CONFIG overrides every default", () => {
  withHomes(({ root, box, sandbox }) => {
    const custom = join(root, "custom.yml")
    writeFileSync(custom, "tunnel: other\n")
    const result = printConfig({
      HOME: sandbox,
      CODEG_CF_HOME: box,
      CF_CONFIG: custom,
    })
    assert.equal(result.status, 0, result.stderr)
    assert.equal(
      result.stdout,
      `config=${custom} state=found run_home=${box} force=0\n`
    )
  })
})

test("falls back to $HOME only when the box config is absent; missing is reported", () => {
  withHomes(({ root, sandbox }) => {
    const emptyBox = join(root, "empty-box")
    mkdirSync(emptyBox)
    const missing = printConfig({ HOME: sandbox, CODEG_CF_HOME: emptyBox })
    assert.equal(missing.status, 1)
    assert.match(
      missing.stdout,
      /config=.*empty-box\/\.cloudflared\/config\.yml state=missing/
    )

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

// Source resolve_tunnel_edge_args without locking, killing, or launching.
function runEdgeResolve({
  env = {},
  getentIp = "",
  getentExit = 0,
  getentHostsIp = "2606:4700:a0::1",
  doh = {},
  curlFail = false,
} = {}) {
  const root = mkdtempSync(join(tmpdir(), "start-tunnel-edge-"))
  const log = join(root, "cloudflared.log")
  writeFileSync(log, "")
  const source = readFileSync(scriptPath, "utf8")
  const match = source.match(/^resolve_tunnel_edge_args\(\) \{\n[\s\S]*?^\}/m)
  assert.ok(match, "resolve_tunnel_edge_args must exist for DoH --edge")
  const region1 = doh["region1.v2.argotunnel.com"] || []
  const region2 = doh["region2.v2.argotunnel.com"] || []
  const jsonFor = (ips) =>
    JSON.stringify({
      Answer: ips.map((ip) => ({ type: 1, data: ip })),
    })
  const inherited = { ...process.env }
  delete inherited.CODEG_TUNNEL_EDGE_DOH
  delete inherited.CF_CONFIG
  delete inherited.FORCE_RESTART
  const harness = `
set -euo pipefail
${match[0]}
LOG=${JSON.stringify(log)}
timeout() { shift; "$@"; }
getent() {
  printf '%s\\n' "$*" >>${JSON.stringify(join(root, "getent"))}
  if [ "\${1:-}" = hosts ]; then
    printf '%s %s\\n' ${JSON.stringify(getentHostsIp)} "\${2:-}"
    return 0
  fi
  if [ "\${1:-}" = ahostsv4 ] && [ "\${2:-}" = region1.v2.argotunnel.com ]; then
    if [ ${getentExit} -ne 0 ]; then
      return ${getentExit}
    fi
    if [ -n ${JSON.stringify(getentIp)} ]; then
      printf '%s STREAM region1.v2.argotunnel.com\\n' ${JSON.stringify(getentIp)}
    fi
    return 0
  fi
  return 2
}
curl() {
  printf '%s\\n' "$*" >>${JSON.stringify(join(root, "curl"))}
  if [ ${curlFail ? 1 : 0} -eq 1 ]; then
    return 1
  fi
  local url=\${*: -1}
  local name=\${url#*name=}
  name=\${name%%&*}
  case "$name" in
    region1.v2.argotunnel.com) printf '%s' ${JSON.stringify(jsonFor(region1))} ;;
    region2.v2.argotunnel.com) printf '%s' ${JSON.stringify(jsonFor(region2))} ;;
  esac
}
resolve_tunnel_edge_args
printf '%s\\n' "$EDGE_ARGS"
`
  try {
    const result = spawnSync("bash", ["-c", harness], {
      env: { ...inherited, ...env },
      encoding: "utf8",
      timeout: 5000,
    })
    assert.ifError(result.error)
    return {
      ...result,
      log: readFileSync(log, "utf8"),
      curls: (() => {
        try {
          return readFileSync(join(root, "curl"), "utf8")
        } catch {
          return ""
        }
      })(),
      getents: (() => {
        try {
          return readFileSync(join(root, "getent"), "utf8")
        } catch {
          return ""
        }
      })(),
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

test("auto mode uses DoH --edge when system DNS is a 198.18 fake-ip", () => {
  const result = runEdgeResolve({
    getentIp: "198.18.0.1",
    doh: {
      "region1.v2.argotunnel.com": ["198.41.192.7", "198.41.192.77"],
      "region2.v2.argotunnel.com": ["198.41.200.13", "198.41.200.33"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(
    result.stdout.trim(),
    "--edge 198.41.192.7:7844 --edge 198.41.192.77:7844 --edge 198.41.200.13:7844 --edge 198.41.200.33:7844"
  )
  assert.match(result.log, /fake-ip DNS \(198\.18\.0\.1\); using DoH edge:/)
  assert.match(result.log, /--edge 198\.41\.192\.7:7844/)
  assert.match(result.getents, /ahostsv4 region1\.v2\.argotunnel\.com/)
  assert.doesNotMatch(result.getents, /(^|\n)hosts /)
  assert.match(result.curls, /--resolve cloudflare-dns\.com:443:1\.1\.1\.1/)
  assert.match(
    result.curls,
    /cloudflare-dns\.com\/dns-query\?name=region1\.v2\.argotunnel\.com&type=A/
  )
  assert.match(
    result.curls,
    /cloudflare-dns\.com\/dns-query\?name=region2\.v2\.argotunnel\.com&type=A/
  )
})

test("auto mode uses DoH when ahostsv4 is 198.19.x even if hosts is IPv6", () => {
  const result = runEdgeResolve({
    getentIp: "198.19.0.1",
    getentHostsIp: "2606:4700:a0::1",
    doh: {
      "region1.v2.argotunnel.com": ["198.41.192.7"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout.trim(), "--edge 198.41.192.7:7844")
  assert.match(result.getents, /ahostsv4 /)
})

test("auto mode uses DoH --edge when system DNS is empty", () => {
  const result = runEdgeResolve({
    getentIp: "",
    getentExit: 2,
    doh: {
      "region1.v2.argotunnel.com": ["198.41.192.7"],
      "region2.v2.argotunnel.com": ["198.41.200.13"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(
    result.stdout.trim(),
    "--edge 198.41.192.7:7844 --edge 198.41.200.13:7844"
  )
})

test("auto mode skips DoH when system DNS is a real edge address", () => {
  const result = runEdgeResolve({
    getentIp: "198.41.192.7",
    doh: {
      "region1.v2.argotunnel.com": ["1.2.3.4"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout.trim(), "")
  assert.equal(result.curls, "")
  assert.equal(result.log, "")
})

test("CODEG_TUNNEL_EDGE_DOH=on forces DoH even when system DNS looks real", () => {
  const result = runEdgeResolve({
    env: { CODEG_TUNNEL_EDGE_DOH: "on" },
    getentIp: "198.41.192.7",
    doh: {
      "region1.v2.argotunnel.com": ["198.41.200.13"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout.trim(), "--edge 198.41.200.13:7844")
})

test("CODEG_TUNNEL_EDGE_DOH=off never queries DoH", () => {
  const result = runEdgeResolve({
    env: { CODEG_TUNNEL_EDGE_DOH: "off" },
    getentIp: "198.18.0.1",
    doh: {
      "region1.v2.argotunnel.com": ["198.41.192.7"],
    },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout.trim(), "")
  assert.equal(result.curls, "")
  assert.equal(result.log, "")
})

test("empty DoH results fall back to launching without --edge", () => {
  const result = runEdgeResolve({
    getentIp: "198.18.0.1",
    doh: {},
    curlFail: true,
  })
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout.trim(), "")
  assert.match(result.log, /DoH returned no edges, launching without --edge/)
})

test("launch keeps http2, lock-fd close, and optional --edge", () => {
  const source = readFileSync(scriptPath, "utf8")
  assert.match(
    source,
    /nohup "\$CF" tunnel --config "\$CFG" --protocol http2 \$EDGE_ARGS run 9>&-/
  )
  assert.match(
    source,
    /CODEG_TUNNEL_EDGE_DOH:-\s*auto|CODEG_TUNNEL_EDGE_DOH:-auto/
  )
})

test("launchers pin HOME/USER to the box owner before starting services", () => {
  for (const name of [
    "reload-watchdog-once.sh",
    "codeg-supervisor.sh",
    "codeg-watchdog.sh",
  ]) {
    const source = readFileSync(
      new URL(`../deploy/codeg-boot/scripts/${name}`, import.meta.url),
      "utf8"
    )
    const pin = source.indexOf("export HOME=/home/box USER=box LOGNAME=box\n")
    assert.ok(pin > 0, `${name} must pin HOME`)
    const firstLaunch = source.search(/nohup |\n\s*"\$BOOT\/|\nexec 9>/)
    assert.ok(
      firstLaunch === -1 || pin < firstLaunch,
      `${name} pins HOME before launching`
    )
  }
})
