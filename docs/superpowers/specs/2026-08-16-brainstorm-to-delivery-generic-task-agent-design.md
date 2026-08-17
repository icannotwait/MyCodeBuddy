# Brainstorm-to-Delivery Generic Task Agent and Adaptive Routing Design

## Status

Approved in brainstorming on 2026-08-16.

This design extends the writable Simple brainstorm-to-delivery workflow from
`2026-08-11-simple-workflow-v2-retirement-design.md`. It selectively restores
the independent document-authoring and adaptive Task-routing behavior from
`2026-07-27-brainstorm-to-delivery-adaptive-routing-design.md` without
restoring workflow manifests, platform gates, gate settlements, completion
Cards, or platform-owned completion evidence.

## Executive Decision

Simple remains the only writable brainstorm-to-delivery mode. Its coordination
surfaces remain the Brainstorm/Design, Implementation Plan, progress ledger,
Git repository, and generic delegation runs. Routed Task runs additionally
carry an immutable generic orchestration binding. This binding supplies the
durable historical reference that mutable Plan and progress documents cannot
provide; it is run identity, not a platform-owned Simple admission Gate.

The workflow gains five behaviors:

1. The Grok-specific implementation role becomes a user-selectable **Task
   Agent** role. Grok remains the default.
2. A dedicated Codex Plan Author writes and revises the Implementation Plan.
3. Dedicated Codex document producers are separate from Codex document
   reviewers. The parent orchestrator no longer edits Design or Plan content.
4. `b2d_task_risk_v1` selects a normal or high Task route. High Tasks force
   Codex implementation and use Codex plus Task Agent review.
5. The validator derives a versioned route fingerprint for each Plan Task. The
   parent copies the validator's exact `orchestration_binding` into every
   routed Task delegation, and the generic run store preserves it across
   continuation and replacement.

```text
completed Brainstorm
  -> conditional Codex Design review
  -> independent Codex Design Fixer when changes are required
  -> independent Codex Plan Author using writing-plans
  -> independent Codex Plan review
  -> serial Tasks
       normal: Task Agent implementer -> Codex primary reviewer
       high:   Codex implementer -----> Codex primary reviewer
                                 \----> Task Agent auxiliary reviewer
  -> independent Codex final review
  -> owning Task producer fixes findings
  -> local delivery
```

The parent orchestrator selects and dispatches roles, reconciles files and
generic runs, adjudicates findings against repository evidence, updates the
progress ledger, and controls delivery. It does not write Design, Plan, or Task
implementation content.

## Goals

- Keep Grok as the default Task Agent while allowing a user to select another
  built-in or custom Agent in the Skill invocation message.
- Use one Task Agent selection at a time and permit an explicit selection
  change only between Tasks.
- Keep Task-Agent identity stable within every admitted work-unit lineage.
- Restore a dedicated Codex Plan Author for initial Plan creation and revision.
- Route Design fixes and Plan revisions through independent Codex producer
  conversations rather than the parent orchestrator.
- Preserve producer/reviewer conversation independence even when all selected
  Agent types are Codex.
- Restore the deterministic `b2d_task_risk_v1` policy and its normal/high
  routes.
- Keep Simple's non-blocking platform projection and generic delegation
  recovery model.
- Make routing, recovery, and Task-Agent changes machine-checkable through
  bounded Plan, progress, and parent-scoped durable-run evidence contracts.
- Prevent a coordinated Plan/progress rewrite from changing the route or Task
  Agent generation of a Task that has any admitted durable run.
- Preserve the validator-derived workflow generation and route fingerprint in
  generic run identity before child spawn or resume side effects.

## Non-Goals

- Restoring workflow manifest publication or any workflow-v2 mutation tool.
- Restoring platform gate settlement, completion Cards, artifact digests, or
  platform-owned completion decisions.
- Adding an Agent picker to the Codeg frontend.
- Adding project-level Task-Agent configuration.
- Automatically selecting an Agent from task content or availability.
- Silently substituting another Agent when the selected Task Agent is
  unavailable.
- Supporting an Agent handoff inside an active Task.
- Reusing the ACP launch/config `delegation_task_runs.route_fingerprint` for
  workflow orchestration identity.
- Backfilling or guessing orchestration bindings for historical runs.
- Making Rust Simple projection the owner of route admission, completion, or
  recovery decisions.
- Restoring Plan finding-owner subsets, scoped reviewer cohorts, stagnation
  counters, or automatic holistic Plan rewrites.
- Changing standalone/ad-hoc delegation behavior when the optional binding is
  omitted.

## Terminology

**Task Agent** is the workflow-selected auxiliary Agent type and optional
profile. It implements and fixes normal Tasks and acts as the auxiliary
reviewer for high Tasks. Grok is the default, not a special role.

**Task Agent generation** is a monotonically increasing selection record. Each
Task references exactly one generation. Changing the selection appends a new
generation for pending Tasks and never rewrites prior Task identity.

**Orchestration binding** is the optional, versioned generic delegation input
and immutable durable run identity `{schema_version, namespace, generation,
route_fingerprint}`. For this workflow its namespace is
`brainstorm-to-delivery`, its generation is the Plan Task's Task Agent
generation, and its fingerprint covers the complete derived Task route.

**Orchestration route fingerprint** is the lowercase `sha256:<64 hex>` digest
derived by the Node validator from canonical Task route input. It is distinct
from `delegation_task_runs.route_fingerprint`, which identifies the ACP
launch/config snapshot used for session reuse.

**Durable binding snapshot** is a complete, parent-scoped, namespace-filtered,
read-only view of generic run identities, current statuses, and orchestration
bindings. It contains no task prompt or child output and is supplied to the
validator for an execution-admission decision.

**Admitted Task run** is a generic delegation run for which the reserving row
exists. `reserving`, `running`, and every terminal status are admitted states;
a pre-reservation tool failure is not admitted.

**Primary reviewer** is the independent Codex Task reviewer required for both
normal and high Tasks.

**Auxiliary reviewer** is the independent Task Agent reviewer additionally
required for high Tasks.

**Document producer** is either the Design Fixer or Plan Author. A document
producer may continue its own work unit for revisions but cannot review or
approve its own artifact.

**Parent orchestrator** is the root conversation running the Skill. It owns
coordination and adjudication, not document or Task production.

## System Invariants

1. Simple remains manifest-free and has no platform-owned execution gate.
2. The invocation resolves exactly one initial Task Agent generation. An
   omitted selection resolves to Grok.
3. An explicit Task Agent is never silently replaced or downgraded.
4. A Task Agent generation is immutable once any Task referencing it is
   admitted.
5. A Task's risk, producer route, and reviewer slots are fixed at admission.
6. A Task Agent selection change appends a generation and can affect only
   never-admitted pending Tasks after a Task boundary and a newly reviewed Plan
   revision. No Plan/progress rewrite can redefine an existing durable
   binding.
