# Completion Protocol V2-Only Design

## Status

Approved in the 2026-08-09 design discussion.

This design replaces the completion-protocol rollout and legacy restart
behavior defined by
`2026-08-04-platform-generated-completion-evidence-design.md`. Protocol v2
remains unchanged internally: `complete_work`, explicit conclusion lines,
bounded report conclusions, and typed user adjudication are all still valid v2
semantic inputs.

The incompatible decision in this document is narrower: Codeg must never
create, resume, mutate, or fall back to a completion-protocol-v1 workflow.
Persisted v1 workflows remain available as historical read-only data.

## Executive Decision

Codeg has one writable completion protocol:

```text
completion_protocol_version = 2
completion_protocol_mode = v2_enforce
```

There is no rollout selection, shadow mode, profile override, legacy restart,
or runtime fallback to v1. New workflow creation writes the fixed v2 identity.
Every mutation of an existing workflow validates the persisted header before
spending a delegation budget, launching a child, or opening a write
transaction.

Protocol-v1 rows remain in their current tables so historical conversations,
workflow graphs, Cards, and existing predecessor/successor links can still be
read. Their persisted protocol fields are not rewritten. A v1 workflow cannot
be continued or converted in place, and Codeg does not automatically create a
v2 successor.

## Goals

- Make protocol v2 the only protocol that production code can create.
- Remove every configuration, API, tool, helper, and runtime branch capable of
  selecting or executing protocol v1.
- Fail closed when a workflow protocol is v1, unknown, corrupt, or unavailable.
- Keep historical v1 workflows readable without converting or deleting their
  data.
- Make the v2-only invariant visible at compile-time, at public tool-schema
  boundaries, and at the database insertion boundary.
- Preserve all protocol-v2 semantic input channels and evidence behavior.
- Preserve standalone non-workflow delegation behavior.

## Non-Goals

- Migrating v1 Cards, settlements, runs, or evidence into v2.
- Automatically creating a v2 successor for a legacy workflow.
- Deleting historical v1 conversations or workflow rows.
- Removing v2 natural-language conclusion parsing, report parsing, or user
  adjudication.
- Redesigning completion evidence, artifact resolution, gate reduction, or
  attention handling for valid v2 workflows.
- Removing Card parsing used by standalone, non-workflow delegation display.
- Preventing an explicit user-initiated deletion of a historical conversation.

## Terminology

`Writable workflow` means a workflow whose persisted header is exactly
`(version=2, mode=v2_enforce)`.

`Historical v1 workflow` means a persisted workflow whose version is `1`,
including rows whose recorded mode is `v1` or `v2_shadow`.

`Protocol fallback` means handling a workflow-bound operation with v1 Card
semantics after v2 selection, binding, lookup, or execution cannot proceed.
Protocol fallback is forbidden.

The ordered semantic inputs inside protocol v2 are not protocol fallbacks.
They remain part of the v2 contract.

## Rejected Alternatives

### Retain Rollout Types but Accept Only `v2_enforce`

This has a smaller initial diff, but leaves dead modes, profile overrides,
selection sources, shadow metrics, and mixed request schemas in production.
Those surfaces make a future accidental v1 re-entry possible and keep the
parameter-binding ambiguity that motivated this change.

### Move V1 Rows Into Separate Archive Tables

This would produce the cleanest live schema, but it would require duplicating
or unioning workflow graph queries across many foreign-key tables. The
migration and projection risk is disproportionate when the required behavior
is satisfied by a read-only boundary around existing rows.

### Upgrade V1 Rows In Place or Automatically Create Successors

V1 Cards and settlements are not valid v2 evidence. Rewriting a header would
misrepresent evidence, and successor creation is precisely the fallback path
being removed.

## Protocol Ownership

### Current Protocol Constant

The workflow module owns one production creation identity. Callers do not pass
a selection:

```text
CURRENT_COMPLETION_PROTOCOL_VERSION = 2
CURRENT_COMPLETION_PROTOCOL_MODE = v2_enforce
```

The exact Rust representation may be constants or a small constructor, but it
must not accept an agent, profile, environment value, or caller-supplied mode.

The following production concepts are removed:

- `CompletionProtocolRolloutConfig`;
- `CompletionProtocolSelection`;
- `CompletionProtocolSelectionSource`;
- `select_completion_protocol`;
- completion profile keys and override parsing;
- `v1_default()` and `legacy_restart()` constructors; and
- v2 shadow selection and comparison execution.

