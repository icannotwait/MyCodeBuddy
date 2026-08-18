# SIMULATED GROK AUXILIARY REVIEW - WORKFLOW TEST DOUBLE ONLY

This is a simulated workflow-test-double review. It is not a response from
Grok.

## Spec Compliance

The production Skill has one v2 contract block and the exact nine-phase
contract structure. The validator has bounded unfenced extraction, deterministic
routing derivation, routed Plan/progress agreement, and key-based lineage
grouping. The Skill prose directs normal Tasks to the selected Task Agent plus a
Codex primary reviewer and high Tasks to Codex plus primary and auxiliary
reviewers.

However, the two Important findings below mean the implementation does not yet
fully enforce the required completed-lineage admission evidence and did not
replace the retired nine-scenario Rust matrix as requested.

## Strengths

- The positive v2 contract is present once and precedes the workflow phases in
  [`SKILL.md`](.agents/skills/brainstorm-to-delivery/SKILL.md:12), while the
  validator requires exactly one complete unfenced contract
  ([`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:588)).
- The Task Agent default and boundary rules are operationally stated in
  [`SKILL.md`](.agents/skills/brainstorm-to-delivery/SKILL.md:121), and route
  derivation produces the required normal/high identities and explicit reviewer
  slots ([`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:946)).
- Progress groups runs by `work_unit_key`, preserving separate primary and
  auxiliary reviewer lineages
  ([`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1338)).
- The new Rust matrix contains the requested eleven v2 scenario names
  ([`delegation_session_reuse_integration.rs`](src-tauri/tests/delegation_session_reuse_integration.rs:1000)).

## Critical

None.

## Important

1. Completed Tasks can pass routing validation with fabricated, never-admitted
   lineages. `validateRun` only requires a non-empty role, agent, state, and
   work-unit key; `task_id` and `child_conversation_id` are merely optional
   type checks ([`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1222),
   [`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1267)).
   `validateProgressRouting` then treats an expected key as complete solely
   when its last run has `state === "completed"`
   ([`validate-contract.lib.mjs`](.agents/skills/brainstorm-to-delivery/scripts/validate-contract.lib.mjs:1656)).
   A completed routed Task with both required runs set to `completed` but with
   neither `task_id` nor `child_conversation_id` is accepted (reproduced with
   the exported validator against this revision). This violates the required
   terminal completed lineage and permits a Task to be completed without any
   recorded delegation admission. Require non-empty `task_id` and a non-null
   valid child conversation ID for terminal completed runs, and add a negative
   test.

2. The obsolete hard-coded nine-scenario Rust matrix was retained instead of
   being replaced. `legacy_skill_forward_scenarios` still hard-codes Grok
   implementers ([`delegation_session_reuse_integration.rs`](src-tauri/tests/delegation_session_reuse_integration.rs:672)),
   and `skill_forward_routing_invariants_nine_scenarios` still asserts nine old
   outcomes ([`delegation_session_reuse_integration.rs`](src-tauri/tests/delegation_session_reuse_integration.rs:1053),
   [`delegation_session_reuse_integration.rs`](src-tauri/tests/delegation_session_reuse_integration.rs:1158)).
   Task 5 explicitly required replacement of that old Grok-hard-coded
   nine-scenario matrix by the approved eleven scenarios. Keeping the stale
   matrix makes the test suite encode conflicting legacy routing expectations
   alongside v2 and leaves dead, misleading coverage. Remove it or migrate its
   useful assertions into the eleven v2 scenarios.

## Minor

None.

## Final Verdict

SIMULATED GROK WORKFLOW TEST DOUBLE ONLY: changes requested. Critical: 0;
Important: 2; Minor: 0.
