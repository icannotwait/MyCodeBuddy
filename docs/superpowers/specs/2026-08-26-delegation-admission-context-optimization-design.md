# Delegation Admission Context Optimization Design

## Status

Proposed on 2026-08-26 and revised on 2026-08-27 after two review rounds. This
revision addresses the latest two Important findings; status remains Proposed
pending independent re-review. The selected direction is a staged migration:

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
7. Ticket validation, required recovery-authorization consumption, and durable
   run reservation occur under the same existing parent mutation fence. The
   authorization consume and reserving insert share one database transaction;
   child startup happens only after gate release.
8. `dispatch_intent_id` identifies one logical operation and is stable across
   its physical retries. `correlation_id` identifies one physical tool call,
   remains transport-only, and is fresh on every retry.
9. A matching persisted dispatch intent may return the existing run only when
   its durable request fingerprint also matches exactly. The same intent ID
   with different semantic input, including task or working directory, fails
   closed.
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
shutdown removes any unpublished partial file. The companion retains ownership
after atomic publication until the MCP response is relayed; cancellation in
that interval deletes the published artifact before suppressing the response.

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
- delete a published artifact if cancellation wins before its MCP response is
  relayed;
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
    "request_fingerprint": "2a44be9d1662a314cbbd2c8111bcf83159be7bdc93abadff977d01447f986648",
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

`request_fingerprint` is a lowercase 64-hex SHA-256 digest in the exact form
persisted by the reserving insert. Legacy calls and ticket-v1 calls use
separate canonical inputs.

#### Legacy fingerprint compatibility

Calls without the ticket pair keep `run_store.rs::request_fingerprint`
byte-for-byte. The unbound input remains this seven-string array:

```text
[
  tool_name,
  NFC(task),
  work_unit_key || "",
  replaces_task_id || "",
  replacement_reason || "",
  target_task_id || "",
  lowercase(launch_route_fingerprint_hex)
]
```

The bound input remains the 12-string v2 array:

```text
[
  "delegation-request-v2",
  tool_name,
  NFC(task),
  work_unit_key || "",
  replaces_task_id || "",
  replacement_reason || "",
  target_task_id || "",
  lowercase(launch_route_fingerprint_hex),
  decimal(binding.schema_version),
  binding.namespace,
  decimal(binding.generation),
  binding.route_fingerprint
]
```

Both continue to hash the UTF-8 bytes of the compact JSON string array. Their
existing Rust golden digests remain compatibility fixtures.

#### Ticket-v1 v3 fingerprint

Ticket-v1 does not call `canonicalize`, `realpath`, `resolve`, `path.normalize`,
or current-directory lookup while constructing its fingerprint. It hashes one
fixed 15-string array:

```text
[
  "delegation-request-v3",
  tool_name,
  NFC(task),
  normalize_supplied_working_dir(working_dir),
  work_unit_key || "",
  replaces_task_id || "",
  replacement_reason || "",
  target_task_id || "",
  agent_type,
  profile_id || "",
  dispatch_intent_id,
  binding ? decimal(binding.schema_version) : "",
  binding ? binding.namespace : "",
  binding ? decimal(binding.generation) : "",
  binding ? binding.route_fingerprint : ""
]
```

All entries are strings. Node serializes the array with `JSON.stringify` and
hashes those exact UTF-8 bytes; Rust serializes the same strings as compact
JSON and must match the shared vectors. `task` is NFC-normalized but not
trimmed. Other non-null fields retain their validated wire spelling.

`normalize_supplied_working_dir` is the Node-authoritative expression
`value.normalize("NFC").trim()`. A null, omitted, or empty value produces
`""`; it never substitutes the Node process cwd, parent cwd, or child launch
cwd. For `delegate_to_agent`, the Skill passes the resulting non-empty string
unchanged as the physical `working_dir` argument, or omits the argument when
the result is empty. The current `continue_delegation` contract has no cwd
override, so its v3 slot is always `""`; no continuation cwd field is added.

Launch setup remains independent. After ticket-v1 fingerprint verification,
the broker may default and canonicalize the workspace for its launch snapshot,
durable route fingerprint, and child spawn exactly as it does today. Those
derived path bytes are not ticket-v1 fingerprint inputs. Rust recomputes v3
from the physical call's preserved pre-default `requested_working_dir` (or
`""` for continue), before launch canonicalization, and rejects any mismatch
before child side effects.