`CompletionProtocolMode::V1` and `CompletionProtocolMode::V2Shadow` remain in
the database entity only because SeaORM must deserialize historical rows.
They are read-model values, not creation options.

### Removed Environment Configuration

`CODEG_COMPLETION_PROTOCOL_MODE` and
`CODEG_COMPLETION_PROTOCOL_OVERRIDES` are removed configuration surfaces.

Desktop and server startup explicitly reject either variable when it is
present, including an old value of `v2_enforce`. This avoids silently ignoring
stale deployment configuration and proves that runtime selection no longer
exists. The startup error instructs the operator to remove the variables.

Test constructors no longer install a default rollout object.

## Workflow Creation and Publication

`publish_workflow_manifest` no longer chooses an author/profile rollout
subject. The listener validates the document and calls one v2-only store API.

The store behavior is:

```text
no workflow header
  -> insert fixed (2, v2_enforce)
  -> initialize v2 gate states in the same transaction

existing v2 header
  -> apply the normal manifest revision transaction

existing v1 header
  -> return legacy_completion_protocol_read_only before mutation

unknown/inconsistent header
  -> return unsupported_completion_protocol before mutation
```

The public `publish_workflow_manifest_core` becomes v2-only. The
selection-taking variant is deleted. Tests that need historical v1 data must
use an explicitly named test-only fixture that inserts historical rows; no
production helper may construct them.

The protocol remains frozen after creation. Updating a manifest cannot change
the version or mode.

## Central Mutation Guard

One shared guard validates a loaded workflow header before any semantic write:

```text
require_v2_mutation(header)
  (2, v2_enforce) -> allowed
  version 1       -> legacy_completion_protocol_read_only
  anything else   -> unsupported_completion_protocol
```

The guard is applied at the outer boundary and retained at transaction-critical
store boundaries where a direct internal call could otherwise bypass it.
Calling it twice is acceptable; inconsistent mutation behavior is not.

The following production operations require the guard:

- publishing a revision to an existing workflow;
- settling any workflow gate;
- recovering a workflow;
- resolving completion decisions or Design self-review decisions;
- retrying or resolving completion artifacts;
- final-delivery guards and other completion mutations;
- first dispatch of a workflow work unit;
- continuing or replacing a workflow child; and
- terminal semantic completion and gate reduction.

For delegation operations, the check occurs before budget authorization,
reservation insertion, child-process creation, or MCP injection. A historical
v1 work-unit key cannot start a new child.

Infrastructure reconciliation may still close an abandoned process/run for
host consistency, but it cannot parse v1 completion output, write semantic
completion evidence, update a gate, or advance the workflow graph.

## Historical V1 Read Model

The following read behavior remains supported:

- conversation and transcript loading;
- workflow state and graph snapshots;
- historical v1 Card display;
- recorded protocol version and mode;
- existing `legacy_source` and `v2_successor` links; and
- normal explicit deletion of a conversation and its dependent data.

Every v1 workflow projection sets:

```json
{
  "completion_protocol": {
    "version": 1,
    "read_only_reason": "legacy_completion_protocol_read_only"
  }
}
```

The read-only reason no longer depends on whether a successor exists. Existing
successor links remain navigable, but no new successor is created.

The UI shows the historical read-only notice and existing relationship links.
It does not show restart, resume, settle, recovery, or other workflow action
controls for v1.

## Legacy Restart Removal

All write surfaces for legacy restart are removed:

- MCP `restart_legacy_workflow` catalog entry and dispatcher;
- Broker transport request and listener branch;
- Tauri command;
- Axum route and handler;
- frontend API and web-transport command mapping;
- workflow overlay restart button and callback; and
- automatic restart checks before publish, settle, recover, or delegation.

The historical restart tables, context rows, `legacy_source_workflow_id`
column, projection fields, and read queries remain because already-created
links must still render.

## V2-Only Public Tool Schemas

`settle_workflow_gate` exposes one v2 request shape. Legacy settlement fields
are removed from the MCP schema and transport DTO:

- `manifest_revision`;
- `gate_cycle`;
- legacy `outcome`; and
- legacy `evidence`.

The remaining request fields are the existing v2 fields. Their conditional
requirements continue to be validated by gate kind. The listener calls only
the v2 settlement core and contains no version-based v1 branch.

V2 child completion continues to expose `complete_work` only when a committed
workflow binding carries protocol version `2`. Model arguments never populate
the workflow, task, role, node, gate, or protocol identity.

