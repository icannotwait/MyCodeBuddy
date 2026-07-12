# CodeBuddy Delegation Profiles Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add named CodeBuddy-only sub-agent profiles that appear as distinct `@CodeBuddy:<name>` routes and apply their exact ACP mode/model configuration when delegated.

**Architecture:** Persist generic delegation profiles in `app_metadata`, expose them through the existing Tauri/HTTP transport, and carry an optional immutable `profile_id` through the MCP companion into the broker. The composer creates a dedicated profile reference and transport-only routing directive; the broker remains authoritative and resolves the profile to the existing `AgentType + mode + config` spawn interface.

**Tech Stack:** Rust 2021, Tokio, SeaORM/SQLite, serde, Axum/Tauri, React 19, TypeScript strict, Next.js 16, Vitest, React Testing Library, next-intl.

## Global Constraints

- Profiles affect sub-agent delegation only; the main agent selector and main CodeBuddy configuration must not change.
- Persist profiles under the exact key `delegation.profiles.v1`.
- Profile IDs are immutable UUID strings; routing must never parse the display label.
- Profile names are trimmed, non-empty, at most 80 Unicode scalar values, and case-insensitively unique within one agent type.
- Existing `delegation.agent_defaults` and calls without `profile_id` retain current behavior.
- Unknown, disabled, or mismatched profiles fail explicitly and never fall back to base CodeBuddy defaults.
- The first UI creates profiles only for `code_buddy`; shared Rust/TypeScript types remain generic over `AgentType`.
- Use TDD for every behavior change and preserve all unrelated user changes.

---

### Task 1: Profile Domain Model and Persistence

**Files:**
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/commands/delegation.rs`
- Test: inline `#[cfg(test)]` modules in both files

**Interfaces:**
- Produces: `DelegationProfile`, `DelegationProfileDocument`, `load_delegation_profiles`, and `set_delegation_profiles_core`.
- Consumes: existing `AgentType`, `AgentDelegationDefaults`, `app_metadata_service`, and `AppCommandError`.

- [ ] **Step 1: Write failing model-validation tests**

Add tests proving normalization and atomic rejection:

```rust
#[test]
fn profiles_trim_names_and_reject_case_folded_duplicates() {
    let profiles = vec![
        profile("11111111-1111-4111-8111-111111111111", " GLM5.2 "),
        profile("22222222-2222-4222-8222-222222222222", "glm5.2"),
    ];
    let err = normalize_profiles(profiles).unwrap_err();
    assert!(err.to_string().contains("duplicate profile name"));
}

#[test]
fn profile_name_limit_counts_unicode_scalars() {
    let mut p = profile("11111111-1111-4111-8111-111111111111", &"模".repeat(81));
    assert!(normalize_profiles(vec![p.clone()]).is_err());
    p.name = "模".repeat(80);
    assert_eq!(normalize_profiles(vec![p]).unwrap()[0].name.chars().count(), 80);
}
```

- [ ] **Step 2: Run the focused Rust test and verify failure**

Run: `cd src-tauri && cargo test commands::delegation::tests::profiles_trim_names_and_reject_case_folded_duplicates --features test-utils`

Expected: compile failure because the profile types and normalizer do not exist.

- [ ] **Step 3: Add the serializable profile types and normalizer**

