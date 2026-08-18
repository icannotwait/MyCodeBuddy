### Finding Verdicts

- **Progress runs were omitted from exact-key route grouping and progress/durable key conflicts were accepted without a bounded warning.** — ADDRESSED. `src-tauri/src/acp/delegation/workflow/project.rs:2482` defines separate durable/progress route groups; `src-tauri/src/acp/delegation/workflow/project.rs:2509` gives durable observations precedence and falls back to the last progress observation only when no durable row exists; `src-tauri/src/acp/delegation/workflow/project.rs:2791` admits progress fallback only for a complete canonical expected key; `src-tauri/src/acp/delegation/workflow/project.rs:2804` rejects a resolved durable-row key disagreement and emits the bounded/deduplicated `simple_progress_run_durable_key_mismatch` warning; and `src-tauri/src/acp/delegation/workflow/project.rs:2865` groups both sources by exact expected key without double-counting matching durable references. Focused regressions cover the progress-only and conflicting-key cases at `src-tauri/src/acp/delegation/workflow/project.rs:4412` and `src-tauri/src/acp/delegation/workflow/project.rs:4472`, and the appended report records their RED/GREEN output plus the scoped regression runs.

### New Breakage in the Fix Diff

None.

### Out-of-Scope Observations

None.

### Verdict

**Fix round:** All findings addressed, no new Critical/Important breakage.
