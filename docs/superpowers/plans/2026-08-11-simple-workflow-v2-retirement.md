# Simple Workflow and V2 Retirement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> `superpowers:executing-plans` to implement this plan task-by-task. Use
> `superpowers:test-driven-development` for every production behavior change,
> `superpowers:writing-skills` before editing the Skill, and
> `superpowers:verification-before-completion` before delivery.

**Goal:** Make Plan/progress-driven Simple the only writable
brainstorm-to-delivery workflow, preserve persisted manifest workflows as
read-only history, and provide an idempotent path from an archived workflow to
one new Simple successor conversation.

**Architecture:** A locator-only `simple_workflows` row freezes Simple identity
and points at bounded workspace-relative Plan and progress files. The backend
projects Simple display state from parsed Plan tasks, the progress JSON block,
and durable delegation runs. Persisted manifest identity takes precedence and
is fenced before prompt, run, budget, or semantic workflow side effects. The
frontend renders either the Simple projection or an archived manifest graph;
only archived graphs offer the explicit `Continue in Simple` command.

**Tech Stack:** Rust 2021, SeaORM/SQLite, Axum/Tauri, pulldown-cmark, MCP stdio,
React 19, TypeScript strict, Next.js static export, Zustand, Vitest,
next-intl.

**Design:**
`docs/superpowers/specs/2026-08-11-simple-workflow-v2-retirement-design.md`

## Global Constraints

- Simple is the only production-writable brainstorm-to-delivery mode.
- Do not delete or rewrite historical workflow, gate, completion, report, or
  run data in Phase 1.
- A workflow header and Simple descriptor are mutually exclusive. Resolve a
  conflict as corrupt identity and fail closed.
- Once a root acquires a Simple descriptor, recognized Simple A1 history, or a
  persisted workflow header, its mode is frozen; a mode change requires a new
  root conversation.
- Persisted workflow identity always wins over observed A1 history.
- Reject archived mutation before prompt enqueue, transcript append, child
  creation, budget consumption, run reservation, attention creation, or
  semantic workflow transaction.
- Keep generic no-manifest delegation, continuation, replacement, budget, and
  lineage safeguards working.
- Plan/progress parsing and reconciliation failures are display warnings, not
  delegation admission failures.
- All paths are normalized workspace-relative paths. Reuse the existing path
  resolver and bounded UTF-8 readers; do not add string-prefix containment
  checks.
- Desktop and server transports expose the same commands and error codes.
- Preserve explicit conversation deletion. Soft deletion must remove the
  Simple descriptor/link even though SQLite cascade only covers hard delete.
- Production manifest publication is retired, while historical fixture setup
  retains an explicit test-only path so read-model tests remain meaningful.
- Do not stage `.codex-tmp-*`, `.task-runtimes/`, or unrelated worktree files.
- Follow strict RED/GREEN: add one behavior test, run it and observe the
  expected failure, then add the minimum implementation and rerun it.

## Stable Contracts

### Descriptor

```text
simple_workflows
  parent_conversation_id INTEGER PRIMARY KEY
  plan_rel_path          TEXT NOT NULL
  progress_rel_path      TEXT NOT NULL
  source_workflow_id     TEXT NULL UNIQUE
  created_at             TIMESTAMP NOT NULL
  updated_at             TIMESTAMP NOT NULL
```

The parent foreign key references the existing `conversation` table with
`ON DELETE CASCADE`. The optional source foreign key references
`delegation_workflows.workflow_id` with `ON DELETE SET NULL`.

### Stable retirement error

```json
{
  "code": "workflow_v2_retired",
  "message": "This workflow is archived and read-only. Continue in a Simple successor.",
  "successor_conversation_id": null,
  "can_create_simple_successor": true
}
```

The two navigation fields are populated from durable state when the caller is
bound to an archived workflow. Production manifest creation returns the same
stable code even when no workflow exists yet.

### Simple progress marker

```text
<!-- codeg-simple-progress-v1
{"schema_version":1,"tasks":[],"final_review_status":"pending"}
-->
```

The full progress file is limited to 512 KiB, the marked block to 64 KiB, the
Plan to 2 MiB, and one parsed Plan section to 512 KiB.

