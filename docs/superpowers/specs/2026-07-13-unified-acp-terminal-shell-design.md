# Unified ACP Terminal Shell

## Goal

Make the existing system default-shell setting the single source of truth for
both interactive terminal tabs and every ACP agent's client-side
`terminal/create` execution.

The selected shell must be an execution fact, not merely a UI preference. Codeg
must also tell agents which command dialect to generate, while remaining
compatible with protocol-conforming agents and agents that place a complete
shell line in `CreateTerminalRequest.command` with an empty `args` array.

## Background

The current setting controls only terminal tabs. ACP terminal execution instead
uses an independent runtime whose Windows shell fallback is hard-coded to
`COMSPEC`. The UI explicitly says the setting does not affect agent tools.

The current ACP runtime first passes the entire `request.command` string to
`Command::new`. It retries through a platform shell only when that direct spawn
returns `NotFound`, `args` is empty, and the command contains whitespace. On
Windows, a complete command containing an absolute path such as `D:\...` can
return `ERROR_INVALID_NAME` (OS error 123) instead of `NotFound`, so the shell
fallback never runs. Single-word shell builtins also miss the whitespace gate.

Agents can additionally generate the wrong command dialect because ACP 0.11 has
no standard field that declares the client's terminal shell. Its
`CreateTerminalRequest` contains `command`, `args`, `env`, `cwd`, and `_meta`,
but no shell or dialect property.

## Approved Product Decisions

- Keep one global shell setting. Do not add a separate agent-shell setting.
- The setting controls new interactive terminal tabs and ACP terminal
  execution.
- Resolve and snapshot the shell when an ACP connection is created.
- A setting change affects new terminal tabs, new ACP connections, and manually
  reconnected ACP sessions.
- Existing terminal tabs and running ACP connections retain their original
  shell.
- An internal automatic reconnect of the same logical connection retains its
  existing shell snapshot.
- If an explicitly selected shell cannot be resolved, block new ACP connections
  with a localized error. Do not silently select another dialect.
- The `system` selection may continue using the existing platform fallback
  chain.
- Codeg may append a hidden terminal-context block to the first prompt sent on
  each new or manually reconnected ACP connection.
- Agent-specific adapters are an evidence-driven exception mechanism, not the
  primary architecture.

## Non-Goals

- Translating command text between CMD, PowerShell, and POSIX dialects.
- Retrying an already-started command through a different shell.
- Giving each ACP agent a separate user-facing shell preference.
- Rewriting protocol-conforming `command + args` requests into shell text.
- Adding agent-specific branches before a real incompatibility is observed.

## Alternatives Considered

### Prompt-only declaration

Inject environment variables or instructions but leave terminal execution
unchanged. This is small, but an agent may ignore the declaration and it does
not fix Windows spawn error 123. Rejected.

### Per-agent terminal implementations

Maintain separate Grok, Claude, Codex, CodeBuddy, and other execution paths.
This can optimize known agents but grows into an agent-by-platform-by-shell
matrix and leaves new ACP implementations unsupported by default. Rejected as
the primary architecture.

### Unified executor with layered declaration

Resolve one shell specification, make the ACP terminal runtime execute complete
command lines through that shell, and declare the dialect through environment,
ACP metadata, and a hidden prompt block. Retain a narrow optional adapter hook
for proven agent quirks. This is the selected approach.

## Architecture

### Resolved shell specification

Introduce one backend-owned value used by both terminal subsystems:

```text
ResolvedShellSpec
  executable       concrete executable or resolved command
  dialect          cmd | powershell | posix | custom
  display_name     stable user-facing name
  source           system | explicit | custom
  command_strategy arguments/environment needed to execute one command line
```

Resolution belongs in a shared shell module rather than the interactive
terminal manager or ACP runtime. The module owns:

- system fallback resolution;
- explicit and custom path validation;
- Windows and POSIX shell-flavor detection;
- non-interactive command-line invocation arguments;
- UTF-8-related environment and startup commands.

The interactive terminal manager may add interactive/login flags, but it must
derive them from the same resolved specification and flavor detection.

### Connection snapshot

Before an ACP process is spawned, Codeg loads the persisted terminal setting,
resolves it, and fails preflight when an explicit shell is unavailable. The
resulting `ResolvedShellSpec` is stored with the connection and passed to:

