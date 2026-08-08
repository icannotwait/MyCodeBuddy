# Window Creation Diagnostics Design

## Status

Direction approved in the 2026-08-08 design discussion; this written spec is
pending user review.

## Incident

DrawCode intermittently fails to open conversation pop-outs and other
independent windows such as Settings. The frontend reports a generic failure,
while the durable application log only contains the terminal Tauri/Wry error:

```text
failed to create webview: WebView2 error: WindowsError(
  Error {
    code: HRESULT(0x80010108),
    message: "The object invoked has disconnected from its clients."
  }
)
```

The failure is therefore below the conversation handoff state machine. It
occurs while `WebviewWindowBuilder::build()` creates a second WebView2-backed
window.

The evidence also rules out application uptime as the sole cause:

- DrawCode and its WebView2 browser process restarted at 2026-08-08 02:10
  local time.
- Auxiliary-window creation failed twice at 10:53 and 10:54 with the same
  `0x80010108` result.
- At 09:33, Windows Error Reporting recorded a failed Edge/WebView2 update to
  `151.0.4129.72`; the running DrawCode process was using
  `151.0.4129.59`.
- The update failure and window failures are correlated, but the available
  evidence does not prove causation.

The current application logs do not identify which window request produced a
Tauri runtime error, what WebView2/runtime configuration was active, whether
other runtime-created windows succeeded nearby, or where to find WebView2's
own diagnostic output.

## Decision

Add a shared diagnostic boundary around every application-owned runtime
`WebviewWindowBuilder::build()` call. This includes the main window created in
Tauri's setup hook, which provides a launch-time control sample for later
auxiliary-window attempts.

The boundary emits low-volume structured application events by default. An
explicit `CODEG_WEBVIEW_DEBUG=1` startup option additionally enables WebView2's
internal Chromium log in an application-owned diagnostics file.

This change is diagnostic only:

- it does not retry a failed window build;
- it does not restart DrawCode or WebView2;
- it does not alter conversation pop-out compensation;
- it does not change the error returned to the frontend; and
- it does not patch or replace Tauri, Wry, or WebView2.

## Goals

- Correlate each application-owned runtime window request with its exact
  success or failure.
- Distinguish conversation pop-outs from Settings, import, Git, remote
  workspace, pet, and other runtime-created windows.
- Capture enough runtime context to compare successful and failed attempts.
- Record WebView2 version and effective startup options on every application
  launch.
- Provide an opt-in WebView2 internal log for cases where Wry exposes only an
  HRESULT.
- Keep default log volume small and avoid user content and credentials.
- Preserve all current window and pop-out behavior.

## Non-Goals

- Fixing or retrying `RPC_E_DISCONNECTED` in this change.
- Treating the failed Edge updater event as a proven root cause.
- Adding a user-facing diagnostics settings panel.
- Uploading diagnostics automatically.
- Reading Windows Event Logs or Edge installer logs from inside DrawCode.
- Registering low-level WebView2 COM `ProcessFailed` handlers in the first
  iteration.
- Logging complete window URLs, titles, user/content filesystem paths,
  prompts, tokens, or remote credentials. The opt-in app-owned WebView2 log
  path is reported so an operator can find the requested diagnostic output.

## Considered Approaches

### 1. Pop-out-only logging

This is the smallest edit, but it cannot explain why Settings and other
independent windows fail at the same time. It also cannot provide successful
control samples from other window types. This approach is rejected.

### 2. Shared structured diagnostics plus opt-in WebView2 logs

This covers the common `WebviewWindowBuilder::build()` boundary, associates the
otherwise anonymous Tauri/Wry error with a request, and provides deeper runtime
logs only when explicitly enabled. It has low default overhead and does not
change recovery behavior. This is the selected approach.

### 3. Direct WebView2 COM process-failure hooks

Registering `ICoreWebView2::ProcessFailed` handlers could expose process kind,
exit code, and failure reason. It requires Windows-only COM bindings and
couples application code to WebView2 interfaces below Tauri/Wry. It is deferred
until the selected approach demonstrates that those fields are necessary.

## Architecture

