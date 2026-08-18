### Spec Compliance

- **Verdict: Needs fixes.** The v2 contract, deterministic normal/high route shapes, risk arithmetic, explicit reviewer slots, per-key lineage grouping, legacy reviewer readability, and most Skill ownership rules are present. However, the generation-boundary validator makes an adopted Agent change impossible to continue, the on-disk validator silently recreates a missing generation as Grok, and the Design workflow can omit the mandatory Codex reviewer.
- The implementer reports all requested Node and Rust checks passing. Per the review instruction, I did not rerun those suites. I used two read-only focused Node probes to confirm the malformed-progress throw and the overlength derived-key case below.

### Strengths

- The embedded v2 contract deep-equals the required positive contract, precedes the nine imperative phases, and the Skill remains below 500 lines (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:588`, `.agents/skills/brainstorm-to-delivery/SKILL.md:12`).
- Risk validation recomputes fixed soft scores, rejects duplicate signal/evidence counting, forces high for every hard trigger, and derives exact normal/high routes rather than accepting free-form routing (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:848`, `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:947`).
- Routed progress uses explicit primary/auxiliary keys, groups lineage by complete `work_unit_key`, enforces global task-ID uniqueness, and prevents distinct keys from sharing a child conversation (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1338`, `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1354`).
- The Skill correctly keeps Task execution serial, fans out high reviews only after implementation, returns fixes to the owning producer, reruns every required Task review, and routes final findings through Task ownership (`.agents/skills/brainstorm-to-delivery/SKILL.md:200`, `.agents/skills/brainstorm-to-delivery/SKILL.md:240`).
- Integration coverage now exercises separate Design Fixer/Reviewer, Plan Author/Reviewer, high-route implementer/primary/auxiliary, all-Codex high routes, and final-review children through real broker delegation (`src-tauri/tests/delegation_session_reuse_integration.rs:1351`, `src-tauri/tests/delegation_session_reuse_integration.rs:1477`).

### Issues

#### Critical (Must Fix)

None.

#### Important (Should Fix)

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1671`: Every validation of a later Task-Agent generation requires its boundary Task to remain `pending` with `runs: []` and requires global `active_task_index` to remain null. This is valid only at the instant the revised Plan is adopted. As soon as that legitimate boundary Task's implementer is reserved, the same boundary becomes active and gains a run, so validation fails. The Skill requires validation before every Task dispatch (`.agents/skills/brainstorm-to-delivery/SKILL.md:202`), which means the required primary/auxiliary reviewers cannot be dispatched; after completion, the next Task also cannot start because the historical boundary is no longer empty. Preserve the empty-pending rule while adopting a new generation, but accept already-adopted historical boundaries whose frozen progress metadata and run keys still agree with the Plan. Add a lifecycle test that validates before the boundary implementer, after its admission, before its reviewers, after completion, and before the following Task.

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1008`: A missing `task_agent_generations` field is silently synthesized as generation 1 Grok. The validator has no invocation context, so it cannot tell an omitted user selection from a damaged or rewritten Plan; this is precisely the kind of silent explicit-selection substitution the contract forbids. It also blesses a document the Rust reader rejects, because `SimpleRoutingSnapshot.task_agent_generations` is a required serde field (`src-tauri/src/acp/delegation/workflow/simple_parse.rs:133`, `src-tauri/src/acp/delegation/workflow/simple_parse.rs:571`). Resolve an omitted invocation to Grok before Plan authoring, serialize the explicit generation, and require the authoritative on-disk routing block to contain a non-empty generation array. The test at `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.test.mjs:442` currently locks in the incompatible behavior and should instead test invocation-to-snapshot construction.

- `.agents/skills/brainstorm-to-delivery/SKILL.md:135`: The Design phase says to dispatch a user-named Design Reviewer *instead of* the conditional independent Codex Design Reviewer. The approved role contract requires the Codex Design Reviewer whenever Design review is triggered, with user-named document reviewers added to that review cohort; user-named reviewers may not replace the mandatory independent Codex review. Dispatch the conditional Codex reviewer first and add any user-named Design reviewers as separate document-only work units. Also extend the prose test so this replacement wording cannot pass while the embedded JSON remains correct.

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:829`: Agent/profile validation is performed independently from canonical key validation. A valid 64-byte custom Agent ID plus a valid 128-character profile produces a 226-character high auxiliary key at `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:983`, even though recognized work-unit keys are capped at 200 characters (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:371`). The focused probe returned no failures and a 226-character key; a later progress run using it is then rejected as non-canonical at line 1245, and platform key construction cannot admit it. Validate every derived key with the same canonical builder/parser and fail the route before it reaches progress or delegation; add boundary tests combining maximum custom IDs, profiles, and reviewer slots.

#### Minor (Nice to Have)

- `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1673`: The generation-boundary pass dereferences every raw `snapshot.tasks` entry without checking that it is an object. `parseSimpleProgress` records malformed Task entries but still calls agreement validation (`.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1776`, `.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1796`), so a multi-generation route plus `tasks: [null]` throws `TypeError` instead of returning deterministic rule failures. Filter to validated task objects, or skip agreement checks after structural progress failures, and add a no-throw malformed-progress case.

- `src-tauri/tests/delegation_session_reuse_integration.rs:672`: The brief required replacing the Grok-hard-coded nine-scenario matrix with the approved eleven scenarios, but the old matrix remains as `legacy_skill_forward_scenarios` and is still executed by `skill_forward_routing_invariants_nine_scenarios` at line 1053. It does not currently break runtime behavior, but it preserves obsolete policy language and duplicates a large test path. Remove or reduce it to genuinely legacy key/session compatibility assertions, leaving the eleven v2 scenarios as the policy matrix.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The core contract and routing implementation are substantial and mostly well covered, but the current generation validation deadlocks the advertised boundary-change workflow after its first admitted run. Missing-generation Grok fallback and optional replacement of mandatory Codex Design review also violate binding ownership/selection rules. These are behavioral contract issues, not documentation polish, and should be resolved before Task 5 approval.
