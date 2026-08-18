### Spec Compliance

- ❌ Issues found: routed projection does not actually group the progress-side runs by exact expected work-unit key. Progress entries are inspected for route warnings and dereferenced by `task_id`, but only durable `delegation_task_run::Model` values enter the implementer/reviewer groups (`src-tauri/src/acp/delegation/workflow/project.rs:2397`, `src-tauri/src/acp/delegation/workflow/project.rs:2752`, `src-tauri/src/acp/delegation/workflow/project.rs:2804`, `src-tauri/src/acp/delegation/workflow/project.rs:2853`). This misses the brief's durable/progress grouping contract and fails to diagnose a progress key that disagrees with the resolved durable run's key.
- ⚠️ Cannot verify from diff: Step 2's chronological failing RED run. The implementer explicitly reports that no command transcript survived and tests plus implementation were squashed together (`.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-report.md:87`). The controller should treat the static incompatibility argument as evidence that the tests would fail on the base, but not as evidence that the required RED command was actually run.

### Strengths

- Route nodes use the required stable implementer/primary/auxiliary IDs and exact durable-key filtering, so identical Agent/profile reviewers remain separate (`src-tauri/src/acp/delegation/workflow/project.rs:2803`, `src-tauri/src/acp/delegation/workflow/project.rs:2839`, `src-tauri/src/acp/delegation/workflow/project.rs:2844`).
- The edge construction correctly fans out implementer-to-reviewers and fans the prior reviewers into the next Task implementer (`src-tauri/src/acp/delegation/workflow/project.rs:2811`, `src-tauri/src/acp/delegation/workflow/project.rs:2858`, `src-tauri/src/acp/delegation/workflow/project.rs:2885`).
- Route-local status derivation prevents aggregate completed progress from filling missing nodes, blocks failed/canceled required runs, and makes stale completed reviews non-complete (`src-tauri/src/acp/delegation/workflow/project.rs:2493`, `src-tauri/src/acp/delegation/workflow/project.rs:2498`, `src-tauri/src/acp/delegation/workflow/project.rs:2517`).
- The Simple projection retains `workflow_id: None`, `manifest_revision: None`, `compatibility: Simple`, and an empty Gate list (`src-tauri/src/acp/delegation/workflow/project.rs:3121`, `src-tauri/src/acp/delegation/workflow/project.rs:3149`).
- Focused tests cover normal/high topology, route-local active state, stale review, incomplete completion, invalid-route fallback, and terminal failures (`src-tauri/src/acp/delegation/workflow/project.rs:4215`, `src-tauri/src/acp/delegation/workflow/project.rs:4242`, `src-tauri/src/acp/delegation/workflow/project.rs:4334`, `src-tauri/src/acp/delegation/workflow/project.rs:4436`, `src-tauri/src/acp/delegation/workflow/project.rs:4503`, `src-tauri/src/acp/delegation/workflow/project.rs:4523`).

### Issues

#### Critical (Must Fix)

None.

#### Important (Should Fix)

- `src-tauri/src/acp/delegation/workflow/project.rs:2752`: Progress runs are reduced to optional durable-row lookups by `task_id`; their own `work_unit_key` and `state` never participate in node grouping or node derivation. The later implementer/reviewer filters operate exclusively on durable rows (`src-tauri/src/acp/delegation/workflow/project.rs:2804`, `src-tauri/src/acp/delegation/workflow/project.rs:2853`). Consequently, a progress-only exact-key run remains an unobserved pending node, and a progress entry whose expected reviewer key points to a durable implementer row silently populates the implementer rather than emitting a bounded progress/durable disagreement warning. This violates the explicit durable/progress exact-key grouping requirement and makes the projection fragile during missing-row or disagreement cases. Build route-local groups from both sources, validate a progress reference's key against the resolved durable row before accepting it, emit a bounded mismatch warning, and add focused progress-only and conflicting-key tests.

#### Minor (Nice to Have)

- `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-4-report.md:167`: The reported GREEN runs emitted a linker warning (`__eh_frame section too large`). The rubric requires warning-free verification output. This appears pre-existing and did not fail the focused tests, so it is not a Task 4 behavior blocker, but the controller should retain it as validation debt or route the same checks through an environment that produces pristine output.

### Assessment

**Task quality:** Needs fixes

**Reasoning:** The routed graph topology and durable-run state logic are strong, but the implementation omits one of the two explicitly required run sources and therefore cannot reliably project or warn on progress/durable route disagreement. That contract and its tests should be fixed before approving the task.
