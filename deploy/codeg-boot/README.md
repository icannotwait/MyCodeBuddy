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

After you `git pull`, refresh an already-installed machine (this script does
not pull by itself; default `CODEG_REPO` is `/workspace/MyCodeBuddy`):

```bash
CODEG_REPO=/path/to/this/clone ./deploy/codeg-boot/sync-boot-from-git.sh
```

## New-machine cost order (cheapest first)

1. **Copy `codeg-dist/`** from a healthy box, or unpack a private/release
   artifact (`codeg-server`, `codeg-mcp`, and `web/`) into
   `/workspace/codeg-dist`. This skips a full Rust/Node rebuild.
2. **Install these boot scripts from git:**
   `./deploy/codeg-boot/install-boot.sh`
3. **Optional ACP restore** from a private `acp-binaries-mirror` tarball
   (not in git). Extract it into
   `/workspace/codeg-data/acp-binaries-mirror`, then run
   `ensure-acp-agents.sh`. It fills the live cache and downloads only what is
   still missing. Prefer the mirror when `dl.google.com` is flaky
   (fake-ip / egress).
4. **Create local-only secrets** from the examples here. Never commit them.
   - Token file (mode 600):

     ```bash
     openssl rand -hex 16 > /workspace/codeg-data/CODEG_TOKEN
     chmod 600 /workspace/codeg-data/CODEG_TOKEN
     ```
   - Tunnel: copy `cloudflared.config.example.yml` to
     `${CF_CONFIG:-$HOME/.cloudflared/config.yml}`, replace `<TUNNEL_UUID>`
     and `<PUBLIC_HOSTNAME>`, and keep the credential JSON next to it on the
     machine only.
   - Optional overrides: start from `env.example` and export only what differs.
5. **Last resort:** build from source (`pnpm install && pnpm build`, then
   `cargo build --release` for `codeg-server` and `codeg-mcp`).

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
