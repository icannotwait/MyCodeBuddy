# Task 17 Implementer Report

## Result

Replaced the B2D protocol-v2 Card harvesting and re-emission workflow with
platform completion state, durable typed attention, and root-owned settlement
or admission re-entry. The Skill now keeps genuine incomplete/infrastructure
recovery separate from semantically terminal completion and isolates frozen v1
history from v2 successors.

Producer admission requires a resolvable clean baseline. Passing Implementer
and Final Fixer outcomes require a clean workflow-owned commit unless durable
Task policy authorizes no-op verification, and code Reviewers validate that
commit at admission and completion. Final aggregation precedes Final Reviewer
admission; a passing Final freezes the reviewed commit through delivery, while
drift reopens Final.

Plan rounds now follow platform-selected nodes and lineage rather than
model-authored findings/count ledgers. All v2 gates use platform outcomes and
validated scope, and non-pass Final routing consumes only the platform
Final-findings package and context.

## TDD Evidence

The initial RED fixture slice had 11 failures and 0 passes. It showed that the
validator still required the obsolete Card recovery rule and did not emit the
new `B2D-COMP-*` IDs for Card templates, digest requests, malformed-completion
continuation, re-emission, clean baselines, producer commits, Final ordering,
root re-entry, or the v1/v2 boundary.

Two focused review-driven RED cycles then proved additional gaps before their
implementation:

- Third-person guidance using `requires`, `continues`, and `reopens` initially
  bypassed the new negative grammar checks: 3 failures, then 11/11 GREEN.
- Removing platform gate outcome/scope reduction or the Final-findings package
  initially emitted no stable rule: 2 failures, then 14/14 GREEN with
  `B2D-COMP-015` and `B2D-COMP-016`.

The final validator exposes `B2D-COMP-001` through `B2D-COMP-016`. Clause-aware
checks permit explicit v2 prohibitions and the frozen v1 historical branch,
but reject affirmative Card/digest/format-repair guidance and unrelated
negation masking. Existing routing, ownership, risk, and `B2D-R001` through
`B2D-R011` recovery fixtures remain active; `B2D-R008` now protects platform
completion attention and root re-entry.

## Focused Verification

Fresh verification on the committed implementation tree:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  519 tests passed, 0 failed across 14 suites.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  PASS with 0 failures and 53 checks passed.
- `pnpm exec prettier --check` for the three validator `.mjs` files: passed.
- `node --check` for `validate-contract.lib.mjs`: passed.
- `git diff --check` for the four Task 17 implementation files: passed.

No full repository suite, build, Rust test, or frontend test was run. Task 17
requires focused validator tests; Task 18/Final owns repository-wide
verification.

## Review

A scoped self-review found and closed two issues before the final verification:

- Inflected completion-repair instructions could evade imperative-only
  matching. New RED fixtures cover those grammatical forms and unrelated
  negation boundaries.
- The existing Design section still referenced Critical/Important finding
  clearance. It now reloads platform-selected reviewer nodes and lineage, and
  the validator rejects finding-count gate reduction.

No Critical, Important, or Minor issue remains from the implementer review.
Independent Codex and Grok review belongs to the High Task gate after this
implementation report.

## Scope And Hygiene

Only the four Task 17 Skill/validator files were staged in the implementation
commit. Existing changes in `.superpowers/sdd/progress.md`, the Task 13 report,
`src-tauri/src/acp/connection.rs`, and
`src-tauri/src/acp/delegation/launch_snapshot.rs` remain unstaged. Existing
publication and manifest JSON files also remain untracked. Plan and Design
documents were not modified.

## Commit

- `78689b35 feat: move b2d completion to platform evidence`

## Concerns

The validator intentionally enforces a bounded operational grammar rather than
general natural-language understanding. Its positive structural clauses and
negative mutation fixtures fail closed for the protocol guidance covered by
Task 17; future wording changes should add a failing fixture before changing
the matcher.
