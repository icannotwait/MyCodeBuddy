# Delegation Admission Context Optimization Design

## Status

Proposed on 2026-08-26. The selected direction is a staged migration:

1. move complete durable binding evidence out of the model context into an
   opaque local artifact; and
2. add an atomic admission ticket for protocol-participating delegation
   calls.

Plan/progress semantics remain owned by the Brainstorm-to-Delivery Skill and
its Node validator. Codeg remains authoritative for durable run identity,
lineage, status, uniqueness, recovery budgets, and run admission. This design
does not move Markdown parsing or workflow policy into Rust.

Phase 1 and Phase 2 are independently releasable. Phase 1 is the required
context fix. Phase 2 closes the remaining check-to-dispatch race after Phase 1
has passed its compatibility and payload benchmarks.

## Executive Decision

The existing `get_delegation_orchestration_bindings` page API remains intact.
It gains an opt-in `delivery: "artifact"` mode. In that mode, `codeg-mcp`
exhausts the existing broker pagination internally, writes the exact existing
durable-evidence wrapper to a private temporary file, and returns only a small
artifact descriptor. The model passes the path and SHA-256 digest directly to
the validator and never reads the file.

Phase 2 extends the same artifact request with an optional
`admission_intent`. The broker creates a short-lived, parent-scoped ticket
bound to the snapshot revision and the exact durable identity of the intended
operation. `delegate_to_agent` or `continue_delegation` consumes that ticket
inside the existing run-store mutation gate and reserving transaction.

The selected design intentionally does not add a second snapshot or admission
tool. Extending the existing tool keeps the Grok `tools/list` catalog smaller
and keeps one model-visible evidence call per admission attempt.

## Incident Evidence

The measured session is Codeg session 4123, "Interactive display
configuration design parallel review".

| Signal | Observed value |
| --- | ---: |
| Grok context window | 500,000 tokens |
| Compaction threshold | 60%, approximately 300,000 tokens |
| Compactions | 10 |
| Top-level turns | 11 |
| Model loops | approximately 1,440 |
| Tool calls | 2,187 |
| MCP calls | 979 |
| Binding-query calls | 795 |
| First-page scans | 125 |
| Continuation pages | 670 |
| Binding-query result bytes retained in history | approximately 2.9 MiB |
| Share of measured dynamic history | approximately 29% |

At inspection time the durable corpus contained 72 rows across 34 work-unit
keys. The raw row JSON was only approximately 42 KiB, or 582 bytes per row on
average. The corpus required approximately 12 model-visible pages because the
companion must keep each duplicated text/structured MCP result under Grok's
7,680-byte JSONL budget.

The large context cost therefore does not come from one unusually large
snapshot. It comes from repeatedly replaying the entire append-only corpus
through the model before each orchestration action. If the number of durable
rows grows with completed actions, repeated full scans approach quadratic
cumulative evidence volume.

The existing 7,680-byte budget solves transport framing for one response. It
does not solve cumulative model history growth.

## Problem Statement

The current safety contract correctly requires a complete fresh durable
binding snapshot before dispatch, continuation, recovery, compaction resume,
or route selection changes. The Node validator needs every selected row to
perform bidirectional Plan/progress/durable reconciliation and exact lost-ACK
adoption.

The inefficient boundary is that every page crosses the MCP/model boundary.
The model does not reason about those rows; it only copies them into an
evidence file consumed by deterministic code. Each page is nevertheless
retained in conversation history, often in both text and structured form.

Reducing page size, increasing page size, shortening row field names, or using
delta pages would alter the multiplier but would leave raw durable state in
the model context. The data should bypass the model entirely.

## Goals

- Reduce one complete binding scan to one model-visible MCP result.
- Keep the final serialized artifact-mode JSON-RPC line, including request ID
  and trailing newline, at or below 2,048 UTF-8 bytes.
- Return no durable run row, page cursor, prompt, report, or child output in an
  artifact-mode MCP result.
- Preserve the exact selected conflict set, ordering, snapshot revision,
  60-second expiry, 4,096-row cap, 4 MiB evidence cap, and validator decisions.
- Preserve the current page API byte-for-byte for legacy callers.
- Reduce binding-query result bytes by at least 90% in a replay of session
  4123 while retaining the same number of safety checks.
- In Phase 2, reject a dispatch if durable state changed after evidence was
  captured and before reservation.
- Make one logical dispatch idempotent across a lost MCP acknowledgement while
  preserving the existing requirement for a fresh call `correlation_id` on
  every physical retry.
- Work in desktop and server/Docker modes without frontend changes.
- Add no dependency; reuse the existing `tempfile`, `sha2`, `uuid`, Serde,
  snapshot cache, mutation gate, and run-store transaction machinery.