- the agent launch environment;
- the ACP `TerminalRuntime`;
- ACP initialization and all session-establishment metadata;
- the first-prompt context injector;
- the connection configuration fingerprint.

The runtime never rereads global shell settings. This keeps the declaration and
actual executor stable for the full connection lifetime.

Changing the setting marks running ACP connections stale through the existing
configuration-fingerprint mechanism. The frontend tells the user that a manual
reconnect is required. An automatic transport reconnect retaining the same
logical connection also retains the shell snapshot.

### ACP request classification

Classify requests deterministically before spawning:

1. When `args` is non-empty, execute `command` directly with exactly those
   arguments. This is the ACP-native path and preserves argument boundaries.
2. When `args` is empty and `command` resolves exactly to an existing executable
   in the effective cwd/PATH context, execute it directly. This preserves real
   executable paths containing spaces.
3. Otherwise, treat `command` as a complete command line and execute it through
   the connection's selected shell.

The effective cwd is the explicit absolute ACP `cwd`, otherwise the session
working directory. Resolution must use the same cwd and effective PATH that the
eventual process will receive.

This removes the current OS-error-driven fallback. A Windows absolute path in a
complete command line is never passed to `Command::new` as if the entire line
were an executable name.

### Shell invocation

Known shell flavors use explicit non-interactive command strategies:

- CMD: `/D /S /C`, with UTF-8 code page setup consistent with interactive tabs.
- PowerShell and pwsh: `-NoLogo -NoProfile -NonInteractive -Command`, with UTF-8
  output setup.
- POSIX shells: the flavor-appropriate non-interactive command option, using a
  login mode only where the existing terminal behavior requires it.
- Custom/unknown shells: a validated generic command strategy. If Codeg cannot
  determine one, connection preflight fails with `terminal_shell_unsupported`.

`COMSPEC` keeps its operating-system meaning on Windows and is not overwritten
with PowerShell. Codeg-specific variables and `SHELL` carry the selected
preference instead.

## Agent Declaration

Codeg declares the connection snapshot through three channels.

### Launch environment

The ACP process receives:

```text
SHELL=<resolved executable>
CODEG_TERMINAL_SHELL=<resolved executable>
CODEG_TERMINAL_DIALECT=<cmd|powershell|posix|custom>
```

These values are added to the agent runtime environment before launch. They do
not expose unrelated agent credentials to terminal commands.

### ACP extension metadata

Add namespaced metadata to `initialize` and every applicable session
establishment request: `session/new`, `session/load`, `session/resume`, and
`session/fork`:

```json
{
  "codeg.dev/terminal": {
    "shell": "pwsh.exe",
    "dialect": "powershell",
    "platform": "windows",
    "commandMode": "selected-shell-for-command-lines"
  }
}
```

ACP implementations may ignore unknown metadata, so this is a cooperative
machine-readable declaration rather than the only delivery mechanism.

### Hidden prompt context

Append a separate text content block to the first prompt sent on each new or
manually reconnected ACP connection:

```text
<codeg_terminal_context version="1">
Selected shell: PowerShell 7
Dialect: powershell
Generate shell command lines using PowerShell syntax.
ACP command+args requests may still execute directly.
This context is authoritative for the current connection and supersedes
earlier terminal context records.
</codeg_terminal_context>
```

The context is generated from the connection snapshot, never from a fresh
global-settings read during a prompt. It is appended to the wire request only;
the live `UserMessage` event continues to contain the original user blocks.

On a resumed conversation, the new block explicitly supersedes terminal
contexts stored by older connections. It may remain in an agent's native
transcript, but Codeg must never render it as user content or use it as a title.

## History Hygiene

Add a shared parser helper that recognizes only the complete, versioned
`codeg_terminal_context` envelope generated by Codeg. Apply it to user text
extraction before:

- constructing visible user content blocks;
- selecting the first real user message;
- generating conversation titles and summaries.

The helper must support a context-only block and a context appended to otherwise
real text. Malformed, partial, or ordinary user XML remains visible. Parser
fixtures cover every supported agent transcript shape so the hidden context does
not leak through history reloads.

## ACP Terminal Adapter Extension

Provide a narrow generic adapter boundary. Its responsibilities are limited to:

- adding an agent-native declaration when that agent cannot consume the generic
  environment, metadata, or context;
