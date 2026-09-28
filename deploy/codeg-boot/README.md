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

It prefers a local `MyCodeBuddy` checkout (`git fetch` + `deploy/codeg-boot`),
and falls back to public `raw.githubusercontent.com` if needed. It only
overwrites known scripts under `/workspace/codeg-boot` — never secrets or ACP
caches. When scripts change, the watchdog reloads itself via
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