## Non-Goals

- Changing Grok's 500,000-token window or 60% compaction threshold.
- Adding ACP Goal support or using Goal state as workflow admission evidence.
- Reducing the number of safety checkpoints required by Brainstorm-to-Delivery.
- Replacing Plan/progress Markdown with a database workflow state machine.
- Porting the Node validator or its routing/risk policy into Rust.
- Replacing the existing inline page API for diagnostic or legacy callers.
- Sending artifact contents through another MCP resource or tool.
- Using delta evidence as an admission authority.
- Making tickets durable across a Codeg process restart. A restart requires a
  fresh artifact, validation, and ticket.
- Globally requiring tickets from old Skills. Ticket enforcement applies to
  calls that opt into `dispatch_intent_id`; a future binding-schema migration
  may make the protocol universal.
- Changing generic delegation correlation, recovery authorization, continuation
  budgets, replacement budgets, or provisional-child cleanup semantics.

## Authority Model

| Concern | Authority after this design |
| --- | --- |
| Planned route, risk, expected keys | Plan plus Node validator |
| Progress mirror and unresolved intent | Progress plus Node validator |
| Durable run identity, status, lineage | Codeg run store |
| Complete evidence selection and revision | Codeg snapshot cache |
| Evidence transport | `codeg-mcp` artifact exporter |
| Semantic reconciliation | Node validator |
| Atomic durable admission | Codeg run store with ticket in Phase 2 |
| Child process/session lifecycle | Existing broker |
| User-visible workflow projection | Existing Simple projection |

An artifact proves what bytes Codeg exported for one durable snapshot. A
validator success proves that those bytes agree with Plan and progress. An
admission ticket proves only that the durable revision and intended run
identity are still admissible. None of these proofs substitutes for another.

## System Invariants

1. Raw binding rows never appear in an artifact-mode MCP result.
2. The artifact contains the same complete ordered run corpus that a correct
   legacy caller would have collected from one snapshot. Page boundaries may
   follow the artifact request's limit.
3. The validator hashes and parses one in-memory byte buffer. It never hashes
   one read and parses a later read.
4. Any parent run mutation invalidates the snapshot and every unconsumed ticket
   issued from it.
5. An expired, incomplete, stale, oversized, or digest-mismatched artifact
   never authorizes an action.
6. A ticket is parent-scoped, snapshot-scoped, intent-scoped, short-lived, and
   single-use.
7. Ticket validation and durable run reservation occur under the same existing
   parent mutation fence. Child startup happens only after reservation.
8. `dispatch_intent_id` identifies one logical operation and is stable across
   its physical retries. `correlation_id` identifies one physical tool call,
   remains transport-only, and is fresh on every retry.
9. A matching persisted dispatch intent may return the existing run. The same
   intent ID with different semantic input fails closed.
10. Ticket admission does not approve Plan/progress semantics and does not
    bypass recovery authorization or budget checks.
11. Legacy page requests and delegation requests without ticket fields retain
    current behavior.
12. Artifacts contain run identity only. They never contain task prompts,
    reports, assistant messages, or child output.

## Approaches Considered

### Smaller or delta model-visible snapshots

Rejected as the primary design. Field compaction and larger pages reduce call
count, while delta pages reduce repeated bytes in steady state, but both keep
operational data in model history. Delta state also introduces baseline,
ordering, restart, and completeness proofs into the Skill.

### Artifact export only

Selected for Phase 1. It removes the measured context multiplier with the
smallest behavioral change and reuses the current validator and page format.
It does not close the revision change between validator completion and run
reservation.

### Full backend reconciliation

Rejected. Rust would need to parse evolving Plan/progress formats and duplicate
the large Node policy engine. This would create two semantic authorities and a
cross-language compatibility problem without improving the Phase 1 context
result.

### Artifact export followed by an atomic ticket

Selected as the final architecture. Phase 1 delivers the immediate context
benefit. Phase 2 reuses the artifact revision and existing mutation gate to
close the check-to-dispatch race without moving Markdown policy into Codeg.

## Phase 1: Opaque Snapshot Artifact

### Public request contract

`get_delegation_orchestration_bindings` adds `delivery`:

```json
{
  "namespace": "brainstorm-to-delivery",
  "delivery": "artifact"
}
```

The accepted modes are:

- omitted or `"page"`: the exact current request and response behavior;
- `"artifact"`: a first-page request that the companion completes internally.

For `delivery: "artifact"`, `snapshot_id` and `cursor` are forbidden. `limit`
remains optional and validated as `1..=200`; the companion defaults it to 200
because broker frames are not constrained by Grok's MCP stdout boundary.
Unknown properties remain rejected.

### Companion flow

