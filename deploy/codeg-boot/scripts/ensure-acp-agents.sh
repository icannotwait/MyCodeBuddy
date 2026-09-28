#!/bin/bash
# Restore ACP binary cache after Update wipe, then re-download if still missing.
#
# Since Codeg 0.31.0 the live cache is durable under CODEG_DATA_DIR
# (NOT ~/.cache). See src-tauri/src/acp/binary_cache.rs::cache_dir.
#
# Live cache:     /workspace/codeg-data/acp-binaries
# Durable mirror: /workspace/codeg-data/acp-binaries-mirror  (extra backup)
# Legacy caches (migrated once if present):
#   ~/.cache/app.mycodebuddy/acp-binaries
#   ~/.cache/app.codeg/acp-binaries
#
# Grok is npx (@xai-official/grok); its native binary lands in ~/.grok/bin.
# We also mirror those binaries under acp-binaries-mirror/grok-bin/.
set -uo pipefail
export PATH=/workspace/bin:/exec-daemon:$HOME/.local/bin:$PATH

HB=/workspace/heartbeat
DATA=/workspace/codeg-data
# Match server: CODEG_DATA_DIR/acp-binaries
CACHE="${CODEG_DATA_DIR:-$DATA}/acp-binaries"
MIRROR="$DATA/acp-binaries-mirror"
LEGACY_MY="$HOME/.cache/app.mycodebuddy/acp-binaries"
LEGACY_CODEG="$HOME/.cache/app.codeg/acp-binaries"
GROK_BIN_DIR="$HOME/.grok/bin"
GROK_MIRROR="$MIRROR/grok-bin"
LOG=$HB/ensure-acp-agents.log
LOCK=$HB/ensure-acp-agents.lock
TOKEN_FILE=$DATA/CODEG_TOKEN
API=http://127.0.0.1:3080
# Registry pin for Grok after 0.31.0 sync
GROK_PKG='@xai-official/grok@1.0.34'
GROK_VER='1.0.34'
# Antigravity binary pin (dl.google.com; system DNS may fake-ip this host)
ANTIGRAVITY_VER='1.2.1'
ANTIGRAVITY_ZIP_URL="https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-${ANTIGRAVITY_VER}-linux-x86_64.zip"
CURSOR_VER='2026.09.15-d2fe57e'

mkdir -p "$HB" "$MIRROR" "$CACHE" "$GROK_MIRROR"

exec 7>"$LOCK"
if ! flock -n 7; then
  exit 0
fi

ts() { date '+%Y-%m-%d %H:%M:%S %Z'; }
log() { echo "$(ts) $*" >>"$LOG"; }

# True when antigravity version dir has entry + required sibling.
antigravity_installed() {
  local ver=${1:-$ANTIGRAVITY_VER}
  local dir="$CACHE/antigravity-acp/$ver/linux-x86_64"
  [ -s "$dir/agy_acp_server.par" ] && [ -s "$dir/localharness_external" ]
}

# Resolve A records via HTTPS DoH (UDP DNS on this box is fake-ip hijacked).
doh_a_records() {
  local name=$1
  python3 - "$name" <<'PY'
import json, sys, urllib.request
name = sys.argv[1]
urls = [
  f"https://cloudflare-dns.com/dns-query?name={name}&type=A",
  f"https://dns.google/resolve?name={name}&type=A",
]
headers_list = [
  {"Accept": "application/dns-json"},
  {},
]
for url, headers in zip(urls, headers_list):
  try:
    req = urllib.request.Request(url, headers=headers)
    data = json.load(urllib.request.urlopen(req, timeout=8))
    ips = [a["data"] for a in data.get("Answer", []) if a.get("type") == 1]
    if ips:
      print("\n".join(ips))
      sys.exit(0)
  except Exception:
    pass
sys.exit(1)
PY
}

# Download URL with optional --resolve bypass for dl.google.com fake-ip hang.
curl_maybe_resolve() {
  local url=$1 out=$2
  local host
  host=$(python3 -c 'import sys,urllib.parse as u; print(u.urlparse(sys.argv[1]).hostname or "")' "$url")
  local args=(-fsSL --connect-timeout 20 --max-time 900 -o "$out")
  if [ "$host" = "dl.google.com" ]; then
    local ip
    ip=$(doh_a_records dl.google.com | head -n1 || true)
    if [ -n "$ip" ]; then
      args+=(--resolve "dl.google.com:443:$ip")
      log "curl $url via DoH resolve $ip"
    else
      log "DoH resolve failed for dl.google.com; trying system DNS"
    fi
  fi
  curl "${args[@]}" "$url"
}

