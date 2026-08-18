# Task 2 Report

## Status

Implemented bounded, non-authoritative Simple Plan routing parsing and additive
Simple progress route metadata parsing.

Commit: `6cfd1830 feat(workflow): parse Simple routing metadata`

Committed Task-owned file only:

- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`

## Parser Bounds And Safe Partial Results

- The existing Plan document hard bounds remain 2 MiB and valid UTF-8.
  Violations still return `SimpleParseError::{SizeLimitExceeded, InvalidUtf8}`.
- A routing comment body is limited to 256 KiB of UTF-8 bytes.
- The shared extractor recognizes only exact marker starts on unfenced Markdown
  lines, follows backtick/tilde fence rules, counts multiple live markers, and
  parses the first body through its closing `-->`.
- A missing routing marker is valid legacy input: Plan Tasks are returned,
  `routing` is `None`, and no routing warning is added.
- Multiple, truncated, oversized, invalid-JSON, unsupported-schema, and
  unsupported-policy routing blocks retain the largest safe Plan Task model.
  Invalid routing never creates admission authority, a Gate, or a workflow
  header.
- Progress retains its 512 KiB document and 64 KiB block bounds and its
  existing missing/multiple/truncated/oversized/invalid/schema warnings.
- Legacy progress Tasks deserialize omitted route fields as `None`.
- Structurally malformed nested route metadata and unrecognized expected keys
  return a recoverable `simple_progress_invalid_json` partial result rather
  than panicking. Actual legacy run keys remain readable.
- Unknown Task status and run state remain warning-bearing unknown values and
  cannot become completed state.

## TDD Evidence

### Routing RED

Command:

```text
cargo test --lib --features test-utils simple_parse::tests::simple_parse_routing -- --nocapture
```

Actual expected result: exit 101 during compilation. The new real parser tests
failed because production lacked the requested surface. Representative output:

```text
error[E0425]: cannot find value `MAX_SIMPLE_ROUTING_BLOCK_BYTES` in this scope
error[E0425]: cannot find value `WARNING_ROUTING_MULTIPLE` in this scope
error[E0425]: cannot find value `WARNING_ROUTING_POLICY` in this scope
error[E0609]: no field `routing` on type `SimplePlanDocument`
error: could not compile `codeg` (lib test) due to previous errors
```

After the minimal routing models, bounded extractor, warning mapping, and
parser were implemented, the focused routing filter executed 5 tests and all
5 passed.

### Progress RED

Command:

```text
cargo test --lib --features test-utils simple_parse::tests::simple_parse_progress -- --nocapture
```

Actual expected result: exit 101 during compilation. Representative output:

```text
error[E0609]: no field `risk_level` on type `SimpleProgressTask`
error[E0609]: no field `task_agent_generation` on type `SimpleProgressTask`
error[E0609]: no field `expected_work_unit_keys` on type `SimpleProgressTask`
error: could not compile `codeg` (lib test) due to 6 previous errors
```

After adding the serde models, optional raw fields, canonical-key recognition,
and safe mapping, the focused progress filter executed 6 tests and all 6
passed.

## Final Verification

Run from `src-tauri/` unless noted:

```text
cargo test --lib --features test-utils simple_parse -- --nocapture
14 passed; 0 failed; 4695 filtered out

cargo test --lib --features test-utils workflow::key::tests -- --nocapture
15 passed; 0 failed; 4694 filtered out

cargo fmt --all -- --check
exit 0

git diff --check                         # repository root, before commit
exit 0
```

Every requested filter executed tests. The focused routing and progress GREEN
runs executed 5 and 6 tests respectively.

## Self-Review

- Compared the final diff against the Task 2 brief and confirmed that only
  `simple_parse.rs` changed.
- Routing parsing accepts only the bounded serde shape plus schema/policy
  identifiers. Task/risk/route semantics remain deferred to later validation
  and projection work.
- Progress expected keys use Task 1's canonical recognizer, while actual run
  fields retain legacy pass-through behavior.
- Plan Task extraction and pre-existing warning ordering remain intact; routing
  warnings are additive and deduplicated through the existing bounded warning
  helper.
- No Plan, Task 3-5 file, persistence surface, admission Gate, or workflow
  header was changed.

## Retained Minors

- Rust test builds emit the existing ignored `codeg-mcp` sidecar placeholder
  warning and the existing macOS compact-unwind linker warning. They do not
  affect test execution or outcomes.
- Verification was scoped to the focused Task 2 commands required by the Plan;
  the repository-wide Rust regression suite was not requested or run.

## Fix Round 1

### Scope And Result

Addressed the independent primary review's open Important finding. The shared
unfenced comment extractor now accepts a marker only when the configured marker
text is followed by the line boundary or ASCII whitespace. Existing newline,
CRLF, tab, and inline-space bodies remain recognized, while version and suffix
lookalikes no longer count as v1 blocks or displace later valid metadata.

Commit:
`ab23f5627b71319033b1a0ea74b53453431f9735 fix(workflow): require exact Simple markers`

The commit contains only:

- `src-tauri/src/acp/delegation/workflow/simple_parse.rs`

### RED Evidence

Added focused routing and progress regressions before changing production
marker recognition. Each fixture places both a `v10` version lookalike and a
`v1-extra` prefix lookalike before a valid live v1 marker.

From `src-tauri/`:

```text
cargo test --lib --features test-utils simple_parse::tests::simple_parse_routing -- --nocapture
exit 101
running 6 tests
simple_parse_routing_ignores_prefix_lookalikes_before_live_marker ... FAILED
panic: live routing
test result: FAILED. 5 passed; 1 failed; 4705 filtered out

cargo test --lib --features test-utils simple_parse::tests::simple_parse_progress -- --nocapture
exit 101
running 7 tests
simple_parse_progress_ignores_prefix_lookalikes_before_live_marker ... FAILED
panic: live progress
test result: FAILED. 6 passed; 1 failed; 4704 filtered out
```

Both failures demonstrated the reviewed defect: the first lookalike was counted
and selected, so the later valid live block was discarded.

### GREEN And Verification Evidence

After the minimal delimiter-boundary check:

```text
cargo test --lib --features test-utils simple_parse::tests::simple_parse_routing -- --nocapture
exit 0
running 6 tests
test result: ok. 6 passed; 0 failed; 4705 filtered out

cargo test --lib --features test-utils simple_parse::tests::simple_parse_progress -- --nocapture
exit 0
running 7 tests
test result: ok. 7 passed; 0 failed; 4704 filtered out

cargo test --lib --features test-utils simple_parse -- --nocapture
exit 0
running 16 tests
test result: ok. 16 passed; 0 failed; 4695 filtered out

cargo fmt --all -- --check
exit 0

git diff --check                         # repository root, before commit
exit 0
```

Every requested test filter executed at least one test. Test builds retained
the previously recorded sidecar placeholder and macOS compact-unwind warnings.

### Self-Review

- The boundary rule is documented next to marker recognition and is shared by
  routing and progress extraction.
- The tests independently prove that `v10` and `v1-extra` lookalikes do not
  affect selection, marker counts, or warnings when followed by valid metadata.
- Marker body offsets, byte limits, fence handling, warning insertion order,
  and safe partial behavior are unchanged.
- Existing routing newline fixtures and progress newline/inline-space fixtures
  passed under the new boundary rule.
- The CommonMark backtick info-string Minor was intentionally not addressed;
  it remains recorded for final triage as directed.
