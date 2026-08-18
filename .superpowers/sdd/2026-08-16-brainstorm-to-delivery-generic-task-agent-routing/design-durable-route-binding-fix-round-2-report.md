# Durable Route Binding Design Fix Round 2 Report

## Status

Complete. The Design-only revision is committed as
`f13c0c79fe46635e6b0bbb43be067299e5742a55` (`docs: harden durable route
binding design`). This report is intentionally uncommitted.

## Finding Dispositions

1. **Important: actual Agent/profile proof - valid, fixed.** The durable query
   now returns insert-fixed actual `agent_type` and `profile_id`. Full
   reconciliation compares them with the Plan route, canonical key, progress,
   and binding. Negative tests cover wrong actual Agent/profile for first,
   continuation, and replacement runs.
2. **Important: cross-namespace discovery - valid, fixed.** Query selection is
   the deduplicated union of every requested-namespace row and every keyed row
   regardless of binding namespace. Recognized B2D Task keys in a foreign
   namespace block even after progress deletion; foreign unkeyed rows remain
   excluded.
3. **Important: lost acknowledgement - valid, fixed.** Progress now records an
   exact `first`, `continue`, or `replacement` dispatch intent and expected
   lineage. The validator may emit one deterministic, non-authorizing adoption
   action only for one globally unambiguous intent/row match, followed by a
   fresh query and full validation. `B2D-DURABLE-009` covers absent, ambiguous,
   repeated, or inconsistent adoption. No unresolved intent means no mirror is
   synthesized, so deleted-mirror failure remains intact.
4. **Important: initial Plan bootstrap - valid, fixed.** A non-authorizing
   Plan-only `--derive-plan-routing --output-json` mode now precedes progress
   initialization, combined static validation, full durable admission, and
   independent Plan review. Boundary revisions follow the same safe ordering.
5. **Minor: cursor-chain observability - valid, fixed.** Every page now echoes
   `request_cursor`; the first is null and each later value must exactly equal
   the previous `next_cursor`. Tampering tests retain contiguous offsets while
   changing cursor tokens.

## Review Passes

- **Completeness:** Rechecked Goals, terminology, invariants, progress/query and
  validator contracts, execution/recovery, backend/Skill changes, tests,
  compatibility, and success criteria against all five findings and the
  approved brief.
- **Threat model and internal consistency:** Verified coordinated
  Plan/progress rewrites still fail against durable identity, wrong namespaces
  cannot hide keyed runs, and adoption cannot create rows. Fixed an intermediate
  sequencing conflict by requiring high-review intents to be recorded one at a
  time before sequential admission, then allowing child execution concurrency.
- **Implementation and testability:** Added exact query selection/response
  fields, cursor rules, Plan-only CLI behavior and JSON output, progress intent
  fields, operation-specific lineage predicates, deterministic reconciliation
  action output, stable rule ID, and focused regression scenarios.

## Preserved Decisions

Grok remains the default Task Agent. Normal Tasks retain Task Agent production
plus independent Codex primary review. High Tasks retain independent Codex
production plus independent Codex primary and Task Agent auxiliary review.
Admitted Tasks cannot switch. Document, Task, and final roles remain independent.
The parent remains coordination-only and does not author Design, Plan, Skill
prose, validator/tests, or Task code. Simple remains manifest-free and
platform-gate-free; Rust projection remains warning-only. The exact coordinated
Plan/progress rewrite regression remains mandatory.

## Verification

- Parsed all 12 JSON code fences successfully.
- Recomputed the canonical high-Task vector as
  `sha256:b498416d87bf6ba928bd7ddb5f1a451daf82300584f3d40b606c3c56f169ba7a`.
- Required-invariant assertions passed.
- Placeholder and obsolete-contradiction scans returned no matches.
- `git diff --check` passed before commit.
- Pre-commit scope check showed only the Design document changed.
- Commit contains only the Design document.
- No implementation tests were run because this round was contractually
  limited to Design documentation.

## Concerns

No unresolved Design concern. Implementation must keep adoption
non-authorizing until the emitted action is persisted and a fresh complete
snapshot passes full admission; treating the first matching row as success
without the second validation would violate the contract.
