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

`codeg-watchdog.sh` probes the public edge only when `CODEG_PUBLIC_URL` is
explicitly configured. **Unset or empty skips the probe**; a new installation
never probes another host's URL. `env.example` is documentation, not an
automatically loaded environment file.

For the existing drawcode live box, add this export to the persistent launcher
that starts `codeg-supervisor.sh` / `codeg-watchdog.sh` (or its service environment):

```bash
export CODEG_PUBLIC_URL=https://drawcode.20241021.best/
```

After setting it in the current shell, reload the installed watchdog with
`/workspace/codeg-boot/reload-watchdog-once.sh`. The new process inherits the
export; the persistent launcher setting is still needed after a reboot. Other
hosts must use their own public Codeg URL. Installing/syncing scripts does not
change the machine's environment.

A completed **HTTP 530** response, an explicit **Cloudflare Tunnel error / error
1033** in an HTTP 4xx/5xx body, or a failed transfer while the local `:3080` UI is
healthy counts as a public tunnel failure. A normal HTTP 200 page can contain
`1033` without triggering recovery. Curl transport failures are recorded as
`000`, even if some headers/body arrived. Other HTTP responses are logged and
break the consecutive-failure streak without clearing backoff.

After **2 consecutive failures**, the watchdog force-restarts **only**
`cloudflared` via `FORCE_RESTART=1` / `start-codeg-tunnel.sh --force`:

- Every attempt, including a failed launch, consumes the cooldown/budget. Launch
  errors retain the failure streak and log their exit status
- Attempt spacing starts at **180 seconds**, then doubles (360, 720, 1440) up to
  **1800 seconds** while the public edge remains unhealthy
- A successful force-launch command starts **60 seconds** of readiness grace.
  Probes continue; during grace, a failing edge is summarized as `pending`, does
  not count as broken, and does not add to the streak or trigger another restart.
  Detailed logs retain the actual probe failure. Routine start-if-missing calls and watchdog reloads never
  renew grace. Command success alone does not establish a healthy tunnel
- Observing a completed public **HTTP 200** clears the failure streak, grace, and
  exponential escalation. The last-attempt minimum cooldown and daily count remain
- An optional per-**UTC-calendar-day** attempt cap is **disabled by default (0)**.
  With a positive cap, forced public recovery stops at that count until the next
  UTC day; normal start-if-missing recovery remains available

Cooldown, escalation, day/count, and grace survive watchdog reloads in
`/workspace/heartbeat/tunnel-restart.stamp`. Older timestamp-only stamps retain
their minimum cooldown when upgraded. The public failure streak is in memory.
All policy limits apply to public-probe **force restarts**; the existing local
server/WebDAV checks and routine tunnel-start path are unchanged.

Configuration (decimal integers; leading zeroes are accepted):

- `PUBLIC_FAIL_THRESHOLD=2`: range 1–1000
- `TUNNEL_RESTART_COOLDOWN=180`: range 1–86400 seconds
- `TUNNEL_RESTART_BACKOFF_CAP=1800`: range 1–86400 seconds; raised to the cooldown
  if configured below it
- `TUNNEL_RESTART_DAILY_CAP=0`: range 0–1000, where 0 disables the cap
- `TUNNEL_RESTART_GRACE=60`: range 0–3600 seconds, where 0 disables grace

Invalid/out-of-range values produce a warning and use the documented default.
State is written before starting a force restart; if it cannot be persisted, the
restart is skipped and logged rather than bypassing the limit.

Run the offline regression suite with `node --test scripts/codeg-watchdog.test.mjs`.
It exercises the actual shell functions and daemon loop with mocked network and
service commands; it does not launch services or probe external URLs.