---

### Task 1: Add durable Simple identity and locator storage

**Files:**

- Create: `src-tauri/src/db/migration/m20260811_000001_simple_workflows.rs`
- Modify: `src-tauri/src/db/migration/mod.rs`
- Create: `src-tauri/src/db/entities/simple_workflow.rs`
- Modify: `src-tauri/src/db/entities/mod.rs`
- Modify: `src-tauri/src/db/entities/prelude.rs`
- Create: `src-tauri/src/acp/delegation/workflow/simple.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/commands/conversations.rs`
- Test: co-located migration, Simple store, and conversation deletion tests

**Interfaces:**

- Produce `SimpleWorkflowMode::{Ordinary, SimpleRegistered,
  SimpleObserved, Archived, Corrupt}` or an equivalent typed resolver.
- Produce idempotent locator registration/load/update helpers that accept a
  database connection and parent conversation identity.
- Registration derives the default progress path as
  `.superpowers/sdd/<parent-conversation-id>/progress.md` and never stores an
  absolute path.

- [ ] **Step 1: Add failing migration and entity relation tests**

Cover schema creation, parent cascade, source `SET NULL`, and unique
`source_workflow_id`. Assert behavior through inserts/deletes rather than SQL
source text.

- [ ] **Step 2: Run the migration tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_workflow_migration -- --nocapture
```

Expected: compilation/module failure because the migration and entity do not
exist.

- [ ] **Step 3: Implement the migration and SeaORM entity**

Register the migration last, use the real `conversation` table identifier,
create both foreign keys and the unique source index, and register entity
exports/prelude aliases.

- [ ] **Step 4: Run the migration tests and verify GREEN**

Run the Step 2 command. Expected: PASS.

- [ ] **Step 5: Add failing descriptor and mode-resolution tests**

Cover normalized Plan/progress registration, idempotent replay, locator update,
default progress path, archived conflict, corrupt header-plus-descriptor
identity, observed A1 fallback, and archived precedence over observed history.
Also prove soft conversation deletion removes the descriptor and allows a
future successor link for the same source.

- [ ] **Step 6: Run the Simple store tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_workflow_store -- --nocapture
```

Expected: failure because registration and mode resolution are absent.

- [ ] **Step 7: Implement descriptor storage and frozen mode resolution**

Reuse `workflow/key.rs::normalize_rel_path`. Resolve owning roots and bound
children through existing workflow/run-binding lookups. Keep descriptor data
locator-only. Add explicit descriptor cleanup to the soft-delete transaction.

- [ ] **Step 8: Run Task 1 regression filters**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_workflow -- --nocapture
cargo test --lib --features test-utils delete_conversation -- --nocapture
```

Expected: all filtered tests pass.

- [ ] **Step 9: Commit Task 1**

```powershell
git add -- src-tauri/src/db/migration src-tauri/src/db/entities src-tauri/src/acp/delegation/workflow src-tauri/src/commands/conversations.rs
git commit -m "feat(workflow): add durable Simple workflow identity"
```

---

### Task 2: Parse bounded Plan tasks and Simple progress

**Files:**

- Create: `src-tauri/src/acp/delegation/workflow/simple_parse.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/mod.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/plan_material.rs`
- Test: `src-tauri/src/acp/delegation/workflow/simple_parse.rs`

**Interfaces:**

- Produce parsed Plan tasks with index, title, bounded body, declared file
  touchpoints, and verification text.
- Produce a progress snapshot with schema version, active task, declared task
  state, run references, final-review state, timestamp, and bounded warning
  codes.
- Parsing returns the largest safe partial model plus warnings for recoverable
  document problems; path escape, file-size, and invalid UTF-8 errors remain
  explicit bounded-read failures.

- [ ] **Step 1: Add failing Plan parser table tests**

Use literal Markdown fixtures for H2/H3 `Task <positive integer>: <title>`,
fenced-code lookalikes, duplicate indices, gaps, malformed indices, section
limits, and extraction of files/verification text. Each expected task/warning
is hand-authored.

- [ ] **Step 2: Run Plan parser tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_parse::tests::plan -- --nocapture
```

Expected: compilation failure because the parser is absent.

