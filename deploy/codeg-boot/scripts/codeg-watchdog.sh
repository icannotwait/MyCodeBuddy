#!/bin/bash
set -uo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

BOOT=/workspace/codeg-boot
HB=/workspace/heartbeat
PIDFILE=$HB/watchdog.pid
# v2: old watchdog.lock may still be held by a codeg-server that inherited fd 9
LOCK=$HB/watchdog.lock.v2
LOG=$HB/watchdog.log
TUNNEL_RESTART_STAMP=$HB/tunnel-restart.stamp

watchdog_warning() {
  # Launchers may discard stderr, so retain configuration/state warnings too.
  printf 'watchdog: %s\n' "$*" >&2
  printf 'watchdog: %s\n' "$*" >>"$LOG"
}

# Bound and normalize decimal inputs before Bash arithmetic (including 08).
watchdog_uint() {
  [[ "$1" =~ ^[0-9]+$ ]] && [ "${#1}" -le 10 ] && [ "$((10#$1))" -le "$2" ]
}

watchdog_config_uint() {
  local name=$1 fallback=$2 minimum=$3 maximum=$4 value
  value=${!name-$fallback}
  if ! watchdog_uint "$value" "$maximum" || [ "$((10#$value))" -lt "$minimum" ]; then
    watchdog_warning "invalid $name; using $fallback (range $minimum..$maximum)"
    value=$fallback
  fi
  printf -v "$name" '%s' "$((10#$value))"
}

configure_watchdog() {
  CODEG_PUBLIC_URL=${CODEG_PUBLIC_URL:-}
  watchdog_config_uint PUBLIC_FAIL_THRESHOLD 2 1 1000
  watchdog_config_uint TUNNEL_RESTART_COOLDOWN 180 1 86400
  watchdog_config_uint TUNNEL_RESTART_BACKOFF_CAP 1800 1 86400
  watchdog_config_uint TUNNEL_RESTART_DAILY_CAP 0 0 1000
  watchdog_config_uint TUNNEL_RESTART_GRACE 60 0 3600
  if [ "$TUNNEL_RESTART_BACKOFF_CAP" -lt "$TUNNEL_RESTART_COOLDOWN" ]; then
    watchdog_warning "backoff cap below cooldown; using $TUNNEL_RESTART_COOLDOWN"
    TUNNEL_RESTART_BACKOFF_CAP=$TUNNEL_RESTART_COOLDOWN
  fi
}

mkdir -p "$HB"

# Single-instance via flock
exec 9>"$LOCK"
if ! flock -n 9; then
  exit 0
fi

configure_watchdog

echo $$ > "$PIDFILE"
loop=0
pub_fail_streak=0

port_up() {
  local port=$1
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | grep -q ":${port}"
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/$port) >/dev/null 2>&1
}

# Real UI probe: port-up alone misses "bound but static=/out" empty 404.
http_ok() {
  local code len
  code=$(curl -sS -o /tmp/codeg-watchdog-body.$$ -w '%{http_code}' --connect-timeout 2 --max-time 5 http://127.0.0.1:3080/ 2>/dev/null) || code=000
  len=$(wc -c < /tmp/codeg-watchdog-body.$$ 2>/dev/null | tr -d ' ' || echo 0)
  rm -f /tmp/codeg-watchdog-body.$$
  [ "$code" = "200" ] && [ "${len:-0}" -gt 1000 ]
}

# WebDAV: unauthenticated should be 401; proves auth + listener.
webdav_ok() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 http://127.0.0.1:6065/ 2>/dev/null) || code=000
  [ "$code" = "401" ] || [ "$code" = "200" ]
}

