# Task 2 Report: Gate Conversation Pop-out Creation on Runtime Drift

## Status

**done_with_concerns - implementation complete; independent Codex and Grok review pending**

- Work unit: `task|2|implementer|codex|none`
- Scope: backend conversation pop-out preflight, stable Rust error key, and
  ordered recording-fake coverage only
- Baseline HEAD: `2bcc5e5ee2a4a57beaaaa931ddae20e456c5b5e0`
- Task 1 producer: `4161d398da22863a45cafac4bb0e5e6e46377606`
- Producer commit: `4f5a7136ad87761f801564b09fdf13fb977f9926`
- Frontend work: not started, as required by the Task 2 scope

## Implementation

- Added the stable Rust wire key
  `CONVERSATION_POPOUT_RUNTIME_RESTART_REQUIRED_I18N_KEY` with the exact value
  `ConversationPopout.runtimeRestartRequired`.
- Added the typed restart-required error producer using
  `AppErrorCode::WindowOperationFailed`, the stable key, and no i18n params.
- Replaced the obsolete test-only open/create behavioral model with the
  production-used `decide_conversation_window_preflight` seam.
- Added `ConversationWindowCreatePermit`; only `Unchanged` and `Unknown`
  runtime drift issue a permit, and production insertion consumes it.
- Preserved the existing focus-first behavior: an existing window is
  unminimized and focused without checking drift or doing create-new work.
- Integrated the Task 1 runtime query and blocked-pop-out diagnostic before
  database/title/URL resolution, operation insertion, and window creation.
- Preserved the existing build-failure tombstone and best-effort focus after a
  successful build.

## TDD Evidence

The stable-key, typed-error, and ordered preflight tests were added before the
production implementation. Each of the three required focused commands was run
in the red state. Rust compilation failed for the expected missing constant,
error producer, drift trait method, create permit, preflight enum, and decision
function while the old trait still required its obsolete insert/create model.

After the minimal production implementation, the same filters passed. The
recording fake uses one event log and injectable drift, proving focus bypass,
focus-miss/drift/insert/create order, `Unknown` fail-open behavior, and
`Changed` rejection before insertion or creation.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils conversation_popout_runtime_restart_key -- --nocapture`
  - Pass: 1 passed, 0 failed, 4,295 filtered out.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils conversation_popout_runtime_restart_error -- --nocapture`
  - Pass: 1 passed, 0 failed, 4,295 filtered out.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils conversation_window_preflight -- --nocapture`
  - Pass: 3 passed, 0 failed, 4,293 filtered out.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils conversation_popout -- --nocapture`
  - Pass: 55 passed, 0 failed, 4,241 filtered out.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass: exit 0.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings`
  - Pass: exit 0.
- `rustfmt --edition 2021 --check src-tauri/src/app_error.rs src-tauri/src/commands/conversation_popout.rs`
  - Pass: exit 0.
- Source-order review and `git diff --check`
  - Pass: validation, label, preflight, database lookup, permit-consuming
    insertion, then build; only the two approved Rust source files are in the
    producer commit.

## Commits

- `4f5a7136ad87761f801564b09fdf13fb977f9926` -
  `fix: gate pop-out creation on WebView2 drift`

## Concerns

- Independent Codex and Grok review is pending before Task 3 admission.
- Repository-wide `cargo fmt --check` reports pre-existing formatting drift in
  unrelated Rust files; scoped Rustfmt for both Task 2 files passes.
- The existing Tauri build script warns that the local `codeg-mcp` sidecar is a
  zero-byte placeholder. This warning is outside the producer diff.

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Gated backend conversation pop-out creation behind a focus-first WebView2 drift preflight with a permit-enforced insert order and stable typed restart-required error contract.","commits":[{"sha":"4f5a7136ad87761f801564b09fdf13fb977f9926","subject":"fix: gate pop-out creation on WebView2 drift"}],"tests":{"status":"passed","passed":55,"failed":0,"summary":"All focused wire/error/order filters and the 55-test conversation-popout suite passed, followed by desktop cargo check, strict all-target Clippy, scoped Rustfmt, source-order review, and diff checks."},"concerns":["Independent Codex and Grok review is pending before Task 3.","Repository-wide cargo fmt --check has unrelated existing drift; scoped Task 2 Rustfmt passes.","The existing zero-byte codeg-mcp sidecar warning remains outside this diff."],"report_file":".superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-2-report.md"}
-->