The artifact exporter lives in `codeg-mcp`, not in the LLM and not in the Node
validator:

```text
tools/call delivery=artifact
  -> request first broker page
  -> validate page DTO and snapshot metadata
  -> follow broker cursors internally
  -> validate exact cursor chain and completion
  -> serialize existing evidence wrapper to a private temporary file
  -> hash exact final bytes
  -> atomically publish file
  -> return bounded descriptor to the model
```

The companion uses the existing broker query and page DTO. It does not apply
the 7,680-byte MCP response reducer to internal broker pages. It accepts at
most 4,096 rows and exactly 4 MiB of final evidence bytes, matching the current
validator limits.

The exporter validates all of the conditions already required by the Node
validator: stable snapshot metadata, first offset zero, exact cursor echo,
contiguous ranges, one final page, matching total count, no duplicate task ID,
and no trailing page. The Node validator repeats these checks as defense in
depth.

If a snapshot becomes stale during internal pagination, the companion deletes
the partial file and transparently restarts once from page one. A second stale
result returns the existing typed stale error. There is no unbounded retry.

Cancellation, broker error, serialization error, byte-cap overflow, or process
shutdown removes any unpublished partial file.

### Artifact format

The file is the current validator input, unchanged:

```json
{
  "schema_version": 1,
  "pages": [
    {
      "schema_version": 1,
      "namespace": "brainstorm-to-delivery",
      "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
      "snapshot_revision": "42",
      "snapshot_created_at": "2026-08-26T08:00:00Z",
      "snapshot_expires_at": "2026-08-26T08:01:00Z",
      "total_rows": 0,
      "page_start": 0,
      "request_cursor": null,
      "runs": [],
      "next_cursor": null,
      "complete": true
    }
  ]
}
```

The empty example keeps the page contract internally valid. Production
artifacts contain every selected row. Serialization is compact UTF-8 JSON with
no insignificant whitespace. The digest is lowercase
`sha256:<64 lowercase hex>` over the exact file bytes.

Reusing the wrapper avoids a second evidence parser and keeps reconciliation
semantics identical. Page boundaries remain implementation evidence, not
model-visible data.

### Storage and cleanup

The companion creates an owner-only directory below the operating-system temp
directory:

```text
<temp>/codeg-mcp/orchestration-bindings/<connection-incarnation-id>/
```

Filenames are server-generated UUIDs and never include namespace, parent ID,
task ID, or caller text. The companion creates the directory with restrictive
permissions where the platform supports them and files with user-only access
on Unix. Windows inherits the current user's temp-directory ACL.

The write uses a sibling temporary file followed by atomic persist/rename. A
descriptor is never returned before the final file exists. The companion owns
cleanup:

- delete partial files on every failure and cancellation path;
- delete owned artifacts on normal companion shutdown;
- before a new export, remove artifact files in the fixed Codeg temp root whose
  modification time is more than 10 minutes old; and
- never recursively remove a path until its resolved parent is verified to be
  the fixed Codeg artifact root.

Artifacts expire for admission at the existing snapshot expiry, normally 60
seconds. Files surviving a crash are not valid evidence after expiry even if
the operating system has not removed them yet.

### Public response contract

A successful Phase 1 result has this structured shape:

```json
{
  "schema_version": 1,
  "delivery": "artifact",
  "namespace": "brainstorm-to-delivery",
  "snapshot_id": "1a641e16-36f4-4ec5-aa4f-18d18e6ab107",
  "snapshot_revision": "42",
  "snapshot_created_at": "2026-08-26T08:00:00Z",
  "snapshot_expires_at": "2026-08-26T08:01:00Z",
  "total_rows": 72,
  "artifact_path": "C:\\...\\9f7d6f1b-1547-44d2-82b1-1cbfb124f0ee.json",
  "artifact_format": "codeg-binding-evidence-v1",
  "artifact_bytes": 43021,
  "artifact_sha256": "sha256:7ac4...64-lowercase-hex"
}
```

The human-readable MCP `content` is one short line containing the path, row
count, revision, expiry, and digest. It does not serialize the structured
object again. `structuredContent` contains the object above. The final JSON-RPC
line, including the original request ID and newline, must be at most 2,048
bytes. The existing 256-byte serialized request-ID ceiling applies.

If an unexpectedly long platform path prevents the success result from
fitting, the companion deletes the file and returns a bounded typed error. It
never truncates a path or digest.

### Validator contract

The validator keeps `--durable-evidence FILE` and adds:

```text
--durable-evidence-sha256 sha256:<64 lowercase hex>
```

The new Skill always supplies both values from the artifact descriptor. The
validator opens the file once, reads at most 4 MiB plus one byte, computes the
digest over that buffer, and parses the same buffer with the existing durable
evidence parser. A digest mismatch, oversized file, expired snapshot, invalid
wrapper, or incomplete page chain fails before reconciliation.

