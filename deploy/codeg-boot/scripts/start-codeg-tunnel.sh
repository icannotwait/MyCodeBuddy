#!/bin/bash
set -euo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH

CF=/workspace/bin/cloudflared
LOG=/workspace/heartbeat/cloudflared.log
# Never depend on the caller's $HOME: a watchdog relaunched from a sandbox shell
# with HOME=/workspace/agent-reach/home made a forced restart fail with
# "missing cloudflared config" while the tunnel was a 1033 zombie (2026-10-08).
# Resolution order: CF_CONFIG > box owner's ~/.cloudflared > $HOME/.cloudflared
# > /etc/cloudflared. CODEG_CF_HOME (default /home/box) exists for tests.
CF_HOME_DEFAULT=${CODEG_CF_HOME:-/home/box}
PIDFILE=/workspace/heartbeat/cloudflared.pid
# v2: old cloudflared.start.lock may still be held by a cloudflared that
# inherited the flock fd from an earlier start (blocks every later start).
LOCK=/workspace/heartbeat/cloudflared.start.lock.v2

FORCE=0
PRINT_CONFIG=0
for arg in "$@"; do
  case $arg in
    --force) FORCE=1 ;;
    --print-config) PRINT_CONFIG=1 ;;
  esac
done
if [ "${FORCE_RESTART:-0}" = "1" ]; then
  FORCE=1
fi

resolve_cf_config() {
  local candidate
  if [ -n "${CF_CONFIG:-}" ]; then
    printf '%s' "$CF_CONFIG"
    return 0
  fi
  for candidate in "$CF_HOME_DEFAULT/.cloudflared/config.yml" \
    "${HOME:+$HOME/.cloudflared/config.yml}" /etc/cloudflared/config.yml; do
    if [ -n "$candidate" ] && [ -f "$candidate" ]; then
      printf '%s' "$candidate"
      return 0
    fi
  done
  printf '%s' "$CF_HOME_DEFAULT/.cloudflared/config.yml"
}

CFG=$(resolve_cf_config)
# cloudflared's own default lookups (e.g. cert.pem in ~/.cloudflared) must see
# the same home as the config; credentials-file inside the config is absolute.
case $CFG in
  */.cloudflared/config.yml) CF_RUN_HOME=${CFG%/.cloudflared/config.yml} ;;
  *) CF_RUN_HOME=$CF_HOME_DEFAULT ;;
esac

# Dry run: show what a (forced) start would use; never locks, kills or starts.
if [ "$PRINT_CONFIG" = "1" ]; then
  if [ -f "$CFG" ]; then state=found; else state=missing; fi
  printf 'config=%s state=%s run_home=%s force=%s\n' "$CFG" "$state" "$CF_RUN_HOME" "$FORCE"
  [ "$state" = found ]
  exit $?
fi

mkdir -p /workspace/heartbeat

# The watchdog discards stderr; keep start failures in cloudflared.log too.
fail() {
  echo "$*" >&2
  echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-tunnel: $* (force=$FORCE caller_home=${HOME:-unset})" >>"$LOG"
  exit 1
}

if [ ! -x "$CF" ]; then
  fail "missing cloudflared at $CF"
fi
if [ ! -f "$CFG" ]; then
  fail "missing cloudflared config $CFG"
fi

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

# Gateway fake-ip DNS (Clash/sing-box) maps *.argotunnel.com to 198.18.0.0/15,
# so cloudflared dials 198.18.0.1:7844, times out, registers 0 connections,
# and the public hostname returns 530/1033. Use ahostsv4 (getent hosts can
# return only AAAA and hide a hijacked A). Resolve real edge A records via
# DoH and pass --edge. CODEG_TUNNEL_EDGE_DOH=auto|on|off (default auto).
# Empty DoH results leave EDGE_ARGS empty so launch is unchanged.
resolve_tunnel_edge_args() {
  local mode=${CODEG_TUNNEL_EDGE_DOH:-auto}
  local sys_ip="" need_doh=0 host ip
  EDGE_ARGS=""
  [ "$mode" != "off" ] || return 0

  # ahostsv4: IPv4 only. timeout: a hung stub resolver must not hold the start lock.
  sys_ip=$( { timeout 2 getent ahostsv4 region1.v2.argotunnel.com 2>/dev/null || true; } | awk '{print $1; exit}')
  case "$sys_ip" in
    198.18.*|198.19.*|"") need_doh=1 ;;
  esac
  [ "$mode" = "on" ] && need_doh=1
  [ "$need_doh" = 1 ] || return 0

  for host in region1.v2.argotunnel.com region2.v2.argotunnel.com; do
    # Ignore curl/grep failures so a blank DoH answer falls back.
    # Pin 1.1.1.1 so DoH does not use the same fake-ip resolver.
    # shellcheck disable=SC2046
    for ip in $(
      curl -s -m 8 -H 'accept: application/dns-json' \
        --resolve cloudflare-dns.com:443:1.1.1.1 \
        "https://cloudflare-dns.com/dns-query?name=${host}&type=A" \
        | grep -o '"data":"[0-9.]*"' \
        | cut -d'"' -f4 \
        | head -4 \
        || true
    ); do
      EDGE_ARGS="$EDGE_ARGS --edge ${ip}:7844"
    done
  done

  if [ -n "$EDGE_ARGS" ]; then
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-tunnel: fake-ip DNS (${sys_ip}); using DoH edge:$EDGE_ARGS" >>"$LOG"
  else
    echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-tunnel: fake-ip DNS (${sys_ip}); DoH returned no edges, launching without --edge" >>"$LOG"
  fi
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

echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-codeg-tunnel: starting force=$FORCE config=$CFG run_home=$CF_RUN_HOME" >>"$LOG"
resolve_tunnel_edge_args
# Critical: close lock fd in the child so cloudflared does not inherit flock.
# shellcheck disable=SC2086
HOME=$CF_RUN_HOME nohup "$CF" tunnel --config "$CFG" --protocol http2 $EDGE_ARGS run 9>&- >>"$LOG" 2>&1 &
echo $! >"$PIDFILE"
echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) started cloudflared pid=$! force=$FORCE config=$CFG" >>"$LOG"
exit 0
