# CodeBuddy Delegation Profiles Design

Date: 2026-07-12

Status: Approved for implementation

## Summary

Add reusable delegation profiles so the `@` composer can expose multiple
CodeBuddy sub-agents backed by the same `code_buddy` ACP implementation but
different session configuration. Example entries are:

```text
@CodeBuddy:GLM5.2
@CodeBuddy:KimiCode2.7
@CodeBuddy:Opus4.8
```

Profiles are available only for sub-agent delegation. They do not appear in the
main-session agent selector and do not change CodeBuddy's normal global or
main-session configuration.

The persisted model is generic enough to support profiles for other ACP agents
later, but the first UI exposes profile management only for CodeBuddy.

## Goals

1. Let users create multiple named CodeBuddy delegation profiles by copying the
   current CodeBuddy delegation defaults and changing the advertised model or
   other ACP session options.
2. Show each enabled profile as a distinct `@` suggestion named
   `CodeBuddy:<profile name>`.
3. Treat a profile mention as an explicit route. The generated delegation call
   must carry the selected stable profile ID.
4. When one message mentions several profiles, require one independent,
   parallel delegation for each distinct mention.
5. Resolve the profile in the main process immediately before spawn and apply
   its exact mode and configuration values to the child ACP session.
6. Preserve all existing agent-type delegation behavior and persisted
   `delegation.agent_defaults` values.

## Non-Goals

- Adding profiles to the main-session agent selector.
- Creating new `AgentType` variants for models.
- Installing or launching a different CodeBuddy ACP package per profile.
- Giving profiles independent API keys, environment variables, MCP servers,
  skills, or CodeBuddy home directories.
- Exposing multi-profile creation for agents other than CodeBuddy in the first
  release.
- Automatically migrating the existing CodeBuddy default into a profile.
- Changing existing plain `@CodeBuddy` mention behavior.

## Current Constraints

Codeg currently treats an ACP implementation as a unique `AgentType` across the
registry, composer, delegation tool, broker, connection lifecycle, parser, and
conversation storage. `delegation.agent_defaults` is keyed by `AgentType`, so
CodeBuddy can have only one default mode/configuration pair.

Agent references in the composer currently serialize as readable links such as
`[@CodeBuddy](codeg://agent/code_buddy)`. The URI is an opaque routing anchor;
it does not carry a selectable configuration instance. The delegation MCP tool
accepts only `agent_type`, so the broker cannot distinguish two CodeBuddy model
configurations.

## Architecture

### 1. Delegation Profile Model

Add a shared serializable `DelegationProfile`:

```text
id: String                         stable UUID
agent_type: AgentType              code_buddy in the first UI
name: String                       user-owned short name, e.g. GLM5.2
mode_id: Option<String>            copied ACP session-mode override
config_values: BTreeMap<String,String>
enabled: bool
created_at: i64                    Unix milliseconds
updated_at: i64                    Unix milliseconds
```

The profile stores launch-time ACP session choices only. It intentionally does
not duplicate package, environment, authentication, provider, or parser state.
The display label is derived as `<agent display name>:<profile name>` and is not
persisted separately.

Profile IDs are immutable. Names are trimmed, non-empty, limited to 80 Unicode
scalar values, and unique case-insensitively within one `agent_type`. Colons are
allowed in names, because routing uses the ID rather than parsing the label.

### 2. Persistence and Commands

Persist all profiles as one versioned JSON document in `app_metadata` under:

```text
delegation.profiles.v1
```

The document uses whole-list replacement, matching the existing
`delegation.agent_defaults` persistence style. Add backend commands and matching
HTTP handlers for:

```text
get_delegation_profiles
set_delegation_profiles
```

The setter validates IDs, names, duplicate IDs, duplicate names, and option
shapes before writing. It returns the normalized saved list. The first frontend
only submits `agent_type=code_buddy`, but backend validation accepts every known
`AgentType` so the data contract remains reusable.

Corrupt stored JSON is reported as a structured configuration error rather than
silently replacing profiles with an empty list. A settings save must never erase
profiles merely because the prior document could not be parsed.

`delegation.agent_defaults` remains unchanged. It continues to control legacy
delegation calls that specify only `agent_type` and supplies the initial values
when the user creates a new CodeBuddy profile.

### 3. Profile Management UI

Add a `CodeBuddy profiles` section to the existing Multi-Agent Collaboration
settings, beside the current agent-default controls.

