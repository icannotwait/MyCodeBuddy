# Task 4 Re-review - Codex (HIGH)

- **Fix implementer task:** `51d1e3d5-ef5a-46c1-9e09-072259fb99e3`
- **Lineage / original implementer task:** `aac0bcf4-09ac-482e-af17-9d2a2f28a960`
- **Fix producer commit:** `3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c`
- **Original reviewed producer:** `7b826557fe38fca115dfadd65c10b2eb0da54abf`
- **Scope:** Re-review `T4-CODEX-I1`, `T4-CODEX-I2`, and `T4-GROK-I1`
- **Mode:** Review only; no implementation

## Verdict

**`approve`**

All three blocking Important findings are closed. The fix restores fail-closed
root admission across every durable conversation association, preserves typed
non-retryable protocol failures at the transaction-local Design preflight, and
removes the MCP recovery path's automatic legacy restart. No new finding was
identified in the requested fix-round scope.

## Finding Disposition

| Finding | Status | Re-review evidence |
| --- | --- | --- |
| `T4-CODEX-I1` | **Closed** | `load_completion_protocol_for_conversation` now combines the conversation-owned workflow with workflow ids from every durable task run whose child is the conversation (`store.rs:5023-5050`). The database unique index on `(parent_conversation_id, workflow_kind)` makes the single owned lookup complete, while the child side uses all runs rather than the latest generation. A `BTreeSet` deduplicates and stabilizes evaluation. Missing headers return an error, corrupt headers retain typed loading errors, and decodable pairs resolve with deterministic rejection precedence `unsupported -> legacy -> allowed` (`store.rs:5053-5072`). Both masking regressions pass. |
| `T4-CODEX-I2` | **Closed** | `prepare_v2_design_self_review` now calls `require_owned_stored_v2_header` inside its transaction before decoding the full workflow model or making semantic writes (`store.rs:3404-3413`). `map_design_preflight_completion_error` preserves direct and nested `legacy_completion_protocol_read_only` / `unsupported_completion_protocol` variants as non-retryable store errors (`store.rs:3372-3393`). The concurrent corrupt-mode regression proves the exact unsupported code and unchanged graph, gate, Design binding, and attention state. |
| `T4-GROK-I1` | **Closed** | `process_recover_workflow` no longer calls `restart_legacy_if_required`; it enters `recover_workflow_core` directly (`listener.rs:2376-2394`). The listener-level regression returns `legacy_completion_protocol_read_only` for a historical workflow and asserts that the workflow count remains one, so no successor is created. |

## Review Counts

- **Critical:** 0
- **Important:** 0
- **Minor:** 0

These counts cover this requested three-finding closure review. The producer's
explicitly deferred optional minors and the previously documented broader Task
2/3 fixture debt were not fix-round targets and are not reclassified here.

## Verification Evidence

Fresh commands run at `3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c`:

| Command | Result |
| --- | --- |
| `cargo test --manifest-path src-tauri/Cargo.toml --test completion_protocol_v2 --features test-utils` | **Pass:** 32 passed, 0 failed, including both root association regressions |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils workflow_mutations_reach_v2_store_guards_without_rollout_restart` | **Pass:** 1 passed, listener recovery reaches the v2 store guard without successor creation |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils design_preflight_completion_protocol_errors_keep_stable_classification` | **Pass:** 1 passed, direct/nested protocol mappings retain exact non-retryable codes |
| `cargo test --manifest-path src-tauri/Cargo.toml --lib --features test-utils design_self_review_preflight_maps_concurrent_corrupt_header_without_writes` | **Pass:** 1 passed, concurrent corrupt header is unsupported and rolls back semantic writes |
| `cargo check --manifest-path src-tauri/Cargo.toml` | **Pass** |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features test-utils -- -D warnings` | **Pass** |
| `rustfmt --edition 2021 --check src-tauri/src/acp/delegation/listener.rs src-tauri/src/acp/delegation/workflow/store.rs src-tauri/tests/completion_protocol_v2.rs` | **Pass** |
| `git diff 7b826557fe38fca115dfadd65c10b2eb0da54abf 3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c --check` | **Pass** |

The repository-wide `cargo fmt --all -- --check` is not green because of
formatting drift in unrelated files outside this producer diff
(`connection.rs`, `launch_snapshot.rs`, `document_translate/service.rs`,
`lib.rs`, and `window_diagnostics.rs`). The three files changed by the fix
commit pass direct `rustfmt --check`. Cargo emitted the existing zero-byte
`codeg-mcp` sidecar warning during checks; it did not affect the results.

## Conclusion

**approve** - `T4-CODEX-I1`, `T4-CODEX-I2`, and `T4-GROK-I1` are closed with
production-path fixes and focused regressions. The Task 4 fix round may proceed
past this Codex re-review.

<!-- codeg-card-summary-v1
{"kind":"review","reviewed_task_id":"51d1e3d5-ef5a-46c1-9e09-072259fb99e3","lineage_task_id":"aac0bcf4-09ac-482e-af17-9d2a2f28a960","producer_commit":"3f0fb8f43c162e207f04d0813f7c1a6f84a3ca2c","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"All three Task 4 Important findings are closed: root admission scans every durable association and fails closed, Design preflight preserves typed non-retryable protocol failures inside its transaction, and MCP recovery no longer auto-restarts historical workflows.","report_file":".superpowers/sdd/task-4-review-codex-report-r2.md"}
-->
