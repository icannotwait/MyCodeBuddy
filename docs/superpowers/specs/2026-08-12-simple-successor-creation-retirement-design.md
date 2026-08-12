# Simple Successor Creation Retirement Design

**Date:** 2026-08-12

**Status:** Approved design, pending implementation plan

## Summary

Codeg will no longer create, reopen, link, or bootstrap a Simple conversation
from an archived manifest workflow. An archived conversation remains read-only
history. To continue the underlying work, the user creates an ordinary new
conversation through the existing new-conversation flow and starts from a new
Design and Plan.

The existing `continue_archived_workflow_in_simple` Tauri command and server
route remain registered as stable rejection surfaces for stale clients. Every
domain-valid invocation returns one permanent error and performs no workflow,
database, filesystem, conversation, prompt, or event work.

This change removes the successor feature rather than repairing its prompt
admission, replay, or locator-update behavior.

## Motivation

Automatic successor creation attempted to bridge two incompatible trust
domains:

- an archived manifest workflow with persisted task, gate, evidence, and
  completion semantics; and
- a new Plan/progress-driven Simple workflow whose state must be reconstructed
  from current repository evidence.

That bridge requires durable source identity, idempotent conversation creation,
bootstrap prompt admission, replay behavior, locator synchronization, and
cross-transport UI navigation. It also creates an ambiguous promise: the
product appears able to continue old workflow state even though Simple must not
trust that state.

The product does not need this bridge. A normal new conversation already gives
the user the correct isolation boundary. Requiring a new Design and Plan makes
the new conversation's intent and authority explicit and removes the risk of
silently importing stale archived assumptions.

## User Experience

An archived workflow continues to show its historical graph, transcript,
reports, Cards, gates, and child navigation. It remains read-only.

The archived UI has no **Continue in Simple** button, menu item, loading state,
error state, or replacement shortcut. Codeg does not create a conversation,
open a conversation, copy a Plan path, or send a bootstrap prompt on the
user's behalf.

To continue related work, the user:

1. uses the existing general new-conversation action;
2. selects the desired workspace and agent normally;
3. creates a new Design for the current repository state;
4. creates a new Plan from that Design; and
5. runs the ordinary Simple workflow.

The new conversation has no durable or inferred link to the archived workflow.
The user may inspect the archive manually, but Codeg does not present archived
state as authority in the new conversation.

## Scope

### In scope

- Retain the old Tauri command name as a stable rejection entry point.
- Retain the old authenticated server route as a stable rejection entry point.
- Add the exact permanent retirement error contract defined below.
- Remove all production successor creation, lookup, replay, retry, concurrency,
  deletion-recreation, and bootstrap admission logic.
- Remove the archived successor UI and all frontend request helpers for it.
- Remove the successor source link from the Simple descriptor schema, entity,
  registration API, and queries.
- Remove successor source navigation from the Simple projection DTO and
  frontend type.
- Remove the bootstrap table, entity, migration registration, and connection
  admission hooks.
- Preserve archived response fields that existing readers may deserialize, but
  project them as constants indicating that no successor exists or can be
  created.
- Preserve ordinary Simple registration, locator updates, projection,
  delegation, review, recovery, and execution.

### Out of scope

- Deleting archived manifest workflow history or its read projections.
- Making archived conversations writable again.
- Copying an archived Design, Plan, progress ledger, transcript, task ID,
  approval, gate, Card, evidence, or completion result into a new conversation.
- Adding a different archive-to-new-conversation shortcut.
- Automatically creating a new conversation when a workflow is archived.
- Preserving already generated successor rows in development databases.
- Adding a forward or data-preserving database migration for this unpublished
  feature.
- Changing unrelated legacy identity such as
  `delegation_workflows.legacy_source_workflow_id`.
- Redesigning ordinary Simple projection. Existing requirements such as marking
  a stale progress Plan path `out_of_sync` remain in force and are not
  superseded by this document.

## Relationship To The Existing Design And Plan

This document supersedes only the automatic successor portions of:

- `docs/superpowers/specs/2026-08-11-simple-workflow-v2-retirement-design.md`
- `docs/superpowers/plans/2026-08-11-simple-workflow-v2-retirement.md`

Specifically, it replaces requirements concerning:

- the Simple successor definition and archived source identity;
- `simple_workflows.source_workflow_id`;
- source-to-successor discovery and navigation;
- the **Continue in Simple** action;
- successor creation, replay, concurrency, and recreation after deletion;
- successor bootstrap construction, persistence, and prompt admission;
- successor-specific error and eligibility handling; and
- successor-specific verification and success criteria.

All other requirements in the earlier design remain authoritative, including:

- Simple as the only writable brainstorm-to-delivery workflow mode;
- archived manifest workflows as read-only history;
- the archived mutation fence;
- ordinary Simple descriptor registration and locator updates;
- Plan/progress projection and reconciliation;
- generic delegation lineage and recovery rules; and
- independent review and verification requirements.

The old implementation plan is not edited in place. The implementation plan
for this design will identify the obsolete successor tasks and the removal work
that replaces them.

## Architecture

After this change, archived and Simple workflows have no cross-mode creation or
identity edge:

```text
Archived manifest conversation
  -> read-only history projection
  -> no successor action
  -> no Simple link

Existing new-conversation flow
  -> ordinary root conversation
  -> new Design and Plan
  -> ordinary Simple registration
  -> Plan/progress-driven execution
```

The only remaining artifact named after the old transition is its compatibility
API surface. That surface terminates immediately in a pure retirement error.
It is not a navigation service and has no access to application state.

### Component boundaries

1. **Retired command core**

   A small shared constructor returns the exact
   `simple_successor_creation_retired` application error. It accepts no
   database, emitter, filesystem path, conversation service, or connection
   manager dependency.

2. **Tauri compatibility command**

   The command remains registered under
   `continue_archived_workflow_in_simple`. It declares no operation-specific
   parameters; extra arguments sent by stale callers are ignored by the Tauri
   command wrapper. The command immediately returns the shared error.

3. **Server compatibility route**

   The authenticated POST route remains registered at
   `/api/continue_archived_workflow_in_simple`. After normal server
   authentication, it immediately returns the shared error. The handler has no
   JSON extractor and does not read the request body, extract `AppState`, or
   consult application state for domain work.

4. **Archived projection**

   Archived graph projection remains readable but no longer queries
   `simple_workflows` or checks archived Plan eligibility. Compatibility fields
   are filled with constant retired values.

5. **Simple workflow store**

   `simple_workflows` returns to locator-only identity for its own parent
   conversation. Registration accepts only the parent conversation, Plan path,
   and optional progress path. It has no archived source parameter or relation.

   The Simple locator projection contains only its Plan and progress paths. Its
   former optional `source_conversation_id` field is removed from Rust and
   TypeScript because current clients never receive or navigate a successor
   relationship.

6. **Frontend**

   The archived overlay renders history only. No frontend module exports or
   calls a successor request helper. The backend compatibility entry points
   exist solely for older clients, not current UI reachability.

## Stable Retirement API Contract

The retained operation is:

```text
continue_archived_workflow_in_simple
```

For every invocation that reaches the registered command or authenticated
route, the result is exactly:

```json
{
  "code": "simple_successor_creation_retired",
  "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
}
```

The server maps this typed error to HTTP `409 Conflict`, matching the existing
workflow retirement conflict family. Tauri and server transports serialize the
same `code` and `message`. Neither transport adds source, successor, Plan, or
token-dependent fields.

The retirement error takes precedence over all operation argument validation.
The compatibility wrappers no longer deserialize the old successor arguments,
so the result does not vary for:

- nonexistent, deleted, ordinary, Simple, archived root, or archived child
  conversation IDs;
- zero, negative, or otherwise semantically invalid IDs;
- empty, repeated, oversized, or otherwise semantically invalid request
  tokens;
- missing or unreadable archived Plans;
- corrupt workflow identity; or
- any database contents.

