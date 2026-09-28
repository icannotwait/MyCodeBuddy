#!/bin/bash
set -euo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

ROOT=/workspace/webdav
START=$ROOT/start.sh
CFG=$ROOT/wsgidav.yaml
LOG=$ROOT/webdav.log
PIDFILE=$ROOT/webdav.pid
LOCK=/workspace/heartbeat/webdav.start.lock
PORT=6065

mkdir -p /workspace/heartbeat "$ROOT/data"

if [ ! -x "$ROOT/venv/bin/wsgidav" ]; then
  echo "missing wsgidav venv at $ROOT/venv" >&2
  exit 1
fi
if [ ! -f "$CFG" ]; then
  echo "missing $CFG" >&2
  exit 1
fi
if [ ! -x "$START" ]; then
  echo "missing $START" >&2
  exit 1
fi

exec 9>"$LOCK"
if ! flock -w 15 9; then
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-webdav: could not get lock in 15s" >>"$LOG"
  exit 1
fi

port_up() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | grep -q ":${PORT}"
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/$PORT) >/dev/null 2>&1
}

# Prefer live listeners; also collapse duplicate wsgidav processes.
pids=$(pgrep -f '/workspace/webdav/venv/bin/wsgidav' 2>/dev/null || true)
if port_up; then
  if [ -n "${pids:-}" ]; then
    keep=$(echo "$pids" | awk 'NR==1{print; exit}')
    extras=$(echo "$pids" | awk -v k="$keep" '$1!=k')
    if [ -n "${extras:-}" ]; then
      # shellcheck disable=SC2086
      kill $extras 2>/dev/null || true
      sleep 1
      for p in $extras; do
        if kill -0 "$p" 2>/dev/null; then kill -9 "$p" 2>/dev/null || true; fi
      done
    fi
    echo "$keep" >"$PIDFILE"
  fi
  exit 0
fi

# Port down: kill stale processes then start
if [ -n "${pids:-}" ]; then
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  sleep 1
  for p in $pids; do
    if kill -0 "$p" 2>/dev/null; then kill -9 "$p" 2>/dev/null || true; fi
  done
fi

nohup "$START" >>"$LOG" 2>&1 9>&- &
echo $! >"$PIDFILE"
exit 0
