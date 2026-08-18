# Independent Plan Review

## Verdict

**CHANGES REQUIRED**

Counts: **0 Critical, 7 Important, 1 Minor**.

## Findings

### Critical

None.

### Important

#### I-1: Tasks 1-3 have invalid `b2d_task_risk_v1` soft-signal sets and arithmetic

The routing block lists six production files for Task 1, seven for Task 2, and
eight for Task 3, but none of those Tasks activates
`broad_production_surface` ([Plan lines 80-186](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
Task 3 also changes an MCP companion/listener/transport contract across a
process boundary while declaring an empty soft-signal list. The approved
policy says every distinct active signal contributes once and defines five or
more production files as the broad-surface trigger ([Design, Task Risk
Policy](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
The hard triggers still make all three Tasks high, so the producer/reviewer
routes do not change, but the evidence objects and scores are invalid and must
be recomputed by the Plan Author before route derivation/review.

#### I-2: Tasks 1-2 cannot compile within their declared file ownership

Task 1 adds a required field to `ReservingRunInsert` and an eighth argument to
`request_fingerprint`, while its file list permits only `run_store.rs` and
`broker.rs` call-site updates ([Plan lines 385-395 and
516-520](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
Existing library-test constructors already occur outside that ownership, for
example [store.rs](../../../src-tauri/src/acp/delegation/store.rs),
[connection.rs](../../../src-tauri/src/acp/connection.rs),
`attention.rs`, `workflow/admission.rs`, `workflow/completion_evidence.rs`,
`workflow/recovery_tests.rs`, and `workflow/store.rs`. Task 2 similarly adds a
required field to `DelegationRequest` and `ContinueDelegationRequest`, whose
constructors also exist in `connection.rs`, `lifecycle.rs`, `transport.rs`,
and `workflow/recovery_tests.rs`. Rust struct literals do not acquire new
optional fields automatically, so the Task 1/2 `cargo test --lib` commands
will fail to compile unless those compatibility call sites are owned and
updated. The full File Map and commit commands also omit affected integration
fixtures such as `completion_protocol_v2.rs` and
`completion_transport_parity.rs`. Expand exact ownership and verification, or
introduce a constructor strategy that demonstrably preserves every current
call site while keeping each serial commit green.

#### I-3: The required shared cross-language binding fixtures are not planned

The Design requires one shared JSON fixture corpus to drive MCP JSON Schema,
listener deserialization, Rust validation, and Node validation, including all
positive bounds and negative grammar vectors ([Design lines
1391-1398](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
Tasks 1, 2, and 4 instead prescribe separate table-driven Rust, raw listener,
and independent Node fixtures, and no shared fixture file appears in the File
Map. The cross-language high-route hash in Task 7 covers fingerprint
canonicalization, not the binding wire grammar. Add an exact shared fixture
artifact, assign its ownership, and make every required consumer load it.

#### I-4: Insert-fixed actual Agent/profile identity lacks the mandated fault-injection test

Task 1 tests that lifecycle paths preserve the four binding columns, and Task
7 checks that ordinary integration runs retain the routed Agent/profile, but
no step fault-injects post-insert lifecycle/status paths and proves that both
`agent_type` and `profile_id` cannot change. That is an explicit Rust testing
requirement alongside binding immutability ([Design lines
1464-1467](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
Add the actual-identity mutation fault matrix to the owning store Task and its
focused GREEN evidence.

#### I-5: The durable boundary-change protocol omits the pending route-change intent

The approved change sequence requires progress to record the requested
Agent/profile and next generation as a pending route-change intent before the
Plan Author rewrites a never-admitted suffix ([Design lines
294-316](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
The Plan mentions only generation-drift validation and a generic boundary
scenario; it defines no progress shape, parser/validator rule, Skill mutation
sequence, or recovery test for that intent ([Plan lines 1003-1078 and
1272-1327](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
Without it, interruption during a selection change loses the durable
coordination record the Design requires. Define and test the intent end to end,
including availability confirmation, Author continuation, exact progress
resynchronization, full re-review, and clearing/settling the intent.

#### I-6: Task 7 does not make status-only durable refresh part of the Skill

Task 5 says a legitimate durable lifecycle advance returns a non-authorizing
status-refresh failure and that the parent updates only progress state before
requerying ([Plan lines
1065-1078](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)),
but Task 7's actual Skill rewrite sequence omits this branch and instead says
any durable mismatch blocks ([Plan lines
1308-1321](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
The Design explicitly requires status-only synchronization after compaction,
interruption, or resume before full admission ([Design lines
1250-1257](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
Put the exact status-only update/requery loop in Task 7's Skill directives and
workflow scenarios so normal lifecycle progress does not become a permanent
blocker.

#### I-7: Task 7 under-specifies fresh admission for document and final-review continuations

The Design requires a fresh complete query plus applicable admission before
every Design Fixer/reviewer continuation, every Plan Author/reviewer
continuation, and every later routed document/final decision ([Design lines
1149-1190](../../../docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md)).
Task 7 hard-codes Design work to document admission, covers only the initial
Plan review, and mentions only the fresh initial final reviewer. It does not
switch later Design work to full admission once routed documents exist, or
require fresh full admission before Plan Author/reviewer re-review
continuations and final-review continuation after a producer fix ([Plan lines
1308-1319](../../../docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md)).
Add explicit per-call cadence and regression assertions; otherwise these
unbound work units can be continued from stale durable evidence.

### Minor

#### M-1: Verification never derives bindings from the actual production Plan

Task 4 labels its GREEN step as production Plan-only derivation, but its
commands run only the test suite, no-argument Skill validation, and Prettier;
the final verification repeats those commands. Once `--derive-plan-routing`
exists, run it against this exact Plan and assert seven ordered Task bindings,
Grok/null generation 1, seven high routes, and the expected keys/fingerprints.
This closes the concern already recorded by the Plan Author that current
static validation cannot yet derive this Plan's fingerprints.

## Confirmed Strengths

- The Plan has seven contiguous Task headings and matching routing indices.
- All seven declared routes are high with Codex implementer, Codex primary,
  and Grok/null auxiliary identities.
- The published high-route digest recomputes exactly to
  `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
- Every Rust command disables default features and enables exactly
  `server,test-utils`; none enables `tauri-runtime`.
- The fixed Grok `7_680`/`7680` regression is retained.
- The Plan keeps orchestration identity separate from ACP
  `route_fingerprint`, keeps the parent coordinator-only, and does not
  reintroduce a writable manifest, Gate, completion Card, or platform-owned
  completion decision.
- Cross-namespace keyed discovery, lost-acknowledgement adoption, actual
  Agent/profile reconciliation, coordinated Plan/progress rewrite rejection,
  warning-only projection, and the under-500-line Skill check are otherwise
  assigned concrete Tasks and tests.
