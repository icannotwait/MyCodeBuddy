# MyCodeBuddy Windows Fork Release Design

## Goal

Prepare the MyCodeBuddy fork for license-compliant Windows distribution from
`icannotwait/MyCodeBuddy`, while preserving local macOS development and
`pnpm tauri build --bundles app`.

## Scope

The release pipeline will produce:

- MyCodeBuddy desktop installers and updater artifacts for Windows x64.
- MyCodeBuddy desktop installers and updater artifacts for Windows ARM64.
- A Windows x64 `codeg-server` ZIP containing `codeg-server.exe`,
  `codeg-mcp.exe`, and the static web application.
- A published GitHub Release triggered by a version tag matching
  `src-tauri/tauri.conf.json`.

The release pipeline will no longer build macOS, Linux, or Docker artifacts.
This restriction applies only to GitHub Release automation. The Tauri project,
icons, Rust conditional compilation, and local macOS build commands remain
cross-platform.

## Repository Identity

Release, update, download, source-code, and help links owned by the fork will
point to:

```text
https://github.com/icannotwait/MyCodeBuddy
```

References that describe the original project's provenance will continue to
identify Codeg and its upstream repository. MyCodeBuddy will not imply that the
upstream project endorses the fork.

README sections for Linux, macOS, and Docker release artifacts will be removed
or rewritten so they do not advertise artifacts that the fork's Windows-only
workflow does not publish. Windows desktop and Windows server installation
instructions will point to the fork.

## Licensing And Attribution

The upstream Apache License 2.0 text remains unchanged in the root `LICENSE`
file.

A root `NOTICE` file will:

- identify MyCodeBuddy as a modified distribution based on Codeg;
- link to the upstream source repository;
- state that modifications are made by MyCodeBuddy contributors;
- avoid claiming ownership of upstream trademarks.

A generated third-party license report will list bundled production npm and
Cargo packages, their versions, declared licenses, and available license text.
The generator must be deterministic and must fail when a bundled package has no
discoverable license declaration or license file, unless that package is
explicitly documented in a small reviewed exception list.

Tauri will bundle `LICENSE`, `NOTICE`, and the generated third-party license
report on every desktop platform. Therefore the compliance resources are also
present in local macOS application bundles.

## Updater Signing

The fork will use a newly generated Tauri updater signing key pair:

- the public key is committed in `src-tauri/tauri.conf.json`;
- the private key is never committed;
- the private key and password are stored as GitHub Actions secrets named
  `TAURI_SIGNING_PRIVATE_KEY` and
  `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

Desktop and server update URLs will use the fork's GitHub Releases. A static
test will reject upstream update URLs in runtime update configuration.

The Windows installer will not use Authenticode because no code-signing
certificate is available. Documentation will state that Windows SmartScreen
may warn on first installation. This is independent of the Tauri updater
signature, which protects update artifact integrity.

## Release Workflow

The existing tag-triggered release workflow will be simplified rather than
adding a second competing workflow.

The workflow will:

1. verify that the tag commit belongs to the default branch;
2. verify that the tag exactly matches the Tauri application version;
3. create or reuse a draft GitHub Release;
4. build and upload Windows x64 and ARM64 NSIS/updater artifacts;
5. build, smoke-test, sign, and upload the Windows x64 server ZIP;
6. publish the release only after all Windows jobs succeed.

Apple certificates, Linux cross-compilers, Docker buildx, Docker Hub secrets,
and unrelated release jobs will be removed from this workflow.

## Local macOS Compatibility

The following commands must remain supported:

```bash
pnpm tauri dev
pnpm tauri build --bundles app
```

No macOS icons, Cargo target dependencies, Tauri macOS settings, or
platform-specific Rust code will be removed. Local macOS `.app` builds do not
require the Windows release workflow or Authenticode certificate.

Updater artifacts are not required for the local self-use `.app` command. The
verification process will build the frontend, run Rust tests, and perform a
local macOS app-only build when the local signing environment permits it.

## Tests And Verification

Automated verification will cover:

- deterministic third-party license report generation;
- required compliance files appearing in Tauri resource configuration;
- no runtime updater URL pointing to `xintaofei/codeg`;
- release workflow containing only Windows build targets;
- frontend production build;
- frontend tests and lint;
- Rust tests and clippy for the affected release tooling;
- local macOS `app` bundle build where supported.

The final implementation will not modify or discard unrelated uncommitted
changes already present in the working tree.
