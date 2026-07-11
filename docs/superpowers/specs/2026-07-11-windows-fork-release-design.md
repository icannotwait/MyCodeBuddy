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

## Upstream Synchronization

The local repository will use two remotes:

```text
origin    -> icannotwait/MyCodeBuddy
upstream  -> xintaofei/codeg
```

MyCodeBuddy's `main` branch will track `origin/main`. Published history will
not be rewritten. New Codeg versions will be integrated through a temporary
branch named `sync/codeg-<version>`:

```bash
git fetch upstream
git switch main
git pull --ff-only origin main
git switch -c sync/codeg-<version>
git merge --no-ff upstream/main
```

After resolving conflicts and passing the full verification suite, the sync
branch will be pushed to the fork and merged through a pull request. Direct
rebases of published MyCodeBuddy history are out of scope.

Repository-local documentation will record this workflow in
`docs/UPSTREAM_SYNC.md`, including conflict-resolution guidance for branding,
release configuration, removed upstream features, and local functional
changes. Git `rerere` will be recommended so repeated conflicts can reuse
previous resolutions.

Branding, updater, licensing, and release-only changes will remain concentrated
in a small set of files. Functional modifications will remain in separate
commits where practical so future upstream conflicts are attributable and
reviewable.

## Versioning

MyCodeBuddy releases will preserve the upstream base version and add a SemVer
prerelease suffix:

```text
0.18.8-mycodebuddy.1
0.18.8-mycodebuddy.2
0.18.9-mycodebuddy.1
```

The corresponding Git tags are:

```text
v0.18.8-mycodebuddy.1
v0.18.8-mycodebuddy.2
v0.18.9-mycodebuddy.1
```

The counter resets to `1` whenever the upstream base version changes. The
package, Cargo, and Tauri versions must remain identical. The release workflow
will accept only `-mycodebuddy.` versions and will reject tags that do not
exactly match the configured application version.

Although the suffix is represented by SemVer's prerelease component, completed
MyCodeBuddy builds will be published as non-prerelease GitHub Releases. This is
required because GitHub's `/releases/latest` and
`/releases/latest/download/...` routes exclude releases marked as prereleases;
the updater and Windows server installer depend on those routes.

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
The generator will collect npm production dependencies once and use the
deterministic union of Cargo packages resolved for exactly:

- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

Cargo records with the same ecosystem, name, and version will be deduplicated.
Equivalent declarations, homepages, and license texts may be merged; conflicting
metadata will fail generation. The generator must also fail when a bundled
package has no discoverable license declaration or license file, unless that
package is explicitly documented in a small reviewed exception list.

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

The default `src-tauri/tauri.conf.json` will set
`bundle.createUpdaterArtifacts` to `false`. A minimal
`src-tauri/tauri.release.conf.json` override will set it to `true`, and only
the Windows desktop release workflow will pass that override explicitly.
`includeUpdaterJson: true` remains required for release publication.

## Release Workflow

The existing tag-triggered release workflow will be simplified rather than
adding a second competing workflow.

The workflow will:

1. verify that the tag commit belongs to the default branch;
2. verify that the tag exactly matches the package, Cargo, and Tauri versions;
3. create or reuse a draft GitHub Release;
4. build and upload Windows x64 and ARM64 NSIS/updater artifacts;
5. build, smoke-test, sign, and upload the Windows x64 server ZIP;
6. publish the release only after all Windows jobs succeed.

The server installer will validate both executables plus `LICENSE`, `NOTICE`,
and `THIRD_PARTY_LICENSES.txt` before writing to its destination, then install
all five files. Windows prebuilt server upgrades are manual: users rerun
`install.ps1` or replace the installation from the next Windows ZIP.

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
require the Windows release workflow, Authenticode certificate, updater
private key, or updater password.

Updater artifacts are disabled by default and enabled only by the release
override. The normal local command is:

```bash
pnpm tauri build --bundles app
```

It must succeed with all Tauri signing variables unset and must retain
`LICENSE`, `NOTICE`, and `THIRD_PARTY_LICENSES.txt` in the `.app`.

## Tests And Verification

Automated verification will cover:

- deterministic third-party license report generation;
- four-target Cargo union, deterministic deduplication, and conflict rejection;
- required compliance files appearing in Tauri resource configuration;
- release-only updater artifact configuration and workflow usage;
- installer validation and copying of all compliance files;
- no runtime updater URL pointing to `xintaofei/codeg`;
- package, Cargo, Tauri, and tag version consistency;
- release workflow containing only Windows build targets;
- upstream sync documentation containing the required remote and merge flow;
- frontend production build;
- frontend tests and lint;
- Rust tests and clippy for the affected release tooling;
- local macOS `app` bundle build where supported.

The final implementation will not modify or discard unrelated uncommitted
changes already present in the working tree.