- [ ] **Step 3: Implement Plan parsing on pulldown-cmark events**

Extract reusable bounded material handling from `plan_material.rs`. Do not scan
headings with line regexes and do not recognize headings inside code blocks.

- [ ] **Step 4: Run Plan parser tests and verify GREEN**

Run Step 2 again. Expected: PASS.

- [ ] **Step 5: Add failing progress parser table tests**

Cover one valid block, missing file, missing marker, two blocks, truncated
marker, oversized file/block, invalid JSON/schema, duplicate task indices,
unknown task/run states, stale `plan_rel_path`, missing commits, and Markdown
notes outside the marker. Unknown values must warn and must never become
completed.

- [ ] **Step 6: Run progress parser tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_parse::tests::progress -- --nocapture
```

Expected: failing assertions for the missing progress parser.

- [ ] **Step 7: Implement the single-block bounded JSON parser**

Use serde models with explicit version/state validation. Cap emitted warning
codes and never include arbitrary file content in a warning or transport
error.

- [ ] **Step 8: Run Task 2 regression filters**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_parse -- --nocapture
cargo test --lib --features test-utils plan_material -- --nocapture
```

Expected: all filtered tests pass.

- [ ] **Step 9: Commit Task 2**

```powershell
git add -- src-tauri/src/acp/delegation/workflow/simple_parse.rs src-tauri/src/acp/delegation/workflow/plan_material.rs src-tauri/src/acp/delegation/workflow/mod.rs
git commit -m "feat(workflow): parse Simple plans and progress"
```

---

### Task 3: Project Simple graphs from documents and durable runs

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/dto.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/project.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/state_dto.rs`
- Modify: `src-tauri/src/commands/workflow_graph.rs`
- Modify: `src-tauri/src/models/conversation.rs`
- Modify: `src/lib/types.ts`
- Create: `src/lib/workflow-types.test.ts`
- Test: co-located Rust projector tests and TypeScript contract fixtures

**Interfaces:**

- Add `WorkflowCompatibility::Simple`.
- Add `Pending` overall state and `Pending`/`InProgress` node states without
  gate semantics.
- Add bounded `projection_warning_codes` to the snapshot and nodes.
- Add node `sync_state: in_sync | out_of_sync`.
- Add Simple locator/navigation data and archived successor navigation data to
  the graph DTO without exposing absolute workspace paths.

- [ ] **Step 1: Add failing DTO serialization tests**

Assert literal JSON shapes for one Simple snapshot and one archived snapshot,
including new enum wire values, warning caps, sync state, absent manifest-only
fields for Simple, and source/successor navigation.

- [ ] **Step 2: Run DTO tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils workflow::dto::tests -- --nocapture
```

Expected: compilation failure for missing Simple variants/fields.

- [ ] **Step 3: Extend Rust and TypeScript transport mirrors**

Update every Rust snapshot struct literal deliberately. Use defaults only for
backward-compatible deserialization, not to conceal missing production data.

- [ ] **Step 4: Add failing Simple projection tests**

Cover Plan-only pending state, declared in-progress/completed/blocked states,
final-review completion, active durable run enrichment, failed-run versus
declared-completed mismatch, missing commit, progress-only task, task-ID
mismatch, stale Plan path, malformed/missing files, header precedence, corrupt
identity, and observed-only fallback when no descriptor exists.

- [ ] **Step 5: Run Simple projection tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_projection -- --nocapture
```

Expected: descriptor-backed conversations do not yet project as Simple.

- [ ] **Step 6: Implement deterministic Simple projection**

Order `project_inner` as: load workflow header and descriptor, fail closed on
both, project archived manifest when a header exists, project Simple when a
descriptor exists, otherwise retain observed-only behavior. Join durable runs
by validated task/run identity. A mismatch changes only warnings/sync state,
never delegation authorization.

- [ ] **Step 7: Run Task 3 regressions**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_projection -- --nocapture
cargo test --lib --features test-utils workflow::project::tests -- --nocapture
Set-Location ..
pnpm test -- src/lib/workflow-types.test.ts
```

Expected: Simple and historical projections pass; existing observed-only
snapshots remain readable.

- [ ] **Step 8: Commit Task 3**

