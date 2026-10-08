import assert from "node:assert/strict"
import { spawnSync } from "node:child_process"
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import { tmpdir } from "node:os"
import { join } from "node:path"
import test from "node:test"

const watchdogPath = new URL(
  "../deploy/codeg-boot/scripts/codeg-watchdog.sh",
  import.meta.url
)

// Source the real top-level shell functions without running the daemon's
// filesystem setup or infinite loop. Only external I/O (curl/start/date) is fake.
function runWatchdog(commands, env = {}, runLoop = false) {
  const root = mkdtempSync(join(tmpdir(), "codeg-watchdog-test-"))
  const source = readFileSync(watchdogPath, "utf8")
  const functions = [...source.matchAll(/^\w+\(\) \{\n[\s\S]*?^\}/gm)]
    .map(([body]) => body)
    .join("\n")
  writeFileSync(join(root, "functions.sh"), functions)
  writeFileSync(join(root, "now"), "2000000000\n")
  writeFileSync(
    join(root, "start-codeg-tunnel.sh"),
    '#!/bin/bash\nprintf "%s %s\\n" "$FORCE_RESTART" "$*" >>"$TEST_ROOT/starts"\nexit "${START_EXIT:-0}"\n',
    { mode: 0o755 }
  )
  const daemonLoop = source.slice(source.indexOf("\nwhile true; do"))
  const harness = `
set -uo pipefail
source "$TEST_ROOT/functions.sh"
BOOT=$TEST_ROOT
HB=$TEST_ROOT
LOG=$TEST_ROOT/watchdog.log
TUNNEL_RESTART_STAMP=$TEST_ROOT/tunnel-restart.stamp
CODEG_PUBLIC_URL=https://example.invalid/
PUBLIC_FAIL_THRESHOLD=2
TUNNEL_RESTART_COOLDOWN=180
TUNNEL_RESTART_BACKOFF_CAP=1800
TUNNEL_RESTART_DAILY_CAP=0
TUNNEL_RESTART_GRACE=60
pub_fail_streak=0
http=ok
scf=up
broken=0
curl() {
  local output=
  printf '%s\\n' "$*" >>"$TEST_ROOT/curls"
  while [ "$#" -gt 0 ]; do
    if [ "$1" = -o ]; then output=$2; shift; fi
    shift
  done
  [ -z "$output" ] || printf '%s' "\${CURL_BODY:-}" >"$output"
  printf '%s' "\${CURL_CODE:-000}"
  return "\${CURL_EXIT:-0}"
}
date() {
  if [ "$*" = +%s ]; then cat "$TEST_ROOT/now"; else command date "$@"; fi
}
sleep() { :; }
ps() { return 0; }
${commands}
${runLoop ? daemonLoop : ""}
`
  try {
    const result = spawnSync("bash", ["-c", harness], {
      env: { ...process.env, ...env, TEST_ROOT: root },
      encoding: "utf8",
      timeout: 5000,
    })
    assert.ifError(result.error)
    return {
      ...result,
      log: readOptional(join(root, "watchdog.log")),
      starts: readOptional(join(root, "starts")),
      stamp: readOptional(join(root, "tunnel-restart.stamp")),
      curls: readOptional(join(root, "curls")),
    }
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
}

function readOptional(path) {
  try {
    return readFileSync(path, "utf8")
  } catch (error) {
    if (error.code !== "ENOENT") throw error
    return ""
  }
}

const probe = `probe_public; result=$?; printf '%s|%s|%s|%s\\n' "$result" "$pub_status" "$pub_code" "$pub_detail"`

test("transport errors count once with a normalized 000 when local UI is healthy", () => {
  for (const code of ["000", "200"]) {
    const result = runWatchdog(probe, { CURL_CODE: code, CURL_EXIT: "28" })
    assert.equal(result.status, 0, result.stderr)
    assert.match(result.stdout, /^1\|fail\|000\|connect_fail local=ok/)
  }
})

test("transport failure with a down local UI does not blame the tunnel", () => {
  const result = runWatchdog(`http=down\n${probe}`, { CURL_EXIT: "7" })
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stdout, /^0\|fail\|000\|connect_fail local=down/)
})