Implement the shared shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfile {
    pub id: String,
    pub agent_type: AgentType,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config_values: BTreeMap<String, String>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfileDocument {
    #[serde(default)]
    pub profiles: Vec<DelegationProfile>,
}
```

Use `uuid::Uuid::parse_str`, trim names, reject names with `chars().count() > 80`, and track `(AgentType, name.to_lowercase())` plus IDs in `BTreeSet`s. Do not regenerate IDs or timestamps during normalization.

- [ ] **Step 4: Write failing persistence tests**

Cover missing data, valid round trip, corrupt JSON, and preservation of the prior value after a rejected save. The corrupt case must assert a structured error rather than an empty document.

- [ ] **Step 5: Implement persistence helpers**

Add:

```rust
pub const KEY_DELEGATION_PROFILES_V1: &str = "delegation.profiles.v1";

pub async fn load_delegation_profiles(
    conn: &DatabaseConnection,
) -> Result<DelegationProfileDocument, AppCommandError>;

pub async fn set_delegation_profiles_core(
    conn: &DatabaseConnection,
    desired: DelegationProfileDocument,
) -> Result<DelegationProfileDocument, AppCommandError>;
```

Missing metadata returns `DelegationProfileDocument::default()`. Parse or validation failures return `configuration_invalid`. Validate and serialize before calling `upsert_value` so a failed request cannot overwrite the previous document.

- [ ] **Step 6: Run focused tests and commit**

Run: `cd src-tauri && cargo test commands::delegation::tests --features test-utils`

Expected: all delegation command tests pass.

Commit:

```bash
git add src-tauri/src/acp/delegation/types.rs src-tauri/src/commands/delegation.rs
git commit -m "feat(delegation): persist sub-agent profiles"
```

### Task 2: Profile Command and Transport Surface

**Files:**
- Modify: `src-tauri/src/commands/delegation.rs`
- Modify: `src-tauri/src/web/handlers/delegation.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Create: `src/lib/delegation-profiles-api.test.ts`

**Interfaces:**
- Consumes: `DelegationProfileDocument`, `load_delegation_profiles`, and `set_delegation_profiles_core` from Task 1.
- Produces: Tauri/HTTP commands `get_delegation_profiles` and `set_delegation_profiles`, plus TypeScript APIs with the same camel-case wrappers as existing delegation settings.

- [ ] **Step 1: Write failing TypeScript transport tests**

Assert exact command names and payload shapes:

```ts
await getDelegationProfiles()
expect(call).toHaveBeenCalledWith("get_delegation_profiles")

await setDelegationProfiles({ profiles })
expect(call).toHaveBeenCalledWith("set_delegation_profiles", {
  document: { profiles },
})
```

- [ ] **Step 2: Add Rust command and HTTP handlers**

Expose:

```rust
#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_delegation_profiles(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, crate::db::AppDatabase>,
) -> Result<DelegationProfileDocument, AppCommandError>;

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn set_delegation_profiles(
    ...,
    document: DelegationProfileDocument,
) -> Result<DelegationProfileDocument, AppCommandError>;
```

Register both commands in `generate_handler!`, add `/get_delegation_profiles` and `/set_delegation_profiles` POST routes, and keep the web JSON request field named `document`.

- [ ] **Step 3: Add TypeScript mirrors and API calls**

```ts
export interface DelegationProfile {
  id: string
  agent_type: AgentType
  name: string
  mode_id?: string | null
  config_values: Record<string, string>
  enabled: boolean
  created_at: number
  updated_at: number
}

export interface DelegationProfileDocument {
  profiles: DelegationProfile[]
}

export const getDelegationProfiles = () =>
  getTransport().call<DelegationProfileDocument>("get_delegation_profiles")

export const setDelegationProfiles = (document: DelegationProfileDocument) =>
  getTransport().call<DelegationProfileDocument>("set_delegation_profiles", {
    document,
  })
```

- [ ] **Step 4: Run checks and commit**

Run: `pnpm test -- src/lib/api`

Run: `cd src-tauri && cargo check`

Expected: both commands succeed.

Commit:

```bash
git add src-tauri/src/commands/delegation.rs src-tauri/src/web/handlers/delegation.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts
git commit -m "feat(delegation): expose profile settings API"
```

### Task 3: Profile-Aware Delegation Protocol and Broker

**Files:**
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/event_emitter.rs`
- Modify: `src-tauri/src/acp/types.rs`
- Modify: `src-tauri/src/commands/delegation.rs`
- Test: inline Rust tests in the same modules

**Interfaces:**
- Consumes: persisted profile document from Task 1.
- Produces: optional `profile_id` and `profile_label` snapshots on requests, reports, running tasks, and delegation events; broker profile resolution before `ConnectionSpawner::spawn`.

- [ ] **Step 1: Write failing broker tests**

Add tests with a deterministic profile lookup:

```rust
#[tokio::test]
async fn profile_override_wins_over_agent_default() {
    let profile = codebuddy_profile("11111111-1111-4111-8111-111111111111", "GLM5.2", "glm-5.2");
    let broker = broker_with_profiles(vec![profile]);
    let mut req = request(1, "tool-1");
    req.agent_type = AgentType::CodeBuddy;
    req.profile_id = Some("11111111-1111-4111-8111-111111111111".into());
    broker.start_delegation(req).await;
    let spawn = broker.mock_spawner().spawn_args.lock().await;
    assert_eq!(spawn[0].preferred_config_values["model"], "glm-5.2");
}
```

Also prove unknown, disabled, and mismatched profiles produce the exact error codes `invalid_delegation_profile`, `delegation_profile_disabled`, and `delegation_profile_agent_mismatch`, with zero spawn calls. Retain the existing legacy-default test unchanged.

- [ ] **Step 2: Extend wire and event types compatibly**

Add `#[serde(default, skip_serializing_if = "Option::is_none")]` fields:

```rust
pub profile_id: Option<String>,
pub profile_label: Option<String>,
```

`DelegationRequest` carries `profile_id`; reports and events carry both ID and the label snapshot. Keep all new fields optional so old snapshots deserialize.

- [ ] **Step 3: Add the broker lookup interface**

```rust
#[async_trait]
pub trait DelegationProfileLookup: Send + Sync {
    async fn find_profile(&self, id: &str) -> Result<Option<DelegationProfile>, DelegationError>;
}
```

Implement a database-backed lookup in `commands/delegation.rs` and inject it when constructing `DelegationBroker` in desktop/server startup. Test code uses an in-memory lookup.

- [ ] **Step 4: Resolve profiles before spawn**

Replace the current single defaults lookup with:

```rust
let (profile_id, profile_label, preferred_mode_id, preferred_config_values) =
    if let Some(id) = req.profile_id.as_deref() {
        let profile = self.profiles.find_profile(id).await?
            .ok_or_else(|| DelegationError::InvalidDelegationProfile(id.into()))?;
        if !profile.enabled { return profile_disabled(req.agent_type, id); }
        if profile.agent_type != req.agent_type {
            return profile_agent_mismatch(req.agent_type, id);
        }
        (
            Some(profile.id.clone()),
            Some(format!("{}:{}", profile.agent_type, profile.name)),
            profile.mode_id,
            profile.config_values,
        )
    } else {
        let defaults = cfg.agent_defaults.get(&req.agent_type).cloned().unwrap_or_default();
        (None, None, defaults.mode_id, defaults.config_values)
    };
```

Use `get_agent_meta(profile.agent_type).name` rather than `Display` when building the final label so it is exactly `CodeBuddy:<name>`.

- [ ] **Step 5: Propagate the snapshot through lifecycle reports**

Store profile identity in `RunningTask` and `CompletedTask`, include it in `DelegationStarted`/`DelegationCompleted`, status reports, DB fallback reports where available, and frontend-facing snapshots. Renames/deletes after start must not change the running task's label.

- [ ] **Step 6: Run focused tests and commit**

Run: `cd src-tauri && cargo test acp::delegation --features test-utils`

Expected: all delegation tests pass, including legacy request serialization.

Commit:

```bash
git add src-tauri/src/acp/delegation src-tauri/src/acp/types.rs src-tauri/src/commands/delegation.rs src-tauri/src/lib.rs src-tauri/src/bin/codeg_server.rs
git commit -m "feat(delegation): route children through profiles"
```

### Task 4: MCP Companion Profile Argument

**Files:**
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Test: inline tests in `companion.rs` and `listener.rs`

**Interfaces:**
- Consumes: `DelegationRequest.profile_id` from Task 3.
- Produces: optional `profile_id` input on `delegate_to_agent`, forwarded unchanged to the listener/broker.

- [ ] **Step 1: Write failing companion/listener tests**

Assert that `tools/list` exposes `profile_id` as an optional string, a valid UUID is forwarded, a malformed value returns `-32602`, and a call without the field retains the current request shape.

- [ ] **Step 2: Extend the tool schema**

Add:

```json
"profile_id": {
  "type": "string",
  "description": "Optional immutable delegation-profile UUID. When supplied, the broker must run exactly this profile and must not fall back to agent defaults."
}
```

Update the tool description to say that every `codeg://delegation-profile/<uuid>` mention is mandatory and multiple distinct mentions must be fanned out as separate calls.

- [ ] **Step 3: Parse and forward the value**

In the companion, accept a trimmed optional string and reject non-string or empty values. In the listener, validate `Uuid::parse_str` before constructing `DelegationRequest`; do not access profile storage there.

- [ ] **Step 4: Run tests and commit**

Run: `cd src-tauri && cargo test acp::delegation::companion --features test-utils && cargo test acp::delegation::listener --features test-utils`

Expected: companion and listener suites pass.

Commit:

```bash
git add src-tauri/src/acp/delegation/tool_schema.json src-tauri/src/acp/delegation/companion.rs src-tauri/src/acp/delegation/listener.rs
git commit -m "feat(delegation): accept profile routes in MCP"
```

### Task 5: Composer Profile Suggestions and Mandatory Route Context

**Files:**
- Modify: `src/components/chat/composer/types.ts`
- Modify: `src/components/chat/composer/suggestion/adapters.ts`
- Modify: `src/components/chat/composer/use-reference-search.ts`
- Modify: `src/components/chat/composer/reference-text.ts`
- Modify: `src/components/chat/composer/reference-uri.ts`
- Modify: `src/components/chat/composer/to-prompt-blocks.ts`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/chat/composer/badges/reference-badge.tsx`
- Test: corresponding `.test.ts` and `.test.tsx` files

**Interfaces:**
- Consumes: `getDelegationProfiles()` and `DelegationProfile` from Task 2.
- Produces: `delegation_profile` references and prompt blocks containing one transport-only route directive per distinct profile ID.

- [ ] **Step 1: Write failing adapter/search tests**

Use a profile fixture and assert:

```ts
expect(profileToSuggestion(profile).reference).toEqual({
  refType: "delegation_profile",
  id: profile.id,
  label: "CodeBuddy:GLM5.2",
  uri: `codeg://delegation-profile/${profile.id}`,
  meta: { agentType: "code_buddy", profileId: profile.id },
})
```

Search tests must prove disabled profiles are excluded and enabled profiles are placed after plain CodeBuddy inside the Agents group. The main `AgentSelector` fixture remains unchanged.

- [ ] **Step 2: Add the reference kind and URI parser**

Extend `ReferenceKind`, `REFERENCE_KINDS`, `ReferenceMeta.profileId`, badge icon selection, and `parseCodegReferenceUri` for `codeg://delegation-profile/<uuid>`. Render visible markdown as `[@<label>](<uri>)`.

- [ ] **Step 3: Load and merge profile suggestions**

Have `useReferenceSearch` fetch profiles once per enabled composer lifecycle, retain the prior successful list on transient errors, and merge enabled profiles immediately after their base agent. Do not add profiles to `useSortedAvailableAgents` or `AgentSelector`.

- [ ] **Step 4: Write failing prompt-routing tests**

Build a ProseMirror document with GLM, Opus, then GLM again. Assert `docToPromptBlocks` returns visible user text plus exactly two route context blocks in first-mention order. The required directive content is:

```text
Codeg mandatory delegation route: call delegate_to_agent exactly once with agent_type="code_buddy" and profile_id="<uuid>" for @CodeBuddy:GLM5.2. Fan out all mandatory routes before collecting results. Do not substitute another profile or the base agent default.
```

- [ ] **Step 5: Separate display serialization from send context**

Walk the document to collect distinct `delegation_profile` attrs. Keep `serializeDocToDisplayText` unchanged. Make `docToPromptBlocks` prepend one clearly delimited text block containing every route directive, followed by the user's visible text block. Ensure optimistic/display text continues to use only `serializeDocToDisplayText`. Centralize collection in:

```ts
export function collectDelegationProfileRoutes(
  doc: ProseMirrorNode
): Array<{ profileId: string; agentType: AgentType; label: string }>
```

- [ ] **Step 6: Run focused tests and commit**

Run: `pnpm test -- src/components/chat/composer src/components/chat/agent-selector.test.tsx`

Expected: all composer tests and the unchanged main selector test pass.

Commit:

```bash
git add src/components/chat/composer src/components/chat/message-input.tsx
git commit -m "feat(composer): mention CodeBuddy delegation profiles"
```

### Task 6: CodeBuddy Profile Settings UI

**Files:**
- Create: `src/components/settings/delegation-profiles.tsx`
- Create: `src/components/settings/delegation-profiles.test.tsx`
- Modify: `src/components/settings/delegation-settings.tsx`
- Modify: `src/components/settings/delegation-agent-defaults.tsx`
- Modify: all files under `src/i18n/messages/*.json`
- Modify: `src/i18n/messages.test.ts`

**Interfaces:**
- Consumes: profile APIs from Task 2, `describeAgentOptions`, and the saved CodeBuddy `AgentDelegationDefaults`.
- Produces: CodeBuddy-only create/edit/duplicate/delete/enable workflow.

- [ ] **Step 1: Extract a reusable capability-options editor**

Move the mode/config row rendering from `DelegationAgentDefaultsPanel` into a focused component accepting:

```ts
interface DelegationOptionEditorProps {
  snapshot: AgentOptionsSnapshot
  value: AgentDelegationDefaults
  onChange: (value: AgentDelegationDefaults) => void
  disabled?: boolean
}
```

Keep stale persisted values as explicit unavailable select items. Run the existing delegation-default tests before and after extraction.

- [ ] **Step 2: Write failing profile UI tests**

Cover:

- create copies `agentDefaults.code_buddy` before edits;
- generated label is `CodeBuddy:GLM5.2`;
- duplicate receives a new mocked UUID and a collision-free `copy 2` name;
- disabled rows disappear from mention data after save;
- delete confirmation removes only the selected ID;
- stale model values remain in the submitted document.

- [ ] **Step 3: Implement the profile panel**

Use `crypto.randomUUID()` for new client IDs and `Date.now()` for millisecond timestamps. Keep one save owner in `DelegationSettingsSection`: load settings and profiles together, pass profile state to the panel, and save both documents sequentially with errors surfaced independently. Do not overwrite profiles if their initial load failed.

Use familiar controls: `Plus`, `Copy`, `Pencil`, and `Trash2` icon buttons with tooltips; `Switch` for enabled; `AlertDialog` for delete; `Input` for name; capability-driven `Select`s for mode/model/options. Keep card radius at the repository default and do not nest cards.

- [ ] **Step 4: Add all locale keys**

Add the same key set to all ten locale files. English and Simplified Chinese receive native copy; other locales may use accurate English fallback text only if that matches the repository's established untranslated-key policy. Update `messages.test.ts` so locale key parity fails if any profile label is missing.

- [ ] **Step 5: Run focused tests and commit**

Run: `pnpm test -- src/components/settings/delegation-profiles.test.tsx src/components/settings/delegation-settings.test.tsx src/i18n/messages.test.ts`

Expected: all tests pass.

Commit:

```bash
git add src/components/settings/delegation-profiles.tsx src/components/settings/delegation-profiles.test.tsx src/components/settings/delegation-settings.tsx src/components/settings/delegation-agent-defaults.tsx src/i18n/messages
git commit -m "feat(settings): manage CodeBuddy delegation profiles"
```

### Task 7: Profile Identity in Delegation Status UI

**Files:**
- Modify: `src/lib/types.ts`
- Modify: `src/lib/delegation-card.ts`
- Modify: `src/lib/delegation-status.ts`
- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/message/delegation-status-row.tsx`
- Modify: `src/components/message/delegation-status-card.tsx`
- Modify: related `.test.ts` and `.test.tsx` files

**Interfaces:**
- Consumes: optional `profile_id`/`profile_label` event and report fields from Task 3.
- Produces: stable `CodeBuddy:<name>` labels for running, completed, reloaded, and legacy delegation cards.

- [ ] **Step 1: Write failing rendering and reload tests**

Assert that a `profile_label: "CodeBuddy:GLM5.2"` event wins over `AGENT_LABELS.code_buddy`, survives completion-only hydration, and a legacy event with no profile fields still renders `CodeBuddy`.

- [ ] **Step 2: Mirror optional fields and normalize once**

Add optional snake-case fields to TypeScript event/report types. Update the central delegation-card model builder to expose `agentDisplayLabel = profile_label ?? AGENT_LABELS[agent_type]`; leaf components must not repeat fallback logic.

- [ ] **Step 3: Render the normalized label everywhere**

Use the normalized label in overlay rows, status rows/cards, dialogs, and accessible labels. Keep the CodeBuddy icon based on canonical `agent_type`.

- [ ] **Step 4: Run tests and commit**

Run: `pnpm test -- src/lib/delegation-status.test.ts src/components/chat/sub-agent-overlay.test.tsx src/components/message/delegation-status-card.test.tsx`

Expected: profile and legacy cases pass.

Commit:

```bash
git add src/lib/types.ts src/lib/delegation-card.ts src/lib/delegation-status.ts src/components/chat/sub-agent-overlay.tsx src/components/message/delegation-status-row.tsx src/components/message/delegation-status-card.tsx
git commit -m "feat(delegation): display child profile identity"
```

### Task 8: End-to-End Regression and Documentation Review

**Files:**
- Modify only files required by failures found during verification
- Review: `docs/superpowers/specs/2026-07-12-codebuddy-delegation-profiles-design.md`

**Interfaces:**
- Consumes: all prior tasks.
- Produces: verified feature with no profile leakage into main sessions.

- [ ] **Step 1: Run formatting and static checks**

Run:

```bash
pnpm prettier --write src src-tauri/src
pnpm eslint .
cd src-tauri && cargo fmt --check
cd src-tauri && cargo check
cd src-tauri && cargo check --no-default-features --bin codeg-server
```

Expected: every command exits 0.

- [ ] **Step 2: Run frontend and Rust test suites**

Run:

```bash
pnpm test
cd src-tauri && cargo test --features test-utils
cd src-tauri && cargo test --no-default-features --bin codeg-server --lib
```

Expected: all suites pass.

- [ ] **Step 3: Run builds and clippy**

Run:

```bash
pnpm build
cd src-tauri && cargo clippy --all-targets --features test-utils -- -D warnings
cd src-tauri && cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cd src-tauri && cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: all commands exit 0 with no warnings.

- [ ] **Step 4: Verify the user workflow manually**

Start the normal development application, create three CodeBuddy profiles with distinct model values, and verify:

```text
main selector: one CodeBuddy entry only
@ menu: CodeBuddy + three enabled CodeBuddy:<profile> entries
one message with three profile mentions: three independent child delegations
child labels: retain their profile snapshot after rename/delete
invalid/deleted profile draft: explicit failure, no default fallback
```

- [ ] **Step 5: Review scope and commit verification fixes**

Run `git diff --check`, compare every design requirement to an implemented test, and confirm no `TBD`, `TODO`, placeholder, copied profile secret, or new `AgentType` exists.

Commit any verification-only fixes:

```bash
git add -u
git commit -m "fix(delegation): close profile verification gaps"
```