Legacy manually assembled evidence remains accepted without the digest flag
for old Skill tests and page-mode compatibility. Artifact-mode Skill text
requires the digest; it cannot intentionally omit it.

Validator JSON output remains bounded to derived bindings, reconciliation
actions, snapshot identity, and stable failure IDs. It never echoes durable
rows or raw evidence.

### Skill contract

The Brainstorm-to-Delivery Skill changes each complete-snapshot instruction to
the following operation:

1. request `delivery: "artifact"`;
2. pass `artifact_path` and `artifact_sha256` directly to the validator;
3. consume only the validator's bounded JSON result; and
4. never open, print, summarize, copy, embed, or ask another model to inspect
   the artifact.

The Skill may use legacy pagination only when the live tool schema does not
advertise artifact delivery. Once artifact delivery is advertised, artifact
I/O, digest, stale, or validation failure blocks the workflow. It does not
silently fall back to model-visible pages, because doing so would reintroduce
the incident under pressure.

The existing cadence and fail-closed reconciliation rules do not change.

### Phase 1 residual race

Phase 1 preserves the current timing window:

```text
capture revision R -> validate Plan/progress/R -> another run mutates R+1
  -> dispatch request reaches existing admission path
```

Existing run-store uniqueness, lineage, binding, and recovery checks still
reject many unsafe outcomes, but there is no general proof that the validated
durable corpus remained unchanged. Phase 2 adds that proof.

## Phase 2: Atomic Admission Ticket

### Intent request

Phase 2 adds an optional `admission_intent` to the same artifact request:

```json
{
  "namespace": "brainstorm-to-delivery",
  "delivery": "artifact",
  "admission_intent": {
    "schema_version": 1,
    "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
    "kind": "continue",
    "work_unit_key": "task|7|implementer|codex|none",
    "agent_type": "codex",
    "profile_id": null,
    "target_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
    "replacement_reason": null,
    "orchestration_binding": {
      "schema_version": 1,
      "namespace": "brainstorm-to-delivery",
      "generation": 2,
      "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
    }
  }
}
```

The input object is strict. `dispatch_intent_id` is a canonical lowercase UUID.
The operation contracts are:

| Kind | Target | Replacement reason |
| --- | --- | --- |
| `first` | null | null |
| `continue` | existing source task ID | null |
| `replacement` | replaced source task ID | exact supported reason |

`agent_type`, `profile_id`, `work_unit_key`, and binding describe the intended
durable identity. For continuation, the broker also verifies them against the
source run. For an intentionally unbound document work unit,
`orchestration_binding` is null; key, Agent/profile, and operation identity
remain bound.

Task prompt and working directory are not duplicated into the intent. They are
already covered by the existing durable request fingerprint and are not part
of Plan/progress/durable binding reconciliation.

### Ticket issuance

The broker creates the snapshot and ticket under the existing snapshot-cache
read side of the mutation gate. It validates the intent against the captured
durable rows and stores an in-memory ticket entry containing:

- random ticket ID;
- parent conversation and connection incarnation;
- namespace, snapshot ID, and snapshot revision;
- expiry no later than the snapshot expiry;
- dispatch intent ID;
- canonical digest of the strict intent object; and
- unused state.

The ticket is an opaque random capability, not a signed self-describing token.
Codeg is both issuer and consumer in one process, so an in-memory lookup is
smaller and revocation is immediate. A process restart drops every ticket and
snapshot, forcing fresh validation.

The artifact result adds:

```json
{
  "admission": {
    "protocol": "ticket_v1",
    "outcome": "prepared",
    "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
    "ticket": "4a67bba4-e1f5-46d1-a9b1-aa796598ffce",
    "expires_at": "2026-08-26T08:01:00Z"
  }
}
```

The complete artifact result remains under the 2,048-byte JSONL limit.

If the same parent and `dispatch_intent_id` already identify a durable row whose
projected kind, target, key, Agent/profile, binding, and replacement reason
match the strict intent, the outcome is `already_admitted` with only the
existing `task_id`; no ticket is issued. The artifact still contains that row,
so the validator performs the existing lost-acknowledgement adoption before
the parent takes another action. A different projected durable identity
returns `delegation_dispatch_intent_conflict` and no artifact or ticket. The
full request fingerprint, including task and working directory, is compared if
a physical dispatch reaches the exact replay path.

### Progress intent identity

The progress `dispatch_intent` object gains optional `intent_id`:

```json
{
  "intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
  "kind": "continue",
  "continuation_target_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
  "replacement_target_task_id": null,
  "replacement_reason": null,
  "expected_root_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
  "expected_lineage_root_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
  "expected_generic_generation": 2,
  "expected_child_conversation_id": 931,
  "adopted_after_lost_acknowledgement": false
}
```