```powershell
git add -- src-tauri/src/acp/delegation/workflow src-tauri/src/commands/workflow_graph.rs src-tauri/src/models/conversation.rs src/lib/types.ts src/lib/workflow-types.test.ts
git commit -m "feat(workflow): project Simple workflow graphs"
```

---

### Task 4: Register Simple locators through MCP and refresh events

**Files:**

- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/transport.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/simple.rs`
- Modify: `src-tauri/src/web/event_bridge.rs` or the existing workflow event helper
- Modify: `src/lib/api.ts`
- Modify: `src/lib/workflow-graph-store.ts`
- Test: schema, companion/listener, event, API, and store tests

**Interfaces:**

- Add Root-only MCP tool `register_simple_workflow` with
  `plan_rel_path` and optional `progress_rel_path`.
- Derive the parent conversation from the authenticated launch token/database
  binding; the model cannot supply a conversation ID.
- Return normalized paths, default progress path, registration mode, and an
  idempotent result.
- Emit a conversation-scoped graph refresh event after descriptor creation or
  locator update.

- [ ] **Step 1: Add failing catalog and authorization tests**

Prove the Root catalog exposes registration, child catalogs do not, the schema
has no parent ID input, and a stale/cross-conversation token cannot register a
descriptor.

- [ ] **Step 2: Run catalog tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --no-default-features --bin codeg-mcp register_simple_workflow -- --nocapture
cargo test --lib --features test-utils register_simple_workflow -- --nocapture
```

Expected: tool lookup/dispatch failure because it is not registered.

- [ ] **Step 3: Implement schema, transport, listener, and store dispatch**

Keep the tool non-semantic: it only validates/updates locators. Reuse the
connection's durable conversation identity and descriptor transaction.

- [ ] **Step 4: Add failing idempotency and refresh tests**

Cover default path, normalized explicit path, replay with the same inputs,
locator update, conflict with archived identity, event emitted after commit,
and no event on rollback.

- [ ] **Step 5: Run listener/store tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils register_simple_workflow -- --nocapture
```

Expected: at least refresh/idempotency assertions fail before wiring is
complete.

- [ ] **Step 6: Complete registration and refresh invalidation**

Teach the frontend store to invalidate a conversation on the existing graph
event while preserving request-generation ordering and bounded fallback
refresh behavior.

- [ ] **Step 7: Run Task 4 regressions**

```powershell
Set-Location src-tauri
cargo test --no-default-features --bin codeg-mcp register_simple_workflow -- --nocapture
cargo test --lib --features test-utils register_simple_workflow -- --nocapture
Set-Location ..
pnpm test -- src/lib/api.test.ts src/lib/workflow-graph-store.test.ts
```

Expected: all targeted tests pass.

- [ ] **Step 8: Commit Task 4**

```powershell
git add -- src-tauri/src/acp/delegation src-tauri/src/web/event_bridge.rs src/lib/api.ts src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts
git commit -m "feat(workflow): register Simple workflow locators"
```

---

### Task 5: Retire manifest workflow writes and fence archived side effects

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/error.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/store.rs`
- Modify: `src-tauri/src/acp/delegation/workflow/admission.rs`
- Modify: `src-tauri/src/acp/delegation/run_store.rs`
- Modify: `src-tauri/src/acp/delegation/broker.rs`
- Modify: `src-tauri/src/acp/delegation/listener.rs`
- Modify: `src-tauri/src/acp/manager.rs`
- Modify: `src-tauri/src/acp/connection.rs`
- Modify: `src-tauri/src/acp/delegation/companion.rs`
- Modify: `src-tauri/src/acp/delegation/tool_schema.json`
- Test: focused guard, broker, manager, listener, and catalog tests

**Interfaces:**

- Add `WorkflowStoreError::WorkflowV2Retired` with stable code and navigation
  metadata.
- Change `require_v2_mutation(2, V2Enforce)` to reject production mutation.
- Keep an explicitly named `#[cfg(test)]` manifest fixture publication helper
  for historical read-model tests.
- Stop injecting workflow v2 Root tools and child `complete_work` tools into
  new production MCP catalogs.

- [ ] **Step 1: Add failing central guard matrix tests**

