#!/bin/bash
set -euo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

CF=/workspace/bin/cloudflared
CFG=${CF_CONFIG:-$HOME/.cloudflared/config.yml}
LOG=/workspace/heartbeat/cloudflared.log
PIDFILE=/workspace/heartbeat/cloudflared.pid
# v2: old cloudflared.start.lock may still be held by a cloudflared that
# inherited the flock fd from an earlier start (blocks every later start).
LOCK=/workspace/heartbeat/cloudflared.start.lock.v2

FORCE=0
if [ "${FORCE_RESTART:-0}" = "1" ] || [ "${1:-}" = "--force" ]; then
  FORCE=1
fi

if [ ! -x "$CF" ]; then
  echo "missing cloudflared at $CF" >&2
  exit 1
fi
if [ ! -f "$CFG" ]; then
  echo "missing cloudflared config $CFG" >&2
  exit 1
fi

mkdir -p /workspace/heartbeat

# Serialize starts so concurrent supervisor/watchdog/login hooks cannot race.
# -w 15: never hang the watchdog forever if something else holds the lock.
exec 9>"$LOCK"
if ! flock -w 15 9; then
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-tunnel: could not get lock in 15s" >>"$LOG"
  exit 1
fi

# Match real cloudflared processes only (NOT shells whose cmdline mentions cloudflared).
cf_pids() {
  # ps -C exits 1 when no match; under set -euo pipefail that aborted the whole start.
  { ps -C cloudflared -o pid= 2>/dev/null || true; } | awk 'NF{print $1}' | sort -u
}

kill_cloudflared() {
  local pids=$1
  if [ -z "${pids:-}" ]; then
    return 0
  fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) killing cloudflared force=$FORCE pids=$(echo $pids | tr '\n' ' ')" >>"$LOG"
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  sleep 1
  for p in $pids; do
    if kill -0 "$p" 2>/dev/null; then
      kill -9 "$p" 2>/dev/null || true
    fi
  done
  sleep 1
}

pids=$(cf_pids)
if [ -n "$pids" ] && [ "$FORCE" = "0" ]; then
  # Oldest by elapsed time (ps etimes descending).
  keep=$({ ps -C cloudflared -o etimes=,pid= 2>/dev/null || true; } | awk 'NF{print $1,$2}' | sort -nr | awk 'NR==1{print $2}')
  if [ -z "${keep:-}" ]; then
    keep=$(echo "$pids" | head -n1)
  fi
  extras=$(echo "$pids" | awk -v k="$keep" '$1!=k')
  if [ -n "${extras:-}" ]; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) collapsing duplicate cloudflared; keep=$keep kill=$(echo $extras | tr '\n' ' ')" >>"$LOG"
    # shellcheck disable=SC2086
    kill $extras 2>/dev/null || true
    sleep 1
    for p in $extras; do
      if kill -0 "$p" 2>/dev/null; then
        kill -9 "$p" 2>/dev/null || true
      fi
    done
  fi
  echo "$keep" >"$PIDFILE"
  exit 0
fi

if [ -n "$pids" ] && [ "$FORCE" = "1" ]; then
  kill_cloudflared "$pids"
  pids=$(cf_pids)
  if [ -n "$pids" ]; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) force restart: cloudflared still alive after kill: $(echo $pids | tr '\n' ' ')" >>"$LOG"
    kill_cloudflared "$pids"
  fi
fi

# Critical: close lock fd in the child so cloudflared does not inherit flock.
nohup "$CF" tunnel --config "$CFG" --protocol http2 run 9>&- >>"$LOG" 2>&1 &
echo $! >"$PIDFILE"
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) started cloudflared pid=$! force=$FORCE" >>"$LOG"
exit 0
