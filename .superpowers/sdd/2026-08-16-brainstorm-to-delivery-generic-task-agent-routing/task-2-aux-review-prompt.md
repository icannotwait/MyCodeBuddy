You are the independent Grok auxiliary reviewer for high-risk Task 2. Review
the latest Codex producer result for spec compliance and code quality. Your
review is an independent gate input, not commentary on the primary reviewer.

Read these files first:

- Task brief: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-brief.md`
- Implementer report: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-2-report.md`
- Diff package: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/review-6973793f..6cfd1830.diff`

Diff base is `6973793f`; head is `6cfd1830`. Read the diff package once. Do not
rerun git commands or crawl the broader codebase. Inspect unchanged code only
for a concrete named risk and report what you checked. Work read-only: do not
change files, the index, HEAD, or branch state.

Binding global constraints copied from the Plan:

- Simple remains the only writable brainstorm-to-delivery mode. Do not add a workflow manifest, platform Gate, gate settlement, completion Card, artifact digest, reviewed task ID, or platform-owned completion decision.
- Preserve `b2d_task_risk_v1` exactly: any hard trigger is `high`; otherwise a unique-evidence soft score of `0..=2` is `normal` and `>= 3` is `high`.
- Normal route: selected Task Agent implements and fixes; an independent Codex primary reviewer reviews. High route: an independent Codex implements and fixes; an independent Codex primary reviewer and independent selected Task Agent auxiliary reviewer both review the latest producer result.
- Existing five-part Task reviewer keys remain readable as legacy primary reviewers. New runs always emit explicit six-part primary/auxiliary keys.
- Plan/progress/run disagreement is a deterministic Skill-validator failure before dispatch, but only a bounded projection warning in Rust. It must never become a platform admission Gate.
- Keep the Plan at or below 2 MiB, the routing block at or below 256 KiB, progress at or below 512 KiB, and the progress block at or below 64 KiB.

Treat the implementer report as unverified. The controller already reran the
focused Simple parser tests (14/14), key tests (15/15), formatting check, and
diff check on the exact head. Do not rerun suites. Run only a focused test for
a specific doubt not answered by that evidence.

Check missing, extra, and misunderstood requirements; bounded and safe-partial
behavior; legacy compatibility; warning semantics; test integrity; error
handling; and the shared extractor's Markdown fence and exact-marker behavior.
Every finding must cite `file:line`, explain impact, and state a fix when not
obvious. Critical and Important findings block approval; Minor findings do not.

Your final response must be the complete review. Do not try to write files.

Use this report structure:

### Spec Compliance
- `PASS` or `FAIL`, with cited gaps
- `Cannot verify`, if any

### Strengths

### Issues
#### Critical
#### Important
#### Minor

### Assessment
- Task quality: `Approved` or `Needs fixes`
- Reasoning: one or two technical sentences