The updated validator accepts exactly two dispatch-intent shapes: the legacy
field set without `intent_id`, and the ticket-v1 field set with one canonical
UUID. A ticket-v1 admission requires the latter. The progress marker remains
`codeg-simple-progress-v1`; this is an additive recovery identity, not a new
workflow state format.

The parent writes the intent ID to progress before requesting the artifact and
retains it until the operation is definitively failed or reconciled. It reuses
that ID only for a retry of the same logical operation.

### Delegation request contract

`delegate_to_agent` and `continue_delegation` add an optional pair:

```json
{
  "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52",
  "admission_ticket": "4a67bba4-e1f5-46d1-a9b1-aa796598ffce"
}
```

Both fields must be present together or absent together. Calls participating
in ticket-v1 always supply both. Legacy calls supply neither and retain current
behavior. A ticket-v1 call still supplies a fresh `correlation_id`; the two
identities are not interchangeable.

### Atomic consume sequence

The consume order is:

```text
physical MCP call
  -> existing correlation validation/claim
  -> parse ticket and dispatch intent
  -> acquire existing run-store mutation write gate
  -> find and remove one correctly scoped unexpired ticket
  -> compare parent, incarnation, revision, intent ID, and intent digest
  -> check exact durable intent replay/conflict
  -> execute existing reserving DB transaction
  -> persist dispatch_intent_id with the run
  -> increment parent snapshot revision and invalidate remaining tickets
  -> release gate
  -> perform existing child spawn/resume and promotion flow
```

Ticket validation occurs before provisional child creation, session spawn or
resume, recovery-authorization consumption, or recovery-budget charge. The
reserving row remains the durable admission boundary. The ticket does not hold
a database transaction open across child startup.

A correctly scoped ticket is burned when a consume attempt enters the mutation
gate, whether the later business check succeeds or fails. Retrying after a
failure requires a new artifact, validator run, and ticket. This avoids
ambiguous ticket reuse and keeps recovery simple.

The existing run-store transaction continues to enforce work-unit uniqueness,
lineage, actual Agent/profile, binding inheritance, replacement authorization,
continuation and replacement budgets, writable workflow state, and request
fingerprints. Ticket checks are an additional revision/intent fence, not a
replacement implementation.

### Durable intent idempotency

Add nullable `dispatch_intent_id` to `delegation_task_runs` with:

- strict canonical UUID validation on new non-null values;
- a partial unique index on
  `(parent_conversation_id, dispatch_intent_id)` where the ID is non-null;
- immutability after insert; and
- no historical backfill.

The field is included in the existing request fingerprint and reserving insert
but omitted from legacy binding-page DTOs and user-facing cards. Omitting it
from the evidence DTO preserves the exact v1 validator format; recovery uses
the artifact's existing row identity while the broker uses the internal column
for exact replay classification.

An exact replay returns the existing run through the normal delegation result
shape and may set bounded structured metadata `idempotent_replay: true`. A
replay with changed kind, target, key, Agent/profile, binding, replacement
reason, task, or working directory returns
`delegation_dispatch_intent_conflict` before child side effects.

This supplements rather than replaces current parent-tool-use idempotency.
`parent_tool_use_id` identifies a host tool invocation when available;
`dispatch_intent_id` survives a new physical invocation after a lost result.

## End-to-End Data Flows

### Phase 1

```text
Parent writes/updates progress intent
  -> one artifact-mode MCP call
  -> companion privately collects every broker page
  -> model receives path + digest + revision only
  -> Node validator reads artifact directly
  -> validator returns bounded admission/reconciliation JSON
  -> parent dispatches through existing API
```

### Phase 2 success

```text
Parent writes progress intent with intent_id
  -> artifact request includes exact admission_intent
  -> broker captures revision R and issues ticket T
  -> companion returns artifact descriptor + T
  -> validator reconciles Plan/progress/artifact R
  -> delegate/continue sends T + same intent_id + fresh correlation_id
  -> run store consumes T and reserves under revision R
  -> mutation advances revision to R+1
```

### Phase 2 concurrent mutation

```text
Artifact/ticket captured at R
  -> another run mutation advances parent to R+1 and revokes T
  -> dispatch with T fails orchestration_admission_ticket_stale
  -> parent obtains a new artifact and revalidates
```

### Lost acknowledgement

```text
Run reserved with dispatch_intent_id D
  -> MCP result is lost
  -> parent keeps unresolved progress intent D
  -> next artifact request with D observes existing durable row
  -> response says already_admitted and artifact contains the row
  -> validator returns the existing exact adoption action
  -> parent updates progress; no second child or budget charge
```

