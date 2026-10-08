#!/bin/bash
# Keep this box joined to the user's tailnet. Called by the watchdog every loop
# and safe to run by hand; a healthy daemon is left alone.
#   start-tailscale.sh          ensure binaries + tailscaled + login
#   start-tailscale.sh --state  print up|starting|needs-login|down|missing|disabled
#                               (read-only; used for the watchdog status line)
# Auth key (reusable, machine-local, never logged or synced): env TS_AUTHKEY,
# else $TS_DIR/authkey (mode 600, see set-tailscale-key.sh). Without a key it
# never blocks: it only logs the interactive login URL (rate-limited).
# Optional TS_* overrides (env wins) in the watchdog's local.env: TS_ENABLE,
# TS_HOSTNAME, TS_SSH, TS_USERSPACE (auto|0|1), TS_EXTRA_UP_ARGS,
# TS_URL_LOG_SECS, TS_AUTH_RETRY_SECS.
set -uo pipefail
export PATH=/workspace/bin:/exec-daemon:$PATH
export HOME=/home/box USER=box LOGNAME=box

BOOT=${BOOT:-/workspace/codeg-boot}
HB=${CODEG_HB:-/workspace/heartbeat}
BIN=${TS_BIN_DIR:-/workspace/bin}
TS_DIR=${TS_DIR:-$HOME/tailscale}
STATE=$TS_DIR/tailscaled.state
SOCK=$TS_DIR/tailscaled.sock
DLOG=$TS_DIR/tailscaled.log
KEYFILE=$TS_DIR/authkey
LOG=$HB/watchdog.log
LOCK=$HB/tailscale.start.lock
URL_STAMP=$HB/tailscale-url.stamp
AUTH_STAMP=$HB/tailscale-auth.stamp
DL_STAMP=$HB/tailscale-download.stamp

tslog() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) start-tailscale: $*" >>"$LOG"; }
redact() { sed -E 's/tskey-[A-Za-z0-9_?=&.-]+/tskey-REDACTED/g'; }
uint_or() { case $1 in '' | *[!0-9]*) echo "$2" ;; *) echo "$((10#$1))" ;; esac; }