Cover v2 root and bound child prompt admission, delegate, continue,
replacement, settlement, recovery, completion submission/repair, final
delivery, and automatic root wake. For each early-admission path, assert zero
new transcript messages, conversations, budget rows, task runs, authorization
rows, attention rows, or semantic workflow revisions.

- [ ] **Step 2: Run the guard matrix and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
```

Expected: current v2 mutations succeed or reject after one or more side
effects.

- [ ] **Step 3: Implement the stable retirement error and shared durable guard**

Use workflow ownership/run binding to resolve archived mode and linked
successor metadata. Preserve existing v1 read-only codes where callers already
depend on them. Place the guard before all semantic transactions.

- [ ] **Step 4: Add failing prompt and broker ordering tests**

Exercise foreground, automation, chat-channel, root prompt, direct bound-child
prompt, `start_delegation`, and `continue_delegation`. Specifically prove the
broker preflight occurs before provisional child creation and budget/run
reservation. Preserve ordinary child first prompts and generic no-manifest A1
admission.

- [ ] **Step 5: Run ordering tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils archived_workflow_prompt -- --nocapture
cargo test --lib --features test-utils archived_workflow_delegation -- --nocapture
```

Expected: at least bound-child prompt or pre-provisional-child assertions fail.

- [ ] **Step 6: Fence manager and broker entry points**

Extend the manager fence beyond `delegation.is_none()` only when durable
workflow identity exists, so ordinary newly created children retain their
first prompt. Add durable archived preflight to start/continue and retain the
transaction guard as a race fence.

- [ ] **Step 7: Add failing manifest creation and MCP catalog tests**

Prove stale publication with and without a prior header returns
`workflow_v2_retired` and creates no row. Prove new Root catalogs contain only
generic delegation plus Simple registration, and archived children do not
receive completion v2 tools.

- [ ] **Step 8: Run creation/catalog tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils manifest_publication_is_retired -- --nocapture
cargo test --lib --features test-utils workflow_tool_catalog_is_retired -- --nocapture
```

Expected: production publication and v2 catalog exposure still succeed.

- [ ] **Step 9: Retire production publication and catalogs**

Reject listener/public transport manifest publication unconditionally. Route
historical test setup through the explicit fixture helper. Remove v2 mutation
tools from new catalogs while keeping stale direct-call server guards.

- [ ] **Step 10: Run Task 5 regressions**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
cargo test --lib --features test-utils archived_workflow -- --nocapture
cargo test --lib --features test-utils no_manifest -- --nocapture
cargo test --no-default-features --bin codeg-mcp tool_catalog -- --nocapture
```

Expected: archived paths are side-effect-free, historical read fixtures still
project, and ordinary/Simple delegation remains writable.

- [ ] **Step 11: Commit Task 5**

```powershell
git add -- src-tauri/src/acp
git commit -m "feat(workflow): retire manifest workflow mutations"
```

---

### Task 6: Create or reopen an idempotent Simple successor

**Files:**

- Create: `src-tauri/src/commands/simple_workflow.rs`
- Modify: `src-tauri/src/commands/mod.rs`
- Modify: `src-tauri/src/lib.rs`
- Create: `src-tauri/src/web/handlers/simple_workflow.rs`
- Modify: `src-tauri/src/web/handlers/mod.rs`
- Modify: `src-tauri/src/web/router.rs`
- Modify: `src/lib/api.ts`
- Modify: `src/lib/tauri.ts`
- Modify: `src/lib/types.ts`
- Test: command, Axum handler/router, and frontend transport tests

**Interfaces:**

- Add `continue_archived_workflow_in_simple(source_conversation_id,
  client_request_token)` for both desktop and server transports.
- Return `{ successor_conversation_id, created, plan_rel_path,
  progress_rel_path, bootstrap_prompt }` or an equivalent stable DTO.
- The bootstrap prompt contains Plan/design locations and a request to
  reconstruct repository-grounded progress; it contains no gates, task IDs,
  approvals, completion Cards, evidence counters, or recovery counters.

- [ ] **Step 1: Add failing core command tests**

