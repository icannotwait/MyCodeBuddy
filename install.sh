#!/usr/bin/env bash
#
# Codeg Server installer
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/xintaofei/codeg/main/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/xintaofei/codeg/main/install.sh | bash -s -- --version v0.5.0
#

set -euo pipefail

REPO="xintaofei/codeg"
INSTALL_DIR="${CODEG_INSTALL_DIR:-/usr/local/bin}"
VERSION=""
# Stale codeg-server / codeg-mcp binaries elsewhere in PATH are removed by
# default so the user's `codeg-server` command always runs the freshly
# installed binary AND the runtime locates the matching companion via the
# exe-sibling lookup. Set CODEG_NO_CLEANUP=1 (or pass --no-cleanup) to
# disable.
CLEANUP_CONFLICTS=1
if [ "${CODEG_NO_CLEANUP:-0}" = "1" ]; then
  CLEANUP_CONFLICTS=0
fi

# Names of binaries this installer manages. `codeg-server` is the user-facing
# entry point; `codeg-mcp` is the stdio MCP companion that the server's ACP
# layer spawns per session for delegation. Both must live in the same
# directory — `locate_codeg_mcp_binary()` in src-tauri/src/acp/connection.rs
# resolves the companion as a sibling of the running server executable.
MANAGED_BINS=(codeg-server codeg-mcp)

# ── Parse arguments ──

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)    VERSION="$2"; shift 2 ;;
    --dir)        INSTALL_DIR="$2"; shift 2 ;;
    --no-cleanup) CLEANUP_CONFLICTS=0; shift ;;
    --help)
      echo "Usage: install.sh [--version VERSION] [--dir INSTALL_DIR] [--no-cleanup]"
      echo ""
      echo "Options:"
      echo "  --version     Version to install (e.g. v0.5.0). Default: latest"
      echo "  --dir         Installation directory. Default: /usr/local/bin"
      echo "  --no-cleanup  Keep stale codeg-server binaries found elsewhere in PATH"
      echo "                (default: remove them so the new install is what runs)"
      echo ""
      echo "Environment:"
      echo "  CODEG_INSTALL_DIR  Same as --dir"
      echo "  CODEG_NO_CLEANUP   Set to 1 to behave like --no-cleanup"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

# ── Detect platform ──

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)  PLATFORM="linux" ;;
  Darwin) PLATFORM="darwin" ;;
  *)      echo "Error: unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH_SUFFIX="x64" ;;
  aarch64|arm64)  ARCH_SUFFIX="arm64" ;;
  *)              echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

ARTIFACT="codeg-server-${PLATFORM}-${ARCH_SUFFIX}"

# ── Resolve version ──

if [ -z "$VERSION" ]; then
  echo "Fetching latest release..."
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)
  if [ -z "$VERSION" ]; then
    echo "Error: could not determine latest version"
    exit 1
  fi
fi

# ── Helpers ──

# Canonicalize a path (resolve symlinks). Falls back to the input if no tool available.
canon_path() {
  local p="$1"
  [ -z "$p" ] && return 0
  if command -v readlink >/dev/null 2>&1 && readlink -f / >/dev/null 2>&1; then
    readlink -f "$p" 2>/dev/null || echo "$p"
  elif command -v realpath >/dev/null 2>&1; then
    realpath "$p" 2>/dev/null || echo "$p"
  else
    echo "$p"
  fi
}

# Read the version of a codeg-server binary (with a 3s timeout for old binaries
# that lack --version support and would otherwise start the full server).
read_bin_version() {
  local bin="$1"
  [ -x "$bin" ] || return 0
  local tmp pid guard
  tmp=$(mktemp)
  "$bin" --version > "$tmp" 2>/dev/null &
  pid=$!
  ( sleep 3 && kill "$pid" 2>/dev/null ) &
  guard=$!
  wait "$pid" 2>/dev/null || true
  kill "$guard" 2>/dev/null || true
  wait "$guard" 2>/dev/null || true
  head -1 "$tmp" 2>/dev/null | tr -d '[:space:]'
  rm -f "$tmp"
}

