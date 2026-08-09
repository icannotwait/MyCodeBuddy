# Task 5 Re-Review Report (Codex, HIGH)

## Review Identity

- `reviewed_task_id`: `0a4e6cc1-1fa2-47b6-95b4-6fc5995e29d4`
- Prior lineage: `7c63eb27` / `20149d71`
- Producer fix commit: `0239f462bf33c922cefe4fbe172f881f38479aaa`
- Platform artifact / report tip: `ec180b603fdc7d49ca362e4afc1dc752539f1206`
- Scope: Plan Task 5 and the approved terminal fail-closed host surface
- Mode: review only; no production implementation

## Verdict

`approve_with_minors`

All three previously Important findings are closed. The remaining findings are
nonblocking consistency and test-observability gaps; neither permits launch or
terminal semantic fallback.

## Required Fix Re-Check

### T5-CODEX-I1 - Closed

`is_transient_db_error` preserves typed `ConnectionAcquire`, SQLx pool timeout
and pool-closed, closed-connection, and SQLite busy/locked failures before
stringification (`store.rs:557`). `run_store::map_db_err` uses it before the
permanent fallback (`run_store.rs:953`). Retryable workflow-header
`WorkflowStoreError::Persistence` remains transient, while direct nontransient
query errors remain `Permanent` and corrupt/unknown header decoding becomes a
permanent `WorkflowAdmission` rejection (`run_store.rs:976`). The focused
mapper test covers each requested class (`run_store.rs:5581`).

### T5-CODEX-I2 - Closed

The terminal publication path now derives failed/canceled conversation status
from the persisted winning report (`broker.rs:8113`). The gated CAS regression
changes the protocol header after the pre-read and proves durable run,
conversation projection, wait report, `ConversationStatusChanged`, and the
terminal event all agree on `Failed` / `Cancelled` and the stable protocol code
(`broker.rs:31241`). No stale `PendingReview` event remains on this path.

### T5-CODEX-I3 / T5-GROK-I1 - Closed

`WorkflowLaunchLoadError` retains `TaskStoreError` and prefers its workflow
admission code over `spawn_failed` / `admission_failed` fallbacks
(`broker.rs:2505`). First-dispatch protocol rejection is durably settled before
spawn instead of being erased by provisional compensation (`broker.rs:4445`).
The first, continuation, and replacement checkpoint races all assert the
stable protocol code and no spawn/resume growth (`broker.rs:35651`, `35698`,
`35771`).

## Matrix Check

The previously missing pair coverage is materially closed:

- The launch matrix exercises all five rejected decodable pairs through first,
  continuation, and replacement paths (`broker.rs:35840`).
- The terminal host matrix exercises the same five pairs plus unknown version
  and corrupt mode, checking durable row, conversation projection, wait report,
  status event, terminal event, Card/completion fields, attention, retry state,
  and shadow metrics (`broker.rs:31344`, `31450`).
- Transient-then-v2 success and dangling replacement admission have dedicated
  regressions.

## Findings

### Critical

None.

### Important

None.

### Minor

#### T5-CODEX-M1: The instruction loader retains a narrow second pre-spawn classification race

After `workflow_child_mcp_binding` has returned the typed exact-v2 binding, the
broker separately calls `append_admitted_completion_instruction`. Its loader
re-reads the full workflow model (`workflow/admission.rs:775`) using `map_db`,
which recognizes SQLite contention but not the new connection-availability
classifier (`workflow/admission.rs:766`). If the workflow becomes dangling or
its header becomes undecodable between those two reads, the second read can
surface `completion_instruction_binding_failed` rather than
`unsupported_completion_protocol`; a connection-availability failure can also
lose `persistence_error`. The launch still fails closed before spawn, so this
does not reopen an Important launch-side-effect defect. A later cleanup should
load the typed header here or consume the already validated binding, with a
gate between the two reads for regression coverage.

#### T5-CODEX-M2: Matrix rows do not assert every named plan observable directly

The matrices now cover the required protocol pairs and principal host
surfaces, but the launch rows infer prompt/MCP-feature absence from zero
spawn/resume calls and do not snapshot budget reservations. The terminal
helper asserts persisted Card/completion fields, attention, retry, events, and
empty shadow metrics, but does not use explicit Card-parser/shadow-comparator
call counters or a complete semantic-table count snapshot. This is a coverage
precision gap, not evidence of an observed behavior failure.

## Verification Evidence

Fresh verification at `ec180b603fdc7d49ca362e4afc1dc752539f1206`:

| Command / filter | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils` | 34 passed, 0 failed |
| library filter `terminal_protocol` | 6 passed, 0 failed |
| library filter `pre_spawn_protocol_race` | 3 passed, 0 failed |
| library filter `workflow_launch` | 2 passed, 0 failed |
| library filter `pending_terminal_retry` | 1 passed, 0 failed |
| library filter `workflow_binding` | 2 passed, 0 failed |
| library filter `dangling_workflow_admission` | 2 passed, 0 failed |
| `git diff 0239f462^ 0239f462 --check` | passed |

Cargo emitted the existing ignored zero-byte `codeg-mcp` sidecar packaging
warning. It is outside the producer diff and did not affect the tests.

## Conclusion

**approve_with_minors**

The fix round closes T5-CODEX-I1, T5-CODEX-I2, and T5-CODEX-I3 /
T5-GROK-I1 with fresh green mapper, CAS-race, launch-race, host-matrix, and
retry evidence. Task 5 can pass the review gate; the two residual minors may be
handled in later cleanup without reopening the terminal fail-closed contract.

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"0a4e6cc1-1fa2-47b6-95b4-6fc5995e29d4","lineage_task_id":"7c63eb27 / 20149d71","producer_commit":"0239f462bf33c922cefe4fbe172f881f38479aaa","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"All three prior Important findings are closed with fresh green connection-classification, authoritative-status, pre-spawn race, launch-matrix, and terminal-host evidence. Two nonblocking gaps remain: a narrow second instruction-loader classification race and incomplete direct instrumentation of every named matrix observable.","report_file":".superpowers/sdd/task-5-review-codex-report.md"}
-->
