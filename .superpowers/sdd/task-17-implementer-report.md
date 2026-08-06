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

## Review Fix Result

Closed `T17-CODEX-I1` by restricting the frozen-v1 exception to simple clauses
whose explicit subject or prepositional scope is a frozen/historical v1
workflow or branch. Any v2 token, compound clause, contrast, or subject reset
fails closed. Direct `provide`, `return`, and `retry` forms now enter the same
Card, digest, and completion-format prohibitions as the original verbs.

Closed `T17-GROK-I1` by treating Card harvest and Card-based settlement as one
unsafe authority condition shared by `B2D-COMP-001` and `B2D-R008`. The matcher
covers active and non-leading harvest, harvested-Card settlement, Card use or
acceptance as settlement evidence, Card-authorized settlement, and active,
passive, or imperative gate settlement from a Card. Scoped negative forms and
the explicit frozen-v1 branch remain allowed.

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

The dual-review fix used additional RED -> GREEN cycles:

- The seven Codex fail-open probes plus eight harvest/settlement probes first
  produced 15 failures in the completion-contract slice.
- Plain Card settlement, bare-v2 mixing, and unfrozen-v1 subjects were each
  added as failing fixtures before their matcher changes.
- Scoped review exposed compound frozen-v1 subject resets and bidirectional
  Card authority. Twelve positive/negative fixtures failed before the shared
  settlement and polarity grammar was introduced.
- A second adversarial review added passive/imperative settlement, Card
  acceptance/treatment/authorization, `no`, `shall not`, and
  `under no circumstances` cases. Twelve fixtures failed before the final
  fail-closed and action-scoped implementation; an unrelated-`no` masking
  fixture then failed before the negation window was narrowed.

The final validator exposes `B2D-COMP-001` through `B2D-COMP-016`. Clause-aware
checks permit explicit v2 prohibitions and the frozen v1 historical branch,
but reject affirmative Card/digest/format-repair guidance and unrelated
negation masking. Existing routing, ownership, risk, and `B2D-R001` through
`B2D-R011` recovery fixtures remain active; `B2D-R008` now protects platform
completion attention and root re-entry.

## Focused Verification

Fresh verification on the committed implementation tree:

- `node --test .agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs`:
  562 tests passed, 0 failed across 14 suites.
- `node .agents/skills/brainstorm-to-delivery/scripts/validate-contract.mjs`:
  PASS with 0 failures and 53 checks passed.
- `pnpm exec prettier --check` for the three validator `.mjs` files: passed.
- `node --check` for the changed validator library and test: passed.
- `git diff --check` for the two Task 17 fix files: passed.

No full repository suite, build, Rust test, or frontend test was run. Task 17
requires focused validator tests; Task 18/Final owns repository-wide
verification.

## Review

The initial High Codex and Grok reviews each returned one Important finding:
the broad frozen-v1 clause exemption masked prohibited v2 actions, and Card
harvest/settlement authority could re-enter without COMP/R008. Both findings
are covered by exact report probes plus broader grammatical and polarity
fixtures.

A separate scoped review of the fix found two further Important-aligned
variants: contrasting subjects after a frozen-v1 prefix, and asymmetric Card
settlement/negation handling. Those variants drove two more RED -> GREEN
rounds. The final implementation no longer enumerates allowed subject resets;
compound frozen-v1 clauses fail closed, and settlement authority is reduced by
the same scoped polarity helper in every supported direction.

## Scope And Hygiene

The review fix staged only the validator library and focused test file. The
production Skill and Plan/Design documents were not modified. Existing changes
in `.superpowers/sdd/progress.md`, the Task 13 report,
`src-tauri/src/acp/connection.rs`, and
`src-tauri/src/acp/delegation/launch_snapshot.rs` remain unstaged. Existing
publication and manifest JSON files also remain untracked.

## Commit

- `78689b35 feat: move b2d completion to platform evidence`
- `40e57ec7 fix: close b2d completion validator gaps`

## Concerns

The validator intentionally enforces a bounded operational grammar rather than
general natural-language understanding. Frozen-v1 exceptions now fail closed
for compound prose, and Card settlement polarity is action/object scoped for
the Task 17 grammar. Future wording changes should add a failing fixture before
changing the matcher.