7. Distinct work-unit keys cannot share a child conversation ID. A continuation
   reuses only its own work unit's child conversation.
8. Consequently, the Design Fixer, Plan Author, Task producers, document
   reviewers, Task reviewers, and final reviewer remain separate. This applies
   even when their Agent types and profiles match.
9. Task order is serial. The two high-Task reviews may run concurrently after
   the producer finishes.
10. Any implementation or fix invalidates all prior Task review conclusions.
11. A high Task completes only after both reviewers cover and approve the
    latest producer result.
12. Generic delegation continuation and replacement budgets remain
    authoritative for each stable work-unit lineage.
13. Every routed Task dispatch, continuation, or replacement uses the exact
    binding emitted by deterministic validation. Continuations and
    replacements inherit the source binding, and an explicit mismatch is
    rejected before side effects or recovery-budget consumption.
14. An orchestration binding is written in the reserving transaction and is
    immutable after insert. Lifecycle and status updates cannot alter it.
15. Before a workflow action that could dispatch, continue, recover, resume
    after compaction, or change Task Agent selection, the Skill obtains a
    complete fresh durable binding snapshot and fails closed on any missing,
    extra, malformed, stale, truncated, or inconsistent evidence.
16. Plan/progress/run disagreement may create bounded Rust Simple projection
    warnings, but never a platform admission Gate. The Skill and Node
    validator own the fail-closed execution decision.
17. Requirement, scope, architecture, or user-data decisions remain owned by
    the user. Agents may not infer a material change from a review finding.

## Role Contract

### Document roles

| Work | Producer | Reviewer | Trigger |
| --- | --- | --- | --- |
| Design revision | Codex Design Fixer | Mandatory Codex Design Reviewer plus user-named document reviewers | Conditional Design review finds a valid issue |
| Plan initial draft | Codex Plan Author | Mandatory Codex Plan Reviewer plus user-named document reviewers | Every workflow |
| Plan revision | Same Codex Plan Author work unit | Same document reviewer work units | Valid Plan finding or pending-Task route change |

Design review keeps the current conditional trigger. It is required when the
Brainstorm spans modules, migration, concurrency, security, persistence,
externally visible compatibility, or material ambiguity. Otherwise the
completed Brainstorm remains the requirements baseline without a Design review
round.

Document Reviewers are read-only. Codex is mandatory; reviewers explicitly
named by the user remain optional and may participate only in Design and Plan
review. When the parent adjudicates a Design finding as valid and it does not
require a user decision, the parent sends a consolidated revision brief to the
Design Fixer. Later Design fixes continue the same Fixer work unit, and
re-review continues the same Reviewer work units.

The Plan Author is the only role that creates or edits the Plan. Its first
prompt requires it to invoke and follow `writing-plans`. A review-driven Plan
revision continues the same Author work unit. The parent may update progress
and adjudication notes but cannot patch Plan content.

Plan re-review remains deliberately Simple: the same independent document
Reviewer work units review the complete latest Plan after every revision until
no Critical or Important finding remains. There is no finding-owner subset,
stagnation state, or automatic rewrite rail.

### Task routes

| Risk | Implementer and fixer | Required reviewers |
| --- | --- | --- |
| `normal` | Task Agent | Independent Codex primary reviewer |
| `high` | Independent Codex implementer | Independent Codex primary reviewer and independent Task Agent auxiliary reviewer |

The Task Agent is auxiliary at the workflow level rather than an unconditional
implementer. High risk always forces the producer to Codex. The selected Task
Agent then supplies the second review perspective.

When the Task Agent is also Codex, high risk intentionally creates three
different Codex child conversations: implementer, primary reviewer, and
auxiliary reviewer. Conversation independence remains mandatory even though
cross-model diversity is absent.

Task review findings are consolidated by the parent and returned to the owning
producer work unit. A normal fix returns to the Task Agent implementer. A high
fix returns to the Codex implementer. After any fix, the normal primary review
or both high reviews must re-run against the latest result.

### Final review

After every Task is complete and covering verification passes, a fresh Codex
final reviewer inspects the complete delivery. Final findings are mapped to the
owning Task producer:

- normal Task findings return to that Task's Task Agent implementer;
- high Task findings return to that Task's Codex implementer.

Fixes reopen covering Task review and final review. There is no separate Final
Fixer that bypasses Task ownership.

## Task Agent Selection

### Invocation resolution

The Skill reads the invocation message before Plan authoring:

- no Agent named: `agent_type: "grok"`, `profile_id: null` unless a Grok
  profile is explicitly selected;
- Agent named: resolve the canonical built-in or `custom:*` wire identity from
  live delegation schemas and available Agent discovery;
- profile named: bind that profile to the generation;
- Agent or profile ambiguous: ask one focused clarification before dispatch;
- Agent unavailable: record a typed blocker and ask the user to choose; do not
  fall back to Grok.

The initial result is generation 1 and is supplied to the Plan Author.

### Boundary changes

A Task Agent change is legal only when no Task is active and every prior Task
is completed. The change procedure is:

1. Query every page of the current `brainstorm-to-delivery` durable binding
   snapshot and run full admission validation against the current Plan and
   progress. Any unavailable, incomplete, stale, unbound, or mismatched
   evidence blocks the change.
2. Prove from durable evidence, not progress alone, that every affected Task
   is pending and has never had a reserving row. Then record the requested
   Agent/profile and next generation in progress as a pending route-change
   intent.
3. Confirm the Agent and profile are available.
4. Continue the Plan Author with a brief that appends the next contiguous
   generation and rewrites only the never-admitted pending Task suffix.
5. Run static deterministic Plan/routing validation and continue the Plan
   Reviewer for a full latest-Plan review.
6. After approval, update only the pending progress entries from the
   validator's exact derived output, obtain a new complete durable snapshot,
   and run full admission validation before admitting the next Task.

Completed and previously admitted Tasks retain their original generation,
route, work-unit keys, run history, and recovery consumption. An unresolved
blocked Task stops serial execution and is not a boundary for changing its own
Agent. A failed or canceled reserving row is still admission and permanently
freezes that Task route; only a failure before the reserving insert leaves the
Task eligible for a boundary change.

A change requested during an active Task is deferred until that Task reaches a
boundary. If the user requires an immediate switch, the workflow blocks; this
design does not create a same-Task cross-Agent handoff lineage.

## Task Risk Policy

Plan classification uses the existing policy identifier
`b2d_task_risk_v1`.

### Hard triggers

Any hard trigger makes a Task high regardless of soft score:

| Signal | Trigger |
| --- | --- |
| `concurrency_lifecycle` | Threading, async coordination, cancellation, ordering, ownership lifetime, or process lifecycle behavior changes |
| `security_trust_boundary` | Authentication, authorization, secrets, sandboxing, trust-boundary validation, or privilege changes |
| `migration_destructive_persistence` | Schema/data migration, deletion, irreversible persistence, or destructive state transitions |
| `public_compatibility` | Public API, protocol, schema, serialized format, or externally consumed behavior changes |
| `unsafe_ffi` | Rust `unsafe`, native FFI, ABI, memory ownership, or equivalent low-level boundaries |
| `update_rollback` | Installer, updater, rollback, recovery, or version-transition behavior changes |

### Soft signals

When no hard trigger is present, each distinct active soft signal contributes
once:

| Signal | Score | Trigger |
| --- | ---: | --- |
| `cross_runtime_or_process` | 2 | Changes code or a contract across runtime or process boundaries |
| `broad_production_surface` | 1 | Touches at least five production files, excluding tests, docs, snapshots, and generated output |
| `multiple_ownership_modules` | 1 | Touches at least two independently owned modules or subsystems |
| `shared_interface` | 1 | Changes an interface or contract consumed outside the owning module |
| `dependency_or_build` | 1 | Changes dependencies, lockfiles, build configuration, packaging, or deployment |
| `multi_layer_without_test_seam` | 1 | Spans at least two architectural layers without an isolated boundary test seam |

A score of 3 or greater is high; 0 through 2 is normal. Every active signal
requires non-empty file, module, or interface evidence. Unknown signals,
duplicates, contradictory levels, incorrect arithmetic, or evidence-free
signals invalidate the Plan route.

Both `hard_triggers` and `soft_signals` contain objects with a canonical
`kind` and non-empty `evidence` array. Soft-signal objects additionally contain
their fixed policy score. An empty array means that signal class is inactive;
a bare signal name without evidence is invalid.

Before Task admission, new evidence may change classification only through a
Plan Author revision and full Plan re-review. After admission, evidence that
invalidates the frozen classification or route blocks the Task and escalates
to the user. The Skill does not dynamically swap the active producer.

## Structured Contracts

### Skill contract v2

The Skill replaces `codeg-b2d-skill-contract-v1` with exactly one
`codeg-b2d-skill-contract-v2` JSON comment. Its positive contract records:

- existing phase order and generic delegation interfaces;
- Codex document roles and independence;
- Task Agent default, invocation source, and boundary-only changes;
- `b2d_task_risk_v1`;
- normal and high producer/reviewer routes;
- reviewer slots;
- validator-derived durable orchestration bindings and complete binding-query
  reconciliation before routed execution or recovery;
- serial Task execution and parallel high review fan-out;
- existing generic recovery limits; and
- independent Codex final review.

The repository validator treats this block as authoritative and rejects prose
that negates or contradicts it.

### Plan routing block

Every Plan contains exactly one bounded JSON comment marked
`codeg-b2d-routing-v1`. It is the machine-readable source for Agent selection,
risk classification, and Task routes. Its conceptual shape is:

```json
{
  "schema_version": 1,
  "risk_policy_version": "b2d_task_risk_v1",
  "task_agent_generations": [
    {
      "generation": 1,
      "agent_type": "grok",
      "profile_id": null,
      "effective_from_task_index": 1
    }
  ],
  "tasks": [
    {
      "index": 1,
      "task_agent_generation": 1,
      "risk": {
        "level": "high",
        "hard_triggers": [],
        "soft_signals": [
          {
            "kind": "cross_runtime_or_process",
            "score": 2,
            "evidence": ["src/lib/transport", "src-tauri/src/web"]
          },
          {
            "kind": "shared_interface",
            "score": 1,
            "evidence": ["transport request contract"]
          }
        ],
        "score": 3,
        "reason": "Changes a shared desktop/server transport boundary."
      },
      "route": {
        "implementer": {
          "agent_type": "codex",
          "profile_id": null
        },
        "reviewers": [
          {
            "slot": "primary",
            "agent_type": "codex",
            "profile_id": null
          },
          {
            "slot": "auxiliary",
            "agent_type": "grok",
            "profile_id": null
          }
        ]
      }
    }
  ]
}
```

Task indices match the Plan headings exactly. Generations are contiguous,
strictly increasing, and cover Task ranges without rewriting prior ranges.
Normal and high routes are derived rather than freely chosen.

Plan Task bodies retain concise human-readable risk reasoning and evidence for
review. There is no second Markdown routing matrix; avoiding duplicate
machine-readable sources prevents drift. The Plan does not hand-author an
orchestration fingerprint. The validator derives it from the accepted routing
block so the parent never has a second hashing implementation.

### Generic orchestration binding v1

`delegate_to_agent` and `continue_delegation` accept an optional
`orchestration_binding`. The exact v1 wire shape is:

```json
{
  "schema_version": 1,
  "namespace": "brainstorm-to-delivery",
  "generation": 2,
  "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
}
```

The object is all-or-none and rejects unknown fields. `schema_version` is the
integer `1`. `namespace` is an ASCII string matching
`^[a-z][a-z0-9-]{0,63}$`; this workflow requires the exact value
`brainstorm-to-delivery`. `generation` is an integer in
`1..=4294967295`, matching the Plan's positive unsigned 32-bit Task Agent
generation. `route_fingerprint` must match
`^sha256:[0-9a-f]{64}$` exactly. Empty strings, uppercase hex, numeric strings,
partial objects, additional properties, and out-of-range numbers are
malformed.

The database representation uses four new nullable columns on
`delegation_task_runs`:

```text
orchestration_schema_version INTEGER NULL
orchestration_namespace TEXT NULL
orchestration_generation INTEGER NULL
orchestration_route_fingerprint TEXT NULL
```

All four columns are null for an unbound legacy/ad-hoc run or all four contain
one valid binding. They are separate from the existing ACP/config
`route_fingerprint`. The migration performs no backfill. A database trigger
rejects any post-insert change to any of the four columns, including clearing
or adding a binding; run-store APIs also omit them from every update model.

For a routed Task, the Node validator constructs the following exact
positional JSON value after validating the Plan route and deriving every key:

```json
[
  "codeg-b2d-route-binding-v1",
  1,
  "brainstorm-to-delivery",
  7,
  2,
  "high",
  ["codex", null],
  [
    ["primary", "codex", null],
    ["auxiliary", "grok", null]
  ],
  [
    "task|7|implementer|codex|none",
    "task|7|reviewer|primary|codex|none",
    "task|7|reviewer|auxiliary|grok|none"
  ]
]
```

