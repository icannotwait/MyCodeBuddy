# Independent Design Rereview Round 2

Reviewed Design commit `f13c0c79fe46635e6b0bbb43be067299e5742a55`
against the approved durable-binding brief, the prior independent review, the
round-2 Fixer report, the complete current Design, and the relevant generic
delegation, run-store, MCP companion/listener, entity/migration, Simple
projection, key, and validator surfaces.

## Findings

### Critical

None.

### Important

None.

### Minor

1. **Retain the Grok fixed-size `tools/list` budget as an explicit schema-growth
   regression.** This Design adds one tool and nested binding fields to two
   existing tools (`Design:1296-1303`, `Design:1457-1463`), while the repository
   has a Grok compatibility test that hard-limits the serialized JSONL catalog
   to 7,680 bytes because Grok splits an 8,192-byte line
   (`src-tauri/src/acp/delegation/companion.rs:5434-5460`). A read-only
   serialization of the current selected schema is already approximately
   7,336 bytes before these additions. Existing tests should expose an
   overflow, so this is not an architectural blocker, but the implementation
   Plan should explicitly retain that test and budget the new schemas instead
   of weakening the literal or discovering the constraint only in broad
   regression.

## Prior Finding Dispositions

1. **Actual Agent/profile proof: resolved.** The query now returns the durable
   `agent_type` and `profile_id` (`Design:806-849`), full reconciliation compares
   them with the Plan, progress, and canonical key (`Design:1077-1080`), and the
   test matrix covers first, continuation, and replacement mismatches
   (`Design:1422-1428`). The fields come from the actual reserving row rather
   than being decoded from the caller-supplied fingerprint.

2. **Cross-namespace conflict discovery: resolved.** Snapshot selection is the
   deduplicated union of requested-namespace rows and every keyed row regardless
   of namespace (`Design:778-788`). A recognized B2D Task key in another
   namespace is explicitly blocking even without a progress mirror
   (`Design:1064-1088`), with positive and negative query/validator coverage at
   `Design:1429-1432` and `Design:1476-1485`.

3. **Lost acknowledgement versus a deleted mirror: resolved.** Progress records
   operation-specific unresolved intent before each call (`Design:694-720`).
   Adoption requires exactly one globally unambiguous intent/candidate pair,
   exact route identity, and exact first/continue/replacement lineage
   (`Design:990-1017`). The validator emits a non-authorizing deterministic
   action, the parent persists it, requeries, and reruns full admission
   (`Design:1019-1059`). With no unresolved intent, a deleted mirror remains an
   unmatched durable row and blocks. The added tests cover successful adoption,
   ambiguity, lineage mismatches, repeated adoption, and no-intent deletion
   (`Design:1433-1439`). As with all mutable-progress reconciliation, this proves
   the current validated state rather than cryptographically timestamping prior
   file contents; importantly, it cannot rewrite the durable binding, actual
   route, or lineage.

4. **Initial Plan bootstrap ordering: resolved.** The Plan-only
   `--derive-plan-routing` mode derives non-authorizing bindings without progress
   (`Design:900-919`). The execution flow now has a coherent Author derivation,
   independent parent rerun, progress initialization, combined static
   validation, full durable admission, and only then Plan review
   (`Design:1161-1196`). Boundary revisions repeat the same ordering
   (`Design:294-316`), and the test design includes the end-to-end bootstrap and
   inverse ordering control (`Design:1407-1418`).

5. **Cursor-chain observability: resolved.** Pages echo `request_cursor`
   (`Design:806-860`), and the offline evidence validator requires the first
   cursor to be null and every later cursor to equal the preceding
   `next_cursor` exactly (`Design:862-896`). The query and evidence tests include
   token tampering with otherwise contiguous row offsets (`Design:1476-1485`).

## Holistic Review

The durable binding remains separate from the existing ACP launch/config
`route_fingerprint`, is persisted in the reserving transaction, is immutable
after insert, participates in bound request idempotency, and is inherited by
continuations and replacements before side effects or budget consumption.
Operation-specific adoption preserves generic root/previous/lineage,
generation, child, replacement reason, Agent/profile, key, and binding rules.

High-review sequencing is internally consistent: primary intent/admission and
fresh reconciliation precede auxiliary intent/admission, after which the two
already-admitted children may run concurrently (`Design:1216-1224`). This avoids
two unresolved intents while preserving the approved parallel review fan-out.

All retained user decisions remain intact: Grok is the default Task Agent; the
Task Agent is workflow-level auxiliary; normal Tasks use Task Agent production
and independent Codex primary review; high Tasks force Codex production plus
independent Codex primary and Task Agent auxiliary review; admitted Tasks cannot
switch Agent; Plan Author, Design Fixer, document reviewers, Task work units,
and final reviewer remain independent; and the parent is explicitly limited to
coordination, progress updates, and adjudication rather than artifact
authorship. Simple remains manifest-free and platform-gate-free, and Rust
projection remains warning-only.

The canonical hash vector still recomputes to
`sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
The Design's Rust commands consistently disable default features and enable
exactly `server,test-utils`; none enables the default Tauri runtime. Placeholder
and contradiction scans found no new issue, and the Design-only diff passes
`git diff --check`.

## Counts And Verdict

- Critical: 0
- Important: 0
- Minor: 1
- Verdict: **APPROVED**

The prior blocking findings are genuinely resolved. The remaining Minor is a
focused implementation-test retention concern, not a Design correctness gap.
