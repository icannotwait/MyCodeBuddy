#!/bin/bash
set -euo pipefail
# Include npm global bin so ACP adapters can resolve `pi`, `grok`, etc.
export PATH=$HOME/.local/bin:/workspace/bin:/exec-daemon:$PATH
export CODEG_HOST=127.0.0.1
export CODEG_PORT=3080
export CODEG_STATIC_DIR=/workspace/codeg-dist/web
export CODEG_DATA_DIR=/workspace/codeg-data
export CODEG_MCP_BIN=/workspace/codeg-dist/codeg-mcp
# Disable Ready-session idle reclaim (user ~<=10 conns; lease maintenance still runs)
export CODEG_ACP_IDLE_TIMEOUT_SECS=0
# Lease ping / renew debug (CODEG_LOG wins over RUST_LOG); keep other filters if present
if [ -n "${CODEG_LOG:-}" ]; then
  case ",${CODEG_LOG}," in
    *,codeg_lib::web::ws=debug,*|*,codeg_lib::web::ws=trace,*) ;;
    *) export CODEG_LOG="${CODEG_LOG},codeg_lib::web::ws=debug" ;;
  esac
elif [ -n "${RUST_LOG:-}" ]; then
  case ",${RUST_LOG}," in
    *,codeg_lib::web::ws=debug,*|*,codeg_lib::web::ws=trace,*) export CODEG_LOG="$RUST_LOG" ;;
    *) export CODEG_LOG="${RUST_LOG},codeg_lib::web::ws=debug" ;;
  esac
else
  export CODEG_LOG=info,codeg_lib::web::ws=debug
fi
# Point pi-acp at the installed pi binary even if PATH is later narrowed.
export PI_ACP_PI_COMMAND=$HOME/.local/bin/pi
# Antigravity wire companion is unsupported in-code (skip inject; no probe env).
TOKEN_FILE=/workspace/codeg-data/CODEG_TOKEN
if [ ! -f "$TOKEN_FILE" ]; then echo "missing token file"; exit 1; fi
export CODEG_TOKEN=$(tr -d "\n\r" < "$TOKEN_FILE")
unset TOKEN_FILE

BIN=/workspace/codeg-dist/codeg-server
INDEX="$CODEG_STATIC_DIR/index.html"
# Refuse to start during deploy races: missing binary or index.html makes the
# server fall back to cwd/out (/out) and serve empty 404 forever while still
# binding :3080 — watchdog used to treat that as healthy.
ready=0
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
  if [ -x "$BIN" ] && [ -f "$INDEX" ]; then
    ready=1
    break
  fi
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-server: waiting for binary+index (bin=$([ -x "$BIN" ] && echo ok || echo missing) index=$([ -f "$INDEX" ] && echo ok || echo missing))"
  sleep 1
done
if [ "$ready" != "1" ]; then
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-server: abort — need executable $BIN and $INDEX" >&2
  exit 1
fi

# Roundtable: crun must create containers under codeg-roundtable, so put this
# process (which becomes codeg-server via exec) into the delegated launcher
# cgroup. Narrow sudo: one tee to that one file with our own PID. No-op when
# the cgroup dir is missing; never fatal.
RT_LAUNCHER=/sys/fs/cgroup/codeg-roundtable/launcher
if [ -d "$RT_LAUNCHER" ] && [ -f "$RT_LAUNCHER/cgroup.procs" ]; then
  if echo $$ | sudo -n /usr/bin/tee "$RT_LAUNCHER/cgroup.procs" >/dev/null 2>&1; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-server: pid $$ -> codeg-roundtable/launcher"
  else
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-server: WARN could not join codeg-roundtable/launcher (roundtable containers will fail)" >&2
  fi
fi
unset RT_LAUNCHER

exec "$BIN"
