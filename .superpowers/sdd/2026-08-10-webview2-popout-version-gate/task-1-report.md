# Task 1 Report: Project Windows WebView2 Runtime Drift

## Status

**done_with_concerns - implementation complete; independent Codex and Grok review pending**

- Work unit: `task|1|implementer|codex|none`
- Scope: WebView2 Runtime drift projection in `window_diagnostics` only
- Baseline HEAD: `7891c0f794d58275bae5e30684460d81042a6f52`
- Producer commit: `4161d398da22863a45cafac4bb0e5e6e46377606`
- Task 2 command wiring: not started, as required by the Task 1 scope

## Implementation

- Added `WebviewRuntimeDrift` with `Unchanged`, `Changed`, and `Unknown`
  outcomes.
- Added trimmed exact-string projection for startup and currently available
  four-component WebView2 Runtime versions without SemVer parsing.
- Added a Windows-only fresh `tauri::webview_version()` query using the
  immutable process-start snapshot as its baseline.
- Added the non-Windows `Unchanged` implementation seam with no query input or
  query call.
- Added sanitized diagnostics for fresh-query failures and the blocked-pop-out
  diagnostic emitter consumed by Task 2.
- Left `initialize`, `open_conversation_window`, dependencies, window creation,
  and operation records unchanged.

## TDD Evidence

The five specified unit tests were added before production code. After creating
the ignored `out/` directory required by the existing Tauri build script, the
focused red command failed compilation with the expected unresolved
`WebviewRuntimeDrift`, projector, platform-helper, and sanitizer symbols.

The first green compile exposed one type mismatch in the approved snippet:
`Result::as_deref().err()` returns `Option<&String>`. The implementation applies
the minimal `String::as_str` conversion before sanitization. The focused drift
matrix then passed, and the sanitizer test passed under its explicit filter.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils webview_runtime_drift -- --nocapture`
  - Pass: 4 passed, 0 failed, 4,289 filtered out.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils webview_runtime_query_error_diagnostic_is_sanitized -- --nocapture`
  - Pass: 1 passed, 0 failed, 4,292 filtered out.
- `cargo check --manifest-path src-tauri/Cargo.toml`
  - Pass: exit 0.
- `cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --features server --bin codeg-server`
  - Pass: exit 0.
- `rustfmt --edition 2021 --check src-tauri/src/window_diagnostics.rs`
  - Pass: exit 0.
- Source allowlist, query-boundary review, and `git diff --check`
  - Pass: producer commit changes only `src-tauri/src/window_diagnostics.rs`;
    the fresh query is within `#[cfg(windows)]`, and the non-Windows helper has
    no query call.

## Commits

- `4161d398da22863a45cafac4bb0e5e6e46377606` -
  `feat: project WebView2 runtime drift`

## Concerns

- Independent Codex and Grok review is pending before Task 2 admission.
- The plan's `webview_runtime_drift` test filter does not select
  `webview_runtime_query_error_diagnostic_is_sanitized`; it was therefore run
  explicitly as a second focused command.
- Until Task 2 consumes `current_webview_runtime_drift` and
  `emit_webview_runtime_drift_blocked_popout`, Rust emits expected temporary
  dead-code warnings for the new shared interfaces.
- The existing Tauri build script warns that the local `codeg-mcp` sidecar is a
  zero-byte placeholder. This warning is outside the producer diff.

## Conclusion

done_with_concerns

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added trimmed exact WebView2 Runtime drift projection, a Windows-only fresh query, fail-open non-Windows behavior, sanitized query diagnostics, and blocked-pop-out diagnostics without wiring Task 2.","commits":[{"sha":"4161d398da22863a45cafac4bb0e5e6e46377606","subject":"feat: project WebView2 runtime drift"}],"tests":{"status":"passed","passed":5,"failed":0,"summary":"Four drift/non-Windows tests and the separately filtered sanitizer test passed; desktop and server cargo checks, rustfmt, source allowlist, and diff checks passed."},"concerns":["Independent Codex and Grok review is pending before Task 2.","The approved drift filter omits the sanitizer test, which was run separately.","Task 1 alone emits temporary dead-code warnings until Task 2 consumes the new interfaces.","The existing zero-byte codeg-mcp sidecar warning remains outside this diff."],"report_file":".superpowers/sdd/2026-08-10-webview2-popout-version-gate/task-1-report.md"}
-->
