# Incremental Reference Search Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the composer's all-at-once `@` lookup with a cache-first, independently paged, regex-capable reference controller whose selection remains stable across refresh, validation, cancellation, and IME input.

**Architecture:** A process-wide Rust registry owns guarded source identities, pull cursors, replay, cancellation, deadlines, concurrency, and the backend-owned result limit for files, cross-project conversations, and current-repository commits. A window-scoped frontend cache and controller synchronously publish enabled agents/profiles and safe cached previews, then merge each authoritative source revision independently while preserving selection by URI. Tauri and Axum expose one source-neutral protocol and the existing popup becomes a subscriber rather than the owner of one aggregate Promise.

**Tech Stack:** Rust 2021, Tokio, `tokio-util::CancellationToken`, SeaORM/SQLite, `ignore`, Rust `regex`, Git subprocess streaming, Tauri 2, Axum, React 19, TypeScript strict mode, Zustand, Tiptap 3, Vitest, next-intl.

## Global Constraints

- This plan starts after the automatic-conversation-title plan, which owns `ConversationExperienceSettings`, its revision event/store, and the persisted `set_reference_search_limit_persisted_core` interface.
- Bare `@` synchronously shows every enabled base agent and every effectively enabled delegation profile and starts no file, conversation, commit, regex, profile-load, or candidate-validation request.
- A profile is effectively enabled only when delegation is globally enabled, the profile is enabled, and its backing base agent is enabled; runtime availability is display metadata, not a mention-time probe.
- Non-empty queries start file, all-project root-conversation, and current-repository commit searches independently; no source waits on another source or on `Promise.all`.
- Every first and subsequent resource page contains at most five items; each source stops at the backend-owned global limit, default 50 and clamped to 10 through 500 independently per source.
- The exact Unicode text after `@` is query identity; it is never trimmed or normalized. ASCII case-sensitive `re:` selects Rust regex mode and only that prefix is removed for compilation.
- Literal queries are at most 512 UTF-8 bytes; regex patterns are non-empty, at most 256 UTF-8 bytes, and compile with a 1 MiB automaton size limit.
- Regex descriptor requests contain at most 1,024 rows; each row has a non-empty ID of at most 1,024 UTF-8 bytes, at most 1,024 total field slots, and at most 4,096 searchable UTF-8 bytes. Only that HTTP route raises Axum's body cap, to 64 MiB.
- `searchSessionId`, `requestId`, and `validationRequestId` are canonical UUIDv4 strings; `sourceSequence` is a positive JavaScript safe integer and is ordered only within one controller/source.
- `workspacePath` is required and non-empty for file/commit search and validation, and must be omitted for backend-global conversation operations.
- Registry identity is `(searchSessionId, source)` guarded by `(sourceSequence, requestId)`; higher starts replace, equal identical starts join/replay, lower starts fail as `stale_start`, and cancel-before-register uses a 30-second tombstone.
- Guard high-water records live five minutes with a process cap of 256; registered jobs are capped at 64 total, 24 file, 32 conversation, and eight commit.
- At most 12 page scans run process-wide and at most four per source; idle entries and each page request have separate 30-second deadlines.
- The registry retains immutable page zero and the latest page only, so an entry stores at most ten candidates; final pages release source resources immediately but remain replayable for 30 seconds.
- Window cache is backend/scope isolated, has one candidate LRU capped by both 10,000 entries and 64 MiB of retained UTF-8 candidate data plus one 200-expression regex-snapshot LRU, never evicts pinned selected/visible entries, and is cleared only by backend identity reset/logout.
- Conversation event/page ordering uses a separate backend/URI watermark LRU capped at 10,000 entries so an older in-flight page cannot roll back a newer upsert/status event.
- `not_match` removes only the current provisional membership and retains refreshed resource metadata; only `not_found` evicts the URI and all regex references.
- Commit cache identity includes canonical repository, branch/detached marker, and HEAD; an epoch mismatch is `source_epoch_changed`, not deletion.
- Selection identity is the stable reference URI. Pages, ranking, insertion, and refresh do not move a still-valid selection; explicit invalidation chooses the nearest survivor.
- During IME composition, mention search, navigation, insertion, and submit do nothing; `compositionend` rematches once in a microtask after ProseMirror applies final text.
- Both Tauri and Axum call the same cores and return the same typed source-local errors; every production behavior begins with a focused failing test.
- Conversation-experience setters hold the shared process-local mutation gate through persistence, runtime epoch/cancellation effects, and event emission; delegation mutations likewise serialize persistence plus broker application on a broker-owned gate.
- Frontend files use no semicolons, two spaces, trailing commas, and `@/*` imports; Rust remains valid in desktop and `--no-default-features` server builds.

---

## Prerequisite Contract

This document is independently reviewable: every consumed shared symbol is repeated below. Execution is intentionally sequential, however; the completed automatic-title plan must provide these exact symbols before Task 1 begins:

```rust
pub struct ConversationExperienceSettings {
    pub auto_title_agent: Option<AgentType>,
    pub reference_search_limit: u16,
    pub revision: u64,
}

pub async fn set_reference_search_limit_persisted_core(
    conn: &DatabaseConnection,
    limit: u16,
) -> Result<ConversationExperienceSettings, AppCommandError>;

pub struct ConversationExperienceMutationGate {
    inner: tokio::sync::Mutex<()>,
}

impl ConversationExperienceMutationGate {
    pub async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()>;
}
```

The frontend baseline contains `useConversationExperienceStore`, whose snapshot already carries `reference_search_limit` and applies only strictly newer revisions. This plan extends those interfaces; it does not replace their keys, event name, or revision rules.

## File Map

- `src-tauri/src/reference_search/types.rs`: transport requests/pages/candidates, source enums, rank metadata, validation union, error mapping, identity validation, canonical URI codecs, and constants.
- `src-tauri/src/reference_search/matcher.rs`: exact query parsing, literal/regex field matching, bounds, Rust-authoritative ranking, and descriptor batching core.
- `src-tauri/src/reference_search/registry.rs`: sequence guards, tombstones, job caps, page replay, shared in-flight work, idle/deadline cleanup, limit epochs, and concurrency permits.
- `src-tauri/src/reference_search/sources/mod.rs`: source cursor enum and common page contract.
- `src-tauri/src/reference_search/sources/file.rs`: shared canonical open-workspace resolver and stable ignore-aware pull walker.
- `src-tauri/src/reference_search/sources/conversation.rs`: cross-project SQLite keyset cursor with bounded seen IDs.
- `src-tauri/src/reference_search/sources/commit.rs`: cancellable streaming `git log` cursor consuming the canonical Git identity resolved by `commands/folders.rs`.
- `src-tauri/src/reference_search/validation.rs`: source-specific URI validation and `match`/`not_match`/`not_found` outcomes.
- `src-tauri/src/reference_search/mod.rs`: focused exports and registry construction.
- `src-tauri/src/commands/reference_search.rs`: shared command cores, regex helper, validation, settings-limit runtime update, and Tauri wrappers.
- `src-tauri/src/commands/mod.rs`: exports the reference-search command module.
- `src-tauri/src/commands/conversation_experience.rs`: wraps persisted reference limit with registry epoch cancellation and full settings event.
- `src-tauri/src/commands/delegation.rs`: revisioned delegation profile catalog transactions and event emission.
- `src-tauri/src/acp/delegation/broker.rs`: broker-owned mutation gate that keeps concurrent settings/profile persistence and live config application in commit order.
- `src-tauri/src/acp/delegation/types.rs`: `DelegationProfileCatalog` mirror.
- `src-tauri/src/commands/folders.rs`: exposes the existing ignore-aware builder internally and owns canonical repository, full `head_sha`, `CommitSourceEpoch`, and the reference-source epoch returned in `GitHeadInfo`.
- `src-tauri/src/app_error.rs`: reference protocol error codes.
- `src-tauri/src/web/handlers/error.rs`: HTTP status mapping for the new codes.
- `src-tauri/src/app_state.rs`: server/test `Arc<ReferenceSearchRegistry>`.
- `src-tauri/src/lib.rs`: module exports, desktop managed registry, limit initialization, Tauri command registration, and sweeper startup.
- `src-tauri/src/bin/codeg_server.rs`: server registry construction, authoritative limit initialization, and sweeper startup.
- `src-tauri/src/web/mod.rs`: clones the desktop-managed registry into the embedded Axum `AppState`.
- `src-tauri/src/web/handlers/reference_search.rs`: Axum request mirrors.
- `src-tauri/src/web/handlers/conversation_experience.rs`: exposes the reference-limit setter through the shared persisted settings core and live registry.
- `src-tauri/src/web/handlers/delegation.rs`: profile catalog event parity.
- `src-tauri/src/web/handlers/mod.rs`: exports the reference handler.
- `src-tauri/src/web/router.rs`: reference protocol and settings/catalog routes.
- `src-tauri/tests/api_integration.rs`: HTTP/core parity, cap enforcement, cancellation, replay, and validation integration tests.
- `src/lib/types.ts`: protocol, candidate, settings, profile catalog, Git HEAD, and error mirrors.
- `src/lib/api.ts`: transport-neutral reference/search/settings/catalog calls.
- `src/lib/api.test.ts`: flat reference-protocol payload, timeout, and abort-signal forwarding coverage.
- `src/lib/tauri.ts`: direct Tauri parity wrappers.
- `src/lib/transport/index.ts`: stable current-backend cache identity helper.
- `src/lib/transport/index.test.ts`: local/web/remote cache-key isolation coverage.
- `src/lib/transport/types.ts`: per-call `AbortSignal` support.
- `src/lib/transport/web-transport.ts`: composes caller cancellation with its request timeout.
- `src/lib/transport/web-transport.test.ts`: distinguishes caller abort from timeout.
- `src/lib/transport/tauri-transport.ts`: documents IPC cancellation as generation-guarded only.
- `src/lib/transport/remote-desktop-transport.ts`: documents proxy cancellation as generation-guarded only.
- `src/stores/delegation-profile-store.ts`: backend-scoped revisioned catalog bootstrap/subscription/recovery.
- `src/stores/delegation-profile-store.test.ts`: zero-mention-fetch, stale revision, reconnect, and reset tests.
- `src/stores/conversation-experience-store.ts`: applies reference-limit responses/events and feeds live controller limits.
- `src/stores/conversation-experience-store.test.ts`: reference-limit revision and reset coverage.
- `src/stores/app-workspace-store.ts`: retains complete Git HEAD/repository/epoch identity changes.
- `src/stores/app-workspace-store.test.ts`: same-branch full-HEAD and epoch update regression coverage.
- `src/contexts/app-workspace-context.tsx`: starts agent/profile bootstrap before composer mention enablement and forwards conversation cache events.
- `src/contexts/app-workspace-context.test.tsx`: verifies conversation upsert/status/delete events reach both workspace state and the reference cache.
- `src/lib/reference-search-cache.ts`: global item/regex LRUs, buckets, mutation/event clocks, pins, observable conversation event projection, and reset.
- `src/lib/reference-search-cache.test.ts`: literal/regex reuse, pins, caps, stale membership, and negative validation semantics.
- `src/components/chat/composer/reference-search-controller.ts`: query generations, per-source sequences, independent paging/recovery, regex batching, validation, selection pins, and snapshots.
- `src/components/chat/composer/reference-search-controller.test.ts`: source isolation, cancellation, restart, late result, validation, and selection tests.
- `src/components/chat/composer/use-reference-search.ts`: constructs/updates/closes the controller from shared stores and workspace Git identity, then removes the transitional aggregate hook when all composer callers migrate.
- `src/components/chat/composer/use-reference-search.test.ts`: bare-query network zero, catalog updates, limit changes, and workspace/HEAD transitions.
- `src/components/chat/composer/suggestion/types.ts`: controller/snapshot/group state contract replacing `ReferenceSearch` Promise.
- `src/components/chat/composer/suggestion/adapters.ts`: backend candidate and catalog descriptor adaptation.
- `src/components/chat/composer/suggestion/adapters.test.ts`: camel-case wire adaptation and complete suggestion-shape coverage.
- `src/components/chat/composer/suggestion/suggestion-popup.tsx`: subscriptions, stable URI selection, source-local status, async confirmation, and nearest-survivor logic.
- `src/components/chat/composer/suggestion/suggestion-popup.test.tsx`: stable selection, group pinning, cache validation, source error, and keyboard tests.
- `src/components/chat/composer/suggestion/mention-suggestion.ts`: IME suppression and idempotent post-composition rematch.
- `src/components/chat/composer/suggestion/mention-suggestion.test.ts`: Chinese/English composition and key-229 tests.
- `src/components/chat/composer/rich-composer.tsx`: controller prop, async candidate confirmation, and composition-safe routing.
- `src/components/chat/composer/rich-composer-mention.test.tsx`: end-to-end mention/IME/Enter integration.
- `src/components/chat/chat-input.tsx`: forwards authoritative folder scope through the composer chain.
- `src/components/chat/conversation-shell.tsx`: carries nullable folder scope into active-session chat input.
- `src/components/chat/message-input.tsx`: controller wiring and localized source/pattern labels.
- `src/components/chat/message-input.test.tsx`: nullable folder-scope hook wiring and controller mock coverage.
- `src/components/automations/automation-editor.tsx`: migrates automation prompts to the same scoped controller and localized labels.
- `src/components/conversations/conversation-detail-panel.tsx`: supplies `ownFolderId` to active and welcome composer branches.
- `src/components/settings/delegation-settings.tsx`: loads profiles from the revisioned catalog getter instead of the legacy profile document call.
- `src/components/settings/delegation-settings.test.tsx`: updates catalog API mocks and profile-loading coverage.
- `src/components/settings/conversation-experience-settings.tsx`: adds numeric reference-result limit control.
- `src/components/settings/conversation-experience-settings.test.tsx`: clamp/save/event tests for the limit.
- `src/i18n/messages/ar.json`: Arabic settings/pattern/source/profile copy.
- `src/i18n/messages/de.json`: German settings/pattern/source/profile copy.
- `src/i18n/messages/en.json`: English settings/pattern/source/profile copy.
- `src/i18n/messages/es.json`: Spanish settings/pattern/source/profile copy.
- `src/i18n/messages/fr.json`: French settings/pattern/source/profile copy.
- `src/i18n/messages/ja.json`: Japanese settings/pattern/source/profile copy.
- `src/i18n/messages/ko.json`: Korean settings/pattern/source/profile copy.
- `src/i18n/messages/pt.json`: Portuguese settings/pattern/source/profile copy.
- `src/i18n/messages/zh-CN.json`: Simplified Chinese settings/pattern/source/profile copy.
- `src/i18n/messages/zh-TW.json`: Traditional Chinese settings/pattern/source/profile copy.

---

### Task 1: Publish a Revisioned Delegation Profile Catalog Before Mentions Enable

**Files:**
- Modify: `src-tauri/src/acp/delegation/types.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/commands/delegation.rs`
- Modify: `src-tauri/src/web/handlers/delegation.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/tauri.ts`
- Create: `src/stores/delegation-profile-store.ts`
- Create: `src/stores/delegation-profile-store.test.ts`
- Modify: `src/contexts/app-workspace-context.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`
- Modify: `src/components/settings/delegation-settings.tsx`
- Modify: `src/components/settings/delegation-settings.test.tsx`

**Interfaces:**
- Produces: `DelegationProfileCatalog { profiles: Vec<DelegationProfile>, delegation_enabled: bool, revision: u64 }`
- Produces: `async fn load_delegation_profile_catalog(conn: &DatabaseConnection) -> Result<DelegationProfileCatalog, AppCommandError>`
- Produces: `KEY_DELEGATION_PROFILE_REVISION = "delegation.profile_catalog_revision"`
- Produces: `DELEGATION_PROFILE_CATALOG_CHANGED_EVENT = "delegation-profile-catalog://changed"`
- Produces: frontend `getDelegationProfileCatalog() -> Promise<DelegationProfileCatalog>` through both transport facades
- Produces: `useDelegationProfileStore` with `ready`, `error`, `catalog`, idempotent `initialize`, force-fetching `refresh`, and revision-gated `applyCatalog`
- Changes: all three delegation mutation cores acquire the broker-owned mutation gate before opening their write transaction and hold it through the corresponding full live-config application; `set_delegation_profiles_core` therefore also accepts `&DelegationBroker`
- Produces: `async fn DelegationBroker::configuration_mutation_guard(&self) -> tokio::sync::MutexGuard<'_, ()>` over a mutex separate from the broker's config lock; command cores acquire it explicitly, while `set_config`/`set_profiles` never reacquire it

- [ ] **Step 1: Write failing atomic revision and bootstrap-store tests**

```rust
#[tokio::test]
async fn every_catalog_affecting_save_advances_revision_in_its_transaction() {
    let db = crate::db::test_helpers::fresh_in_memory_db().await;
    let broker = make_broker();
    let settings = set_delegation_settings_core(
        &db.conn,
        &broker,
        DelegationSettings { enabled: true, ..Default::default() },
    )
    .await
    .expect("settings");
    let profiles = set_delegation_profiles_core(
        &db.conn,
        &broker,
        DelegationProfileDocument {
            profiles: vec![profile(
                "11111111-1111-4111-8111-111111111111",
                "A",
            )],
        },
    )
    .await
    .expect("profiles");
    assert_eq!(settings.catalog.revision, 1);
    assert_eq!(profiles.catalog.revision, 2);
    assert!(profiles.catalog.delegation_enabled);
    let live = broker.config_snapshot().await;
    assert!(live.enabled);
    assert_eq!(live.profiles.len(), 1);
}
```

```ts
it("initializes once and drops stale catalog events", async () => {
  await useDelegationProfileStore.getState().initialize()
  mocks.getDelegationProfileCatalog.mockClear()
  useDelegationProfileStore.getState().applyCatalog({
    profiles: [],
    delegation_enabled: false,
    revision: 0,
  })
  await useDelegationProfileStore.getState().initialize()
  expect(mocks.getDelegationProfileCatalog).not.toHaveBeenCalled()
  expect(useDelegationProfileStore.getState().catalog?.revision).toBe(1)
})
```

In `delegation-profile-store.test.ts`, create the API mock with `const mocks = vi.hoisted(() => ({ getDelegationProfileCatalog: vi.fn() }))` before using it from the hoisted `vi.mock` factory, and make its first response `{ profiles: [], delegation_enabled: true, revision: 1 }`; a plain top-level object is unsafe because Vitest hoists `vi.mock` above its initialization. Reset the store and mock between tests. The Rust test reuses the existing local `make_broker()` and two-argument `profile(id, name)` helpers already present in `commands::delegation::tests`.

Add `failed_bootstrap_is_ready_with_error_and_focus_refresh_recovers`: reject the initial getter, require `ready: true`, capture the bootstrap's focus callback, then resolve revision 2 and invoke that callback; assert the catalog/error converge without opening a mention. This distinguishes one-time `initialize` from force-fetching `refresh`. Add `successful_equal_revision_refresh_clears_a_transient_error_without_replacing_catalog`: seed revision 2, fail one refresh, then return the same revision 2 and require `error === null`.

Add `concurrent_settings_and_profile_saves_get_distinct_revisions`: keep a `TempDir` alive, open it through `crate::db::init_database(temp.path(), "profile-catalog-concurrency-test")`, run one settings-only save and one profiles-only save against the same broker with `tokio::join!`, sort the returned catalog revisions and assert `[1, 2]`, then load one catalog snapshot and assert it contains both committed values at revision `2`. Finally assert the broker snapshot has the same effective enabled flag and normalized profile map as that revision-2 database snapshot. An in-memory single-connection fixture, sequential saves, or checking only persisted rows does not test the write-plus-runtime ordering contract.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils every_catalog_affecting_save_advances_revision_in_its_transaction
cargo test --features test-utils concurrent_settings_and_profile_saves_get_distinct_revisions
cd ..
pnpm test -- src/stores/delegation-profile-store.test.ts src/contexts/app-workspace-context.test.tsx src/components/settings/delegation-settings.test.tsx
```

Expected: FAIL because the catalog has no revision/event or backend-scoped bootstrap store.

- [ ] **Step 3: Implement catalog transactions, event emission, and bootstrap store**

Add the wire type without changing profile setter input:

```rust
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationProfileCatalog {
    #[serde(default)]
    pub profiles: Vec<DelegationProfile>,
    pub delegation_enabled: bool,
    pub revision: u64,
}

