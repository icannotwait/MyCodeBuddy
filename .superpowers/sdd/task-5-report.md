# Task 5 Report: V2 Admission and Typed Terminal Failure

## Status

**IMPLEMENTATION COMPLETE; INDEPENDENT CODEX/GROK REVIEW PENDING**

- Work unit: `task|5|implementer|codex|none`
- Scope: Plan Task 5 only
- Task 6: not started

Task 5 now admits workflow children only when the durable completion-protocol
header is exactly `(2, v2_enforce)`, returns only immutable exact-v2 MCP
bindings, and classifies workflow-bound terminal processing before Card/report
parsing. Permanent protocol failures become typed durable failures outside the
`persistence_error` / `PendingTerminalRetry` rail; transient database lookup
failures retain the existing bounded retry policy.

## Implementation

- Added `TerminalCompletionProtocol::{Standalone, V2}` and made terminal
  protocol lookup fail closed for every claimed workflow binding.
- Moved exact-pair admission ahead of budget reservation, run insertion,
  binding creation, process launch, prompt delivery, and MCP feature exposure.
- Made continuation and replacement admission detect a missing workflow header
  still claimed by durable run bindings and return
  `unsupported_completion_protocol` without side effects.
- Restricted `load_workflow_child_mcp_binding` to immutable exact-v2 bindings;
  dangling, corrupt, historical, and inconsistent headers return stable typed
  protocol errors.
- Revalidated the exact protocol pair when loading admitted completion
  instructions, so `(2, v2_shadow)` cannot receive the canonical v2 contract.
- Split broker terminal handling before Card/report parsing. Historical v1
  returns `legacy_completion_protocol_read_only`; inconsistent, corrupt, and
  dangling headers return `unsupported_completion_protocol`.
- Kept only transient terminal lookup failures on the bounded persistence retry
  rail. Permanent protocol failures are durably settled as `Failed`, with the
  same stored code projected to wait reports and terminal events and no pending
  retry registration.
- Reclassified the protocol inside the terminal CAS transaction and cleared
  stale Card, completion, and remediation columns atomically when authority
  rejects the terminal attempt.
- Removed production v2-shadow terminal comparison/fallback from this path
  while retaining the test-only comparison helper used by historical tests.
- Preserved standalone Card-summary behavior and the valid v2 semantic inputs:
  `complete_work`, explicit terminal conclusions, eligible bounded-report
  conclusions, ambiguity attention, and typed user adjudication.
- Changed test-only terminal lookup fault injection from a one-shot flag to a
  bounded counter so retry success and retry exhaustion are deterministic.

`listener.rs` changed only to enforce the immutable MCP binding/admission
boundary. No Task 6 restart tool, route, schema, API, or UI removal was made.

## TDD Evidence

RED was observed before the corresponding production changes:

- Non-v2 admission inserted a reserving run before rejecting the workflow.
- Terminal protocol lookup returned a raw optional header pair rather than a
  typed standalone/v2 classification.
- A broker protocol rejection left the durable task row `Running`.
- `(2, v2_shadow)` still received the canonical completion instruction.
- Transient-exhaustion coverage could not be expressed with the old one-shot
  lookup fault injection.
- A preexisting Card survived a typed terminal protocol failure.
- A dangling continuation attempted an unbound resume and returned
  `unresumable` instead of `unsupported_completion_protocol`.

Fixture-only setup issues found during RED construction were corrected before
using the tests as behavioral evidence: an obsolete duplicate gate-state row,
an incorrect module path, a SQLite check constraint that blocked direct corrupt
mode injection, and a replacement fixture whose source had already completed.

GREEN was then observed for first dispatch, continue, and replacement
admission; exact-v2 launch and immutable MCP binding; dangling/corrupt header
classification; permanent and transient terminal dispositions; terminal CAS
stale-authority cleanup; row/wait/event code parity; standalone Card display;
and every retained v2 semantic input.

## Verification

- `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils`
  - Pass: 34 passed, 0 failed.
- `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils pending_terminal_retry`
  - Pass: 1 passed, 0 failed, 4278 filtered out.
- Focused admission, terminal, dangling-header, exact-pair instruction,
  committed-binding, standalone Card, and semantic-input regressions passed
  during TDD iteration.