- normalizing a documented non-standard terminal request shape;
- rejecting a proven incompatible agent/shell combination before execution.

The adapter cannot override the selected `ResolvedShellSpec`, switch shells,
translate command text, or retry a command. All agents use the generic adapter
until a real incompatibility justifies a dedicated implementation.

## Settings UX

Keep the existing default-shell picker and stored `default_shell` field. Update
the description in every supported locale to state that the setting controls:

- new terminal tabs;
- new ACP agent tool execution;
- manually reconnected ACP sessions.

The UI also states that running terminals and ACP connections are unchanged.
When the selected explicit shell is unavailable, the existing picker may retain
the value and show its warning, but a new ACP connection is blocked with a clear
localized preflight error.

The displayed effective shell must reflect the selected value, not only the
system-default fallback. No agent-specific shell setting is added.

## Error Model

Use stable error codes suitable for frontend localization:

- `terminal_shell_unavailable`: an explicit shell cannot be resolved;
- `terminal_shell_unsupported`: Codeg cannot determine how to execute a command
  line through the selected custom shell;
- `terminal_shell_spawn_failed`: the selected shell itself cannot start;
- `terminal_program_spawn_failed`: a protocol-native direct executable cannot
  start.

Diagnostics include the shell display name, executable path, execution mode,
and original OS error. They must not include the full launch environment or
credentials.

Once a shell or direct program starts, syntax errors and non-zero exits are
normal terminal results. Codeg does not translate the command or retry it in a
different shell. This prevents mutating commands from executing twice.

## Windows Test Executable Manifest

The Windows desktop test executable currently imports
`comctl32!TaskDialogIndirect` through the default `tauri-runtime` feature but has
no embedded manifest requesting Common Controls v6. Windows therefore loads the
v5.82 library, which lacks that entry point, and exits with
`0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND` before running tests.

As a verification prerequisite, add a narrowly scoped Windows test-link setup
that embeds the Common Controls v6 manifest dependency in Rust test executables.
This is not part of shell selection at runtime, but it is required for the
repository-mandated `cargo test --features test-utils` command to execute.

## Testing

### Shell resolution

- Resolve system default, CMD, Windows PowerShell, PowerShell 7, POSIX shells,
  and custom paths.
- Block a new connection for an unavailable explicit shell.
- Reject a custom shell without a supported command strategy.
- Verify a setting update changes new snapshots but not running connections.

### Request classification and execution

- Preserve direct `command + args` boundaries.
- Run a complete command line through the selected shell.
- Run a single-word shell builtin through the selected shell.
- Directly execute a real executable whose path contains spaces.
- Cover Windows absolute paths, quotes, pipelines, redirects, and `&&`.
- Cover the Grok regression commands using PowerShell `Set-Location` and
  `Get-Location`, CMD `cd /d` and `where`, and a Cargo manifest path containing
  `D:\...`.
- Assert that Windows error 123 is not produced for complete command lines.

### Declaration lifecycle

- Propagate environment variables to the ACP process.
- Attach correct metadata to `initialize`, `session/new`, `session/load`,
  `session/resume`, and `session/fork` when each path is used.
- Append hidden context once per connection attachment.
- Use a new shell after manual reconnect.
- Retain the old snapshot on an internal automatic reconnect.
- Make resumed-session context supersede prior declarations.

### History and safety

- Keep hidden context out of live user events, history, titles, and summaries.
- Exercise every supported agent parser with its native transcript shape.
- Preserve ordinary and malformed user XML.
- Run a marker-writing command that exits non-zero and assert it executes once.
- Verify diagnostics do not contain credential environment variables.

### Required verification

Run focused frontend tests for settings copy/state and the relevant parser and
ACP backend tests. Then run the repository checks appropriate to the changed
shared behavior:

```bash
pnpm eslint .
pnpm test
pnpm build
```

From `src-tauri/`:

```bash
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

## Rollout and Compatibility

The persisted settings schema does not change. Existing users keep their
selected shell, but its documented scope expands to ACP execution. Users with
an unavailable explicit shell can continue seeing that stored choice, while new
ACP connections fail clearly until they select or install a valid shell.

Protocol-conforming agents continue to use direct executable requests. Agents
that send complete command lines gain deterministic selected-shell behavior.
Unknown agents receive the same generic declaration and executor without a
registration change. Dedicated adapters are added only in response to observed
incompatibility.
