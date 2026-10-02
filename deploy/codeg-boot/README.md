# drawcode / codeg box boot scripts

Grok-Bot-oriented ops scripts for a Linux drawcode host
(`codeg-server` + Cloudflare tunnel + watchdog + ACP cache restore).

These scripts are **small text only**. They intentionally do **not** ship:

- `CODEG_TOKEN`
- Cloudflare `cert.pem` / tunnel credential JSON
- ACP binary caches (hundreds of MB–GB)
- `codeg-dist` build artifacts

## Install onto a machine

From a clone of this repo:

```bash
./deploy/codeg-boot/install-boot.sh
# or: BOOT_DEST=/workspace/codeg-boot ./deploy/codeg-boot/install-boot.sh
```

This copies scripts into `/workspace/codeg-boot` (default) without touching
secrets or binary caches.

## New-machine cost order (cheapest first)

1. **Copy `codeg-dist/` from a healthy box** (or a release tarball you keep
   privately) — avoids a full Rust/Node rebuild.
2. **Install these boot scripts** from git (`install-boot.sh`).
3. **Restore ACP offline** if you have a private `acp-binaries-mirror` tarball:
   extract into `/workspace/codeg-data/acp-binaries-mirror`, then run
   `ensure-acp-agents.sh` (restores live cache; downloads only what is still
   missing). Prefer this over fresh Google downloads when `dl.google.com` is
   flaky (fake-ip / egress).
4. Create local-only secrets (`CODEG_TOKEN`, cloudflared creds) using the
   examples in this folder — never commit them.
5. Last resort: build from source (`pnpm` + `cargo`).

## Layout expected by scripts

```
/workspace/codeg-boot/     # this pack after install
/workspace/codeg-dist/     # codeg-server, codeg-mcp, web/
/workspace/codeg-data/     # CODEG_TOKEN, db, acp-binaries(+mirror)
/workspace/heartbeat/      # logs, pid, locks
/workspace/bin/cloudflared
```

See also the handoff doc pattern under your private notes
(`codeg-clean-machine-deploy`) for full tunnel steps with placeholders.

## Automatic updates (watchdog)

`codeg-watchdog.sh` calls `auto-sync-boot.sh` on the same ~10 minute cadence as
`ensure-acp-agents.sh`. The sync script itself rate-limits to once per hour
(`CODEG_BOOT_SYNC_INTERVAL_SECS`, default `3600`).

It prefers a local `MyCodeBuddy` checkout. After `git fetch` it copies each
known script with `git cat-file -e` and
`git show <ref>:deploy/codeg-boot/scripts/<file>` into a staging dir. That
reads blobs only, so the checkout's index and worktree stay clean. If git
cannot supply the scripts it falls back to public `raw.githubusercontent.com`.
It only overwrites known scripts under `/workspace/codeg-boot` — never secrets
or ACP caches. When scripts change, the watchdog reloads itself via
`reload-watchdog-once.sh`.

Disable by removing execute bit: `chmod a-x /workspace/codeg-boot/auto-sync-boot.sh`.

## ACP versions

`ensure-acp-agents.sh` does **not** hardcode agent versions. It asks the live
`codeg-server` (`POST /api/acp_list_agents`) for each enabled agent's
`registry_version` (from the MyCodeBuddy registry baked into that build) and
only downloads/upgrades when `installed_version` differs.

So after you deploy a new `codeg-dist` that bumps Cursor / Antigravity / Grok
in registry, the next watchdog `ensure-acp` cycle pulls those versions.
Optional overrides: `CODEG_ANTIGRAVITY_VER`, `CODEG_CURSOR_VER`, `CODEG_GROK_VER`.

A failed direct download or failed API download backs off that agent only.
The stamp `/workspace/heartbeat/ensure-acp-<agent>.fail` stores the failure
time and the next wait (start `3600` seconds, double each failure, cap
`21600`). While the wait is active the script skips that agent's download
and continues with the others. Success deletes the stamp. Overrides:
`CODEG_ACP_BACKOFF_BASE_SECS`, `CODEG_ACP_BACKOFF_CAP_SECS`.

If the wanted version is already on disk but `installed_version` still
differs, the script does not stop at "already on disk". Antigravity and
Cursor call `POST /api/acp_download_agent_binary` for that version (a complete
server cache hit does not re-download). After a direct Antigravity unzip, it
re-reads `acp_list_agents` and uses the same API download when the server
still disagrees. Grok is an npx agent, so the binary download endpoint
rejects it; the script calls `POST /api/acp_detect_agent_local_version` and,
if the probed version is still wrong, `POST /api/acp_prepare_npx_agent`.
## Public tunnel probe

`codeg-watchdog.sh` optionally probes `CODEG_PUBLIC_URL` (default on this live
host: `https://drawcode.20241021.best/`). If unset, the default applies; if set
to empty, the probe is skipped.

When the public edge returns **530**, the body mentions Cloudflare error
**1033**, or the request fails to connect while local `:3080` is healthy, the
watchdog treats the tunnel as a zombie: after **2 consecutive** failures and a
**~3 minute** cooldown, it force-restarts **only** `cloudflared` via
`FORCE_RESTART=1` / `start-codeg-tunnel.sh --force`. It never restarts
`codeg-server` for this path.
