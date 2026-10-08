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
#
# Download failures back off per agent (stamp $HB/ensure-acp-<agent>.fail).
# When the wanted files are already on disk but acp_list_agents still reports
# a different installed_version, binary agents re-register via
# acp_download_agent_binary (cache hit is a no-op). Grok is npx, so that
# endpoint rejects it: refresh with acp_detect_agent_local_version and, if
# the probed version is still wrong, acp_prepare_npx_agent.
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
# The registry_version from the running codeg-server (acp_list_agents) is a
# MINIMUM, not an exact pin: when the installed version is semver >= registry
# (e.g. the user upgraded early to 1.3.0 while the registry pins 1.2.1) the
# agent is kept and nothing is downloaded or "refreshed". Only a missing,
# unparseable, or older install is moved to the registry version.
# Optional exact pins (explicit operator choice, may downgrade):
#   CODEG_ANTIGRAVITY_VER / CODEG_CURSOR_VER / CODEG_GROK_VER
# ENSURE_ACP_PLAN_ONLY=1 (or --plan-only): print the plan and exit without
# restoring, mirroring, downloading, or calling any mutating endpoint.
PLAN_ONLY=${ENSURE_ACP_PLAN_ONLY:-0}
if [ "${1:-}" = "--plan-only" ]; then PLAN_ONLY=1; fi
GROK_PKG_BASE='@xai-official/grok'


mkdir -p "$HB" "$MIRROR" "$CACHE" "$GROK_MIRROR"

exec 7>"$LOCK"
if ! flock -n 7; then
  exit 0
fi

ts() { date '+%Y-%m-%d %H:%M:%S %Z'; }
log() { echo "$(ts) $*" >>"$LOG"; }

# Per-agent download backoff. Stamp line: "<unix_ts> <next_backoff_secs>".
BACKOFF_BASE=${CODEG_ACP_BACKOFF_BASE_SECS:-3600}
BACKOFF_CAP=${CODEG_ACP_BACKOFF_CAP_SECS:-21600}
case $BACKOFF_BASE in
  ''|*[!0-9]*) BACKOFF_BASE=3600 ;;
esac
case $BACKOFF_CAP in
  ''|*[!0-9]*) BACKOFF_CAP=21600 ;;
esac
if [ "$BACKOFF_BASE" -lt 1 ]; then
  BACKOFF_BASE=3600
fi
if [ "$BACKOFF_CAP" -lt 1 ]; then
  BACKOFF_CAP=21600
fi
if [ "$BACKOFF_CAP" -lt "$BACKOFF_BASE" ]; then
  BACKOFF_CAP=$BACKOFF_BASE
fi

acp_fail_stamp() {
  echo "$HB/ensure-acp-$1.fail"
}

# 0 = skip this agent's download; other plan rows still run.
acp_download_blocked() {
  local agent=$1 stamp last next now until
  stamp=$(acp_fail_stamp "$agent")
  [ -f "$stamp" ] || return 1
  read -r last next <"$stamp" || return 1
  case "$last" in
    ''|*[!0-9]*) return 1 ;;
  esac
  case "$next" in
    ''|*[!0-9]*) return 1 ;;
  esac
  now=$(date +%s)
  until=$((last + next))
  if [ "$now" -lt "$until" ]; then
    log "skip $agent download: backoff ${next}s until $(date -d "@$until" '+%Y-%m-%d %H:%M:%S %Z' 2>/dev/null || echo "$until")"
    return 0
  fi
  return 1
}

acp_backoff_fail() {
  local agent=$1 stamp now prev next
  stamp=$(acp_fail_stamp "$agent")
  now=$(date +%s)
  next=$BACKOFF_BASE
  if [ -f "$stamp" ]; then
    read -r _ prev <"$stamp" || prev=$BACKOFF_BASE
    case "$prev" in
      ''|*[!0-9]*) prev=$BACKOFF_BASE ;;
    esac
    next=$((prev * 2))
    if [ "$next" -lt "$BACKOFF_BASE" ]; then
      next=$BACKOFF_BASE
    fi
  fi
  if [ "$next" -gt "$BACKOFF_CAP" ]; then
    next=$BACKOFF_CAP
  fi
  printf '%s %s\n' "$now" "$next" >"$stamp"
  log "backoff $agent: next_backoff_secs=$next"
}

acp_backoff_clear() {
  local agent=$1 stamp
  stamp=$(acp_fail_stamp "$agent")
  if [ -f "$stamp" ]; then
    rm -f "$stamp"
    log "cleared download backoff for $agent"
  fi
}