The section lists compact rows with:

- the derived label `CodeBuddy:<name>`;
- the selected model label, when one is available;
- enabled/disabled state;
- edit, duplicate, and delete actions.

Creating a profile performs these steps:

1. Probe CodeBuddy through the existing `describeAgentOptions` API.
2. Copy the currently saved CodeBuddy entry from
   `delegation.agent_defaults`.
3. Open an editor initialized with that copied mode/configuration.
4. Render the same capability-driven mode and config-option selectors used by
   delegation defaults, including the live model list advertised by CodeBuddy.
5. Require a unique short profile name and save a new UUID-backed profile.

Editing retains the ID and timestamps it on save. Duplicating creates a new ID,
copies all mode/config values, defaults the name to `<name> copy`, and resolves
name collisions with a numeric suffix. Deleting requires confirmation. Disabling
is non-destructive and removes the entry from future `@` suggestions.

If a persisted option is no longer advertised by the live ACP probe, show it as
unavailable and retain it until the user explicitly changes or clears it. This
matches the existing delegation-default behavior and avoids destructive saves
after a CodeBuddy upgrade.

### 4. Composer Suggestions and References

Extend the reference-search source with enabled delegation profiles. Profiles
appear in the existing Agents group immediately after their base agent, with the
CodeBuddy icon and the derived label. Plain `@CodeBuddy` remains present and
retains its current semantics.

Add a distinct reference type, `delegation_profile`, with:

```text
id: <profile UUID>
label: CodeBuddy:<name>
uri: codeg://delegation-profile/<profile UUID>
meta.agentType: code_buddy
meta.profileId: <profile UUID>
```

The stable ID, not the visible name or model string, is the routing key. Renaming
a profile therefore does not invalidate drafts or historical transcript links.

The composer serializes visible text as:

```text
[@CodeBuddy:GLM5.2](codeg://delegation-profile/<uuid>)
```

On send, each distinct profile reference also contributes a generated routing
directive to the parent prompt. The directive tells the parent to call
`delegate_to_agent` exactly once for that profile, pass the referenced UUID as
`profile_id`, and fan out all mentioned profiles before collecting results.
Duplicate mentions of the same profile in one message produce one route.

The human-facing optimistic message and transcript badge continue to show only
the original visible mention. Routing directives are transport-only prompt
context and must not be rendered as user-authored text. Prompt conversion owns
this transformation so desktop, server, automation, queued-send, and retry paths
cannot drift.

The parent model still decides how to turn the surrounding request into the
self-contained child `task`, as it does for current agent mentions. Codeg's hard
guarantee begins at the tool boundary: once a profile ID is supplied, the broker
either runs exactly that profile or returns an error; it never falls back to a
different model or the base CodeBuddy default.

### 5. Delegation Tool and Companion

Add an optional `profile_id` string to `delegate_to_agent`. Existing callers may
continue to send only `agent_type`.

When a profile mention is present, the routing directive requires both:

```json
{
  "agent_type": "code_buddy",
  "profile_id": "<uuid>",
  "task": "..."
}
```

Keeping `agent_type` preserves the current static schema enum, makes the tool
call legible, and permits early mismatch validation. The companion forwards
`profile_id` without reading configuration itself. Profile data and secrets are
never copied into the MCP tool schema or the parent model's context.

The listener validates only the profile-ID shape and forwards it in
`DelegationRequest`. The broker is the authority for existence, enabled state,
agent-type match, and option resolution.

### 6. Broker Resolution and Spawn

Add a profile lookup abstraction to `DelegationBroker`, backed by the persisted
profile store. Immediately before spawning a child:

1. If `profile_id` is absent, use the existing `agent_defaults[agent_type]`
   path unchanged.
2. If `profile_id` is present, load that profile.
3. Reject an unknown or disabled profile.
4. Reject a profile whose `agent_type` differs from the requested `agent_type`.
5. Use the profile's `mode_id` and `config_values` instead of the per-agent
   defaults.
6. Pass the resolved values into the existing `ConnectionSpawner::spawn`.

The connection manager, CodeBuddy command, ACP initialization, session creation,
MCP forwarding, parser, and conversation database continue to see
`AgentType::CodeBuddy`. No profile-specific ACP process or parser branch is
introduced.

