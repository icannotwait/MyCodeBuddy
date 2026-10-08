#!/bin/bash
# Store this box's reusable Tailscale auth key locally, then (re)join.
#   set-tailscale-key.sh                      prompt (input hidden)
#   TS_AUTHKEY_INPUT=... set-tailscale-key.sh  from env
#   ... | set-tailscale-key.sh                 from stdin
# The key never leaves this machine: it is written to $TS_DIR/authkey (600)
# and is not printed, logged, synced, or committed.
set -euo pipefail
TS_DIR=${TS_DIR:-/home/box/tailscale}
KEYFILE=$TS_DIR/authkey
BOOT=${BOOT:-/workspace/codeg-boot}
HB=${CODEG_HB:-/workspace/heartbeat}

if [ -n "${TS_AUTHKEY_INPUT:-}" ]; then
  key=$TS_AUTHKEY_INPUT
elif [ -t 0 ]; then
  read -rsp 'Tailscale auth key (hidden): ' key
  echo >&2
else
  IFS= read -r key || true
fi
key=$(printf '%s' "$key" | tr -d '[:space:]')
if [[ ! "$key" =~ ^tskey-[A-Za-z0-9_?=\&.-]+$ ]]; then
  echo "set-tailscale-key: input is not a tskey-... auth key; nothing written" >&2
  exit 1
fi

umask 077
mkdir -p "$TS_DIR"
chmod 700 "$TS_DIR"
tmp=$(mktemp "$TS_DIR/.authkey.XXXXXX")
printf '%s\n' "$key" >"$tmp"
unset key TS_AUTHKEY_INPUT
chmod 600 "$tmp"
mv -f "$tmp" "$KEYFILE"
rm -f "$HB/tailscale-auth.stamp"  # skip the retry backoff for this attempt
echo "set-tailscale-key: saved to $KEYFILE (600)" >&2

"$BOOT/start-tailscale.sh" || true
echo "tailscale=$("$BOOT/start-tailscale.sh" --state)" >&2
