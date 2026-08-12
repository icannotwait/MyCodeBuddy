# Final review consolidated fix report

Status: DONE_WITH_CONCERNS

Base: `dc3e1220f060ae1f1f9c6da92841656ed90600d7`

Final commit: the commit containing this report, with subject
`fix(workflow): close simple successor review findings`. Its exact SHA is
reported in the handoff and encoded in the final review package name. A Git
commit cannot embed its own final SHA without changing that SHA.

## Result

The final-review wave closes all four accepted findings:

1. Archived-to-Simple creation now persists a durable bootstrap intent and
   admits it through the normal linked foreground prompt path after connect.
2. Successor replay treats the immutable source link as identity and returns
   the live descriptor's normalized Plan/progress locators.
3. A stale progress Plan path marks every Task consuming that snapshot
   `out_of_sync` without changing its declared lifecycle status.
4. Archived graph and retired-error navigation advertise successor creation
   only when the same bounded, contained UTF-8 Plan read used by creation is
   eligible.

Production workflow v2 remains archived and read-only. No frontend or retired
mutation surface was added.

## Architecture and durable state

Migration `m20260812_000001_simple_successor_bootstraps` adds one durable row
per source workflow and successor conversation. The row stores the validated
client request token, current bounded bootstrap prompt, frozen
`admitted_prompt`, `pending|admitted` status, and timestamps. Unique indexes on
both identities converge replays/races. Foreign keys to the Simple descriptor
and source workflow cascade cleanup; deleting the public successor therefore
releases the link and permits explicit recreation.

State transitions are:

```text
no successor
  -> transaction(conversation + Simple descriptor + pending bootstrap)
  -> commit
  -> usable desktop/server ACP connection
  -> keyed in-process admission lock
  -> send_prompt_linked_with_message_id(simple-bootstrap-<bootstrap-id>)
  -> admitted + admitted_prompt + admitted_at
```

Conversation, descriptor, and bootstrap insertion roll back together. A unique
source-link loser loads the winner and retains the winner's token/turn. Replay
uses current normalized descriptor locators; before admission it refreshes the
pending compatibility prompt without creating another row or turn.

Both desktop and server `acp_connect` call
`admit_simple_successor_bootstrap_after_connect` after spawn. A send failure
leaves the row pending, disconnects the spawned connection with
`AbandonedConnect`, maps through the existing ACP/App error path, and permits a
later legitimate connect to retry. A successfully admitted row is a no-op on
later connects.

## Files changed

- `src-tauri/src/commands/simple_workflow.rs`: transaction, replay,
  coordinator, production prompt sink, post-connect hook, and focused tests.
- `src-tauri/src/commands/acp.rs`: desktop post-spawn admission hook.
- `src-tauri/src/web/handlers/acp.rs`: server post-spawn admission hook.
- `src-tauri/src/db/entities/simple_successor_bootstrap.rs`: durable entity and
  lifecycle enum.
- `src-tauri/src/db/migration/m20260812_000001_simple_successor_bootstraps.rs`:
  table, uniqueness, foreign keys, cascade regression.
- `src-tauri/src/db/entities/mod.rs`, `src-tauri/src/db/migration/mod.rs`:
  registries.
- `src-tauri/src/acp/delegation/workflow/simple.rs`: shared locator and Plan
  eligibility rule.
- `src-tauri/src/acp/delegation/workflow/error.rs`: retired-error availability
  and missing/oversized/invalid-UTF-8 regressions.
- `src-tauri/src/acp/delegation/workflow/project.rs`: honest archived
  availability and stale progress propagation/tests.
- `src-tauri/src/acp/delegation/workflow/mod.rs`: exports for the shared rule.
- `src-tauri/src/acp/delegation/broker.rs`,
  `src-tauri/src/acp/delegation/listener.rs`: test-only historical fixture
  expectations changed from `true` to `false` because those fixtures have no
  readable Plan; production code is unchanged.
- This report.