# Public edge probe. Sets globals: pub_status (skip|ok|bad|fail|other), pub_code, pub_detail.
# Returns 0 if healthy/skipped, 1 if tunnel looks broken.
probe_public() {
  pub_status=skip
  pub_code=000
  pub_detail=
  if [ -z "${CODEG_PUBLIC_URL:-}" ]; then
    return 0
  fi
  local body=/tmp/codeg-watchdog-public.$$
  pub_code=$(curl -sS -o "$body" -w '%{http_code}' --connect-timeout 5 --max-time 12 \
    -L --max-redirs 2 "$CODEG_PUBLIC_URL" 2>/dev/null) || pub_code=000
  local mention=
  if [[ "$pub_code" =~ ^[45][0-9][0-9]$ ]] && [ -f "$body" ] &&
    grep -qiE 'error([[:space:]]+code)?[^[:alnum:]]*1033([^0-9]|$)|Cloudflare Tunnel error' "$body" 2>/dev/null; then
    mention=1033
  fi
  rm -f "$body"

  # 530 / 1033: classic zombie tunnel (process up, edge broken).
  if [ "$pub_code" = "530" ] || [ -n "$mention" ]; then
    pub_status=bad
    pub_detail="code=$pub_code mention=${mention:-none}"
    return 1
  fi
  # Connect/timeout failure only counts when local UI is healthy (isolates tunnel).
  if [ "$pub_code" = "000" ]; then
    if [ "${http:-down}" = "ok" ]; then
      pub_status=fail
      pub_detail="connect_fail local=ok"
      return 1
    fi
    pub_status=fail
    pub_detail="connect_fail local=${http:-down}"
    return 0
  fi
  if [ "$pub_code" = "200" ]; then
    pub_status=ok
    return 0
  fi
  # Other non-200: log but do not treat as tunnel-zombie (avoid false restarts).
  pub_status=other
  pub_detail="code=$pub_code"
  return 0
}