Capture `profile_id` and the derived display label in running/completed
delegation reports and delegation events. This lets status cards and child
session UI distinguish `CodeBuddy:GLM5.2` from `CodeBuddy:Opus4.8` while keeping
the child conversation's canonical agent type as `code_buddy`.

Snapshot the resolved label when the delegation starts. Historical cards remain
readable after a profile is renamed or deleted.

## Data Flow

```text
Settings: create from CodeBuddy defaults
  -> probe ACP options
  -> save DelegationProfile in delegation.profiles.v1

Composer: choose one or more @CodeBuddy:<profile>
  -> serialize visible profile links
  -> add transport-only mandatory route directives
  -> parent calls delegate_to_agent once per profile_id

Companion
  -> forwards agent_type + profile_id + task

Listener
  -> validates wire shape
  -> builds DelegationRequest

Broker
  -> resolves enabled profile by immutable ID
  -> verifies agent_type
  -> applies profile mode/config values
  -> spawns normal CodeBuddy ACP child

Events and reports
  -> carry agent_type + profile identity snapshot
  -> render CodeBuddy:<profile> in delegation UI
```

## Error Handling

- Empty, duplicate, or oversized profile names: reject settings save with
  `configuration_invalid` and keep the previous document intact.
- Duplicate or malformed profile IDs: reject the whole save atomically.
- Corrupt persisted profile JSON: return a configuration error; do not treat it
  as an empty list during a save.
- CodeBuddy probe failure: keep existing profiles viewable and editable by raw
  persisted values; disable only controls that require a live catalog.
- Unknown `profile_id` at delegation time: fail with
  `invalid_delegation_profile`.
- Disabled profile: fail with `delegation_profile_disabled`.
- `agent_type` mismatch: fail with `delegation_profile_agent_mismatch`.
- Unsupported or stale mode/config value: surface the existing child spawn or
  ACP set-option error. Do not silently clear or replace it.
- A profile deleted after a draft was composed: the mention stays readable, but
  sending/delegating fails explicitly instead of using base CodeBuddy.
- One failed route in a multi-profile fan-out does not cancel the other routes.

## Compatibility and Migration

No database migration is required because the new document lives in
`app_metadata`. Missing `delegation.profiles.v1` means an empty profile list.

Existing delegation settings, `delegate_to_agent` calls, sessions, references,
and plain agent mentions remain valid. The new `profile_id` field is optional on
all wire and snapshot types so older persisted state deserializes normally.

Profile data should be included automatically in existing backup/restore because
`app_metadata` is already part of the database snapshot. No external CodeBuddy
configuration file is modified by profile management.

## Testing

### Rust

- Profile validation: trimming, uniqueness, length, UUID shape, timestamps, and
  atomic rejection.
- Persistence: missing document, valid round trip, corrupt JSON, and preservation
  of existing `agent_defaults`.
- Listener/tool parsing: legacy call without `profile_id`, valid profile call,
  malformed ID, and agent/profile mismatch propagation.
- Broker: profile override wins over agent defaults; unknown, disabled, and
  mismatched profiles fail without spawning; legacy delegation is unchanged.
- Concurrent fan-out: two CodeBuddy profile requests retain independent option
  maps and status identities.
- Event/report serialization: optional profile identity round trips and old
  payloads still deserialize.

### TypeScript and React

- API/type mirrors for profile load/save.
- Settings create/edit/duplicate/delete/enable behavior and validation.
- Creation copies CodeBuddy delegation defaults before changes.
- Stale ACP option values remain visible and survive save.
- `@` search shows only enabled profiles with derived labels and stable URIs.
- Prompt conversion emits one mandatory route per distinct profile mention and
  keeps directives out of the rendered user message.
- Multiple profile mentions preserve independent IDs.
- Delegation cards prefer the captured profile label and fall back to the base
  agent label for legacy data.

### Regression

- Existing delegation settings and broker tests.
- Composer reference serialization and transcript badge tests.
- Agent selector tests proving profiles never appear in the main selector.
- Frontend lint, focused Vitest suites, Rust formatting, focused Cargo tests,
  and the broader checks required by `AGENTS.md` in proportion to runtime cost.

## Delivery Boundaries

The first release is complete when a user can create at least three enabled
CodeBuddy profiles, select all three from one composer message, observe three
independent delegated children labeled by profile, and verify that each child
received the profile's saved ACP model/configuration. The main-session CodeBuddy
selector and configuration must remain unchanged.