Add a desktop-only module:

```text
src-tauri/src/window_diagnostics.rs
```

It owns three responsibilities:

1. process-start WebView runtime diagnostics;
2. one correlated attempt object around each runtime window build; and
3. opt-in Windows WebView2 browser-argument and log-file configuration.

Correlated window-attempt records are emitted on every desktop platform. The
WebView2 argument and Chromium-log configuration is compiled and applied only
on Windows; other platforms report their available webview version without
setting WebView2-specific environment variables.

The request flow becomes:

```text
Tauri setup hook or command
  -> construct WebviewWindowBuilder
  -> WindowCreationAttempt::begin(...)
       -> window_create_started
  -> builder.build()
       -> success: window_create_succeeded
       -> failure: window_create_failed
  -> existing success/error handling continues unchanged
```

The module uses a process-local atomic sequence to generate IDs such as
`window-<pid>-<sequence>`. It does not add a UUID dependency or persist mutable
state.

## Coverage

The first implementation wraps application-owned runtime window creation in:

- `src-tauri/src/lib.rs`;
- `src-tauri/src/commands/conversation_popout.rs`;
- `src-tauri/src/commands/windows.rs`; and
- `src-tauri/src/commands/remote_workspace.rs`.

This includes the setup-created main window plus conversation, Settings,
import, commit, merge, stash, push, project boot, pet, pet panel, and remote
workspace windows. Test-only builders are excluded. The Tauri configuration
currently declares no static windows (`app.windows` is empty), so there is no
configuration-created window to wrap.

## Structured Events

All events use the tracing target `codeg::window` and stable event names. The
runtime snapshot, start, and success records use `INFO`; failures use `ERROR`;
abandoned attempts use `WARN`. `window_kind` is an allowlisted static value,
not caller-provided text.

### `webview_runtime_snapshot`

Emitted once during desktop startup after logging is initialized and after
DrawCode has resolved its rendering/debug preferences, but before the Tauri
builder creates any webview.

Fields:

- `app_version`;
- `app_pid`;
- `webview_version` or `webview_version_error`;
- `disable_hardware_acceleration`;
- `webview_debug_enabled`;
- safe, recognized WebView2 switches such as `disable_gpu`, `enable_logging`,
  and verbosity level;
- presence, but not the value, of path/channel override variables; and
- `webview_log_path` only when opt-in logging is enabled. This app-owned
  diagnostics path is the sole intentional exception to the rule against
  logging filesystem paths.

### `window_create_started`

Fields:

- `attempt_id`;
- `window_kind`;
- `window_label`;
- `caller_label` when available;
- `operation_id` for a conversation pop-out;
- `app_pid`;
- `app_uptime_ms`;
- `registered_window_count`; and
- sorted `registered_window_labels`.

Window labels are application-generated identifiers. Titles and URLs are not
logged.

### `window_create_succeeded`

Fields:

- `attempt_id`;
- `window_kind`;
- `window_label`; and
- `elapsed_ms`.

### `window_create_failed`

Fields:

- all stable identity fields from `window_create_started`;
- `elapsed_ms`;
- `failure_kind`;
- the error's display representation;
- its debug representation only when it differs from the display form;
- `webview_version` from the current snapshot; and
- current safe WebView2 switch state.

Known HRESULTs are classified without changing the original error. At minimum,
`0x80010108` maps to `rpc_disconnected`; unknown failures map to `unknown`.

The attempt object records exactly one terminal event. Dropping an unfinished
attempt emits `window_create_abandoned`, which protects against future early
returns introduced between `begin` and `finish`.

### `window_create_abandoned`

Fields:

- the stable identity fields captured by `window_create_started`; and
- `elapsed_ms`.

## WebView2 Internal Logging

