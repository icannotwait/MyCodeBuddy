# Grok Build ACP Integration Design

Date: 2026-07-10

Status: Revised after design review; pending user approval

## Summary

Add xAI Grok Build as a first-class Codeg agent using Grok's native ACP
server:

```text
grok agent stdio
```

The integration reuses Codeg's shared ACP runtime for sessions, streaming,
tools, permissions, terminal and filesystem requests, MCP forwarding, and
multi-agent delegation. Grok-specific work is limited to:

- managed installation and versioning;
- capability-driven ACP authentication;
- Grok login and credential status;
- Codeg model-provider projection into Grok configuration;
- ACP session-model selection;
- Grok session-history parsing;
- Grok-specific settings and diagnostics.

The implementation must not create a second Grok chat protocol or wrap
headless streaming JSON.

## Goals

1. Install, upgrade, and launch a verified Grok CLI version from Codeg.
2. Support browser OAuth, device-code login, logout, API keys, and Codeg model
   providers.
3. Create and restore Grok ACP sessions with normal Codeg chat behavior.
4. Import sessions created by the official Grok CLI or another ACP client.
5. Support ACP session models, standard permissions, commands, and MCP
   transports.
6. Allow Grok to act as either parent or child in Codeg delegation.
7. Preserve shared official Grok state under `~/.grok`.
8. Keep Claude Code ACP and Codex ACP behavior unchanged.

## Non-Goals

- Reimplementing Grok over `--output-format streaming-json`.
- Implementing Grok's private `x.ai/session/*` fork and rewind APIs.
- Writing Grok-native MCP configuration from Codeg.
- Replacing Grok's permission engine or synthesizing Grok permission modes.
- Copying Grok transcripts into a second Codeg-owned message store.
- Managing every field supported by `~/.grok/config.toml`.

## Official Baseline

The verified stable package at design time is:

```text
package: @xai-official/grok@0.2.93
binary: grok
node: >=20
license: Proprietary
platforms: macOS, Linux, Windows; x64 and arm64
```

The ACP launch contract is:

```text
command: grok
arguments: agent stdio
environment:
  GROK_DISABLE_AUTOUPDATER: "1"
```

Live probing of version `0.2.93` returned:

- protocol version 1;
- `loadSession: true`;
- no standard resume or fork capability;
- text and embedded-context prompts, but no image or audio prompts;
- stdio, HTTP, and SSE MCP support;
- authentication methods `xai.api_key` and `grok.com`, depending on available
  credentials;
- standard available commands including `/compact`, `/always-approve`,
  `/context`, `/session-info`, and `/goal`;
- `session/new.models.currentModelId = "grok-build"` with a non-empty
  `models.availableModels` list;
- no model-category entry in standard `configOptions` in the tested response;
- no standard session permission selector in the tested configuration.

Primary official references:

- <https://docs.x.ai/build/overview>
- <https://docs.x.ai/build/cli/headless-scripting>
- <https://docs.x.ai/build/cli/reference>
- <https://docs.x.ai/build/settings>
- the versioned user guide installed by the official Grok package.

### Protocol and Dependency Decisions

The implementation targets the repository's resolved
`agent-client-protocol-schema 0.11.7`.

- `authenticate`, `AuthenticateRequest`, and the basic agent-owned
  `AuthMethod::Agent` variant are available without
  `unstable_auth_methods`. Do not enable that feature unless a later design
  explicitly adopts the advanced environment-variable or terminal auth
  variants.
- Grok's `models` response field, `SessionModelState`, and
  `session/set_model` are gated by `unstable_session_model`. Add that feature
  to the existing schema dependency alongside the already enabled session
  features.
- Isolate session-model adaptation behind the shared ACP connection and state
  boundaries, cover its wire shape with tests, and re-audit it on every schema
  upgrade or downgrade because the capability is explicitly unstable.
- Add a direct `toml_edit = "0.23"` dependency and use `DocumentMut` for
  localized Grok configuration edits. The existing `toml` deserialize and
  reserialize path cannot preserve comments or unrelated formatting.

## Comparison With Existing Agents

