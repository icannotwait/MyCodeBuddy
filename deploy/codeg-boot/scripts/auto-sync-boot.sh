#!/bin/bash
# Sync /workspace/codeg-boot from MyCodeBuddy deploy/codeg-boot.
# Intended to be called from codeg-watchdog's existing poll loop.
# Never touches CODEG_TOKEN, cloudflared creds, or ACP binary caches.
set -uo pipefail

BOOT=${BOOT_DEST:-/workspace/codeg-boot}
HB=${CODEG_HB:-/workspace/heartbeat}
REPO=${CODEG_REPO:-/workspace/MyCodeBuddy}
REMOTE=${CODEG_BOOT_REMOTE:-origin}
BRANCH=${CODEG_BOOT_BRANCH:-main}
REF=${CODEG_BOOT_REF:-origin/main}
GH_OWNER=${CODEG_BOOT_GH_OWNER:-icannotwait}
GH_REPO=${CODEG_BOOT_GH_REPO:-MyCodeBuddy}
GH_REF=${CODEG_BOOT_GH_REF:-main}
# Skip if last successful attempt was within this window (default 1h).
INTERVAL_SECS=${CODEG_BOOT_SYNC_INTERVAL_SECS:-3600}
STAMP=$HB/boot-sync.stamp
LOG=$HB/boot-sync.log
LOCK=$HB/boot-sync.lock
TMP=$HB/boot-sync-staging
NEED_RELOAD=$HB/boot-sync.reload

mkdir -p "$HB" "$BOOT"

ts() { date '+%Y-%m-%d %H:%M:%S %Z'; }
log() { echo "$(ts) $*" >>"$LOG"; }

exec 8>"$LOCK"
if ! flock -n 8; then
  exit 0
fi

now=$(date +%s)
if [ -f "$STAMP" ]; then
  last=$(tr -d ' \n' <"$STAMP" 2>/dev/null || echo 0)
  if [ -n "${last:-}" ] && [ "$last" -gt 0 ] 2>/dev/null; then
    if [ $((now - last)) -lt "$INTERVAL_SECS" ]; then
      exit 0
    fi
  fi
fi

# Files we install into BOOT (basename).
SCRIPTS=(
  ensure-acp-agents.sh
  codeg-watchdog.sh
  codeg-supervisor.sh
  start-codeg-server.sh
  start-codeg-tunnel.sh
  start-webdav.sh
  reload-watchdog-once.sh
  auto-sync-boot.sh
)

stage_clean() {
  rm -rf "$TMP"
  mkdir -p "$TMP"
}

# Read deploy/codeg-boot blobs from the local clone. git show / cat-file only —
# never checkout, so the MyCodeBuddy index and worktree stay untouched.
fetch_via_git() {
  [ -d "$REPO/.git" ] || return 1
  if ! git -C "$REPO" fetch --quiet "$REMOTE" "$BRANCH" 2>>"$LOG"; then
    return 1
  fi
  stage_clean
  local f spec
  for f in "${SCRIPTS[@]}"; do
    spec="$REF:deploy/codeg-boot/scripts/$f"
    if ! git -C "$REPO" cat-file -e "$spec" 2>>"$LOG"; then
      log "git: absent $spec"
      continue
    fi
    if ! git -C "$REPO" show "$spec" >"$TMP/$f.part" 2>>"$LOG"; then
      rm -f "$TMP/$f.part"
      log "git show failed $spec"
      continue
    fi
    mv "$TMP/$f.part" "$TMP/$f"
  done
  [ -s "$TMP/codeg-watchdog.sh" ] && [ -s "$TMP/ensure-acp-agents.sh" ]
}

# Public-repo fallback: raw.githubusercontent.com (no local clone required).
fetch_via_github() {
  stage_clean
  local f url code got=0
  for f in "${SCRIPTS[@]}"; do
    url="https://raw.githubusercontent.com/${GH_OWNER}/${GH_REPO}/${GH_REF}/deploy/codeg-boot/scripts/${f}"
    code=$(curl -sS -L --connect-timeout 8 --max-time 60 \
      -o "$TMP/$f.tmp" -w '%{http_code}' "$url" 2>>"$LOG" || echo 000)
    if [ "$code" = "200" ] && [ -s "$TMP/$f.tmp" ]; then
      mv "$TMP/$f.tmp" "$TMP/$f"
      got=1
    else
      rm -f "$TMP/$f.tmp"
    fi
  done
  [ "$got" = "1" ] && [ -s "$TMP/codeg-watchdog.sh" ] && [ -s "$TMP/ensure-acp-agents.sh" ]
}

apply_staged() {
  local f changed=0
  for f in "${SCRIPTS[@]}"; do
    [ -f "$TMP/$f" ] || continue
    if [ -f "$BOOT/$f" ] && cmp -s "$TMP/$f" "$BOOT/$f"; then
      continue
    fi
    install -m 755 "$TMP/$f" "$BOOT/$f"
    log "updated $f"
    changed=1
  done
  echo "$changed"
}

if fetch_via_git; then
  log "source=git $REPO $REF"
elif fetch_via_github; then
  log "source=github-raw ${GH_OWNER}/${GH_REPO}@${GH_REF}"
else
  log "skip: deploy/codeg-boot not available (PR not merged or network)"
  echo "$now" >"$STAMP"
  rm -rf "$TMP"
  exit 0
fi

changed=$(apply_staged)
echo "$now" >"$STAMP"
rm -rf "$TMP"

if [ "${changed:-0}" = "1" ]; then
  log "boot scripts changed; flag watchdog reload"
  echo 1 >"$NEED_RELOAD"
else
  log "already up to date"
fi
exit 0
