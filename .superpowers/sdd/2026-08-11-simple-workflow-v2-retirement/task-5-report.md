# Task 5 Report: v2 write retirement and archived side-effect fence

Status: complete with verification deferred by task instruction.

## Changes

- Added the stable `WorkflowStoreError::WorkflowV2Retired` code and surfaced it through ACP and MCP error mappings.
- Retired production `publish_workflow_manifest_core`; it now rejects before validation or persistence. Historical read-model setup uses the explicitly named `#[cfg(test)] publish_workflow_manifest_fixture` helper.
- Added archived durable-mode preflight before broker child/folder/run allocation and retained the same check inside RunStore transactions as the race fence.
- Extended prompt admission to inspect durable identity for both roots and already-bound children while allowing an ordinary newly prebound child to receive its first prompt.
- Disabled `workflow_v2` and `completion_v2` feature injection for new companion launches. Existing direct-call server paths remain guarded by the retired mutation error.
- Moved workflow-history test fixtures in workflow, listener, project, completion, and recovery test modules to the named helper.

## Tests

- Added `manifest_publication_is_retired_without_creating_a_header` and updated the central protocol matrix assertion.
- Tests, compilation, lint, formatting, and builds were intentionally not run, per the Task 5 instruction.

## Self Review

- Verified production manifest publication has no route to the prior write body.
- Verified broker preflight precedes provisional child creation and RunStore repeats the check in its transaction.
- Verified MCP catalog injection carries neither retired feature for new Root or child launches.
- Ran `git diff --check`; no whitespace errors were reported.

## Risks

- Runtime verification is deferred to the Task 5-8 unified validation pass.
- Existing historical workflow unit fixtures continue to exercise legacy projection internals under `cfg(test)`; production mutation paths remain retired.