| Area | Claude Code ACP | Codex ACP | Grok ACP |
| --- | --- | --- | --- |
| ACP implementation | Adapter around Claude Agent SDK | Adapter around Codex app-server | Native official CLI server |
| Launch | `claude-agent-acp` | `codex-acp` | `grok agent stdio` |
| Distribution | Pinned npm package | Pinned npm package | Pinned official npm package |
| Authentication | Adapter reads Claude config/env | Adapter reads Codex auth/config | Advertised ACP auth plus official Grok state |
| History | `~/.claude/projects/*.jsonl` | `~/.codex/sessions/**/rollout-*.jsonl` | Per-session directories under `~/.grok/sessions` |
| Special live handling | Claude raw SDK metadata/events | Codex mode, diff, goal normalization | Authentication and unstable session-model support |
| Permission UI | Standard ACP requests | ACP requests plus Codex preset fallback | Standard ACP requests only |

Grok therefore follows the same first-class agent skeleton as Claude and
Codex, but it must not inherit their private compatibility code.

## Architecture

### 1. Agent Registration

Add `AgentType::Grok` and the `"grok"` wire value everywhere agent types are
enumerated:

- Rust model and display implementation;
- ACP registry IDs and all-agent lists;
- parser maps and conversation import;
- frontend types, labels, order, colors, and icons;
- delegation agent lists and defaults;
- automation and settings selectors;
- all locale message files.

Audit every `AgentType` match, including matches with `_ =>` fallbacks that do
not become compiler errors. In particular, `commands/acp.rs::agent_env_keys`
must receive an explicit Grok arm so Grok is not silently assigned OpenAI
environment variable names.

Register Grok as `AgentDistribution::Npx` with:

```text
version: 0.2.93
package: @xai-official/grok@0.2.93
cmd: grok
args: ["agent", "stdio"]
env: [("GROK_DISABLE_AUTOUPDATER", "1")]
node_required: Some("20.0.0")
supports_mcp: true
```

Codeg owns upgrades. Grok's internal updater is disabled for Codeg-launched
processes.

### 2. Generic ACP Authentication Phase

Extend the shared ACP connection lifecycle from:

```text
initialize -> resume -> load -> new
```

to:

```text
initialize -> authenticate-if-required -> resume -> load -> new
```

This is a generic ACP feature, not a Grok-only request embedded throughout the
connection loop.

The authentication policy receives:

- agent type;
- advertised authentication methods;
- saved authentication preference;
- effective runtime credentials.

For Grok:

| Preference | ACP method |
| --- | --- |
| Browser/OIDC/external-provider session | `grok.com` |
| `XAI_API_KEY` | `xai.api_key` |
| Codeg-managed model provider | `xai.api_key` |

Rules:

1. Authentication is gated by an explicit agent allowlist whose initial and
   only member is Grok.
2. Only call a method advertised by the allowlisted agent.
3. If the agent is not allowlisted or no supported method is advertised,
   preserve the current flow.
4. Complete authentication before any session operation.
5. Convert failures into structured authentication errors.
6. Claude Code, Codex, and Kimi retain their existing adapter/config,
   device-code, and gate-token flows. Advertising an auth method alone must
   never route them through the generic phase.

### 3. Shared ACP Runtime

After authentication, Grok uses the existing shared implementations for:

- `session/new`, `session/load`, and capability-gated resume/fork;
- prompt streaming and cancellation;
- tool-call state and output normalization;
- terminal and filesystem client requests;
- `session/request_permission`;
- session commands;
- MCP transport mapping;
- live snapshots and reconnection;
- `codeg-mcp` delegation.

No Grok-specific content conversion is added unless a verified protocol
payload cannot be represented by the existing standard mappings.

## Authentication and Settings

### Credential Ownership

Codeg must not write `~/.grok/auth.json` directly.

Official Grok commands own login state:

```text
grok login --oauth
grok login --device-auth
grok logout
```

`grok login` is the only interactive credential-establishment flow. The ACP
connection lifecycle never starts a browser or device-code login. It calls
`authenticate { methodId }` only to bind credentials that are already
available to that ACP process. If the selected credential is missing, startup
returns a structured authentication-required result with an action that opens
Grok settings.

Codeg stores an API key in the existing agent-setting secret environment as
`XAI_API_KEY`.

Credential resolution must be displayed according to Grok's actual priority:

1. selected model `api_key` or `env_key`;
2. active session token in `~/.grok/auth.json`;
3. `XAI_API_KEY`.

A saved API key must not be reported as effective while an OAuth token has
priority. The settings panel offers an explicit "sign out and use API key"
operation rather than silently deleting login state.

### Authentication Status

Expose a sanitized backend model:

