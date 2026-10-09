#!/usr/bin/env bash
# Install Tailscale and join this machine to the tailnet (Linux / macOS).
#   bash join-tailnet.sh [--key-file PATH] [--reset-key] [--hostname NAME] [--no-ssh]
#   curl -fsSL <url>/join-tailnet.sh | bash -s -- [options]
# Auth key (reusable; stored ONLY on this machine, never echoed):
#   env TS_AUTHKEY (this run only) > key file > first-run hidden prompt, which
#   saves it to the key file for later runs. Default key file:
#   Linux /etc/tailscale-join/authkey (root 600), macOS ~/.config/tailscale-join/authkey (600).
#   Press Enter at the prompt to skip and log in via the printed URL instead.
# Windows: install https://tailscale.com/download/windows, save the key to a
#   file, then in an admin PowerShell:
#   tailscale up --unattended --auth-key=file:C:\path\to\authkey
set -euo pipefail

HOSTNAME_ARG=${TS_HOSTNAME:-}
SSH=${TS_SSH:-1}
KEY_FILE=
RESET_KEY=0
while [ $# -gt 0 ]; do
  case $1 in
    --key-file) KEY_FILE=$2; shift 2 ;;
    --reset-key) RESET_KEY=1; shift ;;
    --hostname) HOSTNAME_ARG=$2; shift 2 ;;
    --no-ssh) SSH=0; shift ;;
    -h | --help) sed -n '2,13p' "$0" 2>/dev/null; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

SUDO=
case $(uname -s) in
  Linux)
    [ "$(id -u)" = 0 ] || SUDO=sudo
    KEY_FILE=${KEY_FILE:-/etc/tailscale-join/authkey}
    if ! command -v tailscale >/dev/null 2>&1; then
      curl -fsSL https://tailscale.com/install.sh | sh
    fi
    TS=$(command -v tailscale)
    ;;
  Darwin)
    KEY_FILE=${KEY_FILE:-$HOME/.config/tailscale-join/authkey}
    APP=/Applications/Tailscale.app/Contents/MacOS/Tailscale
    if [ -x "$APP" ]; then
      TS=$APP # GUI app: no sudo; its CLI cannot run the Tailscale SSH server
      if [ "$SSH" = 1 ]; then echo "note: --ssh is unsupported by the macOS app; skipping (use Remote Login)"; SSH=0; fi
    else
      if ! command -v tailscale >/dev/null 2>&1; then
        command -v brew >/dev/null 2>&1 || {
          echo "Install the app (https://tailscale.com/download/mac) or Homebrew, then re-run." >&2
          exit 1
        }
        brew install tailscale
      fi
      sudo brew services start tailscale >/dev/null # open-source tailscaled runs as root
      SUDO=sudo
      TS=$(command -v tailscale)
    fi
    ;;
  *) echo "unsupported OS; on Windows see the header of this script" >&2; exit 1 ;;
esac

# --- auth key: never echoed, passed to tailscale as file:PATH (not on argv) ---
[ "$RESET_KEY" = 1 ] && $SUDO rm -f "$KEY_FILE"
KEY_SRC=
TMP_KEY=
if [ -n "${TS_AUTHKEY:-}" ]; then
  TMP_KEY=$(umask 077 && mktemp)
  trap 'rm -f "$TMP_KEY"' EXIT
  printf '%s\n' "$TS_AUTHKEY" >"$TMP_KEY"
  KEY_SRC=$TMP_KEY
elif $SUDO test -s "$KEY_FILE"; then
  KEY_SRC=$KEY_FILE
elif [ -r /dev/tty ]; then
  printf 'Tailscale auth key (hidden, Enter to use browser login): ' >/dev/tty
  IFS= read -rs key </dev/tty || key=
  echo >/dev/tty
  if [ -n "$key" ]; then
    $SUDO mkdir -p "$(dirname "$KEY_FILE")"
    $SUDO chmod 700 "$(dirname "$KEY_FILE")"
    # shellcheck disable=SC2016  # $1 expands in the inner sh
    printf '%s\n' "$key" | $SUDO sh -c 'umask 077; cat >"$1"' sh "$KEY_FILE"
    unset key
    echo "saved key to $KEY_FILE (600); later runs reuse it (--reset-key to replace)"
    KEY_SRC=$KEY_FILE
  fi
fi

ARGS=(up)
[ -n "$HOSTNAME_ARG" ] && ARGS+=(--hostname="$HOSTNAME_ARG")
[ "$SSH" = 1 ] && ARGS+=(--ssh)
[ -n "$KEY_SRC" ] && ARGS+=(--auth-key="file:$KEY_SRC")
[ -z "$KEY_SRC" ] && echo "No auth key: open the login URL printed below to approve this machine."
$SUDO "$TS" "${ARGS[@]}"

echo
echo "Tailscale IPv4: $($SUDO "$TS" ip -4 | head -n 1)"
$SUDO "$TS" status