# Parse TS_* keys from local.env as data (never sourced). The auth key is
# deliberately not accepted here: it lives only in $KEYFILE or env.
load_ts_config() {
  local file=${CODEG_WATCHDOG_CONFIG:-$BOOT/local.env} line key value
  [ -f "$file" ] && [ -r "$file" ] || return 0
  while IFS= read -r line || [ -n "$line" ]; do
    line=${line%$'\r'}
    line=${line#"${line%%[![:space:]]*}"}
    line=${line#export }
    [[ "$line" =~ ^(TS_ENABLE|TS_HOSTNAME|TS_SSH|TS_USERSPACE|TS_EXTRA_UP_ARGS|TS_URL_LOG_SECS|TS_AUTH_RETRY_SECS)=(.*)$ ]] || continue
    key=${BASH_REMATCH[1]}
    value=${BASH_REMATCH[2]}
    if [[ "$value" =~ ^\"([^\"]*)\"[[:space:]]*(#.*)?$ ]] ||
      [[ "$value" =~ ^\'([^\']*)\'[[:space:]]*(#.*)?$ ]]; then
      value=${BASH_REMATCH[1]}
    else
      value=${value%%[[:space:]]#*}
      value=${value%"${value##*[![:space:]]}"}
    fi
    [ -n "${!key:-}" ] || printf -v "$key" '%s' "$value"
  done <"$file"
}

load_ts_config
TS_ENABLE=${TS_ENABLE:-1}
TS_HOSTNAME=${TS_HOSTNAME:-grokbot}
TS_SSH=${TS_SSH:-1}
TS_USERSPACE=${TS_USERSPACE:-auto}
TS_EXTRA_UP_ARGS=${TS_EXTRA_UP_ARGS:-}
TS_URL_LOG_SECS=$(uint_or "${TS_URL_LOG_SECS:-}" 1800)
TS_AUTH_RETRY_SECS=$(uint_or "${TS_AUTH_RETRY_SECS:-}" 600)
if [[ ! "$TS_HOSTNAME" =~ ^[A-Za-z0-9][A-Za-z0-9-]{0,62}$ ]]; then
  tslog "invalid TS_HOSTNAME; using grokbot"
  TS_HOSTNAME=grokbot
fi

if [ "$(id -u)" = 0 ]; then
  SUDO=()
elif sudo -n true 2>/dev/null; then
  SUDO=(sudo -n)
else
  SUDO=(false)
fi

tsc() { "${SUDO[@]}" "$BIN/tailscale" --socket="$SOCK" "$@"; }
status_json() { tsc status --peers=false --json 2>/dev/null; }
json_field() { sed -n "s/^[[:space:]]*\"$1\": *\"\([^\"]*\)\".*/\1/p" | head -n 1; }
backend_state() { status_json | json_field BackendState; }
daemon_pid() { pgrep -f -- "^$BIN/tailscaled .*--socket=$SOCK( |\$)" 2>/dev/null | head -n 1; }

state_word() {
  [ "$TS_ENABLE" = 0 ] && { echo disabled; return; }
  if [ ! -x "$BIN/tailscale" ] || [ ! -x "$BIN/tailscaled" ]; then echo missing; return; fi
  case $(backend_state) in
    Running) echo up ;;
    Starting) echo starting ;;
    NeedsLogin | NeedsMachineAuth | Stopped) echo needs-login ;;
    *) echo down ;;
  esac
}

if [ "${1:-}" = "--state" ]; then
  state_word
  exit 0
fi
[ "$TS_ENABLE" = 0 ] && exit 0

mkdir -p "$HB"
exec 9>"$LOCK"
flock -w 20 9 || { tslog "could not get lock in 20s"; exit 1; }

ensure_binaries() {
  [ -x "$BIN/tailscale" ] && [ -x "$BIN/tailscaled" ] && return 0
  local now last=0 arch name tmp
  now=$(date +%s)
  [ -f "$DL_STAMP" ] && last=$(uint_or "$(cat "$DL_STAMP" 2>/dev/null)" 0)
  [ $((now - last)) -lt 3600 ] && return 1
  echo "$now" >"$DL_STAMP"
  case $(uname -m) in
    x86_64) arch=amd64 ;; aarch64 | arm64) arch=arm64 ;; armv7l | armv6l) arch=arm ;; i?86) arch=386 ;;
    *) tslog "unsupported arch $(uname -m)"; return 1 ;;
  esac
  name=$(curl -fsS --max-time 20 'https://pkgs.tailscale.com/stable/?mode=json' |
    sed -n "s/.*\"$arch\": *\"\(tailscale_[0-9.]*_$arch\.tgz\)\".*/\1/p" | head -n 1)
  [ -n "$name" ] || { tslog "cannot resolve stable tarball for $arch"; return 1; }
  tmp=$(mktemp -d)
  if curl -fsSL --max-time 300 -o "$tmp/ts.tgz" "https://pkgs.tailscale.com/stable/$name" &&
    [ "$(curl -fsS --max-time 20 "https://pkgs.tailscale.com/stable/$name.sha256" | tr -d '[:space:]')" = \
      "$(sha256sum "$tmp/ts.tgz" | cut -d' ' -f1)" ] &&
    tar -xzf "$tmp/ts.tgz" -C "$tmp" &&
    install -m 755 "$tmp"/tailscale_*/tailscale "$tmp"/tailscale_*/tailscaled "$BIN/"; then
    tslog "installed $name into $BIN"
    rm -rf "$tmp"
    return 0
  fi
  rm -rf "$tmp"
  tslog "download/verify of $name failed; retry in 1h"
  return 1
}

start_daemon() {
  local args pid age
  if [ "${SUDO[0]:-}" = false ]; then tslog "no passwordless sudo; cannot start tailscaled"; return 1; fi
  pid=$(daemon_pid)
  if [ -n "$pid" ]; then
    status_json >/dev/null && return 0
    sleep 3 9>&-
    status_json >/dev/null && return 0
    age=$(uint_or "$(ps -o etimes= -p "$pid" 2>/dev/null | tr -d ' ')" 0)
    [ "$age" -lt 60 ] && return 0
    tslog "tailscaled pid=$pid unresponsive for ${age}s; restarting"
    "${SUDO[@]}" kill "$pid" 2>/dev/null || true
    sleep 2
    "${SUDO[@]}" kill -9 "$pid" 2>/dev/null || true
  fi
  mkdir -p "$TS_DIR" && chmod 700 "$TS_DIR"
  args=(--state="$STATE" --socket="$SOCK" --statedir="$TS_DIR")
  if [ "$TS_USERSPACE" = 1 ] || { [ "$TS_USERSPACE" = auto ] && [ ! -c /dev/net/tun ]; }; then
    args+=(--tun=userspace-networking --socks5-server=localhost:1055 --outbound-http-proxy-listen=localhost:1055)
  fi
  "${SUDO[@]}" nohup "$BIN/tailscaled" "${args[@]}" >>"$DLOG" 2>&1 </dev/null 9>&- &
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    status_json >/dev/null && { tslog "started tailscaled pid=$(daemon_pid) ${args[*]:3}"; return 0; }
  done
  tslog "tailscaled did not answer on $SOCK within 10s; see $DLOG"
  return 1
}