pub struct DelegationMutation<T> {
    pub value: T,
    pub catalog: DelegationProfileCatalog,
}
```

Add a broker-owned `config_mutation: tokio::sync::Mutex<()>` used only to order persisted configuration mutations, plus `configuration_mutation_guard()` returning its guard. Keep it separate from the existing config lock, and do not make `set_config` or `set_profiles` acquire it themselves, because the mutation cores call those methods while already holding the outer guard. Each settings-only, profiles-only, and bundle core acquires it before opening the write transaction and holds it until the full corresponding broker config has been applied after commit. Change `set_delegation_profiles_core` to accept `&DelegationBroker` and move its `set_profiles` application into the gated core; delete the now-redundant `apply_profiles_to_broker` helper and remove it from both wrapper imports. Wrappers must not perform a second ungated application. This keeps separate desktop/HTTP requests on the same broker from applying an older catalog after a newer transaction.

Move revision increment into each gated settings-only, profiles-only, and bundle transaction. Use the same write-first revision-row algorithm as `ConversationExperienceSettings`: insert revision zero if absent, atomically increment it before any catalog read, then load normalized profiles plus effective delegation settings from that transaction. This prevents concurrent settings/profile windows from returning the same revision. Return `DelegationMutation<T>`, apply the broker only after commit while the gate is still held, and have Tauri/Axum wrappers emit `mutation.catalog` after the core returns before returning `mutation.value`. Add a read-only `get_delegation_profile_catalog` route/command whose read transaction returns settings, profiles, and revision from one snapshot. Use strict generic transaction readers for operational database errors; retain the existing documented fallback only for missing/malformed persisted preference values.

The frontend store initializes outside mention opening, subscribes once to the catalog event, accepts only a strictly higher revision, retries on focus/reconnect, enters `ready: true` even on load failure, and registers `resetDelegationProfileStore` with the backend-scoped reset registry. `initialize()` owns one shared in-flight bootstrap and installs listeners once; subsequent calls are no-ops. `refresh()` always starts a request (coalescing only a currently in-flight refresh), retains the last good catalog on failure, sets `ready: true` plus `error`, and is what the installed focus/reconnect callbacks call. On any successful refresh, call `applyCatalog(response)` and then clear `error` even when the response revision equals the current catalog and the revision gate correctly declines to rewrite it. The mention controller reads state only and never calls either method:

```ts
applyCatalog: (incoming) => {
  const current = get().catalog
  if (current && incoming.revision <= current.revision) return
  set({ catalog: incoming, ready: true, error: null })
},
```

In `AppWorkspaceProvider`, call both `useAcpAgents()` and `useDelegationProfileBootstrap()` at mount. Export a `referenceCatalogReady` selector that becomes true once agents are fresh and the profile store is ready; a profile error still permits agent-only mentions. In `app-workspace-context.test.tsx`, mock `useDelegationProfileBootstrap` as a hoisted spy/no-op beside the automatic-title bootstrap mock and assert both hooks are called; keeping these bootstrap subscriptions mocked lets the file's conversation/folder event harness retain one handler per channel. Migrate both initial-load and failure-resync calls in `DelegationSettingsSection` from `getDelegationProfiles()` to `getDelegationProfileCatalog()`, taking its `profiles` array while continuing to load the full delegation settings document separately. Update `delegation-settings.test.tsx` to mock the new getter as `{ profiles: [], delegation_enabled: false, revision: 0 }` and remove its old getter mock/import. This leaves `getDelegationProfiles` used only by the transitional aggregate mention hook. Task 8 tests the actual bare-mention zero-request contract after the controller exists. Task 8 adds the controller hook beside the legacy aggregate hook so that intermediate commit still type-checks; Task 9 removes `getDelegationProfiles`, its focus cache, and the legacy hook after migrating every remaining composer caller.

- [ ] **Step 4: Verify atomic catalog convergence and bootstrap recovery**

Run:

```powershell
cd src-tauri
cargo test --features test-utils commands::delegation::tests
cargo check --no-default-features --bin codeg-server
cd ..
pnpm test -- src/stores/delegation-profile-store.test.ts src/components/settings/delegation-settings.test.tsx
```

Expected: all commands exit 0; every catalog-affecting save emits one post-commit revision, old events are ignored, and mention opening performs no profile request.

- [ ] **Step 5: Commit the shared profile catalog**

```powershell
git add src-tauri/src/acp/delegation/types.rs src-tauri/src/acp/delegation/broker.rs src-tauri/src/commands/delegation.rs src-tauri/src/web/handlers/delegation.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src/lib/types.ts src/lib/api.ts src/lib/tauri.ts src/stores/delegation-profile-store.ts src/stores/delegation-profile-store.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx src/components/settings/delegation-settings.tsx src/components/settings/delegation-settings.test.tsx
git commit -m "feat(references): publish delegation profile catalog"
```

---

### Task 2: Define the Source-Neutral Protocol and Authoritative Matcher

**Files:**
- Create: `src-tauri/src/reference_search/mod.rs`
- Create: `src-tauri/src/reference_search/types.rs`
- Create: `src-tauri/src/reference_search/matcher.rs`
- Create: `src-tauri/src/commands/reference_search.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/src/app_error.rs`
- Modify: `src-tauri/src/web/handlers/error.rs`

**Interfaces:**
- Produces: `ReferenceSearchSource::{File, Conversation, Commit}` with `Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash` and snake-case wire values
- Produces: request/page/validation Rust mirrors of the approved TypeScript protocol
- Produces: `ReferenceCandidate { source, uri, id, label, detail, keywords, metadata, source_ordinal, regex_rank }` with the exact tagged metadata variants below
- Produces: `parse_canonical_uuid_v4(value: &str) -> Result<Uuid, ReferenceSearchError>`, reused by search, cancel, page, and validation identities
- Produces: `validate_source_scope(source: ReferenceSearchSource, workspace_path: Option<&str>) -> Result<(), ReferenceSearchError>`
- Produces: `validate_source_epoch_scope(source: ReferenceSearchSource, source_epoch: Option<&str>) -> Result<(), ReferenceSearchError>` for candidate validation
- Produces: `SearchPattern::parse(query: &str) -> Result<SearchPattern, ReferenceSearchError>`
- Produces: `match_fields(pattern: &SearchPattern, primary: &[&str], secondary: &[&str]) -> Option<ReferenceFieldMatch>`
- Produces: `match_reference_candidate(pattern: &SearchPattern, candidate: &ReferenceCandidate) -> Option<ReferenceFieldMatch>`, the one resource-field mapping used by file/conversation/commit search and validation
- Produces: `match_reference_regex_core(request: MatchReferenceRegexRequest) -> Result<Vec<ReferenceRegexMatch>, AppCommandError>`
- Produces: `encode_uri_component`, `build_file_uri`, `build_session_uri`, and `build_commit_uri`; all authoritative resource candidates use these Rust codecs

- [ ] **Step 1: Write failing identity, bounds, field, and rank tests**

```rust
#[test]
fn query_identity_and_regex_bounds_are_exact() {
    assert!(matches!(SearchPattern::parse("").unwrap_err().code, AppErrorCode::InvalidRequest));
    assert!(matches!(SearchPattern::parse(" File ").unwrap(), SearchPattern::Literal { raw, .. } if raw == " File "));
    assert!(matches!(SearchPattern::parse("re:(?i)^src/").unwrap(), SearchPattern::Regex { raw, .. } if raw == "re:(?i)^src/"));
    assert!(matches!(SearchPattern::parse("re:").unwrap_err().code, AppErrorCode::InvalidPattern));
    assert!(matches!(SearchPattern::parse(&format!("re:{}", "x".repeat(257))).unwrap_err().code, AppErrorCode::InvalidPattern));
    assert!(matches!(SearchPattern::parse(&"x".repeat(513)).unwrap_err().code, AppErrorCode::InvalidRequest));
}

#[test]
fn best_field_rank_cannot_be_replaced_by_secondary_match() {
    let pattern = SearchPattern::parse("read").unwrap();
    let rank = match_fields(
        &pattern,
        &["README.md"],
        &["project/read archive"],
    )
    .expect("match");
    assert_eq!(rank.field_tier, 1);
}

#[test]
fn uuid_and_sequence_validation_rejects_non_v4_or_unsafe_values() {
    assert!(SearchIdentity::parse(UUID_V4, 1, UUID_V4).is_ok());
    assert!(SearchIdentity::parse("not-a-uuid", 1, UUID_V4).is_err());
    assert!(SearchIdentity::parse(UUID_V4, 0, UUID_V4).is_err());
    assert!(SearchIdentity::parse(UUID_V4, 9_007_199_254_740_992, UUID_V4).is_err());
}

#[test]
fn source_scope_requires_workspace_only_for_file_and_commit() {
    assert!(validate_source_scope(ReferenceSearchSource::File, Some("workspace-root")).is_ok());
    assert!(validate_source_scope(ReferenceSearchSource::Commit, Some("workspace-root")).is_ok());
    assert!(validate_source_scope(ReferenceSearchSource::Conversation, None).is_ok());
    assert!(validate_source_scope(ReferenceSearchSource::File, None).is_err());
    assert!(validate_source_scope(ReferenceSearchSource::Commit, Some("")).is_err());
    assert!(validate_source_scope(ReferenceSearchSource::Conversation, Some("workspace-root")).is_err());
    assert!(validate_source_epoch_scope(ReferenceSearchSource::Commit, Some("v1:epoch")).is_ok());
    assert!(validate_source_epoch_scope(ReferenceSearchSource::Commit, None).is_err());
    assert!(validate_source_epoch_scope(ReferenceSearchSource::File, Some("v1:epoch")).is_err());
}

#[test]
fn canonical_resource_uris_match_the_existing_frontend_codec() {
    assert_eq!(build_file_uri(Path::new("/repo/a b#c.ts")), "file:///repo/a%20b%23c.ts");
    assert_eq!(build_file_uri(Path::new(r"C:\repo\app.ts")), "file:///C%3A/repo/app.ts");
    assert_eq!(build_file_uri(Path::new(r"\\server\share\文档.md")), "file://server/share/%E6%96%87%E6%A1%A3.md");
    assert_eq!(build_file_uri(Path::new(r"\\?\C:\repo\app.ts")), "file:///C%3A/repo/app.ts");
    assert_eq!(build_file_uri(Path::new(r"\\?\UNC\server\share\文档.md")), "file://server/share/%E6%96%87%E6%A1%A3.md");
    assert_eq!(build_session_uri(42), "codeg://session/42");
    assert_eq!(
        build_commit_uri("/repo with space", "abc123"),
        "codeg://commit/%2Frepo%20with%20space@abc123",
    );
}
```

Define `const UUID_V4: &str = "11111111-1111-4111-8111-111111111111"` in `reference_search::types::tests`; do not rely on a fixture imported from another task.

- [ ] **Step 2: Run matcher/protocol tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils query_identity_and_regex_bounds_are_exact
cargo test --features test-utils best_field_rank_cannot_be_replaced_by_secondary_match
cargo test --features test-utils resource_candidates_use_the_approved_fields_in_declared_order
cargo test --features test-utils uuid_and_sequence_validation_rejects_non_v4_or_unsafe_values
cargo test --features test-utils source_scope_requires_workspace_only_for_file_and_commit
cargo test --features test-utils canonical_resource_uris_match_the_existing_frontend_codec
```

Expected: FAIL because the protocol and matcher modules do not exist.

- [ ] **Step 3: Implement exact types, typed errors, and matching semantics**