## Error Contract

| Code | Meaning | Retry rule |
| --- | --- | --- |
| `orchestration_binding_query_invalid` | Invalid page/artifact input | Fix input |
| `orchestration_binding_query_too_large` | More than 4,096 selected rows | Block |
| `orchestration_binding_snapshot_stale` | Mutation, expiry, or restart invalidated scan | Fresh artifact |
| `orchestration_binding_query_failed` | DB or broker failure | Bounded retry, then block |
| `orchestration_binding_artifact_io_failed` | Private file create/write/persist failed | Block; no page fallback |
| `orchestration_binding_artifact_too_large` | Final evidence exceeded 4 MiB | Block |
| `orchestration_binding_artifact_result_too_large` | Descriptor could not fit 2,048 bytes | Block; artifact deleted |
| `orchestration_admission_intent_invalid` | Intent grammar or source identity is invalid | Fix/reconcile intent |
| `orchestration_admission_ticket_missing` | Only one ticket field was supplied | Fresh artifact/ticket |
| `orchestration_admission_ticket_stale` | Ticket expired, was revoked, or revision changed | Fresh artifact/validation |
| `orchestration_admission_ticket_mismatch` | Parent, incarnation, or intent digest differs | Block and investigate |
| `orchestration_admission_ticket_consumed` | Ticket was already attempted | Reconcile intent, then fresh artifact |
| `delegation_dispatch_intent_conflict` | Same durable intent ID has different semantics | Block and investigate |

All errors are bounded MCP results and contain no durable rows or artifact
contents. Error text does not advise blind redispatch. Ticket errors do not
cancel an existing child.

Error precedence for a ticket-v1 dispatch is: input grammar, physical call
correlation, ticket scope/revision, exact intent replay/conflict, existing
recovery authorization, existing admission/business rules, then child
lifecycle. Earlier failure performs none of the later side effects.

## Compatibility and Rollout

### Phase 0: measurement

Land counters and a deterministic 72-row/session-4123 benchmark fixture before
changing Skill behavior. Record baseline visible calls and serialized result
bytes.

### Phase 1: artifact preferred

- Add artifact delivery without changing page mode.
- Update validator digest handling and Skill instructions together.
- New Skill uses artifact mode when advertised.
- Old Skill continues to paginate against the unchanged API.
- Artifact-capable Skill fails closed on artifact errors rather than silently
  restoring page mode.

Phase 1 may ship and remain deployed independently. It solves the context
incident even if Phase 2 is delayed.

### Phase 2: ticket-v1

- Add `admission_intent`, ticket state, delegation ticket fields, and the
  nullable durable dispatch intent column.
- Update Skill and validator so every new logical action has an intent ID and
  every ticket-capable action uses the ticket pair.
- Requests without `dispatch_intent_id` remain legacy-compatible.
- Requests with `dispatch_intent_id` are always ticket-enforced; omission or
  mismatch cannot fall back to legacy behavior.

This compatibility choice means old Skills can still use the legacy race. The
atomic guarantee is exact for ticket-v1 participants. Making tickets mandatory
for every B2D binding would require a versioned binding/Skill activation
contract and is outside this design.

### Rollback

The Skill can stop requesting artifact or ticket fields and return to legacy
pagination without a database rollback. The nullable intent column and partial
index are inert for legacy calls. Artifact files are temporary and require no
data migration. Page-mode wire compatibility is the rollback boundary.

## Security and Privacy

- Parent identity continues to come from the MCP token; no parent ID is
  accepted in artifact or ticket input.
- Artifact paths are random, local, short-lived, and outside the workspace.
- Artifacts contain no prompts or generated content.
- The validator verifies the exact digest before parsing.
- Ticket IDs are random and useful only with the same parent connection,
  unmodified intent, unmodified revision, and unexpired in-process entry.
- Logs and metrics must not record artifact paths, ticket IDs, intent IDs,
  task IDs, child IDs, profile IDs, or digests. Counts, byte sizes, durations,
  revisions as deltas, and stable error codes are sufficient.
- Neither artifact paths nor ticket IDs appear in user-facing delegation
  cards.
- No endpoint serves artifact contents over HTTP. Desktop and server modes
  rely on the existing fact that `codeg-mcp`, the ACP CLI, and validator run on
  the same host/container and user filesystem.
- If a future remote ACP topology does not share the filesystem, artifact mode
  must be reported unavailable. Streaming the file through the model is not a
  fallback.

## Observability

Add bounded counters/histograms:

- artifact export attempts by stable outcome;
- internal page count, selected row count, evidence bytes, export duration,
  and final MCP result bytes;