Server authentication still occurs before the handler and may return `401`.
Failures below application routing, such as an invalid HTTP request or a closed
Tauri invocation channel, remain transport failures. The retired operation
itself does not parse the request body or successor argument values, so missing,
malformed, wrongly typed, or extra operation arguments cannot select a former
successor-domain error.

The endpoint never returns its former success DTO. The Rust and TypeScript
`SimpleSuccessorResult` types can therefore be deleted from current product
code. Both registered Rust wrappers use `Result<(), AppCommandError>`; the unit
success value is unreachable and therefore never appears on the wire.

Add `AppErrorCode::SimpleSuccessorCreationRetired`, serialized as
`simple_successor_creation_retired` and mapped to HTTP 409. Remove the now-dead
successor domain codes `SimpleSuccessorPlanUnavailable`,
`SimpleSuccessorSourceNotArchived`, and
`SimpleSuccessorSourceAlreadySimple` after all internal callers and tests are
removed. They are not retained compatibility surfaces because every call to
the only public successor operation now resolves to the single retirement
code.

## Zero-Side-Effect Contract

Calling the retired operation must not:

- open a database connection or execute a query;
- read, stat, normalize, create, update, or delete a filesystem path;
- load or create a conversation;
- create or update a Simple descriptor;
- inspect or mutate a manifest workflow;
- enqueue or append a prompt or transcript entry;
- connect, disconnect, wake, or otherwise touch an ACP session;
- emit a conversation or workflow refresh event;
- enqueue auto-title work;
- reserve a delegation run, budget, authorization, or attention item; or
- retry based on SQLite or uniqueness errors.

This is primarily enforced by dependency shape: the shared retired core has no
stateful inputs. Tests additionally compare representative database state and
event observations before and after transport calls.

## Archived Wire Compatibility

`ArchivedWorkflowNavigationSnapshot` keeps its existing field names so older
frontends can continue to deserialize archived graph responses:

```json
{
  "source_conversation_id": 123,
  "plan_rel_path": "docs/superpowers/plans/example.md",
  "successor_conversation_id": null,
  "can_create_simple_successor": false
}
```

`source_conversation_id` and `plan_rel_path` remain archive navigation data.
The successor fields are constants:

- `successor_conversation_id` is always `null`;
- `can_create_simple_successor` is always `false`.

Projection must not query a Simple descriptor by source workflow, inspect Plan
eligibility, or infer a successor from conversation history to populate these
fields.

Existing `workflow_v2_retired` mutation errors remain the stable archived
write fence. Its code remains unchanged, but its obsolete instruction is
replaced with:

```text
This workflow is archived and read-only. Create a new conversation and use a new Design.
```

If its compatibility metadata includes successor navigation, that metadata
must likewise stop querying source links and must never advertise creation: no
successor ID and `can_create_simple_successor=false`. The new
`simple_successor_creation_retired` code is specific to direct calls to the old
successor API.

## Data Model Removal

The clean-install Simple descriptor becomes:

```text
simple_workflows
  parent_conversation_id  INTEGER PRIMARY KEY REFERENCES conversation(id)
  plan_rel_path           TEXT NOT NULL
  progress_rel_path       TEXT NOT NULL
  created_at              TIMESTAMP NOT NULL
  updated_at              TIMESTAMP NOT NULL
```

Remove from the unpublished schema and model:

- the nullable `simple_workflows.source_workflow_id` column;
- its foreign key to `delegation_workflows`;
- its unique index;
- the SeaORM model field and relation;
- source-aware registration parameters and helper functions;
- source mismatch and source-not-found registration errors; and
- every query that looks up a Simple descriptor by archived workflow ID.

Remove the optional Simple locator projection field
`source_conversation_id` from the Rust DTO and TypeScript mirror. This is not
the archived snapshot's required `source_conversation_id`; the two structures
have different compatibility contracts.

Remove the entire successor bootstrap data model:

