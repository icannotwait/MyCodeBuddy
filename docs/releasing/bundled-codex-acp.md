# Codex ACP (official npm)

MyCodeBuddy launches Codex ACP from the **official npm package** on every
platform:

```text
@agentclientprotocol/codex-acp@1.1.7
```

There is **no** bundled Windows `codex-acp.exe` sidecar and **no** packaged
`codex-acp-seed` in the desktop installer or server ZIP. Agent Settings
installs/uninstalls via `npm install -g` / `npm uninstall -g`, same as other
npx-distributed agents.

## Runtime dependency: host Codex (app-server default)

The adapter embeds `@openai/codex` for app-server. Product **defaults** to
app-server mode via distribution env `CODEX_ACP_USE_CLI=0` on **all platforms**.
Agent Settings exposes a toggle; turning it on writes `CODEX_ACP_USE_CLI=1`
(user env wins over the distribution pin) so the adapter runs turns with host
`codex exec --json` instead.

Resolution order for `CODEX_PATH` (only required when CLI mode is on):

1. Explicit `CODEX_PATH` in the process or agent environment
2. `codex` / `codex.cmd` on `PATH`
3. npm global prefix (`%APPDATA%\npm`) `codex.cmd` or
   `node_modules/@openai/codex/bin/codex.js`

Users need a working `codex-acp` on PATH (Agent Settings → Install, or
`npm install -g @agentclientprotocol/codex-acp@1.1.7`). CLI mode additionally
needs a host Codex CLI (e.g. `npm install -g @openai/codex`) unless
`CODEX_PATH` is set.

Optional `CODEX_ACP_CLI_MODEL` selects the model advertised/passed to
`codex exec` when CLI mode is on (adapter default `gpt-5`). CLI mode only
advertises a single virtual model; app-server mode provides a multi-model
`model/list`.

Sessions created under one runtime cannot be resumed after switching the other
because ACP IDs are not interchangeable with Codex thread IDs. The app must tell
users to create a new Codex conversation; no session migration or
legacy-runtime fallback is supported.

## Clean-machine verification

1. Fresh install of MyCodeBuddy only → Codex agent not installed until Agent
   Settings install (or manual `npm install -g`).
2. Install `@agentclientprotocol/codex-acp@1.1.7` → connect under default
   app-server (`CODEX_ACP_USE_CLI=0`).
3. Agent Settings → Codex → turn **Use CLI exec runtime** on (needs host Codex)
   → CLI exec path.
4. Registry distribution env for Codex **must** include `CODEX_ACP_USE_CLI=0`.
   Opt-in is user Agent env `CODEX_ACP_USE_CLI=1` (or the settings toggle).

## Historical note

Older MyCodeBuddy builds briefly shipped a Stop-patched vendored pin via
`resources/codex-acp-seed` and a managed npm prefix under the app data dir.
That path was retired in favor of official registry npm only; do not re-add
seed packaging to desktop/server/Docker release jobs.