The positions are, in order: domain/schema tag, schema version, namespace,
Task index, Task Agent generation, risk level, implementer Agent/profile,
ordered reviewer tuples, and ordered expected work-unit keys. Reviewer order
is always primary then auxiliary; normal Tasks omit the auxiliary tuple and
key. Profiles use JSON `null` when absent, while work-unit keys retain their
canonical `none` token. Agent/profile values and keys are the exact validated
wire strings, including their exact Unicode scalar values; visually equivalent
composed and decomposed profile IDs remain distinct route identities. The array
is serialized as RFC 8785 JSON Canonicalization Scheme JSON with no
insignificant whitespace, encoded as UTF-8, hashed with SHA-256, encoded as 64
lowercase hexadecimal characters, and prefixed with `sha256:`. Implementations
must use the published cross-language vectors; they may not approximate the
encoding with delimiter joins or independently selected object-key order.

The resulting Task binding is:

```json
{
  "schema_version": 1,
  "namespace": "brainstorm-to-delivery",
  "generation": 2,
  "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
}
```

Every implementer and reviewer run for that Task uses the same binding because
the fingerprint commits to the entire route and all its work-unit keys. The
validator emits this object; the parent copies that exact object into the tool
call and progress. The parent and Plan Author do not recompute the hash.

For a first `delegate_to_agent`, binding validation occurs before depth,
recovery-budget, child allocation, spawn, or resume work. The binding is part
of `ReservingRunInsert` and all four columns are written by the same
transaction that claims the run in `reserving`. A failure rolls back the whole
reservation; there is no post-spawn attachment window.

For `continue_delegation`, the source row is loaded under the existing
admission transaction. An omitted binding inherits the source binding. A
supplied binding must equal the source in all four fields. A bound source can
never continue unbound, and an unbound source cannot acquire a binding through
continuation. Replacement admission applies the same rule to the replaced
lineage before replacement eligibility, authorization consumption, budget
preflight/charge, child allocation, or spawn. The replacement row copies the
source binding even when the caller supplies the matching object. These rules
apply to every continuation/replacement generation and preserve the existing
stable lineage and recovery budgets.

Transport parsing returns `orchestration_binding_invalid` for malformed or
partial input. Continue/replacement admission returns
`orchestration_binding_lineage_mismatch` when a well-formed explicit binding
does not exactly match the source or attempts bound/unbound conversion. Both
are stable typed errors. Neither path performs child side effects or consumes
recovery authorization/budget.

The effective binding also participates in idempotency. For an unbound request,
the current seven-string request-fingerprint tuple remains unchanged for old
clients and stored replay rows. For a bound request, the request fingerprint is
SHA-256 over deterministic JSON for this exact string array:

```json
[
  "delegation-request-v2",
  "delegate_to_agent",
  "Implement Task 7 from the reviewed Plan.",
  "task|7|implementer|codex|none",
  "",
  "",
  "",
  "5ea0c72cf8b44015a7fe8e796a05dc22",
  "1",
  "brainstorm-to-delivery",
  "2",
  "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
]
```

After the v2 domain tag, positions retain the existing order: tool, NFC task
text, work-unit key, replaced Task ID, replacement reason, continuation target
Task ID, and lowercase ACP route fingerprint. The final four strings are the
orchestration schema version, namespace, decimal generation, and route
fingerprint. Continue and replacement fingerprint the inherited effective
binding after source comparison, even when the caller omitted it. Therefore
two otherwise identical calls with different orchestration bindings cannot
alias, while an exact retry remains idempotent. Rust serializes this fixed
string array with `serde_json::to_vec`, hashes the resulting UTF-8 bytes, and
stores the existing unprefixed lowercase hexadecimal request digest.

### Progress contract

The existing `codeg-simple-progress-v1` marker and schema remain compatible
with current Simple projection. Each Task gains additive routing context:

- `risk_level`;
- `task_agent_generation`;
- the validator-derived `route_fingerprint`;
- the expected implementer and reviewer-slot work-unit keys; and
- the existing actual `runs` list.

A routed high-Task entry and its admitted implementer use this additive shape:

```json
{
  "index": 7,
  "status": "in_progress",
  "risk_level": "high",
  "task_agent_generation": 2,
  "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a",
  "expected_work_unit_keys": {
    "implementer": "task|7|implementer|codex|none",
    "reviewers": {
      "primary": "task|7|reviewer|primary|codex|none",
      "auxiliary": "task|7|reviewer|auxiliary|grok|none"
    }
  },
  "runs": [
    {
      "role": "implementer",
      "agent_type": "codex",
      "profile_id": null,
      "state": "running",
      "work_unit_key": "task|7|implementer|codex|none",
      "task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
      "child_conversation_id": 931,
      "task_agent_generation": 2,
      "orchestration_binding": {
        "schema_version": 1,
        "namespace": "brainstorm-to-delivery",
        "generation": 2,
        "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
      }
    }
  ]
}
```

Every routed run intent mirrors the exact `orchestration_binding`; once the run
is admitted, its mirror also records `task_id`, `child_conversation_id`,
canonical `work_unit_key`, Agent/profile, role/slot, and current run state. A
pre-reservation failure may retain an intent with no Task or child ID, but it
is not treated as durable admission. A row in any durable status is admission.

The Plan block is authoritative for planned classification and routes. Progress
mirrors the effective route so the parent can reconcile intent with actual
runs after compaction. The durable row is authoritative for historical
admission identity. The validator rejects Plan/progress disagreement and any
progress/durable absence or mismatch. Mutable progress cannot erase admission:
a bound durable row missing from progress is an error, not evidence that the
Task is pending. The platform parser may project disagreement as warning state;
it never turns the routing metadata into a workflow Gate.

The existing Plan, progress-block, and progress-document size limits remain in
force. A bounded routing-block limit is added and must fit within the Plan's
existing 2 MiB limit.

### Durable binding query and snapshot

The MCP companion adds the exact read-only tool name
`get_delegation_orchestration_bindings`. Parent identity comes from the MCP
token and is never accepted as input, so one parent cannot enumerate another
parent's runs. A first-page input is:

```json
{
  "namespace": "brainstorm-to-delivery",
  "limit": 100
}
```

A later-page input is:

```json
{
  "namespace": "brainstorm-to-delivery",
  "limit": 100,
  "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
  "cursor": "page-100"
}
```

`namespace` is required and uses the binding namespace grammar. `limit` is
optional, defaults to 100, and is limited to `1..=200`. The first call omits
both `snapshot_id` and `cursor`; every later page supplies both values from the
previous response. Supplying only one, changing namespace or limit mid-scan,
or using a token from another parent is a typed error. Repeating the same
snapshot/cursor request before expiry idempotently returns the same page, so a
lost response is recoverable; the validator still rejects duplicate pages in
the assembled evidence file.

The input object rejects additional properties. `snapshot_id` is a
server-minted lowercase UUID string. `cursor` is an opaque base64url string of
1 to 128 ASCII characters bound to parent, namespace, snapshot, and offset;
callers never construct it. `snapshot_revision` in responses is an unsigned
64-bit counter serialized as a 1-to-20 digit decimal string so Node never loses
integer precision. Snapshot timestamps are UTC RFC 3339 strings.

