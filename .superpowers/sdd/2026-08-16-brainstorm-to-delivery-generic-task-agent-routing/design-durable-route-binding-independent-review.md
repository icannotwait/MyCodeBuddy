# Independent Design Review: Durable Orchestration Route Binding

Reviewed commit `08224d24a01ceb9ca109c3de8ad1c0b2d8761289` against the
approved durable-route-binding brief and the current generic delegation, run
store, MCP companion/listener, Simple projection, validator, entity, and
migration surfaces.

## Findings

### Critical

None.

### Important

1. **The durable snapshot cannot prove that the requested Agent/profile was
   actually launched.** The fingerprint commits to the planned Agent/profile
   and expected keys (`Design:483-535`), but the generic backend validates only
   the binding's format and has no way to reverse the hash into a route. The
   durable run already stores the actual `agent_type` and `profile_id`
   (`src-tauri/src/db/entities/delegation_task_run.rs:58-59`), yet the proposed
   query page omits both (`Design:727-767`) and reconciliation compares only the
   mutable progress Agent/profile against the Plan and encoded key
   (`Design:876-889`). A call can therefore carry the validator's correct
   binding and expected work-unit key while supplying a different
   `agent_type` or `profile_id`; Plan, progress, key, and binding then agree and
   admission passes even though the child ran on the wrong route. Return the
   durable Agent/profile as bounded run-identity fields, compare them with the
   key/Plan/progress in full admission, and add wrong-actual-Agent and
   wrong-actual-profile negatives for first dispatch and replacement.

2. **Namespace filtering leaves a deletion-hiding path for a wrongly bound
   routed run.** The query selects rows bound to the requested namespace plus
   unbound rows with a key, but explicitly excludes rows bound to any other
   namespace (`Design:702-709`). Because the generic transport accepts any
   valid namespace and does not know that a `task|...` key belongs to B2D, a
   routed call can be admitted with the correct recognized key but a different
   valid namespace. If mutable progress is then deleted or rewritten, the B2D
   query cannot discover that durable row at all, so the bidirectional rule at
   `Design:871-892` has nothing to reject. This falls short of the brief's
   complete parent-scoped discovery requirement and its fail-closed namespace
   mismatch rule. The snapshot needs a bounded conflict set that also exposes
   nonmatching-namespace rows with potentially orchestrated work-unit keys (or
   an equivalent complete parent-scoped mechanism), and the test matrix needs
   the wrong-namespace-plus-deleted-mirror regression.

3. **A successful reservation followed by a lost acknowledgement cannot be
   reconciled.** Progress intentionally records a pre-reservation intent with
   no Task or child ID (`Design:642-646`), while the parent fills those IDs only
   after acknowledgement (`Design:984-991`). If the reserving transaction
   commits and the response is lost or compaction occurs before that update,
   the fresh query contains the run but reconciliation maps only by `task_id`,
   requires every bound row to have an exact mirror, and permits only
   status-only updates (`Design:871-883`). It therefore classifies the run as
   an extra/missing mirror and blocks before the idempotent retry or recovery
   path can run. Define a one-time, fail-closed intent-adoption rule using the
   exact expected key/binding and durable lineage fields (including unambiguous
   first/continue/replacement controls), then rerun admission. Add response-loss
   cases for first dispatch, continuation, and replacement. A genuinely
   deleted mirror must continue to fail.

4. **Initial Plan validation and progress initialization have a circular
   dependency.** The only static Plan mode requires `--plan`, `--progress`, and
   `--plan-rel-path` together and says it derives bindings for progress
   initialization (`Design:803-812`). The Plan Author must run that static
   validation immediately after writing the Plan (`Design:966-970`), but the
   parent does not synchronize Task entries and derived fingerprints into
   progress until after Plan approval (`Design:976-979`). The current validator
   also requires progress Task indices to exactly match the Plan, so an empty
   initial progress document cannot bootstrap this sequence. Specify a
   non-authorizing Plan-only derivation mode, or an explicit two-step sequence
   that obtains trusted derived bindings before synchronizing progress and then
   validates Plan/progress together before review. Add an end-to-end initial
   authoring/bootstrap test rather than only static-mode unit assertions.

### Minor

1. **The evidence wrapper cannot validate the claimed exact cursor chain.** The
   validator is required to check exact cursor chaining (`Design:793-797`), but
   each stored page contains only its outgoing `next_cursor`; it does not echo
   the cursor used to request that page (`Design:727-760`). Contiguous
   `page_start` values can prove row coverage but not that page N used page
   N-1's token. Either include a bounded `request_cursor`/page token in the raw
   envelope or narrow the offline validator requirement to the completeness
   properties it can actually observe, leaving token validation to the server.

## Retained Decisions

The Design retains the approved product and role decisions: Grok is the
default Task Agent; the Task Agent is workflow-level auxiliary; normal Tasks
use the Task Agent producer plus independent Codex primary review; high Tasks
force a Codex producer plus Codex primary and Task Agent auxiliary review;
active/admitted Tasks cannot switch Agent; Plan authoring uses an independent
Codex Plan Author following `writing-plans`; Design Fixer, document reviewers,
Task work units, and final review use distinct child conversations; and Simple
remains manifest-free, platform-gate-free, and warning-only in Rust projection.
The parent is described as coordinator rather than document or Task producer,
although the next revision should preserve the already-approved explicit ban
on parent-authored Skill prose and validator/tests as well as Design, Plan, and
Task code.

The canonical high-Task hash vector was independently recomputed and matches
`sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
The Rust verification examples all use
`--no-default-features --features server,test-utils` and do not enable the
default Tauri runtime.

## Counts And Verdict

- Critical: 0
- Important: 4
- Minor: 1
- Verdict: **CHANGES REQUIRED**

Approval is withheld because the Design has four Important gaps in durable
route proof, complete discovery, recovery, and Plan bootstrap ordering.