- `simple_successor_bootstraps` table definition;
- both unique indexes and both foreign keys;
- its migration registration;
- its SeaORM entity and module exports;
- pending/admitted status values; and
- bootstrap insertion, lookup, admission, replay, and cleanup code.

The feature has not been published. The migration history may therefore be
rewritten to describe the intended clean-install schema. No forward migration,
column-copy table rebuild, compatibility view, or runtime cleanup is added.
Development databases created from the superseded branch are unsupported and
must be rebuilt. Tests use fresh databases created from the final migration
chain.

Already generated development successor conversations and rows are not
migrated or reinterpreted. Rebuilding the development database discards their
database identity; repository files are not automatically deleted.

## Production Logic Removal

Delete successor-only logic, including:

- archived source loading and Plan availability checks for creation;
- successor title and progress-path construction;
- transaction, uniqueness convergence, retry, and rollback handling;
- client request token validation and idempotency;
- conversation and descriptor creation for successors;
- auto-title and compatibility refresh emission for successors;
- existing-successor replay and locator comparison;
- delete-then-recreate behavior;
- bootstrap prompt construction and byte limits;
- pending bootstrap admission locks and prompt sinks;
- post-connect bootstrap admission and failure disconnect hooks; and
- successor-specific test controls and fixtures.

Any helper that is also used by ordinary Simple registration or archived
read-only projection remains, but its successor-only parameter or branch is
removed.

The connection lifecycle no longer asks whether a newly connected conversation
has a pending successor bootstrap. A normal new conversation receives prompts
only through the ordinary explicit user-send path.

## Frontend Removal

Remove from current frontend code:

- the **Continue in Simple** control and icon;
- pending/double-click/error handling for that control;
- navigation to an existing or newly returned successor;
- request-token generation for successor creation;
- `continueArchivedWorkflowInSimple` API and Tauri helpers;
- the `SimpleSuccessorResult` TypeScript type; and
- successor-specific localized strings and interaction tests.

Archived UI tests instead assert that history remains visible and every
successor action is absent even when compatibility data is supplied.

The general new-conversation UI is unchanged. No explanatory replacement
button or archive-specific callout is added.

## Error Handling

| Condition | Required behavior |
| --- | --- |
| Valid old Tauri invocation | Exact retirement error; no side effects |
| Authenticated server request with any body | HTTP 409 and exact retirement error |
| Unauthenticated server request | Existing HTTP 401 before handler |
| Missing, malformed, or semantically invalid old arguments | Exact retirement error; no validation |
| Failure before application routing | Existing transport failure |
| Archived graph read | History plus constant `null`/`false` successor fields |
| Archived mutation | Existing `workflow_v2_retired` fence |
| Normal new conversation | Existing behavior, with no archived link |
| Ordinary Simple registration/update | Existing locator behavior without a source argument |

No fallback may call the former creation logic after returning or logging the
retirement error.

## Verification Strategy

Implementation follows test-driven removal: first change tests to express the
retired contract and observe failures against the current creator, then remove
the implementation until those tests pass.

### Retired core and side effects

- Assert the exact code and message from the shared retired core.
- Assert the core's public dependency shape has no database or emitter input.
- Invoke it with representative archived, ordinary, Simple, missing, deleted,
  zero, and negative IDs plus missing, wrongly typed, empty, and oversized
  semantic token values; every routed call returns the same error.
- Snapshot relevant conversation, Simple descriptor, workflow, bootstrap,
  transcript, delegation, and authorization counts before and after calls and
  assert no change while the old schema is still available during RED testing.
- Assert no application event or prompt sink is invoked.

### Tauri and server parity

- Assert the production Tauri registry still contains
  `continue_archived_workflow_in_simple`.
- Assert the production server router still contains the authenticated route.
- Assert both wrappers serialize the exact same code and message.
- Assert the authenticated server response is HTTP 409.
- Assert server authentication still returns 401 before retirement handling.
- Assert empty, malformed JSON, wrongly typed legacy fields, and extra fields
  all reach the same authenticated server retirement response because the body
  is not extracted.