test("successful pages containing 1033 or Cloudflare text do not trigger recovery", () => {
  const result = runWatchdog(probe, {
    CURL_CODE: "200",
    CURL_BODY: "Build 1033: docs about Cloudflare Tunnel error",
  })
  assert.equal(result.stdout, "0|ok|200|\n")
})

test("530 and explicit 1033 error pages are tunnel failures", () => {
  for (const [code, body] of [
    ["530", "unavailable"],
    ["503", "<h1>Cloudflare Tunnel error</h1>"],
    ["502", "Error code: 1033"],
  ]) {
    const result = runWatchdog(probe, { CURL_CODE: code, CURL_BODY: body })
    assert.equal(result.status, 0, result.stderr)
    assert.match(result.stdout, /^1\|bad\|/)
  }
})

test("an unrelated error body containing the number 1033 is not a tunnel error", () => {
  const result = runWatchdog(probe, {
    CURL_CODE: "404",
    CURL_BODY: "Record 1033 does not exist",
  })
  assert.equal(result.stdout, "0|other|404|code=404\n")
})

test("empty or unset public URL reports unconfigured without curl", () => {
  for (const setup of ["CODEG_PUBLIC_URL=", "unset CODEG_PUBLIC_URL"]) {
    const result = runWatchdog(`${setup}\n${probe}`)
    assert.equal(result.status, 0, result.stderr)
    assert.equal(result.stdout, "0|unconfigured|000|\n")
    assert.equal(result.curls, "")
    assert.match(result.log, /CODEG_PUBLIC_URL is not configured/)
  }
})

test("local probes reject incomplete transfers even after successful HTTP headers", () => {
  const result = runWatchdog(
    `http_ok; printf 'ui=%s\\n' "$?"\nwebdav_ok; printf 'webdav=%s\\n' "$?"`,
    { CURL_CODE: "200", CURL_BODY: "x".repeat(1200), CURL_EXIT: "18" }
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "ui=1\nwebdav=1\n")
})

test("complete local UI and authenticated WebDAV responses remain healthy", () => {
  const result = runWatchdog(
    `http_ok; printf 'ui=%s\\n' "$?"\nCURL_CODE=401\nwebdav_ok; printf 'webdav=%s\\n' "$?"`,
    { CURL_CODE: "200", CURL_BODY: "x".repeat(1200) }
  )
  assert.equal(result.stdout, "ui=0\nwebdav=0\n")
})

test("failed tunnel launch propagates its status and retains an attempt cooldown", () => {
  const result = runWatchdog(
    `restart_tunnel test; printf 'restart=%s\\n' "$?"\ntunnel_cooldown_ok; printf 'allowed=%s\\n' "$?"`,
    { START_EXIT: "9" }
  )
  assert.equal(result.stdout, "restart=9\nallowed=1\n")
  assert.match(result.log, /restart.*failed.*9/)
  assert.match(result.stamp, /^2000000000(?: |\n)/)
})

test("repeated attempts back off to a bounded maximum, including failed launches", () => {
  const result = runWatchdog(
    `
for now in 2000000000 2000000180 2000000540 2000001260 2000002700; do
  echo "$now" >"$TEST_ROOT/now"
  restart_tunnel test
done
echo 2000004499 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'before=%s\\n' "$?"
echo 2000004500 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'at=%s\\n' "$?"
restart_tunnel test
echo 2000006299 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'capped_before=%s\\n' "$?"
echo 2000006300 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'capped_at=%s\\n' "$?"
`,
    { START_EXIT: "9" }
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "before=1\nat=0\ncapped_before=1\ncapped_at=0\n")
  assert.equal(result.starts.trim().split("\n").length, 6)
})