Cover authorized archived root, archived bound child resolving to its root,
ordinary/Simple source rejection, missing/escaped/oversized/non-UTF-8 Plan,
same workspace/folder/agent/route inheritance, isolated progress path, and no
semantic v2 row changes.

- [ ] **Step 2: Run command tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_successor -- --nocapture
```

Expected: command/module missing.

- [ ] **Step 3: Implement transactionally idempotent successor creation**

Resolve the active persisted `plan_target_rel_path` from the source workflow,
bounded-read it inside the source workspace, check the unique source link,
create a new root through the shared conversation service, then insert its
descriptor in one transaction or rollback-safe sequence. Handle the unique
race by loading and returning the winner. Persist/reuse the request token
through the existing idempotency convention if one exists; do not trust
client-supplied workspace or agent fields.

- [ ] **Step 4: Add failing concurrency/deletion tests**

Run two distinct client requests concurrently and assert one successor ID, one
conversation, one descriptor, and `created` true for only one winner. Delete
the successor through the public cleanup path, then prove a later request may
create a new successor.

- [ ] **Step 5: Run concurrency tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_successor_concurrent -- --nocapture
```

Expected: duplicate creation or missing retry-after-delete behavior.

- [ ] **Step 6: Finish race convergence and cleanup behavior**

Use the unique source index as the durable arbiter. Ensure the archived graph
navigation reflects the winner immediately after commit.

- [ ] **Step 7: Add failing desktop/server transport parity tests**

Assert the Tauri command registration, Axum route, handler authorization, JSON
shape, stable error mapping, and frontend API call arguments.

- [ ] **Step 8: Run transport tests and verify RED**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_successor_transport -- --nocapture
Set-Location ..
pnpm test -- src/lib/api.test.ts
```

Expected: route/invoke call missing.

- [ ] **Step 9: Wire desktop, server, and TypeScript clients**

Keep static-export constraints: use the existing command/fetch transport, not
a dynamic Next.js route.

- [ ] **Step 10: Run Task 6 regressions**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_successor -- --nocapture
cargo test --no-default-features --features server --bin codeg-server --lib simple_successor -- --nocapture
Set-Location ..
pnpm test -- src/lib/api.test.ts
```

Expected: core and both runtime surfaces pass.

- [ ] **Step 11: Commit Task 6**

```powershell
git add -- src-tauri/src/commands src-tauri/src/web src-tauri/src/lib.rs src/lib/api.ts src/lib/tauri.ts src/lib/types.ts
git commit -m "feat(workflow): continue archived work in Simple"
```

---

### Task 7: Render archived and Simple workflows and refresh exact files

**Files:**

- Modify: `src/components/chat/sub-agent-overlay.tsx`
- Modify: `src/components/chat/workflow-graph-panel.tsx`
- Modify: `src/components/chat/workflow-phase-rail.tsx`
- Modify: `src/components/chat/workflow-status-icon.tsx`
- Modify: `src/components/message/message-list-view.tsx`
- Modify: `src/lib/workflow-graph-store.ts`
- Modify: `src/hooks/use-workspace-state-store.ts` only if a reusable selector is required
- Modify: `src/i18n/messages/{en,zh-CN,zh-TW,ja,ko,es,de,fr,pt,ar}.json`
- Test: `src/components/chat/workflow-overlay.test.tsx`
- Test: `src/components/chat/sub-agent-overlay.test.tsx`
- Test: `src/lib/workflow-graph-store.test.ts`

**Interfaces:**

- Archived manifests render a clear read-only banner, retain historical graph,
  report, Card, gate, and child navigation, hide mutation affordances, and
  expose `Continue in Simple` when permitted.
- Simple renders Plan tasks, declared status, live run activity, sync warnings,
  Plan/progress navigation, and no gate/settlement/completion-evidence wording.
- Workspace `changed_paths` invalidates only when the exact normalized Plan or
  progress path for that conversation changed, with debounce and request
  generation protection.

- [ ] **Step 1: Add failing archived overlay interaction tests**

Render a full realistic archived snapshot. Assert read-only semantics,
historical navigation remains usable, mutation actions are absent, successor
creation is called once with a stable per-click request token, double click is
deduplicated, an existing successor opens instead of creating, and errors keep
the source overlay intact.