At the first call, the backend reads one parent-scoped database snapshot and
materializes at most 4096 rows. The selected rows are (a) every row whose
orchestration namespace equals the requested namespace and (b) every unbound
row with a non-null `work_unit_key`. Rows bound to another namespace are
excluded. Including unbound orchestrated rows lets the validator detect a
routed legacy run that cannot retroactively gain evidence. The stable order is
`(created_at, task_id)`. More than 4096 selected rows fails without returning a
partial snapshot.

The in-process snapshot has a 60-second expiry and captures a parent-scoped
binding-evidence revision. Every parent run insert and durable status
transition increments that revision. If the revision changes between pages, the
token expires, the process restarts, or 60 seconds elapse, pagination returns
`orchestration_binding_snapshot_stale`; the parent discards every collected
page and restarts at page one. It never combines pages from different
snapshots. This produces one stable view even while statuses or run sets are
changing.

Malformed cursor/snapshot requests return
`orchestration_binding_query_invalid`; row-cap overflow returns
`orchestration_binding_query_too_large`; database/materialization failures
return `orchestration_binding_query_failed`; and expired, restarted, or
revision-invalidated scans return `orchestration_binding_snapshot_stale`.
Every error returns no partial page.

Each successful page has this envelope:

```json
{
  "schema_version": 1,
  "namespace": "brainstorm-to-delivery",
  "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
  "snapshot_revision": "42",
  "snapshot_created_at": "2026-08-17T08:00:00Z",
  "snapshot_expires_at": "2026-08-17T08:01:00Z",
  "total_rows": 1,
  "page_start": 0,
  "runs": [
    {
      "task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
      "root_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
      "previous_task_id": null,
      "lineage_root_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
      "replaced_task_id": null,
      "generic_generation": 1,
      "work_unit_key": "task|7|implementer|codex|none",
      "child_conversation_id": 931,
      "status": "running",
      "orchestration_binding": {
        "schema_version": 1,
        "namespace": "brainstorm-to-delivery",
        "generation": 2,
        "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
      }
    }
  ],
  "next_cursor": null,
  "complete": true
}
```

The identity fields are exact durable values. Status is the durable
`reserving`, `running`, `completed`, `failed`, or `canceled` value at snapshot
creation. `orchestration_binding` is null only for an unbound selected row.
The response never returns Agent prompt text, task preview, result text,
termination details, card summaries, profile configuration, or child output.

The parent preserves the raw page envelopes in an OS-temporary evidence JSON
file with this exact wrapper:

```json
{
  "schema_version": 1,
  "pages": [
    {
      "schema_version": 1,
      "namespace": "brainstorm-to-delivery",
      "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
      "snapshot_revision": "42",
      "snapshot_created_at": "2026-08-17T08:00:00Z",
      "snapshot_expires_at": "2026-08-17T08:01:00Z",
      "total_rows": 0,
      "page_start": 0,
      "runs": [],
      "next_cursor": null,
      "complete": true
    }
  ]
}
```

The evidence file is capped at 4 MiB. The validator requires identical
snapshot metadata on all pages, first `page_start` zero, contiguous
non-overlapping ranges, exact cursor chaining, `runs.length` totals matching
`total_rows`, and exactly one final page with `complete: true` and
`next_cursor: null`. Missing, duplicate, reordered, mixed, expired, or
truncated pages fail. The temporary file is not a workflow manifest or durable
workflow artifact and is removed after validation.

### Validator modes and durable reconciliation

The validator retains two contract-only modes for repository tests and author
feedback:

- no arguments validates only `SKILL.md`;
- `--plan FILE --progress FILE --plan-rel-path REL_PATH` validates static
  Skill/Plan/progress structure and derives route bindings, but reports
  `admission_authorized: false` because no durable evidence was checked. It may
  add `--output-json` to receive the derived bindings in structured output for
  progress initialization; without that flag it retains the existing readable
  PASS/FAIL output.

Before a reviewed routing block and synchronized progress exist, document
dispatches use this deterministic pre-route admission mode:

```text
validate-contract.mjs --document-admission \
  --durable-evidence FILE --output-json
```

It validates the Skill and complete durable page set and authorizes only an
unbound Design/Plan work unit when there is no bound or unbound recognized Task
row. It does not derive a Task binding. Once a reviewed Plan and synchronized
progress exist, every document, Task, final-review, continuation, recovery, and
selection-change decision uses the full admission mode below with Skill, Plan,
progress, and durable evidence together.

Full routed execution uses this exact mode:

```text
validate-contract.mjs --plan FILE --progress FILE \
  --plan-rel-path REL_PATH --admission \
  --durable-evidence FILE --output-json
```

`--admission` requires both `--durable-evidence` and `--output-json`. Successful
JSON has this exact success shape:

```json
{
  "schema_version": 1,
  "admission_authorized": true,
  "durable_snapshot": {
    "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
    "snapshot_revision": "42"
  },
  "task_bindings": [
    {
      "task_index": 7,
      "orchestration_binding": {
        "schema_version": 1,
        "namespace": "brainstorm-to-delivery",
        "generation": 2,
        "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
      }
    }
  ],
  "failures": []
}
```

Static JSON uses the same shape with `admission_authorized: false`,
`durable_snapshot: null`, and the derived `task_bindings`. Admission failure
exits nonzero, emits `admission_authorized: false`, emits no usable
`task_bindings`, and returns `{rule_id, message}` objects in `failures`. The
Skill selects the active Task entry and copies the binding field-for-field
without reserialization assumptions or recomputation; it never hashes the Plan
itself.

Full reconciliation constructs maps by durable `task_id` and progress
`task_id` and enforces both directions:

1. Every admitted progress run has exactly one durable row, and every bound
   durable row in this namespace has exactly one progress mirror.
2. Task ID, canonical work-unit key, child conversation ID, replacement link,
   and current status agree; durable root/previous/lineage identities are also
   internally consistent. Progress `cancelled` normalizes to durable
   `canceled`; `stalled` may match only durable `running`; `unknown` never
   authorizes admission. If identity and binding already match and only a
   legitimate durable lifecycle transition is newer, the parent first updates
   the progress state from the query and reruns validation; it never rewrites
   identity or binding to make a mismatch pass.
3. The work-unit key belongs to the indexed Plan Task and to its exact derived
   implementer/reviewer set. Role, reviewer slot, Agent, and profile encoded in
   the key agree with progress and Plan.
4. Durable binding schema/namespace, Task Agent generation, and fingerprint
   equal the validator-derived binding for that Plan Task and the progress
   mirror.
5. A selected unbound row whose work-unit key is a recognized Task key for the
   routed Plan blocks as unverifiable. A bound row with no Plan Task or no
   progress mirror is an unexpected extra row and also blocks.