# ── Scan PATH for codeg-server binaries that shadow the target install ──
#
# A binary "shadows" the install only if it appears in PATH BEFORE the
# destination directory: that's the binary `command -v codeg-server` would
# return after install. Walk PATH and stop at the destination directory —
# anything past it cannot affect resolution today, so we leave it alone.

DEST_BIN="${INSTALL_DIR}/codeg-server"
DEST_BIN_REAL="$(canon_path "$DEST_BIN")"
INSTALL_DIR_REAL="$(canon_path "$INSTALL_DIR")"

# Scan PATH for both managed binaries — a stale `codeg-mcp` in an earlier
# PATH entry would be picked by the runtime's `which` fallback once
# `codeg-server` was upgraded out from under it, breaking delegation in
# subtle ways. Track conflicts uniformly for cleanup.
PATH_CONFLICTS=()
DEST_IN_PATH=0
_SEEN_REAL=":"
IFS=':' read -ra _PATH_DIRS <<< "${PATH:-}"
for _dir in "${_PATH_DIRS[@]}"; do
  [ -z "$_dir" ] && continue
  # Match by canonical path string so the destination is recognized even when
  # the directory doesn't exist yet (e.g. first install into a fresh prefix).
  if [ "$(canon_path "$_dir")" = "$INSTALL_DIR_REAL" ]; then
    DEST_IN_PATH=1
    break
  fi
  for _name in "${MANAGED_BINS[@]}"; do
    _bin="$_dir/$_name"
    if [ -f "$_bin" ] && [ -x "$_bin" ]; then
      _real="$(canon_path "$_bin")"
      case "$_SEEN_REAL" in
        *":$_real:"*) continue ;;
      esac
      _SEEN_REAL="${_SEEN_REAL}${_real}:"
      PATH_CONFLICTS+=("$_bin")
    fi
  done
done

# If the destination directory isn't on PATH, nothing "shadows" the install —
# the new binary just won't be reachable as `codeg-server`. Drop any collected
# entries; the post-install check will tell the user to fix PATH instead.
if [ "$DEST_IN_PATH" -eq 0 ]; then
  PATH_CONFLICTS=()
fi

# What does `codeg-server` actually resolve to right now in PATH?
ACTIVE_BIN=""
if command -v codeg-server >/dev/null 2>&1; then
  ACTIVE_BIN="$(command -v codeg-server)"
fi

# ── Version detection — prefer the binary the user actually invokes ──

VERSION_CHECK_BIN=""
if [ -n "$ACTIVE_BIN" ] && [ -x "$ACTIVE_BIN" ]; then
  VERSION_CHECK_BIN="$ACTIVE_BIN"
elif [ -x "$DEST_BIN" ]; then
  VERSION_CHECK_BIN="$DEST_BIN"
fi

CURRENT_VERSION=""
if [ -n "$VERSION_CHECK_BIN" ]; then
  CURRENT_VERSION="$(read_bin_version "$VERSION_CHECK_BIN")"
fi

# Normalize: strip leading "v" for comparison
TARGET_VER="${VERSION#v}"

# Only short-circuit when the active binary is up to date AND the destination
# itself has it AND no other PATH entries shadow it. Otherwise we still need to
# install / clean up so the user's `codeg-server` command runs the new version.
if [ -n "$CURRENT_VERSION" ] && [ "$CURRENT_VERSION" = "$TARGET_VER" ] \
   && [ "${#PATH_CONFLICTS[@]}" -eq 0 ] \
   && [ -x "$DEST_BIN" ]; then
  echo "codeg-server is already at version ${TARGET_VER}, nothing to do."
  exit 0
fi

if [ -n "$CURRENT_VERSION" ]; then
  echo "Upgrading codeg-server: ${CURRENT_VERSION} -> ${TARGET_VER}..."
else
  echo "Installing codeg-server ${VERSION} (${PLATFORM}/${ARCH_SUFFIX})..."
fi

# ── Warn about codeg-server binaries shadowing the target install ──

