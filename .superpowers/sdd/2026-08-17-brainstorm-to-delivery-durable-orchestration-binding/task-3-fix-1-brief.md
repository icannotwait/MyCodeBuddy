# Task 3 Fix Round 1

Continue the same Task 3 implementer. Re-inspect Git, files, brief, report,
and both reviews. Treat earlier reasoning as provisional.

Parent adjudication — fix these:

## Important 1 (primary)

Malformed public query calls bypass `orchestration_binding_query_invalid`.
The published schema accepts one-sided snapshot/cursor. Companion local
validation returns JSON-RPC `-32602` and never reaches the listener
structured mapper. Encode the all-or-none snapshot/cursor relation in the
schema and preserve the stable structured
`error.code = "orchestration_binding_query_invalid"` on every public
invalid-input path.

## Important 2 (primary + auxiliary)

`promote_running_detailed` can commit reserving→running without advancing
the parent revision if the unlocked pre-read fails. After a successful
`Promoted` outcome, increment from the transaction result's
`parent_conversation_id`. Do not gate increment on the unlocked pre-read.
Cover the transient-pre-read / successful-promote sequence.

## Minor (include in this same fix)

Budget-driven description edits made `continue_delegation` say Join.
Restore operational meaning for `continue_delegation`, the new query, and
`register_simple_workflow` without weakening the `7_680`/`7680` literals.
Keep `proposed_user_reason` as reset_plan_lineage-only if that still fits.

Re-run covering query tests, the exact Grok budget test, and record the
printed byte count. Append the fix report. One focused commit.

Return status, commit hash, covering-test summary, printed Grok JSONL
bytes, and concerns.
