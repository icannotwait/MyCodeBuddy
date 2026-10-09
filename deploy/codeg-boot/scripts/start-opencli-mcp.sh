#!/bin/bash
# Ensure opencli-mcp (Streamable HTTP on 127.0.0.1:18765) is running.
# Does NOT launch Chrome. opencli daemon is started by the MCP server itself.
set -euo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

ROOT=/workspace/agent-reach/opencli-mcp
START=$ROOT/run.sh
STOP=$ROOT/stop.sh
LOG=/workspace/heartbeat/opencli-mcp-watch.log
PIDFILE=$ROOT/opencli-mcp.pid
LOCK=/workspace/heartbeat/opencli-mcp.start.lock
PORT=18765

mkdir -p /workspace/heartbeat "$ROOT/logs"

if [ ! -x "$START" ]; then
  echo "missing $START" >&2
  exit 1
fi

exec 9>"$LOCK"
if ! flock -w 15 9; then
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-opencli-mcp: could not get lock in 15s" >>"$LOG"
  exit 1
fi

port_up() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | grep -q "127.0.0.1:${PORT}"
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/"$PORT") >/dev/null 2>&1
}

# Health: unauthenticated POST to /mcp must return 401 (proves auth + listener).
mcp_ok() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 2 --max-time 5 \
    -X POST -H 'Content-Type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"ping"}' \
    "http://127.0.0.1:${PORT}/mcp" 2>/dev/null) || code=000
  [ "$code" = "401" ]
}

# Warn only — never launch or drive Chrome / Browser Bridge.
chrome_bridge_warn() {
  if pgrep -f -- '--user-data-dir=/home/box/chrome-profile/' >/dev/null 2>&1; then
    return 0
  fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) warn: box Chrome profile (Browser Bridge) not running; OpenCLI browser reads may fail" >>"$LOG"
}

if port_up && mcp_ok; then
  chrome_bridge_warn
  exit 0
fi

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) opencli-mcp unhealthy or down; restarting" >>"$LOG"
if [ -x "$STOP" ]; then
  "$STOP" >>"$LOG" 2>&1 || true
fi
# Also kill any stray listener on the port
if port_up; then
  # Prefer pidfile
  if [ -f "$PIDFILE" ]; then
    old=$(cat "$PIDFILE" 2>/dev/null || true)
    if [ -n "${old:-}" ]; then kill "$old" 2>/dev/null || true; sleep 1; kill -9 "$old" 2>/dev/null || true; fi
  fi
fi
rm -f "$PIDFILE"
nohup "$START" >>"$LOG" 2>&1 9>&- &
for i in 1 2 3 4 5 6 7 8 9 10 12 15 20 25 30 40 50 60; do
  sleep 1
  if port_up && mcp_ok; then
    chrome_bridge_warn
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) opencli-mcp up after ${i}s" >>"$LOG"
    exit 0
  fi
done
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) opencli-mcp failed to become healthy" >>"$LOG"
exit 1