test("optional daily cap counts failed attempts and unlocks on the next UTC day", () => {
  const result = runWatchdog(
    `
TUNNEL_RESTART_DAILY_CAP=2
restart_tunnel first
echo 2000000180 >"$TEST_ROOT/now"
restart_tunnel second
echo 2000000540 >"$TEST_ROOT/now"
restart_tunnel third; printf 'capped=%s\\n' "$?"
echo 2000073600 >"$TEST_ROOT/now"
restart_tunnel next_day; printf 'next_day=%s\\n' "$?"
`,
    { START_EXIT: "9" }
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "capped=2\nnext_day=9\n")
  assert.equal(result.starts.trim().split("\n").length, 3)
  assert.match(result.log, /daily cap/)
})

test("disabled daily cap does not stop recovery at an arbitrary attempt count", () => {
  const result = runWatchdog(`
TUNNEL_RESTART_GRACE=0
for now in 2000000000 2000001800 2000003600 2000005400 2000007200 2000009000 2000010800; do
  echo "$now" >"$TEST_ROOT/now"
  restart_tunnel test || exit 1
done
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.starts.trim().split("\n").length, 7)
})

test("legacy timestamp-only state still enforces cooldown after an upgrade", () => {
  const result = runWatchdog(`
echo 1999999999 >"$TUNNEL_RESTART_STAMP"
tunnel_cooldown_ok; printf 'allowed=%s\\n' "$?"
echo 2000000179 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'expired=%s\\n' "$?"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "allowed=1\nexpired=0\n")
})

test("invalid numeric config falls back safely before arithmetic", () => {
  const result = runWatchdog(`
PUBLIC_FAIL_THRESHOLD='-2'
TUNNEL_RESTART_COOLDOWN='1+1'
TUNNEL_RESTART_BACKOFF_CAP=999999999999999999999999999999999
TUNNEL_RESTART_DAILY_CAP=hello
TUNNEL_RESTART_GRACE=-1
configure_watchdog
printf '%s %s %s %s %s\\n' "$PUBLIC_FAIL_THRESHOLD" "$TUNNEL_RESTART_COOLDOWN" "$TUNNEL_RESTART_BACKOFF_CAP" "$TUNNEL_RESTART_DAILY_CAP" "$TUNNEL_RESTART_GRACE"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "2 180 1800 0 60\n")
  assert.match(result.stderr, /invalid PUBLIC_FAIL_THRESHOLD/)
  assert.match(result.stderr, /invalid TUNNEL_RESTART_COOLDOWN/)
  assert.match(result.log, /invalid TUNNEL_RESTART_COOLDOWN/)
})

test("decimal config accepts leading zeroes and explicit disabled cap/grace", () => {
  const result = runWatchdog(`
PUBLIC_FAIL_THRESHOLD=08
TUNNEL_RESTART_COOLDOWN=0008
TUNNEL_RESTART_BACKOFF_CAP=0001
TUNNEL_RESTART_DAILY_CAP=0
TUNNEL_RESTART_GRACE=0
configure_watchdog
printf '%s %s %s %s %s\\n' "$PUBLIC_FAIL_THRESHOLD" "$TUNNEL_RESTART_COOLDOWN" "$TUNNEL_RESTART_BACKOFF_CAP" "$TUNNEL_RESTART_DAILY_CAP" "$TUNNEL_RESTART_GRACE"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "8 8 8 0 0\n")
})

test("public failures must be consecutive before a force restart", () => {
  const result = runWatchdog(`
CURL_CODE=530
check_public_tunnel
CURL_CODE=404
check_public_tunnel
CURL_CODE=530
check_public_tunnel
printf 'streak=%s\\n' "$pub_fail_streak"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "streak=1\n")
  assert.equal(result.starts, "")
})

test("failed force restart keeps the public failure streak", () => {
  const result = runWatchdog(
    `
CURL_CODE=530
check_public_tunnel
check_public_tunnel; printf 'result=%s\\n' "$?"
printf 'streak=%s\\n' "$pub_fail_streak"
`,
    { START_EXIT: "9" }
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=9\nstreak=2\n")
  assert.equal(result.starts, "1 --force\n")
  assert.match(result.log, /restart failed: exit=9/)
})

test("successful force launch preserves failures during a fixed readiness grace", () => {
  const result = runWatchdog(`
CURL_CODE=530
check_public_tunnel
check_public_tunnel
printf 'launched=%s\\n' "$pub_fail_streak"
echo 2000000059 >"$TEST_ROOT/now"
source "$TEST_ROOT/functions.sh"
check_public_tunnel
printf 'grace=%s\\n' "$pub_fail_streak"
echo 2000000060 >"$TEST_ROOT/now"
check_public_tunnel
printf 'expired=%s\\n' "$pub_fail_streak"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "launched=2\ngrace=2\nexpired=3\n")
  assert.equal(result.starts, "1 --force\n")
  assert.match(result.log, /readiness grace/)
})