6. A new Task Agent generation is legal only for a pending Task suffix with no
   durable row in any status. Historical bindings remain byte-for-byte
   unchanged.

The stable durable rule families are:

| Rule ID | Condition |
| --- | --- |
| `B2D-DURABLE-001` | Evidence envelope/page set is malformed, incomplete, stale, oversized, or mixed |
| `B2D-DURABLE-002` | Binding format, namespace, or canonical fingerprint is invalid |
| `B2D-DURABLE-003` | An admitted progress run has no durable row |
| `B2D-DURABLE-004` | A bound durable row is missing from progress or is outside the Plan |
| `B2D-DURABLE-005` | Task, work-unit, child, lineage, or status identity disagrees |
| `B2D-DURABLE-006` | Durable and derived generation/fingerprint disagree |
| `B2D-DURABLE-007` | A routed Plan Task has an admitted unbound run |
| `B2D-DURABLE-008` | A generation change touches a durably admitted Task |

These validator failures are execution blockers even though any similarly
named Rust Simple projection warning remains informational.

## Canonical Work-Unit Keys

New work uses these keys:

```text
design|{design_rel_path}|fixer|codex|{profile_or_none}
design|{design_rel_path}|reviewer|{agent}|{profile_or_none}
plan|{plan_rel_path}|author|codex|{profile_or_none}
plan|{plan_rel_path}|reviewer|{agent}|{profile_or_none}
task|{index}|implementer|{agent}|{profile_or_none}
task|{index}|reviewer|primary|{agent}|{profile_or_none}
task|{index}|reviewer|auxiliary|{agent}|{profile_or_none}
final_review|reviewer|codex|{profile_or_none}
```

The reviewer slot is part of identity. It allows primary and auxiliary
reviewers to use the same Agent type and profile without key collision.

Existing five-part Task reviewer keys remain recognized as legacy primary
reviewer keys so historical Simple runs and archived projections remain
readable. New Skill runs always emit the explicit six-part reviewer key.

Lineage validation groups runs by complete work-unit key, not by the generic
`reviewer` role. The role remains `reviewer` in generic run metadata; the key
provides the stable slot identity.

## Execution Flow

### Establish current truth

The parent reads repository instructions, Brainstorm, relevant code and tests,
recent commits, user changes, live delegation schemas, and available Agents.
It verifies that `get_delegation_orchestration_bindings` is available and can
return a complete snapshot, then runs document-admission validation before
document work starts and resolves the initial Task Agent. Before a routed Plan
exists, a new workflow requires the namespace snapshot to contain no bound or
unbound recognized Task row; an existing Task row without recovery documents
blocks rather than being ignored.

### Review and revise Design

When a Design trigger is present, the parent records review intent and
obtains a complete binding snapshot, supplies it to document-admission
validation, and then dispatches the independent Design Reviewer. Valid
non-material findings are consolidated into the Design Fixer brief. Material
requirement, scope, architecture, or user-data changes pause for user decision
before the Fixer is continued. The parent obtains a fresh complete snapshot
and repeats document-admission validation before every Fixer continuation and
Design re-review. These document work units remain unbound because no Task
route is being admitted.

### Author and review Plan

The parent creates the initial progress document and records Plan Author
intent after a complete binding query and document-admission validation. The
independent Codex Plan Author invokes `writing-plans`, writes the Plan and
routing block, runs static validation, and reports the result. The independent
Plan Reviewer reviews task decomposition, risk evidence, routing,
verification, and repository fit. A fresh complete query and applicable
document/full admission validation precede the reviewer and every Author
continuation. These document runs remain unbound. Valid findings return to the
same Plan Author work unit.

After Plan approval, the parent registers the Simple descriptor and syncs all
Task entries into progress using the validator's exact derived generation and
route fingerprint. It then obtains a new complete snapshot and runs full
admission validation. Registration remains locator metadata rather than an
execution Gate.

### Execute Tasks serially

Immediately before each Task and before each individual delegation action
inside it, the parent applies the existing workspace gate, exhausts every page
of a fresh binding snapshot, and runs full Plan/progress/durable admission
validation. It records the run intent with the emitted binding and passes the
exact same object to `delegate_to_agent`, `continue_delegation`, or a
replacement call. After acknowledgement it records the returned Task and child
identities and durable status in progress. Any intervening delegation action
invalidates the prior snapshot, so the next action requeries.

For a normal Task, it dispatches or continues the Task Agent implementer,
checks the report and repository state, then dispatches the Codex primary
reviewer. Both use the Task's single derived binding.

For a high Task, it dispatches or continues the Codex implementer, checks the
report and repository state, records both review intents, dispatches primary
and auxiliary reviewers as separate work units, and joins both. The two review
admissions occur sequentially with a fresh query/validation between them; once
admitted, their child runs may execute concurrently. All three work units use
the same Task binding. The next Task cannot start until both reviews settle.

Critical and Important findings return in one adjudicated producer brief.
After a fix, every reviewer required by that Task route re-reviews the latest
result. Retained Minor findings require a recorded reason.

### Verify and deliver

After all Tasks pass their routes, the parent runs scope-appropriate test,
lint, build, and project checks. It obtains a fresh complete binding snapshot
and full validation before dispatching a fresh Codex final reviewer. The final
review work unit remains unbound because it is not a Plan Task route. Final
fixes return to owning Task producer work units with that Task's exact durable
binding and reopen the affected Task review plus final review. Delivery is
complete only from current repository evidence, covering verification,
durable reconciliation, and approved final review.

## Recovery and Error Handling

Every first run uses `delegate_to_agent`; later work on the same unit uses
`continue_delegation`. The existing limits remain two unexpected
continuations and one logical replacement per established lineage, with
pre-admission retries retaining current semantics. Routed Task continuations
and replacements inherit the source orchestration binding regardless of what
mutable documents currently claim.

After compaction, interruption, or resume, the parent re-reads the Design,
Plan, routing block, progress, reports, Git state, and live generic run state.
It obtains every page of a new durable binding snapshot before status polling,
continuation, replacement, recovery authorization, or another dispatch, then
synchronizes status-only lifecycle advances into progress and runs full
admission validation. `get_delegation_status` remains useful for task reports,
but cannot substitute for the binding query because it does not expose
work-unit or orchestration identity. Remembered routing and a retained report
are provisional until all sources agree.

The workflow blocks without substitution when:

- the selected Task Agent or profile is unavailable;
- the binding query/tool is unavailable or returns a DB error;
- pagination is incomplete, truncated, mixed, expired, stale, oversized, or
  otherwise not a complete snapshot;
- the routing block is absent, malformed, oversized, or inconsistent;
- risk signals, evidence, score, level, or derived route are invalid;
- an expected durable row is missing, an extra bound row exists, a routed run
  is unbound, or any Task/work-unit/child/status/generation/fingerprint differs;
