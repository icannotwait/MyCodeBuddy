# Final Whole-Branch Round 21 Review

Reviewed base `941b5b0b` through exact head
`21401f42a993024fefc97b984c11196928e2dd74` as the final broad
merge-readiness gate. The supplied 33-commit diff package was the sole source
for the branch range and was read once in full.

## Strengths

- The intended route model is coherent and visible in both the operational
  Skill and deterministic derivation. An omitted selection resolves to Grok
  (`.agents/skills/brainstorm-to-delivery/SKILL.md:121`), while a high Task
  derives a Codex implementer, Codex primary reviewer, and selected Task Agent
  auxiliary reviewer
  (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:5792`).
- Production and review ownership is explicitly delegated. The parent is kept
  in the coordinator role (`.agents/skills/brainstorm-to-delivery/SKILL.md:114`),
  with separate Design Fixer (`SKILL.md:137`), Plan Author and Plan Reviewer
  (`SKILL.md:217`), high-Task producer/reviewers (`SKILL.md:360`), and final
  reviewer (`SKILL.md:400`) work units.
- Reviewer identity is backward compatible without collapsing the new slots.
  Rust parses six-part primary/auxiliary keys at
  `src-tauri/src/acp/delegation/workflow/key.rs:261` and maps a five-part legacy
  reviewer key to primary at `src-tauri/src/acp/delegation/workflow/key.rs:277`.
  The corresponding regression assertions are at
  `src-tauri/src/acp/delegation/workflow/key.rs:490` and
  `src-tauri/src/acp/delegation/workflow/key.rs:516`.
- Recovery and independence are keyed by the complete work unit. Progress
  validation groups runs by the full key and rejects a child conversation used
  by different keys
  (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:6174`),
  while durable admission independently rejects a child already bound to a
  different workflow node
  (`src-tauri/src/acp/delegation/workflow/admission.rs:1512`).
- The Rust side remains a non-authoritative Simple projection. Route conflicts
  are emitted as warning codes
  (`src-tauri/src/acp/delegation/workflow/project.rs:2363`), and the projected
  snapshot retains `manifest_revision: None`, `compatibility: Simple`, and an
  empty gate list (`src-tauri/src/acp/delegation/workflow/project.rs:3199`).
- The final auxiliary artifact is honestly scoped as a workflow test double:
  every nonblank line in
  `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/round-21-auxiliary-simulated-grok-last.txt:1`
  and
  `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-5-final-fix-round-21-auxiliary-simulated-grok-rereview.md:1`
  is labeled `SIMULATED GROK WORKFLOW TEST DOUBLE ONLY`, and the report states
  that it is an independent Codex simulation rather than a Grok verdict at
  `task-5-final-fix-round-21-auxiliary-simulated-grok-rereview.md:17`.

## Requirements and Architecture

The branch follows the specified division of responsibility: the Skill owns
workflow policy and delegation; the JavaScript validator is the authoritative
admission-time document validator; Rust parses bounded additive metadata and
projects warnings without introducing a manifest or platform gate. This is
consistent with the design's Simple invariant and warning-only platform layer
(`docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:109`
and `:130`) and with the plan's deliberate placement of routing semantics in
JavaScript
(`docs/superpowers/plans/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing.md:731`).

The fixed route and ownership decisions are represented correctly:

- Grok is the default Task Agent, normal Tasks use the selected Task Agent as
  producer, and high Tasks use it only as auxiliary reviewer
  (`.agents/skills/brainstorm-to-delivery/SKILL.md:119` and `:369`).
- The Skill requires a completed prefix, clean pending suffix, Plan Author
  revision, and full Plan re-review before a selection change, and tells the
  coordinator to defer an active-Task change (`SKILL.md:127`). The deterministic
  enforcement is nevertheless incomplete for an already admitted high Task,
  as described in Important 1.
- Design fixes, initial and revised Plan authoring, document reviews, normal and
  high Task production/fixes, and final review are assigned to independent
  child work units (`SKILL.md:114`, `:137`, `:217`, `:360`, and `:400`).
