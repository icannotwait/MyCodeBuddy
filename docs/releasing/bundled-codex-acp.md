# Bundled codex-acp maintenance

MyCodeBuddy's Windows x64 installer includes the customized codex-acp fork as
`codex-acp.exe`. The source is pinned as the public Git submodule at
`src-tauri/vendor/codex-acp`; Agent Settings never replaces this executable.

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

Resolve conflicts without dropping the CLI runtime customization. Set the
version in both `package.json` and `package-lock.json` to the merged upstream
version plus a MyCodeBuddy revision. For example, another patch on upstream
1.1.2 becomes `1.1.2-mycodebuddy.2`; an update to upstream 1.2.0 starts at
`1.2.0-mycodebuddy.1`. Then verify and publish:

```bash
npm run build
node dist/index.js --version
git add package.json package-lock.json src vitest.config.ts
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
its fork version, and invokes its embedded Codex CLI. A failure in any step must
block the installer release.

On a clean Windows x64 machine, verify that MyCodeBuddy starts Codex without
Node.js or a global codex-acp installation. Then install the official global
adapter and confirm MyCodeBuddy logs still identify the sibling bundled
`codex-acp.exe` path.