Task prompt and working directory are not duplicated into `admission_intent`.
Only their opaque v3 digest is present; the model never derives the digest from
artifact contents.

#### Pending-call construction contract

`validate-contract.lib.mjs` owns the pure
`deriveTicketV1RequestFingerprint(pendingCall)` helper. The Skill invokes it
through the exact pre-artifact CLI mode
`validate-contract.mjs --ticket-v1-fingerprint --output-json`. That mode reads
one UTF-8 pending-call JSON object from stdin, bounded by the existing 2 MiB
Plan-document cap, and accepts no Plan, progress, evidence, derivation,
document-admission, or admission flags. The strict object is:

```json
{
  "schema_version": 1,
  "tool_name": "continue_delegation",
  "task": "Continue the approved implementation",
  "working_dir": null,
  "work_unit_key": "task|7|implementer|codex|none",
  "replaces_task_id": null,
  "replacement_reason": null,
  "target_task_id": "6b228a7d-4ac9-4bc7-a16e-f4ecf6f0fd45",
  "agent_type": "codex",
  "profile_id": null,
  "orchestration_binding": {
    "schema_version": 1,
    "namespace": "brainstorm-to-delivery",
    "generation": 2,
    "route_fingerprint": "sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a"
  },
  "dispatch_intent_id": "8f95dd45-9eca-42a8-9909-0ac00be8ad52"
}
```

Every listed key is required; nullable operation fields use JSON null and
unknown keys fail. The helper validates the first/continue/replacement matrix.
Its bounded output is exactly:

```json
{
  "schema_version": 1,
  "request_fingerprint": "2a44be9d1662a314cbbd2c8111bcf83159be7bdc93abadff977d01447f986648",
  "normalized_working_dir": ""
}
```

It never returns task text. The CLI receives the object through stdin rather
than a command argument or persistent evidence file. The Skill runs this mode
before the artifact request, copies the digest into
`admission_intent.request_fingerprint`, and uses the same pending-call values
for the later physical call. Plan/progress reconciliation remains a separate
validator step after artifact creation.

### Ticket issuance

The broker creates the snapshot and ticket under the existing snapshot-cache
read side of the mutation gate. It validates the intent against the captured
durable rows and stores an in-memory ticket entry containing:

- random ticket ID;
- parent conversation and connection incarnation;
- namespace, snapshot ID, and snapshot revision;
- expiry no later than the snapshot expiry;
- dispatch intent ID;
- exact request fingerprint;
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

If the same parent and `dispatch_intent_id` already identify a durable row, the
broker requires exact equality of projected kind, target, key, Agent/profile,
binding, replacement reason, **and the persisted `request_fingerprint`**. Only
then is the outcome `already_admitted` with the existing `task_id`; no ticket
is issued. The artifact still contains that row, so the validator performs the
existing lost-acknowledgement adoption before the parent takes another action.

A fingerprint mismatch on that same intent ID, including one caused by changed
task or working directory, returns `delegation_dispatch_intent_conflict` with
no artifact and no ticket. A lost-acknowledgement retry of the same operation
retains the same fingerprint and still reaches `already_admitted` plus
validator adoption. Physical dispatch recomputation remains a second line of
defense; it is not the first place task/cwd divergence can be detected.

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

The parent writes the intent ID to progress before constructing the pending
call, then retains the pending-call values and resulting v3 digest until the
operation is definitively failed or reconciled. It reuses that ID and digest
only for a retry of the same logical operation.

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
identities are not interchangeable. A ticket-v1 delegate also uses the exact
non-empty normalized `working_dir` returned by the Node helper, or omits the
field when the normalized value is empty.

### Atomic consume sequence

The consume order is:

```text
physical MCP call
  -> existing correlation validation/claim
  -> parse ticket and dispatch intent
  -> broker acquires existing run-store mutation write gate
  -> find and remove one correctly scoped unexpired ticket
  -> compare parent, incarnation, revision, intent ID, intent digest,
     and request fingerprint
  -> check exact durable intent replay/conflict
  -> for gen-1/replacement, create the provisional child conversation row
     required by the non-null child_conversation_id FK
  -> for continuation, retain the existing child conversation
  -> execute existing reserving DB transaction:
       validate and consume any required recovery authorization
       run existing admission/business checks
       insert the reserving run with dispatch_intent_id
  -> increment parent snapshot revision and invalidate remaining tickets
  -> release gate
  -> perform existing child process/session spawn or resume and promotion flow
```

The broker, not a new policy component, owns the existing write guard from
ticket burn through successful reserving insert and revision invalidation. The
run store exposes a gate-aware internal admission entry point so the broker-held
guard is not reacquired; legacy entry points continue to acquire the same guard
themselves. Existing transaction-local uniqueness, lineage, binding, writable
workflow, recovery-authorization validation/consume, and budget rules remain
in the run store.

`child_conversation_id` remains a non-null foreign key. This is an ownership
clarification of the current gen-1 sequence: provisional creation moves inside
the broker-held mutation fence, before the reserving insert, rather than
changing the database relationship. The ticket does not hold a database
transaction open across child startup.

A correctly scoped ticket is burned when a consume attempt enters the mutation
gate, whether the later business check succeeds or fails. Retrying after a
failure requires a new artifact, validator run, and ticket. This avoids
ambiguous ticket reuse and keeps recovery simple.

If provisional creation fails, the transaction does not run and no durable run
is persisted. If provisional creation succeeds but recovery authorization is
expired, already consumed, loses its CAS, or any later reserving check/insert
fails, the database transaction rolls back and persists no run. Before
releasing the write gate, the broker rolls back the child when transaction
ownership permits; otherwise it terminalizes/marks it for the existing
provisional-orphan cleanup path. A cleanup failure retains the existing visible
failed-provisional behavior, but still persists no delegation run. The burned
ticket is never restored; every such path requires a fresh artifact and
ticket, while the unchanged parent snapshot revision may be captured again.

Required recovery authorization is therefore fully consumed before a
model-visible reserving row can exist. No reserved/claimed authorization state
is added, and no unauthorized reserving row is exposed for replay adoption.
Child process/session spawn and resume remain after gate release. Existing
budget accounting retains its run-store admission/lifecycle boundary; ticket
checks do not duplicate it. Two concurrent consumers can produce at most one
reserving row. The winner reserves; the loser receives consumed, stale,
authorization failure, or exact-replay handling, never a second admitted run
or provisional child.

### Durable intent idempotency

Add nullable `dispatch_intent_id` to `delegation_task_runs` with:

- strict canonical UUID validation on new non-null values;
- a partial unique index on
  `(parent_conversation_id, dispatch_intent_id)` where the ID is non-null;
- immutability after insert; and
- no historical backfill.

For ticket-v1 calls, the field is included in the existing request fingerprint
and reserving insert. The same fingerprint is carried in
`admission_intent.request_fingerprint`; legacy fingerprint construction is
unchanged. The intent column and fingerprint remain omitted from legacy
binding-page DTOs and user-facing cards. Omitting them from the evidence DTO
preserves the exact v1 validator format; recovery uses the artifact's existing
row identity while the broker uses the internal columns for exact replay
classification.

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
  -> Node v3 helper hashes the strict pending call without filesystem reads
  -> parent copies the digest to admission_intent and retains the pending call
  -> artifact request includes exact admission_intent
  -> broker captures revision R and issues ticket T
  -> companion returns artifact descriptor + T
  -> validator reconciles Plan/progress/artifact R
  -> delegate/continue sends T + same intent_id + fresh correlation_id
  -> broker recomputes v3 from the physical call's supplied cwd bytes
  -> run store consumes required recovery authorization and reserves
     in one DB transaction under revision R
  -> mutation advances revision to R+1
  -> gate releases; child process/session spawns or resumes
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
  -> parent keeps unresolved progress intent D, pending call, and v3 digest
  -> next artifact request with D and that fingerprint observes the durable row
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
lifecycle. Earlier failure performs none of the later side effects. Once the
ticket has been burned, an authorization expiry/CAS error retains its existing
typed authorization code but always returns with no durable run and a consumed
ticket; retry requires a fresh artifact and ticket.

## Compatibility and Rollout