```text
preference:
  oauth | api_key | model_provider

effective_source:
  model_api_key | oauth_session | xai_api_key | none

oauth_present: boolean
api_key_present: boolean
advertised_methods: string[]
diagnostics: sanitized diagnostic code[]
```

Never return token strings, API key values, or raw `auth.json`.
Credential conflicts, invalid files, and externally modified managed
configuration are represented in `diagnostics`, not as an impossible
`effective_source`.

### Login Task

Login is a backend-owned long-running task:

```text
idle
  -> starting
  -> waiting_for_user
  -> authenticated | cancelled | failed | timed_out
```

Requirements:

- one active login per effective Grok home;
- browser OAuth by default for local desktop;
- device-code login by default for server, Docker, and remote workspaces;
- login continues across settings-page navigation;
- cancellation terminates the child process;
- output is sanitized before it reaches logs or frontend events;
- logout uses the official command.

After login succeeds, newly created ACP connections use `grok.com` and call
ACP `authenticate` before the first session operation. Existing failed
connections are reconnected; the login task does not inject credentials into
an already failed session in place.

The official login command does not promise machine-readable output. The
backend therefore streams a sanitized login transcript, extracts a URL and
device code only as a best-effort convenience, and always retains the
sanitized transcript as the fallback UI. Browser OAuth does not depend on
output parsing because the official command opens the browser itself.

Desktop and server routes must call the same core implementation.

### Codeg Model Provider

When a Codeg model provider is bound, manage one reserved TOML table:

```toml
[model.codeg-managed]
model = "<provider model>"
base_url = "<provider API URL>"
name = "Codeg Managed Provider"
env_key = "CODEG_GROK_PROVIDER_API_KEY"
```

Launch environment:

```text
CODEG_GROK_PROVIDER_API_KEY=<secret>
GROK_DEFAULT_MODEL=codeg-managed
```

Properties:

- the secret never appears in TOML;
- per-model `env_key` intentionally outranks OAuth;
- the default applies only to Codeg-launched processes;
- terminal Grok keeps the user's normal default model;
- all unrelated TOML sections and comments are preserved;
- an existing unowned `[model.codeg-managed]` is a conflict, not an overwrite;
- unbinding removes only the Codeg-owned table.

Ownership is tracked outside Grok's TOML because unknown model keys may be
rejected by Grok. The Codeg agent setting stores a canonical snapshot hash of
the last table it wrote. The hash input contains only these normalized managed
fields, in this fixed order:

```text
model
base_url
name
env_key
```

Comments, key order, whitespace, unrelated tables, and secret values are never
part of the hash.

The normal ownership states are:

- no table and no saved hash: Codeg may create it;
- table and matching saved hash: Codeg may update or remove it;
- table and no saved hash: treat it as user-owned and fail;
- table and a mismatched saved hash: treat it as externally modified and fail.

All reads, ownership checks, edits, and hash updates for one effective Grok
configuration path run under a path-scoped lock. Parse with
`toml_edit::DocumentMut`, change only `[model.codeg-managed]`, write a temporary
file in the same directory, then atomically replace the original. Store the new
canonical hash only after the replacement succeeds.

An explicit user-triggered reclaim operation is the only recovery from a
mismatched or missing hash. Reclaim is offered only when the reserved table
uses exactly the four-field schema above and
`env_key = "CODEG_GROK_PROVIDER_API_KEY"`. The UI shows the current table and
the proposed Codeg-managed table as a diff before confirmation. Confirmation
replaces the table and records a new hash; external edits are never silently
accepted.

OAuth and ordinary API-key modes do not modify `config.toml`.

The managed table and relevant runtime environment participate in the existing
connection configuration fingerprint. Authentication token refresh does not,
because Grok hot-loads credential changes.

## ACP Session Models

Grok `0.2.93` returns the ACP `models` state from `session/new`. The verified
response contained:

```json
{
  "models": {
    "currentModelId": "grok-build",
    "availableModels": [
      {
        "modelId": "grok-build",
        "name": "..."
      }
    ]
  }
}
```

The same response did not contain a model-category standard `configOptions`
entry. The existing `SetConfigOption` path therefore cannot switch the model
for this Grok version. Codeg must enable `unstable_session_model` and add
generic session-model support rather than converting this into a Grok private
extension.

Backend requirements:

- preserve `models` across new, load, resume, and fork response adaptation;
- store model state in `SessionState`;
- emit model-state and model-changed events;
- add a `SetModel` connection command using `session/set_model`;
- include models in options probes and snapshots.