# Install antigravity from Google zip straight into the durable cache layout.
install_antigravity_direct() {
  local ver=$ANTIGRAVITY_VER
  local dest="$CACHE/antigravity-acp/$ver/linux-x86_64"
  local zip=/tmp/agy-acp-server-${ver}-linux-x86_64.zip
  local url=$ANTIGRAVITY_ZIP_URL

  if antigravity_installed "$ver"; then
    log "antigravity $ver already complete in cache"
    return 0
  fi

  log "direct-download antigravity $ver"
  rm -rf "$CACHE/antigravity-acp/$ver"
  mkdir -p "$dest"
  if ! curl_maybe_resolve "$url" "$zip"; then
    log "direct-download antigravity failed to fetch zip"
    rm -rf "$CACHE/antigravity-acp/$ver"
    return 1
  fi
  if ! unzip -o "$zip" -d "$dest" >/dev/null; then
    log "direct-download antigravity unzip failed"
    rm -rf "$CACHE/antigravity-acp/$ver" "$zip"
    return 1
  fi
  chmod u+rwx "$dest/agy_acp_server.par" "$dest/localharness_external" 2>/dev/null || true
  if ! antigravity_installed "$ver"; then
    log "direct-download antigravity incomplete after unzip"
    rm -rf "$CACHE/antigravity-acp/$ver"
    return 1
  fi
  sync_dir "$CACHE/antigravity-acp/$ver" "$MIRROR/antigravity-acp/$ver"
  log "direct-download antigravity $ver ok -> $dest"
  rm -f "$zip"
  return 0
}


port_up() {
  if command -v ss >/dev/null 2>&1; then
    ss -ltn 2>/dev/null | grep -q ':3080'
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/3080) >/dev/null 2>&1
}

sync_dir() {
  local src=$1 dst=$2
  [ -d "$src" ] || return 0
  [ "$(ls -A "$src" 2>/dev/null)" ] || return 0
  mkdir -p "$dst"
  if command -v rsync >/dev/null 2>&1; then
    rsync -a "$src"/ "$dst"/
  else
    cp -a "$src"/. "$dst"/
  fi
}

# --- 0) One-shot migrate legacy XDG caches into durable data dir ---
for legacy in "$LEGACY_MY" "$LEGACY_CODEG"; do
  if [ -d "$legacy" ] && [ "$(ls -A "$legacy" 2>/dev/null)" ]; then
    log "migrating legacy $legacy -> $CACHE"
    sync_dir "$legacy" "$CACHE" || true
  fi
done