### Phase 0: measurement

Land counters and a deterministic 72-row/session-4123 benchmark fixture before
changing Skill behavior. The fixture and counters form a separately runnable
benchmark target with a frozen baseline of 125 first-page calls, 670
continuation-page calls, and approximately 2.9 MiB of model-visible binding
result bytes. Record any baseline revision explicitly rather than silently
regenerating it from current behavior.

### Grok catalog compaction and phase budgets

The current `GROK_FEATURES` fixture serializes to 7,673 bytes, leaving only 7
bytes below the literal `7_680` ceiling. Schema additions must not land on that
shape. Before Phase 1, compact the existing catalog in place while preserving
tool names, order, feature/role gating, runtime validation, and the exact
page-mode request/response behavior. No new tool is added.

The compact catalog shape is fixed as follows. These are catalog-only cuts;
the Rust request DTOs and parsers retain their current strict validation.

- Keep the current descriptions and every phrase asserted by
  `tool_schema_retains_essential_agent_guidance` for delegate, status/Join,
  cancel, feedback, ask, session info, parent decision, and reply. Keep the
  recovery-authorization result guidance.
- Remove top-level `description` from `register_simple_workflow`,
  `get_delegation_orchestration_bindings`, `continue_delegation`,
  `get_workflow_capabilities`, `get_workflow_state`, `recover_workflow`,
  `publish_workflow_manifest`, and `settle_workflow_gate`. Remove leaf
  descriptions from delegate `agent_type`/`correlation_id`, continue
  `correlation_id`, and recovery authorization `proposed_user_reason`; retain
  delegate `replacement_reason.description` as `""` because the existing
  recovery contract test reads that node.
- Remove `$defs.b` from delegate and continue; advertise each
  `orchestration_binding` property as `{}`. Strict binding grammar remains in
  the Rust DTO.
- Collapse these leaves to `{}`: delegate `profile_id`, `profile_label`,
  `task`, `correlation_id`, `working_dir`, `replaces_task_id`; registration
  `plan_rel_path`, `progress_rel_path`; binding query `snapshot_id`; continue
  `task_id`, `task`, `correlation_id`; cancel `task_id`; reply `request_id`,
  `reply`; workflow state `workflow_id`; recover `workflow_id`,
  `recovery_authorization_id`; publish `workflow_id`, `plan_target_rel_path`;
  and settle `workflow_id`, `gate_id`, `recovery_authorization_id`, `summary`.
- Remove redundant `type` from delegate `agent_type`,
  `recovery_authorization_id`, `work_unit_key`, `replacement_reason`; binding
  query `namespace`, `limit`, `cursor`; continue `recovery_authorization_id`,
  `work_unit_key`; status `wait_ms`, `return_when`; cancel `reason`; session
  info `max_messages`; all four recovery-authorization properties; workflow
  state `detail`; recover `expected_manifest_revision`, `correlation_id`;
  publish `schema_version`, `expected_manifest_revision`,
  `risk_policy_version`, `task_policies`; and settle
  `expected_graph_revision`, `expected_review_round`, `expected_gate_cycle`,
  `expected_outcome`. Remove `inputSchema.type` from
  `get_workflow_capabilities`. The remaining enum, pattern, const, minimum,
  maximum, default, and `maxLength` keywords stay.
- Phase 2 uses one compact local UUID `$defs` entry in each delegation tool for
  `dispatch_intent_id` and `admission_ticket`, with `dependentRequired` in both
  directions. The binding tool adds one compact strict `admission_intent`,
  including `request_fingerprint`, and one local strict binding definition.
  Operation-specific nullable combinations remain parser-enforced.

All existing tool names, order, `required` arrays, root
`additionalProperties`, feature/role gating, and runtime error behavior remain
unchanged. Page-mode request and response bytes do not depend on these catalog
description/schema-leaf reductions and remain byte-for-byte compatible.

Measured with the current Grok tool order and exact JSON-RPC wrapper, this
shape is 5,393 bytes before additions, 5,433 bytes with Phase 1 `delivery`, and
7,211 bytes with the complete Phase 2 intent and ticket pair. Release gates use
slightly wider hard budgets so implementation details are not forced to match
one property order:

| Catalog gate | Maximum JSONL bytes | Required remainder below 7680 |
| --- | ---: | ---: |
| Phase 1: compact catalog plus `delivery` only | 5,500 | 2,180 |
| Phase 2: Phase 1 plus intent and both ticket pairs | 7,300 | 380 |

The existing test retains the Rust literal `7_680` and the user-facing message
`7680 bytes`; phase-specific assertions add the 5,500 and 7,300 caps. If a
phase cannot meet both its phase cap and the absolute 7,680-byte ceiling, that
phase fails closed and is not advertised or released. The remedy is further
catalog compaction, never truncation, removing a required tool, raising the
literal, adding a second tool, or changing page mode.

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
- Add the Node-owned pending-call preflight and cross-runtime v3 fingerprint;
  legacy unbound/v2 fingerprint bytes remain unchanged.
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
- Cancellation after atomic publication but before response relay deletes the
  published artifact.
- Cleanup resolves and verifies the fixed temp root before deleting stale
  files.
- Root/coordination feature gates and parent-token isolation remain exact.
- Grok's complete Phase 1 `tools/list` JSONL line is at most 5,500 bytes and
  retains at least 2,180 bytes below the absolute 7,680-byte ceiling.

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
- The pre-artifact fingerprint mode accepts only bounded stdin plus
  `--ticket-v1-fingerprint --output-json`, rejects missing/extra/wrongly typed
  pending-call fields, and never emits task text.
- Shared Node/Rust golden fixtures retain the current unbound seven-string and
  bound v2 legacy digests, then add ticket-v1 vectors for composed/decomposed
  NFC task text, every omitted/null slot, unbound and bound requests, JSON cwd
  values `"D:\\repo"` versus `"D:/repo"` as distinct supplied strings,
  surrounding cwd whitespace trimming, and `dispatch_intent_id` changes.

### Phase 2 Rust tests

- Ticket claims are exact for parent, connection incarnation, namespace,
  snapshot, revision, expiry, intent ID, request fingerprint, and intent digest.
- Any parent mutation between issue and consume makes the ticket stale.
- Expiry, restart, wrong parent, wrong intent, partial ticket fields, reuse,
  and concurrent consume fail without child side effects.
- Exactly one of two concurrent consumers reserves a new run; the other
  obtains consumed, stale, or exact-replay handling and never creates a second
  admitted run or provisional child.
- The broker holds the mutation write gate across ticket burn, provisional
  child creation where required, reserving insert, and revision invalidation.
- Provisional creation failure and reserving-insert failure persist no durable
  run; an allocated child is rolled back or enters the existing provisional
  cleanup path, and the ticket cannot be reused.
- Rust v3 recomputation uses the physical request's pre-default supplied
  `working_dir`, never canonicalized launch workspace bytes; omitted/empty is
  `""`, and Windows slash variants remain distinct.
- Ticket validation occurs before provisional child creation. Required
  recovery authorization is consumed in the same reserving DB transaction;
  spawn and resume occur after gate release.
- Authorization expiry, prior consumption, or CAS loss rolls back the
  reserving insert, persists no run, cleans the provisional child, leaves the
  ticket burned, and requires a fresh artifact/ticket.
- No recovery-authorization reserved/claimed state is added and no
  unauthorized reserving row can appear in binding evidence or replay lookup.
- `child_conversation_id` remains non-null; no migration weakens its foreign
  key contract.
- The durable partial unique index rejects duplicate parent/intent pairs.
- The intent column is immutable and historical rows remain null.
- Same intent plus same request fingerprint returns the existing run; any
  semantic change returns `delegation_dispatch_intent_conflict`.
- An artifact-time retry with the same intent ID but changed task or working
  directory returns `delegation_dispatch_intent_conflict` with no artifact or
  ticket; it cannot reach validator adoption.
- A lost result followed by a fresh physical call uses the same intent ID and
  a fresh correlation ID, then converges through existing adoption.
- Existing parent-tool idempotency, correlation, continuation, replacement,
  recovery authorization, and provisional cleanup tests remain green.