- `cargo check --manifest-path src-tauri/Cargo.toml`: pass.
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings`: pass.
- `rustfmt --edition 2021 --check` over all six touched Rust files: pass.
- `git diff --check` and `git diff --cached --check`: pass before the producer
  commit.
- Scope audit: exactly six Task 5 implementation/test files in the producer
  commit; no Task 6 restart-surface deletions.

Cargo emitted the existing local packaging warning that the ignored
`codeg-mcp` sidecar is a zero-byte placeholder. It did not affect compilation,
linting, or tests and is not part of the producer diff.

## Producer Commit

- `d145b2c2b7a1811d4c11905935227625e0849e44` -
  `fix: fail closed on workflow terminal protocol errors`

## Conclusion

done

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Enforced exact-v2 workflow admission and immutable MCP bindings, then moved workflow terminal handling onto a typed fail-closed host surface with permanent protocol failures outside the persistence retry rail while preserving standalone Card behavior and all v2 semantic inputs.","commits":[{"sha":"d145b2c2b7a1811d4c11905935227625e0849e44","subject":"fix: fail closed on workflow terminal protocol errors"}],"tests":{"status":"passed","passed":35,"failed":0,"summary":"The 34-test completion_protocol_v2 target and one pending-terminal-retry library regression passed, followed by cargo check, strict all-target Clippy, scoped rustfmt, and working/cached diff checks."},"concerns":["The local build continues to emit the existing zero-byte codeg-mcp sidecar packaging warning; it is outside this producer diff.","Independent Codex and Grok review is pending before Task 6."],"report_file":".superpowers/sdd/task-5-report.md"}
-->

## Fix Round 1

Both independent reviewers requested changes on producer `d145b2c2`. Fix round
1 closes all three Important findings and the requested cheap matrix gaps in a
new producer commit:

- `T5-CODEX-I1`: added typed database availability classification before
  stringification. SeaORM connection-acquire timeout/closed, SQLx pool
  timeout/closed, closed-connection, and SQLite busy/locked errors stay on the
  transient rail. Retryable workflow-header `Persistence` stays transient;
  permanent query failures and corrupt/unknown header decoding remain
  permanent typed failures.
- `T5-CODEX-I2`: terminal publication now derives its conversation status from
  the persisted winning report. If the in-transaction protocol recheck changes
  a producer completion into `Failed`, durable run/conversation state, waiter
  report, `ConversationStatusChanged`, and the terminal event all publish the
  same `Failed` / `Cancelled` disposition and stable code.
- `T5-CODEX-I3` / `T5-GROK-I1`: the pre-spawn MCP binding loader now returns
  structured `TaskStoreError` through `RunStore` and
  `WorkflowLaunchLoadError`, preserving protocol codes and transient versus
  permanent persistence classification. First-dispatch and replacement
  protocol races retain a durable failed row; generic binding failures keep
  the existing provisional compensation behavior. Protocol failure settlement
  skips workflow semantic side effects.
- `T5-CODEX-M1` / `T5-GROK-M1`: added all five rejected protocol pairs through
  first, continue, and replacement launch paths, plus permanent terminal host
  parity for all five pairs, unknown version, and corrupt mode.
- `T5-GROK-M2`: added transient terminal lookup followed by successful exact-v2
  semantic settlement with no Card authority.
- `T5-GROK-M4`: added dangling replacement rejection before process spawn in
  addition to the post-admission dangling replacement race.

No Task 6 restart tool, route, schema, API, transport, or UI removal was made.

### Fix-Round TDD Evidence

RED was observed before each production fix:

- Connection availability failed the new mapper regression as a permanent
  error.
- The gated terminal CAS race persisted `Failed` / `Cancelled` but emitted
  stale `PendingReview` through `ConversationStatusChanged`.
- Gated pre-spawn races returned `spawn_failed` for first dispatch,
  `admission_failed` for continuation, and cleanup-induced
  `workflow_not_found` for dangling replacement instead of the stable protocol
  codes.

GREEN was then observed for typed pool/connection classification, the
transaction-authoritative status race, and all three pre-spawn protocol races.
An initial unknown-version matrix fixture hit SQLite's protocol-version check
constraint before terminal processing; the fixture was corrected to use the
same explicit check-constraint bypass as the established corrupt-header tests.

### Fix-Round Verification

- Full `completion_protocol_v2` integration target: 34 passed, 0 failed.
- `terminal_protocol` library filter: 6 passed, 0 failed.
- Committed exact-v2 `workflow_binding` filter: 2 passed, 0 failed.
- First/continue/replacement `pre_spawn_protocol_race` filter: 3 passed,
  0 failed.
- Five-pair `workflow_launch` variant filter: 2 passed, 0 failed.
- Dangling header/admission/binding filter: 5 passed, 0 failed.
- Transient retry filter: 7 passed, 0 failed.
- `pending_terminal_retry`: 1 passed, 0 failed.
- Generic first/continue binding-failure compatibility, direct dangling MCP
  binding, and structured launch-error mapping regressions passed.
- `cargo check`: passed.
- Strict all-target Clippy with `-D warnings`: passed.
- Scoped Rustfmt and working/cached diff checks: passed.

The focused library filter counts overlap. Fix round 1 added nine distinct
regressions. Cargo continued to emit only the existing ignored zero-byte
`codeg-mcp` sidecar packaging warning.

### Fix-Round Producer Commit

- `0239f462bf33c922cefe4fbe172f881f38479aaa` -
  `fix: close terminal protocol fail-closed gaps`

Independent Codex and Grok re-review is pending before Task 6.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done","summary":"Closed Task 5 review gaps by preserving typed connection-availability retry, publishing transaction-authoritative terminal status, and carrying stable protocol codes through first/continue/replacement pre-spawn MCP binding races; completed the requested launch, terminal, transient-success, and dangling-replacement matrices.","commits":[{"sha":"0239f462bf33c922cefe4fbe172f881f38479aaa","subject":"fix: close terminal protocol fail-closed gaps"}],"tests":{"status":"passed","passed":43,"failed":0,"summary":"The 34-test completion_protocol_v2 target and nine new fix regressions passed; terminal, binding, launch, dangling, transient, pending-retry, compatibility, cargo check, strict Clippy, Rustfmt, and diff gates were also green."},"concerns":["The existing zero-byte codeg-mcp sidecar packaging warning remains outside this diff.","Independent Codex and Grok re-review is pending before Task 6."],"report_file":".superpowers/sdd/task-5-report.md"}
-->
