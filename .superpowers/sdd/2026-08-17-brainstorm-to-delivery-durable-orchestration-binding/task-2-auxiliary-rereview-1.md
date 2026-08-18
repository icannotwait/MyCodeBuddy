# Task 2 Auxiliary Re-review 1

- **Reviewer:** Grok (independent auxiliary)
- **Task:** Enforce binding transport and lineage admission
- **Fix range:** `43c63745..344d2ab9`
- **Latest producer commit:** `344d2ab99fabbf0c7e62d14bfb852f8272ce0f9c`
- **Subject:** `test(delegation): prove continuation binding fence`
- **Inputs:** `task-2-brief.md`, appended `task-2-report.md`, `review-43c63745..344d2ab9.diff`, Plan Global Constraints
- **Open finding:** missing broker-level continuation mismatch side-effect proof
- **Mode:** read-only review of the fix range; no production edits; full library suite not re-run

## Verdict

**Open finding:** ADDRESSED

**Latest result remains approved:** Yes

**Spec compliance:** Compliant

**Task quality:** Approved

| Severity | New count |
| --- | --- |
| Critical | 0 |
| Important | 0 |
| Minor | 0 |

No new Critical or Important findings.

## Disposition of the open finding

The parent-adjudicated Important gap was real. The first producer commit
covered continuation lineage only through `RunStore::admit_continue_reserving`.
That cannot see a broker regression that resumes before store admission, and it
does not prove a supplied continuation authorization remains consumable after
rejection.

Commit `344d2ab9` adds
`orchestration_binding_lineage_continue_mismatch_precedes_admission_and_resume`
in `broker.rs`. It is test-only (269 insertions, no production edits).
`inherited_binding` is unchanged and still fail-closed on supplied mismatch.

The new test drives `continue_delegation` for both required recovery cases:

| Case | Source | Rejected supply | Retry supply |
| --- | --- | --- | --- |
| bound | fixture binding | changed namespace | exact source binding |
| unbound | `None` | fixture binding | omitted |

Each rejected call asserts:

- wire code `orchestration_binding_lineage_mismatch`
- `MockSpawner::resume_args` and `spawn_args` unchanged
- no durable row under the rejected parent tool-use ID

Each retry then uses the same approved `parent_disconnected` continuation
authorization, reaches `TaskStatus::Running`, performs exactly one resume
(and the mock's inner spawn bookkeeping call), reuses the source child,
persists the exact source binding, and records that authorization on the new
run.

That is the missing broker/resumer/authorization proof from
`task-2-brief.md:70-81`.

The implementer's mutation RED is consistent with the current mock: if
`inherited_binding` ignores the supplied value, the bound mismatch proceeds
into resume and cannot return the required lineage code. Production
`inherited_binding` was not left mutated.

## New issues

### Critical (Must Fix)

- None.

### Important (Should Fix)

- None.

### Minor (Nice to Have)

- None.

## Independent verification

Re-ran only focused filters from `src-tauri/` with
`--no-default-features --features server,test-utils`. Did not re-run the
full `--lib` suite.

| Command | Result |
| --- | --- |
| `cargo test ... --lib orchestration_binding_lineage_ -- --nocapture` | 5 passed, 0 failed, 4627 filtered out |
| exact `orchestration_binding_lineage_continue_mismatch_precedes_admission_and_resume` | 1 passed, 0 failed, 4631 filtered out |
| `cargo test ... --lib orchestration_binding_transport_ -- --nocapture` | 2 passed, 0 failed, 4630 filtered out |
| `cargo test ... --lib request_fingerprint_ -- --nocapture` | 2 passed, 0 failed, 4630 filtered out |

`git diff --check 43c63745..344d2ab9` is clean. HEAD is `344d2ab9`. The
commit contains only `src-tauri/src/acp/delegation/broker.rs`. The report
append remains untracked. The existing macOS `__eh_frame` warning still
appears while linking; tests still passed.

## Assessment

**Latest result remains approved:** Yes

**Reasoning:** The fix supplies the required broker-level continuation
mismatch proof without changing production admission. Focused lineage
coverage is now 5 tests. No new Critical or Important breakage.

```json
{
  "kind": "task_rereview",
  "task": 2,
  "slot": "auxiliary",
  "round": 1,
  "reviewer": "grok",
  "producer_commit": "344d2ab99fabbf0c7e62d14bfb852f8272ce0f9c",
  "range": "43c63745..344d2ab9",
  "open_finding": "addressed",
  "latest_approved": true,
  "spec_compliance": "compliant",
  "task_quality": "approved",
  "new_critical": 0,
  "new_important": 0,
  "new_minor": 0,
  "findings": [],
  "verification": {
    "orchestration_binding_lineage_": "5 passed",
    "continue_mismatch_exact": "1 passed",
    "orchestration_binding_transport_": "2 passed",
    "request_fingerprint_": "2 passed"
  }
}
```
