# Task 2 Fix Round 1

Continue the same Task 2 Codex implementer. Re-inspect Git, current
files, the Task brief, your report, and the primary review. Treat earlier
reasoning as provisional. Audit any partial changes. Recreate the report
append if needed.

Open finding (verbatim):

> The required continuation mismatch side-effect proof is missing.
> Task 2 explicitly requires a lineage mismatch to be exercised through the
> broker before resume, recovery-authorization consumption, budget work, or
> any other child/process side effect (`task-2-brief.md:70-81`). The focused
> broker tests cover first dispatch and replacement only
> (`broker.rs:16668`, `broker.rs:16749`). Continuation is tested only by
> calling `RunStore::admit_continue_reserving` and checking that no row was
> inserted (`run_store.rs:6284-6381`), which cannot detect a broker regression
> that resumes the child before store admission and does not prove a supplied
> continuation authorization remains usable after rejection. Add a
> broker-level bound/unbound continuation mismatch test that asserts the
> exact `orchestration_binding_lineage_mismatch` code, unchanged
> `MockSpawner::resume_args`/spawn counters, no new run, and successful reuse
> of the same authorization by the subsequent exact or omitted-binding call.

The parent adjudicated this as a valid Important spec gap. Auxiliary
review found no other issues.

Add the broker-level continuation mismatch test. Re-run covering tests
named in the brief (`orchestration_binding_lineage_` and related). Append
the fix report to `task-2-report.md` with command, executed count, and
output. One focused fix commit. Do not start Task 3.

Return status, commit hash, covering-test summary, and concerns.
