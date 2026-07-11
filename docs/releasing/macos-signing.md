# macOS Developer ID Signing

This fork does not publish prebuilt macOS artifacts. Its GitHub release
workflow is Windows-only. The instructions below apply only to an optional
manually distributed macOS build outside the App Store; they are not required
for normal local development.

A normal local app build is secret-free because the default Tauri config sets
`bundle.createUpdaterArtifacts` to `false`:

```bash
pnpm tauri build --bundles app
```

Run that command with Tauri signing variables unset and without sourcing
`local-build.env`. The resulting app still includes `LICENSE`, `NOTICE`, and
`THIRD_PARTY_LICENSES.txt`.

References:

- Tauri macOS code signing: https://v2.tauri.app/distribute/sign/macos/
- Apple Developer ID: https://developer.apple.com/support/developer-id/

## Optional Distribution Credentials

| Secret | Value |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64 contents of the exported `.p12` certificate |
| `APPLE_CERTIFICATE_PASSWORD` | Password used when exporting the `.p12` |
| `KEYCHAIN_PASSWORD` | Random password for the temporary CI keychain |
| `APPLE_ID` | Apple ID email |
| `APPLE_PASSWORD` | Apple app-specific password |
| `APPLE_TEAM_ID` | Apple Developer Team ID |

`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` are used
by the Windows release workflow for updater artifacts. They are separate from
Apple code signing and are not needed for a normal local macOS `.app` build.

## Generate The Certificate Secret

1. On a Mac, open Keychain Access.
2. Create a Certificate Signing Request:
   `Keychain Access > Certificate Assistant > Request a Certificate From a
   Certificate Authority`.
3. In Apple Developer, open Certificates, IDs & Profiles and create a
   `Developer ID Application` certificate. This is the certificate type for
   distribution outside the App Store.
4. Download the `.cer` file and double-click it to install it into the login
   keychain.
5. In Keychain Access, open `login > My Certificates`, expand the Developer ID
   Application certificate, right-click its private key, and export it as a
   `.p12` file. Set a strong export password.
6. From the repo root, convert the `.p12` and generate the CI keychain password:

```bash
scripts/prepare-macos-signing-secrets.sh /path/to/DeveloperIDApplication.p12
```

Then run the `gh secret set ...` commands printed by the script.

## Generate Apple Notarization Secrets

Set the notarization secrets:

```bash
gh secret set APPLE_ID --body "you@example.com"
gh secret set APPLE_PASSWORD --body "xxxx-xxxx-xxxx-xxxx"
gh secret set APPLE_TEAM_ID --body "TEAMID1234"
```

`APPLE_PASSWORD` must be an Apple app-specific password, not the normal Apple ID
password. Create it from the Apple ID account security page.

Find `APPLE_TEAM_ID` in Apple Developer membership details.

## Verify Locally

After installing the certificate locally, this should show a Developer ID
Application identity:

```bash
security find-identity -v -p codesigning | grep "Developer ID Application"
```

For a local notarized DMG build:

```bash
export APPLE_SIGNING_IDENTITY="Developer ID Application: Your Name (TEAMID1234)"
export APPLE_ID="you@example.com"
export APPLE_PASSWORD="xxxx-xxxx-xxxx-xxxx"
export APPLE_TEAM_ID="TEAMID1234"
pnpm tauri build --bundles dmg
```

Validate the output:

```bash
xcrun stapler validate src-tauri/target/release/bundle/dmg/*.dmg
spctl -a -vvv -t install src-tauri/target/release/bundle/dmg/*.dmg
```

## CI Behavior

The current GitHub Actions release workflow has no macOS matrix and reads no
Apple signing or notarization secrets. Adding macOS distribution later would
require a separate reviewed release-policy change; these optional credentials
must not be added to the existing Windows-only workflow.
