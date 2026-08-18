# Plan Author Revision 1

Continue the same Plan Author work unit. Re-inspect Git, the current Plan,
the independent review, the approved Design, and current sources. Treat
pre-review reasoning as provisional. Edit only:

`docs/superpowers/plans/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding.md`

Do not implement production code. Do not edit the Design, Skill, validator,
Rust sources, or progress JSON. Append a revision note to
`.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-author-report.md`.

Independent Plan review of the current Plan is CHANGES REQUIRED:
0 Critical, 7 Important, 1 Minor. The parent adjudicated all eight items as
valid Design/implementability gaps. None changes user-owned requirements.
Revise the complete latest Plan until every item below is resolved.

Working directory:
`/Users/pengchao/Documents/Codeg_Fork/codeg/.worktrees/b2d-generic-task-agent-routing`

Read first:

- `.superpowers/sdd/2026-08-17-brainstorm-to-delivery-durable-orchestration-binding/plan-review.md`
- `docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md`
- current Plan
- `writing-plans`

Keep generation 1 as grok/null. Keep all seven Tasks high with Codex
implementer, Codex primary, and Grok auxiliary. Keep every Rust command on
`--no-default-features --features server,test-utils`. Do not weaken the
Grok `7_680`/`7680` budget.

## Required revisions

### I-1 — Recompute Tasks 1-3 risk evidence

Activate every distinct live soft signal with non-empty evidence.

- Task 1 File Map has six production files. Add
  `broad_production_surface` score 1. Recalculate `score`.
- Task 2 File Map has at least five production files. Add
  `broad_production_surface` if that remains true after I-2 ownership
  expansion, plus any other newly active signals.
- Task 3 changes companion/listener/transport across a process boundary
  and has eight production files. Add `cross_runtime_or_process` score 2
  and `broad_production_surface` score 1. Recalculate `score`.
- Keep hard triggers. Routes stay high. Update both the routing JSON and
  each Task body's human-readable risk paragraph so they agree.

### I-2 — Own every compile-breaking call site

Adding fields to `ReservingRunInsert`, `DelegationRequest`,
`ContinueDelegationRequest`, and an extra `request_fingerprint` argument
will not compile existing struct literals. Repository evidence already
includes constructors in at least:

- `src-tauri/src/acp/delegation/run_store.rs`
- `src-tauri/src/acp/delegation/broker.rs`
- `src-tauri/src/acp/delegation/store.rs`
- `src-tauri/src/acp/delegation/attention.rs`
- `src-tauri/src/acp/delegation/listener.rs`
- `src-tauri/src/acp/connection.rs`
- `src-tauri/src/acp/delegation/workflow/admission.rs`
- `src-tauri/src/acp/delegation/workflow/completion_evidence.rs`
- `src-tauri/src/acp/delegation/workflow/recovery_tests.rs`
- `src-tauri/src/acp/delegation/workflow/store.rs`
- `src-tauri/tests/delegation_session_reuse_integration.rs`
- `src-tauri/tests/completion_protocol_v2.rs`
- `src-tauri/tests/completion_transport_parity.rs`

Expand Task 1 and Task 2 file ownership, GREEN commands, and commit
`git add` lists so each Task's commit is independently green. Re-scan at
revision time and name every remaining literal. Do not rely on Rust
adding optional fields automatically.

### I-3 — Shared cross-language binding fixture corpus

Add one exact shared JSON fixture file to the File Map. Task-own it.
MCP JSON Schema, listener deserialization, Rust validation, and Node
validation must load that same corpus. Include valid min/max generation,
namespace bounds, exact lowercase fingerprints, and the Design's negative
grammar vectors. Do not leave three independent fixture tables as the
only source.

### I-4 — Agent/profile immutability fault injection

In the store Task, add a focused fault-injection matrix proving that no
post-insert lifecycle/status path can mutate durable `agent_type` or
`profile_id`, in addition to the four binding columns. Name the test and
the GREEN command.

### I-5 — Pending route-change intent

Specify the progress shape, validator rules, Skill mutation sequence, and
recovery tests for the Design's pending route-change intent:

1. Complete durable snapshot and full admission first.
2. Prove affected Tasks are pending with no durable row.
3. Record requested Agent/profile and next generation as pending
   route-change intent in progress.
4. Confirm Agent/profile availability.
5. Continue Plan Author to append the generation and rewrite only the
   never-admitted suffix.
6. Plan-only derive, parent resync of only never-admitted affected
   entries, combined static + full admission, then full Plan re-review.
7. Clear/settle the intent after approval.
8. Cover interruption/recovery while the intent is pending.

### I-6 — Status-only durable refresh in Task 7

Put the exact status-only update/requery loop into Task 7 Skill
directives and workflow scenarios. A legitimate newer durable lifecycle
status is not a permanent blocker: update only progress state, requery,
and rerun full admission. Do not rewrite identity or binding to make a
mismatch pass.

### I-7 — Fresh admission cadence for document and final-review work

In Task 7, require a fresh complete snapshot plus the applicable
admission mode before every Design Fixer/reviewer continuation, every
Plan Author/reviewer continuation, and every final-review continuation
after a producer fix. Once a reviewed Plan and synchronized progress
exist, later document and final-review decisions use full admission, not
document-admission. Add regression assertions.

### M-1 — Derive the production Plan

Once `--derive-plan-routing` exists, Task 4 GREEN or final verification
must run it against this exact Plan path and assert seven ordered Task
bindings, Grok/null generation 1, seven high routes, and the expected
keys/fingerprints.

## After revision

Re-read the Design Testing/Success Criteria sections and your own
self-review checklist. Keep one unfenced routing block aligned with
headings. Report status, what changed, and remaining concerns. Do not
paste the whole Plan into chat.