Frontend requirements:

- display the current and available models in the existing composer config
  area;
- persist selected model preferences where automations and delegation need
  them;
- call the ACP session-model setter;
- when a future agent response contains both `models` and a model-category
  config option, render one selector backed by `models`; do not expose
  duplicate controls.

The Codeg-managed provider sets the initial model. The ACP session-model
capability controls subsequent in-session switching. Because the schema marks
this capability unstable, protocol adaptation stays confined to the shared ACP
adapter and is covered by exact request/response serialization tests.

## Session Lifecycle

### New Sessions

After authentication:

1. resolve Codeg MCP entries;
2. inject the Grok-only connection-scoped `codeg-mcp-<suffix>` server;
3. send `session/new`;
4. retain the returned session ID and model state;
5. attach the shared conversation loop.

### Existing Sessions

Use capabilities exactly as advertised:

```text
standard resume advertised -> resume
otherwise loadSession true -> load
otherwise -> new session only
```

This is an ordered fallback chain, not three interchangeable choices.
Authentication failures, malformed protocol responses, and
`ResourceNotFound` for an existing external session are terminal for that
connection attempt and do not advance to a new empty session.

For the verified Grok version, restoration uses `session/load`.

Do not call private `x.ai/session/*` extensions to emulate standard resume or
fork. Hide unsupported fork actions.

On `ResourceNotFound`, keep the existing Codeg behavior: surface a load error
and do not silently create an empty session under the old conversation.

## Grok History Parser

This is a new, Grok-specific complex module. Existing parsers remain unchanged;
the implementation must not broaden this work into a repository-wide parser
refactor.

### Root and Discovery

Resolve:

```text
GROK_HOME if non-empty
otherwise ~/.grok
```

Scan:

```text
<grok-home>/sessions/<encoded-cwd>/<session-id>/
```

Use `summary.json` as the list index. If an encoded group name was shortened,
read the group's `.cwd` file for the original working directory.

### Summary Mapping

Map:

- session ID;
- working directory and folder name;
- `generated_title`, then `session_summary`, then first user text;
- creation and update timestamps;
- message count;
- current model;
- parent session ID;
- active agent name.

Grok forks and native subagents may establish parent relationships, but they
must not be labeled as Codeg delegation unless Codeg delegation metadata is
present.

### Detail Sources

Try sources in this strict priority:

1. `updates.jsonl` as the authoritative ACP update stream;
2. `events.jsonl` when no valid update record is available and events can be
   mapped without guessing;
3. `chat_history.jsonl` as a text-only fallback.

Select the first source that contains at least one valid, renderable record.
If a higher-priority file exists but is empty or entirely malformed, fall back
to the next source and attach a sanitized degraded-history warning. Never merge
lower-priority content into a usable higher-priority stream, because that would
duplicate turns.

### Event Reconstruction

Map at minimum:

- user message chunks;
- agent message chunks;
- thought chunks;
- tool calls;
- tool-call updates;
- plans;
- usage and turn completion where present.

Use protocol `messageId` and `toolCallId` values. Generate deterministic IDs
from session ID and line position only when an ID is absent.

Associate tool-call updates with the original tool call and preserve raw input,
raw output, status, locations, and content blocks supported by Codeg.

Use `signals.json` for aggregate usage and counters when available. Represent
missing values as `Option` in backend models. The frontend renders unknown
statistics as an em dash or hides the field; it must never coerce unknown to
zero.

### Corruption and Concurrent Writes

- Ignore an incomplete final JSONL line.
- Skip an isolated malformed line and retain the rest of the session.
- Emit a degraded-history warning when source fallback or skipped corruption
  means the reconstructed session may be incomplete.
- Mark a session corrupt only when its summary or usable content cannot be
  recovered.
- One corrupt session must not block listing others.
- Do not rewrite Grok-owned history.

Codeg stores only its normal database index and association metadata. Grok
files remain the transcript source of truth.

## MCP Coexistence

### Verified Behavior

Grok loads native, project, Claude-compatible, Cursor-compatible, and standard
project MCP configuration independently of ACP `session/new.mcpServers`.

A live collision probe showed that a native Grok server wins when an ACP-wire
server has the same name.

Therefore shared `~/.grok` state cannot provide a strict Codeg-only MCP
environment without a fragile projected home. The approved design is
coexistence.

### Rules