test("healthy probe resets escalation while preserving cooldown and daily usage", () => {
  const result = runWatchdog(`
TUNNEL_RESTART_DAILY_CAP=2
restart_tunnel first
echo 2000000180 >"$TEST_ROOT/now"
restart_tunnel second
CURL_CODE=200
check_public_tunnel
printf 'streak=%s\\n' "$pub_fail_streak"
tunnel_cooldown_ok; printf 'daily=%s\\n' "$?"
TUNNEL_RESTART_DAILY_CAP=0
tunnel_cooldown_ok; printf 'cooldown=%s\\n' "$?"
echo 2000000360 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'recovered=%s\\n' "$?"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "streak=0\ndaily=1\ncooldown=1\nrecovered=0\n")
  assert.equal(result.starts.trim().split("\n").length, 2)
})

test("unset URL remains opt-in after configuration is initialized", () => {
  const result = runWatchdog(
    `unset CODEG_PUBLIC_URL\nconfigure_watchdog\n${probe}`
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "0|unconfigured|000|\n")
  assert.equal(result.curls, "")
})

test("unrelated error responses do not reset restart backoff", () => {
  const result = runWatchdog(`
TUNNEL_RESTART_GRACE=0
restart_tunnel first
echo 2000000180 >"$TEST_ROOT/now"
restart_tunnel second
CURL_CODE=404
check_public_tunnel
echo 2000000360 >"$TEST_ROOT/now"
tunnel_cooldown_ok; printf 'allowed=%s\\n' "$?"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "allowed=1\n")
  assert.equal(result.stderr, "")
})