if [ "${#PATH_CONFLICTS[@]}" -gt 0 ]; then
  echo ""
  echo "Found other codeg-server binaries in PATH that may shadow ${DEST_BIN}:"
  for _c in "${PATH_CONFLICTS[@]}"; do
    _cv="$(read_bin_version "$_c" 2>/dev/null || true)"
    if [ -n "$_cv" ]; then
      echo "  - $_c  (version ${_cv})"
    else
      echo "  - $_c"
    fi
  done
  if [ "$CLEANUP_CONFLICTS" = "1" ]; then
    echo "These will be removed after installation. Pass --no-cleanup to keep them."
  else
    echo "Keeping them (--no-cleanup). You may need to remove them manually so that"
    echo "typing 'codeg-server' runs the new install at ${DEST_BIN}."
  fi
  echo ""
fi

# ── Stop running service before upgrade ──

RESTARTED_PIDS=""
if pgrep -x codeg-server >/dev/null 2>&1; then
  echo "Stopping running codeg-server process(es)..."
  RESTARTED_PIDS=$(pgrep -x codeg-server || true)
  if kill $RESTARTED_PIDS 2>/dev/null; then
    # Wait up to 10 seconds for graceful shutdown
    for i in $(seq 1 10); do
      if ! pgrep -x codeg-server >/dev/null 2>&1; then
        break
      fi
      sleep 1
    done
    # Force kill if still running
    if pgrep -x codeg-server >/dev/null 2>&1; then
      echo "Force stopping codeg-server..."
      kill -9 $RESTARTED_PIDS 2>/dev/null || true
      sleep 1
    fi
  fi
  echo "codeg-server stopped."
fi

# ── Download and extract ──

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARTIFACT}.tar.gz"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading ${DOWNLOAD_URL}..."
if ! curl -fSL --progress-bar -o "${TMP_DIR}/${ARTIFACT}.tar.gz" "$DOWNLOAD_URL"; then
  echo "Error: download failed. Check that version ${VERSION} exists and has a ${ARTIFACT} asset."
  exit 1
fi

echo "Extracting..."
tar xzf "${TMP_DIR}/${ARTIFACT}.tar.gz" -C "$TMP_DIR"

# ── Install binaries ──
#
# Verify both binaries are present in the archive BEFORE writing anything
# to INSTALL_DIR. Without the companion, delegation degrades silently on
# every new ACP session — fail fast instead.

for _name in "${MANAGED_BINS[@]}"; do
  if [ ! -f "${TMP_DIR}/${ARTIFACT}/${_name}" ]; then
    echo "Error: ${_name} not found in archive ${ARTIFACT}.tar.gz"
    echo "       This release tarball is incomplete; please report it."
    exit 1
  fi
done

mkdir -p "$INSTALL_DIR"
_install_one() {
  local name="$1"
  local src="${TMP_DIR}/${ARTIFACT}/${name}"
  local dst="${INSTALL_DIR}/${name}"
  if [ -w "$INSTALL_DIR" ]; then
    cp "$src" "$dst"
    chmod +x "$dst"
  else
    sudo cp "$src" "$dst"
    sudo chmod +x "$dst"
  fi
}

if [ ! -w "$INSTALL_DIR" ]; then
  echo "Need sudo to install to ${INSTALL_DIR}"
fi
for _name in "${MANAGED_BINS[@]}"; do
  _install_one "$_name"
done

# Re-canonicalize destination now that the file exists. Pre-install canon may
# leave the final non-existent component unresolved (notably macOS readlink -f),
# which would mis-compare against the post-install `command -v` result.
DEST_BIN_REAL="$(canon_path "$DEST_BIN")"

# ── Install web assets ──

WEB_SRC="${TMP_DIR}/${ARTIFACT}/web"
WEB_DIR="${CODEG_WEB_DIR:-/usr/local/share/codeg/web}"

