# WebView2 Conversation Pop-out Version Gate Delivery Report

## Delivery Identity

- Plan digest: `679b80ef946049fb471f519620e049d07cfcf16c58e8e59d152db59ef58709d6`
- Design digest: `381cb13e014b6a224b3342e353adb929543733d6e597ac8cb86f54732528a39f`
- Implementation base: `7891c0f794d58275bae5e30684460d81042a6f52`
- Task 1-5 baseline / Task 6 admission HEAD:
  `71cab930ae36795ac7c9a8f466c999596b7011cc`
- Implementation HEAD before this report:
  `81b0ca5d6a9e5a2ded7932f80e0a5e34dbf7c39f`
- Task 6 report path:
  `.superpowers/sdd/2026-08-10-webview2-popout-version-gate/delivery-report.md`

## Producer And Review Audit

| Task | Producer commit | Codex verdict | Grok verdict |
| --- | --- | --- | --- |
| 1 | `4161d398da22863a45cafac4bb0e5e6e46377606` (`feat: project WebView2 runtime drift`) | `approve` | `approve` |
| 2 | `4f5a7136ad87761f801564b09fdf13fb977f9926` (`fix: gate pop-out creation on WebView2 drift`) | `approve_with_minors` | `approve` |
| 3 | `bb24727a2ef6308ee811025fc76657c3b1044699` (`fix: bypass pop-out compensation for runtime drift`) | `approve` | `approve` |
| 4 | `6eec014a48920283e79c37e9e8038fd493e31579` (`feat: prompt for pop-out runtime restart`) | `approve` | `approve` |
| 5 | `71cab930ae36795ac7c9a8f466c999596b7011cc` (`feat: localize pop-out restart prompt`) | `approve` | `approve` |

Task 2 Codex recorded one minor issue: the replacement focus test does not
also assert the preserved `focusedExisting` wire value. It was non-blocking.

Task 6 server Clippy exposed a Task 2 feature-surface regression: the new Rust
wire-key constant was unused in production server builds. The regression was
reproduced with the exact server Clippy command, fixed test-first by compiling
the constant only for `tauri-runtime` or tests, and committed separately as
`81b0ca5d6a9e5a2ded7932f80e0a5e34dbf7c39f` (`fix: gate pop-out key to desktop
builds`). Its focused Task 2 tests, desktop check/Clippy, server check/Clippy,
and MCP check/Clippy all pass. Per the Task 6 brief, this integrator did not
spawn reviewers; Task 6 reviewers must review the final report HEAD including
this fix.

## Aggregate Verification

All commands below were rerun on Windows after the Task 2 fix unless an entry
is explicitly identified as diagnostic red evidence.

### Step 1: Source Invariants

- Required Rust and TypeScript constant searches: pass.
- Stable wire-literal producer/lockstep searches: pass.
- Preflight, drift, sanitization, non-Windows, toast, and relaunch searches:
  pass.
- Forbidden `restartApp|restart_app|active task|active goal|wait.*idle`
  search: pass, with the expected `rg` no-match status 1 normalized to command
  exit 0 after validation.

### Step 2: Frontend

| Command | Outcome |
| --- | --- |
| `pnpm eslint .` | Pass, exit 0: 0 errors and 24 warnings. |
| `pnpm test` | Pass, exit 0: complete Vitest suite. |
| `pnpm build` | Pass, exit 0: Next.js static export compiled and generated all 33 pages. |

### Step 3: Desktop Rust