- transparent stale restarts;
- artifact cleanup successes/failures;
- tickets issued, consumed, stale, mismatched, and already-admitted outcomes;
- dispatch-intent exact replays and conflicts; and
- legacy page-mode calls after artifact capability is available.

The session benchmark reports aggregate model-visible binding result bytes and
calls. It must not ingest artifact file contents into a model transcript to
measure them.

## Testing Strategy

### Phase 1 Rust tests

- Existing page request fixtures serialize exactly as before.
- A 72-row corpus that previously needs approximately 12 Grok-safe pages
  returns one artifact result with zero `runs` fields in the JSON-RPC line.
- The final artifact JSON-RPC line is at most 2,048 bytes with the maximum
  accepted request ID and a representative Windows path.
- Artifact bytes parse as the existing `{schema_version,pages}` wrapper and
  select the same rows in the same order as legacy pagination.
- SHA-256 matches the exact persisted bytes.
- Empty, one-row, 4,096-row, and 4 MiB boundary cases are exact.
- Row-cap and byte-cap overflow leave no published file.
- A stale page chain is restarted once; a second stale outcome is bounded and
  cleans both attempts.
- Cancellation, broker failure, serialization failure, and shutdown remove
  partial files.
- Cleanup resolves and verifies the fixed temp root before deleting stale
  files.
- Root/coordination feature gates and parent-token isolation remain exact.
- Grok's complete `tools/list` JSONL line remains at or below 7,680 bytes after
  the schema extension.

### Validator and Skill tests

- Correct digest passes; wrong, malformed, or omitted artifact-mode digest
  fails with one stable rule ID.
- The same buffer is hashed and parsed; a post-read file replacement cannot
  change parsed evidence.
- Expired, mixed, incomplete, duplicate, reordered, and oversized evidence
  retains current failure behavior.
- Legacy evidence without a digest remains accepted in legacy mode.
- Validator output never contains a durable run object.
- Skill mutation fixtures require artifact mode when advertised, direct path
  forwarding, and explicit prohibition on reading/printing evidence.
- Skill mutation fixtures reject silent page fallback after an artifact error.
- Existing Plan/progress/durable reconciliation and lost-ACK fixtures produce
  the same decisions from artifact bytes as from manually assembled evidence.

### Phase 2 Rust tests

- Ticket claims are exact for parent, connection incarnation, namespace,
  snapshot, revision, expiry, intent ID, and intent digest.
- Any parent mutation between issue and consume makes the ticket stale.
- Expiry, restart, wrong parent, wrong intent, partial ticket fields, reuse,
  and concurrent consume fail without child side effects.
- Exactly one of two concurrent consumers reserves a new run; the other
  obtains exact replay or a consumed result and never creates a second admitted
  run.
- Ticket validation occurs before provisional child creation, resume,
  recovery-authorization consumption, and budget charge.
- The durable partial unique index rejects duplicate parent/intent pairs.
- The intent column is immutable and historical rows remain null.
- Same intent plus same request fingerprint returns the existing run; any
  semantic change returns `delegation_dispatch_intent_conflict`.
- A lost result followed by a fresh physical call uses the same intent ID and
  a fresh correlation ID, then converges through existing adoption.
- Existing parent-tool idempotency, correlation, continuation, replacement,
  recovery authorization, and provisional cleanup tests remain green.

### Integration and benchmark tests

- Desktop and server-mode companions can create an artifact readable by the
  validator in their actual launch environment.
- Session-4123 replay performs 125 model-visible artifact calls, zero
  model-visible continuation-page calls, and no more than 256 KiB of aggregate
  artifact result JSONL, at least 90% below the approximately 2.9 MiB baseline.
- The replay reaches the same reconciliation decisions and dispatch sequence
  as legacy pagination.
- A forced mutation between validator success and dispatch is accepted by
  Phase 1 only when existing business checks allow it, but is always rejected
  as stale by ticket-v1 before reservation.
- An intentionally lost dispatch response creates one durable run and one
  child lineage, then adopts it without a second budget charge.

## Acceptance Criteria

1. Legacy page request and response contracts remain compatible.
2. One artifact request produces one model-visible result regardless of page
   count, with no durable row in `content` or `structuredContent`.
3. Every successful artifact JSON-RPC line is at most 2,048 UTF-8 bytes and
   every Grok `tools/list` line remains at most 7,680 bytes.
4. Artifact bytes are at most 4 MiB, contain at most 4,096 rows, and produce
   the same validator outcome as the equivalent legacy page set.
5. Session-4123 replay reduces aggregate binding-query result bytes by at
   least 90% and removes all 670 model-visible continuation-page calls.
6. Artifact-capable Skill behavior never silently falls back to raw pages
   after an artifact failure.
7. Phase 2 rejects every revision mutation between snapshot capture and run
   reservation for ticket-v1 calls.