Root `workflow_v2` capability remains distinct from completion protocol v2; it
continues to expose the remaining root workflow tools, excluding legacy
restart.

## Task Admission and MCP Binding

A workflow-bound task must load a header and pass `require_v2_mutation` before
admission. Successful admission always:

1. builds and persists the v2 completion instruction scope;
2. commits a `WorkflowChildMcpBinding` with `protocol_version=2`;
3. appends the exact canonical instruction to the child prompt; and
4. injects the child-only `completion_v2` MCP feature.

Missing headers, dangling run bindings, v1 headers, unsupported modes, and
instruction-scope errors abort launch. They never produce a child without the
v2 binding and never expose `complete_work` through an unbound token.

## Terminal Completion

Terminal completion distinguishes workflow binding from standalone
delegation before choosing processing behavior:

```text
standalone delegation
  -> existing standalone display/Card-summary behavior

workflow binding + exact v2 header
  -> v2 intent resolver and platform-generated evidence

workflow binding + v1 header
  -> legacy_completion_protocol_read_only

workflow binding + unknown/missing/corrupt header
  -> fail closed with the typed persistence/protocol error
```

The v1 Card parser and v2-shadow comparator are never invoked for a
workflow-bound terminal run. A protocol lookup failure is retryable according
to existing persistence policy; if it remains terminally unavailable, the run
surfaces the typed failure without writing Card authority, completion
evidence, attention, settlement, or gate state.

The following protocol-v2 inputs remain ordered and supported:

1. a valid `complete_work` call;
2. an explicit terminal conclusion line;
3. an explicit conclusion in an eligible bounded report; and
4. typed user adjudication for missing or ambiguous meaning.

These channels all produce v2 platform-generated evidence and are not v1
fallbacks.

## Database Enforcement

A new forward migration adds database enforcement without rewriting old rows.

### V2-Only Insert Trigger

A `BEFORE INSERT` trigger on `delegation_workflows` rejects every row whose
protocol pair is not exactly `(2, 'v2_enforce')` or whose
`legacy_source_workflow_id` is non-null. This also rejects inserts that omit
the protocol columns and would otherwise receive the historical
`DEFAULT 1/'v1'` values, and prevents a future caller from recreating the
removed legacy-successor path.

The old migration is not edited. On a fresh database, migrations finish and
install the trigger before application traffic starts. On an existing
database, historical v1 rows remain untouched.

### Frozen Protocol Trigger

A `BEFORE UPDATE OF completion_protocol_version,
completion_protocol_mode` trigger rejects any attempt to change either value.
This prevents both v2 downgrade and v1 in-place upgrade.

The application guard remains necessary because semantic workflow state also
lives in related tables. The trigger is a final insertion/freeze invariant,
not a replacement for authorization at every mutation boundary.

Migration rollback removes only the new triggers. It does not alter workflow
rows.

## Settings, Metrics, and UI Cleanup

The completion rollout is no longer a setting or a status surface. Remove:

- `get_completion_protocol_settings` Tauri and HTTP APIs;
- the frontend API type and call;
- the delegation settings completion-protocol status block;
- default-mode, profile-override, shadow-difference, rollout-window, and
  rollout-decision metrics and translations; and
- metrics code used only to compare v1 with v2 shadow behavior.

Keep metrics that observe v2 completion intent sources, evidence resolution,
attention, artifact recovery, and typed completion outcomes.

Creation telemetry may record the fixed v2 protocol if useful for operational
counts, but it must not expose or depend on a selectable creation mode.

## Error Contract

All transports preserve stable error codes.

| Condition | Stable code | Retry behavior |
| --- | --- | --- |
| Mutation targets a persisted v1 workflow | `legacy_completion_protocol_read_only` | Not retryable in that workflow |
| Persisted protocol pair is unknown or inconsistent | `unsupported_completion_protocol` | Not retryable until data is repaired |
| V2 instruction or scope binding cannot be built | `completion_instruction_binding_failed` | Retry only after the reported material problem changes |
| Workflow/header lookup has a transient database error | Existing persistence code | Existing transient retry policy |
| Removed completion rollout environment variable is present | `completion_protocol_configuration_removed` | Remove the variable and restart |

No error mapper converts any of these conditions into a successful v1 result,
a legacy restart projection, or a Card re-emission request.

## Frontend Behavior

For a v2 workflow, existing graph and completion controls remain unchanged.