- Assert no former source/Plan-specific error can escape the retained route.

### Archived projection compatibility

- Serialize archived root and child snapshots and assert literal
  `successor_conversation_id: null` and
  `can_create_simple_successor: false`.
- Seed unrelated Simple descriptors and prove archived projection never treats
  them as successors.
- Assert archived projection no longer reads the archived Plan merely to
  decide successor eligibility.
- Preserve archived graph, report, Card, gate, runtime, and child-navigation
  coverage.

### Schema and store

- Build a fresh database and inspect `simple_workflows`; assert no source
  column, foreign key, or source index exists.
- Assert the bootstrap table and indexes do not exist.
- Assert deleting a Simple parent still cascades its locator descriptor.
- Assert ordinary Simple registration and locator updates remain idempotent.
- Assert registered Simple roots still reject conflicting archived identity
  under the surviving mode-resolution rules.
- Run migration and completion-protocol integration tests to catch accidental
  changes to unrelated archived tables.

### Frontend

- Render realistic archived overlays with both old successor field values and
  retired constant values; assert no successor action is present.
- Assert no click path invokes
  `continue_archived_workflow_in_simple` or navigates to a successor.
- Assert current TypeScript modules no longer export the request helper or
  success DTO.
- Preserve archived history navigation and ordinary new-conversation tests.
- Run the focused overlay and API tests, then the full frontend suite and
  ESLint.

### Rust regression matrix

Run Cargo commands serially, with `RUST_MIN_STACK=16777216` where established
by the repository test environment:

- focused retired command, archived projection, migration, and ordinary Simple
  tests;
- desktop library and integration tests with `test-utils`;
- server library and binary tests;
- desktop Clippy with all targets and `test-utils`;
- server Clippy; and
- MCP Clippy.

Use long waits for the full Rust commands. Do not overlap Cargo processes.

### Frontend production build

Do not run `pnpm build` in the protected main worktree. If build evidence is
required, create a clean detached worktree at the reviewed commit, install or
reuse dependencies according to repository policy, run the build there, and
remove only that explicitly created worktree afterward.

## Success Criteria

- Current UI exposes no archive-to-Simple creation or navigation action.
- Current frontend code cannot call the old successor API.
- Old Tauri and server callers receive the exact permanent retirement error.
- The retired command cannot access state or produce side effects.
- Archived wire readers continue to receive their expected field names with
  `null` and `false` values.
- Clean databases contain no Simple source link or bootstrap table.
- No source-link, bootstrap, creation, replay, or concurrency code remains.
- A user can still create an ordinary conversation and run a new Design, Plan,
  and Simple workflow.
- Archived history remains readable and all archived mutation fences remain
  effective.
- Ordinary Simple registration, locator updates, projection, delegation,
  review, recovery, and execution remain green.

## Alternatives Considered

### Repair automatic successor bootstrap and replay

Rejected. Correctness would require durable prompt idempotency, transaction and
admission failure handling, replay across locator changes, concurrent creation
convergence, and permanent source identity. That complexity exists only to
offer a shortcut the product does not need.

### Create a successor when the source is archived

Rejected. Archival would gain a hidden write side effect and could create a
conversation the user did not request or intend to continue.

### Keep the UI button but make it open the generic new-conversation dialog

Rejected. It would still imply archive-specific continuation and invite future
copying or linking behavior. The existing global new-conversation action is
sufficient and semantically honest.

### Remove the old API routes entirely

Rejected. Stale clients would receive transport-specific unknown-command or
not-implemented failures. A registered rejection surface provides a stable,
actionable code and message while guaranteeing no old behavior can revive.

### Preserve source links for already created successors

Rejected. The feature is unpublished, development databases may be rebuilt,
and retaining the field would preserve dead identity and query complexity with
no supported product behavior.

## Implementation Gate

No implementation change is authorized by this document alone. After this
specification is committed and reviewed by the user, the next step is to write
a detailed implementation plan. That plan must preserve unrelated worktree
changes and must not rewrite or drop unrelated historical commits.