1. Never write Grok-native MCP configuration.
2. Forward Codeg MCP entries over the ACP wire.
3. Keep native Grok MCP entries active.
4. Initially report each forwarded Codeg entry as `forwarded_unverified`.
5. Treat a same-name Codeg entry as `shadowed_by_native` only after native
   server information confirms the collision.
6. Report the source and collision instead of claiming the Codeg entry is
   active.
7. For Grok connections only, inject the built-in companion under
   `codeg-mcp-<short-connection-suffix>`. Preserve the fixed `"codeg-mcp"`
   server name for every existing agent.
8. Continue capability filtering for HTTP and SSE.
9. Treat Grok private MCP status notifications as diagnostics only.

Native MCP discovery may complete after `initialize`. Codeg does not block
session creation waiting for a private notification and does not run
`grok inspect` as a second source of truth. When
`_x.ai/mcp/servers_updated` arrives, Codeg reconciles the displayed source
list and marks any already-forwarded same-name Codeg server as shadowed. If
the private notification is absent in a future version, core MCP forwarding
continues, forwarded entries remain `forwarded_unverified`, and the native
count is displayed as unavailable rather than zero. Absence of the private
notification is never interpreted as proof that no native servers exist.

The Grok-only dynamic companion name is an adapter parameter, not a global
rename. Existing suffix-based tool normalization and delegation title/argument
detection must continue to recognize the dynamic prefix. Dedicated regression
tests cover both the dynamic Grok name and the unchanged Claude/Codex
`"codeg-mcp"` name.

The settings UI displays native and Codeg-injected counts separately.

## Permissions

Permissions follow Codeg's existing runtime ACP path:

- display the options returned by `session/request_permission`;
- send the selected option ID unchanged;
- support allow-once, allow-always, reject-once, and reject-always when the
  agent offers them;
- do not launch with `--always-approve`;
- do not synthesize a Grok permission selector;
- expose `/always-approve` through the existing available-command UI.

The `0.2.93` live probe found no standard permission selector. Therefore this
design does not add a Grok-specific selector or infer permission state from
private metadata.

If Grok later advertises a standard config option for permissions, Codeg will
display it through the generic session-config implementation.

## Multi-Agent Delegation

Add Grok to:

- known delegation agents;
- parent and child selection;
- per-agent delegation defaults;
- options probing;
- session and status rendering.

Codeg delegation and Grok-native subagents remain distinct:

- `codeg-mcp` creates separate Codeg-managed child connections and
  conversations;
- Grok's own subagent tools remain internal Grok tool calls and persisted Grok
  child sessions;
- neither path is converted into the other.

Child Grok sessions use the same authentication and model-provider settings as
normal Grok connections unless an existing delegation override explicitly
changes standard session options.

## Frontend Design

### Agent Surfaces

Add Grok to:

- new conversation and quick agent selectors;
- conversation lists and search;
- status bar and live session views;
- automations;
- delegation settings and cards;
- agent install and settings pages.

Use a Grok/xAI monochrome icon asset consistent with the existing `AgentIcon`
system.

### Grok Settings Panel

Provide four compact sections:

1. Installation: installed version, verified version, install, upgrade,
   reinstall, uninstall, and dependency status.
2. Authentication: effective source, browser login, device login, logout,
   masked API-key replace/clear.
3. Model: official default model, Codeg provider binding, reconnect status,
   and an open-config-file action.
4. MCP sources: native count, Codeg count, and shadowed names.

Do not offer a generic raw overwrite of `config.toml`.

### Conversation UI

- Standard models appear in the existing composer config area.
- Permission requests reuse `PermissionDialog`.
- Available slash commands use the existing command menu.
- Image and audio controls remain capability-driven.
- Official CLI-created sessions appear in the same history list.
- Unsupported standard fork controls are hidden.

All new strings must be present in the ten supported locale files.

## Error Model

Add stable error codes:

```text
grok_authentication_required
grok_auth_method_unavailable
grok_login_failed
grok_login_cancelled
grok_login_timeout
grok_config_invalid
grok_managed_model_conflict
grok_session_corrupt
```

`grok_mcp_shadowed` is a non-terminal warning/diagnostic code, not an
`AppError`.

Behavior:

- preserve the session and input draft on authentication failure;
- allow Grok's own token refresh before surfacing a mid-turn auth error;
- never auto-delete credentials after a failed refresh;
- refuse TOML writes when the existing file cannot be parsed;
- never overwrite an unowned managed-model table;
- isolate corrupt sessions;
- do not terminate chat solely because an optional MCP server failed;
- redact credentials from logs, errors, and events.

