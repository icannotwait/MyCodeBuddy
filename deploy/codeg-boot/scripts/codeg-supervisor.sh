#!/bin/bash
set -uo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH
# Box services run as box. Pin HOME/USER so a launch from a sandbox shell
# (HOME=/workspace/agent-reach/home) cannot leak into $HOME-based paths
# (cloudflared config, ~/.grok/bin, ~/.local/bin/pi).
export HOME=/home/box USER=box LOGNAME=box

BOOT=/workspace/codeg-boot
HB=/workspace/heartbeat
LOCK=$HB/supervisor.lock

mkdir -p /workspace/codeg-data "$HB"

got_lock=0
exec 8>"$LOCK"
if flock -n 8; then
  got_lock=1
fi

# Handoff: even without lock, restore once then exit
restore_once() {
  if [ ! -x /workspace/codeg-dist/codeg-server ]; then
    echo "missing codeg-server binary" >&2
  fi

  port_up() {
    if command -v ss >/dev/null 2>&1; then
      ss -ltn 2>/dev/null | grep -q ':3080'
      return $?
    fi
    (echo >/dev/tcp/127.0.0.1/3080) >/dev/null 2>&1
  }

  if ! port_up; then
    nohup "$BOOT/start-codeg-server.sh" >>"$HB/codeg-server.log" 2>&1 8>&- 9>&- &
    for i in 1 2 3 4 5; do
      sleep 1
      if port_up; then break; fi
    done
  fi

  if ! ps -C cloudflared >/dev/null 2>&1; then
    "$BOOT/start-codeg-tunnel.sh" 8>&- 9>&- || true
  fi

  if [ -x "$BOOT/start-webdav.sh" ]; then
    "$BOOT/start-webdav.sh" 8>&- 9>&- || true
  fi

  # watchdog running?
  wd_running=0
  if [ -f "$HB/watchdog.pid" ]; then
    wpid=$(cat "$HB/watchdog.pid" 2>/dev/null || true)
    if [ -n "${wpid:-}" ] && kill -0 "$wpid" 2>/dev/null; then
      wd_running=1
    fi
  fi
  if [ "$wd_running" = "0" ]; then
    if ! pgrep -f '[c]odeg-watchdog\.sh' >/dev/null 2>&1; then
      nohup "$BOOT/codeg-watchdog.sh" >/dev/null 2>&1 8>&- &
    fi
  fi

  # ACP binary cache survives Update via durable mirror + re-download
  if [ -x "$BOOT/ensure-acp-agents.sh" ]; then
    nohup "$BOOT/ensure-acp-agents.sh" >/dev/null 2>&1 8>&- 9>&- &
  fi
}

restore_once

if [ "$got_lock" = "0" ]; then
  # 没抢到锁仍会恢复然后退出
  exit 0
fi

# Holding lock: stay briefly so concurrent login hooks see lock, then exit
# (watchdog owns long-term recovery; supervisor is bootstrap)
exit 0
