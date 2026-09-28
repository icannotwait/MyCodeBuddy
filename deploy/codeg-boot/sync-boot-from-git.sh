#!/bin/bash
# Update /workspace/codeg-boot from a MyCodeBuddy checkout (deploy/codeg-boot).
# Does not git-pull itself — caller updates the repo first.
set -euo pipefail
REPO=${CODEG_REPO:-/workspace/MyCodeBuddy}
PACK="$REPO/deploy/codeg-boot"
if [ ! -x "$PACK/install-boot.sh" ]; then
  echo "missing $PACK/install-boot.sh — is the repo checked out / branch merged?" >&2
  exit 1
fi
BOOT_DEST=${BOOT_DEST:-/workspace/codeg-boot} "$PACK/install-boot.sh"
