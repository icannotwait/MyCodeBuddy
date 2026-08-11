# completion_protocol_v2 consolidated repair report

Status: DONE_WITH_CONCERNS

Base: `36aa5bd48b6ba0b072479ae08d92258e2474afe9`

## Implemented

- Updated the conflicting owned-v2/bound-legacy loader case to assert stable
  archived-v2 retirement precedence on repeated loads.
- Replaced retired production feature-token parsing in the historical semantic
  fixture with explicit root/child historical feature values.
- Kept retired MCP and HTTP v2 mutation routes retired; historical semantic
  mutation cases now use the existing transaction adapters under lexical
  fixture permission.
- Scoped the final-delivery listener future with historical fixture permission.
- Propagated that already-active permission across the listener's nested Unix
  and Windows per-connection Tokio spawns only in `test`/`test-utils` builds.
- Changed archived navigation to select only the four required workflow-header
  fields and reuse the narrow completion-header decode-error classifier.
- Added a regression proving a corrupt completion mode remains the stable,
  non-retryable `unsupported_completion_protocol` error.

Production v2 publication, feature-token parsing, completion mutation routes,
and prompt fence ordering were not re-enabled or reordered.

## Files changed

- `src-tauri/src/acp/delegation/listener.rs`
- `src-tauri/src/acp/delegation/workflow/error.rs`
- `src-tauri/src/acp/delegation/workflow/mod.rs`
- `src-tauri/src/acp/delegation/workflow/store.rs`
- `src-tauri/tests/completion_protocol_v2.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/completion-protocol-v2-repair-report.md`

## TDD evidence

### Corrupt-header classification

RED, run before the production query/classifier change by the implementer:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --lib --features test-utils acp::delegation::workflow::error::tests::archived_workflow_navigation_preserves_corrupt_header_classification -- --exact
```

Result: `0 passed; 1 failed`. The new assertion expected
`UnsupportedCompletionProtocolHeader`, while archived navigation returned the
generic `Persistence` classification from full-model enum decoding.

GREEN, independently rerun by the controller after the complete change:

```text
1 passed; 0 failed; 4350 filtered out
```

### Nested listener fixture scope

RED: after wrapping only `listener.run(...)`, the exact final-delivery case
still returned `completion_scope_changed`; Tokio task-local permission did not
cross the listener's nested per-connection spawn.

GREEN:

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 session_2889_and_final_drift_have_no_format_repair_escape -- --exact
```

Result: `1 passed; 0 failed; 26 filtered out`.

## Verification

All Rust commands ran from `src-tauri` with `RUST_MIN_STACK=16777216`, one Cargo
process at a time.

Focused unit regressions, each `1 passed; 0 failed`:

```text
acp::delegation::workflow::error::tests::archived_workflow_navigation_preserves_corrupt_header_classification
acp::delegation::companion::tests::features_parse_defaults_and_tokens
acp::delegation::companion::tests::completion_v2_stale_feature_token_is_ignored
acp::delegation::workflow::store::tests::header_db_error_classification
acp::manager::tests::archived_workflow_prompt_surfaces_fence_root_and_bound_child_without_side_effects
acp::delegation::workflow::store::tests::completion_artifact_contract_final_delivery_drift_reopens_full_final_review
```

Complete integration target:

```powershell
cargo test --features test-utils --test completion_protocol_v2
```

Result: `27 passed; 0 failed`.

Full desktop command:

```powershell
cargo test --features test-utils
```

Result: nonzero after advancing through all previously blocked targets:

- lib: `4350 passed; 0 failed; 1 ignored`
- codeg-mcp: `1 passed; 0 failed`
- api_integration: `17 passed; 0 failed`
- backup_api: `3 passed; 0 failed`
- completion_protocol_migrations: `12 passed; 0 failed`
- completion_protocol_v2: `27 passed; 0 failed`
- completion_transport_parity: `7 passed; 3 failed`

The three new failures are:

- `legacy_restart_surface_is_absent`: stale inventory ban conflicts with the
  required `successor_conversation_id` retirement-navigation field.
- `v2_only_removed_surface_inventory`: same stale broad token ban.
- `attention_authenticated_context_owns_durable_root_across_core_and_http`:
  the foreign direct-core branch lacks historical fixture permission and
  returns retirement before reaching the ownership comparison.

`git diff --check` passed for all five owned files, with only LF-to-CRLF
informational warnings. An owned-file `rustfmt --check` remains nonzero because
these large files contain previously ledgered formatting drift outside this
change; newly changed hunks were normalized without reformatting unrelated
sections.

## Self-review

- The DB error mapper remains narrow: only SeaORM type/conversion failures map
  to unsupported-header; query, connection, and execution failures remain
  `Persistence`.
- Listener scope propagation reads the lexical flag before spawning and is
  compiled only for `test`/`test-utils`; unscoped and ordinary production
  listener behavior follows the existing path.
- Explicit historical feature values do not change production parsing.
- The integration expectations remain strict: Final must reopen with
  `final_artifact_drift`, while dirty worktrees retain
  `completion_artifact_unavailable`.

## Remaining concern

The full desktop gate is not yet green because the next integration target has
three diagnosed historical test issues. They are intentionally not folded into
this repair without their own scoped change and review.

## Fix Round 1: Reviewer Evidence Correction

The report itself is an owned deliverable and is intentionally force-added even
though `.gitignore` excludes the `.superpowers/` path. This round adds the
previously omitted exact-filter verification evidence; it does not modify
production or test source.

The loader filter was renamed during the repair from
`root_protocol_loader_rejects_bound_legacy_when_conversation_owns_v2` to
`root_protocol_loader_retires_when_conversation_owns_archived_v2`.

All commands below were run serially from `src-tauri` in PowerShell with the
listed environment assignment. Each exited with code `0`.

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 root_protocol_loader_retires_when_conversation_owns_archived_v2 -- --exact
```

Result: `1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out`.

Owned report validation:

```powershell
git diff --cached --check -- '.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/completion-protocol-v2-repair-report.md'
```

Exit code: `0`. Output: empty; no whitespace errors in the staged report.

```powershell
pnpm exec prettier --check '.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/completion-protocol-v2-repair-report.md'
```

Exit code: `0`. Output: `All matched files use Prettier code style!`.

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 completion_v2_semantic_inputs -- --exact
```

Result: `1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out`.

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 session_2889_and_final_drift_have_no_format_repair_escape -- --exact
```

Result: `1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out`.

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 final_drift_report_enrichment_reopens_and_omits_stale_completion -- --exact
```

Result: `1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out`.

```powershell
$env:RUST_MIN_STACK='16777216'
cargo test --features test-utils --test completion_protocol_v2 corrupt_header_nonterminal_fences -- --exact
```

Result: `1 passed; 0 failed; 0 ignored; 0 measured; 26 filtered out`.