Define every transport struct below with the shown derives and `#[serde(rename_all = "camelCase")]`; `Clone` is required for idempotent-start tests and registry-owned tasks, while `PartialEq, Eq` is required for exact replay assertions:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartReferenceSearchRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
    pub query: String,
    pub workspace_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NextReferenceSearchPageRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
    pub page_index: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelReferenceSearchRequest {
    pub search_session_id: String,
    pub source_sequence: u64,
    pub request_id: String,
    pub source: ReferenceSearchSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidateReferenceCandidateRequest {
    pub validation_request_id: String,
    pub source: ReferenceSearchSource,
    pub uri: String,
    pub query: String,
    pub workspace_path: Option<String>,
    pub source_epoch: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceDoneReason {
    Exhausted,
    Limit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceSearchPage {
    pub source_sequence: u64,
    pub request_id: String,
    pub page_index: u32,
    pub items: Vec<ReferenceCandidate>,
    pub source_epoch: Option<String>,
    pub done: bool,
    pub done_reason: Option<ReferenceDoneReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceRegexRank {
    pub field_tier: u32,
    pub start: u32,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceFieldMatch {
    pub field_tier: u32,
    pub regex_rank: Option<ReferenceRegexRank>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ReferenceCandidateMetadata {
    File {
        canonical_workspace_root: String,
        relative_path: String,
        entry_kind: ReferenceFileKind,
    },
    Conversation {
        conversation_id: i32,
        agent_type: AgentType,
        status: String,
        branch: Option<String>,
        project_name: String,
        project_path: String,
    },
    Commit {
        canonical_repo: String,
        full_hash: String,
        short_hash: String,
        subject: String,
        message: String,
        author: String,
        authored_at: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceFileKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceCandidate {
    pub source: ReferenceSearchSource,
    pub uri: String,
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    pub keywords: String,
    pub metadata: ReferenceCandidateMetadata,
    pub source_ordinal: u64,
    pub regex_rank: Option<ReferenceRegexRank>,
}
```

Add these `AppErrorCode` variants and derive `PartialEq, Eq` on the enum. Map pattern/request to HTTP 400, ordering/control results to 409, expiry to 410, overload to 429, timeout to 408, and source failure to 500: `Cancelled`, `StaleStart`, `JobExpired`, `StalePage`, `LimitEpochChanged`, `InvalidPattern`, `SourceEpochChanged`, `SourceTimeout`, `RegistryOverloaded`, `SourceFailed`, and `InvalidRequest`. `ReferenceSearchError.code` is this same `AppErrorCode`; do not introduce a second `ReferenceErrorCode` enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ReferenceSearchError {
    pub code: AppErrorCode,
    pub message: String,
}

impl From<ReferenceSearchError> for AppCommandError {
    fn from(error: ReferenceSearchError) -> Self {
        AppCommandError::new(error.code, error.message)
    }
}
```

`parse_canonical_uuid_v4` parses with `Uuid::parse_str`, requires `get_version_num() == 4`, and requires `parsed.hyphenated().to_string() == value`; uppercase, braced, simple, and URN spellings are rejected even when the UUID crate could parse them. `SearchIdentity::parse` and candidate validation both call this helper rather than maintaining separate UUID rules.

`validate_source_scope` accepts `Some(non_empty)` only for `File`/`Commit` and `None` only for `Conversation`; every other combination is `InvalidRequest`. Registry start calls it after advancing the sequence high-water mark but before acquiring a registered-job slot, validation calls it before URI parsing, and the frontend omits `workspacePath` from conversation requests rather than sending the current folder gratuitously.

`validate_source_epoch_scope` requires `Some(non_empty)` for `Commit` validation and `None` for `File`/`Conversation`; it rejects missing commit epochs and irrelevant epochs as `InvalidRequest` before any filesystem, database, or Git lookup.

Reject the empty query as `InvalidRequest`; bare `@` is a frontend-local catalog state, not a resource-search protocol request. Literal matching uses Unicode lowercase only for comparison and preserves raw identity. Rank exact primary as tier 0, primary prefix as 1, primary word boundary as 2, primary substring as 3, and every secondary match as tier 4; use declared primary/secondary field index only as an internal tie-breaker, not as a new literal tier. A word boundary is the start of a field or a position whose preceding Unicode scalar is neither alphanumeric nor `_`. When several fields match, retain the lexicographically best `(field_tier, declared_field_index)` result. In regex mode, `ReferenceRegexRank.field_tier` is instead the flattened declared field ordinal: primary indices first, then secondary indices offset by `primary.len()`. That encoding makes primary fields and declared order authoritative before byte start/length. `ReferenceFieldMatch` carries the mode-appropriate tier and optional `ReferenceRegexRank`; sources copy regex metadata only in regex mode. Regex uses:

```rust
RegexBuilder::new(pattern)
    .size_limit(1024 * 1024)
    .build()
    .map_err(|error| ReferenceSearchError::invalid_pattern(error.to_string()))?
```

Implement `match_reference_candidate` with this exact declared order. Preserve every declared slot: an absent conversation branch contributes `""`, which cannot match a non-empty query but keeps project field tiers identical across candidates.

| Candidate metadata | Primary fields | Secondary fields |
| --- | --- | --- |
| `File` | `candidate.label` (entry name) | `relative_path` |
| `Conversation` | `candidate.label` (folded title or `#<id>` fallback) | `candidate.id`, snake-case serialized `agent_type`, `status`, `branch.unwrap_or("")`, `project_name`, `project_path` |
| `Commit` | `short_hash`, `full_hash`, `subject` | `message`, `author` |

Add `resource_candidates_use_the_approved_fields_in_declared_order` in `matcher::tests`: construct one complete candidate of each metadata variant, prove file relative path/conversation project/commit author are tier-4 matches, prove commit subject is a primary match, and assert the regex `field_tier` values reflect the exact flattened order above. File, conversation, commit, cache validation, and candidate validation must call this helper instead of rebuilding field arrays.

Return regex field ordinals and byte offsets directly after checked conversion to `u32`. Do not narrow `field_tier` to `u8`: fixed resource candidates have few fields, but the descriptor protocol accepts up to 1,024 caller-provided field slots and therefore has no 255-field contract. Define `ReferenceDescriptor { id: String, source_ordinal: u64, primary: Vec<String>, secondary: Vec<String> }`, `MatchReferenceRegexRequest { query: String, descriptors: Vec<ReferenceDescriptor> }`, and `ReferenceRegexMatch { id: String, source_ordinal: u64, rank: ReferenceRegexRank }`, all with camel-case wire fields. The descriptor helper accepts at most 1,024 rows; each row requires a unique non-empty ID of at most 1,024 UTF-8 bytes, at least one and at most 1,024 combined primary/secondary slots, and at most 4,096 UTF-8 searchable bytes across those fields. It rejects oversized/duplicate input or an ordinal that cannot fit `u32` as `InvalidRequest`, returns stable IDs plus authoritative rank, and never truncates matches within the accepted batch. Add unit cases for every exact boundary and one-over rejection, including more than 255 field slots to preserve the `u32` contract.

Implement `encode_uri_component` over UTF-8 bytes with exactly JavaScript `encodeURIComponent`'s unescaped ASCII set (`A-Z a-z 0-9 - _ . ! ~ * ' ( )`) and uppercase `%HH`. Before URI classification, normalize `\` to `/`, strip Windows `//?/C:/...` to `C:/...`, and map `//?/UNC/server/share/...` back to `//server/share/...`; this is required because `std::fs::canonicalize` produces verbatim paths on Windows. `build_file_uri` segment-encodes while preserving separators, emits authority form for UNC paths, `file://<encoded>` for POSIX absolute paths, and `file:///<encoded>` for Windows drive paths. `build_session_uri` accepts only a positive database ID. `build_commit_uri` component-encodes the entire canonical repository string before appending `@<full_hash>`. The Task 4/5 sources call these helpers rather than rebuilding URIs ad hoc; Task 5 validates and decodes the same grammar.

- [ ] **Step 4: Verify matcher behavior and both Rust modes**

Run:

```powershell
cd src-tauri
cargo test --features test-utils reference_search::matcher::tests
cargo test --features test-utils reference_search::types::tests
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; Rust regex is authoritative, unsupported lookaround/backreferences fail consistently, and rank tuples follow declared field order.

- [ ] **Step 5: Commit protocol and matcher foundations**

```powershell
git add src-tauri/src/reference_search/mod.rs src-tauri/src/reference_search/types.rs src-tauri/src/reference_search/matcher.rs src-tauri/src/commands/reference_search.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs src-tauri/src/app_error.rs src-tauri/src/web/handlers/error.rs
git commit -m "feat(references): define search protocol and matcher"
```

---

### Task 3: Implement Guarded Pull Jobs, Replay, Deadlines, and Limit Epochs

**Files:**
- Create: `src-tauri/src/reference_search/registry.rs`
- Create: `src-tauri/src/reference_search/sources/mod.rs`
- Modify: `src-tauri/src/reference_search/mod.rs`
- Modify: `src-tauri/src/reference_search/types.rs`

**Interfaces:**
- Produces: `SourcePage { items: Vec<ReferenceCandidate>, source_epoch: Option<String>, done: bool, done_reason: Option<ReferenceDoneReason> }`
- Produces: `#[async_trait] trait ReferenceSourceCursor: Send { async fn next_page(&mut self, page_size: usize, token: CancellationToken) -> Result<SourcePage, AppCommandError>; async fn close(&mut self); }`
- Produces: `#[async_trait] trait ReferenceSourceFactory: Send + Sync { async fn open(&self, request: &StartReferenceSearchRequest, pattern: SearchPattern, limit: usize) -> Result<Box<dyn ReferenceSourceCursor>, AppCommandError>; }`
- Produces: `fn ReferenceSearchRegistry::new(limit: u16, factory: Arc<dyn ReferenceSourceFactory>) -> Arc<Self>`
- Produces: `async fn start(&self, request: StartReferenceSearchRequest) -> Result<ReferenceSearchPage, AppCommandError>`
- Produces: `async fn next_page(&self, request: NextReferenceSearchPageRequest) -> Result<ReferenceSearchPage, AppCommandError>`
- Produces: `async fn cancel(&self, request: CancelReferenceSearchRequest) -> Result<bool, AppCommandError>`
- Produces: `async fn set_limit(&self, limit: u16) -> u64` returning the new limit epoch after cancelling every old-epoch job under the registry lock
- Produces: `async fn sweep_expired(&self, now: Instant)` and `async fn run_reference_search_sweeper(registry: Arc<ReferenceSearchRegistry>)`

- [ ] **Step 1: Write failing ordering, pre-cancel, replay, cap, and timeout tests**

```rust
#[tokio::test]
async fn pre_cancel_tombstone_beats_a_reordered_start() {
    let registry = test_registry(50);
    let request = start_request(ReferenceSearchSource::File, 4, UUID_A);
    assert!(!registry.cancel(request.cancel_request()).await.unwrap());
    let error = registry.start(request).await.expect_err("pre-cancelled");
    assert!(matches!(error.code, AppErrorCode::Cancelled));
    assert_eq!(registry.registered_count().await, 0);
}

#[tokio::test]
async fn duplicate_start_and_latest_page_share_or_replay_work() {
    let registry = registry_with_cursor(12);
    let start = start_request(ReferenceSearchSource::Conversation, 1, UUID_A);
    let (a, b) = tokio::join!(registry.start(start.clone()), registry.start(start.clone()));
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(registry.cursor_advance_count(), 1);
    let page1 = registry.next_page(start.next_request(1)).await.unwrap();
    let replay = registry.next_page(start.next_request(1)).await.unwrap();
    assert_eq!(page1, replay);
    assert_eq!(registry.cursor_advance_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn registered_and_scan_caps_are_enforced_exactly() {
    for (source, cap) in [
        (ReferenceSearchSource::File, 24),
        (ReferenceSearchSource::Conversation, 32),
        (ReferenceSearchSource::Commit, 8),
    ] {
        let registry = test_registry(50);
        seed_registered(&registry, source, cap).await;
        assert_overloaded(registry.start(unique_start(source)).await);
    }

    let registry = test_registry(50);
    seed_registered(&registry, ReferenceSearchSource::File, 24).await;
    seed_registered(&registry, ReferenceSearchSource::Conversation, 32).await;
    seed_registered(&registry, ReferenceSearchSource::Commit, 8).await;
    assert_eq!(registry.registered_count().await, 64);
    assert_overloaded(registry.start(unique_start(ReferenceSearchSource::File)).await);

    let scans = blocked_scan_fixture().await;
    scans.start_distinct_sources_to_global_cap(12).await;
    assert_eq!(scans.started_scan_count(), 12);
    let mut thirteenth = scans.spawn_one_more(ReferenceSearchSource::Conversation);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut thirteenth).await.is_err());
    scans.release_one(ReferenceSearchSource::Conversation).await;
    assert_eq!(scans.wait_for_started_scan_count(13).await, 13);
    scans.release_all().await;
    thirteenth.await.unwrap().unwrap();

    let per_source = blocked_scan_fixture().await;
    per_source.start_source_to_scan_cap(ReferenceSearchSource::Commit, 4).await;
    let mut fifth = per_source.spawn_one_more(ReferenceSearchSource::Commit);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut fifth).await.is_err());
    per_source.release_one(ReferenceSearchSource::Commit).await;
    assert_eq!(per_source.wait_for_started_scan_count(5).await, 5);
    per_source.release_all().await;
    fifth.await.unwrap().unwrap();

    let fairness = blocked_scan_fixture().await;
    fairness.spawn_source_backlog(ReferenceSearchSource::File, 12).await;
    assert_eq!(fairness.wait_for_source_scan_count(ReferenceSearchSource::File, 4).await, 4);
    let mut conversation = fairness.spawn_one_more(ReferenceSearchSource::Conversation);
    fairness
        .wait_for_source_scan_count(ReferenceSearchSource::Conversation, 1)
        .await;
    assert!(!conversation.is_finished());
    fairness.release_all().await;
    conversation.await.unwrap().unwrap();
}

#[tokio::test]
async fn guard_table_cap_rejects_new_identity_without_evicting_live_high_water() {
    let registry = test_registry(50);
    let retained = unique_start(ReferenceSearchSource::File);
    assert!(!registry.cancel(retained.cancel_request()).await.unwrap());
    seed_cancel_guards(&registry, 255).await;
    let overflow = unique_start(ReferenceSearchSource::Conversation);
    assert_overloaded(registry.cancel(overflow.cancel_request()).await);
    assert_cancelled(registry.start(retained).await);
}
```

Use paused Tokio time for 30-second page deadlines/final replay expiry and five-minute guard expiry. Add `idle_expiry_releases_the_job_but_retains_high_water`: complete a final page, replay it at second 29, advance 30 seconds from that last valid access, call `sweep_expired(Instant::now())`, and require `job_expired` on the same identity. Prove lower/equal starts remain rejected until the five-minute guard retention elapses, sweep again, then require that exact identity can be admitted as new work only after its guard is gone. A running page does not refresh idle activity merely because its task is still polling; its separate 30-second page deadline terminates it.

Define all test support in `reference_search::registry::tests`: `UUID_A` is a canonical v4 string; a local `RequestTestExt` trait on `StartReferenceSearchRequest` implements `cancel_request`/`next_request`; `CountingFactory` returns a cursor over a configured candidate count and exposes an atomic advance count; `test_registry`, `registry_with_cursor`, `start_request`, `unique_start`, `seed_registered`, `seed_cancel_guards`, and `blocked_scan_fixture` construct those exact types. `BlockedScanFixture` gives every cursor an arrival/release pair, `spawn_one_more` returns a live `JoinHandle<Result<ReferenceSearchPage, AppCommandError>>`, and all identities are distinct, so the tests measure tasks that entered `next_page`, not merely semaphore constants. Its fairness branch proves twelve registered file scans yield only four entered file cursors while a conversation cursor can still enter before any file release; this fails if source waiters hoard global permits. `assert_overloaded` and `assert_cancelled` are generic over the result's success type and match `AppErrorCode` rather than parsing messages. Test-only permit/count accessors stay under `#[cfg(test)]`.

- [ ] **Step 2: Run registry tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils pre_cancel_tombstone_beats_a_reordered_start
cargo test --features test-utils duplicate_start_and_latest_page_share_or_replay_work
cargo test --features test-utils registered_and_scan_caps_are_enforced_exactly
cargo test --features test-utils guard_table_cap_rejects_new_identity_without_evicting_live_high_water
cargo test --features test-utils idle_expiry_releases_the_job_but_retains_high_water
```

Expected: FAIL because there is no registry.

- [ ] **Step 3: Implement one locked ordering boundary and registry-owned page tasks**

Keep one guard record per `(search_session_id, source)`:

```rust
struct GuardRecord {
    source_sequence: u64,
    request_id: Uuid,
    fingerprint: Option<RequestFingerprint>,
    limit_epoch: u64,
    terminal: Option<AppCommandError>,
    pre_cancel_until: Option<Instant>,
    retain_until: Instant,
}
```

`fingerprint` is `None` for an unknown cancel tombstone because no query/workspace arguments exist yet. A start can compute its immutable-argument fingerprint without compiling the query; it fills/compares the record while advancing high water, then performs pattern/source validation. Under the same registry mutex: sweep expired records; validate identity and advance high water before pattern/source validation; consume matching tombstones; compare equal-sequence fingerprints; replace/cancel lower entries; enforce 256 guards, 64 total jobs, and 24/32/8 source caps; snapshot authoritative limit/epoch; then register. Invalid-pattern/request outcomes are stored as the complete cloneable `AppCommandError` on the guard but consume no registered-job slot. Storing only `AppErrorCode` is insufficient because an exact retry must replay the original message/detail as well as its code.

Each `JobEntry` stores identity/fingerprint, cancellation token, `Option<Box<dyn ReferenceSourceCursor>>`, page zero, latest page, next expected index, shared `Notify`, in-flight page index, terminal result, and last activity. The first registry-owned page task calls the injected factory and keeps that trait-object cursor for later pulls. Handlers only await the shared result; dropping a handler waiter never drops the task. This makes Task 3 compile and pass entirely against `CountingFactory`; Tasks 4-5 add production cursors, and Task 6 constructs the runtime registry only after all sources exist.

Acquire the per-source semaphore first and the global semaphore second inside one 30-second `timeout_at` that also covers cursor scanning, and select the job cancellation token while waiting for either permit. Never hold a global permit while queued for a source permit: otherwise twelve file waiters could occupy the global cap while only four scan and starve conversation/commit despite their free source capacity. If global acquisition is cancelled/times out, drop the already-acquired source permit immediately. Publish only when full identity and limit epoch still match. Cache page zero plus only the newest page; permit page-zero replay through `next_page`, same-index replay, and exactly-next advancement. Other indices return `stale_page`. A valid start join/replay or page request refreshes `JobEntry.last_activity` when admitted under the registry lock; background scan progress does not. When a published page has `done = true`, take and asynchronously `close()` its cursor outside the mutex immediately, but retain the immutable terminal pages for 30 seconds after their last valid access. Sweeping any idle final or non-final entry stores a cloneable `JobExpired` terminal result in its five-minute guard record before removing the job, so an exact late retry cannot silently restart the same sequence. A timeout or source failure similarly copies the complete cloneable terminal error into the five-minute guard record, cancels the job token, removes the `JobEntry` immediately so it releases the global/source registered caps, wakes all shared waiters to read that guard result, and takes/closes the cursor outside the lock. Exact equal-sequence retries replay the guard error without consuming a slot; a higher sequence may start new work. Cancel/replacement/expiry/limit changes likewise remove an entry under the mutex, then take and asynchronously `close()` its cursor outside the mutex; never await process cleanup while holding the registry lock. `kill_on_drop` remains the last-resort guard if shutdown aborts that cleanup future.

`set_limit` clamps, updates the live value, increments an internal epoch for every persisted settings write, and cancels/removes all old-epoch jobs while holding the registry mutex. Before waking their waiters, store the complete typed `LimitEpochChanged` terminal result in each removed identity's five-minute guard record; this includes a start registered just before the settings write but still waiting for its first scan permit. Runtime initialization from persisted settings belongs to Task 6, after the production source factory exists.

- [ ] **Step 4: Verify protocol races, expiry, caps, and both runtimes**

Run:

```powershell
cd src-tauri
cargo test --features test-utils reference_search::registry::tests
cargo test --features test-utils limit_epoch
cargo check
cargo check --no-default-features --bin codeg-server
```

Expected: all commands exit 0; start/cancel/page retries are idempotent, stale operations cannot disturb replacements, and limit changes cancel registration races.

- [ ] **Step 5: Commit the registry**

```powershell
git add src-tauri/src/reference_search/registry.rs src-tauri/src/reference_search/sources/mod.rs src-tauri/src/reference_search/mod.rs src-tauri/src/reference_search/types.rs
git commit -m "feat(references): add guarded incremental search registry"
```

---

### Task 4: Add Stable File and Cross-Project Conversation Cursors

**Files:**
- Create: `src-tauri/src/reference_search/sources/file.rs`
- Create: `src-tauri/src/reference_search/sources/conversation.rs`
- Modify: `src-tauri/src/reference_search/sources/mod.rs`
- Modify: `src-tauri/src/commands/folders.rs`

**Interfaces:**
- Produces: `async fn FileCursor::open(db: &DatabaseConnection, workspace_path: &str, pattern: SearchPattern, limit: usize) -> Result<Self, AppCommandError>`
- Produces: `fn ConversationCursor::open(conn: DatabaseConnection, pattern: SearchPattern, limit: usize) -> Self`
- Produces in `sources::file`: `pub(crate) async fn resolve_open_workspace_root(conn: &DatabaseConnection, requested_path: &str) -> Result<PathBuf, AppCommandError>`, returning the canonical root only when it matches one live open-folder row under platform path-equivalence rules
- Implements: `ReferenceSourceCursor` for both cursor types; Task 5 assembles the production factory after `CommitCursor` exists
- Produces: exactly five matches per non-final `SourcePage`, never more than the cursor's snapshotted limit

- [ ] **Step 1: Write failing file and conversation paging tests**

```rust
#[tokio::test]
async fn file_cursor_is_ignore_aware_stable_and_stops_at_limit() {
    let fixture = open_workspace_fixture().await;
    fixture.write(".gitignore", "ignored/\n");
    fixture.write("ignored/no.ts", "x");
    for name in ["b.ts", "a.ts", "dir/c.ts", "dir/d.ts", "dir/e.ts", "z.ts"] {
        fixture.write(name, "x");
    }
    let mut cursor = FileCursor::open(&fixture.db.conn, fixture.path(), literal(".ts"), 6)
        .await
        .expect("cursor");
    let first = cursor.next_page(5, CancellationToken::new()).await.unwrap();
    let second = cursor.next_page(5, CancellationToken::new()).await.unwrap();
    assert_eq!(first.items.len(), 5);
    assert_eq!(second.items.len(), 1);
    assert!(matches!(
        &first.items[0].metadata,
        ReferenceCandidateMetadata::File { relative_path, .. } if relative_path == "a.ts"
    ));
    assert!(first.items.iter().chain(&second.items).all(|item| !item.uri.contains("ignored")));
}

#[tokio::test]
async fn conversation_cursor_spans_projects_excludes_non_roots_and_deduplicates_moves() {
    let fixture = conversation_search_fixture().await;
    fixture.seed_regular_chat_delegate_loop_and_deleted().await;
    let mut cursor = ConversationCursor::open(fixture.db.conn.clone(), literal("match"), 12);
    let first = cursor.next_page(5, CancellationToken::new()).await.unwrap();
    fixture.move_below_cursor(first.items[0].id.parse().unwrap()).await;
    let rest = drain_cursor(&mut cursor).await;
    let ids: HashSet<_> = first
        .items
        .iter()
        .chain(rest.iter())
        .map(|item| item.id.clone())
        .collect();
    assert_eq!(ids.len(), first.items.len() + rest.len());
    assert!(first.items.iter().chain(rest.iter()).all(|item| matches!(
        &item.metadata,
        ReferenceCandidateMetadata::Conversation { .. }
    )));
    assert!(first.items.iter().chain(rest.iter()).any(|item| matches!(
        &item.metadata,
        ReferenceCandidateMetadata::Conversation { project_name, .. }
            if project_name == "Project B"
    )));
}
```

Define `OpenWorkspaceFixture` plus `async fn open_workspace_fixture() -> OpenWorkspaceFixture` in `sources::file::tests` with a `TempDir`, migrated DB, and a live folder row for that exact temp path; `write` creates parent directories before writing. Define `ConversationSearchFixture` plus `async fn conversation_search_fixture() -> ConversationSearchFixture` in `sources::conversation::tests` with two live folders and rows covering regular/chat/delegate/loop/deleted cases, plus `move_below_cursor`, which sets the already-returned row's `updated_at` strictly older than the first page's captured keyset boundary. This deliberately makes the row eligible to appear again on a later SQL page, so the seen-ID assertion fails if deduplication is absent; merely bumping it newer would leave it before the keyset and would not exercise the stated race. Under `#[cfg(test)]` in `sources/mod.rs`, define `pub(crate) fn literal(query: &str) -> SearchPattern` using `SearchPattern::parse(query).expect("literal pattern")` and this exact shared drain helper:

```rust
pub(crate) async fn drain_cursor(
    cursor: &mut dyn ReferenceSourceCursor,
) -> Vec<ReferenceCandidate> {
    let mut items = Vec::new();
    loop {
        let page = cursor
            .next_page(5, CancellationToken::new())
            .await
            .expect("source page");
        items.extend(page.items);
        assert!(items.len() <= 500, "test cursor exceeded the protocol cap");
        if page.done {
            return items;
        }
    }
}
```

File, conversation, commit, and validation tests import these helpers explicitly; no test relies on a private helper from another module.

- [ ] **Step 2: Run source tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils file_cursor_is_ignore_aware_stable_and_stops_at_limit
cargo test --features test-utils conversation_cursor_spans_projects_excludes_non_roots_and_deduplicates_moves
```

Expected: FAIL because source cursors do not exist.

- [ ] **Step 3: Implement pull-driven source cursors**

Expose `workspace_walk_builder` as `pub(crate)` and reuse it with ignores enabled. Add `sources::file::resolve_open_workspace_root`: reject an empty/non-absolute request, canonicalize it, call the existing `folder_service::list_open_folders`, canonicalize each stored folder path, and compare with the repository's Windows case/separator/verbatim-prefix rules or exact canonical equality on Unix. Skip and debug-log a stored open-folder row whose path has gone stale instead of letting one unrelated missing folder break every search. A missing/non-directory/unopened requested path is `InvalidRequest`; database errors stay database errors, while other requested-path canonicalization failures use the existing typed I/O conversion. Before constructing the file walker, call this helper and use only its returned canonical root; reject symlink escapes. Task 5 commit open/validation and file validation call the same helper rather than reimplementing membership. Configure `WalkBuilder::sort_by_file_path`, keep `ignore::Walk` between page requests, omit the workspace root itself, return matching files and directories, skip `.git`, `__pycache__`, `.DS_Store`, `.gitignore`, `.ignore`, and `.rgignore`, and assign an increasing `source_ordinal` to every visited eligible entry before matching. Check cancellation between entries.

The ignore walker performs blocking filesystem calls. Store it as `Option<ignore::Walk>`, take it at the start of `next_page`, and move it plus the cursor counters into `tokio::task::spawn_blocking`; the closure returns the walker and updated counters with the `SourcePage`, and `next_page` restores them before returning. Check the supplied `CancellationToken` on every entry so cancellation/timeout bounds the blocking task. Do not iterate `ignore::Walk` directly on a Tokio worker thread.

Every file candidate carries the canonical workspace root, slash-normalized relative path, and kind in `ReferenceCandidateMetadata::File`. Canonicalize each candidate target before publication and require it to remain under the canonical root; skip broken links and escaping symlink targets with a debug log, even though directory symlinks are not followed. Use that resolved target only for containment: build the URI from `canonical_root.join(relative_path)` so an allowed in-root symlink keeps the path the user selected. Keep the canonical root `PathBuf` for containment checks, but convert its wire/display string through Task 2's verbatim-prefix normalization before storing metadata or building the URI; otherwise Windows canonicalization would misclassify every drive path as UNC. Its backend-built `uri` uses Task 2's `build_file_uri` contract, including an encoded drive colon such as `file:///C%3A/...` on Windows. Populate the remaining fields exactly: `id = relative_path`, `label = final path component`, `detail = Some(relative_path.clone())`, and `keywords = relative_path.clone()`. Build that complete candidate first, pass it to Task 2's `match_reference_candidate`, and copy only that result's optional regex rank; file matching therefore uses name as primary and relative path as secondary. The frontend never rebuilds an authoritative URI. After the first authoritative candidate, it may remember an alias from the requested workspace string to the candidate's canonical root for later cache lookup; an empty first page creates no alias and therefore no unsafe provisional bucket.

Conversation queries use batches and this keyset:

```rust
query
    .filter(conversation::Column::DeletedAt.is_null())
    .filter(conversation::Column::ParentId.is_null())
    .filter(conversation::Column::Kind.is_in([ConversationKind::Regular, ConversationKind::Chat]))
    .filter(folder::Column::DeletedAt.is_null())
    .order_by_desc(conversation::Column::UpdatedAt)
    .order_by_desc(conversation::Column::Id)
```

Join folder name/path, continue after `(updated_at, id)`, scan until five matches or exhaustion/limit, retain a seen-ID set bounded by the maximum limit of 500 so sort-position changes cannot duplicate an item, and increment `source_ordinal` for every eligible row before matching. Build every conversation URI with Task 2's `build_session_uri`. Decode the row's plain snake-case `agent_type` by wrapping it in `serde_json::Value::String` before deserializing `AgentType`; an unknown value is logged and skipped rather than mislabeled as another agent. Serialize the typed `ConversationStatus` through serde and extract its inner snake-case string (`"in_progress"`, `"pending_review"`, `"completed"`, or `"cancelled"`), never `Debug`/`Display`. Put this row-to-candidate builder in `sources::conversation` with `pub(crate)` visibility so Task 5 validation rebuilds the identical fields. Fold reference links in the title and use `#<id>` only when the folded title is empty. Set `id = conversation_id.to_string()`, `label` to that folded/fallback title, `detail = Some(branch.clone().unwrap_or_else(|| status.clone()))`, and `keywords = format!("{label} {agent_wire}")`; joined project fields live in metadata and participate through the authoritative matcher rather than duplicated keyword parsing. Build the complete metadata before calling `match_reference_candidate`, so title is primary and ID/agent/status/`branch.unwrap_or("")`/project name/project path remain secondary in the declared order.

- [ ] **Step 4: Verify source semantics and cancellation**

Run:

```powershell
cd src-tauri
cargo test --features test-utils reference_search::sources::file::tests
cargo test --features test-utils reference_search::sources::conversation::tests
cargo test --features test-utils search_workspace_files
```

Expected: all commands exit 0; existing workspace search remains intact, reference cursors page five at a time, and cancellation stops scanning without publishing partial stale pages.

- [ ] **Step 5: Commit file and conversation cursors**

```powershell
git add src-tauri/src/reference_search/sources/file.rs src-tauri/src/reference_search/sources/conversation.rs src-tauri/src/reference_search/sources/mod.rs src-tauri/src/commands/folders.rs
git commit -m "feat(references): add file and conversation cursors"
```

---

### Task 5: Stream Commit Pages and Validate Selected Candidates

**Files:**
- Create: `src-tauri/src/reference_search/sources/commit.rs`
- Create: `src-tauri/src/reference_search/validation.rs`
- Modify: `src-tauri/src/reference_search/sources/mod.rs`
- Modify: `src-tauri/src/reference_search/registry.rs`
- Modify: `src-tauri/src/reference_search/types.rs`
- Modify: `src-tauri/src/commands/folders.rs`

**Interfaces:**
- Produces in `commands::folders`: `CommitSourceEpoch { canonical_repo: String, branch: Option<String>, detached: bool, head: String }` whose `head` is the full object ID or the reserved literal `"unborn"`, and whose `opaque()` value is `v1:<sha256>` over a versioned length-delimited encoding of those fields; both Git-head reporting and commit search consume this one type
- Produces: `MAX_REFERENCE_COMMIT_RECORD_BYTES = 64 * 1024` and a bounded NUL-field reader that drains, skips, and re-synchronizes an oversized six-field record without retaining it
- Changes: `pub(crate) async fn resolve_git_head(path: &str) -> Result<GitHeadInfo, AppCommandError>` and produces `fn CommitSourceEpoch::opaque(&self) -> String`
- Produces: `async fn CommitCursor::open(db: &DatabaseConnection, workspace_path: &str, pattern: SearchPattern, limit: usize) -> Result<Self, AppCommandError>`
- Produces: `ProductionReferenceSourceFactory { db: DatabaseConnection }` implementing the Task 3 factory for file, conversation, and commit sources
- Produces: `async fn validate_reference_candidate_core(db: &AppDatabase, request: ValidateReferenceCandidateRequest) -> Result<ReferenceCandidateValidation, AppCommandError>`
- Produces privately in `validation.rs`: `async fn validate_file_candidate(db: &AppDatabase, request: &ValidateReferenceCandidateRequest) -> Result<Option<ReferenceCandidate>, AppCommandError>`, plus identically typed `validate_conversation_candidate` and `validate_commit_candidate`
- Changes: `GitHeadInfo` adds `canonical_repo: Option<String>`, `head_sha: Option<String>`, and `reference_source_epoch: Option<String>`; canonical repo/epoch are present for committed and unborn repositories, while `head_sha` is present only when a commit exists

- [ ] **Step 1: Write failing commit epoch, cancellation, and validation outcome tests**

```rust
#[tokio::test]
async fn commit_cursor_streams_current_history_without_diff_payloads_and_kills_on_cancel() {
    let fixture = git_history_fixture(12).await;
    let mut cursor = CommitCursor::open(&fixture.db.conn, fixture.path(), literal("commit"), 12)
        .await
        .expect("cursor");
    let page = cursor.next_page(5, CancellationToken::new()).await.unwrap();
    assert_eq!(page.items.len(), 5);
    assert!(page.items.iter().all(|item| matches!(
        &item.metadata,
        ReferenceCandidateMetadata::Commit { .. }
    )));
    assert!(!cursor.spawned_args_for_test().iter().any(|arg| {
        matches!(arg.as_str(), "--raw" | "--numstat" | "--name-only" | "--stat")
    }));
    let token = CancellationToken::new();
    token.cancel();
    assert_cancelled(cursor.next_page(5, token).await);
    assert!(!cursor.has_live_child_for_test());
}

#[tokio::test]
async fn validation_distinguishes_match_not_match_not_found_and_epoch_change() {
    let fixture = validation_fixture().await;
    assert!(matches!(
        fixture.validate_file("src/app.ts", "app").await.unwrap(),
        ReferenceCandidateValidation::Match { .. }
    ));
    assert!(matches!(
        fixture.validate_file("src/app.ts", "readme").await.unwrap(),
        ReferenceCandidateValidation::NotMatch { .. }
    ));
    assert!(matches!(
        fixture.validate_file("missing.ts", "missing").await.unwrap(),
        ReferenceCandidateValidation::NotFound { .. }
    ));
    fixture.commit("new head").await;
    let error = fixture.validate_old_commit_epoch().await.expect_err("epoch");
    assert!(matches!(error.code, AppErrorCode::SourceEpochChanged));
}
```

Define `GitHistoryFixture` plus `async fn git_history_fixture(commit_count: usize) -> GitHistoryFixture` in `sources::commit::tests`: initialize a temp repository, set deterministic author config, and create the requested number of empty commits. There is no process-command injection facility in this repository, so do not invent a fixture wrapper around `crate::process::tokio_command`. Instead, make the production cursor build its command from one pure `commit_log_args(captured_head: &str) -> Vec<String>` helper and retain its real `tokio::process::Child`; expose `spawned_args_for_test()` and `has_live_child_for_test()` only under `#[cfg(test)]`. Assert the observed final argument equals the full HEAD captured in the cursor's epoch, then move the repository branch before requesting another page and prove the cursor continues the captured history. The cancellation path must call `start_kill`, await the child, and set the cursor child to `None` before returning `Cancelled`, which makes the last assertion deterministic and cross-platform. Define a module-local `assert_cancelled(Result<SourcePage, AppCommandError>)` that matches `AppErrorCode::Cancelled`; the similarly named Task 3 helper is private to another test module. Define `ValidationFixture` plus `async fn validation_fixture() -> ValidationFixture` in `validation::tests` with one open temp workspace, `src/app.ts`, one root conversation, and one commit; `validate_file(relative, query)` builds the canonical URI with the same URI helper and a fresh UUIDv4 validation ID. No test uses the literal `/repo` path.

Add `unborn_repository_returns_a_stable_empty_epoch_page_without_spawning_git_log`: initialize/open a repository with no commits, require `GitHeadInfo { is_repo: true, head_sha: None, canonical_repo: Some(_), reference_source_epoch: Some(_) }`, open the commit cursor, and assert page zero is empty/exhausted, carries that exact epoch, and has no child process. Create the first commit, resolve HEAD again, and require both `head_sha` and epoch to change.

Add `oversized_commit_metadata_is_drained_and_the_next_record_stays_aligned`: create a newest commit whose body makes the six decoded fields exceed 64 KiB followed by a normal older matching commit. Require the oversized commit to be skipped with a debug/warn diagnostic, the next commit's hash/subject/message fields to remain exact, and cursor memory instrumentation under `#[cfg(test)]` never to retain more than `MAX_REFERENCE_COMMIT_RECORD_BYTES` plus one input-buffer chunk.

- [ ] **Step 2: Run commit/validation tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils commit_cursor_streams_current_history_without_diff_payloads_and_kills_on_cancel
cargo test --features test-utils unborn_repository_returns_a_stable_empty_epoch_page_without_spawning_git_log
cargo test --features test-utils oversized_commit_metadata_is_drained_and_the_next_record_stays_aligned
cargo test --features test-utils validation_distinguishes_match_not_match_not_found_and_epoch_change
```

Expected: FAIL because commit cursor, full HEAD epoch, and validation do not exist.

- [ ] **Step 3: Implement lightweight Git streaming and source-specific validation**

Make `resolve_git_head` `pub(crate)`, run `git rev-parse HEAD` for branch and detached states, and populate full `head_sha`; preserve `head_sha = None` for unborn/non-repo. `CommitCursor::open` first calls Task 4's `sources::file::resolve_open_workspace_root(db, workspace_path)` and passes that canonical open root to Git; the `db` parameter is not decorative. Resolve and canonicalize the repository root with `git rev-parse --show-toplevel`, require the canonical requested workspace to be inside it, strip any Windows verbatim prefix only when producing its stable wire string, and return that root as `GitHeadInfo.canonical_repo`. For an unborn repository use its exact symbolic branch, `detached = false`, and reserved `head = "unborn"` to build `CommitSourceEpoch`; actual Git object IDs are ASCII hex, so the marker cannot collide. Put `epoch.opaque()` in `GitHeadInfo.reference_source_epoch` for both committed and unborn repositories and in every page opened against that same identity. A non-repository has no canonical repo/epoch. `sources::commit` calls this resolver and does not call back into a second epoch implementation.

Use this exact collision-free hash framing:

```rust
fn put_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

let mut hasher = Sha256::new();
hasher.update(b"codeg-reference-commit-v1");
put_field(&mut hasher, self.canonical_repo.as_bytes());
match &self.branch {
    None => hasher.update([0]),
    Some(branch) => {
        hasher.update([1]);
        put_field(&mut hasher, branch.as_bytes());
    }
}
hasher.update([u8::from(self.detached)]);
put_field(&mut hasher, self.head.as_bytes());
format!("v1:{:x}", hasher.finalize())
```

For an unborn epoch, construct an exhausted cursor that returns one empty page with that epoch and never spawns `git log`. Otherwise spawn only commit metadata, pinned to the exact full HEAD used to construct `CommitSourceEpoch`; never pass the moving symbolic name `HEAD` after capturing the epoch:

```rust
let args = commit_log_args(&epoch.head);
crate::process::tokio_command("git")
    .args(&args)
    .current_dir(&canonical_repo)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
```

`commit_log_args` returns `vec!["log", "-z", "--format=%H%x00%h%x00%an%x00%aI%x00%s%x00%B", captured_head]` as owned strings. Keeping the captured object ID in the subprocess arguments makes every streamed page agree with the epoch even if the checked-out branch advances immediately after the cursor opens.

Read exactly six NUL-terminated fields per record: full hash, short hash, author, authored-at, Git subject (`%s`), and full unwrapped message (`%B`). Do not call unbounded `AsyncBufReadExt::read_until` into a growing `Vec`. Implement a `fill_buf`/`consume` loop that appends only while the cumulative six-field payload is at most `MAX_REFERENCE_COMMIT_RECORD_BYTES`, then drains through each remaining NUL without storing bytes and skips the whole record after the sixth delimiter. `git log -z` supplies that sixth terminator after `%B`, and Git commit objects cannot contain NUL, so author names and multiline full messages cannot collide with this framing. Add a two-commit regression case whose subjects/messages contain blank lines and record-separator-like control text, proving the second hash is not consumed as part of the first message. Increment `source_ordinal` for every fully framed commit, including an oversized skipped record, before matching. Build the complete retained commit candidate with `id = full_hash.clone()`, `label = short_hash.clone()`, `detail = Some(subject.clone())`, `keywords = format!("{short_hash} {full_hash} {subject} {author}")`, the backend-built URI, and all six parsed values in metadata. Call Task 2's `match_reference_candidate` and copy its optional regex rank; do not derive `%s` from `%B` or reconstruct a different field order. Validation uses the same 64 KiB bounded metadata reader around `git show -s`; an oversized live commit returns `Ok(None)` rather than constructing a candidate search could never publish. Scan newest-first until five matches/limit and retain the child between pages. Drain stderr concurrently to EOF while retaining only a bounded diagnostic prefix; stopping the drain when the buffer cap is reached could still block Git on a full pipe. On cancellation, timeout, completion, or limit, drop stdout, call `start_kill` only when `try_wait` reports the child still live, await exit, join the stderr drain, and clear every child/pipe handle before returning. Build every commit URI with Task 2's `build_commit_uri`; do not add a second encoder.

Validation rules:

```rust
pub async fn validate_reference_candidate_core(
    db: &AppDatabase,
    request: ValidateReferenceCandidateRequest,
) -> Result<ReferenceCandidateValidation, AppCommandError> {
    let validation_request_id =
        parse_canonical_uuid_v4(&request.validation_request_id)?.hyphenated().to_string();
    validate_source_scope(request.source, request.workspace_path.as_deref())?;
    validate_source_epoch_scope(request.source, request.source_epoch.as_deref())?;
    let pattern = SearchPattern::parse(&request.query)?;
    let candidate = match request.source {
        ReferenceSearchSource::File => validate_file_candidate(db, &request).await?,
        ReferenceSearchSource::Conversation => {
            validate_conversation_candidate(db, &request).await?
        }
        ReferenceSearchSource::Commit => validate_commit_candidate(db, &request).await?,
    };
    let Some(mut candidate) = candidate else {
        return Ok(ReferenceCandidateValidation::NotFound {
            validation_request_id,
        });
    };
    let field_match = match_reference_candidate(&pattern, &candidate);
    let regex_rank = field_match
        .as_ref()
        .and_then(|matched| matched.regex_rank.clone());
    candidate.regex_rank = regex_rank.clone();
    Ok(if field_match.is_some() {
        ReferenceCandidateValidation::Match {
            validation_request_id,
            candidate,
            regex_rank,
        }
    } else {
        ReferenceCandidateValidation::NotMatch {
            validation_request_id,
            candidate,
            regex_rank,
        }
    })
}
```

Each private source helper returns `Ok(None)` only for an authoritative not-found result and `Ok(Some(rebuilt_candidate))` for a live resource; malformed requests and operational failures remain typed errors. Parse each URI according to its declared source and return `InvalidRequest` for a malformed/mismatched scheme. After `validate_source_scope` succeeds, file and commit validation call Task 4's `resolve_open_workspace_root(&db.conn, request.workspace_path.as_deref().expect("validated workspace scope"))`; this prevents direct validation requests from using an arbitrary readable path. Files split encoded path segments before percent-decoding and reject malformed escapes, NUL, decoded `/` or `\` inside one segment, and `.`/`..` segments; reconstruct the platform path, canonicalize the target, and require it under that canonical open root. A missing target is `Ok(None)`, while an escape or unsupported file authority is `InvalidRequest`. Conversations require a numeric `codeg://session/<id>` URI plus a non-deleted regular/chat root, join the same folder fields, and call Task 4's shared row-to-candidate builder so agent/status/title serialization cannot diverge; return `Ok(None)` for a missing/ineligible or unknown-agent row. Commits resolve Git identity from the same canonical open workspace, recompute and compare the current opaque epoch first, require the URI's decoded repository to equal that epoch's canonical root, and return `Ok(None)` immediately when that matching epoch is unborn. Otherwise require an all-ASCII-hex hash whose length equals the current full HEAD length (40 for SHA-1 repositories and 64 for SHA-256 repositories), and run `git cat-file -e <hash>^{commit}` before reachability. A syntactically valid but missing object returns `Ok(None)`; a spawn/I/O failure is `SourceFailed`. Then require `git merge-base --is-ancestor <hash> <current-head>` and read metadata with `git show -s`; merge-base exit code 1 returns `Ok(None)`, while other nonzero failures after a successful object check are `SourceFailed`. An epoch mismatch returns `SourceEpochChanged` before any existence check. The shared core above alone applies `match_reference_candidate` to choose `Match` versus `NotMatch`, so refreshed search and validation cannot diverge on field order or rank.

Implement the validation union as `Match { validation_request_id, candidate, regex_rank }`, `NotMatch { validation_request_id, candidate, regex_rank }`, and `NotFound { validation_request_id }`, serialized with `#[serde(tag = "status", rename_all = "snake_case", rename_all_fields = "camelCase")]`. Echo the canonical request ID on every successful union arm.

- [ ] **Step 4: Verify Git epochs, reachability, and validation safety**

Run:

```powershell
cd src-tauri
cargo test --features test-utils reference_search::sources::commit::tests
cargo test --features test-utils reference_search::validation::tests
cargo test --features test-utils resolve_git_head
```

Expected: all commands exit 0; detached history has a stable marker, branch/HEAD changes invalidate only the active commit bucket, and file validation cannot escape its canonical root.

- [ ] **Step 5: Commit commit search and validation**

```powershell
git add src-tauri/src/reference_search/sources/commit.rs src-tauri/src/reference_search/validation.rs src-tauri/src/reference_search/sources/mod.rs src-tauri/src/reference_search/registry.rs src-tauri/src/reference_search/types.rs src-tauri/src/commands/folders.rs
git commit -m "feat(references): stream commits and validate candidates"
```

---

### Task 6: Expose Runtime-Parity Commands and the Backend-Owned Limit

**Files:**
- Modify: `src-tauri/src/commands/reference_search.rs`
- Modify: `src-tauri/src/commands/conversation_experience.rs`
- Modify: `src-tauri/src/reference_search/mod.rs`
- Modify: `src-tauri/src/app_state.rs`
- Modify: `src-tauri/src/bin/codeg_server.rs`
- Modify: `src-tauri/src/web/mod.rs`
- Create: `src-tauri/src/web/handlers/reference_search.rs`
- Modify: `src-tauri/src/web/handlers/conversation_experience.rs`
- Modify: `src-tauri/src/web/handlers/mod.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src-tauri/src/lib.rs`
- Modify: `src-tauri/tests/api_integration.rs`
- Modify: `src/lib/types.ts`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/api.test.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/transport/index.ts`
- Create: `src/lib/transport/index.test.ts`
- Modify: `src/lib/transport/types.ts`
- Modify: `src/lib/transport/web-transport.ts`
- Modify: `src/lib/transport/web-transport.test.ts`
- Modify: `src/lib/transport/tauri-transport.ts`
- Modify: `src/lib/transport/remote-desktop-transport.ts`
- Modify: `src/stores/conversation-experience-store.ts`
- Modify: `src/stores/conversation-experience-store.test.ts`

**Interfaces:**
- Produces: `start_reference_search`, `next_reference_search_page`, `cancel_reference_search`, `validate_reference_candidate`, and `match_reference_regex` through both transports
- Produces: `set_reference_search_limit(limit: u16)` through both transports
- Produces: `getActiveBackendCacheKey() -> string`
- Changes: `CallOptions` gains `signal?: AbortSignal`; direct WebTransport fetches honor it without misreporting caller cancellation as timeout
- Produces: flat frontend calls `startReferenceSearch(request, signal?)`, `nextReferenceSearchPage(request, signal?)`, `cancelReferenceSearch(request)`, `validateReferenceCandidate(request, signal?)`, and `matchReferenceRegex(request, signal?)`; no transport wraps the protocol object under a nested `request` property
- Produces: `MAX_REFERENCE_REGEX_HTTP_BODY_BYTES = 64 * 1024 * 1024`; only the Axum `match_reference_regex` route applies this `DefaultBodyLimit`
- Consumes: the automatic-title plan's one shared `ConversationExperienceMutationGate` for the reference-limit wrapper
- Produces: `set_reference_search_limit_core(conn, emitter, registry, mutation_gate, limit) -> Result<ConversationExperienceSettings, AppCommandError>`, holding that gate through registry epoch cancellation and settings-event emission
- Preserves: start/search requests carry no result limit

- [ ] **Step 1: Write failing parity, cap, cancellation, and setting-epoch integration tests**

```rust
#[tokio::test]
async fn direct_http_client_cannot_raise_the_backend_limit() {
    let app = reference_api_fixture(10).await;
    let mut payload = start_payload();
    payload["resultLimit"] = serde_json::json!(500);
    let start = app.post("/api/start_reference_search").json(&payload).await;
    let mut page: ReferenceSearchPage = start.json();
    let mut count = page.items.len();
    while !page.done {
        page = app.post("/api/next_reference_search_page")
            .json(&next_payload(page.page_index + 1))
            .await
            .json();
        count += page.items.len();
    }
    assert_eq!(count, 10);
    assert_eq!(page.done_reason, Some(ReferenceDoneReason::Limit));
}

#[tokio::test]
async fn setting_limit_cancels_old_epoch_and_broadcasts_full_snapshot() {
    let fixture = live_registry_fixture(50).await;
    fixture.start_blocked_job().await;
    let saved = set_reference_search_limit_core(
        &fixture.db.conn,
        &fixture.emitter,
        &fixture.registry,
        &fixture.mutation_gate,
        25,
    )
    .await
    .expect("limit");
    assert_eq!(saved.reference_search_limit, 25);
    assert!(fixture.old_job_cancelled().await);
    assert_eq!(fixture.last_settings_event().revision, saved.revision);
}
```

Define `ReferenceApiFixture` plus `async fn reference_api_fixture(limit: u16) -> ReferenceApiFixture` in `src-tauri/tests/api_integration.rs` using the synchronous `AppState::new_for_test` and an open temp workspace containing more than ten matching files. That constructor installs a production source factory at the default limit; before wrapping the state in `Arc`, the async fixture calls `state.reference_search_registry.set_limit(limit).await` so this test does not require making every unrelated `new_for_test` caller async. `start_payload` returns a concrete `serde_json::Value` with canonical UUIDv4 IDs and source sequence 1; `next_payload` reuses that identity and changes only `pageIndex`. Define `LiveRegistryFixture` plus `async fn live_registry_fixture(limit: u16) -> LiveRegistryFixture` in `commands::conversation_experience::tests` with a blocking fake source factory, one `ConversationExperienceMutationGate`, test `EventEmitter`, and settings-event receiver.

In new `src/lib/transport/index.test.ts`, mock `detectEnvironment` plus the dynamically required transport classes, reset module transport state after each case, and assert `getActiveBackendCacheKey()` returns exactly `"local:tauri"`, `web:${window.location.origin}`, and `remote:42` for the three modes. Extend `web-transport.test.ts` with pre-abort, mid-fetch abort, timeout, and listener-cleanup cases, and extend `conversation-experience-store.test.ts` with a returned-snapshot limit save case; all are RED before Step 3.

In the `src/lib/api.test.ts` created by the automatic-title plan, add `reference_calls_use_flat_protocol_payloads_and_forward_signals`: reuse its hoisted transport mock, call start/next/cancel/validate/regex with one `AbortController.signal`, and assert each `call` receives the protocol fields directly rather than `{ request }`. Require `{ timeoutMs: 35_000, signal }` for start/next, `{ signal }` for validation/regex, no options for guarded cancel, and prove the conversation start object has no own `workspacePath` property.

In `src-tauri/tests/api_integration.rs`, add `regex_helper_http_route_accepts_a_valid_body_above_axum_default`: send 100 descriptors whose searchable field is 4,096 NUL scalars (valid UTF-8 bytes but JSON-escaped above 2 MiB), use a non-matching valid regex, and require HTTP/core success with an empty result. Add a one-byte-over descriptor unit case in Task 2 to prove the raised route limit does not weaken core bounds. Do not disable the body limit globally or on search/page/validation routes.

Add `concurrent_limit_saves_hold_the_gate_through_registry_application`: under `#[cfg(test)]`, add `ReferenceSearchRegistry::pause_next_limit_apply_before_effect()` with one arrival and one release handle. Start the first limit wrapper, wait until its database transaction has committed and `set_limit` reaches that hook, then start the second wrapper and prove with a short `tokio::time::timeout` that it remains pending. Release the first call, await both, and assert the final persisted document, registry limit, limit epoch, and last emitted snapshot all correspond to the higher revision. Compile the hook out of production. The test fails if the shared mutation gate is released before registry epoch cancellation/event emission.

- [ ] **Step 2: Run integration tests and confirm RED**

Run:

```powershell
cd src-tauri
cargo test --features test-utils --test api_integration direct_http_client_cannot_raise_the_backend_limit
cargo test --features test-utils --test api_integration regex_helper_http_route_accepts_a_valid_body_above_axum_default
cargo test --features test-utils setting_limit_cancels_old_epoch_and_broadcasts_full_snapshot
cargo test --features test-utils concurrent_limit_saves_hold_the_gate_through_registry_application
cd ..
pnpm test -- src/lib/api.test.ts src/lib/transport/index.test.ts src/lib/transport/web-transport.test.ts src/stores/conversation-experience-store.test.ts
```

Expected: FAIL because handlers/routes/settings runtime effects are not exposed.

- [ ] **Step 3: Implement shared cores, handlers, commands, and frontend mirrors**

Construct one `ProductionReferenceSourceFactory` and `ReferenceSearchRegistry` in desktop setup, server startup, and `AppState::new_for_test`. Desktop/server initialization loads the persisted limit before construction (desktop uses its existing `tauri::async_runtime::block_on` setup boundary); the intentionally synchronous test constructor uses `DEFAULT_REFERENCE_SEARCH_LIMIT`, and async fixtures that need another value call `set_limit` before sharing the state. Store the registry in managed desktop state/`AppState`, clone that same managed instance into the embedded-server `AppState` literal in `web/mod.rs`, and start one sweeper task tied to runtime shutdown in production only. Each command core delegates directly to that registry or validation/matcher. Axum structs use `#[serde(rename_all = "camelCase")]`; Tauri wrappers take the individual protocol fields so their JavaScript argument object is the same flat shape as the Axum JSON body. Do not introduce a Tauri-only `{ request: ... }` envelope. Register all six commands/routes. Wrap only `post(handlers::reference_search::match_reference_regex)` with `DefaultBodyLimit::max(MAX_REFERENCE_REGEX_HTTP_BODY_BYTES)`; keep Axum's default limit everywhere else. The 64 MiB value covers the worst JSON escaping allowed by Task 2's row/ID/field-slot/searchable-byte bounds while still rejecting an unbounded body before deserialization.

Wrap the persisted limit setter in this order:

```rust
let _mutation_guard = mutation_gate.lock().await;
let saved = set_reference_search_limit_persisted_core(conn, limit).await?;
registry.set_limit(saved.reference_search_limit).await;
emit_event(
    emitter,
    CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
    saved.clone(),
);
Ok(saved)
```

Acquire the same `ConversationExperienceMutationGate` wired by the automatic-title plan before persistence and release it only after `registry.set_limit` plus synchronous event emission. This serializes title and limit field mutations as one revision stream and prevents a delayed older limit request from restoring an obsolete runtime cap after a newer revision.

Expose the frontend functions with these exact flat signatures and transport payloads; start and page use 35 seconds so the backend's 30-second page deadline remains authoritative:

```ts
export async function startReferenceSearch(
  request: StartReferenceSearchRequest,
  signal?: AbortSignal
): Promise<ReferenceSearchPage> {
  return getTransport().call("start_reference_search", { ...request }, {
    timeoutMs: 35_000,
    signal,
  })
}

export async function nextReferenceSearchPage(
  request: NextReferenceSearchPageRequest,
  signal?: AbortSignal
): Promise<ReferenceSearchPage> {
  return getTransport().call("next_reference_search_page", { ...request }, {
    timeoutMs: 35_000,
    signal,
  })
}

export async function cancelReferenceSearch(
  request: CancelReferenceSearchRequest
): Promise<boolean> {
  return getTransport().call("cancel_reference_search", { ...request })
}

export async function validateReferenceCandidate(
  request: ValidateReferenceCandidateRequest,
  signal?: AbortSignal
): Promise<ReferenceCandidateValidation> {
  return getTransport().call("validate_reference_candidate", { ...request }, {
    signal,
  })
}

export async function matchReferenceRegex(
  request: MatchReferenceRegexRequest,
  signal?: AbortSignal
): Promise<ReferenceRegexMatch[]> {
  return getTransport().call("match_reference_regex", { ...request }, { signal })
}
```

Extend `useConversationExperienceStore.setReferenceSearchLimit` to apply the returned snapshot through the same revision gate. Add TypeScript mirrors of every Task 2/5 wire type, preserving backend source `"conversation"` while adapting it to frontend group kind `"session"`. Every new reference request, page, candidate, metadata-variant field, descriptor, and validation property uses the camel-case serde wire name in TypeScript (`sourceOrdinal`, `regexRank`, `relativePath`, `agentType`, `projectName`, `shortHash`, and so forth); snake-case spellings remain Rust-only identifiers. Rust `GitHeadInfo` is an existing snake-case wire type and always serializes the three new `Option<String>` fields as a value or `null`; declare only those mirrors as `canonical_repo?: string | null`, `head_sha?: string | null`, and `reference_source_epoch?: string | null` so a window attached to an older remote backend degrades to no commit preview instead of failing structural fixtures. Search/page/validation API calls accept an `AbortSignal` and pass it as `CallOptions.signal`; page calls use a 35-second transport timeout so the backend's 30-second page deadline wins. Start requests have no limit field, and the backend ignores an unknown direct-client `resultLimit` field as demonstrated by the integration test.

Extend the transport contract exactly as follows:

```ts
export interface CallOptions {
  timeoutMs?: number
  signal?: AbortSignal
}
```

`WebTransport.call` first normalizes an already-aborted caller signal to `new DOMException("The operation was aborted.", "AbortError")`, then creates its existing internal timeout controller, registers a one-shot listener that aborts it when `options.signal` aborts, and removes that listener in `finally`. Track a separate `timedOut` boolean: an internal timer sets it before aborting and throws the existing `Error("Request timed out")`, while a caller-triggered abort throws a DOM `AbortError` so the controller treats it as silent cancellation. Register the listener before calling `fetch` and recheck `signal.aborted` immediately afterward to close the check/listener race. Add focused tests for pre-abort, mid-fetch abort, timeout, and listener cleanup. Tauri invoke and the remote-desktop proxy cannot cancel an already-dispatched IPC command; they check `signal.throwIfAborted()` before dispatch, otherwise document that the controller must send guarded `cancel_reference_search` and reject late results by generation. They must not pretend an IPC request was physically aborted.

Add a stable backend cache key:

```ts
export function getActiveBackendCacheKey(): string {
  const remote = getActiveRemoteConnectionId()
  if (remote != null) return `remote:${remote}`
  if (typeof window !== "undefined" && detectEnvironment() === "web") {
    return `web:${window.location.origin}`
  }
  return "local:tauri"
}
```

- [ ] **Step 4: Verify transport parity and settings convergence**

Run:

```powershell
cd src-tauri
cargo test --features test-utils --test api_integration reference_search
cargo test --features test-utils commands::conversation_experience::tests
cargo check
cargo check --no-default-features --bin codeg-server
cd ..
pnpm test -- src/lib/api.test.ts src/lib/workspace-file-api.test.ts src/lib/transport/index.test.ts src/lib/transport/web-transport.test.ts src/stores/conversation-experience-store.test.ts
```

Expected: all commands exit 0; direct clients cannot supply a higher cap, both transports return identical payloads/codes, and settings revisions cancel old epochs before new starts.

- [ ] **Step 5: Commit the complete backend/API surface**

```powershell
git add src-tauri/src/commands/reference_search.rs src-tauri/src/commands/conversation_experience.rs src-tauri/src/reference_search/mod.rs src-tauri/src/app_state.rs src-tauri/src/bin/codeg_server.rs src-tauri/src/web/mod.rs src-tauri/src/web/handlers/reference_search.rs src-tauri/src/web/handlers/conversation_experience.rs src-tauri/src/web/handlers/mod.rs src-tauri/src/web/router.rs src-tauri/src/lib.rs src-tauri/tests/api_integration.rs src/lib/types.ts src/lib/api.ts src/lib/api.test.ts src/lib/tauri.ts src/lib/transport/index.ts src/lib/transport/index.test.ts src/lib/transport/types.ts src/lib/transport/web-transport.ts src/lib/transport/web-transport.test.ts src/lib/transport/tauri-transport.ts src/lib/transport/remote-desktop-transport.ts src/stores/conversation-experience-store.ts src/stores/conversation-experience-store.test.ts
git commit -m "feat(references): expose incremental search protocol"
```

---

### Task 7: Build the Window-Scoped Candidate and Regex Snapshot Cache

**Files:**
- Create: `src/lib/reference-search-cache.ts`
- Create: `src/lib/reference-search-cache.test.ts`
- Modify: `src/contexts/app-workspace-context.tsx`
- Modify: `src/contexts/app-workspace-context.test.tsx`

**Interfaces:**
- Produces: singleton `referenceSearchCache`
- Produces: `ReferenceSearchCacheOptions { candidateCap?: number, candidateByteCap?: number, regexSnapshotCap?: number }` with defaults `10_000`, `64 * 1024 * 1024`, and `200`; candidate pruning enforces both candidate caps window-wide
- Produces: `ReferenceCacheBucketKey` as `{ backend, source: "file", canonicalRoot } | { backend, source: "conversation" } | { backend, source: "commit", canonicalRepo, sourceEpoch }`
- Produces: `captureMutationRevision() -> number`, `mergeCandidate(bucket, candidate, options?: { conversationPageStartedAt?: number }): CachedCandidate | null`, `literalPreview`, `getRegexSnapshot`, `beginRegexRefresh`, `commitRegexRefresh`, `discardRegexRefresh`, `markConversationUpsert`, `markConversationStatus`, `markConversationDelete`, `markConversationNotFoundIfRevision`, `subscribeConversationChanges`, `evictUri`, `evictIfRevision`, `pinVisible`, `pinSelected`, `releaseController`, `rememberFileRootAlias(backend, requestedRoot, canonicalRoot)`, `resolveFileRootAlias(backend, requestedRoot) -> string | null`, and `reset`; `null` means a conversation tombstone or a newer conversation-event watermark rejected a late candidate
- Produces: `ReferenceConversationCacheChange = { kind: "upsert"; backend: string; uri: string; summary: DbConversationSummary; mutationRevision: number | null } | { kind: "status"; backend: string; uri: string; status: string; mutationRevision: number | null } | { kind: "delete"; backend: string; uri: string }`; provider-forwarded upsert/status/delete and a revision-matched `markConversationNotFoundIfRevision` publish this signal, while page merges, positive validation merges, and generic file/commit evictions stay silent
- Produces: a backend-scoped, 512-entry FIFO conversation-delete tombstone set; candidate/page merges and later upsert/status events cannot resurrect a soft-deleted URI, and `reset` clears the tombstones
- Produces: `LiteralCachePreview { items: CachedCandidate[]; truncated: boolean }` and regex snapshots with the same `truncated` bit; `commitRegexRefresh` accepts the completed source's boolean
- Produces: `candidateSearchFields(candidate: ReferenceCandidate) -> { primary: string[]; secondary: string[] }` and `rankLiteralFields(query: string, primary: readonly string[], secondary: readonly string[]) -> number | null`
- Produces: window-monotonic `mutationRevision` that never reuses a value after eviction/re-addition

- [ ] **Step 1: Write failing cache reuse, pin, mutation, and cap tests**

```ts
it("reuses literal items and exact regex snapshots without duplicating metadata", () => {
  const cache = new ReferenceSearchCache()
  const bucket = fileBucket("local:tauri", "C:/repo")
  cache.mergeCandidate(bucket, candidate("file:///C%3A/repo/src/app.ts", "app.ts"))
  const refresh = cache.beginRegexRefresh("controller", bucket, "re:^src/")
  cache.commitRegexRefresh(refresh, [
    { uri: "file:///C%3A/repo/src/app.ts", rank: regexRank(1, 0, 3) },
  ], false)
  expect(
    cache.literalPreview(bucket, "APP", 50).items.map((item) => item.candidate.uri)
  ).toEqual(["file:///C%3A/repo/src/app.ts"])
  expect(cache.getRegexSnapshot(bucket, "re:^src/")?.items[0].candidate.label).toBe(
    "app.ts"
  )
  expect(cache.debugStats().candidateCount).toBe(1)
})

it("pins selected and visible entries while enforcing global LRUs", () => {
  const cache = new ReferenceSearchCache({ candidateCap: 3, regexSnapshotCap: 2 })
  const bucket = conversationBucket("local:tauri")
  for (const id of ["1", "2", "3"]) cache.mergeCandidate(bucket, sessionCandidate(id))
  cache.pinSelected("controller", bucket, "codeg://session/1")
  cache.pinVisible("controller", bucket, ["codeg://session/2"])
  cache.mergeCandidate(bucket, sessionCandidate("4"))
  expect(cache.has(bucket, "codeg://session/1")).toBe(true)
  expect(cache.has(bucket, "codeg://session/2")).toBe(true)
  expect(cache.has(bucket, "codeg://session/3")).toBe(false)
})

it("evicts on UTF-8 byte pressure and prunes pinned overflow after release", () => {
  const bucket = fileBucket("local:tauri", "C:/repo")
  const small = candidate("file:///C%3A/repo/a.ts", "a.ts")
  const large = candidate(
    "file:///C%3A/repo/large.ts",
    "\u754c".repeat(4096)
  )
  const grown = candidate(small.uri, "\u754c".repeat(4096))
  const probe = new ReferenceSearchCache({
    candidateCap: 10,
    candidateByteCap: Number.MAX_SAFE_INTEGER,
    regexSnapshotCap: 2,
  })
  probe.mergeCandidate(bucket, small)
  const smallBytes = probe.debugStats().candidateBytes
  probe.reset()
  probe.mergeCandidate(bucket, large)
  const largeBytes = probe.debugStats().candidateBytes
  probe.reset()
  probe.mergeCandidate(bucket, grown)
  const grownBytes = probe.debugStats().candidateBytes

  const pressured = new ReferenceSearchCache({
    candidateCap: 10,
    candidateByteCap: largeBytes,
    regexSnapshotCap: 2,
  })
  pressured.mergeCandidate(bucket, small)
  pressured.mergeCandidate(bucket, large)
  expect(pressured.has(bucket, small.uri)).toBe(false)
  expect(pressured.has(bucket, large.uri)).toBe(true)
  expect(pressured.debugStats().candidateBytes).toBe(largeBytes)

  const pinned = new ReferenceSearchCache({
    candidateCap: 10,
    candidateByteCap: smallBytes,
    regexSnapshotCap: 2,
  })
  pinned.mergeCandidate(bucket, small)
  pinned.pinSelected("controller", bucket, small.uri)
  pinned.mergeCandidate(bucket, grown)
  expect(grownBytes).toBeGreaterThan(smallBytes)
  expect(pinned.has(bucket, small.uri)).toBe(true)
  expect(pinned.debugStats().candidateBytes).toBe(grownBytes)
  pinned.releaseController("controller")
  expect(pinned.has(bucket, small.uri)).toBe(false)
  expect(pinned.debugStats().candidateBytes).toBe(0)
})

it("rejects an old not_found after a fresh page mutates the same URI", () => {
  const cache = new ReferenceSearchCache()
  const bucket = conversationBucket("local:tauri")
  const old = cache.mergeCandidate(bucket, sessionCandidate("7"))!
  const fresh = cache.mergeCandidate(bucket, sessionCandidate("7"))!
  expect(fresh.mutationRevision).toBeGreaterThan(old.mutationRevision)
  expect(cache.evictIfRevision(bucket, fresh.candidate.uri, old.mutationRevision)).toBe(false)
})

it("resolves only authoritative file aliases and never rewinds mutation revisions", () => {
  const cache = new ReferenceSearchCache()
  expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBeNull()
  cache.rememberFileRootAlias("local:tauri", "C:/repo-link", "C:/repo")
  expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBe("C:/repo")
  const before = cache.mergeCandidate(
    fileBucket("local:tauri", "C:/repo"),
    candidate("file:///C%3A/repo/a.ts", "a.ts")
  )!
  cache.reset()
  expect(cache.resolveFileRootAlias("local:tauri", "C:/repo-link")).toBeNull()
  cache.rememberFileRootAlias("local:tauri", "C:/repo-link", "C:/repo")
  const after = cache.mergeCandidate(
    fileBucket("local:tauri", "C:/repo"),
    candidate("file:///C%3A/repo/a.ts", "a.ts")
  )!
  expect(after.mutationRevision).toBeGreaterThan(before.mutationRevision)
})
```

Define `CachedCandidate { candidate: ReferenceCandidate, mutationRevision: number, stale: boolean }`; both literal previews and resolved regex snapshot items return this shape inside their `{ items, truncated }` wrapper. Add a case with three literal matches and limit two plus a committed regex refresh marked truncated, and assert both preview kinds preserve `truncated: true`. Define all test builders in `reference-search-cache.test.ts`: `candidate(uri, label)` returns a complete file `ReferenceCandidate`; `sessionCandidate(id, label?)`, `regexRank(fieldTier, start, length)`, and the three bucket builders return the exact Task 2/7 types. `debugStats` and `has` are test-only cache methods. No test helper is imported from the later controller task.

Add `visible_pins_coexist_across_buckets_and_replace_per_bucket`: pin one file and one conversation URI for the same controller, pressure the global candidate LRU, and prove both survive; replace only the file bucket with an empty set and prove the conversation remains pinned while the old file becomes evictable. Then select a commit URI and prove replacing that one selected pin clears only the prior selected pin, not either visible bucket.

In `app-workspace-context.test.tsx`, mock `referenceSearchCache.markConversationUpsert`, `markConversationStatus`, and `markConversationDelete`, then add `forwards_conversation_changes_to_the_reference_cache_once`: deliver one existing upsert, one status, and one delete through the provider's already captured `conversation://changed` handler, assert the original workspace-store behavior still occurs, and assert exactly one matching cache call per event. Do not add a second transport subscription solely for the cache. In `reference-search-cache.test.ts`, add `conversation_upsert_refolds_title_and_preserves_joined_project_metadata`: seed a candidate whose project fields came from the search join, upsert a summary title containing `[README](file:///repo/README.md)`, and require label `README`, refreshed agent/status/branch fields, and unchanged project name/path. Add `conversation_changes_publish_once_after_cache_mutation`: subscribe, apply one cached and one uncached upsert, one cached and one uncached status change, plus one delete; assert callbacks carry the summary/status after any cache mutation, use a non-null revision only for cached rows, and prove unsubscribe suppresses later calls. Add `conversation_delete_tombstone_rejects_late_page_and_upsert`: delete URI 7, attempt to merge an older search-page candidate and forward a stale upsert/status for 7, and assert none recreates the cache entry or publishes a contradictory update; insert 513 distinct tombstones and assert the oldest is FIFO-evicted while the total remains 512. Extend the old-`not_found` revision test: `markConversationNotFoundIfRevision` with the stale revision returns false and creates no tombstone, while the current revision returns true, evicts, publishes delete, and rejects a later page merge.

Add `conversation_event_watermark_rejects_an_older_page_without_adding_unrelated_rows`: capture the cache revision for a page request, apply an uncached upsert/status for URI 9, then try to merge an older page candidate with `conversationPageStartedAt` set to the capture. Require `null`, no candidate insertion, and no contradictory cache-change publication. Capture again after the event and prove a new page can merge. Add `conversation_change_during_regex_refresh_cannot_commit_an_old_rank_as_fresh`: add a working regex ref at candidate revision N, apply an upsert/status that advances it to N+1, commit the refresh, and require that ref to be stored stale (omitted from ordinary preview but resolvable for a selected pending-validation path), never fresh with the old rank.

- [ ] **Step 2: Run cache tests and confirm RED**

Run:

```powershell
pnpm test -- src/lib/reference-search-cache.test.ts src/contexts/app-workspace-context.test.tsx
```

Expected: FAIL because the cache module does not exist.

- [ ] **Step 3: Implement global LRUs, pins, buckets, and conversation events**

Serialize the structured bucket key with a collision-free length-delimited helper; do not join unescaped fields with `|`. Store candidate metadata once in a global `Map<bucket-key, Map<uri, ItemEntry>>` and regex snapshots as ordered `{ uri, rank, stale, mutationRevision }` references only. Each `ItemEntry` stores one `retainedUtf8Bytes` computed on insertion/replacement with a shared `TextEncoder`: sum the UTF-8 byte length of every string-valued leaf actually retained by `ReferenceCandidate`, including the outer source/URI/ID/label/detail/keywords fields and the selected metadata variant's discriminator, enum, and optional/string fields. Count repeated values once per stored protocol field, count no object keys or numeric/rank fields, and never charge regex references for candidate text they do not copy. Subtract the old entry size before installing a replacement and add the new size once, so `candidateBytes` remains exact across authoritative metadata refresh, eviction, and reset. Maintain explicit LRU counters (not accidental object iteration order), global candidate count/byte totals, snapshot counts, pin reference counts by controller, and a single incrementing mutation clock exposed read-only through `captureMutationRevision`. Prune the least-recently-used unpinned candidates while either candidate count exceeds `candidateCap` or retained bytes exceed `candidateByteCap`; if every remaining candidate is pinned, retain the temporary overflow. Every accepted `mergeCandidate` assigns a new mutation revision even when all candidate fields equal the cached value, because a replayed/newer authoritative page must defeat a `not_found` captured before it. Maintain a separate backend/URI conversation-event watermark LRU capped at 10,000 entries; every upsert/status/delete advances the clock and records its revision even when the URI is not in the candidate cache. A conversation page merge carrying `conversationPageStartedAt` returns `null` without changing candidate metadata when that URI's latest event watermark is newer than the page capture. Calls for file/commit or positive validation omit the option. A regex working ref captures the candidate's mutation revision; `commitRegexRefresh` compares it with the current item and marks the installed ref stale (or drops it when gone/tombstoned) if the revision changed while the source was draining. It never promotes an old rank back to fresh after a conversation event.

`pinVisible(controllerId, bucket, uris)` atomically replaces that controller's prior visible set for that exact bucket; callers clear a disappeared bucket with an empty set. `pinSelected(controllerId, bucket, uri: string | null)` atomically replaces or clears the controller's one cross-bucket selected pin. Neither call accumulates forgotten query pins, and each reruns candidate pruning only after its replacement is installed, so releasing an oversized pinned entry restores both count and byte caps immediately. Starting/committing/discarding a controller-owned regex refresh likewise owns an active-expression pin for that exact controller/bucket token, allowing file, conversation, and commit refreshes to coexist. `releaseController(controllerId)` is idempotent, is used only for actual close/disposal, releases visible/selected/active-expression pins across every bucket, and immediately reruns both pruners. `reset` clears candidates, snapshots, pins, aliases, tombstones, event watermarks, and byte/count totals but does not reset the mutation clock, so a stale validation capture can never collide with an eviction/re-addition even across a backend reset in the same window. Keep a separate backend-keyed FIFO/set pair for at most 512 soft-deleted conversation URIs, mirroring the existing app-workspace store's bounded deletion tombstones; `mergeCandidate` rejects a conversation candidate whose URI remains tombstoned. The file alias map is scoped by backend and maps the exact requested workspace string only after an authoritative file candidate supplies its canonical root. `resolveFileRootAlias` performs no path normalization or filesystem guess in JavaScript; it returns only that stored canonical root. Repointing an alias to a newly observed canonical root removes the old alias relation but does not evict the old canonical bucket, which may still be used by another open workspace/controller.

Literal preview applies the declared source field matcher synchronously, ranks the accumulated set, sets `truncated` by comparing the full match count with the requested display limit, and then truncates. `candidateSearchFields(candidate: ReferenceCandidate)` must mirror Task 2 through the TypeScript wire names exactly: file `[candidate.label]` then `[candidate.metadata.relativePath]`; conversation `[candidate.label]` then `[candidate.id, candidate.metadata.agentType, candidate.metadata.status, candidate.metadata.branch ?? "", candidate.metadata.projectName, candidate.metadata.projectPath]`; commit `[candidate.metadata.shortHash, candidate.metadata.fullHash, candidate.metadata.subject]` then `[candidate.metadata.message, candidate.metadata.author]`. `agentType` already contains the snake-case serialized enum value; do not rename the property itself to `agent_type`. Keep the empty branch slot so later regex field tiers do not shift. `rankLiteralFields` reproduces literal tiers 0-4 and declared-order tie-breaking over Unicode scalar iteration; resource cache previews and Task 8's local catalog matching reuse it rather than matching flattened `keywords`. Regex preview only returns the complete exact-expression snapshot and its source-completion `truncated` bit; `beginRegexRefresh` creates a controller/source-owned working set, `commitRegexRefresh` swaps it atomically only after authoritative source completion, and cancellation/failure calls `discardRegexRefresh` without touching the last complete snapshot.

On conversation upsert, first reject a tombstoned URI, then update only an already-indexed `codeg://session/<id>` candidate. Recompute its label with the existing `formatConversationTitle(summary.title).trim()` and the same `#<id>` empty-title fallback, so event-refreshed metadata cannot reintroduce raw reference Markdown that the Rust candidate builder folded. Refresh agent/status/branch plus matching detail/keywords, but preserve the authoritative URI/source ordinal and joined metadata `projectName`/`projectPath` because `DbConversationSummary` does not carry those project fields. `markConversationStatus` takes the numeric ID plus serialized status, rejects a tombstoned URI, and when cached updates `metadata.status` plus `detail` only when there is no branch; it never adds an uncached candidate. For either cached upsert/status, advance mutation revision and mark all regex references to that URI stale. Whether cached or not, publish one corresponding change after this lookup, with `mutationRevision: null` for an uncached URI. These signals let an active controller update a current-generation row that global LRU pressure removed without adding an unrelated row back to cache. A selected stale URI remains available as pending validation while consumers omit unselected stale references. `markConversationDelete` records/moves the URI at the tail of the backend's tombstone FIFO, authoritatively removes the URI, all snapshot references, and any dangling pin memberships for that URI, then publishes one `delete` change even when the item was already absent so an open controller can discard its own current-generation copy. A duplicate delete refreshes FIFO recency without growing the set. `markConversationNotFoundIfRevision` performs that same tombstone/evict/publish sequence only when the cached item's current mutation revision still equals the validation capture; otherwise it returns false without changing tombstones. Keep generic `evictUri`/`evictIfRevision` silent for file/commit validation paths, whose not-found outcome may be followed by a legitimately recreated resource. `subscribeConversationChanges` returns an idempotent unsubscribe and listener failures cannot prevent later listeners. `AppWorkspaceProvider` forwards all three existing `conversation://changed` variants with `getActiveBackendCacheKey()` into the upsert/status/delete methods, so a remote event cannot mutate another backend's conversation bucket.

When pins prevent pruning below either the 10,000-candidate/64 MiB candidate caps or the 200-expression cap, leave temporary overflow and call the corresponding pruner again on pin replacement, query change, or controller close. Remove empty bucket shells and aliases that point to removed file buckets. Register `() => referenceSearchCache.reset()` with the existing backend-scoped reset registry from the cache module itself; no change to the registry implementation is required and the wrapper avoids losing the cache method's receiver.

- [ ] **Step 4: Verify cache semantics and existing reset registry**

Run:

```powershell
pnpm test -- src/lib/reference-search-cache.test.ts src/stores/backend-scoped-store-reset.test.ts src/contexts/app-workspace-context.test.tsx
```

Expected: all commands exit 0; no metadata is copied into regex snapshots, only `not_found` globally evicts, count/byte pressure respects pins and converges after release, and reset clears every bucket and retained-byte total.

- [ ] **Step 5: Commit the window cache**

```powershell
git add src/lib/reference-search-cache.ts src/lib/reference-search-cache.test.ts src/contexts/app-workspace-context.tsx src/contexts/app-workspace-context.test.tsx
git commit -m "feat(references): add window-scoped search cache"
```

---

### Task 8: Replace the Aggregate Promise with an Independent Source Controller

**Files:**
- Create: `src/components/chat/composer/reference-search-controller.ts`
- Create: `src/components/chat/composer/reference-search-controller.test.ts`
- Modify: `src/components/chat/composer/use-reference-search.ts`
- Modify: `src/components/chat/composer/use-reference-search.test.ts`
- Modify: `src/components/chat/composer/suggestion/types.ts`
- Modify: `src/components/chat/composer/suggestion/adapters.ts`
- Modify: `src/components/chat/composer/suggestion/adapters.test.ts`
- Modify: `src/components/chat/composer/suggestion/suggestion-popup.test.tsx`
- Modify: `src/components/chat/composer/rich-composer-mention.test.tsx`
- Modify: `src/stores/app-workspace-store.ts`
- Modify: `src/stores/app-workspace-store.test.ts`

**Interfaces:**
- Produces: `ReferenceSearchController.setQuery(query: string): void`
- Produces: `ReferenceSearchControllerInputs { agents: AcpAgentInfo[]; profileCatalog: DelegationProfileCatalog | null; profileCatalogError: boolean; referenceLimit: number; gitHead: GitHeadInfo | null; labels: ReferenceGroupLabels }`
- Produces: `updateInputs(inputs: ReferenceSearchControllerInputs): void`, `subscribe(listener: () => void): () => void`, `getSnapshot(): ReferenceSearchSnapshot`, `setSelectedUri(uri: string | null): void`, `confirmCandidate(uri: string): Promise<ReferenceAttrs | null>`, and `close(): void`
- Consumes: `referenceSearchCache.subscribeConversationChanges` only while active; `setQuery` reacquires it after a prior close and `close` always unsubscribes before releasing cache pins
- Produces: one canonical UUIDv4 `searchSessionId` generated once per controller, one local query generation, one independently incremented catalog-regex run generation, plus independent positive `sourceSequence` counters for file/conversation/commit; every source start/restart generates a fresh canonical UUIDv4 `requestId`
- Guarantees: `close()` is idempotent, and reopening through `setQuery` reacquires exactly one cache subscription without reusing any live request identity
- Changes: `SuggestionItem.reference` is `ReferenceAttrs & { uri: string }`; every mention candidate has a canonical non-null URI, while the broader inserted-node `ReferenceAttrs.uri` type remains nullable for non-mention invocation kinds
- Produces in `suggestion/adapters.ts`: `CatalogSearchEntry = { kind: "agent"; agent: AcpAgentInfo } | { kind: "profile"; profile: DelegationProfile; backingAgent: AcpAgentInfo }` and `catalogSearchFields(entry: CatalogSearchEntry) -> { primary: string[]; secondary: string[] }`
- Consumes: `GitHeadInfo { is_repo, canonical_repo, head_sha, reference_source_epoch, branch, detached }` plus injected `fetchGitHead(): Promise<GitHeadInfo>` and `applyGitHead(head: GitHeadInfo): void`; commit cache lookup never derives an epoch or repository root in JavaScript, and only a guard-current fetch result is applied
- Produces: `useReferenceSearchController({ folderId, defaultPath, enabled, labels }) -> ReferenceSearchController | null`; the option keys are required but accept `number | null` and `string | null`, and commit identity/refresh requires both values to be non-null
- Preserves temporarily: the existing aggregate `useReferenceSearch` export and `ReferenceSearch` type only through Task 8 so untouched composer callers remain type-correct; Task 9 deletes both after migration

- [ ] **Step 1: Write failing bare, independent page, stale, recovery, regex, and validation tests**

```ts
it("publishes bare agents and effective profiles synchronously with zero requests", () => {
  const controller = createController(catalogFixture())
  controller.setQuery("")
  expect(
    controller.getSnapshot().groups.agent.items.map((item) => item.reference.uri)
  ).toEqual([
    "codeg://agent/codex",
    "codeg://delegation-profile/code_buddy/11111111-1111-4111-8111-111111111111",
  ])
  expect(mocks.startReferenceSearch).not.toHaveBeenCalled()
  expect(mocks.matchReferenceRegex).not.toHaveBeenCalled()
  expect(mocks.validateReferenceCandidate).not.toHaveBeenCalled()
})

it("publishes each first page without waiting for sibling sources", async () => {
  const controller = createController(catalogFixture())
  controller.setQuery("app")
  mocks.file.resolve(page("file", 0, [fileCandidate("app.ts")], true))
  await flushMicrotasks()
  expect(controller.getSnapshot().groups.file.items).toHaveLength(1)
  expect(controller.getSnapshot().groups.session.loading).toBe(true)
  expect(controller.getSnapshot().groups.commit.loading).toBe(true)
})

it("restarts only an expired source with a higher source sequence", async () => {
  const controller = createController(catalogFixture())
  controller.setQuery("fix")
  mocks.conversation.reject(appError("job_expired"))
  await flushMicrotasks()
  expect(sequencesFor("conversation")).toEqual([1, 2])
  expect(sequencesFor("file")).toEqual([1])
  expect(sequencesFor("commit")).toEqual([1])
})

it("not_match is query-local while not_found evicts by captured revision", async () => {
  const controller = createController(cachedConversationFixture())
  controller.setQuery("old")
  controller.setSelectedUri("codeg://session/7")
  mocks.validation.resolve(validation("not_match", freshSessionCandidate("7")))
  await flushMicrotasks()
  expect(controller.getSnapshot().groups.session.items).toHaveLength(0)
  controller.setQuery("fresh")
  expect(
    controller.getSnapshot().groups.session.items[0].reference.uri
  ).toBe("codeg://session/7")
})
```

In these tests, `catalogFixture` returns one enabled/available Codex descriptor plus one enabled CodeBuddy profile with the typed URI above; `createController` injects a fresh cache, deterministic UUID generator, deferred API mocks, backend key, limit, and Git/workspace identity. Define `page`, `fileCandidate`, `freshSessionCandidate`, `validation`, `appError`, `sequencesFor`, and `flushMicrotasks` in this test file with the exact Task 2/6 wire types. `cachedConversationFixture` seeds the same injected cache before constructing the controller. No helper comes from Task 9.

Add `bare_catalog_applies_effective_enablement_without_availability_probes`: include an enabled-but-unavailable base agent, a disabled base agent, profiles disabled by each of global delegation Off/profile Off/backing-agent Off, and one effectively enabled profile. Assert the unavailable enabled agent and effective profile remain, every disabled entry is absent, and no mention-triggered API mock was called.

Add `commit_page_epoch_mismatch_refreshes_identity_and_restarts_only_commit`: begin with Git epoch A, resolve the first commit page with epoch B, and make `fetchGitHead` return the full epoch-B identity. Assert the mismatched page's candidate never publishes or enters the cache, `applyGitHead` receives that exact returned identity once after the guard passes, commit sequences become `[1, 2]`, and file/conversation sequences remain `[1]`. Repeat the case with initially missing canonical repo/epoch to prove an authoritative page initializes identity through the same guarded fetch/apply/restart path. Add `git_head_input_change_on_the_same_branch_restarts_only_commit`: call `updateInputs` with a different full `head_sha`/`reference_source_epoch` but the same branch and assert the commit bucket/sequence changes while healthy sibling identities and results remain untouched. Add `closed_controller_does_not_restart_for_input_changes`: close a non-empty query, apply limit/catalog/Git input changes, assert no new API call, then call `setQuery` and assert normal work resumes.

Add `mixed_repository_commit_page_is_rejected_before_any_cache_merge`: use the current epoch but return two commit candidates whose `canonicalRepo` values are the current repo and a different repo. Require a generic commit-group protocol error, zero candidate merges/publications from that page, and no Git identity refresh because the page epoch itself was current.

Add `concurrent_page_and_validation_epoch_changes_share_one_git_refresh`: hold one injected fetch promise, deliver a commit page mismatch and `source_epoch_changed` validation for the same captured identity, and require one fetch invocation. Resolve it, then require one guarded `applyGitHead`, one internal identity adoption, one higher commit sequence, and no file/conversation restart; a late second waiter performs no additional effect. Add a stale-fetch branch: change `updateInputs.gitHead` before the held fetch resolves and require zero apply/adopt/restart from the old gate.

Add `known_non_repo_skips_commit_while_unknown_identity_may_probe`: with `gitHead: { is_repo: false, ...null identity }`, run a non-empty query and assert file/conversation start once while commit never starts and has no error; repeat with `gitHead: null` and assert commit does start so an uninitialized identity can self-heal. Add an unborn identity/page case proving its non-empty epoch selects a commit bucket and an empty exhausted page remains a normal empty group rather than a source error.

Add `catalog_literal_and_regex_descriptors_share_declared_fields`: prove an agent matches its display name as primary and snake-case type/description as secondary, while a profile matches its displayed `<agent label>:<profile name>` as primary and type/backing-agent description/configured model as secondary; assert the regex helper receives the identical arrays used by literal ranking. Add `regex_catalog_over_1024_batches_without_truncation_or_more_than_four_in_flight`: generate 4,100 descriptors, hold every matcher promise behind a deferred gate, assert batch sizes `[1024, 1024, 1024, 1024, 4]`, observe exactly four concurrent calls before releasing a gate, then release all gates and assert all returned stable IDs appear. Add `catalog_change_restarts_only_regex_catalog_matcher`, proving only matcher calls increase while resource start/cancel counts remain unchanged.

Add `one_failed_regex_catalog_batch_never_publishes_a_partial_prefix`: seed continuity rows, resolve four of five batches with matches and reject the last with a source/transport error, then require no new run membership from the successful batches, continuity rows non-selectable with only the Agent/Profile error, and no resource-source cancellation/restart.

Add `late_regex_catalog_run_cannot_replace_a_newer_catalog`: start a regex helper run for catalog A, update inputs to catalog B before A resolves, resolve B first and A last, and assert only B membership remains while file/conversation/commit identities never restart. This test requires a catalog-run guard separate from the whole-query generation because both helper runs intentionally belong to the same resource query generation.

Add `conversation_cache_events_update_an_open_controller_without_restarting_sources`: open a literal query with cached sessions, call `markConversationUpsert` and require the published row metadata to change, call `markConversationStatus` and require status matching/ranking to update, then call `markConversationDelete` for the selected URI and require it to disappear immediately while all three resource start/cancel counts remain unchanged. Close the controller, emit another cache change, and prove no publication occurs; call `setQuery` again and prove the latest cache state is read and the subscription is reacquired. Add a regex variant asserting an upsert/status removes an unselected stale snapshot reference but keeps the selected URI as non-fresh pending validation.

Add `file_cache_uses_only_an_authoritative_canonical_root_alias`: seed a file bucket under canonical root `C:/real`, remember the backend-scoped alias from requested `C:/link`, and require a literal query at `C:/link` to publish that cache immediately. In a fresh cache with no alias, require zero provisional file rows; return a first authoritative page whose metadata says `canonicalWorkspaceRoot: "C:/real"`, then require the alias, bucket, and published row to appear together. Return a later page with a different canonical root and require the whole page to be rejected as a source protocol error rather than mixed into either bucket. An empty exhausted first page must leave the alias absent.

Add `close_is_idempotent_and_reopen_allocates_new_source_identities`: start all resources, call `close()` twice, require one guarded cancel per live identity, one conversation-cache unsubscribe, and one cache-pin release, then call `setQuery` again. The controller keeps its original `searchSessionId`, allocates higher per-source sequences with fresh request IDs, and owns exactly one new cache subscription.

Add `invalid_or_cancelled_validation_never_falls_through_to_cached_insertion`: select a cache-only candidate, reject validation with typed `invalid_request`, `invalid_pattern`, and `cancelled` in table cases, and require `confirmCandidate` to return null. Keep a separate operational source/transport-error case that returns the still-current cached reference, matching the approved availability fallback.

In `app-workspace-store.test.ts`, add `apply_git_head_updates_when_full_head_or_reference_epoch_changes_on_the_same_branch`: apply two `GitHeadInfo` values with identical branch/detached/short-SHA fields but different `head_sha`/`reference_source_epoch`, and assert the second complete object is stored. This prevents the existing equality guard from dropping ordinary same-branch commits, whose legacy `short_sha` is null.

- [ ] **Step 2: Run controller/hook tests and confirm RED**

Run:

```powershell
pnpm test -- src/components/chat/composer/reference-search-controller.test.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/rich-composer-mention.test.tsx src/stores/app-workspace-store.test.ts
```

Expected: FAIL because the controller hook does not exist and the only current hook returns one aggregate Promise that waits on all sources.

- [ ] **Step 3: Implement generation-safe controller state and independent drain loops**

Define snapshots by group, not one global loading flag:

```ts
export interface ReferenceGroupSnapshot {
  kind: ReferenceGroupKind
  label: string
  items: SuggestionItem[]
  loading: boolean
  truncated: boolean
  error: "profile" | "source" | null
}

export type ReferenceGroupKind = "agent" | "file" | "session" | "commit"

export interface ReferenceSearchSnapshot {
  query: string
  generation: number
  patternError: boolean
  groups: Record<"agent" | "file" | "session" | "commit", ReferenceGroupSnapshot>
}
```

Extend the existing `SuggestionItem` with controller-owned fields `selectable: boolean`, `freshness: "cache" | "fresh" | "validating"`, `sourceOrdinal: number`, and `regexRank: ReferenceRegexRank | null`. Stable identity remains `item.reference.uri`; do not duplicate it as a second top-level `uri` field. Agent/profile adapters assign deterministic source ordinals and non-null existing `codeg://agent/...` / typed `codeg://delegation-profile/<agent_type>/<uuid>` URIs.

Add `candidateToSuggestion(candidate: ReferenceCandidate, freshness: SuggestionItem["freshness"], selectable = true): SuggestionItem`; it maps backend file/conversation/commit metadata to existing `ReferenceAttrs` without rebuilding the authoritative URI. Map backend source `conversation` to `refType: "session"`; map file `candidate.metadata.entryKind === "directory"` to existing `meta.fileKind: "dir"` and `"file"` unchanged; map conversation `candidate.metadata.agentType`/`status`/`branch` into existing meta; map commit `candidate.metadata.shortHash`/`subject`/`author` to `shortHash`/`message`/`author` with `pushed: null`. In every arm copy `candidate.id`, `candidate.label`, `candidate.uri`, `candidate.detail`, `candidate.keywords`, `candidate.sourceOrdinal`, and `candidate.regexRank` without deriving alternate identity/rank values. Keep the old `FlatFileEntry`/`DbConversationSummary`/`GitLogEntry` adapter exports only for the transitional aggregate hook, filling the new fields with `selectable: true`, `freshness: "fresh"`, deterministic input-order ordinals supplied by that old builder, and `regexRank: null`. Update `adapters.test.ts`, the legacy popup fixtures, and the RichComposer mention fixture with complete item shapes so Task 8 introduces no type-broken test objects. Task 9 removes those three obsolete source adapters together with the aggregate hook; agent/profile and backend-candidate adapters remain.

Build local catalog fields through `catalogSearchFields` only. For an agent, primary is `[agent.name || AGENT_LABELS[agent.agent_type]]` and secondary is `[agent.agent_type, agent.description, ""]` because the current base-agent catalog has no per-agent model field. For a profile, primary is ``[`${AGENT_LABELS[profile.agent_type]}:${profile.name}`]`` and secondary is `[profile.agent_type, backingAgent.description, profile.config_values.model ?? ""]`. Preserve empty description/model slots so regex field tiers stay comparable across catalog entries. Literal filtering calls Task 7's `rankLiteralFields`; regex descriptors use the non-null canonical mention URI as `id` and copy the same primary/secondary arrays verbatim. This prevents agent-type/profile-UUID namespace collisions and is the concrete meaning of the approved `Agent/Profile | Display name | Agent type, description, model` mapping.

Sort each collected group with one mode-specific tuple. Literal items use `(rankLiteralFields(...), sourceOrdinal)` where the rank is 0-4; regex items require non-null authoritative metadata and use `(regexRank.fieldTier, regexRank.start, regexRank.length, sourceOrdinal)`. Compare all components numerically and use URI only as a deterministic final fallback when two malformed/duplicate ordinals tie. Never execute a regex or slice a JavaScript string with the returned UTF-8 byte offsets.

Generate and retain one canonical UUIDv4 `searchSessionId` in the controller constructor. Initialize each source sequence to zero, increment and safe-integer-check it before every start/restart, and generate a new canonical UUIDv4 request ID for that exact sequence. Page and cancel payloads copy the active identity verbatim. A query/limit/close transition increments the frontend generation but never resets a source sequence; reopening the same controller therefore cannot collide with a retained backend high-water record. Recreating the controller for a different backend/folder/path generates a new session ID.

Before a start/page response can mutate collected state or cache, require the captured frontend generation plus its echoed `sourceSequence`, `requestId`, and exact expected `pageIndex` to match the source's current identity. Validate the whole `items` array has the requested `candidate.source` and matching metadata variant before merging any item; file and commit then apply their stronger canonical-root/repository checks below. An old/malformed echo is discarded (malformed current-identity data also marks only that source as a protocol error). This guard is mandatory even when an `AbortSignal` was supplied, because Tauri and remote-desktop transports cannot physically abort a dispatched IPC call.

`setQuery` increments generation, reacquires the conversation-cache subscription when transitioning from inactive to active, guarded-cancels all old live identities/validation, and invalidates old working regex refreshes. Compute the next synchronous catalog/cache projection while the prior selected/visible pins still protect the candidates needed for URI preservation. Before notifying subscribers, run one controller `syncVisiblePins` boundary: build the current `{ bucket -> visible URI set }` projection, replace each current bucket's pins, write an empty set for every previously pinned bucket no longer present, and only then allow pruning. Every later page/event/rerank publication uses that same boundary, so an item is pinned before React can display it. Do not call `releaseController` during a query transition, and do not duplicate popup selection logic inside `setQuery`: `setSelectedUri` is the sole selected-pin writer, so the prior selected pin remains briefly until Task 9 reconciles the synchronous snapshot and calls it with the preserved URI or `null`. Publish that projection synchronously and for non-empty queries start the resource sources independently. File requests require non-null `defaultPath`; commit requests require non-null `folderId`/`defaultPath` and start when Git identity is unknown (`gitHead === null`) or known to be a repository. A known `gitHead.is_repo === false` leaves the commit group empty without a request or error. File/commit requests carry `defaultPath`; conversation requests omit `workspacePath` because their scope is backend-global and start regardless of folder scope.

Each source owns `{ sourceSequence, requestId, pageInFlight }`, publishes each page, and requests exactly the next page while collected count is below the current limit and `done` is false. Immediately before dispatching conversation start/page calls, capture `cache.captureMutationRevision()` on that in-flight page record; every candidate from its response calls `mergeCandidate` with that exact `conversationPageStartedAt`, never a receipt-time value. File/commit page merges and positive validation merges omit the option. Every page candidate and every `match`/`not_match` validation candidate must pass through `mergeCandidate`; publish/collect it only when the returned cached entry is non-null, so a late page or validation cannot bypass a conversation delete tombstone or newer upsert/status watermark. Set `truncated` only when the backend terminal page says `doneReason: "limit"` (or a local cache contains more literal candidates than the displayed limit); `exhausted`, loading, and source failure do not guess that more matches exist. Backend source `conversation` maps only at the adapter boundary to frontend group `session`; no snapshot has a `conversation` property.

For a file cache preview, call `resolveFileRootAlias(backendKey, defaultPath)` and construct a bucket only when it returns a canonical root; JavaScript never canonicalizes `defaultPath` or creates a provisional requested-path bucket. For an authoritative file page, require every candidate to be a file candidate with one identical non-empty `metadata.canonicalWorkspaceRoot`. On the first non-empty page, call `rememberFileRootAlias`, switch the current file bucket to that canonical root, remove old-alias provisional rows from current membership, and only then merge/publish the page. If a remembered alias points elsewhere, the authoritative root atomically repoints it and the controller discards any old-bucket working regex refresh before beginning one in the new bucket. The old canonical cache bucket itself remains intact for other controllers. A mixed/missing root is an invalid source response and no candidate from that page enters cache. An empty first page has no root authority, creates no alias/snapshot, and publishes only its normal empty/exhausted source state.

Commit previews require both `gitHead.canonical_repo` and `gitHead.reference_source_epoch`; if either is absent, publish no provisional commit cache but still allow an authoritative commit start when stable `folderId` and `defaultPath` exist. Before any commit page merges, require a non-empty `page.sourceEpoch` equal to the controller's current `gitHead.reference_source_epoch`, then validate the whole page contains only commit candidates whose `metadata.canonicalRepo` exactly equals the current `gitHead.canonical_repo`. Perform these checks before merging the first item so a malformed mixed-repository page cannot partially contaminate the bucket. A missing epoch/source/repository field is an invalid protocol response and becomes a generic commit-group error. On a missing or mismatched current identity, discard the entire page without caching any candidate and enter one controller-owned Git-refresh gate keyed by `{ generation, oldCommitIdentity }`. Page and validation epoch errors for that key join the same promise; only its creator calls the injected `fetchGitHead`. After await, first recheck both key fields and active state; only a current result calls the injected synchronous `applyGitHead(head)` exactly once, adopts that same object internally, switches buckets, and restarts commit once. A stale result performs none of those effects. Clear the gate on settle, query/identity change, or close. Even when the fetch returns the page's epoch, do not merge the discarded page; the restarted job is the first authority for the new bucket. This implements the design's discard rule and lets an authoritative first page initialize previously missing Git identity safely.

`updateInputs` compares semantic values rather than object identity. Agent/profile/error changes recompute the local group as described below, with `profileCatalogError` mapping only to the Agent/Profile group's `error: "profile"`; label-only changes republish labels without restarting work; a reference-limit change retruncates caches, increments the whole query generation, and restarts every eligible resource only for a current active non-empty query. Treat `is_repo` as part of commit identity alongside canonical repo/full HEAD/reference epoch: a change to known non-repo guarded-cancels commit and leaves it idle, while a change from non-repo/unknown to a repository starts or restarts only commit when active. Conversation-cache notifications affect only the session group: on upsert, update an existing current-generation row from the carried summary while preserving its project metadata even when the cache revision is null; on status, patch only an existing row's status/detail. Literal mode then reranks/removes the row by the current query, while regex mode drops an unselected stale reference or keeps the selected one non-fresh and starts its ordinary selection validation. Delete removes the URI from both current collected membership and the published snapshot. None of these notifications adds an unrelated membership or restarts a source. Keep an explicit internal `active` flag: every `setQuery`, including `setQuery("")` for bare `@`, sets it true. Only an active-to-inactive `close()` transition increments generation, unsubscribes from conversation-cache changes, cancels identities/validation, discards working regex refreshes, and releases pins; repeated `close()` while inactive is a no-op. While inactive, cache notifications cannot retain or call the controller, and `updateInputs` may update stored inputs/cache projections but starts no helper, resource, Git refresh, or validation call. A later `setQuery` resubscribes and reuses the controller normally.

For regex, increment a catalog-regex run generation every time the enabled catalog changes or a new query starts, then partition enabled agent/profile descriptors in stable batches of at most 1,024, use a four-permit frontend semaphore, and never execute the regex in JavaScript. Collect validated batch results in one run-local stable-ID map without truncation; only after every batch in the current run succeeds may the controller atomically replace Agent/Profile membership and publish it. An unknown/duplicate returned ID or a returned `sourceOrdinal` that differs from its descriptor is a helper protocol failure. Capture both the whole-query generation and catalog-run generation in every batch; a result may commit only when both still match. Any batch failure discards the whole working map, retains prior continuity rows as non-fresh/non-selectable where applicable, and marks only Agent/Profile; it never exposes a successful prefix as a complete catalog. Query change and `close()` invalidate the current catalog run, while a catalog-only change increments only this run generation and leaves all resource identities untouched. Every resource start still validates the pattern server-side before registration.

Map errors exactly: `cancelled`/`stale_start` silent; `job_expired`/`stale_page`/`limit_epoch_changed` restart only that source; `source_epoch_changed` and a locally detected commit-page epoch mismatch use the guarded refresh/discard/restart path above; invalid pattern retains continuity rows non-selectable; timeout/overload/source failure retain cache plus group error; invalid request logs detail plus generic group error. A late Git refresh is guarded by the query generation and old commit identity before it may change state.

Validation records `{ validationRequestId, bucket, generation, selectedUri, mutationRevision }`. New selection aborts transport when supported and always invalidates the prior ID. Fresh current-generation resource rows and local Agent/Profile rows confirm immediately; only cache-only/non-fresh resource rows start validation. Build its request from the captured source: file uses the controller's exact `defaultPath` and no epoch, conversation omits both workspace and epoch, and commit uses the exact `defaultPath` plus the captured bucket epoch. `confirmCandidate` reuses an in-flight validation and waits at most one second. Before any outcome mutates cache or membership, require the echoed validation request ID, bucket, generation, selected URI, and captured mutation revision to remain current. For `match`, merge the rebuilt candidate, adopt its new mutation revision, and mark it fresh/selectable only when `mergeCandidate` returns non-null. For `not_match`, merge the rebuilt live metadata under the same guard, then remove only current-query membership and return null. `not_found` conditionally evicts the captured revision and returns null. `cancelled`, `invalid_pattern`, `invalid_request`, and any identity/guard mismatch return null without insertion or cache mutation; only a timeout or operational backend/transport failure invalidates the request and returns the still-current cached reference for insertion. On conversation `not_found`, call `markConversationNotFoundIfRevision`; file/commit continue to use `evictIfRevision`. A validation `source_epoch_changed` joins the same guarded Git fetch/apply path as a page error and restarts only commit search; it never falls through the operational-error branch that permits insertion.

`useReferenceSearchController` creates one controller per active backend/folder/path only after `referenceCatalogReady`. The stable constructor dependencies are `{ backendKey, folderId, defaultPath, fetchGitHead, applyGitHead }`; the hook calls `updateInputs` with agents, profile catalog, `profileCatalogError: Boolean(profileStore.error)`, settings limit, complete `GitHeadInfo`, and labels without recreating the controller. Implement `fetchGitHead` as one stable callback that only awaits and returns `getGitHead(path)`. Implement `applyGitHead` as a separate stable synchronous callback that calls `appWorkspaceStore.applyGitHead(folderId, head)`; only the controller invokes it after its async guard passes. The controller then adopts that exact object immediately instead of waiting for the React/store round trip. Close/recreate on backend, folder, path, or disable transitions and close on unmount. `close` must make duplicate hook/composer cleanup harmless by checking `active` and per-source live identity before unsubscribing, cancelling, discarding refreshes, or releasing pins. Before readiness return `null`, keeping the mention controller inert; Task 9 keeps the Tiptap extension installed so a later non-null controller works without rebuilding the editor. Profile bootstrap failure still reaches ready and yields agent-only local results plus a profile-group error. A settings limit change retruncates cache, increments generation, and restarts only a current non-empty query; bare `@` remains local. Leave the old aggregate `useReferenceSearch` implementation untouched in this task as a short-lived compatibility export; none of the new tests or controller code may call it.

Extend `appWorkspaceStore.applyGitHead` equality to compare `canonical_repo`, `head_sha`, and `reference_source_epoch` in addition to the legacy fields. A changed full HEAD/epoch must publish a new map entry even when branch, detached, and short SHA are unchanged; otherwise the controller can never switch commit buckets for a normal new commit on the same branch.

An applied agent/profile catalog change recomputes bare/literal agent rows synchronously. For a current regex query, invalidate only the agent-group membership and rerun its descriptor batches under the existing query generation; do not cancel or restart file, conversation, or commit identities. Add focused assertions for both literal and regex catalog changes.

- [ ] **Step 4: Verify controller generations, source isolation, and cache validation**

Run:

```powershell
pnpm test -- src/components/chat/composer/reference-search-controller.test.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/rich-composer-mention.test.tsx src/lib/reference-search-cache.test.ts src/stores/app-workspace-store.test.ts
pnpm eslint src/components/chat/composer/reference-search-controller.ts src/components/chat/composer/use-reference-search.ts
```

Expected: all commands exit 0; cached previews are synchronous, pages are independent, old generations never mutate state, and source-only restarts preserve healthy sibling results.

- [ ] **Step 5: Commit the controller**

```powershell
git add src/components/chat/composer/reference-search-controller.ts src/components/chat/composer/reference-search-controller.test.ts src/components/chat/composer/use-reference-search.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/composer/suggestion/types.ts src/components/chat/composer/suggestion/adapters.ts src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/rich-composer-mention.test.tsx src/stores/app-workspace-store.ts src/stores/app-workspace-store.test.ts
git commit -m "feat(references): add independent search controller"
```

---

### Task 9: Preserve URI Selection Through Paging, Ranking, and Validation

**Files:**
- Modify: `src/lib/api.ts`
- Modify: `src/components/chat/composer/suggestion/suggestion-popup.tsx`
- Modify: `src/components/chat/composer/suggestion/suggestion-popup.test.tsx`
- Modify: `src/components/chat/composer/suggestion/types.ts`
- Modify: `src/components/chat/composer/suggestion/adapters.ts`
- Modify: `src/components/chat/composer/suggestion/adapters.test.ts`
- Modify: `src/components/chat/composer/rich-composer.tsx`
- Modify: `src/components/chat/composer/rich-composer-mention.test.tsx`
- Modify: `src/components/chat/composer/use-reference-search.ts`
- Modify: `src/components/chat/composer/use-reference-search.test.ts`
- Modify: `src/components/chat/chat-input.tsx`
- Modify: `src/components/chat/conversation-shell.tsx`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/chat/message-input.test.tsx`
- Modify: `src/components/automations/automation-editor.tsx`
- Modify: `src/components/conversations/conversation-detail-panel.tsx`

**Interfaces:**
- Changes: `SuggestionPopup` consumes `ReferenceSearchController`, not `ReferenceSearch`
- Changes: `RichComposerProps.referenceController?: ReferenceSearchController | null` replaces `referenceSearch?: ReferenceSearch`
- Stores: `selectedUri: string | null`, `pinnedTab: ReferenceGroupKind | null`, and `confirmingUri: string | null`
- Produces: async `selectCandidate(uri, range)` that calls `controller.confirmCandidate` before insertion
- Preserves: current active group while it has a valid selected item; switching groups intentionally selects that group's first selectable item
- Removes: legacy aggregate `ReferenceSearch`, `buildReferenceGroups`, `useReferenceSearch`, and the now-unused frontend `getDelegationProfiles`; both message and automation composers use `useReferenceSearchController`
- Changes: `MentionUiLabels` adds optional `invalidPattern`, `sourceError`, and `profileError` strings with English popup fallbacks; Task 10 supplies localized values

- [ ] **Step 1: Write failing stable-selection and nearest-survivor tests**

```ts
it("keeps the selected URI when pages insert and rerank around it", async () => {
  const controller = fakeController(snapshotWithFiles([file("b.ts"), file("c.ts")]))
  const { ref } = mountPopup({ controller })
  act(() => ref.current?.onKeyDown(key("ArrowDown")))
  expect(activeUri()).toBe("file:///repo/c.ts")
  controller.publish(snapshotWithFiles([file("a.ts"), file("b.ts"), file("c.ts")]))
  expect(activeUri()).toBe("file:///repo/c.ts")
})

it("moves explicit invalidation to same index then previous then none", () => {
  const controller = fakeController(snapshotWithSessions([session("1"), session("2"), session("3")]))
  const { ref } = mountPopup({ controller })
  act(() => ref.current?.onKeyDown(key("ArrowDown")))
  controller.publish(snapshotWithSessions([session("1"), session("3")]))
  expect(activeUri()).toBe("codeg://session/3")
  controller.publish(snapshotWithSessions([session("1")]))
  expect(activeUri()).toBe("codeg://session/1")
  controller.publish(snapshotWithSessions([]))
  expect(activeUri()).toBeNull()
})

it("consumes Enter while validation is pending and inserts only a permitted result", async () => {
  const controller = validatingController(file("cached.ts"))
  const { ref, onSelect } = mountPopup({ controller })
  act(() => expect(ref.current?.onKeyDown(key("Enter"))).toBe(true))
  act(() => expect(ref.current?.onKeyDown(key("Enter"))).toBe(true))
  expect(controller.confirmCallCount()).toBe(1)
  expect(onSelect).not.toHaveBeenCalled()
  controller.resolveConfirmation(file("cached.ts").reference)
  await waitFor(() => expect(onSelect).toHaveBeenCalledOnce())
})
```

Define `FakeReferenceSearchController`, `fakeController`, `validatingController`, `snapshotWithFiles`, `snapshotWithSessions`, `file`, `session`, `mountPopup`, `key`, and `activeUri` in `suggestion-popup.test.tsx`. The fake implements every Task 8 controller method, stores listeners, and `publish` synchronously notifies them; validating confirmation resolves through an explicit deferred promise and exposes `confirmCallCount()`. These helpers return complete `SuggestionItem` values with `reference.uri`, `selectable`, freshness, ordinal, and regex rank fields.

Add `known_negative_confirmation_keeps_picker_open_on_nearest_survivor`: make confirmation resolve `null` after the controller publishes removal of the selected URI, assert no insertion/close callback fires, and assert the reconciled nearest URI remains active and can be confirmed next.

Add `settled_confirmation_cannot_insert_after_the_same_query_moves_range`: begin confirmation, publish a `MentionRenderState` with unchanged query but a different `{ from, to }`, resolve the old promise non-null, and assert no insertion/close occurs. A query-only guard is insufficient because document edits before the trigger can remap its range without changing its text.

Add `bare_query_publishes_catalog_without_the_legacy_fetch_debounce`: mount with fake timers and a controller whose `setQuery("")` synchronously publishes one agent, assert the option is present before advancing any timer, and assert the module schedules no 150 ms search timeout. This prevents the old aggregate-Promise debounce from surviving the controller migration.

Add `message_input_forwards_folder_scope_to_controller_hook` in `message-input.test.tsx`: make the hoisted hook mock capture its options, render with `folderId={7}`, `defaultPath="C:/repo"`, and `isActive`, and assert both exact scope values plus `enabled: true`; rerender with `folderId={null}` and no path, then assert the hook receives both scope values as null rather than converting the folder ID to `0`.

In `rich-composer-mention.test.tsx`, add `controller_becomes_available_after_editor_mount_without_rebuilding_the_editor`: mount with `referenceController={null}`, retain the editor instance, rerender with a fake controller, type bare `@`, and assert the same editor instance opens/publishes the picker. Add `replacing_or_disabling_controller_has_one_idempotent_close_effect`: open with controller A, rerender with B and then null, and require one active-to-inactive close effect for A/B plus popup teardown without a stale insertion. The fake's `close` mirrors Task 8 idempotency, because hook cleanup and composer ownership may both invoke it during the same React transition; repeated raw calls must not duplicate cancels, unsubscribes, or pin release.

- [ ] **Step 2: Run popup/composer tests and confirm RED**

Run:

```powershell
pnpm test -- src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/rich-composer-mention.test.tsx src/components/chat/composer/use-reference-search.test.ts src/components/chat/message-input.test.tsx
```

Expected: FAIL because the popup resets `selectedIndex` on every resolved search and composer callers still consume the legacy aggregate hook.

- [ ] **Step 3: Subscribe to controller snapshots and implement URI-based selection**

Delete the transitional aggregate hook/type/helpers and their obsolete tests first, then remove the old `FlatFileEntry`/`DbConversationSummary`/`GitLogEntry` adapter exports and their tests; keep agent/profile plus `candidateToSuggestion`. Remove `getDelegationProfiles` from `src/lib/api.ts` only after confirming Task 1 migrated both settings-panel callers and this task removed the final mention-hook caller; keep the backend route for transport compatibility. Add nullable `folderId` props through `ConversationDetailPanel -> ConversationShell -> ChatInput -> MessageInput`, and pass the same value to the detail panel's direct welcome-mode `ChatInput`; use `ownFolderId`, not the detail panel's `0` command fallback, so folderless drafts remain explicitly unscoped. `MessageInput` calls `useReferenceSearchController` with that authoritative folder ID plus path. `AutomationEditor` already owns `folderId` and `folderPath`, so pass both there as well. A null folder ID disables only commit identity/refresh; agents, profiles, backend-global conversations, and files with a non-null open path still work. Both composers pass the returned value as `RichComposer.referenceController`; update `message-input.test.tsx` to mock the hook as returning `null` by default.

Preserve RichComposer's existing one-editor lifecycle: always install `MentionSuggestion` with the stable React-to-ProseMirror `MentionController`, store the latest `referenceController` in a ref, and make `onStart`/`onUpdate` inert while that ref is null. Do not conditionally remove the extension when catalog readiness is false, because the editor is not rebuilt when the prop later becomes non-null. When the controller prop changes while a picker is open, close the previous controller, clear mention state/ARIA ownership, and let a later trigger use the new ref; disabling it follows the same path. The Tiptap controller's `onExit` closes exactly the instance that owns the active picker, not whichever replacement happens to be current by the time an async callback runs.

Delete `FETCH_DEBOUNCE_MS`, its timeout ref/state, and the aggregate Promise effect. Subscribe with `useSyncExternalStore(controller.subscribe, controller.getSnapshot, controller.getSnapshot)` using controller methods implemented as bound arrow properties (or stable wrappers) so React never loses their receiver, then call `controller.setQuery(state.query)` from the existing isomorphic layout effect whenever the exact query changes. Because `setQuery` publishes catalog/cache rows synchronously, the external-store update rerenders before paint; do not put a timer or async Promise in front of bare `@`. On each snapshot/query:

```ts
function reconcileSelectedUri(
  previousUri: string | null,
  previousIndex: number,
  nextItems: SuggestionItem[]
): string | null {
  if (
    previousUri &&
    nextItems.some(
      (item) => item.reference.uri === previousUri && item.selectable
    )
  ) {
    return previousUri
  }
  const sameIndex = nextItems[previousIndex]
  if (sameIndex?.selectable) return sameIndex.reference.uri
  for (let index = Math.min(previousIndex - 1, nextItems.length - 1); index >= 0; index--) {
    if (nextItems[index].selectable) return nextItems[index].reference.uri
  }
  return null
}
```

For literal query changes, preserve only when the provisional matcher accepts the URI. For an exact repeated regex, preserve only when the complete cached snapshot contains it. For a new regex, continuity rows are non-selectable and selection is null until an authoritative match arrives.

Feed `selectedUri` to the controller so cache pins and selection-scoped validation stay current. When rank truncation would hide it, reserve one visible slot for the selected item and omit the lowest-ranked unselected item. Do not auto-switch to a newly non-empty group while the current group has a selectable selection. Tab/click group changes set its first selectable URI.

Arrow keys move by current URI index. Use `item.reference.uri` as each option's React key; keep the index-based DOM id only for the current `aria-activedescendant` relationship. Enter/mousedown capture `{ controller, query, range.from, range.to, uri }`, set `confirmingUri`, and then call `confirmCandidate`; while it is non-null, consume repeated activation without issuing another confirmation or attaching another insertion continuation. Clear it only when that exact promise settles. A non-null result inserts exactly once and then closes (including the controller's timeout-permitted cached result); a null known-negative result inserts nothing, keeps the picker open, and lets the published snapshot reconcile to the nearest survivor. Any controller, query, range, or unmount/exit change invalidates the capture so a settled old promise cannot insert at a remapped trigger. The mention controller's `onExit` calls `controller.close()` to cancel live source/validation identities and release cache pins without clearing cached items; the same controller instance may accept a later `setQuery` when the picker reopens. Use the three optional `MentionUiLabels` error strings with English fallbacks: show one panel-level invalid-pattern row while continuity items remain non-selectable, and keep profile/source errors in only their own group. Stale continuity rows use `aria-disabled="true"` and cannot be selected.

- [ ] **Step 4: Verify selection, accessibility, and async confirmation**

Run:

```powershell
pnpm test -- src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/rich-composer-mention.test.tsx src/components/chat/composer/use-reference-search.test.ts src/components/chat/message-input.test.tsx
pnpm eslint src/lib/api.ts src/components/chat/composer/suggestion/suggestion-popup.tsx src/components/chat/composer/suggestion/adapters.ts src/components/chat/composer/rich-composer.tsx src/components/chat/composer/use-reference-search.ts src/components/chat/chat-input.tsx src/components/chat/conversation-shell.tsx src/components/chat/message-input.tsx src/components/automations/automation-editor.tsx src/components/conversations/conversation-detail-panel.tsx
```

Expected: all commands exit 0; `aria-activedescendant` follows URI selection, invalid rows cannot insert, and source updates do not jump the active candidate.

- [ ] **Step 5: Commit stable popup selection**

```powershell
git add src/lib/api.ts src/components/chat/composer/suggestion/suggestion-popup.tsx src/components/chat/composer/suggestion/suggestion-popup.test.tsx src/components/chat/composer/suggestion/types.ts src/components/chat/composer/suggestion/adapters.ts src/components/chat/composer/suggestion/adapters.test.ts src/components/chat/composer/rich-composer.tsx src/components/chat/composer/rich-composer-mention.test.tsx src/components/chat/composer/use-reference-search.ts src/components/chat/composer/use-reference-search.test.ts src/components/chat/chat-input.tsx src/components/chat/conversation-shell.tsx src/components/chat/message-input.tsx src/components/chat/message-input.test.tsx src/components/automations/automation-editor.tsx src/components/conversations/conversation-detail-panel.tsx
git commit -m "feat(references): preserve mention selection by URI"
```

---

### Task 10: Restore Mentions After IME, Add Settings/Copy, and Verify End to End

**Files:**
- Modify: `src/components/chat/composer/suggestion/mention-suggestion.ts`
- Create: `src/components/chat/composer/suggestion/mention-suggestion.test.ts`
- Modify: `src/components/chat/composer/rich-composer.tsx`
- Modify: `src/components/chat/composer/rich-composer-mention.test.tsx`
- Modify: `src/components/settings/conversation-experience-settings.tsx`
- Modify: `src/components/settings/conversation-experience-settings.test.tsx`
- Modify: `src/components/chat/message-input.tsx`
- Modify: `src/components/automations/automation-editor.tsx`
- Modify: `src/i18n/messages/ar.json`
- Modify: `src/i18n/messages/de.json`
- Modify: `src/i18n/messages/en.json`
- Modify: `src/i18n/messages/es.json`
- Modify: `src/i18n/messages/fr.json`
- Modify: `src/i18n/messages/ja.json`
- Modify: `src/i18n/messages/ko.json`
- Modify: `src/i18n/messages/pt.json`
- Modify: `src/i18n/messages/zh-CN.json`
- Modify: `src/i18n/messages/zh-TW.json`

**Interfaces:**
- Produces: idempotent `compositionend` microtask rematch through `SuggestionPluginKey`
- Adds: numeric reference limit control wired to `setReferenceSearchLimit`
- Adds: localized invalid-pattern, source-error, profile-error, and reference-limit labels in all ten catalogs

- [ ] **Step 1: Write failing composition and settings-control tests**

```ts
it.each(["中文", "english"])(
  "rematches @%s exactly once after compositionend",
  async (text) => {
    const fixture = mentionCompositionFixture(`@${text}`)
    fixture.startComposition()
    fixture.dispatchKey({ key: "Enter", isComposing: true, keyCode: 229 })
    expect(fixture.searchCount()).toBe(0)
    fixture.endComposition()
    await fixture.flushMicrotask()
    expect(fixture.currentQuery()).toBe(text)
    expect(fixture.searchCount()).toBe(1)
    fixture.dispatchEquivalentProseMirrorTransaction()
    expect(fixture.searchCount()).toBe(1)
    expect(fixture.insertCount()).toBe(0)
    expect(fixture.submitCount()).toBe(0)
  }
)

it("saves a clamped reference limit and adopts the returned revision", async () => {
  mocks.setReferenceSearchLimit.mockResolvedValue({
    auto_title_agent: "codex",
    reference_search_limit: 500,
    revision: 9,
  })
  renderSettings()
  fireEvent.change(await screen.findByLabelText("Reference result limit"), {
    target: { value: "999" },
  })
  fireEvent.click(screen.getByRole("button", { name: "Save reference limit" }))
  await waitFor(() => expect(mocks.setReferenceSearchLimit).toHaveBeenCalledWith(500))
  expect(useConversationExperienceStore.getState().settings?.revision).toBe(9)
})
```

Define `MentionCompositionFixture` and `mentionCompositionFixture(initialText)` in `mention-suggestion.test.ts` with a real Tiptap editor, the extension, and counters on the supplied controller. `startComposition`/`endComposition` dispatch real DOM composition events, `dispatchEquivalentProseMirrorTransaction` sends the same metadata-only rematch transaction, and `flushMicrotask` awaits two resolved promises. Define `renderSettings` and its API/store mocks in the existing settings component test file; reset the Zustand store before each case.

Add `unmount_after_compositionend_before_microtask_is_a_noop`: end composition, destroy the fixture before flushing, then assert no search, insertion, submission, or dispatch error.

In `rich-composer-mention.test.tsx`, add `compositionstart_closes_an_open_picker_and_blocks_pointer_confirmation`: open a populated picker, dispatch real `compositionstart`, attempt Enter and candidate mousedown while composing, and require one idempotent close with zero confirmation, insertion, or submit. End composition and require the normal one-microtask rematch to reopen from the final text.

- [ ] **Step 2: Run composition/settings/i18n tests and confirm RED**

Run:

```powershell
pnpm test -- src/components/chat/composer/suggestion/mention-suggestion.test.ts src/components/chat/composer/rich-composer-mention.test.tsx src/components/settings/conversation-experience-settings.test.tsx src/i18n/messages.test.ts
```

Expected: FAIL because composition end does not force a rematch and locale keys/limit control are absent.

- [ ] **Step 3: Implement idempotent IME rematch, limit control, and exact locale copy**

Import `Plugin` from `@tiptap/pm/state` and `findSuggestionMatch`/`SuggestionPluginKey` from `@tiptap/suggestion`. Add a second ProseMirror plugin beside the Tiptap suggestion plugin; DOM events belong in `Plugin.props.handleDOMEvents`, not in `PluginView`. Its `compositionstart` handler increments the sequence, calls the supplied mention controller's idempotent `onExit()` to close any old picker and cancel its pointer/validation path, and returns `false`. Its `compositionend` handler increments the sequence again, queues one microtask, and returns `false` so ProseMirror continues its normal composition handling. The microtask exits if a newer composition occurred or `view.composing` is still true, computes the current match with the same trigger options, compares query/range with `SuggestionPluginKey.getState(view.state)`, and dispatches one metadata-only transaction only when the suggestion plugin has not already reopened the same match:

```ts
const mentionMatchOptions = {
  char: "@",
  allowSpaces: false,
  allowToIncludeChar: false,
  allowedPrefixes: [" "],
  startOfLine: false,
}
let compositionSequence = 0
const compositionRematch = new Plugin({
  props: {
    handleDOMEvents: {
      compositionstart: () => {
        compositionSequence += 1
        controller.onExit()
        return false
      },
      compositionend: (view) => {
        const sequence = ++compositionSequence
        queueMicrotask(() => {
          if (
            sequence !== compositionSequence ||
            view.isDestroyed ||
            view.composing
          ) {
            return
          }
          const match = findSuggestionMatch({
            ...mentionMatchOptions,
            $position: view.state.selection.$from,
          })
          const current = SuggestionPluginKey.getState(view.state)
          const sameRange =
            current?.range.from === match?.range.from &&
            current?.range.to === match?.range.to
          if (
            !match ||
            (current?.active && current.query === match.query && sameRange)
          ) {
            return
          }
          view.dispatch(
            view.state.tr.setMeta("codegMentionCompositionRematch", sequence)
          )
        })
        return false
      },
    },
  },
})
```

Keep the suggestion and rematch plugins on the same options object, and return both from `addProseMirrorPlugins` with this complete existing-controller wiring:

```ts
const editor = this.editor
const controller = this.options.controller
const mentionSuggestion = Suggestion({
  editor,
  ...mentionMatchOptions,
  items: () => [],
  command: () => {},
  allow: ({ state }) => {
    if (editor.view.composing) return false
    return !state.selection.$from.parent.type.spec.code
  },
  render: () => ({
    onStart: (props) => controller.onStart(toRenderState(props)),
    onUpdate: (props) => controller.onUpdate(toRenderState(props)),
    onExit: () => controller.onExit(),
    onKeyDown: ({ event }) => {
      if (event.isComposing || event.keyCode === 229 || editor.view.composing) {
        return false
      }
      return controller.onKeyDown(event)
    },
  }),
})
return [mentionSuggestion, compositionRematch]
```

Keep `mentionMatchOptions`, `compositionSequence`, and both plugin constructions inside one `addProseMirrorPlugins` invocation so the sequence is editor-instance-local. Any metadata-only transaction re-runs the suggestion plugin's existing `apply` matcher, so no private Tiptap command or synthetic text edit is needed.

Guard suggestion `onKeyDown`, RichComposer key routing, and the popup's RichComposer-owned candidate-selection callback when `event.isComposing`, `event.keyCode === 229`, or `view.composing`; the pointer callback has no keyboard event, so it checks `editor.view.composing` directly before calling `confirmCandidate`. Return control to IME without moving selection, validating, inserting, or submitting.

Add a numeric input with `min={10}`, `max={500}`, and integer clamp on blur/save. Saving calls the store action; the returned full document updates both controls only through revision gating. A limit event immediately changes displayed cache truncation and the controller behavior from Task 8. Add `invalidPattern`, `sourceError`, and `profileError` to the `MentionUiLabels` objects in both `MessageInput` and `AutomationEditor`, using their existing `Folder.chat.messageInput` translators.

Merge these exact values into the named namespaces in every catalog; preserve `{message}` for interpolation:

```json
{
  "en": {
    "referenceSearchLimit": "Reference result limit",
    "referenceSearchLimitHint": "Maximum cached and searched results per resource source (10-500).",
    "referenceSearchLimitSave": "Save reference limit",
    "referenceSearchLimitSaveFailed": "Failed to save reference limit: {message}",
    "mentionInvalidPattern": "Invalid regular expression",
    "mentionSourceError": "This source could not be refreshed",
    "mentionProfileError": "Delegation profiles could not be loaded"
  },
  "ar": {
    "referenceSearchLimit": "حد نتائج المراجع",
    "referenceSearchLimitHint": "الحد الأقصى للنتائج المخزنة مؤقتًا والتي يجري البحث عنها لكل مصدر موارد (10-500).",
    "referenceSearchLimitSave": "حفظ حد المراجع",
    "referenceSearchLimitSaveFailed": "تعذر حفظ حد نتائج المراجع: {message}",
    "mentionInvalidPattern": "تعبير نمطي غير صالح",
    "mentionSourceError": "تعذر تحديث هذا المصدر",
    "mentionProfileError": "تعذر تحميل ملفات تعريف التفويض"
  },
  "de": {
    "referenceSearchLimit": "Limit für Referenzergebnisse",
    "referenceSearchLimitHint": "Maximale Anzahl zwischengespeicherter und durchsuchter Ergebnisse pro Ressourcenquelle (10-500).",
    "referenceSearchLimitSave": "Referenzlimit speichern",
    "referenceSearchLimitSaveFailed": "Referenzlimit konnte nicht gespeichert werden: {message}",
    "mentionInvalidPattern": "Ungültiger regulärer Ausdruck",
    "mentionSourceError": "Diese Quelle konnte nicht aktualisiert werden",
    "mentionProfileError": "Delegierungsprofile konnten nicht geladen werden"
  },
  "es": {
    "referenceSearchLimit": "Límite de resultados de referencia",
    "referenceSearchLimitHint": "Máximo de resultados en caché y buscados por cada fuente de recursos (10-500).",
    "referenceSearchLimitSave": "Guardar límite de referencias",
    "referenceSearchLimitSaveFailed": "No se pudo guardar el límite de resultados de referencia: {message}",
    "mentionInvalidPattern": "Expresión regular no válida",
    "mentionSourceError": "No se pudo actualizar esta fuente",
    "mentionProfileError": "No se pudieron cargar los perfiles de delegación"
  },
  "fr": {
    "referenceSearchLimit": "Limite de résultats de référence",
    "referenceSearchLimitHint": "Nombre maximal de résultats mis en cache et recherchés par source de ressources (10-500).",
    "referenceSearchLimitSave": "Enregistrer la limite de références",
    "referenceSearchLimitSaveFailed": "Impossible d'enregistrer la limite de résultats de référence : {message}",
    "mentionInvalidPattern": "Expression régulière non valide",
    "mentionSourceError": "Impossible d'actualiser cette source",
    "mentionProfileError": "Impossible de charger les profils de délégation"
  },
  "ja": {
    "referenceSearchLimit": "参照結果の上限",
    "referenceSearchLimitHint": "リソースソースごとにキャッシュおよび検索する結果の最大数（10～500）。",
    "referenceSearchLimitSave": "参照上限を保存",
    "referenceSearchLimitSaveFailed": "参照結果の上限を保存できませんでした: {message}",
    "mentionInvalidPattern": "正規表現が無効です",
    "mentionSourceError": "このソースを更新できませんでした",
    "mentionProfileError": "委任プロファイルを読み込めませんでした"
  },
  "ko": {
    "referenceSearchLimit": "참조 결과 한도",
    "referenceSearchLimitHint": "리소스 소스별로 캐시하고 검색할 최대 결과 수(10~500).",
    "referenceSearchLimitSave": "참조 한도 저장",
    "referenceSearchLimitSaveFailed": "참조 결과 한도를 저장하지 못했습니다: {message}",
    "mentionInvalidPattern": "잘못된 정규식",
    "mentionSourceError": "이 소스를 새로 고치지 못했습니다",
    "mentionProfileError": "위임 프로필을 불러오지 못했습니다"
  },
  "pt": {
    "referenceSearchLimit": "Limite de resultados de referência",
    "referenceSearchLimitHint": "Máximo de resultados armazenados em cache e pesquisados por fonte de recursos (10-500).",
    "referenceSearchLimitSave": "Salvar limite de referências",
    "referenceSearchLimitSaveFailed": "Falha ao salvar o limite de resultados de referência: {message}",
    "mentionInvalidPattern": "Expressão regular inválida",
    "mentionSourceError": "Não foi possível atualizar esta fonte",
    "mentionProfileError": "Não foi possível carregar os perfis de delegação"
  },
  "zh-CN": {
    "referenceSearchLimit": "引用结果上限",
    "referenceSearchLimitHint": "每个资源来源最多缓存和搜索的结果数（10-500）。",
    "referenceSearchLimitSave": "保存引用上限",
    "referenceSearchLimitSaveFailed": "保存引用结果上限失败：{message}",
    "mentionInvalidPattern": "正则表达式无效",
    "mentionSourceError": "无法刷新此来源",
    "mentionProfileError": "无法加载委托配置"
  },
  "zh-TW": {
    "referenceSearchLimit": "參照結果上限",
    "referenceSearchLimitHint": "每個資源來源最多快取與搜尋的結果數（10-500）。",
    "referenceSearchLimitSave": "儲存參照上限",
    "referenceSearchLimitSaveFailed": "儲存參照結果上限失敗：{message}",
    "mentionInvalidPattern": "規則運算式無效",
    "mentionSourceError": "無法重新整理此來源",
    "mentionProfileError": "無法載入委派設定檔"
  }
}
```

For each locale object above, place the four `referenceSearchLimit*` keys under `GeneralSettings` and the three `mention*` keys under the existing `Folder.chat.messageInput` object used by both `MessageInput` and `AutomationEditor`; the locale names are routing labels for this plan and are not written into the message files.

- [ ] **Step 4: Run full frontend and Rust verification**

Run:

```powershell
pnpm eslint .
pnpm test
pnpm build
cd src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings
cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

Expected: every command exits 0; bare `@` is network-free, resource pages arrive independently in fives, caps/concurrency/cancellation are backend-enforced, cache and selection remain stable, regex errors are isolated, and composition confirmation never inserts or submits.

- [ ] **Step 5: Commit IME, settings, localization, and final integration**

```powershell
git add src/components/chat/composer/suggestion/mention-suggestion.ts src/components/chat/composer/suggestion/mention-suggestion.test.ts src/components/chat/composer/rich-composer.tsx src/components/chat/composer/rich-composer-mention.test.tsx src/components/settings/conversation-experience-settings.tsx src/components/settings/conversation-experience-settings.test.tsx src/components/chat/message-input.tsx src/components/automations/automation-editor.tsx src/i18n/messages/ar.json src/i18n/messages/de.json src/i18n/messages/en.json src/i18n/messages/es.json src/i18n/messages/fr.json src/i18n/messages/ja.json src/i18n/messages/ko.json src/i18n/messages/pt.json src/i18n/messages/zh-CN.json src/i18n/messages/zh-TW.json
git commit -m "feat(references): finish incremental mention search"
```
