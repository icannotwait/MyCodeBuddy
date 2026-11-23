# Upstream 0.47.0 conflict resolution

Merge of upstream `main` `a7427c4e66926f1de7f928d1b963cdb7f8566fe5` (tag
`v0.47.0` is `21c02cc8d`, plus the WeChat QR docs commit on top) into
MyCodeBuddy `origin/main` `bc0b2673388d8857655c99cc09587dded614f475`.
Merge-base is tag `v0.45.0` (`bd88b9dac`). Resulting fork version is
`0.47.0-mycodebuddy.1`.

## Decisions that must survive the next sync

1. **Branding stays ours.** Version `0.47.0-mycodebuddy.1` in `package.json`,
   `src-tauri/Cargo.toml`, the `codeg` package in `src-tauri/Cargo.lock`,
   `src-tauri/tauri.conf.json`, `install.ps1`, and the README install
   examples. `productName` / `mainBinaryName` stay `DrawCode`, identifier
   `app.mycodebuddy`, updater endpoint and pubkey stay the MyCodeBuddy ones.
   `bundle.createUpdaterArtifacts` stays `false`. NSIS targets stay, including
   the Restart Manager include in `src-tauri/windows/installer.nsi`.
2. **OpenClaw stays deleted.** `ALL_MCP_APPS` stays `pub(crate)` and length 19.
3. **`codeg-server` stays** `src/server_bin/main.rs`. Do not take an upstream
   `src/bin/codeg_server.rs`.
4. **Codex ACP stays pinned at 2.1.1** (`codex_distribution()`, preflight
   `@agentclientprotocol/codex-acp@2.1.1`). Nested fallback stays `0.159.3`.
   Do not take upstream 2.8.0.
5. **Keep `production_tauri_commands!`.** Register new upstream commands inside
   the macro (harness, SRN, skills engine, DeepSeek provider, deep link).
   Do not replace it with an expanded `generate_handler!`.
6. **Delegation, roundtable, admission, codeg-boot, Superpowers, Grok, and the
   Windows installer stay.** Upstream session/harness work is adapted around
   them. `EventEmitter::from_app` is used at Tauri command boundaries; fork
   core signatures (registry, coordinator, route plan) stay.
7. **Skills engine bodies come from upstream** (`legacy::statuses` /
   `SkillsCtx`). Settings pages for MCP and Skills are deleted with upstream;
   those surfaces live on the Harness page.
8. **Locks are regenerated, not hand-merged.** `pnpm-lock.yaml` is upstream
   plus `pnpm install`. `Cargo.lock` is upstream plus a lockfile refresh, with
   the `codeg` package version `0.47.0-mycodebuddy.1`.
9. **i18n is a JSON deep merge** from the merge-base, ours, and upstream.
   Unchanged leaves follow the side that edited them. DrawCode / MyCodeBuddy
   strings and roundtable / admission / watchdog paths stay ours. A key we
   removed and upstream left untouched stays removed.