- a producer and reviewer share a child conversation;
- a high Task lacks either reviewer slot;
- a Task Agent change touches an active, completed, or previously admitted
  Task;
- a review covers stale producer output;
- a requested active-Task Agent handoff cannot be represented safely; or
- generic continuation or replacement rails are exhausted.

The Skill never falls back to Plan/progress-only execution. It restarts a stale
query from page one, but a repeated query failure remains a blocker rather than
permission to continue. Plan/progress/durable-run mismatches may also produce
Simple projection warnings for display; those warnings do not authorize the
Skill to bypass an invalid route, generic identity rule, or recovery budget.

## Backend and Projection Changes

This design adds durable generic run identity and bounded read access, not a
new workflow state machine:

- `src-tauri/src/acp/delegation/types.rs` adds the strictly validated
  `OrchestrationBindingV1` to
  `DelegationRequest` and `ContinueDelegationRequest`, plus query request/page
  DTOs and stable malformed/mismatch error codes;
- `src-tauri/src/acp/delegation/tool_schema.json`, companion
  dispatch/rendering, and `src-tauri/src/acp/delegation/listener.rs` parsing
  expose the binding on both delegation tools and add
  `get_delegation_orchestration_bindings` with no caller-supplied parent ID;
- the broker validates first-dispatch bindings, resolves the effective source
  binding for continuation/replacement, and rejects mismatches before child or
  recovery side effects;
- `src-tauri/src/acp/delegation/run_store.rs` includes the four binding fields in
  `ReservingRunInsert`/`PersistedRun`, writes them in the reserving transaction,
  incorporates the effective binding into v2 request fingerprints, and never
  includes them in lifecycle update sets;
- the `src-tauri/src/db/entities/delegation_task_run.rs` SeaORM entity and new
  `src-tauri/src/db/migration/m20260817_000001_delegation_orchestration_bindings.rs`
  migration add four nullable columns, an all-or-none insert trigger named
  `trg_dtr_orchestration_binding_shape`, an update trigger named
  `trg_dtr_orchestration_binding_immutable`, and the index
  `(parent_conversation_id, orchestration_namespace, created_at, task_id)`
  without guessed backfill;
- the parent-scoped query materializes bounded, revision-stable pages and
  returns only run identity, status, and binding;
- existing Simple parsing still recognizes Design Fixer keys, explicit primary
  and auxiliary reviewer keys, and legacy five-part primary reviewer keys; and
- projection may add bounded warning codes
  `simple_orchestration_binding_missing`,
  `simple_orchestration_binding_mismatch`, and
  `simple_orchestration_binding_orphan`, while preserving separate high-Task
  nodes and warning-only sync state.

The migration changes an existing generic run table but introduces no new
database table, workflow header, manifest revision, Gate cycle, gate
settlement, completion Card/evidence row, or platform completion decision.
Rust Simple projection observes and warns; it does not authorize or reject a
Task route. Generic transport may reject malformed input or a source-lineage
mismatch because those are run identity violations, not Simple platform Gates.

## Skill and Validator Changes

`.agents/skills/brainstorm-to-delivery/SKILL.md` will be revised to:

- resolve the Task Agent from the invocation with Grok default;
- dispatch independent Design Fixer, Plan Author, and document reviewers;
- prohibit parent Design and Plan edits;
- require `b2d_task_risk_v1` evidence and derived routes;
- call `get_delegation_orchestration_bindings` to exhaustion before every
  dispatch, continuation, recovery, compaction resume, and selection change;
- treat query unavailability, stale/incomplete evidence, and every durable
  mismatch as blocking without Plan/progress fallback;
- consume the validator's exact binding output for routed Task calls and
  progress mirrors rather than independently hashing;
- execute normal and high routes exactly;
- support boundary-only Task Agent changes;
- make both high reviewers stale after every producer mutation;
- preserve generic recovery and workspace safety; and
- avoid every workflow-v2 mutation interface.

The JavaScript validator will add deterministic parsers and checks for:

- Skill contract v2;
- the Plan routing block and size bound;
- Task Agent generations;
- hard and soft risk arithmetic;
- normal/high route derivation;
- RFC 8785 canonical route fingerprint derivation and JSON output;
- strict binding and durable-snapshot parsing with byte/row/page bounds;
- primary/auxiliary reviewer keys;
- Plan/progress/durable agreement in both directions;
- per-key lineage stability;
- producer/reviewer conversation independence when IDs are known; and
- boundary-only changes limited to never-admitted pending Tasks.

Static Skill-only and document-only entry points remain usable without a live
database, but their output explicitly cannot authorize execution. Only
document-admission or full `--admission` with a complete current durable
snapshot can return `admission_authorized: true`.

## Testing

### Binding and validator tests

- Shared JSON fixtures exercise valid minimum/maximum generations, namespace
  boundaries, and exact lowercase fingerprints across MCP JSON Schema,
  listener deserialization, Rust validation, and Node validation. Negative
  vectors cover missing/extra fields, partial bindings, wrong types, zero and
  overflow generation, empty/oversized/invalid namespace, uppercase or
  wrong-length hex, and invalid prefix.
- The canonical high-Task vector in this Design hashes to
  `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`
  in Node and the independent Rust test helper. Normal/high vectors, exact
  Unicode strings, composed/decomposed profile distinctions,
  null/profile values, custom Agents, and maximum Task indices prove
  deterministic output. Reordering reviewer slots or work-unit keys, changing
  risk, Agent/profile, Task index, or generation must change the digest;
  irrelevant JSON object insertion order cannot.
- Static Skill-only and document-only CLI modes remain usable and explicitly
  return `admission_authorized: false`; admission mode rejects a missing,
  expired, malformed, oversized, mixed, non-contiguous, or incomplete durable
  snapshot.
- Document-admission mode authorizes unbound Design/Plan work only with a
  complete empty Task evidence set and rejects either a bound or unbound
  recognized Task row before reviewed route documents exist.
- Durable reconciliation has strict negatives for a missing/deleted progress
  mirror, fabricated Task ID, wrong child conversation, wrong work-unit key,
  wrong current status, wrong generation, wrong fingerprint, duplicate or
  unexpected extra durable row, and a routed admitted run whose durable
  binding is null.
- The exact root-cause regression rewrites the Plan Task generation/route,
  rewrites progress Task generation/fingerprint and every mirrored run
  binding, and keeps the retained admitted high-Task Codex implementer durable
  row at its original generation/fingerprint. Validation must fail with a
  `B2D-DURABLE-*` identity rule even though the generation-invariant Codex
  implementer work-unit key still matches.
- Legal controls append a generation only for a never-admitted pending suffix,
  keep all earlier durable rows unchanged, and pass. A reserving, failed,
  canceled, running, or completed row on any affected Task makes the same
  change fail.