8. The same logical dispatch intent cannot admit two runs; exact replay returns
   the existing run and changed semantics fail closed.
9. Existing correlation, recovery authorization, lineage, budget, and binding
   rules remain authoritative and tested.
10. Plan/progress semantic authority remains in the Node validator; Codeg does
    not parse them for ticket admission.
11. No frontend or user-visible card change is required.
12. Temporary evidence is removed on normal paths and is unusable after its
    snapshot expires.

## File Ownership

### Phase 1 Codeg

- `src-tauri/src/acp/delegation/tool_schema.json`: add artifact delivery schema.
- `src-tauri/src/acp/delegation/types.rs`: strict delivery request and artifact
  descriptor DTOs.
- `src-tauri/src/acp/delegation/companion.rs`: internal pagination, artifact
  writer, digest, cleanup, bounded renderer, and tests.
- `src-tauri/src/acp/delegation/transport.rs`: reuse the existing binding page
  round trip; no new public transport is required for Phase 1.
- Existing orchestration binding query, listener, and run-store selection stay
  unchanged except where tests expose shared helpers.

### Phase 1 Skill package

- `.agents/skills/brainstorm-to-delivery/SKILL.md`: artifact-first protocol and
  no-inspection/no-fallback rules.
- `scripts/validate-contract.mjs`: digest CLI flag and single-buffer file read.
- `scripts/validate-contract.lib.mjs`: digest option plumbing and stable rule
  assertions without changing reconciliation semantics.
- Existing validator tests: artifact equivalence, digest, and Skill mutation
  coverage.

### Phase 2 Codeg

- `src-tauri/src/acp/delegation/types.rs`: admission intent, ticket claims, and
  delegation request fields.
- `src-tauri/src/acp/delegation/orchestration_binding_query.rs`: ticket state
  tied to snapshot revision and parent mutation invalidation.
- `src-tauri/src/acp/delegation/companion.rs` and `tool_schema.json`: intent
  input, ticket output, and delegation ticket pair.
- `src-tauri/src/acp/delegation/transport.rs` and `listener.rs`: broker wire
  fields and parent/role authorization.
- `src-tauri/src/acp/delegation/broker.rs`: pre-side-effect ticket handoff and
  exact replay result.
- `src-tauri/src/acp/delegation/run_store.rs`: atomic ticket consume, intent
  fingerprinting, unique replay classification, and reserving persistence.
- `src-tauri/src/db/entities/delegation_task_run.rs`: nullable intent column.
- A new additive migration: nullable column, partial unique index, validation,
  and immutability protection without backfill.

### Phase 2 Skill package

- Skill write-ahead intent UUID and ticket-required flow.
- Validator dual exact dispatch-intent shapes and ticket-v1 requirements.
- Lost-acknowledgement fixtures that distinguish stable intent ID from fresh
  physical correlation ID.

## Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Artifact mode merely moves data to a file the model later reads | Skill contract and mutation tests prohibit inspection; validator output stays bounded |
| ACP CLI and companion do not share a filesystem | Advertise artifact mode only for same-host companion launches; fail closed otherwise |
| Temp evidence survives a crash | 60-second semantic expiry, private directory, normal cleanup, and 10-minute stale sweep |
| Tool schema growth breaks Grok framing | Extend one existing tool, keep descriptions compact, and retain the literal 7,680-byte catalog test |
| Validator takes longer than snapshot/ticket TTL | Return typed stale, discard result, and repeat one fresh artifact/validation cycle |
| Ticket becomes a second policy engine | Ticket validates only revision and durable identity; Node remains semantic authority |
| Old Skill still causes context growth | Legacy support is intentional; new Skill uses artifact mode whenever advertised and telemetry exposes legacy use |
| Process restart invalidates a valid-looking file | Snapshot expiry plus missing in-process ticket forces fresh validation; startup files are never trusted by path alone |
| New intent identity conflicts with correlation ID | Separate field names, persistence rules, retry rules, and regression tests |
| Ticket validation is placed after child side effects | Explicit error precedence and pre-side-effect integration tests |

## Deliberate Simplifications

- Phase 1 writes the existing evidence wrapper instead of inventing a compact
  artifact schema. Local disk size is not the incident, and parser reuse is
  safer.
- Tickets are in-memory rather than signed or persisted. Process restart is a
  natural invalidation boundary and already requires fresh durable evidence.
- Ticket-v1 is enforced only when `dispatch_intent_id` is supplied so old
  Skills continue to work. A universal mandatory protocol needs a separate
  versioned rollout.
- No delta cache is introduced. The 4 MiB local snapshot is cheap compared
  with retaining repeated pages in model history.

These simplifications are intentional boundaries, not placeholders.
