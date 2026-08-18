You are the independent Codex primary reviewer for high-risk Task 3. Review
the complete latest producer result for spec compliance and code quality.

Read these files first:

- Task brief: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-brief.md`
- Implementer report: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/task-3-report.md`
- Diff package: `.superpowers/sdd/2026-08-16-brainstorm-to-delivery-generic-task-agent-routing/review-ab23f562..d63a2951.diff`

Base is `ab23f562`; head is `d63a2951`. Read the diff package once. Do not
rerun git commands or crawl the broader codebase. Inspect unchanged code only
for a concrete named risk and report what you checked. Work read-only: do not
change files, the index, HEAD, or branch state.

Binding global constraints copied from the Plan:

- Simple remains the only writable brainstorm-to-delivery mode. Do not add a workflow manifest, platform Gate, gate settlement, completion Card, artifact digest, reviewed task ID, or platform-owned completion decision.
- Resolve exactly one initial Task Agent generation from the invocation. Omitted selection means `agent_type: "grok"` and `profile_id: null`; never silently substitute an unavailable, invalid, or ambiguous Agent/profile.
- Preserve `b2d_task_risk_v1` exactly: any hard trigger is `high`; otherwise a unique-evidence soft score of `0..=2` is `normal` and `>= 3` is `high`.
- Normal route: selected Task Agent implements and fixes; an independent Codex primary reviewer reviews. High route: an independent Codex implements and fixes; an independent Codex primary reviewer and independent selected Task Agent auxiliary reviewer both review the latest producer result.
- Keep every producer and reviewer on a distinct work-unit key and child conversation, even when Agent type and profile are identical. A continuation may reuse only its own work unit.
- Plan/progress/run disagreement is a deterministic Skill-validator failure before dispatch, but only a bounded projection warning in Rust. It must never become a platform admission Gate.
- Existing five-part Task reviewer keys remain readable as legacy primary reviewers. New runs always emit explicit six-part primary/auxiliary keys.

Treat the report as unverified. The implementer reports that permitted
server-only filters passed: 3 route tests and 3 warning tests. Per the user's
binding instruction, do not run Rust tests with default features. Do not rerun
the reported tests. Run only a focused
`--no-default-features --features server,test-utils` test for a specific doubt
not answered by that evidence.

Check missing, extra, and misunderstood requirements; invalid route fallback;
complete-key matching; bounded warning behavior; child independence; test
integrity; error handling; and absence of admission/Gate authority. Every
finding must cite `file:line`, explain impact, and state a fix when not obvious.
Critical and Important findings block approval; Minor findings do not.

Output only this complete review:

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