- Grok's complete Phase 2 `tools/list` JSONL line is at most 7,300 bytes,
  retaining at least 380 bytes below the absolute 7,680-byte ceiling while all
  essential-guidance assertions remain green.

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
- The Node and Rust implementations produce byte-identical v3 arrays and
  digests from every shared golden vector without reading the filesystem.

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
   the existing run only on exact request-fingerprint equality, while changed
   task, working directory, or other semantics fail closed before artifact or
   ticket issuance.
9. Node and Rust produce the same ticket-v1 v3 digest from the strict pending
   call without path resolution, while legacy unbound and v2 digest bytes stay
   unchanged.
10. Required recovery authorization is consumed atomically with the reserving
   insert under the broker-held gate; authorization failure exposes no run and
   requires a fresh artifact/ticket.
11. Existing correlation, recovery authorization, lineage, budget, and binding
    rules remain authoritative and tested.
12. Plan/progress semantic authority remains in the Node validator; Codeg does
    not parse them for ticket admission.
13. No frontend or user-visible card change is required.
14. Temporary evidence is removed on normal paths and is unusable after its
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
  fields, parent/role authorization, and preservation of the caller-supplied
  pre-default working-directory string for v3 recomputation.
- `src-tauri/src/acp/delegation/broker.rs`: pre-side-effect ticket handoff and
  exact replay result, plus ownership of the mutation guard across ticket burn,
  provisional child creation, and reserving admission.
- `src-tauri/src/acp/delegation/run_store.rs`: gate-aware internal reserving
  admission, legacy-compatible fingerprinting, ticket-v1 v3 fingerprinting,
  unique replay classification, transaction-local recovery-authorization
  consume, and durable persistence without reacquiring the broker-held guard.
- `src-tauri/src/db/entities/delegation_task_run.rs`: nullable dispatch-intent
  column; the existing non-null child conversation FK is unchanged.
- A new additive migration: nullable dispatch-intent column, partial unique
  index, validation, and immutability protection without backfill. It does not
  alter `child_conversation_id` nullability.

### Phase 2 Skill package

- Skill write-ahead intent UUID and ticket-required flow.
- `scripts/validate-contract.mjs`: bounded-stdin
  `--ticket-v1-fingerprint --output-json` pre-artifact mode.
- `scripts/validate-contract.lib.mjs`: strict pending-call parser and the
  authoritative `deriveTicketV1RequestFingerprint` helper.
- Validator dual exact dispatch-intent shapes and ticket-v1 requirements;
  Plan/progress semantic reconciliation remains unchanged.
- Shared Node/Rust legacy and v3 golden-vector fixture.
- Lost-acknowledgement fixtures that distinguish stable intent ID from fresh
  physical correlation ID and retain the exact pending-call digest.

## Risks and Mitigations

| Risk | Mitigation |
| --- | --- |
| Artifact mode merely moves data to a file the model later reads | Skill contract and mutation tests prohibit inspection; validator output stays bounded |
| ACP CLI and companion do not share a filesystem | Advertise artifact mode only for same-host companion launches; fail closed otherwise |
| Temp evidence survives a crash | 60-second semantic expiry, private directory, normal cleanup, and 10-minute stale sweep |
| Tool schema growth breaks Grok framing | Compact before adding fields; enforce Phase 1 at 5,500 bytes, Phase 2 at 7,300, and retain the literal 7,680-byte absolute test |
| Validator takes longer than snapshot/ticket TTL | Return typed stale, discard result, and repeat one fresh artifact/validation cycle |
| Ticket becomes a second policy engine | Ticket validates only revision and durable identity; Node remains semantic authority |
| Old Skill still causes context growth | Legacy support is intentional; new Skill uses artifact mode whenever advertised and telemetry exposes legacy use |
| Process restart invalidates a valid-looking file | Snapshot expiry plus missing in-process ticket forces fresh validation; startup files are never trusted by path alone |
| New intent identity conflicts with correlation ID | Separate field names, persistence rules, retry rules, and regression tests |
| Ticket validation is placed after child side effects | Explicit error precedence and pre-side-effect integration tests |
| Node and Rust hash different path bytes | V3 hashes the supplied NFC/trimmed string, never an OS-resolved path, and both runtimes consume one golden-vector fixture |
| Authorization expires after an unauthorized run is reserved | Authorization consume and reserving insert share one guarded DB transaction; failure rolls back the run and cleans the provisional child |

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
