# Task 7 Report

## Status

PASS. The root MCP workflow protocol is atomically v2-only. Only the literal
`workflow_v2` launch feature enables the stable four-tool root catalog, and
the capability/store header is `workflow_manifest_v2`. Delegation children,
missing catalogs, partial catalogs, `workflow_v1`, and schema-v1 documents all
fail closed without a legacy execution mode.

## Commit

`feat(mcp): expose workflow manifest v2` (this report is part of that commit;
Git assigns the commit hash after the report content is finalized).

Base HEAD: `9212bfac424e0fb907f6946a8bc3592ea085403d`.

No push was performed.

## Files

- `src-tauri/src/acp/connection.rs`
- `src-tauri/src/bin/codeg_mcp.rs`
- `src-tauri/src/acp/delegation/companion.rs`
- `src-tauri/src/acp/delegation/transport.rs`
- `src-tauri/src/acp/delegation/listener.rs`
- `src-tauri/src/acp/delegation/tool_schema.json`
- `src-tauri/src/acp/delegation/workflow/store.rs`
- `.superpowers/sdd/b2d-adaptive-routing/task-7-report.md`

## RED

All counted RED runs executed matching tests. Compile-only and zero-match runs
are not counted.

1. Initial `workflow_v2` RED:
   - Exit 1; 4 matching tests failed, 3,658 filtered out.
   - Feature parsing, launch assembly, exact catalog, and root/child gating
     still used the v1 protocol.
2. Initial `workflow_manifest_v2` RED:
   - Exit 1; 5 matching tests failed, 3,658 filtered out.
   - Capability payload, transport evidence, listener/store mappings, and
     persisted headers still used the v1 contract.
3. Review regression RED with the exact `workflow_manifest_v2` filter:
   - Exit 1; 9 matching tests executed: 5 passed, 4 failed, 3,658 filtered out.
   - Failures semantically proved that replay/update could retain a v1 header,
     real Plan digest producers returned generic `gate_not_ready`, and the
     companion-to-store named-pipe round trip exposed that generic code.
4. A preceding regression attempt failed to compile because a test assertion
   moved an error payload. It executed 0 tests and is explicitly not RED
   evidence; test-only scaffolding was corrected before item 3.

## GREEN

Commands ran from `src-tauri/` with `CARGO_BUILD_JOBS=2`; feature-enabled tests
used the reduced test-only `TAURI_CONFIG`.

1. `cargo test --features test-utils workflow_manifest_v2 -- --nocapture`
   - Exit 0; 9 passed, 0 failed, 3,658 filtered out.
2. `cargo test --features test-utils workflow_v2 -- --nocapture`
   - Exit 0; 6 passed, 0 failed, 3,661 filtered out.
3. `cargo test --features test-utils grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --nocapture`
   - Exit 0; 1 passed, 0 failed, 3,666 filtered out.
   - Grok `tools/list` JSONL: 7,669 bytes; literal ceiling remains `7_680`.
4. `cargo test --features test-utils tool_schema_retains_essential_agent_guidance -- --nocapture`
   - Exit 0; 1 passed, 0 failed, 3,666 filtered out.
5. `cargo test --features test-utils workflow::store -- --nocapture`
   - Exit 0; 55 passed, 0 failed, 3,612 filtered out.
6. `cargo check --no-default-features --bin codeg-mcp`
   - Exit 0; finished `dev` profile in 13.78s.
7. `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`
   - Exit 0; finished `dev` profile in 20.79s with no clippy diagnostics.
8. Targeted `rustfmt --edition 2021` ran on all six touched Rust files.
9. `tool_schema.json` parsed successfully with `ConvertFrom-Json`.
10. `git diff --check` exited 0.

## Review

- Independent frozen-high-risk review initially found one Critical and two
  Important issues: replay/update header stamping, real typed digest producers,
  and a listener-only test that bypassed the companion.
- Regression tests were added before production fixes and observed the
  nonzero semantic RED in item 3 above.
- Re-review found no remaining Critical or Important issues in those areas.

## Self-review

- Catalog modes are exactly `Unavailable`, `WorkflowManifestV2`, and
  `Inconsistent`. Missing and partial catalogs never enter a legacy mode.
- `WORKFLOW_V2_TOOLS` contains exactly the four stable names. Root plus the
  literal `workflow_v2` feature exposes all four; children expose none.
- `workflow_v1` is ignored as an unknown launch token. Remaining v1 strings
  are negative or upgrade test fixtures, not active runtime parsing/fallback.
- Successful fresh publish, explicit update, same-digest replay, and race
  reclassification stamp `workflow_manifest_v2`; cross-parent and mismatched
  publications cannot mutate the header.
- Structured tagged Design/Plan evidence crosses companion, transport,
  listener, and store. The integration test uses the real local socket path.
- Stable mappings cover `risk_assessment_invalid`, `task_route_mismatch`,
  `reviewer_set_mismatch`, `reviewed_task_stale`,
  `artifact_digest_mismatch`, and `cohort_frozen` from real producers.
- Manifest schema version 2 and risk policy `b2d_task_risk_v1` remain
  canonical. Schema v1 is rejected.
- The tool schema retains the required full/scoped, revision-kind, Author
  coverage, reviewer-set, finding-update, and report-path guidance.
- The literal catalog budget remains `7_680`; it was not raised.
- Final routing logic is unchanged; only workflow capability assembly and
  protocol plumbing changed.
- The diff contains only the seven Task 7 source files and this report.

## Concerns

None unresolved.

## Warnings

- Feature-enabled builds warn that the `codeg-mcp` sidecar is absent and use a
  zero-byte test placeholder. This predates Task 7 and does not affect tests.
- Cargo reports a future-incompatibility notice for
  `proc-macro-error2 v2.0.1`; check and clippy still exit 0.
- Git reports configured LF-to-CRLF conversion notices for touched files;
  `git diff --check` is clean.