For a v1 workflow:

- the graph and transcript remain visible;
- a read-only explanation is visible;
- already-persisted predecessor/successor links remain available;
- workflow mutation controls are absent; and
- the normal conversation delete action remains available.

The frontend does not synthesize a successor or automatically open a new
conversation. Continuing the subject requires an explicit new conversation
started by the user, which creates an unrelated v2 workflow through the normal
creation path.

## Testing Strategy

### Protocol Construction Tests

- Every new workflow, across agent and profile combinations, persists exactly
  `(2, v2_enforce)`.
- Publication revisions retain the original v2 pair.
- Production code has no caller-supplied completion selection.
- Test-only legacy fixtures cannot be imported by non-test builds.

### Startup Configuration Tests

- Desktop startup rejects each removed environment variable.
- Server startup rejects each removed environment variable with exit code `2`.
- An environment value of `v1`, `v2_shadow`, or `v2_enforce` is equally
  rejected because the configuration surface itself is removed.

### Migration Tests

- A v1 row inserted before the new migration remains readable afterward.
- An omitted protocol pair is rejected after migration.
- Explicit `(1, v1)` and `(1, v2_shadow)` inserts are rejected.
- Exact `(2, v2_enforce)` inserts succeed.
- A new v2 row with `legacy_source_workflow_id` is rejected.
- Updating either protocol field is rejected for both historical and current
  rows.
- Rolling back the migration removes the triggers without changing rows.

### Historical Read-Only Matrix

For a seeded v1 workflow, verify that each operation returns
`legacy_completion_protocol_read_only` and leaves workflow revision, gate
state, settlements, attentions, run bindings, and child-spawn counts unchanged:

- publish revision;
- settle gate;
- recover workflow;
- resolve completion decision;
- retry completion artifact;
- first workflow dispatch;
- continue delegation; and
- replace delegation.

State and graph reads must still succeed. Existing relationship links must
still project.

### Tool and Transport Schema Tests

- `settle_workflow_gate` contains no legacy settlement properties.
- `restart_legacy_workflow` is absent from MCP, HTTP, Tauri, frontend API, and
  web-transport allowlists.
- A v2 child receives `complete_work`; root, standalone, unbound, and historical
  v1 children do not.
- Unknown tool arguments continue to fail schema validation.

### Terminal Processing Tests

- A v2 workflow run uses only v2 completion resolution.
- A v1-bound terminal attempt returns the read-only error and does not invoke
  the Card parser.
- A dangling or failed workflow-protocol lookup never produces a Card summary
  or shadow sample.
- Standalone delegation retains its existing display summary behavior.
- `complete_work`, conclusion line, report conclusion, ambiguity attention, and
  user adjudication remain valid v2 paths.

### Frontend Tests

- A historical v1 snapshot renders read-only state and existing links.
- No restart, resume, settle, or recovery control renders for v1.
- The completion rollout settings request and status component are removed.
- V2 workflow controls continue to render and operate.

### Verification Commands

Use focused Rust tests while iterating, then run the checks proportional to the
cross-cutting change:

```bash
pnpm eslint .
pnpm test
pnpm build

cd src-tauri
cargo check
cargo test --lib --features test-utils
cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo check --no-default-features --bin codeg-mcp
```

Run Clippy targets when the corresponding checks fit the available machine
memory. Low-memory commands remain opt-in according to `AGENTS.md`.

## Acceptance Criteria

- No production creation path can persist completion protocol version `1` or
  mode `v1`/`v2_shadow`.
- No runtime configuration can select a completion protocol.
- No public mutation schema contains legacy settlement or restart parameters.
- Every historical v1 workflow is readable and semantically immutable.
- Every workflow-bound protocol error fails closed without Card, shadow, or
  successor fallback.
- Every valid new workflow child receives an immutable v2 binding and canonical
  v2 completion instruction.
- Protocol-v2 semantic input and evidence regressions remain passing.
- Existing historical data is neither rewritten nor deleted.

## Implementation Order

1. Add the v2-only protocol constructor, shared mutation guard, stable errors,
   and negative tests.
2. Make publication and settlement v2-only and remove rollout selection.
3. Enforce v2 admission and terminal processing without Card/shadow fallback.
4. Remove legacy restart write surfaces and add historical read-only
   projection behavior.
5. Add database triggers and migration tests.
6. Remove rollout settings, metrics, frontend APIs, controls, and translations.
7. Update fixtures and run cross-surface verification.
