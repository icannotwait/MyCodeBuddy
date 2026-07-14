# Codex CLI Hybrid Model Control Plane

## Context

MyCodeBuddy now injects `CODEX_ACP_USE_CLI=1` into the Codex ACP
distribution environment by default. The customized Windows adapter currently
treats that flag as a complete runtime cutover: it skips app-server
initialization, exposes one synthetic model, and executes turns with
`codex exec --json`.

The desired behavior is narrower. CLI mode should replace turn execution, but
the existing app-server model discovery experience must remain available. The
selected model and reasoning effort must be forwarded to each CLI turn.

Separately, preflight currently checks for a host Codex CLI only on Windows.
On macOS and Linux, the distribution default enables CLI mode after preflight,
so preflight can pass even though launch later rejects the missing host CLI.

## Scope

### Goals

- Keep `CODEX_ACP_USE_CLI=1` as the default distribution environment value.
- On the customized Windows bundled adapter, use app-server as a control plane
  for initialization, configuration defaults, and the complete model list.
- Continue using the existing CLI runtime for session storage and turn
  execution.
- Pass the selected model and reasoning effort to `codex exec`.
- Persist the last selected model and reasoning effort with the CLI session so
  load/resume restores the selection when it remains valid.
- Make preflight derive host-Codex requirements from the same effective
  distribution-plus-user environment used at launch.

### Non-goals

- Do not change the official Npx adapter used on non-Windows platforms.
- Do not publish a new Npm package or add non-Windows bundled adapter assets.
- Do not migrate CLI session IDs to app-server thread IDs.
- Do not run turns through app-server while CLI mode is enabled.
- Do not change the behavior of `CODEX_ACP_USE_CLI=0`.

## Architecture

### Windows hybrid adapter

The customized adapter keeps both existing components alive in CLI mode:

- `CodexAppServerClient` is the control plane.
- `CodexCliRuntime` is the execution plane.

`CodexAcpClient.initialize` always initializes app-server. CLI mode no longer
short-circuits initialization. Authentication behavior stays CLI-oriented: the
adapter does not force an additional ACP authentication flow merely because
app-server is available for control-plane reads.

Model discovery always uses the existing paginated app-server `model/list`
path. CLI-specific synthetic `availableModels()` is no longer used to populate
ACP session model options in the customized Windows build.

CLI session creation remains local to `CodexCliRuntime`; it does not call
`thread/start` and therefore does not create an unused app-server thread.

### Initial model selection

For a new CLI session, the adapter resolves the initial selection in this
order:

1. A valid model requested through ACP session metadata.
2. The model and reasoning effort returned by app-server `config/read`, when
   the configured model exists in the discovered list.
3. The first discovered model and that model's default reasoning effort.

An explicit `CODEX_ACP_CLI_MODEL` remains an execution override for backward
compatibility. It does not replace the app-server model list.

### Session persistence

The CLI session map gains an optional wire-stable selected model ID containing
both model and reasoning effort. Older session-map records remain valid.

Before a CLI turn starts, `CodexCliRuntime` records the `ModelId` supplied by
the ACP session state. Once the CLI thread mapping is persisted, that selection
is persisted with it. Load/resume resolves the current selection in this order:

1. A valid model requested by the resume request.
2. The persisted selected model ID, if it is still present in the current model
   list and the effort is supported.
3. The current app-server config default.
4. The first discovered model default.

Invalid or removed persisted selections fail open to the normal default rather
than preventing the session from loading.

### CLI arguments

Every CLI turn continues to pass the selected model with `-m`. When the
selected `ModelId` contains a reasoning effort, the adapter also passes:

```text
-c model_reasoning_effort="<effort>"
```

The existing `CODEX_ACP_CLI_MODEL` override still wins over the selected model
at execution time. Reasoning effort continues to come from ACP session state.

## Preflight Environment Consistency

Preflight constructs the effective Codex launch environment using the same
precedence relevant to the runtime decision:

1. Distribution environment defaults.
2. Saved per-agent runtime environment overrides.

For Codex, a host CLI check is required when either:

- the target platform is Windows, because the bundled adapter always needs a
  host Codex executable; or
- the effective environment enables CLI mode.

The platform-dependent decision is extracted into a pure helper so Windows
tests can cover macOS/Linux behavior without cross-compilation. The production
preflight still reports the existing `Codex CLI` check item and installation
guidance.

## Error Handling

- App-server initialization or model discovery failures remain visible as
  session setup failures, matching the previous app-server model-list behavior.
- An empty model list fails session creation with the existing
  `Codex did not return any models` error.
- A stale persisted model or unsupported effort falls back to the normal
  default selection.
- CLI spawn and turn failures retain their existing error mapping.
- A missing host Codex CLI is reported during preflight instead of being
  deferred to connection launch.

## Distribution And Versioning

The adapter behavior change applies only to the Windows bundled fork in this
iteration. The vendor package revision and the parent registry bundled version
must advance together. The parent repository records the new vendor submodule
commit.

The non-Windows registry remains on the official Npx package. Documentation
must state that preserving the app-server model list in CLI mode is currently a
Windows bundled-adapter capability.

## Testing

Tests are written before production changes and must demonstrate the current
failure before implementation.

Adapter tests cover:

- CLI mode still initializes app-server.
- CLI new/load/resume session metadata uses the paginated app-server model
  list, not the synthetic single-model list.
- App-server config supplies the initial model and reasoning effort.
- ACP model changes reach `codex exec -m`.
- ACP reasoning effort reaches the `model_reasoning_effort` CLI override.
- The selected model ID survives CLI session-map persistence and resume.
- A stale persisted selection falls back safely.
- CLI mode off retains the existing pure app-server behavior.

Parent Rust tests cover:

- Distribution default `CODEX_ACP_USE_CLI=1` makes non-Windows preflight require
  a host Codex CLI.
- User `CODEX_ACP_USE_CLI=0` overrides the distribution default on non-Windows.
- Windows requires the host CLI in either runtime mode.
- Launch and preflight use the same runtime-mode precedence.

Verification includes the focused adapter tests and typecheck, focused Rust
tests, frontend settings tests, and the repository-required Rust checks for the
affected desktop/server targets where practical.
