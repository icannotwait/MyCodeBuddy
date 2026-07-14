# Releasing MyCodeBuddy For Windows

MyCodeBuddy uses one Tauri updater signing key for desktop updater artifacts
and the Windows server ZIP. The Windows installer is not Authenticode-signed,
so Microsoft Defender SmartScreen may warn users during installation. The
updater signature protects release integrity but does not replace
Authenticode.

## Signing Material

The local signing helper generated these files outside the repository:

- Private key:
  `~/.config/mycodebuddy/signing/updater-signing.key`
- Password:
  `~/.config/mycodebuddy/signing/updater-signing.password`
- Local build environment:
  `~/.config/mycodebuddy/signing/local-build.env`
- GitHub secret setup reference:
  `~/.config/mycodebuddy/signing/GITHUB_SECRETS.md`

The repository contains only the generated public key in
`src-tauri/tauri.conf.json`. Do not commit the private key, password,
`local-build.env`, or copies of their contents.

Normal local desktop builds do not use this signing material.
`src-tauri/tauri.conf.json` sets `bundle.createUpdaterArtifacts` to `false`, so
the following command must run with all Tauri signing variables unset:

```bash
pnpm tauri build --bundles app
```

The command still bundles `LICENSE`, `NOTICE`, and
`THIRD_PARTY_LICENSES.txt`. Do not source `local-build.env` for a normal local
app build.

Back up the private key and password together in an encrypted, access-controlled
location. Losing either prevents publishing updates that existing
installations will trust. Replacing the key requires distributing a trusted
application build with the replacement public key.

If the helper is interrupted, it fails closed on its generation lock. Verify
no helper is active, then delete only
`~/.config/mycodebuddy/signing/.updater-signing-generation.lock` before
rerunning it. The helper never deletes a stale lock automatically.

## Configure GitHub Secrets

Do not push a release tag until both repository secrets are configured.

1. Open the `icannotwait/MyCodeBuddy` repository on GitHub.
2. Select **Settings**, then **Secrets and variables**, then **Actions**.
3. Select **New repository secret**.
4. Create `TAURI_SIGNING_PRIVATE_KEY` using the complete contents of
   `~/.config/mycodebuddy/signing/updater-signing.key`.
5. Create `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` using the complete contents of
   `~/.config/mycodebuddy/signing/updater-signing.password`.
6. Confirm both names appear under repository Actions secrets before creating
   or pushing a release tag.

Enter secret values only through the GitHub repository Settings UI. Do not
paste them into issues, pull requests, workflow files, shell history, or build
logs.

## Release Sequence

After the release commit is merged to the default branch and both GitHub
secrets are configured, run:

```bash
pnpm release:check
git tag v0.20.2-mycodebuddy.5
git push origin v0.20.2-mycodebuddy.5
```

The tag starts the Windows release workflow. After all builds and uploads
succeed, the workflow publishes the release automatically. Inspect the
published release afterward to confirm it contains the Windows x64 desktop
updater artifacts and the signed Windows x64 server ZIP.

The desktop release step explicitly passes
`--config src-tauri/tauri.release.conf.json`. That minimal override enables
`bundle.createUpdaterArtifacts`, while `includeUpdaterJson: true` publishes the
combined updater manifest. The signing secrets are therefore required only by
release jobs that create signed updater artifacts or sign the server ZIP.
