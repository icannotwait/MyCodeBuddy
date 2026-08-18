### Task 3: Derive and reconcile normal/high Simple routes without Gates

**Dependencies:** Task 1 canonical keys and Task 2 parsed routing/progress models.

**Files:**

- Modify: `src-tauri/src/acp/delegation/workflow/project.rs` (route derivation, reconciliation warnings, focused tests)
- Report: `.superpowers/sdd/b2d-generic-task-agent-routing/task-3-report.md` (do not commit)

**Interfaces:**

- Consumes: `SimpleRoutingSnapshot`, additive progress route fields, `build_work_unit_key`, `ReviewerSlot`, and durable `delegation_task_run` rows.
- Produces: validated expected-route derivation plus bounded non-blocking reconciliation warnings consumed by Task 4.

Introduce these private projection helpers:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct SimpleExpectedRoute {
    risk_level: String,
    task_agent_generation: u32,
    implementer_key: String,
    primary_reviewer_key: String,
    auxiliary_reviewer_key: Option<String>,
}

fn derive_simple_expected_route(
    routing: &SimpleRoutingSnapshot,
    task_index: u32,
) -> Result<SimpleExpectedRoute, &'static str>;

fn reconcile_simple_progress_route(
    expected: &SimpleExpectedRoute,
    progress: Option<&SimpleProgressTask>,
    warnings: &mut Vec<String>,
);

fn run_matches_work_unit_key(
    run: &delegation_task_run::Model,
    expected_key: &str,
) -> bool;
```

`derive_simple_expected_route` uses the Task's recorded Agent/profile selections and Task 1's builders. It returns these exact route shapes:

```text
normal: implementer(selected Task Agent), primary(codex), no auxiliary
high:   implementer(codex), primary(codex), auxiliary(selected Task Agent)
```

It returns an error for unknown level, missing/duplicate Task index, missing generation, wrong reviewer slots/count, invalid Agent/profile, or a route not derived from the referenced generation. The caller adds `simple_plan_routing_invalid` and falls back to the legacy aggregate Task node; it never fails admission.

`reconcile_simple_progress_route` adds only bounded/deduplicated warning codes:

```text
simple_progress_risk_level_mismatch
simple_progress_task_agent_generation_mismatch
simple_progress_implementer_key_mismatch
simple_progress_primary_reviewer_key_mismatch
simple_progress_auxiliary_reviewer_key_mismatch
simple_progress_expected_route_missing
simple_progress_run_outside_expected_route
simple_progress_route_child_not_independent
```

- [ ] **Step 1: Write failing route derivation/reconciliation unit tests**

Add table cases for normal Grok, normal custom Agent/profile, high Grok, and high Task Agent Codex. Assert high Codex produces three different keys even though all three Agent types/profiles match:

```rust
assert_eq!(route.implementer_key, "task|4|implementer|codex|none");
assert_eq!(
    route.primary_reviewer_key,
    "task|4|reviewer|primary|codex|none"
);
assert_eq!(
    route.auxiliary_reviewer_key.as_deref(),
    Some("task|4|reviewer|auxiliary|codex|none")
);
```

Mutate each mirrored progress field and assert the corresponding warning is emitted once. Give two distinct expected keys the same non-null `child_conversation_id` and assert `simple_progress_route_child_not_independent`. These warnings must not return `ProjectError`.

- [ ] **Step 2: Run route helper tests and verify RED**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_route -- --nocapture
```

Expected: FAIL because routing helpers and warning codes do not exist.

- [ ] **Step 3: Implement deterministic route derivation and warnings**

Compare complete work-unit keys, not generic `role`. When checking child independence, group admitted runs by non-null child ID and fail only the reconciliation state when one child appears under two different expected keys. Do not infer Agent fallback or rewrite Plan/progress.

- [ ] **Step 4: Run Task 3 GREEN and focused regressions**

From `src-tauri/`:

```bash
cargo test --lib --features test-utils simple_projection_route -- --nocapture
cargo test --lib --features test-utils simple_projection_warns -- --nocapture
```

Expected: route derivation and warning tests PASS; the pre-existing legacy warning projection tests remain green.

- [ ] **Step 5: Commit Task 3**

```bash
git add -- src-tauri/src/acp/delegation/workflow/project.rs
git commit -m "feat(workflow): reconcile Simple task routes"
```

- [ ] **Step 6: Write the Task report**

Create `.superpowers/sdd/b2d-generic-task-agent-routing/task-3-report.md` with route fixtures, reconciliation warnings, commands, commit hash, and retained Minors. Do not stage it.

---

