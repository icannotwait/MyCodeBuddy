# Bundled Codex ACP for Windows x64

## Goal

Ship MyCodeBuddy for Windows x64 with the customized `codex-acp` implementation
inside the existing NSIS installer. Users download and install one package and do
not need Node.js, npm, a second installer, or a network download to start Codex.

The bundled adapter must take precedence over globally installed official
`codex-acp` versions and must not be replaced by the agent settings update flow.

## Scope

- Target: `x86_64-pc-windows-msvc` only.
- Desktop NSIS release only.
- macOS development keeps the existing PATH/npm adapter behavior.
- The customized adapter remains independently maintained and can continue to
  merge upstream `agentclientprotocol/codex-acp` changes.
- Windows ARM64 packaging is removed from the desktop release matrix.

## Source Management

The customized adapter lives in a public fork at
`https://github.com/icannotwait/codex-acp.git`. The existing local repository is
prepared by making the official repository the `upstream` remote and the public
fork the `origin` remote. All current committed and uncommitted customization is
reviewed, tested, committed, and pushed before it is referenced by Codeg.
Creating that public fork and pushing the reviewed customization is a required
implementation prerequisite; the repository does not exist at design time.

MyCodeBuddy includes that fork as a Git submodule at:

```text
src-tauri/vendor/codex-acp
```

The Codeg repository records an exact submodule commit. Updating the bundled
adapter is therefore a deliberate two-stage operation:

1. Merge or rebase official upstream changes into the public fork, resolve the
   customization conflicts, test, and push the resulting commit.
2. Advance the submodule pointer in MyCodeBuddy and commit that pointer change.

GitHub Actions checks out submodules recursively. Because the fork is public,
the release workflow does not require a deploy key or private-repository token.

## Adapter Versioning

The first bundled fork release uses the distinct valid semver
`1.1.0-mycodebuddy.1`. Every subsequent change to the customized adapter
increments the MyCodeBuddy prerelease revision. The
version printed by `codex-acp.exe --version`, the version shown in Codeg settings,
and the submodule commit used for the release must describe the same source.

The adapter's package lock is committed. Generated dependency directories and
build output (`node_modules`, `dist`, and `dist/bin`) are not committed.

## Build And Packaging

The Windows x64 release runner installs the pinned Node and Bun versions. The
sidecar preparation step performs these operations:

1. Verify the codex-acp submodule is initialized at a commit.
2. Run `npm ci` to install dependencies from `package-lock.json` without
   updating it.
3. Run `npm run typecheck` and `npm test`.
4. Run `npm run bundle:win-x64` to build the existing Bun Windows x64
   standalone executable.
5. Copy it to Tauri's target-qualified sidecar name:
   `src-tauri/binaries/codex-acp-x86_64-pc-windows-msvc.exe`.
6. Verify the file is non-empty and that `--version` reports the expected fork
   version.
7. Run a small ACP initialize smoke test to catch dependencies that Bun failed
   to embed, particularly the supported `@openai/codex` runtime.

The Windows release Tauri configuration lists both `codeg-mcp` and `codex-acp`
as external binaries. Tauri places `codex-acp.exe` beside `MyCodeBuddy.exe` in
the installed application. It remains an internal sidecar, not a separately
published installer or release asset.

The normal developer configuration does not require a macOS codex-acp sidecar.
Only the Windows release configuration adds it to `externalBin`, so existing
macOS development remains usable without building another platform artifact.

The release-policy checks enforce the x64 matrix, sidecar staging, non-empty
verification, recursive submodule checkout, and required bundled binary entry.

## Runtime Resolution

Windows resolves the Codex ACP executable in this order:

1. `CODEG_CODEX_ACP_BIN`, when it names an existing executable file. This is an
   explicit development and diagnostic override.
2. `codex-acp.exe` beside the running MyCodeBuddy executable. This is the normal
   production path.
3. `codex-acp` on PATH as a development fallback when the sidecar is absent.

The selected path is logged without credentials or environment values. A
packaged build in which the sidecar is missing or cannot start reports a clear
installation-corruption error instead of silently installing the official npm
package.

The child process continues to receive Codeg's existing Codex runtime settings,
and the bundled Windows recipe supplies the same fork defaults as the current
development shim: `CODEX_ACP_USE_CLI=1` and `CODEX_ACP_CLI_MODEL=gpt-5.5`.
Explicit per-agent runtime settings override these defaults through the existing
environment merge order. No wrapper shell script is used on Windows.

## Registry And Settings Behavior

On Windows x64, Codex is represented as a bundled distribution rather than an
npm-managed distribution. Agent discovery and preflight therefore do not
require Node.js or npm and report the bundled fork version as installed.

The settings page labels Codex ACP as built in. It does not offer install,
custom-version install, upgrade, or uninstall actions for the bundled adapter.
Updating MyCodeBuddy is the supported way to update it. Codex authentication,
model-provider, environment, and runtime configuration controls remain
unchanged.

On non-Windows development builds, the current npm/PATH distribution behavior
remains available. This platform distinction is explicit in the registry rather
than inferred from whether a random global executable happens to exist.

## Failure Handling

- Missing submodule: fail the release build before dependency installation.
- Dependency or test failure: fail the release build; do not reuse stale output.
- Missing or zero-byte sidecar: fail before Tauri packaging.
- Wrong `--version`: fail before packaging to prevent shipping the official or
  stale adapter accidentally.
- ACP smoke-test failure: fail before packaging with captured adapter stderr.
- Missing embedded Codex runtime: fail the ACP smoke test. A successful Bun
  compilation alone is insufficient because `@openai/codex` selects a
  platform-specific executable at runtime.
- Installed sidecar missing: show an installation-corruption error instructing
  the user to reinstall or update MyCodeBuddy; do not expose an agent-level
  repair action and do not mutate global npm state.
- Explicit override invalid: report the invalid path and do not silently ignore
  an operator's explicit configuration.

## Licensing And Signing

The fork's Apache license and notice, plus the licenses of JavaScript
dependencies embedded into the Bun executable, are included in the generated
third-party license resource. The release policy verifies these inputs are
present.

The existing Tauri updater signature covers update integrity but is not Windows
Authenticode signing. The release smoke test includes a clean Windows x64
installation and a Microsoft Defender scan. Authenticode signing can be added
separately when a Windows code-signing certificate is available; lack of such a
certificate does not change the one-installer architecture.

## Verification

Automated verification covers:

- submodule checkout and pinned-commit detection;
- target-name mapping for the Windows x64 sidecar;
- runtime precedence: explicit override, bundled sibling, then PATH fallback;
- registry/preflight behavior without Node or npm on Windows;
- settings behavior that prevents managed npm actions for bundled Codex;
- fork version identity;
- ACP initialize and session creation smoke tests that actually start the
  embedded Codex runtime without requiring a live model turn;
- release-policy enforcement; and
- existing Rust and frontend regression suites affected by the registry change.

Release acceptance is performed on a clean Windows x64 machine or VM with no
global `codex-acp` and no Node.js installation. The installed MyCodeBuddy must
start a Codex session using the sibling bundled executable. A second test with
an official global `codex-acp` installed confirms the bundled executable still
wins.

## Out Of Scope

- Publishing a separate codex-acp installer or release download.
- Windows ARM64, macOS, or Linux bundled adapter binaries.
- Automatic upstream synchronization.
- Allowing end users to replace the bundled adapter through the normal agent
  update UI.