On Windows, `CODEG_WEBVIEW_DEBUG` enables diagnostics when its trimmed value is
`1` or an ASCII case-insensitive `true`. Other values leave diagnostics off.
When enabled, DrawCode configures the existing
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` startup boundary with:

```text
--enable-logging --v=1 --log-file=<application log path>
```

The options are merged before any Tauri plugin or async worker starts, using
the same ordering constraint as the existing `--disable-gpu` preference. When
debugging is enabled, DrawCode owns the three logging switches: it keeps one
`--enable-logging`, normalizes verbosity to `--v=1`, and replaces any existing
`--log-file` with the current process's app-owned path. It preserves every
unrelated caller-provided argument value, avoids duplicate recognized
switches, and quotes/escapes the generated log path according to Chromium
command-line rules so spaces in the logs directory remain valid. When the
debug option is disabled, existing caller-provided logging switches are not
modified.

Each application process writes to a unique file:

```text
<codeg logs root>/webview2-<pid>.log
```

Startup prunes only files strictly matching DrawCode's own
`webview2-<pid>.log` naming pattern. After reserving the current target it
retains at most five matching files in total, ordered by last modification
time, and never deletes the current target. Failure to create, configure, or
prune these files is logged but never blocks startup.

WebView2 internal logs may contain local navigation URLs and runtime details.
They are therefore opt-in, local-only, and never included in ordinary export
or upload behavior automatically.

## Rendering Argument Handling

The current `--disable-gpu` preference and debug switches must be composed in
one startup function so all WebView2 environments created by the process see
identical options. Per-window browser arguments are forbidden: WebView2 can
reject environments that share a user-data directory but use different
environment options.

The structured startup event reports only recognized switches. Arbitrary
caller-provided argument values are not copied into durable logs.

## Error and Privacy Boundaries

- Existing `AppCommandError` values and messages remain unchanged.
- Conversation pop-out operation insertion and rollback stay in their current
  order.
- A diagnostic logging failure cannot turn a successful window build into an
  application failure.
- A diagnostics failure is reported once through the normal tracing pipeline.
- Full application routes are excluded because query parameters may contain
  identifiers or future sensitive fields.
- Window titles are excluded because they can contain conversation titles or
  repository names.
- Error fields contain only the original Tauri/Wry build error; application
  URLs, titles, and paths are never appended as diagnostic context.
- Environment variables are allowlisted and summarized; raw environment dumps
  are forbidden.

## Testing

Implementation follows test-driven development.

Pure unit tests cover:

- monotonic attempt ID formatting;
- stable HRESULT classification, including `0x80010108`;
- safe environment snapshot redaction;
- WebView2 argument merging with existing arguments;
- duplicate-switch prevention;
- debug log-path construction and strict retention filename matching; and
- success, failure, and abandoned attempt terminal-state selection through a
  recording diagnostic sink.

Behavioral tests use an injected build closure rather than constructing a real
WebView2 controller. They prove that the wrapper returns the original success
value or original error unchanged and that logging failures are non-fatal.

Targeted verification includes:

```text
cargo test --lib --features test-utils window_diagnostics
cargo check
cargo clippy --all-targets --features test-utils -- -D warnings
```

Manual verification on Windows uses two launches:

1. normal launch: open Settings and a conversation pop-out, then verify the
   correlated default events and absence of a WebView2 internal log;
2. `CODEG_WEBVIEW_DEBUG=1`: repeat the operations and verify the startup event,
   correlated attempts, and the reported WebView2 log file.

An actual `RPC_E_DISCONNECTED` failure is not required for automated tests.
The next natural occurrence should provide the evidence needed for a separate
root-cause fix.

## Diagnostic Interpretation

The resulting evidence supports the following distinctions:

- all window kinds fail with the same version and options: shared runtime or
  WebView2 environment failure;
- only one window kind fails: application-specific builder/configuration path;
- a runtime version changes between successful and failed launches: updater or
  runtime-version correlation;
- WebView2 logs report browser/renderer/process termination: candidate for a
  direct `ProcessFailed` handler or runtime recovery design;
- environment options differ: startup argument composition defect; and
- Wry reports `rpc_disconnected` without a corresponding WebView2 internal
  event: candidate Tauri/Wry COM lifecycle issue.

## Follow-Up Boundary

This design ends after producing a correlated, privacy-safe evidence chain. A
retry, process recovery, runtime pinning, Edge updater workaround, or direct
COM event integration requires a separate root-cause design based on the new
logs.
