#!/bin/bash
# Start WsgiDAV on :6065 if it is not listening. Called by the watchdog every
# loop. The venv is regenerable state that box restores/updates may drop (it
# vanished on 2026-09-29 and WebDAV stayed down for 9 days), so a missing or
# broken venv is rebuilt here from pinned packages, at most once per
# CODEG_WEBDAV_BOOTSTRAP_INTERVAL seconds. Never creates the config or the
# password file: those are machine-local secrets.
set -euo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

ROOT=${CODEG_WEBDAV_ROOT:-/workspace/webdav}
HB=${CODEG_HB:-/workspace/heartbeat}
VENV=$ROOT/venv
START=$ROOT/start.sh
CFG=$ROOT/wsgidav.yaml
LOG=$ROOT/webdav.log
PIDFILE=$ROOT/webdav.pid
LOCK=$HB/webdav.start.lock
BOOTSTRAP_STAMP=$HB/webdav-venv-bootstrap.stamp
PORT=${CODEG_WEBDAV_PORT:-6065}
PIP_SPEC=${CODEG_WEBDAV_PIP_SPEC:-WsgiDAV==4.3.5 cheroot==11.1.2}
BOOTSTRAP_INTERVAL=${CODEG_WEBDAV_BOOTSTRAP_INTERVAL:-3600}
PYTHON=${CODEG_WEBDAV_PYTHON:-python3}
case $BOOTSTRAP_INTERVAL in ''|*[!0-9]*) BOOTSTRAP_INTERVAL=3600 ;; esac

mkdir -p "$HB" "$ROOT/data"

# The watchdog discards stderr; keep failures visible in webdav.log too.
fail() {
  echo "start-webdav: $*" >&2
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-webdav: $*" >>"$LOG"
  exit 1
}

wlog() {
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-webdav: $*" >>"$LOG"
}

[ -f "$CFG" ] || fail "missing $CFG (machine-local; not created automatically)"
[ -x "$START" ] || fail "missing $START"

exec 9>"$LOCK"
if ! flock -w 15 9; then
  fail "could not get lock in 15s"
fi

port_up() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | grep -q ":${PORT} "
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/"$PORT") >/dev/null 2>&1
}

venv_ok() {
  [ -x "$VENV/bin/wsgidav" ] && [ -x "$VENV/bin/python" ] &&
    "$VENV/bin/python" -c 'import wsgidav, cheroot' >/dev/null 2>&1
}

# Rebuild in place: venv scripts embed absolute paths, so no temp+rename.
bootstrap_venv() {
  local now last=0
  now=$(date +%s)
  if [ -f "$BOOTSTRAP_STAMP" ]; then
    read -r last <"$BOOTSTRAP_STAMP" || last=0
    case $last in ''|*[!0-9]*) last=0 ;; esac
  fi
  if [ $((now - last)) -lt "$BOOTSTRAP_INTERVAL" ]; then
    fail "venv missing/broken at $VENV; rebuild attempted $((now - last))s ago, retry after ${BOOTSTRAP_INTERVAL}s"
  fi
  echo "$now" >"$BOOTSTRAP_STAMP"
  wlog "venv missing/broken at $VENV; rebuilding ($PIP_SPEC)"
  rm -rf "$VENV"
  # shellcheck disable=SC2086
  if "$PYTHON" -m venv "$VENV" >>"$LOG" 2>&1 &&
    timeout 600 "$VENV/bin/pip" install --quiet --disable-pip-version-check \
      $PIP_SPEC >>"$LOG" 2>&1 && venv_ok; then
    wlog "venv rebuilt"
    return 0
  fi
  rm -rf "$VENV"
  fail "venv rebuild failed; see $LOG"
}

# Prefer live listeners; also collapse duplicate wsgidav processes. Match
# "<python> <path>/bin/wsgidav --config=$CFG" anchored at argv[0], so a shell
# whose command line merely mentions wsgidav is never matched (or killed).
pids=$(pgrep -f -- "^[^ ]*python[0-9.]* [^ ]*bin/wsgidav --config=$CFG( |\$)" 2>/dev/null || true)
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

venv_ok || bootstrap_venv

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