- [ ] **Step 2: Run archived UI tests and verify RED**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/chat/sub-agent-overlay.test.tsx
```

Expected: read-only banner and successor action are missing.

- [ ] **Step 3: Implement archived read-only UI and successor navigation**

Use the existing icon library and compact operational styling. Do not nest
cards or add explanatory feature copy. Disable the action while pending and
open the returned conversation through the existing navigation path.

- [ ] **Step 4: Add failing Simple projection UI tests**

Cover pending/in-progress/completed/blocked Tasks, live child activity,
out-of-sync warnings, partial projection, Plan/progress links, no gate labels,
and long translated labels at narrow width.

- [ ] **Step 5: Run Simple UI tests and verify RED**

Run the Step 2 command. Expected: Simple snapshots fall through manifest UI or
show gate language.

- [ ] **Step 6: Implement compatibility-specific Simple rendering**

Reuse unframed graph/list surfaces where useful, but branch semantics by
`compatibility`. Keep task row dimensions stable and warning text bounded.

- [ ] **Step 7: Add failing exact-path refresh tests**

Feed workspace envelopes for unrelated paths, suffix/prefix lookalikes, exact
Plan path, exact progress path, bursts, folder changes, and a stale request
resolving after a newer request. Assert only exact relevant paths debounce to
one refresh and old results cannot overwrite the new projection.

- [ ] **Step 8: Run store refresh tests and verify RED**

```powershell
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Expected: exact workspace file changes do not yet invalidate the graph.

- [ ] **Step 9: Wire the path-only subscription and debounce**

Pass the conversation folder plus locator paths to the overlay/store. Reuse
`subscribeEnvelopes` with the low-cost `paths` subscription. Normalize path
separators/case according to the workspace platform before exact comparison.

- [ ] **Step 10: Add all ten locale messages and validate key parity**

Add concise Simple, read-only, warning, and successor strings to every locale.
Keep existing manifest historical labels intact.

- [ ] **Step 11: Run Task 7 regressions**

```powershell
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/components/chat/sub-agent-overlay.test.tsx src/lib/workflow-graph-store.test.ts
pnpm eslint -- src/components/chat/sub-agent-overlay.tsx src/components/chat/workflow-graph-panel.tsx src/lib/workflow-graph-store.ts
```

Expected: UI, refresh, and lint filters pass.

- [ ] **Step 12: Commit Task 7**

```powershell
git add -- src/components/chat src/components/message/message-list-view.tsx src/lib/workflow-graph-store.ts src/lib/workflow-graph-store.test.ts src/hooks/use-workspace-state-store.ts src/i18n/messages
git commit -m "feat(workflow): render Simple and archived workflows"
```

---

### Task 8: Rewrite brainstorm-to-delivery and close cross-runtime regressions

**Files:**

- Modify: `.agents/skills/brainstorm-to-delivery/SKILL.md`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`
- Modify: `.agents/skills/brainstorm-to-delivery/agents/openai.yaml` if its prompt text is v2-specific
- Test: Skill pressure scenarios plus the contract validator suite
- Test: focused backend/frontend cross-runtime suites

**Interfaces:**

- Base the new Skill on the behavior at commit
  `99ddba923112cf82f9bde1dd5b8455a691133c0d`.
- Preserve later generic tool discovery, continuation, replacement, workspace
  safety, report recovery, and post-compaction disk reinspection.
- Remove capability, manifest, settlement, workflow-recovery,
  completion-evidence/Card, and v2 gate dependencies.
- Require writing-plans, Simple registration after the Plan exists, serial
  Task execution, a structured progress block, generic delegation/continuation,
  parent adjudication, repository evidence, and final review.

- [ ] **Step 1: Read the Skill testing methodology and build pressure fixtures**

Read `testing-skills-with-subagents.md` from the installed `writing-skills`
package. Define at least three scenarios combining time pressure, a stale
progress ledger, an interrupted/compacted context, and a v2 tool still visible
from a stale client. The desired behavior is to use Simple documents and
generic run safety without publishing/repairing v2 state.

- [ ] **Step 2: Run RED baselines without the proposed Skill**

Run fresh-context samples against the historical baseline/no-guidance control.
Record concrete violations or wrong output shape in a temporary test artifact
outside the Skill. Also add validator tests that execute controlled Plan and
progress fixtures and currently fail because the validator requires v2
manifest/gate/Card contracts.

```powershell
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: new Simple contract cases fail against the current validator.

