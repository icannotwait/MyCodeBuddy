# Task 8 Implementer Report

## Outcome

Implemented Task 8, "Parse Plan Material and Classify Localized Changes," with
TDD. No Task 9+ behavior, Plan/Design edits, push, or PR was performed.

Task 8 now parses bounded CommonMark Plan material into exact normalized shared
and Task sections, binds that material to canonical manifest policy, derives
reviewer selectors from durable identities and routes, and classifies localized
post-review changes through an opaque platform-owned authorization context.

## Implementation

- Added exact CommonMark span parsing for front matter, global preamble, Global
  Constraints, and ATX H2/H3 `Task N` bodies, including nested blockquote/list
  headings and correct same-or-higher Task boundaries.
- Added strict bounds, duplicate/missing Task rejection, UTF-8 validation, BOM
  removal, newline normalization, NFC normalization, trailing-space removal,
  exact terminal newline handling, lowercase SHA-256 identities, and committed
  golden fixtures.
- Added opaque bound Plan maps and material selectors whose hashes, key sets,
  normalized bodies, source bytes, section bounds, and manifest task references
  are revalidated before use.
- Added canonical manifest-policy material, Task specification identities,
  reviewer subject identities, explicit server-owned holistic selectors, and
  durable agent/profile route-derived reviewer selectors.
- Added estimated-publication comparison over both material hashes/key sets and
  derived selector sets, exposed through a store-facing validated decision.
- Added localized-change authorization for exactly the current required Plan
  reviewer set while retaining the full Plan reviewer cohort as reset context.
  Missing authorization, unparseable material, shared/policy changes, selector
  mismatch, ambiguous key sets, and uncovered changes all fail closed to a new
  lineage and select the full cohort.
- Added localized corrective reviewer selection from non-passing or
  changed-material-intersecting reviewers within the authorized required set.

The current durable publish request does not carry Plan bytes. Task 8 therefore
does not claim a durable publication mutation; it provides the validated
store-facing material decision for the later Task 9/14 integration points.

## TDD Evidence

### RED

- Initial grammar, normalization, bounds, identity, selector, authorization,
  and publication tests failed because the Task 8 parser and types were absent.
- A Setext Global Constraints regression exposed a heading-boundary error.
- A nested CommonMark heading regression exposed rejection of valid ATX Task
  headings inside blockquotes/lists.
- Corrupt-map tests exposed missing revalidation of retained source bytes,
  section bounds, body hashes, and canonical policy material.
- A store-level selector-set regression exposed that unchanged Plan bytes could
  hide a manifest-derived reviewer selector change.
- Review-loop tests exposed two authorization defects: missing/unparseable
  changes returned an empty reset cohort, and authorization incorrectly required
  outcomes for the entire cohort instead of the current required-reviewer set.
- The final RED failed to compile on the wished-for
  `localized_plan_change_context` API and non-optional classifier context, then
  passed only after the cohort-context implementation was added.

### GREEN

```powershell
cargo test --lib -j 1 --features test-utils plan_material::tests -- --list
# 16 listed

cargo test --lib -j 1 --features test-utils plan_material::tests -- --nocapture
# 16 passed, 0 failed

cargo test --lib -j 1 --features test-utils plan_material -- --nocapture
# 17 passed, 0 failed, including the store-facing publication test
```

The Plan command from the task text without `--features test-utils` remains
blocked by pre-existing integration-helper feature gating elsewhere in the
crate. Task 8 verification uses the repository's test-utils feature and library
target to exercise the same unit-test surface.

## Review

The assigned Task 8 reviewer completed the fix loop with no Critical or
Important findings. Both earlier Important findings were verified resolved:
full-cohort reset context is retained without localized authorization, and a
valid strict subset of current required reviewers can authorize localization.
Code quality and Task 8 spec compliance passed. The review's remaining Minor
items were to create this report and exclude unrelated `project.rs` formatting
churn from the commit.

## Final Verification

- Task 8 tests: 17 passed, 0 failed.
- Desktop `cargo check -j 1`: exit 0.
- Server `cargo check -j 1 --no-default-features --features server --bin
  codeg-server`: exit 0.
- MCP `cargo check -j 1 --no-default-features --bin codeg-mcp`: exit 0.
- Desktop/server/MCP Clippy: exit 0 under `-D warnings` with the unrelated
  baseline `clippy::too_many_arguments` lint allowed.
- Task-owned `rustfmt --check`: exit 0.
- Fixture JSON parse and BOM/CRLF byte checks: exit 0.
- Staged `git diff --check` excluding the two raw whitespace-normalization
  fixtures: exit 0. The unscoped check reports only the fixtures' intentional
  trailing whitespace/CRLF input lines.
- Repository-wide `cargo fmt --check` remains blocked by unrelated pre-existing
  formatting drift outside Task 8; no unrelated file is included in this task.

## Scope And Integrity

- Plan SHA-256:
  `965289a2cf8727725a55e3d896406d446f90203059ea5ca6f89979acb8302820`
- Design SHA-256:
  `8e9f2555366aed2fcb0afdf801d9a1ddd13cbf4e562d9309957e98e51a855914`
- The approved Plan and Design have no staged or committed diff.
- Pre-existing `.superpowers/sdd/progress.md`, `project.rs`, and publication
  JSON changes were not staged, committed, reverted, or otherwise modified by
  Task 8 completion work.

## Concerns

None within Task 8. Durable publication integration remains explicitly deferred
because the current publish request lacks the Plan source bytes required by the
validated material decision.