log_login_url() {
  local url now last=0 lasturl=
  url=$(status_json | json_field AuthURL)
  [ -n "$url" ] || return 0
  now=$(date +%s)
  [ -f "$URL_STAMP" ] && read -r last lasturl <"$URL_STAMP"
  last=$(uint_or "$last" 0)
  if [ "$url" != "$lasturl" ] || [ $((now - last)) -ge "$TS_URL_LOG_SECS" ]; then
    echo "$now $url" >"$URL_STAMP"
    tslog "needs login (no auth key): open $url or run set-tailscale-key.sh"
  fi
}

ensure_login() {
  local st key_src='' tmpkey='' perm now last=0 up err rc
  st=$(backend_state)
  case $st in NeedsLogin | Stopped) ;; NeedsMachineAuth) log_login_url; return 0 ;; *) return 0 ;; esac
  up=(up --reset --hostname="$TS_HOSTNAME" --timeout=30s)
  [ "$TS_SSH" = 1 ] && up+=(--ssh)
  # shellcheck disable=SC2206
  [ -n "$TS_EXTRA_UP_ARGS" ] && up+=($TS_EXTRA_UP_ARGS)
  if [ -n "${TS_AUTHKEY:-}" ]; then
    tmpkey=$(umask 077 && mktemp "$TS_DIR/.authkey.XXXXXX") || return 1
    printf '%s\n' "$TS_AUTHKEY" >"$tmpkey"
    key_src=$tmpkey
  elif [ -f "$KEYFILE" ]; then
    perm=$(stat -c '%a %U' "$KEYFILE" 2>/dev/null)
    case $perm in "600 box" | "400 box" | "600 root" | "400 root") key_src=$KEYFILE ;;
      *) tslog "ignoring auth key file: perms '$perm' (need 600, owner box)" ;; esac
  fi
  now=$(date +%s)
  [ -f "$AUTH_STAMP" ] && last=$(uint_or "$(cat "$AUTH_STAMP" 2>/dev/null)" 0)
  if [ -z "$key_src" ]; then
    # No key: never block. Kick an interactive login in the background only
    # when there is no pending URL yet (e.g. after a daemon restart) or the
    # node is Stopped, then just report the URL.
    if { [ "$st" = Stopped ] || [ -z "$(status_json | json_field AuthURL)" ]; } &&
      [ $((now - last)) -ge "$TS_AUTH_RETRY_SECS" ]; then
      echo "$now" >"$AUTH_STAMP"
      tslog "backend=$st; requesting interactive login"
      timeout 40 "${SUDO[@]}" "$BIN/tailscale" --socket="$SOCK" "${up[@]}" >/dev/null 2>&1 </dev/null 9>&- &
      sleep 3 9>&-
    fi
    log_login_url
    return 0
  fi
  if [ $((now - last)) -lt "$TS_AUTH_RETRY_SECS" ]; then
    [ -n "$tmpkey" ] && rm -f "$tmpkey"
    return 0
  fi
  echo "$now" >"$AUTH_STAMP"
  err=$(tsc "${up[@]}" --auth-key="file:$key_src" 2>&1 </dev/null 9>&-)
  rc=$?
  [ -n "$tmpkey" ] && rm -f "$tmpkey"
  if [ "$rc" = 0 ]; then
    rm -f "$URL_STAMP"
    tslog "joined tailnet with auth key as $TS_HOSTNAME ($(tsc ip -4 2>/dev/null | head -n 1))"
  else
    tslog "tailscale up with auth key failed rc=$rc: $(printf '%s' "$err" | redact | tr '\n' ' ' | cut -c1-300); retry in ${TS_AUTH_RETRY_SECS}s"
  fi
}

ensure_binaries || exit 0
start_daemon || exit 0
ensure_login
exit 0
