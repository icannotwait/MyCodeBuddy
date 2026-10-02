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

# Public URL probe: unset → live-box default; empty string → skip probe.
if [ ! -v CODEG_PUBLIC_URL ]; then
  CODEG_PUBLIC_URL="https://drawcode.20241021.best/"
fi
# Consecutive public failures before treating tunnel as broken (anti-flap).
PUBLIC_FAIL_THRESHOLD=${PUBLIC_FAIL_THRESHOLD:-2}
# Seconds between tunnel-only restarts (anti-flap). Default 3 minutes.
TUNNEL_RESTART_COOLDOWN=${TUNNEL_RESTART_COOLDOWN:-180}

mkdir -p "$HB"

# Single-instance via flock
exec 9>"$LOCK"
if ! flock -n 9; then
  exit 0
fi

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
  code=$(curl -sS -o /tmp/codeg-watchdog-body.$$ -w '%{http_code}' --connect-timeout 2 --max-time 5 http://127.0.0.1:3080/ 2>/dev/null || echo 000)
  len=$(wc -c < /tmp/codeg-watchdog-body.$$ 2>/dev/null | tr -d ' ' || echo 0)
  rm -f /tmp/codeg-watchdog-body.$$
  [ "$code" = "200" ] && [ "${len:-0}" -gt 1000 ]
}

# WebDAV: unauthenticated should be 401; proves auth + listener.
webdav_ok() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 http://127.0.0.1:6065/ 2>/dev/null || echo 000)
  [ "$code" = "401" ] || [ "$code" = "200" ]
}

# Public edge probe. Sets globals: pub_status (skip|ok|bad|fail), pub_code, pub_detail.
# Returns 0 if healthy/skipped, 1 if tunnel looks broken.
probe_public() {
  pub_status=skip
  pub_code=000
  pub_detail=
  if [ -z "${CODEG_PUBLIC_URL}" ]; then
    return 0
  fi
  local body=/tmp/codeg-watchdog-public.$$
  pub_code=$(curl -sS -o "$body" -w '%{http_code}' --connect-timeout 5 --max-time 12 \
    -L --max-redirs 2 "$CODEG_PUBLIC_URL" 2>/dev/null || echo 000)
  local mention=
  if [ -f "$body" ] && grep -qiE '1033|error.?code.?1033|Cloudflare Tunnel error' "$body" 2>/dev/null; then
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
    pub_detail="connect_fail local=$http"
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

tunnel_cooldown_ok() {
  local now last
  now=$(date +%s)
  if [ -f "$TUNNEL_RESTART_STAMP" ]; then
    last=$(cat "$TUNNEL_RESTART_STAMP" 2>/dev/null || echo 0)
    if [ -n "${last:-}" ] && [ "$last" -gt 0 ] 2>/dev/null; then
      if [ $((now - last)) -lt "$TUNNEL_RESTART_COOLDOWN" ]; then
        return 1
      fi
    fi
  fi
  return 0
}

# Restart ONLY cloudflared (never codeg-server). Uses force mode so start script
# does not exit early when a zombie process is still present.
restart_tunnel() {
  local reason=$1
  echo "$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST') restart cloudflared ONLY: $reason" >>"$LOG"
  date +%s >"$TUNNEL_RESTART_STAMP"
  FORCE_RESTART=1 "$BOOT/start-codeg-tunnel.sh" --force 9>&- || true
  sleep 2
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

  # Public edge probe: detect zombie tunnels (process up, edge 530/1033).
  if probe_public; then
    pub="$pub_status"
    if [ "$pub_status" = "ok" ] || [ "$pub_status" = "skip" ]; then
      pub_fail_streak=0
    fi
  else
    pub="$pub_status"
    pub_fail_streak=$((pub_fail_streak + 1))
    broken=$((broken + 1))
    echo "$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST') public probe FAIL streak=$pub_fail_streak/$PUBLIC_FAIL_THRESHOLD url=$CODEG_PUBLIC_URL $pub_detail cloudflared=$scf http=$http" >>"$LOG"
    if [ "$pub_fail_streak" -ge "$PUBLIC_FAIL_THRESHOLD" ]; then
      if tunnel_cooldown_ok; then
        restart_tunnel "public $pub_detail after ${pub_fail_streak} consecutive fails (local http=$http)"
        pub_fail_streak=0
        # Re-check process after force restart
        if cf_up; then scf=up; else scf=down; broken=$((broken + 1)); fi
        # Optional immediate re-probe for log clarity (does not count toward streak).
        if probe_public; then
          pub="$pub_status"
        else
          pub="$pub_status"
        fi
      else
        last=$(cat "$TUNNEL_RESTART_STAMP" 2>/dev/null || echo 0)
        now=$(date +%s)
        left=$((TUNNEL_RESTART_COOLDOWN - (now - last)))
        echo "$(TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M:%S CST') tunnel restart skipped: cooldown ${left}s remaining" >>"$LOG"
      fi
    fi
  fi

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