api_installed_version() {
  local agent=$1
  python3 - "$agent" <<'PY' 2>>"$LOG"
import json, os, sys, urllib.request
agent = sys.argv[1]
token = open(os.environ["CODEG_TOKEN_FILE"]).read().strip()
req = urllib.request.Request(
  "http://127.0.0.1:3080/api/acp_list_agents",
  data=b"{}",
  headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
  method="POST",
)
agents = json.load(urllib.request.urlopen(req, timeout=30))
for a in agents:
  if (a.get("agent_type") or "") == agent:
    inst = (a.get("installed_version") or "").strip()
    sys.stdout.write(inst if inst else "-")
    raise SystemExit(0)
sys.stdout.write("-")
PY
}

api_download_binary() {
  local agent=$1 want=$2 inst=${3:--}
  local tid code
  tid="ensure-$(date +%s)-$agent"
  log "downloading $agent version=$want taskId=$tid (was $inst)"
  code=$(curl -sS -m 900 -o "/tmp/ensure-acp-$agent.body" -w '%{http_code}' \
    -X POST "$API/api/acp_download_agent_binary" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"agentType\":\"$agent\",\"version\":\"$want\",\"taskId\":\"$tid\"}" \
    2>/tmp/ensure-acp-$agent.err || echo "000")
  if [ "$code" = "200" ]; then
    log "download $agent@$want ok"
    return 0
  fi
  log "download $agent@$want failed http=$code body=$(head -c 200 /tmp/ensure-acp-$agent.body 2>/dev/null) err=$(head -c 120 /tmp/ensure-acp-$agent.err 2>/dev/null)"
  return 1
}

# Grok is npx: acp_download_agent_binary refuses it. This probe writes
# installed_version from npm list / the system CLI.
api_detect_version() {
  local agent=$1 code
  code=$(curl -sS -m 120 -o "/tmp/ensure-acp-$agent.detect" -w '%{http_code}' \
    -X POST "$API/api/acp_detect_agent_local_version" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"agentType\":\"$agent\"}" 2>>"$LOG" || echo "000")
  if [ "$code" != "200" ]; then
    log "detect $agent failed http=$code body=$(head -c 200 /tmp/ensure-acp-$agent.detect 2>/dev/null)"
    return 1
  fi
  python3 -c 'import json,sys
try:
    v=json.load(open(sys.argv[1]))
except Exception:
    v=None
sys.stdout.write(v if isinstance(v, str) else "")' "/tmp/ensure-acp-$agent.detect"
}

api_prepare_npx() {
  local agent=$1 want=$2 inst=${3:--}
  local tid code
  tid="ensure-$(date +%s)-$agent"
  log "preparing npx $agent version=$want taskId=$tid (was $inst)"
  code=$(curl -sS -m 900 -o "/tmp/ensure-acp-$agent.body" -w '%{http_code}' \
    -X POST "$API/api/acp_prepare_npx_agent" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"agentType\":\"$agent\",\"registryVersion\":\"$want\",\"version\":\"$want\",\"cleanFirst\":false,\"taskId\":\"$tid\"}" \
    2>/tmp/ensure-acp-$agent.err || echo "000")
  if [ "$code" = "200" ]; then
    log "prepare $agent@$want ok body=$(head -c 80 /tmp/ensure-acp-$agent.body 2>/dev/null)"
    return 0
  fi
  log "prepare $agent@$want failed http=$code body=$(head -c 200 /tmp/ensure-acp-$agent.body 2>/dev/null) err=$(head -c 120 /tmp/ensure-acp-$agent.err 2>/dev/null)"
  return 1
}

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


# stdout: acp_list_agents JSON from the live server (token read from file).
fetch_agents_json() {
  python3 /dev/fd/3 3<<'PY'
import json, os, sys, urllib.request
token = open(os.environ["CODEG_TOKEN_FILE"]).read().strip()
req = urllib.request.Request(
  "http://127.0.0.1:3080/api/acp_list_agents",
  data=b"{}",
  headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
  method="POST",
)
try:
  json.dump(json.load(urllib.request.urlopen(req, timeout=30)), sys.stdout)
except Exception as e:
  sys.stderr.write(str(e))
  sys.exit(1)
PY
}