## Testing

### Rust Unit Tests

- registry metadata, IDs, version, arguments, Node requirement, and env;
- authentication method selection and handshake ordering;
- auth allowlist tests proving Claude Code, Codex, and Kimi never enter the
  generic authenticate phase even when they advertise auth methods;
- structured auth failures;
- exact schema feature coverage and serialization for
  `unstable_session_model`;
- preservation of session models across all session paths;
- `session/set_model` command and state updates;
- `DocumentMut` managed TOML patching, comment preservation, canonical hash,
  reclaim confirmation, path locking, atomic replacement, conflicts, and
  removal;
- secret-free TOML output;
- parser fixtures for all supported history sources and fallback priority;
- `.cwd`, parents, subagents, malformed lines, and incomplete final writes;
- degraded-history warnings and optional unknown statistics;
- MCP forwarded-unverified state, native-name shadowing, absent private
  notifications, and Grok-only unique companion naming;
- shared desktop/server command cores.

### Frontend Tests

- agent type, order, icon, labels, and selectors;
- all authentication and login task states;
- API-key masking;
- session-model selector and no duplicate model controls;
- MCP source and shadow warnings;
- unknown usage/counter rendering without fabricated zeroes;
- external session restoration and corruption errors;
- locale key parity.

### Shared ACP Regression Tests

The shared-path changes must preserve these Claude Code and Codex workflows:

- delegation round-trip with the fixed `"codeg-mcp"` server name;
- `session/load` history drain;
- config-fingerprint stale detection and reconnect behavior;
- `PermissionDialog` request and selected-option response path.

### Real ACP Smoke Test

Provide an opt-in or appropriately isolated test that:

1. launches the pinned Grok binary;
2. initializes protocol version 1;
3. verifies advertised authentication and `loadSession` capabilities;
4. authenticates against a temporary test configuration;
5. creates a session without sending an inference prompt;
6. asserts `models.currentModelId == "grok-build"` and that
   `models.availableModels` is non-empty and contains the current model;
7. asserts that no model-category standard `configOptions` entry is present;
8. verifies native versus wire MCP collision behavior.

Default tests must not require a paid inference call.

### Required Verification

Backend:

```bash
cd src-tauri
cargo check
cargo test --features test-utils
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo check --no-default-features --bin codeg-mcp
```

Frontend:

```bash
pnpm eslint .
pnpm test
pnpm build
```

Review parser snapshots explicitly when they change.

## Acceptance Criteria

1. Codeg can install, upgrade, uninstall, and launch the verified Grok package.
2. Browser OAuth, device-code login, API-key auth, and model-provider auth work.
3. The UI accurately reports the effective credential source.
4. New and official external Grok sessions can be opened and continued.
5. History survives Codeg and Grok process restarts.
6. Text, thought, plan, tool, permission, and completion updates render.
7. ACP session-model switching works through the isolated unstable capability.
8. Grok-native and Codeg MCP servers coexist with visible collisions.
9. Grok works as a Codeg delegation parent and child.
10. Malformed config, expired auth, corrupt history, and MCP failures are
    isolated and recoverable.
11. Claude Code ACP and Codex ACP delegation, history load, config-fingerprint,
    and permission-dialog workflows do not regress.

## Implementation Sequence

The implementation plan should divide work into these checkpoints:

1. Generic ACP authentication and isolated unstable session-model support.
2. Grok registration, installation, authentication, and model-provider config.
3. Grok's new complex parser module and external-session import.
4. Frontend settings, agent surfaces, icon, and localization.
5. MCP/delegation diagnostics, real smoke testing, and full regression.

This sequence keeps generic ACP changes reviewable before layering Grok-specific
features on top.

## Design Review Resolution

The design-review findings were incorporated with one technical correction:

- `authenticate` and basic agent-owned auth methods are stable in schema
  `0.11.7`; `unstable_auth_methods` is not required and will not be enabled.
- Grok's verified response uses `models`, not a model-category config option,
  so `unstable_session_model` is required and its use is isolated and tested.
- Comment-preserving managed configuration requires direct `toml_edit`
  `DocumentMut` usage.
- Authentication ownership, allowlisting, TOML reclaim and atomicity, parser
  fallback, MCP diagnostic states, Grok-only companion naming, `AgentType`
  catch-all audits, unknown statistics, and shared-agent regressions are now
  explicit implementation requirements.