| Command | Outcome |
| --- | --- |
| `cargo check --manifest-path src-tauri/Cargo.toml` | Pass, exit 0. The existing development warning notes the zero-byte `codeg-mcp` sidecar placeholder. |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils` | Fail, exit 1: 4,293 passed, 2 failed, 1 ignored. The deterministic unrelated failure is `web::handlers::tool_watchdog::tests::settings_routes_require_auth_and_share_cores`; `update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps` also hit one transient Windows permission error. |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils update::install::tests::swap_dir_via_copy_keeps_backup_and_swaps -- --exact --nocapture` | Pass, exit 0: 1 passed, confirming the update-copy failure did not reproduce in isolation. |
| `cargo test --manifest-path src-tauri/Cargo.toml --features test-utils` | Fail, exit 1: 4,294 passed, 1 failed, 1 ignored; only the deterministic watchdog assertion failed. |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings` | Pass, exit 0. |

The watchdog failure reproduces exactly in isolation. Its test expects
`enabled == true` at `src-tauri/src/web/handlers/tool_watchdog.rs:169`, while
`load_tool_watchdog_settings_from` documents and returns the current product
default `enabled=false`. That module is unchanged in the implementation range,
so Task 6 did not edit this unrelated owner.

### Step 4: Server And MCP

| Command | Outcome |
| --- | --- |
| `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server` | Pass, exit 0, with no dead-code warning after the Task 2 fix. |
| `cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib` | Fail, exit 1: 4,179 passed, 1 failed, 1 ignored; only the same unrelated watchdog assertion failed. |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server --lib -- -D warnings` | Pass, exit 0. The pre-fix red run failed only because the new wire-key constant was unused. |
| `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp` | Pass, exit 0. |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --no-default-features --bin codeg-mcp -- -D warnings` | Pass, exit 0. |

### Step 5: Local Non-Windows Compile Gate

| Command or gate | Outcome |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils window_diagnostics::tests::non_windows_webview_runtime_drift_is_unchanged_without_query -- --exact` | Pass: 1 passed, 0 failed. |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils window_diagnostics::tests::webview_runtime_query_error_diagnostic_is_sanitized -- --exact` | Pass: 1 passed, 0 failed. |
| Regex extraction of `non_windows_webview_runtime_drift` | Pass: helper exists and contains no `tauri::webview_version`. |
| Regex extraction of non-Windows `current_webview_runtime_drift` | Pass: wrapper delegates directly to the tested helper. |

This is the authorized local non-Windows behavior and compile proof. It used no
WSL distribution, Docker daemon, push, PR, hosted CI, or human action.

## Changed Paths

The complete implementation range
`7891c0f794d58275bae5e30684460d81042a6f52..81b0ca5d6a9e5a2ded7932f80e0a5e34dbf7c39f`
contains these 27 paths:

```text
.superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-1-report.md
.superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-2-report.md
.superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-3-report.md
src-tauri/src/app_error.rs
src-tauri/src/commands/conversation_popout.rs
src-tauri/src/window_diagnostics.rs
src/components/conversations/sidebar-conversation-card.test.tsx
src/components/conversations/sidebar-conversation-card.tsx
src/components/tabs/tab-bar.tsx
src/components/tabs/tab-strip-wiring.test.ts
src/i18n/messages.test.ts
src/i18n/messages/ar.json
src/i18n/messages/de.json
src/i18n/messages/en.json
src/i18n/messages/es.json
src/i18n/messages/fr.json
src/i18n/messages/ja.json
src/i18n/messages/ko.json
src/i18n/messages/pt.json
src/i18n/messages/zh-CN.json
src/i18n/messages/zh-TW.json
src/lib/api.test.ts
src/lib/api.ts
src/lib/conversation-popout-notifications.test.ts
src/lib/conversation-popout-notifications.ts
src/lib/conversation-popout.test.ts
src/lib/conversation-popout.ts
```

The Task 6 delivery range adds only this report path. `git diff --check`
passes. No design specification, dependency manifest, lockfile, or generated
build output changed.

## Design Traceability

| Design requirement | Result | Proof |
| --- | --- | --- |
| Startup snapshot vs fresh Windows query | Pass | `webview_runtime_drift_projects_trimmed_exact_versions`; `non_windows_webview_runtime_drift_is_unchanged_without_query`; source invariant. |
| Trim exact strings; four components; whitespace is Unknown | Pass | `webview_runtime_drift_projects_trimmed_exact_versions`; `webview_runtime_drift_does_not_apply_three_component_semver_rules`; `webview_runtime_drift_projects_unavailable_or_blank_as_unknown`. |
| Unknown fails open with diagnostics | Pass | `webview_runtime_drift_projects_unavailable_or_blank_as_unknown`; `webview_runtime_query_error_diagnostic_is_sanitized`; `conversation_window_preflight_checks_drift_after_focus_miss_before_insert`. |
| Focus-existing bypasses drift | Pass | `conversation_window_preflight_existing_focus_bypasses_drift_and_creation`. |
| Drift is after focus and before insert/create | Pass | `conversation_window_preflight_checks_drift_after_focus_miss_before_insert`; `conversation_window_preflight_changed_runtime_stops_before_insert_and_create`. |
| Stable Rust/TS wire key | Pass | `conversation_popout_runtime_restart_key_has_stable_wire_value`; `conversation_popout_runtime_restart_error_uses_exact_typed_contract`; `locks the runtime restart wire key to the Rust literal`. |
| Changed rejects without backend state/window | Pass | `conversation_window_preflight_changed_runtime_stops_before_insert_and_create`; command source-order audit. |
| Drift clears fence without compensation | Pass | `clears only the transfer fence for runtime drift`. |
| Generic compensation and pure web stay unchanged | Pass | `keeps generic open failures on the compensation path`; `opens once, keeps the main tab, and enters no desktop handoff stage`. |
| Persistent fixed-ID restart toast | Pass | `uses one persistent fixed-id toast and does not relaunch automatically`. |
| Plain relaunch only; rejection stays in action | Pass | `calls plain relaunch only from the action`; `catches relaunch rejection inside the action and reports failure`; forbidden-symbol source audit. |
| Both tab and sidebar producers | Pass | `uses the shared runtime gate and failure notifier`; `routes runtime restart rejection through the shared notifier`. |
| Ten localized catalogs | Pass | `defines restart-required pop-out copy in all ten locales`. |
| No automatic restart or active-work inspection | Pass | `uses one persistent fixed-id toast and does not relaunch automatically`; forbidden-symbol source audit. |
| TOCTOU race remains on existing fallback | Pass | Source-order audit plus `keeps generic open failures on the compensation path`. |

## Residuals And Admission Status

- The accepted preflight/build TOCTOU interval remains; existing window
  diagnostics, ready timeout, and generic compensation cover observed build
  failures.
- Human Windows Runtime-drift acceptance is deferred to Post-Delivery Residual
  Work.
- No automatic restart, task-idle wait, Runtime listener, fixed Runtime bundle,
  or auxiliary-window gate was added.
- Aggregate Rust test admission is not fully green because of the deterministic
  unrelated stale watchdog default assertion described above. All WebView2
  focused tests, all frontend verification, every compile gate, strict Clippy
  surface, and the local non-Windows proof pass.
- Independent Task 6 Codex and Grok review is pending. Both must review the same
  report commit HEAD; this integrator did not spawn them as instructed.

## Conclusion

done_with_concerns