# stdin: acp_list_agents JSON. stdout, one row per managed enabled agent:
#   plan <dist> <agent> <want> <installed_or_->   (needs install/upgrade)
#   keep <dist> <agent> <installed> <registry> <reason>
# Registry is a minimum; installed >= registry (SemVer 2.0 precedence, build
# metadata ignored, prerelease < release) is kept. CODEG_<AGENT>_VER is exact.
registry_plan() {
  python3 /dev/fd/3 3<<'PY'
import json, os, re, sys

MANAGED = ("antigravity", "cursor", "grok")
OVERRIDE = {a: os.environ.get("CODEG_%s_VER" % a.upper(), "").strip() for a in MANAGED}
VERSION = re.compile(r"^(\d+(?:\.\d+)*)(?:-([0-9A-Za-z.-]+))?$")

def parse(v):
  v = v.strip()
  if v[:1] in ("v", "V"):
    v = v[1:]
  m = VERSION.match(v.split("+", 1)[0])
  if not m:
    return None
  core = [int(x) for x in m.group(1).split(".")]
  return core, (m.group(2).split(".") if m.group(2) else None)

def cmp_pre(a, b):
  for x, y in zip(a, b):
    if x.isdigit() and y.isdigit():
      c = (int(x) > int(y)) - (int(x) < int(y))
    elif x.isdigit() or y.isdigit():
      c = -1 if x.isdigit() else 1
    else:
      c = (x > y) - (x < y)
    if c:
      return c
  return (len(a) > len(b)) - (len(a) < len(b))

def vcmp(a, b):
  pa, pb = parse(a), parse(b)
  if pa is None or pb is None:
    return None
  ca, cb = pa[0], pb[0]
  n = max(len(ca), len(cb))
  ca, cb = ca + [0] * (n - len(ca)), cb + [0] * (n - len(cb))
  if ca != cb:
    return (ca > cb) - (ca < cb)
  if pa[1] == pb[1]:
    return 0
  if pa[1] is None or pb[1] is None:
    return 1 if pa[1] is None else -1
  return cmp_pre(pa[1], pb[1])

for a in json.load(sys.stdin):
  at = a.get("agent_type") or ""
  dist = a.get("distribution_type") or ""
  if not a.get("enabled") or dist not in ("binary", "npx") or at not in MANAGED:
    continue
  reg = (a.get("registry_version") or "").strip()
  inst = (a.get("installed_version") or "").strip() or "-"
  pin = OVERRIDE.get(at)
  if pin:
    if inst != pin:
      print(f"plan {dist} {at} {pin} {inst}")
    continue
  if not reg:
    continue
  if inst == "-":
    print(f"plan {dist} {at} {reg} {inst}")
    continue
  c = vcmp(inst, reg)
  if c is None or c < 0:
    print(f"plan {dist} {at} {reg} {inst}")
  else:
    print(f"keep {dist} {at} {inst} {reg} {'equal' if c == 0 else 'newer'}")
PY
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

if [ "$PLAN_ONLY" != "1" ]; then
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
  if [ -x "$GROK_BIN_DIR/grok" ]; then
    :
  elif ls "$GROK_BIN_DIR"/grok-* >/dev/null 2>&1; then
    latest=$(ls -1 "$GROK_BIN_DIR"/grok-* | sort -V | tail -n1)
    ln -sfn "$(basename "$latest")" "$GROK_BIN_DIR/grok"
  fi
fi

# --- 2) Mirror current cache back ---
sync_dir "$CACHE" "$MIRROR"
if [ -d "$GROK_BIN_DIR" ] && [ "$(ls -A "$GROK_BIN_DIR" 2>/dev/null)" ]; then
  sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
fi
fi # PLAN_ONLY

# --- 3) Downloads if server up: registry_version is a minimum (see header) ---
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

ROWS=
if AGENTS_JSON=$(fetch_agents_json 2>>"$LOG"); then
  ROWS=$(printf '%s' "$AGENTS_JSON" | registry_plan 2>>"$LOG" || true)
else
  log "registry: acp_list_agents failed; no plan this cycle"
fi
PLAN=$(printf '%s\n' "$ROWS" | sed -n 's/^plan //p')
KEEP=$(printf '%s\n' "$ROWS" | sed -n 's/^keep //p')

NEWER=$(printf '%s\n' "$KEEP" | grep ' newer$' || true)
if [ -n "${NEWER:-}" ]; then
  log "registry: keep user-installed newer than registry minimum: $(echo "$NEWER" | tr '\n' ';')"
fi
if [ -z "${PLAN:-}" ]; then
  log "registry: nothing to update (enabled agents at or above registry_version)"
else
  log "registry plan: $(echo "$PLAN" | tr '\n' ';')"
fi
if [ "$PLAN_ONLY" = "1" ]; then
  printf '%s\n' "$ROWS" | sed '/^$/d'
  log "plan-only: exiting before downloads"
  exit 0
fi

while IFS= read -r line; do
  [ -n "$line" ] || continue
  # shellcheck disable=SC2086
  set -- $line
  dist=$1 agent=$2 want=$3 inst=${4:--}

  case "$agent" in
    antigravity)
      ANTIGRAVITY_VER=$want
      ANTIGRAVITY_ZIP_URL="https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-${ANTIGRAVITY_VER}-linux-x86_64.zip"
      if acp_download_blocked "$agent"; then
        continue
      fi
      if antigravity_installed "$ANTIGRAVITY_VER"; then
        log "antigravity $ANTIGRAVITY_VER already on disk but api installed=$inst != $want; refreshing via POST /api/acp_download_agent_binary"
        if api_download_binary "$agent" "$want" "$inst"; then
          acp_backoff_clear "$agent"
        else
          acp_backoff_fail "$agent"
        fi
        continue
      fi
      if install_antigravity_direct; then
        fresh=$(api_installed_version antigravity | tr -d '[:space:]' || true)
        [ -n "${fresh:-}" ] || fresh="?"
        if [ "$fresh" = "$want" ]; then
          log "antigravity $want direct install matches api installed=$fresh"
          acp_backoff_clear "$agent"
          continue
        fi
        log "antigravity $want on disk after direct install but api installed=$fresh != $want; falling through to API download"
      else
        acp_backoff_fail "$agent"
        log "antigravity direct install failed; falling back to API download version=$want"
      fi
      if api_download_binary "$agent" "$want" "$inst"; then
        acp_backoff_clear "$agent"
      else
        acp_backoff_fail "$agent"
      fi
      ;;
    cursor)
      if acp_download_blocked "$agent"; then
        continue
      fi
      if [ -d "$CACHE/cursor/$want" ] && [ -n "$(ls -A "$CACHE/cursor/$want" 2>/dev/null)" ]; then
        log "cursor $want already on disk but api installed=$inst != $want; refreshing via POST /api/acp_download_agent_binary"
      fi
      if api_download_binary "$agent" "$want" "$inst"; then
        acp_backoff_clear "$agent"
      else
        acp_backoff_fail "$agent"
      fi
      ;;
    grok)
      if acp_download_blocked "$agent"; then
        continue
      fi
      GROK_VER=$want
      GROK_PKG="${GROK_PKG_BASE}@${GROK_VER}"
      grok_on_disk=0
      if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ] \
        || "$GROK_BIN_DIR/grok" --version 2>/dev/null | grep -q "$GROK_VER"; then
        grok_on_disk=1
      fi
      if [ "$grok_on_disk" != "1" ] && command -v npm >/dev/null 2>&1; then
        log "installing $GROK_PKG via npm (installed=$inst registry=$want)"
        if ! npm install -g "$GROK_PKG" >>"$LOG" 2>&1; then
          log "npm install grok failed"
          acp_backoff_fail "$agent"
        fi
      fi
      if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ]; then
        ln -sfn "grok-$GROK_VER" "$GROK_BIN_DIR/grok"
        mkdir -p "$GROK_MIRROR"
        sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
        log "mirrored grok bins -> $GROK_MIRROR"
        grok_on_disk=1
      elif command -v grok >/dev/null 2>&1; then
        grok --version >>"$LOG" 2>&1 || true
        if [ -x "$GROK_BIN_DIR/grok-$GROK_VER" ]; then
          ln -sfn "grok-$GROK_VER" "$GROK_BIN_DIR/grok"
          sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
          grok_on_disk=1
        fi
      fi
      # Download endpoint is binary-only. Probe first; prepare if the DB
      # installed_version is still not the wanted build.
      if [ "$grok_on_disk" = "1" ]; then
        log "grok $want already on disk but api installed=$inst != $want; refreshing via POST /api/acp_detect_agent_local_version"
        detected=$(api_detect_version grok | tr -d '[:space:]' || true)
        if [ "${detected:-}" = "$want" ]; then
          log "grok installed_version refreshed to $want"
          acp_backoff_clear "$agent"
          continue
        fi
        log "grok detect returned ${detected:-none} != $want; falling through to POST /api/acp_prepare_npx_agent"
      fi
      if api_prepare_npx "$agent" "$want" "$inst"; then
        acp_backoff_clear "$agent"
      else
        acp_backoff_fail "$agent"
      fi
      ;;
  esac
done <<EOF
$PLAN
EOF

# --- 4) Keep grok symlink/mirror warm even when already matching registry ---
if [ -d "$GROK_BIN_DIR" ] && [ "$(ls -A "$GROK_BIN_DIR" 2>/dev/null)" ]; then
  sync_dir "$GROK_BIN_DIR" "$GROK_MIRROR"
fi

# Final mirror of agent cache
sync_dir "$CACHE" "$MIRROR"
log "done cache=$(ls -1 "$CACHE" 2>/dev/null | tr '\n' ',' ) grok=$(ls -1 "$GROK_BIN_DIR" 2>/dev/null | tr '\n' ',' )"