# State: last_attempt backoff_seconds utc_epoch_day attempts_today grace_until.
# The timestamp-only stamp from older installs still enforces base cooldown.
read_tunnel_state() {
  local now=$1 last delay day attempts grace extra
  tunnel_last_attempt=0
  tunnel_backoff=0
  tunnel_day=$((now / 86400))
  tunnel_attempts=0
  tunnel_grace_until=0
  [ -f "$TUNNEL_RESTART_STAMP" ] || return 0
  read -r last delay day attempts grace extra <"$TUNNEL_RESTART_STAMP" || true
  if watchdog_uint "${last:-}" 9999999999; then
    tunnel_last_attempt=$((10#$last))
  else
    watchdog_warning "ignoring invalid tunnel restart timestamp"
    return 0
  fi
  if [ -z "${delay:-}" ]; then return 0; fi
  if watchdog_uint "$delay" 86400 && watchdog_uint "${day:-}" 9999999999 &&
    watchdog_uint "${attempts:-}" 9999999999 && watchdog_uint "${grace:-}" 9999999999 &&
    [ -z "${extra:-}" ]; then
    tunnel_backoff=$((10#$delay))
    if [ "$((10#$day))" -eq "$tunnel_day" ]; then
      tunnel_attempts=$((10#$attempts))
    fi
    tunnel_grace_until=$((10#$grace))
  else
    watchdog_warning "ignoring invalid tunnel restart counters"
  fi
}

write_tunnel_state() {
  local temporary=$TUNNEL_RESTART_STAMP.$$
  if printf '%s %s %s %s %s\n' "$tunnel_last_attempt" "$tunnel_backoff" \
    "$tunnel_day" "$tunnel_attempts" "$tunnel_grace_until" >"$temporary" &&
    mv -f "$temporary" "$TUNNEL_RESTART_STAMP"; then
    return 0
  fi
  rm -f "$temporary"
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) cannot persist tunnel restart state" >>"$LOG"
  return 1
}

tunnel_cooldown_ok() {
  local now wait remaining
  now=$(date +%s)
  read_tunnel_state "$now"
  tunnel_block_reason=
  if [ "$TUNNEL_RESTART_DAILY_CAP" -gt 0 ] && [ "$tunnel_attempts" -ge "$TUNNEL_RESTART_DAILY_CAP" ]; then
    tunnel_block_reason="daily cap $tunnel_attempts/$TUNNEL_RESTART_DAILY_CAP (UTC)"
    return 1
  fi
  if [ "$now" -lt "$tunnel_grace_until" ]; then
    tunnel_block_reason="readiness grace $((tunnel_grace_until - now))s remaining"
    return 1
  fi
  wait=$tunnel_backoff
  if [ "$wait" -lt "$TUNNEL_RESTART_COOLDOWN" ]; then wait=$TUNNEL_RESTART_COOLDOWN; fi
  remaining=$((tunnel_last_attempt + wait - now))
  if [ "$tunnel_last_attempt" -gt 0 ] && [ "$remaining" -gt 0 ]; then
    tunnel_block_reason="cooldown/backoff ${remaining}s remaining"
    return 1
  fi
  return 0
}

# Restart ONLY cloudflared (never codeg-server). Budget attempts, not successes:
# a missing config or failed start must not turn into a tight retry loop.
restart_tunnel() {
  local reason=$1 status now
  if ! tunnel_cooldown_ok; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) tunnel restart skipped: $tunnel_block_reason" >>"$LOG"
    return 2
  fi
  now=$(date +%s)
  tunnel_last_attempt=$now
  if [ "$tunnel_backoff" -eq 0 ]; then
    tunnel_backoff=$TUNNEL_RESTART_COOLDOWN
  else
    tunnel_backoff=$((tunnel_backoff * 2))
  fi
  if [ "$tunnel_backoff" -gt "$TUNNEL_RESTART_BACKOFF_CAP" ]; then
    tunnel_backoff=$TUNNEL_RESTART_BACKOFF_CAP
  fi
  tunnel_attempts=$((tunnel_attempts + 1))
  tunnel_grace_until=0
  write_tunnel_state || return 1
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) restart cloudflared ONLY: $reason; backoff=${tunnel_backoff}s attempts_today=$tunnel_attempts" >>"$LOG"
  if FORCE_RESTART=1 "$BOOT/start-codeg-tunnel.sh" --force 9>&-; then
    # Launch acceptance is not readiness. Only a healthy probe clears failures.
    tunnel_grace_until=$(($(date +%s) + TUNNEL_RESTART_GRACE))
    write_tunnel_state || return 1
    return 0
  else
    status=$?
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) tunnel restart failed: exit=$status; cooldown retained" >>"$LOG"
    return "$status"
  fi
}

restart_server() {
  local reason=$1
  echo "$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST') restart codeg-server: $reason" >>"$LOG"
  # Prefer the real binary; avoid killing the start script wrapper.
  pids=$(pgrep -x codeg-server 2>/dev/null || true)
  if [ -z "${pids:-}" ]; then
    pids=$(pgrep -f '/workspace/codeg-dist/codeg-server' 2>/dev/null || true)
  fi
  if [ -n "${pids:-}" ]; then
    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true
    sleep 1
    for p in $pids; do
      if kill -0 "$p" 2>/dev/null; then
        kill -9 "$p" 2>/dev/null || true
      fi
    done
    sleep 1
  fi
  nohup "$BOOT/start-codeg-server.sh" >>"$HB/codeg-server.log" 2>&1 9>&- &
  sleep 2
}

cf_up() {
  # Process name only — avoids matching shells that mention cloudflared in argv.
  ps -C cloudflared >/dev/null 2>&1
}

check_public_tunnel() {
  local now restart_status=0
  if probe_public; then
    pub=$pub_status
    # Only adjacent, counted failures qualify as consecutive failures.
    pub_fail_streak=0
    if [ "$pub_status" = ok ]; then
      read_tunnel_state "$(date +%s)"
      if [ "$tunnel_backoff" -gt 0 ] || [ "$tunnel_grace_until" -gt 0 ]; then
        tunnel_backoff=0
        tunnel_grace_until=0
        write_tunnel_state || return 1
      fi
    fi
    return 0
  fi

  pub=$pub_status
  now=$(date +%s)
  read_tunnel_state "$now"
  if [ "$now" -lt "$tunnel_grace_until" ]; then
    pub=pending
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) public probe $pub_status $pub_detail; readiness grace $((tunnel_grace_until - now))s remaining" >>"$LOG"
    return 0
  fi
  broken=$((broken + 1))
  pub_fail_streak=$((pub_fail_streak + 1))
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) public probe FAIL streak=$pub_fail_streak/$PUBLIC_FAIL_THRESHOLD url=$CODEG_PUBLIC_URL $pub_detail cloudflared=$scf http=$http" >>"$LOG"
  if [ "$pub_fail_streak" -ge "$PUBLIC_FAIL_THRESHOLD" ]; then
    restart_tunnel "public $pub_detail after ${pub_fail_streak} consecutive fails (local http=$http)" || restart_status=$?
    # A successful start is not a healthy edge. Preserve the failure streak;
    # subsequent probes observe readiness without triggering another restart.
    if cf_up; then scf=up; else scf=down; broken=$((broken + 1)); fi
  fi
  return "$restart_status"
}

while true; do
  s3080=down
  scf=down
  swebdav=down
  broken=0
  http=down
  pub=skip

  if port_up 3080; then
    s3080=up
    if http_ok; then
      http=ok
    else
      http=bad
      broken=$((broken + 1))
      restart_server "port up but GET / not 200 with body (static dir / deploy race)"
      if port_up 3080 && http_ok; then
        s3080=up
        http=ok
      fi
    fi
  else
    broken=$((broken + 1))
    # Close flock fd so children never inherit the lock
    nohup "$BOOT/start-codeg-server.sh" >>"$HB/codeg-server.log" 2>&1 9>&- &
    sleep 2
    if port_up 3080; then
      s3080=up
      if http_ok; then http=ok; else http=bad; broken=$((broken + 1)); fi
    fi
  fi

  # Always call start script: it flock-serializes and collapses duplicate connectors.
  "$BOOT/start-codeg-tunnel.sh" 9>&- || true
  if cf_up; then
    scf=up
    ncf=$(ps -C cloudflared -o pid= 2>/dev/null | awk 'NF' | wc -l | tr -d ' ')
    if [ "${ncf:-0}" -gt 1 ]; then
      broken=$((broken + 1))
    fi
  else
    broken=$((broken + 1))
    sleep 1
    if cf_up; then scf=up; fi
  fi

  # Public edge recovery never restarts the local server.
  check_public_tunnel

  # WebDAV (WsgiDAV on :6065) — was missing from watchdog; dies after reboot/update.
  if [ -x "$BOOT/start-webdav.sh" ]; then
    "$BOOT/start-webdav.sh" 9>&- || true
    if port_up 6065 && webdav_ok; then
      swebdav=up
    else
      broken=$((broken + 1))
      sleep 1
      if port_up 6065 && webdav_ok; then swebdav=up; fi
    fi
  else
    swebdav=missing
    broken=$((broken + 1))
  fi

  ts=$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST')
  echo "$ts 3080=$s3080 http=$http cloudflared=$scf public=$pub pubcode=${pub_code:-na} webdav=$swebdav broken=$broken" >>"$LOG"

  loop=$((loop + 1))
  # Every ~10 min: restore/mirror ACP binaries if Update wiped ~/.cache
  if [ $((loop % 10)) -eq 0 ] && [ -x "$BOOT/ensure-acp-agents.sh" ]; then
    "$BOOT/ensure-acp-agents.sh" 9>&- || true
  fi
  # Same cadence: sync boot scripts from git/GitHub (auto-sync self-rate-limits to 1h)
  if [ $((loop % 10)) -eq 0 ] && [ -x "$BOOT/auto-sync-boot.sh" ]; then
    "$BOOT/auto-sync-boot.sh" 9>&- || true
    if [ -f "$HB/boot-sync.reload" ]; then
      rm -f "$HB/boot-sync.reload"
      echo "$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST') boot scripts updated; reloading watchdog" >>"$LOG"
      # Replace this process with a fresh watchdog so the new script is loaded.
      if [ -x "$BOOT/reload-watchdog-once.sh" ]; then
        nohup "$BOOT/reload-watchdog-once.sh" >/dev/null 2>&1 9>&- &
        exit 0
      fi
    fi
  fi

  sleep 60 9>&-
done