## TDD evidence

Every Rust command used `RUST_MIN_STACK=16777216` and Cargo processes were run
serially. Genuine RED evidence recorded during implementation, before the
corresponding production edits:

- `cargo test --lib --features test-utils simple_projection_stale_plan_marks_every_snapshot_task_out_of_sync -- --nocapture`
  failed (runner exit `1`): one test failed because the stale node was `InSync` rather than
  `OutOfSync`.
- The missing-Plan archived projection/retired-error focused cases exited
  nonzero: each observed `can_create_simple_successor=true` instead of `false`.
- The initial bootstrap persistence regression exited nonzero because
  `simple_successor_bootstraps` did not exist.
- The locator-update replay regression exited nonzero with
  `WorkflowIdentityCorrupt` after normal descriptor registration changed the
  live Plan locator.
- The first admission concurrency compilation exited nonzero because
  `SimpleBootstrapPromptSink` and the keyed coordinator did not yet exist.

The final historical-filter RED was reproduced after implementation but before
its authorized fixture correction:

```text
cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
runner exit 1; 5 passed, 2 failed
broker.rs: navigation.can_create_simple_successor was false
listener.rs: left Bool(false), right true
```

Those two fixtures do not create a readable Plan, so `false` is the accepted
contract result. Only their three boolean expectations changed.

Fresh GREEN evidence on the final source tree:

```text
cargo test --lib --features test-utils simple_successor -- --nocapture
exit 0; 24 passed, 0 failed

cargo test --lib --features test-utils simple_projection -- --nocapture
exit 0; 8 passed, 0 failed

cargo test --lib --features test-utils workflow_v2_retired -- --nocapture
exit 0; 7 passed, 0 failed

cargo test --lib --features test-utils simple_workflow_migration -- --nocapture
exit 0; 2 passed, 0 failed

cargo test --no-default-features --features server --bin codeg-server --lib simple_successor -- --nocapture
exit 0; library 23 passed, 0 failed; server binary 0 selected; no warnings
```

The focused suite covers transaction rollback, source-link race convergence,
same/different-token replay, public deletion/recreation, live locator replay,
concurrent admission, admitted replay no-op, send failure/retry, real linked
prompt dispatch, failed-connect cleanup, migration uniqueness/cascade, stale
node lifecycle preservation, bounded Plan eligibility, and desktop/server hook
parity.

## Formatting and diff evidence

- `rustfmt --check --edition 2021 --config skip_children=true` over the 11
  implementation/entity/migration/handler files exited `0`.
- The same check including `broker.rs` and `listener.rs` exited `1` because
  those two pre-existing 36k/10k-line historical test files contain unrelated
  formatting drift at imports and old fixture wrappers. Applying whole-file
  rustfmt would violate the narrow test-assertion ownership. The three changed
  boolean lines themselves require no formatting change.
- `git diff --check` exited `0`.
- `git diff -w --stat` and per-file inspection confirmed substantive changes
  remain limited to the owned source/test surfaces; no protected temporary or
  runtime path is included.

## Adjudication and risks

- Replay/concurrency: one source/successor/bootstrap identity; keyed admission
  serializes concurrent attempts, and a successful send plus durable admitted
  update makes later connects no-ops. Losing tokens do not replace the durable
  winner.
- Rollback: forced failure after descriptor registration leaves no candidate
  conversation, descriptor, auto-title job, or bootstrap row.
- Send failure: pending is durable and retryable; the spawned connection is not
  left orphaned.
- Runtime parity: desktop 24/24 and server 23/23 focused filters exercise the
  same shared post-connect hook.

Remaining concern: the lower linked-prompt transport has no durable idempotency
fence spanning a process crash or a database failure after send succeeds but
before `admitted` is committed. This implementation therefore provides
serialized exactly-once admission under concurrent/replayed connects in one
process, but deliberately does **not** claim crash-level exactly-once delivery.
The unrelated historical full-file rustfmt drift described above also remains.