- [ ] **Step 3: Rewrite the Skill and validator minimally**

Use the old revision as a reference, not a wholesale checkout. Express the
workflow as a positive ordered contract. Keep enforcement that matters to
generic delegation identity and replacement. Make the validator parse/validate
the structured Simple progress block and reject v2-only output requirements;
do not test the Skill by grepping exact prose.

- [ ] **Step 4: Run GREEN Skill contract tests**

```powershell
node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs
```

Expected: all validator behavior tests pass.

- [ ] **Step 5: Pressure-test the rewritten Skill**

Run the same fresh-context scenarios with the full rewritten Skill. Confirm
agents register Simple after Plan creation, update progress around state
changes, dispatch serially, use continuation correctly, recover from disk, and
do not call v2 tools. Close only observed loopholes and rerun until behavior is
consistent.

- [ ] **Step 6: Run focused backend/frontend retirement regressions**

```powershell
Set-Location src-tauri
cargo test --lib --features test-utils simple_workflow -- --nocapture
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
cargo test --lib --features test-utils simple_successor -- --nocapture
cargo check --no-default-features --features server --bin codeg-server
cargo check --no-default-features --bin codeg-mcp
Set-Location ..
pnpm test -- src/components/chat/workflow-overlay.test.tsx src/lib/workflow-graph-store.test.ts src/lib/api.test.ts
```

Expected: all focused tests and both opt-in runtime checks pass.

- [ ] **Step 7: Run repository-level static validation**

```powershell
pnpm eslint .
pnpm build
Set-Location src-tauri
cargo check
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

If machine memory prevents a desktop Rust check, run the documented
low-memory shared/server/MCP checks and record the exact omitted command and
reason. Do not report an unrun suite as passing.

- [ ] **Step 8: Review spec coverage and forbidden placeholders**

Compare implementation against every design success criterion and invariant.
Search changed production files for `TODO`, `FIXME`, `unimplemented!`, stale
v2 Skill tool names, and duplicated Rust/TypeScript wire values. Inspect `git
diff --check`, the full staged diff, and ensure only task files are staged.

- [ ] **Step 9: Request code review and address findings**

Use `superpowers:requesting-code-review`. Reviewers must prioritize side-effect
ordering, archived read preservation, successor race convergence, path
containment, bounded parsing, desktop/server parity, and missing tests. Apply
accepted findings through RED/GREEN and rerun covering checks.

- [ ] **Step 10: Commit Task 8**

```powershell
git add -- .agents/skills/brainstorm-to-delivery
git commit -m "docs(skill): switch brainstorm delivery to Simple"
```

- [ ] **Step 11: Verify final branch state**

```powershell
git status --short --branch
git log --oneline cab3efbe..HEAD
git diff --check cab3efbe..HEAD
```

Expected: only the known unrelated untracked files remain; the plan and each
implementation task have reviewable commits.

## Acceptance Matrix

| Scenario | Required result |
| --- | --- |
| New brainstorm-to-delivery root | No manifest; Simple descriptor after Plan |
| Registered Simple root | Plan/progress/run projection; delegation writable |
| Recognized A1 without descriptor | Generic compatibility path remains writable |
| Archived root prompt | `workflow_v2_retired`, zero semantic side effects |
| Archived bound-child prompt | Same stable rejection before transcript append |
| Archived delegate/continue/replace | Same stable rejection before child/budget/run |
| Stale manifest publish with no header | Stable rejection; no workflow row |
| Historical graph read | Existing graph, reports, gates, Cards remain visible |
| Continue in Simple replay/race | Exactly one live successor is returned |
| Deleted successor | Source link clears; a later successor may be created |
| Malformed Plan/progress | Partial/empty projection plus bounded warning |
| Ledger/run disagreement | `out_of_sync`; no admission failure |
| Plan/progress exact file change | Debounced fresh projection |
| Unrelated workspace file change | No Simple projection request |
| Desktop/server/MCP | Same contracts and retirement behavior |