- Existing contract cases retain Grok defaulting, all supported built-in and
  valid `custom:*` selection, no silent fallback, producer/reviewer
  independence, risk arithmetic, exact normal/high routes, explicit reviewer
  slots, legacy five-part primary readability, and Plan/progress checks.

### Rust transport, store, query, and projection tests

- Migration/entity round trips cover a valid binding, all-null legacy rows,
  all-or-none rejection, no guessed backfill, exact separation from ACP
  `route_fingerprint`, lookup indexing, and the database immutability trigger.
- MCP schema and listener tests accept valid optional bindings on delegate and
  continue, reject malformed values with `orchestration_binding_invalid`, and
  expose the exact query tool. Parent isolation proves a token cannot supply or
  infer another parent ID.
- First dispatch persists the binding in the same reserving transaction before
  spawn. Transaction rollback leaves no partial binding/run, and fault
  injection proves no post-insert lifecycle/status path can mutate the four
  fields.
- Bound request fingerprints separate different generation/fingerprint values,
  preserve exact idempotent replay, use the effective inherited binding, and
  leave the existing unbound seven-field fingerprint behavior unchanged.
- Continue inherits when omitted, accepts an exact explicit match, and rejects
  mismatch or bound/unbound conversion before resume, child side effects,
  authorization use, or budget consumption. Replacement has the same cases,
  copies the replaced lineage binding, and rejects before eligibility side
  effects or recovery-budget consumption.
- Query tests cover default/min/max page size, stable ordering, multiple pages,
  exact cursor chaining, total-row accounting, namespace filtering, inclusion
  of unbound work-unit rows, revision change, expiry/restart staleness,
  truncation/row-cap/DB failure, and retry from page one. Serialized pages are
  asserted not to contain prompt, preview, output, result, completion evidence,
  profile config, or termination detail.
- Simple key/parser/projection tests retain Design Fixer and slotted reviewer
  round trips, legacy primary parsing, same-profile Codex reviewer separation,
  invalid-key rejection, high producer plus both reviewer nodes, no workflow
  header, bounded binding warnings, and warning-only sync state.

### Workflow scenarios

1. No override runs a normal Task with Grok implementation, the shared Task
   binding, and independent Codex review.
2. A selected non-Grok Task Agent runs the complete normal route; a high Task
   still forces Codex implementation plus Codex primary and Task Agent
   auxiliary review.
3. Task Agent Codex creates three independent high-Task conversations with one
   shared binding and distinct canonical keys. A producer fix makes both
   reviewers stale and both re-review.
4. After a high Task's Codex producer has a reserving durable row under Grok's
   auxiliary route, a coordinated Plan/progress attempt to switch that Task to
   another Task Agent is blocked before reviewer dispatch.
5. A boundary change after completed prior Tasks updates only never-admitted
   pending Tasks, while an active-Task switch blocks or defers without a
   handoff.
6. Deleted progress mirrors, fabricated identities, unbound routed history,
   stale pagination, unavailable query, compaction, continuation, replacement,
   and exhausted recovery rails all fail closed or preserve the original
   binding as specified.
7. Conditional Design review keeps separate Codex Reviewer/Fixer sessions;
   Plan authoring and revisions keep one independent Plan Author session using
   `writing-plans`; document runs remain outside Task binding admission.
8. Final findings return to the correct normal or high producer, preserve its
   Task binding, and reopen covering Task reviews plus final review.
9. A legacy Simple workflow without `codeg-b2d-routing-v1` retains legacy
   behavior. Adding the routing block while an admitted matching run is
   unbound blocks rather than inventing evidence.
10. Projection remains warning-only, and no scenario creates a manifest,
    platform Gate, gate settlement, completion Card/evidence, or platform
    completion decision.

### Verification commands

Implementation verification includes the focused Skill validator suite, its
production-file check, cross-language vectors, Rust query/store/listener/
projection tests, and brainstorm-to-delivery integration scenarios. Every Rust
compile, test, or lint command for this work disables default Tauri features
and enables exactly `server,test-utils`, for example:

```bash
cd src-tauri
cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp
cargo test --no-default-features --features server,test-utils durable_binding
cargo test --no-default-features --features server,test-utils simple_workflow
cargo clippy --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp -- -D warnings
```

No verification command for this change enables the default `tauri-runtime`.

## Compatibility and Rollout

Existing Simple workflows continue to parse their progress-v1 files and
five-part reviewer keys. Migration columns are nullable and no historical row
is assigned a guessed orchestration namespace, generation, or fingerprint.
Standalone/ad-hoc delegation and old clients may omit the optional binding and
retain the existing request-fingerprint behavior.

A legacy Simple workflow without `codeg-b2d-routing-v1` keeps its recorded
legacy behavior and treats binding projection as informational. Adopting the
new routing block requires a Plan Author revision before the next pending Task
and full durable validation. If any matching Task run was already admitted
without a binding, the workflow cannot retroactively manufacture evidence and
blocks for user resolution. Missing binding is not itself a Rust Simple Gate;
it becomes an execution blocker only under the revised routed Skill contract.

Archived manifest workflows remain read-only and retain their current
projection. This design does not revive or convert them in place.

The change ships atomically across migration/entity, generic request and query
surfaces, broker/run store, repository Skill, validator, canonical key parser,
Simple projection, and integration contracts. A revised Skill running against
a backend without the query or binding schema fails closed; it never falls
back to mutable documents. `get_delegation_status` remains wire-compatible but
is not extended into the binding evidence surface, avoiding prompt/output
leakage and preserving its current task-report contract.

## Success Criteria

- Grok is a default Task Agent selection, not a hard-coded implementation
  role.
- A user can select another Task Agent from the invocation message.
- A Task Agent can change between Tasks without altering earlier lineages.
- Design and Plan producers never review their own artifacts.
- The parent orchestrator does not edit Design, Plan, or Task code.
- Every Task has a validated `b2d_task_risk_v1` classification and derived
  route before admission, and the parent uses the validator's exact binding.
- High risk always forces Codex implementation and two independent reviewers.
- Same-Agent reviewer combinations cannot collide in work-unit identity.
- A reserving transaction durably fixes the complete Task route generation and
  fingerprint before child side effects; no status/continuation/replacement
  path can mutate it.
- Complete parent-scoped durable evidence detects missing mirrors, extra rows,
  identity changes, unbound routed history, and coordinated Plan/progress
  rewrites before execution or recovery.
- Legal boundary changes affect only never-admitted pending Tasks and preserve
  every historical binding and recovery lineage.
- Existing unbound generic delegation and routing-block-free legacy Simple
  workflows remain compatible without guessed backfill.
- Simple remains manifest-free, platform-gate-free, and recoverable through
  Plan, progress, Git, reports, and generic delegation state. Rust projection
  remains warning-only and owns no completion or admission decision.
