# Bundled codex-acp maintenance

MyCodeBuddy's Windows x64 installer includes the customized codex-acp fork as
`codex-acp.exe`. The source is pinned as the public Git submodule at
`src-tauri/vendor/codex-acp`; Agent Settings never replaces this executable.

## Runtime dependency: host Codex (app-server)

MyCodeBuddy ships the **codex-acp adapter** (`codex-acp.exe`) next to the app.
That fork keeps MyCodeBuddy's custom ACP surface. On Windows it does **not**
force `CODEX_ACP_USE_CLI`; the adapter starts the host Codex as
`codex app-server` via `CODEX_PATH` so clients get a real `model/list`, normal
turns, and session APIs.

Resolution order for `CODEX_PATH`:

1. Explicit `CODEX_PATH` in the process or agent environment
2. `codex` / `codex.cmd` on `PATH`
3. npm global prefix (`%APPDATA%\npm`) `codex.cmd` or
   `node_modules/@openai/codex/bin/codex.js`

Users still do **not** need a global `codex-acp` package; they do need a host
Codex CLI (e.g. `npm install -g @openai/codex`) unless they set `CODEX_PATH`.

Optional experimental CLI exec mode (`CODEX_ACP_USE_CLI=1` + optional
`CODEX_ACP_CLI_MODEL`) remains available in the fork for debugging, but is
**not** the Windows product default (it only advertises a single virtual model).

Sessions created with the prior CLI-runtime default cannot be resumed after
switching to app-server because their ACP IDs are not Codex thread IDs. The app
must tell users to create a new Codex conversation; no session migration or
legacy-runtime fallback is supported.

Clean-machine verification:

1. Install MyCodeBuddy only → Codex preflight should fail on "Codex CLI" with install guidance.
2. `npm install -g @openai/codex` → preflight passes; new Codex session initializes with a multi-model list from app-server.
3. With a global official `codex-acp` also installed, logs must still show the sibling
   bundled `codex-acp.exe` path as the adapter.

## Update from upstream

Work in the standalone fork clone so its branch and remotes remain explicit:

```bash
cd <path-to-codex-acp-fork>
git fetch upstream
git checkout codex/codex-acp-cli-runtime
git merge upstream/main
npm ci
npm run typecheck
npm test
```

Resolve conflicts without dropping fork-specific ACP customizations (including
the optional CLI runtime path). Set the version in both `package.json` and
`package-lock.json` to the merged upstream version plus a MyCodeBuddy revision.
For example, another patch on upstream 1.1.2 becomes `1.1.2-mycodebuddy.2`; an
update to upstream 1.2.0 starts at `1.2.0-mycodebuddy.1`. Then verify and publish:

```bash
npm run build
node dist/index.js --version
git add package.json package-lock.json src tsconfig.json vitest.config.ts
git commit -m "chore: update MyCodeBuddy codex-acp"
git push origin codex/codex-acp-cli-runtime
```

Advance the Codeg gitlink only after the fork commit is public and tested:

```bash
cd <path-to-MyCodeBuddy>
git submodule update --init src-tauri/vendor/codex-acp
git -C src-tauri/vendor/codex-acp fetch origin codex/codex-acp-cli-runtime
git -C src-tauri/vendor/codex-acp checkout origin/codex/codex-acp-cli-runtime
git add src-tauri/vendor/codex-acp
git commit -m "chore: update bundled codex-acp"
```

## Release verification

The Windows release job checks out submodules recursively, pins Bun 1.3.14,
runs the fork's typecheck and tests, builds the Windows x64 executable, verifies
its fork version, and smoke-starts the adapter. A failure in any step must block
the installer release.

On a clean Windows x64 machine, verify:

1. MyCodeBuddy only (no host Codex CLI) → Codex preflight fails with install guidance.
2. After `npm install -g @openai/codex` (or `CODEX_PATH` set) → preflight passes and a new Codex session initializes with models from app-server `model/list`.
3. Users need **no** global `codex-acp` package; the sibling bundled `codex-acp.exe` is used.
4. With a global official `codex-acp` also installed, logs must still identify the sibling bundled `codex-acp.exe` path as the adapter.
5. Registry distribution env for Windows Codex must **not** include `CODEX_ACP_USE_CLI` or `CODEX_ACP_CLI_MODEL`.