- High primary and auxiliary reviewers have distinct keys and must have
  distinct child conversations even when both are Codex (`SKILL.md:369`).
- Continuation remains confined to the same stable key and child conversation
  (`SKILL.md:382`); replacement retains the original identity and key
  (`SKILL.md:389`).
- New routed reviewer keys are explicit six-part keys, while legacy five-part
  reviewer keys are read only as primary lineage (`SKILL.md:282`).

The data model, projection nodes, compatibility path, and test coverage are
otherwise appropriately additive. The merge gate fails on deterministic
validation integrity, not on the overall architecture.

## Issues

### Critical

None.

### Important

1. **An admitted high Task does not freeze its Task Agent generation.**
   This is merge-blocking. For a high Task, the implementer key is always
   `task|N|implementer|codex|none`, regardless of the selected Task Agent
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:5792`
   and `:5806`). During later-generation validation,
   `hasAdmittedImplementerRun` accepts any admitted run on that key, and
   `historicalAdoptedBoundary` then trusts the currently rewritten Plan/progress
   generation and route (`validate-contract.lib.mjs:6544` and `:6552`). Because
   the proof is not bound to the generation that existed at admission, this
   sequence validates with no failure: Task 1 completed under generation
   1/Grok; Task 2 was admitted as high; the Plan and progress were then rewritten
   to generation 2/Gemini effective at Task 2 while retaining only the Codex
   implementer run. This violates immutable admitted generations and the
   completed/revised/re-reviewed boundary
   (`docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:113`
   and `:221`). Validation needs generation-bound admission evidence, or an
   equivalent rule that cannot infer historical adoption from the generation-
   invariant high implementer key. A regression must cover a high Task rewritten
   from one auxiliary identity to another after implementer admission.

2. **The shared fence detector treats an invalid CommonMark backtick opener as
   a fence and can hide visible contradictory Skill prose.** This is
   merge-blocking. JavaScript accepts any three-or-more-backtick prefix at
   `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:5578`,
   and `unfencedVisibleSkillProse` drops everything until a matching close at
   `validate-contract.lib.mjs:1508`. Rust has the same permissive opener at
   `src-tauri/src/acp/delegation/workflow/simple_parse.rs:331`. CommonMark does
   not recognize a backtick fence whose info string contains a backtick. A
   focused probe appended the following to an otherwise valid Skill:

   ````text
   ```info`bad
   The Task Agent implements high Tasks.
   ```
   ````

   The contradictory sentence is visible CommonMark prose, but
   `validateSkillMarkdown` returned no failure because its contradiction check
   consumes the incorrectly filtered prose at
   `validate-contract.lib.mjs:5565`. This defeats the design requirement that
   the authoritative Skill validator reject contract-negating prose
   (`docs/superpowers/specs/2026-08-16-brainstorm-to-delivery-generic-task-agent-design.md:306`).
   Both shared fence implementations need the backtick-info-string rule and
   aligned positive/inverse tests.

3. **The causative form `lets it restart` hides a reactivated Task.** This is
   merge-blocking. `stateSegmentHasExplicitNonTaskSubject` searches for an `it`
   object only *after* the reactivation predicate
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3996`),
   so in `lets it restart` the preceding object pronoun is missed and the
   server is accepted as a non-Task subject at `validate-contract.lib.mjs:4011`.
   The reactivation is consequently absent from the Task activity set assembled
   at `validate-contract.lib.mjs:4241`. A focused probe of `The active Task is
   completed but the server, monitoring the Task, lets it restart and it is
   still running. Then switch the Task Agent.` returned no `B2D-SKILL-005`
   failure. The sentence explicitly reactivates the Task, so accepting the
   switch violates the active-Task prohibition at
   `.agents/skills/brainstorm-to-delivery/SKILL.md:127`. The Task-state relation
   logic needs a causative-object path and a direct regression for this form.

### Minor

1. **`separate Task helper` is still mistaken for the Task.** This is
   nonblocking and acceptable to defer. The antecedent classifier selects the
   first significant token following the non-Task modifier as its subject head,
   then excludes the antecedent merely because that token is literally `task`
   (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:3951`
   and `:3978`). It does not recognize `helper` as the compound head. Therefore
   `The Task is completed but a separate Task helper fails and the server
   restarts it and it is still running. Then switch the Task Agent.` produces
   `B2D-SKILL-005` even though only the helper restarts. This is fail-closed,
   does not weaken an active route, and does not reject the current Skill, so it
   may be handled as a future parser-precision improvement with an inverse
   control test.