test("corrupt persisted numbers never reach Bash arithmetic", () => {
  const result = runWatchdog(`
printf '%s\\n' '999999999999999999999999 1 2 3 4' >"$TUNNEL_RESTART_STAMP"
tunnel_cooldown_ok; printf 'corrupt_timestamp=%s\\n' "$?"
printf '%s\\n' '1999999999 1+2 2 3 4' >"$TUNNEL_RESTART_STAMP"
tunnel_cooldown_ok; printf 'corrupt_counters=%s\\n' "$?"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "corrupt_timestamp=0\ncorrupt_counters=1\n")
  assert.match(result.stderr, /invalid tunnel restart/)
})

test("daemon loop logs failed recovery without clearing consecutive failures", () => {
  const result = runWatchdog(
    `
loop=0
# Guard every service boundary: this test can never inspect or kill host services.
port_up() { return 0; }
http_ok() { return 0; }
webdav_ok() { return 0; }
cf_up() { return 0; }
restart_server() { echo 'unexpected server restart' >&2; exit 90; }
ps() { printf '123\\n'; }
cat >"$BOOT/start-webdav.sh" <<'MOCK'
#!/bin/bash
exit 0
MOCK
chmod +x "$BOOT/start-webdav.sh"
sleep() {
  if [ "$1" = 60 ]; then
    if [ "$loop" -ge 3 ]; then exit 0; fi
    echo "$((2000000000 + loop * 60))" >"$TEST_ROOT/now"
  fi
}
CURL_CODE=530
`,
    { START_EXIT: "9" },
    true
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stderr, "")
  assert.match(result.log, /streak=3\/2/)
  assert.match(result.log, /restart failed: exit=9/)
  assert.match(result.log, /tunnel restart skipped: cooldown\/backoff/)
  assert.match(result.log, /3080=up http=ok cloudflared=up public=bad/)
  assert.equal(result.starts.match(/1 --force/g)?.length, 1)
})

test("a failed state write prevents an unbudgeted force restart", () => {
  const result = runWatchdog(`
TUNNEL_RESTART_STAMP=$TEST_ROOT/missing/stamp
restart_tunnel test; printf 'result=%s\\n' "$?"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=1\n")
  assert.equal(result.starts, "")
  assert.match(result.log, /cannot persist tunnel restart state/)
})

test("readiness grace reports pending without marking the public edge broken", () => {
  const result = runWatchdog(`
restart_tunnel test
CURL_CODE=530
check_public_tunnel
printf 'grace=%s|%s|%s|%s\\n' "$pub" "$pub_status" "$broken" "$pub_fail_streak"
echo 2000000060 >"$TEST_ROOT/now"
check_public_tunnel
printf 'expired=%s|%s|%s|%s\\n' "$pub" "$pub_status" "$broken" "$pub_fail_streak"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "grace=pending|bad|0|0\nexpired=bad|bad|1|1\n")
  assert.match(result.log, /public probe bad code=530.*readiness grace/)
})

// Local config file (default $BOOT/local.env, override CODEG_WATCHDOG_CONFIG).
// The harness presets every key as if exported; drop them to model a launcher
// that passes no environment.
const unsetConfigEnv = `unset CODEG_PUBLIC_URL PUBLIC_FAIL_THRESHOLD TUNNEL_RESTART_COOLDOWN TUNNEL_RESTART_BACKOFF_CAP TUNNEL_RESTART_DAILY_CAP TUNNEL_RESTART_GRACE`
const showConfig = `printf '%s|%s|%s|%s|%s|%s\\n' "$CODEG_PUBLIC_URL" "$PUBLIC_FAIL_THRESHOLD" "$TUNNEL_RESTART_COOLDOWN" "$TUNNEL_RESTART_BACKOFF_CAP" "$TUNNEL_RESTART_DAILY_CAP" "$TUNNEL_RESTART_GRACE"`

test("local config file supplies the public URL and limits without env", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
cat >"$BOOT/local.env" <<'CFG'
# comment line

export CODEG_PUBLIC_URL="https://edge.example.test/"   # quoted + comment
PUBLIC_FAIL_THRESHOLD=3 # inline comment
  TUNNEL_RESTART_COOLDOWN='240'
TUNNEL_RESTART_GRACE=30\r
CODEG_TOKEN=must-be-ignored
CFG
configure_watchdog
${showConfig}
printf 'token=%s\\n' "\${CODEG_TOKEN:-unset}"
CURL_CODE=200
${probe}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(
    result.stdout,
    "https://edge.example.test/|3|240|1800|0|30\ntoken=unset\n0|ok|200|\n"
  )
  assert.match(result.curls, /https:\/\/edge\.example\.test\//)
  assert.equal(result.stderr, "")
})

test("config parser never executes file content and rejects bad values", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
cat >"$BOOT/local.env" <<'CFG'
touch "$TEST_ROOT/pwned-line"
CODEG_PUBLIC_URL=$(touch "$TEST_ROOT/pwned-url")
PUBLIC_FAIL_THRESHOLD=\`touch "$TEST_ROOT/pwned-int"\`
TUNNEL_RESTART_COOLDOWN=1+1
CFG
configure_watchdog
${showConfig}
[ -e "$TEST_ROOT/pwned-line" ] || [ -e "$TEST_ROOT/pwned-url" ] || [ -e "$TEST_ROOT/pwned-int" ] && echo EXECUTED
${probe}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "|2|180|1800|0|60\n0|unconfigured|000|\n")
  assert.match(result.log, /local\.env:1: ignoring malformed line/)
  assert.match(result.log, /invalid CODEG_PUBLIC_URL/)
  assert.match(result.log, /invalid PUBLIC_FAIL_THRESHOLD/)
  assert.equal(result.curls, "")
})

test("non-empty launcher env overrides the config file; empty env does not", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
printf '%s\\n' CODEG_PUBLIC_URL=https://file.example.test/ PUBLIC_FAIL_THRESHOLD=5 >"$BOOT/local.env"
CODEG_PUBLIC_URL=https://env.example.test/
PUBLIC_FAIL_THRESHOLD=
configure_watchdog
${showConfig}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "https://env.example.test/|5|180|1800|0|60\n")
})

test("CODEG_WATCHDOG_CONFIG selects an alternate config path", () => {
  const result = runWatchdog(
    `
${unsetConfigEnv}
printf '%s\\n' CODEG_PUBLIC_URL=https://default.example.test/ >"$BOOT/local.env"
printf '%s\\n' CODEG_PUBLIC_URL=https://alt.example.test/ >"$TEST_ROOT/alt.env"
configure_watchdog
${showConfig}
`,
    { CODEG_WATCHDOG_CONFIG: "" }
  )
  assert.equal(result.stdout, "https://default.example.test/|2|180|1800|0|60\n")
  const alt = runWatchdog(`
${unsetConfigEnv}
printf '%s\\n' CODEG_PUBLIC_URL=https://default.example.test/ >"$BOOT/local.env"
printf '%s\\n' CODEG_PUBLIC_URL=https://alt.example.test/ >"$TEST_ROOT/alt.env"
CODEG_WATCHDOG_CONFIG=$TEST_ROOT/alt.env
configure_watchdog
${showConfig}
`)
  assert.equal(alt.status, 0, alt.stderr)
  assert.equal(alt.stdout, "https://alt.example.test/|2|180|1800|0|60\n")
})

test("config edits apply on the next loop without a restart", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
configure_watchdog
${showConfig}
reload_watchdog_config
printf 'CODEG_PUBLIC_URL=https://edge.example.test/\\nTUNNEL_RESTART_GRACE=5\\n' >"$BOOT/local.env"
reload_watchdog_config
${showConfig}
reload_watchdog_config
printf '# disabled\\n' >"$BOOT/local.env"
reload_watchdog_config
${showConfig}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(
    result.stdout,
    "|2|180|1800|0|60\nhttps://edge.example.test/|2|180|1800|0|5\n|2|180|1800|0|60\n"
  )
  assert.equal(result.log.match(/config reloaded/g)?.length, 2)
})

test("unconfigured public probe warns at most hourly", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
configure_watchdog
probe_public
echo 2000003599 >"$TEST_ROOT/now"
probe_public
echo 2000003600 >"$TEST_ROOT/now"
probe_public
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.log.match(/CODEG_PUBLIC_URL is not configured/g)?.length, 2)
})

function daemonLoopSetup(extra = "") {
  return `
loop=0
port_up() { return 0; }
http_ok() { return 0; }
webdav_ok() { return 0; }
cf_up() { return 0; }
restart_server() { echo 'unexpected server restart' >&2; exit 90; }
ps() { printf '123\\n'; }
cat >"$BOOT/start-webdav.sh" <<'MOCK'
#!/bin/bash
exit 0
MOCK
chmod +x "$BOOT/start-webdav.sh"
sleep_hook() { :; }
sleep() {
  if [ "$1" = 60 ]; then
    sleep_hook
    if [ "$loop" -ge 2 ]; then exit 0; fi
    echo "$((2000000000 + loop * 60))" >"$TEST_ROOT/now"
  fi
}
${unsetConfigEnv}
configure_watchdog
${extra}
`
}

test("daemon loop reports public=unconfigured, then probes once config appears", () => {
  const result = runWatchdog(
    daemonLoopSetup(`
CURL_CODE=530
# Create the config after the first loop's status line, with no restart.
sleep_hook() {
  if [ "$loop" -eq 1 ]; then
    printf 'CODEG_PUBLIC_URL=https://edge.example.test/\\n' >"$BOOT/local.env"
  fi
}
`),
    {},
    true
  )
  assert.equal(result.status, 0, result.stderr)
  const status = result.log.split("\n").filter((l) => /3080=up/.test(l))
  assert.equal(status.length, 2)
  assert.match(status[0], /public=unconfigured/)
  assert.match(status[1], /public=bad pubcode=530/)
  assert.match(result.log, /config reloaded .*public_url=https:\/\/edge\.example\.test\//)
  // Routine start-if-missing only; one 530 is below the threshold of 2.
  assert.doesNotMatch(result.starts, /--force/)
})

// Resume/wake detection. The VM is paused when idle; a pause shows up only as
// a sleep that took far longer on the wall clock than requested. The fake
// sleep advances the fake clock to model that.
const wakeSetup = (sleptSeconds) => `
configure_watchdog
TUNNEL_RESTART_GRACE=0
sleep() {
  echo "$*" >>"$TEST_ROOT/sleeps"
  if [ "$1" = 60 ] || [ "$1" = 15 ]; then
    echo "$(( $(cat "$TEST_ROOT/now") + ${sleptSeconds} ))" >"$TEST_ROOT/now"
  fi
}
watchdog_sleep
`
const wakeReport = `printf 'result=%s streak=%s used=%s pub=%s\\n' "$result" "$pub_fail_streak" "\${wake_restart_used:-unset}" "$pub"`

test("a resume gap probes immediately and restarts cloudflared on the first 1033", () => {
  const result = runWatchdog(`
${wakeSetup(400)}
CURL_CODE=530
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=0 streak=1 used=1 pub=bad\n")
  assert.equal(result.starts, "1 --force\n")
  assert.match(result.log, /resume detected gap=340s \(slept 400s, expected 60s\)/)
  assert.match(result.log, /resume: public probe bad code=530 mention=none; re-probing in 10s/)
  assert.match(result.log, /restart cloudflared ONLY \(wake: cooldown bypassed\): resume: public code=530/)
  assert.equal(result.curls.trim().split("\n").length, 2)
})

test("a normal loop interval is not a wake and keeps the 2-failure threshold", () => {
  const result = runWatchdog(`
${wakeSetup(100)}
CURL_CODE=530
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=0 streak=1 used=unset pub=bad\n")
  assert.equal(result.starts, "")
  assert.doesNotMatch(result.log, /resume/)
})

test("a resume with a healthy edge does not restart and switches to fast checks", () => {
  const result = runWatchdog(`
${wakeSetup(400)}
CURL_CODE=200
check_public_tunnel; result=$?
${wakeReport}
watchdog_sleep
printf 'sleeps=%s\n' "$(tr '\n' ' ' <"$TEST_ROOT/sleeps")"
`)
  assert.equal(result.status, 0, result.stderr)
  // First sleep is the normal 60s; inside the watch window the next is 15s.
  assert.equal(result.stdout, "result=0 streak=0 used=0 pub=ok\nsleeps=60 15 \n")
  assert.equal(result.starts, "")
  assert.match(result.log, /resume: public probe ok code=200; no restart \(watching 300s\)/)
})

test("a re-probe that recovers (self-reconnect) avoids a restart", () => {
  const result = runWatchdog(`
${wakeSetup(400)}
CURL_CODE=530
sleep() { echo "$*" >>"$TEST_ROOT/sleeps"; CURL_CODE=200; }
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=0 streak=0 used=0 pub=ok\n")
  assert.equal(result.starts, "")
  assert.match(result.log, /resume: re-probe ok code=200; no restart/)
})

test("a late zombie inside the watch window still gets the single fast restart", () => {
  const result = runWatchdog(`
${wakeSetup(400)}
CURL_CODE=200
check_public_tunnel
echo $(( $(cat "$TEST_ROOT/now") + 120 )) >"$TEST_ROOT/now"
CURL_CODE=530
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "result=0 streak=1 used=1 pub=bad\n")
  assert.equal(result.starts, "1 --force\n")
})

test("wake bypasses cooldown once, never the daily cap or readiness grace", () => {
  const capped = runWatchdog(`
TUNNEL_RESTART_DAILY_CAP=1
printf '%s\\n' "1999999990 180 $((2000000000 / 86400)) 1 0" >"$TUNNEL_RESTART_STAMP"
${wakeSetup(400)}
TUNNEL_RESTART_DAILY_CAP=1
CURL_CODE=530
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(capped.stdout, "result=2 streak=1 used=1 pub=bad\n")
  assert.equal(capped.starts, "")
  assert.match(capped.log, /tunnel restart skipped: daily cap 1\/1/)

  const cooldown = runWatchdog(`
printf '%s\\n' "2000000300 180 $((2000000000 / 86400)) 1 0" >"$TUNNEL_RESTART_STAMP"
${wakeSetup(400)}
CURL_CODE=530
check_public_tunnel
${wakeReport.replace("$result", "0")}
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(cooldown.status, 0, cooldown.stderr)
  // Recent attempt (100s ago) is bypassed once; the next failure is normal.
  assert.equal(cooldown.starts, "1 --force\n")
  assert.equal(cooldown.stdout, "result=0 streak=1 used=1 pub=bad\nresult=2 streak=2 used=1 pub=bad\n")
  assert.match(cooldown.log, /tunnel restart skipped: cooldown\/backoff/)

  const grace = runWatchdog(`
${wakeSetup(400)}
printf '%s\\n' "2000000300 180 $((2000000000 / 86400)) 1 2000000430" >"$TUNNEL_RESTART_STAMP"
CURL_CODE=530
check_public_tunnel; result=$?
${wakeReport}
`)
  assert.equal(grace.stdout, "result=0 streak=0 used=0 pub=pending\n")
  assert.equal(grace.starts, "")
})

test("wake settings come from the local config whitelist with range checks", () => {
  const result = runWatchdog(`
${unsetConfigEnv}
printf '%s\\n' WAKE_GAP_THRESHOLD=600 WAKE_REPROBE_DELAY=0 WAKE_WATCH_SECS=999999 WAKE_FAST_SLEEP=20 >"$BOOT/local.env"
configure_watchdog
printf '%s %s %s %s\\n' "$WAKE_GAP_THRESHOLD" "$WAKE_REPROBE_DELAY" "$WAKE_WATCH_SECS" "$WAKE_FAST_SLEEP"
`)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stdout, "600 0 300 20\n")
  assert.match(result.log, /invalid WAKE_WATCH_SECS/)
})

test("daemon loop: a pause during sleep triggers one immediate restart and fast re-checks", () => {
  const result = runWatchdog(
    `
loop=0
port_up() { return 0; }
http_ok() { return 0; }
webdav_ok() { return 0; }
cf_up() { return 0; }
restart_server() { echo 'unexpected server restart' >&2; exit 90; }
ps() { printf '123\\n'; }
cat >"$BOOT/start-webdav.sh" <<'MOCK'
#!/bin/bash
exit 0
MOCK
chmod +x "$BOOT/start-webdav.sh"
TUNNEL_RESTART_GRACE=0
sleep() {
  echo "$*" >>"$TEST_ROOT/sleeps"
  case $1 in
    60|15)
      if [ "$loop" -ge 3 ]; then
        printf 'sleeps=%s\\n' "$(tr '\\n' ' ' <"$TEST_ROOT/sleeps")"
        exit 0
      fi
      # Loop 1 sleeps through a 20-minute VM pause; later sleeps are normal.
      if [ "$loop" -eq 1 ]; then step=1200; else step=$1; fi
      echo "$(( $(cat "$TEST_ROOT/now") + step ))" >"$TEST_ROOT/now"
      ;;
  esac
}
CURL_CODE=530
`,
    {},
    true
  )
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stderr, "")
  assert.match(result.log, /resume detected gap=1140s/)
  assert.match(result.log, /cooldown bypassed/)
  // Loop 1: 530 x1 (below threshold). Wake after loop 1's sleep: immediate
  // restart. Loop 2 sleeps 15s (watch window). No codeg-server restart.
  assert.equal(result.starts.match(/1 --force/g)?.length, 1)
  assert.equal(result.stdout, "sleeps=60 10 15 15 \n")
})