# --- 1) Offline restore from durable mirror ---
if [ -d "$MIRROR" ] && [ "$(ls -A "$MIRROR" 2>/dev/null)" ]; then
  need_restore=0
  if [ ! -d "$CACHE" ] || [ -z "$(ls -A "$CACHE" 2>/dev/null)" ]; then
    need_restore=1
  else
    for d in "$MIRROR"/*; do
      [ -d "$d" ] || continue
      name=$(basename "$d")
      [ "$name" = "grok-bin" ] && continue
      if [ ! -d "$CACHE/$name" ]; then
        need_restore=1
        break
      fi
    done
  fi
  if [ "$need_restore" = "1" ]; then
    # restore agent trees only (skip grok-bin)
    for d in "$MIRROR"/*; do
      [ -d "$d" ] || continue
      name=$(basename "$d")
      [ "$name" = "grok-bin" ] && continue
      sync_dir "$d" "$CACHE/$name" || true
    done
    log "restored agent trees from mirror into $CACHE"
  fi
fi

# Restore grok native bins from mirror if missing
if [ -d "$GROK_MIRROR" ] && [ "$(ls -A "$GROK_MIRROR" 2>/dev/null)" ]; then
  mkdir -p "$GROK_BIN_DIR"
  sync_dir "$GROK_MIRROR" "$GROK_BIN_DIR" || true
  if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ]; then
    ln -sfn "grok-$GROK_VER" "$GROK_BIN_DIR/grok"
  fi
fi

# --- 2) Mirror current cache back ---
sync_dir "$CACHE" "$MIRROR"
if [ -d "$GROK_BIN_DIR" ] && [ "$(ls -A "$GROK_BIN_DIR" 2>/dev/null)" ]; then
  sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
fi

# --- 3) Downloads if server up ---
if ! port_up; then
  log "skip download: 3080 down"
  exit 0
fi
if [ ! -f "$TOKEN_FILE" ]; then
  log "skip download: no CODEG_TOKEN"
  exit 0
fi
TOKEN=$(tr -d '\n\r' < "$TOKEN_FILE")
if [ -z "$TOKEN" ]; then
  log "skip download: empty token"
  exit 0
fi
export CODEG_TOKEN_FILE="$TOKEN_FILE"

agents_to_fix=""
if command -v python3 >/dev/null 2>&1; then
  agents_to_fix=$(python3 - <<'PY' 2>/dev/null || true
import json, urllib.request, sys, os
token=open(os.environ["CODEG_TOKEN_FILE"]).read().strip()
req=urllib.request.Request(
  "http://127.0.0.1:3080/api/acp_list_agents",
  data=b"{}",
  headers={"Authorization":"Bearer "+token,"Content-Type":"application/json"},
  method="POST",
)
try:
  agents=json.load(urllib.request.urlopen(req, timeout=30))
except Exception as e:
  sys.stderr.write(str(e))
  sys.exit(1)
need=[]
for a in agents:
  if not a.get("enabled"):
    continue
  if a.get("distribution_type") != "binary":
    continue
  if a.get("installed_version"):
    continue
  need.append(a["agent_type"])
print(" ".join(need))
PY
) || agents_to_fix="antigravity cursor"
fi
[ -z "$agents_to_fix" ] && agents_to_fix=""

# Always ensure pinned binary agents exist as COMPLETE installs (empty version dirs do not count)
for a in antigravity cursor; do
  case "$a" in
    antigravity)
      if antigravity_installed "$ANTIGRAVITY_VER"; then
        continue
      fi
      ;;
    cursor)
      folder=cursor; want=$CURSOR_VER
      # cursor ships a single binary tree; require non-empty version dir
      if [ -d "$CACHE/$folder/$want" ] && [ -n "$(ls -A "$CACHE/$folder/$want" 2>/dev/null)" ]; then
        continue
      fi
      ;;
    *) continue ;;
  esac
  case " $agents_to_fix " in
    *" $a "*) ;;
    *) agents_to_fix="$agents_to_fix $a" ;;
  esac
done

agents_to_fix=$(echo "$agents_to_fix" | tr ' ' '\n' | awk 'NF' | sort -u | tr '\n' ' ')

for agent in $agents_to_fix; do
  if [ "$agent" = "antigravity" ]; then
    # Prefer direct DoH/--resolve path: codeg's own downloader uses system DNS,
    # which fake-ips dl.google.com and hangs on this box.
    if install_antigravity_direct; then
      continue
    fi
    log "antigravity direct install failed; falling back to API download"
  fi
  tid="ensure-$(date +%s)-$agent"
  log "downloading $agent taskId=$tid"
  code=$(curl -sS -m 900 -o /tmp/ensure-acp-$agent.body -w '%{http_code}' -X POST "$API/api/acp_download_agent_binary" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"agentType\":\"$agent\",\"taskId\":\"$tid\"}" 2>/tmp/ensure-acp-$agent.err || echo "000")
  if [ "$code" = "200" ]; then
    log "download $agent ok"
  else
    log "download $agent failed http=$code body=$(head -c 200 /tmp/ensure-acp-$agent.body 2>/dev/null) err=$(head -c 120 /tmp/ensure-acp-$agent.err 2>/dev/null)"
  fi
done

# --- 4) Grok npx pin + durable binary mirror ---
if command -v npm >/dev/null 2>&1; then
  if ! "$GROK_BIN_DIR/grok" --version 2>/dev/null | grep -q "$GROK_VER"; then
    log "installing $GROK_PKG via npm"
    npm install -g "$GROK_PKG" >>"$LOG" 2>&1 || log "npm install grok failed"
  fi
fi
# Ensure symlink + mirror
if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ]; then
  ln -sfn "grok-$GROK_VER" "$GROK_BIN_DIR/grok"
  mkdir -p "$GROK_MIRROR"
  sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
  log "mirrored grok bins -> $GROK_MIRROR"
elif command -v grok >/dev/null 2>&1; then
  # Trigger decompress by running once
  grok --version >>"$LOG" 2>&1 || true
  if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ]; then
    ln -sfn "grok-$GROK_VER" "$GROK_BIN_DIR/grok"
    sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
    log "mirrored grok bins after launch -> $GROK_MIRROR"
  fi
fi

# Final mirror of agent cache
sync_dir "$CACHE" "$MIRROR"
log "done cache=$(ls -1 "$CACHE" 2>/dev/null | tr '\n' ',' ) grok=$(ls -1 "$GROK_BIN_DIR" 2>/dev/null | tr '\n' ',' )"