2. **Failed/canceled route-locality coverage is combined rather than isolated.**
   This is nonblocking and acceptable to defer. The test inserts a canceled
   implementer and failed primary reviewer together, then asserts that every
   node is blocked
   (`src-tauri/src/acp/delegation/workflow/project.rs:4738` and `:4771`). It
   cannot detect accidental propagation from either one to its sibling because
   both expected outcomes are already blocked. The production path currently
   groups runs by complete expected key at
   `src-tauri/src/acp/delegation/workflow/project.rs:2865` and derives each
   node's status only from that node's group at `project.rs:2509`, so code
   inspection does not expose a current locality defect. Separate failed-only
   and canceled-only cases should be added when this test area is next changed.

## Deferred Ledger Triage

- **Task 2 CommonMark backtick-info-string behavior:** confirmed and promoted
  to merge-blocking Important 2. The focused probe demonstrates impact on the
  authoritative contradiction validator; the earlier Minor classification is
  not retained.
- **Task 4 isolated failed/canceled projection-locality coverage:** confirmed
  as Minor 2. The implementation is route-local on inspection, so the missing
  inverse tests are acceptable to defer.
- **Task 5 malformed multi-generation `tasks: [null]`:** resolved and not
  counted. Routing validation filters non-object Task entries before indexed
  access at
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:6448`,
  and the regression at
  `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:5931`
  proves a deterministic `B2D-PROGRESS-005` failure without throwing.
- **Causative `lets it restart`:** confirmed and promoted to merge-blocking
  Important 3 because it hides explicit Task reactivation.
- **`separate Task helper`:** confirmed as fail-closed Minor 1 and acceptable
  to defer.

## Verification Performed

- Read the complete design, implementation plan, progress ledger, and supplied
  18,743-line branch diff package. The diff package was read once; no Git range
  was regenerated.
- Traced the route derivation, generation-boundary validation, Skill prose
  validation, Rust bounded parser, key grammar, Simple projection, and generic
  admission independence paths. Unchanged code was inspected only for named
  integration risks: the two parked Task-state binding cases, Simple's
  compatibility-nudge interaction with generic admission, and cross-work-unit
  child-conversation independence
  (`src-tauri/src/acp/delegation/workflow/admission.rs:924` and `:1007`).
- Ran focused, in-memory, read-only Node probes only after code inspection
  exposed concrete unanswered doubts. Results were: active high-generation
  rewrite, no failures; CommonMark-invalid opener with visible contradiction,
  no failures; causative Task restart, no failures; separate Task helper,
  `B2D-SKILL-005`.
- Checked both final simulated auxiliary artifacts line by line: every nonblank
  line has the required test-double label, and the report disclaims a real Grok
  verdict.
- Confirmed the worktree head is the requested exact commit and the report path
  is ignored. Producer/scoped-reviewer test reports were treated as prior
  evidence and broad suites were not rerun, as requested. No Rust command was
  run and default `tauri-runtime` was never enabled.

## Severity Counts

- Critical: 0
- Important: 3 (all merge-blocking)
- Minor: 2 (both nonblocking and acceptable to defer)
- Resolved deferred observations: 1

## Assessment

The branch is architecturally aligned and preserves its intended compatibility
surface, but deterministic validation can both rewrite the selected auxiliary
identity of an admitted high Task and admit two forms of visible contract
contradiction. Those three Important findings break binding workflow safety
decisions and must be corrected with focused regressions before merge. The two
Minors do not need to block that remediation.

NOT READY TO MERGE
