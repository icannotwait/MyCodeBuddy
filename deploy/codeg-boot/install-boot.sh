#!/bin/bash
# Install tracked boot scripts onto this machine. Never copies secrets.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
DEST=${BOOT_DEST:-/workspace/codeg-boot}
SRC="$ROOT/scripts"

mkdir -p "$DEST" /workspace/heartbeat /workspace/codeg-data /workspace/codeg-dist /workspace/bin

# Only ship known scripts; never rsync whole tree with backups.
for f in ensure-acp-agents.sh codeg-watchdog.sh codeg-supervisor.sh \
         start-codeg-server.sh start-codeg-tunnel.sh start-webdav.sh \
         reload-watchdog-once.sh auto-sync-boot.sh; do
  if [ ! -f "$SRC/$f" ]; then
    echo "missing $SRC/$f" >&2
    exit 1
  fi
  install -m 755 "$SRC/$f" "$DEST/$f"
done

echo "installed boot scripts -> $DEST"
echo "secrets NOT touched. Ensure:"
echo "  - /workspace/codeg-data/CODEG_TOKEN (mode 600; create with openssl rand -hex 16)"
echo "  - cloudflared config at \${CF_CONFIG:-/home/box/.cloudflared/config.yml} (check: start-codeg-tunnel.sh --print-config)"
echo "Optional: restore ACP mirror tarball into /workspace/codeg-data/acp-binaries-mirror"