if [ -d "$WEB_SRC" ]; then
  echo "Installing web assets to ${WEB_DIR}..."
  if [ -w "$(dirname "$WEB_DIR")" ] 2>/dev/null; then
    mkdir -p "$WEB_DIR"
    cp -r "$WEB_SRC"/* "$WEB_DIR"/
  else
    sudo mkdir -p "$WEB_DIR"
    sudo cp -r "$WEB_SRC"/* "$WEB_DIR"/
  fi
fi

# ── Remove shadowing binaries from earlier PATH entries ──

EXIT_STATUS=0

if [ "${#PATH_CONFLICTS[@]}" -gt 0 ] && [ "$CLEANUP_CONFLICTS" = "1" ]; then
  echo ""
  echo "Removing stale codeg-server binaries..."
  for _c in "${PATH_CONFLICTS[@]}"; do
    _parent="$(dirname "$_c")"
    _rm_ok=0
    if [ -w "$_parent" ] && { [ ! -e "$_c" ] || [ -w "$_c" ]; }; then
      if rm -f "$_c" 2>/dev/null; then _rm_ok=1; fi
    else
      if sudo rm -f "$_c" 2>/dev/null; then _rm_ok=1; fi
    fi
    if [ "$_rm_ok" -eq 1 ]; then
      echo "  removed $_c"
    else
      echo "  failed to remove $_c (remove it manually so 'codeg-server' resolves to the new install)"
      EXIT_STATUS=1
    fi
  done
fi

# ── Restart service if it was running ──

if [ -n "$RESTARTED_PIDS" ]; then
  echo ""
  echo "Note: codeg-server was stopped for the upgrade."
  echo "Please restart it manually to ensure your environment variables (CODEG_PORT, CODEG_TOKEN, etc.) are preserved:"
  echo "  CODEG_STATIC_DIR=${WEB_DIR} codeg-server"
fi

# ── Done ──

echo ""
echo "codeg-server installed to ${INSTALL_DIR}/codeg-server"
echo "codeg-mcp    installed to ${INSTALL_DIR}/codeg-mcp"
INSTALLED_VER=$("${INSTALL_DIR}/codeg-server" --version 2>/dev/null || echo "${TARGET_VER}")
echo "Version: ${INSTALLED_VER}"

# Final smoke: codeg-mcp must exist next to codeg-server so the runtime's
# `locate_codeg_mcp_binary()` exe-sibling lookup hits. A failure here means
# the tarball was malformed or a previous `_install_one` was silently
# blocked — surface it loudly rather than ship a half-broken install.
if [ ! -x "${INSTALL_DIR}/codeg-mcp" ]; then
  echo ""
  echo "Error: ${INSTALL_DIR}/codeg-mcp missing or not executable after install."
  echo "       Delegation (sub-agent tooling) will not work. Re-run the installer."
  EXIT_STATUS=1
fi

# Verify the user's `codeg-server` command actually resolves to the new binary.
ACTIVE_BIN_AFTER=""
if command -v codeg-server >/dev/null 2>&1; then
  ACTIVE_BIN_AFTER="$(command -v codeg-server)"
fi
ACTIVE_BIN_AFTER_REAL="$(canon_path "$ACTIVE_BIN_AFTER")"

if [ -z "$ACTIVE_BIN_AFTER" ]; then
  echo ""
  echo "Note: ${INSTALL_DIR} is not on your PATH. Add it so 'codeg-server' resolves directly:"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
  EXIT_STATUS=1
elif [ "$ACTIVE_BIN_AFTER_REAL" != "$DEST_BIN_REAL" ]; then
  echo ""
  echo "Warning: typing 'codeg-server' still runs ${ACTIVE_BIN_AFTER}, not ${DEST_BIN}."
  echo "Another binary earlier in PATH is shadowing the new install. To fix, either:"
  echo "  - re-run without --no-cleanup (the default removes shadowing binaries), or"
  echo "  - remove the stale binary manually: rm '${ACTIVE_BIN_AFTER}', or"
  echo "  - put ${INSTALL_DIR} before its directory in PATH."
  EXIT_STATUS=1
else
  # Same path: a previous shell session may have cached the old inode.
  echo ""
  echo "Tip: if you ran codeg-server earlier in this shell, run 'hash -r' (bash/zsh) to clear the path cache."
fi

echo ""
echo "Quick start:"
echo "  CODEG_STATIC_DIR=${WEB_DIR} codeg-server"
echo ""
echo "Or with custom settings:"
echo "  CODEG_PORT=3080 CODEG_TOKEN=your-secret CODEG_STATIC_DIR=${WEB_DIR} codeg-server"
echo ""
echo "The auth token is printed to stderr on startup if not set via CODEG_TOKEN."

exit "$EXIT_STATUS"
