#!/bin/bash
# Simulated tests for codeg-watchdog.sh wake detection and 1033 recovery.
# Globals are consumed by the sourced watchdog; status checks read $? on purpose.
# shellcheck disable=SC2034,SC1007,SC2319
# Runs offline: sources the watchdog with stubs and a file-backed fake clock;
# never touches real services. Usage: bash tests/test-watchdog-wake.sh
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
WATCHDOG=$HERE/../scripts/codeg-watchdog.sh
fails=0

setup() {
  T=$(mktemp -d)
  mkdir -p "$T/boot" "$T/hb"
  echo 1800000000 >"$T/clock"
  echo ok >"$T/pub"
  : >"$T/restarts"
  # Stub boot scripts. start-webdav.sh may simulate a VM freeze mid-loop.
  cat >"$T/boot/start-codeg-tunnel.sh" <<STUB
#!/bin/bash
[ "\${1:-}" = --force ] && echo force >>"$T/restarts"
exit 0
STUB
  cat >"$T/boot/start-webdav.sh" <<STUB
#!/bin/bash
if [ -f "$T/freeze" ]; then
  echo \$((\$(cat "$T/clock") + \$(cat "$T/freeze"))) >"$T/clock"
  rm -f "$T/freeze"
  echo zombie >"$T/pub"
fi
exit 0
STUB
  printf '#!/bin/bash\n[ "${1:-}" = --state ] && echo disabled\nexit 0\n' >"$T/boot/start-tailscale.sh"
  chmod +x "$T/boot/"*.sh
  : >"$T/local.env"
  export CODEG_WATCHDOG_CONFIG=$T/local.env CODEG_PUBLIC_URL=https://example.invalid/
  # shellcheck source=../scripts/codeg-watchdog.sh
  CODEG_WATCHDOG_SOURCE_ONLY=1 source "$WATCHDOG"
  BOOT=$T/boot HB=$T/hb LOG=$T/hb/watchdog.log TUNNEL_RESTART_STAMP=$T/hb/tunnel-restart.stamp
  date() {
    if [ "${1:-}" = +%s ]; then cat "$T/clock"; return; fi
    local a=() x
    for x in "$@"; do [ "$x" = -u ] || a+=("$x"); done
    command date -u -d "@$(cat "$T/clock")" "${a[@]}"
  }
  sleep() { echo $(($(cat "$T/clock") + ${1%.*})) >"$T/clock"; }
  port_up() { return 0; }
  http_ok() { return 0; }
  webdav_ok() { return 0; }
  cf_up() { return 0; }
  probe_public() {
    pub_code=200 pub_detail= pub_mention= pub_status=ok
    case $(cat "$T/pub") in
      zombie) pub_code=530 pub_mention=1033 pub_status=bad pub_detail="code=530 mention=1033"; return 1 ;;
      530) pub_code=530 pub_status=bad pub_detail="code=530 mention=none"; return 1 ;;
    esac
    return 0
  }
  configure_watchdog
  loop=0 pub_fail_streak=0 prev_loop_end= last_sleep_interval= wake_noted_at=0
  wake_pending= wake_restart_used=1 wake_watch_until=0
}

check() { # name, condition result
  if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fails=$((fails + 1)); sed 's/^/    /' "$LOG"; fi
}
restarts() { wc -l <"$T/restarts" | tr -d ' '; }

# 1+2: freeze inside an iteration (after the public probe returned 200).
for imm in 0 1; do
  setup; echo "PUBLIC_1033_IMMEDIATE=$imm" >"$T/local.env"; configure_watchdog
  watchdog_iteration; watchdog_sleep
  echo 2700 >"$T/freeze"
  watchdog_iteration
  grep -q 'resume detected gap=.*(loop iteration took' "$LOG"; check "midloop freeze detected (immediate=$imm)" $?
  [ "$(restarts)" = 1 ]; check "midloop freeze restarts cloudflared in same iteration (immediate=$imm)" $?
  ! tail -1 "$LOG" | grep -q 'public=ok'; check "status line not the stale pre-freeze public=ok (immediate=$imm)" $?
  rm -rf "$T"
done

# Regression: freeze during the sleep is still caught by the sleep check and
# the next iteration probes before slow steps (one restart, one wake line).
setup; echo PUBLIC_1033_IMMEDIATE=0 >"$T/local.env"; configure_watchdog
watchdog_iteration
sleep() { echo $(($(cat "$T/clock") + ${1%.*} + 3000)) >"$T/clock"; echo zombie >"$T/pub"; }
watchdog_sleep
sleep() { echo $(($(cat "$T/clock") + ${1%.*})) >"$T/clock"; }
watchdog_iteration
[ "$(grep -c 'resume detected' "$LOG")" = 1 ] && grep -q '(sleep took' "$LOG"; check "sleep freeze detected once" $?
[ "$(restarts)" = 1 ]; check "sleep freeze restarts once" $?
rm -rf "$T"

# No freeze: normal iterations never report a wake.
setup; for _ in 1 2 3; do watchdog_iteration; watchdog_sleep; done
! grep -q 'resume detected' "$LOG"; check "no false wake on normal loops" $?
rm -rf "$T"

# 3: first 530 with 1033, local ok, cloudflared up -> immediate restart.
setup; watchdog_iteration; watchdog_sleep; echo zombie >"$T/pub"; watchdog_iteration
[ "$(restarts)" = 1 ] && grep -q 'on first fail: 1033 zombie' "$LOG"; check "1033 first fail restarts" $?
rm -rf "$T"

# 3: respects cooldown (a restart 30s ago blocks it).
setup; now=$(cat "$T/clock")
echo "$((now - 30)) 180 $((now / 86400)) 1 0" >"$TUNNEL_RESTART_STAMP"
echo zombie >"$T/pub"; watchdog_iteration
[ "$(restarts)" = 0 ] && grep -q 'tunnel restart skipped: cooldown' "$LOG"; check "1033 first fail respects cooldown" $?
rm -rf "$T"

# 3: respects daily cap.
setup; echo TUNNEL_RESTART_DAILY_CAP=2 >"$T/local.env"; configure_watchdog; now=$(cat "$T/clock")
echo "$((now - 9999)) 0 $((now / 86400)) 2 0" >"$TUNNEL_RESTART_STAMP"
echo zombie >"$T/pub"; watchdog_iteration
[ "$(restarts)" = 0 ] && grep -q 'skipped: daily cap' "$LOG"; check "1033 first fail respects daily cap" $?
rm -rf "$T"

# 3: plain 530 without 1033 still waits for PUBLIC_FAIL_THRESHOLD.
setup; echo 530 >"$T/pub"; watchdog_iteration
[ "$(restarts)" = 0 ]; check "530 without 1033 waits for threshold" $?
watchdog_sleep; watchdog_iteration
[ "$(restarts)" = 1 ]; check "530 without 1033 restarts at threshold" $?
rm -rf "$T"

# 3: can be disabled.
setup; echo PUBLIC_1033_IMMEDIATE=0 >"$T/local.env"; configure_watchdog
echo zombie >"$T/pub"; watchdog_iteration
[ "$(restarts)" = 0 ]; check "PUBLIC_1033_IMMEDIATE=0 keeps threshold" $?
rm -rf "$T"

echo "failures=$fails"
[ "$fails" = 0 ]
